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
    let m = message.to_lowercase();
    if [
        "migrate",
        "multi-machine",
        "across my computers",
        "deploy",
        "refactor",
        "root cause",
    ]
    .iter()
    .any(|w| m.contains(w))
    {
        return Work::Complex;
    }
    if [
        "terminal", "command", "script", "folder", "file", "install", "download", "network", "ssh",
        "browser", "tab", "ram", "memory", "app",
    ]
    .iter()
    .any(|w| m.contains(w))
    {
        if [
            "create ", "move ", "rename ", "free ", "unload ", "resume ", "pause ",
        ]
        .iter()
        .any(|w| m.starts_with(w))
            && !m.contains(" and ")
        {
            Work::Routine
        } else {
            Work::MultiStep
        }
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
