//! In-process-quality embeddings without CUDA in the daemon's process.
//!
//! Tab relevance uses mxbai-embed-large on the GPU, through the
//! `ramwarden-gpu embed` worker: started on the first request, kept for
//! follow-up requests, and gone — with its ~1.3 GB of VRAM and the whole CUDA
//! runtime — after ten idle minutes, when it exits by itself. Without the
//! worker (no GPU, or a CPU-only install) nothing is attached and the Ollama
//! path stays in charge.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

use ramwarden_ai::embedder::TextEmbedder;
use ramwarden_ai::provider::Provider;

struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct GpuEmbedder {
    exe: PathBuf,
    worker: Mutex<Option<Worker>>,
}

impl GpuEmbedder {
    fn spawn(&self) -> Result<Worker, String> {
        let mut cmd = Command::new(&self.exe);
        cmd.arg("embed").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn().map_err(|e| format!("start {}: {e}", self.exe.display()))?;
        Ok(Worker {
            stdin: child.stdin.take().ok_or("no stdin")?,
            stdout: BufReader::new(child.stdout.take().ok_or("no stdout")?),
            child,
        })
    }

    fn ask(w: &mut Worker, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        writeln!(w.stdin, "{}", serde_json::json!({"texts": texts})).map_err(|e| e.to_string())?;
        w.stdin.flush().map_err(|e| e.to_string())?;
        let mut line = String::new();
        if w.stdout.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err("worker exited".into());
        }
        let v: serde_json::Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
            return Err(e.to_string());
        }
        serde_json::from_value(v["vectors"].clone()).map_err(|e| e.to_string())
    }
}

impl TextEmbedder for GpuEmbedder {
    fn model(&self) -> &str {
        "mxbai-embed-large"
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let mut slot = self.worker.lock().unwrap();
        // A worker that idled out has closed its pipes: retry once with a
        // fresh one rather than reporting a failure the user never caused.
        for attempt in 0..2 {
            if slot.is_none() {
                *slot = Some(self.spawn()?);
            }
            match Self::ask(slot.as_mut().unwrap(), texts) {
                Ok(v) => return Ok(v),
                Err(e) if attempt == 0 => {
                    tracing::debug!("embedding worker gone ({e}); starting a new one");
                    *slot = None;
                }
                Err(e) => {
                    *slot = None;
                    return Err(e);
                }
            }
        }
        unreachable!()
    }
}

/// Attach the GPU worker's embedder to `provider`, when the worker is
/// installed and a GPU is present.
pub fn attach(provider: &Provider) {
    let Some(exe) = crate::worker::gpu_worker() else {
        tracing::info!("no GPU worker — tab relevance keeps using Ollama embeddings");
        return;
    };
    let e = Arc::new(GpuEmbedder {
        exe,
        worker: Mutex::new(None),
    });
    if provider.local_embedder.set(e).is_ok() {
        tracing::info!("tab relevance will use mxbai-embed-large on the GPU, via ramwarden-gpu");
    }
}
