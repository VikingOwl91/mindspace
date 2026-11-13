//! Connection-bound legacy memory response reads. No layout or write authority.

use super::*;
use crate::storage::MemoryRead;

pub(super) struct SqliteMemoryRead<'a> {
    conn: ConnectionGuard<'a>,
    fts_integrity: &'a AtomicBool,
    active: bool,
}

impl<'a> SqliteMemoryRead<'a> {
    pub(super) fn begin(store: &'a SqliteStore) -> Result<Self> {
        let mut conn = store.connection()?;
        if !conn.is_autocommit() {
            return Err(Error::Storage(
                "nested SQLite read session is not allowed".into(),
            ));
        }
        if let Err(error) = conn.execute_batch("BEGIN DEFERRED") {
            if !conn.is_autocommit() {
                conn.revision_state.poisoned.store(true, Ordering::SeqCst);
                conn.guard.take();
            }
            return Err(dberr(error));
        }
        Ok(Self {
            conn,
            fts_integrity: &store.fts_integrity,
            active: true,
        })
    }

    #[cfg(test)]
    pub(super) fn connection_for_test(&self) -> &Connection {
        &self.conn
    }

    fn ensure_active(&self) -> Result<()> {
        if !self.active || self.conn.is_autocommit() {
            return Err(Error::Storage(
                "SQLite read session lost its transaction".into(),
            ));
        }
        Ok(())
    }

    fn end(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        let result = self.conn.execute_batch("ROLLBACK").map_err(dberr);
        self.active = false;
        if result.is_err() || !self.conn.is_autocommit() {
            self.conn
                .revision_state
                .poisoned
                .store(true, Ordering::SeqCst);
            self.conn.guard.take();
            return result.and(Err(Error::Storage(
                "SQLite read session did not end cleanly".into(),
            )));
        }
        result
    }
}

impl Drop for SqliteMemoryRead<'_> {
    fn drop(&mut self) {
        let _ = self.end();
    }
}

impl MemoryRead for SqliteMemoryRead<'_> {
    fn get_space(&self, name: &str) -> Result<Space> {
        self.ensure_active()?;
        get_space_on(&self.conn, name)
    }

    fn list_spaces(&self) -> Result<Vec<Space>> {
        self.ensure_active()?;
        list_spaces_on(&self.conn)
    }

    fn list_spaces_with_counts(&self) -> Result<Vec<(Space, u64)>> {
        self.ensure_active()?;
        list_spaces_with_counts_on(&self.conn)
    }

    fn list_space_links(&self) -> Result<Vec<SpaceLink>> {
        self.ensure_active()?;
        list_space_links_on(&self.conn)
    }

    fn embedding_index_status(&self, derivation_key: &str) -> Result<EmbeddingIndexStatus> {
        self.ensure_active()?;
        embedding_index_status_on(&self.conn, derivation_key)
    }

    fn read_embedding_settings(&self) -> Result<EmbeddingSettings> {
        self.ensure_active()?;
        read_embedding_settings_on(&self.conn)
    }

    fn search_vectors(
        &self,
        query_vector: &[f32],
        query: &SearchQuery,
        derivation_key: &str,
    ) -> Result<Vec<SearchHitRaw>> {
        self.ensure_active()?;
        #[cfg(test)]
        read_session_tests::checkpoint("before_vectors");
        search_vectors_on(&self.conn, query_vector, query, derivation_key)
    }

    fn get_memory_detail(&self, id: Uuid) -> Result<Option<MemoryDetail>> {
        self.ensure_active()?;
        get_memory_detail_on(&self.conn, id)
    }

    fn list_memories_by_subject(&self, subject_normalized: &str) -> Result<Vec<MemorySummary>> {
        self.ensure_active()?;
        list_memories_by_subject_on(&self.conn, subject_normalized)
    }

    fn list_replaced_by(&self, id: Uuid) -> Result<Vec<MemorySummary>> {
        self.ensure_active()?;
        list_replaced_by_on(&self.conn, id)
    }

    fn memory_id_prefix_matches(&self, prefix: &str) -> Result<Vec<Uuid>> {
        self.ensure_active()?;
        memory_id_prefix_matches_on(&self.conn, prefix)
    }

    fn search(&self, query: &SearchQuery) -> Result<Vec<SearchHitRaw>> {
        self.ensure_active()?;
        #[cfg(test)]
        read_session_tests::checkpoint("before_search");
        search_on(&self.conn, self.fts_integrity.load(Ordering::SeqCst), query)
    }

    fn list_conflicts(&self) -> Result<Vec<ConflictGroup>> {
        self.ensure_active()?;
        list_conflicts_on(&self.conn)
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        self.end()
    }
}

pub(super) fn get_space_on(conn: &Connection, name: &str) -> Result<Space> {
    let space = conn
        .query_row(
            "SELECT id, name, description, created_at FROM spaces WHERE name = ?1",
            rusqlite::params![name],
            row_space,
        )
        .optional()
        .map_err(dberr)?
        .ok_or_else(|| Error::SpaceNotFound {
            name: name.to_string(),
        })?;
    #[cfg(test)]
    read_session_tests::checkpoint("space_name");
    Ok(space)
}

pub(super) fn list_spaces_on(conn: &Connection) -> Result<Vec<Space>> {
    let mut stmt = conn
        .prepare("SELECT id, name, description, created_at FROM spaces ORDER BY name")
        .map_err(dberr)?;
    let rows = stmt.query_map([], row_space).map_err(dberr)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(dberr)?);
    }
    Ok(out)
}

pub(super) fn list_spaces_with_counts_on(conn: &Connection) -> Result<Vec<(Space, u64)>> {
    let mut stmt = conn
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

pub(super) fn list_space_links_on(conn: &Connection) -> Result<Vec<SpaceLink>> {
    let mut stmt = conn
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

pub(super) fn embedding_index_status_on(
    conn: &Connection,
    derivation_key: &str,
) -> Result<EmbeddingIndexStatus> {
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM assertions", [], |r| r.get(0))
        .map_err(dberr)?;
    let embedded: i64 = conn
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

pub(super) fn search_vectors_on(
    conn: &Connection,
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
        "SELECT m.id, a.id AS assertion_id, a.subject, a.content, ae.embedding, ae.dimension, \
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
    let mut stmt = conn.prepare(&sql).map_err(dberr)?;
    let rows = stmt
        .query_map(params_from_iter(params), |r| {
            let memory_id: String = r.get("id")?;
            let assertion_id: String = r.get("assertion_id")?;
            let embedding_bytes: Vec<u8> = r.get("embedding")?;
            let stored_dim: i64 = r.get("dimension")?;
            Ok((
                SearchHitRaw {
                    memory_id: Uuid::parse_str(&memory_id).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(0, Type::Text, Box::new(e))
                    })?,
                    assertion_id: Uuid::parse_str(&assertion_id).map_err(|e| {
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
    let names = space_names_by_memory(conn, &ids)?;
    for h in &mut hits {
        if let Some(ns) = names.get(&h.memory_id.to_string()) {
            h.space_names = ns.clone();
        }
    }
    // Assign rank is deferred to retrieval (M4); store returns raw hits.
    Ok(hits)
}

pub(super) fn get_memory_detail_on(conn: &Connection, id: Uuid) -> Result<Option<MemoryDetail>> {
    let memory = conn
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
    #[cfg(test)]
    read_session_tests::checkpoint("detail_memory");
    let assertion = load_assertion(conn, memory.assertion_id)?;
    let evidence = load_evidence(conn, assertion.evidence_id)?;
    let source = conn
        .query_row(
            "SELECT id, logical_id, kind, location, title, content, content_hash, \
                        media_type, size_bytes, ingested_at \
                 FROM sources WHERE id = ?1",
            rusqlite::params![evidence.source_id.to_string()],
            row_source,
        )
        .map_err(dberr)?;
    let source_lifecycle = load_source_lifecycle(conn, source.logical_id)?
        .ok_or_else(|| Error::Storage("source without logical identity".to_string()))?;
    let mut spaces_stmt = conn
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
        Some(sb) => load_summaries(conn, "", "m.id = ?", vec![Value::Text(sb.to_string())])?
            .into_iter()
            .next(),
        None => None,
    };
    let supersedes = load_summaries(
        conn,
        "",
        "m.superseded_by = ?",
        vec![Value::Text(memory.id.to_string())],
    )?;
    let git = load_git_provenance(conn, &source.id)?;
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

pub(super) fn list_memories_by_subject_on(
    conn: &Connection,
    subject_normalized: &str,
) -> Result<Vec<MemorySummary>> {
    load_summaries(
        conn,
        "",
        "a.subject_normalized = ?1",
        vec![Value::Text(subject_normalized.to_string())],
    )
}

pub(super) fn list_replaced_by_on(conn: &Connection, id: Uuid) -> Result<Vec<MemorySummary>> {
    load_summaries(
        conn,
        "",
        "m.superseded_by = ?1",
        vec![Value::Text(id.to_string())],
    )
}

pub(super) fn memory_id_prefix_matches_on(conn: &Connection, prefix: &str) -> Result<Vec<Uuid>> {
    let lowered: String = prefix
        .to_lowercase()
        .chars()
        .filter(|c| *c != '-')
        .collect();
    if lowered.is_empty() {
        return Ok(Vec::new());
    }
    let pattern = format!("{lowered}%");
    let mut stmt = conn
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
    #[cfg(test)]
    read_session_tests::checkpoint("prefix_matches");
    Ok(out)
}

pub(super) fn search_on(
    conn: &Connection,
    fts_integrity: bool,
    query: &SearchQuery,
) -> Result<Vec<SearchHitRaw>> {
    if !fts_integrity {
        return Err(Error::FtsIndexInconsistent {
            detail: "the last open-time integrity check failed; verify or rebuild the index"
                .to_string(),
        });
    }
    let text = query.text.trim();
    if text.is_empty() {
        return Err(Error::EmptyQuery);
    }
    let mut sql = String::from(
        "SELECT m.id, a.id AS assertion_id, a.subject, a.content, \
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
        run_search_sql(conn, &sql, p)
    };
    match run(text) {
        Err(Error::QuerySyntax(_)) => run(&format!("\"{}\"", text.replace('"', "\"\""))),
        other => other,
    }
}

pub(super) fn list_conflicts_on(conn: &Connection) -> Result<Vec<ConflictGroup>> {
    let mut stmt = conn
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
        #[cfg(test)]
        read_session_tests::checkpoint("conflict_members");
        let members = load_summaries(
            conn,
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
