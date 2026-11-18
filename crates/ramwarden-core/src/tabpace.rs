//! How long *this* user leaves a tab before coming back to it, learned from
//! what they actually do, so the close floor fits the person instead of a
//! fixed number.
//!
//! # Why
//!
//! A fixed floor is wrong for everyone in a different way. Until 2026-11-18 it
//! was a whole day, which suits nobody who opens thirty tabs in a session: the
//! warden unloaded them and never closed them. A short fixed floor closed tabs
//! the user was still coming back to (the 2026-11-02 Oracle, Agensi and
//! Namecheap closures). What separates "left" from "still in use" is the
//! person's own rhythm, and the warden already sees it:
//!
//! * **Return gaps.** Every time a tab is selected again, the time since it was
//!   last left is a gap the user *did* come back after. The extension reports
//!   each tab's recent activations, and `tab_outcomes` records how long after
//!   an unload the user returned. Gaps under five minutes are tab switching,
//!   not leaving, and are skipped.
//! * **Reopened closes.** A close the user undoes says the floor was too short;
//!   closes nobody reopens say it could be shorter.
//!
//! The floor is the gap within which the user makes nine in ten of their
//! returns: a tab idle longer than that is past the point where this person
//! usually comes back. Reopens then nudge it up or down. Everything else that
//! spares a tab in use (recent tabs, work pages, forms, keeps, the focus list,
//! the return chance, Atlas recall) still applies on top: the floor only says
//! when a tab may be *considered*.
//!
//! Too little evidence means the configured `close_after_hours` is used, so a
//! new install behaves exactly as configured ("degrade, don't fail").

/// Returns shorter than this are switching between tabs, not leaving one.
pub const MIN_GAP_MINUTES: i64 = 5;
/// Fewer return gaps than this, and the configured floor is used.
pub const MIN_SAMPLES: usize = 20;
/// The share of returns the floor covers.
pub const QUANTILE: f64 = 0.9;
/// Never closer than an hour to the last use, never further than a day.
pub const FLOOR_MIN_MINUTES: i64 = 60;
pub const FLOOR_MAX_MINUTES: i64 = 24 * 60;
/// Reopen correction: above this share of closes reopened, wait longer...
pub const REOPEN_HIGH: f64 = 0.2;
/// ...below this (with enough closes), close sooner.
pub const REOPEN_LOW: f64 = 0.05;
pub const MIN_CLOSES: i64 = 10;

/// What the warden has seen of the user's rhythm.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Evidence {
    /// Minutes away before the user came back to a tab.
    pub return_gaps: Vec<i64>,
    /// Automatic closes in the window, and how many the user reopened.
    pub closes: i64,
    pub reopened: i64,
}

/// The floor to use now, and why.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Pace {
    pub floor_minutes: i64,
    /// False when there was too little evidence and the setting was used.
    pub learned: bool,
    pub samples: usize,
    pub closes: i64,
    pub reopened: i64,
    pub why: String,
}

impl Evidence {
    /// Return gaps from one browser's tabs, each given as its activation
    /// times (JavaScript milliseconds, any order). Activations at or before
    /// `since_ms` are ignored, so an old tab's history cannot stand in for the
    /// user's rhythm now.
    ///
    /// Activations are selections, not departures, so the gap between two
    /// selections of the same tab also holds the time spent reading it. Time
    /// away is measured from when the user left: the first activation of any
    /// *other* tab after this one. A tab re-selected with no other tab in
    /// between was never left and adds nothing. Switching back within
    /// `MIN_GAP_MINUTES` is not counted. Windows are not told apart, so a tab
    /// visible in a second window can read as left; that errs toward a longer
    /// gap, which only delays a close.
    pub fn add_browser(&mut self, tabs: &[&[f64]], since_ms: f64) {
        let mut timeline: Vec<(f64, usize)> = Vec::new();
        for (tab, times) in tabs.iter().enumerate() {
            timeline.extend(times.iter().filter(|t| t.is_finite() && **t > since_ms).map(|t| (*t, tab)));
        }
        timeline.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut left: Vec<Option<f64>> = vec![None; tabs.len()];
        let mut current: Option<usize> = None;
        for &(at, tab) in &timeline {
            if let Some(prev) = current && prev != tab && left[prev].is_none() {
                left[prev] = Some(at);
            }
            if current != Some(tab) && let Some(gone) = left[tab].take() {
                let minutes = ((at - gone) / 60_000.0) as i64;
                if minutes >= MIN_GAP_MINUTES { self.return_gaps.push(minutes); }
            }
            current = Some(tab);
        }
    }

    /// The floor for this user, or `configured_minutes` without enough evidence.
    pub fn pace(&self, configured_minutes: i64) -> Pace {
        let mut gaps: Vec<i64> = self.return_gaps.iter().copied().filter(|g| *g >= MIN_GAP_MINUTES).collect();
        let samples = gaps.len();
        let (closes, reopened) = (self.closes, self.reopened);
        if samples < MIN_SAMPLES {
            return Pace {
                floor_minutes: configured_minutes.max(FLOOR_MIN_MINUTES),
                learned: false, samples, closes, reopened,
                why: format!("still learning ({samples} of {MIN_SAMPLES} returns seen); using the setting"),
            };
        }
        gaps.sort_unstable();
        let at = ((samples as f64 * QUANTILE).ceil() as usize).clamp(1, samples) - 1;
        let usual = gaps[at];
        let share = if closes > 0 { reopened as f64 / closes as f64 } else { 0.0 };
        let (factor, nudge) = if closes > 0 && share > REOPEN_HIGH {
            (1.5, format!("; waiting longer because {reopened} of {closes} closes were reopened"))
        } else if closes >= MIN_CLOSES && share < REOPEN_LOW {
            (0.8, format!("; a little sooner because only {reopened} of {closes} closes were reopened"))
        } else {
            (1.0, String::new())
        };
        let floor = ((usual as f64 * factor).round() as i64).clamp(FLOOR_MIN_MINUTES, FLOOR_MAX_MINUTES);
        Pace {
            floor_minutes: floor, learned: true, samples, closes, reopened,
            why: format!("you come back to 9 in 10 tabs within {}{nudge}", minutes_text(usual)),
        }
    }
}

fn minutes_text(m: i64) -> String {
    if m < 90 { format!("{m} min") } else { format!("{:.1} h", m as f64 / 60.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gaps(v: &[i64]) -> Evidence { Evidence { return_gaps: v.to_vec(), ..Default::default() } }

    #[test]
    fn without_enough_returns_the_setting_is_used() {
        let p = gaps(&[30; 10]).pace(240);
        assert!(!p.learned);
        assert_eq!(p.floor_minutes, 240);
    }

    #[test]
    fn a_fast_session_user_gets_a_short_floor_and_a_slow_one_a_long_one() {
        // Comes back within 10-80 minutes: tabs left two hours are done with.
        let quick: Vec<i64> = (0..40).map(|i| 10 + i * 2).collect();
        let p = gaps(&quick).pace(240);
        assert!(p.learned);
        assert_eq!(p.floor_minutes, 80);
        // Comes back after half a day: the floor stretches, capped at a day.
        let slow: Vec<i64> = (0..40).map(|i| 300 + i * 30).collect();
        assert_eq!(gaps(&slow).pace(240).floor_minutes, 1350);
        assert_eq!(gaps(&[5000; 30]).pace(240).floor_minutes, FLOOR_MAX_MINUTES);
        assert_eq!(gaps(&[6; 30]).pace(240).floor_minutes, FLOOR_MIN_MINUTES);
    }

    #[test]
    fn reopened_closes_lengthen_the_floor_and_unregretted_ones_shorten_it() {
        let base: Vec<i64> = vec![100; 30];
        let regret = Evidence { return_gaps: base.clone(), closes: 10, reopened: 4 };
        assert_eq!(regret.pace(240).floor_minutes, 150);
        let fine = Evidence { return_gaps: base.clone(), closes: 40, reopened: 1 };
        assert_eq!(fine.pace(240).floor_minutes, 80);
        let few = Evidence { return_gaps: base, closes: 3, reopened: 0 };
        assert_eq!(few.pace(240).floor_minutes, 100);
    }

    #[test]
    fn time_away_runs_from_leaving_a_tab_not_from_selecting_it() {
        let m = 60_000.0;
        let mut e = Evidence::default();
        // A selected at 0, read until B at 480 (8 h), back to A at 540: one hour
        // away, not nine. B, left at 540, is back at 603: 63 minutes. C, left at
        // 603 and back at 606, was a 3-minute switch and does not count.
        let a = [0.0, 540.0 * m];
        let b = [480.0 * m, 603.0 * m];
        let c = [600.0 * m, 606.0 * m];
        e.add_browser(&[&a, &b, &c], -1.0);
        assert_eq!(e.return_gaps, vec![60, 63]);
    }

    #[test]
    fn visits_before_the_window_are_ignored() {
        let m = 60_000.0;
        let mut e = Evidence::default();
        let a: Vec<f64> = (0..21).map(|i| i as f64 * 20.0 * m).collect();
        let b: Vec<f64> = (0..21).map(|i| (i as f64 * 20.0 + 10.0) * m).collect();
        e.add_browser(&[&a, &b], 1_000_000.0 * m);
        assert!(e.return_gaps.is_empty());
        e.add_browser(&[&a, &b], -1.0);
        assert_eq!(e.return_gaps.len(), 40);
        assert!(e.return_gaps.iter().all(|g| *g == 10));
    }
}
