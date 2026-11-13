use super::*;
use rusqlite::params;

// Synthetic unit vectors exercise storage/receipts, never actual inference correctness.
fn vectors(source: &SemanticSource, alternate: bool) -> Vec<SemanticVector> {
    source
        .entries
        .iter()
        .map(|e| {
            let mut embedding = vec![0_u8; 3072];
            let offset = if alternate { 4 } else { 0 };
            embedding[offset..offset + 4].copy_from_slice(&1_f32.to_le_bytes());
            SemanticVector {
                entry_id: e.lexical.entry_id.clone(),
                ordinal: e.lexical.ordinal,
                vector_key: e.vector_key.clone(),
                payload_hash: e.lexical.payload_hash.clone(),
                token_count: e.stored_token_count,
                encoding_digest: sha256_hex(b"synthetic encoding"),
                dimension: 768,
                vector_hash: sha256_hex(&embedding),
                embedding,
            }
        })
        .collect()
}
fn sample(
    store: &SqliteStore,
    target: &SemanticTarget,
) -> (Intent, SemanticSource, Vec<SemanticVector>) {
    let c = store.connection().unwrap();
    let source = semantic_input::build(&c, target).unwrap();
    let i = Intent {
        nonce: Uuid::now_v7(),
        db_path: "/protected/mind.db".into(),
        db_dev: u64::MAX,
        db_ino: 42,
        actor_uid: 1000,
        expected_revision: c
            .query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| {
                r.get(0)
            })
            .unwrap(),
        input: source.input.clone(),
        contract: StrictContract::current(&target.model_id).unwrap(),
    };
    let rows = vectors(&source, false);
    semantic_input::validate_vector_rows(&source, &i.contract, &rows).unwrap();
    (i, source, rows)
}
fn staged() -> (SqliteStore, SemanticTarget, Intent) {
    let (store, target) = semantic_input::semantic_source_fixture();
    let (i, _, rows) = sample(&store, &target);
    {
        let c = store.connection().unwrap();
        let tx = begin_write(&c, &store.revision_state).unwrap();
        let r = store.revision_state.active_revision.load(Ordering::SeqCst);
        let op = i.operation_id().unwrap();
        let result = semantic_input::result_digest(&i.input.semantic_id, &rows);
        let _scope = arm(&store.semantic_state, &i, &rows, &op, &result, r).unwrap();
        insert_candidate(&tx, &i, &rows, &op, &result, r).unwrap();
        tx.commit().unwrap();
    }
    (store, target, i)
}
fn count(c: &Connection, table: &str) -> i64 {
    c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
#[test]
fn complete_semantic_set_receipt_and_exact_parent_agree() {
    let (store, target, i) = staged();
    let c = store.connection().unwrap();
    let s = stored(&c, &i.operation_id().unwrap()).unwrap().unwrap();
    let source = semantic_input::build(&c, &target).unwrap();
    verify_stored(&c, &source, &s).unwrap();
    assert_eq!(s.revision, i.expected_revision + 1);
    assert_eq!(count(&c, "doc_staged_semantic_sets"), 1);
    assert_eq!(count(&c, "doc_semantic_stage_receipts"), 1);
    assert_eq!(count(&c, "doc_staged_vectors"), source.entries.len() as i64);
    assert_eq!(count(&c, "assertion_embeddings"), 0);
    assert_eq!(count(&c, "assertions"), 0);
    assert_eq!(count(&c, "memories"), 0);
    assert_eq!(outcome(&s).unwrap().current_integrity, "not_checked");
}
#[test]
fn operation_identity_binds_input_and_authority_but_not_float_results() {
    let (store, target) = semantic_input::semantic_source_fixture();
    let (i, source, first) = sample(&store, &target);
    let second = vectors(&source, true);
    semantic_input::validate_vector_rows(&source, &i.contract, &second).unwrap();
    assert_ne!(
        semantic_input::result_digest(&i.input.semantic_id, &first),
        semantic_input::result_digest(&i.input.semantic_id, &second)
    );
    let id = i.operation_id().unwrap();
    let mut variants = vec![];
    let mut v = i.clone();
    v.nonce = Uuid::now_v7();
    variants.push(v);
    let mut v = i.clone();
    v.db_path.push('x');
    variants.push(v);
    let mut v = i.clone();
    v.db_dev -= 1;
    variants.push(v);
    let mut v = i.clone();
    v.db_ino += 1;
    variants.push(v);
    let mut v = i.clone();
    v.actor_uid += 1;
    variants.push(v);
    let mut v = i.clone();
    v.expected_revision += 1;
    variants.push(v);
    let mut v = i.clone();
    v.input.lexical.logical_base.push(0);
    variants.push(v);
    let mut v = i.clone();
    v.input.parent_revision += 1;
    variants.push(v);
    for v in variants {
        assert_ne!(v.operation_id().unwrap(), id);
    }
    assert!(parse_id(&id).is_ok());
    assert!(
        parse_id(&id.replace("local-doc-stage-semantic@1", "local-doc-stage-lexical@1")).is_err()
    );
    let mut v = i;
    v.db_path = "x".repeat(MAX_REQUEST);
    assert!(v.canonical().is_err());
}
#[test]
fn nondeterministic_loser_returns_stored_winner_without_result_comparison() {
    let (store, target, i) = staged();
    let c = store.connection().unwrap();
    let source = semantic_input::build(&c, &target).unwrap();
    let loser = semantic_input::result_digest(&i.input.semantic_id, &vectors(&source, true));
    let winner = same_artifact(&c, &i, &source).unwrap().unwrap();
    assert_ne!(winner.result_digest, loser);
    assert_eq!(winner.outcome, "existing_artifact");
    assert_eq!(winner.committed_revision, None);
    assert_eq!(winner.current_integrity, "verified");
    assert_eq!(count(&c, "doc_semantic_stage_receipts"), 1);
}
#[test]
fn unarmed_writes_are_refused_with_foreign_keys_off_or_on() {
    let (store, target) = semantic_input::semantic_source_fixture();
    let (i, _, rows) = sample(&store, &target);
    let c = store.connection().unwrap();
    for flag in [false, true] {
        c.pragma_update(None, "foreign_keys", flag).unwrap();
        let tx = begin_write(&c, &store.revision_state).unwrap();
        let r = store.revision_state.active_revision.load(Ordering::SeqCst);
        let op = i.operation_id().unwrap();
        let result = semantic_input::result_digest(&i.input.semantic_id, &rows);
        assert!(insert_candidate(&tx, &i, &rows, &op, &result, r).is_err());
        drop(tx);
        assert_eq!(count(&c, "doc_staged_semantic_sets"), 0);
    }
}
#[test]
fn all_successful_semantic_records_reject_replace_upsert_update_delete() {
    let (store, _, i) = staged();
    let c = store.connection().unwrap();
    for fk in [false, true] {
        for recursive in [false, true] {
            c.pragma_update(None, "foreign_keys", fk).unwrap();
            c.pragma_update(None, "recursive_triggers", recursive)
                .unwrap();
            let tx = begin_write(&c, &store.revision_state).unwrap();
            for (table, key) in [
                ("doc_staged_semantic_sets", "semantic_id"),
                ("doc_staged_vectors", "entry_id"),
                ("doc_semantic_stage_receipts", "operation_id"),
            ] {
                assert!(tx.execute(&format!("DELETE FROM {table}"), []).is_err());
                assert!(tx
                    .execute(&format!("UPDATE {table} SET {key}={key}"), [])
                    .is_err());
                assert!(tx
                    .execute(
                        &format!("INSERT OR REPLACE INTO {table} SELECT * FROM {table}"),
                        []
                    )
                    .is_err());
                assert!(tx.execute(&format!("INSERT INTO {table} SELECT * FROM {table} WHERE 1 ON CONFLICT DO UPDATE SET {key}=excluded.{key}"), []).is_err());
            }
            assert!(stored(&tx, &i.operation_id().unwrap()).unwrap().is_some());
        }
    }
}
#[test]
fn partial_write_rollback_removes_vectors_header_availability_and_revision() {
    let (store, target) = semantic_input::semantic_source_fixture();
    let (i, _, rows) = sample(&store, &target);
    let c = store.connection().unwrap();
    let before = count(&c, "m6_row_availability");
    {
        let tx = begin_write(&c, &store.revision_state).unwrap();
        let r = store.revision_state.active_revision.load(Ordering::SeqCst);
        let op = i.operation_id().unwrap();
        let result = semantic_input::result_digest(&i.input.semantic_id, &rows);
        let _scope = arm(&store.semantic_state, &i, &rows, &op, &result, r).unwrap();
        insert_values(
            &tx,
            "doc_staged_semantic_sets",
            SET_COLUMNS,
            &i.set_values(&op, &result, r).unwrap(),
        )
        .unwrap();
        insert_values(
            &tx,
            "doc_staged_vectors",
            VECTOR_COLUMNS,
            &vector_values(&i.input.semantic_id, &rows[0]),
        )
        .unwrap();
    }
    for t in [
        "doc_staged_semantic_sets",
        "doc_staged_vectors",
        "doc_semantic_stage_receipts",
    ] {
        assert_eq!(count(&c, t), 0);
    }
    assert_eq!(count(&c, "m6_row_availability"), before);
    assert_eq!(
        c.query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        i.expected_revision
    );
}
#[test]
fn header_without_receipt_cannot_commit_even_with_all_vectors() {
    let (store, target) = semantic_input::semantic_source_fixture();
    let (i, _, rows) = sample(&store, &target);
    let c = store.connection().unwrap();
    {
        let tx = begin_write(&c, &store.revision_state).unwrap();
        let r = store.revision_state.active_revision.load(Ordering::SeqCst);
        let op = i.operation_id().unwrap();
        let result = semantic_input::result_digest(&i.input.semantic_id, &rows);
        let _scope = arm(&store.semantic_state, &i, &rows, &op, &result, r).unwrap();
        insert_values(
            &tx,
            "doc_staged_semantic_sets",
            SET_COLUMNS,
            &i.set_values(&op, &result, r).unwrap(),
        )
        .unwrap();
        for row in &rows {
            insert_values(
                &tx,
                "doc_staged_vectors",
                VECTOR_COLUMNS,
                &vector_values(&i.input.semantic_id, row),
            )
            .unwrap();
        }
        assert!(tx.commit().is_err());
    }
    assert_eq!(count(&c, "doc_staged_semantic_sets"), 0);
    assert_eq!(
        c.query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        i.expected_revision
    );
}
#[test]
fn private_permit_does_not_replace_exact_sql_entry_membership_checks() {
    let (store, target) = semantic_input::semantic_source_fixture();
    let (i, _, mut rows) = sample(&store, &target);
    rows[0].entry_id = "invented-entry".into();
    let c = store.connection().unwrap();
    let tx = begin_write(&c, &store.revision_state).unwrap();
    let r = store.revision_state.active_revision.load(Ordering::SeqCst);
    let op = i.operation_id().unwrap();
    let result = semantic_input::result_digest(&i.input.semantic_id, &rows);
    let _scope = arm(&store.semantic_state, &i, &rows, &op, &result, r).unwrap();
    assert!(insert_candidate(&tx, &i, &rows, &op, &result, r).is_err());
}
#[test]
fn incomplete_permitted_vectors_cannot_create_success_receipt() {
    let (store, target) = semantic_input::semantic_source_fixture();
    let (i, _, rows) = sample(&store, &target);
    let c = store.connection().unwrap();
    let tx = begin_write(&c, &store.revision_state).unwrap();
    let r = store.revision_state.active_revision.load(Ordering::SeqCst);
    let op = i.operation_id().unwrap();
    let result = semantic_input::result_digest(&i.input.semantic_id, &rows);
    let _scope = arm(&store.semantic_state, &i, &rows, &op, &result, r).unwrap();
    assert!(insert_candidate(&tx, &i, &[], &op, &result, r).is_err());
}
#[test]
fn bitrot_is_detected_without_changing_historical_success() {
    let (store, target, i) = staged();
    let c = store.connection().unwrap();
    // Deliberate test-only schema bypass simulates offline corruption, outside Model A.
    c.execute_batch("DROP TRIGGER m6_s3d2_vector_update; UPDATE doc_staged_vectors SET embedding=zeroblob(3072)").unwrap();
    let s = stored(&c, &i.operation_id().unwrap()).unwrap().unwrap();
    assert_eq!(outcome(&s).unwrap().current_integrity, "not_checked");
    let source = semantic_input::build(&c, &target).unwrap();
    assert!(verify_stored(&c, &source, &s).is_err());
}

#[test]
fn corrupt_stored_scalar_values_cannot_alias_valid_vector_evidence() {
    for (column, expression) in [
        ("token_count", "token_count + 4294967296"),
        ("dimension", "dimension + 4294967296"),
        ("ordinal", "-1"),
    ] {
        let (store, target, i) = staged();
        let c = store.connection().unwrap();
        let revision: i64 = c
            .query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| {
                r.get(0)
            })
            .unwrap();
        let sql = format!("UPDATE doc_staged_vectors SET {column}={expression}");
        assert!(c.execute(&sql, []).is_err());

        // Simulate offline corruption, not an authorized transition or a bypass
        // available through ordinary writes. Restore every guard before reading.
        let guard: String = c
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name='m6_s3d2_vector_update'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        c.execute_batch("BEGIN IMMEDIATE; DROP TRIGGER m6_s3d2_vector_update")
            .unwrap();
        assert!(c.execute(&sql, []).is_err(), "CHECK must reject {column}");
        c.pragma_update(None, "ignore_check_constraints", true)
            .unwrap();
        assert_eq!(c.execute(&sql, []).unwrap(), 1);
        c.execute_batch(&guard).unwrap();
        c.pragma_update(None, "ignore_check_constraints", false)
            .unwrap();
        c.execute_batch("COMMIT").unwrap();

        let s = stored(&c, &i.operation_id().unwrap()).unwrap().unwrap();
        assert_eq!(s.revision, revision);
        assert_eq!(outcome(&s).unwrap().current_integrity, "not_checked");
        let source = semantic_input::build(&c, &target).unwrap();
        assert!(
            verify_stored(&c, &source, &s).is_err(),
            "corrupt {column} must not alias its valid value during readback"
        );
        assert_eq!(count(&c, "doc_semantic_stage_receipts"), 1);
        assert_eq!(
            c.query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
            revision
        );
    }
}

#[test]
fn sql_rejects_reordered_and_wrong_token_vectors_even_when_permitted() {
    for wrong_token in [false, true] {
        let (store, target) = semantic_input::semantic_source_fixture();
        let (i, _, mut rows) = sample(&store, &target);
        if wrong_token {
            rows[0].token_count += 1;
        } else {
            rows[0].ordinal += 1;
        }
        let c = store.connection().unwrap();
        let tx = begin_write(&c, &store.revision_state).unwrap();
        let r = store.revision_state.active_revision.load(Ordering::SeqCst);
        let op = i.operation_id().unwrap();
        let result = semantic_input::result_digest(&i.input.semantic_id, &rows);
        let _scope = arm(&store.semantic_state, &i, &rows, &op, &result, r).unwrap();
        assert!(insert_candidate(&tx, &i, &rows, &op, &result, r).is_err());
    }
}
#[test]
fn sql_refuses_publication_document_or_forged_parent_header() {
    for column in ["state", "parent_result_digest", "parent_revision"] {
        let (store, target) = semantic_input::semantic_source_fixture();
        let (i, _, rows) = sample(&store, &target);
        let c = store.connection().unwrap();
        let tx = begin_write(&c, &store.revision_state).unwrap();
        let r = store.revision_state.active_revision.load(Ordering::SeqCst);
        let op = i.operation_id().unwrap();
        let result = semantic_input::result_digest(&i.input.semantic_id, &rows);
        let _scope = arm(&store.semantic_state, &i, &rows, &op, &result, r).unwrap();
        let mut header = i.set_values(&op, &result, r).unwrap();
        let (index, value) = match column {
            "state" => (17, Value::Text("published".into())),
            "parent_result_digest" => (4, Value::Text("f".repeat(64))),
            _ => (3, Value::Integer(r)),
        };
        header[index] = value;
        assert!(insert_values(&tx, "doc_staged_semantic_sets", SET_COLUMNS, &header).is_err());
    }
}
#[test]
fn availability_cannot_be_forged_even_in_generic_write_transaction() {
    let (store, _, i) = staged();
    let c = store.connection().unwrap();
    let tx = begin_write(&c, &store.revision_state).unwrap();
    for table in [
        "doc_staged_semantic_sets",
        "doc_staged_vectors",
        "doc_semantic_stage_receipts",
    ] {
        assert!(tx.execute("INSERT OR REPLACE INTO m6_row_availability(table_name,entity_key,revision_id) VALUES(?1,?2,m6_write_revision())",params![table,i.input.semantic_id]).is_err());
        assert!(tx
            .execute(
                "DELETE FROM m6_row_availability WHERE table_name=?1",
                [table]
            )
            .is_err());
        assert!(tx.execute("UPDATE m6_row_availability SET revision_id=m6_write_revision() WHERE table_name=?1",[table]).is_err());
    }
}
#[test]
fn disposable_receipt_guard_mutation_demonstrates_incomplete_set_counterexample() {
    // The production guard is covered by incomplete_permitted_vectors_... above.
    // Mutate only this disposable database, never the production migration/source.
    let (store, target) = semantic_input::semantic_source_fixture();
    let (i, source, rows) = sample(&store, &target);
    let c = store.connection().unwrap();
    c.execute_batch("DROP TRIGGER m6_s3d2_receipt_insert")
        .unwrap();
    {
        let tx = begin_write(&c, &store.revision_state).unwrap();
        let r = store.revision_state.active_revision.load(Ordering::SeqCst);
        let op = i.operation_id().unwrap();
        let result = semantic_input::result_digest(&i.input.semantic_id, &rows);
        let _scope = arm(&store.semantic_state, &i, &rows, &op, &result, r).unwrap();
        insert_candidate(&tx, &i, &[], &op, &result, r).unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(count(&c, "doc_staged_vectors"), 0);
    let s = stored(&c, &i.operation_id().unwrap()).unwrap().unwrap();
    assert!(verify_stored(&c, &source, &s).is_err());
}
#[test]
fn disposable_vector_guard_mutation_demonstrates_token_substitution_counterexample() {
    let (store, target) = semantic_input::semantic_source_fixture();
    let (i, source, mut rows) = sample(&store, &target);
    rows[0].token_count += 1;
    let c = store.connection().unwrap();
    c.execute_batch("DROP TRIGGER m6_s3d2_vector_insert")
        .unwrap();
    {
        let tx = begin_write(&c, &store.revision_state).unwrap();
        let r = store.revision_state.active_revision.load(Ordering::SeqCst);
        let op = i.operation_id().unwrap();
        let result = semantic_input::result_digest(&i.input.semantic_id, &rows);
        let _scope = arm(&store.semantic_state, &i, &rows, &op, &result, r).unwrap();
        insert_candidate(&tx, &i, &rows, &op, &result, r).unwrap();
        tx.commit().unwrap();
    }
    let s = stored(&c, &i.operation_id().unwrap()).unwrap().unwrap();
    assert!(verify_stored(&c, &source, &s).is_err());
}
#[test]
#[ignore = "opt-in in-memory SQLite storage-phase measurement; synthetic vectors, no inference"]
fn semantic_storage_phase_measurements() {
    for iteration in 0..5 {
        // Fixture construction, canonical validation, token provenance and synthetic
        // unit-vector construction are deliberately outside every measured interval.
        let (store, target) = semantic_input::semantic_source_fixture();
        let (i, source, rows) = sample(&store, &target);
        let c = store.connection().unwrap();
        let started = Instant::now();
        let tx = begin_write(&c, &store.revision_state).unwrap();
        let begin_us = started.elapsed().as_micros();
        let r = store.revision_state.active_revision.load(Ordering::SeqCst);
        let started = Instant::now();
        let op = i.operation_id().unwrap();
        let result = semantic_input::result_digest(&i.input.semantic_id, &rows);
        let set = i.set_values(&op, &result, r).unwrap();
        let receipt = i.receipt_values(&op, &result, r).unwrap();
        let serialized_vectors = rows
            .iter()
            .map(|row| vector_values(&i.input.semantic_id, row))
            .collect::<Vec<_>>();
        let metadata_bytes = i.metadata_bytes(&op, &result, r).unwrap();
        std::hint::black_box((&set, &receipt, &serialized_vectors));
        let metadata_serialization_us = started.elapsed().as_micros();
        let _scope = arm(&store.semantic_state, &i, &rows, &op, &result, r).unwrap();
        let started = Instant::now();
        // Production insertion includes its own metadata serialization, private
        // guards, availability triggers and final foreign-key integrity query.
        insert_candidate(&tx, &i, &rows, &op, &result, r).unwrap();
        let insertion_us = started.elapsed().as_micros();
        let started = Instant::now();
        tx.commit().unwrap();
        let commit_us = started.elapsed().as_micros();
        let started = Instant::now();
        let persisted = stored(&c, &op).unwrap().unwrap();
        let receipt_decode_us = started.elapsed().as_micros();
        let started = Instant::now();
        let revalidated_source = semantic_input::build(&c, &target).unwrap();
        let canonical_revalidation_us = started.elapsed().as_micros();
        let started = Instant::now();
        verify_stored(&c, &revalidated_source, &persisted).unwrap();
        let vector_verification_us = started.elapsed().as_micros();
        println!(
            "{}",
            serde_json::json!({
                "measurement": "semantic-storage-phase@1",
                "database": "sqlite-in-memory",
                "iteration": iteration,
                "source_fixture": "prepared Markdown with CRLF, combining marks, CJK and controls",
                "vector_fixture": "synthetic unit vectors; not model-correctness evidence",
                "excluded": "fixture/source/token/model/vector generation; disk durability; RSS",
                "source_bytes": source.input.lexical.source_bytes_len,
                "vectors": rows.len(),
                "vector_blob_bytes": rows.iter().map(|row| row.embedding.len()).sum::<usize>(),
                "charged_semantic_metadata_bytes": metadata_bytes,
                "begin_immediate_us": begin_us,
                "standalone_metadata_and_vector_serialization_us": metadata_serialization_us,
                "production_insert_candidate_us": insertion_us,
                "commit_us": commit_us,
                "stored_receipt_decode_us": receipt_decode_us,
                "canonical_source_revalidation_us": canonical_revalidation_us,
                "verify_stored_vectors_us": vector_verification_us
            })
        );
    }
}
