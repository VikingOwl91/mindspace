//! Passive schema17 uses the actual bundled engine and leaves old ordinary state alone.
use super::ownership_migration_tests::old_store;
use super::*;

fn revision(c: &Connection) -> i64 {
    c.query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| {
        r.get(0)
    })
    .unwrap()
}
fn ordinary_rows(c: &Connection) -> BTreeMap<String, Vec<String>> {
    let names: Vec<String> = c.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE 'asset_fence_%' AND name NOT IN ('schema_migrations','m6_revisions','m6_row_availability') ORDER BY name").unwrap().query_map([],|r|r.get(0)).unwrap().collect::<std::result::Result<_,_>>().unwrap();
    names
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
                .map(|col| format!("quote(\"{col}\")"))
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
fn populate(old: &SqliteStore, root: &Path) {
    old.create_space("notes", "passive compatibility").unwrap();
    for (name, text) in [
        ("a.md", "# First\r\n\r\nquartz primary 漢字🦉 e\u{301}.\r\n"),
        ("b.md", "# Second\n\nquartz distinct knowledge.\n"),
    ] {
        let file = root.join(name);
        std::fs::write(&file, text).unwrap();
        crate::ingest::ingest_file(old, &file, &["notes"]).unwrap();
    }
    let (f,fs):(String,String)=old.connection().unwrap().query_row("SELECT l.id,s.id FROM logical_sources l JOIN sources s ON s.logical_id=l.id WHERE l.kind='file' ORDER BY l.id LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    let asset = ownership::enroll_test_source(old, Uuid::parse_str(&f).unwrap());
    let file = old
        .connection()
        .unwrap()
        .query_row("SELECT location FROM sources WHERE id=?1", [&fs], |r| {
            r.get::<_, String>(0)
        })
        .unwrap();
    let bytes = std::fs::read(&file).unwrap();
    let draft = crate::ingest::document::extract_document_bytes(
        &file,
        &bytes,
        Some("md"),
        &Default::default(),
        &Default::default(),
        Ts::now(),
    )
    .unwrap()
    .draft;
    let d = old.record_document_extraction(draft).unwrap();
    document_preparation::enroll_test_preparation(
        old,
        &document_preparation::PreparationTarget {
            asset_id: asset,
            legacy_source_version: Uuid::parse_str(&fs).unwrap(),
            document_id: d.logical_id,
            document_source_version: d.source_id,
            extraction_id: d.extraction_id,
        },
    );
}

// These verified fixture assets and vectors exercise ranking plumbing, not model inference.
const MODEL_BYTES: &[u8] = b"synthetic model artifact for retrieval tests\n";
const TOKENIZER_BYTES: &[u8] = b"{\"synthetic\":true}\n";
static MODEL: crate::embeddings::manifest::ModelManifestEntry =
    crate::embeddings::manifest::ModelManifestEntry {
        id: "passive-migration-fixture",
        version: "1",
        revision: "test",
        architecture: "synthetic",
        runtime: "fixture",
        runtime_version: "1",
        dimension: 4,
        max_seq_len: 512,
        truncation: "none",
        prefix_document: "search_document: ",
        prefix_query: "search_query: ",
        pooling: "mean",
        normalization: "l2",
        license: "test-only",
        source: "synthetic",
        artifacts: &[
            crate::embeddings::manifest::Artifact {
                filename: "model.safetensors",
                url: "https://example.invalid/model",
                sha256: "269cbb1a5ed49786cc52cfa6e29a80d5147d461ca878f9c84651c947bf5f750f",
                size: MODEL_BYTES.len() as u64,
            },
            crate::embeddings::manifest::Artifact {
                filename: "tokenizer.json",
                url: "https://example.invalid/tokenizer",
                sha256: "cb8daed7b30399a1c3c8b83b3b7bee4b774a30fc389afc1d375e4c23a3cc4ae8",
                size: TOKENIZER_BYTES.len() as u64,
            },
        ],
    };
struct QueryFixture;
impl crate::embeddings::EmbeddingRuntime for QueryFixture {
    fn embed_document(&self, _: &str) -> Result<Vec<f32>> {
        panic!("dormant migration must never embed a document")
    }
    fn embed_query(&self, _: &str) -> Result<Vec<f32>> {
        Ok(vec![1., 0., 0., 0.])
    }
    fn dimension(&self) -> usize {
        4
    }
    fn model_id(&self) -> &str {
        MODEL.id
    }
}
fn install_vectors(old: &SqliteStore, base: &Path) {
    let dir = crate::embeddings::manager::model_dir_in(base, MODEL.id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("model.safetensors"), MODEL_BYTES).unwrap();
    std::fs::write(dir.join("tokenizer.json"), TOKENIZER_BYTES).unwrap();
    old.set_meta("embedding.enabled", "true").unwrap();
    old.set_meta("embedding.model", MODEL.id).unwrap();
    let ids: Vec<String> = old
        .connection()
        .unwrap()
        .prepare("SELECT id FROM assertions ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    let key = crate::embeddings::manifest::derivation_from_manifest(&MODEL).derivation_key();
    for (n, id) in ids.iter().enumerate() {
        old.save_embeddings(&[AssertionEmbedding {
            assertion_id: Uuid::parse_str(id).unwrap(),
            derivation_key: key.clone(),
            model: MODEL.id.into(),
            model_version: MODEL.version.into(),
            dimension: 4,
            embedding: if n == 0 {
                vec![1., 0., 0., 0.]
            } else {
                vec![0.8, 0.6, 0., 0.]
            },
        }])
        .unwrap();
    }
}
fn outcomes(store: &SqliteStore, base: &Path, at: i64) -> Vec<serde_json::Value> {
    use crate::retrieval::{self, RetrievalOptions, SearchMode};
    let capability = crate::retrieval::semantic::resolve_with(
        store,
        base,
        |id| (id == MODEL.id).then_some(&MODEL),
        &|_, _| Ok(Box::new(QueryFixture)),
    )
    .unwrap();
    assert!(matches!(
        &capability,
        crate::retrieval::SemanticCapability::Ready(_)
    ));
    let mut results = Vec::new();
    for timestamp in [None, Some(Ts(at))] {
        for mode in [
            SearchMode::Lexical,
            SearchMode::Semantic,
            SearchMode::Hybrid,
            SearchMode::Auto,
        ] {
            let q = SearchQuery {
                text: "quartz".into(),
                space_ids: None,
                limit: 20,
                at: timestamp,
                include_superseded: false,
                include_retired: false,
            };
            let result = retrieval::search_with_mode(
                store,
                q,
                mode,
                &capability,
                RetrievalOptions::default(),
            )
            .unwrap();
            assert!(!result.hits.is_empty());
            results.push(serde_json::to_value(result).unwrap());
        }
    }
    results
}

#[test]
fn populated_sixteen_upgrade_preserves_all_modes_scores_indexes_and_preparation() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("mind.db");
    let base = d.path().join("models");
    let old = old_store(&db, 16);
    populate(&old, d.path());
    install_vectors(&old, &base);
    let at = Ts::now().0 + 1;
    let before = outcomes(&old, &base, at);
    let mut capabilities =
        serde_json::to_value(old.temporal_history_capabilities().unwrap()).unwrap();
    capabilities
        .as_object_mut()
        .unwrap()
        .remove("current_revision");
    let (rows, r, availability) = {
        let c = old.connection().unwrap();
        (
            ordinary_rows(&c),
            revision(&c),
            c.query_row("SELECT count(*) FROM m6_row_availability", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
        )
    };
    let new = SqliteStore::open(&db).unwrap();
    assert_eq!(outcomes(&new, &base, at), before);
    let mut after_capabilities =
        serde_json::to_value(new.temporal_history_capabilities().unwrap()).unwrap();
    assert_eq!(after_capabilities["current_revision"], r + 1);
    after_capabilities
        .as_object_mut()
        .unwrap()
        .remove("current_revision");
    assert_eq!(after_capabilities, capabilities);
    {
        let c = new.connection().unwrap();
        assert_eq!(ordinary_rows(&c), rows);
        assert_eq!(revision(&c), r + 1);
        assert_eq!(
            c.query_row("SELECT count(*) FROM m6_row_availability", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            availability + 1
        );
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
        assert_eq!(c.query_row("SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND tbl_name NOT LIKE 'asset_fence_%' AND sql LIKE '%s3e2_%'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    }
    // This handle has only v16 UDFs: new private guards must not affect ordinary writes.
    old.create_space("old-still-writable", "same old connection")
        .unwrap();
    crate::ingest::ingest_file(&old, &d.path().join("a.md"), &["notes"]).unwrap();
    old.set_meta("passive-positive-control", "unrelated")
        .unwrap();
    assert!(!old
        .search(&SearchQuery {
            text: "quartz".into(),
            space_ids: None,
            limit: 20,
            at: None,
            include_superseded: false,
            include_retired: false
        })
        .unwrap()
        .is_empty());
}

#[test]
fn late_seventeen_failure_and_commit_veto_leave_sixteen_and_legacy_writable() {
    for fault in ["late_ddl", "veto"] {
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("mind.db");
        let old = old_store(&db, 16);
        old.create_space("legacy", "").unwrap();
        let c = old.connection().unwrap();
        let r = revision(&c);
        if fault == "late_ddl" {
            c.execute_batch("CREATE TABLE asset_fence_plan_receipts(sentinel TEXT); INSERT INTO asset_fence_plan_receipts VALUES('preserve')").unwrap();
        } else {
            c.commit_hook(Some(|| true)).unwrap();
        }
        let rows = ordinary_rows(&c);
        let e = run_migrations(&c, &old.revision_state).unwrap_err();
        assert!(!matches!(e, Error::CommitOutcomeUnknown { .. }), "{e}");
        assert_eq!(revision(&c), r);
        assert_eq!(ordinary_rows(&c), rows);
        assert_eq!(
            c.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            16
        );
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='asset_fence_plans'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        c.commit_hook(None::<fn() -> bool>).unwrap();
        drop(c);
        old.create_space("after-failed-migration", "").unwrap();
    }
}

#[test]
fn old_deferred_snapshot_and_prepared_statement_survive_dormant_migration_honestly() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("mind.db");
    let old = old_store(&db, 16);
    old.create_space("before", "").unwrap();
    let c = old.connection().unwrap();
    let mut cached = c.prepare_cached("SELECT count(*) FROM memories").unwrap();
    assert_eq!(cached.query_row([], |r| r.get::<_, i64>(0)).unwrap(), 0);
    let tx = Transaction::new_unchecked(&c, TransactionBehavior::Deferred).unwrap();
    assert_eq!(
        tx.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        16
    );
    let new = SqliteStore::open(&db).unwrap();
    assert_eq!(
        tx.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        16
    );
    let e = tx
        .execute("INSERT INTO schema_migrations VALUES(18,0)", [])
        .unwrap_err();
    assert!(
        matches!(e,rusqlite::Error::SqliteFailure(code,_) if code.extended_code==rusqlite::ffi::SQLITE_BUSY_SNAPSHOT),
        "{e}"
    );
    tx.rollback().unwrap();
    assert_eq!(
        c.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        17
    );
    assert_eq!(cached.query_row([], |r| r.get::<_, i64>(0)).unwrap(), 0);
    drop(cached);
    drop(c);
    old.create_space("new-transaction", "").unwrap();
    new.create_space("new-client", "").unwrap();
}

#[test]
fn false_when_unknown_udf_is_not_an_inert_old_writer_guard_on_bundled_sqlite() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("mind.db");
    let old = old_store(&db, 16);
    old.create_space("before", "").unwrap();
    let _new = SqliteStore::open(&db).unwrap();
    {
        let c = old.connection().unwrap();
        c.execute_batch("CREATE TABLE empty_activation_probe(id INTEGER PRIMARY KEY); CREATE TRIGGER hypothetical_bad_shared_guard BEFORE INSERT ON m6_row_availability WHEN EXISTS(SELECT 1 FROM empty_activation_probe) BEGIN SELECT unavailable_s3e2_guard(); END").unwrap();
    }
    let file = d.path().join("probe.md");
    std::fs::write(&file, "# Probe\n\nquartz shared metadata guard\n").unwrap();
    let before = ordinary_rows(&old.connection().unwrap());
    let e = crate::ingest::ingest_file(&old, &file, &["before"]).unwrap_err();
    assert!(e.to_string().contains("no such function"), "{e}");
    assert_eq!(ordinary_rows(&old.connection().unwrap()), before);
    {
        let c = old.connection().unwrap();
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM spaces WHERE name='must-not-partially-commit'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        c.execute_batch("DROP TRIGGER hypothetical_bad_shared_guard")
            .unwrap();
    }
    old.create_space("safe-again", "").unwrap();
    crate::ingest::ingest_file(&old, &file, &["before"]).unwrap();
}

#[test]
fn lost_migration_ack_reopens_once_without_consuming_another_schema_revision() {
    const CHILD: &str = "MINDSPACE_S3E2_MIGRATION_ACK_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let out=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","storage::sqlite::fence_plan_migration_tests::lost_migration_ack_reopens_once_without_consuming_another_schema_revision","--nocapture"]).env(CHILD,"1").output().unwrap();
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
    let old = old_store(&db, 16);
    old.create_space("legacy", "").unwrap();
    let r = revision(&old.connection().unwrap());
    drop(old);
    gate::LOSE_ACK.store(true, Ordering::SeqCst);
    let e = SqliteStore::open(&db)
        .err()
        .expect("lost migration acknowledgement");
    assert!(matches!(e, Error::CommitOutcomeUnknown { .. }), "{e}");
    for _ in 0..2 {
        let reopened = SqliteStore::open(&db).unwrap();
        let c = reopened.connection().unwrap();
        assert_eq!(revision(&c), r + 1);
        assert_eq!(
            c.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            17
        );
    }
}
