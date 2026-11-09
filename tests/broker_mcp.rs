//! M7.3b.3a: the broker inside the real `mind mcp` startup.
//!
//! Every test starts the production binary (`mind mcp --allow-root .. --grants --broker`): stage 1
//! forks the broker, confines itself and execs stage 2 under real Landlock + seccomp; the broker
//! runs outside that domain with its cgroup. The only test-only part is the last step of the
//! frontend (feature `spawn-gate`, see `worker::gate`): instead of serving MCP it reads commands
//! on stdin and calls the production client over the inherited broker descriptor. No unconfined
//! stand-in frontend is used. Stand-in WORKERS (shell scripts, run by the broker) appear only in
//! the hostile-worker tests and are marked.
//!
//! Skipped with a message when the broker cannot start (no cgroup v2 delegation).

#![allow(clippy::field_reassign_with_default)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

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
    fn rows(&self) -> i64 {
        let c = rusqlite::Connection::open_with_flags(
            &self.db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        ["sources", "code_derivations", "observations"]
            .iter()
            .filter_map(|t| {
                c.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| {
                    r.get::<_, i64>(0)
                })
                .ok()
            })
            .sum()
    }
    fn grant_id(&self, path: &Path) -> String {
        let v: Value = serde_json::from_str(&self.ok(&["grant", "list", "--json"])).unwrap();
        v.as_array()
            .unwrap()
            .iter()
            .find(|g| g["path"].as_str() == Some(path.to_str().unwrap()))
            .and_then(|g| g["id"].as_str())
            .unwrap()
            .to_string()
    }
}

struct Fe {
    child: Child,
    stdin: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
    broker_pid: i32,
    pid: u32,
}

impl Fe {
    /// `vars`: extra environment for the frontend (inherited by the broker forked in stage 1).
    fn start(w: &World, static_root: &Path, vars: &[(&str, String)]) -> Option<Fe> {
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
            child,
            stdin,
            out,
            broker_pid: 0,
            pid,
        };
        let me = fe.cmd("self");
        if me.get("fatal").is_some() || me["broker_pid"].as_i64().unwrap_or(0) == 0 {
            eprintln!("SKIPPED: the broker did not start: {me}");
            return None;
        }
        fe.broker_pid = me["broker_pid"].as_i64().unwrap() as i32;
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
    fn extract(&mut self, root: &Path) -> Value {
        self.cmd(&format!("extract {}", root.display()))
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

fn children_of(pid: i32) -> Vec<u32> {
    std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect()
}

fn slow_source() -> String {
    format!("const X: [u8; 1] = [{}];\n", vec!["1"; 950_000].join(","))
}

// ------------------------------------------------------------------ 1-3: authority

#[test]
fn static_and_dynamic_roots_unauthorized_roots_and_the_frontends_own_view() {
    let w = World::new();
    let project = w.dir(
        "project",
        &[
            ("a.md", "# A\n\nalpha\n"),
            ("src/lib.rs", "pub fn f() {}\n"),
        ],
    );
    let dynamic = w.dir(
        "dynamic",
        &[("a.md", "# D\n\ndynamic\n"), ("x.rs", "pub fn d() {}\n")],
    );
    let other = w.dir("other", &[("a.md", "# O\n")]);
    let mut fe = fe!(w, project, &[]);
    eprintln!(
        "frontend pid {}, broker pid {}, broker cgroup {}, frontend cgroup {}",
        fe.pid,
        fe.broker_pid,
        cgroup_of(fe.broker_pid as u32),
        cgroup_of(fe.pid)
    );
    // the broker descriptor must not leak into Git children: close-on-exec as startup left it
    let me = fe.cmd("self");
    assert_eq!(me["broker_fd_cloexec"], true, "{me}");
    // 1. a static authorized root, through the broker
    let r0 = w.rows();
    let r = fe.extract(&project);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["persisted"], true, "{r}");
    assert_eq!(r["observed"]["seccomp"], "2");
    assert_eq!(r["observed"]["no_new_privs"], "1");
    assert_eq!(r["exit"], "exit code 0");
    assert_eq!(r["exited_by_itself"], true);
    let wp = r["worker_pid"].as_u64().unwrap() as u32;
    assert!(gone(wp));
    eprintln!("static: worker pid {wp}, observed {}", r["observed"]);
    assert!(w.rows() > r0);
    // the worker's cgroup is the broker's, not the frontend's
    assert!(r["observed"]["cgroup"].as_str().unwrap().contains("/op-"));
    // 3. unauthorized
    for p in [&other, &w.root, &project.join("src")] {
        let r = fe.extract(p);
        assert_eq!(r["ok"], false, "{r}");
        assert!(
            r["error"].as_str().unwrap().contains("not authorized"),
            "{r}"
        );
    }
    // 2. a root approved AFTER the frontend started, without restarting it
    let r = fe.extract(&dynamic);
    assert!(
        r["error"].as_str().unwrap().contains("not authorized"),
        "{r}"
    );
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    let r1 = w.rows();
    let r = fe.extract(&dynamic);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["persisted"], true, "{r}");
    assert!(w.rows() > r1);
    eprintln!(
        "dynamic: worker pid {}, observed {}",
        r["worker_pid"], r["observed"]
    );
    // 4. the frontend itself cannot read the dynamic root (and can read its static one)
    let p = fe.cmd(&format!("probe {}", dynamic.display()));
    assert_eq!(p["read_dir"], "EACCES", "{p}");
    assert_eq!(p["read_file"], "EACCES", "{p}");
    let p = fe.cmd(&format!("probe {}", project.display()));
    assert_eq!(p["read_dir"], "ALLOWED", "{p}");
    // revocation: the next operation is refused
    let id = w.grant_id(&dynamic);
    w.ok(&["grant", "revoke", &id, "--reason", "test"]);
    let r = fe.extract(&dynamic);
    assert!(
        r["error"].as_str().unwrap().contains("not authorized"),
        "{r}"
    );
    fe.finish();
}

// ------------------------------------------------------------------ 5, 9, 11: boundaries

#[test]
fn the_worker_cannot_read_the_database_or_grant_store_and_the_frontend_cannot_touch_the_broker() {
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n")]);
    let dynamic = w.dir("dynamic", &[("ok.md", "# fine\n")]);
    w.ok(&["grant", "add", dynamic.to_str().unwrap(), "--yes"]);
    // hostile content: symlinks to the database and to every file of the grant store
    std::os::unix::fs::symlink(&w.db, dynamic.join("db.md")).unwrap();
    let mut n = 0;
    for e in std::fs::read_dir(w.state.join("mindspace"))
        .unwrap()
        .flatten()
    {
        if e.path().is_file() {
            std::os::unix::fs::symlink(e.path(), dynamic.join(format!("store{n}.md"))).unwrap();
            n += 1;
        }
    }
    assert!(n > 0, "the grant store has files");
    let id = w.grant_id(&dynamic);
    let mut fe = fe!(
        w,
        project,
        &[("GATE_FORBID", format!("SQLite format|{id}|granted"))]
    );
    let r = fe.extract(&dynamic);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["leaked"].as_array().unwrap().len(), 0, "{r}");
    // the frontend cannot reach the grant store either (it never could)
    let p = fe.cmd(&format!("probe {}", w.state.join("mindspace").display()));
    assert_eq!(p["read_dir"], "EACCES", "{p}");
    // 11. the broker as the frontend sees it: no ptrace, no memory, no environment
    let b = fe.cmd("broker");
    eprintln!("broker as seen from the frontend: {b}");
    assert_ne!(b["ptrace_errno"], 0, "{b}");
    assert_ne!(b["broker_pid"].as_i64().unwrap(), fe.pid as i64, "{b}");
    assert_ne!(b["read_mem"], "ALLOWED", "{b}");
    assert_ne!(b["read_environ"], "ALLOWED", "{b}");
    fe.finish();
    // 9. the frontend cannot inject authority: no grant id, access, ttl, descriptor or limit. The
    // broker closes a connection at the first protocol violation, so each injection gets its own
    // frontend.
    for extra in [
        ",\"grant\":\"g-0123456789ab\"",
        ",\"access\":\"rw\"",
        ",\"ttl\":\"1h\"",
        ",\"fd\":9",
        ",\"wall_timeout\":99999",
    ] {
        let mut fe = fe!(w, project, &[]);
        let line = format!(
            "raw {{\"type\":\"begin\",\"v\":1,\"op\":\"{}\",\"kind\":\"directory_extract\",\"root\":\"{}\",\"spaces\":[]{extra}}}",
            uuid_like(),
            dynamic.display()
        );
        let a = fe.cmd(&line);
        assert!(
            a["answer"].as_str().unwrap().contains("unreadable"),
            "{extra}: {a}"
        );
        assert_eq!(a["had_fd"], false, "{extra}: {a}");
        fe.finish();
    }
}

fn uuid_like() -> String {
    // a v7-looking id; the broker only needs a valid UUID
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(
        "{:08x}-0000-7000-8000-{:012x}",
        (t >> 32) as u32,
        t as u64 & 0xffff_ffff_ffff
    )
}

#[test]
fn replay_of_an_operation_id_is_refused() {
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n")]);
    let mut fe = fe!(w, project, &[]);
    let op = uuid_like();
    let line = format!(
        "raw {{\"type\":\"begin\",\"v\":1,\"op\":\"{op}\",\"kind\":\"directory_extract\",\"root\":\"{}\",\"spaces\":[]}}",
        project.display()
    );
    let first = fe.cmd(&line);
    assert!(
        first["answer"].as_str().unwrap().contains("\"ready\""),
        "{first}"
    );
    assert_eq!(first["had_fd"], true);
    assert!(
        first["finished"].as_str().unwrap().contains("finished"),
        "{first}"
    );
    let second = fe.cmd(&line);
    assert!(
        second["answer"].as_str().unwrap().contains("already used"),
        "{second}"
    );
    fe.finish();
}

// ------------------------------------------------------------------ 6, 7, 10: hostile and failing workers

#[test]
fn a_real_worker_crash_leaves_broker_and_frontend_alive() {
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n"), ("z.rs", &slow_source())]);
    let mut fe = fe!(w, project, &[]);
    let before = w.rows();
    fe.send(&format!("extract {}", project.display()));
    // find the worker: the only child of the broker
    let end = Instant::now() + Duration::from_secs(20);
    let worker = loop {
        if let Some(p) = children_of(fe.broker_pid).first() {
            break *p;
        }
        assert!(Instant::now() < end, "no worker appeared");
        std::thread::sleep(Duration::from_millis(5));
    };
    eprintln!(
        "crash test: killing worker pid {worker} (broker {}, frontend {})",
        fe.broker_pid, fe.pid
    );
    unsafe { libc::kill(worker as i32, libc::SIGKILL) };
    let r = fe.read();
    assert_eq!(r["ok"], false, "{r}");
    assert!(gone(worker));
    assert_eq!(w.rows(), before, "nothing persisted");
    assert!(alive(fe.broker_pid as u32), "the broker survived");
    assert!(alive(fe.pid), "the frontend survived");
    // and both still work
    let small = w.dir("small", &[("a.md", "# S\n")]);
    // `small` is not authorized: the refusal itself proves the broker still answers
    let r = fe.extract(&small);
    assert!(
        r["error"].as_str().unwrap().contains("not authorized"),
        "{r}"
    );
    fe.finish();
}

fn standin(w: &World, name: &str, body: &str) -> String {
    let p = w.root.join(name);
    std::fs::write(&p, body).unwrap();
    p.display().to_string()
}

fn wait_pidfile(p: &Path) -> u32 {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(t) = std::fs::read_to_string(p) {
            if let Ok(v) = t.trim().parse() {
                return v;
            }
        }
        assert!(Instant::now() < end, "pid file never written");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_setsid_descendant_is_terminated_with_the_operation() {
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n")]);
    let pidfile = w.root.join("desc.pid");
    // STAND-IN worker (runs unconfined under the broker; the broker's confinement check is off
    // for it by the test-only hook): READY/GO, then a descendant in a new session.
    let script = standin(
        &w,
        "worker.sh",
        &format!(
            "#!/bin/sh\nprintf 'READY\\n' >&5\nread go <&5\nsetsid sh -c 'echo $$ > {}; exec sleep 600' &\nsleep 1\nexec sleep 600\n",
            pidfile.display()
        ),
    );
    let mut fe = fe!(
        w,
        project,
        &[
            ("MINDSPACE_BROKER_GATE_WORKER", script),
            ("MINDSPACE_BROKER_GATE_WALL_MS", "2500".into()),
            ("GATE_WALL_MS", "2500".into()),
            ("GATE_NO_EVIDENCE", "1".into()),
        ]
    );
    let r = fe.extract(&project);
    assert_eq!(r["ok"], false, "{r}");
    let d = wait_pidfile(&pidfile);
    eprintln!("setsid descendant pid {d}: cgroup while alive is gone with the operation");
    assert!(gone(d), "the setsid descendant {d} survived");
    fe.finish();
}

#[test]
fn a_malformed_worker_stream_causes_no_partial_persistence() {
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n")]);
    let script = standin(
        &w,
        "garbage.sh",
        "#!/bin/sh\nprintf 'READY\\n' >&5\nread go <&5\nprintf 'garbage\\n' >&3\nsleep 5\n",
    );
    let mut fe = fe!(
        w,
        project,
        &[
            ("MINDSPACE_BROKER_GATE_WORKER", script),
            ("GATE_WALL_MS", "8000".into()),
            ("GATE_NO_EVIDENCE", "1".into()),
        ]
    );
    let before = w.rows();
    let r = fe.extract(&project);
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(w.rows(), before);
    fe.finish();
}

// ------------------------------------------------------------------ 12: lifecycle

#[test]
fn cleanup_after_normal_exit_frontend_crash_and_broker_crash() {
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n")]);
    let desc = |n: &str| w.root.join(n);
    let script = |w: &World, name: &str, pidfile: &Path| {
        standin(
            w,
            name,
            &format!(
                "#!/bin/sh\nprintf 'READY\\n' >&5\nread go <&5\nsetsid sh -c 'echo $$ > {}; exec sleep 600' &\nsleep 600\n",
                pidfile.display()
            ),
        )
    };
    // --- normal exit: stdin closes, frontend exits, broker exits, its cgroup tree is removed
    {
        let mut fe = fe!(w, project, &[]);
        let r = fe.extract(&project);
        assert_eq!(r["ok"], true, "{r}");
        let (bp, fp) = (fe.broker_pid as u32, fe.pid);
        let cg = cgroup_of(bp);
        eprintln!("normal exit: frontend {fp}, broker {bp}, broker cgroup {cg}");
        fe.finish();
        assert!(gone(bp), "the broker did not exit with the frontend");
        let tree = cg_dir(&cg).parent().unwrap().to_path_buf();
        assert!(
            !tree.exists(),
            "the broker left its cgroup tree {} behind",
            tree.display()
        );
    }
    // --- frontend crash during an operation: SIGKILL; the broker sees the hang-up, ends the
    //     operation (worker and its setsid descendant) and exits
    {
        let pf = desc("d1.pid");
        let mut fe = fe!(
            w,
            project,
            &[
                ("MINDSPACE_BROKER_GATE_WORKER", script(&w, "w1.sh", &pf)),
                ("GATE_WALL_MS", "60000".into()),
                ("MINDSPACE_BROKER_GATE_WALL_MS", "60000".into()),
                ("GATE_NO_EVIDENCE", "1".into()),
            ]
        );
        let bp = fe.broker_pid as u32;
        let bcg = cgroup_of(bp);
        fe.send(&format!("extract {}", project.display()));
        let d = wait_pidfile(&pf);
        let worker = children_of(fe.broker_pid)[0];
        eprintln!(
            "frontend crash: frontend {}, broker {bp}, worker {worker}, descendant {d}",
            fe.pid
        );
        unsafe { libc::kill(fe.pid as i32, libc::SIGKILL) };
        assert!(gone(worker), "the worker survived the frontend");
        assert!(gone(d), "the setsid descendant survived the frontend crash");
        assert!(gone(bp), "the broker did not exit after the frontend died");
        let _ = fe.child.wait();
        std::thread::sleep(Duration::from_millis(200));
        let tree = cg_dir(&bcg).parent().unwrap().to_path_buf();
        assert!(
            !tree.exists(),
            "the broker left its cgroup tree {} behind",
            tree.display()
        );
    }
    // --- broker crash during an operation: SIGKILL. The worker dies with it (PDEATHSIG); an
    //     escaped descendant is NOT ended by anything until the next broker start sweeps the dead
    //     broker's cgroup tree. This is the documented limitation.
    {
        let pf = desc("d2.pid");
        let mut fe = fe!(
            w,
            project,
            &[
                ("MINDSPACE_BROKER_GATE_WORKER", script(&w, "w2.sh", &pf)),
                // the escaped descendant holds the worker channel open, so the frontend only
                // notices through its own wall clock: that is the bound that applies here
                ("GATE_WALL_MS", "6000".into()),
                ("MINDSPACE_BROKER_GATE_WALL_MS", "60000".into()),
                ("GATE_NO_EVIDENCE", "1".into()),
            ]
        );
        fe.send(&format!("extract {}", project.display()));
        let d = wait_pidfile(&pf);
        let worker = children_of(fe.broker_pid)[0];
        unsafe { libc::kill(fe.broker_pid, libc::SIGKILL) };
        let r = fe.read();
        assert_eq!(
            r["ok"], false,
            "the frontend gets an error, not a hang: {r}"
        );
        assert!(alive(fe.pid), "the frontend survives its broker");
        assert!(gone(worker), "the worker follows the broker (PDEATHSIG)");
        let leaked = alive(d);
        eprintln!("broker crash: escaped descendant {d} alive after the broker died: {leaked}");
        fe.finish();
        // the next broker start sweeps the dead broker's tree and kills what is left
        let fe2 = fe!(w, project, &[]);
        assert!(
            gone(d),
            "the sweep at the next broker start did not end the descendant {d}"
        );
        let left = mindspace_cgroups();
        eprintln!("mindspace cgroup trees after the sweep: {left:?}");
        fe2.finish();
    }
}

#[test]
fn without_cgroup_delegation_the_broker_fails_closed_and_the_frontend_keeps_running() {
    let w = World::new();
    let project = w.dir("project", &[("a.md", "# A\n")]);
    // test-only override: a parent the user cannot write to (the cgroup root); the real kernel
    // answers EACCES, exactly as on a system service without delegation
    let mut c = w.mind(&["mcp", "--allow-root"]);
    c.arg(&project)
        .args(["--grants", "--broker"])
        .env("MINDSPACE_BROKER_GATE", "1")
        .env("MINDSPACE_BROKER_GATE_CGROUP_PARENT", "/sys/fs/cgroup")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(w.root.join("fe.stderr")).unwrap());
    let mut child = c.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut ask = |l: &str| -> Value {
        writeln!(stdin, "{l}").unwrap();
        let mut s = String::new();
        out.read_line(&mut s).unwrap();
        serde_json::from_str(s.trim()).unwrap_or_else(|_| panic!("no answer: `{s}`"))
    };
    let before = w.rows();
    let r = ask(&format!("extract {}", project.display()));
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(r["worker_pid"], 0, "no worker may be started: {r}");
    assert_eq!(w.rows(), before);
    // the frontend itself is unaffected
    let me = ask("self");
    assert!(me["sandbox"].as_str().unwrap().contains("enforced"), "{me}");
    let err = std::fs::read_to_string(w.root.join("fe.stderr")).unwrap();
    assert!(err.contains("no cgroup delegation"), "{err}");
    eprintln!(
        "fail-closed diagnostic: {}",
        err.lines().find(|l| l.contains("broker")).unwrap_or("")
    );
    drop(stdin);
    let _ = child.wait();
}
