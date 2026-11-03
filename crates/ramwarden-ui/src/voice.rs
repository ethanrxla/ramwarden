//! The warden's speech bubble, shared by the GTK window (Linux) and the egui
//! window and tray (Windows).
//!
//! The daemon decides what the warden says and when (`ramwarden-daemon::voice`,
//! `docs/specs/warden-voice.md`); this is only the wire shape and the calls, so
//! both faces stay thin and say exactly the same thing.

use serde::Deserialize;

use crate::client::{Client, Result, TIMEOUT};

/// A button under a bubble.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Choice {
    pub id: String,
    pub label: String,
}

/// The tab a check-in is about.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct BubbleTab {
    pub browser: String,
    pub id: i64,
    pub url: String,
    #[serde(default)]
    pub title: String,
}

/// What the warden is saying right now.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Bubble {
    pub id: String,
    #[serde(default)]
    pub warden: String,
    /// `activity`, `check_in`, `quip` or `reply`.
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub tab: Option<BubbleTab>,
    #[serde(default)]
    pub expires_in_s: u64,
}

impl Bubble {
    /// A question waiting for an answer, as opposed to a remark.
    pub fn is_question(&self) -> bool {
        !self.choices.is_empty()
    }
}

/// `GET /warden/bubble`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Speech {
    #[serde(default)]
    pub bubble: Option<Bubble>,
    /// Pets come with Plus and Pro, not Lite. Absent (an older daemon): yes.
    #[serde(default = "yes")]
    pub pet: bool,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub muted_until: Option<u64>,
}

/// `POST /warden/answer` and `/warden/mute`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Reply {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub reply: String,
    #[serde(default)]
    pub closed: bool,
}

/// The mute choices every face offers, as (label, hours). `None`: until unmuted.
pub const MUTE_CHOICES: [(&str, Option<f64>); 3] =
    [("Quiet for an hour", Some(1.0)), ("Quiet for today", Some(12.0)), ("Quiet until I unmute", None)];

fn yes() -> bool {
    true
}

impl Client {
    pub async fn bubble(&self) -> Result<Speech> {
        self.get("/warden/bubble", TIMEOUT).await
    }

    pub async fn answer(&self, bubble: &str, choice: &str) -> Result<Reply> {
        // Closing waits on the browser's confirmation.
        self.post("/warden/answer", serde_json::json!({"id": bubble, "choice": choice}), std::time::Duration::from_secs(40)).await
    }

    pub async fn mute(&self, muted: bool, hours: Option<f64>) -> Result<Reply> {
        self.post("/warden/mute", serde_json::json!({"muted": muted, "hours": hours}), TIMEOUT).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_in_parses_with_its_tab_and_choices() {
        let s: Speech = serde_json::from_str(r#"{"bubble":{"id":"a","warden":"ram","kind":"check_in",
            "text":"Coming back?","choices":[{"id":"keep","label":"Yes, keep it"}],
            "tab":{"browser":"b","id":3,"url":"https://x/","title":"X"},"expires_in_s":600},
            "muted":false,"muted_until":null}"#).unwrap();
        let b = s.bubble.unwrap();
        assert!(b.is_question());
        assert_eq!(b.tab.unwrap().id, 3);
    }

    #[test]
    fn silence_and_an_older_daemon_both_parse_as_no_bubble() {
        let s: Speech = serde_json::from_str(r#"{"bubble":null,"muted":true}"#).unwrap();
        assert!(s.bubble.is_none() && s.muted);
        let s: Speech = serde_json::from_str("{}").unwrap();
        assert!(s.bubble.is_none() && !s.muted);
    }

    #[test]
    fn a_remark_is_not_a_question() {
        let b: Bubble = serde_json::from_str(r#"{"id":"q","kind":"quip","text":"Hi"}"#).unwrap();
        assert!(!b.is_question());
    }
}
