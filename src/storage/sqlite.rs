use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::functions::FunctionFlags;
use rusqlite::types::{Type, Value};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use uuid::Uuid;

use crate::core::{
    cosine_similarity, decode_f32_le, encode_f32_le, sha256_hex, Anchor, Assertion, AssertionDraft,
    AssertionEmbedding, AssertionKind, CodeDerivation, CodeEntity, CodeImport, CodeObservation,
    CodeObservationReport, ConflictGroup, DocumentExtractionDraft, DocumentExtractionInfo,
    DocumentExtractionReport, EmbeddingIndexStatus, EmbeddingSettings, Error, Evidence,
    EvidenceDraft, EvidenceKind, FileObservation, FileOutcome, GitFileOutcome, GitFileReport,
    GitObservation, GitObservationReport, GitProvenance, GitRefEventRecord, GitRefObservation,
    IngestObservation, IngestScope, LogicalKind, LogicalSource, ManualMemoryDraft,
    ManualMemoryReport, Memory, MemoryAction, MemoryActionKind, MemoryDetail, MemoryState,
    MemorySummary, ObservationReport, ObservedFile, Result, SearchHitRaw, SearchQuery, Source,
    SourceEvent, SourceEventKind, SourceKind, SourceLifecycle, SourcePresence, Space, SpaceLink,
    StoredCitation, TemporalCoverage, TemporalCoverageStatus, TemporalDimension,
    TemporalHistoryCapabilities, Ts, UnembeddedAssertion, EXTRACTION_MANUAL_WRITE,
};
use crate::storage::Store;
mod read_session;

const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../../migrations/0001_init.sql")),
    (
        2,
        include_str!("../../migrations/0002_source_lifecycle.sql"),
    ),
    (3, include_str!("../../migrations/0003_git_ingest.sql")),
    (4, include_str!("../../migrations/0004_code_structure.sql")),
    (
        5,
        include_str!("../../migrations/0005_code_relationships.sql"),
    ),
    (6, include_str!("../../migrations/0006_manual_writes.sql")),
    (7, include_str!("../../migrations/0007_embeddings.sql")),
    (
        8,
        include_str!("../../migrations/0008_temporal_recording.sql"),
    ),
    (
        9,
        include_str!("../../migrations/0009_scopes_and_views.sql"),
    ),
    (10, include_str!("../../migrations/0010_documents.sql")),
    (
        11,
        include_str!("../../migrations/0011_doc_code_blocks.sql"),
    ),
    (12, include_str!("../../migrations/0012_doc_chunks.sql")),
    (
        13,
        include_str!("../../migrations/0013_legacy_ownership.sql"),
    ),
    (
        14,
        include_str!("../../migrations/0014_document_preparation.sql"),
    ),
    (
        15,
        include_str!("../../migrations/0015_lexical_staging.sql"),
    ),
    (
        16,
        include_str!("../../migrations/0016_semantic_staging.sql"),
    ),
    (
        17,
        include_str!("../../migrations/0017_passive_fence_plans.sql"),
    ),
];

const MANUAL_SOURCE_LOCATION: &str = "manual://writes";
const CURRENT_SCHEMA_VERSION: i64 = 17;
/// The migration that creates the M6 baseline and installs the write fence.
/// Those steps run exactly once, when this migration is applied, and not for
/// whichever migration happens to be latest. Later migrations add tables
/// outside the fence's inventory.
const M6_VERSION: i64 = 8;
const FILE_PRODUCER_FAMILY: &str = "file:legacy";
const GIT_PRODUCER_FAMILY: &str = "git:legacy";
const MANUAL_PRODUCER_FAMILY: &str = "manual:write";

pub(crate) mod document_preparation;
mod documents;
pub(crate) mod fence_plans;
pub(crate) mod lexical_input;
pub(crate) mod lexical_staging;
pub(crate) mod ownership;
pub(crate) mod semantic_input;
pub(crate) mod semantic_staging;

pub struct SqliteStore {
    conn: Mutex<Option<Connection>>,
    revision_state: Arc<RevisionState>,
    ownership_state: Arc<ownership::OwnershipState>,
    preparation_state: Arc<document_preparation::PreparationState>,
    lexical_state: Arc<lexical_staging::LexicalState>,
    semantic_state: Arc<semantic_staging::SemanticState>,
    fence_plan_state: Arc<fence_plans::FencePlanState>,
    fts_integrity: AtomicBool,
}

struct RevisionState {
    active_revision: AtomicI64,
    poisoned: AtomicBool,
}

struct ConnectionGuard<'a> {
    guard: MutexGuard<'a, Option<Connection>>,
    revision_state: Arc<RevisionState>,
}

impl Deref for ConnectionGuard<'_> {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        self.guard
            .as_ref()
            .expect("connection guard never contains a discarded handle")
    }
}

impl Drop for ConnectionGuard<'_> {
    fn drop(&mut self) {
        if self.revision_state.poisoned.load(Ordering::SeqCst) {
            let still_open = self.guard.as_ref().is_some_and(|conn| conn.is_autocommit());
            if !still_open {
                self.guard.take();
            }
        }
    }
}

struct WriteTransaction<'conn> {
    tx: Option<Transaction<'conn>>,
    conn: &'conn Connection,
    revision_state: Arc<RevisionState>,
}

impl<'conn> Deref for WriteTransaction<'conn> {
    type Target = Transaction<'conn>;

    fn deref(&self) -> &Self::Target {
        self.tx
            .as_ref()
            .expect("write transaction is present until commit or rollback")
    }
}

impl<'conn> DerefMut for WriteTransaction<'conn> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.tx
            .as_mut()
            .expect("write transaction is present until commit or rollback")
    }
}

impl WriteTransaction<'_> {
    fn commit(mut self) -> Result<()> {
        let tx = self
            .tx
            .take()
            .expect("write transaction is present until commit");
        let result = tx.commit();
        self.revision_state
            .active_revision
            .store(0, Ordering::SeqCst);
        if let Err(e) = result {
            if !self.conn.is_autocommit() {
                let _ = self.conn.execute_batch("ROLLBACK");
            }
            let error = commit_failure(e);
            // A COMMIT SQLite defines as "not committed" (busy, locked, constraint) that left the
            // connection outside a transaction again leaves a clean handle: keep using it. An
            // unknown outcome, or a handle still inside a transaction, is discarded.
            if matches!(error, Error::CommitOutcomeUnknown { .. }) || !self.conn.is_autocommit() {
                self.revision_state.poisoned.store(true, Ordering::SeqCst);
            }
            return Err(error);
        }
        if !self.conn.is_autocommit() {
            self.revision_state.poisoned.store(true, Ordering::SeqCst);
            return Err(Error::CommitOutcomeUnknown {
                detail: "SQLite commit returned with an active transaction".to_string(),
            });
        }
        // Test builds only: the COMMIT really succeeded, but the caller must not know it, and the
        // handle is discarded exactly as for a real unknown outcome.
        #[cfg(feature = "spawn-gate")]
        if gate::LOSE_ACK.swap(false, Ordering::SeqCst) {
            self.revision_state.poisoned.store(true, Ordering::SeqCst);
            return Err(Error::CommitOutcomeUnknown {
                detail: "test hook: the commit succeeded but its acknowledgement was lost"
                    .to_string(),
            });
        }
        Ok(())
    }
}

impl Drop for WriteTransaction<'_> {
    fn drop(&mut self) {
        self.revision_state
            .active_revision
            .store(0, Ordering::SeqCst);
        if let Some(tx) = self.tx.take() {
            if tx.rollback().is_err() || !self.conn.is_autocommit() {
                self.revision_state.poisoned.store(true, Ordering::SeqCst);
            }
        }
    }
}

/// Classifies a failed COMMIT. Busy, locked and constraint failures (a deferred constraint, or a
/// commit hook that vetoed) are, by SQLite's contract, a transaction that was NOT committed:
/// the ordinary error. Anything else (I/O, a full disk, corruption...) can fail after the commit
/// record reached the log, so what became durable is unknown: [`Error::CommitOutcomeUnknown`].
fn commit_failure(e: rusqlite::Error) -> Error {
    let definitive = matches!(
        &e,
        rusqlite::Error::SqliteFailure(code, _) if matches!(
            code.code,
            rusqlite::ErrorCode::DatabaseBusy
                | rusqlite::ErrorCode::DatabaseLocked
                | rusqlite::ErrorCode::ConstraintViolation
        )
    );
    if definitive {
        dberr(e)
    } else {
        Error::CommitOutcomeUnknown {
            detail: e.to_string(),
        }
    }
}

/// Test builds only (feature `spawn-gate`): one-shot faults at the COMMIT boundary, armed by the
/// MCP tool's test hooks. Absent from a plain build.
#[cfg(feature = "spawn-gate")]
pub mod gate {
    use std::sync::atomic::AtomicBool;
    /// The next COMMIT is vetoed by the commit hook: a real SQLite commit failure (rolled back).
    pub static FAIL_COMMIT: AtomicBool = AtomicBool::new(false);
    /// The next COMMIT really succeeds, but the caller is told the outcome is unknown.
    pub static LOSE_ACK: AtomicBool = AtomicBool::new(false);
    /// The next COMMIT is held INSIDE SQLite (in its commit hook, with the write transaction open
    /// and the COMMIT running) until `<prefix>.go` exists; `<prefix>.reached` announces it. The
    /// prefix is read from `MINDSPACE_STORE_GATE_PAUSE_IN_COMMIT`.
    pub static PAUSE_IN_COMMIT: AtomicBool = AtomicBool::new(false);
}

/// Facts about a database file that `mind doctor` needs, read without touching
/// the file (see [`inspect_database`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseInspection {
    /// Applied migration versions, ascending. Empty when the file has no
    /// `schema_migrations` table.
    pub schema_versions: Vec<i64>,
    /// The newest schema this binary supports.
    pub supported_version: i64,
    pub embedding_enabled: Option<String>,
    pub embedding_model: Option<String>,
    /// The inspection had to see a write-ahead log (`mode=ro`); otherwise the
    /// file was opened `immutable` and no auxiliary file was created.
    pub saw_wal: bool,
}

/// Reads schema versions and embedding settings WITHOUT migrating or writing.
///
/// A normal open runs migrations, so it cannot be used to ask "is this
/// database compatible?". Two modes, chosen by whether a `-wal` file exists:
///  - no WAL file: the file is opened `immutable=1`. SQLite then creates no
///    `-wal`/`-shm` file (a plain read-only open of a WAL-mode database leaves
///    both behind) and there is no log to ignore;
///  - WAL file present (a server may be writing): `mode=ro`, which reads the log
///    and, because the files already exist, creates nothing new.
///
/// A writer that starts between the check and the open is not detected; the
/// result is a snapshot.
pub fn inspect_database(path: &Path) -> Result<DatabaseInspection> {
    let abs = std::path::absolute(path).map_err(|e| Error::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    let mut wal = abs.as_os_str().to_owned();
    wal.push("-wal");
    let wal = std::path::PathBuf::from(wal);
    let saw_wal = wal.exists();
    match read_inspection(&abs, saw_wal) {
        Ok(i) => Ok(i),
        // A writer may have started after the WAL check: an `immutable` read of a
        // file that is changing can fail. Look again, now through the log.
        Err(_) if !saw_wal && wal.exists() => read_inspection(&abs, true),
        Err(e) => Err(e),
    }
}

fn read_inspection(abs: &Path, with_wal: bool) -> Result<DatabaseInspection> {
    // The path goes into a URI as bytes: everything but unreserved characters is
    // percent-encoded, so spaces, `?`, `#`, `%` and non-UTF-8 names all name the
    // file they are meant to (SQLite decodes %HH into the raw bytes).
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file:");
    for &byte in abs.as_os_str().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'.' | b'_' | b'-' | b'~' => {
                uri.push(byte as char)
            }
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
    uri.push_str(if with_wal { "?mode=ro" } else { "?immutable=1" });
    let conn = Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| Error::Storage(format!("cannot open {}: {e}", abs.display())))?;
    // One read transaction: schema versions and settings come from the same snapshot.
    let tx = conn.unchecked_transaction().map_err(dberr)?;
    let has_table = |name: &str| -> Result<bool> {
        Ok(tx
            .query_row(
                "SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1",
                rusqlite::params![name],
                |_| Ok(()),
            )
            .optional()
            .map_err(dberr)?
            .is_some())
    };
    let schema_versions = if has_table("schema_migrations")? {
        let mut stmt = tx
            .prepare("SELECT version FROM schema_migrations ORDER BY version")
            .map_err(dberr)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, i64>(0))
            .map_err(dberr)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(dberr)?;
        rows
    } else {
        Vec::new()
    };
    let (mut embedding_enabled, mut embedding_model) = (None, None);
    if has_table("mind_meta")? {
        let get = |key: &str| -> Result<Option<String>> {
            tx.query_row(
                "SELECT value FROM mind_meta WHERE key = ?1",
                rusqlite::params![key],
                |r| r.get(0),
            )
            .optional()
            .map_err(dberr)
        };
        embedding_enabled = get("embedding.enabled")?;
        embedding_model = get("embedding.model")?;
    }
    Ok(DatabaseInspection {
        schema_versions,
        supported_version: CURRENT_SCHEMA_VERSION,
        embedding_enabled,
        embedding_model,
        saw_wal: with_wal,
    })
}

impl SqliteStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| Error::Io {
                    path: parent.display().to_string(),
                    source: e,
                })?;
            }
        }
        let conn = Connection::open(path)
            .map_err(|e| Error::Storage(format!("cannot open database {}: {e}", path.display())))?;
        Self::init_conn(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(dberr)?;
        Self::init_conn(conn)
    }

    fn init_conn(conn: Connection) -> Result<Self> {
        let revision_state = Arc::new(RevisionState {
            active_revision: AtomicI64::new(0),
            poisoned: AtomicBool::new(false),
        });
        conn.busy_timeout(Duration::from_secs(5)).map_err(dberr)?;
        conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
            .map_err(dberr)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(dberr)?;
        register_revision_context(&conn, &revision_state)?;
        let ownership_state = Arc::new(ownership::OwnershipState::default());
        ownership::register_context(&conn, &ownership_state)?;
        let preparation_state = Arc::new(document_preparation::PreparationState::default());
        document_preparation::register_context(&conn, &preparation_state)?;
        let lexical_state = Arc::new(lexical_staging::LexicalState::default());
        lexical_staging::register_context(&conn, &lexical_state)?;
        let semantic_state = Arc::new(semantic_staging::SemanticState::default());
        semantic_staging::register_context(&conn, &semantic_state)?;
        let fence_plan_state = Arc::new(fence_plans::FencePlanState::default());
        fence_plans::register_context(&conn, &fence_plan_state)?;
        reject_future_schema(&conn)?;
        run_migrations(&conn, &revision_state)?;
        validate_fence_inventory(&conn)?;
        let fts_integrity = verify_fts_counts(&conn).is_ok();
        Ok(Self {
            conn: Mutex::new(Some(conn)),
            revision_state,
            ownership_state,
            preparation_state,
            lexical_state,
            semantic_state,
            fence_plan_state,
            fts_integrity: AtomicBool::new(fts_integrity),
        })
    }

    /// Inspect passive metadata with normal SQLite locking, without migrating.
    #[cfg(unix)]
    pub(crate) fn open_fence_plan_inspection(
        ctx: &crate::cli::ownership::AdminContext,
    ) -> Result<Self> {
        let conn = ownership::open_admin_read_only(ctx, |conn| {
            let version: i64 = conn
                .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                    r.get(0)
                })
                .map_err(dberr)?;
            if version != CURRENT_SCHEMA_VERSION {
                return Err(Error::Storage("passive reads require checkpointed initialized schema 17; perform a separate trusted upgrade/checkpoint; read commands never migrate or recover".into()));
            }
            validate_fence_inventory(conn)
        })?;
        let store = Self {
            conn: Mutex::new(Some(conn)),
            revision_state: Arc::new(RevisionState {
                active_revision: AtomicI64::new(0),
                poisoned: AtomicBool::new(false),
            }),
            ownership_state: Arc::new(ownership::OwnershipState::default()),
            preparation_state: Arc::new(document_preparation::PreparationState::default()),
            lexical_state: Arc::new(lexical_staging::LexicalState::default()),
            semantic_state: Arc::new(semantic_staging::SemanticState::default()),
            fence_plan_state: Arc::new(fence_plans::FencePlanState::default()),
            fts_integrity: AtomicBool::new(true),
        };
        ctx.validate()?;
        Ok(store)
    }

    fn connection(&self) -> Result<ConnectionGuard<'_>> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| Error::Storage("SQLite connection lock is poisoned".to_string()))?;
        if guard.is_none() {
            return Err(Error::ConnectionDiscarded);
        }
        Ok(ConnectionGuard {
            guard,
            revision_state: Arc::clone(&self.revision_state),
        })
    }

    pub fn verify_fts_index(&self) -> Result<()> {
        let conn = self.connection()?;
        let result = verify_fts_index(&conn);
        self.fts_integrity.store(result.is_ok(), Ordering::SeqCst);
        result
    }

    pub fn rebuild_fts_index(&self) -> Result<()> {
        let conn = self.connection()?;
        let tx = begin_write(&conn, &self.revision_state)?;
        tx.execute("DELETE FROM assertion_fts", []).map_err(dberr)?;
        tx.execute(
            "INSERT INTO assertion_fts (assertion_id, subject, content) \
             SELECT id, subject, content FROM assertions ORDER BY id",
            [],
        )
        .map_err(dberr)?;
        verify_fts_index(&tx)?;
        tx.commit()?;
        self.fts_integrity.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// A deliberately small, commit-local Rust module view.  It is assembled
/// solely from persisted parser output and `code_contexts`; it never reads a
/// repository checkout.  File paths become modules only when a reachable
/// `mod name;`-shaped structural declaration points at them.
struct RustModuleIndex {
    reachable_paths: BTreeSet<String>,
    file_modules: BTreeMap<String, Vec<String>>,
    entity_modules: BTreeMap<(String, String), Vec<String>>,
    entities: BTreeMap<String, Vec<(String, String)>>,
}

impl RustModuleIndex {
    fn module_for_observation(&self, path: &str, syntax_path: Option<&str>) -> Option<Vec<String>> {
        let file = self.file_modules.get(path)?.clone();
        let Some(syntax_path) = syntax_path else {
            return Some(file);
        };
        self.entity_modules
            .get(&(path.to_string(), syntax_path.to_string()))
            .cloned()
            .or(Some(file))
    }

    fn candidates(&self, target: &[String], relationship_kind: &str) -> Vec<String> {
        let key = target.join("::");
        let allowed: &[&str] = match relationship_kind {
            "calls" => &["function"],
            "implements_trait" => &["trait"],
            "implements_type" => &["struct", "enum", "type_alias"],
            "imports" => &[
                "function",
                "struct",
                "trait",
                "enum",
                "type_alias",
                "module",
                "constant",
                "static",
            ],
            _ => &[],
        };
        self.entities
            .get(&key)
            .into_iter()
            .flatten()
            .filter(|(_, kind)| allowed.contains(&kind.as_str()))
            .map(|(id, _)| id.clone())
            .collect()
    }
}

fn is_internal_rust_path(raw: &str) -> bool {
    raw == "crate"
        || raw == "self"
        || raw == "super"
        || raw.starts_with("crate::")
        || raw.starts_with("self::")
        || raw.starts_with("super::")
}

fn rust_target_path(raw: &str, current_module: &[String]) -> Option<Vec<String>> {
    let parts: Vec<&str> = raw.split("::").filter(|part| !part.is_empty()).collect();
    let (mut target, suffix) = match parts.first().copied()? {
        "crate" => (Vec::new(), &parts[1..]),
        "self" => (current_module.to_vec(), &parts[1..]),
        "super" => {
            let mut base = current_module.to_vec();
            let mut at = 0;
            while parts.get(at) == Some(&"super") {
                base.pop()?;
                at += 1;
            }
            (base, &parts[at..])
        }
        // Calls with a bare identifier are the only unqualified form v0.5
        // attempts.  `foo::bar` needs imports/name resolution and remains
        // explicitly unknown.
        _bare if parts.len() == 1 => (current_module.to_vec(), &parts[..]),
        _ => return None,
    };
    target.extend(suffix.iter().map(|part| (*part).to_string()));
    (!target.is_empty()).then_some(target)
}

fn rust_file_module(path: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 2 || parts.first() != Some(&"src") || !path.ends_with(".rs") {
        return None;
    }
    let stem = parts.last()?.strip_suffix(".rs")?;
    if matches!(stem, "lib" | "main") && parts.len() == 2 {
        return Some(vec![]);
    }
    let mut module: Vec<String> = parts[1..parts.len() - 1]
        .iter()
        .map(|part| (*part).to_string())
        .collect();
    if stem != "mod" {
        module.push(stem.to_string());
    }
    (!module.is_empty()).then_some(module)
}

fn append_qualified(base: &[String], relative: Option<&str>) -> Vec<String> {
    let mut result = base.to_vec();
    if let Some(relative) = relative {
        result.extend(
            relative
                .split("::")
                .filter(|part| !part.is_empty())
                .map(str::to_string),
        );
    }
    result
}

fn rust_module_index(
    tx: &Connection,
    repository_id: Uuid,
    commit_oid: &str,
) -> Result<RustModuleIndex> {
    #[derive(Clone)]
    struct Row {
        path: String,
        id: String,
        kind: String,
        syntax: String,
        qualified: Option<String>,
    }
    let mut stmt = tx.prepare(&format!("SELECT DISTINCT c.path,e.id,e.kind,e.syntax_path,e.qualified_name FROM code_contexts c JOIN code_inputs i ON i.id=c.input_id JOIN code_derivations d ON d.input_id=i.id JOIN code_entities e ON e.derivation_id=d.id WHERE i.repository_id=?1 AND c.commit_oid=?2 AND {EFFECTIVE_DERIVATION} ORDER BY c.path,e.syntax_path,e.id")).map_err(dberr)?;
    let rows = stmt
        .query_map(
            rusqlite::params![repository_id.to_string(), commit_oid],
            |r| {
                Ok(Row {
                    path: r.get(0)?,
                    id: r.get(1)?,
                    kind: r.get(2)?,
                    syntax: r.get(3)?,
                    qualified: r.get(4)?,
                })
            },
        )
        .map_err(dberr)?;
    let mut by_path: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    for row in rows {
        let row = row.map_err(dberr)?;
        by_path.entry(row.path.clone()).or_default().push(row);
    }
    drop(stmt);

    let mut file_modules = BTreeMap::new();
    let mut reachable_paths = BTreeSet::new();
    for path in by_path.keys() {
        if rust_file_module(path).as_deref() == Some(&[]) {
            file_modules.insert(path.clone(), vec![]);
            reachable_paths.insert(path.clone());
        }
    }
    // Module declarations in reachable files may make the conventional
    // `foo.rs` / `foo/mod.rs` context reachable.  Iteration is ordered and
    // terminates because only finitely many persisted contexts exist.
    loop {
        let mut changed = false;
        let paths: Vec<String> = reachable_paths.iter().cloned().collect();
        for path in paths {
            let base = file_modules[&path].clone();
            for row in &by_path[&path] {
                if row.kind != "module" {
                    continue;
                }
                let declared = append_qualified(&base, row.qualified.as_deref());
                for (candidate, candidate_base) in by_path.keys().filter_map(|candidate| {
                    rust_file_module(candidate).map(|base| (candidate, base))
                }) {
                    if candidate_base == declared && reachable_paths.insert(candidate.clone()) {
                        file_modules.insert(candidate.clone(), candidate_base);
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    let mut entity_modules = BTreeMap::new();
    let mut entities: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for path in &reachable_paths {
        let base = file_modules[path].clone();
        for row in &by_path[path] {
            let canonical = append_qualified(&base, row.qualified.as_deref());
            let module = if row.kind == "module" {
                canonical.clone()
            } else {
                canonical[..canonical.len().saturating_sub(1)].to_vec()
            };
            entity_modules.insert((path.clone(), row.syntax.clone()), module);
            if row.qualified.is_some() && !canonical.is_empty() {
                entities
                    .entry(canonical.join("::"))
                    .or_default()
                    .push((row.id.clone(), row.kind.clone()));
            }
        }
    }
    for values in entities.values_mut() {
        values.sort();
        values.dedup();
    }
    Ok(RustModuleIndex {
        reachable_paths,
        file_modules,
        entity_modules,
        entities,
    })
}

fn dberr(e: rusqlite::Error) -> Error {
    if let rusqlite::Error::SqliteFailure(code, _) = &e {
        if matches!(
            code.code,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
        ) {
            return Error::DatabaseBusy {
                detail: e.to_string(),
            };
        }
    }
    Error::Storage(e.to_string())
}

fn register_revision_context(conn: &Connection, state: &Arc<RevisionState>) -> Result<()> {
    let function_state = Arc::clone(state);
    conn.create_scalar_function(
        "m6_write_revision",
        0,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS,
        move |_| {
            let revision = function_state.active_revision.load(Ordering::SeqCst);
            Ok((revision > 0).then_some(revision))
        },
    )
    .map_err(dberr)?;

    let commit_state = Arc::clone(state);
    conn.commit_hook(Some(move || {
        commit_state.active_revision.store(0, Ordering::SeqCst);
        #[cfg(feature = "spawn-gate")]
        if gate::FAIL_COMMIT.swap(false, Ordering::SeqCst) {
            return true; // veto: SQLite turns this COMMIT into a ROLLBACK
        }
        #[cfg(feature = "spawn-gate")]
        if gate::PAUSE_IN_COMMIT.swap(false, Ordering::SeqCst) {
            if let Some(prefix) = std::env::var_os("MINDSPACE_STORE_GATE_PAUSE_IN_COMMIT") {
                let with = |ext: &str| {
                    let mut p = prefix.clone();
                    p.push(ext);
                    std::path::PathBuf::from(p)
                };
                let _ = std::fs::write(with(".reached"), b"");
                let end = std::time::Instant::now() + Duration::from_secs(60);
                while !with(".go").exists() && std::time::Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        false
    }))
    .map_err(dberr)?;
    let rollback_state = Arc::clone(state);
    conn.rollback_hook(Some(move || {
        rollback_state.active_revision.store(0, Ordering::SeqCst);
    }))
    .map_err(dberr)?;
    Ok(())
}

fn begin_write<'conn>(
    conn: &'conn Connection,
    state: &Arc<RevisionState>,
) -> Result<WriteTransaction<'conn>> {
    if state.poisoned.load(Ordering::SeqCst) {
        return Err(Error::ConnectionDiscarded);
    }
    if !conn.is_autocommit() || state.active_revision.load(Ordering::SeqCst) != 0 {
        return Err(Error::Storage(
            "nested or unmanaged SQLite write transaction is not allowed".to_string(),
        ));
    }

    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(dberr)?;
    let last_revision: Option<i64> = tx
        .query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |row| {
            row.get(0)
        })
        .map_err(dberr)?;
    let revision_id = last_revision
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| Error::Storage("recording revision space exhausted".to_string()))?;
    if state
        .active_revision
        .compare_exchange(0, revision_id, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        rollback_unowned_transaction(tx, conn, state);
        return Err(Error::Storage(
            "connection already has an active recording revision".to_string(),
        ));
    }
    if let Err(error) = tx.execute(
        "INSERT INTO m6_revisions (revision_id, recorded_at, kind) VALUES (?1, ?2, 'write')",
        rusqlite::params![revision_id, Ts::now().0],
    ) {
        state.active_revision.store(0, Ordering::SeqCst);
        rollback_unowned_transaction(tx, conn, state);
        return Err(dberr(error));
    }
    Ok(WriteTransaction {
        tx: Some(tx),
        conn,
        revision_state: Arc::clone(state),
    })
}

fn rollback_unowned_transaction(tx: Transaction<'_>, conn: &Connection, state: &RevisionState) {
    state.active_revision.store(0, Ordering::SeqCst);
    if tx.rollback().is_err() || !conn.is_autocommit() {
        state.poisoned.store(true, Ordering::SeqCst);
    }
}

/// Shared filter clause for lexical and vector search: temporal `at`,
/// active/superseded/retired visibility, and space membership. Both search
/// paths call this so space/temporal/lifecycle semantics stay identical.
/// Pushes its parameters onto `params` and returns the SQL fragment with
/// numbered placeholders. The empty-space-list case ("matches nothing") is
/// handled by callers before this is invoked.
fn build_search_filters(query: &SearchQuery, params: &mut Vec<Value>) -> String {
    let mut sql = String::new();
    if let Some(at) = query.at {
        let i1 = params.len() + 1;
        let i2 = i1 + 1;
        let i3 = i1 + 2;
        sql.push_str(&format!(
            " AND m.created_at <= ?{i1} AND (m.superseded_at IS NULL OR m.superseded_at > ?{i2}) \
             AND (m.retired_at IS NULL OR m.retired_at > ?{i3})"
        ));
        params.push(Value::Integer(at.0));
        params.push(Value::Integer(at.0));
        params.push(Value::Integer(at.0));
    } else {
        let mut states = vec!["active"];
        if query.include_superseded {
            states.push("superseded");
        }
        if query.include_retired {
            states.push("retired");
        }
        let list = states
            .iter()
            .map(|s| format!("'{s}'"))
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&format!(" AND m.state IN ({list})"));
    }
    if let Some(ids) = &query.space_ids {
        if !ids.is_empty() {
            let mut id_params = Vec::new();
            for id in ids {
                params.push(Value::Text(id.to_string()));
                id_params.push(format!("?{}", params.len()));
            }
            sql.push_str(&format!(
                " AND m.id IN (SELECT memory_id FROM memory_spaces WHERE space_id IN ({}))",
                id_params.join(", ")
            ));
        }
    }
    sql
}

/// Deterministic content window for non-lexical search hits (semantic/hybrid
/// vectors that are not also an FTS5 match). Takes the first non-empty line
/// region, trimmed, up to 160 chars, with a trailing ellipsis when truncated.
fn content_window(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.chars().count() <= 160 {
        return trimmed.to_string();
    }
    let mut out = String::new();
    for ch in trimmed.chars().take(160) {
        out.push(ch);
    }
    out.push('…');
    out
}

/// FTS5 reports bad query syntax through its error message; surface that as
/// the dedicated user-facing error instead of a generic storage failure.
fn classify_query_err(e: rusqlite::Error) -> Error {
    let s = e.to_string();
    // FTS5 reports malformed column-style expressions such as `absent-term`
    // as "no such column: term" rather than prefixing the error with `fts5:`.
    // This function is only used for MATCH query execution, so both forms
    // are syntax errors eligible for the literal-phrase fallback.
    if s.contains("fts5:") || s.contains("no such column:") {
        Error::QuerySyntax(s)
    } else {
        Error::Storage(s)
    }
}

fn reject_future_schema(conn: &Connection) -> Result<()> {
    let has_migration_table: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='schema_migrations'",
            [],
            |_| Ok(()),
        )
        .optional()
        .map_err(dberr)?
        .is_some();
    if !has_migration_table {
        return Ok(());
    }
    let future: Option<i64> = conn
        .query_row(
            "SELECT MAX(version) FROM schema_migrations WHERE version > ?1",
            rusqlite::params![CURRENT_SCHEMA_VERSION],
            |row| row.get(0),
        )
        .map_err(dberr)?;
    if let Some(version) = future {
        return Err(Error::Storage(format!(
            "database schema version {version} is newer than supported version {CURRENT_SCHEMA_VERSION}"
        )));
    }
    Ok(())
}

fn validate_migration_prefix(conn: &Connection) -> Result<()> {
    let has_migration_table: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='schema_migrations'",
            [],
            |_| Ok(()),
        )
        .optional()
        .map_err(dberr)?
        .is_some();
    if !has_migration_table {
        return Ok(());
    }
    let mut stmt = conn
        .prepare("SELECT version FROM schema_migrations ORDER BY version")
        .map_err(dberr)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, i64>(0))
        .map_err(dberr)?;
    for (expected, row) in (1_i64..).zip(rows) {
        let version = row.map_err(dberr)?;
        if version != expected {
            return Err(Error::Storage(format!(
                "schema migration history is not a contiguous prefix at version {expected}"
            )));
        }
    }
    Ok(())
}

/// A migration-owned revision scope. It never starts or commits a nested transaction.
struct MigrationRevisionScope {
    state: Arc<RevisionState>,
}

impl MigrationRevisionScope {
    fn enter(conn: &Connection, state: &Arc<RevisionState>) -> Result<Self> {
        if conn.is_autocommit() || state.poisoned.load(Ordering::SeqCst) {
            return Err(Error::Storage(
                "metadata migration requires a healthy owned transaction".into(),
            ));
        }
        let previous: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(revision_id),0) FROM m6_revisions",
                [],
                |r| r.get(0),
            )
            .map_err(dberr)?;
        let revision = previous
            .checked_add(1)
            .ok_or_else(|| Error::Storage("recording revision space exhausted".into()))?;
        state
            .active_revision
            .compare_exchange(0, revision, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| {
                Error::Storage(
                    "metadata migration cannot replace active write authorization".into(),
                )
            })?;
        let scope = Self {
            state: Arc::clone(state),
        };
        conn.execute(
            "INSERT INTO m6_revisions(revision_id,recorded_at,kind) VALUES(?1,?2,'write')",
            rusqlite::params![revision, Ts::now().0],
        )
        .map_err(dberr)?;
        Ok(scope)
    }
}

impl Drop for MigrationRevisionScope {
    fn drop(&mut self) {
        self.state.active_revision.store(0, Ordering::SeqCst);
    }
}

fn run_migrations(conn: &Connection, state: &Arc<RevisionState>) -> Result<()> {
    reject_future_schema(conn)?;
    validate_migration_prefix(conn)?;
    conn.pragma_update(None, "foreign_keys", "OFF")
        .map_err(dberr)?;

    let metadata_committed = std::cell::Cell::new(false);
    let migration_result = (|| {
        let has_migration_table: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='schema_migrations'",
                [],
                |_| Ok(()),
            )
            .optional()
            .map_err(dberr)?
            .is_some();
        if !has_migration_table {
            let tx =
                Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(dberr)?;
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                     version INTEGER PRIMARY KEY,
                     applied_at INTEGER NOT NULL
                 )",
            )
            .map_err(dberr)?;
            tx.commit().map_err(dberr)?;
        }

        for (version, sql) in MIGRATIONS {
            let tx =
                Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(dberr)?;
            reject_future_schema(&tx)?;
            let already_applied: bool = tx
                .query_row(
                    "SELECT 1 FROM schema_migrations WHERE version=?1",
                    rusqlite::params![version],
                    |_| Ok(()),
                )
                .optional()
                .map_err(dberr)?
                .is_some();
            if already_applied {
                tx.commit().map_err(dberr)?;
                continue;
            }
            // Ownership metadata records its own birth in this already-owned transaction.
            // Migration 8 must remain the sole installer of the original baseline/fences.
            let metadata_scope = if matches!(*version, 13..=17) {
                Some(MigrationRevisionScope::enter(&tx, state)?)
            } else {
                None
            };
            tx.execute_batch(sql).map_err(dberr)?;
            if *version == M6_VERSION {
                apply_m6_baseline(&tx)?;
            }
            tx.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                rusqlite::params![version, Ts::now().0],
            )
            .map_err(dberr)?;
            if *version == M6_VERSION {
                install_m6_write_fence(&tx)?;
                validate_fence_inventory(&tx)?;
            }
            if matches!(*version, 13..=17) {
                let (table, key) = match *version {
                    13 => ("asset_ownership_schema", "local-enroll@1"),
                    14 => ("document_preparation_schema", "local-doc-prepare@1"),
                    15 => ("lexical_staging_schema", "local-doc-stage-lexical@1"),
                    16 => ("semantic_staging_schema", "local-doc-stage-semantic@1"),
                    _ => ("fence_plan_schema", "local-fence-plan@1"),
                };
                tx.execute("INSERT INTO m6_row_availability(table_name,entity_key,revision_id) VALUES(?1,?2,m6_write_revision())", [table,key]).map_err(dberr)?;
                let violations: i64 = tx
                    .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                        row.get(0)
                    })
                    .map_err(dberr)?;
                if violations != 0 {
                    return Err(Error::Storage(
                        "ownership metadata migration failed foreign_key_check".into(),
                    ));
                }
                validate_fence_inventory(&tx)?;
                // Test-only pause uses the existing SQLite COMMIT hook; absent in production.
                #[cfg(feature = "spawn-gate")]
                if *version == 17 {
                    if let Some(prefix) =
                        std::env::var_os("MINDSPACE_FENCE_PLAN_MIGRATION_GATE_PATH")
                    {
                        std::env::set_var("MINDSPACE_STORE_GATE_PAUSE_IN_COMMIT", prefix);
                        gate::PAUSE_IN_COMMIT.store(true, Ordering::SeqCst);
                    }
                }
                WriteTransaction {
                    tx: Some(tx),
                    conn,
                    revision_state: Arc::clone(state),
                }
                .commit()?;
                metadata_committed.set(true);
            } else {
                tx.commit().map_err(dberr)?;
            }
            drop(metadata_scope);
        }
        validate_migration_prefix(conn)?;
        let current: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .map_err(dberr)?;
        if current != CURRENT_SCHEMA_VERSION {
            return Err(Error::Storage(format!(
                "schema migration stopped at version {current}, expected {CURRENT_SCHEMA_VERSION}"
            )));
        }
        Ok(())
    })();

    let restore_result = conn
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(dberr);
    if let Err(error) = migration_result {
        if metadata_committed.get() {
            state.poisoned.store(true, Ordering::SeqCst);
            return Err(Error::CommitOutcomeUnknown {
                detail: format!(
                    "ownership metadata committed but final verification failed: {error}"
                ),
            });
        }
        return Err(error);
    }
    if let Err(error) = restore_result {
        if metadata_committed.get() {
            state.poisoned.store(true, Ordering::SeqCst);
            return Err(Error::CommitOutcomeUnknown {
                detail: format!(
                    "ownership metadata committed but connection restoration failed: {error}"
                ),
            });
        }
        return Err(error);
    }
    Ok(())
}

const FENCED_TABLES: &[&str] = &[
    "spaces",
    "space_links",
    "sources",
    "evidence",
    "assertions",
    "memories",
    "memory_spaces",
    "logical_sources",
    "ingest_sessions",
    "source_events",
    "source_observations",
    "repositories",
    "git_commits",
    "git_commit_parents",
    "git_trees",
    "git_tree_entries",
    "git_blobs",
    "git_tags",
    "git_refs",
    "git_ref_events",
    "git_source_versions",
    "code_inputs",
    "code_derivations",
    "code_contexts",
    "code_entities",
    "code_imports",
    "code_relationship_observations",
    "code_resolution_derivations",
    "code_relationship_resolutions",
    "code_resolution_candidates",
    "assertion_embeddings",
    "mind_meta",
    "m6_history_coverage",
    "m6_row_availability",
    "m6_memory_history",
    "m6_space_history",
    "m6_space_link_history",
    "m6_memory_space_history",
    "m6_logical_source_history",
    "m6_repository_history",
    "m6_git_ref_history",
];

/// The one rule for which derivation of a code input is current: the greatest `(created_at, id)`
/// over all of its derivations, whatever their identity. Exactly one row matches (the id breaks
/// ties), so symbol search, relationship observation/resolution, entity listing and `structure`
/// agree. `d` is the derivation alias of the enclosing query.
const EFFECTIVE_DERIVATION: &str = "d.id = (SELECT d2.id FROM code_derivations d2 WHERE d2.input_id = d.input_id ORDER BY d2.created_at DESC, d2.id DESC LIMIT 1)";

const IMMUTABLE_TABLES: &[&str] = &[
    "sources",
    "evidence",
    "assertions",
    "ingest_sessions",
    "source_events",
    "source_observations",
    "git_commits",
    "git_commit_parents",
    "git_trees",
    "git_tree_entries",
    "git_blobs",
    "git_tags",
    "git_ref_events",
    "git_source_versions",
    "code_inputs",
    "code_derivations",
    "code_contexts",
    "code_entities",
    "code_imports",
    "code_relationship_observations",
    "code_resolution_derivations",
    "code_relationship_resolutions",
    "code_resolution_candidates",
];

const ROW_AVAILABILITY_KEYS: &[(&str, &str)] = &[
    ("sources", "{id}"),
    ("evidence", "{id}"),
    ("assertions", "{id}"),
    ("ingest_sessions", "{id}"),
    ("source_events", "{id}"),
    (
        "source_observations",
        "hex(CAST({session_id} AS BLOB)) || ':' || hex(CAST({location} AS BLOB))",
    ),
    (
        "git_commits",
        "hex(CAST({repository_id} AS BLOB)) || ':' || hex(CAST({oid} AS BLOB))",
    ),
    (
        "git_commit_parents",
        "hex(CAST({repository_id} AS BLOB)) || ':' || hex(CAST({commit_oid} AS BLOB)) || ':' || hex(CAST({position} AS BLOB))",
    ),
    (
        "git_trees",
        "hex(CAST({repository_id} AS BLOB)) || ':' || hex(CAST({oid} AS BLOB))",
    ),
    (
        "git_tree_entries",
        "hex(CAST({repository_id} AS BLOB)) || ':' || hex(CAST({tree_oid} AS BLOB)) || ':' || hex(CAST({name} AS BLOB))",
    ),
    (
        "git_blobs",
        "hex(CAST({repository_id} AS BLOB)) || ':' || hex(CAST({oid} AS BLOB))",
    ),
    (
        "git_tags",
        "hex(CAST({repository_id} AS BLOB)) || ':' || hex(CAST({oid} AS BLOB))",
    ),
    ("git_ref_events", "{id}"),
    ("git_source_versions", "{source_id}"),
];

fn availability_key_expr(table: &str, key: &str, prefix: &str) -> String {
    let expr = ROW_AVAILABILITY_KEYS
        .iter()
        .find(|(name, _)| *name == table)
        .map(|(_, expr)| *expr)
        .unwrap_or(key);
    let owned = expr.to_string();
    if !owned.contains('{') {
        return owned;
    }
    let columns = owned
        .split('{')
        .skip(1)
        .filter_map(|part| part.split('}').next())
        .map(String::from)
        .collect::<Vec<_>>();
    let mut out = owned;
    for column in columns {
        out = out.replace(&format!("{{{column}}}"), &format!("{prefix}{column}"));
    }
    out
}

const TEMPORAL_COVERAGE: &[(TemporalDimension, bool, &str)] = &[
    (TemporalDimension::BeliefLifecycle, true, ""),
    (TemporalDimension::SpaceState, true, ""),
    (TemporalDimension::SpaceMembership, true, ""),
    (TemporalDimension::SpaceLinks, true, ""),
    (TemporalDimension::SourceState, true, ""),
    (TemporalDimension::RepositoryState, true, ""),
    (TemporalDimension::GitRefs, true, ""),
    (TemporalDimension::CanonicalProvenance, true, ""),
    (TemporalDimension::Observations, true, ""),
    (
        TemporalDimension::WorldValidity,
        false,
        "no world-validity assertion model is implemented",
    ),
    (
        TemporalDimension::CodeStructure,
        false,
        "code derivations and associations do not yet have complete temporal reconstruction",
    ),
    (
        TemporalDimension::EmbeddingIndex,
        false,
        "embedding availability and model execution are current-index semantics only",
    ),
    (
        TemporalDimension::FtsRanking,
        false,
        "FTS index ranking is derived and is not replayed historically",
    ),
    (
        TemporalDimension::Configuration,
        false,
        "database configuration is not historical knowledge",
    ),
];

fn apply_m6_baseline(tx: &Transaction<'_>) -> Result<()> {
    const BASELINE: i64 = 1;
    tx.execute(
        "INSERT INTO m6_revisions (revision_id, recorded_at, kind) VALUES (?1, ?2, 'baseline')",
        rusqlite::params![BASELINE, Ts::now().0],
    )
    .map_err(dberr)?;
    tx.execute(
        "UPDATE memories SET producer_family = CASE \
           WHEN (SELECT ls.kind FROM logical_sources ls JOIN assertions a ON a.id=memories.assertion_id \
                 WHERE ls.id=(SELECT s.logical_id FROM evidence e JOIN sources s ON s.id=e.source_id \
                              WHERE e.id=a.evidence_id)) = 'manual' \
                AND (SELECT extraction FROM assertions WHERE id=memories.assertion_id)='manual:write' \
             THEN 'manual:write' \
           WHEN (SELECT ls.kind FROM logical_sources ls JOIN assertions a ON a.id=memories.assertion_id \
                 WHERE ls.id=(SELECT s.logical_id FROM evidence e JOIN sources s ON s.id=e.source_id \
                              WHERE e.id=a.evidence_id)) = 'file' \
                AND (SELECT extraction FROM assertions WHERE id=memories.assertion_id) IN \
                    ('markdown:section-heading','markdown:preamble','markdown:document','text:document') \
             THEN 'file:legacy' \
           WHEN (SELECT ls.kind FROM logical_sources ls JOIN assertions a ON a.id=memories.assertion_id \
                 WHERE ls.id=(SELECT s.logical_id FROM evidence e JOIN sources s ON s.id=e.source_id \
                              WHERE e.id=a.evidence_id)) = 'git_path' \
                AND (SELECT extraction FROM assertions WHERE id=memories.assertion_id) LIKE 'git:%' \
             THEN 'git:legacy' \
           ELSE 'legacy:isolated:' || id END",
        [],
    )
    .map_err(dberr)?;

    seed_m6_availability(tx, BASELINE)?;
    seed_m6_histories(tx, BASELINE)?;
    for (dimension, covered, reason) in TEMPORAL_COVERAGE {
        tx.execute(
            "INSERT INTO m6_history_coverage \
             (dimension, baseline_revision, complete_from_revision, status, reason) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                dimension.as_str(),
                BASELINE,
                covered.then_some(BASELINE),
                if *covered { "covered" } else { "unsupported" },
                (!covered).then_some(*reason),
            ],
        )
        .map_err(dberr)?;
    }
    install_immutable_triggers(tx)?;
    install_availability_triggers(tx)?;
    install_snapshot_triggers(tx)?;
    verify_fts_index(tx)?;
    Ok(())
}

fn seed_m6_availability(tx: &Transaction<'_>, baseline: i64) -> Result<()> {
    for (table, key) in ROW_AVAILABILITY_KEYS {
        let expr = availability_key_expr(table, key, "");
        let sql = format!(
            "INSERT INTO m6_row_availability(table_name,entity_key,revision_id) \
             SELECT '{table}', CAST(({expr}) AS TEXT), {baseline} FROM {table}"
        );
        tx.execute(&sql, []).map_err(dberr)?;
    }
    Ok(())
}

fn seed_m6_histories(tx: &Transaction<'_>, baseline: i64) -> Result<()> {
    let queries = [
        format!(
            "INSERT INTO m6_memory_history \
             SELECT id,{baseline},0,assertion_id,state,created_at,superseded_by,superseded_at,retired_at,retire_reason,producer_family FROM memories"
        ),
        format!(
            "INSERT INTO m6_space_history \
             SELECT id,{baseline},0,name,description,created_at FROM spaces"
        ),
        format!(
            "INSERT INTO m6_space_link_history \
             SELECT from_space,to_space,{baseline},0,created_at FROM space_links"
        ),
        format!(
            "INSERT INTO m6_memory_space_history \
             SELECT memory_id,space_id,{baseline},0,created_at FROM memory_spaces"
        ),
        format!(
            "INSERT INTO m6_logical_source_history \
             SELECT id,{baseline},0,kind,state,current_location,created_at,repository_id,commit_oid,git_path FROM logical_sources"
        ),
        format!(
            "INSERT INTO m6_repository_history \
             SELECT id,{baseline},0,current_path,created_at FROM repositories"
        ),
        format!(
            "INSERT INTO m6_git_ref_history \
             SELECT repository_id,name,{baseline},0,target_oid,target_kind,symbolic_target,last_observed_at FROM git_refs"
        ),
    ];
    for sql in queries {
        tx.execute(&sql, []).map_err(dberr)?;
    }
    Ok(())
}

fn install_immutable_triggers(conn: &Connection) -> Result<()> {
    for table in IMMUTABLE_TABLES {
        for (suffix, event) in [("u", "UPDATE"), ("d", "DELETE")] {
            conn.execute_batch(&format!(
                "CREATE TRIGGER m6_immutable_{table}_{suffix} BEFORE {event} ON {table} \
                 BEGIN SELECT RAISE(ABORT, '{table} rows are append-only'); END;"
            ))
            .map_err(dberr)?;
        }
    }
    conn.execute_batch(
        "CREATE TRIGGER m6_memory_owner_immutable \
         BEFORE UPDATE OF producer_family ON memories \
         WHEN NEW.producer_family IS NOT OLD.producer_family \
         BEGIN SELECT RAISE(ABORT, 'memory producer family is immutable'); END;",
    )
    .map_err(dberr)?;
    Ok(())
}

fn install_availability_triggers(conn: &Connection) -> Result<()> {
    for (table, key) in ROW_AVAILABILITY_KEYS {
        let expr = availability_key_expr(table, key, "NEW.");
        conn.execute_batch(&format!(
            "CREATE TRIGGER m6_birth_{table} AFTER INSERT ON {table} BEGIN \
             INSERT OR IGNORE INTO m6_row_availability(table_name,entity_key,revision_id) \
             VALUES ('{table}',CAST(({expr}) AS TEXT),m6_write_revision()); END;"
        ))
        .map_err(dberr)?;
    }
    Ok(())
}

struct SnapshotSpec {
    table: &'static str,
    history: &'static str,
    keys: &'static [&'static str],
    history_keys: &'static [&'static str],
    values: &'static [&'static str],
}

const SNAPSHOT_SPECS: &[SnapshotSpec] = &[
    SnapshotSpec {
        table: "memories",
        history: "m6_memory_history",
        keys: &["id"],
        history_keys: &["memory_id"],
        values: &[
            "assertion_id",
            "state",
            "created_at",
            "superseded_by",
            "superseded_at",
            "retired_at",
            "retire_reason",
            "producer_family",
        ],
    },
    SnapshotSpec {
        table: "spaces",
        history: "m6_space_history",
        keys: &["id"],
        history_keys: &["space_id"],
        values: &["name", "description", "created_at"],
    },
    SnapshotSpec {
        table: "space_links",
        history: "m6_space_link_history",
        keys: &["from_space", "to_space"],
        history_keys: &["from_space", "to_space"],
        values: &["created_at"],
    },
    SnapshotSpec {
        table: "memory_spaces",
        history: "m6_memory_space_history",
        keys: &["memory_id", "space_id"],
        history_keys: &["memory_id", "space_id"],
        values: &["created_at"],
    },
    SnapshotSpec {
        table: "logical_sources",
        history: "m6_logical_source_history",
        keys: &["id"],
        history_keys: &["logical_id"],
        values: &[
            "kind",
            "state",
            "current_location",
            "created_at",
            "repository_id",
            "commit_oid",
            "git_path",
        ],
    },
    SnapshotSpec {
        table: "repositories",
        history: "m6_repository_history",
        keys: &["id"],
        history_keys: &["repository_id"],
        values: &["current_path", "created_at"],
    },
    SnapshotSpec {
        table: "git_refs",
        history: "m6_git_ref_history",
        keys: &["repository_id", "name"],
        history_keys: &["repository_id", "ref_name"],
        values: &[
            "target_oid",
            "target_kind",
            "symbolic_target",
            "last_observed_at",
        ],
    },
];

fn install_snapshot_triggers(conn: &Connection) -> Result<()> {
    for spec in SNAPSHOT_SPECS {
        for (action, event, prefix, deleted) in [
            ("i", "INSERT", "NEW", 0),
            ("u", "UPDATE", "NEW", 0),
            ("d", "DELETE", "OLD", 1),
        ] {
            let source_keys = spec
                .keys
                .iter()
                .map(|key| format!("{prefix}.{key}"))
                .collect::<Vec<_>>();
            let history_key_columns = spec.history_keys.join(",");
            let value_columns = spec.values.join(",");
            let value_exprs = spec
                .values
                .iter()
                .map(|column| format!("{prefix}.{column}"))
                .collect::<Vec<_>>();
            let insert_columns = if value_columns.is_empty() {
                format!("{},revision_id,is_deleted", history_key_columns)
            } else {
                format!(
                    "{},revision_id,is_deleted,{}",
                    history_key_columns, value_columns
                )
            };
            let insert_values = if value_exprs.is_empty() {
                format!("{},m6_write_revision(),{deleted}", source_keys.join(","))
            } else {
                format!(
                    "{},m6_write_revision(),{deleted},{}",
                    source_keys.join(","),
                    value_exprs.join(",")
                )
            };
            let conflict = spec
                .history_keys
                .iter()
                .map(|key| (*key).to_string())
                .chain(std::iter::once("revision_id".to_string()))
                .collect::<Vec<_>>()
                .join(",");
            let assignments = std::iter::once("is_deleted=excluded.is_deleted".to_string())
                .chain(
                    spec.values
                        .iter()
                        .map(|column| format!("{column}=excluded.{column}")),
                )
                .collect::<Vec<_>>()
                .join(",");
            conn.execute_batch(&format!(
                "CREATE TRIGGER m6_snapshot_{table}_{action} AFTER {event} ON {table} BEGIN \
                 INSERT INTO {history} ({insert_columns}) VALUES ({insert_values}) \
                 ON CONFLICT({conflict}) DO UPDATE SET {assignments}; END;",
                table = spec.table,
                history = spec.history,
            ))
            .map_err(dberr)?;
        }
        for key in spec.keys {
            let condition = spec
                .keys
                .iter()
                .map(|other| format!("NEW.{other} IS NOT OLD.{other}"))
                .collect::<Vec<_>>()
                .join(" OR ");
            conn.execute_batch(&format!(
                "CREATE TRIGGER m6_key_immutable_{table}_{key} \
                 BEFORE UPDATE OF {key} ON {table} WHEN {condition} \
                 BEGIN SELECT RAISE(ABORT, 'temporal entity keys are immutable'); END;",
                table = spec.table
            ))
            .map_err(dberr)?;
        }
    }
    Ok(())
}

fn install_m6_write_fence(conn: &Connection) -> Result<()> {
    for table in FENCED_TABLES {
        for (suffix, event) in [("i", "INSERT"), ("u", "UPDATE"), ("d", "DELETE")] {
            conn.execute_batch(&format!(
                "CREATE TRIGGER m6_fence_{table}_{suffix} BEFORE {event} ON {table} \
                 WHEN m6_write_revision() IS NULL OR NOT EXISTS \
                   (SELECT 1 FROM m6_revisions WHERE revision_id=m6_write_revision()) \
                 BEGIN SELECT RAISE(ABORT, 'revisioned write required'); END;"
            ))
            .map_err(dberr)?;
        }
    }
    conn.execute_batch(
        "CREATE TRIGGER m6_fence_revisions_insert BEFORE INSERT ON m6_revisions \
         WHEN m6_write_revision() IS NULL \
           OR NEW.revision_id IS NOT m6_write_revision() \
           OR NEW.kind IS NOT 'write' \
           OR NEW.revision_id IS NOT (SELECT COALESCE(MAX(revision_id),0)+1 FROM m6_revisions) \
         BEGIN SELECT RAISE(ABORT, 'revision allocation is storage-owned'); END; \
         CREATE TRIGGER m6_fence_revisions_update BEFORE UPDATE ON m6_revisions \
         BEGIN SELECT RAISE(ABORT, 'recording revisions are append-only'); END; \
         CREATE TRIGGER m6_fence_revisions_delete BEFORE DELETE ON m6_revisions \
         BEGIN SELECT RAISE(ABORT, 'recording revisions are append-only'); END;",
    )
    .map_err(dberr)?;
    Ok(())
}

fn validate_fence_inventory(conn: &Connection) -> Result<()> {
    let mut expected: HashSet<String> = FENCED_TABLES
        .iter()
        .flat_map(|table| {
            ["i", "u", "d"]
                .iter()
                .map(move |suffix| format!("m6_fence_{table}_{suffix}"))
        })
        .chain([
            "m6_fence_revisions_insert".to_string(),
            "m6_fence_revisions_update".to_string(),
            "m6_fence_revisions_delete".to_string(),
        ])
        .collect();
    // Recording triggers: history capture, availability births, immutability.
    for table in IMMUTABLE_TABLES {
        for suffix in ["u", "d"] {
            expected.insert(format!("m6_immutable_{table}_{suffix}"));
        }
    }
    expected.insert("m6_memory_owner_immutable".to_string());
    for (table, _) in ROW_AVAILABILITY_KEYS {
        expected.insert(format!("m6_birth_{table}"));
    }
    for spec in SNAPSHOT_SPECS {
        for suffix in ["i", "u", "d"] {
            expected.insert(format!("m6_snapshot_{}_{}", spec.table, suffix));
        }
        for key in spec.keys {
            expected.insert(format!("m6_key_immutable_{}_{}", spec.table, key));
        }
    }
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type='trigger' AND name LIKE 'm6_%'")
        .map_err(dberr)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(dberr)?;
    for row in rows {
        expected.remove(&row.map_err(dberr)?);
    }
    let ownership_installed: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=13)",
            [],
            |r| r.get(0),
        )
        .map_err(dberr)?;
    if ownership_installed {
        let marker: bool = conn.query_row("SELECT COUNT(*)=1 FROM m6_row_availability a JOIN m6_revisions r ON r.revision_id=a.revision_id WHERE a.table_name='asset_ownership_schema' AND a.entity_key='local-enroll@1' AND r.kind='write'", [], |r| r.get(0)).map_err(dberr)?;
        if !marker {
            return Err(Error::Storage(
                "ownership schema availability marker is missing or invalid".into(),
            ));
        }
        let installed: HashSet<String> = conn
            .prepare("SELECT name FROM sqlite_schema WHERE type='trigger'")
            .map_err(dberr)?
            .query_map([], |r| r.get(0))
            .map_err(dberr)?
            .collect::<std::result::Result<_, _>>()
            .map_err(dberr)?;
        expected.extend(
            ownership::enforcement_names()
                .into_iter()
                .filter(|name| !installed.contains(name)),
        );
    }
    let preparation_installed: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=14)",
            [],
            |r| r.get(0),
        )
        .map_err(dberr)?;
    if preparation_installed {
        let marker: bool = conn.query_row("SELECT COUNT(*)=1 FROM m6_row_availability a JOIN m6_revisions r ON r.revision_id=a.revision_id WHERE a.table_name='document_preparation_schema' AND a.entity_key='local-doc-prepare@1' AND r.kind='write'", [], |r| r.get(0)).map_err(dberr)?;
        if !marker {
            return Err(Error::Storage(
                "document preparation schema availability marker is missing or invalid".into(),
            ));
        }
        let installed: HashSet<String> = conn
            .prepare("SELECT name FROM sqlite_schema WHERE type='trigger'")
            .map_err(dberr)?
            .query_map([], |r| r.get(0))
            .map_err(dberr)?
            .collect::<std::result::Result<_, _>>()
            .map_err(dberr)?;
        expected.extend(
            document_preparation::enforcement_names()
                .into_iter()
                .filter(|name| !installed.contains(name)),
        );
    }
    let lexical_installed: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=15)",
            [],
            |r| r.get(0),
        )
        .map_err(dberr)?;
    if lexical_installed {
        let marker: bool = conn.query_row("SELECT COUNT(*)=1 FROM m6_row_availability a JOIN m6_revisions r ON r.revision_id=a.revision_id WHERE a.table_name='lexical_staging_schema' AND a.entity_key='local-doc-stage-lexical@1' AND r.kind='write'", [], |r|r.get(0)).map_err(dberr)?;
        if !marker {
            return Err(Error::Storage(
                "lexical staging schema availability marker is missing or invalid".into(),
            ));
        }
        let installed: HashSet<String> = conn
            .prepare("SELECT name FROM sqlite_schema WHERE type='trigger'")
            .map_err(dberr)?
            .query_map([], |r| r.get(0))
            .map_err(dberr)?
            .collect::<std::result::Result<_, _>>()
            .map_err(dberr)?;
        expected.extend(
            lexical_staging::enforcement_names()
                .into_iter()
                .filter(|name| !installed.contains(name)),
        );
    }
    let semantic_installed: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=16)",
            [],
            |r| r.get(0),
        )
        .map_err(dberr)?;
    if semantic_installed {
        let marker: bool = conn.query_row("SELECT COUNT(*)=1 FROM m6_row_availability a JOIN m6_revisions r ON r.revision_id=a.revision_id WHERE a.table_name='semantic_staging_schema' AND a.entity_key='local-doc-stage-semantic@1' AND r.kind='write'", [], |r|r.get(0)).map_err(dberr)?;
        if !marker {
            return Err(Error::Storage(
                "semantic staging schema availability marker is missing or invalid".into(),
            ));
        }
        let installed: HashSet<String> = conn
            .prepare("SELECT name FROM sqlite_schema WHERE type='trigger'")
            .map_err(dberr)?
            .query_map([], |r| r.get(0))
            .map_err(dberr)?
            .collect::<std::result::Result<_, _>>()
            .map_err(dberr)?;
        expected.extend(
            semantic_staging::enforcement_names()
                .into_iter()
                .filter(|name| !installed.contains(name)),
        );
    }
    let passive_installed: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=17)",
            [],
            |r| r.get(0),
        )
        .map_err(dberr)?;
    if passive_installed {
        fence_plans::validate_schema(conn)?;
        let marker: bool = conn.query_row("SELECT COUNT(*)=1 FROM m6_row_availability a JOIN m6_revisions r ON r.revision_id=a.revision_id WHERE a.table_name='fence_plan_schema' AND a.entity_key='local-fence-plan@1' AND r.kind='write'", [], |r|r.get(0)).map_err(dberr)?;
        if !marker {
            return Err(Error::Storage(
                "passive fence schema marker is missing or invalid".into(),
            ));
        }
        let installed: HashSet<String> = conn
            .prepare("SELECT name FROM sqlite_schema WHERE type='trigger'")
            .map_err(dberr)?
            .query_map([], |r| r.get(0))
            .map_err(dberr)?
            .collect::<std::result::Result<_, _>>()
            .map_err(dberr)?;
        expected.extend(
            fence_plans::enforcement_names()
                .into_iter()
                .filter(|n| !installed.contains(n)),
        );
    }
    if !expected.is_empty() {
        let mut missing: Vec<String> = expected.into_iter().collect();
        missing.sort_unstable();
        let first = missing.first().map(String::as_str).unwrap_or_default();
        return Err(Error::Storage(format!(
            "revision-fence trigger inventory is incomplete: missing {first} (and {} more)",
            missing.len() - 1
        )));
    }
    Ok(())
}

fn verify_fts_index(conn: &Connection) -> Result<()> {
    verify_fts_counts(conn)?;
    conn.execute(
        "INSERT INTO assertion_fts(assertion_fts,rank) VALUES ('integrity-check',1)",
        [],
    )
    .map_err(|error| Error::FtsIndexInconsistent {
        detail: error.to_string(),
    })?;
    let mismatches: i64 = conn
        .query_row(
            "SELECT \
               (SELECT COUNT(*) FROM assertions a LEFT JOIN assertion_fts f ON f.assertion_id=a.id \
                WHERE f.rowid IS NULL OR f.subject IS NOT a.subject OR f.content IS NOT a.content) + \
               (SELECT COUNT(*) FROM assertion_fts f LEFT JOIN assertions a ON a.id=f.assertion_id \
                WHERE a.id IS NULL)",
            [],
            |row| row.get(0),
        )
        .map_err(dberr)?;
    if mismatches != 0 {
        return Err(Error::FtsIndexInconsistent {
            detail: format!("{mismatches} canonical/FTS row mismatches"),
        });
    }
    Ok(())
}

/// Cheap open-time structural check: row counts must agree between the
/// canonical assertions and the derived FTS index. The full agreement and
/// internal integrity checks run on demand via `verify_fts_index` and before
/// and after every rebuild.
fn verify_fts_counts(conn: &Connection) -> Result<()> {
    let mismatched: bool = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM assertions) \
             IS NOT (SELECT COUNT(*) FROM assertion_fts)",
            [],
            |row| row.get(0),
        )
        .map_err(dberr)?;
    if mismatched {
        return Err(Error::FtsIndexInconsistent {
            detail: "assertion and FTS row counts disagree".to_string(),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Row mapping helpers
// ---------------------------------------------------------------------------

fn uuid_col(row: &Row, name: &str) -> rusqlite::Result<Uuid> {
    let s: String = row.get(name)?;
    Uuid::parse_str(&s)
        .map_err(|_| rusqlite::Error::InvalidColumnType(0, name.to_string(), Type::Text))
}

fn ts_col(row: &Row, name: &str) -> rusqlite::Result<Ts> {
    Ok(Ts(row.get(name)?))
}

fn opt_ts_col(row: &Row, name: &str) -> rusqlite::Result<Option<Ts>> {
    Ok(row.get::<_, Option<i64>>(name)?.map(Ts))
}

fn opt_uuid_col(row: &Row, name: &str) -> rusqlite::Result<Option<Uuid>> {
    Ok(row
        .get::<_, Option<String>>(name)?
        .and_then(|s| Uuid::parse_str(&s).ok()))
}

fn enum_col<T: Copy>(row: &Row, name: &str, parse: fn(&str) -> Option<T>) -> rusqlite::Result<T> {
    let s: String = row.get(name)?;
    parse(&s).ok_or_else(|| rusqlite::Error::InvalidColumnType(0, name.to_string(), Type::Text))
}

fn row_space(row: &Row) -> rusqlite::Result<Space> {
    Ok(Space {
        id: uuid_col(row, "id")?,
        name: row.get("name")?,
        description: row.get("description")?,
        created_at: ts_col(row, "created_at")?,
    })
}

fn row_source(row: &Row) -> rusqlite::Result<Source> {
    Ok(Source {
        id: uuid_col(row, "id")?,
        logical_id: uuid_col(row, "logical_id")?,
        kind: enum_col(row, "kind", SourceKind::parse)?,
        location: row.get("location")?,
        title: row.get("title")?,
        content: row.get("content")?,
        content_hash: row.get("content_hash")?,
        media_type: row.get("media_type")?,
        size_bytes: row.get::<_, i64>("size_bytes")? as u64,
        ingested_at: ts_col(row, "ingested_at")?,
    })
}

fn row_memory(row: &Row) -> rusqlite::Result<Memory> {
    Ok(Memory {
        id: uuid_col(row, "id")?,
        assertion_id: uuid_col(row, "assertion_id")?,
        state: enum_col(row, "state", MemoryState::parse)?,
        created_at: ts_col(row, "created_at")?,
        superseded_by: opt_uuid_col(row, "superseded_by")?,
        superseded_at: opt_ts_col(row, "superseded_at")?,
        retired_at: opt_ts_col(row, "retired_at")?,
        retire_reason: row.get("retire_reason")?,
    })
}

fn row_summary(row: &Row) -> rusqlite::Result<MemorySummary> {
    Ok(MemorySummary {
        memory_id: uuid_col(row, "id")?,
        state: enum_col(row, "state", MemoryState::parse)?,
        created_at: ts_col(row, "created_at")?,
        superseded_by: opt_uuid_col(row, "superseded_by")?,
        superseded_at: opt_ts_col(row, "superseded_at")?,
        retired_at: opt_ts_col(row, "retired_at")?,
        retire_reason: row.get("retire_reason")?,
        assertion_id: uuid_col(row, "assertion_id")?,
        kind: enum_col(row, "kind", AssertionKind::parse)?,
        subject: row.get("subject")?,
        subject_normalized: row.get("subject_normalized")?,
        content: row.get("content")?,
        extraction: row.get("extraction")?,
        spaces: Vec::new(),
        source_location: row.get("location")?,
        source_title: row.get("title")?,
    })
}

const SUMMARY_COLS: &str = "m.id, m.state, m.created_at, m.superseded_by, m.superseded_at, \
     m.retired_at, m.retire_reason, a.id AS assertion_id, a.kind, a.subject, \
     a.subject_normalized, a.content, a.extraction, src.location, src.title";
const SUMMARY_FROM: &str = "FROM memories m \
     JOIN assertions a ON a.id = m.assertion_id \
     JOIN evidence e ON e.id = a.evidence_id \
     JOIN sources src ON src.id = e.source_id";

fn load_summaries(
    conn: &Connection,
    extra_join: &str,
    where_sql: &str,
    params: Vec<Value>,
) -> Result<Vec<MemorySummary>> {
    let sql = format!("SELECT {SUMMARY_COLS} {SUMMARY_FROM} {extra_join} WHERE {where_sql} ORDER BY m.created_at, m.id");
    let mut stmt = conn.prepare(&sql).map_err(dberr)?;
    let mut out = Vec::new();
    let rows = stmt
        .query_map(params_from_iter(params), row_summary)
        .map_err(dberr)?;
    for r in rows {
        out.push(r.map_err(dberr)?);
    }
    #[cfg(test)]
    read_session_tests::checkpoint("summary_enrichment");
    attach_space_names(conn, &mut out)?;
    Ok(out)
}

fn attach_space_names(conn: &Connection, summaries: &mut [MemorySummary]) -> Result<()> {
    if summaries.is_empty() {
        return Ok(());
    }
    let ids: Vec<String> = summaries.iter().map(|s| s.memory_id.to_string()).collect();
    let names = space_names_by_memory(conn, &ids)?;
    for s in summaries.iter_mut() {
        if let Some(names) = names.get(&s.memory_id.to_string()) {
            s.spaces = names.clone();
        }
    }
    Ok(())
}

fn space_names_by_memory(
    conn: &Connection,
    memory_ids: &[String],
) -> Result<BTreeMap<String, Vec<String>>> {
    #[cfg(test)]
    read_session_tests::checkpoint("search_enrichment");
    let placeholders = vec!["?"; memory_ids.len()].join(", ");
    let sql = format!(
        "SELECT ms.memory_id, s.name FROM memory_spaces ms \
         JOIN spaces s ON s.id = ms.space_id \
         WHERE ms.memory_id IN ({placeholders}) ORDER BY s.name"
    );
    let mut stmt = conn.prepare(&sql).map_err(dberr)?;
    let rows = stmt
        .query_map(params_from_iter(memory_ids), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(dberr)?;
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for r in rows {
        let (mid, name) = r.map_err(dberr)?;
        map.entry(mid).or_default().push(name);
    }
    Ok(map)
}

/// One statement snapshot, also usable within an already-owned read transaction.
fn read_embedding_settings_on(conn: &Connection) -> Result<EmbeddingSettings> {
    conn.query_row(
        "SELECT (SELECT value FROM mind_meta WHERE key = 'embedding.enabled'), \
                (SELECT value FROM mind_meta WHERE key = 'embedding.model')",
        [],
        |row| {
            Ok(EmbeddingSettings {
                enabled: row.get(0)?,
                model: row.get(1)?,
            })
        },
    )
    .map_err(dberr)
}

// ---------------------------------------------------------------------------
// Store implementation
// ---------------------------------------------------------------------------

impl Store for SqliteStore {
    fn read_session(&self) -> Result<Box<dyn crate::storage::MemoryRead + '_>> {
        Ok(Box::new(read_session::SqliteMemoryRead::begin(self)?))
    }

    fn temporal_history_capabilities(&self) -> Result<TemporalHistoryCapabilities> {
        let conn = self.connection()?;
        load_temporal_capabilities(&conn)
    }

    fn require_temporal_coverage(
        &self,
        revision: i64,
        dimensions: &[TemporalDimension],
    ) -> Result<()> {
        let conn = self.connection()?;
        require_temporal_coverage(&conn, revision, dimensions)
    }

    fn create_space(&self, name: &str, description: &str) -> Result<Space> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::EmptyName);
        }
        let conn = self.connection()?;
        let tx = begin_write(&conn, &self.revision_state)?;
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM spaces WHERE name = ?1",
                rusqlite::params![name],
                |_| Ok(()),
            )
            .optional()
            .map_err(dberr)?
            .is_some();
        if exists {
            return Err(Error::SpaceExists {
                name: name.to_string(),
            });
        }
        let id = Uuid::now_v7();
        let created_at = Ts::now();
        tx.execute(
            "INSERT INTO spaces (id, name, description, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id.to_string(), name, description, created_at.0],
        )
        .map_err(dberr)?;
        tx.commit()?;
        Ok(Space {
            id,
            name: name.to_string(),
            description: description.to_string(),
            created_at,
        })
    }

    fn get_space(&self, name: &str) -> Result<Space> {
        let session = self.read_session()?;
        let result = session.get_space(name)?;
        session.finish()?;
        Ok(result)
    }

    fn list_spaces(&self) -> Result<Vec<Space>> {
        let session = self.read_session()?;
        let result = session.list_spaces()?;
        session.finish()?;
        Ok(result)
    }

    fn list_spaces_with_counts(&self) -> Result<Vec<(Space, u64)>> {
        let session = self.read_session()?;
        let result = session.list_spaces_with_counts()?;
        session.finish()?;
        Ok(result)
    }

    fn list_space_links(&self) -> Result<Vec<SpaceLink>> {
        let session = self.read_session()?;
        let result = session.list_space_links()?;
        session.finish()?;
        Ok(result)
    }

    fn link_spaces(&self, from: Uuid, to: Uuid) -> Result<SpaceLink> {
        let conn = self.connection()?;
        let tx = begin_write(&conn, &self.revision_state)?;
        let from_space = tx
            .query_row(
                "SELECT id, name, description, created_at FROM spaces WHERE id = ?1",
                rusqlite::params![from.to_string()],
                row_space,
            )
            .optional()
            .map_err(dberr)?
            .ok_or_else(|| Error::SpaceNotFound {
                name: from.to_string(),
            })?;
        let to_space = tx
            .query_row(
                "SELECT id, name, description, created_at FROM spaces WHERE id = ?1",
                rusqlite::params![to.to_string()],
                row_space,
            )
            .optional()
            .map_err(dberr)?
            .ok_or_else(|| Error::SpaceNotFound {
                name: to.to_string(),
            })?;
        if from == to {
            return Err(Error::SelfLink {
                name: from_space.name,
            });
        }
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM space_links WHERE from_space = ?1 AND to_space = ?2",
                rusqlite::params![from.to_string(), to.to_string()],
                |_| Ok(()),
            )
            .optional()
            .map_err(dberr)?
            .is_some();
        if exists {
            return Err(Error::LinkExists {
                from: from_space.name,
                to: to_space.name,
            });
        }
        let created_at = Ts::now();
        tx.execute(
            "INSERT INTO space_links (from_space, to_space, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![from.to_string(), to.to_string(), created_at.0],
        )
        .map_err(dberr)?;
        tx.commit()?;
        Ok(SpaceLink {
            from_space: from,
            to_space: to,
            created_at,
        })
    }

    fn record_observation(&self, observation: IngestObservation) -> Result<ObservationReport> {
        let conn = self.connection()?;
        let tx = begin_write(&conn, &self.revision_state)?;
        let report = record_observation_tx(&tx, observation)?;
        tx.commit()?;
        Ok(report)
    }
    fn record_document_extraction(
        &self,
        draft: DocumentExtractionDraft,
    ) -> Result<DocumentExtractionReport> {
        documents::record(self, draft)
    }
    #[cfg(feature = "embeddings")]
    fn record_chunked_document(
        &self,
        draft: DocumentExtractionDraft,
        budget: &crate::embeddings::budget::TokenBudget,
    ) -> Result<DocumentExtractionReport> {
        let plan = crate::ingest::document::chunk::chunk_document(&draft, budget)?;
        documents::record_with_chunks(self, draft, Some(plan))
    }
    fn resolve_document_anchor(&self, anchor: &Anchor) -> Result<Option<StoredCitation>> {
        documents::resolve(self, anchor)
    }
    fn list_document_extractions(&self, logical_id: Uuid) -> Result<Vec<DocumentExtractionInfo>> {
        documents::list(self, logical_id)
    }
    fn erase_document_bytes(&self, source_bytes_hash: &str) -> Result<bool> {
        documents::erase(self, source_bytes_hash)
    }
    fn get_source_lifecycle(&self, logical_id: Uuid) -> Result<Option<SourceLifecycle>> {
        let conn = self.connection()?;
        load_source_lifecycle(&conn, logical_id)
    }

    fn get_memory_detail(&self, id: Uuid) -> Result<Option<MemoryDetail>> {
        let session = self.read_session()?;
        let result = session.get_memory_detail(id)?;
        session.finish()?;
        Ok(result)
    }

    fn record_git_observation(
        &self,
        observation: crate::core::GitObservation,
    ) -> Result<GitObservationReport> {
        let conn = self.connection()?;
        let tx = begin_write(&conn, &self.revision_state)?;
        let report = record_git_observation_tx(&tx, &observation)?;
        tx.commit()?;
        Ok(report)
    }
    fn record_git_ingest(
        &self,
        observation: crate::core::GitObservation,
        code: Option<CodeObservation>,
        resolutions: crate::storage::GitResolutionPlan<'_>,
        before_commit: &dyn Fn() -> Result<()>,
    ) -> Result<GitObservationReport> {
        // ONE transaction, ONE recording revision for the whole logical ingestion. The resolutions
        // read the rows the code write made a moment earlier: inside this transaction they see
        // them before they are committed. Any error (or a crash before the COMMIT) leaves nothing.
        let repository_id = observation.repository_id;
        let conn = self.connection()?;
        let tx = begin_write(&conn, &self.revision_state)?;
        let mut report = record_git_observation_tx(&tx, &observation)?;
        if let Some(code) = code {
            report.code = record_code_observation_tx(&tx, code)?;
            for commit in resolutions.commits {
                resolve_git_code_relationships_tx(
                    &tx,
                    repository_id,
                    commit,
                    resolutions.resolver,
                    resolutions.version,
                )?;
            }
        }
        // an error here drops `tx`, which rolls back: nothing of this ingestion is committed
        before_commit()?;
        tx.commit()?;
        Ok(report)
    }
    fn git_refs_current(&self, repository_id: Uuid) -> Result<Vec<GitRefObservation>> {
        let conn = self.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT name, target_oid, target_kind, symbolic_target FROM git_refs \
                 WHERE repository_id = ?1 ORDER BY name",
            )
            .map_err(dberr)?;
        let rows = stmt
            .query_map(rusqlite::params![repository_id.to_string()], |r| {
                Ok(GitRefObservation {
                    name: r.get(0)?,
                    target_oid: r.get(1)?,
                    target_kind: enum_col(r, "target_kind", crate::core::GitObjectKind::parse)?,
                    symbolic_target: r.get(3)?,
                })
            })
            .map_err(dberr)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(dberr)?);
        }
        Ok(out)
    }

    fn record_code_observation(
        &self,
        observation: CodeObservation,
    ) -> Result<CodeObservationReport> {
        let conn = self.connection()?;
        let tx = begin_write(&conn, &self.revision_state)?;
        let report = record_code_observation_tx(&tx, observation)?;
        tx.commit()?;
        Ok(report)
    }

    fn record_files_ingest(
        &self,
        observation: IngestObservation,
        code: Option<CodeObservation>,
        before_commit: &dyn Fn() -> Result<()>,
    ) -> Result<ObservationReport> {
        // ONE transaction, ONE recording revision for the whole logical ingestion: the observation
        // and its derivations commit together, or (on any error, or a crash before the commit)
        // neither does. The two `record_*` methods above remain for callers that want one part.
        let conn = self.connection()?;
        let tx = begin_write(&conn, &self.revision_state)?;
        let mut report = record_observation_tx(&tx, observation)?;
        if let Some(code) = code {
            report.code = record_code_observation_tx(&tx, code)?;
        }
        // an error here drops `tx`, which rolls back: nothing of this ingestion is committed
        before_commit()?;
        tx.commit()?;
        Ok(report)
    }

    fn code_structure(&self, target: &str) -> Result<Vec<CodeDerivation>> {
        let conn = self.connection()?;
        let mut stmt=conn.prepare(&format!("SELECT DISTINCT d.id,d.language,d.extractor,d.extractor_version,d.grammar,d.grammar_version,d.status,d.error_count,i.content_hash,d.created_at,(CASE WHEN {EFFECTIVE_DERIVATION} THEN 1 ELSE 0 END) AS effective FROM code_derivations d JOIN code_inputs i ON i.id=d.input_id JOIN code_contexts c ON c.input_id=i.id WHERE c.path=?1 OR i.blob_oid=?1 ORDER BY d.created_at, d.id")).map_err(dberr)?;
        let rows = stmt
            .query_map(rusqlite::params![target], |r| {
                Ok((
                    uuid_col(r, "id")?,
                    enum_col(r, "language", crate::core::CodeLanguage::parse)?,
                    r.get::<_, String>("extractor")?,
                    r.get::<_, String>("extractor_version")?,
                    r.get::<_, String>("grammar")?,
                    r.get::<_, String>("grammar_version")?,
                    enum_col(r, "status", crate::core::CodeParseStatus::parse)?,
                    r.get::<_, u32>("error_count")?,
                    r.get::<_, String>("content_hash")?,
                    r.get::<_, i64>("effective")? == 1,
                ))
            })
            .map_err(dberr)?;
        let mut out = Vec::new();
        for row in rows {
            let (
                id,
                language,
                extractor,
                extractor_version,
                grammar,
                grammar_version,
                status,
                error_count,
                content_hash,
                effective,
            ) = row.map_err(dberr)?;
            let mut es=conn.prepare("SELECT id,kind,name,qualified_name,syntax_path,parent_syntax_path,start_byte,end_byte,start_line,start_column,end_line,end_column,signature,visibility,is_async FROM code_entities WHERE derivation_id=?1 ORDER BY start_byte").map_err(dberr)?;
            let entities = es
                .query_map(rusqlite::params![id.to_string()], |r| {
                    Ok(CodeEntity {
                        id: uuid_col(r, "id")?,
                        kind: enum_col(r, "kind", crate::core::CodeEntityKind::parse)?,
                        name: r.get(2)?,
                        qualified_name: r.get(3)?,
                        syntax_path: r.get(4)?,
                        parent_syntax_path: r.get(5)?,
                        range: crate::core::CodeRange {
                            start_byte: r.get::<_, i64>(6)? as u64,
                            end_byte: r.get::<_, i64>(7)? as u64,
                            start_line: r.get(8)?,
                            start_column: r.get(9)?,
                            end_line: r.get(10)?,
                            end_column: r.get(11)?,
                        },
                        signature: r.get(12)?,
                        visibility: r.get(13)?,
                        is_async: r.get(14)?,
                    })
                })
                .map_err(dberr)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(dberr)?;
            let mut is=conn.prepare("SELECT raw,target,alias,start_byte,end_byte,start_line,start_column,end_line,end_column FROM code_imports WHERE derivation_id=?1 ORDER BY start_byte").map_err(dberr)?;
            let imports = is
                .query_map(rusqlite::params![id.to_string()], |r| {
                    Ok(CodeImport {
                        raw: r.get(0)?,
                        target: r.get(1)?,
                        alias: r.get(2)?,
                        range: crate::core::CodeRange {
                            start_byte: r.get::<_, i64>(3)? as u64,
                            end_byte: r.get::<_, i64>(4)? as u64,
                            start_line: r.get(5)?,
                            start_column: r.get(6)?,
                            end_line: r.get(7)?,
                            end_column: r.get(8)?,
                        },
                    })
                })
                .map_err(dberr)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(dberr)?;
            let identity = crate::core::CodeParserIdentity {
                extractor,
                extractor_version,
                grammar,
                grammar_version,
            };
            let skip_reason = crate::ingest::code::skip_reason_of(&identity);
            out.push(CodeDerivation {
                id,
                language,
                identity,
                status,
                error_count,
                effective,
                skip_reason,
                content_hash,
                entities,
                imports,
            });
        }
        Ok(out)
    }

    fn search_code(
        &self,
        query: &crate::core::CodeSearchQuery,
    ) -> Result<Vec<crate::core::CodeHit>> {
        let conn = self.connection()?;
        let terms: Vec<String> = query
            .text
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        if terms.is_empty() {
            return Err(Error::EmptyQuery);
        }
        let whole = terms.join(" ");
        let mut params: Vec<Value> = Vec::new();
        let push = |params: &mut Vec<Value>, v: &str| -> usize {
            params.push(Value::Text(v.to_string()));
            params.len()
        };

        // Rank against the whole (lowercased) query: exact name, exact
        // qualified name, name prefix, then substring in name, qualified
        // name, signature; anything else matched only through other terms or
        // the path. Imports and fields sort behind real definitions.
        let q = push(&mut params, &whole);
        let rank = format!(
            "(CASE \
               WHEN lower(coalesce(e.name,'')) = ?{q} THEN 0 \
               WHEN lower(coalesce(e.qualified_name,'')) = ?{q} THEN 1 \
               WHEN substr(lower(coalesce(e.name,'')),1,length(?{q})) = ?{q} THEN 2 \
               WHEN instr(lower(coalesce(e.name,'')), ?{q}) > 0 THEN 3 \
               WHEN instr(lower(coalesce(e.qualified_name,'')), ?{q}) > 0 THEN 4 \
               WHEN instr(lower(coalesce(e.signature,'')), ?{q}) > 0 THEN 5 \
               ELSE 6 END)"
        );
        let current = "(CASE WHEN i.repository_id IS NOT NULL THEN \
               EXISTS (SELECT 1 FROM git_views v \
                        WHERE v.repository_id = i.repository_id AND v.commit_oid = c.commit_oid) \
             ELSE \
               EXISTS (SELECT 1 FROM logical_sources ls \
                        WHERE ls.current_location = c.path AND ls.state = 'present' AND ls.kind = 'file' \
                          AND (SELECT s.content_hash FROM sources s WHERE s.logical_id = ls.id \
                                ORDER BY s.ingested_at DESC, s.rowid DESC LIMIT 1) = i.content_hash) \
             END)";

        let mut wheres: Vec<String> = vec![EFFECTIVE_DERIVATION.to_string()];
        for t in &terms {
            let n = push(&mut params, t);
            wheres.push(format!(
                "(instr(lower(coalesce(e.name,'')), ?{n}) > 0 \
                  OR instr(lower(coalesce(e.qualified_name,'')), ?{n}) > 0 \
                  OR instr(lower(coalesce(e.signature,'')), ?{n}) > 0 \
                  OR instr(lower(c.path), ?{n}) > 0)"
            ));
        }
        if !query.kinds.is_empty() {
            let marks: Vec<String> = query
                .kinds
                .iter()
                .map(|k| format!("?{}", push(&mut params, k.as_str())))
                .collect();
            wheres.push(format!("e.kind IN ({})", marks.join(",")));
        }
        if let Some(repo) = query.repository.as_deref().map(str::to_lowercase) {
            let n = push(&mut params, &repo);
            wheres.push(format!(
                "(instr(lower(coalesce(r.current_path,'')), ?{n}) > 0 \
                  OR substr(lower(coalesce(i.repository_id,'')),1,length(?{n})) = ?{n} \
                  OR EXISTS (SELECT 1 FROM repository_remotes rr \
                              WHERE rr.repository_id = i.repository_id AND instr(rr.slug_key, ?{n}) > 0))"
            ));
        }
        if let Some(ids) = &query.space_ids {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let marks: Vec<String> = ids
                .iter()
                .map(|id| format!("?{}", push(&mut params, &id.to_string())))
                .collect();
            wheres.push(format!(
                "EXISTS (SELECT 1 FROM scope_spaces ss JOIN scopes sc ON sc.id = ss.scope_id \
                          WHERE ss.space_id IN ({}) \
                            AND ((sc.repository_id IS NOT NULL AND sc.repository_id = i.repository_id) \
                              OR (sc.root IS NOT NULL AND i.repository_id IS NULL \
                                  AND (c.path = sc.root OR substr(c.path,1,length(sc.root)+1) = sc.root || '/'))))",
                marks.join(",")
            ));
        }
        if let Some(prefix) = query.path_prefix.as_deref() {
            let prefix = prefix.trim_end_matches('/');
            let n = push(&mut params, prefix);
            wheres.push(format!(
                "(c.path = ?{n} OR substr(c.path,1,length(?{n})+1) = ?{n} || '/')"
            ));
        }
        if !query.all_versions {
            wheres.push(format!("{current} = 1"));
        }
        params.push(Value::Integer(i64::from(query.limit)));
        let limit_n = params.len();

        let sql = format!(
            "SELECT e.id, e.kind, e.name, e.qualified_name, e.signature, e.visibility, e.is_async, \
                    d.language, c.path, e.start_line, e.end_line, i.repository_id, r.current_path, \
                    c.commit_oid, i.blob_oid, i.content_hash, {current} AS is_current, {rank} AS match_rank \
             FROM code_entities e \
             JOIN code_derivations d ON d.id = e.derivation_id \
             JOIN code_inputs i ON i.id = d.input_id \
             JOIN code_contexts c ON c.input_id = i.id \
             LEFT JOIN repositories r ON r.id = i.repository_id \
             WHERE {} \
             ORDER BY {rank} + (CASE e.kind WHEN 'import' THEN 10 WHEN 'field' THEN 2 ELSE 0 END), \
                      length(coalesce(e.qualified_name, e.name, '')), c.path, e.start_line, e.id \
             LIMIT ?{limit_n}",
            wheres.join(" AND ")
        );
        let mut stmt = conn.prepare(&sql).map_err(dberr)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params), |r| {
                let kind: String = r.get(1)?;
                let id: String = r.get(0)?;
                let repo: Option<String> = r.get(11)?;
                Ok(crate::core::CodeHit {
                    entity_id: Uuid::parse_str(&id).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(0, Type::Text, Box::new(e))
                    })?,
                    kind: crate::core::CodeEntityKind::parse(&kind).ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            Type::Text,
                            format!("unknown entity kind '{kind}'").into(),
                        )
                    })?,
                    name: r.get(2)?,
                    qualified_name: r.get(3)?,
                    signature: r.get(4)?,
                    visibility: r.get(5)?,
                    is_async: r.get::<_, i64>(6)? != 0,
                    language: r.get(7)?,
                    path: r.get(8)?,
                    start_line: r.get(9)?,
                    end_line: r.get(10)?,
                    repository_id: repo.and_then(|s| Uuid::parse_str(&s).ok()),
                    repository_path: r.get(12)?,
                    repository_slug: None,
                    spaces: Vec::new(),
                    commit_oid: r.get(13)?,
                    blob_oid: r.get(14)?,
                    content_hash: r.get(15)?,
                    current: r.get::<_, i64>(16)? != 0,
                    match_rank: r.get::<_, i64>(17)? as u8,
                })
            })
            .map_err(dberr)?;
        let mut hits = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(dberr)?;

        // Where each definition lives: attached spaces and the repository's
        // primary slug, looked up once per repository or file.
        let mut scope_cache: HashMap<String, (Vec<String>, Option<String>)> = HashMap::new();
        for h in &mut hits {
            let key = match h.repository_id {
                Some(id) => format!("r:{id}"),
                None => format!("f:{}", h.path),
            };
            if !scope_cache.contains_key(&key) {
                let (sql, arg) = match h.repository_id {
                    Some(id) => (
                        "SELECT DISTINCT s.name FROM scope_spaces ss \
                         JOIN scopes sc ON sc.id = ss.scope_id JOIN spaces s ON s.id = ss.space_id \
                         WHERE sc.repository_id = ?1 ORDER BY s.name",
                        id.to_string(),
                    ),
                    None => (
                        "SELECT DISTINCT s.name FROM scope_spaces ss \
                         JOIN scopes sc ON sc.id = ss.scope_id JOIN spaces s ON s.id = ss.space_id \
                         WHERE sc.root IS NOT NULL \
                           AND (?1 = sc.root OR substr(?1,1,length(sc.root)+1) = sc.root || '/') \
                         ORDER BY s.name",
                        h.path.clone(),
                    ),
                };
                let mut stmt = conn.prepare(sql).map_err(dberr)?;
                let names = stmt
                    .query_map(rusqlite::params![arg], |r| r.get::<_, String>(0))
                    .map_err(dberr)?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(dberr)?;
                let slug = match h.repository_id {
                    Some(id) => {
                        let mut stmt = conn
                            .prepare(
                                "SELECT name, slug_key, slug FROM repository_remotes \
                                 WHERE repository_id = ?1 AND is_current = 1 ORDER BY name",
                            )
                            .map_err(dberr)?;
                        let remotes = stmt
                            .query_map(rusqlite::params![id.to_string()], |r| {
                                Ok((
                                    r.get::<_, String>(0)?,
                                    r.get::<_, Option<String>>(1)?,
                                    r.get::<_, Option<String>>(2)?,
                                ))
                            })
                            .map_err(dberr)?
                            .collect::<std::result::Result<Vec<_>, _>>()
                            .map_err(dberr)?;
                        match crate::core::choose_primary_slug(&remotes) {
                            crate::core::PrimarySlug::Chosen { short, .. } => Some(short),
                            _ => None,
                        }
                    }
                    None => None,
                };
                scope_cache.insert(key.clone(), (names, slug));
            }
            let (names, slug) = &scope_cache[&key];
            h.spaces = names.clone();
            h.repository_slug = slug.clone();
        }
        Ok(hits)
    }

    fn has_code_derivation(
        &self,
        _language: crate::core::CodeLanguage,
        content_hash: &str,
        repository_id: Option<Uuid>,
        blob_oid: Option<&str>,
        identity: &crate::core::CodeParserIdentity,
    ) -> Result<bool> {
        let conn = self.connection()?;
        let found:Option<i64>=match (repository_id,blob_oid) { (Some(repo),Some(blob))=>conn.query_row("SELECT 1 FROM code_derivations d JOIN code_inputs i ON i.id=d.input_id WHERE i.repository_id=?1 AND i.blob_oid=?2 AND d.extractor=?3 AND d.extractor_version=?4 AND d.grammar=?5 AND d.grammar_version=?6",rusqlite::params![repo.to_string(),blob,identity.extractor,identity.extractor_version,identity.grammar,identity.grammar_version],|r|r.get(0)).optional().map_err(dberr)?, _=>conn.query_row("SELECT 1 FROM code_derivations d JOIN code_inputs i ON i.id=d.input_id WHERE i.repository_id IS NULL AND i.content_hash=?1 AND d.extractor=?2 AND d.extractor_version=?3 AND d.grammar=?4 AND d.grammar_version=?5",rusqlite::params![content_hash,identity.extractor,identity.extractor_version,identity.grammar,identity.grammar_version],|r|r.get(0)).optional().map_err(dberr)?};
        Ok(found.is_some())
    }

    fn resolve_git_code_relationships(
        &self,
        repository_id: Uuid,
        commit_oid: &str,
        resolver: &str,
        version: &str,
    ) -> Result<crate::core::CodeResolutionReport> {
        let conn = self.connection()?;
        let tx = begin_write(&conn, &self.revision_state)?;
        let report =
            resolve_git_code_relationships_tx(&tx, repository_id, commit_oid, resolver, version)?;
        tx.commit()?;
        Ok(report)
    }

    fn record_manual_write(&self, draft: ManualMemoryDraft) -> Result<ManualMemoryReport> {
        let conn = self.connection()?;
        let tx = begin_write(&conn, &self.revision_state)?;
        let at = Ts::now();
        if draft.content.trim().is_empty() {
            return Err(Error::EmptyContent);
        }

        let subject = draft
            .subject
            .or_else(|| crate::core::default_manual_subject(&draft.content))
            .ok_or(Error::EmptyContent)?;
        let subject_normalized = crate::core::normalize_subject(&subject);
        if subject_normalized.is_empty() {
            return Err(Error::EmptySubject);
        }

        if draft.spaces.is_empty() {
            return Err(Error::SpacesRequired);
        }
        let spaces = validate_space_ids(&tx, &draft.spaces)?;
        if let Some(old_id) = draft.supersedes {
            validate_manual_update_target(&tx, old_id)?;
        }

        let logical_id = ensure_manual_logical_source(&tx, at)?;
        let content_hash = sha256_hex(draft.content.as_bytes());
        let observed = ObservedFile {
            location: MANUAL_SOURCE_LOCATION.to_string(),
            content: draft.content.clone(),
            content_hash,
            title: subject.clone(),
            media_type: "text/plain".to_string(),
            size_bytes: draft.content.len() as u64,
            evidences: vec![EvidenceDraft {
                kind: EvidenceKind::Document,
                heading_path: None,
                start_line: None,
                end_line: None,
                ordinal: 0,
                content: draft.content.clone(),
                content_hash: sha256_hex(draft.content.as_bytes()),
            }],
            assertions: Vec::new(),
            memories: Vec::new(),
        };
        let (_, _, evidence_ids) =
            ensure_version(&tx, logical_id, SourceKind::Manual, &observed, at)?;
        let assertion_draft = AssertionDraft {
            evidence_idx: 0,
            kind: AssertionKind::Claim,
            subject: subject.clone(),
            subject_normalized,
            predicate: None,
            content: draft.content,
            extraction: EXTRACTION_MANUAL_WRITE.to_string(),
        };
        let assertion_id = get_or_create_assertion(&tx, evidence_ids[0], &assertion_draft, at)?;
        let persisted_subject: String = tx
            .query_row(
                "SELECT subject FROM assertions WHERE id = ?1",
                rusqlite::params![assertion_id.to_string()],
                |row| row.get(0),
            )
            .map_err(dberr)?;
        let active_id: Option<String> = tx
            .query_row(
                "SELECT id FROM memories WHERE assertion_id = ?1 AND state = 'active' \
                 AND producer_family = ?2",
                rusqlite::params![assertion_id.to_string(), MANUAL_PRODUCER_FAMILY],
                |row| row.get(0),
            )
            .optional()
            .map_err(dberr)?;

        let (memory_id, action) = match (draft.supersedes, active_id) {
            (Some(old_id), Some(active_id)) => {
                let active_id = uuid_from_storage(&active_id)?;
                if active_id == old_id {
                    let added = add_spaces(&tx, old_id, &spaces, at)?;
                    let action = if added.is_empty() {
                        MemoryActionKind::Unchanged
                    } else {
                        MemoryActionKind::Reused {
                            spaces_added: names_for_space_ids(&tx, &added)?,
                        }
                    };
                    (old_id, action)
                } else {
                    let added = add_spaces(&tx, active_id, &spaces, at)?;
                    supersede_existing_memory(&tx, old_id, active_id, at)?;
                    let _ = added;
                    (
                        active_id,
                        MemoryActionKind::Superseded {
                            old_memory_id: old_id,
                        },
                    )
                }
            }
            (Some(old_id), None) => {
                let new_id = create_memory(&tx, assertion_id, at, MANUAL_PRODUCER_FAMILY)?;
                add_spaces(&tx, new_id, &spaces, at)?;
                supersede_existing_memory(&tx, old_id, new_id, at)?;
                (
                    new_id,
                    MemoryActionKind::Superseded {
                        old_memory_id: old_id,
                    },
                )
            }
            (None, Some(active_id)) => {
                let active_id = uuid_from_storage(&active_id)?;
                let added = add_spaces(&tx, active_id, &spaces, at)?;
                let action = if added.is_empty() {
                    MemoryActionKind::Unchanged
                } else {
                    MemoryActionKind::Reused {
                        spaces_added: names_for_space_ids(&tx, &added)?,
                    }
                };
                (active_id, action)
            }
            (None, None) => {
                let new_id = create_memory(&tx, assertion_id, at, MANUAL_PRODUCER_FAMILY)?;
                add_spaces(&tx, new_id, &spaces, at)?;
                (new_id, MemoryActionKind::Created)
            }
        };
        let space_names = memory_space_names(&tx, memory_id)?;
        tx.commit()?;
        Ok(ManualMemoryReport {
            memory_id,
            assertion_id,
            subject: persisted_subject,
            action,
            spaces: space_names,
        })
    }

    fn add_memory_spaces(&self, memory_id: Uuid, space_ids: Vec<Uuid>) -> Result<Vec<String>> {
        let conn = self.connection()?;
        let tx = begin_write(&conn, &self.revision_state)?;
        validate_active_memory(&tx, memory_id)?;
        let spaces = validate_space_ids(&tx, &space_ids)?;
        let added = add_spaces(&tx, memory_id, &spaces, Ts::now())?;
        let names = names_for_space_ids(&tx, &added)?;
        tx.commit()?;
        Ok(names)
    }

    // -- Phase 2 M1: embeddings / vector index -----------------------------
    fn save_embeddings(&self, embeddings: &[AssertionEmbedding]) -> Result<usize> {
        if embeddings.is_empty() {
            return Ok(0);
        }
        let conn = self.connection()?;
        let tx = begin_write(&conn, &self.revision_state)?;
        let mut inserted = 0usize;
        for e in embeddings {
            // Validate before persisting: dimension must match and the vector
            // must be finite. Malformed input is a controlled error, never a
            // panic.
            if e.embedding.len() != e.dimension as usize {
                return Err(Error::InvalidVector {
                    detail: format!(
                        "embedding dimension mismatch: got {}, declared {}",
                        e.embedding.len(),
                        e.dimension
                    ),
                });
            }
            let blob = encode_f32_le(&e.embedding)?;
            let changed = tx
                .execute(
                    "INSERT INTO assertion_embeddings \
                     (assertion_id, derivation_key, model, model_version, dimension, embedding, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                     ON CONFLICT(assertion_id, derivation_key) DO NOTHING",
                    rusqlite::params![
                        e.assertion_id.to_string(),
                        e.derivation_key,
                        e.model,
                        e.model_version,
                        i64::from(e.dimension),
                        blob,
                        Ts::now().0,
                    ],
                )
                .map_err(dberr)?;
            inserted += changed;
        }
        tx.commit()?;
        Ok(inserted)
    }
    fn list_assertions_needing_embedding(
        &self,
        derivation_key: &str,
        after: Option<(Ts, Uuid)>,
        limit: u32,
    ) -> Result<Vec<UnembeddedAssertion>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let (sql, params): (String, Vec<Value>) = match after {
            None => (
                "SELECT a.id, a.subject, a.content, a.created_at \
                 FROM assertions a \
                 LEFT JOIN assertion_embeddings ae \
                   ON ae.assertion_id = a.id AND ae.derivation_key = ?1 \
                 WHERE ae.assertion_id IS NULL \
                 ORDER BY a.created_at ASC, a.id ASC \
                 LIMIT ?2"
                    .to_string(),
                vec![
                    Value::Text(derivation_key.to_string()),
                    Value::Integer(limit as i64),
                ],
            ),
            Some((ts, id)) => (
                "SELECT a.id, a.subject, a.content, a.created_at \
                 FROM assertions a \
                 LEFT JOIN assertion_embeddings ae \
                   ON ae.assertion_id = a.id AND ae.derivation_key = ?1 \
                 WHERE ae.assertion_id IS NULL \
                   AND (a.created_at > ?3 OR (a.created_at = ?3 AND a.id > ?4)) \
                 ORDER BY a.created_at ASC, a.id ASC \
                 LIMIT ?2"
                    .to_string(),
                vec![
                    Value::Text(derivation_key.to_string()),
                    Value::Integer(limit as i64),
                    Value::Integer(ts.0),
                    Value::Text(id.to_string()),
                ],
            ),
        };
        let conn = self.connection()?;
        let mut stmt = conn.prepare(&sql).map_err(dberr)?;
        let rows = stmt
            .query_map(params_from_iter(params), |r| {
                Ok(UnembeddedAssertion {
                    assertion_id: uuid_col(r, "id")?,
                    subject: r.get("subject")?,
                    content: r.get("content")?,
                    created_at: ts_col(r, "created_at")?,
                })
            })
            .map_err(dberr)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(dberr)?);
        }
        Ok(out)
    }

    fn embedding_index_status(&self, derivation_key: &str) -> Result<EmbeddingIndexStatus> {
        let session = self.read_session()?;
        let result = session.embedding_index_status(derivation_key)?;
        session.finish()?;
        Ok(result)
    }

    fn clear_embeddings(&self, derivation_key: &str) -> Result<usize> {
        let conn = self.connection()?;
        let tx = begin_write(&conn, &self.revision_state)?;
        let n = tx
            .execute(
                "DELETE FROM assertion_embeddings WHERE derivation_key = ?1",
                rusqlite::params![derivation_key],
            )
            .map_err(dberr)?;
        tx.commit()?;
        Ok(n)
    }

    fn read_embedding_settings(&self) -> Result<EmbeddingSettings> {
        let session = self.read_session()?;
        let result = session.read_embedding_settings()?;
        session.finish()?;
        Ok(result)
    }

    fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT value FROM mind_meta WHERE key = ?1",
            rusqlite::params![key],
            |r| r.get(0),
        )
        .optional()
        .map_err(dberr)
    }

    fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.connection()?;
        let tx = begin_write(&conn, &self.revision_state)?;
        tx.execute(
            "INSERT INTO mind_meta (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )
        .map_err(dberr)?;
        tx.commit()?;
        Ok(())
    }

    fn search_vectors(
        &self,
        query_vector: &[f32],
        query: &SearchQuery,
        derivation_key: &str,
    ) -> Result<Vec<SearchHitRaw>> {
        let session = self.read_session()?;
        let result = session.search_vectors(query_vector, query, derivation_key)?;
        session.finish()?;
        Ok(result)
    }

    fn list_memories_by_subject(&self, subject_normalized: &str) -> Result<Vec<MemorySummary>> {
        let session = self.read_session()?;
        let result = session.list_memories_by_subject(subject_normalized)?;
        session.finish()?;
        Ok(result)
    }

    fn list_replaced_by(&self, id: Uuid) -> Result<Vec<MemorySummary>> {
        let session = self.read_session()?;
        let result = session.list_replaced_by(id)?;
        session.finish()?;
        Ok(result)
    }

    fn memory_id_prefix_matches(&self, prefix: &str) -> Result<Vec<Uuid>> {
        let session = self.read_session()?;
        let result = session.memory_id_prefix_matches(prefix)?;
        session.finish()?;
        Ok(result)
    }

    fn search(&self, query: &SearchQuery) -> Result<Vec<SearchHitRaw>> {
        let session = self.read_session()?;
        let result = session.search(query)?;
        session.finish()?;
        Ok(result)
    }

    fn list_conflicts(&self) -> Result<Vec<ConflictGroup>> {
        let session = self.read_session()?;
        let result = session.list_conflicts()?;
        session.finish()?;
        Ok(result)
    }

    fn retire_memory(&self, id: Uuid, reason: Option<String>, at: Ts) -> Result<Memory> {
        let conn = self.connection()?;
        let tx = begin_write(&conn, &self.revision_state)?;
        // The state check and the transition are one conditional statement,
        // so only an active memory can be retired, atomically.
        let changed = tx
            .execute(
                "UPDATE memories SET state = 'retired', retired_at = ?1, retire_reason = ?2 \
                 WHERE id = ?3 AND state = 'active'",
                rusqlite::params![at.0, reason, id.to_string()],
            )
            .map_err(dberr)?;
        if changed == 0 {
            let state: Option<String> = tx
                .query_row(
                    "SELECT state FROM memories WHERE id = ?1",
                    rusqlite::params![id.to_string()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(dberr)?;
            return match state {
                Some(state) => Err(Error::NotRetirable {
                    id: id.to_string(),
                    state: MemoryState::parse(&state)
                        .map(|s| s.to_string())
                        .unwrap_or(state),
                }),
                None => Err(Error::MemoryNotFound {
                    prefix: id.to_string(),
                }),
            };
        }
        let memory = tx
            .query_row(
                "SELECT id, assertion_id, state, created_at, superseded_by, superseded_at, \
                        retired_at, retire_reason \
                 FROM memories WHERE id = ?1",
                rusqlite::params![id.to_string()],
                row_memory,
            )
            .map_err(dberr)?;
        tx.commit()?;
        Ok(memory)
    }
}

fn load_temporal_capabilities(conn: &Connection) -> Result<TemporalHistoryCapabilities> {
    let baseline_revision: i64 = conn
        .query_row("SELECT MIN(revision_id) FROM m6_revisions", [], |row| {
            row.get(0)
        })
        .map_err(dberr)?;
    let current_revision: i64 = conn
        .query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |row| {
            row.get(0)
        })
        .map_err(dberr)?;
    let mut stmt = conn
        .prepare(
            "SELECT dimension, status, complete_from_revision, reason \
             FROM m6_history_coverage ORDER BY dimension",
        )
        .map_err(dberr)?;
    let rows = stmt
        .query_map([], |row| {
            let dimension_name: String = row.get(0)?;
            let status: String = row.get(1)?;
            let complete_from_revision: Option<i64> = row.get(2)?;
            let reason: Option<String> = row.get(3)?;
            let dimension = TemporalDimension::all()
                .iter()
                .copied()
                .find(|d| d.as_str() == dimension_name)
                .ok_or_else(|| {
                    rusqlite::Error::InvalidColumnType(0, "dimension".to_string(), Type::Text)
                })?;
            Ok(TemporalCoverage {
                dimension,
                status: match status.as_str() {
                    "covered" => TemporalCoverageStatus::Covered,
                    _ => TemporalCoverageStatus::Unsupported,
                },
                baseline_revision,
                complete_from_revision,
                reason,
            })
        })
        .map_err(dberr)?;
    let mut dimensions = Vec::new();
    for row in rows {
        dimensions.push(row.map_err(dberr)?);
    }
    Ok(TemporalHistoryCapabilities {
        baseline_revision,
        current_revision,
        dimensions,
    })
}

fn require_temporal_coverage(
    conn: &Connection,
    revision: i64,
    dimensions: &[TemporalDimension],
) -> Result<()> {
    if revision < 1 {
        return Err(Error::TemporalHistoryUnavailable {
            revision,
            dimension: "recording_revision".to_string(),
            reason: "revision must be positive".to_string(),
        });
    }
    for dimension in dimensions {
        let row: Option<(i64, String, Option<i64>, Option<String>)> = conn
            .query_row(
                "SELECT baseline_revision, status, complete_from_revision, reason \
                 FROM m6_history_coverage WHERE dimension=?1",
                rusqlite::params![dimension.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(dberr)?;
        let Some((baseline, status, complete_from, reason)) = row else {
            return Err(Error::TemporalHistoryUnavailable {
                revision,
                dimension: dimension.as_str().to_string(),
                reason: "no coverage record exists".to_string(),
            });
        };
        if status != "covered" {
            return Err(Error::TemporalHistoryUnavailable {
                revision,
                dimension: dimension.as_str().to_string(),
                reason: reason.unwrap_or_else(|| "dimension is unsupported".to_string()),
            });
        }
        if revision < baseline {
            return Err(Error::TemporalHistoryUnavailable {
                revision,
                dimension: dimension.as_str().to_string(),
                reason: "requested revision precedes the v8 baseline".to_string(),
            });
        }
        if let Some(complete_from) = complete_from {
            if revision < complete_from {
                return Err(Error::TemporalHistoryUnavailable {
                    revision,
                    dimension: dimension.as_str().to_string(),
                    reason: format!("coverage is only complete from revision {complete_from}"),
                });
            }
        }
    }
    let max_revision: Option<i64> = conn
        .query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |row| {
            row.get(0)
        })
        .map_err(dberr)?;
    if revision > max_revision.unwrap_or(0) {
        return Err(Error::TemporalHistoryUnavailable {
            revision,
            dimension: "recording_revision".to_string(),
            reason: "requested revision is not yet recorded".to_string(),
        });
    }
    Ok(())
}

use rusqlite::params_from_iter;

// ---------------------------------------------------------------------------
// Observation / ingestion transaction (docs/memory-model.md section 6)
// ---------------------------------------------------------------------------

/// The per-path classification before execution.
enum Plan {
    Unchanged {
        location: String,
        logical: Uuid,
        file_idx: usize,
    },
    Changed {
        location: String,
        logical: Uuid,
        file_idx: usize,
    },
    Created {
        location: String,
        file_idx: usize,
    },
    Continue {
        location: String,
        logical: Uuid,
        from: Option<String>,
        file_idx: usize,
    },
}

/// Captured file identity/lifecycle classification for one serialized operation.
/// This is neither a persisted plan nor proof of physical file or alias identity.
/// Application still performs ordinary version/knowledge reconciliation.
struct FileObservationPlan<'conn> {
    conn: &'conn Connection,
    session: crate::core::SessionDraft,
    files: Vec<ObservedFile>,
    spaces: Vec<Uuid>,
    classifications: Vec<Plan>,
    residual_disappearances: BTreeMap<Uuid, String>,
}

/// What the engine believes about one logical source inside an ingest
/// root: its expected location and the hash of its latest version.
type KnownSource = (String, Option<String>);

/// The relationship resolution of [`Store::resolve_git_code_relationships`], inside a transaction
/// the caller owns (and commits). It reads the code rows of the given commit, so inside the
/// transaction that wrote them it sees them before they are committed.
fn resolve_git_code_relationships_tx(
    tx: &Connection,
    repository_id: Uuid,
    commit_oid: &str,
    resolver: &str,
    version: &str,
) -> Result<crate::core::CodeResolutionReport> {
    let existing:Option<String>=tx.query_row("SELECT id FROM code_resolution_derivations WHERE repository_id=?1 AND commit_oid=?2 AND resolver=?3 AND resolver_version=?4",rusqlite::params![repository_id.to_string(),commit_oid,resolver,version],|r|r.get(0)).optional().map_err(dberr)?;
    if existing.is_some() {
        return Ok(crate::core::CodeResolutionReport {
            derivations_reused: 1,
            ..Default::default()
        });
    };
    let did = Uuid::now_v7();
    tx.execute("INSERT INTO code_resolution_derivations(id,repository_id,commit_oid,resolver,resolver_version,created_at) VALUES(?1,?2,?3,?4,?5,?6)",rusqlite::params![did.to_string(),repository_id.to_string(),commit_oid,resolver,version,Ts::now().0]).map_err(dberr)?;
    let mut report = crate::core::CodeResolutionReport {
        derivations_created: 1,
        ..Default::default()
    };
    let module_index = rust_module_index(tx, repository_id, commit_oid)?;
    let mut stmt=tx.prepare(&format!("SELECT DISTINCT o.id,o.kind,o.raw_target,o.source_syntax_path,c.path FROM code_relationship_observations o JOIN code_derivations d ON d.id=o.derivation_id JOIN code_inputs i ON i.id=d.input_id JOIN code_contexts c ON c.input_id=i.id WHERE i.repository_id=?1 AND c.commit_oid=?2 AND {EFFECTIVE_DERIVATION} ORDER BY o.id,c.path")).map_err(dberr)?;
    let rows = stmt
        .query_map(
            rusqlite::params![repository_id.to_string(), commit_oid],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                ))
            },
        )
        .map_err(dberr)?;
    // A content-addressed observation can occur under several paths in one
    // commit.  The v0.5 resolution table has no path key, so resolving one
    // arbitrarily would create a false edge.  Retain one honest unresolved
    // observation until a later schema slice adds path-local results.
    let mut observations: BTreeMap<String, (String, String, Option<String>, Option<String>)> =
        BTreeMap::new();
    for row in rows {
        let (id, kind, raw, source, path) = row.map_err(dberr)?;
        if !module_index.reachable_paths.contains(&path) {
            continue;
        }
        match observations.entry(id) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert((kind, raw, source, Some(path)));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if entry.get().3.as_deref() != Some(path.as_str()) {
                    entry.get_mut().3 = None;
                }
            }
        }
    }
    drop(stmt);
    for (oid, (kind, raw, source, path)) in observations {
        let current_module = path
            .as_ref()
            .and_then(|path| module_index.module_for_observation(path, source.as_deref()));
        let external = kind == "imports" && !is_internal_rust_path(&raw);
        if external {
            tx.execute("INSERT INTO code_relationship_resolutions(id,resolution_derivation_id,observation_id,state,target_entity_id,raw_external_target) VALUES(?1,?2,?3,'external',NULL,?4)",rusqlite::params![Uuid::now_v7().to_string(),did.to_string(),oid,raw]).map_err(dberr)?;
            report.external += 1;
            continue;
        }
        let candidates = current_module
            .as_deref()
            .and_then(|module| rust_target_path(&raw, module))
            .map(|target| module_index.candidates(&target, &kind))
            .unwrap_or_default();
        let state = if candidates.len() == 1 {
            "resolved"
        } else if candidates.is_empty() {
            "unresolved"
        } else {
            "ambiguous"
        };
        let rid = Uuid::now_v7();
        tx.execute("INSERT INTO code_relationship_resolutions(id,resolution_derivation_id,observation_id,state,target_entity_id,raw_external_target) VALUES(?1,?2,?3,?4,?5,NULL)",rusqlite::params![rid.to_string(),did.to_string(),oid,state,if candidates.len()==1{Some(candidates[0].clone())}else{None}]).map_err(dberr)?;
        if state == "ambiguous" {
            for c in candidates {
                tx.execute(
                    "INSERT INTO code_resolution_candidates(resolution_id,entity_id) VALUES(?1,?2)",
                    rusqlite::params![rid.to_string(), c],
                )
                .map_err(dberr)?;
            }
            report.ambiguous += 1
        } else if state == "resolved" {
            report.resolved += 1
        } else {
            report.unresolved += 1
        }
    }
    Ok(report)
}

/// The code-structure write of [`Store::record_code_observation`], inside a transaction the
/// caller owns (and commits).
fn record_code_observation_tx(
    tx: &Connection,
    observation: CodeObservation,
) -> Result<CodeObservationReport> {
    let mut report = CodeObservationReport::default();
    for input in observation.inputs {
        report.supported_inputs += 1;
        let existing: Option<String> = match (&input.repository_id, &input.blob_oid) {
            (Some(repo), Some(blob)) => tx
                .query_row(
                    "SELECT id FROM code_inputs WHERE repository_id=?1 AND blob_oid=?2",
                    rusqlite::params![repo.to_string(), blob],
                    |r| r.get(0),
                )
                .optional()
                .map_err(dberr)?,
            _ => tx
                .query_row(
                    "SELECT id FROM code_inputs WHERE repository_id IS NULL AND content_hash=?1",
                    rusqlite::params![input.content_hash],
                    |r| r.get(0),
                )
                .optional()
                .map_err(dberr)?,
        };
        let is_new = existing.is_none();
        let input_id = existing.unwrap_or_else(|| Uuid::now_v7().to_string());
        if is_new {
            tx.execute("INSERT INTO code_inputs(id,content_hash,repository_id,blob_oid,created_at) VALUES(?1,?2,?3,?4,?5)",rusqlite::params![input_id,input.content_hash,input.repository_id.map(|x|x.to_string()),input.blob_oid,Ts::now().0]).map_err(dberr)?;
        }
        for context in input.contexts {
            // A directory context has no commit id. A NULL in the key makes every row distinct,
            // so INSERT OR IGNORE would add the same context again on each re-ingestion (and
            // `code_search` would repeat every hit): check explicitly.
            if context.commit_oid.is_none() {
                tx.execute(
                    "INSERT INTO code_contexts(input_id,path,commit_oid) SELECT ?1,?2,NULL \
                     WHERE NOT EXISTS (SELECT 1 FROM code_contexts \
                     WHERE input_id=?1 AND path=?2 AND commit_oid IS NULL)",
                    rusqlite::params![input_id, context.path],
                )
                .map_err(dberr)?;
            } else {
                tx.execute(
                    "INSERT OR IGNORE INTO code_contexts(input_id,path,commit_oid) VALUES(?1,?2,?3)",
                    rusqlite::params![input_id, context.path, context.commit_oid],
                )
                .map_err(dberr)?;
            }
        }
        let d = &input.derivation;
        let derivation_id:Option<String>=tx.query_row("SELECT id FROM code_derivations WHERE input_id=?1 AND extractor=?2 AND extractor_version=?3 AND grammar=?4 AND grammar_version=?5",rusqlite::params![input_id,d.identity.extractor,d.identity.extractor_version,d.identity.grammar,d.identity.grammar_version],|r|r.get(0)).optional().map_err(dberr)?;
        // A policy skip (status `failed`, identity with a `+skip(` suffix) is counted
        // whether the row is new or already there.
        let skip_reason = crate::ingest::code::skip_reason_of(&d.identity);
        if let Some(reason) = &skip_reason {
            report.skipped += 1;
            *report.skip_reasons.entry(reason.clone()).or_default() += 1;
        }
        if derivation_id.is_some() {
            report.reused += 1;
            continue;
        }
        let did = Uuid::now_v7();
        tx.execute("INSERT INTO code_derivations(id,input_id,language,extractor,extractor_version,grammar,grammar_version,status,error_count,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",rusqlite::params![did.to_string(),input_id,input.language.as_str(),d.identity.extractor,d.identity.extractor_version,d.identity.grammar,d.identity.grammar_version,d.status.as_str(),d.error_count,Ts::now().0]).map_err(dberr)?;
        if skip_reason.is_none() {
            report.parsed += 1;
            report.entities += d.entities.len() as u64;
            report.parse_errors += u64::from(d.status != crate::core::CodeParseStatus::Complete);
        }
        for e in &d.entities {
            tx.execute("INSERT INTO code_entities(id,derivation_id,kind,name,qualified_name,syntax_path,parent_syntax_path,start_byte,end_byte,start_line,start_column,end_line,end_column,signature,visibility,is_async) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",rusqlite::params![Uuid::now_v7().to_string(),did.to_string(),e.kind.as_str(),e.name,e.qualified_name,e.syntax_path,e.parent_syntax_path,e.range.start_byte as i64,e.range.end_byte as i64,e.range.start_line,e.range.start_column,e.range.end_line,e.range.end_column,e.signature,e.visibility,e.is_async]).map_err(dberr)?;
        }
        for i in &d.imports {
            tx.execute("INSERT INTO code_imports(id,derivation_id,raw,target,alias,start_byte,end_byte,start_line,start_column,end_line,end_column) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",rusqlite::params![Uuid::now_v7().to_string(),did.to_string(),i.raw,i.target,i.alias,i.range.start_byte as i64,i.range.end_byte as i64,i.range.start_line,i.range.start_column,i.range.end_line,i.range.end_column]).map_err(dberr)?;
        }
        for relationship in &d.relationships {
            tx.execute("INSERT INTO code_relationship_observations(id,derivation_id,kind,source_syntax_path,raw_target,start_byte,end_byte) VALUES(?1,?2,?3,?4,?5,?6,?7)", rusqlite::params![Uuid::now_v7().to_string(),did.to_string(),relationship.kind.as_str(),relationship.source_syntax_path,relationship.raw_target,relationship.range.start_byte as i64,relationship.range.end_byte as i64]).map_err(dberr)?;
        }
    }
    Ok(report)
}

/// Read the complete file classification before any ordinary mutation effects.
/// The caller retains the existing serialized M6 transaction throughout planning
/// and application; supplied roots/locations remain ordinary strings.
fn plan_file_observation<'conn>(
    conn: &'conn Connection,
    observation: IngestObservation,
) -> Result<FileObservationPlan<'conn>> {
    let IngestObservation {
        session,
        mut files,
        spaces,
    } = observation;
    let full = session.scope == IngestScope::Directory;
    let root = session.root.clone();

    // Deterministic processing order.
    files.sort_by(|a, b| a.location.cmp(&b.location));

    // What the engine currently believes exists under this root, derived
    // from logical source state (not from the previous session), so
    // interleaved partial ingests are handled correctly. Only file-kind
    // logical sources participate in file observations; future source
    // kinds bring their own observation adapters.
    let mut present: BTreeMap<Uuid, KnownSource> = BTreeMap::new();
    let mut absent: BTreeMap<Uuid, KnownSource> = BTreeMap::new();
    if full {
        let mut stmt = conn
            .prepare(
                "SELECT id, state, current_location FROM logical_sources \
                 WHERE kind = 'file' AND current_location IS NOT NULL",
            )
            .map_err(dberr)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(dberr)?;
        for r in rows {
            let (id, state, location) = r.map_err(dberr)?;
            let logical = Uuid::parse_str(&id)
                .map_err(|e| Error::Storage(format!("corrupt id '{id}': {e}")))?;
            // Root scoping: only logical sources living under the observed
            // root take part in this boundary's lifecycle reasoning.
            if !Path::new(&location).starts_with(&root) {
                continue;
            }
            let hash = latest_version_hash(conn, &logical)?;
            if state == "present" {
                present.insert(logical, (location, hash));
            } else {
                absent.insert(logical, (location, hash));
            }
        }
    }

    // --- classification -----------------------------------------------------
    let mut plans: Vec<Plan> = Vec::new();
    let mut unmatched_new: Vec<usize> = Vec::new(); // indices into files
    let mut matched: HashMap<Uuid, usize> = HashMap::new(); // logical -> file idx
    let mut gone: BTreeMap<Uuid, String> = BTreeMap::new(); // logical -> last location

    if full {
        let mut expected: HashMap<String, Uuid> = HashMap::new();
        for (logical, (location, _)) in &present {
            expected.insert(location.clone(), *logical);
        }
        for (idx, file) in files.iter().enumerate() {
            if let Some(logical) = expected.get(&file.location).copied() {
                let latest = present[&logical].1.as_deref();
                if latest == Some(file.content_hash.as_str()) {
                    plans.push(Plan::Unchanged {
                        location: file.location.clone(),
                        logical,
                        file_idx: idx,
                    });
                } else {
                    plans.push(Plan::Changed {
                        location: file.location.clone(),
                        logical,
                        file_idx: idx,
                    });
                }
            } else {
                plans.push(Plan::Created {
                    location: file.location.clone(),
                    file_idx: idx,
                });
                unmatched_new.push(idx);
            }
        }
        for (logical, (location, _)) in &present {
            if !files.iter().any(|f| &f.location == location) {
                gone.insert(*logical, location.clone());
            }
        }
    } else {
        // Partial (file-scope) observation: path-local continuity only.
        // Never marks anything disappeared, never infers moves.
        for (idx, file) in files.iter().enumerate() {
            if let Some(logical) = present_logical_at(conn, &file.location)? {
                if latest_version_hash(conn, &logical)?.as_deref()
                    == Some(file.content_hash.as_str())
                {
                    plans.push(Plan::Unchanged {
                        location: file.location.clone(),
                        logical,
                        file_idx: idx,
                    });
                } else {
                    plans.push(Plan::Changed {
                        location: file.location.clone(),
                        logical,
                        file_idx: idx,
                    });
                }
            } else if let Some(logical) = absent_logical_at(conn, &file.location)? {
                let continues =
                    latest_version_hash(conn, &logical)?.is_some_and(|h| h == file.content_hash);
                if continues {
                    plans.push(Plan::Continue {
                        location: file.location.clone(),
                        logical,
                        from: Some(file.location.clone()),
                        file_idx: idx,
                    });
                    continue;
                }
                // A new document recycled the path of an absent source and
                // its content differs; identity is not proven.
                plans.push(Plan::Created {
                    location: file.location.clone(),
                    file_idx: idx,
                });
            } else {
                plans.push(Plan::Created {
                    location: file.location.clone(),
                    file_idx: idx,
                });
            }
        }
    }

    // Move inference (full observations only). Identical content is
    // evidence, not proof: a move is inferred only when exactly one
    // candidate logical source with that content is unaccounted for AND
    // exactly one new path with that content appeared. Candidates are
    // logicals that just went missing from this root plus logicals already
    // absent from it (reappearance). Copies and ambiguous multi-way
    // matches stay separate; false merging is worse than failing to infer
    // a move.
    if full {
        for (idx, file) in files.iter().enumerate() {
            if !unmatched_new.contains(&idx) {
                continue;
            }
            let mut candidates: Vec<(Uuid, Option<String>)> = Vec::new();
            for logical in gone.keys() {
                if !matched.contains_key(logical) {
                    candidates.push((*logical, Some(gone[logical].clone())));
                }
            }
            for logical in absent.keys() {
                if !matched.contains_key(logical) && !gone.contains_key(logical) {
                    candidates.push((*logical, Some(absent[logical].0.clone())));
                }
            }
            let hash_of = |logical: &Uuid| -> Option<String> {
                if gone.contains_key(logical) {
                    // This hash was read fallibly while capturing `present`.
                    // A failed lookup must never become missing move evidence.
                    present[logical].1.clone()
                } else {
                    absent.get(logical).and_then(|(_, h)| h.clone())
                }
            };
            let same_hash_candidates: Vec<Uuid> = candidates
                .iter()
                .filter(|(logical, _)| {
                    hash_of(logical).as_deref() == Some(file.content_hash.as_str())
                })
                .map(|(logical, _)| *logical)
                .collect();
            let same_hash_new: Vec<usize> = unmatched_new
                .iter()
                .copied()
                .filter(|&i| files[i].content_hash == file.content_hash)
                .collect();
            if same_hash_candidates.len() == 1 && same_hash_new.len() == 1 {
                let logical = same_hash_candidates[0];
                matched.insert(logical, idx);
                unmatched_new.retain(|&i| i != idx);
                let from = candidates
                    .iter()
                    .find(|(l, _)| *l == logical)
                    .and_then(|(_, from)| from.clone());
                if let Some(plan) = plans
                    .iter_mut()
                    .find(|p| matches!(p, Plan::Created { file_idx, .. } if *file_idx == idx))
                {
                    *plan = Plan::Continue {
                        location: file.location.clone(),
                        logical,
                        from,
                        file_idx: idx,
                    };
                }
            }
        }
    }

    // Continued moves are already represented by their per-file classification.
    // Only unmatched disappearances remain for application; partial observations
    // never populate this map.
    gone.retain(|logical, _| !matched.contains_key(logical));
    Ok(FileObservationPlan {
        conn,
        session,
        files,
        spaces,
        classifications: plans,
        residual_disappearances: gone,
    })
}

fn record_observation_tx(
    conn: &Connection,
    observation: IngestObservation,
) -> Result<ObservationReport> {
    #[cfg(test)]
    observation_planning_tests::checkpoint("before_plan", conn)?;
    let plan = plan_file_observation(conn, observation)?;
    #[cfg(test)]
    observation_planning_tests::checkpoint("after_plan", conn)?;
    apply_file_observation(plan)
}

/// Apply captured file classifications; ordinary version/knowledge reconciliation remains here.
fn apply_file_observation(plan: FileObservationPlan<'_>) -> Result<ObservationReport> {
    let FileObservationPlan {
        conn,
        session,
        files,
        spaces,
        classifications,
        residual_disappearances,
    } = plan;
    let at = session.observed_at;
    let root = session.root.clone();

    // Preserve the original relative UUID allocation order: session first, then
    // any new logical sources/versions created by the existing execution path.
    let session_id = Uuid::now_v7();
    conn.execute(
        "INSERT INTO ingest_sessions (id, root, scope, observed_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            session_id.to_string(),
            session.root,
            session.scope.as_str(),
            at.0,
        ],
    )
    .map_err(dberr)?;
    upsert_scope(conn, None, Some(&session.root), &spaces, at)?;

    // --- execution -----------------------------------------------------------
    let mut report_files: Vec<FileObservation> = Vec::new();
    let mut events: Vec<SourceEvent> = Vec::new();

    for plan in classifications {
        match plan {
            Plan::Unchanged {
                location,
                logical,
                file_idx,
            } => {
                // Reconcile even for unchanged content: re-ingest with
                // additional --space flags must complete space memberships,
                // and a manually retired memory must be recreated on an
                // explicit re-ingest. Content equality guarantees the
                // reconciliation can only reuse (never supersede).
                let (_, _, evidence_ids) =
                    ensure_version(conn, logical, SourceKind::File, &files[file_idx], at)?;
                let actions = reconcile_logical(
                    conn,
                    logical,
                    &files[file_idx],
                    &evidence_ids,
                    at,
                    FILE_PRODUCER_FAMILY,
                )?;
                record_observation_row(conn, session_id, logical, &files[file_idx], at)?;
                report_files.push(FileObservation {
                    location,
                    logical_id: logical,
                    outcome: FileOutcome::Unchanged,
                    actions,
                });
            }
            Plan::Changed {
                location,
                logical,
                file_idx,
            } => {
                let (_, _, evidence_ids) =
                    ensure_version(conn, logical, SourceKind::File, &files[file_idx], at)?;
                let actions = reconcile_logical(
                    conn,
                    logical,
                    &files[file_idx],
                    &evidence_ids,
                    at,
                    FILE_PRODUCER_FAMILY,
                )?;
                record_observation_row(conn, session_id, logical, &files[file_idx], at)?;
                report_files.push(FileObservation {
                    location,
                    logical_id: logical,
                    outcome: FileOutcome::Changed,
                    actions,
                });
            }
            Plan::Created { location, file_idx } => {
                let logical = create_logical_source(conn, &files[file_idx], at)?;
                let (_, _, evidence_ids) =
                    ensure_version(conn, logical, SourceKind::File, &files[file_idx], at)?;
                let actions = reconcile_logical(
                    conn,
                    logical,
                    &files[file_idx],
                    &evidence_ids,
                    at,
                    FILE_PRODUCER_FAMILY,
                )?;
                record_observation_row(conn, session_id, logical, &files[file_idx], at)?;
                report_files.push(FileObservation {
                    location,
                    logical_id: logical,
                    outcome: FileOutcome::Created,
                    actions,
                });
            }
            Plan::Continue {
                location,
                logical,
                from,
                file_idx,
            } => {
                // The content version already exists and is reused; only
                // the lifecycle moves. No knowledge is touched.
                let (added, _, _) =
                    ensure_version(conn, logical, SourceKind::File, &files[file_idx], at)?;
                debug_assert!(!added, "a continued source reuses its version");
                conn.execute(
                    "UPDATE logical_sources SET state = 'present', current_location = ?1 \
                     WHERE id = ?2",
                    rusqlite::params![location, logical.to_string()],
                )
                .map_err(dberr)?;
                let event = insert_source_event(
                    conn,
                    logical,
                    SourceEventKind::Moved {
                        from_location: from.clone(),
                        to_location: location.clone(),
                    },
                    None,
                    session_id,
                    at,
                )?;
                events.push(event);
                record_observation_row(conn, session_id, logical, &files[file_idx], at)?;
                report_files.push(FileObservation {
                    location,
                    logical_id: logical,
                    outcome: FileOutcome::Continued {
                        from_location: from,
                    },
                    actions: Vec::new(),
                });
            }
        }
    }

    // Disappearances: full observations only, scoped to the observed root.
    // This records absence; it does NOT retire derived memories
    // (disappearance is not falsehood).
    for (logical, location) in &residual_disappearances {
        conn.execute(
            "UPDATE logical_sources SET state = 'absent' WHERE id = ?1",
            rusqlite::params![logical.to_string()],
        )
        .map_err(dberr)?;
        let event = insert_source_event(
            conn,
            *logical,
            SourceEventKind::Disappeared {
                from_location: Some(location.clone()),
            },
            None,
            session_id,
            at,
        )?;
        events.push(event);
    }

    Ok(ObservationReport {
        session_id,
        root,
        scope: session.scope,
        files: report_files,
        events,
        code: Default::default(),
    })
}

/// Latest version of a logical source as (version id, content hash).
fn latest_version(conn: &Connection, logical: &Uuid) -> Result<Option<(Uuid, String)>> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT id, content_hash FROM sources WHERE logical_id = ?1 \
             ORDER BY ingested_at DESC, id DESC LIMIT 1",
            rusqlite::params![logical.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(dberr)?;
    row.map(|(id, hash)| {
        Ok((
            Uuid::parse_str(&id).map_err(|e| Error::Storage(format!("corrupt id '{id}': {e}")))?,
            hash,
        ))
    })
    .transpose()
}

fn latest_version_hash(conn: &Connection, logical: &Uuid) -> Result<Option<String>> {
    Ok(latest_version(conn, logical)?.map(|(_, hash)| hash))
}

fn present_logical_at(conn: &Connection, location: &str) -> Result<Option<Uuid>> {
    logical_at(conn, location, "present")
}

fn absent_logical_at(conn: &Connection, location: &str) -> Result<Option<Uuid>> {
    logical_at(conn, location, "absent")
}

fn logical_at(conn: &Connection, location: &str, state: &str) -> Result<Option<Uuid>> {
    let sql = format!(
        "SELECT id FROM logical_sources WHERE kind = 'file' AND current_location = ?1 \
         AND state = '{state}' ORDER BY created_at DESC, id DESC LIMIT 1"
    );
    let id: Option<String> = conn
        .query_row(&sql, rusqlite::params![location], |r| r.get(0))
        .optional()
        .map_err(dberr)?;
    id.map(|s| Uuid::parse_str(&s).map_err(|e| Error::Storage(format!("corrupt id '{s}': {e}"))))
        .transpose()
}

fn create_logical_source(conn: &Connection, file: &ObservedFile, at: Ts) -> Result<Uuid> {
    let id = Uuid::now_v7();
    conn.execute(
        "INSERT INTO logical_sources (id, kind, state, current_location, created_at) \
         VALUES (?1, 'file', 'present', ?2, ?3)",
        rusqlite::params![id.to_string(), file.location, at.0],
    )
    .map_err(dberr)?;
    Ok(id)
}

/// Finds the version of this logical source with this content, or creates
/// it (with its evidence rows). Returns
/// (version was added, version id, evidence ids in draft order).
fn ensure_version(
    conn: &Connection,
    logical: Uuid,
    kind: SourceKind,
    file: &ObservedFile,
    at: Ts,
) -> Result<(bool, Uuid, Vec<Uuid>)> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM sources WHERE logical_id = ?1 AND content_hash = ?2",
            rusqlite::params![logical.to_string(), file.content_hash],
            |r| r.get(0),
        )
        .optional()
        .map_err(dberr)?;
    if let Some(id) = existing {
        let version_id =
            Uuid::parse_str(&id).map_err(|e| Error::Storage(format!("corrupt id '{id}': {e}")))?;
        let mut evidence_ids = Vec::with_capacity(file.evidences.len());
        let mut stmt = conn
            .prepare("SELECT id FROM evidence WHERE source_id = ?1 ORDER BY ordinal")
            .map_err(dberr)?;
        let rows = stmt
            .query_map(rusqlite::params![version_id.to_string()], |r| {
                r.get::<_, String>(0)
            })
            .map_err(dberr)?;
        for r in rows {
            let s = r.map_err(dberr)?;
            evidence_ids.push(
                Uuid::parse_str(&s)
                    .map_err(|e| Error::Storage(format!("corrupt id '{s}': {e}")))?,
            );
        }
        return Ok((false, version_id, evidence_ids));
    }
    let version_id = Uuid::now_v7();
    conn.execute(
        "INSERT INTO sources (id, logical_id, kind, location, title, content, content_hash, \
             media_type, size_bytes, ingested_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            version_id.to_string(),
            logical.to_string(),
            kind.as_str(),
            file.location,
            file.title,
            file.content,
            file.content_hash,
            file.media_type,
            file.size_bytes as i64,
            at.0,
        ],
    )
    .map_err(dberr)?;
    let mut evidence_ids = Vec::with_capacity(file.evidences.len());
    for d in &file.evidences {
        let id = Uuid::now_v7();
        conn.execute(
            "INSERT INTO evidence (id, source_id, kind, heading_path, start_line, end_line, \
                 ordinal, content, content_hash) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                id.to_string(),
                version_id.to_string(),
                d.kind.as_str(),
                d.heading_path,
                d.start_line.map(i64::from),
                d.end_line.map(i64::from),
                i64::from(d.ordinal),
                d.content,
                d.content_hash,
            ],
        )
        .map_err(dberr)?;
        evidence_ids.push(id);
    }
    Ok((true, version_id, evidence_ids))
}

fn ensure_manual_logical_source(conn: &Connection, at: Ts) -> Result<Uuid> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM logical_sources WHERE kind = 'manual' AND current_location = ?1",
            rusqlite::params![MANUAL_SOURCE_LOCATION],
            |row| row.get(0),
        )
        .optional()
        .map_err(dberr)?;
    if let Some(id) = existing {
        return uuid_from_storage(&id);
    }

    let id = Uuid::now_v7();
    conn.execute(
        "INSERT INTO logical_sources (id, kind, state, current_location, created_at) \
         VALUES (?1, ?2, 'present', ?3, ?4)",
        rusqlite::params![
            id.to_string(),
            LogicalKind::Manual.as_str(),
            MANUAL_SOURCE_LOCATION,
            at.0,
        ],
    )
    .map_err(dberr)?;
    Ok(id)
}

fn validate_space_ids(conn: &Connection, space_ids: &[Uuid]) -> Result<Vec<Uuid>> {
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for id in space_ids {
        if !seen.insert(*id) {
            continue;
        }
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM spaces WHERE id = ?1",
                rusqlite::params![id.to_string()],
                |_| Ok(()),
            )
            .optional()
            .map_err(dberr)?
            .is_some();
        if !exists {
            return Err(Error::SpaceIdNotFound { id: id.to_string() });
        }
        unique.push(*id);
    }
    Ok(unique)
}

fn names_for_space_ids(conn: &Connection, space_ids: &[Uuid]) -> Result<Vec<String>> {
    space_ids
        .iter()
        .map(|id| {
            conn.query_row(
                "SELECT name FROM spaces WHERE id = ?1",
                rusqlite::params![id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(dberr)?
            .ok_or_else(|| Error::SpaceIdNotFound { id: id.to_string() })
        })
        .collect()
}

fn memory_space_names(conn: &Connection, memory_id: Uuid) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT s.name FROM memory_spaces ms JOIN spaces s ON s.id = ms.space_id \
             WHERE ms.memory_id = ?1 ORDER BY s.name",
        )
        .map_err(dberr)?;
    let rows = stmt
        .query_map(rusqlite::params![memory_id.to_string()], |row| row.get(0))
        .map_err(dberr)?;
    let mut names = Vec::new();
    for row in rows {
        names.push(row.map_err(dberr)?);
    }
    Ok(names)
}

fn validate_active_memory(conn: &Connection, memory_id: Uuid) -> Result<()> {
    let state: Option<String> = conn
        .query_row(
            "SELECT state FROM memories WHERE id = ?1",
            rusqlite::params![memory_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(dberr)?;
    match state {
        None => Err(Error::MemoryNotFound {
            prefix: memory_id.to_string(),
        }),
        Some(state) if state == MemoryState::Active.as_str() => Ok(()),
        Some(state) => Err(Error::MemoryNotActive {
            id: memory_id.to_string(),
            state,
        }),
    }
}

fn validate_manual_update_target(conn: &Connection, memory_id: Uuid) -> Result<()> {
    let target: Option<(String, String, String, String, String)> = conn
        .query_row(
            "SELECT m.state, src.kind, ls.kind, a.extraction, e.kind FROM memories m \
             JOIN assertions a ON a.id = m.assertion_id \
             JOIN evidence e ON e.id = a.evidence_id \
             JOIN sources src ON src.id = e.source_id \
             JOIN logical_sources ls ON ls.id = src.logical_id WHERE m.id = ?1",
            rusqlite::params![memory_id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(dberr)?;
    let Some((state, source_kind, logical_kind, extraction, evidence_kind)) = target else {
        return Err(Error::MemoryNotFound {
            prefix: memory_id.to_string(),
        });
    };
    if source_kind != SourceKind::Manual.as_str()
        || logical_kind != LogicalKind::Manual.as_str()
        || extraction != EXTRACTION_MANUAL_WRITE
        || evidence_kind != EvidenceKind::Document.as_str()
    {
        return Err(Error::ManualUpdateNotAllowed {
            id: memory_id.to_string(),
        });
    }
    if state != MemoryState::Active.as_str() {
        return Err(Error::MemoryNotActive {
            id: memory_id.to_string(),
            state,
        });
    }
    Ok(())
}

fn supersede_existing_memory(conn: &Connection, old_id: Uuid, new_id: Uuid, at: Ts) -> Result<()> {
    if old_id == new_id {
        return Err(Error::MemoryNotActive {
            id: old_id.to_string(),
            state: "cannot supersede itself".to_string(),
        });
    }
    let changed = conn
        .execute(
            "UPDATE memories SET state = 'superseded', superseded_by = ?1, \
             superseded_at = ?2 WHERE id = ?3 AND state = 'active'",
            rusqlite::params![new_id.to_string(), at.0, old_id.to_string()],
        )
        .map_err(dberr)?;
    if changed != 1 {
        validate_active_memory(conn, old_id)?;
        return Err(Error::Storage(format!(
            "memory {old_id} could not be superseded"
        )));
    }
    Ok(())
}

fn uuid_from_storage(id: &str) -> Result<Uuid> {
    Uuid::parse_str(id).map_err(|e| Error::Storage(format!("corrupt id '{id}': {e}")))
}

fn record_observation_row(
    conn: &Connection,
    session_id: Uuid,
    logical: Uuid,
    file: &ObservedFile,
    at: Ts,
) -> Result<()> {
    conn.execute(
        "INSERT INTO source_observations (session_id, logical_id, location, content_hash, \
             observed_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            session_id.to_string(),
            logical.to_string(),
            file.location,
            file.content_hash,
            at.0,
        ],
    )
    .map_err(dberr)?;
    Ok(())
}

fn insert_source_event(
    conn: &Connection,
    logical: Uuid,
    kind: SourceEventKind,
    version_id: Option<Uuid>,
    session_id: Uuid,
    at: Ts,
) -> Result<SourceEvent> {
    let id = Uuid::now_v7();
    let (event_kind, from_location, to_location) = match &kind {
        SourceEventKind::Moved {
            from_location,
            to_location,
        } => ("moved", from_location.clone(), Some(to_location.clone())),
        SourceEventKind::Disappeared { from_location } => {
            ("disappeared", from_location.clone(), None)
        }
    };
    conn.execute(
        "INSERT INTO source_events (id, logical_id, kind, from_location, to_location, \
             version_id, session_id, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            id.to_string(),
            logical.to_string(),
            event_kind,
            from_location,
            to_location,
            version_id.map(|v| v.to_string()),
            session_id.to_string(),
            at.0,
        ],
    )
    .map_err(dberr)?;
    Ok(SourceEvent {
        id,
        logical_id: logical,
        kind,
        version_id,
        session_id: Some(session_id),
        created_at: at,
    })
}

/// Reconciles the active memory episodes of one logical source against the
/// fragments of an observed version (docs/memory-model.md section 6).
/// Identity is per logical source, so a moved-but-unchanged source never
/// duplicates knowledge; this runs only when content changed.
fn reconcile_logical(
    conn: &Connection,
    logical: Uuid,
    file: &ObservedFile,
    evidence_ids: &[Uuid],
    at: Ts,
    producer_family: &str,
) -> Result<Vec<MemoryAction>> {
    let mut olds_by_subject: BTreeMap<String, Vec<MemorySummary>> = BTreeMap::new();
    let olds = load_summaries(
        conn,
        "",
        "m.state = 'active' AND src.logical_id = ? AND m.producer_family = ?",
        vec![
            Value::Text(logical.to_string()),
            Value::Text(producer_family.to_string()),
        ],
    )?;
    for o in olds {
        olds_by_subject
            .entry(o.subject_normalized.clone())
            .or_default()
            .push(o);
    }
    let mut news_by_subject: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, d) in file.assertions.iter().enumerate() {
        news_by_subject
            .entry(d.subject_normalized.clone())
            .or_default()
            .push(i);
    }

    // Requested spaces per fragment index, with names for the report.
    let mut requested_by_assertion: HashMap<usize, Vec<Uuid>> = HashMap::new();
    let mut space_names: BTreeMap<Uuid, String> = BTreeMap::new();
    for spec in &file.memories {
        let entry = requested_by_assertion
            .entry(spec.assertion_idx)
            .or_default();
        for sid in &spec.spaces {
            if !entry.contains(sid) {
                entry.push(*sid);
            }
            if !space_names.contains_key(sid) {
                let name: Option<String> = conn
                    .query_row(
                        "SELECT name FROM spaces WHERE id = ?1",
                        rusqlite::params![sid.to_string()],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(dberr)?;
                space_names.insert(
                    *sid,
                    name.ok_or_else(|| Error::SpaceNotFound {
                        name: sid.to_string(),
                    })?,
                );
            }
        }
    }

    let mut assertion_cache: Vec<Option<Uuid>> = vec![None; file.assertions.len()];
    let mut get_assertion = |conn: &Connection, idx: usize| -> Result<Uuid> {
        if let Some(id) = assertion_cache[idx] {
            return Ok(id);
        }
        let d = &file.assertions[idx];
        let evidence_id = evidence_ids[d.evidence_idx];
        let id = get_or_create_assertion(conn, evidence_id, d, at)?;
        assertion_cache[idx] = Some(id);
        Ok(id)
    };

    let mut actions: Vec<MemoryAction> = Vec::new();
    let mut pending_news: Vec<usize> = Vec::new();
    let mut pending_olds: Vec<MemorySummary> = Vec::new();
    let subjects: BTreeSet<String> = olds_by_subject
        .keys()
        .chain(news_by_subject.keys())
        .cloned()
        .collect();
    for subject in subjects {
        let olds: &[MemorySummary] = olds_by_subject
            .get(&subject)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let news: &[usize] = news_by_subject
            .get(&subject)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let pairs = olds.len().max(news.len());
        for i in 0..pairs {
            match (olds.get(i), news.get(i)) {
                (Some(old), Some(&ai)) => {
                    let draft = &file.assertions[ai];
                    if old.content == draft.content {
                        let requested =
                            requested_by_assertion.get(&ai).cloned().unwrap_or_default();
                        let added = add_spaces(conn, old.memory_id, &requested, at)?;
                        if added.is_empty() {
                            push_action(
                                conn,
                                &mut actions,
                                old.memory_id,
                                &draft.subject,
                                MemoryActionKind::Unchanged,
                            )?;
                        } else {
                            let names: Vec<String> =
                                added.iter().map(|id| space_names[id].clone()).collect();
                            push_action(
                                conn,
                                &mut actions,
                                old.memory_id,
                                &draft.subject,
                                MemoryActionKind::Reused {
                                    spaces_added: names,
                                },
                            )?;
                        }
                    } else {
                        supersede(
                            conn,
                            old.memory_id,
                            &draft.subject,
                            ai,
                            &mut get_assertion,
                            at,
                            &requested_by_assertion,
                            &mut actions,
                            producer_family,
                        )?;
                    }
                }
                (Some(old), None) => {
                    pending_olds.push(old.clone());
                }
                (None, Some(&ai)) => {
                    pending_news.push(ai);
                }
                (None, None) => unreachable!("pairing loop bounds"),
            }
        }
    }

    // Content that survived a rename or a move keeps its lineage.
    let mut unmatched_olds: Vec<MemorySummary> = Vec::new();
    for old in pending_olds {
        match pending_news
            .iter()
            .position(|&ai| file.assertions[ai].content == old.content)
        {
            Some(pos) => {
                let ai = pending_news.remove(pos);
                supersede(
                    conn,
                    old.memory_id,
                    &file.assertions[ai].subject,
                    ai,
                    &mut get_assertion,
                    at,
                    &requested_by_assertion,
                    &mut actions,
                    producer_family,
                )?;
            }
            None => unmatched_olds.push(old),
        }
    }
    for ai in pending_news {
        let assertion_id = get_assertion(conn, ai)?;
        let new_id = create_memory(conn, assertion_id, at, producer_family)?;
        let requested = requested_by_assertion.get(&ai).cloned().unwrap_or_default();
        add_spaces(conn, new_id, &requested, at)?;
        push_action(
            conn,
            &mut actions,
            new_id,
            &file.assertions[ai].subject,
            MemoryActionKind::Created,
        )?;
    }
    for old in unmatched_olds {
        retire_conn(
            conn,
            old.memory_id,
            at,
            "subject no longer present in source at re-ingest",
        )?;
        push_action(
            conn,
            &mut actions,
            old.memory_id,
            &old.subject,
            MemoryActionKind::Retired,
        )?;
    }

    Ok(actions)
}

fn get_or_create_assertion(
    conn: &Connection,
    evidence_id: Uuid,
    draft: &AssertionDraft,
    at: Ts,
) -> Result<Uuid> {
    let content_hash = sha256_hex(draft.content.as_bytes());
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM assertions WHERE evidence_id = ?1 AND extraction = ?2 \
             AND subject_normalized = ?3 AND content_hash = ?4",
            rusqlite::params![
                evidence_id.to_string(),
                draft.extraction,
                draft.subject_normalized,
                content_hash,
            ],
            |r| r.get(0),
        )
        .optional()
        .map_err(dberr)?;
    if let Some(id) = existing {
        return Uuid::parse_str(&id).map_err(|e| Error::Storage(format!("corrupt id '{id}': {e}")));
    }

    let id = Uuid::now_v7();
    conn.execute(
        "INSERT INTO assertions (id, kind, subject, subject_normalized, predicate, \
             content, content_hash, extraction, created_at, evidence_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            id.to_string(),
            draft.kind.as_str(),
            draft.subject,
            draft.subject_normalized,
            draft.predicate,
            draft.content,
            content_hash,
            draft.extraction,
            at.0,
            evidence_id.to_string(),
        ],
    )
    .map_err(dberr)?;
    conn.execute(
        "INSERT INTO assertion_fts (assertion_id, subject, content) VALUES (?1, ?2, ?3)",
        rusqlite::params![id.to_string(), draft.subject, draft.content],
    )
    .map_err(dberr)?;
    Ok(id)
}

/// Supersedes one active memory with a new one built from fragment `ai`.
#[allow(clippy::too_many_arguments)]
fn supersede(
    conn: &Connection,
    old_memory_id: Uuid,
    subject: &str,
    ai: usize,
    get_assertion: &mut dyn FnMut(&Connection, usize) -> Result<Uuid>,
    at: Ts,
    requested_by_assertion: &HashMap<usize, Vec<Uuid>>,
    actions: &mut Vec<MemoryAction>,
    producer_family: &str,
) -> Result<()> {
    let assertion_id = get_assertion(conn, ai)?;
    let new_id = create_memory(conn, assertion_id, at, producer_family)?;
    let requested = requested_by_assertion.get(&ai).cloned().unwrap_or_default();
    add_spaces(conn, new_id, &requested, at)?;
    conn.execute(
        "UPDATE memories SET state = 'superseded', superseded_by = ?1, \
         superseded_at = ?2 WHERE id = ?3 AND state = 'active'",
        rusqlite::params![new_id.to_string(), at.0, old_memory_id.to_string()],
    )
    .map_err(dberr)?;
    let names: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT s.name FROM memory_spaces ms JOIN spaces s ON s.id = ms.space_id \
                 WHERE ms.memory_id = ?1 ORDER BY s.name",
            )
            .map_err(dberr)?;
        let rows = stmt
            .query_map(rusqlite::params![new_id.to_string()], |r| {
                r.get::<_, String>(0)
            })
            .map_err(dberr)?;
        let mut names = Vec::new();
        for r in rows {
            names.push(r.map_err(dberr)?);
        }
        names
    };
    actions.push(MemoryAction {
        memory_id: new_id,
        subject: subject.to_string(),
        action: MemoryActionKind::Superseded { old_memory_id },
        spaces: names,
    });
    Ok(())
}

/// Adds the missing space memberships and returns the ids that were added.
fn add_spaces(conn: &Connection, memory_id: Uuid, space_ids: &[Uuid], at: Ts) -> Result<Vec<Uuid>> {
    let mut existing: HashSet<String> = {
        let mut stmt = conn
            .prepare("SELECT space_id FROM memory_spaces WHERE memory_id = ?1")
            .map_err(dberr)?;
        let rows = stmt
            .query_map(rusqlite::params![memory_id.to_string()], |r| {
                r.get::<_, String>(0)
            })
            .map_err(dberr)?;
        let mut set = HashSet::new();
        for r in rows {
            set.insert(r.map_err(dberr)?);
        }
        set
    };
    let mut added = Vec::new();
    for sid in space_ids {
        if existing.contains(&sid.to_string()) {
            continue;
        }
        conn.execute(
            "INSERT INTO memory_spaces (memory_id, space_id, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![memory_id.to_string(), sid.to_string(), at.0],
        )
        .map_err(dberr)?;
        added.push(*sid);
        existing.insert(sid.to_string());
    }
    Ok(added)
}

fn create_memory(
    conn: &Connection,
    assertion_id: Uuid,
    created_at: Ts,
    producer_family: &str,
) -> Result<Uuid> {
    let id = Uuid::now_v7();
    conn.execute(
        "INSERT INTO memories (id, assertion_id, state, created_at, producer_family) VALUES (?1, ?2, 'active', ?3, ?4)",
        rusqlite::params![id.to_string(), assertion_id.to_string(), created_at.0, producer_family],
    )
    .map_err(dberr)?;
    Ok(id)
}

/// Finds or creates the scope of a repository or an ingest root and attaches
/// it to `spaces`. Attaching is add-only and idempotent.
fn upsert_scope(
    conn: &Connection,
    repository_id: Option<Uuid>,
    root: Option<&str>,
    spaces: &[Uuid],
    at: Ts,
) -> Result<()> {
    let existing: Option<String> = match (repository_id, root) {
        (Some(repo), _) => conn
            .query_row(
                "SELECT id FROM scopes WHERE repository_id = ?1",
                rusqlite::params![repo.to_string()],
                |r| r.get(0),
            )
            .optional(),
        (None, Some(root)) => conn
            .query_row(
                "SELECT id FROM scopes WHERE root = ?1",
                rusqlite::params![root],
                |r| r.get(0),
            )
            .optional(),
        (None, None) => return Ok(()),
    }
    .map_err(dberr)?;
    let scope_id = match existing {
        Some(id) => id,
        None => {
            let id = Uuid::now_v7().to_string();
            conn.execute(
                "INSERT INTO scopes (id, kind, repository_id, root, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    id,
                    if repository_id.is_some() { "repository" } else { "directory" },
                    repository_id.map(|r| r.to_string()),
                    if repository_id.is_some() { None } else { root },
                    at.0
                ],
            )
            .map_err(dberr)?;
            id
        }
    };
    for space in spaces {
        conn.execute(
            "INSERT OR IGNORE INTO scope_spaces (scope_id, space_id, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![scope_id, space.to_string(), at.0],
        )
        .map_err(dberr)?;
    }
    Ok(())
}

fn retire_conn(conn: &Connection, id: Uuid, at: Ts, reason: &str) -> Result<()> {
    conn.execute(
        "UPDATE memories SET state = 'retired', retired_at = ?1, retire_reason = ?2 \
         WHERE id = ?3",
        rusqlite::params![at.0, reason, id.to_string()],
    )
    .map_err(dberr)?;
    Ok(())
}

/// Fills in the memory's final space names for the action report.
fn push_action(
    conn: &Connection,
    actions: &mut Vec<MemoryAction>,
    memory_id: Uuid,
    subject: &str,
    action: MemoryActionKind,
) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "SELECT s.name FROM memory_spaces ms JOIN spaces s ON s.id = ms.space_id \
             WHERE ms.memory_id = ?1 ORDER BY s.name",
        )
        .map_err(dberr)?;
    let rows = stmt
        .query_map(rusqlite::params![memory_id.to_string()], |r| {
            r.get::<_, String>(0)
        })
        .map_err(dberr)?;
    let mut spaces = Vec::new();
    for r in rows {
        spaces.push(r.map_err(dberr)?);
    }
    actions.push(MemoryAction {
        memory_id,
        subject: subject.to_string(),
        action,
        spaces,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Detail loaders
// ---------------------------------------------------------------------------

/// Executes an assembled search SQL and attaches each hit's space names.
fn run_search_sql(conn: &Connection, sql: &str, params: Vec<Value>) -> Result<Vec<SearchHitRaw>> {
    let mut stmt = conn.prepare(sql).map_err(classify_query_err)?;
    let rows = stmt
        .query_map(params_from_iter(params), |r| {
            Ok(SearchHitRaw {
                memory_id: uuid_col(r, "id")?,
                assertion_id: uuid_col(r, "assertion_id")?,
                subject: r.get("subject")?,
                content: r.get("content")?,
                snippet: r.get("snip")?,
                score: -r.get::<_, f64>("score")?,
                state: enum_col(r, "state", MemoryState::parse)?,
                created_at: ts_col(r, "created_at")?,
                extraction: r.get("extraction")?,
                space_names: Vec::new(),
                source_title: r.get("title")?,
                source_location: r.get("location")?,
            })
        })
        .map_err(classify_query_err)?;

    let mut hits = Vec::new();
    for r in rows {
        hits.push(r.map_err(classify_query_err)?);
    }

    let ids: Vec<String> = hits.iter().map(|h| h.memory_id.to_string()).collect();
    let names = space_names_by_memory(conn, &ids)?;
    for h in &mut hits {
        if let Some(names) = names.get(&h.memory_id.to_string()) {
            h.space_names = names.clone();
        }
    }
    Ok(hits)
}

fn load_assertion(conn: &Connection, id: Uuid) -> Result<Assertion> {
    conn.query_row(
        "SELECT id, kind, subject, subject_normalized, predicate, content, content_hash, \
             extraction, created_at, evidence_id \
         FROM assertions WHERE id = ?1",
        rusqlite::params![id.to_string()],
        |r| {
            Ok(Assertion {
                id: uuid_col(r, "id")?,
                kind: enum_col(r, "kind", AssertionKind::parse)?,
                subject: r.get("subject")?,
                subject_normalized: r.get("subject_normalized")?,
                predicate: r.get("predicate")?,
                content: r.get("content")?,
                content_hash: r.get("content_hash")?,
                extraction: r.get("extraction")?,
                created_at: ts_col(r, "created_at")?,
                evidence_id: uuid_col(r, "evidence_id")?,
            })
        },
    )
    .map_err(dberr)
}

fn load_evidence(conn: &Connection, id: Uuid) -> Result<Evidence> {
    conn.query_row(
        "SELECT id, source_id, kind, heading_path, start_line, end_line, ordinal, content, \
             content_hash \
         FROM evidence WHERE id = ?1",
        rusqlite::params![id.to_string()],
        |r| {
            Ok(Evidence {
                id: uuid_col(r, "id")?,
                source_id: uuid_col(r, "source_id")?,
                kind: enum_col(r, "kind", EvidenceKind::parse)?,
                heading_path: r.get("heading_path")?,
                start_line: r.get::<_, Option<i64>>("start_line")?.map(|v| v as u32),
                end_line: r.get::<_, Option<i64>>("end_line")?.map(|v| v as u32),
                ordinal: r.get::<_, i64>("ordinal")? as u32,
                content: r.get("content")?,
                content_hash: r.get("content_hash")?,
            })
        },
    )
    .map_err(dberr)
}
// ---------------------------------------------------------------------------
// Source lifecycle loaders
// ---------------------------------------------------------------------------

fn row_logical_source(row: &Row) -> rusqlite::Result<LogicalSource> {
    Ok(LogicalSource {
        id: uuid_col(row, "id")?,
        kind: enum_col(row, "kind", crate::core::LogicalKind::parse)?,
        state: enum_col(row, "state", SourcePresence::parse)?,
        current_location: row.get("current_location")?,
        created_at: ts_col(row, "created_at")?,
    })
}

fn load_source_lifecycle(conn: &Connection, logical_id: Uuid) -> Result<Option<SourceLifecycle>> {
    let logical = conn
        .query_row(
            "SELECT id, kind, state, current_location, created_at FROM logical_sources \
             WHERE id = ?1",
            rusqlite::params![logical_id.to_string()],
            row_logical_source,
        )
        .optional()
        .map_err(dberr)?;
    let Some(logical) = logical else {
        return Ok(None);
    };
    let mut stmt = conn
        .prepare(
            "SELECT id, logical_id, kind, from_location, to_location, version_id, session_id, \
                 created_at \
             FROM source_events WHERE logical_id = ?1 ORDER BY created_at, id",
        )
        .map_err(dberr)?;
    let rows = stmt
        .query_map(rusqlite::params![logical_id.to_string()], |r| {
            let kind_str: String = r.get("kind")?;
            let from: Option<String> = r.get("from_location")?;
            let to: Option<String> = r.get("to_location")?;
            let kind = match kind_str.as_str() {
                "moved" => SourceEventKind::Moved {
                    from_location: from,
                    to_location: to.expect("moved events carry a destination"),
                },
                "disappeared" => SourceEventKind::Disappeared {
                    from_location: from,
                },
                other => {
                    let _ = other;
                    return Err(rusqlite::Error::InvalidColumnType(
                        0,
                        "kind".to_string(),
                        Type::Text,
                    ));
                }
            };
            Ok(SourceEvent {
                id: uuid_col(r, "id")?,
                logical_id: uuid_col(r, "logical_id")?,
                kind,
                version_id: opt_uuid_col(r, "version_id")?,
                session_id: opt_uuid_col(r, "session_id")?,
                created_at: ts_col(r, "created_at")?,
            })
        })
        .map_err(dberr)?;
    let mut events = Vec::new();
    for r in rows {
        events.push(r.map_err(dberr)?);
    }
    Ok(Some(SourceLifecycle { logical, events }))
}

// ---------------------------------------------------------------------------
// Git observation transaction
// ---------------------------------------------------------------------------

fn record_git_observation_tx(
    conn: &Connection,
    observation: &GitObservation,
) -> Result<GitObservationReport> {
    let at = observation.session.observed_at;

    // 1. Session journal entry.
    let session_id = Uuid::now_v7();
    conn.execute(
        "INSERT INTO ingest_sessions (id, root, scope, observed_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            session_id.to_string(),
            observation.session.root,
            observation.session.scope.as_str(),
            at.0,
        ],
    )
    .map_err(dberr)?;

    // 2. Repository identity: the marker-backed id. current_path tracks the
    //    last observed location, like logical_sources.current_location.
    conn.execute(
        "INSERT INTO repositories (id, current_path, created_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT(id) DO UPDATE SET current_path = excluded.current_path",
        rusqlite::params![
            observation.repository_id.to_string(),
            observation.repository_path,
            at.0,
        ],
    )
    .map_err(dberr)?;
    upsert_scope(
        conn,
        Some(observation.repository_id),
        None,
        &observation.spaces,
        at,
    )?;
    // Remotes as observed: the latest observation is current, earlier rows
    // keep their first/last observation times.
    conn.execute(
        "UPDATE repository_remotes SET is_current = 0 WHERE repository_id = ?1",
        rusqlite::params![observation.repository_id.to_string()],
    )
    .map_err(dberr)?;
    for r in &observation.remotes {
        conn.execute(
            "INSERT INTO repository_remotes \
                 (repository_id, name, url, slug_key, slug, first_observed_at, last_observed_at, is_current) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, 1) \
             ON CONFLICT(repository_id, name, url) DO UPDATE SET \
                 slug_key = excluded.slug_key, slug = excluded.slug, \
                 last_observed_at = excluded.last_observed_at, is_current = 1",
            rusqlite::params![
                observation.repository_id.to_string(),
                r.name,
                r.url,
                r.slug_key,
                r.slug,
                at.0
            ],
        )
        .map_err(dberr)?;
    }

    // 3. Objects: immutable, keyed (repository, oid). Existing ones are
    //    reused; only new ones are written. Objects are never deleted, so
    //    unreachable objects that were observed before are retained.
    let mut existing: HashSet<String> = HashSet::new();
    for table in ["git_commits", "git_trees", "git_blobs", "git_tags"] {
        let sql = format!("SELECT oid FROM {table} WHERE repository_id = ?1");
        let mut stmt = conn.prepare(&sql).map_err(dberr)?;
        let rows = stmt
            .query_map(
                rusqlite::params![observation.repository_id.to_string()],
                |r| r.get::<_, String>(0),
            )
            .map_err(dberr)?;
        for r in rows {
            existing.insert(r.map_err(dberr)?);
        }
    }
    let all_oids: Vec<&String> = observation
        .commits
        .iter()
        .map(|c| &c.oid)
        .chain(observation.trees.iter().map(|t| &t.oid))
        .chain(observation.blobs.iter().map(|b| &b.oid))
        .chain(observation.tags.iter().map(|t| &t.oid))
        .collect();
    let mut objects_new: u64 = 0;
    let mut objects_existing: u64 = 0;
    for oid in &all_oids {
        if existing.contains(*oid) {
            objects_existing += 1;
        } else {
            objects_new += 1;
        }
    }
    let is_new = |oid: &str| !existing.contains(oid);
    for c in &observation.commits {
        if !is_new(&c.oid) {
            continue;
        }
        conn.execute(
            "INSERT INTO git_commits (repository_id, oid, tree_oid, author_name, author_email, \
                 author_time, committer_name, committer_email, commit_time, message) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                observation.repository_id.to_string(),
                c.oid,
                c.tree_oid,
                c.author_name,
                c.author_email,
                c.author_time.0,
                c.committer_name,
                c.committer_email,
                c.commit_time.0,
                c.message,
            ],
        )
        .map_err(dberr)?;
        for (position, parent) in c.parents.iter().enumerate() {
            conn.execute(
                "INSERT INTO git_commit_parents (repository_id, commit_oid, position, parent_oid) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    observation.repository_id.to_string(),
                    c.oid,
                    position as i64,
                    parent,
                ],
            )
            .map_err(dberr)?;
        }
    }
    for t in &observation.trees {
        if !is_new(&t.oid) {
            continue;
        }
        conn.execute(
            "INSERT INTO git_trees (repository_id, oid) VALUES (?1, ?2)",
            rusqlite::params![observation.repository_id.to_string(), t.oid],
        )
        .map_err(dberr)?;
        for e in &t.entries {
            conn.execute(
                "INSERT INTO git_tree_entries (repository_id, tree_oid, name, mode, target_oid, \
                     target_kind) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    observation.repository_id.to_string(),
                    t.oid,
                    e.name,
                    e.mode,
                    e.target_oid,
                    e.target_kind.as_str(),
                ],
            )
            .map_err(dberr)?;
        }
    }
    for b in &observation.blobs {
        if !is_new(&b.oid) {
            continue;
        }
        conn.execute(
            "INSERT INTO git_blobs (repository_id, oid, size_bytes, content) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                observation.repository_id.to_string(),
                b.oid,
                b.size_bytes as i64,
                b.content,
            ],
        )
        .map_err(dberr)?;
    }
    for t in &observation.tags {
        if !is_new(&t.oid) {
            continue;
        }
        conn.execute(
            "INSERT INTO git_tags (repository_id, oid, target_oid, target_kind, tagger_name, \
                 tagger_email, tag_time, message) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                observation.repository_id.to_string(),
                t.oid,
                t.target_oid,
                t.target_kind.as_str(),
                t.tagger_name,
                t.tagger_email,
                t.tag_time.map(|ts| ts.0),
                t.message,
            ],
        )
        .map_err(dberr)?;
    }

    // 4. Refs: the latest observed state, plus the journal of what changed.
    //    Ref names are observations, not identity: a deleted ref loses its
    //    current-state row (once), never its journal and never its objects.
    for r in &observation.refs {
        conn.execute(
            "INSERT INTO git_refs (repository_id, name, target_oid, target_kind, \
                 symbolic_target, last_observed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(repository_id, name) DO UPDATE SET \
                 target_oid = excluded.target_oid, \
                 target_kind = excluded.target_kind, \
                 symbolic_target = excluded.symbolic_target, \
                 last_observed_at = excluded.last_observed_at",
            rusqlite::params![
                observation.repository_id.to_string(),
                r.name,
                r.target_oid,
                r.target_kind.as_str(),
                r.symbolic_target,
                at.0,
            ],
        )
        .map_err(dberr)?;
    }
    let mut events = Vec::with_capacity(observation.ref_events.len());
    let (mut refs_added, mut refs_changed, mut refs_removed) = (0u64, 0u64, 0u64);
    for e in &observation.ref_events {
        match e.kind {
            crate::core::GitRefEventKind::Created => refs_added += 1,
            crate::core::GitRefEventKind::FastForward
            | crate::core::GitRefEventKind::NonFastForward => refs_changed += 1,
            crate::core::GitRefEventKind::Deleted => {
                refs_removed += 1;
                // The ref disappears from the current-state table, so the
                // deletion is journalled exactly once.
                conn.execute(
                    "DELETE FROM git_refs WHERE repository_id = ?1 AND name = ?2",
                    rusqlite::params![observation.repository_id.to_string(), e.name],
                )
                .map_err(dberr)?;
            }
        }
        conn.execute(
            "INSERT INTO git_ref_events (id, repository_id, ref_name, kind, from_oid, to_oid, \
                 session_id, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                Uuid::now_v7().to_string(),
                observation.repository_id.to_string(),
                e.name,
                e.kind.as_str(),
                e.from_oid,
                e.to_oid,
                session_id.to_string(),
                at.0,
            ],
        )
        .map_err(dberr)?;
        events.push(GitRefEventRecord {
            ref_name: e.name.clone(),
            kind: e.kind,
            from_oid: e.from_oid.clone(),
            to_oid: e.to_oid.clone(),
            created_at: at,
        });
    }

    // 5. Derivations: text blobs at ref tips routed through the existing
    //    spine. The logical source is scoped per (repository, commit, path):
    //    branches are not belief timelines, so content never supersedes
    //    across commits; same content reconciles to reuse.
    let mut files = Vec::with_capacity(observation.derivations.len());
    let mut derivations_new: u64 = 0;
    let mut derivations_reused: u64 = 0;
    for d in &observation.derivations {
        let logical = find_or_create_git_logical(
            conn,
            observation.repository_id,
            &d.commit_oid,
            &d.path,
            at,
        )?;
        let location = git_display_location(&d.path, &d.commit_oid);
        let observed = ObservedFile {
            location,
            content: d.content.clone(),
            content_hash: d.content_hash.clone(),
            title: d.title.clone(),
            media_type: d.media_type.clone(),
            size_bytes: d.content.len() as u64,
            evidences: d.evidences.clone(),
            assertions: d.assertions.clone(),
            memories: d.memories.clone(),
        };
        let (added, version_id, evidence_ids) =
            ensure_version(conn, logical, d.source_kind, &observed, at)?;
        if added {
            conn.execute(
                "INSERT INTO git_source_versions (source_id, repository_id, blob_oid, path, \
                     commit_oid) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    version_id.to_string(),
                    observation.repository_id.to_string(),
                    d.blob_oid,
                    d.path,
                    d.commit_oid,
                ],
            )
            .map_err(dberr)?;
            derivations_new += 1;
        } else {
            derivations_reused += 1;
        }
        let actions = reconcile_logical(
            conn,
            logical,
            &observed,
            &evidence_ids,
            at,
            GIT_PRODUCER_FAMILY,
        )?;
        files.push(GitFileReport {
            path: d.path.clone(),
            commit_oid: d.commit_oid.clone(),
            blob_oid: d.blob_oid.clone(),
            outcome: if added {
                GitFileOutcome::New
            } else {
                GitFileOutcome::Unchanged
            },
            actions,
        });
    }

    // 6. Current views: the commits in `current_commits` (HEAD, worktrees,
    //    selected refs) define the repository's current knowledge. Memories
    //    derived at any other commit are stale. They are retired (never
    //    deleted) with a reason naming the commit and the current views.
    let mut stale_retired: u64 = 0;
    if let Some(current) = &observation.current_commits {
        let current_set: std::collections::BTreeSet<&str> =
            current.iter().map(String::as_str).collect();
        let derived: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT m.id, g.commit_oid FROM memories m \
                     JOIN assertions a ON a.id = m.assertion_id \
                     JOIN evidence e ON e.id = a.evidence_id \
                     JOIN git_source_versions g ON g.source_id = e.source_id \
                     WHERE m.state = 'active' AND g.repository_id = ?1 \
                       AND g.path NOT LIKE '/refs/tags/%' \
                     ORDER BY m.created_at, m.id",
                )
                .map_err(dberr)?;
            let rows = stmt
                .query_map(
                    rusqlite::params![observation.repository_id.to_string()],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .map_err(dberr)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(dberr)?;
            rows
        };
        let short = |oid: &str| oid[..oid.len().min(12)].to_string();
        let shown: Vec<String> = current.iter().take(3).map(|c| short(c)).collect();
        let more = current.len().saturating_sub(3);
        for (id, commit) in derived {
            if current_set.contains(commit.as_str()) {
                continue;
            }
            let id = Uuid::parse_str(&id).map_err(|e| Error::Storage(e.to_string()))?;
            let reason = format!(
                "derived at {}, which is no longer a current view (current: {}{})",
                short(&commit),
                shown.join(", "),
                if more > 0 {
                    format!(" +{more}")
                } else {
                    String::new()
                }
            );
            retire_conn(conn, id, at, &reason)?;
            stale_retired += 1;
        }
    }

    // 6b. Current code views of this repository, replaced wholesale. Syntax
    //     facts are immutable, so currency is recorded here, not on them.
    conn.execute(
        "DELETE FROM git_views WHERE repository_id = ?1",
        rusqlite::params![observation.repository_id.to_string()],
    )
    .map_err(dberr)?;
    for commit in &observation.view_commits {
        conn.execute(
            "INSERT OR IGNORE INTO git_views (repository_id, commit_oid, observed_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![observation.repository_id.to_string(), commit, at.0],
        )
        .map_err(dberr)?;
    }

    // 7. Releases follow their tag refs, not the views: a deleted or moved
    //    tag retires its release memory.
    if let Some(current) = &observation.current_tags {
        let keep: std::collections::BTreeSet<(&str, &str)> = current
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let releases: Vec<(String, String, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT m.id, g.path, g.blob_oid FROM memories m \
                     JOIN assertions a ON a.id = m.assertion_id \
                     JOIN evidence e ON e.id = a.evidence_id \
                     JOIN git_source_versions g ON g.source_id = e.source_id \
                     WHERE m.state = 'active' AND g.repository_id = ?1 \
                       AND g.path LIKE '/refs/tags/%' \
                     ORDER BY m.created_at, m.id",
                )
                .map_err(dberr)?;
            let rows = stmt
                .query_map(
                    rusqlite::params![observation.repository_id.to_string()],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    },
                )
                .map_err(dberr)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(dberr)?;
            rows
        };
        for (id, path, blob) in releases {
            if keep.contains(&(path.as_str(), blob.as_str())) {
                continue;
            }
            let id = Uuid::parse_str(&id).map_err(|e| Error::Storage(e.to_string()))?;
            let name = path.trim_start_matches("/refs/tags/");
            retire_conn(conn, id, at, &format!("tag {name} was deleted or moved"))?;
            stale_retired += 1;
        }
    }

    Ok(GitObservationReport {
        session_id,
        repository_id: observation.repository_id,
        repository_path: observation.repository_path.clone(),
        refs_observed: observation.refs.len() as u64,
        refs_added,
        refs_changed,
        refs_removed,
        objects_discovered: all_oids.len() as u64,
        objects_new,
        objects_existing,
        derivations_new,
        derivations_reused,
        stale_retired,
        code: CodeObservationReport::default(),
        files,
        events,
    })
}

/// Find-or-create the logical source of one (repository, commit, path).
fn find_or_create_git_logical(
    conn: &Connection,
    repository_id: Uuid,
    commit_oid: &str,
    path: &str,
    at: Ts,
) -> Result<Uuid> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM logical_sources WHERE repository_id = ?1 AND commit_oid = ?2 \
             AND git_path = ?3",
            rusqlite::params![repository_id.to_string(), commit_oid, path],
            |r| r.get(0),
        )
        .optional()
        .map_err(dberr)?;
    if let Some(id) = existing {
        return Uuid::parse_str(&id).map_err(|e| Error::Storage(format!("corrupt id '{id}': {e}")));
    }
    let id = Uuid::now_v7();
    conn.execute(
        "INSERT INTO logical_sources (id, kind, state, current_location, created_at, \
             repository_id, commit_oid, git_path) \
         VALUES (?1, 'git_path', 'present', ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            id.to_string(),
            git_display_location(path, commit_oid),
            at.0,
            repository_id.to_string(),
            commit_oid,
            path,
        ],
    )
    .map_err(dberr)?;
    Ok(id)
}

fn git_display_location(path: &str, commit_oid: &str) -> String {
    format!("{path} @ {}", &commit_oid[..12.min(commit_oid.len())])
}

/// Git provenance of a source version, if it was derived from a git blob.
fn load_git_provenance(conn: &Connection, source_id: &Uuid) -> Result<Option<GitProvenance>> {
    let row = conn
        .query_row(
            "SELECT g.repository_id, g.blob_oid, g.path, g.commit_oid, r.current_path, \
                    c.commit_time, c.message \
             FROM git_source_versions g \
             JOIN repositories r ON r.id = g.repository_id \
             JOIN git_commits c ON c.repository_id = g.repository_id AND c.oid = g.commit_oid \
             WHERE g.source_id = ?1",
            rusqlite::params![source_id.to_string()],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, String>(6)?,
                ))
            },
        )
        .optional()
        .map_err(dberr)?;
    let Some((repository_id, blob_oid, path, commit_oid, repository_path, commit_time, message)) =
        row
    else {
        return Ok(None);
    };
    let mut stmt = conn
        .prepare(
            "SELECT name FROM git_refs WHERE repository_id = ?1 AND target_oid = ?2 ORDER BY name",
        )
        .map_err(dberr)?;
    let rows = stmt
        .query_map(rusqlite::params![repository_id, commit_oid], |r| {
            r.get::<_, String>(0)
        })
        .map_err(dberr)?;
    let mut refs = Vec::new();
    for r in rows {
        refs.push(r.map_err(dberr)?);
    }
    Ok(Some(GitProvenance {
        repository_path,
        blob_oid,
        path,
        commit_oid,
        commit_time: Ts(commit_time),
        commit_message: message,
        refs,
    }))
}

#[cfg(test)]
mod effective_derivation_tests {
    use super::EFFECTIVE_DERIVATION;

    /// The shared rule picks exactly one derivation per input, the greatest `(created_at, id)`,
    /// also when timestamps are equal; every reader that uses the constant agrees by construction.
    #[test]
    fn exactly_one_derivation_is_effective_per_input_even_with_equal_timestamps() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_derivations (id TEXT PRIMARY KEY, input_id TEXT NOT NULL, created_at INTEGER NOT NULL);").unwrap();
        for (id, input, at) in [
            ("a", "i1", 5),
            ("b", "i1", 5), // tie with a: the greater id wins
            ("c", "i1", 4),
            ("d", "i2", 1),
            ("e", "i2", 9),
            ("f", "i3", 7), // a single derivation
            ("g", "i4", 3),
            ("h", "i4", 3),
            ("i", "i4", 3), // three-way tie
        ] {
            c.execute(
                "INSERT INTO code_derivations VALUES (?1,?2,?3)",
                rusqlite::params![id, input, at],
            )
            .unwrap();
        }
        let sql = format!("SELECT d.input_id, d.id FROM code_derivations d WHERE {EFFECTIVE_DERIVATION} ORDER BY d.input_id");
        let rows: Vec<(String, String)> = c
            .prepare(&sql)
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .flatten()
            .collect();
        let want: Vec<(String, String)> = [("i1", "b"), ("i2", "e"), ("i3", "f"), ("i4", "i")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        assert_eq!(rows, want);
        // the old rule (MAX(created_at)) returns both rows of a tie
        let old: i64 = c
            .query_row("SELECT COUNT(*) FROM code_derivations d WHERE d.input_id='i1' AND d.created_at=(SELECT MAX(d2.created_at) FROM code_derivations d2 WHERE d2.input_id=d.input_id)", [], |r| r.get(0))
            .unwrap();
        assert_eq!(old, 2);
    }
}

#[cfg(test)]
mod commit_failure_tests {
    use super::*;

    fn failure(code: i32) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
    }

    #[test]
    fn only_failures_that_sqlite_defines_as_not_committed_are_rolled_back() {
        // not committed by SQLite's contract: the ordinary errors
        for code in [
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_LOCKED,
            rusqlite::ffi::SQLITE_CONSTRAINT,
        ] {
            assert!(
                !matches!(
                    commit_failure(failure(code)),
                    Error::CommitOutcomeUnknown { .. }
                ),
                "code {code} is a definitive rollback"
            );
        }
        // the log may already hold the commit record: unknown
        for code in [
            rusqlite::ffi::SQLITE_IOERR,
            rusqlite::ffi::SQLITE_FULL,
            rusqlite::ffi::SQLITE_CORRUPT,
            rusqlite::ffi::SQLITE_NOTADB,
            rusqlite::ffi::SQLITE_READONLY,
            rusqlite::ffi::SQLITE_INTERRUPT,
        ] {
            assert!(
                matches!(
                    commit_failure(failure(code)),
                    Error::CommitOutcomeUnknown { .. }
                ),
                "code {code} must be reported as an unknown outcome"
            );
        }
        // a non-SQLite error is not a definitive rollback either
        assert!(matches!(
            commit_failure(rusqlite::Error::InvalidQuery),
            Error::CommitOutcomeUnknown { .. }
        ));
    }
}

impl SqliteStore {
    /// Lists immutable chunk sets of this extraction, ordered by version, budget and source offset.
    pub fn list_document_chunks(
        &self,
        extraction_id: Uuid,
    ) -> Result<Vec<crate::storage::StoredDocumentChunk>> {
        documents::list_chunks(self, extraction_id)
    }
}

#[cfg(test)]
#[path = "sqlite/ownership_migration_tests.rs"]
mod ownership_migration_tests;

#[cfg(test)]
#[path = "sqlite/document_preparation_migration_tests.rs"]
mod document_preparation_migration_tests;

#[cfg(test)]
#[path = "sqlite/lexical_staging_migration_tests.rs"]
mod lexical_staging_migration_tests;

#[cfg(test)]
#[path = "sqlite/semantic_staging_migration_tests.rs"]
mod semantic_staging_migration_tests;

#[cfg(test)]
#[path = "sqlite/fence_plan_migration_tests.rs"]
mod fence_plan_migration_tests;

#[cfg(test)]
#[path = "sqlite/settings_tests.rs"]
mod settings_tests;

#[cfg(test)]
#[path = "sqlite/read_session_tests.rs"]
mod read_session_tests;

#[cfg(test)]
#[path = "sqlite/observation_planning_tests.rs"]
mod observation_planning_tests;
