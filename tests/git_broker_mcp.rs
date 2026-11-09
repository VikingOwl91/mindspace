//! M7.3b.5d: Git operations through the real `mind mcp --grants --broker` process tree.
//!
//! The same harness as `broker_mcp.rs`: the production binary starts stage 1, the watchdog, the
//! broker (outside the frontend's Landlock domain, with its cgroup) and the frontend, which under
//! the `spawn-gate` feature reads commands on stdin and calls the production client. Here the
//! command is `git-extract <root>`, and the broker is given a git executable
//! (`MINDSPACE_BROKER_GATE_GIT`) and optionally a named worker misbehaviour
//! (`MINDSPACE_BROKER_GATE_GIT_MODE`). No Git MCP tool exists yet.
//!
//! Skipped with a message when the broker cannot start (no cgroup v2 delegation).

#![allow(clippy::field_reassign_with_default, dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const GIT: &str = "/usr/bin/git";
const MARKER: &str = "0190b2c4-7d5e-7a31-9c11-3f2d5a6b7c8d";

fn mind() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mind"))
}

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
        for (n, c) in files {
            let p = d.join(n);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        }
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn count(&self, table: &str) -> i64 {
        let c = rusqlite::Connection::open_with_flags(
            &self.db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    /// A Git repository with its identity marker, inside this world.
    fn repo(&self, name: &str) -> PathBuf {
        let d = self.root.join(name);
        std::fs::create_dir_all(d.join("src")).unwrap();
        let g = |args: &[&str]| {
            let o = Command::new(GIT)
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
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        };
        g(&["init", "-q", "-b", "main"]);
        std::fs::write(
            d.join("src/lib.rs"),
            "pub mod util;\npub fn run() { util::helper(); }\n",
        )
        .unwrap();
        std::fs::write(d.join("src/util.rs"), "pub fn helper() {}\n").unwrap();
        std::fs::write(d.join("README.md"), "# R\n\nalpha\n").unwrap();
        g(&["add", "-A"]);
        g(&["commit", "-q", "-m", "one"]);
        std::fs::write(d.join(".git/mindspace-repository-id"), MARKER).unwrap();
        d
    }
}

/// Any broker start sweeps the trees of dead watchdogs (by design), which would end the leak that
/// the watchdog-SIGKILL test must observe. Every frontend holds the read side for its life; that
/// test takes the write side, so no other frontend starts while it observes.
static SWEEP: std::sync::RwLock<()> = std::sync::RwLock::new(());

struct Fe {
    _guard: Option<std::sync::RwLockReadGuard<'static, ()>>,
    child: Child,
    stdin: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
    broker_pid: i32,
    watchdog_pid: i32,
    pid: u32,
}

impl Fe {
    /// `vars`: extra environment for the frontend (inherited by the broker forked in stage 1).
    fn start(w: &World, static_root: &Path, vars: &[(&str, String)]) -> Option<Fe> {
        let guard = SWEEP.read().unwrap_or_else(|e| e.into_inner());
        let mut fe = Self::start_unlocked(w, static_root, vars)?;
        fe._guard = Some(guard);
        Some(fe)
    }

    fn start_unlocked(w: &World, static_root: &Path, vars: &[(&str, String)]) -> Option<Fe> {
        let mut c = w.mind(&["mcp", "--allow-root"]);
        c.arg(static_root)
            .args(["--grants", "--broker"])
            .env("MINDSPACE_BROKER_GATE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(w.root.join("fe.stderr")).unwrap());
        for (k, v) in vars {
            c.env(k, v);
        }
        let mut child = c.spawn().unwrap();
        let stdin = child.stdin.take();
        let out = BufReader::new(child.stdout.take().unwrap());
        let pid = child.id();
        let mut fe = Fe {
            _guard: None,
            child,
            stdin,
            out,
            broker_pid: 0,
            watchdog_pid: 0,
            pid,
        };
        let me = fe.cmd("self");
        // the broker is the watchdog's child; the (unconfined) test can look, the frontend cannot
        let wd = find_watchdog(pid).unwrap_or(0);
        let end = Instant::now() + Duration::from_secs(3);
        let broker = loop {
            if let Some(b) = children_of(wd).first() {
                break *b as i64;
            }
            if Instant::now() >= end {
                break 0;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let mut me = me;
        me["broker_pid"] = broker.into();
        if me.get("fatal").is_some() || broker == 0 {
            let err = std::fs::read_to_string(w.root.join("fe.stderr")).unwrap_or_default();
            eprintln!("SKIPPED: the broker did not start: {me} / stderr: {err}");
            return None;
        }
        fe.broker_pid = me["broker_pid"].as_i64().unwrap() as i32;
        fe.watchdog_pid = wd;
        // the broker must be alive (it exits at once if cgroups are unavailable)
        std::thread::sleep(Duration::from_millis(300));
        if !alive(fe.broker_pid as u32) {
            let err = std::fs::read_to_string(w.root.join("fe.stderr")).unwrap_or_default();
            eprintln!("SKIPPED: the broker is gone (no cgroup delegation?): {err}");
            return None;
        }
        assert!(
            me["sandbox"].as_str().unwrap().contains("enforced"),
            "the frontend must be sandboxed: {me}"
        );
        Some(fe)
    }
    fn send(&mut self, line: &str) {
        let s = self.stdin.as_mut().unwrap();
        writeln!(s, "{line}").unwrap();
        s.flush().unwrap();
    }
    fn read(&mut self) -> Value {
        let mut l = String::new();
        self.out.read_line(&mut l).unwrap();
        serde_json::from_str(l.trim()).unwrap_or_else(|_| panic!("no JSON answer: `{l}`"))
    }
    fn cmd(&mut self, line: &str) -> Value {
        self.send(line);
        self.read()
    }
    fn git_extract(&mut self, root: &Path) -> Value {
        self.cmd(&format!("git-extract {}", root.display()))
    }
    fn finish(mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

impl Drop for Fe {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
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

macro_rules! fe {
    ($w:expr, $root:expr, $vars:expr) => {
        match Fe::start(&$w, &$root, $vars) {
            Some(f) => f,
            None => return,
        }
    };
}

fn cgroup_of(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn mindspace_cgroups() -> Vec<PathBuf> {
    fn walk(d: &Path, out: &mut Vec<PathBuf>, depth: u32) {
        if depth == 0 {
            return;
        }
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                if e.path().is_dir() {
                    if e.file_name().to_string_lossy().starts_with("mindspace-") {
                        out.push(e.path());
                    }
                    walk(&e.path(), out, depth - 1);
                }
            }
        }
    }
    let own = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    let rel = own.trim().strip_prefix("0::").unwrap_or("").to_string();
    let mut base = PathBuf::from("/sys/fs/cgroup").join(rel.trim_start_matches('/'));
    // the slice above (where a broker places its tree when the scope has no controllers)
    base.pop();
    let mut v = Vec::new();
    walk(&base, &mut v, 3);
    v
}

/// The directory of a `/proc/<pid>/cgroup` line (`0::/path`).
fn cg_dir(line: &str) -> PathBuf {
    PathBuf::from("/sys/fs/cgroup").join(
        line.trim()
            .trim_start_matches("0::")
            .trim_start_matches('/'),
    )
}

/// The watchdog names itself `mind-wd-<frontend pid>`; it is not the frontend's child (double
/// fork), so it is found through `/proc` by that name.
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

fn children_of(pid: i32) -> Vec<u32> {
    std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect()
}

fn pstate(pid: u32) -> char {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| s.rsplit_once(") ").and_then(|(_, r)| r.chars().next()))
        .unwrap_or('-')
}

fn settle(pids: &[u32], tree: &Path, limit: Duration) -> Option<Duration> {
    let t = Instant::now();
    loop {
        if pids.iter().all(|p| pstate(*p) == '-') && !tree.exists() {
            return Some(t.elapsed());
        }
        if t.elapsed() > limit {
            return None;
        }
        std::thread::sleep(Duration::from_micros(200));
    }
}

// ============================================================================ Git operations

fn git_vars(mode: Option<&str>) -> Vec<(&'static str, String)> {
    let mut v = vec![
        ("MINDSPACE_BROKER_GATE_GIT", GIT.to_string()),
        ("GATE_WALL_MS", "120000".into()),
        ("MINDSPACE_BROKER_GATE_WALL_MS", "120000".into()),
    ];
    if let Some(m) = mode {
        v.push(("MINDSPACE_BROKER_GATE_GIT_MODE", m.into()));
    }
    v
}

#[test]
fn a_git_ingestion_through_the_sandboxed_frontend_persists_once_and_repeats_idempotently() {
    if !Path::new(GIT).exists() {
        return;
    }
    let w = World::new();
    let repo = w.repo("repo");
    let mut fe = fe!(w, repo, &git_vars(None));
    let r0 = w.count("m6_revisions");
    let a = fe.git_extract(&repo);
    assert_eq!(a["ok"], true, "{a}");
    assert_eq!(a["persisted"], true, "{a}");
    assert_eq!(a["exit"], "exit code 0");
    assert_eq!(a["exited_by_itself"], true);
    // observed from outside by the broker
    assert_eq!(a["observed"]["seccomp"], "2");
    assert_eq!(a["observed"]["no_new_privs"], "1");
    // contained: the worker ran INSIDE the cgroup the frontend started in (under a service manager
    // that is the unit's own cgroup, so stopping the unit ends the tree)
    let fe_cg = cgroup_of(fe.pid);
    let fe_cg = fe_cg.trim().trim_start_matches("0::").trim_end_matches('/');
    let worker_cg = a["observed"]["cgroup"]
        .as_str()
        .unwrap()
        .trim_start_matches("0::");
    assert!(
        worker_cg.starts_with(&format!("{fe_cg}/")),
        "the worker's cgroup `{worker_cg}` is not below the frontend's `{fe_cg}`"
    );
    assert!(
        a["verification"]["content_verified_objects"]
            .as_u64()
            .unwrap()
            >= 3,
        "{a}"
    );
    assert_eq!(
        w.count("m6_revisions") - r0,
        1,
        "one ingestion, one recording revision"
    );
    assert_eq!(w.count("git_commits"), 1);
    let derivations = w.count("code_derivations");
    assert!(derivations > 0);
    // again: announced placeholders, the same derived structure, one more revision
    let b = fe.git_extract(&repo);
    assert_eq!(b["persisted"], true, "{b}");
    assert_eq!(w.count("code_derivations"), derivations);
    assert_eq!(w.count("m6_revisions") - r0, 2);
    // the identity the broker bound is the repository's marker; nothing was written into it
    assert_eq!(
        std::fs::read_to_string(repo.join(".git/mindspace-repository-id")).unwrap(),
        MARKER
    );
}

/// A frontend with a running Git operation whose worker hangs with a git child blocked on stdin.
struct Hung {
    _w: World,
    fe: Fe,
    worker: u32,
    child: u32,
    tree: PathBuf,
}

fn hung(mode: &str) -> Option<Hung> {
    let w = World::new();
    let repo = w.repo("repo");
    let mut fe = Fe::start(&w, &repo, &git_vars(Some(mode)))?;
    fe.send(&format!("git-extract {}", repo.display()));
    let end = Instant::now() + Duration::from_secs(20);
    let (worker, child) = loop {
        if let Some(wk) = children_of(fe.broker_pid).first().copied() {
            if let Some(c) = children_of(wk as i32).first().copied() {
                break (wk, c);
            }
        }
        assert!(
            Instant::now() < end,
            "the worker's git child never appeared"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let tree = cg_dir(&cgroup_of(fe.broker_pid as u32))
        .parent()
        .unwrap()
        .to_path_buf();
    Some(Hung {
        _w: w,
        fe,
        worker,
        child,
        tree,
    })
}

#[test]
fn broker_sigkill_ends_the_git_worker_and_its_git_child() {
    if !Path::new(GIT).exists() {
        return;
    }
    let Some(mut h) = hung("hang") else { return };
    let (wd, br) = (h.fe.watchdog_pid as u32, h.fe.broker_pid as u32);
    unsafe { libc::kill(br as i32, libc::SIGKILL) };
    settle(&[br, h.worker, h.child], &h.tree, Duration::from_secs(10))
        .expect("the worker, its git child or the cgroup tree outlived the broker");
    let r = h.fe.read();
    assert_eq!(r["ok"], false, "{r}");
    assert!(gone(wd), "the watchdog did not exit");
    assert!(alive(h.fe.pid), "the frontend survives");
    h.fe.finish();
}

#[test]
fn frontend_sigkill_ends_the_git_worker_and_its_git_child() {
    if !Path::new(GIT).exists() {
        return;
    }
    let Some(mut h) = hung("hang") else { return };
    let (wd, br) = (h.fe.watchdog_pid as u32, h.fe.broker_pid as u32);
    unsafe { libc::kill(h.fe.pid as i32, libc::SIGKILL) };
    settle(
        &[wd, br, h.worker, h.child],
        &h.tree,
        Duration::from_secs(10),
    )
    .expect("processes or the cgroup tree outlived the frontend");
    let _ = h.fe.child.wait();
}

#[test]
fn without_usable_cgroup_containment_the_git_operation_is_refused_and_runs_nothing() {
    if !Path::new(GIT).exists() {
        return;
    }
    let _exclusive = SWEEP.write().unwrap_or_else(|e| e.into_inner());
    let w = World::new();
    let repo = w.repo("repo");
    let before = mindspace_cgroups();
    let mut c = w.mind(&["mcp", "--allow-root"]);
    c.arg(&repo)
        .args(["--grants", "--broker"])
        .env("MINDSPACE_BROKER_GATE", "1")
        .env("MINDSPACE_BROKER_GATE_GIT", GIT)
        // a cgroup parent the broker cannot use: it must refuse to run, not fall back to a sibling
        // placement or to process groups
        .env("MINDSPACE_BROKER_GATE_CGROUP_PARENT", "/nonexistent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    if let Some(mut s) = child.stdin.take() {
        let _ = writeln!(s, "git-extract {}", repo.display());
    }
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    eprintln!(
        "exit {:?}; stdout: {text}; stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    // the broker never started (no usable containment), so the operation is refused outright: no
    // worker, no in-process or sibling-cgroup fallback, nothing persisted
    let v: Value = serde_json::from_str(text.lines().next().unwrap_or("{}")).unwrap();
    assert_eq!(v["ok"], false, "{text}");
    assert_eq!(v["worker_pid"], 0, "{text}");
    assert!(
        v["error"]
            .as_str()
            .unwrap_or("")
            .contains("broker is not available"),
        "{text}"
    );
    assert_eq!(w.count("git_commits"), 0);
    assert!(
        mindspace_cgroups().iter().all(|t| before.contains(t)),
        "a cgroup tree was left behind"
    );
}

#[test]
fn a_broker_started_the_production_way_without_a_git_executable_refuses_every_git_operation() {
    if !Path::new(GIT).exists() {
        return;
    }
    let w = World::new();
    let repo = w.repo("repo");
    // no MINDSPACE_BROKER_GATE_GIT: exactly the configuration `mind mcp --broker` has today
    let mut fe = fe!(w, repo, &[("GATE_WALL_MS", "120000".to_string())]);
    let r = fe.git_extract(&repo);
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(r["worker_pid"], 0, "{r}");
    assert!(
        r["error"].as_str().unwrap().contains("not configured"),
        "{r}"
    );
    assert_eq!(w.count("git_commits"), 0);
    // and a directory operation through the same broker still works (nothing else changed)
    let dir = w.dir("plain", &[("a.md", "# A\n")]);
    let d = fe.cmd(&format!("extract {}", dir.display()));
    assert!(d.get("ok").is_some(), "{d}");
}
