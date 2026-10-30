//! `ramwarden-gpu`: the short-lived GPU worker.
//!
//! The daemon must stay small: a CUDA build of it held 640 MB private (cuBLAS
//! and the CUDA context load at startup and never leave), measured on the
//! target machine. So everything that wants the GPU runs here instead, in a
//! process the daemon starts when needed and that exits when done — taking
//! every byte of the CUDA runtime with it.
//!
//! ```text
//! ramwarden-gpu train --db <history.db> --out <predictor.safetensors> --horizon <secs>
//!     Train the reuse predictor on the GPU, save it, print metrics JSON, exit.
//! ramwarden-gpu embed
//!     JSON lines on stdin/stdout: {"texts":[..]} -> {"vectors":[[..]]}.
//!     Exits after ten minutes without a request.
//! ```

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use ramwarden_core::history::History;
use ramwarden_ml::embed::{LocalEmbedder, MXBAI};
use ramwarden_ml::features::examples;
use ramwarden_ml::jepa::{self, Jepa};

/// Exit after this long without an embedding request.
const IDLE: Duration = Duration::from_secs(600);
const EPOCHS: usize = 600;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn train(args: &[String]) -> Result<(), String> {
    let db = PathBuf::from(arg(args, "--db").ok_or("--db is required")?);
    let out = PathBuf::from(arg(args, "--out").ok_or("--out is required")?);
    let horizon: i64 = arg(args, "--horizon").and_then(|h| h.parse().ok()).unwrap_or(1800);

    let history = History::open(&db).map_err(|e| e.to_string())?;
    let rows = history
        .return_labels(horizon, ramwarden_core::signals::CPU_BUSY_SECONDS)
        .map_err(|e| e.to_string())?;
    let ex = examples(&rows);
    if ex.len() < jepa::MIN_TRAIN {
        return Err(format!("{} usable examples; training needs {}", ex.len(), jepa::MIN_TRAIN));
    }
    let model = Jepa::train(&ex, EPOCHS, &jepa::device()).map_err(|e| e.to_string())?;
    model.save(&out)?;
    println!("{}", serde_json::to_string(&model.metrics).map_err(|e| e.to_string())?);
    Ok(())
}

fn embed() -> Result<(), String> {
    let embedder = LocalEmbedder::load(MXBAI, &jepa::device())?;
    // Read on a thread so the main loop can time out and exit when idle.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut stdout = std::io::stdout().lock();
    loop {
        let line = match rx.recv_timeout(IDLE) {
            Ok(l) => l,
            // Idle, or the daemon closed our stdin: either way we are done.
            Err(_) => return Ok(()),
        };
        // Windows PowerShell can prefix piped input with a byte-order mark.
        let line = line.trim_start_matches('\u{feff}');
        let reply = match serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|v| serde_json::from_value::<Vec<String>>(v["texts"].clone()).ok())
        {
            Some(texts) => match embedder.embed(&texts) {
                Ok(v) => serde_json::json!({"vectors": v}),
                Err(e) => serde_json::json!({"error": e}),
            },
            None => serde_json::json!({"error": "expected {\"texts\": [...]}"}),
        };
        writeln!(stdout, "{reply}").map_err(|e| e.to_string())?;
        stdout.flush().map_err(|e| e.to_string())?;
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("train") => train(&args),
        Some("embed") => embed(),
        _ => Err("usage: ramwarden-gpu train --db <path> --out <path> [--horizon secs] | embed".into()),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
