//! Bounded, byte-exact lexical inputs. No model, live-file lookup, or retrieval mutation.
use super::document_preparation::{self, PreparationTarget};
use super::documents::{self, ChunkIdentity, PreparedChunk};
use super::{dberr, Connection};
use crate::core::{sha256_hex, Error, NodeKind, Result, Uuid};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(super) const MAX_TEXT: u64 = 16 * 1024 * 1024;
pub(super) const MAX_ENTRIES: u64 = 8192;
pub(super) const MAX_METADATA: u64 = 16 * 1024 * 1024;
const MAX_CANDIDATE: u64 = 64 * 1024 * 1024;
const MAX_INPUT: usize = 65536;
const CITATION_POLICY: &str = "s1-root-byte-exact@1;canonical_verified;original_bytes_not_checked";
fn refuse(why: &str) -> Error {
    Error::Storage(format!("lexical staging refused: {why}"))
}
fn string(out: &mut Vec<u8>, value: &str) {
    bytes(out, value.as_bytes());
}
fn bytes(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value);
}
fn digest_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LexicalStageTarget {
    pub asset_id: Uuid,
    pub legacy_source_version: Uuid,
    pub document_id: Uuid,
    pub document_source_version: Uuid,
    pub extraction_id: Uuid,
    pub chunker_version: String,
    pub budget_identity: String,
    pub plan_hash: String,
}
impl LexicalStageTarget {
    fn canonical(&self, out: &mut Vec<u8>) {
        for id in [
            self.asset_id,
            self.legacy_source_version,
            self.document_id,
            self.document_source_version,
            self.extraction_id,
        ] {
            out.extend_from_slice(id.as_bytes());
        }
        for s in [
            &self.chunker_version,
            &self.budget_identity,
            &self.plan_hash,
        ] {
            string(out, s);
        }
    }
    fn validate(&self) -> Result<()> {
        if self.chunker_version != "heading_aware@1"
            || self.budget_identity.is_empty()
            || self.budget_identity.len() > 2048
            || !digest_valid(&self.plan_hash)
        {
            return Err(refuse("unknown chunk codec or invalid chunk-set key"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct LexicalInput {
    pub target: LexicalStageTarget,
    pub source_id: Uuid,
    pub preparation_operation_id: String,
    pub preparation_revision: i64,
    pub provenance: Vec<u8>,
    pub logical_base: Vec<u8>,
    pub input_manifest: String,
    pub stage_id: String,
    pub source_bytes_len: u64,
    pub entry_count: u64,
    pub metadata_bytes: u64,
    pub lexical_descriptor: String,
    pub citation_policy: String,
}
impl LexicalInput {
    /// Acyclic artifact input; mutable logical bases and recording revisions belong to intent only.
    pub fn artifact_canonical(&self) -> Vec<u8> {
        let mut out = Vec::new();
        string(&mut out, "mindspace.s3d.lexical-artifact@1");
        self.target.canonical(&mut out);
        out.extend_from_slice(self.source_id.as_bytes());
        string(&mut out, &self.preparation_operation_id);
        bytes(&mut out, &self.provenance);
        for s in [
            &self.input_manifest,
            &self.lexical_descriptor,
            &self.citation_policy,
        ] {
            string(&mut out, s);
        }
        out.extend_from_slice(&self.source_bytes_len.to_be_bytes());
        out.extend_from_slice(&self.entry_count.to_be_bytes());
        out
    }
    pub fn validate(&self) -> Result<()> {
        self.target.validate()?;
        if self.preparation_revision<=0 || self.preparation_operation_id.is_empty() || self.provenance.is_empty() || self.logical_base.is_empty()
            || self.source_id==self.target.document_id || (self.entry_count==0)!=(self.source_bytes_len==0) || !digest_valid(&self.input_manifest)
            || !digest_valid(&self.stage_id) || self.stage_id!=sha256_hex(&self.artifact_canonical())
            || self.source_bytes_len>MAX_TEXT || self.entry_count>MAX_ENTRIES || self.metadata_bytes>MAX_METADATA
            || self.source_bytes_len+self.metadata_bytes>MAX_CANDIDATE
            || self.citation_policy!=CITATION_POLICY || !self.lexical_descriptor.starts_with("sqlite-fts5@1;unicode61 remove_diacritics 1;content-only;detail=full;prefix=none;sqlite=") {
            return Err(refuse("invalid bounded artifact input"));
        }
        let encoded = serde_json::to_vec(self).map_err(|_| refuse("cannot encode input"))?;
        if encoded.len() > MAX_INPUT {
            return Err(refuse("input metadata exceeds 64 KiB"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct LexicalEntry {
    pub entry_id: String,
    pub ordinal: u64,
    pub chunk_id: String,
    pub start: u64,
    pub end: u64,
    pub content: String,
    pub payload_hash: String,
    pub anchor: String,
    pub context_json: String,
    pub citation_state: String,
    pub original_bytes_state: String,
}
impl LexicalEntry {
    fn pre_entry(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.ordinal.to_be_bytes());
        string(out, &self.chunk_id);
        out.extend_from_slice(&self.start.to_be_bytes());
        out.extend_from_slice(&self.end.to_be_bytes());
        for s in [&self.payload_hash, &self.anchor, &self.context_json] {
            string(out, s);
        }
    }
    fn identity(&self, stage: &str) -> String {
        let mut out = Vec::new();
        string(&mut out, "mindspace.s3d.lexical-entry@1");
        string(&mut out, stage);
        self.pre_entry(&mut out);
        sha256_hex(&out)
    }
}
pub(super) struct LexicalCandidate {
    pub input: LexicalInput,
    pub entries: Vec<LexicalEntry>,
}
impl LexicalCandidate {
    pub fn result_digest(&self) -> String {
        result_digest(&self.entries)
    }
}
pub(super) fn result_digest(entries: &[LexicalEntry]) -> String {
    let mut hash = Sha256::new();
    hash_string(&mut hash, "mindspace.s3d.lexical-result@1");
    hash.update((entries.len() as u64).to_be_bytes());
    for entry in entries {
        hash_string(&mut hash, &entry.entry_id);
        let mut pre_entry = Vec::new();
        entry.pre_entry(&mut pre_entry);
        hash.update(pre_entry);
        for field in [
            &entry.content,
            &entry.citation_state,
            &entry.original_bytes_state,
        ] {
            hash_string(&mut hash, field);
        }
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
fn hash_string(hash: &mut Sha256, value: &str) {
    hash.update((value.len() as u64).to_be_bytes());
    hash.update(value.as_bytes());
}

pub(super) fn build(conn: &Connection, target: &LexicalStageTarget) -> Result<LexicalCandidate> {
    target.validate()?;
    let text_len: i64 = conn
        .query_row(
            "SELECT octet_length(content) FROM sources WHERE id=?1",
            [target.document_source_version.to_string()],
            |r| r.get(0),
        )
        .optional()
        .map_err(dberr)?
        .ok_or_else(|| refuse("missing canonical source"))?;
    if !(0..=MAX_TEXT as i64).contains(&text_len) {
        return Err(refuse("canonical text exceeds 16 MiB"));
    }
    let (operation,revision,source):(String,i64,String)=conn.query_row(
        "SELECT p.operation_id,p.created_revision,p.source_id FROM document_preparation_enrollments p JOIN document_preparation_receipts r ON r.operation_id=p.operation_id AND r.asset_id=p.asset_id AND r.source_id=p.source_id AND r.document_id=p.document_id AND r.committed_revision=p.created_revision WHERE p.asset_id=?1 AND p.document_id=?2 AND p.state='document_preparation_enrolled'",
        params![target.asset_id.to_string(),target.document_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(dberr)?.ok_or_else(||refuse("matching prepared association required"))?;
    let selected = document_preparation::validated_staging_snapshot(
        conn,
        &PreparationTarget {
            asset_id: target.asset_id,
            legacy_source_version: target.legacy_source_version,
            document_id: target.document_id,
            document_source_version: target.document_source_version,
            extraction_id: target.extraction_id,
        },
    )?;
    if source != selected.source_id.to_string() {
        return Err(refuse("prepared legacy identity differs"));
    }
    let (plan,count):(String,i64)=conn.query_row("SELECT plan_hash,chunk_count FROM doc_chunk_sets WHERE extraction_id=?1 AND version=?2 AND budget_identity=?3",params![target.extraction_id.to_string(),target.chunker_version,target.budget_identity],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(dberr)?.ok_or_else(||refuse("selected chunk set does not exist"))?;
    if plan != target.plan_hash || !(0..=MAX_ENTRIES as i64).contains(&count) {
        return Err(refuse("chunk plan differs or entry bound exceeded"));
    }
    // Metadata byte-length uses SQLite metadata, before allocating attacker-controlled row strings.
    let metadata:i64=conn.query_row("SELECT COALESCE(SUM(octet_length(id)+octet_length(anchor)+octet_length(context_json)),0) FROM doc_chunks WHERE extraction_id=?1 AND version=?2 AND budget_identity=?3",params![target.extraction_id.to_string(),target.chunker_version,target.budget_identity],|r|r.get(0)).map_err(dberr)?;
    if !(0..=MAX_METADATA as i64).contains(&metadata) {
        return Err(refuse("chunk metadata exceeds 16 MiB"));
    }
    let draft = &selected.draft;
    let text_hash = draft.text_hash();
    let structure_hash = draft.structure_hash();
    let identity = ChunkIdentity {
        logical_id: target.document_id,
        draft,
        text_hash: &text_hash,
        structure_hash: &structure_hash,
        version: &target.chunker_version,
        budget_identity: &target.budget_identity,
    };
    // One sorted section-event pass, bounded by the validated structural depth, never nodes × chunks.
    let mut sections = draft
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Section)
        .filter_map(|n| n.text_range.map(|(a, b)| (a, b, n.path.clone())))
        .filter(|(a, b, _)| a < b)
        .collect::<Vec<_>>();
    sections.sort_by(|(a, b, p), (c, d, q)| {
        a.cmp(c)
            .then_with(|| d.cmp(b))
            .then_with(|| p.len().cmp(&q.len()))
    });
    let mut stack: Vec<(u64, Vec<u32>)> = Vec::new();
    for (a, b, path) in &sections {
        while stack.last().is_some_and(|(end, _)| end <= a) {
            stack.pop();
        }
        if stack.last().is_some_and(|(end, parent)| {
            b > end || !path.starts_with(parent) || path.len() <= parent.len()
        }) {
            return Err(refuse("section ranges do not form a hierarchy"));
        }
        stack.push((*b, path.clone()));
    }
    let mut events = sections.into_iter().peekable();
    let mut active = BTreeMap::<Vec<u32>, u64>::new();
    let mut stmt=conn.prepare("SELECT id,ordinal,text_start,text_end,token_count,context_json,anchor FROM doc_chunks WHERE extraction_id=?1 AND version=?2 AND budget_identity=?3 ORDER BY ordinal LIMIT 8193").map_err(dberr)?;
    let mut rows = stmt
        .query(params![
            target.extraction_id.to_string(),
            target.chunker_version,
            target.budget_identity
        ])
        .map_err(dberr)?;
    let mut entries = Vec::new();
    let mut codec = Vec::new();
    let mut end = 0u64;
    let mut metadata_bytes = 0u64;
    while let Some(row) = rows.next().map_err(dberr)? {
        let ordinal: i64 = row.get(1).map_err(dberr)?;
        let start: i64 = row.get(2).map_err(dberr)?;
        let stop: i64 = row.get(3).map_err(dberr)?;
        let tokens: i64 = row.get(4).map_err(dberr)?;
        if entries.len() as u64 >= MAX_ENTRIES
            || ordinal != entries.len() as i64
            || start < 0
            || stop <= start
            || start as u64 != end
            || tokens <= 0
        {
            return Err(refuse("chunk ordinals or canonical coverage are invalid"));
        }
        let context_json: String = row.get(5).map_err(dberr)?;
        let context: Vec<Vec<u32>> =
            serde_json::from_str(&context_json).map_err(|_| refuse("invalid chunk context"))?;
        if serde_json::to_string(&context).map_err(|_| refuse("invalid chunk context"))?
            != context_json
        {
            return Err(refuse("noncanonical chunk context"));
        }
        while events.peek().is_some_and(|(a, _, _)| *a <= start as u64) {
            let (_, b, p) = events.next().unwrap();
            active.insert(p, b);
        }
        active.retain(|_, b| *b > start as u64);
        let expected = active
            .iter()
            .filter(|(_, b)| **b >= stop as u64)
            .map(|(p, _)| p.clone())
            .collect::<Vec<_>>();
        if context != expected {
            return Err(refuse("chunk context does not name its enclosing sections"));
        }
        let c = PreparedChunk {
            start: start as u64,
            end: stop as u64,
            tokens: usize::try_from(tokens).map_err(|_| refuse("invalid token provenance"))?,
            context,
        };
        let chunk_id: String = row.get(0).map_err(dberr)?;
        if chunk_id != documents::chunk_identity(&identity, &c) {
            return Err(refuse("stored chunk identity is corrupt"));
        }
        let anchor: String = row.get(6).map_err(dberr)?;
        let content =
            documents::verify_root_chunk(target.document_id, draft, (c.start, c.end), &anchor)?;
        let entry = LexicalEntry {
            entry_id: String::new(),
            ordinal: ordinal as u64,
            chunk_id,
            start: c.start,
            end: c.end,
            content: content.to_owned(),
            payload_hash: sha256_hex(content.as_bytes()),
            anchor,
            context_json,
            citation_state: "canonical_verified".into(),
            original_bytes_state: "not_checked".into(),
        };
        // Fixed IDs/statuses/counts plus variable anchor/context are admitted as metadata.
        metadata_bytes = metadata_bytes
            .checked_add(
                (entry.anchor.len()
                    + entry.context_json.len()
                    + entry.chunk_id.len()
                    + 64
                    + 64
                    + 64) as u64,
            )
            .ok_or_else(|| refuse("metadata overflow"))?;
        if metadata_bytes > MAX_METADATA || text_len as u64 + metadata_bytes > MAX_CANDIDATE {
            return Err(refuse("candidate metadata bound exceeded"));
        }
        end = c.end;
        codec.push(c);
        entries.push(entry);
    }
    if end != draft.canonical_text.len() as u64
        || entries.len() != count as usize
        || documents::chunk_plan_hash(&target.chunker_version, &target.budget_identity, &codec)
            != target.plan_hash
    {
        return Err(refuse("incomplete or corrupt canonical chunk set"));
    }
    drop(codec);
    let mut manifest = Sha256::new();
    hash_string(&mut manifest, "mindspace.s3d.lexical-input-manifest@1");
    manifest.update((entries.len() as u64).to_be_bytes());
    manifest.update(end.to_be_bytes());
    for entry in &entries {
        let mut pre_entry = Vec::new();
        entry.pre_entry(&mut pre_entry);
        manifest.update(pre_entry);
    }
    let input_manifest = manifest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let sqlite: String = conn
        .query_row("SELECT sqlite_version()", [], |r| r.get(0))
        .map_err(dberr)?;
    let mut input=LexicalInput { target:target.clone(),source_id:selected.source_id,preparation_operation_id:operation,preparation_revision:revision,provenance:selected.provenance,logical_base:selected.logical_base,input_manifest,stage_id:String::new(),source_bytes_len:end,entry_count:entries.len() as u64,metadata_bytes,lexical_descriptor:format!("sqlite-fts5@1;unicode61 remove_diacritics 1;content-only;detail=full;prefix=none;sqlite={sqlite}"),citation_policy:CITATION_POLICY.into() };
    input.stage_id = sha256_hex(&input.artifact_canonical());
    input.validate()?;
    for entry in &mut entries {
        entry.entry_id = entry.identity(&input.stage_id);
    }
    Ok(LexicalCandidate { input, entries })
}

/// Reconstruct an immutable canonical parent using its authenticated historical policy.
/// Only the known lexical@1 definition is supported; this does not attest cache health.
/// Fresh lexical builds deliberately continue to use the currently linked SQLite version.
pub(super) fn pin_historical_descriptor(
    candidate: &mut LexicalCandidate,
    descriptor: &str,
) -> Result<()> {
    const PREFIX: &str =
        "sqlite-fts5@1;unicode61 remove_diacritics 1;content-only;detail=full;prefix=none;sqlite=";
    let version = descriptor
        .strip_prefix(PREFIX)
        .ok_or_else(|| refuse("unsupported historical lexical descriptor"))?;
    if version.len() > 20 {
        return Err(refuse("unsupported historical SQLite descriptor"));
    }
    let parts = version
        .split('.')
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| refuse("invalid historical SQLite descriptor"))?;
    if parts.len() != 3
        || parts[0] != 3
        || format!("{}.{}.{}", parts[0], parts[1], parts[2]) != version
    {
        return Err(refuse("unsupported historical SQLite descriptor"));
    }
    candidate.input.lexical_descriptor = descriptor.to_owned();
    candidate.input.stage_id = sha256_hex(&candidate.input.artifact_canonical());
    candidate.input.validate()?;
    for entry in &mut candidate.entries {
        entry.entry_id = entry.identity(&candidate.input.stage_id);
    }
    Ok(())
}

#[cfg(test)]
#[path = "lexical_input_tests.rs"]
mod tests;

#[cfg(test)]
pub(super) fn chunked_test_fixture() -> (super::SqliteStore, LexicalStageTarget) {
    tests::fixture()
}
