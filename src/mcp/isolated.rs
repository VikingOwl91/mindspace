//! `repo_ingest_isolated` (M7.3b.4): the first MCP tool that ingests a directory through the
//! broker. EXPERIMENTAL and opt-in (`mind mcp --isolated-ingest`, hidden): without the flag the
//! tool does not exist, is not listed and cannot be called.
//!
//! Boundaries (docs/m7.3b-mcp-isolated-ingest.md):
//!
//! * **Authority is the broker's.** The caller supplies a path and space names, nothing else: no
//!   grant id, access mode, limit, descriptor or "approved" claim exists in the schema (unknown
//!   arguments are refused). The frontend cannot even canonicalize a root approved after it was
//!   confined, so it passes the string through untouched; the broker matches it exactly against
//!   the authorities it derives itself (static roots, or the grant store re-read now) and pins
//!   the directory. This tool therefore never consults the frontend's own root list.
//! * **The worker reads, the frontend writes.** The worker holds one read-only root descriptor
//!   inside its own Landlock + seccomp domain, in a cgroup of the frontend's own unit. It returns
//!   a record stream that this process validates completely (the existing assembler, the
//!   operation-bound context exchange and `check_result`) before anything is persisted.
//! * **Persistence only after the broker's `Finished`.** The result is persisted when, and only
//!   when, the broker reports `ended_by = done` (its final authority recheck is the
//!   linearization point for revocation), the worker exited cleanly and its cgroup lies inside
//!   this process's cgroup. Every other end persists nothing.
//! * **No resource guarantee beyond the existing bounds**: wall clock (`--ingest-timeout`),
//!   stream and structural-parse limits. There is NO aggregate per-operation memory limit; the
//!   unit's own `MemoryMax=` (if any) covers the tree because it lives inside the unit.

use std::cell::{Cell, RefCell};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{json, Map, Value};

use crate::core::Uuid;
use crate::ingest::deadline::Deadline;
use crate::ingest::extract::{persist_files_gated, Extracted, StoreContext};
use crate::storage::Store;
use crate::worker::client::{
    commit_done, commit_request, extract_via_broker, WorkerConfig, WorkerError,
};

use super::repo_tools::{summarize, McpConfig, MAX_UNSCOPED_INGEST_FILES};
use super::tools::ToolFailure;

pub const TOOL: &str = "repo_ingest_isolated";

/// How long this side may use a commit lease: shorter than the broker's ceiling.
const LEASE_BUDGET: std::time::Duration = std::time::Duration::from_millis(2500);

/// The frontend's end of the broker channel. One operation at a time (the lock); a link that may
/// be out of step is never used again.
#[derive(Debug)]
pub struct IsolatedIngest {
    link: Mutex<UnixStream>,
    poisoned: AtomicBool,
}

impl IsolatedIngest {
    pub fn new(link: UnixStream) -> Self {
        Self {
            link: Mutex::new(link),
            poisoned: AtomicBool::new(false),
        }
    }
}

pub fn definition() -> Value {
    json!({
        "name": TOOL,
        "description": "EXPERIMENTAL. Ingest a directory (not a git repository) through an isolated worker started by a broker outside this server's sandbox. `path` must be the canonical absolute path of a directory the broker is authorized to serve: a root this server was started with, or a directory you granted with `mind grant` (also after this server started; no restart needed). The caller cannot name a grant, an access mode or a limit; the broker decides. The worker only reads (its own Landlock and seccomp domain); this server validates the whole result and writes it only afterwards. A grant revoked while the operation runs ends it and persists nothing. Bounds: the `--ingest-timeout` wall clock and the existing stream and parse limits; there is no aggregate per-operation memory limit. Results and failures are structured (`structuredContent`; failures carry `error.kind` and `persisted: false`).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1, "description": "Canonical absolute path of the directory root." },
                "spaces": { "type": "array", "items": { "type": "string", "minLength": 1 }, "minItems": 1, "maxItems": 64 }
            },
            "required": ["path", "spaces"],
            "additionalProperties": false
        }
    })
}

fn failure(kind: &str, message: impl Into<String>) -> ToolFailure {
    failure_with(kind, message, json!(false))
}

fn failure_with(kind: &str, message: impl Into<String>, persisted: Value) -> ToolFailure {
    let message = message.into();
    let structured = json!({
        "error": { "kind": kind, "message": message },
        "persisted": persisted
    });
    ToolFailure {
        message: structured.to_string(),
        structured: Some(structured),
    }
}

/// An idle, healthy link has nothing to read. Readable means the peer closed it (end of file) or
/// something unexpected is queued; both mean it must not be used.
fn link_idle(link: &UnixStream) -> bool {
    let mut p = libc::pollfd {
        fd: link.as_raw_fd(),
        events: libc::POLLIN | libc::POLLRDHUP,
        revents: 0,
    };
    // SAFETY: poll on a descriptor this process owns, zero timeout.
    let r = unsafe { libc::poll(&mut p, 1, 0) };
    r == 0
}

fn error_of(e: WorkerError) -> (ToolFailure, bool) {
    use WorkerError::*;
    let text = e.to_string();
    let poison = matches!(e, Io(_));
    let kind = match &e {
        Refused(m) if m.contains("not authorized") => "root_not_authorized",
        Refused(m) if m.contains("broker is not available") => "broker_unavailable",
        Refused(m) if m.contains("busy") => "busy",
        Refused(m) if m.contains("already used") => "replay_refused",
        Refused(_) => "refused",
        Revoked => "revoked",
        Timeout(_) => "timeout",
        Cancelled => "cancelled",
        Context(_) => "context_refused",
        Stream(m) if m.contains("files exceed the limit") => "too_many_files",
        Stream(_) => "result_refused",
        Sandbox(_) => "confinement_not_verified",
        Died { .. } => "worker_failed",
        Spawn(_) | Io(_) => "broker_io",
    };
    let text = if kind == "too_many_files" {
        format!(
            "refusing an ingest of more than {MAX_UNSCOPED_INGEST_FILES} ingestable files: this tool \
             takes a whole directory and cannot narrow it; ingest a smaller directory ({text})"
        )
    } else {
        text
    };
    (failure(kind, text), poison)
}

/// Test builds only (feature `spawn-gate`): makes a boundary deterministic. With `<var>=<prefix>`
/// the frontend creates `<prefix>.reached`, then waits (at most 15 s) for `<prefix>.go`.
#[cfg(feature = "spawn-gate")]
fn gate_pause(var: &str) {
    let Some(prefix) = std::env::var_os(var) else {
        return;
    };
    let prefix = std::path::PathBuf::from(prefix);
    let with = |ext: &str| {
        let mut p = prefix.clone().into_os_string();
        p.push(ext);
        std::path::PathBuf::from(p)
    };
    let _ = std::fs::write(with(".reached"), b"");
    let end = Instant::now() + std::time::Duration::from_secs(15);
    while !with(".go").exists() && Instant::now() < end {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Own cgroup as the kernel reports it (`/proc/self` is the one entry the sandbox allows).
fn own_cgroup() -> Option<String> {
    std::fs::read_to_string("/proc/self/cgroup")
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("0::").map(|p| p.trim().to_string()))
}

pub fn run(
    store: &dyn Store,
    config: &McpConfig,
    obj: &Map<String, Value>,
) -> std::result::Result<Value, ToolFailure> {
    let Some(iso) = config.isolated() else {
        return Err(failure(
            "not_enabled",
            "repo_ingest_isolated is not enabled on this server",
        ));
    };
    let path = obj
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| failure("invalid_argument", "argument 'path' must be a string"))?;
    if path.is_empty() || path.len() > 4096 || path.contains('\0') || !Path::new(path).is_absolute()
    {
        return Err(failure(
            "invalid_argument",
            "argument 'path' must be a canonical absolute path",
        ));
    }
    let names: Vec<&str> = obj
        .get("spaces")
        .and_then(Value::as_array)
        .ok_or_else(|| failure("invalid_argument", "argument 'spaces' must be an array"))?
        .iter()
        .map(|v| v.as_str().filter(|s| !s.trim().is_empty()))
        .collect::<Option<_>>()
        .ok_or_else(|| {
            failure(
                "invalid_argument",
                "argument 'spaces' must contain non-empty strings",
            )
        })?;
    if names.is_empty() || names.len() > 64 {
        return Err(failure(
            "invalid_argument",
            "argument 'spaces' must name 1 to 64 spaces",
        ));
    }
    let mut ids: Vec<Uuid> = Vec::new();
    for n in &names {
        let id = store
            .get_space(n)
            .map_err(|e| failure("unknown_space", e.to_string()))?
            .id;
        if ids.contains(&id) {
            return Err(failure("invalid_argument", "a space is named twice"));
        }
        ids.push(id);
    }

    // availability is checked before anything is sent
    if iso.poisoned.load(Ordering::SeqCst) {
        return Err(failure(
            "broker_unavailable",
            "the broker is not available (its channel is out of step; restart the server)",
        ));
    }
    let link = iso
        .link
        .lock()
        .map_err(|_| failure("broker_unavailable", "the broker channel is unusable"))?;
    if !link_idle(&link) {
        iso.poisoned.store(true, Ordering::SeqCst);
        return Err(failure(
            "broker_unavailable",
            "the broker is not available (it never started or has exited)",
        ));
    }

    #[allow(unused_mut)]
    let mut cfg = WorkerConfig::new("/nonexistent"); // the broker starts the worker; this path is unused
    cfg.wall_timeout = config.ingest_timeout();
    // This tool is always unscoped (there is no `paths`), so it gets the limit `repo_ingest` has
    // for unscoped trees. The producer streams files one by one: the 2001st is refused as it
    // arrives and the worker is ended, without a pre-scan.
    cfg.stream.extraction.max_files = MAX_UNSCOPED_INGEST_FILES as usize;
    // Test builds only (feature `spawn-gate`): the stand-in workers of the failure tests cannot
    // say hello as a real worker does.
    #[cfg(feature = "spawn-gate")]
    if std::env::var_os("MINDSPACE_ISOLATED_GATE_NO_EVIDENCE").is_some() {
        cfg.require_sandbox_evidence = false;
    }
    let cancel = AtomicBool::new(false);
    let t0 = Instant::now();
    let (extracted, stats, rep) = match extract_via_broker(
        &cfg,
        &link,
        Path::new(path),
        &ids,
        &StoreContext(store),
        &cancel,
    ) {
        Ok(ok) => ok,
        Err(e) => {
            let (f, poison) = error_of(e);
            if poison {
                iso.poisoned.store(true, Ordering::SeqCst);
            }
            return Err(f);
        }
    };
    // The worker must have run inside this server's own cgroup (and so inside its unit): a
    // placement elsewhere would silently escape the unit's lifecycle and limits.
    let contained = match (own_cgroup(), rep.observed.cgroup.as_deref()) {
        (Some(own), Some(w)) => w.starts_with(&format!("{}/", own.trim_end_matches('/'))),
        _ => false,
    };
    if !contained {
        return Err(failure(
            "containment_not_verified",
            "the worker did not run inside this server's cgroup; nothing was persisted",
        ));
    }

    // Persistence: ONE transaction for the whole ingestion (observation and derivations), and a
    // commit-time authorization decision from the broker taken inside it, immediately before the
    // COMMIT. A denial rolls the transaction back. While the commit runs the broker holds the
    // grant store's shared lock, so a revocation that starts now is ordered after the commit.
    let Extracted::Files(files) = extracted else {
        return Err(failure(
            "result_refused",
            "the result is not a directory ingestion",
        ));
    };
    let leased = Cell::new(false);
    let denial: RefCell<Option<String>> = RefCell::new(None);
    let t_persist = Instant::now();
    let result = persist_files_gated(store, files, &Deadline::none(), &|| {
        #[cfg(feature = "spawn-gate")]
        gate_pause("MINDSPACE_ISOLATED_GATE_PAUSE_BEFORE");
        match commit_request(&link, rep.op) {
            Ok(()) => {
                leased.set(true);
                let granted_at = Instant::now();
                #[cfg(feature = "spawn-gate")]
                gate_pause("MINDSPACE_ISOLATED_GATE_PAUSE_AFTER");
                // The broker lets revocations proceed after COMMIT_HOLD_MAX. A commit must end
                // inside the lease: use a shorter budget here (the broker's clock started
                // earlier than this one) and abandon the commit otherwise.
                if granted_at.elapsed() >= LEASE_BUDGET {
                    *denial.borrow_mut() =
                        Some("the commit lease expired before the commit".to_string());
                    return Err(crate::core::Error::Storage(
                        "the commit lease expired before the commit".to_string(),
                    ));
                }
                Ok(())
            }
            Err(reason) => {
                *denial.borrow_mut() = Some(reason.clone());
                Err(crate::core::Error::Storage(format!(
                    "the commit was not authorized: {reason}"
                )))
            }
        }
    });
    if leased.get() {
        commit_done(&link); // success or not: the lease ends with the commit attempt
    }
    drop(link);
    let persist_ms = t_persist.elapsed().as_millis() as u64;
    if let Some(reason) = denial.into_inner() {
        // rolled back before the COMMIT: nothing was written, which is certain
        let kind = if reason.contains("broker is not available") {
            "broker_unavailable"
        } else if reason.contains("lease expired") {
            "commit_not_authorized"
        } else {
            "revoked"
        };
        return Err(failure(kind, format!("the commit was refused: {reason}")));
    }
    let report = result.map_err(|e| {
        let mut f = failure_with("persist_failed", e.to_string(), json!("unknown"));
        // the single transaction either committed everything or nothing; only a failure of the
        // COMMIT itself leaves the outcome unknown
        if let Some(s) = f.structured.as_mut() {
            s["atomic"] = json!(true);
            f.message = s.to_string();
        }
        f
    })?;
    let outcome = crate::core::IngestOutcome::Files(report);
    let authority = match rep.authority {
        crate::worker::broker::AuthoritySource::Static => "static_root",
        crate::worker::broker::AuthoritySource::Grant(_) => "grant",
    };
    Ok(json!({
        "ingested": [summarize(&outcome)],
        "isolation": {
            "authority": authority,
            "worker": {
                "seccomp": rep.observed.seccomp,
                "no_new_privs": rep.observed.no_new_privs,
                "tracer_pid": rep.observed.tracer_pid,
                "cap_eff": rep.observed.cap_eff,
                "cgroup": rep.observed.cgroup,
                "contained_in_server_cgroup": contained,
                "cleanup": rep.observed.cleanup,
                "peak_rss_kb": rep.peak_rss_kb,
                "exit": rep.exit,
            },
            "ended_by": rep.ended_by,
            "descendants_killed": rep.descendants_killed,
        },
        "context_exchange": {
            "manifest_entries": stats.manifest_entries,
            "known_derivations_reused": stats.known_sent,
        },
        "resource_policy": {
            "placement": "inside the server's own cgroup (the unit's lifecycle and limits apply)",
            "wall_clock_limit_ms": config.ingest_timeout().as_millis() as u64,
            "per_operation_memory_limit": null,
            "max_files": MAX_UNSCOPED_INGEST_FILES,
            "max_stream_bytes": 512 * 1024 * 1024,
            "max_blob_bytes": 32 * 1024 * 1024,
            "note": "there is no aggregate per-operation memory limit; only the wall clock, the stream limits and the structural parse policy bound the operation",
        },
        "persist_ms": persist_ms,
        "commit_authorization": "granted by the broker immediately before the COMMIT",
        "elapsed_ms": t0.elapsed().as_millis() as u64,
        "sandbox": config.sandbox().label(),
        "seccomp": config.seccomp(),
        "next": "Use code_search for definitions and memory_search for documents."
    }))
}
