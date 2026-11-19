//! How big a folder is, measured within a time budget.
//!
//! # Why a budget
//!
//! A home folder holds millions of files, and a cold walk of one takes
//! minutes on a hard disk. "What's eating my storage?" should be answered in
//! seconds, so every walk shares one [`Budget`]: a deadline and a ceiling on
//! entries. When it runs out the walk stops and the sizes so far are reported
//! as "at least", marked incomplete. A second ask is faster, because the
//! first one warmed the filesystem cache.
//!
//! # What is counted
//!
//! - Space on disk (`st_blocks`), not file length, where the OS reports it.
//!   A sparse 60 GB virtual disk that occupies 8 GB counts as 8 GB.
//! - Symlinks and junctions are never followed: the target is counted where
//!   it lives, or not at all if that is another disk.
//! - Other filesystems mounted below the folder are skipped (Unix), so a
//!   network share under `~/mnt` is not charged to the home folder.
//! - A file with several hard links is counted once per link. Rare outside
//!   package stores; it can overstate those.
//!
//! # Caches found on the way
//!
//! A folder holding a valid `CACHEDIR.TAG` (the bford.info convention; Cargo
//! writes one into every `target/`) declares itself regenerable. The walk
//! records the outermost such folders, which is how old build output is found
//! wherever the projects live, with no list of project folders.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

/// The first bytes of a valid `CACHEDIR.TAG`.
pub const CACHE_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";
pub const CACHE_TAG: &str = "CACHEDIR.TAG";
/// Folders nested deeper than this are not entered. Guards the stack.
const MAX_DEPTH: usize = 96;
/// Worker threads for the top-level folders. Disk walks are I/O bound, and
/// four keeps an SSD busy without starving the ladder on a hard disk.
const WORKERS: usize = 4;

/// Whether `dir` holds a valid cache tag.
pub fn is_cache_dir(dir: &Path) -> bool {
    let mut buf = [0u8; 43];
    fs::File::open(dir.join(CACHE_TAG)).and_then(|mut f| f.read_exact(&mut buf)).is_ok() && buf == CACHE_SIGNATURE
}

/// What every walk in one scan shares.
pub struct Budget {
    deadline: Instant,
    entries: AtomicI64,
    spent: AtomicBool,
    /// Folders never entered (`Places::private`): the figure for whatever
    /// holds one is then a floor.
    private: Vec<PathBuf>,
}

impl Budget {
    pub fn new(time: Duration, entries: i64) -> Self {
        Budget { deadline: Instant::now() + time, entries: AtomicI64::new(entries), spent: AtomicBool::new(false), private: Vec::new() }
    }

    /// Leave these folders, and everything in them, out of every walk.
    pub fn skipping(mut self, private: &[PathBuf]) -> Self {
        self.private = private.to_vec();
        self
    }

    fn private(&self, path: &Path) -> bool {
        self.private.iter().any(|p| path.starts_with(p))
    }

    /// Count one entry. False once the budget is gone.
    fn take(&self) -> bool {
        if self.spent.load(Ordering::Relaxed) {
            return false;
        }
        let left = self.entries.fetch_sub(1, Ordering::Relaxed);
        // The clock is read every 256 entries: reading it per file doubled
        // the cost of a warm walk.
        if left <= 0 || (left & 0xff == 0 && Instant::now() >= self.deadline) {
            self.spent.store(true, Ordering::Relaxed);
            return false;
        }
        true
    }

    pub fn spent(&self) -> bool {
        self.spent.load(Ordering::Relaxed)
    }
}

/// A measured folder or file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Measured {
    pub bytes: u64,
    pub files: u64,
    /// False when the budget ran out inside it: `bytes` is then a floor.
    pub complete: bool,
}

/// A self-declared cache folder found during a walk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheDir {
    pub path: PathBuf,
    pub bytes: u64,
}

#[cfg(unix)]
pub(super) fn on_disk(m: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (m.blocks() * 512, m.dev())
}

#[cfg(not(unix))]
pub(super) fn on_disk(m: &fs::Metadata) -> (u64, u64) {
    (m.len(), 0)
}

struct Walk<'a> {
    budget: &'a Budget,
    device: u64,
    files: u64,
    complete: bool,
    caches: &'a Mutex<Vec<CacheDir>>,
}

impl Walk<'_> {
    /// Size of everything below `dir`, and whether all of it was seen. A
    /// folder or entry that cannot be read, or a branch past `MAX_DEPTH`,
    /// makes the figure a floor, the same as running out of budget. Other
    /// disks mounted below are left out on purpose and do not.
    fn dir(&mut self, dir: &Path, depth: usize, in_cache: bool) -> (u64, bool) {
        let Ok(entries) = fs::read_dir(dir) else { return (0, false) };
        let cache = !in_cache && is_cache_dir(dir);
        let mut total = 0u64;
        let mut complete = true;
        for entry in entries {
            if !self.budget.take() {
                complete = false;
                break;
            }
            // `DirEntry::metadata` does not follow links.
            let Ok(meta) = entry.and_then(|e| e.metadata().map(|m| (e, m))) else {
                complete = false;
                continue;
            };
            let (entry, meta) = meta;
            let kind = meta.file_type();
            if kind.is_symlink() {
                continue;
            }
            let (bytes, device) = on_disk(&meta);
            if kind.is_dir() {
                if device != self.device {
                    continue;
                }
                if depth >= MAX_DEPTH || self.budget.private(&entry.path()) {
                    complete = false;
                    continue;
                }
                let (below, seen) = self.dir(&entry.path(), depth + 1, in_cache || cache);
                total += bytes + below;
                complete &= seen;
            } else {
                total += bytes;
                self.files += 1;
            }
        }
        // A cache folder is offered only when it was measured in full.
        if cache && complete {
            self.caches.lock().unwrap().push(CacheDir { path: dir.to_path_buf(), bytes: total });
        }
        self.complete &= complete;
        (total, complete)
    }
}

/// Measure one path. Cache folders found inside are appended to `caches`.
pub fn measure(path: &Path, budget: &Budget, caches: &Mutex<Vec<CacheDir>>) -> Measured {
    if budget.private(path) {
        return Measured::default();
    }
    let Ok(meta) = fs::symlink_metadata(path) else { return Measured { complete: true, ..Default::default() } };
    let (bytes, device) = on_disk(&meta);
    if meta.file_type().is_symlink() {
        return Measured { complete: true, ..Default::default() };
    }
    if !meta.is_dir() {
        return Measured { bytes, files: 1, complete: true };
    }
    let mut walk = Walk { budget, device, files: 0, complete: !budget.spent(), caches };
    let (below, _) = walk.dir(path, 0, false);
    Measured { bytes: bytes + below, files: walk.files, complete: walk.complete }
}

/// Measure a path with nothing else to report.
pub fn measure_alone(path: &Path, budget: &Budget) -> Measured {
    measure(path, budget, &Mutex::new(Vec::new()))
}

/// One immediate child of a scanned folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Child {
    pub path: PathBuf,
    pub measured: Measured,
}

/// Measure every immediate child of `root`, largest first. Children are
/// walked by a few threads at once so a budget that runs out leaves every
/// large folder partly measured, not the first one fully and the rest at zero.
/// Private children are left out; the caller names them.
pub fn children(root: &Path, budget: &Budget, caches: &Mutex<Vec<CacheDir>>) -> Vec<Child> {
    let mut names: Vec<PathBuf> = fs::read_dir(root).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| !budget.private(p)).collect();
    names.sort();
    let queue = Mutex::new(names);
    let out = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..WORKERS {
            scope.spawn(|| {
                loop {
                    let Some(path) = queue.lock().unwrap().pop() else { break };
                    let measured = measure(&path, budget, caches);
                    out.lock().unwrap().push(Child { path, measured });
                }
            });
        }
    });
    let mut out = out.into_inner().unwrap();
    out.sort_by(|a, b| b.measured.bytes.cmp(&a.measured.bytes).then(a.path.cmp(&b.path)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![7u8; bytes]).unwrap();
    }

    fn roomy() -> Budget {
        Budget::new(Duration::from_secs(30), 1_000_000)
    }

    #[test]
    fn sizes_add_up_and_sort_largest_first() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("big/a.bin"), 300_000);
        write(&dir.path().join("big/deep/b.bin"), 200_000);
        write(&dir.path().join("small/c.bin"), 10_000);
        write(&dir.path().join("loose.bin"), 50_000);
        let caches = Mutex::new(Vec::new());
        let kids = children(dir.path(), &roomy(), &caches);
        let names: Vec<_> = kids.iter().map(|c| c.path.file_name().unwrap().to_str().unwrap().to_string()).collect();
        assert_eq!(names, vec!["big", "loose.bin", "small"]);
        assert!(kids[0].measured.bytes >= 500_000 && kids[0].measured.files == 2 && kids[0].measured.complete);
        assert!(caches.into_inner().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn links_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("huge.bin"), 400_000);
        write(&dir.path().join("in/a.bin"), 1_000);
        std::os::unix::fs::symlink(outside.path(), dir.path().join("in/link")).unwrap();
        let m = measure_alone(&dir.path().join("in"), &roomy());
        assert!(m.bytes < 100_000, "the link's target was counted: {m:?}");
        assert_eq!(m.files, 1);
    }

    #[test]
    fn only_the_outermost_tagged_cache_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("proj/target");
        write(&target.join("debug/x.rlib"), 100_000);
        fs::write(target.join(CACHE_TAG), [CACHE_SIGNATURE, b"\n# cargo"].concat()).unwrap();
        fs::write(target.join("debug").join(CACHE_TAG), CACHE_SIGNATURE).unwrap();
        // A tag with the wrong signature declares nothing.
        write(&dir.path().join("docs/a.txt"), 10);
        fs::write(dir.path().join("docs").join(CACHE_TAG), b"Signature: nope").unwrap();
        let caches = Mutex::new(Vec::new());
        measure(dir.path(), &roomy(), &caches);
        let found = caches.into_inner().unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, target);
        assert!(found[0].bytes >= 100_000);
    }

    #[test]
    fn a_spent_budget_gives_a_floor_marked_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..50 {
            write(&dir.path().join(format!("d/f{i}")), 5_000);
        }
        let caches = Mutex::new(Vec::new());
        let m = measure(&dir.path().join("d"), &Budget::new(Duration::from_secs(30), 10), &caches);
        assert!(!m.complete);
        assert!(m.files < 50);
        let m = measure(&dir.path().join("d"), &Budget::new(Duration::ZERO, 1_000_000), &caches);
        assert!(m.complete || m.files < 50);
    }

    #[test]
    fn a_branch_past_the_depth_limit_marks_the_figure_a_floor() {
        let dir = tempfile::tempdir().unwrap();
        let mut deep = dir.path().join("d");
        for _ in 0..=MAX_DEPTH + 1 {
            deep.push("x");
        }
        write(&deep.join("f"), 10);
        assert!(!measure_alone(&dir.path().join("d"), &roomy()).complete);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_folder_marks_the_figure_a_floor_but_spares_sibling_caches() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a/locked/f"), 10);
        let target = dir.path().join("z/target");
        write(&target.join("x"), 10);
        fs::write(target.join(CACHE_TAG), CACHE_SIGNATURE).unwrap();
        let locked = dir.path().join("a/locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let readable = fs::read_dir(&locked).is_ok(); // root reads anything
        let caches = Mutex::new(Vec::new());
        let m = measure(dir.path(), &roomy(), &caches);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(caches.into_inner().unwrap().len(), 1, "an unreadable folder elsewhere must not hide a complete cache");
        if !readable {
            assert!(!m.complete);
        }
    }
}
