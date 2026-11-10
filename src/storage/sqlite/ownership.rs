//! S3b control metadata only. The sole authority constructor lives in the direct CLI.
use super::*;
use rusqlite::types::Value;
use serde::Serialize;

#[cfg(unix)]
use crate::cli::ownership::AdminContext;

const SQL: &str = include_str!("../../../migrations/0013_legacy_ownership.sql");
const DOMAIN: &str = "mindspace.s3b.local-enroll@1";
const ACTOR: &str = "local-os-account@1";
const MAX_REQUEST: usize = 65536;

#[derive(Default)]
pub(super) struct OwnershipState {
    permit: Mutex<Option<Permit>>,
}

struct Permit {
    receipt: Vec<Value>,
    owner: Vec<Value>,
}

struct PermitScope<'a>(&'a OwnershipState);
impl Drop for PermitScope<'_> {
    fn drop(&mut self) {
        if let Ok(mut permit) = self.0.permit.lock() {
            *permit = None;
        }
    }
}

pub(super) fn register_context(conn: &Connection, state: &Arc<OwnershipState>) -> Result<()> {
    for (name, owner) in [("s3b_receipt_valid", false), ("s3b_owner_valid", true)] {
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
                let expected = if owner {
                    &permit.owner
                } else {
                    &permit.receipt
                };
                if ctx.len() != expected.len() {
                    return Ok(false);
                }
                for (i, value) in expected.iter().enumerate() {
                    if ctx.get::<Value>(i)? != *value {
                        return Ok(false);
                    }
                }
                Ok(true)
            },
        )
        .map_err(dberr)?;
    }
    let availability_state = Arc::clone(state);
    conn.create_scalar_function(
        "s3b_availability_valid",
        3,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS,
        move |ctx| {
            let Ok(guard) = availability_state.permit.lock() else {
                return Ok(false);
            };
            let Some(permit) = guard.as_ref() else {
                return Ok(false);
            };
            let table: String = ctx.get(0)?;
            let key: Value = ctx.get(1)?;
            let revision: Value = ctx.get(2)?;
            let expected = match table.as_str() {
                "asset_ownership" | "asset_ownership_history" => &permit.owner[0],
                "asset_control_intents" => &permit.owner[2],
                _ => return Ok(false),
            };
            Ok(key == *expected && revision == permit.owner[6])
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
    Error::Storage(format!("legacy enrollment refused: {message}"))
}
fn string(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

#[derive(Clone)]
struct Intent {
    nonce: Uuid,
    db_path: String,
    db_dev: u64,
    db_ino: u64,
    actor_uid: u32,
    source: Uuid,
    expected: i64,
    base: String,
}
impl Intent {
    fn canonical(&self) -> Result<Vec<u8>> {
        if self.expected <= 0 {
            return Err(refuse("expected revision must be positive"));
        }
        let mut out = Vec::new();
        string(&mut out, DOMAIN);
        out.extend_from_slice(self.nonce.as_bytes());
        string(&mut out, &self.db_path);
        out.extend_from_slice(&self.db_dev.to_be_bytes());
        out.extend_from_slice(&self.db_ino.to_be_bytes());
        string(&mut out, ACTOR);
        // Real and effective UID are identical by the validated administrative context.
        out.extend_from_slice(&(self.actor_uid as u64).to_be_bytes());
        out.extend_from_slice(&(self.actor_uid as u64).to_be_bytes());
        out.extend_from_slice(self.source.as_bytes());
        string(&mut out, "file");
        string(&mut out, &self.base);
        out.extend_from_slice(&(self.expected as u64).to_be_bytes());
        string(&mut out, "enroll_legacy@1");
        string(&mut out, "Unmanaged");
        out.extend_from_slice(&0_u64.to_be_bytes());
        string(&mut out, "LegacyEnrolled");
        string(&mut out, "legacy");
        out.extend_from_slice(&1_u64.to_be_bytes());
        if out.len() > MAX_REQUEST {
            return Err(refuse("canonical request exceeds 64 KiB"));
        }
        Ok(out)
    }
    fn digest(&self) -> Result<String> {
        Ok(sha256_hex(&self.canonical()?))
    }
    fn id(&self) -> Result<String> {
        Ok(format!("local-enroll@1:{}:{}", self.nonce, self.digest()?))
    }
}

fn parse_id(id: &str) -> Result<(Uuid, &str)> {
    let parts: Vec<_> = id.split(':').collect();
    if parts.len() != 3 || parts[0] != "local-enroll@1" {
        return Err(refuse("invalid full operation identity"));
    }
    let nonce = Uuid::parse_str(parts[1]).map_err(|_| refuse("invalid operation nonce"))?;
    if nonce.to_string() != parts[1]
        || parts[2].len() != 64
        || !parts[2]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(refuse("operation identity is not canonical"));
    }
    Ok((nonce, parts[2]))
}

struct SourceBase {
    fingerprint: String,
    location: Option<String>,
}
fn source_base(conn: &Connection, source: Uuid) -> Result<SourceBase> {
    let row: Option<(String, String, Option<String>, i64, bool)> = conn
        .query_row(
            "SELECT kind,state,current_location,created_at,repository_id IS NULL AND commit_oid IS NULL AND git_path IS NULL FROM logical_sources WHERE id=?1",
            [source.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(dberr)?;
    let Some((kind, state, location, created, git_null)) = row else {
        return Err(refuse("source identity does not exist"));
    };
    if kind != "file" || !git_null || !matches!(state.as_str(), "present" | "absent") {
        return Err(refuse("existing legacy file identity required"));
    }
    let mut bytes = source.as_bytes().to_vec();
    string(&mut bytes, &kind);
    string(&mut bytes, &state);
    match &location {
        None => bytes.push(0),
        Some(location) => {
            bytes.push(1);
            string(&mut bytes, location);
        }
    }
    bytes.extend_from_slice(&created.to_be_bytes());
    Ok(SourceBase {
        fingerprint: sha256_hex(&bytes),
        location,
    })
}

#[derive(Serialize)]
pub(crate) struct OwnershipPreview {
    pub operation_id: String,
    pub digest: String,
    pub source_id: Uuid,
    pub expected_revision: i64,
    pub base_fingerprint: String,
    pub db_path: String,
    pub db_dev: u64,
    pub db_ino: u64,
    pub actor_uid: u32,
}
#[derive(Serialize)]
pub(crate) struct EnrollmentReceipt {
    pub operation_id: String,
    pub digest: String,
    pub source_id: Uuid,
    pub asset_id: Uuid,
    pub committed_revision: i64,
    pub actor_kind: String,
    pub actor_uid: u32,
    pub db_path: String,
    pub db_dev: u64,
    pub db_ino: u64,
}

#[cfg(unix)]
fn intent(
    ctx: &AdminContext,
    source: Uuid,
    expected: i64,
    nonce: Uuid,
    base: String,
) -> Result<Intent> {
    let db_path = ctx
        .path()
        .to_str()
        .ok_or_else(|| refuse("database path must be UTF-8"))?;
    Ok(Intent {
        nonce,
        db_path: db_path.to_owned(),
        db_dev: ctx.dev(),
        db_ino: ctx.ino(),
        actor_uid: ctx.uid(),
        source,
        expected,
        base,
    })
}

struct Stored {
    intent: Intent,
    canonical: Vec<u8>,
    digest: String,
    asset: Uuid,
    revision: i64,
}
fn stored(conn: &Connection, operation: &str) -> Result<Option<Stored>> {
    conn.query_row(
        "SELECT nonce,db_path,db_dev,db_ino,actor_uid,source_id,expected_revision,base_fingerprint,canonical_request,digest,asset_id,committed_revision FROM asset_control_intents WHERE operation_id=?1",
        [operation],
        |row| {
            let parse = |index: usize| -> rusqlite::Result<Uuid> {
                let raw: String = row.get(index)?;
                Uuid::parse_str(&raw).map_err(|e| rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(e)))
            };
            let number = |index: usize| -> rusqlite::Result<u64> {
                let raw: String = row.get(index)?;
                raw.parse().map_err(|e| rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(e)))
            };
            Ok(Stored { intent: Intent { nonce: parse(0)?, db_path: row.get(1)?, db_dev: number(2)?, db_ino: number(3)?, actor_uid: row.get(4)?, source: parse(5)?, expected: row.get(6)?, base: row.get(7)? }, canonical: row.get(8)?, digest: row.get(9)?, asset: parse(10)?, revision: row.get(11)? })
        },
    ).optional().map_err(dberr)
}

#[cfg(unix)]
fn replay(
    value: Stored,
    ctx: &AdminContext,
    source: Uuid,
    expected: i64,
    operation: &str,
) -> Result<EnrollmentReceipt> {
    let i = &value.intent;
    if i.source != source
        || i.expected != expected
        || i.actor_uid != ctx.uid()
        || i.db_path != ctx.path().to_str().unwrap_or("")
        || i.db_dev != ctx.dev()
        || i.db_ino != ctx.ino()
        || i.canonical()? != value.canonical
        || i.digest()? != value.digest
        || i.id()? != operation
        || value.revision
            != expected
                .checked_add(1)
                .ok_or_else(|| refuse("revision overflow"))?
    {
        return Err(refuse("operation replay conflicts with immutable intent"));
    }
    Ok(receipt(
        i,
        operation.to_owned(),
        value.digest,
        value.asset,
        value.revision,
    ))
}
fn receipt(
    i: &Intent,
    operation_id: String,
    digest: String,
    asset_id: Uuid,
    committed_revision: i64,
) -> EnrollmentReceipt {
    EnrollmentReceipt {
        operation_id,
        digest,
        source_id: i.source,
        asset_id,
        committed_revision,
        actor_kind: ACTOR.to_owned(),
        actor_uid: i.actor_uid,
        db_path: i.db_path.clone(),
        db_dev: i.db_dev,
        db_ino: i.db_ino,
    }
}

#[cfg(unix)]
fn validate_connection(conn: &Connection, ctx: &AdminContext) -> Result<()> {
    ctx.validate()?;
    if conn.path() != ctx.path().to_str() {
        return Err(refuse(
            "administrative context belongs to a different database connection",
        ));
    }
    Ok(())
}

/// Recognize initialized application state before WAL setup or migrations can write.
/// This is supported-schema validation under Model A, not authentication of DB contents.
fn require_initialized_database(conn: &Connection) -> Result<()> {
    let marker: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='schema_migrations')", [], |r| r.get(0)).map_err(dberr)?;
    if !marker {
        return Err(refuse("target is not an initialized Mindspace database"));
    }
    let required = [
        ("schema_migrations", &["version", "applied_at"][..]),
        ("spaces", &["id", "name", "description", "created_at"][..]),
        ("space_links", &["from_space", "to_space", "created_at"][..]),
        (
            "sources",
            &["id", "kind", "location", "content", "content_hash"][..],
        ),
        ("evidence", &["id", "source_id", "kind", "ordinal"][..]),
        (
            "assertions",
            &["id", "evidence_id", "subject", "content"][..],
        ),
        ("memories", &["id", "assertion_id", "state"][..]),
        ("memory_spaces", &["memory_id", "space_id"][..]),
        ("assertion_fts", &["assertion_id", "subject", "content"][..]),
    ];
    for (table, columns) in required {
        let actual_table: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                [table],
                |r| r.get(0),
            )
            .map_err(dberr)?;
        if !actual_table {
            return Err(refuse("target lacks required Mindspace tables"));
        }
        let names: HashSet<String> = conn
            .prepare("SELECT name FROM pragma_table_info(?1)")
            .map_err(dberr)?
            .query_map([table], |r| r.get(0))
            .map_err(dberr)?
            .collect::<std::result::Result<_, _>>()
            .map_err(dberr)?;
        if columns.iter().any(|name| !names.contains(*name)) {
            return Err(refuse("target has an incompatible Mindspace table shape"));
        }
    }
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
        .map_err(dberr)?;
    if count == 0 {
        return Err(refuse("target has no committed Mindspace migrations"));
    }
    reject_future_schema(conn)?;
    validate_migration_prefix(conn)?;
    Ok(())
}

#[cfg(unix)]
fn sidecar_path(ctx: &AdminContext, suffix: &str) -> std::path::PathBuf {
    let mut path = ctx.path().as_os_str().to_os_string();
    path.push(suffix);
    path.into()
}

#[cfg(unix)]
fn reject_rollback_journal(ctx: &AdminContext) -> Result<()> {
    match std::fs::symlink_metadata(sidecar_path(ctx, "-journal")) {
        Ok(_) => Err(refuse(
            "rollback-journal targets require separate SQLite recovery before administration",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(refuse(&format!("cannot inspect rollback journal: {error}"))),
    }
}

#[cfg(unix)]
fn recognize_read_only(ctx: &AdminContext, with_wal: bool) -> Result<()> {
    ctx.validate()?;
    reject_rollback_journal(ctx)?;
    let flags =
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let conn = if with_wal {
        if !sidecar_path(ctx, "-shm").is_file() {
            return Err(refuse(
                "WAL without an existing shared-memory file requires separate SQLite recovery",
            ));
        }
        Connection::open_with_flags(ctx.path(), flags)
    } else {
        // Like doctor inspection: an immutable read creates no WAL/SHM for a closed
        // WAL-mode database. This internal URI encodes the pinned path, never caller URI options.
        let mut uri = String::from("file:");
        for byte in ctx
            .path()
            .to_str()
            .ok_or_else(|| refuse("database path must be UTF-8"))?
            .bytes()
        {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'.' | b'_' | b'-' | b'~' => {
                    uri.push(byte as char)
                }
                byte => uri.push_str(&format!("%{byte:02X}")),
            }
        }
        uri.push_str("?immutable=1");
        Connection::open_with_flags(uri, flags | rusqlite::OpenFlags::SQLITE_OPEN_URI)
    }
    .map_err(dberr)?;
    let read = Transaction::new_unchecked(&conn, TransactionBehavior::Deferred).map_err(dberr)?;
    require_initialized_database(&read)?;
    read.rollback().map_err(dberr)?;
    ctx.validate()?;
    reject_rollback_journal(ctx)
}

#[cfg(unix)]
fn recognize_before_write(ctx: &AdminContext) -> Result<()> {
    let with_wal = sidecar_path(ctx, "-wal").is_file();
    let first = recognize_read_only(ctx, with_wal);
    // Another legitimate connection may have started writing after the WAL check.
    // Do not accept a stale immutable schema view when its WAL has appeared.
    if !with_wal && sidecar_path(ctx, "-wal").is_file() {
        recognize_read_only(ctx, true)
    } else {
        first
    }
}

#[cfg(unix)]
impl SqliteStore {
    pub(crate) fn open_ownership_admin(ctx: &AdminContext) -> Result<Self> {
        ctx.validate()?;
        recognize_before_write(ctx)?;
        ctx.validate()?;
        reject_rollback_journal(ctx)?;
        let conn = Connection::open_with_flags(
            ctx.path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(dberr)?;
        ctx.validate()?;
        require_initialized_database(&conn)?;
        ctx.validate()?;
        let store = Self::init_conn(conn)?;
        ctx.validate()?;
        Ok(store)
    }
    pub(crate) fn ownership_preview(
        &self,
        ctx: &AdminContext,
        source: Uuid,
        expected: i64,
        nonce: Uuid,
    ) -> Result<OwnershipPreview> {
        ctx.validate()?;
        let conn = self.connection()?;
        validate_connection(&conn, ctx)?;
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
        if read
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM asset_ownership WHERE source_id=?1)",
                [source.to_string()],
                |r| r.get::<_, bool>(0),
            )
            .map_err(dberr)?
        {
            return Err(refuse("source is already enrolled"));
        }
        let base = source_base(&read, source)?;
        let i = intent(ctx, source, expected, nonce, base.fingerprint)?;
        let out = OwnershipPreview {
            operation_id: i.id()?,
            digest: i.digest()?,
            source_id: source,
            expected_revision: expected,
            base_fingerprint: i.base,
            db_path: i.db_path,
            db_dev: i.db_dev,
            db_ino: i.db_ino,
            actor_uid: i.actor_uid,
        };
        read.rollback().map_err(dberr)?;
        Ok(out)
    }
    pub(crate) fn ownership_enroll(
        &self,
        ctx: &AdminContext,
        source: Uuid,
        expected: i64,
        operation: &str,
        confirmation: &str,
    ) -> Result<EnrollmentReceipt> {
        ctx.validate()?;
        let (nonce, digest) = parse_id(operation)?;
        if confirmation != digest {
            return Err(refuse("exact intent confirmation required"));
        }
        let conn = self.connection()?;
        validate_connection(&conn, ctx)?;
        if let Some(value) = stored(&conn, operation)? {
            return replay(value, ctx, source, expected, operation);
        }
        #[cfg(feature = "spawn-gate")]
        test_pause("before_begin");
        let tx = begin_write(&conn, &self.revision_state)?;
        #[cfg(feature = "spawn-gate")]
        test_pause("after_begin");
        if let Some(value) = stored(&tx, operation)? {
            return replay(value, ctx, source, expected, operation);
        }
        let revision = self.revision_state.active_revision.load(Ordering::SeqCst);
        if revision.checked_sub(1) != Some(expected) {
            return Err(refuse("stale expected recording revision"));
        }
        if tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM asset_control_intents WHERE nonce=?1)",
                [nonce.to_string()],
                |r| r.get::<_, bool>(0),
            )
            .map_err(dberr)?
        {
            return Err(refuse("committed nonce has different intent"));
        }
        if tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM asset_ownership WHERE source_id=?1)",
                [source.to_string()],
                |r| r.get::<_, bool>(0),
            )
            .map_err(dberr)?
        {
            return Err(refuse("source is already enrolled"));
        }
        let base = source_base(&tx, source)?;
        let i = intent(ctx, source, expected, nonce, base.fingerprint)?;
        if i.id()? != operation {
            return Err(refuse(
                "operation identity conflicts with immutable parameters",
            ));
        }
        let asset = Uuid::now_v7();
        let canonical = i.canonical()?;
        let permit = Permit {
            receipt: vec![
                operation.to_owned().into(),
                nonce.to_string().into(),
                digest.to_owned().into(),
                canonical.clone().into(),
                i.db_path.clone().into(),
                i.db_dev.to_string().into(),
                i.db_ino.to_string().into(),
                (i.actor_uid as i64).into(),
                source.to_string().into(),
                expected.into(),
                i.base.clone().into(),
                asset.to_string().into(),
                revision.into(),
                1_i64.into(),
                ACTOR.to_owned().into(),
            ],
            owner: vec![
                asset.to_string().into(),
                source.to_string().into(),
                operation.to_owned().into(),
                "legacy".to_owned().into(),
                1_i64.into(),
                base.location.clone().map_or(Value::Null, Value::Text),
                revision.into(),
                revision.into(),
            ],
        };
        {
            let mut active = self
                .ownership_state
                .permit
                .lock()
                .map_err(|_| refuse("administrative scope poisoned"))?;
            if active.is_some() {
                return Err(refuse("nested administrative scope"));
            }
            *active = Some(permit);
        }
        let _scope = PermitScope(&self.ownership_state);
        tx.execute("INSERT INTO asset_ownership(asset_id,source_id,owner,epoch,enrollment_location,operation_id,created_revision,updated_revision) VALUES(?1,?2,'legacy',1,?3,?4,?5,?5)", rusqlite::params![asset.to_string(),source.to_string(),base.location,operation,revision]).map_err(dberr)?;
        tx.execute("INSERT INTO asset_control_intents(operation_id,nonce,digest,canonical_request,request_version,db_path,db_dev,db_ino,actor_kind,actor_uid,source_id,action,expected_state,expected_epoch,expected_revision,base_fingerprint,intended_state,owner,epoch,asset_id,committed_revision) VALUES(?1,?2,?3,?4,1,?5,?6,?7,'local-os-account@1',?8,?9,'enroll_legacy@1','Unmanaged',0,?10,?11,'LegacyEnrolled','legacy',1,?12,?13)", rusqlite::params![operation,nonce.to_string(),digest,canonical,i.db_path,i.db_dev.to_string(),i.db_ino.to_string(),i.actor_uid as i64,source.to_string(),expected,i.base,asset.to_string(),revision]).map_err(dberr)?;
        #[cfg(feature = "spawn-gate")]
        test_pause("after_write");
        #[cfg(feature = "spawn-gate")]
        test_pause("before_commit");
        ctx.validate()?;
        #[cfg(feature = "spawn-gate")]
        {
            match std::env::var("MINDSPACE_OWNERSHIP_FAULT").as_deref() {
                Ok("veto") => gate::FAIL_COMMIT.store(true, Ordering::SeqCst),
                Ok("lost_ack") => gate::LOSE_ACK.store(true, Ordering::SeqCst),
                _ => {}
            }
            if std::env::var("MINDSPACE_OWNERSHIP_PAUSE").as_deref() == Ok("in_commit") {
                if let Some(prefix) = std::env::var_os("MINDSPACE_OWNERSHIP_GATE_PATH") {
                    std::env::set_var("MINDSPACE_STORE_GATE_PAUSE_IN_COMMIT", prefix);
                    gate::PAUSE_IN_COMMIT.store(true, Ordering::SeqCst);
                }
            }
        }
        if let Err(error) = tx.commit() {
            if matches!(error, Error::CommitOutcomeUnknown { .. }) {
                // Unlike a generic store read handle, an uncertain administrative connection
                // is permanently removed before its transaction-private permit is released.
                drop(_scope);
                drop(conn);
                if let Ok(mut connection) = self.conn.lock() {
                    connection.take();
                }
            }
            return Err(error);
        }
        #[cfg(feature = "spawn-gate")]
        test_pause("after_commit");
        Ok(receipt(
            &i,
            operation.to_owned(),
            digest.to_owned(),
            asset,
            revision,
        ))
    }
}

#[cfg(feature = "spawn-gate")]
fn test_pause(phase: &str) {
    if std::env::var("MINDSPACE_OWNERSHIP_PAUSE").as_deref() != Ok(phase) {
        return;
    }
    let Some(prefix) = std::env::var_os("MINDSPACE_OWNERSHIP_GATE_PATH") else {
        return;
    };
    let with = |extension: &str| {
        let mut p = prefix.clone();
        p.push(extension);
        std::path::PathBuf::from(p)
    };
    let _ = std::fs::write(with(".reached"), b"");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !with(".go").exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
#[path = "ownership_tests.rs"]
mod tests;
