//! `repo_outline` and `repo_ingest`: let an agent look at a repository and
//! ingest chosen parts of it, behind a restrictive allowlist.
//!
//! The MCP server has no authentication, and these tools read the local
//! filesystem and (for git) write a repository identity marker into the
//! repository's common dir. They therefore fail closed:
//!
//! - no allowed roots (the default) means both tools refuse every call;
//! - roots are canonicalized and validated at startup, `/` is refused;
//! - a requested path must be absolute, and after `canonicalize` (symlinks
//!   and `..` resolved) must lie inside an allowed root;
//! - paths with a `.git` component are refused;
//! - a refused path reveals nothing about whether it exists.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::core::{Error, IngestOutcome, Result};
use crate::ingest::{self, outline::PathSelection, GitScope, GitViews};
use crate::storage::Store;

/// An unscoped ingest of more ingestable files than this is refused: the
/// call is synchronous, and the agent should pick directories from the
/// outline instead.
pub const MAX_UNSCOPED_INGEST_FILES: u64 = 2000;
/// Outline responses are capped so a huge tree cannot flood the context.
pub const MAX_OUTLINE_NODES: usize = 300;
const SAMPLE: usize = 10;

#[derive(Debug, Clone, Default)]
pub struct McpConfig {
    allow_roots: Vec<PathBuf>,
}

impl McpConfig {
    /// No roots: the repository tools are disabled.
    pub fn none() -> Self {
        Self::default()
    }

    pub fn new<P: AsRef<Path>>(roots: &[P]) -> Result<Self> {
        let mut allow_roots = Vec::new();
        for root in roots {
            let root = root.as_ref();
            let canonical = std::fs::canonicalize(root).map_err(|e| Error::Io {
                path: root.display().to_string(),
                source: e,
            })?;
            if !canonical.is_dir() {
                return Err(Error::InvalidPath {
                    path: root.display().to_string(),
                    reason: "allowed root must be a directory".into(),
                });
            }
            if canonical.parent().is_none() {
                return Err(Error::InvalidPath {
                    path: root.display().to_string(),
                    reason: "refusing to allow the filesystem root; name a specific directory"
                        .into(),
                });
            }
            allow_roots.push(canonical);
        }
        allow_roots.sort();
        allow_roots.dedup();
        Ok(Self { allow_roots })
    }

    pub fn allow_roots(&self) -> &[PathBuf] {
        &self.allow_roots
    }

    /// Resolves a requested path to a canonical directory inside an allowed
    /// root, or explains the refusal without disclosing what exists.
    pub fn authorize(&self, raw: &str) -> std::result::Result<PathBuf, String> {
        if self.allow_roots.is_empty() {
            return Err("repository tools are disabled: this server has no allowed roots; \
                        start it with `mind mcp --allow-root <DIR>` (or MINDSPACE_MCP_ALLOW_ROOTS)"
                .to_string());
        }
        let path = Path::new(raw);
        if !path.is_absolute() {
            return Err(format!("path must be absolute: '{raw}'"));
        }
        let denied = || {
            format!(
                "path is not accessible or outside the allowed roots ({})",
                self.allow_roots
                    .iter()
                    .map(|r| r.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let canonical = std::fs::canonicalize(path).map_err(|_| denied())?;
        if !self.allow_roots.iter().any(|r| canonical.starts_with(r)) {
            return Err(denied());
        }
        if canonical.components().any(|c| c.as_os_str() == ".git") {
            return Err("paths inside .git are not allowed".to_string());
        }
        if !canonical.is_dir() {
            return Err("path must be a directory or a git repository root".to_string());
        }
        Ok(canonical)
    }
}

pub fn outline(config: &McpConfig, obj: &serde_json::Map<String, Value>) -> Result<Value> {
    let path = config.authorize(str_arg(obj, "path")?).map_err(Error::Storage)?;
    let depth = obj
        .get("depth")
        .and_then(Value::as_u64)
        .map(|d| d as u32)
        .unwrap_or(2);
    let mut outline = ingest::scan_path(&path, depth)?;
    let total = outline.nodes.len();
    let truncated = total > MAX_OUTLINE_NODES;
    if truncated {
        // Keep the shallowest nodes: they carry the structure.
        outline
            .nodes
            .sort_by(|a, b| (a.depth, &a.path).cmp(&(b.depth, &b.path)));
        outline.nodes.truncate(MAX_OUTLINE_NODES);
        outline
            .nodes
            .sort_by(|a, b| (a.depth != 0, &a.path).cmp(&(b.depth != 0, &b.path)));
    }
    Ok(json!({
        "outline": outline,
        "node_count": total,
        "truncated": truncated,
        "hint": "Pick directories with ingestable_files > 0 and pass them as `paths` to repo_ingest."
    }))
}

pub fn ingest(
    store: &dyn Store,
    config: &McpConfig,
    obj: &serde_json::Map<String, Value>,
) -> Result<Value> {
    let path = config.authorize(str_arg(obj, "path")?).map_err(Error::Storage)?;
    let spaces = string_list(obj, "spaces")?;
    let selected = string_list(obj, "paths").unwrap_or_default();
    let selection = PathSelection::new(&selected)?;
    let flag = |k: &str| obj.get(k).and_then(Value::as_bool).unwrap_or(false);
    let scope = if flag("history") {
        GitScope::History
    } else if flag("branches") || flag("remotes") || flag("tags") {
        GitScope::Views(GitViews {
            branches: flag("branches"),
            remotes: flag("remotes"),
            tags: flag("tags"),
        })
    } else {
        GitScope::Head
    };

    if selection.is_all() {
        let total = ingest::scan_path(&path, 0)?
            .nodes
            .first()
            .map(|n| n.ingestable_files)
            .unwrap_or(0);
        if total > MAX_UNSCOPED_INGEST_FILES {
            return Err(Error::Storage(format!(
                "refusing an unscoped ingest of {total} ingestable files (limit \
                 {MAX_UNSCOPED_INGEST_FILES}); call repo_outline and pass the directories you \
                 need as `paths`"
            )));
        }
    }

    let outcomes = ingest::ingest_paths(store, &path, &selection, scope, &spaces)?;
    Ok(json!({
        "ingested": outcomes.iter().map(summarize).collect::<Vec<_>>(),
        "next": "Use code_search for definitions and memory_search for documents."
    }))
}

fn summarize(outcome: &IngestOutcome) -> Value {
    fn capped(v: Value) -> Value {
        let items = v.as_array().cloned().unwrap_or_default();
        json!({ "total": items.len(), "sample": items.into_iter().take(SAMPLE).collect::<Vec<_>>() })
    }
    match outcome {
        IngestOutcome::Files(report) => {
            let mut v = serde_json::to_value(report).unwrap_or(Value::Null);
            v["kind"] = json!("files");
            v["files"] = capped(v["files"].clone());
            v["events"] = capped(v["events"].clone());
            v
        }
        IngestOutcome::Git(report) => {
            let mut v = serde_json::to_value(report).unwrap_or(Value::Null);
            v["kind"] = json!("git");
            let files = v["files"].as_array().cloned().unwrap_or_default();
            v["files"] = json!({
                "total": files.len(),
                "sample": files.iter().take(SAMPLE).map(|f| json!({
                    "path": f["path"], "commit_oid": f["commit_oid"], "outcome": f["outcome"]
                })).collect::<Vec<_>>()
            });
            v["events"] = capped(v["events"].clone());
            v
        }
    }
}

fn str_arg<'a>(obj: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a str> {
    obj.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Storage(format!("missing or invalid string argument '{key}'")))
}

fn string_list(obj: &serde_json::Map<String, Value>, key: &str) -> Result<Vec<String>> {
    obj.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Storage(format!("missing or invalid array argument '{key}'")))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| Error::Storage(format!("argument '{key}' entries must be strings")))
        })
        .collect()
}
