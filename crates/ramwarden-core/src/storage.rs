//! When disk space is worth interrupting the user for.
//!
//! # Why this exists
//!
//! A full system disk does not fail politely. On the reference PC the C:
//! drive reached 0.2 GB free (2026-11): builds failed with "not enough space
//! on the disk", updates could not unpack and the daemon could not write its
//! own log. Nothing had warned at 20 GB or at 5 GB, when clearing caches would
//! have taken a minute.
//!
//! # The rule
//!
//! Each volume is [`Level::Ok`], [`Level::Low`] or [`Level::Critical`]. A
//! level's line is the smaller of a gigabyte figure and a percent of the
//! volume ([`config::Storage`]), so it scales from a 64 GB laptop to a 4 TB
//! data disk.
//!
//! # Why it is rationed
//!
//! The alert is a sound and a bubble, the two most intrusive things the warden
//! does. [`Watch`] therefore speaks only when a volume gets **worse**, and
//! otherwise repeats a level no sooner than its repeat interval (a day for
//! low, four hours for critical). A volume hovering on the line would
//! otherwise alert every time a browser cache grew, so recovery needs 10%
//! more free space than the line itself before the level drops.

use std::collections::HashMap;
use std::path::PathBuf;

use ramwarden_platform::Volume;
use serde::Serialize;

use crate::config;

const GB: f64 = 1_073_741_824.0;
/// Free space must exceed a line by this factor before the level drops.
const RECOVERY: f64 = 1.1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Ok,
    Low,
    Critical,
}

impl Level {
    /// The level of a volume with `free` of `total` bytes. `previous` adds
    /// the recovery margin: a level only drops once free space is clearly
    /// past the line.
    pub fn of(cfg: &config::Storage, total: u64, free: u64, previous: Level) -> Level {
        let line = |gb: f64, percent: f64| (gb * GB).min(total as f64 * percent / 100.0);
        let free = free as f64;
        let under = |level: Level, at: f64| free < if previous >= level { at * RECOVERY } else { at };
        if under(Level::Critical, line(cfg.critical_free_gb, cfg.critical_free_percent)) {
            Level::Critical
        } else if under(Level::Low, line(cfg.low_free_gb, cfg.low_free_percent)) {
            Level::Low
        } else {
            Level::Ok
        }
    }
}

/// A volume and where it stands.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Reading {
    pub name: String,
    pub mount: PathBuf,
    pub total: u64,
    pub free: u64,
    pub system: bool,
    pub level: Level,
}

/// Something worth telling the user now.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Alert {
    pub volume: Reading,
    /// Seconds since the Unix epoch.
    pub at: u64,
}

#[derive(Clone, Copy)]
struct Seen {
    level: Level,
    alerted: Option<(Level, u64)>,
}

/// The watcher's memory: each volume's level and when it last alerted.
#[derive(Default)]
pub struct Watch {
    seen: HashMap<PathBuf, Seen>,
    readings: Vec<Reading>,
}

impl Watch {
    /// Take a fresh list of volumes. Returns at most one alert, for the
    /// worst volume that is due one (the system volume first on a tie).
    pub fn observe(&mut self, cfg: &config::Storage, volumes: &[Volume], unix: u64) -> Option<Alert> {
        let mut due: Vec<Reading> = Vec::new();
        self.readings.clear();
        for v in volumes {
            let seen = self.seen.entry(v.mount.clone()).or_insert(Seen { level: Level::Ok, alerted: None });
            let level = Level::of(cfg, v.total, v.free, seen.level);
            seen.level = level;
            let reading =
                Reading { name: v.name.clone(), mount: v.mount.clone(), total: v.total, free: v.free, system: v.system, level };
            if level == Level::Ok {
                // Recovered: the next dip is news again.
                seen.alerted = None;
            } else {
                // Eased from critical to low: remember the lower level, so
                // a return to critical is news again rather than waiting
                // out the 4-hour repeat. The recovery margin in
                // `Level::of` keeps this from flapping at the line.
                if let Some((at_level, at)) = seen.alerted
                    && level < at_level
                {
                    seen.alerted = Some((level, at));
                }
                let repeat = if level == Level::Critical { cfg.critical_repeat_hours } else { cfg.low_repeat_hours };
                let quiet = match seen.alerted {
                    None => false,
                    Some((at_level, _)) if level > at_level => false,
                    Some((_, at)) => (unix.saturating_sub(at) as f64) < repeat * 3600.0,
                };
                if !quiet {
                    due.push(reading.clone());
                }
            }
            self.readings.push(reading);
        }
        self.seen.retain(|mount, _| volumes.iter().any(|v| &v.mount == mount));
        let worst = due.into_iter().max_by(|a, b| a.level.cmp(&b.level).then(a.system.cmp(&b.system)).then(b.free.cmp(&a.free)))?;
        if !cfg.enabled {
            return None;
        }
        if let Some(seen) = self.seen.get_mut(&worst.mount) {
            seen.alerted = Some((worst.level, unix));
        }
        Some(Alert { volume: worst, at: unix })
    }

    /// The volumes as of the last [`Watch::observe`].
    pub fn readings(&self) -> &[Reading] {
        &self.readings
    }

    /// The worst level among them.
    pub fn level(&self) -> Level {
        self.readings.iter().map(|r| r.level).max().unwrap_or(Level::Ok)
    }
}

/// "0.2 GB", "18 GB", "1.4 TB", "350 MB".
pub fn size_text(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1024.0 * GB {
        format!("{:.1} TB", b / (1024.0 * GB))
    } else if b >= 10.0 * GB {
        format!("{:.0} GB", b / GB)
    } else if b >= 0.1 * GB {
        format!("{:.1} GB", b / GB)
    } else {
        format!("{:.0} MB", b / 1_048_576.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: u64 = 1 << 30;

    fn vol(name: &str, total_gb: u64, free_gb: f64, system: bool) -> Volume {
        Volume { mount: PathBuf::from(name), name: name.into(), total: total_gb * G, free: (free_gb * G as f64) as u64, system }
    }

    #[test]
    fn the_smaller_of_gigabytes_and_percent_decides() {
        let cfg = config::Storage::default();
        // 1 TB: the lines are 20 GB and 5 GB, not 10% and 3%.
        assert_eq!(Level::of(&cfg, 1000 * G, 100 * G, Level::Ok), Level::Ok);
        assert_eq!(Level::of(&cfg, 1000 * G, 19 * G, Level::Ok), Level::Low);
        assert_eq!(Level::of(&cfg, 1000 * G, 4 * G, Level::Ok), Level::Critical);
        // 64 GB: the lines are 6.4 GB and 1.92 GB.
        assert_eq!(Level::of(&cfg, 64 * G, 10 * G, Level::Ok), Level::Ok);
        assert_eq!(Level::of(&cfg, 64 * G, 6 * G, Level::Ok), Level::Low);
        assert_eq!(Level::of(&cfg, 64 * G, G, Level::Ok), Level::Critical);
    }

    #[test]
    fn recovery_needs_a_margin() {
        let cfg = config::Storage::default();
        assert_eq!(Level::of(&cfg, 1000 * G, 21 * G, Level::Ok), Level::Ok);
        assert_eq!(Level::of(&cfg, 1000 * G, 21 * G, Level::Low), Level::Low);
        assert_eq!(Level::of(&cfg, 1000 * G, 23 * G, Level::Low), Level::Ok);
        assert_eq!(Level::of(&cfg, 1000 * G, 5 * G + G / 4, Level::Critical), Level::Critical);
    }

    #[test]
    fn alerts_once_then_only_when_worse_or_after_the_repeat() {
        let cfg = config::Storage::default();
        let mut w = Watch::default();
        assert!(w.observe(&cfg, &[vol("C:", 930, 200.0, true)], 0).is_none());
        let a = w.observe(&cfg, &[vol("C:", 930, 15.0, true)], 60).unwrap();
        assert_eq!(a.volume.level, Level::Low);
        // Same level a minute and an hour later: quiet.
        assert!(w.observe(&cfg, &[vol("C:", 930, 14.0, true)], 120).is_none());
        assert!(w.observe(&cfg, &[vol("C:", 930, 14.0, true)], 3_660).is_none());
        // Worse: speaks at once, whatever the clock says.
        let a = w.observe(&cfg, &[vol("C:", 930, 0.2, true)], 3_720).unwrap();
        assert_eq!(a.volume.level, Level::Critical);
        assert!(w.observe(&cfg, &[vol("C:", 930, 0.2, true)], 3_780).is_none());
        // Still critical four hours on: one reminder.
        assert!(w.observe(&cfg, &[vol("C:", 930, 0.2, true)], 3_720 + 4 * 3600).is_some());
        // Back to low is better news, not an alert.
        assert!(w.observe(&cfg, &[vol("C:", 930, 12.0, true)], 3_720 + 4 * 3600 + 60).is_none());
        assert_eq!(w.level(), Level::Low);
    }

    #[test]
    fn critical_again_after_easing_to_low_is_news() {
        let cfg = config::Storage::default();
        let mut w = Watch::default();
        assert_eq!(w.observe(&cfg, &[vol("C:", 930, 0.2, true)], 0).unwrap().volume.level, Level::Critical);
        // Something freed a little: low, quiet.
        assert!(w.observe(&cfg, &[vol("C:", 930, 12.0, true)], 600).is_none());
        // Filled up again within the hour: speaks, not four hours later.
        assert_eq!(w.observe(&cfg, &[vol("C:", 930, 0.3, true)], 1_200).unwrap().volume.level, Level::Critical);
        // And the low it eased to is no new alert either, until its own repeat.
        assert!(w.observe(&cfg, &[vol("C:", 930, 12.0, true)], 1_800).is_none());
    }

    #[test]
    fn recovery_makes_the_next_dip_news_again() {
        let cfg = config::Storage::default();
        let mut w = Watch::default();
        assert!(w.observe(&cfg, &[vol("/", 500, 10.0, true)], 0).is_some());
        assert!(w.observe(&cfg, &[vol("/", 500, 80.0, true)], 60).is_none());
        assert!(w.observe(&cfg, &[vol("/", 500, 10.0, true)], 120).is_some());
    }

    #[test]
    fn one_alert_at_a_time_worst_volume_first() {
        let cfg = config::Storage::default();
        let mut w = Watch::default();
        let vols = [vol("D:", 2000, 10.0, false), vol("C:", 930, 0.2, true)];
        assert_eq!(w.observe(&cfg, &vols, 0).unwrap().volume.name, "C:");
        // The other volume is still owed its own warning.
        assert_eq!(w.observe(&cfg, &vols, 60).unwrap().volume.name, "D:");
        assert!(w.observe(&cfg, &vols, 120).is_none());
    }

    #[test]
    fn disabled_reads_but_never_alerts() {
        let cfg = config::Storage { enabled: false, ..Default::default() };
        let mut w = Watch::default();
        assert!(w.observe(&cfg, &[vol("C:", 930, 0.2, true)], 0).is_none());
        assert_eq!(w.level(), Level::Critical);
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(size_text(214_748_364), "0.2 GB");
        assert_eq!(size_text(48 * G), "48 GB");
        assert_eq!(size_text(50 * 1_048_576), "50 MB");
        assert_eq!(size_text(1500 * G), "1.5 TB");
    }
}
