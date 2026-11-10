use super::*;

fn sample(store: &SqliteStore, target: &LexicalStageTarget) -> (Intent, LexicalCandidate) {
    let c = store.connection().unwrap();
    let candidate = lexical_input::build(&c, target).unwrap();
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
        input: candidate.input.clone(),
        result_digest: candidate.result_digest(),
    };
    (i, candidate)
}
fn staged() -> (SqliteStore, LexicalStageTarget, Intent) {
    let (store, target) = lexical_input::chunked_test_fixture();
    let (i, c) = sample(&store, &target);
    {
        let conn = store.connection().unwrap();
        let tx = begin_write(&conn, &store.revision_state).unwrap();
        let r = store.revision_state.active_revision.load(Ordering::SeqCst);
        let op = i.operation_id().unwrap();
        let _scope = arm(&store.lexical_state, &i, &c, &op, r).unwrap();
        insert_candidate(&tx, &i, &c, &op, r).unwrap();
        tx.commit().unwrap();
    }
    (store, target, i)
}
#[test]
fn immutable_intent_binds_every_authority_field() {
    let (store, target) = lexical_input::chunked_test_fixture();
    let (i, _) = sample(&store, &target);
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
    v.input.logical_base.push(0);
    variants.push(v);
    let mut v = i.clone();
    v.input.preparation_revision += 1;
    variants.push(v);
    let mut v = i.clone();
    v.result_digest = "a".repeat(64);
    variants.push(v);
    for v in variants {
        assert_ne!(v.operation_id().unwrap(), id)
    }
    assert!(parse_id(&id).is_ok());
    assert!(parse_id(&id.replace("local-doc-stage-lexical@1", "local-doc-prepare@1")).is_err());
    let mut v = i;
    v.db_path = "x".repeat(MAX_REQUEST);
    assert!(v.canonical().is_err());
}
#[test]
fn complete_canonical_entries_receipt_and_cache_agree() {
    let (store, target, i) = staged();
    let c = store.connection().unwrap();
    let candidate = lexical_input::build(&c, &target).unwrap();
    let s = verify_candidate(&c, &candidate).unwrap();
    assert_eq!(s.intent.operation_id().unwrap(), i.operation_id().unwrap());
    assert_eq!(s.revision, i.expected_revision + 1);
    assert_eq!(
        c.query_row("SELECT count(*) FROM doc_staged_lexical_sets", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        c.query_row("SELECT count(*) FROM doc_stage_receipts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM doc_staged_fts WHERE doc_staged_fts MATCH 'Snapshot'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}
#[test]
fn unarmed_sql_cannot_create_or_replace_any_lexical_record() {
    let (store, target) = lexical_input::chunked_test_fixture();
    let (i, c) = sample(&store, &target);
    let conn = store.connection().unwrap();
    let tx = begin_write(&conn, &store.revision_state).unwrap();
    let r = store.revision_state.active_revision.load(Ordering::SeqCst);
    let op = i.operation_id().unwrap();
    assert!(insert_candidate(&tx, &i, &c, &op, r).is_err());
    drop(tx);
    assert_eq!(
        conn.query_row("SELECT count(*) FROM doc_staged_lexical_sets", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn canonical_records_and_selected_inputs_are_sealed() {
    let (store, target, i) = staged();
    let c = store.connection().unwrap();
    let tx = begin_write(&c, &store.revision_state).unwrap();
    for table in [
        "doc_staged_lexical_sets",
        "doc_staged_entries",
        "doc_stage_receipts",
    ] {
        assert!(tx.execute(&format!("DELETE FROM {table}"), []).is_err());
        assert!(tx
            .execute(
                &format!(
                    "UPDATE {table} SET {}={}",
                    if table == "doc_staged_entries" {
                        "content"
                    } else {
                        "stage_id"
                    },
                    if table == "doc_staged_entries" {
                        "content"
                    } else {
                        "stage_id"
                    }
                ),
                []
            )
            .is_err());
        assert!(tx
            .execute(
                &format!("INSERT OR REPLACE INTO {table} SELECT * FROM {table}"),
                []
            )
            .is_err());
    }
    for (table, condition) in [
        (
            "sources",
            format!("id='{}'", target.document_source_version),
        ),
        ("doc_extractions", format!("id='{}'", target.extraction_id)),
        (
            "doc_chunk_sets",
            format!("extraction_id='{}'", target.extraction_id),
        ),
        (
            "doc_chunks",
            format!("extraction_id='{}'", target.extraction_id),
        ),
        (
            "doc_nodes",
            format!("extraction_id='{}'", target.extraction_id),
        ),
    ] {
        assert!(tx
            .execute(&format!("DELETE FROM {table} WHERE {condition}"), [])
            .is_err());
        assert!(tx
            .execute(
                &format!("INSERT OR REPLACE INTO {table} SELECT * FROM {table} WHERE {condition}"),
                []
            )
            .is_err());
    }
    assert!(stored(&tx, &i.operation_id().unwrap()).unwrap().is_some());
}
#[test]
fn cache_corruption_is_detected_without_rewriting_historical_receipt() {
    let (store, target, i) = staged();
    let c = store.connection().unwrap();
    let candidate = lexical_input::build(&c, &target).unwrap();
    c.execute(
        "INSERT INTO doc_staged_fts(doc_staged_fts) VALUES('delete-all')",
        [],
    )
    .unwrap();
    assert!(verify_candidate(&c, &candidate).is_err());
    let s = stored(&c, &i.operation_id().unwrap()).unwrap().unwrap();
    assert_eq!(s.revision, i.expected_revision + 1);
    assert_eq!(
        outcome(&s.intent, s.revision).unwrap().current_integrity,
        "not_checked"
    );
}
#[test]
fn dropping_partial_write_rolls_back_entries_fts_and_recording_revision() {
    let (store, target) = lexical_input::chunked_test_fixture();
    let (i, c) = sample(&store, &target);
    let conn = store.connection().unwrap();
    let prior: i64 = conn
        .query_row("SELECT max(revision_id) FROM m6_revisions", [], |r| {
            r.get(0)
        })
        .unwrap();
    {
        let tx = begin_write(&conn, &store.revision_state).unwrap();
        let r = store.revision_state.active_revision.load(Ordering::SeqCst);
        let op = i.operation_id().unwrap();
        let _scope = arm(&store.lexical_state, &i, &c, &op, r).unwrap();
        insert_values(
            &tx,
            "doc_staged_lexical_sets",
            SET_COLUMNS,
            &i.set_values(&op, r).unwrap(),
        )
        .unwrap();
        for e in &c.entries {
            insert_values(
                &tx,
                "doc_staged_entries",
                ENTRY_COLUMNS,
                &entry_values(&i.input.stage_id, e),
            )
            .unwrap();
        }
    }
    assert_eq!(
        conn.query_row("SELECT max(revision_id) FROM m6_revisions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        prior
    );
    for t in [
        "doc_staged_lexical_sets",
        "doc_staged_entries",
        "doc_stage_receipts",
    ] {
        assert_eq!(
            conn.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        )
    }
    verify_cache(&conn).unwrap();
}
#[test]
fn receipt_is_required_in_the_same_revision() {
    let (store, target) = lexical_input::chunked_test_fixture();
    let (i, c) = sample(&store, &target);
    let conn = store.connection().unwrap();
    let tx = begin_write(&conn, &store.revision_state).unwrap();
    let r = store.revision_state.active_revision.load(Ordering::SeqCst);
    let op = i.operation_id().unwrap();
    let _scope = arm(&store.lexical_state, &i, &c, &op, r).unwrap();
    insert_values(
        &tx,
        "doc_staged_lexical_sets",
        SET_COLUMNS,
        &i.set_values(&op, r).unwrap(),
    )
    .unwrap();
    assert!(tx.commit().is_err());
    assert!(stored(&conn, &op).unwrap().is_none());
}

#[test]
fn newly_selected_versions_are_sealed_beyond_initial_preparation() {
    use super::super::documents::{
        chunk_identity, chunk_plan_hash, root_chunk_anchor, ChunkIdentity, PreparedChunk,
    };
    let (store, mut target) = lexical_input::chunked_test_fixture();
    let text = "# Later revision\r\n\r\nNew canonical 漢字 e\u{301} payload.\r\n";
    let f = {
        let c = store.connection().unwrap();
        c.query_row(
            "SELECT source_id FROM asset_ownership WHERE asset_id=?1",
            [target.asset_id.to_string()],
            |r| r.get::<_, String>(0),
        )
        .unwrap()
    };
    let f = Uuid::parse_str(&f).unwrap();
    {
        let conn = store.connection().unwrap();
        let tx = begin_write(&conn, &store.revision_state).unwrap();
        let observed = ObservedFile {
            location: "/snapshot.md".into(),
            content: text.into(),
            content_hash: sha256_hex(text.as_bytes()),
            title: "Later".into(),
            media_type: "text/markdown".into(),
            size_bytes: text.len() as u64,
            evidences: vec![],
            assertions: vec![],
            memories: vec![],
        };
        target.legacy_source_version = ensure_version(&tx, f, SourceKind::File, &observed, Ts(1))
            .unwrap()
            .1;
        tx.commit().unwrap();
    }
    let extraction = crate::ingest::document::extract_document_bytes(
        "/snapshot.md",
        text.as_bytes(),
        Some("md"),
        &Default::default(),
        &Default::default(),
        Ts(1),
    )
    .unwrap();
    let draft = extraction.draft.clone();
    let info = store.record_document_extraction(extraction.draft).unwrap();
    assert_eq!(info.logical_id, target.document_id);
    target.document_source_version = info.source_id;
    target.extraction_id = info.extraction_id;
    let chunk = PreparedChunk {
        start: 0,
        end: draft.canonical_text.len() as u64,
        tokens: 42,
        context: draft
            .nodes
            .iter()
            .filter(|n| {
                n.kind == crate::core::NodeKind::Section
                    && n.text_range == Some((0, draft.canonical_text.len() as u64))
            })
            .map(|n| n.path.clone())
            .collect(),
    };
    target.plan_hash = chunk_plan_hash(
        &target.chunker_version,
        &target.budget_identity,
        std::slice::from_ref(&chunk),
    );
    {
        let conn = store.connection().unwrap();
        let tx = begin_write(&conn, &store.revision_state).unwrap();
        tx.execute(
            "INSERT INTO doc_chunk_sets VALUES(?1,?2,?3,?4,1)",
            rusqlite::params![
                target.extraction_id.to_string(),
                target.chunker_version,
                target.budget_identity,
                target.plan_hash
            ],
        )
        .unwrap();
        let text_hash = draft.text_hash();
        let structure_hash = draft.structure_hash();
        let identity = ChunkIdentity {
            logical_id: target.document_id,
            draft: &draft,
            text_hash: &text_hash,
            structure_hash: &structure_hash,
            version: &target.chunker_version,
            budget_identity: &target.budget_identity,
        };
        tx.execute(
            "INSERT INTO doc_chunks VALUES(?1,?2,?3,?4,0,0,?5,42,?6,?7)",
            rusqlite::params![
                chunk_identity(&identity, &chunk),
                target.extraction_id.to_string(),
                target.chunker_version,
                target.budget_identity,
                chunk.end as i64,
                serde_json::to_string(&chunk.context).unwrap(),
                root_chunk_anchor(target.document_id, &draft, (0, chunk.end))
                    .unwrap()
                    .to_string()
            ],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let (i, c) = sample(&store, &target);
    {
        let conn = store.connection().unwrap();
        let tx = begin_write(&conn, &store.revision_state).unwrap();
        let r = store.revision_state.active_revision.load(Ordering::SeqCst);
        let op = i.operation_id().unwrap();
        let _scope = arm(&store.lexical_state, &i, &c, &op, r).unwrap();
        insert_candidate(&tx, &i, &c, &op, r).unwrap();
        tx.commit().unwrap();
    }
    let conn = store.connection().unwrap();
    let tx = begin_write(&conn, &store.revision_state).unwrap();
    for (table, key, id) in [
        ("sources", "id", target.legacy_source_version),
        ("sources", "id", target.document_source_version),
        ("doc_extractions", "id", target.extraction_id),
    ] {
        assert!(tx
            .execute(
                &format!("INSERT OR REPLACE INTO {table} SELECT * FROM {table} WHERE {key}=?1"),
                [id.to_string()]
            )
            .is_err());
    }
    assert!(tx.execute("INSERT INTO doc_nodes(extraction_id,path,kind,precision) VALUES(?1,'9999','paragraph','none')",[target.extraction_id.to_string()]).is_err());
}
