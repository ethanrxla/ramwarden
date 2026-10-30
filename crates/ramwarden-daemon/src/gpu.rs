//! VRAM: who holds it, and the one lever RamWarden pulls on its own.
//!
//! A game and a loaded local model are the two big VRAM consumers on this
//! machine, and they collide: a 12 GB card running Fortnite has no room for an
//! idle 8 GB model LM Studio forgot to unload, and the game stutters as the
//! driver pages textures into system RAM. Unloading the model is clean — the
//! next request reloads it — so when a game is running and VRAM is tight,
//! RamWarden unloads idle local models. Nothing else on the GPU is touched.

use std::time::{Duration, Instant};

use ramwarden_ai::models::ModelHosts;
use ramwarden_ai::vram;
use ramwarden_core::detector::Detector;
use ramwarden_core::roles::Role;
use serde_json::{Value, json};

/// Don't unload again within this long: a model the user immediately
/// reloads should not be fought over.
pub const COOLDOWN: Duration = Duration::from_secs(300);

/// Whether the situation calls for unloading models. Pure.
pub fn should_unload(game_running: bool, vram_percent: f64, threshold: f64, models_loaded: usize) -> bool {
    game_running && models_loaded > 0 && vram_percent >= threshold
}

/// The running game, if any: the largest process in the game role, so the
/// game itself is named rather than its overlay or anti-cheat.
pub fn game_running(det: &Detector) -> Option<String> {
    det.snapshot()
        .values()
        .filter(|s| s.role == Role::Game)
        .max_by_key(|s| s.pss)
        .map(|s| s.name.clone())
}

/// Remembers when it last acted, for the cooldown.
#[derive(Default)]
pub struct GpuPolicy {
    last: Option<Instant>,
}

impl GpuPolicy {
    /// One check. Cheap unless a game is running and VRAM is tight; only then
    /// does it ask the runtimes what they have loaded.
    pub async fn tick(&mut self, game: Option<String>, threshold: f64, hosts: &ModelHosts) -> Vec<String> {
        let Some(game) = game else { return Vec::new() };
        if self.last.is_some_and(|t| t.elapsed() < COOLDOWN) {
            return Vec::new();
        }
        let Some(gpu) = vram::primary() else { return Vec::new() };
        if gpu.percent_used() < threshold {
            return Vec::new();
        }
        let loaded = hosts.loaded().await;
        if !should_unload(true, gpu.percent_used(), threshold, loaded.len()) {
            return Vec::new();
        }
        self.last = Some(Instant::now());
        let mut notes = Vec::new();
        for m in loaded {
            let r = hosts.unload(&m).await;
            notes.push(match r {
                Ok(()) => format!(
                    "unloaded {} model {} ({} MB) — {game} is running and VRAM was {:.0}% used",
                    m.host,
                    m.name,
                    m.bytes.unwrap_or(0) / 1_000_000,
                    gpu.percent_used()
                ),
                Err(e) => format!("could not unload {} model {}: {e}", m.host, m.name),
            });
        }
        notes
    }
}

/// Everything `/gpu` reports.
pub async fn report(det: &Detector, hosts: &ModelHosts) -> Value {
    let gpu = vram::primary();
    let procs: Vec<Value> = gpu
        .as_ref()
        .map(|g| vram::processes(g.index))
        .unwrap_or_default()
        .into_iter()
        .take(25)
        .map(|p| {
            json!({
                "pid": p.pid,
                "name": det.get(p.pid as i32).map(|s| s.name.clone()),
                "vram_mb": p.used / 1_000_000,
            })
        })
        .collect();
    json!({
        "gpu": gpu.map(|g| json!({
            "name": g.name, "used_mb": g.used / 1_000_000, "total_mb": g.total / 1_000_000,
            "percent_used": (g.percent_used() * 10.0).round() / 10.0,
            "utilisation": g.utilisation,
        })),
        "processes": procs,
        "models": hosts.loaded().await,
        "game": game_running(det),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_are_unloaded_only_for_a_game_on_a_full_card() {
        assert!(should_unload(true, 92.0, 85.0, 1));
        assert!(!should_unload(false, 99.0, 85.0, 1), "no game: the model is the user's work");
        assert!(!should_unload(true, 60.0, 85.0, 1), "room to spare");
        assert!(!should_unload(true, 99.0, 85.0, 0), "nothing loaded");
    }

    #[test]
    fn a_running_game_is_recognised_from_the_detector() {
        use ramwarden_core::roles::classify;
        use ramwarden_core::signals::Signals;
        let game = "FortniteClient-Win64-Shipping";
        let det = Detector::preloaded(
            true,
            vec![
                Signals::new(1, game, classify(game, "", None)),
                Signals::new(2, "brave", classify("brave", "", None)),
            ],
        );
        assert_eq!(game_running(&det).as_deref(), Some(game));
        let det = Detector::preloaded(true, vec![Signals::new(2, "brave", classify("brave", "", None))]);
        assert!(game_running(&det).is_none());
    }
}
