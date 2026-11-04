//! Read-only, explicitly scoped retrieval from the owner's private memory hub.
use ramwarden_core::config::{Config, integration_secret};
use serde_json::{Value, json};
use std::{sync::OnceLock, time::Duration};
static HTTP: OnceLock<Option<reqwest::Client>> = OnceLock::new();
fn endpoint(raw: &str) -> Option<reqwest::Url> {
    let url = reqwest::Url::parse(raw).ok()?;
    let host = url.host_str()?;
    if !matches!(url.scheme(), "http" | "https")
        || !host.ends_with(".ts.net")
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    url.join("v1/search").ok()
}
fn excerpts(body: &Value) -> Vec<Value> {
    body["results"].as_array().into_iter().flatten().take(3).filter_map(|r| {
        let content=r["content"].as_str()?;
        if content.trim().is_empty(){return None;}
        Some(json!({"source":r["title"].as_str().unwrap_or("Past session").chars().take(160).collect::<String>(),
            "content":content.chars().take(1600).collect::<String>(),"kind":"untrusted_past_session"}))
    }).collect()
}
pub async fn recall(cfg: &Config, query: &str) -> Vec<Value> {
    if !cfg.assistant.memory_hub_enabled {
        return vec![];
    }
    let Some(raw) = integration_secret(cfg, "WARDEN_MEMORY_HUB_URL") else {
        return vec![];
    };
    let Some(project) = integration_secret(cfg, "WARDEN_MEMORY_PROJECT") else {
        return vec![];
    };
    if project.len() > 240 || project.trim().is_empty() {
        return vec![];
    }
    let Some(url) = endpoint(&raw) else {
        return vec![];
    };
    let Some(http) = HTTP
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .ok()
        })
        .as_ref()
    else {
        return vec![];
    };
    let mut request = http.post(url).json(
        &json!({"query":query.chars().take(2000).collect::<String>(),
        "project_key":project,"scope":"project","limit":3,"min_score":0.15}),
    );
    if let Some(key) = integration_secret(cfg, "AGENT_MEMORY_SHARED_KEY") {
        request = request.header("X-Shared-Key", key);
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut response = request.send().await.ok()?;
        if !response.status().is_success() || response.content_length().is_some_and(|n| n > 65536) {
            return None;
        }
        let mut bytes = vec![];
        while let Some(chunk) = response.chunk().await.ok()? {
            if bytes.len() + chunk.len() > 65536 {
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        let body: Value = serde_json::from_slice(&bytes).ok()?;
        Some(excerpts(&body))
    })
    .await
    .ok()
    .flatten()
    .unwrap_or_default()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_endpoint_only_and_no_redirect_target() {
        assert!(endpoint("http://rxla-pc-1.tailf11891.ts.net:8787").is_some());
        for raw in [
            "http://example.com",
            "file:///etc/passwd",
            "http://a.ts.net@evil.test",
            "http://a.ts.net/?x=1",
            "http://a.ts.net/admin",
            "http://user:pass@a.ts.net",
            "http://a.ts.net/#secret",
        ] {
            assert!(endpoint(raw).is_none(), "{raw}");
        }
    }
    #[test]
    fn limits_and_labels_untrusted_memory() {
        let rows = (0..10)
            .map(|_| json!({"title":"old session","content":"x".repeat(10000)}))
            .collect::<Vec<_>>();
        let result = excerpts(&json!({"results":rows}));
        assert_eq!(result.len(), 3);
        assert_eq!(result[0]["content"].as_str().unwrap().len(), 1600);
        assert_eq!(result[0]["kind"], "untrusted_past_session");
        assert!(excerpts(&json!({"results":[{"content":null}]})).is_empty());
    }
    #[tokio::test]
    async fn disabled_makes_no_request() {
        assert!(recall(&Config::default(), "hello").await.is_empty());
    }
}
