//! The loop that drives the ladder.
//!
//! # Kernel wakeups where possible, adaptive polling where not
//!
//! The intended design was a PSI trigger: tell the kernel "wake me if tasks
//! stall more than N ms per second" and block on `poll()`. That is still
//! attempted first, and on a kernel that permits it the loop reacts in under a
//! second while costing nothing when idle.
//!
//! It does not work on the machine this was written for. Linux 7.1.5 opens
//! `/proc/pressure/memory` `O_RDWR` and reads it happily, but rejects every
//! trigger write with `EINVAL` — trigger creation is privileged there, and the
//! cgroup `memory.pressure` files are not writable at all.
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
use std::time::{Duration, Instant};

use ramwarden_core::actuator::Outcome;
use ramwarden_core::history::{SampleContext, SignalSample};
use ramwarden_core::signals::Signals;
use ramwarden_core::desktop::DesktopProbe;
use ramwarden_core::ladder::{self, TabCloser, World};
use ramwarden_kernel::{cgroup, psi};

use crate::hub::AppState;

/// How long the stall must last within the window to wake the thread.
///
/// 50 ms per second is roughly "5% pressure sustained" — comfortably below the
/// first rung, so the ladder is awake and sampling before it needs to act.
const STALL_BUDGET: Duration = Duration::from_millis(50);
const STALL_WINDOW: Duration = Duration::from_secs(1);

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
/// The ladder is synchronous; closing tabs means awaiting a browser. The monitor
/// runs on its own OS thread rather than inside the runtime, so it can block on
/// the async work — which would deadlock if this ran as a tokio task.
struct Tabs {
    state: AppState,
    handle: tokio::runtime::Handle,
}

impl TabCloser for Tabs {
    fn close_stale(&mut self, goal: &str) -> Outcome {
        let state = self.state.clone();
        let _ = goal;
        self.handle.block_on(async move {
            state.request_all_tabs().await;
            let targets = {
                let hub = state.hub.lock().unwrap();
                crate::browser::snapshot(&hub.reg, state.cfg.thresholds.inactivity_minutes as i64)
                    .into_iter().filter(|r|r.eligible).take(crate::browser::BATCH_LIMIT)
                    .map(|r|r.target).collect::<Vec<_>>()
            };
            let result = state.discard_tabs(&targets).await;
            Outcome { affected: Vec::new(), bytes_freed: 0,
                notes: vec![format!("{} tabs unloaded, {} queued, {} refused; tabs remain open",
                    result.confirmed.len(), result.queued.len(), result.refused.len())] }
        })
    }
}

/// Start the monitor on its own thread.
pub fn spawn(state: AppState, handle: tokio::runtime::Handle) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("ramwarden-monitor".into())
        .spawn(move || run(state, handle))
        .expect("spawn monitor thread")
}

fn run(state: AppState, handle: tokio::runtime::Handle) {
    let mut probe = DesktopProbe::new();
    let uid = unsafe { getuid() };

    // The trigger is a nicety, not a requirement: without CONFIG_PSI the loop
    // falls back to the heartbeat alone, which is still better than v1's.
    let mut trigger = match psi::Trigger::memory(&state.root, STALL_BUDGET, STALL_WINDOW) {
        Ok(t) => {
            tracing::info!(
                "PSI trigger armed: waking on {}ms of stall per {}s",
                STALL_BUDGET.as_millis(),
                STALL_WINDOW.as_secs()
            );
            Some(t)
        }
        Err(e) => {
            tracing::warn!(
                "PSI trigger unavailable ({e}) — using adaptive polling \
                 ({}s idle / {}s watchful / {}s busy)",
                IDLE_INTERVAL.as_secs(),
                WATCHFUL_INTERVAL.as_secs(),
                BUSY_INTERVAL.as_secs()
            );
            None
        }
    };

    let cfg = Arc::clone(&state.cfg);
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

        // 2. Look.
        let pressure = match psi::system_memory(&state.root) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("pressure read failed: {e}");
                continue;
            }
        };
        let available = ladder::available_bytes(&state.root);
        let hierarchy = cgroup::Hierarchy::user_session(&state.root, uid).ok();

        // 3. Act.
        let mut closer = Tabs {
            state: state.clone(),
            handle: handle.clone(),
        };
        let det = state.det.read().unwrap();
        let mut world = World {
            det: &det,
            hierarchy: hierarchy.as_ref(),
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

        if report.did_something() || woken_by_pressure {
            tracing::info!(
                "rung {} (some={:.1}% full={:.1}% avail={} MB) — freed {} MB in {:?}",
                report.rung.map(|r| r.as_str()).unwrap_or("?"),
                pressure.some.avg10,
                pressure.full.avg10,
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
                psi_some: pressure.some.avg10,
                psi_full: pressure.full.avg10,
                available: available as i64,
            };
            if let Ok(h) = state.history.lock()
                && let Err(e) = h.log_signals(&samples, ctx)
            {
                tracing::debug!("behaviour log write failed: {e}");
            }
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

unsafe extern "C" {
    #[link_name = "getuid"]
    fn getuid() -> u32;
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

    /// The trigger is optional. If this machine grants one, good; if not, the
    /// loop must still be able to start.
    #[test]
    fn a_missing_trigger_does_not_prevent_the_loop_from_running() {
        let root = ramwarden_kernel::Root::system();
        match psi::Trigger::memory(&root, STALL_BUDGET, STALL_WINDOW) {
            Ok(_) => {}
            Err(e) => assert!(
                matches!(
                    e,
                    ramwarden_kernel::Error::Unsupported(_) | ramwarden_kernel::Error::Denied { .. }
                ),
                "a trigger refusal must be a capability error, not {e:?}"
            ),
        }
    }
}
