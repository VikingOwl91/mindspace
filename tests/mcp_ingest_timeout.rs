//! Time limit for `repo_ingest` over MCP (MIND-4).
//!
//! `mind mcp --ingest-timeout <SECONDS>` (default 30) bounds the part of an
//! ingest that can be stopped cleanly: everything before the first database
//! write, including the git subprocesses. An aborted ingest changes nothing in
//! the database. Once the first write transaction starts it runs to the end,
//! because the write fence treats an interrupted transaction as an uncertain
//! outcome and refuses all later writes of the session (verified in a
//! throwaway experiment, recorded in the ADR-026 addendum). So the limit is NOT
//! a bound on total duration; the tests only claim what the design delivers.
//!
//! These tests were written before the implementation. Before it, the flag did
//! not exist and an ingest could only run to completion.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn mind() -> Command {
    Command::new(env!("CARGO_BIN_EXE_mind"))
}

fn new_db(dir: &Path) -> PathBuf {
    let db = dir.join("mind.db");
    for args in [vec!["init"], vec!["space", "create", "p"]] {
        let o = mind().env("MINDSPACE_DB", &db).args(args).output().unwrap();
        assert!(o.status.success());
    }
    db
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    next_id: u64,
}

impl Server {
    /// `timeout` is the raw `--ingest-timeout` value; `None` leaves the default.
    fn start(db: &Path, root: &Path, timeout: Option<&str>) -> Self {
        Self::start_with(db, root, timeout, None)
    }

    fn start_with(db: &Path, root: &Path, timeout: Option<&str>, path_env: Option<&Path>) -> Self {
        let mut cmd = mind();
        cmd.env("MINDSPACE_DB", db)
            .env_remove("MINDSPACE_MCP_ALLOW_ROOTS")
            .env_remove("MINDSPACE_MCP_INGEST_TIMEOUT")
            .arg("mcp")
            .arg("--allow-root")
            .arg(format!("{}:rw", root.canonicalize().unwrap().display()));
        if let Some(t) = timeout {
            cmd.arg("--ingest-timeout").arg(t);
        }
        if let Some(p) = path_env {
            let old = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{}:{old}", p.display()));
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

    fn send(&mut self, method: &str, params: Value) -> u64 {
        self.next_id += 1;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc":"2.0","id":self.next_id,"method":method,"params":params})
        )
        .unwrap();
        self.next_id
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or(Value::Null)
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.send(method, params);
        self.read()
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        self.rpc("tools/call", json!({"name": name, "arguments": args}))["result"].clone()
    }

    fn ingest(&mut self, path: &Path) -> Value {
        self.call(
            "repo_ingest",
            json!({"path": path.canonicalize().unwrap().display().to_string(), "spaces": ["p"]}),
        )
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(r: &Value) -> String {
    r["content"][0]["text"].as_str().unwrap_or("").to_string()
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
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
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

/// A small repository with two documents and a Rust file.
fn small_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q", "-b", "main"]);
    write(path, "docs/alpha.md", "# Alpha\n\nalphamarker\n");
    write(path, "docs/beta.md", "# Beta\n\nbetamarker\n");
    write(path, "src/lib.rs", "pub fn lib_fn() {}\n");
    git(path, &["add", "-A"]);
    git(path, &["commit", "-q", "-m", "one"]);
}

/// Row counts of every table, to compare the whole database before and after.
fn snapshot(db: &Path) -> std::collections::BTreeMap<String, i64> {
    let c = rusqlite::Connection::open(db).unwrap();
    let tables: Vec<String> = c
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    tables
        .into_iter()
        .map(|t| {
            let n: i64 = c
                .query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0))
                .unwrap();
            (t, n)
        })
        .collect()
}

fn integrity_ok(db: &Path) {
    let c = rusqlite::Connection::open(db).unwrap();
    let r: String = c
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(r, "ok");
    let fk: i64 = c
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fk, 0, "foreign key violations");
}

fn active_memories(db: &Path) -> i64 {
    let c = rusqlite::Connection::open(db).unwrap();
    c.query_row(
        "SELECT count(*) FROM memories WHERE state = 'active'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

fn startup(db: &Path, root: &Path, timeout: &str) -> Output {
    mind()
        .env("MINDSPACE_DB", db)
        .env_remove("MINDSPACE_MCP_ALLOW_ROOTS")
        .env_remove("MINDSPACE_MCP_INGEST_TIMEOUT")
        .args(["mcp", "--allow-root"])
        .arg(format!("{}:ro", root.canonicalize().unwrap().display()))
        .arg(format!("--ingest-timeout={timeout}"))
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

// ---------------------------------------------------------------------------
// The limit aborts cleanly
// ---------------------------------------------------------------------------

/// The limit expires during preparation: the call fails with an actionable
/// message, and not a single row in the database changed.
#[test]
fn an_expired_limit_aborts_with_a_clear_error_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    small_repo(&repo);
    let db = new_db(tmp.path());
    let before = snapshot(&db);

    let mut s = Server::start(&db, tmp.path(), Some("0.001"));
    let r = s.ingest(&repo);
    drop(s);

    assert_eq!(r["isError"], true, "must fail, got {r}");
    let msg = text(&r);
    for needle in ["paths", "repo_outline", "--ingest-timeout", "not changed"] {
        assert!(msg.contains(needle), "message lacks `{needle}`: {msg}");
    }
    assert_eq!(
        snapshot(&db),
        before,
        "database changed by an aborted ingest"
    );
    integrity_ok(&db);
}

/// No partial retirements: an ingest that would retire memories (a document
/// disappeared from HEAD) leaves every memory active when it is aborted, and a
/// plain retry then converges to the same state as an uninterrupted ingest.
#[test]
fn an_aborted_ingest_retires_nothing_and_the_retry_converges() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    small_repo(&repo);
    let db = new_db(tmp.path());

    // State A, ingested normally.
    let mut s = Server::start(&db, tmp.path(), None);
    assert_eq!(s.ingest(&repo)["isError"], false);
    drop(s);
    let active_a = active_memories(&db);
    assert!(active_a >= 2, "fixture should have memories: {active_a}");

    // HEAD moves: beta.md is gone, so a completed ingest would retire it.
    git(&repo, &["rm", "-q", "docs/beta.md"]);
    git(&repo, &["commit", "-q", "-m", "drop beta"]);
    let before = snapshot(&db);

    let mut s = Server::start(&db, tmp.path(), Some("0.001"));
    assert_eq!(s.ingest(&repo)["isError"], true);
    drop(s);
    assert_eq!(
        snapshot(&db),
        before,
        "an aborted ingest changed the database"
    );
    assert_eq!(
        active_memories(&db),
        active_a,
        "memories were retired by an aborted ingest"
    );
    integrity_ok(&db);

    // Retry without the tiny limit completes and retires exactly the old memory.
    let mut s = Server::start(&db, tmp.path(), None);
    assert_eq!(s.ingest(&repo)["isError"], false);
    drop(s);
    assert!(
        active_memories(&db) < active_a,
        "the retry should retire the dropped document"
    );

    // Repeating changes nothing (idempotent), also with the tiny limit
    // refusing before any write.
    let settled = snapshot(&db);
    let mut s = Server::start(&db, tmp.path(), None);
    assert_eq!(s.ingest(&repo)["isError"], false);
    drop(s);
    let again = snapshot(&db);
    for (table, n) in &settled {
        if table == "m6_revisions" || table.starts_with("m6_") || table == "ingest_sessions" {
            continue; // each write records its own revision/session
        }
        assert_eq!(
            again[table], *n,
            "table {table} changed on an idempotent repeat"
        );
    }
}

/// An abort must not damage the session: later writes in the same server
/// process still work (the write fence is not poisoned).
#[test]
fn the_session_stays_writable_after_an_aborted_ingest() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    small_repo(&repo);
    let db = new_db(tmp.path());

    let mut s = Server::start(&db, tmp.path(), Some("0.001"));
    assert_eq!(s.ingest(&repo)["isError"], true);
    let w = s.call(
        "memory_create",
        json!({"content": "survivor note", "subject": "survivor", "spaces": ["p"]}),
    );
    assert_eq!(
        w["isError"],
        false,
        "write after an abort failed: {}",
        text(&w)
    );
    let found = s.call("memory_search", json!({"query": "survivor"}));
    assert!(found["structuredContent"]["hits"]
        .as_array()
        .is_some_and(|h| !h.is_empty()));
}

/// Puts a fake `git` first on PATH: it records its pid, then hangs.
fn fake_hanging_git(dir: &Path) -> (PathBuf, PathBuf) {
    let bin = dir.join("fakebin");
    std::fs::create_dir_all(&bin).unwrap();
    let pidfile = dir.join("git.pid");
    let script = bin.join("git");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho $$ >> '{}'\nexec sleep 60\n",
            pidfile.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    (bin, pidfile)
}

fn assert_killed(pidfile: &Path) {
    let pids = std::fs::read_to_string(pidfile).unwrap_or_default();
    assert!(
        !pids.trim().is_empty(),
        "the fake git never ran, the test proved nothing"
    );
    std::thread::sleep(Duration::from_millis(300));
    for pid in pids.split_whitespace() {
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "git process {pid} is still running after the abort"
        );
    }
}

/// A hung git process is killed at the deadline, not left running. This is the
/// failure mode of the original deadlock, which hung without any message.
/// Unscoped ingest: the size check (`scan_path`) is the first git user.
#[test]
fn a_hung_git_is_killed_when_the_limit_expires() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    small_repo(&repo);
    let db = new_db(tmp.path());
    let (bin, pidfile) = fake_hanging_git(tmp.path());

    let mut s = Server::start_with(&db, tmp.path(), Some("1"), Some(&bin));
    let started = Instant::now();
    let r = s.ingest(&repo);
    let took = started.elapsed();

    assert_eq!(r["isError"], true, "{r}");
    assert!(
        took < Duration::from_secs(8),
        "limit of 1s but the call took {took:?}"
    );
    assert_killed(&pidfile);
}

/// Same for a scoped ingest (`paths`), where the ingest's own reader is the
/// first git user: it must carry the deadline too.
#[test]
fn a_hung_git_is_killed_during_a_scoped_ingest() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    small_repo(&repo);
    let db = new_db(tmp.path());
    let (bin, pidfile) = fake_hanging_git(tmp.path());

    let mut s = Server::start_with(&db, tmp.path(), Some("1"), Some(&bin));
    let started = Instant::now();
    let r = s.call(
        "repo_ingest",
        json!({"path": repo.canonicalize().unwrap().display().to_string(),
               "spaces": ["p"], "paths": ["docs"]}),
    );
    let took = started.elapsed();

    assert_eq!(r["isError"], true, "{r}");
    assert!(text(&r).contains("--ingest-timeout"), "{}", text(&r));
    assert!(
        took < Duration::from_secs(8),
        "limit of 1s but the call took {took:?}"
    );
    assert_killed(&pidfile);
}

// ---------------------------------------------------------------------------
// Requests, configuration, defaults
// ---------------------------------------------------------------------------

/// Requests are served one after the other, in order. A second request sent
/// while the first is running is queued, not interleaved.
#[test]
fn a_second_request_is_queued_and_answered_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    small_repo(&repo);
    let db = new_db(tmp.path());

    let mut s = Server::start(&db, tmp.path(), None);
    let path = repo.canonicalize().unwrap().display().to_string();
    let first = s.send(
        "tools/call",
        json!({"name":"repo_ingest","arguments":{"path":path,"spaces":["p"]}}),
    );
    let second = s.send(
        "tools/call",
        json!({"name":"repo_ingest","arguments":{"path":path,"spaces":["p"]}}),
    );
    let (a, b) = (s.read(), s.read());
    assert_eq!(a["id"], first, "responses out of order: {a}");
    assert_eq!(b["id"], second, "responses out of order: {b}");
    assert_eq!(a["result"]["isError"], false, "{a}");
    assert_eq!(b["result"]["isError"], false, "{b}");
    drop(s);
    integrity_ok(&db);
}

/// A successful ingest reports how long it took against the limit, so a client
/// can see when it is running close to it.
#[test]
fn a_successful_ingest_reports_elapsed_time_and_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    small_repo(&repo);
    let db = new_db(tmp.path());
    let mut s = Server::start(&db, tmp.path(), Some("120"));
    let r = s.ingest(&repo);
    assert_eq!(r["isError"], false, "{r}");
    let sc = &r["structuredContent"];
    assert_eq!(sc["limit_ms"], 120_000, "{sc}");
    assert!(sc["elapsed_ms"].as_u64().is_some(), "{sc}");
    assert_eq!(sc["over_limit"], false, "{sc}");
}

/// Bad values stop the server at startup, with a message about the flag.
#[test]
fn invalid_timeouts_stop_the_server() {
    let tmp = tempfile::tempdir().unwrap();
    small_repo(&tmp.path().join("repo"));
    let db = new_db(tmp.path());
    for bad in [
        "0",
        "-1",
        "abc",
        "",
        "NaN",
        "inf",
        "1e999",
        "100000",
        "0.0000001",
    ] {
        let o = startup(&db, tmp.path(), bad);
        assert!(
            !o.status.success(),
            "--ingest-timeout {bad:?} must be refused"
        );
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(
            err.contains("ingest-timeout") || err.contains("ingest timeout"),
            "{bad:?} failed for the wrong reason: {err}"
        );
    }
    for good in ["0.5", "30", "3600"] {
        assert!(
            startup(&db, tmp.path(), good).status.success(),
            "{good} should be accepted"
        );
    }
}

#[test]
fn the_default_is_documented_in_the_help() {
    let o = mind().args(["mcp", "--help"]).output().unwrap();
    let help = String::from_utf8_lossy(&o.stdout);
    assert!(help.contains("--ingest-timeout"), "{help}");
    assert!(help.contains("30"), "default not documented: {help}");
}
