//! Proposed clean-ups, and the one function that deletes.
//!
//! # Propose, then approve
//!
//! Nothing here runs by itself. A clean-up starts as a [`Proposal`]: a list of
//! regenerable places with their sizes, fixed at the moment it was made. It
//! is applied only when the user approves that proposal by its id (a button
//! in the warden's bubble, or `ramwarden storage free` at a terminal), and
//! only within [`PROPOSAL_LIFE`]. A proposal names catalogue ids and folders
//! the daemon found itself; a caller can never supply a path.
//!
//! # What deleting checks, at the moment of deleting
//!
//! 1. Each folder passes [`Places::may_clear`] again: a real folder under the
//!    user's home, temp or local-data folder, reached through no link.
//! 2. A build-output folder must still hold a valid `CACHEDIR.TAG`.
//! 3. Only the folder's **contents** go; the folder stays, so the app that
//!    owns it finds it where it expects.
//! 4. Links inside are unlinked, never followed.
//! 5. In a temp folder, files modified within the minimum age are kept, at
//!    any depth, along with the folders that hold them.
//! 6. The walk stays on the folder's own disk: another disk mounted inside
//!    is left alone.
//! 7. A file that will not delete (open in an app, no permission) is skipped
//!    and counted, never retried harder.
//!
//! The [`Outcome`] reports bytes of entries that were actually removed. If
//! that is zero the outcome is a refusal, not a success.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use ramwarden_platform::Outcome;
use serde::Serialize;

use super::catalog::Places;
use super::scan;

/// How long an unanswered proposal can still be approved.
pub const PROPOSAL_LIFE: Duration = Duration::from_secs(15 * 60);

/// One place in a proposal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Line {
    pub id: String,
    pub label: String,
    pub bytes: u64,
    #[serde(skip)]
    pub paths: Vec<PathBuf>,
    /// Keep recent entries (temp folders).
    #[serde(skip)]
    pub aged: bool,
    /// The folder vouches for itself with a cache tag, re-read before deleting.
    #[serde(skip)]
    pub tagged: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    Pending,
    Applied { freed_bytes: u64, skipped: u64, notes: Vec<String> },
    Declined,
    Expired,
}

/// A clean-up waiting for the user's answer, or its result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Proposal {
    pub id: String,
    pub lines: Vec<Line>,
    pub bytes: u64,
    /// Seconds since the Unix epoch.
    pub created: u64,
    #[serde(flatten)]
    pub state: State,
}

impl Proposal {
    pub fn new(lines: Vec<Line>, unix: u64) -> Self {
        let bytes = lines.iter().map(|l| l.bytes).sum();
        Proposal { id: uuid::Uuid::new_v4().to_string(), lines, bytes, created: unix, state: State::Pending }
    }

    /// Mark a pending proposal expired once it has outlived [`PROPOSAL_LIFE`].
    pub fn age(&mut self, unix: u64) {
        if self.state == State::Pending && unix.saturating_sub(self.created) > PROPOSAL_LIFE.as_secs() {
            self.state = State::Expired;
        }
    }
}

/// What clearing one folder did.
#[derive(Debug, Default, PartialEq, Eq)]
struct Cleared {
    bytes: u64,
    removed: u64,
    skipped: u64,
}

/// Delete the contents of `dir`, keeping files newer than `keep_newer`.
///
/// The walk goes file by file rather than handing a whole folder to
/// `remove_dir_all`, for two reasons. Age is checked on every file, not on
/// the top-level entry: an old temp folder can hold a file written a moment
/// ago, and that file stays (with the folders above it). And bytes are
/// counted per file actually removed, so a folder that half-deletes reports
/// the half that went instead of nothing.
fn clear_contents(dir: &Path, keep_newer: Option<Duration>) -> Cleared {
    let mut out = Cleared::default();
    let Ok(meta) = fs::symlink_metadata(dir) else { return out };
    Clearer { now: SystemTime::now(), keep_newer, device: scan::on_disk(&meta).1 }.contents(dir, 0, &mut out);
    out
}

/// Deepest folder the clean-up descends into. Anything below is kept.
const MAX_CLEAR_DEPTH: usize = 64;

struct Clearer {
    now: SystemTime,
    keep_newer: Option<Duration>,
    device: u64,
}

impl Clearer {
    fn old_enough(&self, meta: &fs::Metadata) -> bool {
        let Some(min_age) = self.keep_newer else { return true };
        let age = meta.modified().ok().and_then(|m| self.now.duration_since(m).ok());
        age.is_some_and(|a| a >= min_age)
    }

    /// Clear what is inside `dir`. True when `dir` is left empty.
    fn contents(&self, dir: &Path, depth: usize, out: &mut Cleared) -> bool {
        let Ok(entries) = fs::read_dir(dir) else {
            out.skipped += 1;
            return false;
        };
        let mut empty = true;
        for entry in entries {
            // `DirEntry::metadata` does not follow links.
            let Ok((entry, meta)) = entry.and_then(|e| e.metadata().map(|m| (e, m))) else {
                out.skipped += 1;
                empty = false;
                continue;
            };
            let path = entry.path();
            if meta.is_dir() {
                if depth >= MAX_CLEAR_DEPTH || scan::on_disk(&meta).1 != self.device || !self.contents(&path, depth + 1, out) {
                    empty = false;
                    continue;
                }
                // Emptied: the folder itself goes. Its own date changed as
                // its contents went, so it is not age-checked.
                match fs::remove_dir(&path) {
                    Ok(()) => {
                        out.bytes += scan::on_disk(&meta).0;
                        out.removed += 1;
                    }
                    Err(_) => {
                        out.skipped += 1;
                        empty = false;
                    }
                }
                continue;
            }
            if !self.old_enough(&meta) {
                empty = false;
                continue;
            }
            // A link is removed as a link and counts as nothing; its target
            // is never touched.
            let bytes = if meta.file_type().is_symlink() { 0 } else { scan::on_disk(&meta).0 };
            match fs::remove_file(&path) {
                Ok(()) => {
                    out.bytes += bytes;
                    out.removed += 1;
                }
                Err(_) => {
                    out.skipped += 1;
                    empty = false;
                }
            }
        }
        empty
    }
}

/// Apply every line of a proposal. Returns what was really removed.
pub fn apply(places: &Places, lines: &[Line], temp_min_age: Duration) -> (Outcome, u64) {
    let mut outcome = Outcome::default();
    let mut skipped = 0;
    for line in lines {
        for dir in &line.paths {
            if !places.may_clear(dir) {
                outcome.notes.push(format!("{}: left alone, it is no longer a plain folder inside your user folders", line.label));
                continue;
            }
            if line.tagged && !scan::is_cache_dir(dir) {
                outcome.notes.push(format!("{}: left alone, it no longer marks itself as a cache", line.label));
                continue;
            }
            let cleared = clear_contents(dir, line.aged.then_some(temp_min_age));
            outcome.bytes_freed += cleared.bytes;
            skipped += cleared.skipped;
            if cleared.skipped > 0 {
                outcome.notes.push(format!("{}: {} item(s) in use or not removable were kept", line.label, cleared.skipped));
            }
        }
    }
    if outcome.bytes_freed == 0 {
        let mut refused = Outcome::refused("nothing was removed");
        refused.notes.extend(outcome.notes);
        return (refused, skipped);
    }
    (outcome, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::scan::{CACHE_SIGNATURE, CACHE_TAG};

    fn write(path: &Path, bytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![1u8; bytes]).unwrap();
    }

    fn line(id: &str, path: PathBuf) -> Line {
        Line { id: id.into(), label: id.into(), bytes: 0, paths: vec![path], aged: false, tagged: false }
    }

    fn places(home: &Path) -> Places {
        Places { os: "linux", home: home.to_path_buf(), ..Default::default() }
    }

    #[test]
    fn contents_go_and_the_folder_stays() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join(".cache");
        write(&cache.join("app/blob"), 200_000);
        write(&cache.join("loose"), 50_000);
        let (outcome, skipped) = apply(&places(dir.path()), &[line("app-caches", cache.clone())], Duration::ZERO);
        assert!(outcome.bytes_freed >= 250_000);
        assert_eq!(skipped, 0);
        assert!(cache.is_dir());
        assert_eq!(fs::read_dir(&cache).unwrap().count(), 0);
    }

    #[test]
    fn an_empty_folder_is_a_refusal_not_a_success() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join(".cache");
        fs::create_dir_all(&cache).unwrap();
        let (outcome, _) = apply(&places(dir.path()), &[line("app-caches", cache)], Duration::ZERO);
        assert!(outcome.did_nothing());
        assert_eq!(outcome.notes[0], "nothing was removed");
    }

    #[cfg(unix)]
    #[test]
    fn links_are_unlinked_and_their_targets_survive() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("precious.txt"), 1_000);
        let cache = dir.path().join(".cache");
        write(&cache.join("real"), 10_000);
        std::os::unix::fs::symlink(outside.path(), cache.join("to-dir")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("precious.txt"), cache.join("to-file")).unwrap();
        apply(&places(dir.path()), &[line("app-caches", cache.clone())], Duration::ZERO);
        assert_eq!(fs::read_dir(&cache).unwrap().count(), 0);
        assert!(outside.path().join("precious.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_swapped_for_a_link_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("precious.txt"), 1_000);
        std::os::unix::fs::symlink(outside.path(), dir.path().join(".cache")).unwrap();
        let (outcome, _) = apply(&places(dir.path()), &[line("app-caches", dir.path().join(".cache"))], Duration::ZERO);
        assert!(outcome.did_nothing());
        assert!(outside.path().join("precious.txt").exists());
    }

    #[test]
    fn build_output_needs_its_tag_at_the_moment_of_deleting() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("proj/target");
        write(&target.join("debug/a"), 10_000);
        let mut l = line("build-1", target.clone());
        l.tagged = true;
        let (outcome, _) = apply(&places(dir.path()), std::slice::from_ref(&l), Duration::ZERO);
        assert!(outcome.did_nothing(), "no tag, no delete");
        assert!(target.join("debug/a").exists());
        fs::write(target.join(CACHE_TAG), CACHE_SIGNATURE).unwrap();
        let (outcome, _) = apply(&places(dir.path()), &[l], Duration::ZERO);
        assert!(outcome.bytes_freed >= 10_000);
        assert!(!target.join("debug").exists());
    }

    #[test]
    fn recent_temp_files_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("Temp");
        write(&temp.join("in-use.tmp"), 10_000);
        let p = Places { os: "windows", home: PathBuf::new(), temp: Some(temp.clone()), ..Default::default() };
        let mut l = line("temp-files", temp.clone());
        l.aged = true;
        let (outcome, _) = apply(&p, std::slice::from_ref(&l), Duration::from_secs(86_400));
        assert!(outcome.did_nothing());
        assert!(temp.join("in-use.tmp").exists());
        let (outcome, _) = apply(&p, &[l], Duration::ZERO);
        assert!(outcome.bytes_freed >= 10_000);
    }

    fn age(path: &Path, secs: u64) {
        let when = SystemTime::now() - Duration::from_secs(secs);
        fs::File::options().write(true).open(path).unwrap().set_modified(when).unwrap();
    }

    #[test]
    fn an_old_temp_folder_keeps_the_new_file_inside_it() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("Temp");
        write(&temp.join("job/old.tmp"), 20_000);
        write(&temp.join("job/fresh.tmp"), 10_000);
        write(&temp.join("stale/a/b.tmp"), 30_000);
        age(&temp.join("job/old.tmp"), 3 * 86_400);
        age(&temp.join("stale/a/b.tmp"), 3 * 86_400);
        let p = Places { os: "linux", home: PathBuf::new(), temp: Some(temp.clone()), ..Default::default() };
        let mut l = line("temp-files", temp.clone());
        l.aged = true;
        let (outcome, skipped) = apply(&p, &[l], Duration::from_secs(86_400));
        assert_eq!(skipped, 0);
        assert!(temp.join("job/fresh.tmp").exists(), "a file written a moment ago survives its old folder");
        assert!(!temp.join("job/old.tmp").exists());
        assert!(!temp.join("stale").exists(), "a folder left empty goes too");
        // Counted per file removed: both old files, not the kept one.
        assert!(outcome.bytes_freed >= 50_000 && outcome.bytes_freed < 70_000, "{}", outcome.bytes_freed);
    }

    #[test]
    fn proposals_expire() {
        let mut p = Proposal::new(vec![], 1_000);
        p.age(1_000 + 60);
        assert_eq!(p.state, State::Pending);
        p.age(1_000 + 16 * 60);
        assert_eq!(p.state, State::Expired);
    }
}
