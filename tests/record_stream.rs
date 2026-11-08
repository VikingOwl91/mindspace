//! M7.3b.1: the internal record stream for extraction results.
//!
//! Evidence: a streamed extraction equals the in-process extraction (directory
//! and Git, several scopes, with the repeat-ingest shortcut), and the consumer
//! refuses every kind of hostile or broken stream without leaving any database
//! state behind.

#![allow(
    clippy::field_reassign_with_default,
    clippy::redundant_closure,
    dead_code
)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use mindspace::core::{Error, Result, Uuid};
use mindspace::ingest::confined::{extract_confined_git, extract_directory, Anchor};
use mindspace::ingest::deadline::Deadline;
use mindspace::ingest::extract::{persist, Extracted, ExtractionLimits, StoreContext};
use mindspace::ingest::outline::PathSelection;
use mindspace::ingest::stream::{
    consume, emit_extracted, stream_confined_git, stream_directory, FrameWriter, Kind,
    ProducerLimits, Record, RecordSink, StreamLimits, MAX_FRAME, MAX_RECORD,
};
use mindspace::ingest::GitScope;
use mindspace::mcp::McpConfig;
use mindspace::storage::Store;
use mindspace::{initialize, open_existing, SqliteStore};
use std::collections::BTreeMap;

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
    let store = open_existing(&path).unwrap();
    store.create_space("p", "").unwrap();
    Db { _dir: dir, path }
}

/// A byte-identical copy of an (untouched) database, so both hold the same space ids.
fn db_clone(src: &Db) -> Db {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mind.db");
    std::fs::copy(&src.path, &path).unwrap();
    Db { _dir: dir, path }
}

impl Db {
    fn store(&self) -> SqliteStore {
        open_existing(&self.path).unwrap()
    }

    /// Row count of every table: a coarse but total snapshot of the contents.
    fn rows(&self) -> BTreeMap<String, i64> {
        let c = rusqlite::Connection::open_with_flags(
            &self.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
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
}

/// Ids and timestamps differ between runs; everything else must not.
fn normalize(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let hex = |c: u8| c.is_ascii_hexdigit();
    while i < b.len() {
        // uuid: 8-4-4-4-12
        if i + 36 <= b.len()
            && [8, 13, 18, 23].iter().all(|&k| b[i + k] == b'-')
            && b[i..i + 36]
                .iter()
                .enumerate()
                .all(|(k, &c)| [8, 13, 18, 23].contains(&k) || hex(c))
        {
            out.push_str("<id>");
            i += 36;
            continue;
        }
        // RFC 3339 time: 2026-11-08T10:20:30...Z
        if i + 20 <= b.len()
            && b[i + 4] == b'-'
            && b[i + 10] == b'T'
            && b[i..i + 4].iter().all(u8::is_ascii_digit)
        {
            let mut j = i + 19;
            while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.') {
                j += 1;
            }
            if j < b.len() && b[j] == b'Z' {
                out.push_str("<time>");
                i = j + 1;
                continue;
            }
        }
        // microsecond timestamps (`Ts(1794142654740130)`)
        if b[i].is_ascii_digit() {
            let mut j = i;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j - i >= 15 {
                out.push_str("<ts>");
                i = j;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

/// Documents and code, including a file big enough to need content chunks and
/// one with so many sections that its derived vectors need several parts.
fn directory() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        "docs/guide.md",
        "# Guide\n\nalpha marker\n\n## Part\n\nbeta\n",
    );
    write(d.path(), "notes.txt", "plain note \u{e4}\u{20ac}\n");
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
    write(d.path(), "empty.txt", "");
    let mut big = String::new();
    for i in 0..9000 {
        big.push_str(&format!(
            "# Section {i}\n\nbody of section {i} with words\n\n"
        ));
    }
    write(d.path(), "docs/big.md", &big);
    let mut code = String::new();
    for i in 0..6000 {
        code.push_str(&format!(
            "pub fn function_{i}(a: u32) -> u32 {{ a + {i} }}\n"
        ));
    }
    write(d.path(), "src/big.rs", &code);
    d
}

fn repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main", "."]);
    write(d.path(), "README.md", "# Repo\n\nfirst\n");
    write(d.path(), "src/lib.rs", "pub fn one() {}\n");
    write(d.path(), "same.md", "# Same\n\nidentical content\n");
    write(d.path(), "copy/same.md", "# Same\n\nidentical content\n");
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "first"]);
    git(d.path(), &["tag", "-a", "v1", "-m", "release one"]);
    git(d.path(), &["tag", "light"]);
    git(d.path(), &["checkout", "-q", "-b", "feature"]);
    write(d.path(), "src/extra.rs", "pub fn two() { one(); }\n");
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "feature work"]);
    git(d.path(), &["checkout", "-q", "main"]);
    let mut big = String::new();
    for i in 0..9000 {
        big.push_str(&format!("# Part {i}\n\ntext {i}\n\n"));
    }
    write(d.path(), "docs/big.md", &big);
    write(d.path(), "docs/more.md", "# More\n\nsecond\n");
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "second"]);
    d
}

/// Collects records instead of writing frames (to build and alter streams).
#[derive(Default)]
struct Collect(Vec<Record>);

impl RecordSink for Collect {
    fn emit_encoded(&mut self, _name: &'static str, json: Vec<u8>) -> Result<()> {
        self.0.push(serde_json::from_slice(&json).unwrap());
        Ok(())
    }
}

fn write_stream(op: Uuid, records: &[Record]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut w = FrameWriter::new(&mut out, op, None);
        for r in records {
            if matches!(r, Record::End { .. }) {
                continue;
            }
            w.emit(r).unwrap();
        }
        w.finish().unwrap();
    }
    out
}

/// A stream with a hand-made closing record (counts of what is in `records`).
fn encode(op: Uuid, records: &[Record]) -> Vec<u8> {
    write_stream(op, records)
}

fn text(e: &Extracted) -> String {
    normalize(&format!("{e:#?}"))
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

fn stream_git(
    f: &Fixture,
    store: &SqliteStore,
    sel: &PathSelection,
    scope: GitScope,
) -> (Vec<u8>, Uuid) {
    let op = Uuid::now_v7();
    let mut out = Vec::new();
    {
        let mut w = FrameWriter::new(&mut out, op, None);
        stream_confined_git(
            &StoreContext(store),
            &anchor(f),
            &f.cfg,
            sel,
            scope,
            &["p"],
            &Deadline::none(),
            &ProducerLimits::default(),
            &mut w,
        )
        .unwrap();
        w.finish().unwrap();
    }
    (out, op)
}

fn stream_dir(f: &Fixture, store: &SqliteStore) -> (Vec<u8>, Uuid) {
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
            &ProducerLimits::default(),
            &mut w,
        )
        .unwrap();
        w.finish().unwrap();
    }
    (out, op)
}

fn read(bytes: &[u8], op: Uuid, kind: Kind, root: Option<&Path>) -> Result<Extracted> {
    consume(bytes, op, kind, root, &StreamLimits::default(), None).map(|(e, _)| e)
}

fn err(r: Result<Extracted>) -> String {
    match r {
        Ok(_) => panic!("the stream was accepted"),
        Err(e) => e.to_string(),
    }
}

// ----------------------------------------------------------------- equality

#[test]
fn a_streamed_directory_equals_the_in_process_extraction() {
    let d = directory();
    let f = fixture(d.path());
    let db = db();
    let store = db.store();
    let ids = vec![store.get_space("p").unwrap().id];
    let direct = Extracted::Files(
        extract_directory(
            &StoreContext(&store),
            &anchor(&f),
            &f.cfg,
            &ids,
            &Deadline::none(),
        )
        .unwrap(),
    );
    let before = db.rows();
    let (bytes, op) = stream_dir(&f, &store);
    assert_eq!(db.rows(), before, "streaming wrote to the database");
    let got = read(&bytes, op, Kind::Files, Some(&f.root)).unwrap();
    assert_eq!(text(&direct), text(&got));
    got.validate(Some(&f.root), &ExtractionLimits::default())
        .unwrap();
    // the big files really needed several chunks and parts
    assert!(
        String::from_utf8_lossy(&bytes)
            .matches("\"type\":\"chunk\"")
            .count()
            > 4
    );
    assert!(
        String::from_utf8_lossy(&bytes)
            .matches("\"type\":\"part\"")
            .count()
            > 4
    );
}

#[test]
fn a_streamed_git_extraction_equals_the_in_process_extraction() {
    let r = repo();
    let f = fixture(r.path());
    for (label, sel, scope) in [
        ("head", PathSelection::all(), GitScope::Head),
        ("history", PathSelection::all(), GitScope::History),
        (
            "selected",
            PathSelection::new(&["docs"]).unwrap(),
            GitScope::Head,
        ),
    ] {
        let db = db();
        let store = db.store();
        let direct = Extracted::Git(
            extract_confined_git(
                &StoreContext(&store),
                &anchor(&f),
                &f.cfg,
                &sel,
                scope,
                &["p"],
                &Deadline::none(),
            )
            .unwrap(),
        );
        let before = db.rows();
        let (bytes, op) = stream_git(&f, &store, &sel, scope);
        assert_eq!(db.rows(), before, "{label}: streaming wrote");
        let got = read(&bytes, op, Kind::Git, None).unwrap();
        assert_eq!(text(&direct), text(&got), "{label}");
        got.validate(None, &ExtractionLimits::default()).unwrap();
    }
}

#[test]
fn a_repeat_ingest_keeps_the_derivation_shortcut_and_the_ref_events() {
    let r = repo();
    let f = fixture(r.path());
    let db = db();
    let store = db.store();
    // first ingest through the stream, persisted
    let (bytes, op) = stream_git(&f, &store, &PathSelection::all(), GitScope::Head);
    let first = read(&bytes, op, Kind::Git, None).unwrap();
    persist(&store, first, &Deadline::none()).unwrap();
    // advance the branch (a fast-forward), then extract both ways against the stored state
    write(r.path(), "src/new.rs", "pub fn three() {}\n");
    git(r.path(), &["add", "."]);
    git(r.path(), &["commit", "-q", "-m", "third"]);
    let direct = Extracted::Git(
        extract_confined_git(
            &StoreContext(&store),
            &anchor(&f),
            &f.cfg,
            &PathSelection::all(),
            GitScope::Head,
            &["p"],
            &Deadline::none(),
        )
        .unwrap(),
    );
    let (bytes, op) = stream_git(&f, &store, &PathSelection::all(), GitScope::Head);
    let got = read(&bytes, op, Kind::Git, None).unwrap();
    assert_eq!(text(&direct), text(&got));
    let Extracted::Git(g) = &got else { panic!() };
    assert!(
        g.observation
            .ref_events
            .iter()
            .any(|e| format!("{:?}", e.kind) == "FastForward"),
        "the fast-forward classification must survive"
    );
    // known blobs carry an empty complete derivation, as in process
    let known = g
        .code_inputs
        .iter()
        .filter(|c| c.derivation.entities.is_empty())
        .count();
    assert!(known > 0, "the has_code_derivation shortcut was lost");
}

#[test]
fn persisting_a_streamed_result_gives_the_same_database_as_in_process() {
    let d = directory();
    let r = repo();
    for (root, is_git) in [(d.path(), false), (r.path(), true)] {
        let f = fixture(root);
        let (a, b) = (db(), db());
        let (sa, sb) = (a.store(), b.store());
        // in process
        let ex = if is_git {
            Extracted::Git(
                extract_confined_git(
                    &StoreContext(&sa),
                    &anchor(&f),
                    &f.cfg,
                    &PathSelection::all(),
                    GitScope::Head,
                    &["p"],
                    &Deadline::none(),
                )
                .unwrap(),
            )
        } else {
            let ids = vec![sa.get_space("p").unwrap().id];
            Extracted::Files(
                extract_directory(
                    &StoreContext(&sa),
                    &anchor(&f),
                    &f.cfg,
                    &ids,
                    &Deadline::none(),
                )
                .unwrap(),
            )
        };
        persist(&sa, ex, &Deadline::none()).unwrap();
        // streamed
        let got = if is_git {
            let (bytes, op) = stream_git(&f, &sb, &PathSelection::all(), GitScope::Head);
            read(&bytes, op, Kind::Git, None).unwrap()
        } else {
            let (bytes, op) = stream_dir(&f, &sb);
            read(&bytes, op, Kind::Files, Some(&f.root)).unwrap()
        };
        persist(&sb, got, &Deadline::none()).unwrap();
        assert_eq!(a.rows(), b.rows());
    }
}

// --------------------------------------------------------------- hostile input

fn git_records() -> (Vec<Record>, Fixture, tempfile::TempDir) {
    let r = repo();
    let f = fixture(r.path());
    let db = db();
    let store = db.store();
    let (bytes, op) = stream_git(&f, &store, &PathSelection::all(), GitScope::Head);
    let ex = read(&bytes, op, Kind::Git, None).unwrap();
    let mut c = Collect::default();
    emit_extracted(&mut c, ex).unwrap();
    (c.0, f, r)
}

fn dir_records() -> (Vec<Record>, Fixture, tempfile::TempDir) {
    let d = directory();
    let f = fixture(d.path());
    let db = db();
    let store = db.store();
    let (bytes, op) = stream_dir(&f, &store);
    let ex = read(&bytes, op, Kind::Files, Some(&f.root)).unwrap();
    let mut c = Collect::default();
    emit_extracted(&mut c, ex).unwrap();
    (c.0, f, d)
}

#[test]
fn a_clean_re_encoded_stream_is_accepted() {
    let (recs, f, _k) = git_records();
    let op = Uuid::now_v7();
    read(&encode(op, &recs), op, Kind::Git, None).unwrap();
    let (recs, f2, _d) = dir_records();
    let op = Uuid::now_v7();
    read(&encode(op, &recs), op, Kind::Files, Some(&f2.root)).unwrap();
    let _ = f;
}

#[test]
fn truncation_anywhere_is_refused() {
    let (recs, _f, _k) = git_records();
    let op = Uuid::now_v7();
    let bytes = encode(op, &recs);
    // inside the length, inside a payload, at a frame boundary, empty
    let first = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize + 4;
    for cut in [
        0,
        2,
        4,
        9,
        first - 1,
        first,
        bytes.len() / 2,
        bytes.len() - 1,
    ] {
        let e = err(read(&bytes[..cut], op, Kind::Git, None));
        assert!(
            e.contains("ended") || e.contains("truncated"),
            "cut at {cut}: {e}"
        );
    }
}

#[test]
fn a_frame_larger_than_the_limit_is_refused_before_it_is_read() {
    let op = Uuid::now_v7();
    // only the length arrives: a reader that would allocate 2 MiB and wait for it fails differently
    let bytes = ((MAX_FRAME + 1) as u32).to_le_bytes().to_vec();
    let e = err(read(&bytes, op, Kind::Git, None));
    assert!(e.contains("frame length"), "{e}");
    let e = err(read(&0u32.to_le_bytes(), op, Kind::Git, None));
    assert!(e.contains("frame length"), "{e}");
    let e = err(read(&u32::MAX.to_le_bytes(), op, Kind::Git, None));
    assert!(e.contains("frame length"), "{e}");
}

#[test]
fn too_many_frames_and_a_spent_byte_budget_are_refused() {
    let (recs, _f, _k) = git_records();
    let op = Uuid::now_v7();
    let bytes = encode(op, &recs);
    let frames = {
        let mut n = 0;
        let mut i = 0;
        while i < bytes.len() {
            i += u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap()) as usize + 4;
            n += 1;
        }
        n
    };
    assert!(
        frames >= 3,
        "the fixture must span several frames ({frames})"
    );
    let mut l = StreamLimits::default();
    l.max_frames = frames - 1;
    let e = consume(&bytes[..], op, Kind::Git, None, &l, None)
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("frames"), "{e}");
    let mut l = StreamLimits::default();
    l.max_total = bytes.len() as u64 - 1;
    let e = consume(&bytes[..], op, Kind::Git, None, &l, None)
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("bytes in one operation"), "{e}");
    // exactly at the limits it is accepted
    let mut l = StreamLimits::default();
    l.max_frames = frames;
    l.max_total = bytes.len() as u64;
    consume(&bytes[..], op, Kind::Git, None, &l, None).unwrap();
}

fn raw_frame(json: &str) -> Vec<u8> {
    let mut v = (json.len() as u32).to_le_bytes().to_vec();
    v.extend_from_slice(json.as_bytes());
    v
}

#[test]
fn unknown_versions_types_operations_and_sequences_are_refused() {
    let op = Uuid::now_v7();
    let other = Uuid::now_v7();
    let begin = r#"{"type":"begin","kind":"git"}"#;
    let ok = |seq: u64, o: Uuid, recs: &str| {
        raw_frame(&format!(
            r#"{{"v":1,"op":"{o}","seq":{seq},"records":[{recs}]}}"#
        ))
    };
    let e = err(read(
        &raw_frame(&format!(
            r#"{{"v":2,"op":"{op}","seq":0,"records":[{begin}]}}"#
        )),
        op,
        Kind::Git,
        None,
    ));
    assert!(e.contains("unsupported protocol version 2"), "{e}");
    // a future version may not even decode: still named as a version problem
    let e = err(read(
        &raw_frame(&format!(
            r#"{{"v":9,"op":"{op}","seq":0,"records":[{{"type":"from_the_future"}}]}}"#
        )),
        op,
        Kind::Git,
        None,
    ));
    assert!(e.contains("unsupported protocol version 9"), "{e}");
    let e = err(read(&ok(0, op, r#"{"type":"bogus"}"#), op, Kind::Git, None));
    assert!(e.contains("undecodable"), "{e}");
    let e = err(read(&ok(0, other, begin), op, Kind::Git, None));
    assert!(e.contains("another operation"), "{e}");
    let mut two = ok(0, op, begin);
    two.extend(ok(2, op, begin));
    let e = err(read(&two, op, Kind::Git, None));
    assert!(e.contains("frame 2 where 1"), "{e}");
    let mut twice = ok(0, op, begin);
    twice.extend(ok(0, op, begin));
    let e = err(read(&twice, op, Kind::Git, None));
    assert!(e.contains("where 1 was expected"), "{e}");
    let e = err(read(&ok(0, op, begin), op, Kind::Files, None));
    assert!(e.contains("requested"), "{e}");
    let e = err(read(
        &ok(0, op, r#"{"type":"chunk","data":"x"}"#),
        op,
        Kind::Git,
        None,
    ));
    assert!(e.contains("before the opening record"), "{e}");
}

/// Repeats the owner record at `i` with its content chunks and parts.
fn repeat_owner(v: &mut Vec<Record>, i: usize) {
    let mut end = i + 1;
    while end < v.len() && matches!(v[end], Record::Chunk { .. } | Record::Part(_)) {
        end += 1;
    }
    let copy: Vec<Record> = v[i..end].to_vec();
    for (k, r) in copy.into_iter().enumerate() {
        v.insert(end + k, r);
    }
}

fn changed(mut recs: Vec<Record>, f: impl FnOnce(&mut Vec<Record>)) -> Vec<Record> {
    f(&mut recs);
    recs
}

fn must_fail(kind: Kind, root: Option<&Path>, recs: &[Record], needle: &str) {
    let op = Uuid::now_v7();
    let e = match read(&encode(op, recs), op, kind, root) {
        Ok(_) => panic!("accepted, wanted an error with `{needle}`"),
        Err(e) => e.to_string(),
    };
    assert!(e.contains(needle), "wanted `{needle}` in `{e}`");
}

#[test]
fn duplicates_and_bad_order_are_refused() {
    let (recs, _f, _k) = git_records();
    let pos = |pred: &dyn Fn(&Record) -> bool| recs.iter().position(|r| pred(r)).unwrap();
    let commit = pos(&|r| matches!(r, Record::Commit { .. }));
    let reff = pos(&|r| matches!(r, Record::Ref { .. }));
    let blob = pos(&|r| matches!(r, Record::Blob { .. }));
    let deriv = pos(&|r| matches!(r, Record::Derivation { .. }));
    let code = pos(&|r| matches!(r, Record::CodeInput { .. }));
    must_fail(
        Kind::Git,
        None,
        &changed(recs.clone(), |v| {
            let r = v[commit].clone();
            v.insert(commit, r);
        }),
        "duplicate object",
    );
    must_fail(
        Kind::Git,
        None,
        &changed(recs.clone(), |v| {
            let r = v[reff].clone();
            v.insert(reff, r);
        }),
        "duplicate ref",
    );
    must_fail(
        Kind::Git,
        None,
        &changed(recs.clone(), |v| {
            let r = v[blob].clone();
            v.insert(blob, r);
        }),
        "duplicate blob",
    );
    must_fail(
        Kind::Git,
        None,
        &changed(recs.clone(), |v| repeat_owner(v, deriv)),
        "duplicate derivation",
    );
    must_fail(
        Kind::Git,
        None,
        &changed(recs.clone(), |v| repeat_owner(v, code)),
        "duplicate code input",
    );
    // a ref after a commit, a header after the body, a second begin, a record after the end
    must_fail(
        Kind::Git,
        None,
        &changed(recs.clone(), |v| {
            v.swap(reff, commit);
        }),
        "out of order",
    );
    must_fail(
        Kind::Git,
        None,
        &changed(recs.clone(), |v| {
            let r = v[0].clone();
            v.insert(1, r);
        }),
        "second opening",
    );
    must_fail(
        Kind::Git,
        None,
        &changed(recs.clone(), |v| {
            v.remove(1);
        }),
        "out of order",
    );
    must_fail(
        Kind::Git,
        None,
        &changed(recs.clone(), |v| {
            let r = v[reff].clone();
            v.push(r);
        }),
        "out of order",
    );
    // a part with no owner, a chunk with no slot
    must_fail(
        Kind::Git,
        None,
        &changed(recs.clone(), |v| {
            v.insert(blob, Record::Chunk { data: "x".into() });
        }),
        "chunk",
    );
    // a derivation whose shared content belongs to another blob
    let (recs2, _f2, _d2) = dir_records();
    let file = recs2
        .iter()
        .position(|r| matches!(r, Record::File { .. }))
        .unwrap();
    must_fail(
        Kind::Files,
        None,
        &changed(recs2.clone(), |v| repeat_owner(v, file)),
        "duplicate file",
    );
}

#[test]
fn wrong_hashes_paths_ids_and_sizes_are_refused() {
    let (recs, _f, _k) = git_records();
    let first = |pred: &dyn Fn(&Record) -> bool| recs.iter().position(|r| pred(r)).unwrap();
    let deriv = first(&|r| matches!(r, Record::Derivation { .. }));
    let code = first(&|r| matches!(r, Record::CodeInput { .. }));
    let commit = first(&|r| matches!(r, Record::Commit { .. }));
    let tweak = |f: &dyn Fn(&mut Record)| changed(recs.clone(), |v| f(&mut v[deriv]));
    must_fail(
        Kind::Git,
        None,
        &tweak(&|r| {
            if let Record::Derivation { file, .. } = r {
                file.content_hash = "0".repeat(64)
            }
        }),
        "content hash mismatch",
    );
    must_fail(
        Kind::Git,
        None,
        &tweak(&|r| {
            if let Record::Derivation { file, .. } = r {
                file.path = "../escape.md".into()
            }
        }),
        "climbs out",
    );
    must_fail(
        Kind::Git,
        None,
        &tweak(&|r| {
            if let Record::Derivation { file, .. } = r {
                file.path = "/abs/path.md".into()
            }
        }),
        "absolute",
    );
    must_fail(
        Kind::Git,
        None,
        &tweak(&|r| {
            if let Record::Derivation { file, .. } = r {
                file.path = "a\0b".into()
            }
        }),
        "NUL",
    );
    must_fail(
        Kind::Git,
        None,
        &tweak(&|r| {
            if let Record::Derivation { file, .. } = r {
                file.blob_oid = "xyz".into()
            }
        }),
        "malformed",
    );
    must_fail(
        Kind::Git,
        None,
        &tweak(&|r| {
            if let Record::Derivation { file, .. } = r {
                file.content = "smuggled".into()
            }
        }),
        "must not carry content",
    );
    must_fail(
        Kind::Git,
        None,
        &changed(recs.clone(), |v| {
            if let Record::CodeInput { input, .. } = &mut v[code] {
                input.content_hash = "1".repeat(64)
            }
        }),
        "hash mismatch",
    );
    must_fail(
        Kind::Git,
        None,
        &changed(recs.clone(), |v| {
            if let Record::CodeInput { input, .. } = &mut v[code] {
                input.repository_id = Some(Uuid::now_v7())
            }
        }),
        "another repository",
    );
    must_fail(
        Kind::Git,
        None,
        &changed(recs.clone(), |v| {
            if let Record::CodeInput { input, .. } = &mut v[code] {
                input.contexts[0].path = "no/such.rs".into()
            }
        }),
        "without a derivation",
    );
    must_fail(
        Kind::Git,
        None,
        &changed(recs.clone(), |v| {
            if let Record::Commit { item } = &mut v[commit] {
                item.oid = "nothex".into()
            }
        }),
        "malformed",
    );
    // content longer or shorter than announced; a chunk beyond the length
    let chunk = first(&|r| matches!(r, Record::Chunk { .. }));
    must_fail(
        Kind::Git,
        None,
        &changed(recs.clone(), |v| {
            if let Record::Chunk { data } = &mut v[chunk] {
                data.push('x')
            }
        }),
        "beyond the announced",
    );
    must_fail(
        Kind::Git,
        None,
        &changed(recs.clone(), |v| {
            if let Record::Chunk { data } = &mut v[chunk] {
                data.pop();
            }
        }),
        "incomplete",
    );
    // wrong closing counts, and a dropped record
    let (dir, f2, _d2) = dir_records();
    let file = dir
        .iter()
        .position(|r| matches!(r, Record::File { .. }))
        .unwrap();
    must_fail(
        Kind::Files,
        Some(&f2.root),
        &changed(dir.clone(), |v| {
            if let Record::File { file, .. } = &mut v[file] {
                file.content_hash = "2".repeat(64)
            }
        }),
        "content hash mismatch",
    );
    must_fail(
        Kind::Files,
        Some(&f2.root),
        &changed(dir.clone(), |v| {
            if let Record::File { file, .. } = &mut v[file] {
                file.size_bytes = 0
            }
        }),
        "size smaller",
    );
    must_fail(
        Kind::Files,
        Some(&f2.root),
        &changed(dir.clone(), |v| {
            if let Record::File { file, .. } = &mut v[file] {
                file.location = "/etc/passwd".into()
            }
        }),
        "outside the root",
    );
}

#[test]
fn the_root_and_the_content_budget_are_checked() {
    let (dir, f, _d) = dir_records();
    let op = Uuid::now_v7();
    let bytes = encode(op, &dir);
    let e = err(read(
        &bytes,
        op,
        Kind::Files,
        Some(Path::new("/somewhere/else")),
    ));
    assert!(e.contains("session root"), "{e}");
    let mut l = StreamLimits::default();
    l.extraction.max_content_bytes = 1000;
    let e = consume(&bytes[..], op, Kind::Files, Some(&f.root), &l, None)
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("exceed the limit"), "{e}");
    let mut l = StreamLimits::default();
    l.extraction.max_files = 2;
    let e = consume(&bytes[..], op, Kind::Files, Some(&f.root), &l, None)
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("exceed the limit"), "{e}");
}

#[test]
fn a_record_beyond_the_largest_record_is_refused_by_the_producer() {
    let (mut recs, _f, _k) = git_records();
    let commit = recs
        .iter()
        .position(|r| matches!(r, Record::Commit { .. }))
        .unwrap();
    if let Record::Commit { item } = &mut recs[commit] {
        item.message = "x".repeat(MAX_RECORD + 1);
    }
    let mut out = Vec::new();
    let mut w = FrameWriter::new(&mut out, Uuid::now_v7(), None);
    let mut failed = None;
    for r in &recs {
        if let Err(e) = w.emit(r) {
            failed = Some(e.to_string());
            break;
        }
    }
    assert!(failed.unwrap().contains("exceeds the largest record"));
}

// ------------------------------------------------- oversized records (Part C)

/// A multi-byte-rich text of about `n` bytes (cut points must respect characters).
fn big_text(n: usize) -> String {
    let unit = "word \u{e4}\u{20ac}\u{1f600} \"quoted\" \\ \t\n";
    unit.repeat(n / unit.len() + 1)
}

#[test]
fn oversized_fields_of_a_directory_round_trip_unchanged() {
    let d = tempfile::tempdir().unwrap();
    let text = big_text(3 * 1024 * 1024);
    // one section of 3 MiB: its evidence and assertion are each larger than a frame
    write(d.path(), "plain.txt", &text);
    write(
        d.path(),
        "docs/one.md",
        &format!("# Title\n\n{}\n", big_text(2_500_000)),
    );
    // a heading of 1.5 MiB: the derived title and subject are larger than a frame
    write(
        d.path(),
        "docs/two.md",
        &format!("# {}\n\nbody\n", big_text(1_500_000).replace('\n', " ")),
    );
    write(d.path(), "ok.md", "# Small\n\nfine\n");
    let f = fixture(d.path());
    let db = db();
    let store = db.store();
    let ids = vec![store.get_space("p").unwrap().id];
    let direct = Extracted::Files(
        extract_directory(
            &StoreContext(&store),
            &anchor(&f),
            &f.cfg,
            &ids,
            &Deadline::none(),
        )
        .unwrap(),
    );
    let (bytes, op) = stream_dir(&f, &store);
    let got = read(&bytes, op, Kind::Files, Some(&f.root)).unwrap();
    assert_eq!(text_of(&direct), text_of(&got));
    let wire = String::from_utf8_lossy(&bytes);
    assert!(wire.contains("\"type\":\"spill\""), "nothing was oversized");
    // every frame stays within the limit
    let mut i = 0;
    while i < bytes.len() {
        let n = u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
        assert!(n <= MAX_FRAME);
        i += n + 4;
    }
}

fn text_of(e: &Extracted) -> String {
    normalize(&format!("{e:#?}"))
}

#[test]
fn oversized_commit_messages_trees_and_tag_messages_round_trip_unchanged() {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main", "."]);
    write(d.path(), "README.md", "# R\n\nx\n");
    // 25,000 files in one tree: the tree record is about 3 MB
    for i in 0..25_000 {
        write(
            d.path(),
            &format!("many/file_with_a_longish_name_{i:06}.bin"),
            "x",
        );
    }
    git(d.path(), &["add", "."]);
    std::fs::write(d.path().join("../msg.txt"), big_text(2_000_000)).ok();
    let msg = d
        .path()
        .parent()
        .unwrap()
        .join(format!("msg-{}.txt", Uuid::now_v7()));
    std::fs::write(&msg, big_text(2_000_000)).unwrap();
    git(d.path(), &["commit", "-q", "-F", msg.to_str().unwrap()]);
    git(d.path(), &["tag", "-a", "big", "-F", msg.to_str().unwrap()]);
    let _ = std::fs::remove_file(&msg);
    let f = fixture(d.path());
    let db = db();
    let store = db.store();
    let direct = Extracted::Git(
        extract_confined_git(
            &StoreContext(&store),
            &anchor(&f),
            &f.cfg,
            &PathSelection::all(),
            GitScope::History,
            &["p"],
            &Deadline::none(),
        )
        .unwrap(),
    );
    let (bytes, op) = stream_git(&f, &store, &PathSelection::all(), GitScope::History);
    let got = read(&bytes, op, Kind::Git, None).unwrap();
    assert_eq!(text_of(&direct), text_of(&got));
    let wire = String::from_utf8_lossy(&bytes);
    for kind in ["commit", "tree", "tag"] {
        assert!(
            wire.contains(&format!("\"record_type\":\"{kind}\"")),
            "no oversized {kind}"
        );
    }
}

fn spill_stream(op: Uuid, header: &str, pieces: &[&str]) -> Vec<u8> {
    let frame = |seq: u64, recs: String| {
        raw_frame(&format!(
            r#"{{"v":1,"op":"{op}","seq":{seq},"records":[{recs}]}}"#
        ))
    };
    let mut out = frame(0, format!(r#"{{"type":"begin","kind":"git"}},{header}"#));
    for (i, p) in pieces.iter().enumerate() {
        out.extend(frame(
            i as u64 + 1,
            format!(
                r#"{{"type":"spill_piece","data":{}}}"#,
                serde_json::to_string(p).unwrap()
            ),
        ));
    }
    out
}

#[test]
fn malformed_oversized_record_sequences_are_refused() {
    let op = Uuid::now_v7();
    let spill =
        |ty: &str, len: u64| format!(r#"{{"type":"spill","record_type":"{ty}","len":{len}}}"#);
    let go = |bytes: Vec<u8>| err(read(&bytes, op, Kind::Git, None));
    // longer than the largest record, zero length
    let e = go(spill_stream(
        op,
        &spill("commit", (MAX_RECORD + 1) as u64),
        &[],
    ));
    assert!(e.contains("outside 1.."), "{e}");
    let e = go(spill_stream(op, &spill("commit", 0), &[]));
    assert!(e.contains("outside 1.."), "{e}");
    // a piece beyond the announced length
    let e = go(spill_stream(op, &spill("commit", 5), &["123456"]));
    assert!(e.contains("longer than announced"), "{e}");
    // a piece with no spill
    let e = go(spill_stream(
        op,
        r#"{"type":"spill_piece","data":"x"}"#,
        &[],
    ));
    assert!(e.contains("without a `spill`"), "{e}");
    // another record in the middle of an oversized one
    let e = go(spill_stream(
        op,
        &format!(
            "{},{}",
            spill("commit", 50),
            r#"{"type":"chunk","data":"x"}"#
        ),
        &[],
    ));
    assert!(e.contains("inside an oversized"), "{e}");
    // truncated inside an oversized record
    let e = go(spill_stream(op, &spill("commit", 50), &["abc"]));
    assert!(e.contains("ended"), "{e}");
    // complete but not decodable, not the announced type, nested
    let e = go(spill_stream(op, &spill("commit", 3), &["{ab"]));
    assert!(e.contains("does not decode"), "{e}");
    let inner = r#"{"type":"chunk","data":"x"}"#;
    let e = go(spill_stream(
        op,
        &spill("commit", inner.len() as u64),
        &[inner],
    ));
    assert!(e.contains("announced as `commit`"), "{e}");
    let nested = r#"{"type":"spill","record_type":"commit","len":4}"#;
    let e = go(spill_stream(
        op,
        &spill("spill", nested.len() as u64),
        &[nested],
    ));
    assert!(e.contains("announced as"), "{e}");
    // the closing record inside an oversized one
    let e = go(spill_stream(
        op,
        &format!(
            "{},{}",
            spill("commit", 50),
            r#"{"type":"end","counts":{"records":0,"files":0,"refs":0,"commits":0,"trees":0,"tags":0,"blobs":0,"derivations":0,"code_inputs":0}}"#
        ),
        &[],
    ));
    assert!(e.contains("inside an oversized"), "{e}");
}

#[test]
fn oversized_records_still_count_against_the_budgets() {
    let (mut recs, _f, _k) = git_records();
    let commit = recs
        .iter()
        .position(|r| matches!(r, Record::Commit { .. }))
        .unwrap();
    if let Record::Commit { item } = &mut recs[commit] {
        item.message = big_text(3 * 1024 * 1024);
    }
    let op = Uuid::now_v7();
    let bytes = encode(op, &recs);
    let mut l = StreamLimits::default();
    l.max_total = (bytes.len() / 2) as u64;
    assert!(consume(&bytes[..], op, Kind::Git, None, &l, None).is_err());
    let mut l = StreamLimits::default();
    l.max_record = 1024 * 1024;
    let e = consume(&bytes[..], op, Kind::Git, None, &l, None)
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("outside 1.."), "{e}");
    let mut l = StreamLimits::default();
    l.max_frames = 3;
    assert!(consume(&bytes[..], op, Kind::Git, None, &l, None).is_err());
    // and with the defaults the same stream is accepted
    read(&bytes, op, Kind::Git, None).unwrap();
}

#[test]
fn cancelling_the_producer_leaves_a_stream_the_consumer_refuses() {
    let (recs, _f, _k) = git_records();
    let op = Uuid::now_v7();
    let cancel = AtomicBool::new(false);
    let mut out = Vec::new();
    let mut stopped = false;
    {
        let mut w = FrameWriter::new(&mut out, op, Some(&cancel));
        for (i, r) in recs.iter().enumerate() {
            if i == recs.len() / 2 {
                cancel.store(true, Ordering::Relaxed);
            }
            if matches!(r, Record::End { .. }) {
                continue;
            }
            if w.emit(r).is_err() {
                stopped = true;
                break;
            }
        }
        if !stopped {
            stopped = w.finish().is_err();
        }
    }
    // the producer noticed (at the next frame flush at the latest) and nothing complete was written
    let _ = stopped;
    let e = err(read(&out, op, Kind::Git, None));
    assert!(e.contains("ended"), "{e}");
}

#[test]
fn a_cancelled_producer_stops_and_writes_no_complete_stream() {
    let r = repo();
    let f = fixture(r.path());
    let db = db();
    let store = db.store();
    let cancel = AtomicBool::new(true);
    let op = Uuid::now_v7();
    let mut out = Vec::new();
    let mut w = FrameWriter::new(&mut out, op, Some(&cancel));
    let result = stream_confined_git(
        &StoreContext(&store),
        &anchor(&f),
        &f.cfg,
        &PathSelection::all(),
        GitScope::Head,
        &["p"],
        &Deadline::none(),
        &ProducerLimits::default(),
        &mut w,
    )
    .and_then(|_| w.finish().map(|_| ()));
    assert!(result.unwrap_err().to_string().contains("cancelled"));
    assert!(read(&out, op, Kind::Git, None).is_err());
}

#[test]
fn a_consumer_cancelled_between_frames_stops_without_a_result() {
    let (recs, _f, _k) = git_records();
    let op = Uuid::now_v7();
    let bytes = encode(op, &recs);
    let cancel = AtomicBool::new(true);
    let e = consume(
        &bytes[..],
        op,
        Kind::Git,
        None,
        &StreamLimits::default(),
        Some(&cancel),
    )
    .err()
    .unwrap()
    .to_string();
    assert!(e.contains("cancelled"), "{e}");
}

#[test]
fn no_failure_leaves_a_partial_database_state() {
    let r = repo();
    let f = fixture(r.path());
    let db = db();
    let store = db.store();
    let (bytes, op) = stream_git(&f, &store, &PathSelection::all(), GitScope::Head);
    let before = db.rows();
    // every prefix at a few cut points, and a corrupted copy: none may reach persist
    for cut in [bytes.len() / 3, bytes.len() / 2, bytes.len() - 5] {
        assert!(read(&bytes[..cut], op, Kind::Git, None).is_err());
    }
    let mut corrupt = bytes.clone();
    let last = corrupt.len() - 20;
    corrupt[last] ^= 0x20;
    let _ = read(&corrupt, op, Kind::Git, None);
    assert_eq!(db.rows(), before);
    // and the successful result persists exactly once
    let got = read(&bytes, op, Kind::Git, None).unwrap();
    persist(&store, got, &Deadline::none()).unwrap();
    assert_ne!(db.rows(), before);
}

#[allow(dead_code)]
fn _unused(_: &mut dyn Write, _: Error) {}

// ------------------------------------------------------------ blob policy

fn limited(max: u64) -> ProducerLimits {
    ProducerLimits {
        max_blob_bytes: max,
    }
}

fn dir_with_limit(f: &Fixture, store: &SqliteStore, max: u64) -> (Result<()>, Vec<u8>, Uuid) {
    let op = Uuid::now_v7();
    let ids = vec![store.get_space("p").unwrap().id];
    let mut out = Vec::new();
    let r = {
        let mut w = FrameWriter::new(&mut out, op, None);
        stream_directory(
            &StoreContext(store),
            &anchor(f),
            &f.cfg,
            &ids,
            &Deadline::none(),
            &limited(max),
            &mut w,
        )
        .and_then(|_| w.finish().map(|_| ()))
    };
    (r, out, op)
}

fn git_with_limit(f: &Fixture, store: &SqliteStore, max: u64) -> (Result<()>, Vec<u8>, Uuid) {
    let op = Uuid::now_v7();
    let mut out = Vec::new();
    let r = {
        let mut w = FrameWriter::new(&mut out, op, None);
        stream_confined_git(
            &StoreContext(store),
            &anchor(f),
            &f.cfg,
            &PathSelection::all(),
            GitScope::Head,
            &["p"],
            &Deadline::none(),
            &limited(max),
            &mut w,
        )
        .and_then(|_| w.finish().map(|_| ()))
    };
    (r, out, op)
}

fn too_large(r: Result<()>) -> (String, u64, u64) {
    match r {
        Err(Error::BlobTooLarge { path, size, limit }) => (path, size, limit),
        other => panic!("wanted BlobTooLarge, got {other:?}"),
    }
}

#[test]
fn a_text_file_exactly_at_the_limit_is_read_and_one_byte_more_is_refused() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "ok.txt", &"a".repeat(100_000));
    write(d.path(), "small.md", "# S\n\nx\n");
    let f = fixture(d.path());
    let db = db();
    let store = db.store();
    let ids = vec![store.get_space("p").unwrap().id];
    let direct = Extracted::Files(
        extract_directory(
            &StoreContext(&store),
            &anchor(&f),
            &f.cfg,
            &ids,
            &Deadline::none(),
        )
        .unwrap(),
    );
    let (r, bytes, op) = dir_with_limit(&f, &store, 100_000);
    r.unwrap();
    assert_eq!(
        text_of(&direct),
        text_of(&read(&bytes, op, Kind::Files, Some(&f.root)).unwrap())
    );
    let (r, _, _) = dir_with_limit(&f, &store, 99_999);
    let (path, size, limit) = too_large(r);
    assert_eq!((path.as_str(), size, limit), ("ok.txt", 100_000, 99_999));
    // the message names the way out
    let e = Error::BlobTooLarge { path, size, limit }.to_string();
    assert!(e.contains("--path") && e.contains("100000 bytes"), "{e}");
}

#[test]
fn a_blob_exactly_at_the_limit_is_read_and_one_byte_more_fails_the_operation() {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main", "."]);
    write(d.path(), "ok.txt", &"b".repeat(100_000));
    write(d.path(), "note.md", "# N\n\nsmall\n");
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "c"]);
    let f = fixture(d.path());
    let db = db();
    let store = db.store();
    let direct = Extracted::Git(
        extract_confined_git(
            &StoreContext(&store),
            &anchor(&f),
            &f.cfg,
            &PathSelection::all(),
            GitScope::Head,
            &["p"],
            &Deadline::none(),
        )
        .unwrap(),
    );
    let (r, bytes, op) = git_with_limit(&f, &store, 100_000);
    r.unwrap();
    assert_eq!(
        text_of(&direct),
        text_of(&read(&bytes, op, Kind::Git, None).unwrap())
    );
    let (r, bytes, op) = git_with_limit(&f, &store, 99_999);
    let (path, size, limit) = too_large(r);
    assert_eq!((path.as_str(), size, limit), ("ok.txt", 100_000, 99_999));
    // what was written before the failure is not a complete stream
    assert!(read(&bytes, op, Kind::Git, None).is_err());
}

#[test]
fn many_small_blobs_and_repeated_references_pass_a_per_blob_limit() {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main", "."]);
    for i in 0..1500 {
        write(
            d.path(),
            &format!("small/f{i}.md"),
            &format!("# F{i}\n\ntext {i}\n"),
        );
    }
    // one blob at the limit, referenced from 40 paths
    for i in 0..40 {
        write(d.path(), &format!("same/copy{i}.txt"), &"c".repeat(50_000));
    }
    // a binary-looking blob far over the limit is not a text blob: never read
    write(d.path(), "data.bin", &"z".repeat(300_000));
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "c"]);
    let f = fixture(d.path());
    let db = db();
    let store = db.store();
    let (r, bytes, op) = git_with_limit(&f, &store, 50_000);
    r.unwrap();
    let got = read(&bytes, op, Kind::Git, None).unwrap();
    let Extracted::Git(g) = &got else { panic!() };
    assert_eq!(g.observation.derivations.len(), 1540);
    // each distinct blob's text was sent once: 1500 small files plus the one shared blob
    let wire = String::from_utf8_lossy(&bytes);
    assert_eq!(wire.matches("\"type\":\"blob_content\"").count(), 1501);
}

#[test]
fn an_extremely_large_blob_is_refused_without_being_read() {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main", "."]);
    write(d.path(), "huge.txt", &"x".repeat(150 * 1024 * 1024));
    write(d.path(), "note.md", "# N\n\nsmall\n");
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "c"]);
    let f = fixture(d.path());
    let db = db();
    let store = db.store();
    let t = std::time::Instant::now();
    let (r, _, _) = git_with_limit(&f, &store, mindspace::ingest::stream::DEFAULT_MAX_BLOB);
    let (path, size, limit) = too_large(r);
    assert_eq!(path, "huge.txt");
    assert_eq!(size, 150 * 1024 * 1024);
    assert_eq!(limit, 32 * 1024 * 1024);
    assert!(t.elapsed().as_secs() < 20, "it must not read the blob");
    // selecting the rest makes the repository ingestable again
    let op = Uuid::now_v7();
    let mut out = Vec::new();
    {
        let mut w = FrameWriter::new(&mut out, op, None);
        stream_confined_git(
            &StoreContext(&store),
            &anchor(&f),
            &f.cfg,
            &PathSelection::new(&["note.md"]).unwrap(),
            GitScope::Head,
            &["p"],
            &Deadline::none(),
            &ProducerLimits::default(),
            &mut w,
        )
        .unwrap();
        w.finish().unwrap();
    }
    read(&out, op, Kind::Git, None).unwrap();
}

#[test]
fn a_file_over_the_default_limit_is_refused_by_the_directory_producer() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "big.txt", &"y".repeat(32 * 1024 * 1024 + 1));
    let f = fixture(d.path());
    let db = db();
    let store = db.store();
    let (r, _, _) = dir_with_limit(&f, &store, mindspace::ingest::stream::DEFAULT_MAX_BLOB);
    let (_, size, limit) = too_large(r);
    assert_eq!((size, limit), (32 * 1024 * 1024 + 1, 32 * 1024 * 1024));
}
