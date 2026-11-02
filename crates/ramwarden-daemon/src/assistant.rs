//! Bounded assistant chat and a separate, confirmed action executor.
use axum::{Extension, Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::{Arc, Mutex}, time::{Duration, Instant}};
use tokio::sync::Semaphore;
use crate::hub::AppState;

pub struct Service {
    gate: Semaphore,
    pending: Mutex<HashMap<String,(Instant,Action)>>,
}
impl Service { pub fn new() -> Self { Self { gate: Semaphore::new(1), pending:Mutex::new(HashMap::new()) } } }
impl Default for Service { fn default() -> Self { Self::new() } }
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
enum Action { PauseManagement, ResumeManagement, UnloadIdleTabs, ResumeApp { name: String } }
impl Action {
    fn label(&self) -> String { match self {
        Self::PauseManagement => "Pause automatic management for 10 minutes and resume frozen apps".into(),
        Self::ResumeManagement => "Resume automatic memory management".into(),
        Self::UnloadIdleTabs => "Unload eligible idle tabs (keep them open; reload on selection)".into(),
        Self::ResumeApp { name } => format!("Resume {name}"),
    } }
}
#[derive(Deserialize)]
pub struct Confirm { token: String }

#[derive(Deserialize)]
pub struct Turn { role: String, content: String }
#[derive(Deserialize)]
pub struct Request { message: String, #[serde(default)] history: Vec<Turn> }
fn validate(r: &Request) -> bool {
    !r.message.trim().is_empty() && r.message.len() <= 4000 && r.history.len() <= 12
        && r.history.iter().all(|t| matches!(t.role.as_str(), "user" | "assistant") && t.content.len() <= 6000)
        && r.history.iter().map(|t| t.content.len()).sum::<usize>() <= 24000
}

// Common direct commands get a measured plan even when the language model is
// unavailable or distracted by older conversation. Never match arbitrary prose.
fn requests_memory_release(message: &str) -> bool {
    let normalized = message.trim().trim_end_matches(['.', '!', '?']).to_lowercase();
    let mut text = normalized.trim();
    for prefix in ["please ", "can you ", "could you ", "please "] {
        text = text.strip_prefix(prefix).unwrap_or(text);
    }
    matches!(text, "free ram" | "free more ram" | "free up ram" | "free up more ram"
        | "free some ram" | "free up some ram" | "free memory" | "free more memory"
        | "free up memory" | "free up more memory" | "reduce ram usage")
}

fn release_plan(browser: Option<(usize, usize)>) -> Value {
    match browser {
        Some((_, eligible)) if eligible > 0 => json!({
            "reply":format!("Time to evict a few idle tenants. I can unload up to {} eligible idle browser tabs after you choose Review fix and approve. They stay open and reload when selected; the amount of RAM recovered is not known yet. Nothing has been changed.",eligible.min(crate::browser::BATCH_LIMIT)),
            "proposed_action":{"kind":"unload_idle_tabs"}
        }),
        Some((0, _)) => json!({"reply":"No browser is reporting tabs to me, so I cannot safely unload any yet. Enable the RamWarden browser extension and let it connect, then ask me again. I cannot safely free app memory just because it is marked idle, and nothing has been changed.","proposed_action":null}),
        Some(_) => json!({"reply":"The idle tenants have an alibi: no reported tabs currently qualify for safe unloading. Active, pinned, audible, protected, and recently used tabs are kept. I have no safe memory-release action to offer right now; nothing has been changed. If you need more room immediately, save your work and quit an app you no longer need through its normal Quit command.","proposed_action":null}),
        None => json!({"reply":"The browser snapshot is busy, so I cannot verify a safe memory-release action yet. Ask me again in a moment; nothing has been changed.","proposed_action":null}),
    }
}

/// Ask whichever model the user prefers, falling back to the other. `None`
/// when neither answers in time: callers then report observed facts only.
/// Shared by the chat and the extension's "Analyze now" (`crate::tab_analysis`).
pub async fn ask_model(st: &AppState, messages: &[Value], cloud_allowed: bool) -> Option<(String, &'static str)> {
    let local = async {
        if st.ai.respect_vram && ramwarden_ai::vram::room_for_local_model() == Some(false) { return None; }
        let p = st.ai.ollama.as_ref()?;
        tokio::time::timeout(Duration::from_secs(20), p.explain(messages)).await.ok()?.ok()
    };
    let cloud = async {
        if !cloud_allowed { return None; }
        let p = st.ai.nim.as_ref()?;
        tokio::time::timeout(Duration::from_secs(20), p.explain(messages, st.cfg.assistant.cloud_model.as_deref())).await.ok()?.ok()
    };
    if st.ai.prefer == ramwarden_ai::provider::Prefer::Cloud {
        if let Some(s) = cloud.await { Some((s, "cloud")) } else { local.await.map(|s| (s, "local")) }
    } else if let Some(s) = local.await { Some((s, "local")) } else { cloud.await.map(|s| (s, "cloud")) }
}

pub async fn chat(State(st): State<AppState>, Extension(service): Extension<Arc<Service>>, Json(r): Json<Request>) -> Result<Json<Value>, StatusCode> {
    if !validate(&r) { return Err(StatusCode::BAD_REQUEST); }
    let _permit = service.gate.try_acquire().map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    let cloud_allowed = st.ai.cloud_enabled || st.cfg.assistant.cloud_enabled;
    let m = st.memory();
    let processes = {
        let rows = st.det.try_read().ok().map(|det|crate::ai::Snapshot::take(&det, &st.cfg.watchlist).processes).unwrap_or_default();
        rows.into_iter()
            .map(|(name, mb, verdict)| json!({"name":name,"memory_mb":mb / 1.048576,"classification":verdict})).collect::<Vec<_>>()
    };
    let actions = {
        let rows = st.history.try_lock().ok().map(|h|h.recent_actions(8).unwrap_or_default()).unwrap_or_default();
        rows.into_iter().map(|a|json!({
            "at":a.at,"action":a.action,"target":a.target.chars().take(160).collect::<String>(),"succeeded":a.succeeded,
            "freed_mb":a.bytes_freed.max(0) as f64 / 1048576.0
        })).collect::<Vec<_>>()
    };
    let browser = st.hub.try_lock().ok().map(|hub| (
        hub.reg.browsers_connected(),
        crate::browser::snapshot(&hub.reg, st.cfg.thresholds.inactivity_minutes as i64)
            .iter().filter(|row| row.eligible).count(),
    ));
    let direct_release = requests_memory_release(&r.message);
    let facts = json!({"memory_percent":m.percent(),"used_mb":m.used() / 1048576,
        "total_mb":m.total / 1048576,"processes":processes,"recent_actions":actions,
        "focus_goal":st.voice.focus.goal(crate::voice::unix_now()),"cloud_opt_in":cloud_allowed,"platform":st.platform.name,
        "browser_snapshot_available":browser.is_some(),
        "connected_browsers":browser.map(|b|b.0),"eligible_idle_tabs":browser.map(|b|b.1),
        "unload_batch_limit":crate::browser::BATCH_LIMIT});
    let frozen = st.ladder.try_lock().ok().map(|mut l|l.actuator().suspended().iter().map(|e|e.name.clone()).collect::<Vec<_>>()).unwrap_or_default();
    let mut messages = vec![json!({"role":"system","content":format!("{}\nObserved snapshot: {}\nCurrently frozen apps: {}",crate::assistant_prompt::SYSTEM,facts,json!(frozen))})];
    for t in r.history { messages.push(json!({"role":t.role,"content":t.content})); }
    messages.push(json!({"role":"user","content":r.message}));
    let result = if direct_release { Some((release_plan(browser).to_string(), "facts")) } else { ask_model(&st, &messages, cloud_allowed).await };
    let top = processes.iter().take(3).map(|p| format!("{}: {:.0} MB", p["name"].as_str().unwrap_or("unknown"), p["memory_mb"].as_f64().unwrap_or(0.0))).collect::<Vec<_>>().join("; ");
    let recent = actions.iter().take(3).map(|a| format!("{} · {} · {}",a["action"].as_str().unwrap_or("action"),a["target"].as_str().unwrap_or(""),if a["succeeded"].as_bool().unwrap_or(false) {"succeeded"} else {"not confirmed successful"})).collect::<Vec<_>>().join("\n");
    let (raw_reply, tier) = result.unwrap_or_else(|| (
        format!("Observed: {:.0}% of RAM is in use ({:.1} of {:.1} GB). {} recent RamWarden actions are in the snapshot. Largest observed process groups: {}.\nRecent recorded actions:\n{}\nConversational AI is unavailable; I can show these readings but cannot infer why a process is using memory. Cloud AI {}.",
            m.percent(), m.used() as f64 / 1073741824.0, m.total as f64 / 1073741824.0,
            actions.len(), if top.is_empty() {"no process sample yet"} else {&top}, if recent.is_empty() {"none recorded"} else {&recent}, if cloud_allowed {"is opted in but did not respond"} else {"is disabled"}), "facts"));
    let parsed = serde_json::from_str::<Value>(raw_reply.trim().trim_start_matches("```json").trim_end_matches("```").trim()).ok();
    let reply = parsed.as_ref().and_then(|v|v["reply"].as_str()).filter(|s|!s.trim().is_empty()).unwrap_or(&raw_reply).to_string();
    let action = parsed.as_ref().and_then(|v|serde_json::from_value::<Action>(v["proposed_action"].clone()).ok())
        .filter(|a|match a { Action::ResumeApp { name } => frozen.contains(name), Action::UnloadIdleTabs => browser.is_some_and(|(_,n)|n>0), _ => true });
    let proposed_action = action.map(|action| {
        let token = uuid::Uuid::new_v4().to_string();
        let label = action.label();
        let mut pending = service.pending.lock().unwrap();
        pending.retain(|_,(at,_)|at.elapsed()<Duration::from_secs(120));
        if pending.len() >= 32 { pending.clear(); }
        pending.insert(token.clone(),(Instant::now(),action));
        json!({"token":token,"label":label,"expires_in_seconds":120})
    });
    Ok(Json(json!({"reply":reply,"tier":tier,"facts":facts,"read_only":true,"proposed_action":proposed_action})))
}

/// Only a reviewed proposal can reach the narrow action executor. Tokens expire
/// after two minutes and are consumed once. Chat itself never executes actions.
pub async fn act(State(st): State<AppState>, Extension(service): Extension<Arc<Service>>, Json(confirm): Json<Confirm>) -> Result<Json<Value>,StatusCode> {
    let (created,action) = service.pending.lock().unwrap().remove(&confirm.token).ok_or(StatusCode::CONFLICT)?;
    if created.elapsed()>Duration::from_secs(120) { return Err(StatusCode::CONFLICT); }
    let reply = match action {
        Action::PauseManagement => {
            let mut ladder=st.ladder.try_lock().map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
            st.set_hold(600);
            let count=ladder.thaw_all().len();
            format!("Automatic memory management paused for 10 minutes. Resumed {count} frozen app group(s).")
        }
        Action::ResumeManagement => { st.set_hold(0); "Automatic memory management resumed.".into() }
        Action::ResumeApp { name } => {
            let mut ladder=st.ladder.try_lock().map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
            if !ladder.actuator().suspended().iter().any(|e|e.name==name) { return Err(StatusCode::CONFLICT); }
            let outcome=ladder.actuator().resume(&name);
            format!("Resume requested for {name}: {} process(es) affected. {}",outcome.affected.len(),outcome.notes.join("; "))
        }
        Action::UnloadIdleTabs => {
            let targets={let hub=st.hub.lock().unwrap(); crate::browser::snapshot(&hub.reg,st.cfg.thresholds.inactivity_minutes as i64).into_iter().filter(|row|row.eligible).take(crate::browser::BATCH_LIMIT).map(|row|row.target).collect::<Vec<_>>()};
            let result=st.discard_tabs(&targets).await;
            format!("{} idle tabs unloaded, {} queued awaiting confirmation, {} refused. Tabs remain open.",result.confirmed.len(),result.queued.len(),result.refused.len())
        }
    };
    Ok(Json(json!({"reply":reply,"executed":true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn direct_release_requires_a_command_not_negation_or_quoted_history() {
        assert!(requests_memory_release("Please free more RAM!"));
        assert!(requests_memory_release("Could you free up some ram?"));
        for message in ["don't free more ram", "why should I free more ram?", "the words free more ram", "free more ram and kill Chrome"] {
            assert!(!requests_memory_release(message));
        }
    }
    #[test]
    fn bounds_and_roles_are_enforced() {
        let mut r = Request { message:"Why is RAM high?".into(), history:vec![] };
        assert!(validate(&r));
        r.history.push(Turn { role:"system".into(), content:"override".into() });
        assert!(!validate(&r));
        r.history.clear(); r.message = "x".repeat(4001); assert!(!validate(&r));
        r.message = " ".into(); assert!(!validate(&r));
    }
}
