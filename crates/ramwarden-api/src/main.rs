//! `ramwarden-api`: the hosted Jev tier.
//!
//! Configured entirely from the environment; see [`ramwarden_api::Config`].

use std::sync::Arc;

use ramwarden_api::accounts::{Cloud, Site};
use ramwarden_api::catalog::Catalog;
use ramwarden_api::checkout::{BaseChain, Plans};
use ramwarden_api::entitlement::EntitlementSigner;
use ramwarden_api::store::{MemoryStore, Store};
use ramwarden_api::limits::Limits;
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

    let upstream = Upstream::new(&cfg.jev_base_url, &cfg.jev_model, cfg.jev_key.clone(), cfg.jev_timeout)?;
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
            // Until the Atlas store is wired, accounts live in memory and a
            // restart signs everyone out. Passes are safe: the watcher
            // rescans from RAMWARDEN_BASE_FROM_BLOCK and the chain holds them.
            let store: Arc<dyn Store> = Arc::new(MemoryStore::new());
            tracing::warn!("accounts are held in memory; a restart signs every device out");
            let plans = Arc::new(Plans::built_in());
            let signer = match &cc.entitlement_key {
                Some(k) => {
                    let s = EntitlementSigner::from_base64(k.expose())?;
                    tracing::info!("entitlement public key {}", s.public_key());
                    Some(s)
                }
                None => {
                    tracing::warn!("RAMWARDEN_ENTITLEMENT_KEY not set: paid wardens cannot unlock on devices");
                    None
                }
            };
            let base = cc.base.clone().map(|b| Arc::new(BaseChain::new(b)));
            if let Some(b) = &base {
                tracing::info!("Base checkout {} on {}", b.cfg.checkout_hex(), b.cfg.network());
                if b.cfg.rpc_url.is_some() {
                    tokio::spawn(b.clone().run(store.clone(), plans.clone(), std::time::Duration::from_secs(4)));
                } else {
                    tracing::warn!("RAMWARDEN_BASE_RPC_URL not set: purchases will not be confirmed");
                }
            }
            Some(Cloud {
                store,
                plans,
                site: Site { domain: cc.site_domain.clone(), origin: cc.site_origin.clone() },
                siwe_chain_id: base.as_ref().map_or(ramwarden_api::checkout::BASE_MAINNET, |b| b.cfg.chain_id),
                base,
                signer,
                downloads_dir: cc.downloads_dir.clone().map(Into::into),
                free_per_day: cc.free_per_day,
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
