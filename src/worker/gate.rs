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
    let result = super::client::extract_directory(&cfg, &anchor, &[space], &cancel);
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
