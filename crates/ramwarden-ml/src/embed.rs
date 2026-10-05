//! Sentence embeddings in-process, instead of through Ollama.
//!
//! The goal-aware tab ranking (`ramwarden_ai::rerank`) needs embeddings. On
//! Linux those came from Ollama in Docker; on Windows that means WSL2 and a
//! `vmmem` process holding gigabytes of RAM just to embed a few dozen tab
//! titles. This runs the same model — mxbai-embed-large, which the README
//! measured as the one that actually separates relevant from irrelevant tabs —
//! inside the daemon with candle: on the RTX 5070 when built with `cuda`
//! (about 1.3 GB of VRAM in f32), on the CPU otherwise.
//!
//! Weights come from the Hugging Face hub on first use and are cached.

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

/// The model the rest of RamWarden's ranking was tuned against.
pub const MXBAI: &str = "mixedbread-ai/mxbai-embed-large-v1";

pub struct LocalEmbedder {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl LocalEmbedder {
    /// Download (once) and load `repo` on `device`.
    pub fn load(repo: &str, device: &Device) -> Result<Self, String> {
        let api = hf_hub::api::sync::Api::new().map_err(err)?;
        let repo = api.model(repo.to_string());
        let config = repo.get("config.json").map_err(err)?;
        let tokenizer = repo.get("tokenizer.json").map_err(err)?;
        let weights = repo.get("model.safetensors").map_err(err)?;

        let config: Config = serde_json::from_str(&std::fs::read_to_string(config).map_err(err)?).map_err(err)?;
        let mut tokenizer = Tokenizer::from_file(tokenizer).map_err(err)?;
        tokenizer.with_padding(Some(PaddingParams::default()));
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: 512,
                ..Default::default()
            }))
            .map_err(err)?;

        // f32 everywhere. Half precision would halve the VRAM, but candle
        // 0.11's BERT returns NaN embeddings in both f16 and bf16 on the
        // RTX 5070 (f32 on the same GPU is correct), so the GPU path pays
        // ~1.3 GB of VRAM — on the card, not in system RAM, and released
        // after ten idle minutes.
        let dtype = DType::F32;
        // SAFETY: the file is a safetensors download from the hub, memory-
        // mapped read-only for the lifetime of the model.
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[weights], dtype, device).map_err(err)? };
        let model = BertModel::load(vb, &config).map_err(err)?;
        Ok(LocalEmbedder {
            model,
            tokenizer,
            device: device.clone(),
        })
    }

    /// One L2-normalised vector per text: the `[CLS]` token's final hidden
    /// state, which is how mxbai-embed-large is meant to be pooled.
    pub fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let enc = self.tokenizer.encode_batch(texts.to_vec(), true).map_err(err)?;
        let ids: Vec<Tensor> = enc
            .iter()
            .map(|e| Tensor::new(e.get_ids(), &self.device))
            .collect::<Result<_, _>>()
            .map_err(err)?;
        let mask: Vec<Tensor> = enc
            .iter()
            .map(|e| Tensor::new(e.get_attention_mask(), &self.device))
            .collect::<Result<_, _>>()
            .map_err(err)?;
        let ids = Tensor::stack(&ids, 0).map_err(err)?;
        let mask = Tensor::stack(&mask, 0).map_err(err)?;
        let types = ids.zeros_like().map_err(err)?;

        let hidden = self.model.forward(&ids, &types, Some(&mask)).map_err(err)?;
        let cls = hidden.narrow(1, 0, 1).and_then(|t| t.squeeze(1)).map_err(err)?;
        let cls = cls.to_dtype(DType::F32).map_err(err)?;
        let norm = cls
            .sqr()
            .and_then(|t| t.sum_keepdim(1))
            .and_then(|t| t.sqrt())
            .map_err(err)?;
        let unit = cls.broadcast_div(&norm).map_err(err)?;
        unit.to_vec2::<f32>().map_err(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Downloads ~1.3 GB on first run, so it is opt-in:
    /// `cargo test -p ramwarden-ml --features embed -- --ignored`.
    #[test]
    #[ignore]
    fn related_texts_are_closer_than_unrelated_ones() {
        let e = LocalEmbedder::load(MXBAI, &crate::jepa::device()).unwrap();
        let v = e
            .embed(&[
                "Represent this sentence for searching relevant passages: rust memory management".into(),
                "Rust ownership and borrowing explained".into(),
                "Best pizza places in Chicago".into(),
            ])
            .unwrap();
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert!((dot(&v[0], &v[0]) - 1.0).abs() < 1e-3, "normalised");
        assert!(dot(&v[0], &v[1]) > dot(&v[0], &v[2]) + 0.1, "{} vs {}", dot(&v[0], &v[1]), dot(&v[0], &v[2]));
    }
}
