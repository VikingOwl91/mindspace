//! Read-only repository outline and path selection.
//!
//! `mind scan` answers "what is in here, and where?" without touching the
//! database or any repository state: per-directory file counts, bytes, and
//! file kinds down to a bounded depth. The result is meant to drive a
//! targeted ingest (`mind ingest <repo> --path crates/foo --path docs`).
//!
//! Git repositories are outlined from the object database at HEAD (the same
//! source ingest reads), plain directories from the filesystem with
//! project-local ignore rules. Both are deterministic for the same input.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::{Error, Result};

/// Coarse file kind used in outlines. `other` is never ingested.
pub fn file_kind(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("md" | "markdown") => "markdown",
        Some("txt") => "text",
        Some("rs") => "rust",
        _ => "other",
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct OutlineNode {
    /// Directory path relative to the root, `/`-separated; `.` is the root.
    pub path: String,
    pub depth: u32,
    /// All files below this directory, recursively.
    pub files: u64,
    pub bytes: u64,
    /// Files whose kind Mindspace can ingest (anything but `other`).
    pub ingestable_files: u64,
    pub kinds: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Outline {
    pub root: String,
    /// `git` or `directory`.
    pub source: String,
    /// The commit outlined, for git sources.
    pub commit: Option<String>,
    pub depth: u32,
    /// Root first, then directories sorted by path.
    pub nodes: Vec<OutlineNode>,
}

/// Aggregates `(relative path, size)` pairs into directory nodes down to
/// `depth`. Files count toward every ancestor directory within the depth.
pub fn build_nodes<I>(files: I, depth: u32) -> Vec<OutlineNode>
where
    I: IntoIterator<Item = (String, u64)>,
{
    let mut nodes: BTreeMap<String, OutlineNode> = BTreeMap::new();
    let mut root = OutlineNode {
        path: ".".to_string(),
        depth: 0,
        ..Default::default()
    };
    for (path, size) in files {
        let kind = file_kind(Path::new(&path));
        let dirs: Vec<&str> = {
            let mut parts: Vec<&str> = path.split('/').collect();
            parts.pop(); // the file name
            parts
        };
        let apply = |node: &mut OutlineNode| {
            node.files += 1;
            node.bytes += size;
            if kind != "other" {
                node.ingestable_files += 1;
            }
            *node.kinds.entry(kind.to_string()).or_default() += 1;
        };
        apply(&mut root);
        for d in 1..=(depth as usize).min(dirs.len()) {
            let key = dirs[..d].join("/");
            let node = nodes.entry(key.clone()).or_insert_with(|| OutlineNode {
                path: key,
                depth: d as u32,
                ..Default::default()
            });
            apply(node);
        }
    }
    let mut out = vec![root];
    out.extend(nodes.into_values());
    out
}

/// A validated set of include prefixes. Empty means "everything".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathSelection {
    prefixes: Vec<String>,
}

impl PathSelection {
    pub fn all() -> Self {
        Self::default()
    }

    /// Accepts repository-relative directory or file paths. Rejects absolute
    /// paths and `..` so a selection can never leave the ingest root.
    pub fn new<S: AsRef<str>>(paths: &[S]) -> Result<Self> {
        let mut prefixes = Vec::new();
        for raw in paths {
            let raw = raw.as_ref();
            let normalized = raw
                .trim()
                .trim_start_matches("./")
                .trim_end_matches('/')
                .to_string();
            let invalid = |reason: &str| Error::InvalidPath {
                path: raw.to_string(),
                reason: reason.to_string(),
            };
            if normalized.is_empty() || normalized == "." {
                return Err(invalid("empty path; omit --path to select everything"));
            }
            if normalized.starts_with('/') || Path::new(&normalized).is_absolute() {
                return Err(invalid("must be relative to the ingest root"));
            }
            if normalized.split('/').any(|c| c == ".." || c.is_empty()) {
                return Err(invalid("must not contain '..' or empty components"));
            }
            prefixes.push(normalized);
        }
        prefixes.sort();
        prefixes.dedup();
        Ok(Self { prefixes })
    }

    pub fn is_all(&self) -> bool {
        self.prefixes.is_empty()
    }

    pub fn prefixes(&self) -> &[String] {
        &self.prefixes
    }

    /// True when `path` equals a prefix or lies below it (component-wise,
    /// so `doc` does not select `docs/x`).
    pub fn matches(&self, path: &str) -> bool {
        self.is_all()
            || self.prefixes.iter().any(|p| {
                path == p
                    || path
                        .strip_prefix(p.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            })
    }
}

/// Deterministic, ignore-aware walk of ordinary files below `root`.
///
/// Project-local `.gitignore` and `.ignore` files apply (even outside a git
/// work tree); global and parent-directory ignore files do not, so the result
/// depends only on the contents of `root`. `.git` is never entered, hidden
/// files are included, and symlinks are skipped (cycle-safe, as before).
pub fn walk_files(root: &Path) -> Result<Vec<PathBuf>> {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .parents(false)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_path(|a, b| a.cmp(b))
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();
    let mut files = Vec::new();
    for entry in walker {
        let entry = entry.map_err(|e| Error::Io {
            path: root.display().to_string(),
            source: std::io::Error::other(e.to_string()),
        })?;
        if entry.file_type().is_some_and(|t| t.is_file()) {
            files.push(entry.into_path());
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(list: &[(&str, u64)]) -> Vec<(String, u64)> {
        list.iter().map(|(p, s)| (p.to_string(), *s)).collect()
    }

    #[test]
    fn aggregates_recursively_to_depth() {
        let nodes = build_nodes(
            files(&[
                ("README.md", 10),
                ("crates/a/src/lib.rs", 100),
                ("crates/a/src/x/y.rs", 50),
                ("crates/b/Cargo.toml", 5),
                ("docs/guide.md", 20),
            ]),
            2,
        );
        let by: BTreeMap<_, _> = nodes.iter().map(|n| (n.path.as_str(), n)).collect();
        assert_eq!(nodes[0].path, ".");
        assert_eq!(by["."].files, 5);
        assert_eq!(by["."].bytes, 185);
        assert_eq!(by["."].ingestable_files, 4);
        assert_eq!(by["crates"].files, 3);
        assert_eq!(by["crates/a"].kinds["rust"], 2);
        assert_eq!(by["crates/b"].kinds["other"], 1);
        assert!(!by.contains_key("crates/a/src"), "depth 2 stops here");
        assert_eq!(by["docs"].depth, 1);
    }

    #[test]
    fn depth_zero_is_root_only() {
        let nodes = build_nodes(files(&[("a/b.md", 1)]), 0);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].files, 1);
    }

    #[test]
    fn selection_matches_on_component_boundaries() {
        let sel = PathSelection::new(&["docs", "./crates/a/"]).unwrap();
        assert!(sel.matches("docs/x.md"));
        assert!(sel.matches("docs"));
        assert!(sel.matches("crates/a/src/lib.rs"));
        assert!(!sel.matches("documents/x.md"));
        assert!(!sel.matches("crates/ab/lib.rs"));
        assert!(PathSelection::all().matches("anything"));
    }

    #[test]
    fn selection_rejects_escaping_paths() {
        for bad in ["/etc", "../x", "a/../b", "", ".", "a//b"] {
            assert!(PathSelection::new(&[bad]).is_err(), "{bad:?} must fail");
        }
    }
}
