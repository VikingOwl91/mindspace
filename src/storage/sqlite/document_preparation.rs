//! One-time unpublished document association. Only the direct local administrative CLI can arm it.
use super::*;
#[cfg(unix)]
use crate::cli::ownership::AdminContext;
use crate::core::document::MAX_CANONICAL_TEXT_BYTES;
use crate::core::{AnchorPrecision, BBox, DocNodeDraft, NodeKind, MAX_NODES};
use serde::{Deserialize, Serialize};

const SQL: &str = include_str!("../../../migrations/0014_document_preparation.sql");
const DOMAIN: &str = "mindspace.s3c.local-document-preparation@1";
const ACTOR: &str = "local-os-account@1";
const ACTION: &str = "enroll_document_preparation@1";
const MAX_REQUEST: usize = 65536;
const ENROLL_COLUMNS: &str = "asset_id,source_id,document_id,legacy_operation_id,legacy_revision,legacy_source_version,document_source_version,extraction_id,snapshot_digest,operation_id,created_revision,state";
const RECEIPT_COLUMNS: &str = "operation_id,nonce,digest,canonical_request,intent_json,request_version,db_path,db_dev,db_ino,actor_kind,actor_uid,asset_id,source_id,document_id,legacy_source_version,document_source_version,extraction_id,snapshot_digest,expected_revision,committed_revision,action,expected_state,intended_state";

#[derive(Default)]
pub(super) struct PreparationState {
    permit: Mutex<Option<Permit>>,
}
struct Permit {
    enrollment: Vec<Value>,
    receipt: Vec<Value>,
}
struct PermitScope<'a>(&'a PreparationState);
impl Drop for PermitScope<'_> {
    fn drop(&mut self) {
        if let Ok(mut p) = self.0.permit.lock() {
            *p = None;
        }
    }
}
pub(super) fn register_context(conn: &Connection, state: &Arc<PreparationState>) -> Result<()> {
    for (name, enrollment) in [("s3c_enrollment_valid", true), ("s3c_receipt_valid", false)] {
        let state = Arc::clone(state);
        conn.create_scalar_function(
            name,
            -1,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS,
            move |ctx| {
                let Ok(guard) = state.permit.lock() else {
                    return Ok(false);
                };
                let Some(p) = guard.as_ref() else {
                    return Ok(false);
                };
                let values = if enrollment {
                    &p.enrollment
                } else {
                    &p.receipt
                };
                if ctx.len() != values.len() {
                    return Ok(false);
                }
                for (n, value) in values.iter().enumerate() {
                    if ctx.get::<Value>(n)? != *value {
                        return Ok(false);
                    }
                }
                Ok(true)
            },
        )
        .map_err(dberr)?;
    }
    let state = Arc::clone(state);
    conn.create_scalar_function(
        "s3c_availability_valid",
        3,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS,
        move |ctx| {
            let Ok(guard) = state.permit.lock() else {
                return Ok(false);
            };
            let Some(p) = guard.as_ref() else {
                return Ok(false);
            };
            let table: String = ctx.get(0)?;
            let key = match table.as_str() {
                "document_preparation_enrollments" => &p.enrollment[0],
                "document_preparation_receipts" => &p.receipt[0],
                _ => return Ok(false),
            };
            Ok(ctx.get::<Value>(1)? == *key && ctx.get::<Value>(2)? == p.enrollment[10])
        },
    )
    .map_err(dberr)?;
    Ok(())
}
pub(super) fn enforcement_names() -> Vec<String> {
    SQL.split_whitespace()
        .collect::<Vec<_>>()
        .windows(3)
        .filter(|w| w[0] == "CREATE" && w[1] == "TRIGGER")
        .map(|w| w[2].to_owned())
        .collect()
}
fn refuse(message: &str) -> Error {
    Error::Storage(format!("document preparation refused: {message}"))
}
fn string(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u64).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}
fn optional(out: &mut Vec<u8>, s: &Option<String>) {
    match s {
        Some(s) => {
            out.push(1);
            string(out, s)
        }
        None => out.push(0),
    }
}
fn id(raw: String) -> Result<Uuid> {
    let v = Uuid::parse_str(&raw).map_err(|_| refuse("invalid stored UUID"))?;
    if v.to_string() != raw {
        return Err(refuse("noncanonical stored UUID"));
    }
    Ok(v)
}
fn number(n: i64) -> Result<u64> {
    u64::try_from(n).map_err(|_| refuse("negative stored count"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparationTarget {
    pub asset_id: Uuid,
    pub legacy_source_version: Uuid,
    pub document_id: Uuid,
    pub document_source_version: Uuid,
    pub extraction_id: Uuid,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LogicalBase {
    id: Uuid,
    kind: String,
    state: String,
    location: Option<String>,
    created_at: i64,
}
impl LogicalBase {
    fn canonical(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self.id.as_bytes());
        string(out, &self.kind);
        string(out, &self.state);
        optional(out, &self.location);
        out.extend_from_slice(&self.created_at.to_be_bytes());
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Version {
    id: Uuid,
    logical_id: Uuid,
    kind: String,
    location: String,
    title: String,
    hash: String,
    media_type: String,
    size: u64,
    ingested_at: i64,
}
impl Version {
    fn canonical(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self.id.as_bytes());
        out.extend_from_slice(self.logical_id.as_bytes());
        for v in [
            &self.kind,
            &self.location,
            &self.title,
            &self.hash,
            &self.media_type,
        ] {
            string(out, v)
        }
        out.extend_from_slice(&self.size.to_be_bytes());
        out.extend_from_slice(&self.ingested_at.to_be_bytes());
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Extraction {
    id: Uuid,
    logical_id: Uuid,
    source_id: Uuid,
    bytes_hash: String,
    source_size: u64,
    text_is_source: bool,
    format: String,
    format_hint: Option<String>,
    detector_version: String,
    extractor_id: String,
    extractor_version: String,
    params_hash: String,
    structure_hash: String,
    node_count: u64,
    created_at: i64,
}
impl Extraction {
    fn canonical(&self, out: &mut Vec<u8>) {
        for v in [self.id, self.logical_id, self.source_id] {
            out.extend_from_slice(v.as_bytes())
        }
        string(out, &self.bytes_hash);
        out.extend_from_slice(&self.source_size.to_be_bytes());
        out.push(u8::from(self.text_is_source));
        string(out, &self.format);
        optional(out, &self.format_hint);
        for s in [
            &self.detector_version,
            &self.extractor_id,
            &self.extractor_version,
            &self.params_hash,
            &self.structure_hash,
        ] {
            string(out, s)
        }
        out.extend_from_slice(&self.node_count.to_be_bytes());
        out.extend_from_slice(&self.created_at.to_be_bytes());
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    target: PreparationTarget,
    legacy_source_id: Uuid,
    legacy_operation_id: String,
    legacy_revision: i64,
    legacy_base: LogicalBase,
    document_base: LogicalBase,
    legacy_version: Version,
    document_version: Version,
    extraction: Extraction,
}
impl Snapshot {
    fn canonical(&self) -> Vec<u8> {
        let mut out = Vec::new();
        string(&mut out, "mindspace.s3c.snapshot@1");
        out.extend_from_slice(self.target.asset_id.as_bytes());
        out.extend_from_slice(self.legacy_source_id.as_bytes());
        string(&mut out, &self.legacy_operation_id);
        out.extend_from_slice(&self.legacy_revision.to_be_bytes());
        string(&mut out, "legacy");
        out.extend_from_slice(&1_u64.to_be_bytes());
        self.legacy_version.canonical(&mut out);
        self.document_version.canonical(&mut out);
        self.extraction.canonical(&mut out);
        out
    }
    fn digest(&self) -> String {
        sha256_hex(&self.canonical())
    }
    fn validate_links(&self) -> Result<()> {
        let t = &self.target;
        if self.legacy_revision <= 0
            || self.legacy_source_id == t.document_id
            || self.legacy_base.id != self.legacy_source_id
            || self.legacy_base.kind != "file"
            || self.document_base.id != t.document_id
            || self.document_base.kind != "document"
            || self.legacy_version.id != t.legacy_source_version
            || self.legacy_version.logical_id != self.legacy_source_id
            || self.legacy_version.kind != "file"
            || self.document_version.id != t.document_source_version
            || self.document_version.logical_id != t.document_id
            || self.document_version.kind != "document"
            || self.extraction.id != t.extraction_id
            || self.extraction.logical_id != t.document_id
            || self.extraction.source_id != t.document_source_version
            || self.extraction.bytes_hash != self.legacy_version.hash
            || self.extraction.source_size != self.legacy_version.size
        {
            return Err(refuse(
                "incompatible selected source or extraction identities",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    nonce: Uuid,
    db_path: String,
    db_dev: u64,
    db_ino: u64,
    actor_uid: u32,
    expected_revision: i64,
    snapshot: Snapshot,
}
impl Intent {
    fn canonical(&self) -> Result<Vec<u8>> {
        if self.expected_revision <= 0 || self.expected_revision.checked_add(1).is_none() {
            return Err(refuse("expected revision is invalid"));
        }
        self.snapshot.validate_links()?;
        let mut out = Vec::new();
        string(&mut out, DOMAIN);
        out.extend_from_slice(self.nonce.as_bytes());
        string(&mut out, &self.db_path);
        out.extend_from_slice(&self.db_dev.to_be_bytes());
        out.extend_from_slice(&self.db_ino.to_be_bytes());
        string(&mut out, ACTOR);
        out.extend_from_slice(&(self.actor_uid as u64).to_be_bytes());
        out.extend_from_slice(&(self.actor_uid as u64).to_be_bytes());
        let snapshot = self.snapshot.canonical();
        out.extend_from_slice(&(snapshot.len() as u64).to_be_bytes());
        out.extend_from_slice(&snapshot);
        self.snapshot.legacy_base.canonical(&mut out);
        self.snapshot.document_base.canonical(&mut out);
        out.extend_from_slice(&(self.expected_revision as u64).to_be_bytes());
        for s in [
            ACTION,
            "NoDocumentPreparation",
            "DocumentPreparationEnrolled",
        ] {
            string(&mut out, s)
        }
        if out.len() > MAX_REQUEST {
            return Err(refuse("canonical request exceeds 64 KiB"));
        }
        Ok(out)
    }
    fn digest(&self) -> Result<String> {
        Ok(sha256_hex(&self.canonical()?))
    }
    fn operation_id(&self) -> Result<String> {
        Ok(format!(
            "local-doc-prepare@1:{}:{}",
            self.nonce,
            self.digest()?
        ))
    }
    fn enrollment_values(&self, operation: &str, revision: i64) -> Vec<Value> {
        let s = &self.snapshot;
        vec![
            s.target.asset_id.to_string().into(),
            s.legacy_source_id.to_string().into(),
            s.target.document_id.to_string().into(),
            s.legacy_operation_id.clone().into(),
            s.legacy_revision.into(),
            s.target.legacy_source_version.to_string().into(),
            s.target.document_source_version.to_string().into(),
            s.target.extraction_id.to_string().into(),
            s.digest().into(),
            operation.to_owned().into(),
            revision.into(),
            "document_preparation_enrolled".to_owned().into(),
        ]
    }
    fn receipt_values(&self, operation: &str, revision: i64) -> Result<Vec<Value>> {
        let s = &self.snapshot;
        let json = serde_json::to_vec(self).map_err(|_| refuse("cannot encode stored intent"))?;
        if json.len() > MAX_REQUEST {
            return Err(refuse("durable intent exceeds 64 KiB"));
        }
        Ok(vec![
            operation.to_owned().into(),
            self.nonce.to_string().into(),
            self.digest()?.into(),
            self.canonical()?.into(),
            json.into(),
            1_i64.into(),
            self.db_path.clone().into(),
            self.db_dev.to_string().into(),
            self.db_ino.to_string().into(),
            ACTOR.to_owned().into(),
            (self.actor_uid as i64).into(),
            s.target.asset_id.to_string().into(),
            s.legacy_source_id.to_string().into(),
            s.target.document_id.to_string().into(),
            s.target.legacy_source_version.to_string().into(),
            s.target.document_source_version.to_string().into(),
            s.target.extraction_id.to_string().into(),
            s.digest().into(),
            self.expected_revision.into(),
            revision.into(),
            ACTION.to_owned().into(),
            "NoDocumentPreparation".to_owned().into(),
            "DocumentPreparationEnrolled".to_owned().into(),
        ])
    }
}
fn parse_id(value: &str) -> Result<(Uuid, &str)> {
    let p = value.split(':').collect::<Vec<_>>();
    if p.len() != 3 || p[0] != "local-doc-prepare@1" || !crate::core::is_sha256_hex(p[2]) {
        return Err(refuse("invalid full operation identity"));
    }
    let nonce = id(p[1].to_owned())?;
    Ok((nonce, p[2]))
}

fn logical(conn: &Connection, source: Uuid, kind: &str) -> Result<LogicalBase> {
    let row=conn.query_row("SELECT kind,state,current_location,created_at,repository_id IS NULL AND commit_oid IS NULL AND git_path IS NULL FROM logical_sources WHERE id=?1",[source.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,i64>(3)?,r.get::<_,bool>(4)?))).optional().map_err(dberr)?.ok_or_else(||refuse("logical source does not exist"))?;
    if row.0 != kind || !row.4 || !matches!(row.1.as_str(), "present" | "absent") {
        return Err(refuse("incompatible logical source kind"));
    }
    Ok(LogicalBase {
        id: source,
        kind: row.0,
        state: row.1,
        location: row.2,
        created_at: row.3,
    })
}
fn version(conn: &Connection, source: Uuid) -> Result<(Version, String)> {
    let length: i64 = conn
        .query_row(
            "SELECT octet_length(content) FROM sources WHERE id=?1",
            [source.to_string()],
            |r| r.get(0),
        )
        .optional()
        .map_err(dberr)?
        .ok_or_else(|| refuse("source version does not exist"))?;
    if number(length)? > MAX_CANONICAL_TEXT_BYTES as u64 {
        return Err(refuse("selected source content exceeds the document bound"));
    }
    let (logical,kind,location,title,content,hash,media,size,at)=conn.query_row("SELECT logical_id,kind,location,title,content,content_hash,media_type,size_bytes,ingested_at FROM sources WHERE id=?1",[source.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,i64>(7)?,r.get::<_,i64>(8)?))).map_err(dberr)?;
    if number(size)? != content.len() as u64 || sha256_hex(content.as_bytes()) != hash {
        return Err(refuse("stored source version hash or length is corrupt"));
    }
    Ok((
        Version {
            id: source,
            logical_id: id(logical)?,
            kind,
            location,
            title,
            hash,
            media_type: media,
            size: number(size)?,
            ingested_at: at,
        },
        content,
    ))
}
fn extraction(conn: &Connection, selected: Uuid) -> Result<Extraction> {
    conn.query_row("SELECT logical_id,source_id,source_bytes_hash,source_size,text_is_source,format,format_hint,detector_version,extractor_id,extractor_version,params_hash,structure_hash,node_count,created_at FROM doc_extractions WHERE id=?1",[selected.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,String>(5)?,r.get::<_,Option<String>>(6)?,r.get::<_,String>(7)?,r.get::<_,String>(8)?,r.get::<_,String>(9)?,r.get::<_,String>(10)?,r.get::<_,String>(11)?,r.get::<_,i64>(12)?,r.get::<_,i64>(13)?))).optional().map_err(dberr)?.ok_or_else(||refuse("extraction does not exist")).and_then(|r|{
        if !matches!(r.4,0|1){return Err(refuse("invalid extraction text-is-source value"))}
        Ok(Extraction{id:selected,logical_id:id(r.0)?,source_id:id(r.1)?,bytes_hash:r.2,source_size:number(r.3)?,text_is_source:r.4==1,format:r.5,format_hint:r.6,detector_version:r.7,extractor_id:r.8,extractor_version:r.9,params_hash:r.10,structure_hash:r.11,node_count:number(r.12)?,created_at:r.13})
    })
}
fn nodes(conn: &Connection, x: &Extraction) -> Result<Vec<DocNodeDraft>> {
    if x.node_count > MAX_NODES as u64 {
        return Err(refuse("too many extraction nodes"));
    }
    let mut statement=conn.prepare("SELECT path,kind,text_start,text_end,page_no,bbox_x0,bbox_y0,bbox_x1,bbox_y1,precision FROM doc_nodes WHERE extraction_id=?1 ORDER BY path LIMIT ?2").map_err(dberr)?;
    let rows = statement
        .query_map(
            rusqlite::params![x.id.to_string(), MAX_NODES as i64 + 1],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<f64>>(5)?,
                    r.get::<_, Option<f64>>(6)?,
                    r.get::<_, Option<f64>>(7)?,
                    r.get::<_, Option<f64>>(8)?,
                    r.get::<_, String>(9)?,
                ))
            },
        )
        .map_err(dberr)?;
    let mut nodes = Vec::new();
    for row in rows {
        let (p, k, a, b, page, x0, y0, x1, y1, precision) = row.map_err(dberr)?;
        let path = p
            .split('.')
            .map(|n| {
                n.parse::<u32>()
                    .map_err(|_| refuse("invalid stored node path"))
            })
            .collect::<Result<Vec<_>>>()?;
        if path
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(".")
            != p
        {
            return Err(refuse("noncanonical stored node path"));
        }
        let text_range = match (a, b) {
            (Some(a), Some(b)) => Some((number(a)?, number(b)?)),
            (None, None) => None,
            _ => return Err(refuse("invalid node range")),
        };
        let page = page
            .map(|p| u32::try_from(p).map_err(|_| refuse("invalid node page")))
            .transpose()?;
        let bbox = match (x0, y0, x1, y1) {
            (Some(a), Some(b), Some(c), Some(d)) => Some(BBox {
                x0: a as f32,
                y0: b as f32,
                x1: c as f32,
                y1: d as f32,
            }),
            (None, None, None, None) => None,
            _ => return Err(refuse("invalid node bounding box")),
        };
        nodes.push(DocNodeDraft {
            path,
            kind: NodeKind::parse(&k).ok_or_else(|| refuse("invalid node kind"))?,
            text_range,
            page,
            bbox,
            precision: AnchorPrecision::parse(&precision)
                .ok_or_else(|| refuse("invalid node precision"))?,
        });
    }
    if nodes.len() as u64 != x.node_count {
        return Err(refuse("stored extraction node count is corrupt"));
    }
    Ok(nodes)
}
fn snapshot_with_draft(
    conn: &Connection,
    target: &PreparationTarget,
) -> Result<(Snapshot, DocumentExtractionDraft)> {
    let (f,legacy_operation_id,legacy_revision)=conn.query_row("SELECT a.source_id,a.operation_id,a.created_revision FROM asset_ownership a JOIN asset_control_intents r ON r.operation_id=a.operation_id AND r.asset_id=a.asset_id AND r.source_id=a.source_id AND r.committed_revision=a.created_revision JOIN asset_ownership_history h ON h.asset_id=a.asset_id AND h.source_id=a.source_id AND h.operation_id=a.operation_id AND h.revision_id=a.created_revision WHERE a.asset_id=?1 AND a.owner='legacy' AND a.epoch=1",[target.asset_id.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?))).optional().map_err(dberr)?.ok_or_else(||refuse("existing legacy asset enrollment required"))?;
    let f = id(f)?;
    let legacy_base = logical(conn, f, "file")?;
    let document_base = logical(conn, target.document_id, "document")?;
    let (legacy_version, original) = version(conn, target.legacy_source_version)?;
    drop(original);
    let (document_version, text) = version(conn, target.document_source_version)?;
    let x = extraction(conn, target.extraction_id)?;
    let draft = DocumentExtractionDraft {
        location: document_version.location.clone(),
        title: document_version.title.clone(),
        media_type: document_version.media_type.clone(),
        format: x.format.clone(),
        format_hint: x.format_hint.clone(),
        detector_version: x.detector_version.clone(),
        extractor_id: x.extractor_id.clone(),
        extractor_version: x.extractor_version.clone(),
        params_hash: x.params_hash.clone(),
        source_bytes_hash: x.bytes_hash.clone(),
        source_size: x.source_size,
        text_is_source: x.text_is_source,
        canonical_text: text,
        nodes: nodes(conn, &x)?,
        retain_bytes: None,
        observed_at: Ts(x.created_at),
    };
    draft.validate()?;
    if draft.structure_hash() != x.structure_hash {
        return Err(refuse("stored extraction structure hash is corrupt"));
    }
    let value = Snapshot {
        target: target.clone(),
        legacy_source_id: f,
        legacy_operation_id,
        legacy_revision,
        legacy_base,
        document_base,
        legacy_version,
        document_version,
        extraction: x,
    };
    value.validate_links()?;
    Ok((value, draft))
}

fn snapshot(conn: &Connection, target: &PreparationTarget) -> Result<Snapshot> {
    snapshot_with_draft(conn, target).map(|(snapshot, _)| snapshot)
}

/// A single hash-checked source/structure snapshot, shared by all staged citations.
pub(super) struct ValidatedStagingSnapshot {
    pub source_id: Uuid,
    pub provenance: Vec<u8>,
    pub logical_base: Vec<u8>,
    pub draft: DocumentExtractionDraft,
}
pub(super) fn validated_staging_snapshot(
    conn: &Connection,
    target: &PreparationTarget,
) -> Result<ValidatedStagingSnapshot> {
    let (snapshot, draft) = snapshot_with_draft(conn, target)?;
    let mut logical_base = Vec::new();
    snapshot.legacy_base.canonical(&mut logical_base);
    snapshot.document_base.canonical(&mut logical_base);
    let mut provenance = Vec::new();
    string(&mut provenance, "mindspace.s3d.selected-provenance@1");
    for v in [&snapshot.legacy_version, &snapshot.document_version] {
        provenance.extend_from_slice(v.id.as_bytes());
        provenance.extend_from_slice(v.logical_id.as_bytes());
        for field in [&v.kind, &v.location, &v.title, &v.hash, &v.media_type] {
            string(&mut provenance, field);
        }
        provenance.extend_from_slice(&v.size.to_be_bytes());
    }
    let x = &snapshot.extraction;
    for v in [x.id, x.logical_id, x.source_id] {
        provenance.extend_from_slice(v.as_bytes());
    }
    string(&mut provenance, &x.bytes_hash);
    provenance.extend_from_slice(&x.source_size.to_be_bytes());
    provenance.push(u8::from(x.text_is_source));
    string(&mut provenance, &x.format);
    optional(&mut provenance, &x.format_hint);
    for field in [
        &x.detector_version,
        &x.extractor_id,
        &x.extractor_version,
        &x.params_hash,
        &x.structure_hash,
    ] {
        string(&mut provenance, field);
    }
    provenance.extend_from_slice(&x.node_count.to_be_bytes());
    Ok(ValidatedStagingSnapshot {
        source_id: snapshot.legacy_source_id,
        provenance,
        logical_base,
        draft,
    })
}

/// Test fixtures use exactly the production receipt/permit constraints, without a CLI principal.
#[cfg(test)]
pub(super) fn enroll_test_preparation(store: &SqliteStore, target: &PreparationTarget) {
    let conn = store.connection().unwrap();
    let tx = begin_write(&conn, &store.revision_state).unwrap();
    let revision = store.revision_state.active_revision.load(Ordering::SeqCst);
    let intent = Intent {
        nonce: Uuid::now_v7(),
        db_path: "/unit-fixture.db".into(),
        db_dev: 1,
        db_ino: 1,
        actor_uid: 1000,
        expected_revision: revision - 1,
        snapshot: snapshot(&tx, target).unwrap(),
    };
    let operation = intent.operation_id().unwrap();
    *store.preparation_state.permit.lock().unwrap() = Some(Permit {
        enrollment: intent.enrollment_values(&operation, revision),
        receipt: intent.receipt_values(&operation, revision).unwrap(),
    });
    let _scope = PermitScope(&store.preparation_state);
    insert_values(
        &tx,
        "document_preparation_enrollments",
        ENROLL_COLUMNS,
        &intent.enrollment_values(&operation, revision),
    )
    .unwrap();
    insert_values(
        &tx,
        "document_preparation_receipts",
        RECEIPT_COLUMNS,
        &intent.receipt_values(&operation, revision).unwrap(),
    )
    .unwrap();
    tx.commit().unwrap();
}

#[cfg(test)]
pub(super) fn prepared_test_fixture() -> (SqliteStore, PreparationTarget) {
    let (store, target) = tests::fixture();
    enroll_test_preparation(&store, &target);
    (store, target)
}

#[derive(Serialize)]
pub(crate) struct PreparationPreview {
    pub operation_id: String,
    pub digest: String,
    #[serde(flatten)]
    pub target: PreparationTarget,
    pub source_id: Uuid,
    pub expected_revision: i64,
    pub snapshot_digest: String,
    pub db_path: String,
    pub db_dev: u64,
    pub db_ino: u64,
    pub actor_kind: &'static str,
    pub actor_uid: u32,
    snapshot: Snapshot,
}
#[derive(Serialize)]
pub(crate) struct PreparationReceipt {
    pub operation_id: String,
    pub digest: String,
    #[serde(flatten)]
    pub target: PreparationTarget,
    pub source_id: Uuid,
    pub snapshot_digest: String,
    pub committed_revision: i64,
    pub actor_kind: &'static str,
    pub actor_uid: u32,
    pub db_path: String,
    pub db_dev: u64,
    pub db_ino: u64,
}
fn receipt(i: &Intent, revision: i64) -> Result<PreparationReceipt> {
    Ok(PreparationReceipt {
        operation_id: i.operation_id()?,
        digest: i.digest()?,
        target: i.snapshot.target.clone(),
        source_id: i.snapshot.legacy_source_id,
        snapshot_digest: i.snapshot.digest(),
        committed_revision: revision,
        actor_kind: ACTOR,
        actor_uid: i.actor_uid,
        db_path: i.db_path.clone(),
        db_dev: i.db_dev,
        db_ino: i.db_ino,
    })
}
struct Stored {
    intent: Intent,
    revision: i64,
}
fn stored(conn: &Connection, operation: &str) -> Result<Option<Stored>> {
    let values = conn
        .query_row(
            &format!(
                "SELECT {RECEIPT_COLUMNS} FROM document_preparation_receipts WHERE operation_id=?1"
            ),
            [operation],
            |r| {
                (0..23)
                    .map(|n| r.get::<_, Value>(n))
                    .collect::<rusqlite::Result<Vec<_>>>()
            },
        )
        .optional()
        .map_err(dberr)?;
    let Some(values) = values else {
        return Ok(None);
    };
    let (Value::Blob(json), Value::Integer(revision)) = (&values[4], &values[19]) else {
        return Err(refuse("invalid stored receipt types"));
    };
    if json.len() > MAX_REQUEST {
        return Err(refuse("stored intent exceeds limit"));
    }
    let intent: Intent =
        serde_json::from_slice(json).map_err(|_| refuse("invalid stored intent encoding"))?;
    if intent.operation_id()? != operation
        || intent.expected_revision.checked_add(1) != Some(*revision)
        || intent.receipt_values(operation, *revision)? != values
    {
        return Err(refuse("stored receipt conflicts with immutable intent"));
    }
    let enrollment = conn
        .query_row(
            &format!(
                "SELECT {ENROLL_COLUMNS} FROM document_preparation_enrollments WHERE asset_id=?1"
            ),
            [intent.snapshot.target.asset_id.to_string()],
            |r| {
                (0..12)
                    .map(|n| r.get::<_, Value>(n))
                    .collect::<rusqlite::Result<Vec<_>>>()
            },
        )
        .optional()
        .map_err(dberr)?;
    if enrollment.as_ref() != Some(&intent.enrollment_values(operation, *revision)) {
        return Err(refuse("stored enrollment and receipt disagree"));
    }
    Ok(Some(Stored {
        intent,
        revision: *revision,
    }))
}
#[cfg(unix)]
fn replay(
    stored: Stored,
    ctx: &AdminContext,
    target: &PreparationTarget,
    expected: i64,
) -> Result<PreparationReceipt> {
    let i = &stored.intent;
    if &i.snapshot.target != target
        || i.expected_revision != expected
        || i.actor_uid != ctx.uid()
        || i.db_path != ctx.path().to_str().unwrap_or("")
        || i.db_dev != ctx.dev()
        || i.db_ino != ctx.ino()
    {
        return Err(refuse(
            "operation replay conflicts with immutable parameters",
        ));
    }
    receipt(i, stored.revision)
}
#[cfg(unix)]
fn intent(ctx: &AdminContext, nonce: Uuid, expected: i64, snapshot: Snapshot) -> Result<Intent> {
    Ok(Intent {
        nonce,
        db_path: ctx
            .path()
            .to_str()
            .ok_or_else(|| refuse("database path must be UTF-8"))?
            .to_owned(),
        db_dev: ctx.dev(),
        db_ino: ctx.ino(),
        actor_uid: ctx.uid(),
        expected_revision: expected,
        snapshot,
    })
}
fn conflicts(conn: &Connection, target: &PreparationTarget) -> Result<bool> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM document_preparation_enrollments WHERE asset_id=?1 OR document_id=?2)",rusqlite::params![target.asset_id.to_string(),target.document_id.to_string()],|r|r.get(0)).map_err(dberr)
}
fn insert_values(conn: &Connection, table: &str, columns: &str, values: &[Value]) -> Result<()> {
    let placeholders = (1..=values.len())
        .map(|n| format!("?{n}"))
        .collect::<Vec<_>>()
        .join(",");
    conn.execute(
        &format!("INSERT INTO {table}({columns}) VALUES({placeholders})"),
        rusqlite::params_from_iter(values),
    )
    .map_err(dberr)?;
    Ok(())
}
#[cfg(unix)]
impl SqliteStore {
    pub(crate) fn preparation_preview(
        &self,
        ctx: &AdminContext,
        target: &PreparationTarget,
        expected: i64,
        nonce: Uuid,
    ) -> Result<PreparationPreview> {
        ctx.validate()?;
        let conn = self.connection()?;
        ownership::validate_connection(&conn, ctx)?;
        let read =
            Transaction::new_unchecked(&conn, TransactionBehavior::Deferred).map_err(dberr)?;
        let current: i64 = read
            .query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| {
                r.get(0)
            })
            .map_err(dberr)?;
        if current != expected {
            return Err(refuse("stale expected recording revision"));
        }
        if conflicts(&read, target)? {
            return Err(refuse("asset or document is already enrolled"));
        }
        let i = intent(ctx, nonce, expected, snapshot(&read, target)?)?;
        // Ensure both canonical and durable codecs fit before confirming an intent.
        i.receipt_values(
            &i.operation_id()?,
            expected
                .checked_add(1)
                .ok_or_else(|| refuse("revision overflow"))?,
        )?;
        let out = PreparationPreview {
            operation_id: i.operation_id()?,
            digest: i.digest()?,
            target: target.clone(),
            source_id: i.snapshot.legacy_source_id,
            expected_revision: expected,
            snapshot_digest: i.snapshot.digest(),
            db_path: i.db_path,
            db_dev: i.db_dev,
            db_ino: i.db_ino,
            actor_kind: ACTOR,
            actor_uid: i.actor_uid,
            snapshot: i.snapshot,
        };
        read.rollback().map_err(dberr)?;
        Ok(out)
    }
    pub(crate) fn preparation_enroll(
        &self,
        ctx: &AdminContext,
        target: &PreparationTarget,
        expected: i64,
        operation: &str,
        confirmation: &str,
    ) -> Result<PreparationReceipt> {
        ctx.validate()?;
        let (nonce, digest) = parse_id(operation)?;
        if confirmation != digest {
            return Err(refuse("exact intent confirmation required"));
        }
        let conn = self.connection()?;
        ownership::validate_connection(&conn, ctx)?;
        if let Some(value) = stored(&conn, operation)? {
            return replay(value, ctx, target, expected);
        }
        #[cfg(feature = "spawn-gate")]
        test_pause("before_begin");
        let tx = begin_write(&conn, &self.revision_state)?;
        #[cfg(feature = "spawn-gate")]
        test_pause("after_begin");
        if let Some(value) = stored(&tx, operation)? {
            return replay(value, ctx, target, expected);
        }
        let revision = self.revision_state.active_revision.load(Ordering::SeqCst);
        if revision.checked_sub(1) != Some(expected) {
            return Err(refuse("stale expected recording revision"));
        }
        if tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM document_preparation_receipts WHERE nonce=?1)",
                [nonce.to_string()],
                |r| r.get::<_, bool>(0),
            )
            .map_err(dberr)?
        {
            return Err(refuse("committed nonce has different intent"));
        }
        if conflicts(&tx, target)? {
            return Err(refuse("asset or document is already enrolled"));
        }
        let i = intent(ctx, nonce, expected, snapshot(&tx, target)?)?;
        if i.operation_id()? != operation {
            return Err(refuse(
                "operation identity conflicts with immutable parameters",
            ));
        }
        let enrollment = i.enrollment_values(operation, revision);
        let values = i.receipt_values(operation, revision)?;
        {
            let mut p = self
                .preparation_state
                .permit
                .lock()
                .map_err(|_| refuse("administrative scope poisoned"))?;
            if p.is_some() {
                return Err(refuse("nested administrative scope"));
            }
            *p = Some(Permit {
                enrollment: enrollment.clone(),
                receipt: values.clone(),
            });
        }
        let scope = PermitScope(&self.preparation_state);
        insert_values(
            &tx,
            "document_preparation_enrollments",
            ENROLL_COLUMNS,
            &enrollment,
        )?;
        insert_values(
            &tx,
            "document_preparation_receipts",
            RECEIPT_COLUMNS,
            &values,
        )?;
        #[cfg(feature = "spawn-gate")]
        test_pause("after_write");
        #[cfg(feature = "spawn-gate")]
        test_pause("before_commit");
        ctx.validate()?;
        #[cfg(feature = "spawn-gate")]
        {
            match std::env::var("MINDSPACE_PREPARATION_FAULT").as_deref() {
                Ok("veto") => gate::FAIL_COMMIT.store(true, Ordering::SeqCst),
                Ok("lost_ack") => gate::LOSE_ACK.store(true, Ordering::SeqCst),
                _ => {}
            }
            if std::env::var("MINDSPACE_PREPARATION_PAUSE").as_deref() == Ok("in_commit") {
                if let Some(prefix) = std::env::var_os("MINDSPACE_PREPARATION_GATE_PATH") {
                    std::env::set_var("MINDSPACE_STORE_GATE_PAUSE_IN_COMMIT", prefix);
                    gate::PAUSE_IN_COMMIT.store(true, Ordering::SeqCst);
                }
            }
        }
        if let Err(error) = tx.commit() {
            if matches!(error, Error::CommitOutcomeUnknown { .. }) {
                drop(scope);
                drop(conn);
                if let Ok(mut connection) = self.conn.lock() {
                    connection.take();
                }
            }
            return Err(error);
        }
        #[cfg(feature = "spawn-gate")]
        test_pause("after_commit");
        receipt(&i, revision)
    }
}
#[cfg(feature = "spawn-gate")]
fn test_pause(phase: &str) {
    if std::env::var("MINDSPACE_PREPARATION_PAUSE").as_deref() != Ok(phase) {
        return;
    }
    let Some(prefix) = std::env::var_os("MINDSPACE_PREPARATION_GATE_PATH") else {
        return;
    };
    let with = |suffix: &str| {
        let mut p = prefix.clone();
        p.push(suffix);
        std::path::PathBuf::from(p)
    };
    let _ = std::fs::write(with(".reached"), b"");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !with(".go").exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[cfg(test)]
#[path = "document_preparation_tests.rs"]
mod tests;
