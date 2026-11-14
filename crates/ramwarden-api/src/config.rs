//! Server settings, all from the environment.
//!
//! The Jev key comes only from `JEV_API_KEY` in the host's secret store; there is
//! no config file to commit it to by mistake. Client tokens are hashed as soon as
//! they are read, so the plain values do not sit in memory for the server's life.

use std::net::SocketAddr;
use std::time::Duration;

use ramwarden_ai::secret::Secret;
use sha2::{Digest, Sha256};

use crate::checkout::{BASE_MAINNET, BASE_SEPOLIA, BaseConfig, parse_address};

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
    /// Set only for the explicit production profile, never by key presence alone.
    pub production_model: Option<(Secret, String)>,
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
    pub base: Option<BaseConfig>,
    pub stripe: Option<StripeKeys>,
    pub mail: MailConfig,
    /// SHA-256 of `RAMWARDEN_SUPPORT_TOKEN`: the team's key to the ticket
    /// queue. Unset, the site chat still answers and opens tickets; nobody
    /// can read them through the API until it is set.
    pub support_token: Option<TokenHash>,
    /// This API's public URL (`RAMWARDEN_PUBLIC_URL`), named in install lines.
    pub public_url: String,
    /// `RAMWARDEN_RELEASES_URL`: where install scripts fetch release assets.
    pub releases_url: String,
}

/// Stripe's two secrets. Both or neither.
#[derive(Clone)]
pub struct StripeKeys {
    pub secret_key: Secret,
    pub webhook_secret: Secret,
    /// `STRIPE_PRICE_PLAN_<id>`: the operator's own Stripe price for a plan.
    /// Unset, checkout builds the price from `card_cents`.
    pub prices: std::collections::BTreeMap<u32, String>,
}

/// How email sign-in codes are sent.
#[derive(Clone)]
pub enum MailConfig {
    /// Email sign-in is off; wallets still work.
    Off,
    Resend { key: Secret, from: String },
    /// Development only (`RAMWARDEN_DEV_MAIL_LOG=1`): codes go to the log.
    Log,
}

impl std::fmt::Debug for CloudConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudConfig")
            .field("site_origin", &self.site_origin)
            .field("entitlement_key", &self.entitlement_key.as_ref().map(|_| "<set>"))
            .field("downloads_dir", &self.downloads_dir)
            .field("base", &self.base)
            .field("stripe", &self.stripe.as_ref().map(|_| "<set>"))
            .field(
                "mail",
                &match &self.mail {
                    MailConfig::Off => "off",
                    MailConfig::Resend { .. } => "resend",
                    MailConfig::Log => "log",
                },
            )
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
        let production_model = match var("RAMWARDEN_API_PROFILE").as_deref() {
            None | Some("development") => None,
            Some("production") => Some((
                Secret::new(var("OPENAI_API_KEY").ok_or("OPENAI_API_KEY is required for production")?),
                var("OPENAI_MODEL_ID").unwrap_or_else(|| "gpt-6-luna".into()),
            )),
            Some(_) => return Err("RAMWARDEN_API_PROFILE must be development or production".into()),
        };

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
            production_model,
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
        let parsed = reqwest::Url::parse(&origin).map_err(|_| "Invalid RAMWARDEN_SITE_ORIGIN")?;
        if !safe_service_url(&parsed) || parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
            return Err("RAMWARDEN_SITE_ORIGIN must be an HTTPS origin or an HTTP loopback origin".into());
        }
        let origin = parsed.origin().ascii_serialization();
        let domain = origin.split_once("://").expect("validated scheme").1.to_string();
        let num = |k: &str, default: u64| -> Result<u64, String> {
            var(k).map_or(Ok(default), |v| v.parse().map_err(|_| format!("{k} must be a number, got `{v}`")))
        };
        // Real money waits for the contract's outside audit (spec 10.4): Base
        // mainnet and live Stripe keys are refused unless the operator says
        // so in as many words.
        let live_ok = var("RAMWARDEN_ALLOW_LIVE_PAYMENTS").as_deref() == Some("1");
        let base = match var("RAMWARDEN_CHECKOUT_ADDRESS") {
            None => None,
            Some(addr) => {
                let chain_id = num("RAMWARDEN_BASE_CHAIN_ID", BASE_SEPOLIA)?;
                if chain_id == BASE_MAINNET && !live_ok {
                    return Err("Base mainnet takes real money; set RAMWARDEN_ALLOW_LIVE_PAYMENTS=1 once the contract is audited".into());
                }
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
        let stripe = match (var("STRIPE_SECRET_KEY"), var("STRIPE_WEBHOOK_SECRET")) {
            (None, None) => None,
            (Some(k), Some(w)) => {
                if (k.starts_with("sk_live_") || k.starts_with("rk_live_")) && !live_ok {
                    return Err("STRIPE_SECRET_KEY is a live key; use a test key, or set RAMWARDEN_ALLOW_LIVE_PAYMENTS=1".into());
                }
                let mut prices = std::collections::BTreeMap::new();
                for id in 1..=4u32 {
                    if let Some(p) = var(&format!("STRIPE_PRICE_PLAN_{id}")) {
                        if !p.starts_with("price_") {
                            return Err(format!("STRIPE_PRICE_PLAN_{id} should be a Stripe price id (price_...)"));
                        }
                        prices.insert(id, p);
                    }
                }
                Some(StripeKeys { secret_key: Secret::new(k), webhook_secret: Secret::new(w), prices })
            }
            _ => return Err("set both STRIPE_SECRET_KEY and STRIPE_WEBHOOK_SECRET, or neither".into()),
        };
        let mail = match (var("RESEND_API_KEY"), var("RAMWARDEN_MAIL_FROM")) {
            (Some(key), Some(from)) => MailConfig::Resend { key: Secret::new(key), from },
            (Some(_), None) => return Err("RESEND_API_KEY needs RAMWARDEN_MAIL_FROM, like RamWarden <hello@ramwarden.app>".into()),
            _ if var("RAMWARDEN_DEV_MAIL_LOG").as_deref() == Some("1") => MailConfig::Log,
            _ => MailConfig::Off,
        };
        let public_url = var("RAMWARDEN_PUBLIC_URL")
            .map(|u| u.trim_end_matches('/').to_string())
            .unwrap_or_else(|| if origin.starts_with("http://") { "http://localhost:8080".into() } else { format!("https://api.{domain}") });
        if !reqwest::Url::parse(&public_url).is_ok_and(|url| safe_service_url(&url) && url.query().is_none() && url.fragment().is_none()) || public_url.contains('\'') {
            return Err("RAMWARDEN_PUBLIC_URL must be an https:// URL".into());
        }
        let support_token = match var("RAMWARDEN_SUPPORT_TOKEN") {
            Some(t) if t.trim().len() < MIN_TOKEN_LEN => {
                return Err(format!("RAMWARDEN_SUPPORT_TOKEN must be at least {MIN_TOKEN_LEN} characters"));
            }
            Some(t) => Some(hash_token(t.trim())),
            None => None,
        };
        let releases_url = var("RAMWARDEN_RELEASES_URL")
            .map(|u| u.trim_end_matches('/').to_string())
            .unwrap_or_else(|| crate::install::DEFAULT_RELEASES_URL.into());
        if !releases_url.starts_with("https://") || releases_url.contains('\'') {
            return Err("RAMWARDEN_RELEASES_URL must be an https:// URL".into());
        }
        Ok(Some(CloudConfig {
            public_url,
            releases_url,
            site_origin: origin,
            site_domain: domain,
            entitlement_key: var("RAMWARDEN_ENTITLEMENT_KEY").map(Secret::new),
            downloads_dir: var("RAMWARDEN_DOWNLOADS_DIR"),
            base,
            stripe,
            mail,
            support_token,
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
    fn an_openai_key_does_not_switch_personal_development_to_production() {
        let base = [("JEV_API_KEY", "test-jev"), ("RAMWARDEN_API_TOKENS", TOKEN), ("OPENAI_API_KEY", "openai-test")];
        assert!(cfg(&base).unwrap().production_model.is_none());
        let mut production = base.to_vec();
        production.push(("RAMWARDEN_API_PROFILE", "production"));
        assert_eq!(cfg(&production).unwrap().production_model.unwrap().1, "gpt-6-luna");
        production.retain(|(key, _)| *key != "OPENAI_API_KEY");
        assert!(cfg(&production).unwrap_err().contains("OPENAI_API_KEY"));
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
        assert_eq!(cloud.public_url, "https://api.ramwarden.app");

        let c = cfg(&[
            ("JEV_API_KEY", "k"),
            ("RAMWARDEN_SITE_ORIGIN", "https://ramwarden.app"),
            ("RAMWARDEN_CHECKOUT_ADDRESS", "0x00000000000000000000000000000000000000cc"),
        ])
        .unwrap();
        let base = c.cloud.unwrap().base.unwrap();
        assert_eq!(base.chain_id, BASE_SEPOLIA, "testnet unless told otherwise");
        assert_eq!(base.usdc, BaseConfig::default_usdc(84532).unwrap(), "USDC filled in for Base Sepolia");
        assert_eq!(base.confirmations, 5);

        assert!(cfg(&[("JEV_API_KEY", "k"), ("RAMWARDEN_SITE_ORIGIN", "http://evil.example")]).is_err());
        assert!(
            cfg(&[("JEV_API_KEY", "k"), ("RAMWARDEN_SITE_ORIGIN", "https://a.b"), ("RAMWARDEN_CHECKOUT_ADDRESS", "0x12")])
                .is_err()
        );
    }

    #[test]
    fn real_money_needs_saying_so() {
        let site = ("RAMWARDEN_SITE_ORIGIN", "https://ramwarden.app");
        let mainnet = [("JEV_API_KEY", "k"), site, ("RAMWARDEN_CHECKOUT_ADDRESS", "0x00000000000000000000000000000000000000cc"), ("RAMWARDEN_BASE_CHAIN_ID", "8453")];
        assert!(cfg(&mainnet).unwrap_err().contains("RAMWARDEN_ALLOW_LIVE_PAYMENTS"));
        let mut ok = mainnet.to_vec();
        ok.push(("RAMWARDEN_ALLOW_LIVE_PAYMENTS", "1"));
        assert_eq!(cfg(&ok).unwrap().cloud.unwrap().base.unwrap().chain_id, BASE_MAINNET);

        let live = [("JEV_API_KEY", "k"), site, ("STRIPE_SECRET_KEY", "sk_live_x"), ("STRIPE_WEBHOOK_SECRET", "whsec_x")];
        assert!(cfg(&live).is_err());
        let test = [("JEV_API_KEY", "k"), site, ("STRIPE_SECRET_KEY", "sk_test_x"), ("STRIPE_WEBHOOK_SECRET", "whsec_x")];
        assert!(cfg(&test).unwrap().cloud.unwrap().stripe.is_some());
        assert!(cfg(&[("JEV_API_KEY", "k"), site, ("STRIPE_SECRET_KEY", "sk_test_x")]).is_err(), "no webhook secret");
    }

    #[test]
    fn email_sign_in_needs_a_sender_or_the_dev_flag() {
        let site = ("RAMWARDEN_SITE_ORIGIN", "https://ramwarden.app");
        let mail = |extra: &[(&'static str, &'static str)]| {
            let mut v = vec![("JEV_API_KEY", "k"), site];
            v.extend_from_slice(extra);
            cfg(&v).map(|c| c.cloud.unwrap().mail)
        };
        assert!(matches!(mail(&[]).unwrap(), MailConfig::Off));
        assert!(matches!(mail(&[("RAMWARDEN_DEV_MAIL_LOG", "1")]).unwrap(), MailConfig::Log));
        assert!(matches!(
            mail(&[("RESEND_API_KEY", "re_x"), ("RAMWARDEN_MAIL_FROM", "RamWarden <a@b.c>")]).unwrap(),
            MailConfig::Resend { .. }
        ));
        assert!(mail(&[("RESEND_API_KEY", "re_x")]).is_err());
    }

    #[test]
    fn the_jev_key_does_not_print() {
        let c = cfg(&[("JEV_API_KEY", "jev-secret-value-XYZ123"), ("RAMWARDEN_API_TOKENS", TOKEN)]).unwrap();
        let printed = format!("{c:?}");
        assert!(!printed.contains("jev-secret-value"), "{printed}");
        assert!(!printed.contains(TOKEN), "{printed}");
    }
}

fn safe_service_url(url: &reqwest::Url) -> bool {
    url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && (url.scheme() == "https" || (url.scheme() == "http" && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]" | "::1"))))
}

#[cfg(test)]
mod local_origin_tests {
    #[test]
    fn only_real_loopback_hosts_may_use_http() {
        for host in ["localhost:8000", "127.0.0.1:8080", "[::1]:8000"] {
            assert!(super::safe_service_url(&reqwest::Url::parse(&format!("http://{host}")).unwrap()));
        }
        for url in ["http://localhost.evil.example", "http://localhost@evil.example", "https://user:pass@example.com", "http://example.com"] {
            assert!(!super::safe_service_url(&reqwest::Url::parse(url).unwrap()), "{url}");
        }
    }
}
