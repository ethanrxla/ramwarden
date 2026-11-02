//! The process table as `ramwarden-platform` samples.

use std::time::{Duration, Instant};

use ramwarden_platform::{Cadence, ProcessSample, ProcessSource, Result};

use crate::groups::{group_name, roots};
use crate::owner::Identities;
use crate::{net, snapshot};

/// The TCP table is re-read at least this often, and no sooner than
/// [`SOCKETS_SOONEST`] after a new process appears. Listening ports change far
/// less often than the CPU and working-set figures a tick exists to refresh.
/// Same cadence as Linux and macOS (see `ramwarden_platform::cache`).
pub const SOCKETS_EVERY: Duration = Duration::from_secs(10);
pub const SOCKETS_SOONEST: Duration = Duration::from_secs(3);

/// Samples every process with its owner, sockets and group.
///
/// Proportional size is the private working set, which `snapshot` reads for
/// every process in the same call — so unlike Linux there is no measurement
/// floor and no way for accounting to silently degrade to resident size.
///
/// Owner and command line are cached per process instance (PID plus creation
/// time) in [`Identities`], so a process is opened once in its life, not once
/// a tick. The socket table is cached on a [`Cadence`].
pub struct NtProcesses {
    identities: Identities,
    sockets: net::Sockets,
    tables: Cadence,
}

impl Default for NtProcesses {
    fn default() -> Self {
        Self::new()
    }
}

impl NtProcesses {
    pub fn new() -> Self {
        NtProcesses {
            identities: Identities::new(),
            sockets: net::Sockets::default(),
            tables: Cadence::new(SOCKETS_EVERY, SOCKETS_SOONEST),
        }
    }
}

impl ProcessSource for NtProcesses {
    fn sample(&mut self) -> Result<Vec<ProcessSample>> {
        let table = snapshot::processes()?;
        self.identities.retain(&table);
        let session = self.identities.session();
        let appeared = table.iter().any(|p| !self.identities.knows(p));
        let tick = Instant::now();
        if self.tables.due(tick, appeared) {
            self.tables.mark(tick);
            self.sockets = net::sockets();
        }
        let sockets = &self.sockets;
        let now = snapshot::now_filetime();

        // Groups cover only the user's own processes, exactly as NtGroups
        // builds them, so a sample's `group` always names a reachable group.
        let ids: Vec<_> = table.iter().map(|p| self.identities.get(p)).collect();
        let reachable: Vec<&snapshot::NtProcess> = table
            .iter()
            .zip(&ids)
            .filter(|(p, id)| {
                p.session == session
                    && !p.is_kernel()
                    && id.owner == ramwarden_platform::Owner::CurrentUser
            })
            .map(|(p, _)| p)
            .collect();
        let group_of = roots(&reachable);
        let by_pid: std::collections::HashMap<u32, &snapshot::NtProcess> =
            reachable.iter().map(|p| (p.pid, *p)).collect();

        Ok(table
            .iter()
            .zip(ids)
            .filter(|(p, _)| p.pid != 0)
            .map(|(p, id)| ProcessSample {
                pid: p.pid as i32,
                ppid: p.ppid as i32,
                started: p.create_time as u64,
                name: p.name.clone(),
                cmdline: id.cmdline,
                owner: id.owner,
                cpu_seconds: p.cpu_seconds(),
                resident: p.working_set,
                proportional: p.private_working_set,
                state: match (p.threads, p.suspended) {
                    (0, _) => 'Z',
                    (_, true) => 'T',
                    _ => 'S',
                },
                has_tty: false,
                age_seconds: p.age_seconds(now),
                listening_ports: sockets.ports_for(p.pid).to_vec(),
                established: sockets.established_for(p.pid),
                group: group_of
                    .get(&p.pid)
                    .and_then(|root| by_pid.get(root))
                    .map(|root| group_name(root)),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_sampled_as_ours_and_grouped() {
        let mut src = NtProcesses::new();
        let all = src.sample().unwrap();
        let me = all.iter().find(|s| s.pid == std::process::id() as i32).unwrap();
        assert_eq!(me.owner, ramwarden_platform::Owner::CurrentUser);
        assert!(me.proportional > 0 && me.resident >= me.proportional);
        assert!(me.group.is_some(), "the user's own process belongs to a group");
        assert_eq!(me.state, 'S');
    }

    #[test]
    fn services_are_never_grouped() {
        let mut src = NtProcesses::new();
        for s in src.sample().unwrap() {
            if s.owner == ramwarden_platform::Owner::System {
                assert!(s.group.is_none(), "{} (pid {}) was grouped", s.name, s.pid);
            }
        }
    }
}
