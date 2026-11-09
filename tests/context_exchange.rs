//! M7.3b.2c: the context exchange between the parent and the isolated directory worker.
//!
//! Evidence: known derivations are reused without parsing, policy skips stay idempotent, a changed
//! policy re-evaluates, results and persisted rows equal the in-process ingest, and every malformed,
//! oversized, replayed or authority-seeking message is refused before anything is persisted.

#![allow(clippy::field_reassign_with_default)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use mindspace::core::Uuid;
use mindspace::ingest::code::ParsePolicy;
use mindspace::ingest::confined::{extract_directory, Anchor};
use mindspace::ingest::deadline::Deadline;
use mindspace::ingest::extract::{persist, Extracted, StoreContext};
use mindspace::mcp::McpConfig;
use mindspace::storage::Store;
use mindspace::worker::client::{extract_directory as isolated, WorkerConfig, WorkerError};
use mindspace::worker::context::{identities_for, MAX_CONTEXT_LINE, MAX_MANIFEST_LINE};
use mindspace::worker::Request;
use mindspace::{initialize, open_existing, SqliteStore};

fn mind() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mind"))
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
    fn space(&self) -> Uuid {
        self.store().get_space("p").unwrap().id
    }
    fn conn(&self) -> rusqlite::Connection {
        rusqlite::Connection::open_with_flags(
            &self.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }
    /// Row counts of every table.
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
                let n = c
                    .query_row(&format!("SELECT COUNT(*) FROM \"{t}\""), [], |r| r.get(0))
                    .unwrap();
                (t, n)
            })
            .collect()
    }
    /// Which derivations exist: status, parser version, entity count; ids and times left out.
    fn derivations(&self) -> Vec<(String, String, i64)> {
        let c = self.conn();
        let mut s = c
            .prepare(
                "SELECT d.status, d.extractor_version, (SELECT COUNT(*) FROM code_entities e WHERE e.derivation_id=d.id) \
                 FROM code_derivations d ORDER BY d.extractor_version, d.status, 3",
            )
            .unwrap();
        let rows = s
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .flatten()
            .collect();
        rows
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
    Anchor::open(&f.root, Some(&f.root), &f.cfg, false).unwrap()
}

fn cfg() -> WorkerConfig {
    let mut c = WorkerConfig::new(mind());
    c.wall_timeout = Duration::from_secs(120);
    c
}

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

fn small_tree() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "docs/guide.md", "# Guide\n\nalpha\n");
    write(
        d.path(),
        "src/lib.rs",
        "pub fn alpha_symbol() {}\npub struct Beta;\n",
    );
    write(
        d.path(),
        "src/more.rs",
        "use crate::alpha_symbol;\nfn gamma() { alpha_symbol(); }\n",
    );
    d
}

/// About 1.9 MiB of one-line array: a parse needs seconds and hundreds of MiB.
fn slow_source() -> String {
    format!("const X: [u8; 1] = [{}];\n", vec!["1"; 950_000].join(","))
}

/// Over the default 2 MiB source budget: skipped without a parse.
fn oversized_source() -> String {
    "pub fn a() {}\n".repeat(160_000)
}

fn run_worker(
    db: &Db,
    f: &Fixture,
    c: &WorkerConfig,
) -> (Extracted, mindspace::worker::client::WorkerStats) {
    let store = db.store();
    isolated(
        c,
        &anchor(f),
        &[db.space()],
        &StoreContext(&store),
        &no_cancel(),
    )
    .unwrap()
}

fn in_process(db: &Db, f: &Fixture) -> Extracted {
    let store = db.store();
    Extracted::Files(
        extract_directory(
            &StoreContext(&store),
            &anchor(f),
            &f.cfg,
            &[db.space()],
            &Deadline::none(),
        )
        .unwrap(),
    )
}

fn complete(db: &Db) -> usize {
    db.derivations()
        .iter()
        .filter(|d| d.0 == "complete")
        .count()
}

// ------------------------------------------------------------- reuse and equivalence

#[test]
fn known_complete_derivations_are_reused_without_reparsing_and_match_the_in_process_ingest() {
    let d = small_tree();
    write(d.path(), "z.rs", &slow_source());
    let f = fixture(d.path());
    let (worker_db, direct_db) = (db(), db());

    // first run: nothing is known, everything is parsed (the slow file takes seconds)
    let t = Instant::now();
    let (first, s1) = run_worker(&worker_db, &f, &cfg());
    let first_took = t.elapsed();
    assert_eq!(s1.known_sent, 0);
    assert_eq!(s1.manifest_entries, 3, "three distinct source blobs");
    persist(&worker_db.store(), first, &Deadline::none()).unwrap();
    let a = in_process(&direct_db, &f);
    persist(&direct_db.store(), a, &Deadline::none()).unwrap();
    assert_eq!(worker_db.rows(), direct_db.rows());
    assert_eq!(worker_db.derivations(), direct_db.derivations());
    assert_eq!(complete(&worker_db), 3);

    // second run: everything is known. A 128 MiB address-space limit would kill the worker if it
    // parsed the slow file (the existing crash test shows exactly that), so success proves that it
    // did not.
    let mut c = cfg();
    c.rlimit_as_mib = Some(128);
    let t = Instant::now();
    let (second, s2) = run_worker(&worker_db, &f, &c);
    let second_took = t.elapsed();
    assert_eq!(
        s2.known_sent, 3,
        "all three blobs known under the current identity"
    );
    let Extracted::Files(files) = &second else {
        panic!()
    };
    assert_eq!(files.code_inputs.len(), 3);
    assert!(
        files
            .code_inputs
            .iter()
            .all(|i| i.derivation.entities.is_empty()
                && i.derivation.status == mindspace::core::CodeParseStatus::Complete),
        "known blobs come back as empty placeholders of the stored derivation"
    );
    assert!(
        second_took < first_took / 2,
        "no reparse: {second_took:?} vs {first_took:?}"
    );
    // the in-process ingest agrees, and persisting either changes nothing
    let b = in_process(&direct_db, &f);
    let before = worker_db.rows();
    persist(&worker_db.store(), second, &Deadline::none()).unwrap();
    persist(&direct_db.store(), b, &Deadline::none()).unwrap();
    assert_eq!(worker_db.derivations(), direct_db.derivations());
    // observations are appended by both in the same way; derivations are not duplicated
    assert_eq!(complete(&worker_db), 3);
    let after = worker_db.rows();
    assert_eq!(before["code_derivations"], after["code_derivations"]);
    assert_eq!(before["code_entities"], after["code_entities"]);
    assert_eq!(worker_db.rows(), direct_db.rows());
}

#[test]
fn known_policy_skips_stay_idempotent_and_match_the_in_process_ingest() {
    let d = small_tree();
    write(d.path(), "huge.rs", &oversized_source());
    let f = fixture(d.path());
    let (worker_db, direct_db) = (db(), db());

    let (first, s1) = run_worker(&worker_db, &f, &cfg());
    assert_eq!(s1.known_sent, 0);
    persist(&worker_db.store(), first, &Deadline::none()).unwrap();
    let a = in_process(&direct_db, &f);
    persist(&direct_db.store(), a, &Deadline::none()).unwrap();
    let skips: Vec<_> = worker_db
        .derivations()
        .into_iter()
        .filter(|d| d.1.contains("+skip("))
        .collect();
    assert_eq!(skips.len(), 1, "{:?}", worker_db.derivations());
    assert_eq!(skips[0].0, "failed");
    assert_eq!(worker_db.derivations(), direct_db.derivations());

    // again: the skip is known under its identity; the worker sends back the skip placeholder
    let (second, s2) = run_worker(&worker_db, &f, &cfg());
    assert_eq!(s2.known_sent, 3, "two complete derivations and one skip");
    let before = worker_db.derivations();
    persist(&worker_db.store(), second, &Deadline::none()).unwrap();
    let b = in_process(&direct_db, &f);
    persist(&direct_db.store(), b, &Deadline::none()).unwrap();
    assert_eq!(worker_db.derivations(), before, "idempotent");
    assert_eq!(worker_db.rows(), direct_db.rows());
}

fn skip_policy(max_source: u64) -> ParsePolicy {
    ParsePolicy {
        max_source_bytes: max_source,
        ..ParsePolicy::default()
    }
}

#[test]
fn a_changed_policy_reevaluates_and_a_lower_budget_never_removes_existing_structure() {
    let d = tempfile::tempdir().unwrap();
    // 40 lines: small, parsed under the default policy, over a 200-byte budget
    write(d.path(), "m.rs", &"pub fn f() {}\n".repeat(40));
    let f = fixture(d.path());
    let w = db();
    let mut strict = cfg();
    strict.producer.parse_policy = skip_policy(200);
    let mut lax = cfg();
    lax.producer.parse_policy = skip_policy(1 << 20);

    // 1. strict budget: skipped
    let (r, s) = run_worker(&w, &f, &strict);
    assert_eq!(s.known_sent, 0);
    persist(&w.store(), r, &Deadline::none()).unwrap();
    assert_eq!(complete(&w), 0);
    assert_eq!(w.derivations().len(), 1);
    assert!(w.derivations()[0].1.contains("source_bytes>200"));

    // 2. the same strict budget again: the skip is known, nothing changes
    let (r, s) = run_worker(&w, &f, &strict);
    assert_eq!(s.known_sent, 1);
    persist(&w.store(), r, &Deadline::none()).unwrap();
    assert_eq!(w.derivations().len(), 1);

    // 3. the budget is raised: the old skip does not apply, the file is parsed
    let (r, s) = run_worker(&w, &f, &lax);
    assert_eq!(s.known_sent, 0, "a skip under another limit is not known");
    let Extracted::Files(files) = &r else {
        panic!()
    };
    assert!(
        !files.code_inputs[0].derivation.entities.is_empty(),
        "re-evaluated: real structure"
    );
    persist(&w.store(), r, &Deadline::none()).unwrap();
    assert_eq!(complete(&w), 1);
    assert_eq!(
        w.derivations().len(),
        2,
        "the skip row is history, not rewritten"
    );

    // 4. lowered again: the complete derivation exists and wins; no reparse, no new row
    let (r, s) = run_worker(&w, &f, &strict);
    assert_eq!(s.known_sent, 1);
    persist(&w.store(), r, &Deadline::none()).unwrap();
    assert_eq!(w.derivations().len(), 2);

    // the in-process ingest (default policy) lands in the same state for the same file
    let direct = db();
    let a = in_process(&direct, &f);
    persist(&direct.store(), a, &Deadline::none()).unwrap();
    assert_eq!(complete(&direct), 1);
}

// ------------------------------------------------ the worker's side, driven by a fake parent

struct Session {
    /// Exit code (`None`: killed by a signal).
    code: Option<i32>,
    stderr: String,
    /// Bytes the worker sent after the manifest (the record stream).
    after_manifest: usize,
    manifest: String,
}

/// Starts the real worker with a root and a channel and plays the parent by hand.
fn drive(
    root: &Path,
    request: impl FnOnce(Uuid, &Path) -> Vec<u8>,
    context: impl FnOnce(Uuid, &str) -> Vec<u8>,
) -> Session {
    let rootf = std::fs::File::open(root).unwrap();
    let (mine, theirs) = UnixStream::pair().unwrap();
    let (r, c) = (rootf.as_raw_fd(), theirs.as_raw_fd());
    let mut cmd = Command::new(mind());
    cmd.arg("extraction-worker")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // SAFETY: async-signal-safe calls only (fcntl, dup2, close).
    unsafe {
        cmd.pre_exec(move || {
            let t1 = libc::fcntl(r, libc::F_DUPFD, 500);
            let t2 = libc::fcntl(c, libc::F_DUPFD, 500);
            libc::dup2(t1, 4);
            libc::dup2(t2, 3);
            libc::close(t1);
            libc::close(t2);
            Ok(())
        });
    }
    let mut child = cmd.spawn().unwrap();
    drop(theirs);
    drop(rootf);
    mine.set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    mine.set_write_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut reader = std::io::BufReader::new(mine.try_clone().unwrap());
    let mut mine = mine;
    let mut line = String::new();
    use std::io::BufRead;
    reader.read_line(&mut line).unwrap(); // hello
    assert!(line.contains("\"seccomp\":\"enforced\""), "{line}");
    let op = Uuid::now_v7();
    let _ = mine.write_all(&request(op, root));
    let mut manifest = String::new();
    let _ = reader.read_line(&mut manifest);
    if !manifest.is_empty() {
        let _ = mine.write_all(&context(op, &manifest));
    }
    let _ = mine.shutdown(std::net::Shutdown::Write);
    let mut rest = Vec::new();
    let _ = reader.read_to_end(&mut rest);
    let status = child.wait().unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    Session {
        code: status.code(),
        stderr,
        after_manifest: rest.len(),
        manifest,
    }
}

fn request_json(op: Uuid, root: &Path, spaces: &[Uuid]) -> serde_json::Value {
    let r = Request {
        v: 2,
        op,
        root: std::fs::canonicalize(root).unwrap().display().to_string(),
        spaces: spaces.to_vec(),
        max_blob_bytes: 64 * 1024 * 1024,
        max_source_bytes: ParsePolicy::default().max_source_bytes,
        max_entity_depth: ParsePolicy::default().max_entity_depth,
        max_output_bytes: ParsePolicy::default().max_output_bytes,
    };
    serde_json::to_value(r).unwrap()
}

fn line(v: &serde_json::Value) -> Vec<u8> {
    let mut b = serde_json::to_vec(v).unwrap();
    b.push(b'\n');
    b
}

fn good_context(op: Uuid) -> serde_json::Value {
    serde_json::json!({"v": 2, "op": op, "identities": identities_for(&ParsePolicy::default()), "known": []})
}

fn space() -> Uuid {
    Uuid::now_v7()
}

/// Runs the worker with the given request and context mutation; the worker must refuse with exit
/// code 2, mention `why`, and send nothing after the manifest.
fn worker_refuses(
    request: impl FnOnce(&mut serde_json::Value),
    context: impl FnOnce(&mut serde_json::Value),
    why: &str,
) {
    let d = small_tree();
    let s = drive(
        d.path(),
        |op, root| {
            let mut r = request_json(op, root, &[space()]);
            request(&mut r);
            line(&r)
        },
        |op, _| {
            let mut c = good_context(op);
            context(&mut c);
            line(&c)
        },
    );
    assert_eq!(s.code, Some(2), "{}", s.stderr);
    assert!(s.stderr.contains(why), "{why:?} not in {:?}", s.stderr);
    assert_eq!(s.after_manifest, 0, "nothing was streamed");
}

#[test]
fn control_a_well_formed_exchange_streams() {
    let d = small_tree();
    let s = drive(
        d.path(),
        |op, root| line(&request_json(op, root, &[space()])),
        |op, _| line(&good_context(op)),
    );
    assert_eq!(s.code, Some(0), "{}", s.stderr);
    assert!(s.after_manifest > 1000);
    assert!(s.manifest.contains("\"entries\""));
}

const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn the_worker_refuses_every_malformed_context() {
    // unknown protocol version
    worker_refuses(|_| {}, |c| c["v"] = 1.into(), "context protocol 1");
    worker_refuses(|_| {}, |c| c["v"] = 99.into(), "context protocol 99");
    // cross-operation replay: a context made for another operation
    worker_refuses(
        |_| {},
        |c| c["op"] = Uuid::now_v7().to_string().into(),
        "another operation",
    );
    // missing required context
    worker_refuses(
        |_| {},
        |c| {
            c.as_object_mut().unwrap().remove("known");
        },
        "missing field `known`",
    );
    worker_refuses(
        |_| {},
        |c| {
            c.as_object_mut().unwrap().remove("identities");
        },
        "missing field `identities`",
    );
    // duplicate identity
    worker_refuses(
        |_| {},
        |c| {
            let first = c["identities"][0].clone();
            c["identities"] = serde_json::json!([first, first]);
        },
        "listed twice",
    );
    // conflicting identity: not one this operation's policy can produce
    worker_refuses(
        |_| {},
        |c| c["identities"][1]["extractor_version"] = "999+skip(p1:source_bytes>1)".into(),
        "cannot ask about",
    );
    worker_refuses(
        |_| {},
        |c| {
            let mut other = c["identities"][0].clone();
            other["grammar"] = "evil".into();
            c["identities"][2] = other;
        },
        "cannot ask about",
    );
    // duplicate known pair, dangling identity index, malformed hash
    worker_refuses(
        |_| {},
        |c| c["known"] = serde_json::json!([[HASH_A, 0], [HASH_A, 0]]),
        "repeated",
    );
    worker_refuses(
        |_| {},
        |c| c["known"] = serde_json::json!([[HASH_A, 7]]),
        "missing identity",
    );
    worker_refuses(
        |_| {},
        |c| c["known"] = serde_json::json!([["AAAA", 0]]),
        "not a lowercase SHA-256",
    );
    worker_refuses(
        |_| {},
        |c| c["known"] = serde_json::json!([[HASH_A, 0, "extra"]]),
        "bad context",
    );
    // not JSON at all is handled below; an extra field is an attempt to widen the exchange
    worker_refuses(
        |_| {},
        |c| c["grant"] = serde_json::json!({"root": "/etc", "access": "read"}),
        "unknown field `grant`",
    );
    worker_refuses(|_| {}, |c| c["root"] = "/".into(), "unknown field `root`");
}

#[test]
fn the_worker_refuses_a_bad_request_before_it_reads_anything() {
    // invalid space identifiers: duplicates, too many, not a UUID
    let d = small_tree();
    let s = space();
    for (spaces, why) in [
        (serde_json::json!([s, s]), "named twice"),
        (
            serde_json::json!((0..65).map(|_| Uuid::now_v7()).collect::<Vec<_>>()),
            "spaces (at most 64)",
        ),
        (serde_json::json!(["not-a-uuid"]), "bad request"),
    ] {
        let out = drive(
            d.path(),
            |op, root| {
                let mut r = request_json(op, root, &[]);
                r["spaces"] = spaces;
                line(&r)
            },
            |_, _| unreachable!("no manifest is sent after a bad request"),
        );
        assert_eq!(out.code, Some(2), "{}", out.stderr);
        assert!(out.stderr.contains(why), "{why}: {}", out.stderr);
        assert!(out.manifest.is_empty());
    }
    // unknown protocol version, and an unknown field (the worker never takes another authority)
    for (key, value, why) in [
        ("v", serde_json::json!(1), "unsupported protocol version 1"),
        ("v", serde_json::json!(3), "unsupported protocol version 3"),
        (
            "database",
            serde_json::json!("/x.db"),
            "unknown field `database`",
        ),
        (
            "grant_store",
            serde_json::json!("/x"),
            "unknown field `grant_store`",
        ),
    ] {
        let out = drive(
            d.path(),
            |op, root| {
                let mut r = request_json(op, root, &[space()]);
                r[key] = value;
                line(&r)
            },
            |_, _| unreachable!(),
        );
        assert_eq!(out.code, Some(2), "{}", out.stderr);
        assert!(out.stderr.contains(why), "{why}: {}", out.stderr);
    }
}

#[test]
fn the_worker_refuses_oversized_truncated_and_unparsable_contexts() {
    let d = small_tree();
    type Reply = Box<dyn FnOnce(Uuid, &str) -> Vec<u8>>;
    let go = |context: Reply| {
        drive(
            d.path(),
            |op, root| line(&request_json(op, root, &[space()])),
            context,
        )
    };
    // oversized: more than the limit without a newline, then more
    let s = go(Box::new(|_, _| {
        let mut b = vec![b'a'; MAX_CONTEXT_LINE + 4096];
        b.push(b'\n');
        b
    }));
    assert_eq!(s.code, Some(2), "{}", s.stderr);
    assert!(s.stderr.contains("context is too long"), "{}", s.stderr);
    assert_eq!(s.after_manifest, 0);
    // truncated: half a document, then the channel closes
    let s = go(Box::new(|op, _| {
        let full = serde_json::to_vec(&good_context(op)).unwrap();
        full[..full.len() / 2].to_vec()
    }));
    assert_eq!(s.code, Some(2), "{}", s.stderr);
    assert!(s.stderr.contains("truncated"), "{}", s.stderr);
    assert_eq!(s.after_manifest, 0);
    // closed without any context
    let s = go(Box::new(|_, _| Vec::new()));
    assert_eq!(s.code, Some(2), "{}", s.stderr);
    assert!(
        s.stderr.contains("closed before the context"),
        "{}",
        s.stderr
    );
    // not JSON, not UTF-8, wrong shape
    for bytes in [
        b"hello\n".to_vec(),
        vec![0xff, 0xfe, b'\n'],
        b"[]\n".to_vec(),
        b"{}\n".to_vec(),
        b"null\n".to_vec(),
    ] {
        let s = go(Box::new(move |_, _| bytes));
        assert_eq!(s.code, Some(2), "{}", s.stderr);
        assert_eq!(s.after_manifest, 0);
    }
}

// ----------------------------------------- the parent's side, against stand-in workers

mod fake_workers {
    use super::*;

    /// Reads the request, then runs `body` with `$OP` set. The stand-ins are not sandboxed, so the
    /// confinement evidence is turned off for them (and only for them).
    fn script(dir: &Path, body: &str) -> WorkerConfig {
        let path = dir.join("fake.sh");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\necho hello >&3\nIFS= read -r REQ <&3\nOP=$(printf '%s' \"$REQ\" | sed 's/.*\"op\":\"\\([^\"]*\\)\".*/\\1/')\n{body}\n"
            ),
        )
        .unwrap();
        let mut c = WorkerConfig::new("/bin/sh");
        c.args = vec![path.into_os_string()];
        c.require_sandbox_evidence = false;
        c.wall_timeout = Duration::from_secs(20);
        c.idle_timeout = Duration::from_secs(10);
        c
    }

    fn run(c: &WorkerConfig) -> Result<(), WorkerError> {
        run_with(c, &no_cancel(), None)
    }

    fn run_with(
        c: &WorkerConfig,
        cancel: &AtomicBool,
        spaces: Option<Vec<Uuid>>,
    ) -> Result<(), WorkerError> {
        let d = small_tree();
        let f = fixture(d.path());
        let db = db();
        let before = db.rows();
        let store = db.store();
        let spaces = spaces.unwrap_or_else(|| vec![db.space()]);
        let r = isolated(c, &anchor(&f), &spaces, &StoreContext(&store), cancel).map(|_| ());
        assert_eq!(db.rows(), before, "nothing was persisted");
        r
    }

    /// A stand-in that answers the request with this manifest line (the real `$OP` is substituted
    /// for the placeholder `OP`).
    fn manifest(dir: &Path, json: &str) -> WorkerConfig {
        let text = format!("{json}\n");
        let file = dir.join("manifest.json");
        std::fs::write(&file, text).unwrap();
        script(
            dir,
            &format!("sed \"s/OP/$OP/\" {} >&3; sleep 2", file.display()),
        )
    }

    fn context_err(r: Result<(), WorkerError>, why: &str) {
        match r {
            Err(WorkerError::Context(m)) => assert!(m.contains(why), "{why:?} not in {m:?}"),
            other => panic!("{why}: {other:?}"),
        }
    }

    #[test]
    fn an_unknown_space_is_refused_before_any_worker_is_started() {
        let t = tempfile::tempdir().unwrap();
        let pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut c = script(t.path(), "sleep 5");
        c.observe_pid = Some(pid.clone());
        let r = run_with(&c, &no_cancel(), Some(vec![Uuid::now_v7()]));
        context_err(r, "does not exist");
        assert_eq!(
            pid.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "no process"
        );
        // duplicates are refused as well
        let db = db();
        let s = db.space();
        let r = run_with(&c, &no_cancel(), Some(vec![s, s]));
        context_err(r, "named twice");
    }

    #[test]
    fn malformed_manifests_are_refused() {
        let t = tempfile::tempdir().unwrap();
        let h = "bb".repeat(32);
        let cases: Vec<(String, &str)> = vec![
            (
                r#"{"v":1,"op":"OP","entries":[]}"#.into(),
                "manifest protocol 1",
            ),
            (
                r#"{"v":99,"op":"OP","entries":[]}"#.into(),
                "manifest protocol 99",
            ),
            (r#"{"v":2,"entries":[]}"#.into(), "unreadable manifest"),
            (r#"{"v":2,"op":"OP"}"#.into(), "unreadable manifest"),
            (
                r#"{"v":2,"op":"00000000-0000-7000-8000-000000000000","entries":[]}"#.into(),
                "another operation",
            ),
            (
                format!(
                    r#"{{"v":2,"op":"OP","entries":[{{"language":"rust","hash":"{h}"}},{{"language":"rust","hash":"{h}"}}]}}"#
                ),
                "repeated",
            ),
            (
                r#"{"v":2,"op":"OP","entries":[{"language":"rust","hash":"ABC"}]}"#.into(),
                "not a lowercase SHA-256",
            ),
            (
                r#"{"v":2,"op":"OP","entries":[{"language":"cobol","hash":"x"}]}"#.into(),
                "unreadable manifest",
            ),
            // a worker asking for more authority than it was given
            (
                r#"{"v":2,"op":"OP","entries":[],"root":"/etc"}"#.into(),
                "unknown field `root`",
            ),
            (
                r#"{"v":2,"op":"OP","entries":[],"grant":{"path":"/home","access":"rw"}}"#.into(),
                "unknown field `grant`",
            ),
            (
                format!(
                    r#"{{"v":2,"op":"OP","entries":[{{"language":"rust","hash":"{h}","path":"/etc/shadow"}}]}}"#
                ),
                "unknown field `path`",
            ),
            ("not json".into(), "unreadable manifest"),
            ("[]".into(), "unreadable manifest"),
        ];
        for (json, why) in cases {
            context_err(run(&manifest(t.path(), &json)), why);
        }
    }

    #[test]
    fn an_oversized_manifest_is_refused_by_size_and_by_count() {
        let t = tempfile::tempdir().unwrap();
        // longer than the line limit, never terminated
        let c = script(
            t.path(),
            &format!(
                "head -c {} /dev/zero | tr '\\0' 'a' >&3; sleep 2",
                MAX_MANIFEST_LINE + 1024
            ),
        );
        context_err(run(&c), "too long");
        // within the line limit but more entries than any directory can have
        let n = mindspace::worker::context::MAX_MANIFEST_ENTRIES + 1;
        let entry = format!(r#"{{"language":"rust","hash":"{}"}}"#, "cc".repeat(32));
        let body = format!(
            r#"{{"v":2,"op":"OP","entries":[{}]}}"#,
            vec![entry; n].join(",")
        );
        assert!(body.len() < MAX_MANIFEST_LINE, "{}", body.len());
        // all hashes equal would be refused as repeated: make them distinct
        let body = {
            let mut out = String::from(r#"{"v":2,"op":"OP","entries":["#);
            for i in 0..n {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&format!(r#"{{"language":"rust","hash":"{:064x}"}}"#, i));
            }
            out.push_str("]}");
            out
        };
        assert!(body.len() < MAX_MANIFEST_LINE, "{}", body.len());
        context_err(run(&manifest(t.path(), &body)), "manifest entries");
    }

    #[test]
    fn a_truncated_manifest_and_a_crash_during_the_exchange_are_errors() {
        let t = tempfile::tempdir().unwrap();
        // half a manifest, then exit
        let c = script(t.path(), r#"printf '{"v":2,"op":"%s","ent' "$OP" >&3"#);
        assert!(matches!(run(&c), Err(WorkerError::Died { .. })));
        // exit right after the request
        let c = script(t.path(), "exit 7");
        match run(&c) {
            Err(WorkerError::Died { status, .. }) => assert!(
                status.contains("exit code 7") || status.contains("closed"),
                "{status}"
            ),
            other => panic!("{other:?}"),
        }
        // a valid manifest, then the process dies before the stream (killed by itself)
        let c = script(
            t.path(),
            r#"printf '{"v":2,"op":"%s","entries":[]}\n' "$OP" >&3; IFS= read -r CTX <&3; kill -9 $$"#,
        );
        match run(&c) {
            Err(WorkerError::Died { status, .. }) => {
                assert!(status.contains("signal 9"), "{status}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_worker_that_stalls_in_the_exchange_is_cut_off_and_a_cancel_is_prompt() {
        let t = tempfile::tempdir().unwrap();
        // silent after the request: the idle limit
        let mut c = script(t.path(), "sleep 30");
        c.idle_timeout = Duration::from_millis(600);
        let started = Instant::now();
        assert!(matches!(run(&c), Err(WorkerError::Timeout(_))));
        assert!(started.elapsed() < Duration::from_secs(5));
        // keeps the manifest line open (no newline) and the wall clock expires
        let mut c = script(t.path(), r#"printf '{"v":2' >&3; sleep 30"#);
        c.wall_timeout = Duration::from_millis(1000);
        c.idle_timeout = Duration::from_secs(30);
        assert!(matches!(run(&c), Err(WorkerError::Timeout(_))));
        // cancelled while waiting for the manifest
        let c = script(t.path(), "sleep 30");
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let started = Instant::now();
        let r = run_with(&c, &cancel, None);
        h.join().unwrap();
        assert!(matches!(r, Err(WorkerError::Cancelled)), "{r:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}

// ---------------------------------------------- the result is checked against the request

#[test]
fn a_result_for_other_spaces_or_with_foreign_identities_is_refused_before_persistence() {
    use mindspace::core::{
        CodeContextDraft, CodeDerivationDraft, CodeInputDraft, CodeLanguage, CodeParseStatus,
    };
    use mindspace::worker::context::check_result;
    let d = small_tree();
    let f = fixture(d.path());
    let db = db();
    let Extracted::Files(mut files) = in_process(&db, &f) else {
        panic!()
    };
    let spaces = [db.space()];
    let policy = ParsePolicy::default();
    check_result(&files, &spaces, &policy).unwrap();
    // another space than asked
    assert!(check_result(&files, &[Uuid::now_v7()], &policy)
        .unwrap_err()
        .contains("other spaces"));
    // a derivation with an identity the operation cannot produce
    let mut foreign = files.code_inputs[0].clone();
    foreign.derivation.identity.grammar = "evil".into();
    files.code_inputs.push(foreign);
    assert!(check_result(&files, &spaces, &policy)
        .unwrap_err()
        .contains("identity"));
    files.code_inputs.pop();
    // a skip claimed for a source that is within the budget
    let mut lie = CodeInputDraft {
        language: CodeLanguage::Rust,
        content_hash: "dd".repeat(32),
        content: "fn small() {}\n".into(),
        repository_id: None,
        blob_oid: None,
        contexts: vec![CodeContextDraft {
            path: "/x.rs".into(),
            commit_oid: None,
        }],
        derivation: CodeDerivationDraft {
            identity: mindspace::ingest::code::skip_identity(
                mindspace::ingest::code::SkipReason::SourceBytes {
                    limit: policy.max_source_bytes,
                },
            ),
            status: CodeParseStatus::Failed,
            error_count: 0,
            entities: vec![],
            imports: vec![],
            relationships: vec![],
        },
    };
    files.code_inputs.push(lie.clone());
    assert!(check_result(&files, &spaces, &policy)
        .unwrap_err()
        .contains("skip derivation"));
    // ...and the reverse: a depth skip claimed for an oversized source
    files.code_inputs.pop();
    lie.content = "x".repeat(policy.max_source_bytes as usize + 1);
    lie.derivation.identity =
        mindspace::ingest::code::skip_identity(mindspace::ingest::code::SkipReason::EntityDepth {
            limit: policy.max_entity_depth,
        });
    files.code_inputs.push(lie);
    assert!(check_result(&files, &spaces, &policy)
        .unwrap_err()
        .contains("skip derivation"));
}
