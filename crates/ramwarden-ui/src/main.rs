//! The RamWarden window.
//!
//! Usage: ramwarden [--port 7823] [--host 127.0.0.1]

use ramwarden_ui::client::Client;

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("RAMWARDEN_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut port: u16 = 7823;
    let mut host = "127.0.0.1".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                if let Some(v) = args.next().and_then(|v| v.parse().ok()) {
                    port = v;
                }
            }
            "--host" => {
                if let Some(v) = args.next() {
                    host = v;
                }
            }
            "-h" | "--help" => {
                println!("ramwarden [--host HOST] [--port PORT]");
                return std::process::ExitCode::SUCCESS;
            }
            other => eprintln!("ignoring unknown argument {other:?}"),
        }
    }

    let client = match Client::new(&host, port) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("could not build a client: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    // Refuse to open an empty window against the wrong daemon. v1 answers
    // /health with a bare {"ok":true} and has no /state, so attaching to it
    // produced a second, identically-titled window with nothing in it.
    let runtime = match ramwarden_ui::runtime::network_runtime() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not start a runtime: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match runtime.block_on(client.verify()) {
        Ok(version) => tracing::info!("connected to RamWarden {version} at {}", client.base()),
        Err(e) => {
            eprintln!("{e}");
            return std::process::ExitCode::FAILURE;
        }
    }
    // GTK drives its own loop from here; the runtime stays alive for the
    // client's futures on GLib; its worker threads drive networking and timers.
    let _guard = runtime.enter();

    match ramwarden_ui::app::run(client) {
        code if code == gtk4::glib::ExitCode::SUCCESS => std::process::ExitCode::SUCCESS,
        _ => std::process::ExitCode::FAILURE,
    }
}
