//! A record of what RamWarden has frozen, so a crash cannot strand it.
//!
//! The registry of suspended apps lives in memory. If the daemon is killed hard,
//! restarted by launchd or crashes, that registry is gone while the processes
//! stay stopped, and nothing would ever wake them. The monitor writes the current
//! set here after every change; on startup anything listed that is still stopped
//! under the same name is thawed.

use std::path::Path;

use ramwarden_core::detector::Detector;
use ramwarden_platform::ProcessControl;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Frozen {
    pub name: String,
    pub pid: i32,
}

pub fn write(path: &Path, frozen: &[Frozen]) {
    let result = if frozen.is_empty() {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        serde_json::to_vec(frozen).map_err(std::io::Error::other).and_then(|b| std::fs::write(path, b))
    };
    if let Err(e) = result {
        tracing::warn!("could not update {}: {e}", path.display());
    }
}

/// Thaw whatever a previous run left frozen. Returns the names woken.
///
/// A listed pid is woken only if it still has the recorded name and is stopped:
/// a recycled pid, or a process the user has since continued, is left alone.
pub fn thaw_leftovers(path: &Path, det: &Detector, control: &dyn ProcessControl) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path) else { return Vec::new() };
    let listed: Vec<Frozen> = serde_json::from_slice(&bytes).unwrap_or_default();
    let mut woke = Vec::new();
    for f in &listed {
        let still_ours = det.get(f.pid).is_some_and(|s| s.name == f.name && s.state == 'T');
        if still_ours && control.open(f.pid).and_then(|h| h.resume()).is_ok() {
            woke.push(format!("{} (pid {})", f.name, f.pid));
        }
    }
    let _ = std::fs::remove_file(path);
    woke
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramwarden_core::roles::classify;
    use ramwarden_core::signals::Signals;
    use ramwarden_platform::fake;

    fn detector(pid: i32, name: &str, state: char) -> Detector {
        let mut s = Signals::new(pid, name, classify(name, "", Some(501)));
        s.state = state;
        Detector::preloaded(true, vec![s])
    }

    #[test]
    fn write_then_read_round_trips_and_an_empty_set_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("suspended.json");
        write(&path, &[Frozen { name: "node".into(), pid: 7 }]);
        let back: Vec<Frozen> = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back, vec![Frozen { name: "node".into(), pid: 7 }]);
        write(&path, &[]);
        assert!(!path.exists());
    }

    #[test]
    fn a_missing_or_corrupt_journal_is_harmless() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("suspended.json");
        let w = fake::World::default();
        let platform = fake::platform(std::sync::Arc::new(std::sync::Mutex::new(w)));
        assert!(thaw_leftovers(&path, &detector(1, "x", 'T'), platform.control.as_ref()).is_empty());
        std::fs::write(&path, b"not json").unwrap();
        assert!(thaw_leftovers(&path, &detector(1, "x", 'T'), platform.control.as_ref()).is_empty());
        assert!(!path.exists(), "a corrupt journal is discarded, not retried forever");
    }

    #[test]
    fn a_listed_process_is_woken_only_if_it_is_still_stopped_under_the_same_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("suspended.json");
        let world = std::sync::Arc::new(std::sync::Mutex::new(fake::World {
            processes: vec![fake::process(10, "node", 1 << 20)],
            suspended: vec![10],
            ..Default::default()
        }));
        let platform = fake::platform(world.clone());

        let journal = [Frozen { name: "node".into(), pid: 10 }, Frozen { name: "node".into(), pid: 11 }];
        write(&path, &journal);
        // pid 10: stopped, same name. pid 11: not running at all.
        let woke = thaw_leftovers(&path, &detector(10, "node", 'T'), platform.control.as_ref());
        assert_eq!(woke, vec!["node (pid 10)".to_string()]);
        assert!(world.lock().unwrap().suspended.is_empty(), "SIGCONT delivered");
        assert!(!path.exists());

        // A recycled pid (different name) or an already-running process is left alone.
        write(&path, &[Frozen { name: "node".into(), pid: 10 }]);
        assert!(thaw_leftovers(&path, &detector(10, "something-else", 'T'), platform.control.as_ref()).is_empty());
        write(&path, &[Frozen { name: "node".into(), pid: 10 }]);
        assert!(thaw_leftovers(&path, &detector(10, "node", 'S'), platform.control.as_ref()).is_empty());
    }
}
