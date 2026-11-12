//! Sandbox self-test for `mind doctor` (Linux).
//!
//! It exercises the REAL server path instead of re-implementing any security
//! logic: a child `mind mcp` is started WITHOUT `--insecure-no-sandbox`, with a
//! temporary database and one temporary read-write root, and is asked two
//! things over MCP:
//!
//!  1. `repo_outline` inside the root: allowed, and the `sandbox` field of the
//!     result says what the server established at start;
//!  2. `repo_outline` outside the root (also through `..`): refused.
//!
//! What this proves, and what it does not:
//!  - KERNEL evidence is only the server's own report. The server restricts
//!    itself with Landlock, then verifies the restriction by probes that must be
//!    denied (canary files, descriptors; ADR-029) before it says `enforced`.
//!    The doctor cannot observe the kernel state of another process from outside.
//!  - The refusal of an outside path is APPLICATION-layer evidence: the
//!    allowlist rejects it before the kernel is involved. It shows the tool
//!    behaves as documented; it does not by itself prove the kernel sandbox.
//!
//! Only temporary paths are used (a private directory under the system temp
//! dir, removed on every exit path), the production database is never opened,
//! and there is no network access.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// What the child server said about the kernel sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelEvidence {
    /// `sandbox: enforced (...)`: the server confined itself and verified it.
    Enforced(String),
    /// The repository tools are off (fail closed); the reason as reported.
    Unavailable(String),
    /// `DISABLED ...`: must never happen, the self-test does not pass the opt-out.
    Disabled(String),
    /// The server did not give a usable answer (exited early, odd result).
    NotReported(String),
}

/// Outcome of a completed self-test run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selftest {
    pub kernel: KernelEvidence,
    /// `repo_outline` inside the temporary root succeeded.
    pub inside_allowed: Option<bool>,
    /// `repo_outline` on a sibling directory outside the root was refused.
    pub outside_refused: Option<bool>,
    /// The same for a path that climbs out of the root with `..`.
    pub traversal_refused: Option<bool>,
    /// The refusal text of the outside request (application-layer message).
    pub outside_message: Option<String>,
    /// The typed `seccomp` field of the server's `repo_outline` result, if any.
    pub seccomp: Option<Value>,
    pub elapsed_ms: u128,
}

/// The self-test could not be carried out (as opposed to: it ran and the
/// sandbox is unavailable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelftestError {
    Setup(String),
    Spawn(String),
    Timeout(Duration),
    Protocol(String),
}

impl std::fmt::Display for SelftestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Setup(d) => write!(f, "could not prepare the self-test: {d}"),
            Self::Spawn(d) => write!(f, "could not start the server for the self-test: {d}"),
            Self::Timeout(t) => write!(
                f,
                "the server did not answer within {} s; it was stopped",
                t.as_secs_f32()
            ),
            Self::Protocol(d) => write!(f, "unexpected server behaviour: {d}"),
        }
    }
}

/// A private temporary directory that is removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn create_in(base: &Path) -> std::io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        // `create` (not `create_dir_all`) fails if the name exists, and mode 0700
        // keeps other users out; a pre-planted symlink with this name is refused.
        let path = base.join(format!("mind-doctor-{}-{nanos}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Owns the child process: it is killed and reaped on every exit path.
///
/// The reader threads are deliberately NOT joined. A grandchild that inherited
/// the pipes would keep them open after the child is dead, and a join would then
/// block far beyond the deadline. The threads end by themselves at EOF and hold
/// no resource but their pipe end.
struct ChildGuard {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const STDERR_LIMIT: usize = 4096;

struct Client {
    guard: ChildGuard,
    lines: Receiver<String>,
    /// The first `STDERR_LIMIT` bytes of the child's stderr, delivered at EOF.
    stderr: Receiver<String>,
    next_id: u64,
    deadline: Instant,
    timeout: Duration,
}

impl Client {
    /// Starts `cmd` with piped stdio and the reader threads.
    fn spawn(
        mut cmd: Command,
        exe: &Path,
        started: Instant,
        timeout: Duration,
    ) -> Result<Self, SelftestError> {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| SelftestError::Spawn(format!("{}: {e}", exe.display())))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let (tx, lines) = mpsc::channel::<String>();
        let (etx, stderr) = mpsc::channel::<String>();
        if let Some(out) = stdout {
            std::thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        if let Some(mut err) = stderr_pipe {
            std::thread::spawn(move || {
                // Keep the head, but keep draining: a child that is blocked on a
                // full stderr pipe must not be mistaken for a hung one.
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while let Ok(n) = err.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let room = STDERR_LIMIT.saturating_sub(head.len());
                    head.extend_from_slice(&buf[..n.min(room)]);
                }
                let _ = etx.send(String::from_utf8_lossy(&head).trim().to_string());
            });
        }
        Ok(Self {
            guard: ChildGuard { child, stdin },
            lines,
            stderr,
            next_id: 0,
            deadline: started + timeout,
            timeout,
        })
    }

    /// Writes one JSON line. `Ok(false)` means the server is gone (its stdin is
    /// closed): that is a finding about the server, not a failure of the test.
    fn send(&mut self, v: &Value) -> Result<bool, SelftestError> {
        let Some(stdin) = self.guard.stdin.as_mut() else {
            return Ok(false);
        };
        match writeln!(stdin, "{v}").and_then(|_| stdin.flush()) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(false),
            Err(e) => Err(SelftestError::Protocol(format!(
                "writing to the server failed: {e}"
            ))),
        }
    }

    /// Sends a request and waits for its answer. `Ok(None)` means the server
    /// closed its output or its input (exited) before answering.
    fn request(&mut self, method: &str, params: Value) -> Result<Option<Value>, SelftestError> {
        self.next_id += 1;
        let id = self.next_id;
        if !self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))? {
            return Ok(None);
        }
        loop {
            let left = self.deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    let Ok(v) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    if v.get("id") == Some(&json!(id)) {
                        return Ok(Some(v));
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(SelftestError::Timeout(self.timeout))
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
            }
        }
    }

    /// What the child wrote to stderr (empty if it does not finish within 2 s).
    fn stderr_text(&self) -> String {
        self.stderr
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_default()
    }
}

fn outline(c: &mut Client, path: &Path) -> Result<Option<(bool, String)>, SelftestError> {
    let r = c.request(
        "tools/call",
        json!({"name":"repo_outline","arguments":{"path":path.display().to_string()}}),
    )?;
    Ok(r.map(|v| {
        let res = &v["result"];
        let text = res["content"][0]["text"].as_str().unwrap_or("").to_string();
        (res["isError"].as_bool().unwrap_or(true), text)
    }))
}

/// A refusal counts only if it is the allowlist's message. Any other error (a
/// crash, a different failure) must not pass as "the outside path was refused".
fn is_allowlist_refusal(is_error: bool, text: &str) -> bool {
    is_error && text.contains("outside the allowed roots")
}

fn classify(sandbox_field: &str) -> KernelEvidence {
    let s = sandbox_field.trim();
    if s.starts_with("enforced") {
        KernelEvidence::Enforced(s.to_string())
    } else if s.starts_with("DISABLED") {
        KernelEvidence::Disabled(s.to_string())
    } else if s.starts_with("unavailable") {
        KernelEvidence::Unavailable(s.to_string())
    } else {
        KernelEvidence::NotReported(format!("unexpected sandbox state `{s}`"))
    }
}

/// Runs the self-test with the system temp directory.
pub fn run(exe: &Path, timeout: Duration) -> Result<Selftest, SelftestError> {
    run_in(&std::env::temp_dir(), exe, timeout)
}

/// Runs the self-test with temporary files under `tmp_base` (tests use their own).
pub fn run_in(tmp_base: &Path, exe: &Path, timeout: Duration) -> Result<Selftest, SelftestError> {
    let started = Instant::now();
    let tmp = TempDir::create_in(tmp_base).map_err(|e| {
        SelftestError::Setup(format!(
            "temporary directory in {}: {e}",
            tmp_base.display()
        ))
    })?;
    let base = std::fs::canonicalize(&tmp.0).map_err(|e| SelftestError::Setup(e.to_string()))?;
    let (dbdir, root, outside) = (base.join("db"), base.join("root"), base.join("outside"));
    let probe = root.join("probe");
    for d in [&dbdir, &root, &outside, &probe] {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(d)
            .map_err(|e| SelftestError::Setup(format!("{}: {e}", d.display())))?;
    }
    std::fs::write(probe.join("note.md"), "# probe\n")
        .map_err(|e| SelftestError::Setup(e.to_string()))?;
    let db = dbdir.join("mind.db");
    crate::initialize(&db).map_err(|e| SelftestError::Setup(format!("temporary database: {e}")))?;

    let mut cmd = Command::new(exe);
    cmd.arg("--db")
        .arg(&db)
        .arg("mcp")
        .arg("--allow-root")
        .arg(format!("{}:rw", root.display()))
        .arg("--ingest-timeout")
        .arg("30")
        // The user's own configuration must not leak into the test, and no
        // sandbox marker may be inherited.
        .env_remove("MINDSPACE_DB")
        .env_remove("MINDSPACE_MCP_ALLOW_ROOTS")
        .env_remove("MINDSPACE_MCP_INGEST_TIMEOUT")
        .env_remove(crate::sandbox::ENV_STAGE2)
        .env_remove(crate::sandbox::ENV_CANARY);
    let mut c = Client::spawn(cmd, exe, started, timeout)?;

    let early_exit = |c: &Client, what: &str| Selftest {
        kernel: KernelEvidence::NotReported(format!(
            "the server exited before answering {what}: {}",
            c.stderr_text()
        )),
        inside_allowed: None,
        outside_refused: None,
        traversal_refused: None,
        outside_message: None,
        seccomp: None,
        elapsed_ms: started.elapsed().as_millis(),
    };

    let init = c.request(
        "initialize",
        json!({"protocolVersion": crate::mcp::MCP_PROTOCOL_VERSION,
               "capabilities": {}, "clientInfo": {"name": "mind-doctor", "version": env!("CARGO_PKG_VERSION")}}),
    )?;
    if init.is_none() {
        return Ok(early_exit(&c, "initialize"));
    }
    // If the server is gone already, the next request reports it.
    c.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))?;

    let Some((inside_err, inside_text)) = outline(&mut c, &probe)? else {
        return Ok(early_exit(&c, "repo_outline"));
    };
    let seccomp = if inside_err {
        None
    } else {
        serde_json::from_str::<Value>(&inside_text)
            .ok()
            .map(|v| v["seccomp"].clone())
            .filter(|v| !v.is_null())
    };
    let kernel = if inside_err {
        if inside_text.contains("repository tools are disabled") {
            KernelEvidence::Unavailable(inside_text.clone())
        } else {
            KernelEvidence::NotReported(format!(
                "repo_outline inside the root failed: {inside_text}"
            ))
        }
    } else {
        match serde_json::from_str::<Value>(&inside_text)
            .ok()
            .and_then(|v| v["sandbox"].as_str().map(str::to_string))
        {
            Some(s) => classify(&s),
            None => KernelEvidence::NotReported("the result has no `sandbox` field".into()),
        }
    };

    // Refusals are only meaningful when the tools are on.
    let (mut outside_refused, mut traversal_refused, mut outside_message) = (None, None, None);
    if !inside_err {
        if let Some((e, t)) = outline(&mut c, &outside)? {
            outside_refused = Some(is_allowlist_refusal(e, &t));
            outside_message = Some(t);
        }
        let climb = root.join("..").join("outside");
        if let Some((e, t)) = outline(&mut c, &climb)? {
            traversal_refused = Some(is_allowlist_refusal(e, &t));
        }
    }
    Ok(Selftest {
        kernel,
        inside_allowed: Some(!inside_err),
        outside_refused,
        traversal_refused,
        outside_message,
        seccomp,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sandbox_field_is_classified_by_its_prefix() {
        assert!(matches!(
            classify("enforced (Landlock, ABI 3 or newer)"),
            KernelEvidence::Enforced(_)
        ));
        assert!(matches!(
            classify("unavailable (no landlock)"),
            KernelEvidence::Unavailable(_)
        ));
        assert!(matches!(
            classify("DISABLED by --insecure-no-sandbox (application layer only)"),
            KernelEvidence::Disabled(_)
        ));
        assert!(matches!(
            classify("not needed"),
            KernelEvidence::NotReported(_)
        ));
        assert!(matches!(classify(""), KernelEvidence::NotReported(_)));
    }

    #[test]
    fn only_the_allowlist_message_counts_as_a_refusal() {
        assert!(is_allowlist_refusal(
            true,
            "storage error: path is not accessible or outside the allowed roots (x)"
        ));
        assert!(!is_allowlist_refusal(true, "internal error: boom"));
        assert!(!is_allowlist_refusal(false, "outside the allowed roots"));
    }

    #[test]
    fn a_server_that_already_exited_is_reported_as_gone_not_as_a_protocol_error() {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("exit 0");
        let mut c =
            Client::spawn(cmd, Path::new("sh"), Instant::now(), Duration::from_secs(5)).unwrap();
        let _ = c.guard.child.wait(); // really gone: nobody reads its stdin any more
        assert!(matches!(c.request("ping", json!({})), Ok(None)));
    }

    #[test]
    fn a_planted_name_in_the_temp_directory_is_refused_not_followed() {
        let base = std::env::temp_dir().join(format!("mind-doctor-unit-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let t = TempDir::create_in(&base).unwrap();
        let name = t.0.clone();
        // Creating the same name again must fail instead of reusing it.
        use std::os::unix::fs::DirBuilderExt;
        assert!(std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&name)
            .is_err());
        drop(t);
        assert!(!name.exists(), "the directory is removed on drop");
        let _ = std::fs::remove_dir_all(&base);
    }
}
