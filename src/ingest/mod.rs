pub mod code;
#[cfg(target_os = "linux")]
pub mod confined;
pub mod confined_git;
pub mod deadline;
pub mod extract;
pub mod git;
pub mod gitverify;
pub mod markdown;
pub mod outline;
pub mod release;
pub mod remote;
pub mod repo_info;
pub mod stream;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::core::{
    normalize_subject, sha256_hex, AssertionDraft, Error, EvidenceDraft, GitDerivedFile,
    GitObjectKind, GitObservation, GitObservationReport, GitRefEventDraft, GitRefEventKind,
    IngestObservation, IngestOutcome, IngestScope, MemorySpec, ObservationReport, ObservedFile,
    Result, SessionDraft, Ts, Uuid,
};
use crate::storage::Store;
use deadline::Deadline;
use extract::{Extracted, ExtractedFiles, ExtractedGit, ExtractionContext, StoreContext};

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
    ingest_path_with_deadline(store, path, space_names, &Deadline::none())
}

/// Like [`ingest_path`], but stops cleanly with [`Error::IngestTimeout`] when
/// `deadline` expires before the first database write. Once writing has
/// started it always runs to its end (see [`deadline`]).
pub fn ingest_path_with_deadline<S: AsRef<str>>(
    store: &dyn Store,
    path: &Path,
    space_names: &[S],
    deadline: &Deadline,
) -> Result<IngestOutcome> {
    let extracted = extract_path_with_deadline(&StoreContext(store), path, space_names, deadline)?;
    extract::persist(store, extracted, deadline)
}

/// The extraction half of [`ingest_path_with_deadline`]: reads and parses, never
/// writes (the context is read-only). See [`extract`].
pub fn extract_path_with_deadline<S: AsRef<str>>(
    ctx: &dyn ExtractionContext,
    path: &Path,
    space_names: &[S],
    deadline: &Deadline,
) -> Result<Extracted> {
    deadline.check("starting")?;
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
        return Ok(Extracted::Git(
            extract_git_repository_selected_with_deadline(
                ctx,
                &canonical,
                space_names,
                &outline::PathSelection::all(),
                GitScope::Head,
                deadline,
            )?,
        ));
    }

    // A single file must fail on its format before spaces are resolved.
    if metadata.is_file() {
        media_type_of(&canonical)?;
    }

    // Spaces must pre-exist; the first missing one fails the ingest.
    let space_ids = resolve_spaces_with(ctx, space_names)?;

    let (root, scope, files) = if metadata.is_dir() {
        (
            canonical.display().to_string(),
            IngestScope::Directory,
            observe_directory(&canonical, &space_ids, deadline)?,
        )
    } else {
        let file = observe_file(&canonical, &space_ids)?;
        (
            canonical.display().to_string(),
            IngestScope::File,
            vec![file],
        )
    };

    prepare_observed_files(ctx, root, scope, files, space_ids, deadline).map(Extracted::Files)
}

/// The code input of one observed file: `None` for files that are not source code.
/// A blob already parsed with this parser identity gets an empty, complete
/// derivation instead of a new parse.
pub(crate) fn code_input_for_file(
    ctx: &dyn ExtractionContext,
    f: &ObservedFile,
    deadline: &Deadline,
    policy: &code::ParsePolicy,
) -> Result<Option<crate::core::CodeInputDraft>> {
    Ok(code_input_for_file_marked(ctx, f, deadline, policy)?.map(|(input, _)| input))
}

/// Like [`code_input_for_file`], and says whether the derivation was reused from the context
/// (a placeholder) rather than produced by this call.
pub(crate) fn code_input_for_file_marked(
    ctx: &dyn ExtractionContext,
    f: &ObservedFile,
    deadline: &Deadline,
    policy: &code::ParsePolicy,
) -> Result<Option<(crate::core::CodeInputDraft, bool)>> {
    let Some(language) = code::language_for(Path::new(&f.location)) else {
        return Ok(None);
    };
    deadline.check("parsing source files")?;
    let (derivation, reused) = structural_decision(
        ctx,
        language,
        &f.content,
        &f.content_hash,
        None,
        None,
        policy,
        true,
    )?;
    Ok(Some((
        crate::core::CodeInputDraft {
            language,
            content_hash: f.content_hash.clone(),
            content: f.content.clone(),
            repository_id: None,
            blob_oid: None,
            contexts: vec![crate::core::CodeContextDraft {
                path: f.location.clone(),
                commit_oid: None,
            }],
            derivation,
        },
        reused,
    )))
}

/// The structural derivation of one source, under the resource policy (design:
/// `docs/design-m7.3b-structural-parse-policy.md`). The order is fixed:
///  1. a derivation with the current parser identity already exists: an empty placeholder with
///     that identity (persistence finds the row and reuses it); the policy is not consulted, so
///     lowering a budget never removes or re-derives existing structure;
///  2. a skip row for a reason that still applies under the current limits exists: the same, with
///     the skip identity;
///  3. the source is over the size budget: a skip derivation (nothing is parsed);
///  4. the source is parsed under the policy: its derivation, or a skip derivation if the depth
///     or output budget was exceeded (the whole structural result is discarded).
///
/// `strict` makes a failing existence probe an error (directory ingest); otherwise it counts as
/// "not known" (Git ingest). A parser that cannot run is an error, never a stored derivation.
#[allow(clippy::too_many_arguments)]
pub fn structural_derivation(
    ctx: &dyn ExtractionContext,
    language: crate::core::CodeLanguage,
    content: &str,
    content_hash: &str,
    repository_id: Option<Uuid>,
    blob_oid: Option<&str>,
    policy: &code::ParsePolicy,
    strict: bool,
) -> Result<crate::core::CodeDerivationDraft> {
    structural_decision(
        ctx,
        language,
        content,
        content_hash,
        repository_id,
        blob_oid,
        policy,
        strict,
    )
    .map(|(d, _)| d)
}

/// [`structural_derivation`] plus its provenance: `true` when the derivation is a placeholder
/// for one the context announced (steps 1 and 2), `false` when it was produced here (3 and 4).
#[allow(clippy::too_many_arguments)]
pub fn structural_decision(
    ctx: &dyn ExtractionContext,
    language: crate::core::CodeLanguage,
    content: &str,
    content_hash: &str,
    repository_id: Option<Uuid>,
    blob_oid: Option<&str>,
    policy: &code::ParsePolicy,
    strict: bool,
) -> Result<(crate::core::CodeDerivationDraft, bool)> {
    let known = |identity: &crate::core::CodeParserIdentity| -> Result<bool> {
        let r = ctx.has_code_derivation(language, content_hash, repository_id, blob_oid, identity);
        if strict {
            r
        } else {
            Ok(r.unwrap_or(false))
        }
    };
    let identity = code::identity();
    if known(&identity)? {
        return Ok((
            crate::core::CodeDerivationDraft {
                identity,
                status: crate::core::CodeParseStatus::Complete,
                error_count: 0,
                entities: vec![],
                imports: vec![],
                relationships: vec![],
            },
            true,
        ));
    }
    let oversized = content.len() as u64 > policy.max_source_bytes;
    for reason in policy.reasons() {
        // a source within the size budget cannot have been skipped for its size
        if matches!(reason, code::SkipReason::SourceBytes { .. }) && !oversized {
            continue;
        }
        if oversized && !matches!(reason, code::SkipReason::SourceBytes { .. }) {
            continue;
        }
        if known(&code::skip_identity(reason))? {
            return Ok((code::skip_derivation(reason), true));
        }
    }
    let outcome = code::parse_with_policy(language, content, policy);
    if let Some(why) = outcome.transient {
        return Err(Error::InvalidExtraction(format!(
            "structural parsing could not run: {why}"
        )));
    }
    Ok((outcome.derivation, false))
}

/// Parses the source files among `files` and assembles the observation (shared by
/// the path-based and the anchored directory ingest). Writes nothing.
pub(crate) fn prepare_observed_files(
    ctx: &dyn ExtractionContext,
    root: String,
    scope: IngestScope,
    files: Vec<ObservedFile>,
    space_ids: Vec<Uuid>,
    deadline: &Deadline,
) -> Result<ExtractedFiles> {
    let mut code_inputs = Vec::new();
    for f in &files {
        if let Some(input) = code_input_for_file(ctx, f, deadline, &code::ParsePolicy::default())? {
            code_inputs.push(input);
        }
    }
    let observation = IngestObservation {
        session: SessionDraft {
            root,
            scope,
            observed_at: Ts::now(),
        },
        files,
        spaces: space_ids.clone(),
    };
    Ok(ExtractedFiles {
        observation,
        code_inputs,
        reused: Default::default(),
    })
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
    ingest_paths_with_deadline(
        store,
        path,
        selection,
        scope,
        space_names,
        &Deadline::none(),
    )
}

/// Like [`ingest_paths`], with a time limit (see [`ingest_path_with_deadline`]).
/// For a plain directory every selected sub-path is its own ingest root; when
/// the limit expires between roots, the finished ones are kept and the error
/// says how many.
pub fn ingest_paths_with_deadline<S: AsRef<str>>(
    store: &dyn Store,
    path: &Path,
    selection: &outline::PathSelection,
    scope: GitScope,
    space_names: &[S],
    deadline: &Deadline,
) -> Result<Vec<IngestOutcome>> {
    deadline.check("starting")?;
    let canonical_is_git = std::fs::canonicalize(path)
        .map(|c| c.is_dir() && git::looks_like_git_repository(&c))
        .unwrap_or(false);
    if selection.is_all() && !(canonical_is_git && scope != GitScope::Head) {
        return Ok(vec![ingest_path_with_deadline(
            store,
            path,
            space_names,
            deadline,
        )?]);
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
        return Ok(vec![IngestOutcome::Git(
            ingest_git_repository_selected_with_deadline(
                store,
                &canonical,
                space_names,
                selection,
                scope,
                deadline,
            )?,
        )]);
    }
    let mut outcomes = Vec::new();
    let total = selection.prefixes().len();
    for prefix in selection.prefixes() {
        let kept = outcomes.len();
        if kept == 0 {
            deadline.check("starting")?;
        } else {
            deadline.check_with(
                "ingesting selected paths",
                &format!(
                    "{kept} of {total} selected paths were ingested and kept; repeating the call \
                     continues with the rest and is idempotent"
                ),
            )?;
        }
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
        outcomes.push(ingest_path_with_deadline(
            store,
            &target,
            space_names,
            deadline,
        )?);
    }
    Ok(outcomes)
}

/// Read-only outline of a git repository (at HEAD) or directory, down to
/// `depth` directory levels. Opens no database and writes nothing.
pub fn scan_path(path: &Path, depth: u32) -> Result<outline::Outline> {
    scan_path_with_deadline(path, depth, &Deadline::none())
}

/// Like [`scan_path`]; git is killed when `deadline` expires.
pub fn scan_path_with_deadline(
    path: &Path,
    depth: u32,
    deadline: &Deadline,
) -> Result<outline::Outline> {
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
        let reader = git::GitReader::open_with_deadline(&canonical, *deadline)?;
        return scan_git(&reader, root, depth);
    }
    scan_dir(&canonical, root, depth, deadline)
}

/// Outline of a git repository at HEAD, through an already opened reader.
pub(crate) fn scan_git(
    reader: &git::GitReader,
    root: String,
    depth: u32,
) -> Result<outline::Outline> {
    let commit = reader.head_commit()?;
    let files = reader.commit_blob_sizes(&commit)?;
    Ok(outline::Outline {
        root,
        source: "git".into(),
        commit: Some(commit),
        depth,
        nodes: outline::build_nodes(files, depth),
    })
}

/// Outline of a plain directory. `base` is where the walk starts; `root` is the
/// label recorded.
pub(crate) fn scan_dir(
    base: &Path,
    root: String,
    depth: u32,
    deadline: &Deadline,
) -> Result<outline::Outline> {
    let mut files = Vec::new();
    for file in outline::walk_files(base)? {
        deadline.check("measuring the directory")?;
        let rel = file
            .strip_prefix(base)
            .unwrap_or(&file)
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let size = std::fs::symlink_metadata(&file)
            .map(|m| m.len())
            .unwrap_or(0);
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
    resolve_spaces_with(&StoreContext(store), space_names)
}

pub(crate) fn resolve_spaces_with<S: AsRef<str>>(
    ctx: &dyn ExtractionContext,
    space_names: &[S],
) -> Result<Vec<Uuid>> {
    let mut space_ids = Vec::with_capacity(space_names.len());
    for name in space_names {
        space_ids.push(ctx.space_id(name.as_ref())?);
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
    ingest_git_repository_selected_with_deadline(
        store,
        repo_path,
        space_names,
        selection,
        scope,
        &Deadline::none(),
    )
}

/// Like [`ingest_git_repository_selected`], with a time limit. The limit is
/// checked through the whole preparation (git processes, parsing, deriving)
/// and once more right before the database write. The write itself, the code
/// structure and the relationship resolution that follow it are not
/// interrupted: they complete as one unit, or the ingest stopped before them.
pub fn ingest_git_repository_selected_with_deadline<S: AsRef<str>>(
    store: &dyn Store,
    repo_path: &Path,
    space_names: &[S],
    selection: &outline::PathSelection,
    scope: GitScope,
    deadline: &Deadline,
) -> Result<GitObservationReport> {
    let extracted = extract_git_repository_selected_with_deadline(
        &StoreContext(store),
        repo_path,
        space_names,
        selection,
        scope,
        deadline,
    )?;
    extract::persist_git(store, extracted, deadline)
}

/// The extraction half of [`ingest_git_repository_selected_with_deadline`].
pub fn extract_git_repository_selected_with_deadline<S: AsRef<str>>(
    ctx: &dyn ExtractionContext,
    repo_path: &Path,
    space_names: &[S],
    selection: &outline::PathSelection,
    scope: GitScope,
    deadline: &Deadline,
) -> Result<ExtractedGit> {
    let reader = git::GitReader::open_with_deadline(repo_path, *deadline)?;
    extract_git_with_reader(
        ctx,
        reader,
        repo_path,
        space_names,
        selection,
        scope,
        deadline,
    )
}

/// The body of a git ingest for a reader that is already bound: everything up
/// to, and not including, the database write. The context is read-only.
/// `repo_path` is only the label recorded for the repository and session.
pub(crate) fn extract_git_with_reader<S: AsRef<str>>(
    ctx: &dyn ExtractionContext,
    reader: git::GitReader,
    repo_path: &Path,
    space_names: &[S],
    selection: &outline::PathSelection,
    scope: GitScope,
    deadline: &Deadline,
) -> Result<ExtractedGit> {
    // before the identity marker is created or anything is read (see `require_sha1`)
    reader.require_sha1()?;
    let repository_id = reader.repository_id()?;
    let space_ids = resolve_spaces_with(ctx, space_names)?;

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
    deadline.check("discovering git objects")?;
    let reachable = reader.reachable_oids(&tips)?;
    deadline.check("checking git objects")?;
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
    deadline.check("reading git objects")?;
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
        deadline.check("listing files")?;
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
    deadline.check("reading file contents")?;
    let contents = reader.blob_contents(&text_oids)?;
    let content_by_oid: std::collections::HashMap<String, String> = contents
        .into_iter()
        .map(|b| (b.oid, b.content.unwrap_or_default()))
        .collect();

    let mut derivations: Vec<GitDerivedFile> = Vec::new();
    for (commit, files) in tip_files {
        for (blob_oid, path) in files {
            deadline.check("deriving memories")?;
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
    let current = ctx.git_refs_current(repository_id)?;
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

    let code_inputs = tip_files_to_code_inputs(
        ctx,
        repository_id,
        &derivations,
        &content_by_oid,
        deadline,
        &code::ParsePolicy::default(),
    )?;

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
        spaces: space_ids.clone(),
        remotes: reader
            .remotes()?
            .into_iter()
            .map(|(name, raw)| {
                let parsed = remote::parse(&raw);
                crate::core::GitRemoteDraft {
                    name,
                    url: parsed.url,
                    slug_key: parsed.slug.as_ref().map(|s| s.key()),
                    slug: parsed.slug.as_ref().map(|s| s.short()),
                }
            })
            .collect(),
        current_commits: (scope != GitScope::History)
            .then(|| tip_commits.iter().cloned().collect()),
    };
    Ok(ExtractedGit {
        observation,
        code_inputs,
        reused: Default::default(),
    })
}

fn tip_files_to_code_inputs(
    ctx: &dyn ExtractionContext,
    repository_id: Uuid,
    derivations: &[GitDerivedFile],
    contents: &std::collections::HashMap<String, String>,
    deadline: &Deadline,
    policy: &code::ParsePolicy,
) -> Result<Vec<crate::core::CodeInputDraft>> {
    let mut grouped: std::collections::BTreeMap<String, crate::core::CodeInputDraft> =
        std::collections::BTreeMap::new();
    for d in derivations {
        let p = Path::new(&d.path);
        let Some(language) = code::language_for(p) else {
            continue;
        };
        deadline.check("parsing source files")?;
        if !grouped.contains_key(&d.blob_oid) {
            let content = contents.get(&d.blob_oid).cloned().unwrap_or_default();
            let content_hash = sha256_hex(content.as_bytes());
            let derivation = structural_derivation(
                ctx,
                language,
                &content,
                &content_hash,
                Some(repository_id),
                Some(&d.blob_oid),
                policy,
                false,
            )?;
            grouped.insert(
                d.blob_oid.clone(),
                crate::core::CodeInputDraft {
                    language,
                    content_hash,
                    content,
                    repository_id: Some(repository_id),
                    blob_oid: Some(d.blob_oid.clone()),
                    contexts: vec![],
                    derivation,
                },
            );
        }
        if let Some(entry) = grouped.get_mut(&d.blob_oid) {
            entry.contexts.push(crate::core::CodeContextDraft {
                path: d.path.clone(),
                commit_oid: Some(d.commit_oid.clone()),
            });
        }
    }
    Ok(grouped.into_values().collect())
}

// ---------------------------------------------------------------------------
// Directory and file observation
// ---------------------------------------------------------------------------

/// Walks one directory deterministically (see [`outline::walk_files`]):
/// project-local ignore files apply, symlinks are skipped, and only
/// `.md`, `.markdown`, `.txt`, and `.rs` files take part in the observed set.
fn observe_directory(
    root: &Path,
    space_ids: &[Uuid],
    deadline: &Deadline,
) -> Result<Vec<ObservedFile>> {
    let mut paths: Vec<PathBuf> = outline::walk_files(root)?
        .into_iter()
        .filter(|p| is_ingestable(p))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|p| {
            deadline.check("reading files")?;
            observe_file(p, space_ids)
        })
        .collect()
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
    media_type_of(path)?;
    let bytes = std::fs::read(path).map_err(|e| Error::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    let location = std::fs::canonicalize(path)
        .map_err(|e| Error::Io {
            path: path.display().to_string(),
            source: e,
        })?
        .display()
        .to_string();
    observe_bytes(path, bytes, location, space_ids)
}

/// Builds the observation of one file from bytes that were already read.
/// `name` decides the media type and title; `location` is what is recorded.
pub(crate) fn observe_bytes(
    name: &Path,
    bytes: Vec<u8>,
    location: String,
    space_ids: &[Uuid],
) -> Result<ObservedFile> {
    let path = name;
    let media_type = media_type_of(path)?;
    let text = String::from_utf8(bytes.clone()).map_err(|_| Error::InvalidEncoding {
        path: path.display().to_string(),
    })?;

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

#[cfg(test)]
mod deadline_checkpoints {
    //! Every checkpoint of the ingest pipeline, exercised one by one: the limit
    //! expires exactly at that phase, and the ingest must fail with a timeout
    //! naming the phase and leave every table of the database untouched. A
    //! checkpoint that is removed (or never reached by the fixture) makes the
    //! ingest succeed and fails the test.

    use super::*;
    use crate::SqliteStore;
    use std::collections::BTreeMap;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let o = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }

    fn fixture(dir: &Path, with_git: bool) {
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("docs/a.md"), "# A\n\nalpha\n").unwrap();
        std::fs::write(dir.join("lib.rs"), "pub fn f() {}\n").unwrap();
        if with_git {
            git(dir, &["init", "-q", "-b", "main"]);
            git(dir, &["add", "-A"]);
            git(dir, &["commit", "-q", "-m", "i"]);
        }
    }

    fn snapshot(db: &Path) -> BTreeMap<String, i64> {
        let c = rusqlite::Connection::open(db).unwrap();
        let tables: Vec<String> = c
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        tables
            .into_iter()
            .map(|t| {
                let n = c
                    .query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0))
                    .unwrap();
                (t, n)
            })
            .collect()
    }

    fn run(phases: &[&'static str], with_git: bool) {
        for phase in phases {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("root");
            fixture(&root, with_git);
            let db = tmp.path().join("m.db");
            let store = SqliteStore::open(&db).unwrap();
            store.create_space("s", "").unwrap();
            let before = snapshot(&db);

            let result = ingest_paths_with_deadline(
                &store,
                &root,
                &outline::PathSelection::all(),
                GitScope::Head,
                &["s"],
                &Deadline::expiring_at(phase),
            );
            match result {
                Err(Error::IngestTimeout { message }) => {
                    assert!(
                        message.contains(phase),
                        "phase `{phase}` not named: {message}"
                    );
                    assert!(
                        message.contains("database was not changed"),
                        "`{phase}`: {message}"
                    );
                }
                Err(e) => panic!("phase `{phase}`: wrong error: {e}"),
                Ok(_) => panic!("phase `{phase}` was never checked: the ingest succeeded"),
            }
            assert_eq!(
                snapshot(&db),
                before,
                "database changed when aborting at `{phase}`"
            );
        }
    }

    #[test]
    fn git_pipeline_aborts_cleanly_at_every_checkpoint() {
        run(
            &[
                "starting",
                "discovering git objects",
                "checking git objects",
                "reading git objects",
                "listing files",
                "reading file contents",
                "deriving memories",
                "parsing source files",
                "preparing to write",
            ],
            true,
        );
    }

    #[test]
    fn directory_pipeline_aborts_cleanly_at_every_checkpoint() {
        run(
            &[
                "starting",
                "reading files",
                "parsing source files",
                "preparing to write",
            ],
            false,
        );
    }
}
