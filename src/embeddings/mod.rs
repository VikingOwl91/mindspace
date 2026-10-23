//! Embedding runtime, trusted model registry, and model management (M2).
//!
//! The manifest, the model manager, and the runtime trait compile in every
//! build. Only the Candle runtime is gated on `embeddings`. Without the feature,
//! `CandleRuntime::load` returns a controlled "not available" error.

pub mod manager;
pub mod manifest;

#[cfg(feature = "embeddings")]
pub mod runtime;

use crate::core::Result;

/// Minimal runtime trait. Independent of SQLite and of any specific backend.
pub trait EmbeddingRuntime: Send + Sync {
    fn embed_document(&self, text: &str) -> Result<Vec<f32>>;
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;
    fn dimension(&self) -> usize;
    fn model_id(&self) -> &str;
}

#[cfg(feature = "embeddings")]
pub use runtime::CandleRuntime;

#[cfg(not(feature = "embeddings"))]
pub struct CandleRuntime;

#[cfg(not(feature = "embeddings"))]
impl CandleRuntime {
    pub fn load(_model_dir: &std::path::Path) -> Result<Self> {
        Err(crate::Error::Storage(
            "embeddings feature is disabled; rebuild with --features embeddings".to_string(),
        ))
    }
}
