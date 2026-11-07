//! Confinement of MCP ingest to the authorized roots (MIND-18).
//!
//! These tests state SECURITY INVARIANTS as observable outcomes and say nothing
//! about how a fix achieves them (no error texts, no mechanism):
//!
//!  I1 no byte of a file outside the allowed roots reaches the database or a
//!     tool response (outline included);
//!  I2 ingest reads only from locations governed by an rw root (git dir,
//!     common dir, object stores, work tree);
//!  I3 the only filesystem write of an ingest is the identity marker, inside an
//!     rw root;
//!  I4 read-only areas, also nested ones, are neither ingested nor changed;
//!  liveness: a legitimate ingest still works in the same session, so
//!     "always refuse" is not a solution.
//!
//! Oracles: (1) raw byte scan of the database file and its WAL for a unique
//! token per forbidden location, code AND markdown (a `code_search` oracle is
//! blind to markdown leaks, which was verified); (2) the tool responses;
//! (3) a content fingerprint of every tree that must not change.
//!
//! RUNNING THEM. All tests run in `cargo test`, with few race attempts (a smoke
//! check). The mandatory gate is the security profile, `scripts/security-profile.sh`,
//! which runs the full number of attempts, in both modes (kernel sandbox plus
//! application layer, and the application layer alone with
//! `MINDSPACE_TEST_MCP_ARGS=--insecure-no-sandbox`). Everything uses temporary
//! directories; no real repository is touched.

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Race attempts per test. `MINDSPACE_RACE_ATTEMPTS` overrides it; the security
/// profile sets 150. The default is a smoke check only: calibrated against the
/// code as of MIND-4 (12 runs of each of the 9 race tests, 108 samples) the first
/// leak came after 1 to 67 attempts, mean about 15 to 20, roughly geometric, so
/// 40 attempts miss a regression in about one test run in ten, and 150 in about
/// one in a thousand. Passing is deterministic for a protected implementation:
/// any leak fails.
const DEFAULT_ATTEMPTS: usize = 40;
const REPO_FILES: usize = 8;
/// Files in a plain-directory fixture. This matters: the directory walk checks
/// an entry and the file is read later, and the window between the two grows
/// with the number of files before it. Measured against the unfixed code, 20
/// files give a first leak after 2 to 67 attempts (mean about 15), 6 files
/// after 2 to 131 (mean about 45). Keep the calibrated size.
const DIR_FILES: usize = 20;
const DIR_TARGET: &str = "n010.md";

fn attempts() -> usize {
    std::env::var("MINDSPACE_RACE_ATTEMPTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_ATTEMPTS)
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Server {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    next_id: u64,
    /// Every tool response text seen, for the response oracle.
    seen: Vec<String>,
}

impl Server {
    fn start(db: &Path, specs: &[String]) -> Self {
        // MINDSPACE_TEST_MCP_BIN lets a spike run the same invariants against a
        // wrapped binary (for example one started inside a sandbox).
        let bin = std::env::var("MINDSPACE_TEST_MCP_BIN")
            .unwrap_or_else(|_| env!("CARGO_BIN_EXE_mind").to_string());
        let mut cmd = Command::new(bin);
        cmd.env("MINDSPACE_DB", db)
            .env_remove("MINDSPACE_MCP_ALLOW_ROOTS")
            .arg("mcp");
        for s in specs {
            cmd.arg("--allow-root").arg(s);
        }
        // MINDSPACE_TEST_MCP_ARGS adds server flags, for example to run the same
        // invariants with `--insecure-no-sandbox` (the application layer alone).
        if let Ok(extra) = std::env::var("MINDSPACE_TEST_MCP_ARGS") {
            cmd.args(extra.split_whitespace());
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut s = Self {
            child,
            stdin,
            out,
            next_id: 1,
            seen: Vec::new(),
        };
        let init = s.rpc(
            "initialize",
            json!({"protocolVersion":"2025-06-18","capabilities":{},
                   "clientInfo":{"name":"t","version":"0"}}),
        );
        assert!(init.get("result").is_some(), "server did not start: {init}");
        writeln!(
            s.stdin,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        s
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc":"2.0","id":self.next_id,"method":method,"params":params})
        )
        .unwrap();
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or(Value::Null)
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        self.start_call(name, args);
        self.finish_call()
    }

    /// Sends a request without waiting for the answer, so a test can act while
    /// the server works. Pair with [`finish_call`](Self::finish_call).
    fn start_call(&mut self, name: &str, args: Value) {
        self.next_id += 1;
        let req = json!({"jsonrpc":"2.0","id":self.next_id,"method":"tools/call",
                         "params":{"name": name, "arguments": args}});
        writeln!(self.stdin, "{req}").unwrap();
    }

    fn finish_call(&mut self) -> Value {
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        let r = serde_json::from_str::<Value>(&line).unwrap_or(Value::Null)["result"].clone();
        self.seen.push(r.to_string());
        r
    }

    /// True when the call succeeded. Whether a refusal is right is judged by
    /// the invariants, never by this value alone.
    fn ingest(&mut self, path: &Path) -> bool {
        let r = self.call(
            "repo_ingest",
            json!({"path": path.display().to_string(), "spaces": ["p"]}),
        );
        r["isError"] == false
    }

    fn outline(&mut self, path: &Path) {
        self.call("repo_outline", json!({"path": path.display().to_string()}));
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        // `git commit` spawns a detached `git maintenance run --auto`, which creates and removes
        // objects/maintenance.lock a few milliseconds later. Fixture setup must not leave that behind,
        // or a fingerprint taken right after it races with it (seen on a loaded CI runner).
        .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// A git repository with code, a markdown document and a directory whose NAME
/// is the token (for the outline oracle). Every location gets its own tokens.
fn repo(path: &Path, tag: &str) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q", "-b", "main"]);
    for i in 0..REPO_FILES {
        std::fs::write(
            path.join(format!("f{i}.rs")),
            format!("pub fn {tag}_CODE_{i}() {{}}\n"),
        )
        .unwrap();
    }
    std::fs::write(path.join("doc.md"), format!("# Doc\n\n{tag}_DOC text\n")).unwrap();
    // The outline lists directories, so the name token lives in a directory.
    std::fs::create_dir_all(path.join(format!("{tag}_NAME"))).unwrap();
    std::fs::write(path.join(format!("{tag}_NAME/inner.md")), "# Inner\n").unwrap();
    git(path, &["add", "-A"]);
    git(path, &["commit", "-q", "-m", "init"]);
}

/// A plain directory of markdown files plus one secret file.
fn docs(dir: &Path, count: usize) {
    std::fs::create_dir_all(dir).unwrap();
    for i in 0..count {
        std::fs::write(
            dir.join(format!("n{i:03}.md")),
            format!("# Note {i}\n\nnote {i}\n"),
        )
        .unwrap();
    }
}

fn tokens(tag: &str) -> Vec<String> {
    vec![
        format!("{tag}_CODE_"),
        format!("{tag}_DOC"),
        format!("{tag}_NAME"),
    ]
}

// ---------------------------------------------------------------------------
// Oracles
// ---------------------------------------------------------------------------

fn db_has(db: &Path, token: &str) -> bool {
    ["", "-wal", "-shm"].iter().any(|s| {
        std::fs::read(format!("{}{s}", db.display()))
            .map(|b| b.windows(token.len()).any(|w| w == token.as_bytes()))
            .unwrap_or(false)
    })
}

type Fingerprint = BTreeMap<String, String>;

/// Relative path -> content hash (symlinks recorded by target, not followed).
fn fingerprint(dir: &Path) -> Fingerprint {
    fn walk(base: &Path, dir: &Path, out: &mut Fingerprint) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().to_string();
            let md = std::fs::symlink_metadata(&p).unwrap();
            if md.file_type().is_symlink() {
                out.insert(
                    rel,
                    format!("-> {}", std::fs::read_link(&p).unwrap().display()),
                );
            } else if md.is_dir() {
                out.insert(rel.clone(), "dir".into());
                walk(base, &p, out);
            } else {
                let bytes = std::fs::read(&p).unwrap_or_default();
                let hex: String = Sha256::digest(&bytes)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                out.insert(rel, hex);
            }
        }
    }
    let mut out = Fingerprint::new();
    walk(dir, dir, &mut out);
    out
}

fn changed(before: &Fingerprint, after: &Fingerprint) -> Vec<String> {
    let mut d: Vec<String> = after
        .iter()
        .filter(|(k, v)| before.get(*k) != Some(v))
        .map(|(k, _)| format!("changed/new: {k}"))
        .collect();
    d.extend(
        before
            .keys()
            .filter(|k| !after.contains_key(*k))
            .map(|k| format!("removed: {k}")),
    );
    d
}

// ---------------------------------------------------------------------------
// The world: an rw root, optional ro areas, and forbidden locations
// ---------------------------------------------------------------------------

struct World {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
    ro_root: PathBuf,
    db: PathBuf,
    specs: Vec<String>,
}

impl World {
    /// `root` is rw and holds a legitimate repository `good` (liveness).
    /// `ro_root` is a separate read-only root. `outside` is under no root.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().canonicalize().unwrap();
        let (root, outside, ro_root) = (p.join("root"), p.join("outside"), p.join("ro"));
        for d in [&root, &outside, &ro_root] {
            std::fs::create_dir_all(d).unwrap();
        }
        repo(&root.join("good"), "GOOD");
        // The database gets a directory of its own: a sandbox must open the
        // whole DB directory (the -wal and -shm files live beside the DB), so it
        // must not share a parent with anything that has to stay unreadable.
        std::fs::create_dir_all(p.join("state")).unwrap();
        let db = p.join("state/mind.db");
        for args in [vec!["init"], vec!["space", "create", "p"]] {
            let o = Command::new(env!("CARGO_BIN_EXE_mind"))
                .env("MINDSPACE_DB", &db)
                .args(args)
                .output()
                .unwrap();
            assert!(o.status.success());
        }
        let specs = vec![
            format!("{}:rw", root.display()),
            format!("{}:ro", ro_root.display()),
        ];
        World {
            _tmp: tmp,
            root,
            outside,
            ro_root,
            db,
            specs,
        }
    }

    fn server(&self) -> Server {
        Server::start(&self.db, &self.specs)
    }

    /// Liveness: the legitimate repository can be ingested and is searchable.
    fn assert_liveness(&self, s: &mut Server) {
        assert!(
            s.ingest(&self.root.join("good")),
            "a legitimate ingest was refused (liveness)"
        );
        assert!(
            db_has(&self.db, "GOOD_CODE_"),
            "legitimate content is missing from the database"
        );
    }

    /// I1/I2/I4 on the database and the responses, I3/I4 on the filesystem.
    fn assert_confined(
        &self,
        s: &Server,
        forbidden: &[String],
        untouched: &[(&Path, &Fingerprint)],
    ) {
        for t in forbidden {
            assert!(
                !db_has(&self.db, t),
                "forbidden content `{t}` reached the database"
            );
            assert!(
                !s.seen.iter().any(|r| r.contains(t.as_str())),
                "forbidden content `{t}` appeared in a tool response"
            );
        }
        for (dir, before) in untouched {
            let delta = changed(before, &fingerprint(dir));
            assert!(
                delta.is_empty(),
                "{} was modified ({} entries), first: {:?}",
                dir.display(),
                delta.len(),
                &delta[..delta.len().min(4)]
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Attacker: toggles a redirection while ingests run
// ---------------------------------------------------------------------------

struct Attacker {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Attacker {
    fn start(on: impl Fn() -> bool + Send + 'static, off: impl Fn() + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                if on() {
                    std::thread::sleep(Duration::from_millis(4));
                    off();
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            off();
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for Attacker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// `link` is turned into a symlink to `target` and back (the original is parked).
fn leaf_swap(
    link: PathBuf,
    target: PathBuf,
) -> (
    impl Fn() -> bool + Send + 'static,
    impl Fn() + Send + 'static,
) {
    let parked = PathBuf::from(format!("{}.parked", link.display()));
    let (l1, p1, t1) = (link.clone(), parked.clone(), target);
    let on = move || std::fs::rename(&l1, &p1).is_ok() && symlink(&t1, &l1).is_ok();
    let off = move || {
        if std::fs::symlink_metadata(&link)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            let _ = std::fs::remove_file(&link);
        }
        if parked.exists() && !link.exists() {
            let _ = std::fs::rename(&parked, &link);
        }
    };
    (on, off)
}

/// Runs `ATTEMPTS` ingests of `victim` under attack; stops at the first leak.
/// Returns the attempt of the first leak, if any.
fn race(w: &World, s: &mut Server, victim: &Path, forbidden: &[String]) -> Option<usize> {
    for n in 1..=attempts() {
        s.ingest(victim);
        if forbidden.iter().any(|t| db_has(&w.db, t)) {
            return Some(n);
        }
    }
    None
}

/// An attacker variant that really redirects: polls `probe` until it has seen
/// the redirection `needed` times, or fails after a timeout.
fn assert_attacker_effective(
    label: &str,
    on: impl Fn() -> bool + Send + 'static,
    off: impl Fn() + Send + 'static,
    probe: impl Fn() -> bool,
) {
    let _a = Attacker::start(on, off);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = 0;
    while Instant::now() < deadline && seen < 3 {
        if probe() {
            seen += 1;
            std::thread::sleep(Duration::from_millis(6));
        }
        std::thread::yield_now();
    }
    assert!(seen >= 3, "attacker `{label}` never redirected ({seen})");
}

// ===========================================================================
// Controls (run normally)
// ===========================================================================

#[test]
fn control_legitimate_ingest_and_outline_work() {
    let w = World::new();
    let mut s = w.server();
    w.assert_liveness(&mut s);
    s.outline(&w.root.join("good"));
    assert!(
        s.seen.last().unwrap().contains("GOOD_NAME"),
        "outline of a legitimate repo is empty"
    );
}

#[test]
fn control_oracles_detect_a_leak() {
    // The oracles must be able to fail: ingest a repository that IS allowed and
    // check that the scan finds its tokens and the fingerprint sees changes.
    let w = World::new();
    let before = fingerprint(&w.root);
    let mut s = w.server();
    assert!(s.ingest(&w.root.join("good")));
    assert!(
        tokens("GOOD").iter().any(|t| db_has(&w.db, t)),
        "byte scan missed present content"
    );
    assert!(!db_has(&w.db, "NEVER_PRESENT_TOKEN"));
    assert!(
        !changed(&before, &fingerprint(&w.root)).is_empty(),
        "the marker write must be visible to the fingerprint"
    );
}

#[test]
fn control_attackers_really_redirect() {
    let w = World::new();
    repo(&w.outside, "OUT");
    // R1 leaf, R2 ancestor, R3 .git entry, R4 alternates file, R5 file in a directory.
    repo(&w.root.join("work"), "W");
    let (on, off) = leaf_swap(w.root.join("work"), w.outside.clone());
    let (r1, root) = (w.root.join("work"), w.outside.canonicalize().unwrap());
    assert_attacker_effective("leaf", on, off, move || {
        r1.canonicalize().ok().as_deref() == Some(root.as_path())
    });

    std::fs::create_dir_all(w.root.join("a")).unwrap();
    std::fs::rename(w.root.join("work"), w.root.join("a/work")).unwrap();
    std::fs::create_dir_all(w.outside.join("work")).unwrap();
    let (on, off) = leaf_swap(w.root.join("a"), w.outside.clone());
    let (r2, outside) = (w.root.join("a"), w.outside.canonicalize().unwrap());
    assert_attacker_effective("ancestor", on, off, move || {
        r2.canonicalize().ok().as_deref() == Some(outside.as_path())
    });

    let (on, off) = leaf_swap(w.root.join("a/work/.git"), w.outside.join(".git"));
    let (r3, outside_git) = (
        w.root.join("a/work/.git"),
        w.outside.join(".git").canonicalize().unwrap(),
    );
    assert_attacker_effective(".git entry", on, off, move || {
        r3.canonicalize().ok().as_deref() == Some(outside_git.as_path())
    });

    docs(&w.root.join("docs"), 5);
    std::fs::write(w.outside.join("secret.md"), "# S\n").unwrap();
    let (on, off) = leaf_swap(w.root.join("docs/n002.md"), w.outside.join("secret.md"));
    let (r5, secret) = (
        w.root.join("docs/n002.md"),
        w.outside.join("secret.md").canonicalize().unwrap(),
    );
    assert_attacker_effective("file", on, off, move || {
        r5.canonicalize().ok().as_deref() == Some(secret.as_path())
    });
}

// ===========================================================================
// Static scenarios: no race, no write access to the root needed
// ===========================================================================

/// Runs one static scenario: `build(w)` plants the pointer; the ingest and the
/// outline of `root/work` are attempted; invariants must hold either way.
fn static_scenario(build: impl FnOnce(&World), forbidden_tag: &str) {
    let w = World::new();
    repo(&w.outside, forbidden_tag);
    build(&w);
    let outside_before = fingerprint(&w.outside);
    let forbidden = tokens(forbidden_tag);
    let mut s = w.server();
    s.ingest(&w.root.join("work"));
    s.outline(&w.root.join("work"));
    w.assert_confined(&s, &forbidden, &[(&w.outside, &outside_before)]);
    w.assert_liveness(&mut s);
}

#[test]
fn s1_git_entry_symlink_to_outside_repo() {
    static_scenario(
        |w| {
            std::fs::create_dir_all(w.root.join("work")).unwrap();
            symlink(w.outside.join(".git"), w.root.join("work/.git")).unwrap();
        },
        "OUT1",
    );
}

#[test]
fn s2_gitdir_pointer_file_to_outside_repo() {
    static_scenario(
        |w| {
            std::fs::create_dir_all(w.root.join("work")).unwrap();
            std::fs::write(
                w.root.join("work/.git"),
                format!("gitdir: {}/.git\n", w.outside.display()),
            )
            .unwrap();
        },
        "OUT2",
    );
}

#[test]
fn s3_alternates_pointing_to_outside_objects() {
    static_scenario(
        |w| {
            let work = w.root.join("work");
            std::fs::create_dir_all(&work).unwrap();
            git(&work, &["init", "-q", "-b", "main"]);
            let head = git(&w.outside, &["rev-parse", "HEAD"]);
            std::fs::write(
                work.join(".git/objects/info/alternates"),
                format!("{}/.git/objects\n", w.outside.display()),
            )
            .unwrap();
            std::fs::write(work.join(".git/refs/heads/main"), format!("{head}\n")).unwrap();
        },
        "OUT3",
    );
}

#[test]
fn s4_linked_worktree_whose_main_repo_is_outside_all_roots() {
    // Decision: linked worktrees with an external common dir are refused.
    static_scenario(
        |w| {
            git(
                &w.outside,
                &[
                    "worktree",
                    "add",
                    "-q",
                    w.root.join("work").to_str().unwrap(),
                    "-b",
                    "wt",
                ],
            );
        },
        "OUT4",
    );
}

#[test]
fn s5_git_entry_pointing_into_a_read_only_root() {
    let w = World::new();
    repo(&w.ro_root.join("repo"), "RO5");
    std::fs::create_dir_all(w.root.join("work")).unwrap();
    symlink(w.ro_root.join("repo/.git"), w.root.join("work/.git")).unwrap();
    let ro_before = fingerprint(&w.ro_root);
    let mut s = w.server();
    s.ingest(&w.root.join("work"));
    // No outline here: an outline may show the content of a read-only root, that is
    // what such a root is for. The invariant is that nothing is INGESTED from it.
    w.assert_confined(&s, &tokens("RO5"), &[(&w.ro_root, &ro_before)]);
    w.assert_liveness(&mut s);
}

#[test]
fn s6_git_entry_pointing_into_a_nested_read_only_area_of_the_same_rw_root() {
    let mut w = World::new();
    let protected = w.root.join("protected");
    repo(&protected.join("repo"), "NEST6");
    std::fs::create_dir_all(w.root.join("work")).unwrap();
    symlink(protected.join("repo/.git"), w.root.join("work/.git")).unwrap();
    w.specs.push(format!("{}:ro", protected.display()));
    let before = fingerprint(&protected);
    let mut s = w.server();
    s.ingest(&w.root.join("work"));
    w.assert_confined(&s, &tokens("NEST6"), &[(&protected, &before)]);
    w.assert_liveness(&mut s);
}

/// Adversarial review (MIND-18): more places inside a repository from which git
/// reads, each made to point outside. They hold under the kernel sandbox, and the
/// application layer alone must hold them too, so they run in both modes.
fn symlink_every_object_dir(w: &World, work: &Path) {
    // Loose objects of the outside repository, reached through links inside the
    // repository that is allowed. The ref names an outside commit.
    let outside_objects = w.outside.join(".git/objects");
    for e in std::fs::read_dir(&outside_objects).unwrap().flatten() {
        let name = e.file_name();
        let n = name.to_string_lossy();
        if n.len() == 2 {
            symlink(e.path(), work.join(".git/objects").join(&*n)).unwrap();
        }
    }
    let head = git(&w.outside, &["rev-parse", "HEAD"]);
    std::fs::write(work.join(".git/refs/heads/main"), format!("{head}\n")).unwrap();
}

#[test]
fn s8_object_directories_inside_the_git_dir_that_are_links_to_outside() {
    static_scenario(
        |w| {
            let work = w.root.join("work");
            std::fs::create_dir_all(&work).unwrap();
            git(&work, &["init", "-q", "-b", "main"]);
            symlink_every_object_dir(w, &work);
        },
        "OUT8",
    );
}

#[test]
fn s9_a_relative_gitdir_pointer_that_climbs_out_of_the_root() {
    static_scenario(
        |w| {
            std::fs::create_dir_all(w.root.join("work")).unwrap();
            // root/work/.git -> ../../outside/.git
            std::fs::write(w.root.join("work/.git"), "gitdir: ../../outside/.git\n").unwrap();
        },
        "OUT9",
    );
}

#[test]
fn s10_a_commondir_file_that_points_outside() {
    static_scenario(
        |w| {
            // A linked worktree of a repository inside the root, then its
            // `commondir` is rewritten to the outside repository.
            repo(&w.root.join("main"), "MAIN10");
            git(
                &w.root.join("main"),
                &[
                    "worktree",
                    "add",
                    "-q",
                    w.root.join("work").to_str().unwrap(),
                    "-b",
                    "wt",
                ],
            );
            let gitdir_file = std::fs::read_to_string(w.root.join("work/.git")).unwrap();
            let gitdir = gitdir_file
                .trim()
                .trim_start_matches("gitdir:")
                .trim()
                .to_string();
            // The outside repository has the branch the worktree's HEAD names, so a
            // redirect would succeed instead of failing on a missing branch.
            git(&w.outside, &["branch", "wt"]);
            std::fs::write(
                Path::new(&gitdir).join("commondir"),
                format!("{}/.git\n", w.outside.display()),
            )
            .unwrap();
        },
        "OUT10",
    );
}

/// The identity marker is only ever created exclusively and never through a
/// link: a link planted under its name must not make the ingest write to the
/// link's target.
#[test]
fn s11_a_symlink_planted_under_the_identity_marker_name() {
    let w = World::new();
    repo(&w.outside, "OUT11");
    // A DANGLING link: a careless "read, then create" would write through it and
    // create the target outside the roots.
    let target = w.outside.join("created-through-the-marker-link.txt");
    repo(&w.root.join("work"), "W11");
    symlink(&target, w.root.join("work/.git/mindspace-repository-id")).unwrap();
    let outside_before = fingerprint(&w.outside);
    let mut s = w.server();
    s.ingest(&w.root.join("work"));
    w.assert_confined(&s, &tokens("OUT11"), &[(&w.outside, &outside_before)]);
    assert!(
        !target.exists(),
        "the identity marker was written through a link"
    );
    w.assert_liveness(&mut s);
}

/// A FIFO put in place of a listed file, after the walk saw a regular file, must
/// not hang the ingest (opening it for reading blocks until a writer appears).
#[test]
fn r8_a_fifo_swapped_in_during_a_directory_ingest_does_not_hang() {
    let w = World::new();
    docs(&w.root.join("docs"), DIR_FILES);
    let file = w.root.join("docs").join(DIR_TARGET);
    let parked = PathBuf::from(format!("{}.parked", file.display()));
    let (f1, p1) = (file.clone(), parked.clone());
    let on = move || {
        std::fs::rename(&f1, &p1).is_ok() && {
            let c = std::ffi::CString::new(f1.to_str().unwrap()).unwrap();
            unsafe { libc::mkfifo(c.as_ptr(), 0o600) == 0 }
        }
    };
    let off = move || {
        if std::fs::symlink_metadata(&file)
            .map(|m| !m.is_file())
            .unwrap_or(false)
        {
            let _ = std::fs::remove_file(&file);
        }
        if parked.exists() && !file.exists() {
            let _ = std::fs::rename(&parked, &file);
        }
    };
    let mut s = w.server();
    // Watchdog: a hang must fail the test instead of blocking the suite.
    let pid = s.child.id();
    let done = Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    let dog = std::thread::spawn(move || {
        let start = Instant::now();
        while !flag.load(Ordering::Relaxed) {
            if start.elapsed() > Duration::from_secs(60) {
                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    });
    {
        let _attacker = Attacker::start(on, off);
        for _ in 0..attempts().min(60) {
            s.ingest(&w.root.join("docs"));
            if dog.is_finished() {
                break;
            }
        }
    }
    done.store(true, Ordering::Relaxed);
    assert!(!dog.join().unwrap(), "the ingest hung on a FIFO");
    w.assert_liveness(&mut s);
}

/// A race with a CHOSEN time: the ingest starts clean (so every check at its
/// start sees an honest repository), then, `delay` milliseconds later, `install`
/// puts the redirection in place and it STAYS until the answer is in. A random
/// swap rarely lands after the checks; this does it on purpose, which is what
/// separates "checked once" from "bound to what was checked".
fn late_swap(
    w: &World,
    victim: &Path,
    forbidden: &[String],
    install: &dyn Fn() -> bool,
    undo: &dyn Fn(),
    step_us: u64,
    span: u64,
) -> Option<usize> {
    let mut s = w.server();
    for n in 1..=attempts() {
        s.start_call(
            "repo_ingest",
            json!({"path": victim.display().to_string(), "spaces": ["p"]}),
        );
        std::thread::sleep(Duration::from_micros((n as u64 * 7 % span) * step_us));
        install();
        s.finish_call();
        undo();
        if forbidden.iter().any(|t| db_has(&w.db, t)) {
            return Some(n);
        }
    }
    None
}

#[test]
fn r3b_the_git_entry_is_swapped_after_the_ingest_started_and_stays_swapped() {
    let w = World::new();
    repo(&w.outside, "OUTR3B");
    repo(&w.root.join("work"), "W3B");
    let outside_before = fingerprint(&w.outside);
    let (on, off) = leaf_swap(w.root.join("work/.git"), w.outside.join(".git"));
    let forbidden = tokens("OUTR3B");
    let leak = late_swap(&w, &w.root.join("work"), &forbidden, &on, &off, 3000, 15);
    assert_eq!(
        leak, None,
        "outside content reached the database (attempt {leak:?})"
    );
    let mut s = w.server();
    w.assert_confined(&s, &forbidden, &[(&w.outside, &outside_before)]);
    w.assert_liveness(&mut s);
}

/// The same with an alternates file. Under the kernel sandbox this must hold. With
/// the application layer alone it cannot be excluded (the file can be created
/// after the check), so the security profile runs it only in sandbox mode and
/// reports the rate for the other mode.
#[test]
fn r4b_an_alternates_file_appears_after_the_ingest_started_and_stays() {
    let w = World::new();
    repo(&w.outside, "OUTR4B");
    let work = w.root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    let head = git(&w.outside, &["rev-parse", "HEAD"]);
    std::fs::write(work.join(".git/refs/heads/main"), format!("{head}\n")).unwrap();
    let alt = work.join(".git/objects/info/alternates");
    let content = format!("{}/.git/objects\n", w.outside.display());
    let (alt2, c2) = (alt.clone(), content.clone());
    let forbidden = tokens("OUTR4B");
    let leak = late_swap(
        &w,
        &work,
        &forbidden,
        &move || std::fs::write(&alt2, &c2).is_ok(),
        &move || {
            let _ = std::fs::remove_file(&alt);
        },
        250,
        60,
    );
    assert_eq!(
        leak, None,
        "outside content reached the database (attempt {leak:?})"
    );
}

/// Positive scenario: a linked worktree whose main repo is inside the roots
/// must keep working, with the marker as the only change.
#[test]
fn s7_linked_worktree_with_main_repo_inside_the_root_still_works() {
    let w = World::new();
    repo(&w.root.join("main"), "MAIN7");
    git(
        &w.root.join("main"),
        &[
            "worktree",
            "add",
            "-q",
            w.root.join("wt").to_str().unwrap(),
            "-b",
            "wt",
        ],
    );
    let mut s = w.server();
    assert!(
        s.ingest(&w.root.join("wt")),
        "a worktree inside the roots must be ingestable"
    );
    assert!(db_has(&w.db, "MAIN7_CODE_"), "worktree content missing");
    w.assert_liveness(&mut s);
}

// ===========================================================================
// Races
// ===========================================================================

fn race_scenario(
    forbidden_tag: &str,
    outside_repo: bool,
    plan: impl FnOnce(&World) -> (PathBuf, Box<dyn Fn() -> bool + Send>, Box<dyn Fn() + Send>),
    mut_world: impl FnOnce(&mut World),
) {
    let mut w = World::new();
    if outside_repo {
        repo(&w.outside, forbidden_tag);
    }
    mut_world(&mut w);
    let (victim, on, off) = plan(&w);
    // After the fixtures exist, before the server starts: only what the ingest
    // does may count as a change.
    let outside_before = fingerprint(&w.outside);
    let ro_before = fingerprint(&w.ro_root);
    let forbidden = tokens(forbidden_tag);
    let mut s = w.server();
    w.assert_liveness(&mut s);
    let leak = {
        let _attacker = Attacker::start(on, off);
        race(&w, &mut s, &victim, &forbidden)
    };
    assert_eq!(
        leak, None,
        "forbidden content reached the database at attempt {leak:?}"
    );
    w.assert_confined(
        &s,
        &forbidden,
        &[(&w.outside, &outside_before), (&w.ro_root, &ro_before)],
    );
    w.assert_liveness(&mut s);
}

type Plan = (PathBuf, Box<dyn Fn() -> bool + Send>, Box<dyn Fn() + Send>);

fn boxed(victim: PathBuf, link: PathBuf, target: PathBuf) -> Plan {
    let (on, off) = leaf_swap(link, target);
    (victim, Box::new(on), Box::new(off))
}

#[test]
fn r1_leaf_swap_of_the_authorized_directory_to_an_outside_repo() {
    race_scenario(
        "OUTR1",
        true,
        |w| {
            repo(&w.root.join("work"), "W1");
            boxed(w.root.join("work"), w.root.join("work"), w.outside.clone())
        },
        |_| {},
    );
}

#[test]
fn r2_swap_of_an_ancestor_directory() {
    race_scenario(
        "OUTR2",
        false,
        |w| {
            std::fs::create_dir_all(w.root.join("a")).unwrap();
            repo(&w.root.join("a/work"), "W2");
            // The outside tree has a repository at the same relative path, so the
            // swapped ancestor still resolves to something that looks right.
            let holder = w.outside.join("holder");
            repo(&holder.join("work"), "OUTR2");
            boxed(w.root.join("a/work"), w.root.join("a"), holder)
        },
        |_| {},
    );
}

#[test]
fn r3_swap_of_the_git_entry_inside_the_authorized_directory() {
    race_scenario(
        "OUTR3",
        true,
        |w| {
            repo(&w.root.join("work"), "W3");
            boxed(
                w.root.join("work"),
                w.root.join("work/.git"),
                w.outside.join(".git"),
            )
        },
        |_| {},
    );
}

#[test]
fn r4_alternates_file_appearing_during_the_ingest() {
    race_scenario(
        "OUTR4",
        true,
        |w| {
            let work = w.root.join("work");
            std::fs::create_dir_all(&work).unwrap();
            git(&work, &["init", "-q", "-b", "main"]);
            let head = git(&w.outside, &["rev-parse", "HEAD"]);
            std::fs::write(work.join(".git/refs/heads/main"), format!("{head}\n")).unwrap();
            let alt = work.join(".git/objects/info/alternates");
            let content = format!("{}/.git/objects\n", w.outside.display());
            let alt2 = alt.clone();
            let on: Box<dyn Fn() -> bool + Send> =
                Box::new(move || std::fs::write(&alt, &content).is_ok());
            let off: Box<dyn Fn() + Send> = Box::new(move || {
                let _ = std::fs::remove_file(&alt2);
            });
            (work, on, off)
        },
        |_| {},
    );
}

#[test]
fn r5_file_swapped_during_a_plain_directory_ingest() {
    race_scenario(
        "OUTR5",
        false,
        |w| {
            docs(&w.root.join("docs"), DIR_FILES);
            std::fs::write(w.outside.join("secret.md"), "# S\n\nOUTR5_DOC text\n").unwrap();
            boxed(
                w.root.join("docs"),
                w.root.join("docs").join(DIR_TARGET),
                w.outside.join("secret.md"),
            )
        },
        |_| {},
    );
}

#[test]
fn r6_swap_to_a_repo_in_a_read_only_root() {
    race_scenario(
        "RO6",
        false,
        |w| {
            repo(&w.root.join("work"), "W6");
            repo(&w.ro_root.join("repo"), "RO6");
            boxed(
                w.root.join("work"),
                w.root.join("work"),
                w.ro_root.join("repo"),
            )
        },
        |_| {},
    );
}

#[test]
fn r6b_file_swapped_to_a_file_in_a_read_only_root() {
    race_scenario(
        "RO6B",
        false,
        |w| {
            docs(&w.root.join("docs"), DIR_FILES);
            std::fs::write(w.ro_root.join("secret.md"), "# S\n\nRO6B_DOC text\n").unwrap();
            boxed(
                w.root.join("docs"),
                w.root.join("docs").join(DIR_TARGET),
                w.ro_root.join("secret.md"),
            )
        },
        |_| {},
    );
}

#[test]
fn r7_swap_to_a_nested_read_only_area_of_the_same_rw_root() {
    race_scenario(
        "NEST7",
        false,
        |w| {
            repo(&w.root.join("work"), "W7");
            repo(&w.root.join("protected/repo"), "NEST7");
            boxed(
                w.root.join("work"),
                w.root.join("work"),
                w.root.join("protected/repo"),
            )
        },
        |w| {
            w.specs
                .push(format!("{}:ro", w.root.join("protected").display()));
        },
    );
}

#[test]
fn r7b_file_swapped_to_a_file_in_a_nested_read_only_area() {
    race_scenario(
        "NEST7B",
        false,
        |w| {
            docs(&w.root.join("docs"), DIR_FILES);
            std::fs::create_dir_all(w.root.join("protected")).unwrap();
            std::fs::write(
                w.root.join("protected/secret.md"),
                "# S\n\nNEST7B_DOC text\n",
            )
            .unwrap();
            boxed(
                w.root.join("docs"),
                w.root.join("docs").join(DIR_TARGET),
                w.root.join("protected/secret.md"),
            )
        },
        |w| {
            w.specs
                .push(format!("{}:ro", w.root.join("protected").display()));
        },
    );
}
