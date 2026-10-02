//! Persistent record of what RamWarden did.
//!
//! # Two tables, two purposes
//!
//! `closed_tabs` is inherited from v1 verbatim, down to the column defaults,
//! because there is a real database on this machine with 424 rows in it and the
//! Rust daemon has to keep reading and writing it. The schema is recreated
//! exactly, including v1's `goal_context` migration.
//!
//! `actions` is new, and it is what makes autonomous operation defensible. When
//! nobody is watching the ladder escalate, the only way to answer "why is my
//! editor frozen?" afterwards is a record of which rung fired, what the pressure
//! was at the time, and what the action actually achieved. Every rung writes
//! here, including the ones that decided to do nothing.
//!
//! `signals` is a periodic snapshot of the detector's numeric features, kept so
//! the hand-tuned idle heuristic can eventually be replaced by something learned
//! from this machine rather than guessed at. See [`History::log_signals`] for why
//! it is sampled rather than written every tick, and
//! [`History::return_labels`] for how the labels come out of it.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};

pub type Result<T> = std::result::Result<T, rusqlite::Error>;

/// A tab RamWarden closed. Column names match v1's table exactly.
#[derive(Clone, Debug, PartialEq)]
pub struct ClosedTab {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub closed_at: String,
    pub ram_freed_mb: f64,
    /// `"auto"` or `"manual"`.
    pub trigger_type: String,
    pub goal_context: String,
}

/// One thing the ladder did, and what it achieved.
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub id: i64,
    pub at: String,
    /// Which rung fired: 0 reclaim, 1 page-out, 2 tabs, 3 suspend, 4 kill.
    /// `-1` for a manual action taken from the UI.
    pub rung: i32,
    /// What engaged it — `"psi"`, `"manual"`, `"release"`.
    pub trigger: String,
    /// PSI `some avg10` when the decision was made.
    pub psi_some: f64,
    /// PSI `full avg10`. The figure that distinguishes thrashing from busy.
    pub psi_full: f64,
    /// `"reclaim"`, `"pageout"`, `"soft_cap"`, `"suspend"`, `"resume"`,
    /// `"close_tab"`, `"kill"`, `"release"`.
    pub action: String,
    /// Scope name, process name, or tab URL.
    pub target: String,
    /// Bytes measurably returned. Measured, never requested.
    pub bytes_freed: i64,
    /// Whether it did what it set out to do.
    pub succeeded: bool,
    /// Refusal reasons and shortfalls, joined with "; ".
    pub notes: String,
}

/// One process's numeric features at one moment.
///
/// Every field is a number or a short category — nothing here is free text, and
/// nothing identifies a document or a URL. This is a record of *behaviour*, which
/// is what a predictor needs, and keeping it that way means the log is not a
/// second copy of the user's activity.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SignalSample {
    pub pid: i32,
    pub name: String,
    pub role: String,
    pub verdict: String,
    pub protection: String,
    pub pss: i64,
    pub rss: i64,
    pub cpu_recent: f64,
    pub age_minutes: f64,
    pub established: i64,
    /// How many ports it is listening on, not which.
    pub listening: i64,
    pub focused: bool,
    pub windowed: bool,
    pub audio: bool,
    pub tty: bool,
    pub descendant: bool,
    pub scope: Option<String>,
}

/// The machine-wide context a batch of samples was taken in.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SampleContext {
    pub psi_some: f64,
    pub psi_full: f64,
    pub available: i64,
}

/// One training row: the features at a moment, and whether the user came back.
#[derive(Clone, Debug, PartialEq)]
pub struct LabelledSample {
    pub epoch: i64,
    pub sample: SignalSample,
    pub context: SampleContext,
    /// True if the same process was later observed focused, or burning CPU,
    /// inside the horizon. This is the thing worth predicting: not "is it idle
    /// now" but "will the user want it back".
    pub returned: bool,
}

/// A new action to record, before it has an id or timestamp.
#[derive(Clone, Debug, Default)]
pub struct NewAction {
    pub rung: i32,
    pub trigger: String,
    pub psi_some: f64,
    pub psi_full: f64,
    pub action: String,
    pub target: String,
    pub bytes_freed: i64,
    pub succeeded: bool,
    pub notes: String,
}

/// The history database.
pub struct History {
    conn: Connection,
    path: PathBuf,
}

impl History {
    /// Open (creating if absent) and bring the schema up to date.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        let db = History {
            conn,
            path: path.to_path_buf(),
        };
        db.migrate()?;
        Ok(db)
    }

    /// An in-memory database, for tests.
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = History {
            conn,
            path: PathBuf::from(":memory:"),
        };
        db.migrate()?;
        Ok(db)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Create what is missing, touch what exists.
    ///
    /// `goal_context` is added separately because v1 shipped without it and
    /// added it by `ALTER TABLE`; a database created by an early v1 has the
    /// table but not the column.
    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS closed_tabs (
                 id           INTEGER PRIMARY KEY AUTOINCREMENT,
                 url          TEXT NOT NULL,
                 title        TEXT NOT NULL,
                 closed_at    TEXT NOT NULL,
                 ram_freed_mb REAL NOT NULL DEFAULT 0,
                 trigger_type TEXT NOT NULL DEFAULT 'auto',
                 goal_context TEXT NOT NULL DEFAULT ''
             );
             CREATE TABLE IF NOT EXISTS actions (
                 id          INTEGER PRIMARY KEY AUTOINCREMENT,
                 at          TEXT NOT NULL,
                 rung        INTEGER NOT NULL,
                 trigger     TEXT NOT NULL,
                 psi_some    REAL NOT NULL DEFAULT 0,
                 psi_full    REAL NOT NULL DEFAULT 0,
                 action      TEXT NOT NULL,
                 target      TEXT NOT NULL DEFAULT '',
                 bytes_freed INTEGER NOT NULL DEFAULT 0,
                 succeeded   INTEGER NOT NULL DEFAULT 1,
                 notes       TEXT NOT NULL DEFAULT ''
             );
             CREATE INDEX IF NOT EXISTS actions_at ON actions(at DESC);
             CREATE TABLE IF NOT EXISTS signals (
                 id          INTEGER PRIMARY KEY AUTOINCREMENT,
                 at          TEXT    NOT NULL,
                 epoch       INTEGER NOT NULL,
                 pid         INTEGER NOT NULL,
                 name        TEXT    NOT NULL,
                 role        TEXT    NOT NULL DEFAULT '',
                 verdict     TEXT    NOT NULL DEFAULT '',
                 protection  TEXT    NOT NULL DEFAULT '',
                 pss         INTEGER NOT NULL DEFAULT 0,
                 rss         INTEGER NOT NULL DEFAULT 0,
                 cpu_recent  REAL    NOT NULL DEFAULT 0,
                 age_minutes REAL    NOT NULL DEFAULT 0,
                 established INTEGER NOT NULL DEFAULT 0,
                 listening   INTEGER NOT NULL DEFAULT 0,
                 focused     INTEGER NOT NULL DEFAULT 0,
                 windowed    INTEGER NOT NULL DEFAULT 0,
                 audio       INTEGER NOT NULL DEFAULT 0,
                 tty         INTEGER NOT NULL DEFAULT 0,
                 descendant  INTEGER NOT NULL DEFAULT 0,
                 scope       TEXT,
                 psi_some    REAL    NOT NULL DEFAULT 0,
                 psi_full    REAL    NOT NULL DEFAULT 0,
                 available   INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS signals_epoch ON signals(epoch);
             CREATE INDEX IF NOT EXISTS signals_pid_epoch ON signals(pid, epoch);",
        )?;

        // v1's migration, kept because databases predating it exist. An error
        // here means the column is already present, which is the normal case.
        let _ = self.conn.execute(
            "ALTER TABLE closed_tabs ADD COLUMN goal_context TEXT NOT NULL DEFAULT ''",
            [],
        );
        Ok(())
    }

    // ── Closed tabs ─────────────────────────────────────────────────────────

    /// Record closed tabs, dividing the freed total between them.
    ///
    /// v1 attributed `ram_freed_mb / len(tabs)` to each tab, which is a guess —
    /// the browser does not say which tab held what. Kept identical so the
    /// existing 424 rows stay comparable with new ones.
    pub fn save_tabs(
        &self,
        tabs: &[(String, String)],
        ram_freed_mb: f64,
        trigger_type: &str,
        goal_context: &str,
    ) -> Result<usize> {
        if tabs.is_empty() {
            return Ok(0);
        }
        let now = now_iso();
        let per_tab = (ram_freed_mb / tabs.len() as f64 * 10.0).round() / 10.0;

        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO closed_tabs
                   (url, title, closed_at, ram_freed_mb, trigger_type, goal_context)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for (url, title) in tabs {
                // v1 fell back to the URL when a tab had no title.
                let title = if title.is_empty() { url } else { title };
                stmt.execute(params![url, title, now, per_tab, trigger_type, goal_context])?;
            }
        }
        tx.commit()?;
        Ok(tabs.len())
    }

    pub fn recent_tabs(&self, limit: u32) -> Result<Vec<ClosedTab>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, url, title, closed_at, ram_freed_mb, trigger_type, goal_context
               FROM closed_tabs ORDER BY closed_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |r| {
            Ok(ClosedTab {
                id: r.get(0)?,
                url: r.get(1)?,
                title: r.get(2)?,
                closed_at: r.get(3)?,
                ram_freed_mb: r.get(4)?,
                trigger_type: r.get(5)?,
                goal_context: r.get(6)?,
            })
        })?;
        rows.collect()
    }

    pub fn count_tabs(&self) -> Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM closed_tabs", [], |r| r.get(0))
    }

    pub fn clear_tabs(&self) -> Result<usize> {
        self.conn.execute("DELETE FROM closed_tabs", [])
    }

    // ── Actions ─────────────────────────────────────────────────────────────

    /// Record one thing the ladder did.
    pub fn log(&self, a: &NewAction) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO actions
               (at, rung, trigger, psi_some, psi_full, action, target,
                bytes_freed, succeeded, notes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                now_iso(),
                a.rung,
                a.trigger,
                a.psi_some,
                a.psi_full,
                a.action,
                a.target,
                a.bytes_freed,
                a.succeeded as i32,
                a.notes,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn recent_actions(&self, limit: u32) -> Result<Vec<Action>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, at, rung, trigger, psi_some, psi_full, action, target,
                    bytes_freed, succeeded, notes
               FROM actions ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |r| {
            Ok(Action {
                id: r.get(0)?,
                at: r.get(1)?,
                rung: r.get(2)?,
                trigger: r.get(3)?,
                psi_some: r.get(4)?,
                psi_full: r.get(5)?,
                action: r.get(6)?,
                target: r.get(7)?,
                bytes_freed: r.get(8)?,
                succeeded: r.get::<_, i32>(9)? != 0,
                notes: r.get(10)?,
            })
        })?;
        rows.collect()
    }

    // ── Signals ─────────────────────────────────────────────────────────────

    /// Record a batch of feature samples.
    ///
    /// Deliberately not called every tick. At 10-second ticks and 750 visible
    /// processes this table would take six and a half million rows a day, which
    /// is both useless and rude on the user's disk. The caller samples on a
    /// slower cadence and only for processes large enough to matter — about forty
    /// of them here, which is roughly 15 MB a week.
    pub fn log_signals(&self, samples: &[SignalSample], ctx: SampleContext) -> Result<usize> {
        if samples.is_empty() {
            return Ok(0);
        }
        let now = now_iso();
        let epoch = unix_now() as i64;

        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO signals
                   (at, epoch, pid, name, role, verdict, protection, pss, rss,
                    cpu_recent, age_minutes, established, listening,
                    focused, windowed, audio, tty, descendant, scope,
                    psi_some, psi_full, available)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
            )?;
            for s in samples {
                stmt.execute(params![
                    now, epoch, s.pid, s.name, s.role, s.verdict, s.protection,
                    s.pss, s.rss, s.cpu_recent, s.age_minutes, s.established, s.listening,
                    s.focused as i32, s.windowed as i32, s.audio as i32, s.tty as i32,
                    s.descendant as i32, s.scope, ctx.psi_some, ctx.psi_full, ctx.available,
                ])?;
            }
        }
        tx.commit()?;
        Ok(samples.len())
    }

    pub fn count_signals(&self) -> Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM signals", [], |r| r.get(0))
    }

    /// Training rows: features, plus whether the user came back within `horizon`.
    ///
    /// A row is labelled by looking *forward* from it, so only samples with a
    /// full horizon of observation behind them are usable — the most recent
    /// `horizon` seconds are excluded rather than labelled `false`, because
    /// "nothing has happened yet" is not the same as "nothing will".
    ///
    /// Coming back means the process was later seen focused or burning CPU.
    /// Those are the two signals that mean the user actually wanted it, as
    /// opposed to it merely still existing.
    pub fn return_labels(&self, horizon_secs: i64, busy_cpu: f64) -> Result<Vec<LabelledSample>> {
        let cutoff = unix_now() as i64 - horizon_secs;
        let mut stmt = self.conn.prepare(
            "SELECT s.epoch, s.pid, s.name, s.role, s.verdict, s.protection,
                    s.pss, s.rss, s.cpu_recent, s.age_minutes, s.established,
                    s.listening, s.focused, s.windowed, s.audio, s.tty,
                    s.descendant, s.scope, s.psi_some, s.psi_full, s.available,
                    EXISTS (
                      SELECT 1 FROM signals f
                       WHERE f.pid = s.pid
                         AND f.name = s.name
                         AND f.epoch > s.epoch
                         AND f.epoch <= s.epoch + ?1
                         AND (f.focused = 1 OR f.cpu_recent >= ?2)
                    ) AS returned
               FROM signals s
              WHERE s.epoch <= ?3
              ORDER BY s.epoch",
        )?;

        let rows = stmt.query_map(params![horizon_secs, busy_cpu, cutoff], |r| {
            Ok(LabelledSample {
                epoch: r.get(0)?,
                sample: SignalSample {
                    pid: r.get(1)?,
                    name: r.get(2)?,
                    role: r.get(3)?,
                    verdict: r.get(4)?,
                    protection: r.get(5)?,
                    pss: r.get(6)?,
                    rss: r.get(7)?,
                    cpu_recent: r.get(8)?,
                    age_minutes: r.get(9)?,
                    established: r.get(10)?,
                    listening: r.get(11)?,
                    focused: r.get::<_, i32>(12)? != 0,
                    windowed: r.get::<_, i32>(13)? != 0,
                    audio: r.get::<_, i32>(14)? != 0,
                    tty: r.get::<_, i32>(15)? != 0,
                    descendant: r.get::<_, i32>(16)? != 0,
                    scope: r.get(17)?,
                },
                context: SampleContext {
                    psi_some: r.get(18)?,
                    psi_full: r.get(19)?,
                    available: r.get(20)?,
                },
                returned: r.get::<_, i32>(21)? != 0,
            })
        })?;
        rows.collect()
    }

    /// Drop samples older than `days`.
    ///
    /// The log exists to train on recent behaviour, and behaviour from two months
    /// ago is not evidence about this week. Pruning also keeps the database from
    /// growing without bound on a machine that is never reinstalled.
    pub fn prune_signals(&self, days: i64) -> Result<usize> {
        let cutoff = unix_now() as i64 - days * 86_400;
        self.conn
            .execute("DELETE FROM signals WHERE epoch < ?1", [cutoff])
    }

    pub fn clear_signals(&self) -> Result<usize> {
        self.conn.execute("DELETE FROM signals", [])
    }

    /// Total bytes returned by a given action type since the daemon started
    /// recording. Lets the UI answer "has reclaim actually been worth it?"
    pub fn total_freed(&self, action: &str) -> Result<i64> {
        self.conn.query_row(
            "SELECT COALESCE(SUM(bytes_freed), 0) FROM actions WHERE action = ?1",
            [action],
            |r| r.get(0),
        )
    }

    pub fn clear_actions(&self) -> Result<usize> {
        self.conn.execute("DELETE FROM actions", [])
    }
}

/// Seconds since the Unix epoch.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// UTC timestamp in the same shape v1 wrote (`datetime.utcnow().isoformat()`).
fn now_iso() -> String {
    let secs = unix_now();
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_micros())
        .unwrap_or(0);

    // Civil date from a Unix timestamp (Howard Hinnant's algorithm), so no date
    // crate is needed for the one format v1 happens to use.
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}",
        y,
        m,
        d,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60,
        micros
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs() -> Vec<(String, String)> {
        vec![
            ("https://youtube.com/".into(), "YouTube".into()),
            ("https://reddit.com/r/rust".into(), "r/rust".into()),
        ]
    }

    #[test]
    fn saves_and_reads_back_closed_tabs() {
        let h = History::in_memory().unwrap();
        assert_eq!(h.save_tabs(&tabs(), 100.0, "auto", "bug bounty").unwrap(), 2);

        let rows = h.recent_tabs(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].trigger_type, "auto");
        assert_eq!(rows[0].goal_context, "bug bounty");
        assert_eq!(h.count_tabs().unwrap(), 2);
    }

    /// v1 divided the freed total evenly across tabs. Kept so the 424 existing
    /// rows remain comparable with anything v2 writes.
    #[test]
    fn freed_memory_is_divided_evenly_the_way_v1_did() {
        let h = History::in_memory().unwrap();
        h.save_tabs(&tabs(), 100.0, "auto", "").unwrap();
        for row in h.recent_tabs(10).unwrap() {
            assert_eq!(row.ram_freed_mb, 50.0);
        }
    }

    #[test]
    fn a_tab_with_no_title_falls_back_to_its_url() {
        let h = History::in_memory().unwrap();
        h.save_tabs(&[("https://x.test/".into(), String::new())], 0.0, "manual", "")
            .unwrap();
        assert_eq!(h.recent_tabs(1).unwrap()[0].title, "https://x.test/");
    }

    #[test]
    fn saving_nothing_writes_nothing_and_does_not_divide_by_zero() {
        let h = History::in_memory().unwrap();
        assert_eq!(h.save_tabs(&[], 100.0, "auto", "").unwrap(), 0);
        assert_eq!(h.count_tabs().unwrap(), 0);
    }

    #[test]
    fn clearing_removes_every_tab() {
        let h = History::in_memory().unwrap();
        h.save_tabs(&tabs(), 10.0, "auto", "").unwrap();
        assert_eq!(h.clear_tabs().unwrap(), 2);
        assert_eq!(h.count_tabs().unwrap(), 0);
    }

    #[test]
    fn the_limit_is_honoured_and_newest_comes_first() {
        let h = History::in_memory().unwrap();
        for i in 0..5 {
            h.save_tabs(&[(format!("https://x/{i}"), format!("t{i}"))], 0.0, "auto", "")
                .unwrap();
        }
        let rows = h.recent_tabs(2).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].url, "https://x/4", "newest first");
    }

    // ── Actions ─────────────────────────────────────────────────────────────

    fn action(rung: i32, what: &str, bytes: i64) -> NewAction {
        NewAction {
            rung,
            trigger: "psi".into(),
            psi_some: 12.5,
            psi_full: 3.25,
            action: what.into(),
            target: "app-flatpak-com.brave.Browser-1681016714.scope".into(),
            bytes_freed: bytes,
            succeeded: true,
            notes: String::new(),
        }
    }

    /// The record that makes autonomous operation answerable after the fact.
    #[test]
    fn logs_a_ladder_action_with_the_pressure_that_caused_it() {
        let h = History::in_memory().unwrap();
        h.log(&action(0, "reclaim", 137_000_000)).unwrap();

        let rows = h.recent_actions(10).unwrap();
        assert_eq!(rows.len(), 1);
        let a = &rows[0];
        assert_eq!(a.rung, 0);
        assert_eq!(a.action, "reclaim");
        assert_eq!(a.trigger, "psi");
        assert_eq!(a.psi_some, 12.5);
        assert_eq!(a.psi_full, 3.25);
        assert_eq!(a.bytes_freed, 137_000_000);
        assert!(a.succeeded);
        assert!(a.target.contains("brave"));
    }

    /// A rung that refused must be recorded too — "RamWarden did nothing and
    /// here is why" is the answer to most questions about autonomous behaviour.
    #[test]
    fn a_refusal_is_recorded_as_a_failed_action_with_its_reason() {
        let h = History::in_memory().unwrap();
        h.log(&NewAction {
            rung: 3,
            trigger: "psi".into(),
            action: "suspend".into(),
            target: "Discord".into(),
            succeeded: false,
            notes: "Discord is in use — playing audio".into(),
            ..Default::default()
        })
        .unwrap();

        let a = &h.recent_actions(1).unwrap()[0];
        assert!(!a.succeeded);
        assert_eq!(a.bytes_freed, 0);
        assert!(a.notes.contains("playing audio"));
    }

    #[test]
    fn actions_come_back_newest_first() {
        let h = History::in_memory().unwrap();
        h.log(&action(0, "reclaim", 1)).unwrap();
        h.log(&action(3, "suspend", 2)).unwrap();
        let rows = h.recent_actions(10).unwrap();
        assert_eq!(rows[0].action, "suspend");
        assert_eq!(rows[1].action, "reclaim");
    }

    #[test]
    fn totals_are_summed_per_action_type() {
        let h = History::in_memory().unwrap();
        h.log(&action(0, "reclaim", 100)).unwrap();
        h.log(&action(0, "reclaim", 250)).unwrap();
        h.log(&action(3, "suspend", 900)).unwrap();
        assert_eq!(h.total_freed("reclaim").unwrap(), 350);
        assert_eq!(h.total_freed("suspend").unwrap(), 900);
        assert_eq!(h.total_freed("never-happened").unwrap(), 0);
    }

    #[test]
    fn a_manual_action_is_distinguishable_from_an_autonomous_one() {
        let h = History::in_memory().unwrap();
        h.log(&NewAction {
            rung: -1,
            trigger: "manual".into(),
            action: "kill".into(),
            target: "Discord".into(),
            succeeded: true,
            ..Default::default()
        })
        .unwrap();
        let a = &h.recent_actions(1).unwrap()[0];
        assert_eq!(a.rung, -1, "-1 marks a user-initiated action");
        assert_eq!(a.trigger, "manual");
    }

    // ── Signals and labels ──────────────────────────────────────────────────

    fn sample(pid: i32, name: &str, focused: bool, cpu: f64) -> SignalSample {
        SignalSample {
            pid,
            name: name.into(),
            role: "APP".into(),
            verdict: if focused { "IN_USE".into() } else { "IDLE".into() },
            pss: 500 * 1024 * 1024,
            rss: 650 * 1024 * 1024,
            cpu_recent: cpu,
            age_minutes: 600.0,
            focused,
            windowed: true,
            ..Default::default()
        }
    }

    fn ctx() -> SampleContext {
        SampleContext {
            psi_some: 3.5,
            psi_full: 0.1,
            available: 9_000_000_000,
        }
    }

    /// Insert a sample at an explicit epoch, which the public API deliberately
    /// does not allow — labels are derived from time, so the tests need control
    /// of it.
    fn insert_at(h: &History, epoch: i64, s: &SignalSample, c: SampleContext) {
        h.conn
            .execute(
                "INSERT INTO signals (at, epoch, pid, name, role, verdict, protection,
                    pss, rss, cpu_recent, age_minutes, established, listening,
                    focused, windowed, audio, tty, descendant, scope,
                    psi_some, psi_full, available)
                 VALUES ('t',?1,?2,?3,?4,?5,'',?6,?7,?8,?9,0,0,?10,0,0,0,0,NULL,?11,?12,?13)",
                params![
                    epoch, s.pid, s.name, s.role, s.verdict, s.pss, s.rss,
                    s.cpu_recent, s.age_minutes, s.focused as i32,
                    c.psi_some, c.psi_full, c.available
                ],
            )
            .unwrap();
    }

    #[test]
    fn samples_are_recorded_with_their_machine_context() {
        let h = History::in_memory().unwrap();
        let n = h
            .log_signals(&[sample(1, "brave", false, 0.0), sample(2, "Discord", false, 0.0)], ctx())
            .unwrap();
        assert_eq!(n, 2);
        assert_eq!(h.count_signals().unwrap(), 2);
    }

    #[test]
    fn logging_nothing_writes_nothing() {
        let h = History::in_memory().unwrap();
        assert_eq!(h.log_signals(&[], ctx()).unwrap(), 0);
        assert_eq!(h.count_signals().unwrap(), 0);
    }

    /// The label that matters: a process the user came back to must not be
    /// learned as reclaimable.
    #[test]
    fn a_process_the_user_returned_to_is_labelled_returned() {
        let h = History::in_memory().unwrap();
        let now = unix_now() as i64;
        // Idle at T-3600, then focused at T-3000 — inside a 1800s horizon.
        insert_at(&h, now - 3600, &sample(100, "brave", false, 0.0), ctx());
        insert_at(&h, now - 3000, &sample(100, "brave", true, 0.0), ctx());

        let rows = h.return_labels(1800, 0.5).unwrap();
        let first = rows.iter().find(|r| r.epoch == now - 3600).unwrap();
        assert!(first.returned, "the user came back 10 minutes later");
    }

    #[test]
    fn a_cpu_spike_also_counts_as_coming_back() {
        let h = History::in_memory().unwrap();
        let now = unix_now() as i64;
        insert_at(&h, now - 3600, &sample(101, "cargo", false, 0.0), ctx());
        insert_at(&h, now - 3300, &sample(101, "cargo", false, 2.5), ctx());

        let rows = h.return_labels(1800, 0.5).unwrap();
        assert!(rows.iter().find(|r| r.epoch == now - 3600).unwrap().returned);
    }

    #[test]
    fn a_process_that_stayed_idle_is_labelled_not_returned() {
        let h = History::in_memory().unwrap();
        let now = unix_now() as i64;
        for back in [3600, 3300, 3000, 2400] {
            insert_at(&h, now - back, &sample(102, "Discord", false, 0.0), ctx());
        }
        let rows = h.return_labels(1800, 0.5).unwrap();
        assert!(rows.iter().all(|r| !r.returned), "nothing ever came back");
    }

    /// A return *outside* the horizon is not a return for that row's purposes.
    #[test]
    fn a_return_after_the_horizon_does_not_label_the_earlier_sample() {
        let h = History::in_memory().unwrap();
        let now = unix_now() as i64;
        insert_at(&h, now - 7200, &sample(103, "brave", false, 0.0), ctx());
        // Focused 100 minutes later — well past a 30-minute horizon.
        insert_at(&h, now - 1200, &sample(103, "brave", true, 0.0), ctx());

        let rows = h.return_labels(1800, 0.5).unwrap();
        let early = rows.iter().find(|r| r.epoch == now - 7200).unwrap();
        assert!(!early.returned, "a return 100 min later is not within 30 min");
    }

    /// The subtle one: rows too recent to have been observed for a full horizon
    /// must be excluded, not labelled `false`. "Nothing has happened yet" is not
    /// "nothing will happen", and training on that teaches the opposite of the
    /// truth.
    #[test]
    fn rows_without_a_full_horizon_of_observation_are_excluded() {
        let h = History::in_memory().unwrap();
        let now = unix_now() as i64;
        insert_at(&h, now - 60, &sample(104, "brave", false, 0.0), ctx());
        insert_at(&h, now - 5000, &sample(105, "Discord", false, 0.0), ctx());

        let rows = h.return_labels(1800, 0.5).unwrap();
        assert!(
            rows.iter().all(|r| r.epoch <= now - 1800),
            "an unobservable row leaked into the training set"
        );
        assert!(rows.iter().any(|r| r.sample.pid == 105));
        assert!(!rows.iter().any(|r| r.sample.pid == 104));
    }

    /// Two different processes must not label each other, even at the same pid
    /// after a reuse — which is why the name is matched too.
    #[test]
    fn a_recycled_pid_does_not_label_its_predecessor() {
        let h = History::in_memory().unwrap();
        let now = unix_now() as i64;
        insert_at(&h, now - 3600, &sample(200, "old-app", false, 0.0), ctx());
        // Same pid, different program, later focused.
        insert_at(&h, now - 3000, &sample(200, "new-app", true, 0.0), ctx());

        let rows = h.return_labels(1800, 0.5).unwrap();
        let old = rows.iter().find(|r| r.sample.name == "old-app").unwrap();
        assert!(!old.returned, "a different program reusing the pid is not a return");
    }

    #[test]
    fn labels_carry_the_features_and_the_context_through() {
        let h = History::in_memory().unwrap();
        let now = unix_now() as i64;
        insert_at(&h, now - 3600, &sample(300, "brave", false, 0.0), ctx());
        let row = &h.return_labels(1800, 0.5).unwrap()[0];
        assert_eq!(row.sample.name, "brave");
        assert_eq!(row.sample.pss, 500 * 1024 * 1024);
        assert_eq!(row.sample.age_minutes, 600.0);
        assert_eq!(row.context.psi_some, 3.5);
        assert_eq!(row.context.available, 9_000_000_000);
    }

    #[test]
    fn an_empty_log_yields_no_training_rows() {
        let h = History::in_memory().unwrap();
        assert!(h.return_labels(1800, 0.5).unwrap().is_empty());
    }

    #[test]
    fn pruning_drops_only_what_is_older_than_the_window() {
        let h = History::in_memory().unwrap();
        let now = unix_now() as i64;
        insert_at(&h, now - 40 * 86_400, &sample(1, "old", false, 0.0), ctx());
        insert_at(&h, now - 2 * 86_400, &sample(2, "recent", false, 0.0), ctx());

        assert_eq!(h.prune_signals(30).unwrap(), 1);
        assert_eq!(h.count_signals().unwrap(), 1);
        assert_eq!(h.prune_signals(30).unwrap(), 0, "pruning twice is a no-op");
    }

    /// The signal log must not be a second copy of the user's activity. No URLs,
    /// no titles, no document names — only numbers and short categories.
    #[test]
    fn a_sample_holds_no_content_only_behaviour() {
        let s = sample(1, "brave", false, 0.0);
        // The only free-text fields are a program name, a role, a verdict and a
        // cgroup scope. None of them can carry a document or an address.
        assert_eq!(s.name, "brave");
        assert!(s.scope.is_none());
        // `listening` is a count, deliberately not a list of ports.
        assert_eq!(s.listening, 0);
    }

    /// The table is added to an existing database without disturbing it.
    #[test]
    fn the_signal_table_appears_on_an_older_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE closed_tabs (
                     id INTEGER PRIMARY KEY AUTOINCREMENT, url TEXT NOT NULL,
                     title TEXT NOT NULL, closed_at TEXT NOT NULL,
                     ram_freed_mb REAL NOT NULL DEFAULT 0,
                     trigger_type TEXT NOT NULL DEFAULT 'auto');
                 INSERT INTO closed_tabs (url,title,closed_at) VALUES ('u','t','2026-01-01');",
            )
            .unwrap();
        }
        let h = History::open(&path).unwrap();
        assert_eq!(h.count_tabs().unwrap(), 1, "existing rows survive");
        assert_eq!(h.count_signals().unwrap(), 0);
        h.log_signals(&[sample(1, "x", false, 0.0)], ctx()).unwrap();
        assert_eq!(h.count_signals().unwrap(), 1);
    }

    // ── Compatibility ───────────────────────────────────────────────────────

    /// The real compatibility requirement: open a database v1 created, read its
    /// rows, and write alongside them.
    #[test]
    fn opens_a_v1_database_and_writes_alongside_its_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");

        // Build exactly what v1's init_db leaves behind, including the
        // ALTER-added goal_context column.
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE closed_tabs (
                     id           INTEGER PRIMARY KEY AUTOINCREMENT,
                     url          TEXT NOT NULL,
                     title        TEXT NOT NULL,
                     closed_at    TEXT NOT NULL,
                     ram_freed_mb REAL NOT NULL DEFAULT 0,
                     trigger_type TEXT NOT NULL DEFAULT 'auto'
                 , goal_context TEXT NOT NULL DEFAULT '');
                 INSERT INTO closed_tabs (url, title, closed_at, ram_freed_mb, trigger_type)
                 VALUES ('chrome://downloads/', 'Downloads', '2026-06-01T00:00:00', 66.7, 'auto');",
            )
            .unwrap();
        }

        let h = History::open(&path).unwrap();
        assert_eq!(h.count_tabs().unwrap(), 1, "v1 rows must survive");
        assert_eq!(h.recent_tabs(1).unwrap()[0].ram_freed_mb, 66.7);

        h.save_tabs(&tabs(), 20.0, "auto", "").unwrap();
        assert_eq!(h.count_tabs().unwrap(), 3);
        // The new table is created on an old database without disturbing it.
        h.log(&action(0, "reclaim", 5)).unwrap();
        assert_eq!(h.recent_actions(1).unwrap().len(), 1);
    }

    /// A database from an early v1 that predates the goal_context migration.
    #[test]
    fn migrates_a_database_without_the_goal_context_column() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE closed_tabs (
                     id           INTEGER PRIMARY KEY AUTOINCREMENT,
                     url          TEXT NOT NULL,
                     title        TEXT NOT NULL,
                     closed_at    TEXT NOT NULL,
                     ram_freed_mb REAL NOT NULL DEFAULT 0,
                     trigger_type TEXT NOT NULL DEFAULT 'auto'
                 );
                 INSERT INTO closed_tabs (url, title, closed_at)
                 VALUES ('https://old/', 'Old', '2026-01-01T00:00:00');",
            )
            .unwrap();
        }

        let h = History::open(&path).unwrap();
        let rows = h.recent_tabs(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].goal_context, "", "the added column defaults to empty");
    }

    #[test]
    fn opening_twice_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.db");
        {
            let h = History::open(&path).unwrap();
            h.save_tabs(&tabs(), 0.0, "auto", "").unwrap();
        }
        let h = History::open(&path).unwrap();
        assert_eq!(h.count_tabs().unwrap(), 2);
    }

    #[test]
    fn a_missing_parent_directory_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep/er/still/history.db");
        let h = History::open(&path).unwrap();
        assert_eq!(h.count_tabs().unwrap(), 0);
        assert!(path.exists());
    }

    // ── Timestamps ──────────────────────────────────────────────────────────

    #[test]
    fn timestamps_match_the_shape_v1_wrote() {
        let s = now_iso();
        // datetime.utcnow().isoformat() -> 2026-10-01T12:34:56.123456
        assert_eq!(s.len(), 26, "{s}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "T");
        assert_eq!(&s[19..20], ".");
        let year: u32 = s[0..4].parse().unwrap();
        assert!(year >= 2026, "{s}");
    }

    #[test]
    fn timestamps_sort_lexicographically_which_is_what_the_queries_rely_on() {
        let a = "2026-09-30T23:59:59.000000";
        let b = "2026-10-01T00:00:00.000000";
        assert!(a < b);
    }
}
