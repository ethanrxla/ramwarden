//! ThreatWarden, hosted: RamWarden's own API runs Wazuh, so a paying user
//! never sees a manager address, a password or a certificate.
//!
//! The daemon authenticates with the device token from device sign-in. The API
//! enrols the computer into the account's own agent group, picks the agent
//! package, and returns alerts already triaged by its AI tier. Tenant
//! isolation is the API's job: it filters by verified agent bindings, never by a
//! name this client sends. See `docs/specs/wardens.md` ("Hosted Wazuh").

use std::time::Duration;

use ramwarden_platform::Secret;
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};

use crate::api::{Alert, ManagerAgent};
use crate::{Error, Result};

/// What the API needs to pick the right agent package and name.
#[derive(Clone, Debug, Serialize)]
pub struct EnrollRequest {
    pub os: String,
    pub arch: String,
    pub hostname: String,
}

impl EnrollRequest {
    /// This computer.
    pub fn here() -> EnrollRequest {
        EnrollRequest {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            hostname: crate::default_agent_name(),
        }
    }
}

/// The agent package to install: Wazuh's own, pinned by hash.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Package {
    pub url: String,
    pub sha256: String,
    /// `msi`, `pkg`, `deb` or `rpm`.
    pub kind: String,
}

/// Everything the agent installer needs.
///
/// The API registers the agent itself (manager API `POST /agents`, then puts
/// it in the account's group) and hands back that one agent's key. There is no
/// shared enrolment password, so one customer can never enrol a machine into
/// another customer's group, and the manager's enrolment port stays closed.
/// The key is the agent's own identity; it is written only into the agent's
/// `client.keys`, where Wazuh keeps it.
#[derive(Clone, Debug, Deserialize)]
pub struct Enrollment {
    pub manager: String,
    pub agent_name: String,
    pub group: String,
    /// Wazuh's agent key: base64 of `"<id> <name> <ip|any> <64 hex>"`.
    pub agent_key: Secret,
    pub package: Package,
}

/// What the hosted API says about this account's agents and alerts.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CloudStatus {
    #[serde(default)]
    pub agents: Vec<ManagerAgent>,
    #[serde(default)]
    pub alerts: Vec<Alert>,
    /// A coverage summary, including whether hosted AI review is available.
    #[serde(default)]
    pub summary: Option<String>,
}

/// A client for the hosted API's Wazuh endpoints.
pub struct Cloud {
    http: reqwest::Client,
    base: Url,
    token: Secret,
}

impl Cloud {
    /// `base` is the hosted API, e.g. `https://api.ramwarden.app`. It must be
    /// https (http only to this computer, for tests and local development).
    pub fn new(base: &str, token: Secret, timeout: Duration) -> Result<Cloud> {
        let base = crate::api::parse_url(base)?.ok_or_else(|| Error::BadUrl(base.into(), "empty".into()))?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(if timeout.is_zero() { Duration::from_secs(15) } else { timeout })
            .build()
            .map_err(|e| Error::Unreachable("cloud", e.to_string()))?;
        Ok(Cloud { http, base, token })
    }

    fn url(&self, path: &str) -> Url {
        crate::api::join(&self.base, path)
    }

    fn check(status: StatusCode) -> Result<()> {
        match status {
            s if s.is_success() => Ok(()),
            StatusCode::UNAUTHORIZED => Err(Error::SignedOut),
            StatusCode::FORBIDDEN | StatusCode::PAYMENT_REQUIRED => Err(Error::NoPass),
            s => Err(Error::Status("cloud", s.as_u16())),
        }
    }

    /// Ask the API to enrol this computer. The answer is validated before it
    /// is returned, because its values end up on an elevated command line.
    pub async fn enroll(&self, req: &EnrollRequest) -> Result<Enrollment> {
        let resp = self
            .http
            .post(self.url("v1/wardens/threat/enroll"))
            .bearer_auth(self.token.expose())
            .json(req)
            .send()
            .await
            .map_err(|e| Error::Unreachable("cloud", e.without_url().to_string()))?;
        Cloud::check(resp.status())?;
        let e: Enrollment = resp.json().await.map_err(|e| Error::Parse("cloud", e.to_string()))?;
        crate::install::validate(&e)?;
        Ok(e)
    }

    /// This account's agents and triaged alerts.
    pub async fn status(&self) -> Result<CloudStatus> {
        let resp = self
            .http
            .get(self.url("v1/wardens/threat/status"))
            .bearer_auth(self.token.expose())
            .send()
            .await
            .map_err(|e| Error::Unreachable("cloud", e.without_url().to_string()))?;
        Cloud::check(resp.status())?;
        let mut s: CloudStatus = resp.json().await.map_err(|e| Error::Parse("cloud", e.to_string()))?;
        s.alerts.truncate(crate::api::MAX_ALERTS);
        Ok(s)
    }
}
