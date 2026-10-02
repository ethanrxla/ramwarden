//! Proportional set size, and which of a process's pages are cold.
//!
//! # Why this module exists
//!
//! RamWarden v1 measured a process group by summing `RSS` across its processes.
//! That is wrong whenever processes share pages, which for a browser is *most*
//! of them. Measured on a live 75-process Brave:
//!
//! ```text
//! summed RSS  15,094 MB     <- what v1 reported
//! summed PSS   6,512 MB     <- the truth
//! ```
//!
//! RSS counts a shared page once per process mapping it; PSS counts it once,
//! divided across the processes sharing it. Summing PSS over a process group is
//! therefore meaningful, and summing RSS is not.

use std::fs;
use std::path::Path;

use crate::{Error, KIB, Result, Root};

/// One process's memory, as the kernel accounts it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rollup {
    /// Resident set size: every page mapped by this process, shared or not.
    /// Kept only so callers can show how badly naive summing would mislead.
    pub rss: u64,
    /// Proportional set size: private pages in full, shared pages divided by the
    /// number of processes mapping them. **This is the number to sum.**
    pub pss: u64,
    /// The dirty part of PSS — pages that must be written to zram or swap to be
    /// reclaimed, rather than simply dropped. Sizing a reclaim request against
    /// this avoids asking the kernel for memory it can only get by evicting
    /// clean page cache it would rather keep.
    pub pss_dirty: u64,
    /// Anonymous share of PSS: heap and stack, the part zram compresses.
    pub pss_anon: u64,
    /// Pages touched since the last time the kernel cleared the referenced bit.
    /// `rss - referenced` is the cold footprint, and the basis of every reclaim
    /// decision in the ladder.
    pub referenced: u64,
    /// Pages the process has locked into RAM. Unreclaimable by any means, so a
    /// reclaim request sized without subtracting these will always fall short.
    pub locked: u64,
}

impl Rollup {
    /// Bytes that look reclaimable without killing anything: resident, not
    /// recently touched, not locked.
    ///
    /// Saturating because `referenced` is sampled a moment after `rss` and can
    /// legitimately exceed it on a process that is allocating quickly.
    pub fn cold_bytes(&self) -> u64 {
        self.rss
            .saturating_sub(self.referenced)
            .saturating_sub(self.locked)
    }

    /// How far a naive RSS sum would overstate this process. 1.0 means no
    /// sharing at all; a browser renderer runs around 1.3, a whole browser well
    /// above 2.
    pub fn sharing_factor(&self) -> f64 {
        if self.pss == 0 {
            return 1.0;
        }
        self.rss as f64 / self.pss as f64
    }
}

/// Read `/proc/<pid>/smaps_rollup`.
///
/// The kernel pre-aggregates this file, so it costs one read and a dozen lines
/// regardless of how many mappings the process has. Parsing full `smaps` for the
/// same totals means walking thousands of entries per process per tick — the
/// reason v1 never attempted PSS at all.
pub fn rollup(root: &Root, pid: i32) -> Result<Rollup> {
    let path = root.proc_pid(pid, "smaps_rollup");
    let text = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
    parse_rollup(&text, &path)
}

fn parse_rollup(text: &str, path: &Path) -> Result<Rollup> {
    let mut out = Rollup::default();
    let mut saw_pss = false;

    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue; // the leading "[rollup]" address line
        };
        let slot = match key {
            "Rss" => &mut out.rss,
            "Pss" => {
                saw_pss = true;
                &mut out.pss
            }
            "Pss_Dirty" => &mut out.pss_dirty,
            "Pss_Anon" => &mut out.pss_anon,
            "Referenced" => &mut out.referenced,
            "Locked" => &mut out.locked,
            _ => continue,
        };
        *slot = parse_kib(rest, path, key)?;
    }

    // A rollup without a Pss line is not a rollup. Returning a zeroed struct
    // here would read downstream as "this process uses no memory", which is the
    // kind of silent wrong answer this rewrite exists to eliminate.
    if !saw_pss {
        return Err(Error::parse(path, "no Pss: line"));
    }
    Ok(out)
}

/// Parse a `"   722500 kB"` value into bytes.
fn parse_kib(rest: &str, path: &Path, key: &str) -> Result<u64> {
    let field = rest
        .split_whitespace()
        .next()
        .ok_or_else(|| Error::parse(path, format!("{key}: empty value")))?;
    let kib: u64 = field
        .parse()
        .map_err(|_| Error::parse(path, format!("{key}: {field:?} is not a number")))?;
    Ok(kib * KIB)
}

/// Sum PSS across a set of processes, skipping any that vanished mid-read.
///
/// Processes exiting while we walk them is routine, not exceptional — a browser
/// spawns and reaps renderers constantly — so a missing `smaps_rollup` drops the
/// process from the total instead of failing the whole measurement. Any other
/// error is returned, because it means we are misreading the kernel.
pub fn sum_pss(root: &Root, pids: &[i32]) -> Result<u64> {
    let mut total = 0u64;
    for &pid in pids {
        match rollup(root, pid) {
            Ok(r) => total += r.pss,
            Err(e) if e.is_missing() => continue,
            // A process we may not inspect (another user's, or one that became a
            // zombie) contributes nothing rather than aborting the tick.
            Err(Error::Denied { .. }) => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Verbatim from /proc/2070834/smaps_rollup on the machine this was written
    /// for — a real Brave renderer, so the parser is tested against the exact
    /// bytes the kernel produces rather than a tidied-up approximation.
    const BRAVE_RENDERER: &str = "\
00010000-7ffe752cd000 ---p 00000000 00:00 0                              [rollup]
Rss:              722500 kB
Pss:              555845 kB
Pss_Dirty:        549208 kB
Pss_Anon:         539200 kB
Pss_File:           6636 kB
Pss_Shmem:         10008 kB
Shared_Clean:     157116 kB
Shared_Dirty:      24700 kB
Private_Clean:       616 kB
Private_Dirty:    540068 kB
Referenced:       687372 kB
Anonymous:        539200 kB
LazyFree:              0 kB
AnonHugePages:         0 kB
ShmemPmdMapped:        0 kB
FilePmdMapped:         0 kB
Shared_Hugetlb:        0 kB
Private_Hugetlb:       0 kB
Swap:              12844 kB
SwapPss:           12844 kB
Locked:                0 kB
";

    fn p() -> PathBuf {
        PathBuf::from("/proc/test/smaps_rollup")
    }

    #[test]
    fn parses_a_real_rollup() {
        let r = parse_rollup(BRAVE_RENDERER, &p()).unwrap();
        assert_eq!(r.rss, 722_500 * KIB);
        assert_eq!(r.pss, 555_845 * KIB);
        assert_eq!(r.pss_dirty, 549_208 * KIB);
        assert_eq!(r.pss_anon, 539_200 * KIB);
        assert_eq!(r.referenced, 687_372 * KIB);
        assert_eq!(r.locked, 0);
    }

    #[test]
    fn cold_bytes_is_resident_minus_referenced() {
        let r = parse_rollup(BRAVE_RENDERER, &p()).unwrap();
        assert_eq!(r.cold_bytes(), (722_500 - 687_372) * KIB);
    }

    #[test]
    fn cold_bytes_excludes_locked_pages() {
        let r = Rollup {
            rss: 1000 * KIB,
            referenced: 200 * KIB,
            locked: 300 * KIB,
            ..Default::default()
        };
        assert_eq!(r.cold_bytes(), 500 * KIB);
    }

    #[test]
    fn cold_bytes_saturates_when_referenced_exceeds_rss() {
        // Sampled a moment apart on a fast-allocating process; must not panic.
        let r = Rollup {
            rss: 100 * KIB,
            referenced: 400 * KIB,
            ..Default::default()
        };
        assert_eq!(r.cold_bytes(), 0);
    }

    /// The headline bug, pinned. Two processes sharing pages must sum to less
    /// under PSS than under RSS; if this test ever passes trivially, the
    /// accounting has regressed to v1's behaviour.
    #[test]
    fn summing_pss_does_not_double_count_shared_pages() {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());

        // Two renderers each mapping 100 MB, of which 80 MB is the same shared
        // text. RSS says 200 MB is in use; the truth is 120 MB.
        for pid in [101, 102] {
            let d = root.join(format!("proc/{pid}"));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("smaps_rollup"),
                "0-1 ---p 0 00:00 0 [rollup]\n\
                 Rss:              102400 kB\n\
                 Pss:               61440 kB\n\
                 Referenced:        50000 kB\n\
                 Locked:                0 kB\n",
            )
            .unwrap();
        }

        let summed_rss: u64 = [101, 102]
            .iter()
            .map(|&pid| rollup(&root, pid).unwrap().rss)
            .sum();
        let summed_pss = sum_pss(&root, &[101, 102]).unwrap();

        assert_eq!(summed_rss, 204_800 * KIB, "RSS sum double-counts");
        assert_eq!(summed_pss, 122_880 * KIB, "PSS sum is the real figure");
        assert!(
            summed_pss < summed_rss,
            "PSS must be below RSS when pages are shared"
        );
    }

    #[test]
    fn sharing_factor_reports_the_overcount() {
        let r = parse_rollup(BRAVE_RENDERER, &p()).unwrap();
        assert!((r.sharing_factor() - 1.299).abs() < 0.01, "{}", r.sharing_factor());
    }

    #[test]
    fn a_process_that_exits_mid_walk_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        let d = root.join("proc/7");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("smaps_rollup"),
            "0-1 ---p 0 00:00 0 [rollup]\nRss: 100 kB\nPss: 100 kB\n",
        )
        .unwrap();

        // pid 8 never existed, standing in for one reaped between listing and reading.
        assert_eq!(sum_pss(&root, &[7, 8]).unwrap(), 100 * KIB);
    }

    #[test]
    fn a_truncated_rollup_is_an_error_not_a_zero() {
        let err = parse_rollup("0-1 ---p 0 00:00 0 [rollup]\nRss:  100 kB\n", &p()).unwrap_err();
        assert!(matches!(err, Error::Parse { .. }), "{err:?}");
    }

    #[test]
    fn a_non_numeric_field_is_an_error() {
        let err = parse_rollup("Pss:   banana kB\n", &p()).unwrap_err();
        assert!(matches!(err, Error::Parse { .. }), "{err:?}");
    }
}

// ── Per-VMA detail, for targeted page-out ───────────────────────────────────

/// One mapping in a process's address space.
///
/// Reading full `smaps` is expensive — thousands of entries for a browser — so
/// this is never used on the monitoring path. It is only walked for a process
/// the ladder has already decided to page out, where the cost is paid once and
/// buys precision that cgroup-wide reclaim cannot: the kernel reclaims a whole
/// scope's cold memory, whereas this targets chosen regions of one process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vma {
    pub start: u64,
    pub end: u64,
    pub rss: u64,
    pub referenced: u64,
    pub anonymous: u64,
    pub locked: bool,
    /// Private mapping (`p` rather than `s` in the permission field).
    pub private: bool,
}

impl Vma {
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Worth paging out: resident, anonymous (so zram can compress it), private,
    /// unlocked, and largely untouched since the last scan.
    ///
    /// Anonymous is the key filter. Paging out clean file-backed pages gains
    /// nothing — the kernel drops those for free under pressure — while the heap
    /// and stack are exactly what zram compresses 4.5x on this machine.
    pub fn is_cold(&self, referenced_ratio: f64) -> bool {
        if self.locked || !self.private || self.rss == 0 || self.anonymous == 0 {
            return false;
        }
        (self.referenced as f64) <= (self.rss as f64) * referenced_ratio
    }
}

/// Mappings of `pid` worth paging out, largest first.
///
/// `referenced_ratio` is how much recent touching still counts as cold (0.2 is a
/// reasonable default); `min_bytes` drops mappings too small to be worth a
/// syscall. Returns an empty vector rather than an error if the process exits
/// mid-walk, which for a browser renderer is routine.
pub fn cold_vmas(
    root: &Root,
    pid: i32,
    referenced_ratio: f64,
    min_bytes: u64,
) -> Result<Vec<Vma>> {
    let path = root.proc_pid(pid, "smaps");
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(&path, e)),
    };

    let mut out: Vec<Vma> = parse_vmas(&text, &path)?
        .into_iter()
        .filter(|v| v.len() >= min_bytes && v.is_cold(referenced_ratio))
        .collect();
    out.sort_by_key(|v| std::cmp::Reverse(v.rss));
    Ok(out)
}

fn parse_vmas(text: &str, path: &Path) -> Result<Vec<Vma>> {
    let mut out = Vec::new();
    let mut cur: Option<Vma> = None;

    for line in text.lines() {
        // A header line starts with a hex range and has no "Key: value" shape.
        if let Some(v) = parse_vma_header(line) {
            if let Some(prev) = cur.take() {
                out.push(prev);
            }
            cur = Some(v);
            continue;
        }

        let Some(v) = cur.as_mut() else { continue };
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        match key {
            "Rss" => v.rss = parse_kib(rest, path, key)?,
            "Referenced" => v.referenced = parse_kib(rest, path, key)?,
            "Anonymous" => v.anonymous = parse_kib(rest, path, key)?,
            "Locked" => v.locked = parse_kib(rest, path, key)? > 0,
            "VmFlags" => {
                // `lo` means mlocked; `dd` means do-not-dump, often a region the
                // kernel will refuse to touch anyway.
                if rest.split_whitespace().any(|f| f == "lo") {
                    v.locked = true;
                }
            }
            _ => continue,
        }
    }
    if let Some(last) = cur {
        out.push(last);
    }
    Ok(out)
}

fn parse_vma_header(line: &str) -> Option<Vma> {
    let mut fields = line.split_whitespace();
    let range = fields.next()?;
    let perms = fields.next()?;
    let (start, end) = range.split_once('-')?;

    // Guard against a pathname that happens to contain a dash.
    let start = u64::from_str_radix(start, 16).ok()?;
    let end = u64::from_str_radix(end, 16).ok()?;
    if perms.len() != 4 {
        return None;
    }

    Some(Vma {
        start,
        end,
        rss: 0,
        referenced: 0,
        anonymous: 0,
        locked: false,
        private: perms.ends_with('p'),
    })
}

#[cfg(test)]
mod vma_tests {
    use super::*;

    /// Shaped exactly like /proc/<pid>/smaps: an anonymous heap region that is
    /// cold, a hot one, a file-backed mapping, and an mlocked region.
    const SMAPS: &str = "\
55d0b1000000-55d0b5000000 rw-p 00000000 00:00 0                          [heap]
Size:              65536 kB
Rss:               40960 kB
Pss:               40960 kB
Referenced:         2048 kB
Anonymous:         40960 kB
Locked:                0 kB
VmFlags: rd wr mr mw me ac
7f1000000000-7f1000400000 rw-p 00000000 00:00 0
Size:               4096 kB
Rss:                4096 kB
Pss:                4096 kB
Referenced:         4096 kB
Anonymous:          4096 kB
Locked:                0 kB
VmFlags: rd wr mr mw me ac
7f2000000000-7f2000800000 r--p 00000000 fd:01 1234                       /usr/lib/libc.so.6
Size:               8192 kB
Rss:                8192 kB
Pss:                 512 kB
Referenced:              0 kB
Anonymous:             0 kB
Locked:                0 kB
VmFlags: rd mr mw me
7f3000000000-7f3000100000 rw-p 00000000 00:00 0
Size:               1024 kB
Rss:                1024 kB
Pss:                1024 kB
Referenced:            0 kB
Anonymous:          1024 kB
Locked:             1024 kB
VmFlags: rd wr mr mw me lo
7f4000000000-7f4000200000 rw-s 00000000 00:00 0
Size:               2048 kB
Rss:                2048 kB
Pss:                1024 kB
Referenced:            0 kB
Anonymous:             0 kB
Locked:                0 kB
VmFlags: rd wr sh mr mw me
";

    fn vmas() -> Vec<Vma> {
        parse_vmas(SMAPS, Path::new("/proc/1/smaps")).unwrap()
    }

    #[test]
    fn parses_every_mapping() {
        assert_eq!(vmas().len(), 5);
    }

    #[test]
    fn a_cold_anonymous_heap_region_is_selected() {
        let heap = vmas()[0];
        assert_eq!(heap.rss, 40960 * KIB);
        assert_eq!(heap.referenced, 2048 * KIB);
        assert!(heap.private);
        assert!(heap.is_cold(0.2), "heap touched 5% should be cold");
    }

    #[test]
    fn a_fully_referenced_region_is_not_cold() {
        assert!(!vmas()[1].is_cold(0.2), "100% referenced must not be cold");
    }

    /// Clean file-backed pages are dropped for free under pressure, so paging
    /// them out buys nothing and costs a syscall.
    #[test]
    fn a_file_backed_mapping_is_never_cold() {
        let libc = vmas()[2];
        assert_eq!(libc.anonymous, 0);
        assert!(!libc.is_cold(0.9));
    }

    #[test]
    fn an_mlocked_region_is_never_cold() {
        let locked = vmas()[3];
        assert!(locked.locked, "Locked: and VmFlags lo must both mark it");
        assert!(!locked.is_cold(0.9));
    }

    #[test]
    fn a_shared_mapping_is_never_cold() {
        let shared = vmas()[4];
        assert!(!shared.private, "rw-s is shared");
        assert!(!shared.is_cold(0.9));
    }

    #[test]
    fn cold_vmas_filters_sorts_and_respects_a_size_floor() {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        let d = root.join("proc/5");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("smaps"), SMAPS).unwrap();

        // Only the heap qualifies on coldness.
        let picked = cold_vmas(&root, 5, 0.2, 0).unwrap();
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].rss, 40960 * KIB);

        // A floor above the heap's 64 MB excludes everything.
        assert!(cold_vmas(&root, 5, 0.2, 128 * 1024 * 1024).unwrap().is_empty());
    }

    #[test]
    fn a_process_that_exits_mid_walk_yields_nothing_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        assert!(cold_vmas(&root, 9999, 0.2, 0).unwrap().is_empty());
    }

    #[test]
    fn a_pathname_containing_a_dash_does_not_confuse_the_header_parser() {
        let line = "7f2000000000-7f2000800000 r--p 00000000 fd:01 1 /usr/lib/x86-64-gnu/libfoo-1.so";
        let v = parse_vma_header(line).unwrap();
        assert_eq!(v.start, 0x7f2000000000);
        assert_eq!(v.end, 0x7f2000800000);
    }

    #[test]
    fn a_key_value_line_is_not_mistaken_for_a_header() {
        assert!(parse_vma_header("Rss:               40960 kB").is_none());
        assert!(parse_vma_header("VmFlags: rd wr mr mw me ac").is_none());
    }
}
