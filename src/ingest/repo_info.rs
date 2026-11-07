//! Read-only description of a git repository: identity, HEAD, remotes with
//! slugs, and worktrees. Opens no database and writes nothing, in particular
//! no identity marker.

use std::path::Path;

use serde::Serialize;
use uuid::Uuid;

use crate::core::{choose_primary_slug, Error, GitObjectKind, PrimarySlug, Result};

use super::git::{self, GitReader};
use super::remote;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemoteInfo {
    pub name: String,
    /// The configured URL with credentials removed.
    pub url: String,
    /// Lowercased `host/owner/name`, when the URL has one.
    pub slug_key: Option<String>,
    /// `Owner/Name` as written.
    pub slug: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorktreeInfo {
    /// `None` when redacted for a caller that may not see this path.
    pub path: Option<String>,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    pub prunable: bool,
    pub is_main: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeadInfo {
    pub commit: String,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoInfo {
    pub root: String,
    /// Mindspace identity, present once the repository has been ingested.
    pub identity: Option<Uuid>,
    pub head: Option<HeadInfo>,
    /// `Owner/Name` of the primary slug, see `slug_source`.
    pub slug: Option<String>,
    pub slug_key: Option<String>,
    /// Why this slug was chosen: the `origin` remote, or the only remote slug.
    pub slug_source: Option<String>,
    /// Set instead of a slug when remotes disagree and there is no `origin`.
    pub slug_candidates: Vec<String>,
    pub remotes: Vec<RemoteInfo>,
    pub worktrees: Vec<WorktreeInfo>,
}

pub fn repo_info(path: &Path) -> Result<RepoInfo> {
    let canonical = std::fs::canonicalize(path).map_err(|e| Error::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    if !canonical.is_dir() || !git::looks_like_git_repository(&canonical) {
        return Err(Error::InvalidPath {
            path: path.display().to_string(),
            reason: "not a git repository".into(),
        });
    }
    let reader = GitReader::open(&canonical)?;

    let remotes: Vec<RemoteInfo> = reader
        .remotes()?
        .into_iter()
        .map(|(name, raw)| {
            let parsed = remote::parse(&raw);
            RemoteInfo {
                name,
                url: parsed.url,
                slug_key: parsed.slug.as_ref().map(|s| s.key()),
                slug: parsed.slug.as_ref().map(|s| s.short()),
            }
        })
        .collect();

    // Primary slug: the `origin` remote when it has one; otherwise the single
    // slug all remotes agree on. Anything else stays ambiguous and unnamed.
    let triples: Vec<(String, Option<String>, Option<String>)> = remotes
        .iter()
        .map(|r| (r.name.clone(), r.slug_key.clone(), r.slug.clone()))
        .collect();
    let (mut slug, mut slug_key, mut slug_source, mut slug_candidates) =
        (None, None, None, Vec::new());
    match choose_primary_slug(&triples) {
        PrimarySlug::Chosen { key, short, source } => {
            slug = Some(short);
            slug_key = Some(key);
            slug_source = Some(source);
        }
        PrimarySlug::Ambiguous(keys) => slug_candidates = keys,
        PrimarySlug::None => {}
    }

    let head = reader
        .refs()?
        .into_iter()
        .find(|r| r.name == "HEAD" && r.target_kind == GitObjectKind::Commit)
        .map(|h| HeadInfo {
            commit: h.target_oid,
            branch: h
                .symbolic_target
                .map(|b| b.trim_start_matches("refs/heads/").to_string()),
        });

    let worktrees = reader
        .worktrees()?
        .into_iter()
        .map(|w| WorktreeInfo {
            path: Some(w.path),
            head: w.head,
            branch: w.branch,
            detached: w.detached,
            bare: w.bare,
            prunable: w.prunable,
            is_main: w.is_main,
        })
        .collect();

    Ok(RepoInfo {
        root: canonical.display().to_string(),
        identity: reader.existing_repository_id()?,
        head,
        slug,
        slug_key,
        slug_source,
        slug_candidates,
        remotes,
        worktrees,
    })
}
