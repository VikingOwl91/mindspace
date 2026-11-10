//! Versioned source-slice contract and stored-vector checks, available without inference.
//! This namespace deliberately does not change the legacy EmbeddingDerivation recipe.
use crate::core::{decode_f32_le, is_sha256_hex, sha256_hex, Error, Result};
use crate::embeddings::manifest::{self, ModelManifestEntry};
use serde::{Deserialize, Serialize};

const LOCK: &str = include_str!("../../Cargo.lock");
pub(crate) const TOKENIZER_ENGINE: &str = "0.23.2";
pub(crate) const DIMENSION: u32 = 768;
pub(crate) const MAX_TOKENS: u32 = 2048;
fn bad(detail: impl Into<String>) -> Error {
    Error::InvalidVector {
        detail: detail.into(),
    }
}

/// This is the original S3 recipe, shared rather than approximated by preview.
pub(crate) fn budget_identity(entry: &ModelManifestEntry) -> Result<String> {
    if manifest::find_model(entry.id) != Some(entry) {
        return Err(bad("unsupported embedding token-budget configuration"));
    }
    let tokenizer = entry
        .artifacts
        .iter()
        .find(|a| a.filename == "tokenizer.json")
        .ok_or_else(|| bad("model has no tokenizer artifact"))?;
    Ok(sha256_hex(serde_json::json!({
        "contract":"complete-input@1", "engine":"tokenizers@0.23.2",
        "model":entry.id,"revision":entry.revision,"tokenizer":tokenizer.sha256,
        "limit":entry.max_seq_len,"document_prefix":entry.prefix_document,
        "query_prefix":entry.prefix_query,"special_tokens":true,"padding":false,"truncation":false
    }).to_string().as_bytes()))
}

/// One source of effective Nomic parameters for both old and new forward paths.
pub(super) fn effective_config(dimension: usize) -> serde_json::Value {
    serde_json::json!({"vocab_size":30528,"n_embd":dimension,"n_head":12,"n_layer":12,
        "n_inner":3072,"n_positions":8192,"type_vocab_size":2,"layer_norm_epsilon":1e-12,
        "rotary_emb_fraction":1.0,"rotary_emb_base":1000.0,"rotary_emb_interleaved":false,
        "qkv_proj_bias":false,"mlp_fc1_bias":false,"mlp_fc2_bias":false,
        "activation_function":"swiglu","prenorm":false,"model_type":"nomic_bert"})
}
fn resolved_version(package: &str) -> Result<String> {
    let found = LOCK
        .split("[[package]]")
        .filter_map(|section| {
            let mut name = None;
            let mut version = None;
            for line in section.lines() {
                if let Some(value) = line.strip_prefix("name = \"") {
                    name = value.strip_suffix('"');
                }
                if let Some(value) = line.strip_prefix("version = \"") {
                    version = value.strip_suffix('"');
                }
            }
            (name == Some(package)).then_some(version).flatten()
        })
        .collect::<Vec<_>>();
    if found.len() != 1 {
        return Err(bad(format!("ambiguous resolved runtime package {package}")));
    }
    Ok(found[0].to_owned())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct StrictContract {
    pub schema_version: u32,
    pub algorithm_version: u32,
    pub model_id: String,
    pub model_version: String,
    pub model_revision: String,
    pub architecture: String,
    pub weights_sha256: String,
    pub weights_bytes: u64,
    pub tokenizer_sha256: String,
    pub tokenizer_bytes: u64,
    pub budget_identity: String,
    pub prefix_document: String,
    pub prefix_query: String,
    pub dimension: u32,
    pub max_tokens: u32,
    pub payload_policy: String,
    pub encoding_policy: String,
    pub pooling: String,
    pub normalization: String,
    pub serialization: String,
    pub verification_policy: String,
    pub runtime_policy: String,
    pub candle_core_version: String,
    pub candle_nn_version: String,
    pub candle_transformers_version: String,
    pub tokenizers_version: String,
    pub config_json: String,
    pub config_sha256: String,
    pub lock_sha256: String,
    pub build_fingerprint: String,
}
impl StrictContract {
    pub(crate) fn current(model_id: &str) -> Result<Self> {
        let e = manifest::find_model(model_id)
            .ok_or_else(|| bad("unsupported strict embedding model"))?;
        let weights = e
            .artifacts
            .iter()
            .find(|a| a.filename == "model.safetensors")
            .ok_or_else(|| bad("missing pinned weights contract"))?;
        let tok = e
            .artifacts
            .iter()
            .find(|a| a.filename == "tokenizer.json")
            .ok_or_else(|| bad("missing pinned tokenizer contract"))?;
        let core = resolved_version("candle-core")?;
        let nn = resolved_version("candle-nn")?;
        let transformers = resolved_version("candle-transformers")?;
        let tokenizer = resolved_version("tokenizers")?;
        if core != "0.11.1"
            || nn != "0.11.1"
            || transformers != "0.11.1"
            || tokenizer != TOKENIZER_ENGINE
        {
            return Err(bad(
                "strict runtime build requires reviewed Candle/tokenizer versions",
            ));
        }
        let config_json = effective_config(e.dimension as usize).to_string();
        let mut value=Self {
            schema_version:1,algorithm_version:1,model_id:e.id.into(),model_version:e.version.into(),
            model_revision:e.revision.into(),architecture:e.architecture.into(),weights_sha256:weights.sha256.into(),
            weights_bytes:weights.size,tokenizer_sha256:tok.sha256.into(),tokenizer_bytes:tok.size,
            budget_identity:budget_identity(e)?,prefix_document:e.prefix_document.into(),prefix_query:e.prefix_query.into(),
            dimension:e.dimension,max_tokens:e.max_seq_len,payload_policy:"canonical-source-slice@1".into(),
            encoding_policy:"doc-semantic-encoding@1;special=true;padding=false;overflow=reject;token-types=none".into(),
            pooling:"attention-mask-mean-including-prefix-specials@1".into(),normalization:"l2;f64-norm-tolerance=0.0001".into(),
            serialization:"finite-f32-le@1".into(),verification_policy:"doc-semantic-verification@1".into(),
            runtime_policy:"candle-nomic-cpu-f32@1".into(),candle_core_version:core,candle_nn_version:nn,
            candle_transformers_version:transformers,tokenizers_version:tokenizer,
            config_sha256:sha256_hex(config_json.as_bytes()),config_json,lock_sha256:sha256_hex(LOCK.as_bytes()),build_fingerprint:String::new(),
        };
        value.build_fingerprint = value.expected_build_fingerprint();
        value.validate_stored()?;
        Ok(value)
    }
    fn expected_build_fingerprint(&self) -> String {
        sha256_hex(
            serde_json::json!([
                "doc-semantic-runtime-build@1",
                self.candle_core_version,
                self.candle_nn_version,
                self.candle_transformers_version,
                self.tokenizers_version,
                self.config_sha256,
                self.lock_sha256,
                self.runtime_policy
            ])
            .to_string()
            .as_bytes(),
        )
    }
    pub(crate) fn canonical(&self) -> Vec<u8> {
        // Struct fields have a fixed declared order. No caller-supplied maps are hashed.
        serde_json::to_vec(self).expect("fixed contract fields serialize")
    }
    pub(crate) fn key(&self) -> String {
        let mut bytes = Vec::new();
        put(&mut bytes, "doc-source-slice-strict@1");
        let contract = self.canonical();
        bytes.extend_from_slice(&(contract.len() as u64).to_be_bytes());
        bytes.extend(contract);
        sha256_hex(&bytes)
    }
    /// Validate known storage encoding; historical lock/version fields are not current-build checks.
    pub(crate) fn validate_stored(&self) -> Result<()> {
        if self.schema_version!=1||self.algorithm_version!=1||self.dimension!=DIMENSION||self.max_tokens!=MAX_TOKENS
            ||self.architecture!="nomic-bert"||self.payload_policy!="canonical-source-slice@1"
            ||self.encoding_policy!="doc-semantic-encoding@1;special=true;padding=false;overflow=reject;token-types=none"
            ||self.pooling!="attention-mask-mean-including-prefix-specials@1"
            ||self.normalization!="l2;f64-norm-tolerance=0.0001"||self.serialization!="finite-f32-le@1"
            ||self.verification_policy!="doc-semantic-verification@1"||self.runtime_policy!="candle-nomic-cpu-f32@1"
            ||self.weights_bytes==0||self.weights_bytes>2_147_483_648||self.tokenizer_bytes==0||self.tokenizer_bytes>4_194_304
            ||self.config_json.len()>4096||self.canonical().len()>65536 {
            return Err(bad("unsupported or malformed stored strict contract"));
        }
        for value in [
            &self.model_id,
            &self.model_version,
            &self.model_revision,
            &self.prefix_document,
            &self.prefix_query,
            &self.candle_core_version,
            &self.candle_nn_version,
            &self.candle_transformers_version,
            &self.tokenizers_version,
        ] {
            if value.is_empty() || value.len() > 256 {
                return Err(bad("invalid strict contract scalar"));
            }
        }
        for value in [
            &self.weights_sha256,
            &self.tokenizer_sha256,
            &self.budget_identity,
            &self.config_sha256,
            &self.lock_sha256,
            &self.build_fingerprint,
        ] {
            if !is_sha256_hex(value) {
                return Err(bad("invalid strict contract digest"));
            }
        }
        let config: serde_json::Value = serde_json::from_str(&self.config_json)
            .map_err(|_| bad("invalid effective model configuration"))?;
        if !config.is_object()
            || config["n_embd"].as_u64() != Some(u64::from(self.dimension))
            || sha256_hex(self.config_json.as_bytes()) != self.config_sha256
            || self.expected_build_fingerprint() != self.build_fingerprint
        {
            return Err(bad("strict contract configuration/build digest mismatch"));
        }
        Ok(())
    }
    #[cfg(any(feature = "embeddings", test))]
    pub(crate) fn validate_current(&self) -> Result<()> {
        self.validate_stored()?;
        if *self != Self::current(&self.model_id)? {
            return Err(bad(
                "strict contract differs from current authenticated pipeline",
            ));
        }
        Ok(())
    }
}

pub(crate) fn put(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}
#[cfg(any(feature = "embeddings", test))]
pub(crate) fn encoding_digest(
    contract: &StrictContract,
    payload_hash: &str,
    ids: &[u32],
    mask: &[u32],
) -> Result<String> {
    if ids.is_empty()
        || ids.len() != mask.len()
        || ids.len() > contract.max_tokens as usize
        || !is_sha256_hex(payload_hash)
    {
        return Err(bad("invalid checked document encoding"));
    }
    let mut out = Vec::new();
    put(&mut out, "doc-semantic-encoding@1");
    put(&mut out, &contract.key());
    put(&mut out, &contract.budget_identity);
    put(&mut out, payload_hash);
    for values in [ids, mask] {
        out.extend_from_slice(&(values.len() as u64).to_be_bytes());
        for value in values {
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
    out.push(0); // token-type input is None in the actual shared forward call.
    Ok(sha256_hex(&out))
}

pub(crate) fn validate_blob(contract: &StrictContract, bytes: &[u8]) -> Result<()> {
    contract.validate_stored()?;
    if bytes.len() != contract.dimension as usize * 4 {
        return Err(bad("strict vector byte length/dimension mismatch"));
    }
    let vector = decode_f32_le(bytes, Some(contract.dimension as usize))?;
    let norm = vector
        .iter()
        .map(|&v| f64::from(v) * f64::from(v))
        .sum::<f64>()
        .sqrt();
    if !norm.is_finite() || (norm - 1.0).abs() > 1e-4 {
        return Err(bad("strict vector is zero or not unit-normalized"));
    }
    Ok(())
}
#[cfg(any(feature = "embeddings", test))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct StrictVector {
    pub token_count: u32,
    pub encoding_digest: String,
    pub embedding: Vec<u8>,
    pub vector_hash: String,
}
#[cfg(any(feature = "embeddings", test))]
impl StrictVector {
    pub(crate) fn validate(&self, contract: &StrictContract) -> Result<()> {
        if self.token_count == 0
            || self.token_count > contract.max_tokens
            || !is_sha256_hex(&self.encoding_digest)
            || sha256_hex(&self.embedding) != self.vector_hash
        {
            return Err(bad("strict vector verification evidence mismatch"));
        }
        validate_blob(contract, &self.embedding)
    }
}

#[cfg(test)]
#[path = "strict_contract_tests.rs"]
mod tests;
