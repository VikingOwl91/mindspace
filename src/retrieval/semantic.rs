//! The semantic capability: whether query-time vector search can run, and the
//! verified query encoder when it can.
//!
//! Resolution is offline. It reads metadata, checks that the model files are
//! present and well formed, confirms that vectors exist for the active
//! derivation key, and verifies the artifacts against the trusted manifest with
//! a full SHA-256 pass before the lazy runtime is built. It never downloads,
//! installs, or indexes. Each `resolve_with` call re-verifies. MCP may retain
//! one verified runtime per session, with live readiness and change checks.

use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::core::{Error, Result};
use crate::embeddings::manager;
use crate::embeddings::manifest::{self, ModelManifestEntry};
use crate::embeddings::EmbeddingRuntime;
use crate::retrieval::FallbackReason;
use crate::storage::Store;

#[cfg(all(target_os = "linux", feature = "embeddings"))]
mod cache;
#[cfg(all(target_os = "linux", feature = "embeddings"))]
pub(crate) use cache::SemanticCache;

#[cfg(not(all(target_os = "linux", feature = "embeddings")))]
#[derive(Default)]
pub(crate) struct SemanticCache {}

#[cfg(not(all(target_os = "linux", feature = "embeddings")))]
impl SemanticCache {
    pub(crate) fn search(
        &mut self,
        store: &dyn Store,
        query: crate::core::SearchQuery,
        mode: crate::retrieval::SearchMode,
    ) -> Result<crate::retrieval::SearchOutcome> {
        crate::retrieval::search_with_mode_resolving_capability(store, query, mode)
    }
}

/// Maps a stored model id to its trusted manifest entry.
pub type ModelLookup = fn(&str) -> Option<&'static ModelManifestEntry>;

/// Builds a query encoder bound to a verified model directory.
pub type RuntimeFactory<'a> =
    &'a dyn Fn(&ModelManifestEntry, &Path) -> Result<Box<dyn EmbeddingRuntime>>;

/// The outcome of resolving the semantic capability for one search.
#[derive(Debug)]
pub enum SemanticCapability {
    Ready(SemanticSource),
    Unavailable(FallbackReason),
}

/// A verified query encoder for one derivation key. Only `resolve_with`
/// constructs one, so every instance has passed the readiness checks.
pub struct SemanticSource {
    derivation_key: String,
    model_id: String,
    dimension: usize,
    runtime: Box<dyn EmbeddingRuntime>,
}

impl SemanticSource {
    /// The active derivation key. Vectors are matched on this exact value.
    pub fn derivation_key(&self) -> &str {
        &self.derivation_key
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// Embeds a query through the runtime's query path. The runtime applies the
    /// query prefix; this method never adds one. The vector must have the
    /// manifest dimension and only finite values.
    pub fn embed_query(&self, text: &str) -> std::result::Result<Vec<f32>, FallbackReason> {
        let vector =
            self.runtime
                .embed_query(text)
                .map_err(|e| FallbackReason::QueryEmbeddingFailed {
                    detail: storage_detail(e),
                })?;
        if vector.len() != self.dimension {
            return Err(FallbackReason::QueryEmbeddingFailed {
                detail: format!(
                    "dimension mismatch: got {}, expected {}",
                    vector.len(),
                    self.dimension
                ),
            });
        }
        if let Some(index) = vector.iter().position(|value| !value.is_finite()) {
            return Err(FallbackReason::QueryEmbeddingFailed {
                detail: format!("non-finite value at index {index}"),
            });
        }
        Ok(vector)
    }
}

impl fmt::Debug for SemanticSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SemanticSource")
            .field("model_id", &self.model_id)
            .field("dimension", &self.dimension)
            .field("derivation_key", &self.derivation_key)
            .finish_non_exhaustive()
    }
}

/// Resolves the capability against an explicit data directory and manifest
/// lookup. Storage errors propagate. Every capability problem becomes
/// `Unavailable` with the reason that explains it.
///
/// Checks run in cost order: metadata, then file structure, then index
/// coverage, then hashing, then runtime construction. The factory runs only
/// after every artifact has verified.
pub fn resolve_with(
    store: &dyn Store,
    base: &Path,
    lookup: ModelLookup,
    factory: RuntimeFactory<'_>,
) -> Result<SemanticCapability> {
    match readiness(store, base, lookup)? {
        Readiness::Ready(model) => resolve_ready(&model, factory),
        Readiness::Unavailable(reason) => Ok(SemanticCapability::Unavailable(reason)),
    }
}

#[derive(PartialEq, Eq)]
struct ReadyModel {
    entry: &'static ModelManifestEntry,
    dir: PathBuf,
    derivation_key: String,
}

enum Readiness {
    Ready(ReadyModel),
    Unavailable(FallbackReason),
}

fn readiness(store: &dyn Store, base: &Path, lookup: ModelLookup) -> Result<Readiness> {
    if !manager::read_enabled(store)? {
        return Ok(Readiness::Unavailable(FallbackReason::EmbeddingsDisabled));
    }
    let Some(selected) = manager::read_selected_model(store)? else {
        return Ok(Readiness::Unavailable(FallbackReason::NoModelSelected));
    };
    let Some(entry) = lookup(&selected) else {
        return Ok(Readiness::Unavailable(FallbackReason::UnknownModel {
            id: selected,
        }));
    };
    let dir = manager::model_dir_in(base, entry.id);
    if let Some(reason) = artifacts_present(base, &dir, entry) {
        return Ok(Readiness::Unavailable(reason));
    }
    let derivation_key = manifest::derivation_from_manifest(entry).derivation_key();
    if store.embedding_index_status(&derivation_key)?.embedded == 0 {
        return Ok(Readiness::Unavailable(FallbackReason::NoCompatibleIndex));
    }
    Ok(Readiness::Ready(ReadyModel {
        entry,
        dir,
        derivation_key,
    }))
}

fn resolve_ready(model: &ReadyModel, factory: RuntimeFactory<'_>) -> Result<SemanticCapability> {
    let entry = model.entry;
    if let Err(detail) = verify_artifacts(&model.dir, entry) {
        return Ok(SemanticCapability::Unavailable(
            FallbackReason::ModelFilesInvalid {
                model: entry.id.to_string(),
                detail,
            },
        ));
    }
    let runtime = match factory(entry, &model.dir) {
        Ok(runtime) => runtime,
        Err(e) => {
            return Ok(SemanticCapability::Unavailable(
                FallbackReason::RuntimeUnavailable {
                    detail: storage_detail(e),
                },
            ))
        }
    };
    if runtime.model_id() != entry.id {
        return Ok(SemanticCapability::Unavailable(
            FallbackReason::RuntimeUnavailable {
                detail: format!(
                    "runtime reports model '{}', expected '{}'",
                    runtime.model_id(),
                    entry.id
                ),
            },
        ));
    }
    if runtime.dimension() != entry.dimension as usize {
        return Ok(SemanticCapability::Unavailable(
            FallbackReason::RuntimeUnavailable {
                detail: format!(
                    "runtime dimension {} does not match manifest dimension {}",
                    runtime.dimension(),
                    entry.dimension
                ),
            },
        ));
    }
    Ok(SemanticCapability::Ready(SemanticSource {
        derivation_key: model.derivation_key.clone(),
        model_id: entry.id.to_string(),
        dimension: entry.dimension as usize,
        runtime,
    }))
}

/// Resolves the capability for the user's data directory and the compiled-in
/// manifest. Without the `embeddings` feature it is always unavailable.
pub fn resolve(store: &dyn Store) -> Result<SemanticCapability> {
    #[cfg(feature = "embeddings")]
    {
        // No usable data directory means no model can be found: semantic search
        // is unavailable (Auto falls back to lexical), it does not guess a path.
        let base = match manager::data_dir() {
            Ok(base) => base,
            Err(e) => {
                return Ok(SemanticCapability::Unavailable(
                    FallbackReason::RuntimeUnavailable {
                        detail: e.to_string(),
                    },
                ))
            }
        };
        resolve_with(store, &base, manifest::find_model, &candle_runtime)
    }
    #[cfg(not(feature = "embeddings"))]
    {
        let _ = store;
        Ok(SemanticCapability::Unavailable(
            FallbackReason::FeatureDisabled,
        ))
    }
}

#[cfg(feature = "embeddings")]
fn candle_runtime(entry: &ModelManifestEntry, dir: &Path) -> Result<Box<dyn EmbeddingRuntime>> {
    Ok(Box::new(crate::embeddings::CandleRuntime::new_in(
        entry,
        dir.to_path_buf(),
    )))
}

/// Cheap structural checks that run before any hashing: the model directory
/// and each artifact exist, are regular files, are not symlinks, and have the
/// manifest size.
fn artifacts_present(
    base: &Path,
    dir: &Path,
    entry: &ModelManifestEntry,
) -> Option<FallbackReason> {
    let model = entry.id.to_string();
    let missing = || {
        Some(FallbackReason::ModelFilesMissing {
            model: model.clone(),
        })
    };
    let invalid = |detail: String| {
        Some(FallbackReason::ModelFilesInvalid {
            model: model.clone(),
            detail,
        })
    };

    for path in [base.to_path_buf(), base.join("models"), dir.to_path_buf()] {
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return invalid(format!("refusing symlinked path {}", path.display()));
            }
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => return missing(),
            Err(e) => return invalid(e.to_string()),
        }
    }
    for art in entry.artifacts {
        let path = dir.join(art.filename);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return invalid(format!("refusing symlinked artifact {}", path.display()));
            }
            Ok(meta) if !meta.is_file() => {
                return invalid(format!("{} is not a regular file", path.display()));
            }
            Ok(meta) if meta.len() != art.size => {
                return invalid(format!(
                    "size mismatch for {}: got {}, expected {}",
                    path.display(),
                    meta.len(),
                    art.size
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => return missing(),
            Err(e) => return invalid(e.to_string()),
        }
    }
    None
}

/// Full SHA-256 verification of every artifact, the same check installation
/// performs.
fn verify_artifacts(dir: &Path, entry: &ModelManifestEntry) -> std::result::Result<(), String> {
    for art in entry.artifacts {
        manager::verify_file(&dir.join(art.filename), art).map_err(storage_detail)?;
    }
    Ok(())
}

/// The message of a storage error without its category prefix.
fn storage_detail(error: Error) -> String {
    match error {
        Error::Storage(message) => message,
        other => other.to_string(),
    }
}
