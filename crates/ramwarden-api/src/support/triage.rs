//! Which topic a visitor's message is about, and whether they need a person.
//!
//! Two routers, the second always available:
//!
//! 1. **Jev**, with the operator's key on this server. The visitor's message
//!    is the `state`; the questions are fixed here (a `choice` over the
//!    answer bank's topics and a `noul` for "needs a person"), so a visitor
//!    can only ever get one of [`super::kb::TOPICS`] back, never free text.
//! 2. **Keywords**, when Jev is unreachable, over budget or unsure. Phrases
//!    outscore single words. Nothing matched means "I don't know", which
//!    offers a ticket rather than a guess (spec 2.6).
//!
//! "Degrade, don't fail": a visitor never sees which router answered.

use ramwarden_ai::jev::{Question, build_request, parse_response};
use serde_json::{Map, Value};

use super::kb::{self, TOPICS, UNKNOWN};
use crate::upstream::Upstream;

/// Below this, Jev's topic is not trusted and the keywords decide.
pub const MIN_TOPIC_CONFIDENCE: f64 = 0.45;
/// At or above this, the answer offers a ticket whatever the topic.
pub const PERSON_THRESHOLD: f64 = 0.6;
/// Longest message the router reads. The rest is still stored for staff.
pub const MAX_STATE_CHARS: usize = 600;

#[derive(Clone, Debug, PartialEq)]
pub struct Triage {
    /// A topic id, or [`UNKNOWN`].
    pub topic: &'static str,
    pub wants_person: bool,
    /// `jev` or `keywords`, for the server log and the staff view.
    pub via: &'static str,
    /// Jev's input tokens, for the daily budget. Zero for keywords.
    pub input_tokens: u64,
}

pub fn questions(warden: &str) -> Vec<Question> {
    let criteria: Vec<(&str, &str)> =
        TOPICS.iter().map(|t| (t.id, t.meaning)).chain([(UNKNOWN, "anything else, or not a question about RamWarden")]).collect();
    let persona = if warden == "threat" { "ThreatWarden's" } else { "RamWarden's" };
    vec![
        Question::choice(
            "topic",
            &format!("A visitor on the RamWarden website wrote this to {persona} support chat. Which topic is the visitor asking about?"),
            &criteria,
        ),
        Question::new(
            "needs_person",
            "The visitor asks for a human, reports something broken with their account, payment, download or install, or asks for something only a person could do (like a refund).",
        ),
    ]
}

pub fn state(message: &str) -> String {
    message.chars().take(MAX_STATE_CHARS).collect()
}

/// The keyword router. Never fails.
pub fn by_keywords(message: &str) -> Triage {
    let text = format!(" {} ", normalise(message));
    let mut best: (&'static str, usize) = (UNKNOWN, 0);
    for t in TOPICS {
        let score: usize = t
            .keywords
            .iter()
            .filter(|k| text.contains(&format!(" {} ", normalise(k))) || (k.len() == 1 && text.contains(*k)))
            .map(|k| if k.contains(' ') { 3 } else { 1 })
            .sum();
        if score > best.1 {
            best = (t.id, score);
        }
    }
    let wants_person = best.0 == "human" || best.0 == "refund";
    Triage { topic: best.0, wants_person, via: "keywords", input_tokens: 0 }
}

/// Lowercase words separated by single spaces; `$` kept so "$5" can match.
fn normalise(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '$' || c == '-' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Reads Jev's reply. `None` when it is missing, malformed or names a topic
/// the bank does not have.
pub fn from_jev(reply: &Value) -> Option<Triage> {
    let a = parse_response(&reply.to_string()).ok()?;
    let c = a.choice.get("topic")?;
    let topic = TOPICS.iter().map(|t| t.id).chain([UNKNOWN]).find(|id| *id == c.choice)?;
    if c.confidence < MIN_TOPIC_CONFIDENCE {
        return None;
    }
    let wants_person = a.noul.get("needs_person").is_some_and(|p| *p >= PERSON_THRESHOLD);
    Some(Triage { topic, wants_person, via: "jev", input_tokens: a.input_tokens })
}

/// Asks Jev; the keyword router when Jev is off, failing or unsure. A topic
/// Jev calls "unknown" gets a second chance with the keywords, because a
/// visitor who typed "price?" deserves the price.
pub async fn triage(upstream: Option<&Upstream>, warden: &str, message: &str) -> Triage {
    let words = by_keywords(message);
    let Some(up) = upstream else { return words };
    let req = build_request(up.model(), &state(message), &questions(warden));
    let qs: Map<String, Value> = req["questions"].as_object().cloned().unwrap_or_default();
    match up.ask_sales(&state(message), &qs).await {
        Ok((reply, tokens)) => match from_jev(&reply) {
            Some(t) if t.topic == UNKNOWN && words.topic != UNKNOWN => {
                Triage { wants_person: t.wants_person || words.wants_person, input_tokens: tokens, ..words }
            }
            Some(t) => t,
            None => Triage { input_tokens: tokens, ..words },
        },
        Err(e) => {
            tracing::info!("support triage fell back to keywords: {e:?}");
            words
        }
    }
}

/// The topic's answer, for the chips' own questions.
pub fn for_chip(id: &str) -> Option<Triage> {
    kb::topic(id).map(|t| Triage { topic: t.id, wants_person: t.id == "human", via: "chip", input_tokens: 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keywords_find_the_obvious_topic() {
        for (q, want) in [
            ("How much is Pro?", "pricing"),
            ("how much does it cost per month", "pricing"),
            ("Is there a free trial?", "trial"),
            ("does it run on linux", "platforms"),
            ("How do I install on Windows", "install"),
            ("what's ThreatWarden", "threatwarden"),
            ("can I pay with Phantom", "payment"),
            ("I want a refund", "refund"),
            ("my download is not working", "human"),
            ("how do I cancel", "cancel"),
            ("is it open source?", "open_source"),
            ("what plan am I on", "my_account"),
            ("asdf qwerty", UNKNOWN),
        ] {
            assert_eq!(by_keywords(q).topic, want, "{q}");
        }
        assert!(by_keywords("I want a refund").wants_person);
        assert!(!by_keywords("how much is it").wants_person);
    }

    #[test]
    fn the_questions_only_offer_the_banks_topics() {
        let qs = questions("ram");
        let crit = qs[0].criteria.as_ref().unwrap();
        assert_eq!(crit.len(), TOPICS.len() + 1);
        assert!(crit.iter().any(|(k, _)| k == UNKNOWN));
        assert!(questions("threat")[0].instructions.contains("ThreatWarden"));
    }

    #[test]
    fn jev_replies_are_trusted_only_when_sure_and_known() {
        let r = |topic: &str, conf: f64, person: f64| {
            json!({"model": "jev-1", "answers": {
                "topic": {"type": "choice", "choice": topic, "confidence": conf},
                "needs_person": {"type": "noul", "noul": person}},
                "usage": {"input_tokens": 120}})
        };
        let t = from_jev(&r("trial", 0.9, 0.1)).unwrap();
        assert_eq!((t.topic, t.wants_person, t.via, t.input_tokens), ("trial", false, "jev", 120));
        assert!(from_jev(&r("refund", 0.8, 0.9)).unwrap().wants_person);
        assert!(from_jev(&r("trial", 0.2, 0.1)).is_none(), "unsure");
        assert!(from_jev(&r("write me a poem", 0.99, 0.0)).is_none(), "not a topic");
        assert!(from_jev(&json!({"answers": {}})).is_none());
    }

    #[tokio::test]
    async fn an_unreachable_jev_falls_back_to_keywords() {
        let up = Upstream::new("http://127.0.0.1:9", "jev-latest", ramwarden_ai::secret::Secret::new("k"), std::time::Duration::from_millis(300))
            .unwrap();
        let t = triage(Some(&up), "ram", "How much is Plus?").await;
        assert_eq!((t.topic, t.via), ("pricing", "keywords"));
    }

    #[test]
    fn the_state_is_capped() {
        assert_eq!(state(&"a".repeat(5000)).len(), MAX_STATE_CHARS);
    }
}
