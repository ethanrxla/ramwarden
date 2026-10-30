//! A string that will not end up in a log.
//!
//! The same idea as `ramwarden_ai::secret::Secret`, kept here because the
//! server must build on macOS and Windows too, and `ramwarden-ai` pulls in
//! `ramwarden-kernel`, whose syscalls are Linux-only.

use std::fmt;

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Secret(s.into())
    }

    /// The only way to read it. Named so a reviewer notices.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Enough to tell two values apart in a log without revealing either.
    pub fn fingerprint(&self) -> String {
        let s = self.0.trim();
        if s.len() < 8 {
            return "***".into();
        }
        let tail: String = s.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
        format!("***{tail}")
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret({})", self.fingerprint())
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.fingerprint())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printing_shows_only_the_tail() {
        let s = Secret::new("mongodb+srv://u:hunter2@cluster0.abcde.mongodb.net/");
        assert_eq!(format!("{s}"), "***b.net/");
        assert!(!format!("{s:?}").contains("hunter2"));
        assert_eq!(Secret::new("abc").fingerprint(), "***");
    }
}
