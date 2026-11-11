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

// DO NOT APPLY: retained, UNEXECUTED experimental additions below.
// These are evidence of the rejected investigation, not production acceptance.

// Source-independent successful-operation trace. The same source is compiled
// against the signed baseline and candidate. The runner compares table rows,
// reports and validated UUID allocation offsets, not random UUID suffix bytes.
#[test]
fn s3e4d_success_compatibility_fixed_clock() {
    let fixed_ms = std::env::var("MINDSPACE_S3E4D_FIXED_REALTIME_SECONDS")
        .expect("run this exact test with the fixed-CLOCK_REALTIME shim")
        .parse::<u64>()
        .unwrap()
        * 1000
        + std::env::var("MINDSPACE_S3E4D_FIXED_REALTIME_NANOS")
            .unwrap()
            .parse::<u64>()
            .unwrap()
            / 1_000_000;
    let anchor = Uuid::now_v7();
    let control = Uuid::now_v7();
    assert_eq!((anchor.as_u128() >> 80) as u64, fixed_ms);
    assert_eq!((control.as_u128() >> 80) as u64, fixed_ms);
    assert_eq!(s3e4d_public_uuid_gap(anchor, control), 1);
    let w = World::new();
    let file = |name: &str, text: &str, spaces: &[Uuid]| {
        crate::ingest::observe_bytes(
            Path::new(name),
            text.as_bytes().to_vec(),
            format!("{ROOT}/{name}"),
            spaces,
        )
        .unwrap()
    };
    let input = |scope: IngestScope, files: Vec<ObservedFile>, at: i64| IngestObservation {
        session: SessionDraft {
            root: ROOT.into(),
            scope,
            observed_at: Ts(at),
        },
        files,
        spaces: vec![w.notes, w.other],
    };
    let mut trace = Vec::new();
    let mut capture = |stage: &str, report: &ObservationReport| {
        assert_integrity(&w.store);
        let observed_uuid = Uuid::now_v7();
        assert_eq!((observed_uuid.as_u128() >> 80) as u64, fixed_ms);
        trace.push(serde_json::json!({
            "stage":stage, "report":report,
            "all_table_rows":dump(&w.store.connection().unwrap(),true),
            "ledger":ledger(&w.store), "observed_uuid":observed_uuid.to_string(),
        }));
    };
    let a_old = "# Same\n\nSYNTHETIC_A_OLD\n\n# Same\n\nSYNTHETIC_A_KEEP\n\n# Removed\n\nSYNTHETIC_REMOVED\n";
    let a_new =
        "# Same\n\nSYNTHETIC_A_NEW\n\n# Same\n\nSYNTHETIC_A_KEEP\n\n# Added\n\nSYNTHETIC_ADDED\n";
    let b_text = "# Other\n\nSYNTHETIC_B\n";
    let seed = w
        .store
        .record_observation(input(
            IngestScope::Directory,
            vec![
                file("b.md", b_text, &[w.notes]),
                file("a.md", a_old, &[w.notes]),
            ],
            100,
        ))
        .unwrap();
    assert_eq!(seed.files.len(), 2);
    capture("populated_seed", &seed);
    let first_assertion = w.store.list_memories_by_subject("same").unwrap()[0].assertion_id;
    w.store
        .save_embeddings(&[AssertionEmbedding {
            assertion_id: first_assertion,
            derivation_key: "synthetic-s3e4d@1".into(),
            model: "synthetic".into(),
            model_version: "1".into(),
            dimension: 2,
            embedding: vec![1.0, 0.0],
        }])
        .unwrap();
    let mut changed = file("a.md", a_new, &[w.notes, w.other]);
    // Repeated and overlapping membership specifications must not duplicate
    // additions or independently reconcile the same old target.
    changed.memories.push(changed.memories[0].clone());
    changed.memories[0]
        .spaces
        .extend([w.other, w.notes, w.other]);
    let changed = w
        .store
        .record_observation(input(
            IngestScope::Directory,
            vec![changed, file("b.md", b_text, &[w.notes, w.other])],
            200,
        ))
        .unwrap();
    assert_eq!(changed.files[0].logical_id, seed.files[0].logical_id);
    assert!(changed.files[0]
        .actions
        .iter()
        .any(|a| matches!(a.action, MemoryActionKind::Superseded { .. })));
    assert!(changed.files[0]
        .actions
        .iter()
        .any(|a| matches!(a.action, MemoryActionKind::Retired)));
    assert!(changed.files[0]
        .actions
        .iter()
        .any(|a| matches!(a.action, MemoryActionKind::Created)));
    capture("existing_targets_repeated_specs", &changed);
    let b_memory = changed.files[1].actions[0].memory_id;
    w.store
        .retire_memory(
            b_memory,
            Some("SYNTHETIC_MANUAL_RETIREMENT".into()),
            Ts(250),
        )
        .unwrap();
    let recreated = w
        .store
        .record_observation(input(
            IngestScope::File,
            vec![file("b.md", b_text, &[w.notes, w.other])],
            300,
        ))
        .unwrap();
    assert!(matches!(
        recreated.files[0].actions[0].action,
        MemoryActionKind::Created
    ));
    assert_ne!(recreated.files[0].actions[0].memory_id, b_memory);
    capture("partial_manual_retirement_recreation", &recreated);
    let moved = w
        .store
        .record_observation(input(
            IngestScope::Directory,
            vec![
                file("moved.md", a_new, &[w.notes, w.other]),
                file("b.md", b_text, &[w.notes, w.other]),
            ],
            400,
        ))
        .unwrap();
    assert!(matches!(
        moved.files[1].outcome,
        FileOutcome::Continued { .. }
    ));
    assert!(moved.files[1].actions.is_empty());
    capture("full_move", &moved);
    let disappeared = w
        .store
        .record_observation(input(
            IngestScope::Directory,
            vec![file("b.md", b_text, &[w.notes, w.other])],
            500,
        ))
        .unwrap();
    assert_eq!(disappeared.events.len(), 1);
    capture("full_disappearance", &disappeared);
    let reappeared = w
        .store
        .record_observation(input(
            IngestScope::Directory,
            vec![
                file("moved.md", a_new, &[w.notes, w.other]),
                file("b.md", b_text, &[w.notes, w.other]),
            ],
            600,
        ))
        .unwrap();
    assert!(matches!(
        reappeared.files[1].outcome,
        FileOutcome::Continued { .. }
    ));
    assert!(reappeared.files[1].actions.is_empty());
    capture("full_reappearance", &reappeared);
    let fixture = w._dir.keep();
    s3e4d_probe_save(
        "success_compatibility_fixed_clock",
        serde_json::json!({
            "baseline":"3f7fd3422047b40f9dc6fa787d4b523d7d90d857",
            "category":"successful-operation compatibility trace, not publication acceptance",
            "fixed_unix_millis":fixed_ms,"uuid_anchor":anchor.to_string(),
            "adjacent_control_gap":s3e4d_public_uuid_gap(anchor,control),
            "trace":trace,"all_integrity_checks_passed":true,
            "retained_fixture":fixture.to_string_lossy(),
        }),
    );
}

#[test]
fn s3e4d_read_failure_compatibility_fixed_clock() {
    let seconds = std::env::var("MINDSPACE_S3E4D_FIXED_REALTIME_SECONDS")
        .expect("run this exact test with the fixed-CLOCK_REALTIME shim")
        .parse::<u64>()
        .unwrap();
    let nanos = std::env::var("MINDSPACE_S3E4D_FIXED_REALTIME_NANOS")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let fixed_ms = seconds * 1000 + nanos / 1_000_000;
    let anchor = Uuid::now_v7();
    let control = Uuid::now_v7();
    assert_eq!((anchor.as_u128() >> 80) as u64, fixed_ms);
    assert_eq!(s3e4d_public_uuid_gap(anchor, control), 1);
    let mut trace = Vec::new();
    for phase in ["summary", "requested_space", "reuse_membership"] {
        let w = World::new();
        let seed_input = if phase == "reuse_membership" {
            let file = crate::ingest::observe_bytes(
                Path::new("a.md"),
                b"# A\n\nSYNTHETIC_OLD\n\n# Z\n\nSYNTHETIC_KEEP\n".to_vec(),
                format!("{ROOT}/a.md"),
                &[w.notes],
            )
            .unwrap();
            s3e4d_probe_observation(vec![file], &[w.notes])
        } else {
            w.observation(
                IngestScope::File,
                &[("a.md", "SYNTHETIC_READ_SEED")],
                &[w.notes],
                100,
            )
        };
        let seed = w.store.record_observation(seed_input).unwrap();
        let membership_stage = if phase == "reuse_membership" {
            let old = seed.files[0]
                .actions
                .iter()
                .find(|a| a.subject == "Z")
                .unwrap();
            Some(format!("membership_read:{}", old.memory_id))
        } else {
            None
        };
        let failed_input = if phase == "reuse_membership" {
            let file = crate::ingest::observe_bytes(
                Path::new("a.md"),
                b"# A\n\nSYNTHETIC_NEW\n\n# Z\n\nSYNTHETIC_KEEP\n".to_vec(),
                format!("{ROOT}/a.md"),
                &[w.notes, w.other],
            )
            .unwrap();
            let mut input = s3e4d_probe_observation(vec![file], &[w.notes, w.other]);
            input.session.observed_at = Ts(200);
            input
        } else {
            w.observation(
                IngestScope::File,
                &[("a.md", "SYNTHETIC_READ_CHANGED")],
                &[w.notes, w.other],
                200,
            )
        };
        if phase == "requested_space" {
            w.store
                .retire_memory(
                    seed.files[0].actions[0].memory_id,
                    Some("SYNTHETIC_EMPTY_ACTIVE_SUMMARY".into()),
                    Ts(150),
                )
                .unwrap();
        }
        let before_rows = dump(&w.store.connection().unwrap(), true);
        let before_ledger = ledger(&w.store);
        let read_before = Uuid::now_v7();
        let hits = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&hits);
        let outcome = with_hook(
            move |stage, conn| {
                if membership_stage.as_deref() == Some(stage) {
                    count.fetch_add(1, AtomicOrdering::SeqCst);
                    return Err(Error::Storage(
                        "synthetic target-specific membership read failure".into(),
                    ));
                }
                if stage == "before_plan" {
                    let count = Arc::clone(&count);
                    conn.authorizer(Some(move |ctx:AuthContext<'_>| {
                    let rejected = matches!(ctx.action,
                        AuthAction::Read {table_name:"memories",column_name:"id"} if phase=="summary")
                        || matches!(ctx.action,
                        AuthAction::Read {table_name:"spaces",column_name:"name"} if phase=="requested_space");
                    if rejected { count.fetch_add(1,AtomicOrdering::SeqCst); Authorization::Deny }
                    else { Authorization::Allow }
                })).map_err(dberr)?;
                }
                Ok(())
            },
            || w.store.record_observation(failed_input.clone()),
        );
        clear_authorizer(&w.store.connection().unwrap());
        let read_after = Uuid::now_v7();
        assert!(outcome.is_err(), "the selected read boundary must fail");
        assert!(hits.load(AtomicOrdering::SeqCst) > 0);
        assert_eq!(before_rows, dump(&w.store.connection().unwrap(), true));
        assert_eq!(before_ledger, ledger(&w.store));
        assert!(w.store.connection().unwrap().is_autocommit());
        let mut next_input = failed_input;
        next_input.session.observed_at = Ts(300);
        let next = w.store.record_observation(next_input).unwrap();
        assert_eq!(s3e4d_public_uuid_gap(read_after, next.session_id), 1);
        assert_integrity(&w.store);
        trace.push(serde_json::json!({
            "phase":phase,"actual_error":format!("{outcome:?}"),
            "injection_kind":if phase=="reuse_membership" {"synthetic target-specific read-boundary failure"} else {"actual SQLite authorizer read denial"},
            "read_before":read_before.to_string(),"read_after":read_after.to_string(),
            "failed_gap":s3e4d_public_uuid_gap(read_before,read_after),
            "all_rows_rolled_back":true,"ledger_before":before_ledger,
            "next_report":next,"all_table_rows":dump(&w.store.connection().unwrap(),true),
            "next_session_gap_from_failed_after":s3e4d_public_uuid_gap(read_after,next.session_id),
        }));
        w._dir.keep();
    }
    s3e4d_probe_save(
        "read_failure_compatibility_fixed_clock",
        serde_json::json!({
            "baseline":"3f7fd3422047b40f9dc6fa787d4b523d7d90d857",
            "category":"read-failure compatibility trace, not publication acceptance",
            "fixed_unix_millis":fixed_ms,"uuid_anchor":anchor.to_string(),
            "adjacent_control_gap":s3e4d_public_uuid_gap(anchor,control),
            "trace":trace,"all_rows_rolled_back":true,"all_integrity_checks_passed":true,
        }),
    );
}
