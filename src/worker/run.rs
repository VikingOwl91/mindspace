//! The worker side: `mind extraction-worker`. Runs in a freshly exec'd process whose only
//! descriptors are stdio, the channel (3) and the root directory (4).

use std::io::{BufWriter, Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use crate::core::{Error, Result, Uuid};
use crate::ingest::confined::{Anchor, Mode, PathPolicy};
use crate::ingest::deadline::Deadline;
use crate::ingest::extract::ExtractionContext;
use crate::ingest::stream::{stream_directory, FrameWriter, ProducerLimits};

use super::{Hello, Request, CHANNEL_FD, MAX_REQUEST, PROTOCOL, ROOT_FD};

/// The application-layer label of the one root. The kernel rule is read-only; this only lets the
/// anchored reads proceed (they ask for a "writable" mode because the in-process ingest needs one).
struct WorkerPolicy(PathBuf);

impl PathPolicy for WorkerPolicy {
    fn governing(&self, real: &std::path::Path) -> Option<(PathBuf, Mode)> {
        real.starts_with(&self.0)
            .then(|| (self.0.clone(), Mode::ReadWrite))
    }
    fn contains_read_only(&self, _dir: &std::path::Path) -> bool {
        false
    }
}

/// The worker has no database: it knows no spaces by name and no earlier derivations (so it
/// always parses; the parent's persistence reuses what exists).
struct NoDatabase;

impl ExtractionContext for NoDatabase {
    fn space_id(&self, name: &str) -> Result<Uuid> {
        Err(Error::InvalidExtraction(format!(
            "the worker cannot resolve the space `{name}`"
        )))
    }
    fn git_refs_current(
        &self,
        _repository_id: Uuid,
    ) -> Result<Vec<crate::core::GitRefObservation>> {
        Ok(Vec::new())
    }
    fn has_code_derivation(
        &self,
        _language: crate::core::CodeLanguage,
        _content_hash: &str,
        _repository_id: Option<Uuid>,
        _blob_oid: Option<&str>,
        _identity: &crate::core::CodeParserIdentity,
    ) -> Result<bool> {
        Ok(false)
    }
}

fn read_line(sock: &mut UnixStream, max: usize) -> std::result::Result<String, String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match sock.read(&mut byte) {
            Ok(0) => return Err("the channel closed before the request".into()),
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => {
                line.push(byte[0]);
                if line.len() > max {
                    return Err("the request is too long".into());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("reading the request: {e}")),
        }
    }
    String::from_utf8(line).map_err(|_| "the request is not UTF-8".into())
}

fn fail(msg: &str, code: i32) -> i32 {
    eprintln!("mind extraction-worker: {msg}");
    code
}

/// Entry point. Returns the process exit code: 0 done, 2 bad request or extraction failure,
/// 78 the confinement could not be established (nothing was read).
pub fn main() -> i32 {
    // SAFETY: the parent placed these two descriptors before exec; nothing else of ours owns them.
    let (root, channel) = unsafe {
        (
            OwnedFd::from_raw_fd(ROOT_FD),
            UnixStream::from_raw_fd(CHANNEL_FD),
        )
    };
    let mut channel = channel;
    let sandbox = match crate::sandbox::enter_extraction_worker(&root, &[CHANNEL_FD, ROOT_FD]) {
        Ok(s) => s,
        Err(e) => return fail(&format!("sandbox: {e}"), 78),
    };
    let hello = Hello {
        v: PROTOCOL,
        pid: std::process::id(),
        sandbox,
    };
    let mut line = serde_json::to_vec(&hello).unwrap_or_default();
    line.push(b'\n');
    if channel.write_all(&line).is_err() {
        return fail("cannot send the hello", 2);
    }
    let request: Request = match read_line(&mut channel, MAX_REQUEST)
        .and_then(|l| serde_json::from_str::<Request>(&l).map_err(|e| format!("bad request: {e}")))
    {
        Ok(r) if r.v == PROTOCOL => r,
        Ok(r) => return fail(&format!("unsupported protocol version {}", r.v), 2),
        Err(e) => return fail(&e, 2),
    };
    let root_path = PathBuf::from(&request.root);
    let anchor = match Anchor::from_verified_fd(root, root_path.clone()) {
        Ok(a) => a,
        Err(e) => return fail(&format!("root: {e}"), 2),
    };
    let limits = ProducerLimits {
        max_blob_bytes: request.max_blob_bytes,
        parse_policy: request.parse_policy(),
    };
    let out = match channel.try_clone() {
        Ok(c) => c,
        Err(e) => return fail(&format!("channel: {e}"), 2),
    };
    let mut writer = FrameWriter::new(BufWriter::with_capacity(1 << 20, out), request.op, None);
    let result = stream_directory(
        &NoDatabase,
        &anchor,
        &WorkerPolicy(root_path),
        &request.spaces,
        &Deadline::none(),
        &limits,
        &mut writer,
    )
    .and_then(|_| writer.finish().map(|_| ()));
    match result {
        Ok(()) => 0,
        Err(e) => fail(&format!("extraction failed: {e}"), 2),
    }
}
