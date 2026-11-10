//! Immutable advisory plans. Nothing in this module fences an ordinary writer.
//! Authorization functions are referenced exclusively by triggers on the new tables.
use super::document_preparation::PreparationTarget;
use super::*;
#[cfg(unix)]
use crate::cli::ownership::AdminContext;
use serde::{Deserialize, Serialize};

const SQL: &str = include_str!("../../../migrations/0017_passive_fence_plans.sql");
const DOMAIN: &str = "mindspace.s3e2.local-passive-fence-plan@1";
const ACTOR: &str = "local-os-account@1";
const ACTION: &str = "prepare_fence_plan@1";
const POLICY: &str = "passive-fence-plan@1";
const WITNESS_STATUS: &str = "unverified_operator_statement";
const MAX_REQUEST: usize = 65536;
const MAX_COHORT: usize = 1024 * 1024;
const PLAN_COLUMNS: &str = "plan_id,asset_id,source_id,document_id,preparation_operation_id,preparation_revision,legacy_source_version,document_source_version,extraction_id,snapshot_digest,bindings_digest,binding_count,expected_revision,operation_id,created_revision,state,policy";
const BINDING_COLUMNS: &str =
    "plan_id,ordinal,namespace,binding_key,witness_digest,witness_status,created_revision";
const RECEIPT_COLUMNS: &str = "operation_id,nonce,digest,canonical_request,intent_json,request_version,db_path,db_dev,db_ino,actor_kind,actor_uid,expected_revision,plan_id,committed_revision,action,expected_state,intended_state";

#[derive(Default)]
pub(super) struct FencePlanState {
    permit: Mutex<Option<Permit>>,
}
struct Permit {
    plan: Vec<Value>,
    bindings: Vec<Vec<Value>>,
    receipt: Vec<Value>,
}
struct PermitScope<'a>(&'a FencePlanState);
impl Drop for PermitScope<'_> {
    fn drop(&mut self) {
        if let Ok(mut permit) = self.0.permit.lock() {
            *permit = None;
        }
    }
}
pub(super) fn register_context(conn: &Connection, state: &Arc<FencePlanState>) -> Result<()> {
    for (name, kind) in [
        ("s3e2_plan_valid", 0),
        ("s3e2_binding_valid", 1),
        ("s3e2_receipt_valid", 2),
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
                let Some(permit) = guard.as_ref() else {
                    return Ok(false);
                };
                let values = match kind {
                    0 => Some(&permit.plan),
                    2 => Some(&permit.receipt),
                    _ => ctx
                        .get::<i64>(1)
                        .ok()
                        .and_then(|n| usize::try_from(n).ok())
                        .and_then(|n| permit.bindings.get(n)),
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
/// Fixed passive schema admission, including guard bodies rather than just names.
/// This is corruption/compatibility detection under Model A, not DB authentication.
pub(super) fn validate_schema(conn: &Connection) -> Result<()> {
    let normalize = |sql: &str| sql.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut definitions = Vec::new();
    for part in SQL.split("CREATE TABLE ").skip(1) {
        let body = part
            .split(';')
            .next()
            .ok_or_else(|| refuse("invalid compiled table contract"))?;
        definitions.push(("table", format!("CREATE TABLE {body}")));
    }
    for part in SQL.split("CREATE TRIGGER ").skip(1) {
        definitions.push((
            "trigger",
            format!("CREATE TRIGGER {}", part.trim().trim_end_matches(';')),
        ));
    }
    for (kind, expected) in definitions {
        let name = expected
            .split_whitespace()
            .nth(2)
            .ok_or_else(|| refuse("invalid compiled schema contract"))?;
        let actual: Option<(String, String)> = conn
            .query_row(
                "SELECT type,sql FROM sqlite_schema WHERE name=?1",
                [name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(dberr)?;
        if !actual.is_some_and(|(actual_kind, sql)| {
            actual_kind == kind
                && normalize(sql.trim().trim_end_matches(';')) == normalize(&expected)
        }) {
            return Err(refuse(&format!(
                "passive schema definition is missing or incompatible: {name}"
            )));
        }
    }
    Ok(())
}

fn refuse(message: &str) -> Error {
    Error::Storage(format!("passive fence plan refused: {message}"))
}
fn string(out: &mut Vec<u8>, value: &str) {
    bytes(out, value.as_bytes())
}
fn bytes(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value);
}
fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value).map_err(|_| refuse("cannot encode durable intent"))?;
    if bytes.len() > MAX_REQUEST {
        return Err(refuse("durable intent exceeds 64 KiB"));
    }
    Ok(bytes)
}
fn parse_id(operation: &str) -> Result<(Uuid, &str)> {
    let parts: Vec<_> = operation.split(':').collect();
    if parts.len() != 3 || parts[0] != "local-fence-plan@1" || !crate::core::is_sha256_hex(parts[2])
    {
        return Err(refuse("full passive plan operation identity required"));
    }
    let nonce = Uuid::parse_str(parts[1]).map_err(|_| refuse("invalid operation nonce"))?;
    if nonce.to_string() != parts[1] {
        return Err(refuse("noncanonical operation nonce"));
    }
    Ok((nonce, parts[2]))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FenceBinding {
    pub namespace: String,
    pub key: String,
    pub witness_digest: String,
}
impl FenceBinding {
    fn validate(&self) -> Result<()> {
        if self.key.is_empty()
            || self.key.len() > 4096
            || self.key.contains('\0')
            || !crate::core::is_sha256_hex(&self.witness_digest)
        {
            return Err(refuse("invalid bounded proposed binding or witness digest"));
        }
        match self.namespace.as_str() {
            "opaque@1" => {}
            "file-absolute@1" => {
                if !self.key.starts_with('/')
                    || self.key.contains("//")
                    || self.key.contains("/./")
                    || self.key.contains("/../")
                    || self.key.ends_with("/.")
                    || self.key.ends_with("/..")
                    || (self.key != "/" && self.key.ends_with('/'))
                {
                    return Err(refuse(
                        "file binding must be syntactically normalized absolute text",
                    ));
                }
            }
            _ => return Err(refuse("unsupported proposed binding namespace")),
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FencePlanTarget {
    pub selected: PreparationTarget,
    pub bindings: Vec<FenceBinding>,
}
impl FencePlanTarget {
    fn validate(&self) -> Result<()> {
        if self.bindings.len() > 128 {
            return Err(refuse(
                "at most 128 explicit proposed bindings are supported",
            ));
        }
        let mut seen = BTreeSet::new();
        for binding in &self.bindings {
            binding.validate()?;
            if !seen.insert((&binding.namespace, &binding.key)) {
                return Err(refuse("duplicate proposed binding within one plan"));
            }
        }
        encode(self)?;
        Ok(())
    }
    fn bindings_digest(&self) -> Result<String> {
        let mut b = Vec::new();
        string(&mut b, "mindspace.passive-proposed-bindings@1");
        bytes(&mut b, &encode(&self.bindings)?);
        string(&mut b, WITNESS_STATUS);
        Ok(sha256_hex(&b))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    source_id: Uuid,
    preparation_operation_id: String,
    preparation_revision: i64,
    provenance_digest: String,
    logical_base_digest: String,
    canonical_text_digest: String,
    structure_digest: String,
    cohort_digest: String,
    cohort_count: u64,
    cohort_bytes: u64,
}
impl Snapshot {
    fn digest(&self) -> Result<String> {
        let mut b = Vec::new();
        string(&mut b, "mindspace.passive-fence-snapshot@1");
        bytes(&mut b, &encode(self)?);
        Ok(sha256_hex(&b))
    }
}

/// Full typed current rows, not just caller labels or stored hash columns.
/// The explicit one-MiB profile is deliberately narrower than document ingestion.
fn cohort(conn: &Connection, source: Uuid) -> Result<(String, u64, u64)> {
    let mut b = Vec::new();
    string(&mut b, "mindspace.passive-active-cohort@1");
    let queries = [
        ("memories", "SELECT m.* FROM memories m JOIN assertions a ON a.id=m.assertion_id JOIN evidence e ON e.id=a.evidence_id JOIN sources s ON s.id=e.source_id WHERE s.logical_id=?1 AND m.state='active' ORDER BY m.id"),
        ("assertions", "SELECT DISTINCT a.* FROM assertions a JOIN memories m ON m.assertion_id=a.id JOIN evidence e ON e.id=a.evidence_id JOIN sources s ON s.id=e.source_id WHERE s.logical_id=?1 AND m.state='active' ORDER BY a.id"),
        ("evidence", "SELECT DISTINCT e.* FROM evidence e JOIN assertions a ON a.evidence_id=e.id JOIN memories m ON m.assertion_id=a.id JOIN sources s ON s.id=e.source_id WHERE s.logical_id=?1 AND m.state='active' ORDER BY e.id"),
        ("sources", "SELECT DISTINCT s.* FROM sources s JOIN evidence e ON e.source_id=s.id JOIN assertions a ON a.evidence_id=e.id JOIN memories m ON m.assertion_id=a.id WHERE s.logical_id=?1 AND m.state='active' ORDER BY s.id"),
        ("memberships", "SELECT ms.* FROM memory_spaces ms JOIN memories m ON m.id=ms.memory_id JOIN assertions a ON a.id=m.assertion_id JOIN evidence e ON e.id=a.evidence_id JOIN sources s ON s.id=e.source_id WHERE s.logical_id=?1 AND m.state='active' ORDER BY ms.memory_id,ms.space_id"),
    ];
    let mut count = 0;
    for (label, sql) in queries {
        string(&mut b, label);
        let mut stmt = conn.prepare(sql).map_err(dberr)?;
        let columns = stmt.column_count();
        let mut rows = stmt.query([source.to_string()]).map_err(dberr)?;
        while let Some(row) = rows.next().map_err(dberr)? {
            b.push(1);
            if label == "memories" {
                count += 1;
            }
            for n in 0..columns {
                // Inspect the borrowed value length before allocating/copying row data.
                let value = row.get_ref(n).map_err(dberr)?;
                use rusqlite::types::ValueRef;
                let required = match value {
                    ValueRef::Null => 1,
                    ValueRef::Integer(_) | ValueRef::Real(_) => 9,
                    ValueRef::Text(v) | ValueRef::Blob(v) => 9 + v.len(),
                };
                if required > MAX_COHORT.saturating_sub(b.len()) {
                    return Err(refuse(
                        "current active cohort exceeds one-MiB passive profile",
                    ));
                }
                match value {
                    ValueRef::Null => b.push(0),
                    ValueRef::Integer(v) => {
                        b.push(1);
                        b.extend_from_slice(&v.to_be_bytes());
                    }
                    ValueRef::Real(v) => {
                        b.push(2);
                        b.extend_from_slice(&v.to_bits().to_be_bytes());
                    }
                    ValueRef::Text(v) => {
                        b.push(3);
                        bytes(&mut b, v);
                    }
                    ValueRef::Blob(v) => {
                        b.push(4);
                        bytes(&mut b, v);
                    }
                }
            }
        }
        b.push(0);
        if b.len() > MAX_COHORT {
            return Err(refuse(
                "current active cohort exceeds one-MiB passive profile",
            ));
        }
    }
    Ok((sha256_hex(&b), count, b.len() as u64))
}
fn snapshot(conn: &Connection, target: &FencePlanTarget) -> Result<Snapshot> {
    target.validate()?;
    let t = &target.selected;
    let (operation, revision, source): (String, i64, String) = conn.query_row(
        "SELECT p.operation_id,p.created_revision,p.source_id FROM document_preparation_enrollments p JOIN document_preparation_receipts r ON r.operation_id=p.operation_id AND r.asset_id=p.asset_id AND r.source_id=p.source_id AND r.document_id=p.document_id AND r.committed_revision=p.created_revision WHERE p.asset_id=?1 AND p.document_id=?2 AND p.state='document_preparation_enrolled'",
        rusqlite::params![t.asset_id.to_string(),t.document_id.to_string()],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(dberr)?.ok_or_else(||refuse("matching existing document preparation required"))?;
    // Bound source loading before using the established hash/structure validator.
    let too_large: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sources WHERE id IN (?1,?2) AND octet_length(content)>16777216)",
        rusqlite::params![t.legacy_source_version.to_string(),t.document_source_version.to_string()],
        |r|r.get(0)).map_err(dberr)?;
    if too_large {
        return Err(refuse("selected source exceeds 16 MiB"));
    }
    let validated = document_preparation::validated_staging_snapshot(conn, t)?;
    if source != validated.source_id.to_string() {
        return Err(refuse("prepared legacy identity differs"));
    }
    let (cohort_digest, cohort_count, cohort_bytes) = cohort(conn, validated.source_id)?;
    Ok(Snapshot {
        source_id: validated.source_id,
        preparation_operation_id: operation,
        preparation_revision: revision,
        provenance_digest: sha256_hex(&validated.provenance),
        logical_base_digest: sha256_hex(&validated.logical_base),
        canonical_text_digest: validated.draft.text_hash(),
        structure_digest: validated.draft.structure_hash(),
        cohort_digest,
        cohort_count,
        cohort_bytes,
    })
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    nonce: Uuid,
    db_path: String,
    db_dev: u64,
    db_ino: u64,
    actor_uid: u32,
    expected_revision: i64,
    target: FencePlanTarget,
    snapshot: Snapshot,
}
impl Intent {
    fn canonical(&self) -> Result<Vec<u8>> {
        self.target.validate()?;
        if self.expected_revision <= 0 || self.snapshot.preparation_revision <= 0 {
            return Err(refuse(
                "positive expected recording/preparation revisions required",
            ));
        }
        let mut b = Vec::new();
        for v in [
            DOMAIN,
            ACTOR,
            ACTION,
            POLICY,
            "LegacyWritable",
            "PassivePlanOnly",
        ] {
            string(&mut b, v);
        }
        b.extend_from_slice(self.nonce.as_bytes());
        string(&mut b, &self.db_path);
        b.extend_from_slice(&self.db_dev.to_be_bytes());
        b.extend_from_slice(&self.db_ino.to_be_bytes());
        b.extend_from_slice(&(self.actor_uid as u64).to_be_bytes());
        b.extend_from_slice(&self.expected_revision.to_be_bytes());
        bytes(&mut b, &encode(&self.target)?);
        bytes(&mut b, &encode(&self.snapshot)?);
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
            "local-fence-plan@1:{}:{}",
            self.nonce,
            self.digest()?
        ))
    }
    fn plan_id(&self) -> Result<String> {
        let mut b = Vec::new();
        string(&mut b, "mindspace.passive-fence-artifact@1");
        string(&mut b, &self.digest()?);
        Ok(sha256_hex(&b))
    }
    fn plan_values(&self, revision: i64) -> Result<Vec<Value>> {
        let t = &self.target.selected;
        Ok(vec![
            self.plan_id()?.into(),
            t.asset_id.to_string().into(),
            self.snapshot.source_id.to_string().into(),
            t.document_id.to_string().into(),
            self.snapshot.preparation_operation_id.clone().into(),
            self.snapshot.preparation_revision.into(),
            t.legacy_source_version.to_string().into(),
            t.document_source_version.to_string().into(),
            t.extraction_id.to_string().into(),
            self.snapshot.digest()?.into(),
            self.target.bindings_digest()?.into(),
            (self.target.bindings.len() as i64).into(),
            self.expected_revision.into(),
            self.operation_id()?.into(),
            revision.into(),
            "passive".to_owned().into(),
            POLICY.to_owned().into(),
        ])
    }
    fn binding_values(&self, revision: i64) -> Result<Vec<Vec<Value>>> {
        let plan = self.plan_id()?;
        Ok(self
            .target
            .bindings
            .iter()
            .enumerate()
            .map(|(n, v)| {
                vec![
                    plan.clone().into(),
                    (n as i64).into(),
                    v.namespace.clone().into(),
                    v.key.clone().into(),
                    v.witness_digest.clone().into(),
                    WITNESS_STATUS.to_owned().into(),
                    revision.into(),
                ]
            })
            .collect())
    }
    fn receipt_values(&self, revision: i64) -> Result<Vec<Value>> {
        let json =
            String::from_utf8(encode(self)?).map_err(|_| refuse("invalid intent encoding"))?;
        Ok(vec![
            self.operation_id()?.into(),
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
            self.expected_revision.into(),
            self.plan_id()?.into(),
            revision.into(),
            ACTION.to_owned().into(),
            "LegacyWritable".to_owned().into(),
            "PassivePlanOnly".to_owned().into(),
        ])
    }
}

#[derive(Serialize)]
pub(crate) struct FencePlanPreview {
    pub operation_id: String,
    pub digest: String,
    pub plan_id: String,
    pub target: FencePlanTarget,
    pub source_id: Uuid,
    pub expected_revision: i64,
    pub snapshot_digest: String,
    pub cohort_count: u64,
    pub cohort_bytes: u64,
    pub binding_count: usize,
    pub canonical_intent: Vec<u8>,
    pub state: &'static str,
    pub active_fence: bool,
    pub witnesses_unverified: bool,
}
#[derive(Serialize)]
pub(crate) struct FencePlanReceipt {
    pub operation_id: String,
    pub digest: String,
    pub plan_id: String,
    pub target: FencePlanTarget,
    pub source_id: Uuid,
    pub expected_revision: i64,
    pub committed_revision: i64,
    pub state: &'static str,
    pub active_fence: bool,
    pub witnesses_unverified: bool,
}
fn receipt(i: &Intent, revision: i64) -> Result<FencePlanReceipt> {
    Ok(FencePlanReceipt {
        operation_id: i.operation_id()?,
        digest: i.digest()?,
        plan_id: i.plan_id()?,
        target: i.target.clone(),
        source_id: i.snapshot.source_id,
        expected_revision: i.expected_revision,
        committed_revision: revision,
        state: "passive",
        active_fence: false,
        witnesses_unverified: true,
    })
}
#[derive(Serialize)]
pub(crate) struct FencePlanInspection {
    pub receipt: FencePlanReceipt,
    pub freshness: &'static str,
    pub freshness_detail: String,
    pub current_revision: Option<i64>,
    pub active_fence: bool,
    pub witnesses_unverified: bool,
}
struct Stored {
    intent: Intent,
    revision: i64,
}
fn row_values(conn: &Connection, sql: &str, key: &str) -> Result<Option<Vec<Value>>> {
    let mut stmt = conn.prepare(sql).map_err(dberr)?;
    let columns = stmt.column_count();
    stmt.query_row([key], |r| (0..columns).map(|n| r.get(n)).collect())
        .optional()
        .map_err(dberr)
}
fn stored(conn: &Connection, operation: &str) -> Result<Option<Stored>> {
    let lengths: Option<(i64,i64)> = conn.query_row("SELECT octet_length(intent_json),length(canonical_request) FROM asset_fence_plan_receipts WHERE operation_id=?1",[operation],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(dberr)?;
    let Some((json_len, canonical_len)) = lengths else {
        return Ok(None);
    };
    if !(1..=MAX_REQUEST as i64).contains(&json_len)
        || !(1..=MAX_REQUEST as i64).contains(&canonical_len)
    {
        return Err(refuse("stored intent exceeds bounded codec"));
    }
    let (json,revision):(String,i64)=conn.query_row("SELECT intent_json,committed_revision FROM asset_fence_plan_receipts WHERE operation_id=?1",[operation],|r|Ok((r.get(0)?,r.get(1)?))).map_err(dberr)?;
    let i: Intent = serde_json::from_str(&json).map_err(|_| refuse("invalid stored intent"))?;
    if i.operation_id()? != operation || i.expected_revision.checked_add(1) != Some(revision) {
        return Err(refuse("stored operation identity is corrupt"));
    }
    let plan = i.plan_id()?;
    if row_values(
        conn,
        &format!("SELECT {RECEIPT_COLUMNS} FROM asset_fence_plan_receipts WHERE operation_id=?1"),
        operation,
    )?
    .as_ref()
        != Some(&i.receipt_values(revision)?)
        || row_values(
            conn,
            &format!("SELECT {PLAN_COLUMNS} FROM asset_fence_plans WHERE plan_id=?1"),
            &plan,
        )?
        .as_ref()
            != Some(&i.plan_values(revision)?)
    {
        return Err(refuse("stored plan/receipt binding is corrupt"));
    }
    let mut stmt=conn.prepare(&format!("SELECT {BINDING_COLUMNS} FROM asset_fence_plan_bindings WHERE plan_id=?1 ORDER BY ordinal")).map_err(dberr)?;
    let cols = stmt.column_count();
    let rows = stmt
        .query_map([&plan], |r| {
            (0..cols)
                .map(|n| r.get::<_, Value>(n))
                .collect::<std::result::Result<Vec<_>, _>>()
        })
        .map_err(dberr)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(dberr)?;
    if rows != i.binding_values(revision)? {
        return Err(refuse("stored proposed bindings are corrupt or incomplete"));
    }
    Ok(Some(Stored {
        intent: i,
        revision,
    }))
}
#[cfg(unix)]
fn replay(
    value: Stored,
    ctx: &AdminContext,
    target: &FencePlanTarget,
    expected: i64,
) -> Result<FencePlanReceipt> {
    let i = &value.intent;
    if i.target != *target
        || i.expected_revision != expected
        || i.db_path != ctx.path().to_str().unwrap_or("")
        || i.db_dev != ctx.dev()
        || i.db_ino != ctx.ino()
        || i.actor_uid != ctx.uid()
    {
        return Err(refuse("operation replay conflicts with immutable intent"));
    }
    receipt(i, value.revision)
}
fn insert(conn: &Connection, table: &str, columns: &str, values: &[Value]) -> Result<()> {
    let args = (1..=values.len())
        .map(|n| format!("?{n}"))
        .collect::<Vec<_>>()
        .join(",");
    conn.execute(
        &format!("INSERT INTO {table}({columns}) VALUES({args})"),
        rusqlite::params_from_iter(values),
    )
    .map_err(dberr)?;
    Ok(())
}
#[cfg(unix)]
fn intent(
    ctx: &AdminContext,
    target: &FencePlanTarget,
    expected: i64,
    nonce: Uuid,
    snapshot: Snapshot,
) -> Result<Intent> {
    let i = Intent {
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
        target: target.clone(),
        snapshot,
    };
    i.canonical()?;
    encode(&i)?;
    Ok(i)
}

#[cfg(unix)]
impl SqliteStore {
    pub(crate) fn fence_plan_preview(
        &self,
        ctx: &AdminContext,
        target: &FencePlanTarget,
        expected: i64,
        nonce: Uuid,
    ) -> Result<FencePlanPreview> {
        ctx.validate()?;
        target.validate()?;
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
        let i = intent(ctx, target, expected, nonce, snapshot(&read, target)?)?;
        i.receipt_values(
            expected
                .checked_add(1)
                .ok_or_else(|| refuse("revision overflow"))?,
        )?;
        let out = FencePlanPreview {
            operation_id: i.operation_id()?,
            digest: i.digest()?,
            plan_id: i.plan_id()?,
            target: i.target.clone(),
            source_id: i.snapshot.source_id,
            expected_revision: expected,
            snapshot_digest: i.snapshot.digest()?,
            cohort_count: i.snapshot.cohort_count,
            cohort_bytes: i.snapshot.cohort_bytes,
            binding_count: target.bindings.len(),
            canonical_intent: i.canonical()?,
            state: "passive",
            active_fence: false,
            witnesses_unverified: true,
        };
        read.rollback().map_err(dberr)?;
        ctx.validate()?;
        Ok(out)
    }
    pub(crate) fn fence_plan_prepare(
        &self,
        ctx: &AdminContext,
        target: &FencePlanTarget,
        expected: i64,
        operation: &str,
        confirmation: &str,
    ) -> Result<FencePlanReceipt> {
        ctx.validate()?;
        target.validate()?;
        let (nonce, digest) = parse_id(operation)?;
        if confirmation != digest {
            return Err(refuse("exact passive intent confirmation required"));
        }
        let conn = self.connection()?;
        ownership::validate_connection(&conn, ctx)?;
        if let Some(v) = stored(&conn, operation)? {
            return replay(v, ctx, target, expected);
        }
        #[cfg(feature = "spawn-gate")]
        test_pause("before_begin");
        let tx = begin_write(&conn, &self.revision_state)?;
        #[cfg(feature = "spawn-gate")]
        test_pause("after_begin");
        if let Some(v) = stored(&tx, operation)? {
            return replay(v, ctx, target, expected);
        }
        // Nonce conflict comes before current-state freshness: changed intent is not a stale retry.
        if tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM asset_fence_plan_receipts WHERE nonce=?1)",
                [nonce.to_string()],
                |r| r.get::<_, bool>(0),
            )
            .map_err(dberr)?
        {
            return Err(refuse("committed nonce has different intent"));
        }
        let revision = self.revision_state.active_revision.load(Ordering::SeqCst);
        if revision.checked_sub(1) != Some(expected) {
            return Err(refuse("stale expected recording revision"));
        }
        let i = intent(ctx, target, expected, nonce, snapshot(&tx, target)?)?;
        if i.operation_id()? != operation {
            return Err(refuse(
                "operation identity conflicts with immutable parameters",
            ));
        }
        let plan = i.plan_values(revision)?;
        let bindings = i.binding_values(revision)?;
        let receipt_values = i.receipt_values(revision)?;
        {
            let mut p = self
                .fence_plan_state
                .permit
                .lock()
                .map_err(|_| refuse("administrative scope poisoned"))?;
            if p.is_some() {
                return Err(refuse("nested passive plan permit"));
            }
            *p = Some(Permit {
                plan: plan.clone(),
                bindings: bindings.clone(),
                receipt: receipt_values.clone(),
            });
        }
        let scope = PermitScope(&self.fence_plan_state);
        insert(&tx, "asset_fence_plans", PLAN_COLUMNS, &plan)?;
        for values in &bindings {
            insert(&tx, "asset_fence_plan_bindings", BINDING_COLUMNS, values)?;
        }
        insert(
            &tx,
            "asset_fence_plan_receipts",
            RECEIPT_COLUMNS,
            &receipt_values,
        )?;
        #[cfg(feature = "spawn-gate")]
        test_pause("after_write");
        #[cfg(feature = "spawn-gate")]
        test_pause("before_commit");
        ctx.validate()?;
        #[cfg(feature = "spawn-gate")]
        {
            match std::env::var("MINDSPACE_FENCE_PLAN_FAULT").as_deref() {
                Ok("veto") => gate::FAIL_COMMIT.store(true, Ordering::SeqCst),
                Ok("lost_ack") => gate::LOSE_ACK.store(true, Ordering::SeqCst),
                _ => {}
            }
            if std::env::var("MINDSPACE_FENCE_PLAN_PAUSE").as_deref() == Ok("in_commit") {
                if let Some(prefix) = std::env::var_os("MINDSPACE_FENCE_PLAN_GATE_PATH") {
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
    pub(crate) fn fence_plan_inspect(
        &self,
        ctx: &AdminContext,
        operation: &str,
    ) -> Result<FencePlanInspection> {
        ctx.validate()?;
        let conn = self.connection()?;
        ownership::validate_connection(&conn, ctx)?;
        inspect_on_connection(&conn, ctx, operation)
    }
}
#[cfg(unix)]
pub(super) fn inspect_on_connection(
    conn: &Connection,
    ctx: &AdminContext,
    operation: &str,
) -> Result<FencePlanInspection> {
    ctx.validate()?;
    parse_id(operation)?;
    let read = Transaction::new_unchecked(conn, TransactionBehavior::Deferred).map_err(dberr)?;
    let value = stored(&read, operation)?.ok_or_else(|| {
        refuse("passive operation not observed; absence proves no rollback or process exit")
    })?;
    let historical = replay(
        Stored {
            intent: value.intent.clone(),
            revision: value.revision,
        },
        ctx,
        &value.intent.target,
        value.intent.expected_revision,
    )?;
    let current: i64 = read
        .query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| {
            r.get(0)
        })
        .map_err(dberr)?;
    let (freshness,detail)=match snapshot(&read,&value.intent.target){
        Ok(s) if s==value.intent.snapshot && current==value.revision =>("Fresh","Stored database snapshot is unchanged since plan commitment; witnesses remain unverified.".to_owned()),
        Ok(_)=>("Stale","Database revision or bound source/cohort state changed; this advisory plan confers no authority.".to_owned()),
        Err(error)=>("Unavailable",format!("Current bound inputs could not be verified: {error}")),
    };
    read.rollback().map_err(dberr)?;
    ctx.validate()?;
    Ok(FencePlanInspection {
        receipt: historical,
        freshness,
        freshness_detail: detail,
        current_revision: Some(current),
        active_fence: false,
        witnesses_unverified: true,
    })
}
#[cfg(feature = "spawn-gate")]
fn test_pause(phase: &str) {
    if std::env::var("MINDSPACE_FENCE_PLAN_PAUSE").as_deref() != Ok(phase) {
        return;
    }
    let Some(prefix) = std::env::var_os("MINDSPACE_FENCE_PLAN_GATE_PATH") else {
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
mod tests {
    use super::*;

    fn fixture() -> (SqliteStore, FencePlanTarget) {
        let store = SqliteStore::open_in_memory().unwrap();
        let space = store.create_space("passive", "").unwrap();
        let text = "# Passive\n\nOrdinary legacy knowledge stays writable.\n";
        let observed = crate::ingest::observe_bytes(
            Path::new("/passive.md"),
            text.as_bytes().to_vec(),
            "/passive.md".to_owned(),
            &[space.id],
        )
        .unwrap();
        store
            .record_observation(IngestObservation {
                session: crate::core::SessionDraft {
                    root: "/passive.md".to_owned(),
                    scope: IngestScope::File,
                    observed_at: Ts::now(),
                },
                files: vec![observed],
                spaces: vec![space.id],
            })
            .unwrap();
        let (f, fs):(String,String) = store.connection().unwrap().query_row("SELECT l.id,s.id FROM logical_sources l JOIN sources s ON s.logical_id=l.id WHERE l.kind='file'", [], |r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        let asset_id = ownership::enroll_test_source(&store, Uuid::parse_str(&f).unwrap());
        let draft = crate::ingest::document::extract_document_bytes(
            "/passive.md",
            text.as_bytes(),
            Some("md"),
            &Default::default(),
            &Default::default(),
            Ts::now(),
        )
        .unwrap()
        .draft;
        let document = store.record_document_extraction(draft).unwrap();
        let selected = PreparationTarget {
            asset_id,
            legacy_source_version: Uuid::parse_str(&fs).unwrap(),
            document_id: document.logical_id,
            document_source_version: document.source_id,
            extraction_id: document.extraction_id,
        };
        document_preparation::enroll_test_preparation(&store, &selected);
        (
            store,
            FencePlanTarget {
                selected,
                bindings: vec![FenceBinding {
                    namespace: "opaque@1".to_owned(),
                    key: "explicit advisory alias".to_owned(),
                    witness_digest: "a".repeat(64),
                }],
            },
        )
    }
    fn test_intent(conn: &Connection, target: &FencePlanTarget, expected: i64) -> Intent {
        Intent {
            nonce: Uuid::now_v7(),
            db_path: ":memory:".to_owned(),
            db_dev: 1,
            db_ino: 2,
            actor_uid: 1000,
            expected_revision: expected,
            target: target.clone(),
            snapshot: snapshot(conn, target).unwrap(),
        }
    }
    fn arm<'a>(store: &'a SqliteStore, i: &Intent, revision: i64) -> PermitScope<'a> {
        *store.fence_plan_state.permit.lock().unwrap() = Some(Permit {
            plan: i.plan_values(revision).unwrap(),
            bindings: i.binding_values(revision).unwrap(),
            receipt: i.receipt_values(revision).unwrap(),
        });
        PermitScope(&store.fence_plan_state)
    }
    fn persist(store: &SqliteStore, i: &Intent) -> i64 {
        let c = store.connection().unwrap();
        let tx = begin_write(&c, &store.revision_state).unwrap();
        let revision = store.revision_state.active_revision.load(Ordering::SeqCst);
        let _permit = arm(store, i, revision);
        insert(
            &tx,
            "asset_fence_plans",
            PLAN_COLUMNS,
            &i.plan_values(revision).unwrap(),
        )
        .unwrap();
        for values in i.binding_values(revision).unwrap() {
            insert(&tx, "asset_fence_plan_bindings", BINDING_COLUMNS, &values).unwrap();
        }
        insert(
            &tx,
            "asset_fence_plan_receipts",
            RECEIPT_COLUMNS,
            &i.receipt_values(revision).unwrap(),
        )
        .unwrap();
        tx.commit().unwrap();
        revision
    }
    fn current(conn: &Connection) -> i64 {
        conn.query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| {
            r.get(0)
        })
        .unwrap()
    }

    #[test]
    fn migration_authorization_functions_attach_only_to_new_passive_tables() {
        let (store, _) = fixture();
        let c = store.connection().unwrap();
        let mut stmt = c.prepare("SELECT name,tbl_name,sql FROM sqlite_schema WHERE type='trigger' AND name LIKE 'm6_s3e2_%'").unwrap();
        let guards = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(guards.len(), 9);
        for (_, table, _) in &guards {
            assert!(matches!(
                table.as_str(),
                "asset_fence_plans" | "asset_fence_plan_bindings" | "asset_fence_plan_receipts"
            ));
        }
        let mut stmt = c.prepare("SELECT tbl_name FROM sqlite_schema WHERE type='trigger' AND sql LIKE '%s3e2_%valid%'").unwrap();
        for table in stmt.query_map([], |r| r.get::<_, String>(0)).unwrap() {
            assert!(matches!(
                table.unwrap().as_str(),
                "asset_fence_plans" | "asset_fence_plan_bindings" | "asset_fence_plan_receipts"
            ));
        }
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM asset_fence_plans", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn binding_validation_is_bounded_syntactic_and_does_not_admit_aliases() {
        let (_, mut t) = fixture();
        for key in [
            "relative", "/a/../b", "/a/./b", "/a//b", "/a/", "/a/.", "/a/..",
        ] {
            t.bindings = vec![FenceBinding {
                namespace: "file-absolute@1".to_owned(),
                key: key.to_owned(),
                witness_digest: "a".repeat(64),
            }];
            assert!(t.validate().is_err(), "{key}");
        }
        t.bindings[0].key = "/a/b".to_owned();
        assert!(t.validate().is_ok());
        t.bindings.push(t.bindings[0].clone());
        assert!(t.validate().is_err());
        t.bindings.clear();
        assert!(t.validate().is_ok());
        assert!(serde_json::from_str::<FenceBinding>(
            r#"{"namespace":"opaque@1","key":"x","witness_digest":"x","active":true}"#
        )
        .is_err());
    }

    #[test]
    fn plan_receipt_and_bindings_commit_as_one_exact_immutable_effect() {
        let (store, target) = fixture();
        let i = {
            let c = store.connection().unwrap();
            test_intent(&c, &target, current(&c))
        };
        let revision = persist(&store, &i);
        let c = store.connection().unwrap();
        let value = stored(&c, &i.operation_id().unwrap()).unwrap().unwrap();
        assert_eq!(value.revision, revision);
        assert_eq!(value.intent, i);
        assert_eq!(
            c.query_row("SELECT state FROM asset_fence_plans", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "passive"
        );
        assert_eq!(
            c.query_row(
                "SELECT witness_status FROM asset_fence_plan_bindings",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            WITNESS_STATUS
        );
        assert_eq!(c.query_row("SELECT COUNT(*) FROM m6_row_availability WHERE table_name IN ('asset_fence_plans','asset_fence_plan_bindings','asset_fence_plan_receipts')", [], |r|r.get::<_,i64>(0)).unwrap(),0);
        assert!(!receipt(&i, revision).unwrap().active_fence);
    }

    #[test]
    fn absent_administrative_permit_and_ordinary_write_revision_cannot_create_plan() {
        let (store, target) = fixture();
        let c = store.connection().unwrap();
        let before = current(&c);
        let i = test_intent(&c, &target, before);
        let tx = begin_write(&c, &store.revision_state).unwrap();
        let revision = store.revision_state.active_revision.load(Ordering::SeqCst);
        assert!(insert(
            &tx,
            "asset_fence_plans",
            PLAN_COLUMNS,
            &i.plan_values(revision).unwrap()
        )
        .is_err());
        drop(tx);
        assert_eq!(current(&c), before);
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM asset_fence_plans", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn sql_rejects_wrong_source_binding_even_with_an_exact_test_permit() {
        for column in [1, 2, 3, 4, 5, 6, 7, 8] {
            let (store, target) = fixture();
            let c = store.connection().unwrap();
            let i = test_intent(&c, &target, current(&c));
            let tx = begin_write(&c, &store.revision_state).unwrap();
            let revision = store.revision_state.active_revision.load(Ordering::SeqCst);
            let mut values = i.plan_values(revision).unwrap();
            values[column] = if column == 5 {
                1_i64.into()
            } else {
                Uuid::now_v7().to_string().into()
            };
            *store.fence_plan_state.permit.lock().unwrap() = Some(Permit {
                plan: values.clone(),
                bindings: Vec::new(),
                receipt: i.receipt_values(revision).unwrap(),
            });
            let _scope = PermitScope(&store.fence_plan_state);
            assert!(
                insert(&tx, "asset_fence_plans", PLAN_COLUMNS, &values).is_err(),
                "column {column}"
            );
        }
    }

    #[test]
    fn sqlite_rejects_active_publication_states_even_with_exact_forged_permits() {
        for (table, column, invalid) in [
            ("plan", 15, Value::Text("active".to_owned())),
            ("plan", 16, Value::Text("publication@1".to_owned())),
            ("receipt", 14, Value::Text("publish_document@1".to_owned())),
            ("receipt", 15, Value::Text("DocumentOwned".to_owned())),
            ("receipt", 16, Value::Text("DocumentPublished".to_owned())),
            ("receipt", 5, Value::Integer(2)),
        ] {
            let (store, target) = fixture();
            let c = store.connection().unwrap();
            let i = test_intent(&c, &target, current(&c));
            let tx = begin_write(&c, &store.revision_state).unwrap();
            let revision = store.revision_state.active_revision.load(Ordering::SeqCst);
            let mut plan = i.plan_values(revision).unwrap();
            let mut receipt_values = i.receipt_values(revision).unwrap();
            if table == "plan" {
                plan[column] = invalid;
            } else {
                receipt_values[column] = invalid;
            }
            let bindings = i.binding_values(revision).unwrap();
            *store.fence_plan_state.permit.lock().unwrap() = Some(Permit {
                plan: plan.clone(),
                bindings: bindings.clone(),
                receipt: receipt_values.clone(),
            });
            let _scope = PermitScope(&store.fence_plan_state);
            if table == "plan" {
                assert!(insert(&tx, "asset_fence_plans", PLAN_COLUMNS, &plan).is_err());
            } else {
                insert(&tx, "asset_fence_plans", PLAN_COLUMNS, &plan).unwrap();
                for values in &bindings {
                    insert(&tx, "asset_fence_plan_bindings", BINDING_COLUMNS, values).unwrap();
                }
                assert!(insert(
                    &tx,
                    "asset_fence_plan_receipts",
                    RECEIPT_COLUMNS,
                    &receipt_values
                )
                .is_err());
            }
        }
    }

    #[test]
    fn immutable_rows_refuse_replace_upsert_update_delete_even_with_original_permit() {
        let (store, target) = fixture();
        let i = {
            let c = store.connection().unwrap();
            test_intent(&c, &target, current(&c))
        };
        let c = store.connection().unwrap();
        c.execute_batch("PRAGMA recursive_triggers=OFF; PRAGMA foreign_keys=OFF")
            .unwrap();
        let tx = begin_write(&c, &store.revision_state).unwrap();
        let revision = store.revision_state.active_revision.load(Ordering::SeqCst);
        // Repeat within the live exact permit AND matching revision, so neither
        // missing authorization nor an inactive recording context masks REPLACE.
        let _permit = arm(&store, &i, revision);
        insert(
            &tx,
            "asset_fence_plans",
            PLAN_COLUMNS,
            &i.plan_values(revision).unwrap(),
        )
        .unwrap();
        for values in i.binding_values(revision).unwrap() {
            insert(&tx, "asset_fence_plan_bindings", BINDING_COLUMNS, &values).unwrap();
        }
        insert(
            &tx,
            "asset_fence_plan_receipts",
            RECEIPT_COLUMNS,
            &i.receipt_values(revision).unwrap(),
        )
        .unwrap();
        for (table, columns, values) in [
            (
                "asset_fence_plans",
                PLAN_COLUMNS,
                i.plan_values(revision).unwrap(),
            ),
            (
                "asset_fence_plan_bindings",
                BINDING_COLUMNS,
                i.binding_values(revision).unwrap().remove(0),
            ),
            (
                "asset_fence_plan_receipts",
                RECEIPT_COLUMNS,
                i.receipt_values(revision).unwrap(),
            ),
        ] {
            let args = (1..=values.len())
                .map(|n| format!("?{n}"))
                .collect::<Vec<_>>()
                .join(",");
            for prefix in ["INSERT OR REPLACE", "REPLACE"] {
                assert!(
                    c.execute(
                        &format!("{prefix} INTO {table}({columns}) VALUES({args})"),
                        rusqlite::params_from_iter(&values)
                    )
                    .is_err(),
                    "{table} {prefix}"
                );
            }
            let first = columns.split(',').next().unwrap();
            assert!(c.execute(&format!("INSERT INTO {table}({columns}) VALUES({args}) ON CONFLICT DO UPDATE SET {first}=excluded.{first}"),rusqlite::params_from_iter(&values)).is_err());
            assert!(c
                .execute(&format!("UPDATE {table} SET {first}={first}"), [])
                .is_err());
            assert!(c.execute(&format!("DELETE FROM {table}"), []).is_err());
        }
        assert_eq!(
            stored(&c, &i.operation_id().unwrap())
                .unwrap()
                .unwrap()
                .intent,
            i
        );
    }

    #[test]
    fn missing_receipt_or_binding_cannot_commit_a_complete_looking_plan() {
        for omit in ["receipt", "binding"] {
            let (store, target) = fixture();
            let c = store.connection().unwrap();
            let before = current(&c);
            let i = test_intent(&c, &target, before);
            let tx = begin_write(&c, &store.revision_state).unwrap();
            let revision = store.revision_state.active_revision.load(Ordering::SeqCst);
            let _permit = arm(&store, &i, revision);
            insert(
                &tx,
                "asset_fence_plans",
                PLAN_COLUMNS,
                &i.plan_values(revision).unwrap(),
            )
            .unwrap();
            if omit == "receipt" {
                assert!(tx.commit().is_err());
            } else {
                assert!(insert(
                    &tx,
                    "asset_fence_plan_receipts",
                    RECEIPT_COLUMNS,
                    &i.receipt_values(revision).unwrap()
                )
                .is_err());
                drop(tx);
            }
            assert_eq!(current(&c), before);
            assert_eq!(
                c.query_row("SELECT COUNT(*) FROM asset_fence_plans", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }

    #[test]
    fn ordinary_writes_stale_a_passive_snapshot_without_consulting_plans() {
        let (store, target) = fixture();
        let before = snapshot(&store.connection().unwrap(), &target).unwrap();
        let i = {
            let c = store.connection().unwrap();
            test_intent(&c, &target, current(&c))
        };
        persist(&store, &i);
        // A same-source retire changes the cohort despite its immutable selected source pins.
        let memory=store.connection().unwrap().query_row("SELECT m.id FROM memories m JOIN assertions a ON a.id=m.assertion_id JOIN evidence e ON e.id=a.evidence_id JOIN sources s ON s.id=e.source_id WHERE s.logical_id=?1 AND m.state='active' LIMIT 1",[before.source_id.to_string()],|r|r.get::<_,String>(0)).unwrap();
        store
            .retire_memory(
                Uuid::parse_str(&memory).unwrap(),
                Some("passive plan has no fence".to_owned()),
                Ts::now(),
            )
            .unwrap();
        let c = store.connection().unwrap();
        let after = snapshot(&c, &target).unwrap();
        assert_ne!(after.cohort_digest, before.cohort_digest);
        assert_eq!(
            stored(&c, &i.operation_id().unwrap())
                .unwrap()
                .unwrap()
                .intent,
            i
        );
    }
}
