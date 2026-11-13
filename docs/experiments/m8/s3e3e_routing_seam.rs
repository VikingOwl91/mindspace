//! Disposable unit-test child of storage::sqlite; never production routing.
//! Acceptance oracles established before execution:
//! E01 preserve populated knowledge and all supported raw metadata values.
//! E02 real connection-local M6 authorization applies; no metadata read ACL is invented.
//! E03 successful writes allocate exactly one R, reads/refusals allocate none; no app quota exists.
//! E04 transaction-bound layout and pair observations remain coherent across concurrent commits.
//! E05 queued historical Store writes cannot mutate relocated authority or leave phantom R.
//! E06 held/stepped OLD snapshots are allowed; later old-name execution fails.
//! E07 precommit errors and real vetoed commits preserve prior rows/R through actual cleanup.
//! E08 actual lost acknowledgement retains committed rows/R and forbids blind write retry.
//! E09 missing/ambiguous/malformed layout or unsupported source refuses without fallback.
//! E10 absent optional keys remain absent; a cross-transaction pair is not a snapshot guarantee.
//! E11 actual historical diagnostics distinguish OLD disclosure from tested stable NEW absence.
//! E12 stale expected R/source refuses atomically; refresh is a separate explicit operation.
//! Neither A nor B is selected. A-shaped tables outside the real M6 inventory are
//! deliberately characterized as unguarded, not endowed with a synthetic permit.

use super::*;
#[path = "s3e3e_common.rs"]
mod common;
use common::{Case, ChildGuard};
use serde_json::{json, Value as Json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc;

const MOVED: &str = "probe_m6_meta";
const ENABLED: &str = "embedding.enabled";
const MODEL: &str = "embedding.model";
const OLD: &str = "SYNTHETIC_M6_OLD";
const NEW: &str = "SYNTHETIC_M6_NEW";
const NEXT: &str = "SYNTHETIC_M6_NEXT";
type Metadata = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Observation {
    revision: i64,
    table: &'static str,
    values: Metadata,
}
fn refusal(detail: &str) -> Error {
    Error::Storage(format!("fixture routing: {detail}"))
}
fn revision(c: &Connection) -> i64 {
    c.query_row(
        "SELECT COALESCE(MAX(revision_id),0) FROM m6_revisions",
        [],
        |r| r.get(0),
    )
    .unwrap()
}
fn objects(c: &Connection, name: &str) -> Vec<String> {
    c.prepare("SELECT type FROM sqlite_schema WHERE name=?1")
        .unwrap()
        .query_map([name], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap()
}
fn normalized(sql: &str) -> String {
    sql.replace('"', "")
        .trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn selected(c: &Connection) -> Result<&'static str> {
    // Both discovery and validation run in the caller's existing transaction.
    validate_fence_inventory(c)?;
    for name in ["probe_q_pair", "probe_q_state", "probe_q_receipt"] {
        if !objects(c, name).is_empty() {
            return Err(refusal("unsupported A-shaped authority"));
        }
    }
    let legacy = objects(c, "mind_meta");
    let moved = objects(c, MOVED);
    let target = match (legacy.as_slice(), moved.as_slice()) {
        ([kind], []) if kind == "table" => "mind_meta",
        ([], [kind]) if kind == "table" => MOVED,
        _ => return Err(refusal("missing, ambiguous or aliased metadata authority")),
    };
    let mut stmt = c
        .prepare(&format!("PRAGMA table_info({target})"))
        .map_err(dberr)?;
    let columns = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(5)?,
            ))
        })
        .map_err(dberr)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(dberr)?;
    if columns
        != vec![
            ("key".into(), "TEXT".into(), 0, 1),
            ("value".into(), "TEXT".into(), 1, 0),
        ]
    {
        return Err(refusal("unsupported metadata columns"));
    }
    // Validation only: compare the genuine guard bodies/bindings, never install a new gate.
    for (suffix, event) in [("i", "INSERT"), ("u", "UPDATE"), ("d", "DELETE")] {
        let name = format!("m6_fence_mind_meta_{suffix}");
        let (table, sql): (String, String) = c
            .query_row(
                "SELECT tbl_name,sql FROM sqlite_schema WHERE type='trigger' AND name=?1",
                [&name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(dberr)?;
        let expected=format!("CREATE TRIGGER {name} BEFORE {event} ON {target} WHEN m6_write_revision() IS NULL OR NOT EXISTS (SELECT 1 FROM m6_revisions WHERE revision_id=m6_write_revision()) BEGIN SELECT RAISE(ABORT, 'revisioned write required'); END");
        if table != target || normalized(&sql) != normalized(&expected) {
            return Err(refusal("metadata guard body or binding mismatch"));
        }
    }
    Ok(target)
}
fn values(c: &Connection, target: &str) -> Result<Metadata> {
    assert!(matches!(target, "mind_meta" | MOVED));
    let mut stmt = c
        .prepare(&format!("SELECT key,value FROM {target} ORDER BY key"))
        .map_err(dberr)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(dberr)?;
    rows.collect::<std::result::Result<_, _>>().map_err(dberr)
}
fn observe(c: &Connection) -> Result<Observation> {
    let table = selected(c)?;
    Ok(Observation {
        revision: revision(c),
        table,
        values: values(c, table)?,
    })
}
fn read(store: &SqliteStore) -> Result<Observation> {
    let c = store.connection()?;
    let tx = Transaction::new_unchecked(&c, TransactionBehavior::Deferred).map_err(dberr)?;
    let result = observe(&tx)?;
    tx.commit().map_err(dberr)?;
    Ok(result)
}
fn get(store: &SqliteStore, key: &str) -> Result<Option<String>> {
    Ok(read(store)?.values.get(key).cloned())
}
fn apply(c: &Connection, table: &str, writes: &[(&str, Option<&str>)]) -> Result<()> {
    assert!(matches!(table, "mind_meta" | MOVED));
    for (key, value) in writes {
        match value {
            Some(value) => {
                c.execute(&format!("INSERT INTO {table}(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value"),rusqlite::params![key,value]).map_err(dberr)?;
            }
            None => {
                c.execute(&format!("DELETE FROM {table} WHERE key=?1"), [key])
                    .map_err(dberr)?;
            }
        }
    }
    Ok(())
}
fn write(
    store: &SqliteStore,
    writes: &[(&str, Option<&str>)],
    expected: Option<&Observation>,
) -> Result<i64> {
    let c = store.connection()?;
    let tx = begin_write(&c, &store.revision_state)?;
    let observation = observe(&tx)?;
    let allocated = observation.revision;
    if let Some(expected) = expected {
        if expected.revision != allocated - 1
            || expected.table != observation.table
            || expected.values != observation.values
        {
            return Err(refusal(
                "stale expected revision/layout/source; explicit refresh required",
            ));
        }
    }
    apply(&tx, observation.table, writes)?;
    tx.commit()?;
    Ok(allocated)
}
fn pair(store: &SqliteStore, enabled: Option<&str>, model: Option<&str>) -> Result<i64> {
    write(store, &[(ENABLED, enabled), (MODEL, model)], None)
}
fn cutover(store: &SqliteStore) -> Result<i64> {
    let c = store.connection()?;
    let tx = begin_write(&c, &store.revision_state)?;
    let before = observe(&tx)?; // row decoding validates supported input BEFORE DDL.
    if before.table != "mind_meta" {
        return Err(refusal("not a legacy cutover source"));
    }
    tx.execute_batch("ALTER TABLE mind_meta RENAME TO probe_m6_meta")
        .map_err(dberr)?;
    let after = observe(&tx)?;
    if after.table != MOVED || after.values != before.values {
        return Err(refusal("cutover fidelity mismatch"));
    }
    tx.commit()?;
    Ok(before.revision)
}
fn plain_rows(c: &Connection) -> BTreeMap<String, Vec<String>> {
    let names:Vec<String>=c.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT IN ('mind_meta','probe_m6_meta','m6_revisions') ORDER BY name").unwrap()
        .query_map([],|r|r.get(0)).unwrap().collect::<std::result::Result<_,_>>().unwrap();
    names
        .into_iter()
        .map(|name| {
            let columns: Vec<String> = c
                .prepare(&format!("PRAGMA table_info(\"{name}\")"))
                .unwrap()
                .query_map([], |r| r.get(1))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            let expr = columns
                .iter()
                .map(|col| format!("quote(\"{col}\")"))
                .collect::<Vec<_>>()
                .join("||'|'||");
            let mut rows: Vec<String> = c
                .prepare(&format!("SELECT {expr} FROM \"{name}\" "))
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            rows.sort();
            (name, rows)
        })
        .collect()
}
fn populated(case: &Case) -> SqliteStore {
    let store = SqliteStore::open(&case.db).unwrap();
    store
        .create_space("notes", "SYNTHETIC actual M6 fixture")
        .unwrap();
    let path = case.work.join("synthetic.md");
    std::fs::write(&path, "# Synthetic\n\nm6seamknowledgecanary.\n").unwrap();
    crate::ingest::ingest_file(&store, &path, &["notes"]).unwrap();
    for (key, value) in [
        (ENABLED, "false"),
        (MODEL, OLD),
        ("fixture.empty", ""),
        ("fixture.unrelated", "SYNTHETIC_\0_\n_\u{202e}_TEXT"),
    ] {
        store.set_meta(key, value).unwrap();
    }
    store
}
fn assert_active_zero(store: &SqliteStore) {
    assert_eq!(
        store.revision_state.active_revision.load(Ordering::SeqCst),
        0
    );
    let c = store.connection().unwrap();
    assert!(c.is_autocommit());
    assert_eq!(
        c.query_row("SELECT m6_write_revision()", [], |r| r
            .get::<_, Option<i64>>(0))
            .unwrap(),
        None
    );
}
fn finish(case: &Case, kind: &str, oracle: &str, details: Json) {
    case.save("after", &case.fingerprint());
    case.finish(kind, oracle, details);
}

#[test]
#[ignore = "run disposable S3e.3e runner"]
fn e01_populated_routing_uses_real_m6_and_preserves_ordinary_rows() {
    let case = Case::new("e01_preservation");
    let store = populated(&case);
    let before = read(&store).unwrap();
    let ordinary = plain_rows(&store.connection().unwrap());
    case.save("before", &case.fingerprint());
    assert_eq!(get(&store, MODEL).unwrap().as_deref(), Some(OLD));
    let allocated = cutover(&store).unwrap();
    assert_eq!(allocated, before.revision + 1);
    let after = read(&store).unwrap();
    assert_eq!(after.values, before.values);
    assert_eq!(after.table, MOVED);
    assert_eq!(plain_rows(&store.connection().unwrap()), ordinary);
    let r = pair(&store, Some("true"), Some(NEW)).unwrap();
    assert_eq!(r, allocated + 1);
    assert_eq!(get(&store, MODEL).unwrap().as_deref(), Some(NEW));
    let r2 = write(
        &store,
        &[("fixture.unrelated", Some("SYNTHETIC_UPDATED"))],
        None,
    )
    .unwrap();
    assert_eq!(r2, r + 1);
    assert_eq!(
        get(&store, "fixture.unrelated").unwrap().as_deref(),
        Some("SYNTHETIC_UPDATED")
    );
    assert!(store.get_meta(MODEL).is_err()); // actual unchanged API still lacks routing.
    assert_active_zero(&store);
    finish(&case,"regression_invariant","unit-test seam resolves layout inside actual read/write transactions, preserves ordinary rows and allocates actual R",json!({"before_R":before.revision,"cutover_R":allocated,"pair_R":r,"generic_R":r2,"public_Store_routing":"not implemented; public old-name getter explicitly fails","permission_scope":"actual M6 context/guards, not an administrative cutover permit"}));
}

#[test]
#[ignore = "run disposable S3e.3e runner"]
fn e02_real_authorization_and_additional_admin_gate_remain_enforced() {
    let case = Case::new("e02_authorization");
    let store = populated(&case);
    cutover(&store).unwrap();
    let before = read(&store).unwrap();
    let c = store.connection().unwrap();
    let direct = c
        .execute(
            "INSERT INTO probe_m6_meta VALUES('fixture.outside','SYNTHETIC')",
            [],
        )
        .unwrap_err()
        .to_string();
    assert!(direct.contains("revisioned write required"));
    let tx = begin_write(&c, &store.revision_state).unwrap();
    let r = revision(&tx);
    let source: String = tx
        .query_row(
            "SELECT id FROM logical_sources WHERE kind='file' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let administrative=tx.execute("INSERT INTO asset_ownership(asset_id,source_id,owner,epoch,operation_id,created_revision,updated_revision) VALUES(?1,?2,'legacy',1,'SYNTHETIC_INVALID_ADMIN',?3,?3)",rusqlite::params![Uuid::now_v7().to_string(),source,r]).unwrap_err().to_string();
    assert!(administrative.contains("validated local administrative"));
    drop(tx);
    drop(c);
    let raw = Connection::open(&case.db).unwrap();
    assert_eq!(
        raw.query_row(
            "SELECT value FROM probe_m6_meta WHERE key=?1",
            [MODEL],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        OLD
    );
    assert!(raw
        .execute(
            "UPDATE probe_m6_meta SET value='SYNTHETIC_RAW' WHERE key='embedding.model'",
            []
        )
        .is_err());
    assert_eq!(read(&store).unwrap(), before);
    assert_active_zero(&store);
    finish(&case,"regression_invariant","actual M6 denies out-of-transaction and foreign-connection writes; active M6 R grants no S3b administrative permit",json!({"direct_error":direct,"administrative_error":administrative,"native_read_succeeded":true,"read_ACL":"none in existing metadata API; trusted local account, not hostile same-UID confidentiality"}));
}

#[test]
#[ignore = "run disposable S3e.3e runner"]
fn e03_reads_errors_and_query_only_allocate_no_revision_no_app_quota_is_invented() {
    let case = Case::new("e03_accounting");
    let store = populated(&case);
    let before = read(&store).unwrap();
    for _ in 0..3 {
        assert_eq!(read(&store).unwrap(), before);
    }
    let c = store.connection().unwrap();
    c.pragma_update(None, "query_only", true).unwrap();
    drop(c);
    assert!(pair(&store, Some("true"), Some(NEW)).is_err());
    let c = store.connection().unwrap();
    c.pragma_update(None, "query_only", false).unwrap();
    drop(c);
    assert_eq!(read(&store).unwrap(), before);
    assert_active_zero(&store);
    let large = "SYNTHETIC".repeat(8193); // 73,737 bytes, beyond unrelated admin 64KiB request bound.
    let next = write(&store, &[("fixture.large", Some(&large))], None).unwrap();
    assert_eq!(next, before.revision + 1);
    assert_eq!(get(&store, "fixture.large").unwrap().unwrap(), large);
    finish(&case,"regression_invariant","reads and query-only failure allocate no R; one permitted large metadata write allocates one R",json!({"before_R":before.revision,"after_R":next,"metadata_bytes":large.len(),"quota":"no application metadata quota exists; this is not an unbounded capacity or disk-full guarantee","query_only":"actual SQLite connection restriction, not an invented M6 read ACL"}));
}

#[test]
#[ignore = "run disposable S3e.3e runner"]
fn e04_existing_supported_connection_and_pair_snapshot_span_cutover() {
    let case = Case::new("e04_concurrency");
    let writer = populated(&case);
    let reader = SqliteStore::open(&case.db).unwrap();
    let c = reader.connection().unwrap();
    let held = Transaction::new_unchecked(&c, TransactionBehavior::Deferred).unwrap();
    let old = observe(&held).unwrap();
    let target = cutover(&writer).unwrap();
    pair(&writer, Some("true"), Some(NEW)).unwrap();
    assert_eq!(observe(&held).unwrap(), old);
    held.commit().unwrap();
    drop(c);
    let fresh = read(&reader).unwrap();
    assert_eq!(fresh.table, MOVED);
    assert_eq!(fresh.values[ENABLED], "true");
    assert_eq!(fresh.values[MODEL], NEW);
    let other = SqliteStore::open(&case.db).unwrap();
    let conn = writer.connection().unwrap();
    let tx = begin_write(&conn, &writer.revision_state).unwrap();
    let r = revision(&tx);
    let table = selected(&tx).unwrap();
    apply(&tx, table, &[(ENABLED, Some("false")), (MODEL, Some(NEXT))]).unwrap();
    let (started_rx, finished_rx, join) = {
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let join = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            done_tx
                .send(write(
                    &other,
                    &[("fixture.concurrent", Some("SYNTHETIC"))],
                    None,
                ))
                .unwrap();
        });
        (started_rx, done_rx, join)
    };
    started_rx.recv_timeout(common::WAIT).unwrap();
    assert!(matches!(
        finished_rx.recv_timeout(Duration::from_millis(150)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    assert_eq!(read(&reader).unwrap().values[MODEL], NEW);
    tx.commit().unwrap();
    drop(conn);
    let other_r = finished_rx.recv_timeout(common::WAIT).unwrap().unwrap();
    join.join().unwrap();
    assert_eq!(other_r, r + 1);
    let current = read(&reader).unwrap();
    assert_eq!(current.values[ENABLED], "false");
    assert_eq!(current.values[MODEL], NEXT);
    finish(&case,"regression_invariant","actual transaction snapshot stays OLD across cutover; queued supported writer resolves new layout after serialization",json!({"cutover_R":target,"first_update_R":r,"queued_update_R":other_r,"timing":"acknowledged request spans writer; exact blocked instruction is not claimed"}));
}

struct OldActor(ChildGuard);
impl OldActor {
    fn start(case: &Case) -> Self {
        let exe = PathBuf::from(std::env::var_os("S3E3C_OLD_ACTOR_BINARY").unwrap());
        let mut cmd = case.command(&exe);
        cmd.env("S3E3C_ACTOR_DB", &case.db)
            .env("S3E3C_FIXTURE_ROOT", &case.work)
            .args(["--ignored", "--exact", "old_store_actor", "--nocapture"]);
        let mut actor = Self(ChildGuard::spawn(cmd, case.artifacts.join("old.stderr")));
        assert_eq!(actor.receive()["event"], "ready");
        actor
    }
    fn receive(&mut self) -> Json {
        for _ in 0..12 {
            let line = self.0.line();
            if let Some(v) = line.strip_prefix("S3E3C_OLD ") {
                return serde_json::from_str(v).unwrap();
            }
        }
        panic!("missing old Store event")
    }
    fn call(&mut self, v: Json) -> Json {
        self.0.send(&v.to_string());
        self.receive()
    }
    fn close(mut self) {
        assert_eq!(self.call(json!({"op":"exit"}))["event"], "released");
        assert!(self.0.wait().success());
    }
}
#[test]
#[ignore = "run disposable S3e.3e runner"]
fn e05_actual_old_queued_setter_fails_without_phantom_revision() {
    let case = Case::new("e05_old_queue");
    let mut old = OldActor::start(&case);
    assert_eq!(
        old.call(json!({"op":"set","key":MODEL,"value":OLD}))["ok"],
        true
    );
    let store = SqliteStore::open(&case.db).unwrap();
    let before = read(&store).unwrap();
    let c = store.connection().unwrap();
    let tx = begin_write(&c, &store.revision_state).unwrap();
    let r = revision(&tx);
    let source = observe(&tx).unwrap();
    tx.execute_batch("ALTER TABLE mind_meta RENAME TO probe_m6_meta")
        .unwrap();
    assert_eq!(observe(&tx).unwrap().values, source.values);
    old.0
        .send(&json!({"op":"set","key":MODEL,"value":"SYNTHETIC_QUEUED_OLD"}).to_string());
    assert!(old.0.try_line(Duration::from_millis(150)).is_none());
    tx.commit().unwrap();
    drop(c);
    let response = old.receive();
    assert_eq!(response["ok"], false);
    assert!(response["error"].as_str().unwrap().contains("mind_meta"));
    old.close();
    let after = read(&store).unwrap();
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(after.revision, r);
    assert_eq!(after.values, source.values);
    finish(&case,"regression_invariant","actual historical setter queued across actual M6 cutover fails and rolls back its tentative R",json!({"before_R":before.revision,"after_R":after.revision,"old_reply":response,"legacy_target_absent":true,"scope":"existing historical Store opened16; real library upgrades17"}));
}

#[test]
#[ignore = "run disposable S3e.3e runner"]
fn e06_actual_store_connection_prepared_and_stepped_old_sql() {
    for mode in ["prepared", "stepped"] {
        let case = Case::new(&format!("e06_{mode}"));
        let writer = populated(&case);
        let reader = SqliteStore::open(&case.db).unwrap();
        let c = reader.connection().unwrap();
        let mut stmt = c
            .prepare("SELECT key,value FROM mind_meta ORDER BY key")
            .unwrap();
        if mode == "prepared" {
            cutover(&writer).unwrap();
            pair(&writer, Some("true"), Some(NEW)).unwrap();
            let attempted = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .and_then(|rows| rows.collect::<std::result::Result<Vec<_>, _>>());
            assert!(attempted.is_err());
        } else {
            let mut rows = stmt.query([]).unwrap();
            let first = rows.next().unwrap().unwrap();
            let mut seen = vec![(
                first.get::<_, String>(0).unwrap(),
                first.get::<_, String>(1).unwrap(),
            )];
            cutover(&writer).unwrap();
            pair(&writer, Some("true"), Some(NEW)).unwrap();
            while let Some(row) = rows.next().unwrap() {
                seen.push((row.get(0).unwrap(), row.get(1).unwrap()));
            }
            assert!(seen.iter().any(|(k, v)| k == MODEL && v == OLD));
            assert!(!seen.iter().any(|(_, v)| v == NEW));
        }
        drop(stmt);
        assert!(c
            .query_row("SELECT value FROM mind_meta WHERE key=?1", [MODEL], |r| {
                r.get::<_, String>(0)
            })
            .is_err());
        drop(c);
        assert_eq!(get(&reader, MODEL).unwrap().as_deref(), Some(NEW));
        finish(&case,"regression_invariant","real Store-owned connection prepared-only old SQL fails; stepped statement retains wholly OLD and later old-name SQL fails",json!({"mode":mode,"scope":"actual private connection; not every production Store cache/API path audited"}));
    }
}

#[test]
#[ignore = "run disposable S3e.3e runner"]
fn e07_actual_drop_and_commit_veto_rollback_values_layout_and_revision() {
    let case = Case::new("e07_rollback");
    let store = populated(&case);
    let before = read(&store).unwrap();
    {
        let c = store.connection().unwrap();
        let tx = begin_write(&c, &store.revision_state).unwrap();
        tx.execute_batch("ALTER TABLE mind_meta RENAME TO probe_m6_meta")
            .unwrap();
        apply(&tx, MOVED, &[(MODEL, Some(NEW))]).unwrap();
        assert!(tx
            .execute_batch("CREATE TABLE probe_m6_meta(duplicate TEXT)")
            .is_err());
    }
    assert_eq!(read(&store).unwrap(), before);
    assert_active_zero(&store);
    gate::FAIL_COMMIT.store(true, Ordering::SeqCst);
    let error = cutover(&store).unwrap_err();
    assert!(!matches!(error, Error::CommitOutcomeUnknown { .. }));
    assert_eq!(read(&store).unwrap(), before);
    assert_active_zero(&store);
    assert_eq!(cutover(&store).unwrap(), before.revision + 1);
    finish(&case,"regression_invariant","actual WriteTransaction Drop and SQLite commit veto roll back layout/values/R and clear active authorization",json!({"before_R":before.revision,"commit_veto_error":error.to_string(),"after_successful_cutover_R":read(&store).unwrap().revision}));
}

#[test]
#[ignore = "run disposable S3e.3e runner"]
fn e08_actual_lost_ack_retains_commit_refuses_writes_but_can_keep_reads() {
    let case = Case::new("e08_lost_ack");
    let store = populated(&case);
    cutover(&store).unwrap();
    let before = read(&store).unwrap();
    gate::LOSE_ACK.store(true, Ordering::SeqCst);
    let error = pair(&store, Some("true"), Some(NEW)).unwrap_err();
    assert!(matches!(error, Error::CommitOutcomeUnknown { .. }));
    assert!(store.revision_state.poisoned.load(Ordering::SeqCst));
    assert!(matches!(
        pair(&store, Some("false"), Some(NEXT)),
        Err(Error::ConnectionDiscarded)
    ));
    let readable = read(&store).unwrap();
    assert_eq!(readable.values[MODEL], NEW);
    assert_eq!(readable.revision, before.revision + 1);
    drop(store);
    let healthy = SqliteStore::open(&case.db).unwrap();
    assert_eq!(read(&healthy).unwrap(), readable);
    finish(&case,"known_unsafe_characterization","real commit-before-ack-loss retains values/R and blocks writes, but autocommit poisoned handle remains readable; no full-discard claim",json!({"error":error.to_string(),"committed_R":readable.revision,"read_after_unknown_succeeded":true,"healthy_reopen_same_state":true,"receipt":"none introduced; layout/state inspection is not operation attribution or writer-liveness proof"}));
}

#[test]
#[ignore = "run disposable S3e.3e runner"]
fn e09_corrupt_authority_and_preexisting_unsupported_source_refuse_atomically() {
    for variant in ["both", "missing", "view", "shape", "guard", "null_source"] {
        let case = Case::new(&format!("e09_{variant}"));
        let store = populated(&case);
        let before = read(&store).unwrap();
        {
            let c = store.connection().unwrap();
            match variant {
            "both"=>c.execute_batch("CREATE TABLE probe_m6_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)").unwrap(),
            "missing"=>c.execute_batch("DROP TABLE mind_meta").unwrap(),
            "view"=>c.execute_batch("ALTER TABLE mind_meta RENAME TO fixture_hidden; CREATE VIEW mind_meta AS SELECT * FROM fixture_hidden").unwrap(),
            "shape"=>c.execute_batch("ALTER TABLE mind_meta ADD COLUMN unexpected TEXT").unwrap(),
            "guard"=>c.execute_batch("DROP TRIGGER m6_fence_mind_meta_u").unwrap(),
            "null_source"=>{let tx=begin_write(&c,&store.revision_state).unwrap();tx.execute("INSERT INTO mind_meta VALUES(NULL,'SYNTHETIC_NULL')",[]).unwrap();tx.commit().unwrap();},
            _=>unreachable!(),
        }
        }
        let raw = Connection::open(&case.db).unwrap();
        let r = revision(&raw);
        let legacy_before = objects(&raw, "mind_meta");
        let moved_before = objects(&raw, MOVED);
        assert!(read(&store).is_err());
        assert!(cutover(&store).is_err());
        assert!(pair(&store, Some("true"), Some(NEW)).is_err());
        assert_eq!(revision(&raw), r);
        assert_eq!(objects(&raw, "mind_meta"), legacy_before);
        assert_eq!(objects(&raw, MOVED), moved_before);
        assert_active_zero(&store);
        finish(&case,"regression_invariant","enumerated invalid authorities and unsupported preexisting NULL source refuse without committing cutover or phantom R",json!({"variant":variant,"valid_before_R":before.revision,"corrupt_fixture_R":r,"after_failed_attempt_R":revision(&raw),"scope":"deliberate disposable corruption; schema authenticity/exhaustive corrupted-state validation UNKNOWN"}));
    }
}

#[test]
#[ignore = "run disposable S3e.3e runner"]
fn e10_optional_values_and_cross_transaction_pair_are_not_silently_reinterpreted() {
    let case = Case::new("e10_optional_pair");
    let store = populated(&case);
    cutover(&store).unwrap();
    for (enabled, model) in [
        (None, None),
        (Some("arbitrary-enabled"), Some("")),
        (Some("false"), None),
        (None, Some(OLD)),
    ] {
        pair(&store, enabled, model).unwrap();
        let observation = read(&store).unwrap();
        assert_eq!(observation.values.get(ENABLED).map(String::as_str), enabled);
        assert_eq!(observation.values.get(MODEL).map(String::as_str), model);
    }
    pair(&store, Some("false"), Some(OLD)).unwrap();
    let first = get(&store, ENABLED).unwrap();
    pair(&store, Some("true"), Some(NEW)).unwrap();
    let second = get(&store, MODEL).unwrap();
    assert_eq!(first.as_deref(), Some("false"));
    assert_eq!(second.as_deref(), Some(NEW));
    let coherent = read(&store).unwrap();
    assert_eq!(coherent.values[ENABLED], "true");
    assert_eq!(coherent.values[MODEL], NEW);
    finish(&case,"known_unsafe_characterization","optional raw values are preserved; individually coherent getter calls can span generations, so consumers require one pair/read snapshot",json!({"split_observation":{"enabled":first,"model":second},"pair_reader_coherent":true,"scope":"no invented paired-presence constraint; real manager/runtime/indexer routing remains unintegrated"}));
}

fn old_doctor(case: &Case, label: &str) -> Json {
    let mut command = case.mind(&common::old());
    command.args(["doctor", "--json"]);
    let output = common::run(command, label, case);
    let report: Json = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["format"], "mind-doctor/1");
    assert_eq!(
        output.status.code().map(i64::from),
        report["summary"]["exit_code"].as_i64()
    );
    for marker in [NEW, NEXT] {
        assert!(!String::from_utf8_lossy(&output.stdout).contains(marker));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(marker));
    }
    let checks = report["checks"].as_array().unwrap();
    let database = checks.iter().find(|v| v["id"] == "database").unwrap();
    assert_eq!(database["status"], "fail");
    assert_eq!(database["details"]["schema_version"], 17);
    assert_eq!(database["details"]["supported_schema_version"], 16);
    checks.iter().find(|v| v["id"] == "embeddings").unwrap()["details"]
        .get("selected_model")
        .expect("explicit model field")
        .clone()
}
#[test]
#[ignore = "run disposable S3e.3e runner"]
fn e11_historical_diagnostic_and_actual_ordinary_memory_operations() {
    let case = Case::new("e11_diagnostic_ordinary");
    let store = populated(&case);
    assert_eq!(old_doctor(&case, "legacy-doctor"), OLD);
    let before = plain_rows(&store.connection().unwrap());
    cutover(&store).unwrap();
    pair(&store, Some("true"), Some(NEW)).unwrap();
    assert!(old_doctor(&case, "relocated-doctor").is_null());
    assert_eq!(plain_rows(&store.connection().unwrap()), before);
    let note = case.work.join("second.md");
    std::fs::write(&note, "# More synthetic\n\nm6seamsecondcanary.\n").unwrap();
    let c = store.connection().unwrap();
    let count: i64 = c
        .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))
        .unwrap();
    drop(c);
    crate::ingest::ingest_file(&store, &note, &["notes"]).unwrap();
    assert!(
        store
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get::<_, i64>(0))
            .unwrap()
            > count
    );
    store.verify_fts_index().unwrap();
    finish(&case,"regression_invariant","actual old doctor reports incompatibility and OLD then omits stable NEW; actual ordinary file ingestion/M6/FTS remains functional",json!({"diagnostic":"stable WAL-aware cases only; prior S3e.3d combined DELETE preparation disclosure is NOT fixed or requalified","ordinary_file_ingest_succeeded":true,"active_memory_exclusion":"not implemented; Q10 remains independent"}));
}

#[test]
#[ignore = "run disposable S3e.3e runner"]
fn e12_stale_source_revision_refuses_without_r_then_explicit_refresh_succeeds() {
    let case = Case::new("e12_stale_retry");
    let store = populated(&case);
    let stale = read(&store).unwrap();
    cutover(&store).unwrap();
    pair(&store, Some("true"), Some(NEW)).unwrap();
    let current = read(&store).unwrap();
    assert!(write(&store, &[(MODEL, Some(OLD))], Some(&stale)).is_err());
    assert_eq!(read(&store).unwrap(), current);
    let next = write(&store, &[(MODEL, Some(NEXT))], Some(&current)).unwrap();
    assert_eq!(next, current.revision + 1);
    assert_eq!(get(&store, MODEL).unwrap().as_deref(), Some(NEXT));
    assert_active_zero(&store);
    finish(&case,"regression_invariant","stale layout/source/R precondition refuses and rolls back tentative R; explicit coherent refresh can start a new operation",json!({"stale_R":stale.revision,"fresh_R":current.revision,"committed_R":next,"UNKNOWN_retry":"not attempted; no automatic replay or receipt system"}));
}

#[test]
#[ignore = "run disposable S3e.3e runner"]
fn e13_actual_m6_transaction_does_not_guard_new_a_shaped_tables() {
    let case = Case::new("e13_a_coverage");
    let store = populated(&case);
    let before = read(&store).unwrap();
    {
        let c = store.connection().unwrap();
        let tx = begin_write(&c, &store.revision_state).unwrap();
        tx.execute_batch(
            "CREATE TABLE probe_q_pair(generation INTEGER PRIMARY KEY,enabled TEXT,model TEXT)",
        )
        .unwrap();
        tx.execute("INSERT INTO probe_q_pair VALUES(1,'true',?1)", [NEW])
            .unwrap();
        tx.commit().unwrap();
    }
    let raw = Connection::open(&case.db).unwrap();
    assert_eq!(revision(&raw), before.revision + 1);
    raw.execute(
        "UPDATE probe_q_pair SET model=?1 WHERE generation=1",
        [NEXT],
    )
    .unwrap();
    assert_eq!(revision(&raw), before.revision + 1);
    assert_eq!(
        raw.query_row("SELECT model FROM probe_q_pair", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        NEXT
    );
    validate_fence_inventory(&raw).unwrap(); // existing inventory does not register this new table.
    assert!(read(&store).is_err());
    finish(&case,"known_unsafe_characterization","actual M6 accounting around new A-shaped table does not add missing guard/inventory coverage; raw update mutates it without R",json!({"actual_create_R":before.revision+1,"raw_mutation_R":revision(&raw),"real_inventory_passes":true,"seam_refuses_unsupported_A_authority":true,"scope":"disposable new table, not a production vulnerability or accepted A protocol"}));
}
