use super::*;

fn sample() -> Intent {
    Intent {
        nonce: Uuid::parse_str("e054f8e8-6e47-4ba6-b808-645fda4c5f21").unwrap(),
        db_path: "/private/mind.db".into(),
        db_dev: u64::MAX,
        db_ino: 42,
        actor_uid: 1000,
        source: Uuid::parse_str("3222e594-3295-47e7-a890-b3500d83d8fa").unwrap(),
        expected: 3,
        base: "b".repeat(64),
    }
}

#[test]
fn full_id_is_stable_and_binds_each_intent_field() {
    let i = sample();
    let original = i.id().unwrap();
    assert_eq!(i.id().unwrap(), original);
    assert_eq!(parse_id(&original).unwrap().0, i.nonce);
    let mut variants = Vec::new();
    let mut x = i.clone();
    x.nonce = Uuid::now_v7();
    variants.push(x);
    let mut x = i.clone();
    x.source = Uuid::now_v7();
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
    x.expected += 1;
    variants.push(x);
    let mut x = i.clone();
    x.base = "c".repeat(64);
    variants.push(x);
    for changed in variants {
        assert_ne!(changed.id().unwrap(), original);
    }
    assert!(parse_id(&i.nonce.to_string()).is_err());
    assert!(parse_id(&original.to_uppercase()).is_err());
    assert!(parse_id(&(original + ":ignored")).is_err());
    let mut oversized = i.clone();
    oversized.db_path = "x".repeat(MAX_REQUEST);
    assert!(oversized.id().is_err());
    let mut invalid = i;
    invalid.expected = -1;
    assert!(invalid.id().is_err());
}

fn fixture() -> (SqliteStore, Uuid) {
    let store = SqliteStore::open_in_memory().unwrap();
    let source = Uuid::now_v7();
    {
        let c = store.connection().unwrap();
        let tx = begin_write(&c, &store.revision_state).unwrap();
        tx.execute("INSERT INTO logical_sources(id,kind,state,current_location,created_at) VALUES(?1,'file','present',?2,-42)", rusqlite::params![source.to_string(), "a\r\n\u{202e}\u{200b}e\u{301}"]).unwrap();
        tx.commit().unwrap();
    }
    (store, source)
}

#[test]
fn source_base_preserves_signed_timestamp_and_original_location_bytes() {
    let (store, source) = fixture();
    let c = store.connection().unwrap();
    let a = source_base(&c, source).unwrap();
    assert_eq!(a.location.as_deref(), Some("a\r\n\u{202e}\u{200b}e\u{301}"));
    assert_eq!(a.fingerprint, source_base(&c, source).unwrap().fingerprint);
    let tx = begin_write(&c, &store.revision_state).unwrap();
    tx.execute(
        "UPDATE logical_sources SET current_location=?1 WHERE id=?2",
        rusqlite::params!["a\n\u{202e}\u{200b}é", source.to_string()],
    )
    .unwrap();
    let b = source_base(&tx, source).unwrap();
    assert_ne!(a.fingerprint, b.fingerprint);
    drop(tx);
    assert_eq!(a.fingerprint, source_base(&c, source).unwrap().fingerprint);
    assert!(source_base(&c, Uuid::now_v7()).is_err());
}

fn owner_values(
    asset: Uuid,
    source: Uuid,
    op: &str,
    rev: i64,
    owner: &str,
    epoch: i64,
) -> Vec<Value> {
    vec![
        asset.to_string().into(),
        source.to_string().into(),
        op.to_owned().into(),
        owner.to_owned().into(),
        epoch.into(),
        Value::Null,
        rev.into(),
        rev.into(),
    ]
}

#[test]
fn normal_recording_scope_cannot_enroll_or_forge_availability() {
    let (store, source) = fixture();
    let c = store.connection().unwrap();
    let tx = begin_write(&c, &store.revision_state).unwrap();
    let r = store.revision_state.active_revision.load(Ordering::SeqCst);
    assert!(tx
        .execute(
            "INSERT INTO asset_ownership VALUES(?1,?2,'legacy',1,NULL,'forged',?3,?3)",
            rusqlite::params![Uuid::now_v7().to_string(), source.to_string(), r]
        )
        .is_err());
    assert!(tx
        .execute(
            "INSERT INTO m6_row_availability VALUES('asset_ownership','forged',?1)",
            [r]
        )
        .is_err());
    assert!(tx.execute("UPDATE m6_row_availability SET entity_key='changed' WHERE table_name='asset_ownership_schema'",[]).is_err());
    assert!(tx
        .execute(
            "DELETE FROM m6_row_availability WHERE table_name='asset_ownership_schema'",
            []
        )
        .is_err());
    assert!(store.ownership_state.permit.lock().unwrap().is_none());
}

#[test]
fn document_owner_and_later_epoch_are_rejected_even_with_matching_private_scope() {
    let (store, source) = fixture();
    let c = store.connection().unwrap();
    let tx = begin_write(&c, &store.revision_state).unwrap();
    let r = store.revision_state.active_revision.load(Ordering::SeqCst);
    for (owner, epoch) in [("document", 1), ("legacy", 2), ("legacy", 0)] {
        let asset = Uuid::now_v7();
        *store.ownership_state.permit.lock().unwrap() = Some(Permit {
            receipt: Vec::new(),
            owner: owner_values(asset, source, "intent", r, owner, epoch),
        });
        let _scope = PermitScope(&store.ownership_state);
        assert!(tx
            .execute(
                "INSERT INTO asset_ownership VALUES(?1,?2,?3,?4,NULL,'intent',?5,?5)",
                rusqlite::params![asset.to_string(), source.to_string(), owner, epoch, r]
            )
            .is_err());
    }
    assert!(store.ownership_state.permit.lock().unwrap().is_none());
    drop(tx);
    assert!(c
        .prepare("SELECT publication FROM asset_ownership")
        .is_err());
}

#[test]
fn deferred_receipt_relation_rolls_back_partial_owner_history_and_revision() {
    let (store, source) = fixture();
    let c = store.connection().unwrap();
    let before: i64 = c
        .query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| {
            r.get(0)
        })
        .unwrap();
    let tx = begin_write(&c, &store.revision_state).unwrap();
    let r = store.revision_state.active_revision.load(Ordering::SeqCst);
    let asset = Uuid::now_v7();
    *store.ownership_state.permit.lock().unwrap() = Some(Permit {
        receipt: Vec::new(),
        owner: owner_values(asset, source, "missing-receipt", r, "legacy", 1),
    });
    let scope = PermitScope(&store.ownership_state);
    tx.execute(
        "INSERT INTO asset_ownership VALUES(?1,?2,'legacy',1,NULL,'missing-receipt',?3,?3)",
        rusqlite::params![asset.to_string(), source.to_string(), r],
    )
    .unwrap();
    assert_eq!(
        tx.query_row("SELECT COUNT(*) FROM asset_ownership_history", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let err = tx.commit().unwrap_err();
    assert!(
        !matches!(err, Error::CommitOutcomeUnknown { .. }),
        "deferred FK failure must be definitive: {err}"
    );
    drop(scope);
    for t in [
        "asset_ownership",
        "asset_ownership_history",
        "asset_control_intents",
    ] {
        assert_eq!(
            c.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(
        c.query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        before
    );
    assert!(store.ownership_state.permit.lock().unwrap().is_none());
}

#[test]
fn every_control_trigger_is_in_the_persistent_inventory() {
    let names = enforcement_names();
    assert!(names.len() >= 15);
    assert!(names.iter().all(|n| n.starts_with("m6_s3b_")));
    let store = SqliteStore::open_in_memory().unwrap();
    let c = store.connection().unwrap();
    for name in names {
        assert!(c
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='trigger' AND name=?1)",
                [name],
                |r| r.get::<_, bool>(0)
            )
            .unwrap());
    }
}

#[test]
fn sql_binds_all_receipt_fields_and_forbids_successful_receipt_rewrites() {
    let (store, source) = fixture();
    let c = store.connection().unwrap();
    let tx = begin_write(&c, &store.revision_state).unwrap();
    let r = store.revision_state.active_revision.load(Ordering::SeqCst);
    let mut i = sample();
    i.source = source;
    i.expected = r - 1;
    let operation = i.id().unwrap();
    let asset = Uuid::now_v7();
    let values: Vec<Value> = vec![
        operation.clone().into(),
        i.nonce.to_string().into(),
        i.digest().unwrap().into(),
        i.canonical().unwrap().into(),
        1_i64.into(),
        i.db_path.clone().into(),
        i.db_dev.to_string().into(),
        i.db_ino.to_string().into(),
        ACTOR.to_owned().into(),
        i64::from(i.actor_uid).into(),
        source.to_string().into(),
        "enroll_legacy@1".to_owned().into(),
        "Unmanaged".to_owned().into(),
        0_i64.into(),
        i.expected.into(),
        i.base.clone().into(),
        "LegacyEnrolled".to_owned().into(),
        "legacy".to_owned().into(),
        1_i64.into(),
        asset.to_string().into(),
        r.into(),
    ];
    let indexes = [0, 1, 2, 3, 5, 6, 7, 9, 10, 14, 15, 19, 20, 4, 8];
    let location: Option<String> = tx
        .query_row(
            "SELECT current_location FROM logical_sources WHERE id=?1",
            [source.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    let mut owner = owner_values(asset, source, &operation, r, "legacy", 1);
    owner[5] = location.clone().map_or(Value::Null, Value::Text);
    *store.ownership_state.permit.lock().unwrap() = Some(Permit {
        receipt: indexes.iter().map(|&n| values[n].clone()).collect(),
        owner,
    });
    let _scope = PermitScope(&store.ownership_state);
    let sql="INSERT INTO asset_control_intents(operation_id,nonce,digest,canonical_request,request_version,db_path,db_dev,db_ino,actor_kind,actor_uid,source_id,action,expected_state,expected_epoch,expected_revision,base_fingerprint,intended_state,owner,epoch,asset_id,committed_revision) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)";
    for index in 0..values.len() {
        let mut changed = values.clone();
        changed[index] = match &values[index] {
            Value::Text(s) => Value::Text(format!("{s}changed")),
            Value::Integer(n) => Value::Integer(n + 1),
            Value::Blob(b) => {
                let mut b = b.clone();
                b.push(0);
                Value::Blob(b)
            }
            _ => panic!("unexpected fixture type"),
        };
        assert!(
            tx.execute(sql, rusqlite::params_from_iter(changed))
                .is_err(),
            "unbound receipt column index {index}"
        );
    }
    tx.execute(sql, rusqlite::params_from_iter(values)).unwrap();
    tx.execute(
        "INSERT INTO asset_ownership VALUES(?1,?2,'legacy',1,?3,?4,?5,?5)",
        rusqlite::params![
            asset.to_string(),
            source.to_string(),
            location,
            operation,
            r
        ],
    )
    .unwrap();
    for table in [
        "asset_ownership",
        "asset_control_intents",
        "asset_ownership_history",
    ] {
        assert!(tx.execute(&format!("DELETE FROM {table}"), []).is_err());
        let column = if table == "asset_control_intents" {
            "digest"
        } else {
            "owner"
        };
        assert!(tx
            .execute(&format!("UPDATE {table} SET {column}={column}"), [])
            .is_err());
    }
    tx.commit().unwrap();
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM asset_control_intents", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn enrolled_source_cannot_change_representation_under_ordinary_recording_authority() {
    let (store, source) = fixture();
    let c = store.connection().unwrap();
    let tx = begin_write(&c, &store.revision_state).unwrap();
    let r = store.revision_state.active_revision.load(Ordering::SeqCst);
    let asset = Uuid::now_v7();
    let location = source_base(&tx, source).unwrap().location;
    let mut owner = owner_values(asset, source, "identity-guard", r, "legacy", 1);
    owner[5] = location.map_or(Value::Null, Value::Text);
    *store.ownership_state.permit.lock().unwrap() = Some(Permit {
        receipt: Vec::new(),
        owner,
    });
    let scope = PermitScope(&store.ownership_state);
    tx.execute(
        "INSERT INTO asset_ownership VALUES(?1,?2,'legacy',1,?3,'identity-guard',?4,?4)",
        rusqlite::params![
            asset.to_string(),
            source.to_string(),
            source_base(&tx, source).unwrap().location,
            r
        ],
    )
    .unwrap();
    drop(scope); // only ordinary M6 recording authority remains for the following edits.
    for assignment in [
        "kind='document'",
        "kind='manual'",
        "repository_id='forged'",
        "commit_oid='forged'",
        "git_path='forged'",
    ] {
        assert!(
            tx.execute(
                &format!("UPDATE logical_sources SET {assignment} WHERE id=?1"),
                [source.to_string()]
            )
            .is_err(),
            "{assignment}"
        );
    }
    tx.execute(
        "UPDATE logical_sources SET state='absent',current_location='/moved' WHERE id=?1",
        [source.to_string()],
    )
    .unwrap();
    assert!(store.ownership_state.permit.lock().unwrap().is_none());
}

#[test]
fn initialized_database_recognition_accepts_supported_older_prefixes_only() {
    for version in [1, 7, 11, 12] {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,applied_at INTEGER NOT NULL)").unwrap();
        // Preflight requires stable migration-one columns, not post-upgrade additions.
        c.execute_batch(MIGRATIONS[0].1).unwrap();
        for v in 1..=version {
            c.execute("INSERT INTO schema_migrations VALUES(?1,0)", [v])
                .unwrap();
        }
        require_initialized_database(&c).unwrap();
    }
    for sabotage in [
        "DELETE FROM schema_migrations",
        "DELETE FROM schema_migrations WHERE version=1",
        "INSERT INTO schema_migrations VALUES(99,0)",
        "DROP TABLE memory_spaces",
        "ALTER TABLE spaces RENAME COLUMN description TO wrong",
    ] {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,applied_at INTEGER NOT NULL); INSERT INTO schema_migrations VALUES(1,0); INSERT INTO schema_migrations VALUES(2,0)").unwrap();
        c.execute_batch(MIGRATIONS[0].1).unwrap();
        c.execute_batch(sabotage).unwrap();
        assert!(require_initialized_database(&c).is_err(), "{sabotage}");
    }
}

#[test]
fn git_manual_and_document_identities_are_not_legacy_files() {
    let (store, source) = fixture();
    let conn = store.connection().unwrap();
    let tx = begin_write(&conn, &store.revision_state).unwrap();
    for kind in ["git", "manual", "document"] {
        tx.execute(
            "UPDATE logical_sources SET kind=?1 WHERE id=?2",
            rusqlite::params![kind, source.to_string()],
        )
        .unwrap();
        assert!(source_base(&tx, source).is_err(), "{kind}");
    }
    assert!(store.ownership_state.permit.lock().unwrap().is_none());
    assert_eq!(
        tx.query_row("SELECT COUNT(*) FROM asset_ownership", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn ordinary_recording_cannot_replace_the_schema_cutover_marker() {
    for recursive in [false, true] {
        let (store, _) = fixture();
        let conn = store.connection().unwrap();
        conn.pragma_update(None, "recursive_triggers", recursive)
            .unwrap();
        let original: i64 = conn.query_row("SELECT revision_id FROM m6_row_availability WHERE table_name='asset_ownership_schema'", [], |r|r.get(0)).unwrap();
        let tx = begin_write(&conn, &store.revision_state).unwrap();
        let r = store.revision_state.active_revision.load(Ordering::SeqCst);
        for verb in ["INSERT OR REPLACE", "REPLACE"] {
            assert!(tx.execute(&format!("{verb} INTO m6_row_availability VALUES('asset_ownership_schema','local-enroll@1',?1)"), [r]).is_err(), "{verb} recursive={recursive}");
        }
        assert_eq!(tx.query_row("SELECT revision_id FROM m6_row_availability WHERE table_name='asset_ownership_schema'", [], |r|r.get::<_,i64>(0)).unwrap(), original);
        assert!(store.ownership_state.permit.lock().unwrap().is_none());
    }
}

#[test]
fn ordinary_recording_cannot_replace_an_enrolled_source_representation() {
    for recursive in [false, true] {
        let (store, source) = fixture();
        let conn = store.connection().unwrap();
        conn.pragma_update(None, "recursive_triggers", recursive)
            .unwrap();
        let tx = begin_write(&conn, &store.revision_state).unwrap();
        let r = store.revision_state.active_revision.load(Ordering::SeqCst);
        let asset = Uuid::now_v7();
        let base = source_base(&tx, source).unwrap();
        let mut owner = owner_values(asset, source, "replace-guard", r, "legacy", 1);
        owner[5] = base.location.clone().map_or(Value::Null, Value::Text);
        *store.ownership_state.permit.lock().unwrap() = Some(Permit {
            receipt: Vec::new(),
            owner,
        });
        let scope = PermitScope(&store.ownership_state);
        tx.execute(
            "INSERT INTO asset_ownership VALUES(?1,?2,'legacy',1,?3,'replace-guard',?4,?4)",
            rusqlite::params![asset.to_string(), source.to_string(), base.location, r],
        )
        .unwrap();
        drop(scope);
        for (kind, git_path) in [
            ("document", None),
            ("manual", None),
            ("file", Some("forged")),
        ] {
            assert!(tx.execute("INSERT OR REPLACE INTO logical_sources(id,kind,state,current_location,created_at,repository_id,commit_oid,git_path) SELECT id,?1,state,current_location,created_at,NULL,NULL,?2 FROM logical_sources WHERE id=?3", rusqlite::params![kind,git_path,source.to_string()]).is_err(), "{kind} recursive={recursive}");
        }
        assert_eq!(
            source_base(&tx, source).unwrap().fingerprint,
            base.fingerprint
        );
        assert!(store.ownership_state.permit.lock().unwrap().is_none());
    }
}
