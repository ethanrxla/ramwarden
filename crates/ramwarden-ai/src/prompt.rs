//! What we ask the model, and how its answer is read back.
//!
//! The system prompt and the reply schema are ported from v1 nearly verbatim.
//! They encode rules learned from watching the thing misbehave — "only name
//! processes from the RECLAIMABLE list", "never close devtools://" — and a
//! smaller local model needs those rules more than Sonnet did, not less.
//!
//! # Two guardrails that matter more now
//!
//! **The reply is clamped.** Whatever the model names, only processes the
//! activity gate already approved survive [`Recommendation::clamp`]. The daemon
//! would refuse the rest anyway; dropping them here stops the UI promising the
//! user something that will then be denied.
//!
//! **The parser is forgiving.** v1's `_extract_json` took the outermost brace
//! pair because replies arrive wrapped in markdown fences or prefaced with a
//! sentence. A 4B model does that far more often than a frontier model, and
//! Nemotron is a reasoning model whose visible thinking has to be stepped over.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// Ported from v1, with the Claude-specific framing removed.
pub const SYSTEM_PROMPT: &str = r#"You are a RAM management assistant for a Linux desktop.

You receive browser tab data, process info, terminal shell info, and optionally the user's current goal.
Your job: recommend which STALE tabs to close and which idle processes to suspend.

RULES — read carefully:
1. Tab closing:
   - ONLY close tabs inactive > the stated threshold.
   - DO NOT close tabs that look like active work: auth pages, localhost/dev servers, online documents or editors, anything the user is mid-task on.
   - DO close: background social media, finished videos, old search results, news articles, shopping tabs — things the user forgot about.
   - If several tabs are open on the same site, close only the most inactive ones.
   - Be assertive but not reckless.
   - NEVER close: devtools://, chrome://, about:, moz-extension://, or any internal browser page.

2. When a USER GOAL is provided:
   - Tabs RELEVANT to the goal → KEEP, even if recently inactive.
   - Tabs CLEARLY IRRELEVANT to the goal → CLOSE, even if recently visited.
   - When in doubt, keep the tab and say so in the summary.

3. Process suspension:
   - ONLY name processes from the RECLAIMABLE list below. It is already filtered by
     the watchlist and by live activity signals — anything absent from it will be
     refused by the daemon regardless of what you say, so naming it wastes the turn.
   - The PROTECTED list explains what is off-limits and why. Do not argue with it.
   - Prefer suspending nothing over suspending something the user is mid-way through.

4. Idle terminals:
   - idle_terminals_to_close: ONLY PIDs with zero child processes.
   - NEVER include busy shells (running agents, servers, builds).

5. Workspace sorting:
   - workspaces_to_sort: only true if the layout is messy and rules are configured.

6. Format: respond with ONLY the JSON object, no prose, no fences, no reasoning."#;

/// The reply shape, as a JSON Schema.
///
/// Ollama takes this in its `format` field and NVIDIA NIM in
/// `response_format.json_schema`. Constraining the reply is what stops a model
/// answering a tab list in prose — which v1 saw happen, and which silently
/// dropped the whole analysis back to the heuristic for a reason unrelated to RAM.
pub fn reply_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "tabs_to_close": {"type": "array", "items": {"type": "integer"}},
            "processes_to_suspend": {"type": "array", "items": {"type": "string"}},
            "idle_terminals_to_close": {"type": "array", "items": {"type": "integer"}},
            "workspaces_to_sort": {"type": "boolean"},
            "estimated_ram_freed_mb": {"type": "number"},
            "summary": {"type": "string"}
        },
        "required": [
            "tabs_to_close", "processes_to_suspend", "idle_terminals_to_close",
            "workspaces_to_sort", "estimated_ram_freed_mb", "summary"
        ],
        "additionalProperties": false
    })
}

/// What the model recommends.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Recommendation {
    /// Tab ids. Parsed leniently because a small model echoes the prompt's own
    /// rendering: asked for ids it had seen written as `id=1`, nemotron-3-nano
    /// returned the *strings* `["id=1","id=5"]`. The schema prevents that when it
    /// is enforced, and this copes when it is not.
    #[serde(default, deserialize_with = "lenient_ids")]
    pub tabs_to_close: Vec<i64>,
    #[serde(default)]
    pub processes_to_suspend: Vec<String>,
    /// `terminals_to_close` is accepted as an alias: the model used that name
    /// unprompted, and refusing it loses a correct answer over a label.
    #[serde(default, alias = "terminals_to_close", deserialize_with = "lenient_ids_i32")]
    pub idle_terminals_to_close: Vec<i32>,
    #[serde(default)]
    pub workspaces_to_sort: bool,
    #[serde(default)]
    pub estimated_ram_freed_mb: f64,
    #[serde(default)]
    pub summary: String,
    /// Which tier produced this: `heuristic`, `local`, or `cloud`.
    #[serde(default)]
    pub tier: String,
}

impl Recommendation {
    /// Drop anything the daemon would refuse anyway.
    ///
    /// Returns what was dropped, so the caller can log that the model went out of
    /// bounds — useful signal about prompt drift, and the reason v1 logged it too.
    ///
    /// Observed with `nemotron-3-nano:4b`: told explicitly not to close dev
    /// servers or online documents, it still selected `localhost:3000/admin` and a
    /// SharePoint document. The daemon's protection rules would refuse both, but
    /// without this the UI would have offered them to the user first.
    pub fn clamp_all(
        &mut self,
        allowed_processes: &HashSet<String>,
        allowed_terminals: &[i32],
        allowed_tabs: &[i64],
    ) -> Dropped {
        let names = self.clamp(allowed_processes, allowed_terminals);

        let (keep, drop): (Vec<i64>, Vec<i64>) = self
            .tabs_to_close
            .drain(..)
            .partition(|id| allowed_tabs.contains(id));
        self.tabs_to_close = keep;

        Dropped {
            processes: names,
            tabs: drop,
        }
    }

    /// Drop anything the daemon would refuse anyway.
    ///
    /// Returns the names that were dropped, so the caller can log that the model
    /// suggested something out of bounds — useful signal about prompt drift, and
    /// the reason v1 logged it too.
    pub fn clamp(&mut self, allowed_processes: &HashSet<String>, allowed_terminals: &[i32]) -> Vec<String> {
        let (keep, drop): (Vec<String>, Vec<String>) = self
            .processes_to_suspend
            .drain(..)
            .partition(|n| allowed_processes.contains(n));
        self.processes_to_suspend = keep;

        // A busy shell holding a build or an agent must never be closed, however
        // confidently it is named.
        self.idle_terminals_to_close
            .retain(|pid| allowed_terminals.contains(pid));

        drop
    }

    pub fn is_empty(&self) -> bool {
        self.tabs_to_close.is_empty()
            && self.processes_to_suspend.is_empty()
            && self.idle_terminals_to_close.is_empty()
            && !self.workspaces_to_sort
    }
}

/// What [`Recommendation::clamp_all`] refused to pass on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dropped {
    pub processes: Vec<String>,
    pub tabs: Vec<i64>,
}

impl Dropped {
    pub fn is_empty(&self) -> bool {
        self.processes.is_empty() && self.tabs.is_empty()
    }

    /// A line for the log, when the model overreached.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.processes.is_empty() {
            parts.push(format!("processes {:?}", self.processes));
        }
        if !self.tabs.is_empty() {
            parts.push(format!("protected tabs {:?}", self.tabs));
        }
        parts.join(", ")
    }
}

/// Parse one id from an integer, a float, or a string like `"7"` or `"id=7"`.
fn one_id(v: &serde_json::Value) -> Option<i64> {
    match v {
        serde_json::Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.round() as i64)),
        serde_json::Value::String(s) => {
            let digits: String = s.chars().filter(|c| c.is_ascii_digit() || *c == '-').collect();
            digits.parse().ok()
        }
        _ => None,
    }
}

fn lenient_ids<'de, D>(d: D) -> std::result::Result<Vec<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<serde_json::Value>::deserialize(d).unwrap_or_default();
    Ok(raw.iter().filter_map(one_id).collect())
}

fn lenient_ids_i32<'de, D>(d: D) -> std::result::Result<Vec<i32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<serde_json::Value>::deserialize(d).unwrap_or_default();
    Ok(raw
        .iter()
        .filter_map(one_id)
        .filter_map(|v| i32::try_from(v).ok())
        .collect())
}

/// Pull the JSON object out of a model reply.
///
/// Tries the whole string first, then the outermost brace pair. That second step
/// is what copes with markdown fences, a sentence of preamble, and a reasoning
/// model's visible thinking — all of which a 4B model emits readily even when
/// told not to.
pub fn extract_json(raw: &str) -> Result<Recommendation, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err("empty reply".into());
    }

    if let Ok(r) = serde_json::from_str::<Recommendation>(text) {
        return Ok(r);
    }

    let start = text.find('{');
    let end = text.rfind('}');
    if let (Some(s), Some(e)) = (start, end)
        && e > s
        && let Ok(r) = serde_json::from_str::<Recommendation>(&text[s..=e])
    {
        return Ok(r);
    }

    Err(format!(
        "no JSON object in the reply (first 200 chars: {:?})",
        &text[..text.len().min(200)]
    ))
}

/// Strip a reasoning model's `<think>` block.
///
/// Nemotron is reasoning-capable and emits its thinking even with the schema
/// set. The brace scan would otherwise pick up a brace from inside the reasoning.
pub fn strip_thinking(raw: &str) -> &str {
    if let Some(end) = raw.find("</think>") {
        return raw[end + "</think>".len()..].trim_start();
    }
    raw
}

/// Build the user message.
pub struct Context<'a> {
    pub psi_some: f64,
    pub used_mb: f64,
    pub total_mb: f64,
    pub inactivity_minutes: u32,
    pub goal: &'a str,
    /// (name, pss_mb, verdict) for the largest processes.
    pub processes: &'a [(String, f64, &'static str)],
    pub protected: &'a [(String, f64, String)],
    pub reclaimable: &'a [(String, f64, String)],
    /// (id, inactive_minutes, url, title)
    pub tabs: &'a [(i64, i64, String, String)],
    /// (pid, shell, idle, children)
    pub terminals: &'a [(i32, String, bool, Vec<String>)],
}

impl Context<'_> {
    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "Memory pressure: {:.1}% stalled (PSI some avg10); {:.0} MB of {:.0} MB committed\n\
             Inactivity threshold: {} min\n",
            self.psi_some, self.used_mb, self.total_mb, self.inactivity_minutes
        ));

        if !self.goal.is_empty() {
            s.push_str(&format!(
                "\nUSER'S CURRENT GOAL: \"{}\"\n\
                 Use this to decide relevance — keep goal-related tabs, close unrelated ones.\n",
                self.goal
            ));
        }

        s.push_str("\nTop processes:\n");
        for (name, mb, verdict) in self.processes.iter().take(10) {
            s.push_str(&format!("  - {name}: {mb:.0} MB [{verdict}]\n"));
        }

        s.push_str("\nPROTECTED — never suggest these:\n");
        if self.protected.is_empty() {
            s.push_str("  none\n");
        }
        for (name, mb, why) in self.protected.iter().take(12) {
            s.push_str(&format!("  - {name} ({mb:.0} MB): {why}\n"));
        }

        s.push_str("\nRECLAIMABLE — the only names you may put in processes_to_suspend:\n");
        if self.reclaimable.is_empty() {
            s.push_str("  none — suspend nothing this round\n");
        }
        for (name, mb, why) in self.reclaimable {
            s.push_str(&format!("  - {name} ({mb:.0} MB): {why}\n"));
        }

        s.push_str("\nBrowser tabs:\n");
        if self.tabs.is_empty() {
            s.push_str("  (no tabs reported)\n");
        }
        s.push_str("  (the leading number is the tab id; put bare integers in tabs_to_close)\n");
        for (id, inactive, url, title) in self.tabs.iter().take(120) {
            s.push_str(&format!(
                "  {id}. inactive {inactive}min — {url} — \"{title}\"\n"
            ));
        }

        s.push_str("\nTerminal shells:\n");
        if self.terminals.is_empty() {
            s.push_str("  none found\n");
        }
        for (pid, shell, idle, children) in self.terminals {
            if *idle {
                s.push_str(&format!("  - PID={pid} ({shell}) — IDLE, safe to close\n"));
            } else {
                s.push_str(&format!(
                    "  - PID={pid} ({shell}) children: {} — DO NOT CLOSE\n",
                    children
                        .iter()
                        .take(3)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }

        s.push_str("\nReturn your JSON recommendation.");
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"tabs_to_close":[1,2],"processes_to_suspend":["Discord"],
        "idle_terminals_to_close":[100],"workspaces_to_sort":false,
        "estimated_ram_freed_mb":512.5,"summary":"closed two stale tabs"}"#;

    #[test]
    fn parses_a_clean_reply() {
        let r = extract_json(GOOD).unwrap();
        assert_eq!(r.tabs_to_close, vec![1, 2]);
        assert_eq!(r.processes_to_suspend, vec!["Discord"]);
        assert_eq!(r.idle_terminals_to_close, vec![100]);
        assert_eq!(r.estimated_ram_freed_mb, 512.5);
        assert_eq!(r.summary, "closed two stale tabs");
    }

    /// Models wrap JSON in fences even when told not to. v1 hit this; a 4B model
    /// hits it more.
    #[test]
    fn parses_a_reply_wrapped_in_a_markdown_fence() {
        let r = extract_json(&format!("```json\n{GOOD}\n```")).unwrap();
        assert_eq!(r.tabs_to_close, vec![1, 2]);
    }

    #[test]
    fn parses_a_reply_with_a_sentence_of_preamble() {
        let raw = format!("Sure! Here is the recommendation you asked for:\n\n{GOOD}\n\nHope that helps.");
        assert_eq!(extract_json(&raw).unwrap().tabs_to_close, vec![1, 2]);
    }

    /// Nemotron is reasoning-capable and emits its thinking even with a schema
    /// set. A brace inside the reasoning would otherwise confuse the scan.
    #[test]
    fn reasoning_output_is_stripped_before_parsing() {
        let raw = format!(
            "<think>Let me consider {{this}} carefully. The user has 3 tabs.</think>\n{GOOD}"
        );
        let r = extract_json(strip_thinking(&raw)).unwrap();
        assert_eq!(r.tabs_to_close, vec![1, 2]);
    }

    #[test]
    fn stripping_thinking_leaves_a_reply_without_it_alone() {
        assert_eq!(strip_thinking(GOOD), GOOD);
    }

    #[test]
    fn a_reply_with_missing_fields_still_parses_with_defaults() {
        let r = extract_json(r#"{"summary":"nothing to do"}"#).unwrap();
        assert!(r.tabs_to_close.is_empty());
        assert!(!r.workspaces_to_sort);
        assert_eq!(r.summary, "nothing to do");
        assert!(r.is_empty());
    }

    #[test]
    fn a_prose_only_reply_is_an_error_with_the_text_quoted() {
        let err = extract_json("I think you should close some tabs.").unwrap_err();
        assert!(err.contains("no JSON object"), "{err}");
        assert!(err.contains("close some tabs"), "the reply must be quoted: {err}");
    }

    #[test]
    fn an_empty_reply_is_an_error() {
        assert!(extract_json("").is_err());
        assert!(extract_json("   \n ").is_err());
    }

    // ── Clamping ────────────────────────────────────────────────────────────

    /// The guardrail that matters most: whatever the model names, only what the
    /// activity gate already approved survives.
    #[test]
    fn a_hallucinated_process_is_dropped_and_reported() {
        let mut r = extract_json(
            r#"{"processes_to_suspend":["Discord","qemu-system-x86_64","cosmic-comp"],
                "tabs_to_close":[],"idle_terminals_to_close":[],
                "workspaces_to_sort":false,"estimated_ram_freed_mb":0,"summary":""}"#,
        )
        .unwrap();

        let allowed: HashSet<String> = ["Discord".to_string()].into_iter().collect();
        let dropped = r.clamp(&allowed, &[]);

        assert_eq!(r.processes_to_suspend, vec!["Discord"]);
        assert_eq!(dropped.len(), 2);
        assert!(dropped.contains(&"qemu-system-x86_64".to_string()));
        assert!(dropped.contains(&"cosmic-comp".to_string()));
    }

    /// A busy shell holding an agent must never be closed, however confidently
    /// the model names it.
    #[test]
    fn a_busy_terminal_is_dropped_from_the_recommendation() {
        let mut r = Recommendation {
            idle_terminals_to_close: vec![100, 200, 300],
            ..Default::default()
        };
        r.clamp(&HashSet::new(), &[100, 300]);
        assert_eq!(r.idle_terminals_to_close, vec![100, 300]);
    }

    /// What nemotron-3-nano:4b actually did: picked a dev server and a SharePoint
    /// document despite being told not to. The daemon would refuse both, but the
    /// UI must not offer them in the first place.
    #[test]
    fn protected_tabs_the_model_picked_anyway_are_dropped() {
        let mut r = Recommendation {
            // 1 = YouTube, 2 = localhost dev server, 3 = SharePoint doc, 5 = recipe
            tabs_to_close: vec![1, 2, 3, 5],
            processes_to_suspend: vec!["Discord".into()],
            ..Default::default()
        };
        let allowed_tabs = vec![1, 5]; // only the genuinely closeable ones
        let allowed: HashSet<String> = ["Discord".to_string()].into_iter().collect();

        let dropped = r.clamp_all(&allowed, &[], &allowed_tabs);

        assert_eq!(r.tabs_to_close, vec![1, 5]);
        assert_eq!(dropped.tabs, vec![2, 3]);
        assert!(dropped.processes.is_empty());
        assert!(dropped.describe().contains("protected tabs"), "{}", dropped.describe());
    }

    #[test]
    fn a_fully_in_bounds_recommendation_drops_nothing() {
        let mut r = Recommendation {
            tabs_to_close: vec![1],
            processes_to_suspend: vec!["Discord".into()],
            idle_terminals_to_close: vec![100],
            ..Default::default()
        };
        let allowed: HashSet<String> = ["Discord".to_string()].into_iter().collect();
        let dropped = r.clamp_all(&allowed, &[100], &[1]);
        assert!(dropped.is_empty());
        assert_eq!(dropped.describe(), "");
    }

    /// The model invented "12000 MB freed" from four tabs and one chat app. The
    /// figure is display-only and nothing may decide from it.
    #[test]
    fn the_models_freed_estimate_is_not_load_bearing() {
        let r = extract_json(r#"{"estimated_ram_freed_mb":12000,"summary":"x"}"#).unwrap();
        assert_eq!(r.estimated_ram_freed_mb, 12000.0);
        // It contributes nothing to whether there is anything to do.
        assert!(r.is_empty(), "an estimate alone is not a recommendation");
    }

    #[test]
    fn clamping_an_empty_allowed_set_removes_every_process() {
        let mut r = Recommendation {
            processes_to_suspend: vec!["Discord".into(), "anything".into()],
            ..Default::default()
        };
        let dropped = r.clamp(&HashSet::new(), &[]);
        assert!(r.processes_to_suspend.is_empty());
        assert_eq!(dropped.len(), 2);
    }

    // ── Schema and prompt ───────────────────────────────────────────────────

    #[test]
    fn the_schema_requires_every_field_and_forbids_extras() {
        let s = reply_schema();
        let required = s["required"].as_array().unwrap();
        assert_eq!(required.len(), 6);
        assert_eq!(s["additionalProperties"], serde_json::json!(false));
        for f in ["tabs_to_close", "processes_to_suspend", "summary"] {
            assert!(s["properties"].get(f).is_some(), "{f}");
        }
    }

    /// A clean reply must satisfy the schema's own required list, or the schema
    /// and the struct have drifted apart.
    #[test]
    fn the_schema_and_the_struct_agree_on_field_names() {
        let value: serde_json::Value = serde_json::from_str(GOOD).unwrap();
        for f in reply_schema()["required"].as_array().unwrap() {
            let name = f.as_str().unwrap();
            assert!(value.get(name).is_some(), "schema requires {name}, reply lacks it");
        }
    }

    fn ctx<'a>(goal: &'a str, tabs: &'a [(i64, i64, String, String)]) -> Context<'a> {
        Context {
            psi_some: 12.5,
            used_mb: 22_000.0,
            total_mb: 31_000.0,
            inactivity_minutes: 45,
            goal,
            processes: &[],
            protected: &[],
            reclaimable: &[],
            tabs,
            terminals: &[],
        }
    }

    #[test]
    fn the_prompt_states_the_pressure_and_the_threshold() {
        let p = ctx("", &[]).render();
        assert!(p.contains("12.5%"), "{p}");
        assert!(p.contains("45 min"), "{p}");
        assert!(p.contains("(no tabs reported)"));
    }

    #[test]
    fn a_goal_appears_in_the_prompt_and_an_empty_one_does_not() {
        assert!(ctx("bug bounty", &[]).render().contains("bug bounty"));
        assert!(!ctx("", &[]).render().contains("USER'S CURRENT GOAL"));
    }

    /// The model must be told what it may *not* name, or it will name it.
    #[test]
    fn an_empty_reclaimable_list_says_so_explicitly() {
        let p = ctx("", &[]).render();
        assert!(p.contains("none — suspend nothing this round"), "{p}");
        assert!(p.contains("PROTECTED — never suggest these"), "{p}");
    }

    #[test]
    fn tabs_are_rendered_with_their_ids_and_ages() {
        let tabs = vec![(7i64, 6087i64, "https://x.test/".to_string(), "X".to_string())];
        let p = ctx("", &tabs).render();
        assert!(p.contains("7. inactive 6087min"), "{p}");
        assert!(p.contains("bare integers"), "the id type must be stated: {p}");
    }

    /// What nemotron-3-nano:4b actually returned when the prompt rendered ids as
    /// `id=1`: the strings back, not the numbers. Refusing it loses a correct
    /// answer over a formatting slip.
    #[test]
    fn ids_echoed_back_as_strings_are_still_understood() {
        let r = extract_json(
            r#"{"tabs_to_close":["id=1","id=5","3"],"processes_to_suspend":["Discord"],
                "terminals_to_close":["226631"],"workspaces_to_sort":false,
                "estimated_ram_freed_mb":0,"summary":"x"}"#,
        )
        .unwrap();
        assert_eq!(r.tabs_to_close, vec![1, 5, 3]);
        assert_eq!(r.idle_terminals_to_close, vec![226631], "alias field accepted");
    }

    #[test]
    fn ids_given_as_floats_or_mixed_types_are_understood() {
        let r = extract_json(r#"{"tabs_to_close":[1,2.0,"3",null,{"a":1}]}"#).unwrap();
        assert_eq!(r.tabs_to_close, vec![1, 2, 3], "unusable entries are dropped");
    }

    /// A prompt is a cost. 120 tabs is already a lot of context for a 4B model,
    /// and the embedding ranker exists so the list does not have to be complete.
    #[test]
    fn the_tab_list_is_capped() {
        let tabs: Vec<(i64, i64, String, String)> = (0..300)
            .map(|i| (i, 1000, format!("https://x/{i}"), format!("t{i}")))
            .collect();
        let p = ctx("", &tabs).render();
        assert!(p.contains("119. inactive"), "the cap should be 120");
        assert!(!p.contains("150. inactive"), "beyond the cap must be omitted");
    }

    #[test]
    fn the_system_prompt_keeps_v1s_hard_won_rules() {
        for rule in [
            "ONLY name processes from the RECLAIMABLE list",
            "NEVER close: devtools://",
            "online documents or editors",
            "Prefer suspending nothing",
        ] {
            assert!(SYSTEM_PROMPT.contains(rule), "missing: {rule}");
        }
    }
}
