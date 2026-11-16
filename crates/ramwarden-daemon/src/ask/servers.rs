//! Local model servers other than Ollama and LM Studio.
//!
//! The Windows PC serves its models from `llama-server` (llama.cpp). The chat
//! only asked Ollama, so "which model is running?" had no answer there. These
//! servers all speak the OpenAI-compatible `/v1/models` route, so RamWarden
//! finds them by process name, asks each of its listening ports, and reports
//! what that server says it has loaded. llama.cpp also reports its slots, which
//! tell whether it is generating right now.
//!
//! Only loopback is asked, with a short timeout. A server that does not answer
//! is reported as running with its model unknown, never as idle.

use std::time::Duration;

use serde_json::{Value, json};

/// Process names (lowercase, without `.exe`) of OpenAI-compatible servers.
const SERVER_NAMES: &[&str] = &[
    "llama-server",
    "llama_server",
    "server", // llama.cpp's older binary name; only counted with a model flag
    "llamafile",
    "koboldcpp",
    "text-generation-launcher",
    "text-generation-server",
    "tabbyapi",
    "localai",
    "jan",
];

const TIMEOUT: Duration = Duration::from_millis(1500);
/// Ports asked per server. A server listens on one or two.
const MAX_PORTS: usize = 2;

/// A local server process found in the process table.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub pid: i32,
    pub name: String,
    pub ports: Vec<u16>,
    /// The redacted command line, which usually names the model file.
    pub command: String,
}

/// Whether a process is a model server worth asking.
pub fn is_model_server(name: &str, redacted_command: &str) -> bool {
    let n = name.trim_end_matches(".exe").to_ascii_lowercase();
    let cmd = redacted_command.to_ascii_lowercase();
    if n == "server" {
        return cmd.contains(" -m ") || cmd.contains("--model") || cmd.contains(".gguf");
    }
    SERVER_NAMES.contains(&n.as_str())
        || ((n.starts_with("python") || n == "vllm") && cmd.contains("vllm"))
        || (n.starts_with("python") && cmd.contains("sglang"))
}

/// Ask every found server what it has loaded. Never fails: a server that
/// does not answer is listed with `"answered": false`.
pub async fn inspect(found: Vec<Found>) -> Vec<Value> {
    let Ok(client) = reqwest::Client::builder().timeout(TIMEOUT).no_proxy().build() else {
        return Vec::new();
    };
    let asks = found.into_iter().map(|f| {
        let client = client.clone();
        async move {
            for port in f.ports.iter().take(MAX_PORTS) {
                let base = format!("http://127.0.0.1:{port}");
                let Some(models) = get(&client, &format!("{base}/v1/models")).await else { continue };
                let ids = model_ids(&models);
                // llama.cpp only: whether a slot is generating now.
                let busy = get(&client, &format!("{base}/slots")).await.and_then(|v| busy_slots(&v));
                return json!({"server":f.name,"pid":f.pid,"port":port,"answered":true,
                    "models":ids,"busy_slots":busy,"command":f.command});
            }
            json!({"server":f.name,"pid":f.pid,"ports":f.ports,"answered":false,"command":f.command,
                "note":"Running, but it did not answer on its ports; its loaded model is unknown."})
        }
    });
    futures::future::join_all(asks).await
}

async fn get(client: &reqwest::Client, url: &str) -> Option<Value> {
    let r = client.get(url).send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.json().await.ok()
}

/// `{"data":[{"id":"qwen3-8b"}]}` (OpenAI) or `{"models":[{"name":..}]}`.
pub fn model_ids(v: &Value) -> Vec<String> {
    let rows = v["data"].as_array().or_else(|| v["models"].as_array());
    rows.into_iter()
        .flatten()
        .filter_map(|m| m["id"].as_str().or_else(|| m["name"].as_str()).or_else(|| m["model"].as_str()))
        .map(super::redact::command_line)
        .take(8)
        .collect()
}

/// How many llama.cpp slots are generating, from `/slots`.
pub fn busy_slots(v: &Value) -> Option<usize> {
    let slots = v.as_array()?;
    Some(
        slots
            .iter()
            .filter(|s| s["is_processing"].as_bool().unwrap_or(false) || s["state"].as_i64().is_some_and(|n| n != 0))
            .count(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llama_server_is_recognised_and_plain_servers_are_not() {
        assert!(is_model_server("llama-server.exe", "llama-server.exe -m q.gguf"));
        assert!(is_model_server("server", "server -m qwen.gguf --port 8080"));
        assert!(!is_model_server("server", "server --port 8080"));
        assert!(is_model_server("python3", "python3 -m vllm.entrypoints.openai.api_server"));
        assert!(!is_model_server("python3", "python3 manage.py runserver"));
        assert!(!is_model_server("chrome", "chrome"));
    }

    #[test]
    fn model_ids_and_slots_are_read() {
        let v = json!({"object":"list","data":[{"id":"C:\\Users\\ethan\\models\\qwen3-8b.gguf"},{"id":"gemma"}]});
        assert_eq!(model_ids(&v), vec!["qwen3-8b.gguf", "gemma"]);
        let slots = json!([{"id":0,"is_processing":true},{"id":1,"is_processing":false}]);
        assert_eq!(busy_slots(&slots), Some(1));
        assert_eq!(busy_slots(&json!({"error":"disabled"})), None);
    }
}
