//! M7.3b.2a measurements: one isolated directory extraction, one JSON line on stdout.
//! usage: worker_run <dir> [--mind <path>] [--rlimit-mib N] [--wall-ms N] [--cancel-ms N]
//!                       [--wrap <program> <args...> --]   (e.g. systemd-run --user --scope -q -p MemoryMax=300M --)
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use mindspace::ingest::confined::Anchor;
use mindspace::mcp::McpConfig;
use mindspace::worker::client::{extract_directory, WorkerConfig};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = std::fs::canonicalize(&a[1]).unwrap();
    let mut mind = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("mind");
    let (mut rlimit, mut wall, mut cancel_ms, mut wrap): (
        Option<u64>,
        u64,
        Option<u64>,
        Vec<String>,
    ) = (None, 300_000, None, vec![]);
    let mut i = 2;
    while i < a.len() {
        match a[i].as_str() {
            "--mind" => {
                mind = a[i + 1].clone().into();
                i += 2
            }
            "--rlimit-mib" => {
                rlimit = Some(a[i + 1].parse().unwrap());
                i += 2
            }
            "--wall-ms" => {
                wall = a[i + 1].parse().unwrap();
                i += 2
            }
            "--cancel-ms" => {
                cancel_ms = Some(a[i + 1].parse().unwrap());
                i += 2
            }
            "--wrap" => {
                i += 1;
                while a[i] != "--" {
                    wrap.push(a[i].clone());
                    i += 1
                }
                i += 1;
            }
            other => panic!("unknown option {other}"),
        }
    }
    let cfgm = McpConfig::new(&[&dir]).unwrap();
    let anchor = Anchor::open(&dir, Some(&dir), &cfgm, false).unwrap();
    let mut cfg = WorkerConfig::new(&mind);
    cfg.rlimit_as_mib = rlimit;
    cfg.wall_timeout = Duration::from_millis(wall);
    if !wrap.is_empty() {
        cfg.exe = wrap[0].clone().into();
        let mut args: Vec<std::ffi::OsString> = wrap[1..].iter().map(Into::into).collect();
        args.push(mind.into_os_string());
        args.push("extraction-worker".into());
        cfg.args = args;
    }
    let pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    cfg.observe_pid = Some(pid.clone());
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    if let Some(ms) = cancel_ms {
        let c = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            c.store(true, std::sync::atomic::Ordering::SeqCst);
        });
    }
    let ids = vec![];
    let t = Instant::now();
    let r = extract_directory(&cfg, &anchor, &ids, &cancel);
    let wall_ms = t.elapsed().as_secs_f64() * 1000.0;
    let gone = {
        let p = pid.load(std::sync::atomic::Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        !std::path::Path::new(&format!("/proc/{p}")).exists()
    };
    let parent_rss_kb = std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find(|l| l.starts_with("VmHWM:"))
        .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
        .unwrap_or(0);
    match r {
        Ok((ex, s)) => println!(
            "{}",
            serde_json::json!({
                "ok": true, "wall_ms": wall_ms, "startup_ms": s.startup.as_secs_f64()*1000.0,
                "frames": s.frames, "bytes": s.bytes, "records": s.records,
                "worker_peak_rss_mib": s.worker_peak_rss_kb / 1024, "exit": s.exit,
                "content_bytes": ex.content_bytes(), "worker_gone": gone, "parent_peak_rss_mib": parent_rss_kb/1024,
            })
        ),
        Err(e) => println!(
            "{}",
            serde_json::json!({
                "ok": false, "wall_ms": wall_ms, "error": e.to_string().chars().take(200).collect::<String>(),
                "worker_gone": gone, "parent_alive": true, "parent_peak_rss_mib": parent_rss_kb/1024,
            })
        ),
    }
}
