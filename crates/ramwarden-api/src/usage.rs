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
    usage::{UsageDay, UsageRecord},
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/v1/usage/config", get(config))
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
    let since = consent(&a);
    Ok(
        Json(json!({"enabled":since.is_some(),"since":since,"device_id":d.id,"retention_days":90}))
            .into_response(),
    )
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
    let since = consent(&a).ok_or_else(|| {
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
    Ok(Json(json!({"enabled":consent(&a).is_some(),"days":c.store.account_usage(&a.id).await.map_err(storage)?})).into_response())
}
