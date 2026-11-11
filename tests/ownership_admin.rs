//! S3b: the real administrative CLI, durable retries, process failure and MCP isolation.
//! These tests trust the local OS account; they do not authenticate a distinct human.
#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use mindspace::core::{AssertionEmbedding, ManualMemoryDraft, Uuid};
use mindspace::ingest;
use mindspace::storage::Store;
use mindspace::{initialize, open_existing};
use rusqlite::Connection;
use serde_json::{json, Value};

struct World {
    dir: tempfile::TempDir,
    db: PathBuf,
    source: Uuid,
    note: PathBuf,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mind.db");
        let note = dir.path().join("legacy.md");
        std::fs::write(&note, "# Legacy\n\nDurable enrollment marker.\n").unwrap();
        initialize(&db).unwrap();
        {
            let store = open_existing(&db).unwrap();
            store.create_space("p", "").unwrap();
            ingest::ingest_path(&store, &note, &["p"]).unwrap();
        }
        let raw = Connection::open(&db).unwrap();
        let source: String = raw
            .query_row(
                "SELECT id FROM logical_sources WHERE kind='file'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        Self {
            dir,
            db,
            source: Uuid::parse_str(&source).unwrap(),
            note,
        }
    }

    fn raw(&self) -> Connection {
        Connection::open(&self.db).unwrap()
    }

    fn revision(&self) -> i64 {
        self.raw()
            .query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| {
                r.get(0)
            })
            .unwrap()
    }

    fn count(&self, table: &str) -> i64 {
        self.raw()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn command(&self) -> Command {
        let mut c = clean_command();
        c.arg("--db").arg(&self.db);
        c
    }

    fn preview_command(&self, source: Uuid, revision: i64, nonce: Uuid) -> Command {
        let mut c = self.command();
        c.args(["ownership", "enroll-legacy", "--source-id"])
            .arg(source.to_string())
            .arg("--expected-revision")
            .arg(revision.to_string())
            .arg("--nonce")
            .arg(nonce.to_string())
            .arg("--json");
        c
    }

    fn preview(&self) -> Value {
        output_json(
            self.preview_command(self.source, self.revision(), Uuid::now_v7())
                .output()
                .unwrap(),
        )
    }

    fn apply_command(&self, plan: &Value) -> Command {
        let mut c = self.command();
        c.args(["ownership", "enroll-legacy", "--source-id"])
            .arg(plan["source_id"].as_str().unwrap())
            .arg("--expected-revision")
            .arg(plan["expected_revision"].as_i64().unwrap().to_string())
            .arg("--operation-id")
            .arg(plan["operation_id"].as_str().unwrap())
            .args(["--apply", "--confirm-intent"])
            .arg(plan["digest"].as_str().unwrap())
            .arg("--json");
        c
    }

    fn apply(&self, plan: &Value) -> Value {
        output_json(self.apply_command(plan).output().unwrap())
    }

    fn unchanged_knowledge(&self) -> BTreeMap<String, Vec<String>> {
        // Compare row contents, including FTS and vectors, rather than only counts.
        let raw = self.raw();
        let tables = raw
            .prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE 'asset_%' AND name NOT IN ('m6_revisions','m6_row_availability','m6_history_coverage') ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        let mut dump = BTreeMap::new();
        for table in tables {
            let cols = raw
                .prepare(&format!("PRAGMA table_info(\"{table}\")"))
                .unwrap()
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>();
            let expr = cols
                .iter()
                .map(|col| format!("quote(\"{col}\")"))
                .collect::<Vec<_>>()
                .join("||'|'||");
            let mut rows = raw
                .prepare(&format!("SELECT {expr} FROM \"{table}\""))
                .unwrap()
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>();
            rows.sort();
            dump.insert(table, rows);
        }
        dump
    }

    fn search(&self) -> Value {
        output_json(
            self.command()
                .args(["search", "enrollment", "--json"])
                .output()
                .unwrap(),
        )
    }
}

fn clean_command() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_mind"));
    c.env_remove("MINDSPACE_DB")
        .env_remove("MINDSPACE_OWNERSHIP_PAUSE")
        .env_remove("MINDSPACE_OWNERSHIP_GATE_PATH")
        .env_remove("MINDSPACE_OWNERSHIP_FAULT")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

fn output_json(out: Output) -> Value {
    assert!(
        out.status.success(),
        "CLI failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid CLI JSON: {e}: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn refused(out: Output) -> String {
    assert!(
        !out.status.success(),
        "unexpected success: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn local_admin_enrollment_changes_only_control_metadata_and_exact_retry_is_noop() {
    let w = World::new();
    let store = open_existing(&w.db).unwrap();
    let assertion: String = w
        .raw()
        .query_row("SELECT id FROM assertions LIMIT 1", [], |r| r.get(0))
        .unwrap();
    store
        .save_embeddings(&[AssertionEmbedding {
            assertion_id: Uuid::parse_str(&assertion).unwrap(),
            derivation_key: "s3b-compatibility-sentinel".into(),
            model: "sentinel".into(),
            model_version: "1".into(),
            dimension: 2,
            embedding: vec![1.0, 0.0],
        }])
        .unwrap();
    drop(store);
    let plan = w.preview();
    let before_revision = w.revision();
    let before = w.unchanged_knowledge();
    let before_search = w.search();
    assert_eq!(w.count("asset_ownership"), 0);
    let receipt = w.apply(&plan);
    assert_eq!(receipt["operation_id"], plan["operation_id"]);
    assert_eq!(receipt["source_id"], plan["source_id"]);
    assert_eq!(receipt["committed_revision"], before_revision + 1);
    assert_eq!(w.revision(), before_revision + 1);
    assert_eq!(w.count("asset_ownership"), 1);
    assert_eq!(w.count("asset_control_intents"), 1);
    assert_eq!(w.unchanged_knowledge(), before);
    assert_eq!(w.search(), before_search);
    assert_eq!(w.apply(&plan), receipt);
    assert_eq!(w.revision(), before_revision + 1);
    let integrity: String = w
        .raw()
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    assert!(!w
        .raw()
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .exists([])
        .unwrap());
}

#[test]
fn exact_committed_replay_precedes_current_source_and_revision_checks() {
    let w = World::new();
    let plan = w.preview();
    let receipt = w.apply(&plan);
    {
        let store = open_existing(&w.db).unwrap();
        store
            .create_space("later", "changes the recording revision")
            .unwrap();
        std::fs::write(&w.note, "# Changed\n\nA later source revision.\n").unwrap();
        ingest::ingest_path(&store, &w.note, &["p", "later"]).unwrap();
    }
    std::fs::remove_file(&w.note).unwrap();
    let revision = w.revision();
    assert_eq!(w.apply(&plan), receipt);
    assert_eq!(w.revision(), revision);
}

#[test]
fn enrolled_legacy_source_keeps_existing_move_absence_and_reversion_behavior() {
    let w = World::new();
    ingest::ingest_path(&open_existing(&w.db).unwrap(), w.dir.path(), &["p"]).unwrap();
    let plan = w.preview();
    let original_receipt = w.apply(&plan);
    let before_search = w.search();
    let moved = w.dir.path().join("moved.md");
    std::fs::rename(&w.note, &moved).unwrap();
    ingest::ingest_path(&open_existing(&w.db).unwrap(), w.dir.path(), &["p"]).unwrap();
    let current_location: String = w
        .raw()
        .query_row(
            "SELECT current_location FROM logical_sources WHERE id=?1",
            [w.source.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(Path::new(&current_location), moved);
    std::fs::remove_file(&moved).unwrap();
    ingest::ingest_path(&open_existing(&w.db).unwrap(), w.dir.path(), &["p"]).unwrap();
    let state: String = w
        .raw()
        .query_row(
            "SELECT state FROM logical_sources WHERE id=?1",
            [w.source.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "absent");
    assert_eq!(w.search(), before_search);
    std::fs::write(&moved, "# Legacy\n\nDurable enrollment marker.\n").unwrap();
    ingest::ingest_path(&open_existing(&w.db).unwrap(), w.dir.path(), &["p"]).unwrap();
    let revision = w.revision();
    assert_eq!(w.apply(&plan), original_receipt);
    assert_eq!(w.revision(), revision);
    assert_eq!(w.count("asset_ownership"), 1);
    assert_eq!(w.count("asset_control_intents"), 1);
}

#[test]
fn changed_full_intent_confirmation_and_committed_nonce_replays_are_refused() {
    let w = World::new();
    let plan = w.preview();
    for key in ["source_id", "expected_revision", "digest", "operation_id"] {
        let mut altered = plan.clone();
        altered[key] = match key {
            "source_id" => json!(Uuid::now_v7().to_string()),
            "expected_revision" => json!(w.revision() + 1),
            "digest" => json!("0".repeat(64)),
            _ => json!(format!("{}0", plan[key].as_str().unwrap())),
        };
        refused(w.apply_command(&altered).output().unwrap());
        assert_eq!(w.count("asset_ownership"), 0);
    }
    let committed = w.apply(&plan);
    for key in ["source_id", "expected_revision"] {
        let mut altered = plan.clone();
        altered[key] = if key == "source_id" {
            json!(Uuid::now_v7().to_string())
        } else {
            json!(w.revision())
        };
        refused(w.apply_command(&altered).output().unwrap());
    }
    assert_eq!(w.apply(&plan), committed);
    assert_eq!(w.count("asset_control_intents"), 1);
}

#[test]
fn a_successful_operation_nonce_cannot_be_rebound_to_a_different_source() {
    let w = World::new();
    let first = w.preview();
    w.apply(&first);
    let nonce = first["operation_id"]
        .as_str()
        .unwrap()
        .split(':')
        .nth(1)
        .unwrap();
    let second_note = w.dir.path().join("second.md");
    std::fs::write(&second_note, "# Second\n\nAnother legacy source.\n").unwrap();
    ingest::ingest_path(&open_existing(&w.db).unwrap(), &second_note, &["p"]).unwrap();
    let second_id: String = w
        .raw()
        .query_row(
            "SELECT id FROM logical_sources WHERE kind='file' AND id<>?1",
            [w.source.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    let revision = w.revision();
    let second = output_json(
        w.preview_command(
            Uuid::parse_str(&second_id).unwrap(),
            revision,
            Uuid::parse_str(nonce).unwrap(),
        )
        .output()
        .unwrap(),
    );
    assert_ne!(first["operation_id"], second["operation_id"]);
    refused(w.apply_command(&second).output().unwrap());
    assert_eq!(w.count("asset_ownership"), 1);
    assert_eq!(w.count("asset_control_intents"), 1);
    assert_eq!(w.revision(), revision);
}

#[test]
fn explicit_existing_database_is_required_and_environment_cannot_select_it() {
    let w = World::new();
    let mut env_only = clean_command();
    env_only.env("MINDSPACE_DB", &w.db).args([
        "ownership",
        "enroll-legacy",
        "--source-id",
        &w.source.to_string(),
        "--expected-revision",
        &w.revision().to_string(),
        "--nonce",
        &Uuid::now_v7().to_string(),
    ]);
    refused(env_only.output().unwrap());
    for target in [
        w.dir.path().join("missing.db"),
        PathBuf::from("relative.db"),
        PathBuf::from(":memory:"),
    ] {
        let out = clean_command()
            .arg("--db")
            .arg(&target)
            .args([
                "ownership",
                "enroll-legacy",
                "--source-id",
                &w.source.to_string(),
                "--expected-revision",
                &w.revision().to_string(),
                "--nonce",
                &Uuid::now_v7().to_string(),
            ])
            .output()
            .unwrap();
        refused(out);
    }
    assert!(!w.dir.path().join("missing.db").exists());
    assert_eq!(w.count("asset_ownership"), 0);
}

#[test]
fn existing_empty_foreign_or_malformed_databases_are_refused_without_any_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let cases = [
        ("zero-byte", None),
        ("arbitrary-text", None),
        ("foreign-sqlite", Some("CREATE TABLE unrelated(value TEXT); INSERT INTO unrelated VALUES ('sentinel')")),
        ("empty-migrations", Some("CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL)")),
        ("malformed-migrations", Some("CREATE TABLE schema_migrations(unrelated TEXT); INSERT INTO schema_migrations VALUES ('sentinel')")),
        ("unrelated-migration-prefix", Some("CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL); INSERT INTO schema_migrations VALUES (1,1)")),
    ];
    for (name, sql) in cases {
        let target = dir.path().join(format!("{name}.db"));
        if let Some(sql) = sql {
            let raw = Connection::open(&target).unwrap();
            raw.execute_batch(sql).unwrap();
            drop(raw);
        } else {
            std::fs::write(
                &target,
                if name == "arbitrary-text" {
                    b"non-SQLite sentinel\n".as_slice()
                } else {
                    b"".as_slice()
                },
            )
            .unwrap();
        }
        let before = std::fs::read(&target).unwrap();
        let metadata = std::fs::metadata(&target).unwrap();
        let modified = metadata.modified().unwrap();
        let wal = PathBuf::from(format!("{}-wal", target.display()));
        let shm = PathBuf::from(format!("{}-shm", target.display()));
        assert!(!wal.exists() && !shm.exists());
        refused(
            clean_command()
                .arg("--db")
                .arg(&target)
                .args([
                    "ownership",
                    "enroll-legacy",
                    "--source-id",
                    &Uuid::now_v7().to_string(),
                    "--expected-revision",
                    "1",
                    "--nonce",
                    &Uuid::now_v7().to_string(),
                    "--json",
                ])
                .output()
                .unwrap(),
        );
        let after = std::fs::metadata(&target).unwrap();
        assert_eq!(
            std::fs::read(&target).unwrap(),
            before,
            "{name}: target bytes changed"
        );
        assert_eq!(after.len(), metadata.len(), "{name}: target size changed");
        assert_eq!(
            after.modified().unwrap(),
            modified,
            "{name}: target mtime changed"
        );
        assert!(!wal.exists(), "{name}: a WAL file was created");
        assert!(!shm.exists(), "{name}: a SHM file was created");
    }
}

#[test]
fn symlink_hardlink_and_unsafe_database_permissions_are_refused() {
    let w = World::new();
    let sym = w.dir.path().join("alias.db");
    symlink(&w.db, &sym).unwrap();
    let hard = w.dir.path().join("hard.db");
    std::fs::hard_link(&w.db, &hard).unwrap();
    for target in [&sym, &hard, &w.db] {
        refused(
            clean_command()
                .arg("--db")
                .arg(target)
                .args([
                    "ownership",
                    "enroll-legacy",
                    "--source-id",
                    &w.source.to_string(),
                    "--expected-revision",
                    &w.revision().to_string(),
                    "--nonce",
                    &Uuid::now_v7().to_string(),
                ])
                .output()
                .unwrap(),
        );
    }
    std::fs::remove_file(hard).unwrap();
    std::fs::set_permissions(&w.db, std::fs::Permissions::from_mode(0o666)).unwrap();
    refused(
        w.preview_command(w.source, w.revision(), Uuid::now_v7())
            .output()
            .unwrap(),
    );
    std::fs::set_permissions(&w.db, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(w.dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    refused(
        w.preview_command(w.source, w.revision(), Uuid::now_v7())
            .output()
            .unwrap(),
    );
    std::fs::set_permissions(w.dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn cloned_database_does_not_accept_an_operation_bound_to_original_file_identity() {
    let w = World::new();
    let plan = w.preview();
    let other = World::new();
    // Close all SQLite handles before making this disposable physical copy.
    std::fs::copy(&w.db, &other.db).unwrap();
    refused(other.apply_command(&plan).output().unwrap());
    assert_eq!(other.count("asset_ownership"), 0);
    assert_eq!(w.count("asset_ownership"), 0);
}

#[test]
fn missing_and_incompatible_source_kinds_never_enroll() {
    let w = World::new();
    let store = open_existing(&w.db).unwrap();
    let space = store.get_space("p").unwrap().id;
    store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("manual".into()),
            content: "manual authority is not enrollment".into(),
            spaces: vec![space],
            supersedes: None,
        })
        .unwrap();
    ingest::document::ingest_document_file(&store, &w.note, &Default::default()).unwrap();
    drop(store);
    let ids = w
        .raw()
        .prepare("SELECT id FROM logical_sources WHERE kind<>'file'")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    assert!(ids.len() >= 2);
    let revision = w.revision();
    for id in ids
        .into_iter()
        .map(|s| Uuid::parse_str(&s).unwrap())
        .chain([Uuid::now_v7()])
    {
        refused(
            w.preview_command(id, revision, Uuid::now_v7())
                .output()
                .unwrap(),
        );
    }
    assert_eq!(w.revision(), revision);
    assert_eq!(w.count("asset_ownership"), 0);
}

#[test]
fn stale_uncommitted_intent_does_not_silently_rebase() {
    let w = World::new();
    let plan = w.preview();
    open_existing(&w.db)
        .unwrap()
        .create_space("later", "")
        .unwrap();
    let revision = w.revision();
    refused(w.apply_command(&plan).output().unwrap());
    assert_eq!(w.revision(), revision);
    assert_eq!(w.count("asset_control_intents"), 0);
}

#[test]
fn ordinary_metadata_and_forged_administrative_arguments_cannot_enroll() {
    let w = World::new();
    let store = open_existing(&w.db).unwrap();
    for key in [
        "ownership.owner",
        "ownership.authorized",
        "ownership.principal",
        "ownership.operation",
    ] {
        store.set_meta(key, "legacy:approved").unwrap();
    }
    drop(store);
    let revision = w.revision();
    for extra in [
        "--principal",
        "--actor-uid",
        "--publish",
        "--owner",
        "--yes",
    ] {
        let out = w
            .preview_command(w.source, revision, Uuid::now_v7())
            .arg(extra)
            .arg("legacy")
            .output()
            .unwrap();
        refused(out);
    }
    assert_eq!(w.count("asset_ownership"), 0);
    assert_eq!(w.revision(), revision);
}

#[test]
fn receipts_and_owner_rows_are_immutable_and_publication_columns_do_not_exist() {
    let w = World::new();
    w.apply(&w.preview());
    let raw = w.raw();
    for sql in [
        "UPDATE asset_ownership SET owner='document'",
        "UPDATE asset_ownership SET epoch=2",
        "DELETE FROM asset_ownership",
        "DELETE FROM asset_control_intents",
        "UPDATE asset_control_intents SET operation_id='forged'",
        "UPDATE asset_ownership SET publication_state='active'",
        "UPDATE asset_ownership SET document_logical_id='forged'",
    ] {
        assert!(
            raw.execute(sql, []).is_err(),
            "raw mutation accepted: {sql}"
        );
    }
    assert_eq!(w.count("asset_ownership"), 1);
    assert_eq!(w.count("asset_control_intents"), 1);
}

#[test]
fn six_process_exact_retries_have_one_durable_effect() {
    let w = World::new();
    let plan = w.preview();
    let revision = w.revision();
    let children = (0..6)
        .map(|_| w.apply_command(&plan).spawn().unwrap())
        .collect::<Vec<_>>();
    let receipts = children
        .into_iter()
        .map(|c| output_json(c.wait_with_output().unwrap()))
        .collect::<Vec<_>>();
    assert!(receipts.iter().all(|r| r == &receipts[0]));
    assert_eq!(w.revision(), revision + 1);
    assert_eq!(w.count("asset_control_intents"), 1);
    assert_eq!(w.count("asset_ownership"), 1);
}

#[test]
fn competing_confirmed_intents_for_one_source_have_one_winner() {
    let w = World::new();
    let plans = (0..6).map(|_| w.preview()).collect::<Vec<_>>();
    let revision = w.revision();
    let children = plans
        .iter()
        .map(|p| w.apply_command(p).spawn().unwrap())
        .collect::<Vec<_>>();
    let outcomes = children
        .into_iter()
        .map(|c| c.wait_with_output().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(outcomes.iter().filter(|o| o.status.success()).count(), 1);
    assert_eq!(w.revision(), revision + 1);
    assert_eq!(w.count("asset_control_intents"), 1);
}

struct Paused {
    child: Option<Child>,
    prefix: PathBuf,
}

impl Paused {
    fn start(w: &World, plan: &Value, phase: &str) -> Self {
        let prefix = w.dir.path().join(format!("pause-{phase}"));
        let child = w
            .apply_command(plan)
            .env("MINDSPACE_OWNERSHIP_PAUSE", phase)
            .env("MINDSPACE_OWNERSHIP_GATE_PATH", &prefix)
            .spawn()
            .unwrap();
        let mut paused = Self {
            child: Some(child),
            prefix,
        };
        let end = Instant::now() + Duration::from_secs(10);
        while !paused.marker("reached").exists() {
            if let Some(status) = paused.child.as_mut().unwrap().try_wait().unwrap() {
                let out = paused.child.take().unwrap().wait_with_output().unwrap();
                panic!(
                    "child exited before {phase} gate ({status}): {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            assert!(Instant::now() < end, "{phase} gate was not reached");
            std::thread::sleep(Duration::from_millis(5));
        }
        paused
    }

    fn marker(&self, extension: &str) -> PathBuf {
        let mut name = self.prefix.as_os_str().to_owned();
        name.push(format!(".{extension}"));
        name.into()
    }

    fn release(mut self) -> Output {
        std::fs::write(self.marker("go"), b"").unwrap();
        self.child.take().unwrap().wait_with_output().unwrap()
    }

    fn kill(mut self) {
        let mut child = self.child.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
    }
}

impl Drop for Paused {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn paused_prebegin_writer_returns_original_receipt_after_retry_wins() {
    let w = World::new();
    let plan = w.preview();
    let revision = w.revision();
    let old = Paused::start(&w, &plan, "before_begin");
    let winner = w.apply(&plan);
    assert_eq!(output_json(old.release()), winner);
    assert_eq!(w.revision(), revision + 1);
    assert_eq!(w.count("asset_control_intents"), 1);
}

#[test]
fn process_death_before_or_during_commit_rolls_back_and_retry_has_one_effect() {
    for phase in ["before_begin", "after_begin", "after_write", "in_commit"] {
        let w = World::new();
        let plan = w.preview();
        let revision = w.revision();
        let before = w.unchanged_knowledge();
        let staged = Paused::start(&w, &plan, phase);
        staged.kill();
        assert_eq!(w.count("asset_ownership"), 0, "{phase}");
        assert_eq!(w.count("asset_control_intents"), 0, "{phase}");
        assert_eq!(w.revision(), revision, "{phase}");
        assert_eq!(w.unchanged_knowledge(), before, "{phase}");
        w.apply(&plan);
        assert_eq!(w.revision(), revision + 1, "{phase}");
    }
}

#[test]
fn process_death_after_commit_preserves_receipt_and_restart_retry_spends_no_revision() {
    let w = World::new();
    let plan = w.preview();
    let revision = w.revision();
    // The real COMMIT completed, but the caller has not received a result.
    // Silence is an unknown outcome to that caller, not evidence of rollback.
    let mut pending = Paused::start(&w, &plan, "after_commit");
    assert_eq!(w.count("asset_ownership"), 1);
    assert_eq!(w.count("asset_control_intents"), 1);
    assert_eq!(w.revision(), revision + 1);
    let mut child = pending.child.take().unwrap();
    child.kill().unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "a receipt escaped before the output gate"
    );
    let receipt = w.apply(&plan);
    assert_eq!(receipt["operation_id"], plan["operation_id"]);
    assert_eq!(receipt["committed_revision"], revision + 1);
    assert_eq!(w.revision(), revision + 1);
    assert_eq!(w.count("asset_ownership"), 1);
    assert_eq!(w.count("asset_control_intents"), 1);
}

#[test]
fn inflight_writer_blocks_retry_without_claiming_previous_rollback() {
    let w = World::new();
    let plan = w.preview();
    let old = Paused::start(&w, &plan, "after_write");
    let error = refused(w.apply_command(&plan).output().unwrap()).to_lowercase();
    assert!(
        error.contains("busy") || error.contains("unknown") || error.contains("blocked"),
        "{error}"
    );
    assert!(!error.contains("rolled back"), "{error}");
    let committed = output_json(old.release());
    assert_eq!(w.apply(&plan), committed);
    assert_eq!(w.count("asset_control_intents"), 1);
}

#[test]
fn veto_is_known_noncommit_but_lost_ack_preserves_durable_receipt() {
    let w = World::new();
    let plan = w.preview();
    let revision = w.revision();
    let error = refused(
        w.apply_command(&plan)
            .env("MINDSPACE_OWNERSHIP_FAULT", "veto")
            .output()
            .unwrap(),
    );
    assert!(!error.to_lowercase().contains("unknown"), "{error}");
    assert_eq!(w.count("asset_control_intents"), 0);
    assert_eq!(w.revision(), revision);
    let error = refused(
        w.apply_command(&plan)
            .env("MINDSPACE_OWNERSHIP_FAULT", "lost_ack")
            .output()
            .unwrap(),
    );
    assert!(error.to_lowercase().contains("unknown"), "{error}");
    assert_eq!(w.count("asset_control_intents"), 1);
    assert_eq!(w.count("asset_ownership"), 1);
    assert_eq!(w.revision(), revision + 1);
    let recovered = w.apply(&plan);
    assert_eq!(recovered["operation_id"], plan["operation_id"]);
    assert_eq!(recovered["committed_revision"], revision + 1);
    assert_eq!(w.revision(), revision + 1);
}

#[test]
fn real_mcp_transport_has_no_enrollment_route_before_or_after_local_enrollment() {
    let w = World::new();
    let before = mcp_probe(&w.db);
    assert!(before["unknown"]["error"].is_object());
    let tools = before["tools"]["result"]["tools"].as_array().unwrap();
    assert!(tools
        .iter()
        .all(|t| !t["name"].as_str().unwrap().contains("ownership")));
    w.apply(&w.preview());
    assert_eq!(mcp_probe(&w.db), before);
    assert_eq!(w.count("asset_control_intents"), 1);
}

fn mcp_probe(db: &Path) -> Value {
    let mut child = clean_command()
        .arg("--db")
        .arg(db)
        .arg("mcp")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let result = {
        let mut send = |v: Value| -> Value {
            writeln!(input, "{v}").unwrap();
            input.flush().unwrap();
            if v["method"] == "notifications/initialized" {
                return Value::Null;
            }
            let mut line = String::new();
            assert_ne!(output.read_line(&mut line).unwrap(), 0);
            serde_json::from_str(&line).unwrap()
        };
        let init = send(
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"untrusted-owner-client","version":"1"}}}),
        );
        assert!(init["result"].is_object());
        send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        let tools = send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
        let unknown = send(
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"ownership_enroll_legacy","arguments":{"principal":"operator","owner":"legacy","apply":true}}}),
        );
        json!({"tools":tools,"unknown":unknown})
    };
    drop(input);
    assert!(child.wait().unwrap().success());
    result
}

fn refuse_preview_of(target: &Path) {
    refused(
        clean_command()
            .arg("--db")
            .arg(target)
            .args([
                "ownership",
                "enroll-legacy",
                "--source-id",
                "02878f25-c7da-43e8-a64a-777c2c391034",
                "--expected-revision",
                "1",
                "--nonce",
                "2b269047-f01c-4533-81b5-a73edce5bac9",
                "--json",
            ])
            .output()
            .unwrap(),
    );
}

#[test]
fn foreign_hot_rollback_journal_is_not_recovered_on_refusal() {
    if let Some(path) = std::env::var_os("MINDSPACE_REVIEW_HOT_CHILD") {
        let raw = Connection::open(path).unwrap();
        raw.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA cache_size=8; BEGIN IMMEDIATE; UPDATE unrelated SET value=randomblob(16384)").unwrap();
        // Exit without Rust destructors: leave dirty spilled pages and a genuine hot journal.
        std::process::exit(0);
    }
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("foreign.db");
    let raw = Connection::open(&target).unwrap();
    raw.execute_batch("CREATE TABLE unrelated(id INTEGER PRIMARY KEY,value BLOB); WITH RECURSIVE rows(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM rows WHERE n<192) INSERT INTO unrelated SELECT n,zeroblob(16384) FROM rows").unwrap();
    drop(raw);
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "foreign_hot_rollback_journal_is_not_recovered_on_refusal",
            "--nocapture",
        ])
        .env("MINDSPACE_REVIEW_HOT_CHILD", &target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let journal = target.with_file_name("foreign.db-journal");
    let hot = std::fs::read(&journal).unwrap();
    assert_eq!(
        &hot[..8],
        &[0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7],
        "fixture must contain a real hot journal"
    );
    for journal_bytes in [hot, Vec::new()] {
        // Both hot and cold/PERSIST journal entries must remain untouched on refusal.
        std::fs::write(&journal, &journal_bytes).unwrap();
        let before = std::fs::read(&target).unwrap();
        let modified = std::fs::metadata(&target).unwrap().modified().unwrap();
        refuse_preview_of(&target);
        assert_eq!(std::fs::read(&target).unwrap(), before);
        assert_eq!(
            std::fs::metadata(&target).unwrap().modified().unwrap(),
            modified
        );
        assert_eq!(std::fs::read(&journal).unwrap(), journal_bytes);
    }
}

#[test]
fn foreign_wal_preflight_preserves_main_and_does_not_repair_missing_shm() {
    if let Some(path) = std::env::var_os("MINDSPACE_REVIEW_WAL_CHILD") {
        let raw = Connection::open(path).unwrap();
        raw.execute_batch(
            "PRAGMA journal_mode=WAL; INSERT INTO unrelated VALUES('committed WAL sentinel')",
        )
        .unwrap();
        std::process::exit(0);
    }
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("foreign.db");
    let wal = target.with_file_name("foreign.db-wal");
    let shm = target.with_file_name("foreign.db-shm");
    let raw = Connection::open(&target).unwrap();
    raw.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE unrelated(value TEXT)")
        .unwrap();
    drop(raw);
    assert!(!wal.exists() && !shm.exists());
    let before = std::fs::read(&target).unwrap();
    let modified = std::fs::metadata(&target).unwrap().modified().unwrap();
    refuse_preview_of(&target);
    assert_eq!(std::fs::read(&target).unwrap(), before);
    assert_eq!(
        std::fs::metadata(&target).unwrap().modified().unwrap(),
        modified
    );
    assert!(!wal.exists() && !shm.exists());
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "foreign_wal_preflight_preserves_main_and_does_not_repair_missing_shm",
            "--nocapture",
        ])
        .env("MINDSPACE_REVIEW_WAL_CHILD", &target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(wal.exists() && shm.exists());
    // A real foreign WAL pair can be inspected read-only without checkpointing its main DB.
    let before = std::fs::read(&target).unwrap();
    let wal_before = std::fs::read(&wal).unwrap();
    refuse_preview_of(&target);
    assert_eq!(std::fs::read(&target).unwrap(), before);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_before);
    std::fs::remove_file(&shm).unwrap();
    refuse_preview_of(&target);
    assert_eq!(std::fs::read(&target).unwrap(), before);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_before);
    assert!(
        !shm.exists(),
        "refusal must not repair an unsupported missing SHM"
    );
}

// These are deliberately synthetic metadata-transition fixtures. No SQLite
// connection owns the synthetic WAL, and the administrative child pauses before
// it opens SQLite. Real native last-close ordering is separately exercised by
// the retained controlled-unlink experiment, not inferred from this fixture.
fn synthetic_wal(w: &World) -> PathBuf {
    let path = w.db.with_file_name("mind.db-wal");
    assert!(!path.exists());
    assert!(!w.db.with_file_name("mind.db-shm").exists());
    std::fs::write(&path, b"").unwrap();
    path
}

#[derive(Debug, PartialEq, Eq)]
struct PhysicalState {
    bytes: Vec<u8>,
    dev: u64,
    ino: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

fn physical_state(path: &Path) -> PhysicalState {
    let bytes = std::fs::read(path).unwrap();
    let metadata = std::fs::metadata(path).unwrap();
    PhysicalState {
        bytes,
        dev: metadata.dev(),
        ino: metadata.ino(),
        mode: metadata.mode(),
        uid: metadata.uid(),
        gid: metadata.gid(),
        mtime: (metadata.mtime(), metadata.mtime_nsec()),
        ctime: (metadata.ctime(), metadata.ctime_nsec()),
    }
}

fn immutable_revision(path: &Path) -> i64 {
    let uri = format!("file:{}?immutable=1", path.display());
    Connection::open_with_flags(
        uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap()
    .query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |row| {
        row.get(0)
    })
    .unwrap()
}

#[test]
fn transient_missing_shm_repeats_admission_without_losing_the_exact_intent() {
    let w = World::new();
    let plan = w.preview();
    let revision = w.revision();
    let knowledge = w.unchanged_knowledge();
    let before = physical_state(&w.db);
    let wal = synthetic_wal(&w);
    let old = Paused::start(&w, &plan, "missing_shm");
    assert_eq!(physical_state(&w.db), before);
    assert!(!w.db.with_file_name("mind.db-shm").exists());
    // Retain the synthetic entry rather than deleting any real SQLite sidecar.
    std::fs::rename(&wal, w.dir.path().join("synthetic-wal-retained")).unwrap();
    let committed = output_json(old.release());
    assert_eq!(committed["operation_id"], plan["operation_id"]);
    assert_eq!(committed["committed_revision"], revision + 1);
    assert_eq!(w.revision(), revision + 1);
    assert_eq!(w.count("asset_control_intents"), 1);
    assert_eq!(w.count("asset_ownership"), 1);
    assert_eq!(w.unchanged_knowledge(), knowledge);
    assert_eq!(w.apply(&plan), committed);
    assert_eq!(w.revision(), revision + 1);
}

#[test]
fn stable_missing_shm_is_bounded_and_never_repaired_or_written() {
    let w = World::new();
    let plan = w.preview();
    let revision = w.revision();
    let wal = synthetic_wal(&w);
    let before = physical_state(&w.db);
    let wal_before = physical_state(&wal);
    let started = Instant::now();
    let error = refused(w.apply_command(&plan).output().unwrap());
    assert!(
        error.contains("WAL without an existing shared-memory file"),
        "{error}"
    );
    assert!(
        started.elapsed() >= Duration::from_secs(4),
        "missing-SHM retry did not wait for its bounded deadline"
    );
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "missing-SHM retry exceeded its bound"
    );
    assert_eq!(physical_state(&w.db), before);
    assert_eq!(physical_state(&wal), wal_before);
    assert!(!w.db.with_file_name("mind.db-shm").exists());
    assert_eq!(immutable_revision(&w.db), revision);
}

#[test]
fn missing_shm_retry_revalidates_identity_journal_and_nonregular_sidecar() {
    for hazard in ["identity", "journal", "directory", "permissions"] {
        let w = World::new();
        let plan = w.preview();
        let revision = w.revision();
        let wal = synthetic_wal(&w);
        let old = Paused::start(&w, &plan, "missing_shm");
        let extra = match hazard {
            "identity" => {
                let retained = w.dir.path().join("retained-original.db");
                std::fs::rename(&w.db, &retained).unwrap();
                std::fs::copy(&retained, &w.db).unwrap();
                Some(retained)
            }
            "journal" => {
                let journal = w.db.with_file_name("mind.db-journal");
                // Existing admission rejects even a cold journal; this does not
                // claim to synthesize hot-journal recovery or its contents.
                std::fs::write(&journal, b"").unwrap();
                Some(journal)
            }
            "directory" => {
                std::fs::create_dir(w.db.with_file_name("mind.db-shm")).unwrap();
                None
            }
            "permissions" => {
                std::fs::set_permissions(&wal, std::fs::Permissions::from_mode(0o666)).unwrap();
                None
            }
            _ => unreachable!(),
        };
        let before = physical_state(&w.db);
        let wal_before = physical_state(&wal);
        let extra_before = extra.as_ref().map(|path| physical_state(path));
        let started = Instant::now();
        let error = refused(old.release());
        match hazard {
            "identity" => assert!(error.contains("changed"), "{error}"),
            "journal" => assert!(error.contains("rollback-journal"), "{error}"),
            "directory" => assert!(
                error.contains("SQLite sidecar must be a regular"),
                "{error}"
            ),
            "permissions" => assert!(error.contains("not writable by group/others"), "{error}"),
            _ => unreachable!(),
        }
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "invalid state entered the missing-SHM wait: {hazard}"
        );
        assert_eq!(physical_state(&w.db), before);
        assert_eq!(physical_state(&wal), wal_before);
        if let Some(path) = extra {
            assert_eq!(Some(physical_state(&path)), extra_before);
        }
        assert_eq!(immutable_revision(&w.db), revision);
    }
}
