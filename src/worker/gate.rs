//! GATE SCAFFOLDING (M7.3b.2b feasibility gate), not product code and not committed. Runs inside the
//! real sandboxed `mind mcp` frontend (after the production stage 1 / stage 2 startup) instead of serving
//! MCP: it obtains a root through the same authorization the repository tools use, starts the extraction
//! worker with the production client, persists the result through the production persistence path
//! and prints one JSON line. Controlled by `MINDSPACE_SPAWN_GATE=<root>` and `GATE_*` variables.

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::core::{IngestOutcome, Result};
use crate::ingest::confined::Anchor;
use crate::ingest::deadline::Deadline;
use crate::ingest::extract::persist;
use crate::mcp::Access;
use crate::mcp::McpConfig;
use crate::storage::Store;

fn probe<T>(r: std::io::Result<T>) -> String {
    match r {
        Ok(_) => "ALLOWED".into(),
        Err(e) => match e.raw_os_error() {
            Some(libc::EACCES) => "EACCES".into(),
            Some(n) => format!("errno {n}"),
            None => e.to_string(),
        },
    }
}

fn status(key: &str) -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix(key).map(|v| v.trim().to_string()))
        })
        .unwrap_or_default()
}

pub fn run(store: &dyn Store, config: &McpConfig, root_arg: &str) -> Result<()> {
    use serde_json::json;
    let env = |k: &str| std::env::var(k).ok();
    let mut out = serde_json::Map::new();
    // the frontend as the kernel sees it
    let inet = {
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        if fd >= 0 {
            unsafe { libc::close(fd) };
            "ALLOWED".to_string()
        } else {
            probe::<()>(Err(std::io::Error::last_os_error()))
        }
    };
    let pair = {
        let mut fds = [0i32; 2];
        let r = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        if r == 0 {
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            "ALLOWED".to_string()
        } else {
            probe::<()>(Err(std::io::Error::last_os_error()))
        }
    };
    let probes = json!({
        "read /etc/passwd": probe(std::fs::read("/etc/passwd")),
        "socket(AF_INET)": inet,
        "socketpair(AF_UNIX)": pair,
        "read /proc/self/maps": probe(std::fs::read("/proc/self/maps")),
        "list /proc": probe(std::fs::read_dir("/proc").map(|_| ())),
    });
    out.insert(
        "frontend".into(),
        json!({
            "pid": std::process::id(),
            "sandbox": config.sandbox().label(),
            "seccomp_mode": status("Seccomp:"),
            "no_new_privs": status("NoNewPrivs:"),
            "probes": probes,
        }),
    );
    let path = match config.authorize(root_arg, Access::ReadOnly) {
        Ok(p) => p,
        Err(e) => {
            out.insert("authorize".into(), json!({"ok": false, "error": e}));
            println!("{}", serde_json::Value::Object(out));
            return Ok(());
        }
    };
    let anchor = match Anchor::open(&path, Some(&path), config, false) {
        Ok(a) => a,
        Err(e) => {
            out.insert(
                "anchor".into(),
                json!({"ok": false, "error": e.to_string()}),
            );
            println!("{}", serde_json::Value::Object(out));
            return Ok(());
        }
    };
    let space = store.get_space("p")?.id;
    let exe = env("GATE_EXE").map(Into::into).unwrap_or(
        std::env::current_exe().map_err(|e| crate::core::Error::Storage(e.to_string()))?,
    );
    let mut cfg = super::client::WorkerConfig::new(exe);
    if let Some(args) = env("GATE_ARGS") {
        cfg.args = serde_json::from_str::<Vec<String>>(&args)
            .unwrap_or_default()
            .into_iter()
            .map(Into::into)
            .collect();
    }
    if let Some(ms) = env("GATE_WALL_MS").and_then(|v| v.parse().ok()) {
        cfg.wall_timeout = Duration::from_millis(ms);
    }
    if let Some(m) = env("GATE_RLIMIT_MIB").and_then(|v| v.parse().ok()) {
        cfg.rlimit_as_mib = Some(m);
    }
    if env("GATE_NO_PROC_VERIFY").is_some() {
        cfg.verify_via_proc = false;
    }
    if env("GATE_NO_EVIDENCE").is_some() {
        cfg.require_sandbox_evidence = false;
    }
    let pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    cfg.observe_pid = Some(pid.clone());
    if let Some(file) = env("GATE_PIDFILE") {
        // lets the test harness find the worker while it runs
        let p = pid.clone();
        std::thread::spawn(move || loop {
            let v = p.load(std::sync::atomic::Ordering::SeqCst);
            if v != 0 {
                let _ = std::fs::write(&file, v.to_string());
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        });
    }
    let cancel = AtomicBool::new(false);
    let result = super::client::extract_directory(
        &cfg,
        &anchor,
        &[space],
        &crate::ingest::extract::StoreContext(store),
        &cancel,
    );
    let worker_pid = pid.load(std::sync::atomic::Ordering::SeqCst);
    match result {
        Ok((extracted, stats)) => {
            let persisted = persist(store, extracted, &Deadline::none());
            out.insert(
                "worker".into(),
                json!({"ok": true, "pid": worker_pid, "startup_ms": stats.startup.as_secs_f64()*1000.0, "frames": stats.frames, "records": stats.records,
                       "exit": stats.exit, "hello": stats.hello}),
            );
            match persisted {
                Ok(IngestOutcome::Files(r)) => {
                    out.insert(
                        "persist".into(),
                        json!({"ok": true, "files": r.files.len(), "code_parsed": r.code.parsed}),
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    out.insert(
                        "persist".into(),
                        json!({"ok": false, "error": e.to_string()}),
                    );
                }
            }
        }
        Err(e) => {
            out.insert(
                "worker".into(),
                json!({"ok": false, "pid": worker_pid, "error": e.to_string()}),
            );
        }
    }
    let gone = worker_pid != 0 && !std::path::Path::new(&format!("/proc/{worker_pid}")).exists();
    out.insert("worker_gone".into(), json!(gone));
    println!("{}", serde_json::Value::Object(out));
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// M7.3b.3a broker gate (same status as above: test scaffolding behind the `spawn-gate` feature).
// Runs inside the real sandboxed `mind mcp --broker` frontend and talks to the real broker over
// the one inherited descriptor. Commands on stdin, one JSON result per line on stdout:
//   extract <root>      extraction through the broker, persisted by this process on success
//   probe <path>        what this (confined) frontend can read directly
//   broker              the broker as this frontend sees it (pid, signal, ptrace, /proc/mem)
//   raw <json>          one raw line to the broker (replay / injection tests)
//   self                this process
// ---------------------------------------------------------------------------------------------

pub fn run_broker_gate(store: &dyn Store, config: &McpConfig) -> Result<()> {
    use serde_json::json;
    use std::io::BufRead;
    // as startup left it, before anything in the gate touches the descriptor
    let fd_cloexec = std::env::var(crate::sandbox::ENV_BROKER_FD)
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
        // SAFETY: F_GETFD only reads the flags.
        .map(|fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC != 0);
    let Some(link) = super::broker::take_frontend_link() else {
        println!("{}", json!({"fatal": "no broker channel"}));
        return Ok(());
    };
    let space = store.get_space("p")?.id;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let (cmd, arg) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        let out = match cmd {
            "self" => json!({
                "pid": std::process::id(),
                "sandbox": config.sandbox().label(),
                "seccomp_mode": status("Seccomp:"),
                "broker_fd_cloexec": fd_cloexec,
            }),
            "probe" => json!({
                "read_dir": probe(std::fs::read_dir(arg).map(|_| ())),
                "read_file": probe(std::fs::read(std::path::Path::new(arg).join("a.md"))),
            }),
            "broker" => {
                let peer_pid: i32 = arg.trim().parse().unwrap_or(0);
                // SAFETY: a null-signal probe and a ptrace attach attempt on the broker's pid.
                let sig = unsafe { libc::kill(peer_pid, 0) };
                let sig_errno = if sig == 0 {
                    0
                } else {
                    std::io::Error::last_os_error().raw_os_error().unwrap_or(-1)
                };
                let pt = unsafe { libc::ptrace(libc::PTRACE_ATTACH, peer_pid, 0, 0) };
                let pt_errno = if pt == 0 {
                    unsafe { libc::ptrace(libc::PTRACE_DETACH, peer_pid, 0, 0) };
                    0
                } else {
                    std::io::Error::last_os_error().raw_os_error().unwrap_or(-1)
                };
                json!({
                    "broker_pid": peer_pid,
                    "kill0_errno": sig_errno,
                    "ptrace_errno": pt_errno,
                    "read_mem": probe(std::fs::read(format!("/proc/{peer_pid}/mem")).map(|_| ())),
                    "read_environ": probe(std::fs::read(format!("/proc/{peer_pid}/environ")).map(|_| ())),
                })
            }
            "raw" => {
                use std::io::Write;
                let _ = (&link).write_all(format!("{arg}\n").as_bytes());
                match super::broker::recv_with_fd(&link) {
                    Ok((bytes, fd)) => {
                        let text = String::from_utf8_lossy(&bytes).trim_end().to_string();
                        let mut fin = String::new();
                        if fd.is_some() {
                            // an operation started: end it and collect the report
                            let _ = (&link).write_all(b"{\"type\":\"cancel\"}\n");
                            let mut r = std::io::BufReader::new(&link);
                            let _ = r.read_line(&mut fin);
                        }
                        json!({"answer": text, "had_fd": fd.is_some(), "finished": fin.trim_end()})
                    }
                    Err(e) => json!({"error": e.to_string()}),
                }
            }
            "extract" => {
                let mut cfg = super::client::WorkerConfig::new("/nonexistent");
                if let Some(ms) = std::env::var("GATE_WALL_MS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                {
                    cfg.wall_timeout = Duration::from_millis(ms);
                }
                if std::env::var_os("GATE_NO_EVIDENCE").is_some() {
                    cfg.require_sandbox_evidence = false;
                }
                let pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
                cfg.observe_pid = Some(pid.clone());
                let cancel = AtomicBool::new(false);
                let res = super::client::extract_via_broker(
                    &cfg,
                    &link,
                    std::path::Path::new(arg),
                    &[space],
                    &crate::ingest::extract::StoreContext(store),
                    &cancel,
                );
                let worker_pid = pid.load(std::sync::atomic::Ordering::SeqCst);
                match res {
                    Ok((extracted, stats, rep)) => {
                        // bytes that must never appear in a worker's result (db, grant store...)
                        let text = serde_json::to_string(&extracted).unwrap_or_default();
                        let leaked: Vec<String> = std::env::var("GATE_FORBID")
                            .unwrap_or_default()
                            .split('|')
                            .filter(|m| !m.is_empty() && text.contains(m))
                            .map(str::to_string)
                            .collect();
                        let persisted = persist(store, extracted, &Deadline::none());
                        json!({
                            "ok": true,
                            "leaked": leaked,
                            "worker_pid": worker_pid,
                            "observed": rep.observed,
                            "exit": rep.exit,
                            "exited_by_itself": rep.exited_by_itself,
                            "descendants_killed": rep.descendants_killed,
                            "peak_rss_kb": rep.peak_rss_kb,
                            "cgroup_peak_bytes": rep.cgroup_peak_bytes,
                            "records": stats.records,
                            "persisted": persisted.is_ok(),
                            "persist_error": persisted.err().map(|e| e.to_string()),
                        })
                    }
                    Err(e) => {
                        json!({"ok": false, "worker_pid": worker_pid, "error": e.to_string()})
                    }
                }
            }
            "git-extract" => {
                // HEAD of a Git repository through the broker, persisted with the commit lease
                let mut cfg = super::client::WorkerConfig::new("/nonexistent");
                if let Some(ms) = std::env::var("GATE_WALL_MS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                {
                    cfg.wall_timeout = Duration::from_millis(ms);
                }
                if std::env::var_os("GATE_NO_EVIDENCE").is_some() {
                    cfg.require_sandbox_evidence = false;
                }
                let pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
                cfg.observe_pid = Some(pid.clone());
                let cancel = AtomicBool::new(false);
                let res = super::client::extract_git_via_broker(
                    &cfg,
                    &link,
                    std::path::Path::new(arg),
                    &[space],
                    &crate::ingest::extract::StoreContext(store),
                    &cancel,
                );
                let worker_pid = pid.load(std::sync::atomic::Ordering::SeqCst);
                match res {
                    Ok((g, stats, rep)) => {
                        let persisted = super::client::persist_git_with_lease(
                            store,
                            &link,
                            rep.op,
                            g,
                            &Deadline::none(),
                        );
                        json!({
                            "ok": true,
                            "worker_pid": worker_pid,
                            "observed": rep.observed,
                            "exit": rep.exit,
                            "exited_by_itself": rep.exited_by_itself,
                            "descendants_killed": rep.descendants_killed,
                            "records": stats.records,
                            "verification": stats.verification.map(|v| json!({
                                "content_verified_objects": v.content_verified_objects,
                                "content_verified_blobs": v.content_verified_blobs,
                                "unverified_blobs": v.unverified_blobs,
                                "external_references": v.external_references,
                            })),
                            "persisted": persisted.is_ok(),
                            "persist_error": persisted.err().map(|e| format!("{e:?}")),
                        })
                    }
                    Err(e) => {
                        json!({"ok": false, "worker_pid": worker_pid, "error": e.to_string()})
                    }
                }
            }
            "quit" => break,
            other => json!({"error": format!("unknown command {other}")}),
        };
        println!("{out}");
    }
    Ok(())
}
