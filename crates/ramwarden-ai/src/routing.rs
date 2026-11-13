//! Model choice is advisory, never authority to execute a tool.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Work {
    Conversation,
    Routine,
    MultiStep,
    Complex,
    Specialist,
}
impl Work {
    pub fn model(self) -> &'static str {
        match self {
            Self::Conversation | Self::Routine => "grok-4.20-0309-non-reasoning",
            Self::MultiStep | Self::Complex => "grok-4.7",
            Self::Specialist => "gpt-6.1-terra",
        }
    }
}
/// An explicit public-web lookup, never inferred from files or quoted history.
pub fn public_web_request(message:&str)->bool {
    let normalized=message.trim().to_lowercase();let mut text=normalized.as_str();
    for prefix in ["please ","can you ","could you ","please "] {text=text.strip_prefix(prefix).unwrap_or(text);}
    ["search the web ","search web ","search online ","search the internet ","look up online ","find online "].iter().any(|prefix|text.starts_with(prefix))
}
/// Cheap first pass. Multi-step requests use Grok 4.7 rather than
/// paying for a separate model call just to reword every message.
pub fn classify(message: &str) -> Work {
    if public_web_request(message) {return Work::Conversation;}
    let normalized = message.trim().to_lowercase();
    let mut m = normalized.as_str();
    for prefix in ["please ", "can you ", "could you ", "please "] {
        m = m.strip_prefix(prefix).unwrap_or(m);
    }
    let words: Vec<_> = m
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|s| !s.is_empty())
        .collect();
    let Some(first) = words.first() else {
        return Work::Conversation;
    };
    if ["ask gpt ","gpt review ","get a second opinion "].iter().any(|prefix|m.starts_with(prefix)){return Work::Specialist;}
    if ["research","implement","debug","code","refactor","investigate"].contains(first)
        || (["write","build","review"].contains(first) && words.iter().any(|w|["function","script","code","program","patch"].contains(w)))
        || message.contains("```") {return Work::Complex;}
    let technical_help=["how ","why ","help ","explain "].iter().any(|prefix|m.starts_with(prefix))
        && words.iter().any(|word|["implement","debug","crash","crashes","panic","panics","deadlock","stacktrace","refactor"].contains(word));
    if technical_help || m.contains("root cause") || words.len()>100 {return Work::Complex;}
    if ["explain", "what", "why", "hello", "hi", "tell"].contains(first) {
        return Work::Conversation;
    }
    if ["migrate", "deploy", "refactor"].contains(first)
        || m.contains("root cause")
        || m.contains("across my computers")
        || words.contains(&"multi-machine")
    {
        return Work::Complex;
    }
    let routine = [
        "create", "move", "rename", "free", "unload", "resume", "pause",
    ];
    if routine.contains(first) && !words.contains(&"and") && words.len() <= 24 {
        return Work::Routine;
    }
    let tools = [
        "arrange", "organize", "tile", "create", "move", "rename", "free", "unload", "resume", "pause", "open", "close", "list",
        "read", "find", "search", "check", "scan", "start", "stop", "install", "download",
        "connect", "run", "execute", "edit", "delete", "inspect", "diagnose", "fix",
    ];
    if tools.contains(first) {
        Work::MultiStep
    } else {
        Work::Conversation
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routes_by_work_not_personality() {
        assert_eq!(classify("tell me a joke"), Work::Conversation);
        assert_eq!(classify("happy birthday"), Work::Conversation);
        assert_eq!(classify("explain what a deployment is"), Work::Conversation);
        assert_eq!(classify("please free RAM"), Work::Routine);
        assert_eq!(classify("free ram"), Work::Routine);
        assert_eq!(
            classify("inspect network connections and explain the slowdown"),
            Work::MultiStep
        );
        assert_eq!(classify("deploy across my computers"), Work::Complex);
    }
    #[test]
    fn explicit_model_ids() {
        assert_eq!(Work::Routine.model(), "grok-4.20-0309-non-reasoning");
        assert_eq!(Work::MultiStep.model(), "grok-4.7");
        assert_eq!(classify("ask GPT to review this fix"),Work::Specialist);
        assert_eq!(classify("write a function to sort a list"),Work::Complex);
        assert_eq!(classify("research this architecture"),Work::Complex);
        assert_eq!(classify("How do I implement binary search in Rust?"),Work::Complex);
        assert_eq!(classify("Why does this function panic?"),Work::Complex);
        assert_eq!(classify("Explain the root cause of the crash"),Work::Complex);
        assert_eq!(classify("What is RAM?"),Work::Conversation);
    }
}
