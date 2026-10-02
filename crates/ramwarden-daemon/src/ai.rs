//! Building the model tier from config, and the analysis the daemon serves.
//!
//! The heuristic is not a failure mode here, it is the floor. Every path through
//! this module ends in a usable answer: the model refines the tab selection and
//! the goal ranking, and when it cannot, the rules that shipped with v1 decide.

use std::collections::HashSet;
use std::time::Duration;

use ramwarden_ai::client::{Nim, Ollama};
use ramwarden_ai::prompt::Context;
use ramwarden_ai::provider::{Analysis, Prefer, Provider};
use ramwarden_ai::secret::Secret;
use ramwarden_core::config::Config;
use ramwarden_core::detector::Detector;
use ramwarden_core::signals::Verdict;

use crate::tabpolicy;
use crate::tabs::Tab;

/// Assemble the provider from config. Never fails: a tier that cannot be built
/// is simply absent.
pub fn provider(cfg: &Config) -> Provider {
    let ollama = Ollama::new(
        &cfg.ollama.host,
        &cfg.ollama.model,
        &cfg.ollama.embed_model,
        Duration::from_secs(cfg.ollama.timeout_seconds),
    )
    .inspect_err(|e| tracing::warn!("local model tier unavailable: {e}"))
    .ok();

    let nim = if cfg.nvidia.api_key.trim().is_empty() {
        None
    } else {
        Nim::new(
            &cfg.nvidia.base_url,
            Secret::new(cfg.nvidia.api_key.trim()),
            &cfg.nvidia.model,
            &cfg.nvidia.embed_model,
            Duration::from_secs(cfg.nvidia.timeout_seconds),
        )
        .inspect(|n| {
            tracing::info!(
                "cloud tier configured: {} key {} ({})",
                n.model,
                n.key_fingerprint(),
                if cfg.nvidia.enabled { "enabled" } else { "DISABLED — set [nvidia] enabled = true to use it" }
            );
        })
        .inspect_err(|e| tracing::warn!("cloud model tier unavailable: {e}"))
        .ok()
    };

    Provider {
        ollama,
        nim,
        cloud_enabled: cfg.nvidia.enabled,
        prefer: if cfg.nvidia.prefer.eq_ignore_ascii_case("cloud") {
            Prefer::Cloud
        } else {
            Prefer::Local
        },
        respect_vram: cfg.ollama.respect_vram,
    }
}

/// What the daemon decided, from whichever tier answered.
pub struct Decision {
    pub analysis: Analysis,
    /// The tabs that will actually be closed, after protection and clamping.
    pub tabs: Vec<i64>,
    /// Tabs the goal ranking judged irrelevant, if it could judge.
    pub irrelevant: Option<Vec<i64>>,
    pub summary: String,
}

/// The memory picture the prompt reports.
#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryState {
    /// PSI `some avg10` — the share of the last ten seconds spent stalled.
    pub psi_some: f64,
    pub used_mb: f64,
    pub total_mb: f64,
}

/// Everything the model tier needs from the detector, owned.
///
/// Extracted synchronously so no lock guard is held across an `await`. A
/// `RwLockReadGuard` is not `Send`, and holding one over the model call makes the
/// whole handler future non-`Send` — which axum rejects, and which would in any
/// case block every other reader for the length of a network round trip.
pub struct Snapshot {
    pub processes: Vec<(String, f64, &'static str)>,
    pub protected: Vec<(String, f64, String)>,
    pub reclaimable: Vec<(String, f64, String)>,
    pub reclaimable_names: HashSet<String>,
}

impl Snapshot {
    pub fn take(det: &Detector, watchlist: &[String]) -> Self {
        let mut sigs: Vec<_> = det.snapshot().values().collect();
        sigs.sort_by_key(|s| std::cmp::Reverse(s.pss));

        let processes = sigs
            .iter()
            .take(10)
            .map(|s| (s.name.clone(), s.pss as f64 / 1e6, s.verdict.as_str()))
            .collect();
        let protected = sigs
            .iter()
            .filter(|s| s.verdict == Verdict::Protected && s.pss >= 100 * 1024 * 1024)
            .take(12)
            .map(|s| {
                (
                    s.name.clone(),
                    s.pss as f64 / 1e6,
                    s.reasons.first().cloned().unwrap_or_default(),
                )
            })
            .collect();
        let reclaimable: Vec<(String, f64, String)> = det
            .reclaimable(watchlist)
            .iter()
            .map(|s| (s.name.clone(), s.pss as f64 / 1e6, s.reasons.join("; ")))
            .collect();
        let reclaimable_names = reclaimable.iter().map(|(n, _, _)| n.clone()).collect();

        Snapshot {
            processes,
            protected,
            reclaimable,
            reclaimable_names,
        }
    }
}

/// Decide what to reclaim.
///
/// The heuristic runs first and defines the *bounds*: nothing outside its
/// selection may be closed, whatever a model says. The model then narrows that
/// set, and the goal ranking can widen it — but only within tabs that are not
/// protected.
pub async fn decide(
    cfg: &Config,
    provider: &Provider,
    snap: &Snapshot,
    tabs: &[Tab],
    state: MemoryState,
    goal: &str,
) -> Decision {
    let MemoryState {
        psi_some,
        used_mb,
        total_mb,
    } = state;
    let threshold = cfg.thresholds.inactivity_minutes as i64;

    // The floor, and the ceiling. A tab the rules protect is never closeable, so
    // this is the set every later stage may only subtract from.
    let heuristic: Vec<i64> = tabpolicy::stale_tabs(tabs, threshold)
        .iter()
        .map(|t| t.id)
        .collect();

    // Relevance may *add* to the set, but only among tabs that are not protected
    // — an unsaved document is not closeable just because it is off-topic.
    let rankable: Vec<(i64, String, String)> = tabs
        .iter()
        .filter(|t| !tabpolicy::is_protected(&t.url))
        .map(|t| (t.id, t.title.clone(), t.url.clone()))
        .collect();
    let irrelevant = provider.irrelevant_tabs(goal, &rankable).await;

    let mut allowed: HashSet<i64> = heuristic.iter().copied().collect();
    if let Some(off_topic) = &irrelevant {
        allowed.extend(off_topic.iter().copied());
    }
    let mut allowed_tabs: Vec<i64> = allowed.into_iter().collect();
    allowed_tabs.sort_unstable();

    let terms = ramwarden_core::terminals::list(&ramwarden_kernel::Root::system()).unwrap_or_default();
    let allowed_terminals: Vec<i32> =
        terms.iter().filter(|t| t.is_idle).map(|t| t.pid).collect();

    let tab_rows: Vec<(i64, i64, String, String)> = tabs
        .iter()
        .filter(|t| allowed_tabs.contains(&t.id))
        .map(|t| (t.id, t.inactive_minutes, t.url.clone(), t.title.clone()))
        .collect();
    let term_rows: Vec<(i32, String, bool, Vec<String>)> = terms
        .iter()
        .map(|t| (t.pid, t.shell.clone(), t.is_idle, t.children.clone()))
        .collect();

    let ctx = Context {
        psi_some,
        used_mb,
        total_mb,
        inactivity_minutes: cfg.thresholds.inactivity_minutes,
        goal,
        processes: &snap.processes,
        protected: &snap.protected,
        reclaimable: &snap.reclaimable,
        tabs: &tab_rows,
        terminals: &term_rows,
    };

    let analysis = provider
        .analyze(&ctx, &snap.reclaimable_names, &allowed_terminals, &allowed_tabs)
        .await;

    // A model that answered picks from the allowed set; otherwise the heuristic's
    // own selection stands.
    let model_answered = analysis.tier != Some("heuristic");
    let tabs_out = if model_answered {
        analysis.recommendation.tabs_to_close.clone()
    } else {
        heuristic.clone()
    };

    let summary = if model_answered && !analysis.recommendation.summary.trim().is_empty() {
        analysis.recommendation.summary.clone()
    } else {
        tabpolicy::summary(tabs_out.len(), psi_some)
    };

    Decision {
        analysis,
        tabs: tabs_out,
        irrelevant,
        summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramwarden_core::config;

    fn cfg_with(enabled: bool, key: &str, prefer: &str) -> Config {
        let mut c = config::Config::default();
        c.nvidia.enabled = enabled;
        c.nvidia.api_key = key.to_string();
        c.nvidia.prefer = prefer.to_string();
        c
    }

    #[test]
    fn a_provider_is_built_even_with_nothing_configured() {
        let p = provider(&Config::default());
        // Ollama is always constructed — it may simply not answer.
        assert!(p.ollama.is_some());
        assert!(p.nim.is_none(), "no key means no cloud client");
        assert!(!p.cloud_enabled);
    }

    /// A key alone must not switch the cloud tier on.
    #[test]
    fn a_key_without_enabled_builds_the_client_but_leaves_it_off() {
        let p = provider(&cfg_with(false, "nvapi-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "local"));
        assert!(p.nim.is_some(), "the client is built so it can be reported");
        assert!(!p.cloud_enabled, "but it must not be used");
    }

    #[test]
    fn enabling_the_cloud_tier_and_preferring_it_both_take_effect() {
        let p = provider(&cfg_with(true, "nvapi-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "cloud"));
        assert!(p.cloud_enabled);
        assert_eq!(p.prefer, Prefer::Cloud);
    }

    #[test]
    fn the_prefer_field_is_case_insensitive_and_defaults_to_local() {
        assert_eq!(provider(&cfg_with(true, "nvapi-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "CLOUD")).prefer, Prefer::Cloud);
        assert_eq!(provider(&cfg_with(true, "nvapi-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "nonsense")).prefer, Prefer::Local);
    }

    #[test]
    fn a_blank_key_is_treated_as_no_key() {
        assert!(provider(&cfg_with(true, "   ", "local")).nim.is_none());
    }
}
