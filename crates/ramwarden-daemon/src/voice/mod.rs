//! The warden's voice: what it says in its speech bubble, and when.
//!
//! # One voice, many faces
//!
//! The macOS ram, the Windows tray and window, the GTK window and the browser
//! extension all show the same bubble, so the decision about what to say lives
//! here and the clients only render it (`GET /warden/bubble`) and post the
//! user's choice back (`POST /warden/answer`). An answer given in the popup is
//! therefore an answer everywhere, and the warden never asks twice.
//!
//! # Why it asks
//!
//! The sweep closes tabs on estimates: idle time, visit history and Jev's
//! "will the user come back within 24 hours?". The user knows the answer for
//! certain, so for the tabs about to close (and the borderline ones, where Jev
//! is asked too) the warden simply asks. "Keep" holds the tab out of every
//! automatic close for a day; "close" closes it now through the checked manual
//! path; "later" waits. A question never widens what may be closed: only
//! closeable tabs are asked about, and the close itself re-checks everything.
//!
//! # Why it is rationed
//!
//! A pet that talks every minute gets muted, then uninstalled. The gaps, the
//! daily cap and the mute are ceilings, and each kind of bubble has its own so
//! jokes can never crowd out a question. See `docs/specs/warden-voice.md`.

pub mod lines;
pub mod routes;

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ramwarden_core::config;
use ramwarden_core::structures::{LinkedQueue, Lru};
use serde::{Deserialize, Serialize};

use crate::browser::{Row, Target};
use lines::{Lines, fill};

/// How long each kind of bubble stays up if nobody answers it.
const QUESTION_LIFE: Duration = Duration::from_secs(10 * 60);
const REMARK_LIFE: Duration = Duration::from_secs(60);
const REPLY_LIFE: Duration = Duration::from_secs(10);
/// A release older than this is no longer news.
const EVENT_SHELF_LIFE: Duration = Duration::from_secs(30 * 60);
/// Lines not to repeat: the last few said.
const RECENT_LINES: usize = 6;
/// Answers remembered (tabs), coldest forgotten first.
const ANSWER_CAPACITY: usize = 512;
/// Borderline return chances, the band where the daemon asks Jev too.
const BORDERLINE: std::ops::RangeInclusive<f64> = 0.15..=0.6;
/// Tab counts from here on are worth a remark.
const MANY_TABS: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Activity,
    CheckIn,
    Quip,
    Reply,
}

/// A button under a bubble.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Choice {
    pub id: &'static str,
    pub label: &'static str,
}

const CHECK_IN_CHOICES: [Choice; 3] = [
    Choice { id: "keep", label: "Yes, keep it" },
    Choice { id: "close", label: "No, close it" },
    Choice { id: "later", label: "Ask me later" },
];

/// The tab a check-in is about.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TabRef {
    #[serde(flatten)]
    pub target: Target,
    pub title: String,
}

/// What a client shows.
#[derive(Clone, Debug, Serialize)]
pub struct Bubble {
    pub id: String,
    pub warden: &'static str,
    pub kind: Kind,
    pub text: String,
    pub choices: Vec<Choice>,
    pub tab: Option<TabRef>,
    /// Seconds until it goes away on its own, as of this reply.
    pub expires_in_s: u64,
}

/// What the user picked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pick {
    Keep,
    Close,
    Later,
    Dismiss,
}

/// What an answer asks the caller to do.
#[derive(Debug, PartialEq)]
pub enum Answered {
    /// Nothing more: show this reply.
    Said(String),
    /// Close this tab, then report with [`Voice::after_close`].
    Close(TabRef),
}

/// The answer was for a bubble that is no longer up.
#[derive(Debug, PartialEq, Eq)]
pub struct Stale;

/// How a close the user asked for went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseResult {
    Confirmed,
    Queued,
    Refused,
}

/// What the world looks like when the voice is asked to speak.
pub struct Scene<'a> {
    /// Every tab the browsers reported, ranked.
    pub rows: &'a [Row],
    /// What the next sweep would close.
    pub closing: &'a [Target],
    /// Memory in use, percent.
    pub percent: f64,
    pub now: Instant,
    /// Wall-clock seconds, for the daily cap and the mute.
    pub unix: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    Keep,
    Later,
}

#[derive(Clone, Debug)]
struct Event {
    unloaded: usize,
    closed: usize,
    mb: u64,
    at: Instant,
}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    /// Unix seconds; `u64::MAX` is "until unmuted".
    muted_until: Option<u64>,
}

struct Inner {
    /// The bubble on screen and when it goes away.
    current: Option<(Bubble, Instant)>,
    last_any: Option<Instant>,
    last_ask: Option<Instant>,
    last_quip: Option<Instant>,
    day: u64,
    said_today: usize,
    jokes_today: usize,
    muted_until: Option<u64>,
    /// Tabs the user answered about, most recent at the head.
    answers: Lru<Target, (Answer, Instant)>,
    /// The last few lines said, oldest first, so a pool rotates.
    recent: LinkedQueue<String>,
    /// Releases waiting to be reported, oldest first.
    events: LinkedQueue<Event>,
    seq: usize,
}

/// The voice. Shared by every route and the monitor; one lock, never held
/// across an await.
pub struct Voice {
    inner: Mutex<Inner>,
    /// Where the mute is kept. `None`: in memory only (tests).
    store: Option<PathBuf>,
}

impl Default for Voice {
    fn default() -> Self {
        Voice::new(None)
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The default store: `<data dir>/ramwarden/voice.json`.
pub fn default_store() -> PathBuf {
    config::data_dir().join("ramwarden").join("voice.json")
}

fn within(at: Option<Instant>, gap: Duration, now: Instant) -> bool {
    at.is_some_and(|t| now.saturating_duration_since(t) < gap)
}

fn minutes(m: f64) -> Duration {
    Duration::from_secs_f64((m.max(0.0)) * 60.0)
}

/// "45 minutes", "3 hours", "2 days".
pub fn idle_text(minutes: i64) -> String {
    let plural = |n: i64, unit: &str| if n == 1 { format!("1 {unit}") } else { format!("{n} {unit}s") };
    match minutes.max(1) {
        m @ 1..=89 => plural(m, "minute"),
        m @ 90..=2159 => plural((m + 30) / 60, "hour"),
        m => plural((m + 720) / 1440, "day"),
    }
}

/// "512 MB", "1.1 GB".
pub fn mb_text(mb: u64) -> String {
    if mb >= 1024 { format!("{:.1} GB", mb as f64 / 1024.0) } else { format!("{mb} MB") }
}

/// A tab's name for a bubble: its title, or its host, kept short.
fn tab_name(row: &Row) -> String {
    let name = if row.title.trim().is_empty() {
        url::Url::parse(&row.target.url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| "this tab".into())
    } else {
        row.title.trim().to_string()
    };
    if name.chars().count() > 48 { format!("{}…", name.chars().take(47).collect::<String>()) } else { name }
}

impl Voice {
    pub fn new(store: Option<PathBuf>) -> Self {
        let muted_until = store
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Stored>(&b).ok())
            .and_then(|s| s.muted_until);
        Voice {
            inner: Mutex::new(Inner {
                current: None,
                last_any: None,
                last_ask: None,
                last_quip: None,
                day: 0,
                said_today: 0,
                jokes_today: 0,
                muted_until,
                answers: Lru::new(),
                recent: LinkedQueue::new(),
                events: LinkedQueue::new(),
                seq: 0,
            }),
            store,
        }
    }

    // ── Speaking ────────────────────────────────────────────────────────────

    /// The bubble to show now, if any. Called on every client poll: it returns
    /// the bubble already up until it expires, and only then considers saying
    /// something new.
    pub fn speak(&self, cfg: &config::Voice, scene: &Scene) -> Option<Bubble> {
        let mut s = self.inner.lock().unwrap();
        let now = scene.now;
        let day = scene.unix / 86_400;
        if s.day != day {
            s.day = day;
            s.said_today = 0;
            s.jokes_today = 0;
        }
        if !cfg.enabled || s.muted(scene.unix) {
            s.current = None;
            return None;
        }
        if let Some((bubble, until)) = &s.current {
            if now < *until {
                let mut b = bubble.clone();
                b.expires_in_s = until.saturating_duration_since(now).as_secs();
                return Some(b);
            }
            // Ignored: a question that timed out counts as "ask me later".
            if let Some(tab) = bubble.tab.clone() {
                s.answers.put_hot(tab.target, (Answer::Later, now));
            }
            s.current = None;
        }
        if s.said_today >= cfg.max_per_day || within(s.last_any, minutes(cfg.min_gap_minutes), now) {
            return None;
        }
        while s.events.first().is_some_and(|e| now.saturating_duration_since(e.at) > EVENT_SHELF_LIFE) {
            s.events.dequeue();
        }

        let lines = Lines::ram();
        let (kind, text, tab) = if let Some(e) = s.events.dequeue() {
            let (pool, count) = if e.closed > 0 { (&lines.activity.closed, e.closed) } else { (&lines.activity.unloaded, e.unloaded) };
            let line = s.pick(pool);
            (Kind::Activity, fill(&line, &[("count", &count.to_string()), ("mb", &mb_text(e.mb))]), None)
        } else if let Some(row) = (cfg.ask_about_tabs && !within(s.last_ask, minutes(cfg.ask_every_minutes), now))
            .then(|| Self::ask_about(&s, cfg, scene))
            .flatten()
        {
            let line = s.pick(&lines.check_in);
            let text = fill(&line, &[("title", &tab_name(row)), ("idle", &idle_text(row.inactive_minutes))]);
            (Kind::CheckIn, text, Some(TabRef { target: row.target.clone(), title: row.title.clone() }))
        } else if cfg.quips && s.jokes_today < cfg.max_quips_per_day && !within(s.last_quip, minutes(cfg.quip_every_minutes), now) {
            let tabs = scene.rows.len();
            let q = &lines.quips;
            // Alternate a general joke with one about the real numbers.
            let pool = if s.seq.is_multiple_of(2) {
                &q.any
            } else if scene.percent >= 85.0 {
                &q.high_memory
            } else if tabs >= MANY_TABS {
                &q.many_tabs
            } else if scene.percent > 0.0 && scene.percent < 50.0 {
                &q.calm
            } else {
                &q.any
            };
            let line = s.pick(pool);
            let text = fill(&line, &[("tabs", &tabs.to_string()), ("percent", &format!("{:.0}", scene.percent))]);
            (Kind::Quip, text, None)
        } else {
            return None;
        };

        s.said_today += 1;
        s.last_any = Some(now);
        match kind {
            Kind::CheckIn => s.last_ask = Some(now),
            Kind::Quip => {
                s.last_quip = Some(now);
                s.jokes_today += 1;
            }
            _ => {}
        }
        let life = if kind == Kind::CheckIn { QUESTION_LIFE } else { REMARK_LIFE };
        let bubble = Bubble {
            id: uuid::Uuid::new_v4().to_string(),
            warden: "ram",
            kind,
            text,
            choices: if kind == Kind::CheckIn { CHECK_IN_CHOICES.to_vec() } else { Vec::new() },
            tab,
            expires_in_s: life.as_secs(),
        };
        s.current = Some((bubble.clone(), now + life));
        Some(bubble)
    }

    /// The tab worth asking about: first what the next sweep would close, then
    /// the borderline ones. Biggest estimated memory first, then longest idle.
    fn ask_about<'a>(s: &Inner, cfg: &config::Voice, scene: &Scene<'a>) -> Option<&'a Row> {
        let askable = |r: &&Row| {
            r.closeable
                && r.inactive_minutes as f64 >= cfg.ask_after_minutes
                && !s.answered(&r.target, cfg, scene.now)
        };
        let best = |rows: Vec<&'a Row>| {
            rows.into_iter().max_by(|a, b| a.est_mb.cmp(&b.est_mb).then(a.inactive_minutes.cmp(&b.inactive_minutes)).then(b.target.id.cmp(&a.target.id)))
        };
        let soon: Vec<&Row> = scene.rows.iter().filter(|r| scene.closing.contains(&r.target)).filter(askable).collect();
        best(soon).or_else(|| {
            best(scene.rows.iter()
                .filter(|r| matches!(r.status, "candidate" | "unloaded") && BORDERLINE.contains(&r.p_return))
                .filter(askable)
                .collect())
        })
    }

    // ── Answers ─────────────────────────────────────────────────────────────

    /// Take the user's choice on bubble `id`.
    pub fn answer(&self, id: &str, pick: Pick, now: Instant) -> Result<Answered, Stale> {
        let mut s = self.inner.lock().unwrap();
        let Some((bubble, until)) = s.current.clone() else { return Err(Stale) };
        if bubble.id != id || now >= until {
            return Err(Stale);
        }
        s.current = None;
        let lines = Lines::ram();
        let answered = match (bubble.tab, pick) {
            (Some(tab), Pick::Close) => Ok(Answered::Close(tab)),
            (Some(tab), Pick::Keep) => {
                s.answers.put_hot(tab.target, (Answer::Keep, now));
                Ok(Answered::Said(s.reply(&lines.replies.keep, now)))
            }
            (Some(tab), Pick::Later | Pick::Dismiss) => {
                s.answers.put_hot(tab.target, (Answer::Later, now));
                let text = if pick == Pick::Later { s.reply(&lines.replies.later, now) } else { String::new() };
                Ok(Answered::Said(text))
            }
            // A remark has nothing to keep or close: any choice dismisses it.
            (None, _) => Ok(Answered::Said(String::new())),
        };
        s.trim_answers();
        answered
    }

    /// Report a close the user asked for. Only a confirmed or queued close
    /// counts; a refused one says so and is treated as "ask me later".
    pub fn after_close(&self, tab: &Target, result: CloseResult, now: Instant) -> String {
        let mut s = self.inner.lock().unwrap();
        let r = &Lines::ram().replies;
        match result {
            CloseResult::Confirmed => s.reply(&r.close, now),
            CloseResult::Queued => s.reply(&r.close_queued, now),
            CloseResult::Refused => {
                s.answers.put_hot(tab.clone(), (Answer::Later, now));
                s.reply(&r.close_failed, now)
            }
        }
    }

    // ── What the answers do to closing ──────────────────────────────────────

    /// Whether automatic closing must leave this tab alone: the user said they
    /// are coming back, or the question about it is on screen right now.
    pub fn holds(&self, cfg: &config::Voice, target: &Target, now: Instant) -> bool {
        let s = self.inner.lock().unwrap();
        s.holds(cfg, target, now)
    }

    /// Drop held tabs from a list the sweep is about to close.
    pub fn hold_back(&self, cfg: &config::Voice, targets: &mut Vec<Target>, now: Instant) -> usize {
        let s = self.inner.lock().unwrap();
        let before = targets.len();
        targets.retain(|t| !s.holds(cfg, t, now));
        before - targets.len()
    }

    /// A release pass finished: worth a word, if it did anything.
    pub fn note_release(&self, unloaded: usize, closed: usize, mb: u64, now: Instant) {
        if unloaded == 0 && closed == 0 {
            return;
        }
        let mut s = self.inner.lock().unwrap();
        s.events.enqueue(Event { unloaded, closed, mb, at: now });
        while s.events.len() > 4 {
            s.events.dequeue();
        }
    }

    // ── Mute ────────────────────────────────────────────────────────────────

    /// `(muted, until)`; `until` is `None` for "until unmuted".
    pub fn mute_state(&self, unix: u64) -> (bool, Option<u64>) {
        let s = self.inner.lock().unwrap();
        let muted = s.muted(unix);
        (muted, s.muted_until.filter(|&u| muted && u != u64::MAX))
    }

    /// Mute for `hours`, or until unmuted with `None`; unmute with `false`.
    /// Saved, so it survives a restart.
    pub fn set_mute(&self, muted: bool, hours: Option<f64>, unix: u64) -> String {
        let mut s = self.inner.lock().unwrap();
        s.muted_until = muted.then(|| match hours {
            Some(h) if h > 0.0 => unix.saturating_add((h * 3600.0) as u64),
            _ => u64::MAX,
        });
        if muted {
            s.current = None;
        }
        if let Some(path) = &self.store {
            let stored = Stored { muted_until: s.muted_until };
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = std::fs::write(path, serde_json::to_vec(&stored).unwrap_or_default()) {
                tracing::warn!("voice: could not save the mute to {}: {e}", path.display());
            }
        }
        if muted { s.pick(&Lines::ram().replies.muted) } else { String::new() }
    }
}

impl Inner {
    fn muted(&self, unix: u64) -> bool {
        self.muted_until.is_some_and(|u| unix < u)
    }

    fn answered(&self, target: &Target, cfg: &config::Voice, now: Instant) -> bool {
        match self.answers.peek(target) {
            Some((Answer::Keep, at)) => now.saturating_duration_since(*at) < minutes(cfg.keep_hours * 60.0),
            Some((Answer::Later, at)) => now.saturating_duration_since(*at) < minutes(cfg.later_hours * 60.0),
            None => false,
        }
    }

    fn holds(&self, cfg: &config::Voice, target: &Target, now: Instant) -> bool {
        let asking = self.current.as_ref().is_some_and(|(b, until)| now < *until && b.tab.as_ref().is_some_and(|t| &t.target == target));
        let kept = matches!(self.answers.peek(target), Some((Answer::Keep, at)) if now.saturating_duration_since(*at) < minutes(cfg.keep_hours * 60.0));
        asking || kept
    }

    fn trim_answers(&mut self) {
        while self.answers.len() > ANSWER_CAPACITY {
            self.answers.pop_coldest();
        }
    }

    /// The next line from `pool` that was not said recently.
    fn pick(&mut self, pool: &[String]) -> String {
        if pool.is_empty() {
            return String::new();
        }
        self.seq = self.seq.wrapping_add(1);
        let start = self.seq % pool.len();
        let line = (0..pool.len())
            .map(|i| &pool[(start + i) % pool.len()])
            .find(|l| !self.recent.iter().any(|r| r == *l))
            .unwrap_or(&pool[start])
            .clone();
        self.recent.enqueue(line.clone());
        while self.recent.len() > RECENT_LINES {
            self.recent.dequeue();
        }
        line
    }

    /// A short reply bubble: up for a few seconds, never counted.
    fn reply(&mut self, pool: &[String], now: Instant) -> String {
        let text = self.pick(pool);
        self.current = Some((
            Bubble {
                id: uuid::Uuid::new_v4().to_string(),
                warden: "ram",
                kind: Kind::Reply,
                text: text.clone(),
                choices: Vec::new(),
                tab: None,
                expires_in_s: REPLY_LIFE.as_secs(),
            },
            now + REPLY_LIFE,
        ));
        text
    }
}

#[cfg(test)]
mod tests;
