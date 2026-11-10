//! Real populated schema-13 cutover, rollback and uncertain-commit recovery.
use super::ownership_migration_tests::old_store;
use super::*;

fn revision(conn: &Connection) -> i64 {
    conn.query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| {
        r.get(0)
    })
    .unwrap()
}

fn snapshot(conn: &Connection) -> BTreeMap<String, Vec<String>> {
    let tables: Vec<String> = conn.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE 'asset_fence_%' AND name NOT LIKE 'document_preparation_%' AND name NOT LIKE 'doc_stage%' AND name NOT LIKE 'doc_semantic_%' AND name NOT IN ('schema_migrations','m6_revisions','m6_row_availability') ORDER BY name").unwrap().query_map([], |r|r.get(0)).unwrap().collect::<std::result::Result<_,_>>().unwrap();
    tables
        .into_iter()
        .map(|table| {
            let columns: Vec<String> = conn
                .prepare(&format!("PRAGMA table_info(\"{table}\")"))
                .unwrap()
                .query_map([], |r| r.get(1))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            let values = columns
                .iter()
                .map(|c| format!("quote(\"{c}\")"))
                .collect::<Vec<_>>()
                .join("||'|'||");
            let mut rows: Vec<String> = conn
                .prepare(&format!("SELECT {values} FROM \"{table}\""))
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            rows.sort();
            (table, rows)
        })
        .collect()
}

#[test]
fn populated_thirteen_preserves_owner_receipts_documents_indexes_and_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mind.db");
    let old = old_store(&path, 13);
    old.create_space("notes", "unchanged").unwrap();
    let file = dir.path().join("document.md");
    let text = "# Preparation\r\n\r\nquartz migration marker 漢字🦉\r\n";
    std::fs::write(&file, text).unwrap();
    crate::ingest::ingest_file(&old, &file, &["notes"]).unwrap();
    let source: String = old
        .connection()
        .unwrap()
        .query_row(
            "SELECT id FROM logical_sources WHERE kind='file'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let asset = ownership::enroll_test_source(&old, Uuid::parse_str(&source).unwrap());
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
    old.record_document_extraction(draft).unwrap();
    let assertion: String = old
        .connection()
        .unwrap()
        .query_row("SELECT id FROM assertions LIMIT 1", [], |r| r.get(0))
        .unwrap();
    old.save_embeddings(&[AssertionEmbedding {
        assertion_id: Uuid::parse_str(&assertion).unwrap(),
        derivation_key: "migration-fixture".into(),
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
    let caps = old.temporal_history_capabilities().unwrap();
    let (before, r) = {
        let c = old.connection().unwrap();
        (snapshot(&c), revision(&c))
    };
    drop(old);
    let upgraded = SqliteStore::open(&path).unwrap();
    assert_eq!(upgraded.search(&q).unwrap(), hits);
    let after_caps = upgraded.temporal_history_capabilities().unwrap();
    assert_eq!(after_caps.baseline_revision, caps.baseline_revision);
    assert_eq!(
        serde_json::to_value(after_caps.dimensions).unwrap(),
        serde_json::to_value(caps.dimensions).unwrap()
    );
    let c = upgraded.connection().unwrap();
    assert_eq!(snapshot(&c), before);
    assert_eq!(revision(&c), r + 4);
    assert_eq!(c.query_row("SELECT revision_id FROM m6_row_availability WHERE table_name='document_preparation_schema' AND entity_key='local-doc-prepare@1'",[],|r|r.get::<_,i64>(0)).unwrap(),r+1);
    assert_eq!(
        c.query_row(
            "SELECT owner FROM asset_ownership WHERE asset_id=?1",
            [asset.to_string()],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "legacy"
    );
    for table in [
        "document_preparation_enrollments",
        "document_preparation_receipts",
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
    let again = SqliteStore::open(&path).unwrap();
    assert_eq!(revision(&again.connection().unwrap()), r + 4);
}

#[test]
fn late_fourteen_ddl_failure_or_veto_leaves_thirteen_exact_and_scope_cleared() {
    for mode in ["late_ddl", "commit_veto"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mind.db");
        let old = old_store(&path, 13);
        old.create_space("preserve", "old").unwrap();
        let c = old.connection().unwrap();
        if mode == "late_ddl" {
            c.execute_batch("CREATE TABLE document_preparation_receipts(sentinel TEXT); INSERT INTO document_preparation_receipts VALUES('preserve')").unwrap();
        }
        let r = revision(&c);
        let before = snapshot(&c);
        if mode == "commit_veto" {
            c.commit_hook(Some(|| true)).unwrap();
        }
        let error = run_migrations(&c, &old.revision_state).unwrap_err();
        assert!(
            !matches!(error, Error::CommitOutcomeUnknown { .. }),
            "{mode}: {error}"
        );
        assert_eq!(revision(&c), r);
        assert_eq!(snapshot(&c), before);
        assert_eq!(
            c.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            13
        );
        assert_eq!(
            c.query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name='document_preparation_enrollments'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            c.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(old.revision_state.active_revision.load(Ordering::SeqCst), 0);
        assert!(c.is_autocommit());
        if mode == "late_ddl" {
            assert_eq!(
                c.query_row(
                    "SELECT sentinel FROM document_preparation_receipts",
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
fn lost_fourteen_migration_ack_is_unknown_and_reopen_does_not_repeat_cutover() {
    if std::env::var_os("MINDSPACE_TEST_S3C_METADATA_CHILD").is_none() {
        let out=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","storage::sqlite::document_preparation_migration_tests::lost_fourteen_migration_ack_is_unknown_and_reopen_does_not_repeat_cutover","--nocapture"]).env("MINDSPACE_TEST_S3C_METADATA_CHILD","1").output().unwrap();
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mind.db");
    let old = old_store(&path, 13);
    old.create_space("preserve", "receipt").unwrap();
    let r = revision(&old.connection().unwrap());
    drop(old);
    gate::LOSE_ACK.store(true, Ordering::SeqCst);
    let error = SqliteStore::open(&path).err().expect("ack lost");
    assert!(
        matches!(error, Error::CommitOutcomeUnknown { .. }),
        "{error}"
    );
    for _ in 0..2 {
        let reopened = SqliteStore::open(&path).unwrap();
        let c = reopened.connection().unwrap();
        assert_eq!(revision(&c), r + 4);
        assert_eq!(c.query_row("SELECT COUNT(*) FROM m6_row_availability WHERE table_name='document_preparation_schema'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        assert_eq!(
            c.query_row(
                "SELECT COUNT(*) FROM document_preparation_receipts",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}

#[test]
fn every_preparation_trigger_and_schema_marker_is_required_on_open() {
    for attack in document_preparation::enforcement_names()
        .into_iter()
        .map(Some)
        .chain(std::iter::once(None))
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mind.db");
        drop(SqliteStore::open(&path).unwrap());
        let raw = Connection::open(&path).unwrap();
        match attack {
            Some(name) => raw
                .execute_batch(&format!("DROP TRIGGER \"{name}\""))
                .unwrap(),
            None => {
                let triggers:Vec<String>=raw.prepare("SELECT name FROM sqlite_schema WHERE type='trigger' AND (name LIKE 'm6_s3c_%' OR name='m6_fence_m6_row_availability_d')").unwrap().query_map([],|r|r.get(0)).unwrap().collect::<std::result::Result<_,_>>().unwrap();
                for name in triggers {
                    raw.execute_batch(&format!("DROP TRIGGER \"{name}\""))
                        .unwrap();
                }
                raw.execute("DELETE FROM m6_row_availability WHERE table_name='document_preparation_schema'",[]).unwrap();
            }
        }
        assert!(SqliteStore::open(&path).is_err());
    }
}
