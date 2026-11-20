//! Host-side speech: no key is ever sent to the Mac UI or stored in its bundle.
use std::{collections::HashMap, sync::{Mutex, OnceLock}, time::Duration};
use axum::{Json, extract::State, http::{StatusCode, header}, response::{IntoResponse, Response}};
use serde::Deserialize;
use serde_json::json;
use crate::hub::AppState;
use ramwarden_core::config::integration_secret;

pub const RAM_VOICE: &str = "ib8aaABAPZQpTo6hx8Jr";
pub const THREAT_VOICE: &str = "DMyrgzQFny3JI1Y1paM5";
#[derive(Default)]
struct Usage { day: u64, minute: u64, calls: u32, chars: usize, cache: HashMap<(String,String),Vec<u8>> }
static USAGE: OnceLock<Mutex<Usage>> = OnceLock::new();
static GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
#[derive(Deserialize)]
pub struct Request { text: String, warden: String }

type Refusal = (StatusCode, Json<serde_json::Value>);

fn refuse(status: StatusCode, why: &str) -> Refusal {
    (status, Json(json!({"error": why})))
}

/// Give back the budget a call that produced no audio took.
fn refund(text: &str) {
    if let Some(m) = USAGE.get() {
        let mut usage = m.lock().unwrap();
        usage.calls = usage.calls.saturating_sub(1);
        usage.chars = usage.chars.saturating_sub(text.chars().count());
    }
}

/// The status the UI gets for an ElevenLabs refusal. ElevenLabs reports a
/// used-up quota as 401 with `quota_exceeded`; that is a payment matter, not a
/// rejected key, so it goes out as 402 like the spec says.
fn map_status(upstream: u16, code: &str) -> StatusCode {
    match (upstream, code) {
        (_, "quota_exceeded") | (402, _) => StatusCode::PAYMENT_REQUIRED,
        (401 | 403, _) => StatusCode::UNAUTHORIZED,
        (429, _) => StatusCode::TOO_MANY_REQUESTS,
        _ => StatusCode::BAD_GATEWAY,
    }
}

/// What to tell the user, from ElevenLabs' status and its `detail.status`.
pub fn explain(upstream: u16, code: &str) -> &'static str {
    match (upstream, code) {
        (_, "quota_exceeded") => "ElevenLabs says this key's character quota is used up.",
        (_, "detected_unusual_activity") => "ElevenLabs paused free-tier use for this key (unusual activity). A paid plan or a new key fixes it.",
        (_, "voice_not_found") | (404, _) => "ElevenLabs can't find the warden's voice on this account. Add the voice in ElevenLabs' Voice Library, or set ELEVENLABS_RAM_VOICE_ID.",
        (_, "invalid_api_key") | (401, _) => "ElevenLabs rejected the API key. Check ELEVENLABS_API_KEY.",
        (_, "missing_permissions") | (403, _) => "This ElevenLabs key lacks text-to-speech permission. Enable it on the key in ElevenLabs.",
        (402, _) => "ElevenLabs needs a paid plan for this voice.",
        (429, _) => "ElevenLabs is rate limiting this key. Try again in a minute.",
        (400 | 422, _) => "ElevenLabs refused the request (voice or model setting). Check ELEVENLABS_MODEL_ID and the voice ID.",
        _ => "ElevenLabs had a server problem. Try again shortly.",
    }
}

pub async fn speak(State(st): State<AppState>, Json(request): Json<Request>) -> Result<Response, Refusal> {
    if !matches!(request.warden.as_str(),"ram"|"threat") || request.text.trim().is_empty() || request.text.chars().count()>800 {
        return Err(refuse(StatusCode::UNPROCESSABLE_ENTITY,"Nothing to say."));
    }
    if !st.cfg.voice.enabled || (request.warden=="ram" && st.voice.mute_state(super::unix_now()).0) { return Err(refuse(StatusCode::FORBIDDEN,"The warden's voice is off or muted.")); }
    let key = integration_secret(&st.cfg,"ELEVENLABS_API_KEY").ok_or_else(||refuse(StatusCode::SERVICE_UNAVAILABLE,"No ElevenLabs key is set (ELEVENLABS_API_KEY)."))?;
    let voice = integration_secret(&st.cfg,if request.warden=="ram" {"ELEVENLABS_RAM_VOICE_ID"} else {"ELEVENLABS_THREAT_VOICE_ID"})
        .unwrap_or_else(||if request.warden=="ram" {RAM_VOICE.into()} else {THREAT_VOICE.into()});
    if voice.len()>64 || !voice.bytes().all(|b|b.is_ascii_alphanumeric()) { return Err(refuse(StatusCode::SERVICE_UNAVAILABLE,"The ElevenLabs voice ID setting is not valid.")); }
    let model = integration_secret(&st.cfg,"ELEVENLABS_MODEL_ID").unwrap_or_else(||"eleven_flash_v2_5".into());
    let cache_key = (format!("{voice}:{model}"),request.text.clone());
    let now = super::unix_now();
    {
        let mut usage = USAGE.get_or_init(||Mutex::new(Usage::default())).lock().unwrap();
        if usage.day != now/86400 { usage.day=now/86400; usage.chars=0; usage.cache.clear(); }
        if let Some(bytes) = usage.cache.get(&cache_key) { return Ok(([(header::CONTENT_TYPE,"audio/mpeg")],bytes.clone()).into_response()); }
        if usage.minute != now/60 { usage.minute=now/60; usage.calls=0; }
        if usage.calls>=6 || usage.chars+request.text.chars().count()>20_000 { return Err(refuse(StatusCode::TOO_MANY_REQUESTS,"The warden has spoken a lot just now; voice is resting. Text chat still works.")); }
        usage.calls+=1; usage.chars+=request.text.chars().count();
    }
    let _permit = GATE.try_acquire().map_err(|_|refuse(StatusCode::TOO_MANY_REQUESTS,"The warden is already speaking."))?;
    let client = reqwest::Client::builder().timeout(Duration::from_secs(25)).build().map_err(|_|refuse(StatusCode::BAD_GATEWAY,"Spoken voice could not start."))?;
    let body = json!({"text":request.text,"model_id":model,
        "voice_settings":{"stability":if request.warden=="ram" {0.45} else {0.65},"similarity_boost":0.75}});
    // One retry for ElevenLabs' own hiccups (a 5xx or a dropped connection);
    // anything it refuses on purpose is reported straight away.
    let mut attempt = 0;
    let response = loop {
        attempt += 1;
        let sent = client.post(format!("https://api.elevenlabs.io/v1/text-to-speech/{voice}?output_format=mp3_44100_128"))
            .header("xi-api-key",&key).json(&body).send().await;
        match sent {
            Ok(r) if r.status().is_server_error() && attempt == 1 => {
                tracing::warn!(status = %r.status(), "ElevenLabs had a server error; retrying once");
                tokio::time::sleep(Duration::from_millis(600)).await;
            }
            Err(e) if attempt == 1 && !e.is_timeout() => {
                tracing::warn!(error = %e, "could not reach ElevenLabs; retrying once");
                tokio::time::sleep(Duration::from_millis(600)).await;
            }
            Ok(r) => break r,
            Err(e) => {
                refund(&request.text);
                tracing::warn!(error = %e, timeout = e.is_timeout(), "could not reach ElevenLabs");
                return Err(refuse(StatusCode::BAD_GATEWAY, if e.is_timeout() {
                    "ElevenLabs did not answer in time. Text chat still works."
                } else {
                    "Could not reach ElevenLabs from this computer (network, firewall or proxy). Text chat still works."
                }));
            }
        }
    };
    let status = response.status();
    if !status.is_success() {
        refund(&request.text);
        // ElevenLabs explains a refusal as {"detail":{"status":"...","message":"..."}}.
        // Its message never echoes the key, so it is safe to log.
        let detail: serde_json::Value = response.json().await.unwrap_or_default();
        let code = detail["detail"]["status"].as_str().or_else(|| detail["detail"]["code"].as_str()).unwrap_or("").to_string();
        let message: String = detail["detail"]["message"].as_str().or_else(|| detail["detail"].as_str()).unwrap_or("").chars().take(300).collect();
        tracing::warn!(status = status.as_u16(), code = %code, message = %message, voice = %voice, model = %model, "ElevenLabs refused speech");
        return Err(refuse(map_status(status.as_u16(), &code), explain(status.as_u16(), &code)));
    }
    if response.content_length().is_some_and(|n|n>2_000_000) { return Err(refuse(StatusCode::BAD_GATEWAY,"ElevenLabs sent more audio than expected.")); }
    let bytes = response.bytes().await.map_err(|e|{ tracing::warn!(error = %e, "ElevenLabs audio was cut off"); refuse(StatusCode::BAD_GATEWAY,"The voice audio was cut off. Try again.") })?.to_vec();
    if bytes.len()>2_000_000 { return Err(refuse(StatusCode::BAD_GATEWAY,"ElevenLabs sent more audio than expected.")); }
    let mut usage = USAGE.get().unwrap().lock().unwrap();
    if usage.cache.len()>=8 { usage.cache.clear(); }
    usage.cache.insert(cache_key,bytes.clone());
    Ok(([(header::CONTENT_TYPE,"audio/mpeg")],bytes).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_name_their_cause() {
        assert!(explain(401, "detected_unusual_activity").contains("unusual activity"));
        assert!(explain(401, "quota_exceeded").contains("quota"));
        assert!(explain(401, "invalid_api_key").contains("rejected the API key"));
        assert!(explain(404, "").contains("voice"));
        assert!(explain(503, "").contains("server problem"));
        assert_eq!(map_status(401, "invalid_api_key"), StatusCode::UNAUTHORIZED);
        assert_eq!(map_status(401, "quota_exceeded"), StatusCode::PAYMENT_REQUIRED);
        assert_eq!(map_status(500, ""), StatusCode::BAD_GATEWAY);
    }
}
