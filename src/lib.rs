//! Mindspace engine: domain model, storage, ingest, retrieval, CLI.
//!
//! Module boundaries (docs/architecture.md): `core` is pure domain types,
//! only `storage` knows SQLite, `ingest` and `retrieval` orchestrate through
//! the `Store` trait, `cli` renders.

pub mod cli;
pub mod core;
pub mod ingest;
pub mod mcp;
pub mod retrieval;
pub mod storage;

pub use core::{Error, Result};
pub use storage::sqlite::SqliteStore;

use std::path::{Path, PathBuf};

use storage::Store;
use uuid::Uuid;

/// Default database location: `$XDG_DATA_HOME/mindspace/mind.db`, falling
/// back to `$HOME/.local/share/mindspace/mind.db`.
pub fn default_db_path() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("mindspace").join("mind.db");
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".local/share/mindspace/mind.db");
        }
    }
    PathBuf::from("mind.db")
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
    let mut ids = store.memory_id_prefix_matches(prefix)?;
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
