//! Signing in: the daemon's device flow (RFC 8628), and the two ways a person
//! signs in on the website (product spec 3.1).
//!
//! - **Wallet**: Sign-In with Ethereum / Solana. The server writes the
//!   message and keeps it by nonce; verify accepts only that exact text,
//!   once, signed by the wallet it names. No gas, no transaction.
//! - **Email**: a six-digit code, mailed, single use, ten minutes, five
//!   guesses. A code (rather than a magic link) works when the email is read
//!   on a phone and the site is open on the desktop.
//!
//! Either one finds or creates the account for that identity, optionally
//! approves the device code the link page was opened with, and starts a web
//! session in the `rw_session` cookie.

#![allow(clippy::result_large_err)]

use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use ramwarden_cloud::model::{Account, Challenge, Claim, EmailCheck, Identity, Platform};
use ramwarden_cloud::store::{email_code, normalise_user_code, random_bytes};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::accounts::{
    Cloud, Reply, SESSION_COOKIE, account_json, at_device_limit, cloud, device_limit, device_limit_reached, session_token,
    storage,
};
use crate::routes::{AppState, error};
use crate::wallet::{self, Chain, MessageParts, iso8601};

/// How long a device sign-in waits for the person (RFC 8628 suggests ~15 min).
pub const DEVICE_CODE_TTL: u64 = 10 * 60;
/// Seconds between the daemon's polls.
pub const DEVICE_POLL_INTERVAL: u64 = 5;
/// How long a signed message may sit in the wallet before it is stale.
pub const CHALLENGE_TTL: u64 = 5 * 60;
/// How long an emailed code works.
pub const EMAIL_CODE_TTL: u64 = 10 * 60;
/// A web session. Signing in again is one signature or one code.
pub const SESSION_TTL: u64 = 12 * 60 * 60;

pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/v1/device/start", post(device_start))
        .route("/v1/device/token", post(device_token))
        .route("/v1/device/lookup", get(device_lookup))
        .route("/v1/auth/challenge", post(challenge))
        .route("/v1/auth/verify", post(verify))
        .route("/v1/auth/email/start", post(email_start))
        .route("/v1/auth/email/verify", post(email_verify))
        .route("/v1/auth/logout", post(logout))
}

/// `Set-Cookie` for a session token, or to clear it (`None`).
pub fn session_cookie(token: Option<&str>) -> HeaderValue {
    let v = match token {
        Some(t) => format!("{SESSION_COOKIE}={t}; Path=/; Max-Age={SESSION_TTL}; HttpOnly; Secure; SameSite=Strict"),
        None => format!("{SESSION_COOKIE}=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Strict"),
    };
    HeaderValue::from_str(&v).expect("token is hex")
}

fn code_gone() -> Response {
    error(StatusCode::NOT_FOUND, "That code is unknown, used or expired. Start sign-in again in RamWarden.")
}

/// A typed device code that is still waiting, normalised.
async fn pending_code(c: &Cloud, raw: Option<&str>, now: u64) -> Result<Option<String>, Response> {
    let Some(raw) = raw.filter(|s| !s.trim().is_empty()) else { return Ok(None) };
    let code = normalise_user_code(raw).ok_or_else(code_gone)?;
    c.store.pending_device_code(&code, now).await.map_err(storage)?.ok_or_else(code_gone)?;
    Ok(Some(code))
}

/// What a sign-in returns: the account, the device it linked, and the session
/// as a cookie (or in the body for a tool that asked for a bearer).
async fn signed_in(c: &Cloud, account: Account, user_code: Option<&str>, bearer: bool, now: u64) -> Reply {
    let mut linked = Value::Null;
    if let Some(code) = user_code {
        let dc = c.store.pending_device_code(code, now).await.map_err(storage)?;
        let Some(dc) = dc else {
            return Err(error(StatusCode::GONE, "The device code expired while you were signing in. Start sign-in again in RamWarden."));
        };
        // Refused here, on the page the person is looking at, rather than
        // when the daemon claims the code.
        if at_device_limit(c, &account, now).await? {
            return Err(device_limit_reached(device_limit(&account, now)));
        }
        if !c.store.approve_device_code(code, &account.id, now).await.map_err(storage)? {
            return Err(error(StatusCode::GONE, "The device code expired while you were signing in. Start sign-in again in RamWarden."));
        }
        linked = json!({"name": dc.device_name, "platform": dc.platform});
    }
    let token = c.store.create_session(&account.id, now, SESSION_TTL).await.map_err(storage)?;
    let mut body = json!({"account": account_json(c, &account, now).await?, "linked_device": linked});
    if bearer {
        body["session"] = json!(token);
        return Ok(Json(body).into_response());
    }
    let mut r = Json(body).into_response();
    r.headers_mut().insert(header::SET_COOKIE, session_cookie(Some(&token)));
    Ok(r)
}

// ------------------------------------------------------------- device flow

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
    /// The app names itself when it claims an install link, which was
    /// issued before anyone knew which machine would run it.
    #[serde(default)]
    device_name: Option<String>,
    #[serde(default)]
    platform: Option<Platform>,
}

/// The daemon's poll. Error codes are RFC 8628's (plus `device_limit`),
/// since only the daemon reads them.
async fn device_token(State(state): State<Arc<AppState>>, Json(b): Json<DeviceToken>) -> Reply {
    let c = cloud(&state)?;
    let now = (state.now)();
    match c.store.claim_device_code(&b.device_code, now).await.map_err(storage)? {
        Claim::Pending => Err(error(StatusCode::BAD_REQUEST, "authorization_pending")),
        Claim::Gone => Err(error(StatusCode::BAD_REQUEST, "expired_token")),
        Claim::Approved { account_id, device_name, platform } => {
            let named: Option<String> = b
                .device_name
                .map(|n| n.trim().chars().filter(|ch| !ch.is_control()).take(64).collect())
                .filter(|n: &String| !n.is_empty());
            let device_name = named.unwrap_or(device_name);
            let platform = b.platform.unwrap_or(platform);
            let account = c.store.account(&account_id).await.map_err(storage)?.ok_or_else(|| storage("account vanished"))?;
            if at_device_limit(c, &account, now).await? {
                return Err(error(StatusCode::FORBIDDEN, "device_limit"));
            }
            let Some((d, token)) =
                c.store.register_device(&account_id, &device_name, platform, now).await.map_err(storage)?
            else {
                return Err(error(StatusCode::FORBIDDEN, "device_limit"));
            };
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
    let code = normalise_user_code(&q.user_code).ok_or_else(code_gone)?;
    let dc = c.store.pending_device_code(&code, (state.now)()).await.map_err(storage)?.ok_or_else(code_gone)?;
    Ok(Json(json!({
        "status": "pending",
        "device_name": dc.device_name,
        "platform": dc.platform,
        "expires_at": iso8601(dc.expires_at),
    }))
    .into_response())
}

// ------------------------------------------------------------- wallet

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
    let address = wallet::normalise(b.chain, &b.address).map_err(|e| error(StatusCode::UNPROCESSABLE_ENTITY, &e))?;
    let user_code = pending_code(c, b.user_code.as_deref(), now).await?;
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

/// `"session": "bearer"` in a verify body returns the session token in the
/// reply instead of setting the cookie.
fn wants_bearer(s: &Option<String>) -> bool {
    s.as_deref() == Some("bearer")
}

#[derive(Deserialize)]
struct VerifyReq {
    chain: Chain,
    address: String,
    message: String,
    signature: String,
    #[serde(default)]
    session: Option<String>,
}

async fn verify(State(state): State<Arc<AppState>>, Json(b): Json<VerifyReq>) -> Reply {
    let c = cloud(&state)?;
    let now = (state.now)();
    let stale = || error(StatusCode::UNAUTHORIZED, "That sign-in request expired or was already used. Try again.");
    let nonce = wallet::nonce_of(&b.message).ok_or_else(stale)?;
    // Taken before checking anything, so a wrong signature also uses it up.
    let ch = c.store.take_challenge(nonce, now).await.map_err(storage)?.ok_or_else(stale)?;
    let address = wallet::normalise(b.chain, &b.address).map_err(|e| error(StatusCode::UNPROCESSABLE_ENTITY, &e))?;
    if ch.chain != b.chain || ch.address != address || ch.message != b.message {
        return Err(error(StatusCode::UNAUTHORIZED, "The signed message is not the one this server issued."));
    }
    wallet::verify(b.chain, &address, &b.message, &b.signature)
        .map_err(|_| error(StatusCode::UNAUTHORIZED, "The signature did not match the wallet. Try again."))?;

    let identity = Identity::Wallet { chain: b.chain, address };
    let (account, created) = c.store.account_for_identity(&identity, now).await.map_err(storage)?;
    if created {
        tracing::info!(account = %account.id, "account created (wallet)");
    }
    signed_in(c, account, ch.user_code.as_deref(), wants_bearer(&b.session), now).await
}

// ------------------------------------------------------------- email

/// Enough to refuse typos and header tricks; the code proves the rest.
pub fn plausible_email(e: &str) -> bool {
    let Some((local, domain)) = e.split_once('@') else { return false };
    e.len() <= 254
        && !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && e.bytes().all(|b| b.is_ascii_graphic() && !matches!(b, b'<' | b'>' | b',' | b';' | b'"'))
}

#[derive(Deserialize)]
struct EmailStart {
    email: String,
    #[serde(default)]
    user_code: Option<String>,
}

async fn email_start(State(state): State<Arc<AppState>>, Json(b): Json<EmailStart>) -> Reply {
    let c = cloud(&state)?;
    let mailer = c
        .mailer
        .as_ref()
        .ok_or_else(|| error(StatusCode::SERVICE_UNAVAILABLE, "Email sign-in opens soon. Sign in with a wallet for now."))?;
    let now = (state.now)();
    let Identity::Email { email } = Identity::email(&b.email) else { unreachable!() };
    if !plausible_email(&email) {
        return Err(error(StatusCode::UNPROCESSABLE_ENTITY, "That doesn't look like an email address."));
    }
    let user_code = pending_code(c, b.user_code.as_deref(), now).await?;
    let code = email_code();
    if !c.store.start_email_code(&email, &code, user_code.as_deref(), now, EMAIL_CODE_TTL).await.map_err(storage)? {
        return Err(error(StatusCode::TOO_MANY_REQUESTS, "A code was sent less than a minute ago. Check your inbox, or wait a minute."));
    }
    mailer.send_code(&email, &code).await.map_err(|e| {
        tracing::error!("mail: {e}");
        error(StatusCode::BAD_GATEWAY, "The code could not be sent. Try again in a minute.")
    })?;
    Ok(Json(json!({"sent": true, "expires_at": iso8601(now + EMAIL_CODE_TTL)})).into_response())
}

#[derive(Deserialize)]
struct EmailVerify {
    email: String,
    code: String,
    #[serde(default)]
    session: Option<String>,
}

async fn email_verify(State(state): State<Arc<AppState>>, Json(b): Json<EmailVerify>) -> Reply {
    let c = cloud(&state)?;
    if c.mailer.is_none() {
        return Err(error(StatusCode::SERVICE_UNAVAILABLE, "Email sign-in opens soon. Sign in with a wallet for now."));
    }
    let now = (state.now)();
    let identity = Identity::email(&b.email);
    let Identity::Email { email } = &identity else { unreachable!() };
    let code: String = b.code.chars().filter(char::is_ascii_digit).collect();
    let user_code = match c.store.check_email_code(email, &code, now).await.map_err(storage)? {
        EmailCheck::Ok { user_code } => user_code,
        EmailCheck::Wrong => return Err(error(StatusCode::UNAUTHORIZED, "That code is not right. Check the latest email.")),
        EmailCheck::Gone => {
            return Err(error(StatusCode::UNAUTHORIZED, "That code expired or was used too many times. Send a new one."));
        }
    };
    let (account, created) = c.store.account_for_identity(&identity, now).await.map_err(storage)?;
    if created {
        tracing::info!(account = %account.id, "account created (email)");
    }
    signed_in(c, account, user_code.as_deref(), wants_bearer(&b.session), now).await
}

// ------------------------------------------------------------- sign out

async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Reply {
    let c = cloud(&state)?;
    if let Some(raw) = session_token(&headers) {
        c.store.end_session(raw).await.map_err(storage)?;
    }
    let mut r = Json(json!({"ok": true})).into_response();
    r.headers_mut().insert(header::SET_COOKIE, session_cookie(None));
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_shapes() {
        for ok in ["a@b.co", "ethan.r+rw@example.com"] {
            assert!(plausible_email(ok), "{ok}");
        }
        for bad in ["", "a", "a@b", "@b.co", "a@.co", "a@b.co.", "a b@c.de", "a@b.co\r\nBcc: x@y.z", "<a@b.co>", "a@b@c.co"] {
            assert!(!plausible_email(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_cookie_is_http_only_strict_and_secure() {
        let c = session_cookie(Some("rwse_ab"));
        let s = c.to_str().unwrap();
        for part in ["rw_session=rwse_ab", "HttpOnly", "Secure", "SameSite=Strict", "Path=/"] {
            assert!(s.contains(part), "{s}");
        }
        assert!(session_cookie(None).to_str().unwrap().contains("Max-Age=0"));
    }
}
