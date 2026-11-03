//! Model choice is advisory, never authority to execute a tool.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Work {
    Conversation,
    Routine,
    MultiStep,
    Complex,
}
impl Work {
    pub fn model(self) -> &'static str {
        match self {
            Self::Conversation => "grok-4.6",
            Self::Routine => "gpt-6-luna",
            Self::MultiStep => "gpt-6.1-terra",
            Self::Complex => "gpt-6-sol",
        }
    }
}
/// Cheap first pass. Unknown requests that need tools use Terra rather than
/// paying for a separate model call just to reword every message.
pub fn classify(message: &str) -> Work {
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
        "create", "move", "rename", "free", "unload", "resume", "pause", "open", "close", "list",
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
        assert_eq!(Work::Routine.model(), "gpt-6-luna");
        assert_eq!(Work::MultiStep.model(), "gpt-6.1-terra");
    }
}
