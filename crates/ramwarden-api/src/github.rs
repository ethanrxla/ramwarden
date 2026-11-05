//! GitHub device authorization. No repository scope or shared client secret.
#![allow(clippy::result_large_err)]
use crate::{
    accounts::{Reply, cloud, session, storage},
    routes::{AppState, error},
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use ramwarden_cloud::{model::Identity, store::random_bytes};
use serde::{Deserialize,Serialize};
use aes_gcm::{Aes256Gcm,Nonce,aead::{Aead,KeyInit,Payload}};
use base64::{Engine as _,engine::general_purpose::STANDARD};
use ramwarden_cloud::store::{OAuthState,OAuthClaim};
use serde_json::{Value, json};
use std::{sync::Arc,time::Duration};
#[derive(Serialize,Deserialize)]
struct Pending {
    code: String,
    expires: u64,
    next: u64,
    interval: u64,
    user_code: Option<String>,
    link_account: Option<String>,
    #[serde(default)]
    verified_id: Option<String>,
}
fn cipher()->Result<Aes256Gcm,axum::response::Response> {
    let key=std::env::var("RAMWARDEN_OAUTH_STATE_KEY").ok().and_then(|v|STANDARD.decode(v).ok()).filter(|v|v.len()==32)
        .ok_or_else(||error(StatusCode::SERVICE_UNAVAILABLE,"GitHub sign-in encryption is not configured."))?;
    Aes256Gcm::new_from_slice(&key).map_err(storage)
}
fn seal(p:&Pending,ticket:&str)->Result<OAuthState,axum::response::Response> {
    seal_with(&cipher()?,p,ticket)
}
fn seal_with(cipher:&Aes256Gcm,p:&Pending,ticket:&str)->Result<OAuthState,axum::response::Response> {
    let nonce=random_bytes::<12>();let bytes=serde_json::to_vec(p).map_err(storage)?;
    let encrypted=cipher.encrypt(Nonce::from_slice(&nonce),Payload{msg:&bytes,aad:ticket.as_bytes()}).map_err(|_|storage("OAuth encryption failed"))?;
    Ok(OAuthState{encrypted:STANDARD.encode([nonce.to_vec(),encrypted].concat()),expires_at:p.expires,next_poll:p.next,lease:None})
}
fn unseal(state:&OAuthState,ticket:&str)->Result<Pending,axum::response::Response> {
    unseal_with(&cipher()?,state,ticket)
}
fn unseal_with(cipher:&Aes256Gcm,state:&OAuthState,ticket:&str)->Result<Pending,axum::response::Response> {
    let bytes=STANDARD.decode(&state.encrypted).map_err(storage)?;
    if bytes.len()<28 {return Err(storage("Invalid encrypted OAuth state"))}
    let plain=cipher.decrypt(Nonce::from_slice(&bytes[..12]),Payload{msg:&bytes[12..],aad:ticket.as_bytes()}).map_err(|_|storage("OAuth state verification failed"))?;
    serde_json::from_slice(&plain).map_err(storage)
}
fn client_id() -> Option<String> {
    std::env::var("GITHUB_CLIENT_ID")
        .ok()
        .filter(|v| !v.trim().is_empty())
}
fn client() -> Result<reqwest::Client, axum::response::Response> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("RamWarden")
        .build()
        .map_err(storage)
}
fn origin(c: &crate::accounts::Cloud, h: &HeaderMap) -> Result<(), axum::response::Response> {
    if h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) != Some(c.site.origin.as_str()) {
        return Err(error(
            StatusCode::FORBIDDEN,
            "Open sign-in on the RamWarden website.",
        ));
    }
    Ok(())
}
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/auth/github/config", get(config))
        .route("/v1/auth/github/start", post(start))
        .route("/v1/auth/github/poll", post(poll))
}
async fn config() -> Json<Value> {
    Json(json!({"enabled":client_id().is_some() && cipher().is_ok()}))
}
#[derive(Deserialize)]
struct Start {
    #[serde(default)]
    user_code: Option<String>,
    #[serde(default)]
    link: bool,
}
async fn start(State(st): State<Arc<AppState>>, headers: HeaderMap, Json(b): Json<Start>) -> Reply {
    let c = cloud(&st)?;
    origin(c, &headers)?;
    let id = client_id().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "GitHub sign-in is not configured yet.",
        )
    })?;
    let now = (st.now)();
    let linked = if b.link {
        Some(
            session(c, &axum::http::Method::POST, &headers, now)
                .await?
                .id,
        )
    } else {
        None
    };
    if b.link && b.user_code.is_some() {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "Link a sign-in method separately from a device.",
        ));
    }
    let user_code = crate::signin::pending_code(c, b.user_code.as_deref(), now).await?;
    let _ = cipher()?;
    let r = client()?
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .form(&[("client_id", id.as_str())])
        .send()
        .await
        .map_err(|_| error(StatusCode::BAD_GATEWAY, "GitHub is unavailable."))?;
    if !r.status().is_success() {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "GitHub could not start sign-in.",
        ));
    }
    let v: Value = r.json().await.map_err(storage)?;
    let code = v["device_code"]
        .as_str()
        .ok_or_else(|| error(StatusCode::BAD_GATEWAY, "GitHub returned no device code."))?
        .to_owned();
    let display = v["user_code"]
        .as_str()
        .ok_or_else(|| error(StatusCode::BAD_GATEWAY, "GitHub returned no user code."))?;
    // Do not accept a provider-supplied arbitrary navigation destination.
    if v["verification_uri"].as_str() != Some("https://github.com/login/device") {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "GitHub returned an unexpected approval page.",
        ));
    }
    let ttl = v["expires_in"].as_u64().unwrap_or(600).min(900);
    let interval = v["interval"].as_u64().unwrap_or(5).clamp(5, 60);
    let ticket = hex::encode(random_bytes::<32>());
    let p=Pending{code,expires:now+ttl,next:now+interval,interval,user_code,link_account:linked,verified_id:None};
    c.store.put_oauth(&ticket,seal(&p,&ticket)?,now).await.map_err(storage)?;
    Ok(Json(json!({"ticket":ticket,"user_code":display,"verification_uri":"https://github.com/login/device","expires_in":ttl,"interval":interval})).into_response())
}
#[derive(Deserialize)]
struct Poll {
    ticket: String,
}
async fn poll(State(st): State<Arc<AppState>>, headers: HeaderMap, Json(b): Json<Poll>) -> Reply {
    let c = cloud(&st)?;
    origin(c, &headers)?;
    let now = (st.now)();
    if b.ticket.len() != 64 {
        return Err(error(StatusCode::BAD_REQUEST, "Invalid sign-in ticket."));
    }
    let lease=hex::encode(random_bytes::<32>());
    let state=match c.store.claim_oauth(&b.ticket,now,&lease).await.map_err(storage)? {
        OAuthClaim::Ready(p)=>p,
        OAuthClaim::Wait(seconds)=>return Ok(Json(json!({"pending":true,"interval":seconds})).into_response()),
        OAuthClaim::Gone=>return Err(error(StatusCode::GONE,"GitHub sign-in expired. Start again.")),
    };
    let mut p=unseal(&state,&b.ticket)?;
    let result=poll_claimed(c,&headers,&mut p,now).await;
    let keep=match &result {Ok(response)=>response.status()==StatusCode::ACCEPTED,Err(response)=>response.status().is_server_error()};
    p.next=now+p.interval;
    let replacement=if keep {Some(seal(&p,&b.ticket)?)}else{None};
    if !c.store.finish_oauth(&b.ticket,&lease,replacement).await.map_err(storage)? {
        return Err(error(StatusCode::CONFLICT,"Sign-in changed. Retry approval."));
    }
    match result {
        Ok(response) if response.status()==StatusCode::ACCEPTED=>Ok(Json(json!({"pending":true,"interval":p.interval})).into_response()),
        other=>other,
    }
}
async fn poll_claimed(c:&crate::accounts::Cloud,headers:&HeaderMap,p:&mut Pending,now:u64)->Reply {
    if let Some(account) = &p.link_account
        && session(c, &axum::http::Method::POST, headers, now)
            .await?
            .id
            != *account
    {
        return Err(error(
            StatusCode::FORBIDDEN,
            "Account changed. Start again.",
        ));
    }
    let verified_id=if let Some(id)=&p.verified_id {id.clone()} else {
    let id = client_id().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "GitHub sign-in is unavailable.",
        )
    })?;
    let http = client()?;
    let r = http
        .post("https://github.com/login/oauth/access_token")
        .header("Accept", "application/json")
        .form(&[
            ("client_id", id.as_str()),
            ("device_code", p.code.as_str()),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ])
        .send()
        .await
        .map_err(|_| {
            error(
                StatusCode::BAD_GATEWAY,
                "GitHub is unavailable. Start again.",
            )
        })?;
    let v: Value = r.json().await.map_err(storage)?;
    match v["error"].as_str() {
        Some("authorization_pending") | Some("slow_down") => {
            if v["error"] == "slow_down" {
                p.interval = (p.interval + 5).min(60);
            }
            p.next = now + p.interval;
            let interval = p.interval;
            return Ok((StatusCode::ACCEPTED,Json(json!({"pending":true,"interval":interval}))).into_response());
        }
        Some(_) => {
            return Err(error(
                StatusCode::UNAUTHORIZED,
                "GitHub approval was cancelled or expired.",
            ));
        }
        None => {}
    }
    let token = v["access_token"]
        .as_str()
        .ok_or_else(|| error(StatusCode::BAD_GATEWAY, "GitHub returned no authorization."))?;
    let r = http
        .get("https://api.github.com/user")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| error(StatusCode::BAD_GATEWAY, "Could not verify GitHub account."))?;
    if !r.status().is_success() {
        return Err(error(
            StatusCode::UNAUTHORIZED,
            "GitHub account verification failed.",
        ));
    }
    let user: Value = r.json().await.map_err(storage)?;
    let id = user["id"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or_else(|| error(StatusCode::BAD_GATEWAY, "GitHub returned no account ID."))?;
    p.verified_id=Some(id.to_string());
    id.to_string()
    };
    let identity = Identity::Github { id: verified_id };
    // Never match by email or mutable login name, and never retain the GitHub token.
    let account = if let Some(account) = &p.link_account {
        if !c
            .store
            .link_identity(account, &identity)
            .await
            .map_err(storage)?
        {
            return Err(error(
                StatusCode::CONFLICT,
                "This GitHub account already belongs to another RamWarden account.",
            ));
        }
        c.store
            .account(account)
            .await
            .map_err(storage)?
            .ok_or_else(|| storage("account vanished"))?
    } else {
        c.store
            .account_for_identity(&identity, now)
            .await
            .map_err(storage)?
            .0
    };
    crate::signin::signed_in(c, account, p.user_code.as_deref(), false, now).await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oauth_state_is_encrypted_and_bound_to_ticket() {
        let key=Aes256Gcm::new_from_slice(&[7;32]).unwrap();
        let p=Pending{code:"provider-secret".into(),expires:200,next:105,interval:5,user_code:None,link_account:Some("owner".into()),verified_id:None};
        let state=seal_with(&key,&p,"ticket-a").unwrap();
        assert!(!state.encrypted.contains("provider-secret"));
        assert_eq!(unseal_with(&key,&state,"ticket-a").unwrap().code,"provider-secret");
        assert!(unseal_with(&key,&state,"ticket-b").is_err());
        assert!(unseal_with(&Aes256Gcm::new_from_slice(&[8;32]).unwrap(),&state,"ticket-a").is_err());
    }
    #[test]
    fn tickets_are_unpredictable_and_unique() {
        let a = hex::encode(random_bytes::<32>());
        let b = hex::encode(random_bytes::<32>());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }
    #[test]
    fn github_identity_uses_stable_id() {
        let value = serde_json::to_value(Identity::Github { id: "42".into() }).unwrap();
        assert_eq!(value, json!({"kind":"github","id":"42"}));
        assert_ne!(Identity::Github { id: "42".into() }, Identity::email("42"));
    }
}

#[cfg(test)]
mod flow_tests {
    use crate::flows::{Auth, app};
    use axum::http::StatusCode;
    use serde_json::json;
    #[tokio::test]
    async fn rejects_foreign_origin_before_provider_call() {
        let a = app();
        let (status, _, _) = a
            .raw(
                "POST",
                "/v1/auth/github/start",
                Auth::None,
                "https://evil.invalid",
                Some(json!({})),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    #[tokio::test]
    async fn unknown_poll_cannot_sign_in() {
        let a = app();
        let (status, body) = a
            .call(
                "POST",
                "/v1/auth/github/poll",
                Auth::None,
                Some(json!({"ticket":"0".repeat(64)})),
            )
            .await;
        assert_eq!(status, StatusCode::GONE, "{body}");
    }
}
