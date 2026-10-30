//! The call to TypeSafe.
//!
//! The reply is passed back whole rather than through
//! [`ramwarden_ai::jev::parse_response`], which keeps only `noul` answers: a
//! daemon that asks a `choice` question must get its answer, and the server
//! should not need a release every time the daemon learns a new question type.

use std::time::Duration;

use ramwarden_ai::secret::Secret;
use serde_json::{Map, Value, json};

#[derive(Debug)]
pub enum UpstreamError {
    /// Could not reach Jev, or it timed out.
    Unreachable(String),
    /// Jev answered with a non-success status. The body is kept for the server's
    /// log only; it is not shown to clients.
    Status(u16, String),
    /// Jev answered 2xx with something that is not a JSON object.
    Reply(String),
}

pub struct Upstream {
    http: reqwest::Client,
    base_url: String,
    model: String,
    key: Secret,
}

impl Upstream {
    pub fn new(base_url: &str, model: &str, key: Secret, timeout: Duration) -> Result<Self, String> {
        let http = reqwest::Client::builder().timeout(timeout).build().map_err(|e| e.to_string())?;
        Ok(Upstream { http, base_url: base_url.trim_end_matches('/').into(), model: model.into(), key })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn key_fingerprint(&self) -> String {
        self.key.fingerprint()
    }

    /// Ask `questions` about `state`. Returns Jev's reply and its input tokens.
    pub async fn ask(&self, state: &str, questions: &Map<String, Value>) -> Result<(Value, u64), UpstreamError> {
        let body = json!({"model": self.model, "state": state, "questions": questions});
        let resp = self
            .http
            .post(format!("{}/v1/systemone", self.base_url))
            .bearer_auth(self.key.expose())
            .json(&body)
            .send()
            .await
            .map_err(|e| UpstreamError::Unreachable(e.to_string()))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| UpstreamError::Unreachable(e.to_string()))?;
        if !status.is_success() {
            return Err(UpstreamError::Status(status.as_u16(), text.chars().take(200).collect()));
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| UpstreamError::Reply(e.to_string()))?;
        if !v.is_object() {
            return Err(UpstreamError::Reply("not a JSON object".into()));
        }
        let input_tokens = v["usage"]["input_tokens"].as_u64().unwrap_or(0);
        Ok((v, input_tokens))
    }
}
