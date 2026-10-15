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

/// `exp(-(t/λ)^k)` with λ chosen so the curve passes through 0.5 at the half-life.
pub fn p_return(idle_minutes: i64, c: Class, older_duplicate: bool) -> f64 {
    let t = idle_minutes.max(0) as f64;
    let lambda = half_life_minutes(c) / std::f64::consts::LN_2.powf(1.0 / SHAPE);
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
            let p = p_return(half_life_minutes(c) as i64, c, false);
            assert!((p - 0.5).abs() < 1e-3, "{c:?}: {p}");
        }
    }

    #[test]
    fn longer_idle_means_less_likely_to_return() {
        let mut last = 1.0;
        for t in [0, 10, 60, 600, 6_000, 60_000] {
            let p = p_return(t, Class::Page, false);
            assert!(p <= last && (0.0..=1.0).contains(&p));
            last = p;
        }
        assert_eq!(p_return(0, Class::Page, false), 1.0);
        assert_eq!(p_return(-5, Class::Page, false), 1.0, "clock skew cannot raise it above 1");
    }

    #[test]
    fn heavy_tail_keeps_a_long_idle_tab_possible() {
        // A shape of 0.6 decays far slower than an exponential at the same half-life.
        let weibull = p_return(5_760, Class::Page, false); // 8 half-lives
        let exponential = 0.5f64.powi(8);
        assert!(weibull > exponential * 2.0, "{weibull} vs {exponential}");
    }

    #[test]
    fn an_older_duplicate_is_cut_tenfold() {
        let a = p_return(300, Class::Page, false);
        let b = p_return(300, Class::Page, true);
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
        let forgotten_video = score(est_mb(Class::Feed), p_return(600, Class::Feed, false));
        let recent_reference = score(est_mb(Class::Reference), p_return(60, Class::Reference, false));
        assert!(forgotten_video > recent_reference * 10.0);
    }

    #[test]
    fn at_equal_regret_the_bigger_tab_goes_first() {
        let p = 0.2;
        assert!(score(400, p) > score(80, p));
        // And at equal size, the one least likely to return goes first.
        assert!(score(120, 0.05) > score(120, 0.5));
    }
}
