//! M7.3b.1p: the structural-parse resource policy, end to end.
//!
//! A source over a budget keeps its text and gets a skip row (status `failed`, a policy-bearing
//! identity); the skip is idempotent, re-evaluated when the policy or the parser changes, never
//! hides or replaces an existing derivation by itself, and every reader agrees on which
//! derivation of an input is current.

#![allow(clippy::field_reassign_with_default, dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use mindspace::core::{
    CodeDerivationDraft, CodeEntityDraft, CodeEntityKind, CodeInputDraft, CodeLanguage,
    CodeObservation, CodeParseStatus, CodeRange, CodeSearchQuery, Uuid,
};
use mindspace::ingest::code::{self, ParsePolicy};
use mindspace::ingest::confined::Anchor;
use mindspace::ingest::deadline::Deadline;
use mindspace::ingest::extract::{persist, StoreContext};
use mindspace::ingest::outline::PathSelection;
use mindspace::ingest::stream::{
    consume, stream_confined_git, stream_directory, FrameWriter, Kind, ProducerLimits, StreamLimits,
};
use mindspace::ingest::GitScope;
use mindspace::mcp::McpConfig;
use mindspace::storage::Store;
use mindspace::{initialize, open_existing, SqliteStore};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

struct Db {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

fn db() -> Db {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mind.db");
    initialize(&path).unwrap();
    open_existing(&path).unwrap().create_space("p", "").unwrap();
    Db { _dir: dir, path }
}

impl Db {
    fn store(&self) -> SqliteStore {
        open_existing(&self.path).unwrap()
    }
    fn conn(&self) -> rusqlite::Connection {
        rusqlite::Connection::open_with_flags(
            &self.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }
    fn rows(&self) -> BTreeMap<String, i64> {
        let c = self.conn();
        let tables: Vec<String> = c
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect();
        tables
            .into_iter()
            .map(|t| {
                let n: i64 = c
                    .query_row(&format!("SELECT COUNT(*) FROM \"{t}\""), [], |r| r.get(0))
                    .unwrap();
                (t, n)
            })
            .collect()
    }
    /// (extractor_version, status, error_count, entities) of every derivation, oldest first.
    fn derivations(&self) -> Vec<(String, String, i64, i64)> {
        self.conn()
            .prepare("SELECT d.extractor_version,d.status,d.error_count,(SELECT COUNT(*) FROM code_entities e WHERE e.derivation_id=d.id) FROM code_derivations d ORDER BY d.created_at,d.id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .flatten()
            .collect()
    }
}

fn policy(source: u64, depth: u32, output: u64) -> ProducerLimits {
    ProducerLimits {
        parse_policy: ParsePolicy {
            max_source_bytes: source,
            max_entity_depth: depth,
            max_output_bytes: output,
        },
        ..Default::default()
    }
}

struct Fixture {
    root: PathBuf,
    cfg: McpConfig,
}

fn fixture(root: &Path) -> Fixture {
    let root = std::fs::canonicalize(root).unwrap();
    let cfg = McpConfig::new(&[&root]).unwrap();
    Fixture { root, cfg }
}

fn anchor(f: &Fixture) -> Anchor {
    Anchor::open(&f.root, Some(&f.root), &f.cfg, true).unwrap()
}

fn stream_dir(f: &Fixture, store: &SqliteStore, limits: &ProducerLimits) -> (Vec<u8>, Uuid) {
    let op = Uuid::now_v7();
    let ids = vec![store.get_space("p").unwrap().id];
    let mut out = Vec::new();
    {
        let mut w = FrameWriter::new(&mut out, op, None);
        stream_directory(
            &StoreContext(store),
            &anchor(f),
            &f.cfg,
            &ids,
            &Deadline::none(),
            limits,
            &mut w,
        )
        .unwrap();
        w.finish().unwrap();
    }
    (out, op)
}

fn stream_git(f: &Fixture, store: &SqliteStore, limits: &ProducerLimits) -> (Vec<u8>, Uuid) {
    let op = Uuid::now_v7();
    let mut out = Vec::new();
    {
        let mut w = FrameWriter::new(&mut out, op, None);
        stream_confined_git(
            &StoreContext(store),
            &anchor(f),
            &f.cfg,
            &PathSelection::all(),
            GitScope::Head,
            &["p"],
            &Deadline::none(),
            limits,
            &mut w,
        )
        .unwrap();
        w.finish().unwrap();
    }
    (out, op)
}

/// Streams a directory ingest and persists it; returns the code report.
fn ingest_dir(
    f: &Fixture,
    db: &Db,
    limits: &ProducerLimits,
) -> mindspace::core::CodeObservationReport {
    let store = db.store();
    let (bytes, op) = stream_dir(f, &store, limits);
    let (ex, _) = consume(
        &bytes[..],
        op,
        Kind::Files,
        Some(&f.root),
        &StreamLimits::default(),
        None,
    )
    .unwrap();
    match persist(&store, ex, &Deadline::none()).unwrap() {
        mindspace::core::IngestOutcome::Files(r) => r.code,
        _ => panic!(),
    }
}

fn ingest_git(
    f: &Fixture,
    db: &Db,
    limits: &ProducerLimits,
) -> mindspace::core::GitObservationReport {
    let store = db.store();
    let (bytes, op) = stream_git(f, &store, limits);
    let (ex, _) = consume(
        &bytes[..],
        op,
        Kind::Git,
        None,
        &StreamLimits::default(),
        None,
    )
    .unwrap();
    match persist(&store, ex, &Deadline::none()).unwrap() {
        mindspace::core::IngestOutcome::Git(r) => r,
        _ => panic!(),
    }
}

fn big_source(n: usize) -> String {
    (0..n).map(|i| format!("pub fn big_{i}() {{}}\n")).collect()
}

fn symbols(store: &SqliteStore, text: &str) -> Vec<String> {
    store
        .search_code(&CodeSearchQuery {
            text: text.into(),
            kinds: vec![],
            repository: None,
            path_prefix: None,
            space_ids: None,
            limit: 100,
            all_versions: false,
        })
        .unwrap()
        .into_iter()
        .filter_map(|h| h.name)
        .collect()
}

fn dir_with(big_bytes: usize) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "small.rs", "pub fn small_one() {}\n");
    write(d.path(), "big.rs", &big_source(big_bytes / 22 + 1));
    write(d.path(), "notes.md", "# Notes\n\nplain text\n");
    d
}

const SKIP_1000: &str = "4+skip(p1:source_bytes>1000)";

#[test]
fn an_oversized_source_keeps_its_text_and_gets_one_idempotent_skip_row() {
    let d = dir_with(3000);
    let f = fixture(d.path());
    let db = db();
    let lim = policy(1000, 256, 32 << 20);
    let r = ingest_dir(&f, &db, &lim);
    assert_eq!((r.skipped, r.parsed, r.parse_errors), (1, 1, 0), "{r:?}");
    assert_eq!(r.skip_reasons.get("p1:source_bytes>1000"), Some(&1));
    let ds = db.derivations();
    assert_eq!(ds.len(), 2, "{ds:?}");
    let skip = ds.iter().find(|d| d.0 == SKIP_1000).expect("skip row");
    // status an older binary decodes, no errors, no entities
    assert_eq!((skip.1.as_str(), skip.2, skip.3), ("failed", 0, 0));
    assert!(CodeParseStatus::parse(&skip.1).is_some());
    assert!(ds
        .iter()
        .any(|d| d.0 == "4" && d.1 == "complete" && d.3 == 1));
    // the text of the skipped file is stored
    let len: i64 = db
        .conn()
        .query_row(
            "SELECT length(content) FROM sources WHERE location LIKE '%big.rs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(len > 1000);
    assert_eq!(symbols(&db.store(), "big_"), Vec::<String>::new());
    assert_eq!(
        symbols(&db.store(), "small_one"),
        vec!["small_one".to_string()]
    );
    // unchanged policy and content: zero new rows, still reported
    let before = db.rows();
    let again = ingest_dir(&f, &db, &lim);
    assert_eq!((again.skipped, again.parsed), (1, 0), "{again:?}");
    let after = db.rows();
    for t in ["code_derivations", "code_inputs", "code_entities"] {
        assert_eq!(before[t], after[t], "{t}");
    }
}

#[test]
fn raising_the_limit_parses_the_file_and_the_complete_row_becomes_effective() {
    let d = dir_with(3000);
    let f = fixture(d.path());
    let db = db();
    ingest_dir(&f, &db, &policy(1000, 256, 32 << 20));
    let r = ingest_dir(&f, &db, &policy(100_000, 256, 32 << 20));
    assert_eq!((r.skipped, r.parsed), (0, 1), "{r:?}");
    assert!(symbols(&db.store(), "big_").len() > 10);
    let sd = db.store().code_structure("big.rs").unwrap_or_default();
    let by_path = db
        .store()
        .code_structure(&format!("{}/big.rs", f.root.display()))
        .unwrap();
    let items: Vec<_> = sd.into_iter().chain(by_path).collect();
    let big: Vec<_> = items
        .iter()
        .filter(|x| x.entities.len() > 10 || x.skip_reason.is_some())
        .collect();
    assert_eq!(big.iter().filter(|x| x.effective).count(), 1);
    let eff = big.iter().find(|x| x.effective).unwrap();
    assert!(eff.skip_reason.is_none() && eff.entities.len() > 10);
    let old = big.iter().find(|x| !x.effective).unwrap();
    assert_eq!(old.skip_reason.as_deref(), Some("p1:source_bytes>1000"));
}

#[test]
fn lowering_the_limit_neither_removes_nor_re_derives_existing_structure() {
    let d = dir_with(3000);
    let f = fixture(d.path());
    let db = db();
    ingest_dir(&f, &db, &policy(100_000, 256, 32 << 20));
    let before = db.rows();
    let r = ingest_dir(&f, &db, &policy(1000, 256, 32 << 20));
    assert_eq!(r.skipped, 0, "{r:?}");
    assert_eq!(before["code_derivations"], db.rows()["code_derivations"]);
    assert!(symbols(&db.store(), "big_").len() > 10);
}

#[test]
fn a_changed_limit_or_policy_revision_re_evaluates_a_skip() {
    let d = dir_with(3000);
    let f = fixture(d.path());
    let db = db();
    ingest_dir(&f, &db, &policy(1000, 256, 32 << 20));
    let r = ingest_dir(&f, &db, &policy(900, 256, 32 << 20));
    assert_eq!((r.skipped, r.parsed), (1, 0), "{r:?}");
    let versions: Vec<String> = db.derivations().into_iter().map(|d| d.0).collect();
    assert!(versions.contains(&SKIP_1000.to_string()));
    assert!(versions.contains(&"4+skip(p1:source_bytes>900)".to_string()));
    // a different revision is a different identity: simulate an older revision's row
    let store = db.store();
    let content = std::fs::read_to_string(f.root.join("big.rs")).unwrap();
    let hash = mindspace::core::sha256_hex(content.as_bytes());
    let mut id = code::identity();
    id.extractor_version = "3+skip(p0:source_bytes>900)".into();
    store
        .record_code_observation(CodeObservation {
            inputs: vec![CodeInputDraft {
                language: CodeLanguage::Rust,
                content_hash: hash,
                content: String::new(),
                repository_id: None,
                blob_oid: None,
                contexts: vec![],
                derivation: CodeDerivationDraft {
                    identity: id,
                    status: CodeParseStatus::Failed,
                    error_count: 0,
                    entities: vec![],
                    imports: vec![],
                    relationships: vec![],
                },
            }],
        })
        .unwrap();
    // p0 is not what the policy would write now: the current revision's row still satisfies it
    let n = db.derivations().len();
    ingest_dir(&f, &db, &policy(900, 256, 32 << 20));
    assert_eq!(db.derivations().len(), n);
}

#[test]
fn a_parser_version_change_re_evaluates_and_a_new_skip_hides_old_entities_from_readers() {
    let d = dir_with(3000);
    let f = fixture(d.path());
    let db = db();
    let store = db.store();
    // structure derived by an older parser version
    let content = std::fs::read_to_string(f.root.join("big.rs")).unwrap();
    let hash = mindspace::core::sha256_hex(content.as_bytes());
    let mut old = code::identity();
    old.extractor_version = "2".into();
    let range = CodeRange {
        start_byte: 0,
        end_byte: 5,
        start_line: 1,
        start_column: 1,
        end_line: 1,
        end_column: 6,
    };
    store
        .record_code_observation(CodeObservation {
            inputs: vec![CodeInputDraft {
                language: CodeLanguage::Rust,
                content_hash: hash,
                content: String::new(),
                repository_id: None,
                blob_oid: None,
                contexts: vec![mindspace::core::CodeContextDraft {
                    path: format!("{}/big.rs", f.root.display()),
                    commit_oid: None,
                }],
                derivation: CodeDerivationDraft {
                    identity: old,
                    status: CodeParseStatus::Complete,
                    error_count: 0,
                    entities: vec![CodeEntityDraft {
                        kind: CodeEntityKind::Function,
                        name: Some("old_only_symbol".into()),
                        qualified_name: Some("old_only_symbol".into()),
                        syntax_path: "function:old_only_symbol@0".into(),
                        parent_syntax_path: None,
                        range,
                        signature: Some("fn old_only_symbol()".into()),
                        visibility: None,
                        is_async: false,
                    }],
                    imports: vec![],
                    relationships: vec![],
                },
            }],
        })
        .unwrap();
    // wire the input to the ingested path and ingest under the current parser and a small limit
    let r = ingest_dir(&f, &db, &policy(1000, 256, 32 << 20));
    assert_eq!(
        r.skipped, 1,
        "the old-version row does not satisfy the current identity: {r:?}"
    );
    let store = db.store();
    assert!(
        symbols(&store, "old_only_symbol").is_empty(),
        "old entities must not be current"
    );
    let items = store
        .code_structure(&format!("{}/big.rs", f.root.display()))
        .unwrap();
    assert_eq!(items.iter().filter(|x| x.effective).count(), 1);
    let eff = items.iter().find(|x| x.effective).unwrap();
    assert_eq!(eff.skip_reason.as_deref(), Some("p1:source_bytes>1000"));
    let sup = items.iter().find(|x| !x.effective).unwrap();
    assert_eq!(sup.identity.extractor_version, "2");
    assert_eq!(
        sup.entities.len(),
        1,
        "history keeps the old entities, marked superseded"
    );
}

#[test]
fn depth_output_and_hostile_shapes_are_skipped_with_their_text_kept() {
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        "deep.rs",
        &format!("{}{}\n", "mod m { ".repeat(300), "}".repeat(300)),
    );
    write(
        d.path(),
        "chain.rs",
        &format!("fn f() {{ a{} }}\n", ".b()".repeat(10_000)),
    );
    write(d.path(), "ok.rs", "pub fn ok() {}\n");
    let f = fixture(d.path());
    let db = db();
    let r = ingest_dir(&f, &db, &ProducerLimits::default());
    assert_eq!((r.skipped, r.parsed), (2, 1), "{r:?}");
    assert_eq!(r.skip_reasons.len(), 2, "{:?}", r.skip_reasons);
    assert!(r.skip_reasons.contains_key("p1:entity_depth>256"));
    assert!(r.skip_reasons.contains_key("p1:output_bytes>33554432"));
    let n: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sources WHERE location LIKE '%.rs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 3, "all three texts ingested");
    for d in db.derivations().iter().filter(|d| d.0.contains("skip(")) {
        assert_eq!((d.2, d.3), (0, 0), "no partial entities");
    }
}

#[test]
fn git_ingest_skips_oversized_blobs_keeps_their_text_and_reuses_the_skip_on_fast_forward() {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main", "."]);
    write(d.path(), "small.rs", "pub fn small_one() {}\n");
    write(d.path(), "big.rs", &big_source(120));
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "one"]);
    let f = fixture(d.path());
    let db = db();
    let lim = policy(1000, 256, 32 << 20);
    let r1 = ingest_git(&f, &db, &lim);
    assert_eq!(r1.code.skipped, 1, "{:?}", r1.code);
    let blob_text: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM git_blobs WHERE content IS NOT NULL AND length(content) > 1000",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(blob_text, 1, "the oversized blob's text is kept");
    // fast-forward: a new commit with another oversized file and a changed small one
    write(d.path(), "big2.rs", &big_source(130));
    write(d.path(), "small.rs", "pub fn small_two() {}\n");
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "two"]);
    let before = db.derivations().len();
    let r2 = ingest_git(&f, &db, &lim);
    assert!(r2
        .events
        .iter()
        .any(|e| format!("{:?}", e.kind) == "FastForward"));
    assert_eq!(
        r2.code.skipped, 2,
        "the old skip is reused, the new one added: {:?}",
        r2.code
    );
    assert_eq!(
        db.derivations().len(),
        before + 2,
        "one new skip row and one new parse"
    );
    assert_eq!(
        symbols(&db.store(), "small_two"),
        vec!["small_two".to_string()]
    );
    // a third run changes nothing
    let rows = db.rows();
    let r3 = ingest_git(&f, &db, &lim);
    assert_eq!(r3.code.skipped, 2);
    assert_eq!(rows["code_derivations"], db.rows()["code_derivations"]);
}

#[test]
fn an_interrupted_extraction_stores_nothing_and_the_retry_records_one_skip() {
    let d = dir_with(3000);
    let f = fixture(d.path());
    let db = db();
    let lim = policy(1000, 256, 32 << 20);
    let store = db.store();
    let before = db.rows();
    let (bytes, op) = stream_dir(&f, &store, &lim);
    for cut in [bytes.len() / 3, bytes.len() / 2, bytes.len() - 3] {
        assert!(consume(
            &bytes[..cut],
            op,
            Kind::Files,
            Some(&f.root),
            &StreamLimits::default(),
            None
        )
        .is_err());
    }
    assert_eq!(db.rows(), before, "nothing reached the store");
    let r = ingest_dir(&f, &db, &lim);
    assert_eq!(r.skipped, 1);
    assert_eq!(
        db.derivations()
            .iter()
            .filter(|d| d.0.contains("skip("))
            .count(),
        1
    );
}

#[test]
fn the_in_process_ingest_applies_the_default_policy() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "ok.rs", "pub fn ok() {}\n");
    write(
        d.path(),
        "deep.rs",
        &format!("{}{}\n", "mod m { ".repeat(300), "}".repeat(300)),
    );
    let db = db();
    let store = db.store();
    let out = mindspace::ingest::ingest_path(&store, d.path(), &["p"]).unwrap();
    let mindspace::core::IngestOutcome::Files(r) = out else {
        panic!()
    };
    assert_eq!((r.code.skipped, r.code.parsed), (1, 1), "{:?}", r.code);
}
