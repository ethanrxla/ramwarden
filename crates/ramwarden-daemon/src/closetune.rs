//! When a tab counts as unused: in active browsing time, stretched per tab.
//!
//! Wall-clock idle treated a tab left overnight like one passed over all
//! afternoon, so a sweep tuned for one was wrong for the other. Two changes,
//! ported from the Mac build (f430a22):
//!
//! * **Active idle** ([`ActivityClock`]) counts browsing time spent elsewhere
//!   since the tab was last used. The extension reports the last 24 activations
//!   of every tab; their union is the user's recent browsing. A gap between two
//!   pieces of activity counts up to [`AWAY_GAP_MINUTES`], so sleep, a meeting or
//!   a lunch break adds at most that. A report with no activation history (an
//!   old extension, polling) has no clock, and wall-clock idle is used instead.
//!   Known limit: one long read in a single tab, with no switching, adds only
//!   up to ten minutes, which errs toward keeping tabs.
//! * **Per-tab stretch** ([`Row::stretch`]): the floor (`close_idle_minutes`, which
//!   `tabpace` already learns from this user's habits) is lengthened for a tab
//!   visited often, on a site in use now, or of a kind people keep open (mail,
//!   chat, docs), and shortened for feeds and older copies of an open page.
//!
//! Active idle is never more than wall-clock idle, so the wall-clock floor the
//! extension re-checks ([`crate::browser::close_floor`]) is always met by a
//! tab that cleared the active one.

use ramwarden_core::config::Tabs;

use crate::browser::Row;
use crate::tabs::Tab;

/// A gap in browsing longer than this counts as time away, not time spent elsewhere.
pub const AWAY_GAP_MINUTES: f64 = 10.0;
/// The extension keeps this many activations per tab (`background.js`). A tab
/// at the cap has lost its older ones, so its history is incomplete before its
/// oldest retained activation.
pub const ACTIVATIONS_KEPT: usize = 24;
/// At most this much longer for a tab visited often.
const MAX_VISIT_STRETCH: f64 = 5.0;

/// The user's recent browsing activity in one browser, as sorted timestamps (ms).
#[derive(Debug, Default)]
pub struct ActivityClock {
    times: Vec<f64>,
    /// Before this time some tab's activations were cut off, so the gaps there
    /// are unknown and count as wall-clock time, as they did before active idle.
    /// Without it, steady switching between a few tabs would roll their history
    /// over every half hour and an untouched tab would never reach the floor.
    complete_after: f64,
}

impl ActivityClock {
    /// Every tab's activations, plus each tab's last use when any activation
    /// history exists. Empty when the report has none.
    pub fn from_tabs(tabs: &[Tab]) -> Self {
        let mut times: Vec<f64> = tabs.iter().flat_map(|t| t.activations.iter().copied()).filter(|t| t.is_finite() && *t > 0.0).collect();
        if times.is_empty() {
            return ActivityClock::default();
        }
        times.extend(tabs.iter().map(|t| t.last_active_ms).filter(|t| t.is_finite() && *t > 0.0));
        times.sort_by(f64::total_cmp);
        times.dedup();
        let complete_after = tabs
            .iter()
            .filter(|t| t.activations.len() >= ACTIVATIONS_KEPT)
            .filter_map(|t| t.activations.iter().copied().filter(|a| a.is_finite() && *a > 0.0).min_by(f64::total_cmp))
            .fold(0.0, f64::max);
        ActivityClock { times, complete_after }
    }

    pub fn is_empty(&self) -> bool {
        self.times.is_empty()
    }

    /// Minutes of browsing elsewhere between `since_ms` and `now_ms`. At most
    /// the wall-clock time between them.
    pub fn active_minutes_since(&self, since_ms: f64, now_ms: f64) -> f64 {
        let gap = AWAY_GAP_MINUTES * 60_000.0;
        // The span with incomplete history counts in full; the walk starts
        // after it, so an older timestamp cannot take that time back.
        let mut prev = since_ms.max(self.complete_after.min(now_ms));
        let mut total = prev - since_ms;
        let start = self.times.partition_point(|t| *t <= prev);
        for &t in &self.times[start..] {
            if t > now_ms {
                break;
            }
            total += (t - prev).min(gap);
            prev = t;
        }
        total += (now_ms - prev).max(0.0).min(gap);
        total / 60_000.0
    }

    /// A tab's active idle in minutes, or its wall-clock idle without a clock.
    pub fn idle_minutes(&self, tab: &Tab, now_ms: f64) -> i64 {
        if self.is_empty() || !tab.last_active_ms.is_finite() || tab.last_active_ms <= 0.0 {
            return tab.inactive_minutes.max(0);
        }
        (self.active_minutes_since(tab.last_active_ms, now_ms).round() as i64).min(tab.inactive_minutes.max(0))
    }
}

impl Row {
    /// How much longer than the floor this tab must go unused.
    pub fn stretch(&self) -> f64 {
        let visits = crate::tabrank::half_life_multiplier(self.frecency, false).min(MAX_VISIT_STRETCH);
        let related = if self.related { 2.0 } else { 1.0 };
        let class = match self.class {
            "feed" => 0.5,
            "reference" => 1.5,
            // Mail, calendar, chat, music: tabs people keep open to come back to.
            "app" => 3.0,
            _ => 1.0,
        };
        let copy = if self.duplicate { 0.5 } else { 1.0 };
        visits * related * class * copy
    }

    /// Active-idle minutes this tab needs before the sweep may take it. A
    /// browser settings page holds no work, so it gets the plain floor.
    pub fn needed_minutes(&self, cfg: &Tabs) -> i64 {
        let floor = cfg.close_idle_minutes() as f64;
        let need = if self.status == "browser page" { floor } else { floor * self.stretch() };
        need.round() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: f64 = 60_000.0;

    fn tab(id: i64, last_ms: f64, activations: &[f64], now_ms: f64) -> Tab {
        serde_json::from_value(serde_json::json!({"id":id,"url":"https://example.org/","lastActiveMs":last_ms,
            "inactiveMinutes":((now_ms-last_ms)/MIN) as i64,"activations":activations})).unwrap()
    }

    #[test]
    fn active_idle_counts_browsing_elsewhere_not_time_away() {
        let now = 1_000.0 * MIN;
        // Tab 1 last used at minute 700. Tab 2 was used every 5 minutes from
        // 700 to 1000: five hours of browsing elsewhere.
        let busy: Vec<f64> = (0..=60).map(|i| (700.0 + 5.0 * i as f64) * MIN).collect();
        let tabs = [tab(1, 700.0 * MIN, &[700.0 * MIN], now), tab(2, now, &busy, now)];
        let clock = ActivityClock::from_tabs(&tabs);
        assert_eq!(clock.idle_minutes(&tabs[0], now), 300);
        // The same five hours with the browser untouched (asleep): 10 minutes.
        let asleep = [tab(1, 700.0 * MIN, &[700.0 * MIN], now), tab(2, 700.0 * MIN, &[650.0 * MIN], now)];
        assert_eq!(ActivityClock::from_tabs(&asleep).idle_minutes(&asleep[0], now), 10);
        // No activation history at all: wall-clock idle.
        let legacy = [tab(1, 700.0 * MIN, &[], now)];
        assert_eq!(ActivityClock::from_tabs(&legacy).idle_minutes(&legacy[0], now), 300);
    }

    /// Two tabs switched every minute for seven hours keep only their last 24
    /// activations each. The cut-off span counts as wall clock, so the
    /// untouched tab still reaches seven hours, not the 48 minutes left.
    #[test]
    fn rolled_over_history_still_counts_a_long_busy_session() {
        let now = 1_000.0 * MIN;
        let a: Vec<f64> = (0..24).map(|i| now - (47.0 - 2.0 * i as f64) * MIN).collect();
        let b: Vec<f64> = (0..24).map(|i| now - (46.0 - 2.0 * i as f64) * MIN).collect();
        let tabs = [tab(1, now - 420.0 * MIN, &[now - 420.0 * MIN], now), tab(2, now - MIN, &a, now), tab(3, now, &b, now)];
        assert_eq!(ActivityClock::from_tabs(&tabs).idle_minutes(&tabs[0], now), 420);
        // Another tab used once inside the cut-off span adds nothing and takes nothing.
        let with_old = [tab(1, now - 420.0 * MIN, &[now - 420.0 * MIN], now), tab(2, now - MIN, &a, now), tab(3, now, &b, now),
            tab(4, now - 400.0 * MIN, &[now - 400.0 * MIN], now)];
        assert_eq!(ActivityClock::from_tabs(&with_old).idle_minutes(&with_old[0], now), 420);
        // A short history that was never cut off still skips time away.
        let short = [tab(1, now - 420.0 * MIN, &[now - 420.0 * MIN], now), tab(2, now - 400.0 * MIN, &[now - 400.0 * MIN], now)];
        assert_eq!(ActivityClock::from_tabs(&short).idle_minutes(&short[0], now), 20);
    }
}
