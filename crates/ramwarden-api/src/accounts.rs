//! Phase 2 routes: device sign-in for the daemon, wallet sign-in for the
//! website, the account page, checkout, the signed entitlement and paid
//! warden downloads. The website's half is specified in
//! `docs/specs/website.md`, the rest in `docs/specs/hosted-api.md`.
//!
//! Three kinds of bearer credential, never interchangeable:
//!
//! | Credential | Who holds it | Opens |
//! | --- | --- | --- |
//! | web session (`rwse_`) | one browser tab | account, devices, checkout |
//! | device token (`rwdt_`) | one daemon install | Jev questions, entitlement, downloads |
//! | phase 1 token | the operator's own machines | Jev questions |

// Helpers here return axum's `Response` as their error so handlers can `?`
// straight into a reply. It is built at most once per request, so its size
// does not matter.
#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::checkout::{BaseChain, Plans, usdc_minor};
use crate::entitlement::EntitlementSigner;
use crate::routes::{AppState, error};
use crate::store::{
    Account, Challenge, Claim, Device, Payment, PaymentMethod, PaymentStatus, Platform, Store, normalise_user_code,
    random_bytes,
};
use crate::wallet::{self, Chain, MessageParts, iso8601};

/// How long a device sign-in waits for the person (RFC 8628 suggests ~15 min).
pub const DEVICE_CODE_TTL: u64 = 10 * 60;
/// Seconds between the daemon's polls.
pub const DEVICE_POLL_INTERVAL: u64 = 5;
/// How long a signed message may sit in the wallet before it is stale.
pub const CHALLENGE_TTL: u64 = 5 * 60;
/// A web session; the page keeps it in `sessionStorage`, so usually it dies
/// with the tab first.
pub const SESSION_TTL: u64 = 12 * 60 * 60;
/// The warden whose pass is the hosted (Pro) tier.
pub const PRO_WARDEN: &str = "ram";

/// The website, as sign-in messages and links name it.
#[derive(Clone, Debug)]
pub struct Site {
    /// Host only, e.g. `ramwarden.app`.
    pub domain: String,
    /// Origin, e.g. `https://ramwarden.app`. Also the CORS origin.
    pub origin: String,
}

/// Everything phase 2 needs. Absent, the routes answer 503 and the server is
/// phase 1 only.
pub struct Cloud {
    pub store: Arc<dyn Store>,
    pub plans: Arc<Plans>,
    pub site: Site,
    pub base: Option<Arc<BaseChain>>,
    pub signer: Option<EntitlementSigner>,
    /// `<dir>/<warden>/<file>` for paid wardens' installers.
    pub downloads_dir: Option<PathBuf>,
    /// Hosted decisions a day without a Pro pass.
    pub free_per_day: u32,
    /// The chain id named in SIWE messages.
    pub siwe_chain_id: u64,
}

pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/v1/device/start", post(device_start))
        .route("/v1/device/token", post(device_token))
        .route("/v1/device/lookup", get(device_lookup))
        .route("/v1/auth/challenge", post(challenge))
        .route("/v1/auth/verify", post(verify))
        .route("/v1/account", get(account))
        .route("/v1/account/devices/{id}", delete(revoke_device))
        .route("/v1/checkout/config", get(checkout_config))
        .route("/v1/checkout/base", post(checkout_base))
        .route("/v1/checkout/solana", post(checkout_unavailable))
        .route("/v1/checkout/stripe", post(checkout_unavailable))
        .route("/v1/checkout/payments/{id}", get(payment))
        .route("/v1/entitlement", get(entitlement))
        .route("/v1/downloads/{warden}/{file}", get(download))
}

type Reply = Result<Response, Response>;

fn cloud(state: &AppState) -> Result<&Cloud, Response> {
    state
        .cloud
        .as_ref()
        .ok_or_else(|| error(StatusCode::SERVICE_UNAVAILABLE, "Hosted sign-in is not enabled on this server."))
}

fn storage<E: std::fmt::Display>(e: E) -> Response {
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

async fn session(c: &Cloud, headers: &HeaderMap, now: u64) -> Result<Account, Response> {
    let refuse = || error(StatusCode::UNAUTHORIZED, "Your sign-in has expired. Sign in again.");
    let raw = bearer(headers).ok_or_else(refuse)?;
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

async fn account_json(c: &Cloud, a: &Account, now: u64) -> Result<Value, Response> {
    let devices = c.store.devices(&a.id).await.map_err(storage)?;
    let used = c.store.used_today(&a.id, now).await.map_err(storage)?;
    let pro = a.active(PRO_WARDEN, now);
    Ok(json!({
        "id": a.id,
        "tier": if pro { "pro" } else { "free" },
        "entitlement_until": a.entitlements.get(PRO_WARDEN).map(|&t| iso8601(t)),
        "entitlements": a.entitlements.iter().map(|(w, &t)| (w.clone(), json!(iso8601(t)))).collect::<serde_json::Map<_, _>>(),
        "used_today": used,
        "wallets": a.wallets.iter().map(|w| json!({"chain": w.chain, "address": w.chain.display(&w.address)})).collect::<Vec<_>>(),
        "devices": devices.iter().map(device_json).collect::<Vec<_>>(),
    }))
}

fn device_json(d: &Device) -> Value {
    json!({"id": d.id, "name": d.name, "platform": d.platform, "last_seen_at": iso8601(d.last_seen_at)})
}

// ------------------------------------------------------------- device sign-in

#[derive(Deserialize)]
struct DeviceStart {
    device_name: String,
    platform: Platform,
}

/// The daemon starts a sign-in and opens `verification_uri_complete`.
async fn device_start(State(state): State<Arc<AppState>>, Json(b): Json<DeviceStart>) -> Reply {
    let c = cloud(&state)?;
    let name: String = b.device_name.trim().chars().filter(|ch| !ch.is_control()).take(64).collect();
    if name.is_empty() {
        return Err(error(StatusCode::UNPROCESSABLE_ENTITY, "device_name is empty"));
    }
    let now = (state.now)();
    let (raw, dc) = c.store.start_device_code(&name, b.platform, now, DEVICE_CODE_TTL).await.map_err(storage)?;
    let link = format!("{}/link.html", c.site.origin);
    Ok(Json(json!({
        "device_code": raw,
        "user_code": dc.user_code,
        "verification_uri": link,
        "verification_uri_complete": format!("{link}?code={}", dc.user_code),
        "expires_in": DEVICE_CODE_TTL,
        "interval": DEVICE_POLL_INTERVAL,
    }))
    .into_response())
}

#[derive(Deserialize)]
struct DeviceToken {
    device_code: String,
}

/// The daemon's poll. Error codes are RFC 8628's, since only the daemon reads
/// them.
async fn device_token(State(state): State<Arc<AppState>>, Json(b): Json<DeviceToken>) -> Reply {
    let c = cloud(&state)?;
    let now = (state.now)();
    match c.store.claim_device_code(&b.device_code, now).await.map_err(storage)? {
        Claim::Pending => Err(error(StatusCode::BAD_REQUEST, "authorization_pending")),
        Claim::Gone => Err(error(StatusCode::BAD_REQUEST, "expired_token")),
        Claim::Approved { account_id, device_name, platform } => {
            let (d, token) =
                c.store.register_device(&account_id, &device_name, platform, now).await.map_err(storage)?;
            tracing::info!(account = %account_id, device = %d.id, "device signed in");
            Ok(Json(json!({"device_token": token, "account_id": account_id, "device_id": d.id})).into_response())
        }
    }
}

#[derive(Deserialize)]
struct Lookup {
    user_code: String,
}

/// The link page shows which device it is approving before asking to sign.
async fn device_lookup(State(state): State<Arc<AppState>>, Query(q): Query<Lookup>) -> Reply {
    let c = cloud(&state)?;
    let gone = || error(StatusCode::NOT_FOUND, "That code is unknown, used or expired. Start sign-in again in RamWarden.");
    let code = normalise_user_code(&q.user_code).ok_or_else(gone)?;
    let dc = c.store.pending_device_code(&code, (state.now)()).await.map_err(storage)?.ok_or_else(gone)?;
    Ok(Json(json!({
        "status": "pending",
        "device_name": dc.device_name,
        "platform": dc.platform,
        "expires_at": iso8601(dc.expires_at),
    }))
    .into_response())
}

// ------------------------------------------------------------- wallet sign-in

#[derive(Deserialize)]
struct ChallengeReq {
    chain: Chain,
    address: String,
    #[serde(default)]
    user_code: Option<String>,
}

async fn challenge(State(state): State<Arc<AppState>>, Json(b): Json<ChallengeReq>) -> Reply {
    let c = cloud(&state)?;
    let now = (state.now)();
    let address = b.chain.normalise(&b.address).map_err(|e| error(StatusCode::UNPROCESSABLE_ENTITY, &e))?;
    let user_code = match b.user_code.as_deref().filter(|s| !s.trim().is_empty()) {
        None => None,
        Some(raw) => {
            let gone = || error(StatusCode::NOT_FOUND, "That code is unknown, used or expired. Start sign-in again in RamWarden.");
            let code = normalise_user_code(raw).ok_or_else(gone)?;
            c.store.pending_device_code(&code, now).await.map_err(storage)?.ok_or_else(gone)?;
            Some(code)
        }
    };
    let nonce = hex::encode(random_bytes::<16>());
    let expires_at = now + CHALLENGE_TTL;
    let message = wallet::message(
        b.chain,
        &address,
        &MessageParts {
            domain: &c.site.domain,
            uri: &c.site.origin,
            chain_id: c.siwe_chain_id,
            nonce: &nonce,
            issued_at: now,
            expires_at,
            user_code: user_code.as_deref(),
        },
    );
    let stored = Challenge { chain: b.chain, address, message: message.clone(), user_code, expires_at };
    c.store.put_challenge(&nonce, stored, now).await.map_err(storage)?;
    Ok(Json(json!({"message": message, "expires_at": iso8601(expires_at)})).into_response())
}

#[derive(Deserialize)]
struct VerifyReq {
    chain: Chain,
    address: String,
    message: String,
    signature: String,
}

async fn verify(State(state): State<Arc<AppState>>, Json(b): Json<VerifyReq>) -> Reply {
    let c = cloud(&state)?;
    let now = (state.now)();
    let stale = || error(StatusCode::UNAUTHORIZED, "That sign-in request expired or was already used. Try again.");
    let nonce = wallet::nonce_of(&b.message).ok_or_else(stale)?;
    // Taken before checking anything, so a wrong signature also uses it up.
    let ch = c.store.take_challenge(nonce, now).await.map_err(storage)?.ok_or_else(stale)?;
    let address = b.chain.normalise(&b.address).map_err(|e| error(StatusCode::UNPROCESSABLE_ENTITY, &e))?;
    if ch.chain != b.chain || ch.address != address || ch.message != b.message {
        return Err(error(StatusCode::UNAUTHORIZED, "The signed message is not the one this server issued."));
    }
    wallet::verify(b.chain, &address, &b.message, &b.signature)
        .map_err(|_| error(StatusCode::UNAUTHORIZED, "The signature did not match the wallet. Try again."))?;

    let account = c.store.account_for_wallet(b.chain, &address, now).await.map_err(storage)?;
    let mut linked = Value::Null;
    if let Some(code) = &ch.user_code {
        let dc = c.store.pending_device_code(code, now).await.map_err(storage)?;
        match dc {
            Some(dc) if c.store.approve_device_code(code, &account.id, now).await.map_err(storage)? => {
                linked = json!({"name": dc.device_name, "platform": dc.platform});
            }
            _ => {
                return Err(error(
                    StatusCode::GONE,
                    "The device code expired while you were signing. Start sign-in again in RamWarden.",
                ));
            }
        }
    }
    let session = c.store.create_session(&account.id, now, SESSION_TTL).await.map_err(storage)?;
    let account = account_json(c, &account, now).await?;
    Ok(Json(json!({"session": session, "account": account, "linked_device": linked})).into_response())
}

// ------------------------------------------------------------- account

async fn account(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Reply {
    let c = cloud(&state)?;
    let now = (state.now)();
    let a = session(c, &headers, now).await?;
    Ok(Json(account_json(c, &a, now).await?).into_response())
}

async fn revoke_device(State(state): State<Arc<AppState>>, headers: HeaderMap, Path(id): Path<String>) -> Reply {
    let c = cloud(&state)?;
    let a = session(c, &headers, (state.now)()).await?;
    if c.store.revoke_device(&a.id, &id).await.map_err(storage)? {
        Ok(Json(json!({"ok": true})).into_response())
    } else {
        Err(error(StatusCode::NOT_FOUND, "No such device on this account."))
    }
}

// ------------------------------------------------------------- checkout

async fn checkout_config(State(state): State<Arc<AppState>>) -> Reply {
    let c = cloud(&state)?;
    Ok(Json(c.plans.config_json(c.base.as_deref().map(|b| &b.cfg))).into_response())
}

#[derive(Deserialize)]
struct CheckoutReq {
    plan: u32,
    #[serde(default)]
    payer: Option<String>,
}

async fn checkout_base(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(b): Json<CheckoutReq>) -> Reply {
    let c = cloud(&state)?;
    let now = (state.now)();
    let a = session(c, &headers, now).await?;
    let base = c.base.as_ref().ok_or_else(|| error(StatusCode::SERVICE_UNAVAILABLE, "Paying on Base opens soon."))?;
    let plan = c.plans.plan(b.plan).ok_or_else(|| error(StatusCode::NOT_FOUND, "No such plan."))?;
    let payer = match &b.payer {
        Some(p) => Some(Chain::Evm.normalise(p).map_err(|e| error(StatusCode::UNPROCESSABLE_ENTITY, &e))?),
        None => None,
    };
    let payment_ref = random_bytes::<32>();
    let needs_approve = match &payer {
        Some(p) => base.needs_approve(p, usdc_minor(plan.usd)).await,
        None => true,
    };
    let p = Payment {
        id: hex::encode(payment_ref),
        account_id: a.id.clone(),
        plan: plan.id,
        method: PaymentMethod::Base,
        status: PaymentStatus::Pending,
        reason: None,
        payer,
        created_at: now,
        wardens_until: BTreeMap::new(),
        external_id: None,
    };
    let id = p.id.clone();
    c.store.create_payment(p).await.map_err(storage)?;
    Ok(Json(json!({
        "payment_id": id,
        "chain_id": base.cfg.chain_id,
        "transactions": base.cfg.transactions(&a.id, plan, &payment_ref, needs_approve),
    }))
    .into_response())
}

async fn checkout_unavailable(State(state): State<Arc<AppState>>) -> Reply {
    cloud(&state)?;
    Err(error(
        StatusCode::SERVICE_UNAVAILABLE,
        "This payment method opens soon. Pay with USDC on Base from MetaMask or Phantom.",
    ))
}

async fn payment(State(state): State<Arc<AppState>>, headers: HeaderMap, Path(id): Path<String>) -> Reply {
    let c = cloud(&state)?;
    let a = session(c, &headers, (state.now)()).await?;
    let p = c
        .store
        .payment(&id)
        .await
        .map_err(storage)?
        .filter(|p| p.account_id == a.id)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "No such payment on this account."))?;
    let until = p.wardens_until.values().max().map(|&t| iso8601(t));
    Ok(Json(json!({
        "id": p.id,
        "plan": p.plan,
        "method": p.method,
        "status": p.status,
        "reason": p.reason,
        "entitlement_until": until,
        "wardens_until": p.wardens_until.iter().map(|(w, &t)| (w.clone(), json!(iso8601(t)))).collect::<serde_json::Map<_, _>>(),
    }))
    .into_response())
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
    Ok(Json(signer.sign(&a.id, &d.id, now, a.entitlements.clone())).into_response())
}

/// A paid warden's installer, for a device whose account holds that warden's
/// pass. The launcher checks size and SHA-256 against the signed release
/// manifest, so this route decides only *who* may download, never *what*.
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
        return Err(error(StatusCode::FORBIDDEN, "This warden needs a pass. Buy one on your account page."));
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

#[cfg(test)]
mod tests {
    //! The website's and the daemon's flows end to end, against the router,
    //! with wallets that sign the way MetaMask and Phantom do.
    use super::*;
    use crate::catalog::Catalog;
    use crate::checkout::{BASE_SEPOLIA, BaseConfig};
    use crate::limits::Limits;
    use crate::routes::router;
    use crate::store::MemoryStore;
    use crate::upstream::Upstream;
    use crate::wallet::testing::{EvmWallet, SolWallet};
    use axum::body::to_bytes;
    use axum::http::Request;
    use ramwarden_ai::secret::Secret;
    use std::time::Duration;
    use tower::ServiceExt;

    const NOW: u64 = 1_793_000_000;
    const SEED: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";

    struct App {
        router: axum::Router,
        store: Arc<MemoryStore>,
    }

    fn app(downloads: Option<PathBuf>) -> App {
        let store = Arc::new(MemoryStore::new());
        let base = BaseConfig {
            chain_id: BASE_SEPOLIA,
            checkout: [0xcc; 20],
            usdc: BaseConfig::default_usdc(BASE_SEPOLIA).unwrap(),
            rpc_url: None,
            confirmations: 3,
            from_block: None,
        };
        let cloud = Cloud {
            store: store.clone(),
            plans: Arc::new(Plans::built_in()),
            site: Site { domain: "ramwarden.app".into(), origin: "https://ramwarden.app".into() },
            base: Some(Arc::new(BaseChain::new(base))),
            signer: Some(EntitlementSigner::from_base64(SEED).unwrap()),
            downloads_dir: downloads,
            free_per_day: 2,
            siwe_chain_id: BASE_SEPOLIA,
        };
        let state = AppState {
            tokens: Default::default(),
            catalog: Catalog::built_in(),
            limits: Limits::new(100, 100, u64::MAX),
            upstream: Upstream::new("http://127.0.0.1:9", "jev-latest", Secret::new("k-0123456789"), Duration::from_secs(1))
                .unwrap(),
            max_state_bytes: 1000,
            max_questions: 4,
            now: || NOW,
            cloud: Some(cloud),
        };
        App { router: router(Arc::new(state)), store }
    }

    impl App {
        async fn call(&self, method: &str, uri: &str, bearer: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
            let mut r = Request::builder().method(method).uri(uri).header(header::ORIGIN, "https://ramwarden.app");
            if let Some(t) = bearer {
                r = r.header(header::AUTHORIZATION, format!("Bearer {t}"));
            }
            let req = match body {
                Some(b) => r.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())),
                None => r.body(Body::empty()),
            }
            .unwrap();
            let resp = self.router.clone().oneshot(req).await.unwrap();
            let status = resp.status();
            let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
            (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
        }

        async fn sign_in_evm(&self, w: &EvmWallet, code: Option<&str>) -> (StatusCode, Value) {
            let (s, ch) = self
                .call("POST", "/v1/auth/challenge", None, Some(json!({"chain": "evm", "address": w.address(), "user_code": code})))
                .await;
            assert_eq!(s, StatusCode::OK, "{ch}");
            let m = ch["message"].as_str().unwrap();
            let sig = w.personal_sign(m);
            self.call("POST", "/v1/auth/verify", None, Some(json!({"chain": "evm", "address": w.address(), "message": m, "signature": sig})))
                .await
        }

        /// The daemon's whole sign-in: start, approve with a wallet, claim.
        async fn sign_in_device(&self, w: &EvmWallet) -> (String, Value) {
            let (_, start) = self
                .call("POST", "/v1/device/start", None, Some(json!({"device_name": "ethan-desktop", "platform": "linux"})))
                .await;
            let dc = start["device_code"].as_str().unwrap().to_string();
            let (s, v) = self.sign_in_evm(w, start["user_code"].as_str()).await;
            assert_eq!(s, StatusCode::OK, "{v}");
            let (s, tok) = self.call("POST", "/v1/device/token", None, Some(json!({"device_code": dc}))).await;
            assert_eq!(s, StatusCode::OK, "{tok}");
            (tok["device_token"].as_str().unwrap().to_string(), v)
        }
    }

    #[tokio::test]
    async fn a_daemon_signs_in_through_the_link_page_with_metamask() {
        let a = app(None);
        let (_, start) = a
            .call("POST", "/v1/device/start", None, Some(json!({"device_name": "ethan-desktop", "platform": "linux"})))
            .await;
        let code = start["user_code"].as_str().unwrap();
        assert_eq!(start["verification_uri_complete"], format!("https://ramwarden.app/link.html?code={code}"));
        let dc = start["device_code"].as_str().unwrap();

        let (s, poll) = a.call("POST", "/v1/device/token", None, Some(json!({"device_code": dc}))).await;
        assert_eq!((s, poll["error"].as_str()), (StatusCode::BAD_REQUEST, Some("authorization_pending")));

        // The link page shows the device first, from a code typed in lowercase.
        let (s, look) = a.call("GET", &format!("/v1/device/lookup?user_code={}", code.to_lowercase()), None, None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!((look["device_name"].as_str(), look["platform"].as_str()), (Some("ethan-desktop"), Some("linux")));

        let w = EvmWallet::new(3);
        let (s, v) = a.sign_in_evm(&w, Some(code)).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["linked_device"]["name"], "ethan-desktop");
        assert_eq!(v["account"]["tier"], "free");
        assert_eq!(v["account"]["wallets"][0]["address"], w.address(), "shown checksummed");

        let (s, tok) = a.call("POST", "/v1/device/token", None, Some(json!({"device_code": dc}))).await;
        assert_eq!(s, StatusCode::OK);
        assert!(tok["device_token"].as_str().unwrap().starts_with("rwdt_"));
        let (s, again) = a.call("POST", "/v1/device/token", None, Some(json!({"device_code": dc}))).await;
        assert_eq!((s, again["error"].as_str()), (StatusCode::BAD_REQUEST, Some("expired_token")), "claimed once");
        let (s, _) = a.call("GET", &format!("/v1/device/lookup?user_code={code}"), None, None).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "a used code no longer looks up");

        let session = v["session"].as_str().unwrap();
        let (_, acct) = a.call("GET", "/v1/account", Some(session), None).await;
        assert_eq!(acct["devices"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn phantom_signs_in_on_the_web_and_reuses_its_account() {
        let a = app(None);
        let w = SolWallet::new(4);
        let mut ids = vec![];
        for _ in 0..2 {
            let (_, ch) = a.call("POST", "/v1/auth/challenge", None, Some(json!({"chain": "solana", "address": w.address()}))).await;
            let m = ch["message"].as_str().unwrap();
            assert!(m.contains("with your Solana account:"));
            let (s, v) = a
                .call("POST", "/v1/auth/verify", None, Some(json!({"chain": "solana", "address": w.address(), "message": m, "signature": w.sign_message(m)})))
                .await;
            assert_eq!(s, StatusCode::OK, "{v}");
            assert!(v["linked_device"].is_null());
            ids.push(v["account"]["id"].clone());
        }
        assert_eq!(ids[0], ids[1]);
    }

    #[tokio::test]
    async fn forged_replayed_or_foreign_signatures_are_refused() {
        let a = app(None);
        let (w, thief) = (EvmWallet::new(5), EvmWallet::new(6));
        let challenge = |addr: String| {
            let a = &a;
            async move { a.call("POST", "/v1/auth/challenge", None, Some(json!({"chain": "evm", "address": addr}))).await.1 }
        };
        // Signed by someone else.
        let m = challenge(w.address()).await["message"].as_str().unwrap().to_string();
        let body = json!({"chain": "evm", "address": w.address(), "message": m, "signature": thief.personal_sign(&m)});
        assert_eq!(a.call("POST", "/v1/auth/verify", None, Some(body)).await.0, StatusCode::UNAUTHORIZED);
        // The right signature, but the challenge was used up by the failure.
        let body = json!({"chain": "evm", "address": w.address(), "message": m, "signature": w.personal_sign(&m)});
        assert_eq!(a.call("POST", "/v1/auth/verify", None, Some(body)).await.0, StatusCode::UNAUTHORIZED);
        // A message the server did not write, quoting a live nonce.
        let m = challenge(w.address()).await["message"].as_str().unwrap().to_string();
        let edited = m.replace("ramwarden.app wants", "evil.example wants");
        let body = json!({"chain": "evm", "address": w.address(), "message": edited, "signature": w.personal_sign(&edited)});
        assert_eq!(a.call("POST", "/v1/auth/verify", None, Some(body)).await.0, StatusCode::UNAUTHORIZED);
        // A challenge for one wallet, signed and claimed by another.
        let m = challenge(w.address()).await["message"].as_str().unwrap().to_string();
        let body = json!({"chain": "evm", "address": thief.address(), "message": m, "signature": thief.personal_sign(&m)});
        assert_eq!(a.call("POST", "/v1/auth/verify", None, Some(body)).await.0, StatusCode::UNAUTHORIZED);
        // Unknown device codes are refused before anything is signed.
        let (s, _) = a
            .call("POST", "/v1/auth/challenge", None, Some(json!({"chain": "evm", "address": w.address(), "user_code": "BCDF-GHJK"})))
            .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        // No session, no account.
        assert_eq!(a.call("GET", "/v1/account", None, None).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(a.call("GET", "/v1/account", Some("rwse_nope"), None).await.0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn checkout_builds_base_transactions_and_the_chain_credits_each_warden() {
        let a = app(None);
        let w = EvmWallet::new(7);
        let (device_token, v) = a.sign_in_device(&w).await;
        let session = v["session"].as_str().unwrap().to_string();
        let account_id = v["account"]["id"].as_str().unwrap().to_string();

        let (_, cfg) = a.call("GET", "/v1/checkout/config", None, None).await;
        assert_eq!(cfg["base"]["chain_id"], BASE_SEPOLIA);
        let bundle = cfg["plans"].as_array().unwrap().iter().find(|p| p["wardens"] == json!(["ram", "wazuh"])).unwrap();
        let plan = bundle["id"].as_u64().unwrap() as u32;

        let (s, co) = a.call("POST", "/v1/checkout/base", Some(&session), Some(json!({"plan": plan, "payer": w.address()}))).await;
        assert_eq!(s, StatusCode::OK, "{co}");
        assert_eq!(co["transactions"].as_array().unwrap().len(), 2, "approve, then purchase");
        let pid = co["payment_id"].as_str().unwrap().to_string();
        let (_, p) = a.call("GET", &format!("/v1/checkout/payments/{pid}"), Some(&session), None).await;
        assert_eq!(p["status"], "pending");

        // Before paying: free tier, and the paid warden's entitlement is empty.
        let (_, ent) = a.call("GET", "/v1/entitlement", Some(&device_token), None).await;
        assert_eq!(ent["wardens"], json!({}));

        // The contract's logs for that purchase, as the watcher reads them.
        let mut r = [0u8; 32];
        r.copy_from_slice(&hex::decode(&pid).unwrap());
        let logs = crate::checkout::tests::purchase_logs(
            &[0xcc; 20],
            crate::store::chain_key(&account_id),
            plan,
            r,
            &[(0, NOW + 30 * 86_400), (1, NOW + 30 * 86_400)],
            "0xt",
        );
        let plans = Plans::built_in();
        for paid in crate::checkout::decode_logs(&logs, &[0xcc; 20], &plans) {
            assert!(crate::checkout::credit(a.store.as_ref(), &paid).await.unwrap());
        }

        let (_, p) = a.call("GET", &format!("/v1/checkout/payments/{pid}"), Some(&session), None).await;
        assert_eq!(p["status"], "confirmed");
        let (_, acct) = a.call("GET", "/v1/account", Some(&session), None).await;
        assert_eq!(acct["tier"], "pro");
        assert_eq!(acct["entitlement_until"], iso8601(NOW + 30 * 86_400));
        assert_eq!(acct["entitlements"]["wazuh"], iso8601(NOW + 30 * 86_400));

        // The daemon's signed record names both wardens and verifies.
        let (_, ent) = a.call("GET", "/v1/entitlement", Some(&device_token), None).await;
        let rec: crate::entitlement::Record = serde_json::from_value(ent).unwrap();
        assert_eq!(rec.wardens["wazuh"], NOW + 30 * 86_400);
        let key = EntitlementSigner::from_base64(SEED).unwrap().public_key();
        assert!(crate::entitlement::verify(&rec, &key));

        // Another account cannot read the payment.
        let (_, other) = a.sign_in_evm(&EvmWallet::new(8), None).await;
        let (s, _) = a.call("GET", &format!("/v1/checkout/payments/{pid}"), other["session"].as_str(), None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn paid_downloads_need_that_wardens_pass_and_a_plain_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("wazuh")).unwrap();
        std::fs::write(dir.path().join("wazuh/wazuh-warden-linux-x86_64.tar.gz"), b"installer").unwrap();
        let a = app(Some(dir.path().to_path_buf()));
        let (tok, v) = a.sign_in_device(&EvmWallet::new(9)).await;
        let uri = "/v1/downloads/wazuh/wazuh-warden-linux-x86_64.tar.gz";

        assert_eq!(a.call("GET", uri, None, None).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(a.call("GET", uri, Some(&tok), None).await.0, StatusCode::FORBIDDEN, "no wazuh pass");

        let id = v["account"]["id"].as_str().unwrap();
        a.store.extend_entitlements(id, &BTreeMap::from([("ram".to_string(), NOW + 100)])).await.unwrap();
        assert_eq!(a.call("GET", uri, Some(&tok), None).await.0, StatusCode::FORBIDDEN, "a RAM pass is not a Wazuh pass");

        a.store.extend_entitlements(id, &BTreeMap::from([("wazuh".to_string(), NOW + 100)])).await.unwrap();
        let req = Request::get(uri).header(header::AUTHORIZATION, format!("Bearer {tok}")).body(Body::empty()).unwrap();
        let resp = a.router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(&to_bytes(resp.into_body(), 1024).await.unwrap()[..], b"installer");

        for bad in ["/v1/downloads/wazuh/..%2Fsecret", "/v1/downloads/wazuh/.env", "/v1/downloads/wazuh/missing.exe"] {
            assert_eq!(a.call("GET", bad, Some(&tok), None).await.0, StatusCode::NOT_FOUND, "{bad}");
        }

        // A removed device loses access at once.
        let session = v["session"].as_str().unwrap();
        let (_, acct) = a.call("GET", "/v1/account", Some(session), None).await;
        let dev = acct["devices"][0]["id"].as_str().unwrap();
        let (s, _) = a.call("DELETE", &format!("/v1/account/devices/{dev}"), Some(session), None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(a.call("GET", uri, Some(&tok), None).await.0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn signed_in_devices_get_the_free_quota_on_jev_until_they_hold_a_ram_pass() {
        let a = app(None);
        let (tok, _) = a.sign_in_device(&EvmWallet::new(10)).await;
        let (s, _) = a.call("GET", "/v1/models", Some(&tok), None).await;
        assert_eq!(s, StatusCode::OK, "a device token opens the Jev endpoints");
        assert_eq!(a.call("GET", "/v1/models", Some("rwdt_unknown"), None).await.0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn only_the_site_origin_gets_cors() {
        let a = app(None);
        let pre = |origin: &'static str| {
            Request::builder()
                .method("OPTIONS")
                .uri("/v1/account")
                .header(header::ORIGIN, origin)
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
                .body(Body::empty())
                .unwrap()
        };
        let ok = a.router.clone().oneshot(pre("https://ramwarden.app")).await.unwrap();
        assert_eq!(ok.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "https://ramwarden.app");
        // An exact origin is always named back; the browser refuses any page
        // that is not it.
        let bad = a.router.clone().oneshot(pre("https://evil.example")).await.unwrap();
        assert_ne!(bad.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).map(|v| v.as_bytes()), Some(&b"https://evil.example"[..]));
    }
}
