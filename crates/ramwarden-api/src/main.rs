//! `ramwarden-api`: the hosted Jev tier.
//!
//! Configured entirely from the environment; see [`ramwarden_api::Config`].

use std::sync::Arc;

use ramwarden_api::accounts::{Cloud, Site};
use ramwarden_api::billing::Billing;
use ramwarden_api::catalog::Catalog;
use ramwarden_api::checkout::{BaseChain, Plans};
use ramwarden_api::config::MailConfig;
use ramwarden_api::entitlement::EntitlementSigner;
use ramwarden_api::limits::Limits;
use ramwarden_api::mail::{LogMailer, Mailer, ResendMailer};
use ramwarden_api::stripe::Stripe;
use ramwarden_cloud::config::ConfigError;
use ramwarden_cloud::{MemoryStore, MongoStore, Store};
use ramwarden_api::routes::unix_now;
use ramwarden_api::upstream::Upstream;
use ramwarden_api::{AppState, Config, router};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("RAMWARDEN_API_LOG")
                .unwrap_or_else(|_| "ramwarden_api=info".into()),
        )
        .init();

    if let Err(e) = run().await {
        tracing::error!("{e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let cfg = Config::from_env()?;

    let mut catalog = Catalog::built_in();
    if let Some(path) = &cfg.catalog_path {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        let n = catalog.extend_from_json(&text)?;
        tracing::info!("loaded {n} extra question definitions from {path}");
    }

    let mut upstream = Upstream::new(&cfg.jev_base_url, &cfg.jev_model, cfg.jev_key.clone(), cfg.jev_timeout)?;
    if let Some((key, model)) = &cfg.production_model {
        upstream = upstream.with_openai(key.clone(), model.clone());
    }
    tracing::info!(
        "jev {} model {} key {}; {} tokens; {} questions; {}/min {}/day per token; {} input tokens/day",
        cfg.jev_base_url,
        cfg.jev_model,
        upstream.key_fingerprint(),
        cfg.tokens.len(),
        catalog.len(),
        cfg.per_minute,
        cfg.per_day,
        cfg.daily_input_tokens
    );

    let cloud = match &cfg.cloud {
        None => {
            tracing::info!("accounts off (RAMWARDEN_SITE_ORIGIN not set): phase 1 tokens only");
            None
        }
        Some(cc) => {
            let store = open_store().await?;
            let plans = Arc::new(Plans::built_in());
            let signer = match &cc.entitlement_key {
                Some(k) => {
                    let s = EntitlementSigner::from_base64(k.expose())?;
                    tracing::info!("entitlement public key {}", s.public_key());
                    Some(s)
                }
                None => {
                    tracing::warn!("RAMWARDEN_ENTITLEMENT_KEY not set: wardens cannot unlock on devices");
                    None
                }
            };
            let base = cc.base.clone().map(|b| Arc::new(BaseChain::new(b)));
            if let Some(b) = &base {
                tracing::info!("Base checkout {} on {}", b.cfg.checkout_hex(), b.cfg.network());
                if b.cfg.rpc_url.is_some() {
                    tokio::spawn(b.clone().run(store.clone(), plans.clone(), std::time::Duration::from_secs(4)));
                } else {
                    tracing::warn!("RAMWARDEN_BASE_RPC_URL not set: subscriptions on Base will not be recorded");
                }
            }
            let stripe = cc.stripe.as_ref().map(|k| Stripe::new(k.secret_key.clone(), k.webhook_secret.clone()));
            match &stripe {
                Some(s) => tracing::info!("card checkout on ({} mode)", if s.test_mode() { "test" } else { "live" }),
                None => tracing::info!("card checkout off (STRIPE_SECRET_KEY not set)"),
            }
            let mailer: Option<Arc<dyn Mailer>> = match &cc.mail {
                MailConfig::Off => {
                    tracing::info!("email sign-in off (RESEND_API_KEY not set)");
                    None
                }
                MailConfig::Resend { key, from } => Some(Arc::new(ResendMailer::new(key.clone(), from.clone()))),
                MailConfig::Log => {
                    tracing::warn!("RAMWARDEN_DEV_MAIL_LOG=1: sign-in codes go to this log. Never in production.");
                    Some(Arc::new(LogMailer))
                }
            };
            Some(Cloud {
                threat: cc.threat.clone().map(ramwarden_api::threat::ThreatService::new).transpose()?,
                store,
                plans,
                site: Site { domain: cc.site_domain.clone(), origin: cc.site_origin.clone() },
                siwe_chain_id: base.as_ref().map_or(ramwarden_api::checkout::BASE_SEPOLIA, |b| b.cfg.chain_id),
                billing: Billing { base, stripe },
                signer,
                mailer,
                downloads_dir: cc.downloads_dir.clone().map(Into::into),
                public_url: cc.public_url.clone(),
                releases_url: cc.releases_url.clone(),
                support: {
                    if cc.support_token.is_none() {
                        tracing::info!("support staff routes off (RAMWARDEN_SUPPORT_TOKEN not set); the site chat still answers");
                    }
                    ramwarden_api::support::SupportDesk::new(cc.support_token)
                },
            })
        }
    };

    let state = Arc::new(AppState {
        tokens: cfg.tokens.iter().copied().collect(),
        catalog,
        limits: Limits::new(cfg.per_minute, cfg.per_day, cfg.daily_input_tokens),
        upstream,
        max_state_bytes: cfg.max_state_bytes,
        max_questions: cfg.max_questions,
        now: unix_now,
        cloud,
    });

    let listener = tokio::net::TcpListener::bind(cfg.bind).await.map_err(|e| format!("bind {}: {e}", cfg.bind))?;
    tracing::info!("listening on {}", cfg.bind);
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| e.to_string())
}

/// MongoDB Atlas when a connection string is configured (`MONGODB_URI`,
/// `.env` or `secrets.toml`); otherwise memory, for local development only.
async fn open_store() -> Result<Arc<dyn Store>, String> {
    match ramwarden_cloud::CloudConfig::load() {
        Ok(cfg) => {
            let m = MongoStore::connect(&cfg).await.map_err(|e| e.to_string())?;
            m.ensure_indexes().await.map_err(|e| e.to_string())?;
            Ok(Arc::new(m))
        }
        Err(ConfigError::Missing(_)) => development_store(
            std::env::var("RAMWARDEN_DEV_MEMORY_STORE").as_deref() == Ok("1")
        ),
        Err(e) => Err(e.to_string()),
    }
}

// A missing production secret must never quietly discard account/payment state.
fn development_store(explicitly_enabled: bool) -> Result<Arc<dyn Store>, String> {
    if !explicitly_enabled {
        return Err("MONGODB_URI is required for hosted accounts. For disposable local development only, set RAMWARDEN_DEV_MEMORY_STORE=1; all accounts and payment mappings disappear on restart.".into());
    }
    tracing::warn!("RAMWARDEN_DEV_MEMORY_STORE=1: disposable accounts; do not accept real payments");
    Ok(Arc::new(MemoryStore::new()))
}

#[cfg(test)]
mod startup_tests {
    #[test]
    fn missing_database_fails_closed_unless_development_is_explicit() {
        assert!(super::development_store(false).is_err());
        assert!(super::development_store(true).is_ok());
    }
}
