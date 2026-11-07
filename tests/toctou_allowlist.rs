//! Time-of-check to time-of-use against the MCP allowlist (MIND-18).
//!
//! `repo_ingest` authorizes a path once and then uses the resolved path for the
//! whole ingest. A local actor that can rename directories inside an allowed
//! root can put a symlink to a directory *outside* every allowed root (or to a
//! read-only root) in place of the authorized directory while the ingest runs.
//! The ingest then reads that content into the database and writes the
//! repository identity marker into it.
//!
//! STATUS: the two attack tests are `#[ignore]`d because they FAIL against the
//! current code: that is the open security issue (a release blocker, see
//! MIND-18), reproduced here and in earlier manual runs (5 of 5, first leak
//! after 8 to 10 attempts). Run them with
//!
//!     cargo test --test toctou_allowlist -- --ignored
//!
//! When MIND-18 is fixed, remove the `#[ignore]` attributes: they then must
//! pass reliably. The two control tests run normally and guard the harness:
//! without them a green attack test could just mean the attacker never won.
//!
//! Everything happens in temporary directories created by this test; no real
//! repository or external target is touched, and the "attacker" is a thread of
//! this process renaming directories inside the temporary allowed root.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Ingest attempts per attack test. The unfixed code leaks within about ten.
const ATTEMPTS: usize = 150;
const FILES_PER_REPO: usize = 40;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Server {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    next_id: u64,
}

impl Server {
    fn start(db: &Path, specs: &[String]) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_mind"));
        cmd.env("MINDSPACE_DB", db)
            .env_remove("MINDSPACE_MCP_ALLOW_ROOTS")
            .arg("mcp");
        for s in specs {
            cmd.arg("--allow-root").arg(s);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut s = Self {
            child,
            stdin,
            out,
            next_id: 1,
        };
        let init = s.rpc(
            "initialize",
            json!({"protocolVersion":"2025-06-18","capabilities":{},
                   "clientInfo":{"name":"t","version":"0"}}),
        );
        assert!(init.get("result").is_some(), "server did not start: {init}");
        writeln!(
            s.stdin,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        s
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

    /// True when the ingest call itself succeeded.
    fn ingest(&mut self, path: &Path) -> bool {
        let r = self.call(
            "repo_ingest",
            json!({"path": path.display().to_string(), "spaces": ["p"]}),
        );
        r["isError"] == false
    }

    fn knows(&mut self, symbol: &str) -> bool {
        let r = self.call("code_search", json!({"query": symbol}));
        r["structuredContent"].to_string().contains(symbol)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

/// A small git repository whose functions are named `<prefix>_<n>`.
fn repo(path: &Path, prefix: &str) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q", "-b", "main"]);
    for i in 0..FILES_PER_REPO {
        std::fs::write(
            path.join(format!("f{i}.rs")),
            format!("pub fn {prefix}_{i}() {{}}\n"),
        )
        .unwrap();
    }
    git(path, &["add", "-A"]);
    git(path, &["commit", "-q", "-m", "init"]);
}

fn new_db(dir: &Path) -> PathBuf {
    let db = dir.join("mind.db");
    for args in [vec!["init"], vec!["space", "create", "p"]] {
        let o = Command::new(env!("CARGO_BIN_EXE_mind"))
            .env("MINDSPACE_DB", &db)
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success());
    }
    db
}

fn marker(repo: &Path) -> PathBuf {
    repo.join(".git/mindspace-repository-id")
}

/// The attacker: while running, `work` repeatedly turns into a symlink to
/// `target` and back. It only renames entries inside the temporary root, which
/// is exactly what any process with write access to that directory can do.
struct Attacker {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Attacker {
    fn start(work: PathBuf, target: PathBuf) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::spawn(move || {
            let parked = work.with_extension("orig");
            while !flag.load(Ordering::Relaxed) {
                let swapped = std::fs::rename(&work, &parked).is_ok()
                    && std::os::unix::fs::symlink(&target, &work).is_ok();
                std::thread::sleep(Duration::from_millis(4));
                if swapped {
                    let _ = std::fs::remove_file(&work);
                    let _ = std::fs::rename(&parked, &work);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            // Leave the tree as it was found.
            if !work.exists() && parked.exists() {
                let _ = std::fs::remove_file(&work);
                let _ = std::fs::rename(&parked, &work);
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for Attacker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Runs `ATTEMPTS` ingests of `work` while the attacker redirects it to
/// `forbidden_repo`, whose functions are named `FORBIDDEN_<n>`. Returns
/// (successful ingests, attempt of the first leak into the database).
fn race(
    db: &Path,
    specs: &[String],
    work: &Path,
    forbidden_repo: &Path,
    attack: bool,
) -> (usize, Option<usize>) {
    let mut s = Server::start(db, specs);
    let attacker =
        attack.then(|| Attacker::start(work.to_path_buf(), forbidden_repo.to_path_buf()));
    let (mut ok, mut leak) = (0, None);
    for n in 1..=ATTEMPTS {
        if s.ingest(work) {
            ok += 1;
        }
        if s.knows("FORBIDDEN") {
            leak = Some(n);
            break;
        }
    }
    drop(attacker);
    (ok, leak)
}

// ---------------------------------------------------------------------------
// Control tests (run normally): the harness itself is sound
// ---------------------------------------------------------------------------

/// Without an attacker nothing leaks and ingests really succeed. A green
/// attack test is only meaningful if this holds.
#[test]
fn control_without_attacker_nothing_leaks_and_ingest_works() {
    let tmp = tempfile::tempdir().unwrap();
    let (root, outside) = (tmp.path().join("root"), tmp.path().join("outside"));
    repo(&root.join("work"), "inside");
    repo(&outside, "FORBIDDEN");
    let db = new_db(tmp.path());
    let specs = [format!("{}:rw", root.canonicalize().unwrap().display())];

    let mut s = Server::start(&db, &specs);
    for _ in 0..5 {
        assert!(s.ingest(&root.join("work")), "plain ingest must work");
    }
    assert!(s.knows("inside_0"), "ingested content must be searchable");
    assert!(!s.knows("FORBIDDEN"));
    assert!(!marker(&outside).exists());
}

/// The attacker really redirects the authorized directory outside the root.
/// If this ever fails, the attack tests below would pass vacuously.
#[test]
fn control_attacker_really_redirects_the_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let (root, outside) = (tmp.path().join("root"), tmp.path().join("outside"));
    repo(&root.join("work"), "inside");
    repo(&outside, "FORBIDDEN");
    let outside_real = outside.canonicalize().unwrap();
    let work = root.join("work");

    let _attacker = Attacker::start(work.clone(), outside.clone());
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut redirected = 0;
    while Instant::now() < deadline && redirected < 3 {
        if work.canonicalize().ok().as_deref() == Some(outside_real.as_path()) {
            redirected += 1;
            std::thread::sleep(Duration::from_millis(6));
        }
        std::thread::yield_now();
    }
    assert!(
        redirected >= 3,
        "attacker never redirected the directory ({redirected})"
    );
}

// ---------------------------------------------------------------------------
// The attack (IGNORED until MIND-18 is fixed; must then pass reliably)
// ---------------------------------------------------------------------------

/// Redirect to a repository outside every allowed root: no content may reach
/// the database and no identity marker may be written there.
#[test]
#[ignore = "MIND-18: reproduces an open TOCTOU; remove #[ignore] when fixed"]
fn concurrent_swap_cannot_ingest_or_mark_a_repo_outside_all_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let (root, outside) = (tmp.path().join("root"), tmp.path().join("outside"));
    repo(&root.join("work"), "inside");
    repo(&outside, "FORBIDDEN");
    let db = new_db(tmp.path());
    let specs = [format!("{}:rw", root.canonicalize().unwrap().display())];

    let (ok, leak) = race(&db, &specs, &root.join("work"), &outside, true);

    assert_eq!(
        leak, None,
        "outside content reached the database (attempt {leak:?})"
    );
    assert!(
        !marker(&outside).exists(),
        "identity marker written outside every allowed root"
    );
    assert!(ok >= 1, "no ingest ever succeeded, the test proved nothing");
}

/// Redirect to a repository inside a READ-ONLY root: the read-only area must
/// stay unread by ingest and unwritten, even under the race.
#[test]
#[ignore = "MIND-18: reproduces an open TOCTOU; remove #[ignore] when fixed"]
fn concurrent_swap_cannot_ingest_or_mark_a_read_only_root() {
    let tmp = tempfile::tempdir().unwrap();
    let (root, ro_root) = (tmp.path().join("root"), tmp.path().join("ro"));
    let ro_repo = ro_root.join("repo");
    repo(&root.join("work"), "inside");
    repo(&ro_repo, "FORBIDDEN");
    let db = new_db(tmp.path());
    let specs = [
        format!("{}:rw", root.canonicalize().unwrap().display()),
        format!("{}:ro", ro_root.canonicalize().unwrap().display()),
    ];

    let (ok, leak) = race(&db, &specs, &root.join("work"), &ro_repo, true);

    assert_eq!(
        leak, None,
        "read-only content was ingested (attempt {leak:?})"
    );
    assert!(
        !marker(&ro_repo).exists(),
        "identity marker written into a read-only root"
    );
    assert!(ok >= 1, "no ingest ever succeeded, the test proved nothing");
}
