//! Copy this machine's decision log into MongoDB, embed it, and feed what the
//! embeddings recall back to the daemon.
//!
//! This runs as its own process (`ramwarden-cloud-db decisions sync`), never
//! inside the daemon: the daemon stays free of a MongoDB driver and of any
//! network wait, and keeps a complete log on disk whether or not this runs.
//!
//! # Steps
//!
//! 1. **Sync.** Read local decisions with `rev` past the saved cursor (new rows
//!    and rows whose outcome arrived since), and ladder actions past their own
//!    cursor; turn them into [`DecisionDoc`]s; upsert. The cursor is saved
//!    only after Atlas accepted the batch, so a failed run resends, and the
//!    upsert makes resending harmless.
//! 2. **Embed.** Tab and app decisions get a `text_vector` from the local
//!    Ollama (`[ollama] embed_model`, mxbai-embed-large). No Ollama: documents
//!    go up without one, and recall is skipped. Degrade, don't fail.
//! 3. **Recall.** For each tab the daemon would consider closing now, find the
//!    nearest settled past closes by title embedding and count how many the
//!    user reopened. Write `recall.json` beside the history database; the
//!    sweep holds back a close when most of a tab's neighbours were reopened
//!    (`ramwarden-daemon/src/recall.rs`).

use std::path::{Path, PathBuf};

use mongodb::bson::DateTime;
use ramwarden_core::decisions::Decision;
use ramwarden_core::history::{Action, History};
use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::decisions::{DecisionDoc, Neighbour, TEXT_INDEX, settled_closes, to_document};

/// Rows per round trip.
const BATCH: u32 = 200;
/// A close older than this with no reopen is counted as "not missed".
pub const SETTLE_SECS: i64 = 24 * 3600;
/// Title neighbours below this cosine score are a different kind of tab.
pub const MIN_SCORE: f64 = 0.85;
pub const NEIGHBOURS: u32 = 8;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// What has been sent, kept beside the history database.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Cursor {
    pub decision_rev: i64,
    pub action_id: i64,
}

impl Cursor {
    pub fn path(history: &Path) -> PathBuf {
        history.with_file_name("decisions-sync.json")
    }
    pub fn load(history: &Path) -> Cursor {
        std::fs::read_to_string(Self::path(history)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    pub fn save(&self, history: &Path) -> std::io::Result<()> {
        write_atomic(&Self::path(history), &serde_json::to_vec_pretty(self).unwrap_or_default())
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}

/// This machine's name in the log: `$RAMWARDEN_DEVICE`, else the hostname.
pub fn device_name() -> String {
    if let Ok(d) = std::env::var("RAMWARDEN_DEVICE")
        && !d.trim().is_empty()
    {
        return d.trim().to_string();
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "this-device".into())
}

fn millis(secs: i64) -> DateTime {
    DateTime::from_millis(secs.saturating_mul(1000))
}

/// One local decision as a document. The URL is dropped here, always.
pub fn decision_doc(device: &str, d: &Decision, titles: bool) -> DecisionDoc {
    DecisionDoc {
        id: format!("{device}:{}", d.id),
        device: device.into(),
        local_id: d.id,
        at: millis(d.at),
        kind: d.kind.clone(),
        action: d.action.clone(),
        result: d.result.clone(),
        subject: d.subject.clone(),
        title: (titles && !d.title.is_empty()).then(|| d.title.clone()),
        browser: d.browser.clone(),
        tab_id: d.tab_id,
        trigger: d.trigger.clone(),
        reason: d.reason.clone(),
        inputs: to_document(&d.inputs),
        jev: d.jev.as_ref().map(to_document),
        vector: d.vector.clone(),
        text_vector: None,
        embed_model: None,
        bytes: d.bytes,
        outcome: d.outcome.clone(),
        outcome_at: d.outcome_at.map(millis),
        synced_at: DateTime::now(),
    }
}

/// One ladder rung as a document, so the collection covers everything.
pub fn action_doc(device: &str, a: &Action) -> DecisionDoc {
    let at = parse_iso(&a.at).unwrap_or_else(DateTime::now);
    DecisionDoc {
        id: format!("{device}:a{}", a.id),
        device: device.into(),
        local_id: a.id,
        at,
        kind: "ladder".into(),
        action: a.action.clone(),
        result: if a.succeeded { "confirmed".into() } else { "refused".into() },
        subject: a.target.clone(),
        trigger: a.trigger.clone(),
        reason: a.notes.clone(),
        inputs: mongodb::bson::doc! { "rung": a.rung, "psi_some": a.psi_some, "psi_full": a.psi_full },
        bytes: a.bytes_freed,
        synced_at: DateTime::now(),
        ..Default::default()
    }
}

/// The local `actions` timestamps: naive UTC, `2026-10-01T12:34:56.123456`.
fn parse_iso(s: &str) -> Option<DateTime> {
    let with_zone = format!("{}Z", s.get(..23)?);
    DateTime::parse_rfc3339_str(with_zone).ok()
}

// ── Embeddings ──────────────────────────────────────────────────────────────

/// Ollama's `/api/embed`, the same server and model the daemon uses.
pub struct Embedder {
    http: reqwest::Client,
    url: String,
    pub model: String,
}

impl Embedder {
    pub fn new(host: &str, model: &str) -> Self {
        Embedder {
            http: reqwest::Client::builder().timeout(std::time::Duration::from_secs(60)).build().unwrap_or_default(),
            url: format!("{}/api/embed", host.trim_end_matches('/')),
            model: model.into(),
        }
    }

    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, BoxError> {
        #[derive(Deserialize)]
        struct Reply {
            embeddings: Vec<Vec<f32>>,
        }
        let reply: Reply = self
            .http
            .post(&self.url)
            .json(&serde_json::json!({ "model": self.model, "input": texts }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if reply.embeddings.len() != texts.len() {
            return Err(format!("asked for {} embeddings, got {}", texts.len(), reply.embeddings.len()).into());
        }
        Ok(reply.embeddings)
    }

    /// Fill `text_vector` on tab and app documents. On failure the documents
    /// are left without one and the error is returned for the caller to report.
    pub async fn attach(&self, docs: &mut [DecisionDoc]) -> Result<usize, BoxError> {
        let idx: Vec<usize> = (0..docs.len()).filter(|&i| docs[i].kind != "ladder").collect();
        if idx.is_empty() {
            return Ok(0);
        }
        let texts: Vec<String> = idx.iter().map(|&i| docs[i].embed_text()).collect();
        let vectors = self.embed(&texts).await?;
        for (i, v) in idx.into_iter().zip(vectors) {
            docs[i].text_vector = Some(v);
            docs[i].embed_model = Some(self.model.clone());
        }
        Ok(texts.len())
    }
}

// ── Sync ────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct SyncReport {
    pub decisions: usize,
    pub actions: usize,
    pub embedded: usize,
    pub embed_error: Option<String>,
}

pub struct Syncer<'a> {
    pub db: &'a Db,
    pub history_path: PathBuf,
    pub device: String,
    pub titles: bool,
    pub embedder: Option<Embedder>,
}

impl Syncer<'_> {
    /// Send everything not sent yet. Safe to run as often as wanted.
    pub async fn sync(&self) -> Result<SyncReport, BoxError> {
        let history = History::open(&self.history_path)?;
        let mut cursor = Cursor::load(&self.history_path);
        let mut report = SyncReport::default();
        loop {
            let rows = history.decisions_since(cursor.decision_rev, BATCH)?;
            let Some(last) = rows.last().map(|d| d.rev) else { break };
            let mut docs: Vec<DecisionDoc> = rows.iter().map(|d| decision_doc(&self.device, d, self.titles)).collect();
            if let Some(e) = &self.embedder {
                match e.attach(&mut docs).await {
                    Ok(n) => report.embedded += n,
                    Err(err) => report.embed_error = Some(err.to_string()),
                }
            }
            report.decisions += self.db.upsert_decisions(&docs).await?;
            cursor.decision_rev = last;
            cursor.save(&self.history_path)?;
        }
        loop {
            let rows = history.actions_after(cursor.action_id, BATCH)?;
            let Some(last) = rows.last().map(|a| a.id) else { break };
            let docs: Vec<DecisionDoc> = rows.iter().map(|a| action_doc(&self.device, a)).collect();
            report.actions += self.db.upsert_decisions(&docs).await?;
            cursor.action_id = last;
            cursor.save(&self.history_path)?;
        }
        Ok(report)
    }
}

// ── Recall ──────────────────────────────────────────────────────────────────

/// One open tab and what its neighbours say. Mirrors the daemon's
/// `recall::Hint`; the daemon decides what to do with it.
#[derive(Debug, Serialize, PartialEq)]
pub struct Hint {
    pub browser: String,
    pub tab_id: i64,
    pub url: String,
    pub similar: u32,
    pub reopened: u32,
    pub examples: Vec<String>,
}

/// Count reopens among neighbours that are similar enough to count.
pub fn summarise(neighbours: &[Neighbour]) -> (u32, u32, Vec<String>) {
    let close: Vec<&Neighbour> = neighbours.iter().filter(|n| n.score >= MIN_SCORE).collect();
    let reopened: Vec<String> = close
        .iter()
        .filter(|n| n.doc.outcome.as_deref() == Some("reopened"))
        .map(|n| n.doc.id.clone())
        .collect();
    (close.len() as u32, reopened.len() as u32, reopened)
}

/// A tab as `GET /browser/tabs` reports it: only what recall needs.
#[derive(Debug, Deserialize)]
pub struct OpenTab {
    pub browser: String,
    pub id: i64,
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub closeable: bool,
    /// What the sweep may close unattended (`Row::auto_closeable`). Older
    /// daemons report only `closeable`.
    #[serde(default)]
    pub auto_closeable: Option<bool>,
}

impl OpenTab {
    fn host(&self) -> String {
        let rest = self.url.split_once("://").map(|(_, r)| r).unwrap_or(&self.url);
        let host = rest.split(['/', '?', '#']).next().unwrap_or("").to_ascii_lowercase();
        let host = host.rsplit_once('@').map(|(_, h)| h.to_string()).unwrap_or(host);
        let host = host.split(':').next().unwrap_or("").to_string();
        host.strip_prefix("www.").map(str::to_string).unwrap_or(host)
    }

    /// Only tabs the sweep could close are worth a query.
    fn closeable_now(&self) -> bool {
        self.auto_closeable.unwrap_or(self.closeable) && matches!(self.status.as_str(), "candidate" | "unloaded")
    }
}

/// Query Atlas for every closeable open tab and write `recall.json`.
pub async fn write_recall(db: &Db, embedder: &Embedder, daemon: &str, history_path: &Path) -> Result<usize, BoxError> {
    #[derive(Deserialize)]
    struct Snapshot {
        #[serde(default)]
        tabs: Vec<OpenTab>,
    }
    let snap: Snapshot = reqwest::get(format!("{}/browser/tabs", daemon.trim_end_matches('/'))).await?.error_for_status()?.json().await?;
    let tabs: Vec<&OpenTab> = snap.tabs.iter().filter(|t| t.closeable_now()).collect();
    let mut hints = Vec::new();
    if !tabs.is_empty() {
        let texts: Vec<String> = tabs
            .iter()
            .map(|t| DecisionDoc { subject: t.host(), title: Some(t.title.clone()), ..Default::default() }.embed_text())
            .collect();
        let vectors = embedder.embed(&texts).await?;
        let settled = DateTime::from_millis(DateTime::now().timestamp_millis() - SETTLE_SECS * 1000);
        for (t, v) in tabs.iter().zip(vectors) {
            let near = db.similar_decisions(TEXT_INDEX, &v, NEIGHBOURS, settled_closes(settled)).await?;
            let (similar, reopened, examples) = summarise(&near);
            if similar > 0 {
                hints.push(Hint { browser: t.browser.clone(), tab_id: t.id, url: t.url.clone(), similar, reopened, examples });
            }
        }
    }
    let body = serde_json::json!({
        "generated_at": ramwarden_core::history::unix_now(),
        "model": embedder.model,
        "tabs": hints,
    });
    write_atomic(&history_path.with_file_name("recall.json"), &serde_json::to_vec_pretty(&body)?)?;
    Ok(hints.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramwarden_core::decisions::NewDecision;

    #[test]
    fn the_url_never_reaches_the_document_and_titles_are_optional() {
        let h = History::in_memory().unwrap();
        let id = h
            .record_decision(
                &NewDecision {
                    kind: "tab".into(),
                    action: "close".into(),
                    result: "confirmed".into(),
                    subject: "docs.google.com".into(),
                    title: "Q3 plan".into(),
                    url: "https://docs.google.com/document/d/SECRET-ID/edit".into(),
                    inputs: serde_json::json!({"idle_minutes": 1800}),
                    jev: Some(serde_json::json!({"return_24h": 0.1})),
                    vector: vec![0.5; 16],
                    ..Default::default()
                },
                1_700_000_000,
            )
            .unwrap();
        let d = h.decision(id).unwrap().unwrap();
        let doc = decision_doc("mac", &d, true);
        assert_eq!(doc.id, format!("mac:{id}"));
        assert_eq!(doc.title.as_deref(), Some("Q3 plan"));
        assert_eq!(doc.inputs.get_i64("idle_minutes").unwrap(), 1800);
        assert_eq!(doc.at.timestamp_millis(), 1_700_000_000_000);
        let bson = mongodb::bson::to_document(&doc).unwrap().to_string();
        assert!(!bson.contains("SECRET-ID"), "{bson}");
        assert_eq!(decision_doc("mac", &d, false).title, None);
    }

    #[test]
    fn ladder_actions_parse_the_local_timestamp() {
        let a = Action {
            id: 3,
            at: "2026-10-01T12:34:56.123456".into(),
            rung: 1,
            trigger: "psi".into(),
            psi_some: 12.0,
            psi_full: 1.0,
            action: "pageout".into(),
            target: "app-firefox.scope".into(),
            bytes_freed: 1 << 20,
            succeeded: true,
            notes: String::new(),
        };
        let d = action_doc("mac", &a);
        assert_eq!(d.id, "mac:a3");
        assert_eq!(d.kind, "ladder");
        assert_eq!(d.at.try_to_rfc3339_string().unwrap(), "2026-10-01T12:34:56.123Z");
    }

    #[test]
    fn only_close_enough_neighbours_count_and_reopens_are_listed() {
        let n = |id: &str, score: f64, outcome: Option<&str>| Neighbour {
            doc: DecisionDoc { id: id.into(), outcome: outcome.map(str::to_string), ..Default::default() },
            score,
        };
        let (similar, reopened, examples) =
            summarise(&[n("a", 0.95, Some("reopened")), n("b", 0.9, None), n("c", 0.5, Some("reopened"))]);
        assert_eq!((similar, reopened), (2, 1));
        assert_eq!(examples, vec!["a".to_string()]);
    }

    #[test]
    fn hosts_are_read_without_credentials_ports_or_www() {
        let t = |url: &str| OpenTab { browser: "b".into(), id: 1, url: url.into(), title: String::new(), status: String::new(), closeable: true, auto_closeable: None };
        assert_eq!(t("https://www.Example.com/a?b").host(), "example.com");
        assert_eq!(t("https://user:pw@docs.example.com:8443/x").host(), "docs.example.com");
    }

    #[test]
    fn recall_asks_only_about_tabs_the_sweep_could_close_unattended() {
        let tab = |v: serde_json::Value| serde_json::from_value::<OpenTab>(v).unwrap();
        let base = serde_json::json!({"browser":"b","id":1,"url":"https://x.test/","status":"candidate","closeable":true});
        assert!(tab(base.clone()).closeable_now(), "an older daemon reports only closeable");
        let mut manual_only = base.clone();
        manual_only["auto_closeable"] = false.into();
        assert!(!tab(manual_only).closeable_now());
        let mut recent = base;
        recent["status"] = "recent".into();
        assert!(!tab(recent).closeable_now());
    }

    #[test]
    fn the_cursor_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("history.db");
        assert_eq!(Cursor::load(&db), Cursor::default());
        Cursor { decision_rev: 9, action_id: 4 }.save(&db).unwrap();
        assert_eq!(Cursor::load(&db), Cursor { decision_rev: 9, action_id: 4 });
    }
}
