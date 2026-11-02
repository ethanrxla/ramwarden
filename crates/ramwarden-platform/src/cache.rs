//! Caching shared by every process source: what to keep between ticks, and
//! when a slow whole-system read is due again.
//!
//! # Why sampling is split into cheap and expensive halves
//!
//! A monitor tick needs fresh CPU times and resident sizes for every process,
//! because "was it busy?" is a difference between two samples. It does not need
//! a fresh command line, owner, proportional size or socket table: those either
//! never change for the life of a process, or change slowly enough that a few
//! seconds of staleness cannot change a decision. Re-reading them on every tick
//! was most of what a tick cost. On Linux the socket scan alone walks every file
//! descriptor of every process (a `readlink` each) to find who owns which
//! socket, and a `smaps_rollup` read makes the kernel walk every mapping of the
//! process, thousands for a browser.
//!
//! So each process source keeps the expensive facts in an [`Instances`] cache
//! and refreshes whole-system tables on a [`Cadence`].
//!
//! # Process identity
//!
//! A cache keyed by PID alone would hand a recycled PID the previous owner's
//! command line, and with it the previous owner's role and protection. Every
//! entry is therefore keyed by PID *and* start time (`starttime` on Linux, the
//! creation FILETIME on Windows, `pbi_start` on macOS). A different start time
//! is a different process, and its entry is replaced, never reused.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::time::{Duration, Instant};

/// When a slow, whole-system read is due again.
///
/// Due when `every` has passed since the last read, or sooner — but no sooner
/// than `soonest` — when the caller has seen something change that the read
/// would reflect, such as a new process that may have opened a listening
/// socket.
#[derive(Clone, Debug)]
pub struct Cadence {
    every: Duration,
    soonest: Duration,
    last: Option<Instant>,
}

impl Cadence {
    pub const fn new(every: Duration, soonest: Duration) -> Self {
        Cadence { every, soonest, last: None }
    }

    /// Whether to read again at `now`. Always true before the first read.
    pub fn due(&self, now: Instant, changed: bool) -> bool {
        match self.last {
            None => true,
            Some(t) => {
                let age = now.saturating_duration_since(t);
                age >= self.every || (changed && age >= self.soonest)
            }
        }
    }

    /// Record that the read happened at `now`.
    pub fn mark(&mut self, now: Instant) {
        self.last = Some(now);
    }

    /// Make the next [`Cadence::due`] true, e.g. after a read failed.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

struct Slot<V> {
    started: u64,
    /// The sweep generation in which this entry was last looked up.
    seen: u64,
    value: V,
}

/// Per-process facts that outlive one tick, keyed by PID and start time.
///
/// Use it once per tick: [`Instances::begin`], then [`Instances::entry`] for
/// every live process, then [`Instances::sweep`] to drop the processes that
/// were not seen. The sweep needs no set of live PIDs; each entry remembers the
/// generation it was last seen in.
pub struct Instances<V> {
    map: HashMap<i32, Slot<V>>,
    generation: u64,
}

impl<V> Default for Instances<V> {
    fn default() -> Self {
        Instances { map: HashMap::new(), generation: 0 }
    }
}

impl<V> Instances<V> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a tick.
    pub fn begin(&mut self) {
        self.generation += 1;
    }

    /// The entry for this process instance, and whether it was just created.
    ///
    /// An entry left by an earlier process with the same PID but a different
    /// start time is discarded and rebuilt with `make`, never returned.
    pub fn entry(&mut self, pid: i32, started: u64, make: impl FnOnce() -> V) -> (&mut V, bool) {
        let generation = self.generation;
        match self.map.entry(pid) {
            Entry::Occupied(o) => {
                let slot = o.into_mut();
                let replaced = slot.started != started;
                if replaced {
                    *slot = Slot { started, seen: generation, value: make() };
                }
                slot.seen = generation;
                (&mut slot.value, replaced)
            }
            Entry::Vacant(v) => {
                let slot = v.insert(Slot { started, seen: generation, value: make() });
                (&mut slot.value, true)
            }
        }
    }

    /// The entry for this process instance, if one exists, marking it seen.
    pub fn get_mut(&mut self, pid: i32, started: u64) -> Option<&mut V> {
        let generation = self.generation;
        let slot = self.map.get_mut(&pid).filter(|s| s.started == started)?;
        slot.seen = generation;
        Some(&mut slot.value)
    }

    /// Whether this process instance has an entry. Does not mark it seen.
    pub fn contains(&self, pid: i32, started: u64) -> bool {
        self.map.get(&pid).is_some_and(|s| s.started == started)
    }

    /// The entry for `pid` whatever its start time, without marking it seen.
    pub fn peek(&self, pid: i32) -> Option<&V> {
        self.map.get(&pid).map(|s| &s.value)
    }

    /// Drop every entry not seen since [`Instances::begin`]. Returns how many.
    pub fn sweep(&mut self) -> usize {
        let before = self.map.len();
        let generation = self.generation;
        self.map.retain(|_, s| s.seen == generation);
        before - self.map.len()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recycled_pid_never_inherits_the_previous_process_entry() {
        let mut c: Instances<&str> = Instances::new();
        c.begin();
        let (v, new) = c.entry(42, 1000, || "firefox");
        assert!(new);
        assert_eq!(*v, "firefox");

        c.begin();
        // Same pid, same start time: the same process, served from the cache.
        let (v, new) = c.entry(42, 1000, || unreachable!("cached"));
        assert!(!new);
        assert_eq!(*v, "firefox");
        assert_eq!(c.get_mut(42, 1000).copied(), Some("firefox"));

        // Same pid, later start time: a different process.
        assert!(c.get_mut(42, 2000).is_none());
        let (v, new) = c.entry(42, 2000, || "bash");
        assert!(new);
        assert_eq!(*v, "bash");
    }

    #[test]
    fn sweep_drops_what_was_not_seen_this_tick() {
        let mut c: Instances<u32> = Instances::new();
        c.begin();
        c.entry(1, 1, || 1);
        c.entry(2, 2, || 2);
        c.begin();
        c.entry(1, 1, || 0);
        assert_eq!(c.sweep(), 1);
        assert_eq!(c.len(), 1);
        assert!(c.peek(2).is_none());
        assert_eq!(c.peek(1), Some(&1));
    }

    #[test]
    fn cadence_is_due_first_then_on_its_interval_or_on_change() {
        let t0 = Instant::now();
        let mut c = Cadence::new(Duration::from_secs(10), Duration::from_secs(2));
        assert!(c.due(t0, false));
        c.mark(t0);
        assert!(!c.due(t0 + Duration::from_secs(1), true), "too soon even with a change");
        assert!(!c.due(t0 + Duration::from_secs(5), false));
        assert!(c.due(t0 + Duration::from_secs(5), true));
        assert!(c.due(t0 + Duration::from_secs(10), false));
        c.reset();
        assert!(c.due(t0, false));
    }
}
