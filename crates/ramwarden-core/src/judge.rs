//! The second opinion the ladder asks before suspending or killing anything.
//!
//! The detector and the watchlist decide what *may* be reclaimed; the LRU decides
//! the order to consider it in, coldest first. That still leaves the question no
//! signal answers directly: *will the user be back for this soon, and is it
//! something that must not be closed?* A [`Judge`] answers that for a short list
//! of candidates from the LRU tail. The daemon implements it with Jev (see
//! `ramwarden_ai::jev`); the ladder only sees this trait, so it stays free of
//! network code.
//!
//! A judge can veto, never widen: it is only ever shown candidates that already
//! passed the gate, so a confident model still cannot reach a protected process.

use crate::signals::Signals;

/// The action a judgement is for. Killing loses unsaved work, so a judge should
/// hold it to a higher bar than suspending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    Suspend,
    Kill,
}

impl Intent {
    pub fn as_str(self) -> &'static str {
        match self {
            Intent::Suspend => "suspend",
            Intent::Kill => "close",
        }
    }
}

/// One application up for judgement. Numbers and short categories only — no
/// window titles or URLs leave the machine through this.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Candidate {
    pub name: String,
    /// 0 is the coldest entry in the LRU.
    pub cold_rank: usize,
    /// Minutes since the user last focused it, played audio from it or launched
    /// it. At least this long, for an app that was already running at startup.
    pub idle_minutes: f64,
    pub pss: u64,
    pub role: String,
    pub has_window: bool,
    pub established: u32,
    pub listening: bool,
    pub reasons: Vec<String>,
}

impl Candidate {
    /// Build from the processes sharing `name`, summing their memory.
    pub fn from_signals(name: &str, cold_rank: usize, idle_minutes: f64, procs: &[&Signals]) -> Self {
        let first = procs.first();
        Candidate {
            name: name.to_string(),
            cold_rank,
            idle_minutes,
            pss: procs.iter().map(|s| s.pss).sum(),
            role: first.map(|s| s.role.as_str().to_string()).unwrap_or_default(),
            has_window: procs.iter().any(|s| s.has_window),
            established: procs.iter().map(|s| s.established).sum(),
            listening: procs.iter().any(|s| !s.listening_ports.is_empty()),
            reasons: first.map(|s| s.reasons.clone()).unwrap_or_default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Judgement {
    pub name: String,
    pub close: bool,
    /// How sure the judge is that closing is safe, 0..=1.
    pub confidence: f64,
    /// Shown in the log and the UI, e.g. "Jev is not convinced (61%)".
    pub reason: String,
}

pub trait Judge {
    /// Judge every candidate. An `Err` means the judge could not be reached; the
    /// ladder then falls back to the gate alone, as it did before judges existed.
    fn judge(&mut self, intent: Intent, candidates: &[Candidate]) -> Result<Vec<Judgement>, String>;
}
