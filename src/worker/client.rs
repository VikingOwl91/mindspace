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
            observe_pid: None,
        }
    }
}

#[derive(Debug)]
pub enum WorkerError {
    Spawn(String),
    /// The worker is not confined as required (or said nothing sensible); it was killed.
    Sandbox(String),
    /// The hard wall-clock limit or the idle limit expired; the worker's group was killed.
    Timeout(&'static str),
    Cancelled,
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
            Self::Sandbox(m) => write!(f, "the extraction worker is not confined: {m}"),
            Self::Timeout(w) => write!(f, "the extraction worker timed out ({w})"),
            Self::Cancelled => write!(f, "the extraction was cancelled"),
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
            // every other descriptor above the two: closed
            if libc::syscall(libc::SYS_close_range, 5u32, u32::MAX, 0u32) != 0 {
                for fd in 5..1024 {
                    libc::close(fd);
                }
            }
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
    cancel: &AtomicBool,
) -> Result<(Extracted, WorkerStats), WorkerError> {
    let begun = Instant::now();
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
        cfg, anchor, spaces, cancel, parent_end, begun, pid, &mut stats,
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
    let mut err_text = String::new();
    if let Some(mut e) = stderr {
        let mut buf = Vec::new();
        let _ = (&mut e).take(8192).read_to_end(&mut buf);
        err_text = String::from_utf8_lossy(&buf).trim().to_string();
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
        Err(e) => Err(e),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_session(
    cfg: &WorkerConfig,
    anchor: &Anchor,
    spaces: &[Uuid],
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
        verify_hello(&hello, pid)?;
        stats.hello = Some(hello);
    }
    stats.startup = begun.elapsed();

    // 2. the request: what to extract, under which limits
    let op = Uuid::now_v7();
    let request = Request {
        v: PROTOCOL,
        op,
        root: anchor.real().display().to_string(),
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

    // 3. the stream, validated record by record; nothing is returned unless it is complete
    match consume(
        &mut reader,
        op,
        Kind::Files,
        Some(anchor.real()),
        &cfg.stream,
        Some(cancel),
    ) {
        Ok((extracted, st)) => {
            stats.frames = st.frames;
            stats.bytes = st.bytes;
            stats.records = st.records;
            Ok(extracted)
        }
        Err(e) => Err(by_cause(WorkerError::Stream(e.to_string()))),
    }
}

fn verify_hello(h: &Hello, pid: u32) -> Result<(), WorkerError> {
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
