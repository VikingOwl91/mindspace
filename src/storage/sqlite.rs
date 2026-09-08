use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use rusqlite::types::{Type, Value};
use rusqlite::{Connection, OptionalExtension, Row};
use uuid::Uuid;

use crate::core::{
    Assertion, AssertionKind, ConflictGroup, Error, Evidence, EvidenceKind, IngestBundle,
    IngestRecord, Memory, MemoryAction, MemoryActionKind, MemoryDetail, MemoryState, MemorySummary,
    Result, SearchHitRaw, SearchQuery, Source, SourceKind, Space, SpaceLink, Ts,
};
use crate::storage::Store;

const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("../../migrations/0001_init.sql"))];

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
        conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
            .map_err(dberr)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(dberr)?;
        run_migrations(&conn)?;
        Ok(Self { conn })
    }
}

fn dberr(e: rusqlite::Error) -> Error {
    Error::Storage(e.to_string())
}

/// FTS5 reports bad query syntax through its error message; surface that as
/// the dedicated user-facing error instead of a generic storage failure.
fn classify_query_err(e: rusqlite::Error) -> Error {
    let s = e.to_string();
    if s.contains("fts5:") {
        Error::QuerySyntax(s)
    } else {
        Error::Storage(s)
    }
}

fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    INTEGER PRIMARY KEY,
             applied_at INTEGER NOT NULL
         )",
    )
    .map_err(dberr)?;
    let mut applied: HashSet<i64> = HashSet::new();
    {
        let mut stmt = conn
            .prepare("SELECT version FROM schema_migrations")
            .map_err(dberr)?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).map_err(dberr)?;
        for v in rows {
            applied.insert(v.map_err(dberr)?);
        }
    }
    for (version, sql) in MIGRATIONS {
        if applied.contains(version) {
            continue;
        }
        let tx = conn.unchecked_transaction().map_err(dberr)?;
        tx.execute_batch(sql).map_err(dberr)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            rusqlite::params![version, Ts::now().0],
        )
        .map_err(dberr)?;
        tx.commit().map_err(dberr)?;
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
        let exists: bool = self
            .conn
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
        self.conn
            .execute(
                "INSERT INTO spaces (id, name, description, created_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id.to_string(), name, description, created_at.0],
            )
            .map_err(dberr)?;
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
        let from_space = self
            .conn
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
        let to_space = self
            .conn
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
        let exists: bool = self
            .conn
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
        self.conn
            .execute(
                "INSERT INTO space_links (from_space, to_space, created_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![from.to_string(), to.to_string(), created_at.0],
            )
            .map_err(dberr)?;
        Ok(SpaceLink {
            from_space: from,
            to_space: to,
            created_at,
        })
    }

    fn find_source(
        &self,
        kind: SourceKind,
        location: &str,
        content_hash: &str,
    ) -> Result<Option<Source>> {
        self.conn
            .query_row(
                "SELECT id, kind, location, title, content, content_hash, media_type, \
                        size_bytes, ingested_at \
                 FROM sources WHERE kind = ?1 AND location = ?2 AND content_hash = ?3",
                rusqlite::params![kind.as_str(), location, content_hash],
                row_source,
            )
            .optional()
            .map_err(dberr)
    }

    fn record_ingestion(&self, bundle: IngestBundle) -> Result<IngestRecord> {
        let tx = self.conn.unchecked_transaction().map_err(dberr)?;
        let record = record_ingestion_tx(&tx, bundle)?;
        tx.commit().map_err(dberr)?;
        Ok(record)
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
                "SELECT id, kind, location, title, content, content_hash, media_type, \
                        size_bytes, ingested_at \
                 FROM sources WHERE id = ?1",
                rusqlite::params![evidence.source_id.to_string()],
                row_source,
            )
            .map_err(dberr)?;
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
        Ok(Some(MemoryDetail {
            memory,
            assertion,
            evidence,
            source,
            spaces,
            superseded_by,
            supersedes,
        }))
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

        if let Some(at) = query.at {
            sql.push_str(
                " AND m.created_at <= ? AND (m.superseded_at IS NULL OR m.superseded_at > ?) \
                 AND (m.retired_at IS NULL OR m.retired_at > ?)",
            );
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
            if ids.is_empty() {
                return Ok(Vec::new());
            }
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

        sql.push_str(&format!(" ORDER BY score ASC LIMIT ?{}", params.len() + 1));
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
            .map_err(dberr)?
            .ok_or_else(|| Error::MemoryNotFound {
                prefix: id.to_string(),
            })?;
        if memory.state != MemoryState::Active {
            return Err(Error::NotRetirable {
                id: id.to_string(),
                state: memory.state.to_string(),
            });
        }
        self.conn
            .execute(
                "UPDATE memories SET state = 'retired', retired_at = ?1, retire_reason = ?2 \
                 WHERE id = ?3",
                rusqlite::params![at.0, reason, id.to_string()],
            )
            .map_err(dberr)?;
        Ok(Memory {
            state: MemoryState::Retired,
            retired_at: Some(at),
            retire_reason: reason,
            ..memory
        })
    }
}

use rusqlite::params_from_iter;

// ---------------------------------------------------------------------------
// Ingestion transaction (docs/memory-model.md section 6.2)
// ---------------------------------------------------------------------------

fn record_ingestion_tx(conn: &Connection, bundle: IngestBundle) -> Result<IngestRecord> {
    let ingested_at = bundle.source.ingested_at;

    // 1. The source, keyed by (kind, location, content_hash). Re-ingesting
    //    identical content reuses the existing source row.
    let existing = find_source_conn(
        conn,
        bundle.source.kind,
        &bundle.source.location,
        &bundle.source.content_hash,
    )?;
    let (source, source_reused) = match existing {
        Some(s) => (s, true),
        None => {
            let id = Uuid::now_v7();
            conn.execute(
                "INSERT INTO sources (id, kind, location, title, content, content_hash, \
                     media_type, size_bytes, ingested_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    id.to_string(),
                    bundle.source.kind.as_str(),
                    bundle.source.location,
                    bundle.source.title,
                    bundle.source.content,
                    bundle.source.content_hash,
                    bundle.source.media_type,
                    bundle.source.size_bytes as i64,
                    ingested_at.0,
                ],
            )
            .map_err(dberr)?;
            (
                Source {
                    id,
                    kind: bundle.source.kind,
                    location: bundle.source.location.clone(),
                    title: bundle.source.title.clone(),
                    content: bundle.source.content.clone(),
                    content_hash: bundle.source.content_hash.clone(),
                    media_type: bundle.source.media_type.clone(),
                    size_bytes: bundle.source.size_bytes,
                    ingested_at,
                },
                false,
            )
        }
    };

    // 2. Evidence for a new source version. Evidence is written once, when
    //    the source row is written; a reused source already has its evidence.
    let mut evidence_ids = Vec::with_capacity(bundle.evidences.len());
    if !source_reused {
        for d in &bundle.evidences {
            let id = Uuid::now_v7();
            conn.execute(
                "INSERT INTO evidence (id, source_id, kind, heading_path, start_line, end_line, \
                     ordinal, content, content_hash) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    id.to_string(),
                    source.id.to_string(),
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
    }

    // 3. Fragment -> evidence mapping. A new source version carries fresh
    //    evidence rows; a reused source keeps its existing ones.
    let fragment_evidence: Vec<Uuid> = if !source_reused {
        evidence_ids
    } else {
        bundle
            .evidences
            .iter()
            .map(|d| -> Result<Uuid> {
                let existing: Option<String> = conn
                    .query_row(
                        "SELECT id FROM evidence WHERE source_id = ?1 AND kind = ?2 AND ordinal = ?3",
                        rusqlite::params![
                            source.id.to_string(),
                            d.kind.as_str(),
                            i64::from(d.ordinal),
                        ],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(dberr)?;
                existing
                    .map(|s| {
                        Uuid::parse_str(&s)
                            .map_err(|e| Error::Storage(format!("corrupt id '{s}': {e}")))
                    })
                    .transpose()?
                    .ok_or_else(|| {
                        Error::Storage(format!(
                            "reused source {} is missing evidence for ordinal {}",
                            source.id, d.ordinal
                        ))
                    })
            })
            .collect::<Result<Vec<_>>>()?
    };

    // 4. Assertions are created lazily, only for fragments that actually
    //    become or replace memories, and every new memory gets a fresh
    //    assertion (one assertion per memory, one memory per assertion)
    //    citing the fragment's evidence. A reused section never produces
    //    orphan assertion rows.
    let mut assertion_cache: Vec<Option<Uuid>> = vec![None; bundle.assertions.len()];
    let mut get_assertion = |conn: &Connection, idx: usize| -> Result<Uuid> {
        if let Some(id) = assertion_cache[idx] {
            return Ok(id);
        }
        let d = &bundle.assertions[idx];
        let id = Uuid::now_v7();
        conn.execute(
            "INSERT INTO assertions (id, kind, subject, subject_normalized, predicate, \
                 content, extraction, evidence_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                id.to_string(),
                d.kind.as_str(),
                d.subject,
                d.subject_normalized,
                d.predicate,
                d.content,
                d.extraction,
                fragment_evidence[d.evidence_idx].to_string(),
            ],
        )
        .map_err(dberr)?;
        conn.execute(
            "INSERT INTO assertion_fts (assertion_id, subject, content) VALUES (?1, ?2, ?3)",
            rusqlite::params![id.to_string(), d.subject, d.content],
        )
        .map_err(dberr)?;
        assertion_cache[idx] = Some(id);
        Ok(id)
    };

    // 4. Requested spaces, with names for the action report.
    let mut requested_by_assertion: HashMap<usize, Vec<Uuid>> = HashMap::new();
    let mut space_names: BTreeMap<Uuid, String> = BTreeMap::new();
    for spec in &bundle.memories {
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

    // 5. Reconciliation. Old memories: active memories whose source sits at
    //    this location, ordered (created_at, id). New memories: fragments of
    //    the same subject, ordered by evidence ordinal. Pair index-wise.
    let olds = load_summaries(
        conn,
        "",
        "m.state = 'active' AND src.kind = ? AND src.location = ?",
        vec![
            Value::Text(bundle.source.kind.as_str().to_string()),
            Value::Text(bundle.source.location.clone()),
        ],
    )?;
    let mut olds_by_subject: BTreeMap<String, Vec<MemorySummary>> = BTreeMap::new();
    for o in olds {
        olds_by_subject
            .entry(o.subject_normalized.clone())
            .or_default()
            .push(o);
    }
    let mut news_by_subject: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, d) in bundle.assertions.iter().enumerate() {
        news_by_subject
            .entry(d.subject_normalized.clone())
            .or_default()
            .push(i);
    }

    let mut actions: Vec<MemoryAction> = Vec::new();
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
                    let draft = &bundle.assertions[ai];
                    if old.content == draft.content {
                        let requested =
                            requested_by_assertion.get(&ai).cloned().unwrap_or_default();
                        let added = add_spaces(conn, old.memory_id, &requested, ingested_at)?;
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
                        let assertion_id = get_assertion(conn, ai)?;
                        let new_id = create_memory(conn, assertion_id, ingested_at)?;
                        let requested =
                            requested_by_assertion.get(&ai).cloned().unwrap_or_default();
                        add_spaces(conn, new_id, &requested, ingested_at)?;
                        conn.execute(
                            "UPDATE memories SET state = 'superseded', superseded_by = ?1, \
                             superseded_at = ?2 WHERE id = ?3 AND state = 'active'",
                            rusqlite::params![
                                new_id.to_string(),
                                ingested_at.0,
                                old.memory_id.to_string(),
                            ],
                        )
                        .map_err(dberr)?;
                        push_action(
                            conn,
                            &mut actions,
                            new_id,
                            &draft.subject,
                            MemoryActionKind::Superseded {
                                old_memory_id: old.memory_id,
                            },
                        )?;
                    }
                }
                (Some(old), None) => {
                    retire_conn(
                        conn,
                        old.memory_id,
                        ingested_at,
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
                (None, Some(&ai)) => {
                    let draft = &bundle.assertions[ai];
                    let assertion_id = get_assertion(conn, ai)?;
                    let new_id = create_memory(conn, assertion_id, ingested_at)?;
                    let requested = requested_by_assertion.get(&ai).cloned().unwrap_or_default();
                    add_spaces(conn, new_id, &requested, ingested_at)?;
                    push_action(
                        conn,
                        &mut actions,
                        new_id,
                        &draft.subject,
                        MemoryActionKind::Created,
                    )?;
                }
                (None, None) => unreachable!("pairing loop bounds"),
            }
        }
    }

    Ok(IngestRecord {
        source,
        source_reused,
        evidence_count: bundle.evidences.len(),
        assertion_count: bundle.assertions.len(),
        actions,
    })
}

fn find_source_conn(
    conn: &Connection,
    kind: SourceKind,
    location: &str,
    content_hash: &str,
) -> Result<Option<Source>> {
    conn.query_row(
        "SELECT id, kind, location, title, content, content_hash, media_type, size_bytes, \
             ingested_at \
         FROM sources WHERE kind = ?1 AND location = ?2 AND content_hash = ?3",
        rusqlite::params![kind.as_str(), location, content_hash],
        row_source,
    )
    .optional()
    .map_err(dberr)
}

/// Adds the missing space memberships and returns the ids that were added.
fn add_spaces(conn: &Connection, memory_id: Uuid, space_ids: &[Uuid], at: Ts) -> Result<Vec<Uuid>> {
    let existing: HashSet<String> = {
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
        "SELECT id, kind, subject, subject_normalized, predicate, content, extraction, \
             evidence_id \
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
                extraction: r.get("extraction")?,
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
