//! Jev as the ladder's judge, before a suspend or kill.
//!
//! The question is small, fixed and repeated every few seconds under pressure:
//! *will the user be back for this soon, and is it something that must not be
//! closed?* One request covers every candidate from the LRU tail, with two
//! questions each:
//!
//! - `return_{i}`, a noul: the chance the user comes back within the hour.
//! - `kind_{i}`, a choice: routine, important, sensitive or protected.
//!
//! A candidate may be closed only when it is routine and the chance the user is
//! *not* coming back clears the threshold for the intent: higher for a kill,
//! which loses unsaved work, than for a suspend, which does not.
//!
//! Only names, numbers and the detector's short reason strings are sent: no
//! window titles, no URLs, no command lines.

use ramwarden_core::judge::{Candidate, Intent, Judgement};
use serde_json::json;

use ramwarden_ai::client::Result;
use ramwarden_ai::jev::{Answers, Jev, Question};

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

/// The state and questions for one batch of candidates.
pub fn request(intent: Intent, candidates: &[Candidate]) -> (String, Vec<Question>) {
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

    let mut questions = Vec::with_capacity(candidates.len() * 2);
    for (i, c) in candidates.iter().enumerate() {
        questions.push(Question::new(
            &format!("return_{i}"),
            &format!("The user will come back to {} (candidate {i}) within the next hour", c.name),
        ));
        questions.push(Question::choice(
            &format!("kind_{i}"),
            &format!("What kind of application {} (candidate {i}) is", c.name),
            &KINDS,
        ));
    }
    (state.to_string(), questions)
}

/// Turn Jev's answers into judgements.
///
/// A missing answer for a candidate is a veto, never a pass: closing on the
/// strength of an answer that was not given is the failure this whole step
/// exists to prevent.
pub fn decide(answers: &Answers, intent: Intent, candidates: &[Candidate], threshold: f64) -> Vec<Judgement> {
    candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let returning = answers.noul.get(&format!("return_{i}")).copied();
            let kind = answers.choice.get(&format!("kind_{i}")).map(|k| k.choice.as_str());
            let (close, confidence, reason) = match (returning, kind) {
                (Some(back), Some(kind)) => {
                    let away = (1.0 - back).clamp(0.0, 1.0);
                    let pct = (away * 100.0).round();
                    if kind != "routine" {
                        (false, away, format!("Jev says it is {kind}"))
                    } else if away < threshold {
                        (false, away, format!("Jev is not convinced ({pct}%)"))
                    } else {
                        (true, away, format!("Jev: not coming back soon ({pct}%), ok to {}", intent.as_str()))
                    }
                }
                _ => (false, 0.0, "Jev gave no answer for it".to_string()),
            };
            Judgement { name: c.name.clone(), close, confidence, reason }
        })
        .collect()
}

/// Ask about every candidate in one request and decide at `threshold`.
pub async fn judge(jev: &Jev, intent: Intent, candidates: &[Candidate], threshold: f64) -> Result<Vec<Judgement>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let (state, questions) = request(intent, candidates);
    let answers = jev.ask(&state, &questions).await?;
    Ok(decide(&answers, intent, candidates, threshold))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramwarden_ai::jev::{build_request, parse_response};

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

    fn answers(body: &str) -> Answers {
        parse_response(body).unwrap()
    }

    #[test]
    fn the_request_asks_two_questions_per_candidate() {
        let (state, qs) = request(Intent::Kill, &[cand("Discord", 0), cand("Slack", 1)]);
        let body = build_request("jev-latest", &state, &qs);
        let q = body["questions"].as_object().unwrap();
        assert_eq!(q.len(), 4);
        assert_eq!(q["return_0"]["type"], "noul");
        assert_eq!(q["kind_1"]["type"], "choice");
        assert!(q["kind_1"]["criteria"]["routine"].is_string());
        assert!(q["return_1"]["instructions"].as_str().unwrap().contains("Slack"));

        let state: serde_json::Value = serde_json::from_str(body["state"].as_str().unwrap()).unwrap();
        assert_eq!(state["candidates"][0]["name"], "Discord");
        assert_eq!(state["candidates"][0]["memory_mb"], 600);
        assert_eq!(state["candidates"][0]["minutes_since_used"], 42.0);
        assert!(state["task"].as_str().unwrap().contains("close"));
    }

    /// A real reply from jev-1.14.0 (2026-10-30) to this module's request for
    /// Preview, Discord and KeePassXC, so the parsing is pinned to what the
    /// API actually sends.
    const LIVE: &str = r#"{"model":"jev-1.14.0","answers":{"kind_0":{"type":"choice","choice":"routine","confidence":0.89,"probabilities":{"important":0.08,"protected":0.0,"routine":0.92,"sensitive":0.0}},"kind_1":{"type":"choice","choice":"routine","confidence":0.71,"probabilities":{"important":0.01,"protected":0.0,"routine":0.78,"sensitive":0.21}},"kind_2":{"type":"choice","choice":"sensitive","confidence":0.99,"probabilities":{"important":0.0,"protected":0.01,"routine":0.0,"sensitive":0.99}},"return_0":{"type":"noul","noul":0.16},"return_1":{"type":"noul","noul":0.48},"return_2":{"type":"noul","noul":0.44}},"usage":{"input_tokens":939,"output_tokens":194}}"#;

    #[test]
    fn a_live_reply_parses_into_the_expected_verdicts() {
        let a = answers(LIVE);
        let cands = [cand("Preview", 0), cand("Discord", 1), cand("KeePassXC", 2)];

        // Kill bar (85%): Preview at 84% just misses; nothing is killed.
        let kill = decide(&a, Intent::Kill, &cands, 0.85);
        assert_eq!(kill[0].reason, "Jev is not convinced (84%)");
        assert!(kill.iter().all(|j| !j.close));

        // Suspend bar (70%): Preview goes, Discord (52%) stays, KeePassXC is sensitive.
        let suspend = decide(&a, Intent::Suspend, &cands, 0.7);
        assert!(suspend[0].close);
        assert!((suspend[0].confidence - 0.84).abs() < 1e-9);
        assert!(!suspend[1].close);
        assert_eq!(suspend[2].reason, "Jev says it is sensitive");
    }

    #[test]
    fn anything_not_routine_is_kept_however_cold() {
        for kind in ["important", "sensitive", "protected"] {
            let a = answers(&format!(
                r#"{{"answers":{{"return_0":{{"type":"noul","noul":0.0}},"kind_0":{{"type":"choice","choice":"{kind}"}}}}}}"#
            ));
            let j = decide(&a, Intent::Suspend, &[cand("KeePassXC", 0)], 0.5);
            assert!(!j[0].close, "{kind}");
            assert!(j[0].reason.contains(kind));
        }
    }

    #[test]
    fn a_missing_answer_is_a_veto() {
        let a = answers(r#"{"answers":{"return_0":{"type":"noul","noul":0.0},"kind_0":{"type":"choice","choice":"routine"}}}"#);
        let j = decide(&a, Intent::Suspend, &[cand("A", 0), cand("B", 1)], 0.5);
        assert!(j[0].close);
        assert!(!j[1].close);
        assert!(j[1].reason.contains("no answer"));
    }
}
