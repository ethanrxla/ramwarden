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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ramwarden_core::actuator::Outcome;
use ramwarden_core::history::{SampleContext, SignalSample};
use ramwarden_core::signals::Signals;
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
struct Tabs {
    state: AppState,
    handle: tokio::runtime::Handle,
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

/// Start the monitor on its own thread.
pub fn spawn(state: AppState, handle: tokio::runtime::Handle) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("ramwarden-monitor".into())
        .spawn(move || run(state, handle))
        .expect("spawn monitor thread")
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

    loop {
        let woken_by_pressure = match trigger.as_mut() {
            Some(t) => t.wait(interval).unwrap_or(false),
            None => {
                std::thread::sleep(interval);
                false
            }
        };

        let started = Instant::now();

        // 1. Sample. The desktop probe caches its subprocesses internally.
        let desktop = probe.sample();
        if let Err(e) = state.det.write().unwrap().sample(&desktop) {
            tracing::warn!("sampling failed: {e}");
            continue;
        }

        // 1b. Has the user come back to a tab we unloaded? Cheap: one query per
        // browser, then a hash lookup per reported tab.
        reconcile_returns(&state);

        // 2. Look.
        let pressure = match state.platform.pressure.read() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("pressure read failed: {e}");
                continue;
            }
        };
        let available = ladder::available_bytes(state.platform.memory.as_ref());

        // 3. Act.
        let mut closer = Tabs {
            state: state.clone(),
            handle: handle.clone(),
        };
        let det = state.det.read().unwrap();
        let mut world = World {
            det: &det,
            groups: state.platform.ready_groups(),
            psi: pressure,
            available_bytes: available,
            watchlist: &cfg.watchlist,
            goal: String::new(),
            tabs: Some(&mut closer),
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
