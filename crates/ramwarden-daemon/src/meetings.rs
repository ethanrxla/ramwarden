//! Local meeting records. Transcript text is data, never executor authority.
//! Capture and hosted synchronization are separate adapters, not implied by a record.
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{io::Write, path::Path};

const MAX_TRANSCRIPT: usize = 4 * 1024 * 1024;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid meeting input")]
    Invalid,
    #[error("Meeting not found")]
    NotFound,
    #[error("Meeting state changed or summary attempt expired")]
    Conflict,
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Recording,
    Stopped,
    Processing,
    Ready,
    Failed,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    pub sequence: u32,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    pub overview: String,
    pub decisions: Vec<String>,
    pub action_items: Vec<String>,
    pub open_questions: Vec<String>,
}
impl Summary {
    fn valid(&self) -> bool {
        !self.overview.trim().is_empty()
            && self.overview.len() <= 32000
            && [&self.decisions, &self.action_items, &self.open_questions]
                .iter()
                .all(|items| {
                    items.len() <= 64
                        && items
                            .iter()
                            .all(|s| !s.trim().is_empty() && s.len() <= 2000)
                })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meeting {
    pub id: String,
    pub title: String,
    pub phase: Phase,
    pub created_at: u64,
    pub stopped_at: Option<u64>,
    pub segments: Vec<Segment>,
    pub summary: Option<Summary>,
    #[serde(default)]
    pub insights: Vec<Insight>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Insight {
    pub id: String,
    pub text: String,
    pub first_segment: u32,
    pub last_segment: u32,
}
/// Listing history never returns every meeting's complete transcript.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MeetingHeader {
    pub id: String,
    pub title: String,
    pub phase: Phase,
    pub created_at: u64,
    pub stopped_at: Option<u64>,
}
pub struct Store {
    connection: Connection,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(3))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS meetings_v1 (
                id TEXT PRIMARY KEY, owner TEXT NOT NULL, record TEXT NOT NULL,
                attempt TEXT, lease_until INTEGER NOT NULL DEFAULT 0);
            CREATE INDEX IF NOT EXISTS meetings_owner_v1 ON meetings_v1(owner);",
        )?;
        Ok(Self { connection })
    }
    pub fn create(&self, owner: &str, title: &str, now: u64) -> Result<Meeting> {
        if owner.trim().is_empty()
            || owner.len() > 240
            || title.trim().is_empty()
            || title.len() > 200
        {
            return Err(Error::Invalid);
        }
        let meeting = Meeting {
            id: uuid::Uuid::new_v4().to_string(),
            title: title.trim().into(),
            phase: Phase::Recording,
            created_at: now,
            stopped_at: None,
            segments: vec![],
            summary: None,
            insights: vec![],
        };
        self.connection.execute(
            "INSERT INTO meetings_v1(id,owner,record) VALUES (?1,?2,?3)",
            params![meeting.id, owner, serde_json::to_string(&meeting)?],
        )?;
        Ok(meeting)
    }
    pub fn get(&self, owner: &str, id: &str) -> Result<Meeting> {
        let text: String = self
            .connection
            .query_row(
                "SELECT record FROM meetings_v1 WHERE owner=?1 AND id=?2",
                params![owner, id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        Ok(serde_json::from_str(&text)?)
    }
    pub fn list(&self, owner: &str) -> Result<Vec<MeetingHeader>> {
        let mut statement = self.connection.prepare(
            "SELECT record FROM meetings_v1 WHERE owner=?1 ORDER BY rowid DESC LIMIT 100",
        )?;
        let records = statement.query_map([owner], |r| r.get::<_, String>(0))?;
        records.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }
    pub fn delete(&self, owner: &str, id: &str) -> Result<()> {
        if self.connection.execute(
            "DELETE FROM meetings_v1 WHERE owner=?1 AND id=?2",
            params![owner, id],
        )? == 0
        {
            return Err(Error::NotFound);
        }
        Ok(())
    }
    pub fn add_insight(&mut self, owner: &str, id: &str, insight: Insight) -> Result<Meeting> {
        if uuid::Uuid::parse_str(&insight.id).is_err()
            || insight.text.trim().is_empty()
            || insight.text.len() > 2000
            || insight.first_segment > insight.last_segment
        {
            return Err(Error::Invalid);
        }
        self.change(owner, id, |meeting| {
            if let Some(old) = meeting.insights.iter().find(|i| i.id == insight.id) {
                return if old.text == insight.text
                    && old.first_segment == insight.first_segment
                    && old.last_segment == insight.last_segment
                {
                    Ok(())
                } else {
                    Err(Error::Conflict)
                };
            }
            if meeting.insights.len() >= 200
                || insight.last_segment as usize >= meeting.segments.len()
            {
                return Err(Error::Invalid);
            }
            meeting.insights.push(insight);
            Ok(())
        })
    }
    fn change(
        &mut self,
        owner: &str,
        id: &str,
        edit: impl FnOnce(&mut Meeting) -> Result<()>,
    ) -> Result<Meeting> {
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let text: String = tx
            .query_row(
                "SELECT record FROM meetings_v1 WHERE owner=?1 AND id=?2",
                params![owner, id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        let mut meeting: Meeting = serde_json::from_str(&text)?;
        edit(&mut meeting)?;
        tx.execute(
            "UPDATE meetings_v1 SET record=?3 WHERE owner=?1 AND id=?2",
            params![owner, id, serde_json::to_string(&meeting)?],
        )?;
        tx.commit()?;
        Ok(meeting)
    }
    pub fn append(&mut self, owner: &str, id: &str, segment: Segment) -> Result<Meeting> {
        if segment.text.trim().is_empty()
            || segment.text.len() > 16000
            || segment.end_ms < segment.start_ms
            || segment.end_ms > 24 * 60 * 60 * 1000
        {
            return Err(Error::Invalid);
        }
        self.change(owner, id, |meeting| {
            if let Some(existing) = meeting.segments.get(segment.sequence as usize) {
                return if existing == &segment {
                    Ok(())
                } else {
                    Err(Error::Conflict)
                };
            }
            if meeting.phase != Phase::Recording
                || segment.sequence as usize != meeting.segments.len()
            {
                return Err(Error::Conflict);
            }
            if meeting
                .segments
                .last()
                .is_some_and(|s| segment.start_ms < s.start_ms)
                || meeting.segments.len() >= 30000
                || meeting.segments.iter().map(|s| s.text.len()).sum::<usize>() + segment.text.len()
                    > MAX_TRANSCRIPT
            {
                return Err(Error::Invalid);
            }
            meeting.segments.push(segment);
            Ok(())
        })
    }
    pub fn stop(&mut self, owner: &str, id: &str, now: u64) -> Result<Meeting> {
        self.change(owner, id, |meeting| {
            if meeting.phase == Phase::Recording {
                if now < meeting.created_at {
                    return Err(Error::Invalid);
                }
                meeting.phase = Phase::Stopped;
                meeting.stopped_at = Some(now);
            }
            Ok(())
        })
    }
    /// A durable lease makes retries/restarts safe; only the winning attempt may finish.
    pub fn claim_summary(&mut self, owner: &str, id: &str, now: u64) -> Result<(String, Meeting)> {
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (text, lease): (String, u64) = tx
            .query_row(
                "SELECT record,lease_until FROM meetings_v1 WHERE owner=?1 AND id=?2",
                params![owner, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        let mut meeting: Meeting = serde_json::from_str(&text)?;
        if meeting.segments.is_empty()
            || meeting.phase == Phase::Recording
            || meeting.phase == Phase::Ready
            || (meeting.phase == Phase::Processing && lease > now)
        {
            return Err(Error::Conflict);
        }
        let token = uuid::Uuid::new_v4().to_string();
        meeting.phase = Phase::Processing;
        tx.execute(
            "UPDATE meetings_v1 SET record=?3,attempt=?4,lease_until=?5 WHERE owner=?1 AND id=?2",
            params![
                owner,
                id,
                serde_json::to_string(&meeting)?,
                token,
                now.saturating_add(300)
            ],
        )?;
        tx.commit()?;
        Ok((token, meeting))
    }
    pub fn finish_summary(
        &mut self,
        owner: &str,
        id: &str,
        token: &str,
        summary: Option<Summary>,
        now: u64,
    ) -> Result<Meeting> {
        if summary.as_ref().is_some_and(|s| !s.valid()) {
            return Err(Error::Invalid);
        }
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let text: String = tx.query_row("SELECT record FROM meetings_v1 WHERE owner=?1 AND id=?2 AND attempt=?3 AND lease_until>?4", params![owner,id,token,now], |r|r.get(0)).optional()?.ok_or(Error::Conflict)?;
        let mut meeting: Meeting = serde_json::from_str(&text)?;
        if meeting.phase != Phase::Processing {
            return Err(Error::Conflict);
        }
        meeting.phase = if summary.is_some() {
            Phase::Ready
        } else {
            Phase::Failed
        };
        meeting.summary = summary;
        tx.execute(
            "UPDATE meetings_v1 SET record=?3,attempt=NULL,lease_until=0 WHERE owner=?1 AND id=?2",
            params![owner, id, serde_json::to_string(&meeting)?],
        )?;
        tx.commit()?;
        Ok(meeting)
    }
    /// Only the caller-selected directory is used; model text cannot select filenames.
    /// Never replace an existing file (including a symlink).
    pub fn export_markdown(
        &self,
        owner: &str,
        id: &str,
        directory: &Path,
    ) -> Result<std::path::PathBuf> {
        let meeting = self.get(owner, id)?;
        let summary = meeting
            .summary
            .as_ref()
            .filter(|_| meeting.phase == Phase::Ready)
            .ok_or(Error::Conflict)?;
        let safe_id = uuid::Uuid::parse_str(&meeting.id).map_err(|_| Error::Invalid)?;
        let path = directory.join(format!("meeting-{safe_id}.md"));
        let mut text = format!(
            "# {}\n\nMeeting: {}\n\n## Summary\n\n{}\n",
            meeting.title.replace(['\r', '\n'], " "),
            safe_id,
            summary.overview
        );
        for (heading, items) in [
            ("Decisions", &summary.decisions),
            ("Action items", &summary.action_items),
            ("Open questions", &summary.open_questions),
        ] {
            text.push_str(&format!("\n## {heading}\n\n"));
            if items.is_empty() {
                text.push_str("None identified.\n");
            }
            for item in items {
                text.push_str(&format!("- {}\n", item.replace(['\r', '\n'], " ")));
            }
        }
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.write_all(text.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist_noclobber(&path)
            .map_err(|e| Error::Io(e.error))?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn segment(sequence: u32) -> Segment {
        Segment {
            sequence,
            start_ms: u64::from(sequence) * 1000,
            end_ms: u64::from(sequence) * 1000 + 999,
            text: "Decision: test the beta.".into(),
        }
    }
    fn summary() -> Summary {
        Summary {
            overview: "Discussed the beta.".into(),
            decisions: vec!["Test the beta.".into()],
            action_items: vec![],
            open_questions: vec![],
        }
    }
    #[test]
    fn restart_isolation_and_idempotent_segments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meetings.db");
        let mut store = Store::open(&path).unwrap();
        let m = store.create("alice", "Beta", 100).unwrap();
        store.append("alice", &m.id, segment(0)).unwrap();
        store.append("alice", &m.id, segment(0)).unwrap();
        assert!(store.append("alice", &m.id, segment(2)).is_err());
        let mut changed = segment(0);
        changed.text = "different".into();
        assert!(store.append("alice", &m.id, changed).is_err());
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.get("alice", &m.id).unwrap().segments.len(), 1);
        assert!(matches!(store.get("bob", &m.id), Err(Error::NotFound)));
        assert!(store.list("bob").unwrap().is_empty());
    }
    #[test]
    fn summary_lease_recovers_and_stale_results_cannot_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("db")).unwrap();
        let m = store.create("alice", "Beta", 100).unwrap();
        store.append("alice", &m.id, segment(0)).unwrap();
        assert!(store.claim_summary("alice", &m.id, 101).is_err());
        store.stop("alice", &m.id, 102).unwrap();
        assert!(store.append("alice", &m.id, segment(1)).is_err());
        let (old, _) = store.claim_summary("alice", &m.id, 103).unwrap();
        assert!(store.claim_summary("alice", &m.id, 104).is_err());
        let (new, _) = store.claim_summary("alice", &m.id, 404).unwrap();
        assert!(
            store
                .finish_summary("alice", &m.id, &old, Some(summary()), 405)
                .is_err()
        );
        store
            .finish_summary("alice", &m.id, &new, Some(summary()), 405)
            .unwrap();
        assert!(
            store
                .finish_summary("alice", &m.id, &new, Some(summary()), 406)
                .is_err()
        );
        assert!(store.claim_summary("alice", &m.id, 407).is_err());
    }
    #[test]
    fn markdown_is_generated_safely_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("db")).unwrap();
        let m = store.create("alice", "../../outside\nBeta", 1).unwrap();
        store.append("alice", &m.id, segment(0)).unwrap();
        assert!(store.export_markdown("alice", &m.id, dir.path()).is_err());
        store.stop("alice", &m.id, 2).unwrap();
        let (token, _) = store.claim_summary("alice", &m.id, 3).unwrap();
        store
            .finish_summary("alice", &m.id, &token, Some(summary()), 4)
            .unwrap();
        let path = store.export_markdown("alice", &m.id, dir.path()).unwrap();
        assert_eq!(path.parent(), Some(dir.path()));
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("## Decisions")
        );
        std::fs::write(&path, "user edited").unwrap();
        assert!(store.export_markdown("alice", &m.id, dir.path()).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "user edited");
    }
    #[test]
    fn empty_and_failed_jobs_do_not_claim_completion() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("db")).unwrap();
        let m = store.create("alice", "Beta", 1).unwrap();
        store.stop("alice", &m.id, 2).unwrap();
        assert!(store.claim_summary("alice", &m.id, 3).is_err());
        let m = store.create("alice", "Beta", 4).unwrap();
        store.append("alice", &m.id, segment(0)).unwrap();
        store.stop("alice", &m.id, 5).unwrap();
        let (token, _) = store.claim_summary("alice", &m.id, 6).unwrap();
        store
            .finish_summary("alice", &m.id, &token, None, 7)
            .unwrap();
        assert_eq!(store.get("alice", &m.id).unwrap().phase, Phase::Failed);
        assert!(store.export_markdown("alice", &m.id, dir.path()).is_err());
        assert!(store.claim_summary("alice", &m.id, 8).is_ok());
    }
    #[test]
    fn insights_retain_source_segments_and_cannot_cross_owners() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("db")).unwrap();
        let m = store.create("alice", "Beta", 1).unwrap();
        store.append("alice", &m.id, segment(0)).unwrap();
        let insight = Insight {
            id: uuid::Uuid::new_v4().to_string(),
            text: "A beta decision was discussed.".into(),
            first_segment: 0,
            last_segment: 0,
        };
        assert!(store.add_insight("bob", &m.id, insight.clone()).is_err());
        store.add_insight("alice", &m.id, insight.clone()).unwrap();
        store.add_insight("alice", &m.id, insight).unwrap();
        assert_eq!(store.get("alice", &m.id).unwrap().insights.len(), 1);
        let invalid = Insight {
            id: uuid::Uuid::new_v4().to_string(),
            text: "Missing source".into(),
            first_segment: 1,
            last_segment: 2,
        };
        assert!(store.add_insight("alice", &m.id, invalid).is_err());
        assert!(
            !serde_json::to_string(&store.list("alice").unwrap())
                .unwrap()
                .contains("segments")
        );
        assert!(store.delete("bob", &m.id).is_err());
        store.delete("alice", &m.id).unwrap();
        assert!(store.get("alice", &m.id).is_err());
    }
}
