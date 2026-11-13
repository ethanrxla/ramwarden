//! Sending the one-time sign-in code for email accounts.
//!
//! Production sends through Resend's HTTP API (`RESEND_API_KEY`,
//! `RAMWARDEN_MAIL_FROM`), chosen because it needs one key and no SMTP
//! server. Without a key, email sign-in is off unless the operator asks for
//! [`LogMailer`], which writes codes to the server log for local testing and
//! must never run where real people sign in.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use ramwarden_ai::secret::Secret;
use serde_json::json;

#[async_trait]
pub trait Mailer: Send + Sync {
    async fn send_code(&self, to: &str, code: &str) -> Result<(), String>;
}

pub fn subject() -> &'static str {
    "Your RamWarden sign-in code"
}

pub fn body(code: &str) -> String {
    format!(
        "Your RamWarden sign-in code is {code}.\n\n\
         It works once, for 10 minutes. If you didn't ask for it, ignore this email: \
         nobody can sign in without the code."
    )
}

pub struct ResendMailer {
    key: Secret,
    from: String,
    http: reqwest::Client,
}

impl ResendMailer {
    pub fn new(key: Secret, from: String) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().unwrap_or_default();
        ResendMailer { key, from, http }
    }
}

#[async_trait]
impl Mailer for ResendMailer {
    async fn send_code(&self, to: &str, code: &str) -> Result<(), String> {
        let r = self
            .http
            .post("https://api.resend.com/emails")
            .bearer_auth(self.key.expose())
            .json(&json!({"from": self.from, "to": [to], "subject": subject(), "text": body(code)}))
            .send()
            .await
            .map_err(|e| format!("resend: {e}"))?;
        if r.status().is_success() {
            Ok(())
        } else {
            Err(format!("resend answered {}", r.status()))
        }
    }
}

/// Development only: logs the code instead of sending it.
pub struct LogMailer;

#[async_trait]
impl Mailer for LogMailer {
    async fn send_code(&self, to: &str, code: &str) -> Result<(), String> {
        tracing::warn!("development mailer: sign-in code for {to} is {code}");
        Ok(())
    }
}

/// Tests: keeps what was sent.
#[derive(Default)]
pub struct MemoryMailer {
    pub sent: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl Mailer for MemoryMailer {
    async fn send_code(&self, to: &str, code: &str) -> Result<(), String> {
        self.sent.lock().unwrap_or_else(|p| p.into_inner()).push((to.to_string(), code.to_string()));
        Ok(())
    }
}

impl MemoryMailer {
    pub fn last_code(&self, to: &str) -> Option<String> {
        let sent = self.sent.lock().unwrap_or_else(|p| p.into_inner());
        sent.iter().rev().find(|(t, _)| t == to).map(|(_, c)| c.clone())
    }
}
