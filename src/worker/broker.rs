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

    /// The commit-time decision. Takes whatever excludes a concurrent revocation FIRST (for a
    /// grant: the shared lock of the grant store, which `mind grant revoke` needs exclusively),
    /// THEN verifies that `auth` is still valid and will stay valid for `horizon`. Dropping the
    /// returned [`Hold`] releases the exclusion. Err: not authorized (fail closed).
    fn hold(&self, auth: &AuthorizedRoot, horizon: Duration) -> Result<Hold, String>;
}

/// See [`RootAuthority::hold`].
#[derive(Debug)]
pub struct Hold(#[allow(dead_code)] Option<crate::grants::StoreHold>);

/// How long a commit lease can last: after this the broker lets revocations proceed whether or not
/// the frontend has finished. Also the horizon a grant must stay valid for to be granted a lease.
pub const COMMIT_HOLD_MAX: Duration = Duration::from_secs(3);

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

    fn hold(&self, auth: &AuthorizedRoot, _horizon: Duration) -> Result<Hold, String> {
        // a static root cannot be revoked: nothing to exclude, only the root must still be ours
        self.0
            .iter()
            .any(|r| r.path == auth.path && r.dev == auth.dev && r.ino == auth.ino)
            .then_some(Hold(None))
            .ok_or_else(|| "the static root is not configured".to_string())
    }
}

/// Active grants of the store, re-validated now (path policy, device/inode as approved).
pub struct GrantRoots {
    pub store: crate::grants::Store,
    pub policy: crate::grants::PathPolicy,
}

impl GrantRoots {
    /// The grants valid at `now` (the commit lease asks for `now + horizon`, so a grant that
    /// would expire inside the lease is not leased).
    fn roots_at(&self, now: time::OffsetDateTime) -> Result<Vec<AuthorizedRoot>, String> {
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

impl RootAuthority for GrantRoots {
    fn roots(&self) -> Result<Vec<AuthorizedRoot>, String> {
        self.roots_at(time::OffsetDateTime::now_utc())
    }

    fn hold(&self, auth: &AuthorizedRoot, horizon: Duration) -> Result<Hold, String> {
        // order matters: the lock first, the check second. A revocation that completed before the
        // lock is seen by the check; one that starts after it waits for the lock.
        let guard = self.store.hold_shared().map_err(|e| e.to_string())?;
        let until = time::OffsetDateTime::now_utc() + horizon;
        let valid = self.roots_at(until)?.iter().any(|r| {
            r.source == auth.source && r.path == auth.path && r.dev == auth.dev && r.ino == auth.ino
        });
        if valid {
            Ok(Hold(guard))
        } else {
            Err("the grant was revoked, expired or can no longer be verified".to_string())
        }
    }
}

/// Static roots always; grants too when a store is configured. A grant store that cannot be
/// verified contributes NO root (fewer roots, never more), the static roots stay usable.
pub struct CombinedAuthority {
    pub static_roots: StaticRoots,
    pub grants: Option<GrantRoots>,
}

impl RootAuthority for CombinedAuthority {
    fn roots(&self) -> Result<Vec<AuthorizedRoot>, String> {
        let mut v = self.static_roots.roots()?;
        if let Some(g) = &self.grants {
            if let Ok(more) = g.roots() {
                v.extend(more);
            }
        }
        Ok(v)
    }

    fn hold(&self, auth: &AuthorizedRoot, horizon: Duration) -> Result<Hold, String> {
        match &auth.source {
            AuthoritySource::Static => self.static_roots.hold(auth, horizon),
            AuthoritySource::Grant(_) => match &self.grants {
                Some(g) => g.hold(auth, horizon),
                None => Err("no grant store is configured".to_string()),
            },
        }
    }
}

// ------------------------------------------------------------------ the broker process (stage 1)

/// Body of the broker process forked by the watchdog: outside the frontend's Landlock domain, under the inherited socket-deny filter, with exactly one descriptor (the
/// channel to the frontend). It serves that one connection and exits when the frontend closes it
/// (which includes the frontend dying), after ending every operation it had running.
fn process_main(
    link: UnixStream,
    exe: PathBuf,
    static_roots: Vec<PathBuf>,
    grants: Option<(PathBuf, crate::grants::PathPolicy)>,
    scope: &CgroupScope,
    watchdog: i32,
) -> ! {
    ScopeHandle::adopt(scope);
    // SAFETY: plain prctl/open/dup2 calls on this process. Not dumpable: the frontend is an
    // ancestor and must not be able to ptrace it or read its memory. It dies with the watchdog
    // (a broker without its supervisor must not keep serving).
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
        if libc::getppid() != watchdog {
            libc::_exit(72);
        }
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
        // the broker must not keep the client's stdin/stdout pipes open (stderr stays: diagnostics)
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if null >= 0 {
            libc::dup2(null, 0);
            libc::dup2(null, 1);
            if null > 2 {
                libc::close(null);
            }
        }
    }
    crate::sandbox::close_all_except(&[link.as_raw_fd()]);
    let code = run_process(link, exe, static_roots, grants);
    // SAFETY: leave without running the parent's atexit handlers or flushing its buffers.
    unsafe { libc::_exit(code) }
}

fn run_process(
    link: UnixStream,
    exe: PathBuf,
    static_roots: Vec<PathBuf>,
    grants: Option<(PathBuf, crate::grants::PathPolicy)>,
) -> i32 {
    let fail = |m: String, c: i32| {
        eprintln!("mind broker: {m}");
        c
    };
    let static_roots = match StaticRoots::new(&static_roots) {
        Ok(s) => s,
        Err(e) => return fail(format!("static roots: {e}"), 70),
    };
    let authority = CombinedAuthority {
        static_roots,
        grants: grants.map(|(dir, policy)| GrantRoots {
            store: crate::grants::Store::in_dir(dir),
            policy,
        }),
    };
    #[allow(unused_mut)]
    let mut cfg = BrokerConfig::new(exe);
    // Test builds only (feature `spawn-gate`, enabled for `cargo test`, absent from a plain
    // build): stand-in workers and short limits for the failure tests.
    #[cfg(feature = "spawn-gate")]
    {
        if let Ok(script) = std::env::var("MINDSPACE_BROKER_GATE_WORKER") {
            cfg.exe = PathBuf::from("/bin/sh");
            cfg.args = vec![script.into()];
            cfg.verify_confinement = false;
        }
        if let Some(m) = std::env::var("MINDSPACE_BROKER_GATE_MEMORY_MAX")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            cfg.cleanup = CleanupPolicy::Cgroup {
                memory_max: Some(m),
                pids_max: Some(256),
            };
        }
        if let Some(ms) = std::env::var("MINDSPACE_BROKER_GATE_COMMIT_INFLIGHT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            cfg.commit_inflight_ceiling = Duration::from_millis(ms);
        }
        if let Some(ms) = std::env::var("MINDSPACE_BROKER_GATE_WALL_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            cfg.wall_timeout = Duration::from_millis(ms);
        }
    }
    let mut broker = match Broker::new(Box::new(authority), cfg) {
        Ok(b) => b,
        Err(e) => return fail(format!("cannot start (no cgroup delegation?): {e}"), 71),
    };
    // The peer of a socketpair is named by whoever created it: stage 1, which became the
    // frontend. Take a pidfd now, while that process certainly exists.
    {
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: SO_PEERCRED on our own socket.
        let ok = unsafe {
            libc::getsockopt(
                link.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut _ as *mut _,
                &mut len,
            )
        } == 0;
        if ok && cred.pid > 0 {
            // SAFETY: pidfd_open on that pid; the descriptor is owned from here on.
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, cred.pid, 0) } as i32;
            if fd >= 0 {
                broker.set_frontend(unsafe { OwnedFd::from_raw_fd(fd) });
            }
        }
    }
    broker.serve(link);
    0
}

/// Whether the broker is configured with a per-operation limit that needs cgroup controllers. The
/// product configures none yet; the test hook is the only source.
fn limits_requested() -> bool {
    #[cfg(feature = "spawn-gate")]
    if std::env::var_os("MINDSPACE_BROKER_GATE_MEMORY_MAX").is_some() {
        return true;
    }
    false
}

/// The watchdog (M7.3b.3b): forked by stage 1 outside the frontend's Landlock domain, it is the
/// broker's PARENT and the owner of the cgroup tree. It exists because nothing inside the
/// broker's own failure domain can clean up after the broker's SIGKILL, and because the
/// frontend, being confined, cannot touch cgroups.
///
/// It holds no IPC and parses nothing: it creates the tree, forks the broker into it, and waits
/// on two process handles (`pidfd`): the broker and the frontend. When the broker ends (for any
/// reason) it kills every operation cgroup, reaps what was orphaned to it (it is a subreaper, so
/// escaped descendants and the workers reach it instead of an unknown init), waits until the
/// cgroups are empty, removes the tree and exits. When the frontend ends first it gives the
/// broker a few seconds to end its operations by itself, then kills it and does the same.
/// Cleanup therefore never depends on a future broker start.
///
/// Not covered (a double fault): the watchdog itself being killed. The broker then dies with it
/// (PDEATHSIG) and its workers with the broker, but an escaped descendant survives until the next
/// start sweeps the tree.
pub fn watchdog_main(
    back: UnixStream,
    frontend: i32,
    frontend_pidfd: i32,
    exe: PathBuf,
    static_roots: Vec<PathBuf>,
    grants: Option<(PathBuf, crate::grants::PathPolicy)>,
) -> ! {
    let fail = |m: String, c: i32| -> ! {
        eprintln!("mind broker: {m}");
        // SAFETY: leave without running the parent's atexit handlers.
        unsafe { libc::_exit(c) }
    };
    // SAFETY: plain prctl/open/dup2 calls on this process.
    unsafe {
        // findable and identifiable in `ps`: the frontend's pid is part of the name (15 bytes max)
        let name = std::ffi::CString::new(format!("mind-wd-{frontend}")).unwrap_or_default();
        libc::prctl(libc::PR_SET_NAME, name.as_ptr(), 0, 0, 0);
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
        libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if null >= 0 {
            libc::dup2(null, 0);
            libc::dup2(null, 1);
            if null > 2 {
                libc::close(null);
            }
        }
    }
    crate::sandbox::close_all_except(&[back.as_raw_fd(), frontend_pidfd]);
    // Required facility: without a cgroup tree there is no broker (the frontend sees the channel
    // close and every dynamic operation fails explicitly).
    let scope = match CgroupScope::create(limits_requested()) {
        Ok(s) => s,
        Err(e) => fail(format!("cannot start (no cgroup delegation?): {e}"), 71),
    };
    let broker = unsafe { libc::fork() };
    if broker < 0 {
        scope.teardown();
        fail("cannot fork the broker".into(), 70);
    }
    if broker == 0 {
        process_main(back, exe, static_roots, grants, &scope, unsafe {
            libc::getppid()
        });
    }
    drop(back); // the broker holds the only copy; the watchdog has no channel at all
    let pidfd = |pid: i32| -> i32 {
        // SAFETY: pidfd_open on a pid we just obtained; the descriptor is closed with the process.
        unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 }
    };
    let bfd = pidfd(broker);
    let ffd = frontend_pidfd;
    let poll = |fds: &mut [libc::pollfd], ms: i32| {
        // SAFETY: poll over descriptors owned by this process.
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, ms) }
    };
    if bfd < 0 {
        scope.teardown();
        fail(
            "pidfd_open is unavailable (kernel older than 5.3?)".into(),
            71,
        );
    }
    // Either the broker ends or the frontend does (a pidfd becomes readable when its process
    // exits, also if that already happened).
    let mut fds = [
        libc::pollfd {
            fd: bfd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: ffd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        let r = poll(&mut fds, -1);
        if r < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        break;
    }
    if fds[0].revents == 0 {
        // the frontend ended first: the broker gets time to end its operations itself
        let mut b = [libc::pollfd {
            fd: bfd,
            events: libc::POLLIN,
            revents: 0,
        }];
        poll(&mut b, 3000);
    }
    // The broker is gone or is made gone now; either way its operations end.
    // SAFETY: SIGKILL to our own child (a no-op if it already ended).
    unsafe { libc::kill(broker, libc::SIGKILL) };
    let mut st = 0;
    unsafe { libc::waitpid(broker, &mut st, 0) };
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let mut left = 0;
        if let Ok(rd) = std::fs::read_dir(&scope.base) {
            for e in rd.flatten() {
                if !e.file_name().to_string_lossy().starts_with("op-") || !e.path().is_dir() {
                    continue;
                }
                let _ = std::fs::write(e.path().join("cgroup.kill"), "1");
                // reap what was orphaned to us (subreaper): zombies keep a cgroup populated
                loop {
                    let mut s = 0;
                    // SAFETY: reaping our own (adopted) children only.
                    let r = unsafe { libc::waitpid(-1, &mut s, libc::WNOHANG) };
                    if r <= 0 {
                        break;
                    }
                }
                if std::fs::remove_dir(e.path()).is_err() {
                    left += 1;
                }
            }
        }
        if left == 0 || Instant::now() >= end {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    scope.teardown();
    // SAFETY: leave without running the parent's atexit handlers.
    unsafe { libc::_exit(0) }
}

/// Stage 2 (the frontend): the channel to the broker, once. `None` when the server was not
/// started with a broker, when the descriptor is not a unix stream socket, or on a second call.
pub fn take_frontend_link() -> Option<UnixStream> {
    static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if TAKEN.swap(true, Ordering::SeqCst) {
        return None;
    }
    let fd = crate::sandbox::broker_fd_from_env()?;
    // SAFETY: the descriptor was validated as a socket and is owned by this process from here on;
    // close-on-exec keeps it out of the Git children the frontend starts.
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        Some(UnixStream::from_raw_fd(fd))
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
    /// Right before the COMMIT of the result of the operation that just ended `done`: may the
    /// frontend commit? The answer is a lease (see [`RootAuthority::hold`]).
    CommitRequest {
        op: Uuid,
    },
    /// The frontend is about to run COMMIT (sent right before it). From here the broker keeps the
    /// lease until `CommitDone` or the frontend's death, up to the in-flight ceiling; at the
    /// ceiling it removes the frontend (SIGKILL) before it releases the lease.
    CommitBegin,
    /// The frontend's commit finished (or was abandoned): the lease may be released.
    CommitDone,
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

/// How an operation ended, as the broker saw it. Only `Done` permits persisting the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndedBy {
    /// The frontend validated the stream and said `Done`, and the authority was still valid at
    /// the broker's final recheck (the point after which a revocation no longer matters).
    Done,
    /// `Cancel`, a hang-up or a protocol violation from the frontend.
    Cancelled,
    /// The broker's wall-clock limit.
    Timeout,
    /// The grant behind the operation was revoked or expired, or could not be verified.
    Revoked,
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
    /// The frontend may commit now; revocations wait until `CommitDone`, a hang-up or
    /// [`COMMIT_HOLD_MAX`].
    CommitGranted {
        op: Uuid,
    },
    /// The frontend must not commit.
    CommitDenied {
        op: Uuid,
        reason: String,
    },
    Finished {
        op: Uuid,
        exit: String,
        ended_by: EndedBy,
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
    /// Only the owner (the watchdog in the product, the creating process in library use) removes
    /// the tree. A broker that was handed the scope by its watchdog only uses it.
    owner: bool,
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
        let s = Arc::new(CgroupScope::create(true)?);
        st.scope = Some(s.clone());
        st.refs = 1;
        Ok(Self(s))
    }
}

impl ScopeHandle {
    /// In the broker process forked by the watchdog: use the watchdog's scope without owning it.
    /// The count never reaches zero in this process, so nothing here ever tears it down.
    fn adopt(scope: &CgroupScope) {
        if let Ok(mut st) = SCOPE.lock() {
            st.scope = Some(Arc::new(CgroupScope {
                base: scope.base.clone(),
                original: scope.original.clone(),
                owner: false,
                controllers: scope.controllers.clone(),
            }));
            st.refs = 1;
        }
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
    /// Where the tree goes. Containment first: INSIDE the cgroup the process started in, so that a
    /// service manager that kills the unit's cgroup (`KillMode=control-group`, the default) also
    /// ends the tree, even when the watchdog itself was killed (VERIFIED with `systemd-run
    /// --user`). A cgroup that holds processes cannot delegate controllers, so that placement has
    /// none: per-operation `memory.max`/`pids.max` are then unavailable and refused, never
    /// skipped. When `need_controllers` (a limit is configured) the tree goes beside the original
    /// cgroup, where the parent can delegate them, at the price of leaving the unit.
    /// With no usable parent at all this fails and no broker is started.
    fn create(need_controllers: bool) -> Result<Self, String> {
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
        // Without a configured limit there is exactly one acceptable placement: inside the
        // cgroup this process started in (so the unit's lifecycle and limits cover the tree). If
        // that fails the start fails; a sibling placement would silently leave the unit.
        if !need_controllers {
            candidates.truncate(1);
        }
        // Test builds only (feature `spawn-gate`): point the broker at a parent it cannot write
        // to, to observe the real kernel refusal and the fail-closed start.
        #[cfg(feature = "spawn-gate")]
        if let Ok(p) = std::env::var("MINDSPACE_BROKER_GATE_CGROUP_PARENT") {
            // `own`: only inside the cgroup this process started in (e.g. a delegated unit)
            candidates = if p == "own" {
                vec![original.clone()]
            } else {
                vec![PathBuf::from(p)]
            };
        }
        // Strictly one at a time: the process lives in at most one `supervisor`. A candidate
        // without controllers is torn down (the process goes home) before the next is tried;
        // if none has controllers the first one that works is created again and kept.
        let mut first_error = None;
        let mut first_ok: Option<PathBuf> = None;
        for parent in &candidates {
            match Self::create_in(parent, &original) {
                Ok(s) if !need_controllers || !s.controllers.is_empty() => return Ok(s),
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
    /// up): processes still in them are killed (`cgroup.kill`) and the tree is removed. Only
    /// directories named `mindspace-<pid>[-n]` whose pid is gone are touched.
    fn sweep(parent: &Path) {
        fn remove_tree(dir: &Path) {
            // a dead broker may have left workers (or their escaped descendants) behind
            let _ = std::fs::write(dir.join("cgroup.kill"), "1");
            let end = Instant::now() + Duration::from_millis(500);
            while Instant::now() < end
                && std::fs::read_to_string(dir.join("cgroup.events"))
                    .is_ok_and(|e| !e.lines().any(|l| l.trim() == "populated 0"))
            {
                std::thread::sleep(Duration::from_millis(5));
            }
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
            owner: true,
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
        if !self.owner {
            return;
        }
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
    /// A frontend that was granted a commit lease must send `CommitBegin` (or `CommitDone`)
    /// within this time, else it is removed.
    pub commit_grant_ceiling: Duration,
    /// Once `CommitBegin` was sent, the COMMIT may take this long; then the frontend is removed.
    pub commit_inflight_ceiling: Duration,
    /// How often the authority behind a running operation is verified again. Only revocable
    /// authority (a grant) is rechecked; a static root cannot be revoked.
    pub recheck_interval: Duration,
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
            commit_grant_ceiling: COMMIT_HOLD_MAX,
            commit_inflight_ceiling: Duration::from_secs(30),
            recheck_interval: Duration::from_millis(100),
            max_concurrent: 1,
        }
    }
}

pub struct Broker {
    authority: Box<dyn RootAuthority>,
    cfg: BrokerConfig,
    /// The frontend process, as a pidfd taken when the connection was made (so a recycled pid can
    /// never be signalled). `None` in library use, where the "frontend" is the caller's own process.
    frontend: Option<OwnedFd>,
    scope: Option<ScopeHandle>,
    active: AtomicUsize,
    seen: Mutex<(HashSet<Uuid>, VecDeque<Uuid>)>,
}

/// What `operation` leaves for the serve loop.
struct OpEnd {
    terminal_pending: bool,
    commit: Option<(Uuid, AuthorizedRoot)>,
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
            frontend: None,
            scope,
            active: AtomicUsize::new(0),
            seen: Mutex::new((HashSet::new(), VecDeque::new())),
        })
    }

    /// Product use: the process on the other end of the channel, which the broker may remove when
    /// it holds a commit lease for longer than the ceilings allow.
    pub fn set_frontend(&mut self, pidfd: OwnedFd) {
        self.frontend = Some(pidfd);
    }

    /// Removes the frontend and waits until it is gone. The lease is released only AFTER this: a
    /// process that can no longer run cannot complete a COMMIT. Without a known frontend nothing
    /// can be removed and the caller falls back to releasing at the ceiling (documented).
    /// A process that cannot die yet (blocked in the kernel) is waited for: its COMMIT is then
    /// genuinely in flight and the revocation must wait with it.
    fn remove_frontend(&self) -> bool {
        let Some(fd) = &self.frontend else {
            return false;
        };
        // SAFETY: pidfd_send_signal on a pidfd this process owns; no siginfo.
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            );
        }
        let mut p = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            // SAFETY: poll on a descriptor this process owns; readable once the process exited.
            let r = unsafe { libc::poll(&mut p, 1, -1) };
            if r > 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return true;
            }
        }
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
        // After an operation the BROKER ended (revocation, timeout) the frontend still sends its
        // own terminal message (`Cancel`, because its result is an error). Exactly that one
        // message is expected and ignored; any other stray message stays a protocol violation.
        let mut terminal_pending = false;
        // the operation that ended `done` and whose result the frontend may ask to commit
        let mut awaiting: Option<(Uuid, AuthorizedRoot)> = None;
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
                    terminal_pending = false;
                    awaiting = None;
                    match self.operation(&sock, &mut reader, v, op, kind, root, spaces) {
                        Ok(end) => {
                            terminal_pending = end.terminal_pending;
                            awaiting = end.commit;
                        }
                        Err(why) => self.refuse(&sock, &why),
                    }
                }
                Ok(ToBroker::CommitRequest { op }) => {
                    self.commit_lease(&sock, &mut reader, op, awaiting.take());
                }
                // A completion message that arrives after its lease ended (the ceiling, or a
                // frontend that gave up waiting for the grant) is harmless: ignore it.
                Ok(ToBroker::CommitDone | ToBroker::CommitBegin) => {}
                Ok(ToBroker::Done | ToBroker::Cancel) if terminal_pending => {
                    terminal_pending = false;
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
    ) -> Result<OpEnd, String> {
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
            authority: auth.source.clone(),
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
        let (ended_by, by_itself, consumed) = self.supervise(sock, reader, &mut run, &auth);
        // the timeout belongs to supervision only; the connection serves the next operation
        let _ = sock.set_read_timeout(None);
        let peak = run.cg.as_ref().and_then(|c| c.peak());
        let killed = run.finish(self.cfg.exit_grace, by_itself);
        let fin = FromBroker::Finished {
            op,
            exit: describe(run.status),
            ended_by,
            exited_by_itself: by_itself && run.leader_exited_alone(),
            descendants_killed: killed,
            peak_rss_kb: run.rusage.ru_maxrss,
            cgroup_peak_bytes: peak,
            cleanup,
        };
        let mut b = serde_json::to_vec(&fin).map_err(|e| e.to_string())?;
        b.push(b'\n');
        let _ = (&*sock).write_all(&b);
        // terminal_pending: the broker ended it, so the frontend's terminal message is still on
        // its way. commit: only an operation that ended `done` may ask for a commit lease.
        Ok(OpEnd {
            terminal_pending: !consumed,
            commit: (ended_by == EndedBy::Done).then_some((op, auth)),
        })
    }

    /// The commit-time decision (see [`RootAuthority::hold`]). Single use per finished operation.
    fn commit_lease(
        &self,
        sock: &UnixStream,
        reader: &mut BufReader<UnixStream>,
        op: Uuid,
        awaiting: Option<(Uuid, AuthorizedRoot)>,
    ) {
        let reply = |m: &FromBroker| {
            let mut b = serde_json::to_vec(m).unwrap_or_default();
            b.push(b'\n');
            let _ = (&*sock).write_all(&b);
        };
        let Some((_, auth)) = awaiting.filter(|(a, _)| *a == op) else {
            reply(&FromBroker::CommitDenied {
                op,
                reason: "no finished operation awaits a commit".into(),
            });
            return;
        };
        match self.authority.hold(&auth, COMMIT_HOLD_MAX) {
            Err(reason) => reply(&FromBroker::CommitDenied { op, reason }),
            Ok(hold) => {
                // Test builds only (feature `spawn-gate`): answer late, to exercise a frontend
                // that gives up waiting while the lease is already held.
                #[cfg(feature = "spawn-gate")]
                if let Some(ms) = std::env::var("MINDSPACE_BROKER_GATE_COMMIT_REPLY_DELAY_MS")
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                {
                    std::thread::sleep(Duration::from_millis(ms));
                }
                reply(&FromBroker::CommitGranted { op });
                // The lease lasts until the frontend says `CommitDone`, or hangs up (it died, so
                // nothing can commit any more). Ceilings, not guesses: a frontend that does not
                // begin its COMMIT within the grant ceiling, or does not finish it within the
                // in-flight ceiling, is removed BEFORE the lock is released; SQLite offers no way
                // to cancel a COMMIT that is running, and a process that is gone cannot finish one.
                let mut next = self.cfg.commit_grant_ceiling;
                loop {
                    let _ = sock.set_read_timeout(Some(next));
                    match read_line_limited(reader) {
                        Ok(None) => break, // hang-up: the frontend is gone
                        Ok(Some(line)) => match serde_json::from_str::<ToBroker>(&line) {
                            Ok(ToBroker::CommitDone) => break,
                            Ok(ToBroker::CommitBegin) => {
                                next = self.cfg.commit_inflight_ceiling;
                                continue;
                            }
                            _ => {
                                self.remove_frontend();
                                break;
                            }
                        },
                        Err(_) => {
                            // the ceiling passed (or the channel is broken)
                            self.remove_frontend();
                            break;
                        }
                    }
                }
                let _ = sock.set_read_timeout(None);
                drop(hold);
            }
        }
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

    /// Whether the authority an operation started with still holds. A static root is not
    /// revocable and is not rechecked against the grant store (the two stay distinct). A grant
    /// must still be listed with the same id, path and device/inode; if the store cannot be
    /// verified the answer is no (fail closed).
    fn still_authorized(&self, auth: &AuthorizedRoot) -> bool {
        match &auth.source {
            AuthoritySource::Static => true,
            AuthoritySource::Grant(_) => self.authority.roots().is_ok_and(|rs| {
                rs.iter().any(|r| {
                    r.source == auth.source
                        && r.path == auth.path
                        && r.dev == auth.dev
                        && r.ino == auth.ino
                })
            }),
        }
    }

    /// Runs until the operation ends and says how. A revocable grant is rechecked every
    /// `recheck_interval`, whatever the worker is doing (parsing, or waiting inside the context
    /// exchange), and once more when the frontend says `Done`: that last check is the
    /// linearization point after which the result may be persisted.
    fn supervise(
        &self,
        sock: &UnixStream,
        reader: &mut BufReader<UnixStream>,
        run: &mut Running,
        auth: &AuthorizedRoot,
    ) -> (EndedBy, bool, bool) {
        let deadline = Instant::now() + self.cfg.wall_timeout;
        let mut next_check = Instant::now() + self.cfg.recheck_interval;
        let _ = sock.set_read_timeout(Some(Duration::from_millis(10)));
        loop {
            let now = Instant::now();
            if now >= deadline {
                return (EndedBy::Timeout, false, false);
            }
            if now >= next_check {
                next_check = now + self.cfg.recheck_interval;
                if !self.still_authorized(auth) {
                    return (EndedBy::Revoked, false, false);
                }
            }
            // the worker may end on its own (crash, or done) while the frontend is still reading
            let mut line = Vec::new();
            match reader
                .by_ref()
                .take(MAX_LINE as u64 + 1)
                .read_until(b'\n', &mut line)
            {
                Ok(0) => return (EndedBy::Cancelled, false, true), // frontend hung up
                Ok(_) if line.last() == Some(&b'\n') => {
                    line.pop();
                    return match serde_json::from_slice::<ToBroker>(&line) {
                        Ok(ToBroker::Done) => {
                            let by_itself = run.wait_leader(self.cfg.exit_grace);
                            if self.still_authorized(auth) {
                                (EndedBy::Done, by_itself, true)
                            } else {
                                (EndedBy::Revoked, false, true)
                            }
                        }
                        // Cancel, a second Begin, junk: all end the operation
                        _ => (EndedBy::Cancelled, false, true),
                    };
                }
                Ok(_) => return (EndedBy::Cancelled, false, true), // too long / truncated
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return (EndedBy::Cancelled, false, true),
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
