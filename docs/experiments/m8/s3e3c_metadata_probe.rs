//! Disposable S3e.3c feasibility adapter, never production routing or migration.
//! Copy into tests/metadata_quarantine_feasibility.rs in an archived source tree.
#![cfg(target_os = "linux")]

#[path = "support/adversarial_common.rs"]
mod common;

use common::{Case, ChildGuard};
use mindspace::core::sha256_hex;
use mindspace::storage::Store;
use mindspace::SqliteStore;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

const ENABLED: &str = "embedding.enabled";
const MODEL: &str = "embedding.model";
const OLD_VALUE: &str = "SYNTHETIC_LEGACY_MODEL_S3E3C";
const NEW_VALUE: &str = "SYNTHETIC_PRIVATE_MODEL_S3E3C";
const NEXT_VALUE: &str = "SYNTHETIC_PRIVATE_NEXT_S3E3C";
const PROTOCOL: &str = "fixture-quarantine@1";
const EVENT: &str = "S3E3C_EVENT ";
type ProbeResult<T> = Result<T, String>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Pair {
    enabled: Option<String>,
    model: Option<String>,
}

fn pair(model: &str) -> Pair {
    Pair {
        enabled: Some("false".into()),
        model: Some(model.into()),
    }
}

fn error(e: rusqlite::Error) -> String {
    e.to_string()
}
fn canonical(p: &Pair) -> String {
    serde_json::to_string(p).unwrap()
}

fn legacy(conn: &Connection) -> Pair {
    let get = |key: &str| {
        conn.query_row("SELECT value FROM mind_meta WHERE key=?1", [key], |r| {
            r.get(0)
        })
        .optional()
        .unwrap()
    };
    Pair {
        enabled: get(ENABLED),
        model: get(MODEL),
    }
}

fn table(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [name],
        |r| r.get(0),
    )
    .unwrap()
}

fn minimal(path: &Path, journal: &str, version: i64) {
    assert!(matches!(journal, "WAL" | "DELETE"));
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(&format!("PRAGMA journal_mode={journal}; PRAGMA synchronous=FULL; CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY); CREATE TABLE mind_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE payload(content TEXT); INSERT INTO payload VALUES('SYNTHETIC_NONSETTING_CONTENT');")).unwrap();
    for n in 1..=version {
        conn.execute("INSERT INTO schema_migrations VALUES(?1)", [n])
            .unwrap();
    }
    conn.execute("INSERT INTO mind_meta VALUES(?1,'false')", [ENABLED])
        .unwrap();
    conn.execute(
        "INSERT INTO mind_meta VALUES(?1,?2)",
        params![MODEL, OLD_VALUE],
    )
    .unwrap();
}

/// Explicit raw migration simulation for disposable full-Mindspace fixtures.
/// It preserves existing M6 guards, but is not production permit construction,
/// metadata history coverage or an accepted administrative API.
fn begin(conn: &Connection) -> Transaction<'_> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate).unwrap();
    if table(&tx, "m6_revisions") {
        let revision: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(revision_id),0)+1 FROM m6_revisions",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.create_scalar_function(
            "m6_write_revision",
            0,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |_| Ok(revision),
        )
        .unwrap();
        tx.execute(
            "INSERT INTO m6_revisions(revision_id,recorded_at,kind) VALUES(?1,1,'write')",
            [revision],
        )
        .unwrap();
    }
    tx
}

fn install(tx: &Connection, p: &Pair, op: &str, guarded: bool) -> ProbeResult<()> {
    tx.execute_batch("CREATE TABLE probe_q_pair(generation INTEGER PRIMARY KEY CHECK(generation>0), enabled TEXT, model TEXT, operation TEXT NOT NULL UNIQUE, digest TEXT NOT NULL);
      CREATE TABLE probe_q_state(singleton INTEGER PRIMARY KEY CHECK(singleton=1), generation INTEGER NOT NULL, protocol TEXT NOT NULL);
      CREATE TABLE probe_q_receipt(operation TEXT PRIMARY KEY,expected INTEGER NOT NULL,generation INTEGER NOT NULL UNIQUE,intent TEXT NOT NULL);").map_err(error)?;
    let intent = canonical(p);
    tx.execute(
        "INSERT INTO probe_q_pair VALUES(1,?1,?2,?3,?4)",
        params![p.enabled, p.model, op, sha256_hex(intent.as_bytes())],
    )
    .map_err(error)?;
    tx.execute("INSERT INTO probe_q_state VALUES(1,1,?1)", [PROTOCOL])
        .map_err(error)?;
    tx.execute(
        "INSERT INTO probe_q_receipt VALUES(?1,0,1,?2)",
        params![op, intent],
    )
    .map_err(error)?;
    tx.execute(
        "DELETE FROM mind_meta WHERE key IN (?1,?2)",
        params![ENABLED, MODEL],
    )
    .map_err(error)?;
    if guarded {
        tx.execute_batch("CREATE TRIGGER probe_q_legacy_insert BEFORE INSERT ON mind_meta WHEN NEW.key IN ('embedding.enabled','embedding.model') BEGIN SELECT RAISE(ABORT,'fixture legacy setting quarantined'); END;
          CREATE TRIGGER probe_q_legacy_update BEFORE UPDATE ON mind_meta WHEN OLD.key IN ('embedding.enabled','embedding.model') OR NEW.key IN ('embedding.enabled','embedding.model') BEGIN SELECT RAISE(ABORT,'fixture legacy setting quarantined'); END;").map_err(error)?;
    }
    Ok(())
}

fn prepare(conn: &Connection, p: &Pair, op: &str, guarded: bool) {
    let tx = begin(conn);
    install(&tx, p, op, guarded).unwrap();
    tx.commit().unwrap();
}

fn authoritative(conn: &Connection) -> ProbeResult<(i64, Pair)> {
    // This is one statement/snapshot. No fallback to legacy metadata exists.
    let mut stmt=conn.prepare("SELECT s.generation,s.protocol,p.enabled,p.model,p.operation,p.digest,r.expected,r.generation,r.intent,(SELECT COUNT(*) FROM mind_meta WHERE key IN ('embedding.enabled','embedding.model'))
      FROM probe_q_state s LEFT JOIN probe_q_pair p ON p.generation=s.generation LEFT JOIN probe_q_receipt r ON r.operation=p.operation WHERE s.singleton=1").map_err(error)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                Pair {
                    enabled: r.get(2)?,
                    model: r.get(3)?,
                },
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, i64>(9)?,
            ))
        })
        .map_err(error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(error)?;
    if rows.len() != 1 {
        return Err("missing or conflicting selected generation".into());
    }
    let (generation, protocol, p, _op, digest, expected, receipt_generation, intent, legacy_count) =
        &rows[0];
    if protocol != PROTOCOL
        || *generation < 1
        || *receipt_generation != *generation
        || *expected != generation - 1
        || canonical(p) != *intent
        || sha256_hex(intent.as_bytes()) != *digest
    {
        return Err("inconsistent marker/pair/receipt/digest".into());
    }
    if *legacy_count != 0 {
        return Err("conflicting legacy authority".into());
    }
    Ok((*generation, p.clone()))
}

fn update(conn: &Connection, expected: i64, p: &Pair, op: &str) -> ProbeResult<i64> {
    let tx = begin(conn);
    let intent = canonical(p);
    let prior: Option<(i64, i64, String)> = tx
        .query_row(
            "SELECT expected,generation,intent FROM probe_q_receipt WHERE operation=?1",
            [op],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(error)?;
    if let Some((prior_expected, generation, prior_intent)) = prior {
        if prior_expected != expected || prior_intent != intent {
            return Err("changed operation intent".into());
        }
        // Roll back any simulated M6 allocation on a read-only exact replay.
        tx.rollback().map_err(error)?;
        return Ok(generation);
    }
    let (generation, _) = authoritative(&tx)?;
    if generation != expected {
        return Err("stale expected generation".into());
    }
    let next = generation + 1;
    tx.execute(
        "INSERT INTO probe_q_pair VALUES(?1,?2,?3,?4,?5)",
        params![next, p.enabled, p.model, op, sha256_hex(intent.as_bytes())],
    )
    .map_err(error)?;
    tx.execute(
        "INSERT INTO probe_q_receipt VALUES(?1,?2,?3,?4)",
        params![op, expected, next, intent],
    )
    .map_err(error)?;
    let changed = tx
        .execute(
            "UPDATE probe_q_state SET generation=?1 WHERE singleton=1 AND generation=?2",
            params![next, expected],
        )
        .map_err(error)?;
    if changed != 1 {
        return Err("generation CAS lost".into());
    }
    tx.commit().map_err(error)?;
    Ok(next)
}

fn doctor(case: &Case, label: &str) -> Value {
    let mut command = case.mind(&common::old());
    command.args(["doctor", "--json"]);
    let out = common::run(command, label, case);
    for marker in [NEW_VALUE, NEXT_VALUE] {
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains(marker),
            "private marker in old diagnostic stdout"
        );
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains(marker),
            "private marker in old diagnostic stderr"
        );
    }
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        report.get("format").and_then(Value::as_str),
        Some("mind-doctor/1")
    );
    assert_eq!(
        out.status.code().map(i64::from),
        report
            .get("summary")
            .and_then(|v| v.get("exit_code"))
            .and_then(Value::as_i64)
    );
    let database = report
        .get("checks")
        .and_then(Value::as_array)
        .expect("doctor checks array")
        .iter()
        .find(|v| v.get("id").and_then(Value::as_str) == Some("database"))
        .expect("database diagnostic");
    assert!(["pass", "warn", "fail", "skip"].contains(
        &database
            .get("status")
            .and_then(Value::as_str)
            .expect("database status")
    ));
    assert!(
        database
            .get("details")
            .and_then(|v| v.get("schema_version"))
            .and_then(Value::as_i64)
            .is_some(),
        "actual schema inspection required"
    );
    assert_eq!(
        database
            .get("details")
            .and_then(|v| v.get("supported_schema_version"))
            .and_then(Value::as_i64),
        Some(16)
    );
    case.save(&format!("{label}-files"), &case.fingerprint());
    report
}

fn selected(report: &Value) -> Value {
    let selected = report
        .get("checks")
        .and_then(Value::as_array)
        .expect("doctor checks array")
        .iter()
        .find(|r| r.get("id").and_then(Value::as_str) == Some("embeddings"))
        .expect("embedding diagnostic")
        .get("details")
        .and_then(|v| v.get("selected_model"))
        .expect("explicit selected_model field required; missing inspection is not suppression");
    assert!(
        selected.is_null() || selected.is_string(),
        "selected_model type"
    );
    selected.clone()
}

fn incompatible(report: &Value) -> bool {
    let c = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "database")
        .unwrap();
    c["status"] == "fail"
        && c["details"]["schema_version"] == 17
        && c["details"]["supported_schema_version"] == 16
}

struct OldActor(ChildGuard);
impl OldActor {
    fn start(case: &Case) -> Self {
        let exe = PathBuf::from(std::env::var_os("S3E3C_OLD_ACTOR_BINARY").unwrap());
        let mut command = case.command(&exe);
        command
            .env("S3E3C_ACTOR_DB", &case.db)
            .env("S3E3C_FIXTURE_ROOT", &case.work)
            .args(["--ignored", "--exact", "old_store_actor", "--nocapture"]);
        let mut actor = Self(ChildGuard::spawn(
            command,
            case.artifacts.join("old-store.stderr"),
        ));
        assert_eq!(actor.receive()["event"], "ready");
        actor
    }
    fn receive(&mut self) -> Value {
        for _ in 0..12 {
            let line = self.0.line();
            if let Some(v) = line.strip_prefix("S3E3C_OLD ") {
                return serde_json::from_str(v).unwrap();
            }
        }
        panic!("missing historical Store actor event")
    }
    fn call(&mut self, v: Value) -> Value {
        self.0.send(&v.to_string());
        self.receive()
    }
    fn close(mut self) {
        self.0.send(r#"{"op":"exit"}"#);
        assert_eq!(self.receive()["event"], "released");
        assert!(self.0.wait().success());
    }
}

fn real17(case: &Case) {
    let mut init = case.mind(&common::old());
    init.arg("init");
    common::success(common::run(init, "init16", case));
    let store = SqliteStore::open(&case.db).unwrap();
    store.set_meta(ENABLED, "false").unwrap();
    store.set_meta(MODEL, OLD_VALUE).unwrap();
}

fn event(v: Value) {
    println!("\n{EVENT}{v}");
    io::stdout().flush().unwrap();
}
fn gate() {
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["command"],
        "release"
    );
}

#[test]
#[ignore = "subprocess protocol endpoint, not a scenario"]
fn candidate_actor() {
    let db = PathBuf::from(std::env::var_os("S3E3C_NATIVE_DB").unwrap());
    let root = PathBuf::from(std::env::var_os("S3E3C_NATIVE_ROOT").unwrap());
    assert!(db.is_absolute() && db.starts_with(root) && db.is_file());
    let mode = std::env::var("S3E3C_NATIVE_MODE").unwrap();
    let conn = Connection::open(&db).unwrap();
    conn.busy_timeout(Duration::from_secs(3)).unwrap();
    match mode.as_str() {
        "prepare_flush" | "prepare_wait" => {
            let tx = begin(&conn);
            install(&tx, &pair(OLD_VALUE), "prepare", true).unwrap();
            if mode == "prepare_flush" {
                conn.cache_flush().unwrap();
            }
            event(json!({"state":"prepared_uncommitted"}));
            gate();
            tx.rollback().unwrap();
        }
        "update_flush" => {
            let tx = begin(&conn);
            let p = pair(NEXT_VALUE);
            let intent = canonical(&p);
            tx.execute(
                "INSERT INTO probe_q_pair VALUES(2,?1,?2,'update-flush',?3)",
                params![p.enabled, p.model, sha256_hex(intent.as_bytes())],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO probe_q_receipt VALUES('update-flush',1,2,?1)",
                [intent],
            )
            .unwrap();
            tx.execute("UPDATE probe_q_state SET generation=2", [])
                .unwrap();
            conn.cache_flush().unwrap();
            event(json!({"state":"updated_uncommitted"}));
            gate();
            tx.rollback().unwrap();
        }
        "commit_before_reply" => {
            prepare(&conn, &pair(NEW_VALUE), "prepare", true);
            event(json!({"state":"committed_before_reply"}));
            gate();
        }
        "held_snapshot" => {
            conn.execute_batch("BEGIN DEFERRED").unwrap();
            assert!(legacy(&conn).model.is_some());
            event(json!({"state":"snapshot_held"}));
            gate();
            conn.execute_batch("ROLLBACK").unwrap();
        }
        "wal_hold" => {
            assert!(legacy(&conn).model.is_some());
            event(json!({"state":"connection_held"}));
            gate();
        }
        other => panic!("unknown actor mode {other}"),
    }
    drop(conn);
    event(json!({"state":"released"}));
}

fn actor(case: &Case, mode: &str) -> ChildGuard {
    let mut command = case.command(&std::env::current_exe().unwrap());
    command
        .env("S3E3C_NATIVE_DB", &case.db)
        .env("S3E3C_NATIVE_ROOT", &case.work)
        .env("S3E3C_NATIVE_MODE", mode)
        .args(["--ignored", "--exact", "candidate_actor", "--nocapture"]);
    ChildGuard::spawn(command, case.artifacts.join(format!("{mode}.stderr")))
}
fn receive(child: &mut ChildGuard, state: &str) {
    for _ in 0..12 {
        let line = child.line();
        if let Some(v) = line.strip_prefix(EVENT) {
            assert_eq!(serde_json::from_str::<Value>(v).unwrap()["state"], state);
            return;
        }
    }
    panic!("missing actor state {state}")
}
fn release(mut child: ChildGuard) {
    child.send(r#"{"command":"release"}"#);
    receive(&mut child, "released");
    assert!(child.wait().success());
}

#[test]
#[ignore = "run S3e.3c disposable feasibility runner"]
fn q01_old_doctor_suppression_does_not_supply_current_routing() {
    let case = Case::new("q01_doctor_and_current_routing");
    real17(&case);
    let baseline = doctor(&case, "baseline-doctor");
    assert!(incompatible(&baseline));
    assert_eq!(selected(&baseline), OLD_VALUE);
    let conn = Connection::open(&case.db).unwrap();
    prepare(&conn, &pair(NEW_VALUE), "prepare", true);
    drop(conn);
    let after = doctor(&case, "quarantined-doctor");
    assert!(incompatible(&after));
    assert!(selected(&after).is_null());
    let store = SqliteStore::open(&case.db).unwrap();
    assert_eq!(store.get_meta(MODEL).unwrap(), None);
    assert_eq!(store.get_meta(ENABLED).unwrap(), None);
    drop(store);
    let conn = Connection::open(&case.db).unwrap();
    assert_eq!(authoritative(&conn).unwrap(), (1, pair(NEW_VALUE)));
    case.finish("known_unsafe_characterization","stable legacy diagnostic lookup omits the new private marker, but real current Store still has no authoritative routing",json!({"baseline_disclosed":OLD_VALUE,"new_marker_disclosed":false,"current_store_model":null,"authoritative_generation":1,"scope":"prototype raw migration context; no production routing or M6 migration proof"}));
}

#[test]
#[ignore = "run S3e.3c disposable feasibility runner"]
fn q02_snapshots_and_prepared_statements_retain_prior_knowledge() {
    let case = Case::new("q02_snapshots_and_prepared");
    minimal(&case.db, "WAL", 17);
    let writer = Connection::open(&case.db).unwrap();
    writer.execute_batch("PRAGMA wal_autocheckpoint=0").unwrap();
    let reader = Connection::open(&case.db).unwrap();
    reader.execute_batch("BEGIN DEFERRED").unwrap();
    assert_eq!(legacy(&reader), pair(OLD_VALUE));
    // Both statements address the old physical mind_meta BEFORE cutover DDL.
    let unstepped_connection = Connection::open(&case.db).unwrap();
    let mut old_prepared = unstepped_connection
        .prepare("SELECT value FROM mind_meta WHERE key='embedding.model'")
        .unwrap();
    let stepped_connection = Connection::open(&case.db).unwrap();
    let mut old_stepped = stepped_connection
        .prepare("SELECT key,value FROM mind_meta ORDER BY key")
        .unwrap();
    let mut old_rows = old_stepped.query([]).unwrap();
    assert_eq!(
        old_rows
            .next()
            .unwrap()
            .unwrap()
            .get::<_, String>(0)
            .unwrap(),
        ENABLED
    );
    prepare(&writer, &pair(NEW_VALUE), "prepare", true);
    let old_prepared_value: Option<String> =
        old_prepared.query_row([], |r| r.get(0)).optional().unwrap();
    assert_eq!(old_prepared_value, None);
    let old_stepped_value: String = old_rows.next().unwrap().unwrap().get(1).unwrap();
    assert_eq!(old_stepped_value, OLD_VALUE);
    assert!(old_rows.next().unwrap().is_none());
    drop(old_rows);
    drop(old_stepped);
    drop(stepped_connection);
    drop(old_prepared);
    drop(unstepped_connection);
    assert_eq!(legacy(&reader), pair(OLD_VALUE));
    assert_eq!(
        legacy(&writer),
        Pair {
            enabled: None,
            model: None
        }
    );
    case.save("held_snapshot", &case.fingerprint());
    reader.execute_batch("ROLLBACK").unwrap();
    assert!(legacy(&reader).model.is_none());
    // Preparation without stepping uses the new committed view on its first read.
    let mut prepared=reader.prepare("SELECT model FROM probe_q_pair WHERE generation=(SELECT generation FROM probe_q_state)").unwrap();
    update(&writer, 1, &pair(NEXT_VALUE), "next").unwrap();
    let prepared_value: String = prepared.query_row([], |r| r.get(0)).unwrap();
    assert_eq!(prepared_value, NEXT_VALUE);
    drop(prepared);
    // A stepped statement holds its prior generation even after a new commit.
    let mut stmt = reader
        .prepare("SELECT generation,model FROM probe_q_pair ORDER BY generation")
        .unwrap();
    let mut rows = stmt.query([]).unwrap();
    assert_eq!(rows.next().unwrap().unwrap().get::<_, i64>(0).unwrap(), 1);
    update(&writer, 2, &pair("SYNTHETIC_GENERATION_THREE"), "third").unwrap();
    assert_eq!(
        rows.next().unwrap().unwrap().get::<_, String>(1).unwrap(),
        NEXT_VALUE
    );
    assert!(rows.next().unwrap().is_none());
    drop(rows);
    drop(stmt);
    assert_eq!(authoritative(&reader).unwrap().0, 3);
    case.finish("known_unsafe_characterization","cutover does not erase a held old snapshot; prepared-only and stepped statements observe distinct generation lifetimes",json!({"old_snapshot_disclosed":OLD_VALUE,"fresh_legacy_model":null,"old_prepared_first_step_after_cutover":old_prepared_value,"old_partly_stepped_after_cutover":old_stepped_value,"prepared_first_read":prepared_value,"stepped_snapshot_max_generation":2,"released_generation":3}));
}

#[test]
#[ignore = "run S3e.3c disposable feasibility runner"]
fn q03_actual_old_store_queued_setter_requires_static_guards() {
    for guarded in [false, true] {
        let case = Case::new(if guarded {
            "q03_guarded"
        } else {
            "q03_unguarded_control"
        });
        let mut old = OldActor::start(&case);
        assert!(
            old.call(json!({"op":"set","key":ENABLED,"value":"false"}))["ok"]
                .as_bool()
                .unwrap()
        );
        assert!(
            old.call(json!({"op":"set","key":MODEL,"value":OLD_VALUE}))["ok"]
                .as_bool()
                .unwrap()
        );
        let mut migrate = case.mind(&common::current());
        migrate.arg("init");
        common::success(common::run(migrate, "actual-migration17", &case));
        let conn = Connection::open(&case.db).unwrap();
        let tx = begin(&conn);
        install(&tx, &pair(NEW_VALUE), "prepare", guarded).unwrap();
        old.0.send(
            &json!({"op":"set","key":MODEL,"value":"SYNTHETIC_QUEUED_LEGACY_REPOPULATION"})
                .to_string(),
        );
        assert!(old.0.try_line(Duration::from_millis(150)).is_none());
        case.save("held_prepare", &case.fingerprint());
        tx.commit().unwrap();
        let response = old.receive();
        assert_eq!(response["ok"], !guarded);
        if guarded {
            assert!(authoritative(&conn).is_ok());
            assert!(legacy(&conn).model.is_none());
        } else {
            assert!(authoritative(&conn)
                .unwrap_err()
                .contains("conflicting legacy"));
            assert_eq!(
                legacy(&conn).model.as_deref(),
                Some("SYNTHETIC_QUEUED_LEGACY_REPOPULATION")
            );
        }
        assert!(
            old.call(json!({"op":"set","key":"fixture.unrelated","value":"synthetic-safe"}))["ok"]
                .as_bool()
                .unwrap()
        );
        let generation: i64 = conn
            .query_row("SELECT generation FROM probe_q_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(generation, 1);
        drop(conn);
        old.close();
        let report = doctor(&case, "after-old-setter");
        assert_eq!(selected(&report).is_null(), guarded);
        case.finish(if guarded{"regression_invariant"}else{"known_unsafe_characterization"},"actual historical Store setter queued across fixture cutover repopulates unguarded legacy metadata; static guarded case refuses and preserves private generation",json!({"guarded":guarded,"old_setter_reply":response,"generation":generation,"unrelated_old_setter_succeeded":true,"diagnostic_model_absent":guarded,"timing_limit":"request spans held IMMEDIATE; exact old wait instruction is not claimed","migration_scope":"explicit raw M6 revision simulation, not an accepted production migration"}));
    }
}

#[test]
#[ignore = "run S3e.3c disposable feasibility runner"]
fn q04_hot_journal_preparation_and_private_update_recovery() {
    let case = Case::new("q04_hot_journal_boundaries");
    minimal(&case.db, "DELETE", 17);
    let mut child = actor(&case, "prepare_flush");
    receive(&mut child, "prepared_uncommitted");
    child.kill();
    assert!(!child.wait().success());
    case.save("initial_prepare_crash", &case.fingerprint());
    let report = doctor(&case, "before-initial-recovery");
    assert!(selected(&report).is_null());
    let conn = Connection::open(&case.db).unwrap();
    assert_eq!(legacy(&conn), pair(OLD_VALUE));
    assert!(!table(&conn, "probe_q_state"));
    drop(conn);
    let restored = doctor(&case, "after-initial-recovery");
    assert_eq!(selected(&restored), OLD_VALUE);
    let conn = Connection::open(&case.db).unwrap();
    prepare(&conn, &pair(NEW_VALUE), "prepare", true);
    drop(conn);
    let mut child = actor(&case, "update_flush");
    receive(&mut child, "updated_uncommitted");
    child.kill();
    assert!(!child.wait().success());
    let journal = fs::read(format!("{}-journal", case.db.display())).unwrap();
    assert!(journal.len() > 512);
    assert_eq!(
        &journal[..8],
        &[0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7]
    );
    case.save("private_update_crash", &case.fingerprint());
    let report = doctor(&case, "before-private-recovery");
    assert!(selected(&report).is_null());
    let readonly =
        Connection::open_with_flags(&case.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let e = readonly
        .query_row("SELECT generation FROM probe_q_state", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap_err();
    assert_eq!(
        e.sqlite_error().unwrap().extended_code,
        rusqlite::ffi::SQLITE_READONLY_ROLLBACK
    );
    drop(readonly);
    let conn = Connection::open(&case.db).unwrap();
    assert_eq!(authoritative(&conn).unwrap(), (1, pair(NEW_VALUE)));
    drop(conn);
    case.save("after_recovery", &case.fingerprint());
    case.finish("known_unsafe_characterization","initial preparation rollback restores legacy disclosure; a later private-only update crash does not repopulate legacy keys",json!({"initial_recovery_legacy_disclosed":OLD_VALUE,"new_uncommitted_private_marker_disclosed":false,"private_update_recovered_generation":1,"scope":"genuine SQLite cache-flushed minimal DELETE fixture, not ordinary dirty reads or populated production migration proof"}));
}

#[test]
#[ignore = "run S3e.3c disposable feasibility runner"]
fn q05_wal_main_visibility_and_shm_effects() {
    let case = Case::new("q05_wal_main_shm");
    minimal(&case.db, "WAL", 17);
    let mut held = actor(&case, "wal_hold");
    receive(&mut held, "connection_held");
    let writer = Connection::open(&case.db).unwrap();
    writer.execute_batch("PRAGMA wal_autocheckpoint=0").unwrap();
    prepare(&writer, &pair(NEW_VALUE), "prepare", true);
    case.save("wal_cutover", &case.fingerprint());
    assert!(legacy(&writer).model.is_none());
    let uri = format!("file:{}?immutable=1", case.db.display());
    let immutable = Connection::open_with_flags(
        uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap();
    assert_eq!(legacy(&immutable), pair(OLD_VALUE));
    drop(immutable);
    let observed = doctor(&case, "wal-aware-old-doctor");
    assert!(selected(&observed).is_null());
    drop(writer);
    held.kill();
    assert!(!held.wait().success());
    let shm = PathBuf::from(format!("{}-shm", case.db.display()));
    assert!(shm.exists());
    fs::remove_file(&shm).unwrap();
    let before = case.fingerprint();
    case.save("before_missing_shm", &before);
    let report = doctor(&case, "recreating-shm-doctor");
    assert!(selected(&report).is_null());
    let after = case.fingerprint();
    assert!(after["-shm"]["exists"].as_bool().unwrap());
    assert_eq!(before["main"]["sha256"], after["main"]["sha256"]);
    assert_eq!(before["-wal"]["sha256"], after["-wal"]["sha256"]);
    case.finish("known_unsafe_characterization","WAL-aware legacy lookup omits private values but immutable MAIN retains old committed settings; read-only diagnostics recreate SHM",json!({"immutable_main_old_model":OLD_VALUE,"wal_aware_old_model":null,"shm_created":true,"before":before,"after":after}));
}

fn lease(file: &File, mode: i32) -> io::Result<()> {
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLEASE, mode) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[test]
#[ignore = "run S3e.3c disposable feasibility runner"]
fn q06_drain_witness_does_not_protect_private_raw_access() {
    let case = Case::new("q06_inode_drain");
    minimal(&case.db, "WAL", 17);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&case.db)
        .unwrap();
    if let Err(e) = lease(&file, libc::F_WRLCK) {
        let n = e.raw_os_error().unwrap();
        if [
            libc::EINVAL,
            libc::ENOSYS,
            libc::EOPNOTSUPP,
            libc::EPERM,
            libc::EACCES,
        ]
        .contains(&n)
        {
            case.skip(&format!("native lease unavailable: {e} (errno {n})"));
            return;
        }
        panic!("clean baseline lease failure {e}");
    }
    lease(&file, libc::F_UNLCK).unwrap();
    let mut held = actor(&case, "held_snapshot");
    receive(&mut held, "snapshot_held");
    assert_eq!(
        lease(&file, libc::F_WRLCK).unwrap_err().raw_os_error(),
        Some(libc::EAGAIN)
    );
    let conn = Connection::open(&case.db).unwrap();
    prepare(&conn, &pair(NEW_VALUE), "prepare", true);
    drop(conn);
    release(held);
    lease(&file, libc::F_WRLCK).unwrap();
    lease(&file, libc::F_UNLCK).unwrap();
    drop(file);
    let raw = Connection::open(&case.db).unwrap();
    assert_eq!(authoritative(&raw).unwrap().1, pair(NEW_VALUE));
    case.finish("known_unsafe_characterization","snapshot connection blocks exclusive witness; closure permits acquisition, but raw same-UID access reads private metadata after release",json!({"held_errno":libc::EAGAIN,"acquired_after_close":true,"raw_private_model":NEW_VALUE,"limitation":"No future admission, lease handoff or hostile same-UID exclusion is established"}));
}

#[test]
#[ignore = "run S3e.3c disposable feasibility runner"]
fn q07_floor_ordering_has_distinct_partial_states() {
    for floor_first in [true, false] {
        let case = Case::new(if floor_first {
            "q07_floor_first"
        } else {
            "q07_quarantine_first"
        });
        minimal(&case.db, "WAL", 16);
        let conn = Connection::open(&case.db).unwrap();
        if floor_first {
            conn.execute("INSERT INTO schema_migrations VALUES(17)", [])
                .unwrap();
        } else {
            prepare(&conn, &pair(NEW_VALUE), "prepare", true);
        }
        drop(conn);
        let report = doctor(&case, "partial-order-doctor");
        if floor_first {
            assert!(incompatible(&report));
            assert_eq!(selected(&report), OLD_VALUE);
        } else {
            assert!(!incompatible(&report));
            assert!(selected(&report).is_null());
        }
        let conn = Connection::open(&case.db).unwrap();
        if floor_first {
            prepare(&conn, &pair(NEW_VALUE), "prepare", true);
        } else {
            conn.execute("INSERT INTO schema_migrations VALUES(17)", [])
                .unwrap();
        }
        assert_eq!(authoritative(&conn).unwrap().1, pair(NEW_VALUE));
        drop(conn);
        case.finish("known_unsafe_characterization","split floor/quarantine ordering exposes either incompatible-but-disclosing or no-floor-but-relocated intermediate state",json!({"floor_first":floor_first,"intermediate_incompatible":floor_first,"intermediate_legacy_disclosed":floor_first,"scope":"minimal native version16->17 simulation, no accepted migration0018 or current Store admission proof"}));
    }
    let case = Case::new("q07_single_transaction");
    minimal(&case.db, "WAL", 16);
    let conn = Connection::open(&case.db).unwrap();
    let tx = begin(&conn);
    tx.execute("INSERT INTO schema_migrations VALUES(17)", [])
        .unwrap();
    install(&tx, &pair(NEW_VALUE), "prepare", true).unwrap();
    tx.commit().unwrap();
    assert_eq!(authoritative(&conn).unwrap().1, pair(NEW_VALUE));
    assert_eq!(
        conn.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        17
    );
    assert_eq!(
        legacy(&conn),
        Pair {
            enabled: None,
            model: None
        }
    );
    drop(conn);
    let joint_report = doctor(&case, "joint-cutover-doctor");
    assert!(incompatible(&joint_report));
    assert!(selected(&joint_report).is_null());
    case.finish("regression_invariant","one native transaction commits floor marker plus both candidate domains together",json!({"scope":"minimal native atomicity only, not existing Store routing or read eviction"}));
    let case = Case::new("q07_future_sentinel_rejection");
    minimal(&case.db, "WAL", 17);
    let conn = Connection::open(&case.db).unwrap();
    conn.execute("INSERT INTO schema_migrations VALUES(999)", [])
        .unwrap();
    drop(conn);
    for (label, binary) in [("old", common::old()), ("current", common::current())] {
        let mut command = case.mind(&binary);
        command.arg("init");
        let out = common::run(command, label, &case);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("newer than supported version"));
    }
    let report = doctor(&case, "future-sentinel-old-doctor");
    assert_eq!(selected(&report), OLD_VALUE);
    case.finish("known_unsafe_characterization","a deliberately impossible native fixture future sentinel rejects fresh old/current Store opens but does not suppress old diagnostics",json!({"fixture_sentinel":999,"not_an_allocated_schema_or_migration":true,"old16_fresh_store_rejected":true,"current17_fresh_store_rejected":true,"old_diagnostic_model":OLD_VALUE}));
}

#[test]
#[ignore = "run S3e.3c disposable feasibility runner"]
fn q08_interrupted_prepare_and_postcommit_reply_loss() {
    let case = Case::new("q08_known_precommit_crash");
    minimal(&case.db, "WAL", 17);
    let mut child = actor(&case, "prepare_wait");
    receive(&mut child, "prepared_uncommitted");
    child.kill();
    assert!(!child.wait().success());
    let conn = Connection::open(&case.db).unwrap();
    assert!(!table(&conn, "probe_q_state"));
    assert_eq!(legacy(&conn), pair(OLD_VALUE));
    drop(conn);
    case.finish("regression_invariant","controlled precommit process termination rolls back both prototype metadata domains",json!({"old_pair_restored":true,"candidate_marker_absent":true,"scope":"known precommit point, not generic UNKNOWN rollback"}));
    let case = Case::new("q08_commit_before_reply_loss");
    minimal(&case.db, "WAL", 17);
    let mut child = actor(&case, "commit_before_reply");
    receive(&mut child, "committed_before_reply");
    child.kill();
    assert!(!child.wait().success());
    let conn = Connection::open(&case.db).unwrap();
    assert_eq!(authoritative(&conn).unwrap(), (1, pair(NEW_VALUE)));
    let receipt: (i64, String) = conn
        .query_row(
            "SELECT generation,intent FROM probe_q_receipt WHERE operation='prepare'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(receipt, (1, canonical(&pair(NEW_VALUE))));
    assert_eq!(update(&conn, 0, &pair(NEW_VALUE), "prepare").unwrap(), 1);
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM probe_q_pair", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    case.finish("regression_invariant","a controlled postcommit/pre-reply crash is resolved by exact durable prototype receipt replay without another generation",json!({"recovered_generation":1,"receipt_operation":"prepare","limitation":"test-control commit acknowledgement is not a production lost-ACK receipt protocol or liveness proof"}));
}

#[test]
#[ignore = "run S3e.3c disposable feasibility runner"]
fn q09_generation_cas_replay_and_corruption_refuse() {
    let case = Case::new("q09_generation_and_corruption");
    minimal(&case.db, "WAL", 17);
    let conn = Connection::open(&case.db).unwrap();
    // Preserve exact optional/arbitrary TEXT rather than normalizing configuration.
    conn.execute(
        "UPDATE mind_meta SET value='not-a-boolean' WHERE key='embedding.enabled'",
        [],
    )
    .unwrap();
    conn.execute("DELETE FROM mind_meta WHERE key='embedding.model'", [])
        .unwrap();
    let arbitrary = legacy(&conn);
    assert_eq!(
        arbitrary,
        Pair {
            enabled: Some("not-a-boolean".into()),
            model: None
        }
    );
    prepare(&conn, &arbitrary, "prepare", true);
    assert_eq!(authoritative(&conn).unwrap().1, arbitrary);
    assert_eq!(update(&conn, 1, &pair(NEW_VALUE), "update").unwrap(), 2);
    assert_eq!(update(&conn, 1, &pair(NEW_VALUE), "update").unwrap(), 2);
    assert!(update(&conn, 1, &pair(NEXT_VALUE), "update")
        .unwrap_err()
        .contains("changed"));
    assert!(update(&conn, 1, &pair(NEXT_VALUE), "stale")
        .unwrap_err()
        .contains("stale"));
    for sql in [
        "UPDATE probe_q_state SET protocol='corrupt'",
        "DELETE FROM probe_q_pair WHERE generation=2",
        "DELETE FROM probe_q_receipt WHERE operation='update'",
        "UPDATE probe_q_pair SET model='tampered' WHERE generation=2",
        "DELETE FROM probe_q_state",
    ] {
        let tx = conn.unchecked_transaction().unwrap();
        tx.execute_batch(sql).unwrap();
        assert!(authoritative(&tx).is_err(), "corruption accepted: {sql}");
        tx.rollback().unwrap();
    }
    assert_eq!(authoritative(&conn).unwrap(), (2, pair(NEW_VALUE)));
    conn.execute(
        "INSERT INTO mind_meta VALUES('fixture.unrelated','safe')",
        [],
    )
    .unwrap();
    for sql in ["INSERT OR REPLACE INTO mind_meta VALUES('embedding.model','evil')", "INSERT INTO mind_meta VALUES('embedding.enabled','true') ON CONFLICT(key) DO UPDATE SET value='evil'", "UPDATE mind_meta SET key='embedding.model' WHERE key='fixture.unrelated'"] {assert!(conn.execute_batch(sql).is_err(),"legacy guard bypass {sql}");}
    conn.execute(
        "UPDATE mind_meta SET value='still-safe' WHERE key='fixture.unrelated'",
        [],
    )
    .unwrap();
    assert_eq!(authoritative(&conn).unwrap(), (2, pair(NEW_VALUE)));
    case.finish("regression_invariant","prototype exact replay on intact receipt is stable; changed intent/stale generation and selected-read corruption controls refuse without fallback",json!({"generation":2,"optional_raw_text_preserved":true,"corruption_controls":5,"scope":"one actual legacy-pair fidelity sample and selected-read checks; historical receipt replay corruption is a separate known-unsafe counterexample"}));

    let case = Case::new("q09_exact_replay_corruption_counterexample");
    minimal(&case.db, "WAL", 17);
    let conn = Connection::open(&case.db).unwrap();
    prepare(&conn, &pair(OLD_VALUE), "prepare", true);
    assert_eq!(update(&conn, 1, &pair(NEW_VALUE), "update").unwrap(), 2);
    conn.execute(
        "UPDATE probe_q_receipt SET generation=99 WHERE operation='update'",
        [],
    )
    .unwrap();
    let selected_error = authoritative(&conn).unwrap_err();
    let invalid_replay = update(&conn, 1, &pair(NEW_VALUE), "update").unwrap();
    assert_eq!(invalid_replay, 99);
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM probe_q_pair WHERE generation=99",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    case.finish("known_unsafe_characterization","prototype receipt-first exact replay returns a corrupted never-committed generation although selected-state read refuses; complete receipt validation is missing",json!({"corrupted_receipt_generation":99,"selected_state_error":selected_error,"incorrect_replayed_generation":invalid_replay,"corresponding_pair_exists":false,"scope":"intentional disposable raw corruption; this toy replay path is not accepted recovery or hostile same-UID isolation"}));
}

fn mcp(child: &mut ChildGuard, id: i64, method: &str, params: Value) -> Value {
    child.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string());
    let result: Value = serde_json::from_str(&child.line()).unwrap();
    assert_eq!(result["id"], id);
    assert!(result.get("error").is_none());
    result
}

#[test]
#[ignore = "run S3e.3c disposable feasibility runner"]
fn q10_quarantine_does_not_exclude_old_memory_store_access() {
    let case = Case::new("q10_old_knowledge_boundary");
    for (label, args) in [
        ("init", vec!["init"]),
        ("space", vec!["space", "create", "notes"]),
    ] {
        let mut c = case.mind(&common::old());
        c.args(args);
        common::success(common::run(c, label, &case));
    }
    let note = case.work.join("note.md");
    fs::write(&note, "# Synthetic\n\nquarantinememorycanary.\n").unwrap();
    let mut ingest = case.mind(&common::old());
    ingest.arg("ingest").arg(&note).args(["--space", "notes"]);
    common::success(common::run(ingest, "ingest", &case));
    let mut command = case.mind(&common::old());
    command.arg("mcp").arg("--allow-root").arg(&case.work);
    let mut old = ChildGuard::spawn(command, case.artifacts.join("old-mcp.stderr"));
    mcp(
        &mut old,
        1,
        "initialize",
        json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"s3e3c","version":"1"}}),
    );
    old.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    let mut migrate = case.mind(&common::current());
    migrate.arg("init");
    common::success(common::run(migrate, "migrate17", &case));
    let conn = Connection::open(&case.db).unwrap();
    prepare(&conn, &pair(NEW_VALUE), "prepare", true);
    drop(conn);
    let found = mcp(
        &mut old,
        2,
        "tools/call",
        json!({"name":"memory_search","arguments":{"query":"quarantinememorycanary","mode":"lexical"}}),
    );
    assert_eq!(found["result"]["isError"], false);
    assert!(!found["result"]["structuredContent"]["hits"]
        .as_array()
        .unwrap()
        .is_empty());
    let created = mcp(
        &mut old,
        3,
        "tools/call",
        json!({"name":"memory_create","arguments":{"content":"synthetic ordinary write survives quarantine","spaces":["notes"]}}),
    );
    assert_eq!(created["result"]["isError"], false);
    drop(old);
    case.finish("known_unsafe_characterization","old production MCP still reads active memory and performs ordinary writes after two-key fixture quarantine",json!({"old_active_knowledge_read_succeeded":true,"old_ordinary_write_succeeded":true,"metadata_isolation_is_not_knowledge_or_mutation_isolation":true,"scope":"no active owner/fence/publication or retrieval code changed; exact old metadata paths only"}));
}
