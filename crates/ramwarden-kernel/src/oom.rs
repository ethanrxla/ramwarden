//! Make the kernel's OOM killer enact RamWarden's ranking.
//!
//! # Why this is worth doing
//!
//! RamWarden's README opens by promising to reclaim memory "instead of letting
//! the OOM killer decide for you", and v1 never delivered on it: if the ladder
//! lost the race, the kernel picked a victim by its own heuristic — which scores
//! on resident size and therefore tends to choose the browser holding all the
//! user's work.
//!
//! `oom_score_adj` biases that choice, in the range -1000..=1000, and a process
//! can set it for anything it may signal. So RamWarden can hand the kernel its
//! own verdicts in advance: push idle, reclaimable applications up the list and
//! pull the compositor and the user's focused work down. If pressure outruns the
//! ladder entirely, the kernel still kills the thing RamWarden would have
//! chosen — no daemon involvement needed at the moment it matters.
//!
//! Note the asymmetry: raising a score needs no privileges, but *lowering* one
//! below its current value requires `CAP_SYS_RESOURCE`. Protecting the
//! compositor is therefore best-effort, while deprioritising idle apps — the
//! part that does the work — always succeeds.

use std::fs;

use crate::{Error, Result, Root};

/// The kernel's permitted range for `oom_score_adj`.
pub const MIN: i32 = -1000;
/// A score of 1000 guarantees selection; use [`FIRST_TO_GO`] instead unless you
/// truly mean "kill this before anything else, always".
pub const MAX: i32 = 1000;

/// Suggested biases, so the ladder and the UI agree on what a score means.
pub mod bias {
    /// Structurally protected: the compositor, a hypervisor, a container
    /// runtime. Killing one of these takes the session down with it.
    pub const PROTECTED: i32 = -800;
    /// The user is demonstrably using this right now.
    pub const IN_USE: i32 = -200;
    /// No activity signal in the sample window, but not opted in for reclaim.
    pub const NEUTRAL: i32 = 0;
    /// Idle and on the watchlist: what RamWarden would suspend first, so it is
    /// also what the kernel should kill first.
    pub const IDLE: i32 = 500;
}

/// Kill this before anything else.
pub const FIRST_TO_GO: i32 = 900;

// The ladder's whole premise is that the kernel's fallback choice agrees with
// RamWarden's: whatever it would reclaim first must also be what the kernel
// kills first. Enforced at compile time, so reordering the constants cannot
// silently invert the policy.
const _: () = {
    assert!(bias::PROTECTED < bias::IN_USE);
    assert!(bias::IN_USE < bias::NEUTRAL);
    assert!(bias::NEUTRAL < bias::IDLE);
    assert!(bias::IDLE < FIRST_TO_GO);
    assert!(bias::PROTECTED >= MIN);
    assert!(FIRST_TO_GO <= MAX);
};

/// Read a process's current OOM bias.
pub fn score_adj(root: &Root, pid: i32) -> Result<i32> {
    let path = root.proc_pid(pid, "oom_score_adj");
    let text = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
    text.trim()
        .parse()
        .map_err(|_| Error::parse(&path, format!("{:?} is not an integer", text.trim())))
}

/// The kernel's own composite badness score, 0..=1000. Useful for showing the
/// user who the kernel would currently pick.
pub fn score(root: &Root, pid: i32) -> Result<u32> {
    let path = root.proc_pid(pid, "oom_score");
    let text = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
    text.trim()
        .parse()
        .map_err(|_| Error::parse(&path, format!("{:?} is not an integer", text.trim())))
}

/// Bias the OOM killer's choice for one process.
///
/// `adj` is clamped into the kernel's range rather than rejected: callers derive
/// it from policy constants and a rounding error should not abort a tick.
pub fn set_score_adj(root: &Root, pid: i32, adj: i32) -> Result<()> {
    let path = root.proc_pid(pid, "oom_score_adj");
    let clamped = adj.clamp(MIN, MAX);
    fs::write(&path, clamped.to_string()).map_err(|e| match e.kind() {
        // Lowering a score below its current value needs CAP_SYS_RESOURCE.
        std::io::ErrorKind::PermissionDenied => Error::Denied {
            path: path.clone(),
            hint: "lowering oom_score_adj needs CAP_SYS_RESOURCE; raising it does not",
        },
        _ => Error::io(&path, e),
    })
}

/// Apply a bias to every process in a group, reporting how many took it.
///
/// Partial success is the expected outcome, not a failure: processes exit while
/// we walk them, and protective (negative) biases are refused without
/// `CAP_SYS_RESOURCE`. Returning a count lets the caller log what actually
/// happened instead of claiming the whole group was ranked.
pub fn set_group(root: &Root, pids: &[i32], adj: i32) -> usize {
    pids.iter()
        .filter(|&&pid| set_score_adj(root, pid, adj).is_ok())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(pids: &[(i32, i32)]) -> (tempfile::TempDir, Root) {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        for (pid, adj) in pids {
            let d = root.join(format!("proc/{pid}"));
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("oom_score_adj"), format!("{adj}\n")).unwrap();
            fs::write(d.join("oom_score"), "667\n").unwrap();
        }
        (dir, root)
    }

    #[test]
    fn reads_and_writes_a_score() {
        let (_d, root) = fixture(&[(42, 0)]);
        assert_eq!(score_adj(&root, 42).unwrap(), 0);
        set_score_adj(&root, 42, bias::IDLE).unwrap();
        assert_eq!(score_adj(&root, 42).unwrap(), 500);
    }

    #[test]
    fn reads_the_kernels_composite_score() {
        let (_d, root) = fixture(&[(42, 0)]);
        assert_eq!(score(&root, 42).unwrap(), 667);
    }

    #[test]
    fn clamps_rather_than_rejecting_out_of_range_values() {
        let (_d, root) = fixture(&[(42, 0)]);
        set_score_adj(&root, 42, 99_999).unwrap();
        assert_eq!(score_adj(&root, 42).unwrap(), MAX);
        set_score_adj(&root, 42, -99_999).unwrap();
        assert_eq!(score_adj(&root, 42).unwrap(), MIN);
    }

    #[test]
    fn group_writes_report_only_the_processes_that_took_it() {
        let (_d, root) = fixture(&[(1, 0), (2, 0)]);
        // pid 3 stands in for one that exited between listing and writing.
        assert_eq!(set_group(&root, &[1, 2, 3], bias::IDLE), 2);
        assert_eq!(score_adj(&root, 1).unwrap(), 500);
        assert_eq!(score_adj(&root, 2).unwrap(), 500);
    }

    #[test]
    fn a_missing_process_is_a_clear_not_found() {
        let (_d, root) = fixture(&[]);
        assert!(score_adj(&root, 999).unwrap_err().is_missing());
    }

    #[test]
    fn a_garbled_score_is_a_parse_error() {
        let (_d, root) = fixture(&[(42, 0)]);
        fs::write(root.proc_pid(42, "oom_score_adj"), "banana\n").unwrap();
        assert!(matches!(score_adj(&root, 42), Err(Error::Parse { .. })));
    }
}
