//! RamWarden daemon.
//!
//! Serves the same `:7823` API v1 served, so the shipped browser extensions keep
//! working untouched, and drives the remediation ladder from PSI triggers.

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use ramwarden_core::actuator::Actuator;
use ramwarden_core::detector::Detector;
use ramwarden_core::history::History;
use ramwarden_core::ladder::Ladder;
use ramwarden_core::{config, host};
use ramwarden_daemon::hub::{AppState, Hub};
use ramwarden_daemon::{ai, monitor, routes};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `--log-file <path>`: for a headless daemon (the Windows logon task)
    // that has no terminal to write to.
    let filter = tracing_subscriber::EnvFilter::try_from_env("RAMWARDEN_LOG")
        .unwrap_or_else(|_| "info".into());
    let args: Vec<String> = std::env::args().collect();
    let log_file = args
        .iter()
        .position(|a| a == "--log-file")
        .and_then(|i| args.get(i + 1))
        .and_then(|p| {
            let p = std::path::Path::new(p);
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            std::fs::OpenOptions::new().create(true).append(true).open(p).ok()
        });
    match log_file {
        Some(f) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(f))
            .init(),
        None => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }

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

    // Whatever a previous run froze and never woke (a crash, a hard kill, a
    // restart) is thawed now: nothing else would ever wake it.
    let journal = cfg.db_path.with_file_name("suspended.json");
    for woke in ramwarden_daemon::freeze_journal::thaw_leftovers(&journal, &detector, platform.control.as_ref()) {
        tracing::warn!("thawed {woke}, left frozen by a previous run");
    }

    let predictor = Arc::new(ramwarden_daemon::predict::LivePredictor::new(
        cfg.db_path.with_file_name("predictor.safetensors"),
    ));
    let mut ladder = Ladder::new(cfg.ladder.clone(), Actuator::for_platform(&platform))
        .with_history(ladder_history)
        .with_ranker(predictor.clone());
    if cfg.tidy.enabled {
        tracing::info!(
            "tidy: hinting and trimming apps quiet for {} min (background hints: {}, trim: {})",
            cfg.tidy.after_minutes,
            cfg.tidy.background_hints,
            cfg.tidy.trim
        );
        ladder = ladder.with_tidy(cfg.tidy.clone());
    }

    // The Jev decision tier: advisory, opt-in, and a no-op unless enabled and keyed.
    {
        let j = &cfg.jev;
        let asker: Option<Box<dyn ramwarden_daemon::tabintel::Asker>> = if j.enabled && !j.api_key.trim().is_empty() {
            match ramwarden_ai::jev::Jev::new(
                &j.base_url,
                ramwarden_ai::secret::Secret::new(j.api_key.trim()),
                &j.model,
                Duration::from_secs(j.timeout_seconds),
            ) {
                Ok(c) => {
                    tracing::info!("jev tier on: model {} key {} weight {} (feature descriptions only; no URLs or titles)", j.model, c.key_fingerprint(), j.weight);
                    Some(Box::new(c))
                }
                Err(e) => {
                    tracing::warn!("jev tier off: {e}");
                    None
                }
            }
        } else {
            tracing::info!("jev tier off ({})", if !j.enabled { "set [jev] enabled = true to use it" } else { "no JEV_API_KEY" });
            None
        };
        ramwarden_daemon::tabintel::install(Arc::new(ramwarden_daemon::tabintel::TabIntel::new(j.clone(), asker)));
    }

    let provider = ai::provider(&cfg);
    ramwarden_daemon::embedder::attach(&provider);
    let state = AppState {
        cfg: Arc::new(cfg),
        platform,
        det: Arc::new(RwLock::new(detector)),
        ladder: Arc::new(Mutex::new(ladder)),
        hub: Arc::new(Mutex::new(Hub::new())),
        history: Arc::new(Mutex::new(api_history)),
        models: Arc::new(ramwarden_ai::models::ModelHosts::new(provider.ollama.clone())),
        predictor,
        ai: Arc::new(provider),
        started: Instant::now(),
        hold_until: Arc::new(Mutex::new(None)),
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

    // No frozen app outlives the daemon that froze it.
    for (name, outcome) in state.ladder.lock().unwrap().thaw_all() {
        tracing::info!("thawed {name} on shutdown ({} process(es))", outcome.affected.len());
    }
    ramwarden_daemon::freeze_journal::write(&journal, &[]);

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
        // launchd and systemd stop a service with SIGTERM, which would kill the
        // process outright and skip the cleanup below.
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
}
