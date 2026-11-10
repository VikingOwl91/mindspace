#![cfg(feature = "embeddings")]

#[path = "support/schema_fixture.rs"]
mod schema_fixture;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Barrier, RwLock};

use mindspace::core::{DocumentExtractionDraft, Error, Ts};
use mindspace::embeddings::{budget::TokenBudget, manifest::NOMIC_V1_5};
use mindspace::ingest::document::{extract_document_bytes, DocumentIngestOptions, Identity};
use mindspace::storage::Store;
use mindspace::{initialize, SqliteStore};

static HOOKS: RwLock<()> = RwLock::new(());

fn budget() -> TokenBudget {
    TokenBudget::from_file(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nomic-tokenizer.json")
            .as_path(),
        &NOMIC_V1_5,
    )
    .unwrap()
}
fn draft(location: &str, text: &str) -> DocumentExtractionDraft {
    extract_document_bytes(
        location,
        text.as_bytes(),
        Some("md"),
        &DocumentIngestOptions::default(),
        &Identity::default(),
        Ts::now(),
    )
    .unwrap()
    .draft
}
struct Db {
    _t: tempfile::TempDir,
    path: PathBuf,
}
impl Db {
    fn new() -> Self {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("mind.db");
        initialize(&path).unwrap();
        Self { _t: t, path }
    }
    fn open(&self) -> SqliteStore {
        SqliteStore::open(&self.path).unwrap()
    }
    fn raw(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.path).unwrap()
    }
    fn counts(&self) -> BTreeMap<String, i64> {
        let c = self.raw();
        let names: Vec<String> = c.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap().query_map([], |r| r.get(0)).unwrap().collect::<std::result::Result<_,_>>().unwrap();
        names
            .into_iter()
            .map(|name| {
                let n = c
                    .query_row(&format!("SELECT COUNT(*) FROM \"{name}\""), [], |r| {
                        r.get(0)
                    })
                    .unwrap();
                (name, n)
            })
            .collect()
    }
}

#[test]
fn chunk_sets_are_atomic_idempotent_and_citations_quote_exact_source_bytes() {
    let _lock = HOOKS.read().unwrap();
    let db = Db::new();
    let store = db.open();
    let b = budget();
    let text = format!(
        "# One\r\n\r\n{}\r\n# Two\r\n\r\nlast 👩‍💻 e\u{301} \u{202e}bidi\u{202c}\r\n",
        "日本語 words ".repeat(1600)
    );
    let d = draft("/source.md", &text);
    let first = store.record_chunked_document(d.clone(), &b).unwrap();
    let chunks = store.list_document_chunks(first.extraction_id).unwrap();
    assert!(chunks.len() > 1);
    let mut offset = 0;
    for chunk in &chunks {
        assert_eq!(chunk.start, offset);
        assert!(chunk.end > chunk.start);
        assert_eq!(
            chunk.tokens,
            b.validate_document(&text[chunk.start as usize..chunk.end as usize])
                .unwrap()
        );
        let resolved = store
            .resolve_document_anchor(&chunk.anchor)
            .unwrap()
            .unwrap();
        assert_eq!(
            resolved.text.as_deref(),
            Some(&text[chunk.start as usize..chunk.end as usize])
        );
        offset = chunk.end;
    }
    assert_eq!(offset, text.len() as u64);
    let before = db.counts();
    let second = store.record_chunked_document(d, &b).unwrap();
    assert_eq!(second.extraction_id, first.extraction_id);
    assert!(!second.extraction_created);
    assert_eq!(before, db.counts());
    assert_eq!(
        chunks,
        store.list_document_chunks(first.extraction_id).unwrap()
    );
    for table in ["assertions", "evidence", "memories", "assertion_embeddings"] {
        assert_eq!(db.counts()[table], 0);
    }
}

#[test]
fn existing_s2_extraction_can_gain_chunks_once_without_replacing_anything() {
    let _lock = HOOKS.read().unwrap();
    let db = Db::new();
    let store = db.open();
    let b = budget();
    let d = draft("/source.md", "# Old\n\ncontent\n");
    let s2 = store.record_document_extraction(d.clone()).unwrap();
    let before = db.counts();
    let s3 = store.record_chunked_document(d.clone(), &b).unwrap();
    assert_eq!(s2.extraction_id, s3.extraction_id);
    assert!(!s3.extraction_created);
    let after = db.counts();
    for table in [
        "logical_sources",
        "sources",
        "doc_extractions",
        "doc_nodes",
        "memories",
        "assertions",
        "evidence",
    ] {
        assert_eq!(before[table], after[table], "{table}");
    }
    assert_eq!(after["m6_revisions"], before["m6_revisions"] + 1);
    store.record_chunked_document(d, &b).unwrap();
    assert_eq!(after, db.counts());
}

#[test]
fn a_middle_chunk_insert_failure_rolls_back_the_extraction_and_recording_revision() {
    let _lock = HOOKS.read().unwrap();
    let db = Db::new();
    let store = db.open();
    let b = budget();
    db.raw().execute_batch("CREATE TRIGGER fail_chunk BEFORE INSERT ON doc_chunks WHEN NEW.ordinal = 1 BEGIN SELECT RAISE(ABORT, 'injected middle chunk'); END;").unwrap();
    let before = db.counts();
    let d = draft("/source.md", &"paragraph word.\n\n".repeat(3000));
    let error = store.record_chunked_document(d.clone(), &b).unwrap_err();
    assert!(
        error.to_string().contains("injected middle chunk"),
        "{error}"
    );
    assert_eq!(before, db.counts());
    db.raw().execute_batch("DROP TRIGGER fail_chunk").unwrap();
    let retry = store.record_chunked_document(d, &b).unwrap();
    assert!(retry.extraction_created);
    assert!(
        store
            .list_document_chunks(retry.extraction_id)
            .unwrap()
            .len()
            > 1
    );
}

#[test]
fn vetoed_commit_is_rolled_back_and_unknown_commit_is_idempotent_on_retry() {
    let _lock = HOOKS.write().unwrap();
    let db = Db::new();
    let store = db.open();
    let b = budget();
    let d = draft("/source.md", "# Header\n\ncontent\n");
    let before = db.counts();
    mindspace::storage::sqlite::gate::FAIL_COMMIT.store(true, std::sync::atomic::Ordering::SeqCst);
    let error = store.record_chunked_document(d.clone(), &b).unwrap_err();
    assert!(!matches!(error, Error::CommitOutcomeUnknown { .. }));
    assert_eq!(before, db.counts());
    mindspace::storage::sqlite::gate::LOSE_ACK.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(matches!(
        store.record_chunked_document(d.clone(), &b),
        Err(Error::CommitOutcomeUnknown { .. })
    ));
    let committed = db.counts();
    assert_eq!(committed["doc_chunk_sets"], 1);
    assert!(committed["doc_chunks"] >= 1);
    drop(store);
    let retry = db.open().record_chunked_document(d, &b).unwrap();
    assert!(!retry.extraction_created);
    assert_eq!(committed, db.counts());
}

#[test]
fn concurrent_connections_create_one_chunk_set_and_consume_one_recording_revision() {
    let _lock = HOOKS.read().unwrap();
    let db = Db::new();
    let b = budget();
    let d = draft("/source.md", &"# section\n\nwords.\n\n".repeat(100));
    let before = db.counts();
    let barrier = Barrier::new(6);
    let reports = std::thread::scope(|scope| {
        let mut threads = Vec::new();
        for _ in 0..6 {
            let (path, b, d, barrier) = (&db.path, &b, &d, &barrier);
            threads.push(scope.spawn(move || {
                let store = SqliteStore::open(path).unwrap();
                barrier.wait();
                store.record_chunked_document(d.clone(), b).unwrap()
            }));
        }
        threads
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(reports.iter().filter(|r| r.extraction_created).count(), 1);
    assert!(reports
        .iter()
        .all(|r| r.extraction_id == reports[0].extraction_id));
    let after = db.counts();
    assert_eq!(after["m6_revisions"], before["m6_revisions"] + 1);
    assert_eq!(after["doc_chunk_sets"], 1);
}

#[test]
fn different_documents_and_extractor_versions_have_distinct_chunks_and_reverts_reuse() {
    let _lock = HOOKS.read().unwrap();
    let db = Db::new();
    let store = db.open();
    let b = budget();
    let original = draft("/source.md", "# First\n\ntext\n");
    let first = store.record_chunked_document(original.clone(), &b).unwrap();
    let first_chunks = store.list_document_chunks(first.extraction_id).unwrap();
    let mut other = original.clone();
    other.location = "/other.md".into();
    let second = store.record_chunked_document(other, &b).unwrap();
    let second_chunks = store.list_document_chunks(second.extraction_id).unwrap();
    assert_ne!(first.logical_id, second.logical_id);
    assert_ne!(first_chunks[0].id, second_chunks[0].id);
    let mut new_extractor = original.clone();
    new_extractor.extractor_version = "2".into();
    let third = store.record_chunked_document(new_extractor, &b).unwrap();
    assert_eq!(first.source_id, third.source_id);
    assert_ne!(first.extraction_id, third.extraction_id);
    assert_ne!(
        first_chunks[0].id,
        store.list_document_chunks(third.extraction_id).unwrap()[0].id
    );
    let changed = store
        .record_chunked_document(draft("/source.md", "# Changed\n\nnew text\n"), &b)
        .unwrap();
    assert_ne!(first.source_id, changed.source_id);
    let before = db.counts();
    let reverted = store.record_chunked_document(original, &b).unwrap();
    assert_eq!(first.extraction_id, reverted.extraction_id);
    assert_eq!(before, db.counts());
    assert_eq!(
        first_chunks,
        store.list_document_chunks(reverted.extraction_id).unwrap()
    );
}

#[test]
fn citations_survive_source_replacement_deletion_and_wrong_extractors_do_not_resolve() {
    let _lock = HOOKS.read().unwrap();
    let db = Db::new();
    let store = db.open();
    let b = budget();
    let file = db._t.path().join("original.md");
    let text = "# Original\n\nold source\n";
    std::fs::write(&file, text).unwrap();
    let report = mindspace::ingest::document::ingest_chunked_document_file(
        &store,
        &file,
        &DocumentIngestOptions::default(),
        &b,
    )
    .unwrap();
    let c = store
        .list_document_chunks(report.extraction_id)
        .unwrap()
        .remove(0);
    std::fs::write(&file, "new source").unwrap();
    store
        .record_chunked_document(draft(file.to_str().unwrap(), "new source"), &b)
        .unwrap();
    assert_eq!(
        store
            .resolve_document_anchor(&c.anchor)
            .unwrap()
            .unwrap()
            .text
            .as_deref(),
        Some(&text[c.start as usize..c.end as usize])
    );
    std::fs::remove_file(file).unwrap();
    assert_eq!(
        store
            .resolve_document_anchor(&c.anchor)
            .unwrap()
            .unwrap()
            .text
            .as_deref(),
        Some(&text[c.start as usize..c.end as usize])
    );
    let mut wrong = c.anchor;
    wrong.extractor_version = "999".into();
    assert!(store.resolve_document_anchor(&wrong).unwrap().is_none());
}

#[test]
fn chunk_storage_is_immutable_and_upgrade_from_populated_schema_eleven_preserves_anchors() {
    let _lock = HOOKS.read().unwrap();
    let db = Db::new();
    let store = db.open();
    let b = budget();
    let d = draft("/source.md", "# Title\n\nwords\n");
    let original = store.record_chunked_document(d.clone(), &b).unwrap();
    let chunks = store.list_document_chunks(original.extraction_id).unwrap();
    for sql in [
        "UPDATE doc_chunks SET token_count=1",
        "DELETE FROM doc_chunks",
        "UPDATE doc_chunk_sets SET chunk_count=99",
        "DELETE FROM doc_chunk_sets",
    ] {
        assert!(db.raw().execute(sql, []).is_err(), "{sql}");
    }
    drop(store);
    schema_fixture::remove_empty_ownership_metadata(&db.raw());
    db.raw().execute_batch("DROP TABLE doc_chunks; DROP TABLE doc_chunk_sets; DELETE FROM schema_migrations WHERE version=12").unwrap();
    let before = db.counts();
    let store = db.open();
    let after = db.counts();
    for table in ["logical_sources", "sources", "doc_extractions", "doc_nodes"] {
        assert_eq!(before[table], after[table]);
    }
    assert_eq!(after["m6_revisions"], before["m6_revisions"] + 3);
    assert_eq!(
        store
            .resolve_document_anchor(&chunks[0].anchor)
            .unwrap()
            .unwrap()
            .text
            .as_deref(),
        Some(&d.canonical_text[chunks[0].start as usize..chunks[0].end as usize])
    );
    store.record_chunked_document(d, &b).unwrap();
    assert_eq!(
        store.list_document_chunks(original.extraction_id).unwrap(),
        chunks
    );
    assert_eq!(
        db.raw()
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        0
    );
}

#[test]
fn corrupted_completed_rows_are_refused_without_spending_another_revision() {
    let _lock = HOOKS.read().unwrap();
    let db = Db::new();
    let store = db.open();
    let b = budget();
    let d = draft("/source.md", "words\n");
    store.record_chunked_document(d.clone(), &b).unwrap();
    db.raw().execute_batch("DROP TRIGGER doc_chunks_no_update; UPDATE doc_chunks SET token_count = token_count + 1;").unwrap();
    let before = db.counts();
    assert!(matches!(
        store.record_chunked_document(d, &b),
        Err(Error::InvalidDocument(_))
    ));
    assert_eq!(db.counts(), before);
}

#[test]
fn empty_chunk_plan_has_a_completion_marker_and_repeat_consumes_no_revision() {
    let _lock = HOOKS.read().unwrap();
    let db = Db::new();
    let store = db.open();
    let b = budget();
    let mut d = draft("/empty.md", "placeholder");
    d.canonical_text.clear();
    d.nodes.clear();
    d.source_size = 0;
    d.source_bytes_hash = mindspace::core::sha256_hex(b"");
    let first = store.record_chunked_document(d.clone(), &b).unwrap();
    assert!(store
        .list_document_chunks(first.extraction_id)
        .unwrap()
        .is_empty());
    let before = db.counts();
    assert_eq!(before["doc_chunk_sets"], 1);
    let second = store.record_chunked_document(d, &b).unwrap();
    assert_eq!(first.extraction_id, second.extraction_id);
    assert_eq!(db.counts(), before);
}

#[test]
fn legacy_ingestion_and_explicit_chunks_never_create_competing_memories_in_either_order() {
    let _lock = HOOKS.read().unwrap();
    for documents_first in [false, true] {
        let db = Db::new();
        let store = db.open();
        let b = budget();
        let root = db._t.path().join("files");
        std::fs::create_dir(&root).unwrap();
        let path = root.join("notes.md");
        let text = "# Zebra\n\nlong lived zebras.\n";
        std::fs::write(&path, text).unwrap();
        let d = draft(path.to_str().unwrap(), text);
        store.create_space("p", "").unwrap();
        if documents_first {
            store.record_chunked_document(d.clone(), &b).unwrap();
        }
        mindspace::ingest::ingest_path(&store, &root, &["p"]).unwrap();
        let before = db.counts();
        store.record_chunked_document(d, &b).unwrap();
        let after = db.counts();
        for table in ["memories", "evidence", "assertions", "assertion_embeddings"] {
            assert_eq!(before[table], after[table], "{table}");
        }
        let families: Vec<String> = db
            .raw()
            .prepare("SELECT DISTINCT producer_family FROM memories ORDER BY producer_family")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(families, vec!["file:legacy"]);
    }
}

#[test]
fn prior_chunker_version_set_is_retained_when_current_version_is_derived() {
    let _lock = HOOKS.read().unwrap();
    let db = Db::new();
    let store = db.open();
    let b = budget();
    let d = draft("/source.md", "# Title\n\ntext\n");
    let s2 = store.record_document_extraction(d.clone()).unwrap();
    // Synthetic prior-version fixture: same source ranges but an older identity rule without
    // the strategy tag. The current version must coexist without colliding or reusing its rows.
    let plan = mindspace::ingest::document::chunk::chunk_document(&d, &b).unwrap();
    let raw = db.raw();
    raw.execute("INSERT INTO doc_chunk_sets (extraction_id,version,budget_identity,plan_hash,chunk_count) VALUES (?1, 'heading_aware@0', ?2, ?3, ?4)", rusqlite::params![s2.extraction_id.to_string(), b.identity(), mindspace::core::sha256_hex(b"synthetic old plan"), plan.chunks.len() as i64]).unwrap();
    let mut old_ids = Vec::new();
    for (ordinal, c) in plan.chunks.iter().enumerate() {
        let old_id = mindspace::core::sha256_hex(
            serde_json::json!([
                s2.logical_id.to_string(),
                d.source_bytes_hash,
                d.text_hash(),
                d.detector_version,
                d.extractor_id,
                d.extractor_version,
                d.params_hash,
                d.structure_hash(),
                b.identity(),
                c.start,
                c.end,
                c.context,
            ])
            .to_string()
            .as_bytes(),
        );
        let anchor = mindspace::core::Anchor::new(
            s2.logical_id,
            &d.source_bytes_hash,
            &d.detector_version,
            &d.extractor_id,
            &d.extractor_version,
            &d.params_hash,
            vec![],
            Some((c.start, c.end)),
            None,
            None,
            mindspace::core::AnchorPrecision::ByteExact,
        )
        .unwrap();
        raw.execute("INSERT INTO doc_chunks (id,extraction_id,version,budget_identity,ordinal,text_start,text_end,token_count,context_json,anchor) VALUES (?1,?2,'heading_aware@0',?3,?4,?5,?6,?7,?8,?9)",rusqlite::params![old_id,s2.extraction_id.to_string(),b.identity(),ordinal as i64,c.start as i64,c.end as i64,c.tokens as i64,serde_json::to_string(&c.context).unwrap(),anchor.to_string()]).unwrap();
        old_ids.push(old_id);
    }
    store.record_chunked_document(d.clone(), &b).unwrap();
    assert_eq!(db.counts()["doc_chunk_sets"], 2);
    let rows = store.list_document_chunks(s2.extraction_id).unwrap();
    assert_eq!(rows.len(), 2 * plan.chunks.len());
    for id in old_ids {
        assert_eq!(rows.iter().filter(|c| c.id == id).count(), 1);
    }
    assert_eq!(
        rows.iter().filter(|c| c.version == plan.version).count(),
        plan.chunks.len()
    );
    let before = db.counts();
    store.record_chunked_document(d, &b).unwrap();
    assert_eq!(before, db.counts());
}

#[test]
#[ignore = "release resource measurement, run explicitly"]
fn measure_one_mib_chunk_storage_overhead() {
    let _lock = HOOKS.read().unwrap();
    let db = Db::new();
    let store = db.open();
    let b = budget();
    let unit = "# Section\n\nrepresentative English and German Wörter with 日本語.\n\n";
    let text = unit.repeat((1024 * 1024) / unit.len());
    let d = draft("/measurement.md", &text);
    let first = store.record_document_extraction(d.clone()).unwrap();
    let size = |db: &Db| {
        let c = db.raw();
        c.query_row("SELECT (SELECT page_count FROM pragma_page_count) * (SELECT page_size FROM pragma_page_size)", [], |r| r.get::<_,i64>(0)).unwrap()
    };
    let before = size(&db);
    let began = std::time::Instant::now();
    store.record_chunked_document(d, &b).unwrap();
    let elapsed = began.elapsed();
    let after = size(&db);
    let chunks = store.list_document_chunks(first.extraction_id).unwrap();
    println!("chunk-storage input_bytes={} chunks={} encoded_tokens={} derivation_and_write_ms={} document_db_bytes={} chunk_overhead_bytes={}", text.len(), chunks.len(), chunks.iter().map(|c| c.tokens).sum::<usize>(), elapsed.as_millis(), before, after - before);
}

#[test]
fn migration_twelve_failure_after_first_table_creation_rolls_back_the_whole_upgrade() {
    let _lock = HOOKS.read().unwrap();
    let db = Db::new();
    let store = db.open();
    store
        .record_document_extraction(draft("/source.md", "retained source\n"))
        .unwrap();
    drop(store);
    schema_fixture::remove_empty_ownership_metadata(&db.raw());
    db.raw().execute_batch("DROP TABLE doc_chunks; DROP TABLE doc_chunk_sets; DELETE FROM schema_migrations WHERE version=12; CREATE TABLE doc_chunks (sentinel TEXT); INSERT INTO doc_chunks VALUES ('preserve me');").unwrap();
    let schema = |db: &Db| -> Vec<(String, String)> {
        db.raw()
            .prepare("SELECT type || ':' || name, COALESCE(sql, '') FROM sqlite_master ORDER BY 1")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    };
    let before = db.counts();
    let before_schema = schema(&db);
    let err = match SqliteStore::open(&db.path) {
        Ok(_) => panic!("migration must fail"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("doc_chunks"), "{err}");
    assert_eq!(schema(&db), before_schema);
    assert_eq!(db.counts(), before);
    assert_eq!(
        db.raw()
            .query_row("SELECT sentinel FROM doc_chunks", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "preserve me"
    );
    db.raw().execute_batch("DROP TABLE doc_chunks").unwrap();
    let store = db.open();
    assert_eq!(db.counts()["doc_extractions"], 1);
    assert!(store
        .list_document_chunks(mindspace::core::Uuid::nil())
        .unwrap()
        .is_empty());
    assert_eq!(
        db.raw()
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        15
    );
}

#[test]
fn invalid_section_hierarchy_is_refused_before_any_persistence() {
    let _lock = HOOKS.read().unwrap();
    let db = Db::new();
    let store = db.open();
    let b = budget();
    let mut d = draft("/bad.md", "# Heading\n\ntext\n");
    let first = d
        .nodes
        .iter()
        .find(|n| n.kind == mindspace::core::NodeKind::Section)
        .unwrap()
        .clone();
    let mut sibling = first.clone();
    sibling.path = vec![99];
    d.nodes = vec![first, sibling];
    d.validate().unwrap();
    let before = db.counts();
    assert!(matches!(
        store.record_chunked_document(d, &b),
        Err(Error::InvalidDocument(_))
    ));
    assert_eq!(before, db.counts());
}

#[test]
fn chunk_range_trigger_uses_byte_metadata_and_rejects_out_of_bounds_unicode_ranges() {
    let _lock = HOOKS.read().unwrap();
    let db = Db::new();
    let store = db.open();
    let b = budget();
    let d = draft("/unicode.md", "漢字🦉e\u{301}\r\n");
    let report = store.record_chunked_document(d.clone(), &b).unwrap();
    let raw = db.raw();
    let chunks = store.list_document_chunks(report.extraction_id).unwrap();
    let c = &chunks[0];
    // Inspect the actual INSERT trigger bytecode, not a separate lookalike SELECT.
    let sql = "INSERT INTO doc_chunks (id,extraction_id,version,budget_identity,ordinal,text_start,text_end,token_count,context_json,anchor) VALUES (?1,?2,?3,?4,99,0,?5,1,'[]',?6)";
    let new_id = mindspace::core::sha256_hex(b"test metadata byte length");
    let args = rusqlite::params![
        new_id,
        report.extraction_id.to_string(),
        c.version,
        c.budget_identity,
        d.canonical_text.len() as i64 + 1,
        c.anchor.to_string()
    ];
    let ops: Vec<(String, Option<String>, i64)> = raw
        .prepare(&format!("EXPLAIN {sql}"))
        .unwrap()
        .query_map(args, |r| Ok((r.get(1)?, r.get(5)?, r.get(6)?)))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert!(ops
        .iter()
        .any(|(op, p4, _)| (op == "Function" || op == "PureFunc")
            && p4
                .as_deref()
                .is_some_and(|s| s.starts_with("octet_length("))));
    assert!(
        ops.iter()
            .any(|(op, _, p5)| op == "Column" && p5 & 0xc0 == 0xc0),
        "canonical text must be read as byte-length metadata"
    );
    let err = raw
        .execute(
            sql,
            rusqlite::params![
                new_id,
                report.extraction_id.to_string(),
                c.version,
                c.budget_identity,
                d.canonical_text.len() as i64 + 1,
                c.anchor.to_string()
            ],
        )
        .unwrap_err();
    assert!(err.to_string().contains("outside canonical text"), "{err}");
    assert_eq!(
        raw.query_row("SELECT octet_length(?1)", [&d.canonical_text], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        d.canonical_text.len() as i64
    );
}
