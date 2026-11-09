//! The broker (M7.3b.3): the process OUTSIDE the frontend's Landlock domain that starts, observes
//! and ends extraction workers. Design: `docs/m7.3b-broker-architecture.md`.
//!
//! What it does for one operation, in this order:
//!
//! 1. reads one strict `Begin` from the frontend (operation id, operation type, root, spaces);
//! 2. derives the authority from TRUSTED state ([`RootAuthority`]: static roots or the grant
//!    store, re-read for every operation) and refuses any root that is not exactly one of them;
//! 3. opens that root itself with `openat2` (no symlinks), checks device/inode against the
//!    authority's pinned identity and keeps the descriptor (the frontend never supplies one);
//! 4. creates a cgroup for the operation (fail closed unless the experimental mode is chosen),
//!    spawns the worker into it with exactly three descriptors (channel 3, root 4, control 5);
//! 5. waits for the worker's `READY` (sent after it confined itself), observes it from outside
//!    (`/proc/<pid>/status`, `/proc/<pid>/cgroup`) and only then sends `GO`;
//! 6. hands the frontend the worker's channel (SCM_RIGHTS) together with what it observed;
//! 7. supervises: wall clock, `Cancel`, frontend hang-up; on any end it kills the whole cgroup
//!    (`cgroup.kill`, which also reaches descendants that left the process group), reaps the
//!    leader and reports.
//!
//! The broker never touches the database, never parses repository content and never relays the
//! record stream: the frontend reads the worker's channel directly and validates it.

use std::collections::{HashSet, VecDeque};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::core::Uuid;

use super::context::validate_spaces;
use super::{CHANNEL_FD, CONTROL_FD, ROOT_FD};

pub const BROKER_PROTOCOL: u32 = 1;
/// Every broker line is at most this long (newline included).
pub const MAX_LINE: usize = 8 * 1024;
const MAX_ROOT_BYTES: usize = 4096;
/// Operation ids remembered for replay detection (oldest forgotten first).
const SEEN_OPS: usize = 4096;

// ------------------------------------------------------------------ authority

/// Where a root's authorization comes from. Informational for audit; the broker derives it, the
/// frontend can neither name nor choose it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum AuthoritySource {
    /// A root fixed at start (`--allow-root`).
    Static,
    /// An active grant of the store (the grant id, a label only).
    Grant(String),
}

/// One root the broker may serve, with the identity that was approved.
#[derive(Debug, Clone)]
pub struct AuthorizedRoot {
    pub source: AuthoritySource,
    pub path: PathBuf,
    pub dev: u64,
    pub ino: u64,
}

/// Trusted state, consulted afresh for every operation (so a revoked or expired grant is refused
/// at once). Never fed by the frontend.
pub trait RootAuthority: Send + Sync {
    fn roots(&self) -> Result<Vec<AuthorizedRoot>, String>;
}

/// Roots fixed at construction; their device/inode are pinned then.
pub struct StaticRoots(Vec<AuthorizedRoot>);

impl StaticRoots {
    pub fn new(paths: &[PathBuf]) -> Result<Self, String> {
        use std::os::unix::fs::MetadataExt;
        let mut v = Vec::new();
        for p in paths {
            let c = std::fs::canonicalize(p).map_err(|e| format!("{}: {e}", p.display()))?;
            let m = std::fs::metadata(&c).map_err(|e| format!("{}: {e}", c.display()))?;
            if !m.is_dir() {
                return Err(format!("{} is not a directory", c.display()));
            }
            v.push(AuthorizedRoot {
                source: AuthoritySource::Static,
                path: c,
                dev: m.dev(),
                ino: m.ino(),
            });
        }
        Ok(Self(v))
    }
}

impl RootAuthority for StaticRoots {
    fn roots(&self) -> Result<Vec<AuthorizedRoot>, String> {
        Ok(self.0.clone())
    }
}

/// Active grants of the store, re-validated now (path policy, device/inode as approved).
pub struct GrantRoots {
    pub store: crate::grants::Store,
    pub policy: crate::grants::PathPolicy,
}

impl RootAuthority for GrantRoots {
    fn roots(&self) -> Result<Vec<AuthorizedRoot>, String> {
        let now = time::OffsetDateTime::now_utc();
        let res = crate::grants::resolve_for_start(&self.store, &self.policy, now);
        if let Some(e) = res.store_error {
            return Err(format!("the grant store cannot be verified: {e}"));
        }
        let state = self.store.load().map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for (path, _access) in &res.roots {
            let (dev, ino) = res.identities.get(path).copied().unwrap_or_default();
            let id = state
                .records
                .iter()
                .find(|r| &r.grant.path == path && res.applied.contains(&r.grant.id))
                .map(|r| r.grant.id.clone())
                .unwrap_or_default();
            out.push(AuthorizedRoot {
                source: AuthoritySource::Grant(id),
                path: path.clone(),
                dev,
                ino,
            });
        }
        Ok(out)
    }
}

// ------------------------------------------------------------------ protocol

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    DirectoryExtract,
}

/// Frontend to broker. The vocabulary is closed: there is no grant id, access mode, ttl, limit,
/// descriptor or "approved" field to send.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToBroker {
    Begin {
        v: u32,
        op: Uuid,
        kind: OpKind,
        root: String,
        spaces: Vec<Uuid>,
    },
    /// The frontend validated the stream; let the worker finish and report.
    Done,
    Cancel,
}

/// What the broker saw of the worker from outside.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Observed {
    pub pid: u32,
    pub seccomp: String,
    pub no_new_privs: String,
    pub tracer_pid: String,
    pub cap_eff: String,
    /// The worker's cgroup as the kernel reports it (`None`: cleanup is process-group only).
    pub cgroup: Option<String>,
    /// How descendants are ended: see [`Cleanup`].
    pub cleanup: Cleanup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cleanup {
    /// `cgroup.kill` on the operation's cgroup: reaches descendants that left the process group.
    Cgroup,
    /// EXPERIMENTAL, explicitly chosen: SIGKILL to the process group. A descendant that calls
    /// `setsid` survives.
    ProcessGroupOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromBroker {
    /// Carries the worker's channel as an SCM_RIGHTS descriptor.
    Ready {
        v: u32,
        op: Uuid,
        spaces: Vec<Uuid>,
        root: String,
        authority: AuthoritySource,
        observed: Observed,
    },
    Refused {
        reason: String,
    },
    Finished {
        op: Uuid,
        exit: String,
        /// The leader ended on its own before the broker ended it.
        exited_by_itself: bool,
        /// Processes still in the operation's cgroup when it was killed (descendants).
        descendants_killed: u32,
        peak_rss_kb: i64,
        /// `memory.peak` of the operation's cgroup, when the memory controller is available.
        cgroup_peak_bytes: Option<u64>,
        cleanup: Cleanup,
    },
}

// ------------------------------------------------------------------ descriptors over the socket

fn send_with_fd(sock: &UnixStream, data: &[u8], fd: RawFd) -> std::io::Result<()> {
    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut _,
        iov_len: data.len(),
    };
    // SAFETY: a well-formed msghdr with one iovec and one SCM_RIGHTS control message.
    unsafe {
        let space = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as usize;
        let mut ctl = vec![0u8; space];
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = ctl.as_mut_ptr() as *mut _;
        msg.msg_controllen = space as _;
        let c = libc::CMSG_FIRSTHDR(&msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
        std::ptr::copy_nonoverlapping(
            &fd as *const RawFd as *const u8,
            libc::CMSG_DATA(c),
            std::mem::size_of::<RawFd>(),
        );
        if libc::sendmsg(sock.as_raw_fd(), &msg, libc::MSG_NOSIGNAL) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Receives one message and at most one descriptor. A message with more than one descriptor, or
/// one that was truncated, is an error (every extra descriptor is closed).
pub fn recv_with_fd(sock: &UnixStream) -> std::io::Result<(Vec<u8>, Option<OwnedFd>)> {
    let mut buf = vec![0u8; MAX_LINE];
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut _,
        iov_len: buf.len(),
    };
    // SAFETY: a well-formed msghdr; descriptors received are taken into OwnedFd immediately.
    unsafe {
        let space = libc::CMSG_SPACE((4 * std::mem::size_of::<RawFd>()) as u32) as usize;
        let mut ctl = vec![0u8; space];
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = ctl.as_mut_ptr() as *mut _;
        msg.msg_controllen = space as _;
        let n = libc::recvmsg(sock.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC);
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut fds: Vec<OwnedFd> = Vec::new();
        let mut c = libc::CMSG_FIRSTHDR(&msg);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let bytes = (*c).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                let data = libc::CMSG_DATA(c) as *const RawFd;
                for i in 0..bytes / std::mem::size_of::<RawFd>() {
                    fds.push(OwnedFd::from_raw_fd(std::ptr::read_unaligned(data.add(i))));
                }
            }
            c = libc::CMSG_NXTHDR(&msg, c);
        }
        buf.truncate(n as usize);
        if msg.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 || fds.len() > 1 {
            return Err(std::io::Error::other("truncated or too many descriptors"));
        }
        Ok((buf, fds.pop()))
    }
}

// ------------------------------------------------------------------ cgroups

/// One cgroup subtree per broker: `<parent>/mindspace-<pid>/{supervisor, op-<id>...}`. The broker
/// lives in `supervisor` (a cgroup with processes cannot delegate controllers), the operations
/// below `base`. Restores the broker's original cgroup and removes the tree on drop.
pub struct CgroupScope {
    base: PathBuf,
    original: PathBuf,
    /// Controllers available for the operations (`memory`, `pids`).
    pub controllers: Vec<String>,
}

struct ScopeState {
    refs: usize,
    scope: Option<Arc<CgroupScope>>,
}

/// The process-wide scope with an explicit count: the last handle tears it down while this lock
/// is held, so a new scope can never be created inside one that is going away.
static SCOPE: Mutex<ScopeState> = Mutex::new(ScopeState {
    refs: 0,
    scope: None,
});

/// A counted reference to the process-wide [`CgroupScope`].
pub struct ScopeHandle(Arc<CgroupScope>);

impl std::ops::Deref for ScopeHandle {
    type Target = CgroupScope;
    fn deref(&self) -> &CgroupScope {
        &self.0
    }
}

impl ScopeHandle {
    /// The scope (created by the first caller, shared by the others, removed with the last). An
    /// error means cgroup-based cleanup is unavailable in this environment.
    pub fn acquire() -> Result<Self, String> {
        let mut st = SCOPE.lock().map_err(|_| "cgroup scope lock poisoned")?;
        if let Some(s) = &st.scope {
            let s = s.clone();
            st.refs += 1;
            return Ok(Self(s));
        }
        let s = Arc::new(CgroupScope::create()?);
        st.scope = Some(s.clone());
        st.refs = 1;
        Ok(Self(s))
    }
}

impl Clone for ScopeHandle {
    fn clone(&self) -> Self {
        if let Ok(mut st) = SCOPE.lock() {
            st.refs += 1;
        }
        Self(self.0.clone())
    }
}

impl Drop for ScopeHandle {
    fn drop(&mut self) {
        let Ok(mut st) = SCOPE.lock() else {
            return;
        };
        st.refs = st.refs.saturating_sub(1);
        if st.refs == 0 {
            if let Some(s) = st.scope.take() {
                s.teardown();
            }
        }
    }
}

fn own_cgroup() -> Result<PathBuf, String> {
    let text = std::fs::read_to_string("/proc/self/cgroup").map_err(|e| e.to_string())?;
    let rel = text
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .ok_or("not a cgroup v2 system (no unified hierarchy entry)")?;
    Ok(PathBuf::from("/sys/fs/cgroup").join(rel.trim_start_matches('/')))
}

fn write_file(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

impl CgroupScope {
    /// Tries the broker's own cgroup as the parent first. A cgroup that holds processes cannot
    /// delegate controllers, so when that yields none (typical for a terminal's scope) the parent's
    /// directory is tried, where the broker's tree sits beside its original cgroup. If neither
    /// has controllers the scope still gives `cgroup.kill`; limits that need a controller are then
    /// refused per operation.
    fn create() -> Result<Self, String> {
        let original = own_cgroup()?;
        if !original.join("cgroup.kill").exists() {
            return Err("cgroup.kill is not available (kernel older than 5.14?)".into());
        }
        let mut candidates = vec![original.clone()];
        if let Some(p) = original.parent() {
            if p.starts_with("/sys/fs/cgroup") && p != Path::new("/sys/fs/cgroup") {
                candidates.push(p.to_path_buf());
            }
        }
        // Strictly one at a time: the process lives in at most one `supervisor`. A candidate
        // without controllers is torn down (the process goes home) before the next is tried;
        // if none has controllers the first one that works is created again and kept.
        let mut first_error = None;
        let mut first_ok: Option<PathBuf> = None;
        for parent in &candidates {
            match Self::create_in(parent, &original) {
                Ok(s) if !s.controllers.is_empty() => return Ok(s),
                Ok(s) => {
                    s.teardown();
                    first_ok.get_or_insert_with(|| parent.clone());
                }
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        match first_ok {
            Some(parent) => Self::create_in(&parent, &original),
            None => Err(first_error.unwrap_or_else(|| "no usable cgroup".into())),
        }
    }

    /// Removes the empty trees of brokers that no longer exist (killed before they could clean
    /// up). Only directories named `mindspace-<pid>[-n]` whose pid is gone, and only empty ones.
    fn sweep(parent: &Path) {
        fn remove_tree(dir: &Path) {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    if e.path().is_dir() {
                        remove_tree(&e.path());
                    }
                }
            }
            let _ = std::fs::remove_dir(dir);
        }
        let Ok(rd) = std::fs::read_dir(parent) else {
            return;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(rest) = name.strip_prefix("mindspace-") else {
                continue;
            };
            let pid: Option<u32> = rest.split('-').next().and_then(|p| p.parse().ok());
            if let Some(pid) = pid {
                if pid != std::process::id() && !Path::new(&format!("/proc/{pid}")).exists() {
                    remove_tree(&e.path());
                }
            }
        }
    }

    fn create_in(parent: &Path, original: &Path) -> Result<Self, String> {
        Self::sweep(parent);
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let base = parent.join(format!(
            "mindspace-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&base).map_err(|e| {
            format!(
                "cannot create a cgroup below {} (no delegation?): {e}",
                parent.display()
            )
        })?;
        let undo = |m: String| {
            let _ = std::fs::remove_dir(base.join("supervisor"));
            let _ = std::fs::remove_dir(&base);
            m
        };
        std::fs::create_dir(base.join("supervisor")).map_err(|e| undo(e.to_string()))?;
        write_file(
            &base.join("supervisor/cgroup.procs"),
            &std::process::id().to_string(),
        )
        .map_err(&undo)?;
        let available: Vec<String> = std::fs::read_to_string(base.join("cgroup.controllers"))
            .unwrap_or_default()
            .split_whitespace()
            .filter(|c| matches!(*c, "memory" | "pids"))
            .map(str::to_string)
            .collect();
        let mut controllers = Vec::new();
        for c in &available {
            if write_file(&base.join("cgroup.subtree_control"), &format!("+{c}")).is_ok() {
                controllers.push(c.clone());
            }
        }
        Ok(Self {
            base,
            original: original.to_path_buf(),
            controllers,
        })
    }

    fn has(&self, c: &str) -> bool {
        self.controllers.iter().any(|x| x == c)
    }
}

impl CgroupScope {
    /// Leaves and removes the tree. The caller holds the `SCOPE` lock (or is `create`, which does).
    fn teardown(&self) {
        // Leave first. If the process cannot be moved back it is still inside `supervisor`, and
        // nothing below may be touched (killing or removing it would kill this process).
        if write_file(
            &self.original.join("cgroup.procs"),
            &std::process::id().to_string(),
        )
        .is_err()
        {
            return;
        }
        if let Ok(rd) = std::fs::read_dir(&self.base) {
            for e in rd.flatten() {
                let is_op = e.file_name().to_string_lossy().starts_with("op-");
                if is_op && e.path().is_dir() {
                    let _ = std::fs::write(e.path().join("cgroup.kill"), "1");
                    let _ = std::fs::remove_dir(e.path());
                }
            }
        }
        let _ = std::fs::remove_dir(self.base.join("supervisor"));
        let _ = std::fs::remove_dir(&self.base);
    }
}

/// The cgroup of one operation.
struct OpCgroup {
    dir: PathBuf,
    scope: ScopeHandle,
}

impl OpCgroup {
    fn create(
        scope: &ScopeHandle,
        op: Uuid,
        memory_max: Option<u64>,
        pids_max: Option<u64>,
    ) -> Result<Self, String> {
        // a limit that was asked for but cannot be enforced is an error, never a silent omission
        if memory_max.is_some() && !scope.has("memory") {
            return Err(
                "a memory limit was requested but the memory controller is unavailable".into(),
            );
        }
        if pids_max.is_some() && !scope.has("pids") {
            return Err(
                "a process limit was requested but the pids controller is unavailable".into(),
            );
        }
        let dir = scope.base.join(format!("op-{op}"));
        std::fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let cg = Self {
            dir,
            scope: scope.clone(),
        };
        if let Some(m) = memory_max {
            write_file(&cg.dir.join("memory.max"), &m.to_string())?;
            let _ = write_file(&cg.dir.join("memory.swap.max"), "0");
            let _ = write_file(&cg.dir.join("memory.oom.group"), "1");
        }
        if let Some(p) = pids_max {
            write_file(&cg.dir.join("pids.max"), &p.to_string())?;
        }
        Ok(cg)
    }

    fn procs(&self) -> Vec<u32> {
        std::fs::read_to_string(self.dir.join("cgroup.procs"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    }

    /// Kills every process in the cgroup (descendants in other sessions and groups included) and
    /// waits until none is left. Returns how many were found just before.
    fn kill(&self) -> u32 {
        let found = self.procs().len() as u32;
        let _ = std::fs::write(self.dir.join("cgroup.kill"), "1");
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            let ev = std::fs::read_to_string(self.dir.join("cgroup.events")).unwrap_or_default();
            if ev.lines().any(|l| l.trim() == "populated 0") {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        found
    }

    fn peak(&self) -> Option<u64> {
        if !self.scope.has("memory") {
            return None;
        }
        std::fs::read_to_string(self.dir.join("memory.peak"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }
}

impl Drop for OpCgroup {
    fn drop(&mut self) {
        self.kill();
        let _ = std::fs::remove_dir(&self.dir);
    }
}

// ------------------------------------------------------------------ the broker

#[derive(Debug, Clone)]
pub enum CleanupPolicy {
    /// Required: an operation whose cgroup cannot be set up is refused.
    Cgroup {
        memory_max: Option<u64>,
        pids_max: Option<u64>,
    },
    /// EXPERIMENTAL and explicit: no cgroup; the process group is killed, escaped descendants are
    /// NOT. The result says so ([`Cleanup::ProcessGroupOnly`]).
    UnreliableProcessGroupOnly,
}

#[derive(Debug, Clone)]
pub struct BrokerConfig {
    /// The worker binary and its arguments (`mind extraction-worker`).
    pub exe: PathBuf,
    pub args: Vec<OsString>,
    /// Hard bound for one operation, start-up included. Set by the broker, not the frontend.
    pub wall_timeout: Duration,
    /// Spawn until `READY`.
    pub startup_timeout: Duration,
    /// How long a worker that was told `Done` may take to exit before it is killed.
    pub exit_grace: Duration,
    /// Per-process address space (`RLIMIT_AS`, MiB). Experimental: no production value exists.
    pub rlimit_as_mib: Option<u64>,
    /// Per-process CPU seconds (`RLIMIT_CPU`).
    pub rlimit_cpu_secs: Option<u64>,
    pub cleanup: CleanupPolicy,
    /// Refuse a worker whose `/proc` entry does not show seccomp, no-new-privs, no tracer and no
    /// capabilities. Only tests that stand in a fake worker turn this off.
    pub verify_confinement: bool,
    /// Operations at once across all connections; more are refused (`busy`), not queued.
    pub max_concurrent: usize,
}

impl BrokerConfig {
    pub fn new(exe: impl Into<PathBuf>) -> Self {
        Self {
            exe: exe.into(),
            args: vec![OsString::from("extraction-worker")],
            wall_timeout: Duration::from_secs(300),
            startup_timeout: Duration::from_secs(10),
            exit_grace: Duration::from_secs(5),
            rlimit_as_mib: None,
            rlimit_cpu_secs: None,
            cleanup: CleanupPolicy::Cgroup {
                memory_max: None,
                pids_max: None,
            },
            verify_confinement: true,
            max_concurrent: 1,
        }
    }
}

pub struct Broker {
    authority: Box<dyn RootAuthority>,
    cfg: BrokerConfig,
    scope: Option<ScopeHandle>,
    active: AtomicUsize,
    seen: Mutex<(HashSet<Uuid>, VecDeque<Uuid>)>,
}

/// Released when an operation ends, whatever the way.
struct Permit<'a>(&'a AtomicUsize);
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn describe(status: i32) -> String {
    if libc::WIFEXITED(status) {
        format!("exit code {}", libc::WEXITSTATUS(status))
    } else if libc::WIFSIGNALED(status) {
        format!("killed by signal {}", libc::WTERMSIG(status))
    } else {
        format!("status {status}")
    }
}

fn status_field(pid: u32, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix(key))
        .map(|v| v.trim().to_string())
}

/// Opens a directory through a verified descriptor of it (no path is resolved again): a normal
/// read-only descriptor for the worker's Landlock rule and anchored reads.
fn reopen_dir(pinned: &OwnedFd) -> std::io::Result<OwnedFd> {
    let dot = std::ffi::CString::new(".").unwrap_or_default();
    // SAFETY: openat on a descriptor we own, relative path ".".
    let fd = unsafe {
        libc::openat(
            pinned.as_raw_fd(),
            dot.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: freshly returned by the kernel.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn real_path_of_fd(fd: &OwnedFd) -> std::io::Result<PathBuf> {
    std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd()))
}

fn read_line_limited(r: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut line = Vec::new();
    match r.take(MAX_LINE as u64 + 1).read_until(b'\n', &mut line) {
        Ok(0) => Ok(None),
        Ok(_) if line.last() == Some(&b'\n') => {
            line.pop();
            String::from_utf8(line)
                .map(Some)
                .map_err(|_| "a line is not UTF-8".into())
        }
        Ok(_) if line.len() > MAX_LINE => Err("a line is too long".into()),
        Ok(_) => Err("a line is truncated".into()),
        Err(e) => Err(e.to_string()),
    }
}

impl Broker {
    /// With `CleanupPolicy::Cgroup` the cgroup scope is set up now; if the environment cannot
    /// provide it this is an error (the broker does not start with a weaker guarantee).
    pub fn new(authority: Box<dyn RootAuthority>, cfg: BrokerConfig) -> Result<Self, String> {
        let scope = match &cfg.cleanup {
            CleanupPolicy::Cgroup { .. } => Some(ScopeHandle::acquire()?),
            CleanupPolicy::UnreliableProcessGroupOnly => None,
        };
        Ok(Self {
            authority,
            cfg,
            scope,
            active: AtomicUsize::new(0),
            seen: Mutex::new((HashSet::new(), VecDeque::new())),
        })
    }

    fn remember(&self, op: Uuid) -> bool {
        let Ok(mut g) = self.seen.lock() else {
            return false;
        };
        if !g.0.insert(op) {
            return false;
        }
        g.1.push_back(op);
        if g.1.len() > SEEN_OPS {
            if let Some(old) = g.1.pop_front() {
                g.0.remove(&old);
            }
        }
        true
    }

    /// Serves one frontend connection until it closes. Each `Begin` is one operation; operations
    /// on one connection are sequential.
    pub fn serve(&self, sock: UnixStream) {
        let Ok(read_half) = sock.try_clone() else {
            return;
        };
        let mut reader = BufReader::new(read_half);
        loop {
            let line = match read_line_limited(&mut reader) {
                Ok(Some(l)) => l,
                Ok(None) => return,
                Err(e) => {
                    self.refuse(&sock, &e);
                    return;
                }
            };
            match serde_json::from_str::<ToBroker>(&line) {
                Ok(ToBroker::Begin {
                    v,
                    op,
                    kind,
                    root,
                    spaces,
                }) => {
                    if let Err(why) = self.operation(&sock, &mut reader, v, op, kind, root, spaces)
                    {
                        self.refuse(&sock, &why);
                    }
                }
                // Done/Cancel outside an operation, or anything unparsable: a protocol violation
                Ok(_) => {
                    self.refuse(&sock, "a message outside an operation");
                    return;
                }
                Err(e) => {
                    self.refuse(&sock, &format!("unreadable message: {e}"));
                    return;
                }
            }
        }
    }

    fn refuse(&self, sock: &UnixStream, why: &str) {
        let m = FromBroker::Refused {
            reason: why.chars().take(500).collect(),
        };
        let mut b = serde_json::to_vec(&m).unwrap_or_default();
        b.push(b'\n');
        let _ = (&*sock).write_all(&b);
    }

    #[allow(clippy::too_many_arguments)]
    fn operation(
        &self,
        sock: &UnixStream,
        reader: &mut BufReader<UnixStream>,
        v: u32,
        op: Uuid,
        _kind: OpKind,
        root: String,
        spaces: Vec<Uuid>,
    ) -> Result<(), String> {
        if v != BROKER_PROTOCOL {
            return Err(format!("broker protocol {v}"));
        }
        if root.len() > MAX_ROOT_BYTES || root.contains('\0') {
            return Err("the root name is too long or contains NUL".into());
        }
        validate_spaces(&spaces)?;
        // 1. authority: from trusted state, the root must be exactly one of the authorized ones
        let roots = self.authority.roots()?;
        let Some(auth) = roots.into_iter().find(|r| r.path == Path::new(&root)) else {
            return Err("the root is not authorized".into());
        };
        // 2. replay: an operation id is usable once, and only after authorization succeeded
        if !self.remember(op) {
            return Err("the operation id was already used".into());
        }
        // 3. concurrency
        if self.active.fetch_add(1, Ordering::SeqCst) >= self.cfg.max_concurrent {
            self.active.fetch_sub(1, Ordering::SeqCst);
            return Err("busy: too many operations".into());
        }
        let _permit = Permit(&self.active);
        // 4. the root, pinned: opened here, checked against the approved identity and path
        let pinned = crate::sandbox::open_verified_dir(&auth.path, (auth.dev, auth.ino))?;
        let real = real_path_of_fd(&pinned).map_err(|e| e.to_string())?;
        if real != auth.path {
            return Err("the root is not at its authorized path".into());
        }
        let root_fd = reopen_dir(&pinned).map_err(|e| format!("root descriptor: {e}"))?;
        // 5. cgroup
        let cg = match (&self.cfg.cleanup, &self.scope) {
            (
                CleanupPolicy::Cgroup {
                    memory_max,
                    pids_max,
                },
                Some(scope),
            ) => Some(OpCgroup::create(scope, op, *memory_max, *pids_max)?),
            _ => None,
        };
        // 6. the worker
        let (front_end, child_end) = UnixStream::pair().map_err(|e| format!("socketpair: {e}"))?;
        let (ctl_broker, ctl_child) = UnixStream::pair().map_err(|e| format!("socketpair: {e}"))?;
        let procs_fd = match &cg {
            Some(c) => Some(
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(c.dir.join("cgroup.procs"))
                    .map_err(|e| format!("cgroup.procs: {e}"))?,
            ),
            None => None,
        };
        let child = self.spawn(&child_end, &root_fd, &ctl_child, procs_fd.as_ref())?;
        drop(child_end);
        drop(ctl_child);
        drop(root_fd);
        drop(procs_fd);
        let pid = child.id();
        let mut run = Running {
            pid,
            cg,
            leader_reaped: false,
            status: 0,
            rusage: unsafe { std::mem::zeroed() },
            unreliable: self.scope.is_none(),
        };
        let _ = child; // reaped by `Running::finish` with wait4 (rusage), never by Child's Drop
        let cleanup = if run.cg.is_some() {
            Cleanup::Cgroup
        } else {
            Cleanup::ProcessGroupOnly
        };
        // 7. READY, observed from outside, then GO
        let observed = match self.await_ready(&ctl_broker, &run, cleanup) {
            Ok(o) => o,
            Err(e) => {
                run.finish(self.cfg.exit_grace, false);
                return Err(e);
            }
        };
        if (&ctl_broker).write_all(b"GO\n").is_err() {
            run.finish(self.cfg.exit_grace, false);
            return Err("the worker vanished before it was released".into());
        }
        // 8. the frontend gets the channel and what was observed
        let ready = FromBroker::Ready {
            v: BROKER_PROTOCOL,
            op,
            spaces,
            root,
            authority: auth.source,
            observed,
        };
        let mut b = serde_json::to_vec(&ready).map_err(|e| e.to_string())?;
        b.push(b'\n');
        if send_with_fd(sock, &b, front_end.as_raw_fd()).is_err() {
            run.finish(self.cfg.exit_grace, false);
            return Err("the frontend went away".into());
        }
        drop(front_end);
        // 9. supervise
        let by_itself = self.supervise(sock, reader, &mut run);
        // the timeout belongs to supervision only; the connection serves the next operation
        let _ = sock.set_read_timeout(None);
        let peak = run.cg.as_ref().and_then(|c| c.peak());
        let killed = run.finish(self.cfg.exit_grace, by_itself);
        let fin = FromBroker::Finished {
            op,
            exit: describe(run.status),
            exited_by_itself: by_itself && run.leader_exited_alone(),
            descendants_killed: killed,
            peak_rss_kb: run.rusage.ru_maxrss,
            cgroup_peak_bytes: peak,
            cleanup,
        };
        let mut b = serde_json::to_vec(&fin).map_err(|e| e.to_string())?;
        b.push(b'\n');
        let _ = (&*sock).write_all(&b);
        Ok(())
    }

    fn spawn(
        &self,
        channel: &UnixStream,
        root: &OwnedFd,
        control: &UnixStream,
        cgroup_procs: Option<&std::fs::File>,
    ) -> Result<std::process::Child, String> {
        let mut cmd = Command::new(&self.cfg.exe);
        cmd.args(&self.cfg.args)
            .env_clear()
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let (chan, rootfd, ctl) = (channel.as_raw_fd(), root.as_raw_fd(), control.as_raw_fd());
        let procs = cgroup_procs.map(|f| f.as_raw_fd());
        let (rlimit_as, rlimit_cpu) = (self.cfg.rlimit_as_mib, self.cfg.rlimit_cpu_secs);
        // SAFETY: only async-signal-safe calls between fork and exec (write, setpgid, prctl,
        // setrlimit, fcntl, dup2, close).
        unsafe {
            cmd.pre_exec(move || {
                // join the operation's cgroup before anything else can fork
                if let Some(fd) = procs {
                    if libc::write(fd, b"0".as_ptr() as *const _, 1) != 1 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
                let zero = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                libc::setrlimit(libc::RLIMIT_CORE, &zero);
                if let Some(mib) = rlimit_as {
                    let l = libc::rlimit {
                        rlim_cur: mib << 20,
                        rlim_max: mib << 20,
                    };
                    if libc::setrlimit(libc::RLIMIT_AS, &l) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                if let Some(s) = rlimit_cpu {
                    let l = libc::rlimit {
                        rlim_cur: s,
                        rlim_max: s,
                    };
                    if libc::setrlimit(libc::RLIMIT_CPU, &l) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                // stage the three descriptors above the targets, then place them
                let pairs = [(chan, CHANNEL_FD), (rootfd, ROOT_FD), (ctl, CONTROL_FD)];
                let mut staged = [0i32; 3];
                for (i, (from, _)) in pairs.iter().enumerate() {
                    let t = libc::fcntl(*from, libc::F_DUPFD, 500);
                    if t < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    staged[i] = t;
                }
                for (i, (_, to)) in pairs.iter().enumerate() {
                    if libc::dup2(staged[i], *to) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                for t in staged {
                    libc::close(t);
                }
                Ok(())
            });
        }
        cmd.spawn()
            .map_err(|e| format!("cannot start the worker: {e}"))
    }

    /// Waits for `READY` and checks the process from outside. What can be seen from outside is
    /// limited (see the architecture note): seccomp mode, no-new-privs, no tracer, no
    /// capabilities and the cgroup. The Landlock domain is NOT visible in `/proc`.
    fn await_ready(
        &self,
        ctl: &UnixStream,
        run: &Running,
        cleanup: Cleanup,
    ) -> Result<Observed, String> {
        ctl.set_read_timeout(Some(self.cfg.startup_timeout))
            .map_err(|e| e.to_string())?;
        let mut line = [0u8; 6];
        let mut got = 0;
        while got < line.len() {
            match (&*ctl).read(&mut line[got..]) {
                Ok(0) => return Err("the worker ended before it was confined".into()),
                Ok(n) => got += n,
                Err(_) => return Err("the worker did not report in time".into()),
            }
        }
        if &line != b"READY\n" {
            return Err("the worker sent something else than READY".into());
        }
        let pid = run.pid;
        let field = |k: &str| status_field(pid, k).unwrap_or_default();
        let o = Observed {
            pid,
            seccomp: field("Seccomp:"),
            no_new_privs: field("NoNewPrivs:"),
            tracer_pid: field("TracerPid:"),
            cap_eff: field("CapEff:"),
            cgroup: std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
                .ok()
                .and_then(|t| {
                    t.lines()
                        .find_map(|l| l.strip_prefix("0::"))
                        .map(str::to_string)
                }),
            cleanup,
        };
        if self.cfg.verify_confinement && o.seccomp != "2" {
            return Err(format!("/proc says Seccomp: {:?}, expected 2", o.seccomp));
        }
        if self.cfg.verify_confinement && o.no_new_privs != "1" {
            return Err(format!("/proc says NoNewPrivs: {:?}", o.no_new_privs));
        }
        if self.cfg.verify_confinement && o.tracer_pid != "0" {
            return Err(format!("the worker is traced: {:?}", o.tracer_pid));
        }
        if self.cfg.verify_confinement && !o.cap_eff.trim_start_matches('0').is_empty() {
            return Err(format!("the worker holds capabilities: {}", o.cap_eff));
        }
        if let Some(cg) = &run.cg {
            let want = cg
                .dir
                .strip_prefix("/sys/fs/cgroup")
                .map(|p| format!("/{}", p.display()))
                .unwrap_or_default();
            if o.cgroup.as_deref() != Some(want.as_str()) {
                return Err(format!(
                    "the worker is in cgroup {:?}, expected {want}",
                    o.cgroup
                ));
            }
        }
        Ok(o)
    }

    /// Returns whether the leader ended by itself after `Done`. Ends (and returns false) on
    /// `Cancel`, a frontend hang-up, a protocol violation or the wall-clock limit.
    fn supervise(
        &self,
        sock: &UnixStream,
        reader: &mut BufReader<UnixStream>,
        run: &mut Running,
    ) -> bool {
        let deadline = Instant::now() + self.cfg.wall_timeout;
        let _ = sock.set_read_timeout(Some(Duration::from_millis(10)));
        loop {
            if Instant::now() >= deadline {
                return false;
            }
            // the worker may end on its own (crash, or done) while the frontend is still reading
            let mut line = Vec::new();
            match reader
                .by_ref()
                .take(MAX_LINE as u64 + 1)
                .read_until(b'\n', &mut line)
            {
                Ok(0) => return false, // frontend hung up
                Ok(_) if line.last() == Some(&b'\n') => {
                    line.pop();
                    return match serde_json::from_slice::<ToBroker>(&line) {
                        Ok(ToBroker::Done) => run.wait_leader(self.cfg.exit_grace),
                        // Cancel, a second Begin, junk: all end the operation
                        _ => false,
                    };
                }
                Ok(_) => return false, // too long / truncated
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return false,
            }
        }
    }
}

/// The worker of one operation and what ends it.
struct Running {
    pid: u32,
    cg: Option<OpCgroup>,
    leader_reaped: bool,
    status: i32,
    rusage: libc::rusage,
    unreliable: bool,
}

impl Running {
    /// Waits (without reaping) until the leader has exited, up to `grace`.
    fn wait_leader(&self, grace: Duration) -> bool {
        let end = Instant::now() + grace;
        loop {
            // SAFETY: waitid WNOHANG|WNOWAIT on our own child observes without reaping.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let r = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if r != 0 || unsafe { info.si_pid() } != 0 {
                return r == 0;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn leader_exited_alone(&self) -> bool {
        libc::WIFEXITED(self.status)
    }

    /// Ends everything of the operation and reaps the leader. Returns how many processes were
    /// still alive in the cgroup when it was killed (0 without a cgroup: unknown). `_by_itself`
    /// is informational only; the kill is unconditional.
    fn finish(&mut self, _grace: Duration, _by_itself: bool) -> u32 {
        // The leader first: a SIGKILL to its group is a no-op on one that already exited by
        // itself, so its real exit status survives; reaping it before the cgroup is counted keeps
        // the leader's own (zombie) entry out of the count of descendants.
        // SAFETY: SIGKILL to the group this broker created for the worker.
        unsafe { libc::kill(-(self.pid as i32), libc::SIGKILL) };
        if !self.leader_reaped {
            let mut st = 0i32;
            // SAFETY: wait4 on our own child; fills status and resource usage.
            let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
            unsafe { libc::wait4(self.pid as i32, &mut st, 0, &mut ru) };
            self.status = st;
            self.rusage = ru;
            self.leader_reaped = true;
        }
        // then everything else the operation started, in any group or session
        let killed = self.cg.as_ref().map(|c| c.kill()).unwrap_or(0);
        let _ = self.unreliable;
        killed
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if !self.leader_reaped {
            self.finish(Duration::ZERO, false);
        }
    }
}
