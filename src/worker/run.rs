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

use super::context::{
    validate_spaces, Collector, Context, GitContext, GitKnownSet, GitManifest, GitManifestEntry,
    KnownSet, Manifest, MAX_CONTEXT_LINE,
};
use super::{Hello, Request, CHANNEL_FD, CONTROL_FD, MAX_REQUEST, PROTOCOL, ROOT_FD};

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
    // a broker passes a control descriptor; its absence means the in-frontend client started us
    // SAFETY: F_GETFD only tests whether the number is open.
    let control = (unsafe { libc::fcntl(CONTROL_FD, libc::F_GETFD) } != -1)
        // SAFETY: the broker placed this descriptor before exec; nothing else of ours owns it.
        .then(|| unsafe { UnixStream::from_raw_fd(CONTROL_FD) });
    let keep: Vec<i32> = if control.is_some() {
        vec![CHANNEL_FD, ROOT_FD, CONTROL_FD]
    } else {
        vec![CHANNEL_FD, ROOT_FD]
    };
    let sandbox = match crate::sandbox::enter_extraction_worker(&root, &keep) {
        Ok(s) => s,
        Err(e) => return fail(&format!("sandbox: {e}"), 78),
    };
    // Confined. A broker observes this process from outside (/proc, cgroup) before it lets the
    // frontend see a byte: report READY, then wait for GO. Anything else, or EOF, ends us.
    if let Some(mut ctl) = control {
        if ctl.write_all(b"READY\n").is_err() {
            return fail("cannot report to the broker", 2);
        }
        let mut go = [0u8; 3];
        if ctl.read_exact(&mut go).is_err() || &go != b"GO\n" {
            return fail("the broker did not release the worker", 2);
        }
    }
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

/// TEST HARNESS: a context that claims every derivation is known (a worker forging reuse).
#[cfg(feature = "spawn-gate")]
struct AlwaysKnown;

#[cfg(feature = "spawn-gate")]
impl crate::ingest::extract::ExtractionContext for AlwaysKnown {
    fn space_id(&self, name: &str) -> Result<crate::core::Uuid> {
        Err(crate::core::Error::InvalidExtraction(format!(
            "the worker cannot resolve the space `{name}`"
        )))
    }
    fn git_refs_current(
        &self,
        _: crate::core::Uuid,
    ) -> Result<Vec<crate::core::GitRefObservation>> {
        Ok(Vec::new())
    }
    fn has_code_derivation(
        &self,
        _: crate::core::CodeLanguage,
        _: &str,
        _: Option<crate::core::Uuid>,
        _: Option<&str>,
        _: &crate::core::CodeParserIdentity,
    ) -> Result<bool> {
        Ok(true)
    }
}

// ------------------------------------------------------------------------------------- Git

/// `mind git-extraction-worker --git-exe <path> --repository-id <uuid>`: the Git mode of the same
/// confined worker. The mode, the executable and the expected repository identity come from the
/// BROKER's argv (the frontend can influence none of them); nothing the repository contains
/// selects anything.
///
/// Descriptors, as for the directory worker: stdio, the channel (3), the root (4) and, with a
/// broker, the control channel (5). The root descriptor is the only authority over the repository.
/// Before it confines itself the worker resolves the git executable and its ELF interpreter; its
/// Landlock domain then allows reading the root and the system directories and EXECUTING exactly
/// those two files. Every git child inherits that domain.
///
/// Exit codes: 0 done, 2 bad request, extraction failure or identity mismatch, 78 the
/// confinement could not be established (nothing was read).
pub fn main_git(
    git_path: &std::path::Path,
    expected: crate::core::Uuid,
    gate: Option<&str>,
) -> i32 {
    #[cfg(not(feature = "spawn-gate"))]
    let _ = gate;
    use crate::ingest::confined_git::{ConfinedGitReader, GitExecutable};
    use crate::ingest::gitverify::{verify_head, VerifyLimits};
    use crate::ingest::outline::PathSelection;
    use crate::ingest::stream::{head_code_candidates, stream_verified_head};
    use std::os::fd::AsFd;

    // SAFETY: the parent placed these two descriptors before exec; nothing else of ours owns them.
    let (root, channel) = unsafe {
        (
            OwnedFd::from_raw_fd(ROOT_FD),
            UnixStream::from_raw_fd(CHANNEL_FD),
        )
    };
    let mut channel = channel;
    // SAFETY: F_GETFD only tests whether the number is open.
    let control = (unsafe { libc::fcntl(CONTROL_FD, libc::F_GETFD) } != -1)
        // SAFETY: the broker placed this descriptor before exec; nothing else of ours owns it.
        .then(|| unsafe { UnixStream::from_raw_fd(CONTROL_FD) });
    let keep: Vec<i32> = if control.is_some() {
        vec![CHANNEL_FD, ROOT_FD, CONTROL_FD]
    } else {
        vec![CHANNEL_FD, ROOT_FD]
    };
    // resolved BEFORE the confinement, from the broker's argv
    let git = match GitExecutable::resolve(git_path) {
        Ok(g) => g,
        Err(e) => return fail(&format!("git executable: {e}"), 78),
    };
    let sandbox =
        match crate::sandbox::enter_extraction_worker_with_exec(&root, &keep, &git.exec_grants()) {
            Ok(s) => s,
            Err(e) => return fail(&format!("sandbox: {e}"), 78),
        };
    if let Some(mut ctl) = control {
        if ctl.write_all(b"READY\n").is_err() {
            return fail("cannot report to the broker", 2);
        }
        let mut go = [0u8; 3];
        if ctl.read_exact(&mut go).is_err() || &go != b"GO\n" {
            return fail("the broker did not release the worker", 2);
        }
    }
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
    // the descriptor must be the directory the request names
    match std::fs::read_link(format!("/proc/self/fd/{ROOT_FD}")) {
        Ok(p) if p == std::path::Path::new(&request.root) => {}
        _ => {
            return fail(
                "root: the descriptor does not name the announced directory",
                2,
            )
        }
    }
    let limits = ProducerLimits {
        max_blob_bytes: request.max_blob_bytes,
        parse_policy: request.parse_policy(),
    };
    let verify = VerifyLimits::default();
    let reader = match ConfinedGitReader::open(root.as_fd(), &git, Deadline::none()) {
        Ok(r) => r,
        Err(e) => return fail(&format!("repository refused: {e}"), 2),
    };
    // the identity the BROKER bound: this worker reads the marker itself and refuses a mismatch
    // (the frontend refuses it too; a worker that lies is caught there)
    let repository_id = match reader.repository_id() {
        Ok(id) if id == expected => id,
        Ok(_) => return fail("the repository identity is not the one the broker bound", 2),
        Err(e) => return fail(&format!("repository identity: {e}"), 2),
    };
    let closure = match reader.head_closure(&verify) {
        Ok(c) => c,
        Err(e) => return fail(&format!("reading HEAD: {e}"), 2),
    };
    // the same verification the frontend will apply, here only to learn the paths of the tree
    let head = match verify_head(&closure.commit_oid, &closure.objects, &verify) {
        Ok(h) => h,
        Err(e) => return fail(&format!("HEAD does not verify: {e}"), 2),
    };
    let selection = PathSelection::all();
    // TEST HARNESS (feature `spawn-gate`): a worker that misbehaves in one named way, started with
    // the real confinement. Absent from a plain build.
    #[cfg(feature = "spawn-gate")]
    if gate == Some("crash") {
        std::process::abort();
    }
    #[cfg(feature = "spawn-gate")]
    let manifest_identity = if gate == Some("lie-manifest-identity") {
        crate::core::Uuid::now_v7()
    } else {
        repository_id
    };
    #[cfg(not(feature = "spawn-gate"))]
    let manifest_identity = repository_id;
    #[cfg(feature = "spawn-gate")]
    let manifest_head = if gate == Some("other-head-manifest") {
        "ab".repeat(20)
    } else {
        closure.commit_oid.clone()
    };
    #[cfg(not(feature = "spawn-gate"))]
    let manifest_head = closure.commit_oid.clone();
    // first exchange: which source blobs would have to be parsed?
    let manifest = GitManifest {
        v: PROTOCOL,
        op: request.op,
        repository_id: manifest_identity,
        head: manifest_head.clone(),
        entries: head_code_candidates(&head.files, &selection)
            .into_iter()
            .map(|(language, blob_oid)| GitManifestEntry { language, blob_oid })
            .collect(),
    };
    let mut bytes = serde_json::to_vec(&manifest).unwrap_or_default();
    bytes.push(b'\n');
    if channel.write_all(&bytes).is_err() {
        return fail("cannot send the manifest", 2);
    }
    let context: GitContext =
        match read_line(&mut input, MAX_CONTEXT_LINE, "context").and_then(|l| {
            serde_json::from_str::<GitContext>(&l).map_err(|e| format!("bad context: {e}"))
        }) {
            Ok(c) => c,
            Err(e) => return fail(&e, 2),
        };
    #[cfg(feature = "spawn-gate")]
    let skip_validation = matches!(gate, Some("other-head-manifest" | "lie-manifest-identity"));
    #[cfg(not(feature = "spawn-gate"))]
    let skip_validation = false;
    if !skip_validation {
        if let Err(e) = context.validate(
            request.op,
            repository_id,
            &closure.commit_oid,
            &request.parse_policy(),
        ) {
            return fail(&format!("bad context: {e}"), 2);
        }
    }
    #[cfg(feature = "spawn-gate")]
    if gate == Some("hang") {
        // a git child that blocks on its stdin, then nothing: the broker must end both
        let _child = reader.spawn_raw(&["cat-file", "--batch"]);
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
    #[cfg(feature = "spawn-gate")]
    if gate == Some("garbage") {
        let _ = channel.write_all(b"this is not a frame stream\n");
        return 0;
    }
    #[cfg(feature = "spawn-gate")]
    let known: Box<dyn crate::ingest::extract::ExtractionContext> = if gate == Some("forge-reuse") {
        Box::new(AlwaysKnown)
    } else {
        Box::new(GitKnownSet::new(context))
    };
    #[cfg(not(feature = "spawn-gate"))]
    let known = GitKnownSet::new(context);
    let out = match channel.try_clone() {
        Ok(c) => c,
        Err(e) => return fail(&format!("channel: {e}"), 2),
    };
    #[cfg(feature = "spawn-gate")]
    let stream_op = if gate == Some("replay-op") {
        crate::core::Uuid::now_v7()
    } else {
        request.op
    };
    #[cfg(not(feature = "spawn-gate"))]
    let stream_op = request.op;
    #[cfg(feature = "spawn-gate")]
    let stream_identity = if gate == Some("lie-header-identity") {
        crate::core::Uuid::now_v7()
    } else {
        repository_id
    };
    #[cfg(not(feature = "spawn-gate"))]
    let stream_identity = repository_id;
    #[cfg(feature = "spawn-gate")]
    let stream_label = if gate == Some("wrong-root-label") {
        "/somewhere/else".to_string()
    } else {
        request.root.clone()
    };
    #[cfg(not(feature = "spawn-gate"))]
    let stream_label = request.root.clone();
    #[cfg(feature = "spawn-gate")]
    let known_ctx: &dyn crate::ingest::extract::ExtractionContext = known.as_ref();
    #[cfg(not(feature = "spawn-gate"))]
    let known_ctx: &dyn crate::ingest::extract::ExtractionContext = &known;
    let mut writer = FrameWriter::new(BufWriter::with_capacity(1 << 20, out), stream_op, None);
    #[cfg(feature = "spawn-gate")]
    if gate == Some("oversized") {
        // a raw object whose declared size is far beyond the limit, with no body
        use crate::ingest::stream::{Kind, Record, RecordSink};
        let _ = writer.emit(&Record::Begin {
            kind: Kind::GitVerified,
        });
        let _ = writer.emit(&Record::GitHeader {
            session: crate::core::SessionDraft {
                root: request.root.clone(),
                scope: crate::core::IngestScope::GitRepository,
                observed_at: crate::core::Ts::now(),
            },
            repository_id,
            repository_path: request.root.clone(),
            spaces: request.spaces.clone(),
        });
        let _ = writer.emit(&Record::Head {
            symbolic: None,
            commit_oid: closure.commit_oid.clone(),
        });
        let _ = writer.emit(&Record::RawObject {
            kind: crate::core::GitObjectKind::Tree,
            oid: "cd".repeat(20),
            size: 1 << 40,
            hex: String::new(),
        });
        let _ = writer.finish();
        return 0;
    }
    #[cfg(feature = "spawn-gate")]
    let truncate = gate == Some("truncate");
    #[cfg(not(feature = "spawn-gate"))]
    let truncate = false;
    let result = stream_verified_head(
        known_ctx,
        &reader,
        &closure,
        &head.files,
        stream_identity,
        std::path::Path::new(&stream_label),
        &request.spaces,
        &selection,
        &Deadline::none(),
        &limits,
        &mut writer,
    )
    .and_then(|_| {
        if truncate {
            // the stream stops before its closing record
            drop(writer);
            return Ok(());
        }
        writer.finish().map(|_| ())
    });
    match result {
        Ok(()) => 0,
        Err(e) => fail(&format!("extraction failed: {e}"), 2),
    }
}
