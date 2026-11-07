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
//! - a refused path reveals nothing about whether it exists;
//! - roots are read-only (`:ro`) or read-write (`:rw`, the default).
//!   `repo_outline` works on any root, `repo_ingest` needs a read-write one.
//!   The most specific (longest) root governing a path decides, the same
//!   directory listed twice gets the more restrictive mode, and an ingest
//!   target may not contain a read-only root.

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

/// What a root permits. Ordered so that `min` is the more restrictive mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Access {
    ReadOnly,
    ReadWrite,
}

impl Access {
    fn as_str(self) -> &'static str {
        match self {
            Access::ReadOnly => "ro",
            Access::ReadWrite => "rw",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowRoot {
    pub path: PathBuf,
    pub access: Access,
}

#[derive(Debug, Clone, Default)]
pub struct McpConfig {
    roots: Vec<AllowRoot>,
}

impl McpConfig {
    /// No roots: the repository tools are disabled.
    pub fn none() -> Self {
        Self::default()
    }

    /// Read-write roots (the behavior of a root without a suffix).
    pub fn new<P: AsRef<Path>>(roots: &[P]) -> Result<Self> {
        Self::from_roots(roots.iter().map(|r| (r.as_ref().to_path_buf(), Access::ReadWrite)))
    }

    /// Roots from `--allow-root` / `MINDSPACE_MCP_ALLOW_ROOTS` values. Each
    /// value is `PATH[:ro|:rw]`, and several may be joined with `:`
    /// (`/a:ro:/b:rw`). A mode belongs to the path before it *within the same
    /// value*, so a lone `--allow-root ro` is an error rather than a mode for
    /// the previous argument. A directory literally named `ro` or `rw` is
    /// written `./ro`. A value with more than one part must use absolute or
    /// explicit `./` / `../` paths, so a mistyped mode such as `/a:rx` cannot
    /// silently become a second, relative root.
    pub fn from_specs<S: AsRef<str>>(specs: &[S]) -> Result<Self> {
        let mut parsed = Vec::new();
        for spec in specs {
            parsed.extend(parse_spec(spec.as_ref())?);
        }
        Self::from_roots(parsed)
    }

    fn from_roots<I: IntoIterator<Item = (PathBuf, Access)>>(roots: I) -> Result<Self> {
        let mut merged: std::collections::BTreeMap<PathBuf, Access> = Default::default();
        for (root, access) in roots {
            let canonical = std::fs::canonicalize(&root).map_err(|e| Error::Io {
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
            // The same directory twice (also via a symlink or a trailing
            // slash) keeps the more restrictive mode.
            merged
                .entry(canonical)
                .and_modify(|a| *a = (*a).min(access))
                .or_insert(access);
        }
        Ok(Self {
            roots: merged
                .into_iter()
                .map(|(path, access)| AllowRoot { path, access })
                .collect(),
        })
    }

    pub fn roots(&self) -> &[AllowRoot] {
        &self.roots
    }

    /// The root that governs `path`: the most specific (longest) one that
    /// contains it. Roots are unique after canonicalization, so there are no ties.
    fn governing(&self, path: &Path) -> Option<&AllowRoot> {
        self.roots
            .iter()
            .filter(|r| path.starts_with(&r.path))
            .max_by_key(|r| r.path.components().count())
    }

    /// Lexical check that a path string lies inside an allowed root, used to
    /// redact paths in responses (it does not touch the filesystem).
    pub fn is_inside(&self, path: &str) -> bool {
        self.roots.iter().any(|r| Path::new(path).starts_with(&r.path))
    }

    /// Resolves a requested path to a canonical directory inside an allowed
    /// root that grants at least `need`, or explains the refusal without
    /// disclosing what exists. The permission is decided on the canonical
    /// path, so a symlink cannot move a request into a more permissive root.
    pub fn authorize(&self, raw: &str, need: Access) -> std::result::Result<PathBuf, String> {
        if self.roots.is_empty() {
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
                self.roots
                    .iter()
                    .map(|r| format!("{}:{}", r.path.display(), r.access.as_str()))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let canonical = std::fs::canonicalize(path).map_err(|_| denied())?;
        let root = self.governing(&canonical).ok_or_else(denied)?;
        if need == Access::ReadWrite && root.access == Access::ReadOnly {
            return Err(read_only_message(&root.path));
        }
        if canonical.components().any(|c| c.as_os_str() == ".git") {
            return Err("paths inside .git are not allowed".to_string());
        }
        if !canonical.is_dir() {
            return Err("path must be a directory or a git repository root".to_string());
        }
        Ok(canonical)
    }

    /// Checks what an ingest would cover, after [`authorize`](Self::authorize)
    /// accepted `base`. Every target (`base` itself, or each selected path)
    /// must be governed by a read-write root, and must not contain a read-only
    /// root: otherwise ingesting a parent would capture the read-only part.
    /// Selected paths are checked lexically; directory ingest does not follow
    /// symlinks and git ingest reads objects, not the working tree.
    pub fn check_ingest_scope(
        &self,
        base: &Path,
        selection: &PathSelection,
    ) -> std::result::Result<(), String> {
        let targets: Vec<(String, PathBuf)> = if selection.is_all() {
            vec![(".".to_string(), base.to_path_buf())]
        } else {
            selection
                .prefixes()
                .iter()
                .map(|p| (p.clone(), base.join(p)))
                .collect()
        };
        for (label, target) in targets {
            if let Some(root) = self.governing(&target) {
                if root.access == Access::ReadOnly {
                    return Err(read_only_message(&root.path));
                }
            }
            if let Some(inner) = self
                .roots
                .iter()
                .find(|r| r.access == Access::ReadOnly && r.path != target && r.path.starts_with(&target))
            {
                return Err(format!(
                    "ingesting '{label}' would include the read-only root {}; select paths \
                     that avoid it (see repo_outline) or make that root read-write",
                    inner.path.display()
                ));
            }
        }
        Ok(())
    }
}

fn read_only_message(root: &Path) -> String {
    format!(
        "path is inside the read-only root {root}; ingesting needs a read-write root \
         (start the server with --allow-root {root}:rw)",
        root = root.display()
    )
}

/// Parses one `--allow-root` value, see [`McpConfig::from_specs`].
fn parse_spec(spec: &str) -> Result<Vec<(PathBuf, Access)>> {
    let bad = |reason: &str| Error::InvalidPath {
        path: spec.to_string(),
        reason: reason.to_string(),
    };
    if spec.is_empty() {
        return Err(bad("empty allowed root"));
    }
    let parts: Vec<&str> = spec.split(':').collect();
    let multi = parts.len() > 1;
    let mut out: Vec<(PathBuf, Option<Access>)> = Vec::new();
    for part in parts {
        match part {
            "" => return Err(bad("empty path or stray ':' in allowed root")),
            "ro" | "rw" => {
                let mode = if part == "ro" { Access::ReadOnly } else { Access::ReadWrite };
                match out.last_mut() {
                    None => {
                        return Err(bad(
                            "mode without a path; write PATH:ro or PATH:rw \
                             (a directory named 'ro' is './ro')",
                        ))
                    }
                    Some((_, Some(_))) => return Err(bad("more than one mode for one path")),
                    Some((_, m)) => *m = Some(mode),
                }
            }
            p if p.eq_ignore_ascii_case("ro") || p.eq_ignore_ascii_case("rw") => {
                return Err(bad("modes are lowercase: 'ro' or 'rw'"));
            }
            p => {
                let explicit = p.starts_with('/') || p.starts_with("./") || p.starts_with("../");
                if multi && !explicit {
                    return Err(bad(
                        "unknown mode or relative path; modes are 'ro' or 'rw', and a path \
                         in a multi-part value must be absolute or start with ./ or ../",
                    ));
                }
                out.push((PathBuf::from(p), None));
            }
        }
    }
    Ok(out
        .into_iter()
        .map(|(p, m)| (p, m.unwrap_or(Access::ReadWrite)))
        .collect())
}

pub fn outline(config: &McpConfig, obj: &serde_json::Map<String, Value>) -> Result<Value> {
    let path = config
        .authorize(str_arg(obj, "path")?, Access::ReadOnly)
        .map_err(Error::Storage)?;
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
    // Repository facts for choosing a space; worktree paths outside the
    // allowed roots are redacted so the tool cannot be used to learn them.
    let repository = (outline.source == "git")
        .then(|| ingest::repo_info::repo_info(&path).ok())
        .flatten()
        .map(|mut info| {
            for w in &mut info.worktrees {
                if w.path.as_deref().is_some_and(|p| !config.is_inside(p)) {
                    w.path = None;
                }
            }
            info
        });
    Ok(json!({
        "outline": outline,
        "repository": repository,
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
    let path = config
        .authorize(str_arg(obj, "path")?, Access::ReadWrite)
        .map_err(Error::Storage)?;
    let spaces = string_list(obj, "spaces")?;
    let selected = string_list(obj, "paths").unwrap_or_default();
    let selection = PathSelection::new(&selected)?;
    config
        .check_ingest_scope(&path, &selection)
        .map_err(Error::Storage)?;
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
