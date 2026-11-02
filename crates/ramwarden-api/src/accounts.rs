//! Accounts on the hosted API: the shared pieces every account route uses
//! (who is calling, how an account is shown), and the routes that read or
//! change the account itself, the signed entitlement and paid downloads.
//! Sign-in is in [`crate::signin`], checkout in [`crate::billing`]. The
//! website's half is specified in `docs/specs/website.md`, the rest in
//! `docs/specs/hosted-api.md`, both under `docs/product-spec.md` 4.
//!
//! Three kinds of credential, never interchangeable:
//!
//! | Credential | Who holds it | Opens |
//! | --- | --- | --- |
//! | web session (`rwse_`) | one browser, as the `rw_session` cookie | account, devices, prefs, checkout |
//! | device token (`rwdt_`) | one daemon install | Jev questions, entitlement, downloads |
//! | phase 1 token | the operator's own machines | Jev questions |
//!
//! The web session is an `HttpOnly; Secure; SameSite=Strict` cookie (spec
//! 4.1), so a script injected into the page can't read it and another site
//! can't send it. A tool without a cookie jar (an agent's wallet tool) may ask
//! for the same token in the body and send it as a bearer instead.

// Helpers here return axum's `Response` as their error so handlers can `?`
// straight into a reply. It is built at most once per request, so its size
// does not matter.
#![allow(clippy::result_large_err)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, put};
use ramwarden_cloud::model::{Account, Device, Identity};
use ramwarden_cloud::store::Store;
use serde_json::{Map, Value, json};

use crate::billing::Billing;
use crate::checkout::Plans;
use crate::entitlement::{Claims, EntitlementSigner};
use crate::mail::Mailer;
use crate::routes::{AppState, error};
use crate::wallet::{self, iso8601};

/// Devices on RamWarden Lite, the free tier (an account with no plan, or a
/// lapsed one).
pub const FREE_DEVICE_LIMIT: u32 = 1;
/// Devices on RamWarden Plus (user decision, 2026-11-02).
pub const PLUS_DEVICE_LIMIT: u32 = 3;
/// Devices on RamWarden Pro.
pub const PRO_DEVICE_LIMIT: u32 = 5;

/// How many devices this account may have now: its plan's limit while the
/// plan is in good standing, else the free tier's. Devices already linked
/// stay linked when a plan lapses; only adding one is refused.
pub fn device_limit(a: &Account, now: u64) -> u32 {
    let plan = match plan_name(a, now) {
        "pro" => PRO_DEVICE_LIMIT,
        "plus" => PLUS_DEVICE_LIMIT,
        _ => FREE_DEVICE_LIMIT,
    };
    // The stored limit is a per-account cap the operator can lower.
    plan.min(a.device_limit)
}

/// The card the account is on: `lite`, `plus` or `pro`.
pub fn plan_name(a: &Account, now: u64) -> &'static str {
    match a.tier {
        Some(t) if a.active(crate::routes::HOSTED_WARDEN, now) => match t {
            ramwarden_cloud::model::Tier::Ram => "plus",
            ramwarden_cloud::model::Tier::Pro => "pro",
        },
        _ => "lite",
    }
}

/// A refusal naming the limit, for the page the person is looking at.
pub fn device_limit_reached(limit: u32) -> Response {
    let msg = if limit <= FREE_DEVICE_LIMIT {
        "RamWarden Lite covers 1 device. Remove it on your account page, or upgrade to Plus for 3.".to_string()
    } else if limit <= PLUS_DEVICE_LIMIT {
        format!("RamWarden Plus covers {limit} devices. Remove one on your account page, or upgrade to Pro for {PRO_DEVICE_LIMIT}.")
    } else {
        format!("This account already has {limit} devices. Remove one on your account page first.")
    };
    error(StatusCode::CONFLICT, &msg)
}

/// True when the account can't take another device now.
pub async fn at_device_limit(c: &Cloud, a: &Account, now: u64) -> Result<bool, Response> {
    Ok(c.store.devices(&a.id).await.map_err(storage)?.len() >= device_limit(a, now) as usize)
}

/// The web session cookie.
pub const SESSION_COOKIE: &str = "rw_session";
/// Largest `prefs` object accepted, as JSON.
pub const MAX_PREFS_BYTES: usize = 4096;

/// The website, as sign-in messages and links name it.
#[derive(Clone, Debug)]
pub struct Site {
    /// Host only, e.g. `ramwarden.app`.
    pub domain: String,
    /// Origin, e.g. `https://ramwarden.app`. Also the CORS origin.
    pub origin: String,
}

/// Everything accounts need. Absent, these routes answer 503 and the server
/// is phase 1 only.
pub struct Cloud {
    pub store: Arc<dyn Store>,
    pub plans: Arc<Plans>,
    pub site: Site,
    pub billing: Billing,
    pub signer: Option<EntitlementSigner>,
    /// Sends email sign-in codes. `None`: email sign-in is off.
    pub mailer: Option<Arc<dyn Mailer>>,
    /// `<dir>/<warden>/<file>` for paid wardens' installers.
    pub downloads_dir: Option<PathBuf>,
    /// The chain id named in SIWE messages.
    pub siwe_chain_id: u64,
    /// This API's own public base URL, for the install line.
    pub public_url: String,
    /// Where release assets are downloaded from.
    pub releases_url: String,
}

pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/v1/account", get(account))
        .route("/v1/account/prefs", put(set_prefs))
        .route("/v1/account/devices/{id}", delete(revoke_device))
        .route("/v1/entitlement", get(entitlement))
        .route("/v1/downloads/{warden}/{file}", get(download))
        .merge(crate::signin::routes())
        .merge(crate::billing::routes())
        .merge(crate::install::routes())
}

pub type Reply = Result<Response, Response>;

pub fn cloud(state: &AppState) -> Result<&Cloud, Response> {
    state
        .cloud
        .as_ref()
        .ok_or_else(|| error(StatusCode::SERVICE_UNAVAILABLE, "Hosted sign-in is not enabled on this server."))
}

pub fn storage<E: std::fmt::Display>(e: E) -> Response {
    tracing::error!("store: {e}");
    error(StatusCode::SERVICE_UNAVAILABLE, "The server could not reach its database. Try again in a minute.")
}

pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

/// The value of one cookie.
pub fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.trim())
        .filter(|v| !v.is_empty())
}

/// The web session's raw token, from the cookie or a bearer.
pub fn session_token(headers: &HeaderMap) -> Option<&str> {
    cookie(headers, SESSION_COOKIE).or_else(|| bearer(headers).filter(|t| t.starts_with("rwse_")))
}

/// The signed-in account. A cookie-authenticated request that changes
/// something must also come from the site's own origin: SameSite=Strict
/// already keeps other sites' requests from carrying the cookie, and this
/// keeps that true on a browser that ignores the attribute.
pub async fn session(c: &Cloud, method: &Method, headers: &HeaderMap, now: u64) -> Result<Account, Response> {
    let refuse = || error(StatusCode::UNAUTHORIZED, "Your sign-in has expired. Sign in again.");
    let raw = session_token(headers).ok_or_else(refuse)?;
    if method != Method::GET
        && cookie(headers, SESSION_COOKIE).is_some()
        && let Some(o) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())
        && o != c.site.origin
    {
        return Err(error(StatusCode::FORBIDDEN, "That request did not come from the RamWarden site."));
    }
    let id = c.store.session_account(raw, now).await.map_err(storage)?.ok_or_else(refuse)?;
    c.store.account(&id).await.map_err(storage)?.ok_or_else(refuse)
}

pub async fn device(c: &Cloud, headers: &HeaderMap, now: u64) -> Result<(Device, Account), Response> {
    let refuse = || error(StatusCode::UNAUTHORIZED, "This device is not signed in, or was removed from the account.");
    let raw = bearer(headers).ok_or_else(refuse)?;
    let d = c.store.device_for_token(raw, now).await.map_err(storage)?.ok_or_else(refuse)?;
    let a = c.store.account(&d.account_id).await.map_err(storage)?.ok_or_else(refuse)?;
    Ok((d, a))
}

fn identity_json(i: &Identity) -> Value {
    match i {
        Identity::Wallet { chain, address } => {
            json!({"kind": "wallet", "chain": chain, "address": wallet::display(*chain, address)})
        }
        Identity::Email { email } => json!({"kind": "email", "email": email}),
    }
}

/// `GET /v1/account`, and the `account` in a sign-in reply.
pub async fn account_json(c: &Cloud, a: &Account, now: u64) -> Result<Value, Response> {
    let devices = c.store.devices(&a.id).await.map_err(storage)?;
    let used = c.store.used_today(&a.id, now).await.map_err(storage)?;
    let wardens = a.wardens();
    let until = a.served_until();
    Ok(json!({
        "id": a.id,
        "identity": identity_json(&a.identity),
        "tier": a.tier.map(|t| t.as_str()),
        "interval": a.interval,
        "status": a.status(now).map(|s| s.as_str()),
        "trial_ends_at": a.trial_ends_at.filter(|&t| t > 0).map(iso8601),
        "paid_until": a.paid_until.map(iso8601),
        "trial_available": !a.trial_used,
        "billing": a.billing.as_ref().map(|b| json!({"method": b.method})),
        "entitlement_until": (until > now).then(|| iso8601(until)),
        "entitlements": wardens.iter().map(|(w, &t)| (w.clone(), json!(iso8601(t)))).collect::<Map<_, _>>(),
        "used_today": used,
        "plan": plan_name(a, now),
        // Lite has no warden pet (user decision, 2026-11-02).
        "pet": plan_name(a, now) != "lite",
        "device_limit": device_limit(a, now),
        "devices": devices.iter().map(device_json).collect::<Vec<_>>(),
        "prefs": a.prefs,
    }))
}

fn device_json(d: &Device) -> Value {
    json!({"id": d.id, "name": d.name, "platform": d.platform, "last_seen_at": iso8601(d.last_seen_at)})
}

// ------------------------------------------------------------- account

async fn account(State(state): State<Arc<AppState>>, method: Method, headers: HeaderMap) -> Reply {
    let c = cloud(&state)?;
    let now = (state.now)();
    let a = session(c, &method, &headers, now).await?;
    Ok(Json(account_json(c, &a, now).await?).into_response())
}

/// Replaces `prefs` (spec 4.3: warden colors, the pet-comment mute). The
/// server keeps it opaque and small; the apps read it at sign-in.
async fn set_prefs(State(state): State<Arc<AppState>>, method: Method, headers: HeaderMap, Json(b): Json<Value>) -> Reply {
    let c = cloud(&state)?;
    let now = (state.now)();
    let a = session(c, &method, &headers, now).await?;
    let Value::Object(prefs) = b else {
        return Err(error(StatusCode::UNPROCESSABLE_ENTITY, "prefs must be a JSON object"));
    };
    if Value::Object(prefs.clone()).to_string().len() > MAX_PREFS_BYTES {
        return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "prefs are limited to 4 KB"));
    }
    let a = c.store.set_prefs(&a.id, prefs).await.map_err(storage)?.ok_or_else(|| storage("account vanished"))?;
    Ok(Json(json!({"prefs": a.prefs})).into_response())
}

async fn revoke_device(
    State(state): State<Arc<AppState>>,
    method: Method,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Reply {
    let c = cloud(&state)?;
    let a = session(c, &method, &headers, (state.now)()).await?;
    if c.store.revoke_device(&a.id, &id).await.map_err(storage)? {
        Ok(Json(json!({"ok": true})).into_response())
    } else {
        Err(error(StatusCode::NOT_FOUND, "No such device on this account."))
    }
}

// ------------------------------------------------------------- daemon

/// The signed record the daemon saves as `entitlement.json`.
async fn entitlement(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Reply {
    let c = cloud(&state)?;
    let now = (state.now)();
    let (d, a) = device(c, &headers, now).await?;
    let signer = c
        .signer
        .as_ref()
        .ok_or_else(|| error(StatusCode::SERVICE_UNAVAILABLE, "Entitlements are not enabled on this server."))?;
    Ok(Json(signer.sign(Claims::for_account(&a, &d.id, now))).into_response())
}

/// A paid warden's installer, for a device whose account's tier includes
/// that warden and is in good standing. The launcher checks size and SHA-256
/// against the signed release manifest, so this route decides only *who* may
/// download, never *what*.
async fn download(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((warden, file)): Path<(String, String)>,
) -> Reply {
    let c = cloud(&state)?;
    let now = (state.now)();
    let plain = |s: &str| {
        !s.is_empty()
            && !s.starts_with('.')
            && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    };
    if !plain(&warden) || !plain(&file) {
        return Err(error(StatusCode::NOT_FOUND, "No such download."));
    }
    let (_, a) = device(c, &headers, now).await?;
    if !a.active(&warden, now) {
        let why = if warden == "threat" {
            "ThreatWarden comes with RamWarden Pro. Upgrade on your account page."
        } else {
            "This needs an active RamWarden subscription. Renew on your account page."
        };
        return Err(error(StatusCode::FORBIDDEN, why));
    }
    let dir = c.downloads_dir.as_ref().ok_or_else(|| error(StatusCode::NOT_FOUND, "No such download."))?;
    let path = dir.join(&warden).join(&file);
    let f = tokio::fs::File::open(&path).await.map_err(|_| error(StatusCode::NOT_FOUND, "No such download."))?;
    let len = f.metadata().await.map(|m| m.len()).ok();
    let mut r = Body::from_stream(tokio_util::io::ReaderStream::new(f)).into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, header::HeaderValue::from_static("application/octet-stream"));
    if let Some(len) = len {
        h.insert(header::CONTENT_LENGTH, len.into());
    }
    if let Ok(v) = header::HeaderValue::from_str(&format!("attachment; filename=\"{file}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    tracing::info!(account = %a.id, %warden, %file, "download");
    Ok(r)
}
