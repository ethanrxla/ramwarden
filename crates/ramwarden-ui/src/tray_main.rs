//! `ramwarden-tray`: the Windows tray icon; `--window` is the window process
//! it launches, `--hidden` starts in the tray without opening the window.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("RAMWARDEN_LOG").unwrap_or_else(|_| "warn".into()),
        )
        .init();
    let port = std::env::var("RAMWARDEN_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(7823);
    let client = match ramwarden_ui::client::Client::new("127.0.0.1", port) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let args: Vec<String> = std::env::args().collect();
    let result = if args.iter().any(|a| a == "--window") {
        ramwarden_ui::tray::run_window(client)
    } else {
        ramwarden_ui::tray::run_tray(client, args.iter().any(|a| a == "--hidden"))
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
