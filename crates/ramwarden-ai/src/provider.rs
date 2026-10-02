//! Choosing a tier, and degrading when one is unavailable.
//!
//! # The order, and why local comes first
//!
//! The prompt carries the titles and hosts of every open tab. The cloud tier is
//! measurably better — it spared a `localhost` dev server that the local 4B model
//! selected — but "better" is not the only axis. Default order is therefore:
//!
//! 1. **local** `nemotron-3-nano:4b`, if Ollama has it and the GPU has room
//! 2. **cloud** NVIDIA NIM, only when explicitly enabled
//! 3. **heuristic**, which the caller owns and which always works
//!
//! Set `prefer = "cloud"` to invert the first two. Having an API key on the
//! machine is not the same as consenting to send browsing data off it, so the
//! cloud tier stays off until asked for by name.

use std::collections::HashSet;

use crate::client::{Nim, Ollama};
use crate::prompt::{self, Context, Dropped, Recommendation};
use crate::rerank;
use crate::vram;

/// Which tier produced an answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Local,
    Cloud,
    /// Every model tier declined or failed; the caller's rules decide.
    Heuristic,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Local => "local",
            Tier::Cloud => "cloud",
            Tier::Heuristic => "heuristic",
        }
    }
}

/// Which tier to try first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Prefer {
    #[default]
    Local,
    Cloud,
}

/// What the model tier concluded, and everything a caller needs to report it.
#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub recommendation: Recommendation,
    pub tier: Option<&'static str>,
    /// What the clamp refused to pass on.
    pub dropped: Dropped,
    /// Why a tier was skipped or failed. Shown to the user, so it explains
    /// rather than just records.
    pub notes: Vec<String>,
}

impl Analysis {
    pub fn heuristic(reason: impl Into<String>) -> Self {
        Analysis {
            tier: Some(Tier::Heuristic.as_str()),
            notes: vec![reason.into()],
            ..Default::default()
        }
    }
}

pub struct Provider {
    pub ollama: Option<Ollama>,
    pub nim: Option<Nim>,
    /// Cloud stays off unless asked for, even when a key is present.
    pub cloud_enabled: bool,
    pub prefer: Prefer,
    /// Skip the local model when the GPU is already full of the user's work.
    pub respect_vram: bool,
}

impl Provider {
    /// Ask a model which tabs and processes to reclaim.
    ///
    /// `allowed_*` are the sets the daemon would actually permit. The reply is
    /// clamped to them, so a model naming something out of bounds costs a log line
    /// rather than a refused action the user already saw offered.
    pub async fn analyze(
        &self,
        ctx: &Context<'_>,
        allowed_processes: &HashSet<String>,
        allowed_terminals: &[i32],
        allowed_tabs: &[i64],
    ) -> Analysis {
        let user = ctx.render();
        let order: Vec<Tier> = match self.prefer {
            Prefer::Local => vec![Tier::Local, Tier::Cloud],
            Prefer::Cloud => vec![Tier::Cloud, Tier::Local],
        };

        let mut notes = Vec::new();
        for tier in order {
            match self.try_tier(tier, &user, &mut notes).await {
                Some(mut rec) => {
                    let dropped = rec.clamp_all(allowed_processes, allowed_terminals, allowed_tabs);
                    if !dropped.is_empty() {
                        let line = format!("{} suggested out-of-bounds {}", tier.as_str(), dropped.describe());
                        tracing::warn!("{line}");
                        notes.push(line);
                    }
                    ensure_summary(&mut rec);
                    return Analysis {
                        recommendation: rec,
                        tier: Some(tier.as_str()),
                        dropped,
                        notes,
                    };
                }
                None => continue,
            }
        }

        Analysis {
            tier: Some(Tier::Heuristic.as_str()),
            notes,
            ..Default::default()
        }
    }

    async fn try_tier(
        &self,
        tier: Tier,
        user: &str,
        notes: &mut Vec<String>,
    ) -> Option<Recommendation> {
        match tier {
            Tier::Local => {
                let o = self.ollama.as_ref()?;
                // A model that has to be pulled mid-request turns a one-second
                // analysis into a multi-gigabyte download, on a machine already
                // short of memory.
                let (ready, missing) = o.ready().await;
                if !ready {
                    notes.push(format!(
                        "local model unavailable (missing: {})",
                        if missing.is_empty() {
                            "ollama not reachable".to_string()
                        } else {
                            missing.join(", ")
                        }
                    ));
                    return None;
                }
                if self.respect_vram && vram::room_for_local_model() == Some(false) {
                    let used = vram::primary().map(|g| g.percent_used()).unwrap_or(0.0);
                    notes.push(format!(
                        "skipped the local model — GPU is {used:.0}% full of your work"
                    ));
                    return None;
                }
                match o.recommend(prompt::SYSTEM_PROMPT, user).await {
                    Ok(r) => Some(r),
                    Err(e) => {
                        notes.push(format!("local model failed: {e}"));
                        None
                    }
                }
            }
            Tier::Cloud => {
                if !self.cloud_enabled {
                    return None;
                }
                let n = self.nim.as_ref()?;
                match n.recommend(prompt::SYSTEM_PROMPT, user).await {
                    Ok(r) => Some(r),
                    Err(e) => {
                        notes.push(format!("cloud model failed: {e}"));
                        None
                    }
                }
            }
            Tier::Heuristic => None,
        }
    }

    /// Tabs that look unrelated to the goal, by embedding similarity.
    ///
    /// Returns `None` when relevance could not be judged — no goal, no embedder,
    /// or an embedder whose scores did not separate. `None` means "age decides
    /// alone", which is different from "nothing is irrelevant".
    pub async fn irrelevant_tabs(
        &self,
        goal: &str,
        tabs: &[(i64, String, String)],
    ) -> Option<Vec<i64>> {
        if goal.trim().is_empty() || tabs.is_empty() {
            return None;
        }
        let o = self.ollama.as_ref()?;

        let docs: Vec<String> = tabs
            .iter()
            .map(|(_, title, url)| rerank::tab_text(title, url))
            .collect();
        let goal_vector = o.embed_query(goal).await.ok()?;
        let doc_vectors = o.embed(&docs).await.ok()?;
        if goal_vector.is_empty() || doc_vectors.len() != docs.len() {
            tracing::debug!("embedding count mismatch — skipping relevance");
            return None;
        }

        let ids: Vec<i64> = tabs.iter().map(|(id, _, _)| *id).collect();
        let scored = rerank::rank(&goal_vector, &ids, &doc_vectors);
        if !rerank::discriminating(&scored) {
            tracing::info!(
                "embedder did not separate the tabs (spread {:.3}) — ignoring relevance",
                rerank::spread(&scored)
            );
            return None;
        }
        Some(rerank::irrelevant_relative(&scored, rerank::RELATIVE_CUT))
    }
}

/// Give the user a sentence even when the model did not.
///
/// `json_object` mode constrains only that the reply is JSON, not its shape, and
/// `nemotron-3-super-120b` leaves `summary` empty every time. A blank line in the
/// UI is worse than a generated one, and the counts are facts rather than claims.
pub fn ensure_summary(rec: &mut Recommendation) {
    if !rec.summary.trim().is_empty() {
        return;
    }
    let mut parts = Vec::new();
    if !rec.tabs_to_close.is_empty() {
        parts.push(format!("{} stale tab(s)", rec.tabs_to_close.len()));
    }
    if !rec.processes_to_suspend.is_empty() {
        parts.push(format!("{} idle app(s)", rec.processes_to_suspend.len()));
    }
    if !rec.idle_terminals_to_close.is_empty() {
        parts.push(format!("{} idle shell(s)", rec.idle_terminals_to_close.len()));
    }
    rec.summary = if parts.is_empty() {
        "nothing worth reclaiming right now.".to_string()
    } else {
        format!("{}.", parts.join(", "))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(tabs: Vec<i64>, procs: Vec<&str>) -> Recommendation {
        Recommendation {
            tabs_to_close: tabs,
            processes_to_suspend: procs.into_iter().map(String::from).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn tier_names_match_what_the_api_reports() {
        assert_eq!(Tier::Local.as_str(), "local");
        assert_eq!(Tier::Cloud.as_str(), "cloud");
        assert_eq!(Tier::Heuristic.as_str(), "heuristic");
    }

    #[test]
    fn local_is_preferred_by_default() {
        assert_eq!(Prefer::default(), Prefer::Local);
    }

    /// `nemotron-3-super-120b` leaves `summary` empty on every run. A blank line
    /// in the UI is worse than a generated one.
    #[test]
    fn an_empty_summary_is_filled_from_the_counts() {
        let mut r = rec(vec![1, 5], vec!["Discord"]);
        ensure_summary(&mut r);
        assert_eq!(r.summary, "2 stale tab(s), 1 idle app(s).");
    }

    #[test]
    fn a_models_own_summary_is_left_alone() {
        let mut r = rec(vec![1], vec![]);
        r.summary = "closed the YouTube tab you forgot about".into();
        ensure_summary(&mut r);
        assert_eq!(r.summary, "closed the YouTube tab you forgot about");
    }

    #[test]
    fn an_empty_recommendation_still_gets_a_sentence() {
        let mut r = Recommendation::default();
        ensure_summary(&mut r);
        assert_eq!(r.summary, "nothing worth reclaiming right now.");
    }

    #[test]
    fn a_whitespace_only_summary_counts_as_empty() {
        let mut r = rec(vec![1], vec![]);
        r.summary = "   \n ".into();
        ensure_summary(&mut r);
        assert_eq!(r.summary, "1 stale tab(s).");
    }

    #[test]
    fn idle_shells_appear_in_the_generated_summary() {
        let mut r = Recommendation {
            idle_terminals_to_close: vec![1, 2, 3],
            ..Default::default()
        };
        ensure_summary(&mut r);
        assert_eq!(r.summary, "3 idle shell(s).");
    }

    fn empty_provider(cloud: bool) -> Provider {
        Provider {
            ollama: None,
            nim: None,
            cloud_enabled: cloud,
            prefer: Prefer::Local,
            respect_vram: true,
        }
    }

    #[tokio::test]
    async fn with_no_tiers_configured_the_result_is_the_heuristic() {
        let p = empty_provider(true);
        let ctx = Context {
            psi_some: 10.0,
            used_mb: 1.0,
            total_mb: 2.0,
            inactivity_minutes: 45,
            goal: "",
            processes: &[],
            protected: &[],
            reclaimable: &[],
            tabs: &[],
            terminals: &[],
        };
        let a = p.analyze(&ctx, &HashSet::new(), &[], &[]).await;
        assert_eq!(a.tier, Some("heuristic"));
        assert!(a.recommendation.is_empty());
    }

    /// Relevance needs a goal; without one, age decides alone. `None` is the
    /// right answer, and is not the same as "nothing is irrelevant".
    #[tokio::test]
    async fn relevance_is_not_judged_without_a_goal() {
        let p = empty_provider(false);
        let tabs = vec![(1i64, "t".to_string(), "https://x/".to_string())];
        assert_eq!(p.irrelevant_tabs("", &tabs).await, None);
        assert_eq!(p.irrelevant_tabs("   ", &tabs).await, None);
        assert_eq!(p.irrelevant_tabs("a goal", &[]).await, None);
    }

    #[tokio::test]
    async fn relevance_without_an_embedder_is_unknown_rather_than_empty() {
        let p = empty_provider(false);
        let tabs = vec![(1i64, "t".to_string(), "https://x/".to_string())];
        assert_eq!(p.irrelevant_tabs("security research", &tabs).await, None);
    }

    /// Having a key is not consent to send browsing data off the machine.
    #[tokio::test]
    async fn the_cloud_tier_is_skipped_when_not_enabled() {
        use crate::secret::Secret;
        use std::time::Duration;

        let nim = Nim::new(
            "http://127.0.0.1:1",
            Secret::new("nvapi-wouldneverbeused0000000000000000"),
            "m",
            "e",
            Duration::from_millis(100),
        )
        .unwrap();
        let p = Provider {
            ollama: None,
            nim: Some(nim),
            cloud_enabled: false,
            prefer: Prefer::Cloud,
            respect_vram: true,
        };
        let ctx = Context {
            psi_some: 10.0,
            used_mb: 1.0,
            total_mb: 2.0,
            inactivity_minutes: 45,
            goal: "",
            processes: &[],
            protected: &[],
            reclaimable: &[],
            tabs: &[],
            terminals: &[],
        };
        let a = p.analyze(&ctx, &HashSet::new(), &[], &[]).await;
        assert_eq!(a.tier, Some("heuristic"));
        // Disabled means not attempted, so there is no failure to report.
        assert!(
            !a.notes.iter().any(|n| n.contains("cloud model failed")),
            "{:?}",
            a.notes
        );
    }
}
