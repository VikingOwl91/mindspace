//! Private administrative construction of immutable, unpublished lexical artifacts.
pub(crate) use super::lexical_input::LexicalStageTarget;
use super::lexical_input::{self, LexicalCandidate, LexicalEntry, LexicalInput};
use super::*;
#[cfg(unix)]
use crate::cli::ownership::AdminContext;
use serde::{Deserialize, Serialize};

const SQL: &str = include_str!("../../../migrations/0015_lexical_staging.sql");
const DOMAIN: &str = "mindspace.s3d.local-lexical-stage@1";
const ACTOR: &str = "local-os-account@1";
const ACTION: &str = "stage_document_lexical@1";
const MAX_REQUEST: usize = 65536;
const SET_COLUMNS: &str = "stage_id,asset_id,source_id,document_id,preparation_operation_id,preparation_revision,legacy_source_version,document_source_version,extraction_id,chunker_version,budget_identity,plan_hash,input_manifest,input_json,lexical_descriptor,citation_policy,entry_count,source_bytes_len,metadata_bytes,result_digest,operation_id,created_revision,state,token_assurance";
const ENTRY_COLUMNS: &str = "entry_id,stage_id,ordinal,chunk_id,text_start,text_end,content,payload_hash,anchor,context_json,citation_state,original_bytes_state";
const RECEIPT_COLUMNS: &str = "operation_id,nonce,digest,canonical_request,intent_json,request_version,db_path,db_dev,db_ino,actor_kind,actor_uid,expected_revision,stage_id,result_digest,committed_revision,action,expected_state,intended_state";

#[derive(Default)]
pub(super) struct LexicalState {
    permit: Mutex<Option<Permit>>,
}
struct Permit {
    set: Vec<Value>,
    entries: HashMap<String, Vec<Value>>,
    receipt: Vec<Value>,
}
struct PermitScope<'a>(&'a LexicalState);
impl Drop for PermitScope<'_> {
    fn drop(&mut self) {
        if let Ok(mut p) = self.0.permit.lock() {
            *p = None;
        }
    }
}
pub(super) fn register_context(conn: &Connection, state: &Arc<LexicalState>) -> Result<()> {
    for (name, kind) in [
        ("s3d_set_valid", 0),
        ("s3d_entry_valid", 1),
        ("s3d_receipt_valid", 2),
    ] {
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
                let values = match kind {
                    0 => Some(&p.set),
                    2 => Some(&p.receipt),
                    _ => p.entries.get(&ctx.get::<String>(0)?),
                };
                let Some(values) = values else {
                    return Ok(false);
                };
                if ctx.len() != values.len() {
                    return Ok(false);
                }
                for (n, v) in values.iter().enumerate() {
                    if ctx.get::<Value>(n)? != *v {
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
        "s3d_availability_valid",
        3,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS,
        move |ctx| {
            let Ok(guard) = state.permit.lock() else {
                return Ok(false);
            };
            let Some(p) = guard.as_ref() else {
                return Ok(false);
            };
            let key: Value = ctx.get(1)?;
            let valid = match ctx.get::<String>(0)?.as_str() {
                "doc_staged_lexical_sets" => key == p.set[0],
                "doc_stage_receipts" => key == p.receipt[0],
                "doc_staged_entries" => match key {
                    Value::Text(k) => p.entries.contains_key(&k),
                    _ => false,
                },
                _ => false,
            };
            Ok(valid && ctx.get::<Value>(2)? == p.set[21])
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
    Error::Storage(format!("lexical staging refused: {s}"))
}
fn string(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u64).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
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
    if p.len() != 3 || p[0] != "local-doc-stage-lexical@1" || !crate::core::is_sha256_hex(p[2]) {
        return Err(refuse("invalid full operation identity"));
    }
    let n = Uuid::parse_str(p[1]).map_err(|_| refuse("invalid nonce"))?;
    if n.to_string() != p[1] {
        return Err(refuse("noncanonical nonce"));
    }
    Ok((n, p[2]))
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
    input: LexicalInput,
    result_digest: String,
}
impl Intent {
    fn canonical(&self) -> Result<Vec<u8>> {
        self.input.validate()?;
        if self.expected_revision <= 0 || !crate::core::is_sha256_hex(&self.result_digest) {
            return Err(refuse("invalid intent metadata"));
        }
        let mut out = Vec::new();
        string(&mut out, DOMAIN);
        out.extend_from_slice(self.nonce.as_bytes());
        string(&mut out, &self.db_path);
        out.extend_from_slice(&self.db_dev.to_be_bytes());
        out.extend_from_slice(&self.db_ino.to_be_bytes());
        string(&mut out, ACTOR);
        out.extend_from_slice(&(self.actor_uid as u64).to_be_bytes());
        let artifact = self.input.artifact_canonical();
        out.extend_from_slice(&(artifact.len() as u64).to_be_bytes());
        out.extend_from_slice(&artifact);
        out.extend_from_slice(&self.input.preparation_revision.to_be_bytes());
        out.extend_from_slice(&self.input.metadata_bytes.to_be_bytes());
        out.extend_from_slice(&(self.input.logical_base.len() as u64).to_be_bytes());
        out.extend_from_slice(&self.input.logical_base);
        string(&mut out, &self.result_digest);
        out.extend_from_slice(&self.expected_revision.to_be_bytes());
        for s in [
            ACTION,
            "NoLexicalArtifact",
            "CreateVerifiedUnpublishedLexicalStage",
        ] {
            string(&mut out, s)
        }
        if out.len() > MAX_REQUEST {
            return Err(refuse("canonical intent exceeds 64 KiB"));
        }
        Ok(out)
    }
    fn digest(&self) -> Result<String> {
        Ok(sha256_hex(&self.canonical()?))
    }
    fn operation_id(&self) -> Result<String> {
        Ok(format!(
            "local-doc-stage-lexical@1:{}:{}",
            self.nonce,
            self.digest()?
        ))
    }
    fn set_values(&self, operation: &str, r: i64) -> Result<Vec<Value>> {
        let i = &self.input;
        let t = &i.target;
        Ok(vec![
            i.stage_id.clone().into(),
            t.asset_id.to_string().into(),
            i.source_id.to_string().into(),
            t.document_id.to_string().into(),
            i.preparation_operation_id.clone().into(),
            i.preparation_revision.into(),
            t.legacy_source_version.to_string().into(),
            t.document_source_version.to_string().into(),
            t.extraction_id.to_string().into(),
            t.chunker_version.clone().into(),
            t.budget_identity.clone().into(),
            t.plan_hash.clone().into(),
            i.input_manifest.clone().into(),
            bounded_json(i)?.into(),
            i.lexical_descriptor.clone().into(),
            i.citation_policy.clone().into(),
            (i.entry_count as i64).into(),
            (i.source_bytes_len as i64).into(),
            (i.metadata_bytes as i64).into(),
            self.result_digest.clone().into(),
            operation.to_owned().into(),
            r.into(),
            "verified_lexical_unpublished".to_owned().into(),
            "not_performed_lexical".to_owned().into(),
        ])
    }
    fn receipt_values(&self, operation: &str, r: i64) -> Result<Vec<Value>> {
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
            self.input.stage_id.clone().into(),
            self.result_digest.clone().into(),
            r.into(),
            ACTION.to_owned().into(),
            "NoLexicalArtifact".to_owned().into(),
            "CreateVerifiedUnpublishedLexicalStage".to_owned().into(),
        ])
    }
}
fn entry_values(stage: &str, e: &LexicalEntry) -> Vec<Value> {
    vec![
        e.entry_id.clone().into(),
        stage.to_owned().into(),
        (e.ordinal as i64).into(),
        e.chunk_id.clone().into(),
        (e.start as i64).into(),
        (e.end as i64).into(),
        e.content.clone().into(),
        e.payload_hash.clone().into(),
        e.anchor.clone().into(),
        e.context_json.clone().into(),
        e.citation_state.clone().into(),
        e.original_bytes_state.clone().into(),
    ]
}
fn insert_values(conn: &Connection, table: &str, cols: &str, values: &[Value]) -> Result<()> {
    let ps = (1..=values.len())
        .map(|n| format!("?{n}"))
        .collect::<Vec<_>>()
        .join(",");
    conn.execute(
        &format!("INSERT INTO {table}({cols}) VALUES({ps})"),
        rusqlite::params_from_iter(values),
    )
    .map_err(dberr)?;
    Ok(())
}
fn select_values(
    conn: &Connection,
    table: &str,
    cols: &str,
    key: &str,
    id: &str,
    count: usize,
) -> Result<Option<Vec<Value>>> {
    conn.query_row(
        &format!("SELECT {cols} FROM {table} WHERE {key}=?1"),
        [id],
        |r| {
            (0..count)
                .map(|n| r.get(n))
                .collect::<rusqlite::Result<Vec<Value>>>()
        },
    )
    .optional()
    .map_err(dberr)
}
struct Stored {
    intent: Intent,
    revision: i64,
}
fn stored(conn: &Connection, operation: &str) -> Result<Option<Stored>> {
    let Some(values) = select_values(
        conn,
        "doc_stage_receipts",
        RECEIPT_COLUMNS,
        "operation_id",
        operation,
        18,
    )?
    else {
        return Ok(None);
    };
    let (Value::Blob(json), Value::Integer(revision)) = (&values[4], &values[14]) else {
        return Err(refuse("invalid stored receipt types"));
    };
    if json.len() > MAX_REQUEST {
        return Err(refuse("oversized stored intent"));
    }
    let i: Intent =
        serde_json::from_slice(json).map_err(|_| refuse("invalid stored intent encoding"))?;
    if i.operation_id()? != operation
        || i.expected_revision.checked_add(1) != Some(*revision)
        || i.receipt_values(operation, *revision)? != values
    {
        return Err(refuse("stored receipt conflicts with immutable intent"));
    }
    if select_values(
        conn,
        "doc_staged_lexical_sets",
        SET_COLUMNS,
        "stage_id",
        &i.input.stage_id,
        24,
    )?
    .as_ref()
        != Some(&i.set_values(operation, *revision)?)
    {
        return Err(refuse("stored artifact and receipt disagree"));
    }
    Ok(Some(Stored {
        intent: i,
        revision: *revision,
    }))
}
#[derive(Serialize)]
pub(crate) struct LexicalStagePreview {
    pub operation_id: String,
    pub digest: String,
    pub stage_id: String,
    pub expected_revision: i64,
    #[serde(flatten)]
    pub target: LexicalStageTarget,
    pub source_id: Uuid,
    pub entry_count: u64,
    pub source_bytes_len: u64,
    pub db_path: String,
    pub db_dev: u64,
    pub db_ino: u64,
    pub actor_kind: &'static str,
    pub actor_uid: u32,
    pub preparation_operation_id: String,
    pub preparation_revision: i64,
    pub provenance_digest: String,
    pub logical_base_digest: String,
    pub input_manifest: String,
    pub result_digest: String,
    pub lexical_descriptor: String,
    pub citation_policy: String,
    pub token_assurance: &'static str,
}
#[derive(Serialize)]
pub(crate) struct LexicalStageOutcome {
    pub outcome: &'static str,
    pub operation_id: Option<String>,
    pub stage_id: String,
    pub committed_revision: Option<i64>,
    pub result_digest: String,
    pub current_integrity: &'static str,
}
#[derive(Serialize)]
pub(crate) struct LexicalStageInspection {
    pub stage_id: String,
    pub verified_at_revision: i64,
    pub entry_count: u64,
    pub canonical_verified_at_commit: bool,
    pub current_integrity: &'static str,
    pub token_assurance: &'static str,
    pub original_bytes_status: &'static str,
    pub error: Option<String>,
}
fn outcome(i: &Intent, r: i64) -> Result<LexicalStageOutcome> {
    Ok(LexicalStageOutcome {
        outcome: "committed",
        operation_id: Some(i.operation_id()?),
        stage_id: i.input.stage_id.clone(),
        committed_revision: Some(r),
        result_digest: i.result_digest.clone(),
        current_integrity: "not_checked",
    })
}
#[cfg(unix)]
fn replay(
    s: Stored,
    ctx: &AdminContext,
    target: &LexicalStageTarget,
    expected: i64,
) -> Result<LexicalStageOutcome> {
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
    outcome(i, s.revision)
}
#[cfg(unix)]
fn intent(ctx: &AdminContext, nonce: Uuid, expected: i64, c: &LexicalCandidate) -> Result<Intent> {
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
        input: c.input.clone(),
        result_digest: c.result_digest(),
    })
}
fn arm<'a>(
    state: &'a LexicalState,
    i: &Intent,
    c: &LexicalCandidate,
    op: &str,
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
        set: i.set_values(op, r)?,
        entries: c
            .entries
            .iter()
            .map(|e| (e.entry_id.clone(), entry_values(&i.input.stage_id, e)))
            .collect(),
        receipt: i.receipt_values(op, r)?,
    });
    Ok(PermitScope(state))
}
fn insert_candidate(
    conn: &Connection,
    i: &Intent,
    c: &LexicalCandidate,
    op: &str,
    r: i64,
) -> Result<()> {
    insert_values(
        conn,
        "doc_staged_lexical_sets",
        SET_COLUMNS,
        &i.set_values(op, r)?,
    )?;
    for e in &c.entries {
        insert_values(
            conn,
            "doc_staged_entries",
            ENTRY_COLUMNS,
            &entry_values(&i.input.stage_id, e),
        )?;
        #[cfg(feature = "spawn-gate")]
        if std::env::var("MINDSPACE_LEXICAL_STAGE_FAULT").as_deref() == Ok("entry:1") {
            return Err(refuse("injected partial lexical build failure"));
        }
    }
    insert_values(
        conn,
        "doc_stage_receipts",
        RECEIPT_COLUMNS,
        &i.receipt_values(op, r)?,
    )?;
    #[cfg(feature = "spawn-gate")]
    if std::env::var("MINDSPACE_LEXICAL_STAGE_FAULT").as_deref() == Ok("receipt") {
        return Err(refuse("injected receipt failure"));
    }
    verify_cache(conn)
}
fn verify_cache(conn: &Connection) -> Result<()> {
    // FTS is a mutable cache: its actual tokenizer/table contract must match the pinned codec.
    let schema: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='doc_staged_fts' AND type='table'",
            [],
            |r| r.get(0),
        )
        .map_err(dberr)?;
    if schema != "CREATE VIRTUAL TABLE doc_staged_fts USING fts5(content,content='doc_staged_entries',content_rowid='entry_rowid',tokenize='unicode61 remove_diacritics 1',detail=full)" {
        return Err(refuse("incompatible staged FTS schema"));
    }
    conn.execute(
        "INSERT INTO doc_staged_fts(doc_staged_fts,rank) VALUES('integrity-check',1)",
        [],
    )
    .map_err(dberr)?;
    Ok(())
}
/// Full exact comparison against a freshly bounded, citation-validated source snapshot.
fn verify_canonical_candidate(conn: &Connection, c: &LexicalCandidate) -> Result<Stored> {
    let op: String = conn
        .query_row(
            "SELECT operation_id FROM doc_staged_lexical_sets WHERE stage_id=?1",
            [&c.input.stage_id],
            |r| r.get(0),
        )
        .map_err(dberr)?;
    let s = stored(conn, &op)?.ok_or_else(|| refuse("missing immutable receipt"))?;
    if s.intent.input.artifact_canonical() != c.input.artifact_canonical()
        || s.intent.result_digest != c.result_digest()
    {
        return Err(refuse("current artifact input or result differs"));
    }
    let mut stmt=conn.prepare(&format!("SELECT {ENTRY_COLUMNS} FROM doc_staged_entries WHERE stage_id=?1 ORDER BY ordinal LIMIT 8193")).map_err(dberr)?;
    let actual = stmt
        .query_map([&c.input.stage_id], |r| {
            (0..12)
                .map(|n| r.get::<_, Value>(n))
                .collect::<rusqlite::Result<Vec<Value>>>()
        })
        .map_err(dberr)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(dberr)?;
    if actual.len() != c.entries.len()
        || actual
            .iter()
            .zip(&c.entries)
            .any(|(a, e)| *a != entry_values(&c.input.stage_id, e))
    {
        return Err(refuse(
            "staged canonical entries differ from exact citations",
        ));
    }
    Ok(s)
}
fn verify_candidate(conn: &Connection, c: &LexicalCandidate) -> Result<Stored> {
    let stored = verify_canonical_candidate(conn, c)?;
    verify_cache(conn)?;
    Ok(stored)
}
/// Canonical source evidence is independent of the mutable lexical FTS cache.
/// The existing lexical callers still validate both canonical rows and FTS.
pub(super) struct CanonicalParent {
    pub candidate: LexicalCandidate,
    pub operation_id: String,
    pub revision: i64,
    pub result_digest: String,
}
pub(super) fn canonical_parent(conn: &Connection, stage: &str) -> Result<CanonicalParent> {
    if !crate::core::is_sha256_hex(stage) {
        return Err(refuse("invalid lexical parent identity"));
    }
    let operation: String = conn
        .query_row(
            "SELECT operation_id FROM doc_staged_lexical_sets WHERE stage_id=?1",
            [stage],
            |r| r.get(0),
        )
        .optional()
        .map_err(dberr)?
        .ok_or_else(|| refuse("lexical parent does not exist"))?;
    let original =
        stored(conn, &operation)?.ok_or_else(|| refuse("lexical parent receipt is missing"))?;
    let mut candidate = lexical_input::build(conn, &original.intent.input.target)?;
    // Use only the immutable descriptor authenticated by its original receipt.
    // Engine compatibility of the current mutable FTS cache is not asserted here.
    lexical_input::pin_historical_descriptor(
        &mut candidate,
        &original.intent.input.lexical_descriptor,
    )?;
    let verified = verify_canonical_candidate(conn, &candidate)?;
    Ok(CanonicalParent {
        candidate,
        operation_id: operation,
        revision: verified.revision,
        result_digest: verified.intent.result_digest,
    })
}

#[cfg(test)]
pub(super) fn enroll_test_lexical(store: &SqliteStore, target: &LexicalStageTarget) -> String {
    enroll_test_lexical_descriptor(store, target, None)
}
#[cfg(test)]
pub(super) fn enroll_test_lexical_descriptor(
    store: &SqliteStore,
    target: &LexicalStageTarget,
    descriptor: Option<&str>,
) -> String {
    let conn = store.connection().unwrap();
    let tx = begin_write(&conn, &store.revision_state).unwrap();
    let r = store.revision_state.active_revision.load(Ordering::SeqCst);
    let mut candidate = lexical_input::build(&tx, target).unwrap();
    if let Some(descriptor) = descriptor {
        lexical_input::pin_historical_descriptor(&mut candidate, descriptor).unwrap();
    }
    let intent = Intent {
        nonce: Uuid::now_v7(),
        db_path: "/unit-lexical-fixture.db".into(),
        db_dev: 1,
        db_ino: 1,
        actor_uid: 1000,
        expected_revision: r - 1,
        input: candidate.input.clone(),
        result_digest: candidate.result_digest(),
    };
    let op = intent.operation_id().unwrap();
    let _permit = arm(&store.lexical_state, &intent, &candidate, &op, r).unwrap();
    insert_candidate(&tx, &intent, &candidate, &op, r).unwrap();
    tx.commit().unwrap();
    candidate.input.stage_id
}
#[cfg(unix)]
impl SqliteStore {
    pub(crate) fn lexical_stage_preview(
        &self,
        ctx: &AdminContext,
        target: &LexicalStageTarget,
        expected: i64,
        nonce: Uuid,
    ) -> Result<LexicalStagePreview> {
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
        let c = lexical_input::build(&read, target)?;
        let i = intent(ctx, nonce, expected, &c)?;
        let op = i.operation_id()?;
        i.receipt_values(
            &op,
            expected
                .checked_add(1)
                .ok_or_else(|| refuse("revision overflow"))?,
        )?;
        let out = LexicalStagePreview {
            operation_id: op,
            digest: i.digest()?,
            stage_id: i.input.stage_id.clone(),
            expected_revision: expected,
            target: target.clone(),
            source_id: i.input.source_id,
            entry_count: i.input.entry_count,
            source_bytes_len: i.input.source_bytes_len,
            db_path: i.db_path.clone(),
            db_dev: i.db_dev,
            db_ino: i.db_ino,
            actor_kind: ACTOR,
            actor_uid: i.actor_uid,
            preparation_operation_id: i.input.preparation_operation_id.clone(),
            preparation_revision: i.input.preparation_revision,
            provenance_digest: sha256_hex(&i.input.provenance),
            logical_base_digest: sha256_hex(&i.input.logical_base),
            input_manifest: i.input.input_manifest.clone(),
            result_digest: i.result_digest.clone(),
            lexical_descriptor: i.input.lexical_descriptor.clone(),
            citation_policy: i.input.citation_policy.clone(),
            token_assurance: "not_performed_lexical",
        };
        read.rollback().map_err(dberr)?;
        Ok(out)
    }
    pub(crate) fn lexical_stage_apply(
        &self,
        ctx: &AdminContext,
        target: &LexicalStageTarget,
        expected: i64,
        operation: &str,
        confirmation: &str,
    ) -> Result<LexicalStageOutcome> {
        ctx.validate()?;
        let (nonce, digest) = parse_id(operation)?;
        if digest != confirmation {
            return Err(refuse("exact intent confirmation required"));
        }
        let conn = self.connection()?;
        ownership::validate_connection(&conn, ctx)?;
        if let Some(s) = stored(&conn, operation)? {
            return replay(s, ctx, target, expected);
        }
        #[cfg(feature = "spawn-gate")]
        test_pause("before_build");
        let read =
            Transaction::new_unchecked(&conn, TransactionBehavior::Deferred).map_err(dberr)?;
        let prepared = lexical_input::build(&read, target)?;
        let pre = intent(ctx, nonce, expected, &prepared)?;
        if pre.operation_id()? != operation {
            return Err(refuse(
                "operation identity conflicts with immutable parameters",
            ));
        }
        read.rollback().map_err(dberr)?;
        drop(prepared);
        #[cfg(feature = "spawn-gate")]
        test_pause("before_begin");
        let tx = begin_write(&conn, &self.revision_state)?;
        #[cfg(feature = "spawn-gate")]
        test_pause("after_begin");
        if let Some(s) = stored(&tx, operation)? {
            return replay(s, ctx, target, expected);
        }
        let c = lexical_input::build(&tx, target)?;
        let i = intent(ctx, nonce, expected, &c)?;
        if i.operation_id()? != operation {
            return Err(refuse(
                "source or intended transition changed before commit",
            ));
        }
        if tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM doc_stage_receipts WHERE nonce=?1)",
                [nonce.to_string()],
                |r| r.get::<_, bool>(0),
            )
            .map_err(dberr)?
        {
            return Err(refuse("committed nonce has different intent"));
        }
        if tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM doc_staged_lexical_sets WHERE stage_id=?1)",
                [&i.input.stage_id],
                |r| r.get::<_, bool>(0),
            )
            .map_err(dberr)?
        {
            let s = verify_candidate(&tx, &c)?;
            return Ok(LexicalStageOutcome {
                outcome: "existing_artifact",
                operation_id: None,
                stage_id: i.input.stage_id,
                committed_revision: None,
                result_digest: s.intent.result_digest,
                current_integrity: "verified",
            });
        }
        let r = self.revision_state.active_revision.load(Ordering::SeqCst);
        if r.checked_sub(1) != Some(expected) {
            return Err(refuse("stale expected recording revision"));
        }
        let scope = arm(&self.lexical_state, &i, &c, operation, r)?;
        insert_candidate(&tx, &i, &c, operation, r)?;
        #[cfg(feature = "spawn-gate")]
        test_pause("after_write");
        #[cfg(feature = "spawn-gate")]
        test_pause("before_commit");
        ctx.validate()?;
        #[cfg(feature = "spawn-gate")]
        configure_faults();
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
        outcome(&i, r)
    }
    pub(crate) fn lexical_stage_inspect(
        &self,
        ctx: &AdminContext,
        stage: &str,
    ) -> Result<LexicalStageInspection> {
        ctx.validate()?;
        if !crate::core::is_sha256_hex(stage) {
            return Err(refuse("invalid staged artifact identity"));
        }
        let conn = self.connection()?;
        ownership::validate_connection(&conn, ctx)?;
        let tx = begin_write(&conn, &self.revision_state)?;
        let op: String = tx
            .query_row(
                "SELECT operation_id FROM doc_staged_lexical_sets WHERE stage_id=?1",
                [stage],
                |r| r.get(0),
            )
            .optional()
            .map_err(dberr)?
            .ok_or_else(|| refuse("staged artifact does not exist"))?;
        let s = stored(&tx, &op)?.ok_or_else(|| refuse("missing staged receipt"))?;
        let check = lexical_input::build(&tx, &s.intent.input.target)
            .and_then(|c| verify_candidate(&tx, &c).map(|_| ()));
        let error = check.err().map(|e| e.to_string());
        let status = if error.is_none() {
            "verified"
        } else {
            "corrupt"
        };
        let result = LexicalStageInspection {
            stage_id: stage.to_owned(),
            verified_at_revision: s.revision,
            entry_count: s.intent.input.entry_count,
            canonical_verified_at_commit: true,
            current_integrity: status,
            token_assurance: "not_performed_lexical",
            original_bytes_status: "not_checked",
            error,
        };
        // Writer barrier is rolled back: inspection never records a revision or repairs the cache.
        drop(tx);
        ctx.validate()?;
        Ok(result)
    }
}
#[cfg(feature = "spawn-gate")]
fn configure_faults() {
    match std::env::var("MINDSPACE_LEXICAL_STAGE_FAULT").as_deref() {
        Ok("veto") => gate::FAIL_COMMIT.store(true, Ordering::SeqCst),
        Ok("lost_ack") => gate::LOSE_ACK.store(true, Ordering::SeqCst),
        _ => {}
    }
    if std::env::var("MINDSPACE_LEXICAL_STAGE_PAUSE").as_deref() == Ok("in_commit") {
        if let Some(prefix) = std::env::var_os("MINDSPACE_LEXICAL_STAGE_GATE_PATH") {
            std::env::set_var("MINDSPACE_STORE_GATE_PAUSE_IN_COMMIT", prefix);
            gate::PAUSE_IN_COMMIT.store(true, Ordering::SeqCst);
        }
    }
}
#[cfg(feature = "spawn-gate")]
fn test_pause(phase: &str) {
    if std::env::var("MINDSPACE_LEXICAL_STAGE_PAUSE").as_deref() != Ok(phase) {
        return;
    }
    let Some(prefix) = std::env::var_os("MINDSPACE_LEXICAL_STAGE_GATE_PATH") else {
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
#[path = "lexical_staging_tests.rs"]
mod tests;
