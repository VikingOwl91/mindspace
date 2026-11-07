//! Running a client's own command line (`claude mcp ...`) with a deadline.
//!
//! Always an argument vector, never a shell: paths with spaces, quotes or `$`
//! reach the client unchanged. The child is killed at the deadline; its output
//! comes over channels (no thread is joined), so a grandchild that keeps a pipe
//! open cannot stretch the deadline.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct Out {
    /// `None` if the process was killed or has no exit code.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl Out {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }
    /// stdout and stderr, for matching messages.
    pub fn text(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

const LIMIT: u64 = 256 * 1024;

fn drain<R: Read + Send + 'static>(mut r: R) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut r).take(LIMIT).read_to_end(&mut buf);
        let mut sink = [0u8; 4096];
        while matches!(r.read(&mut sink), Ok(n) if n > 0) {}
        let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
    });
    rx
}

pub fn run(program: &Path, args: &[String], timeout: Duration) -> std::io::Result<Out> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let out = drain(child.stdout.take().expect("piped"));
    let err = drain(child.stderr.take().expect("piped"));
    let deadline = Instant::now() + timeout;
    let (mut code, mut timed_out) = (None, false);
    loop {
        match child.try_wait()? {
            Some(status) => {
                code = status.code();
                break;
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                timed_out = true;
                break;
            }
            None => std::thread::sleep(Duration::from_millis(15)),
        }
    }
    let grace = Duration::from_secs(2);
    Ok(Out {
        code,
        stdout: out.recv_timeout(grace).unwrap_or_default(),
        stderr: err.recv_timeout(grace).unwrap_or_default(),
        timed_out,
    })
}
