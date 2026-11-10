//! Isolated lexical staging, real local administrative processes, and durable recovery.
//! Model A trusts the operating account; these tests do not authenticate a human.
#![cfg(all(target_os = "linux", feature = "embeddings"))]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};

use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use mindspace::core::{AssertionEmbedding, SearchQuery, Ts, Uuid};
use mindspace::embeddings::{budget::TokenBudget, manifest::NOMIC_V1_5};
use mindspace::ingest::{self, document};
use mindspace::storage::Store;
use mindspace::{initialize, open_existing};
use rusqlite::Connection;
use serde_json::{json, Value};

const TEXT: &str = "# Enrollment\n\nPreparation preserves searchable knowledge.\n";
const SETS: &str = "doc_staged_lexical_sets";
const RECEIPTS: &str = "doc_stage_receipts";

struct World {
    dir: tempfile::TempDir,
    db: PathBuf,
    note: PathBuf,
    target: Value,
}

impl World {
    fn new() -> Self {
        Self::with_text(TEXT)
    }

    fn with_text(text: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mind.db");
        let note = dir.path().join("legacy.md");
        std::fs::write(&note, text).unwrap();
        initialize(&db).unwrap();
        let store = open_existing(&db).unwrap();
        store.create_space("p", "").unwrap();
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
                derivation_key: mindspace::embeddings::manifest::derivation_from_manifest(
                    &FAKE_ENTRY,
                )
                .derivation_key(),
                model: FAKE_ID.into(),
                model_version: FAKE_ENTRY.version.into(),
                dimension: 4,
                embedding: vec![1.0, 0.0, 0.0, 0.0],
            }])
            .unwrap();
        let budget = TokenBudget::from_file(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nomic-tokenizer.json"),
            &NOMIC_V1_5,
        )
        .unwrap();
        let report =
            document::ingest_chunked_document_file(&store, &note, &Default::default(), &budget)
                .unwrap();
        let chunk_set: (String,String,String) = raw.query_row(
            "SELECT version,budget_identity,plan_hash FROM doc_chunk_sets WHERE extraction_id=?1",
            [report.extraction_id.to_string()], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
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
            "document_id":report.logical_id.to_string(),"document_source_version":report.source_id.to_string(),"extraction_id":report.extraction_id.to_string()});
        let mut w = Self {
            dir,
            db,
            note,
            target,
        };
        let r = w.revision();
        let mut prep = w.target_command(&w.target, r);
        prep.arg("--nonce").arg(Uuid::now_v7().to_string());
        let p = output_json(prep.output().unwrap());
        let mut apply = w.target_command(&w.target, r);
        apply
            .arg("--operation-id")
            .arg(p["operation_id"].as_str().unwrap())
            .args(["--apply", "--confirm-intent"])
            .arg(p["digest"].as_str().unwrap());
        output_json(apply.output().unwrap());
        w.target["chunker_version"] = chunk_set.0.into();
        w.target["budget_identity"] = chunk_set.1.into();
        w.target["plan_hash"] = chunk_set.2.into();
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
    fn target_command(&self, target: &Value, revision: i64) -> Command {
        let mut c = self.command();
        let staging = target.get("plan_hash").is_some();
        c.args([
            "ownership",
            if staging {
                "stage-document-lexical"
            } else {
                "enroll-document-preparation"
            },
        ]);
        for (flag, key) in [
            ("--asset-id", "asset_id"),
            ("--legacy-source-version", "legacy_source_version"),
            ("--document-id", "document_id"),
            ("--document-source-version", "document_source_version"),
            ("--extraction-id", "extraction_id"),
        ] {
            c.arg(flag).arg(target[key].as_str().unwrap());
        }
        if staging {
            for (flag, key) in [
                ("--chunker-version", "chunker_version"),
                ("--budget-identity", "budget_identity"),
                ("--plan-hash", "plan_hash"),
            ] {
                c.arg(flag).arg(target[key].as_str().unwrap());
            }
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
        let tables=raw.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE 'doc_staged_%' AND name NOT IN ('doc_stage_receipts') AND name NOT IN ('m6_revisions','m6_row_availability') ORDER BY name").unwrap().query_map([],|r|r.get::<_,String>(0)).unwrap().map(Result::unwrap).collect::<Vec<_>>();
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
    fn assert_unstaged(&self, revision: i64) {
        assert_eq!(self.count(SETS), 0);
        assert_eq!(self.count("doc_staged_entries"), 0);
        assert_eq!(self.count("doc_staged_fts"), 0);
        assert_eq!(self.count(RECEIPTS), 0);
        assert_eq!(self.revision(), revision);
    }
}

fn clean_command() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_mind"));
    for key in [
        "MINDSPACE_DB",
        "MINDSPACE_LEXICAL_STAGE_PAUSE",
        "MINDSPACE_LEXICAL_STAGE_GATE_PATH",
        "MINDSPACE_LEXICAL_STAGE_FAULT",
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

struct Paused {
    child: Option<Child>,
    prefix: PathBuf,
}
impl Paused {
    fn start(w: &World, p: &Value, phase: &str) -> Self {
        let prefix = w.dir.path().join(format!("pause-{phase}"));
        let child = w
            .apply_command(p)
            .env("MINDSPACE_LEXICAL_STAGE_PAUSE", phase)
            .env("MINDSPACE_LEXICAL_STAGE_GATE_PATH", &prefix)
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
    w.assert_unstaged(r);
}

#[test]
fn process_interruption_before_or_during_commit_leaves_no_partial_lexical_staging() {
    for phase in [
        "before_build",
        "before_begin",
        "after_begin",
        "after_write",
        "before_commit",
        "in_commit",
    ] {
        let w = World::new();
        let p = w.preview();
        let r = w.revision();
        let before = w.preserved();
        Paused::start(&w, &p, phase).kill();
        w.assert_unstaged(r);
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
fn concurrent_inspection_waits_for_staging_commit_or_rollback_without_partial_readiness() {
    struct Inspector(Option<Child>);
    impl Drop for Inspector {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    for commit in [true, false] {
        let w = World::new();
        let p = w.preview();
        let r = w.revision();
        let writer = Paused::start(&w, &p, "after_write");
        let mut inspector = Inspector(Some(
            w.command()
                .args(["ownership", "inspect-lexical-stage", "--stage-id"])
                .arg(p["stage_id"].as_str().unwrap())
                .arg("--json")
                .spawn()
                .unwrap(),
        ));
        // Observe that this process actually opened the target, rather than assuming
        // it was scheduled merely because spawn returned. /proc belongs to our child.
        let fd_dir = PathBuf::from(format!("/proc/{}/fd", inspector.0.as_ref().unwrap().id()));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                inspector.0.as_mut().unwrap().try_wait().unwrap().is_none(),
                "inspection returned while staging was uncommitted"
            );
            if std::fs::read_dir(&fd_dir).is_ok_and(|fds| {
                fds.flatten()
                    .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|path| path == w.db))
            }) {
                break;
            }
            assert!(Instant::now() < deadline, "inspector never opened target");
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(100));
        assert!(inspector.0.as_mut().unwrap().try_wait().unwrap().is_none());
        w.assert_unstaged(r);
        if commit {
            output_json(writer.release());
        } else {
            assert!(!writer.kill().status.success());
        }
        let result = inspector.0.take().unwrap().wait_with_output().unwrap();
        if commit {
            let status = output_json(result);
            assert_eq!(status["current_integrity"], "verified");
            assert_eq!(status["verified_at_revision"], r + 1);
            assert_eq!(w.revision(), r + 1, "inspection must not consume R");
            assert_eq!(w.count(RECEIPTS), 1);
        } else {
            assert!(refused(result).contains("staged artifact does not exist"));
            w.assert_unstaged(r);
        }
    }
}

#[test]
fn veto_rolls_back_but_lost_ack_retains_original_successful_receipt() {
    let w = World::new();
    let p = w.preview();
    let r = w.revision();
    let before = w.preserved();
    let error = refused(
        w.apply_command(&p)
            .env("MINDSPACE_LEXICAL_STAGE_FAULT", "veto")
            .output()
            .unwrap(),
    );
    assert!(!error.to_lowercase().contains("unknown"), "{error}");
    w.assert_unstaged(r);
    assert_eq!(w.preserved(), before);
    let error = refused(
        w.apply_command(&p)
            .env("MINDSPACE_LEXICAL_STAGE_FAULT", "lost_ack")
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

fn inspect(w: &World, stage: &Value) -> Value {
    output_json(
        w.command()
            .args([
                "ownership",
                "inspect-lexical-stage",
                "--stage-id",
                stage["stage_id"].as_str().unwrap(),
                "--json",
            ])
            .output()
            .unwrap(),
    )
}

#[test]
fn explicit_stage_is_isolated_and_exact_retry_preserves_original_receipt() {
    let w = World::new();
    let before = w.preserved();
    let search = w.search();
    let r = w.revision();
    let p = w.preview();
    w.assert_unstaged(r);
    assert!(p["operation_id"]
        .as_str()
        .unwrap()
        .starts_with("local-doc-stage-lexical@1:"));
    let stage = w.apply(&p);
    assert_eq!(stage["outcome"], "committed");
    assert_eq!(stage["current_integrity"], "not_checked");
    assert_eq!(w.count(SETS), 1);
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.apply(&p), stage);
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.preserved(), before);
    assert_eq!(w.search(), search);
    let status = inspect(&w, &stage);
    assert_eq!(status["current_integrity"], "verified");
    assert_eq!(status["canonical_verified_at_commit"], true);
    assert_eq!(status["token_assurance"], "not_performed_lexical");
    assert_eq!(status["original_bytes_status"], "not_checked");
    assert_eq!(w.revision(), r + 1);
    assert!(!w
        .raw()
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .exists([])
        .unwrap());
}

#[test]
fn new_operation_for_existing_artifact_is_read_only_and_not_an_extra_receipt() {
    let w = World::new();
    let first = w.apply(&w.preview());
    let r = w.revision();
    let before = w.preserved();
    let second = w.apply(&w.preview());
    assert_eq!(second["outcome"], "existing_artifact");
    assert_eq!(second["stage_id"], first["stage_id"]);
    assert!(second["operation_id"].is_null());
    assert!(second["committed_revision"].is_null());
    assert_eq!(second["current_integrity"], "verified");
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.revision(), r);
    assert_eq!(w.preserved(), before);
}

#[test]
fn all_intent_fields_and_confirmation_are_bound_before_and_after_commit() {
    let w = World::new();
    let p = w.preview();
    for committed in [false, true] {
        if committed {
            w.apply(&p);
        }
        let r = w.revision();
        for key in [
            "asset_id",
            "legacy_source_version",
            "document_id",
            "document_source_version",
            "extraction_id",
            "chunker_version",
            "budget_identity",
            "plan_hash",
        ] {
            let mut forged = p.clone();
            forged["test_target"][key] = if key == "plan_hash" {
                "0".repeat(64)
            } else if matches!(key, "chunker_version" | "budget_identity") {
                "other@1".into()
            } else {
                Uuid::now_v7().to_string()
            }
            .into();
            refused(w.apply_command(&forged).output().unwrap());
            assert_eq!(w.revision(), r);
        }
        let mut forged = p.clone();
        forged["digest"] = "0".repeat(64).into();
        refused(w.apply_command(&forged).output().unwrap());
        let mut forged = p.clone();
        forged["test_revision"] = (r + 77).into();
        refused(w.apply_command(&forged).output().unwrap());
        assert_eq!(w.revision(), r);
    }
}

#[test]
fn six_process_exact_retries_commit_one_stage_and_one_receipt() {
    let w = World::new();
    let p = w.preview();
    let r = w.revision();
    let children = (0..6)
        .map(|_| w.apply_command(&p).spawn().unwrap())
        .collect::<Vec<_>>();
    let receipts = children
        .into_iter()
        .map(|c| output_json(c.wait_with_output().unwrap()))
        .collect::<Vec<_>>();
    assert!(receipts.iter().all(|x| x == &receipts[0]));
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.count(SETS), 1);
}

#[test]
fn competing_process_intents_cannot_create_two_artifacts() {
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
    // Stale expected-state refusals or an already existing artifact are both safe.
    let successes = outcomes
        .into_iter()
        .filter(|x| x.status.success())
        .map(output_json)
        .collect::<Vec<_>>();
    assert!(!successes.is_empty());
    assert_eq!(
        successes
            .iter()
            .filter(|x| x["outcome"] == "committed")
            .count(),
        1
    );
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.count(SETS), 1);
}

#[test]
fn canonical_ranges_preserve_crlf_unicode_controls_and_structural_metadata() {
    let text=format!("\u{feff}# First\r\n\r\n{}\r\n## Second\r\n\r\n```rust\r\nlet x = \"👩‍💻 e\u{301} \u{200b}\u{202e}bidi\u{202c}\";\r\n```\r\n", "knowledge 日本語 words ".repeat(1800));
    let w = World::with_text(&text);
    let p = w.preview();
    let stage = w.apply(&p);
    let raw = w.raw();
    let mut stmt=raw.prepare("SELECT e.ordinal,e.text_start,e.text_end,e.content,e.anchor,e.context_json,c.anchor,c.context_json FROM doc_staged_entries e JOIN doc_chunks c ON c.id=e.chunk_id WHERE e.stage_id=?1 ORDER BY e.ordinal").unwrap();
    let rows = stmt
        .query_map([stage["stage_id"].as_str().unwrap()], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)? as usize,
                r.get::<_, i64>(2)? as usize,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
            ))
        })
        .unwrap();
    let canonical = text.trim_start_matches('\u{feff}');
    let mut offset = 0;
    let mut reconstructed = String::new();
    let store = open_existing(&w.db).unwrap();
    for (n, row) in rows.enumerate() {
        let (ordinal, start, end, content, anchor, context, original_anchor, original_context) =
            row.unwrap();
        assert_eq!(ordinal, n as i64);
        assert_eq!(start, offset);
        assert!(end > start);
        assert!(canonical.is_char_boundary(start) && canonical.is_char_boundary(end));
        assert_eq!(content, canonical[start..end]);
        assert_eq!(anchor, original_anchor);
        assert_eq!(context, original_context);
        let parsed = anchor.parse().unwrap();
        let resolved = store.resolve_document_anchor(&parsed).unwrap().unwrap();
        assert_eq!(resolved.text.as_deref(), Some(content.as_str()));
        reconstructed.push_str(&content);
        offset = end;
    }
    assert_eq!(offset, canonical.len());
    assert_eq!(reconstructed, canonical);
    assert_eq!(inspect(&w, &stage)["current_integrity"], "verified");
}

#[test]
fn missing_original_file_is_not_misrepresented_as_original_byte_authentication() {
    let w = World::new();
    std::fs::remove_file(&w.note).unwrap();
    let stage = w.apply(&w.preview());
    let status = inspect(&w, &stage);
    assert_eq!(status["current_integrity"], "verified");
    assert_eq!(status["original_bytes_status"], "not_checked");
}

#[test]
fn historical_receipt_survives_later_reingestion_without_resolving_a_new_revision() {
    let w = World::new();
    let p = w.preview();
    let receipt = w.apply(&p);
    std::fs::write(&w.note, "# Later\n\nChanged searchable knowledge.\n").unwrap();
    let store = open_existing(&w.db).unwrap();
    ingest::ingest_path(&store, &w.note, &["p"]).unwrap();
    document::ingest_document_file(&store, &w.note, &Default::default()).unwrap();
    drop(store);
    let r = w.revision();
    assert_eq!(w.apply(&p), receipt);
    assert_eq!(w.revision(), r);
    assert_eq!(inspect(&w, &receipt)["current_integrity"], "verified");
}

#[test]
fn fts_posting_loss_refuses_current_readiness_but_preserves_original_commit_receipt() {
    let w = World::new();
    let p = w.preview();
    let receipt = w.apply(&p);
    let r = w.revision();
    let raw = w.raw();
    // External-content COUNT remains unchanged after posting deletion: a count-only verifier fails this oracle.
    let count = w.count("doc_staged_fts");
    raw.execute_batch("INSERT INTO doc_staged_fts(doc_staged_fts) VALUES('delete-all')")
        .unwrap();
    assert_eq!(w.count("doc_staged_fts"), count);
    assert_ne!(inspect(&w, &receipt)["current_integrity"], "verified");
    assert_eq!(w.apply(&p), receipt);
    assert_eq!(w.revision(), r);
    refused(w.apply_command(&w.preview()).output().unwrap());
}

// Query-time fake runtime only: staging must never invoke this document encoder.
const FAKE_ID: &str = "test-embed-model";
const MODEL_BYTES: &[u8] = b"synthetic model artifact for retrieval tests\n";
const TOKENIZER_BYTES: &[u8] = b"{\"synthetic\":true}\n";
static FAKE_ENTRY: mindspace::embeddings::manifest::ModelManifestEntry =
    mindspace::embeddings::manifest::ModelManifestEntry {
        id: FAKE_ID,
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
            mindspace::embeddings::manifest::Artifact {
                filename: "model.safetensors",
                url: "https://example.invalid/model.safetensors",
                sha256: "269cbb1a5ed49786cc52cfa6e29a80d5147d461ca878f9c84651c947bf5f750f",
                size: MODEL_BYTES.len() as u64,
            },
            mindspace::embeddings::manifest::Artifact {
                filename: "tokenizer.json",
                url: "https://example.invalid/tokenizer.json",
                sha256: "cb8daed7b30399a1c3c8b83b3b7bee4b774a30fc389afc1d375e4c23a3cc4ae8",
                size: TOKENIZER_BYTES.len() as u64,
            },
        ],
    };
struct QueryRuntime;
impl mindspace::embeddings::EmbeddingRuntime for QueryRuntime {
    fn embed_document(&self, _: &str) -> mindspace::Result<Vec<f32>> {
        panic!("lexical staging must never embed a document")
    }
    fn embed_query(&self, _: &str) -> mindspace::Result<Vec<f32>> {
        Ok(vec![1., 0., 0., 0.])
    }
    fn dimension(&self) -> usize {
        4
    }
    fn model_id(&self) -> &str {
        FAKE_ID
    }
}
fn fake_lookup(id: &str) -> Option<&'static mindspace::embeddings::manifest::ModelManifestEntry> {
    (id == FAKE_ID).then_some(&FAKE_ENTRY)
}
fn capability(w: &World) -> mindspace::retrieval::SemanticCapability {
    let store = open_existing(&w.db).unwrap();
    mindspace::embeddings::manager::set_enabled(&store, true).unwrap();
    store.set_meta("embedding.model", FAKE_ID).unwrap();
    let base = w.dir.path().join("data");
    let dir = mindspace::embeddings::manager::model_dir_in(&base, FAKE_ID);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("model.safetensors"), MODEL_BYTES).unwrap();
    std::fs::write(dir.join("tokenizer.json"), TOKENIZER_BYTES).unwrap();
    mindspace::retrieval::semantic::resolve_with(&store, &base, fake_lookup, &|_, _| {
        Ok(Box::new(QueryRuntime))
    })
    .unwrap()
}
fn all_retrieval(w: &World, cap: &mindspace::retrieval::SemanticCapability, cutoff: Ts) -> Value {
    use mindspace::retrieval::{search_with_mode, RetrievalOptions, SearchMode};
    let store = open_existing(&w.db).unwrap();
    let mut out = Vec::new();
    for historical in [false, true] {
        for mode in [
            SearchMode::Lexical,
            SearchMode::Semantic,
            SearchMode::Hybrid,
            SearchMode::Auto,
        ] {
            let query = SearchQuery {
                text: "knowledge".into(),
                space_ids: None,
                limit: 50,
                at: historical.then_some(cutoff),
                include_superseded: false,
                include_retired: false,
            };
            let result =
                search_with_mode(&store, query, mode, cap, RetrievalOptions::default()).unwrap();
            assert!(
                !result.hits.is_empty(),
                "empty {mode:?} result is not an isolation oracle"
            );
            out.push(serde_json::to_value(result).unwrap());
        }
    }
    json!(out)
}

#[test]
fn actual_lexical_semantic_hybrid_auto_and_asof_results_and_raw_scores_stay_identical() {
    let w = World::new();
    // A second visible source provides real corpus statistics and a nontrivial ranking.
    let second = w.dir.path().join("second.md");
    std::fs::write(
        &second,
        "# Other\n\nKnowledge knowledge with another competing phrase.\n",
    )
    .unwrap();
    let store = open_existing(&w.db).unwrap();
    ingest::ingest_path(&store, &second, &["p"]).unwrap();
    drop(store);
    let cap = capability(&w);
    assert!(matches!(
        cap,
        mindspace::retrieval::SemanticCapability::Ready(_)
    ));
    let cutoff = Ts::now();
    let before = all_retrieval(&w, &cap, cutoff);
    let rows = w.preserved();
    let p = w.preview();
    w.apply(&p);
    assert_eq!(all_retrieval(&w, &cap, cutoff), before);
    assert_eq!(w.preserved(), rows);
}

#[test]
fn ordinary_m6_scope_cannot_forge_or_mutate_staged_authority_and_citations() {
    use rusqlite::functions::FunctionFlags;
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
        assert!(
            raw.execute("UPDATE logical_sources SET state=state", [])
                .unwrap()
                > 0
        );
        for table in [SETS, RECEIPTS, "doc_staged_entries"] {
            for sql in [
                format!("UPDATE {table} SET rowid=rowid"),
                format!("DELETE FROM {table}"),
                format!("INSERT OR REPLACE INTO {table} SELECT * FROM {table}"),
            ] {
                assert!(raw.execute(&sql, []).is_err(), "accepted {sql}");
            }
        }
        for sql in [
            "UPDATE doc_staged_lexical_sets SET state='published'",
            "UPDATE doc_staged_lexical_sets SET token_assurance='token_verified'",
            "UPDATE asset_ownership SET owner='document'",
            "UPDATE logical_sources SET kind='document' WHERE id IN (SELECT source_id FROM doc_staged_lexical_sets)",
            "UPDATE sources SET content_hash='0000000000000000000000000000000000000000000000000000000000000000' WHERE id IN (SELECT document_source_version FROM doc_staged_lexical_sets)",
            "UPDATE asset_ownership SET epoch=2",
            "INSERT OR REPLACE INTO doc_chunk_sets SELECT * FROM doc_chunk_sets",
            "INSERT OR REPLACE INTO doc_chunks SELECT * FROM doc_chunks",
            "INSERT OR REPLACE INTO doc_nodes SELECT * FROM doc_nodes",
            "INSERT OR REPLACE INTO doc_extractions SELECT * FROM doc_extractions",
            "INSERT OR REPLACE INTO sources SELECT * FROM sources",
            "INSERT INTO doc_nodes(extraction_id,path,kind,precision) SELECT extraction_id,'999999','paragraph','none' FROM doc_chunk_sets",
            "INSERT INTO doc_chunks SELECT replace(id,substr(id,1,1),'0'),extraction_id,version,budget_identity,ordinal+999,text_start,text_end,token_count,context_json,anchor FROM doc_chunks",
            "INSERT INTO doc_staged_entries SELECT entry_rowid+99,entry_id,stage_id,ordinal+99,chunk_id,text_start,text_end,content,payload_hash,anchor,context_json,citation_state,original_bytes_state FROM doc_staged_entries",
            "INSERT INTO doc_stage_receipts SELECT * FROM doc_stage_receipts WHERE 1 ON CONFLICT(operation_id) DO UPDATE SET digest=excluded.digest",
        ] {
            let error = raw.execute(sql, []).expect_err(&format!("accepted {sql}"));
            assert!(!error.to_string().contains("syntax error"), "invalid SQL oracle: {sql}: {error}");
        }
        raw.execute_batch("ROLLBACK").unwrap();
    }
    assert_eq!(w.revision(), r);
    assert_eq!(w.preserved(), before);
    assert_eq!(w.count(RECEIPTS), 1);
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
            "ownership_stage_document_lexical",
            "stage_document_lexical",
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
fn real_mcp_has_no_staging_or_publication_route_and_tools_are_unchanged() {
    let w = World::new();
    let before = mcp_probe(&w.db);
    let tools = before["tools"]["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().all(
        |t| !["ownership", "staging", "publish"]
            .iter()
            .any(|needle| t["name"].as_str().unwrap().contains(needle))
    ));
    w.apply(&w.preview());
    assert_eq!(mcp_probe(&w.db), before);
}

#[test]
fn missing_or_incompatible_explicit_versions_are_refused_without_automatic_selection() {
    let w = World::new();
    let r = w.revision();
    let before = w.preserved();
    for key in [
        "asset_id",
        "legacy_source_version",
        "document_id",
        "document_source_version",
        "extraction_id",
        "chunker_version",
        "budget_identity",
        "plan_hash",
    ] {
        let mut target = w.target.clone();
        target[key] = if key == "plan_hash" {
            "0".repeat(64)
        } else if matches!(key, "chunker_version" | "budget_identity") {
            "missing@1".into()
        } else {
            Uuid::now_v7().to_string()
        }
        .into();
        refused(
            w.preview_command(&target, r, Uuid::now_v7())
                .output()
                .unwrap(),
        );
    }
    let mut wrong = w.target.clone();
    wrong["document_id"] = wrong["asset_id"].clone();
    refused(
        w.preview_command(&wrong, r, Uuid::now_v7())
            .output()
            .unwrap(),
    );
    let args = w
        .preview_command(&w.target, r, Uuid::now_v7())
        .get_args()
        .map(|s| s.to_owned())
        .collect::<Vec<_>>();
    for omitted in ["--chunker-version", "--budget-identity", "--plan-hash"] {
        let position = args.iter().position(|x| x == omitted).unwrap();
        let remaining = args
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != position && *i != position + 1)
            .map(|(_, arg)| arg);
        refused(clean_command().args(remaining).output().unwrap());
    }
    w.assert_unstaged(r);
    assert_eq!(w.preserved(), before);
}

#[test]
fn malformed_modes_and_control_bearing_versions_do_not_create_authority() {
    let w = World::new();
    let r = w.revision();
    let mut command = w.target_command(&w.target, r);
    command.arg("--apply");
    refused(command.output().unwrap());
    let mut target = w.target.clone();
    target["chunker_version"] = "chunk\u{202e}wrong".into();
    let output = w
        .preview_command(&target, r, Uuid::now_v7())
        .output()
        .unwrap();
    refused(output);
    w.assert_unstaged(r);
}

#[test]
fn partial_entry_fts_and_receipt_failures_roll_back_the_entire_artifact() {
    for fault in ["entry:1", "receipt"] {
        let w = World::new();
        let p = w.preview();
        let r = w.revision();
        let before = w.preserved();
        let error = refused(
            w.apply_command(&p)
                .env("MINDSPACE_LEXICAL_STAGE_FAULT", fault)
                .output()
                .unwrap(),
        );
        assert!(!error.to_lowercase().contains("unknown"), "{error}");
        w.assert_unstaged(r);
        assert_eq!(w.preserved(), before);
        assert_eq!(w.apply(&p)["committed_revision"], r + 1);
    }
}

#[test]
fn broken_stdout_reports_committed_unknown_but_existing_artifact_is_read_only_failure() {
    let w = World::new();
    let p = w.preview();
    let r = w.revision();
    let mut child = w.apply_command(&p).spawn().unwrap();
    drop(child.stdout.take());
    let error = refused(child.wait_with_output().unwrap());
    assert!(error.to_lowercase().contains("unknown"), "{error}");
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.revision(), r + 1);
    let recovered = w.apply(&p);
    assert_eq!(recovered["committed_revision"], r + 1);
    let p2 = w.preview();
    let mut child = w.apply_command(&p2).spawn().unwrap();
    drop(child.stdout.take());
    let error = refused(child.wait_with_output().unwrap());
    assert!(
        !error.to_lowercase().contains("unknown"),
        "read-only ExistingArtifact was misclassified: {error}"
    );
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.revision(), r + 1);
}

#[test]
fn nonce_cannot_be_rehashed_into_a_different_intent_after_original_commit() {
    let w = World::new();
    let original = w.preview();
    w.apply(&original);
    let r = w.revision();
    let operation = original["operation_id"].as_str().unwrap();
    let nonce = Uuid::parse_str(operation.split(':').nth(1).unwrap()).unwrap();
    let mut changed = output_json(w.preview_command(&w.target, r, nonce).output().unwrap());
    assert_ne!(changed["operation_id"], original["operation_id"]);
    changed["test_target"] = w.target.clone();
    changed["test_revision"] = r.into();
    refused(w.apply_command(&changed).output().unwrap());
    assert_eq!(w.revision(), r);
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.apply(&original)["committed_revision"], r);
}

/// Opt-in measurements of the actual administrative processes; setup/token chunking is excluded.
/// Root can wrap the test binary in /usr/bin/time for process-tree peak RSS observation.
#[test]
#[ignore = "opt-in process performance measurements; no timing acceptance threshold"]
fn lexical_staging_process_measurements() {
    let bytes = std::env::var("MINDSPACE_S3D_PROCESS_MEASURE_BYTES")
        .ok()
        .map(|v| v.parse::<usize>().expect("measurement bytes"))
        .unwrap_or(256 * 1024);
    let runs = std::env::var("MINDSPACE_S3D_PROCESS_MEASURE_RUNS")
        .ok()
        .map(|v| v.parse::<usize>().expect("measurement runs"))
        .unwrap_or(3);
    fn physical(w: &World) -> Value {
        let size = |suffix: &str| {
            let mut path = w.db.as_os_str().to_owned();
            path.push(suffix);
            std::fs::metadata(Path::new(&path))
                .map(|m| m.len())
                .unwrap_or(0)
        };
        json!({"database":size(""),"wal":size("-wal"),"shm":size("-shm")})
    }
    for (kind,unit) in [
        ("prose","Knowledge preserves exact citation boundaries across paragraphs.\n\n"),
        ("cjk","知識検索日本語中文記録。\n\n"),
        ("unbroken","uninterruptedcharacters0123456789"),
        ("markdown","# Section\n\nKnowledge with **formatting**.\n\n- first item\n- second item\n\n```rust\nlet unicode = \"😀\";\n```\n\n"),
    ] {
        if std::env::var("MINDSPACE_S3D_PROCESS_MEASURE_KIND").is_ok_and(|selected| selected != kind) {continue;}
        for run in 0..runs {
            let mut text=unit.repeat(bytes.div_ceil(unit.len()));
            let mut end=bytes.min(text.len());while !text.is_char_boundary(end) {end-=1;}text.truncate(end);
            let w=World::with_text(&text);let before=physical(&w);
            let start=Instant::now();let p=w.preview();let preview_ms=start.elapsed().as_secs_f64()*1000.;
            let start=Instant::now();let receipt=w.apply(&p);let apply_ms=start.elapsed().as_secs_f64()*1000.;
            let start=Instant::now();let status=inspect(&w,&receipt);let inspect_ms=start.elapsed().as_secs_f64()*1000.;
            assert_eq!(status["current_integrity"],"verified");
            let raw=w.raw();let (chunks,source_bytes,metadata_bytes):(i64,i64,i64)=raw.query_row("SELECT entry_count,source_bytes_len,metadata_bytes FROM doc_staged_lexical_sets",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();drop(raw);
            println!("{}",json!({"measurement":"s3d.lexical.process@1","kind":kind,"run":run+1,"requested_bytes":bytes,"source_bytes":source_bytes,"entries":chunks,"logical_metadata_bytes":metadata_bytes,"preview_ms":preview_ms,"apply_ms":apply_ms,"inspect_ms":inspect_ms,"physical_before":before,"physical_after":physical(&w)}));
        }
    }
}

#[test]
#[ignore = "requires an independently built production binary; explicitly invoked"]
fn ordinary_production_binary_stages_and_inspects_without_test_hooks() {
    let binary = std::env::var_os("MINDSPACE_S3D_PRODUCTION_BINARY")
        .expect("path to independently built production binary");
    let bytes = std::fs::read(&binary).unwrap();
    for marker in [
        "MINDSPACE_LEXICAL_STAGE_FAULT",
        "MINDSPACE_LEXICAL_STAGE_PAUSE",
        "MINDSPACE_LEXICAL_STAGE_GATE_PATH",
        "MINDSPACE_STORE_GATE_PAUSE_IN_COMMIT",
    ] {
        assert!(!bytes.windows(marker.len()).any(|b| b == marker.as_bytes()));
    }
    let w = World::new();
    let run = |command: Command| {
        let mut production = Command::new(&binary);
        production
            .args(command.get_args())
            .env_remove("MINDSPACE_DB");
        production
            .env("MINDSPACE_LEXICAL_STAGE_FAULT", "lost_ack")
            .env("MINDSPACE_LEXICAL_STAGE_PAUSE", "after_begin")
            .env(
                "MINDSPACE_LEXICAL_STAGE_GATE_PATH",
                w.dir.path().join("ignored-gate"),
            )
            .stdin(Stdio::null());
        output_json(production.output().unwrap())
    };
    let r = w.revision();
    let mut preview = run(w.preview_command(&w.target, r, Uuid::now_v7()));
    preview["test_target"] = w.target.clone();
    preview["test_revision"] = r.into();
    let receipt = run(w.apply_command(&preview));
    assert_eq!(receipt["outcome"], "committed");
    assert_eq!(w.revision(), r + 1);
    assert_eq!(run(w.apply_command(&preview)), receipt);
    let mut inspect = w.command();
    inspect
        .args(["ownership", "inspect-lexical-stage", "--stage-id"])
        .arg(receipt["stage_id"].as_str().unwrap())
        .arg("--json");
    assert_eq!(run(inspect)["current_integrity"], "verified");
    assert!(!w.dir.path().join("ignored-gate.reached").exists());
    println!("production smoke passed: {}", Path::new(&binary).display());
}

#[test]
fn later_selected_versions_stage_and_seal_independently_while_corrupt_new_fs_is_refused() {
    use rusqlite::functions::FunctionFlags;
    fn later_target(w: &World) -> Value {
        let text = "# Later stored revision\r\n\r\nDifferent knowledge 漢字 e\u{301} payload.\r\n";
        std::fs::write(&w.note, text).unwrap();
        let store = open_existing(&w.db).unwrap();
        ingest::ingest_path(&store, &w.note, &["p"]).unwrap();
        let budget = TokenBudget::from_file(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nomic-tokenizer.json"),
            &NOMIC_V1_5,
        )
        .unwrap();
        let report =
            document::ingest_chunked_document_file(&store, &w.note, &Default::default(), &budget)
                .unwrap();
        assert_eq!(report.logical_id.to_string(), w.target["document_id"]);
        let raw = w.raw();
        let fs:String=raw.query_row("SELECT s.id FROM sources s JOIN asset_ownership a ON a.source_id=s.logical_id WHERE a.asset_id=?1 AND s.content_hash=?2", rusqlite::params![w.target["asset_id"].as_str().unwrap(), mindspace::core::sha256_hex(text.as_bytes())], |r|r.get(0)).unwrap();
        let (version,budget,plan):(String,String,String)=raw.query_row("SELECT version,budget_identity,plan_hash FROM doc_chunk_sets WHERE extraction_id=?1",[report.extraction_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        let mut target = w.target.clone();
        target["legacy_source_version"] = fs.into();
        target["document_source_version"] = report.source_id.to_string().into();
        target["extraction_id"] = report.extraction_id.to_string().into();
        target["chunker_version"] = version.into();
        target["budget_identity"] = budget.into();
        target["plan_hash"] = plan.into();
        for field in [
            "legacy_source_version",
            "document_source_version",
            "extraction_id",
            "plan_hash",
        ] {
            assert_ne!(
                target[field], w.target[field],
                "later {field} was not distinct"
            );
        }
        target
    }
    fn preview(w: &World, target: &Value) -> Value {
        let revision = w.revision();
        let mut p = output_json(
            w.preview_command(target, revision, Uuid::now_v7())
                .output()
                .unwrap(),
        );
        p["test_target"] = target.clone();
        p["test_revision"] = revision.into();
        p
    }
    fn ordinary_scope(raw: &Connection, revision: i64) {
        raw.pragma_update(None, "foreign_keys", "ON").unwrap();
        raw.create_scalar_function(
            "m6_write_revision",
            0,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS,
            move |_| Ok(revision),
        )
        .unwrap();
        // Private administrative scopes are absent; SQL availability guards still need
        // their registered functions, which deny every authority-bearing request.
        for name in [
            "s3b_availability_valid",
            "s3c_availability_valid",
            "s3d_availability_valid",
        ] {
            raw.create_scalar_function(
                name,
                3,
                FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS,
                |_| Ok(false),
            )
            .unwrap();
        }
        raw.execute_batch("BEGIN IMMEDIATE").unwrap();
    }
    let w = World::new();
    let original_plan = w.preview();
    let original = w.apply(&original_plan);
    let (old_anchor,old_content): (String,String) = w
        .raw()
        .query_row(
            "SELECT anchor,content FROM doc_staged_entries WHERE stage_id=?1 ORDER BY ordinal LIMIT 1",
            [original["stage_id"].as_str().unwrap()],
            |r| Ok((r.get(0)?,r.get(1)?)),
        )
        .unwrap();
    let target = later_target(&w);
    let before = w.preserved();
    let r = w.revision();
    let later = w.apply(&preview(&w, &target));
    assert_ne!(later["stage_id"], original["stage_id"]);
    assert_eq!(w.count(SETS), 2);
    assert_eq!(w.count(RECEIPTS), 2);
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.preserved(), before);
    assert_eq!(inspect(&w, &later)["current_integrity"], "verified");
    assert_eq!(w.apply(&original_plan), original);
    let store = open_existing(&w.db).unwrap();
    let anchor = old_anchor.parse().unwrap();
    assert_eq!(
        store
            .resolve_document_anchor(&anchor)
            .unwrap()
            .unwrap()
            .text
            .as_deref(),
        Some(old_content.as_str())
    );
    drop(store);
    let r = w.revision();
    for recursive in [false, true] {
        let raw = w.raw();
        raw.pragma_update(None, "recursive_triggers", recursive)
            .unwrap();
        ordinary_scope(&raw, r + 1);
        raw.execute("INSERT INTO m6_revisions VALUES(?1,0,'write')", [r + 1])
            .unwrap();
        for (table, column, key) in [
            ("sources", "id", "legacy_source_version"),
            ("sources", "id", "document_source_version"),
            ("doc_extractions", "id", "extraction_id"),
            ("doc_chunk_sets", "extraction_id", "extraction_id"),
            ("doc_chunks", "extraction_id", "extraction_id"),
            ("doc_nodes", "extraction_id", "extraction_id"),
        ] {
            let sql =
                format!("INSERT OR REPLACE INTO {table} SELECT * FROM {table} WHERE {column}=?1");
            assert!(
                raw.execute(&sql, [target[key].as_str().unwrap()]).is_err(),
                "accepted selected-new input substitution: {sql}"
            );
        }
        assert!(raw.execute("INSERT INTO doc_nodes(extraction_id,path,kind,precision) VALUES(?1,'999999','paragraph','none')",[target["extraction_id"].as_str().unwrap()]).is_err());
        assert!(raw
            .execute(
                "UPDATE sources SET content_hash=?2 WHERE id=?1",
                rusqlite::params![
                    target["legacy_source_version"].as_str().unwrap(),
                    "0".repeat(64)
                ]
            )
            .is_err());
        raw.execute_batch("ROLLBACK").unwrap();
    }
    assert_eq!(w.revision(), r);
    assert_eq!(w.count(SETS), 2);

    // Deliberate same-account corruption of a newly selected, not-yet-sealed Fs.
    // Ordinary ingestion cannot do this. No private administrative permit or trigger removal is used.
    // Retaining R avoids mistaking a stale-revision rejection for content-hash verification.
    let corrupt = World::new();
    let target = later_target(&corrupt);
    let plan = preview(&corrupt, &target);
    let r = corrupt.revision();
    let raw = corrupt.raw();
    raw.pragma_update(None, "recursive_triggers", false)
        .unwrap();
    ordinary_scope(&raw, r);
    let columns = raw
        .prepare("PRAGMA table_info(sources)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    let expr = columns
        .iter()
        .map(|c| {
            if c == "content_hash" {
                "?2".into()
            } else {
                format!("\"{c}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("INSERT OR REPLACE INTO sources SELECT {expr} FROM sources WHERE id=?1");
    assert_eq!(
        raw.execute(
            &sql,
            rusqlite::params![
                target["legacy_source_version"].as_str().unwrap(),
                "0".repeat(64)
            ]
        )
        .unwrap(),
        1
    );
    raw.execute_batch("COMMIT").unwrap();
    drop(raw);
    assert_eq!(corrupt.revision(), r);
    let error = refused(corrupt.apply_command(&plan).output().unwrap());
    assert!(
        !error.contains("stale expected"),
        "only stale revision was checked: {error}"
    );
    assert!(
        error.contains("hash") || error.contains("source bytes"),
        "unexpected refusal: {error}"
    );
    corrupt.assert_unstaged(r);
}
