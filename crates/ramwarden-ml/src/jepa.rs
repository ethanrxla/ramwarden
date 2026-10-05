//! A JEPA-style predictor of "will the user come back to this?".
//!
//! # Shape
//!
//! Joint-Embedding Predictive Architecture, at desktop scale:
//!
//! ```text
//!  context window ──▶ context encoder ──▶ z ──▶ predictor ──▶ ẑ ─┐
//!                                          │                     ├─ MSE (in embedding space)
//!  next window ─────▶ target encoder ──▶ z′ (stop-gradient) ─────┘
//!                     (EMA of the context encoder)
//!                                          │
//!                                          └─▶ probe ──▶ P(user returns within the horizon)
//! ```
//!
//! The encoder learns a representation of a process's recent behaviour by
//! predicting the representation of its *next* behaviour, rather than its raw
//! features — the JEPA idea: predict what matters, not every detail. A linear
//! probe on top answers the question RamWarden actually asks.
//!
//! # The gate
//!
//! A model that has not beaten the obvious baseline must not influence
//! anything. Training holds out the most recent 20% of examples (by time, so
//! nothing from the future leaks into training) and compares the probe's AUC
//! with the LRU baseline's — "recently used means used again". The predictor
//! is [`Metrics::enabled`] only with enough data *and* a margin over LRU.
//!
//! # GPU batch scoring
//!
//! Scoring is one batched forward pass over every candidate process — encoder
//! matmuls and the probe's sigmoid — on whatever [`Device`] the model lives on:
//! the RTX 5070 when built with the `cuda` feature, the CPU otherwise.

use std::path::Path;

use candle_core::{DType, Device, Tensor};
use candle_nn::{AdamW, Linear, Module, Optimizer, ParamsAdamW, VarBuilder, VarMap, linear, loss};
use serde::{Deserialize, Serialize};

use crate::features::{Example, FEATURES, WINDOW, auc};

const HIDDEN: usize = 64;
const EMBED: usize = 32;
/// EMA momentum for the target encoder.
const MOMENTUM: f64 = 0.99;
/// Fewer training examples than this is not evidence of anything.
pub const MIN_TRAIN: usize = 300;
/// The probe must beat LRU by at least this AUC margin to be enabled.
pub const MARGIN: f64 = 0.02;
/// Validation interval for early stopping, in epochs.
pub const CHECK_EVERY: usize = 25;

pub type Result<T> = std::result::Result<T, candle_core::Error>;

struct Encoder {
    l1: Linear,
    l2: Linear,
}

impl Encoder {
    fn new(vb: VarBuilder) -> Result<Self> {
        Ok(Encoder {
            l1: linear(WINDOW * FEATURES, HIDDEN, vb.pp("l1"))?,
            l2: linear(HIDDEN, EMBED, vb.pp("l2"))?,
        })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        self.l2.forward(&self.l1.forward(x)?.relu()?)
    }
}

struct Mlp {
    l1: Linear,
    l2: Linear,
}

impl Mlp {
    fn new(vb: VarBuilder, out: usize) -> Result<Self> {
        Ok(Mlp {
            l1: linear(EMBED, HIDDEN, vb.pp("l1"))?,
            l2: linear(HIDDEN, out, vb.pp("l2"))?,
        })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        self.l2.forward(&self.l1.forward(x)?.relu()?)
    }
}

/// How the last training run went.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    pub n_train: usize,
    pub n_test: usize,
    /// Probe AUC on the held-out, most recent examples.
    pub auc: Option<f64>,
    /// LRU baseline AUC on the same examples.
    pub lru_auc: Option<f64>,
    pub final_loss: f32,
    /// Whether the predictor may influence decisions.
    pub enabled: bool,
    pub device: String,
}

impl Metrics {
    fn decide(&mut self) {
        self.enabled = self.n_train >= MIN_TRAIN
            && matches!((self.auc, self.lru_auc), (Some(m), Some(b)) if m >= b + MARGIN);
    }
}

/// The trained model.
pub struct Jepa {
    device: Device,
    vars: VarMap,
    encoder: Encoder,
    probe: Linear,
    pub metrics: Metrics,
}

/// The device to use: the first CUDA GPU when built with `cuda` and one is
/// present, otherwise the CPU.
pub fn device() -> Device {
    #[cfg(feature = "cuda")]
    if let Ok(d) = Device::new_cuda(0) {
        return d;
    }
    Device::Cpu
}

fn tensor(rows: &[&[f32]], width: usize, device: &Device) -> Result<Tensor> {
    let flat: Vec<f32> = rows.iter().flat_map(|r| r.iter().copied()).collect();
    Tensor::from_vec(flat, (rows.len(), width), device)
}

impl Jepa {
    fn build(device: &Device) -> Result<(VarMap, Encoder, Mlp, Linear)> {
        let vars = VarMap::new();
        let vb = VarBuilder::from_varmap(&vars, DType::F32, device);
        let encoder = Encoder::new(vb.pp("encoder"))?;
        let predictor = Mlp::new(vb.pp("predictor"), EMBED)?;
        let probe = linear(EMBED, 1, vb.pp("probe"))?;
        Ok((vars, encoder, predictor, probe))
    }

    /// Train on `examples` (time-ordered), holding out the newest 20% for the
    /// final comparison with LRU.
    ///
    /// Within the training part, the newest 10% is a validation set: the
    /// weights kept are the ones that scored best on it, checked every
    /// [`CHECK_EVERY`] epochs. Full-batch training on a small model can
    /// overshoot and collapse late; early stopping makes the result
    /// independent of picking exactly the right epoch count.
    pub fn train(examples: &[Example], epochs: usize, device: &Device) -> Result<Self> {
        let split = examples.len() * 4 / 5;
        let (train_all, test) = examples.split_at(split);
        let fit_end = train_all.len() * 9 / 10;
        let (fit, val) = train_all.split_at(fit_end);

        let (vars, encoder, predictor, probe) = Self::build(device)?;
        // The target encoder: same shape, its own variables, updated only by EMA.
        let target_vars = VarMap::new();
        let target = Encoder::new(VarBuilder::from_varmap(&target_vars, DType::F32, device).pp("encoder"))?;
        copy_encoder(&vars, &target_vars, 0.0)?;

        let mut opt = AdamW::new(
            vars.all_vars(),
            ParamsAdamW {
                lr: 1e-3,
                weight_decay: 1e-4,
                ..Default::default()
            },
        )?;

        let mut model = Jepa {
            device: device.clone(),
            vars,
            encoder,
            probe,
            metrics: Metrics::default(),
        };

        let mut final_loss = f32::NAN;
        if !fit.is_empty() {
            let fit_ctx: Vec<&[f32]> = fit.iter().map(|e| e.context.as_slice()).collect();
            let fit_tgt: Vec<&[f32]> = fit.iter().map(|e| e.target.as_slice()).collect();
            let ctx = tensor(&fit_ctx, WINDOW * FEATURES, device)?;
            let tgt = tensor(&fit_tgt, WINDOW * FEATURES, device)?;
            let y: Vec<f32> = fit.iter().map(|e| e.returned as u8 as f32).collect();
            let y = Tensor::from_vec(y, (fit.len(), 1), device)?;
            let val_ctx: Vec<&[f32]> = val.iter().map(|e| e.context.as_slice()).collect();
            let val_y: Vec<bool> = val.iter().map(|e| e.returned).collect();

            let mut best: Option<(f64, Vec<(String, Tensor)>)> = None;
            for epoch in 1..=epochs {
                let z = model.encoder.forward(&ctx)?;
                let z_target = target.forward(&tgt)?.detach();
                let jepa = loss::mse(&predictor.forward(&z)?, &z_target)?;
                let bce = loss::binary_cross_entropy_with_logit(&model.probe.forward(&z)?, &y)?;
                let total = (jepa + bce)?;
                opt.backward_step(&total)?;
                copy_encoder(&model.vars, &target_vars, MOMENTUM)?;
                final_loss = total.to_scalar::<f32>()?;

                if epoch % CHECK_EVERY == 0 || epoch == epochs {
                    let score = model.score(&val_ctx).ok().and_then(|s| auc(&s, &val_y)).unwrap_or(0.5);
                    if best.as_ref().is_none_or(|(b, _)| score > *b) {
                        best = Some((score, snapshot(&model.vars)?));
                    }
                }
            }
            if let Some((_, weights)) = best {
                restore(&model.vars, &weights)?;
            }
        }

        let scores = model.score(&test.iter().map(|e| e.context.as_slice()).collect::<Vec<_>>())?;
        let labels: Vec<bool> = test.iter().map(|e| e.returned).collect();
        let lru: Vec<f32> = test.iter().map(|e| e.lru).collect();
        model.metrics = Metrics {
            n_train: train_all.len(),
            n_test: test.len(),
            auc: auc(&scores, &labels),
            lru_auc: auc(&lru, &labels),
            final_loss,
            enabled: false,
            device: format!("{:?}", device),
        };
        model.metrics.decide();
        Ok(model)
    }

    /// P(user returns) for each context window, in one batched pass.
    pub fn score(&self, contexts: &[&[f32]]) -> Result<Vec<f32>> {
        if contexts.is_empty() {
            return Ok(Vec::new());
        }
        let x = tensor(contexts, WINDOW * FEATURES, &self.device)?;
        let logits = self.probe.forward(&self.encoder.forward(&x)?)?;
        candle_nn::ops::sigmoid(&logits)?.flatten_all()?.to_vec1::<f32>()
    }

    /// Save weights (safetensors) and metrics (JSON) side by side.
    pub fn save(&self, path: &Path) -> std::result::Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        self.vars.save(path).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(&self.metrics).map_err(|e| e.to_string())?;
        std::fs::write(path.with_extension("json"), json).map_err(|e| e.to_string())
    }

    pub fn load(path: &Path, device: &Device) -> std::result::Result<Self, String> {
        let (mut vars, encoder, _predictor, probe) = Self::build(device).map_err(|e| e.to_string())?;
        vars.load(path).map_err(|e| e.to_string())?;
        let metrics = std::fs::read_to_string(path.with_extension("json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Ok(Jepa {
            device: device.clone(),
            vars,
            encoder,
            probe,
            metrics,
        })
    }
}

/// A copy of every variable's current value.
fn snapshot(vars: &VarMap) -> Result<Vec<(String, Tensor)>> {
    vars.data()
        .lock()
        .unwrap()
        .iter()
        .map(|(k, v)| Ok((k.clone(), v.as_tensor().copy()?)))
        .collect()
}

fn restore(vars: &VarMap, weights: &[(String, Tensor)]) -> Result<()> {
    let data = vars.data().lock().unwrap();
    for (k, t) in weights {
        if let Some(v) = data.get(k) {
            v.set(t)?;
        }
    }
    Ok(())
}

/// target ← momentum·target + (1 − momentum)·context, for the encoder's
/// variables. `momentum = 0` copies.
fn copy_encoder(from: &VarMap, to: &VarMap, momentum: f64) -> Result<()> {
    let src = from.data().lock().unwrap();
    let dst = to.data().lock().unwrap();
    for (name, var) in dst.iter() {
        if let Some(s) = src.get(name) {
            let mixed = ((var.as_tensor() * momentum)? + (s.as_tensor().detach() * (1.0 - momentum))?)?;
            var.set(&mixed)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::{FEATURES, examples};
    use ramwarden_core::history::{LabelledSample, SampleContext, SignalSample};

    /// Ten days of 5-minute samples for a few apps. `morning` is opened every
    /// day at 09:00 and closed by 09:30: before 9 it has been idle all night,
    /// so recency says "won't return" — but the time of day says otherwise.
    /// `noise` is used at random. A model that learns the routine beats LRU.
    fn routine() -> Vec<LabelledSample> {
        let mut rows = Vec::new();
        let mut rng = 12345u64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        const STEP: i64 = 300;
        const HORIZON: i64 = 1800;
        for (pid, name) in [(1, "morning"), (2, "noise"), (3, "evening")] {
            let mut active = Vec::new();
            for day in 0..10i64 {
                for slot in 0..288i64 {
                    let t = day * 86_400 + slot * STEP;
                    let hour = slot as f64 * STEP as f64 / 3600.0;
                    let on = match name {
                        "morning" => (9.0..9.5).contains(&hour),
                        "evening" => (20.0..20.5).contains(&hour),
                        _ => next() % 10 == 0,
                    };
                    active.push((t, on));
                }
            }
            for (i, (t, on)) in active.iter().enumerate() {
                let returned = active[i + 1..]
                    .iter()
                    .take_while(|(t2, _)| *t2 <= t + HORIZON)
                    .any(|(_, a)| *a);
                rows.push(LabelledSample {
                    epoch: *t,
                    sample: SignalSample {
                        pid,
                        name: name.into(),
                        pss: 400 * 1_048_576,
                        age_minutes: 600.0,
                        focused: *on,
                        windowed: true,
                        verdict: if *on { "IN_USE".into() } else { "IDLE".into() },
                        ..Default::default()
                    },
                    context: SampleContext::default(),
                    returned,
                });
            }
        }
        rows
    }

    #[test]
    fn a_daily_routine_is_learned_and_beats_recency() {
        let ex = examples(&routine());
        assert!(ex.len() > 5000);
        let device = Device::Cpu;
        let m = Jepa::train(&ex, 300, &device).unwrap();
        let (auc, lru) = (m.metrics.auc.unwrap(), m.metrics.lru_auc.unwrap());
        eprintln!("routine: model AUC {auc:.3}, LRU AUC {lru:.3}, {} train / {} test", m.metrics.n_train, m.metrics.n_test);
        assert!(auc > lru + MARGIN, "model {auc:.3} vs LRU {lru:.3}");
        assert!(m.metrics.enabled, "{:?}", m.metrics);
    }

    /// With too little data the gate stays shut whatever the scores say.
    #[test]
    fn too_little_data_never_enables_the_predictor() {
        let ex = examples(&routine());
        let m = Jepa::train(&ex[..200], 20, &Device::Cpu).unwrap();
        assert!(!m.metrics.enabled);
    }

    #[test]
    fn scores_are_probabilities_and_survive_a_save_and_load() {
        let ex = examples(&routine());
        let m = Jepa::train(&ex[..1000], 30, &Device::Cpu).unwrap();
        let ctx: Vec<&[f32]> = ex[..10].iter().map(|e| e.context.as_slice()).collect();
        let before = m.score(&ctx).unwrap();
        assert!(before.iter().all(|p| (0.0..=1.0).contains(p)));
        assert_eq!(ctx[0].len(), WINDOW * FEATURES);

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("predictor.safetensors");
        m.save(&p).unwrap();
        let loaded = Jepa::load(&p, &Device::Cpu).unwrap();
        // JSON round-trips f64 to within an ulp, not bit-exactly.
        assert_eq!(loaded.metrics.enabled, m.metrics.enabled);
        assert_eq!(loaded.metrics.n_train, m.metrics.n_train);
        assert!((loaded.metrics.auc.unwrap() - m.metrics.auc.unwrap()).abs() < 1e-12);
        let after = loaded.score(&ctx).unwrap();
        for (a, b) in before.iter().zip(&after) {
            assert!((a - b).abs() < 1e-5);
        }
    }
}
