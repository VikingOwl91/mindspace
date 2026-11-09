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

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{json, Map, Value};

use crate::core::Uuid;
use crate::ingest::deadline::Deadline;
use crate::ingest::extract::{persist, StoreContext};
use crate::storage::Store;
use crate::worker::client::{extract_via_broker, WorkerConfig, WorkerError};

use super::repo_tools::{summarize, McpConfig};
use super::tools::ToolFailure;

pub const TOOL: &str = "repo_ingest_isolated";

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
        Stream(_) => "result_refused",
        Sandbox(_) => "confinement_not_verified",
        Died { .. } => "worker_failed",
        Spawn(_) | Io(_) => "broker_io",
    };
    (failure(kind, text), poison)
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
    drop(link);

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

    // Only now, with the broker's `done` and a validated result, anything is written.
    // `persist` is the existing two-step writer: a failure may leave the first step committed, so
    // "persisted" is honestly unknown here.
    let outcome = persist(store, extracted, &Deadline::none())
        .map_err(|e| failure_with("persist_failed", e.to_string(), json!("unknown")))?;
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
            "note": "there is no aggregate per-operation memory limit; only the wall clock, the stream limits and the structural parse policy bound the operation",
        },
        "elapsed_ms": t0.elapsed().as_millis() as u64,
        "sandbox": config.sandbox().label(),
        "seccomp": config.seccomp(),
        "next": "Use code_search for definitions and memory_search for documents."
    }))
}
