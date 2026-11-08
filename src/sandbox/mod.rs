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

pub mod seccomp;

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use landlock::{
    Access, AccessFs, BitFlags, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetStatus, Scope, ABI,
};

/// Minimum Landlock ABI. ABI 3 (Linux 6.2) adds truncate control; with ABI 1
/// and 2 initialization succeeds but `truncate()` of an outside file by path
/// stays allowed (measured), so those kernels are refused.
pub const MIN_ABI: ABI = ABI::V3;

/// What the sandbox of `mind mcp` handles, as a declaration for
/// `mind doctor`. `build_ruleset` handles filesystem rights of [`MIN_ABI`] only
/// (no network, no scope, no pathname-socket right) and no seccomp filter is
/// installed. The doctor reports a channel as `enforced` only if it is declared
/// here AND evidence shows the sandbox is active; a kernel ABI number alone never
/// makes a channel "enforced". Whoever adds a right to `build_ruleset` must flip
/// the matching flag here (a unit test pins the filesystem part).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Declared {
    pub filesystem: bool,
    pub tcp: bool,
    pub unix_path_sockets: bool,
    pub unix_abstract_sockets: bool,
    pub signals: bool,
    pub udp: bool,
    pub seccomp: bool,
}

/// `seccomp` is declared: a sandboxed `mind mcp` installs the socket-deny
/// filter (`seccomp.rs`) on supported platforms, and reports honestly where it
/// does not. The socket channels are declared through it, never through Landlock.
pub const DECLARED: Declared = Declared {
    filesystem: true,
    tcp: false,
    unix_path_sockets: false,
    unix_abstract_sockets: false,
    signals: true,
    udp: false,
    seccomp: true,
};

pub(crate) const ENV_STAGE2: &str = "MINDSPACE_SANDBOX_STAGE2";
pub(crate) const ENV_CANARY: &str = "MINDSPACE_SANDBOX_CANARY";

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
    /// The socket-deny seccomp policy as established in this process.
    pub seccomp: seccomp::Status,
    /// Whether signals to processes outside the sandbox are refused.
    pub signal_scope: SignalScope,
}

/// Landlock's signal scope (ABI 6, Linux 6.12): a sandboxed process can then
/// signal only itself, its descendants and processes in its own domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalScope {
    /// Applied, and a process outside the sandbox answered `EPERM` to a signal.
    Enforced,
    /// The kernel offers no signal scope (Landlock below ABI 6); nothing was applied.
    NotAvailable,
    /// Applied, but no outside process was left to ask.
    Unverified,
    /// Not a sandboxed start.
    NotEvaluated,
}

impl SignalScope {
    pub fn label(&self) -> &'static str {
        match self {
            SignalScope::Enforced => "enforced",
            SignalScope::NotAvailable => "not_available",
            SignalScope::Unverified => "unverified",
            SignalScope::NotEvaluated => "not_evaluated",
        }
    }
}

/// Highest Landlock ABI the kernel reports (0: none).
pub fn landlock_abi() -> u32 {
    // SAFETY: the version query takes no pointer and installs nothing.
    let r = unsafe {
        libc::syscall(
            444,
            std::ptr::null::<u8>(),
            0usize,
            1 as libc::c_ulong, // LANDLOCK_CREATE_RULESET_VERSION
        )
    };
    if r < 0 {
        0
    } else {
        r as u32
    }
}

/// The ABI that introduced `Scope::Signal`.
const SIGNAL_SCOPE_ABI: u32 = 6;

pub(crate) const ENV_SIGNAL_CANARY: &str = "MINDSPACE_SANDBOX_SIGNAL_CANARY";

/// A process OUTSIDE the sandbox-to-be, reparented to init (double fork), that
/// sleeps for a while. After the restriction, signalling it must fail with
/// `EPERM`: the observable proof that the scope is in effect. Returns its pid.
fn spawn_signal_canary() -> Option<i32> {
    let server = std::process::id();
    let mut fds = [0i32; 2];
    // SAFETY: pipe/fork/_exit and friends in a single-threaded process (the seccomp
    // installation just verified one thread); the children only use async-signal-safe calls.
    unsafe {
        if libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) != 0 {
            return None;
        }
        let a = libc::fork();
        if a < 0 {
            return None;
        }
        if a == 0 {
            let b = libc::fork();
            if b == 0 {
                // the canary: no inherited stdio (it must not hold the client's pipes open)
                let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
                for fd in 0..3 {
                    libc::dup2(null, fd);
                }
                libc::close(fds[0]);
                libc::close(fds[1]);
                // Lives as long as the server (this pid survives the exec into stage 2),
                // so that nothing is left behind after it; never longer than a minute.
                for _ in 0..1200 {
                    let gone = match std::fs::read_to_string(format!("/proc/{server}/stat")) {
                        Ok(stat) => stat.contains(") Z ") || stat.contains(") X "),
                        Err(_) => true,
                    };
                    if gone {
                        break;
                    }
                    libc::usleep(50_000);
                }
                libc::_exit(0);
            }
            let bytes = (b as i32).to_ne_bytes();
            libc::write(fds[1], bytes.as_ptr() as *const _, 4);
            libc::_exit(0);
        }
        libc::close(fds[1]);
        let mut buf = [0u8; 4];
        let n = libc::read(fds[0], buf.as_mut_ptr() as *mut _, 4);
        libc::close(fds[0]);
        let mut st = 0;
        libc::waitpid(a, &mut st, 0);
        let pid = i32::from_ne_bytes(buf);
        (n == 4 && pid > 0).then_some(pid)
    }
}

/// `EPERM` from signal 0 to `pid` means the signal scope keeps us out; success
/// means we can signal it; `ESRCH` means it is gone (no evidence either way).
fn probe_signal_scope(pid: i32) -> Option<bool> {
    // SAFETY: signal 0 only asks for permission.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return Some(false);
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::EPERM) => Some(true),
        _ => None,
    }
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
    /// Device and inode each root had when it was approved (a grant) or when the
    /// plan was made (a static root). The rule for such a root is created from a
    /// descriptor opened WITHOUT following symlinks and compared with this
    /// identity, so a directory swapped after validation is refused, not trusted.
    pub identities: std::collections::BTreeMap<PathBuf, (u64, u64)>,
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
        for p in ["/usr", "/lib", "/lib64", "/bin", "/sbin", "/sys/fs/cgroup"] {
            r.push(Rule {
                path: PathBuf::from(p),
                rights: Rights::Read,
            });
        }
        // Only this process's own `/proc` entries (`self`, `thread-self`, the pid
        // path Git children use to reach the pinned roots). Not `/proc` as a whole:
        // that would let the process list other processes and read their command
        // lines and status. The kernel already refuses another process's
        // `environ`/`maps` to a different Landlock domain; this closes the rest.
        // If `/proc` does not belong to this pid namespace (`/proc/self` is not our
        // pid), `/proc/<pid>` would name some other process: no rule at all then.
        // There is deliberately no fallback to the whole `/proc`.
        if let Some(own) = own_proc_dir() {
            r.push(Rule {
                path: own,
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
        if let Some(git) = git_binary() {
            r.push(Rule {
                path: git,
                rights: Rights::FileExec,
            });
        }
        r
    }
}

/// `/proc/<pid>` of this process, if the mounted `/proc` is the one of this pid
/// namespace (`/proc/self` resolves to our pid).
fn own_proc_dir() -> Option<PathBuf> {
    let pid = std::process::id().to_string();
    (std::fs::read_link("/proc/self").ok()? == Path::new(&pid))
        .then(|| PathBuf::from(format!("/proc/{pid}")))
}

/// Is this file name a shared library or the dynamic loader (`libc.so.6`,
/// `libz.so.1`, `ld-linux-x86-64.so.2`)? Data files with `.so` in the name, such
/// as the loader cache `ld.so.cache`, are not.
fn is_shared_object_name(name: &str) -> bool {
    (name.starts_with("lib") || name.starts_with("ld-") || name.starts_with("ld64"))
        && (name.ends_with(".so") || name.contains(".so."))
}

/// Top-level directories that are never granted as a library directory, whatever
/// is mapped from them: they hold configuration, devices, state or user data.
const NEVER_A_LIBRARY_DIR: &[&str] = &["/", "/home", "/root"];

/// Nor anything below these (a library mapped from `/dev/shm`, `/tmp` or `/etc`
/// is not a reason to read that tree).
const NEVER_BELOW: &[&str] = &["/etc", "/proc", "/sys", "/dev", "/run", "/tmp", "/var"];

/// The parent directories of the shared libraries named in a `/proc/<pid>/maps`
/// listing (pure, for tests).
fn library_dirs_from_maps(maps: &str) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for line in maps.lines() {
        let Some(path) = line.split_whitespace().nth(5) else {
            continue;
        };
        let p = Path::new(path);
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !path.starts_with('/') || !is_shared_object_name(name) {
            continue;
        }
        if let Some(parent) = p.parent() {
            let exact = NEVER_A_LIBRARY_DIR.iter().any(|d| parent == Path::new(d));
            let below = NEVER_BELOW.iter().any(|d| parent.starts_with(d));
            if !exact && !below {
                dirs.push(parent.to_path_buf());
            }
        }
    }
    dirs
}

/// Directories of shared libraries this process has mapped, and of the Git
/// binary. Under a standard layout they are inside `/usr`; on other layouts
/// (store-based distributions) they are not, and the Git probe shows whether
/// that is enough.
fn extra_runtime_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(maps) = std::fs::read_to_string("/proc/self/maps") {
        dirs.extend(library_dirs_from_maps(&maps));
    }
    if let Some(git) = git_binary() {
        // The binary itself gets a file rule (see `rules`), not its directory: Git
        // under `$HOME/bin` must not make everything in `$HOME/bin` readable.
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

/// The `git` that `Command::new("git")` will run: the first executable file
/// named `git` on `PATH`, symlinks resolved (a rule names the inode it was made
/// for, so the resolved file is what must be granted).
fn git_binary() -> Option<PathBuf> {
    find_in_path("git")
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

/// `open_how` for `openat2(2)`.
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

/// Opens `path` as a directory descriptor (`O_PATH`) with NO symlink anywhere
/// in the path and no magic links, then checks that it is the directory with the
/// expected device and inode. The Landlock rule is made from this descriptor, so
/// what was checked is what is allowed: there is no window between the check and
/// the rule.
pub(crate) fn open_verified_dir(
    path: &Path,
    expected: (u64, u64),
) -> Result<std::os::fd::OwnedFd, String> {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
    const RESOLVE_NO_SYMLINKS: u64 = 0x04;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("{}: path contains NUL", path.display()))?;
    let how = OpenHow {
        flags: (libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS,
    };
    // SAFETY: openat2 with a valid NUL-terminated path and a correctly sized struct.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            libc::AT_FDCWD,
            c.as_ptr(),
            &how as *const OpenHow,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(format!(
            "{}: cannot open without following symlinks: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: the kernel just returned this descriptor; it is owned here.
    let owned = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    let meta = std::fs::File::from(owned.try_clone().map_err(|e| e.to_string())?)
        .metadata()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if (meta.dev(), meta.ino()) != expected {
        return Err(format!(
            "{} is not the directory that was approved (device/inode changed since validation); refusing to create a rule for it",
            path.display()
        ));
    }
    Ok(owned)
}

fn build_ruleset(
    rules: &[Rule],
    identities: &std::collections::BTreeMap<PathBuf, (u64, u64)>,
) -> Result<landlock::RulesetCreated, String> {
    let all: BitFlags<AccessFs> = AccessFs::from_all(MIN_ABI);
    let read: BitFlags<AccessFs> = AccessFs::from_read(MIN_ABI);
    let mut builder = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(all)
        .map_err(|e| format!("Landlock ABI {:?} or newer is required: {e}", MIN_ABI))?;
    // Signals to processes outside the sandbox: only where the kernel has the scope
    // (the ruleset then stays "fully enforced"; below ABI 6 nothing is added).
    if landlock_abi() >= SIGNAL_SCOPE_ABI {
        builder = builder
            .scope(Scope::Signal)
            .map_err(|e| format!("the signal scope: {e}"))?;
    }
    let mut rs = builder
        .create()
        .map_err(|e| format!("Landlock is not available: {e}"))?;
    for rule in rules {
        let strict = identities.get(&rule.path);
        if strict.is_none() && !rule.path.exists() {
            continue;
        }
        let rights: BitFlags<AccessFs> = match rule.rights {
            Rights::All => all,
            Rights::Read => read,
            Rights::FileRead => AccessFs::ReadFile.into(),
            Rights::FileReadWrite => AccessFs::ReadFile | AccessFs::WriteFile | AccessFs::Truncate,
            Rights::FileExec => AccessFs::Execute | AccessFs::ReadFile,
        };
        if let Some(identity) = strict {
            // A root: the rule comes from the verified descriptor itself.
            let fd = open_verified_dir(&rule.path, *identity)?;
            rs = rs
                .add_rule(PathBeneath::new(fd, rights))
                .map_err(|e| format!("rule for {}: {e}", rule.path.display()))?;
            continue;
        }
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
    enter_with_args(plan, &[])
}

/// As [`enter`], and stage 2 is started with `extra_args` appended to this
/// process's arguments (roots that stage 1 resolved, for example granted ones,
/// which stage 2 could no longer read from disk once confined).
pub fn enter_with_args(plan: &Plan, extra_args: &[OsString]) -> Outcome {
    if plan.roots.is_empty() {
        return Outcome::NotNeeded;
    }
    // The socket-deny filter FIRST, before any other work (even the Git child that
    // `plan.rules()` starts to learn Git's exec path): if it is required and cannot
    // be established nothing has been restricted yet, and the repository tools stay
    // off. It is installed on this thread, which is the one that execs below, so
    // stage 2 inherits it. Descriptors were closed at the start of `mind mcp`.
    // (A "not applied" platform is reported once, by stage 2.)
    if let Err(e) = seccomp::apply() {
        return Outcome::Unavailable(format!("seccomp: {e}"));
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
    // An outsider to signal after the restriction (only where the scope applies).
    let signal_canary = if landlock_abi() >= SIGNAL_SCOPE_ABI {
        match spawn_signal_canary() {
            Some(pid) => Some(pid),
            None => return Outcome::Unavailable("cannot create the signal canary process".into()),
        }
    } else {
        None
    };
    let ruleset = match build_ruleset(&rules, &plan.identities) {
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

    if let Some(pid) = signal_canary {
        if probe_signal_scope(pid) != Some(true) {
            die(
                "the signal scope is not in effect: a process outside the sandbox can be signalled",
            );
        }
    }
    let mut cmd = std::process::Command::new(&plan.exe);
    if let Some(pid) = signal_canary {
        cmd.env(ENV_SIGNAL_CANARY, pid.to_string());
    }
    cmd.args(std::env::args_os().skip(1))
        .args(extra_args)
        .env(ENV_STAGE2, "1")
        .env(ENV_CANARY, &canary_dir)
        .env("TMPDIR", &tmp);
    let err = cmd.exec();
    die(&format!("cannot restart under the sandbox: {err}"));
}

// ---------------------------------------------------------------------------
// The extraction worker (M7.3b.2a)
// ---------------------------------------------------------------------------

/// What the extraction worker found out about its own confinement.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct WorkerSandbox {
    pub landlock_abi: u32,
    /// Landlock signal scope active (needs ABI 6); `false` is reported, not hidden.
    pub signal_scope: bool,
    /// `enforced` or the reason it is not.
    pub seccomp: String,
    /// Each probe and what the kernel answered; all of them were as required.
    pub probes: Vec<(String, String)>,
}

/// The rules of a worker: system libraries and the few files the runtime reads, its own `/proc`
/// entry, `/dev/null`. No data directory, no database, no models, no Git, no `/etc`. The data
/// root is added separately, as a descriptor.
fn worker_rules() -> Vec<Rule> {
    let mut r = Vec::new();
    for p in ["/usr", "/lib", "/lib64"] {
        r.push(Rule {
            path: PathBuf::from(p),
            rights: Rights::Read,
        });
    }
    for p in ["/etc/ld.so.cache", "/dev/urandom"] {
        r.push(Rule {
            path: PathBuf::from(p),
            rights: Rights::FileRead,
        });
    }
    r.push(Rule {
        path: PathBuf::from("/dev/null"),
        rights: Rights::FileReadWrite,
    });
    if let Some(own) = own_proc_dir() {
        r.push(Rule {
            path: own,
            rights: Rights::Read,
        });
    }
    if let Ok(maps) = std::fs::read_to_string("/proc/self/maps") {
        for d in library_dirs_from_maps(&maps) {
            if let Ok(c) = std::fs::canonicalize(d) {
                r.push(Rule {
                    path: c,
                    rights: Rights::Read,
                });
            }
        }
    }
    r
}

/// Closes every descriptor above stderr except `keep`.
pub fn close_all_except(keep: &[i32]) {
    for fd in open_fds_above_stderr() {
        if !keep.contains(&fd) {
            // SAFETY: closing a descriptor number that was just listed.
            unsafe { libc::close(fd) };
        }
    }
}

/// Confines the calling (single-threaded) process as an extraction worker: no new privileges, not
/// dumpable, the socket-deny seccomp filter, and a Landlock ruleset that allows [`worker_rules`]
/// and read access (files and directory listing, no execute, no write) beneath `root`, a
/// descriptor the parent verified. Then it checks by observation that the confinement is real. Any
/// failure is an error; the caller must exit. `keep` are the descriptors the worker is meant to
/// hold (channel, root).
pub fn enter_extraction_worker(
    root: &std::os::fd::OwnedFd,
    keep: &[i32],
) -> Result<WorkerSandbox, String> {
    use std::os::fd::AsRawFd;
    // SAFETY: plain prctl calls on this process.
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err("cannot set no_new_privs".into());
        }
    }
    close_all_except(keep);
    let root_raw = root.as_raw_fd();
    let seccomp = match seccomp::apply_allowing(keep)? {
        seccomp::Status::Enforced(_) => "enforced".to_string(),
        seccomp::Status::NotApplied(why) => {
            return Err(format!(
                "the socket filter is not applied: {}",
                why.reason()
            ))
        }
        other => return Err(format!("the socket filter is not enforced: {other:?}")),
    };
    let rs = build_ruleset(&worker_rules(), &Default::default())?
        .add_rule(PathBeneath::new(
            root,
            AccessFs::ReadFile | AccessFs::ReadDir,
        ))
        .map_err(|e| format!("rule for the root descriptor: {e}"))?;
    let status = rs
        .restrict_self()
        .map_err(|e| format!("restricting the process failed: {e}"))?;
    if status.ruleset != RulesetStatus::FullyEnforced {
        return Err(format!("the sandbox is only {:?}", status.ruleset));
    }
    let probes = worker_self_test(root_raw)?;
    Ok(WorkerSandbox {
        landlock_abi: landlock_abi(),
        signal_scope: landlock_abi() >= SIGNAL_SCOPE_ABI,
        seccomp,
        probes,
    })
}

/// Observations after confinement: what must be refused is refused, what the worker needs works.
fn worker_self_test(root_fd: i32) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut fail = Vec::new();
    let mut check = |name: &str, got: Probe, want_denied: bool| {
        let denied = matches!(got, Probe::Denied);
        out.push((name.to_string(), format!("{got:?}")));
        if denied != want_denied {
            fail.push(format!("{name}: {got:?}"));
        }
    };
    let via_root = format!("/proc/{}/fd/{root_fd}", std::process::id());
    check(
        "read /etc/passwd",
        classify(std::fs::read("/etc/passwd")),
        true,
    );
    check(
        "list /home",
        classify(std::fs::read_dir("/home").map(|_| ())),
        true,
    );
    check(
        "list /tmp",
        classify(std::fs::read_dir("/tmp").map(|_| ())),
        true,
    );
    check(
        "write inside the root",
        classify(std::fs::File::create(format!("{via_root}/.mindspace-write-probe")).map(|_| ())),
        true,
    );
    check(
        "climb out of the root",
        classify(std::fs::read_dir(format!("{via_root}/..")).map(|_| ())),
        true,
    );
    // SAFETY: socket creation attempts; the descriptor (if any) is closed at once.
    let sock = |domain: i32| {
        let fd = unsafe { libc::socket(domain, libc::SOCK_STREAM, 0) };
        if fd >= 0 {
            unsafe { libc::close(fd) };
            Probe::Allowed
        } else {
            classify::<()>(Err(std::io::Error::last_os_error()))
        }
    };
    check("socket(AF_INET)", sock(libc::AF_INET), true);
    check("socket(AF_UNIX)", sock(libc::AF_UNIX), true);
    check(
        "list the root",
        classify(std::fs::read_dir(&via_root).map(|_| ())),
        false,
    );
    check(
        "write /dev/null",
        classify(
            std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/null")
                .map(|_| ()),
        ),
        false,
    );
    if fail.is_empty() {
        Ok(out)
    } else {
        Err(format!("the confinement does not hold: {fail:?}"))
    }
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
    // The socket policy is re-derived here (architecture and kernel), not read
    // from an environment variable: where it is required it must demonstrably be active.
    let seccomp = seccomp::verify_stage2()?;
    let signal_scope = if landlock_abi() < SIGNAL_SCOPE_ABI {
        SignalScope::NotAvailable
    } else {
        match std::env::var(ENV_SIGNAL_CANARY)
            .ok()
            .and_then(|v| v.parse::<i32>().ok())
            .and_then(probe_signal_scope)
        {
            Some(true) => SignalScope::Enforced,
            Some(false) => return Err(
                "the signal scope is not in effect: a process outside the sandbox can be signalled"
                    .into(),
            ),
            None => SignalScope::Unverified,
        }
    };
    Ok(Some(Report {
        description: "Landlock, ABI 3 or newer, fully enforced, canary verified".to_string(),
        seccomp,
        signal_scope,
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
            identities: Default::default(),
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
    fn the_declaration_matches_the_rights_the_ruleset_handles() {
        // Filesystem rights of the project minimum only: bits 0..=14 of the
        // kernel's handled_access_fs. Network, scopes and the pathname-socket
        // right (bit 16, a later ABI) are not handled; `DECLARED` says so.
        let bits = AccessFs::from_all(MIN_ABI).bits();
        assert_eq!(bits & !((1u64 << 15) - 1), 0, "a newer right is handled");
        assert_eq!(bits.count_ones(), 15);
        let today = Declared {
            filesystem: true,
            tcp: false,
            unix_path_sockets: false,
            unix_abstract_sockets: false,
            signals: true,
            udp: false,
            seccomp: true,
        };
        assert_eq!(DECLARED, today);
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

    #[test]
    fn a_root_is_opened_without_symlinks_and_must_be_the_approved_directory() {
        use std::os::unix::fs::MetadataExt;
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        let real = base.join("real");
        let other = base.join("other");
        std::fs::create_dir(&real).unwrap();
        std::fs::create_dir(&other).unwrap();
        let id = |p: &Path| {
            let m = std::fs::metadata(p).unwrap();
            (m.dev(), m.ino())
        };
        // the approved directory
        assert!(open_verified_dir(&real, id(&real)).is_ok());
        // another directory at the same path (replaced after approval)
        let approved = id(&real);
        std::fs::remove_dir(&real).unwrap();
        std::fs::create_dir(&real).unwrap();
        let err = open_verified_dir(&real, (approved.0, approved.1 + 1_000_003)).unwrap_err();
        assert!(err.contains("not the directory that was approved"), "{err}");
        // a symlink as the last component, and a symlink in the middle
        let link = base.join("link");
        std::os::unix::fs::symlink(&other, &link).unwrap();
        assert!(open_verified_dir(&link, id(&other)).is_err());
        let via = base.join("via");
        std::os::unix::fs::symlink(&base, &via).unwrap();
        assert!(open_verified_dir(&via.join("other"), id(&other)).is_err());
        // a plain file is not a root
        let f = base.join("f");
        std::fs::write(&f, "x").unwrap();
        assert!(open_verified_dir(&f, id(&f)).is_err());
        // and the ruleset refuses to be built for a swapped root (nothing is restricted)
        let rules = vec![Rule {
            path: real.clone(),
            rights: Rights::All,
        }];
        let mut ids = std::collections::BTreeMap::new();
        ids.insert(real.clone(), (approved.0, approved.1 + 1_000_003));
        assert!(build_ruleset(&rules, &ids).is_err());
        ids.insert(real.clone(), id(&real));
        assert!(build_ruleset(&rules, &ids).is_ok());
        // a root that vanished is an error, not a skipped rule
        let gone = base.join("gone");
        let mut ids = std::collections::BTreeMap::new();
        ids.insert(gone.clone(), (1, 1));
        assert!(build_ruleset(
            &[Rule {
                path: gone,
                rights: Rights::All
            }],
            &ids
        )
        .is_err());
    }

    /// Regression (M7.3b, finding F1): the loader cache `/etc/ld.so.cache` is
    /// mapped into every dynamically linked process and its name contains `.so`;
    /// the runtime directories once included its parent, which made ALL of `/etc`
    /// readable inside the sandbox.
    #[test]
    fn the_runtime_directories_never_include_etc() {
        let dirs = extra_runtime_dirs();
        assert!(
            !dirs.iter().any(|d| d == Path::new("/etc")),
            "all of /etc would be readable: {dirs:?}"
        );
        let plan = plan(&[("/data/repos", true)], "/data/state");
        let wide: Vec<_> = plan
            .rules()
            .into_iter()
            .filter(|r| r.path == Path::new("/etc") || r.path == Path::new("/"))
            .collect();
        assert!(wide.is_empty(), "{wide:?}");
    }

    #[test]
    fn only_libraries_count_and_data_files_with_so_in_the_name_do_not() {
        for ok in [
            "libc.so.6",
            "libz.so.1",
            "libstdc++.so.6",
            "ld-linux-x86-64.so.2",
            "libfoo.so",
            "ld64.so.2",
        ] {
            assert!(is_shared_object_name(ok), "{ok}");
        }
        for no in [
            "ld.so.cache",
            "ld.so.preload",
            "ld.so.conf",
            "libfoo.conf",
            "socket",
            "foo.so",
            "mind",
            "libfoo.sock",
            "xlib.so.1",
        ] {
            assert!(!is_shared_object_name(no), "{no}");
        }
        let maps = "\
7f00 r--p 00000000 00:00 1 /usr/lib/libc.so.6\n\
7f01 r--p 00000000 00:00 2 /etc/ld.so.cache\n\
7f02 r--p 00000000 00:00 3 /opt/rocm/lib/libamdhip64.so.6\n\
7f03 r--p 00000000 00:00 4 /etc/libsomething.so.1\n\
7f04 r--p 00000000 00:00 5 /dev/shm/libx.so.1\n\
7f05 r--p 00000000 00:00 6 [vdso]\n\
7f06 r--p 00000000 00:00 7 /tmp/libtmp.so\n\
7f07 r--p 00000000 00:00 8 /home/u/.cache/libcache.so.1\n";
        let dirs = library_dirs_from_maps(maps);
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/usr/lib"),
                PathBuf::from("/opt/rocm/lib"),
                PathBuf::from("/home/u/.cache"),
            ]
        );
    }

    /// Child half of the kernel test below: applies the product's rules to itself
    /// and reports what the kernel allows. Does nothing in a normal test run.
    #[test]
    fn rules_child() {
        if std::env::var_os("MINDSPACE_RULES_CHILD").is_none() {
            return;
        }
        // the test harness prints "test ... " without a newline first
        println!();
        let plan = plan(&[("/nonexistent-root", true)], "/nonexistent-db");
        let rs = build_ruleset(&plan.rules(), &Default::default()).expect("ruleset");
        let st = rs.restrict_self().expect("restrict");
        assert_eq!(st.ruleset, RulesetStatus::FullyEnforced);
        let probe = |p: &str| match std::fs::read(p) {
            Ok(b) => format!("ok {}", b.len()),
            Err(e) => format!("errno {}", e.raw_os_error().unwrap_or(0)),
        };
        println!("RESULT etc_hostname {}", probe("/etc/hostname"));
        println!("RESULT etc_passwd {}", probe("/etc/passwd"));
        println!("RESULT etc_ld_cache {}", probe("/etc/ld.so.cache"));
        println!("RESULT etc_os_release {}", probe("/etc/os-release"));
        println!(
            "RESULT usr_bin_listing {}",
            match std::fs::read_dir("/usr/bin") {
                Ok(_) => "ok".to_string(),
                Err(e) => format!("errno {}", e.raw_os_error().unwrap_or(0)),
            }
        );
        println!(
            "RESULT dev_null {}",
            match std::fs::OpenOptions::new().write(true).open("/dev/null") {
                Ok(_) => "ok".to_string(),
                Err(e) => format!("errno {}", e.raw_os_error().unwrap_or(0)),
            }
        );
    }

    /// Child half of the `/proc` kernel test: prints what the process can read in
    /// `/proc` before and after the product's rules. `MINDSPACE_PROC_FOREIGN` is
    /// the pid of a process of the same user that the parent started.
    #[test]
    fn proc_child() {
        let Some(foreign) = std::env::var_os("MINDSPACE_PROC_FOREIGN") else {
            return;
        };
        let foreign = foreign.to_string_lossy().into_owned();
        println!();
        let own_file = std::fs::File::open("/dev/null").unwrap();
        let own_fd = std::os::fd::AsRawFd::as_raw_fd(&own_file);
        let probe_all = |tag: &str| {
            let me = std::process::id();
            let ppid = unsafe { libc::getppid() };
            let read = |p: String| match std::fs::read(&p) {
                Ok(b) => format!("ok {}", b.len().min(1)),
                Err(e) => format!("errno {}", e.raw_os_error().unwrap_or(0)),
            };
            let list =
                |p: String| match std::fs::read_dir(&p).and_then(|mut d| d.next().transpose()) {
                    Ok(Some(_)) => "ok".to_string(),
                    Ok(None) => "empty".to_string(),
                    Err(e) => format!("errno {}", e.raw_os_error().unwrap_or(0)),
                };
            let link = |p: String| match std::fs::read_link(&p) {
                Ok(_) => "ok".to_string(),
                Err(e) => format!("errno {}", e.raw_os_error().unwrap_or(0)),
            };
            let thread = std::thread::spawn(|| match std::fs::read("/proc/thread-self/status") {
                Ok(_) => "ok".to_string(),
                Err(e) => format!("errno {}", e.raw_os_error().unwrap_or(0)),
            })
            .join()
            .unwrap();
            let rows: Vec<(&str, String)> = vec![
                ("self_status", read("/proc/self/status".into())),
                ("self_maps", read("/proc/self/maps".into())),
                ("self_cgroup", read("/proc/self/cgroup".into())),
                ("self_fd_list", list("/proc/self/fd".into())),
                ("self_fd_link", link(format!("/proc/self/fd/{own_fd}"))),
                ("pid_fd_link", link(format!("/proc/{me}/fd/{own_fd}"))),
                ("pid_fd_open", read(format!("/proc/{me}/fd/{own_fd}"))),
                ("thread_self", thread),
                ("foreign_environ", read(format!("/proc/{foreign}/environ"))),
                ("foreign_cmdline", read(format!("/proc/{foreign}/cmdline"))),
                ("foreign_fd_list", list(format!("/proc/{foreign}/fd"))),
                ("foreign_maps", read(format!("/proc/{foreign}/maps"))),
                ("parent_environ", read(format!("/proc/{ppid}/environ"))),
                ("init_status", read("/proc/1/status".into())),
                ("proc_listing", list("/proc".into())),
            ];
            for (k, v) in rows {
                println!("RESULT {tag}_{k} {v}");
            }
        };
        probe_all("before");
        let plan = plan(&[("/nonexistent-root", true)], "/nonexistent-db");
        let rs = build_ruleset(&plan.rules(), &Default::default()).expect("ruleset");
        let st = rs.restrict_self().expect("restrict");
        assert_eq!(st.ruleset, RulesetStatus::FullyEnforced);
        probe_all("after");
    }

    /// The kernel's answer to the `/proc` audit: after the rules, the process still
    /// reads its own `/proc` entries, but no other process's environment, command
    /// line, descriptors or maps, and cannot list `/proc`.
    #[test]
    fn the_kernel_hides_other_processes_from_a_sandboxed_process() {
        let mut other = std::process::Command::new("sleep")
            .arg("30")
            .env("MINDSPACE_SECRET_PROBE", "x")
            .spawn()
            .unwrap();
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sandbox::tests::proc_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("MINDSPACE_PROC_FOREIGN", other.id().to_string())
            .output()
            .unwrap();
        let _ = other.kill();
        let _ = other.wait();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "{text}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        eprintln!("{text}");
        let get = |k: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(&format!("RESULT {k} ")))
                .unwrap_or_else(|| panic!("no {k} in {text}"))
                .to_string()
        };
        // the probes are meaningful: before the rules everything is readable
        for k in [
            "foreign_environ",
            "foreign_cmdline",
            "foreign_fd_list",
            "parent_environ",
            "init_status",
            "proc_listing",
        ] {
            assert!(get(&format!("before_{k}")).starts_with("ok"), "before {k}");
        }
        // what the product needs stays available
        for k in [
            "self_status",
            "self_maps",
            "self_cgroup",
            "self_fd_list",
            "self_fd_link",
            "pid_fd_link",
            "pid_fd_open",
            "thread_self",
        ] {
            assert!(get(&format!("after_{k}")).starts_with("ok"), "after {k}");
        }
        // other processes are closed
        for k in [
            "foreign_environ",
            "foreign_cmdline",
            "foreign_fd_list",
            "foreign_maps",
            "parent_environ",
            "init_status",
            "proc_listing",
        ] {
            assert_eq!(get(&format!("after_{k}")), "errno 13", "after {k}");
        }
    }

    /// Child half of the Git binary rule test. Layout (all under `MINDSPACE_GIT_HOME`):
    /// `bin/git` (a copy of the real Git) next to `bin/secret.txt`, `real/git` plus
    /// `link/git -> ../real/git` next to `link/secret.txt`, `repo/` (a rw root).
    /// `MINDSPACE_GIT_CASE` is `copy`, `symlink`, or `replaced` (the file is renamed
    /// over after the rules were built).
    #[test]
    fn git_rules_child() {
        let (Some(home), Some(case)) = (
            std::env::var_os("MINDSPACE_GIT_HOME"),
            std::env::var_os("MINDSPACE_GIT_CASE"),
        ) else {
            return;
        };
        let (home, case) = (PathBuf::from(home), case.to_string_lossy().into_owned());
        println!();
        let bindir = if case == "symlink" { "link" } else { "bin" };
        let path = format!("{}:/nonexistent", home.join(bindir).display());
        std::env::set_var("PATH", &path);
        let repo = home.join("repo");
        let plan = plan(&[(repo.to_str().unwrap(), true)], "/nonexistent-db");
        let rs = build_ruleset(&plan.rules(), &Default::default()).expect("ruleset");
        if case == "replaced" {
            let b = home.join("bin");
            std::fs::copy(b.join("git"), b.join("git.new")).unwrap();
            std::fs::rename(b.join("git.new"), b.join("git")).unwrap();
        }
        let st = rs.restrict_self().expect("restrict");
        assert_eq!(st.ruleset, RulesetStatus::FullyEnforced);
        fn res<T>(r: std::io::Result<T>) -> String {
            match r {
                Ok(_) => "ok".to_string(),
                Err(e) => format!("errno {}", e.raw_os_error().unwrap_or(0)),
            }
        }
        println!(
            "RESULT secret {}",
            res(std::fs::read(home.join(bindir).join("secret.txt")))
        );
        println!(
            "RESULT listing {}",
            res(std::fs::read_dir(home.join(bindir)).map(|_| ()))
        );
        println!(
            "RESULT home_file {}",
            res(std::fs::read(home.join("other.txt")))
        );
        let git = |args: &[&str]| {
            let o = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .env("HOME", "/nonexistent")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output();
            match o {
                Ok(o) if o.status.success() => "ok".to_string(),
                Ok(o) => format!(
                    "exit {} {}",
                    o.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&o.stderr).trim().replace('\n', " ")
                ),
                Err(e) => format!("errno {}", e.raw_os_error().unwrap_or(0)),
            }
        };
        println!("RESULT git_version {}", git(&["--version"]));
        println!("RESULT git_init {}", git(&["init", "-q", "."]));
        println!("RESULT git_refs {}", git(&["for-each-ref"]));
        println!("RESULT git_exec_path {}", git(&["--exec-path"]));
    }

    /// Git outside `/usr`: the binary is granted by itself, not through its directory.
    #[test]
    fn the_kernel_grants_a_git_under_home_only_the_binary() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let Some(real_git) = find_in_path("git") else {
            return;
        };
        let run = |case: &str| -> String {
            let h = tempfile::tempdir().unwrap();
            let home = h.path();
            for d in ["bin", "real", "link", "repo"] {
                std::fs::create_dir(home.join(d)).unwrap();
            }
            for (d, f) in [("bin", "git"), ("real", "git")] {
                let t = home.join(d).join(f);
                std::fs::copy(&real_git, &t).unwrap();
                std::fs::set_permissions(&t, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            symlink("../real/git", home.join("link/git")).unwrap();
            for f in ["bin/secret.txt", "link/secret.txt", "other.txt"] {
                std::fs::write(home.join(f), "SECRET").unwrap();
            }
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sandbox::tests::git_rules_child",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("MINDSPACE_GIT_HOME", home)
                .env("MINDSPACE_GIT_CASE", case)
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            assert!(
                out.status.success(),
                "{text}{}",
                String::from_utf8_lossy(&out.stderr)
            );
            eprintln!("--- {case}\n{text}");
            text
        };
        let get = |text: &str, k: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(&format!("RESULT {k} ")))
                .unwrap_or_else(|| panic!("no {k} in {text}"))
                .to_string()
        };
        for case in ["copy", "symlink"] {
            let t = run(case);
            assert_eq!(
                get(&t, "secret"),
                "errno 13",
                "{case}: sibling file readable"
            );
            assert_eq!(get(&t, "listing"), "errno 13", "{case}: directory listable");
            assert_eq!(get(&t, "home_file"), "errno 13", "{case}");
            for k in ["git_version", "git_init", "git_refs", "git_exec_path"] {
                assert_eq!(get(&t, k), "ok", "{case}: {k}");
            }
        }
        // a replaced executable is a different inode: fail closed, not a silent regrant
        let t = run("replaced");
        assert_eq!(get(&t, "secret"), "errno 13");
        assert_eq!(get(&t, "git_version"), "errno 13", "{t}");
    }

    /// The kernel's answer to F1: with the product's rules, /etc is not readable
    /// (except the loader files that have their own rules); libraries and /dev/null still work.
    #[test]
    fn the_kernel_does_not_let_a_sandboxed_process_read_etc() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sandbox::tests::rules_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("MINDSPACE_RULES_CHILD", "1")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "{text}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let get = |k: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(&format!("RESULT {k} ")))
                .unwrap_or_else(|| panic!("no {k} in {text}"))
                .to_string()
        };
        eprintln!("{text}");
        // (/etc/os-release is a symlink into /usr/lib and stays readable through its target)
        for (key, path) in [
            ("etc_hostname", "/etc/hostname"),
            ("etc_passwd", "/etc/passwd"),
        ] {
            if Path::new(path).exists() {
                assert_eq!(get(key), "errno 13", "{path}");
            }
        }
        assert!(
            get("etc_ld_cache").starts_with("ok"),
            "the loader cache keeps its own rule"
        );
        assert_eq!(get("usr_bin_listing"), "ok");
        assert_eq!(get("dev_null"), "ok");
    }
}
