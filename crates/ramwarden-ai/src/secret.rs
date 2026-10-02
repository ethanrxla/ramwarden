//! A string that will not end up in a log.
//!
//! RamWarden logs generously — the ladder's decisions have to be auditable after
//! the fact. An API key held in a plain `String` inside a `#[derive(Debug)]`
//! struct reaches a log line the first time anyone adds a `tracing::debug!` of
//! the surrounding config.
//!
//! This machine already demonstrates the failure mode: four distinct `nvapi-`
//! keys were found on it, several of them sitting in agent session transcripts
//! because they were pasted into a chat. Making the type refuse to print itself
//! is cheap insurance.

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

    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }

    /// Enough to tell two keys apart in a log without revealing either.
    pub fn fingerprint(&self) -> String {
        let s = self.0.trim();
        if s.len() < 8 {
            return "***".into();
        }
        format!("***{}", &s[s.len() - 6..])
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

impl<'de> serde::Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(Secret)
    }
}

/// Never serialised — there is no legitimate reason to write a key into an API
/// response or a config dump, and the only way to be sure is not to implement it.
impl serde::Serialize for Secret {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.fingerprint())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "nvapi-abcdefghijklmnopqrstuvwxyz0123456ZZTOP9";

    #[test]
    fn debug_output_does_not_contain_the_key() {
        let s = Secret::new(KEY);
        let printed = format!("{s:?}");
        assert!(!printed.contains(KEY), "{printed}");
        assert!(!printed.contains("abcdefgh"), "{printed}");
        assert!(printed.contains("***ZZTOP9"), "{printed}");
    }

    #[test]
    fn display_output_does_not_contain_the_key() {
        let printed = format!("{}", Secret::new(KEY));
        assert!(!printed.contains(KEY));
        assert_eq!(printed, "***ZZTOP9");
    }

    /// A key inside a derived-Debug struct is the realistic leak path.
    #[test]
    fn a_key_nested_in_a_derived_debug_struct_stays_hidden() {
        #[derive(Debug)]
        struct Config {
            model: String,
            key: Secret,
        }
        let c = Config {
            model: "nemotron".into(),
            key: Secret::new(KEY),
        };
        let printed = format!("{c:?}");
        assert!(!printed.contains(KEY), "{printed}");
        assert!(printed.contains("nemotron"), "the rest must still print");
        assert_eq!(c.model, "nemotron");
        assert_eq!(c.key.expose(), KEY, "the value is still reachable deliberately");
    }

    #[test]
    fn serialising_emits_only_the_fingerprint() {
        let json = serde_json::to_string(&Secret::new(KEY)).unwrap();
        assert!(!json.contains(KEY), "{json}");
        assert_eq!(json, r#""***ZZTOP9""#);
    }

    #[test]
    fn exposing_returns_the_real_value() {
        assert_eq!(Secret::new(KEY).expose(), KEY);
    }

    #[test]
    fn a_short_or_blank_secret_fingerprints_without_panicking() {
        assert_eq!(Secret::new("abc").fingerprint(), "***");
        assert_eq!(Secret::new("").fingerprint(), "***");
        assert!(Secret::new("   ").is_empty());
        assert!(!Secret::new(KEY).is_empty());
    }

    #[test]
    fn deserialises_from_a_plain_json_string() {
        let s: Secret = serde_json::from_str(r#""nvapi-xyz123456789abc""#).unwrap();
        assert_eq!(s.expose(), "nvapi-xyz123456789abc");
    }
}
