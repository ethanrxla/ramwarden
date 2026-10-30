//! Least-squares forecast of when available memory hits a floor.
//!
//! Pure arithmetic over a sliding window of `(seconds, available_bytes)`
//! samples: closed-form simple linear regression, no training, no state beyond
//! the window. Deterministic, so the tests pin exact answers.

use std::collections::VecDeque;

/// Samples older than this are dropped.
const WINDOW_SECONDS: f64 = 60.0;
/// Fewer samples, or a shorter span, than this is too little to fit a line.
const MIN_SAMPLES: usize = 6;
const MIN_SPAN_SECONDS: f64 = 15.0;
/// A trend that explains less than this share of the variance is noise.
const MIN_R2: f64 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    /// Bytes per second; negative while memory is draining.
    pub slope: f64,
    /// Fitted value at the newest sample, which is steadier than the raw reading.
    pub level: f64,
    pub r2: f64,
}

#[derive(Default)]
pub struct Forecaster {
    window: VecDeque<(f64, f64)>,
}

impl Forecaster {
    pub fn push(&mut self, t: f64, available: f64) {
        self.window.push_back((t, available));
        while self.window.front().is_some_and(|&(t0, _)| t - t0 > WINDOW_SECONDS) {
            self.window.pop_front();
        }
    }

    /// Ordinary least squares over the window, or `None` when it is too thin.
    pub fn fit(&self) -> Option<Fit> {
        let n = self.window.len();
        let (first, last) = (self.window.front()?.0, self.window.back()?.0);
        if n < MIN_SAMPLES || last - first < MIN_SPAN_SECONDS {
            return None;
        }
        // Centre time on the newest sample: keeps the sums small and makes the
        // intercept the fitted level *now*.
        let nf = n as f64;
        let (mut st, mut sx, mut stt, mut stx) = (0.0, 0.0, 0.0, 0.0);
        for &(t, x) in &self.window {
            let t = t - last;
            st += t;
            sx += x;
            stt += t * t;
            stx += t * x;
        }
        let denom = nf * stt - st * st;
        if denom.abs() < f64::EPSILON {
            return None;
        }
        let slope = (nf * stx - st * sx) / denom;
        let level = (sx - slope * st) / nf;

        let mean = sx / nf;
        let (mut ss_tot, mut ss_res) = (0.0, 0.0);
        for &(t, x) in &self.window {
            let pred = level + slope * (t - last);
            ss_tot += (x - mean).powi(2);
            ss_res += (x - pred).powi(2);
        }
        let r2 = if ss_tot > 0.0 { 1.0 - ss_res / ss_tot } else { 0.0 };
        Some(Fit { slope, level, r2 })
    }

    /// Seconds until the trend crosses `floor`, if memory is draining on a
    /// trend that is not noise. `Some(0.0)` means already at or below it.
    pub fn seconds_until(&self, floor: f64) -> Option<f64> {
        let fit = self.fit()?;
        if fit.level <= floor {
            return Some(0.0);
        }
        (fit.slope < 0.0 && fit.r2 >= MIN_R2).then(|| (fit.level - floor) / -fit.slope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(f: &mut Forecaster, start: f64, per_sec: f64, secs: usize) {
        for s in 0..secs {
            f.push(s as f64, start + per_sec * s as f64);
        }
    }

    #[test]
    fn recovers_an_exact_line() {
        let mut f = Forecaster::default();
        line(&mut f, 4_000.0, -10.0, 31);
        let fit = f.fit().unwrap();
        assert!((fit.slope + 10.0).abs() < 1e-9);
        assert!((fit.level - 3_700.0).abs() < 1e-9);
        assert!((fit.r2 - 1.0).abs() < 1e-9);
        // 3700 now, losing 10/s: reaches 1000 in 270 s.
        assert!((f.seconds_until(1_000.0).unwrap() - 270.0).abs() < 1e-6);
    }

    #[test]
    fn rising_or_flat_memory_never_crosses() {
        let mut f = Forecaster::default();
        line(&mut f, 1_000.0, 5.0, 31);
        assert_eq!(f.seconds_until(500.0), None);
        let mut g = Forecaster::default();
        line(&mut g, 1_000.0, 0.0, 31);
        assert_eq!(g.seconds_until(500.0), None);
    }

    #[test]
    fn already_below_the_floor_is_zero() {
        let mut f = Forecaster::default();
        line(&mut f, 400.0, -1.0, 31);
        assert_eq!(f.seconds_until(1_000.0), Some(0.0));
    }

    #[test]
    fn too_little_data_gives_no_forecast() {
        let mut f = Forecaster::default();
        line(&mut f, 4_000.0, -10.0, 5);
        assert!(f.fit().is_none(), "fewer than MIN_SAMPLES");
        let mut g = Forecaster::default();
        for s in 0..8 {
            g.push(s as f64, 4_000.0 - 10.0 * s as f64);
        }
        assert!(g.fit().is_none(), "span under MIN_SPAN_SECONDS");
    }

    #[test]
    fn noise_is_not_a_trend() {
        let mut f = Forecaster::default();
        for s in 0..31 {
            // Zig-zag with a faint downward lean: slope < 0 but r² is tiny.
            let x = 4_000.0 + if s % 2 == 0 { 300.0 } else { -300.0 } - 0.2 * s as f64;
            f.push(s as f64, x);
        }
        assert!(f.fit().unwrap().r2 < MIN_R2);
        assert_eq!(f.seconds_until(1_000.0), None);
    }

    #[test]
    fn old_samples_age_out() {
        let mut f = Forecaster::default();
        line(&mut f, 9_000.0, -50.0, 30);
        for s in 200..231 {
            f.push(s as f64, 2_000.0);
        }
        let fit = f.fit().unwrap();
        assert!(fit.slope.abs() < 1e-9, "only the flat recent window remains");
    }
}
