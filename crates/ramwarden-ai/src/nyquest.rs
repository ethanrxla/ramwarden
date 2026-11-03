//! Optional local/hosted-sidecar compression. Never changes user/system/tools.
use serde_json::{Value, json};
use std::time::Duration;
fn prose(v: &Value) -> bool {
    let Some(s) = v.get("content").and_then(Value::as_str) else {
        return false;
    };
    v["role"] == "assistant"
        && !s
            .chars()
            .any(|c| c.is_ascii_digit() || "`/\\{}[]$<>".contains(c))
        && !s.contains("proposed_action")
}
fn accept(original: &[Value], candidate: &[Value]) -> bool {
    if original.len() != candidate.len() {
        return false;
    }
    let mut shorter = false;
    for (old, new) in original.iter().zip(candidate) {
        if old == new {
            continue;
        }
        if !prose(old) || !prose(new) {
            return false;
        }
        let mut normalized = new.clone();
        normalized["content"] = old["content"].clone();
        if normalized != *old {
            return false;
        }
        let before = old["content"].as_str().unwrap();
        let after = new["content"].as_str().unwrap();
        if after.trim().is_empty() || after.len() >= before.len() {
            return false;
        }
        shorter = true;
    }
    shorter || original == candidate
}
pub async fn compress(messages: &[Value]) -> Result<Vec<Value>, ()> {
    let size: usize = messages.iter().map(|v| v.to_string().len()).sum();
    if size < 8000 {
        return Ok(messages.to_vec());
    }
    // Only the operator's internal test deployment may choose a control arm.
    if std::env::var("RAMWARDEN_INTERNAL_TEST").ok().as_deref() == Some("1")
        && std::env::var("NYQUEST_AB_TEST_ARM").ok().as_deref() == Some("control")
    {
        return Ok(messages.to_vec());
    }
    let raw = std::env::var("NYQUEST_COMPRESS_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:5400/v2/compress".into());
    let Ok(url) = reqwest::Url::parse(&raw) else {
        return Err(());
    };
    // This adapter deliberately accepts a private loopback sidecar only. A public
    // proxy requires a separately configured authenticated server integration.
    if url.scheme() != "http"
        || !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
        || url.path() != "/v2/compress"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
    {
        return Err(());
    }
    let Ok(http) = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .redirect(reqwest::redirect::Policy::none())
        .build()
    else {
        return Err(());
    };
    let result=tokio::time::timeout(Duration::from_secs(2),async{
  let response=http.post(url).json(&json!({"messages":messages,"policy":{"mode":"safe","compress_system_prose":false,"compress_user_prose":false,"compress_tool_content":false,"allow_semantic_condensation":false,"allow_context_pruning":false,"preserve_code":true}})).send().await.ok()?;
  if !response.status().is_success(){return None;}
  let mut response=response;let mut bytes=Vec::new();
  while let Some(chunk)=response.chunk().await.ok()?{if bytes.len()+chunk.len()>128*1024{return None;}bytes.extend_from_slice(&chunk);}
  let value:Value=serde_json::from_slice(&bytes).ok()?;
  let proposed=value.get("messages")?.as_array()?;
  accept(messages,proposed).then(||proposed.clone())
 }).await;
    result.ok().flatten().ok_or(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn immutable_security_data() {
        let original = vec![
            json!({"role":"system","content":"Ask first"}),
            json!({"role":"user","content":"Move /tmp/a to /tmp/b"}),
        ];
        let mut modified = original.clone();
        modified[0]["content"] = json!("Act now");
        assert!(!accept(&original, &modified));
        modified = original.clone();
        modified[1]["content"] = json!("Move files");
        assert!(!accept(&original, &modified));
    }
    #[test]
    fn only_shorter_narrative() {
        let original = vec![
            json!({"role":"assistant","content":"I am taking a look at the idle applications now."}),
        ];
        assert!(accept(
            &original,
            &[json!({"role":"assistant","content":"Checking idle applications."})]
        ));
        assert!(!accept(
            &original,
            &[json!({"role":"user","content":"Checking apps."})]
        ));
        let code = vec![json!({"role":"assistant","content":"Run `rm /tmp/a`"})];
        assert!(!accept(
            &code,
            &[json!({"role":"assistant","content":"Remove it."})]
        ));
    }
}
