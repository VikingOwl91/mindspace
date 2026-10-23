//! Incremental embedding indexing orchestration (M3).
//!
//! Connects M1 storage to an `EmbeddingRuntime`. No semantic retrieval here.

use crate::core::{Result, Uuid};
use crate::embeddings::EmbeddingRuntime;
use crate::storage::Store;

pub struct IndexFailure {
    pub assertion_id: Uuid,
    pub error: String,
}

pub struct IndexReport {
    pub derivation_key: String,
    pub eligible: u64,
    pub already_indexed: u64,
    pub indexed: u64,
    pub failed: u64,
    pub remaining: u64,
    pub failures: Vec<IndexFailure>,
}

/// Run incremental embedding indexing.
///
/// `batch_size` bounds the number of candidates fetched per database round-trip
/// (memory and work per batch). `limit`, when `Some`, caps the total number of
/// assertions *attempted* in this invocation. Failed assertions remain in the
/// needing set and are retried on the next invocation; with a small `limit` a
/// persistently failing oldest assertion can therefore starve later candidates
/// across runs. Running without a `limit` processes all remaining candidates.
pub fn index_assertions(
    store: &dyn Store,
    runtime: &dyn EmbeddingRuntime,
    derivation: &crate::core::EmbeddingDerivation,
    batch_size: u32,
    limit: Option<u32>,
) -> Result<IndexReport> {
    if batch_size == 0 {
        return Err(crate::core::Error::Storage(
            "batch_size must be greater than 0".to_string(),
        ));
    }
    let total_cap = limit.unwrap_or(u32::MAX);
    let derivation_key = derivation.derivation_key();

    let status_before = store.embedding_index_status(&derivation_key)?;
    let eligible = status_before.total_assertions;
    let already_indexed = status_before.embedded;

    let mut indexed = 0u64;
    let mut failed = 0u64;
    let mut failures: Vec<IndexFailure> = Vec::new();

    // Bounded batching with a cursor: fetch at most `batch_size` per batch,
    // advance the cursor past each batch. Failed items are not retried within
    // this invocation (they remain in the needing set for the next run), which
    // prevents unbounded memory use and infinite retry loops.
    let mut cursor: Option<(crate::core::Ts, Uuid)> = None;
    let mut processed: u32 = 0;
    loop {
        if processed >= total_cap {
            break;
        }
        let want = (total_cap - processed).min(batch_size);
        if want == 0 {
            break;
        }
        let candidates = store.list_assertions_needing_embedding(&derivation_key, cursor, want)?;
        if candidates.is_empty() {
            break;
        }
        for c in &candidates {
            if processed >= total_cap {
                break;
            }
            processed += 1;
            let text = if c.subject.trim().is_empty() {
                c.content.clone()
            } else {
                format!("{}: {}", c.subject, c.content)
            };
            match runtime.embed_document(&text) {
                Ok(vec) => {
                    if vec.len() != derivation.dimension as usize {
                        failed += 1;
                        failures.push(IndexFailure {
                            assertion_id: c.assertion_id,
                            error: format!(
                                "dimension mismatch: got {}, expected {}",
                                vec.len(),
                                derivation.dimension
                            ),
                        });
                        continue;
                    }
                    if vec.iter().any(|v| !v.is_finite()) {
                        failed += 1;
                        failures.push(IndexFailure {
                            assertion_id: c.assertion_id,
                            error: "non-finite embedding".to_string(),
                        });
                        continue;
                    }
                    let emb = crate::core::AssertionEmbedding {
                        assertion_id: c.assertion_id,
                        derivation_key: derivation_key.clone(),
                        model: derivation.model_id.clone(),
                        model_version: derivation.model_version.clone(),
                        dimension: derivation.dimension,
                        embedding: vec,
                    };
                    match store.save_embeddings(&[emb]) {
                        Ok(n) => indexed += n as u64,
                        Err(e) => {
                            failed += 1;
                            failures.push(IndexFailure {
                                assertion_id: c.assertion_id,
                                error: e.to_string(),
                            });
                        }
                    }
                }
                Err(e) => {
                    failed += 1;
                    failures.push(IndexFailure {
                        assertion_id: c.assertion_id,
                        error: e.to_string(),
                    });
                }
            }
        }
        if let Some(last) = candidates.last() {
            cursor = Some((last.created_at, last.assertion_id));
        }
        if (candidates.len() as u32) < want {
            break;
        }
    }

    let status_after = store.embedding_index_status(&derivation_key)?;
    let remaining = status_after.missing();

    Ok(IndexReport {
        derivation_key,
        eligible,
        already_indexed,
        indexed,
        failed,
        remaining,
        failures,
    })
}
