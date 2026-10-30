//! `ramwarden-api`: the hosted Jev tier.
//!
//! Configured entirely from the environment; see [`ramwarden_api::Config`].

use std::sync::Arc;

use ramwarden_api::catalog::Catalog;
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

    let state = Arc::new(AppState {
        tokens: cfg.tokens.iter().copied().collect(),
        catalog,
        limits: Limits::new(cfg.per_minute, cfg.per_day, cfg.daily_input_tokens),
        upstream,
        max_state_bytes: cfg.max_state_bytes,
        max_questions: cfg.max_questions,
        now: unix_now,
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
