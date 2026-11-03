//! Opt-in hosted model routing; executor permissions stay with the caller.
use crate::routing::Work;
use serde_json::{Value, json};
use std::{sync::OnceLock, time::Duration};
static HTTP: OnceLock<reqwest::Client> = OnceLock::new();
fn http() -> Option<&'static reqwest::Client> {
    if HTTP.get().is_none() {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .ok()?;
        let _ = HTTP.set(client);
    }
    HTTP.get()
}
pub fn enabled() -> bool {
    std::env::var("RAMWARDEN_ROUTED_ASSISTANT").ok().as_deref() == Some("1")
}
pub async fn explain(work: Work, messages: &[Value]) -> Option<String> {
    if !enabled() {
        return None;
    }
    let (origin, key_name, model_name) = if work == Work::Conversation {
        ("https://api.x.ai", "XAI_API_KEY", "XAI_MODEL_ID")
    } else {
        (
            "https://api.openai.com",
            "OPENAI_API_KEY",
            match work {
                Work::Routine => "WARDEN_LUNA_MODEL",
                Work::MultiStep => "WARDEN_TERRA_MODEL",
                _ => "WARDEN_SOL_MODEL",
            },
        )
    };
    let key = crate::secret::Secret::new(std::env::var(key_name).ok()?);
    if key.is_empty() {
        return None;
    }
    let model = std::env::var(model_name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| work.model().into());
    let r=http()?.post(format!("{origin}/v1/responses")).bearer_auth(key.expose()).json(&json!({
  "model":model,"input":messages,"store":false,"max_output_tokens":1200,"text":{"format":{"type":"json_object"}}
 })).send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    // Cap the response before parsing; do not log provider bodies or keys.
    if r.content_length().is_some_and(|n| n > 128 * 1024) {
        return None;
    }
    let mut r = r;
    let mut bytes = Vec::new();
    while let Some(chunk) = r.chunk().await.ok()? {
        if bytes.len() + chunk.len() > 128 * 1024 {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    let body: Value = serde_json::from_slice(&bytes).ok()?;
    output_text(&body)
}
fn output_text(body: &Value) -> Option<String> {
    let mut text = String::new();
    for item in body.get("output")?.as_array()? {
        if item["type"] != "message" {
            continue;
        }
        for content in item.get("content")?.as_array()? {
            if content["type"] == "output_text" {
                text.push_str(content["text"].as_str()?);
            }
        }
    }
    if text.is_empty() || text.len() > 24000 {
        return None;
    }
    let json: Value = serde_json::from_str(&text).ok()?;
    if !json.get("reply").is_some_and(Value::is_string) {
        return None;
    }
    Some(text)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reasoning_is_not_chat() {
        assert!(
            output_text(&json!({"output":[{"type":"reasoning","content":[{"text":"secret"}]}]}))
                .is_none()
        );
    }
    #[test]
    fn only_structured_reply_passes() {
        assert_eq!(
            output_text(
                &json!({"output":[{"type":"message","content":[{"type":"output_text","text":"{\"reply\":\"Hello\"}"}]}]})
            ),
            Some("{\"reply\":\"Hello\"}".into())
        );
        assert!(output_text(&json!({"output":[{"type":"message","content":[{"type":"output_text","text":"plain text"}]}]})).is_none());
    }
}
