//! The in-process embedder, loaded only on a GPU and only while it is used.
//!
//! On the RTX 5070 the model costs ~0.7 GB of VRAM in f16. On the CPU it would
//! cost ~1.3 GB of *RAM* for as long as it stayed loaded — a memory manager
//! holding a gigabyte to save a few hundred megabytes of tabs has lost the
//! plot — so without a CUDA device it is never attached and the Ollama path
//! stays in charge. On the GPU it loads on first use and is dropped again
//! after [`IDLE`] without a request.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ramwarden_ai::embedder::TextEmbedder;
use ramwarden_ai::provider::Provider;
use ramwarden_ml::embed::{LocalEmbedder, MXBAI};

/// Unload after this long without a request.
pub const IDLE: Duration = Duration::from_secs(600);

pub struct GpuEmbedder {
    device: candle_core::Device,
    slot: Mutex<Option<(Arc<LocalEmbedder>, Instant)>>,
}

impl GpuEmbedder {
    fn get(&self) -> Result<Arc<LocalEmbedder>, String> {
        let mut slot = self.slot.lock().unwrap();
        if let Some((e, at)) = slot.as_mut() {
            *at = Instant::now();
            return Ok(e.clone());
        }
        tracing::info!("loading {MXBAI} on {:?}", self.device);
        let e = Arc::new(LocalEmbedder::load(MXBAI, &self.device)?);
        *slot = Some((e.clone(), Instant::now()));
        Ok(e)
    }

    fn expire(&self) {
        let mut slot = self.slot.lock().unwrap();
        if slot.as_ref().is_some_and(|(_, at)| at.elapsed() >= IDLE) {
            *slot = None;
            tracing::info!("embedder idle for {} min — unloaded, VRAM returned", IDLE.as_secs() / 60);
        }
    }
}

impl TextEmbedder for GpuEmbedder {
    fn model(&self) -> &str {
        "mxbai-embed-large"
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        self.get()?.embed(texts)
    }
}

/// Attach a GPU embedder to `provider` when a CUDA device exists.
pub fn attach(provider: &Provider) {
    let device = ramwarden_ml::jepa::device();
    if !device.is_cuda() {
        tracing::info!("no CUDA device — tab relevance keeps using Ollama embeddings");
        return;
    }
    let e = Arc::new(GpuEmbedder {
        device,
        slot: Mutex::new(None),
    });
    let reaper = e.clone();
    let _ = std::thread::Builder::new()
        .name("ramwarden-embed-reaper".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(60));
                reaper.expire();
            }
        });
    if provider.local_embedder.set(e).is_ok() {
        tracing::info!("tab relevance will use {MXBAI} in-process on the GPU");
    }
}
