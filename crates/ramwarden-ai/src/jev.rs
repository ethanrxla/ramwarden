//! TypeSafe's Jev: a decision model that answers typed questions about a piece
//! of text with calibrated probabilities, instead of generating prose.
//!
//! One request carries a `state` (the text to judge) and a set of named
//! questions: `noul` (yes/no, answered as P(yes)) and `choice` (one of a fixed
//! set of labels, with a probability for each). Verified against the live
//! service:
//!
//! ```text
//! POST {base}/v1/systemone      Authorization: Bearer <key>
//! {"model":"jev-latest","state":"...",
//!  "questions":{"return_24h":{"type":"noul","instructions":"..."}}}
//!
//! {"model":"jev-1.13.0",
//!  "answers":{"return_24h":{"type":"noul","noul":0.07}},
//!  "usage":{"input_tokens":338,"output_tokens":41}}
//! ```
//!
//! `GET {base}/v1/models` lists model names and sends no text, so it doubles as a
//! key check. The versioned model that answered is returned and should be logged,
//! since `jev-latest` moves.
//!
//! The caller decides what goes in `state`. RamWarden sends feature descriptions
//! (site type, idle hours, visit count) and never a URL or a title.
//!
//! Three callers share this client: the tab tier (`tabintel`), the quit tier
//! (`appintel`), and the ladder's judge (`ladderjudge` in the daemon), which
//! asks about the coldest apps on the LRU before a suspend or kill.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Value, json};

use crate::client::{Error, Result};
use crate::secret::Secret;

const SERVICE: &str = "jev";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub name: String,
    pub instructions: String,
    /// `None` for a noul. For a choice, each label and what it means.
    pub criteria: Option<Vec<(String, String)>>,
}

impl Question {
    /// A yes/no question, answered as P(yes).
    pub fn new(name: &str, instructions: &str) -> Self {
        Question { name: name.into(), instructions: instructions.into(), criteria: None }
    }

    /// Pick one of `criteria` (label, meaning).
    pub fn choice(name: &str, instructions: &str, criteria: &[(&str, &str)]) -> Self {
        Question {
            name: name.into(),
            instructions: instructions.into(),
            criteria: Some(criteria.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()),
        }
    }
}

/// The answer to a choice question.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub choice: String,
    pub confidence: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Answers {
    /// The versioned model that answered (`jev-1.13.0`, not `jev-latest`).
    pub model: String,
    /// P(yes) per noul question, clamped to `[0, 1]`.
    pub noul: HashMap<String, f64>,
    /// The picked label per choice question.
    pub choice: HashMap<String, Choice>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Clone)]
pub struct Jev {
    http: reqwest::Client,
    base_url: String,
    key: Secret,
    model: String,
}

impl Jev {
    pub fn new(base_url: &str, key: Secret, model: &str, timeout: Duration) -> Result<Self> {
        if key.is_empty() {
            return Err(Error::NotConfigured("jev api key"));
        }
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(Jev { http, base_url: base_url.trim_end_matches('/').to_string(), key, model: model.to_string() })
    }

    pub fn key_fingerprint(&self) -> String {
        self.key.fingerprint()
    }

    /// Model names the key can use. Sends no text.
    pub async fn models(&self) -> Result<Vec<String>> {
        let resp = self
            .http
            .get(format!("{}/v1/models", self.base_url))
            .bearer_auth(self.key.expose())
            .send()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        let body = read_ok(resp).await?;
        let v: Value = serde_json::from_str(&body).map_err(|e| Error::Reply(e.to_string()))?;
        Ok(v["models"]
            .as_array()
            .map(|a| a.iter().filter_map(|m| m["name"].as_str().map(str::to_string)).collect())
            .unwrap_or_default())
    }

    /// Ask every question about `state` in one request.
    pub async fn ask(&self, state: &str, questions: &[Question]) -> Result<Answers> {
        let resp = self
            .http
            .post(format!("{}/v1/systemone", self.base_url))
            .bearer_auth(self.key.expose())
            .json(&build_request(&self.model, state, questions))
            .send()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        parse_response(&read_ok(resp).await?)
    }
}

async fn read_ok(resp: reqwest::Response) -> Result<String> {
    let status = resp.status();
    let body = resp.text().await.map_err(|e| Error::Transport(e.to_string()))?;
    if status.is_success() {
        Ok(body)
    } else {
        // 401 bad key, 422 invalid body (names the field), 429 rate limit,
        // 529 overloaded. The body is short and never contains the key.
        Err(Error::Status { service: SERVICE, status: status.as_u16(), body: body.chars().take(200).collect() })
    }
}

pub fn build_request(model: &str, state: &str, questions: &[Question]) -> Value {
    let qs: serde_json::Map<String, Value> = questions
        .iter()
        .map(|q| {
            let body = match &q.criteria {
                None => json!({"type": "noul", "instructions": q.instructions}),
                Some(c) => {
                    let criteria: serde_json::Map<String, Value> =
                        c.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
                    json!({"type": "choice", "instructions": q.instructions, "criteria": criteria})
                }
            };
            (q.name.clone(), body)
        })
        .collect();
    json!({"model": model, "state": state, "questions": qs})
}

pub fn parse_response(body: &str) -> Result<Answers> {
    let v: Value = serde_json::from_str(body).map_err(|e| Error::Reply(e.to_string()))?;
    let answers = v["answers"].as_object().ok_or_else(|| Error::Reply("no `answers` object".into()))?;
    let noul = answers
        .iter()
        .filter_map(|(name, a)| Some((name.clone(), a["noul"].as_f64()?.clamp(0.0, 1.0))))
        .collect();
    let choice = answers
        .iter()
        .filter_map(|(name, a)| {
            let c = a["choice"].as_str()?;
            let confidence = a["confidence"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
            Some((name.clone(), Choice { choice: c.to_string(), confidence }))
        })
        .collect();
    Ok(Answers {
        model: v["model"].as_str().unwrap_or("").to_string(),
        noul,
        choice,
        input_tokens: v["usage"]["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: v["usage"]["output_tokens"].as_u64().unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// What the live service returned for a synthetic tab on 2026-10-15.
    const LIVE: &str = r#"{"model":"jev-1.13.0","answers":{"return_24h":{"type":"noul","noul":0.07},"unsaved":{"type":"noul","noul":0.12}},"usage":{"input_tokens":338,"output_tokens":41}}"#;

    #[test]
    fn parses_the_live_response() {
        let a = parse_response(LIVE).unwrap();
        assert_eq!(a.model, "jev-1.13.0");
        assert_eq!(a.noul["return_24h"], 0.07);
        assert_eq!(a.noul["unsaved"], 0.12);
        assert_eq!((a.input_tokens, a.output_tokens), (338, 41));
    }

    #[test]
    fn odd_replies_are_errors_or_clamped_not_panics() {
        assert!(parse_response("not json").is_err());
        assert!(parse_response(r#"{"model":"x"}"#).is_err(), "no answers object");
        let a = parse_response(r#"{"answers":{"a":{"type":"noul","noul":1.7},"b":{"type":"choice","choice":"x"}}}"#).unwrap();
        assert_eq!(a.noul["a"], 1.0, "clamped");
        assert!(!a.noul.contains_key("b"), "a choice is not a noul");
        assert_eq!(a.choice["b"].choice, "x");
        assert!(!a.choice.contains_key("a"), "a noul is not a choice");
    }

    #[test]
    fn the_request_has_the_documented_shape() {
        let r = build_request("jev-latest", "Browser tab.", &[Question::new("q", "Will it return?")]);
        assert_eq!(r["model"], "jev-latest");
        assert_eq!(r["state"], "Browser tab.");
        assert_eq!(r["questions"]["q"], json!({"type": "noul", "instructions": "Will it return?"}));

        let r = build_request("jev-latest", "s", &[Question::choice("k", "Which?", &[("a", "first"), ("b", "second")])]);
        assert_eq!(
            r["questions"]["k"],
            json!({"type": "choice", "instructions": "Which?", "criteria": {"a": "first", "b": "second"}})
        );
    }

    #[test]
    fn a_missing_key_is_refused_up_front() {
        assert!(matches!(
            Jev::new("https://api.typesafe.ai", Secret::new("  "), "jev-latest", Duration::from_secs(1)),
            Err(Error::NotConfigured(_))
        ));
    }

    /// A one-shot HTTP server: returns `reply` with `status`, hands back the raw request.
    async fn serve_once(status: u16, reply: &'static str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 16384];
            let mut got = Vec::new();
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got).to_string();
                if let Some(split) = text.find("\r\n\r\n") {
                    let len = text[..split]
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                        .unwrap_or(0);
                    if got.len() >= split + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let resp = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&got).to_string()
        });
        (format!("http://{addr}"), handle)
    }

    #[tokio::test]
    async fn ask_sends_the_key_and_body_and_returns_probabilities() {
        let (base, req) = serve_once(200, LIVE).await;
        let jev = Jev::new(&base, Secret::new("test-key-123456"), "jev-latest", Duration::from_secs(5)).unwrap();
        let a = jev.ask("Browser tab.", &[Question::new("return_24h", "Will it return?")]).await.unwrap();
        assert_eq!(a.noul["return_24h"], 0.07);
        let raw = req.await.unwrap();
        assert!(raw.starts_with("POST /v1/systemone"), "{raw}");
        assert!(raw.to_ascii_lowercase().contains("authorization: bearer test-key-123456"));
        assert!(raw.contains("\"state\":\"Browser tab.\""));
    }

    #[tokio::test]
    async fn http_errors_carry_the_status_and_never_the_key() {
        let (base, _) = serve_once(429, r#"{"error":"rate limited"}"#).await;
        let jev = Jev::new(&base, Secret::new("test-key-123456"), "jev-latest", Duration::from_secs(5)).unwrap();
        let e = jev.ask("s", &[Question::new("q", "i")]).await.unwrap_err();
        assert!(matches!(e, Error::Status { status: 429, .. }), "{e:?}");
        assert!(!e.to_string().contains("test-key-123456"));
    }

    #[tokio::test]
    async fn models_lists_names() {
        let (base, req) = serve_once(200, r#"{"models":[{"name":"jev-latest"},{"name":"jev-preview"}]}"#).await;
        let jev = Jev::new(&base, Secret::new("k-1234567890"), "jev-latest", Duration::from_secs(5)).unwrap();
        assert_eq!(jev.models().await.unwrap(), vec!["jev-latest", "jev-preview"]);
        assert!(req.await.unwrap().starts_with("GET /v1/models"));
    }
}
