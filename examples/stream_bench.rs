//! M7.3b.1: in-process extraction vs the record stream, one measurement per process.
//! usage: stream_bench inproc  <git|dir> <repo>
//!        stream_bench produce <git|dir> <repo>   (stream on stdout)
//!        stream_bench consume <git|dir> <repo>   (stream on stdin; <repo> is the requested root)
//! Prints one JSON object on stderr. The operation id is fixed so that a producer and a consumer agree.
use std::io::{BufWriter, Write};
use std::time::Instant;

use mindspace::core::Uuid;
use mindspace::ingest::confined::{extract_confined_git, extract_directory, Anchor};
use mindspace::ingest::deadline::Deadline;
use mindspace::ingest::extract::{Extracted, StoreContext};
use mindspace::ingest::outline::PathSelection;
use mindspace::ingest::stream::{
    consume, stream_confined_git, stream_directory, FrameWriter, Kind, StreamLimits,
};
use mindspace::ingest::GitScope;
use mindspace::mcp::McpConfig;
use mindspace::storage::Store;
use mindspace::{initialize, open_existing};

fn status_kb(key: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        .unwrap_or(0)
}

fn pss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/smaps_rollup")
        .ok()
        .and_then(|t| {
            t.lines()
                .find(|l| l.starts_with("Pss:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (mode, kind, repo) = (
        a[1].as_str(),
        a[2].as_str(),
        std::path::PathBuf::from(&a[3]),
    );
    let op = Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0);
    let root = std::fs::canonicalize(&repo).unwrap();
    let t0 = Instant::now();
    let mut out = serde_json::Map::new();
    out.insert("mode".into(), mode.into());
    if mode == "consume" {
        let k = if kind == "git" {
            Kind::Git
        } else {
            Kind::Files
        };
        if let Ok(n) = std::env::var("PIPE_SIZE") {
            // a bigger pipe lets the producer run while the consumer decodes a frame
            unsafe {
                libc::fcntl(0, libc::F_SETPIPE_SZ, n.parse::<i32>().unwrap());
            }
        }
        let root_arg = (k == Kind::Files).then_some(root.as_path());
        // PREFETCH=<buffers of 256 KiB> reads ahead on a thread
        let reader: Box<dyn std::io::Read> = match std::env::var("PREFETCH") {
            Ok(d) => Box::new(mindspace::ingest::stream::Prefetch::new(
                std::io::stdin(),
                d.parse().unwrap(),
            )),
            Err(_) => Box::new(std::io::BufReader::with_capacity(
                1 << 20,
                std::io::stdin().lock(),
            )),
        };
        let (ex, st) = consume(reader, op, k, root_arg, &StreamLimits::default(), None).unwrap();
        out.insert(
            "consume_ms".into(),
            (t0.elapsed().as_secs_f64() * 1e3).into(),
        );
        out.insert("frames".into(), st.frames.into());
        out.insert("bytes".into(), st.bytes.into());
        out.insert("records".into(), st.records.into());
        out.insert("content_bytes".into(), ex.content_bytes().into());
    } else {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        initialize(&db).unwrap();
        let store = open_existing(&db).unwrap();
        store.create_space("p", "").unwrap();
        let ids = vec![store.get_space("p").unwrap().id];
        let cfg = McpConfig::new(&[&root]).unwrap();
        let anchor = Anchor::open(&root, Some(&root), &cfg, true).unwrap();
        let ctx = StoreContext(&store);
        let t = Instant::now();
        if mode == "inproc" {
            let ex = if kind == "git" {
                Extracted::Git(
                    extract_confined_git(
                        &ctx,
                        &anchor,
                        &cfg,
                        &PathSelection::all(),
                        GitScope::Head,
                        &["p"],
                        &Deadline::none(),
                    )
                    .unwrap(),
                )
            } else {
                Extracted::Files(
                    extract_directory(&ctx, &anchor, &cfg, &ids, &Deadline::none()).unwrap(),
                )
            };
            out.insert(
                "extract_ms".into(),
                (t.elapsed().as_secs_f64() * 1e3).into(),
            );
            out.insert("content_bytes".into(), ex.content_bytes().into());
        } else {
            let stdout = std::io::stdout().lock();
            let mut w = FrameWriter::new(BufWriter::with_capacity(1 << 20, stdout), op, None);
            if kind == "git" {
                stream_confined_git(
                    &ctx,
                    &anchor,
                    &cfg,
                    &PathSelection::all(),
                    GitScope::Head,
                    &["p"],
                    &Deadline::none(),
                    &mut w,
                )
                .unwrap();
            } else {
                stream_directory(&ctx, &anchor, &cfg, &ids, &Deadline::none(), &mut w).unwrap();
            }
            let st = w.finish().unwrap();
            out.insert(
                "produce_ms".into(),
                (t.elapsed().as_secs_f64() * 1e3).into(),
            );
            out.insert("frames".into(), st.frames.into());
            out.insert("bytes".into(), st.bytes.into());
        }
    }
    out.insert("peak_rss_mib".into(), (status_kb("VmHWM:") / 1024).into());
    out.insert("pss_mib".into(), (pss_kb() / 1024).into());
    let _ = std::io::stdout().flush();
    eprintln!("{}", serde_json::Value::Object(out));
}
