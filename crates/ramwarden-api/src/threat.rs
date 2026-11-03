//! Hosted, read-only ThreatWarden status. Account ownership comes exclusively
//! from the device token and server-verified bindings, never a request parameter.

// Match the account handlers: an error response is constructed only on refusal.
#![allow(clippy::result_large_err)]

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use ramwarden_platform::Secret;
use ramwarden_wazuh::{Client, CloudStatus, Endpoints};

use crate::accounts::{self, Reply};
use crate::config::hash_token;
use crate::limits::Limits;
use crate::routes::{AppState, error};

/// Operator configuration only. Passwords never appear in Debug output.
#[derive(Clone)]
pub struct Config {
    pub deployment: String,
    pub endpoints: Endpoints,
    pub ca_file: Option<String>,
}

impl Config {
    pub fn from_lookup(get: &dyn Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        const KEYS: [&str; 7] = [
            "RAMWARDEN_WAZUH_DEPLOYMENT",
            "RAMWARDEN_WAZUH_MANAGER_URL",
            "RAMWARDEN_WAZUH_API_USER",
            "RAMWARDEN_WAZUH_API_PASSWORD",
            "RAMWARDEN_WAZUH_INDEXER_URL",
            "RAMWARDEN_WAZUH_INDEXER_USER",
            "RAMWARDEN_WAZUH_INDEXER_PASSWORD",
        ];
        let values: Vec<_> = KEYS.iter().map(|k| get(k)).collect();
        let ca_file = get("RAMWARDEN_WAZUH_CA_FILE");
        if values.iter().all(Option::is_none) && ca_file.is_none() {
            return Ok(None);
        }
        for (key, value) in KEYS.iter().zip(&values) {
            if value.as_ref().is_none_or(|v| v.trim().is_empty()) {
                return Err(format!("{key} is required for hosted ThreatWarden"));
            }
        }
        let value = |i: usize| values[i].as_ref().unwrap().clone();
        let deployment = value(0);
        if deployment.len() > 64
            || !deployment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Err(
                "RAMWARDEN_WAZUH_DEPLOYMENT must be a stable identifier (letters, digits, - or _)"
                    .into(),
            );
        }
        for i in [1, 4] {
            let valid = reqwest::Url::parse(&value(i)).is_ok_and(|u| {
                crate::config::safe_service_url(&u) && u.query().is_none() && u.fragment().is_none()
            });
            if !valid {
                return Err(format!(
                    "{} must use HTTPS (HTTP is allowed only on loopback), with no credentials, query or fragment",
                    KEYS[i]
                ));
            }
        }
        Ok(Some(Self {
            deployment,
            endpoints: Endpoints {
                manager_url: value(1),
                api_user: value(2),
                api_password: Secret::new(value(3)),
                indexer_url: value(4),
                indexer_user: value(5),
                indexer_password: Secret::new(value(6)),
                ca_pem: None,
                timeout: Duration::from_secs(10),
            },
            ca_file,
        }))
    }
}

pub struct ThreatService {
    deployment: String,
    client: Client,
    // Independent of model quotas. Five devices may each poll every 30 s.
    limits: Limits,
}

impl ThreatService {
    pub fn new(mut cfg: Config) -> Result<Self, String> {
        if let Some(path) = &cfg.ca_file {
            cfg.endpoints.ca_pem =
                Some(std::fs::read(path).map_err(|_| "Cannot read RAMWARDEN_WAZUH_CA_FILE")?);
        }
        let client = Client::new(cfg.endpoints)
            .map_err(|_| "Invalid hosted Wazuh endpoints or CA certificate")?;
        if !client.has_manager() || !client.has_indexer() {
            return Err("Hosted ThreatWarden needs both the manager and indexer".into());
        }
        Ok(Self {
            deployment: cfg.deployment,
            client,
            limits: Limits::new(30, u32::MAX, u64::MAX),
        })
    }
}

pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/v1/wardens/threat/status", get(status))
}

async fn agent_ids(
    c: &accounts::Cloud,
    account_id: &str,
    deployment: &str,
) -> Result<Vec<String>, axum::response::Response> {
    let bindings = c
        .store
        .threat_agents(account_id)
        .await
        .map_err(accounts::storage)?;
    let mut ids: Vec<_> = bindings
        .into_iter()
        .filter(|b| b.deployment == deployment)
        .map(|b| b.agent_id)
        .collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

async fn status(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Reply {
    let c = accounts::cloud(&state)?;
    let (_, account) = accounts::device(c, &headers, (state.now)()).await?;
    if !account.active("threat", (state.now)()) {
        return Err(error(
            StatusCode::PAYMENT_REQUIRED,
            "ThreatWarden requires an active RamWarden Pro plan.",
        ));
    }
    let service = c.threat.as_ref().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Hosted ThreatWarden is not configured yet.",
        )
    })?;
    if service
        .limits
        .take(&hash_token(&account.id), (state.now)())
        .is_err()
    {
        return Err((StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "60")],
            Json(serde_json::json!({"error": "Too many security status requests. Try again in a minute."}))).into_response());
    }
    let ids = agent_ids(c, &account.id, &service.deployment).await?;
    let result = if ids.is_empty() {
        CloudStatus {
            summary: Some("No security agents are linked to this account yet.".into()),
            ..Default::default()
        }
    } else {
        let unavailable = |_| {
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Security status is temporarily unavailable. Protection has not been confirmed.",
            )
        };
        let agents = service
            .client
            .agents_by_ids(&ids)
            .await
            .map_err(unavailable)?;
        if agents.len() != ids.len() {
            return Err(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "A linked security agent could not be found. Protection has not been confirmed.",
            ));
        }
        let alerts = service
            .client
            .alerts_by_ids(&ids, 7, 24)
            .await
            .map_err(unavailable)?;
        // Never call this an AI verdict or imply that absence of alerts proves safety.
        CloudStatus { agents, alerts, summary: Some("Wazuh alerts from the past 24 hours, severity 7 and above. AI review is not available yet.".into()) }
    };
    // A revocation or plan change during upstream I/O must not release cached access.
    let (_, current) = accounts::device(c, &headers, (state.now)()).await?;
    if current.id != account.id || !current.active("threat", (state.now)()) {
        return Err(error(
            StatusCode::PAYMENT_REQUIRED,
            "ThreatWarden access has changed. Refresh your account.",
        ));
    }
    if agent_ids(c, &current.id, &service.deployment).await? != ids {
        return Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Linked security devices changed. Try again.",
        ));
    }
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(result)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_is_all_or_nothing_and_rejects_unsafe_urls() {
        let mut vars = std::collections::HashMap::new();
        assert!(
            Config::from_lookup(&|k| vars.get(k).cloned())
                .unwrap()
                .is_none()
        );
        vars.insert("RAMWARDEN_WAZUH_DEPLOYMENT", "test".to_string());
        assert!(Config::from_lookup(&|k| vars.get(k).cloned()).is_err());
        for (key, value) in [
            ("RAMWARDEN_WAZUH_MANAGER_URL", "https://wazuh.example:55000"),
            ("RAMWARDEN_WAZUH_API_USER", "readonly"),
            ("RAMWARDEN_WAZUH_API_PASSWORD", "manager-secret"),
            ("RAMWARDEN_WAZUH_INDEXER_URL", "https://wazuh.example:9200"),
            ("RAMWARDEN_WAZUH_INDEXER_USER", "readonly"),
            ("RAMWARDEN_WAZUH_INDEXER_PASSWORD", "indexer-secret"),
        ] {
            vars.insert(key, value.to_string());
        }
        assert!(
            Config::from_lookup(&|k| vars.get(k).cloned())
                .unwrap()
                .is_some()
        );
        for bad in [
            "http://wazuh.example:55000",
            "https://user:manager-secret@wazuh.example",
            "https://wazuh.example?secret=x",
            "https://wazuh.example#fragment",
        ] {
            vars.insert("RAMWARDEN_WAZUH_MANAGER_URL", bad.to_string());
            let err = Config::from_lookup(&|k| vars.get(k).cloned())
                .err()
                .unwrap();
            assert!(!err.contains("manager-secret"));
        }
    }
}
