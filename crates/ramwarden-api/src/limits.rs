//! Per-token quotas and the global spend cap, in memory.
//!
//! Phase 1 runs one instance, so a mutex is the whole store; a restart forgets
//! today's counts, which errs towards the user. Accounts move the daily count to
//! the database (`usage_daily`) so it survives restarts and spans instances.
//!
//! A request takes its slot before the upstream call, and keeps it if the call
//! fails: a daemon retrying into an outage should run out of quota, not multiply
//! the outage.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::config::TokenHash;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Too many requests this minute from this token.
    Minute,
    /// This token's daily quota is spent.
    Day,
    /// The server's daily upstream budget is spent, for everyone.
    Budget,
}

impl Refusal {
    pub fn message(self) -> &'static str {
        match self {
            Refusal::Minute => "too many requests this minute",
            Refusal::Day => "daily quota used up; RamWarden falls back to its local estimate until tomorrow (UTC)",
            Refusal::Budget => "hosted decisions are paused for today; RamWarden falls back to its local estimate",
        }
    }
}

#[derive(Default)]
struct Usage {
    minute: u64,
    in_minute: u32,
    in_day: u32,
}

#[derive(Default)]
struct Inner {
    day: u64,
    input_tokens: u64,
    tokens: HashMap<TokenHash, Usage>,
}

pub struct Limits {
    per_minute: u32,
    per_day: u32,
    daily_input_tokens: u64,
    inner: Mutex<Inner>,
}

impl Limits {
    pub fn new(per_minute: u32, per_day: u32, daily_input_tokens: u64) -> Self {
        Limits { per_minute, per_day, daily_input_tokens, inner: Mutex::new(Inner::default()) }
    }

    /// Take one request slot for `token` at `now` (Unix seconds). On success,
    /// returns how many requests the token has left today.
    pub fn take(&self, token: &TokenHash, now: u64) -> Result<u32, Refusal> {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        roll_day(&mut g, now);
        if g.input_tokens >= self.daily_input_tokens {
            return Err(Refusal::Budget);
        }
        let u = g.tokens.entry(*token).or_default();
        let minute = now / 60;
        if u.minute != minute {
            u.minute = minute;
            u.in_minute = 0;
        }
        if u.in_day >= self.per_day {
            return Err(Refusal::Day);
        }
        if u.in_minute >= self.per_minute {
            return Err(Refusal::Minute);
        }
        u.in_minute += 1;
        u.in_day += 1;
        Ok(self.per_day - u.in_day)
    }

    /// Count upstream input tokens against today's budget.
    pub fn spend(&self, input_tokens: u64, now: u64) {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        roll_day(&mut g, now);
        g.input_tokens = g.input_tokens.saturating_add(input_tokens);
    }
}

fn roll_day(g: &mut Inner, now: u64) {
    let day = now / 86_400;
    if g.day != day {
        g.day = day;
        g.input_tokens = 0;
        g.tokens.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: TokenHash = [1; 32];
    const B: TokenHash = [2; 32];
    const T0: u64 = 1_793_000_000 / 86_400 * 86_400; // midnight UTC

    #[test]
    fn the_minute_limit_resets_next_minute() {
        let l = Limits::new(2, 100, u64::MAX);
        assert_eq!(l.take(&A, T0), Ok(99));
        assert_eq!(l.take(&A, T0 + 1), Ok(98));
        assert_eq!(l.take(&A, T0 + 2), Err(Refusal::Minute));
        assert_eq!(l.take(&B, T0 + 2), Ok(99), "tokens are counted separately");
        assert_eq!(l.take(&A, T0 + 60), Ok(97));
    }

    #[test]
    fn the_daily_quota_resets_at_utc_midnight() {
        let l = Limits::new(100, 2, u64::MAX);
        l.take(&A, T0).unwrap();
        l.take(&A, T0 + 61).unwrap();
        assert_eq!(l.take(&A, T0 + 122), Err(Refusal::Day));
        assert_eq!(l.take(&A, T0 + 86_400), Ok(1));
    }

    #[test]
    fn the_budget_stops_everyone_until_tomorrow() {
        let l = Limits::new(100, 100, 1000);
        l.take(&A, T0).unwrap();
        l.spend(1000, T0);
        assert_eq!(l.take(&B, T0 + 1), Err(Refusal::Budget));
        assert!(l.take(&B, T0 + 86_400).is_ok());
    }
}
