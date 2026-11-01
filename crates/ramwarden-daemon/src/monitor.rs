//! The loop that drives the ladder.
//!
//! # Kernel wakeups where possible, adaptive polling where not
//!
//! The intended design is an OS wakeup: on Linux a PSI trigger ("wake me if
//! tasks stall more than N ms per second"), on Windows the low-memory resource
//! notification. Either is attempted first through the platform's
//! [`PressureWaker`](ramwarden_platform::PressureWaker), and where granted the
//! loop reacts at once while costing nothing when idle.
//!
//! It frequently is not granted. Linux 7.1.5 on the machine this was written
//! for opens `/proc/pressure/memory` `O_RDWR` and reads it happily, but rejects
//! every trigger write with `EINVAL` — trigger creation is privileged there.
//!
//! So the fallback is the normal path, and it is **adaptive** rather than the
//! fixed ten-second sleep v1 used. The interval follows the rung: a quiet
//! machine is sampled rarely, and one under pressure is sampled every second,
//! which recovers most of what the trigger would have bought.
//!
//! Either way the loop also ticks on a timeout, and that matters as much as the
//! wakeup: pressure *passing* is not an event anything reports, so the slow tick
//! is what resumes suspended applications and lifts soft caps.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ramwarden_core::actuator::Outcome;
use ramwarden_core::history::{SampleContext, SignalSample};
use ramwarden_core::signals::Signals;
use ramwarden_ai::jev::Jev;
use ramwarden_ai::secret::Secret;
use ramwarden_core::judge::{Candidate, Intent, Judge, Judgement};
use ramwarden_core::ladder::{self, TabCloser, World};

use crate::hub::AppState;

/// Sampling interval on a quiet machine. Also the de-escalation interval, since
/// nothing wakes us when pressure passes.
const IDLE_INTERVAL: Duration = Duration::from_secs(10);

/// Sampling interval once pressure is in the hysteresis band — something is
/// happening, so look more often in case it keeps rising.
const WATCHFUL_INTERVAL: Duration = Duration::from_secs(2);

/// Sampling interval while actively remediating.
const BUSY_INTERVAL: Duration = Duration::from_secs(1);

/// How often to sample next, given the rung just handled.
///
/// This is what replaces the PSI trigger on a kernel that will not grant one:
/// react within a second while it matters, and stay out of the way when it does
/// not. A fixed interval has to choose one or the other.
fn next_interval(rung: Option<ladder::Rung>) -> Duration {
    match rung {
        None | Some(ladder::Rung::Release) => IDLE_INTERVAL,
        Some(ladder::Rung::Hold) => WATCHFUL_INTERVAL,
        Some(_) => BUSY_INTERVAL,
    }
}

/// Bridges the ladder's tab rung to the browsers.
///
/// The ladder is synchronous and runs while the monitor holds the ladder mutex
/// and the detector read lock. Tab unloading is async and can wait seconds for
/// a browser, so it is **spawned**, never awaited from here: blocking this thread
/// on the runtime while holding those locks deadlocks as soon as an HTTP handler
/// on a worker thread wants one of them. That worker then cannot drive the timers
/// the unload is waiting on, the unload never finishes, and the locks are never
/// released — the daemon stays alive, keeps its port open, and answers nothing.
pub struct Tabs {
    state: AppState,
    handle: tokio::runtime::Handle,
}

impl Tabs {
    pub fn new(state: AppState, handle: tokio::runtime::Handle) -> Self {
        Tabs { state, handle }
    }
}

/// One unload at a time: the ladder ticks every second while it sits on the tabs
/// rung, and each tick would otherwise queue another wait for the same browser.
static UNLOAD_RUNNING: AtomicBool = AtomicBool::new(false);

/// Clears [`UNLOAD_RUNNING`] when the task ends, including by panic.
struct UnloadGuard;

impl Drop for UnloadGuard {
    fn drop(&mut self) {
        UNLOAD_RUNNING.store(false, Ordering::Release);
    }
}

impl TabCloser for Tabs {
    fn close_stale(&mut self, _goal: &str) -> Outcome {
        if UNLOAD_RUNNING.swap(true, Ordering::AcqRel) {
            return Outcome {
                notes: vec!["tab unload already in progress".into()],
                ..Outcome::default()
            };
        }
        let state = self.state.clone();
        self.handle.spawn(async move {
            let _guard = UnloadGuard;
            state.request_all_tabs().await;
            let targets = {
                let hub = state.hub.lock().unwrap();
                crate::browser::snapshot(&hub.reg, state.cfg.thresholds.inactivity_minutes as i64)
                    .into_iter().filter(|r|r.eligible).take(crate::browser::BATCH_LIMIT)
                    .map(|r|r.target).collect::<Vec<_>>()
            };
            let result = state.discard_tabs(&targets).await;
            tracing::info!("{} tabs unloaded, {} queued, {} refused; tabs remain open",
                result.confirmed.len(), result.queued.len(), result.refused.len());
        });
        Outcome {
            notes: vec!["tab unload started in the background".into()],
            ..Outcome::default()
        }
    }
}

/// Bridges the ladder's judge to Jev, the same way [`Tabs`] bridges to the
/// browsers: the ladder is synchronous, so block on the request here.
struct JevJudge {
    jev: Jev,
    suspend_threshold: f64,
    kill_threshold: f64,
    handle: tokio::runtime::Handle,
}

impl Judge for JevJudge {
    fn judge(&mut self, intent: Intent, candidates: &[Candidate]) -> Result<Vec<Judgement>, String> {
        let started = Instant::now();
        let threshold = match intent {
            Intent::Suspend => self.suspend_threshold,
            Intent::Kill => self.kill_threshold,
        };
        let out = self
            .handle
            .block_on(crate::ladderjudge::judge(&self.jev, intent, candidates, threshold))
            .map_err(|e| e.to_string())?;
        for j in &out {
            tracing::info!(
                "jev: {} {}: {} ({:?})",
                if j.close { "approve" } else { "keep" },
                j.name,
                j.reason,
                started.elapsed()
            );
        }
        Ok(out)
    }
}

/// Jev, if it is enabled and has a key.
///
/// Its own client rather than the tab tier's: the ladder cannot wait 15
/// seconds mid-pressure, so it gets `ladder_timeout_ms`.
fn jev_judge(cfg: &ramwarden_core::config::Config, handle: &tokio::runtime::Handle) -> Option<JevJudge> {
    let j = &cfg.jev;
    if !j.enabled {
        return None;
    }
    let jev = Jev::new(
        &j.base_url,
        Secret::new(j.api_key.trim()),
        &j.model,
        Duration::from_millis(j.ladder_timeout_ms),
    )
    .inspect(|c| {
        tracing::info!(
            "Jev judging suspends (>= {:.0}%) and kills (>= {:.0}%) with {} key {}",
            j.suspend_threshold * 100.0,
            j.kill_threshold * 100.0,
            j.model,
            c.key_fingerprint()
        )
    })
    .inspect_err(|e| tracing::warn!("Jev unavailable to the ladder: {e} — the gate alone decides"))
    .ok()?;
    Some(JevJudge {
        jev,
        suspend_threshold: j.suspend_threshold,
        kill_threshold: j.kill_threshold,
        handle: handle.clone(),
    })
}

/// Seconds since the Unix epoch when the monitor last finished a tick.
static LAST_TICK: AtomicU64 = AtomicU64::new(0);

/// Where the monitor is in its tick, and since when, so a stall names its phase.
/// Until 2026-10-20 the watchdog could only say how long the monitor had been
/// stuck, and every stall left nothing to go on but the tick before it.
static PHASE: AtomicU8 = AtomicU8::new(Phase::Waiting as u8);
static PHASE_SINCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Phase {
    Waiting,
    Desktop,
    Sample,
    Pressure,
    Ladder,
    Gpu,
    Behaviour,
    Upkeep,
}

impl Phase {
    const ALL: [Phase; 8] = [
        Phase::Waiting,
        Phase::Desktop,
        Phase::Sample,
        Phase::Pressure,
        Phase::Ladder,
        Phase::Gpu,
        Phase::Behaviour,
        Phase::Upkeep,
    ];

    fn enter(self) {
        PHASE.store(self as u8, Ordering::Relaxed);
        PHASE_SINCE.store(epoch_secs(), Ordering::Relaxed);
    }

    fn current() -> (Phase, u64) {
        let phase = Self::ALL[PHASE.load(Ordering::Relaxed) as usize];
        (phase, PHASE_SINCE.load(Ordering::Relaxed))
    }

    fn as_str(self) -> &'static str {
        match self {
            Phase::Waiting => "waiting for the next tick",
            Phase::Desktop => "desktop probe (windows, focus, audio)",
            Phase::Sample => "process sample",
            Phase::Pressure => "pressure read",
            Phase::Ladder => "ladder step",
            Phase::Gpu => "VRAM policy",
            Phase::Behaviour => "behaviour log",
            Phase::Upkeep => "training and pruning",
        }
    }
}

/// How long the monitor may go without finishing a tick before the daemon is
/// restarted. A tick is bounded by its slowest step (tab reports and closes
/// time out in seconds, the service pipe in five), and the idle interval is ten
/// seconds, so three minutes only passes when something is stuck for good.
const STALL_LIMIT: Duration = Duration::from_secs(180);

/// Exit code for a stalled monitor.
const STALL_EXIT: i32 = 70;

/// How often the watchdog looks, and how much longer than that one look may take
/// before the machine is taken to have been asleep.
const WATCH_EVERY: Duration = Duration::from_secs(30);
const SLEEP_GAP: Duration = Duration::from_secs(90);

/// Start a fresh daemon through the same launcher, with the same arguments. It
/// waits for this one to release the port (see `bind_when_free` in main).
///
/// The logon task's restart-on-failure never fired: the daemon runs under
/// `conhost --headless`, which reports success whatever the daemon's exit code
/// (2026-10-12: task result 0x0 after an exit 70, and no daemon for the night).
///
/// The launcher is spawned directly. The first version built a `cmd /c "timeout
/// … & "launcher" args"` line and passed it through `Command::args`, which
/// escapes inner quotes as `\"`. cmd.exe reads those literally, so it looked for
/// a program named `\"C:\Program Files\…\ramwarden-daemon.cmd\"` and no
/// replacement ever ran (2026-10-13, -19, -20). `timeout` also exits at once
/// without a console, so its delay never happened either. Rust escapes a `.cmd`
/// it spawns itself correctly, and a hidden console (not none) keeps the
/// launcher's own commands working.
#[cfg(windows)]
fn relaunch() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(exe) = std::env::current_exe() else { return };
    let launcher = exe.with_file_name("ramwarden-daemon.cmd");
    let target = if launcher.exists() { launcher } else { exe };
    match replacement(&target, std::env::args_os().skip(1))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        Ok(child) => tracing::info!("replacement daemon started (pid {}) from {}", child.id(), target.display()),
        Err(e) => tracing::error!("could not start a replacement daemon from {}: {e}", target.display()),
    }
}

#[cfg(not(windows))]
fn relaunch() {}

/// The command that starts a replacement: `target` with `args`, and no stdio
/// to inherit. The daemon writes to `--log-file`.
#[cfg_attr(not(windows), allow(dead_code))]
fn replacement(
    target: &std::path::Path,
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(target);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Start the monitor on its own thread, and a watchdog that restarts the daemon
/// if the monitor stops ticking.
///
/// A stuck monitor is worse than a dead one: it can hold the ladder lock or wait
/// on the detector's write lock, and the HTTP handlers then queue behind it until
/// every runtime worker is blocked, so the window shows "Waiting for the daemon"
/// with no error anywhere (2026-10-11: hung for a day and a half after a pageout
/// rung). The daemon then starts its own replacement and exits.
pub fn spawn(state: AppState, handle: tokio::runtime::Handle) -> std::thread::JoinHandle<()> {
    LAST_TICK.store(epoch_secs(), Ordering::Relaxed);
    std::thread::Builder::new()
        .name("ramwarden-watchdog".into())
        .spawn(|| {
            let mut looked = epoch_secs();
            loop {
                std::thread::sleep(WATCH_EVERY);
                let now = epoch_secs();
                // Sleep and hibernate stop every thread but not the wall clock: after
                // a 9 minute sleep the monitor looked stalled and the daemon was shut
                // down on wake (2026-10-12). A look that took far longer than asked
                // means the machine was suspended, so the monitor gets a fresh start.
                if now.saturating_sub(looked) > SLEEP_GAP.as_secs() {
                    tracing::info!("machine was suspended for about {}s; watchdog reset", now - looked);
                    LAST_TICK.store(now, Ordering::Relaxed);
                }
                looked = now;
                let quiet = now.saturating_sub(LAST_TICK.load(Ordering::Relaxed));
                if quiet > STALL_LIMIT.as_secs() {
                    let (phase, since) = Phase::current();
                    tracing::error!(
                        "monitor has not finished a tick in {quiet}s (in {} for {}s); restarting the daemon",
                        phase.as_str(),
                        now.saturating_sub(since)
                    );
                    relaunch();
                    std::process::exit(STALL_EXIT);
                }
            }
        })
        .expect("spawn watchdog thread");
    std::thread::Builder::new()
        .name("ramwarden-monitor".into())
        .spawn(move || run(state, handle))
        .expect("spawn monitor thread")
}

/// Ask the model tier about borderline tabs, in the background. The monitor
/// never waits on it: answers land in a cache that ranking reads.
fn refresh_intel(state: &AppState, handle: &tokio::runtime::Handle, last: &mut Instant) {
    static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
    let Some(intel) = crate::tabintel::global().filter(|i| i.active()) else { return };
    if last.elapsed() < Duration::from_secs(30) || IN_FLIGHT.swap(true, Ordering::AcqRel) {
        return;
    }
    *last = Instant::now();
    let rows = {
        let hub = state.hub.lock().unwrap();
        crate::browser::snapshot(&hub.reg, state.cfg.thresholds.inactivity_minutes as i64)
    };
    let states = crate::tabintel::borderline_states(&rows, intel.config());
    let intel = intel.clone();
    handle.spawn(async move {
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                IN_FLIGHT.store(false, Ordering::Release);
            }
        }
        let _done = Done;
        let n = intel.refresh(&states, Instant::now()).await;
        if n > 0 {
            tracing::info!("jev answered {n} tab description(s)");
        }
    });
}

/// Mark unloaded tabs the user has since returned to.
fn reconcile_returns(state: &AppState) {
    let now = ramwarden_core::history::unix_now() as i64;
    let reports: Vec<(String, Vec<crate::tabs::Tab>)> = {
        let hub = state.hub.lock().unwrap();
        hub.reg
            .socket_ids()
            .into_iter()
            .chain(hub.reg.poll_ids())
            .map(|b| {
                let tabs = hub.reg.tabs_for(&b).to_vec();
                (b, tabs)
            })
            .collect()
    };
    if let Ok(h) = state.history.lock() {
        for (browser, tabs) in &reports {
            crate::outcomes::reconcile(&h, browser, tabs, now);
        }
    }
}

fn run(state: AppState, handle: tokio::runtime::Handle) {
    let mut probe = state.platform.desktop();

    // The wakeup is a nicety, not a requirement: without it the loop falls
    // back to the heartbeat alone, which is still better than v1's.
    let mut trigger = match state.platform.pressure.waker() {
        Ok(t) => {
            tracing::info!("pressure wakeup armed — {}", t.describe());
            Some(t)
        }
        Err(e) => {
            tracing::warn!(
                "pressure wakeup unavailable ({e}) — using adaptive polling \
                 ({}s idle / {}s watchful / {}s busy)",
                IDLE_INTERVAL.as_secs(),
                WATCHFUL_INTERVAL.as_secs(),
                BUSY_INTERVAL.as_secs()
            );
            None
        }
    };

    let cfg = Arc::clone(&state.cfg);
    let mut judge = jev_judge(&cfg, &handle);
    let mut gpu_policy = crate::gpu::GpuPolicy::default();
    // Train soon after start when no model exists yet, then once a day.
    let mut last_train = if state.predictor.model_path().exists() {
        Some(Instant::now())
    } else {
        None
    };
    let mut interval = IDLE_INTERVAL;
    let mut last_sample = Instant::now() - Duration::from_secs(cfg.logging.interval_seconds);
    let mut last_prune = Instant::now();
    let mut last_intel = Instant::now() - Duration::from_secs(60);
    let mut quitter = crate::appquit::Quitter::new();
    let mut held = false;

    loop {
        LAST_TICK.store(epoch_secs(), Ordering::Relaxed);
        Phase::Waiting.enter();
        let woken_by_pressure = match trigger.as_mut() {
            Some(t) => t.wait(interval).unwrap_or(false),
            None => {
                std::thread::sleep(interval);
                false
            }
        };

        let started = Instant::now();

        // A hold (a window arranger at work) means: nothing frozen, nothing new
        // frozen. Thaw once at the start; skip the ladder until it expires.
        if state.on_hold() {
            if !held {
                held = true;
                for (name, _) in state.ladder.lock().unwrap().thaw_all() {
                    tracing::info!("thawed {name}: on hold");
                }
                crate::freeze_journal::write(&cfg.db_path.with_file_name("suspended.json"), &[]);
            }
            interval = WATCHFUL_INTERVAL;
            continue;
        }
        held = false;

        // 1. Sample. The desktop probe caches its subprocesses internally.
        Phase::Desktop.enter();
        let desktop = probe.sample();
        Phase::Sample.enter();
        if let Err(e) = state.det.write().unwrap().sample(&desktop) {
            tracing::warn!("sampling failed: {e}");
            continue;
        }

        // 1b. Has the user come back to a tab we unloaded? Cheap: one query per
        // browser, then a hash lookup per reported tab.
        reconcile_returns(&state);
        refresh_intel(&state, &handle, &mut last_intel);
        crate::appquit::consider(&state, &handle, &mut quitter);

        // 2. Look.
        Phase::Pressure.enter();
        let pressure = match state.platform.pressure.read() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("pressure read failed: {e}");
                continue;
            }
        };
        let available = ladder::available_bytes(state.platform.memory.as_ref());

        // 3. Act.
        Phase::Ladder.enter();
        let mut closer = Tabs::new(state.clone(), handle.clone());
        let det = state.det.read().unwrap();
        let mut world = World {
            det: &det,
            groups: state.platform.ready_groups(),
            psi: pressure,
            available_bytes: available,
            watchlist: &cfg.watchlist,
            goal: String::new(),
            tabs: Some(&mut closer),
            judge: judge.as_mut().map(|j| j as &mut dyn Judge),
        };

        let report = {
            let mut ladder = state.ladder.lock().unwrap();
            ladder.step(&mut world)
        };
        drop(det);

        // Keep the on-disk record of frozen apps current, so a crash cannot strand them.
        if report.steps.iter().any(|(a, _, _)| matches!(*a, "suspend" | "resume")) {
            let frozen: Vec<crate::freeze_journal::Frozen> = state
                .ladder
                .lock()
                .unwrap()
                .actuator()
                .suspended()
                .iter()
                .flat_map(|e| e.live_pids().into_iter().map(|pid| crate::freeze_journal::Frozen { name: e.name.clone(), pid }))
                .collect();
            crate::freeze_journal::write(&cfg.db_path.with_file_name("suspended.json"), &frozen);
        }

        if report.did_something() || woken_by_pressure {
            tracing::info!(
                "rung {} (some={:.1}% full={:.1}% avail={} MB) — freed {} MB in {:?}",
                report.rung.map(|r| r.as_str()).unwrap_or("?"),
                pressure.some_avg10,
                pressure.full_avg10,
                available / 1_000_000,
                report.total_freed / 1_000_000,
                started.elapsed(),
            );
            for (action, target, outcome) in &report.steps {
                if outcome.did_nothing() {
                    tracing::debug!("  {action} {target}: {}", outcome.notes.join("; "));
                } else {
                    tracing::info!(
                        "  {action} {target}: freed {} MB{}",
                        outcome.bytes_freed / 1_000_000,
                        if outcome.notes.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", outcome.notes.join("; "))
                        }
                    );
                }
            }
        }

        // ── VRAM: unload idle local models when a game needs the card ──────
        if cfg.gpu.unload_models_for_games {
            Phase::Gpu.enter();
            let game = crate::gpu::game_running(&state.det.read().unwrap());
            let notes = handle.block_on(gpu_policy.tick(game, cfg.gpu.vram_percent, &state.models));
            for n in notes {
                tracing::info!("gpu: {n}");
                if let Ok(h) = state.history.lock() {
                    let _ = h.log(&ramwarden_core::history::NewAction {
                        rung: -1,
                        trigger: "vram".into(),
                        action: "unload_model".into(),
                        target: n.clone(),
                        succeeded: n.starts_with("unloaded"),
                        notes: n,
                        ..Default::default()
                    });
                }
            }
        }

        // ── behaviour log ───────────────────────────────────────────────────
        // Sampled on its own slower clock, independent of the ladder's cadence:
        // the point is an even picture of ordinary behaviour, not a burst of rows
        // every time pressure rises.
        if cfg.logging.enabled
            && last_sample.elapsed() >= Duration::from_secs(cfg.logging.interval_seconds)
        {
            Phase::Behaviour.enter();
            last_sample = Instant::now();
            let floor = cfg.logging.floor_mb * 1024 * 1024;
            let samples: Vec<SignalSample> = {
                let det = state.det.read().unwrap();
                det.snapshot()
                    .values()
                    .filter(|s| s.pss >= floor)
                    .map(to_sample)
                    .collect()
            };
            let ctx = SampleContext {
                psi_some: pressure.some_avg10,
                psi_full: pressure.full_avg10,
                available: available as i64,
            };
            let epoch = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            state.predictor.observe(&samples, epoch, pressure.some_avg10);
            if let Ok(h) = state.history.lock()
                && let Err(e) = h.log_signals(&samples, ctx)
            {
                tracing::debug!("behaviour log write failed: {e}");
            }
        }

        // Retrain the reuse predictor daily, off this thread: it reads the
        // whole behaviour log and takes seconds to minutes.
        Phase::Upkeep.enter();
        if cfg.logging.enabled && last_train.is_none_or(|t| t.elapsed() >= Duration::from_secs(86_400)) {
            last_train = Some(Instant::now());
            crate::predict::train_in_background(&state);
        }

        // Pruning once a day is enough; it only deletes whole days.
        if cfg.logging.enabled && last_prune.elapsed() >= Duration::from_secs(86_400) {
            last_prune = Instant::now();
            if let Ok(h) = state.history.lock()
                && let Ok(n) = h.prune_signals(cfg.logging.keep_days)
                && n > 0
            {
                tracing::info!("pruned {n} behaviour sample(s) older than {} days", cfg.logging.keep_days);
            }
        }

        interval = next_interval(report.rung);

        if let Some(p) = &report.kill_pending {
            tracing::warn!(
                "KILL ARMED: {} in {}s — POST /ladder/cancel-kill to stop it",
                p.targets.join(", "),
                p.remaining().as_secs()
            );
        }
    }
}

/// Flatten one process's signals into a log row.
///
/// Numbers and short categories only — no URLs, no window titles, no document
/// names. The log records behaviour, which is what a predictor needs, and keeping
/// it to that means it never becomes a second copy of the user's activity.
fn to_sample(s: &Signals) -> SignalSample {
    SignalSample {
        pid: s.pid,
        name: s.name.clone(),
        role: s.role.as_str().to_string(),
        verdict: s.verdict.as_str().to_string(),
        protection: match s.protection {
            ramwarden_core::signals::Protection::None => String::new(),
            ramwarden_core::signals::Protection::Structural => "structural".into(),
            ramwarden_core::signals::Protection::Serving => "serving".into(),
        },
        pss: s.pss as i64,
        rss: s.rss as i64,
        cpu_recent: s.cpu_seconds_recent,
        age_minutes: s.age_minutes,
        established: s.established as i64,
        // A count, deliberately not the port numbers.
        listening: s.listening_ports.len() as i64,
        focused: s.is_focused,
        windowed: s.has_window,
        audio: s.playing_audio,
        tty: s.has_tty,
        descendant: s.active_descendant.is_some(),
        scope: s.scope.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramwarden_core::ladder::Rung;

    /// The first relaunch never ran: its hand-built `cmd /c` line reached cmd.exe
    /// with `\"`-escaped quotes. The launcher lives in a path with a space and is
    /// passed a path with a space, as `--log-file` is.
    #[cfg(windows)]
    #[test]
    fn a_replacement_launcher_runs_with_its_arguments_intact() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("Program Files");
        std::fs::create_dir(&bin).unwrap();
        let out = dir.path().join("args.txt");
        let launcher = bin.join("ramwarden-daemon.cmd");
        std::fs::write(&launcher, format!("@echo %* > \"{}\"\r\n", out.display())).unwrap();

        let log = dir.path().join("log dir").join("daemon.log");
        let status = replacement(&launcher, ["--log-file".into(), log.clone().into_os_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let args = std::fs::read_to_string(&out).unwrap();
        assert!(args.contains("--log-file"), "{args}");
        assert!(args.contains(&log.display().to_string()), "{args}");
    }

    #[test]
    fn every_phase_round_trips_through_the_breadcrumb() {
        for p in Phase::ALL {
            p.enter();
            assert_eq!(Phase::current().0, p);
        }
        Phase::Waiting.enter();
    }

    /// A quiet machine must not be sampled every second, and one under pressure
    /// must not be sampled every ten.
    #[test]
    fn the_interval_follows_the_rung() {
        assert_eq!(next_interval(None), IDLE_INTERVAL);
        assert_eq!(next_interval(Some(Rung::Release)), IDLE_INTERVAL);
        assert_eq!(next_interval(Some(Rung::Hold)), WATCHFUL_INTERVAL);
        for r in [Rung::Reclaim, Rung::PageOut, Rung::Tabs, Rung::Suspend, Rung::Kill] {
            assert_eq!(next_interval(Some(r)), BUSY_INTERVAL, "{r:?}");
        }
    }

    #[test]
    fn the_intervals_are_ordered_so_escalation_samples_faster() {
        assert!(BUSY_INTERVAL < WATCHFUL_INTERVAL);
        assert!(WATCHFUL_INTERVAL < IDLE_INTERVAL);
    }
}
