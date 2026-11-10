//! Genuine populated schema-15 upgrade, rollback, acknowledgement and writer evidence.
use super::ownership_migration_tests::old_store;
use super::*;
fn revision(c: &Connection) -> i64 {
    c.query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| {
        r.get(0)
    })
    .unwrap()
}
fn old_rows(c: &Connection) -> BTreeMap<String, Vec<String>> {
    let tables:Vec<String>=c.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT IN ('schema_migrations','m6_revisions','m6_row_availability','doc_staged_semantic_sets','doc_staged_vectors','doc_semantic_stage_receipts') ORDER BY name").unwrap().query_map([],|r|r.get(0)).unwrap().collect::<std::result::Result<_,_>>().unwrap();
    tables
        .into_iter()
        .map(|t| {
            let cols: Vec<String> = c
                .prepare(&format!("PRAGMA table_info(\"{t}\")"))
                .unwrap()
                .query_map([], |r| r.get(1))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            let expr = cols
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
fn populated_fifteen_upgrade_preserves_lexical_receipts_citations_vectors_and_ownership() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("mind.db");
    let old = old_store(&db, 15);
    old.create_space("notes", "").unwrap();
    let file = d.path().join("source.md");
    let text = "# Semantic migration\r\n\r\nquartz knowledge 漢字 e\u{301} 🦉\u{202e}\u{200b}.\r\n";
    std::fs::write(&file, text).unwrap();
    crate::ingest::ingest_file(&old, &file, &["notes"]).unwrap();
    let (f,fs):(String,String)=old.connection().unwrap().query_row("SELECT l.id,s.id FROM logical_sources l JOIN sources s ON s.logical_id=l.id WHERE l.kind='file'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    let asset = ownership::enroll_test_source(&old, Uuid::parse_str(&f).unwrap());
    let budget = crate::embeddings::budget::TokenBudget::from_file(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nomic-tokenizer.json"),
        &crate::embeddings::manifest::NOMIC_V1_5,
    )
    .unwrap();
    let draft = crate::ingest::document::extract_document_bytes(
        file.to_str().unwrap(),
        text.as_bytes(),
        Some("md"),
        &Default::default(),
        &Default::default(),
        Ts::now(),
    )
    .unwrap()
    .draft;
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
    let plan: String = old
        .connection()
        .unwrap()
        .query_row(
            "SELECT plan_hash FROM doc_chunk_sets WHERE extraction_id=?1",
            [report.extraction_id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    let target = lexical_input::LexicalStageTarget {
        asset_id: asset,
        legacy_source_version: Uuid::parse_str(&fs).unwrap(),
        document_id: report.logical_id,
        document_source_version: report.source_id,
        extraction_id: report.extraction_id,
        chunker_version: "heading_aware@1".into(),
        budget_identity: budget.identity().into(),
        plan_hash: plan,
    };
    let stage = lexical_staging::enroll_test_lexical(&old, &target);
    let assertion: String = old
        .connection()
        .unwrap()
        .query_row("SELECT id FROM assertions LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let mut vector = vec![0.; 768];
    vector[0] = 1.;
    old.save_embeddings(&[AssertionEmbedding {
        assertion_id: Uuid::parse_str(&assertion).unwrap(),
        derivation_key: "migration-semantic-fixture".into(),
        model: "fixture".into(),
        model_version: "1".into(),
        dimension: 768,
        embedding: vector.clone(),
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
        .search_vectors(&vector, &q, "migration-semantic-fixture")
        .unwrap();
    assert!(!vectors.is_empty());
    let chunks = old.list_document_chunks(report.extraction_id).unwrap();
    let cite = old.resolve_document_anchor(&chunks[0].anchor).unwrap();
    let caps = old.temporal_history_capabilities().unwrap();
    let (before, r) = {
        let c = old.connection().unwrap();
        (old_rows(&c), revision(&c))
    };
    drop(old);
    let new = SqliteStore::open(&db).unwrap();
    assert_eq!(new.search(&q).unwrap(), hits);
    assert_eq!(
        new.search_vectors(&vector, &q, "migration-semantic-fixture")
            .unwrap(),
        vectors
    );
    assert_eq!(
        new.list_document_chunks(report.extraction_id).unwrap(),
        chunks
    );
    assert_eq!(
        new.resolve_document_anchor(&chunks[0].anchor).unwrap(),
        cite
    );
    let after_caps = new.temporal_history_capabilities().unwrap();
    assert_eq!(after_caps.baseline_revision, caps.baseline_revision);
    assert_eq!(
        serde_json::to_value(after_caps.dimensions).unwrap(),
        serde_json::to_value(caps.dimensions).unwrap()
    );
    let c = new.connection().unwrap();
    assert_eq!(old_rows(&c), before);
    assert_eq!(revision(&c), r + 1);
    assert_eq!(c.query_row("SELECT revision_id FROM m6_row_availability WHERE table_name='semantic_staging_schema' AND entity_key='local-doc-stage-semantic@1'",[],|r|r.get::<_,i64>(0)).unwrap(),r+1);
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM doc_staged_lexical_sets WHERE stage_id=?1",
            [stage],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    for t in [
        "doc_staged_semantic_sets",
        "doc_staged_vectors",
        "doc_semantic_stage_receipts",
    ] {
        assert_eq!(
            c.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(
        c.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
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
    drop(new);
    let again = SqliteStore::open(&db).unwrap();
    assert_eq!(revision(&again.connection().unwrap()), r + 1);
}
#[test]
fn late_sixteen_ddl_and_commit_veto_preserve_fifteen() {
    for failure in ["late_ddl", "veto"] {
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("mind.db");
        let old = old_store(&db, 15);
        old.create_space("preserve", "").unwrap();
        let c = old.connection().unwrap();
        if failure == "late_ddl" {
            c.execute_batch("CREATE TABLE doc_semantic_stage_receipts(sentinel TEXT); INSERT INTO doc_semantic_stage_receipts VALUES('preserve')").unwrap();
        }
        let r = revision(&c);
        let before = old_rows(&c);
        if failure == "veto" {
            c.commit_hook(Some(|| true)).unwrap();
        }
        let e = run_migrations(&c, &old.revision_state).unwrap_err();
        assert!(!matches!(e, Error::CommitOutcomeUnknown { .. }), "{e}");
        assert_eq!(revision(&c), r);
        assert_eq!(old_rows(&c), before);
        assert_eq!(
            c.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            15
        );
        assert_eq!(
            c.query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name='doc_staged_semantic_sets'",
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
                c.query_row(
                    "SELECT sentinel FROM doc_semantic_stage_receipts",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                "preserve"
            );
        }
    }
}
#[test]
#[cfg(feature = "spawn-gate")]
fn lost_sixteen_ack_stays_unknown_and_reopen_is_idempotent() {
    if std::env::var_os("MINDSPACE_S3D2_METADATA_CHILD").is_none() {
        let o=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","storage::sqlite::semantic_staging_migration_tests::lost_sixteen_ack_stays_unknown_and_reopen_is_idempotent","--nocapture"]).env("MINDSPACE_S3D2_METADATA_CHILD","1").output().unwrap();
        assert!(
            o.status.success(),
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("mind.db");
    let old = old_store(&db, 15);
    old.create_space("legacy", "").unwrap();
    let r = revision(&old.connection().unwrap());
    drop(old);
    gate::LOSE_ACK.store(true, Ordering::SeqCst);
    let e = SqliteStore::open(&db).err().expect("lost ACK");
    assert!(matches!(e, Error::CommitOutcomeUnknown { .. }), "{e}");
    for _ in 0..2 {
        let new = SqliteStore::open(&db).unwrap();
        let c = new.connection().unwrap();
        assert_eq!(revision(&c), r + 1);
        assert_eq!(c.query_row("SELECT COUNT(*) FROM m6_row_availability WHERE table_name='semantic_staging_schema'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        assert_eq!(
            c.query_row(
                "SELECT COUNT(*) FROM doc_semantic_stage_receipts",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}
#[test]
fn every_semantic_trigger_and_marker_is_required_on_open() {
    for attack in semantic_staging::enforcement_names()
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
            let names:Vec<String>=raw.prepare("SELECT name FROM sqlite_schema WHERE type='trigger' AND (name LIKE 'm6_s3d2_%' OR name='m6_fence_m6_row_availability_d')").unwrap().query_map([],|r|r.get(0)).unwrap().collect::<std::result::Result<_,_>>().unwrap();
            for name in names {
                raw.execute_batch(&format!("DROP TRIGGER \"{name}\""))
                    .unwrap();
            }
            raw.execute(
                "DELETE FROM m6_row_availability WHERE table_name='semantic_staging_schema'",
                [],
            )
            .unwrap();
        }
        assert!(SqliteStore::open(&db).is_err());
    }
}
#[test]
fn six_processes_install_one_semantic_metadata_cutover() {
    if let Some(db) = std::env::var_os("MINDSPACE_S3D2_MIGRATION_CHILD_DB") {
        drop(SqliteStore::open(Path::new(&db)).unwrap());
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("mind.db");
    let old = old_store(&db, 15);
    old.create_space("legacy", "").unwrap();
    let r = revision(&old.connection().unwrap());
    drop(old);
    let children=(0..6).map(|_|std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","storage::sqlite::semantic_staging_migration_tests::six_processes_install_one_semantic_metadata_cutover","--nocapture"]).env("MINDSPACE_S3D2_MIGRATION_CHILD_DB",&db).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap()).collect::<Vec<_>>();
    for c in children {
        let o = c.wait_with_output().unwrap();
        assert!(
            o.status.success(),
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
    }
    let new = SqliteStore::open(&db).unwrap();
    let c = new.connection().unwrap();
    assert_eq!(revision(&c), r + 1);
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM m6_row_availability WHERE table_name='semantic_staging_schema'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}
