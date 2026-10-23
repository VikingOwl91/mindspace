use mindspace::core::{
    cosine_similarity, decode_f32_le, encode_f32_le, Error, ManualMemoryDraft, SearchQuery, Ts,
    Uuid,
};
use mindspace::retrieval;
use mindspace::storage::Store;
use mindspace::SqliteStore;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use tempfile::TempDir;

struct TestDb {
    _dir: TempDir,
    path: std::path::PathBuf,
}

impl TestDb {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mind.db");
        SqliteStore::open(&path).unwrap();
        Self { _dir: dir, path }
    }

    fn open(&self) -> SqliteStore {
        SqliteStore::open(&self.path).unwrap()
    }
}

fn space(store: &SqliteStore, name: &str) -> Uuid {
    match store.create_space(name, "") {
        Ok(s) => s.id,
        Err(_) => store.get_space(name).unwrap().id,
    }
}

// ---------------------------------------------------------------------------
// Pure vector math
// ---------------------------------------------------------------------------

#[test]
fn encode_decode_round_trips_finite_values() {
    let input = [1.0f32, -2.5, 0.0, 1.2345, 1e-6];
    let bytes = encode_f32_le(&input).unwrap();
    assert_eq!(bytes.len(), input.len() * 4);
    let decoded = decode_f32_le(&bytes, None).unwrap();
    assert_eq!(decoded, input.to_vec());
}

#[test]
fn encode_rejects_non_finite_values() {
    assert!(encode_f32_le(&[f32::NAN]).is_err());
    assert!(encode_f32_le(&[f32::INFINITY]).is_err());
    assert!(encode_f32_le(&[f32::NEG_INFINITY]).is_err());
}

#[test]
fn decode_rejects_trailing_bytes() {
    let mut bytes = encode_f32_le(&[1.0f32]).unwrap();
    bytes.push(0);
    assert!(matches!(
        decode_f32_le(&bytes, None),
        Err(Error::InvalidVector { .. })
    ));
}

#[test]
fn decode_rejects_non_finite_values() {
    // NaN as little-endian f32.
    let bytes = 0x7fc00000u32.to_le_bytes();
    assert!(matches!(
        decode_f32_le(&bytes, None),
        Err(Error::InvalidVector { .. })
    ));
}

#[test]
fn decode_validates_expected_dimension() {
    let bytes = encode_f32_le(&[1.0f32, 2.0]).unwrap();
    assert!(decode_f32_le(&bytes, Some(2)).is_ok());
    assert!(matches!(
        decode_f32_le(&bytes, Some(3)),
        Err(Error::InvalidVector { .. })
    ));
}

#[test]
fn cosine_similarity_known_values() {
    assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
    assert!((cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]) - 0.0).abs() < 1e-6);
    assert!((cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
}

#[test]
fn cosine_similarity_handles_zero_and_mismatched() {
    assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
    assert_eq!(cosine_similarity(&[1.0, 0.0], &[0.0, 0.0]), 0.0);
    assert_eq!(cosine_similarity(&[1.0], &[1.0, 0.0]), 0.0); // dim mismatch
    assert_eq!(cosine_similarity(&[], &[]), 0.0); // empty
}

#[test]
fn cosine_similarity_uses_stable_accumulation() {
    // Large-magnitude inputs that would lose precision in f32 accumulation
    // must still produce a finite, bounded result.
    let a = vec![1e6_f32; 8];
    let b = vec![1e6_f32; 8];
    let sim = cosine_similarity(&a, &b);
    assert!(sim.is_finite());
    assert!((sim - 1.0).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// Migration
// ---------------------------------------------------------------------------

#[test]
fn migration_seven_adds_embedding_tables_to_phase_one_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let conn = Connection::open(&path).unwrap();
    let migrations = [
        include_str!("../migrations/0001_init.sql"),
        include_str!("../migrations/0002_source_lifecycle.sql"),
        include_str!("../migrations/0003_git_ingest.sql"),
        include_str!("../migrations/0004_code_structure.sql"),
        include_str!("../migrations/0005_code_relationships.sql"),
        include_str!("../migrations/0006_manual_writes.sql"),
    ];
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL)",
    )
    .unwrap();
    for (version, sql) in migrations.into_iter().enumerate() {
        conn.execute_batch(sql).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, 0)",
            [version as i64 + 1],
        )
        .unwrap();
    }
    let space_id = Uuid::now_v7();
    conn.execute(
        "INSERT INTO spaces (id, name, description, created_at) VALUES (?1, 'legacy', '', 0)",
        [space_id.to_string()],
    )
    .unwrap();
    drop(conn);

    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.get_space("legacy").unwrap().name, "legacy");

    let conn = Connection::open(&path).unwrap();
    let migration: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(migration, 7);

    // New tables exist.
    for table in ["assertion_embeddings", "mind_meta"] {
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |_| Ok(true),
            )
            .optional()
            .unwrap()
            .unwrap_or(false);
        assert!(exists, "missing table {table}");
    }
    // Index exists.
    let idx: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_assertion_embeddings_model'",
            [],
            |_| Ok(true),
        )
        .optional()
        .unwrap()
        .unwrap_or(false);
    assert!(idx);

    // Existing data preserved.
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM spaces", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn reopening_migrated_database_is_idempotent() {
    let db = TestDb::new();
    let store = db.open();
    // Just opening again must not error and must keep schema at v7.
    let store2 = SqliteStore::open(&db.path).unwrap();
    let conn = Connection::open(&db.path).unwrap();
    let v: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(v, 7);
    drop(store);
    drop(store2);
}

#[test]
fn immutability_triggers_still_reject_updates() {
    let db = TestDb::new();
    let store = db.open();
    space(&store, "coding");
    let path = db._dir.path().join("notes.md");
    std::fs::write(&path, "# T\n\nbody\n").unwrap();
    mindspace::ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    drop(store);

    let conn = Connection::open(&db.path).unwrap();
    for table in ["sources", "evidence", "assertions"] {
        assert!(conn
            .execute(&format!("UPDATE {table} SET content = 'tampered'"), [])
            .is_err());
    }
}

// ---------------------------------------------------------------------------
// Embedding storage
// ---------------------------------------------------------------------------

fn make_assertion(store: &SqliteStore) -> (Uuid, Uuid) {
    // Returns (memory_id, assertion_id) for a manual memory.
    let space_id = space(store, "coding");
    let report = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("subj".to_string()),
            content: "content body".to_string(),
            spaces: vec![space_id],
            supersedes: None,
        })
        .unwrap();
    (report.memory_id, report.assertion_id)
}

#[test]
fn save_embeddings_is_idempotent() {
    let db = TestDb::new();
    let store = db.open();
    let (_, assertion_id) = make_assertion(&store);

    let derivation = "deriv-a";
    let emb = mindspace::core::AssertionEmbedding {
        assertion_id,
        derivation_key: derivation.to_string(),
        model: "m".to_string(),
        model_version: "v1".to_string(),
        dimension: 4,
        embedding: vec![1.0, 2.0, 3.0, 4.0],
    };
    assert_eq!(
        store.save_embeddings(std::slice::from_ref(&emb)).unwrap(),
        1
    );
    // Second save is a no-op (idempotent).
    assert_eq!(
        store.save_embeddings(std::slice::from_ref(&emb)).unwrap(),
        0
    );

    let status = store.embedding_index_status(derivation).unwrap();
    assert_eq!(status.total_assertions, 1);
    assert_eq!(status.embedded, 1);
    assert_eq!(status.missing(), 0);
}

#[test]
fn derivation_key_isolates_embeddings() {
    let db = TestDb::new();
    let store = db.open();
    let (_, assertion_id) = make_assertion(&store);

    let emb = mindspace::core::AssertionEmbedding {
        assertion_id,
        derivation_key: "key-a".to_string(),
        model: "m".to_string(),
        model_version: "v1".to_string(),
        dimension: 2,
        embedding: vec![1.0, 0.0],
    };
    store.save_embeddings(&[emb]).unwrap();

    // Under a different derivation key the assertion still needs embedding.
    let needing = store
        .list_assertions_needing_embedding("key-b", None, 10)
        .unwrap();
    assert_eq!(needing.len(), 1);
    assert_eq!(needing[0].assertion_id, assertion_id);

    // Under the original key it does not.
    let needing = store
        .list_assertions_needing_embedding("key-a", None, 10)
        .unwrap();
    assert!(needing.is_empty());
}

#[test]
fn clear_embeddings_removes_only_target_derivation() {
    let db = TestDb::new();
    let store = db.open();
    let (_, assertion_id) = make_assertion(&store);

    for key in ["key-a", "key-b"] {
        store
            .save_embeddings(&[mindspace::core::AssertionEmbedding {
                assertion_id,
                derivation_key: key.to_string(),
                model: "m".to_string(),
                model_version: "v1".to_string(),
                dimension: 2,
                embedding: vec![1.0, 0.0],
            }])
            .unwrap();
    }
    assert_eq!(store.clear_embeddings("key-a").unwrap(), 1);
    let status_a = store.embedding_index_status("key-a").unwrap();
    let status_b = store.embedding_index_status("key-b").unwrap();
    assert_eq!(status_a.embedded, 0);
    assert_eq!(status_b.embedded, 1);
}

#[test]
fn save_embeddings_rejects_dimension_mismatch() {
    let db = TestDb::new();
    let store = db.open();
    let (_, assertion_id) = make_assertion(&store);

    let emb = mindspace::core::AssertionEmbedding {
        assertion_id,
        derivation_key: "k".to_string(),
        model: "m".to_string(),
        model_version: "v1".to_string(),
        dimension: 4,
        embedding: vec![1.0, 2.0], // wrong length
    };
    assert!(matches!(
        store.save_embeddings(&[emb]),
        Err(Error::InvalidVector { .. })
    ));
}

#[test]
fn save_embeddings_rejects_non_finite() {
    let db = TestDb::new();
    let store = db.open();
    let (_, assertion_id) = make_assertion(&store);

    let emb = mindspace::core::AssertionEmbedding {
        assertion_id,
        derivation_key: "k".to_string(),
        model: "m".to_string(),
        model_version: "v1".to_string(),
        dimension: 1,
        embedding: vec![f32::NAN],
    };
    assert!(matches!(
        store.save_embeddings(&[emb]),
        Err(Error::InvalidVector { .. })
    ));
}

#[test]
fn meta_round_trips_and_overwrites() {
    let db = TestDb::new();
    let store = db.open();
    assert!(store
        .get_meta("embedding.derivation_key")
        .unwrap()
        .is_none());
    store
        .set_meta("embedding.derivation_key", "abc123")
        .unwrap();
    assert_eq!(
        store.get_meta("embedding.derivation_key").unwrap(),
        Some("abc123".to_string())
    );
    store
        .set_meta("embedding.derivation_key", "def456")
        .unwrap();
    assert_eq!(
        store.get_meta("embedding.derivation_key").unwrap(),
        Some("def456".to_string())
    );
}

// ---------------------------------------------------------------------------
// search_vectors
// ---------------------------------------------------------------------------

#[test]
fn search_vectors_returns_cosine_ranked_hits() {
    let db = TestDb::new();
    let store = db.open();
    let coding = space(&store, "coding");
    let r1 = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("a".to_string()),
            content: "alpha body".to_string(),
            spaces: vec![coding],
            supersedes: None,
        })
        .unwrap();
    let r2 = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("b".to_string()),
            content: "beta body".to_string(),
            spaces: vec![coding],
            supersedes: None,
        })
        .unwrap();
    let a1 = r1.assertion_id;
    let a2 = r2.assertion_id;

    let key = "k";
    store
        .save_embeddings(&[
            mindspace::core::AssertionEmbedding {
                assertion_id: a1,
                derivation_key: key.to_string(),
                model: "m".to_string(),
                model_version: "v1".to_string(),
                dimension: 2,
                embedding: vec![1.0, 0.0],
            },
            mindspace::core::AssertionEmbedding {
                assertion_id: a2,
                derivation_key: key.to_string(),
                model: "m".to_string(),
                model_version: "v1".to_string(),
                dimension: 2,
                embedding: vec![0.0, 1.0],
            },
        ])
        .unwrap();

    let q = SearchQuery {
        text: "ignored".to_string(),
        space_ids: None,
        limit: 10,
        at: None,
        include_superseded: false,
        include_retired: false,
    };
    // Query aligned with a1's vector.
    let hits = store.search_vectors(&[1.0, 0.0], &q, key).unwrap();
    assert_eq!(hits.len(), 2);
    assert!(hits[0].score > hits[1].score, "cosine ranking");
    // First hit should be the one whose embedding is [1,0].
    // Verify by checking the stored mapping via a second query aligned with a2.
    let hits2 = store.search_vectors(&[0.0, 1.0], &q, key).unwrap();
    assert!(hits2[0].score > hits2[1].score);
    assert_ne!(hits[0].memory_id, hits2[0].memory_id);
}

#[test]
fn search_vectors_excludes_retired_and_superseded_by_default() {
    let db = TestDb::new();
    let store = db.open();
    let space_id = space(&store, "coding");
    let report = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("s".to_string()),
            content: "c".to_string(),
            spaces: vec![space_id],
            supersedes: None,
        })
        .unwrap();
    let mem = report.memory_id;
    let assertion = report.assertion_id;

    store
        .save_embeddings(&[mindspace::core::AssertionEmbedding {
            assertion_id: assertion,
            derivation_key: "k".to_string(),
            model: "m".to_string(),
            model_version: "v1".to_string(),
            dimension: 2,
            embedding: vec![1.0, 0.0],
        }])
        .unwrap();

    // Retire it.
    store.retire_memory(mem, None, Ts::now()).unwrap();

    let q = SearchQuery {
        text: "ignored".to_string(),
        space_ids: None,
        limit: 10,
        at: None,
        include_superseded: false,
        include_retired: false,
    };
    assert!(store
        .search_vectors(&[1.0, 0.0], &q, "k")
        .unwrap()
        .is_empty());

    let q2 = SearchQuery {
        text: "ignored".to_string(),
        space_ids: None,
        limit: 10,
        at: None,
        include_superseded: false,
        include_retired: true,
    };
    assert_eq!(
        store.search_vectors(&[1.0, 0.0], &q2, "k").unwrap().len(),
        1
    );
}

#[test]
fn search_vectors_respects_space_filter() {
    let db = TestDb::new();
    let store = db.open();
    let coding = space(&store, "coding");
    let linux = space(&store, "linux");
    let r = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("s".to_string()),
            content: "c".to_string(),
            spaces: vec![coding],
            supersedes: None,
        })
        .unwrap();
    store
        .save_embeddings(&[mindspace::core::AssertionEmbedding {
            assertion_id: r.assertion_id,
            derivation_key: "k".to_string(),
            model: "m".to_string(),
            model_version: "v1".to_string(),
            dimension: 2,
            embedding: vec![1.0, 0.0],
        }])
        .unwrap();

    let q_coding = SearchQuery {
        text: "ignored".to_string(),
        space_ids: Some(vec![coding]),
        limit: 10,
        at: None,
        include_superseded: false,
        include_retired: false,
    };
    let q_linux = SearchQuery {
        text: "ignored".to_string(),
        space_ids: Some(vec![linux]),
        limit: 10,
        at: None,
        include_superseded: false,
        include_retired: false,
    };
    assert_eq!(
        store
            .search_vectors(&[1.0, 0.0], &q_coding, "k")
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .search_vectors(&[1.0, 0.0], &q_linux, "k")
        .unwrap()
        .is_empty());
}

#[test]
fn search_vectors_dimension_mismatch_excluded() {
    let db = TestDb::new();
    let store = db.open();
    let coding = space(&store, "coding");
    let r = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("s".to_string()),
            content: "c".to_string(),
            spaces: vec![coding],
            supersedes: None,
        })
        .unwrap();
    // Stored dimension 2, query dimension 3 → excluded.
    store
        .save_embeddings(&[mindspace::core::AssertionEmbedding {
            assertion_id: r.assertion_id,
            derivation_key: "k".to_string(),
            model: "m".to_string(),
            model_version: "v1".to_string(),
            dimension: 2,
            embedding: vec![1.0, 0.0],
        }])
        .unwrap();
    let q = SearchQuery {
        text: "ignored".to_string(),
        space_ids: None,
        limit: 10,
        at: None,
        include_superseded: false,
        include_retired: false,
    };
    assert!(store
        .search_vectors(&[1.0, 0.0, 0.0], &q, "k")
        .unwrap()
        .is_empty());
}

#[test]
fn search_vectors_empty_space_list_matches_nothing() {
    let db = TestDb::new();
    let store = db.open();
    let coding = space(&store, "coding");
    let r = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("s".to_string()),
            content: "c".to_string(),
            spaces: vec![coding],
            supersedes: None,
        })
        .unwrap();
    store
        .save_embeddings(&[mindspace::core::AssertionEmbedding {
            assertion_id: r.assertion_id,
            derivation_key: "k".to_string(),
            model: "m".to_string(),
            model_version: "v1".to_string(),
            dimension: 2,
            embedding: vec![1.0, 0.0],
        }])
        .unwrap();
    let q = SearchQuery {
        text: "ignored".to_string(),
        space_ids: Some(vec![]),
        limit: 10,
        at: None,
        include_superseded: false,
        include_retired: false,
    };
    assert!(store
        .search_vectors(&[1.0, 0.0], &q, "k")
        .unwrap()
        .is_empty());
}

#[test]
fn search_vectors_skips_malformed_blob() {
    let db = TestDb::new();
    let store = db.open();
    let coding = space(&store, "coding");
    let r = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("s".to_string()),
            content: "c".to_string(),
            spaces: vec![coding],
            supersedes: None,
        })
        .unwrap();
    // Insert a row with a malformed blob directly (wrong length).
    let conn = Connection::open(&db.path).unwrap();
    conn.execute(
        "INSERT INTO assertion_embeddings (assertion_id, derivation_key, model, model_version, dimension, embedding, created_at) \
         VALUES (?1, 'k', 'm', 'v1', 2, ?2, 0)",
        rusqlite::params![r.assertion_id.to_string(), vec![1u8, 2, 3]],
    )
    .unwrap();
    drop(conn);

    let q = SearchQuery {
        text: "ignored".to_string(),
        space_ids: None,
        limit: 10,
        at: None,
        include_superseded: false,
        include_retired: false,
    };
    // Malformed blob is excluded, no panic.
    assert!(store
        .search_vectors(&[1.0, 0.0], &q, "k")
        .unwrap()
        .is_empty());
}

#[test]
fn search_vectors_handles_zero_query_vector() {
    let db = TestDb::new();
    let store = db.open();
    let coding = space(&store, "coding");
    let r = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("s".to_string()),
            content: "c".to_string(),
            spaces: vec![coding],
            supersedes: None,
        })
        .unwrap();
    store
        .save_embeddings(&[mindspace::core::AssertionEmbedding {
            assertion_id: r.assertion_id,
            derivation_key: "k".to_string(),
            model: "m".to_string(),
            model_version: "v1".to_string(),
            dimension: 2,
            embedding: vec![1.0, 0.0],
        }])
        .unwrap();
    let q = SearchQuery {
        text: "ignored".to_string(),
        space_ids: None,
        limit: 10,
        at: None,
        include_superseded: false,
        include_retired: false,
    };
    // Zero query vector → cosine 0 for all, but row is still returned (score 0).
    let hits = store.search_vectors(&[0.0, 0.0], &q, "k").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].score, 0.0);
}

#[test]
fn search_vectors_multiple_episodes_filters_to_active() {
    let db = TestDb::new();
    let store = db.open();
    let coding = space(&store, "coding");
    let first = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("s".to_string()),
            content: "first".to_string(),
            spaces: vec![coding],
            supersedes: None,
        })
        .unwrap();
    // Retire and recreate → new episode, same assertion.
    store
        .retire_memory(first.memory_id, None, Ts::now())
        .unwrap();
    let second = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("s".to_string()),
            content: "first".to_string(),
            spaces: vec![coding],
            supersedes: None,
        })
        .unwrap();
    assert_ne!(second.memory_id, first.memory_id);
    assert_eq!(second.assertion_id, first.assertion_id);

    // One embedding for the shared assertion.
    store
        .save_embeddings(&[mindspace::core::AssertionEmbedding {
            assertion_id: first.assertion_id,
            derivation_key: "k".to_string(),
            model: "m".to_string(),
            model_version: "v1".to_string(),
            dimension: 2,
            embedding: vec![1.0, 0.0],
        }])
        .unwrap();

    let q = SearchQuery {
        text: "ignored".to_string(),
        space_ids: None,
        limit: 10,
        at: None,
        include_superseded: false,
        include_retired: false,
    };
    // Default (active only): the new episode is active → returned.
    let hits = store.search_vectors(&[1.0, 0.0], &q, "k").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].memory_id, second.memory_id);

    // Include retired: the retired episode also surfaces (same assertion vector).
    let q2 = SearchQuery {
        text: "ignored".to_string(),
        space_ids: None,
        limit: 10,
        at: None,
        include_superseded: false,
        include_retired: true,
    };
    let hits2 = store.search_vectors(&[1.0, 0.0], &q2, "k").unwrap();
    assert_eq!(hits2.len(), 2);
}

#[test]
fn search_vectors_temporal_filter() {
    let db = TestDb::new();
    let store = db.open();
    let coding = space(&store, "coding");
    let r = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("s".to_string()),
            content: "c".to_string(),
            spaces: vec![coding],
            supersedes: None,
        })
        .unwrap();
    store
        .save_embeddings(&[mindspace::core::AssertionEmbedding {
            assertion_id: r.assertion_id,
            derivation_key: "k".to_string(),
            model: "m".to_string(),
            model_version: "v1".to_string(),
            dimension: 2,
            embedding: vec![1.0, 0.0],
        }])
        .unwrap();

    // `at` before creation → excluded; `at` after → included.
    let before = SearchQuery {
        text: "ignored".to_string(),
        space_ids: None,
        limit: 10,
        at: Some(Ts(0)),
        include_superseded: false,
        include_retired: false,
    };
    // created_at is now (microseconds), so at=0 excludes it.
    assert!(store
        .search_vectors(&[1.0, 0.0], &before, "k")
        .unwrap()
        .is_empty());

    let after = SearchQuery {
        text: "ignored".to_string(),
        space_ids: None,
        limit: 10,
        at: None,
        include_superseded: false,
        include_retired: false,
    };
    assert_eq!(
        store
            .search_vectors(&[1.0, 0.0], &after, "k")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn lexical_and_vector_share_filter_universe() {
    let db = TestDb::new();
    let store = db.open();
    let coding = space(&store, "coding");
    let linux = space(&store, "linux");
    let r = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("needle".to_string()),
            content: "haystack body".to_string(),
            spaces: vec![coding],
            supersedes: None,
        })
        .unwrap();
    store
        .save_embeddings(&[mindspace::core::AssertionEmbedding {
            assertion_id: r.assertion_id,
            derivation_key: "k".to_string(),
            model: "m".to_string(),
            model_version: "v1".to_string(),
            dimension: 2,
            embedding: vec![1.0, 0.0],
        }])
        .unwrap();

    // Space filter: coding includes it, linux excludes it — for both paths.
    let q_coding = SearchQuery {
        text: "needle".to_string(),
        space_ids: Some(vec![coding]),
        limit: 10,
        at: None,
        include_superseded: false,
        include_retired: false,
    };
    let q_linux = SearchQuery {
        text: "needle".to_string(),
        space_ids: Some(vec![linux]),
        limit: 10,
        at: None,
        include_superseded: false,
        include_retired: false,
    };
    // Lexical
    assert_eq!(
        retrieval::search(&store, q_coding.clone()).unwrap().len(),
        1
    );
    assert!(retrieval::search(&store, q_linux.clone())
        .unwrap()
        .is_empty());
    // Vector: same qualification.
    assert_eq!(
        store
            .search_vectors(&[1.0, 0.0], &q_coding, "k")
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .search_vectors(&[1.0, 0.0], &q_linux, "k")
        .unwrap()
        .is_empty());
}

#[test]
fn search_vectors_deterministic_tie_break() {
    let db = TestDb::new();
    let store = db.open();
    let coding = space(&store, "coding");
    // Two assertions with identical embeddings → equal cosine → tie-break by memory_id.
    let r1 = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("a".to_string()),
            content: "a".to_string(),
            spaces: vec![coding],
            supersedes: None,
        })
        .unwrap();
    let r2 = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("b".to_string()),
            content: "b".to_string(),
            spaces: vec![coding],
            supersedes: None,
        })
        .unwrap();
    for r in [&r1, &r2] {
        store
            .save_embeddings(&[mindspace::core::AssertionEmbedding {
                assertion_id: r.assertion_id,
                derivation_key: "k".to_string(),
                model: "m".to_string(),
                model_version: "v1".to_string(),
                dimension: 2,
                embedding: vec![0.0, 1.0],
            }])
            .unwrap();
    }
    let q = SearchQuery {
        text: "ignored".to_string(),
        space_ids: None,
        limit: 10,
        at: None,
        include_superseded: false,
        include_retired: false,
    };
    let hits = store.search_vectors(&[1.0, 0.0], &q, "k").unwrap();
    assert_eq!(hits.len(), 2);
    // Both cosine 0.0 → tie-break by memory_id ascending.
    assert!(hits[0].memory_id < hits[1].memory_id);
    assert_eq!(hits[0].score, 0.0);
    assert_eq!(hits[1].score, 0.0);
}

#[test]
fn embedding_index_status_counts() {
    let db = TestDb::new();
    let store = db.open();
    let coding = space(&store, "coding");
    let r1 = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("a".to_string()),
            content: "a".to_string(),
            spaces: vec![coding],
            supersedes: None,
        })
        .unwrap();
    let r2 = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("b".to_string()),
            content: "b".to_string(),
            spaces: vec![coding],
            supersedes: None,
        })
        .unwrap();
    // Embed only one.
    store
        .save_embeddings(&[mindspace::core::AssertionEmbedding {
            assertion_id: r1.assertion_id,
            derivation_key: "k".to_string(),
            model: "m".to_string(),
            model_version: "v1".to_string(),
            dimension: 2,
            embedding: vec![1.0, 0.0],
        }])
        .unwrap();
    let _ = r2;
    let status = store.embedding_index_status("k").unwrap();
    assert_eq!(status.total_assertions, 2);
    assert_eq!(status.embedded, 1);
    assert_eq!(status.missing(), 1);
}

#[test]
fn list_assertions_needing_embedding_respects_limit() {
    let db = TestDb::new();
    let store = db.open();
    let coding = space(&store, "coding");
    for i in 0..5 {
        store
            .record_manual_write(ManualMemoryDraft {
                subject: Some(format!("s{i}")),
                content: format!("c{i}"),
                spaces: vec![coding],
                supersedes: None,
            })
            .unwrap();
    }
    let needing = store
        .list_assertions_needing_embedding("k", None, 2)
        .unwrap();
    assert_eq!(needing.len(), 2);
    let needing_all = store
        .list_assertions_needing_embedding("k", None, 100)
        .unwrap();
    assert_eq!(needing_all.len(), 5);
}

#[test]
fn derivation_key_canonical_encoding_is_stable_and_collision_free() {
    use mindspace::core::EmbeddingDerivation;
    let d = EmbeddingDerivation {
        model_id: "m".to_string(),
        model_version: "v1".to_string(),
        model_sha256: "a".repeat(64),
        tokenizer_sha256: "b".repeat(64),
        prefix_strategy: "search_document:".to_string(),
        pooling: "mean".to_string(),
        normalization: "l2".to_string(),
        max_seq_len: 8192,
        truncation: "truncate".to_string(),
        dimension: 768,
        runtime: "candle".to_string(),
        runtime_version: "0.9".to_string(),
    };
    // Deterministic.
    assert_eq!(d.derivation_key(), d.derivation_key());
    // Different input → different key.
    let mut d2 = d.clone();
    d2.prefix_strategy = "none".to_string();
    assert_ne!(d.derivation_key(), d2.derivation_key());
    // Canonical bytes are valid JSON.
    let bytes = d.canonical_bytes();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(parsed.is_object());
}

// ---------------------------------------------------------------------------
// Phase 2 M1 review regressions
// ---------------------------------------------------------------------------

#[test]
fn search_vectors_top_n_evaluates_all_candidates_before_limit() {
    let db = TestDb::new();
    let store = db.open();
    let coding = space(&store, "coding");

    for i in 0..4 {
        let report = store
            .record_manual_write(ManualMemoryDraft {
                subject: Some(format!("poor{i}")),
                content: format!("poor{i}"),
                spaces: vec![coding],
                supersedes: None,
            })
            .unwrap();
        store
            .save_embeddings(&[mindspace::core::AssertionEmbedding {
                assertion_id: report.assertion_id,
                derivation_key: "k".to_string(),
                model: "m".to_string(),
                model_version: "v1".to_string(),
                dimension: 2,
                embedding: vec![0.0, 1.0],
            }])
            .unwrap();
    }
    let best = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("best".to_string()),
            content: "best".to_string(),
            spaces: vec![coding],
            supersedes: None,
        })
        .unwrap();
    store
        .save_embeddings(&[mindspace::core::AssertionEmbedding {
            assertion_id: best.assertion_id,
            derivation_key: "k".to_string(),
            model: "m".to_string(),
            model_version: "v1".to_string(),
            dimension: 2,
            embedding: vec![1.0, 0.0],
        }])
        .unwrap();

    let q = SearchQuery {
        text: "ignored".to_string(),
        space_ids: None,
        limit: 2,
        at: None,
        include_superseded: false,
        include_retired: false,
    };
    let hits = store.search_vectors(&[1.0, 0.0], &q, "k").unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].memory_id, best.memory_id);
    assert!((hits[0].score - 1.0).abs() < 1e-6);
}

#[test]
fn save_embeddings_errors_on_nonexistent_assertion() {
    let db = TestDb::new();
    let store = db.open();
    let emb = mindspace::core::AssertionEmbedding {
        assertion_id: Uuid::now_v7(),
        derivation_key: "k".to_string(),
        model: "m".to_string(),
        model_version: "v1".to_string(),
        dimension: 2,
        embedding: vec![1.0, 0.0],
    };
    assert!(matches!(
        store.save_embeddings(&[emb]),
        Err(Error::Storage(_))
    ));
}

#[test]
fn search_vectors_rejects_nan_query_vector() {
    let db = TestDb::new();
    let store = db.open();
    let q = SearchQuery {
        text: "ignored".to_string(),
        space_ids: None,
        limit: 10,
        at: None,
        include_superseded: false,
        include_retired: false,
    };
    assert!(matches!(
        store.search_vectors(&[f32::NAN, 0.0], &q, "k"),
        Err(Error::InvalidVector { .. })
    ));
}

#[test]
fn search_vectors_rejects_infinity_query_vector() {
    let db = TestDb::new();
    let store = db.open();
    let q = SearchQuery {
        text: "ignored".to_string(),
        space_ids: None,
        limit: 10,
        at: None,
        include_superseded: false,
        include_retired: false,
    };
    assert!(matches!(
        store.search_vectors(&[f32::INFINITY, 0.0], &q, "k"),
        Err(Error::InvalidVector { .. })
    ));
    assert!(matches!(
        store.search_vectors(&[f32::NEG_INFINITY, 0.0], &q, "k"),
        Err(Error::InvalidVector { .. })
    ));
}

#[test]
fn derivation_key_canonical_encoding_resists_delimiter_injection() {
    use mindspace::core::EmbeddingDerivation;
    let base = EmbeddingDerivation {
        model_id: "m\"}".to_string(),
        model_version: "v1".to_string(),
        model_sha256: "a".repeat(64),
        tokenizer_sha256: "b".repeat(64),
        prefix_strategy: "search_document:".to_string(),
        pooling: "mean".to_string(),
        normalization: "l2".to_string(),
        max_seq_len: 8192,
        truncation: "truncate".to_string(),
        dimension: 768,
        runtime: "candle".to_string(),
        runtime_version: "0.9".to_string(),
    };
    let bytes = base.canonical_bytes();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(parsed.is_object());
    assert_eq!(base.derivation_key(), base.derivation_key());

    let mut other = base.clone();
    other.model_id = "m".to_string();
    other.prefix_strategy = "search_document:\"}".to_string();
    assert_ne!(base.derivation_key(), other.derivation_key());

    let mut variant = base.clone();
    variant.model_id = "".to_string();
    assert_ne!(base.derivation_key(), variant.derivation_key());
}
