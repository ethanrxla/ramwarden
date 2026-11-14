//! Hosted, device-scoped Wazuh access. A hostname never authorizes a query.
#![allow(clippy::result_large_err)]
use crate::{
    accounts::{Reply, cloud, device, storage},
    routes::{AppState, error},
};
use axum::{
    Json, Router,
    extract::{State,Path,Query},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use ramwarden_ai::secret::Secret;
use ramwarden_cloud::model::{Account, Device, Platform, SecurityBinding};
use ramwarden_cloud::store::hash;
use ramwarden_wazuh::{Client, Endpoints, cloud::Package, enrollment::EnrollmentClient};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct ThreatDesk {
    client: Option<Client>,
    enroller: Option<EnrollmentClient>,
    manager: String,
    packages: HashMap<String, Package>,
    cache: Mutex<HashMap<String, (Instant, Option<Value>)>>,
    enroll_attempts: Mutex<HashMap<String, Instant>>,
}
impl ThreatDesk {
    #[cfg(test)]
    pub(crate) fn testing(base: &str) -> Self {
        let ep = Endpoints {
            manager_url: base.into(),
            indexer_url: base.into(),
            ..Default::default()
        };
        Self {
            client: Some(Client::new(ep.clone()).unwrap()),
            enroller:Some(EnrollmentClient::new(ep).unwrap()),
            manager:"manager.test".into(),
            packages:HashMap::from([("linux-x86_64".into(),Package{url:"https://packages.wazuh.com/4.x/apt/pool/main/w/wazuh-agent/wazuh-agent_4.14.9-1_amd64.deb".into(),sha256:"a".repeat(64),kind:"deb".into()})]),
            ..Default::default()
        }
    }
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let env = |key: &str| std::env::var(key).unwrap_or_default();
        if env("WAZUH_MANAGER_URL").is_empty() {
            return Ok(Self::default());
        }
        let ep = Endpoints {
            manager_url: env("WAZUH_MANAGER_URL"),
            api_user: env("WAZUH_API_USER"),
            api_password: Secret::new(env("WAZUH_API_PASSWORD")),
            indexer_url: env("WAZUH_INDEXER_URL"),
            indexer_user: env("WAZUH_INDEXER_USER"),
            indexer_password: Secret::new(env("WAZUH_INDEXER_PASSWORD")),
            ca_pem: std::env::var("WAZUH_CA_FILE")
                .ok()
                .map(std::fs::read)
                .transpose()?,
            timeout: Duration::from_secs(8),
        };
        let packages: HashMap<String, Package> = match std::env::var("WAZUH_PACKAGES_FILE") {
            Ok(path) => serde_json::from_slice(&std::fs::read(path)?)?,
            Err(_) => HashMap::new(),
        };
        for package in packages.values() {
            let url = reqwest::Url::parse(&package.url)?;
            if url.scheme() != "https"
                || url.host_str() != Some("packages.wazuh.com")
                || !url.username().is_empty()
                || url.password().is_some()
                || package.sha256.len() != 64
                || !package.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err("Invalid official Wazuh package pin".into());
            }
        }
        Ok(Self {
            client: Some(Client::new(ep.clone())?),
            enroller: Some(EnrollmentClient::new(ep)?),
            manager: env("WAZUH_AGENT_HOST"),
            packages,
            cache: Default::default(),
            enroll_attempts: Default::default(),
        })
    }
}
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/wardens/threat/status", get(status))
        .route("/v1/wardens/threat/events/{id}",get(event))
        .route("/v1/wardens/threat/events",get(events))
        .route("/v1/wardens/threat/enroll", post(enroll))
}
async fn authorized(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(Device, Account), axum::response::Response> {
    let c = cloud(state)?;
    let now = (state.now)();
    let (d, a) = device(c, headers, now).await?;
    if !a.active("threat", now) {
        return Err(error(
            StatusCode::FORBIDDEN,
            "ThreatWarden needs an active Pro pass.",
        ));
    }
    Ok((d, a))
}
async fn status(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Reply {
    let (d, _) = authorized(&state, &headers).await?;
    let c = cloud(&state)?;
    let desk = &c.threat;
    let client = desk.client.as_ref().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "ThreatWarden reporting is not configured.",
        )
    })?;
    let Some(binding) = &d.security else {
        return Ok(Json(json!({"agents":[],"alerts":[],"summary":"This computer has not been enrolled with ThreatWarden yet."})).into_response());
    };
    {
        let mut cache = desk.cache.lock().unwrap();
        if let Some((at, value)) = cache.get(&d.id)
            && at.elapsed() < Duration::from_secs(30)
        {
            return match value {
                Some(v) => Ok(([("Cache-Control", "no-store")], Json(v.clone())).into_response()),
                None => Err(error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Security report is being refreshed; retry shortly.",
                )),
            };
        }
        cache.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(60));
        if cache.len() >= 1024 {
            return Err(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Security reporting is busy.",
            ));
        }
        cache.insert(d.id.clone(), (Instant::now(), None));
    }
    let result=async{
        let agent=client.agent(&binding.agent_name).await.map_err(|_|error(StatusCode::BAD_GATEWAY,"The security manager is unavailable."))?;
        if agent.as_ref().is_some_and(|a|a.id!=binding.agent_id){return Err(error(StatusCode::BAD_GATEWAY,"Security agent binding could not be verified."));}
        if !client.has_indexer(){return Err(error(StatusCode::SERVICE_UNAVAILABLE,"Security alert reporting is not configured."));}
        let alerts=client.alerts_for_agent(&binding.agent_name,&binding.agent_id,7,24).await.map_err(|_|error(StatusCode::BAD_GATEWAY,"The security alert index is unavailable."))?;
        let (current,_)=authorized(&state,&headers).await?;if current.id!=d.id || current.security!=d.security{return Err(error(StatusCode::FORBIDDEN,"Device access changed during reporting."));}
        Ok(json!({"agents":agent.into_iter().collect::<Vec<_>>(),"alerts":alerts,"summary":format!("Showing {} recent alerts at level 7 or above from the last 24 hours (maximum 50).",alerts.len())}))
    }.await;
    let mut cache = desk.cache.lock().unwrap();
    match result {
        Ok(v) => {
            cache.insert(d.id, (Instant::now(), Some(v.clone())));
            Ok(([("Cache-Control", "no-store")], Json(v)).into_response())
        }
        Err(e) => {
            cache.insert(d.id, (Instant::now() - Duration::from_secs(25), None));
            Err(e)
        }
    }
}
#[derive(Deserialize,Default)]
#[serde(deny_unknown_fields)]
struct EventSearch {rule:Option<String>,group:Option<String>,hours:Option<u32>,before:Option<String>}
async fn events(State(state):State<Arc<AppState>>,headers:HeaderMap,Query(query):Query<EventSearch>)->Reply {
    if query.rule.as_ref().is_some_and(|v|v.is_empty() || v.len()>20 || !v.bytes().all(|b|b.is_ascii_digit()))
        || query.group.as_ref().is_some_and(|v|v.is_empty() || v.len()>64 || !v.bytes().all(|b|b.is_ascii_alphanumeric() || b==b'_' || b==b'-'))
        || query.before.as_ref().is_some_and(|v|v.len()>40 || v.is_empty() || !v.chars().all(|c|c.is_ascii_digit() || "TZtz:.-+".contains(c))){return Err(error(StatusCode::BAD_REQUEST,"Invalid evidence search filter."));}
    let (device,_)=authorized(&state,&headers).await?;
    let binding=device.security.as_ref().ok_or_else(||error(StatusCode::NOT_FOUND,"This computer has no security binding."))?;
    let client=cloud(&state)?.threat.client.as_ref().ok_or_else(||error(StatusCode::SERVICE_UNAVAILABLE,"Evidence reporting is not configured."))?;
    let hours=query.hours.unwrap_or(24).clamp(1,720);
    let records=client.events(&binding.agent_name,&binding.agent_id,query.rule.as_deref(),query.group.as_deref(),hours,query.before.as_deref()).await.map_err(|_|error(StatusCode::BAD_GATEWAY,"Evidence search is unavailable."))?;
    let (current,_)=authorized(&state,&headers).await?;if current.id!=device.id || current.security!=device.security{return Err(error(StatusCode::FORBIDDEN,"Device access changed during search."));}
    Ok(([("Cache-Control","no-store")],Json(json!({"events":records,"limit":50,"hours":hours,"read_only":true,"pagination":"Use the oldest returned timestamp as before for the next page"}))).into_response())
}

async fn event(State(state):State<Arc<AppState>>,headers:HeaderMap,Path(id):Path<String>)->Reply {
    static GATE:std::sync::OnceLock<tokio::sync::Semaphore>=std::sync::OnceLock::new();
    let _permit=GATE.get_or_init(||tokio::sync::Semaphore::new(4)).try_acquire().map_err(|_|error(StatusCode::TOO_MANY_REQUESTS,"Evidence retrieval is busy."))?;
    let (device,_)=authorized(&state,&headers).await?;
    let binding=device.security.as_ref().ok_or_else(||error(StatusCode::NOT_FOUND,"This computer has no security binding."))?;
    if !ramwarden_wazuh::evidence::valid_event_id(&id){return Err(error(StatusCode::BAD_REQUEST,"Invalid event reference."));}
    let client=cloud(&state)?.threat.client.as_ref().ok_or_else(||error(StatusCode::SERVICE_UNAVAILABLE,"Evidence reporting is not configured."))?;
    let record=client.event(&binding.agent_name,&binding.agent_id,&id).await.map_err(|_|error(StatusCode::BAD_GATEWAY,"The source event could not be retrieved within its response limit."))?.ok_or_else(||error(StatusCode::NOT_FOUND,"No matching event belongs to this computer."))?;
    let (current,_)=authorized(&state,&headers).await?;
    if current.id!=device.id || current.security!=device.security {return Err(error(StatusCode::FORBIDDEN,"Device access changed during evidence retrieval."));}
    Ok(([("Cache-Control","no-store")],Json(record)).into_response())
}

#[derive(Deserialize)]
struct EnrollRequest {
    os: String,
    arch: String,
    #[serde(default)]
    hostname: String,
}
async fn enroll(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<EnrollRequest>,
) -> Reply {
    let (d, a) = authorized(&state, &headers).await?;
    let c = cloud(&state)?;
    let desk = &c.threat;
    let expected = match d.platform {
        Platform::Linux => "linux",
        Platform::Macos => "macos",
        Platform::Windows => "windows",
    };
    if req.os != expected
        || req.hostname.len() > 253
        || !["aarch64", "x86_64"].contains(&req.arch.as_str())
    {
        return Err(error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Platform does not match the signed-in device.",
        ));
    }
    let package = desk
        .packages
        .get(&format!("{}-{}", req.os, req.arch))
        .ok_or_else(|| {
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "A verified agent package is not configured for this platform.",
            )
        })?;
    let enroller = desk.enroller.as_ref().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Security enrollment is not configured.",
        )
    })?;
    {
        let mut attempts = desk.enroll_attempts.lock().unwrap();
        attempts.retain(|_, at| at.elapsed() < Duration::from_secs(30));
        if attempts.contains_key(&d.id) {
            return Err(error(
                StatusCode::TOO_MANY_REQUESTS,
                "Enrollment was just requested. Retry in 30 seconds.",
            ));
        }
        if attempts.len() >= 1024 {
            return Err(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Enrollment is busy.",
            ));
        }
        attempts.insert(d.id.clone(), Instant::now());
    }

    let agent_name = format!("rw-{}", &hash(&format!("{}:{}", a.id, d.id))[..32]);
    let group = format!("acct-{}", &hash(&a.id)[..24]);
    if d.security
        .as_ref()
        .is_some_and(|b| b.agent_name != agent_name || b.group != group)
    {
        return Err(error(
            StatusCode::CONFLICT,
            "This device already has an operator-managed security enrollment.",
        ));
    }
    let (agent_id, key) = enroller.enroll(&agent_name, &group).await.map_err(|_| {
        error(
            StatusCode::BAD_GATEWAY,
            "Security enrollment is incomplete. Retrying will reuse the same agent.",
        )
    })?;
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&key)
        .map_err(|_| error(StatusCode::BAD_GATEWAY, "Invalid agent enrollment key."))?;
    if std::str::from_utf8(&decoded)
        .ok()
        .and_then(|s| s.split_whitespace().next())
        != Some(agent_id.as_str())
    {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "Enrollment key does not match the bound agent.",
        ));
    }
    let binding = SecurityBinding {
        agent_id,
        agent_name: agent_name.clone(),
        group: group.clone(),
    };
    if !c
        .store
        .bind_security(&a.id, &d.id, &binding)
        .await
        .map_err(storage)?
    {
        return Err(error(
            StatusCode::CONFLICT,
            "Security enrollment binding changed. Contact support.",
        ));
    }
    let body = json!({"manager":desk.manager,"agent_name":agent_name,"group":group,"agent_key":key,"package":{"url":package.url,"sha256":package.sha256,"kind":package.kind}});
    // Reuse the installer's strict key/hostname/package validation before returning secrets.
    let parsed: ramwarden_wazuh::Enrollment =
        serde_json::from_value(body.clone()).map_err(|_| {
            error(
                StatusCode::BAD_GATEWAY,
                "Invalid security enrollment response.",
            )
        })?;
    ramwarden_wazuh::install::validate(&parsed).map_err(|_| {
        error(
            StatusCode::BAD_GATEWAY,
            "Security enrollment validation failed.",
        )
    })?;
    Ok(([("Cache-Control", "no-store")], Json(body)).into_response())
}
