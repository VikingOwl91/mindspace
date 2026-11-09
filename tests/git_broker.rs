//! M7.3b.5d: the Git worker mode, broker-spawned, against the real kernel.
//!
//! The broker runs in this (unconfined) test process; the "frontend" is the code on the other end
//! of a socketpair. The worker is the real `mind git-extraction-worker` (own Landlock domain with
//! execute rights for the git binary and its interpreter only, seccomp, cgroup). Hostile and failing
//! workers are the SAME binary with a named misbehaviour (`BrokerConfig::git_gate`, test builds
//! only), so they run with the real confinement.
//!
//! Skipped, loudly, where cgroup v2 delegation, Landlock ABI 3 or /usr/bin/git is missing.

#![allow(clippy::field_reassign_with_default)]

use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mindspace::core::Uuid;
use mindspace::ingest::confined_git::GitExecutable;
use mindspace::ingest::deadline::Deadline;
use mindspace::ingest::extract::{persist_git, ExtractedGit, StoreContext};
use mindspace::ingest::outline::PathSelection;
use mindspace::ingest::{extract_git_repository_selected_with_deadline, GitScope};
use mindspace::storage::Store;
use mindspace::worker::broker::{
    recv_with_fd, Broker, BrokerConfig, GrantRoots, OpKind, RootAuthority, StaticRoots, ToBroker,
    BROKER_PROTOCOL,
};
use mindspace::worker::client::{
    extract_git_via_broker, persist_git_with_lease, BrokerReport, PersistFailure, WorkerConfig,
    WorkerError, WorkerStats,
};
use mindspace::{initialize, open_existing, SqliteStore};

const GIT: &str = "/usr/bin/git";
const MARKER: &str = "0190b2c4-7d5e-7a31-9c11-3f2d5a6b7c8d";

fn mind() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mind"))
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(GIT)
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(args)
        .env_clear()
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git available");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn git_exe() -> GitExecutable {
    GitExecutable::resolve(Path::new(GIT)).unwrap()
}

fn prerequisites() -> bool {
    if !Path::new(GIT).exists() {
        eprintln!("SKIPPED: {GIT} not found");
        return false;
    }
    if mindspace::sandbox::landlock_abi() < 3 {
        eprintln!("SKIPPED: Landlock ABI 3 or newer is required");
        return false;
    }
    let mut c = BrokerConfig::new(mind());
    c.git = Some(git_exe());
    match Broker::new(
        Box::new(StaticRoots::new(&[std::env::temp_dir()]).unwrap()),
        c,
    ) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("SKIPPED: cgroup v2 delegation is unavailable here: {e}");
            false
        }
    }
}

/// The commit-fault hooks are process-global one-shot switches; every test of this file holds this
/// lock so that an armed fault is consumed only by the test that armed it.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

macro_rules! need {
    () => {
        if !prerequisites() {
            return;
        }
        let _serial = serial();
    };
}

// ------------------------------------------------------------------------------------ fixture

struct Repo {
    _d: tempfile::TempDir,
    path: PathBuf,
}

/// A repository like `mind ingest` leaves it: two commits, Rust modules that reference each
/// other, a document and the identity marker (`marker`: its text, `None`: no marker).
fn repo_with(marker: Option<&str>, format: &str) -> Option<Repo> {
    let d = tempfile::tempdir().unwrap();
    let path = std::fs::canonicalize(d.path()).unwrap().join("repo");
    std::fs::create_dir_all(path.join("src")).unwrap();
    std::fs::create_dir_all(path.join("docs")).unwrap();
    let init = Command::new(GIT)
        .arg("-C")
        .arg(&path)
        .args([
            "init",
            "-q",
            "-b",
            "main",
            &format!("--object-format={format}"),
        ])
        .env_clear()
        .output()
        .unwrap();
    if !init.status.success() {
        return None;
    }
    let w = |rel: &str, text: &str| std::fs::write(path.join(rel), text).unwrap();
    w("docs/guide.md", "# Guide\n\nalpha text\n");
    w(
        "src/lib.rs",
        "pub mod util;\npub fn run() { util::helper(); }\n",
    );
    w("src/util.rs", "pub fn helper() {}\n");
    git(&path, &["add", "-A"]);
    git(&path, &["commit", "-q", "-m", "one"]);
    w("src/util.rs", "pub fn helper() {}\npub fn extra() {}\n");
    git(&path, &["commit", "-q", "-am", "two"]);
    if let Some(m) = marker {
        std::fs::write(path.join(".git/mindspace-repository-id"), m).unwrap();
    }
    Some(Repo { _d: d, path })
}

fn new_repo() -> Repo {
    repo_with(Some(MARKER), "sha1").unwrap()
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
    fn space(&self) -> Uuid {
        self.store().get_space("p").unwrap().id
    }
    fn raw(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.path).unwrap()
    }
    fn counts(&self) -> BTreeMap<String, i64> {
        let c = self.raw();
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
                let n = c
                    .query_row(&format!("SELECT COUNT(*) FROM \"{t}\""), [], |r| r.get(0))
                    .unwrap();
                (t, n)
            })
            .collect()
    }
    fn count(&self, t: &str) -> i64 {
        self.counts()[t]
    }
    fn revisions(&self) -> i64 {
        self.count("m6_revisions")
    }
    /// Only the code-structure tables: the ones a repeated ingestion must not grow.
    fn code(&self) -> BTreeMap<String, i64> {
        self.counts()
            .into_iter()
            .filter(|(k, _)| k.starts_with("code_") || k.starts_with("git_"))
            .collect()
    }
    /// What an older binary (`mind-code@3`) left behind. The store makes derivations immutable and
    /// versions their changes with SQL functions only its own connection has; for this throwaway
    /// database only, the UPDATE guards are lifted for the statements and restored.
    fn relabel_as_at3(&self) {
        let c = self.raw();
        let triggers: Vec<(String, String)> = c
            .prepare(
                "SELECT name, sql FROM sqlite_master WHERE type='trigger' \
                 AND tbl_name IN ('code_derivations','code_resolution_derivations') \
                 AND upper(sql) LIKE '% UPDATE %'",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .flatten()
            .collect();
        for (name, _) in &triggers {
            c.execute_batch(&format!("DROP TRIGGER \"{name}\""))
                .unwrap();
        }
        c.execute("UPDATE code_derivations SET extractor_version='3'", [])
            .unwrap();
        c.execute(
            "UPDATE code_resolution_derivations SET resolver_version='1'",
            [],
        )
        .unwrap();
        for (_, sql) in &triggers {
            c.execute_batch(sql).unwrap();
        }
    }
}

// ----------------------------------------------------------------------------------- the rig

struct Rig {
    link: UnixStream,
    _broker: Arc<Broker>,
}

fn rig(authority: Box<dyn RootAuthority>, cfg: BrokerConfig) -> Rig {
    let broker = Arc::new(Broker::new(authority, cfg).expect("broker"));
    let (a, b) = UnixStream::pair().unwrap();
    let br = broker.clone();
    std::thread::spawn(move || br.serve(b));
    Rig {
        link: a,
        _broker: broker,
    }
}

fn bcfg() -> BrokerConfig {
    let mut c = BrokerConfig::new(mind());
    c.git = Some(git_exe());
    c
}

fn gated(mode: &str) -> BrokerConfig {
    let mut c = bcfg();
    c.git_gate = Some(mode.into());
    c
}

fn fixed(root: &Path) -> Box<dyn RootAuthority> {
    Box::new(StaticRoots::new(&[root.to_path_buf()]).unwrap())
}

fn wcfg() -> WorkerConfig {
    let mut c = WorkerConfig::new(mind());
    c.wall_timeout = Duration::from_secs(120);
    c
}

type Outcome = Result<(ExtractedGit, WorkerStats, BrokerReport), WorkerError>;

fn ingest(r: &Rig, root: &Path, db: &Db) -> Outcome {
    ingest_with(r, &wcfg(), root, db, &AtomicBool::new(false))
}

fn ingest_with(r: &Rig, c: &WorkerConfig, root: &Path, db: &Db, cancel: &AtomicBool) -> Outcome {
    let store = db.store();
    extract_git_via_broker(
        c,
        &r.link,
        root,
        &[db.space()],
        &StoreContext(&store),
        cancel,
    )
}

/// Persists a verified result the way the frontend does: one transaction, commit-time lease.
fn persist(r: &Rig, db: &Db, g: ExtractedGit, rep: &BrokerReport) {
    let report = persist_git_with_lease(&db.store(), &r.link, rep.op, g, &Deadline::none())
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert!(report.objects_discovered > 0);
}

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| !s.contains(") Z "))
        .unwrap_or(false)
}

fn gone(pid: u32) -> bool {
    for _ in 0..300 {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn children_of(pid: u32) -> Vec<u32> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for e in rd.flatten() {
            let Ok(p) = e.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            if let Ok(s) = std::fs::read_to_string(format!("/proc/{p}/stat")) {
                let after = s.rsplit(')').next().unwrap_or("");
                let ppid: u32 = after
                    .split_whitespace()
                    .nth(1)
                    .and_then(|x| x.parse().ok())
                    .unwrap_or(0);
                if ppid == pid {
                    v.push(p);
                }
            }
        }
    }
    v
}

// =========================================================================== the happy path

#[test]
fn a_broker_spawned_git_worker_extracts_head_and_the_result_persists_in_one_revision() {
    need!();
    let repo = new_repo();
    let db = db();
    let r = rig(fixed(&repo.path), bcfg());
    let (g, stats, rep) = ingest(&r, &repo.path, &db).unwrap();
    // observed by the broker from outside, not told by the worker
    assert_eq!(rep.observed.seccomp, "2");
    assert_eq!(rep.observed.no_new_privs, "1");
    assert_eq!(rep.observed.tracer_pid, "0");
    assert!(rep.observed.cap_eff.chars().all(|c| c == '0'));
    assert!(rep.observed.cgroup.as_deref().unwrap().contains("/op-"));
    assert_eq!(rep.exit, "exit code 0");
    assert!(rep.exited_by_itself);
    assert_eq!(rep.descendants_killed, 0);
    assert!(gone(rep.observed.pid));
    // the worker's own report of its Landlock domain (it executes git, and only git)
    assert!(stats.hello.as_ref().unwrap().sandbox.landlock_abi >= 3);
    // the identity is the broker's, read from the marker
    assert_eq!(g.observation.repository_id.to_string(), MARKER);
    // HEAD scope, verified
    let head = git(&repo.path, &["rev-parse", "HEAD"]);
    assert_eq!(g.observation.view_commits, vec![head.clone()]);
    assert_eq!(g.observation.commits[0].oid, head);
    let v = stats.verification.unwrap();
    assert!(
        v.content_verified_objects >= 4 && v.content_verified_blobs >= 3,
        "{v:?}"
    );
    // persistence: one transaction, ONE recording revision
    let r0 = db.revisions();
    persist(&r, &db, g, &rep);
    assert_eq!(db.revisions() - r0, 1);
    assert!(db.count("code_derivations") > 0 && db.count("git_commits") == 1);
}

#[test]
fn the_brokered_result_stores_the_same_structure_as_the_in_process_ingestion() {
    need!();
    let repo = new_repo();
    let db_a = db();
    let r = rig(fixed(&repo.path), bcfg());
    let (g, _, rep) = ingest(&r, &repo.path, &db_a).unwrap();
    persist(&r, &db_a, g, &rep);
    // the reference: the in-process HEAD ingestion into a copy of an empty database with the SAME
    // space ids
    let dir = tempfile::tempdir().unwrap();
    let pb = dir.path().join("b.db");
    {
        // db_a before persistence is not available; build the twin from a fresh copy of the
        // initial schema + the space of db_a
        initialize(&pb).unwrap();
        let s = open_existing(&pb).unwrap();
        s.create_space("p", "").unwrap();
    }
    let b = open_existing(&pb).unwrap();
    let ex = extract_git_repository_selected_with_deadline(
        &StoreContext(&b),
        &repo.path,
        &["p"],
        &PathSelection::all(),
        GitScope::Head,
        &Deadline::none(),
    )
    .unwrap();
    persist_git(&b, ex, &Deadline::none()).unwrap();
    drop(b);
    let q = |p: &Path, sql: &str| -> Vec<String> {
        let c = rusqlite::Connection::open(p).unwrap();
        let mut v: Vec<String> = c
            .prepare(sql)
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .flatten()
            .collect();
        v.sort();
        v
    };
    for sql in [
        "SELECT kind||'|'||name||'|'||syntax_path FROM code_entities",
        "SELECT state FROM code_relationship_resolutions",
        "SELECT language||'|'||status||'|'||error_count FROM code_derivations",
        "SELECT path FROM code_contexts",
        "SELECT oid||'|'||tree_oid FROM git_commits WHERE oid = (SELECT commit_oid FROM git_views)",
        "SELECT content_hash FROM code_inputs",
    ] {
        assert_eq!(q(&db_a.path, sql), q(&pb, sql), "{sql}");
    }
}

#[test]
fn repeated_ingestion_reuses_announced_derivations_and_an_extractor_change_re_derives() {
    need!();
    let repo = new_repo();
    let db = db();
    let r = rig(fixed(&repo.path), bcfg());
    // first: nothing known
    let (g, s1, rep) = ingest(&r, &repo.path, &db).unwrap();
    assert_eq!(s1.known_sent, 0);
    assert!(g.reused.is_empty());
    persist(&r, &db, g, &rep);
    let once = db.code();
    // second: every source blob was announced, comes back as a verified placeholder, adds nothing
    let (g, s2, rep) = ingest(&r, &repo.path, &db).unwrap();
    assert!(s2.known_sent >= 2, "{s2:?}");
    assert_eq!(g.reused.len() as u64, s2.known_sent);
    persist(&r, &db, g, &rep);
    assert_eq!(
        once,
        db.code(),
        "a repeated ingestion grew the derived structure"
    );
    // an older extractor's derivations are not announced as known: the worker derives again
    db.relabel_as_at3();
    let (g, s3, rep) = ingest(&r, &repo.path, &db).unwrap();
    assert_eq!(s3.known_sent, 0, "@3 derivations were announced under @4");
    assert!(g.reused.is_empty());
    assert!(g
        .code_inputs
        .iter()
        .any(|c| !c.derivation.entities.is_empty()));
    persist(&r, &db, g, &rep);
    let upgraded = db.code();
    assert_eq!(upgraded["code_derivations"], 2 * once["code_derivations"]);
    assert_eq!(upgraded["code_inputs"], once["code_inputs"]);
    // and it settles again
    let (g, s4, rep) = ingest(&r, &repo.path, &db).unwrap();
    assert!(s4.known_sent >= 2);
    persist(&r, &db, g, &rep);
    assert_eq!(upgraded, db.code());
}

// ================================================================== refusals before any worker

fn refused(out: Outcome) -> String {
    match out {
        Err(WorkerError::Refused(m)) => m,
        other => panic!("expected a refusal, got {:?}", other.map(|_| ())),
    }
}

#[test]
fn git_is_refused_without_configuration_for_an_unauthorized_root_and_for_unsupported_repositories()
{
    need!();
    let db = db();
    // a broker without Git configured: no worker, no fallback
    let repo = new_repo();
    let r = rig(fixed(&repo.path), BrokerConfig::new(mind()));
    let m = refused(ingest(&r, &repo.path, &db));
    assert!(m.contains("not configured"), "{m}");
    // an unauthorized root
    let other = tempfile::tempdir().unwrap();
    let r = rig(fixed(&repo.path), bcfg());
    let m = refused(ingest(
        &r,
        &std::fs::canonicalize(other.path()).unwrap(),
        &db,
    ));
    assert!(m.contains("not authorized"), "{m}");

    // layouts and identities the broker refuses BEFORE a worker exists
    #[allow(clippy::type_complexity)]
    let cases: Vec<(&str, Box<dyn Fn(&Repo)>)> = vec![
        (
            "identity_missing",
            Box::new(|r| {
                std::fs::remove_file(r.path.join(".git/mindspace-repository-id")).unwrap();
            }),
        ),
        (
            "not a UUID",
            Box::new(|r| {
                std::fs::write(r.path.join(".git/mindspace-repository-id"), "not-a-uuid").unwrap();
            }),
        ),
        (
            "alternates",
            Box::new(|r| {
                std::fs::write(r.path.join(".git/objects/info/alternates"), "/elsewhere\n")
                    .unwrap();
            }),
        ),
        (
            "grafts",
            Box::new(|r| {
                std::fs::write(r.path.join(".git/info/grafts"), "").unwrap();
            }),
        ),
        (
            "commondir",
            Box::new(|r| {
                std::fs::write(r.path.join(".git/commondir"), "..\n").unwrap();
            }),
        ),
        (
            "linked worktrees",
            Box::new(|r| {
                let wt = r.path.parent().unwrap().join("wt");
                git(
                    &r.path,
                    &[
                        "worktree",
                        "add",
                        "-q",
                        "--detach",
                        wt.to_str().unwrap(),
                        "HEAD~1",
                    ],
                );
            }),
        ),
        (
            "symbolic link",
            Box::new(|r| {
                let real = r.path.parent().unwrap().join("moved.git");
                std::fs::rename(r.path.join(".git"), &real).unwrap();
                std::os::unix::fs::symlink(&real, r.path.join(".git")).unwrap();
            }),
        ),
    ];
    for (needle, tamper) in cases {
        let repo = new_repo();
        tamper(&repo);
        let pid = Arc::new(AtomicU32::new(0));
        let mut c = wcfg();
        c.observe_pid = Some(pid.clone());
        let r = rig(fixed(&repo.path), bcfg());
        let before = db.counts();
        let m = refused(ingest_with(
            &r,
            &c,
            &repo.path,
            &db,
            &AtomicBool::new(false),
        ));
        assert!(m.contains(needle), "wanted `{needle}`: {m}");
        assert_eq!(
            pid.load(Ordering::SeqCst),
            0,
            "{needle}: a worker was started"
        );
        assert_eq!(before, db.counts());
    }
    // a root that is no repository at all
    let plain = tempfile::tempdir().unwrap();
    let plain = std::fs::canonicalize(plain.path()).unwrap();
    let r = rig(fixed(&plain), bcfg());
    let m = refused(ingest(&r, &plain, &db));
    assert!(m.contains("not a git repository"), "{m}");
}

#[test]
fn a_sha256_repository_is_refused_before_anything_is_persisted() {
    need!();
    let Some(repo) = repo_with(Some(MARKER), "sha256") else {
        eprintln!("SKIPPED: this git cannot create a SHA-256 repository");
        return;
    };
    let db = db();
    let before = db.counts();
    let r = rig(fixed(&repo.path), bcfg());
    // the broker cannot tell (it runs no git); the confined worker refuses at open
    let e = ingest(&r, &repo.path, &db).expect_err("a SHA-256 repository was accepted");
    assert!(
        !matches!(e, WorkerError::Refused(ref m) if m.contains("not authorized")),
        "{e}"
    );
    assert_eq!(before, db.counts());
    // the repository was not touched
    assert_eq!(
        std::fs::read_to_string(repo.path.join(".git/mindspace-repository-id")).unwrap(),
        MARKER
    );
}

// ====================================================== hostile workers (real confinement)

fn hostile(mode: &str, needle: &str) {
    let repo = new_repo();
    let db = db();
    let before = db.counts();
    let pid = Arc::new(AtomicU32::new(0));
    let mut c = wcfg();
    c.observe_pid = Some(pid.clone());
    let r = rig(fixed(&repo.path), gated(mode));
    let e = ingest_with(&r, &c, &repo.path, &db, &AtomicBool::new(false))
        .err()
        .unwrap_or_else(|| panic!("`{mode}`: a hostile worker's result was accepted"));
    let text = e.to_string();
    assert!(
        text.contains(needle),
        "`{mode}`: wanted `{needle}`, got: {text}"
    );
    assert_eq!(before, db.counts(), "`{mode}`: rows were written");
    assert!(
        gone(pid.load(Ordering::SeqCst)),
        "`{mode}`: the worker is still running"
    );
    // the broker serves an honest operation afterwards
    let r2 = rig(fixed(&repo.path), bcfg());
    let (g, _, rep) = ingest(&r2, &repo.path, &db).unwrap();
    persist(&r2, &db, g, &rep);
}

#[test]
fn a_worker_that_lies_about_the_repository_identity_is_refused() {
    need!();
    // in the manifest (the parent holds the broker's value) ...
    hostile("lie-manifest-identity", "another repository");
    // ... and in the stream's header
    hostile("lie-header-identity", "another repository");
}

#[test]
fn a_worker_refuses_a_repository_identity_other_than_the_one_the_broker_bound() {
    need!();
    // the marker read by the worker is not the identity it was told: it reads nothing further
    hostile("broker-wrong-identity", "before the manifest");
}

#[test]
fn a_worker_cannot_swap_the_head_the_root_the_operation_or_forge_reuse() {
    need!();
    hostile("other-head-manifest", "another head");
    hostile("wrong-root-label", "another root");
    hostile("replay-op", "another operation");
    hostile("forge-reuse", "did not announce");
}

#[test]
fn malformed_oversized_and_truncated_streams_are_refused_and_nothing_is_persisted() {
    need!();
    hostile("garbage", "");
    hostile("oversized", "");
    hostile("truncate", "");
}

// ================================================================================ lifecycle

#[test]
fn a_crashing_git_worker_costs_an_error_and_the_broker_serves_the_next_operation() {
    need!();
    let repo = new_repo();
    let db = db();
    let pid = Arc::new(AtomicU32::new(0));
    let mut c = wcfg();
    c.observe_pid = Some(pid.clone());
    let r = rig(fixed(&repo.path), gated("crash"));
    let before = db.counts();
    let e = ingest_with(&r, &c, &repo.path, &db, &AtomicBool::new(false))
        .err()
        .unwrap();
    assert!(
        matches!(e, WorkerError::Died { .. } | WorkerError::Stream(_)),
        "{e}"
    );
    assert!(gone(pid.load(Ordering::SeqCst)));
    assert_eq!(before, db.counts());
}

/// Starts an operation against a worker that hangs (with a git child blocked on its stdin) and
/// returns what a test needs to watch it.
#[allow(clippy::type_complexity)]
fn start_hanging(
    r: &Rig,
    repo: &Repo,
    db: Arc<Db>,
    cfg: WorkerConfig,
) -> (
    Arc<AtomicU32>,
    Arc<AtomicBool>,
    std::thread::JoinHandle<Result<(), WorkerError>>,
) {
    let pid = Arc::new(AtomicU32::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    let mut cfg = cfg;
    cfg.observe_pid = Some(pid.clone());
    let link = r.link.try_clone().unwrap();
    let c2 = cancel.clone();
    let root = repo.path.clone();
    let h = std::thread::spawn(move || {
        let store = db.store();
        extract_git_via_broker(
            &cfg,
            &link,
            &root,
            &[db.space()],
            &StoreContext(&store),
            &c2,
        )
        .map(|_| ())
    });
    (pid, cancel, h)
}

fn wait_for_git_child(pid: &AtomicU32) -> (u32, u32) {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        let w = pid.load(Ordering::SeqCst);
        if w != 0 {
            if let Some(c) = children_of(w).first() {
                return (w, *c);
            }
        }
        assert!(Instant::now() < end, "the git child never appeared");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_hung_worker_and_its_git_child_are_ended_at_the_broker_wall_clock() {
    need!();
    let repo = new_repo();
    let db = Arc::new(db());
    let mut bc = gated("hang");
    bc.wall_timeout = Duration::from_secs(4);
    let r = rig(fixed(&repo.path), bc);
    let t = Instant::now();
    let (pid, _cancel, h) = start_hanging(&r, &repo, db.clone(), wcfg());
    let (worker, child) = wait_for_git_child(&pid);
    // the git child is inside the worker's domain
    let status = std::fs::read_to_string(format!("/proc/{child}/status")).unwrap();
    assert!(status
        .lines()
        .any(|l| l.starts_with("NoNewPrivs:") && l.trim().ends_with('1')));
    let e = h.join().unwrap().unwrap_err();
    assert!(matches!(e, WorkerError::Timeout(_)), "{e}");
    assert!(t.elapsed() < Duration::from_secs(30));
    assert!(gone(worker), "the worker survived the wall clock");
    assert!(gone(child), "the git child survived the wall clock");
    assert_eq!(db.count("git_commits"), 0);
}

#[test]
fn cancelling_ends_the_worker_and_its_git_child() {
    need!();
    let repo = new_repo();
    let db = Arc::new(db());
    let r = rig(fixed(&repo.path), gated("hang"));
    let (pid, cancel, h) = start_hanging(&r, &repo, db.clone(), wcfg());
    let (worker, child) = wait_for_git_child(&pid);
    cancel.store(true, Ordering::SeqCst);
    let e = h.join().unwrap().unwrap_err();
    assert!(matches!(e, WorkerError::Cancelled), "{e}");
    assert!(gone(worker) && gone(child));
}

// ============================================================ revocation and the commit lease

struct Grants {
    /// `XDG_STATE_HOME` of the revoking CLI: the store lives in `<xdg>/mindspace`.
    xdg: tempfile::TempDir,
    _dbdir: tempfile::TempDir,
    store: mindspace::grants::Store,
    id: String,
    authority: Option<GrantRoots>,
}

fn granted(root: &Path) -> Grants {
    use mindspace::grants::{add_grant, NewGrant, PathPolicy, Store};
    let xdg = tempfile::tempdir().unwrap();
    let state = xdg.path().join("mindspace");
    let dbdir = tempfile::tempdir().unwrap();
    let store = Store::in_dir(&state);
    let policy = PathPolicy::standard(&state, Some(dbdir.path()), None);
    let g = add_grant(
        &store,
        &policy,
        &NewGrant {
            path: root,
            access: mindspace::mcp::Access::ReadOnly,
            ttl: None,
            request_id: None,
            source: "cli-add",
        },
        time::OffsetDateTime::now_utc(),
    )
    .unwrap();
    let authority = Some(GrantRoots {
        store: Store::in_dir(&state),
        policy,
    });
    Grants {
        xdg,
        _dbdir: dbdir,
        store,
        id: g.id,
        authority,
    }
}

impl Grants {
    fn revoke(&self) {
        mindspace::grants::revoke_grant(
            &self.store,
            &self.id,
            Some("test"),
            time::OffsetDateTime::now_utc(),
        )
        .unwrap();
    }

    /// `mind grant revoke` as a separate process: it needs the grant store's exclusive lock, which
    /// the broker holds (shared) for the duration of a commit lease.
    fn revoke_process(&self) -> std::process::Child {
        Command::new(mind())
            .args(["grant", "revoke", &self.id])
            .env_clear()
            .env("XDG_STATE_HOME", self.xdg.path())
            .env("HOME", self.xdg.path())
            .env("MINDSPACE_DB", self.xdg.path().join("unused.db"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }
}

#[test]
fn revocation_during_extraction_ends_the_operation() {
    need!();
    let repo = new_repo();
    let db = Arc::new(db());
    let mut g = granted(&repo.path);
    let r = rig(Box::new(g.authority.take().unwrap()), gated("hang"));
    let (pid, _cancel, h) = start_hanging(&r, &repo, db.clone(), wcfg());
    let (worker, child) = wait_for_git_child(&pid);
    g.revoke();
    let e = h.join().unwrap().unwrap_err();
    assert!(matches!(e, WorkerError::Revoked), "{e}");
    assert!(gone(worker) && gone(child));
    assert_eq!(db.count("git_commits"), 0);
}

#[test]
fn revocation_before_persistence_denies_the_commit_and_leaves_no_trace() {
    need!();
    let repo = new_repo();
    let db = db();
    let mut g = granted(&repo.path);
    let r = rig(Box::new(g.authority.take().unwrap()), bcfg());
    let (res, _, rep) = ingest(&r, &repo.path, &db).unwrap();
    // the grant is revoked after the stream was verified and before the commit
    g.revoke();
    let before = db.counts();
    let r0 = db.revisions();
    match persist_git_with_lease(&db.store(), &r.link, rep.op, res, &Deadline::none()) {
        Err(PersistFailure::Denied { kind, .. }) => assert_eq!(kind, "revoked"),
        other => panic!("{other:?}"),
    }
    assert_eq!(before, db.counts(), "a denied commit left rows behind");
    assert_eq!(
        db.revisions(),
        r0,
        "a denied commit consumed a recording revision"
    );
}

#[test]
fn a_commit_needs_the_lease_of_its_own_operation() {
    need!();
    let repo = new_repo();
    let db = db();
    let r = rig(fixed(&repo.path), bcfg());
    let (res, _, rep) = ingest(&r, &repo.path, &db).unwrap();
    // another operation's id, or none: no lease
    let before = db.counts();
    match persist_git_with_lease(
        &db.store(),
        &r.link,
        Uuid::now_v7(),
        res.clone(),
        &Deadline::none(),
    ) {
        Err(PersistFailure::Denied { .. }) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(before, db.counts());
    // the link is out of step after a denied lease; a new rig for the honest persistence
    let _ = rep;
}

// ====================================================================== commit outcomes

#[test]
fn a_vetoed_sqlite_commit_is_a_clean_rollback_and_a_lost_acknowledgement_is_unknown() {
    need!();
    use mindspace::storage::sqlite::gate::{FAIL_COMMIT, LOSE_ACK};
    let repo = new_repo();
    let db = db();
    let r = rig(fixed(&repo.path), bcfg());

    // ROLLED_BACK: SQLite refuses the COMMIT. Nothing is committed, no revision is consumed, the
    // lease is released, and the same database handle is usable afterwards.
    let (g, _, rep) = ingest(&r, &repo.path, &db).unwrap();
    let before = db.counts();
    let r0 = db.revisions();
    let store = db.store();
    FAIL_COMMIT.store(true, Ordering::SeqCst);
    let e = persist_git_with_lease(&store, &r.link, rep.op, g, &Deadline::none()).unwrap_err();
    match &e {
        PersistFailure::Store(err) => assert!(
            !matches!(err, mindspace::core::Error::CommitOutcomeUnknown { .. }),
            "{err}"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(before, db.counts(), "a vetoed commit left rows behind");
    assert_eq!(db.revisions(), r0);
    let (g, _, rep) = ingest(&r, &repo.path, &db).unwrap();
    persist_git_with_lease(&store, &r.link, rep.op, g, &Deadline::none())
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(db.revisions() - r0, 1);

    // UNKNOWN: the commit succeeded but its acknowledgement is lost. The data IS committed; the
    // error says so honestly ("unknown"), the handle is discarded, and a retry through a fresh
    // handle adds no derived structure.
    let db2 = self::db();
    let (g, _, rep) = ingest(&r, &repo.path, &db2).unwrap();
    let store2 = db2.store();
    LOSE_ACK.store(true, Ordering::SeqCst);
    let e = persist_git_with_lease(&store2, &r.link, rep.op, g, &Deadline::none()).unwrap_err();
    assert!(
        matches!(
            e,
            PersistFailure::Store(mindspace::core::Error::CommitOutcomeUnknown { .. })
        ),
        "{e:?}"
    );
    assert_eq!(db2.count("git_commits"), 1, "the committed data is there");
    let committed = db2.code();
    let (g, _, rep) = ingest(&r, &repo.path, &db2).unwrap();
    persist_git_with_lease(&db2.store(), &r.link, rep.op, g, &Deadline::none())
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(
        committed,
        db2.code(),
        "the retry after an unknown outcome grew the structure"
    );
}

// ================================================================== revocation ordering

#[test]
fn a_commit_that_outlasts_the_grant_ceiling_keeps_its_lease_and_a_revocation_waits_for_it() {
    need!();
    use mindspace::storage::sqlite::gate::PAUSE_IN_COMMIT;
    let repo = new_repo();
    let db = db();
    let mut g = granted(&repo.path);
    let mut bc = bcfg();
    // the lease must be extended by CommitBegin: without it the broker lets revocations proceed
    // after this ceiling, in the middle of the commit
    bc.commit_grant_ceiling = Duration::from_millis(300);
    bc.commit_inflight_ceiling = Duration::from_secs(30);
    let r = rig(Box::new(g.authority.take().unwrap()), bc);
    let (res, _, rep) = ingest(&r, &repo.path, &db).unwrap();
    let pause = tempfile::tempdir().unwrap();
    // opened BEFORE the hook is armed: opening a handle must not consume the one-shot pause
    let store = db.store();
    std::env::set_var(
        "MINDSPACE_STORE_GATE_PAUSE_IN_COMMIT",
        pause.path().join("commit"),
    );
    PAUSE_IN_COMMIT.store(true, Ordering::SeqCst);
    let (committed_at, outcome, still_waiting) = std::thread::scope(|s| {
        let watcher = s.spawn(|| {
            let reached = pause.path().join("commit.reached");
            let end = Instant::now() + Duration::from_secs(20);
            while !reached.exists() {
                assert!(Instant::now() < end, "the commit never reached the pause");
                std::thread::sleep(Duration::from_millis(5));
            }
            // the commit is running inside SQLite; start the revocation (another process)
            let mut revoke = g.revoke_process();
            // well past the grant ceiling (300 ms): the revocation must STILL be waiting
            std::thread::sleep(Duration::from_millis(1500));
            let early = revoke.try_wait().unwrap();
            let waiting = early.is_none();
            std::fs::write(pause.path().join("commit.go"), b"").unwrap();
            (revoke, waiting)
        });
        let outcome = persist_git_with_lease(&store, &r.link, rep.op, res, &Deadline::none());
        let committed_at = Instant::now();
        let (mut revoke, waiting) = watcher.join().unwrap();
        // the revocation completes once the commit is over
        let status = revoke.wait().unwrap();
        assert!(status.success(), "{status:?}");
        (committed_at, outcome, waiting)
    });
    std::env::remove_var("MINDSPACE_STORE_GATE_PAUSE_IN_COMMIT");
    assert!(
        still_waiting,
        "the revocation completed in the middle of the commit"
    );
    outcome.unwrap_or_else(|e| panic!("{e:?}"));
    let _ = committed_at;
    assert_eq!(db.count("git_commits"), 1);
}

// ============================================================================ operation identity

#[test]
fn a_git_operation_id_cannot_be_reused_and_the_identity_in_ready_is_the_brokers() {
    need!();
    use std::io::{BufRead, Write};
    let repo = new_repo();
    let db = db();
    let r = rig(fixed(&repo.path), bcfg());
    let op = Uuid::now_v7();
    let begin = |op: Uuid| {
        let mut b = serde_json::to_vec(&ToBroker::Begin {
            v: BROKER_PROTOCOL,
            op,
            kind: OpKind::GitExtract,
            root: repo.path.display().to_string(),
            spaces: vec![db.space()],
        })
        .unwrap();
        b.push(b'\n');
        b
    };
    (&r.link).write_all(&begin(op)).unwrap();
    let (bytes, fd) = recv_with_fd(&r.link).unwrap();
    let ready = String::from_utf8_lossy(&bytes).to_string();
    assert!(fd.is_some(), "{ready}");
    // the identity is in the broker's answer; the operation is the one that was asked for
    assert!(
        ready.contains(MARKER) && ready.contains(&op.to_string()),
        "{ready}"
    );
    // end it
    (&r.link).write_all(b"{\"type\":\"cancel\"}\n").unwrap();
    let mut line = String::new();
    std::io::BufReader::new(r.link.try_clone().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(line.contains("finished"), "{line}");
    // the same id again, on the same broker: refused, no worker, no descriptor
    (&r.link).write_all(&begin(op)).unwrap();
    let (bytes, fd) = recv_with_fd(&r.link).unwrap();
    assert!(fd.is_none());
    assert!(
        String::from_utf8_lossy(&bytes).contains("already used"),
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    // a fresh id is served
    (&r.link).write_all(&begin(Uuid::now_v7())).unwrap();
    let (_, fd) = recv_with_fd(&r.link).unwrap();
    assert!(fd.is_some());
    (&r.link).write_all(b"{\"type\":\"cancel\"}\n").unwrap();
}
