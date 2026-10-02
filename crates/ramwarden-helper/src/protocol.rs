//! The helper's wire protocol: newline-delimited JSON over a unix socket.
//!
//! Deliberately tiny. This process holds `CAP_SYS_NICE`, so every byte of its
//! input surface is a liability — the protocol does one thing and validates
//! everything.

use serde::{Deserialize, Serialize};

/// The largest number of address ranges one request may carry.
///
/// `process_madvise` itself accepts `UIO_MAXIOV` (1024). Matching that bounds the
/// work a single request can ask for, so a malformed or hostile caller cannot
/// make the helper walk an unbounded list.
pub const MAX_RANGES: usize = 1024;

/// What to do with the pages.
///
/// Only these two. The syscall accepts other advice values, some of which are
/// destructive (`MADV_DONTNEED` discards anonymous pages outright, losing data);
/// the helper refuses to express them at all rather than validating them later.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Advice {
    /// Move the pages to the inactive list, to be reclaimed first under pressure.
    Cold,
    /// Write the pages out now and free the RAM.
    PageOut,
}

impl Advice {
    pub fn as_madvise(self) -> i32 {
        match self {
            Advice::Cold => ramwarden_kernel::madvise::COLD,
            Advice::PageOut => ramwarden_kernel::madvise::PAGEOUT,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Request {
    pub pid: i32,
    /// `[start, end)` byte ranges in the target's address space.
    pub ranges: Vec<(u64, u64)>,
    pub advice: Advice,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Response {
    Ok { bytes: u64 },
    Error { message: String },
}

impl Response {
    pub fn error(msg: impl Into<String>) -> Self {
        Response::Error {
            message: msg.into(),
        }
    }
}

/// Why a request was rejected before any syscall was made.
#[derive(Debug, PartialEq, Eq)]
pub enum Invalid {
    NoRanges,
    TooManyRanges(usize),
    /// A range whose end is not after its start, which would be a no-op at best
    /// and an arithmetic trap at worst.
    EmptyRange,
    /// pid 1 is init. Nothing good comes of paging out init.
    ForbiddenPid(i32),
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Invalid::NoRanges => write!(f, "no ranges given"),
            Invalid::TooManyRanges(n) => write!(f, "{n} ranges exceeds the limit of {MAX_RANGES}"),
            Invalid::EmptyRange => write!(f, "a range ends at or before it starts"),
            Invalid::ForbiddenPid(p) => write!(f, "pid {p} may not be targeted"),
        }
    }
}

impl Request {
    /// Check a request before it reaches the syscall.
    pub fn validate(&self) -> Result<(), Invalid> {
        if self.pid <= 1 {
            return Err(Invalid::ForbiddenPid(self.pid));
        }
        if self.ranges.is_empty() {
            return Err(Invalid::NoRanges);
        }
        if self.ranges.len() > MAX_RANGES {
            return Err(Invalid::TooManyRanges(self.ranges.len()));
        }
        if self.ranges.iter().any(|(s, e)| e <= s) {
            return Err(Invalid::EmptyRange);
        }
        Ok(())
    }

    /// Total bytes this request covers.
    pub fn span(&self) -> u64 {
        self.ranges.iter().map(|(s, e)| e.saturating_sub(*s)).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(pid: i32, ranges: Vec<(u64, u64)>) -> Request {
        Request {
            pid,
            ranges,
            advice: Advice::PageOut,
        }
    }

    #[test]
    fn a_well_formed_request_validates() {
        assert_eq!(req(1234, vec![(0x1000, 0x2000)]).validate(), Ok(()));
    }

    /// Nothing good comes of paging out init, and pid 0 is not a process.
    #[test]
    fn init_and_nonsense_pids_are_refused() {
        assert_eq!(
            req(1, vec![(0x1000, 0x2000)]).validate(),
            Err(Invalid::ForbiddenPid(1))
        );
        assert_eq!(
            req(0, vec![(0x1000, 0x2000)]).validate(),
            Err(Invalid::ForbiddenPid(0))
        );
        assert_eq!(
            req(-5, vec![(0x1000, 0x2000)]).validate(),
            Err(Invalid::ForbiddenPid(-5))
        );
    }

    #[test]
    fn an_empty_or_inverted_range_is_refused() {
        assert_eq!(req(100, vec![]).validate(), Err(Invalid::NoRanges));
        assert_eq!(
            req(100, vec![(0x2000, 0x2000)]).validate(),
            Err(Invalid::EmptyRange)
        );
        assert_eq!(
            req(100, vec![(0x3000, 0x1000)]).validate(),
            Err(Invalid::EmptyRange)
        );
    }

    #[test]
    fn an_oversized_request_is_refused_before_any_syscall() {
        let ranges: Vec<(u64, u64)> = (0..MAX_RANGES as u64 + 1)
            .map(|i| (i * 0x2000, i * 0x2000 + 0x1000))
            .collect();
        assert_eq!(
            req(100, ranges).validate(),
            Err(Invalid::TooManyRanges(MAX_RANGES + 1))
        );
    }

    #[test]
    fn exactly_the_limit_is_allowed() {
        let ranges: Vec<(u64, u64)> = (0..MAX_RANGES as u64)
            .map(|i| (i * 0x2000, i * 0x2000 + 0x1000))
            .collect();
        assert_eq!(req(100, ranges).validate(), Ok(()));
    }

    #[test]
    fn span_sums_the_ranges_without_overflowing() {
        let r = req(100, vec![(0, 100), (1000, 1500)]);
        assert_eq!(r.span(), 600);
        // A range at the top of the address space must not wrap.
        let r = req(100, vec![(u64::MAX - 10, u64::MAX)]);
        assert_eq!(r.span(), 10);
    }

    /// The destructive advice values must not be expressible. `MADV_DONTNEED`
    /// discards anonymous pages outright — that is data loss, not reclaim.
    #[test]
    fn only_the_two_non_destructive_advices_exist() {
        assert_eq!(Advice::Cold.as_madvise(), 20);
        assert_eq!(Advice::PageOut.as_madvise(), 21);
        assert!(serde_json::from_str::<Advice>(r#""dontneed""#).is_err());
        assert!(serde_json::from_str::<Advice>(r#""free""#).is_err());
        assert!(serde_json::from_str::<Advice>(r#""remove""#).is_err());
    }

    #[test]
    fn requests_round_trip_as_json() {
        let r = Request {
            pid: 4242,
            ranges: vec![(0x1000, 0x5000)],
            advice: Advice::Cold,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), r);
        assert!(json.contains(r#""advice":"cold""#), "{json}");
    }

    #[test]
    fn responses_round_trip_as_json() {
        let ok = serde_json::to_string(&Response::Ok { bytes: 500 }).unwrap();
        assert_eq!(ok, r#"{"status":"ok","bytes":500}"#);
        let err = serde_json::to_string(&Response::error("nope")).unwrap();
        assert_eq!(err, r#"{"status":"error","message":"nope"}"#);
    }

    #[test]
    fn a_malformed_request_does_not_deserialise() {
        assert!(serde_json::from_str::<Request>("{}").is_err());
        assert!(serde_json::from_str::<Request>(r#"{"pid":1}"#).is_err());
        assert!(serde_json::from_str::<Request>("not json").is_err());
    }
}
