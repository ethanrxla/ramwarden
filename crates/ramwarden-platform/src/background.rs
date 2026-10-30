use crate::{Error, Result};

/// Marks a process as background work, or restores it.
///
/// A hint, not an action: nothing is paged out and nothing is stopped. On
/// Windows it lowers the process's memory priority — so its standby pages are
/// repurposed first — and opts it into EcoQoS scheduling. Fully reversible,
/// which is why the Tidy rung may apply it at any pressure.
pub trait Background: Send + Sync {
    fn available(&self) -> bool;

    fn set(&self, pid: i32, background: bool) -> Result<()>;
}

/// A platform with no background hint.
pub struct NoBackground;

impl Background for NoBackground {
    fn available(&self) -> bool {
        false
    }

    fn set(&self, _: i32, _: bool) -> Result<()> {
        Err(Error::Unsupported("background hints"))
    }
}
