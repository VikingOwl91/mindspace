//! M7.3b.5e: `repo_ingest_git_isolated` over the real confined `mind mcp`.
//!
//! The production binary is started with `--grants --isolated-git-ingest --git-exe /usr/bin/git`
//! and spoken to over JSON-RPC: real Landlock, seccomp and cgroups, a real watchdog and broker,
//! the real Git worker. Test-only parts (feature `spawn-gate`, compiled into `cargo test` builds
//! only): hostile worker modes, pauses at persistence boundaries, store faults. The frontend is
//! never a stand-in. Skipped with a message without cgroup delegation.

#![allow(clippy::field_reassign_with_default, dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn mind() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mind"))
}

const TOOL: &str = "repo_ingest_git_isolated";
const DIR_TOOL: &str = "repo_ingest_isolated";
const GIT: &str = "/usr/bin/git";
const MARKER: &str = "0190b2c4-7d5e-7a31-9c11-3f2d5a6b7c8d";

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
    /// A Git repository (HEAD with two source files and a README), identity marker optional.
    fn repo_with(&self, name: &str, marker: Option<&str>, format: &str) -> Option<PathBuf> {
        let d = self.root.join(name);
        std::fs::create_dir_all(d.join("src")).unwrap();
        let g = |args: &[&str]| {
            Command::new(GIT)
                .arg("-C")
                .arg(&d)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@e",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .env_clear()
                .output()
                .unwrap()
        };
        let init = if format == "sha1" {
            g(&["init", "-q", "-b", "main"])
        } else {
            g(&[
                "init",
                "-q",
                "-b",
                "main",
                &format!("--object-format={format}"),
            ])
        };
        if !init.status.success() {
            return None;
        }
        std::fs::write(
            d.join("src/lib.rs"),
            "pub mod util;\npub fn run() { util::helper(); }\n",
        )
        .unwrap();
        std::fs::write(d.join("src/util.rs"), "pub fn helper() {}\n").unwrap();
        std::fs::write(d.join("README.md"), "# R\n\nalpha\n").unwrap();
        assert!(g(&["add", "-A"]).status.success());
        assert!(g(&["commit", "-q", "-m", "one"]).status.success());
        if let Some(m) = marker {
            std::fs::write(d.join(".git/mindspace-repository-id"), m).unwrap();
        }
        Some(d)
    }
    fn repo(&self, name: &str) -> PathBuf {
        self.repo_with(name, Some(MARKER), "sha1").unwrap()
    }
    fn head(&self, repo: &Path) -> String {
        let o = Command::new(GIT)
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "HEAD"])
            .env_clear()
            .output()
            .unwrap();
        String::from_utf8(o.stdout).unwrap().trim().to_string()
    }
    fn counts(&self) -> std::collections::BTreeMap<String, i64> {
        let c = rusqlite::Connection::open_with_flags(
            &self.db,
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
                let n = c
                    .query_row(&format!("SELECT COUNT(*) FROM \"{t}\""), [], |r| r.get(0))
                    .unwrap();
                (t, n)
            })
            .collect()
    }
    fn revisions(&self) -> i64 {
        self.counts()["m6_revisions"]
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
        if extra.contains(&"--isolated-ingest") || extra.contains(&"--isolated-git-ingest") {
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

const OPT_IN: &[&str] = &["--grants", "--isolated-git-ingest", "--git-exe", GIT];

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

fn err_kind_any(r: &Value) -> String {
    r["structuredContent"]["error"]["kind"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

/// A failure that wrote nothing and says so.
fn assert_refused(r: &Value, kind: &str) {
    assert_eq!(r["isError"], true, "{r}");
    assert_eq!(err_kind_any(r), kind, "{r}");
    assert_eq!(r["structuredContent"]["persisted"], false, "{r}");
    assert_eq!(r["structuredContent"]["commit_state"], "NOT_STARTED", "{r}");
}

fn stderr_of(w: &World) -> String {
    std::fs::read_to_string(w.root.join("mcp.stderr")).unwrap_or_default()
}

fn code_and_git(w: &World) -> std::collections::BTreeMap<String, i64> {
    w.counts()
        .into_iter()
        .filter(|(k, _)| k.starts_with("code_") || k.starts_with("git_"))
        .collect()
}

fn gate_git(mode: &str) -> Vec<(&'static str, String)> {
    vec![
        ("MINDSPACE_BROKER_GATE_GIT", GIT.to_string()),
        ("MINDSPACE_BROKER_GATE_GIT_MODE", mode.to_string()),
    ]
}

fn kill(pid: i32) {
    // SAFETY: plain kill(2) on a pid the test just looked up.
    unsafe { libc::kill(pid, libc::SIGKILL) };
}

// ------------------------------------------------------------------ gates and listing

#[test]
fn the_tool_exists_only_when_every_gate_passes_and_the_default_listing_is_unchanged() {
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n")]);
    let mut m = mcp!(w, &[&project], &[], &[]);
    let base_raw = m.rpc("tools/list", json!({}))["result"]["tools"].clone();
    let base = m.tools();
    assert!(!base.iter().any(|t| t == TOOL));
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
    m.finish();

    // --git-exe alone, --broker alone, --grants alone: nothing changes
    for extra in [
        vec!["--git-exe", GIT],
        vec!["--broker"],
        vec!["--grants", "--git-exe", GIT],
    ] {
        let mut m = mcp!(w, &[&project], &extra, &[]);
        assert_eq!(m.tools(), base, "{extra:?}");
        m.finish();
    }

    // --isolated-git-ingest without an administrator executable: one line, no tool
    let mut m = mcp!(w, &[&project], &["--grants", "--isolated-git-ingest"], &[]);
    assert_eq!(m.tools(), base);
    m.finish();
    assert!(
        stderr_of(&w).contains("needs --git-exe"),
        "{}",
        stderr_of(&w)
    );

    // all gates: the base list is untouched (byte for byte) and the tool is added last
    let mut m = mcp!(w, &[&project], OPT_IN, &[]);
    let listed = m.rpc("tools/list", json!({}))["result"]["tools"].clone();
    let with = listed.as_array().unwrap();
    assert_eq!(
        serde_json::to_string(&with[..with.len() - 1]).unwrap(),
        serde_json::to_string(&base_raw).unwrap(),
        "the default tool definitions are byte-identical"
    );
    let def = with.last().unwrap();
    assert_eq!(def["name"], TOOL);
    let props = def["inputSchema"]["properties"].as_object().unwrap();
    assert_eq!(props.keys().collect::<Vec<_>>(), ["path", "spaces"]);
    assert_eq!(def["inputSchema"]["additionalProperties"], false);
    m.finish();

    // both isolated tools: independent flags, shared channel
    let mut m = mcp!(
        w,
        &[&project],
        &[
            "--grants",
            "--isolated-ingest",
            "--isolated-git-ingest",
            "--git-exe",
            GIT
        ],
        &[]
    );
    let names = m.tools();
    assert_eq!(&names[names.len() - 2..], [DIR_TOOL, TOOL]);
    m.finish();
    // the directory tool alone is unchanged by this feature
    let mut m = mcp!(w, &[&project], &["--grants", "--isolated-ingest"], &[]);
    let names = m.tools();
    assert_eq!(names.last().unwrap(), DIR_TOOL);
    assert!(!names.iter().any(|t| t == TOOL));
    m.finish();
}

#[test]
fn an_executable_that_fails_the_trust_policy_means_no_tool() {
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n")]);
    let base = {
        let mut m = mcp!(w, &[&project], &[], &[]);
        let b = m.tools();
        m.finish();
        b
    };
    let bin = w.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // a user-owned copy of the real git: the file and the directory are not root-owned
    let copy = bin.join("git");
    std::fs::copy(GIT, &copy).unwrap();
    let script = bin.join("git-script");
    std::fs::write(&script, "#!/bin/sh\nexec /usr/bin/git \"$@\"\n").unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let link = bin.join("git-link");
    std::os::unix::fs::symlink(GIT, &link).unwrap();
    let cases: Vec<(String, &str)> = vec![
        (copy.display().to_string(), "user-owned copy"),
        (script.display().to_string(), "script"),
        (
            link.display().to_string(),
            "symlink in a user-owned directory",
        ),
        ("git".to_string(), "bare name (no PATH lookup)"),
        ("/nonexistent/git".to_string(), "missing"),
        ("/tmp".to_string(), "directory"),
    ];
    for (exe, what) in cases {
        let mut m = mcp!(
            w,
            &[&project],
            &["--grants", "--isolated-git-ingest", "--git-exe", &exe],
            &[]
        );
        assert_eq!(m.tools(), base, "{what}: the tool must not be offered");
        let r = m.call(
            TOOL,
            json!({"path": project.to_str().unwrap(), "spaces": ["p"]}),
        );
        assert!(r["rpc_error"].is_object(), "{what}: {r}");
        m.finish();
        assert!(
            stderr_of(&w).contains("NOT offered"),
            "{what}: one line says why: {}",
            stderr_of(&w)
        );
    }
}

#[test]
fn the_caller_cannot_supply_an_executable_environment_arguments_a_grant_or_a_limit() {
    let w = World::new();
    let repo = w.repo("repo");
    let mut m = mcp!(w, &[&repo], OPT_IN, &[]);
    let before = w.counts();
    for (k, v) in [
        ("git_exe", json!("/bin/sh")),
        ("executable", json!("/usr/bin/git")),
        ("git", json!("/usr/bin/git")),
        ("env", json!({"GIT_DIR": "/etc"})),
        ("args", json!(["--exec-path=/tmp"])),
        ("grant", json!("any")),
        ("grant_id", json!("00000000-0000-0000-0000-000000000000")),
        ("max_files", json!(1_000_000)),
        ("timeout", json!(1)),
        ("ref", json!("refs/heads/main")),
        ("history", json!(true)),
    ] {
        let r = m.call(
            TOOL,
            json!({"path": repo.to_str().unwrap(), "spaces": ["p"], k: v}),
        );
        let text = r.to_string();
        assert!(
            r["rpc_error"].is_object() || r["isError"] == true,
            "{k} accepted: {r}"
        );
        assert!(
            text.contains(k) || text.contains("unknown") || text.contains("unexpected"),
            "{k}: {r}"
        );
    }
    // missing / malformed required fields
    for args in [
        json!({"path": repo.to_str().unwrap()}),
        json!({"spaces": ["p"]}),
        json!({"path": "relative/path", "spaces": ["p"]}),
        json!({"path": repo.to_str().unwrap(), "spaces": []}),
        json!({"path": repo.to_str().unwrap(), "spaces": ["p", "p"]}),
        json!({"path": repo.to_str().unwrap(), "spaces": ["nope"]}),
        json!({"path": repo.to_str().unwrap(), "spaces": [1]}),
    ] {
        let r = m.call(TOOL, args.clone());
        assert!(
            r["rpc_error"].is_object() || r["isError"] == true,
            "{args}: {r}"
        );
    }
    assert_eq!(w.counts(), before, "refused calls change nothing");
    m.finish();
}

// ------------------------------------------------------------------ success, scope, repetition

#[test]
fn head_only_ingestion_reports_what_was_verified_and_what_was_not_and_repeats_without_duplicates() {
    let w = World::new();
    let repo = w.repo("repo");
    // a second commit and a side branch: only HEAD is in scope
    let g = |args: &[&str]| {
        assert!(Command::new(GIT)
            .arg("-C")
            .arg(&repo)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@e",
                "-c",
                "commit.gpgsign=false"
            ])
            .args(args)
            .env_clear()
            .output()
            .unwrap()
            .status
            .success());
    };
    g(&["branch", "side"]);
    std::fs::write(repo.join("src/more.rs"), "pub fn more() {}\n").unwrap();
    g(&["add", "-A"]);
    g(&["commit", "-q", "-m", "two"]);
    let head = w.head(&repo);
    let mut m = mcp!(w, &[&repo], OPT_IN, &[]);
    let rev0 = w.revisions();
    let r = m.ingest(&repo);
    assert_eq!(r["isError"], false, "{r}");
    let s = &r["structuredContent"];
    assert_eq!(s["repository"]["id"], MARKER);
    assert_eq!(s["repository"]["path"], repo.to_str().unwrap());
    assert_eq!(s["head"]["commit"], head);
    assert_eq!(s["head"]["symbolic_ref"], "refs/heads/main");
    assert_eq!(s["head"]["scope"], "head_only");
    assert_eq!(s["persisted"], true);
    assert_eq!(s["commit_state"], "COMMITTED");
    assert_eq!(w.revisions(), rev0 + 1, "one recording revision");
    assert_eq!(s["persisted_revision"].as_i64().unwrap(), w.revisions());
    assert!(s["counts"]["objects_new"].as_u64().unwrap() > 0);
    assert!(s["counts"]["code"]["parsed"].as_u64().unwrap() >= 3, "{s}");
    // the result keeps the two claims apart
    let v = &s["verification"];
    assert!(v["internally_verified"]
        .as_str()
        .unwrap()
        .contains("hashes"));
    assert!(v["not_verified"].to_string().contains("real HEAD"));
    assert!(v["not_verified"].to_string().contains("SHA-1"));
    // confined worker evidence
    let wk = &s["isolation"]["worker"];
    assert_eq!(wk["seccomp"], "2");
    assert_eq!(wk["no_new_privs"], "1");
    assert_eq!(wk["contained_in_server_cgroup"], true);
    // HEAD only: the commit of the side branch is the PARENT-less? it is HEAD~0's sibling; the
    // history behind HEAD is not ingested as commits of other refs
    let hit = m.call("code_search", json!({"query": "more"}));
    assert!(
        !hit["structuredContent"]["hits"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{hit}"
    );
    // repeat: nothing new, placeholders reused, structure stable
    let structure = code_and_git(&w);
    let r2 = m.ingest(&repo);
    assert_eq!(r2["isError"], false, "{r2}");
    assert_eq!(r2["structuredContent"]["head"]["commit"], head);
    assert_eq!(r2["structuredContent"]["counts"]["objects_new"], 0, "{r2}");
    assert!(
        r2["structuredContent"]["context_exchange"]["known_derivations_reused"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert_eq!(code_and_git(&w), structure, "no duplicates");
    m.finish();
}

// ------------------------------------------------------------------ authority

#[test]
fn static_and_dynamic_authority_root_identity_and_unsupported_repositories() {
    let w = World::new();
    let project = w.repo("project");
    let dynamic = w
        .repo_with(
            "dynamic",
            Some("0190b2c4-7d5e-7a31-9c11-3f2d5a6b7c99"),
            "sha1",
        )
        .unwrap();
    let outside = w.repo("outside");
    let mut m = mcp!(w, &[&project], OPT_IN, &[]);
    // static root
    let r = m.ingest(&project);
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(
        r["structuredContent"]["isolation"]["authority"],
        "static_root"
    );
    // not authorized (no root, no grant)
    let before = w.counts();
    assert_refused(&m.ingest(&dynamic), "root_not_authorized");
    assert_refused(&m.ingest(&outside), "root_not_authorized");
    assert_eq!(w.counts(), before);
    // root mismatch: the broker matches the exact root, not a spelling of it
    for spelled in [
        format!("{}/", project.display()),
        format!("{}/.", project.display()),
        format!("{}/src/..", project.display()),
        format!("{}/src", project.display()),
        project.join(".git").display().to_string(),
    ] {
        let r = m.call(TOOL, json!({"path": spelled, "spaces": ["p"]}));
        assert_eq!(r["isError"], true, "{spelled}: {r}");
        assert_eq!(r["structuredContent"]["persisted"], false);
    }
    // a symlink to the authorized root is not the root either
    let alias = w.root.join("alias");
    std::os::unix::fs::symlink(&project, &alias).unwrap();
    let r = m.call(
        TOOL,
        json!({"path": alias.to_str().unwrap(), "spaces": ["p"]}),
    );
    assert_eq!(r["isError"], true, "symlink spelling: {r}");
    assert_eq!(r["structuredContent"]["persisted"], false);
    // a grant, added after start-up, authorizes without a restart
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let r = m.ingest(&dynamic);
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["isolation"]["authority"], "grant");
    assert_eq!(
        r["structuredContent"]["repository"]["id"],
        "0190b2c4-7d5e-7a31-9c11-3f2d5a6b7c99"
    );
    // and revoking it removes the authority
    let gid = w.grant_id(&dynamic);
    w.ok(&["grant", "revoke", &gid, "--reason", "done"]);
    assert_refused(&m.ingest(&dynamic), "root_not_authorized");
    m.finish();
}

#[test]
fn missing_identity_unsupported_layouts_and_sha256_are_refused_and_persist_nothing() {
    let w = World::new();
    let no_marker = w.repo_with("nomarker", None, "sha1").unwrap();
    let plain = w.dir("plain", &[("a.md", "# A\n")]);
    let linked = w.repo("main-wt");
    let wt = w.root.join("linked");
    assert!(Command::new(GIT)
        .arg("-C")
        .arg(&linked)
        .args(["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "other"])
        .env_clear()
        .output()
        .unwrap()
        .status
        .success());
    let with_worktrees = w.repo("withwt"); // has a `.git/worktrees` after adding one
    assert!(Command::new(GIT)
        .arg("-C")
        .arg(&with_worktrees)
        .args([
            "worktree",
            "add",
            "-q",
            w.root.join("wt2").to_str().unwrap(),
            "-b",
            "o2"
        ])
        .env_clear()
        .output()
        .unwrap()
        .status
        .success());
    let alternates = w.repo("alternates");
    std::fs::write(
        alternates.join(".git/objects/info/alternates"),
        "/nonexistent\n",
    )
    .unwrap();
    let sha256 = w.repo_with("sha256", Some(MARKER), "sha256");
    let mut roots: Vec<&Path> = vec![
        &no_marker,
        &plain,
        &linked,
        &wt,
        &with_worktrees,
        &alternates,
    ];
    if let Some(s) = sha256.as_deref() {
        roots.push(s);
    }
    let mut m = mcp!(w, &roots, OPT_IN, &[]);
    let before = w.counts();
    assert_refused(&m.ingest(&no_marker), "identity_missing");
    assert_refused(&m.ingest(&plain), "not_a_repository");
    assert_refused(&m.ingest(&wt), "unsupported_repository"); // `.git` is a file
    assert_refused(&m.ingest(&with_worktrees), "unsupported_repository");
    assert_refused(&m.ingest(&alternates), "unsupported_repository");
    assert_eq!(w.counts(), before, "refusals persist nothing");
    assert!(
        !no_marker.join(".git/mindspace-repository-id").exists(),
        "the marker is never created"
    );
    if let Some(s) = &sha256 {
        let r = m.ingest(s);
        assert_eq!(r["isError"], true, "{r}");
        assert_eq!(r["structuredContent"]["persisted"], false);
        assert_eq!(w.counts(), before);
    }
    // the server still works afterwards
    let ok = w.repo("fine");
    w.ok(&["grant", "add", ok.to_str().unwrap(), "--yes"]);
    assert_eq!(m.ingest(&ok)["isError"], false);
    m.finish();
}

// ------------------------------------------------------------------ revocation

#[test]
fn revocation_before_the_commit_denies_it_and_leaves_no_trace_and_no_revision() {
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n")]);
    let repo = w.repo("repo");
    let pause = Pause::new(&w, "before");
    let mut m = mcp!(
        w,
        &[&project],
        OPT_IN,
        &[("MINDSPACE_ISOLATED_GATE_PAUSE_BEFORE", pause.var())]
    );
    w.ok(&["grant", "add", repo.to_str().unwrap(), "--yes"]);
    let before = w.counts();
    let id = m.send_call(
        TOOL,
        json!({"path": repo.to_str().unwrap(), "spaces": ["p"]}),
    );
    pause.wait_reached();
    let gid = w.grant_id(&repo);
    w.ok(&["grant", "revoke", &gid, "--reason", "before the commit"]);
    pause.go();
    let r = m.read_result(id);
    assert_eq!(err_kind_any(&r), "revoked", "{r}");
    assert_eq!(r["structuredContent"]["persisted"], false);
    assert_eq!(r["structuredContent"]["commit_state"], "ROLLED_BACK");
    assert_eq!(
        w.counts(),
        before,
        "the denied commit left no trace, no recording revision"
    );
    m.finish();
}

#[test]
fn revocation_during_extraction_ends_the_worker_and_its_git_child() {
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n")]);
    let repo = w.repo("repo");
    let mut m = mcp!(w, &[&project], OPT_IN, &gate_git("hang"));
    w.ok(&["grant", "add", repo.to_str().unwrap(), "--yes"]);
    let before = w.counts();
    let id = m.send_call(
        TOOL,
        json!({"path": repo.to_str().unwrap(), "spaces": ["p"]}),
    );
    let worker = wait_worker(&m);
    let end = Instant::now() + Duration::from_secs(10);
    let git_child = loop {
        if let Some(c) = children_of(worker as i32).first() {
            break *c;
        }
        assert!(Instant::now() < end, "the worker started no git child");
        std::thread::sleep(Duration::from_millis(5));
    };
    let gid = w.grant_id(&repo);
    w.ok(&["grant", "revoke", &gid, "--reason", "during extraction"]);
    let r = m.read_result(id);
    assert_eq!(err_kind_any(&r), "revoked", "{r}");
    assert_eq!(r["structuredContent"]["persisted"], false);
    assert!(gone(worker), "worker gone");
    assert!(gone(git_child), "git child gone");
    assert_eq!(w.counts(), before);
    m.finish();
}

#[test]
fn a_commit_held_by_the_lease_orders_a_concurrent_revocation_after_it() {
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n")]);
    let repo = w.repo("repo");
    let pause = Pause::new(&w, "committed");
    let mut m = mcp!(
        w,
        &[&project],
        OPT_IN,
        &[("MINDSPACE_ISOLATED_GATE_PAUSE_COMMITTED", pause.var())]
    );
    w.ok(&["grant", "add", repo.to_str().unwrap(), "--yes"]);
    let rev0 = w.revisions();
    let id = m.send_call(
        TOOL,
        json!({"path": repo.to_str().unwrap(), "spaces": ["p"]}),
    );
    pause.wait_reached();
    // the commit is done; the revocation now succeeds and does not undo it
    assert_eq!(w.revisions(), rev0 + 1);
    let gid = w.grant_id(&repo);
    w.ok(&["grant", "revoke", &gid, "--reason", "after the commit"]);
    pause.go();
    let r = m.read_result(id);
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["commit_state"], "COMMITTED");
    assert_eq!(w.revisions(), rev0 + 1);
    assert_refused(&m.ingest(&repo), "root_not_authorized");
    m.finish();
}

// ------------------------------------------------------------------ hostile transport

#[test]
fn a_hostile_git_worker_is_refused_whatever_it_sends_and_persists_nothing() {
    for mode in [
        "garbage",
        "truncate",
        "oversized",
        "replay-op",
        "forge-reuse",
        "lie-header-identity",
        "lie-manifest-identity",
        "other-head-manifest",
        "wrong-root-label",
        "broker-wrong-identity",
        "crash",
    ] {
        let w = World::new();
        let repo = w.repo("repo");
        let mut m = mcp!(w, &[&repo], OPT_IN, &gate_git(mode));
        let before = w.counts();
        let r = m.ingest(&repo);
        assert_eq!(r["isError"], true, "{mode}: accepted: {r}");
        assert_eq!(r["structuredContent"]["persisted"], false, "{mode}: {r}");
        assert_eq!(
            r["structuredContent"]["commit_state"], "NOT_STARTED",
            "{mode}: {r}"
        );
        assert_eq!(w.counts(), before, "{mode}: rows changed");
        m.finish();
        // the same data through an honest server works (the refusal was about the worker)
        let mut ok = mcp!(w, &[&repo], OPT_IN, &[]);
        assert_eq!(ok.ingest(&repo)["isError"], false, "{mode}: honest run");
        ok.finish();
    }
}

// ------------------------------------------------------------------ termination

#[test]
fn killing_the_worker_the_broker_or_the_frontend_leaves_no_process_and_no_rows() {
    // worker: an error, the broker serves the next operation
    {
        let w = World::new();
        let repo = w.repo("repo");
        let mut m = mcp!(w, &[&repo], OPT_IN, &gate_git("hang"));
        let before = w.counts();
        let id = m.send_call(
            TOOL,
            json!({"path": repo.to_str().unwrap(), "spaces": ["p"]}),
        );
        let worker = wait_worker(&m);
        kill(worker as i32);
        let r = m.read_result(id);
        assert_eq!(r["isError"], true, "{r}");
        assert_eq!(r["structuredContent"]["persisted"], false);
        assert!(gone(worker));
        assert_eq!(w.counts(), before);
        m.finish();
    }
    // broker: an error, then a clear "unavailable"; worker and git child are gone
    {
        let w = World::new();
        let repo = w.repo("repo");
        let mut m = mcp!(w, &[&repo], OPT_IN, &gate_git("hang"));
        let before = w.counts();
        let id = m.send_call(
            TOOL,
            json!({"path": repo.to_str().unwrap(), "spaces": ["p"]}),
        );
        let worker = wait_worker(&m);
        let git_child = loop {
            if let Some(c) = children_of(worker as i32).first() {
                break *c;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        kill(m.broker);
        let r = m.read_result(id);
        assert_eq!(r["isError"], true, "{r}");
        assert_eq!(r["structuredContent"]["persisted"], false);
        assert!(gone(worker), "worker gone with the broker");
        assert!(gone(git_child), "git child gone with the broker");
        assert_eq!(w.counts(), before);
        let again = m.ingest(&repo);
        assert_eq!(err_kind_any(&again), "broker_unavailable", "{again}");
        m.finish();
    }
    // frontend: the watchdog ends the tree
    {
        let w = World::new();
        let repo = w.repo("repo");
        let mut m = mcp!(w, &[&repo], OPT_IN, &gate_git("hang"));
        let before = w.counts();
        let _ = m.send_call(
            TOOL,
            json!({"path": repo.to_str().unwrap(), "spaces": ["p"]}),
        );
        let worker = wait_worker(&m);
        let git_child = loop {
            if let Some(c) = children_of(worker as i32).first() {
                break *c;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let (wd, br) = (m.watchdog, m.broker);
        kill(m.child.id() as i32);
        let _ = m.child.wait();
        assert!(gone(worker), "worker gone with the frontend");
        assert!(gone(git_child), "git child gone with the frontend");
        assert!(
            gone(br as u32) && gone(wd as u32),
            "broker and watchdog gone"
        );
        assert_eq!(w.counts(), before);
    }
}

// ------------------------------------------------------------------ commit outcomes

#[test]
fn a_failed_commit_is_a_clean_rollback_and_an_unknown_outcome_is_reported_as_unknown() {
    // vetoed COMMIT: certain rollback, no revision, the handle stays usable
    {
        let w = World::new();
        let repo = w.repo("repo");
        let mut m = mcp!(
            w,
            &[&repo],
            OPT_IN,
            &[("MINDSPACE_STORE_GATE_FAIL_COMMIT", "1".into())]
        );
        let before = w.counts();
        let r = m.ingest(&repo);
        assert_eq!(r["isError"], true, "{r}");
        assert_eq!(r["structuredContent"]["commit_state"], "ROLLED_BACK", "{r}");
        assert_eq!(r["structuredContent"]["persisted"], false);
        assert_eq!(w.counts(), before);
        // one-shot fault: the retry succeeds on the same server
        assert_eq!(m.ingest(&repo)["isError"], false);
        m.finish();
    }
    // lost acknowledgement: committed, but the tool must say UNKNOWN
    {
        let w = World::new();
        let repo = w.repo("repo");
        let mut m = mcp!(
            w,
            &[&repo],
            OPT_IN,
            &[("MINDSPACE_STORE_GATE_LOSE_ACK", "1".into())]
        );
        let before = w.counts();
        let r = m.ingest(&repo);
        assert_eq!(r["isError"], true, "no false success: {r}");
        assert_eq!(err_kind_any(&r), "commit_outcome_unknown", "{r}");
        assert_eq!(r["structuredContent"]["commit_state"], "UNKNOWN");
        assert_eq!(r["structuredContent"]["persisted"], "unknown");
        assert!(!r.to_string().contains("rolled back") || r.to_string().contains("do not assume"));
        assert!(
            w.revisions() > before["m6_revisions"],
            "the data IS committed"
        );
        m.finish();
        // restart and ingest again: reuses what was committed, no duplicate structure
        let structure = code_and_git(&w);
        let mut m2 = mcp!(w, &[&repo], OPT_IN, &[]);
        let r = m2.ingest(&repo);
        assert_eq!(r["isError"], false, "{r}");
        assert_eq!(code_and_git(&w), structure);
        m2.finish();
    }
}
