//! Hosted model routing; executor permissions stay with the caller.
use crate::routing::Work;
use serde_json::{Value, json};
use std::{sync::OnceLock, time::Duration};
static HTTP: OnceLock<reqwest::Client> = OnceLock::new();
fn http() -> Option<&'static reqwest::Client> {
    if HTTP.get().is_none() {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(40))
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
// Short conversation must not acquire a sidecar dependency from system telemetry.
fn needs_compression(work: Work, messages: &[Value]) -> bool {
    let request_bytes=messages.iter().rev().find(|m|m["role"]=="user").and_then(|m|m["content"].as_str()).map(str::len).unwrap_or(0);
    request_bytes >= 8000 || (work != Work::Conversation && messages.iter().map(|m|m.to_string().len()).sum::<usize>() >= 8000)
}
pub async fn explain(work: Work, messages: &[Value]) -> Option<String> {
    if !enabled() {
        return None;
    }
    let compress = needs_compression(work, messages);
    tracing::info!(work=?work, compression_required=compress, "warden model route");
    let prepared = if compress { crate::nyquest::compress(messages).await } else { Ok(messages.to_vec()) };
    let messages = match prepared {
        Ok(messages) => messages,
        Err(_) => {
            tracing::warn!("warden model request stopped: required compression unavailable");
            return None;
        }
    };
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
    let payload = request_body(work, &model, &messages);
    let r = match http()?
        .post(format!("{origin}/v1/responses"))
        .bearer_auth(key.expose())
        .json(&payload)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(
                model = model.as_str(),
                timeout = error.is_timeout(),
                connect = error.is_connect(),
                "warden model request failed"
            );
            return None;
        }
    };
    if !r.status().is_success() {
        tracing::warn!(
            model = model.as_str(),
            status = r.status().as_u16(),
            "warden model provider refused request"
        );
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
    if let Some(usage) = body.get("usage") {
        tracing::info!(
            provider = origin,
            model = model.as_str(),
            input_tokens = usage["input_tokens"].as_u64().unwrap_or(0),
            output_tokens = usage["output_tokens"].as_u64().unwrap_or(0),
            cost_in_usd_ticks = usage["cost_in_usd_ticks"].as_u64(),
            "warden model usage"
        );
    }
    let reply = output_text(&body);
    if reply.is_none() {
        tracing::warn!(
            model = model.as_str(),
            "warden model returned no valid structured reply"
        );
    }
    reply
}
fn request_body(work: Work, model: &str, messages: &[Value]) -> Value {
    let mut body = json!({"model":model,"input":messages,"store":false,"max_output_tokens":1200,"text":{"format":{"type":"json_object"}}});
    // High reasoning is the provider default; simple pet conversation should
    // not spend its entire deadline thinking before producing visible text.
    if work == Work::Conversation {
        body["reasoning"] = json!({"effort":"low"});
    }
    body
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
    fn conversation_uses_low_reasoning_without_changing_planner_effort() {
        let body = request_body(Work::Conversation, "grok-4.6", &[]);
        assert_eq!(body["reasoning"]["effort"], "low");
        assert_eq!(body["store"], false);
        assert!(
            request_body(Work::Complex, "planner", &[])
                .get("reasoning")
                .is_none()
        );
    }
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

#[cfg(test)]
mod compression_routing_tests {
    use super::*;
    #[test]
    fn short_chat_ignores_large_telemetry_for_compression() {
        let messages=vec![json!({"role":"system","content":"Security report ".repeat(2000)}),json!({"role":"user","content":"can you shut the fuck up dumbass goat"})];
        assert!(!needs_compression(Work::Conversation,&messages));
        assert!(needs_compression(Work::Complex,&messages));
    }
    #[test]
    fn long_user_requests_still_require_compression() {
        assert!(needs_compression(Work::Conversation,&[json!({"role":"user","content":"x".repeat(8000)})]));
        assert!(!needs_compression(Work::Routine,&[json!({"role":"user","content":"create notes.txt"})]));
    }
}
