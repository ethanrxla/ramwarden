//! "Analyze now" in the browser extension: the warden reads your tabs and
//! memory and tells you what it makes of them, the way it does in chat.
//!
//! # What the model sees
//!
//! The same rule as [`crate::tabintel`]: features, never a URL, title or host.
//! Each tab is a numbered line (site type, idle time, status, estimated memory,
//! return chance, duplicate or related), and the model refers to tabs by
//! number. The numbers are turned back into titles here, on this machine, so
//! the reply reads naturally ("your 3 copies of "Inbox"") without a title ever
//! leaving it, even when the cloud model answers.
//!
//! # What it may suggest
//!
//! Only lists: which numbered tabs to unload and which to close. Nothing is
//! done. The popup shows the suggestions as buttons the user presses, which go
//! through `/browser/discard` and `/browser/close` and are re-checked there.
//! A suggestion for a tab that is not eligible (unload) or closeable (close) is
//! dropped before the user sees it, so the model can never widen what may be
//! touched.
//!
//! With no model reachable, the rules' own view is written in the warden's
//! voice instead and marked `"tier": "facts"`.

use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};

use crate::browser::{Row, Target};
use crate::hub::AppState;

/// At most this many tabs are described, biggest first.
const MAX_TABS: usize = 40;

pub const SYSTEM: &str = r##"You are Warden, RamWarden's pixel ram: a small, dry-humoured guardian of the user's memory.
The user pressed "Analyze now" in the browser extension. You receive measured memory figures and a numbered list
of their browser tabs, described by features only (no titles or addresses). Refer to a tab ONLY as #N, for example
"#4"; the app replaces #N with the tab's title on the user's machine.

Explain in under 90 words, in a friendly voice with at most one light joke, what is using memory in the browser
and what you would release. Separate measurement from estimate: per-tab memory (est_mb) is an estimate by site type,
not a measurement, so say "roughly". Unloading keeps a tab open and reloads it on click; closing removes it but it
stays in the reopen list. Prefer unloading. Suggest closing only for long-idle tabs with a low return chance, older
duplicates, or tabs outside the user's tab budget. Never suggest a tab whose status is "protected", "recent",
"retained" or "stale". Never claim you did anything. Do not use em dashes.

Reply with JSON only: {"reply": "...", "unload": [N, ...], "close": [N, ...]}. Lists may be empty."##;

fn class_text(class: &str) -> &'static str {
    match class {
        "feed" => "social feed or video",
        "app" => "web app (mail, chat, calendar)",
        "reference" => "code or reference",
        _ => "ordinary page",
    }
}

/// The tabs worth describing, biggest estimated memory first.
pub fn pick(rows: &[Row]) -> Vec<&Row> {
    let mut v: Vec<&Row> = rows.iter().collect();
    v.sort_by(|a, b| b.est_mb.cmp(&a.est_mb).then(b.inactive_minutes.cmp(&a.inactive_minutes)).then(a.target.id.cmp(&b.target.id)));
    v.truncate(MAX_TABS);
    v
}

/// The numbered, feature-only description sent to the model.
pub fn describe(tabs: &[&Row], percent: f64, used_mb: u64, total_mb: u64, keep_tabs: usize) -> Value {
    json!({
        "memory": {"percent_in_use": percent.round(), "used_mb": used_mb, "total_mb": total_mb},
        "tab_budget": keep_tabs,
        "tabs_open": tabs.len(),
        "tabs": tabs.iter().enumerate().map(|(i, r)| json!({
            "n": i + 1,
            "site_type": class_text(r.class),
            "idle": crate::voice::idle_text(r.inactive_minutes),
            "status": r.status,
            "est_mb": r.est_mb,
            "return_chance_percent": (r.p_return * 100.0).round(),
            "duplicate": r.duplicate,
            "same_site_as_current_work": r.related,
        })).collect::<Vec<_>>(),
    })
}

/// What the warden suggests, already filtered to what may be touched.
#[derive(Debug, Default, PartialEq)]
pub struct Verdict {
    pub reply: String,
    pub unload: Vec<(Target, String)>,
    pub close: Vec<(Target, String)>,
}

fn title(r: &Row) -> String {
    let t = r.title.trim();
    let t = if t.is_empty() { "this tab" } else { t };
    let short: String = t.chars().take(40).collect();
    if t.chars().count() > 40 { format!("\u{201c}{short}…\u{201d}") } else { format!("\u{201c}{short}\u{201d}") }
}

/// Put titles back where the model wrote `#N`. Longest numbers first, so `#12`
/// is never read as `#1` followed by a 2.
fn restore_titles(text: &str, tabs: &[&Row]) -> String {
    let mut out = text.replace('\u{2014}', ",");
    for n in (1..=tabs.len()).rev() {
        out = out.replace(&format!("#{n}"), &title(tabs[n - 1]));
    }
    out
}

/// Read the model's JSON. Anything malformed or out of range is dropped.
pub fn parse(raw: &str, tabs: &[&Row], max_close: usize) -> Option<Verdict> {
    let v: Value = serde_json::from_str(raw.trim().trim_start_matches("```json").trim_end_matches("```").trim()).ok()?;
    let reply = v["reply"].as_str().filter(|s| !s.trim().is_empty())?;
    let numbered = |key: &str, ok: &dyn Fn(&Row) -> bool, cap: usize| -> Vec<(Target, String)> {
        let mut seen = std::collections::HashSet::new();
        v[key].as_array().into_iter().flatten()
            .filter_map(|n| n.as_u64())
            .filter_map(|n| tabs.get((n as usize).checked_sub(1)?).copied())
            .filter(|r| ok(r) && seen.insert(r.target.clone()))
            .take(cap)
            .map(|r| (r.target.clone(), r.title.clone()))
            .collect()
    };
    let close = numbered("close", &|r| r.closeable && !matches!(r.status, "protected" | "stale"), max_close);
    let unload = numbered("unload", &|r| r.eligible, crate::browser::BATCH_LIMIT)
        .into_iter().filter(|(t, _)| !close.iter().any(|(c, _)| c == t)).collect();
    Some(Verdict { reply: restore_titles(reply, tabs), unload, close })
}

/// The rules' own view in the warden's voice, for when no model answers.
pub fn fallback(rows: &[Row], close: &[Target], percent: f64) -> Verdict {
    let eligible: Vec<&Row> = rows.iter().filter(|r| r.eligible && !close.contains(&r.target)).take(crate::browser::BATCH_LIMIT).collect();
    let closing: Vec<&Row> = rows.iter().filter(|r| close.contains(&r.target)).collect();
    let mb: u64 = eligible.iter().chain(&closing).map(|r| r.est_mb as u64).sum();
    let reply = if rows.is_empty() {
        "No browser is reporting tabs to me yet. Once the extension connects I'll have opinions.".to_string()
    } else if eligible.is_empty() && closing.is_empty() {
        format!("{} tabs, memory at {percent:.0}%, and nothing I'd release right now. Everything is recent, pinned or protected. Tidy herd.", rows.len())
    } else {
        format!(
            "{} tabs open, memory at {percent:.0}%. I'd unload {} and close {}, roughly {} back. My AI side is offline, so this is the rules talking.",
            rows.len(), eligible.len(), closing.len(), crate::voice::mb_text(mb)
        )
    };
    let pairs = |v: Vec<&Row>| v.into_iter().map(|r| (r.target.clone(), r.title.clone())).collect();
    Verdict { reply, unload: pairs(eligible), close: pairs(closing) }
}

fn targets_json(v: &[(Target, String)]) -> Value {
    json!(v.iter().map(|(t, title)| json!({"browser": t.browser, "id": t.id, "url": t.url, "title": title})).collect::<Vec<_>>())
}

/// `POST /browser/explain`.
pub async fn explain(State(st): State<AppState>) -> Json<Value> {
    st.request_all_tabs().await;
    let threshold = st.cfg.thresholds.inactivity_minutes as i64;
    let (rows, close, browsers) = {
        let hub = st.hub.lock().unwrap();
        let rows = crate::browser::snapshot(&hub.reg, threshold);
        let close = crate::browser::close_candidates(&rows, &st.cfg.tabs);
        (rows, close, hub.reg.browsers_connected())
    };
    let m = st.memory();
    let tabs = pick(&rows);
    let facts = describe(&tabs, m.percent(), m.used() / 1_048_576, m.total / 1_048_576, st.cfg.tabs.keep_tabs);
    let messages = [
        json!({"role": "system", "content": SYSTEM}),
        json!({"role": "user", "content": format!("Analyze now. Snapshot: {facts}")}),
    ];
    let cloud_allowed = st.ai.cloud_enabled || st.cfg.assistant.cloud_enabled;
    let answer = if rows.is_empty() { None } else { crate::assistant::ask_model(&st, &messages, cloud_allowed).await };
    let (verdict, tier) = answer
        .and_then(|(raw, tier)| parse(&raw, &tabs, st.cfg.tabs.max_close_per_sweep).map(|v| (v, tier)))
        .unwrap_or_else(|| (fallback(&rows, &close, m.percent()), "facts"));
    tracing::info!("analyze now ({tier}): {} unload, {} close suggested", verdict.unload.len(), verdict.close.len());
    Json(json!({
        "reply": verdict.reply,
        "tier": tier,
        "unload": targets_json(&verdict.unload),
        "close": targets_json(&verdict.close),
        // The same fields as /browser/analyze, for older popups.
        "browsers_connected": browsers,
        "candidates": rows.iter().filter(|r| r.eligible).count(),
        "tabs": rows,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, status: &'static str, eligible: bool, closeable: bool, mb: u32) -> Row {
        Row {
            target: Target { browser: "b".into(), id, url: format!("https://secret.example/{id}") },
            title: format!("Private title {id}"),
            inactive_minutes: 300,
            eligible,
            closeable,
            auto_closeable: closeable,
            work: false,
            status,
            reason: String::new(),
            priority: 0,
            p_return: 0.2,
            est_mb: mb,
            frecency: 0.0,
            related: false,
            class: "feed",
            duplicate: false,
            p_rules: 0.2,
            p_model: None,
        }
    }

    #[test]
    fn the_model_sees_features_and_numbers_never_titles_or_addresses() {
        let rows = vec![row(1, "candidate", true, true, 400)];
        let text = describe(&pick(&rows), 71.0, 1000, 2000, 6).to_string();
        assert!(text.contains("\"n\":1") && text.contains("social feed"));
        assert!(!text.contains("secret.example") && !text.contains("Private title"), "{text}");
    }

    #[test]
    fn numbers_become_titles_and_suggestions_are_limited_to_what_may_be_touched() {
        let rows = vec![
            row(1, "candidate", true, true, 400),
            row(2, "protected", false, false, 900),
            row(3, "recent", false, true, 100),
        ];
        let tabs = pick(&rows); // 2, 1, 3 by size
        let raw = r##"{"reply":"#2 is huge but #1 can go. Also #3.","unload":[1,2,2,9,0],"close":[2,3,1]}"##;
        let v = parse(raw, &tabs, 10).unwrap();
        assert!(v.reply.contains("Private title 2") && v.reply.contains("Private title 1"), "{}", v.reply);
        assert!(!v.reply.contains('#'));
        let ids = |l: &[(Target, String)]| l.iter().map(|(t, _)| t.id).collect::<Vec<_>>();
        assert_eq!(ids(&v.close), [1, 3], "#1 is the protected tab 2, dropped");
        assert!(ids(&v.unload).is_empty(), "tab 1 is being closed; tab 2 is not eligible");
    }

    #[test]
    fn a_malformed_or_empty_answer_falls_back_to_the_rules() {
        let rows = vec![row(1, "candidate", true, true, 400)];
        let tabs = pick(&rows);
        assert!(parse("not json", &tabs, 10).is_none());
        assert!(parse(r#"{"reply":"  "}"#, &tabs, 10).is_none());
        let f = fallback(&rows, &[], 80.0);
        assert_eq!(f.unload.len(), 1);
        assert!(f.reply.contains("unload 1") && f.reply.contains("roughly 400 MB"), "{}", f.reply);
        assert!(fallback(&[], &[], 50.0).reply.contains("No browser"));
    }

    #[test]
    fn the_prompt_follows_the_house_style() {
        assert!(!SYSTEM.contains('\u{2014}'));
        assert!(SYSTEM.contains("Never claim you did anything"));
    }
}
