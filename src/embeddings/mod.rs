//! Embedding runtime, trusted model registry, and model management (M2).
//!
//! Everything here is feature-gated on `embeddings`. With the feature
//! disabled, only the manifest and pure-data types are available and the
//! runtime returns a controlled "not available" error.

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
