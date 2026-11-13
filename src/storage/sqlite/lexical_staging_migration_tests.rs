//! Genuine schema-14 upgrade: populated ownership, preparation, document/chunk and retrieval state.
use super::ownership_migration_tests::old_store;
use super::*;

fn revision(c: &Connection) -> i64 {
    c.query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| {
        r.get(0)
    })
    .unwrap()
}
fn legacy_snapshot(c: &Connection) -> BTreeMap<String, Vec<String>> {
    let tables:Vec<String>=c.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE 'asset_fence_%' AND name NOT LIKE 'doc_stage%' AND name NOT LIKE 'doc_semantic_%' AND name NOT IN ('schema_migrations','m6_revisions','m6_row_availability') ORDER BY name").unwrap().query_map([],|r|r.get(0)).unwrap().collect::<std::result::Result<_,_>>().unwrap();
    tables
        .into_iter()
        .map(|t| {
            let columns: Vec<String> = c
                .prepare(&format!("PRAGMA table_info(\"{t}\")"))
                .unwrap()
                .query_map([], |r| r.get(1))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            let expr = columns
                .iter()
                .map(|p| format!("quote(\"{p}\")"))
                .collect::<Vec<_>>()
                .join("||'|'||");
            let mut rows: Vec<String> = c
                .prepare(&format!("SELECT {expr} FROM \"{t}\""))
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            rows.sort();
            (t, rows)
        })
        .collect()
}

#[test]
#[cfg(feature = "embeddings")]
fn populated_fourteen_upgrade_preserves_ownership_preparation_chunks_and_retrieval() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("mind.db");
    let old = old_store(&db, 14);
    old.create_space("notes", "preserve memberships").unwrap();
    let note = d.path().join("source.md");
    let text = "# Staging\r\n\r\nquartz migration marker 漢字🦉 e\u{301}.\r\n";
    std::fs::write(&note, text).unwrap();
    crate::ingest::ingest_file(&old, &note, &["notes"]).unwrap();
    let (f,fs):(String,String)=old.connection().unwrap().query_row("SELECT l.id,s.id FROM logical_sources l JOIN sources s ON s.logical_id=l.id WHERE l.kind='file'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    let asset = ownership::enroll_test_source(&old, Uuid::parse_str(&f).unwrap());
    let draft = crate::ingest::document::extract_document_bytes(
        note.to_str().unwrap(),
        text.as_bytes(),
        Some("md"),
        &Default::default(),
        &Default::default(),
        Ts::now(),
    )
    .unwrap()
    .draft;
    let budget = crate::embeddings::budget::TokenBudget::from_file(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nomic-tokenizer.json"),
        &crate::embeddings::manifest::NOMIC_V1_5,
    )
    .unwrap();
    let report = old.record_chunked_document(draft, &budget).unwrap();
    document_preparation::enroll_test_preparation(
        &old,
        &document_preparation::PreparationTarget {
            asset_id: asset,
            legacy_source_version: Uuid::parse_str(&fs).unwrap(),
            document_id: report.logical_id,
            document_source_version: report.source_id,
            extraction_id: report.extraction_id,
        },
    );
    let assertion: String = old
        .connection()
        .unwrap()
        .query_row("SELECT id FROM assertions LIMIT 1", [], |r| r.get(0))
        .unwrap();
    old.save_embeddings(&[AssertionEmbedding {
        assertion_id: Uuid::parse_str(&assertion).unwrap(),
        derivation_key: "migration-staging-fixture".into(),
        model: "fixture".into(),
        model_version: "1".into(),
        dimension: 2,
        embedding: vec![1.0, 0.0],
    }])
    .unwrap();
    let q = SearchQuery {
        text: "quartz".into(),
        space_ids: None,
        limit: 20,
        at: None,
        include_superseded: false,
        include_retired: false,
    };
    let hits = old.search(&q).unwrap();
    assert!(!hits.is_empty());
    let vectors = old
        .search_vectors(&[1., 0.], &q, "migration-staging-fixture")
        .unwrap();
    assert!(!vectors.is_empty());
    let caps = old.temporal_history_capabilities().unwrap();
    let chunks = old.list_document_chunks(report.extraction_id).unwrap();
    let (before, r) = {
        let c = old.connection().unwrap();
        (legacy_snapshot(&c), revision(&c))
    };
    drop(old);
    let upgraded = SqliteStore::open(&db).unwrap();
    assert_eq!(upgraded.search(&q).unwrap(), hits);
    assert_eq!(
        upgraded
            .search_vectors(&[1., 0.], &q, "migration-staging-fixture")
            .unwrap(),
        vectors
    );
    assert_eq!(
        upgraded.list_document_chunks(report.extraction_id).unwrap(),
        chunks
    );
    let after_caps = upgraded.temporal_history_capabilities().unwrap();
    assert_eq!(after_caps.baseline_revision, caps.baseline_revision);
    assert_eq!(
        serde_json::to_value(after_caps.dimensions).unwrap(),
        serde_json::to_value(caps.dimensions).unwrap()
    );
    let c = upgraded.connection().unwrap();
    assert_eq!(legacy_snapshot(&c), before);
    assert_eq!(revision(&c), r + 3);
    assert_eq!(c.query_row("SELECT revision_id FROM m6_row_availability WHERE table_name='lexical_staging_schema' AND entity_key='local-doc-stage-lexical@1'",[],|r|r.get::<_,i64>(0)).unwrap(),r+1);
    for table in [
        "doc_staged_lexical_sets",
        "doc_staged_entries",
        "doc_stage_receipts",
    ] {
        assert_eq!(
            c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    drop(c);
    drop(upgraded);
    let again = SqliteStore::open(&db).unwrap();
    assert_eq!(revision(&again.connection().unwrap()), r + 3);
}

#[test]
fn late_fifteen_failure_and_commit_veto_preserve_fourteen_exactly() {
    for failure in ["late_ddl", "commit_veto"] {
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("mind.db");
        let old = old_store(&db, 14);
        old.create_space("preserve", "metadata").unwrap();
        let mut c = old.connection().unwrap();
        if failure == "late_ddl" {
            c.execute_batch("CREATE TABLE doc_stage_receipts(sentinel TEXT); INSERT INTO doc_stage_receipts VALUES('preserve')").unwrap();
        }
        let r = revision(&c);
        let before = legacy_snapshot(&c);
        if failure == "commit_veto" {
            c.guard
                .as_mut()
                .unwrap()
                .commit_hook(Some(|| true))
                .unwrap();
        }
        let e = run_migrations(&c, &old.revision_state).unwrap_err();
        assert!(!matches!(e, Error::CommitOutcomeUnknown { .. }), "{e}");
        assert_eq!(revision(&c), r);
        assert_eq!(legacy_snapshot(&c), before);
        assert_eq!(
            c.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            14
        );
        assert_eq!(
            c.query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name='doc_staged_lexical_sets'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(old.revision_state.active_revision.load(Ordering::SeqCst), 0);
        assert!(c.is_autocommit());
        assert_eq!(
            c.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        if failure == "late_ddl" {
            assert_eq!(
                c.query_row("SELECT sentinel FROM doc_stage_receipts", [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                "preserve"
            );
        }
    }
}

#[test]
#[cfg(feature = "spawn-gate")]
fn lost_fifteen_ack_is_unknown_and_reopen_spends_no_second_cutover() {
    if std::env::var_os("MINDSPACE_S3D_METADATA_CHILD").is_none() {
        let out=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","storage::sqlite::lexical_staging_migration_tests::lost_fifteen_ack_is_unknown_and_reopen_spends_no_second_cutover","--nocapture"]).env("MINDSPACE_S3D_METADATA_CHILD","1").output().unwrap();
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("mind.db");
    let old = old_store(&db, 14);
    old.create_space("legacy", "preserve").unwrap();
    let r = revision(&old.connection().unwrap());
    drop(old);
    gate::LOSE_ACK.store(true, Ordering::SeqCst);
    let e = SqliteStore::open(&db).err().expect("lost metadata ACK");
    assert!(matches!(e, Error::CommitOutcomeUnknown { .. }), "{e}");
    for _ in 0..2 {
        let s = SqliteStore::open(&db).unwrap();
        let c = s.connection().unwrap();
        assert_eq!(revision(&c), r + 3);
        assert_eq!(c.query_row("SELECT COUNT(*) FROM m6_row_availability WHERE table_name='lexical_staging_schema'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM doc_stage_receipts", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn missing_lexical_trigger_or_marker_fails_open() {
    for attack in lexical_staging::enforcement_names()
        .into_iter()
        .map(Some)
        .chain(std::iter::once(None))
    {
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("mind.db");
        drop(SqliteStore::open(&db).unwrap());
        let raw = Connection::open(&db).unwrap();
        if let Some(name) = attack {
            raw.execute_batch(&format!("DROP TRIGGER \"{name}\""))
                .unwrap();
        } else {
            let names:Vec<String>=raw.prepare("SELECT name FROM sqlite_schema WHERE type='trigger' AND (name LIKE 'm6_s3d_%' OR name='m6_fence_m6_row_availability_d')").unwrap().query_map([],|r|r.get(0)).unwrap().collect::<std::result::Result<_,_>>().unwrap();
            for name in names {
                raw.execute_batch(&format!("DROP TRIGGER \"{name}\""))
                    .unwrap();
            }
            raw.execute(
                "DELETE FROM m6_row_availability WHERE table_name='lexical_staging_schema'",
                [],
            )
            .unwrap();
        }
        assert!(SqliteStore::open(&db).is_err());
    }
}

#[test]
fn concurrent_fifteen_migration_installs_one_cutover() {
    if let Some(db) = std::env::var_os("MINDSPACE_S3D_MIGRATION_CHILD_DB") {
        drop(SqliteStore::open(Path::new(&db)).unwrap());
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("mind.db");
    let old = old_store(&db, 14);
    old.create_space("legacy", "preserve").unwrap();
    let r = revision(&old.connection().unwrap());
    drop(old);
    let children=(0..6).map(|_|std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","storage::sqlite::lexical_staging_migration_tests::concurrent_fifteen_migration_installs_one_cutover","--nocapture"]).env("MINDSPACE_S3D_MIGRATION_CHILD_DB",&db).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap()).collect::<Vec<_>>();
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let store = SqliteStore::open(&db).unwrap();
    let c = store.connection().unwrap();
    assert_eq!(revision(&c), r + 3);
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM m6_row_availability WHERE table_name='lexical_staging_schema'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}
