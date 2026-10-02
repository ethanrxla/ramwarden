//! RamWarden daemon.
//!
//! Serves the same `:7823` API v1 served, so the shipped browser extensions keep
//! working untouched, and drives the remediation ladder from PSI triggers.

use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use ramwarden_core::actuator::Actuator;
use ramwarden_core::detector::Detector;
use ramwarden_core::history::History;
use ramwarden_core::ladder::Ladder;
use ramwarden_core::{config, desktop};
use ramwarden_daemon::hub::{AppState, Hub};
use ramwarden_daemon::{ai, monitor, routes};
use ramwarden_kernel::Root;
use tower_http::cors::CorsLayer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("RAMWARDEN_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = config::load().unwrap_or_else(|e| {
        tracing::error!("config: {e} — falling back to defaults");
        config::Config::default()
    });
    if let Some(src) = &cfg.source {
        tracing::info!("config loaded from {}", src.display());
    }

    let root = Root::system();

    // The ladder and the read endpoints each get their own SQLite connection.
    let ladder_history = History::open(&cfg.db_path)?;
    let api_history = History::open(&cfg.db_path)?;
    tracing::info!(
        "history at {} ({} closed tabs recorded)",
        cfg.db_path.display(),
        api_history.count_tabs().unwrap_or(0)
    );

    let mut detector = Detector::new(root.clone());

    // Take the first sample before serving, so the first request sees something.
    // The detector still refuses to call anything idle until the second sample,
    // which is the point of `is_warm`.
    let mut probe = desktop::DesktopProbe::new();
    if let Err(e) = detector.sample(&probe.sample()) {
        tracing::warn!("first sample failed: {e}");
    }

    let ladder =
        Ladder::new(cfg.ladder.clone(), Actuator::new(root.clone())).with_history(ladder_history);

    let provider = ai::provider(&cfg);
    let state = AppState {
        cfg: Arc::new(cfg),
        root,
        det: Arc::new(RwLock::new(detector)),
        ladder: Arc::new(Mutex::new(ladder)),
        hub: Arc::new(Mutex::new(Hub::new())),
        history: Arc::new(Mutex::new(api_history)),
        ai: Arc::new(provider),
        started: Instant::now(),
    };

    monitor::spawn(state.clone(), tokio::runtime::Handle::current());

    let bind = format!("{}:{}", state.cfg.server.host, state.cfg.server.port);
    // The extensions run from a browser origin, so they need CORS. v1 allowed
    // everything; the surface is a loopback daemon with no credentials.
    let app = routes::router(state).layer(CorsLayer::permissive());

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("RamWarden listening on http://{bind}");

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
        })
        .await?;
    Ok(())
}
