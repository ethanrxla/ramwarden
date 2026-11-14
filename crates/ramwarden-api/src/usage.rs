//! Only an enrolled device can submit numeric diagnostics for its account.
#![allow(clippy::result_large_err)]
use crate::{
    AppState,
    accounts::{Reply, cloud, device, session, storage},
    routes::error,
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, Method, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use ramwarden_cloud::{
    model::Account,
    usage::{DeviceConsent,UsageDay, UsageRecord},
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/v1/usage/config", get(config).post(set_config))
        .route("/v1/usage/daily", post(upload))
        .route("/v1/usage", get(list))
}
fn consent(a: &Account) -> Option<u64> {
    (a.prefs.get("share_usage").and_then(|v| v.as_bool()) == Some(true))
        .then(|| a.prefs.get("usage_since").and_then(|v| v.as_u64()))
        .flatten()
}
async fn config(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Reply {
    let c = cloud(&state)?;
    let (d, a) = device(c, &headers, (state.now)()).await?;
    let since = device_consent(c,&a,&d.id).await?;
    Ok(
        Json(json!({"enabled":since.is_some(),"since":since,"device_id":d.id,"retention_days":90}))
            .into_response(),
    )
}
async fn device_consent(c:&crate::accounts::Cloud,a:&Account,id:&str)->Result<Option<u64>,axum::response::Response> {
    let choice=c.store.device_consent(&a.id,id).await.map_err(storage)?;
    Ok(choice.map(|choice|choice.enabled.then_some(choice.since)).unwrap_or_else(||consent(a)))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Choice {enabled:bool}
async fn set_config(State(state):State<Arc<AppState>>,headers:HeaderMap,Json(choice):Json<Choice>)->Reply {
    let c=cloud(&state)?;let now=(state.now)();let(d,a)=device(c,&headers,now).await?;
    let old=device_consent(c,&a,&d.id).await?;
    let since=if choice.enabled {old.unwrap_or(now)}else{now};
    c.store.set_device_consent(DeviceConsent{account_id:a.id,device_id:d.id.clone(),enabled:choice.enabled,since}).await.map_err(storage)?;
    Ok(Json(json!({"enabled":choice.enabled,"since":choice.enabled.then_some(since),"device_id":d.id})).into_response())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Batch {
    days: Vec<UsageDay>,
}
async fn upload(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(batch): Json<Batch>,
) -> Reply {
    let c = cloud(&state)?;
    let now = (state.now)();
    let (d, a) = device(c, &headers, now).await?;
    let since = device_consent(c,&a,&d.id).await?.ok_or_else(|| {
        error(
            StatusCode::FORBIDDEN,
            "Usage sharing is off for this account.",
        )
    })?;
    if batch.days.len() > 7 || batch.days.iter().any(|day| !day.valid(now, since)) {
        return Err(error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Send at most seven valid daily numeric summaries since consent.",
        ));
    }
    if !state
        .limits
        .take_diagnostics(&crate::config::hash_token(&format!("usage:{}", d.id)), now)
    {
        return Err(error(
            StatusCode::TOO_MANY_REQUESTS,
            "Usage summaries are limited to four uploads per minute.",
        ));
    }
    let count = batch.days.len();
    for data in batch.days {
        c.store
            .save_usage(UsageRecord {
                account_id: a.id.clone(),
                device_id: d.id.clone(),
                platform: d.platform,
                data,
                received_at: now,
            })
            .await
            .map_err(storage)?;
    }
    Ok(Json(json!({"accepted":count})).into_response())
}
async fn list(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Reply {
    let c = cloud(&state)?;
    let a = session(c, &Method::GET, &headers, (state.now)()).await?;
    let choices=c.store.device_consents(&a.id).await.map_err(storage)?;
    let devices=c.store.devices(&a.id).await.map_err(storage)?;
    let enabled=devices.iter().any(|d|choices.iter().find(|choice|choice.device_id==d.id).map(|choice|choice.enabled).unwrap_or_else(||consent(&a).is_some()));
    Ok(Json(json!({"enabled":enabled,"devices":choices,"days":c.store.account_usage(&a.id).await.map_err(storage)?})).into_response())
}
