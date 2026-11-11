//! Mindspace engine: domain model, storage, ingest, retrieval, CLI.
//!
//! Module boundaries (docs/architecture.md): `core` is pure domain types,
//! only `storage` knows SQLite, `ingest` and `retrieval` orchestrate through
//! the `Store` trait, `cli` renders.

pub mod cli;
pub mod core;
pub mod doctor;
pub mod embeddings;
#[cfg(unix)]
pub mod grants;
pub mod indexing;
pub mod ingest;
pub mod mcp;
pub mod pathenv;
pub mod paths;
pub mod retrieval;
#[cfg(target_os = "linux")]
pub mod sandbox;
pub mod setup;
pub mod storage;
pub mod worker;

pub use core::{Error, Result};
pub use storage::sqlite::SqliteStore;

use std::path::{Path, PathBuf};

use storage::Store;
use uuid::Uuid;

/// Default database location: `$XDG_DATA_HOME/mindspace/mind.db`, falling
/// back to `$HOME/.local/share/mindspace/mind.db`. Fails when neither is
/// usable (see [`paths`]); it never falls back to the working directory.
pub fn default_db_path() -> Result<PathBuf> {
    Ok(paths::db_path_from(None, paths::DbOrigin::Unset, paths::data_dir)?.path)
}

/// Creates the database file, its parent directories, and the schema.
/// Idempotent: running it against an initialized database is a no-op.
pub fn initialize(path: &Path) -> Result<PathBuf> {
    SqliteStore::open(path)?;
    Ok(path.to_path_buf())
}

/// Opens an initialized database. Any command other than `init` requires an
/// existing database file.
pub fn open_existing(path: &Path) -> Result<SqliteStore> {
    if !path.exists() {
        return Err(Error::NotInitialized {
            path: path.display().to_string(),
        });
    }
    SqliteStore::open(path)
}

/// Resolves a full UUID or an unambiguous prefix (minimum 4 characters).
pub fn resolve_memory_id(store: &dyn Store, prefix: &str) -> Result<Uuid> {
    resolve_memory_ids(store.memory_id_prefix_matches(prefix)?, prefix)
}

/// Collect a complete read response, including prefix resolution, before rendering.
pub(crate) fn memory_detail_for_prefix(
    store: &dyn Store,
    prefix: &str,
) -> Result<core::MemoryDetail> {
    let session = store.read_session()?;
    let id = resolve_memory_ids(session.memory_id_prefix_matches(prefix)?, prefix)?;
    let detail = session
        .get_memory_detail(id)?
        .ok_or_else(|| Error::MemoryNotFound {
            prefix: prefix.to_string(),
        })?;
    session.finish()?;
    Ok(detail)
}

fn resolve_memory_ids(mut ids: Vec<Uuid>, prefix: &str) -> Result<Uuid> {
    if prefix.len() < 4 || ids.is_empty() {
        return Err(Error::MemoryNotFound {
            prefix: prefix.to_string(),
        });
    }
    match ids.len() {
        1 => Ok(ids.remove(0)),
        _ => {
            let mut candidates: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
            if candidates.len() > 10 {
                candidates.truncate(10);
                candidates.push("...".to_string());
            }
            Err(Error::AmbiguousPrefix {
                prefix: prefix.to_string(),
                candidates: candidates.join(", "),
            })
        }
    }
}
