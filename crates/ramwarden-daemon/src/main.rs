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
use ramwarden_core::{config, host};
use ramwarden_daemon::hub::{AppState, Hub};
use ramwarden_daemon::{ai, monitor, routes};

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

    let platform = Arc::new(host::platform());
    tracing::info!("platform: {}", platform.name);

    // The ladder and the read endpoints each get their own SQLite connection.
    let ladder_history = History::open(&cfg.db_path)?;
    let api_history = History::open(&cfg.db_path)?;
    tracing::info!(
        "history at {} ({} closed tabs recorded)",
        cfg.db_path.display(),
        api_history.count_tabs().unwrap_or(0)
    );

    let mut detector = Detector::new(platform.processes());

    // Take the first sample before serving, so the first request sees something.
    // The detector still refuses to call anything idle until the second sample,
    // which is the point of `is_warm`.
    if let Err(e) = detector.sample(&platform.desktop().sample()) {
        tracing::warn!("first sample failed: {e}");
    }

    let mut ladder = Ladder::new(cfg.ladder.clone(), Actuator::for_platform(&platform))
        .with_history(ladder_history);
    if cfg.tidy.enabled {
        tracing::info!(
            "tidy: hinting and trimming apps quiet for {} min (background hints: {}, trim: {})",
            cfg.tidy.after_minutes,
            cfg.tidy.background_hints,
            cfg.tidy.trim
        );
        ladder = ladder.with_tidy(cfg.tidy.clone());
    }

    let provider = ai::provider(&cfg);
    let state = AppState {
        cfg: Arc::new(cfg),
        platform,
        det: Arc::new(RwLock::new(detector)),
        ladder: Arc::new(Mutex::new(ladder)),
        hub: Arc::new(Mutex::new(Hub::new())),
        history: Arc::new(Mutex::new(api_history)),
        ai: Arc::new(provider),
        started: Instant::now(),
    };

    monitor::spawn(state.clone(), tokio::runtime::Handle::current());

    let bind = format!("{}:{}", state.cfg.server.host, state.cfg.server.port);
    // CORS and the origin guard are part of the router; see `guard`.
    let app = routes::router(state.clone());
    if !ramwarden_daemon::guard::is_loopback(&state.cfg.server.host) {
        tracing::warn!(
            "listening on {} — reachable from other machines with NO authentication. \
             Web pages are still refused, but anything on the network can freeze or \
             kill your apps. Use host = \"127.0.0.1\" unless you mean this.",
            state.cfg.server.host
        );
    }

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("RamWarden listening on http://{bind}");

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            shutdown_signal().await;
            tracing::info!("shutting down");
        })
        .await?;

    // No background hint outlives the daemon that set it.
    for (_, target, outcome) in state.ladder.lock().unwrap().release_tidy() {
        if !outcome.did_nothing() {
            tracing::info!("restored {target}");
        }
    }
    Ok(())
}

/// Ctrl-C everywhere; on Windows also console close, logoff and shutdown, so
/// a daemon ended with the session still restores what Tidy changed.
async fn shutdown_signal() {
    #[cfg(windows)]
    {
        use tokio::signal::windows::{ctrl_close, ctrl_logoff, ctrl_shutdown};
        let (mut close, mut logoff, mut shutdown) = match (ctrl_close(), ctrl_logoff(), ctrl_shutdown()) {
            (Ok(c), Ok(l), Ok(s)) => (c, l, s),
            _ => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = close.recv() => {}
            _ = logoff.recv() => {}
            _ = shutdown.recv() => {}
        }
    }
    #[cfg(not(windows))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
