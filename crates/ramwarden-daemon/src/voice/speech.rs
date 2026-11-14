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

pub async fn speak(State(st): State<AppState>, Json(request): Json<Request>) -> Result<Response, StatusCode> {
    if !matches!(request.warden.as_str(),"ram"|"threat") || request.text.trim().is_empty() || request.text.chars().count()>800 {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    if !st.cfg.voice.enabled || (request.warden=="ram" && st.voice.mute_state(super::unix_now()).0) { return Err(StatusCode::FORBIDDEN); }
    let key = integration_secret(&st.cfg,"ELEVENLABS_API_KEY").ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let voice = integration_secret(&st.cfg,if request.warden=="ram" {"ELEVENLABS_RAM_VOICE_ID"} else {"ELEVENLABS_THREAT_VOICE_ID"})
        .unwrap_or_else(||if request.warden=="ram" {RAM_VOICE.into()} else {THREAT_VOICE.into()});
    if voice.len()>64 || !voice.bytes().all(|b|b.is_ascii_alphanumeric()) { return Err(StatusCode::SERVICE_UNAVAILABLE); }
    let model = integration_secret(&st.cfg,"ELEVENLABS_MODEL_ID").unwrap_or_else(||"eleven_flash_v2_5".into());
    let cache_key = (format!("{voice}:{model}"),request.text.clone());
    let now = super::unix_now();
    {
        let mut usage = USAGE.get_or_init(||Mutex::new(Usage::default())).lock().unwrap();
        if usage.day != now/86400 { usage.day=now/86400; usage.chars=0; usage.cache.clear(); }
        if let Some(bytes) = usage.cache.get(&cache_key) { return Ok(([(header::CONTENT_TYPE,"audio/mpeg")],bytes.clone()).into_response()); }
        if usage.minute != now/60 { usage.minute=now/60; usage.calls=0; }
        if usage.calls>=6 || usage.chars+request.text.chars().count()>20_000 { return Err(StatusCode::TOO_MANY_REQUESTS); }
        usage.calls+=1; usage.chars+=request.text.chars().count();
    }
    let _permit = GATE.try_acquire().map_err(|_|StatusCode::TOO_MANY_REQUESTS)?;
    let client = reqwest::Client::builder().timeout(Duration::from_secs(25)).build().map_err(|_|StatusCode::BAD_GATEWAY)?;
    let response = client.post(format!("https://api.elevenlabs.io/v1/text-to-speech/{voice}?output_format=mp3_44100_128"))
        .header("xi-api-key",key).json(&json!({"text":request.text,"model_id":model,
            "voice_settings":{"stability":if request.warden=="ram" {0.45} else {0.65},"similarity_boost":0.75}}))
        .send().await.map_err(|_|StatusCode::BAD_GATEWAY)?;
    if response.status()==StatusCode::PAYMENT_REQUIRED { return Err(StatusCode::PAYMENT_REQUIRED); }
    if !response.status().is_success() || response.content_length().is_some_and(|n|n>2_000_000) { return Err(StatusCode::BAD_GATEWAY); }
    let bytes = response.bytes().await.map_err(|_|StatusCode::BAD_GATEWAY)?.to_vec();
    if bytes.len()>2_000_000 { return Err(StatusCode::BAD_GATEWAY); }
    let mut usage = USAGE.get().unwrap().lock().unwrap();
    if usage.cache.len()>=8 { usage.cache.clear(); }
    usage.cache.insert(cache_key,bytes.clone());
    Ok(([(header::CONTENT_TYPE,"audio/mpeg")],bytes).into_response())
}
