//! Snapshots of what each tweak changed, so every change can be undone.
//!
//! Written *before* a tweak is applied: if the process dies mid-apply, the
//! journal still holds the way back. One row per applied tweak; reverting
//! deletes it.

use std::path::Path;

use ramwarden_platform::{Error, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

pub struct Journal {
    db: Connection,
}

/// One recorded change.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub id: String,
    pub snapshot: Value,
    pub applied_at: i64,
}

fn db_err(e: rusqlite::Error) -> Error {
    Error::os(format!("debloat journal: {e}"))
}

impl Journal {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::os(format!("{}: {e}", dir.display())))?;
        }
        Self::init(Connection::open(path).map_err(db_err)?)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory().map_err(db_err)?)
    }

    fn init(db: Connection) -> Result<Self> {
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS snapshots (
                 id TEXT PRIMARY KEY,
                 snapshot TEXT NOT NULL,
                 applied_at INTEGER NOT NULL
             );",
        )
        .map_err(db_err)?;
        Ok(Journal { db })
    }

    /// Record the state before a tweak's first application. A second apply of
    /// the same tweak keeps the *original* snapshot, never an intermediate.
    pub fn record(&self, id: &str, snapshot: &Value) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.db
            .execute(
                "INSERT OR IGNORE INTO snapshots (id, snapshot, applied_at) VALUES (?1, ?2, ?3)",
                params![id, snapshot.to_string(), now],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<Record>> {
        self.db
            .query_row(
                "SELECT id, snapshot, applied_at FROM snapshots WHERE id = ?1",
                params![id],
                |r| {
                    let text: String = r.get(1)?;
                    Ok(Record {
                        id: r.get(0)?,
                        snapshot: serde_json::from_str(&text).unwrap_or(Value::Null),
                        applied_at: r.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(db_err)
    }

    pub fn forget(&self, id: &str) -> Result<()> {
        self.db
            .execute("DELETE FROM snapshots WHERE id = ?1", params![id])
            .map_err(db_err)?;
        Ok(())
    }

    pub fn ids(&self) -> Result<Vec<String>> {
        let mut stmt = self.db.prepare("SELECT id FROM snapshots ORDER BY id").map_err(db_err)?;
        let rows = stmt.query_map([], |r| r.get(0)).map_err(db_err)?;
        rows.collect::<std::result::Result<_, _>>().map_err(db_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_first_snapshot_wins_and_forget_removes_it() {
        let j = Journal::in_memory().unwrap();
        j.record("a", &json!({"v": 1})).unwrap();
        j.record("a", &json!({"v": 2})).unwrap();
        assert_eq!(j.get("a").unwrap().unwrap().snapshot, json!({"v": 1}));
        assert_eq!(j.ids().unwrap(), vec!["a"]);
        j.forget("a").unwrap();
        assert!(j.get("a").unwrap().is_none());
    }

    #[test]
    fn a_journal_on_disk_survives_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("debloat.db");
        Journal::open(&p).unwrap().record("x", &json!({"exists": false})).unwrap();
        assert!(Journal::open(&p).unwrap().get("x").unwrap().is_some());
    }
}
