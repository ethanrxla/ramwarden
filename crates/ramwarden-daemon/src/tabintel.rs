//! The model tier for tabs: an advisory second opinion on borderline cases.
//!
//! The closed-form ranking in [`crate::tabrank`] always decides first and always
//! works offline. This module adds Jev (typed yes/no probabilities) *only* for
//! tabs the closed form is unsure about, and only as a cached adjustment:
//!
//! * **What leaves the machine** is a short description of a tab's *features* —
//!   site type, idle time in coarse buckets, visit history, whether it is related
//!   to what the user is doing — never a URL, a title or a host. Tabs with the
//!   same features produce the same text, so they share one answer: the cache
//!   key is the text itself. That is what keeps cost and exposure small.
//! * **Ranking stays synchronous and pure.** [`adjust`] only *reads* the cache. A
//!   background refresh ([`TabIntel::refresh`]) fills it, so no unload ever waits
//!   on the network, and with the tier off, unconfigured, over budget or tripped
//!   the ranking is exactly the closed-form one.
//! * **Fusion** is a weighted blend on the log-odds scale, which treats each
//!   probability as evidence rather than averaging them linearly. A high answer to
//!   "could this hold unsaved work?" vetoes the unload.
//!
//! Calibration: every unload is later labelled by whether the user came back (see
//! [`crate::outcomes`]), so `/outcomes` can show whether the blended estimate
//! beats the closed form before its weight is raised.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ramwarden_ai::client::{Error as AiError, Result as AiResult};
use ramwarden_ai::jev::{Answers, Jev, Question};
use ramwarden_core::config;

// ── Features and the text that describes them ───────────────────────────────

/// Everything about a tab that may be described to a model.
#[derive(Clone, Debug, PartialEq)]
pub struct Features {
    pub class: &'static str,
    pub idle_minutes: i64,
    pub frecency: f64,
    pub related: bool,
    pub duplicate: bool,
}

fn idle_bucket(minutes: i64) -> &'static str {
    match minutes.max(0) {
        0..=59 => "under 1 hour",
        60..=179 => "1 to 3 hours",
        180..=359 => "3 to 6 hours",
        360..=719 => "6 to 12 hours",
        720..=1439 => "12 to 24 hours",
        1440..=2879 => "1 to 2 days",
        2880..=5759 => "2 to 4 days",
        _ => "over 4 days",
    }
}

fn visits_bucket(frecency: f64) -> &'static str {
    if frecency < 0.2 {
        "none recorded"
    } else if frecency < 1.5 {
        "visited occasionally"
    } else if frecency < 4.0 {
        "visited regularly"
    } else {
        "visited very often"
    }
}

fn class_text(class: &str) -> &'static str {
    match class {
        "feed" => "a social feed or video site",
        "app" => "a web app such as mail, chat or a calendar",
        "reference" => "a code or reference site",
        _ => "an ordinary web page",
    }
}

/// The `state` sent to the model. Coarse on purpose: fewer distinct texts means
/// fewer calls, and a coarse text says less about the user.
pub fn describe(f: &Features) -> String {
    format!(
        "A browser tab the user left in the background. Site type: {}. Idle for {}. Visit history: {}. {} {}",
        class_text(f.class),
        idle_bucket(f.idle_minutes),
        visits_bucket(f.frecency),
        if f.related { "It is on the same site as a tab the user is working in now." } else { "It is unrelated to the tabs the user is working in now." },
        if f.duplicate { "A newer copy of the same page is open." } else { "No other copy of the page is open." },
    )
}

pub const Q_RETURN: &str = "return_24h";
pub const Q_UNSAVED: &str = "unsaved";

pub fn questions() -> Vec<Question> {
    vec![
        Question::new(Q_RETURN, "Will the user come back to this tab within the next 24 hours?"),
        Question::new(Q_UNSAVED, "Could this tab hold unsaved work, a half-filled form, or an in-progress task that would be lost if the tab were unloaded?"),
    ]
}

// ── Fusion ──────────────────────────────────────────────────────────────────

const EDGE: f64 = 0.01;

fn logit(p: f64) -> f64 {
    let p = p.clamp(EDGE, 1.0 - EDGE);
    (p / (1.0 - p)).ln()
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

/// `sigmoid((1 - w) * logit(p_rules) + w * logit(p_model))`. `w = 0` returns the
/// closed form, `w = 1` the model; agreement keeps the value, disagreement lands
/// between, nearer whichever is more extreme in log-odds.
pub fn blend(p_rules: f64, p_model: f64, weight: f64) -> f64 {
    let w = weight.clamp(0.0, 1.0);
    sigmoid((1.0 - w) * logit(p_rules) + w * logit(p_model))
}

/// One model answer about one feature description.
#[derive(Clone, Debug, PartialEq)]
pub struct Judgement {
    pub return_24h: f64,
    pub unsaved: f64,
    pub model: String,
}

/// What the model tier did to a tab's estimate.
#[derive(Clone, Debug, PartialEq)]
pub struct Adjusted {
    pub p: f64,
    pub p_model: Option<f64>,
    pub veto: bool,
}

impl Adjusted {
    pub fn none(p: f64) -> Self {
        Adjusted { p, p_model: None, veto: false }
    }
}

/// Pure: apply a judgement to the closed-form estimate.
pub fn fuse(p_rules: f64, j: &Judgement, weight: f64, unsaved_veto: f64) -> Adjusted {
    if j.unsaved >= unsaved_veto {
        return Adjusted { p: p_rules, p_model: Some(j.return_24h), veto: true };
    }
    Adjusted { p: blend(p_rules, j.return_24h, weight), p_model: Some(j.return_24h), veto: false }
}

// ── The asker, the budget and the breaker ───────────────────────────────────

type Reply<'a> = Pin<Box<dyn Future<Output = AiResult<Answers>> + Send + 'a>>;

/// Anything that can answer questions about a state: Jev, or a fake in tests.
pub trait Asker: Send + Sync {
    fn ask<'a>(&'a self, state: &'a str, questions: &'a [Question]) -> Reply<'a>;
}

impl Asker for Jev {
    fn ask<'a>(&'a self, state: &'a str, questions: &'a [Question]) -> Reply<'a> {
        Box::pin(Jev::ask(self, state, questions))
    }
}

#[derive(Default)]
struct Breaker {
    consecutive: u32,
    open_until: Option<Instant>,
}

const TRIP_AFTER: u32 = 3;
const BACKOFF: Duration = Duration::from_secs(10 * 60);
/// A rejected key will not fix itself: stop for an hour.
const AUTH_BACKOFF: Duration = Duration::from_secs(60 * 60);
const MAX_CACHE: usize = 256;

pub struct TabIntel {
    cfg: config::Jev,
    pub(crate) asker: Option<Box<dyn Asker>>,
    cache: Mutex<HashMap<String, (Judgement, Instant)>>,
    /// Answers about whole apps, kept apart from tab answers (see `appintel`).
    pub(crate) app_cache: Mutex<HashMap<String, (crate::appintel::AppJudgement, Instant)>>,
    calls: Mutex<VecDeque<Instant>>,
    breaker: Mutex<Breaker>,
    input_tokens: AtomicU64,
    output_tokens: AtomicU64,
}

impl TabIntel {
    pub fn new(cfg: config::Jev, asker: Option<Box<dyn Asker>>) -> Self {
        TabIntel {
            cfg,
            asker,
            cache: Mutex::new(HashMap::new()),
            app_cache: Mutex::new(HashMap::new()),
            calls: Mutex::new(VecDeque::new()),
            breaker: Mutex::new(Breaker::default()),
            input_tokens: AtomicU64::new(0),
            output_tokens: AtomicU64::new(0),
        }
    }

    /// A tier that does nothing: ranking is the closed form alone.
    pub fn off() -> Self {
        TabIntel::new(config::Jev::default(), None)
    }

    pub fn active(&self) -> bool {
        self.cfg.enabled && self.asker.is_some()
    }

    pub fn config(&self) -> &config::Jev {
        &self.cfg
    }

    /// A fresh answer for this description, if one is cached. Never blocks on the network.
    pub fn lookup(&self, state: &str, now: Instant) -> Option<Judgement> {
        let ttl = Duration::from_secs(self.cfg.cache_hours * 3600);
        let cache = self.cache.lock().unwrap();
        cache.get(state).filter(|(_, at)| now.duration_since(*at) < ttl).map(|(j, _)| j.clone())
    }

    fn insert(&self, state: String, j: Judgement, now: Instant) {
        let mut cache = self.cache.lock().unwrap();
        if cache.len() >= MAX_CACHE
            && let Some(oldest) = cache.iter().min_by_key(|(_, (_, at))| *at).map(|(k, _)| k.clone())
        {
            cache.remove(&oldest);
        }
        cache.insert(state, (j, now));
    }

    /// Which of `states` to ask about now: distinct, not cached, within the
    /// hourly budget, and only while the breaker is closed.
    pub fn plan(&self, states: &[String], now: Instant) -> Vec<String> {
        let room = self.call_room(now);
        let mut seen = HashSet::new();
        states
            .iter()
            .filter(|s| seen.insert(s.as_str()) && self.lookup(s, now).is_none())
            .take(room)
            .cloned()
            .collect()
    }

    /// How many more calls the hourly budget allows right now: zero while the tier
    /// is off or the breaker is open. Shared by the tab and app questions, which
    /// draw on one budget.
    pub(crate) fn call_room(&self, now: Instant) -> usize {
        if !self.active() || self.breaker.lock().unwrap().open_until.is_some_and(|t| now < t) {
            return 0;
        }
        let mut calls = self.calls.lock().unwrap();
        while calls.front().is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(3600)) {
            calls.pop_front();
        }
        (self.cfg.max_calls_per_hour as usize).saturating_sub(calls.len())
    }

    pub(crate) fn note_call(&self, now: Instant) {
        self.calls.lock().unwrap().push_back(now);
    }

    pub(crate) fn note_ok(&self, a: &Answers) {
        self.breaker.lock().unwrap().consecutive = 0;
        self.input_tokens.fetch_add(a.input_tokens, Ordering::Relaxed);
        self.output_tokens.fetch_add(a.output_tokens, Ordering::Relaxed);
    }

    pub(crate) fn breaker_open(&self, now: Instant) -> bool {
        self.breaker.lock().unwrap().open_until.is_some_and(|t| now < t)
    }

    pub(crate) fn record_failure(&self, e: &AiError, now: Instant) {
        let mut b = self.breaker.lock().unwrap();
        b.consecutive += 1;
        let auth = matches!(e, AiError::Status { status: 401 | 403, .. });
        if auth {
            b.open_until = Some(now + AUTH_BACKOFF);
            tracing::warn!("jev rejected the key ({e}); pausing for an hour");
        } else if b.consecutive >= TRIP_AFTER {
            b.open_until = Some(now + BACKOFF);
            tracing::warn!("jev failed {} times in a row ({e}); pausing for 10 minutes", b.consecutive);
        }
    }

    /// Ask about each state in `plan(...)` and cache the answers. Returns how many
    /// were answered. Sequential: there are only ever a handful.
    pub async fn refresh(&self, states: &[String], now: Instant) -> usize {
        let Some(asker) = &self.asker else { return 0 };
        let qs = questions();
        let mut answered = 0;
        for state in self.plan(states, now) {
            self.calls.lock().unwrap().push_back(now);
            match asker.ask(&state, &qs).await {
                Ok(a) => {
                    self.breaker.lock().unwrap().consecutive = 0;
                    self.input_tokens.fetch_add(a.input_tokens, Ordering::Relaxed);
                    self.output_tokens.fetch_add(a.output_tokens, Ordering::Relaxed);
                    let (Some(&r), Some(&u)) = (a.noul.get(Q_RETURN), a.noul.get(Q_UNSAVED)) else {
                        tracing::warn!("jev reply was missing an expected answer; ignoring it");
                        continue;
                    };
                    self.insert(state, Judgement { return_24h: r, unsaved: u, model: a.model }, now);
                    answered += 1;
                }
                Err(e) => {
                    self.record_failure(&e, now);
                    if self.breaker.lock().unwrap().open_until.is_some_and(|t| now < t) {
                        break;
                    }
                }
            }
        }
        answered
    }

    /// For `/intel`.
    pub fn status(&self, now: Instant) -> serde_json::Value {
        let breaker_secs = self
            .breaker
            .lock()
            .unwrap()
            .open_until
            .and_then(|t| t.checked_duration_since(now))
            .map(|d| d.as_secs());
        let calls = self.calls.lock().unwrap().iter().filter(|t| now.duration_since(**t) < Duration::from_secs(3600)).count();
        let cache = self.cache.lock().unwrap();
        serde_json::json!({
            "enabled": self.cfg.enabled,
            "configured": self.asker.is_some(),
            "weight": self.cfg.weight,
            "cached_answers": cache.len(),
            "model": cache.values().map(|(j, _)| j.model.clone()).next(),
            "calls_last_hour": calls,
            "max_calls_per_hour": self.cfg.max_calls_per_hour,
            "paused_for_seconds": breaker_secs,
            "input_tokens": self.input_tokens.load(Ordering::Relaxed),
            "output_tokens": self.output_tokens.load(Ordering::Relaxed),
        })
    }
}

// ── The process-wide instance ───────────────────────────────────────────────

static GLOBAL: OnceLock<Arc<TabIntel>> = OnceLock::new();

/// Install the tier at startup. Ranking consults it through [`adjust`]; with
/// nothing installed (tests, tier off) ranking is the closed form alone.
pub fn install(intel: Arc<TabIntel>) {
    let _ = GLOBAL.set(intel);
}

pub fn global() -> Option<&'static Arc<TabIntel>> {
    GLOBAL.get()
}

/// Pure read: the closed-form estimate, adjusted by a cached model answer when
/// there is one for these features.
pub fn adjust(p_rules: f64, f: &Features) -> Adjusted {
    let Some(intel) = global().filter(|i| i.active()) else { return Adjusted::none(p_rules) };
    let cfg = intel.config();
    // Only borderline tabs were asked about, so only they can have an answer.
    if p_rules < cfg.band_low || p_rules > cfg.band_high {
        return Adjusted::none(p_rules);
    }
    match intel.lookup(&describe(f), Instant::now()) {
        Some(j) => fuse(p_rules, &j, cfg.weight, cfg.unsaved_veto),
        None => Adjusted::none(p_rules),
    }
}

/// The distinct feature descriptions worth asking about right now: eligible
/// tabs whose closed-form estimate is inside the band, biggest first, capped.
pub fn borderline_states(rows: &[crate::browser::Row], cfg: &config::Jev) -> Vec<String> {
    let mut c: Vec<&crate::browser::Row> = rows
        .iter()
        .filter(|r| r.eligible && r.p_rules >= cfg.band_low && r.p_rules <= cfg.band_high)
        .collect();
    c.sort_by(|a, b| b.est_mb.cmp(&a.est_mb).then(a.target.id.cmp(&b.target.id)));
    c.truncate(cfg.max_candidates);
    c.iter()
        .map(|r| {
            describe(&Features {
                class: r.class,
                idle_minutes: r.inactive_minutes,
                frecency: r.frecency,
                related: r.related,
                duplicate: r.duplicate,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feat() -> Features {
        Features { class: "page", idle_minutes: 840, frecency: 0.0, related: false, duplicate: false }
    }

    fn cfg() -> config::Jev {
        config::Jev { enabled: true, max_calls_per_hour: 3, cache_hours: 6, ..config::Jev::default() }
    }

    struct Fake {
        calls: Mutex<u32>,
        reply: Mutex<Vec<AiResult<(f64, f64)>>>,
    }

    impl Fake {
        fn ok(r: f64, u: f64) -> Self {
            Fake { calls: Mutex::new(0), reply: Mutex::new((0..50).map(|_| Ok((r, u))).collect()) }
        }
        fn failing(status: u16) -> Self {
            let mut v = Vec::new();
            for _ in 0..50 {
                v.push(Err(AiError::Status { service: "jev", status, body: "x".into() }));
            }
            Fake { calls: Mutex::new(0), reply: Mutex::new(v) }
        }
    }

    impl Asker for Arc<Fake> {
        fn ask<'a>(&'a self, _s: &'a str, _q: &'a [Question]) -> Reply<'a> {
            *self.calls.lock().unwrap() += 1;
            let next = self.reply.lock().unwrap().pop().unwrap();
            Box::pin(async move {
                next.map(|(r, u)| Answers {
                    model: "jev-1.13.0".into(),
                    noul: [(Q_RETURN.to_string(), r), (Q_UNSAVED.to_string(), u)].into(),
                    input_tokens: 300,
                    output_tokens: 40,
                })
            })
        }
    }

    fn intel(fake: &Arc<Fake>) -> TabIntel {
        TabIntel::new(cfg(), Some(Box::new(fake.clone())))
    }

    #[test]
    fn the_description_carries_features_and_no_identifying_text() {
        let text = describe(&Features { class: "feed", idle_minutes: 400, frecency: 2.0, related: true, duplicate: true });
        assert!(text.contains("social feed") && text.contains("6 to 12 hours") && text.contains("regularly"));
        assert!(text.contains("same site as a tab") && text.contains("newer copy"));
        for leak in ["http", "www", ".com", "title"] {
            assert!(!text.to_lowercase().contains(leak), "{leak}");
        }
    }

    #[test]
    fn nearby_tabs_share_one_description() {
        // 14 h and 15 h idle fall in one bucket: one cache entry, one call.
        let a = Features { idle_minutes: 840, ..feat() };
        let b = Features { idle_minutes: 900, ..feat() };
        assert_eq!(describe(&a), describe(&b));
        assert_ne!(describe(&a), describe(&Features { idle_minutes: 2000, ..feat() }));
    }

    #[test]
    fn blending_is_exact_at_the_edges_and_pulls_toward_the_model() {
        assert!((blend(0.3, 0.9, 0.0) - 0.3).abs() < 1e-12, "weight 0 ignores the model");
        assert!((blend(0.3, 0.9, 1.0) - 0.9).abs() < 1e-12, "weight 1 is the model");
        assert!((blend(0.4, 0.4, 0.7) - 0.4).abs() < 1e-12, "agreement is unchanged");
        let mid = blend(0.3, 0.9, 0.4);
        assert!(mid > 0.3 && mid < 0.9);
        assert!(blend(0.3, 0.9, 0.6) > mid, "more weight, closer to the model");
        // Probabilities of exactly 0 or 1 stay finite.
        assert!(blend(0.0, 1.0, 0.5).is_finite());
    }

    #[test]
    fn a_high_unsaved_probability_vetoes_and_a_low_one_blends() {
        let j = |u| Judgement { return_24h: 0.05, unsaved: u, model: "m".into() };
        assert!(fuse(0.4, &j(0.8), 0.4, 0.6).veto);
        let a = fuse(0.4, &j(0.1), 0.4, 0.6);
        assert!(!a.veto && a.p < 0.4 && a.p_model == Some(0.05));
    }

    #[tokio::test]
    async fn refresh_caches_answers_and_a_second_pass_asks_nothing() {
        let fake = Arc::new(Fake::ok(0.2, 0.1));
        let t = intel(&fake);
        let now = Instant::now();
        let states = vec![describe(&feat()), describe(&feat()), describe(&Features { class: "feed", ..feat() })];
        assert_eq!(t.refresh(&states, now).await, 2, "duplicates collapse to distinct states");
        assert_eq!(*fake.calls.lock().unwrap(), 2);
        let j = t.lookup(&describe(&feat()), now).unwrap();
        assert_eq!((j.return_24h, j.unsaved, j.model.as_str()), (0.2, 0.1, "jev-1.13.0"));
        assert_eq!(t.refresh(&states, now).await, 0);
        assert_eq!(*fake.calls.lock().unwrap(), 2, "answers are reused");
    }

    #[tokio::test]
    async fn answers_expire_after_the_cache_window() {
        let fake = Arc::new(Fake::ok(0.2, 0.1));
        let t = intel(&fake);
        let now = Instant::now();
        t.refresh(&[describe(&feat())], now).await;
        assert!(t.lookup(&describe(&feat()), now + Duration::from_secs(5 * 3600)).is_some());
        assert!(t.lookup(&describe(&feat()), now + Duration::from_secs(7 * 3600)).is_none());
    }

    /// `n` distinct descriptions: classes crossed with idle buckets.
    fn many_states(n: usize) -> Vec<String> {
        let classes = ["page", "feed", "app", "reference"];
        (0..n)
            .map(|i| describe(&Features { class: classes[i % 4], idle_minutes: 100 + 1500 * (i as i64 / 4), ..feat() }))
            .collect()
    }

    #[tokio::test]
    async fn the_hourly_budget_caps_calls_and_resets() {
        let fake = Arc::new(Fake::ok(0.5, 0.1));
        let t = intel(&fake); // 3 calls per hour
        let now = Instant::now();
        let many = many_states(12);
        assert!(many.iter().collect::<HashSet<_>>().len() >= 8);
        assert_eq!(t.refresh(&many, now).await, 3);
        assert_eq!(t.refresh(&many, now).await, 0, "budget spent");
        assert_eq!(t.refresh(&many, now + Duration::from_secs(3601)).await, 3, "a new hour");
    }

    #[tokio::test]
    async fn repeated_failures_trip_the_breaker_and_a_bad_key_pauses_for_an_hour() {
        let fake = Arc::new(Fake::failing(529));
        let t = TabIntel::new(config::Jev { max_calls_per_hour: 100, ..cfg() }, Some(Box::new(fake.clone())));
        let now = Instant::now();
        let many = many_states(12);
        t.refresh(&many, now).await;
        assert_eq!(*fake.calls.lock().unwrap(), 3, "stops after three in a row");
        assert!(t.plan(&many, now + Duration::from_secs(60)).is_empty(), "paused");
        assert!(!t.plan(&many, now + Duration::from_secs(11 * 60)).is_empty(), "resumes after ten minutes");

        let denied = Arc::new(Fake::failing(401));
        let t = TabIntel::new(config::Jev { max_calls_per_hour: 100, ..cfg() }, Some(Box::new(denied.clone())));
        t.refresh(&many, now).await;
        assert_eq!(*denied.calls.lock().unwrap(), 1, "a rejected key stops at once");
        assert!(t.plan(&many, now + Duration::from_secs(30 * 60)).is_empty());
    }

    #[test]
    fn a_disabled_or_unconfigured_tier_never_plans_a_call() {
        let off = TabIntel::off();
        assert!(!off.active());
        assert!(off.plan(&[describe(&feat())], Instant::now()).is_empty());
        let no_asker = TabIntel::new(cfg(), None);
        assert!(!no_asker.active());
        assert_eq!(adjust(0.3, &feat()), Adjusted::none(0.3), "nothing installed: closed form only");
    }

    #[test]
    fn only_borderline_tabs_are_selected_and_capped_biggest_first() {
        use crate::browser::{Row, Target};
        let row = |id: i64, p: f64, mb: u32, eligible: bool| Row {
            target: Target { browser: "b".into(), id, url: format!("https://secret{id}.example.com/private") },
            title: "private title".into(),
            inactive_minutes: 500 + id * 400,
            eligible,
            closeable: true,
            status: "candidate",
            reason: String::new(),
            priority: 0,
            p_return: p,
            est_mb: mb,
            frecency: 0.0,
            related: false,
            class: "page",
            duplicate: false,
            p_rules: p,
            p_model: None,
        };
        let rows = [row(1, 0.05, 120, true), row(2, 0.3, 120, true), row(3, 0.5, 400, true), row(4, 0.9, 400, true), row(5, 0.3, 120, false)];
        let c = config::Jev { max_candidates: 1, ..config::Jev::default() };
        let one = borderline_states(&rows, &c);
        assert_eq!(one.len(), 1);
        assert!(one[0].contains("1 to 2 days"), "the 400 MB tab (id 3) goes first: {}", one[0]);
        let all = borderline_states(&rows, &config::Jev::default());
        assert_eq!(all.len(), 2, "clearly-stale, clearly-active and ineligible tabs are excluded");
        assert!(all.iter().all(|s| !s.contains("secret") && !s.contains("private")), "no URL or title text");
    }
}
