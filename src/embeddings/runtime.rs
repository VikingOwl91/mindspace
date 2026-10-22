//! Candle-backed embedding runtime (feature = "embeddings").

use std::path::Path;
use std::sync::Mutex;

use candle_core::{DType, Device, Tensor};
use candle_transformers::models::nomic_bert::{self, Config, NomicBertModel};

use crate::core::{Error, Result};
use crate::embeddings::EmbeddingRuntime;

pub struct CandleRuntime {
    model_id: String,
    dimension: usize,
    prefix_document: String,
    prefix_query: String,
    max_seq_len: usize,
    inner: Mutex<Option<Loaded>>,
}

struct Loaded {
    model: NomicBertModel,
    tokenizer: tokenizers::Tokenizer,
}

impl CandleRuntime {
    pub fn new(entry: &crate::embeddings::manifest::ModelManifestEntry) -> Self {
        Self {
            model_id: entry.id.to_string(),
            dimension: entry.dimension as usize,
            prefix_document: entry.prefix_document.to_string(),
            prefix_query: entry.prefix_query.to_string(),
            max_seq_len: entry.max_seq_len as usize,
            inner: Mutex::new(None),
        }
    }

    /// Load from a model directory. Uses the default (Nomic) manifest entry.
    pub fn load(model_dir: &Path) -> Result<Self> {
        // find matching manifest entry by dir name
        let id = model_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("nomic-embed-text-v1.5");
        let entry = crate::embeddings::manifest::find_model(id)
            .copied()
            .unwrap_or(crate::embeddings::manifest::NOMIC_V1_5);
        let rt = Self::new(&entry);
        // eager-load to validate artifacts now
        rt.ensure_loaded(model_dir)?;
        Ok(rt)
    }

    fn ensure_loaded(&self, model_dir: &Path) -> Result<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| Error::Storage(e.to_string()))?;
        if guard.is_some() {
            return Ok(());
        }
        let config = Config {
            vocab_size: 30528,
            n_embd: self.dimension,
            n_head: 12,
            n_layer: 12,
            n_inner: 3072,
            n_positions: 8192,
            type_vocab_size: 2,
            layer_norm_epsilon: 1e-12,
            rotary_emb_fraction: 1.0,
            rotary_emb_base: 1000.0,
            rotary_emb_interleaved: false,
            qkv_proj_bias: false,
            mlp_fc1_bias: false,
            mlp_fc2_bias: false,
            activation_function: "swiglu".to_string(),
            prenorm: false,
            model_type: Some("nomic_bert".to_string()),
        };
        let st_path = model_dir.join("model.safetensors");
        let tok_path = model_dir.join("tokenizer.json");
        if !st_path.is_file() || !tok_path.is_file() {
            return Err(Error::Storage(format!(
                "missing model artifacts in {}",
                model_dir.display()
            )));
        }
        let device = Device::Cpu;
        let tensors = candle_core::safetensors::load(&st_path, &device)
            .map_err(|e| Error::Storage(e.to_string()))?;
        let vb = candle_nn::VarBuilder::from_tensors(tensors, DType::F32, &device);
        let model = NomicBertModel::load(vb, &config).map_err(|e| Error::Storage(e.to_string()))?;
        let tokenizer = tokenizers::Tokenizer::from_file(&tok_path)
            .map_err(|e| Error::Storage(e.to_string()))?;
        *guard = Some(Loaded { model, tokenizer });
        Ok(())
    }

    fn encode(&self, text: &str, is_query: bool) -> Result<Vec<f32>> {
        // model_dir is not stored; re-derive from id
        let model_dir = crate::embeddings::manager::model_dir(&self.model_id);
        self.ensure_loaded(&model_dir)?;
        let guard = self
            .inner
            .lock()
            .map_err(|e| Error::Storage(e.to_string()))?;
        let loaded = guard
            .as_ref()
            .ok_or_else(|| Error::Storage("model not loaded".to_string()))?;
        let prefix = if is_query {
            &self.prefix_query
        } else {
            &self.prefix_document
        };
        let full = format!("{prefix}{text}");
        let encoding = loaded
            .tokenizer
            .encode(full.as_str(), true)
            .map_err(|e| Error::Storage(e.to_string()))?;
        let mut ids = encoding.get_ids().to_vec();
        let mut mask = encoding.get_attention_mask().to_vec();
        if ids.len() > self.max_seq_len {
            ids.truncate(self.max_seq_len);
            mask.truncate(self.max_seq_len);
        }
        if ids.is_empty() {
            return Err(Error::InvalidVector {
                detail: "empty tokenized input".to_string(),
            });
        }
        let seq = ids.len();
        let device = Device::Cpu;
        let input_ids = Tensor::new(ids.as_slice(), &device)
            .map_err(|e| Error::Storage(e.to_string()))?
            .reshape((1, seq))
            .map_err(|e| Error::Storage(e.to_string()))?;
        let attention_mask = Tensor::new(mask.as_slice(), &device)
            .map_err(|e| Error::Storage(e.to_string()))?
            .reshape((1, seq))
            .map_err(|e| Error::Storage(e.to_string()))?;
        let hidden = loaded
            .model
            .forward(&input_ids, None, Some(&attention_mask))
            .map_err(|e| Error::Storage(e.to_string()))?;
        let pooled = nomic_bert::mean_pooling(&hidden, &attention_mask)
            .map_err(|e| Error::Storage(e.to_string()))?;
        let normed =
            nomic_bert::l2_normalize(&pooled).map_err(|e| Error::Storage(e.to_string()))?;
        let vec = normed
            .squeeze(0)
            .map_err(|e| Error::Storage(e.to_string()))?
            .to_vec1::<f32>()
            .map_err(|e| Error::Storage(e.to_string()))?;
        // validate
        if vec.len() != self.dimension {
            return Err(Error::InvalidVector {
                detail: format!(
                    "dimension mismatch: got {}, expected {}",
                    vec.len(),
                    self.dimension
                ),
            });
        }
        for v in &vec {
            if !v.is_finite() {
                return Err(Error::InvalidVector {
                    detail: "non-finite embedding output".to_string(),
                });
            }
        }
        Ok(vec)
    }
}

impl EmbeddingRuntime for CandleRuntime {
    fn embed_document(&self, text: &str) -> Result<Vec<f32>> {
        self.encode(text, false)
    }
    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.encode(text, true)
    }
    fn dimension(&self) -> usize {
        self.dimension
    }
    fn model_id(&self) -> &str {
        &self.model_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "embeddings")]
    fn lazy_load_not_triggered_on_construction() {
        // Constructing a runtime must not require artifacts to be present.
        let rt = CandleRuntime::new(&crate::embeddings::manifest::NOMIC_V1_5);
        assert_eq!(rt.dimension(), 768);
        assert_eq!(rt.model_id(), "nomic-embed-text-v1.5");
    }
}
