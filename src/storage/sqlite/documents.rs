//! SQLite side of the document model (M8 S1). See `migrations/0010_documents.sql` and
//! `docs/m8-s1-document-model.md`.
//!
//! One write = one recording revision (`begin_write`), the same transaction as the logical source and
//! the revision's `sources` row. A write that finds nothing to add (an identical repeat) is detected
//! BEFORE a revision is allocated and costs none.

use rusqlite::{params, Connection, OptionalExtension};

use super::{begin_write, dberr, ensure_version, SqliteStore};
use crate::core::{
    sha256_hex, Anchor, AnchorPrecision, BBox, DocumentExtractionDraft, DocumentExtractionInfo,
    DocumentExtractionReport, Error, ObservedFile, Result, RetainedBytes, SourceKind,
    StoredCitation, Ts, Uuid,
};

fn uuid(s: &str) -> Result<Uuid> {
    Uuid::parse_str(s).map_err(|e| Error::Storage(format!("corrupt id '{s}': {e}")))
}

fn path_text(path: &[u32]) -> String {
    path.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

struct Existing {
    extraction_id: Uuid,
    source_id: Uuid,
    logical_id: Uuid,
    structure_hash: String,
    text_hash: String,
}

/// The extraction with this identity at this location, if any. Identity = document location,
/// original-bytes hash, extractor id, extractor version, parameters hash.
fn find_existing(conn: &Connection, d: &DocumentExtractionDraft) -> Result<Option<Existing>> {
    let row: Option<(String, String, String, String, String)> = conn
        .query_row(
            "SELECT e.id, e.source_id, e.logical_id, e.structure_hash, s.content_hash \
             FROM doc_extractions e \
             JOIN logical_sources l ON l.id = e.logical_id \
             JOIN sources s ON s.id = e.source_id \
             WHERE l.kind = 'document' AND l.current_location = ?1 \
               AND e.source_bytes_hash = ?2 AND e.detector_version = ?3 AND e.extractor_id = ?4 \
               AND e.extractor_version = ?5 AND e.params_hash = ?6",
            params![
                d.location,
                d.source_bytes_hash,
                d.detector_version,
                d.extractor_id,
                d.extractor_version,
                d.params_hash
            ],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(dberr)?;
    row.map(|(e, s, l, sh, th)| {
        Ok(Existing {
            extraction_id: uuid(&e)?,
            source_id: uuid(&s)?,
            logical_id: uuid(&l)?,
            structure_hash: sh,
            text_hash: th,
        })
    })
    .transpose()
}

fn blob_present(conn: &Connection, hash: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM doc_blobs WHERE bytes_hash = ?1",
            params![hash],
            |_| Ok(()),
        )
        .optional()
        .map_err(dberr)?
        .is_some())
}

/// An identity that exists must agree with the draft, else the extractor is not deterministic (or the
/// draft is forged): the draft is refused, the stored extraction is never replaced or merged.
fn check_agrees(e: &Existing, d: &DocumentExtractionDraft) -> Result<()> {
    if e.text_hash != d.text_hash() {
        return Err(Error::InvalidDocument(
            "an extraction with this identity exists with a different canonical text".into(),
        ));
    }
    if e.structure_hash != d.structure_hash() {
        return Err(Error::InvalidDocument(
            "an extraction with this identity exists with a different structure".into(),
        ));
    }
    Ok(())
}

pub(super) fn record(
    store: &SqliteStore,
    draft: DocumentExtractionDraft,
) -> Result<DocumentExtractionReport> {
    record_inner(store, draft, None)
}

struct PreparedChunks {
    version: String,
    budget_identity: String,
    hash: String,
    chunks: Vec<PreparedChunk>,
}
struct PreparedChunk {
    start: u64,
    end: u64,
    tokens: usize,
    context: Vec<Vec<u32>>,
}

#[cfg(feature = "embeddings")]
pub(super) fn record_with_chunks(
    store: &SqliteStore,
    draft: DocumentExtractionDraft,
    plan: Option<crate::ingest::document::chunk::ChunkPlan>,
) -> Result<DocumentExtractionReport> {
    let plan = plan.map(|p| {
        let hash = sha256_hex(
            serde_json::json!([
                p.version,
                p.budget_identity,
                p.chunks
                    .iter()
                    .map(|c| serde_json::json!([c.start, c.end, c.tokens, c.context]))
                    .collect::<Vec<_>>()
            ])
            .to_string()
            .as_bytes(),
        );
        PreparedChunks {
            version: p.version,
            budget_identity: p.budget_identity,
            hash,
            chunks: p
                .chunks
                .into_iter()
                .map(|c| PreparedChunk {
                    start: c.start,
                    end: c.end,
                    tokens: c.tokens,
                    context: c.context,
                })
                .collect(),
        }
    });
    record_inner(store, draft, plan)
}

fn chunks_present(
    conn: &Connection,
    extraction: Uuid,
    plan: Option<&PreparedChunks>,
) -> Result<bool> {
    let Some(p) = plan else {
        return Ok(true);
    };
    let hash: Option<String> = conn.query_row(
        "SELECT plan_hash FROM doc_chunk_sets WHERE extraction_id = ?1 AND version = ?2 AND budget_identity = ?3",
        params![extraction.to_string(), p.version, p.budget_identity], |r| r.get(0)
    ).optional().map_err(dberr)?;
    match hash {
        None => Ok(false),
        Some(hash) if hash == p.hash => {
            // The completion marker is not sufficient on its own: refuse incomplete or corrupt
            // rows instead of treating them as a successfully derived set on retry.
            let mut stmt = conn.prepare(
                "SELECT ordinal, text_start, text_end, token_count, context_json FROM doc_chunks WHERE extraction_id=?1 AND version=?2 AND budget_identity=?3 ORDER BY ordinal",
            ).map_err(dberr)?;
            let rows = stmt
                .query_map(
                    params![extraction.to_string(), p.version, p.budget_identity],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, i64>(2)?,
                            r.get::<_, i64>(3)?,
                            r.get::<_, String>(4)?,
                        ))
                    },
                )
                .map_err(dberr)?;
            let mut count = 0;
            for row in rows {
                let (ordinal, start, end, tokens, context) = row.map_err(dberr)?;
                let agrees = p
                    .chunks
                    .get(count)
                    .map(|c| {
                        ordinal == count as i64
                            && start == c.start as i64
                            && end == c.end as i64
                            && tokens == c.tokens as i64
                            && serde_json::from_str::<Vec<Vec<u32>>>(&context)
                                .ok()
                                .as_ref()
                                == Some(&c.context)
                    })
                    .unwrap_or(false);
                if !agrees {
                    return Err(Error::InvalidDocument(
                        "stored chunk rows disagree with their derivation".into(),
                    ));
                }
                count += 1;
            }
            let recorded_count: i64 = conn.query_row(
                "SELECT chunk_count FROM doc_chunk_sets WHERE extraction_id=?1 AND version=?2 AND budget_identity=?3",
                params![extraction.to_string(), p.version, p.budget_identity], |r| r.get(0),
            ).map_err(dberr)?;
            if count != p.chunks.len() || recorded_count != count as i64 {
                return Err(Error::InvalidDocument(
                    "stored chunk set is incomplete".into(),
                ));
            }
            Ok(true)
        }
        Some(_) => Err(Error::InvalidDocument(
            "same chunker identity produced a different chunk plan".into(),
        )),
    }
}

fn record_inner(
    store: &SqliteStore,
    draft: DocumentExtractionDraft,
    chunks: Option<PreparedChunks>,
) -> Result<DocumentExtractionReport> {
    draft.validate()?;
    let text_hash = draft.text_hash();
    let structure_hash = draft.structure_hash();
    let conn = store.connection()?;

    // An identical repeat is recognised before a revision is allocated.
    if let Some(e) = find_existing(&conn, &draft)? {
        check_agrees(&e, &draft)?;
        let present = blob_present(&conn, &draft.source_bytes_hash)?;
        if (draft.retain_bytes.is_none() || present)
            && chunks_present(&conn, e.extraction_id, chunks.as_ref())?
        {
            return Ok(DocumentExtractionReport {
                extraction_id: e.extraction_id,
                logical_id: e.logical_id,
                source_id: e.source_id,
                document_created: false,
                revision_created: false,
                extraction_created: false,
                nodes: draft.nodes.len() as u64,
                bytes_retained_now: false,
                bytes_retained: present,
            });
        }
    }

    let at = draft.observed_at;
    let tx = begin_write(&conn, &store.revision_state)?;

    // An independent connection may have completed the same extraction while this writer waited
    // for SQLite's IMMEDIATE lock. Returning drops/rolls back this unused recording revision.
    if let Some(e) = find_existing(&tx, &draft)? {
        check_agrees(&e, &draft)?;
        let present = blob_present(&tx, &draft.source_bytes_hash)?;
        if (draft.retain_bytes.is_none() || present)
            && chunks_present(&tx, e.extraction_id, chunks.as_ref())?
        {
            return Ok(DocumentExtractionReport {
                extraction_id: e.extraction_id,
                logical_id: e.logical_id,
                source_id: e.source_id,
                document_created: false,
                revision_created: false,
                extraction_created: false,
                nodes: draft.nodes.len() as u64,
                bytes_retained_now: false,
                bytes_retained: present,
            });
        }
    }

    // the document: one logical source of kind 'document' per location
    let logical: Option<String> = tx
        .query_row(
            "SELECT id FROM logical_sources WHERE kind = 'document' AND current_location = ?1 \
             ORDER BY created_at DESC, id DESC LIMIT 1",
            params![draft.location],
            |r| r.get(0),
        )
        .optional()
        .map_err(dberr)?;
    let (logical_id, document_created) = match logical {
        Some(id) => (uuid(&id)?, false),
        None => {
            let id = Uuid::now_v7();
            tx.execute(
                "INSERT INTO logical_sources (id, kind, state, current_location, created_at) \
                 VALUES (?1, 'document', 'present', ?2, ?3)",
                params![id.to_string(), draft.location, at.0],
            )
            .map_err(dberr)?;
            (id, true)
        }
    };

    // the revision: the canonical text, as a source version of that document
    let observed = ObservedFile {
        location: draft.location.clone(),
        content: draft.canonical_text.clone(),
        content_hash: text_hash.clone(),
        title: draft.title.clone(),
        media_type: draft.media_type.clone(),
        size_bytes: draft.canonical_text.len() as u64,
        evidences: Vec::new(),
        assertions: Vec::new(),
        memories: Vec::new(),
    };
    let (revision_created, source_id, _) =
        ensure_version(&tx, logical_id, SourceKind::Document, &observed, at)?;

    // the extraction (re-checked inside the transaction: a concurrent writer may have added it)
    let existing = find_existing(&tx, &draft)?;
    let (extraction_id, extraction_created) = match existing {
        Some(e) => {
            check_agrees(&e, &draft)?;
            (e.extraction_id, false)
        }
        None => {
            let id = Uuid::now_v7();
            tx.execute(
                "INSERT INTO doc_extractions (id, logical_id, source_id, source_bytes_hash, source_size, \
                    text_is_source, format, format_hint, detector_version, extractor_id, extractor_version, \
                    params_hash, structure_hash, node_count, created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                params![
                    id.to_string(),
                    logical_id.to_string(),
                    source_id.to_string(),
                    draft.source_bytes_hash,
                    draft.source_size as i64,
                    draft.text_is_source as i64,
                    draft.format,
                    draft.format_hint,
                    draft.detector_version,
                    draft.extractor_id,
                    draft.extractor_version,
                    draft.params_hash,
                    structure_hash,
                    draft.nodes.len() as i64,
                    at.0,
                ],
            )
            .map_err(dberr)?;
            let mut nodes: Vec<&crate::core::DocNodeDraft> = draft.nodes.iter().collect();
            nodes.sort_by(|a, b| a.path.cmp(&b.path));
            let mut stmt = tx
                .prepare(
                    "INSERT INTO doc_nodes (extraction_id, path, kind, text_start, text_end, page_no, \
                        bbox_x0, bbox_y0, bbox_x1, bbox_y1, precision) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                )
                .map_err(dberr)?;
            for n in nodes {
                stmt.execute(params![
                    id.to_string(),
                    path_text(&n.path),
                    n.kind.as_str(),
                    n.text_range.map(|r| r.0 as i64),
                    n.text_range.map(|r| r.1 as i64),
                    n.page.map(i64::from),
                    n.bbox.map(|b| f64::from(b.x0)),
                    n.bbox.map(|b| f64::from(b.y0)),
                    n.bbox.map(|b| f64::from(b.x1)),
                    n.bbox.map(|b| f64::from(b.y1)),
                    n.precision.as_str(),
                ])
                .map_err(dberr)?;
            }
            (id, true)
        }
    };

    // the original bytes: only on explicit opt-in, content-addressed, at most one row per hash
    let mut bytes_retained_now = false;
    if let Some(bytes) = &draft.retain_bytes {
        let added = tx
            .execute(
                "INSERT OR IGNORE INTO doc_blobs (bytes_hash, size, bytes, created_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![draft.source_bytes_hash, bytes.len() as i64, bytes, at.0],
            )
            .map_err(dberr)?;
        bytes_retained_now = added > 0;
    }
    let bytes_retained = blob_present(&tx, &draft.source_bytes_hash)?;
    if let Some(plan) = chunks.as_ref() {
        if !chunks_present(&tx, extraction_id, Some(plan))? {
            tx.execute(
                "INSERT INTO doc_chunk_sets (extraction_id, version, budget_identity, plan_hash, chunk_count) VALUES (?1,?2,?3,?4,?5)",
                params![extraction_id.to_string(), plan.version, plan.budget_identity, plan.hash, plan.chunks.len() as i64],
            ).map_err(dberr)?;
            for (ordinal, c) in plan.chunks.iter().enumerate() {
                let anchor = Anchor::new(
                    logical_id,
                    &draft.source_bytes_hash,
                    &draft.detector_version,
                    &draft.extractor_id,
                    &draft.extractor_version,
                    &draft.params_hash,
                    vec![],
                    Some((c.start, c.end)),
                    None,
                    None,
                    AnchorPrecision::ByteExact,
                )?;
                let id = sha256_hex(
                    serde_json::json!([
                        logical_id.to_string(),
                        draft.source_bytes_hash,
                        text_hash,
                        draft.detector_version,
                        draft.extractor_id,
                        draft.extractor_version,
                        draft.params_hash,
                        structure_hash,
                        plan.version,
                        plan.budget_identity,
                        c.start,
                        c.end,
                        c.context,
                    ])
                    .to_string()
                    .as_bytes(),
                );
                tx.execute(
                    "INSERT INTO doc_chunks (id, extraction_id, version, budget_identity, ordinal, text_start, text_end, token_count, context_json, anchor) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                    params![id, extraction_id.to_string(), plan.version, plan.budget_identity,
                        ordinal as i64, c.start as i64, c.end as i64, c.tokens as i64,
                        serde_json::to_string(&c.context).map_err(|e| Error::Storage(e.to_string()))?, anchor.to_string()],
                ).map_err(dberr)?;
            }
        }
    }
    tx.commit()?;
    Ok(DocumentExtractionReport {
        extraction_id,
        logical_id,
        source_id,
        document_created,
        revision_created,
        extraction_created,
        nodes: draft.nodes.len() as u64,
        bytes_retained_now,
        bytes_retained,
    })
}

struct NodeRow {
    precision: AnchorPrecision,
    text_range: Option<(u64, u64)>,
    page: Option<u32>,
    bbox: Option<BBox>,
}

fn node_row(conn: &Connection, extraction: &str, path: &[u32]) -> Result<Option<NodeRow>> {
    conn.query_row(
        "SELECT precision, text_start, text_end, page_no, bbox_x0, bbox_y0, bbox_x1, bbox_y1 \
         FROM doc_nodes WHERE extraction_id = ?1 AND path = ?2",
        params![extraction, path_text(path)],
        |r| {
            let precision: String = r.get(0)?;
            let (a, b): (Option<i64>, Option<i64>) = (r.get(1)?, r.get(2)?);
            let page: Option<i64> = r.get(3)?;
            let bb: [Option<f64>; 4] = [r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?];
            Ok((precision, a, b, page, bb))
        },
    )
    .optional()
    .map_err(dberr)?
    .map(|(precision, a, b, page, bb)| {
        Ok(NodeRow {
            precision: AnchorPrecision::parse(&precision)
                .ok_or_else(|| Error::Storage(format!("corrupt precision '{precision}'")))?,
            text_range: a.zip(b).map(|(a, b)| (a as u64, b as u64)),
            page: page.map(|p| p as u32),
            bbox: match bb {
                [Some(x0), Some(y0), Some(x1), Some(y1)] => Some(BBox {
                    x0: x0 as f32,
                    y0: y0 as f32,
                    x1: x1 as f32,
                    y1: y1 as f32,
                }),
                _ => None,
            },
        })
    })
    .transpose()
}

pub(super) fn resolve(store: &SqliteStore, anchor: &Anchor) -> Result<Option<StoredCitation>> {
    anchor.validate()?;
    let conn = store.connection()?;
    let row: Option<(String, String, bool, String, String, String)> = conn
        .query_row(
            "SELECT e.id, e.source_id, e.text_is_source, e.format, s.content, l.current_location \
             FROM doc_extractions e \
             JOIN logical_sources l ON l.id = e.logical_id AND l.kind = 'document' \
             JOIN sources s ON s.id = e.source_id \
             WHERE e.logical_id = ?1 AND e.source_bytes_hash = ?2 AND e.detector_version = ?3 \
               AND e.extractor_id = ?4 AND e.extractor_version = ?5 AND e.params_hash = ?6",
            params![
                anchor.logical_id.to_string(),
                anchor.source_bytes_hash,
                anchor.detector_version,
                anchor.extractor_id,
                anchor.extractor_version,
                anchor.params_hash
            ],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get::<_, i64>(2)? != 0,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(dberr)?;
    let Some((extraction, _source, text_is_source, format, text, locator)) = row else {
        return Ok(None);
    };
    let bad = |why: &str| Err(Error::InvalidAnchor(why.to_string()));

    // what the stored extraction supports at this location
    let (stored_precision, node_range, node_page, node_bbox) = if anchor.node_path.is_empty() {
        (AnchorPrecision::ByteExact, None, None, None)
    } else {
        match node_row(&conn, &extraction, &anchor.node_path)? {
            Some(n) => (n.precision, n.text_range, n.page, n.bbox),
            None => return bad("the extraction has no such node"),
        }
    };
    // A byte range is exact in the canonical text wherever the extraction stores a text range for the
    // node (or names no node): the node's own precision describes how well ITS source location is
    // known, which a resolution never upgrades. Any other precision may not exceed the node's.
    let stored_precision = if anchor.precision == AnchorPrecision::ByteExact {
        if !anchor.node_path.is_empty() && node_range.is_none() {
            return bad("the node stores no text range, a byte range cannot be claimed in it");
        }
        AnchorPrecision::ByteExact
    } else if anchor.precision.rank() > stored_precision.rank() {
        return bad("the anchor claims more precision than the stored extraction supports");
    } else {
        stored_precision
    };
    // nothing may be added to what the node stores
    if let Some(p) = anchor.page {
        let known = if anchor.node_path.is_empty() {
            conn.query_row(
                "SELECT 1 FROM doc_nodes WHERE extraction_id = ?1 AND page_no = ?2 LIMIT 1",
                params![extraction, p],
                |_| Ok(()),
            )
            .optional()
            .map_err(dberr)?
            .is_some()
        } else {
            node_page == Some(p)
        };
        if !known {
            return bad("the page is not one the extraction stores");
        }
    }
    if let Some(b) = &anchor.bbox {
        if node_bbox.as_ref() != Some(b) {
            return bad("the bounding box is not the one the node stores");
        }
    }
    let range = match (anchor.text_range, node_range) {
        (Some((a, b)), Some((na, nb))) => {
            if a < na || b > nb {
                return bad("the text range lies outside the node's range");
            }
            Some((a, b))
        }
        (Some(_), None) if !anchor.node_path.is_empty() => {
            return bad("the node stores no text range");
        }
        (Some(r), None) => Some(r),
        (None, r) => r,
    };
    let quoted = match range {
        Some((a, b)) => {
            let (a, b) = (a as usize, b as usize);
            if b > text.len() || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
                return bad("the text range is outside the canonical text or splits a character");
            }
            Some(text[a..b].to_string())
        }
        None => None,
    };

    // retained original bytes, re-verified against the cited digest on every resolution
    let retained = {
        let bytes: Option<Vec<u8>> = conn
            .query_row(
                "SELECT bytes FROM doc_blobs WHERE bytes_hash = ?1",
                params![anchor.source_bytes_hash],
                |r| r.get(0),
            )
            .optional()
            .map_err(dberr)?;
        match bytes {
            None => RetainedBytes::Absent,
            Some(b) if sha256_hex(&b) == anchor.source_bytes_hash => RetainedBytes::Verified,
            Some(_) => RetainedBytes::Corrupt,
        }
    };
    Ok(Some(StoredCitation {
        anchor: anchor.clone(),
        extraction_id: uuid(&extraction)?,
        format,
        stored_precision,
        text_is_source,
        text_range: range,
        text: quoted,
        retained,
        locator,
    }))
}

pub(super) fn list(store: &SqliteStore, logical_id: Uuid) -> Result<Vec<DocumentExtractionInfo>> {
    let conn = store.connection()?;
    let mut stmt = conn
        .prepare(
            "SELECT e.id, e.logical_id, e.source_id, e.source_bytes_hash, s.content_hash, e.source_size, \
                    e.text_is_source, e.format, e.format_hint, e.detector_version, e.extractor_id, \
                    e.extractor_version, e.params_hash, e.node_count, e.created_at, \
                    EXISTS (SELECT 1 FROM doc_blobs b WHERE b.bytes_hash = e.source_bytes_hash) \
             FROM doc_extractions e JOIN sources s ON s.id = e.source_id \
             WHERE e.logical_id = ?1 ORDER BY e.created_at, e.id",
        )
        .map_err(dberr)?;
    let rows = stmt
        .query_map(params![logical_id.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, String>(10)?,
                r.get::<_, String>(11)?,
                r.get::<_, String>(12)?,
                r.get::<_, i64>(13)?,
                r.get::<_, i64>(14)?,
                r.get::<_, i64>(15)?,
            ))
        })
        .map_err(dberr)?;
    let mut out = Vec::new();
    for row in rows {
        let r = row.map_err(dberr)?;
        out.push(DocumentExtractionInfo {
            extraction_id: uuid(&r.0)?,
            logical_id: uuid(&r.1)?,
            source_id: uuid(&r.2)?,
            source_bytes_hash: r.3,
            text_hash: r.4,
            source_size: r.5 as u64,
            text_is_source: r.6 != 0,
            format: r.7,
            format_hint: r.8,
            detector_version: r.9,
            extractor_id: r.10,
            extractor_version: r.11,
            params_hash: r.12,
            node_count: r.13 as u64,
            created_at: Ts(r.14),
            bytes_retained: r.15 != 0,
        });
    }
    Ok(out)
}

pub(super) fn erase(store: &SqliteStore, source_bytes_hash: &str) -> Result<bool> {
    if !crate::core::is_sha256_hex(source_bytes_hash) {
        return Err(Error::InvalidDocument("not a SHA-256 digest".into()));
    }
    let conn = store.connection()?;
    let n = conn
        .execute(
            "DELETE FROM doc_blobs WHERE bytes_hash = ?1",
            params![source_bytes_hash],
        )
        .map_err(dberr)?;
    Ok(n > 0)
}

pub(super) fn list_chunks(
    store: &SqliteStore,
    extraction_id: Uuid,
) -> Result<Vec<crate::storage::StoredDocumentChunk>> {
    let conn = store.connection()?;
    let mut stmt = conn.prepare(
        "SELECT id, version, budget_identity, text_start, text_end, token_count, context_json, anchor \
         FROM doc_chunks WHERE extraction_id = ?1 ORDER BY version, budget_identity, ordinal",
    ).map_err(dberr)?;
    let rows = stmt
        .query_map(params![extraction_id.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
            ))
        })
        .map_err(dberr)?;
    rows.map(|r| {
        let (id, version, budget_identity, start, end, tokens, context, anchor) =
            r.map_err(dberr)?;
        Ok(crate::storage::StoredDocumentChunk {
            id,
            extraction_id,
            version,
            budget_identity,
            start: start as u64,
            end: end as u64,
            tokens: tokens as usize,
            context: serde_json::from_str(&context).map_err(|e| Error::Storage(e.to_string()))?,
            anchor: anchor.parse()?,
        })
    })
    .collect()
}
