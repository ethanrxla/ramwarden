//! Server settings, all from the environment.
//!
//! The Jev key comes only from `JEV_API_KEY` in the host's secret store; there is
//! no config file to commit it to by mistake. Client tokens are hashed as soon as
//! they are read, so the plain values do not sit in memory for the server's life.

use std::net::SocketAddr;
use std::time::Duration;

use ramwarden_ai::secret::Secret;
use sha2::{Digest, Sha256};

use crate::checkout::{BASE_MAINNET, BaseConfig, parse_address};

/// A client token, kept only as its SHA-256.
pub type TokenHash = [u8; 32];

pub fn hash_token(token: &str) -> TokenHash {
    Sha256::digest(token.trim().as_bytes()).into()
}

/// Enough of a hash to tell tokens apart in a log.
pub fn token_label(h: &TokenHash) -> String {
    h.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// Shortest client token accepted, so a guessable one cannot be configured.
pub const MIN_TOKEN_LEN: usize = 24;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: SocketAddr,
    pub jev_base_url: String,
    pub jev_model: String,
    pub jev_key: Secret,
    pub jev_timeout: Duration,
    /// Phase 1 access list. Accounts and device sign-in replace it.
    pub tokens: Vec<TokenHash>,
    /// Path to extra question definitions (see [`crate::catalog`]).
    pub catalog_path: Option<String>,
    pub per_minute: u32,
    pub per_day: u32,
    pub max_state_bytes: usize,
    pub max_questions: usize,
    /// Upstream input tokens per UTC day, across all clients, before the server
    /// stops calling Jev.
    pub daily_input_tokens: u64,
    /// Phase 2: accounts, sign-in and checkout. On when
    /// `RAMWARDEN_SITE_ORIGIN` is set.
    pub cloud: Option<CloudConfig>,
}

#[derive(Clone)]
pub struct CloudConfig {
    /// The website's origin, e.g. `https://ramwarden.app`.
    pub site_origin: String,
    /// Its host, named in sign-in messages.
    pub site_domain: String,
    /// Base64 Ed25519 seed for signing entitlements. A secret.
    pub entitlement_key: Option<Secret>,
    pub downloads_dir: Option<String>,
    pub free_per_day: u32,
    pub base: Option<BaseConfig>,
}

impl std::fmt::Debug for CloudConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudConfig")
            .field("site_origin", &self.site_origin)
            .field("entitlement_key", &self.entitlement_key.as_ref().map(|_| "<set>"))
            .field("downloads_dir", &self.downloads_dir)
            .field("free_per_day", &self.free_per_day)
            .field("base", &self.base)
            .finish()
    }
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let var = |k: &str| get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        fn num<T: std::str::FromStr>(k: &str, v: Option<String>, default: T) -> Result<T, String> {
            match v {
                Some(s) => s.parse().map_err(|_| format!("{k} must be a number, got `{s}`")),
                None => Ok(default),
            }
        }

        let jev_key = Secret::new(var("JEV_API_KEY").ok_or("JEV_API_KEY is not set")?);

        let cloud = Self::cloud_from(&var)?;

        // With accounts on, devices sign in and phase 1 tokens are optional
        // (the operator's own machines). Without, they are the only way in.
        let raw_tokens = var("RAMWARDEN_API_TOKENS");
        if raw_tokens.is_none() && cloud.is_none() {
            return Err("RAMWARDEN_API_TOKENS is not set (comma-separated)".into());
        }
        let mut tokens = Vec::new();
        for t in raw_tokens.iter().flat_map(|r| r.split(',')).map(str::trim).filter(|t| !t.is_empty()) {
            if t.len() < MIN_TOKEN_LEN {
                return Err(format!("every RAMWARDEN_API_TOKENS entry needs at least {MIN_TOKEN_LEN} characters"));
            }
            tokens.push(hash_token(t));
        }
        if tokens.is_empty() && cloud.is_none() {
            return Err("RAMWARDEN_API_TOKENS has no tokens".into());
        }

        let bind = var("RAMWARDEN_API_BIND").unwrap_or_else(|| "0.0.0.0:8080".into());
        Ok(Config {
            bind: bind.parse().map_err(|_| format!("RAMWARDEN_API_BIND is not an address: `{bind}`"))?,
            jev_base_url: var("JEV_BASE_URL").unwrap_or_else(|| "https://api.typesafe.ai".into()),
            jev_model: var("JEV_MODEL").unwrap_or_else(|| "jev-latest".into()),
            jev_key,
            jev_timeout: Duration::from_secs(num("JEV_TIMEOUT_SECONDS", var("JEV_TIMEOUT_SECONDS"), 10)?),
            tokens,
            catalog_path: var("RAMWARDEN_API_CATALOG"),
            // The daemon caps itself at 60 Jev calls an hour by default; these
            // leave room for that and stop a script.
            per_minute: num("RAMWARDEN_API_PER_MINUTE", var("RAMWARDEN_API_PER_MINUTE"), 20)?,
            per_day: num("RAMWARDEN_API_PER_DAY", var("RAMWARDEN_API_PER_DAY"), 1000)?,
            // The ladder judge sends up to 8 candidates in one request, two
            // questions each, with a JSON state describing all of them.
            max_state_bytes: num("RAMWARDEN_API_MAX_STATE_BYTES", var("RAMWARDEN_API_MAX_STATE_BYTES"), 16_384)?,
            max_questions: num("RAMWARDEN_API_MAX_QUESTIONS", var("RAMWARDEN_API_MAX_QUESTIONS"), 24)?,
            daily_input_tokens: num(
                "RAMWARDEN_API_DAILY_INPUT_TOKENS",
                var("RAMWARDEN_API_DAILY_INPUT_TOKENS"),
                20_000_000,
            )?,
            cloud,
        })
    }

    fn cloud_from(var: &dyn Fn(&str) -> Option<String>) -> Result<Option<CloudConfig>, String> {
        let Some(origin) = var("RAMWARDEN_SITE_ORIGIN") else { return Ok(None) };
        let origin = origin.trim_end_matches('/').to_string();
        let domain = origin
            .strip_prefix("https://")
            .or_else(|| origin.strip_prefix("http://localhost").map(|_| origin.trim_start_matches("http://")))
            .filter(|d| !d.is_empty() && !d.contains('/'))
            .ok_or("RAMWARDEN_SITE_ORIGIN must be an https:// origin, like https://ramwarden.app")?
            .to_string();
        let num = |k: &str, default: u64| -> Result<u64, String> {
            var(k).map_or(Ok(default), |v| v.parse().map_err(|_| format!("{k} must be a number, got `{v}`")))
        };
        let base = match var("RAMWARDEN_CHECKOUT_ADDRESS") {
            None => None,
            Some(addr) => {
                let chain_id = num("RAMWARDEN_BASE_CHAIN_ID", BASE_MAINNET)?;
                let usdc = match var("RAMWARDEN_BASE_USDC") {
                    Some(a) => parse_address(&a)?,
                    None => BaseConfig::default_usdc(chain_id)
                        .ok_or("RAMWARDEN_BASE_USDC is needed for a chain other than Base or Base Sepolia")?,
                };
                Some(BaseConfig {
                    chain_id,
                    checkout: parse_address(&addr).map_err(|e| format!("RAMWARDEN_CHECKOUT_ADDRESS: {e}"))?,
                    usdc,
                    rpc_url: var("RAMWARDEN_BASE_RPC_URL"),
                    confirmations: num("RAMWARDEN_BASE_CONFIRMATIONS", 5)?,
                    from_block: var("RAMWARDEN_BASE_FROM_BLOCK").map(|_| num("RAMWARDEN_BASE_FROM_BLOCK", 0)).transpose()?,
                })
            }
        };
        Ok(Some(CloudConfig {
            site_origin: origin,
            site_domain: domain,
            entitlement_key: var("RAMWARDEN_ENTITLEMENT_KEY").map(Secret::new),
            downloads_dir: var("RAMWARDEN_DOWNLOADS_DIR"),
            free_per_day: num("RAMWARDEN_FREE_PER_DAY", 5)? as u32,
            base,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const TOKEN: &str = "rw_test_token_0123456789abcdef";

    fn cfg(pairs: &[(&str, &str)]) -> Result<Config, String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Config::from_lookup(|k| m.get(k).cloned())
    }

    #[test]
    fn defaults_fill_everything_but_the_secrets() {
        let c = cfg(&[("JEV_API_KEY", "jev-key-abcdef"), ("RAMWARDEN_API_TOKENS", TOKEN)]).unwrap();
        assert_eq!(c.jev_base_url, "https://api.typesafe.ai");
        assert_eq!(c.jev_model, "jev-latest");
        assert_eq!(c.bind.port(), 8080);
        assert_eq!(c.tokens, vec![hash_token(TOKEN)]);
        assert_eq!((c.per_minute, c.per_day, c.max_state_bytes, c.max_questions), (20, 1000, 16_384, 24));
    }

    #[test]
    fn missing_or_weak_secrets_are_refused() {
        assert!(cfg(&[("RAMWARDEN_API_TOKENS", TOKEN)]).unwrap_err().contains("JEV_API_KEY"));
        assert!(cfg(&[("JEV_API_KEY", "k")]).unwrap_err().contains("RAMWARDEN_API_TOKENS"));
        assert!(cfg(&[("JEV_API_KEY", "k"), ("RAMWARDEN_API_TOKENS", "short")]).unwrap_err().contains("24"));
        assert!(cfg(&[("JEV_API_KEY", "k"), ("RAMWARDEN_API_TOKENS", " , ")]).is_err());
    }

    #[test]
    fn bad_numbers_name_the_variable() {
        let e = cfg(&[("JEV_API_KEY", "k"), ("RAMWARDEN_API_TOKENS", TOKEN), ("RAMWARDEN_API_PER_DAY", "lots")])
            .unwrap_err();
        assert!(e.contains("RAMWARDEN_API_PER_DAY"), "{e}");
    }

    #[test]
    fn the_site_origin_turns_on_accounts_and_base_checkout() {
        let c = cfg(&[("JEV_API_KEY", "k"), ("RAMWARDEN_SITE_ORIGIN", "https://ramwarden.app/")]).unwrap();
        let cloud = c.cloud.unwrap();
        assert!(c.tokens.is_empty(), "devices sign in; phase 1 tokens are optional");
        assert_eq!((cloud.site_origin.as_str(), cloud.site_domain.as_str()), ("https://ramwarden.app", "ramwarden.app"));
        assert!(cloud.base.is_none(), "no contract address, no Base checkout");

        let c = cfg(&[
            ("JEV_API_KEY", "k"),
            ("RAMWARDEN_SITE_ORIGIN", "https://ramwarden.app"),
            ("RAMWARDEN_CHECKOUT_ADDRESS", "0x00000000000000000000000000000000000000cc"),
            ("RAMWARDEN_BASE_CHAIN_ID", "84532"),
        ])
        .unwrap();
        let base = c.cloud.unwrap().base.unwrap();
        assert_eq!(base.usdc, BaseConfig::default_usdc(84532).unwrap(), "USDC filled in for Base Sepolia");
        assert_eq!(base.confirmations, 5);

        assert!(cfg(&[("JEV_API_KEY", "k"), ("RAMWARDEN_SITE_ORIGIN", "http://evil.example")]).is_err());
        assert!(
            cfg(&[("JEV_API_KEY", "k"), ("RAMWARDEN_SITE_ORIGIN", "https://a.b"), ("RAMWARDEN_CHECKOUT_ADDRESS", "0x12")])
                .is_err()
        );
    }

    #[test]
    fn the_jev_key_does_not_print() {
        let c = cfg(&[("JEV_API_KEY", "jev-secret-value-XYZ123"), ("RAMWARDEN_API_TOKENS", TOKEN)]).unwrap();
        let printed = format!("{c:?}");
        assert!(!printed.contains("jev-secret-value"), "{printed}");
        assert!(!printed.contains(TOKEN), "{printed}");
    }
}
