use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use rusqlite::types::{Type, Value};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use uuid::Uuid;

use crate::core::{
    cosine_similarity, decode_f32_le, encode_f32_le, sha256_hex, Assertion, AssertionDraft,
    AssertionEmbedding, AssertionKind, CodeDerivation, CodeEntity, CodeImport, CodeObservation,
    CodeObservationReport, ConflictGroup, EmbeddingIndexStatus, Error, Evidence, EvidenceDraft,
    EvidenceKind, FileObservation, FileOutcome, GitFileOutcome, GitFileReport, GitObservation,
    GitObservationReport, GitProvenance, GitRefEventRecord, GitRefObservation, IngestObservation,
    IngestScope, LogicalKind, LogicalSource, ManualMemoryDraft, ManualMemoryReport, Memory,
    MemoryAction, MemoryActionKind, MemoryDetail, MemoryState, MemorySummary, ObservationReport,
    ObservedFile, Result, SearchHitRaw, SearchQuery, Source, SourceEvent, SourceEventKind,
    SourceKind, SourceLifecycle, SourcePresence, Space, SpaceLink, Ts, UnembeddedAssertion,
    EXTRACTION_MANUAL_WRITE,
};
use crate::storage::Store;

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
];

const MANUAL_SOURCE_LOCATION: &str = "manual://writes";

pub struct SqliteStore {
    conn: Connection,
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
        conn.busy_timeout(Duration::from_secs(5)).map_err(dberr)?;
        conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
            .map_err(dberr)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(dberr)?;
        run_migrations(&conn)?;
        Ok(Self { conn })
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
    tx: &rusqlite::Transaction<'_>,
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
    let mut stmt = tx.prepare("SELECT DISTINCT c.path,e.id,e.kind,e.syntax_path,e.qualified_name FROM code_contexts c JOIN code_inputs i ON i.id=c.input_id JOIN code_derivations d ON d.input_id=i.id JOIN code_entities e ON e.derivation_id=d.id WHERE i.repository_id=?1 AND c.commit_oid=?2 ORDER BY c.path,e.syntax_path,e.id").map_err(dberr)?;
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

fn begin_write(conn: &Connection) -> Result<Transaction<'_>> {
    Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(dberr)
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

fn run_migrations(conn: &Connection) -> Result<()> {
    // Migrations run with foreign keys disabled: table rebuilds (the
    // documented SQLite way to change constraints) drop referenced tables.
    // The setting is a no-op inside a transaction, so it is toggled at the
    // connection level around the migration batch and restored after.
    conn.pragma_update(None, "foreign_keys", "OFF")
        .map_err(dberr)?;
    let has_migration_table: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
            [],
            |_| Ok(()),
        )
        .optional()
        .map_err(dberr)?
        .is_some();
    if !has_migration_table {
        let tx = begin_write(conn)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                 version    INTEGER PRIMARY KEY,
                 applied_at INTEGER NOT NULL
             )",
        )
        .map_err(dberr)?;
        tx.commit().map_err(dberr)?;
    }

    let mut applied: HashSet<i64> = HashSet::new();
    {
        let mut stmt = conn
            .prepare("SELECT version FROM schema_migrations")
            .map_err(dberr)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(dberr)?;
        for version in rows {
            applied.insert(version.map_err(dberr)?);
        }
    }

    for (version, sql) in MIGRATIONS {
        if applied.contains(version) {
            continue;
        }
        let tx = begin_write(conn)?;
        let already_applied: bool = tx
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE version = ?1",
                rusqlite::params![version],
                |_| Ok(()),
            )
            .optional()
            .map_err(dberr)?
            .is_some();
        if already_applied {
            tx.commit().map_err(dberr)?;
            applied.insert(*version);
            continue;
        }
        tx.execute_batch(sql).map_err(dberr)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            rusqlite::params![version, Ts::now().0],
        )
        .map_err(dberr)?;
        tx.commit().map_err(dberr)?;
        applied.insert(*version);
    }
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(dberr)?;
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

// ---------------------------------------------------------------------------
// Store implementation
// ---------------------------------------------------------------------------

impl Store for SqliteStore {
    fn create_space(&self, name: &str, description: &str) -> Result<Space> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::EmptyName);
        }
        let tx = begin_write(&self.conn)?;
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
        tx.commit().map_err(dberr)?;
        Ok(Space {
            id,
            name: name.to_string(),
            description: description.to_string(),
            created_at,
        })
    }

    fn get_space(&self, name: &str) -> Result<Space> {
        self.conn
            .query_row(
                "SELECT id, name, description, created_at FROM spaces WHERE name = ?1",
                rusqlite::params![name],
                row_space,
            )
            .optional()
            .map_err(dberr)?
            .ok_or_else(|| Error::SpaceNotFound {
                name: name.to_string(),
            })
    }

    fn list_spaces(&self) -> Result<Vec<Space>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, description, created_at FROM spaces ORDER BY name")
            .map_err(dberr)?;
        let rows = stmt.query_map([], row_space).map_err(dberr)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(dberr)?);
        }
        Ok(out)
    }

    fn list_spaces_with_counts(&self) -> Result<Vec<(Space, u64)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT s.id, s.name, s.description, s.created_at,
                        (SELECT COUNT(*) FROM memory_spaces ms
                         JOIN memories m ON m.id = ms.memory_id
                         WHERE ms.space_id = s.id AND m.state = 'active') AS n
                 FROM spaces s ORDER BY s.name",
            )
            .map_err(dberr)?;
        let rows = stmt
            .query_map([], |r| Ok((row_space(r)?, r.get::<_, i64>("n")? as u64)))
            .map_err(dberr)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(dberr)?);
        }
        Ok(out)
    }

    fn list_space_links(&self) -> Result<Vec<SpaceLink>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT from_space, to_space, created_at FROM space_links \
                 ORDER BY created_at, from_space, to_space",
            )
            .map_err(dberr)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(SpaceLink {
                    from_space: uuid_col(r, "from_space")?,
                    to_space: uuid_col(r, "to_space")?,
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

    fn link_spaces(&self, from: Uuid, to: Uuid) -> Result<SpaceLink> {
        let tx = begin_write(&self.conn)?;
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
        tx.commit().map_err(dberr)?;
        Ok(SpaceLink {
            from_space: from,
            to_space: to,
            created_at,
        })
    }

    fn record_observation(&self, observation: IngestObservation) -> Result<ObservationReport> {
        let tx = begin_write(&self.conn)?;
        let report = record_observation_tx(&tx, observation)?;
        tx.commit().map_err(dberr)?;
        Ok(report)
    }

    fn get_source_lifecycle(&self, logical_id: Uuid) -> Result<Option<SourceLifecycle>> {
        load_source_lifecycle(&self.conn, logical_id)
    }

    fn get_memory_detail(&self, id: Uuid) -> Result<Option<MemoryDetail>> {
        let memory = self
            .conn
            .query_row(
                "SELECT id, assertion_id, state, created_at, superseded_by, superseded_at, \
                        retired_at, retire_reason \
                 FROM memories WHERE id = ?1",
                rusqlite::params![id.to_string()],
                row_memory,
            )
            .optional()
            .map_err(dberr)?;
        let Some(memory) = memory else {
            return Ok(None);
        };
        let assertion = load_assertion(&self.conn, memory.assertion_id)?;
        let evidence = load_evidence(&self.conn, assertion.evidence_id)?;
        let source = self
            .conn
            .query_row(
                "SELECT id, logical_id, kind, location, title, content, content_hash, \
                        media_type, size_bytes, ingested_at \
                 FROM sources WHERE id = ?1",
                rusqlite::params![evidence.source_id.to_string()],
                row_source,
            )
            .map_err(dberr)?;
        let source_lifecycle = load_source_lifecycle(&self.conn, source.logical_id)?
            .ok_or_else(|| Error::Storage("source without logical identity".to_string()))?;
        let mut spaces_stmt = self
            .conn
            .prepare(
                "SELECT s.id, s.name, s.description, s.created_at \
                 FROM memory_spaces ms JOIN spaces s ON s.id = ms.space_id \
                 WHERE ms.memory_id = ?1 ORDER BY s.name",
            )
            .map_err(dberr)?;
        let space_rows = spaces_stmt
            .query_map(rusqlite::params![memory.id.to_string()], row_space)
            .map_err(dberr)?;
        let mut spaces = Vec::new();
        for r in space_rows {
            spaces.push(r.map_err(dberr)?);
        }
        let superseded_by = match memory.superseded_by {
            Some(sb) => load_summaries(
                &self.conn,
                "",
                "m.id = ?",
                vec![Value::Text(sb.to_string())],
            )?
            .into_iter()
            .next(),
            None => None,
        };
        let supersedes = load_summaries(
            &self.conn,
            "",
            "m.superseded_by = ?",
            vec![Value::Text(memory.id.to_string())],
        )?;
        let git = load_git_provenance(&self.conn, &source.id)?;
        Ok(Some(MemoryDetail {
            memory,
            assertion,
            evidence,
            source,
            source_lifecycle,
            spaces,
            superseded_by,
            supersedes,
            git,
        }))
    }

    fn record_git_observation(
        &self,
        observation: crate::core::GitObservation,
    ) -> Result<GitObservationReport> {
        let tx = begin_write(&self.conn)?;
        let report = record_git_observation_tx(&tx, &observation)?;
        tx.commit().map_err(dberr)?;
        Ok(report)
    }

    fn git_refs_current(&self, repository_id: Uuid) -> Result<Vec<GitRefObservation>> {
        let mut stmt = self
            .conn
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
        let tx = begin_write(&self.conn)?;
        let mut report = CodeObservationReport::default();
        for input in observation.inputs {
            report.supported_inputs += 1;
            let existing:Option<String>=match (&input.repository_id,&input.blob_oid) { (Some(repo),Some(blob))=>tx.query_row("SELECT id FROM code_inputs WHERE repository_id=?1 AND blob_oid=?2",rusqlite::params![repo.to_string(),blob],|r|r.get(0)).optional().map_err(dberr)?, _=>tx.query_row("SELECT id FROM code_inputs WHERE repository_id IS NULL AND content_hash=?1",rusqlite::params![input.content_hash],|r|r.get(0)).optional().map_err(dberr)? };
            let is_new = existing.is_none();
            let input_id = existing.unwrap_or_else(|| Uuid::now_v7().to_string());
            if is_new {
                tx.execute("INSERT INTO code_inputs(id,content_hash,repository_id,blob_oid,created_at) VALUES(?1,?2,?3,?4,?5)",rusqlite::params![input_id,input.content_hash,input.repository_id.map(|x|x.to_string()),input.blob_oid,Ts::now().0]).map_err(dberr)?;
            }
            for context in input.contexts {
                tx.execute("INSERT OR IGNORE INTO code_contexts(input_id,path,commit_oid) VALUES(?1,?2,?3)",rusqlite::params![input_id,context.path,context.commit_oid]).map_err(dberr)?;
            }
            let d = &input.derivation;
            let derivation_id:Option<String>=tx.query_row("SELECT id FROM code_derivations WHERE input_id=?1 AND extractor=?2 AND extractor_version=?3 AND grammar=?4 AND grammar_version=?5",rusqlite::params![input_id,d.identity.extractor,d.identity.extractor_version,d.identity.grammar,d.identity.grammar_version],|r|r.get(0)).optional().map_err(dberr)?;
            if derivation_id.is_some() {
                report.reused += 1;
                continue;
            }
            let did = Uuid::now_v7();
            tx.execute("INSERT INTO code_derivations(id,input_id,language,extractor,extractor_version,grammar,grammar_version,status,error_count,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",rusqlite::params![did.to_string(),input_id,input.language.as_str(),d.identity.extractor,d.identity.extractor_version,d.identity.grammar,d.identity.grammar_version,d.status.as_str(),d.error_count,Ts::now().0]).map_err(dberr)?;
            report.parsed += 1;
            report.entities += d.entities.len() as u64;
            report.parse_errors += u64::from(d.status != crate::core::CodeParseStatus::Complete);
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
        tx.commit().map_err(dberr)?;
        Ok(report)
    }

    fn code_structure(&self, target: &str) -> Result<Vec<CodeDerivation>> {
        let mut stmt=self.conn.prepare("SELECT DISTINCT d.id,d.language,d.extractor,d.extractor_version,d.grammar,d.grammar_version,d.status,d.error_count,i.content_hash FROM code_derivations d JOIN code_inputs i ON i.id=d.input_id JOIN code_contexts c ON c.input_id=i.id WHERE c.path=?1 OR i.blob_oid=?1 ORDER BY d.created_at").map_err(dberr)?;
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
            ) = row.map_err(dberr)?;
            let mut es=self.conn.prepare("SELECT id,kind,name,qualified_name,syntax_path,parent_syntax_path,start_byte,end_byte,start_line,start_column,end_line,end_column,signature,visibility,is_async FROM code_entities WHERE derivation_id=?1 ORDER BY start_byte").map_err(dberr)?;
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
            let mut is=self.conn.prepare("SELECT raw,target,alias,start_byte,end_byte,start_line,start_column,end_line,end_column FROM code_imports WHERE derivation_id=?1 ORDER BY start_byte").map_err(dberr)?;
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
            out.push(CodeDerivation {
                id,
                language,
                identity: crate::core::CodeParserIdentity {
                    extractor,
                    extractor_version,
                    grammar,
                    grammar_version,
                },
                status,
                error_count,
                content_hash,
                entities,
                imports,
            });
        }
        Ok(out)
    }

    fn has_code_derivation(
        &self,
        _language: crate::core::CodeLanguage,
        content_hash: &str,
        repository_id: Option<Uuid>,
        blob_oid: Option<&str>,
        identity: &crate::core::CodeParserIdentity,
    ) -> Result<bool> {
        let found:Option<i64>=match (repository_id,blob_oid) { (Some(repo),Some(blob))=>self.conn.query_row("SELECT 1 FROM code_derivations d JOIN code_inputs i ON i.id=d.input_id WHERE i.repository_id=?1 AND i.blob_oid=?2 AND d.extractor=?3 AND d.extractor_version=?4 AND d.grammar=?5 AND d.grammar_version=?6",rusqlite::params![repo.to_string(),blob,identity.extractor,identity.extractor_version,identity.grammar,identity.grammar_version],|r|r.get(0)).optional().map_err(dberr)?, _=>self.conn.query_row("SELECT 1 FROM code_derivations d JOIN code_inputs i ON i.id=d.input_id WHERE i.repository_id IS NULL AND i.content_hash=?1 AND d.extractor=?2 AND d.extractor_version=?3 AND d.grammar=?4 AND d.grammar_version=?5",rusqlite::params![content_hash,identity.extractor,identity.extractor_version,identity.grammar,identity.grammar_version],|r|r.get(0)).optional().map_err(dberr)?};
        Ok(found.is_some())
    }

    fn resolve_git_code_relationships(
        &self,
        repository_id: Uuid,
        commit_oid: &str,
        resolver: &str,
        version: &str,
    ) -> Result<crate::core::CodeResolutionReport> {
        let tx = begin_write(&self.conn)?;
        let existing:Option<String>=tx.query_row("SELECT id FROM code_resolution_derivations WHERE repository_id=?1 AND commit_oid=?2 AND resolver=?3 AND resolver_version=?4",rusqlite::params![repository_id.to_string(),commit_oid,resolver,version],|r|r.get(0)).optional().map_err(dberr)?;
        if existing.is_some() {
            tx.commit().map_err(dberr)?;
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
        let module_index = rust_module_index(&tx, repository_id, commit_oid)?;
        let mut stmt=tx.prepare("SELECT DISTINCT o.id,o.kind,o.raw_target,o.source_syntax_path,c.path FROM code_relationship_observations o JOIN code_derivations d ON d.id=o.derivation_id JOIN code_inputs i ON i.id=d.input_id JOIN code_contexts c ON c.input_id=i.id WHERE i.repository_id=?1 AND c.commit_oid=?2 ORDER BY o.id,c.path").map_err(dberr)?;
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
                    tx.execute("INSERT INTO code_resolution_candidates(resolution_id,entity_id) VALUES(?1,?2)",rusqlite::params![rid.to_string(),c]).map_err(dberr)?;
                }
                report.ambiguous += 1
            } else if state == "resolved" {
                report.resolved += 1
            } else {
                report.unresolved += 1
            }
        }
        tx.commit().map_err(dberr)?;
        Ok(report)
    }

    fn record_manual_write(&self, draft: ManualMemoryDraft) -> Result<ManualMemoryReport> {
        let tx = begin_write(&self.conn)?;
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
                "SELECT id FROM memories WHERE assertion_id = ?1 AND state = 'active'",
                rusqlite::params![assertion_id.to_string()],
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
                let new_id = create_memory(&tx, assertion_id, at)?;
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
                let new_id = create_memory(&tx, assertion_id, at)?;
                add_spaces(&tx, new_id, &spaces, at)?;
                (new_id, MemoryActionKind::Created)
            }
        };
        let space_names = memory_space_names(&tx, memory_id)?;
        tx.commit().map_err(dberr)?;
        Ok(ManualMemoryReport {
            memory_id,
            assertion_id,
            subject: persisted_subject,
            action,
            spaces: space_names,
        })
    }

    fn add_memory_spaces(&self, memory_id: Uuid, space_ids: Vec<Uuid>) -> Result<Vec<String>> {
        let tx = begin_write(&self.conn)?;
        validate_active_memory(&tx, memory_id)?;
        let spaces = validate_space_ids(&tx, &space_ids)?;
        let added = add_spaces(&tx, memory_id, &spaces, Ts::now())?;
        let names = names_for_space_ids(&tx, &added)?;
        tx.commit().map_err(dberr)?;
        Ok(names)
    }

    // -- Phase 2 M1: embeddings / vector index -----------------------------

    fn save_embeddings(&self, embeddings: &[AssertionEmbedding]) -> Result<usize> {
        if embeddings.is_empty() {
            return Ok(0);
        }
        let tx = begin_write(&self.conn)?;
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
        tx.commit().map_err(dberr)?;
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
        let mut stmt = self.conn.prepare(&sql).map_err(dberr)?;
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
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM assertions", [], |r| r.get(0))
            .map_err(dberr)?;
        let embedded: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM assertion_embeddings WHERE derivation_key = ?1",
                rusqlite::params![derivation_key],
                |r| r.get(0),
            )
            .map_err(dberr)?;
        Ok(EmbeddingIndexStatus {
            derivation_key: derivation_key.to_string(),
            total_assertions: total as u64,
            embedded: embedded as u64,
        })
    }

    fn clear_embeddings(&self, derivation_key: &str) -> Result<usize> {
        let tx = begin_write(&self.conn)?;
        let n = tx
            .execute(
                "DELETE FROM assertion_embeddings WHERE derivation_key = ?1",
                rusqlite::params![derivation_key],
            )
            .map_err(dberr)?;
        tx.commit().map_err(dberr)?;
        Ok(n)
    }

    fn get_meta(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM mind_meta WHERE key = ?1",
                rusqlite::params![key],
                |r| r.get(0),
            )
            .optional()
            .map_err(dberr)
    }

    fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        let tx = begin_write(&self.conn)?;
        tx.execute(
            "INSERT INTO mind_meta (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )
        .map_err(dberr)?;
        tx.commit().map_err(dberr)?;
        Ok(())
    }

    fn search_vectors(
        &self,
        query_vector: &[f32],
        query: &SearchQuery,
        derivation_key: &str,
    ) -> Result<Vec<SearchHitRaw>> {
        if query_vector.is_empty() {
            return Err(Error::InvalidVector {
                detail: "query vector is empty".to_string(),
            });
        }
        for (i, v) in query_vector.iter().enumerate() {
            if !v.is_finite() {
                return Err(Error::InvalidVector {
                    detail: format!("non-finite value at index {i}: {v}"),
                });
            }
        }
        // Empty space list means "matches nothing", same as lexical search.
        if let Some(ids) = &query.space_ids {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
        }
        let dim = query_vector.len();
        let mut params: Vec<Value> = Vec::new();
        params.push(Value::Text(derivation_key.to_string()));
        params.push(Value::Integer(dim as i64));
        let filter_sql = build_search_filters(query, &mut params);
        // All filter-qualified candidates are evaluated before ranking (F1).
        // query.limit is applied only after ranking; no silent candidate cap.
        let mut sql = String::from(
            "SELECT m.id, a.subject, a.content, ae.embedding, ae.dimension, \
                    m.state, m.created_at, a.extraction, src.title, src.location \
             FROM assertion_embeddings ae \
             JOIN assertions a ON a.id = ae.assertion_id \
             JOIN memories m ON m.assertion_id = a.id \
             JOIN evidence e ON e.id = a.evidence_id \
             JOIN sources src ON src.id = e.source_id \
             WHERE ae.derivation_key = ?1 AND ae.dimension = ?2",
        );
        sql.push_str(&filter_sql);

        // Fetch candidates (no ORDER BY in SQL; ranking happens in Rust).
        let mut stmt = self.conn.prepare(&sql).map_err(dberr)?;
        let rows = stmt
            .query_map(params_from_iter(params), |r| {
                let memory_id: String = r.get("id")?;
                let embedding_bytes: Vec<u8> = r.get("embedding")?;
                let stored_dim: i64 = r.get("dimension")?;
                Ok((
                    SearchHitRaw {
                        memory_id: Uuid::parse_str(&memory_id).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(0, Type::Text, Box::new(e))
                        })?,
                        subject: r.get("subject")?,
                        content: r.get("content")?,
                        snippet: String::new(),
                        score: 0.0,
                        state: enum_col(r, "state", MemoryState::parse)?,
                        created_at: ts_col(r, "created_at")?,
                        extraction: r.get("extraction")?,
                        space_names: Vec::new(),
                        source_title: r.get("title")?,
                        source_location: r.get("location")?,
                    },
                    embedding_bytes,
                    stored_dim,
                ))
            })
            .map_err(dberr)?;

        let mut candidates: Vec<(f64, SearchHitRaw)> = Vec::new();
        for row in rows {
            let (mut hit, bytes, stored_dim) = row.map_err(dberr)?;
            // Dimension mismatch or malformed blob: exclude with no panic.
            let stored = match decode_f32_le(&bytes, Some(stored_dim as usize)) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if stored.len() != dim {
                continue;
            }
            let sim = cosine_similarity(&stored, query_vector);
            // Deterministic content-window snippet for non-lexical hits.
            hit.snippet = content_window(&hit.content);
            hit.score = sim;
            candidates.push((sim, hit));
        }

        // Deterministic ordering: score desc, then memory_id asc.
        candidates.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.memory_id.cmp(&b.1.memory_id))
        });
        let mut hits: Vec<SearchHitRaw> = candidates
            .into_iter()
            .take(query.limit as usize)
            .map(|(_, h)| h)
            .collect();

        // Fill space_names post-hoc, identical to lexical search.
        let ids: Vec<String> = hits.iter().map(|h| h.memory_id.to_string()).collect();
        let names = space_names_by_memory(&self.conn, &ids)?;
        for h in &mut hits {
            if let Some(ns) = names.get(&h.memory_id.to_string()) {
                h.space_names = ns.clone();
            }
        }
        // Assign rank is deferred to retrieval (M4); store returns raw hits.
        Ok(hits)
    }

    fn list_memories_by_subject(&self, subject_normalized: &str) -> Result<Vec<MemorySummary>> {
        load_summaries(
            &self.conn,
            "",
            "a.subject_normalized = ?1",
            vec![Value::Text(subject_normalized.to_string())],
        )
    }

    fn list_replaced_by(&self, id: Uuid) -> Result<Vec<MemorySummary>> {
        load_summaries(
            &self.conn,
            "",
            "m.superseded_by = ?1",
            vec![Value::Text(id.to_string())],
        )
    }

    fn memory_id_prefix_matches(&self, prefix: &str) -> Result<Vec<Uuid>> {
        let lowered: String = prefix
            .to_lowercase()
            .chars()
            .filter(|c| *c != '-')
            .collect();
        if lowered.is_empty() {
            return Ok(Vec::new());
        }
        let pattern = format!("{lowered}%");
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM memories WHERE REPLACE(id, '-', '') LIKE ?1 ORDER BY id")
            .map_err(dberr)?;
        let rows = stmt
            .query_map(rusqlite::params![pattern], |r| r.get::<_, String>(0))
            .map_err(dberr)?;
        let mut out = Vec::new();
        for r in rows {
            let s = r.map_err(dberr)?;
            if s.replace('-', "").starts_with(&lowered) {
                out.push(
                    Uuid::parse_str(&s)
                        .map_err(|e| Error::Storage(format!("corrupt id '{s}': {e}")))?,
                );
            }
        }
        Ok(out)
    }

    fn search(&self, query: &SearchQuery) -> Result<Vec<SearchHitRaw>> {
        let text = query.text.trim();
        if text.is_empty() {
            return Err(Error::EmptyQuery);
        }

        let mut sql = String::from(
            "SELECT m.id, a.subject, a.content, \
                    snippet(assertion_fts, 2, '[', ']', '…', 12) AS snip, \
                    bm25(assertion_fts) AS score, m.state, m.created_at, a.extraction, \
                    src.title, src.location \
             FROM assertion_fts \
             JOIN assertions a ON a.id = assertion_fts.assertion_id \
             JOIN memories m ON m.assertion_id = a.id \
             JOIN evidence e ON e.id = a.evidence_id \
             JOIN sources src ON src.id = e.source_id \
             WHERE assertion_fts MATCH ?1",
        );
        let mut params: Vec<Value> = vec![Value::Text(text.to_string())];

        // Empty space list means "matches nothing", same as before.
        if let Some(ids) = &query.space_ids {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
        }

        let filter_sql = build_search_filters(query, &mut params);
        sql.push_str(&filter_sql);

        // Deterministic tie-break: score ASC (bm25, best first), then memory id.
        sql.push_str(&format!(
            " ORDER BY score ASC, m.id ASC LIMIT ?{}",
            params.len() + 1
        ));
        params.push(Value::Integer(i64::from(query.limit)));

        // Deterministic two-step: try the text as an FTS5 match expression;
        // on a syntax error retry it as a literal quoted phrase, so plain
        // text containing punctuation ("O(1)") still searches.
        let run = |match_text: &str| -> Result<Vec<SearchHitRaw>> {
            let mut p = params.clone();
            p[0] = Value::Text(match_text.to_string());
            run_search_sql(&self.conn, &sql, p)
        };
        match run(text) {
            Err(Error::QuerySyntax(_)) => run(&format!("\"{}\"", text.replace('"', "\"\""))),
            other => other,
        }
    }

    fn list_conflicts(&self) -> Result<Vec<ConflictGroup>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT s.id, s.name, a.subject_normalized \
                 FROM memory_spaces ms \
                 JOIN memories m ON m.id = ms.memory_id \
                 JOIN assertions a ON a.id = m.assertion_id \
                 JOIN spaces s ON s.id = ms.space_id \
                 WHERE m.state = 'active' \
                 GROUP BY s.id, a.subject_normalized \
                 HAVING COUNT(*) > 1 \
                 ORDER BY s.name, a.subject_normalized",
            )
            .map_err(dberr)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    uuid_col(r, "id")?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(dberr)?;
        let mut groups = Vec::new();
        for r in rows {
            let (space_id, space_name, subject) = r.map_err(dberr)?;
            let members = load_summaries(
                &self.conn,
                "JOIN memory_spaces fms ON fms.memory_id = m.id",
                "m.state = 'active' AND fms.space_id = ? AND a.subject_normalized = ?",
                vec![
                    Value::Text(space_id.to_string()),
                    Value::Text(subject.clone()),
                ],
            )?;
            groups.push(ConflictGroup {
                space_name,
                subject,
                members,
            });
        }
        Ok(groups)
    }

    fn retire_memory(&self, id: Uuid, reason: Option<String>, at: Ts) -> Result<Memory> {
        let tx = begin_write(&self.conn)?;
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
        tx.commit().map_err(dberr)?;
        Ok(memory)
    }
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

/// What the engine believes about one logical source inside an ingest
/// root: its expected location and the hash of its latest version.
type KnownSource = (String, Option<String>);

fn record_observation_tx(
    conn: &Connection,
    observation: IngestObservation,
) -> Result<ObservationReport> {
    let IngestObservation { session, mut files } = observation;
    let at = session.observed_at;
    let full = session.scope == IngestScope::Directory;
    let root = session.root.clone();

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
                    latest_version_hash(conn, logical).ok().flatten()
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

    // --- execution -----------------------------------------------------------
    let mut report_files: Vec<FileObservation> = Vec::new();
    let mut events: Vec<SourceEvent> = Vec::new();

    for plan in plans {
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
                let actions =
                    reconcile_logical(conn, logical, &files[file_idx], &evidence_ids, at)?;
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
                let actions =
                    reconcile_logical(conn, logical, &files[file_idx], &evidence_ids, at)?;
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
                let actions =
                    reconcile_logical(conn, logical, &files[file_idx], &evidence_ids, at)?;
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
    if full {
        for (logical, location) in &gone {
            if matched.contains_key(logical) {
                continue;
            }
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
    }

    Ok(ObservationReport {
        session_id,
        root,
        scope: session.scope,
        files: report_files,
        events,
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
        "SELECT id FROM logical_sources WHERE current_location = ?1 AND state = '{state}' \
         ORDER BY created_at DESC, id DESC LIMIT 1"
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
) -> Result<Vec<MemoryAction>> {
    let mut olds_by_subject: BTreeMap<String, Vec<MemorySummary>> = BTreeMap::new();
    let olds = load_summaries(
        conn,
        "",
        "m.state = 'active' AND src.logical_id = ?",
        vec![Value::Text(logical.to_string())],
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
                )?;
            }
            None => unmatched_olds.push(old),
        }
    }
    for ai in pending_news {
        let assertion_id = get_assertion(conn, ai)?;
        let new_id = create_memory(conn, assertion_id, at)?;
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
) -> Result<()> {
    let assertion_id = get_assertion(conn, ai)?;
    let new_id = create_memory(conn, assertion_id, at)?;
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

fn create_memory(conn: &Connection, assertion_id: Uuid, created_at: Ts) -> Result<Uuid> {
    let id = Uuid::now_v7();
    conn.execute(
        "INSERT INTO memories (id, assertion_id, state, created_at) VALUES (?1, ?2, 'active', ?3)",
        rusqlite::params![id.to_string(), assertion_id.to_string(), created_at.0],
    )
    .map_err(dberr)?;
    Ok(id)
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
            ensure_version(conn, logical, SourceKind::GitBlob, &observed, at)?;
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
        let actions = reconcile_logical(conn, logical, &observed, &evidence_ids, at)?;
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
