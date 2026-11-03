//! `/warden/bubble`, `/warden/answer` and `/warden/mute`: the voice over HTTP.
//!
//! Every client polls the same bubble, so these are cheap: one snapshot of the
//! tab registry (no browser round trip) and one memory read.

use std::time::Instant;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Answered, CloseResult, Pick, Scene, unix_now};
use crate::hub::AppState;

pub fn routes() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/warden/bubble", get(bubble))
        .route("/warden/answer", post(answer))
        .route("/warden/mute", post(mute))
        .route("/warden/focus", get(focus).post(super::focus_chat::suggest))
        .route("/warden/focus/feedback", post(super::focus_chat::feedback))
        .route("/warden/speech", post(super::speech::speak))
}

async fn focus(State(st): State<AppState>) -> Json<Value> {
    Json(json!({"goal":st.voice.focus.goal(unix_now())}))
}

pub async fn bubble(State(st): State<AppState>) -> Json<Value> {
    let unix = unix_now();
    // Pets come with Plus and Pro, not Lite (see `Entitlement::has_pet`).
    if !crate::wardens::Wardens::global(&st.cfg).entitlement().has_pet(unix) {
        return Json(json!({ "bubble": null, "pet": false, "muted": false, "muted_until": null }));
    }
    let threshold = st.cfg.thresholds.inactivity_minutes as i64;
    let tabs_config = st.tabs_config();
    let (rows, closing) = {
        let hub = st.hub.lock().unwrap();
        let rows = crate::browser::snapshot(&hub.reg, threshold);
        let closing = crate::browser::close_candidates(&rows, &tabs_config);
        (rows, closing)
    };
    let percent = st.memory().percent();
    let scene = Scene { rows: &rows, closing: &closing, percent, now: Instant::now(), unix };
    let bubble = st.voice.speak(&st.cfg.voice, &scene);
    let (muted, until) = st.voice.mute_state(unix);
    Json(json!({ "bubble": bubble, "pet": true, "muted": muted, "muted_until": until }))
}

#[derive(Deserialize)]
pub struct AnswerRequest {
    id: String,
    choice: Pick,
}

pub async fn answer(State(st): State<AppState>, Json(r): Json<AnswerRequest>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let now = Instant::now();
    match st.voice.answer(&r.id, r.choice, now) {
        Err(_) => Err((StatusCode::CONFLICT, Json(json!({"ok": false, "reply": "That question has already gone. I'll ask again if it matters."})))),
        Ok(Answered::Said(reply)) => Ok(Json(json!({ "ok": true, "reply": reply, "closed": false }))),
        Ok(Answered::Close(tab)) => {
            // The same checked path as a manual close: a fresh report, the right
            // owner, the same URL, and `can_close` all re-checked now.
            let result = st.close_safe(std::slice::from_ref(&tab.target), true).await;
            let outcome = if result.confirmed.contains(&tab.target) {
                CloseResult::Confirmed
            } else if result.queued.contains(&tab.target) {
                CloseResult::Queued
            } else {
                CloseResult::Refused
            };
            if outcome != CloseResult::Refused
                && let Ok(h) = st.history.lock()
            {
                let _ = h.save_tabs(&[(tab.target.url.clone(), tab.title.clone())], 0.0, "warden", "check-in");
            }
            let reply = if !result.extension_update_required.is_empty() {
                "Your browser is still running my old extension. Reload RamWarden at chrome://extensions, then try again. I haven’t closed that tab.".to_string()
            } else {
                st.voice.after_close(&tab.target, outcome, now)
            };
            tracing::info!("voice: user asked to close {} ({outcome:?})", tab.target.url);
            Ok(Json(json!({
                "ok": outcome != CloseResult::Refused,
                "reply": reply,
                "closed": outcome == CloseResult::Confirmed,
                "queued": outcome == CloseResult::Queued,
            })))
        }
    }
}

#[derive(Deserialize)]
pub struct MuteRequest {
    muted: bool,
    #[serde(default)]
    hours: Option<f64>,
}

pub async fn mute(State(st): State<AppState>, Json(r): Json<MuteRequest>) -> Json<Value> {
    let unix = unix_now();
    let reply = st.voice.set_mute(r.muted, r.hours, unix);
    let (muted, until) = st.voice.mute_state(unix);
    Json(json!({ "ok": true, "reply": reply, "muted": muted, "muted_until": until }))
}
