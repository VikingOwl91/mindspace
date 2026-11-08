//! M7.3b stream gate: repository identity through the stream, and the structural-parse policy in
//! the in-process Git path (the stream paths are covered by `structural_policy`).

#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

use mindspace::core::{IngestOutcome, Uuid};
use mindspace::ingest::confined::{extract_confined_git, Anchor};
use mindspace::ingest::deadline::Deadline;
use mindspace::ingest::extract::{Extracted, StoreContext};
use mindspace::ingest::outline::PathSelection;
use mindspace::ingest::stream::{
    consume, stream_confined_git, FrameWriter, Kind, ProducerLimits, StreamLimits,
};
use mindspace::ingest::GitScope;
use mindspace::mcp::McpConfig;
use mindspace::storage::Store;
use mindspace::{initialize, open_existing};

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

fn repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main", "."]);
    write(d.path(), "a.md", "# A\n\nx\n");
    write(d.path(), "src/lib.rs", "pub fn one() {}\n");
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "one"]);
    d
}

fn streamed_id(root: &Path) -> Uuid {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("m.db");
    initialize(&db).unwrap();
    let store = open_existing(&db).unwrap();
    store.create_space("p", "").unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    let cfg = McpConfig::new(&[&root]).unwrap();
    let anchor = Anchor::open(&root, Some(&root), &cfg, true).unwrap();
    let op = Uuid::now_v7();
    let mut out = Vec::new();
    {
        let mut w = FrameWriter::new(&mut out, op, None);
        stream_confined_git(
            &StoreContext(&store),
            &anchor,
            &cfg,
            &PathSelection::all(),
            GitScope::Head,
            &["p"],
            &Deadline::none(),
            &ProducerLimits::default(),
            &mut w,
        )
        .unwrap();
        w.finish().unwrap();
    }
    let (ex, _) = consume(
        &out[..],
        op,
        Kind::Git,
        None,
        &StreamLimits::default(),
        None,
    )
    .unwrap();
    let Extracted::Git(g) = ex else { panic!() };
    g.observation.repository_id
}

fn inprocess_id(root: &Path) -> Uuid {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("m.db");
    initialize(&db).unwrap();
    let store = open_existing(&db).unwrap();
    store.create_space("p", "").unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    let cfg = McpConfig::new(&[&root]).unwrap();
    let anchor = Anchor::open(&root, Some(&root), &cfg, true).unwrap();
    extract_confined_git(
        &StoreContext(&store),
        &anchor,
        &cfg,
        &PathSelection::all(),
        GitScope::Head,
        &["p"],
        &Deadline::none(),
    )
    .unwrap()
    .observation
    .repository_id
}

fn marker(root: &Path) -> Option<String> {
    std::fs::read_to_string(root.join(".git/mindspace-repository-id"))
        .ok()
        .map(|s| s.trim().to_string())
}

#[test]
fn the_stream_creates_reads_and_keeps_the_repository_identity() {
    let r = repo();
    assert!(
        marker(r.path()).is_none(),
        "a fresh repository has no marker"
    );
    let first = streamed_id(r.path());
    assert_eq!(
        marker(r.path()).as_deref(),
        Some(first.to_string().as_str()),
        "the producer wrote the marker"
    );
    // streamed twice, and in process: the same identity
    assert_eq!(streamed_id(r.path()), first);
    assert_eq!(inprocess_id(r.path()), first);
    // a moved copy of the repository keeps it (the marker travels with .git)
    let moved = tempfile::tempdir().unwrap();
    let target = moved.path().join("elsewhere");
    let st = Command::new("cp")
        .arg("-a")
        .arg(r.path())
        .arg(&target)
        .status()
        .unwrap();
    assert!(st.success());
    assert_eq!(streamed_id(&target), first);
    // a fresh clone starts without one and gets its own
    let clone = moved.path().join("clone");
    git(
        moved.path(),
        &[
            "clone",
            "-q",
            r.path().to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    assert!(marker(&clone).is_none());
    let other = streamed_id(&clone);
    assert_ne!(other, first);
    assert_eq!(marker(&clone).as_deref(), Some(other.to_string().as_str()));
}

#[test]
fn the_in_process_git_path_applies_the_structural_policy() {
    let r = repo();
    write(
        r.path(),
        "src/deep.rs",
        &format!("{}{}\n", "mod m { ".repeat(300), "}".repeat(300)),
    );
    git(r.path(), &["add", "."]);
    git(r.path(), &["commit", "-q", "-m", "deep"]);
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("m.db");
    initialize(&db).unwrap();
    let store = open_existing(&db).unwrap();
    store.create_space("p", "").unwrap();
    let IngestOutcome::Git(rep) = mindspace::ingest::ingest_path(&store, r.path(), &["p"]).unwrap()
    else {
        panic!()
    };
    assert_eq!(
        (rep.code.skipped, rep.code.parsed),
        (1, 1),
        "{:?}",
        rep.code
    );
    assert!(rep.code.skip_reasons.contains_key("p1:entity_depth>256"));
}
