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
    /// Placeholder for a question answered in words (the focus question).
    /// Clients show an always-visible text box; Enter sends it.
    #[serde(default)]
    pub input: Option<String>,
    #[serde(default)]
    pub tab: Option<BubbleTab>,
    #[serde(default)]
    pub expires_in_s: u64,
}

impl Bubble {
    /// A question waiting for an answer, as opposed to a remark.
    pub fn is_question(&self) -> bool {
        !self.choices.is_empty() || self.input.is_some()
    }

    /// The buttons to draw. An older daemon offered a "focus" button that
    /// only opened a text box; the box is always there now, so it is dropped.
    pub fn buttons(&self) -> impl Iterator<Item = &Choice> {
        self.choices.iter().filter(|c| c.id != "focus")
    }

    /// Typing here answers the focus question rather than chatting.
    pub fn asks_focus(&self) -> bool {
        self.kind == "focus" || self.input.is_some() || self.choices.iter().any(|c| c.id == "focus")
    }

    /// The text box's placeholder under this bubble.
    pub fn placeholder(&self) -> &str {
        match &self.input {
            Some(p) => p,
            None if self.asks_focus() => "What are you working on?",
            None => TALK_PLACEHOLDER,
        }
    }
}

/// The text box's placeholder when the warden has not asked anything.
pub const TALK_PLACEHOLDER: &str = "Talk to your warden…";

/// The longest message the daemon takes: 500 characters for a focus, 4000
/// bytes for a chat message. Checked before sending so a long paste says why.
pub fn too_long(text: &str, focus: bool) -> Option<&'static str> {
    if focus && text.chars().count() > 500 {
        Some("Keep your focus under 500 characters.")
    } else if text.len() > 4000 {
        Some("That message is too long. Keep it under 4,000 characters.")
    } else {
        None
    }
}

/// A tab the warden thinks belongs to the focus, as a checkbox.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct FocusTab {
    pub browser: String,
    pub id: i64,
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub selected: bool,
}

/// The editable checklist under a focus reply.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct FocusProposal {
    pub token: String,
    #[serde(default)]
    pub tabs: Vec<FocusTab>,
    #[serde(default)]
    pub expires_at: u64,
}

/// `POST /warden/focus`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct FocusReply {
    #[serde(default)]
    pub reply: String,
    #[serde(default)]
    pub proposal: Option<FocusProposal>,
}

/// A change the chat proposes; nothing happens until the user approves it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Proposed {
    pub token: String,
    pub label: String,
}

/// `POST /assistant/chat`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ChatReply {
    #[serde(default)]
    pub reply: String,
    /// `facts` when no model answered and the reply is readings only.
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub proposed_action: Option<Proposed>,
}

/// What the warden said back to something typed in its bubble.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Said {
    pub you: String,
    pub reply: String,
    pub focus: Option<FocusProposal>,
    pub proposed: Option<Proposed>,
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
pub const MUTE_CHOICES: [(&str, Option<f64>); 3] = [
    ("Quiet for an hour", Some(1.0)),
    ("Quiet for today", Some(12.0)),
    ("Quiet until I unmute", None),
];

fn yes() -> bool {
    true
}

impl Client {
    pub async fn bubble(&self) -> Result<Speech> {
        self.get("/warden/bubble", TIMEOUT).await
    }

    pub async fn answer(&self, bubble: &str, choice: &str) -> Result<Reply> {
        // Closing waits on the browser's confirmation.
        self.post(
            "/warden/answer",
            serde_json::json!({"id": bubble, "choice": choice}),
            std::time::Duration::from_secs(40),
        )
        .await
    }

    pub async fn mute(&self, muted: bool, hours: Option<f64>) -> Result<Reply> {
        self.post(
            "/warden/mute",
            serde_json::json!({"muted": muted, "hours": hours}),
            TIMEOUT,
        )
        .await
    }

    /// Tell the warden what you are working on. The model's pick of tabs
    /// can take a while, so this waits as long as the chat does.
    pub async fn focus(&self, goal: &str) -> Result<FocusReply> {
        self.post(
            "/warden/focus",
            serde_json::json!({"goal": goal}),
            CHAT_TIMEOUT,
        )
        .await
    }

    /// Keep the checked tabs of a focus checklist. Unchecked tabs get no
    /// negative feedback.
    pub async fn focus_feedback(
        &self,
        token: &str,
        tabs: &[FocusTab],
    ) -> Result<serde_json::Value> {
        let selected: Vec<_> = tabs
            .iter()
            .filter(|t| t.selected)
            .map(|t| serde_json::json!({"browser": t.browser, "id": t.id, "url": t.url}))
            .collect();
        self.post(
            "/warden/focus/feedback",
            serde_json::json!({"token": token, "selected": selected}),
            TIMEOUT,
        )
        .await
    }

    /// One read-only chat turn. Changes come back as a proposal.
    pub async fn chat(&self, message: &str) -> Result<ChatReply> {
        self.post(
            "/assistant/chat",
            serde_json::json!({"message": message, "history": []}),
            CHAT_TIMEOUT,
        )
        .await
    }

    /// Apply a proposal the user approved.
    pub async fn chat_action(&self, token: &str) -> Result<ChatReply> {
        self.post(
            "/assistant/action",
            serde_json::json!({"token": token}),
            CHAT_TIMEOUT,
        )
        .await
    }

    /// Send what was typed in the bubble: a focus when the warden asked for
    /// one, otherwise a chat message. Errors become the warden's words, so
    /// every face shows the same thing.
    pub async fn say_to_warden(&self, text: &str, focus: bool) -> Said {
        let you = text.to_string();
        if focus {
            match self.focus(text).await {
                Ok(r) => Said {
                    you,
                    reply: r.reply,
                    focus: r.proposal.filter(|p| !p.tabs.is_empty()),
                    proposed: None,
                },
                Err(e) => Said {
                    you,
                    reply: format!("Your focus wasn't saved: {e}"),
                    ..Said::default()
                },
            }
        } else {
            match self.chat(text).await {
                Ok(r) => Said {
                    you,
                    reply: r.reply,
                    focus: None,
                    proposed: r.proposed_action,
                },
                Err(crate::client::Error::Status { status: 429, .. }) => Said {
                    you,
                    reply: "I'm still answering your last message. Try again in a moment.".into(),
                    ..Said::default()
                },
                Err(e) => Said {
                    you,
                    reply: format!("I couldn't reach the assistant: {e}"),
                    ..Said::default()
                },
            }
        }
    }
}

/// The daemon gives a model about 40 seconds before it answers from facts.
const CHAT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_in_parses_with_its_tab_and_choices() {
        let s: Speech = serde_json::from_str(
            r#"{"bubble":{"id":"a","warden":"ram","kind":"check_in",
            "text":"Coming back?","choices":[{"id":"keep","label":"Yes, keep it"}],
            "tab":{"browser":"b","id":3,"url":"https://x/","title":"X"},"expires_in_s":600},
            "muted":false,"muted_until":null}"#,
        )
        .unwrap();
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
    fn the_focus_question_is_typed_not_clicked() {
        // Today's daemon: no buttons, a placeholder.
        let b: Bubble = serde_json::from_str(
            r#"{"id":"f","kind":"focus","text":"What are you working on?",
            "choices":[],"input":"What are you working on?"}"#,
        )
        .unwrap();
        assert!(b.is_question() && b.asks_focus());
        assert_eq!(b.buttons().count(), 0);
        assert_eq!(b.placeholder(), "What are you working on?");
        // An older daemon's "Tell Warden" button is not drawn.
        let old: Bubble = serde_json::from_str(
            r#"{"id":"f","kind":"focus","text":"?",
            "choices":[{"id":"focus","label":"Tell Warden"}]}"#,
        )
        .unwrap();
        assert!(old.asks_focus());
        assert_eq!(old.buttons().count(), 0);
        // A remark invites chat instead.
        let q: Bubble = serde_json::from_str(r#"{"id":"q","kind":"quip","text":"Hi"}"#).unwrap();
        assert!(!q.asks_focus());
        assert_eq!(q.placeholder(), TALK_PLACEHOLDER);
    }

    #[test]
    fn long_messages_are_refused_before_sending() {
        assert!(too_long(&"é".repeat(500), true).is_none());
        assert!(too_long(&"é".repeat(501), true).is_some());
        assert!(too_long(&"x".repeat(4000), false).is_none());
        assert!(too_long(&"x".repeat(4001), false).is_some());
    }

    #[test]
    fn a_focus_reply_parses_its_checklist() {
        let r: FocusReply = serde_json::from_str(r#"{"ok":true,"reply":"On it.","proposal":{"token":"t",
            "tabs":[{"browser":"chrome","id":4,"url":"https://docs.rs/","title":"Docs","selected":true}],
            "expires_at":99}}"#).unwrap();
        let p = r.proposal.unwrap();
        assert_eq!(p.tabs.len(), 1);
        assert!(p.tabs[0].selected);
    }

    #[test]
    fn a_remark_is_not_a_question() {
        let b: Bubble = serde_json::from_str(r#"{"id":"q","kind":"quip","text":"Hi"}"#).unwrap();
        assert!(!b.is_question());
    }
}
