use std::sync::Arc;

use crate::{
    Background, DesktopSource, GroupSource, MemorySource, Pager, PressureSource, ProcessControl, ProcessSource,
};

type Factory<T> = Box<dyn Fn() -> Box<T> + Send + Sync>;

/// Everything the daemon needs from one operating system, in one place.
///
/// Built once at startup by the mechanism crate (`ramwarden_kernel::platform`
/// or `ramwarden_nt::platform`) and shared read-only; each part carries its own
/// interior state where it needs any.
pub struct Platform {
    /// `"linux"` or `"windows"`, for `/ai` and the status line.
    pub name: &'static str,
    pub memory: Arc<dyn MemorySource>,
    pub pressure: Arc<dyn PressureSource>,
    pub control: Arc<dyn ProcessControl>,
    pub pager: Arc<dyn Pager>,
    pub background: Arc<dyn Background>,
    pub groups: Arc<dyn GroupSource>,
    processes: Factory<dyn ProcessSource>,
    desktop: Factory<dyn DesktopSource>,
}

/// The shared parts of a [`Platform`], so construction reads as named fields
/// rather than a long positional argument list.
pub struct Parts {
    pub name: &'static str,
    pub memory: Arc<dyn MemorySource>,
    pub pressure: Arc<dyn PressureSource>,
    pub control: Arc<dyn ProcessControl>,
    pub pager: Arc<dyn Pager>,
    pub background: Arc<dyn Background>,
    pub groups: Arc<dyn GroupSource>,
}

impl Platform {
    /// `processes` and `desktop` are factories rather than instances: each
    /// consumer (the monitor's detector, a startup probe) owns its own cache.
    pub fn new(
        parts: Parts,
        processes: impl Fn() -> Box<dyn ProcessSource> + Send + Sync + 'static,
        desktop: impl Fn() -> Box<dyn DesktopSource> + Send + Sync + 'static,
    ) -> Self {
        Platform {
            name: parts.name,
            memory: parts.memory,
            pressure: parts.pressure,
            control: parts.control,
            pager: parts.pager,
            background: parts.background,
            groups: parts.groups,
            processes: Box::new(processes),
            desktop: Box::new(desktop),
        }
    }

    /// A fresh process source, for a new detector.
    pub fn processes(&self) -> Box<dyn ProcessSource> {
        (self.processes)()
    }

    /// A fresh desktop probe with its own caches.
    pub fn desktop(&self) -> Box<dyn DesktopSource> {
        (self.desktop)()
    }

    /// The group source when group reclaim works here, `None` otherwise.
    pub fn ready_groups(&self) -> Option<&dyn GroupSource> {
        self.groups.ready().ok().map(|()| self.groups.as_ref())
    }
}
