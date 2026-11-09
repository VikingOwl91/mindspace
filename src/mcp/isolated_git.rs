//! `repo_ingest_git_isolated` (M7.3b.5e): ingest the HEAD of a git repository through the broker's
//! confined Git worker. EXPERIMENTAL and doubly opt-in: the server must be started with
//! `--isolated-git-ingest` AND an administrator `--git-exe` that passed the executable trust
//! policy (`GitExecutable::resolve_trusted`) in stage 1, before the sandbox. Without both the tool
//! does not exist: it is not listed and cannot be called.
//!
//! The pipeline is the 5d one, reused as it is: broker authorization of the exact root (static
//! root or revocable grant) -> pinned root -> confined Git worker (the executable is the one stage
//! 1 approved, pinned and verified again before the worker confines itself) -> GitVerified
//! transport -> verification here (`check_result_git`) -> one atomic SQLite transaction with a
//! broker commit lease (`persist_git_with_lease`).
//!
//! What a success does NOT say: the worker's claim that a commit is the repository's HEAD is not
//! verified by anything here (the git objects are checked for internal consistency only: hash
//! agreement, a closed tree/commit structure). The result states this explicitly.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde_json::{json, Map, Value};

use crate::core::Uuid;
use crate::ingest::deadline::Deadline;
use crate::ingest::extract::StoreContext;
use crate::storage::Store;
use crate::worker::client::{
    extract_git_via_broker, persist_git_with_lease, PersistFailure, WorkerConfig, WorkerError,
};

use super::isolated::link_idle;
use super::repo_tools::McpConfig;
use super::tools::ToolFailure;

pub const TOOL: &str = "repo_ingest_git_isolated";

pub fn definition() -> Value {
    json!({
        "name": TOOL,
        "description": "EXPERIMENTAL. Ingest the HEAD of a git repository (not its history) through an isolated worker that runs git inside its own sandbox, started by a broker outside this server's sandbox. `path` must be the canonical absolute path of a repository root the broker is authorized to serve (a root this server was started with, or a directory granted with `mind grant`, also after start-up). The repository must carry an identity marker (`mind ingest` writes it once) and use a plain `.git` directory with SHA-1 objects; linked worktrees, alternates, grafts and SHA-256 repositories are refused. The caller cannot name a git executable, environment, arguments, a grant or a limit; the server administrator chose the executable. The result states what was verified: git objects are checked for internal consistency, but that the claimed commit is the repository's real HEAD is NOT verified. Failures are structured (`error.kind`, `persisted`, `commit_state`); an unknown commit outcome is reported as unknown, never as rolled back.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1, "description": "Canonical absolute path of the repository root (the directory that contains `.git`)." },
                "spaces": { "type": "array", "items": { "type": "string", "minLength": 1 }, "minItems": 1, "maxItems": 64 }
            },
            "required": ["path", "spaces"],
            "additionalProperties": false
        }
    })
}

fn failure(kind: &str, message: impl Into<String>) -> ToolFailure {
    failure_commit(kind, message, "NOT_STARTED", json!(false))
}

/// `commit_state`: NOT_STARTED (failed before any write), ROLLED_BACK (nothing committed, certain)
/// or UNKNOWN (`CommitOutcomeUnknown`: the data may or may not be committed).
fn failure_commit(
    kind: &str,
    message: impl Into<String>,
    commit_state: &str,
    persisted: Value,
) -> ToolFailure {
    let mut structured = json!({
        "error": { "kind": kind, "message": message.into() },
        "persisted": persisted,
        "commit_state": commit_state
    });
    if commit_state != "NOT_STARTED" {
        structured["atomic"] = json!(true);
    }
    ToolFailure {
        message: structured.to_string(),
        structured: Some(structured),
    }
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
        Refused(m) if m.contains("identity_missing") => "identity_missing",
        Refused(m) if m.contains("unsupported repository") => "unsupported_repository",
        Refused(m) if m.contains("not a git repository") => "not_a_repository",
        Refused(m) if m.contains("git extraction is not configured") => "git_unavailable",
        Refused(m) if m.contains("git executable") || m.contains("before it was confined") => {
            "git_unavailable"
        }
        Refused(m) if m.contains("object id") || m.contains("object format") => {
            "unsupported_object_format"
        }
        Refused(_) => "refused",
        Revoked => "revoked",
        Timeout(_) => "timeout",
        Cancelled => "cancelled",
        Context(_) => "context_refused",
        Stream(m) if m.contains("unsupported object") || m.contains("SHA-1") => {
            "unsupported_object_format"
        }
        Stream(m) if m.contains("exceed") || m.contains("limit") => "too_large",
        Stream(_) => "result_refused",
        Sandbox(_) => "confinement_not_verified",
        Died { .. } => "worker_failed",
        Spawn(_) | Io(_) => "broker_io",
    };
    (failure(kind, text), poison)
}

fn own_cgroup() -> Option<String> {
    std::fs::read_to_string("/proc/self/cgroup")
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("0::").map(|p| p.trim().to_string()))
}

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

pub fn run(
    store: &dyn Store,
    config: &McpConfig,
    obj: &Map<String, Value>,
) -> std::result::Result<Value, ToolFailure> {
    let Some(iso) = config.isolated_git() else {
        return Err(failure(
            "not_enabled",
            "repo_ingest_git_isolated is not enabled on this server",
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
    // The path is stored as the repository's path: it must already be in its lexical canonical
    // form (no `.`, `..`, empty or trailing-slash components). Symlinks are the broker's business.
    {
        use std::path::Component;
        let p = Path::new(path);
        let plain = p
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)));
        if !plain || p.components().collect::<std::path::PathBuf>().as_os_str() != p.as_os_str() {
            return Err(failure(
                "invalid_argument",
                "argument 'path' must be a canonical absolute path (no `.`, `..` or trailing `/`)",
            ));
        }
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
    let mut cfg = WorkerConfig::new("/nonexistent"); // the broker chooses and starts the worker
    cfg.wall_timeout = config.ingest_timeout();
    #[cfg(feature = "spawn-gate")]
    if std::env::var_os("MINDSPACE_ISOLATED_GATE_NO_EVIDENCE").is_some() {
        cfg.require_sandbox_evidence = false;
    }
    let cancel = AtomicBool::new(false);
    let t0 = Instant::now();
    let (extracted, stats, rep) = match extract_git_via_broker(
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

    // what the worker CLAIMED is HEAD (reported back, not verified)
    let head = extracted
        .observation
        .refs
        .iter()
        .find(|r| r.name == "HEAD")
        .map(|r| (r.target_oid.clone(), r.symbolic_target.clone()));

    #[cfg(feature = "spawn-gate")]
    {
        use std::sync::atomic::Ordering::SeqCst;
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
        gate_pause("MINDSPACE_ISOLATED_GATE_PAUSE_BEFORE");
    }
    let t_persist = Instant::now();
    let result = persist_git_with_lease(store, &link, rep.op, extracted, &Deadline::none());
    #[cfg(feature = "spawn-gate")]
    if result.is_ok() {
        gate_pause("MINDSPACE_ISOLATED_GATE_PAUSE_COMMITTED");
    }
    drop(link);
    let persist_ms = t_persist.elapsed().as_millis() as u64;
    let report = match result {
        Ok(r) => r,
        Err(PersistFailure::Denied { kind, reason }) => {
            if kind == "broker_unavailable" {
                iso.poisoned.store(true, Ordering::SeqCst);
            }
            return Err(failure_commit(
                kind,
                format!("the commit was refused: {reason}"),
                "ROLLED_BACK",
                json!(false),
            ));
        }
        Err(PersistFailure::Store(e)) => {
            return Err(match e {
                crate::core::Error::CommitOutcomeUnknown { detail } => {
                    iso.poisoned.store(true, Ordering::SeqCst);
                    failure_commit(
                        "commit_outcome_unknown",
                        format!(
                            "the outcome of the database commit is unknown ({detail}); do not assume \
                             either result. The database handle of this server was discarded: restart \
                             the server, then ingest the same repository again (it reuses what was \
                             committed)"
                        ),
                        "UNKNOWN",
                        json!("unknown"),
                    )
                }
                crate::core::Error::ConnectionDiscarded => failure_commit(
                    "store_discarded",
                    "the database handle of this server was discarded after an uncertain commit \
                     outcome; restart the server",
                    "NOT_STARTED",
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
    // The store's current recording revision right after our commit. The ingestion made exactly one
    // revision; another writer between the commit and this read could only raise the number.
    let revision = store
        .temporal_history_capabilities()
        .map(|c| json!(c.current_revision))
        .unwrap_or(Value::Null);
    let authority = match rep.authority {
        crate::worker::broker::AuthoritySource::Static => "static_root",
        crate::worker::broker::AuthoritySource::Grant(_) => "grant",
    };
    Ok(json!({
        "repository": {
            "id": report.repository_id.to_string(),
            "path": report.repository_path,
            "identity_source": "marker `.git/mindspace-repository-id`, read by the broker (repository content: it is not authenticated)",
        },
        "head": {
            "commit": head.as_ref().map(|h| h.0.clone()),
            "symbolic_ref": head.as_ref().and_then(|h| h.1.clone()),
            "scope": "head_only",
        },
        "persisted": true,
        "commit_state": "COMMITTED",
        "persisted_revision": revision,
        "counts": {
            "refs_observed": report.refs_observed,
            "objects_discovered": report.objects_discovered,
            "objects_new": report.objects_new,
            "objects_existing": report.objects_existing,
            "derivations_new": report.derivations_new,
            "derivations_reused": report.derivations_reused,
            "stale_retired": report.stale_retired,
            "files": report.files.len(),
            "code": {
                "supported_inputs": report.code.supported_inputs,
                "parsed": report.code.parsed,
                "reused": report.code.reused,
                "entities": report.code.entities,
                "parse_errors": report.code.parse_errors,
                "skipped": report.code.skipped,
            },
        },
        "verification": {
            "internally_verified": "every git object this result contains hashes to its id and the commit/tree/blob structure is closed (GitVerified transport, checked by this server before anything was written)",
            "not_verified": [
                "that the claimed commit is the repository's real HEAD (the worker's claim)",
                "that the objects exist in the repository as the git on disk would serve them beyond what the worker read",
                "SHA-1 collision resistance (git's SHA-1 without collision detection)"
            ],
            "unverified_blob_claims": "blobs that are not valid UTF-8 text are recorded as (oid, size) only",
        },
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
        "commit_authorization": "granted by the broker immediately before the COMMIT",
        "persist_ms": persist_ms,
        "elapsed_ms": t0.elapsed().as_millis() as u64,
        "next": "Use code_search for definitions."
    }))
}
