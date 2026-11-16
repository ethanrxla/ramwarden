//! `ramwarden mcp`: a Model Context Protocol server over stdio.
//!
//! This is how an agent in Warp, Claude Code, Cursor or any other MCP client
//! reaches the warden: a handful of named tools with fixed inputs, each one a
//! call to the daemon on `127.0.0.1:7823`. The agent's own terminal can do
//! anything the user lets it; these tools are the part that is safe to call
//! without thinking, because of three rules.
//!
//! 1. **Reads are read-only.** Every tool but one only measures.
//! 2. **An agent proposes, a person approves.** `storage_propose_cleanup`
//!    prepares a clean-up and puts it in the warden's bubble. There is no
//!    tool that deletes. The user presses Free, or types yes to
//!    `ramwarden storage free`.
//! 3. **Paths stay on the computer.** Results carry names and sizes. The
//!    daemon leaves folder paths out of everything served here unless the
//!    user set `[storage] share_paths = true`, because whatever a tool
//!    returns goes to the agent's model, which is usually in the cloud.
//!
//! The protocol is JSON-RPC 2.0, one message per line. Only what tools need
//! is implemented: `initialize`, `ping`, `tools/list`, `tools/call`.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const DAEMON: &str = "http://127.0.0.1:7823";
/// The protocol revision offered when the client names one we do not know.
const PROTOCOL: &str = "2025-06-18";
const KNOWN_PROTOCOLS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

const INSTRUCTIONS: &str = "RamWarden's local tools for this computer. Use storage_status for free disk space, storage_usage for what is using it, and computer_probe for memory, apps, the GPU and local models. To free space, call storage_propose_cleanup: it deletes nothing and asks the user to approve in the warden's bubble. You cannot approve for them; tell them to press Free in the bubble or run `ramwarden storage free`, then check storage_cleanup_status. Places of kind review are the user's own files and kind guide needs the user to follow the listed steps: explain them, do not act on them yourself without the user asking.";

/// One tool: its declaration, and how a call becomes a daemon request.
struct Tool {
    name: &'static str,
    description: &'static str,
    /// JSON Schema properties and the required list.
    properties: fn() -> Value,
    required: &'static [&'static str],
    read_only: bool,
}

const TOOLS: &[Tool] = &[
    Tool {
        name: "storage_status",
        description: "Free and total space on each disk, and whether any is low or critical. Instant; no scan.",
        properties: || json!({}),
        required: &[],
        read_only: true,
    },
    Tool {
        name: "storage_usage",
        description: "What is using the disk: the largest folders in the user's home folder and known places (caches, temp files, build output, downloads, page file, Docker), each with its size, a kind (clear, review or guide) and steps. Reuses a scan under 10 minutes old; a new scan takes up to `seconds`. Sizes marked complete=false are minimums.",
        properties: || {
            json!({
                "refresh": {"type": "boolean", "description": "Scan again even if a recent scan exists."},
                "seconds": {"type": "integer", "minimum": 1, "maximum": 120, "description": "Time budget for a new scan. Default 8; use 60 or more for a full count of a large disk."}
            })
        },
        required: &[],
        read_only: true,
    },
    Tool {
        name: "storage_propose_cleanup",
        description: "Propose clearing regenerable places (kind clear from storage_usage). Deletes nothing: it creates a proposal and shows it in the warden's bubble for the user to approve or decline. Returns the proposal id, its lines and total size. Proposals expire after 15 minutes.",
        properties: || {
            json!({
                "items": {"type": "array", "items": {"type": "string"}, "description": "Ids of kind-clear places from storage_usage, e.g. [\"app-caches\", \"build-1\"]. Omit or leave empty to propose every clearable place of 50 MB or more."}
            })
        },
        required: &[],
        read_only: false,
    },
    Tool {
        name: "storage_cleanup_status",
        description: "Whether the user approved a proposal: pending, applied (with freed_bytes and what was kept), declined or expired.",
        properties: || json!({"id": {"type": "string", "description": "The proposal id from storage_propose_cleanup."}}),
        required: &["id"],
        read_only: true,
    },
    Tool {
        name: "computer_probe",
        description: "One read-only reading of the computer. overview: RAM, swap, pressure, largest apps. app: one app's processes, memory, CPU, VRAM, ports and redacted command lines (needs name). gpu: use, VRAM, temperature and who holds VRAM. models: local models loaded in Ollama, LM Studio, llama-server, vLLM. history: what RamWarden recently did to an app (needs name). storage: disks and what fills them, in brief.",
        properties: || {
            json!({
                "probe": {"type": "string", "enum": ["overview", "app", "gpu", "models", "history", "storage"]},
                "name": {"type": "string", "maxLength": 64, "description": "App or process name, for app and history."}
            })
        },
        required: &["probe"],
        read_only: true,
    },
];

fn declarations() -> Value {
    let tools: Vec<Value> = TOOLS
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": {"type": "object", "properties": (t.properties)(), "required": t.required, "additionalProperties": false},
                "annotations": {"readOnlyHint": t.read_only, "destructiveHint": false, "openWorldHint": false},
            })
        })
        .collect();
    json!({ "tools": tools })
}

/// The daemon request a tool call stands for: `(method, path, body, timeout)`.
/// Arguments are validated here; nothing from the agent reaches a URL or a
/// body unchecked.
fn request(name: &str, args: &Value) -> Result<(&'static str, String, Value, Duration), String> {
    let short = Duration::from_secs(15);
    match name {
        "storage_status" => Ok(("GET", "storage".into(), Value::Null, short)),
        "storage_usage" => {
            let seconds = args["seconds"].as_u64().map(|s| s.clamp(1, 120));
            let mut path = "storage/usage?for=agent".to_string();
            if args["refresh"].as_bool().unwrap_or(false) {
                path += "&refresh=true";
            }
            if let Some(s) = seconds {
                path += &format!("&seconds={s}");
            }
            Ok(("GET", path, Value::Null, Duration::from_secs(seconds.unwrap_or(8) + 20)))
        }
        "storage_propose_cleanup" => {
            let items: Vec<&str> = match &args["items"] {
                Value::Null => Vec::new(),
                Value::Array(a) => a.iter().map(|v| v.as_str().ok_or("items must be strings")).collect::<Result<_, _>>()?,
                _ => return Err("items must be an array of ids".into()),
            };
            if items.iter().any(|i| i.is_empty() || i.len() > 64 || !i.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')) {
                return Err("each item must be an id from storage_usage, such as app-caches".into());
            }
            Ok(("POST", "storage/cleanup/propose".into(), json!({ "items": items }), Duration::from_secs(40)))
        }
        "storage_cleanup_status" => {
            let id = args["id"].as_str().unwrap_or_default();
            if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
                return Err("id must be the proposal id from storage_propose_cleanup".into());
            }
            Ok(("GET", format!("storage/cleanup/{id}"), Value::Null, short))
        }
        "computer_probe" => {
            let probe = args["probe"].as_str().unwrap_or_default();
            if !["overview", "app", "gpu", "models", "history", "storage"].contains(&probe) {
                return Err("probe must be one of overview, app, gpu, models, history, storage".into());
            }
            let name = args["name"].as_str().map(str::trim).filter(|n| !n.is_empty());
            if matches!(probe, "app" | "history") && name.is_none_or(|n| n.len() > 64) {
                return Err(format!("{probe} needs a name of at most 64 characters"));
            }
            Ok(("POST", "ask/probe".into(), json!({"probe": probe, "name": name}), Duration::from_secs(40)))
        }
        _ => Err(format!("unknown tool {name}")),
    }
}

async fn call(http: &reqwest::Client, name: &str, args: &Value) -> Result<Value, String> {
    let (method, path, body, timeout) = request(name, args)?;
    let url = format!("{DAEMON}/{path}");
    let sent = if method == "GET" { http.get(url) } else { http.post(url).json(&body) }.timeout(timeout).send().await;
    let response = sent.map_err(|_| "The RamWarden daemon is not answering on 127.0.0.1:7823. Ask the user to start RamWarden (or run `ramwarden doctor`).".to_string())?;
    let status = response.status();
    let value: Value = response.json().await.map_err(|_| format!("The daemon answered {status} with something that is not JSON. It may be an older version without these tools."))?;
    if status.is_success() {
        Ok(value)
    } else {
        Err(value["error"].as_str().map(str::to_string).unwrap_or_else(|| format!("The daemon refused: {status}. It may be an older version without this tool.")))
    }
}

/// Answer one JSON-RPC message. `None` for notifications, which get no reply.
async fn answer(http: &reqwest::Client, message: &Value) -> Option<Value> {
    let id = message.get("id").filter(|i| !i.is_null())?.clone();
    let ok = |result: Value| Some(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    let fail = |code: i64, text: &str| Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": text}}));
    match message["method"].as_str().unwrap_or_default() {
        "initialize" => {
            let asked = message["params"]["protocolVersion"].as_str().unwrap_or_default();
            ok(json!({
                "protocolVersion": if KNOWN_PROTOCOLS.contains(&asked) { asked } else { PROTOCOL },
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "ramwarden", "title": "RamWarden", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => ok(json!({})),
        "tools/list" => ok(declarations()),
        "tools/call" => {
            let name = message["params"]["name"].as_str().unwrap_or_default();
            if !TOOLS.iter().any(|t| t.name == name) {
                return fail(-32602, &format!("unknown tool: {name}"));
            }
            // A failed call is a result the model can read and recover from,
            // not a protocol error.
            let (text, is_error) = match call(http, name, &message["params"]["arguments"]).await {
                Ok(v) => (v.to_string(), false),
                Err(e) => (e, true),
            };
            ok(json!({"content": [{"type": "text", "text": text}], "isError": is_error}))
        }
        _ => fail(-32601, "method not found"),
    }
}

/// Serve until stdin closes. Stdout carries only protocol messages.
pub async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let http = reqwest::Client::builder().build()?;
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) => answer(&http, &message).await,
            Err(_) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}})),
        };
        if let Some(reply) = reply {
            out.write_all(format!("{reply}\n").as_bytes()).await?;
            out.flush().await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http() -> reqwest::Client {
        reqwest::Client::new()
    }

    #[tokio::test]
    async fn the_handshake_lists_the_tools_and_none_of_them_deletes() {
        let init = answer(&http(), &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}})).await.unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(init["result"]["instructions"].as_str().unwrap().contains("You cannot approve"));
        let odd = answer(&http(), &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}})).await.unwrap();
        assert_eq!(odd["result"]["protocolVersion"], PROTOCOL);
        assert!(answer(&http(), &json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await.is_none());

        let list = answer(&http(), &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).await.unwrap();
        let tools = list["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), TOOLS.len());
        for t in tools {
            let name = t["name"].as_str().unwrap();
            assert!(!name.contains("apply") && !name.contains("delete") && !name.contains("free"), "{name}");
            assert_eq!(t["annotations"]["destructiveHint"], false);
            assert_eq!(t["inputSchema"]["type"], "object");
        }
        assert_eq!(answer(&http(), &json!({"jsonrpc":"2.0","id":3,"method":"resources/list"})).await.unwrap()["error"]["code"], -32601);
    }

    #[test]
    fn calls_map_to_fixed_daemon_routes_and_never_to_apply() {
        assert_eq!(request("storage_status", &json!({})).unwrap().1, "storage");
        assert_eq!(request("storage_usage", &json!({"refresh":true,"seconds":900})).unwrap().1, "storage/usage?for=agent&refresh=true&seconds=120");
        let (method, path, body, _) = request("storage_propose_cleanup", &json!({"items":["app-caches","build-1"]})).unwrap();
        assert_eq!((method, path.as_str()), ("POST", "storage/cleanup/propose"));
        assert_eq!(body, json!({"items":["app-caches","build-1"]}));
        assert_eq!(request("storage_cleanup_status", &json!({"id":"3f2a-00"})).unwrap().1, "storage/cleanup/3f2a-00");
        assert_eq!(request("computer_probe", &json!({"probe":"app","name":" ollama "})).unwrap().2, json!({"probe":"app","name":"ollama"}));
        for t in TOOLS {
            let args = json!({"id":"abc","probe":"gpu"});
            assert!(!request(t.name, &args).unwrap().1.contains("apply"));
        }
    }

    #[test]
    fn arguments_that_could_steer_a_route_are_refused() {
        assert!(request("storage_cleanup_status", &json!({"id":"x/apply"})).is_err());
        assert!(request("storage_cleanup_status", &json!({"id":"../storage"})).is_err());
        assert!(request("storage_cleanup_status", &json!({})).is_err());
        assert!(request("storage_propose_cleanup", &json!({"items":["../../etc"]})).is_err());
        assert!(request("storage_propose_cleanup", &json!({"items":"app-caches"})).is_err());
        assert!(request("computer_probe", &json!({"probe":"shell"})).is_err());
        assert!(request("computer_probe", &json!({"probe":"app"})).is_err());
        assert!(request("run_command", &json!({})).is_err());
    }
}
