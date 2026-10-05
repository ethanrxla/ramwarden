//! Applications as reclaim groups — the Windows counterpart of cgroup scopes.
//!
//! Windows has no per-application memory container a user can reclaim from, so
//! a group is built from the process tree: an application's top-level process
//! and every descendant running the same executable. A browser and its twenty
//! renderers, or Discord and its helpers, become one group with one label.
//!
//! # The safety invariant
//!
//! Only processes owned by the current user, in the current interactive
//! session, are ever placed in a group. Services, other users' processes and
//! anything in session 0 are unreachable through this module by construction —
//! the same property the Linux cgroup hierarchy gives by being rooted at
//! `user@<uid>.service`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use ramwarden_platform::{AppGroup, Error, GroupSource, Owner, Result};

use crate::owner::Identities;
use crate::snapshot::{self, NtProcess};
use crate::workingset::{self, Limits};

/// pid -> the pid of its group's root.
///
/// A process joins its parent's group when the parent runs the same executable
/// *and is older than it*. The age check matters: Windows reuses PIDs, so a
/// stale parent PID can point at an unrelated process started later.
pub fn roots(procs: &[NtProcess]) -> HashMap<u32, u32> {
    let by_pid: HashMap<u32, &NtProcess> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut out = HashMap::with_capacity(procs.len());
    for p in procs {
        let mut cur = p;
        let mut seen = HashSet::from([p.pid]);
        while let Some(parent) = by_pid.get(&cur.ppid) {
            let same_app = parent.name.eq_ignore_ascii_case(&cur.name);
            let genuinely_parent = parent.create_time <= cur.create_time;
            if !same_app || !genuinely_parent || !seen.insert(parent.pid) {
                break;
            }
            cur = parent;
        }
        out.insert(p.pid, cur.pid);
    }
    out
}

/// The group key for a root process.
pub fn group_name(root: &NtProcess) -> String {
    format!("{}#{}", root.name, root.pid)
}

/// What RamWarden changed on one process, so it can be put back.
#[derive(Clone, Debug)]
struct Capped {
    group: String,
    original: Limits,
    cap: u64,
}

/// Every application group in the user's session.
pub struct NtGroups {
    identities: Mutex<Identities>,
    caps: Arc<Mutex<HashMap<u32, Capped>>>,
}

impl Default for NtGroups {
    fn default() -> Self {
        Self::new()
    }
}

impl NtGroups {
    pub fn new() -> Self {
        NtGroups {
            identities: Mutex::new(Identities::new()),
            caps: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The user's own processes in the interactive session, and nothing else.
    fn reachable(&self, table: Vec<NtProcess>) -> Vec<NtProcess> {
        let mut ids = self.identities.lock().unwrap();
        let session = ids.session();
        ids.retain(&table);
        table
            .into_iter()
            .filter(|p| p.session == session && !p.is_kernel())
            .filter(|p| ids.get(p).owner == Owner::CurrentUser)
            .collect()
    }
}

impl GroupSource for NtGroups {
    fn ready(&self) -> Result<()> {
        Ok(())
    }

    fn groups(&self) -> Result<Vec<Box<dyn AppGroup>>> {
        let procs = self.reachable(snapshot::processes()?);
        let roots = roots(&procs);
        let by_pid: HashMap<u32, &NtProcess> = procs.iter().map(|p| (p.pid, p)).collect();

        let mut members: HashMap<u32, Vec<u32>> = HashMap::new();
        for (pid, root) in &roots {
            members.entry(*root).or_default().push(*pid);
        }

        let mut out: Vec<Box<dyn AppGroup>> = members
            .into_iter()
            .filter_map(|(root, mut pids)| {
                let r = by_pid.get(&root)?;
                pids.sort_unstable();
                Some(Box::new(NtGroup {
                    name: group_name(r),
                    label: r.name.clone(),
                    pids,
                    caps: self.caps.clone(),
                }) as Box<dyn AppGroup>)
            })
            .collect();
        out.sort_by(|a, b| a.name().cmp(b.name()));
        Ok(out)
    }
}

/// One application's processes.
pub struct NtGroup {
    name: String,
    label: String,
    pids: Vec<u32>,
    caps: Arc<Mutex<HashMap<u32, Capped>>>,
}

impl NtGroup {
    /// (pid, private working set) for every member still alive.
    fn sizes(&self) -> Vec<(u32, u64)> {
        self.pids
            .iter()
            .filter_map(|pid| workingset::private_working_set_of(*pid).ok().map(|ws| (*pid, ws)))
            .collect()
    }
}

impl AppGroup for NtGroup {
    fn name(&self) -> &str {
        &self.name
    }

    fn label(&self) -> String {
        self.label.clone()
    }

    fn pids(&self) -> Result<Vec<i32>> {
        Ok(self.pids.iter().map(|p| *p as i32).collect())
    }

    fn current(&self) -> Result<u64> {
        Ok(self.sizes().iter().map(|(_, ws)| ws).sum())
    }

    /// The private working set: every private page could be trimmed, and the
    /// ladder only ever asks for a fraction of it.
    fn reclaimable(&self) -> Result<u64> {
        self.current()
    }

    /// Trim members largest-first until `bytes` have measurably moved.
    fn reclaim(&self, bytes: u64) -> Result<u64> {
        let mut sizes = self.sizes();
        if sizes.is_empty() {
            return Err(Error::missing(&self.name));
        }
        sizes.sort_by_key(|(_, ws)| std::cmp::Reverse(*ws));

        let mut freed = 0u64;
        let mut last_err = None;
        let mut any_ok = false;
        for (pid, _) in sizes {
            match workingset::trim(pid) {
                Ok(moved) => {
                    any_ok = true;
                    freed += moved;
                }
                Err(e) => last_err = Some(e),
            }
            if freed >= bytes {
                break;
            }
        }
        match (any_ok, last_err) {
            (false, Some(e)) => Err(e),
            _ => Ok(freed),
        }
    }

    fn soft_cap(&self) -> Result<Option<u64>> {
        let caps = self.caps.lock().unwrap();
        let mine: Vec<u64> = caps.values().filter(|c| c.group == self.name).map(|c| c.cap).collect();
        Ok(if mine.is_empty() { None } else { Some(mine.iter().sum()) })
    }

    /// Divide `bytes` among members by their share of the group's private
    /// working set, recording each member's original limits the first time.
    fn set_soft_cap(&self, bytes: u64) -> Result<()> {
        let sizes = self.sizes();
        let total: u64 = sizes.iter().map(|(_, ws)| ws).sum();
        if total == 0 {
            return Err(Error::missing(&self.name));
        }
        let mut caps = self.caps.lock().unwrap();
        for (pid, ws) in sizes {
            let share = (bytes as f64 * ws as f64 / total as f64) as u64;
            let original = match caps.get(&pid) {
                Some(c) => c.original,
                None => workingset::limits(pid)?,
            };
            workingset::cap(pid, share)?;
            caps.insert(
                pid,
                Capped {
                    group: self.name.clone(),
                    original,
                    cap: share,
                },
            );
        }
        Ok(())
    }

    /// Restore every member RamWarden capped under this group's name —
    /// including any no longer in the group, so a cap never outlives pressure.
    fn clear_soft_cap(&self) -> Result<()> {
        let mut caps = self.caps.lock().unwrap();
        let mine: Vec<(u32, Limits)> = caps
            .iter()
            .filter(|(_, c)| c.group == self.name)
            .map(|(pid, c)| (*pid, c.original))
            .collect();
        for (pid, original) in mine {
            // An exited process took its limits with it; nothing to restore.
            let _ = workingset::restore(pid, original);
            caps.remove(&pid);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, ppid: u32, name: &str, created: i64) -> NtProcess {
        NtProcess {
            pid,
            ppid,
            name: name.into(),
            create_time: created,
            ..Default::default()
        }
    }

    #[test]
    fn a_browser_and_its_renderers_form_one_group() {
        let table = [
            p(100, 50, "explorer", 1),
            p(200, 100, "brave", 10),
            p(201, 200, "brave", 11),
            p(202, 200, "brave", 12),
            p(203, 201, "brave", 13),
            p(300, 100, "Discord", 20),
        ];
        let r = roots(&table);
        assert_eq!(r[&201], 200);
        assert_eq!(r[&202], 200);
        assert_eq!(r[&203], 200, "grandchildren join the root");
        assert_eq!(r[&300], 300, "a different executable is its own group");
        assert_eq!(r[&200], 200);
    }

    /// Windows reuses PIDs: a child's recorded parent PID can belong to a
    /// newer, unrelated process with the same name.
    #[test]
    fn a_reused_parent_pid_does_not_merge_groups() {
        let table = [p(500, 1, "brave", 100), p(400, 500, "brave", 50)];
        let r = roots(&table);
        assert_eq!(r[&400], 400, "the 'parent' was created after the child");
    }

    #[test]
    fn a_parent_cycle_terminates() {
        let table = [p(1, 2, "x", 0), p(2, 1, "x", 0)];
        let r = roots(&table);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn names_compare_case_insensitively() {
        let table = [p(10, 1, "Discord", 1), p(11, 10, "discord", 2)];
        assert_eq!(roots(&table)[&11], 10);
    }

    /// Against the live machine: no system or session-0 process is reachable.
    #[test]
    fn the_live_groups_contain_only_this_users_processes() {
        let groups = NtGroups::new();
        let table = snapshot::processes().unwrap();
        let services: HashSet<i32> = table
            .iter()
            .filter(|p| p.session == 0 || p.pid <= 4)
            .map(|p| p.pid as i32)
            .collect();
        for g in groups.groups().unwrap() {
            for pid in g.pids().unwrap() {
                assert!(!services.contains(&pid), "{} reached session 0 pid {pid}", g.name());
            }
        }
    }
}
