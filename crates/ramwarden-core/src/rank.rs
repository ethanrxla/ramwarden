//! An optional opinion on what the user will come back to.
//!
//! Policy never depends on one: with no ranker the ladder and Tidy behave
//! exactly as without this module. A ranker (in practice the JEPA predictor
//! in `ramwarden-ml`, wired in by the daemon only once it beats the LRU
//! baseline) may *reorder* candidates and *defer* a trim. It can never make
//! anything eligible that the rules did not already allow — protection,
//! the watchlist and the warm-up gate are all checked before it is asked.

/// Probability that the user returns to a process soon.
pub trait ReuseRanker: Send + Sync {
    /// `None` when the ranker has no opinion about this process.
    fn reuse(&self, pid: i32) -> Option<f64>;
}

/// Above this, Tidy leaves a process's working set alone: it would only be
/// faulted straight back in.
pub const DEFER_TRIM_ABOVE: f64 = 0.7;
