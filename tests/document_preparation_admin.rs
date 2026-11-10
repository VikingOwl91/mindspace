//! Preparation-only local administration, real processes and durable recovery.
//! Model A trusts the operating account; these tests do not authenticate a human.
#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use mindspace::core::{AnchorPrecision, AssertionEmbedding, Uuid};
use mindspace::ingest::{self, document};
use mindspace::storage::Store;
use mindspace::{initialize, open_existing};
use rusqlite::{functions::FunctionFlags, Connection};
use serde_json::{json, Value};

const TEXT: &str = "# Enrollment\n\nPreparation preserves searchable knowledge.\n";
const PREPARATIONS: &str = "document_preparation_enrollments";
const RECEIPTS: &str = "document_preparation_receipts";

struct World {
    dir: tempfile::TempDir,
    db: PathBuf,
    note: PathBuf,
    source: Uuid,
    target: Value,
    report: document::DocumentIngestReport,
}

impl World {
    fn new() -> Self {
        Self::with_text(TEXT, false)
    }

    fn with_text(text: &str, documents_first: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mind.db");
        let note = dir.path().join("legacy.md");
        std::fs::write(&note, text).unwrap();
        initialize(&db).unwrap();
        let store = open_existing(&db).unwrap();
        store.create_space("p", "").unwrap();
        let early = documents_first
            .then(|| document::ingest_document_file(&store, &note, &Default::default()).unwrap());
        ingest::ingest_path(&store, &note, &["p"]).unwrap();
        let raw = Connection::open(&db).unwrap();
        let (source, fs): (String, String) = raw.query_row(
            "SELECT l.id,s.id FROM logical_sources l JOIN sources s ON s.logical_id=l.id WHERE l.kind='file'", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
        let assertion: String = raw
            .query_row("SELECT id FROM assertions ORDER BY id LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        store
            .save_embeddings(&[AssertionEmbedding {
                assertion_id: Uuid::parse_str(&assertion).unwrap(),
                derivation_key: "s3c-fixture@1".into(),
                model: "fixture".into(),
                model_version: "1".into(),
                dimension: 2,
                embedding: vec![1.0, 0.0],
            }])
            .unwrap();
        let report = early.unwrap_or_else(|| {
            document::ingest_document_file(&store, &note, &Default::default()).unwrap()
        });
        drop(store);
        drop(raw);
        let source = Uuid::parse_str(&source).unwrap();
        let revision: i64 = Connection::open(&db)
            .unwrap()
            .query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| {
                r.get(0)
            })
            .unwrap();
        let legacy = output_json(
            clean_command()
                .arg("--db")
                .arg(&db)
                .args([
                    "ownership",
                    "enroll-legacy",
                    "--source-id",
                    &source.to_string(),
                    "--expected-revision",
                    &revision.to_string(),
                    "--nonce",
                    &Uuid::now_v7().to_string(),
                    "--json",
                ])
                .output()
                .unwrap(),
        );
        let enrolled = output_json(
            clean_command()
                .arg("--db")
                .arg(&db)
                .args([
                    "ownership",
                    "enroll-legacy",
                    "--source-id",
                    &source.to_string(),
                    "--expected-revision",
                    &revision.to_string(),
                    "--operation-id",
                    legacy["operation_id"].as_str().unwrap(),
                    "--apply",
                    "--confirm-intent",
                    legacy["digest"].as_str().unwrap(),
                    "--json",
                ])
                .output()
                .unwrap(),
        );
        let target = json!({"asset_id":enrolled["asset_id"],"legacy_source_version":fs,
            "document_id":report.persisted.logical_id.to_string(),"document_source_version":report.persisted.source_id.to_string(),"extraction_id":report.persisted.extraction_id.to_string()});
        Self {
            dir,
            db,
            note,
            source,
            target,
            report,
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
    fn target_command(&self, target: &Value, revision: i64) -> Command {
        let mut c = self.command();
        c.args(["ownership", "enroll-document-preparation"]);
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
    fn preview_command(&self, target: &Value, revision: i64, nonce: Uuid) -> Command {
        let mut c = self.target_command(target, revision);
        c.arg("--nonce").arg(nonce.to_string());
        c
    }
    fn preview(&self) -> Value {
        let mut p = output_json(
            self.preview_command(&self.target, self.revision(), Uuid::now_v7())
                .output()
                .unwrap(),
        );
        p["test_target"] = self.target.clone();
        p["test_revision"] = self.revision().into();
        p
    }
    fn apply_command(&self, p: &Value) -> Command {
        let mut c = self.target_command(&p["test_target"], p["test_revision"].as_i64().unwrap());
        c.arg("--operation-id")
            .arg(p["operation_id"].as_str().unwrap())
            .args(["--apply", "--confirm-intent"])
            .arg(p["digest"].as_str().unwrap());
        c
    }
    fn apply(&self, p: &Value) -> Value {
        output_json(self.apply_command(p).output().unwrap())
    }
    fn search(&self) -> Value {
        output_json(
            self.command()
                .args(["search", "knowledge", "--json"])
                .output()
                .unwrap(),
        )
    }
    fn preserved(&self) -> BTreeMap<String, Vec<String>> {
        let raw = self.raw();
        let tables=raw.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE 'document_preparation_%' AND name NOT IN ('m6_revisions','m6_row_availability') ORDER BY name").unwrap().query_map([],|r|r.get::<_,String>(0)).unwrap().map(Result::unwrap).collect::<Vec<_>>();
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
                .map(|x| format!("quote(\"{x}\")"))
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
    fn assert_unprepared(&self, revision: i64) {
        assert_eq!(self.count(PREPARATIONS), 0);
        assert_eq!(self.count(RECEIPTS), 0);
        assert_eq!(self.revision(), revision);
    }
}

fn clean_command() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_mind"));
    for key in [
        "MINDSPACE_DB",
        "MINDSPACE_OWNERSHIP_PAUSE",
        "MINDSPACE_OWNERSHIP_GATE_PATH",
        "MINDSPACE_OWNERSHIP_FAULT",
        "MINDSPACE_PREPARATION_PAUSE",
        "MINDSPACE_PREPARATION_GATE_PATH",
        "MINDSPACE_PREPARATION_FAULT",
    ] {
        c.env_remove(key);
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
fn explicit_preparation_changes_only_new_metadata_and_exact_retry_spends_no_revision() {
    for first in [false, true] {
        let w = World::with_text(TEXT, first);
        let before = w.preserved();
        let search = w.search();
        let r = w.revision();
        let plan = w.preview();
        assert!(plan["operation_id"]
            .as_str()
            .unwrap()
            .starts_with("local-doc-prepare@1:"));
        w.assert_unprepared(r);
        let receipt = w.apply(&plan);
        assert_eq!(receipt["operation_id"], plan["operation_id"]);
        assert_eq!(w.revision(), r + 1);
        assert_eq!(w.count(PREPARATIONS), 1);
        assert_eq!(w.count(RECEIPTS), 1);
        assert_eq!(w.apply(&plan), receipt);
        assert_eq!(w.revision(), r + 1);
        assert_eq!(w.preserved(), before);
        assert_eq!(w.search(), search);
        assert_eq!(
            w.raw()
                .query_row("SELECT owner||':'||epoch FROM asset_ownership", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
            "legacy:1"
        );
        assert!(!w
            .raw()
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
    }
}

#[test]
fn bom_unicode_original_and_canonical_versions_are_distinct_and_citations_stay_exact() {
    let text="\u{feff}# Enrollment\r\n\r\nPreparation knowledge 中文 😀 e\u{301} \u{200b}\u{202e} test.\r\n";
    let w = World::with_text(text, false);
    assert_ne!(w.report.source_bytes_hash, w.report.canonical_hash);
    let plan = w.preview();
    w.apply(&plan);
    let store = open_existing(&w.db).unwrap();
    let canonical = text.trim_start_matches('\u{feff}');
    let anchor = w
        .report
        .anchor(
            &[],
            Some((0, canonical.len() as u64)),
            AnchorPrecision::ByteExact,
        )
        .unwrap();
    let citation = store.resolve_document_anchor(&anchor).unwrap().unwrap();
    assert_eq!(citation.text.as_deref(), Some(canonical));
    assert_eq!(
        w.count("doc_chunk_sets"),
        0,
        "metadata enrollment does not need or create chunks"
    );
}

#[test]
fn committed_replay_precedes_new_versions_and_missing_live_source() {
    let w = World::new();
    let plan = w.preview();
    let original = w.apply(&plan);
    let store = open_existing(&w.db).unwrap();
    std::fs::write(&w.note, "# Changed\n\nLater source knowledge.\n").unwrap();
    ingest::ingest_path(&store, &w.note, &["p"]).unwrap();
    document::ingest_document_file(&store, &w.note, &Default::default()).unwrap();
    std::fs::write(&w.note, TEXT).unwrap();
    document::ingest_document_file(&store, &w.note, &Default::default()).unwrap();
    drop(store);
    std::fs::remove_file(&w.note).unwrap();
    let r = w.revision();
    assert_eq!(w.apply(&plan), original);
    assert_eq!(w.revision(), r);
}

#[test]
fn named_historical_snapshot_can_enroll_without_fabricating_a_current_head() {
    let w = World::new();
    let store = open_existing(&w.db).unwrap();
    std::fs::write(&w.note, "# Changed\n\nLater knowledge.\n").unwrap();
    ingest::ingest_path(&store, &w.note, &["p"]).unwrap();
    document::ingest_document_file(&store, &w.note, &Default::default()).unwrap();
    drop(store);
    // Selected initial Fs/Ds/X remain valid stored evidence; no API says they are current.
    w.apply(&w.preview());
    assert_eq!(w.count(PREPARATIONS), 1);
}

#[test]
fn missing_or_wrong_source_document_and_extraction_never_prepare() {
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
        refused(w.preview_command(&t, r, Uuid::now_v7()).output().unwrap());
        w.assert_unprepared(r);
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
        refused(w.preview_command(&t, r, Uuid::now_v7()).output().unwrap());
        w.assert_unprepared(r);
    }
}

#[test]
fn incompatible_revision_and_original_byte_hash_are_refused() {
    let w = World::new();
    let store = open_existing(&w.db).unwrap();
    std::fs::write(&w.note, "# Different\n\nDifferent knowledge bytes.\n").unwrap();
    let newer = document::ingest_document_file(&store, &w.note, &Default::default()).unwrap();
    drop(store);
    let r = w.revision();
    for change_both in [false, true] {
        let mut t = w.target.clone();
        t["extraction_id"] = newer.persisted.extraction_id.to_string().into();
        if change_both {
            t["document_source_version"] = newer.persisted.source_id.to_string().into();
        }
        refused(w.preview_command(&t, r, Uuid::now_v7()).output().unwrap());
        w.assert_unprepared(r);
    }
}

#[test]
fn ordinary_metadata_and_argument_claims_cannot_authorize_preparation() {
    let w = World::new();
    let store = open_existing(&w.db).unwrap();
    for key in [
        "ownership.owner",
        "ownership.authorized",
        "preparation.authorized",
        "preparation.operation",
        "preparation.principal",
    ] {
        store.set_meta(key, "document:approved").unwrap();
    }
    drop(store);
    let r = w.revision();
    for arg in [
        "--principal",
        "--actor-uid",
        "--owner",
        "--publish",
        "--chunker-version",
        "--yes",
    ] {
        refused(
            w.preview_command(&w.target, r, Uuid::now_v7())
                .arg(arg)
                .arg("document")
                .output()
                .unwrap(),
        );
        w.assert_unprepared(r);
    }
    let mut c = w.target_command(&w.target, r);
    c.arg("--apply");
    refused(c.output().unwrap());
    w.assert_unprepared(r);
}

#[test]
fn missing_empty_foreign_and_symlink_targets_are_not_initialized_or_rebound() {
    let w = World::new();
    let args = w
        .preview_command(&w.target, w.revision(), Uuid::now_v7())
        .get_args()
        .skip(2)
        .map(|a| a.to_owned())
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
        ("foreign.db", b"not a Mindspace database".to_vec()),
    ] {
        let path = w.dir.path().join(name);
        std::fs::write(&path, &bytes).unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        refused(
            clean_command()
                .arg("--db")
                .arg(&path)
                .args(&args)
                .output()
                .unwrap(),
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);
    }
    let sym = w.dir.path().join("alias.db");
    symlink(&w.db, &sym).unwrap();
    refused(
        clean_command()
            .arg("--db")
            .arg(&sym)
            .args(&args)
            .output()
            .unwrap(),
    );
    let hard = w.dir.path().join("hard.db");
    std::fs::hard_link(&w.db, &hard).unwrap();
    refused(
        w.preview_command(&w.target, w.revision(), Uuid::now_v7())
            .output()
            .unwrap(),
    );
    std::fs::remove_file(hard).unwrap();
    std::fs::set_permissions(&w.db, std::fs::Permissions::from_mode(0o666)).unwrap();
    refused(
        w.preview_command(&w.target, w.revision(), Uuid::now_v7())
            .output()
            .unwrap(),
    );
    std::fs::set_permissions(&w.db, std::fs::Permissions::from_mode(0o600)).unwrap();
    refused(
        clean_command()
            .env("MINDSPACE_DB", &w.db)
            .args(&args)
            .output()
            .unwrap(),
    );
}

#[test]
fn cloned_database_does_not_accept_the_original_physical_intent() {
    let w = World::new();
    let plan = w.preview();
    let other = World::new();
    std::fs::copy(&w.db, &other.db).unwrap();
    refused(other.apply_command(&plan).output().unwrap());
    w.assert_unprepared(w.revision());
    other.assert_unprepared(other.revision());
}

#[test]
fn changed_intent_confirmation_or_target_is_refused_before_and_after_commit() {
    let w = World::new();
    let plan = w.preview();
    let r = w.revision();
    for after_commit in [false, true] {
        if after_commit {
            w.apply(&plan);
        }
        let current = w.revision();
        for key in [
            "asset_id",
            "legacy_source_version",
            "document_id",
            "document_source_version",
            "extraction_id",
        ] {
            let mut p = plan.clone();
            p["test_target"][key] = Uuid::now_v7().to_string().into();
            refused(w.apply_command(&p).output().unwrap());
            assert_eq!(w.revision(), current);
        }
        let mut p = plan.clone();
        p["digest"] = "0".repeat(64).into();
        refused(w.apply_command(&p).output().unwrap());
        let mut p = plan.clone();
        p["test_revision"] = (r + 1).into();
        refused(w.apply_command(&p).output().unwrap());
        assert_eq!(w.revision(), current);
    }
}

#[test]
fn a_new_operation_for_an_enrolled_association_conflicts() {
    let w = World::new();
    let old = w.preview();
    let another = w.preview();
    w.apply(&old);
    let r = w.revision();
    refused(w.apply_command(&another).output().unwrap());
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.revision(), r);
    refused(
        w.preview_command(&w.target, r, Uuid::now_v7())
            .output()
            .unwrap(),
    );
}

#[test]
fn source_changes_between_preview_and_commit_reject_stale_intent() {
    let w = World::new();
    let plan = w.preview();
    let before = w.preserved();
    open_existing(&w.db)
        .unwrap()
        .create_space("later", "")
        .unwrap();
    let r = w.revision();
    refused(w.apply_command(&plan).output().unwrap());
    w.assert_unprepared(r);
    assert_eq!(w.count("asset_ownership"), 1);
    assert_ne!(w.preserved(), before);
    let plan = w.preview();
    std::fs::write(&w.note, "# Changed\n\nNew source knowledge.\n").unwrap();
    let store = open_existing(&w.db).unwrap();
    ingest::ingest_path(&store, &w.note, &["p"]).unwrap();
    document::ingest_document_file(&store, &w.note, &Default::default()).unwrap();
    drop(store);
    let r = w.revision();
    let before = w.preserved();
    refused(w.apply_command(&plan).output().unwrap());
    w.assert_unprepared(r);
    assert_eq!(w.preserved(), before);
}

#[test]
fn six_process_exact_retries_have_one_durable_preparation() {
    let w = World::new();
    let plan = w.preview();
    let r = w.revision();
    let before = w.preserved();
    let children = (0..6)
        .map(|_| w.apply_command(&plan).spawn().unwrap())
        .collect::<Vec<_>>();
    let receipts = children
        .into_iter()
        .map(|c| output_json(c.wait_with_output().unwrap()))
        .collect::<Vec<_>>();
    assert!(receipts.iter().all(|x| x == &receipts[0]));
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.preserved(), before);
}

#[test]
fn conflicting_six_process_preparations_have_one_winner() {
    let w = World::new();
    let plans = (0..6).map(|_| w.preview()).collect::<Vec<_>>();
    let r = w.revision();
    let children = plans
        .iter()
        .map(|p| w.apply_command(p).spawn().unwrap())
        .collect::<Vec<_>>();
    let outcomes = children
        .into_iter()
        .map(|c| c.wait_with_output().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(outcomes.iter().filter(|x| x.status.success()).count(), 1);
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.count(RECEIPTS), 1);
}

struct Paused {
    child: Option<Child>,
    prefix: PathBuf,
}
impl Paused {
    fn start(w: &World, p: &Value, phase: &str) -> Self {
        let prefix = w.dir.path().join(format!("pause-{phase}"));
        let child = w
            .apply_command(p)
            .env("MINDSPACE_PREPARATION_PAUSE", phase)
            .env("MINDSPACE_PREPARATION_GATE_PATH", &prefix)
            .spawn()
            .unwrap();
        let mut out = Self {
            child: Some(child),
            prefix,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !out.marker("reached").exists() {
            if let Some(status) = out.child.as_mut().unwrap().try_wait().unwrap() {
                let result = out.child.take().unwrap().wait_with_output().unwrap();
                panic!(
                    "child exited before {phase} gate ({status}): {}",
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
fn paused_prebegin_writer_returns_winners_original_receipt() {
    let w = World::new();
    let p = w.preview();
    let r = w.revision();
    let old = Paused::start(&w, &p, "before_begin");
    let receipt = w.apply(&p);
    assert_eq!(output_json(old.release()), receipt);
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.count(RECEIPTS), 1);
}

#[test]
fn paused_prebegin_writer_rejects_content_changed_after_validation() {
    let w = World::new();
    let p = w.preview();
    let old = Paused::start(&w, &p, "before_begin");
    std::fs::write(&w.note, "# Changed\n\nLater searchable knowledge.\n").unwrap();
    let store = open_existing(&w.db).unwrap();
    ingest::ingest_path(&store, &w.note, &["p"]).unwrap();
    document::ingest_document_file(&store, &w.note, &Default::default()).unwrap();
    drop(store);
    let r = w.revision();
    refused(old.release());
    w.assert_unprepared(r);
}

#[test]
fn process_interruption_before_or_during_commit_leaves_no_partial_preparation() {
    for phase in ["before_begin", "after_begin", "after_write", "in_commit"] {
        let w = World::new();
        let p = w.preview();
        let r = w.revision();
        let before = w.preserved();
        Paused::start(&w, &p, phase).kill();
        w.assert_unprepared(r);
        assert_eq!(w.preserved(), before);
        w.apply(&p);
        assert_eq!(w.revision(), r + 1);
    }
}

#[test]
fn process_interruption_after_commit_is_recovered_from_durable_receipt() {
    let w = World::new();
    let p = w.preview();
    let r = w.revision();
    let old = Paused::start(&w, &p, "after_commit");
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.revision(), r + 1);
    let out = old.kill();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    let receipt = w.apply(&p);
    assert_eq!(receipt["operation_id"], p["operation_id"]);
    assert_eq!(receipt["committed_revision"], r + 1);
    assert_eq!(w.revision(), r + 1);
}

#[test]
fn live_writer_timeout_never_proves_previous_unknown_rolled_back() {
    let w = World::new();
    let p = w.preview();
    let pending = Paused::start(&w, &p, "after_write");
    let error = refused(w.apply_command(&p).output().unwrap()).to_lowercase();
    assert!(
        error.contains("busy") || error.contains("unknown") || error.contains("blocked"),
        "{error}"
    );
    assert!(!error.contains("rolled back"));
    let receipt = output_json(pending.release());
    assert_eq!(w.apply(&p), receipt);
    assert_eq!(w.count(RECEIPTS), 1);
}

#[test]
fn veto_rolls_back_but_lost_ack_retains_original_successful_receipt() {
    let w = World::new();
    let p = w.preview();
    let r = w.revision();
    let before = w.preserved();
    let error = refused(
        w.apply_command(&p)
            .env("MINDSPACE_PREPARATION_FAULT", "veto")
            .output()
            .unwrap(),
    );
    assert!(!error.to_lowercase().contains("unknown"), "{error}");
    w.assert_unprepared(r);
    assert_eq!(w.preserved(), before);
    let error = refused(
        w.apply_command(&p)
            .env("MINDSPACE_PREPARATION_FAULT", "lost_ack")
            .output()
            .unwrap(),
    );
    assert!(error.to_lowercase().contains("unknown"), "{error}");
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.revision(), r + 1);
    let recovered = w.apply(&p);
    assert_eq!(recovered["committed_revision"], r + 1);
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.preserved(), before);
}

#[test]
fn raw_sql_cannot_rewrite_receipts_or_substitute_pinned_identities() {
    let w = World::new();
    w.apply(&w.preview());
    let before = w.preserved();
    let r = w.revision();
    for recursive in [false, true] {
        let raw = w.raw();
        raw.pragma_update(None, "foreign_keys", "ON").unwrap();
        raw.pragma_update(None, "recursive_triggers", recursive)
            .unwrap();
        raw.create_scalar_function(
            "m6_write_revision",
            0,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS,
            move |_| Ok(r + 1),
        )
        .unwrap();
        raw.execute_batch("BEGIN IMMEDIATE").unwrap();
        raw.execute("INSERT INTO m6_revisions VALUES(?1,0,'write')", [r + 1])
            .unwrap();
        // Positive control: a valid generic recording scope can update ordinary source metadata.
        raw.execute(
            "UPDATE logical_sources SET state=state WHERE id=?1",
            [w.source.to_string()],
        )
        .unwrap();
        for table in [PREPARATIONS, RECEIPTS] {
            for sql in [
                format!("UPDATE {table} SET operation_id=operation_id"),
                format!("DELETE FROM {table}"),
                format!("INSERT OR REPLACE INTO {table} SELECT * FROM {table}"),
            ] {
                assert!(raw.execute(&sql, []).is_err(), "mutation accepted: {sql}");
            }
        }
        let fs = w.target["legacy_source_version"].as_str().unwrap();
        let ds = w.target["document_source_version"].as_str().unwrap();
        let x = w.target["extraction_id"].as_str().unwrap();
        let d = w.target["document_id"].as_str().unwrap();
        for id in [fs, ds] {
            assert!(raw
                .execute(
                    "INSERT OR REPLACE INTO sources SELECT * FROM sources WHERE id=?1",
                    [id]
                )
                .is_err());
            let cols = raw
                .prepare("PRAGMA table_info(sources)")
                .unwrap()
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>();
            let expr = cols
                .iter()
                .map(|c| {
                    if c == "id" {
                        "?2".into()
                    } else {
                        format!("\"{c}\"")
                    }
                })
                .collect::<Vec<_>>()
                .join(",");
            assert!(raw
                .execute(
                    &format!(
                        "INSERT OR REPLACE INTO sources SELECT {expr} FROM sources WHERE id=?1"
                    ),
                    rusqlite::params![id, Uuid::now_v7().to_string()]
                )
                .is_err());
        }
        assert!(raw
            .execute(
                "INSERT OR REPLACE INTO doc_extractions SELECT * FROM doc_extractions WHERE id=?1",
                [x]
            )
            .is_err());
        let cols = raw
            .prepare("PRAGMA table_info(doc_extractions)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        let expr = cols
            .iter()
            .map(|c| {
                if c == "id" {
                    "?2".into()
                } else {
                    format!("\"{c}\"")
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        assert!(raw.execute(&format!("INSERT OR REPLACE INTO doc_extractions SELECT {expr} FROM doc_extractions WHERE id=?1"),rusqlite::params![x,Uuid::now_v7().to_string()]).is_err());
        assert!(raw
            .execute("UPDATE logical_sources SET kind='file' WHERE id=?1", [d])
            .is_err());
        assert!(raw
            .execute(
                "INSERT OR REPLACE INTO doc_nodes SELECT * FROM doc_nodes WHERE extraction_id=?1",
                [x]
            )
            .is_err());
        assert!(raw.execute("INSERT INTO doc_nodes(extraction_id,path,kind,precision) VALUES(?1,'999999','paragraph','none')",[x]).is_err());
        raw.execute_batch("ROLLBACK").unwrap();
    }
    assert_eq!(w.revision(), r);
    assert_eq!(w.preserved(), before);
    assert_eq!(w.count(RECEIPTS), 1);
}

#[test]
fn preparation_cannot_represent_document_ownership_or_publication() {
    let w = World::new();
    w.apply(&w.preview());
    let raw = w.raw();
    for sql in [
        "UPDATE asset_ownership SET owner='document'",
        "UPDATE asset_ownership SET epoch=2",
        "UPDATE document_preparation_enrollments SET publication_state='active'",
        "UPDATE document_preparation_enrollments SET owner='document'",
        "UPDATE document_preparation_enrollments SET selected_chunk_set='forged'",
    ] {
        assert!(raw.execute(sql, []).is_err(), "accepted: {sql}");
    }
    let memories:i64=raw.query_row("SELECT COUNT(*) FROM memories m JOIN assertions a ON a.id=m.assertion_id JOIN evidence e ON e.id=a.evidence_id JOIN sources s ON s.id=e.source_id WHERE s.kind='document'",[],|r|r.get(0)).unwrap();
    assert_eq!(memories, 0);
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
        assert!(send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"untrusted-preparation-client","version":"1"}}}))["result"].is_object());
        send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        let tools = send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
        let mut refusals = Vec::new();
        for (i, name) in [
            "ownership_enroll_document_preparation",
            "document_prepare",
            "document_publish",
        ]
        .into_iter()
        .enumerate()
        {
            let out = send(
                json!({"jsonrpc":"2.0","id":i+3,"method":"tools/call","params":{"name":name,"arguments":{"principal":"operator","owner":"document","apply":true}}}),
            );
            assert!(out["error"].is_object());
            refusals.push(out);
        }
        json!({"tools":tools,"refusals":refusals})
    };
    drop(input);
    assert!(child.wait().unwrap().success());
    result
}

#[test]
fn real_mcp_has_no_preparation_or_publication_route_and_tools_are_unchanged() {
    let w = World::new();
    let before = mcp_probe(&w.db);
    let tools = before["tools"]["result"]["tools"].as_array().unwrap();
    assert!(tools
        .iter()
        .all(|t| !["ownership", "preparation", "publish"]
            .iter()
            .any(|needle| t["name"].as_str().unwrap().contains(needle))));
    w.apply(&w.preview());
    assert_eq!(mcp_probe(&w.db), before);
}
