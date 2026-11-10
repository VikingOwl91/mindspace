//! Passive administrative plans. Synthetic vectors test isolation, never real-model correctness.
//! Model A trusts the local account; these tests do not authenticate distinct humans.
#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use mindspace::core::{SearchQuery, Uuid};
use mindspace::ingest::{self, document};
use mindspace::storage::Store;
use mindspace::{initialize, open_existing};
use rusqlite::Connection;
use serde_json::{json, Value};

const TEXT: &str =
    "# Passive planning\r\n\r\nKnowledge remains searchable 中文 😀 e\u{301} \u{200b}\u{202e}.\r\n";
const PLANS: &str = "asset_fence_plans";
const BINDINGS: &str = "asset_fence_plan_bindings";
const RECEIPTS: &str = "asset_fence_plan_receipts";

struct World {
    dir: tempfile::TempDir,
    db: PathBuf,
    note: PathBuf,
    source: Uuid,
    target: Value,
}
impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mind.db");
        let note = dir.path().join("legacy.md");
        std::fs::write(&note, TEXT).unwrap();
        initialize(&db).unwrap();
        let store = open_existing(&db).unwrap();
        store.create_space("p", "").unwrap();
        ingest::ingest_path(&store, &note, &["p"]).unwrap();
        let raw = Connection::open(&db).unwrap();
        let (source, fs): (String, String) = raw.query_row(
            "SELECT l.id,s.id FROM logical_sources l JOIN sources s ON s.logical_id=l.id WHERE l.kind='file'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        let report = document::ingest_document_file(&store, &note, &Default::default()).unwrap();
        drop(store);
        drop(raw);
        let mut w = Self {
            dir,
            db,
            note,
            source: Uuid::parse_str(&source).unwrap(),
            target: Value::Null,
        };
        let revision = w.revision();
        let mut preview = w.command();
        preview.args([
            "ownership",
            "enroll-legacy",
            "--source-id",
            &source,
            "--expected-revision",
            &revision.to_string(),
            "--nonce",
            &Uuid::now_v7().to_string(),
            "--json",
        ]);
        let p = output_json(preview.output().unwrap());
        let mut apply = w.command();
        apply.args([
            "ownership",
            "enroll-legacy",
            "--source-id",
            &source,
            "--expected-revision",
            &revision.to_string(),
            "--operation-id",
            p["operation_id"].as_str().unwrap(),
            "--apply",
            "--confirm-intent",
            p["digest"].as_str().unwrap(),
            "--json",
        ]);
        let enrolled = output_json(apply.output().unwrap());
        w.target = json!({"asset_id":enrolled["asset_id"],"legacy_source_version":fs,
            "document_id":report.persisted.logical_id.to_string(),
            "document_source_version":report.persisted.source_id.to_string(),
            "extraction_id":report.persisted.extraction_id.to_string()});
        let r = w.revision();
        let mut p = w.selected_command("enroll-document-preparation", &w.target, r);
        p.args(["--nonce", &Uuid::now_v7().to_string()]);
        let prepared = output_json(p.output().unwrap());
        let mut c = w.selected_command("enroll-document-preparation", &w.target, r);
        c.args([
            "--operation-id",
            prepared["operation_id"].as_str().unwrap(),
            "--apply",
            "--confirm-intent",
            prepared["digest"].as_str().unwrap(),
        ]);
        output_json(c.output().unwrap());
        w
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
    fn selected_command(&self, operation: &str, target: &Value, revision: i64) -> Command {
        let mut c = self.command();
        c.args(["ownership", operation]);
        for (flag, key) in [
            ("--asset-id", "asset_id"),
            ("--legacy-source-version", "legacy_source_version"),
            ("--document-id", "document_id"),
            ("--document-source-version", "document_source_version"),
            ("--extraction-id", "extraction_id"),
        ] {
            c.arg(flag).arg(target[key].as_str().unwrap());
        }
        c.arg("--expected-revision")
            .arg(revision.to_string())
            .arg("--json");
        c
    }
    fn preview_command(
        &self,
        target: &Value,
        revision: i64,
        nonce: Uuid,
        bindings: &Value,
    ) -> Command {
        let mut c = self.selected_command("prepare-fence-plan", target, revision);
        c.arg("--bindings-json")
            .arg(bindings.to_string())
            .arg("--nonce")
            .arg(nonce.to_string());
        c
    }
    fn preview_bindings(&self, bindings: Value) -> Value {
        let r = self.revision();
        let mut p = output_json(
            self.preview_command(&self.target, r, Uuid::now_v7(), &bindings)
                .output()
                .unwrap(),
        );
        p["test_target"] = self.target.clone();
        p["test_revision"] = r.into();
        p["test_bindings"] = bindings;
        p
    }
    fn preview(&self) -> Value {
        self.preview_bindings(json!([]))
    }
    fn apply_command(&self, p: &Value) -> Command {
        let mut c = self.selected_command(
            "prepare-fence-plan",
            &p["test_target"],
            p["test_revision"].as_i64().unwrap(),
        );
        c.arg("--bindings-json")
            .arg(p["test_bindings"].to_string())
            .args([
                "--operation-id",
                p["operation_id"].as_str().unwrap(),
                "--apply",
                "--confirm-intent",
                p["digest"].as_str().unwrap(),
            ]);
        c
    }
    fn apply(&self, p: &Value) -> Value {
        output_json(self.apply_command(p).output().unwrap())
    }
    fn inspect(&self, p: &Value) -> Value {
        output_json(
            self.command()
                .args([
                    "ownership",
                    "inspect-fence-plan",
                    "--operation-id",
                    p["operation_id"].as_str().unwrap(),
                    "--json",
                ])
                .output()
                .unwrap(),
        )
    }
    fn preserved(&self) -> BTreeMap<String, Vec<String>> {
        let raw = self.raw();
        let tables=raw.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE 'asset_fence_%' AND name NOT IN ('m6_revisions','m6_row_availability') ORDER BY name").unwrap()
            .query_map([],|r|r.get::<_,String>(0)).unwrap().map(Result::unwrap).collect::<Vec<_>>();
        tables
            .into_iter()
            .map(|table| {
                let cols = raw
                    .prepare(&format!("PRAGMA table_info(\"{table}\")"))
                    .unwrap()
                    .query_map([], |r| r.get::<_, String>(1))
                    .unwrap()
                    .map(Result::unwrap)
                    .collect::<Vec<_>>();
                let expr = cols
                    .iter()
                    .map(|c| format!("quote(\"{c}\")"))
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
                (table, rows)
            })
            .collect()
    }
    fn assert_empty(&self, r: i64) {
        assert_eq!(self.count(PLANS), 0);
        assert_eq!(self.count(BINDINGS), 0);
        assert_eq!(self.count(RECEIPTS), 0);
        assert_eq!(self.revision(), r);
    }
}
fn clean_command() -> Command {
    let mut c = Command::new(
        std::env::var_os("MINDSPACE_S3E2_TEST_BINARY")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_mind").into()),
    );
    for name in [
        "MINDSPACE_DB",
        "MINDSPACE_OWNERSHIP_PAUSE",
        "MINDSPACE_OWNERSHIP_GATE_PATH",
        "MINDSPACE_OWNERSHIP_FAULT",
        "MINDSPACE_PREPARATION_PAUSE",
        "MINDSPACE_PREPARATION_GATE_PATH",
        "MINDSPACE_PREPARATION_FAULT",
        "MINDSPACE_FENCE_PLAN_PAUSE",
        "MINDSPACE_FENCE_PLAN_GATE_PATH",
        "MINDSPACE_FENCE_PLAN_FAULT",
    ] {
        c.env_remove(name);
    }
    c.stdin(Stdio::null())
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
    serde_json::from_slice(&out.stdout).unwrap()
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
fn explicit_passive_plan_and_exact_replay_change_only_new_metadata() {
    let w = World::new();
    let before = w.preserved();
    let r = w.revision();
    let p = w.preview();
    assert_eq!(p["state"], "passive");
    assert_eq!(p["active_fence"], false);
    assert_eq!(p["witnesses_unverified"], true);
    w.assert_empty(r);
    let receipt = w.apply(&p);
    assert_eq!(receipt["operation_id"], p["operation_id"]);
    assert_eq!(receipt["plan_id"], p["plan_id"]);
    assert_eq!(receipt["committed_revision"], r + 1);
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.count(PLANS), 1);
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.count(BINDINGS), 0);
    assert_eq!(w.apply(&p), receipt);
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.preserved(), before);
    let r = w.revision();
    let inspection = w.inspect(&p);
    assert_eq!(inspection["receipt"], receipt);
    assert_eq!(inspection["active_fence"], false);
    assert_eq!(inspection["witnesses_unverified"], true);
    assert_eq!(w.revision(), r);
    assert_eq!(
        w.raw()
            .query_row("SELECT owner||':'||epoch FROM asset_ownership", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
        "legacy:1"
    );
    assert_eq!(
        w.raw()
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        0
    );
}

#[test]
fn explicit_binding_is_unverified_and_does_not_reserve_a_locator() {
    let w = World::new();
    let binding = json!([{"namespace":"file-absolute@1","key":w.note.to_str().unwrap(),"witness_digest":"a".repeat(64)}]);
    let first = w.preview_bindings(binding.clone());
    w.apply(&first);
    let second = w.preview_bindings(binding);
    w.apply(&second);
    assert_eq!(w.count(PLANS), 2);
    assert_eq!(w.count(BINDINGS), 2);
    assert_eq!(w.count(RECEIPTS), 2);
    assert_ne!(first["plan_id"], second["plan_id"]);
    let store = open_existing(&w.db).unwrap();
    ingest::ingest_path(&store, &w.note, &["p"]).unwrap();
    std::fs::write(
        &w.note,
        "# Changed\n\nKnowledge still writable after passive plans.\n",
    )
    .unwrap();
    ingest::ingest_path(&store, &w.note, &["p"]).unwrap();
    let unrelated = w.dir.path().join("unrelated.md");
    std::fs::write(&unrelated, "# Other\n\nUnrelated knowledge.\n").unwrap();
    ingest::ingest_path(&store, &unrelated, &["p"]).unwrap();
    assert!(
        store
            .search(&SearchQuery {
                text: "knowledge".into(),
                space_ids: None,
                limit: 50,
                at: None,
                include_retired: false,
                include_superseded: false
            })
            .unwrap()
            .len()
            >= 2
    );
    assert_eq!(w.inspect(&first)["freshness"], "Stale");
    let r = w.revision();
    let receipt = w.apply(&first);
    assert_eq!(receipt["operation_id"], first["operation_id"]);
    assert_eq!(w.revision(), r);
}

#[test]
fn missing_sources_and_incompatible_kind_or_version_never_create_a_plan() {
    let w = World::new();
    let r = w.revision();
    for key in [
        "asset_id",
        "legacy_source_version",
        "document_id",
        "document_source_version",
        "extraction_id",
    ] {
        let mut t = w.target.clone();
        t[key] = Uuid::now_v7().to_string().into();
        refused(
            w.preview_command(&t, r, Uuid::now_v7(), &json!([]))
                .output()
                .unwrap(),
        );
        w.assert_empty(r);
    }
    for (key, other) in [
        ("document_id", w.source.to_string()),
        (
            "document_source_version",
            w.target["legacy_source_version"].as_str().unwrap().into(),
        ),
        (
            "legacy_source_version",
            w.target["document_source_version"].as_str().unwrap().into(),
        ),
    ] {
        let mut t = w.target.clone();
        t[key] = other.into();
        refused(
            w.preview_command(&t, r, Uuid::now_v7(), &json!([]))
                .output()
                .unwrap(),
        );
        w.assert_empty(r);
    }
}

#[test]
fn malformed_binding_authority_fields_and_resource_overflow_are_rejected() {
    let w = World::new();
    let r = w.revision();
    let cases=[json!([{"namespace":"file-absolute@1","key":"relative.md","witness_digest":"a".repeat(64)}]),
        json!([{"namespace":"file-absolute@1","key":"/notes/../alias.md","witness_digest":"a".repeat(64)}]),
        json!([{"namespace":"opaque@1","key":"candidate","witness_digest":"bad"}]),
        json!([{"namespace":"opaque@1","key":"candidate","witness_digest":"a".repeat(64),"owner":"document"}]),
        json!((0..129).map(|i|json!({"namespace":"opaque@1","key":format!("{i}"),"witness_digest":"a".repeat(64)})).collect::<Vec<_>>())];
    for b in cases {
        refused(
            w.preview_command(&w.target, r, Uuid::now_v7(), &b)
                .output()
                .unwrap(),
        );
        w.assert_empty(r);
    }
    let mut c = w.preview_command(&w.target, r, Uuid::now_v7(), &json!([]));
    c.arg("--owner").arg("document");
    refused(c.output().unwrap());
    let huge =
        json!([{"namespace":"opaque@1","key":"x".repeat(65536),"witness_digest":"a".repeat(64)}]);
    refused(
        w.preview_command(&w.target, r, Uuid::now_v7(), &huge)
            .output()
            .unwrap(),
    );
    w.assert_empty(r);
}

#[test]
fn missing_foreign_or_unsafe_database_and_environment_selection_are_refused() {
    let w = World::new();
    let r = w.revision();
    let args = w
        .preview_command(&w.target, r, Uuid::now_v7(), &json!([]))
        .get_args()
        .skip(2)
        .map(|x| x.to_owned())
        .collect::<Vec<_>>();
    let missing = w.dir.path().join("missing.db");
    refused(
        clean_command()
            .arg("--db")
            .arg(&missing)
            .args(&args)
            .output()
            .unwrap(),
    );
    assert!(!missing.exists());
    for (name, bytes) in [
        ("empty.db", Vec::new()),
        ("foreign.db", b"not a database".to_vec()),
    ] {
        let db = w.dir.path().join(name);
        std::fs::write(&db, &bytes).unwrap();
        refused(
            clean_command()
                .arg("--db")
                .arg(&db)
                .args(&args)
                .output()
                .unwrap(),
        );
        assert_eq!(std::fs::read(db).unwrap(), bytes);
    }
    let alias = w.dir.path().join("alias.db");
    symlink(&w.db, &alias).unwrap();
    refused(
        clean_command()
            .arg("--db")
            .arg(alias)
            .args(&args)
            .output()
            .unwrap(),
    );
    refused(
        clean_command()
            .env("MINDSPACE_DB", &w.db)
            .args(&args)
            .output()
            .unwrap(),
    );
    std::fs::set_permissions(&w.db, std::fs::Permissions::from_mode(0o666)).unwrap();
    refused(
        w.preview_command(&w.target, r, Uuid::now_v7(), &json!([]))
            .output()
            .unwrap(),
    );
    std::fs::set_permissions(&w.db, std::fs::Permissions::from_mode(0o600)).unwrap();
    w.assert_empty(r);
}

#[test]
fn altered_target_binding_nonce_or_confirmation_conflicts_before_and_after_commit() {
    let w = World::new();
    let p = w.preview();
    let r = w.revision();
    for committed in [false, true] {
        if committed {
            w.apply(&p);
        }
        let current = w.revision();
        for key in [
            "asset_id",
            "legacy_source_version",
            "document_id",
            "document_source_version",
            "extraction_id",
        ] {
            let mut changed = p.clone();
            changed["test_target"][key] = Uuid::now_v7().to_string().into();
            refused(w.apply_command(&changed).output().unwrap());
            assert_eq!(w.revision(), current);
        }
        let mut changed = p.clone();
        changed["test_bindings"] =
            json!([{"namespace":"opaque@1","key":"different","witness_digest":"a".repeat(64)}]);
        refused(w.apply_command(&changed).output().unwrap());
        let mut changed = p.clone();
        changed["digest"] = "0".repeat(64).into();
        refused(w.apply_command(&changed).output().unwrap());
        let mut changed = p.clone();
        let mut pieces = p["operation_id"]
            .as_str()
            .unwrap()
            .split(':')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        pieces[1] = Uuid::now_v7().to_string();
        changed["operation_id"] = pieces.join(":").into();
        refused(w.apply_command(&changed).output().unwrap());
        assert_eq!(w.revision(), current);
    }
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.count(PLANS), 1);
}

#[test]
fn source_change_after_preview_rejects_stale_new_intent_without_freezing_ingestion() {
    let w = World::new();
    let p = w.preview();
    let store = open_existing(&w.db).unwrap();
    std::fs::write(&w.note, "# Changed\n\nChanged knowledge before apply.\n").unwrap();
    ingest::ingest_path(&store, &w.note, &["p"]).unwrap();
    drop(store);
    let r = w.revision();
    let before = w.preserved();
    refused(w.apply_command(&p).output().unwrap());
    w.assert_empty(r);
    assert_eq!(w.preserved(), before);
}

#[test]
fn exact_committed_retry_precedes_new_ingestion_and_missing_live_file() {
    let w = World::new();
    let p = w.preview();
    let receipt = w.apply(&p);
    let store = open_existing(&w.db).unwrap();
    std::fs::write(&w.note, "# Changed\n\nLater knowledge.\n").unwrap();
    ingest::ingest_path(&store, &w.note, &["p"]).unwrap();
    document::ingest_document_file(&store, &w.note, &Default::default()).unwrap();
    drop(store);
    std::fs::remove_file(&w.note).unwrap();
    let r = w.revision();
    assert_eq!(w.apply(&p), receipt);
    assert_eq!(w.revision(), r);
    assert_eq!(w.inspect(&p)["freshness"], "Stale");
}

#[test]
fn six_exact_process_retries_share_one_receipt_but_new_passive_plans_may_coexist() {
    let w = World::new();
    let p = w.preview();
    let r = w.revision();
    let before = w.preserved();
    let children = (0..6)
        .map(|_| w.apply_command(&p).spawn().unwrap())
        .collect::<Vec<_>>();
    let receipts = children
        .into_iter()
        .map(|c| output_json(c.wait_with_output().unwrap()))
        .collect::<Vec<_>>();
    assert!(receipts.iter().all(|x| x == &receipts[0]));
    assert_eq!(w.count(PLANS), 1);
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.preserved(), before);
    w.apply(&w.preview());
    assert_eq!(w.count(PLANS), 2);
    assert_eq!(w.revision(), r + 2);
}

#[test]
fn distinct_concurrent_intents_serialize_without_silent_rebase() {
    let w = World::new();
    let plans = (0..6).map(|_| w.preview()).collect::<Vec<_>>();
    let r = w.revision();
    let children = plans
        .iter()
        .map(|p| w.apply_command(p).spawn().unwrap())
        .collect::<Vec<_>>();
    let outputs = children
        .into_iter()
        .map(|c| c.wait_with_output().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(outputs.iter().filter(|o| o.status.success()).count(), 1);
    assert_eq!(w.count(PLANS), 1);
    assert_eq!(w.revision(), r + 1);
}

struct Paused {
    child: Option<Child>,
    prefix: PathBuf,
}
impl Paused {
    fn start(w: &World, p: &Value, phase: &str) -> Self {
        let prefix = w.dir.path().join(format!("fence-{phase}"));
        let child = w
            .apply_command(p)
            .env("MINDSPACE_FENCE_PLAN_PAUSE", phase)
            .env("MINDSPACE_FENCE_PLAN_GATE_PATH", &prefix)
            .spawn()
            .unwrap();
        let mut out = Self {
            child: Some(child),
            prefix,
        };
        let deadline = Instant::now() + Duration::from_secs(12);
        while !out.marker("reached").exists() {
            if let Some(status) = out.child.as_mut().unwrap().try_wait().unwrap() {
                let result = out.child.take().unwrap().wait_with_output().unwrap();
                panic!(
                    "child exited before {phase}: {status}: {}",
                    String::from_utf8_lossy(&result.stderr)
                );
            }
            assert!(Instant::now() < deadline, "missing {phase} gate");
            std::thread::sleep(Duration::from_millis(5));
        }
        out
    }
    fn marker(&self, ext: &str) -> PathBuf {
        let mut s = self.prefix.as_os_str().to_owned();
        s.push(format!(".{ext}"));
        s.into()
    }
    fn release(mut self) -> Output {
        std::fs::write(self.marker("go"), b"").unwrap();
        self.child.take().unwrap().wait_with_output().unwrap()
    }
    fn kill(mut self) -> Output {
        let mut c = self.child.take().unwrap();
        c.kill().unwrap();
        c.wait_with_output().unwrap()
    }
}
impl Drop for Paused {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[test]
fn delayed_before_begin_retry_returns_the_winners_original_receipt() {
    let w = World::new();
    let p = w.preview();
    let r = w.revision();
    let old = Paused::start(&w, &p, "before_begin");
    let receipt = w.apply(&p);
    assert_eq!(output_json(old.release()), receipt);
    assert_eq!(w.revision(), r + 1);
}

#[test]
fn interrupted_transactions_leave_no_passive_fragments_and_restart_retry_recovers() {
    for phase in ["before_begin", "after_begin", "after_write", "in_commit"] {
        let w = World::new();
        let p = w.preview();
        let r = w.revision();
        let before = w.preserved();
        Paused::start(&w, &p, phase).kill();
        w.assert_empty(r);
        assert_eq!(w.preserved(), before);
        w.apply(&p);
        assert_eq!(w.revision(), r + 1);
    }
}

#[test]
fn process_death_after_commit_recovers_durable_receipt_without_another_revision() {
    let w = World::new();
    let p = w.preview();
    let r = w.revision();
    let writer = Paused::start(&w, &p, "after_commit");
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.revision(), r + 1);
    assert!(!writer.kill().status.success());
    assert_eq!(w.apply(&p)["committed_revision"], r + 1);
    assert_eq!(w.revision(), r + 1);
}

#[test]
fn veto_is_known_noncommit_while_lost_ack_keeps_one_successful_receipt() {
    let w = World::new();
    let p = w.preview();
    let r = w.revision();
    let before = w.preserved();
    let error = refused(
        w.apply_command(&p)
            .env("MINDSPACE_FENCE_PLAN_FAULT", "veto")
            .output()
            .unwrap(),
    );
    assert!(!error.to_lowercase().contains("unknown"), "{error}");
    w.assert_empty(r);
    assert_eq!(w.preserved(), before);
    let error = refused(
        w.apply_command(&p)
            .env("MINDSPACE_FENCE_PLAN_FAULT", "lost_ack")
            .output()
            .unwrap(),
    );
    assert!(error.to_lowercase().contains("unknown"), "{error}");
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.apply(&p)["committed_revision"], r + 1);
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.preserved(), before);
}

#[test]
fn a_live_paused_writer_timeout_is_not_evidence_of_rollback() {
    let w = World::new();
    let p = w.preview();
    let writer = Paused::start(&w, &p, "after_write");
    let error = refused(w.apply_command(&p).output().unwrap()).to_lowercase();
    assert!(
        error.contains("busy") || error.contains("locked") || error.contains("unknown"),
        "{error}"
    );
    assert!(!error.contains("rolled back"));
    let receipt = output_json(writer.release());
    assert_eq!(w.apply(&p), receipt);
    assert_eq!(w.count(RECEIPTS), 1);
}

#[test]
fn ordinary_metadata_and_direct_sql_cannot_mutate_receipts_or_activate_fences() {
    let w = World::new();
    let p = w.preview_bindings(
        json!([{"namespace":"opaque@1","key":"advisory","witness_digest":"a".repeat(64)}]),
    );
    w.apply(&p);
    let store = open_existing(&w.db).unwrap();
    store.set_meta("fence.active", "document").unwrap();
    store.set_meta("fence.authorized", "true").unwrap();
    drop(store);
    let r = w.revision();
    let before = w.preserved();
    for recursive in [false, true] {
        let c = w.raw();
        c.execute_batch(&format!(
            "PRAGMA foreign_keys=OFF; PRAGMA recursive_triggers={};",
            if recursive { "ON" } else { "OFF" }
        ))
        .unwrap();
        for table in [PLANS, BINDINGS, RECEIPTS] {
            for sql in [format!("UPDATE {table} SET plan_id=plan_id"),format!("DELETE FROM {table}"),format!("INSERT OR REPLACE INTO {table} SELECT * FROM {table}"),format!("INSERT INTO {table} SELECT * FROM {table} WHERE true ON CONFLICT DO UPDATE SET plan_id=plan_id")] {
                assert!(c.execute_batch(&sql).is_err(),"accepted bypass: {sql}");
            }
        }
        for sql in [
            "UPDATE asset_fence_plans SET state='active'",
            "UPDATE asset_fence_plans SET owner='document'",
            "UPDATE asset_fence_plans SET publication_state='visible'",
            "UPDATE asset_ownership SET owner='document'",
        ] {
            assert!(c.execute_batch(sql).is_err(), "accepted transition: {sql}");
        }
    }
    assert_eq!(w.revision(), r);
    assert_eq!(w.preserved(), before);
    assert_eq!(w.count(PLANS), 1);
    assert_eq!(w.count(BINDINGS), 1);
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.raw().query_row("SELECT COUNT(*) FROM memories m JOIN assertions a ON a.id=m.assertion_id JOIN evidence e ON e.id=a.evidence_id JOIN sources s ON s.id=e.source_id WHERE s.kind='document'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
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
        assert!(send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"untrusted-fence-client","version":"1"}}}))["result"].is_object());
        send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        let tools = send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
        for (i, name) in [
            "prepare_fence_plan",
            "ownership_prepare_fence_plan",
            "activate_writer_fence",
            "document_publish",
        ]
        .into_iter()
        .enumerate()
        {
            let response = send(
                json!({"jsonrpc":"2.0","id":i+3,"method":"tools/call","params":{"name":name,"arguments":{"owner":"document","apply":true}}}),
            );
            assert!(response["error"].is_object());
        }
        tools
    };
    drop(input);
    assert!(child.wait().unwrap().success());
    result
}
#[test]
fn default_mcp_definitions_and_exposure_are_unchanged() {
    let w = World::new();
    let before = mcp_probe(&w.db);
    let names = before["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 15);
    assert!(names
        .iter()
        .all(|n| !n.contains("fence") && !n.contains("publish")));
    w.apply(&w.preview());
    assert_eq!(mcp_probe(&w.db), before);
}

#[cfg(feature = "embeddings")]
mod retrieval_isolation {
    use super::*;
    use mindspace::core::{AssertionEmbedding, Ts};
    use mindspace::embeddings::manifest::{derivation_from_manifest, Artifact, ModelManifestEntry};
    use mindspace::embeddings::EmbeddingRuntime;
    use mindspace::retrieval::{search_with_mode, RetrievalOptions, SearchMode};

    const ID: &str = "test-embed-model";
    const MODEL: &[u8] = b"synthetic model artifact for retrieval tests\n";
    const TOKENIZER: &[u8] = b"{\"synthetic\":true}\n";
    static ENTRY: ModelManifestEntry = ModelManifestEntry {
        id: ID,
        version: "test-1",
        revision: "test",
        architecture: "synthetic",
        runtime: "fake",
        runtime_version: "0",
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
            Artifact {
                filename: "model.safetensors",
                url: "https://example.invalid/model.safetensors",
                sha256: "269cbb1a5ed49786cc52cfa6e29a80d5147d461ca878f9c84651c947bf5f750f",
                size: MODEL.len() as u64,
            },
            Artifact {
                filename: "tokenizer.json",
                url: "https://example.invalid/tokenizer.json",
                sha256: "cb8daed7b30399a1c3c8b83b3b7bee4b774a30fc389afc1d375e4c23a3cc4ae8",
                size: TOKENIZER.len() as u64,
            },
        ],
    };
    struct Runtime;
    impl EmbeddingRuntime for Runtime {
        fn embed_document(&self, _: &str) -> mindspace::Result<Vec<f32>> {
            panic!("passive planning and queries must not embed documents")
        }
        fn embed_query(&self, _: &str) -> mindspace::Result<Vec<f32>> {
            Ok(vec![1., 0., 0., 0.])
        }
        fn dimension(&self) -> usize {
            4
        }
        fn model_id(&self) -> &str {
            ID
        }
    }
    fn lookup(id: &str) -> Option<&'static ModelManifestEntry> {
        (id == ID).then_some(&ENTRY)
    }
    fn capability(w: &World) -> mindspace::retrieval::SemanticCapability {
        let store = open_existing(&w.db).unwrap();
        let other = w.dir.path().join("rank-competitor.md");
        std::fs::write(
            &other,
            "# Competitor\n\nKnowledge knowledge with a different phrase.\n",
        )
        .unwrap();
        ingest::ingest_path(&store, &other, &["p"]).unwrap();
        let ids = w
            .raw()
            .prepare("SELECT id FROM assertions ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        let key = derivation_from_manifest(&ENTRY).derivation_key();
        for (i, id) in ids.iter().enumerate() {
            store
                .save_embeddings(&[AssertionEmbedding {
                    assertion_id: Uuid::parse_str(id).unwrap(),
                    derivation_key: key.clone(),
                    model: ID.into(),
                    model_version: ENTRY.version.into(),
                    dimension: 4,
                    embedding: if i % 2 == 0 {
                        vec![1., 0., 0., 0.]
                    } else {
                        vec![0.8, 0.6, 0., 0.]
                    },
                }])
                .unwrap();
        }
        mindspace::embeddings::manager::set_enabled(&store, true).unwrap();
        store.set_meta("embedding.model", ID).unwrap();
        let base = w.dir.path().join("fake-data");
        let dir = mindspace::embeddings::manager::model_dir_in(&base, ID);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.safetensors"), MODEL).unwrap();
        std::fs::write(dir.join("tokenizer.json"), TOKENIZER).unwrap();
        mindspace::retrieval::semantic::resolve_with(&store, &base, lookup, &|_, _| {
            Ok(Box::new(Runtime))
        })
        .unwrap()
    }
    fn results(w: &World, cap: &mindspace::retrieval::SemanticCapability, cutoff: Ts) -> Value {
        let store = open_existing(&w.db).unwrap();
        let mut out = Vec::new();
        for historical in [false, true] {
            for mode in [
                SearchMode::Lexical,
                SearchMode::Semantic,
                SearchMode::Hybrid,
                SearchMode::Auto,
            ] {
                let q = SearchQuery {
                    text: "knowledge".into(),
                    space_ids: None,
                    limit: 50,
                    at: historical.then_some(cutoff),
                    include_retired: false,
                    include_superseded: false,
                };
                let result =
                    search_with_mode(&store, q, mode, cap, RetrievalOptions::default()).unwrap();
                assert!(
                    result.hits.len() >= 2,
                    "{mode:?} needs nonempty competing hits for ranking evidence"
                );
                out.push(serde_json::to_value(result).unwrap());
            }
        }
        json!(out)
    }
    #[test]
    fn lexical_semantic_hybrid_auto_timestamp_results_scores_and_all_active_rows_are_identical() {
        let w = World::new();
        let cap = capability(&w);
        let cutoff = Ts::now();
        let before = results(&w, &cap, cutoff);
        let rows = w.preserved();
        let p = w.preview();
        w.apply(&p);
        assert_eq!(results(&w, &cap, cutoff), before);
        assert_eq!(w.preserved(), rows);
        let store = open_existing(&w.db).unwrap();
        assert_eq!(
            store
                .list_assertions_needing_embedding(
                    &derivation_from_manifest(&ENTRY).derivation_key(),
                    None,
                    100
                )
                .unwrap()
                .len(),
            0
        );
    }
}

#[test]
#[ignore = "local measurements; run release with --ignored --nocapture, no inference"]
fn measure_actual_passive_plan_operations_and_ordinary_ingest() {
    let w = World::new();
    let other = w.dir.path().join("timing.md");
    std::fs::write(
        &other,
        "# Timing\n\nA small unrelated knowledge timing input.\n",
    )
    .unwrap();
    let ingest_samples = || {
        let store = open_existing(&w.db).unwrap();
        (0..12)
            .map(|_| {
                let t = Instant::now();
                ingest::ingest_path(&store, &other, &["p"]).unwrap();
                t.elapsed().as_secs_f64() * 1000.
            })
            .collect::<Vec<_>>()
    };
    let ordinary_before = ingest_samples();
    let bytes = || {
        let c = w.raw();
        let pages: i64 = c.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
        let size: i64 = c.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
        pages * size
    };
    let size_before = bytes();
    let mut previews = Vec::new();
    let mut prepares = Vec::new();
    let mut inspections = Vec::new();
    for _ in 0..20 {
        let t = Instant::now();
        let p=w.preview_bindings(json!([{"namespace":"opaque@1","key":"timing-candidate","witness_digest":"a".repeat(64)}]));
        previews.push(t.elapsed().as_secs_f64() * 1000.);
        let t = Instant::now();
        w.apply(&p);
        prepares.push(t.elapsed().as_secs_f64() * 1000.);
        let t = Instant::now();
        w.inspect(&p);
        inspections.push(t.elapsed().as_secs_f64() * 1000.);
    }
    let size_after = bytes();
    let ordinary_after = ingest_samples();
    assert_eq!(w.count(PLANS), 20);
    assert_eq!(w.count(BINDINGS), 20);
    assert_eq!(w.count(RECEIPTS), 20);
    println!(
        "{}",
        json!({"measurement":"actual passive CLI operations; startup/admin opening included; no inference", "input_bytes":TEXT.len(),"sample_count":20,"preview_ms":previews,"prepare_ms":prepares,"inspect_ms":inspections,"db_page_bytes_before":size_before,"db_page_bytes_after":size_after,"ordinary_store_ingest_samples":12,"ordinary_ingest_before_ms":ordinary_before,"ordinary_ingest_after_20_plans_ms":ordinary_after,"limits":"single local run, unchanged-file ordinary ingest includes observation transaction; not a fence or contention bound"})
    );
}

// Construct a populated old-schema fixture only by removing empty *test* metadata.
// Actual old-binary behavior is exercised separately in fence_plan_compatibility.rs.
#[allow(dead_code)]
#[path = "support/schema_fixture.rs"]
mod schema_fixture;

type DatabaseFiles = BTreeMap<String, (u64, String, std::time::SystemTime)>;
fn database_files(db: &Path) -> DatabaseFiles {
    ["", "-wal", "-shm", "-journal"]
        .into_iter()
        .filter_map(|suffix| {
            let mut path = db.as_os_str().to_owned();
            path.push(suffix);
            let path = PathBuf::from(path);
            match std::fs::metadata(&path) {
                Ok(metadata) => {
                    assert!(metadata.is_file());
                    let bytes = std::fs::read(&path).unwrap();
                    Some((
                        suffix.to_owned(),
                        (
                            metadata.len(),
                            mindspace::core::sha256_hex(&bytes),
                            metadata.modified().unwrap(),
                        ),
                    ))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => panic!("cannot snapshot {}: {error}", path.display()),
            }
        })
        .collect()
}
fn checkpoint(db: &Path) {
    let raw = Connection::open(db).unwrap();
    let result: (i64, i64, i64) = raw
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(result.0, 0, "fixture must not leave a busy checkpoint");
}
fn read_command(w: &World, db: &Path, inspect: bool) -> Command {
    let mut c = clean_command();
    c.arg("--db").arg(db);
    if inspect {
        c.args([
            "ownership",
            "inspect-fence-plan",
            "--operation-id",
            "local-fence-plan@1:00000000-0000-0000-0000-000000000000:0000000000000000000000000000000000000000000000000000000000000000",
            "--json",
        ]);
    } else {
        let args = w
            .preview_command(&w.target, 1, Uuid::now_v7(), &json!([]))
            .get_args()
            .skip(2)
            .map(|x| x.to_owned())
            .collect::<Vec<_>>();
        c.args(args);
    }
    c
}
fn rejected_read_leaves_database_files(w: &World, db: &Path, inspect: bool) {
    let before = database_files(db);
    refused(read_command(w, db, inspect).output().unwrap());
    // Compare filesystem evidence *before* any further SQLite open can remove or
    // checkpoint the sidecars that the command might have created or changed.
    assert_eq!(
        database_files(db),
        before,
        "read refusal changed database files"
    );
}
fn schema16_read_refusal(inspect: bool) {
    let w = World::new();
    {
        let raw = w.raw();
        schema_fixture::remove_empty_fence_plan_metadata(&raw);
    }
    checkpoint(&w.db);
    let before = database_files(&w.db);
    assert!(!before.contains_key("-wal"));
    assert!(!before.contains_key("-shm"));
    // This preview selects real F/D/P pins; rejected read opening must not run
    // migration 17 merely to discover that the supplied expected R is stale.
    rejected_read_leaves_database_files(&w, &w.db, inspect);
    let raw =
        Connection::open_with_flags(&w.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        raw.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        16
    );
}

#[test]
fn rejected_preview_does_not_migrate_schema16_or_create_sidecars() {
    schema16_read_refusal(false);
}
#[test]
fn rejected_inspection_does_not_coordinate_schema16_or_create_sidecars() {
    schema16_read_refusal(true);
}
fn incompatible_read_targets(inspect: bool) {
    let w = World::new();
    let missing = w.dir.path().join("read-missing.db");
    rejected_read_leaves_database_files(&w, &missing, inspect);
    for (name, bytes) in [
        ("read-empty.db", Vec::new()),
        ("read-invalid.db", b"not SQLite".to_vec()),
    ] {
        let db = w.dir.path().join(name);
        std::fs::write(&db, bytes).unwrap();
        rejected_read_leaves_database_files(&w, &db, inspect);
    }
    let foreign = w.dir.path().join("read-foreign-delete.db");
    {
        let c = Connection::open(&foreign).unwrap();
        c.execute_batch("PRAGMA journal_mode=DELETE; CREATE TABLE foreign_data(value TEXT); INSERT INTO foreign_data VALUES('must remain unchanged')").unwrap();
    }
    rejected_read_leaves_database_files(&w, &foreign, inspect);
    let foreign = w.dir.path().join("read-foreign-wal.db");
    let holder = Connection::open(&foreign).unwrap();
    holder.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE foreign_data(value TEXT); INSERT INTO foreign_data VALUES('preserve WAL and SHM')").unwrap();
    // Leave a fresh committed WAL frame not represented by an existing read mark.
    holder
        .execute("INSERT INTO foreign_data VALUES('newer frame')", [])
        .unwrap();
    let before = database_files(&foreign);
    assert!(before.contains_key("-wal"));
    assert!(before.contains_key("-shm"));
    rejected_read_leaves_database_files(&w, &foreign, inspect);
    drop(holder);
    checkpoint(&w.db);
    let rollback = w.dir.path().join("read-rollback.db");
    std::fs::copy(&w.db, &rollback).unwrap();
    let mut journal = rollback.as_os_str().to_owned();
    journal.push("-journal");
    std::fs::write(
        PathBuf::from(journal),
        b"unreviewed rollback-journal evidence",
    )
    .unwrap();
    rejected_read_leaves_database_files(&w, &rollback, inspect);
}
#[test]
fn preview_refusals_preserve_missing_empty_invalid_foreign_and_journal_files() {
    incompatible_read_targets(false);
}
#[test]
fn inspection_refusals_preserve_missing_empty_invalid_foreign_and_journal_files() {
    incompatible_read_targets(true);
}

#[test]
fn preview_and_inspection_refuse_wal_without_shm_without_creating_it() {
    let w = World::new();
    checkpoint(&w.db);
    for inspect in [false, true] {
        let db = w.dir.path().join(format!("read-no-shm-{inspect}.db"));
        std::fs::copy(&w.db, &db).unwrap();
        let mut wal = db.as_os_str().to_owned();
        wal.push("-wal");
        std::fs::write(PathBuf::from(wal), b"unreviewed WAL requires recovery").unwrap();
        assert!(!database_files(&db).contains_key("-shm"));
        rejected_read_leaves_database_files(&w, &db, inspect);
    }
}

#[test]
fn admitted_checkpointed_reads_do_not_write_main_database_or_recording_revision() {
    let w = World::new();
    let p = w.preview();
    w.apply(&p);
    let revision = w.revision();
    checkpoint(&w.db);
    let before = database_files(&w.db)[""].clone();
    // A compatible native RO connection may coordinate existing SHM. Main bytes,
    // main mtime and R must remain unchanged; it cannot migrate or create a plan.
    let fresh = w.preview();
    assert_eq!(w.inspect(&p)["freshness"], "Fresh");
    assert_eq!(database_files(&w.db)[""], before);
    assert_eq!(w.revision(), revision);
    assert_eq!(w.count(PLANS), 1);
    assert_eq!(fresh["expected_revision"], revision);
}

#[test]
fn corrupted_immutable_payloads_are_rejected_after_exact_guards_are_restored() {
    for (table, column, value) in [
        (
            RECEIPTS,
            "canonical_request",
            "zeroblob(length(canonical_request))",
        ),
        (
            PLANS,
            "snapshot_digest",
            "'0000000000000000000000000000000000000000000000000000000000000000'",
        ),
        (PLANS, "binding_count", "127"),
    ] {
        let w = World::new();
        let p = w.preview();
        w.apply(&p);
        let r = w.revision();
        let guard = if table == RECEIPTS {
            "m6_s3e2_receipt_update"
        } else {
            "m6_s3e2_plan_update"
        };
        {
            let raw = w.raw();
            let definition: String = raw
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE name=?1",
                    [guard],
                    |row| row.get(0),
                )
                .unwrap();
            // Deliberate corruption fixture, not an ordinary authorization path:
            // temporarily remove precisely one guard, corrupt one stored value,
            // then restore its original SQL so schema validation still passes.
            raw.execute_batch(&format!(
                "DROP TRIGGER {guard}; UPDATE {table} SET {column}={value}; {definition};"
            ))
            .unwrap();
        }
        checkpoint(&w.db);
        let main = database_files(&w.db)[""].clone();
        let error = refused(
            w.command()
                .args([
                    "ownership",
                    "inspect-fence-plan",
                    "--operation-id",
                    p["operation_id"].as_str().unwrap(),
                    "--json",
                ])
                .output()
                .unwrap(),
        );
        assert!(
            error.contains("corrupt") || error.contains("binding"),
            "{error}"
        );
        assert_eq!(database_files(&w.db)[""], main);
        let error = refused(w.apply_command(&p).output().unwrap());
        assert!(
            error.contains("corrupt") || error.contains("binding"),
            "{error}"
        );
        assert_eq!(w.revision(), r);
    }
}

#[test]
fn same_name_noop_guard_and_changed_table_shape_fail_main_admission() {
    for modified_guard in [false, true] {
        let w = World::new();
        let p = w.preview();
        w.apply(&p);
        {
            let raw = w.raw();
            if modified_guard {
                raw.execute_batch("DROP TRIGGER m6_s3e2_plan_update; CREATE TRIGGER m6_s3e2_plan_update BEFORE UPDATE ON asset_fence_plans BEGIN SELECT 1; END;").unwrap();
            } else {
                raw.execute_batch(
                    "ALTER TABLE asset_fence_plan_bindings ADD COLUMN unreviewed TEXT",
                )
                .unwrap();
            }
        }
        checkpoint(&w.db);
        for inspect in [false, true] {
            rejected_read_leaves_database_files(&w, &w.db, inspect);
        }
    }
}
