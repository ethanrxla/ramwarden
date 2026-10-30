//! Jev, TypeSafe's System One model, as the ladder's judge.
//!
//! # Why a System One model and not the chat tier
//!
//! The question the ladder asks before suspending or killing an app is small,
//! fixed and repeated every few seconds under pressure: *will the user be back
//! for this soon, and is it something that must not be closed?* That is a
//! classification over program state, not a conversation. Jev answers it as
//! typed values with calibrated probabilities, in one request for every
//! candidate at once, in tens to hundreds of milliseconds, which is what a
//! decision taken mid-pressure needs. The chat tier remains for tab analysis.
//!
//! # The request
//!
//! `POST {base}/v1/systemone` with a `state` (the candidates, from the LRU tail,
//! as JSON text) and two questions per candidate:
//!
//! - `return_{i}`, a *noul*: the probability the user comes back to it soon.
//! - `kind_{i}`, a *choice*: routine, important, sensitive or protected.
//!
//! A candidate may be closed only when it is routine and the probability that
//! the user is *not* coming back clears the threshold for the intent: higher
//! for a kill, which loses unsaved work, than for a suspend, which does not.
//!
//! Only names, numbers and the detector's short reason strings are sent: no
//! window titles, no URLs, no command lines.

use std::time::Duration;

use ramwarden_core::judge::{Candidate, Intent, Judgement};
use serde_json::{Value, json};

use crate::client::{Error, Result};
use crate::secret::Secret;

/// The categories Jev sorts each candidate into. Only `routine` may be closed.
pub const KINDS: [(&str, &str); 4] = [
    (
        "routine",
        "an ordinary app that can be closed and reopened later without losing anything",
    ),
    (
        "important",
        "the user's current work depends on it, or it probably holds unsaved work",
    ),
    (
        "sensitive",
        "it handles passwords, payments, private messages, health or other sensitive data",
    ),
    (
        "protected",
        "system infrastructure, a security tool, or something else that must keep running",
    ),
];

#[derive(Clone, Debug)]
pub struct Jev {
    http: reqwest::Client,
    base: String,
    key: Secret,
    pub model: String,
    /// Probability the user is not coming back, at or above which a suspend
    /// is allowed.
    pub suspend_threshold: f64,
    /// The same, for a kill.
    pub kill_threshold: f64,
}

impl Jev {
    pub fn new(
        base: &str,
        key: Secret,
        model: &str,
        suspend_threshold: f64,
        kill_threshold: f64,
        timeout: Duration,
    ) -> Result<Self> {
        if key.is_empty() {
            return Err(Error::NotConfigured("Jev (no TypeSafe API key)"));
        }
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(Jev {
            http,
            base: base.trim_end_matches('/').to_string(),
            key,
            model: model.to_string(),
            suspend_threshold,
            kill_threshold,
        })
    }

    /// Which key is in use, safe to log.
    pub fn key_fingerprint(&self) -> String {
        self.key.fingerprint()
    }

    pub fn threshold(&self, intent: Intent) -> f64 {
        match intent {
            Intent::Suspend => self.suspend_threshold,
            Intent::Kill => self.kill_threshold,
        }
    }

    pub async fn judge(&self, intent: Intent, candidates: &[Candidate]) -> Result<Vec<Judgement>> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let body = request(&self.model, intent, candidates);
        let r = self
            .http
            .post(format!("{}/v1/systemone", self.base))
            .bearer_auth(self.key.expose())
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        let status = r.status();
        let text = r.text().await.map_err(|e| Error::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(Error::Status {
                service: "typesafe",
                status: status.as_u16(),
                body: text.chars().take(200).collect(),
            });
        }
        let reply: Value = serde_json::from_str(&text).map_err(|e| Error::Reply(e.to_string()))?;
        Ok(decide(&reply, intent, candidates, self.threshold(intent)))
    }
}

/// The request body for one batch of candidates.
pub fn request(model: &str, intent: Intent, candidates: &[Candidate]) -> Value {
    let action = intent.as_str();
    let state = json!({
        "task": format!(
            "RamWarden is under memory pressure and wants to {action} applications the \
             user has not used recently. Candidates are listed least recently used first."
        ),
        "candidates": candidates.iter().enumerate().map(|(i, c)| json!({
            "index": i,
            "name": c.name,
            "lru_position": c.cold_rank,
            "minutes_since_used": c.idle_minutes.round(),
            "memory_mb": c.pss / 1_000_000,
            "role": c.role,
            "has_window": c.has_window,
            "open_connections": c.established,
            "serving": c.listening,
            "signals": c.reasons,
        })).collect::<Vec<_>>(),
    });

    let kinds: serde_json::Map<String, Value> = KINDS
        .iter()
        .map(|(k, d)| (k.to_string(), Value::String(d.to_string())))
        .collect();
    let mut questions = serde_json::Map::new();
    for (i, c) in candidates.iter().enumerate() {
        questions.insert(
            format!("return_{i}"),
            json!({
                "type": "noul",
                "instructions": format!(
                    "The user will come back to {} (candidate {i}) within the next hour",
                    c.name
                ),
            }),
        );
        questions.insert(
            format!("kind_{i}"),
            json!({
                "type": "choice",
                "instructions": format!("What kind of application {} (candidate {i}) is", c.name),
                "criteria": kinds,
            }),
        );
    }

    json!({
        "state": state.to_string(),
        "model": model,
        "questions": questions,
    })
}

/// Turn Jev's answers into judgements.
///
/// A missing or malformed answer for a candidate is a veto, never a pass:
/// closing on the strength of an answer that was not given is the failure this
/// whole step exists to prevent.
pub fn decide(reply: &Value, intent: Intent, candidates: &[Candidate], threshold: f64) -> Vec<Judgement> {
    let answers = &reply["answers"];
    candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let returning = answers[format!("return_{i}")]["noul"].as_f64();
            let kind = answers[format!("kind_{i}")]["choice"].as_str();
            let (close, confidence, reason) = match (returning, kind) {
                (Some(back), Some(kind)) => {
                    let away = (1.0 - back).clamp(0.0, 1.0);
                    let pct = (away * 100.0).round();
                    if kind != "routine" {
                        (false, away, format!("Jev says it is {kind}"))
                    } else if away < threshold {
                        (false, away, format!("Jev is not convinced ({pct}%)"))
                    } else {
                        (
                            true,
                            away,
                            format!("Jev: not coming back soon ({pct}%), ok to {}", intent.as_str()),
                        )
                    }
                }
                _ => (false, 0.0, "Jev gave no answer for it".to_string()),
            };
            Judgement {
                name: c.name.clone(),
                close,
                confidence,
                reason,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(name: &str, rank: usize) -> Candidate {
        Candidate {
            name: name.into(),
            cold_rank: rank,
            idle_minutes: 42.4,
            pss: 600_000_000,
            role: "app".into(),
            has_window: true,
            established: 0,
            listening: false,
            reasons: vec!["idle".into()],
        }
    }

    #[test]
    fn the_request_asks_two_questions_per_candidate() {
        let body = request("jev-latest", Intent::Kill, &[cand("Discord", 0), cand("Slack", 1)]);
        assert_eq!(body["model"], "jev-latest");
        let q = body["questions"].as_object().unwrap();
        assert_eq!(q.len(), 4);
        assert_eq!(q["return_0"]["type"], "noul");
        assert_eq!(q["kind_1"]["type"], "choice");
        assert!(q["kind_1"]["criteria"]["routine"].is_string());
        assert!(q["return_1"]["instructions"].as_str().unwrap().contains("Slack"));

        // State is a JSON string, and carries no more than the candidate facts.
        let state: Value = serde_json::from_str(body["state"].as_str().unwrap()).unwrap();
        assert_eq!(state["candidates"][0]["name"], "Discord");
        assert_eq!(state["candidates"][0]["memory_mb"], 600);
        assert_eq!(state["candidates"][0]["minutes_since_used"], 42.0);
        assert!(state["task"].as_str().unwrap().contains("close"));
    }

    fn reply(answers: Value) -> Value {
        json!({ "model": "jev-1.13.0", "answers": answers })
    }

    #[test]
    fn routine_and_unlikely_to_return_is_closed() {
        let r = reply(json!({
            "return_0": { "type": "noul", "noul": 0.1 },
            "kind_0": { "type": "choice", "choice": "routine", "confidence": 0.9 },
        }));
        let j = decide(&r, Intent::Suspend, &[cand("Discord", 0)], 0.7);
        assert!(j[0].close, "{j:?}");
        assert!((j[0].confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn below_the_threshold_says_jev_is_not_convinced() {
        let r = reply(json!({
            "return_0": { "type": "noul", "noul": 0.39 },
            "kind_0": { "type": "choice", "choice": "routine" },
        }));
        let j = decide(&r, Intent::Kill, &[cand("Preview", 0)], 0.85);
        assert!(!j[0].close);
        assert_eq!(j[0].reason, "Jev is not convinced (61%)");
    }

    #[test]
    fn anything_not_routine_is_kept_however_cold() {
        for kind in ["important", "sensitive", "protected"] {
            let r = reply(json!({
                "return_0": { "type": "noul", "noul": 0.0 },
                "kind_0": { "type": "choice", "choice": kind },
            }));
            let j = decide(&r, Intent::Suspend, &[cand("KeePassXC", 0)], 0.5);
            assert!(!j[0].close, "{kind}");
            assert!(j[0].reason.contains(kind));
        }
    }

    /// A real reply from jev-1.14.0 (2026-10-30) to the request
    /// `request("jev-latest", Intent::Kill, ...)` builds for Preview, Discord
    /// and KeePassXC, so the parsing is pinned to what the API actually sends.
    #[test]
    fn a_live_reply_parses_into_the_expected_verdicts() {
        let r: Value = serde_json::from_str(r#"{"model":"jev-1.14.0","answers":{"kind_0":{"type":"choice","choice":"routine","confidence":0.89,"probabilities":{"important":0.08,"protected":0.0,"routine":0.92,"sensitive":0.0}},"kind_1":{"type":"choice","choice":"routine","confidence":0.71,"probabilities":{"important":0.01,"protected":0.0,"routine":0.78,"sensitive":0.21}},"kind_2":{"type":"choice","choice":"sensitive","confidence":0.99,"probabilities":{"important":0.0,"protected":0.01,"routine":0.0,"sensitive":0.99}},"return_0":{"type":"noul","noul":0.16},"return_1":{"type":"noul","noul":0.48},"return_2":{"type":"noul","noul":0.44}},"usage":{"input_tokens":939,"output_tokens":194}}"#).unwrap();
        let cands = [cand("Preview", 0), cand("Discord", 1), cand("KeePassXC", 2)];

        // Kill bar (85%): Preview at 84% just misses; nothing is killed.
        let kill = decide(&r, Intent::Kill, &cands, 0.85);
        assert_eq!(kill[0].reason, "Jev is not convinced (84%)");
        assert!(kill.iter().all(|j| !j.close));

        // Suspend bar (70%): Preview goes, Discord (52%) stays, KeePassXC is sensitive.
        let suspend = decide(&r, Intent::Suspend, &cands, 0.7);
        assert!(suspend[0].close);
        assert!(!suspend[1].close);
        assert_eq!(suspend[2].reason, "Jev says it is sensitive");
    }

    #[test]
    fn a_missing_answer_is_a_veto() {
        let r = reply(json!({
            "return_0": { "type": "noul", "noul": 0.0 },
            "kind_0": { "type": "choice", "choice": "routine" },
        }));
        let j = decide(&r, Intent::Suspend, &[cand("A", 0), cand("B", 1)], 0.5);
        assert!(j[0].close);
        assert!(!j[1].close);
        assert!(j[1].reason.contains("no answer"));
    }

    #[test]
    fn no_key_is_not_configured() {
        let e = Jev::new(
            "https://api.typesafe.ai",
            Secret::new(""),
            "jev-latest",
            0.7,
            0.85,
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(matches!(e, Error::NotConfigured(_)));
    }
}
