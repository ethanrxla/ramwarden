use crate::{Error, Result};

/// One application's memory, as a unit the OS can reclaim from.
///
/// A cgroup scope on Linux. On Windows, a process tree rooted at the
/// application's top-level process — a browser and all of its renderers.
pub trait AppGroup: Send {
    /// Stable key, used to find the group again on a later tick.
    fn name(&self) -> &str;

    /// What to show the user: the process holding the most memory, never a
    /// launcher's directory name.
    fn label(&self) -> String;

    fn pids(&self) -> Result<Vec<i32>>;

    /// Total memory charged to the group, in bytes.
    fn current(&self) -> Result<u64>;

    /// What the OS is likely to hand back if asked to reclaim.
    fn reclaimable(&self) -> Result<u64>;

    /// Ask the OS to reclaim up to `bytes`. Returns what was *measurably*
    /// freed, which may be less, or zero.
    fn reclaim(&self, bytes: u64) -> Result<u64>;

    /// The current soft cap, or `None` when uncapped.
    fn soft_cap(&self) -> Result<Option<u64>>;

    /// Throttle the group above `bytes` without ever killing anything.
    fn set_soft_cap(&self, bytes: u64) -> Result<()>;

    fn clear_soft_cap(&self) -> Result<()>;
}

/// Every group the user's session owns — and nothing outside it.
///
/// Reachability is the safety property: a group the source does not return
/// cannot be reclaimed, so system services stay out of reach by construction.
pub trait GroupSource: Send + Sync {
    /// Whether group reclaim works at all here. An error carries the reason the
    /// ladder should log (no delegated memory controller, for instance).
    fn ready(&self) -> Result<()>;

    fn groups(&self) -> Result<Vec<Box<dyn AppGroup>>>;

    fn group(&self, name: &str) -> Result<Box<dyn AppGroup>> {
        self.groups()?
            .into_iter()
            .find(|g| g.name() == name)
            .ok_or_else(|| Error::missing(name))
    }
}
