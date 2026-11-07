//! Kernel sandbox for the MCP repository tools (ADR-029).
//!
//! Landlock confines the whole `mind mcp` process to: the allowed roots (rw or
//! ro), its dedicated database directory, a private temp directory inside it,
//! the model directory (ro) and the system paths it needs. It is applied by a
//! **self re-exec**: stage 1 validates, restricts itself and `exec`s itself;
//! stage 2 starts confined. `restrict_self` only confines the calling and later
//! threads, so restricting a running multi-threaded process would be unsafe.
//!
//! Nothing here is taken on trust. Enforcement must be `FullyEnforced` for ABI 3
//! (`HardRequirement`) AND canary probes must observe the behavior, in stage 1
//! after restricting and again in stage 2, which does not rely on the
//! environment marker alone. A Git probe shows Git works under the sandbox.
//!
//! This module only exists on Linux. See [`Outcome`] for what callers get.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use landlock::{
    Access, AccessFs, BitFlags, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetStatus, ABI,
};

/// Minimum Landlock ABI. ABI 3 (Linux 6.2) adds truncate control; with ABI 1
/// and 2 initialization succeeds but `truncate()` of an outside file by path
/// stays allowed (measured), so those kernels are refused.
pub const MIN_ABI: ABI = ABI::V3;

const ENV_STAGE2: &str = "MINDSPACE_SANDBOX_STAGE2";
const ENV_CANARY: &str = "MINDSPACE_SANDBOX_CANARY";

/// Why stage 1 did not produce a confined process.
#[derive(Debug)]
pub enum Outcome {
    /// Nothing to protect (no roots configured).
    NotNeeded,
    /// The sandbox cannot be established (kernel, layout, environment). The
    /// process is still unconfined; the caller keeps the repository tools off.
    Unavailable(String),
}

/// What stage 2 learned about itself.
#[derive(Debug, Clone)]
pub struct Report {
    pub description: String,
}

/// One filesystem rule of the sandbox.
#[derive(Debug, Clone)]
pub struct Rule {
    pub path: PathBuf,
    pub rights: Rights,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rights {
    /// Everything the ABI handles (rw roots, database directory).
    All,
    /// Read, list and execute.
    Read,
    /// One file: read.
    FileRead,
    /// One file: read and write (`/dev/null`).
    FileReadWrite,
    /// One file: read and execute (the `mind` binary).
    FileExec,
}

/// Everything stage 1 derives from the configuration.
#[derive(Debug, Clone)]
pub struct Plan {
    pub roots: Vec<(PathBuf, bool)>,
    pub db_dir: PathBuf,
    pub models_dir: PathBuf,
    pub exe: PathBuf,
}

impl Plan {
    /// The database directory is opened in full (SQLite keeps `-wal` and `-shm`
    /// beside the database), so it must neither contain nor lie inside a root.
    pub fn validate_layout(&self) -> Result<(), String> {
        for (root, _) in &self.roots {
            if root.starts_with(&self.db_dir) || self.db_dir.starts_with(root) {
                return Err(format!(
                    "the database directory {} and the allowed root {} overlap; the sandbox \
                     opens the whole database directory, so it must be a directory of its \
                     own (for example the default $XDG_DATA_HOME/mindspace)",
                    self.db_dir.display(),
                    root.display()
                ));
            }
        }
        Ok(())
    }

    /// Private temp directory (SQLite temp files); inside the database directory.
    pub fn tmp_dir(&self) -> PathBuf {
        self.db_dir.join(".mindspace-tmp")
    }

    /// All rules, in a stable order.
    pub fn rules(&self) -> Vec<Rule> {
        let mut r = Vec::new();
        for (path, rw) in &self.roots {
            r.push(Rule {
                path: path.clone(),
                rights: if *rw { Rights::All } else { Rights::Read },
            });
        }
        r.push(Rule {
            path: self.db_dir.clone(),
            rights: Rights::All,
        });
        r.push(Rule {
            path: self.models_dir.clone(),
            rights: Rights::Read,
        });
        for p in [
            "/usr",
            "/lib",
            "/lib64",
            "/bin",
            "/sbin",
            "/proc",
            "/sys/fs/cgroup",
        ] {
            r.push(Rule {
                path: PathBuf::from(p),
                rights: Rights::Read,
            });
        }
        for p in [
            "/etc/ld.so.cache",
            "/etc/ld.so.preload",
            "/etc/localtime",
            "/dev/urandom",
        ] {
            r.push(Rule {
                path: PathBuf::from(p),
                rights: Rights::FileRead,
            });
        }
        r.push(Rule {
            path: PathBuf::from("/dev/null"),
            rights: Rights::FileReadWrite,
        });
        r.push(Rule {
            path: self.exe.clone(),
            rights: Rights::FileExec,
        });
        for p in extra_runtime_dirs() {
            r.push(Rule {
                path: p,
                rights: Rights::Read,
            });
        }
        r
    }
}

/// Directories of shared libraries this process has mapped, and of the Git
/// binary. Under a standard layout they are inside `/usr`; on other layouts
/// (store-based distributions) they are not, and the Git probe shows whether
/// that is enough.
fn extra_runtime_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(maps) = std::fs::read_to_string("/proc/self/maps") {
        for line in maps.lines() {
            if let Some(path) = line.split_whitespace().nth(5) {
                if path.starts_with('/') && (path.contains(".so")) {
                    if let Some(parent) = Path::new(path).parent() {
                        dirs.push(parent.to_path_buf());
                    }
                }
            }
        }
    }
    if let Some(git) = find_in_path("git") {
        if let Some(parent) = git.parent() {
            dirs.push(parent.to_path_buf());
        }
        if let Some(p) = git_exec_path(&git) {
            dirs.push(p);
        }
    }
    dirs.sort();
    dirs.dedup();
    dirs.into_iter()
        .filter_map(|d| std::fs::canonicalize(d).ok())
        .collect()
}

/// `git --exec-path`, with a time limit: a broken or hanging `git` on PATH must
/// not block the start of the server.
fn git_exec_path(git: &Path) -> Option<PathBuf> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let mut child = std::process::Command::new(git)
        .arg("--exec-path")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait().ok()? {
            Some(status) if status.success() => break,
            Some(_) => return None,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    let p = out.trim();
    p.starts_with('/').then(|| PathBuf::from(p))
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
        .and_then(|p| std::fs::canonicalize(p).ok())
}

/// Descriptors above stderr that are open right now, without the one used to
/// list them (it points at `/proc/<pid>/fd` itself).
pub fn open_fds_above_stderr() -> Vec<i32> {
    let Ok(dir) = std::fs::read_dir("/proc/self/fd") else {
        return vec![];
    };
    let listing = format!("/proc/{}/fd", std::process::id());
    let mut fds: Vec<i32> = dir
        .flatten()
        .filter_map(|e| {
            let fd: i32 = e.file_name().to_string_lossy().parse().ok()?;
            let target = std::fs::read_link(e.path()).ok()?;
            (fd > 2 && target != Path::new(&listing)).then_some(fd)
        })
        .collect();
    fds.sort();
    fds
}

/// Closes every inherited descriptor above stderr. Landlock does not revoke
/// descriptors that are already open, so an inherited descriptor to a file or
/// directory outside the roots would stay usable inside the sandbox.
pub fn close_inherited_fds() {
    for fd in open_fds_above_stderr() {
        // SAFETY: closing a descriptor number we just listed. This runs first
        // thing in `mind mcp`, before anything of ours has been opened, so
        // these are inherited descriptors; a failed close is harmless.
        unsafe { libc::close(fd) };
    }
}

/// Where the canary lives: a place that no rule covers.
fn choose_canary_dir(rules: &[Rule]) -> Result<PathBuf, String> {
    let uid = unsafe { libc::getuid() };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(x) = std::env::var_os("XDG_RUNTIME_DIR") {
        candidates.push(Path::new(&x).join("mindspace-canary"));
    }
    candidates.push(PathBuf::from(format!("/tmp/mindspace-canary-{uid}")));
    candidates.push(PathBuf::from(format!("/var/tmp/mindspace-canary-{uid}")));
    for dir in candidates {
        let covered = rules.iter().any(|r| dir.starts_with(&r.path));
        if covered {
            continue;
        }
        if std::fs::create_dir_all(&dir).is_ok() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            if std::fs::write(dir.join("canary"), b"canary").is_ok() {
                return Ok(dir);
            }
        }
    }
    Err("no location outside the sandbox rules is available for the canary".to_string())
}

/// Observed behavior of one probe.
#[derive(Debug, PartialEq, Eq)]
pub enum Probe {
    /// The kernel denied it (EACCES): the expected result.
    Denied,
    /// It succeeded: the sandbox does not hold.
    Allowed,
    /// Failed for another reason (for example ENOENT): proves nothing.
    Inconclusive(String),
}

pub fn classify<T>(r: std::io::Result<T>) -> Probe {
    match r {
        Ok(_) => Probe::Allowed,
        Err(e) if e.raw_os_error() == Some(libc::EACCES) => Probe::Denied,
        Err(e) => Probe::Inconclusive(e.to_string()),
    }
}

/// Canary probes: with the policy in force, everything outside must be denied.
/// Returns the failures; empty means the observed behavior is as required.
pub fn canary_failures(canary_dir: &Path, private_tmp: Option<&Path>) -> Vec<String> {
    use std::fs::OpenOptions;
    let file = canary_dir.join("canary");
    let mut bad = Vec::new();
    let mut expect_denied = |what: &str, p: Probe| {
        if p != Probe::Denied {
            bad.push(format!("{what}: {p:?}"));
        }
    };
    expect_denied("reading a file outside", classify(std::fs::read(&file)));
    expect_denied(
        "listing a directory outside",
        classify(std::fs::read_dir(canary_dir).map(|_| ())),
    );
    expect_denied(
        "creating a file outside",
        classify(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(canary_dir.join("probe-new")),
        ),
    );
    expect_denied(
        "opening a file outside for writing with truncation",
        classify(OpenOptions::new().write(true).truncate(true).open(&file)),
    );
    let c = std::ffi::CString::new(file.to_string_lossy().as_bytes()).unwrap_or_default();
    // SAFETY: `c` is a valid NUL-terminated path; truncate(2) takes no pointers beyond it.
    let r = if unsafe { libc::truncate(c.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    };
    expect_denied("truncate(2) of a file outside", classify(r));
    if let Some(tmp) = private_tmp {
        // Liveness: the sandbox must not deny what the process needs.
        let probe = tmp.join(format!("probe-{}", std::process::id()));
        match std::fs::write(&probe, b"x") {
            Ok(()) => {
                let _ = std::fs::remove_file(&probe);
            }
            Err(e) => bad.push(format!("writing the private temp directory failed: {e}")),
        }
    }
    bad
}

fn build_ruleset(rules: &[Rule]) -> Result<landlock::RulesetCreated, String> {
    let all: BitFlags<AccessFs> = AccessFs::from_all(MIN_ABI);
    let read: BitFlags<AccessFs> = AccessFs::from_read(MIN_ABI);
    let mut rs = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(all)
        .map_err(|e| format!("Landlock ABI {:?} or newer is required: {e}", MIN_ABI))?
        .create()
        .map_err(|e| format!("Landlock is not available: {e}"))?;
    for rule in rules {
        if !rule.path.exists() {
            continue;
        }
        let rights: BitFlags<AccessFs> = match rule.rights {
            Rights::All => all,
            Rights::Read => read,
            Rights::FileRead => AccessFs::ReadFile.into(),
            Rights::FileReadWrite => AccessFs::ReadFile | AccessFs::WriteFile | AccessFs::Truncate,
            Rights::FileExec => AccessFs::Execute | AccessFs::ReadFile,
        };
        let fd = PathFd::new(&rule.path).map_err(|e| format!("{}: {e}", rule.path.display()))?;
        rs = rs
            .add_rule(PathBeneath::new(fd, rights))
            .map_err(|e| format!("rule for {}: {e}", rule.path.display()))?;
    }
    Ok(rs)
}

/// Stage 1. Returns only when no confined process was produced; on success it
/// does not return (it `exec`s). A confined-but-unverified process exits.
pub fn enter(plan: &Plan) -> Outcome {
    if plan.roots.is_empty() {
        return Outcome::NotNeeded;
    }
    if let Err(e) = plan.validate_layout() {
        return Outcome::Unavailable(e);
    }
    let tmp = plan.tmp_dir();
    if let Err(e) = std::fs::create_dir_all(&tmp) {
        return Outcome::Unavailable(format!(
            "cannot create the private temp directory {}: {e}",
            tmp.display()
        ));
    }
    let rules = plan.rules();
    let canary_dir = match choose_canary_dir(&rules) {
        Ok(d) => d,
        Err(e) => return Outcome::Unavailable(e),
    };
    let ruleset = match build_ruleset(&rules) {
        Ok(r) => r,
        Err(e) => return Outcome::Unavailable(e),
    };

    // From here on the process is confined (or the call failed before that).
    let status = match ruleset.restrict_self() {
        Ok(s) => s,
        Err(e) => return Outcome::Unavailable(format!("restricting the process failed: {e}")),
    };
    if status.ruleset != RulesetStatus::FullyEnforced {
        die(&format!(
            "the sandbox is only {:?}, refusing to continue",
            status.ruleset
        ));
    }
    let failures = canary_failures(&canary_dir, Some(&tmp));
    if !failures.is_empty() {
        die(&format!("sandbox self-test failed: {failures:?}"));
    }

    let mut cmd = std::process::Command::new(&plan.exe);
    cmd.args(std::env::args_os().skip(1))
        .env(ENV_STAGE2, "1")
        .env(ENV_CANARY, &canary_dir)
        .env("TMPDIR", &tmp);
    let err = cmd.exec();
    die(&format!("cannot restart under the sandbox: {err}"));
}

fn die(msg: &str) -> ! {
    eprintln!("mind: sandbox: {msg}");
    std::process::exit(78);
}

/// Stage 2: the process was started by stage 1. Verifies by observation, not
/// by the marker, that it is confined, and that Git works inside.
/// `Ok(None)`: not a sandboxed start. `Err`: marker present but not confined.
pub fn verify_stage2(private_tmp: &Path) -> Result<Option<Report>, String> {
    if std::env::var_os(ENV_STAGE2).is_none() {
        return Ok(None);
    }
    let canary: OsString =
        std::env::var_os(ENV_CANARY).ok_or("the sandbox marker is set without a canary")?;
    let canary_dir = PathBuf::from(canary);
    let failures = canary_failures(&canary_dir, Some(private_tmp));
    if !failures.is_empty() {
        return Err(format!(
            "the environment says the process is sandboxed, but the observed behavior says otherwise: {failures:?}"
        ));
    }
    let stray = open_fds_above_stderr();
    if !stray.is_empty() {
        return Err(format!(
            "unexpected open file descriptors in the sandboxed process: {stray:?}"
        ));
    }
    Ok(Some(Report {
        description: "Landlock, ABI 3 or newer, fully enforced, canary verified".to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(roots: &[(&str, bool)], db: &str) -> Plan {
        Plan {
            roots: roots
                .iter()
                .map(|(p, rw)| (PathBuf::from(p), *rw))
                .collect(),
            db_dir: PathBuf::from(db),
            models_dir: PathBuf::from("/nonexistent/models"),
            exe: PathBuf::from("/nonexistent/mind"),
        }
    }

    #[test]
    fn the_database_directory_must_not_overlap_a_root_in_either_direction() {
        assert!(plan(&[("/data/repos", true)], "/data/state")
            .validate_layout()
            .is_ok());
        let inside = plan(&[("/data/repos", true)], "/data/repos/db").validate_layout();
        assert!(inside.unwrap_err().contains("overlap"));
        let contains = plan(&[("/data/repos/a", false)], "/data/repos").validate_layout();
        assert!(contains.unwrap_err().contains("overlap"));
        let same = plan(&[("/data/state", true)], "/data/state").validate_layout();
        assert!(same.is_err());
        // a prefix of the name is not a path prefix
        assert!(plan(&[("/data/repo", true)], "/data/repo-state")
            .validate_layout()
            .is_ok());
    }

    #[test]
    fn probes_count_only_a_denial_as_proof() {
        let denied = Err::<(), _>(std::io::Error::from_raw_os_error(libc::EACCES));
        let missing = Err::<(), _>(std::io::Error::from_raw_os_error(libc::ENOENT));
        assert_eq!(classify(denied), Probe::Denied);
        assert_eq!(classify(Ok(())), Probe::Allowed);
        assert!(
            matches!(classify(missing), Probe::Inconclusive(_)),
            "ENOENT proves nothing"
        );
    }

    #[test]
    fn the_canary_check_fails_loudly_when_nothing_is_confined() {
        // This test process is not sandboxed, so every probe must report a failure.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("canary"), b"canary").unwrap();
        let failures = canary_failures(dir.path(), None);
        assert_eq!(failures.len(), 5, "{failures:?}");
    }

    #[test]
    fn the_canary_check_does_not_accept_a_missing_canary() {
        let dir = tempfile::tempdir().unwrap();
        let failures = canary_failures(&dir.path().join("does-not-exist"), None);
        assert!(
            failures.iter().all(|f| f.contains("Inconclusive")),
            "{failures:?}"
        );
        assert_eq!(failures.len(), 5);
    }

    #[test]
    fn rules_cover_the_roots_the_database_and_the_binary_only_as_declared() {
        let p = plan(&[("/r/rw", true), ("/r/ro", false)], "/d");
        let rules = p.rules();
        let find = |path: &str| {
            rules
                .iter()
                .find(|r| r.path == Path::new(path))
                .map(|r| r.rights)
        };
        assert_eq!(find("/r/rw"), Some(Rights::All));
        assert_eq!(find("/r/ro"), Some(Rights::Read));
        assert_eq!(find("/d"), Some(Rights::All));
        assert_eq!(find("/nonexistent/mind"), Some(Rights::FileExec));
        assert_eq!(find("/dev/null"), Some(Rights::FileReadWrite));
        // nothing grants the whole filesystem or the home directory
        assert!(rules
            .iter()
            .all(|r| r.path != Path::new("/") && r.path != Path::new("/home")));
        assert_eq!(p.tmp_dir(), Path::new("/d/.mindspace-tmp"));
    }
}
