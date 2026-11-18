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

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use serde_json::{json, Map, Value};

use crate::core::Uuid;
use crate::ingest::deadline::Deadline;
use crate::ingest::extract::{persist_files_gated, Extracted, StoreContext};
use crate::storage::Store;
use crate::worker::client::{
    extract_via_broker, link_idle, persist_with_lease, PersistFailure, WorkerConfig, WorkerError,
};

use super::repo_tools::{summarize, McpConfig, MAX_UNSCOPED_INGEST_FILES};
use super::tools::ToolFailure;

pub const TOOL: &str = "repo_ingest_isolated";

/// The frontend's end of the broker channel. One operation at a time (the lock); a link that may
/// be out of step is never used again.
#[derive(Debug)]
pub struct IsolatedIngest {
    link: Mutex<UnixStream>,
    pub(super) poisoned: AtomicBool,
}

impl IsolatedIngest {
    pub fn new(link: UnixStream) -> Self {
        Self {
            link: Mutex::new(link),
            poisoned: AtomicBool::new(false),
        }
    }

    /// Check poison while holding the same lock as the operation that sets it. A queued
    /// caller must see a lost decision even when the late reply has not arrived yet.
    pub(super) fn lock_link(&self) -> Result<MutexGuard<'_, UnixStream>, ToolFailure> {
        let link = self
            .link
            .lock()
            .map_err(|_| failure("broker_unavailable", "the broker channel is unusable"))?;
        if self.poisoned.load(Ordering::SeqCst) {
            return Err(failure(
                "broker_unavailable",
                "the broker is not available (its channel is out of step; restart the server)",
            ));
        }
        if !link_idle(&link) {
            self.poisoned.store(true, Ordering::SeqCst);
            return Err(failure(
                "broker_unavailable",
                "the broker is not available (it never started or has exited)",
            ));
        }

        Ok(link)
    }

    /// Both isolated tools hold the link mutex through this step. The next caller must see
    /// a stale channel even if the late decision is still in transit.
    pub(super) fn lease_refusal(
        &self,
        kind: &str,
        reason: &str,
        discard_link: bool,
    ) -> ToolFailure {
        if discard_link {
            self.poisoned.store(true, Ordering::SeqCst);
        }
        failure_commit(
            kind,
            format!("the commit was refused: {reason}"),
            "ROLLED_BACK",
            json!(false),
        )
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

pub(super) fn failure(kind: &str, message: impl Into<String>) -> ToolFailure {
    failure_with(kind, message, json!(false))
}

fn failure_with(kind: &str, message: impl Into<String>, persisted: Value) -> ToolFailure {
    failure_commit(kind, message, "NOT_STARTED", persisted)
}

/// `commit_state`: NOT_STARTED (failed before any write), ROLLED_BACK (nothing committed, certain)
/// or UNKNOWN (see the persistence section).
fn failure_commit(
    kind: &str,
    message: impl Into<String>,
    commit_state: &str,
    persisted: Value,
) -> ToolFailure {
    let message = message.into();
    let mut structured = json!({
        "error": { "kind": kind, "message": message },
        "persisted": persisted,
        "commit_state": commit_state
    });
    if commit_state != "NOT_STARTED" {
        // one transaction: everything or nothing
        structured["atomic"] = json!(true);
    }
    ToolFailure {
        message: structured.to_string(),
        structured: Some(structured),
    }
}

fn error_of(e: WorkerError) -> (ToolFailure, bool) {
    let text = e.to_string();
    let poison = e.discard_broker_link();
    let kind = e.kind();
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

/// Test builds only: with `<var>=<prefix>` creates `<prefix>.committed` and returns.
#[cfg(feature = "spawn-gate")]
fn gate_mark(var: &str) {
    if let Some(prefix) = std::env::var_os(var) {
        let mut p = prefix;
        p.push(".committed");
        // the instant itself (ns since the epoch, same clock as the test's), not the file's mtime:
        // file times use a coarse clock that can lag the real time by milliseconds
        let ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let _ = std::fs::write(std::path::PathBuf::from(p), ns.to_string());
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
    let deadline = Deadline::after(config.ingest_timeout());
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
            .map_err(|e| match e {
                crate::core::Error::ConnectionDiscarded => failure(
                    "store_discarded",
                    "the database handle of this server was discarded after an uncertain commit \
                     outcome; restart the server",
                ),
                other => failure("unknown_space", other.to_string()),
            })?
            .id;
        if ids.contains(&id) {
            return Err(failure("invalid_argument", "a space is named twice"));
        }
        ids.push(id);
    }

    let link = iso.lock_link()?;

    #[allow(unused_mut)]
    let mut cfg = WorkerConfig::new("/nonexistent"); // the broker starts the worker; this path is unused
    deadline
        .check("starting the extraction worker")
        .map_err(|e| failure("timeout", e.to_string()))?;
    cfg.wall_timeout = deadline
        .remaining()
        .expect("isolated ingest has a configured deadline");
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
    //
    // The outcome is one of three, never inferred from whether a message arrived:
    //   COMMITTED   the storage layer returned Ok: SQLite's COMMIT succeeded.
    //   ROLLED_BACK any other error: nothing was committed (statement error, denied lease, a
    //               COMMIT that SQLite defines as not committed).
    //   UNKNOWN     `CommitOutcomeUnknown`: the COMMIT failed in a way that does not say.
    let Extracted::Files(files) = extracted else {
        return Err(failure(
            "result_refused",
            "the result is not a directory ingestion",
        ));
    };
    let t_persist = Instant::now();
    #[cfg(feature = "spawn-gate")]
    {
        use std::sync::atomic::Ordering::SeqCst;
        // one-shot per server: the first persistence is faulted, later ones are not
        static ARMED: AtomicBool = AtomicBool::new(false);
        if !ARMED.swap(true, SeqCst) {
            if std::env::var_os("MINDSPACE_STORE_GATE_FAIL_COMMIT").is_some() {
                crate::storage::sqlite::gate::FAIL_COMMIT.store(true, SeqCst);
            }
            if std::env::var_os("MINDSPACE_STORE_GATE_LOSE_ACK").is_some() {
                crate::storage::sqlite::gate::LOSE_ACK.store(true, SeqCst);
            }
            if std::env::var_os("MINDSPACE_STORE_GATE_PAUSE_IN_COMMIT").is_some() {
                crate::storage::sqlite::gate::PAUSE_IN_COMMIT.store(true, SeqCst);
            }
        }
    }
    let result = persist_with_lease(
        &link,
        rep.op,
        |authorize| {
            let result = persist_files_gated(store, files, &deadline, &|| {
                #[cfg(feature = "spawn-gate")]
                gate_pause("MINDSPACE_ISOLATED_GATE_PAUSE_BEFORE");
                authorize()
            });
            #[cfg(feature = "spawn-gate")]
            if result.is_ok() {
                gate_mark("MINDSPACE_ISOLATED_GATE_MARK_COMMITTED");
                gate_pause("MINDSPACE_ISOLATED_GATE_PAUSE_COMMITTED");
            }
            result
        },
        #[cfg(feature = "spawn-gate")]
        || gate_pause("MINDSPACE_ISOLATED_GATE_PAUSE_AFTER"),
        #[cfg(feature = "spawn-gate")]
        std::env::var_os("MINDSPACE_ISOLATED_GATE_DROP_COMMIT_DONE").is_some(),
    );
    let persist_ms = t_persist.elapsed().as_millis() as u64;
    let report =
        match result {
            Ok(report) => report,
            Err(PersistFailure::Denied {
                kind,
                reason,
                discard_link,
            }) => {
                return Err(iso.lease_refusal(kind, &reason, discard_link));
            }
            Err(PersistFailure::Store(e)) => {
                return Err(match e {
                crate::core::Error::CommitOutcomeUnknown { detail } => failure_commit(
                    "commit_outcome_unknown",
                    format!(
                        "the outcome of the database commit is unknown ({detail}); do not assume \
                         either result. The database handle of this server was discarded: restart \
                         the server, then ingest the same directory again (it reuses what was \
                         committed)"
                    ),
                    "UNKNOWN",
                    json!("unknown"),
                ),
                // the handle was discarded by an earlier uncertain outcome: it refuses to start a
                // write, so nothing was written (but the worker ran for nothing)
                crate::core::Error::ConnectionDiscarded => failure_commit(
                    "store_discarded",
                    "the database handle of this server was discarded after an uncertain commit \
                     outcome; restart the server",
                    "NOT_STARTED",
                    json!(false),
                ),
                error @ crate::core::Error::IngestWriteNotStarted { .. } => failure_commit(
                    "persist_failed", error.to_string(), "NOT_STARTED", json!(false),
                ),
                error @ crate::core::Error::IngestRollbackUnconfirmed { .. } => failure_commit(
                    "rollback_unconfirmed", error.to_string(), "NOT_COMMITTED", json!(false),
                ),
                crate::core::Error::IngestTimeout { message, writing } => failure_commit(
                    "timeout",
                    message,
                    if writing {
                        "ROLLED_BACK"
                    } else {
                        "NOT_STARTED"
                    },
                    json!(false),
                ),
                other => failure_commit(
                    "persist_failed",
                    other.to_string(),
                    "ROLLED_BACK",
                    json!(false),
                ),
            });
            }
        };
    drop(link);
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
        "commit_state": "COMMITTED",
        "commit_authorization": "granted by the broker immediately before the COMMIT",
        "elapsed_ms": t0.elapsed().as_millis() as u64,
        "sandbox": config.sandbox().label(),
        "seccomp": config.seccomp(),
        "next": "Use code_search for definitions and memory_search for documents."
    }))
}
