//! Trusted-local construction of complete unpublished semantic artifacts.
//! Receipt identity binds inputs; the atomic receipt binds the winning actual result.
pub(crate) use super::semantic_input::SemanticTarget;
use super::semantic_input::{self, SemanticInput, SemanticSource, SemanticVector};
use super::*;
#[cfg(unix)]
use crate::cli::ownership::AdminContext;
use crate::embeddings::strict_contract::StrictContract;
use serde::{Deserialize, Serialize};
use std::time::Instant;

const SQL: &str = include_str!("../../../migrations/0016_semantic_staging.sql");
const DOMAIN: &str = "mindspace.s3d2.local-semantic-stage@1";
const ACTOR: &str = "local-os-account@1";
const ACTION: &str = "stage_document_semantic@1";
const PROFILE: &str = "doc-semantic-local@1;entries=8192;tokens=16777216;vectors=25165824;metadata=8388608;candidate=67108864;deadline=600;batch=1";
const MAX_REQUEST: usize = 65536;
const MAX_METADATA: u64 = 8 * 1024 * 1024;
const MAX_VECTOR: u64 = 24 * 1024 * 1024;
const MAX_CANDIDATE: u64 = 64 * 1024 * 1024;
const SET_COLUMNS: &str = "semantic_id,lexical_stage_id,parent_operation_id,parent_revision,parent_result_digest,input_manifest,input_json,contract_json,pipeline_key,model_id,budget_identity,vector_count,vector_bytes,metadata_bytes,result_digest,operation_id,created_revision,state";
const VECTOR_COLUMNS: &str = "entry_id,semantic_id,ordinal,vector_key,payload_hash,token_count,encoding_digest,dimension,embedding,vector_hash";
const RECEIPT_COLUMNS: &str = "operation_id,nonce,digest,canonical_request,intent_json,request_version,db_path,db_dev,db_ino,actor_kind,actor_uid,expected_revision,semantic_id,result_digest,committed_revision,action,expected_state,intended_state";

#[derive(Default)]
pub(super) struct SemanticState {
    permit: Mutex<Option<Permit>>,
}
struct Permit {
    set: Vec<Value>,
    vectors: HashMap<String, Vec<Value>>,
    receipt: Vec<Value>,
}
struct PermitScope<'a>(&'a SemanticState);
impl Drop for PermitScope<'_> {
    fn drop(&mut self) {
        if let Ok(mut p) = self.0.permit.lock() {
            *p = None;
        }
    }
}
pub(super) fn register_context(conn: &Connection, state: &Arc<SemanticState>) -> Result<()> {
    for (name, kind) in [
        ("s3d2_set_valid", 0),
        ("s3d2_vector_valid", 1),
        ("s3d2_receipt_valid", 2),
    ] {
        let state = Arc::clone(state);
        conn.create_scalar_function(
            name,
            -1,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS,
            move |ctx| {
                let Ok(g) = state.permit.lock() else {
                    return Ok(false);
                };
                let Some(p) = g.as_ref() else {
                    return Ok(false);
                };
                let values = match kind {
                    0 => Some(&p.set),
                    2 => Some(&p.receipt),
                    _ => p.vectors.get(&ctx.get::<String>(0)?),
                };
                let Some(values) = values else {
                    return Ok(false);
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
        "s3d2_availability_valid",
        3,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS,
        move |ctx| {
            let Ok(g) = state.permit.lock() else {
                return Ok(false);
            };
            let Some(p) = g.as_ref() else {
                return Ok(false);
            };
            let key: Value = ctx.get(1)?;
            let valid = match ctx.get::<String>(0)?.as_str() {
                "doc_staged_semantic_sets" => key == p.set[0],
                "doc_semantic_stage_receipts" => key == p.receipt[0],
                "doc_staged_vectors" => match key {
                    Value::Text(ref k) => k.split_once(':').is_some_and(|(s, e)| {
                        p.set[0] == Value::Text(s.to_owned()) && p.vectors.contains_key(e)
                    }),
                    _ => false,
                },
                _ => false,
            };
            Ok(valid && ctx.get::<Value>(2)? == p.set[16])
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
fn refuse(s: &str) -> Error {
    Error::Storage(format!("semantic staging refused: {s}"))
}
fn string(out: &mut Vec<u8>, s: &str) {
    bytes(out, s.as_bytes())
}
fn bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u64).to_be_bytes());
    out.extend_from_slice(b)
}
fn bounded_json<T: Serialize>(v: &T) -> Result<Vec<u8>> {
    let b = serde_json::to_vec(v).map_err(|_| refuse("cannot encode durable metadata"))?;
    if b.len() > MAX_REQUEST {
        return Err(refuse("durable metadata exceeds 64 KiB"));
    }
    Ok(b)
}
fn parse_id(s: &str) -> Result<(Uuid, &str)> {
    let p = s.split(':').collect::<Vec<_>>();
    if p.len() != 3 || p[0] != "local-doc-stage-semantic@1" || !crate::core::is_sha256_hex(p[2]) {
        return Err(refuse("invalid full operation identity"));
    }
    let n = Uuid::parse_str(p[1]).map_err(|_| refuse("invalid nonce"))?;
    if n.to_string() != p[1] {
        return Err(refuse("noncanonical nonce"));
    }
    Ok((n, p[2]))
}
fn deadline_check(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        Err(refuse("cooperative 600-second attempt deadline exceeded"))
    } else {
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    nonce: Uuid,
    db_path: String,
    db_dev: u64,
    db_ino: u64,
    actor_uid: u32,
    expected_revision: i64,
    input: SemanticInput,
    contract: StrictContract,
}
impl Intent {
    fn canonical(&self) -> Result<Vec<u8>> {
        self.input.validate()?;
        self.contract.validate_stored()?;
        if self.expected_revision <= 0
            || self.input.target.pipeline_key != self.contract.key()
            || self.input.target.model_id != self.contract.model_id
            || self.input.lexical.target.budget_identity != self.contract.budget_identity
        {
            return Err(refuse("input differs from exact strict contract"));
        }
        let mut b = Vec::new();
        string(&mut b, DOMAIN);
        b.extend_from_slice(self.nonce.as_bytes());
        string(&mut b, &self.db_path);
        b.extend_from_slice(&self.db_dev.to_be_bytes());
        b.extend_from_slice(&self.db_ino.to_be_bytes());
        string(&mut b, ACTOR);
        b.extend_from_slice(&(self.actor_uid as u64).to_be_bytes());
        bytes(&mut b, &self.input.artifact_canonical());
        string(&mut b, &self.input.parent_operation_id);
        b.extend_from_slice(&self.input.parent_revision.to_be_bytes());
        string(&mut b, &self.input.parent_result_digest);
        bytes(&mut b, &self.input.lexical.artifact_canonical());
        b.extend_from_slice(&self.input.lexical.preparation_revision.to_be_bytes());
        b.extend_from_slice(&self.input.lexical.metadata_bytes.to_be_bytes());
        bytes(&mut b, &self.input.lexical.logical_base);
        b.extend_from_slice(&self.input.semantic_metadata_bytes.to_be_bytes());
        string(&mut b, &self.contract.key());
        b.extend_from_slice(&self.expected_revision.to_be_bytes());
        for s in [
            PROFILE,
            ACTION,
            "NoSemanticArtifact",
            "CreateVerifiedUnpublishedSemanticSet",
        ] {
            string(&mut b, s)
        }
        if b.len() > MAX_REQUEST {
            return Err(refuse("canonical intent exceeds 64 KiB"));
        }
        Ok(b)
    }
    fn digest(&self) -> Result<String> {
        Ok(sha256_hex(&self.canonical()?))
    }
    fn operation_id(&self) -> Result<String> {
        Ok(format!(
            "local-doc-stage-semantic@1:{}:{}",
            self.nonce,
            self.digest()?
        ))
    }
    fn metadata_bytes(&self, operation: &str, result: &str, r: i64) -> Result<u64> {
        let own = self
            .input
            .semantic_metadata_bytes
            .checked_add(
                (bounded_json(&self.input)?.len()
                    + bounded_json(&self.contract)?.len()
                    + self.canonical()?.len()
                    + bounded_json(self)?.len()
                    + operation.len()
                    + result.len()
                    + r.to_string().len()
                    + 1024) as u64,
            )
            .ok_or_else(|| refuse("metadata overflow"))?;
        let vectors = self
            .input
            .lexical
            .entry_count
            .checked_mul(3072)
            .ok_or_else(|| refuse("vector size overflow"))?;
        if own > MAX_METADATA
            || vectors > MAX_VECTOR
            || self.input.lexical.source_bytes_len
                + self.input.lexical.metadata_bytes
                + vectors
                + own
                > MAX_CANDIDATE
        {
            return Err(refuse("semantic logical candidate bounds exceeded"));
        }
        Ok(own)
    }
    fn set_values(&self, operation: &str, result: &str, r: i64) -> Result<Vec<Value>> {
        if !crate::core::is_sha256_hex(result) {
            return Err(refuse("invalid result digest"));
        }
        Ok(vec![
            self.input.semantic_id.clone().into(),
            self.input.target.lexical_stage_id.clone().into(),
            self.input.parent_operation_id.clone().into(),
            self.input.parent_revision.into(),
            self.input.parent_result_digest.clone().into(),
            self.input.input_manifest.clone().into(),
            bounded_json(&self.input)?.into(),
            bounded_json(&self.contract)?.into(),
            self.contract.key().into(),
            self.contract.model_id.clone().into(),
            self.contract.budget_identity.clone().into(),
            (self.input.lexical.entry_count as i64).into(),
            (self.input.lexical.entry_count as i64 * 3072).into(),
            (self.metadata_bytes(operation, result, r)? as i64).into(),
            result.to_owned().into(),
            operation.to_owned().into(),
            r.into(),
            "verified_semantic_unpublished".to_owned().into(),
        ])
    }
    fn receipt_values(&self, operation: &str, result: &str, r: i64) -> Result<Vec<Value>> {
        Ok(vec![
            operation.to_owned().into(),
            self.nonce.to_string().into(),
            self.digest()?.into(),
            self.canonical()?.into(),
            bounded_json(self)?.into(),
            1_i64.into(),
            self.db_path.clone().into(),
            self.db_dev.to_string().into(),
            self.db_ino.to_string().into(),
            ACTOR.to_owned().into(),
            (self.actor_uid as i64).into(),
            self.expected_revision.into(),
            self.input.semantic_id.clone().into(),
            result.to_owned().into(),
            r.into(),
            ACTION.to_owned().into(),
            "NoSemanticArtifact".to_owned().into(),
            "CreateVerifiedUnpublishedSemanticSet".to_owned().into(),
        ])
    }
}
fn vector_values(semantic: &str, v: &SemanticVector) -> Vec<Value> {
    vec![
        v.entry_id.clone().into(),
        semantic.to_owned().into(),
        (v.ordinal as i64).into(),
        v.vector_key.clone().into(),
        v.payload_hash.clone().into(),
        (v.token_count as i64).into(),
        v.encoding_digest.clone().into(),
        (v.dimension as i64).into(),
        v.embedding.clone().into(),
        v.vector_hash.clone().into(),
    ]
}
fn insert_values(c: &Connection, table: &str, columns: &str, values: &[Value]) -> Result<()> {
    let ps = (1..=values.len())
        .map(|n| format!("?{n}"))
        .collect::<Vec<_>>()
        .join(",");
    c.execute(
        &format!("INSERT INTO {table}({columns}) VALUES({ps})"),
        rusqlite::params_from_iter(values),
    )
    .map_err(dberr)?;
    Ok(())
}
fn select_values(
    c: &Connection,
    table: &str,
    columns: &str,
    key: &str,
    id: &str,
    n: usize,
) -> Result<Option<Vec<Value>>> {
    c.query_row(
        &format!("SELECT {columns} FROM {table} WHERE {key}=?1"),
        [id],
        |r| {
            (0..n)
                .map(|i| r.get(i))
                .collect::<rusqlite::Result<Vec<Value>>>()
        },
    )
    .optional()
    .map_err(dberr)
}
struct Stored {
    intent: Intent,
    result_digest: String,
    revision: i64,
}
fn stored(c: &Connection, operation: &str) -> Result<Option<Stored>> {
    let Some(values) = select_values(
        c,
        "doc_semantic_stage_receipts",
        RECEIPT_COLUMNS,
        "operation_id",
        operation,
        18,
    )?
    else {
        return Ok(None);
    };
    let (Value::Blob(json), Value::Text(result), Value::Integer(r)) =
        (&values[4], &values[13], &values[14])
    else {
        return Err(refuse("invalid stored receipt types"));
    };
    if json.len() > MAX_REQUEST {
        return Err(refuse("oversized stored intent"));
    }
    let i: Intent =
        serde_json::from_slice(json).map_err(|_| refuse("invalid stored intent encoding"))?;
    if i.operation_id()? != operation
        || i.expected_revision.checked_add(1) != Some(*r)
        || !crate::core::is_sha256_hex(result)
        || i.receipt_values(operation, result, *r)? != values
    {
        return Err(refuse(
            "stored receipt conflicts with immutable input/result",
        ));
    }
    if select_values(
        c,
        "doc_staged_semantic_sets",
        SET_COLUMNS,
        "semantic_id",
        &i.input.semantic_id,
        18,
    )?
    .as_ref()
        != Some(&i.set_values(operation, result, *r)?)
    {
        return Err(refuse("stored set and receipt disagree"));
    }
    Ok(Some(Stored {
        intent: i,
        result_digest: result.clone(),
        revision: *r,
    }))
}
fn stored_for_semantic(c: &Connection, id: &str) -> Result<Option<Stored>> {
    let op: Option<String> = c
        .query_row(
            "SELECT operation_id FROM doc_staged_semantic_sets WHERE semantic_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(dberr)?;
    op.map(|op| stored(c, &op)?.ok_or_else(|| refuse("missing semantic receipt")))
        .transpose()
}
fn verify_stored(c: &Connection, source: &SemanticSource, s: &Stored) -> Result<()> {
    if source.input.artifact_canonical() != s.intent.input.artifact_canonical() {
        return Err(refuse(
            "current source input differs from stored semantic artifact",
        ));
    }
    let mut stmt=c.prepare(&format!("SELECT {VECTOR_COLUMNS} FROM doc_staged_vectors WHERE semantic_id=?1 ORDER BY ordinal LIMIT 8193")).map_err(dberr)?;
    let rows = stmt
        .query_map([&s.intent.input.semantic_id], |r| {
            // Do not coerce CHECK-violating corruption into valid evidence.
            let ordinal: i64 = r.get(2)?;
            let ordinal = u64::try_from(ordinal)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, ordinal))?;
            Ok(SemanticVector {
                entry_id: r.get(0)?,
                ordinal,
                vector_key: r.get(3)?,
                payload_hash: r.get(4)?,
                token_count: r.get(5)?,
                encoding_digest: r.get(6)?,
                dimension: r.get(7)?,
                embedding: r.get(8)?,
                vector_hash: r.get(9)?,
            })
        })
        .map_err(dberr)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(dberr)?;
    semantic_input::validate_vector_rows(source, &s.intent.contract, &rows)?;
    if semantic_input::result_digest(&s.intent.input.semantic_id, &rows) != s.result_digest {
        return Err(refuse("stored semantic result digest differs"));
    }
    Ok(())
}
#[derive(Serialize)]
pub(crate) struct SemanticStagePreview {
    pub operation_id: String,
    pub digest: String,
    pub semantic_id: String,
    #[serde(flatten)]
    pub target: SemanticTarget,
    pub expected_revision: i64,
    pub vector_count: u64,
    pub source_bytes_len: u64,
    pub contract: StrictContract,
    pub db_path: String,
    pub db_dev: u64,
    pub db_ino: u64,
    pub actor_kind: &'static str,
    pub actor_uid: u32,
    pub(super) input: SemanticInput,
    pub artifact_availability: &'static str,
    pub fresh_tokens: &'static str,
}
#[derive(Serialize)]
pub(crate) struct SemanticStageOutcome {
    pub outcome: &'static str,
    pub operation_id: Option<String>,
    pub semantic_id: String,
    pub committed_revision: Option<i64>,
    pub result_digest: String,
    pub current_integrity: &'static str,
}
#[derive(Serialize)]
pub(crate) struct SemanticStageInspection {
    pub semantic_id: String,
    pub verified_at_revision: i64,
    pub vector_count: u64,
    pub historical_verified: bool,
    pub canonical_integrity: &'static str,
    pub vector_integrity: &'static str,
    pub fresh_tokens: &'static str,
    pub lexical_fts_integrity: &'static str,
    pub encoder_availability: &'static str,
    pub original_bytes_status: &'static str,
    pub error: Option<String>,
}
fn outcome(s: &Stored) -> Result<SemanticStageOutcome> {
    Ok(SemanticStageOutcome {
        outcome: "committed",
        operation_id: Some(s.intent.operation_id()?),
        semantic_id: s.intent.input.semantic_id.clone(),
        committed_revision: Some(s.revision),
        result_digest: s.result_digest.clone(),
        current_integrity: "not_checked",
    })
}
#[cfg(unix)]
fn replay(
    s: Stored,
    ctx: &AdminContext,
    target: &SemanticTarget,
    expected: i64,
) -> Result<SemanticStageOutcome> {
    let i = &s.intent;
    if &i.input.target != target
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
    outcome(&s)
}
#[cfg(unix)]
fn intent(
    ctx: &AdminContext,
    nonce: Uuid,
    expected: i64,
    input: SemanticInput,
    contract: StrictContract,
) -> Result<Intent> {
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
        input,
        contract,
    })
}
fn arm<'a>(
    state: &'a SemanticState,
    i: &Intent,
    rows: &[SemanticVector],
    op: &str,
    result: &str,
    r: i64,
) -> Result<PermitScope<'a>> {
    let mut p = state
        .permit
        .lock()
        .map_err(|_| refuse("administrative scope poisoned"))?;
    if p.is_some() {
        return Err(refuse("nested administrative scope"));
    }
    *p = Some(Permit {
        set: i.set_values(op, result, r)?,
        vectors: rows
            .iter()
            .map(|v| (v.entry_id.clone(), vector_values(&i.input.semantic_id, v)))
            .collect(),
        receipt: i.receipt_values(op, result, r)?,
    });
    Ok(PermitScope(state))
}
fn insert_candidate(
    c: &Connection,
    i: &Intent,
    rows: &[SemanticVector],
    op: &str,
    result: &str,
    r: i64,
) -> Result<()> {
    insert_values(
        c,
        "doc_staged_semantic_sets",
        SET_COLUMNS,
        &i.set_values(op, result, r)?,
    )?;
    for row in rows {
        insert_values(
            c,
            "doc_staged_vectors",
            VECTOR_COLUMNS,
            &vector_values(&i.input.semantic_id, row),
        )?;
        #[cfg(feature = "spawn-gate")]
        if std::env::var("MINDSPACE_SEMANTIC_STAGE_FAULT").as_deref() == Ok("vector:1") {
            return Err(refuse("injected partial semantic vector insertion failure"));
        }
    }
    insert_values(
        c,
        "doc_semantic_stage_receipts",
        RECEIPT_COLUMNS,
        &i.receipt_values(op, result, r)?,
    )?;
    #[cfg(feature = "spawn-gate")]
    if std::env::var("MINDSPACE_SEMANTIC_STAGE_FAULT").as_deref() == Ok("receipt") {
        return Err(refuse("injected semantic receipt failure"));
    }
    let violations: i64 = c
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .map_err(dberr)?;
    if violations != 0 {
        return Err(refuse("semantic transaction failed foreign_key_check"));
    }
    Ok(())
}
fn check_nonce(c: &Connection, nonce: Uuid) -> Result<()> {
    if c.query_row(
        "SELECT EXISTS(SELECT 1 FROM doc_semantic_stage_receipts WHERE nonce=?1)",
        [nonce.to_string()],
        |r| r.get::<_, bool>(0),
    )
    .map_err(dberr)?
    {
        Err(refuse("committed nonce has different intent"))
    } else {
        Ok(())
    }
}
fn same_artifact(
    c: &Connection,
    i: &Intent,
    source: &SemanticSource,
) -> Result<Option<SemanticStageOutcome>> {
    let Some(s) = stored_for_semantic(c, &i.input.semantic_id)? else {
        return Ok(None);
    };
    verify_stored(c, source, &s)?;
    Ok(Some(SemanticStageOutcome {
        outcome: "existing_artifact",
        operation_id: None,
        semantic_id: i.input.semantic_id.clone(),
        committed_revision: None,
        result_digest: s.result_digest,
        current_integrity: "verified",
    }))
}
#[cfg(feature = "embeddings")]
fn generate(
    source: &SemanticSource,
    contract: &StrictContract,
    deadline: Instant,
) -> Result<Vec<SemanticVector>> {
    let inputs = source
        .entries
        .iter()
        .map(|e| crate::embeddings::strict::StrictInput {
            content: &e.lexical.content,
            expected_tokens: e.stored_token_count,
            payload_hash: &e.lexical.payload_hash,
        })
        .collect::<Vec<_>>();
    let outputs = crate::embeddings::strict::encode_all(contract, &inputs, deadline)?;
    if outputs.len() != source.entries.len() {
        return Err(refuse("incomplete strict encoder output"));
    }
    let rows = source
        .entries
        .iter()
        .zip(outputs)
        .map(|(entry, v)| SemanticVector {
            entry_id: entry.lexical.entry_id.clone(),
            ordinal: entry.lexical.ordinal,
            vector_key: entry.vector_key.clone(),
            payload_hash: entry.lexical.payload_hash.clone(),
            token_count: v.token_count,
            encoding_digest: v.encoding_digest,
            dimension: 768,
            embedding: v.embedding,
            vector_hash: v.vector_hash,
        })
        .collect::<Vec<_>>();
    semantic_input::validate_vector_rows(source, contract, &rows)?;
    Ok(rows)
}
#[cfg(not(feature = "embeddings"))]
fn generate(_: &SemanticSource, _: &StrictContract, _: Instant) -> Result<Vec<SemanticVector>> {
    Err(refuse(
        "embeddings feature is disabled; no new semantic artifact can be computed",
    ))
}

#[cfg(unix)]
impl SqliteStore {
    pub(crate) fn semantic_stage_preview(
        &self,
        ctx: &AdminContext,
        target: &SemanticTarget,
        expected: i64,
        nonce: Uuid,
    ) -> Result<SemanticStagePreview> {
        ctx.validate()?;
        target.validate()?;
        if !cfg!(feature = "embeddings") {
            return Err(refuse(
                "embeddings feature is disabled; fresh semantic preview unavailable",
            ));
        }
        let conn = self.connection()?;
        ownership::validate_connection(&conn, ctx)?;
        let read =
            Transaction::new_unchecked(&conn, TransactionBehavior::Deferred).map_err(dberr)?;
        let r: i64 = read
            .query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| {
                r.get(0)
            })
            .map_err(dberr)?;
        if r != expected {
            return Err(refuse("stale expected recording revision"));
        }
        let source = semantic_input::build(&read, target)?;
        let contract = if let Some(s) = stored_for_semantic(&read, &source.input.semantic_id)? {
            s.intent.contract
        } else {
            StrictContract::current(&target.model_id)?
        };
        let i = intent(ctx, nonce, expected, source.input, contract)?;
        let operation = i.operation_id()?;
        i.metadata_bytes(
            &operation,
            &"0".repeat(64),
            expected
                .checked_add(1)
                .ok_or_else(|| refuse("revision overflow"))?,
        )?;
        let out = SemanticStagePreview {
            operation_id: operation,
            digest: i.digest()?,
            semantic_id: i.input.semantic_id.clone(),
            target: target.clone(),
            expected_revision: expected,
            vector_count: i.input.lexical.entry_count,
            source_bytes_len: i.input.lexical.source_bytes_len,
            contract: i.contract.clone(),
            db_path: i.db_path.clone(),
            db_dev: i.db_dev,
            db_ino: i.db_ino,
            actor_kind: ACTOR,
            actor_uid: i.actor_uid,
            input: i.input,
            artifact_availability: "not_checked",
            fresh_tokens: "not_checked",
        };
        read.rollback().map_err(dberr)?;
        Ok(out)
    }
    pub(crate) fn semantic_stage_apply(
        &self,
        ctx: &AdminContext,
        target: &SemanticTarget,
        expected: i64,
        operation: &str,
        confirmation: &str,
    ) -> Result<SemanticStageOutcome> {
        ctx.validate()?;
        target.validate()?;
        let (nonce, digest) = parse_id(operation)?;
        if digest != confirmation {
            return Err(refuse("exact intent confirmation required"));
        }
        let deadline = Instant::now() + Duration::from_secs(600);
        {
            let conn = self.connection()?;
            ownership::validate_connection(&conn, ctx)?;
            if let Some(s) = stored(&conn, operation)? {
                return replay(s, ctx, target, expected);
            }
        }
        #[cfg(feature = "spawn-gate")]
        test_pause("before_admission");
        // Admission reserves no durable work: the unused recording revision is rolled back.
        // All source strings and descriptors leave this scope as owned data.
        let (i, source) = {
            let conn = self.connection()?;
            ownership::validate_connection(&conn, ctx)?;
            let tx = begin_write(&conn, &self.revision_state)?;
            if let Some(s) = stored(&tx, operation)? {
                return replay(s, ctx, target, expected);
            }
            check_nonce(&tx, nonce)?;
            let source = semantic_input::build(&tx, target)?;
            let contract = if let Some(s) = stored_for_semantic(&tx, &source.input.semantic_id)? {
                s.intent.contract
            } else {
                StrictContract::current(&target.model_id)?
            };
            let i = intent(ctx, nonce, expected, source.input.clone(), contract)?;
            if i.operation_id()? != operation {
                return Err(refuse(
                    "operation identity conflicts with immutable parameters",
                ));
            }
            if let Some(status) = same_artifact(&tx, &i, &source)? {
                return Ok(status);
            }
            let r = self.revision_state.active_revision.load(Ordering::SeqCst);
            if r.checked_sub(1) != Some(expected) {
                return Err(refuse("stale expected recording revision"));
            }
            i.metadata_bytes(operation, &"0".repeat(64), r)?;
            deadline_check(deadline)?;
            drop(tx);
            drop(conn);
            (i, source)
        };
        #[cfg(feature = "spawn-gate")]
        test_pause("after_admission");
        #[cfg(feature = "spawn-gate")]
        test_pause("before_inference");
        // No ConnectionGuard or SQLite transaction survives across tokenization/model work.
        let rows = match deadline_check(deadline)
            .and_then(|()| generate(&source, &i.contract, deadline))
        {
            Ok(v) => v,
            Err(error) => {
                return self.semantic_generation_failure(ctx, target, expected, operation, error)
            }
        };
        #[cfg(feature = "spawn-gate")]
        test_pause("after_inference");
        let result = semantic_input::result_digest(&i.input.semantic_id, &rows);
        #[cfg(feature = "spawn-gate")]
        test_pause("before_final");
        let conn = self.connection()?;
        ownership::validate_connection(&conn, ctx)?;
        let tx = begin_write(&conn, &self.revision_state)?;
        #[cfg(feature = "spawn-gate")]
        test_pause("after_final");
        // The already committed winner is authoritative even when our float results differ.
        if let Some(s) = stored(&tx, operation)? {
            return replay(s, ctx, target, expected);
        }
        check_nonce(&tx, nonce)?;
        let current = semantic_input::build(&tx, target)?;
        if current.input != i.input {
            return Err(refuse(
                "source or intended transition changed before commit",
            ));
        }
        if let Some(status) = same_artifact(&tx, &i, &current)? {
            return Ok(status);
        }
        let r = self.revision_state.active_revision.load(Ordering::SeqCst);
        if r.checked_sub(1) != Some(expected) {
            return Err(refuse("stale expected recording revision"));
        }
        deadline_check(deadline)?;
        semantic_input::validate_vector_rows(&current, &i.contract, &rows)?;
        let scope = arm(&self.semantic_state, &i, &rows, operation, &result, r)?;
        insert_candidate(&tx, &i, &rows, operation, &result, r)?;
        #[cfg(feature = "spawn-gate")]
        test_pause("after_write");
        #[cfg(feature = "spawn-gate")]
        test_pause("before_commit");
        deadline_check(deadline)?;
        ctx.validate()?;
        #[cfg(feature = "spawn-gate")]
        configure_faults();
        if let Err(error) = tx.commit() {
            if matches!(error, Error::CommitOutcomeUnknown { .. }) {
                drop(scope);
                drop(conn);
                if let Ok(mut c) = self.conn.lock() {
                    c.take();
                }
            }
            return Err(error);
        }
        #[cfg(feature = "spawn-gate")]
        test_pause("after_commit");
        outcome(&Stored {
            intent: i,
            result_digest: result,
            revision: r,
        })
    }
    // A failed forward pass did not attempt COMMIT in this invocation. One serialized
    // observation can find a concurrent winner, but receipt absence is not a death fence.
    fn semantic_generation_failure(
        &self,
        ctx: &AdminContext,
        target: &SemanticTarget,
        expected: i64,
        operation: &str,
        error: Error,
    ) -> Result<SemanticStageOutcome> {
        let recovery = (|| -> Result<Option<SemanticStageOutcome>> {
            ctx.validate()?;
            let conn = self.connection()?;
            ownership::validate_connection(&conn, ctx)?;
            let tx = begin_write(&conn, &self.revision_state)?;
            if let Some(s) = stored(&tx, operation)? {
                return replay(s, ctx, target, expected).map(Some);
            }
            Ok(None)
        })();
        match recovery{Ok(Some(s))=>Ok(s),Ok(None)=>Err(Error::Storage(format!("semantic generation failed; NoCommitThisInvocation; operation NotObserved at serialized barrier (receipt absence is not rollback/death proof): {error}"))),Err(observation)=>Err(Error::Storage(format!("semantic generation failed; NoCommitThisInvocation; operation UNKNOWN/unobservable (no COMMIT attempted by this invocation): {error}; receipt observation: {observation}")))}
    }
    pub(crate) fn semantic_stage_inspect(
        &self,
        ctx: &AdminContext,
        id: &str,
    ) -> Result<SemanticStageInspection> {
        ctx.validate()?;
        if !crate::core::is_sha256_hex(id) {
            return Err(refuse("invalid semantic artifact identity"));
        }
        let conn = self.connection()?;
        ownership::validate_connection(&conn, ctx)?;
        let tx = begin_write(&conn, &self.revision_state)?;
        let s = stored_for_semantic(&tx, id)?
            .ok_or_else(|| refuse("semantic artifact does not exist"))?;
        let (canonical_integrity, vector_integrity, error) =
            match semantic_input::build(&tx, &s.intent.input.target) {
                Err(error) => ("corrupt", "not_checked", Some(error.to_string())),
                Ok(source) => match verify_stored(&tx, &source, &s) {
                    Ok(()) => ("verified", "verified", None),
                    Err(error) => ("verified", "corrupt", Some(error.to_string())),
                },
            };
        let out = SemanticStageInspection {
            semantic_id: id.to_owned(),
            verified_at_revision: s.revision,
            vector_count: s.intent.input.lexical.entry_count,
            historical_verified: true,
            canonical_integrity,
            vector_integrity,
            fresh_tokens: "not_checked",
            lexical_fts_integrity: "not_checked",
            encoder_availability: "not_checked",
            original_bytes_status: "not_checked",
            error,
        };
        drop(tx);
        ctx.validate()?;
        Ok(out)
    }
}
#[cfg(feature = "spawn-gate")]
fn configure_faults() {
    match std::env::var("MINDSPACE_SEMANTIC_STAGE_FAULT").as_deref() {
        Ok("veto") => gate::FAIL_COMMIT.store(true, Ordering::SeqCst),
        Ok("lost_ack") => gate::LOSE_ACK.store(true, Ordering::SeqCst),
        _ => {}
    }
    if std::env::var("MINDSPACE_SEMANTIC_STAGE_PAUSE").as_deref() == Ok("in_commit") {
        if let Some(p) = std::env::var_os("MINDSPACE_SEMANTIC_STAGE_GATE_PATH") {
            std::env::set_var("MINDSPACE_STORE_GATE_PAUSE_IN_COMMIT", p);
            gate::PAUSE_IN_COMMIT.store(true, Ordering::SeqCst)
        }
    }
}
#[cfg(feature = "spawn-gate")]
fn test_pause(phase: &str) {
    if std::env::var("MINDSPACE_SEMANTIC_STAGE_PAUSE").as_deref() != Ok(phase) {
        return;
    }
    let Some(p) = std::env::var_os("MINDSPACE_SEMANTIC_STAGE_GATE_PATH") else {
        return;
    };
    let path = |suffix: &str| {
        let mut v = p.clone();
        v.push(suffix);
        std::path::PathBuf::from(v)
    };
    let _ = std::fs::write(path(".reached"), b"");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !path(".go").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[cfg(test)]
#[path = "semantic_staging_tests.rs"]
mod tests;
