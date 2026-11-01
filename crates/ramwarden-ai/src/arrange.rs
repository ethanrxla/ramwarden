//! The prompt that turns "put Slack on 3 and float Spotify" into commands.
//!
//! The model sees the current windows (id, app, title, workspace), the
//! workspaces and monitors, and optionally the config file, and answers in the
//! same `{"op": …}` vocabulary as `ramwarden_platform::WmCommand`. Its reply
//! is a *proposal*: `ramwarden_core::arrange` validates it like any other and
//! the user approves it before anything moves. So the prompt asks for
//! precision rather than caution; caution is enforced downstream.

use serde_json::{Value, json};

pub const SYSTEM_PROMPT: &str = "\
You rearrange a user's desktop through their tiling window manager. \
Reply with one JSON object and nothing else: \
{\"summary\": string, \"commands\": [command, ...], \"config\": string or null}. \
Each command is one of: \
{\"op\":\"focus\",\"window\":ID} \
{\"op\":\"move_to_workspace\",\"window\":ID,\"workspace\":NAME} \
{\"op\":\"switch_workspace\",\"workspace\":NAME} \
{\"op\":\"move_workspace_to_monitor\",\"workspace\":NAME,\"monitor\":MONITOR} \
{\"op\":\"set_layout\",\"window\":ID,\"layout\":\"tiles|accordion|horizontal|vertical|floating|tiling\"} \
{\"op\":\"balance\",\"workspace\":NAME}. \
Use only window ids and monitor names from the state you are given. \
Workspace names may be new. You cannot close, quit or kill anything; if asked to, \
say so in the summary and return no commands for it. \
Set \"config\" only when the user asks to change their window manager config: \
then it is the complete new file, keeping everything you were not asked to change. \
Never add lines that start programs (exec, exec-and-forget, after-startup-command). \
The summary is one plain sentence the user reads before approving.";

/// The JSON schema for the reply, for backends that enforce one.
pub fn reply_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "summary": {"type": "string"},
            "commands": {"type": "array", "items": {"type": "object"}},
            "config": {"type": ["string", "null"]}
        },
        "required": ["summary", "commands"]
    })
}

/// The user message: the request, the desktop, and the config if editing it.
///
/// Titles are cut to 80 characters and the window list to 120 entries: the
/// local model's context is small, and a browser's tab titles are long.
pub fn render(request: &str, state: &Value, config: Option<&str>) -> String {
    let mut s = String::new();
    s.push_str("Request: ");
    s.push_str(request.trim());
    s.push_str("\n\nWindow manager: ");
    s.push_str(state["kind"].as_str().unwrap_or("unknown"));
    s.push_str("\n\nWindows (id | app | title | workspace):\n");
    for w in state["windows"].as_array().into_iter().flatten().take(120) {
        let title: String = w["title"].as_str().unwrap_or_default().chars().take(80).collect();
        s.push_str(&format!(
            "{} | {} | {} | {}{}\n",
            w["id"],
            w["app"].as_str().unwrap_or_default(),
            title,
            w["workspace"].as_str().unwrap_or_default(),
            if w["focused"] == true { " (focused)" } else { "" },
        ));
    }
    s.push_str("\nWorkspaces (name | monitor):\n");
    for ws in state["workspaces"].as_array().into_iter().flatten() {
        s.push_str(&format!(
            "{} | {}{}\n",
            ws["name"].as_str().unwrap_or_default(),
            ws["monitor"].as_str().unwrap_or("-"),
            if ws["focused"] == true { " (focused)" } else { "" },
        ));
    }
    s.push_str("\nMonitors: ");
    let mons: Vec<&str> = state["monitors"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    s.push_str(&mons.join(", "));
    s.push('\n');
    if let Some(c) = config {
        s.push_str("\nCurrent config file:\n```\n");
        s.push_str(c);
        s.push_str("\n```\n");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_message_lists_ids_so_the_model_can_name_them() {
        let state = json!({
            "kind": "aerospace",
            "windows": [{"id": 4242, "app": "Slack", "title": "general", "workspace": "1", "focused": true}],
            "workspaces": [{"name": "1", "monitor": "Built-in", "focused": true}],
            "monitors": ["Built-in"]
        });
        let m = render("put slack on 3", &state, None);
        assert!(m.contains("4242 | Slack | general | 1 (focused)"));
        assert!(m.contains("Monitors: Built-in"));
        assert!(!m.contains("config file"));
        assert!(render("gaps", &state, Some("gaps.inner.horizontal = 4")).contains("gaps.inner.horizontal = 4"));
    }
}
