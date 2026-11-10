use super::*;

pub(super) fn fixture() -> (SqliteStore, PreparationTarget) {
    let store = SqliteStore::open_in_memory().unwrap();
    let f = Uuid::now_v7();
    let bytes = "# Snapshot\r\n\r\nOriginal e\u{301} 漢字 \u{202e}\u{200b} text.\r\n";
    let fs;
    {
        let c = store.connection().unwrap();
        let tx = begin_write(&c, &store.revision_state).unwrap();
        tx.execute("INSERT INTO logical_sources(id,kind,state,current_location,created_at) VALUES(?1,'file','present','/snapshot.md',-42)",[f.to_string()]).unwrap();
        let observed = ObservedFile {
            location: "/snapshot.md".into(),
            content: bytes.into(),
            content_hash: sha256_hex(bytes.as_bytes()),
            title: "Snapshot".into(),
            media_type: "text/markdown".into(),
            size_bytes: bytes.len() as u64,
            evidences: vec![],
            assertions: vec![],
            memories: vec![],
        };
        fs = ensure_version(&tx, f, SourceKind::File, &observed, Ts(-42))
            .unwrap()
            .1;
        tx.commit().unwrap();
    }
    let asset_id = ownership::enroll_test_source(&store, f);
    let extraction = crate::ingest::document::extract_document_bytes(
        "/snapshot.md",
        bytes.as_bytes(),
        Some("md"),
        &Default::default(),
        &Default::default(),
        Ts(-42),
    )
    .unwrap();
    let x = store.record_document_extraction(extraction.draft).unwrap();
    (
        store,
        PreparationTarget {
            asset_id,
            legacy_source_version: fs,
            document_id: x.logical_id,
            document_source_version: x.source_id,
            extraction_id: x.extraction_id,
        },
    )
}
fn sample(store: &SqliteStore, target: &PreparationTarget) -> Intent {
    let c = store.connection().unwrap();
    Intent {
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
        snapshot: snapshot(&c, target).unwrap(),
    }
}
fn arm<'a>(store: &'a SqliteStore, i: &Intent, revision: i64) -> PermitScope<'a> {
    *store.preparation_state.permit.lock().unwrap() = Some(Permit {
        enrollment: i.enrollment_values(&i.operation_id().unwrap(), revision),
        receipt: i
            .receipt_values(&i.operation_id().unwrap(), revision)
            .unwrap(),
    });
    PermitScope(&store.preparation_state)
}
fn insert_both(c: &Connection, i: &Intent, revision: i64) {
    let op = i.operation_id().unwrap();
    insert_values(
        c,
        "document_preparation_enrollments",
        ENROLL_COLUMNS,
        &i.enrollment_values(&op, revision),
    )
    .unwrap();
    insert_values(
        c,
        "document_preparation_receipts",
        RECEIPT_COLUMNS,
        &i.receipt_values(&op, revision).unwrap(),
    )
    .unwrap();
}
fn enrolled() -> (SqliteStore, PreparationTarget, Intent) {
    let (store, target) = fixture();
    let i = sample(&store, &target);
    {
        let c = store.connection().unwrap();
        let tx = begin_write(&c, &store.revision_state).unwrap();
        let r = store.revision_state.active_revision.load(Ordering::SeqCst);
        let _scope = arm(&store, &i, r);
        insert_both(&tx, &i, r);
        tx.commit().unwrap();
    }
    (store, target, i)
}

#[test]
fn canonical_intent_has_exact_field_and_domain_binding() {
    let (store, target) = fixture();
    let i = sample(&store, &target);
    let op = i.operation_id().unwrap();
    let mut variants = vec![];
    let mut x = i.clone();
    x.nonce = Uuid::now_v7();
    variants.push(x);
    let mut x = i.clone();
    x.db_path.push('x');
    variants.push(x);
    let mut x = i.clone();
    x.db_dev -= 1;
    variants.push(x);
    let mut x = i.clone();
    x.db_ino += 1;
    variants.push(x);
    let mut x = i.clone();
    x.actor_uid += 1;
    variants.push(x);
    let mut x = i.clone();
    x.expected_revision += 1;
    variants.push(x);
    let mut x = i.clone();
    x.snapshot.target.asset_id = Uuid::now_v7();
    variants.push(x);
    let mut x = i.clone();
    x.snapshot.legacy_base.location = Some("/moved.md".into());
    assert_eq!(x.snapshot.digest(), i.snapshot.digest());
    assert_ne!(x.operation_id().unwrap(), op);
    variants.push(x);
    let mut x = i.clone();
    x.snapshot.document_base.created_at -= 1;
    assert_eq!(x.snapshot.digest(), i.snapshot.digest());
    assert_ne!(x.operation_id().unwrap(), op);
    variants.push(x);
    let mut x = i.clone();
    x.snapshot.legacy_version.title.push('x');
    variants.push(x);
    let mut x = i.clone();
    x.snapshot.extraction.extractor_version = "2".into();
    variants.push(x);
    let mut x = i.clone();
    x.snapshot.extraction.structure_hash = "d".repeat(64);
    variants.push(x);
    for x in variants {
        assert_ne!(x.operation_id().unwrap(), op)
    }
    assert!(parse_id(&op).is_ok());
    assert!(parse_id(&op.replace("local-doc-prepare@1", "local-enroll@1")).is_err());
    assert!(parse_id(&op.to_uppercase()).is_err());
    let mut x = i.clone();
    x.db_path = "a".repeat(MAX_REQUEST);
    assert!(x.canonical().is_err());
    let mut x = i;
    x.snapshot.target.extraction_id = Uuid::now_v7();
    assert!(x.canonical().is_err());
}
#[test]
fn generic_recording_scope_cannot_authorize_preparation_or_availability() {
    let (store, target) = fixture();
    let i = sample(&store, &target);
    let c = store.connection().unwrap();
    let tx = begin_write(&c, &store.revision_state).unwrap();
    let r = store.revision_state.active_revision.load(Ordering::SeqCst);
    assert!(insert_values(
        &tx,
        "document_preparation_enrollments",
        ENROLL_COLUMNS,
        &i.enrollment_values(&i.operation_id().unwrap(), r)
    )
    .is_err());
    assert!(insert_values(
        &tx,
        "document_preparation_receipts",
        RECEIPT_COLUMNS,
        &i.receipt_values(&i.operation_id().unwrap(), r).unwrap()
    )
    .is_err());
    assert!(tx.execute("INSERT INTO m6_row_availability VALUES('document_preparation_enrollments','forged',?1)",[r]).is_err());
    assert!(store.preparation_state.permit.lock().unwrap().is_none());
}
#[test]
fn successful_receipt_is_immutable_and_validates_without_live_snapshot() {
    let (store, target, i) = enrolled();
    let c = store.connection().unwrap();
    let tx = begin_write(&c, &store.revision_state).unwrap();
    tx.execute(
        "UPDATE logical_sources SET current_location='/renamed.md',state='absent' WHERE id=?1",
        [target.document_id.to_string()],
    )
    .unwrap();
    let op = i.operation_id().unwrap();
    let prior = stored(&tx, &op).unwrap().unwrap();
    assert_eq!(prior.intent.snapshot, i.snapshot);
    for table in [
        "document_preparation_enrollments",
        "document_preparation_receipts",
    ] {
        let columns = tx
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for column in columns {
            assert!(
                tx.execute(&format!("UPDATE {table} SET {column}={column}"), [])
                    .is_err(),
                "{table}.{column}"
            )
        }
        assert!(tx.execute(&format!("DELETE FROM {table}"), []).is_err());
        assert!(tx
            .execute(
                &format!("INSERT OR REPLACE INTO {table} SELECT * FROM {table}"),
                []
            )
            .is_err());
    }
    assert!(tx
        .prepare("SELECT owner,head,publication FROM document_preparation_enrollments")
        .is_err());
    assert!(tx.execute("INSERT OR REPLACE INTO m6_row_availability SELECT * FROM m6_row_availability WHERE table_name='document_preparation_schema'",[]).is_err());
    assert_eq!(
        tx.query_row(
            "SELECT owner FROM asset_ownership WHERE asset_id=?1",
            [target.asset_id.to_string()],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "legacy"
    );
}
#[test]
fn missing_receipt_causes_deferred_fk_failure_and_atomic_rollback() {
    let (store, target) = fixture();
    let i = sample(&store, &target);
    let c = store.connection().unwrap();
    let tx = begin_write(&c, &store.revision_state).unwrap();
    let r = store.revision_state.active_revision.load(Ordering::SeqCst);
    let scope = arm(&store, &i, r);
    insert_values(
        &tx,
        "document_preparation_enrollments",
        ENROLL_COLUMNS,
        &i.enrollment_values(&i.operation_id().unwrap(), r),
    )
    .unwrap();
    let error = tx.commit().unwrap_err();
    assert!(!matches!(error, Error::CommitOutcomeUnknown { .. }));
    drop(scope);
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM document_preparation_enrollments",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        i.expected_revision
    );
    assert_eq!(c.query_row("SELECT COUNT(*) FROM m6_row_availability WHERE table_name='document_preparation_enrollments'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    assert!(store.preparation_state.permit.lock().unwrap().is_none());
}
#[test]
fn fixed_states_nulls_types_and_wrong_joins_refuse_even_with_matching_permit() {
    let (store, target) = fixture();
    let i = sample(&store, &target);
    let c = store.connection().unwrap();
    for (index, value) in [
        (11, "document_active".to_owned().into()),
        (0, Value::Null),
        (2, Value::Blob(target.document_id.to_string().into_bytes())),
        (5, target.document_source_version.to_string().into()),
        (8, "bad".to_owned().into()),
    ] {
        let tx = begin_write(&c, &store.revision_state).unwrap();
        let r = store.revision_state.active_revision.load(Ordering::SeqCst);
        let mut enrollment = i.enrollment_values(&i.operation_id().unwrap(), r);
        enrollment[index] = value;
        *store.preparation_state.permit.lock().unwrap() = Some(Permit {
            enrollment: enrollment.clone(),
            receipt: vec![],
        });
        let scope = PermitScope(&store.preparation_state);
        assert!(insert_values(
            &tx,
            "document_preparation_enrollments",
            ENROLL_COLUMNS,
            &enrollment
        )
        .is_err());
        drop(scope);
        drop(tx);
    }
    for (index, value) in [
        (20, "publish_document@1".to_owned().into()),
        (22, "DocumentActive".to_owned().into()),
        (5, 2_i64.into()),
        (11, Value::Null),
        (2, Value::Blob(vec![b'a'; 64])),
        (19, 0_i64.into()),
    ] {
        let tx = begin_write(&c, &store.revision_state).unwrap();
        let r = store.revision_state.active_revision.load(Ordering::SeqCst);
        let mut values = i.receipt_values(&i.operation_id().unwrap(), r).unwrap();
        values[index] = value;
        *store.preparation_state.permit.lock().unwrap() = Some(Permit {
            enrollment: vec![],
            receipt: values.clone(),
        });
        let scope = PermitScope(&store.preparation_state);
        assert!(insert_values(
            &tx,
            "document_preparation_receipts",
            RECEIPT_COLUMNS,
            &values
        )
        .is_err());
        drop(scope);
        drop(tx);
    }
}
#[test]
fn pinned_source_extraction_nodes_and_alternate_unique_keys_cannot_be_replaced() {
    let (store, target, _) = enrolled();
    let c = store.connection().unwrap();
    for recursive in [false, true] {
        c.pragma_update(None, "recursive_triggers", recursive)
            .unwrap();
        let tx = begin_write(&c, &store.revision_state).unwrap();
        for (table, filter, new_id) in [
            (
                "sources",
                format!("id='{}'", target.legacy_source_version),
                true,
            ),
            (
                "sources",
                format!("id='{}'", target.document_source_version),
                true,
            ),
            (
                "doc_extractions",
                format!("id='{}'", target.extraction_id),
                true,
            ),
            (
                "doc_nodes",
                format!("extraction_id='{}'", target.extraction_id),
                false,
            ),
            (
                "logical_sources",
                format!("id='{}'", target.document_id),
                true,
            ),
        ] {
            assert!(
                tx.execute(
                    &format!("INSERT OR REPLACE INTO {table} SELECT * FROM {table} WHERE {filter}"),
                    []
                )
                .is_err(),
                "samePK {table}/{recursive}"
            );
            if new_id {
                let columns = tx
                    .prepare(&format!("PRAGMA table_info({table})"))
                    .unwrap()
                    .query_map([], |r| r.get::<_, String>(1))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap();
                let projection = columns
                    .iter()
                    .map(|n| {
                        if n == "id" {
                            format!("'{}'", Uuid::now_v7())
                        } else {
                            n.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                assert!(tx.execute(&format!("INSERT OR REPLACE INTO {table} SELECT {projection} FROM {table} WHERE {filter}"),[]).is_err(),"alternate key {table}/{recursive}");
            }
        }
        assert!(tx.execute("INSERT INTO doc_nodes(extraction_id,path,kind,precision) VALUES(?1,'999999','paragraph','none')",[target.extraction_id.to_string()]).is_err());
        assert!(tx
            .execute(
                "UPDATE logical_sources SET kind='file' WHERE id=?1",
                [target.document_id.to_string()]
            )
            .is_err());
        assert!(tx
            .execute(
                "UPDATE logical_sources SET repository_id='forged' WHERE id=?1",
                [target.document_id.to_string()]
            )
            .is_err());
        for (table, column, value) in [
            ("sources", "id", target.legacy_source_version),
            ("sources", "id", target.document_source_version),
            ("doc_extractions", "id", target.extraction_id),
            ("logical_sources", "id", target.document_id),
        ] {
            assert!(tx
                .execute(
                    &format!("DELETE FROM {table} WHERE {column}=?1"),
                    [value.to_string()]
                )
                .is_err());
        }
    }
}
#[test]
fn stored_structure_corruption_refuses_before_a_permit() {
    let (store, target) = fixture();
    let c = store.connection().unwrap();
    assert!(snapshot(
        &c,
        &PreparationTarget {
            extraction_id: Uuid::now_v7(),
            ..target.clone()
        }
    )
    .is_err());
    // A normal application writer cannot change source bytes; simulate a pre-enrollment malformed
    // extraction by adding a node after extraction completion, which S1 alone does not seal.
    let tx = begin_write(&c, &store.revision_state).unwrap();
    tx.execute("INSERT INTO doc_nodes(extraction_id,path,kind,precision) VALUES(?1,'999999','paragraph','none')",[target.extraction_id.to_string()]).unwrap();
    tx.commit().unwrap();
    assert!(snapshot(&c, &target).is_err());
    assert!(store.preparation_state.permit.lock().unwrap().is_none());
}

#[test]
fn matching_declared_source_hash_cannot_substitute_for_hashing_actual_bytes() {
    let (store, target) = fixture();
    let original = "# Snapshot\r\n\r\nOriginal e\u{301} 漢字 \u{202e}\u{200b} text.\r\n";
    let mut extraction = crate::ingest::document::extract_document_bytes(
        "/snapshot.md",
        original.as_bytes(),
        Some("md"),
        &Default::default(),
        &Default::default(),
        Ts(-42),
    )
    .unwrap();
    let claimed_hash = "a".repeat(64);
    extraction.draft.source_bytes_hash = claimed_hash.clone();
    extraction.draft.text_is_source = false;
    let new_extraction = store.record_document_extraction(extraction.draft).unwrap();
    let corrupt_version = Uuid::now_v7();
    let c = store.connection().unwrap();
    let tx = begin_write(&c, &store.revision_state).unwrap();
    // Model a pre-enrollment inconsistent stored version without changing existing rows or
    // removing any guard. Its claimed original hash agrees with X, but not its actual bytes.
    tx.execute(
        "INSERT INTO sources(id,logical_id,kind,location,title,content,content_hash,media_type,size_bytes,ingested_at) \
         SELECT ?1,logical_id,kind,location,title,content,?2,media_type,size_bytes,ingested_at \
         FROM sources WHERE id=?3",
        rusqlite::params![corrupt_version.to_string(), claimed_hash, target.legacy_source_version.to_string()],
    )
    .unwrap();
    tx.commit().unwrap();
    let invalid = PreparationTarget {
        legacy_source_version: corrupt_version,
        document_source_version: new_extraction.source_id,
        extraction_id: new_extraction.extraction_id,
        ..target
    };
    let error = snapshot(&c, &invalid).unwrap_err();
    assert!(error
        .to_string()
        .contains("stored source version hash or length is corrupt"));
    assert!(store.preparation_state.permit.lock().unwrap().is_none());
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM document_preparation_enrollments",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}
