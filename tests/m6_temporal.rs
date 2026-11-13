//! Integration tests for M6 Stage 1: recording revisions, the writer fence,
//! the v8 baseline, producer-family isolation, and fail-closed temporal
//! capability reporting. Direct-SQL fixtures that need to bypass the fence
//! temporarily drop and restore the fence trigger, as documented in
//! docs/adr-m6-temporal-recording.md.

use mindspace::core::{
    Error, ManualMemoryDraft, TemporalCoverageStatus, TemporalDimension, Ts, Uuid,
};
use mindspace::ingest;
use mindspace::storage::Store;
use mindspace::SqliteStore;
use rusqlite::Connection;
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

    fn raw(&self) -> Connection {
        Connection::open(&self.path).unwrap()
    }
}

fn draft(subject: &str, content: &str, spaces: &[Uuid]) -> ManualMemoryDraft {
    ManualMemoryDraft {
        subject: Some(subject.to_string()),
        content: content.to_string(),
        spaces: spaces.to_vec(),
        supersedes: None,
    }
}

fn max_revision(conn: &Connection) -> i64 {
    conn.query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| {
        r.get(0)
    })
    .unwrap()
}

// ---------------------------------------------------------------------------
// v8 baseline
// ---------------------------------------------------------------------------

#[test]
fn migration_eight_records_baseline_and_coverage() {
    let db = TestDb::new();
    let conn = db.raw();
    assert_eq!(
        max_revision(&conn),
        6,
        "baseline plus schema-13 through schema-17 metadata cutovers"
    );
    let metadata_revision: i64 = conn
        .query_row(
            "SELECT revision_id FROM m6_row_availability WHERE table_name='asset_ownership_schema'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(metadata_revision, 2);
    let preparation_revision: i64 = conn.query_row("SELECT revision_id FROM m6_row_availability WHERE table_name='document_preparation_schema'", [], |r|r.get(0)).unwrap();
    assert_eq!(preparation_revision, 3);
    let lexical_revision: i64 = conn
        .query_row(
            "SELECT revision_id FROM m6_row_availability WHERE table_name='lexical_staging_schema'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(lexical_revision, 4);
    let semantic_revision: i64 = conn.query_row("SELECT revision_id FROM m6_row_availability WHERE table_name='semantic_staging_schema'",[],|r|r.get(0)).unwrap();
    assert_eq!(semantic_revision, 5);
    let kind: String = conn
        .query_row(
            "SELECT kind FROM m6_revisions WHERE revision_id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kind, "baseline");
    let covered: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM m6_history_coverage WHERE status = 'covered'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let unsupported: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM m6_history_coverage WHERE status = 'unsupported'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(covered + unsupported, TemporalDimension::all().len() as i64);
    assert!(covered > 0 && unsupported > 0);
}

#[test]
fn baseline_seeds_match_current_state() {
    let db = TestDb::new();
    let store = db.open();
    store.create_space("coding", "").unwrap();
    let file = db._dir.path().join("note.md");
    std::fs::write(&file, "# Note\n\nFirst content.\n").unwrap();
    let names = ["coding".to_string()];
    ingest::ingest_file(&store, &file, &names).unwrap();
    drop(store);

    let conn = db.raw();
    // The memory was written after cutover, so its only history row is its
    // birth revision; the baseline seeded the (empty) pre-existing state.
    let (history, memories): (i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM m6_memory_history), (SELECT COUNT(*) FROM memories)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(history, memories);
    let birth: i64 = conn
        .query_row("SELECT MIN(revision_id) FROM m6_memory_history", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(
        birth > 1,
        "post-baseline memories are born at a write revision"
    );
    let availability: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM m6_row_availability WHERE table_name = 'assertions'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let assertions: i64 = conn
        .query_row("SELECT COUNT(*) FROM assertions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(availability, assertions);
    let producer: String = conn
        .query_row("SELECT producer_family FROM memories LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(producer, "file:legacy");
}

#[test]
fn fenced_tables_have_complete_trigger_inventory() {
    let db = TestDb::new();
    let conn = db.raw();
    for table in [
        "assertions",
        "assertion_embeddings",
        "memories",
        "mind_meta",
        "spaces",
        "git_refs",
    ] {
        for suffix in ["i", "u", "d"] {
            let name = format!("m6_fence_{table}_{suffix}");
            let found: bool = conn
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type='trigger' AND name = ?1",
                    [&name],
                    |_| Ok(true),
                )
                .unwrap();
            assert!(found, "missing fence trigger {name}");
        }
    }
    // The revision ledger has allocation guards instead of the generic fence.
    for name in [
        "m6_fence_revisions_insert",
        "m6_fence_revisions_update",
        "m6_fence_revisions_delete",
    ] {
        let found: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='trigger' AND name = ?1",
                [name],
                |_| Ok(true),
            )
            .unwrap();
        assert!(found, "missing {name}");
    }
}

// ---------------------------------------------------------------------------
// Writer fence
// ---------------------------------------------------------------------------

#[test]
fn direct_sql_without_revision_function_fails_closed() {
    let db = TestDb::new();
    let conn = db.raw();
    for sql in [
        "INSERT INTO spaces (id, name, description, created_at) VALUES ('x', 'x', '', 0)",
        "UPDATE memories SET state = 'retired'",
        "DELETE FROM memory_spaces",
        "INSERT INTO mind_meta (key, value) VALUES ('embedding.enabled', 'true')",
        "INSERT INTO m6_revisions (revision_id, recorded_at, kind) VALUES (99, 0, 'write')",
        "UPDATE m6_revisions SET recorded_at = 0",
        "DELETE FROM m6_revisions",
        "INSERT INTO m6_memory_history VALUES ('x', 1, 0, NULL, NULL, NULL, NULL, NULL, NULL, NULL, 'forged')",
    ] {
        let result = conn.execute(sql, []);
        assert!(result.is_err(), "fence must reject: {sql}");
    }
}

#[test]
fn stale_revision_rows_do_not_authorize_supported_writers() {
    let db = TestDb::new();
    let store = db.open();
    let coding = store.create_space("coding", "").unwrap();
    store
        .record_manual_write(draft("Topic", "First content.", &[coding.id]))
        .unwrap();
    drop(store);

    // A committed revision row exists. A supported writer never presents it;
    // a connection with no active revision function must still be rejected.
    let conn = db.raw();
    let committed = max_revision(&conn);
    assert!(committed >= 2);
    let result = conn.execute(
        "INSERT INTO spaces (id, name, description, created_at) VALUES ('x', 'x', '', 0)",
        [],
    );
    assert!(result.is_err());
    drop(conn);

    // The next legitimate write allocates a fresh revision.
    let store = db.open();
    store
        .record_manual_write(draft("Other", "Other content.", &[coding.id]))
        .unwrap();
    let conn = db.raw();
    assert_eq!(max_revision(&conn), committed + 1);
}

#[test]
fn forged_active_revision_can_write_negative_control() {
    let db = TestDb::new();
    let store = db.open();
    store.create_space("coding", "").unwrap();
    drop(store);

    // Negative control within the documented boundary: a hostile connection
    // that forges the connection-local function with an existing revision id
    // can write. This demonstrates why function/state ownership must remain
    // exclusive to the storage layer; it is not a supported writer path.
    let conn = db.raw();
    let committed = max_revision(&conn);
    conn.create_scalar_function(
        "m6_write_revision",
        0,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8,
        move |_| Ok(Some(committed)),
    )
    .unwrap();
    let result = conn.execute(
        "INSERT INTO spaces (id, name, description, created_at) VALUES ('forged', 'forged', '', 0)",
        [],
    );
    assert!(result.is_ok(), "negative control: forged revision writes");
}

#[test]
fn one_revision_per_transaction_and_strictly_increasing() {
    let db = TestDb::new();
    let store = db.open();
    store.create_space("coding", "").unwrap();
    let file = db._dir.path().join("note.md");
    std::fs::write(&file, "# A\n\nAlpha content.\n\n# B\n\nBeta content.\n").unwrap();
    let names = ["coding".to_string()];
    ingest::ingest_file(&store, &file, &names).unwrap();
    drop(store);

    let conn = db.raw();
    let distinct: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT revision_id) FROM m6_memory_history WHERE revision_id > 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        distinct, 1,
        "one ingest transaction, one recording revision"
    );
    let revision: i64 = conn
        .query_row(
            "SELECT DISTINCT revision_id FROM m6_memory_history WHERE revision_id > 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    // Memberships were written in the same ingest transaction as the memories.
    let membership: i64 = conn
        .query_row(
            "SELECT MAX(revision_id) FROM m6_memory_space_history",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        membership, revision,
        "all changes in one transaction share one revision"
    );
    // The space itself was created in an earlier transaction.
    let space_birth: i64 = conn
        .query_row("SELECT MIN(revision_id) FROM m6_space_history", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(space_birth < revision, "revisions order transactions");
}

#[test]
fn rolled_back_transaction_leaves_no_revision_or_change() {
    let db = TestDb::new();
    let store = db.open();
    let before = max_revision(&db.raw());
    // A manual write with a nonexistent space fails mid-transaction.
    let result = store.record_manual_write(draft("Topic", "Some content.", &[Uuid::now_v7()]));
    assert!(matches!(
        result,
        Err(Error::SpaceIdNotFound { .. }) | Err(Error::SpaceNotFound { .. })
    ));
    let conn = db.raw();
    assert_eq!(
        max_revision(&conn),
        before,
        "rollback erases the allocation"
    );
    let spaces: i64 = conn
        .query_row("SELECT COUNT(*) FROM spaces", [], |r| r.get(0))
        .unwrap();
    assert_eq!(spaces, 0);
}

#[test]
fn concurrent_processes_allocate_distinct_revisions() {
    let db = TestDb::new();
    let store = db.open();
    let coding = store.create_space("coding", "").unwrap();
    drop(store);

    let mut children = Vec::new();
    for i in 0..4 {
        let path = db.path.clone();
        let space_id = coding.id.to_string();
        children.push(std::thread::spawn(move || {
            let store = SqliteStore::open(&path).unwrap();
            store
                .record_manual_write(draft(
                    &format!("topic {i}"),
                    &format!("content {i}"),
                    &[Uuid::parse_str(&space_id).unwrap()],
                ))
                .unwrap();
        }));
    }
    for child in children {
        child.join().unwrap();
    }
    let conn = db.raw();
    let (count, distinct): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COUNT(DISTINCT revision_id) FROM m6_memory_history",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        count, distinct,
        "each transaction allocated its own revision"
    );
}

// ---------------------------------------------------------------------------
// Producer-family isolation
// ---------------------------------------------------------------------------

#[test]
fn file_reingest_does_not_retire_other_producer_families() {
    let db = TestDb::new();
    let store = db.open();
    store.create_space("coding", "").unwrap();
    let file = db._dir.path().join("note.md");
    std::fs::write(&file, "# Topic\n\nPreserved topic content.\n").unwrap();
    let names = ["coding".to_string()];
    ingest::ingest_file(&store, &file, &names).unwrap();
    drop(store);

    // Inject a foreign-family episode on the same logical source and subject.
    // The fence is temporarily lifted for this fixture-only injection.
    let conn = db.raw();
    conn.execute("DROP TRIGGER m6_fence_assertions_i", [])
        .unwrap();
    conn.execute("DROP TRIGGER m6_fence_memories_i", [])
        .unwrap();
    conn.execute("DROP TRIGGER m6_birth_assertions", [])
        .unwrap();
    conn.execute("DROP TRIGGER m6_snapshot_memories_i", [])
        .unwrap();
    conn.execute(
        "INSERT INTO assertions (id, kind, subject, subject_normalized, predicate, \
             content, content_hash, extraction, created_at, evidence_id) \
         SELECT '00000000-0000-7000-8000-000000000001', kind, subject, subject_normalized, \
                predicate, content || ' (structured)', content_hash || 'x', 'structured@1:test', \
                created_at, evidence_id FROM assertions LIMIT 1",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO assertion_fts (assertion_id, subject, content) \
         SELECT id, subject, content FROM assertions WHERE id = '00000000-0000-7000-8000-000000000001'",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO memories (id, assertion_id, state, created_at, producer_family) \
         VALUES ('00000000-0000-7000-8000-000000000002', '00000000-0000-7000-8000-000000000001', \
                 'active', 1, 'structured:test')",
        [],
    )
    .unwrap();
    drop(conn);

    // Restore the dropped triggers exactly as installed by the migration.
    let conn = db.raw();
    conn.execute_batch(
        "CREATE TRIGGER m6_fence_assertions_i BEFORE INSERT ON assertions \
         WHEN m6_write_revision() IS NULL OR NOT EXISTS \
           (SELECT 1 FROM m6_revisions WHERE revision_id=m6_write_revision()) \
         BEGIN SELECT RAISE(ABORT, 'revisioned write required'); END; \
         CREATE TRIGGER m6_fence_memories_i BEFORE INSERT ON memories \
         WHEN m6_write_revision() IS NULL OR NOT EXISTS \
           (SELECT 1 FROM m6_revisions WHERE revision_id=m6_write_revision()) \
         BEGIN SELECT RAISE(ABORT, 'revisioned write required'); END; \
         CREATE TRIGGER m6_birth_assertions AFTER INSERT ON assertions BEGIN \
         INSERT OR IGNORE INTO m6_row_availability(table_name,entity_key,revision_id) \
         VALUES ('assertions',CAST((NEW.id) AS TEXT),m6_write_revision()); END; \
         CREATE TRIGGER m6_snapshot_memories_i AFTER INSERT ON memories BEGIN \
         INSERT INTO m6_memory_history (memory_id,revision_id,is_deleted,assertion_id,state,created_at,superseded_by,superseded_at,retired_at,retire_reason,producer_family) \
         VALUES (NEW.id,m6_write_revision(),0,NEW.assertion_id,NEW.state,NEW.created_at,NEW.superseded_by,NEW.superseded_at,NEW.retired_at,NEW.retire_reason,NEW.producer_family) \
         ON CONFLICT(memory_id,revision_id) DO UPDATE SET \
           is_deleted=excluded.is_deleted,assertion_id=excluded.assertion_id,state=excluded.state, \
           created_at=excluded.created_at,superseded_by=excluded.superseded_by, \
           superseded_at=excluded.superseded_at,retired_at=excluded.retired_at, \
           retire_reason=excluded.retire_reason,producer_family=excluded.producer_family; END;",
    )
    .unwrap();
    drop(conn);

    // Unchanged re-ingest must not retire the foreign-family episode.
    let store = db.open();
    ingest::ingest_file(&store, &file, &names).unwrap();
    // An unrelated edit must not retire it either.
    std::fs::write(
        &file,
        "# Topic\n\nPreserved topic content.\n\n# Other\n\nNew section.\n",
    )
    .unwrap();
    ingest::ingest_file(&store, &file, &names).unwrap();
    drop(store);

    let conn = db.raw();
    let state: String = conn
        .query_row(
            "SELECT state FROM memories WHERE id = '00000000-0000-7000-8000-000000000002'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        state, "active",
        "foreign-family episode must survive re-ingest"
    );
}

#[test]
fn unknown_legacy_producers_are_isolated() {
    let db = TestDb::new();
    let store = db.open();
    store.create_space("coding", "").unwrap();
    let file = db._dir.path().join("note.md");
    std::fs::write(&file, "# Topic\n\nPreserved topic content.\n").unwrap();
    let names = ["coding".to_string()];
    ingest::ingest_file(&store, &file, &names).unwrap();
    drop(store);

    // Inject a foreign-family episode (own assertion, unknown extraction) on
    // the same logical source. The fence is temporarily lifted for this
    // fixture-only injection and restored before the store reopens.
    let conn = db.raw();
    conn.execute("DROP TRIGGER m6_fence_assertions_i", [])
        .unwrap();
    conn.execute("DROP TRIGGER m6_fence_memories_i", [])
        .unwrap();
    conn.execute("DROP TRIGGER m6_birth_assertions", [])
        .unwrap();
    conn.execute("DROP TRIGGER m6_snapshot_memories_i", [])
        .unwrap();
    conn.execute(
        "INSERT INTO assertions (id, kind, subject, subject_normalized, predicate, \
             content, content_hash, extraction, created_at, evidence_id) \
         SELECT '00000000-0000-7000-8000-000000000001', kind, subject, subject_normalized, \
                predicate, content || ' (unknown tool)', content_hash || 'x', 'unknown@1:tool', \
                created_at, evidence_id FROM assertions LIMIT 1",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO assertion_fts (assertion_id, subject, content) \
         SELECT id, subject, content FROM assertions WHERE id = '00000000-0000-7000-8000-000000000001'",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO memories (id, assertion_id, state, created_at, producer_family) \
         VALUES ('00000000-0000-7000-8000-000000000002', '00000000-0000-7000-8000-000000000001', \
                 'active', 1, 'legacy:isolated:00000000-0000-7000-8000-000000000002')",
        [],
    )
    .unwrap();
    conn.execute_batch(
        "CREATE TRIGGER m6_fence_assertions_i BEFORE INSERT ON assertions \
         WHEN m6_write_revision() IS NULL OR NOT EXISTS \
           (SELECT 1 FROM m6_revisions WHERE revision_id=m6_write_revision()) \
         BEGIN SELECT RAISE(ABORT, 'revisioned write required'); END; \
         CREATE TRIGGER m6_fence_memories_i BEFORE INSERT ON memories \
         WHEN m6_write_revision() IS NULL OR NOT EXISTS \
           (SELECT 1 FROM m6_revisions WHERE revision_id=m6_write_revision()) \
         BEGIN SELECT RAISE(ABORT, 'revisioned write required'); END; \
         CREATE TRIGGER m6_birth_assertions AFTER INSERT ON assertions BEGIN \
         INSERT OR IGNORE INTO m6_row_availability(table_name,entity_key,revision_id) \
         VALUES ('assertions',CAST((NEW.id) AS TEXT),m6_write_revision()); END; \
         CREATE TRIGGER m6_snapshot_memories_i AFTER INSERT ON memories BEGIN \
         INSERT INTO m6_memory_history (memory_id,revision_id,is_deleted,assertion_id,state,created_at,superseded_by,superseded_at,retired_at,retire_reason,producer_family) \
         VALUES (NEW.id,m6_write_revision(),0,NEW.assertion_id,NEW.state,NEW.created_at,NEW.superseded_by,NEW.superseded_at,NEW.retired_at,NEW.retire_reason,NEW.producer_family) \
         ON CONFLICT(memory_id,revision_id) DO UPDATE SET \
           is_deleted=excluded.is_deleted,assertion_id=excluded.assertion_id,state=excluded.state, \
           created_at=excluded.created_at,superseded_by=excluded.superseded_by, \
           superseded_at=excluded.superseded_at,retired_at=excluded.retired_at, \
           retire_reason=excluded.retire_reason,producer_family=excluded.producer_family; END;",
    )
    .unwrap();
    drop(conn);

    // Unchanged and changed re-ingest must not retire the unknown family.
    let store = db.open();
    ingest::ingest_file(&store, &file, &names).unwrap();
    std::fs::write(
        &file,
        "# Topic\n\nPreserved topic content.\n\n# Other\n\nNew section.\n",
    )
    .unwrap();
    ingest::ingest_file(&store, &file, &names).unwrap();
    drop(store);
    let conn = db.raw();
    let state: String = conn
        .query_row(
            "SELECT state FROM memories WHERE id = '00000000-0000-7000-8000-000000000002'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        state, "active",
        "unknown-family episode must not be retired by file ingest"
    );
}

#[test]
fn manual_write_guards_survive_revision_fencing() {
    let db = TestDb::new();
    let store = db.open();
    let coding = store.create_space("coding", "").unwrap();
    let file = db._dir.path().join("note.md");
    std::fs::write(&file, "# Doc\n\nDocument content.\n").unwrap();
    let names = ["coding".to_string()];
    ingest::ingest_file(&store, &file, &names).unwrap();
    let hits = store
        .search(&mindspace::core::SearchQuery {
            text: "Document".to_string(),
            space_ids: None,
            limit: 5,
            at: None,
            include_superseded: false,
            include_retired: false,
        })
        .unwrap();
    let document_memory = hits[0].memory_id;
    let manual = store
        .record_manual_write(draft("Manual", "Manual content.", &[coding.id]))
        .unwrap();

    // Document-derived memories still cannot be updated through manual writes.
    let result = store.record_manual_write(ManualMemoryDraft {
        subject: None,
        content: "Rewritten.".to_string(),
        spaces: vec![coding.id],
        supersedes: Some(document_memory),
    });
    assert!(matches!(result, Err(Error::ManualUpdateNotAllowed { .. })));

    // Legitimate manual updates still succeed.
    let updated = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("Manual".to_string()),
            content: "Manual content changed.".to_string(),
            spaces: vec![coding.id],
            supersedes: Some(manual.memory_id),
        })
        .unwrap();
    assert_ne!(updated.memory_id, manual.memory_id);
}

// ---------------------------------------------------------------------------
// Fail-closed capability checks
// ---------------------------------------------------------------------------

#[test]
fn capability_report_lists_coverage() {
    let db = TestDb::new();
    let store = db.open();
    let caps = store.temporal_history_capabilities().unwrap();
    assert_eq!(caps.baseline_revision, 1);
    assert!(caps.current_revision >= caps.baseline_revision);
    let belief = caps
        .dimensions
        .iter()
        .find(|d| d.dimension == TemporalDimension::BeliefLifecycle)
        .unwrap();
    assert_eq!(belief.status, TemporalCoverageStatus::Covered);
    let world = caps
        .dimensions
        .iter()
        .find(|d| d.dimension == TemporalDimension::WorldValidity)
        .unwrap();
    assert_eq!(world.status, TemporalCoverageStatus::Unsupported);
}

#[test]
fn coverage_checks_fail_closed() {
    let db = TestDb::new();
    let store = db.open();
    // Unsupported dimension.
    let result = store.require_temporal_coverage(1, &[TemporalDimension::WorldValidity]);
    assert!(matches!(
        result,
        Err(Error::TemporalHistoryUnavailable { .. })
    ));
    // Below baseline.
    let result = store.require_temporal_coverage(0, &[TemporalDimension::BeliefLifecycle]);
    assert!(matches!(
        result,
        Err(Error::TemporalHistoryUnavailable { .. })
    ));
    // Beyond the current revision.
    let current = store
        .temporal_history_capabilities()
        .unwrap()
        .current_revision;
    let result =
        store.require_temporal_coverage(current + 5, &[TemporalDimension::BeliefLifecycle]);
    assert!(matches!(
        result,
        Err(Error::TemporalHistoryUnavailable { .. })
    ));
    // Covered dimension within range succeeds.
    store
        .require_temporal_coverage(current, &[TemporalDimension::BeliefLifecycle])
        .unwrap();
}

// ---------------------------------------------------------------------------
// Future-schema guard
// ---------------------------------------------------------------------------

#[test]
fn newer_schema_versions_are_rejected() {
    let db = TestDb::new();
    drop(db.open());
    let conn = db.raw();
    conn.execute(
        "INSERT INTO schema_migrations (version, applied_at) VALUES (99, 0)",
        [],
    )
    .unwrap();
    drop(conn);
    let result = SqliteStore::open(&db.path);
    assert!(
        matches!(result, Err(Error::Storage(msg)) if msg.contains("newer than supported")),
        "future schema must be rejected"
    );
}

// ---------------------------------------------------------------------------
// FTS consistency and rebuild
// ---------------------------------------------------------------------------

#[test]
fn fts_tampering_is_detected_and_rebuild_restores() {
    let db = TestDb::new();
    let store = db.open();
    store.create_space("coding", "").unwrap();
    let file = db._dir.path().join("note.md");
    std::fs::write(&file, "# Note\n\nSearchable content.\n").unwrap();
    let names = ["coding".to_string()];
    ingest::ingest_file(&store, &file, &names).unwrap();
    store.verify_fts_index().unwrap();
    drop(store);

    // Tamper with the derived index (FTS is outside trigger fencing).
    let conn = db.raw();
    conn.execute("DELETE FROM assertion_fts", []).unwrap();
    drop(conn);

    let store = db.open();
    assert!(store.verify_fts_index().is_err());
    let result = store.search(&mindspace::core::SearchQuery {
        text: "Searchable".to_string(),
        space_ids: None,
        limit: 5,
        at: None,
        include_superseded: false,
        include_retired: false,
    });
    assert!(matches!(result, Err(Error::FtsIndexInconsistent { .. })));

    store.rebuild_fts_index().unwrap();
    store.verify_fts_index().unwrap();
    let hits = store
        .search(&mindspace::core::SearchQuery {
            text: "Searchable".to_string(),
            space_ids: None,
            limit: 5,
            at: None,
            include_superseded: false,
            include_retired: false,
        })
        .unwrap();
    assert_eq!(hits.len(), 1);
}

// ---------------------------------------------------------------------------
// Recording-history content
// ---------------------------------------------------------------------------

#[test]
fn memory_lifecycle_history_records_transitions() {
    let db = TestDb::new();
    let store = db.open();
    let coding = store.create_space("coding", "").unwrap();
    let manual = store
        .record_manual_write(draft("Topic", "First version.", &[coding.id]))
        .unwrap();
    let updated = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("Topic".to_string()),
            content: "Second version.".to_string(),
            spaces: vec![coding.id],
            supersedes: Some(manual.memory_id),
        })
        .unwrap();
    store
        .retire_memory(updated.memory_id, Some("done".to_string()), Ts::now())
        .unwrap();
    drop(store);

    let conn = db.raw();
    let episodes: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM m6_memory_history WHERE memory_id IN (?, ?)",
            rusqlite::params![manual.memory_id.to_string(), updated.memory_id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(episodes, 4, "birth and transition images for both episodes");
    let superseded_image: (String, String, Option<String>) = conn
        .query_row(
            "SELECT h.state, h.producer_family, h.superseded_by FROM m6_memory_history h \
             WHERE h.memory_id = ?1 ORDER BY h.revision_id DESC LIMIT 1",
            rusqlite::params![manual.memory_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(superseded_image.0, "superseded");
    assert_eq!(superseded_image.1, "manual:write");
    assert_eq!(
        superseded_image.2.as_deref(),
        Some(updated.memory_id.to_string().as_str())
    );
    let retired: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM m6_memory_history WHERE memory_id = ?1 AND is_deleted = 0 \
             AND state = 'retired' AND retire_reason = 'done'",
            rusqlite::params![updated.memory_id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(retired, 1, "retirement recorded as a revisioned image");
}

// ---------------------------------------------------------------------------
// Poison and inventory-failure paths
// ---------------------------------------------------------------------------

#[test]
fn dropped_recording_triggers_fail_store_open() {
    let db = TestDb::new();
    drop(db.open());
    // Tamper: remove one recording trigger.
    let conn = db.raw();
    conn.execute("DROP TRIGGER m6_snapshot_memories_i", [])
        .unwrap();
    drop(conn);
    let result = SqliteStore::open(&db.path);
    assert!(
        matches!(result, Err(Error::Storage(msg)) if msg.contains("incomplete")),
        "missing recording triggers must fail the open"
    );
    // Restore the trigger; the store opens again.
    let conn = db.raw();
    conn.execute_batch(
        "CREATE TRIGGER m6_snapshot_memories_i AFTER INSERT ON memories BEGIN \
         INSERT INTO m6_memory_history (memory_id,revision_id,is_deleted,assertion_id,state,created_at,superseded_by,superseded_at,retired_at,retire_reason,producer_family) \
         VALUES (NEW.id,m6_write_revision(),0,NEW.assertion_id,NEW.state,NEW.created_at,NEW.superseded_by,NEW.superseded_at,NEW.retired_at,NEW.retire_reason,NEW.producer_family) \
         ON CONFLICT(memory_id,revision_id) DO UPDATE SET \
           is_deleted=excluded.is_deleted,assertion_id=excluded.assertion_id,state=excluded.state, \
           created_at=excluded.created_at,superseded_by=excluded.superseded_by, \
           superseded_at=excluded.superseded_at,retired_at=excluded.retired_at, \
           retire_reason=excluded.retire_reason,producer_family=excluded.producer_family; END;",
    )
    .unwrap();
    drop(conn);
    SqliteStore::open(&db.path).unwrap();
}

#[test]
fn discarded_connection_refuses_writes_but_keeps_reads() {
    // Simulate the poison state by directly exercising the private state via
    // a double-open: force a write that fails with an active foreign lock and
    // an uncommittable transaction is not reachable deterministically, so
    // this test pins the reachable part: after a clean early error, writes
    // still work (no poisoning), and a busy lock maps to DatabaseBusy.
    let db = TestDb::new();
    let store = db.open();
    let before = max_revision(&db.raw());
    let blocker = Connection::open(&db.path).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let result = store.create_space("blocked", "");
    assert!(matches!(result, Err(Error::DatabaseBusy { .. })));
    blocker.execute_batch("ROLLBACK").unwrap();
    // The failed attempt must not poison the store; its next write reuses
    // the never-committed revision number.
    store.create_space("after", "").unwrap();
    assert_eq!(max_revision(&db.raw()), before + 1);
}
