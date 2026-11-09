//! The parent side of the extraction worker: starts the worker, checks that it really is
//! confined, hands it one request, validates the record stream it returns, and always ends the
//! worker's whole process group. It persists nothing: the caller does, after `Ok`.

use std::cell::Cell;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::core::Uuid;
use crate::ingest::confined::Anchor;
use crate::ingest::extract::Extracted;
use crate::ingest::stream::{consume, Kind, ProducerLimits, StreamLimits};

use super::broker::{
    recv_with_fd, AuthoritySource, EndedBy, FromBroker, Observed, OpKind, ToBroker,
    BROKER_PROTOCOL, MAX_LINE,
};
use super::context::{
    answer, check_result, validate_spaces, ContextSource, Manifest, MAX_MANIFEST_LINE,
};
use super::{Hello, Request, CHANNEL_FD, MAX_HELLO, PROTOCOL, ROOT_FD};

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// The binary to run, and its arguments (`mind` and `extraction-worker`).
    pub exe: PathBuf,
    pub args: Vec<OsString>,
    /// The whole operation, start-up included. Hard: the worker's group is killed.
    pub wall_timeout: Duration,
    /// Longest silence from the worker, once started.
    pub idle_timeout: Duration,
    /// Address-space limit for the worker process (`RLIMIT_AS`, MiB). `None` sets none; no
    /// production value has been established.
    pub rlimit_as_mib: Option<u64>,
    /// How long a worker that delivered a complete stream may take to exit before it is killed.
    pub exit_grace: Duration,
    pub stream: StreamLimits,
    pub producer: ProducerLimits,
    /// Refuse a worker whose hello and `/proc` status do not show the confinement. Only tests
    /// that stand in a fake worker turn this off.
    pub require_sandbox_evidence: bool,
    /// Also check the worker's confinement in `/proc/<pid>/status` (seccomp mode, no-new-privs,
    /// no tracer). A parent that is itself confined cannot read another process's `/proc` entry
    /// and must turn this off, leaving the worker's own report as the only evidence. That is NOT
    /// independent confinement verification: a worker compromised before its hello could lie.
    /// Default true; only the tested frontend-spawn path (docs/m7.3b-frontend-spawn-gate.md) may
    /// turn it off, until a broker verifies from outside the frontend's domain.
    pub verify_via_proc: bool,
    /// Receives the worker's pid as soon as it exists (so a caller or a test can watch the
    /// process even when the call ends in an error).
    pub observe_pid: Option<std::sync::Arc<std::sync::atomic::AtomicU32>>,
}

impl WorkerConfig {
    pub fn new(exe: impl Into<PathBuf>) -> Self {
        Self {
            exe: exe.into(),
            args: vec![OsString::from("extraction-worker")],
            wall_timeout: Duration::from_secs(300),
            idle_timeout: Duration::from_secs(60),
            rlimit_as_mib: None,
            exit_grace: Duration::from_secs(5),
            stream: StreamLimits::default(),
            producer: ProducerLimits::default(),
            require_sandbox_evidence: true,
            verify_via_proc: true,
            observe_pid: None,
        }
    }
}

#[derive(Debug)]
pub enum WorkerError {
    Spawn(String),
    /// The broker refused the operation (root not authorized, replay, busy, confinement not
    /// observed, no cgroup...). Nothing was started or everything started was ended.
    Refused(String),
    /// The worker is not confined as required (or said nothing sensible); it was killed.
    Sandbox(String),
    /// The hard wall-clock limit or the idle limit expired; the worker's group was killed.
    Timeout(&'static str),
    Cancelled,
    /// The grant behind the operation was revoked (or expired, or could not be verified) while it
    /// ran or before its result could be accepted; the worker was ended and nothing may be
    /// persisted.
    Revoked,
    /// The context exchange was refused: a request the parent will not make (unknown space) or a
    /// manifest that is malformed, over budget or for another operation. Nothing was persisted.
    Context(String),
    /// The stream was malformed, truncated, over budget or inconsistent; nothing was kept.
    Stream(String),
    /// The worker ended without delivering a complete stream, or failed after it.
    Died {
        status: String,
        stderr: String,
    },
    Io(String),
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(m) => write!(f, "cannot start the extraction worker: {m}"),
            Self::Refused(m) => write!(f, "the broker refused the extraction: {m}"),
            Self::Sandbox(m) => write!(f, "the extraction worker is not confined: {m}"),
            Self::Timeout(w) => write!(f, "the extraction worker timed out ({w})"),
            Self::Cancelled => write!(f, "the extraction was cancelled"),
            Self::Revoked => write!(
                f,
                "the grant for this directory was revoked while the extraction ran; nothing was persisted"
            ),
            Self::Context(m) => write!(f, "the context exchange was refused: {m}"),
            Self::Stream(m) => write!(f, "the extraction worker's result was refused: {m}"),
            Self::Died { status, stderr } => {
                write!(f, "the extraction worker ended ({status}): {stderr}")
            }
            Self::Io(m) => write!(f, "extraction worker i/o: {m}"),
        }
    }
}

impl std::error::Error for WorkerError {}

#[derive(Debug, Clone, Default)]
pub struct WorkerStats {
    pub pid: u32,
    pub hello: Option<Hello>,
    /// Spawn until the confinement was verified.
    pub startup: Duration,
    pub total: Duration,
    pub frames: u64,
    pub bytes: u64,
    pub records: u64,
    /// Distinct source blobs the worker listed, and how many `(blob, identity)` hits the parent
    /// returned.
    pub manifest_entries: u64,
    pub known_sent: u64,
    /// Peak resident set of the worker (kB) as reported by `wait4`.
    pub worker_peak_rss_kb: i64,
    pub exit: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cause {
    Wall,
    Idle,
    Cancelled,
}

/// Reads the channel with the hard deadline, the idle limit and the cancel flag applied to every
/// wait, so that a worker that says nothing can neither hang the parent nor ignore a cancel.
struct GuardedReader<'a> {
    sock: UnixStream,
    deadline: Instant,
    idle: Duration,
    last: Instant,
    cancel: &'a AtomicBool,
    cause: Rc<Cell<Option<Cause>>>,
}

impl Read for GuardedReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let now = Instant::now();
            let stop = |c: Cause, why: &str| {
                self.cause.set(Some(c));
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    why.to_string(),
                ))
            };
            if self.cancel.load(Ordering::Relaxed) {
                return stop(Cause::Cancelled, "cancelled");
            }
            if now >= self.deadline {
                return stop(Cause::Wall, "wall-clock limit");
            }
            if now.duration_since(self.last) >= self.idle {
                return stop(Cause::Idle, "idle limit");
            }
            let wait = (self.deadline - now)
                .min(self.idle - now.duration_since(self.last))
                .min(Duration::from_millis(100))
                .max(Duration::from_millis(1));
            self.sock.set_read_timeout(Some(wait))?;
            match self.sock.read(buf) {
                Ok(n) => {
                    self.last = Instant::now();
                    return Ok(n);
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e),
            }
        }
    }
}

fn kill_group(pgid: i32) {
    // SAFETY: SIGKILL to the process group this client created for the worker.
    unsafe { libc::kill(-pgid, libc::SIGKILL) };
}

/// Waits (without reaping) until the process has exited, up to `grace`.
fn exited_within(pid: i32, grace: Duration) -> bool {
    let end = Instant::now() + grace;
    loop {
        // SAFETY: waitid with WNOHANG|WNOWAIT on our own child: observes, does not reap.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let r = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if r == 0 {
            // SAFETY: reading the pid field the kernel filled (zero when still running).
            if unsafe { info.si_pid() } != 0 {
                return true;
            }
        } else {
            return true; // no such child: nothing to wait for
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
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

/// Descriptors placed in the child between fork and exec: `(source, target)`.
fn place(pairs: &[(i32, i32)]) -> std::io::Result<()> {
    let mut staged = Vec::new();
    for &(from, _) in pairs {
        // SAFETY: fcntl on a descriptor of this process.
        let t = unsafe { libc::fcntl(from, libc::F_DUPFD, 500) };
        if t < 0 {
            return Err(std::io::Error::last_os_error());
        }
        staged.push(t);
    }
    for (t, &(_, to)) in staged.iter().zip(pairs) {
        // SAFETY: dup2 onto the target number (clears close-on-exec).
        if unsafe { libc::dup2(*t, to) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    for t in staged {
        // SAFETY: closing the staging copies.
        unsafe { libc::close(t) };
    }
    Ok(())
}

fn proc_status_field(pid: u32, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix(key))
        .map(|v| v.trim().to_string())
}

fn spawn(
    cfg: &WorkerConfig,
    child_end: &UnixStream,
    root: &OwnedFd,
) -> Result<std::process::Child, WorkerError> {
    let mut cmd = Command::new(&cfg.exe);
    cmd.args(&cfg.args)
        .env_clear()
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let (chan, rootfd) = (child_end.as_raw_fd(), root.as_raw_fd());
    let rlimit_as = cfg.rlimit_as_mib;
    // SAFETY: only async-signal-safe calls between fork and exec (setpgid, prctl, setrlimit,
    // fcntl, dup2, close, close_range).
    unsafe {
        cmd.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // the worker dies with its parent
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
            place(&[(chan, CHANNEL_FD), (rootfd, ROOT_FD)])?;
            // Other inherited descriptors are NOT closed here: closing them would also close the
            // pipe that std uses to report a failed exec (the child would abort and the parent would
            // think the worker started). The worker closes everything above 4 itself, first thing.
            Ok(())
        });
    }
    cmd.spawn().map_err(|e| WorkerError::Spawn(e.to_string()))
}

/// Runs one directory extraction in an isolated worker and returns the validated result.
/// `anchor` is the verified root the caller is entitled to; the worker receives only a duplicate
/// of its descriptor. The caller persists the result.
pub fn extract_directory(
    cfg: &WorkerConfig,
    anchor: &Anchor,
    spaces: &[Uuid],
    ctx: &dyn ContextSource,
    cancel: &AtomicBool,
) -> Result<(Extracted, WorkerStats), WorkerError> {
    let begun = Instant::now();
    // the parent decides what may be asked: spaces are checked here, before anything is started
    validate_spaces(spaces).map_err(WorkerError::Context)?;
    for id in spaces {
        match ctx.space_exists(*id) {
            Ok(true) => {}
            Ok(false) => return Err(WorkerError::Context(format!("space {id} does not exist"))),
            Err(e) => return Err(WorkerError::Context(format!("checking space {id}: {e}"))),
        }
    }
    let (parent_end, child_end) =
        UnixStream::pair().map_err(|e| WorkerError::Spawn(format!("socketpair: {e}")))?;
    let root = anchor
        .dup_fd()
        .map_err(|e| WorkerError::Spawn(format!("root descriptor: {e}")))?;
    let mut child = spawn(cfg, &child_end, &root)?;
    drop(child_end);
    drop(root);
    let pid = child.id();
    let pgid = pid as i32;
    if let Some(o) = &cfg.observe_pid {
        o.store(pid, Ordering::SeqCst);
    }
    let mut stats = WorkerStats {
        pid,
        ..Default::default()
    };
    let stderr = child.stderr.take();

    let outcome = run_session(
        cfg,
        anchor.real(),
        Uuid::now_v7(),
        None,
        spaces,
        ctx,
        cancel,
        parent_end,
        begun,
        pid,
        &mut stats,
    );

    // Did the worker end by itself? (Asked before we end it; a stream that stops short usually
    // means it did, and the close of its channel can come a moment before it is waitable.)
    let stopped_short = matches!(&outcome, Err(WorkerError::Stream(m)) if m.contains("ended") || m.contains("truncated"));
    let ended_by_itself = match &outcome {
        Ok(_) => exited_within(pgid, cfg.exit_grace),
        Err(_) => exited_within(
            pgid,
            if stopped_short {
                Duration::from_millis(500)
            } else {
                Duration::ZERO
            },
        ),
    };
    let clean = outcome.is_ok() && ended_by_itself;
    // Always: the whole group ends, then the leader is reaped.
    kill_group(pgid);
    let mut status = 0i32;
    // SAFETY: wait4 on our own child; fills the status and the resource usage.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::wait4(pgid, &mut status, 0, &mut ru) };
    stats.worker_peak_rss_kb = ru.ru_maxrss;
    stats.exit = describe(status);
    stats.total = begun.elapsed();
    // What the worker said, at most 8 KiB, without waiting: a descendant that left the process
    // group may still hold the write end of the pipe, and waiting for its end would hang us.
    let mut err_text = String::new();
    if let Some(mut e) = stderr {
        use std::os::fd::AsRawFd;
        // SAFETY: switching our own pipe end to non-blocking mode.
        unsafe {
            let fl = libc::fcntl(e.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(e.as_raw_fd(), libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
        let mut buf = vec![0u8; 8192];
        let mut got = 0;
        while got < buf.len() {
            match e.read(&mut buf[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(_) => break, // would block: nothing more right now
            }
        }
        err_text = String::from_utf8_lossy(&buf[..got]).trim().to_string();
    }
    match outcome {
        Ok(extracted) => {
            if clean && libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
                Ok((extracted, stats))
            } else {
                Err(WorkerError::Died {
                    status: stats.exit.clone(),
                    stderr: err_text,
                })
            }
        }
        Err(WorkerError::Stream(m)) if ended_by_itself => {
            // a stream that stops short because the worker ended: say how it ended
            Err(WorkerError::Died {
                status: format!("{} ({m})", stats.exit),
                stderr: err_text,
            })
        }
        // the session itself saw the worker go: add how the process ended and what it said
        Err(WorkerError::Died { status, .. }) => Err(WorkerError::Died {
            status: format!("{status} [{}]", stats.exit),
            stderr: err_text,
        }),
        Err(e) => Err(e),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_session(
    cfg: &WorkerConfig,
    root: &std::path::Path,
    op: Uuid,
    observed: Option<&Observed>,
    spaces: &[Uuid],
    ctx: &dyn ContextSource,
    cancel: &AtomicBool,
    parent_end: UnixStream,
    begun: Instant,
    pid: u32,
    stats: &mut WorkerStats,
) -> Result<Extracted, WorkerError> {
    let cause = Rc::new(Cell::new(None));
    let writer = parent_end
        .try_clone()
        .map_err(|e| WorkerError::Io(e.to_string()))?;
    writer
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| WorkerError::Io(e.to_string()))?;
    let mut reader = BufReader::with_capacity(
        256 * 1024,
        GuardedReader {
            sock: parent_end,
            deadline: begun + cfg.wall_timeout,
            idle: cfg.idle_timeout,
            last: Instant::now(),
            cancel,
            cause: cause.clone(),
        },
    );
    let by_cause = |fallback: WorkerError| match cause.get() {
        Some(Cause::Cancelled) => WorkerError::Cancelled,
        Some(Cause::Wall) => WorkerError::Timeout("wall clock"),
        Some(Cause::Idle) => WorkerError::Timeout("no data from the worker"),
        None => fallback,
    };

    // 1. the hello: one line, then the confinement is checked from outside as well
    let mut line = Vec::new();
    let n = (&mut reader)
        .take(MAX_HELLO as u64 + 1)
        .read_until(b'\n', &mut line);
    match n {
        Ok(0) => {
            return Err(by_cause(WorkerError::Died {
                status: "closed the channel before saying hello".into(),
                stderr: String::new(),
            }))
        }
        Ok(_) if line.last() == Some(&b'\n') => {}
        Ok(_) => {
            return Err(WorkerError::Sandbox(
                "the hello is too long or unterminated".into(),
            ))
        }
        Err(e) => return Err(by_cause(WorkerError::Io(e.to_string()))),
    }
    if cfg.require_sandbox_evidence {
        let hello: Hello = serde_json::from_slice(&line)
            .map_err(|e| WorkerError::Sandbox(format!("unreadable hello: {e}")))?;
        verify_hello(&hello, pid, cfg.verify_via_proc && observed.is_none())?;
        if let Some(o) = observed {
            // the broker looked at this very process from outside (it is not the worker's word)
            if o.pid != hello.pid {
                return Err(WorkerError::Sandbox(
                    "the broker observed another process than the one that said hello".into(),
                ));
            }
            if o.seccomp != "2" || o.no_new_privs != "1" || o.tracer_pid != "0" {
                return Err(WorkerError::Sandbox(format!(
                    "the broker's observation does not show the confinement: {o:?}"
                )));
            }
        }
        stats.hello = Some(hello);
    }
    stats.startup = begun.elapsed();

    // 2. the request: what to extract, under which limits
    let request = Request {
        v: PROTOCOL,
        op,
        root: root.display().to_string(),
        spaces: spaces.to_vec(),
        max_blob_bytes: cfg.producer.max_blob_bytes,
        max_source_bytes: cfg.producer.parse_policy.max_source_bytes,
        max_entity_depth: cfg.producer.parse_policy.max_entity_depth,
        max_output_bytes: cfg.producer.parse_policy.max_output_bytes,
    };
    let mut bytes = serde_json::to_vec(&request).map_err(|e| WorkerError::Io(e.to_string()))?;
    bytes.push(b'\n');
    (&writer)
        .write_all(&bytes)
        .map_err(|e| WorkerError::Io(format!("sending the request: {e}")))?;

    // 3. the context exchange: the worker lists the blobs it would parse, the parent answers with
    //    the derivations it already has (nothing else about its database)
    let mut line = Vec::new();
    match (&mut reader)
        .take(MAX_MANIFEST_LINE as u64 + 1)
        .read_until(b'\n', &mut line)
    {
        Ok(0) => {
            return Err(by_cause(WorkerError::Died {
                status: "closed the channel before the manifest".into(),
                stderr: String::new(),
            }))
        }
        Ok(_) if line.last() == Some(&b'\n') => {}
        Ok(_) if line.len() > MAX_MANIFEST_LINE => {
            return Err(WorkerError::Context("the manifest is too long".into()))
        }
        Ok(_) => {
            return Err(by_cause(WorkerError::Died {
                status: "the manifest is truncated".into(),
                stderr: String::new(),
            }))
        }
        Err(e) => return Err(by_cause(WorkerError::Io(e.to_string()))),
    }
    let manifest: Manifest = serde_json::from_slice(&line)
        .map_err(|e| WorkerError::Context(format!("unreadable manifest: {e}")))?;
    manifest.validate(op).map_err(WorkerError::Context)?;
    let policy = cfg.producer.parse_policy;
    let context = answer(ctx, op, &policy, &manifest)
        .map_err(|e| WorkerError::Context(format!("answering the manifest: {e}")))?;
    stats.manifest_entries = manifest.entries.len() as u64;
    stats.known_sent = context.known.len() as u64;
    let mut bytes = serde_json::to_vec(&context).map_err(|e| WorkerError::Io(e.to_string()))?;
    bytes.push(b'\n');
    (&writer)
        .write_all(&bytes)
        .map_err(|e| by_cause(WorkerError::Io(format!("sending the context: {e}"))))?;

    // 4. the stream, validated record by record; nothing is returned unless it is complete
    let extracted = match consume(
        &mut reader,
        op,
        Kind::Files,
        Some(root),
        &cfg.stream,
        Some(cancel),
    ) {
        Ok((extracted, st)) => {
            stats.frames = st.frames;
            stats.bytes = st.bytes;
            stats.records = st.records;
            extracted
        }
        Err(e) => return Err(by_cause(WorkerError::Stream(e.to_string()))),
    };
    // 5. the result against what was asked, still before anything is persisted
    if let Extracted::Files(files) = &extracted {
        check_result(files, op, spaces, &policy, &context).map_err(WorkerError::Stream)?;
    }
    Ok(extracted)
}

fn verify_hello(h: &Hello, pid: u32, via_proc: bool) -> Result<(), WorkerError> {
    let bad = |m: String| Err(WorkerError::Sandbox(m));
    if h.v != PROTOCOL {
        return bad(format!("protocol {}", h.v));
    }
    if h.pid != pid {
        return bad(format!(
            "the hello names pid {} but the child is {pid}",
            h.pid
        ));
    }
    if h.sandbox.seccomp != "enforced" || h.sandbox.probes.is_empty() {
        return bad("the worker does not report an enforced socket filter".into());
    }
    if !via_proc {
        return Ok(());
    }
    // observed from outside, not taken from the worker's word
    match proc_status_field(pid, "Seccomp:").as_deref() {
        Some("2") => {}
        other => {
            return bad(format!(
                "/proc says Seccomp: {other:?}, expected 2 (filter mode)"
            ))
        }
    }
    match proc_status_field(pid, "NoNewPrivs:").as_deref() {
        Some("1") => {}
        other => return bad(format!("/proc says NoNewPrivs: {other:?}, expected 1")),
    }
    match proc_status_field(pid, "TracerPid:").as_deref() {
        Some("0") => {}
        other => return bad(format!("the worker is traced: {other:?}")),
    }
    // Not checked from outside: the worker is not dumpable, so its /proc/<pid>/fd is closed to us
    // (by design). Its descriptor table is covered by the pre-exec close_range and the probes.
    Ok(())
}

/// What the broker reported when the operation ended.
#[derive(Debug, Clone)]
pub struct BrokerReport {
    pub observed: Observed,
    /// Where the broker took the authority from (derived by the broker, never by the caller).
    pub authority: AuthoritySource,
    pub ended_by: EndedBy,
    pub exit: String,
    pub exited_by_itself: bool,
    pub descendants_killed: u32,
    pub peak_rss_kb: i64,
    pub cgroup_peak_bytes: Option<u64>,
}

/// Runs one directory extraction through the broker. The frontend names the root (a string the
/// broker matches against its own authorized roots) and receives the worker's channel; it never
/// holds the root descriptor and cannot widen what the broker grants. The broker ends and reaps
/// the worker; this side validates the stream and returns it. The caller persists after `Ok`.
pub fn extract_via_broker(
    cfg: &WorkerConfig,
    link: &UnixStream,
    root: &std::path::Path,
    spaces: &[Uuid],
    ctx: &dyn ContextSource,
    cancel: &AtomicBool,
) -> Result<(Extracted, WorkerStats, BrokerReport), WorkerError> {
    let begun = Instant::now();
    validate_spaces(spaces).map_err(WorkerError::Context)?;
    for id in spaces {
        match ctx.space_exists(*id) {
            Ok(true) => {}
            Ok(false) => return Err(WorkerError::Context(format!("space {id} does not exist"))),
            Err(e) => return Err(WorkerError::Context(format!("checking space {id}: {e}"))),
        }
    }
    let op = Uuid::now_v7();
    let send = |m: &ToBroker| -> Result<(), WorkerError> {
        let mut b = serde_json::to_vec(m).map_err(|e| WorkerError::Io(e.to_string()))?;
        b.push(b'\n');
        (&*link).write_all(&b).map_err(|e| {
            WorkerError::Refused(format!(
                "the broker is not available (it never started or has exited): {e}"
            ))
        })
    };
    send(&ToBroker::Begin {
        v: BROKER_PROTOCOL,
        op,
        kind: OpKind::DirectoryExtract,
        root: root.display().to_string(),
        spaces: spaces.to_vec(),
    })?;
    link.set_read_timeout(Some(cfg.wall_timeout))
        .map_err(|e| WorkerError::Io(e.to_string()))?;
    let (bytes, fd) =
        recv_with_fd(link).map_err(|e| WorkerError::Io(format!("the broker: {e}")))?;
    if bytes.is_empty() {
        // end of file: the broker (or its watchdog) is gone. Explicit, never a silent fallback to
        // an in-process or unconfined extraction.
        return Err(WorkerError::Refused(
            "the broker is not available (it never started or has exited)".into(),
        ));
    }
    let text = String::from_utf8_lossy(&bytes);
    let ready: FromBroker = serde_json::from_str(text.trim_end())
        .map_err(|e| WorkerError::Io(format!("unreadable answer from the broker: {e}")))?;
    let (observed, got_spaces, got_root, authority) = match ready {
        FromBroker::Refused { reason } => return Err(WorkerError::Refused(reason)),
        FromBroker::Ready {
            v,
            op: o,
            spaces: s,
            root: r,
            observed,
            authority,
        } if v == BROKER_PROTOCOL && o == op => (observed, s, r, authority),
        other => {
            return Err(WorkerError::Io(format!(
                "the broker answered with something else than Ready for this operation: {other:?}"
            )))
        }
    };
    let Some(fd) = fd else {
        return Err(WorkerError::Io("Ready without a channel".into()));
    };
    // the broker's answer must be the operation that was asked for, nothing else
    if got_spaces != spaces || std::path::Path::new(&got_root) != root {
        return Err(WorkerError::Io(
            "the broker bound the operation to another root or spaces".into(),
        ));
    }
    let channel = UnixStream::from(fd);
    let pid = observed.pid;
    let mut stats = WorkerStats {
        pid,
        ..Default::default()
    };
    if let Some(o) = &cfg.observe_pid {
        o.store(pid, Ordering::SeqCst);
    }
    let outcome = run_session(
        cfg,
        root,
        op,
        Some(&observed),
        spaces,
        ctx,
        cancel,
        channel,
        begun,
        pid,
        &mut stats,
    );
    // tell the broker how it ended; it ends the worker and everything below it either way
    let _ = send(&if outcome.is_ok() {
        ToBroker::Done
    } else {
        ToBroker::Cancel
    });
    link.set_read_timeout(Some(cfg.exit_grace + Duration::from_secs(15)))
        .map_err(|e| WorkerError::Io(e.to_string()))?;
    let mut reader = BufReader::new(
        link.try_clone()
            .map_err(|e| WorkerError::Io(e.to_string()))?,
    );
    let mut line = Vec::new();
    let read = (&mut reader)
        .take(MAX_LINE as u64 + 1)
        .read_until(b'\n', &mut line);
    let broker_gone = matches!(read, Ok(0));
    let fin = read
        .ok()
        .filter(|n| *n > 0 && line.last() == Some(&b'\n'))
        .and_then(|_| serde_json::from_slice::<FromBroker>(&line).ok());
    stats.total = begun.elapsed();
    let Some(FromBroker::Finished {
        op: fop,
        exit,
        ended_by,
        exited_by_itself,
        descendants_killed,
        peak_rss_kb,
        cgroup_peak_bytes,
        ..
    }) = fin
    else {
        if broker_gone {
            // the broker (or its watchdog) ended during the operation: explicit, not a worker death
            return Err(WorkerError::Refused(
                "the broker is not available (it exited during the operation)".into(),
            ));
        }
        return Err(outcome.err().unwrap_or(WorkerError::Io(
            "the broker did not report how the worker ended".into(),
        )));
    };
    if fop != op {
        return Err(WorkerError::Io(
            "the broker's report is for another operation".into(),
        ));
    }
    stats.exit = exit.clone();
    stats.worker_peak_rss_kb = peak_rss_kb;
    // The broker's word comes first: a revoked operation is never accepted, whatever the stream
    // looked like, and a broker timeout is a timeout, not a dead worker.
    if ended_by == EndedBy::Revoked {
        return Err(WorkerError::Revoked);
    }
    if ended_by == EndedBy::Timeout {
        return Err(WorkerError::Timeout("broker wall clock"));
    }
    let report = BrokerReport {
        observed,
        authority,
        ended_by,
        exit: exit.clone(),
        exited_by_itself,
        descendants_killed,
        peak_rss_kb,
        cgroup_peak_bytes,
    };
    match outcome {
        Ok(extracted) if ended_by == EndedBy::Done && exited_by_itself && exit == "exit code 0" => {
            Ok((extracted, stats, report))
        }
        Ok(_) => Err(WorkerError::Died {
            status: exit,
            stderr: String::new(),
        }),
        Err(WorkerError::Stream(m)) if exited_by_itself => Err(WorkerError::Died {
            status: format!("{exit} ({m})"),
            stderr: String::new(),
        }),
        Err(e) => Err(e),
    }
}
