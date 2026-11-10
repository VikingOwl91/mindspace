//! Token budget for the supported, pinned embedding tokenizer. No weights or inference needed.
//!
//! The limit includes the model-specific prefix AND tokenizer special tokens. Counts always encode
//! the complete input; internal truncation and padding are disabled. The tokenizer's own normalizer
//! (including WordPiece's long-word `[UNK]` behavior) is model behavior, not source normalization.

use std::io::Read;
use std::path::Path;

use crate::core::{sha256_hex, Error, Result};
use crate::embeddings::manifest::{self, ModelManifestEntry};

#[derive(Clone)]
pub struct TokenBudget {
    tokenizer: tokenizers::Tokenizer,
    entry: ModelManifestEntry,
    identity: String,
}

impl TokenBudget {
    /// Read only the tokenizer, verifying the compiled-in artifact identity before parsing it.
    /// Arbitrary model configurations are refused: the registry is the authority for limits.
    pub fn from_file(path: &Path, entry: &ModelManifestEntry) -> Result<Self> {
        if manifest::find_model(entry.id) != Some(entry) {
            return Err(Error::InvalidVector {
                detail: "unsupported embedding token-budget configuration".into(),
            });
        }
        let artifact = entry
            .artifacts
            .iter()
            .find(|a| a.filename == "tokenizer.json")
            .ok_or_else(|| Error::InvalidVector {
                detail: "model has no tokenizer artifact".into(),
            })?;
        let file = std::fs::File::open(path).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })?;
        let mut bytes = Vec::with_capacity(artifact.size as usize);
        file.take(artifact.size + 1)
            .read_to_end(&mut bytes)
            .map_err(|source| Error::Io {
                path: path.display().to_string(),
                source,
            })?;
        if bytes.len() as u64 != artifact.size || sha256_hex(&bytes) != artifact.sha256 {
            return Err(Error::InvalidVector {
                detail: "tokenizer artifact does not match the pinned manifest".into(),
            });
        }
        let mut tokenizer =
            tokenizers::Tokenizer::from_bytes(&bytes).map_err(|e| Error::InvalidVector {
                detail: e.to_string(),
            })?;
        tokenizer
            .with_truncation(None)
            .map_err(|e| Error::InvalidVector {
                detail: e.to_string(),
            })?;
        tokenizer.with_padding(None);
        let identity = sha256_hex(
            serde_json::json!({
                "contract": "complete-input@1", "engine": "tokenizers@0.23.2",
                "model": entry.id, "revision": entry.revision,
                "tokenizer": artifact.sha256, "limit": entry.max_seq_len,
                "document_prefix": entry.prefix_document, "query_prefix": entry.prefix_query,
                "special_tokens": true, "padding": false, "truncation": false,
            })
            .to_string()
            .as_bytes(),
        );
        Ok(Self {
            tokenizer,
            entry: *entry,
            identity,
        })
    }

    pub fn load_default() -> Result<Self> {
        let entry = manifest::default_model();
        Self::from_file(
            &crate::embeddings::manager::model_dir(entry.id)?.join("tokenizer.json"),
            entry,
        )
    }

    /// Includes every rule that can change counting; chunk identities must include this value.
    pub fn identity(&self) -> &str {
        &self.identity
    }
    pub fn limit(&self) -> usize {
        self.entry.max_seq_len as usize
    }
    pub fn model_id(&self) -> &str {
        self.entry.id
    }

    fn encode(&self, text: &str, query: bool) -> Result<tokenizers::Encoding> {
        let prefix = if query {
            self.entry.prefix_query
        } else {
            self.entry.prefix_document
        };
        self.tokenizer
            .encode(format!("{prefix}{text}"), true)
            .map_err(|e| Error::InvalidVector {
                detail: e.to_string(),
            })
    }

    pub fn count_document(&self, text: &str) -> Result<usize> {
        Ok(self.encode(text, false)?.len())
    }
    pub fn count_query(&self, text: &str) -> Result<usize> {
        Ok(self.encode(text, true)?.len())
    }

    fn checked_encoding(&self, text: &str, query: bool) -> Result<tokenizers::Encoding> {
        let encoding = self.encode(text, query)?;
        if encoding.len() > self.limit() {
            return Err(Error::InvalidVector { detail: format!(
                "embedding token budget exceeded for {}: {} tokens (including prefix and special tokens), limit {}; split the input",
                self.entry.id, encoding.len(), self.limit(),
            ) });
        }
        Ok(encoding)
    }

    pub fn validate_document(&self, text: &str) -> Result<usize> {
        Ok(self.encode_document(text)?.len())
    }
    pub fn validate_query(&self, text: &str) -> Result<usize> {
        Ok(self.checked_encoding(text, true)?.len())
    }

    /// Shared by counting and the strict document embedding path: no second tokenization policy.
    pub(crate) fn encode_document(&self, text: &str) -> Result<tokenizers::Encoding> {
        self.checked_encoding(text, false)
    }
}
