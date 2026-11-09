//! M7.3b.4: `repo_ingest_isolated` over the real confined `mind mcp`.
//!
//! Every test starts the production binary (`mind mcp --allow-root .. --grants --isolated-ingest`)
//! and speaks JSON-RPC to it: real Landlock, seccomp and cgroups, a real watchdog and broker. The
//! only test-only parts are hooks compiled into `cargo test` builds (feature `spawn-gate`): a
//! stand-in WORKER script run by the broker (hostile/stalling worker tests, marked) and short
//! limits. The frontend is never a stand-in. Skipped with a message without cgroup delegation.

#![allow(clippy::field_reassign_with_default)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn mind() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mind"))
}

const TOOL: &str = "repo_ingest_isolated";

struct World {
    _t: tempfile::TempDir,
    root: PathBuf,
    db: PathBuf,
    state: PathBuf,
}

impl World {
    fn new() -> Self {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let db = root.join("xdg/mindspace/mind.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let w = World {
            state: root.join("state"),
            _t: t,
            db,
            root,
        };
        for args in [vec!["init"], vec!["space", "create", "p"]] {
            assert!(w.mind(&args).output().unwrap().status.success());
        }
        w
    }
    fn mind(&self, args: &[&str]) -> Command {
        let mut c = Command::new(mind());
        c.args(args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("MINDSPACE_DB", &self.db)
            .env("XDG_DATA_HOME", self.root.join("xdg"))
            .env("XDG_STATE_HOME", &self.state)
            .env("HOME", &self.root)
            .stdin(Stdio::null());
        c
    }
    fn ok(&self, args: &[&str]) -> String {
        let o = self.mind(args).output().unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    }
    fn dir(&self, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let d = self.root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        for (n, c) in files {
            let p = d.join(n);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        }
        d
    }
    fn rows(&self) -> i64 {
        let c = rusqlite::Connection::open_with_flags(
            &self.db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        [
            "sources",
            "code_derivations",
            "ingest_sessions",
            "code_entities",
        ]
        .iter()
        .filter_map(|t| {
            c.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| {
                r.get::<_, i64>(0)
            })
            .ok()
        })
        .sum()
    }
    fn derivations(&self) -> i64 {
        rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM code_derivations", [], |r| r.get(0))
            .unwrap()
    }
    fn grant_id(&self, path: &Path) -> String {
        let v: Value = serde_json::from_str(&self.ok(&["grant", "list", "--json"])).unwrap();
        v.as_array()
            .unwrap()
            .iter()
            .find(|g| g["path"].as_str() == Some(path.to_str().unwrap()) && g["status"] == "active")
            .and_then(|g| g["id"].as_str())
            .unwrap()
            .to_string()
    }
}

struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
    next_id: u64,
    watchdog: i32,
    broker: i32,
}

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| !s.contains(") Z "))
        .unwrap_or(false)
}

fn gone(pid: u32) -> bool {
    for _ in 0..250 {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn children_of(pid: i32) -> Vec<u32> {
    std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect()
}

fn find_watchdog(frontend: u32) -> Option<i32> {
    let want = format!("mind-wd-{frontend}");
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(rd) = std::fs::read_dir("/proc") {
            for e in rd.flatten() {
                if let Ok(c) = std::fs::read_to_string(e.path().join("comm")) {
                    if c.trim() == want {
                        return e.file_name().to_string_lossy().parse().ok();
                    }
                }
            }
        }
        if Instant::now() >= end {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

impl Mcp {
    fn start(w: &World, roots: &[&Path], extra: &[&str], vars: &[(&str, String)]) -> Option<Mcp> {
        let mut c = w.mind(&["mcp"]);
        for r in roots {
            c.arg("--allow-root").arg(r);
        }
        c.args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(w.root.join("mcp.stderr")).unwrap());
        for (k, v) in vars {
            c.env(k, v);
        }
        let mut child = c.spawn().unwrap();
        let stdin = child.stdin.take();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut m = Mcp {
            child,
            stdin,
            out,
            next_id: 1,
            watchdog: 0,
            broker: 0,
        };
        m.rpc(
            "initialize",
            json!({"protocolVersion":"2025-06-18","capabilities":{},
                   "clientInfo":{"name":"t","version":"0"}}),
        );
        m.notify("notifications/initialized");
        if extra.contains(&"--isolated-ingest") {
            let pid = m.child.id();
            let Some(wd) = find_watchdog(pid) else {
                eprintln!("SKIPPED: no watchdog appeared");
                return None;
            };
            let end = Instant::now() + Duration::from_secs(3);
            let br = loop {
                if let Some(b) = children_of(wd).first() {
                    break *b as i32;
                }
                if Instant::now() >= end {
                    break 0;
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            std::thread::sleep(Duration::from_millis(300));
            if br == 0 || !alive(br as u32) {
                let err = std::fs::read_to_string(w.root.join("mcp.stderr")).unwrap_or_default();
                eprintln!("SKIPPED: the broker did not start (no cgroup delegation?): {err}");
                return None;
            }
            m.watchdog = wd;
            m.broker = br;
        }
        Some(m)
    }
    fn write(&mut self, v: Value) {
        let s = self.stdin.as_mut().unwrap();
        writeln!(s, "{v}").unwrap();
        s.flush().unwrap();
    }
    fn notify(&mut self, method: &str) {
        self.write(json!({"jsonrpc":"2.0","method":method}));
    }
    fn read(&mut self) -> Value {
        let mut l = String::new();
        self.out.read_line(&mut l).unwrap();
        serde_json::from_str(l.trim()).unwrap_or_else(|_| panic!("no JSON: `{l}`"))
    }
    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.write(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        let r = self.read();
        assert_eq!(r["id"], id, "{r}");
        r
    }
    fn tools(&mut self) -> Vec<String> {
        self.rpc("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }
    /// A tools/call; returns the `result` (or the JSON-RPC error object under "rpc_error").
    fn call(&mut self, name: &str, args: Value) -> Value {
        let r = self.rpc("tools/call", json!({"name": name, "arguments": args}));
        r.get("result")
            .cloned()
            .unwrap_or_else(|| json!({"rpc_error": r["error"]}))
    }
    fn send_call(&mut self, name: &str, args: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.write(json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
                          "params":{"name":name,"arguments":args}}));
        id
    }
    fn read_result(&mut self, id: u64) -> Value {
        let r = self.read();
        assert_eq!(r["id"], id, "{r}");
        r["result"].clone()
    }
    fn ingest(&mut self, path: &Path) -> Value {
        self.call(
            TOOL,
            json!({"path": path.to_str().unwrap(), "spaces": ["p"]}),
        )
    }
    fn finish(mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

macro_rules! mcp {
    ($w:expr, $roots:expr, $extra:expr, $vars:expr) => {
        match Mcp::start(&$w, $roots, $extra, $vars) {
            Some(m) => m,
            None => return,
        }
    };
}

const OPT_IN: &[&str] = &["--grants", "--isolated-ingest"];

fn err_kind(r: &Value) -> String {
    assert_eq!(r["isError"], true, "{r}");
    assert_eq!(r["structuredContent"]["persisted"], false, "{r}");
    r["structuredContent"]["error"]["kind"]
        .as_str()
        .unwrap_or_else(|| panic!("no structured error: {r}"))
        .to_string()
}

fn slow_source() -> String {
    format!("const X: [u8; 1] = [{}];\n", vec!["1"; 950_000].join(","))
}

fn wait_worker(m: &Mcp) -> u32 {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(p) = children_of(m.broker).first() {
            return *p;
        }
        assert!(Instant::now() < end, "no worker appeared");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// A stand-in worker (STAND-IN: test-only hook, run unconfined by the broker). `body` is shell
/// that runs after the READY/GO handshake with the channel on fd 3.
fn standin(w: &World, name: &str, body: &str) -> String {
    let p = w.root.join(name);
    std::fs::write(
        &p,
        format!("#!/bin/sh\nprintf 'READY\\n' >&5\nread go <&5\n{body}\n"),
    )
    .unwrap();
    p.display().to_string()
}

fn standin_vars(script: String, extra: &[(&str, &str)]) -> Vec<(&'static str, String)> {
    let mut v = vec![
        ("MINDSPACE_BROKER_GATE_WORKER", script),
        ("MINDSPACE_ISOLATED_GATE_NO_EVIDENCE", "1".to_string()),
    ];
    for (k, val) in extra {
        // leak the key: tests are short-lived and the keys are literals
        v.push((Box::leak(k.to_string().into_boxed_str()), val.to_string()));
    }
    v
}

// ------------------------------------------------------------------ 1, 2, 12: opt-in

#[test]
fn the_tool_is_absent_without_opt_in_present_with_it_and_other_tools_are_unchanged() {
    let w = World::new();
    let project = w.dir(
        "project",
        &[("a.md", "# A\n"), ("lib.rs", "pub fn f() {}\n")],
    );
    // 1. no opt-in: not listed, not callable, the existing tools work as before
    let mut m = mcp!(w, &[&project], &[], &[]);
    let base = m.tools();
    assert!(!base.iter().any(|t| t == TOOL), "{base:?}");
    let r = m.call(
        TOOL,
        json!({"path": project.to_str().unwrap(), "spaces": ["p"]}),
    );
    assert!(
        r["rpc_error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown tool"),
        "{r}"
    );
    let r = m.call(
        "repo_ingest",
        json!({"path": project.to_str().unwrap(), "spaces": ["p"]}),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert!(r["structuredContent"]["ingested"].is_array());
    assert!(r["structuredContent"].get("isolation").is_none());
    m.finish();
    // --broker alone starts the broker but does not offer the tool
    let w2 = World::new();
    let p2 = w2.dir("project", &[("a.md", "# A\n")]);
    let mut m = mcp!(w2, &[&p2], &["--broker"], &[]);
    assert_eq!(
        m.tools(),
        base,
        "--broker alone must not change the tool list"
    );
    m.finish();
    // 2. opt-in: the list is the base list plus the tool, last; nothing else changed
    let w3 = World::new();
    let p3 = w3.dir("project", &[("a.md", "# A\n")]);
    let mut m = mcp!(w3, &[&p3], OPT_IN, &[]);
    let with = m.tools();
    assert_eq!(
        &with[..with.len() - 1],
        &base[..],
        "the existing tools are identical"
    );
    assert_eq!(with.last().unwrap(), TOOL);
    let listed = m.rpc("tools/list", json!({}));
    let def = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == TOOL)
        .unwrap();
    let props = def["inputSchema"]["properties"].as_object().unwrap();
    assert_eq!(props.keys().collect::<Vec<_>>(), ["path", "spaces"]);
    assert_eq!(def["inputSchema"]["additionalProperties"], false);
    m.finish();
}

// ------------------------------------------------------------------ 3-6, 11: authority and reuse

#[test]
fn static_and_dynamic_roots_unauthorized_roots_revocation_and_reuse() {
    let w = World::new();
    let project = w.dir(
        "project",
        &[
            ("a.md", "# A\n\nalpha\n"),
            ("src/lib.rs", "pub fn static_symbol() {}\n"),
        ],
    );
    let dynamic = w.dir(
        "dynamic",
        &[("x.rs", "pub fn dynamic_symbol() {}\n"), ("d.md", "# D\n")],
    );
    let other = w.dir("other", &[("a.md", "# O\n")]);
    let mut m = mcp!(w, &[&project], OPT_IN, &[]);
    // 3. static root
    let before = w.rows();
    let r = m.ingest(&project);
    assert_eq!(r["isError"], false, "{r}");
    let s = &r["structuredContent"];
    assert_eq!(s["isolation"]["authority"], "static_root");
    assert_eq!(s["isolation"]["worker"]["seccomp"], "2");
    assert_eq!(s["isolation"]["worker"]["contained_in_server_cgroup"], true);
    assert_eq!(s["isolation"]["ended_by"], "done");
    assert!(w.rows() > before);
    let hit = m.call("code_search", json!({"query": "static_symbol"}));
    assert!(
        hit["structuredContent"]["hits"].as_array().unwrap().len() == 1,
        "{hit}"
    );
    // 5. unauthorized: a stranger, a parent, a subdirectory, a symlink, relative, unknown space
    let before = w.rows();
    let alias = w.root.join("alias");
    std::os::unix::fs::symlink(&project, &alias).unwrap();
    for p in [&other, &w.root, &project.join("src"), &alias] {
        let r = m.ingest(p);
        assert_eq!(err_kind(&r), "root_not_authorized", "{r}");
    }
    let r = m.call(TOOL, json!({"path": "relative/dir", "spaces": ["p"]}));
    assert_eq!(err_kind(&r), "invalid_argument", "{r}");
    let r = m.call(
        TOOL,
        json!({"path": project.to_str().unwrap(), "spaces": ["nope"]}),
    );
    assert_eq!(err_kind(&r), "unknown_space", "{r}");
    assert_eq!(w.rows(), before, "refusals change nothing");
    // 4. a root approved AFTER startup, no restart
    let r = m.ingest(&dynamic);
    assert_eq!(err_kind(&r), "root_not_authorized", "{r}");
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let r = m.ingest(&dynamic);
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["isolation"]["authority"], "grant");
    let hit = m.call("code_search", json!({"query": "dynamic_symbol"}));
    assert!(
        hit["structuredContent"]["hits"].as_array().unwrap().len() == 1,
        "{hit}"
    );
    // 11. repeated ingestion reuses known derivations and adds no derivation
    let d1 = w.derivations();
    let r = m.ingest(&dynamic);
    assert_eq!(r["isError"], false, "{r}");
    let reused = r["structuredContent"]["context_exchange"]["known_derivations_reused"]
        .as_u64()
        .unwrap();
    assert!(reused >= 1, "{r}");
    assert_eq!(w.derivations(), d1, "no new derivation rows");
    // 6. revoked grant
    let id = w.grant_id(&dynamic);
    w.ok(&["grant", "revoke", &id, "--reason", "test"]);
    let r = m.ingest(&dynamic);
    assert_eq!(err_kind(&r), "root_not_authorized", "{r}");
    // the static root is unaffected by the grant store
    let r = m.ingest(&project);
    assert_eq!(r["isError"], false, "{r}");
    m.finish();
}

#[test]
fn the_caller_cannot_inject_authority_or_limits() {
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n")]);
    let mut m = mcp!(w, &[&project], OPT_IN, &[]);
    let before = w.rows();
    for extra in [
        ("grant", json!("g-0123456789ab")),
        ("access", json!("rw")),
        ("ttl", json!("1h")),
        ("fd", json!(9)),
        ("wall_timeout", json!(99999)),
        ("max_blob_bytes", json!(1)),
        ("history", json!(true)),
        ("paths", json!(["x"])),
    ] {
        let mut args = json!({"path": project.to_str().unwrap(), "spaces": ["p"]});
        args[extra.0] = extra.1.clone();
        let r = m.call(TOOL, args);
        assert_eq!(r["isError"], true, "{extra:?}: {r}");
        assert!(
            r["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("unexpected argument"),
            "{r}"
        );
    }
    assert_eq!(w.rows(), before);
    assert!(
        children_of(m.broker).is_empty(),
        "no worker was started by any of them"
    );
    m.finish();
}

// ------------------------------------------------------------------ 7: revocation

#[test]
fn revoking_a_grant_during_extraction_cancels_it_and_persists_nothing() {
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n")]);
    let dynamic = w.dir("dynamic", &[("a.md", "# D\n"), ("z.rs", &slow_source())]);
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let mut m = mcp!(w, &[&project], OPT_IN, &[]);
    let before = w.rows();
    let id = m.send_call(
        TOOL,
        json!({"path": dynamic.to_str().unwrap(), "spaces": ["p"]}),
    );
    let worker = wait_worker(&m);
    std::thread::sleep(Duration::from_millis(200)); // inside the parse
    let gid = w.grant_id(&dynamic);
    w.ok(&["grant", "revoke", &gid, "--reason", "mid-operation"]);
    let t = Instant::now();
    let r = m.read_result(id);
    let latency = t.elapsed();
    assert_eq!(err_kind(&r), "revoked", "{r}");
    assert!(latency < Duration::from_secs(2), "{latency:?}");
    assert!(gone(worker));
    assert_eq!(w.rows(), before, "nothing persisted");
    // MCP is alive, the broker is alive, and a static ingest still works
    assert!(alive(m.child.id()), "MCP died");
    assert!(
        alive(m.broker as u32),
        "the broker (pid {}) died: stderr [{}] state {:?}, watchdog {} alive {}",
        m.broker,
        std::fs::read_to_string(w.root.join("mcp.stderr")).unwrap_or_default(),
        std::fs::read_to_string(format!("/proc/{}/stat", m.broker)).ok(),
        m.watchdog,
        alive(m.watchdog as u32)
    );
    let r = m.ingest(&project);
    assert_eq!(r["isError"], false, "{r}");
    eprintln!(
        "revocation during extraction: error returned {latency:?} after `grant revoke` completed"
    );
    m.finish();
}

#[test]
fn revoking_a_grant_during_the_context_exchange_cancels_it() {
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n")]);
    let dynamic = w.dir("dynamic", &[("a.md", "# D\n")]);
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    // STAND-IN worker: says hello, reads the request, then never sends its manifest: the
    // operation sits inside the context exchange until the broker ends it.
    let script = standin(
        &w,
        "stall.sh",
        "printf '{\"v\":2,\"pid\":1,\"sandbox\":{}}\\n' >&3\nread -r req <&3\nsleep 600",
    );
    let mut m = mcp!(
        w,
        &[&project],
        &["--grants", "--isolated-ingest", "--ingest-timeout", "60"],
        &standin_vars(script, &[])
    );
    let before = w.rows();
    let id = m.send_call(
        TOOL,
        json!({"path": dynamic.to_str().unwrap(), "spaces": ["p"]}),
    );
    let worker = wait_worker(&m);
    std::thread::sleep(Duration::from_millis(300));
    let gid = w.grant_id(&dynamic);
    w.ok(&["grant", "revoke", &gid]);
    let t = Instant::now();
    let r = m.read_result(id);
    let latency = t.elapsed();
    assert_eq!(err_kind(&r), "revoked", "{r}");
    assert!(latency < Duration::from_secs(2), "{latency:?}");
    assert!(gone(worker));
    assert_eq!(w.rows(), before);
    eprintln!("revocation during the context exchange: error returned {latency:?} after revoke");
    m.finish();
}

#[test]
fn a_static_root_is_not_revocable_and_a_timeout_ends_the_operation() {
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n")]);
    // a grant on the very directory that is also a static root: the static authority governs
    w.ok(&["grant", "add", project.to_str().unwrap(), "--yes"]);
    let script = standin(&w, "stall.sh", "sleep 600");
    let mut m = mcp!(
        w,
        &[&project],
        &["--grants", "--isolated-ingest", "--ingest-timeout", "1.5"],
        &standin_vars(script, &[("MINDSPACE_BROKER_GATE_WALL_MS", "20000")])
    );
    let id = m.send_call(
        TOOL,
        json!({"path": project.to_str().unwrap(), "spaces": ["p"]}),
    );
    wait_worker(&m);
    let gid = w.grant_id(&project);
    w.ok(&["grant", "revoke", &gid]);
    std::thread::sleep(Duration::from_millis(500));
    // still running after the revocation (static authority), then the frontend's wall clock ends it
    let t = Instant::now();
    let r = m.read_result(id);
    let k = err_kind(&r);
    assert!(
        matches!(k.as_str(), "timeout" | "worker_failed"),
        "{k}: {r}"
    );
    assert_ne!(
        k, "revoked",
        "a static root must not be treated as a revoked grant"
    );
    eprintln!("timeout ({k}) {:?} after the revoke", t.elapsed());
    m.finish();
}

#[test]
fn the_brokers_wall_clock_ends_a_stalled_operation_and_the_broker_serves_again() {
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n")]);
    // STAND-IN worker that stalls inside the context exchange; the broker's limit (1.5 s) is
    // shorter than the frontend's (60 s), so the BROKER ends it
    let script = standin(
        &w,
        "stall.sh",
        "printf '{\"v\":2,\"pid\":1,\"sandbox\":{}}\\n' >&3\nsleep 600",
    );
    let mut m = mcp!(
        w,
        &[&project],
        &["--grants", "--isolated-ingest", "--ingest-timeout", "60"],
        &standin_vars(script, &[("MINDSPACE_BROKER_GATE_WALL_MS", "1500")])
    );
    let before = w.rows();
    let t = Instant::now();
    let r = m.ingest(&project);
    let took = t.elapsed();
    assert_eq!(err_kind(&r), "timeout", "{r}");
    assert!(
        took >= Duration::from_millis(1400) && took < Duration::from_secs(10),
        "{took:?}"
    );
    assert_eq!(w.rows(), before);
    // the broker ended it and survived: the next call reaches the broker again (same stand-in,
    // so it times out the same explicit way, rather than finding a dead broker)
    assert!(alive(m.broker as u32));
    let r = m.ingest(&project);
    assert_eq!(err_kind(&r), "timeout", "{r}");
    eprintln!("broker wall clock: timeout after {took:?} (limit 1.5 s)");
    m.finish();
}

// ------------------------------------------------------------------ 8, 9, 10: failures

#[test]
fn a_malformed_worker_stream_changes_no_rows() {
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n")]);
    // STAND-IN worker 1: garbage instead of a manifest
    let garbage = standin(
        &w,
        "g1.sh",
        "printf '{\"v\":2,\"pid\":1,\"sandbox\":{}}\\n' >&3\nprintf 'garbage\\n' >&3\nsleep 5",
    );
    // STAND-IN worker 2: a valid manifest and context exchange, then garbage where the record
    // stream must start
    let late = standin(
        &w,
        "g2.sh",
        "printf '{\"v\":2,\"pid\":1,\"sandbox\":{}}\\n' >&3\nread -r req <&3\n\
         op=$(printf '%s' \"$req\" | sed 's/.*\"op\":\"\\([^\"]*\\)\".*/\\1/')\n\
         printf '{\"v\":2,\"op\":\"%s\",\"entries\":[]}\\n' \"$op\" >&3\nread -r ctx <&3\n\
         printf 'not-a-frame-at-all-not-a-frame' >&3\nsleep 5",
    );
    for (script, expect) in [(garbage, "context_refused"), (late, "result_refused")] {
        let mut m = mcp!(
            w,
            &[&project],
            &["--grants", "--isolated-ingest", "--ingest-timeout", "10"],
            &standin_vars(script, &[])
        );
        let before = w.rows();
        let r = m.ingest(&project);
        let k = err_kind(&r);
        assert!(
            k == expect || k == "worker_failed",
            "expected {expect} (or the worker dying first), got {k}: {r}"
        );
        assert_eq!(w.rows(), before, "no partial persistence ({k})");
        m.finish();
    }
}

#[test]
fn a_worker_crash_does_not_end_mcp_and_a_broker_crash_gives_a_clear_error() {
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n")]);
    let slow = w.dir("slow", &[("a.md", "# S\n"), ("z.rs", &slow_source())]);
    w.ok(&["grant", "add", slow.to_str().unwrap(), "--yes"]);
    let mut m = mcp!(w, &[&project], OPT_IN, &[]);
    // 9. a real worker killed mid-operation
    let before = w.rows();
    let id = m.send_call(
        TOOL,
        json!({"path": slow.to_str().unwrap(), "spaces": ["p"]}),
    );
    let worker = wait_worker(&m);
    // let it confine itself and start parsing: killing it earlier is a different (also refused) case
    std::thread::sleep(Duration::from_millis(300));
    unsafe { libc::kill(worker as i32, libc::SIGKILL) };
    let r = m.read_result(id);
    let k = err_kind(&r);
    assert!(
        matches!(
            k.as_str(),
            "worker_failed" | "result_refused" | "context_refused"
        ),
        "{k}: {r}"
    );
    assert_eq!(w.rows(), before);
    assert!(alive(m.child.id()), "MCP survived the worker crash");
    assert!(m.tools().contains(&TOOL.to_string()));
    let r = m.ingest(&project);
    assert_eq!(r["isError"], false, "the broker still serves: {r}");
    // 10. the broker is SIGKILLed: the next call says so, other tools keep working
    unsafe { libc::kill(m.broker, libc::SIGKILL) };
    assert!(gone(m.broker as u32));
    std::thread::sleep(Duration::from_millis(100));
    let before = w.rows();
    let r = m.ingest(&project);
    assert_eq!(err_kind(&r), "broker_unavailable", "{r}");
    assert!(r["structuredContent"]["error"]["message"]
        .as_str()
        .unwrap()
        .contains("broker is not available"));
    assert_eq!(w.rows(), before);
    assert!(alive(m.child.id()));
    let r = m.call("space_list", json!({}));
    assert_eq!(r["isError"], false, "{r}");
    let r = m.call(
        "memory_create",
        json!({"content": "still works", "spaces": ["p"]}),
    );
    assert_eq!(r["isError"], false, "{r}");
    // repeated calls keep failing the same explicit way (never a silent fallback)
    let r = m.ingest(&project);
    assert_eq!(err_kind(&r), "broker_unavailable", "{r}");
    m.finish();
}

// ------------------------------------------------------------------ measurements

fn synthetic(w: &World, files: usize) -> PathBuf {
    let d = w.root.join("synthetic");
    for i in 0..files {
        let body: String = (0..40)
            .map(|j| format!("pub fn f_{i}_{j}(a: u32) -> u32 {{ a + {j} }}\n"))
            .collect();
        let sub = d.join(format!("m{}", i % 20));
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(format!("f{i}.rs")), body).unwrap();
        std::fs::write(
            sub.join(format!("n{i}.md")),
            format!("# Note {i}\n\ntext {i}\n"),
        )
        .unwrap();
    }
    d
}

#[test]
#[ignore = "measurement; run with --release --ignored --nocapture"]
fn measure_isolated_vs_in_process_and_revocation_latency() {
    let repo_src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .canonicalize()
        .unwrap();
    // the repository's own src is copied so that it is a rw static root of its own
    for (name, make) in [
        ("small (3 files)", 0usize),
        ("repo src (~100 files)", 1),
        ("synthetic 1800 files", 2),
    ] {
        let mut iso = vec![];
        let mut inp = vec![];
        let mut reused = vec![];
        for _ in 0..3 {
            let w = World::new();
            let dir = match make {
                0 => w.dir(
                    "t",
                    &[
                        ("a.md", "# A\n"),
                        ("l.rs", "pub fn f() {}\n"),
                        ("m.rs", "fn g() {}\n"),
                    ],
                ),
                1 => {
                    let d = w.root.join("t");
                    let _ = Command::new("cp").arg("-r").arg(&repo_src).arg(&d).status();
                    d
                }
                _ => synthetic(&w, 900),
            };
            let mut m = mcp!(
                w,
                &[&dir],
                &["--isolated-ingest"],
                &[("MINDSPACE_X", "1".into())]
            );
            let a = m.ingest(&dir);
            assert_eq!(a["isError"], false, "{a}");
            iso.push(a["structuredContent"]["elapsed_ms"].as_f64().unwrap());
            let b = m.ingest(&dir); // second run: derivations are known
            reused.push(b["structuredContent"]["elapsed_ms"].as_f64().unwrap());
            m.finish();
            let w2 = World::new();
            let dir2 = match make {
                0 => w2.dir(
                    "t",
                    &[
                        ("a.md", "# A\n"),
                        ("l.rs", "pub fn f() {}\n"),
                        ("m.rs", "fn g() {}\n"),
                    ],
                ),
                1 => {
                    let d = w2.root.join("t");
                    let _ = Command::new("cp").arg("-r").arg(&repo_src).arg(&d).status();
                    d
                }
                _ => synthetic(&w2, 900),
            };
            let mut m2 = mcp!(w2, &[&dir2], &[], &[]);
            let c = m2.call(
                "repo_ingest",
                json!({"path": dir2.to_str().unwrap(), "spaces": ["p"]}),
            );
            assert_eq!(c["isError"], false, "{c}");
            inp.push(c["structuredContent"]["elapsed_ms"].as_f64().unwrap());
            m2.finish();
        }
        let med = |mut v: Vec<f64>| {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[v.len() / 2]
        };
        eprintln!(
            "[{name}] median ms: repo_ingest (in-process) {:.1} | repo_ingest_isolated first {:.1}, repeated (known derivations) {:.1}",
            med(inp), med(iso), med(reused)
        );
    }
    // revocation latency: 8 trials against the slow tree
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n")]);
    let dynamic = w.dir("dynamic", &[("a.md", "# D\n"), ("z.rs", &slow_source())]);
    let mut m = mcp!(w, &[&project], OPT_IN, &[]);
    let mut lat = vec![];
    for i in 0..8 {
        w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
        let id = m.send_call(
            TOOL,
            json!({"path": dynamic.to_str().unwrap(), "spaces": ["p"]}),
        );
        wait_worker(&m);
        std::thread::sleep(Duration::from_millis(100 + 37 * i)); // different phases of the interval
        let gid = w.grant_id(&dynamic);
        w.ok(&["grant", "revoke", &gid]);
        let t = Instant::now();
        let r = m.read_result(id);
        assert_eq!(err_kind(&r), "revoked", "{r}");
        lat.push(t.elapsed().as_secs_f64() * 1e3);
    }
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
    eprintln!(
        "revocation latency after `grant revoke` returned (recheck interval 100 ms), ms: min {:.1} median {:.1} max {:.1}",
        lat[0], lat[lat.len() / 2], lat[lat.len() - 1]
    );
    m.finish();
}

// ------------------------------------------------------------------ commit-time authorization
//
// Linearization point of a revocation against a persistence (docs/m7.3b-mcp-isolated-ingest.md):
// the broker takes the grant store's shared lock and THEN rechecks the grant, immediately before
// the frontend's COMMIT. Revocations that finished earlier deny the commit; revocations that start
// later wait for the lock and are ordered after the commit. The pause hooks (test builds only)
// stop the frontend at exact points; the tests wait on files, not on sleeps.

struct Pause {
    prefix: PathBuf,
}

impl Pause {
    fn new(w: &World, name: &str) -> Self {
        // the database directory is the one place the confined frontend can write
        Self {
            prefix: w.db.parent().unwrap().join(name),
        }
    }
    fn var(&self) -> String {
        self.prefix.display().to_string()
    }
    fn file(&self, ext: &str) -> PathBuf {
        let mut p = self.prefix.clone().into_os_string();
        p.push(ext);
        PathBuf::from(p)
    }
    fn wait_reached(&self) {
        let end = Instant::now() + Duration::from_secs(60);
        while !self.file(".reached").exists() {
            assert!(
                Instant::now() < end,
                "the frontend never reached the boundary"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn go(&self) {
        std::fs::write(self.file(".go"), b"").unwrap();
    }
}

fn grant_world(w: &World, files: &[(&str, &str)]) -> (PathBuf, PathBuf) {
    let project = w.dir("project", &[("a.md", "# A\n")]);
    let dynamic = w.dir("dynamic", files);
    (project, dynamic)
}

#[test]
fn revocation_after_worker_completion_but_before_persistence_denies_the_commit() {
    let w = World::new();
    let (project, dynamic) = grant_world(
        &w,
        &[("x.rs", "pub fn late_symbol() {}\n"), ("d.md", "# D\n")],
    );
    let pause = Pause::new(&w, "before");
    let mut m = mcp!(
        w,
        &[&project],
        OPT_IN,
        &[("MINDSPACE_ISOLATED_GATE_PAUSE_BEFORE", pause.var())]
    );
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let before = w.rows();
    let id = m.send_call(
        TOOL,
        json!({"path": dynamic.to_str().unwrap(), "spaces": ["p"]}),
    );
    // the worker finished, the stream was validated, the write transaction is open but
    // uncommitted, and the frontend is stopped right before it asks the broker
    pause.wait_reached();
    assert_eq!(w.rows(), before, "nothing is visible before the commit");
    let gid = w.grant_id(&dynamic);
    w.ok(&[
        "grant",
        "revoke",
        &gid,
        "--reason",
        "between validation and commit",
    ]);
    pause.go();
    let r = m.read_result(id);
    assert_eq!(err_kind(&r), "revoked", "{r}");
    assert!(r["structuredContent"]["error"]["message"]
        .as_str()
        .unwrap()
        .contains("commit"));
    assert_eq!(w.rows(), before, "the denied commit left no trace");
    let hit = m.call("code_search", json!({"query": "late_symbol"}));
    assert_eq!(
        hit["structuredContent"]["hits"].as_array().unwrap().len(),
        0,
        "{hit}"
    );
    m.finish();
}

#[test]
fn revocation_during_persistence_waits_for_the_commit_and_is_ordered_after_it() {
    let w = World::new();
    let (project, dynamic) = grant_world(&w, &[("x.rs", "pub fn during_symbol() {}\n")]);
    let pause = Pause::new(&w, "after");
    let mut m = mcp!(
        w,
        &[&project],
        OPT_IN,
        &[("MINDSPACE_ISOLATED_GATE_PAUSE_AFTER", pause.var())]
    );
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let before = w.rows();
    let id = m.send_call(
        TOOL,
        json!({"path": dynamic.to_str().unwrap(), "spaces": ["p"]}),
    );
    // the lease is granted: the broker holds the store's shared lock, the commit has not happened
    pause.wait_reached();
    assert_eq!(w.rows(), before);
    let gid = w.grant_id(&dynamic);
    let mut revoke = w
        .mind(&["grant", "revoke", &gid, "--reason", "during the commit"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(700));
    assert!(
        revoke.try_wait().unwrap().is_none(),
        "`mind grant revoke` must wait while a commit is leased"
    );
    pause.go();
    let r = m.read_result(id);
    assert_eq!(r["isError"], false, "the leased commit went through: {r}");
    assert!(w.rows() > before, "committed");
    // only now can the revocation complete: it is ordered AFTER the commit
    let t = Instant::now();
    assert!(revoke.wait().unwrap().success());
    assert!(t.elapsed() < Duration::from_secs(5));
    // after the commit: the data stays (revoking never deletes), new operations are refused
    let hit = m.call("code_search", json!({"query": "during_symbol"}));
    assert_eq!(
        hit["structuredContent"]["hits"].as_array().unwrap().len(),
        1,
        "{hit}"
    );
    let r = m.ingest(&dynamic);
    assert_eq!(err_kind(&r), "root_not_authorized", "{r}");
    m.finish();
}

#[test]
fn a_frontend_that_never_begins_its_commit_is_removed_at_the_grant_ceiling() {
    let w = World::new();
    let (project, dynamic) = grant_world(&w, SYMBOL_FILES);
    let pause = Pause::new(&w, "stalled");
    let mut m = mcp!(
        w,
        &[&project],
        OPT_IN,
        &[("MINDSPACE_ISOLATED_GATE_PAUSE_AFTER", pause.var())]
    );
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let before = w.dump();
    let frontend = m.child.id();
    m.send_call(
        TOOL,
        json!({"path": dynamic.to_str().unwrap(), "spaces": ["p"]}),
    );
    pause.wait_reached(); // leased, nothing begun: the frontend stalls here
    let gid = w.grant_id(&dynamic);
    let mut revoke = w.revoke_bg(&gid);
    assert!(stays_blocked(&mut revoke, Duration::from_millis(1000)));
    // the policy: the broker removes the frontend, THEN the lock is released
    let waited = exits_within(&mut revoke, Duration::from_secs(10)).expect("the lease never ended");
    assert!(revoke.wait().unwrap().success());
    assert!(
        !alive(frontend),
        "the stalled frontend must be gone when the revocation completes"
    );
    pause.go();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(w.integrity(), "ok");
    assert_eq!(
        w.dump(),
        before,
        "a removed frontend cannot commit afterwards"
    );
    eprintln!("stalled before COMMIT: frontend removed, revocation completed after the grant ceiling ({waited:?} after the stall began)");
}

#[test]
fn a_database_failure_at_the_commit_boundary_leaves_nothing_even_through_mcp() {
    let w = World::new();
    let project = w.dir(
        "project",
        &[("a.md", "# A\n\nalpha\n"), ("l.rs", "pub fn fi() {}\n")],
    );
    let mut m = mcp!(w, &[&project], OPT_IN, &[]);
    // a real SQLite fault in the SECOND part of the old two-step write
    rusqlite::Connection::open(&w.db)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fi BEFORE INSERT ON code_derivations BEGIN SELECT RAISE(ABORT,'injected'); END;",
        )
        .unwrap();
    let before = w.rows();
    let r = m.ingest(&project);
    assert_eq!(r["isError"], true, "{r}");
    assert_eq!(
        r["structuredContent"]["error"]["kind"], "persist_failed",
        "{r}"
    );
    assert_eq!(r["structuredContent"]["atomic"], true, "{r}");
    assert_eq!(
        w.rows(),
        before,
        "the observation was rolled back with the derivations"
    );
    rusqlite::Connection::open(&w.db)
        .unwrap()
        .execute_batch("DROP TRIGGER fi")
        .unwrap();
    // the same call now succeeds (a retry heals; there was nothing half-written to repair)
    let r = m.ingest(&project);
    assert_eq!(r["isError"], false, "{r}");
    m.finish();
}

// ------------------------------------------------------------------ resource policy parity

#[test]
fn the_unscoped_file_limit_of_repo_ingest_applies_and_a_refusal_is_early_and_clean() {
    let w = World::new();
    let project = w.dir("project", &[]);
    for i in 0..2001 {
        std::fs::write(project.join(format!("n{i}.md")), format!("# N{i}\n")).unwrap();
    }
    let mut m = mcp!(w, &[&project], OPT_IN, &[]);
    let before = w.rows();
    let t = Instant::now();
    let r = m.ingest(&project);
    let took = t.elapsed();
    assert_eq!(err_kind(&r), "too_many_files", "{r}");
    assert!(r["structuredContent"]["error"]["message"]
        .as_str()
        .unwrap()
        .contains("2000"));
    assert_eq!(w.rows(), before, "nothing persisted");
    assert!(children_of(m.broker).is_empty(), "the worker was ended");
    // the in-process tool applies the same limit to the same tree
    let r2 = m.call(
        "repo_ingest",
        json!({"path": project.to_str().unwrap(), "spaces": ["p"]}),
    );
    assert_eq!(r2["isError"], true);
    assert!(
        r2["content"][0]["text"].as_str().unwrap().contains("2000"),
        "{r2}"
    );
    // exactly the limit passes
    std::fs::remove_file(project.join("n2000.md")).unwrap();
    let t2 = Instant::now();
    let r = m.ingest(&project);
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["resource_policy"]["max_files"], 2000);
    eprintln!(
        "2001 files refused after {took:?}; exactly 2000 files ingested in {:?}",
        t2.elapsed()
    );
    m.finish();
}

// ------------------------------------------------------------------ commit-boundary fault injection
//
// Commit states (never inferred from whether a message arrived):
//   COMMITTED    the storage layer returned Ok.
//   ROLLED_BACK  any other error: nothing was committed (and no recording revision consumed).
//   UNKNOWN      `CommitOutcomeUnknown`: the COMMIT failed in a way that does not say.
// Ordering on the success path: SQLite COMMIT returns -> frontend sends `CommitDone` -> broker
// releases the grant store's shared lock. Process death anywhere releases the lock (the kernel
// drops flock with the descriptor; the broker also releases on hang-up and after 3 s).

impl World {
    fn dump(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        let c = rusqlite::Connection::open_with_flags(
            &self.db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let tables: Vec<String> = c
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect();
        let mut out = std::collections::BTreeMap::new();
        for t in tables {
            let cols: Vec<String> = c
                .prepare(&format!("PRAGMA table_info(\"{t}\")"))
                .unwrap()
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .flatten()
                .collect();
            if cols.is_empty() {
                continue;
            }
            let expr = cols
                .iter()
                .map(|n| format!("quote(\"{n}\")"))
                .collect::<Vec<_>>()
                .join("||'|'||");
            let mut rows: Vec<String> = c
                .prepare(&format!("SELECT {expr} FROM \"{t}\""))
                .unwrap()
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .flatten()
                .collect();
            rows.sort();
            out.insert(t, rows);
        }
        out
    }
    fn count(&self, table: &str) -> i64 {
        rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
            .query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |r| {
                r.get(0)
            })
            .unwrap()
    }
    fn integrity(&self) -> String {
        rusqlite::Connection::open(&self.db)
            .unwrap()
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap()
    }
    /// The tables a repeated ingestion must not grow (derived structure is keyed by content).
    fn structure(&self) -> Vec<i64> {
        [
            "sources",
            "code_inputs",
            "code_derivations",
            "code_entities",
            "code_contexts",
        ]
        .iter()
        .map(|t| self.count(t))
        .collect()
    }
    fn revoke_bg(&self, gid: &str) -> std::process::Child {
        self.mind(&["grant", "revoke", gid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }
}

impl Pause {
    /// The instant the frontend recorded right after its COMMIT returned (ns since the epoch).
    fn committed_ns(&self) -> u128 {
        std::fs::read_to_string(self.file(".committed"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }
    fn reset(&self) {
        for e in [".reached", ".go", ".committed"] {
            let _ = std::fs::remove_file(self.file(e));
        }
    }
}

/// Waits for a child, up to `limit`; returns how long it took, or None if it is still running.
fn exits_within(child: &mut std::process::Child, limit: Duration) -> Option<Duration> {
    let t = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return Some(t.elapsed());
        }
        if t.elapsed() >= limit {
            return None;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// True if the child has NOT exited after `window` (it is blocked, e.g. on the grant lock).
fn stays_blocked(child: &mut std::process::Child, window: Duration) -> bool {
    exits_within(child, window).is_none()
}

fn kill(pid: i32) {
    unsafe { libc::kill(pid, libc::SIGKILL) };
}

fn fault_state(r: &Value) -> (String, Value) {
    (
        r["structuredContent"]["commit_state"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        r["structuredContent"]["persisted"].clone(),
    )
}

const SYMBOL_FILES: &[(&str, &str)] = &[
    ("x.rs", "pub fn fault_symbol() {}\nstruct S;\n"),
    ("d.md", "# D\n\nbody\n"),
];

// 1 ------------------------------------------------------------------------------------------

#[test]
fn frontend_crash_before_commit_leaves_no_trace_and_no_lock_and_a_retry_is_clean() {
    let w = World::new();
    let (project, dynamic) = grant_world(&w, SYMBOL_FILES);
    let pause = Pause::new(&w, "p1");
    let mut m = mcp!(
        w,
        &[&project],
        OPT_IN,
        &[("MINDSPACE_ISOLATED_GATE_PAUSE_BEFORE", pause.var())]
    );
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let before = w.dump();
    m.send_call(
        TOOL,
        json!({"path": dynamic.to_str().unwrap(), "spaces": ["p"]}),
    );
    pause.wait_reached(); // the write transaction is open and uncommitted
    let (wd, br) = (m.watchdog as u32, m.broker as u32);
    kill(m.child.id() as i32);
    let _ = m.child.wait();
    assert!(gone(wd) && gone(br));
    // SQLite recovers the log: the uncommitted transaction left nothing, not even a revision
    assert_eq!(w.integrity(), "ok");
    assert_eq!(w.dump(), before);
    // the broker had not taken the lock yet: a revocation is not delayed at all
    let gid = w.grant_id(&dynamic);
    let t = Instant::now();
    w.ok(&["grant", "revoke", &gid]);
    assert!(t.elapsed() < Duration::from_secs(2));
    // a retry from a new server equals one clean ingestion
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let mut m2 = mcp!(w, &[&project], OPT_IN, &[]);
    let r = m2.ingest(&dynamic);
    assert_eq!(r["isError"], false, "{r}");
    let reference = World::new();
    let rdir = reference.dir("dynamic", SYMBOL_FILES);
    let rp = reference.dir("project", &[("a.md", "# A\n")]);
    let mut mr = mcp!(reference, &[&rp, &rdir], &[], &[]);
    assert_eq!(
        mr.call(
            "repo_ingest",
            json!({"path": rdir.to_str().unwrap(), "spaces": ["p"]})
        )["isError"],
        false
    );
    assert_eq!(w.structure(), reference.structure());
    m2.finish();
    mr.finish();
}

// 2 ------------------------------------------------------------------------------------------

#[test]
fn frontend_crash_right_after_commit_keeps_the_data_frees_the_lock_and_a_retry_adds_no_structure() {
    let w = World::new();
    let (project, dynamic) = grant_world(&w, SYMBOL_FILES);
    let pause = Pause::new(&w, "p2");
    let mut m = mcp!(
        w,
        &[&project],
        OPT_IN,
        &[("MINDSPACE_ISOLATED_GATE_PAUSE_COMMITTED", pause.var())]
    );
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let before_rows = w.rows();
    m.send_call(
        TOOL,
        json!({"path": dynamic.to_str().unwrap(), "spaces": ["p"]}),
    );
    pause.wait_reached(); // COMMIT returned; CommitDone not sent; the broker still holds the lock
    assert!(
        w.rows() > before_rows,
        "committed and visible to another connection"
    );
    let committed = w.dump();
    let gid = w.grant_id(&dynamic);
    let mut revoke = w.revoke_bg(&gid);
    assert!(
        stays_blocked(&mut revoke, Duration::from_millis(300)),
        "the lease is still held"
    );
    kill(m.child.id() as i32);
    let _ = m.child.wait();
    // the hang-up releases the lock: the revocation completes, long before the 3 s ceiling
    let waited = exits_within(&mut revoke, Duration::from_secs(2))
        .expect("the lock leaked after the frontend died");
    assert!(revoke.wait().unwrap().success());
    assert_eq!(w.integrity(), "ok");
    assert_eq!(w.dump(), committed, "the crash changed nothing");
    // retry (the client never got an answer): no duplicate structure, one more observation
    let structure = w.structure();
    let observations = w.count("ingest_sessions");
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let mut m2 = mcp!(w, &[&project], OPT_IN, &[]);
    let r = m2.ingest(&dynamic);
    assert_eq!(r["isError"], false, "{r}");
    assert!(
        r["structuredContent"]["context_exchange"]["known_derivations_reused"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert_eq!(
        w.structure(),
        structure,
        "a retry after an uncertain outcome adds no derived structure"
    );
    assert_eq!(
        w.count("ingest_sessions"),
        observations + 1,
        "documented: exactly-once needs operation ids"
    );
    eprintln!("frontend crash after COMMIT: lock released {waited:?} after the crash");
    m2.finish();
}

// 3 ------------------------------------------------------------------------------------------

#[test]
fn broker_crash_before_the_commit_is_granted_rolls_back() {
    let w = World::new();
    let (project, dynamic) = grant_world(&w, SYMBOL_FILES);
    let pause = Pause::new(&w, "p3");
    let mut m = mcp!(
        w,
        &[&project],
        OPT_IN,
        &[("MINDSPACE_ISOLATED_GATE_PAUSE_BEFORE", pause.var())]
    );
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let before = w.dump();
    let id = m.send_call(
        TOOL,
        json!({"path": dynamic.to_str().unwrap(), "spaces": ["p"]}),
    );
    pause.wait_reached();
    kill(m.broker);
    assert!(gone(m.broker as u32));
    pause.go();
    let r = m.read_result(id);
    assert_eq!(err_kind(&r.clone()).as_str(), "broker_unavailable", "{r}");
    assert_eq!(fault_state(&r).0, "ROLLED_BACK");
    assert_eq!(w.dump(), before, "no unauthorized commit");
    assert!(alive(m.child.id()));
    m.finish();
}

// 4 ------------------------------------------------------------------------------------------

#[test]
fn broker_crash_while_holding_the_grant_lock_frees_it_and_the_frontend_does_not_commit() {
    let w = World::new();
    let (project, dynamic) = grant_world(&w, SYMBOL_FILES);
    let pause = Pause::new(&w, "p4");
    let mut m = mcp!(
        w,
        &[&project],
        OPT_IN,
        &[("MINDSPACE_ISOLATED_GATE_PAUSE_AFTER", pause.var())]
    );
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let before = w.dump();
    let id = m.send_call(
        TOOL,
        json!({"path": dynamic.to_str().unwrap(), "spaces": ["p"]}),
    );
    pause.wait_reached(); // lease granted, lock held, nothing committed
    let gid = w.grant_id(&dynamic);
    let mut revoke = w.revoke_bg(&gid);
    assert!(stays_blocked(&mut revoke, Duration::from_millis(300)));
    kill(m.broker); // the lock dies with the process
    let waited = exits_within(&mut revoke, Duration::from_secs(2))
        .expect("the lock leaked after the broker died");
    assert!(revoke.wait().unwrap().success());
    // the revocation has completed; only now does the frontend resume: it must NOT commit
    pause.go();
    let r = m.read_result(id);
    assert_eq!(err_kind(&r.clone()).as_str(), "broker_unavailable", "{r}");
    assert_eq!(fault_state(&r).0, "ROLLED_BACK");
    assert_eq!(w.dump(), before, "no commit after the revocation completed");
    eprintln!("broker crash during the lease: revocation completed {waited:?} after the kill");
    m.finish();
}

// 5 ------------------------------------------------------------------------------------------

#[test]
fn a_lost_completion_message_costs_the_frontend_at_the_in_flight_ceiling_and_the_data_is_committed()
{
    let w = World::new();
    let (project, dynamic) = grant_world(&w, SYMBOL_FILES);
    let mut m = mcp!(
        w,
        &[&project],
        OPT_IN,
        &[
            ("MINDSPACE_ISOLATED_GATE_DROP_COMMIT_DONE", "1".into()),
            ("MINDSPACE_BROKER_GATE_COMMIT_INFLIGHT_MS", "2500".into()),
        ]
    );
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let frontend = m.child.id();
    let r = m.ingest(&dynamic);
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["commit_state"], "COMMITTED");
    assert!(w.count("code_derivations") > 0);
    let committed = w.dump();
    let gid = w.grant_id(&dynamic);
    let mut revoke = w.revoke_bg(&gid);
    let waited = exits_within(&mut revoke, Duration::from_secs(10)).expect("the lease never ended");
    assert!(revoke.wait().unwrap().success());
    assert!(
        waited >= Duration::from_millis(2000),
        "the lease must hold to the ceiling: {waited:?}"
    );
    // the broker cannot tell a lost message from a stalled COMMIT: it removed the frontend first
    assert!(!alive(frontend));
    assert_eq!(w.dump(), committed);
    eprintln!(
        "lost CommitDone: revoke waited {waited:?}, the frontend was removed, data stays committed"
    );
}

// 6 ------------------------------------------------------------------------------------------

#[test]
fn a_commit_decision_that_arrives_too_late_rolls_back_and_the_link_is_not_reused() {
    let w = World::new();
    let (project, dynamic) = grant_world(&w, SYMBOL_FILES);
    let mut m = mcp!(
        w,
        &[&project],
        OPT_IN,
        &[("MINDSPACE_BROKER_GATE_COMMIT_REPLY_DELAY_MS", "4000".into())]
    );
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let before = w.dump();
    let t = Instant::now();
    let r = m.ingest(&dynamic);
    let took = t.elapsed();
    assert_eq!(err_kind(&r), "commit_not_authorized", "{r}");
    assert_eq!(fault_state(&r).0, "ROLLED_BACK");
    assert!(
        took >= Duration::from_millis(2800) && took < Duration::from_secs(8),
        "{took:?}"
    );
    assert_eq!(
        w.dump(),
        before,
        "a lost answer is a refusal, never a commit"
    );
    // the broker may still answer later: the link is out of step and is not used again
    let r = m.ingest(&dynamic);
    assert_eq!(err_kind(&r), "broker_unavailable", "{r}");
    // the lease the broker granted late ends at once (the frontend's best-effort release)
    let gid = w.grant_id(&dynamic);
    let mut revoke = w.revoke_bg(&gid);
    assert!(
        exits_within(&mut revoke, Duration::from_secs(8)).is_some(),
        "the late lease leaked"
    );
    m.finish();
}

// 7 ------------------------------------------------------------------------------------------

#[test]
fn a_frontend_stalled_after_the_commit_is_removed_and_the_committed_data_stays() {
    let w = World::new();
    let (project, dynamic) = grant_world(&w, SYMBOL_FILES);
    let pause = Pause::new(&w, "p7");
    let mut m = mcp!(
        w,
        &[&project],
        OPT_IN,
        &[
            ("MINDSPACE_ISOLATED_GATE_PAUSE_COMMITTED", pause.var()),
            ("MINDSPACE_BROKER_GATE_COMMIT_INFLIGHT_MS", "2500".into()),
        ]
    );
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let frontend = m.child.id();
    m.send_call(
        TOOL,
        json!({"path": dynamic.to_str().unwrap(), "spaces": ["p"]}),
    );
    pause.wait_reached(); // COMMITTED; the frontend sits on its completion message
    let committed = w.dump();
    let gid = w.grant_id(&dynamic);
    let mut revoke = w.revoke_bg(&gid);
    let waited = exits_within(&mut revoke, Duration::from_secs(10))
        .expect("the ceiling did not end the lease");
    assert!(revoke.wait().unwrap().success());
    assert!(waited >= Duration::from_millis(2000), "{waited:?}");
    assert!(!alive(frontend), "removed before the lock was released");
    assert_eq!(w.integrity(), "ok");
    assert_eq!(w.dump(), committed, "the data is committed and stays");
}

// 8 ------------------------------------------------------------------------------------------

#[test]
fn revocation_racing_the_commit_boundary_is_always_ordered_one_way_or_the_other() {
    let w = World::new();
    let (project, dynamic) = grant_world(&w, SYMBOL_FILES);
    let pause = Pause::new(&w, "p8");
    let mut m = mcp!(
        w,
        &[&project],
        OPT_IN,
        &[
            ("MINDSPACE_ISOLATED_GATE_PAUSE_BEFORE", pause.var()),
            ("MINDSPACE_ISOLATED_GATE_MARK_COMMITTED", pause.var()),
        ]
    );
    let (mut committed, mut denied) = (0, 0);
    for i in 0..14 {
        w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
        pause.reset();
        let before = w.dump();
        let id = m.send_call(
            TOOL,
            json!({"path": dynamic.to_str().unwrap(), "spaces": ["p"]}),
        );
        pause.wait_reached();
        let gid = w.grant_id(&dynamic);
        // the two orders alternate: revocation first / release first. No sleeps between them.
        let mut revoke;
        if i % 2 == 0 {
            revoke = w.revoke_bg(&gid);
            pause.go();
        } else {
            pause.go();
            revoke = w.revoke_bg(&gid);
        }
        let r = m.read_result(id);
        assert!(exits_within(&mut revoke, Duration::from_secs(8)).is_some());
        let revoke_done = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        assert!(revoke.wait().unwrap().success());
        if r["isError"] == false {
            committed += 1;
            // a revocation can complete only after the commit it raced with: the lock orders it
            assert!(
                pause.committed_ns() <= revoke_done,
                "iteration {i}: the revocation completed BEFORE the commit"
            );
            assert!(w.dump() != before);
        } else {
            denied += 1;
            assert_eq!(err_kind(&r), "revoked", "iteration {i}: {r}");
            assert_eq!(
                w.dump(),
                before,
                "iteration {i}: a denied commit left a trace"
            );
            assert!(!pause.file(".committed").exists());
        }
    }
    eprintln!("racing revocation, 14 iterations: {committed} committed (revoke ordered after), {denied} denied (rolled back)");
    m.finish();
}

// 9 ------------------------------------------------------------------------------------------

#[test]
fn a_failed_sqlite_commit_is_rolled_back_and_costs_no_revision() {
    let w = World::new();
    let (project, dynamic) = grant_world(&w, SYMBOL_FILES);
    let mut m = mcp!(
        w,
        &[&project],
        OPT_IN,
        &[("MINDSPACE_STORE_GATE_FAIL_COMMIT", "1".into())]
    );
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let before = w.dump();
    let revisions = w.count("m6_revisions");
    // a real SQLite COMMIT failure (the commit hook vetoes): SQLite turns it into a rollback
    let r = m.ingest(&dynamic);
    assert_eq!(err_kind(&r), "persist_failed", "{r}");
    assert_eq!(fault_state(&r).0, "ROLLED_BACK");
    assert_eq!(w.dump(), before);
    assert_eq!(
        w.count("m6_revisions"),
        revisions,
        "a rolled-back ingestion consumed a revision"
    );
    assert_eq!(w.integrity(), "ok");
    // the lease ended with the attempt: a revocation is not delayed
    let gid = w.grant_id(&dynamic);
    let mut revoke = w.revoke_bg(&gid);
    assert!(
        exits_within(&mut revoke, Duration::from_secs(2)).is_some(),
        "the lock leaked"
    );
    // the fault was one-shot: a retry succeeds and costs exactly one revision
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let r = m.ingest(&dynamic);
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(w.count("m6_revisions"), revisions + 1);
    m.finish();
}

#[test]
fn an_unknown_commit_outcome_is_never_reported_as_success_and_a_retry_is_consistent() {
    let w = World::new();
    let (project, dynamic) = grant_world(&w, SYMBOL_FILES);
    let mut m = mcp!(
        w,
        &[&project],
        OPT_IN,
        &[("MINDSPACE_STORE_GATE_LOSE_ACK", "1".into())]
    );
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let before_rows = w.rows();
    // the COMMIT really succeeds, but the storage layer reports that it does not know
    let r = m.ingest(&dynamic);
    assert_eq!(r["isError"], true, "no false success: {r}");
    assert_eq!(err_kind_unknown(&r), "commit_outcome_unknown", "{r}");
    assert_eq!(fault_state(&r), ("UNKNOWN".to_string(), json!("unknown")));
    assert!(
        w.rows() > before_rows,
        "the data is in fact committed: the tool must not claim otherwise"
    );
    assert_eq!(w.integrity(), "ok");
    let structure = w.structure();
    // the handle was discarded (as for a real unknown outcome): this server says so explicitly
    let r = m.ingest(&dynamic);
    assert_eq!(err_kind(&r), "store_discarded", "{r}");
    m.finish();
    // the documented remedy: restart, then ingest the same directory again. It reuses everything.
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let mut m2 = mcp!(w, &[&project], OPT_IN, &[]);
    let r = m2.ingest(&dynamic);
    assert_eq!(r["isError"], false, "{r}");
    assert!(
        r["structuredContent"]["context_exchange"]["known_derivations_reused"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert_eq!(
        w.structure(),
        structure,
        "no inconsistent or duplicate derivations after the retry"
    );
    assert_eq!(w.integrity(), "ok");
    m2.finish();
}

fn err_kind_unknown(r: &Value) -> String {
    r["structuredContent"]["error"]["kind"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

// ------------------------------------------------------------------ COMMIT held beyond the lease
//
// Invariant I: a `mind grant revoke` that returns success never completes while an
// already-authorized COMMIT is still in flight (the committing process alive, the COMMIT not
// finished). The COMMIT must have finished, or the process that could finish it must be dead.

/// Who holds a flock on the grant store, from the kernel's own table: `(pid, "READ"|"WRITE")`.
fn store_lock_holders(w: &World) -> Vec<(u32, String)> {
    use std::os::unix::fs::MetadataExt;
    let ino = std::fs::metadata(w.state.join("mindspace/grants.jsonl"))
        .unwrap()
        .ino();
    std::fs::read_to_string("/proc/locks")
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("FLOCK"))
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            // "1: FLOCK ADVISORY READ 4242 08:01:123456 0 EOF"
            let inode: u64 = f.get(5)?.rsplit(':').next()?.parse().ok()?;
            (inode == ino).then(|| (f[4].parse().unwrap_or(0), f[3].to_string()))
        })
        .collect()
}

#[test]
fn a_commit_held_inside_sqlite_beyond_the_ceiling_is_aborted_by_removing_the_frontend_before_the_revocation_completes(
) {
    let w = World::new();
    let (project, dynamic) = grant_world(&w, SYMBOL_FILES);
    let pause = Pause::new(&w, "inflight");
    let mut m = mcp!(
        w,
        &[&project],
        OPT_IN,
        &[
            ("MINDSPACE_STORE_GATE_PAUSE_IN_COMMIT", pause.var()),
            ("MINDSPACE_BROKER_GATE_COMMIT_INFLIGHT_MS", "2500".into()),
        ]
    );
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let before = w.dump();
    let frontend = m.child.id();
    m.send_call(
        TOOL,
        json!({"path": dynamic.to_str().unwrap(), "spaces": ["p"]}),
    );
    pause.wait_reached(); // the COMMIT is running inside SQLite and is held there
    let holders = store_lock_holders(&w);
    assert!(
        holders.contains(&(m.broker as u32, "READ".to_string())),
        "{holders:?}"
    );
    let gid = w.grant_id(&dynamic);
    let mut revoke = w.revoke_bg(&gid);
    // inside the in-flight ceiling the revocation WAITS (the old behaviour released at 3 s)
    assert!(
        stays_blocked(&mut revoke, Duration::from_millis(1500)),
        "overtaken while the COMMIT was in flight"
    );
    assert!(alive(frontend), "removed before the in-flight ceiling");
    // beyond the ceiling the frontend is removed FIRST, then the lock is released
    let waited = exits_within(&mut revoke, Duration::from_secs(10)).expect("the lease never ended");
    assert!(revoke.wait().unwrap().success());
    assert!(
        !alive(frontend),
        "VIOLATION: the revocation completed while the committing process was alive"
    );
    assert_eq!(w.integrity(), "ok");
    assert_eq!(w.dump(), before, "the aborted COMMIT left nothing");
    // letting the held COMMIT go changes nothing: the process that could finish it is dead
    pause.go();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        w.dump(),
        before,
        "a COMMIT finished after the revocation completed"
    );
    eprintln!("COMMIT in flight beyond the ceiling: revocation completed {waited:?} after it began blocking, frontend removed first, database unchanged");
}

#[test]
fn a_commit_in_flight_within_the_ceiling_finishes_before_the_revocation_completes() {
    let w = World::new();
    let (project, dynamic) = grant_world(&w, SYMBOL_FILES);
    let pause = Pause::new(&w, "inflight_ok");
    let mut m = mcp!(
        w,
        &[&project],
        OPT_IN,
        &[
            ("MINDSPACE_STORE_GATE_PAUSE_IN_COMMIT", pause.var()),
            ("MINDSPACE_ISOLATED_GATE_MARK_COMMITTED", pause.var()),
        ]
    );
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let before_rows = w.rows();
    let id = m.send_call(
        TOOL,
        json!({"path": dynamic.to_str().unwrap(), "spaces": ["p"]}),
    );
    pause.wait_reached();
    let gid = w.grant_id(&dynamic);
    let mut revoke = w.revoke_bg(&gid);
    // well past the old 3 s ceiling: still waiting, because the COMMIT is still running
    assert!(stays_blocked(&mut revoke, Duration::from_millis(3500)));
    assert_eq!(w.rows(), before_rows);
    pause.go();
    let r = m.read_result(id);
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["commit_state"], "COMMITTED");
    assert!(exits_within(&mut revoke, Duration::from_secs(5)).is_some());
    let revoke_done = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    assert!(revoke.wait().unwrap().success());
    assert!(
        pause.committed_ns() <= revoke_done,
        "the revocation completed before the COMMIT finished"
    );
    assert!(w.rows() > before_rows);
    m.finish();
}

#[test]
fn a_frontend_that_dies_during_the_commit_aborts_it_and_frees_the_lock() {
    let w = World::new();
    let (project, dynamic) = grant_world(&w, SYMBOL_FILES);
    let pause = Pause::new(&w, "inflight_crash");
    let mut m = mcp!(
        w,
        &[&project],
        OPT_IN,
        &[("MINDSPACE_STORE_GATE_PAUSE_IN_COMMIT", pause.var())]
    );
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let before = w.dump();
    m.send_call(
        TOOL,
        json!({"path": dynamic.to_str().unwrap(), "spaces": ["p"]}),
    );
    pause.wait_reached();
    let gid = w.grant_id(&dynamic);
    let mut revoke = w.revoke_bg(&gid);
    assert!(stays_blocked(&mut revoke, Duration::from_millis(300)));
    kill(m.child.id() as i32);
    let _ = m.child.wait();
    let waited = exits_within(&mut revoke, Duration::from_secs(2))
        .expect("the hang-up did not release the lock");
    assert!(revoke.wait().unwrap().success());
    assert_eq!(w.integrity(), "ok");
    assert_eq!(w.dump(), before, "the COMMIT died with the process");
    eprintln!("frontend death during COMMIT: revocation completed {waited:?} after the kill; database unchanged");
}

#[test]
fn a_broker_that_dies_during_the_commit_loses_the_lease_which_is_the_documented_weaker_guarantee() {
    // PINNED LIMIT, not a goal: the lock dies with the broker, so a revocation can complete while
    // a COMMIT that had already begun is still running, and that COMMIT is not aborted. What is
    // guaranteed: a commit that had NOT begun is refused (the begin message fails), and success is
    // reported only because SQLite said so, never because the broker did.
    let w = World::new();
    let (project, dynamic) = grant_world(&w, SYMBOL_FILES);
    let pause = Pause::new(&w, "inflight_broker");
    let mut m = mcp!(
        w,
        &[&project],
        OPT_IN,
        &[
            ("MINDSPACE_STORE_GATE_PAUSE_IN_COMMIT", pause.var()),
            ("MINDSPACE_ISOLATED_GATE_MARK_COMMITTED", pause.var()),
        ]
    );
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let before_rows = w.rows();
    let id = m.send_call(
        TOOL,
        json!({"path": dynamic.to_str().unwrap(), "spaces": ["p"]}),
    );
    pause.wait_reached();
    kill(m.broker);
    let gid = w.grant_id(&dynamic);
    let mut revoke = w.revoke_bg(&gid);
    assert!(
        exits_within(&mut revoke, Duration::from_secs(3)).is_some(),
        "the lock must die with the broker"
    );
    assert!(revoke.wait().unwrap().success());
    // causal order, no clocks: the COMMIT is released only NOW, after the revocation completed
    assert!(!pause.file(".go").exists());
    assert_eq!(
        w.rows(),
        before_rows,
        "the COMMIT had not finished when the revocation completed"
    );
    pause.go();
    let r = m.read_result(id);
    assert_eq!(r["isError"], false, "SQLite said committed: {r}");
    assert!(
        w.rows() > before_rows,
        "and that COMMIT then finished: it was not aborted"
    );
    m.finish();
}
