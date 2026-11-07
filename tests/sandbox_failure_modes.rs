//! Failure modes of the kernel sandbox around `mind mcp` (ADR-029, MIND-18).
//!
//! What must hold when the sandbox cannot be established, cannot be verified,
//! or is switched off on purpose:
//!  - the repository tools are OFF with a reason, memory tools keep working;
//!  - `--insecure-no-sandbox` turns them on, warns on every start, and every
//!    result says so;
//!  - a process that claims to be sandboxed (environment marker) but is not,
//!    does not run;
//!  - inherited file descriptors do not survive into the server.
//!
//! "Landlock unavailable" is emulated with a seccomp filter that makes the three
//! Landlock system calls fail the way a kernel without (`ENOSYS`) or with a
//! disabled (`EOPNOTSUPP`) Landlock would. An older ABI cannot be emulated this
//! way; its handling (refuse below ABI 3) rests on `HardRequirement` of the
//! `landlock` crate and the canary probes, which are unit-tested.

#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

struct Server {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    next_id: u64,
    stderr_path: PathBuf,
}

#[derive(Default)]
struct Opts {
    extra_args: Vec<&'static str>,
    env: Vec<(String, String)>,
    block_landlock_with: Option<i32>,
    inherit_fd_to: Option<(PathBuf, i32)>,
}

/// seccomp filter: the three Landlock syscalls (444 to 446) fail with `errno`.
fn landlock_blocker(errno: i32) -> Vec<libc::sock_filter> {
    let (bpf_ld, bpf_jmp, bpf_ret) = (0x00u16, 0x05u16, 0x06u16);
    let (w, abs, jeq, jge, jgt, k) = (0x00u16, 0x20u16, 0x10u16, 0x30u16, 0x20u16, 0x00u16);
    #[cfg(target_arch = "x86_64")]
    let arch = 0xC000_003Eu32;
    #[cfg(target_arch = "aarch64")]
    let arch = 0xC000_00B7u32;
    let f = |code: u16, jt: u8, jf: u8, k: u32| libc::sock_filter { code, jt, jf, k };
    vec![
        f(bpf_ld | w | abs, 0, 0, 4),
        f(bpf_jmp | jeq | k, 0, 4, arch),
        f(bpf_ld | w | abs, 0, 0, 0),
        f(bpf_jmp | jge | k, 0, 2, 444),
        f(bpf_jmp | jgt | k, 1, 0, 446),
        f(bpf_ret | k, 0, 0, 0x0005_0000 | errno as u32),
        f(bpf_ret | k, 0, 0, 0x7FFF_0000),
    ]
}

impl Server {
    fn start(db: &Path, specs: &[String], o: Opts) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_mind"));
        cmd.env("MINDSPACE_DB", db)
            .env_remove("MINDSPACE_MCP_ALLOW_ROOTS")
            .env_remove("MINDSPACE_SANDBOX_STAGE2")
            .env_remove("MINDSPACE_SANDBOX_CANARY")
            .arg("mcp");
        for s in specs {
            cmd.arg("--allow-root").arg(s);
        }
        cmd.args(&o.extra_args);
        for (k, v) in &o.env {
            cmd.env(k, v);
        }
        let prog = o.block_landlock_with.map(landlock_blocker);
        let inherit = o.inherit_fd_to.clone();
        // SAFETY: the closure only calls prctl and dup2/open-free libc functions on
        // data prepared before the fork; no allocation after fork.
        unsafe {
            cmd.pre_exec(move || {
                if let Some(p) = &prog {
                    let fprog = libc::sock_fprog {
                        len: p.len() as u16,
                        filter: p.as_ptr() as *mut _,
                    };
                    libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
                    if libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &fprog) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                if let Some((_, fd)) = &inherit {
                    // The file was opened by the parent as fd `fd + 100`; make it visible as `fd`
                    // without CLOEXEC, the way a careless launcher leaks a descriptor.
                    libc::dup2(*fd + 100, *fd);
                }
                Ok(())
            });
        }
        let stderr_path = db.with_extension("stderr");
        // For the descriptor-leak test the parent opens the file first.
        let _leak = o.inherit_fd_to.as_ref().map(|(p, fd)| {
            let f = std::fs::File::open(p).unwrap();
            let raw = std::os::fd::AsRawFd::as_raw_fd(&f);
            unsafe { libc::dup2(raw, *fd + 100) };
            f
        });
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(&stderr_path).unwrap())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            out,
            next_id: 1,
            stderr_path,
        }
    }

    fn handshake(&mut self) -> Value {
        let init = self.rpc(
            "initialize",
            json!({"protocolVersion":"2025-06-18","capabilities":{},
                   "clientInfo":{"name":"t","version":"0"}}),
        );
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        init
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
        self.rpc("tools/call", json!({"name": name, "arguments": args}))["result"].clone()
    }

    fn stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr_path).unwrap_or_default()
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
        .env("GIT_AUTHOR_EMAIL", "t@e.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.invalid")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

struct World {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    db: PathBuf,
    specs: Vec<String>,
}

fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let (root, state) = (base.join("root"), base.join("state"));
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.rs"), "pub fn alpha() {}\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "i"]);
    let db = state.join("mind.db");
    for args in [vec!["init"], vec!["space", "create", "p"]] {
        let o = Command::new(env!("CARGO_BIN_EXE_mind"))
            .env("MINDSPACE_DB", &db)
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success());
    }
    let specs = vec![format!("{}:rw", root.display())];
    World {
        _tmp: tmp,
        root,
        repo,
        db,
        specs,
    }
}

fn ingest_args(w: &World) -> Value {
    json!({"path": w.repo.display().to_string(), "spaces": ["p"]})
}

fn memory_tools_work(s: &mut Server) {
    let w = s.call(
        "memory_create",
        json!({"content": "kept working", "subject": "kw", "spaces": ["p"]}),
    );
    assert_eq!(w["isError"], false, "memory write failed: {}", text(&w));
    let r = s.call("memory_search", json!({"query": "kept"}));
    assert!(r["structuredContent"]["hits"]
        .as_array()
        .is_some_and(|h| !h.is_empty()));
}

#[test]
fn the_sandbox_is_on_by_default_and_every_result_says_so() {
    let w = world();
    let mut s = Server::start(&w.db, &w.specs, Opts::default());
    s.handshake();
    let r = s.call("repo_ingest", ingest_args(&w));
    assert_eq!(r["isError"], false, "{}", text(&r));
    let label = r["structuredContent"]["sandbox"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(label.starts_with("enforced"), "sandbox label was {label:?}");
    let o = s.call(
        "repo_outline",
        json!({"path": w.repo.display().to_string()}),
    );
    assert!(
        o["structuredContent"]["sandbox"]
            .as_str()
            .unwrap_or("")
            .starts_with("enforced"),
        "{o}"
    );
}

fn unavailable_case(errno: i32) {
    let w = world();
    let mut s = Server::start(
        &w.db,
        &w.specs,
        Opts {
            block_landlock_with: Some(errno),
            ..Opts::default()
        },
    );
    s.handshake();
    for (tool, args) in [
        ("repo_ingest", ingest_args(&w)),
        (
            "repo_outline",
            json!({"path": w.repo.display().to_string()}),
        ),
    ] {
        let r = s.call(tool, args);
        assert_eq!(
            r["isError"], true,
            "{tool} must be off without a sandbox: {r}"
        );
        let msg = text(&r);
        assert!(
            msg.contains("sandbox") && msg.contains("--insecure-no-sandbox"),
            "{tool}: {msg}"
        );
    }
    memory_tools_work(&mut s);
    assert!(
        s.stderr().contains("repository tools are OFF"),
        "operator was not told: {}",
        s.stderr()
    );
    // nothing was ingested
    assert!(!std::fs::read(&w.db)
        .unwrap()
        .windows(5)
        .any(|x| x == b"alpha"));
}

#[test]
fn without_landlock_enosys_the_repository_tools_are_off_and_memory_tools_work() {
    unavailable_case(libc::ENOSYS);
}

#[test]
fn with_landlock_disabled_eopnotsupp_the_repository_tools_are_off_and_memory_tools_work() {
    unavailable_case(libc::EOPNOTSUPP);
}

#[test]
fn the_explicit_opt_out_turns_the_tools_on_warns_and_labels_results() {
    let w = world();
    let mut s = Server::start(
        &w.db,
        &w.specs,
        Opts {
            block_landlock_with: Some(libc::ENOSYS),
            extra_args: vec!["--insecure-no-sandbox"],
            ..Opts::default()
        },
    );
    s.handshake();
    let r = s.call("repo_ingest", ingest_args(&w));
    assert_eq!(r["isError"], false, "{}", text(&r));
    let label = r["structuredContent"]["sandbox"].as_str().unwrap_or("");
    assert!(
        label.contains("DISABLED") && label.contains("application layer"),
        "{label:?}"
    );
    assert!(
        s.stderr().contains("WARNING") && s.stderr().contains("--insecure-no-sandbox"),
        "{}",
        s.stderr()
    );
}

#[test]
fn a_database_directory_that_overlaps_a_root_turns_the_tools_off() {
    let w = world();
    // The database next to the repository, inside the root.
    let inside = w.root.join("mind.db");
    std::fs::copy(&w.db, &inside).unwrap();
    let mut s = Server::start(&inside, &w.specs, Opts::default());
    s.handshake();
    let r = s.call("repo_ingest", ingest_args(&w));
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("database directory"), "{}", text(&r));
    memory_tools_work(&mut s);
}

#[test]
fn a_sandbox_marker_without_real_confinement_stops_the_process() {
    let w = world();
    let canary = tempfile::tempdir().unwrap();
    std::fs::write(canary.path().join("canary"), b"canary").unwrap();
    // 1: the canary is reachable, so the marker is a lie.
    // 2: the canary does not exist, so nothing can be verified (ENOENT proves nothing).
    for canary_dir in [canary.path().to_path_buf(), canary.path().join("missing")] {
        let o = Command::new(env!("CARGO_BIN_EXE_mind"))
            .env("MINDSPACE_DB", &w.db)
            .env("MINDSPACE_SANDBOX_STAGE2", "1")
            .env("MINDSPACE_SANDBOX_CANARY", &canary_dir)
            .arg("mcp")
            .arg("--allow-root")
            .arg(&w.specs[0])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(
            o.status.code(),
            Some(78),
            "canary {canary_dir:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("sandbox"),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
}

fn leaked_fd_targets(pid: u32) -> Vec<(i32, PathBuf)> {
    std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .flatten()
        .filter_map(|e| {
            let fd: i32 = e.file_name().to_string_lossy().parse().ok()?;
            Some((fd, std::fs::read_link(e.path()).ok()?))
        })
        .filter(|(fd, _)| *fd > 2)
        .collect()
}

#[test]
fn an_inherited_descriptor_to_an_outside_file_does_not_survive_into_the_server() {
    for extra in [vec![], vec!["--insecure-no-sandbox"]] {
        let w = world();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "SECRET").unwrap();
        let mut s = Server::start(
            &w.db,
            &w.specs,
            Opts {
                inherit_fd_to: Some((secret.clone(), 9)),
                extra_args: extra.clone(),
                ..Opts::default()
            },
        );
        s.handshake();
        // Let the (re-)start settle, then look at the live process.
        let r = s.call("memory_search", json!({"query": "x"}));
        assert!(r.get("isError").is_some(), "{r}");
        let fds = leaked_fd_targets(s.child.id());
        let secret_canon = secret.canonicalize().unwrap();
        assert!(
            !fds.iter().any(|(_, t)| t == &secret_canon),
            "the server still holds the outside file (args {extra:?}): {fds:?}"
        );
        assert!(
            !fds.iter().any(|(fd, _)| *fd == 9),
            "descriptor 9 survived (args {extra:?}): {fds:?}"
        );
    }
}

#[test]
fn without_roots_no_sandbox_is_set_up_and_nothing_is_printed() {
    let w = world();
    let mut s = Server::start(&w.db, &[], Opts::default());
    s.handshake();
    let r = s.call("repo_ingest", ingest_args(&w));
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("no allowed roots"), "{}", text(&r));
    memory_tools_work(&mut s);
    assert_eq!(
        s.stderr().trim(),
        "",
        "a server without roots should be silent: {}",
        s.stderr()
    );
    let _ = (Instant::now(), Duration::from_secs(0));
}
