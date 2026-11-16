//! Questions about the computer, answered from live readings.
//!
//! "What is my GPU doing?" and "what is ollama doing?" used to be answered
//! from one fixed snapshot: RAM and the ten largest process names. It had no
//! CPU, GPU, children, ports or model servers, so the model guessed or gave
//! up, and at over 8 KB it made the router demand a compression sidecar that
//! the PC does not have (2026-11-14).
//!
//! Now the warden reads only what the question needs, through read-only
//! [`probes`]:
//!
//! 1. The daemon picks probes from the question itself ([`probes::pick`]):
//!    "GPU" reads the GPU, a running app's name reads that app, "model" reads
//!    every model runtime. The small `overview` probe always runs.
//! 2. The model answers from those readings. If it needs more, it may ask for
//!    up to [`MAX_EXTRA`] more probes, once ([`Readings::requested`]).
//! 3. The reply says what was looked at.
//!
//! Without a model (RamWarden Lite, or every model unavailable) the same
//! readings are worded by [`plain`], so the question still gets an answer.
//!
//! Probes never change anything, and command lines are redacted
//! ([`redact`]) before they are shown to any model. Spec:
//! `docs/specs/ask-the-warden.md`.

pub mod devices;
pub mod plain;
pub mod probes;
pub mod redact;
pub mod servers;

pub use probes::Probe;

use serde_json::Value;

use crate::hub::AppState;

/// Extra probes the model may request, in its one extra round.
pub const MAX_EXTRA: usize = 3;

/// Every probe run for one question, and what each returned.
#[derive(Default)]
pub struct Readings {
    probes: Vec<Probe>,
    results: Vec<Value>,
}

impl Readings {
    /// The overview, plus whatever the question names.
    pub async fn gather(st: &AppState, message: &str) -> Self {
        let names = st.det.try_read().ok().map(|det| probes::running_names(&det)).unwrap_or_default();
        // Tailscale is only asked when the question could be about another
        // device; most questions never spawn it.
        let known = if devices::may_concern_devices(message) { devices::list().await.unwrap_or_default() } else { Vec::new() };
        let mut wanted = vec![Probe::Overview];
        wanted.extend(probes::pick(message, &names, &known));
        let mut r = Readings::default();
        r.run(st, wanted).await;
        r
    }

    /// Run probes not already run, concurrently. Returns how many were new.
    pub async fn run(&mut self, st: &AppState, wanted: Vec<Probe>) -> usize {
        let mut fresh: Vec<Probe> = Vec::new();
        for p in wanted {
            if !self.probes.contains(&p) && !fresh.contains(&p) {
                fresh.push(p);
            }
        }
        let results = futures::future::join_all(fresh.iter().map(|p| probes::run(st, p))).await;
        let n = fresh.len();
        self.probes.extend(fresh);
        self.results.extend(results);
        n
    }

    /// Probes the model asked for in `{"probes":[...]}`, validated and capped.
    /// Only honoured when it gave no answer yet: a reply wins.
    pub fn requested(parsed: &Value) -> Vec<Probe> {
        if parsed["reply"].as_str().is_some_and(|r| !r.trim().is_empty()) {
            return Vec::new();
        }
        parsed["probes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Probe::from_request)
            .take(MAX_EXTRA)
            .collect()
    }

    /// The readings, for the model's prompt.
    pub fn prompt(&self) -> String {
        self.results.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")
    }

    /// What was looked at, for the line under the reply. The overview is
    /// implied unless it was all there was.
    pub fn looked_at(&self) -> Vec<String> {
        let named: Vec<String> = self.probes.iter().filter(|p| **p != Probe::Overview).map(Probe::label).collect();
        if named.is_empty() { vec![Probe::Overview.label()] } else { named }
    }

    /// Whether the question named anything beyond the overview.
    pub fn is_specific(&self) -> bool {
        self.probes.iter().any(|p| *p != Probe::Overview)
    }

    /// The readings as plain sentences, specific probes first.
    pub fn plain(&self) -> String {
        let mut rows: Vec<&Value> = self.results.iter().filter(|v| v["probe"] != "overview").collect();
        rows.extend(self.results.iter().filter(|v| v["probe"] == "overview"));
        rows.into_iter().filter_map(plain::describe).collect::<Vec<_>>().join(" ")
    }
}

impl Readings {
    /// Whether this turn looked at disk space.
    pub fn looked_at_storage(&self) -> bool {
        self.probes.contains(&Probe::Storage)
    }
}

/// `POST /ask/probe`: run one read-only probe and return its reading. The
/// same probes the warden's chat answers from, for local agents (the tool
/// server in `ramwarden mcp`). Body: `{"probe":"gpu"}` or
/// `{"probe":"app","name":"ollama"}`.
pub fn routes() -> axum::Router<AppState> {
    axum::Router::new().route("/ask/probe", axum::routing::post(probe))
}

async fn probe(
    axum::extract::State(st): axum::extract::State<AppState>,
    axum::Json(request): axum::Json<Value>,
) -> Result<axum::Json<Value>, (axum::http::StatusCode, axum::Json<Value>)> {
    match Probe::from_request(&request) {
        Some(p) => Ok(axum::Json(probes::run(&st, &p).await)),
        None => Err((
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({"error":"unknown probe","probes":["overview","app","gpu","models","history","storage","hardware","devices","device"],"note":"app, history and device need a name of at most 64 characters"})),
        )),
    }
}

/// The reply with the "Looked at" line appended.
pub fn with_sources(reply: &str, readings: &Readings) -> String {
    format!("{}\n\nLooked at: {}.", reply.trim_end(), readings.looked_at().join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn model_requests_are_honoured_only_without_a_reply() {
        let ask = json!({"reply":"","probes":[{"probe":"gpu"},{"probe":"app","name":"ollama"},{"probe":"models"},{"probe":"overview"}]});
        assert_eq!(Readings::requested(&ask), vec![Probe::Gpu, Probe::App("ollama".into()), Probe::Models]);
        let answered = json!({"reply":"Your GPU is idle.","probes":[{"probe":"gpu"}]});
        assert!(Readings::requested(&answered).is_empty());
        assert!(Readings::requested(&json!({"probes":"gpu"})).is_empty());
    }

    #[test]
    fn looked_at_names_the_specific_probes() {
        let r = Readings { probes: vec![Probe::Overview, Probe::Gpu, Probe::App("ollama".into())], results: vec![] };
        assert_eq!(r.looked_at(), vec!["GPU", "ollama"]);
        assert_eq!(with_sources("Busy. ", &r), "Busy.\n\nLooked at: GPU, ollama.");
        let only = Readings { probes: vec![Probe::Overview], results: vec![] };
        assert_eq!(only.looked_at(), vec!["memory"]);
    }
}
