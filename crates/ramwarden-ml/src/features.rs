//! Turning behaviour-log rows into fixed-width vectors, and rows into windows.
//!
//! The log (`ramwarden_core::history`) records numbers and short categories
//! only — never titles or URLs — so every feature here is behaviour: size,
//! activity, connections, the time of day, and the machine's pressure.

use std::collections::HashMap;

use ramwarden_core::history::{LabelledSample, SignalSample};
use ramwarden_core::signals::CPU_BUSY_SECONDS;

/// Behaviour features per sample, before the app-identity buckets.
const BEHAVIOUR: usize = 14;

/// App identity, hashed into this many one-hot buckets. Without it the model
/// could learn "something comes back at 9" but not *which* app does; with it,
/// each app's routine is learnable. Names are already in the log, so this
/// records nothing new.
pub const APP_BUCKETS: usize = 8;

/// Features per sample.
pub const FEATURES: usize = BEHAVIOUR + APP_BUCKETS;

/// A stable bucket for an app name (FNV-1a, case-insensitive).
pub fn app_bucket(name: &str) -> usize {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.to_lowercase().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (h % APP_BUCKETS as u64) as usize
}

/// Samples per context window. At the default 5-minute logging interval,
/// four samples is the last 20 minutes of a process's life.
pub const WINDOW: usize = 4;

/// One sample as a feature vector. Every value is roughly in 0..=1.
pub fn encode(s: &SignalSample, epoch: i64, psi_some: f64) -> [f32; FEATURES] {
    let mb = s.pss.max(0) as f64 / 1_048_576.0;
    let hour = ((epoch.rem_euclid(86_400)) as f64 / 3600.0) * std::f64::consts::TAU / 24.0;
    let verdict = |v: &str| (s.verdict == v) as u8 as f32;
    let mut out = [0f32; FEATURES];
    out[BEHAVIOUR + app_bucket(&s.name)] = 1.0;
    let behaviour = [
        ((1.0 + mb).ln() / 10.0) as f32,
        (s.cpu_recent / (4.0 * CPU_BUSY_SECONDS)).min(1.0) as f32,
        ((1.0 + s.age_minutes.max(0.0)).ln() / 10.0) as f32,
        (s.established > 0) as u8 as f32,
        (s.listening > 0) as u8 as f32,
        s.focused as u8 as f32,
        s.windowed as u8 as f32,
        s.audio as u8 as f32,
        s.descendant as u8 as f32,
        verdict("IDLE"),
        verdict("IN_USE"),
        (hour.sin() * 0.5 + 0.5) as f32,
        (hour.cos() * 0.5 + 0.5) as f32,
        (psi_some / 100.0).min(1.0) as f32,
    ];
    out[..BEHAVIOUR].copy_from_slice(&behaviour);
    out
}

/// Whether a sample shows the user actively using the process.
pub fn active(s: &SignalSample) -> bool {
    s.focused || s.cpu_recent >= CPU_BUSY_SECONDS
}

/// A training example: a context window, the window one step later (the
/// JEPA target), and whether the user came back.
#[derive(Clone, Debug)]
pub struct Example {
    pub epoch: i64,
    /// `WINDOW * FEATURES` values, oldest sample first.
    pub context: Vec<f32>,
    /// The same process's window shifted one sample forward.
    pub target: Vec<f32>,
    pub returned: bool,
    /// LRU baseline score: how recently the process was active within the
    /// context window (higher = more recent). The bar the model must clear.
    pub lru: f32,
}

/// Build examples from labelled rows: per process instance, every run of
/// `WINDOW + 1` consecutive samples gives one example.
pub fn examples(rows: &[LabelledSample]) -> Vec<Example> {
    let mut by_proc: HashMap<(i32, &str), Vec<&LabelledSample>> = HashMap::new();
    for r in rows {
        by_proc.entry((r.sample.pid, r.sample.name.as_str())).or_default().push(r);
    }
    let mut out = Vec::new();
    for (_, mut seq) in by_proc {
        seq.sort_by_key(|r| r.epoch);
        let enc: Vec<[f32; FEATURES]> =
            seq.iter().map(|r| encode(&r.sample, r.epoch, r.context.psi_some)).collect();
        for i in 0..seq.len().saturating_sub(WINDOW) {
            let ctx = &enc[i..i + WINDOW];
            let tgt = &enc[i + 1..i + 1 + WINDOW];
            let last = seq[i + WINDOW - 1];
            let lru = (0..WINDOW)
                .rev()
                .find(|j| active(&seq[i + j].sample))
                .map(|j| (j + 1) as f32 / WINDOW as f32)
                .unwrap_or(0.0);
            out.push(Example {
                epoch: last.epoch,
                context: ctx.iter().flatten().copied().collect(),
                target: tgt.iter().flatten().copied().collect(),
                returned: last.returned,
                lru,
            });
        }
    }
    out.sort_by_key(|e| e.epoch);
    out
}

/// Area under the ROC curve: the probability a random positive outranks a
/// random negative. 0.5 is chance. `None` when one class is absent.
pub fn auc(scores: &[f32], labels: &[bool]) -> Option<f64> {
    let mut idx: Vec<usize> = (0..scores.len()).collect();
    idx.sort_by(|a, b| scores[*a].total_cmp(&scores[*b]));
    let (mut rank_sum, mut pos, mut neg) = (0.0f64, 0usize, 0usize);
    let mut i = 0;
    while i < idx.len() {
        // Ties share their average rank.
        let mut j = i;
        while j + 1 < idx.len() && scores[idx[j + 1]] == scores[idx[i]] {
            j += 1;
        }
        let avg = (i + j) as f64 / 2.0 + 1.0;
        for &k in &idx[i..=j] {
            if labels[k] {
                rank_sum += avg;
                pos += 1;
            } else {
                neg += 1;
            }
        }
        i = j + 1;
    }
    if pos == 0 || neg == 0 {
        return None;
    }
    Some((rank_sum - (pos * (pos + 1)) as f64 / 2.0) / (pos * neg) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramwarden_core::history::SampleContext;

    pub(crate) fn row(pid: i32, epoch: i64, focused: bool, returned: bool) -> LabelledSample {
        LabelledSample {
            epoch,
            sample: SignalSample {
                pid,
                name: format!("app{pid}"),
                pss: 300 * 1_048_576,
                age_minutes: 120.0,
                focused,
                verdict: if focused { "IN_USE".into() } else { "IDLE".into() },
                ..Default::default()
            },
            context: SampleContext::default(),
            returned,
        }
    }

    #[test]
    fn auc_matches_known_cases() {
        assert_eq!(auc(&[0.1, 0.9], &[false, true]), Some(1.0));
        assert_eq!(auc(&[0.9, 0.1], &[false, true]), Some(0.0));
        assert_eq!(auc(&[0.5, 0.5], &[false, true]), Some(0.5));
        assert_eq!(auc(&[0.1, 0.2], &[true, true]), None);
    }

    #[test]
    fn windows_are_built_per_process_in_time_order() {
        let mut rows = Vec::new();
        for t in 0..6 {
            rows.push(row(1, 1000 + t * 300, t == 3, t == 4));
        }
        rows.push(row(2, 1000, false, false)); // too short for a window
        let ex = examples(&rows);
        assert_eq!(ex.len(), 2, "6 samples give 2 windows of 4 + 1");
        assert_eq!(ex[0].context.len(), WINDOW * FEATURES);
        assert!(ex[0].epoch < ex[1].epoch);
        // In the second window (samples 1..=4) the active sample is index 2 of 4.
        assert!((ex[1].lru - 0.75).abs() < 1e-6, "{}", ex[1].lru);
    }

    #[test]
    fn features_stay_in_range() {
        let r = row(1, 1_700_000_000, true, false);
        for v in encode(&r.sample, r.epoch, 250.0) {
            assert!((0.0..=1.0).contains(&v), "{v}");
        }
    }
}
