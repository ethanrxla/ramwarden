//! The reuse predictor, live.
//!
//! Keeps each process's last [`WINDOW`] behaviour samples (the same samples,
//! at the same cadence, as the behaviour log the model trained on), scores
//! every process in one batched pass whenever new samples arrive — on the GPU
//! when built with `cuda` — and answers the ladder's `reuse(pid)` from that.
//!
//! It only answers at all while the loaded model passed its gate (beat the
//! LRU baseline on held-out data). Until then, the ladder and Tidy behave
//! exactly as if there were no predictor.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use ramwarden_core::history::{History, SignalSample};
use ramwarden_core::rank::ReuseRanker;
use ramwarden_ml::features::{FEATURES, WINDOW, encode, examples};
use ramwarden_ml::jepa::{self, Jepa, Metrics};

/// Training passes. Early stopping picks the best checkpoint within these.
const EPOCHS: usize = 600;

#[derive(Default)]
struct Inner {
    model: Option<Jepa>,
    /// The last metrics, including from a model that did not pass the gate.
    metrics: Option<Metrics>,
    windows: HashMap<(i32, String), VecDeque<[f32; FEATURES]>>,
    scores: HashMap<i32, f64>,
}

pub struct LivePredictor {
    path: PathBuf,
    inner: RwLock<Inner>,
}

impl LivePredictor {
    /// Load a previously trained model from `path`, if one exists.
    pub fn new(path: PathBuf) -> Self {
        let mut inner = Inner::default();
        if path.exists() {
            match Jepa::load(&path, &jepa::device()) {
                Ok(m) => {
                    tracing::info!(
                        "reuse predictor loaded (AUC {:?} vs LRU {:?}, {})",
                        m.metrics.auc,
                        m.metrics.lru_auc,
                        if m.metrics.enabled { "enabled" } else { "gated off" }
                    );
                    inner.metrics = Some(m.metrics.clone());
                    inner.model = Some(m);
                }
                Err(e) => tracing::warn!("reuse predictor at {} unreadable: {e}", path.display()),
            }
        }
        LivePredictor {
            path,
            inner: RwLock::new(inner),
        }
    }

    pub fn model_path(&self) -> &Path {
        &self.path
    }

    pub fn metrics(&self) -> Option<Metrics> {
        self.inner.read().unwrap().metrics.clone()
    }

    /// Fold in one behaviour-log batch and rescore every process.
    pub fn observe(&self, samples: &[SignalSample], epoch: i64, psi_some: f64) {
        let mut inner = self.inner.write().unwrap();
        let live: std::collections::HashSet<(i32, String)> =
            samples.iter().map(|s| (s.pid, s.name.clone())).collect();
        inner.windows.retain(|k, _| live.contains(k));
        for s in samples {
            let w = inner.windows.entry((s.pid, s.name.clone())).or_default();
            w.push_back(encode(s, epoch, psi_some));
            while w.len() > WINDOW {
                w.pop_front();
            }
        }

        let Some(model) = inner.model.as_ref().filter(|m| m.metrics.enabled) else {
            inner.scores.clear();
            return;
        };
        let full: Vec<(i32, Vec<f32>)> = inner
            .windows
            .iter()
            .filter(|(_, w)| w.len() == WINDOW)
            .map(|((pid, _), w)| (*pid, w.iter().flatten().copied().collect()))
            .collect();
        let rows: Vec<&[f32]> = full.iter().map(|(_, v)| v.as_slice()).collect();
        match model.score(&rows) {
            Ok(p) => {
                inner.scores = full.iter().map(|(pid, _)| *pid).zip(p.into_iter().map(f64::from)).collect();
            }
            Err(e) => tracing::warn!("reuse scoring failed: {e}"),
        }
    }

    /// Train from the behaviour log, save, and swap the model in.
    ///
    /// A model that fails the gate is still saved, so `/predict/eval` can
    /// show how close it came, but it never answers `reuse`.
    pub fn train(&self, history: &History, horizon_secs: i64) -> Result<Metrics, String> {
        let rows = history
            .return_labels(horizon_secs, ramwarden_core::signals::CPU_BUSY_SECONDS)
            .map_err(|e| e.to_string())?;
        let ex = examples(&rows);
        if ex.len() < jepa::MIN_TRAIN {
            return Err(format!(
                "{} usable examples so far; training needs {} (the behaviour log fills at one sample per process per logging interval)",
                ex.len(),
                jepa::MIN_TRAIN
            ));
        }
        let device = jepa::device();
        let model = Jepa::train(&ex, EPOCHS, &device).map_err(|e| e.to_string())?;
        model.save(&self.path)?;
        let metrics = model.metrics.clone();
        tracing::info!(
            "reuse predictor trained on {:?}: AUC {:?} vs LRU {:?} — {}",
            device,
            metrics.auc,
            metrics.lru_auc,
            if metrics.enabled { "enabled" } else { "not better than LRU; gated off" }
        );
        let mut inner = self.inner.write().unwrap();
        inner.metrics = Some(metrics.clone());
        inner.model = Some(model);
        Ok(metrics)
    }
}

impl ReuseRanker for LivePredictor {
    fn reuse(&self, pid: i32) -> Option<f64> {
        self.inner.read().unwrap().scores.get(&pid).copied()
    }
}

/// Train on a background thread with its own history connection.
pub fn train_in_background(state: &crate::hub::AppState) {
    let predictor = state.predictor.clone();
    let db = state.cfg.db_path.clone();
    let horizon = state.cfg.logging.horizon_seconds;
    let _ = std::thread::Builder::new()
        .name("ramwarden-train".into())
        .spawn(move || match History::open(&db) {
            Ok(h) => {
                if let Err(e) = predictor.train(&h, horizon) {
                    tracing::info!("reuse predictor not trained: {e}");
                }
            }
            Err(e) => tracing::warn!("reuse predictor: cannot open history: {e}"),
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_model_there_is_no_opinion() {
        let dir = tempfile::tempdir().unwrap();
        let p = LivePredictor::new(dir.path().join("none.safetensors"));
        p.observe(
            &[SignalSample {
                pid: 7,
                name: "x".into(),
                ..Default::default()
            }],
            0,
            0.0,
        );
        assert_eq!(p.reuse(7), None);
        assert!(p.metrics().is_none());
    }

    #[test]
    fn training_on_an_empty_log_says_why_it_cannot() {
        let dir = tempfile::tempdir().unwrap();
        let p = LivePredictor::new(dir.path().join("p.safetensors"));
        let h = History::in_memory().unwrap();
        let e = p.train(&h, 1800).unwrap_err();
        assert!(e.contains("training needs"), "{e}");
    }
}
