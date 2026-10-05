//! An in-memory platform for policy tests.
//!
//! Drives the detector, the actuator and the ladder without touching a real
//! process, so the same policy tests run on Linux and Windows. Every part shares
//! one [`World`] behind a mutex: a test sets the scene, runs a tick, and then
//! asserts on what the fake recorded.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{
    AppGroup, Compression, Desktop, DesktopSource, Error, GroupSource, MemorySource, MemoryStatus, Outcome, Pager, Parts,
    Platform, Pressure, PressureSource, PressureWaker, ProcessControl, ProcessHandle,
    ProcessSample, ProcessSource, Result,
};

/// One fake reclaim group.
#[derive(Clone, Debug, Default)]
pub struct FakeGroup {
    pub name: String,
    pub pids: Vec<i32>,
    pub current: u64,
    pub reclaimable: u64,
    pub cap: Option<u64>,
}

/// The whole fake machine.
#[derive(Debug, Default)]
pub struct World {
    pub processes: Vec<ProcessSample>,
    pub groups: Vec<FakeGroup>,
    pub groups_ready: bool,
    pub pressure: Pressure,
    pub memory: MemoryStatus,
    pub desktop: Desktop,
    /// pids currently suspended by a handle.
    pub suspended: Vec<i32>,
    /// pids that have exited (terminated, killed, or removed by the test).
    pub exited: Vec<i32>,
    /// Every actuation, in order: `"suspend 42"`, `"reclaim brave 1000"`...
    pub log: Vec<String>,
}

impl World {
    fn alive(&self, pid: i32) -> bool {
        !self.exited.contains(&pid) && self.processes.iter().any(|p| p.pid == pid)
    }
}

pub type Shared = Arc<Mutex<World>>;

/// Build a [`Platform`] over a shared fake world.
pub fn platform(world: Shared) -> Platform {
    let w = world.clone();
    Platform::new(
        Parts {
            name: "fake",
            memory: Arc::new(Fake(world.clone())),
            pressure: Arc::new(Fake(world.clone())),
            control: Arc::new(Fake(world.clone())),
            pager: Arc::new(Fake(world.clone())),
            groups: Arc::new(Fake(world)),
        },
        {
            let w = w.clone();
            move || Box::new(Fake(w.clone()))
        },
        move || Box::new(Fake(w.clone())),
    )
}

/// One newtype implements every trait; they all read the same world.
#[derive(Clone)]
pub struct Fake(pub Shared);

impl Fake {
    fn world(&self) -> std::sync::MutexGuard<'_, World> {
        self.0.lock().unwrap()
    }
}

impl ProcessSource for Fake {
    fn sample(&mut self) -> Result<Vec<ProcessSample>> {
        let w = self.world();
        Ok(w
            .processes
            .iter()
            .filter(|p| !w.exited.contains(&p.pid))
            .cloned()
            .map(|mut p| {
                if w.suspended.contains(&p.pid) {
                    p.state = 'T';
                }
                p
            })
            .collect())
    }
}

impl DesktopSource for Fake {
    fn sample(&mut self) -> Desktop {
        self.world().desktop.clone()
    }
}

impl ProcessControl for Fake {
    fn open(&self, pid: i32) -> Result<Box<dyn ProcessHandle>> {
        if !self.world().alive(pid) {
            return Err(Error::missing(format!("pid {pid}")));
        }
        Ok(Box::new(FakeHandle {
            pid,
            world: self.0.clone(),
        }))
    }

    fn proportional(&self, pid: i32) -> Option<u64> {
        let w = self.world();
        w.processes
            .iter()
            .find(|p| p.pid == pid && !w.exited.contains(&pid))
            .map(|p| p.proportional)
    }
}

struct FakeHandle {
    pid: i32,
    world: Shared,
}

impl FakeHandle {
    fn act(&self, verb: &str, f: impl FnOnce(&mut World)) -> Result<()> {
        let mut w = self.world.lock().unwrap();
        if !w.alive(self.pid) {
            return Err(Error::missing(format!("pid {}", self.pid)));
        }
        w.log.push(format!("{verb} {}", self.pid));
        f(&mut w);
        Ok(())
    }
}

impl ProcessHandle for FakeHandle {
    fn pid(&self) -> i32 {
        self.pid
    }

    fn is_alive(&self) -> bool {
        self.world.lock().unwrap().alive(self.pid)
    }

    fn suspend(&self) -> Result<()> {
        let pid = self.pid;
        self.act("suspend", |w| w.suspended.push(pid))
    }

    fn resume(&self) -> Result<()> {
        let pid = self.pid;
        self.act("resume", |w| w.suspended.retain(|p| *p != pid))
    }

    fn terminate(&self) -> Result<()> {
        let pid = self.pid;
        self.act("terminate", |w| w.exited.push(pid))
    }

    fn kill(&self) -> Result<()> {
        let pid = self.pid;
        self.act("kill", |w| w.exited.push(pid))
    }

    fn wait_exit(&self, _: Duration) -> Result<bool> {
        Ok(!self.is_alive())
    }
}

impl PressureSource for Fake {
    fn read(&self) -> Result<Pressure> {
        Ok(self.world().pressure)
    }

    fn waker(&self) -> Result<Box<dyn PressureWaker>> {
        Err(Error::Unsupported("pressure wakeups in the fake platform"))
    }
}

impl MemorySource for Fake {
    fn status(&self) -> Result<MemoryStatus> {
        Ok(self.world().memory)
    }

    fn compression(&self) -> Result<Compression> {
        Ok(Compression::default())
    }
}

impl Pager for Fake {
    fn available(&self) -> bool {
        true
    }

    fn route(&self) -> &'static str {
        "fake"
    }

    fn page_out(&self, pid: i32, _: f64, _: u64) -> Outcome {
        let mut w = self.world();
        w.log.push(format!("pageout {pid}"));
        Outcome::freed(vec![pid], 1)
    }
}

impl GroupSource for Fake {
    fn ready(&self) -> Result<()> {
        if self.world().groups_ready {
            Ok(())
        } else {
            Err(Error::Unsupported("group reclaim in this fake world"))
        }
    }

    fn groups(&self) -> Result<Vec<Box<dyn AppGroup>>> {
        let w = self.world();
        Ok(w.groups
            .iter()
            .map(|g| {
                Box::new(FakeGroupRef {
                    name: g.name.clone(),
                    world: self.0.clone(),
                }) as Box<dyn AppGroup>
            })
            .collect())
    }
}

struct FakeGroupRef {
    name: String,
    world: Shared,
}

impl FakeGroupRef {
    fn with<T>(&self, f: impl FnOnce(&mut FakeGroup, &mut Vec<String>) -> T) -> Result<T> {
        let mut w = self.world.lock().unwrap();
        let World { groups, log, .. } = &mut *w;
        let g = groups
            .iter_mut()
            .find(|g| g.name == self.name)
            .ok_or_else(|| Error::missing(&self.name))?;
        Ok(f(g, log))
    }
}

impl AppGroup for FakeGroupRef {
    fn name(&self) -> &str {
        &self.name
    }

    fn label(&self) -> String {
        self.name.clone()
    }

    fn pids(&self) -> Result<Vec<i32>> {
        self.with(|g, _| g.pids.clone())
    }

    fn current(&self) -> Result<u64> {
        self.with(|g, _| g.current)
    }

    fn reclaimable(&self) -> Result<u64> {
        self.with(|g, _| g.reclaimable)
    }

    fn reclaim(&self, bytes: u64) -> Result<u64> {
        self.with(|g, log| {
            let freed = bytes.min(g.reclaimable);
            g.reclaimable -= freed;
            g.current -= freed.min(g.current);
            log.push(format!("reclaim {} {bytes}", g.name));
            freed
        })
    }

    fn soft_cap(&self) -> Result<Option<u64>> {
        self.with(|g, _| g.cap)
    }

    fn set_soft_cap(&self, bytes: u64) -> Result<()> {
        self.with(|g, log| {
            g.cap = Some(bytes);
            log.push(format!("cap {} {bytes}", g.name));
        })
    }

    fn clear_soft_cap(&self) -> Result<()> {
        self.with(|g, log| {
            g.cap = None;
            log.push(format!("uncap {}", g.name));
        })
    }
}

/// Convenience: a sample with the fields policy tests usually care about.
pub fn process(pid: i32, name: &str, proportional: u64) -> ProcessSample {
    ProcessSample {
        pid,
        ppid: 1,
        name: name.into(),
        owner: crate::Owner::CurrentUser,
        resident: proportional,
        proportional,
        state: 'S',
        age_seconds: 3600.0,
        ..Default::default()
    }
}

/// Group lookup by name, for assertions.
pub fn group<'a>(w: &'a World, name: &str) -> Option<&'a FakeGroup> {
    w.groups.iter().find(|g| g.name == name)
}

/// Count of log entries starting with `prefix`.
pub fn count(w: &World, prefix: &str) -> usize {
    w.log.iter().filter(|l| l.starts_with(prefix)).count()
}
