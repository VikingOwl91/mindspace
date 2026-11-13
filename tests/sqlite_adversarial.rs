//! Opt-in native bundled-SQLite characterizations. Passing unsafe cases means
//! the named unresolved behavior was reproduced, never that it was fixed.
#![cfg(target_os = "linux")]

#[path = "support/adversarial_common.rs"]
mod common;

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{Case, ChildGuard};
use rusqlite::{Connection, ErrorCode, OpenFlags};
use serde_json::{json, Value};

const EVENT: &str = "ADVERSARIAL_EVENT ";
const COMMITTED: &str = "SYNTHETIC_COMMITTED_METADATA_000000";
const UNCOMMITTED: &str = "SYNTHETIC_UNCOMMITTED_METADATA_0000";
const WAL_VALUE: &str = "SYNTHETIC_WAL_COMMITTED_METADATA";

fn minimal_db(path: &Path, journal: &str) {
    assert!(matches!(journal, "WAL" | "DELETE"));
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(&format!(
        "PRAGMA journal_mode={journal}; PRAGMA synchronous=FULL;
         CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY);
         CREATE TABLE mind_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
         CREATE TABLE synthetic_payload(id INTEGER PRIMARY KEY,body TEXT NOT NULL);"
    ))
    .unwrap();
    for version in 1..=17 {
        conn.execute("INSERT INTO schema_migrations VALUES(?1)", [version])
            .unwrap();
    }
    conn.execute(
        "INSERT INTO mind_meta VALUES('embedding.enabled','true')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO mind_meta VALUES('embedding.model',?1)",
        [COMMITTED],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO synthetic_payload VALUES(1,?1)",
        ["SYNTHETIC_ACTIVE_CONTENT_NOT_A_MODEL_SETTING"],
    )
    .unwrap();
}

fn value(conn: &Connection) -> String {
    conn.query_row(
        "SELECT value FROM mind_meta WHERE key='embedding.model'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn event(value: Value) {
    println!("\n{EVENT}{value}");
    io::stdout().flush().unwrap();
}

fn release_command() {
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line).unwrap();
    let v: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["command"], "release");
}

/// A subprocess-only protocol endpoint. The runner never counts this as a
/// scenario, and invokes it only with an explicit synthetic database path.
#[test]
#[ignore = "subprocess protocol endpoint, not a standalone scenario"]
fn native_actor() {
    let mode = std::env::var("MINDSPACE_ADVERSARIAL_ACTOR_MODE")
        .expect("actor requires explicit fixture mode");
    let path = PathBuf::from(
        std::env::var_os("MINDSPACE_ADVERSARIAL_ACTOR_DB")
            .expect("actor requires explicit fixture DB"),
    );
    assert!(path.is_absolute() && path.is_file());
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
    conn.busy_timeout(Duration::from_secs(2)).unwrap();
    match mode.as_str() {
        "hot_journal" => {
            conn.execute_batch("PRAGMA synchronous=FULL; BEGIN IMMEDIATE")
                .unwrap();
            conn.execute(
                "UPDATE mind_meta SET value=?1 WHERE key='embedding.model'",
                [UNCOMMITTED],
            )
            .unwrap();
            conn.cache_flush().unwrap();
            event(json!({"state":"flushed_uncommitted", "value":value(&conn)}));
            release_command();
            conn.execute_batch("ROLLBACK").unwrap();
        }
        "wal_writer" => {
            conn.execute_batch("PRAGMA wal_autocheckpoint=0").unwrap();
            conn.execute(
                "UPDATE mind_meta SET value=?1 WHERE key='embedding.model'",
                [WAL_VALUE],
            )
            .unwrap();
            event(json!({"state":"wal_committed", "value":value(&conn)}));
            release_command();
        }
        "idle" => {
            assert_eq!(value(&conn), COMMITTED);
            event(json!({"state":"held", "mode":mode}));
            release_command();
        }
        "snapshot" => {
            conn.execute_batch("BEGIN DEFERRED").unwrap();
            assert_eq!(value(&conn), COMMITTED);
            event(json!({"state":"held", "mode":mode}));
            release_command();
            conn.execute_batch("ROLLBACK").unwrap();
        }
        "prepared" => {
            let statement = conn
                .prepare("SELECT value FROM mind_meta ORDER BY key")
                .unwrap();
            event(json!({"state":"held", "mode":mode}));
            release_command();
            drop(statement);
        }
        "stepped" => {
            let mut statement = conn
                .prepare("SELECT value FROM mind_meta ORDER BY key")
                .unwrap();
            let mut rows = statement.query([]).unwrap();
            assert!(rows.next().unwrap().is_some());
            event(json!({"state":"held", "mode":mode}));
            release_command();
            drop(rows);
            drop(statement);
        }
        other => panic!("unknown fixture actor mode: {other}"),
    }
    drop(conn);
    event(json!({"state":"released"}));
}

fn actor(case: &Case, mode: &str) -> ChildGuard {
    let mut command = case.command(&std::env::current_exe().unwrap());
    command
        .env("MINDSPACE_ADVERSARIAL_ACTOR_MODE", mode)
        .env("MINDSPACE_ADVERSARIAL_ACTOR_DB", &case.db)
        .current_dir(&case.work)
        .args(["--ignored", "--exact", "native_actor", "--nocapture"]);
    ChildGuard::spawn(command, case.artifacts.join(format!("{mode}.stderr")))
}

fn receive_actor(child: &mut ChildGuard, state: &str) -> Value {
    // ChildGuard bounds every line wait. The actor emits a fixed, small
    // libtest preamble and exactly one protocol event per transition.
    for _ in 0..12 {
        let line = child.line();
        if let Some(data) = line.strip_prefix(EVENT) {
            let v: Value = serde_json::from_str(data).unwrap();
            assert_eq!(v["state"], state, "{v}");
            return v;
        }
    }
    panic!("actor did not emit {state}");
}

fn release_actor(mut child: ChildGuard) {
    child.send(r#"{"command":"release"}"#);
    receive_actor(&mut child, "released");
    assert!(child.wait().success());
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}

fn wal_crash_fixture(case: &Case) {
    minimal_db(&case.db, "WAL");
    let mut child = actor(case, "wal_writer");
    receive_actor(&mut child, "wal_committed");
    child.kill();
    assert!(!child.wait().success());
    assert!(sidecar(&case.db, "-wal").is_file());
    assert!(sidecar(&case.db, "-shm").is_file());
}

#[test]
#[ignore = "run scripts/m8-s3e3b-harness.py"]
fn n01_read_only_wal_creates_shm_without_application_write() {
    let case = Case::new("n01_read_only_wal_creates_shm_without_application_write");
    wal_crash_fixture(&case);
    let before_existing = case.fingerprint();
    case.save("before_existing_shm", &before_existing);
    {
        let read = Connection::open_with_flags(&case.db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(value(&read), WAL_VALUE);
        case.save("during_existing_shm", &case.fingerprint());
    }
    let after_existing = case.fingerprint();
    case.save("after_existing_shm", &after_existing);

    // No connection remains. Removing this disposable, reconstructible wal-index
    // sidecar tests native read-only reconstruction, not concurrent unlinking.
    assert!(sidecar(&case.db, "-wal").is_file());
    fs::remove_file(sidecar(&case.db, "-shm")).unwrap();
    let before = case.fingerprint();
    let main_bytes = fs::read(&case.db).unwrap();
    let wal_bytes = fs::read(sidecar(&case.db, "-wal")).unwrap();
    case.save("before_missing_shm", &before);
    let read = Connection::open_with_flags(&case.db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(value(&read), WAL_VALUE);
    assert!(sidecar(&case.db, "-shm").is_file());
    let during = case.fingerprint();
    case.save("during_recreated_shm", &during);
    assert_eq!(fs::read(&case.db).unwrap(), main_bytes);
    assert_eq!(fs::read(sidecar(&case.db, "-wal")).unwrap(), wal_bytes);
    drop(read);
    case.save("after_read_close", &case.fingerprint());
    case.finish(
        "known_unsafe_characterization",
        "native READ_ONLY reconstructs missing SHM while observed MAIN/WAL bytes remain identical",
        json!({"sqlite":rusqlite::version(),"existing_shm_before":before_existing,
            "existing_shm_after":after_existing,"missing_shm_before":before,
            "recreated_shm":during,"scope":"synthetic WAL; no universal filesystem non-mutation claim"}),
    );
}

#[test]
#[ignore = "run scripts/m8-s3e3b-harness.py"]
fn n02_delete_read_only_and_immutable_wal_visibility() {
    let case = Case::new("n02_delete_read_only_and_immutable_wal_visibility");
    minimal_db(&case.db, "DELETE");
    let before = case.fingerprint();
    case.save("before_delete_read_only", &before);
    let main = fs::read(&case.db).unwrap();
    {
        let read = Connection::open_with_flags(&case.db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(value(&read), COMMITTED);
    }
    assert_eq!(fs::read(&case.db).unwrap(), main);
    assert!(!sidecar(&case.db, "-wal").exists());
    assert!(!sidecar(&case.db, "-shm").exists());
    assert!(!sidecar(&case.db, "-journal").exists());
    case.save("after_delete_read_only", &case.fingerprint());
    fs::remove_file(&case.db).unwrap();
    wal_crash_fixture(&case);
    let wal_before = case.fingerprint();
    case.save("before_immutable_wal", &wal_before);
    let mut uri = String::from("file:");
    for byte in case.db.as_os_str().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'.' | b'_' | b'-' | b'~' => {
                uri.push(*byte as char)
            }
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
    uri.push_str("?immutable=1");
    let immutable = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap();
    assert_eq!(value(&immutable), COMMITTED);
    drop(immutable);
    case.save("after_immutable_before_native", &case.fingerprint());
    let read = Connection::open_with_flags(&case.db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(value(&read), WAL_VALUE);
    drop(read);
    case.save("after_immutable_and_native", &case.fingerprint());
    case.finish(
        "regression_invariant",
        "DELETE read-only preserves MAIN; immutable MAIN and native WAL readers have distinct observed values",
        json!({"sqlite":rusqlite::version(),"immutable":COMMITTED,"native_read_only":WAL_VALUE,
            "limitation":"immutable MAIN is not a live WAL freshness source; atime can change on measurement"}),
    );
}

#[test]
#[ignore = "run scripts/m8-s3e3b-harness.py"]
fn n03_snapshot_retention_prepared_steps_and_stale_write() {
    let case = Case::new("n03_snapshot_retention_prepared_steps_and_stale_write");
    minimal_db(&case.db, "WAL");
    let writer = Connection::open(&case.db).unwrap();
    writer.busy_timeout(Duration::ZERO).unwrap();
    writer.execute_batch("PRAGMA wal_autocheckpoint=0").unwrap();
    let reader = Connection::open(&case.db).unwrap();
    reader.busy_timeout(Duration::ZERO).unwrap();
    case.save("before_snapshot", &case.fingerprint());
    reader.execute_batch("BEGIN DEFERRED").unwrap();
    assert_eq!(value(&reader), COMMITTED);
    writer
        .execute(
            "UPDATE mind_meta SET value=?1 WHERE key='embedding.model'",
            [WAL_VALUE],
        )
        .unwrap();
    assert_eq!(value(&reader), COMMITTED);
    assert_eq!(value(&writer), WAL_VALUE);
    let checkpoint: (i64, i64, i64) = writer
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(
        checkpoint.0, 1,
        "held read snapshot must prevent complete truncate checkpoint"
    );
    assert!(fs::metadata(sidecar(&case.db, "-wal")).unwrap().len() > 0);
    case.save("held_snapshot_and_wal", &case.fingerprint());
    let stale = reader
        .execute(
            "UPDATE mind_meta SET value='forbidden_stale_write' WHERE key='embedding.model'",
            [],
        )
        .unwrap_err();
    match stale {
        rusqlite::Error::SqliteFailure(e, _) => {
            assert_eq!(e.extended_code, libc_sqlite_busy_snapshot())
        }
        other => panic!("expected SQLITE_BUSY_SNAPSHOT, got {other}"),
    }
    reader.execute_batch("ROLLBACK").unwrap();
    assert_eq!(value(&reader), WAL_VALUE);
    let complete: (i64, i64, i64) = writer
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(complete.0, 0);
    case.save("released_snapshot_and_checkpoint", &case.fingerprint());

    // Preparation without stepping does not begin the SELECT's data snapshot.
    let mut prepared = reader
        .prepare("SELECT value FROM mind_meta WHERE key='embedding.model'")
        .unwrap();
    writer
        .execute(
            "UPDATE mind_meta SET value='prepared_after_commit' WHERE key='embedding.model'",
            [],
        )
        .unwrap();
    let prepared_value: String = prepared.query_row([], |r| r.get(0)).unwrap();
    assert_eq!(prepared_value, "prepared_after_commit");
    drop(prepared);

    // The partly stepped SELECT pins a snapshot even without explicit BEGIN.
    let mut statement = reader
        .prepare("SELECT key,value FROM mind_meta ORDER BY key")
        .unwrap();
    let mut rows = statement.query([]).unwrap();
    let first = rows.next().unwrap().unwrap();
    assert_eq!(first.get::<_, String>(0).unwrap(), "embedding.enabled");
    writer
        .execute(
            "UPDATE mind_meta SET value='after_first_step' WHERE key='embedding.model'",
            [],
        )
        .unwrap();
    let second = rows.next().unwrap().unwrap();
    assert_eq!(second.get::<_, String>(1).unwrap(), "prepared_after_commit");
    drop(rows);
    drop(statement);
    assert_eq!(value(&reader), "after_first_step");
    case.save("after_prepared_statement_release", &case.fingerprint());
    case.finish("regression_invariant",
        "held snapshot retains old committed values/WAL, stale upgrade fails BUSY_SNAPSHOT, preparing and stepping have different snapshot boundaries",
        json!({"sqlite":rusqlite::version(),"held_checkpoint":checkpoint,"released_checkpoint":complete,
            "stale_write_extended_code":libc_sqlite_busy_snapshot(),"prepared_before_step":prepared_value,
            "partly_stepped_value":"prepared_after_commit","after_statement_release":"after_first_step"}));
}

fn libc_sqlite_busy_snapshot() -> i32 {
    rusqlite::ffi::SQLITE_BUSY_SNAPSHOT
}

#[test]
#[ignore = "run scripts/m8-s3e3b-harness.py"]
fn n04_hot_journal_diagnostic_exposure_and_native_recovery() {
    let case = Case::new("n04_hot_journal_diagnostic_exposure_and_native_recovery");
    minimal_db(&case.db, "DELETE");
    case.save("committed_baseline", &case.fingerprint());
    let mut writer = actor(&case, "hot_journal");
    receive_actor(&mut writer, "flushed_uncommitted");
    case.save("before_fixture_writer_crash", &case.fingerprint());
    assert!(sidecar(&case.db, "-journal").is_file());
    writer.kill();
    assert!(!writer.wait().success());
    let journal = fs::read(sidecar(&case.db, "-journal")).unwrap();
    assert!(
        journal.len() > 512,
        "cache-flushed transaction leaves an actual SQLite journal"
    );
    assert_eq!(
        &journal[..8],
        &[0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7],
        "SQLite wrote the journal header; this is not hand-fabricated bytes"
    );
    let main_before = fs::read(&case.db).unwrap();
    assert!(main_before
        .windows(UNCOMMITTED.len())
        .any(|w| w == UNCOMMITTED.as_bytes()));
    let crashed = case.fingerprint();
    case.save("crashed_before_any_native_reopen", &crashed);
    let mut doctor = case.mind(&common::old());
    doctor.args(["doctor", "--json"]);
    let output = common::run(doctor, "hot_journal_old_doctor", &case);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        !output.status.success(),
        "incompatible synthetic schema17 must not be compatible with old16"
    );
    let checks = report["checks"].as_array().unwrap();
    let database = checks.iter().find(|c| c["id"] == "database").unwrap();
    assert_eq!(database["status"], "fail");
    assert!(database["summary"].as_str().unwrap().contains("newer"));
    let embeddings = checks.iter().find(|c| c["id"] == "embeddings").unwrap();
    assert_eq!(embeddings["details"]["selected_model"],UNCOMMITTED,
        "known-unsafe characterization must reproduce the synthetic uncommitted metadata disclosure");
    assert!(!String::from_utf8_lossy(&output.stdout)
        .contains("SYNTHETIC_ACTIVE_CONTENT_NOT_A_MODEL_SETTING"));
    assert!(!String::from_utf8_lossy(&output.stderr)
        .contains("SYNTHETIC_ACTIVE_CONTENT_NOT_A_MODEL_SETTING"));
    assert_eq!(fs::read(&case.db).unwrap(), main_before);
    assert_eq!(fs::read(sidecar(&case.db, "-journal")).unwrap(), journal);
    case.save("after_old_doctor_before_recovery", &case.fingerprint());

    let read = Connection::open_with_flags(&case.db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let recovery_refusal = read
        .query_row(
            "SELECT value FROM mind_meta WHERE key='embedding.model'",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap_err();
    let refusal_code = match &recovery_refusal {
        rusqlite::Error::SqliteFailure(e, _) => {
            assert_eq!(e.code, ErrorCode::ReadOnly);
            assert_eq!(e.extended_code, rusqlite::ffi::SQLITE_READONLY_ROLLBACK);
            e.extended_code
        }
        other => panic!("expected read-only recovery refusal, got {other}"),
    };
    drop(read);
    case.save("after_native_read_only_refusal", &case.fingerprint());
    let recovered = Connection::open(&case.db).unwrap();
    assert_eq!(value(&recovered), COMMITTED);
    let integrity: String = recovered
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    assert!(!sidecar(&case.db, "-journal").exists());
    drop(recovered);
    case.save("after_native_read_write_recovery", &case.fingerprint());
    case.finish("known_unsafe_characterization",
        "old immutable diagnostic reads spilled uncommitted MAIN from genuine synthetic hot journal; native RO refuses recovery and RW restores committed metadata",
        json!({"sqlite":rusqlite::version(),"old_doctor_selected_model":UNCOMMITTED,
            "native_ro_error":recovery_refusal.to_string(),"native_ro_extended_code":refusal_code,
            "recovered":COMMITTED,"integrity":integrity,"active_payload_observed_in_tested_output":false,
            "limitation":"minimal synthetic schema, not a populated Mindspace migration or normal SQLite dirty-read proof"}));
}

fn lease(file: &File, operation: i32) -> io::Result<()> {
    // The descriptor references this fixture's owned MAIN inode only. Leases
    // are released immediately; no unsupported lease-to-SQLite handoff occurs.
    let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLEASE, operation) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[test]
#[ignore = "run scripts/m8-s3e3b-harness.py"]
fn n05_inode_drain_open_connection_states_release_and_retry() {
    let case = Case::new("n05_inode_drain_open_connection_states_release_and_retry");
    minimal_db(&case.db, "WAL");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&case.db)
        .unwrap();
    let baseline = lease(&file, libc::F_WRLCK);
    if let Err(error) = baseline {
        let errno = error.raw_os_error().unwrap();
        if [
            libc::ENOSYS,
            libc::EOPNOTSUPP,
            libc::EINVAL,
            libc::EPERM,
            libc::EACCES,
        ]
        .contains(&errno)
        {
            case.save(
                "unsupported_lease_baseline",
                &json!({"errno":errno,"error":error.to_string(),"family":case.fingerprint()}),
            );
            case.skip(&format!(
                "native F_SETLEASE/F_WRLCK unavailable at clean baseline: errno {errno}: {error}"
            ));
            return;
        }
        panic!("unexplained lease failure at clean fixture baseline: {error}");
    }
    lease(&file, libc::F_UNLCK).unwrap();
    let mut outcomes = Vec::new();
    for mode in ["idle", "snapshot", "prepared", "stepped"] {
        let mut child = actor(&case, mode);
        let ack = receive_actor(&mut child, "held");
        assert_eq!(ack["mode"], mode);
        case.save(&format!("{mode}_held"), &case.fingerprint());
        let error = lease(&file, libc::F_WRLCK).expect_err(
            "another acknowledged native connection must prevent exclusive inode lease",
        );
        assert_eq!(error.raw_os_error(),Some(libc::EAGAIN),"baseline lease works; unexpected held-handle errno is a failure, never an environment skip");
        release_actor(child);
        lease(&file, libc::F_WRLCK).unwrap();
        lease(&file, libc::F_UNLCK).unwrap();
        case.save(&format!("{mode}_released"), &case.fingerprint());
        outcomes.push(json!({"mode":mode,"held_errno":error.raw_os_error(),"acquired_after_connection_release":true}));
    }
    drop(file);
    // Same-UID raw SQLite remains possible after witness release. This is
    // outside the guaranteed admission-respecting Store exclusion model.
    let raw = Connection::open(&case.db).unwrap();
    assert_eq!(value(&raw), COMMITTED);
    drop(raw);
    case.finish("known_unsafe_characterization",
        "acknowledged native open connections block a working exclusive inode lease; closure permits retry, and raw SQLite can reopen after lease release",
        json!({"sqlite":rusqlite::version(),"states":outcomes,"raw_read_after_release":COMMITTED,
            "limitation":"all states retain a connection; this does not attribute blocking to prepared statements alone or prove future knowledge/mutation exclusion"}));
}
