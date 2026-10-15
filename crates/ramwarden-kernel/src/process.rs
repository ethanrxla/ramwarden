//! The process table, read straight from `/proc`.
//!
//! This replaces `psutil.process_iter`. Beyond dropping a Python dependency it
//! buys one real improvement: `/proc/<pid>/stat` already carries a resident-size
//! figure, so the expensive `smaps_rollup` read for accurate PSS can be skipped
//! for the hundreds of small processes that will never be reclaim candidates.
//! v1 paid full price for every process on every tick.
//!
//! # Parsing `stat` is not as simple as it looks
//!
//! The second field is the executable name in parentheses, and it may contain
//! both spaces and parentheses. Real examples from the machine this was written
//! for:
//!
//! ```text
//! 1243 (UVM global queue) S 2 0 0 ...
//! 1508 ((sd-pam)) S 1434 1434 ...
//! ```
//!
//! Splitting on whitespace, or on the first `)`, misreads both. The only correct
//! split is at the *last* `)`, which is what [`ProcEntry::parse`] does.

use std::fs;
use std::path::Path;

use crate::{Error, Result, Root};

/// One row of the process table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcEntry {
    pub pid: i32,
    /// `comm` as reported by the kernel: the executable name, truncated to 15
    /// characters. Note the truncation — it is why `openclaw-gateway` appears as
    /// `openclaw-gatewa` and why name matching must tolerate prefixes.
    pub comm: String,
    /// `R`unning, `S`leeping, `D` uninterruptible, `T` stopped, `Z`ombie, `I`dle.
    pub state: char,
    pub ppid: i32,
    pub session: i32,
    /// 0 when the process has no controlling terminal.
    pub tty_nr: i32,
    /// `utime + stime`, in clock ticks. Only differences between samples are
    /// meaningful; the absolute value is lifetime CPU.
    pub cpu_ticks: u64,
    /// Clock ticks after boot at which this process started.
    pub start_ticks: u64,
    /// Resident pages. Cheap but inaccurate for shared memory — use it only to
    /// decide whether a process is worth measuring properly with
    /// [`crate::smaps::rollup`].
    pub rss_pages: u64,
}

impl ProcEntry {
    /// Approximate resident bytes, from `stat` alone.
    pub fn rss_bytes(&self) -> u64 {
        self.rss_pages * page_size()
    }

    pub fn is_stopped(&self) -> bool {
        self.state == 'T'
    }

    pub fn is_zombie(&self) -> bool {
        self.state == 'Z'
    }

    pub fn has_tty(&self) -> bool {
        self.tty_nr != 0
    }

    pub fn cpu_seconds(&self) -> f64 {
        self.cpu_ticks as f64 / clock_ticks() as f64
    }

    /// Wall-clock age, given the boot time from [`boot_time`].
    pub fn age_seconds(&self, boot_epoch: u64, now_epoch: u64) -> f64 {
        let started = boot_epoch as f64 + (self.start_ticks as f64 / clock_ticks() as f64);
        (now_epoch as f64 - started).max(0.0)
    }

    /// Parse one `/proc/<pid>/stat` line.
    pub fn parse(text: &str, path: &Path) -> Result<Self> {
        let open = text
            .find('(')
            .ok_or_else(|| Error::parse(path, "no '(' before comm"))?;
        // The last ')' — comm may contain both spaces and parentheses.
        let close = text
            .rfind(')')
            .ok_or_else(|| Error::parse(path, "no ')' after comm"))?;
        if close < open {
            return Err(Error::parse(path, "malformed comm field"));
        }

        let pid: i32 = text[..open]
            .trim()
            .parse()
            .map_err(|_| Error::parse(path, "pid is not an integer"))?;
        let comm = text[open + 1..close].to_string();

        let f: Vec<&str> = text[close + 1..].split_whitespace().collect();
        // Indices are offset by 3: f[0] is `state`, field 3 of the file.
        let need = |i: usize, what: &str| -> Result<&str> {
            f.get(i)
                .copied()
                .ok_or_else(|| Error::parse(path, format!("missing {what} (field {})", i + 3)))
        };
        let num = |i: usize, what: &str| -> Result<u64> {
            need(i, what)?
                .parse()
                .map_err(|_| Error::parse(path, format!("{what} is not an integer")))
        };
        let inum = |i: usize, what: &str| -> Result<i32> {
            need(i, what)?
                .parse()
                .map_err(|_| Error::parse(path, format!("{what} is not an integer")))
        };

        Ok(ProcEntry {
            pid,
            comm,
            state: need(0, "state")?
                .chars()
                .next()
                .ok_or_else(|| Error::parse(path, "empty state"))?,
            ppid: inum(1, "ppid")?,
            session: inum(3, "session")?,
            tty_nr: inum(4, "tty_nr")?,
            cpu_ticks: num(11, "utime")? + num(12, "stime")?,
            start_ticks: num(19, "starttime")?,
            rss_pages: num(21, "rss")?,
        })
    }
}

/// Read one process.
pub fn entry(root: &Root, pid: i32) -> Result<ProcEntry> {
    let path = root.proc_pid(pid, "stat");
    let text = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
    ProcEntry::parse(&text, &path)
}

/// Every process currently visible.
///
/// Processes that vanish mid-walk are skipped rather than failing the sweep —
/// on a desktop running a browser, several exit during any given tick.
pub fn table(root: &Root) -> Result<Vec<ProcEntry>> {
    let proc_dir = root.join("proc");
    let entries = fs::read_dir(&proc_dir).map_err(|e| Error::io(&proc_dir, e))?;

    let mut out = Vec::with_capacity(512);
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(pid) = name.parse::<i32>() else {
            continue; // /proc/net, /proc/sys, ...
        };
        match entry(root, pid) {
            Ok(p) => out.push(p),
            Err(e) if e.is_missing() => continue,
            Err(Error::Denied { .. }) => continue,
            // A half-written read of a dying process: skip it, do not abort the
            // whole sweep for one bad row.
            Err(Error::Parse { .. }) => continue,
            Err(e) => return Err(e),
        }
    }
    out.sort_by_key(|p| p.pid);
    Ok(out)
}

/// Full command line, NUL separators replaced with spaces.
///
/// Empty for kernel threads. Used for role classification, where the executable
/// name alone is ambiguous — `ollama serve` is an agent runtime, `ollama list`
/// is a command that will exit in a moment.
pub fn cmdline(root: &Root, pid: i32) -> Result<String> {
    let path = root.proc_pid(pid, "cmdline");
    let raw = fs::read(&path).map_err(|e| Error::io(&path, e))?;
    Ok(raw
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect::<Vec<_>>()
        .join(" "))
}

/// Real UID, from `/proc/<pid>/status`.
pub fn uid(root: &Root, pid: i32) -> Result<u32> {
    let path = root.proc_pid(pid, "status");
    let text = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            return rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| Error::parse(&path, "unreadable Uid line"));
        }
    }
    Err(Error::parse(&path, "no Uid line"))
}

/// The executable behind a process, from the `/proc/<pid>/exe` symlink.
///
/// Unreadable for another user's processes and for kernel threads, which is why
/// callers treat absence as "unknown" rather than as evidence.
pub fn exe(root: &Root, pid: i32) -> Result<std::path::PathBuf> {
    let path = root.proc_pid(pid, "exe");
    fs::read_link(&path).map_err(|e| Error::io(&path, e))
}

/// Boot time as a Unix timestamp, from `/proc/stat`.
///
/// Needed because `stat` reports process start as ticks since boot, not as a
/// date. Read once per tick, not once per process.
pub fn boot_time(root: &Root) -> Result<u64> {
    let path = root.join("proc/stat");
    let text = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
    text.lines()
        .find_map(|l| l.strip_prefix("btime ")?.trim().parse().ok())
        .ok_or_else(|| Error::parse(&path, "no btime line"))
}

/// `USER_HZ` — the unit of the CPU and start-time fields in `stat`.
pub fn clock_ticks() -> u64 {
    // SAFETY: sysconf with a valid name returns a long; no pointers involved.
    let v = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if v > 0 { v as u64 } else { 100 }
}

pub fn page_size() -> u64 {
    // SAFETY: as above.
    let v = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if v > 0 { v as u64 } else { 4096 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Verbatim: a Brave browser process on the target machine.
    const BRAVE: &str = "6669 (brave) S 6668 6650 6650 0 -1 4194304 80645394 1810945 15502 109 171933 49765 290 2710 32 12 39 0 6740 57373945856 169121 18446744073709551615 111102080184320 111102326677904 140733022253840 0 0 0 0 4096 1098990847 0 0 0 17 4 0 0 0 0 0 111102339526656 111102341320705 111102873763840 140733022261694 140733022261804 140733022261804 140733022265319 0";

    /// Verbatim: a kernel thread whose comm contains spaces.
    const SPACED: &str = "1243 (UVM global queue) S 2 0 0 0 -1 2129984 0 0 0 0 0 0 0 0 20 0 1 0 821 0 0 18446744073709551615 0 0 0 0 0 0 0 2147483647 0 0 0 0 17 8 0 0 0 0 0 0 0 0 0 0 0 0 0";

    /// Verbatim: systemd's PAM helper, whose comm contains parentheses.
    const PARENS: &str = "1508 ((sd-pam)) S 1434 1434 1434 0 -1 4194624 53 0 0 0 0 0 0 0 20 0 1 0 870 21884928 907 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0";

    fn p() -> PathBuf {
        PathBuf::from("/proc/1/stat")
    }

    #[test]
    fn parses_a_real_process() {
        let e = ProcEntry::parse(BRAVE, &p()).unwrap();
        assert_eq!(e.pid, 6669);
        assert_eq!(e.comm, "brave");
        assert_eq!(e.state, 'S');
        assert_eq!(e.ppid, 6668);
        assert_eq!(e.session, 6650);
        assert_eq!(e.tty_nr, 0);
        assert_eq!(e.cpu_ticks, 171_933 + 49_765);
        assert_eq!(e.start_ticks, 6_740);
        assert_eq!(e.rss_pages, 169_121);
        assert!(!e.has_tty());
        assert!(!e.is_stopped());
    }

    /// A comm containing spaces must not shift every subsequent field.
    #[test]
    fn parses_a_comm_containing_spaces() {
        let e = ProcEntry::parse(SPACED, &p()).unwrap();
        assert_eq!(e.comm, "UVM global queue");
        assert_eq!(e.pid, 1243);
        assert_eq!(e.ppid, 2, "ppid must not be shifted by the spaces in comm");
        assert_eq!(e.state, 'S');
        assert_eq!(e.rss_pages, 0);
    }

    /// Splitting on the *first* ')' would truncate this comm to "(sd-pam" and
    /// then misread the state field.
    #[test]
    fn parses_a_comm_containing_parentheses() {
        let e = ProcEntry::parse(PARENS, &p()).unwrap();
        assert_eq!(e.comm, "(sd-pam)");
        assert_eq!(e.state, 'S');
        assert_eq!(e.ppid, 1434);
        assert_eq!(e.rss_pages, 907);
    }

    #[test]
    fn rss_bytes_matches_the_page_count() {
        let e = ProcEntry::parse(BRAVE, &p()).unwrap();
        assert_eq!(e.rss_bytes(), 169_121 * page_size());
    }

    #[test]
    fn cpu_seconds_converts_from_clock_ticks() {
        let e = ProcEntry::parse(BRAVE, &p()).unwrap();
        let expected = (171_933.0 + 49_765.0) / clock_ticks() as f64;
        assert!((e.cpu_seconds() - expected).abs() < 1e-9);
    }

    #[test]
    fn age_is_measured_from_boot_plus_start_ticks() {
        let e = ProcEntry::parse(BRAVE, &p()).unwrap();
        let boot = 1_790_097_664u64;
        // 6740 ticks = 67.4s after boot, so 1000s later it is ~932.6s old.
        let age = e.age_seconds(boot, boot + 1000);
        assert!((age - 932.6).abs() < 0.5, "{age}");
    }

    #[test]
    fn age_never_goes_negative() {
        let e = ProcEntry::parse(BRAVE, &p()).unwrap();
        assert_eq!(e.age_seconds(1_790_097_664, 1_790_000_000), 0.0);
    }

    #[test]
    fn a_stopped_process_is_recognised() {
        let stopped = BRAVE.replacen(") S ", ") T ", 1);
        assert!(ProcEntry::parse(&stopped, &p()).unwrap().is_stopped());
    }

    #[test]
    fn a_truncated_stat_line_is_a_parse_error_not_a_panic() {
        assert!(ProcEntry::parse("6669 (brave) S 6668", &p()).is_err());
        assert!(ProcEntry::parse("", &p()).is_err());
        assert!(ProcEntry::parse("no parens here", &p()).is_err());
        assert!(ProcEntry::parse("123 )backwards( S 1 1 1 1", &p()).is_err());
    }

    fn fixture() -> (tempfile::TempDir, Root) {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        fs::create_dir_all(root.join("proc")).unwrap();
        fs::write(
            root.join("proc/stat"),
            "cpu  1 2 3\nbtime 1790097664\nprocesses 99\n",
        )
        .unwrap();

        for (pid, body) in [(6669, BRAVE), (1243, SPACED), (1508, PARENS)] {
            let d = root.join(format!("proc/{pid}"));
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("stat"), body).unwrap();
            fs::write(d.join("status"), "Name:\tx\nUid:\t1000\t1000\t1000\t1000\n").unwrap();
            fs::write(d.join("cmdline"), b"/usr/bin/thing\0--flag\0value\0".as_slice()).unwrap();
        }

        // Non-numeric entries in /proc must be ignored.
        fs::create_dir_all(root.join("proc/net")).unwrap();
        fs::create_dir_all(root.join("proc/sys")).unwrap();
        (dir, root)
    }

    #[test]
    fn walks_the_table_and_ignores_non_pid_entries() {
        let (_d, root) = fixture();
        let t = table(&root).unwrap();
        assert_eq!(t.len(), 3, "{:?}", t.iter().map(|p| p.pid).collect::<Vec<_>>());
        assert_eq!(t[0].pid, 1243, "sorted by pid");
        assert_eq!(t[2].pid, 6669);
    }

    #[test]
    fn a_process_that_exits_mid_walk_is_skipped() {
        let (_d, root) = fixture();
        // An empty stat file stands in for a read that raced with exit.
        let d = root.join("proc/4242");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("stat"), "").unwrap();
        let t = table(&root).unwrap();
        assert_eq!(t.len(), 3, "the unreadable row is dropped, the sweep continues");
    }

    #[test]
    fn reads_boot_time_uid_and_cmdline() {
        let (_d, root) = fixture();
        assert_eq!(boot_time(&root).unwrap(), 1_790_097_664);
        assert_eq!(uid(&root, 6669).unwrap(), 1000);
        assert_eq!(cmdline(&root, 6669).unwrap(), "/usr/bin/thing --flag value");
    }

    #[test]
    fn a_kernel_thread_has_an_empty_cmdline() {
        let (_d, root) = fixture();
        fs::write(root.proc_pid(1243, "cmdline"), b"".as_slice()).unwrap();
        assert_eq!(cmdline(&root, 1243).unwrap(), "");
    }

    #[test]
    fn reads_the_executable_path() {
        let (_d, root) = fixture();
        let d = root.join("proc/6669");
        std::os::unix::fs::symlink("/usr/lib/brave/brave", d.join("exe")).unwrap();
        assert_eq!(
            exe(&root, 6669).unwrap(),
            std::path::PathBuf::from("/usr/lib/brave/brave")
        );
    }

    #[test]
    fn an_unreadable_exe_is_an_error_not_an_empty_path() {
        let (_d, root) = fixture();
        assert!(exe(&root, 6669).is_err(), "no symlink was created");
    }

    #[test]
    fn clock_and_page_constants_are_sane() {
        assert!(clock_ticks() >= 1);
        assert!(page_size().is_power_of_two());
    }
}
