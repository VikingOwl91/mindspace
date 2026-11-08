//! M7.2e: the socket-deny seccomp filter in the PRODUCT (`mind mcp` with roots).
//!
//! No filter is forced from outside here (except where a failure or a
//! container-like pre-existing filter is emulated): the server installs its own
//! in `sandbox::enter`. The tests check that real workloads still work, that the
//! filter is inherited by threads and Git children, that other commands are NOT
//! filtered, that failures are closed, and which descriptors reach the server.
//!
//! Every child runs in its own process group under a watchdog; after each run
//! the group must be empty.

#![cfg(target_os = "linux")]

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const DEADLINE: Duration = Duration::from_secs(120);

// ----------------------------------------------------------------- helpers

struct Env {
    _t: tempfile::TempDir,
    root: PathBuf,
    db: PathBuf,
    xdg: PathBuf,
    repo: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(["-c", "commit.gpgsign=false", "-c", "maintenance.auto=false"])
        .args(["-c", "gc.auto=0"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn env() -> Env {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    let xdg = root.join("xdg");
    std::fs::create_dir_all(xdg.join("mindspace")).unwrap();
    let db = xdg.join("mindspace").join("mind.db");
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join("docs")).unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("docs/guide.md"), "# Guide\n\nguidemarker\n").unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn alpha_symbol() {}\n").unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "fixture"]);
    let e = Env {
        _t: t,
        root,
        db,
        xdg,
        repo,
    };
    for args in [vec!["init"], vec!["space", "create", "p"]] {
        let d = run(mind(&e, &args), Duration::from_secs(60));
        assert_eq!(d.code, Some(0), "setup {args:?}");
        assert!(d.clean && !d.timed_out);
    }
    e
}

/// `mind` with a clean, local environment, in its own process group.
fn mind(e: &Env, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_mind"));
    c.args(args)
        .env("MINDSPACE_DB", &e.db)
        .env("XDG_DATA_HOME", &e.xdg)
        .env("HOME", &e.root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("MINDSPACE_MCP_ALLOW_ROOTS")
        .env_remove("MINDSPACE_SANDBOX_STAGE2")
        .env_remove("MINDSPACE_SANDBOX_CANARY")
        .process_group(0);
    c
}

fn group_is_empty(pgid: i32) -> bool {
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        // SAFETY: signal 0 only tests for existence.
        let r = unsafe { libc::kill(-pgid, 0) };
        if r == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return true;
        }
        if Instant::now() > end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn kill_group(pgid: i32) {
    // SAFETY: SIGKILL to a process group this test created.
    unsafe { libc::kill(-pgid, libc::SIGKILL) };
}

struct Done {
    code: Option<i32>,
    stdout: String,
    timed_out: bool,
    clean: bool,
}

/// Runs to completion or kills the whole group at the deadline.
fn run(mut c: Command, deadline: Duration) -> Done {
    let mut child = c
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let pgid = child.id() as i32;
    let mut out = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let end = Instant::now() + deadline;
    let mut timed_out = false;
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st;
        }
        if Instant::now() > end {
            timed_out = true;
            kill_group(pgid);
            break child.wait().unwrap();
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Done {
        code: status.code(),
        stdout: reader.join().unwrap(),
        timed_out,
        clean: group_is_empty(pgid),
    }
}

/// The Seccomp line of a task: `/proc/<pid>/status` is the process leader.
fn seccomp_of(path: &str) -> String {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Seccomp:").map(|v| v.trim().to_string()))
        })
        .unwrap_or_else(|| "?".into())
}

/// The Seccomp mode of the test process itself (a container may already filter).
fn own_seccomp() -> String {
    seccomp_of("/proc/self/status")
}

// -------------------------------------------------------------- MCP session

trait Customize {
    fn apply(self, c: &mut Command);
}
impl<F: FnOnce(&mut Command)> Customize for F {
    fn apply(self, c: &mut Command) {
        self(c)
    }
}

struct Mcp {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
    out: BufReader<std::process::ChildStdout>,
    id: u64,
    pgid: i32,
}

impl Mcp {
    /// `mind mcp <args>`; `customize` may add a pre-existing filter, inherited
    /// descriptors and so on. The server installs its own filter.
    fn start(e: &Env, args: &[&str], customize: impl Customize) -> Self {
        let mut c = mind(e, &[&["mcp"], args].concat());
        customize.apply(&mut c);
        let mut child = c
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mcp");
        let pgid = child.id() as i32;
        let stdin = child.stdin.take();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut s = Self {
            child,
            stdin,
            out,
            id: 0,
            pgid,
        };
        let init = s.rpc(
            "initialize",
            json!({"protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"t","version":"0"}}),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "mindspace", "{init}");
        let stdin = s.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        s
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let msg = json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params});
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|_| json!({"error": "no answer"}))
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        self.rpc("tools/call", json!({"name": name, "arguments": args}))["result"].clone()
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// EOF on stdin is the shutdown signal. (exit code, group empty, timed out)
    fn shutdown(mut self) -> (Option<i32>, bool, bool) {
        drop(self.stdin.take());
        let end = Instant::now() + Duration::from_secs(15);
        let mut timed_out = false;
        let st = loop {
            if let Some(st) = self.child.try_wait().unwrap() {
                break st;
            }
            if Instant::now() > end {
                timed_out = true;
                kill_group(self.pgid);
                break self.child.wait().unwrap();
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        (st.code(), group_is_empty(self.pgid), timed_out)
    }
}

fn text(r: &Value) -> String {
    r["content"][0]["text"].as_str().unwrap_or("").to_string()
}

fn hits(r: &Value) -> usize {
    r["structuredContent"]["hits"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(usize::MAX)
}

/// The first string under a key called `id` or `memory_id`, depth first.
fn find_id(v: &Value) -> Option<String> {
    match v {
        Value::Object(m) => m
            .iter()
            .find_map(|(k, x)| match (k.as_str(), x) {
                ("id" | "memory_id", Value::String(s)) => Some(s.clone()),
                _ => None,
            })
            .or_else(|| m.values().find_map(find_id)),
        Value::Array(a) => a.iter().find_map(find_id),
        _ => None,
    }
}

fn applicable() -> bool {
    use mindspace::doctor::capabilities::{probe_kernel, LandlockState};
    if mindspace::sandbox::seccomp::audit_arch().is_none() {
        eprintln!("skipped: the filter is not applied on this architecture (UNKNOWN)");
        return false;
    }
    let k = probe_kernel();
    if !matches!(k.landlock, LandlockState::Available(v) if v >= 3) {
        eprintln!("skipped: Landlock ABI 3+ unavailable (the sandbox, and so the filter, is off)");
        return false;
    }
    true
}

// ------------------------------------------------------------------- tests

#[test]
fn the_sandboxed_server_installs_the_filter_and_real_workloads_work() {
    if !applicable() {
        return;
    }
    let e = env();
    let root = e.repo.display().to_string();
    let mut s = Mcp::start(&e, &["--allow-root", &root], |_: &mut Command| {});

    let tools = s.rpc("tools/list", json!({}));
    assert_eq!(
        tools["result"]["tools"].as_array().map(|a| a.len()),
        Some(14)
    );
    let created = s.call(
        "memory_create",
        json!({"content": "compat memory about zebra", "spaces": ["p"]}),
    );
    assert_eq!(created["isError"], false, "{created}");
    let found = s.call("memory_search", json!({"query": "zebra"}));
    assert_eq!(hits(&found), 1);
    let id = find_id(&found["structuredContent"]["hits"][0]).expect("a hit with an id");
    assert_eq!(s.call("memory_get", json!({"id": id}))["isError"], false);

    let outline = s.call("repo_outline", json!({"path": root, "depth": 1}));
    assert_eq!(outline["isError"], false, "{}", text(&outline));
    let sc = &outline["structuredContent"];
    assert!(sc["sandbox"].as_str().unwrap().starts_with("enforced"));
    // the typed policy report, with the negative probes the server ran
    assert_eq!(sc["seccomp"]["state"], "enforced", "{sc}");
    assert_eq!(sc["seccomp"]["probes"]["socket(AF_INET)"], "EACCES");
    assert_eq!(sc["seccomp"]["probes"]["pidfd_getfd"], "EACCES");
    assert_eq!(
        sc["seccomp"]["probes"]["socketpair(AF_UNIX) [control]"],
        "allowed"
    );
    assert!(sc["seccomp"]["scope"]
        .as_str()
        .unwrap()
        .contains("not covered"));

    let ing = s.call(
        "repo_ingest",
        json!({"path": root, "spaces": ["p"], "paths": ["docs", "src"]}),
    );
    assert_eq!(ing["isError"], false, "{}", text(&ing));
    assert_eq!(ing["structuredContent"]["seccomp"]["state"], "enforced");
    assert_eq!(
        hits(&s.call("memory_search", json!({"query": "guidemarker"}))),
        1
    );
    assert_eq!(
        hits(&s.call("code_search", json!({"query": "alpha_symbol"}))),
        1
    );

    // The filter mode: the server and EVERY thread it has created.
    let pid = s.pid();
    assert_eq!(seccomp_of(&format!("/proc/{pid}/status")), "2");
    let tasks: Vec<_> = std::fs::read_dir(format!("/proc/{pid}/task"))
        .unwrap()
        .flatten()
        .collect();
    assert!(!tasks.is_empty());
    for t in tasks {
        let st = seccomp_of(&format!("{}/status", t.path().display()));
        assert_eq!(st, "2", "thread {:?} is not filtered", t.file_name());
    }

    let (code, clean, timed_out) = s.shutdown();
    assert_eq!((code, clean, timed_out), (Some(0), true, false));
}

#[test]
fn git_children_of_the_sandboxed_server_inherit_the_filter() {
    if !applicable() {
        return;
    }
    let real = std::env::var_os("PATH")
        .and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("git"))
                .find(|c| c.is_file())
        })
        .expect("git on PATH");
    let e = env();
    // The shim directory is a second, READ-ONLY root so the sandbox may execute
    // it; the log is written into the read-write repository root.
    let shim_dir = e.root.join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let log = e.repo.join("seccomp.log");
    let shim = shim_dir.join("git");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\ngrep '^Seccomp:' /proc/self/status >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            real.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let repo = e.repo.display().to_string();
    let ro = format!("{}:ro", shim_dir.display());
    let path = format!(
        "{}:{}",
        shim_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut s = Mcp::start(&e, &["--allow-root", &repo, "--allow-root", &ro], {
        let path = path.clone();
        move |c: &mut Command| {
            c.env("PATH", path);
        }
    });
    let ing = s.call(
        "repo_ingest",
        json!({"path": repo, "spaces": ["p"], "paths": ["docs", "src"]}),
    );
    assert_eq!(ing["isError"], false, "{}", text(&ing));
    let (code, clean, _) = s.shutdown();
    assert_eq!((code, clean), (Some(0), true));
    let lines: Vec<String> = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().replace('\t', " "))
        .collect();
    assert!(
        !lines.is_empty(),
        "no Git child went through the shim (PATH not used inside the sandbox?)"
    );
    assert!(
        lines.iter().all(|l| l == "Seccomp: 2"),
        "a Git child was not filtered: {lines:?}"
    );
    eprintln!(
        "{} Git invocations inside the sandbox, all Seccomp: 2",
        lines.len()
    );
}

#[test]
fn other_commands_and_unsandboxed_servers_are_not_filtered() {
    if !applicable() {
        return;
    }
    let own = own_seccomp();
    let e = env();
    // 1. an ordinary CLI command that starts Git: record its Git children
    let real = std::env::var_os("PATH")
        .and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("git"))
                .find(|c| c.is_file())
        })
        .expect("git on PATH");
    let shim_dir = e.root.join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let log = e.root.join("cli.log");
    let shim = shim_dir.join("git");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\ngrep '^Seccomp:' /proc/self/status >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            real.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let repo = e.repo.display().to_string();
    let mut c = mind(&e, &["ingest", &repo, "--space", "p"]);
    c.env(
        "PATH",
        format!(
            "{}:{}",
            shim_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    let d = run(c, DEADLINE);
    assert_eq!(d.code, Some(0));
    assert!(d.clean);
    let lines: Vec<String> = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().replace('\t', " "))
        .collect();
    assert!(!lines.is_empty());
    assert!(
        lines.iter().all(|l| *l == format!("Seccomp: {own}")),
        "`mind ingest` or its Git children changed their seccomp state: {lines:?} (own: {own})"
    );
    // 2. `mind mcp` without roots, and with the explicit opt-out
    for args in [vec![], vec!["--insecure-no-sandbox", "--allow-root", &repo]] {
        let s = Mcp::start(&e, &args, |_: &mut Command| {});
        assert_eq!(
            seccomp_of(&format!("/proc/{}/status", s.pid())),
            own,
            "{args:?}"
        );
        let _ = s.shutdown();
    }
}

#[test]
fn a_filter_that_cannot_be_installed_keeps_the_repository_tools_off() {
    if !applicable() {
        return;
    }
    // Two container-like policies: installing is refused outright (EPERM), or
    // the `seccomp` call looks missing (ENOSYS) AND installing is refused. The
    // second must NOT be mistaken for an unsupported kernel.
    type Rule = (libc::c_long, Option<u32>);
    let cases: Vec<(&str, Vec<Rule>, i32)> = vec![
        (
            "prctl(SET_SECCOMP) -> EPERM",
            vec![(libc::SYS_prctl, Some(libc::PR_SET_SECCOMP as u32))],
            libc::EPERM,
        ),
        (
            "seccomp() and prctl(SET_SECCOMP) -> ENOSYS",
            vec![
                (libc::SYS_seccomp, None),
                (libc::SYS_prctl, Some(libc::PR_SET_SECCOMP as u32)),
            ],
            libc::ENOSYS,
        ),
    ];
    for (name, rules, errno) in cases {
        let e = env();
        let root = e.repo.display().to_string();
        let prog = common::seccomp_denier(&rules, errno).unwrap();
        let mut s = Mcp::start(&e, &["--allow-root", &root], move |c: &mut Command| {
            common::seccomp_command(c, prog)
        });
        // memory tools keep working ...
        let created = s.call(
            "memory_create",
            json!({"content": "still works", "spaces": ["p"]}),
        );
        assert_eq!(created["isError"], false, "{name}: {created}");
        // ... the repository tools are off, with the reason
        for (tool, args) in [
            ("repo_outline", json!({"path": root})),
            (
                "repo_ingest",
                json!({"path": root, "spaces": ["p"], "paths": ["docs"]}),
            ),
        ] {
            let r = s.call(tool, args);
            assert_eq!(r["isError"], true, "{name}: {tool} must be off: {r}");
        }
        assert_eq!(
            hits(&s.call("memory_search", json!({"query": "guidemarker"}))),
            0,
            "{name}: nothing may have been ingested"
        );
        let (code, clean, _) = s.shutdown();
        assert_eq!((code, clean), (Some(0), true), "{name}");
    }
}

#[test]
fn a_blocked_seccomp_call_with_a_working_prctl_still_gets_the_filter() {
    if !applicable() {
        return;
    }
    // `seccomp()` missing (ENOSYS) but `prctl(PR_SET_SECCOMP)` allowed: the
    // kernel supports filters, so the filter is required and installed that way.
    let e = env();
    let root = e.repo.display().to_string();
    let prog = common::seccomp_denier(&[(libc::SYS_seccomp, None)], libc::ENOSYS).unwrap();
    let mut s = Mcp::start(&e, &["--allow-root", &root], move |c: &mut Command| {
        common::seccomp_command(c, prog)
    });
    let o = s.call("repo_outline", json!({"path": root}));
    assert_eq!(o["isError"], false, "{}", text(&o));
    assert_eq!(o["structuredContent"]["seccomp"]["state"], "enforced");
    let _ = s.shutdown();
}

#[test]
fn a_preexisting_filter_stacks_with_ours() {
    if !applicable() {
        return;
    }
    // Container-like: an unrelated syscall is already denied. Both filters apply.
    let e = env();
    let root = e.repo.display().to_string();
    let prog = common::seccomp_denier(&[(libc::SYS_acct, None)], libc::EPERM).unwrap();
    let mut s = Mcp::start(&e, &["--allow-root", &root], move |c: &mut Command| {
        common::seccomp_command(c, prog)
    });
    let o = s.call("repo_outline", json!({"path": root}));
    assert_eq!(o["isError"], false, "{}", text(&o));
    assert_eq!(o["structuredContent"]["seccomp"]["state"], "enforced");
    assert_eq!(seccomp_of(&format!("/proc/{}/status", s.pid())), "2");
    let _ = s.shutdown();
}

#[test]
fn inherited_descriptors_do_not_reach_the_filtered_server() {
    if !applicable() {
        return;
    }
    use std::os::unix::io::AsRawFd;
    let e = env();
    let root = e.repo.display().to_string();
    // A socket that exists but is not connected (the dormant-fd hole of M7.2d),
    // a connected unix socket, and a plain file, all inherited at high numbers.
    // SAFETY: plain socket(2); the fd is owned by this test until it ends.
    let dormant = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
    let file = std::fs::File::open("/etc/hostname")
        .or_else(|_| std::fs::File::open("/proc/self/status"))
        .unwrap();
    // an io_uring ring (it can create sockets by itself), if the host allows one
    let mut params = [0u8; 120];
    // SAFETY: io_uring_setup with a zeroed io_uring_params of the right size.
    let ring = unsafe { libc::syscall(libc::SYS_io_uring_setup, 4u32, params.as_mut_ptr()) } as i32;
    eprintln!(
        "io_uring ring for the test: {}",
        if ring >= 0 {
            "available"
        } else {
            "not available"
        }
    );
    let mut fds = vec![
        (dormant, 100),
        (a.as_raw_fd(), 101),
        (file.as_raw_fd(), 102),
    ];
    if ring >= 0 {
        fds.push((ring, 103));
    }
    let mut s = Mcp::start(&e, &["--allow-root", &root], move |c: &mut Command| {
        // SAFETY: dup2 only, between fork and exec.
        unsafe {
            c.pre_exec(move || {
                for &(from, to) in &fds {
                    if libc::dup2(from, to) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    });
    let o = s.call("repo_outline", json!({"path": root}));
    assert_eq!(o["isError"], false, "{}", text(&o));
    assert_eq!(o["structuredContent"]["seccomp"]["state"], "enforced");
    // None of them survived into the server.
    let pid = s.pid();
    let links: Vec<String> = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .flatten()
        .filter_map(|e| std::fs::read_link(e.path()).ok())
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        !links.iter().any(|l| l.starts_with("socket:")),
        "a socket reached the server: {links:?}"
    );
    for n in [100, 101, 102, 103] {
        assert!(
            std::fs::read_link(format!("/proc/{pid}/fd/{n}")).is_err(),
            "fd {n} is open in the server"
        );
    }
    let _ = s.shutdown();
    unsafe { libc::close(dormant) };
}

/// REMAINING BOUNDARY, documented by this test: stdin/stdout can themselves be
/// sockets (a client's choice). They are the transport and stay usable; the
/// filter does not and cannot close them.
#[test]
fn stdio_sockets_remain_usable_and_are_a_documented_boundary() {
    if !applicable() {
        return;
    }
    use std::os::unix::net::UnixStream;
    let e = env();
    let root = e.repo.display().to_string();
    let (client, server_end) = UnixStream::pair().unwrap();
    let child_in: std::os::fd::OwnedFd = server_end.try_clone().unwrap().into();
    let child_out: std::os::fd::OwnedFd = server_end.into();
    let mut c = mind(&e, &["mcp", "--allow-root", &root]);
    let mut child = c
        .stdin(Stdio::from(child_in))
        .stdout(Stdio::from(child_out))
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pgid = child.id() as i32;
    let mut w = client.try_clone().unwrap();
    let mut notify = client.try_clone().unwrap();
    let mut r = BufReader::new(client);
    let mut ask = |id: u64, method: &str, params: Value| -> Value {
        writeln!(
            w,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        )
        .unwrap();
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let init = ask(
        1,
        "initialize",
        json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "mindspace");
    writeln!(
        notify,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    let o = ask(
        2,
        "tools/call",
        json!({"name":"repo_outline","arguments":{"path":root}}),
    );
    assert_eq!(
        o["result"]["structuredContent"]["seccomp"]["state"],
        "enforced"
    );
    let link = std::fs::read_link(format!("/proc/{}/fd/0", child.id())).unwrap();
    assert!(
        link.display().to_string().starts_with("socket:"),
        "fd 0 is {link:?}: the boundary is not exercised"
    );
    // shutdown: close both ends
    drop(w);
    drop(notify);
    drop(r);
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() > end {
            kill_group(pgid);
            panic!("server did not exit after its socket stdio closed");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(group_is_empty(pgid));
}

#[test]
fn the_harness_kills_a_hung_child_and_its_group() {
    let mut c = Command::new("/bin/sh");
    c.args(["-c", "sleep 300 & sleep 300"]).process_group(0);
    let t = Instant::now();
    let d = run(c, Duration::from_millis(500));
    assert!(d.timed_out);
    assert!(d.clean, "children of the hung process survived");
    assert!(t.elapsed() < Duration::from_secs(10));
    let _ = d.stdout;
}

/// Opt-in: needs an installed embedding model (about 500 MB, copied into a temp
/// directory because the product refuses a symlinked model directory).
/// `M72D_MODEL_SRC=<dir of ~/.local/share/mindspace/models>`
///
/// The index is built by an ordinary (unfiltered) CLI command; the semantic
/// search then runs INSIDE the sandboxed, filtered server.
#[test]
fn semantic_search_works_inside_the_filtered_server() {
    let Some(src) = std::env::var_os("M72D_MODEL_SRC").map(PathBuf::from) else {
        eprintln!("UNKNOWN: embeddings not exercised (set M72D_MODEL_SRC to an installed models directory)");
        return;
    };
    if !applicable() {
        return;
    }
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for ent in std::fs::read_dir(from).unwrap() {
            let ent = ent.unwrap();
            let dest = to.join(ent.file_name());
            if ent.file_type().unwrap().is_dir() {
                copy(&ent.path(), &dest);
            } else if std::fs::hard_link(ent.path(), &dest).is_err() {
                std::fs::copy(ent.path(), &dest).unwrap();
            }
        }
    }
    let e = env();
    copy(&src, &e.xdg.join("mindspace").join("models"));
    let repo = e.repo.display().to_string();
    for args in [
        vec!["model", "use", "nomic-embed-text-v1.5"],
        vec!["embeddings", "enable"],
        vec!["ingest", &repo, "--space", "p"],
        vec!["index", "run"],
    ] {
        let d = run(mind(&e, &args), Duration::from_secs(600));
        assert_eq!(d.code, Some(0), "{args:?}");
        assert!(d.clean && !d.timed_out);
    }
    let mut s = Mcp::start(&e, &["--allow-root", &repo], |_: &mut Command| {});
    let r = s.call(
        "memory_search",
        json!({"query": "guidemarker", "mode": "semantic"}),
    );
    assert_eq!(r["isError"], false, "{}", text(&r));
    let sc = &r["structuredContent"];
    eprintln!(
        "semantic search in the filtered server: effective={}",
        sc["effective"]
    );
    assert_eq!(sc["effective"], "semantic", "{sc}");
    assert_eq!(hits(&r), 1);
    let o = s.call("repo_outline", json!({"path": repo}));
    assert_eq!(o["structuredContent"]["seccomp"]["state"], "enforced");
    let (code, clean, _) = s.shutdown();
    assert_eq!((code, clean), (Some(0), true));
}

/// A container profile that also forbids `socketpair` over-restricts: the
/// server's positive control fails, the policy is not the intended one, and the
/// repository tools stay off rather than running under something unknown.
#[test]
fn an_over_restrictive_preexisting_filter_fails_the_verification_closed() {
    if !applicable() {
        return;
    }
    let e = env();
    let root = e.repo.display().to_string();
    let prog = common::seccomp_denier(&[(libc::SYS_socketpair, None)], libc::EPERM).unwrap();
    let mut s = Mcp::start(&e, &["--allow-root", &root], move |c: &mut Command| {
        common::seccomp_command(c, prog)
    });
    let r = s.call("repo_outline", json!({"path": root}));
    assert_eq!(r["isError"], true, "{r}");
    let (code, clean, _) = s.shutdown();
    assert_eq!((code, clean), (Some(0), true));
}

/// Landlock cannot be established AFTER the filter was installed: the server
/// keeps the filter, the repository tools stay off.
#[test]
fn incomplete_landlock_after_the_filter_leaves_the_tools_off() {
    if !applicable() {
        return;
    }
    for errno in [libc::ENOSYS, libc::EOPNOTSUPP] {
        let e = env();
        let root = e.repo.display().to_string();
        let mut s = Mcp::start(&e, &["--allow-root", &root], move |c: &mut Command| {
            common::block_landlock(c, errno)
        });
        let r = s.call("repo_outline", json!({"path": root}));
        assert_eq!(r["isError"], true, "errno {errno}: {r}");
        assert!(text(&r).contains("disabled"), "errno {errno}: {}", text(&r));
        let created = s.call(
            "memory_create",
            json!({"content": "memory tools still work", "spaces": ["p"]}),
        );
        assert_eq!(created["isError"], false);
        assert_eq!(seccomp_of(&format!("/proc/{}/status", s.pid())), "2");
        let (code, clean, _) = s.shutdown();
        assert_eq!((code, clean), (Some(0), true));
    }
}
