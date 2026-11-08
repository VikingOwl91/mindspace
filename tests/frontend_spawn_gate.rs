//! M7.3b.2b feasibility gate (uncommitted scaffolding): the extraction worker, started from the real
//! sandboxed `mind mcp` frontend. The frontend is the production binary after the production stage 1 /
//! stage 2 startup (`--allow-root`); only its last step differs (it runs the worker client instead of
//! serving MCP, see `worker::gate`). Nothing here fakes or weakens a sandbox: the stand-in workers in
//! the failure tests run INSIDE the frontend's inherited confinement and are used to observe it.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

fn mind() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mind"))
}

struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let e = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(e.root()).unwrap();
        std::fs::create_dir_all(e.db().parent().unwrap()).unwrap();
        let run = |args: &[&str]| {
            let o = Command::new(mind())
                .env("MINDSPACE_DB", e.db())
                .args(args)
                .output()
                .unwrap();
            assert!(
                o.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&o.stderr)
            );
        };
        run(&["init"]);
        run(&["space", "create", "p"]);
        e
    }
    fn root(&self) -> PathBuf {
        self.dir.path().join("repo")
    }
    fn db(&self) -> PathBuf {
        self.dir.path().join("state/mind.db")
    }
    fn write(&self, rel: &str, content: &str) {
        let p = self.root().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
    /// The frontend command with the gate variables set.
    fn frontend(&self, vars: &[(&str, String)]) -> Command {
        let mut c = Command::new(mind());
        c.args(["mcp", "--allow-root"])
            .arg(self.root())
            .env("MINDSPACE_DB", self.db())
            .env("MINDSPACE_SPAWN_GATE", self.root())
            // a sandboxed parent cannot read another process's /proc entry (see the gate report)
            .env("GATE_NO_PROC_VERIFY", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in vars {
            c.env(k, v);
        }
        c
    }
    fn run(&self, vars: &[(&str, String)]) -> Value {
        let o = self.frontend(vars).output().unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        serde_json::from_str(out.lines().last().unwrap_or(""))
            .unwrap_or_else(|_| panic!("no result: {out} / {}", String::from_utf8_lossy(&o.stderr)))
    }
    fn rows(&self, table: &str) -> i64 {
        rusqlite::Connection::open_with_flags(self.db(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
}

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| !s.contains(") Z "))
        .unwrap_or(false)
}

fn gone(pid: u32) -> bool {
    for _ in 0..100 {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn slow(e: &Env) {
    e.write(
        "z.rs",
        &format!("const X: [u8; 1] = [{}];\n", vec!["1"; 950_000].join(",")),
    );
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

#[test]
fn the_sandboxed_frontend_starts_the_worker_extracts_persists_and_cleans_up() {
    let e = Env::new();
    e.write("a.md", "# A\n\nalpha text\n");
    e.write("src/lib.rs", "pub fn f() {}\n");
    let before = e.rows("sources");
    let r = e.run(&[]);
    // the frontend really is the production sandbox
    let f = &r["frontend"];
    assert!(s(&f["sandbox"]).starts_with("enforced (Landlock"), "{f}");
    assert_eq!(s(&f["seccomp_mode"]), "2");
    assert_eq!(s(&f["no_new_privs"]), "1");
    assert_eq!(s(&f["probes"]["read /etc/passwd"]), "EACCES");
    assert_eq!(s(&f["probes"]["socket(AF_INET)"]), "EACCES");
    // it CAN create the channel and start children, and read its own /proc entry only
    assert_eq!(s(&f["probes"]["socketpair(AF_UNIX)"]), "ALLOWED");
    assert_eq!(s(&f["probes"]["read /proc/self/maps"]), "ALLOWED");
    assert_eq!(s(&f["probes"]["list /proc"]), "EACCES");
    // the worker started, said hello, extracted, and was cleaned up
    let w = &r["worker"];
    assert_eq!(w["ok"], true, "{r}");
    assert_eq!(s(&w["exit"]), "exit code 0");
    let h = &w["hello"]["sandbox"];
    assert_eq!(s(&h["seccomp"]), "enforced");
    for (probe, want) in [
        ("read /etc/passwd", "Denied"),
        ("list /home", "Denied"),
        ("write inside the root", "Denied"),
        ("socket(AF_INET)", "Denied"),
        ("list the root", "Allowed"),
    ] {
        let got = h["probes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p[0] == probe)
            .map(|p| s(&p[1]).to_string());
        assert_eq!(got.as_deref(), Some(want), "{probe}");
    }
    assert_eq!(r["worker_gone"], true);
    // persisted through the existing parent-side path
    assert_eq!(r["persist"]["ok"], true, "{r}");
    assert!(e.rows("sources") > before);
}

#[test]
fn a_worker_started_by_the_frontend_still_cannot_read_the_database_the_frontend_may_read() {
    let e = Env::new();
    e.write("ok.md", "# Ok\n\nfine\n");
    // a repository that points at the database: the frontend's domain allows that file, the worker's does not
    std::os::unix::fs::symlink(e.db(), e.root().join("stolen.md")).unwrap();
    let r = e.run(&[]);
    assert_eq!(r["worker"]["ok"], true, "{r}");
    let n: i64 = rusqlite::Connection::open(e.db())
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM sources WHERE content LIKE '%SQLite format%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn inherited_confinement_is_real_and_gives_a_child_more_authority_than_the_worker_may_have() {
    // A stand-in worker WITHOUT its own sandbox, started by the frontend: it inherits the frontend's
    // Landlock domain and seccomp filter. It can read the database (the frontend can) and cannot read
    // /etc/passwd or create a socket. The real worker narrows this further by itself.
    let e = Env::new();
    e.write("a.md", "# A\n");
    let verdict = e.dir.path().join("state/.mindspace-tmp/verdict");
    let script = format!(
        "r() {{ if cat \"$1\" >/dev/null 2>&1; then echo \"$2=ALLOWED\"; else echo \"$2=DENIED\"; fi; }}; \
         {{ r {db} db; r /etc/passwd passwd; r {sib} sibling; }} > {out}; echo hello >&3; sleep 5",
        db = e.db().display(),
        sib = e.dir.path().join("state/outside-of-everything-does-not-exist").display(),
        out = verdict.display()
    );
    let args = serde_json::to_string(&["/bin/sh", "-c", &script][1..]).unwrap();
    let r = e.run(&[
        ("GATE_EXE", "/bin/sh".into()),
        ("GATE_ARGS", args),
        ("GATE_NO_EVIDENCE", "1".into()),
        ("GATE_WALL_MS", "1500".into()),
    ]);
    assert_eq!(r["worker"]["ok"], false, "{r}");
    let text = std::fs::read_to_string(&verdict).unwrap();
    assert!(text.contains("db=ALLOWED"), "{text}");
    assert!(text.contains("passwd=DENIED"), "{text}");
}

#[test]
fn expected_denials_at_each_spawn_stage() {
    let e = Env::new();
    e.write("a.md", "# A\n");
    // the worker binary: only the frontend's own binary (and /usr) may be executed
    let outside = tempfile::tempdir().unwrap();
    let script = outside.path().join("w.sh");
    std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
    let mut p = std::fs::metadata(&script).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut p, 0o755);
    std::fs::set_permissions(&script, p).unwrap();
    let r = e.run(&[
        ("GATE_EXE", script.display().to_string()),
        ("GATE_ARGS", "[]".into()),
    ]);
    let err = s(&r["worker"]["error"]).to_string();
    assert!(
        err.contains("cannot start") && err.contains("Permission denied"),
        "{err}"
    );
    let r = e.run(&[("GATE_EXE", "/nonexistent/worker".into())]);
    assert!(s(&r["worker"]["error"]).contains("No such file"), "{r}");
    // the root: not an authorized one, and one the frontend cannot read
    let r = {
        let o = e
            .frontend(&[])
            .env("MINDSPACE_SPAWN_GATE", outside.path())
            .output()
            .unwrap();
        serde_json::from_str::<Value>(String::from_utf8_lossy(&o.stdout).lines().last().unwrap())
            .unwrap()
    };
    assert_eq!(r["authorize"]["ok"], false, "{r}");
    // the channel: creating it is allowed by the production filter (and cannot be denied by it)
    let ok = e.run(&[]);
    assert_eq!(
        s(&ok["frontend"]["probes"]["socketpair(AF_UNIX)"]),
        "ALLOWED"
    );
}

#[test]
fn a_busy_worker_is_cut_off_by_the_timeout_and_a_killed_worker_is_reported_and_the_frontend_survives(
) {
    let e = Env::new();
    slow(&e);
    let t = Instant::now();
    let r = e.run(&[("GATE_WALL_MS", "1500".into())]);
    assert!(s(&r["worker"]["error"]).contains("timed out"), "{r}");
    assert!(t.elapsed() < Duration::from_secs(10));
    assert_eq!(r["worker_gone"], true);
    // SIGKILL of the worker from outside (a different process than the frontend)
    let pidfile = e.dir.path().join("state/.mindspace-tmp/worker.pid");
    let child = e
        .frontend(&[("GATE_PIDFILE", pidfile.display().to_string())])
        .spawn()
        .unwrap();
    let started = Instant::now();
    let pid: u32 = loop {
        if let Ok(t) = std::fs::read_to_string(&pidfile) {
            if let Ok(p) = t.trim().parse() {
                break p;
            }
        }
        assert!(started.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(20));
    };
    std::thread::sleep(Duration::from_millis(500));
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let v: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    assert!(s(&v["worker"]["error"]).contains("signal 9"), "{v}");
    assert!(out.status.success(), "the frontend survived");
    assert!(gone(pid));
}

#[test]
fn the_worker_dies_with_a_killed_frontend() {
    let e = Env::new();
    slow(&e);
    let pidfile = e.dir.path().join("state/.mindspace-tmp/worker.pid");
    let mut child = e
        .frontend(&[("GATE_PIDFILE", pidfile.display().to_string())])
        .spawn()
        .unwrap();
    let started = Instant::now();
    let pid: u32 = loop {
        if let Ok(t) = std::fs::read_to_string(&pidfile) {
            if let Ok(p) = t.trim().parse() {
                break p;
            }
        }
        assert!(started.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(alive(pid));
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(gone(pid), "the worker outlived its frontend");
}

#[test]
fn a_descendant_that_leaves_the_process_group_survives_the_group_kill() {
    // Known limitation, not a defect of the sandbox: the group kill cannot reach a process that called
    // setsid. It stays inside the inherited confinement but outlives the operation.
    let e = Env::new();
    e.write("a.md", "# A\n");
    let pidfile = e.dir.path().join("state/.mindspace-tmp/escaped.pid");
    let script = format!(
        "setsid sleep 300 & echo $! > {}; echo hello >&3; sleep 300",
        pidfile.display()
    );
    let args = serde_json::to_string(&["-c", script.as_str()]).unwrap();
    let r = e.run(&[
        ("GATE_EXE", "/bin/sh".into()),
        ("GATE_ARGS", args),
        ("GATE_NO_EVIDENCE", "1".into()),
        ("GATE_WALL_MS", "1200".into()),
    ]);
    assert!(s(&r["worker"]["error"]).contains("timed out"), "{r}");
    let escaped: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let survived = alive(escaped);
    unsafe { libc::kill(escaped as i32, libc::SIGKILL) };
    assert!(
        survived,
        "the setsid descendant was expected to survive (documented limitation)"
    );
}
