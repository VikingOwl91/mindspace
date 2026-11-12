//! Genuine old-schema fixtures execute only the old migrations, before S3b exists.
use super::*;
use tempfile::TempDir;

pub(super) fn old_store(path: &Path, version: i64) -> SqliteStore {
    let mut conn = Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=OFF; CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL)").unwrap();
    let state = Arc::new(RevisionState {
        active_revision: AtomicI64::new(0),
        poisoned: AtomicBool::new(false),
    });
    register_revision_context(&mut conn, &state).unwrap();
    let ownership_state = Arc::new(ownership::OwnershipState::default());
    ownership::register_context(&conn, &ownership_state).unwrap();
    let preparation_state = Arc::new(document_preparation::PreparationState::default());
    document_preparation::register_context(&conn, &preparation_state).unwrap();
    let lexical_state = Arc::new(lexical_staging::LexicalState::default());
    lexical_staging::register_context(&conn, &lexical_state).unwrap();
    let semantic_state = Arc::new(semantic_staging::SemanticState::default());
    semantic_staging::register_context(&conn, &semantic_state).unwrap();
    let fence_plan_state = Arc::new(fence_plans::FencePlanState::default());
    if version >= 17 {
        fence_plans::register_context(&conn, &fence_plan_state).unwrap();
    }
    for (number, sql) in MIGRATIONS.iter().filter(|(number, _)| *number <= version) {
        let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate).unwrap();
        let metadata_scope = if matches!(*number, 13..=17) {
            Some(MigrationRevisionScope::enter(&tx, &state).unwrap())
        } else {
            None
        };
        tx.execute_batch(sql).unwrap();
        if *number == M6_VERSION {
            apply_m6_baseline(&tx).unwrap();
        }
        tx.execute("INSERT INTO schema_migrations VALUES(?1,0)", [number])
            .unwrap();
        if *number == M6_VERSION {
            install_m6_write_fence(&tx).unwrap();
        }
        if matches!(*number, 13..=17) {
            let (table, key) = match *number {
                13 => ("asset_ownership_schema", "local-enroll@1"),
                14 => ("document_preparation_schema", "local-doc-prepare@1"),
                15 => ("lexical_staging_schema", "local-doc-stage-lexical@1"),
                16 => ("semantic_staging_schema", "local-doc-stage-semantic@1"),
                _ => ("fence_plan_schema", "local-fence-plan@1"),
            };
            tx.execute(
                "INSERT INTO m6_row_availability VALUES(?1,?2,m6_write_revision())",
                [table, key],
            )
            .unwrap();
            WriteTransaction {
                tx: Some(tx),
                conn: &conn,
                revision_state: Arc::clone(&state),
            }
            .commit()
            .unwrap();
        } else {
            tx.commit().unwrap();
        }
        drop(metadata_scope);
    }
    conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    SqliteStore {
        conn: Mutex::new(Some(conn)),
        revision_state: state,
        ownership_state,
        preparation_state,
        lexical_state,
        semantic_state,
        fence_plan_state,
        fts_integrity: AtomicBool::new(true),
    }
}

fn revision(conn: &Connection) -> i64 {
    conn.query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| {
        r.get(0)
    })
    .unwrap()
}

#[test]
fn populated_eleven_and_twelve_upgrade_without_rewriting_legacy_or_capabilities() {
    for version in [11, 12] {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("mind.db");
        let store = old_store(&path, version);
        store.create_space("notes", "").unwrap();
        let file = dir.path().join("note.md");
        std::fs::write(&file, "# Notes\n\nquartz ownership compatibility\n").unwrap();
        crate::ingest::ingest_file(&store, &file, &["notes"]).unwrap();
        let query = SearchQuery {
            text: "quartz".into(),
            space_ids: None,
            limit: 20,
            at: None,
            include_superseded: false,
            include_retired: false,
        };
        let before = store.search(&query).unwrap();
        assert!(!before.is_empty());
        let caps = store.temporal_history_capabilities().unwrap();
        let before_r = revision(&store.connection().unwrap());
        drop(store);
        let upgraded = SqliteStore::open(&path).unwrap();
        assert_eq!(upgraded.search(&query).unwrap(), before);
        let after_caps = upgraded.temporal_history_capabilities().unwrap();
        assert_eq!(
            serde_json::to_value(after_caps.dimensions).unwrap(),
            serde_json::to_value(caps.dimensions).unwrap()
        );
        assert_eq!(after_caps.baseline_revision, caps.baseline_revision);
        let conn = upgraded.connection().unwrap();
        assert_eq!(revision(&conn), before_r + 5);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM asset_ownership", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(conn.query_row("SELECT revision_id FROM m6_row_availability WHERE table_name='asset_ownership_schema' AND entity_key='local-enroll@1'", [], |r|r.get::<_,i64>(0)).unwrap(),before_r+1);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            0
        );
        drop(conn);
        drop(upgraded);
        let reopened = SqliteStore::open(&path).unwrap();
        assert_eq!(revision(&reopened.connection().unwrap()), before_r + 5);
    }
}

#[test]
fn late_metadata_ddl_failure_and_commit_veto_restore_old_schema_and_clear_scope() {
    for failure in ["late_ddl", "commit_veto"] {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("mind.db");
        let old = old_store(&path, 12);
        let mut conn = old.connection().unwrap();
        let before = revision(&conn);
        if failure == "late_ddl" {
            conn.execute_batch("CREATE TABLE asset_ownership_history(sentinel TEXT)")
                .unwrap();
        } else {
            conn.guard
                .as_mut()
                .unwrap()
                .commit_hook(Some(|| true))
                .unwrap();
        }
        let error = run_migrations(&conn, &old.revision_state).unwrap_err();
        assert!(
            !matches!(error, Error::CommitOutcomeUnknown { .. }),
            "{error}"
        );
        assert_eq!(old.revision_state.active_revision.load(Ordering::SeqCst), 0);
        assert!(conn.is_autocommit());
        assert_eq!(revision(&conn), before);
        assert_eq!(
            conn.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            12
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name='asset_ownership'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}

#[test]
fn missing_schema_marker_or_enforcement_is_a_failed_open() {
    for attack in ["marker", "guard"] {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("mind.db");
        drop(SqliteStore::open(&path).unwrap());
        let raw = Connection::open(&path).unwrap();
        if attack == "marker" {
            raw.execute_batch("DROP TRIGGER m6_s3b_availability_delete; DROP TRIGGER m6_fence_m6_row_availability_d; DELETE FROM m6_row_availability WHERE table_name='asset_ownership_schema'").unwrap();
        } else {
            raw.execute_batch("DROP TRIGGER m6_s3b_receipt_insert")
                .unwrap();
        }
        assert!(SqliteStore::open(&path).is_err());
    }
}

#[test]
#[cfg(feature = "spawn-gate")]
fn lost_migration_ack_is_unknown_and_reopen_applies_no_second_revision() {
    // Isolate the existing process-global one-shot fault from unrelated unit tests.
    if std::env::var_os("MINDSPACE_TEST_METADATA_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "storage::sqlite::ownership_migration_tests::lost_migration_ack_is_unknown_and_reopen_applies_no_second_revision", "--nocapture"])
            .env("MINDSPACE_TEST_METADATA_CHILD", "1").output().unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("mind.db");
    let old = old_store(&path, 12);
    old.create_space("legacy", "preserve").unwrap();
    let before = revision(&old.connection().unwrap());
    drop(old);
    gate::LOSE_ACK.store(true, Ordering::SeqCst);
    let error = SqliteStore::open(&path)
        .err()
        .expect("metadata commit acknowledgement is lost");
    assert!(
        matches!(error, Error::CommitOutcomeUnknown { .. }),
        "{error}"
    );
    let reopened = SqliteStore::open(&path).unwrap();
    let conn = reopened.connection().unwrap();
    assert_eq!(revision(&conn), before + 5);
    assert_eq!(
        conn.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        17
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM m6_row_availability WHERE table_name='asset_ownership_schema'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM asset_control_intents", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(conn);
    drop(reopened);
    let again = SqliteStore::open(&path).unwrap();
    assert_eq!(revision(&again.connection().unwrap()), before + 5);
}
