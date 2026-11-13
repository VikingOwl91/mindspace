use std::time::Duration;

use mindspace::core::{
    AssertionKind, Error, EvidenceKind, ManualMemoryDraft, MemoryActionKind, MemoryState,
    SearchQuery, SourceKind, Ts, Uuid,
};
use mindspace::ingest;
use mindspace::retrieval;
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
}

fn draft(subject: Option<&str>, content: &str, spaces: &[Uuid]) -> ManualMemoryDraft {
    ManualMemoryDraft {
        subject: subject.map(str::to_string),
        content: content.to_string(),
        spaces: spaces.to_vec(),
        supersedes: None,
    }
}

#[test]
fn manual_writes_have_explicit_provenance_and_derived_subjects() {
    let db = TestDb::new();
    let store = db.open();
    let coding = store.create_space("coding", "").unwrap();

    let report = store
        .record_manual_write(draft(
            None,
            "\n  Rust   cache policy\nUse a bounded cache.",
            &[coding.id],
        ))
        .unwrap();
    assert_eq!(report.subject, "Rust   cache policy");
    assert_eq!(report.action, MemoryActionKind::Created);

    let detail = store.get_memory_detail(report.memory_id).unwrap().unwrap();
    assert_eq!(detail.source.kind, SourceKind::Manual);
    assert_eq!(detail.source.location, "manual://writes");
    assert_eq!(detail.source_lifecycle.logical.kind.as_str(), "manual");
    assert_eq!(detail.evidence.kind, EvidenceKind::Document);
    assert_eq!(
        detail.evidence.content,
        "\n  Rust   cache policy\nUse a bounded cache."
    );
    assert_eq!(detail.assertion.kind, AssertionKind::Claim);
    assert_eq!(detail.assertion.extraction, "manual:write");
    assert_eq!(detail.assertion.subject, "Rust   cache policy");
    assert_eq!(detail.spaces[0].name, "coding");
}

#[test]
fn duplicate_writes_reuse_one_episode_and_add_missing_spaces() {
    let db = TestDb::new();
    let store = db.open();
    let coding = store.create_space("coding", "").unwrap();
    let linux = store.create_space("linux", "").unwrap();

    let first = store
        .record_manual_write(draft(
            Some(" Cache   policy "),
            "Keep the cache bounded.",
            &[coding.id],
        ))
        .unwrap();
    let duplicate = store
        .record_manual_write(draft(
            Some("Cache policy"),
            "Keep the cache bounded.",
            &[coding.id, linux.id],
        ))
        .unwrap();

    assert_eq!(first.memory_id, duplicate.memory_id);
    assert_eq!(first.assertion_id, duplicate.assertion_id);
    assert_eq!(first.subject, " Cache   policy ");
    assert_eq!(duplicate.subject, " Cache   policy ");
    assert_eq!(
        duplicate.action,
        MemoryActionKind::Reused {
            spaces_added: vec!["linux".to_string()]
        }
    );
    let detail = store.get_memory_detail(first.memory_id).unwrap().unwrap();
    assert_eq!(detail.spaces.len(), 2);

    let conn = Connection::open(&db.path).unwrap();
    for (table, expected) in [
        ("sources", 1),
        ("evidence", 1),
        ("assertions", 1),
        ("memories", 1),
    ] {
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, expected, "unexpected rows in {table}");
    }
}

#[test]
fn retired_belief_reuses_assertion_but_gets_a_new_episode() {
    let db = TestDb::new();
    let store = db.open();
    let coding = store.create_space("coding", "").unwrap();
    let first = store
        .record_manual_write(draft(Some("Rust"), "Use Rust.", &[coding.id]))
        .unwrap();
    store
        .retire_memory(first.memory_id, Some("reconsidered".to_string()), Ts::now())
        .unwrap();

    let recreated = store
        .record_manual_write(draft(Some("Rust"), "Use Rust.", &[coding.id]))
        .unwrap();
    assert_eq!(recreated.assertion_id, first.assertion_id);
    assert_ne!(recreated.memory_id, first.memory_id);
    assert_eq!(recreated.action, MemoryActionKind::Created);
    assert_eq!(
        store
            .get_memory_detail(first.memory_id)
            .unwrap()
            .unwrap()
            .memory
            .state,
        MemoryState::Retired
    );
}

#[test]
fn update_supersedes_once_and_identical_update_is_a_noop() {
    let db = TestDb::new();
    let store = db.open();
    let coding = store.create_space("coding", "").unwrap();
    let first = store
        .record_manual_write(draft(Some("Cache"), "Cache\nUse SQLite.", &[coding.id]))
        .unwrap();

    let update = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("Cache".to_string()),
            content: "Cache\nUse RocksDB.".to_string(),
            spaces: vec![coding.id],
            supersedes: Some(first.memory_id),
        })
        .unwrap();
    assert_ne!(update.memory_id, first.memory_id);
    assert_eq!(
        update.action,
        MemoryActionKind::Superseded {
            old_memory_id: first.memory_id
        }
    );
    let old = store.get_memory_detail(first.memory_id).unwrap().unwrap();
    assert_eq!(old.memory.state, MemoryState::Superseded);
    assert_eq!(old.memory.superseded_by, Some(update.memory_id));
    assert_eq!(old.superseded_by.unwrap().memory_id, update.memory_id);

    let unchanged = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("Cache".to_string()),
            content: "Cache\nUse RocksDB.".to_string(),
            spaces: vec![coding.id],
            supersedes: Some(update.memory_id),
        })
        .unwrap();
    assert_eq!(unchanged.memory_id, update.memory_id);
    assert_eq!(unchanged.action, MemoryActionKind::Unchanged);
    let conn = Connection::open(&db.path).unwrap();
    let memories: i64 = conn
        .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
        .unwrap();
    assert_eq!(memories, 2);
}

#[test]
fn update_reuses_an_existing_active_successor_without_self_supersession() {
    let db = TestDb::new();
    let store = db.open();
    let coding = store.create_space("coding", "").unwrap();
    let old = store
        .record_manual_write(draft(Some("Cache"), "SQLite cache", &[coding.id]))
        .unwrap();
    let current = store
        .record_manual_write(draft(Some("Cache"), "RocksDB cache", &[coding.id]))
        .unwrap();

    let updated = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("Cache".to_string()),
            content: "RocksDB cache".to_string(),
            spaces: vec![coding.id],
            supersedes: Some(old.memory_id),
        })
        .unwrap();
    assert_eq!(updated.memory_id, current.memory_id);
    assert_eq!(
        updated.action,
        MemoryActionKind::Superseded {
            old_memory_id: old.memory_id
        }
    );
    assert_eq!(
        store
            .get_memory_detail(old.memory_id)
            .unwrap()
            .unwrap()
            .memory
            .superseded_by,
        Some(current.memory_id)
    );
    assert_eq!(
        store
            .get_memory_detail(current.memory_id)
            .unwrap()
            .unwrap()
            .memory
            .state,
        MemoryState::Active
    );
}

#[test]
fn document_memories_and_inactive_manual_memories_cannot_be_updated() {
    let db = TestDb::new();
    let store = db.open();
    let coding = store.create_space("coding", "").unwrap();
    let source = db._dir.path().join("notes.md");
    std::fs::write(&source, "# Database\nUse SQLite.").unwrap();
    let ingest_report = ingest::ingest_file(&store, &source, &["coding"]).unwrap();
    let document_memory = ingest_report.files[0].actions[0].memory_id;
    let doc_error = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("Database".to_string()),
            content: "Use another database.".to_string(),
            spaces: vec![coding.id],
            supersedes: Some(document_memory),
        })
        .unwrap_err();
    assert!(matches!(doc_error, Error::ManualUpdateNotAllowed { .. }));

    let manual = store
        .record_manual_write(draft(Some("Manual"), "Manual content", &[coding.id]))
        .unwrap();
    store
        .retire_memory(manual.memory_id, None, Ts::now())
        .unwrap();
    let inactive_error = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("Manual".to_string()),
            content: "Replacement content".to_string(),
            spaces: vec![coding.id],
            supersedes: Some(manual.memory_id),
        })
        .unwrap_err();
    assert!(matches!(inactive_error, Error::MemoryNotActive { .. }));
}

#[test]
fn add_spaces_is_idempotent_and_rejects_unknown_ids() {
    let db = TestDb::new();
    let store = db.open();
    let coding = store.create_space("coding", "").unwrap();
    let linux = store.create_space("linux", "").unwrap();
    let report = store
        .record_manual_write(draft(Some("Preference"), "Use a terminal.", &[coding.id]))
        .unwrap();

    assert_eq!(
        store
            .add_memory_spaces(report.memory_id, vec![linux.id])
            .unwrap(),
        vec!["linux"]
    );
    assert!(store
        .add_memory_spaces(report.memory_id, vec![linux.id])
        .unwrap()
        .is_empty());
    assert!(matches!(
        store.add_memory_spaces(report.memory_id, vec![Uuid::now_v7()]),
        Err(Error::SpaceIdNotFound { .. })
    ));
}

#[test]
fn manual_memories_are_searchable_temporal_and_space_isolated() {
    let db = TestDb::new();
    let store = db.open();
    let coding = store.create_space("coding", "").unwrap();
    let linux = store.create_space("linux", "").unwrap();
    store.link_spaces(coding.id, linux.id).unwrap();
    let first = store
        .record_manual_write(draft(
            Some("Editor"),
            "The editor uses an old cache index.",
            &[linux.id],
        ))
        .unwrap();
    let before_update = store
        .get_memory_detail(first.memory_id)
        .unwrap()
        .unwrap()
        .memory
        .created_at;
    std::thread::sleep(Duration::from_millis(2));
    let latest = store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("Editor".to_string()),
            content: "The editor uses a new cache index.".to_string(),
            spaces: vec![linux.id],
            supersedes: Some(first.memory_id),
        })
        .unwrap();

    let before = retrieval::search(
        &store,
        SearchQuery {
            text: "old cache index".to_string(),
            space_ids: Some(vec![coding.id]),
            limit: 10,
            at: Some(Ts(before_update.0 + 1)),
            include_superseded: false,
            include_retired: false,
        },
    )
    .unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].memory_id, first.memory_id);
    assert_eq!(before[0].spaces[0].via.as_deref(), Some("linux"));

    let current_old = retrieval::search(
        &store,
        SearchQuery {
            text: "old cache index".to_string(),
            space_ids: None,
            limit: 10,
            at: None,
            include_superseded: false,
            include_retired: false,
        },
    )
    .unwrap();
    assert!(current_old.is_empty());
    let current_new = retrieval::search(
        &store,
        SearchQuery {
            text: "new cache index".to_string(),
            space_ids: Some(vec![coding.id]),
            limit: 10,
            at: None,
            include_superseded: false,
            include_retired: false,
        },
    )
    .unwrap();
    assert_eq!(current_new[0].memory_id, latest.memory_id);

    let conflicts = store.list_conflicts().unwrap();
    assert!(conflicts.is_empty());
    store
        .record_manual_write(draft(
            Some("Editor"),
            "A different editor implementation.",
            &[linux.id],
        ))
        .unwrap();
    let conflicts = store.list_conflicts().unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].space_name, "linux");
    assert_eq!(conflicts[0].members.len(), 2);
    assert!(matches!(
        store.add_memory_spaces(first.memory_id, vec![coding.id]),
        Err(Error::MemoryNotActive { .. })
    ));
}

#[test]
fn manually_created_immutable_rows_remain_protected_by_triggers() {
    let db = TestDb::new();
    let store = db.open();
    let coding = store.create_space("coding", "").unwrap();
    store
        .record_manual_write(draft(
            Some("Policy"),
            "Keep the record immutable.",
            &[coding.id],
        ))
        .unwrap();
    drop(store);

    let conn = Connection::open(&db.path).unwrap();
    for table in ["sources", "evidence", "assertions"] {
        let result = conn.execute(&format!("UPDATE {table} SET content = 'tampered'"), []);
        assert!(result.is_err(), "{table} should reject UPDATE");
    }
}

#[test]
fn migration_seven_adds_embedding_tables_to_phase_one_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);",
    )
    .unwrap();
    let migrations = [
        include_str!("../migrations/0001_init.sql"),
        include_str!("../migrations/0002_source_lifecycle.sql"),
        include_str!("../migrations/0003_git_ingest.sql"),
        include_str!("../migrations/0004_code_structure.sql"),
        include_str!("../migrations/0005_code_relationships.sql"),
    ];
    for (version, sql) in migrations.into_iter().enumerate() {
        conn.execute_batch(sql).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, 0)",
            [version as i64 + 1],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO spaces (id, name, description, created_at) VALUES (?1, 'legacy', '', 0)",
        [Uuid::now_v7().to_string()],
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
    assert_eq!(migration, 17);
    let index: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_logical_sources_manual'",
            [],
            |_| Ok(true),
        )
        .unwrap();
    assert!(index);
}

#[test]
fn oversized_manual_fields_are_rejected_without_writes() {
    let db = TestDb::new();
    let store = db.open();
    let space = store.create_space("bounds", "").unwrap();
    let too_large = "x".repeat(1024 * 1024 + 1);
    for (subject, content) in [
        (Some("valid"), too_large.as_str()),
        (Some(too_large.as_str()), "valid"),
    ] {
        let error = store
            .record_manual_write(draft(subject, content, &[space.id]))
            .unwrap_err();
        assert!(error.to_string().contains("1048576"), "{error}");
    }
    let conn = Connection::open(&db.path).unwrap();
    for table in ["memories", "assertions", "evidence", "ingest_sessions"] {
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
}

#[test]
fn manual_memory_limit_is_inclusive_and_counts_utf8_bytes() {
    use mindspace::core::MAX_MANUAL_MEMORY_BYTES as MAX;
    let db = TestDb::new();
    let store = db.open();
    let space = store.create_space("bounds", "").unwrap();
    // A derived subject can be the entire first line; both limits are equal.
    let boundary = "é".repeat(MAX / 2);
    for length in [MAX - 1, MAX] {
        let content = format!("{}x", "é".repeat((length - 1) / 2));
        let content = if length == MAX {
            boundary.clone()
        } else {
            content
        };
        assert_eq!(content.len(), length);
        let report = store
            .record_manual_write(draft(None, &content, &[space.id]))
            .unwrap();
        assert_eq!(report.subject, content);
        let detail = store.get_memory_detail(report.memory_id).unwrap().unwrap();
        assert_eq!(detail.assertion.content, content);
    }
    let conn = Connection::open(&db.path).unwrap();
    let before: i64 = conn
        .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))
        .unwrap();
    for (subject, content) in [
        (Some("ok"), format!("{boundary}x")),
        (Some(format!("{boundary}x").as_str()), "ok".to_string()),
    ] {
        assert!(store
            .record_manual_write(draft(subject, &content, &[space.id]))
            .is_err());
    }
    let after: i64 = conn
        .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after);
}
