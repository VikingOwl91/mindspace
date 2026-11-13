//! M7.3b.2a: the isolated extraction worker, end to end.
//!
//! Evidence: the worker's result equals the in-process extraction and persists to the same
//! database; the worker is confined (observed by the kernel, not by its word); and whatever goes
//! wrong in or with the worker (crash, hang, cancel, hostile stream, forced termination) costs the
//! parent an error and the database nothing.

#![allow(dead_code, clippy::field_reassign_with_default)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use mindspace::ingest::confined::{extract_directory, Anchor};
use mindspace::ingest::deadline::Deadline;
use mindspace::ingest::extract::{persist, Extracted, StoreContext};
use mindspace::mcp::McpConfig;
use mindspace::storage::Store;
use mindspace::worker::client::{extract_directory as isolated_with, WorkerConfig, WorkerError};
use mindspace::worker::context::ContextSource;
use mindspace::{initialize, open_existing, SqliteStore};
use std::collections::BTreeMap;

/// A parent that knows no derivation and accepts any space (the context-exchange tests use a real
/// store, see `tests/context_exchange.rs`).
struct Blank;

impl ContextSource for Blank {
    fn space_exists(&self, _: mindspace::core::Uuid) -> mindspace::core::Result<bool> {
        Ok(true)
    }
    fn derivation_known(
        &self,
        _: mindspace::core::CodeLanguage,
        _: &str,
        _: &mindspace::core::CodeParserIdentity,
    ) -> mindspace::core::Result<bool> {
        Ok(false)
    }
}

fn isolated(
    cfg: &WorkerConfig,
    anchor: &Anchor,
    spaces: &[mindspace::core::Uuid],
    cancel: &AtomicBool,
) -> Result<(Extracted, mindspace::worker::client::WorkerStats), WorkerError> {
    isolated_with(cfg, anchor, spaces, &Blank, cancel)
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

/// Ids and timestamps differ between runs; everything else must not.
fn normalize(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let hex = |c: u8| c.is_ascii_hexdigit();
    while i < b.len() {
        // uuid: 8-4-4-4-12
        if i + 36 <= b.len()
            && [8, 13, 18, 23].iter().all(|&k| b[i + k] == b'-')
            && b[i..i + 36]
                .iter()
                .enumerate()
                .all(|(k, &c)| [8, 13, 18, 23].contains(&k) || hex(c))
        {
            out.push_str("<id>");
            i += 36;
            continue;
        }
        // RFC 3339 time: 2026-11-08T10:20:30...Z
        if i + 20 <= b.len()
            && b[i + 4] == b'-'
            && b[i + 10] == b'T'
            && b[i..i + 4].iter().all(u8::is_ascii_digit)
        {
            let mut j = i + 19;
            while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.') {
                j += 1;
            }
            if j < b.len() && b[j] == b'Z' {
                out.push_str("<time>");
                i = j + 1;
                continue;
            }
        }
        // microsecond timestamps (`Ts(1794142654740130)`)
        if b[i].is_ascii_digit() {
            let mut j = i;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j - i >= 15 {
                out.push_str("<ts>");
                i = j;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

fn mind() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mind"))
}

struct Db {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

fn db() -> Db {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mind.db");
    initialize(&path).unwrap();
    open_existing(&path).unwrap().create_space("p", "").unwrap();
    Db { _dir: dir, path }
}

impl Db {
    fn store(&self) -> SqliteStore {
        open_existing(&self.path).unwrap()
    }
    fn rows(&self) -> BTreeMap<String, i64> {
        let c = rusqlite::Connection::open_with_flags(
            &self.path,
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
                (
                    t.clone(),
                    c.query_row(&format!("SELECT COUNT(*) FROM \"{t}\""), [], |r| r.get(0))
                        .unwrap(),
                )
            })
            .collect()
    }
}

struct Fixture {
    root: PathBuf,
    cfg: McpConfig,
}

fn fixture(root: &Path) -> Fixture {
    let root = std::fs::canonicalize(root).unwrap();
    let cfg = McpConfig::new(&[&root]).unwrap();
    Fixture { root, cfg }
}

fn anchor(f: &Fixture) -> Anchor {
    Anchor::open(&f.root, Some(&f.root), &f.cfg, false).unwrap()
}

fn tree() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        "docs/guide.md",
        "# Guide\n\nalpha marker\n\n## Part\n\nbeta\n",
    );
    write(d.path(), "notes.txt", "plain note \u{e4}\n");
    write(
        d.path(),
        "src/lib.rs",
        "pub fn alpha_symbol() {}\npub struct Beta;\n",
    );
    write(
        d.path(),
        "src/more.rs",
        "use crate::alpha_symbol;\nfn gamma() { alpha_symbol(); }\n",
    );
    let mut big = String::new();
    for i in 0..4000 {
        big.push_str(&format!("# Section {i}\n\nbody {i}\n\n"));
    }
    write(d.path(), "docs/big.md", &big);
    d
}

fn cfg() -> WorkerConfig {
    let mut c = WorkerConfig::new(mind());
    c.wall_timeout = Duration::from_secs(60);
    c
}

fn text(e: &Extracted) -> String {
    normalize(&format!("{e:#?}"))
}

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

#[test]
fn the_workers_result_equals_the_in_process_extraction_and_persists_identically() {
    let d = tree();
    let f = fixture(d.path());
    let (a, b) = (db(), db());
    let (sa, sb) = (a.store(), b.store());
    let ids_a = vec![sa.get_space("p").unwrap().id];
    let ids_b = vec![sb.get_space("p").unwrap().id];
    let direct = Extracted::Files(
        extract_directory(
            &StoreContext(&sa),
            &anchor(&f),
            &f.cfg,
            &ids_a,
            &Deadline::none(),
        )
        .unwrap(),
    );
    let (got, stats) = isolated(&cfg(), &anchor(&f), &ids_b, &no_cancel()).unwrap();
    assert_eq!(text(&direct), text(&got));
    assert!(stats.frames > 0 && stats.records > 10);
    assert_eq!(stats.exit, "exit code 0");
    let h = stats.hello.expect("the confinement report");
    assert_eq!(h.sandbox.seccomp, "enforced");
    assert!(h.sandbox.probes.len() >= 8);
    // the same database after persisting either
    let before = b.rows();
    persist(&sa, direct, &Deadline::none()).unwrap();
    persist(&sb, got, &Deadline::none()).unwrap();
    assert_ne!(before, b.rows());
    assert_eq!(a.rows(), b.rows());
}

// ------------------------------------------------------------------- helpers

fn alive(pid: u32) -> bool {
    // a reaped process has no /proc entry; a zombie would still show
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| !s.contains(") Z "))
        .unwrap_or(false)
}

fn wait_gone(pid: u32) -> bool {
    for _ in 0..100 {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn slow_source() -> String {
    // about 1.9 MiB of one-line array: tree-sitter needs seconds and hundreds of MiB
    format!("const X: [u8; 1] = [{}];\n", vec!["1"; 950_000].join(","))
}

/// The `worker_probe` example, built for the profile this test binary was built with.
///
/// `cargo test --test extraction_worker` (and `--release`) does not build examples; only a plain
/// `cargo test` does. The test used to assume the example was there, which failed with `NotFound`
/// in a targeted or release run, and ran a STALE example after the probe source changed. So the
/// harness builds it itself, once per test process: incremental, so a fresh example costs a no-op
/// `cargo build`, and always with this binary's profile and target directory.
fn probe_binary() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let exe = std::env::current_exe().unwrap();
            // The profile directory is the ancestor directly below the target directory, which
            // Cargo marks with CACHEDIR.TAG. That holds for <target>/<profile>/deps/<exe> and for
            // the layout of newer Cargo, <target>/<profile>/build/<package>/<hash>/out/<exe>.
            let profile_dir = exe
                .ancestors()
                .skip(1)
                .find(|a| a.parent().is_some_and(|p| p.join("CACHEDIR.TAG").is_file()))
                .unwrap_or_else(|| panic!("cannot find the target directory of {exe:?}"))
                .to_path_buf();
            let target_dir = profile_dir.parent().unwrap();
            let mut cmd = Command::new(env!("CARGO"));
            cmd.args(["build", "--locked", "--quiet", "--example", "worker_probe"])
                .arg("--manifest-path")
                .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
                .arg("--target-dir")
                .arg(target_dir);
            match profile_dir.file_name().and_then(|n| n.to_str()) {
                Some("debug") => {}
                Some("release") => {
                    cmd.arg("--release");
                }
                Some(custom) => {
                    cmd.args(["--profile", custom]);
                }
                None => panic!("cannot tell the build profile from {profile_dir:?}"),
            }
            let out = cmd
                .output()
                .expect("cannot run cargo to build worker_probe");
            assert!(
                out.status.success(),
                "building the worker_probe example failed:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            let path = profile_dir.join("examples/worker_probe");
            assert!(path.exists(), "{path:?} was not produced");
            path
        })
        .clone()
}

/// Runs `worker_probe` with the root at descriptor 4 and a channel at 3, plus `leak` (a secret
/// file duplicated to descriptor 10 and left open, as a careless parent would).
fn probe(root: &Path, leak: Option<&Path>, args: &[String]) -> (Vec<String>, String) {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    let rootf = std::fs::File::open(root).unwrap();
    let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
    let leakf = leak.map(|p| std::fs::File::open(p).unwrap());
    let (r, c) = (rootf.as_raw_fd(), a.as_raw_fd());
    let l = leakf.as_ref().map(|f| f.as_raw_fd());
    let mut cmd = Command::new(probe_binary());
    cmd.args(args)
        .env_clear()
        .stdin(std::process::Stdio::null());
    unsafe {
        cmd.pre_exec(move || {
            let t1 = libc::fcntl(r, libc::F_DUPFD, 500);
            let t2 = libc::fcntl(c, libc::F_DUPFD, 500);
            libc::dup2(t1, 4);
            libc::dup2(t2, 3);
            libc::close(t1);
            libc::close(t2);
            if let Some(l) = l {
                libc::dup2(l, 10);
            }
            Ok(())
        });
    }
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    (
        stdout.lines().map(String::from).collect(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn result(lines: &[String], arg: &str) -> String {
    lines
        .iter()
        .find_map(|l| l.strip_prefix(&format!("RESULT {arg} ")))
        .unwrap_or_else(|| panic!("no result for {arg} in {lines:?}"))
        .to_string()
}

// ------------------------------------------------------------ confinement

#[test]
fn the_confined_process_cannot_leave_its_root_or_reach_data_the_network_or_inherited_descriptors() {
    let base = tempfile::tempdir().unwrap();
    let b = base.path();
    write(b, "root/a.md", "# A\n");
    write(b, "sibling/secret.txt", "SIBLING-SECRET");
    write(b, "db/mind.db", "SQLITE-DATABASE");
    write(b, "db/grants.json", "{\"grants\":[]}");
    write(b, "outside.txt", "OUTSIDE");
    let me = std::process::id();
    let args: Vec<String> = [
        // positive controls: the probes are meaningful
        "read:root/a.md",
        "list:root",
        // sibling repository, database, grant store, other paths
        "read:sibling/secret.txt",
        "read:root/../sibling/secret.txt",
        "list:sibling",
        "read:db/mind.db",
        "read:db/grants.json",
        "read:outside.txt",
        "read:/etc/passwd",
        "list:/home",
        "list:/tmp",
        "list:/root",
        "read:/proc/1/environ",
        // writing, also inside the root
        "write:root/new.md",
        "create:root/new.md",
        "write:root/a.md",
        "write:/tmp/mindspace-probe",
        // other processes
        "proc-list",
        "parent-environ:PID",
        // network
        "socket:inet",
        "socket:inet6",
        "socket:unix",
        "socket:netlink",
        // the inherited secret descriptor, and the descriptor table
        "fd:10",
        "fds",
    ]
    .iter()
    .map(|a| {
        let a = a.replace("PID", &me.to_string());
        match a.split_once(':') {
            Some((k @ ("read" | "list" | "write" | "create"), p)) if !p.starts_with('/') => {
                format!("{k}:{}", b.join(p).display())
            }
            _ => a,
        }
    })
    .collect();
    let (lines, err) = probe(&b.join("root"), Some(&b.join("outside.txt")), &args);
    assert!(
        lines.iter().any(|l| l.starts_with("SANDBOX ")),
        "{lines:?} {err}"
    );
    let get = |k: &str, p: &str| {
        let arg = if p.starts_with('/') || p.is_empty() {
            format!("{k}:{p}")
        } else {
            format!("{k}:{}", b.join(p).display())
        };
        result(&lines, &arg)
    };
    assert_eq!(get("read", "root/a.md"), "ALLOWED");
    assert_eq!(get("list", "root"), "ALLOWED");
    for (k, p) in [
        ("read", "sibling/secret.txt"),
        ("read", "root/../sibling/secret.txt"),
        ("list", "sibling"),
        ("read", "db/mind.db"),
        ("read", "db/grants.json"),
        ("read", "outside.txt"),
        ("read", "/etc/passwd"),
        ("list", "/home"),
        ("list", "/tmp"),
        ("list", "/root"),
        ("read", "/proc/1/environ"),
        ("write", "root/new.md"),
        ("create", "root/new.md"),
        ("write", "root/a.md"),
        ("write", "/tmp/mindspace-probe"),
    ] {
        assert_eq!(get(k, p), "EACCES", "{k} {p}");
    }
    assert_eq!(result(&lines, "proc-list"), "EACCES");
    assert_eq!(result(&lines, &format!("parent-environ:{me}")), "EACCES");
    for s in ["inet", "inet6", "unix", "netlink"] {
        assert_eq!(
            result(&lines, &format!("socket:{s}")),
            "EACCES",
            "socket {s}"
        );
    }
    assert_eq!(
        result(&lines, "fd:10"),
        "EBADF",
        "the inherited descriptor was closed"
    );
    let fds = result(&lines, "fds");
    assert!(
        fds.split(',').all(|f| matches!(f, "0" | "1" | "2" | "3" | "4") || f.parse::<i32>().unwrap_or(0) >= 5),
        "{fds}"
    );
    assert!(!fds.contains("10"), "{fds}");
    assert!(!root_was_written(&b.join("root")));
}

/// M7.3b.5b stabilization: the directory worker has no business executing anything. Its system
/// directories were granted "read" in the Landlock sense, which includes EXECUTE; the worker never
/// executes a program after its own `exec`, so the right was unnecessary authority.
#[test]
fn the_directory_worker_cannot_execute_a_program() {
    let base = tempfile::tempdir().unwrap();
    let b = base.path();
    write(b, "root/a.md", "# A\n");
    // the control: the same programs DO start outside the sandbox, so a refusal means the kernel
    for prog in ["/bin/sh", "/usr/bin/env", "/usr/bin/git"] {
        if Path::new(prog).exists() {
            let st = Command::new(prog).args(["-c", "true"]).output();
            assert!(
                st.is_ok(),
                "{prog} does not start even outside the sandbox: {st:?}"
            );
        }
    }
    let progs: Vec<&str> = [
        "/bin/sh",
        "/usr/bin/sh",
        "/usr/bin/env",
        "/usr/bin/git",
        "/usr/bin/true",
    ]
    .into_iter()
    .filter(|p| Path::new(p).exists())
    .collect();
    assert!(!progs.is_empty());
    let mut args: Vec<String> = progs.iter().map(|p| format!("exec:{p}")).collect();
    // the worker's own binary, reached through the system libraries' directory layout
    args.push(format!("exec:{}", env!("CARGO_BIN_EXE_mind")));
    args.push(format!("read:{}", b.join("root/a.md").display()));
    let (lines, err) = probe(&b.join("root"), None, &args);
    assert!(
        lines.iter().any(|l| l.starts_with("SANDBOX ")),
        "{lines:?} {err}"
    );
    assert_eq!(
        result(&lines, &format!("read:{}", b.join("root/a.md").display())),
        "ALLOWED"
    );
    for a in &args[..args.len() - 1] {
        assert_eq!(
            result(&lines, a),
            "EACCES",
            "{a} was not refused: {lines:?}"
        );
    }
}

fn root_was_written(root: &Path) -> bool {
    root.join("new.md").exists() || std::fs::read_to_string(root.join("a.md")).unwrap() != "# A\n"
}

#[test]
fn the_real_worker_reports_and_proves_its_confinement() {
    let d = tree();
    let f = fixture(d.path());
    let db = db();
    let ids = vec![db.store().get_space("p").unwrap().id];
    // a careless parent leaks a descriptor to a secret file; the worker must not have it
    let secret = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(secret.path(), "SECRET").unwrap();
    let leaked = std::fs::File::open(secret.path()).unwrap();
    use std::os::fd::AsRawFd;
    let high = unsafe { libc::fcntl(leaked.as_raw_fd(), libc::F_DUPFD, 40) };
    assert!(high >= 40);
    let (_got, stats) = isolated(&cfg(), &anchor(&f), &ids, &no_cancel()).unwrap();
    unsafe { libc::close(high) };
    let h = stats.hello.unwrap();
    let probes: BTreeMap<_, _> = h.sandbox.probes.iter().cloned().collect();
    for denied in [
        "read /etc/passwd",
        "list /home",
        "list /tmp",
        "write inside the root",
        "climb out of the root",
        "socket(AF_INET)",
        "socket(AF_UNIX)",
    ] {
        assert_eq!(
            probes.get(denied).map(String::as_str),
            Some("Denied"),
            "{denied}: {probes:?}"
        );
    }
    for allowed in ["list the root", "write /dev/null"] {
        assert_eq!(
            probes.get(allowed).map(String::as_str),
            Some("Allowed"),
            "{allowed}"
        );
    }
    assert_eq!(h.sandbox.seccomp, "enforced");
    assert!(h.sandbox.landlock_abi >= 3);
}

#[test]
fn a_repository_that_links_to_the_database_is_not_followed() {
    let base = tempfile::tempdir().unwrap();
    let b = base.path();
    write(b, "mind.db", "TOP-SECRET-DATABASE-BYTES");
    write(b, "repo/ok.md", "# Ok\n\nfine\n");
    std::os::unix::fs::symlink(b.join("mind.db"), b.join("repo/stolen.md")).unwrap();
    std::os::unix::fs::symlink("../mind.db", b.join("repo/relative.md")).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", b.join("repo/passwd.md")).unwrap();
    let f = fixture(&b.join("repo"));
    let db = db();
    let ids = vec![db.store().get_space("p").unwrap().id];
    let (got, _) = isolated(&cfg(), &anchor(&f), &ids, &no_cancel()).unwrap();
    let dump = format!("{got:?}");
    assert!(
        !dump.contains("TOP-SECRET"),
        "database bytes reached the result"
    );
    assert!(!dump.contains("root:x:"), "/etc/passwd reached the result");
    assert!(dump.contains("fine"));
}

// ------------------------------------------------------- failures and limits

#[test]
fn a_crashing_worker_is_an_error_the_parent_survives_and_nothing_is_persisted() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "a_small.md", "# Small\n\nx\n");
    write(d.path(), "z.rs", &slow_source());
    let f = fixture(d.path());
    let db = db();
    let ids = vec![db.store().get_space("p").unwrap().id];
    let before = db.rows();
    let pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut c = cfg();
    c.rlimit_as_mib = Some(128); // the parse needs several hundred MiB: the worker aborts
    c.observe_pid = Some(pid.clone());
    let err = isolated(&c, &anchor(&f), &ids, &no_cancel()).unwrap_err();
    assert!(matches!(err, WorkerError::Died { .. }), "{err}");
    let text = err.to_string();
    assert!(
        text.contains("signal") || text.contains("exit code"),
        "{text}"
    );
    assert!(
        wait_gone(pid.load(std::sync::atomic::Ordering::SeqCst)),
        "the worker is gone"
    );
    assert_eq!(db.rows(), before, "the database is untouched");
}

#[test]
fn a_hung_worker_is_killed_at_the_wall_clock_limit() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "z.rs", &slow_source());
    let f = fixture(d.path());
    let db = db();
    let ids = vec![db.store().get_space("p").unwrap().id];
    let before = db.rows();
    let pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut c = cfg();
    c.wall_timeout = Duration::from_millis(1500);
    c.observe_pid = Some(pid.clone());
    let t = Instant::now();
    let err = isolated(&c, &anchor(&f), &ids, &no_cancel()).unwrap_err();
    assert!(matches!(err, WorkerError::Timeout(_)), "{err}");
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    assert!(wait_gone(pid.load(std::sync::atomic::Ordering::SeqCst)));
    assert_eq!(db.rows(), before);
}

#[test]
fn cancellation_stops_a_worker_that_is_busy_parsing() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "z.rs", &slow_source());
    let f = fixture(d.path());
    let db = db();
    let ids = vec![db.store().get_space("p").unwrap().id];
    let pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut c = cfg();
    c.observe_pid = Some(pid.clone());
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let t = Instant::now();
    let err = isolated(&c, &anchor(&f), &ids, &cancel).unwrap_err();
    canceller.join().unwrap();
    assert!(matches!(err, WorkerError::Cancelled), "{err}");
    assert!(
        t.elapsed() < Duration::from_secs(4),
        "prompt even though the parse cannot be interrupted: {:?}",
        t.elapsed()
    );
    assert!(wait_gone(pid.load(std::sync::atomic::Ordering::SeqCst)));
}

#[test]
fn killing_the_worker_mid_stream_leaves_a_refused_partial_stream_and_a_live_parent() {
    let d = tempfile::tempdir().unwrap();
    // several MiB of output first, so that frames have crossed the channel, then a slow parse
    let mut big = String::new();
    for i in 0..60_000 {
        big.push_str(&format!(
            "# Section {i}\n\nbody of section {i} with some words\n\n"
        ));
    }
    write(d.path(), "a_big.md", &big);
    write(d.path(), "z.rs", &slow_source());
    let f = fixture(d.path());
    let db = db();
    let ids = vec![db.store().get_space("p").unwrap().id];
    let before = db.rows();
    let pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut c = cfg();
    c.observe_pid = Some(pid.clone());
    c.wall_timeout = Duration::from_secs(120);
    let p2 = pid.clone();
    let killer = std::thread::spawn(move || {
        // wait for the worker, give it time to send its first frames, then SIGKILL it
        let started = Instant::now();
        while p2.load(std::sync::atomic::Ordering::SeqCst) == 0
            && started.elapsed() < Duration::from_secs(10)
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(2500));
        unsafe {
            libc::kill(
                p2.load(std::sync::atomic::Ordering::SeqCst) as i32,
                libc::SIGKILL,
            )
        };
    });
    let err = isolated(&c, &anchor(&f), &ids, &no_cancel()).unwrap_err();
    killer.join().unwrap();
    match &err {
        WorkerError::Died { status, .. } => assert!(status.contains("signal 9"), "{status}"),
        other => panic!("{other}"),
    }
    assert_eq!(db.rows(), before, "no partial persistence");
    // the parent is alive and can run another extraction
    let ok = tree();
    let f2 = fixture(ok.path());
    isolated(&cfg(), &anchor(&f2), &ids, &no_cancel()).unwrap();
}

#[cfg(test)]
mod fake_workers {
    //! Stand-ins that speak the channel badly. They are not sandboxed, so the check of the
    //! worker's confinement is turned off for them (and only for them).
    use super::*;

    /// The worker's side of the handshake with an empty manifest: read the request, answer for
    /// its operation, read the context. What the stand-in does next is the test.
    pub(super) const HANDSHAKE: &str = r#"IFS= read -r REQ <&3; OP=$(printf '%s' "$REQ" | sed 's/.*"op":"\([^"]*\)".*/\1/'); printf '{"v":2,"op":"%s","entries":[]}\n' "$OP" >&3; IFS= read -r CTX <&3;"#;

    fn script(dir: &Path, body: &str) -> WorkerConfig {
        let path = dir.join("fake.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut c = WorkerConfig::new("/bin/sh");
        c.args = vec![path.into_os_string()];
        c.require_sandbox_evidence = false;
        c.wall_timeout = Duration::from_secs(10);
        c.idle_timeout = Duration::from_secs(5);
        c
    }

    fn run(c: &WorkerConfig) -> (Result<(), WorkerError>, Db) {
        let d = tree();
        let f = fixture(d.path());
        let db = db();
        let ids = vec![db.store().get_space("p").unwrap().id];
        let before = db.rows();
        let r = isolated(c, &anchor(&f), &ids, &no_cancel()).map(|_| ());
        assert_eq!(db.rows(), before, "nothing was persisted");
        (r, db)
    }

    #[test]
    fn a_default_config_demands_the_confinement_evidence() {
        assert!(WorkerConfig::new("/x").require_sandbox_evidence);
    }

    #[test]
    fn a_worker_that_is_not_confined_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let mut c = script(t.path(), "echo '{\"v\":1,\"pid\":1,\"sandbox\":{\"landlock_abi\":3,\"signal_scope\":false,\"seccomp\":\"enforced\",\"probes\":[[\"x\",\"Denied\"]]}}' >&3; sleep 5");
        c.require_sandbox_evidence = true;
        let (r, _) = run(&c);
        assert!(matches!(r, Err(WorkerError::Sandbox(_))), "{r:?}");
    }

    #[test]
    fn exiting_before_the_hello_garbage_instead_of_frames_and_a_stale_stream_are_all_refused() {
        let t = tempfile::tempdir().unwrap();
        let (r, _) = run(&script(t.path(), "exit 3"));
        assert!(matches!(r, Err(WorkerError::Died { .. })), "{r:?}");
        let (r, _) = run(&script(
            t.path(),
            &format!(
                "echo hello >&3; {HANDSHAKE} head -c 200 /dev/zero | tr '\\0' 'A' >&3; sleep 1"
            ),
        ));
        match r {
            Err(WorkerError::Stream(m)) => assert!(m.contains("frame length"), "{m}"),
            other => panic!("{other:?}"),
        }
        // a frame of another operation (a replayed stream)
        let frame = r#"{"v":1,"op":"00000000-0000-7000-8000-000000000000","seq":0,"records":[{"type":"begin","kind":"files"}]}"#;
        let body = format!(
            "echo hello >&3; {HANDSHAKE} printf '\\{:03o}\\000\\000\\000' >&3; printf '%s' '{}' >&3; sleep 1",
            frame.len(),
            frame
        );
        let (r, _) = run(&script(t.path(), &body));
        assert!(
            matches!(
                r,
                Err(WorkerError::Stream(_)) | Err(WorkerError::Died { .. })
            ),
            "{r:?}"
        );
    }

    #[test]
    fn a_worker_that_says_nothing_is_cut_off_by_the_idle_limit() {
        let t = tempfile::tempdir().unwrap();
        let mut c = script(t.path(), "echo hello >&3; sleep 30");
        c.idle_timeout = Duration::from_millis(600);
        let started = Instant::now();
        let (r, _) = run(&c);
        assert!(matches!(r, Err(WorkerError::Timeout(_))), "{r:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn the_whole_process_group_ends_with_the_worker() {
        let t = tempfile::tempdir().unwrap();
        let pidfile = t.path().join("child.pid");
        let mut c = script(
            t.path(),
            &format!(
                "sleep 300 &\necho $! > {}\necho hello >&3\nsleep 300",
                pidfile.display()
            ),
        );
        c.wall_timeout = Duration::from_millis(1200);
        let (r, _) = run(&c);
        assert!(matches!(r, Err(WorkerError::Timeout(_))), "{r:?}");
        let child: u32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(
            wait_gone(child),
            "the worker's own child (like a Git process) was killed with the group"
        );
    }
}
