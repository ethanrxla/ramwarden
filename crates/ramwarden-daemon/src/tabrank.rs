//! Which tabs to release first: a closed-form model, no network, no training.
//!
//! Each eligible tab gets two numbers:
//!
//! * `p_return` — the probability the user comes back to it, from a Weibull
//!   survival curve in idle time: `S(t) = exp(-(t/λ)^k)`. `k < 1` gives the heavy
//!   tail real tabs have (most are abandoned quickly, a few are revisited days
//!   later). `λ` comes from a per-site-class half-life, so `S(half_life) = 0.5`
//!   exactly. An older duplicate's `p_return` is cut tenfold: the retained copy
//!   already covers the page.
//! * `est_mb` — a prior for what the tab holds in RAM, by site class. The
//!   extension cannot see per-tab memory, so this is an estimate and is reported
//!   as one.
//!
//! Releasing a tab costs `p_return` (the chance the user pays a reload) and
//! frees `est_mb`, so the best tabs to release are the ones with the most MB per
//! unit of expected regret: `score = est_mb / (p_return + ε)`. For a memory target
//! this is the fractional-knapsack ordering — optimal when tabs are divisible,
//! and within one tab of optimal when they are not.
//!
//! Cost: one pass to count duplicates (hash map), O(1) per tab to classify and
//! score, then one O(n log n) sort. No allocation per tab beyond the row.
//!
//! The half-lives and sizes are priors, not measurements. They are the part to
//! calibrate from `closed_tabs` history, or to refine with a model, later; the
//! ordering logic above them does not change.

use url::Url;

/// Site classes with distinct reuse and memory behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Feeds and video: abandoned fast, heavy while loaded.
    Feed,
    /// Mail, chat, calendar, meetings: revisited often, heavy while loaded.
    App,
    /// Code and reference: revisited, light.
    Reference,
    /// Everything else: articles, shops, search results.
    Page,
}

const FEED: &[&str] = &[
    "youtube.com", "reddit.com", "twitter.com", "x.com", "instagram.com", "tiktok.com", "facebook.com",
    "twitch.tv", "netflix.com", "news.ycombinator.com", "medium.com", "substack.com",
];
const APP: &[&str] = &[
    "mail.google.com", "outlook.live.com", "outlook.office.com", "calendar.google.com", "teams.microsoft.com",
    "meet.google.com", "slack.com", "discord.com", "web.whatsapp.com", "maps.google.com", "open.spotify.com",
];
const REFERENCE: &[&str] = &[
    "github.com", "gitlab.com", "stackoverflow.com", "developer.mozilla.org", "wikipedia.org", "arxiv.org",
    "docs.rs", "crates.io", "pypi.org", "npmjs.com",
];

fn host_matches(host: &str, domain: &str) -> bool {
    host == domain || host.strip_suffix(domain).is_some_and(|p| p.ends_with('.'))
}

pub fn class_name(c: Class) -> &'static str {
    match c {
        Class::Feed => "feed",
        Class::App => "app",
        Class::Reference => "reference",
        Class::Page => "page",
    }
}

pub fn classify(url: &str) -> Class {
    let Some(host) = Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)) else {
        return Class::Page;
    };
    let any = |list: &[&str]| list.iter().any(|d| host_matches(&host, d));
    if any(FEED) {
        Class::Feed
    } else if any(APP) {
        Class::App
    } else if any(REFERENCE) {
        Class::Reference
    } else {
        Class::Page
    }
}

/// Minutes until the chance of returning halves.
pub fn half_life_minutes(c: Class) -> f64 {
    match c {
        Class::Feed => 90.0,
        Class::Page => 720.0,
        Class::App => 360.0,
        Class::Reference => 2_880.0,
    }
}

/// Prior RAM held by a loaded tab of this class, in MB.
pub fn est_mb(c: Class) -> u32 {
    match c {
        Class::Feed => 250,
        Class::App => 400,
        Class::Page => 120,
        Class::Reference => 80,
    }
}

/// Weibull shape: below 1, the hazard falls with age (a long-idle tab is
/// unlikelier to be opened than a recent one, but never impossible).
const SHAPE: f64 = 0.6;
/// A tab that has an identical, more recently used twin is mostly redundant.
const DUPLICATE_FACTOR: f64 = 0.1;
/// Keeps the score finite when `p_return` is vanishingly small.
const EPSILON: f64 = 0.01;

/// Frecency decay: an activation's weight halves roughly every 2 days
/// (`exp(-age/τ)` with τ = 3 days).
const FRECENCY_TAU_MS: f64 = 3.0 * 24.0 * 3_600_000.0;
/// Frecency counts past this add nothing: five visits and fifty look alike.
const FRECENCY_CAP: f64 = 8.0;
/// Each unit of frecency stretches the half-life by this much.
const FRECENCY_GAIN: f64 = 0.5;
/// A tab on the same site as something the user is using right now.
const RELATED_FACTOR: f64 = 2.0;

/// Recency-weighted visit count: `Σ exp(-(now - t_k)/τ)` over activations.
/// A tab opened once a minute ago scores about 1; one visited daily for a week
/// scores about 4. Timestamps ahead of `now` (clock skew) count as just now.
pub fn frecency(activations: &[f64], now_ms: f64) -> f64 {
    activations.iter().map(|&t| (-((now_ms - t).max(0.0)) / FRECENCY_TAU_MS).exp()).sum()
}

/// How much longer than the site-class default this tab's return chance lasts:
/// frequently visited tabs, and tabs related to what the user is doing now,
/// are kept longer. `>= 1`, at most `(1 + 0.5*8) * 2 = 10`.
pub fn half_life_multiplier(frecency: f64, related: bool) -> f64 {
    (1.0 + FRECENCY_GAIN * frecency.min(FRECENCY_CAP)) * if related { RELATED_FACTOR } else { 1.0 }
}

/// A coarse "same site" key: the host without `www.`, reduced to its registered
/// domain (last two labels, or three under `co.uk`-style suffixes).
pub fn site_key(url: &str) -> Option<String> {
    let host = Url::parse(url).ok()?.host_str()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    if host.parse::<std::net::IpAddr>().is_ok() {
        return Some(host.to_string());
    }
    let labels: Vec<&str> = host.split('.').collect();
    let n = labels.len();
    if n <= 2 {
        return Some(host.to_string());
    }
    let second_level = matches!(labels[n - 2], "co" | "com" | "org" | "net" | "ac" | "gov" | "edu") && labels[n - 1].len() == 2;
    let keep = if second_level { 3 } else { 2 };
    Some(labels[n - keep..].join("."))
}

/// `exp(-(t/λ)^k)` with λ chosen so the curve passes through 0.5 at the half-life,
/// stretched by `half_life_mult` (see [`half_life_multiplier`]; pass 1.0 for none).
pub fn p_return(idle_minutes: i64, c: Class, older_duplicate: bool, half_life_mult: f64) -> f64 {
    let t = idle_minutes.max(0) as f64;
    let lambda = half_life_minutes(c) * half_life_mult.max(1.0) / std::f64::consts::LN_2.powf(1.0 / SHAPE);
    let p = (-(t / lambda).powf(SHAPE)).exp();
    if older_duplicate { p * DUPLICATE_FACTOR } else { p }
}

/// MB released per unit of expected regret. Higher means release sooner.
pub fn score(est_mb: u32, p_return: f64) -> f64 {
    est_mb as f64 / (p_return + EPSILON)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_half_life_is_exact() {
        for c in [Class::Feed, Class::App, Class::Reference, Class::Page] {
            let p = p_return(half_life_minutes(c) as i64, c, false, 1.0);
            assert!((p - 0.5).abs() < 1e-3, "{c:?}: {p}");
        }
    }

    #[test]
    fn longer_idle_means_less_likely_to_return() {
        let mut last = 1.0;
        for t in [0, 10, 60, 600, 6_000, 60_000] {
            let p = p_return(t, Class::Page, false, 1.0);
            assert!(p <= last && (0.0..=1.0).contains(&p));
            last = p;
        }
        assert_eq!(p_return(0, Class::Page, false, 1.0), 1.0);
        assert_eq!(p_return(-5, Class::Page, false, 1.0), 1.0, "clock skew cannot raise it above 1");
    }

    #[test]
    fn heavy_tail_keeps_a_long_idle_tab_possible() {
        // A shape of 0.6 decays far slower than an exponential at the same half-life.
        let weibull = p_return(5_760, Class::Page, false, 1.0); // 8 half-lives
        let exponential = 0.5f64.powi(8);
        assert!(weibull > exponential * 2.0, "{weibull} vs {exponential}");
    }

    #[test]
    fn an_older_duplicate_is_cut_tenfold() {
        let a = p_return(300, Class::Page, false, 1.0);
        let b = p_return(300, Class::Page, true, 1.0);
        assert!((a / b - 10.0).abs() < 1e-9);
    }

    #[test]
    fn classification_uses_the_registered_domain() {
        assert_eq!(classify("https://www.youtube.com/watch?v=1"), Class::Feed);
        assert_eq!(classify("https://old.reddit.com/r/rust"), Class::Feed);
        assert_eq!(classify("https://mail.google.com/mail/u/0"), Class::App);
        assert_eq!(classify("https://en.wikipedia.org/wiki/Rust"), Class::Reference);
        assert_eq!(classify("https://notyoutube.com/"), Class::Page, "suffix must be a whole label");
        assert_eq!(classify("not a url"), Class::Page);
    }

    #[test]
    fn heavy_and_forgotten_beats_light_and_recent() {
        let forgotten_video = score(est_mb(Class::Feed), p_return(600, Class::Feed, false, 1.0));
        let recent_reference = score(est_mb(Class::Reference), p_return(60, Class::Reference, false, 1.0));
        assert!(forgotten_video > recent_reference * 10.0);
    }

    #[test]
    fn at_equal_regret_the_bigger_tab_goes_first() {
        let p = 0.2;
        assert!(score(400, p) > score(80, p));
        // And at equal size, the one least likely to return goes first.
        assert!(score(120, 0.05) > score(120, 0.5));
    }

    const DAY: f64 = 86_400_000.0;

    #[test]
    fn frecency_weighs_recent_and_repeated_visits() {
        let now = 10.0 * DAY;
        assert_eq!(frecency(&[], now), 0.0);
        assert!((frecency(&[now], now) - 1.0).abs() < 1e-12);
        // One visit three days ago counts e^-1.
        assert!((frecency(&[now - 3.0 * DAY], now) - (-1.0f64).exp()).abs() < 1e-12);
        // Daily for a week beats one visit a week ago, and beats one visit today's worth.
        let daily: Vec<f64> = (0..7).map(|d| now - d as f64 * DAY).collect();
        assert!(frecency(&daily, now) > 3.0);
        assert!(frecency(&daily, now) > frecency(&[now - 7.0 * DAY], now) * 5.0);
        // Clock skew cannot score above a visit right now.
        assert!((frecency(&[now + DAY], now) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn multiplier_is_bounded_and_monotone() {
        assert_eq!(half_life_multiplier(0.0, false), 1.0);
        assert_eq!(half_life_multiplier(0.0, true), 2.0);
        assert!(half_life_multiplier(2.0, false) > half_life_multiplier(1.0, false));
        assert_eq!(half_life_multiplier(1_000.0, true), 10.0, "capped");
    }

    #[test]
    fn a_frequently_visited_or_related_tab_is_likelier_to_be_kept() {
        let base = p_return(600, Class::Page, false, 1.0);
        let frequent = p_return(600, Class::Page, false, half_life_multiplier(4.0, false));
        let related = p_return(600, Class::Page, false, half_life_multiplier(0.0, true));
        assert!(frequent > base && related > base);
        // The half-life really does stretch: p(2 * h) at multiplier 2 is 0.5.
        let h = half_life_minutes(Class::Page);
        assert!((p_return((2.0 * h) as i64, Class::Page, false, 2.0) - 0.5).abs() < 1e-3);
        // A multiplier below 1 is ignored rather than shortening the half-life.
        assert_eq!(p_return(600, Class::Page, false, 0.1), base);
    }

    #[test]
    fn site_key_groups_a_registered_domain() {
        assert_eq!(site_key("https://www.github.com/a/b").as_deref(), Some("github.com"));
        assert_eq!(site_key("https://gist.github.com/x").as_deref(), Some("github.com"));
        assert_eq!(site_key("https://docs.google.com/d/1").as_deref(), Some("google.com"));
        assert_eq!(site_key("https://news.bbc.co.uk/x").as_deref(), Some("bbc.co.uk"));
        assert_eq!(site_key("http://127.0.0.1:8765/x").as_deref(), Some("127.0.0.1"));
        assert_eq!(site_key("not a url"), None);
    }
}
