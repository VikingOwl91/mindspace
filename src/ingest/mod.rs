pub mod code;
pub mod git;
pub mod markdown;
pub mod outline;
pub mod release;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::core::{
    normalize_subject, sha256_hex, AssertionDraft, Error, EvidenceDraft, GitDerivedFile,
    GitObjectKind, GitObservation, GitObservationReport, GitRefEventDraft, GitRefEventKind,
    IngestObservation, IngestOutcome, IngestScope, MemorySpec, ObservationReport, ObservedFile,
    Result, SessionDraft, Ts, Uuid,
};
use crate::storage::Store;

/// Which ref classes beyond HEAD and worktrees are interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GitViews {
    pub branches: bool,
    pub remotes: bool,
    pub tags: bool,
}

/// Which part of a git repository is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GitScope {
    /// The default: HEAD of the ingest path plus HEAD of every live linked
    /// worktree. These commits are the repository's current views; memories
    /// derived at any other commit are retired as stale. Refs are still
    /// journaled completely.
    #[default]
    Head,
    /// `Head` plus the selected ref classes (local branches, remote
    /// branches, tags). Every selected tip is a current view.
    Views(GitViews),
    /// Every ref tip is observed and interpreted (ADR-015). Derivations at
    /// different commits coexist and nothing is retired.
    History,
}

/// Ingests a path: a single file, a plain directory, or a git repository
/// (detected by the presence of `.git` or bare-repository markers).
///
/// - A file is a partial (file-scope) observation: path-local continuity
///   only.
/// - A directory is a full observation of that root: it also infers moves
///   and records disappearances against the previous full observation of
///   the same root.
/// - A git repository is an observation of committed objects and refs;
///   the working tree is never consulted.
///
/// Spaces must already exist and apply to every ingested file.
pub fn ingest_path<S: AsRef<str>>(
    store: &dyn Store,
    path: &Path,
    space_names: &[S],
) -> Result<IngestOutcome> {
    let canonical = std::fs::canonicalize(path).map_err(|e| Error::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    let metadata = std::fs::metadata(&canonical).map_err(|e| Error::Io {
        path: canonical.display().to_string(),
        source: e,
    })?;

    // A git repository wins over directory ingest: committed history is
    // what it observes, not the working tree.
    if metadata.is_dir() && git::looks_like_git_repository(&canonical) {
        return Ok(IngestOutcome::Git(ingest_git_repository(
            store,
            &canonical,
            space_names,
        )?));
    }

    // A single file must fail on its format before spaces are resolved.
    if metadata.is_file() {
        media_type_of(&canonical)?;
    }

    // Spaces must pre-exist; the first missing one fails the ingest.
    let space_ids = resolve_spaces(store, space_names)?;

    let (root, scope, files) = if metadata.is_dir() {
        (
            canonical.display().to_string(),
            IngestScope::Directory,
            observe_directory(&canonical, &space_ids)?,
        )
    } else {
        let file = observe_file(&canonical, &space_ids)?;
        (
            canonical.display().to_string(),
            IngestScope::File,
            vec![file],
        )
    };

    let mut code_inputs = Vec::new();
    for f in &files {
        let Some(language) = code::language_for(Path::new(&f.location)) else {
            continue;
        };
        let identity = code::identity();
        let known = store.has_code_derivation(language, &f.content_hash, None, None, &identity)?;
        code_inputs.push(crate::core::CodeInputDraft {
            language,
            content_hash: f.content_hash.clone(),
            content: f.content.clone(),
            repository_id: None,
            blob_oid: None,
            contexts: vec![crate::core::CodeContextDraft {
                path: f.location.clone(),
                commit_oid: None,
            }],
            derivation: if known {
                crate::core::CodeDerivationDraft {
                    identity,
                    status: crate::core::CodeParseStatus::Complete,
                    error_count: 0,
                    entities: vec![],
                    imports: vec![],
                    relationships: vec![],
                }
            } else {
                code::parse(language, &f.content)
            },
        });
    }
    let observation = IngestObservation {
        session: SessionDraft {
            root,
            scope,
            observed_at: Ts::now(),
        },
        files,
    };
    let report = store.record_observation(observation)?;
    if !code_inputs.is_empty() {
        let _ = store.record_code_observation(crate::core::CodeObservation {
            inputs: code_inputs,
        })?;
    }
    Ok(IngestOutcome::Files(report))
}

/// Ingests only the selected paths below `path`.
///
/// - Git repository: the whole object graph is observed, but only selected
///   files are interpreted.
/// - Plain directory: every selected sub-path is its own ingest root, so
///   lifecycle inference (moves, disappearances) stays scoped to it. Files
///   outside the selection are never marked absent.
///
/// An empty selection is the same as [`ingest_path`].
pub fn ingest_paths<S: AsRef<str>>(
    store: &dyn Store,
    path: &Path,
    selection: &outline::PathSelection,
    scope: GitScope,
    space_names: &[S],
) -> Result<Vec<IngestOutcome>> {
    let canonical_is_git = std::fs::canonicalize(path)
        .map(|c| c.is_dir() && git::looks_like_git_repository(&c))
        .unwrap_or(false);
    if selection.is_all() && !(canonical_is_git && scope != GitScope::Head) {
        return Ok(vec![ingest_path(store, path, space_names)?]);
    }
    let canonical = std::fs::canonicalize(path).map_err(|e| Error::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    if !canonical.is_dir() {
        return Err(Error::InvalidPath {
            path: path.display().to_string(),
            reason: "--path needs a directory or git repository as the ingest root".into(),
        });
    }
    if git::looks_like_git_repository(&canonical) {
        return Ok(vec![IngestOutcome::Git(ingest_git_repository_selected(
            store,
            &canonical,
            space_names,
            selection,
            scope,
        )?)]);
    }
    let mut outcomes = Vec::new();
    for prefix in selection.prefixes() {
        let target = std::fs::canonicalize(canonical.join(prefix)).map_err(|e| Error::Io {
            path: canonical.join(prefix).display().to_string(),
            source: e,
        })?;
        if !target.starts_with(&canonical) {
            return Err(Error::InvalidPath {
                path: prefix.clone(),
                reason: "resolves outside the ingest root".into(),
            });
        }
        outcomes.push(ingest_path(store, &target, space_names)?);
    }
    Ok(outcomes)
}

/// Read-only outline of a git repository (at HEAD) or directory, down to
/// `depth` directory levels. Opens no database and writes nothing.
pub fn scan_path(path: &Path, depth: u32) -> Result<outline::Outline> {
    let canonical = std::fs::canonicalize(path).map_err(|e| Error::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    if !canonical.is_dir() {
        return Err(Error::InvalidPath {
            path: path.display().to_string(),
            reason: "scan needs a directory or git repository".into(),
        });
    }
    let root = canonical.display().to_string();
    if git::looks_like_git_repository(&canonical) {
        let reader = git::GitReader::open(&canonical)?;
        let commit = reader.head_commit()?;
        let files = reader.commit_blob_sizes(&commit)?;
        return Ok(outline::Outline {
            root,
            source: "git".into(),
            commit: Some(commit),
            depth,
            nodes: outline::build_nodes(files, depth),
        });
    }
    let mut files = Vec::new();
    for file in outline::walk_files(&canonical)? {
        let rel = file
            .strip_prefix(&canonical)
            .unwrap_or(&file)
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
        files.push((rel, size));
    }
    Ok(outline::Outline {
        root,
        source: "directory".into(),
        commit: None,
        depth,
        nodes: outline::build_nodes(files, depth),
    })
}

/// Single-file ingest: a partial observation (kept for API compatibility
/// with the v0 tests and simple workflows).
pub fn ingest_file<S: AsRef<str>>(
    store: &dyn Store,
    path: &Path,
    space_names: &[S],
) -> Result<ObservationReport> {
    match ingest_path(store, path, space_names)? {
        IngestOutcome::Files(report) => Ok(report),
        IngestOutcome::Git(_) => Err(Error::UnsupportedFormat {
            path: path.display().to_string(),
        }),
    }
}

fn resolve_spaces<S: AsRef<str>>(store: &dyn Store, space_names: &[S]) -> Result<Vec<Uuid>> {
    let mut space_ids = Vec::with_capacity(space_names.len());
    for name in space_names {
        space_ids.push(store.get_space(name.as_ref())?.id);
    }
    Ok(space_ids)
}

// ---------------------------------------------------------------------------
// Git repository observation
// ---------------------------------------------------------------------------

/// Ingests one git repository: reads committed objects and refs through the
/// plumbing reader, derives text memories from ref tips through the normal
/// evidence/assertion pipeline, and hands everything to the store in a
/// single transaction.
pub fn ingest_git_repository<S: AsRef<str>>(
    store: &dyn Store,
    repo_path: &Path,
    space_names: &[S],
) -> Result<GitObservationReport> {
    ingest_git_repository_selected(
        store,
        repo_path,
        space_names,
        &outline::PathSelection::all(),
        GitScope::Head,
    )
}

/// Like [`ingest_git_repository`], but derives memories and code structure
/// only for files inside `selection`. The object graph and refs are still
/// observed completely; selection narrows what is interpreted.
pub fn ingest_git_repository_selected<S: AsRef<str>>(
    store: &dyn Store,
    repo_path: &Path,
    space_names: &[S],
    selection: &outline::PathSelection,
    scope: GitScope,
) -> Result<GitObservationReport> {
    let reader = git::GitReader::open(repo_path)?;
    let repository_id = reader.repository_id()?;
    let space_ids = resolve_spaces(store, space_names)?;

    // Refs: heads, remotes, tags, and HEAD, sorted by name.
    let refs = reader.refs()?;
    let views = match scope {
        GitScope::Views(v) => v,
        _ => GitViews::default(),
    };
    let worktree_heads: Vec<String> = match scope {
        GitScope::History => Vec::new(),
        _ => reader.worktree_heads()?,
    };
    let selected: Vec<&crate::core::GitRefObservation> = refs
        .iter()
        .filter(|r| match scope {
            GitScope::History => true,
            _ => {
                r.name == "HEAD"
                    || (views.branches && r.name.starts_with("refs/heads/"))
                    || (views.remotes && r.name.starts_with("refs/remotes/"))
                    || (views.tags && r.name.starts_with("refs/tags/"))
            }
        })
        .collect();

    // Object discovery: everything reachable from the scope's tips.
    let mut tips: Vec<String> = selected.iter().map(|r| r.target_oid.clone()).collect();
    tips.extend(worktree_heads.iter().cloned());
    tips.sort();
    tips.dedup();
    if tips.is_empty() && !refs.is_empty() {
        return Err(Error::GitCommand {
            command: "HEAD".into(),
            stderr: "HEAD does not resolve to a commit; use --history to observe all refs".into(),
        });
    }
    let reachable = reader.reachable_oids(&tips)?;
    let checked = reader.batch_check(&reachable)?;
    let mut structural: Vec<String> = Vec::new();
    let mut blob_sizes: Vec<(String, u64)> = Vec::new();
    for (oid, kind, size) in checked {
        if kind == GitObjectKind::Blob {
            blob_sizes.push((oid, size));
        } else {
            structural.push(oid);
        }
    }
    let (mut commits, trees, tags) = reader.objects(&reachable)?;
    let blob_datas: Vec<crate::core::GitBlobData> = blob_sizes
        .into_iter()
        .map(|(oid, size_bytes)| crate::core::GitBlobData {
            oid,
            size_bytes,
            content: None,
        })
        .collect();

    // Derivation tips: every commit a ref directly points at, plus the
    // commits annotated tags resolve to (peeled through nested tags).
    let mut tip_commits: BTreeSet<String> = selected
        .iter()
        .filter(|r| r.target_kind == GitObjectKind::Commit)
        .map(|r| r.target_oid.clone())
        .collect();
    tip_commits.extend(worktree_heads.iter().cloned());
    let tag_by_oid: std::collections::HashMap<&str, &crate::core::GitTagData> =
        tags.iter().map(|t| (t.oid.as_str(), t)).collect();
    let mut peel_queue: Vec<String> = tags
        .iter()
        .filter(|t| t.target_kind == GitObjectKind::Commit)
        .map(|t| t.target_oid.clone())
        .collect();
    while let Some(oid) = peel_queue.pop() {
        tip_commits.insert(oid.clone());
        // Nested tags (tag of a tag) keep peeling.
        if let Some(t) = tag_by_oid.get(oid.as_str()) {
            if t.target_kind == GitObjectKind::Tag {
                peel_queue.push(t.target_oid.clone());
            }
        }
    }

    // Text paths at each tip, then blob contents in one batch.
    let mut tip_files: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut text_blob_oids: BTreeSet<String> = BTreeSet::new();
    for commit in &tip_commits {
        let mut files: Vec<(String, String)> = Vec::new();
        for (mode, kind, oid, path) in reader.commit_files(commit)? {
            if kind != "blob" || mode.starts_with("12") {
                // Not a blob, or a symlink: never derived, never traversed.
                continue;
            }
            if !selection.matches(&path) {
                continue;
            }
            if !is_ingestable(Path::new(&path)) && code::language_for(Path::new(&path)).is_none() {
                continue;
            }
            text_blob_oids.insert(oid.clone());
            files.push((oid, path));
        }
        tip_files.push((commit.clone(), files));
    }
    let text_oids: Vec<String> = text_blob_oids.into_iter().collect();
    let contents = reader.blob_contents(&text_oids)?;
    let content_by_oid: std::collections::HashMap<String, String> = contents
        .into_iter()
        .map(|b| (b.oid, b.content.unwrap_or_default()))
        .collect();

    let mut derivations: Vec<GitDerivedFile> = Vec::new();
    for (commit, files) in tip_files {
        for (blob_oid, path) in files {
            let Some(content) = content_by_oid.get(&blob_oid) else {
                continue;
            };
            let media_type = media_type_of(Path::new(&path)).unwrap_or("text/rust");
            let stem = Path::new(&path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("untitled")
                .to_string();
            let mut parsed = if media_type == "text/rust" {
                ParsedMaterial {
                    title: stem.clone(),
                    evidences: vec![],
                    assertions: vec![],
                    memories: vec![],
                }
            } else {
                parse_material(&stem, content, media_type, &space_ids)
            };
            // Git derivations carry their own extractor identity: the rule
            // is the same parser, applied to git material.
            for a in &mut parsed.assertions {
                a.extraction = format!("git:{}", a.extraction);
            }
            derivations.push(GitDerivedFile {
                source_kind: crate::core::SourceKind::GitBlob,
                path,
                blob_oid,
                commit_oid: commit.clone(),
                content_hash: sha256_hex(content.as_bytes()),
                content: content.clone(),
                title: parsed.title,
                media_type: media_type.to_string(),
                evidences: parsed.evidences,
                assertions: parsed.assertions,
                memories: parsed.memories,
            });
        }
    }
    derivations.sort_by(|a, b| (&a.commit_oid, &a.path).cmp(&(&b.commit_oid, &b.path)));

    // Ref delta against the last observed state. The fast-forward question
    // is asked to git itself (ancestor check); anything not provably a
    // fast-forward is recorded as non_fast_forward, never as rebase or
    // force-push. A symbolic HEAD is a view of its branch, not an
    // independent pointer: it is observed in the ref state but produces no
    // events of its own. A detached HEAD is a real pointer and is journalled.
    let current = store.git_refs_current(repository_id)?;
    let current_by_name: std::collections::HashMap<String, &crate::core::GitRefObservation> =
        current.iter().map(|r| (r.name.clone(), r)).collect();
    let mut ref_events: Vec<GitRefEventDraft> = Vec::new();
    for r in &refs {
        if r.name == "HEAD" && r.symbolic_target.is_some() {
            continue;
        }
        match current_by_name.get(&r.name) {
            None => ref_events.push(GitRefEventDraft {
                name: r.name.clone(),
                kind: GitRefEventKind::Created,
                from_oid: None,
                to_oid: Some(r.target_oid.clone()),
            }),
            Some(prev) if prev.target_oid != r.target_oid => {
                let kind = if prev.target_kind == GitObjectKind::Commit
                    && r.target_kind == GitObjectKind::Commit
                    && reader.is_ancestor(&prev.target_oid, &r.target_oid)?
                {
                    GitRefEventKind::FastForward
                } else {
                    GitRefEventKind::NonFastForward
                };
                ref_events.push(GitRefEventDraft {
                    name: r.name.clone(),
                    kind,
                    from_oid: Some(prev.target_oid.clone()),
                    to_oid: Some(r.target_oid.clone()),
                });
            }
            Some(_) => {}
        }
    }
    for prev in &current {
        if prev.name == "HEAD" && prev.symbolic_target.is_some() {
            continue;
        }
        if !refs.iter().any(|r| r.name == prev.name) {
            ref_events.push(GitRefEventDraft {
                name: prev.name.clone(),
                kind: GitRefEventKind::Deleted,
                from_oid: Some(prev.target_oid.clone()),
                to_oid: None,
            });
        }
    }
    ref_events.sort_by(|a, b| a.name.cmp(&b.name));

    // Blob rows: content only for the text blobs actually fetched.
    let content_oids: BTreeSet<String> = derivations.iter().map(|d| d.blob_oid.clone()).collect();
    let mut blobs = blob_datas;
    for b in &mut blobs {
        if content_oids.contains(&b.oid) {
            b.content = content_by_oid.get(&b.oid).cloned();
        }
    }

    let code_inputs =
        tip_files_to_code_inputs(store, repository_id, &derivations, &content_by_oid)?;

    // Releases: every tag ref is a document-shaped memory, independent of
    // which commits are current views. A targeted (`--path`) ingest skips
    // them, since tags are not files.
    let current_tags = if selection.is_all() {
        let (tag_files, tag_commits) = release::derive(&reader, &refs, &space_ids)?;
        let known: BTreeSet<String> = commits.iter().map(|c| c.oid.clone()).collect();
        commits.extend(tag_commits.into_iter().filter(|c| !known.contains(&c.oid)));
        let keys = tag_files
            .iter()
            .map(|d| (d.path.clone(), d.blob_oid.clone()))
            .collect::<Vec<_>>();
        derivations.extend(tag_files);
        derivations.sort_by(|a, b| (&a.commit_oid, &a.path).cmp(&(&b.commit_oid, &b.path)));
        Some(keys)
    } else {
        None
    };
    let observation = GitObservation {
        session: SessionDraft {
            root: repo_path.display().to_string(),
            scope: IngestScope::GitRepository,
            observed_at: Ts::now(),
        },
        repository_id,
        repository_path: repo_path.display().to_string(),
        refs,
        commits,
        trees,
        blobs,
        tags,
        derivations,
        ref_events,
        current_tags,
        view_commits: tip_commits.iter().cloned().collect(),
        current_commits: (scope != GitScope::History)
            .then(|| tip_commits.iter().cloned().collect()),
    };
    let mut report = store.record_git_observation(observation)?;
    if !code_inputs.is_empty() {
        report.code = store.record_code_observation(crate::core::CodeObservation {
            inputs: code_inputs,
        })?;
        for commit in tip_commits {
            let _ = store.resolve_git_code_relationships(
                repository_id,
                &commit,
                "mind-rust-resolver",
                "1",
            )?;
        }
    }
    Ok(report)
}

fn tip_files_to_code_inputs(
    store: &dyn Store,
    repository_id: Uuid,
    derivations: &[GitDerivedFile],
    contents: &std::collections::HashMap<String, String>,
) -> Result<Vec<crate::core::CodeInputDraft>> {
    let mut grouped: std::collections::BTreeMap<String, crate::core::CodeInputDraft> =
        std::collections::BTreeMap::new();
    for d in derivations {
        let p = Path::new(&d.path);
        let Some(language) = code::language_for(p) else {
            continue;
        };
        let entry = grouped.entry(d.blob_oid.clone()).or_insert_with(|| {
            let content = contents.get(&d.blob_oid).cloned().unwrap_or_default();
            crate::core::CodeInputDraft {
                language,
                content_hash: sha256_hex(content.as_bytes()),
                content: content.clone(),
                repository_id: Some(repository_id),
                blob_oid: Some(d.blob_oid.clone()),
                contexts: vec![],
                derivation: if store
                    .has_code_derivation(
                        language,
                        &sha256_hex(content.as_bytes()),
                        Some(repository_id),
                        Some(&d.blob_oid),
                        &code::identity(),
                    )
                    .unwrap_or(false)
                {
                    crate::core::CodeDerivationDraft {
                        identity: code::identity(),
                        status: crate::core::CodeParseStatus::Complete,
                        error_count: 0,
                        entities: vec![],
                        imports: vec![],
                        relationships: vec![],
                    }
                } else {
                    code::parse(language, &content)
                },
            }
        });
        entry.contexts.push(crate::core::CodeContextDraft {
            path: d.path.clone(),
            commit_oid: Some(d.commit_oid.clone()),
        });
    }
    Ok(grouped.into_values().collect())
}

// ---------------------------------------------------------------------------
// Directory and file observation
// ---------------------------------------------------------------------------

/// Walks one directory deterministically (see [`outline::walk_files`]):
/// project-local ignore files apply, symlinks are skipped, and only
/// `.md`, `.markdown`, `.txt`, and `.rs` files take part in the observed set.
fn observe_directory(root: &Path, space_ids: &[Uuid]) -> Result<Vec<ObservedFile>> {
    let mut paths: Vec<PathBuf> = outline::walk_files(root)?
        .into_iter()
        .filter(|p| is_ingestable(p))
        .collect();
    paths.sort();
    paths.iter().map(|p| observe_file(p, space_ids)).collect()
}

fn is_ingestable(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("md" | "markdown" | "txt" | "rs")
    )
}

fn media_type_of(path: &Path) -> Result<&'static str> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("md" | "markdown") => Ok("text/markdown"),
        Some("txt") => Ok("text/plain"),
        Some("rs") => Ok("text/rust"),
        _ => Err(Error::UnsupportedFormat {
            path: path.display().to_string(),
        }),
    }
}

/// Reads and deterministically parses one file into an observed file.
fn observe_file(path: &Path, space_ids: &[Uuid]) -> Result<ObservedFile> {
    let media_type = media_type_of(path)?;
    let bytes = std::fs::read(path).map_err(|e| Error::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    let text = String::from_utf8(bytes.clone()).map_err(|_| Error::InvalidEncoding {
        path: path.display().to_string(),
    })?;
    let location = std::fs::canonicalize(path)
        .map_err(|e| Error::Io {
            path: path.display().to_string(),
            source: e,
        })?
        .display()
        .to_string();

    let stem = file_stem(path);
    let parsed = if media_type == "text/rust" {
        ParsedMaterial {
            title: stem.clone(),
            evidences: vec![],
            assertions: vec![],
            memories: vec![],
        }
    } else {
        parse_material(&stem, &text, media_type, space_ids)
    };
    Ok(ObservedFile {
        location,
        content: text,
        content_hash: sha256_hex(&bytes),
        title: parsed.title,
        media_type: media_type.to_string(),
        size_bytes: bytes.len() as u64,
        evidences: parsed.evidences,
        assertions: parsed.assertions,
        memories: parsed.memories,
    })
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled")
        .to_string()
}

/// Deterministic parse of one material into evidence and assertion drafts
/// with requested spaces. Shared by file and git derivation paths.
struct ParsedMaterial {
    title: String,
    evidences: Vec<EvidenceDraft>,
    assertions: Vec<AssertionDraft>,
    memories: Vec<MemorySpec>,
}

fn parse_material(stem: &str, text: &str, media_type: &str, space_ids: &[Uuid]) -> ParsedMaterial {
    let (title, fragments) = if media_type == "text/markdown" {
        let parsed = markdown::parse_markdown(text, stem);
        (parsed.title, parsed.fragments)
    } else {
        (stem.to_string(), markdown::parse_text(text, stem))
    };

    let mut evidences = Vec::with_capacity(fragments.len());
    let mut assertions = Vec::with_capacity(fragments.len());
    let mut memories = Vec::with_capacity(fragments.len());
    for (i, f) in fragments.iter().enumerate() {
        evidences.push(EvidenceDraft {
            kind: f.kind,
            heading_path: f.heading_path.clone(),
            start_line: f.start_line,
            end_line: f.end_line,
            ordinal: i as u32,
            content: f.content.clone(),
            content_hash: sha256_hex(f.content.as_bytes()),
        });
        assertions.push(AssertionDraft {
            evidence_idx: i,
            kind: crate::core::AssertionKind::Claim,
            subject: f.subject.clone(),
            subject_normalized: normalize_subject(&f.subject),
            predicate: None,
            content: f.content.clone(),
            extraction: f.extraction.to_string(),
        });
        memories.push(MemorySpec {
            assertion_idx: i,
            spaces: space_ids.to_vec(),
        });
    }
    ParsedMaterial {
        title,
        evidences,
        assertions,
        memories,
    }
}
