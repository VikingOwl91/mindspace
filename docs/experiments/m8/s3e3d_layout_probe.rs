//! S3e.3d child module appended to the immutable S3e.3c test target in an archive.
//! Pre-implementation oracles (adapter success never implies production safety):
//! O1 route every supported key, preserve values, never stale-fallback; production UNKNOWN.
//! O2 one pair read observes one SQLite snapshot, including around concurrent updates.
//! O3 actual old doctor/get/set cannot observe NEW through old names after cutover.
//! O4 a queued actual old setter cannot recreate legacy authority.
//! O5 old held/stepped snapshots may retain wholly OLD, never newly protected data.
//! O6 preserve populated rows, constraints, unrelated metadata and database integrity.
//! O7 inspect actual M6 trigger binding/revision changes; permission equivalence UNKNOWN.
//! O8 interruption before commit recovers prior SQL, not necessarily application-valid state.
//! O9 commit before reply survives restart; missing acknowledgement never means rollback.
//! O10 missing/corrupt/ambiguous authority refuses; no receipt/liveness claim for B.
//! O11 distinguish MAIN/WAL/SHM/journal logical and physical changes, capture fingerprints.
//! A is the unchanged parent fixture (including its known corrupt-receipt replay).
//! B is same-file whole-table ALTER RENAME, no legacy alias, no production routing.
use super::*;
use std::collections::BTreeMap;
use std::time::Instant;

const RELOCATED: &str = "probe_relocated_meta";
type Metadata = BTreeMap<String, String>;

fn objects(conn: &Connection, name: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE name=?1",
        [name],
        |r| r.get(0),
    )
    .unwrap()
}
fn shape(conn: &Connection, name: &str) -> Vec<(String, String, i64, i64)> {
    assert!(matches!(name, "mind_meta" | RELOCATED));
    conn.prepare(&format!("PRAGMA table_info({name})"))
        .unwrap()
        .query_map([], |r| Ok((r.get(1)?, r.get(2)?, r.get(3)?, r.get(5)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
fn b_validate(conn: &Connection) -> ProbeResult<()> {
    if objects(conn, "mind_meta") != 0
        || !table(conn, RELOCATED)
        || objects(conn, "probe_q_state") != 0
    {
        return Err("missing or ambiguous B authority".into());
    }
    if shape(conn, RELOCATED)
        != vec![
            ("key".into(), "TEXT".into(), 0, 1),
            ("value".into(), "TEXT".into(), 1, 0),
        ]
    {
        return Err("unsupported B table shape".into());
    }
    Ok(())
}
fn all(conn: &Connection, name: &str) -> Metadata {
    assert!(matches!(name, "mind_meta" | RELOCATED));
    conn.prepare(&format!("SELECT key,value FROM {name} ORDER BY key"))
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
// Invalid raw/corrupt keys are refused explicitly rather than panicking.
fn try_all(conn: &Connection, name: &str) -> ProbeResult<Metadata> {
    assert!(matches!(name, "mind_meta" | RELOCATED));
    let mut stmt = conn
        .prepare(&format!("SELECT key,value FROM {name} ORDER BY key"))
        .map_err(error)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(error)?;
    rows.collect::<Result<Metadata, _>>().map_err(error)
}
fn b_target(conn: &Connection) -> ProbeResult<&'static str> {
    // Resolved inside the caller's transaction, never a process-wide cache.
    if table(conn, "mind_meta")
        && objects(conn, RELOCATED) == 0
        && objects(conn, "probe_q_state") == 0
    {
        if shape(conn, "mind_meta")
            != vec![
                ("key".into(), "TEXT".into(), 0, 1),
                ("value".into(), "TEXT".into(), 1, 0),
            ]
        {
            return Err("unsupported legacy table shape".into());
        }
        Ok("mind_meta")
    } else {
        b_validate(conn)?;
        Ok(RELOCATED)
    }
}
fn supported_b_read(conn: &Connection) -> ProbeResult<Metadata> {
    try_all(conn, b_target(conn)?)
}
fn supported_b_set(conn: &Connection, key: &str, value: &str) -> ProbeResult<()> {
    // Test-only fixed-UDF revision simulation, not production authorization.
    let tx = begin(conn);
    let target = b_target(&tx)?;
    tx.execute(&format!("INSERT INTO {target}(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value"),params![key,value]).map_err(error)?;
    tx.commit().map_err(error)
}
fn space_rows(conn: &Connection) -> Vec<(String, String, String, i64)> {
    conn.prepare("SELECT id,name,description,created_at FROM spaces ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
fn index_shape(conn: &Connection, target: &str) -> Vec<(i64, String, i64)> {
    // Autoindex name changes with table rename; compare its constraint properties.
    conn.prepare(&format!("PRAGMA index_list({target})"))
        .unwrap()
        .query_map([], |r| Ok((r.get(2)?, r.get(3)?, r.get(4)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
fn fk_shape(conn: &Connection, target: &str) -> Vec<String> {
    conn.prepare(&format!("PRAGMA foreign_key_list({target})"))
        .unwrap()
        .query_map([], |r| r.get::<_, String>(2))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
fn from_metadata(values: &Metadata) -> Pair {
    Pair {
        enabled: values.get(ENABLED).cloned(),
        model: values.get(MODEL).cloned(),
    }
}
fn b_read(conn: &Connection) -> ProbeResult<Metadata> {
    // Caller owns the transaction: discovery and all reads share that snapshot.
    b_validate(conn)?;
    try_all(conn, RELOCATED)
}
fn a_read(conn: &Connection) -> ProbeResult<Metadata> {
    let (_, p) = authoritative(conn)?;
    let mut values = all(conn, "mind_meta");
    for (k, v) in [(ENABLED, p.enabled), (MODEL, p.model)] {
        if let Some(v) = v {
            values.insert(k.into(), v);
        }
    }
    Ok(values)
}
fn b_install(conn: &Connection) -> ProbeResult<()> {
    if !table(conn, "mind_meta")
        || objects(conn, RELOCATED) != 0
        || objects(conn, "probe_q_state") != 0
    {
        return Err("cutover requires unambiguous legacy authority".into());
    }
    conn.execute_batch("ALTER TABLE mind_meta RENAME TO probe_relocated_meta")
        .map_err(error)?;
    b_validate(conn)
}
fn b_prepare(conn: &Connection) -> ProbeResult<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(error)?;
    b_install(&tx)?;
    tx.commit().map_err(error)
}
fn b_set_pair(conn: &Connection, p: &Pair) {
    // Explicitly inherited fixed-UDF M6 SIMULATION on full fixtures, not a permit.
    let tx = begin(conn);
    b_validate(&tx).unwrap();
    for (key, value) in [(ENABLED, &p.enabled), (MODEL, &p.model)] {
        if let Some(value) = value {
            tx.execute("INSERT INTO probe_relocated_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value]).unwrap();
        } else {
            tx.execute("DELETE FROM probe_relocated_meta WHERE key=?1", [key])
                .unwrap();
        }
    }
    tx.commit().unwrap();
}
fn snapshot(conn: &Connection, candidate: &str) -> Metadata {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Deferred).unwrap();
    let result = if candidate == "A" {
        a_read(&tx)
    } else {
        b_read(&tx)
    }
    .unwrap();
    tx.commit().unwrap();
    result
}
fn integrity(conn: &Connection) {
    assert_eq!(
        conn.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(!conn
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .exists([])
        .unwrap());
}
fn revision(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COALESCE(MAX(revision_id),0) FROM m6_revisions",
        [],
        |r| r.get(0),
    )
    .unwrap()
}
fn seed_populated(case: &Case, enabled: Option<&str>, model: Option<&str>) {
    let store = SqliteStore::open(&case.db).unwrap();
    store
        .create_space("fixture-populated", "SYNTHETIC existing nonmetadata row")
        .unwrap();
    for (key, value) in [(ENABLED, enabled), (MODEL, model)] {
        if let Some(v) = value {
            store.set_meta(key, v).unwrap();
        }
    }
    for (key, value) in [
        ("fixture.empty", ""),
        ("fixture.text", "SYNTHETIC_\0_\n_\u{202e}_TEXT"),
        ("git_views:synthetic", "SYNTHETIC_UNRELATED_METADATA"),
    ] {
        store.set_meta(key, value).unwrap();
    }
    for i in 0..32 {
        store
            .set_meta(
                &format!("fixture.key.{i:03}"),
                &format!("SYNTHETIC_VALUE_{i}"),
            )
            .unwrap();
    }
    drop(store);
}

#[test]
#[ignore = "disposable layout spike"]
fn d01_populated_fidelity_constraints_and_missing_production_routing() {
    for candidate in ["A", "B"] {
        for (index, enabled, model) in [
            (0, Some("false"), Some(OLD_VALUE)),
            (1, None, None),
            (2, Some("arbitrary-not-a-boolean"), Some("")),
        ] {
            let case = Case::new(&format!("d01_{candidate}_{index}"));
            seed_populated(&case, enabled, model);
            let conn = Connection::open(&case.db).unwrap();
            let before = all(&conn, "mind_meta");
            let oldshape = shape(&conn, "mind_meta");
            let original_spaces = space_rows(&conn);
            let original_indexes = index_shape(&conn, "mind_meta");
            let original_fks = fk_shape(&conn, "mind_meta");
            case.save("before", &case.fingerprint());
            if candidate == "A" {
                let tx = begin(&conn);
                let copied = all(&tx, "mind_meta");
                install(&tx, &from_metadata(&copied), "copied-source", true).unwrap();
                tx.commit().unwrap();
            } else {
                b_prepare(&conn).unwrap();
            }
            assert_eq!(snapshot(&conn, candidate), before);
            assert_eq!(
                shape(
                    &conn,
                    if candidate == "A" {
                        "mind_meta"
                    } else {
                        RELOCATED
                    }
                ),
                oldshape
            );
            assert_eq!(space_rows(&conn), original_spaces);
            let target = if candidate == "A" {
                "mind_meta"
            } else {
                RELOCATED
            };
            assert_eq!(index_shape(&conn, target), original_indexes);
            assert_eq!(fk_shape(&conn, target), original_fks);
            let tx = begin(&conn);
            let target = if candidate == "A" {
                "mind_meta"
            } else {
                RELOCATED
            };
            assert!(tx
                .execute(
                    &format!("INSERT INTO {target}(key,value) VALUES('fixture.empty','duplicate')"),
                    []
                )
                .is_err());
            assert!(tx
                .execute(
                    &format!("INSERT INTO {target}(key,value) VALUES('fixture.null',NULL)"),
                    []
                )
                .is_err());
            tx.rollback().unwrap();
            integrity(&conn);
            let current = SqliteStore::open(&case.db);
            let routing = match current {
                Ok(store) => {
                    let got = store.get_meta(MODEL);
                    let unrelated = store.get_meta("fixture.empty");
                    let set = store.set_meta(MODEL, "SYNTHETIC_CURRENT_SETTER");
                    let set_unrelated =
                        store.set_meta("fixture.route.control", "SYNTHETIC_CURRENT_UNRELATED");
                    if candidate == "A" {
                        assert_eq!(got.as_ref().unwrap(), &None);
                        assert_eq!(unrelated.as_ref().unwrap().as_deref(), Some(""));
                        assert!(set.is_err());
                        assert!(set_unrelated.is_ok());
                    } else {
                        assert!(
                            got.is_err()
                                && unrelated.is_err()
                                && set.is_err()
                                && set_unrelated.is_err()
                        );
                    }
                    json!({"open":true,"model":format!("{got:?}"),"unrelated":format!("{unrelated:?}"),"setter":format!("{set:?}"),"unrelated_setter":format!("{set_unrelated:?}")})
                }
                Err(e) => panic!("actual Store unexpectedly failed admission: {e}"),
            };
            if candidate == "B" {
                assert_eq!(objects(&conn, "mind_meta"), 0);
            }
            case.save("after", &case.fingerprint());
            case.finish("known_unsafe_characterization","full copied metadata and original constraints survive the fixture, while production Store routing is absent",json!({"candidate":candidate,"copied_values":before,"production_routing":routing,"adapter_fidelity":"PASS","production_integration":"UNKNOWN; native adapter is not Store routing","new_capability_disabling_accepted":false}));
        }
    }
}

struct Historical(ChildGuard);
impl Historical {
    fn start(case: &Case, mode: &str) -> Self {
        let exe = PathBuf::from(std::env::var_os("S3E3D_OLD_ACTOR_BINARY").unwrap());
        let mut command = case.command(&exe);
        command
            .env("S3E3D_ACTOR_DB", &case.db)
            .env("S3E3D_FIXTURE_ROOT", &case.work)
            .env("S3E3D_MODE", mode)
            .args(["--ignored", "--exact", "old_layout_actor", "--nocapture"]);
        let mut actor = Self(ChildGuard::spawn(
            command,
            case.artifacts.join(format!("historical_{mode}.stderr")),
        ));
        assert_eq!(actor.receive()["event"], "ready");
        actor
    }
    fn receive(&mut self) -> Value {
        for _ in 0..12 {
            if let Some(text) = self.0.line().strip_prefix("S3E3D_OLD ") {
                return serde_json::from_str(text).unwrap();
            }
        }
        panic!("historical actor event missing")
    }
    fn call(&mut self, request: Value) -> Value {
        self.0.send(&request.to_string());
        self.receive()
    }
    fn close(mut self) {
        assert_eq!(self.call(json!({"op":"exit"}))["event"], "released");
        assert!(self.0.wait().success());
    }
}
#[test]
#[ignore = "disposable layout spike"]
fn d02_actual_old_store_queued_setter_and_getters_after_b_cutover() {
    let case = Case::new("d02_old_store_queue");
    let mut old = Historical::start(&case, "store");
    for (key, value) in [
        (ENABLED, "false"),
        (MODEL, OLD_VALUE),
        ("fixture.unrelated", "SYNTHETIC_OLD"),
    ] {
        assert_eq!(
            old.call(json!({"op":"set","key":key,"value":value}))["result"]["ok"],
            true
        );
    }
    let mut migrate = case.mind(&common::current());
    migrate.arg("init");
    common::success(common::run(migrate, "migrate17", &case));
    let conn = Connection::open(&case.db).unwrap();
    let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate).unwrap();
    b_install(&tx).unwrap();
    old.0
        .send(&json!({"op":"set","key":MODEL,"value":"SYNTHETIC_QUEUED_WRITER"}).to_string());
    assert!(old.0.try_line(Duration::from_millis(150)).is_none());
    case.save("cutover_held", &case.fingerprint());
    tx.commit().unwrap();
    let queued = old.receive();
    assert_eq!(queued["result"]["ok"], false);
    assert!(queued["result"]["error"]
        .as_str()
        .unwrap()
        .contains("mind_meta"));
    b_set_pair(&conn, &pair(NEW_VALUE));
    let getter = old.call(json!({"op":"get","key":MODEL}));
    assert_eq!(getter["result"]["ok"], false);
    let setter = old.call(json!({"op":"set","key":"fixture.unrelated","value":"SYNTHETIC_NEW"}));
    assert_eq!(setter["result"]["ok"], false);
    assert_eq!(snapshot(&conn, "B")[MODEL], NEW_VALUE);
    assert_eq!(snapshot(&conn, "B")["fixture.unrelated"], "SYNTHETIC_OLD");
    old.close();
    drop(conn);
    let report = doctor(&case, "after-b-cutover");
    assert!(incompatible(&report));
    assert!(selected(&report).is_null());
    case.save("after", &case.fingerprint());
    case.finish("regression_invariant","actual old queued Store setter/getter and unrelated setter fail on removed lookup target; fresh old doctor reports incompatibility and no new marker",json!({"queued":queued,"get":getter,"set":setter,"scope":"old Store opened16 before real migration17; whole old unrelated metadata API also becomes incompatible","timing":"request spans IMMEDIATE, exact blocked instruction not claimed"}));
}
#[test]
#[ignore = "disposable layout spike"]
fn d03_historical_native_prepared_and_old_snapshots_are_distinct() {
    for mode in ["snapshot", "prepared", "stepped"] {
        let case = Case::new(&format!("d03_{mode}"));
        let mut init = case.mind(&common::old());
        init.arg("init");
        common::success(common::run(init, "init16", &case));
        // Set using an actual old Store, then close that process before the held actor.
        let mut setter = Historical::start(&case, "store");
        for (key, value) in [(ENABLED, "false"), (MODEL, OLD_VALUE)] {
            assert_eq!(
                setter.call(json!({"op":"set","key":key,"value":value}))["result"]["ok"],
                true
            );
        }
        setter.close();
        let mut actor = Historical::start(&case, mode);
        let mut migrate = case.mind(&common::current());
        migrate.arg("init");
        common::success(common::run(migrate, "migrate17", &case));
        let conn = Connection::open(&case.db).unwrap();
        b_prepare(&conn).unwrap();
        b_set_pair(&conn, &pair(NEW_VALUE));
        case.save("after_cutover_with_old_handle", &case.fingerprint());
        let observed = actor.call(json!({"op":"release"}));
        if mode == "prepared" {
            assert_eq!(observed["ok"], false);
            assert!(observed["error"].as_str().unwrap().contains("mind_meta"));
        } else {
            assert_eq!(observed["ok"], true);
            assert!(observed["rows"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r[0] == MODEL && r[1] == OLD_VALUE));
        }
        assert!(!observed.to_string().contains(NEW_VALUE));
        let released = actor.receive();
        assert_eq!(released["event"], "released");
        assert!(released["retry_error"]
            .as_str()
            .unwrap()
            .contains("mind_meta"));
        assert!(actor.0.wait().success());
        assert_eq!(snapshot(&conn, "B")[MODEL], NEW_VALUE);
        integrity(&conn);
        case.finish("regression_invariant","historical native prepared first-step fails after rename, while pinned/stepped old snapshots retain wholly OLD values and never NEW",json!({"mode":mode,"observation":observed,"access":"separate native connection in historical-library binary, not private Store cache instrumentation"}));
    }
}
#[test]
#[ignore = "disposable layout spike"]
fn d04_pair_reads_and_unrelated_writes_share_transaction_snapshots() {
    for candidate in ["A", "B"] {
        let case = Case::new(&format!("d04_pair_{candidate}"));
        seed_populated(&case, Some("false"), Some(OLD_VALUE));
        let writer = Connection::open(&case.db).unwrap();
        if candidate == "A" {
            prepare(&writer, &pair(OLD_VALUE), "copy", true);
        } else {
            b_prepare(&writer).unwrap();
        }
        let reader = Connection::open(&case.db).unwrap();
        let tx = Transaction::new_unchecked(&reader, TransactionBehavior::Deferred).unwrap();
        let read = |conn: &Connection| {
            if candidate == "A" {
                a_read(conn)
            } else {
                b_read(conn)
            }
            .unwrap()
        };
        let before = read(&tx);
        let new = Pair {
            enabled: Some("true".into()),
            model: Some(NEW_VALUE.into()),
        };
        if candidate == "A" {
            update(&writer, 1, &new, "changed-pair").unwrap();
        } else {
            b_set_pair(&writer, &new);
        }
        assert_eq!(read(&tx), before);
        tx.commit().unwrap();
        assert_eq!(from_metadata(&snapshot(&reader, candidate)), new);
        let tx = begin(&writer);
        let target = if candidate == "A" {
            "mind_meta"
        } else {
            RELOCATED
        };
        tx.execute(
            &format!(
                "UPDATE {target} SET value='SYNTHETIC_UPDATED_UNRELATED' WHERE key='fixture.empty'"
            ),
            [],
        )
        .unwrap();
        tx.commit().unwrap();
        assert_eq!(
            snapshot(&reader, candidate)["fixture.empty"],
            "SYNTHETIC_UPDATED_UNRELATED"
        );
        case.finish("regression_invariant","fixture pair readers stay on one transaction snapshot and unrelated supported-adapter writes remain available",json!({"candidate":candidate,"old_pair":from_metadata(&before),"new_pair":new,"production_model_cache_indexer_semantics":"UNKNOWN"}));
    }
}

fn b_event(state: &str) {
    println!("\nS3E3D_EVENT {}", json!({"state":state}));
    io::stdout().flush().unwrap();
}
fn b_receive(child: &mut ChildGuard, state: &str) {
    for _ in 0..12 {
        if let Some(text) = child.line().strip_prefix("S3E3D_EVENT ") {
            assert_eq!(serde_json::from_str::<Value>(text).unwrap()["state"], state);
            return;
        }
    }
    panic!("B crash actor event missing");
}
#[test]
#[ignore = "subprocess endpoint, not a scenario"]
fn cutover_actor() {
    let db = PathBuf::from(std::env::var_os("S3E3D_DB").unwrap());
    let root = PathBuf::from(std::env::var_os("S3E3D_ROOT").unwrap())
        .canonicalize()
        .unwrap();
    assert!(db.is_absolute() && db.canonicalize().unwrap().starts_with(root));
    let mode = std::env::var("S3E3D_MODE").unwrap();
    let conn = Connection::open(&db).unwrap();
    conn.busy_timeout(Duration::from_secs(3)).unwrap();
    let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate).unwrap();
    b_install(&tx).unwrap();
    tx.execute(
        "UPDATE probe_relocated_meta SET value=?1 WHERE key=?2",
        params![NEW_VALUE, MODEL],
    )
    .unwrap();
    if mode == "precommit" {
        conn.cache_flush().unwrap();
        b_event("uncommitted_flushed");
        gate();
        tx.rollback().unwrap();
    } else {
        assert_eq!(mode, "postcommit");
        tx.commit().unwrap();
        b_event("committed_before_reply");
        gate();
    }
    drop(conn);
    b_event("released");
}
fn b_actor_start(case: &Case, mode: &str) -> ChildGuard {
    let mut cmd = case.command(&std::env::current_exe().unwrap());
    cmd.env("S3E3D_DB", &case.db)
        .env("S3E3D_ROOT", &case.work)
        .env("S3E3D_MODE", mode)
        .args([
            "--ignored",
            "--exact",
            "layout_cutover::cutover_actor",
            "--nocapture",
        ]);
    ChildGuard::spawn(cmd, case.artifacts.join(format!("b_{mode}.stderr")))
}
#[test]
#[ignore = "disposable layout spike"]
fn d05_b_crashes_recover_prior_sql_or_committed_layout_not_missing_ack_rollback() {
    for journal in ["DELETE", "WAL"] {
        for mode in ["precommit", "postcommit"] {
            let case = Case::new(&format!("d05_{journal}_{mode}"));
            minimal(&case.db, journal, 17);
            case.save("before", &case.fingerprint());
            let mut child = b_actor_start(&case, mode);
            b_receive(
                &mut child,
                if mode == "precommit" {
                    "uncommitted_flushed"
                } else {
                    "committed_before_reply"
                },
            );
            child.kill();
            assert!(!child.wait().success());
            case.save("crash", &case.fingerprint());
            let mut immutable_observation = Value::Null;
            if journal == "DELETE" && mode == "precommit" {
                let journal_bytes = fs::read(format!("{}-journal", case.db.display())).unwrap();
                assert!(journal_bytes.len() > 512);
                assert_eq!(
                    &journal_bytes[..8],
                    &[0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7]
                );
                let uri = format!("file:{}?immutable=1", case.db.display());
                let immutable = Connection::open_with_flags(
                    uri,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                        | rusqlite::OpenFlags::SQLITE_OPEN_URI,
                )
                .unwrap();
                let relocated_visible = table(&immutable, RELOCATED);
                let legacy_visible = table(&immutable, "mind_meta");
                let values = if relocated_visible {
                    try_all(&immutable, RELOCATED)
                } else if legacy_visible {
                    try_all(&immutable, "mind_meta")
                } else {
                    Err("neither layout visible in MAIN".into())
                };
                immutable_observation = json!({"relocated_visible":relocated_visible,"legacy_visible":legacy_visible,"values":format!("{values:?}"),"scope":"raw immutable inspection of uncommitted MAIN; cache_flush is not a guarantee that DDL pages are persisted"});
                drop(immutable);
                // Deliberate unsafe characterization, NOT the normal no-leak doctor helper.
                // OLD MAIN schema can address the renamed table's flushed NEW value page.
                let mut command = case.mind(&common::old());
                command.args(["doctor", "--json"]);
                let output = common::run(command, "before-delete-recovery", &case);
                let report: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(report["format"], "mind-doctor/1");
                assert_eq!(
                    output.status.code().map(i64::from),
                    report["summary"]["exit_code"].as_i64()
                );
                assert!(incompatible(&report));
                assert_eq!(selected(&report), NEW_VALUE);
                assert!(String::from_utf8_lossy(&output.stdout).contains(NEW_VALUE));
                assert!(!String::from_utf8_lossy(&output.stdout).contains(NEXT_VALUE));
                immutable_observation["old_doctor_uncommitted_NEW_disclosure"] = json!(true);
                immutable_observation["isolation_oracle"] = json!("FAIL: combined rename plus NEW-value preparation leaks via old immutable diagnostic");
                case.save("before-recovery-diagnostic-files", &case.fingerprint());
            }
            let recovered = Connection::open(&case.db).unwrap();
            // A SQL read actually triggers recovery; merely opening does not establish it.
            let max: i64 = recovered
                .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(max, 17);
            if mode == "precommit" {
                assert!(!table(&recovered, RELOCATED));
                assert_eq!(legacy(&recovered), pair(OLD_VALUE));
            } else {
                assert_eq!(snapshot(&recovered, "B")[MODEL], NEW_VALUE);
                assert_eq!(objects(&recovered, "mind_meta"), 0);
                assert!(b_prepare(&recovered).is_err());
            }
            integrity(&recovered);
            drop(recovered);
            let report = doctor(&case, "after-recovery");
            if mode == "precommit" {
                assert_eq!(selected(&report), OLD_VALUE);
            } else {
                assert!(selected(&report).is_null());
            }
            case.save("recovered", &case.fingerprint());
            case.finish(if mode=="precommit"{"known_unsafe_characterization"}else{"regression_invariant"},"controlled precommit crash restores prior SQL/legacy disclosure, postcommit crash retains new B state despite missing normal reply",json!({"journal":journal,"kill_point":mode,"immutable_observation":immutable_observation,"receipt":"B has no operation receipt; layout discovery alone is not liveness or arbitrary operation replay","scope":"process crash after explicit control event; combined rename plus NEW-value preparation intentionally violates non-disclosure in DELETE mode; not pure value-preserving rename or power-loss qualification"}));
        }
    }
}
#[test]
#[ignore = "disposable layout spike"]
fn d06_b_wal_main_and_readonly_diagnostic_shm_side_effects() {
    let case = Case::new("d06_b_wal_shm");
    minimal(&case.db, "WAL", 17);
    let mut held = actor(&case, "wal_hold");
    receive(&mut held, "connection_held");
    let writer = Connection::open(&case.db).unwrap();
    writer.execute_batch("PRAGMA wal_autocheckpoint=0").unwrap();
    case.save("before", &case.fingerprint());
    b_prepare(&writer).unwrap();
    b_set_pair(&writer, &pair(NEW_VALUE));
    assert_eq!(snapshot(&writer, "B")[MODEL], NEW_VALUE);
    let uri = format!("file:{}?immutable=1", case.db.display());
    let immutable = Connection::open_with_flags(
        uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap();
    assert_eq!(legacy(&immutable), pair(OLD_VALUE));
    assert!(!table(&immutable, RELOCATED));
    drop(immutable);
    let report = doctor(&case, "wal-aware-b-doctor");
    assert!(selected(&report).is_null());
    drop(writer);
    held.kill();
    assert!(!held.wait().success());
    let shm = PathBuf::from(format!("{}-shm", case.db.display()));
    assert!(shm.exists());
    fs::remove_file(shm).unwrap();
    let before = case.fingerprint();
    case.save("before_missing_shm", &before);
    let report = doctor(&case, "b-doctor-recreates-shm");
    assert!(selected(&report).is_null());
    let after = case.fingerprint();
    assert_eq!(after["-shm"]["exists"], true);
    assert_eq!(before["main"]["sha256"], after["main"]["sha256"]);
    assert_eq!(before["-wal"]["sha256"], after["-wal"]["sha256"]);
    let conn = Connection::open(&case.db).unwrap();
    assert_eq!(snapshot(&conn, "B")[MODEL], NEW_VALUE);
    case.finish("known_unsafe_characterization","B logical WAL cutover does not rewrite MAIN; old read-only doctor recreates SHM without MAIN/WAL data writes",json!({"immutable_MAIN_model":OLD_VALUE,"before":before,"after":after,"permission_to_delete_live_shm":false}));
}
#[test]
#[ignore = "disposable layout spike"]
fn d07_b_corrupt_ambiguous_layouts_and_rejected_cutovers_refuse() {
    for variant in [
        "both",
        "missing",
        "badshape",
        "alias",
        "candidateA",
        "nullkey",
    ] {
        let case = Case::new(&format!("d07_{variant}"));
        minimal(&case.db, "WAL", 17);
        let conn = Connection::open(&case.db).unwrap();
        b_prepare(&conn).unwrap();
        match variant {
            "both" => conn
                .execute_batch("CREATE TABLE mind_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
                .unwrap(),
            "missing" => conn
                .execute_batch("DROP TABLE probe_relocated_meta")
                .unwrap(),
            "badshape" => conn
                .execute_batch("ALTER TABLE probe_relocated_meta ADD COLUMN ambiguous TEXT")
                .unwrap(),
            "alias" => conn
                .execute_batch("CREATE VIEW mind_meta AS SELECT * FROM probe_relocated_meta")
                .unwrap(),
            "nullkey" => conn
                .execute_batch(
                    "INSERT INTO probe_relocated_meta(key,value) VALUES(NULL,'SYNTHETIC_NULL_KEY')",
                )
                .unwrap(),
            "candidateA" => conn
                .execute_batch("CREATE TABLE probe_q_state(singleton INTEGER)")
                .unwrap(),
            _ => unreachable!(),
        }
        let result = b_read(&conn);
        assert!(result.is_err());
        assert!(b_prepare(&conn).is_err());
        case.finish("regression_invariant","tested ambiguous/missing/unsupported B layouts refuse with no stale fallback",json!({"corruption":variant,"error":result.unwrap_err(),"complete_corruption_authentication":"UNKNOWN; shape checks and hashes are not hostile-account authenticity"}));
    }
    let case = Case::new("d07_rejected_atomic_cutover");
    minimal(&case.db, "WAL", 16);
    let conn = Connection::open(&case.db).unwrap();
    {
        let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate).unwrap();
        b_install(&tx).unwrap();
        assert!(tx
            .execute_batch("CREATE TABLE probe_relocated_meta(invalid TEXT)")
            .is_err());
        tx.rollback().unwrap();
    }
    assert_eq!(legacy(&conn), pair(OLD_VALUE));
    assert!(!table(&conn, RELOCATED));
    let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate).unwrap();
    b_install(&tx).unwrap();
    tx.execute("INSERT INTO schema_migrations VALUES(17)", [])
        .unwrap();
    tx.commit().unwrap();
    assert_eq!(
        conn.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        17
    );
    assert_eq!(objects(&conn, "mind_meta"), 0);
    assert_eq!(snapshot(&conn, "B")[MODEL], OLD_VALUE);
    drop(conn);
    let report = doctor(&case, "joint-floor-layout");
    assert!(incompatible(&report));
    assert!(selected(&report).is_null());
    case.finish("regression_invariant","rejected cutover rolls back; fixture floor and whole-table rename commit atomically in one minimal SQL transaction",json!({"floor":17,"production_migration":"not provided; sentinel fixture version only","new_supported_floor_boundary":"UNKNOWN"}));
}
#[test]
#[ignore = "disposable layout spike"]
fn d08_m6_guards_follow_but_ddl_has_no_storage_owned_revision() {
    for candidate in ["A", "B"] {
        let case = Case::new(&format!("d08_M6_{candidate}"));
        seed_populated(&case, Some("false"), Some(OLD_VALUE));
        let store = SqliteStore::open(&case.db).unwrap();
        let conn = Connection::open(&case.db).unwrap();
        let before_r = revision(&conn);
        let spaces_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM spaces", [], |r| r.get(0))
            .unwrap();
        let metadata_before = all(&conn, "mind_meta");
        case.save("before", &case.fingerprint());
        if candidate == "A" {
            prepare(&conn, &pair(OLD_VALUE), "simulated-prepare", true);
        } else {
            b_prepare(&conn).unwrap();
        }
        let after_cutover = revision(&conn);
        assert_eq!(
            after_cutover - before_r,
            if candidate == "A" { 1 } else { 0 }
        );
        let target = if candidate == "A" {
            "mind_meta"
        } else {
            RELOCATED
        };
        let bindings:Vec<(String,String,String)>=conn.prepare("SELECT name,tbl_name,sql FROM sqlite_schema WHERE type='trigger' AND name IN ('m6_fence_mind_meta_i','m6_fence_mind_meta_u','m6_fence_mind_meta_d') ORDER BY name").unwrap().query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap().collect::<Result<_,_>>().unwrap();
        assert_eq!(bindings.len(), 3);
        assert!(bindings
            .iter()
            .all(|(_, table, sql)| table == target && sql.contains("m6_write_revision()")));
        // New connection has no fake m6_write_revision: real retained guard rejects.
        let unprivileged = Connection::open(&case.db).unwrap();
        let rejected = unprivileged
            .execute(
                &format!(
                    "INSERT INTO {target}(key,value) VALUES('fixture.unrevisioned','SYNTHETIC')"
                ),
                [],
            )
            .unwrap_err()
            .to_string();
        assert!(rejected.contains("m6_write_revision"));
        drop(unprivileged);
        assert_eq!(snapshot(&conn, candidate), metadata_before);
        let space = store
            .create_space(
                &format!("fixture-after-{candidate}"),
                "SYNTHETIC ordinary Store write positive control",
            )
            .unwrap();
        assert_eq!(revision(&conn), after_cutover + 1);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM spaces", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            spaces_before + 1
        );
        let current_model = store.get_meta(MODEL);
        if candidate == "A" {
            assert!(current_model.unwrap().is_none());
        } else {
            assert!(current_model.is_err());
        }
        integrity(&conn);
        case.save("after", &case.fingerprint());
        case.finish("known_unsafe_characterization","M6 metadata guards remain bound and reject unrevisioned DML, but native B DDL allocates no R; production routed metadata permission/accounting equivalence is unproved",json!({"candidate":candidate,"before_R":before_r,"after_cutover_R":after_cutover,"after_real_Store_write_R":revision(&conn),"retained_guards":bindings,"unrevisioned_rejected":rejected,"real_Store_unrelated_space":format!("{:?}",space),"M6_integration":"UNKNOWN; A uses fixed-UDF simulation, B DDL bypasses DML accounting; neither is a private administrative permit"}));
    }
}
#[test]
#[ignore = "disposable layout spike"]
fn d09_adapter_read_cost_is_a_microbenchmark_not_a_selection_oracle() {
    let mut timings = Vec::new();
    for candidate in ["A", "B"] {
        let case = Case::new(&format!("d09_cost_{candidate}"));
        seed_populated(&case, Some("false"), Some(OLD_VALUE));
        let conn = Connection::open(&case.db).unwrap();
        let start = Instant::now();
        if candidate == "A" {
            prepare(&conn, &pair(OLD_VALUE), "prepare", true);
        } else {
            b_prepare(&conn).unwrap();
        }
        let cutover_ns = start.elapsed().as_nanos();
        let start = Instant::now();
        for _ in 0..100 {
            assert_eq!(snapshot(&conn, candidate)[MODEL], OLD_VALUE);
        }
        let reads_ns = start.elapsed().as_nanos();
        timings.push(json!({"candidate":candidate,"cutover_ns":cutover_ns,"read_iterations":100,"reads_ns":reads_ns}));
        case.finish("regression_invariant","100 completed validated adapter reads return expected values; timings are descriptive only",json!({"candidate":candidate,"cutover_ns":cutover_ns,"read_iterations":100,"reads_ns":reads_ns,"not_measured":"production routing, caches, indexer, remote storage, throughput or tail latency"}));
    }
    println!("S3E3D_TIMINGS {}", json!(timings));
}

#[test]
#[ignore = "disposable layout spike"]
fn d10_b_supported_adapter_resolves_layout_in_each_transaction() {
    let case = Case::new("d10_dual_layout");
    seed_populated(&case, Some("false"), Some(OLD_VALUE));
    let writer = Connection::open(&case.db).unwrap();
    supported_b_set(&writer, "fixture.adapter", "SYNTHETIC_BEFORE").unwrap();
    let reader = Connection::open(&case.db).unwrap();
    let held = Transaction::new_unchecked(&reader, TransactionBehavior::Deferred).unwrap();
    let old = supported_b_read(&held).unwrap();
    assert_eq!(old[MODEL], OLD_VALUE);
    b_prepare(&writer).unwrap();
    b_set_pair(
        &writer,
        &Pair {
            enabled: Some("true".into()),
            model: Some(NEW_VALUE.into()),
        },
    );
    supported_b_set(&writer, "fixture.adapter", "SYNTHETIC_AFTER").unwrap();
    assert_eq!(supported_b_read(&held).unwrap(), old);
    held.commit().unwrap();
    let fresh = Transaction::new_unchecked(&reader, TransactionBehavior::Deferred).unwrap();
    let after = supported_b_read(&fresh).unwrap();
    assert_eq!(after[ENABLED], "true");
    assert_eq!(after[MODEL], NEW_VALUE);
    assert_eq!(after["fixture.adapter"], "SYNTHETIC_AFTER");
    fresh.commit().unwrap();
    case.save("after", &case.fingerprint());
    case.finish("regression_invariant","transaction-bound dual-layout fixture reads stay wholly OLD across cutover, then resolve new authority with functioning unrelated writes",json!({"old":old,"after":after,"production_equivalence":"UNKNOWN; native simulated revision context, no real semantic capability/cache/indexer integration"}));
}

#[test]
#[ignore = "disposable layout spike"]
fn d11_b_drain_requires_old_connection_release_and_is_only_a_witness() {
    let case = Case::new("d11_b_drain");
    let mut init = case.mind(&common::old());
    init.arg("init");
    common::success(common::run(init, "init16", &case));
    let mut setter = Historical::start(&case, "store");
    for (key, value) in [(ENABLED, "false"), (MODEL, OLD_VALUE)] {
        assert_eq!(
            setter.call(json!({"op":"set","key":key,"value":value}))["result"]["ok"],
            true
        );
    }
    setter.close();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&case.db)
        .unwrap();
    if let Err(e) = lease(&file, libc::F_WRLCK) {
        let errno = e.raw_os_error().unwrap();
        if [
            libc::EINVAL,
            libc::ENOSYS,
            libc::EOPNOTSUPP,
            libc::EPERM,
            libc::EACCES,
        ]
        .contains(&errno)
        {
            case.skip(&format!("B baseline lease unavailable: {e} errno {errno}"));
            return;
        }
        panic!("B clean baseline lease failed: {e}");
    }
    lease(&file, libc::F_UNLCK).unwrap();
    let mut old = Historical::start(&case, "snapshot");
    let mut migrate = case.mind(&common::current());
    migrate.arg("init");
    common::success(common::run(migrate, "migrate17", &case));
    let conn = Connection::open(&case.db).unwrap();
    b_prepare(&conn).unwrap();
    b_set_pair(&conn, &pair(NEW_VALUE));
    drop(conn);
    assert_eq!(
        lease(&file, libc::F_WRLCK).unwrap_err().raw_os_error(),
        Some(libc::EAGAIN)
    );
    let observed = old.call(json!({"op":"release"}));
    assert_eq!(observed["ok"], true);
    assert_eq!(old.receive()["event"], "released");
    assert!(old.0.wait().success());
    lease(&file, libc::F_WRLCK).unwrap();
    lease(&file, libc::F_UNLCK).unwrap();
    drop(file);
    let raw = Connection::open(&case.db).unwrap();
    assert_eq!(snapshot(&raw, "B")[MODEL], NEW_VALUE);
    case.save("after", &case.fingerprint());
    case.finish("known_unsafe_characterization","B cutover does not close a held old process; release permits exclusive inode witness, then raw same-UID SQL still reads protected metadata",json!({"held_errno":libc::EAGAIN,"retry_after_close":true,"raw_model":NEW_VALUE,"boundary":"raw same-UID SQL is outside Model A admission exclusion; no future admission or handoff guarantee"}));
}

#[test]
#[ignore = "disposable layout spike"]
fn d12_b_naive_stale_adapter_write_has_no_generation_precondition() {
    for candidate in ["A", "B"] {
        let case = Case::new(&format!("d12_stale_{candidate}"));
        minimal(&case.db, "WAL", 17);
        let conn = Connection::open(&case.db).unwrap();
        if candidate == "A" {
            prepare(&conn, &pair(OLD_VALUE), "prepare", true);
        } else {
            b_prepare(&conn).unwrap();
        }
        let stale = from_metadata(&snapshot(&conn, candidate));
        if candidate == "A" {
            update(&conn, 1, &pair(NEW_VALUE), "fresh").unwrap();
            assert!(update(&conn, 1, &stale, "stale").is_err());
            assert_eq!(snapshot(&conn, candidate)[MODEL], NEW_VALUE);
        } else {
            b_set_pair(&conn, &pair(NEW_VALUE));
            b_set_pair(&conn, &stale);
            assert_eq!(snapshot(&conn, candidate)[MODEL], OLD_VALUE);
        }
        case.finish(if candidate=="A"{"regression_invariant"}else{"known_unsafe_characterization"},"A fixture CAS rejects stale generation, while naive B adapter last-writer-wins overwrites newer settings; B needs a separately proved precondition if that guarantee is required",json!({"candidate":candidate,"stale_overwrite":candidate=="B","scope":"test adapter behavior, not newly discovered production vulnerability; custom generation tables not proved indispensable"}));
    }
}
