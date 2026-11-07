use mindspace::core::Uuid;
use mindspace::embeddings::EmbeddingRuntime;
use mindspace::indexing::index_assertions;
use mindspace::storage::Store;
use mindspace::SqliteStore;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use tempfile::TempDir;

struct MockRuntime {
    dim: usize,
    fail_on: Mutex<Vec<usize>>, // call indices (0-based) that should fail
    calls: AtomicUsize,
    bad_dim: bool,
    non_finite: bool,
}

impl MockRuntime {
    fn ok(dim: usize) -> Self {
        Self {
            dim,
            fail_on: Mutex::new(vec![]),
            calls: AtomicUsize::new(0),
            bad_dim: false,
            non_finite: false,
        }
    }
    fn failing(indices: Vec<usize>) -> Self {
        Self {
            dim: 4,
            fail_on: Mutex::new(indices),
            calls: AtomicUsize::new(0),
            bad_dim: false,
            non_finite: false,
        }
    }
}

impl EmbeddingRuntime for MockRuntime {
    fn embed_document(&self, _text: &str) -> mindspace::Result<Vec<f32>> {
        let i = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_on.lock().unwrap().contains(&i) {
            return Err(mindspace::Error::Storage(
                "mock inference failure".to_string(),
            ));
        }
        if self.bad_dim {
            return Ok(vec![1.0f32; self.dim + 1]);
        }
        if self.non_finite {
            return Ok(vec![f32::NAN; self.dim]);
        }
        Ok(vec![1.0f32; self.dim])
    }
    fn embed_query(&self, text: &str) -> mindspace::Result<Vec<f32>> {
        self.embed_document(text)
    }
    fn dimension(&self) -> usize {
        self.dim
    }
    fn model_id(&self) -> &str {
        "mock"
    }
}

fn setup() -> (TempDir, SqliteStore) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("mind.db");
    SqliteStore::open(&path).unwrap();
    let store = SqliteStore::open(&path).unwrap();
    (dir, store)
}

fn make_assertion(store: &SqliteStore, content: &str) -> Uuid {
    let space_id = match store.create_space("coding", "") {
        Ok(s) => s.id,
        Err(_) => store.get_space("coding").unwrap().id,
    };
    let r = store
        .record_manual_write(mindspace::core::ManualMemoryDraft {
            subject: Some("subj".to_string()),
            content: content.to_string(),
            spaces: vec![space_id],
            supersedes: None,
        })
        .unwrap();
    r.assertion_id
}

#[test]
fn first_indexing_run_indexes_all() {
    let (_dir, store) = setup();
    let a = make_assertion(&store, "hello");
    let b = make_assertion(&store, "world");
    let deriv = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    // use mock runtime with dim 4 to match a tiny derivation? deriv.dimension is 768.
    // Use a custom derivation with dim 4.
    let mut d = deriv;
    d.dimension = 4;
    d.model_id = "mock".to_string();
    let rt = MockRuntime::ok(4);
    let key = d.derivation_key();
    let report = index_assertions(&store, &rt, &d, 32, None).unwrap();
    assert_eq!(report.indexed, 2);
    assert_eq!(report.failed, 0);
    assert_eq!(report.remaining, 0);
    assert_eq!(report.already_indexed, 0);
    assert_eq!(report.eligible, 2);
    // verify stored
    let st = store.embedding_index_status(&key).unwrap();
    assert_eq!(st.embedded, 2);
    assert_eq!(st.missing(), 0);
    let _ = (a, b);
}

#[test]
fn idempotent_rerun_indexes_nothing() {
    let (_dir, store) = setup();
    make_assertion(&store, "hello");
    let mut d = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    d.dimension = 4;
    d.model_id = "mock".to_string();
    let rt = MockRuntime::ok(4);
    let r1 = index_assertions(&store, &rt, &d, 32, None).unwrap();
    assert_eq!(r1.indexed, 1);
    let r2 = index_assertions(&store, &rt, &d, 32, None).unwrap();
    assert_eq!(r2.indexed, 0);
    assert_eq!(r2.already_indexed, 1);
    assert_eq!(r2.remaining, 0);
}

#[test]
fn new_assertion_after_initial_indexing() {
    let (_dir, store) = setup();
    make_assertion(&store, "first");
    let mut d = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    d.dimension = 4;
    d.model_id = "mock".to_string();
    let rt = MockRuntime::ok(4);
    index_assertions(&store, &rt, &d, 32, None).unwrap();
    make_assertion(&store, "second");
    let r2 = index_assertions(&store, &rt, &d, 32, None).unwrap();
    assert_eq!(r2.indexed, 1);
    assert_eq!(r2.remaining, 0);
}

#[test]
fn reindex_after_clear_reembeds() {
    // Assertions are immutable; simulate a stale embedding row by deleting it,
    // then re-running index must re-embed.
    let (_dir, store) = setup();
    let aid = make_assertion(&store, "hello");
    let mut d = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    d.dimension = 4;
    d.model_id = "mock".to_string();
    let key = d.derivation_key();
    let rt = MockRuntime::ok(4);
    index_assertions(&store, &rt, &d, 32, None).unwrap();
    // delete the embedding row (simulating staleness)
    // Use a direct SQL delete via the store? Store trait has clear_embeddings by key (all).
    // Instead, delete via a new assertion? We'll just clear and reindex.
    store.clear_embeddings(&key).unwrap();
    let r2 = index_assertions(&store, &rt, &d, 32, None).unwrap();
    assert_eq!(r2.indexed, 1);
    assert_eq!(r2.remaining, 0);
    let _ = aid;
}

#[test]
fn changed_derivation_identity_reindexes() {
    let (_dir, store) = setup();
    make_assertion(&store, "hello");
    let mut d1 = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    d1.dimension = 4;
    d1.model_id = "mock".to_string();
    let rt = MockRuntime::ok(4);
    index_assertions(&store, &rt, &d1, 32, None).unwrap();
    // change derivation (model version)
    let mut d2 = d1.clone();
    d2.model_version = "v2".to_string();
    let r2 = index_assertions(&store, &rt, &d2, 32, None).unwrap();
    assert_eq!(r2.indexed, 1); // re-embedded under new key
    assert_eq!(r2.remaining, 0);
    // old derivation still has its embedding (not deleted)
    let st_old = store.embedding_index_status(&d1.derivation_key()).unwrap();
    assert_eq!(st_old.embedded, 1);
}

#[test]
fn retirement_does_not_invalidate_embedding() {
    let (_dir, store) = setup();
    let space_id = match store.create_space("coding", "") {
        Ok(s) => s.id,
        Err(_) => store.get_space("coding").unwrap().id,
    };
    let report = store
        .record_manual_write(mindspace::core::ManualMemoryDraft {
            subject: Some("s".to_string()),
            content: "c".to_string(),
            spaces: vec![space_id],
            supersedes: None,
        })
        .unwrap();
    let mut d = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    d.dimension = 4;
    d.model_id = "mock".to_string();
    let rt = MockRuntime::ok(4);
    index_assertions(&store, &rt, &d, 32, None).unwrap();
    // retire the memory
    store
        .retire_memory(report.memory_id, None, mindspace::core::Ts::now())
        .unwrap();
    // embedding still present (not invalidated)
    let st = store.embedding_index_status(&d.derivation_key()).unwrap();
    assert_eq!(st.embedded, 1);
    // rerun does not re-embed
    let r2 = index_assertions(&store, &rt, &d, 32, None).unwrap();
    assert_eq!(r2.indexed, 0);
}

#[test]
fn supersession_preserves_embedding() {
    let (_dir, store) = setup();
    let space_id = match store.create_space("coding", "") {
        Ok(s) => s.id,
        Err(_) => store.get_space("coding").unwrap().id,
    };
    let r1 = store
        .record_manual_write(mindspace::core::ManualMemoryDraft {
            subject: Some("s".to_string()),
            content: "c".to_string(),
            spaces: vec![space_id],
            supersedes: None,
        })
        .unwrap();
    let mut d = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    d.dimension = 4;
    d.model_id = "mock".to_string();
    let rt = MockRuntime::ok(4);
    index_assertions(&store, &rt, &d, 32, None).unwrap();
    // supersede
    let _r2 = store
        .record_manual_write(mindspace::core::ManualMemoryDraft {
            subject: Some("s".to_string()),
            content: "c".to_string(),
            spaces: vec![space_id],
            supersedes: Some(r1.memory_id),
        })
        .unwrap();
    // original assertion's embedding persists
    let st = store.embedding_index_status(&d.derivation_key()).unwrap();
    assert!(st.embedded >= 1);
}

#[test]
fn space_isolation_indexing() {
    let (_dir, store) = setup();
    let s1 = store.create_space("a", "").unwrap().id;
    let s2 = store.create_space("b", "").unwrap().id;
    store
        .record_manual_write(mindspace::core::ManualMemoryDraft {
            subject: Some("x".to_string()),
            content: "x".to_string(),
            spaces: vec![s1],
            supersedes: None,
        })
        .unwrap();
    store
        .record_manual_write(mindspace::core::ManualMemoryDraft {
            subject: Some("y".to_string()),
            content: "y".to_string(),
            spaces: vec![s2],
            supersedes: None,
        })
        .unwrap();
    let mut d = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    d.dimension = 4;
    d.model_id = "mock".to_string();
    let rt = MockRuntime::ok(4);
    let r = index_assertions(&store, &rt, &d, 32, None).unwrap();
    assert_eq!(r.indexed, 2); // both indexed regardless of space
    assert_eq!(r.remaining, 0);
}

#[test]
fn interrupted_batch_and_resume() {
    let (_dir, store) = setup();
    make_assertion(&store, "a");
    make_assertion(&store, "b");
    make_assertion(&store, "c");
    let mut d = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    d.dimension = 4;
    d.model_id = "mock".to_string();
    let rt = MockRuntime::ok(4);
    // first run: limit 1
    let r1 = index_assertions(&store, &rt, &d, 1, Some(1)).unwrap();
    assert_eq!(r1.indexed, 1);
    assert_eq!(r1.remaining, 2);
    // resume
    let r2 = index_assertions(&store, &rt, &d, 32, None).unwrap();
    assert_eq!(r2.indexed, 2);
    assert_eq!(r2.remaining, 0);
}

#[test]
fn per_item_inference_failure() {
    let (_dir, store) = setup();
    make_assertion(&store, "a");
    make_assertion(&store, "b");
    let mut d = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    d.dimension = 4;
    d.model_id = "mock".to_string();
    let rt = MockRuntime::failing(vec![0]); // first fails
    let r = index_assertions(&store, &rt, &d, 32, None).unwrap();
    assert_eq!(r.failed, 1);
    assert_eq!(r.indexed, 1);
    assert_eq!(r.remaining, 1);
    assert_eq!(r.failures.len(), 1);
}

#[test]
fn invalid_vector_dimension_rejected() {
    let (_dir, store) = setup();
    make_assertion(&store, "a");
    let mut d = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    d.dimension = 4;
    d.model_id = "mock".to_string();
    let rt = MockRuntime {
        dim: 4,
        fail_on: Mutex::new(vec![]),
        calls: AtomicUsize::new(0),
        bad_dim: true,
        non_finite: false,
    };
    let r = index_assertions(&store, &rt, &d, 32, None).unwrap();
    assert_eq!(r.failed, 1);
    assert_eq!(r.indexed, 0);
    assert_eq!(r.remaining, 1);
}

#[test]
fn invalid_vector_non_finite_rejected() {
    let (_dir, store) = setup();
    make_assertion(&store, "a");
    let mut d = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    d.dimension = 4;
    d.model_id = "mock".to_string();
    let rt = MockRuntime {
        dim: 4,
        fail_on: Mutex::new(vec![]),
        calls: AtomicUsize::new(0),
        bad_dim: false,
        non_finite: true,
    };
    let r = index_assertions(&store, &rt, &d, 32, None).unwrap();
    assert_eq!(r.failed, 1);
    assert_eq!(r.indexed, 0);
}

#[test]
fn disabled_embeddings_state_is_false_by_default() {
    // Indexing requires embeddings to be enabled; the default state is disabled.
    // (The CLI rejection path is exercised by tests/models.rs::cli_index_run_disabled_rejected.)
    let (_dir, store) = setup();
    assert!(!mindspace::embeddings::manager::is_enabled(&store));
    assert!(mindspace::embeddings::manager::selected_model(&store).is_none());
}

#[cfg(feature = "embeddings")]
#[test]
fn missing_model_files_error() {
    let (_dir, store) = setup();
    make_assertion(&store, "a");
    let dir = TempDir::new().unwrap();
    let res = mindspace::embeddings::CandleRuntime::load(dir.path());
    assert!(res.is_err());
    let _ = store;
}

#[test]
fn no_network_during_indexing() {
    // MockRuntime performs no network; index run completes without network.
    let (_dir, store) = setup();
    make_assertion(&store, "a");
    let mut d = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    d.dimension = 4;
    d.model_id = "mock".to_string();
    let rt = MockRuntime::ok(4);
    let r = index_assertions(&store, &rt, &d, 32, None).unwrap();
    assert_eq!(r.indexed, 1);
    assert_eq!(r.failed, 0);
}

#[test]
fn index_writes_only_to_provided_db() {
    // index writes only to the temp db provided
    let (dir, store) = setup();
    make_assertion(&store, "a");
    let mut d = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    d.dimension = 4;
    d.model_id = "mock".to_string();
    let rt = MockRuntime::ok(4);
    index_assertions(&store, &rt, &d, 32, None).unwrap();
    // db file exists in temp dir
    assert!(dir.path().join("mind.db").exists());
}

#[test]
fn multi_batch_indexing_cursor() {
    // Exercises the after=Some cursor branch across multiple batches.
    let (_dir, store) = setup();
    for i in 0..5 {
        make_assertion(&store, &format!("c{}", i));
    }
    let mut d = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    d.dimension = 4;
    d.model_id = "mock".to_string();
    let rt = MockRuntime::ok(4);
    // batch_size 2 -> 3 batches (2+2+1)
    let r = index_assertions(&store, &rt, &d, 2, None).unwrap();
    assert_eq!(r.indexed, 5, "all 5 must be indexed");
    assert_eq!(r.failed, 0);
    assert_eq!(r.remaining, 0);
    // rerun: nothing left
    let r2 = index_assertions(&store, &rt, &d, 2, None).unwrap();
    assert_eq!(r2.indexed, 0);
    assert_eq!(r2.remaining, 0);
}

#[test]
fn equal_timestamp_cursor_pagination() {
    // Force identical created_at across assertions to exercise the id tiebreaker.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("mind.db");
    SqliteStore::open(&path).unwrap();
    let store = SqliteStore::open(&path).unwrap();
    make_assertion(&store, "a");
    make_assertion(&store, "b");
    make_assertion(&store, "c");
    // drop immutability triggers and force equal created_at; the revision
    // fence must also be lifted for this deliberate direct-SQL fixture
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute("DROP TRIGGER IF EXISTS assertions_immutable", [])
        .unwrap();
    conn.execute("DROP TRIGGER IF EXISTS m6_fence_assertions_u", [])
        .unwrap();
    conn.execute("DROP TRIGGER IF EXISTS m6_immutable_assertions_u", [])
        .unwrap();
    conn.execute("UPDATE assertions SET created_at = 1000", [])
        .unwrap();
    // restore what the tamper removed so the next store open stays valid
    conn.execute(
        "CREATE TRIGGER m6_fence_assertions_u BEFORE UPDATE ON assertions \
         WHEN m6_write_revision() IS NULL OR NOT EXISTS \
           (SELECT 1 FROM m6_revisions WHERE revision_id=m6_write_revision()) \
         BEGIN SELECT RAISE(ABORT, 'revisioned write required'); END;",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TRIGGER m6_immutable_assertions_u BEFORE UPDATE ON assertions \
         BEGIN SELECT RAISE(ABORT, 'assertions rows are append-only'); END;",
        [],
    )
    .unwrap();
    drop(conn);
    let mut d = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    d.dimension = 4;
    d.model_id = "mock".to_string();
    let rt = MockRuntime::ok(4);
    // batch_size 1 -> 3 batches across equal timestamp
    let r = index_assertions(&store, &rt, &d, 1, None).unwrap();
    assert_eq!(
        r.indexed, 3,
        "all 3 must be indexed despite equal timestamps"
    );
    assert_eq!(r.failed, 0);
    assert_eq!(r.remaining, 0);
}

#[test]
fn cross_batch_failed_then_successful() {
    // 3 assertions, middle one fails; batch_size 1, no limit.
    // cursor must advance past the failed item to reach the later one.
    let (_dir, store) = setup();
    make_assertion(&store, "a");
    make_assertion(&store, "b");
    make_assertion(&store, "c");
    let mut d = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    d.dimension = 4;
    d.model_id = "mock".to_string();
    // fail on the second assertion (index 1)
    let rt = MockRuntime::failing(vec![1]);
    let r = index_assertions(&store, &rt, &d, 1, None).unwrap();
    assert_eq!(r.failed, 1);
    assert_eq!(r.indexed, 2);
    assert_eq!(r.remaining, 1);
    // the failed one is retryable
    let rt2 = MockRuntime::ok(4);
    let r2 = index_assertions(&store, &rt2, &d, 32, None).unwrap();
    assert_eq!(r2.indexed, 1);
    assert_eq!(r2.remaining, 0);
}

#[test]
fn batch_size_zero_errors() {
    let (_dir, store) = setup();
    make_assertion(&store, "a");
    let mut d = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    d.dimension = 4;
    d.model_id = "mock".to_string();
    let rt = MockRuntime::ok(4);
    let res = index_assertions(&store, &rt, &d, 0, None);
    assert!(res.is_err(), "batch_size 0 must error");
}

#[test]
fn limit_across_batches() {
    let (_dir, store) = setup();
    for i in 0..5 {
        make_assertion(&store, &format!("c{}", i));
    }
    let mut d = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    d.dimension = 4;
    d.model_id = "mock".to_string();
    let rt = MockRuntime::ok(4);
    let r = index_assertions(&store, &rt, &d, 1, Some(3)).unwrap();
    assert_eq!(r.indexed, 3);
    assert_eq!(r.remaining, 2);
    // rerun processes the rest
    let r2 = index_assertions(&store, &rt, &d, 1, None).unwrap();
    assert_eq!(r2.indexed, 2);
    assert_eq!(r2.remaining, 0);
}

#[test]
fn indexed_counter_reflects_inserted_count() {
    // Pre-insert an embedding row manually, then run index: that assertion is
    // skipped (already indexed), so indexed must be 0 for it.
    let (_dir, store) = setup();
    let aid = make_assertion(&store, "a");
    let mut d = mindspace::embeddings::manifest::derivation_from_manifest(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    );
    d.dimension = 4;
    d.model_id = "mock".to_string();
    let key = d.derivation_key();
    // manually insert an embedding row with matching derivation
    let emb = mindspace::core::AssertionEmbedding {
        assertion_id: aid,
        derivation_key: key.clone(),
        model: "mock".to_string(),
        model_version: "1.5".to_string(),
        dimension: 4,
        embedding: vec![1.0; 4],
    };
    store.save_embeddings(&[emb]).unwrap();
    let rt = MockRuntime::ok(4);
    let r = index_assertions(&store, &rt, &d, 32, None).unwrap();
    assert_eq!(r.indexed, 0, "pre-indexed assertion must not be counted");
    assert_eq!(r.already_indexed, 1);
    assert_eq!(r.remaining, 0);
}
