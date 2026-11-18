//! `/storage`: the storage warden over HTTP, and the flows the bubble and the
//! monitor share.
//!
//! | Route | Does |
//! |---|---|
//! | `GET /storage` | Free space per volume and the level. One cheap read. |
//! | `GET /storage/usage` | What is using the space. Scans unless a scan under 10 minutes old exists; `refresh=true` forces one, `seconds` sets its budget (1 to 120), `for=agent` leaves paths out. |
//! | `POST /storage/cleanup/propose` | Propose clearing `items` (ids from the scan; empty for all). Deletes nothing. Raises the warden's bubble so the user can approve. |
//! | `GET /storage/cleanup/{id}` | A proposal's state: pending, applied, declined, expired. |
//! | `POST /storage/cleanup/{id}/apply` | The user's approval. Deletes what the proposal lists. |
//! | `POST /storage/cleanup/{id}/decline` | The user said no. |
//!
//! `apply` is called by RamWarden's own apps when the user clicks, and by
//! `ramwarden storage free` after the user types yes. The tool server for
//! agents does not expose it: an agent proposes, a person approves.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use ramwarden_core::storage::{Alert, Level, Reading, size_text};
use serde::Deserialize;
use serde_json::{Value, json};

use super::cleanup::{Proposal, State as ProposalState};
use super::{FRESH, Refusal, Usage, alert_text, result_text, summary, volumes_view};
use crate::hub::AppState;
use crate::voice::{Pick, StorageSay, unix_now};

type Failure = (StatusCode, Json<Value>);

pub fn routes() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/storage", get(status))
        .route("/storage/usage", get(usage))
        .route("/storage/cleanup/propose", post(propose))
        .route("/storage/cleanup/{id}", get(proposal))
        .route("/storage/cleanup/{id}/apply", post(apply))
        .route("/storage/cleanup/{id}/decline", post(decline))
}

fn refused(r: Refusal) -> Failure {
    let code = match r {
        Refusal::UnknownProposal => StatusCode::NOT_FOUND,
        _ => StatusCode::CONFLICT,
    };
    (code, Json(json!({"ok": false, "error": r.text()})))
}

/// The last scan if it is fresh, else a new one, off the async threads.
pub async fn explain(st: &AppState, seconds: Option<u64>, force: bool) -> Arc<Usage> {
    let unix = unix_now();
    if !force && let Some(u) = st.storage.cached(FRESH, unix) {
        return u;
    }
    let storage = st.storage.clone();
    let seconds = seconds.unwrap_or(st.cfg.storage.scan_seconds);
    tokio::task::spawn_blocking(move || storage.scan(seconds, unix)).await.unwrap_or_default()
}

/// Free space now, read off the async threads (a sleeping disk can stall).
pub async fn volumes(st: &AppState) -> Vec<Reading> {
    let (storage, cfg, platform) = (st.storage.clone(), st.cfg.clone(), st.platform.clone());
    tokio::task::spawn_blocking(move || storage.refresh(&cfg.storage, &platform)).await.unwrap_or_default()
}

/// An alert is due: measure, prepare an offer, and baa. Called by the
/// monitor, which never waits for it.
pub async fn raise(st: AppState, alert: Alert) {
    let usage = explain(&st, None, false).await;
    let unix = unix_now();
    let proposal = st.storage.propose(&[], unix).ok().map(|p| (p.id, p.bytes));
    let say = StorageSay {
        text: alert_text(&alert, usage.clearable()),
        baa: Some(alert.volume.level == Level::Critical),
        proposal,
        explain: true,
    };
    let shown = st.voice.say_storage(&st.cfg.voice, say, Instant::now(), unix);
    tracing::warn!(
        "storage: {} is {:?} with {} free of {} ({})",
        alert.volume.name,
        alert.volume.level,
        size_text(alert.volume.free),
        size_text(alert.volume.total),
        if shown { "warden alerted" } else { "voice is off or muted" }
    );
}

/// The chat just answered a storage question: if something can be cleared,
/// put the offer in the bubble, silently. Not an alert, so no baa.
pub async fn offer(st: &AppState) {
    let unix = unix_now();
    let Ok(p) = st.storage.propose(&[], unix) else { return };
    let text = format!("I can clear {} of caches and temp files, nothing of yours. Shall I?", size_text(p.bytes));
    st.voice.say_storage(&st.cfg.voice, StorageSay { text, proposal: Some((p.id, p.bytes)), ..Default::default() }, Instant::now(), unix);
}

/// The user answered a storage bubble. Returns the reply to show.
pub async fn answered(st: &AppState, pick: Pick, proposal: Option<String>) -> Value {
    let unix = unix_now();
    match (pick, proposal) {
        // `/warden/answer` is served on every listener. Freeing space is
        // only ever approved from this machine.
        (Pick::Free, Some(_)) if !crate::guard::Policy::for_bind(&st.cfg.server.host, st.cfg.server.port).loopback => {
            json!({"ok": false, "reply": "Freeing space can only be approved on this computer.", "closed": false})
        }
        (Pick::Free, Some(id)) => {
            let (storage, cfg) = (st.storage.clone(), st.cfg.clone());
            let applied = tokio::task::spawn_blocking(move || storage.apply(&id, &cfg.storage, unix)).await;
            let reply = match applied {
                Ok(Ok((p, outcome))) => {
                    let skipped = if let ProposalState::Applied { skipped, .. } = p.state { skipped } else { 0 };
                    let free = volumes(st).await.iter().find(|v| v.system).map(|v| format!(" {} now has {} free.", v.name, size_text(v.free)));
                    format!("{}{}", result_text(&outcome, skipped), free.unwrap_or_default())
                }
                Ok(Err(r)) => r.text(),
                Err(_) => "The clean-up stopped unexpectedly. Ask me what is using the space to see where things stand.".into(),
            };
            st.voice.say_storage(&st.cfg.voice, StorageSay { text: reply.clone(), ..Default::default() }, Instant::now(), unix);
            json!({"ok": true, "reply": reply, "closed": false})
        }
        (Pick::Show, proposal) => {
            let usage = explain(st, None, false).await;
            let reply = summary(&volumes(st).await, &usage);
            let pending = proposal
                .and_then(|id| st.storage.proposal(&id, unix))
                .filter(|p| p.state == ProposalState::Pending)
                .map(|p| (p.id, p.bytes));
            st.voice.say_storage(&st.cfg.voice, StorageSay { text: reply.clone(), proposal: pending, ..Default::default() }, Instant::now(), unix);
            json!({"ok": true, "reply": reply, "closed": false})
        }
        (_, proposal) => {
            if let Some(id) = proposal {
                let _ = st.storage.decline(&id, unix);
            }
            json!({"ok": true, "reply": "", "closed": false})
        }
    }
}

async fn status(State(st): State<AppState>) -> Json<Value> {
    let readings = volumes(&st).await;
    let level = readings.iter().map(|r| r.level).max().unwrap_or(Level::Ok);
    Json(json!({
        "enabled": st.cfg.storage.enabled,
        "supported": !readings.is_empty(),
        "level": level,
        "volumes": volumes_view(&readings),
        "scanned_at": st.storage.cached(Duration::MAX, unix_now()).map(|u| u.at),
    }))
}

#[derive(Deserialize, Default)]
pub struct UsageQuery {
    #[serde(default)]
    refresh: bool,
    seconds: Option<u64>,
    /// `agent`: the form a model may see.
    #[serde(rename = "for")]
    audience: Option<String>,
}

async fn usage(State(st): State<AppState>, Query(q): Query<UsageQuery>) -> Json<Value> {
    let usage = explain(&st, q.seconds, q.refresh).await;
    let readings = volumes(&st).await;
    let paths = q.audience.as_deref() != Some("agent") || st.cfg.storage.share_paths;
    let mut v = usage.view(paths);
    v["volumes"] = json!(volumes_view(&readings));
    v["summary"] = json!(summary(&readings, &usage));
    Json(v)
}

fn proposal_view(p: &Proposal) -> Value {
    let mut v = serde_json::to_value(p).unwrap_or_default();
    v["size"] = json!(size_text(p.bytes));
    v["ok"] = json!(true);
    v
}

#[derive(Deserialize, Default)]
pub struct ProposeRequest {
    #[serde(default)]
    items: Vec<String>,
    /// Show the offer in the warden's bubble. On unless set to false.
    ask: Option<bool>,
}

async fn propose(State(st): State<AppState>, Json(r): Json<ProposeRequest>) -> Result<Json<Value>, Failure> {
    let unix = unix_now();
    // Sizes must exist before anything can be proposed.
    explain(&st, None, false).await;
    let items: Vec<String> = r.items.into_iter().filter(|i| i.len() <= 64).take(32).collect();
    let p = st.storage.propose(&items, unix).map_err(refused)?;
    let mut v = proposal_view(&p);
    let asked = r.ask.unwrap_or(true) && {
        let names: Vec<&str> = p.lines.iter().take(4).map(|l| l.label.as_str()).collect();
        let more = if p.lines.len() > names.len() { format!(" and {} more", p.lines.len() - names.len()) } else { String::new() };
        let text = format!("A clean-up is ready for your say-so: {} from {}{more}. Nothing of yours is touched.", size_text(p.bytes), names.join(", "));
        st.voice.say_storage(&st.cfg.voice, StorageSay { text, proposal: Some((p.id.clone(), p.bytes)), explain: true, baa: None }, Instant::now(), unix)
    };
    v["asked_in_bubble"] = json!(asked);
    v["approval"] = json!(if asked {
        "Waiting for the user. They approve with the Free button in the warden's bubble, or by running: ramwarden storage free"
    } else {
        "Waiting for the user. The warden's bubble is off or muted; they approve by running: ramwarden storage free"
    });
    Ok(Json(v))
}

async fn proposal(State(st): State<AppState>, Path(id): Path<String>) -> Result<Json<Value>, Failure> {
    st.storage.proposal(&id, unix_now()).map(|p| Json(proposal_view(&p))).ok_or_else(|| refused(Refusal::UnknownProposal))
}

async fn apply(State(st): State<AppState>, Path(id): Path<String>) -> Result<Json<Value>, Failure> {
    let unix = unix_now();
    let (storage, cfg) = (st.storage.clone(), st.cfg.clone());
    let (p, outcome) = tokio::task::spawn_blocking(move || storage.apply(&id, &cfg.storage, unix))
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": "the clean-up stopped unexpectedly"}))))?
        .map_err(refused)?;
    let skipped = if let ProposalState::Applied { skipped, .. } = p.state { skipped } else { 0 };
    let mut v = proposal_view(&p);
    v["ok"] = json!(!outcome.did_nothing());
    v["reply"] = json!(result_text(&outcome, skipped));
    v["freed"] = json!(size_text(outcome.bytes_freed));
    v["volumes"] = json!(volumes_view(&volumes(&st).await));
    Ok(Json(v))
}

async fn decline(State(st): State<AppState>, Path(id): Path<String>) -> Result<Json<Value>, Failure> {
    st.storage.decline(&id, unix_now()).map(|p| Json(proposal_view(&p))).map_err(refused)
}
