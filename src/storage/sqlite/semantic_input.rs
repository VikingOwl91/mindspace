//! Immutable semantic inputs and source-bound vector identities. No inference or active index writes.
use super::lexical_input::{LexicalEntry, LexicalInput};
use super::{dberr, lexical_staging, Connection};
use crate::core::{is_sha256_hex, sha256_hex, Error, Result, Uuid};
use crate::embeddings::strict_contract::{validate_blob, StrictContract};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(super) const MAX_VECTOR_BYTES: u64 = 24 * 1024 * 1024;
pub(super) const MAX_SEMANTIC_METADATA: u64 = 8 * 1024 * 1024;
pub(super) const MAX_CANDIDATE: u64 = 64 * 1024 * 1024;
pub(super) const VERIFICATION_POLICY: &str =
    "doc-semantic-verified@1;canonical-byte-exact;f32le-finite-unit@1";
const MAX_INPUT: usize = 65536;
// Every vector stores six fixed-size IDs/digests plus ordinal, count and dimension.
const VECTOR_METADATA_BYTES: u64 = 6 * 64 + 8 + 4 + 4;

fn refuse(why: &str) -> Error {
    Error::Storage(format!("semantic staging refused: {why}"))
}
fn string(out: &mut Vec<u8>, s: &str) {
    bytes(out, s.as_bytes());
}
fn bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u64).to_be_bytes());
    out.extend_from_slice(b);
}
fn hash_string(h: &mut Sha256, s: &str) {
    h.update((s.len() as u64).to_be_bytes());
    h.update(s.as_bytes());
}
fn finish(h: Sha256) -> String {
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticTarget {
    pub lexical_stage_id: String,
    pub model_id: String,
    pub pipeline_key: String,
}
impl SemanticTarget {
    /// Historical receipt routing must not require the current registry or model feature.
    pub fn validate(&self) -> Result<()> {
        if !is_sha256_hex(&self.lexical_stage_id)
            || !is_sha256_hex(&self.pipeline_key)
            || self.model_id.is_empty()
            || self.model_id.len() > 256
            || !self
                .model_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-/@:".contains(&c))
        {
            return Err(refuse("invalid semantic target syntax"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct SemanticInput {
    pub target: SemanticTarget,
    pub parent_operation_id: String,
    pub parent_revision: i64,
    pub parent_result_digest: String,
    pub lexical: LexicalInput,
    pub input_manifest: String,
    pub semantic_id: String,
    /// Vector-row metadata only; persistence additionally charges its bounded header/receipt.
    pub semantic_metadata_bytes: u64,
}
impl SemanticInput {
    pub fn artifact_canonical(&self) -> Vec<u8> {
        let mut out = Vec::new();
        string(&mut out, "doc-semantic-set@1");
        string(&mut out, &self.input_manifest);
        string(&mut out, &self.target.pipeline_key);
        string(&mut out, VERIFICATION_POLICY);
        out
    }
    pub fn vector_bytes(&self) -> u64 {
        self.lexical.entry_count * 3072
    }
    pub fn validate(&self) -> Result<()> {
        self.target.validate()?;
        self.lexical.validate()?;
        let parent = self.parent_operation_id.split(':').collect::<Vec<_>>();
        let valid_operation = parent.len() == 3
            && parent[0] == "local-doc-stage-lexical@1"
            && Uuid::parse_str(parent[1]).is_ok_and(|u| u.to_string() == parent[1])
            && is_sha256_hex(parent[2]);
        if !valid_operation
            || self.parent_revision <= 0
            || !is_sha256_hex(&self.parent_result_digest)
            || self.target.lexical_stage_id != self.lexical.stage_id
            || !is_sha256_hex(&self.input_manifest)
            || !is_sha256_hex(&self.semantic_id)
            || self.semantic_id != sha256_hex(&self.artifact_canonical())
            || self.semantic_metadata_bytes != self.lexical.entry_count * VECTOR_METADATA_BYTES
            || self.semantic_metadata_bytes > MAX_SEMANTIC_METADATA
            || self.vector_bytes() > MAX_VECTOR_BYTES
            || self.lexical.source_bytes_len
                + self.lexical.metadata_bytes
                + self.vector_bytes()
                + self.semantic_metadata_bytes
                > MAX_CANDIDATE
        {
            return Err(refuse("invalid bounded semantic input"));
        }
        if serde_json::to_vec(self)
            .map_err(|_| refuse("cannot encode semantic input"))?
            .len()
            > MAX_INPUT
        {
            return Err(refuse("semantic input metadata exceeds 64 KiB"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SemanticEntry {
    pub lexical: LexicalEntry,
    pub stored_token_count: u32,
    pub vector_key: String,
}
pub(super) struct SemanticSource {
    pub input: SemanticInput,
    pub entries: Vec<SemanticEntry>,
}

fn vector_key(input: &LexicalInput, e: &LexicalEntry, pipeline: &str) -> String {
    let t = &input.target;
    let mut out = Vec::new();
    string(&mut out, "doc-source-vector@1");
    for id in [t.document_id, t.document_source_version, t.extraction_id] {
        out.extend_from_slice(id.as_bytes());
    }
    for s in [
        &t.chunker_version,
        &t.budget_identity,
        &t.plan_hash,
        &e.entry_id,
        &e.chunk_id,
    ] {
        string(&mut out, s);
    }
    out.extend_from_slice(&e.start.to_be_bytes());
    out.extend_from_slice(&e.end.to_be_bytes());
    string(&mut out, &e.payload_hash);
    string(&mut out, pipeline);
    sha256_hex(&out)
}
fn manifest(parent: &lexical_staging::CanonicalParent, entries: &[SemanticEntry]) -> String {
    let mut h = Sha256::new();
    hash_string(&mut h, "doc-semantic-input@1");
    hash_string(&mut h, &parent.candidate.input.stage_id);
    hash_string(&mut h, &parent.result_digest);
    // Authoritative lexical recipe carries preparation/Fs/Ds/X/C and all immutable hashes.
    let provenance = parent.candidate.input.artifact_canonical();
    h.update((provenance.len() as u64).to_be_bytes());
    h.update(provenance);
    h.update((entries.len() as u64).to_be_bytes());
    for item in entries {
        let e = &item.lexical;
        h.update(e.ordinal.to_be_bytes());
        for s in [&e.entry_id, &e.chunk_id] {
            hash_string(&mut h, s);
        }
        h.update(e.start.to_be_bytes());
        h.update(e.end.to_be_bytes());
        hash_string(&mut h, &e.payload_hash);
        hash_string(&mut h, &e.anchor);
        h.update(item.stored_token_count.to_be_bytes());
    }
    finish(h)
}
pub(super) fn build(conn: &Connection, target: &SemanticTarget) -> Result<SemanticSource> {
    target.validate()?;
    let parent = lexical_staging::canonical_parent(conn, &target.lexical_stage_id)?;
    let p = &parent.candidate.input.target;
    let mut statement=conn.prepare("SELECT ordinal,token_count FROM doc_chunks WHERE extraction_id=?1 AND version=?2 AND budget_identity=?3 ORDER BY ordinal LIMIT 8193").map_err(dberr)?;
    let stored = statement
        .query_map(
            params![
                p.extraction_id.to_string(),
                p.chunker_version,
                p.budget_identity
            ],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )
        .map_err(dberr)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(dberr)?;
    if stored.len() != parent.candidate.entries.len() {
        return Err(refuse("semantic token provenance population differs"));
    }
    let mut entries = Vec::with_capacity(stored.len());
    let mut total_tokens = 0u64;
    for (e, (ordinal, tokens)) in parent.candidate.entries.iter().zip(stored) {
        if ordinal < 0 || ordinal as u64 != e.ordinal || !(1..=2048).contains(&tokens) {
            return Err(refuse("invalid selected chunk token provenance"));
        }
        total_tokens += tokens as u64;
        entries.push(SemanticEntry {
            lexical: e.clone(),
            stored_token_count: tokens as u32,
            vector_key: vector_key(&parent.candidate.input, e, &target.pipeline_key),
        });
    }
    if total_tokens > 16_777_216 {
        return Err(refuse("semantic token work bound exceeded"));
    }
    let input_manifest = manifest(&parent, &entries);
    let mut input = SemanticInput {
        target: target.clone(),
        parent_operation_id: parent.operation_id,
        parent_revision: parent.revision,
        parent_result_digest: parent.result_digest,
        lexical: parent.candidate.input,
        input_manifest,
        semantic_id: String::new(),
        semantic_metadata_bytes: entries.len() as u64 * VECTOR_METADATA_BYTES,
    };
    input.semantic_id = sha256_hex(&input.artifact_canonical());
    input.validate()?;
    Ok(SemanticSource { input, entries })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct SemanticVector {
    pub entry_id: String,
    pub ordinal: u64,
    pub vector_key: String,
    pub payload_hash: String,
    pub token_count: u32,
    pub encoding_digest: String,
    pub dimension: u32,
    pub embedding: Vec<u8>,
    pub vector_hash: String,
}
pub(super) fn result_digest(semantic_id: &str, vectors: &[SemanticVector]) -> String {
    let mut h = Sha256::new();
    hash_string(&mut h, "doc-semantic-result@1");
    hash_string(&mut h, semantic_id);
    h.update((vectors.len() as u64).to_be_bytes());
    for v in vectors {
        h.update(v.ordinal.to_be_bytes());
        hash_string(&mut h, &v.entry_id);
        hash_string(&mut h, &v.vector_key);
        h.update(v.token_count.to_be_bytes());
        hash_string(&mut h, &v.encoding_digest);
        h.update(v.dimension.to_be_bytes());
        hash_string(&mut h, &v.vector_hash);
    }
    finish(h)
}
pub(super) fn validate_vector_rows(
    source: &SemanticSource,
    contract: &StrictContract,
    vectors: &[SemanticVector],
) -> Result<()> {
    source.input.validate()?;
    contract.validate_stored()?;
    if contract.model_id != source.input.target.model_id
        || contract.key() != source.input.target.pipeline_key
        || contract.budget_identity != source.input.lexical.target.budget_identity
        || vectors.len() != source.entries.len()
    {
        return Err(refuse("semantic vector population or contract differs"));
    }
    let mut vector_bytes = 0u64;
    for (entry, v) in source.entries.iter().zip(vectors) {
        let e = &entry.lexical;
        if v.entry_id != e.entry_id
            || v.ordinal != e.ordinal
            || v.vector_key != entry.vector_key
            || v.payload_hash != e.payload_hash
            || v.token_count != entry.stored_token_count
            || v.dimension != contract.dimension
            || !is_sha256_hex(&v.encoding_digest)
            || !is_sha256_hex(&v.vector_hash)
            || v.vector_hash != sha256_hex(&v.embedding)
        {
            return Err(refuse(
                "semantic vector is not bound to its exact canonical entry",
            ));
        }
        validate_blob(contract, &v.embedding)?;
        vector_bytes += v.embedding.len() as u64;
    }
    if vector_bytes != source.input.vector_bytes() || vector_bytes > MAX_VECTOR_BYTES {
        return Err(refuse("semantic vector byte completeness differs"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "semantic_input_tests.rs"]
mod tests;

/// Structural storage fixture: a real prepared source and committed lexical receipt.
#[cfg(test)]
pub(super) fn semantic_source_fixture() -> (super::SqliteStore, SemanticTarget) {
    tests::fixture()
}
