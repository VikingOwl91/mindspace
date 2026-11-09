//! The worker side: `mind extraction-worker`. Runs in a freshly exec'd process whose only
//! descriptors are stdio, the channel (3) and the root directory (4).

use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use crate::core::Result;
use crate::ingest::confined::{Anchor, Mode, PathPolicy};
use crate::ingest::deadline::Deadline;
use crate::ingest::stream::{stream_directory, FrameWriter, ProducerLimits, Record, RecordSink};

use super::context::{validate_spaces, Collector, Context, KnownSet, Manifest, MAX_CONTEXT_LINE};
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

/// A sink that keeps nothing: the first pass only collects the manifest.
struct NullSink;

impl RecordSink for NullSink {
    fn emit_encoded(&mut self, _: &'static str, _: Vec<u8>) -> Result<()> {
        Ok(())
    }
    fn emit(&mut self, _: &Record) -> Result<()> {
        Ok(())
    }
    fn emit_chunk(&mut self, _: &str) -> Result<()> {
        Ok(())
    }
}

/// One newline-terminated line of at most `max` bytes (newline excluded); never reads past it.
fn read_line(
    input: &mut impl BufRead,
    max: usize,
    what: &str,
) -> std::result::Result<String, String> {
    let mut line = Vec::new();
    match input
        .by_ref()
        .take(max as u64 + 1)
        .read_until(b'\n', &mut line)
    {
        Ok(0) => return Err(format!("the channel closed before the {what}")),
        Ok(_) if line.last() == Some(&b'\n') => {
            line.pop();
        }
        Ok(_) if line.len() > max => return Err(format!("the {what} is too long")),
        Ok(_) => return Err(format!("the {what} is truncated")),
        Err(e) => return Err(format!("reading the {what}: {e}")),
    }
    String::from_utf8(line).map_err(|_| format!("the {what} is not UTF-8"))
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
    let mut input = match channel.try_clone() {
        Ok(c) => BufReader::new(c),
        Err(e) => return fail(&format!("channel: {e}"), 2),
    };
    let request: Request = match read_line(&mut input, MAX_REQUEST, "request")
        .and_then(|l| serde_json::from_str::<Request>(&l).map_err(|e| format!("bad request: {e}")))
    {
        Ok(r) if r.v == PROTOCOL => r,
        Ok(r) => return fail(&format!("unsupported protocol version {}", r.v), 2),
        Err(e) => return fail(&e, 2),
    };
    if let Err(e) = validate_spaces(&request.spaces) {
        return fail(&format!("bad request: {e}"), 2);
    }
    let root_path = PathBuf::from(&request.root);
    let anchor = match Anchor::from_verified_fd(root, root_path.clone()) {
        Ok(a) => a,
        Err(e) => return fail(&format!("root: {e}"), 2),
    };
    let limits = ProducerLimits {
        max_blob_bytes: request.max_blob_bytes,
        parse_policy: request.parse_policy(),
    };
    // pass 1: which source blobs are there? (nothing is parsed, nothing is kept)
    let collector = Collector::default();
    if let Err(e) = stream_directory(
        &collector,
        &anchor,
        &WorkerPolicy(root_path.clone()),
        &request.spaces,
        &Deadline::none(),
        &limits,
        &mut NullSink,
    ) {
        return fail(&format!("listing the sources failed: {e}"), 2);
    }
    let manifest = Manifest {
        v: PROTOCOL,
        op: request.op,
        entries: collector.into_entries(),
    };
    let mut bytes = serde_json::to_vec(&manifest).unwrap_or_default();
    bytes.push(b'\n');
    if channel.write_all(&bytes).is_err() {
        return fail("cannot send the manifest", 2);
    }
    // the parent's answer: the only thing this process will ever know about the database
    let context: Context = match read_line(&mut input, MAX_CONTEXT_LINE, "context")
        .and_then(|l| serde_json::from_str::<Context>(&l).map_err(|e| format!("bad context: {e}")))
    {
        Ok(c) => c,
        Err(e) => return fail(&e, 2),
    };
    if let Err(e) = context.validate(request.op, &request.parse_policy()) {
        return fail(&format!("bad context: {e}"), 2);
    }
    let known = KnownSet::new(context);
    // pass 2: the stream, bound to the operation
    let out = match channel.try_clone() {
        Ok(c) => c,
        Err(e) => return fail(&format!("channel: {e}"), 2),
    };
    let mut writer = FrameWriter::new(BufWriter::with_capacity(1 << 20, out), request.op, None);
    let result = stream_directory(
        &known,
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
