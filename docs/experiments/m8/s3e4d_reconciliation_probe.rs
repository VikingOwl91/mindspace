// Baseline-only characterization of sequential reconciliation before a duplicate
// observation fails. These are not an implementation of pre-effect planning.
#[derive(Debug, Clone, serde::Serialize)]
struct S3e4dProbeEpisode {
    id: String,
    assertion: String,
    content: String,
    state: String,
    created_at: i64,
    superseded_by: Option<String>,
    memberships: Vec<String>,
}

fn s3e4d_probe_rows(conn: &Connection) -> Result<Vec<S3e4dProbeEpisode>> {
    let mut stmt = conn.prepare("SELECT m.id,m.assertion_id,a.content,m.state,m.created_at,m.superseded_by FROM memories m JOIN assertions a ON a.id=m.assertion_id ORDER BY m.created_at,m.id").map_err(dberr)?;
    let mut rows = stmt
        .query_map([], |r| {
            Ok(S3e4dProbeEpisode {
                id: r.get(0)?,
                assertion: r.get(1)?,
                content: r.get(2)?,
                state: r.get(3)?,
                created_at: r.get(4)?,
                superseded_by: r.get(5)?,
                memberships: Vec::new(),
            })
        })
        .map_err(dberr)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(dberr)?;
    drop(stmt);
    for row in &mut rows {
        let mut stmt = conn
            .prepare("SELECT space_id FROM memory_spaces WHERE memory_id=?1 ORDER BY space_id")
            .map_err(dberr)?;
        row.memberships = stmt
            .query_map([&row.id], |r| r.get(0))
            .map_err(dberr)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(dberr)?;
    }
    Ok(rows)
}

fn s3e4d_probe_file(first: &str, spaces: &[Uuid]) -> ObservedFile {
    let file = crate::ingest::observe_bytes(
        Path::new("a.md"),
        format!("# Same\n\n{first}\n\n# Same\n\nSYNTHETIC_KEEP\n").into_bytes(),
        format!("{ROOT}/a.md"),
        spaces,
    )
    .unwrap();
    assert_eq!(file.assertions.len(), 2, "fixture needs two sections");
    assert_eq!(
        file.assertions[0].subject_normalized,
        file.assertions[1].subject_normalized
    );
    assert_ne!(file.assertions[0].content, file.assertions[1].content);
    file
}

fn s3e4d_probe_observation(files: Vec<ObservedFile>, spaces: &[Uuid]) -> IngestObservation {
    IngestObservation {
        session: SessionDraft {
            root: ROOT.into(),
            scope: IngestScope::File,
            observed_at: Ts(100),
        },
        files,
        spaces: spaces.to_vec(),
    }
}

fn s3e4d_probe_save(name: &str, result: serde_json::Value) {
    let root = std::env::var_os("MINDSPACE_S3E4D_PROBE_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/s3e4d-proof"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join(format!("{name}.json")),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    println!(
        "S3E4D_BASELINE_PROBE {}",
        serde_json::to_string(&result).unwrap()
    );
}

fn s3e4d_probe_uuid_case(high: bool) {
    let w = World::new();
    let seed_file = s3e4d_probe_file("SYNTHETIC_OLD", &[w.notes]);
    let seed = w
        .store
        .record_observation(s3e4d_probe_observation(vec![seed_file.clone()], &[w.notes]))
        .unwrap();
    let logical = seed.files[0].logical_id;
    let initial = s3e4d_probe_rows(&w.store.connection().unwrap()).unwrap();
    assert_eq!(initial.len(), 2);
    for m in &initial {
        w.store
            .retire_memory(
                Uuid::parse_str(&m.id).unwrap(),
                Some("synthetic fixture replacement".into()),
                Ts(200),
            )
            .unwrap();
    }
    let ids = if high {
        [
            "ffffffff-ffff-7fff-bfff-ffffffffff01",
            "ffffffff-ffff-7fff-bfff-ffffffffff02",
        ]
    } else {
        [
            "00000000-0000-7000-8000-000000000001",
            "00000000-0000-7000-8000-000000000002",
        ]
    };
    {
        let c = w.store.connection().unwrap();
        let tx = begin_write(&c, &w.store.revision_state).unwrap();
        for (i, id) in ids.iter().enumerate() {
            let assertion = initial
                .iter()
                .find(|m| m.content == seed_file.assertions[i].content)
                .unwrap()
                .assertion
                .clone();
            tx.execute("INSERT INTO memories(id,assertion_id,state,created_at,producer_family) VALUES(?1,?2,'active',100,?3)",rusqlite::params![id,assertion,FILE_PRODUCER_FAMILY]).unwrap();
            tx.execute(
                "INSERT INTO memory_spaces(memory_id,space_id,created_at) VALUES(?1,?2,100)",
                rusqlite::params![id, w.notes.to_string()],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    let changed = s3e4d_probe_file("SYNTHETIC_NEW", &[w.notes]);
    let before = ledger(&w.store);
    let before_rows = dump(&w.store.connection().unwrap(), true);
    let snapshots = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&snapshots);
    let outcome = with_hook(
        move |phase, conn| {
            if phase == "after_reconcile" {
                captured.lock().unwrap().push(s3e4d_probe_rows(conn)?);
            }
            Ok(())
        },
        || {
            w.store.record_observation(s3e4d_probe_observation(
                vec![changed.clone(), changed.clone()],
                &[w.notes],
            ))
        },
    );
    let error = format!("{outcome:?}");
    let snapshots = snapshots.lock().unwrap().clone();
    let after = ledger(&w.store);
    let after_rows = dump(&w.store.connection().unwrap(), true);
    let autocommit = w.store.connection().unwrap().is_autocommit();
    let fixture = w._dir.keep();
    s3e4d_probe_save(
        if high {
            "high_uuid_order"
        } else {
            "low_uuid_order"
        },
        serde_json::json!({
            "baseline":"3f7fd3422047b40f9dc6fa787d4b523d7d90d857",
            "category":"baseline characterization, not a production defect or new planner",
            "high_controlled_ids":high,"controlled_ids":ids,"logical":logical.to_string(),
            "created_at":100,"observed_at":100,"snapshots":snapshots,
            "actual_error":error,"ledger_before":before,"ledger_after":after,
            "all_rows_rolled_back":before_rows==after_rows,"autocommit_after":autocommit,
            "retained_fixture":fixture.to_string_lossy(),
        }),
    );
    if high {
        assert!(error.contains("UNIQUE constraint failed: source_observations.session_id, source_observations.location"),"{error}");
    } else {
        // Its second reconciliation attempts to create an episode for the
        // assertion already active in `fresh`, before the later supersession.
        // The existing active-per-assertion UNIQUE therefore fails first.
        assert!(
            error.contains("UNIQUE constraint failed: memories.assertion_id"),
            "{error}"
        );
    }
    assert_eq!(before, after);
    assert_eq!(before_rows, after_rows);
    assert!(autocommit);
    assert_eq!(snapshots.len(), if high { 2 } else { 1 });
    let first_active = snapshots[0]
        .iter()
        .filter(|m| m.state == "active")
        .collect::<Vec<_>>();
    assert_eq!(first_active.len(), 2);
    let fresh = first_active.iter().find(|m| m.id != ids[1]).unwrap();
    assert_eq!(fresh.content, changed.assertions[0].content);
    assert!(fresh.id.as_str() > "00000000-0000-7000-8000-000000000002");
    assert!(fresh.id.as_str() < "ffffffff-ffff-7fff-bfff-ffffffffff01");
    if high {
        let second_active = snapshots[1]
            .iter()
            .filter(|m| m.state == "active")
            .collect::<Vec<_>>();
        assert_eq!(second_active.len(), 2);
        assert_eq!(first_active[0].id, fresh.id);
        assert_eq!(first_active[1].id, ids[1]);
        assert_eq!(snapshots[1].len(), snapshots[0].len());
        assert_eq!(
            first_active.iter().map(|m| &m.id).collect::<Vec<_>>(),
            second_active.iter().map(|m| &m.id).collect::<Vec<_>>()
        );
    } else {
        assert_eq!(first_active[0].id, ids[1]);
        assert_eq!(first_active[1].id, fresh.id);
        // The second reconciliation did not reach its completion checkpoint.
        assert_eq!(snapshots.len(), 1);
    }
    assert_integrity(&w.store);
}

#[test]
fn s3e4d_baseline_low_uuid_tie_changes_the_second_reconciliation() {
    s3e4d_probe_uuid_case(false)
}

#[test]
fn s3e4d_baseline_high_uuid_tie_retains_the_first_reconciliation() {
    s3e4d_probe_uuid_case(true)
}

#[test]
fn s3e4d_baseline_duplicate_existing_location_reuses_pending_membership() {
    let w = World::new();
    w.record(
        IngestScope::File,
        &[("a.md", "SYNTHETIC_MEMBERSHIP")],
        &[w.notes],
        100,
    );
    let before = ledger(&w.store);
    let before_rows = dump(&w.store.connection().unwrap(), true);
    let snapshots = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&snapshots);
    let outcome = with_hook(
        move |phase, conn| {
            if phase == "after_reconcile" {
                captured.lock().unwrap().push(s3e4d_probe_rows(conn)?);
            }
            Ok(())
        },
        || {
            w.store.record_observation(w.observation(
                IngestScope::File,
                &[
                    ("a.md", "SYNTHETIC_MEMBERSHIP"),
                    ("a.md", "SYNTHETIC_MEMBERSHIP"),
                ],
                &[w.notes, w.other, w.other],
                100,
            ))
        },
    );
    let error = format!("{outcome:?}");
    let snapshots = snapshots.lock().unwrap().clone();
    let after = ledger(&w.store);
    let after_rows = dump(&w.store.connection().unwrap(), true);
    let autocommit = w.store.connection().unwrap().is_autocommit();
    let fixture = w._dir.keep();
    s3e4d_probe_save(
        "pending_membership",
        serde_json::json!({
            "baseline":"3f7fd3422047b40f9dc6fa787d4b523d7d90d857",
            "category":"baseline sequential membership characterization",
            "snapshots":snapshots,"actual_error":error,"ledger_before":before,"ledger_after":after,
            "all_rows_rolled_back":before_rows==after_rows,"autocommit_after":autocommit,
            "retained_fixture":fixture.to_string_lossy(),
        }),
    );
    assert!(error.contains("UNIQUE constraint failed: source_observations.session_id, source_observations.location"),"{error}");
    assert_eq!(before, after);
    assert_eq!(before_rows, after_rows);
    assert!(autocommit);
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0].len(), 1);
    assert_eq!(snapshots[1].len(), 1);
    assert_eq!(snapshots[0][0].id, snapshots[1][0].id);
    assert_eq!(snapshots[0][0].memberships.len(), 2);
    assert_eq!(snapshots[0][0].memberships, snapshots[1][0].memberships);
    assert_integrity(&w.store);
}

// Public-byte observability control for the locked uuid 1.26.0 implementation.
// This does not claim its packed counter slice is a documented stable API.
fn s3e4d_public_uuid_slice(id: Uuid) -> u64 {
    ((id.as_u128() >> 36) & ((1u128 << 26) - 1)) as u64
}

fn s3e4d_public_uuid_gap(before: Uuid, after: Uuid) -> u64 {
    s3e4d_public_uuid_slice(after).wrapping_sub(s3e4d_public_uuid_slice(before))
        & ((1u64 << 26) - 1)
}

#[test]
fn s3e4d_uuid_observability_fixed_clock() {
    let seconds = std::env::var("MINDSPACE_S3E4D_FIXED_REALTIME_SECONDS")
        .expect("run only this exact test with the fixed-CLOCK_REALTIME shim")
        .parse::<u64>()
        .unwrap();
    let nanos = std::env::var("MINDSPACE_S3E4D_FIXED_REALTIME_NANOS")
        .unwrap_or_else(|_| "0".into())
        .parse::<u64>()
        .unwrap();
    let fixed_ms = seconds * 1000 + nanos / 1_000_000;
    let w = World::new();
    w.record(
        IngestScope::File,
        &[("a.md", "SYNTHETIC_COUNTER_SEED")],
        &[w.notes],
        100,
    );
    let adjacent_before = Uuid::now_v7();
    let adjacent_after = Uuid::now_v7();
    let same_ms = |id: Uuid| (id.as_u128() >> 80) as u64 == fixed_ms;
    assert!(
        same_ms(adjacent_before) && same_ms(adjacent_after),
        "CLOCK_REALTIME shim did not fix the UUID millisecond"
    );
    assert_eq!(
        s3e4d_public_uuid_gap(adjacent_before, adjacent_after),
        1,
        "validate public-bit slice before using it as an oracle"
    );
    let ledger_before = ledger(&w.store);
    let rows_before = dump(&w.store.connection().unwrap(), true);
    let no_effect_before = Uuid::now_v7();
    {
        let c = w.store.connection().unwrap();
        let tx = begin_write(&c, &w.store.revision_state).unwrap();
        drop(tx);
    }
    let no_effect_after = Uuid::now_v7();
    let no_effect_gap = s3e4d_public_uuid_gap(no_effect_before, no_effect_after);
    assert!(same_ms(no_effect_before) && same_ms(no_effect_after));
    assert_eq!(no_effect_gap, 1);
    assert_eq!(ledger(&w.store), ledger_before);
    assert_eq!(dump(&w.store.connection().unwrap(), true), rows_before);
    let duplicate = w.observation(
        IngestScope::File,
        &[
            ("a.md", "SYNTHETIC_COUNTER_CHANGED"),
            ("a.md", "SYNTHETIC_COUNTER_CHANGED"),
        ],
        &[w.notes, w.other],
        200,
    );
    let failed_before = Uuid::now_v7();
    let outcome = w.store.record_observation(duplicate);
    let failed_after = Uuid::now_v7();
    let failed_gap = s3e4d_public_uuid_gap(failed_before, failed_after);
    let error = format!("{outcome:?}");
    let ledger_after_failure = ledger(&w.store);
    let rows_after_failure = dump(&w.store.connection().unwrap(), true);
    let autocommit = w.store.connection().unwrap().is_autocommit();
    assert!(same_ms(failed_before) && same_ms(failed_after));
    // A subsequent public successful operation returns IDs in this same stream.
    let next_input = w.observation(
        IngestScope::File,
        &[("b.md", "SYNTHETIC_NEXT_SUCCESS")],
        &[w.notes],
        300,
    );
    let next = w.store.record_observation(next_input).unwrap();
    let after_next = Uuid::now_v7();
    let next_memory = next.files[0].actions[0].memory_id;
    let final_ledger = ledger(&w.store);
    let fixture = w._dir.keep();
    s3e4d_probe_save(
        "uuid_observability_fixed_clock",
        serde_json::json!({
            "baseline":"3f7fd3422047b40f9dc6fa787d4b523d7d90d857",
            "category":"observable serialized UUID bytes after rollback, not a documented counter-gap contract or security defect",
            "locked_uuid_version":"1.26.0","fixed_unix_millis":fixed_ms,
            "decoded_slice":"low 26 counter bits: (uuid.as_u128() >> 36) & ((1<<26)-1); modular gap",
            "adjacent_before":adjacent_before.to_string(),"adjacent_after":adjacent_after.to_string(),
            "adjacent_gap":s3e4d_public_uuid_gap(adjacent_before,adjacent_after),
            "no_effect_before":no_effect_before.to_string(),"no_effect_after":no_effect_after.to_string(),"no_effect_gap":no_effect_gap,
            "failed_before":failed_before.to_string(),"failed_after":failed_after.to_string(),"failed_gap":failed_gap,
            "actual_error":error,"ledger_before":ledger_before,"ledger_after_failure":ledger_after_failure,
            "all_rows_rolled_back":rows_before==rows_after_failure,"autocommit_after_failure":autocommit,
            "next_success_session":next.session_id.to_string(),"next_success_logical":next.files[0].logical_id.to_string(),"next_success_memory":next_memory.to_string(),
            "next_session_gap_from_failed_after":s3e4d_public_uuid_gap(failed_after,next.session_id),"after_next":after_next.to_string(),
            "final_ledger":final_ledger,"retained_fixture":fixture.to_string_lossy(),
            "no_generator_reset_or_undo":true,
        }),
    );
    assert!(error.contains("UNIQUE constraint failed: source_observations.session_id, source_observations.location"),"{error}");
    assert!(
        failed_gap > no_effect_gap,
        "failed operation did not consume a visible UUID counter slice"
    );
    assert_eq!(ledger_after_failure, ledger_before);
    assert_eq!(rows_before, rows_after_failure);
    assert!(autocommit);
    assert!(same_ms(next.session_id) && same_ms(after_next));
    assert_eq!(
        s3e4d_public_uuid_gap(failed_after, next.session_id),
        1,
        "next successful response exposes the subsequent public UUID stream"
    );
    assert_eq!(final_ledger.0, ledger_before.0 + 1);
    assert_eq!(final_ledger.1, ledger_before.1 + 1);
    assert_integrity(&w.store);
}
