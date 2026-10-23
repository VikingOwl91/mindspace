//! Compiled-in trusted model manifest. No remote manifest execution or
//! trust-on-first-use: every entry is baked into the binary.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Artifact {
    pub filename: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelManifestEntry {
    pub id: &'static str,
    pub version: &'static str,
    pub revision: &'static str,
    pub architecture: &'static str,
    pub runtime: &'static str,
    pub runtime_version: &'static str,
    pub dimension: u32,
    pub max_seq_len: u32,
    pub truncation: &'static str,
    pub prefix_document: &'static str,
    pub prefix_query: &'static str,
    pub pooling: &'static str,
    pub normalization: &'static str,
    pub license: &'static str,
    pub source: &'static str,
    pub artifacts: &'static [Artifact],
}

pub const NOMIC_V1_5: ModelManifestEntry = ModelManifestEntry {
    id: "nomic-embed-text-v1.5",
    version: "1.5",
    revision: "e9b6763023c676ca8431644204f50c2b100d9aab",
    architecture: "nomic-bert",
    runtime: "candle",
    runtime_version: "0.11.0",
    dimension: 768,
    max_seq_len: 2048,
    truncation: "truncate to max_seq_len",
    prefix_document: "search_document: ",
    prefix_query: "search_query: ",
    pooling: "mean",
    normalization: "l2",
    license: "Apache-2.0",
    source: "https://huggingface.co/nomic-ai/nomic-embed-text-v1.5",
    artifacts: &[
        Artifact {
            filename: "model.safetensors",
            url: "https://huggingface.co/nomic-ai/nomic-embed-text-v1.5/resolve/e9b6763023c676ca8431644204f50c2b100d9aab/model.safetensors",
            sha256: "9e7d262b1fe5ea350782829496efa831901b77486bbde1cea54a4c822d010d5c",
            size: 546_938_168,
        },
        Artifact {
            filename: "tokenizer.json",
            url: "https://huggingface.co/nomic-ai/nomic-embed-text-v1.5/resolve/e9b6763023c676ca8431644204f50c2b100d9aab/tokenizer.json",
            sha256: "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
            size: 711_396,
        },
    ],
};

pub const MANIFEST: &[ModelManifestEntry] = &[NOMIC_V1_5];

pub fn find_model(id: &str) -> Option<&'static ModelManifestEntry> {
    MANIFEST.iter().find(|m| m.id == id)
}

pub fn default_model() -> &'static ModelManifestEntry {
    &NOMIC_V1_5
}

pub fn derivation_from_manifest(entry: &ModelManifestEntry) -> crate::core::EmbeddingDerivation {
    let model_sha = entry
        .artifacts
        .iter()
        .find(|a| a.filename == "model.safetensors")
        .map(|a| a.sha256.to_string())
        .unwrap_or_default();
    let tok_sha = entry
        .artifacts
        .iter()
        .find(|a| a.filename == "tokenizer.json")
        .map(|a| a.sha256.to_string())
        .unwrap_or_default();
    crate::core::EmbeddingDerivation {
        model_id: entry.id.to_string(),
        model_version: entry.version.to_string(),
        model_sha256: model_sha,
        tokenizer_sha256: tok_sha,
        prefix_strategy: entry.prefix_document.to_string(),
        pooling: entry.pooling.to_string(),
        normalization: entry.normalization.to_string(),
        max_seq_len: entry.max_seq_len,
        truncation: entry.truncation.to_string(),
        dimension: entry.dimension,
        runtime: entry.runtime.to_string(),
        runtime_version: entry.runtime_version.to_string(),
    }
}
