//! An embedder that runs in this process rather than behind a server.
//!
//! Defined here, implemented elsewhere: `ramwarden-ml` provides a candle-based
//! one, and the daemon attaches it to the [`crate::provider::Provider`] when a
//! GPU is available. Keeping only the trait in this crate means the model tier
//! does not pull a tensor library into every build.

/// Turns texts into vectors. Blocking: callers run it off the async runtime.
pub trait TextEmbedder: Send + Sync {
    /// The model name, which decides query/document prefixes
    /// ([`crate::rerank::prefix_query`]).
    fn model(&self) -> &str;

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String>;
}
