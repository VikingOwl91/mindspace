//! Reconcile pre-text-v2 derivations when their paths are reobserved.
//! Source versions, Git objects, evidence and old code parses remain immutable.
use super::{dberr, retire_conn, FILE_PRODUCER_FAMILY, GIT_PRODUCER_FAMILY};
use crate::core::{Error, Result, SkippedFile, Ts, Uuid};
use crate::ingest::{
    gitverify,
    text_policy::{MAX_TEXT_BYTES, REVISION},
};
use rusqlite::{params, Connection, OptionalExtension};

pub(super) fn recheck(
    conn: &Connection,
    repo: Option<Uuid>,
    skip: &SkippedFile,
    at: Ts,
) -> Result<()> {
    let repo_id = repo.map(|r| r.to_string());
    let (sql, family) = match (repo, &skip.git) {
        (Some(_), Some(_)) => (
            "SELECT s.id,s.content_hash,s.size_bytes,CASE WHEN s.size_bytes <= ?4 THEN s.content END
             FROM logical_sources l JOIN sources s ON s.logical_id=l.id
             JOIN git_source_versions g ON g.source_id=s.id
             WHERE l.repository_id=?1 AND l.commit_oid=?2 AND l.git_path=?3 AND g.blob_oid=?5",
            GIT_PRODUCER_FAMILY,
        ),
        (None, None) => (
            "SELECT s.id,s.content_hash,s.size_bytes,NULL FROM logical_sources l
             JOIN sources s ON s.logical_id=l.id
             WHERE l.kind='file' AND l.current_location=?1 AND s.size_bytes>?2",
            FILE_PRODUCER_FAMILY,
        ),
        _ => return Err(Error::InvalidExtraction("skip kind disagrees with observation".into())),
    };
    let mut stmt = conn.prepare_cached(sql).map_err(dberr)?;
    let mut rows = match &skip.git {
        Some(git) => stmt.query(params![
            repo_id,
            git.commit_oid,
            skip.path,
            MAX_TEXT_BYTES as i64,
            git.blob_oid
        ]),
        None => stmt.query(params![skip.path, MAX_TEXT_BYTES as i64]),
    }
    .map_err(dberr)?;
    let mut found_source = false;
    while let Some(row) = rows.next().map_err(dberr)? {
        found_source = true;
        let source_id: String = row.get(0).map_err(dberr)?;
        let hash: String = row.get(1).map_err(dberr)?;
        let size: i64 = row.get(2).map_err(dberr)?;
        let reason = if size > MAX_TEXT_BYTES as i64 {
            "too_large"
        } else if let Some(git) = &skip.git {
            // The worker's exclusion reason alone cannot invalidate a stored fact.
            // Valid UTF-8 containing a literal replacement character still hashes to its OID.
            let text: String = row.get(3).map_err(dberr)?;
            if gitverify::verify_blob(&git.blob_oid, text.as_bytes()).is_ok() {
                continue;
            }
            "invalid_utf8"
        } else {
            continue;
        };
        let note = format!("{REVISION}:{reason}");
        let mut mems = conn.prepare("SELECT m.id FROM memories m JOIN assertions a ON a.id=m.assertion_id
            JOIN evidence e ON e.id=a.evidence_id WHERE e.source_id=?1 AND m.producer_family=?2 AND m.state='active' ORDER BY m.id").map_err(dberr)?;
        let ids = mems
            .query_map(params![source_id, family], |r| r.get::<_, String>(0))
            .map_err(dberr)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(dberr)?;
        for id in ids {
            retire_conn(
                conn,
                Uuid::parse_str(&id).map_err(|e| Error::Storage(e.to_string()))?,
                at,
                &note,
            )?;
        }
        // A newer, explicitly skipped derivation hides obsolete structure from the
        // existing effective-derivation readers. Only inputs already parsed are affected.
        invalidate_structure(
            conn,
            &hash,
            repo_id.as_deref(),
            skip.git.as_ref().map(|g| g.blob_oid.as_str()),
            at,
            &note,
        )?;
    }
    if !found_source {
        if let (Some(repo), Some(git)) = (repo_id.as_deref(), &skip.git) {
            recheck_code_only_blob(conn, repo, &git.blob_oid, at)?;
        }
    }
    Ok(())
}

/// The independent code writer can associate structure before a source version
/// exists. Check the previously stored body when present. A metadata-only row
/// cannot establish that a worker's encoding claim is true.
fn recheck_code_only_blob(conn: &Connection, repo: &str, blob: &str, at: Ts) -> Result<()> {
    let stored: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT i.content_hash,CASE WHEN length(CAST(b.content AS BLOB))<=?3 THEN b.content END
         FROM code_inputs i JOIN git_blobs b ON b.repository_id=i.repository_id AND b.oid=i.blob_oid
         WHERE i.repository_id=?1 AND i.blob_oid=?2 AND b.content IS NOT NULL",
            params![repo, blob, MAX_TEXT_BYTES as i64],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(dberr)?;
    let Some((hash, content)) = stored else {
        return Ok(());
    };
    let reason = match content {
        None => "too_large",
        Some(text)
            if crate::core::sha256_hex(text.as_bytes()) == hash
                && gitverify::verify_blob(blob, text.as_bytes()).is_err() =>
        {
            "invalid_utf8"
        }
        Some(_) => return Ok(()),
    };
    invalidate_structure(
        conn,
        &hash,
        Some(repo),
        Some(blob),
        at,
        &format!("{REVISION}:{reason}"),
    )
}

fn invalidate_structure(
    conn: &Connection,
    hash: &str,
    repo: Option<&str>,
    blob: Option<&str>,
    at: Ts,
    note: &str,
) -> Result<()> {
    let version = format!("{REVISION}+skip({note})");
    let mut stmt = conn.prepare_cached(
        "SELECT i.id,d.language,d.extractor,d.grammar,d.grammar_version,d.created_at
         FROM code_inputs i JOIN code_derivations d ON d.input_id=i.id
         WHERE i.content_hash=?1 AND i.repository_id IS ?2 AND i.blob_oid IS ?3
           AND d.id=(SELECT d2.id FROM code_derivations d2 WHERE d2.input_id=i.id AND d2.language=d.language ORDER BY d2.created_at DESC,d2.id DESC LIMIT 1)
           AND d.extractor_version<>?4").map_err(dberr)?;
    let previous = stmt
        .query_map(params![hash, repo, blob, version], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })
        .map_err(dberr)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(dberr)?;
    for (input, language, extractor, grammar, grammar_version, created) in previous {
        conn.execute("INSERT INTO code_derivations(id,input_id,language,extractor,extractor_version,grammar,grammar_version,status,error_count,created_at)
            VALUES (?1,?2,?3,?4,?5,?6,?7,'failed',0,?8)",
            params![Uuid::now_v7().to_string(),input,language,extractor,version,grammar,grammar_version,at.0.max(created.saturating_add(1))]).map_err(dberr)?;
    }
    Ok(())
}

pub(super) fn last_accepted(conn: &Connection, path: &str) -> Result<Option<Uuid>> {
    conn.query_row(
        "SELECT id FROM (SELECT s.id,s.size_bytes FROM logical_sources l JOIN sources s ON s.logical_id=l.id
         WHERE l.kind='file' AND l.state='present' AND l.current_location=?1
         ORDER BY s.ingested_at DESC,s.id DESC LIMIT 1) WHERE size_bytes <= ?2",
        params![path, MAX_TEXT_BYTES as i64], |r| super::uuid_col(r,"id")).optional().map_err(dberr)
}
