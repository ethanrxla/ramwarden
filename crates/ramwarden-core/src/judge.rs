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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

/// Answers a judge gave earlier, served to the ladder as if it were asking now.
///
/// The daemon asks Jev with no lock held (see
/// [`Ladder::judge_requests`](crate::ladder::Ladder::judge_requests)), records
/// the answers here, then runs the step under its locks with this as the judge.
/// An intent that was never asked about answers with no verdicts, so every
/// candidate is spared: the step can only ever act on what the judge actually
/// approved. A recorded failure is replayed as the same failure, so an
/// unreachable judge still blocks kills and leaves suspends to the gate.
#[derive(Debug, Default)]
pub struct Prejudged {
    answers: Vec<(Intent, Result<Vec<Judgement>, String>)>,
}

impl Prejudged {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record what the judge said about `intent`.
    pub fn record(&mut self, intent: Intent, answer: Result<Vec<Judgement>, String>) {
        self.answers.retain(|(i, _)| *i != intent);
        self.answers.push((intent, answer));
    }

    /// Ask `judge` every question in `requests` and record the answers.
    pub fn ask(judge: &mut dyn Judge, requests: &[(Intent, Vec<Candidate>)]) -> Self {
        let mut out = Self::new();
        for (intent, candidates) in requests {
            out.record(*intent, judge.judge(*intent, candidates));
        }
        out
    }
}

impl Judge for Prejudged {
    fn judge(&mut self, intent: Intent, candidates: &[Candidate]) -> Result<Vec<Judgement>, String> {
        match self.answers.iter().find(|(i, _)| *i == intent) {
            Some((_, Ok(all))) => Ok(all
                .iter()
                .filter(|j| candidates.iter().any(|c| c.name == j.name))
                .cloned()
                .collect()),
            Some((_, Err(e))) => Err(e.clone()),
            None => Ok(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(name: &str) -> Candidate {
        Candidate::from_signals(name, 0, 60.0, &[])
    }

    fn approve(name: &str) -> Judgement {
        Judgement { name: name.into(), close: true, confidence: 0.9, reason: "ok".into() }
    }

    #[test]
    fn replays_only_answers_for_candidates_still_on_the_list() {
        let mut p = Prejudged::new();
        p.record(Intent::Suspend, Ok(vec![approve("slack"), approve("zoom")]));
        let got = p.judge(Intent::Suspend, &[cand("slack"), cand("discord")]).unwrap();
        assert_eq!(got, vec![approve("slack")]);
    }

    #[test]
    fn an_intent_never_asked_about_approves_nothing() {
        let mut p = Prejudged::new();
        p.record(Intent::Suspend, Ok(vec![approve("slack")]));
        assert!(p.judge(Intent::Kill, &[cand("slack")]).unwrap().is_empty());
    }

    #[test]
    fn a_failure_is_replayed_as_a_failure() {
        let mut p = Prejudged::new();
        p.record(Intent::Kill, Err("timed out".into()));
        assert_eq!(p.judge(Intent::Kill, &[cand("slack")]), Err("timed out".into()));
    }
}
