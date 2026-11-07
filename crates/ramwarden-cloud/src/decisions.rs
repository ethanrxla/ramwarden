//! The decision log in MongoDB: every close, unload, quit and keep RamWarden
//! made, with its inputs, Jev's answer and what became of it, searchable by
//! similarity with Atlas Vector Search.
//!
//! # Shape
//!
//! One document per local decision row (`ramwarden_core::decisions`), keyed
//! `"<device>:<id>"` so a re-sync replaces rather than duplicates, and a late
//! outcome ("the user reopened it") lands on the document it belongs to. Ladder
//! rungs from the local `actions` table come along as `kind: "ladder"` so one
//! collection answers "what did RamWarden do on Tuesday afternoon?".
//!
//! # Two vectors
//!
//! - `vector`: the decision's numbers (`TabVector`, 16 dimensions), computed by
//!   the daemon when it decided. Nearest neighbours here are decisions made
//!   *for the same reasons*: same idle time, return chance, site type.
//! - `text_vector`: an embedding of "title · host" (mxbai-embed-large, 1024
//!   dimensions, from the local Ollama). Nearest neighbours here are *the same
//!   kind of tab*: another doc in the same project, another ticket in the same
//!   tracker. This is what reopen recall searches.
//!
//! Each has its own `vectorSearch` index with `kind`, `action`, `result`,
//! `outcome`, `device` and `at` as filter fields, so a search can ask for
//! "settled live closes only".
//!
//! # What is not here
//!
//! Full URLs (paths and query strings carry document ids and tokens) never
//! leave the machine. Titles are sent unless the sync runs with `--no-titles`.
//! This collection belongs to the developer's own cluster (`ramwarden-dev`);
//! the hosted API never receives it (see `docs/specs/decision-log.md`).

use futures::TryStreamExt;
use mongodb::bson::{self, DateTime, Document, doc};
use mongodb::options::IndexOptions;
use mongodb::{Collection, IndexModel, SearchIndexModel, SearchIndexType};
use serde::{Deserialize, Serialize};

use crate::db::{Db, Result};

pub const DECISIONS: &str = "decisions";
pub const FEATURE_INDEX: &str = "decision_features";
pub const TEXT_INDEX: &str = "decision_text";
/// `TabVector::DIMS` in ramwarden-core. The index is built for it.
pub const FEATURE_DIMS: i32 = 16;
/// mxbai-embed-large.
pub const DEFAULT_TEXT_DIMS: i32 = 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionDoc {
    /// `"<device>:<local id>"`, or `"<device>:a<id>"` for a ladder action.
    #[serde(rename = "_id")]
    pub id: String,
    pub device: String,
    pub local_id: i64,
    pub at: DateTime,
    /// `tab`, `app` or `ladder`.
    pub kind: String,
    pub action: String,
    pub result: String,
    /// Host for a tab, app name for an app, scope or process for a rung.
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub browser: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<i64>,
    pub trigger: String,
    pub reason: String,
    #[serde(default)]
    pub inputs: Document,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jev: Option<Document>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vector: Vec<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_vector: Option<Vec<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_model: Option<String>,
    pub bytes: i64,
    /// `returned`, `reopened`, or absent while unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_at: Option<DateTime>,
    pub synced_at: DateTime,
}

impl Default for DecisionDoc {
    fn default() -> Self {
        DecisionDoc {
            id: String::new(),
            device: String::new(),
            local_id: 0,
            at: DateTime::from_millis(0),
            kind: String::new(),
            action: String::new(),
            result: String::new(),
            subject: String::new(),
            title: None,
            browser: String::new(),
            tab_id: None,
            trigger: String::new(),
            reason: String::new(),
            inputs: Document::new(),
            jev: None,
            vector: Vec::new(),
            text_vector: None,
            embed_model: None,
            bytes: 0,
            outcome: None,
            outcome_at: None,
            synced_at: DateTime::from_millis(0),
        }
    }
}

impl DecisionDoc {
    /// The text the title embedding is made from. Without a title (an app, or
    /// a `--no-titles` sync) the host alone still groups tabs by site.
    pub fn embed_text(&self) -> String {
        match &self.title {
            Some(t) if !t.trim().is_empty() => format!("{} · {}", t.trim(), self.subject),
            _ => self.subject.clone(),
        }
    }
}

/// A past decision near a query, with Atlas's similarity score (0-1, cosine).
#[derive(Clone, Debug, Deserialize)]
pub struct Neighbour {
    #[serde(flatten)]
    pub doc: DecisionDoc,
    pub score: f64,
}

/// JSON (as the local log stores it) to BSON, for `inputs` and `jev`.
pub fn to_document(v: &serde_json::Value) -> Document {
    match bson::to_bson(v) {
        Ok(bson::Bson::Document(d)) => d,
        _ => Document::new(),
    }
}

fn vector_index(name: &str, path: &str, dims: i32) -> SearchIndexModel {
    let filter = |p: &str| doc! { "type": "filter", "path": p };
    SearchIndexModel::builder()
        .name(name.to_string())
        .index_type(SearchIndexType::VectorSearch)
        .definition(doc! {
            "fields": [
                { "type": "vector", "path": path, "numDimensions": dims, "similarity": "cosine" },
                filter("kind"), filter("action"), filter("result"),
                filter("outcome"), filter("device"), filter("at"),
            ]
        })
        .build()
}

impl Db {
    pub fn decisions(&self) -> Collection<DecisionDoc> {
        self.database().collection(DECISIONS)
    }

    /// Ordinary indexes for browsing the log, and the two vector indexes.
    ///
    /// Vector indexes exist only on Atlas (or a local Atlas deployment). On a
    /// plain `mongod` their creation fails; that is logged and the log still
    /// works, only `similar`, `why` and `recall` do not.
    pub async fn ensure_decision_indexes(&self, text_dims: i32) -> Result<()> {
        let index = |keys: Document, name: &str| {
            IndexModel::builder().keys(keys).options(IndexOptions::builder().name(name.to_string()).build()).build()
        };
        self.decisions()
            .create_indexes([
                index(doc! { "device": 1, "at": -1 }, "device_at"),
                index(doc! { "kind": 1, "action": 1, "at": -1 }, "kind_action_at"),
                index(doc! { "subject": 1, "at": -1 }, "subject_at"),
            ])
            .await?;

        let existing: Vec<String> = match self.decisions().list_search_indexes().await {
            Ok(cursor) => cursor
                .try_collect::<Vec<Document>>()
                .await
                .unwrap_or_default()
                .iter()
                .filter_map(|d| d.get_str("name").ok().map(str::to_string))
                .collect(),
            Err(e) => {
                tracing::warn!("no Atlas Search on this deployment, vector search unavailable: {e}");
                return Ok(());
            }
        };
        for (name, path, dims) in [(FEATURE_INDEX, "vector", FEATURE_DIMS), (TEXT_INDEX, "text_vector", text_dims)] {
            if existing.iter().any(|n| n == name) {
                continue;
            }
            match self.decisions().create_search_index(vector_index(name, path, dims)).await {
                Ok(_) => tracing::info!("created vector index {name} ({dims} dims); Atlas builds it in the background"),
                Err(e) => tracing::warn!("could not create vector index {name}: {e}"),
            }
        }
        Ok(())
    }

    /// Insert or replace each document by `_id`. Returns how many were written.
    pub async fn upsert_decisions(&self, docs: &[DecisionDoc]) -> Result<usize> {
        // Replace the batch in two round trips; one per document takes hours
        // on a large backlog. A failure between the two is repaired by the
        // resend, since the caller moves its cursor only after this returns.
        if docs.is_empty() {
            return Ok(0);
        }
        let coll = self.decisions();
        let ids: Vec<&str> = docs.iter().map(|d| d.id.as_str()).collect();
        coll.delete_many(doc! { "_id": { "$in": ids } }).await?;
        coll.insert_many(docs).await?;
        Ok(docs.len())
    }

    pub async fn decision(&self, id: &str) -> Result<Option<DecisionDoc>> {
        Ok(self.decisions().find_one(doc! { "_id": id }).await?)
    }

    /// The `k` nearest decisions to `vector` in `index`, restricted by `filter`
    /// (an Atlas Vector Search pre-filter on the indexed filter fields).
    pub async fn similar_decisions(&self, index: &str, vector: &[f32], k: u32, filter: Document) -> Result<Vec<Neighbour>> {
        let path = if index == TEXT_INDEX { "text_vector" } else { "vector" };
        let mut stage = doc! {
            "index": index,
            "path": path,
            "queryVector": vector.iter().map(|x| *x as f64).collect::<Vec<f64>>(),
            "numCandidates": (k * 20).max(100) as i64,
            "limit": k as i64,
        };
        if !filter.is_empty() {
            stage.insert("filter", filter);
        }
        let pipeline = [
            doc! { "$vectorSearch": stage },
            doc! { "$set": { "score": { "$meta": "vectorSearchScore" } } },
            doc! { "$unset": ["text_vector"] },
        ];
        let docs: Vec<Document> = self.decisions().aggregate(pipeline).await?.try_collect().await?;
        Ok(docs.into_iter().filter_map(|d| bson::from_document(d).ok()).collect())
    }

    /// Newest first, optionally only one subject (host or app).
    pub async fn recent_decisions(&self, subject: Option<&str>, limit: i64) -> Result<Vec<DecisionDoc>> {
        let filter = subject.map(|s| doc! { "subject": s }).unwrap_or_default();
        let found = self
            .decisions()
            .find(filter)
            .sort(doc! { "at": -1 })
            .limit(limit)
            .projection(doc! { "text_vector": 0 })
            .await?;
        Ok(found.try_collect().await?)
    }
}

/// The filter recall uses: live tab closes whose outcome is settled, meaning
/// reopened, or old enough (`settled_before`) that silence means "not missed".
pub fn settled_closes(settled_before: DateTime) -> Document {
    doc! {
        "kind": "tab",
        "action": "close",
        "result": { "$in": ["confirmed", "queued"] },
        "$or": [ { "outcome": "reopened" }, { "at": { "$lt": settled_before } } ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embed_text_falls_back_to_the_host() {
        let mut d = DecisionDoc { subject: "docs.google.com".into(), title: Some("  Q3 plan ".into()), ..Default::default() };
        assert_eq!(d.embed_text(), "Q3 plan · docs.google.com");
        d.title = None;
        assert_eq!(d.embed_text(), "docs.google.com");
    }

    #[test]
    fn a_vector_index_declares_every_filter_field_recall_uses() {
        let m = vector_index(TEXT_INDEX, "text_vector", 1024);
        let fields = m.definition.get_array("fields").unwrap();
        let paths: Vec<&str> = fields.iter().filter_map(|f| f.as_document()?.get_str("path").ok()).collect();
        for needed in ["text_vector", "kind", "action", "result", "outcome", "at"] {
            assert!(paths.contains(&needed), "missing {needed}");
        }
        let filter = settled_closes(DateTime::from_millis(0));
        for key in filter.keys() {
            assert!(key == "$or" || paths.contains(&key.as_str()), "{key} is not a filter field");
        }
    }

    #[test]
    fn json_inputs_become_a_document() {
        let d = to_document(&serde_json::json!({"idle_minutes": 1800, "p_model": null}));
        assert_eq!(d.get_i64("idle_minutes").unwrap(), 1800);
        assert!(to_document(&serde_json::json!(3)).is_empty());
    }
}
