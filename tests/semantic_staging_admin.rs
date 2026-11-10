//! Isolated semantic staging protocols. Fake vectors verify storage/recovery, not model correctness.
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
const SETS: &str = "doc_staged_semantic_sets";
const RECEIPTS: &str = "doc_semantic_stage_receipts";

struct World {
    dir: tempfile::TempDir,
    db: PathBuf,
    note: PathBuf,
    target: Value,
    lexical_target: Value,
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
            lexical_target: Value::Null,
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
        w.lexical_target = w.target.clone();
        let r = w.revision();
        let p = w.preview();
        let lexical = w.apply(&p);
        assert_eq!(w.revision(), r + 1);
        w.target = json!({"lexical_stage_id":lexical["stage_id"],"model_id":NOMIC_V1_5.id});
        let discovery = w.preview();
        w.target["pipeline_key"] = discovery["pipeline_key"].clone();
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
        if target.get("lexical_stage_id").is_some() {
            c.args(["ownership", "stage-document-semantic", "--lexical-stage-id"])
                .arg(target["lexical_stage_id"].as_str().unwrap())
                .arg("--model-id")
                .arg(target["model_id"].as_str().unwrap());
            if let Some(key) = target["pipeline_key"].as_str() {
                c.arg("--pipeline-key").arg(key);
            }
            c.arg("--expected-revision")
                .arg(revision.to_string())
                .arg("--json");
            return c;
        }
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
        if let Some(key) = p["pipeline_key"].as_str().map(str::to_owned) {
            p["test_target"]["pipeline_key"] = key.into();
        }
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
        let tables=raw.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT IN ('doc_staged_semantic_sets','doc_staged_vectors','doc_semantic_stage_receipts') AND name NOT IN ('m6_revisions','m6_row_availability') ORDER BY name").unwrap().query_map([],|r|r.get::<_,String>(0)).unwrap().map(Result::unwrap).collect::<Vec<_>>();
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
        assert_eq!(self.count("doc_staged_vectors"), 0);
        assert_eq!(self.count(RECEIPTS), 0);
        assert_eq!(self.revision(), revision);
    }
}

fn clean_command() -> Command {
    // Independent production builds overwrite Cargo's binary path. Preserve the
    // explicitly supplied spawn-gate driver when qualifying an ordinary binary.
    let binary = std::env::var_os("MINDSPACE_S3D2_TEST_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_mind").into());
    let mut c = Command::new(binary);
    for key in [
        "MINDSPACE_DB",
        "MINDSPACE_SEMANTIC_STAGE_PAUSE",
        "MINDSPACE_SEMANTIC_STAGE_GATE_PATH",
        "MINDSPACE_SEMANTIC_STAGE_FAULT",
        "MINDSPACE_SEMANTIC_STAGE_FAKE_ENCODER",
        "MINDSPACE_SEMANTIC_STAGE_TOKENIZER",
        "MINDSPACE_SEMANTIC_STAGE_MODEL_DIR",
        "MINDSPACE_SEMANTIC_ENCODER_PAUSE",
        "MINDSPACE_SEMANTIC_ENCODER_GATE_PATH",
        "MINDSPACE_STORE_GATE_PAUSE_IN_COMMIT",
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
    c.env("MINDSPACE_SEMANTIC_STAGE_FAKE_ENCODER", "unit").env(
        "MINDSPACE_SEMANTIC_STAGE_TOKENIZER",
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nomic-tokenizer.json"),
    );
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
            .env("MINDSPACE_SEMANTIC_STAGE_PAUSE", phase)
            .env("MINDSPACE_SEMANTIC_STAGE_GATE_PATH", &prefix)
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
        panic!("active query runtime must never be used for staged documents")
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
            "ownership_stage_document_semantic",
            "stage_document_semantic",
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

fn inspect_command(w: &World, semantic_id: &str) -> Command {
    let mut c = w.command();
    c.args([
        "ownership",
        "inspect-semantic-stage",
        "--semantic-id",
        semantic_id,
        "--json",
    ]);
    c
}
fn inspect(w: &World, receipt: &Value) -> Value {
    output_json(
        inspect_command(w, receipt["semantic_id"].as_str().unwrap())
            .output()
            .unwrap(),
    )
}
fn vectors(w: &World) -> Vec<(String, Vec<u8>)> {
    w.raw()
        .prepare("SELECT vector_key,embedding FROM doc_staged_vectors ORDER BY ordinal")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn explicit_semantic_stage_is_complete_isolated_and_exact_replay_uses_original_receipt() {
    let w = World::new();
    let r = w.revision();
    let before = w.preserved();
    let search = w.search();
    let p = w.preview();
    w.assert_unstaged(r);
    assert_eq!(p["artifact_availability"], "not_checked");
    assert_eq!(p["fresh_tokens"], "not_checked");
    assert_ne!(
        p["pipeline_key"],
        mindspace::embeddings::manifest::derivation_from_manifest(&NOMIC_V1_5).derivation_key()
    );
    let result = w.apply(&p);
    assert_eq!(result["outcome"], "committed");
    assert_eq!(result["current_integrity"], "not_checked");
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.count(SETS), 1);
    assert_eq!(w.count(RECEIPTS), 1);
    let raw = w.raw();
    let expected: i64 = raw
        .query_row(
            "SELECT entry_count FROM doc_staged_lexical_sets WHERE stage_id=?1",
            [w.target["lexical_stage_id"].as_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(w.count("doc_staged_vectors"), expected);
    assert_eq!(raw.query_row("SELECT COUNT(*) FROM doc_staged_vectors WHERE dimension<>768 OR length(embedding)<>3072 OR token_count NOT BETWEEN 1 AND 2048",[],|row|row.get::<_,i64>(0)).unwrap(),0);
    assert_eq!(
        raw.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
    drop(raw);
    assert_eq!(w.apply(&p), result);
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.preserved(), before);
    assert_eq!(w.search(), search);
    let current = inspect(&w, &result);
    assert_eq!(current["canonical_integrity"], "verified");
    assert_eq!(current["vector_integrity"], "verified");
    for field in [
        "fresh_tokens",
        "lexical_fts_integrity",
        "encoder_availability",
        "original_bytes_status",
    ] {
        assert_eq!(current[field], "not_checked", "{field}");
    }
    assert_eq!(w.revision(), r + 1);
}

#[test]
fn pure_preview_needs_no_tokenizer_weights_or_inference() {
    let w = World::new();
    let r = w.revision();
    let mut c = w.preview_command(&w.target, r, Uuid::now_v7());
    c.env(
        "MINDSPACE_SEMANTIC_STAGE_TOKENIZER",
        w.dir.path().join("missing.json"),
    )
    .env(
        "MINDSPACE_SEMANTIC_STAGE_MODEL_DIR",
        w.dir.path().join("absent"),
    )
    .env("MINDSPACE_SEMANTIC_STAGE_FAKE_ENCODER", "fail:1");
    let p = output_json(c.output().unwrap());
    assert_eq!(p["artifact_availability"], "not_checked");
    assert_eq!(p["fresh_tokens"], "not_checked");
    let mut omitted = w.target.clone();
    omitted.as_object_mut().unwrap().remove("pipeline_key");
    let discovered = output_json(
        w.preview_command(&omitted, r, Uuid::now_v7())
            .output()
            .unwrap(),
    );
    assert_eq!(discovered["pipeline_key"], w.target["pipeline_key"]);
    let mut unbound = w.preview();
    unbound["test_target"]
        .as_object_mut()
        .unwrap()
        .remove("pipeline_key");
    let error = refused(w.apply_command(&unbound).output().unwrap());
    assert!(error.contains("--pipeline-key"), "{error}");
    w.assert_unstaged(r);
}

#[test]
fn all_explicit_input_parameters_and_confirmation_are_bound() {
    let w = World::new();
    let p = w.preview();
    let r = w.revision();
    for committed in [false, true] {
        if committed {
            w.apply(&p);
        }
        for key in ["lexical_stage_id", "model_id", "pipeline_key"] {
            let mut changed = p.clone();
            changed["test_target"][key] = if key == "model_id" {
                "another-model".into()
            } else {
                "0".repeat(64)
            }
            .into();
            refused(w.apply_command(&changed).output().unwrap());
        }
        let mut changed = p.clone();
        changed["test_revision"] = (r + 1).into();
        refused(w.apply_command(&changed).output().unwrap());
        let mut changed = p.clone();
        changed["digest"] = "0".repeat(64).into();
        refused(w.apply_command(&changed).output().unwrap());
    }
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.count(RECEIPTS), 1);
}

#[test]
fn different_operation_same_artifact_does_not_infer_or_create_another_receipt() {
    let w = World::new();
    let first = w.apply(&w.preview());
    let r = w.revision();
    let rows = vectors(&w);
    let p = w.preview();
    let second = output_json(
        w.apply_command(&p)
            .env(
                "MINDSPACE_SEMANTIC_STAGE_TOKENIZER",
                w.dir.path().join("missing.json"),
            )
            .env("MINDSPACE_SEMANTIC_STAGE_FAKE_ENCODER", "fail:1")
            .output()
            .unwrap(),
    );
    assert_eq!(second["outcome"], "existing_artifact");
    assert_eq!(second["semantic_id"], first["semantic_id"]);
    assert_eq!(second["current_integrity"], "verified");
    assert!(second["committed_revision"].is_null());
    assert!(second["operation_id"].is_null());
    assert_eq!(w.revision(), r);
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(vectors(&w), rows);
}

#[test]
fn committed_nonce_cannot_be_rehashed_into_changed_input_intent() {
    let w = World::new();
    let original = w.preview();
    w.apply(&original);
    let r = w.revision();
    let nonce = Uuid::parse_str(
        original["operation_id"]
            .as_str()
            .unwrap()
            .split(':')
            .nth(1)
            .unwrap(),
    )
    .unwrap();
    let mut p = output_json(w.preview_command(&w.target, r, nonce).output().unwrap());
    assert_ne!(p["operation_id"], original["operation_id"]);
    p["test_target"] = w.target.clone();
    p["test_revision"] = r.into();
    refused(w.apply_command(&p).output().unwrap());
    assert_eq!(w.revision(), r);
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.apply(&original)["committed_revision"], r);
}

#[test]
fn six_identical_processes_reuse_one_winning_result_despite_different_valid_floats() {
    let w = World::new();
    let p = w.preview();
    let r = w.revision();
    let children = (0..6)
        .map(|n| {
            w.apply_command(&p)
                .env(
                    "MINDSPACE_SEMANTIC_STAGE_FAKE_ENCODER",
                    if n % 2 == 0 { "unit" } else { "alternate" },
                )
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    let results = children
        .into_iter()
        .map(|c| output_json(c.wait_with_output().unwrap()))
        .collect::<Vec<_>>();
    for result in &results {
        assert_eq!(result, &results[0]);
    }
    assert_eq!(w.count(SETS), 1);
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.revision(), r + 1);
    let values = vectors(&w);
    assert!(!values.is_empty());
    for (_, blob) in values {
        let v = mindspace::core::decode_f32_le(&blob, Some(768)).unwrap();
        assert!((v[0] == 1. && v[1] == 0.) || (v[0] == 0. && v[1] == 1.));
    }
}

#[test]
fn competing_operation_ids_cannot_replace_the_committed_artifact() {
    let w = World::new();
    let a = w.preview();
    let b = w.preview();
    assert_ne!(a["operation_id"], b["operation_id"]);
    assert_eq!(a["semantic_id"], b["semantic_id"]);
    let r = w.revision();
    let children = (0..6)
        .map(|n| {
            w.apply_command(if n % 2 == 0 { &a } else { &b })
                .env(
                    "MINDSPACE_SEMANTIC_STAGE_FAKE_ENCODER",
                    if n % 2 == 0 { "unit" } else { "alternate" },
                )
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    let results = children
        .into_iter()
        .map(|c| output_json(c.wait_with_output().unwrap()))
        .collect::<Vec<_>>();
    assert!(results.iter().any(|r| r["outcome"] == "committed"));
    for result in results {
        assert_eq!(result["semantic_id"], a["semantic_id"]);
        assert!(result["outcome"] == "committed" || result["outcome"] == "existing_artifact");
    }
    assert_eq!(w.count(SETS), 1);
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.revision(), r + 1);
}

#[test]
fn paused_old_writer_returns_winners_receipt_without_comparing_its_floats() {
    let w = World::new();
    let p = w.preview();
    let r = w.revision();
    let old = Paused::start(&w, &p, "before_final");
    let winner = output_json(
        w.apply_command(&p)
            .env("MINDSPACE_SEMANTIC_STAGE_FAKE_ENCODER", "alternate")
            .output()
            .unwrap(),
    );
    assert_eq!(output_json(old.release()), winner);
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.count(RECEIPTS), 1);
}

#[test]
fn local_forward_failure_can_recover_a_concurrent_durable_winner() {
    let w = World::new();
    let p = w.preview();
    let prefix = w.dir.path().join("failed-forward");
    let child = w
        .apply_command(&p)
        .env("MINDSPACE_SEMANTIC_STAGE_FAKE_ENCODER", "fail:1")
        .env("MINDSPACE_SEMANTIC_STAGE_PAUSE", "before_inference")
        .env("MINDSPACE_SEMANTIC_STAGE_GATE_PATH", &prefix)
        .spawn()
        .unwrap();
    let pending = Paused {
        child: Some(child),
        prefix,
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !pending.marker("reached").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let winner = w.apply(&p);
    assert_eq!(output_json(pending.release()), winner);
    assert_eq!(w.count(RECEIPTS), 1);
}

#[test]
fn source_change_after_inference_refuses_stale_intent_without_rebasing() {
    let w = World::new();
    let p = w.preview();
    let old = Paused::start(&w, &p, "after_inference");
    std::fs::write(
        &w.note,
        "# Later\n\nChanged source knowledge e\u{301} 漢字.\n",
    )
    .unwrap();
    let store = open_existing(&w.db).unwrap();
    ingest::ingest_path(&store, &w.note, &["p"]).unwrap();
    document::ingest_document_file(&store, &w.note, &Default::default()).unwrap();
    drop(store);
    let r = w.revision();
    refused(old.release());
    w.assert_unstaged(r);
}

#[test]
fn partial_inference_vector_and_receipt_failures_leave_no_partial_set() {
    for failure in ["forward", "vector:1", "receipt"] {
        let w =
            World::with_text(&"# Many\n\nKnowledge sections preserve every byte.\n\n".repeat(100));
        let p = w.preview();
        let r = w.revision();
        let before = w.preserved();
        let mut command = w.apply_command(&p);
        if failure == "forward" {
            command.env("MINDSPACE_SEMANTIC_STAGE_FAKE_ENCODER", "fail:2");
        } else {
            command.env("MINDSPACE_SEMANTIC_STAGE_FAULT", failure);
        }
        let error = refused(command.output().unwrap());
        assert!(
            !error.to_lowercase().contains("commit outcome is unknown"),
            "{error}"
        );
        w.assert_unstaged(r);
        assert_eq!(w.preserved(), before);
    }
}

#[test]
fn process_death_never_leaves_partial_semantic_state_and_retry_recovers() {
    for phase in [
        "before_admission",
        "after_admission",
        "before_inference",
        "after_inference",
        "before_final",
        "after_final",
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
        assert_eq!(w.apply(&p)["committed_revision"], r + 1);
    }
}

#[test]
fn process_death_after_commit_is_recovered_from_the_original_receipt() {
    let w = World::new();
    let p = w.preview();
    let r = w.revision();
    let pending = Paused::start(&w, &p, "after_commit");
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.revision(), r + 1);
    let out = pending.kill();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert_eq!(w.apply(&p)["committed_revision"], r + 1);
    assert_eq!(w.count(RECEIPTS), 1);
}

#[test]
fn veto_is_known_noncommit_but_lost_ack_has_one_durable_effect() {
    let w = World::new();
    let p = w.preview();
    let r = w.revision();
    let error = refused(
        w.apply_command(&p)
            .env("MINDSPACE_SEMANTIC_STAGE_FAULT", "veto")
            .output()
            .unwrap(),
    );
    assert!(!error.to_lowercase().contains("unknown"), "{error}");
    w.assert_unstaged(r);
    let error = refused(
        w.apply_command(&p)
            .env("MINDSPACE_SEMANTIC_STAGE_FAULT", "lost_ack")
            .output()
            .unwrap(),
    );
    assert!(error.to_lowercase().contains("unknown"), "{error}");
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.revision(), r + 1);
    let winner = w.apply(&p);
    assert_eq!(winner["committed_revision"], r + 1);
    assert_eq!(w.apply(&p), winner);
    assert_eq!(w.revision(), r + 1);
}

#[test]
fn live_writer_timeout_does_not_prove_an_unknown_attempt_rolled_back() {
    let w = World::new();
    let p = w.preview();
    let pending = Paused::start(&w, &p, "after_write");
    let error = refused(w.apply_command(&p).output().unwrap()).to_lowercase();
    assert!(
        error.contains("busy") || error.contains("unknown") || error.contains("locked"),
        "{error}"
    );
    assert!(!error.contains("rolled back"));
    let result = output_json(pending.release());
    assert_eq!(w.apply(&p), result);
}

#[test]
fn inspection_waits_for_staging_commit_or_rollback_without_partial_readiness() {
    for commit in [true, false] {
        let w = World::new();
        let p = w.preview();
        let r = w.revision();
        let writer = Paused::start(&w, &p, "after_write");
        let child = inspect_command(&w, p["semantic_id"].as_str().unwrap())
            .spawn()
            .unwrap();
        let mut inspector = Paused {
            child: Some(child),
            prefix: w.dir.path().join("inspection"),
        };
        let fd_dir = PathBuf::from(format!(
            "/proc/{}/fd",
            inspector.child.as_ref().unwrap().id()
        ));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                inspector
                    .child
                    .as_mut()
                    .unwrap()
                    .try_wait()
                    .unwrap()
                    .is_none(),
                "inspection returned before writer settled"
            );
            if std::fs::read_dir(&fd_dir).is_ok_and(|fds| {
                fds.flatten()
                    .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|p| p == w.db))
            }) {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(100));
        assert!(inspector
            .child
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none());
        w.assert_unstaged(r);
        if commit {
            output_json(writer.release());
        } else {
            writer.kill();
        }
        let result = inspector.child.take().unwrap().wait_with_output().unwrap();
        if commit {
            let status = output_json(result);
            assert_eq!(status["vector_integrity"], "verified");
            assert_eq!(w.revision(), r + 1);
        } else {
            refused(result);
            w.assert_unstaged(r);
        }
    }
}

#[test]
fn all_active_modes_scores_temporal_queries_and_old_rows_are_unchanged() {
    let w = World::new();
    let other = w.dir.path().join("other.md");
    std::fs::write(
        &other,
        "# Other\n\nKnowledge knowledge with a competing phrase.\n",
    )
    .unwrap();
    let store = open_existing(&w.db).unwrap();
    ingest::ingest_path(&store, &other, &["p"]).unwrap();
    drop(store);
    let cap = capability(&w);
    let cutoff = Ts::now();
    let results = all_retrieval(&w, &cap, cutoff);
    let old = w.preserved();
    let store = open_existing(&w.db).unwrap();
    let key = w.target["pipeline_key"].as_str().unwrap();
    let active_key =
        mindspace::embeddings::manifest::derivation_from_manifest(&FAKE_ENTRY).derivation_key();
    let stage_index_before = store.embedding_index_status(key).unwrap();
    let active_index_before = store.embedding_index_status(&active_key).unwrap();
    let backfill_before = store
        .list_assertions_needing_embedding(key, None, 100)
        .unwrap()
        .iter()
        .map(|a| a.assertion_id)
        .collect::<Vec<_>>();
    drop(store);
    let receipt = w.apply(&w.preview());
    assert_eq!(all_retrieval(&w, &cap, cutoff), results);
    assert_eq!(w.preserved(), old);
    let saved_vectors = vectors(&w);
    let store = open_existing(&w.db).unwrap();
    assert_eq!(
        store.embedding_index_status(key).unwrap(),
        stage_index_before
    );
    assert_eq!(
        store.embedding_index_status(&active_key).unwrap(),
        active_index_before
    );
    assert_eq!(
        store
            .list_assertions_needing_embedding(key, None, 100)
            .unwrap()
            .iter()
            .map(|a| a.assertion_id)
            .collect::<Vec<_>>(),
        backfill_before
    );
    assert_eq!(
        store
            .clear_embeddings(w.target["pipeline_key"].as_str().unwrap())
            .unwrap(),
        0
    );
    assert_eq!(vectors(&w), saved_vectors);
    assert_eq!(inspect(&w, &receipt)["vector_integrity"], "verified");
    assert_eq!(all_retrieval(&w, &cap, cutoff), results);
}

#[test]
fn untrusted_mcp_cannot_stage_or_publish_and_default_tools_are_unchanged() {
    let w = World::new();
    let before = mcp_probe(&w.db);
    let rows = w.preserved();
    w.apply(&w.preview());
    assert_eq!(mcp_probe(&w.db), before);
    assert_eq!(w.preserved(), rows);
}

#[test]
fn ordinary_record_context_cannot_forge_semantic_authority_with_sql_pragmas() {
    use rusqlite::functions::FunctionFlags;
    let w = World::new();
    w.apply(&w.preview());
    let old = w.preserved();
    let saved = vectors(&w);
    let r = w.revision();
    for foreign_keys in [false, true] {
        for recursive in [false, true] {
            let raw = w.raw();
            raw.pragma_update(None, "foreign_keys", foreign_keys)
                .unwrap();
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
            for table in [SETS, RECEIPTS, "doc_staged_vectors"] {
                for sql in [
                    format!("UPDATE {table} SET rowid=rowid"),
                    format!("DELETE FROM {table}"),
                    format!("INSERT OR REPLACE INTO {table} SELECT * FROM {table}"),
                ] {
                    let error = raw.execute(&sql, []).expect_err(&format!("accepted {sql}"));
                    assert!(
                        !error.to_string().contains("syntax error"),
                        "{sql}: {error}"
                    );
                }
            }
            for sql in [
                "UPDATE doc_staged_semantic_sets SET state='published'",
                "UPDATE doc_staged_vectors SET ordinal=ordinal+1",
                "UPDATE doc_semantic_stage_receipts SET digest=printf('%064d',0)",
                "UPDATE asset_ownership SET owner='document'",
                "UPDATE asset_ownership SET epoch=2",
                "INSERT INTO doc_staged_vectors SELECT * FROM doc_staged_vectors WHERE 1 ON CONFLICT(semantic_id,ordinal) DO UPDATE SET embedding=excluded.embedding",
                "INSERT INTO doc_semantic_stage_receipts SELECT * FROM doc_semantic_stage_receipts WHERE 1 ON CONFLICT(operation_id) DO UPDATE SET digest=excluded.digest",
                "INSERT OR REPLACE INTO doc_staged_entries SELECT * FROM doc_staged_entries",
                "INSERT OR REPLACE INTO doc_chunks SELECT * FROM doc_chunks",
                "INSERT OR REPLACE INTO sources SELECT * FROM sources",
                "UPDATE sources SET content_hash=printf('%064d',0) WHERE id IN (SELECT document_source_version FROM doc_staged_lexical_sets)",
            ] {
                let error=raw.execute(sql,[]).expect_err(&format!("accepted {sql}"));
                assert!(!error.to_string().contains("syntax error"),"invalid SQL oracle: {sql}: {error}");
            }
            raw.execute_batch("ROLLBACK").unwrap();
        }
    }
    assert_eq!(w.preserved(), old);
    assert_eq!(vectors(&w), saved);
    assert_eq!(w.revision(), r);
}

#[test]
fn canonical_parent_ranges_are_exact_and_survive_source_replacement_or_disappearance() {
    let text=format!("\u{feff}# First\r\n\r\n{}\r\n## Second\r\n\r\n```rust\r\nlet x=\"👩‍💻 e\u{301} \u{200b}\u{202e}bidi\u{202c}\";\r\n```\r\n", "knowledge 日本語 words ".repeat(300));
    let w = World::with_text(&text);
    let p = w.preview();
    let receipt = w.apply(&p);
    let raw = w.raw();
    let mut stmt=raw.prepare("SELECT v.ordinal,e.text_start,e.text_end,e.content,e.anchor,e.context_json,c.anchor,c.context_json FROM doc_staged_vectors v JOIN doc_staged_entries e ON e.entry_id=v.entry_id JOIN doc_chunks c ON c.id=e.chunk_id WHERE v.semantic_id=?1 ORDER BY v.ordinal").unwrap();
    let rows = stmt
        .query_map([receipt["semantic_id"].as_str().unwrap()], |r| {
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
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    let canonical = text.trim_start_matches('\u{feff}');
    let mut offset = 0;
    let mut reconstructed = String::new();
    let store = open_existing(&w.db).unwrap();
    for (n, (ordinal, start, end, content, anchor, context, original_anchor, original_context)) in
        rows.iter().enumerate()
    {
        assert_eq!(*ordinal, n as i64);
        assert_eq!(*start, offset);
        assert!(canonical.is_char_boundary(*start) && canonical.is_char_boundary(*end));
        assert_eq!(content, &canonical[*start..*end]);
        assert_eq!(anchor, original_anchor);
        assert_eq!(context, original_context);
        assert_eq!(
            store
                .resolve_document_anchor(&anchor.parse().unwrap())
                .unwrap()
                .unwrap()
                .text
                .as_deref(),
            Some(content.as_str())
        );
        reconstructed.push_str(content);
        offset = *end;
    }
    assert_eq!(offset, canonical.len());
    assert_eq!(reconstructed, canonical);
    std::fs::write(&w.note, "# Later\n\nChanged searchable knowledge.\n").unwrap();
    ingest::ingest_path(&store, &w.note, &["p"]).unwrap();
    document::ingest_document_file(&store, &w.note, &Default::default()).unwrap();
    drop(store);
    std::fs::remove_file(&w.note).unwrap();
    let r = w.revision();
    let mut retry = w.apply_command(&p);
    retry
        .env(
            "MINDSPACE_SEMANTIC_STAGE_TOKENIZER",
            w.dir.path().join("missing-tokenizer"),
        )
        .env("MINDSPACE_SEMANTIC_STAGE_FAKE_ENCODER", "fail:1");
    assert_eq!(output_json(retry.output().unwrap()), receipt);
    assert_eq!(w.revision(), r);
    let status = inspect(&w, &receipt);
    assert_eq!(status["canonical_integrity"], "verified");
    assert_eq!(status["vector_integrity"], "verified");
    assert_eq!(status["original_bytes_status"], "not_checked");
}

#[test]
fn missing_empty_foreign_symlink_and_unsafe_databases_are_not_created_or_rebound() {
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
fn missing_corrupt_assets_or_unknown_contract_never_create_an_artifact() {
    let w = World::new();
    let p = w.preview();
    let r = w.revision();
    for tokenizer in [
        w.dir.path().join("missing-tokenizer"),
        w.dir.path().join("corrupt-tokenizer"),
    ] {
        if tokenizer.ends_with("corrupt-tokenizer") {
            std::fs::write(&tokenizer, b"{}").unwrap();
        }
        let error = refused(
            w.apply_command(&p)
                .env("MINDSPACE_SEMANTIC_STAGE_TOKENIZER", tokenizer)
                .output()
                .unwrap(),
        );
        assert!(error.contains("NoCommitThisInvocation"), "{error}");
        w.assert_unstaged(r);
    }
    let model = w.dir.path().join("bad-model");
    std::fs::create_dir(&model).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nomic-tokenizer.json"),
        model.join("tokenizer.json"),
    )
    .unwrap();
    for corrupt in [false, true] {
        if corrupt {
            std::fs::write(model.join("model.safetensors"), b"incorrect weights").unwrap();
        }
        let error = refused(
            w.apply_command(&p)
                .env_remove("MINDSPACE_SEMANTIC_STAGE_FAKE_ENCODER")
                .env("MINDSPACE_SEMANTIC_STAGE_MODEL_DIR", &model)
                .output()
                .unwrap(),
        );
        assert!(error.contains("NoCommitThisInvocation"), "{error}");
        w.assert_unstaged(r);
    }
    for (key, value) in [
        ("lexical_stage_id", "0".repeat(64)),
        ("pipeline_key", "1".repeat(64)),
        ("model_id", "unknown-model".to_owned()),
    ] {
        let mut target = w.target.clone();
        target[key] = value.into();
        refused(
            w.preview_command(&target, r, Uuid::now_v7())
                .output()
                .unwrap(),
        );
        w.assert_unstaged(r);
    }
}

#[test]
fn output_loss_after_committed_effect_is_unknown_but_read_only_status_is_not() {
    let w = World::new();
    let p = w.preview();
    let r = w.revision();
    let mut child = w.apply_command(&p).spawn().unwrap();
    drop(child.stdout.take());
    let error = refused(child.wait_with_output().unwrap());
    assert!(error.to_lowercase().contains("unknown"), "{error}");
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.apply(&p)["committed_revision"], r + 1);
    let p2 = w.preview();
    let mut child = w.apply_command(&p2).spawn().unwrap();
    drop(child.stdout.take());
    let error = refused(child.wait_with_output().unwrap());
    assert!(
        !error.to_lowercase().contains("unknown"),
        "read-only existing artifact misclassified: {error}"
    );
    assert_eq!(w.count(RECEIPTS), 1);
    assert_eq!(w.revision(), r + 1);
}

#[test]
#[ignore = "requires an independently built production binary; explicitly invoked"]
fn production_and_no_embeddings_binaries_preserve_receipts_without_test_hooks() {
    let binary = std::env::var_os("MINDSPACE_S3D2_PRODUCTION_BINARY")
        .expect("independently built production binary");
    let no_embeddings =
        std::env::var("MINDSPACE_S3D2_PRODUCTION_MODE").is_ok_and(|v| v == "no-embeddings");
    let bytes = std::fs::read(&binary).unwrap();
    for marker in [
        "MINDSPACE_SEMANTIC_STAGE_FAKE_ENCODER",
        "MINDSPACE_SEMANTIC_STAGE_TOKENIZER",
        "MINDSPACE_SEMANTIC_STAGE_MODEL_DIR",
        "MINDSPACE_SEMANTIC_STAGE_FAULT",
        "MINDSPACE_SEMANTIC_STAGE_PAUSE",
        "MINDSPACE_SEMANTIC_STAGE_GATE_PATH",
        "MINDSPACE_SEMANTIC_ENCODER_PAUSE",
        "MINDSPACE_SEMANTIC_ENCODER_GATE_PATH",
        "MINDSPACE_STORE_GATE_PAUSE_IN_COMMIT",
    ] {
        assert!(
            !bytes.windows(marker.len()).any(|b| b == marker.as_bytes()),
            "test hook in ordinary binary: {marker}"
        );
    }
    let w = World::new();
    let run = |command: Command| {
        let mut production = Command::new(&binary);
        production
            .args(command.get_args())
            .env_remove("MINDSPACE_DB")
            .env("MINDSPACE_SEMANTIC_STAGE_FAKE_ENCODER", "fail:1")
            .env(
                "MINDSPACE_SEMANTIC_STAGE_TOKENIZER",
                w.dir.path().join("must-ignore-tokenizer"),
            )
            .env(
                "MINDSPACE_SEMANTIC_STAGE_MODEL_DIR",
                w.dir.path().join("must-ignore-model"),
            )
            .env("MINDSPACE_SEMANTIC_STAGE_FAULT", "lost_ack")
            .env("MINDSPACE_SEMANTIC_STAGE_PAUSE", "before_commit")
            .env(
                "MINDSPACE_SEMANTIC_STAGE_GATE_PATH",
                w.dir.path().join("ignored-gate"),
            )
            .stdin(Stdio::null());
        production.output().unwrap()
    };
    let p = w.preview();
    let r = w.revision();
    if no_embeddings {
        let error = refused(run(w.apply_command(&p)));
        assert!(error.contains("embeddings feature"), "{error}");
        w.assert_unstaged(r);
        refused(run(w.preview_command(&w.target, r, Uuid::now_v7())));
    }
    let receipt = if no_embeddings {
        w.apply(&p)
    } else {
        output_json(run(w.apply_command(&p)))
    };
    assert_eq!(receipt["outcome"], "committed");
    assert_eq!(w.revision(), r + 1);
    assert_eq!(output_json(run(w.apply_command(&p))), receipt);
    let status = output_json(run(inspect_command(
        &w,
        receipt["semantic_id"].as_str().unwrap(),
    )));
    assert_eq!(status["vector_integrity"], "verified");
    assert_eq!(status["fresh_tokens"], "not_checked");
    let next = w.preview();
    assert_eq!(
        output_json(run(w.apply_command(&next)))["outcome"],
        "existing_artifact"
    );
    assert_eq!(w.revision(), r + 1);
    assert_eq!(w.count(RECEIPTS), 1);
    assert!(!w.dir.path().join("ignored-gate.reached").exists());
    println!(
        "ordinary production semantic smoke passed (no_embeddings={no_embeddings}): {}",
        Path::new(&binary).display()
    );
}

/// Actual cached official weights, never fixture vectors. Setup and S3 token chunking are excluded.
#[test]
#[ignore = "opt-in real CPU model inference and process measurements; no downloads"]
fn real_model_staging_process_measurements_and_exact_citations() {
    let model =
        std::env::var_os("MINDSPACE_S3_MODEL_DIR").expect("cached official model directory");
    let bytes = std::env::var("MINDSPACE_S3D2_PROCESS_MEASURE_BYTES")
        .ok()
        .map(|v| v.parse::<usize>().unwrap())
        .unwrap_or(1024);
    let runs = std::env::var("MINDSPACE_S3D2_PROCESS_MEASURE_RUNS")
        .ok()
        .map(|v| v.parse::<usize>().unwrap())
        .unwrap_or(1);
    let physical = |w: &World| -> Value {
        let size = |suffix: &str| {
            let mut path = w.db.as_os_str().to_owned();
            path.push(suffix);
            std::fs::metadata(Path::new(&path))
                .map(|m| m.len())
                .unwrap_or(0)
        };
        json!({"database":size(""),"wal":size("-wal"),"shm":size("-shm")})
    };
    for (kind,unit) in [("prose","Knowledge preserves exact canonical citation boundaries across paragraphs.\n\n"),("cjk","知識検索日本語中文記録。\n\n"),("unbroken","uninterruptedcharacters0123456789"),("markdown","# Section\n\nKnowledge **formatting**.\n\n- first item\n- second item\n\n```rust\nlet text=\"😀 e\u{301}\u{200b}\u{202e}bidi\u{202c}\";\n```\n\n")] {
        if std::env::var("MINDSPACE_S3D2_PROCESS_MEASURE_KIND").is_ok_and(|v|v!=kind){continue;}
        for run in 0..runs {
            let mut text=unit.repeat(bytes.div_ceil(unit.len()));let mut end=bytes.min(text.len());while !text.is_char_boundary(end){end-=1;}text.truncate(end);
            let w=World::with_text(&text);let old=w.preserved();let before=physical(&w);let start=Instant::now();let p=w.preview();let preview_ms=start.elapsed().as_secs_f64()*1000.;
            let start=Instant::now();let receipt=output_json(w.apply_command(&p).env_remove("MINDSPACE_SEMANTIC_STAGE_FAKE_ENCODER").env_remove("MINDSPACE_SEMANTIC_STAGE_TOKENIZER").env("MINDSPACE_SEMANTIC_STAGE_MODEL_DIR",&model).output().unwrap());let apply_ms=start.elapsed().as_secs_f64()*1000.;
            let start=Instant::now();let status=inspect(&w,&receipt);let inspect_ms=start.elapsed().as_secs_f64()*1000.;assert_eq!(status["canonical_integrity"],"verified");assert_eq!(status["vector_integrity"],"verified");assert_eq!(w.preserved(),old);
            let raw=w.raw();let (count,vector_bytes,metadata_bytes):(i64,i64,i64)=raw.query_row("SELECT vector_count,vector_bytes,metadata_bytes FROM doc_staged_semantic_sets",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
            for (_,blob) in vectors(&w) {assert_eq!(blob.len(),3072);let vals=blob.as_chunks::<4>().0.iter().map(|v|f32::from_le_bytes(*v)).collect::<Vec<_>>();assert!(vals.iter().all(|v|v.is_finite()));let norm=vals.iter().map(|v|(*v as f64).powi(2)).sum::<f64>().sqrt();assert!((norm-1.).abs()<=1e-4);}
            println!("{}",json!({"measurement":"s3d2.semantic.real-process@1","kind":kind,"run":run+1,"source_bytes":text.len(),"vectors":count,"vector_bytes":vector_bytes,"semantic_metadata_bytes":metadata_bytes,"preview_ms":preview_ms,"apply_total_ms":apply_ms,"inspection_ms":inspect_ms,"physical_before":before,"physical_after":physical(&w),"pipeline_key":p["pipeline_key"],"model_id":p["model_id"]}));
        }
    }
}

#[test]
fn stored_vector_corruption_refuses_current_verification_but_preserves_historical_receipt() {
    let w = World::new();
    let p = w.preview();
    let receipt = w.apply(&p);
    let r = w.revision();
    let raw = w.raw();
    // Deliberate corruption by the trusted database account is outside Model A's authority claim.
    // Restore the full trigger inventory before exercising actual healthy-open inspection/replay.
    let trigger: String = raw
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='m6_s3d2_vector_update'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    raw.execute_batch("BEGIN IMMEDIATE; DROP TRIGGER m6_s3d2_vector_update; UPDATE doc_staged_vectors SET embedding=zeroblob(3072);").unwrap();
    raw.execute_batch(&trigger).unwrap();
    raw.execute_batch("COMMIT").unwrap();
    drop(raw);
    let status = inspect(&w, &receipt);
    assert_eq!(status["historical_verified"], true);
    assert_eq!(status["canonical_integrity"], "verified");
    assert_eq!(status["vector_integrity"], "corrupt");
    assert!(status["error"].is_string());
    assert_eq!(w.apply(&p), receipt);
    assert_eq!(w.revision(), r);
    let other = w.preview();
    refused(w.apply_command(&other).output().unwrap());
    assert_eq!(w.revision(), r);
    assert_eq!(w.count(RECEIPTS), 1);
}
