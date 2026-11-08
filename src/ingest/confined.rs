//! Anchored, policy-checked access for the MCP repository tools (ADR-029,
//! application layer). Linux only.
//!
//! The idea in one sentence: **check the object you hold, not the name you were
//! given.** A directory is opened once, and the *opened inode* is checked (its
//! real path as the kernel reports it through `/proc/self/fd`) against the roots
//! and their modes; from then on everything goes through `/proc/<pid>/fd/<n>`,
//! which names that inode whatever its directory entry is renamed to later.
//!
//! - Git runs with the work tree and `GIT_DIR` / `GIT_COMMON_DIR` pinned the
//!   same way, so it does not look `.git` up again (see [`open_git_reader`]).
//! - `.git` is never followed if it is a symbolic link; a `gitdir:` pointer and
//!   `commondir` must resolve into read-write roots; alternates must lie in the
//!   repository's own root, and are refused in a root that contains a read-only
//!   area (a change of that file after the check cannot be excluded).
//! - Every file of a directory ingest is opened and its real path compared with
//!   the path it is supposed to have, so a file swapped for a link is refused.
//!
//! This layer gives the precise read-only / read-write semantics (also nested)
//! that the kernel sandbox cannot express. It stays active under
//! `--insecure-no-sandbox`.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use super::deadline::Deadline;
use super::extract::{ExtractedFiles, ExtractedGit, ExtractionContext, StoreContext};
use super::git::{GitPin, GitReader};
use super::outline::{self, Outline, PathSelection};
use super::repo_info::{self, RepoInfo};
use super::{GitScope, IngestOutcome};
use crate::core::{Error, IngestScope, Result, Uuid};
use crate::storage::Store;

/// What a root permits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    ReadOnly,
    ReadWrite,
}

/// The allowlist as seen from here; implemented by the MCP configuration.
pub trait PathPolicy {
    /// The most specific root containing `real` and its mode; `None` outside every root.
    fn governing(&self, real: &Path) -> Option<(PathBuf, Mode)>;
    /// True when a read-only root lies strictly inside `dir`.
    fn contains_read_only(&self, dir: &Path) -> bool;
}

const OUTSIDE: &str = "path is not accessible or outside the allowed roots";
const SMALL: usize = 64 * 1024;

fn refuse(msg: impl Into<String>) -> Error {
    Error::Storage(msg.into())
}

fn read_only_message(root: &Path) -> String {
    format!(
        "path is inside the read-only root {root}; ingesting needs a read-write root \
         (start the server with --allow-root {root}:rw)",
        root = root.display()
    )
}

/// The path of an open descriptor as the kernel reports it right now.
fn real_path_of(file: &File) -> Result<PathBuf> {
    let link = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .map_err(|_| refuse(OUTSIDE))?;
    if link.to_string_lossy().ends_with(" (deleted)") {
        return Err(refuse(
            "the directory was removed while it was being opened",
        ));
    }
    Ok(link)
}

/// An open directory whose inode was checked against the allowlist.
#[derive(Debug)]
pub struct Anchor {
    file: File,
    real: PathBuf,
}

impl Anchor {
    /// Opens `path` as a directory and checks what was actually opened. With
    /// `expected` (the canonical path from the authorization step) the real path
    /// of the opened inode must be exactly that: if a path component was swapped
    /// in between, the open follows the swap and the comparison fails.
    pub fn open(
        path: &Path,
        expected: Option<&Path>,
        policy: &dyn PathPolicy,
        need_rw: bool,
    ) -> Result<Anchor> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY)
            .open(path)
            .map_err(|_| refuse(OUTSIDE))?;
        let real = real_path_of(&file)?;
        if let Some(e) = expected {
            if real != e {
                return Err(refuse(
                    "the directory changed while it was being opened (a path component was replaced)",
                ));
            }
        }
        match policy.governing(&real) {
            None => Err(refuse(OUTSIDE)),
            Some((root, Mode::ReadOnly)) if need_rw => Err(refuse(read_only_message(&root))),
            Some(_) => Ok(Anchor { file, real }),
        }
    }

    pub fn real(&self) -> &Path {
        &self.real
    }

    /// A path that names this inode for as long as the descriptor is open.
    pub fn proc_path(&self) -> PathBuf {
        PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            self.file.as_raw_fd()
        ))
    }

    pub fn try_clone(&self) -> Result<Anchor> {
        let file = self.file.try_clone().map_err(|e| Error::Io {
            path: self.real.display().to_string(),
            source: e,
        })?;
        Ok(Anchor {
            file,
            real: self.real.clone(),
        })
    }

    fn into_file(self) -> File {
        self.file
    }

    /// A subdirectory, which must be exactly `self.real/rel` (no link on the way).
    pub fn child_dir(&self, rel: &str, policy: &dyn PathPolicy, need_rw: bool) -> Result<Anchor> {
        let expected = self.real.join(rel);
        Anchor::open(
            &self.proc_path().join(rel),
            Some(&expected),
            policy,
            need_rw,
        )
    }

    /// Reads a regular file below this directory, or `None` if it does not exist.
    /// The opened file's real path must be exactly `self.real/rel`, so a file that
    /// was replaced by a link (or sits behind one) is refused, and it must be
    /// governed by a mode that allows the use (`need_rw`).
    pub fn read_beneath(
        &self,
        rel: &Path,
        policy: &dyn PathPolicy,
        need_rw: bool,
        max: Option<usize>,
    ) -> Result<Option<Vec<u8>>> {
        use std::io::Read;
        let opened = OpenOptions::new()
            .read(true)
            // Never block on a FIFO or a device that replaced the file.
            .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
            .open(self.proc_path().join(rel));
        let mut file = match opened {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(Error::Io {
                    path: rel.display().to_string(),
                    source: e,
                })
            }
        };
        let meta = file.metadata().map_err(|e| Error::Io {
            path: rel.display().to_string(),
            source: e,
        })?;
        if !meta.is_file() {
            return Err(refuse(format!("{} is not a regular file", rel.display())));
        }
        let real = real_path_of(&file)?;
        if real != self.real.join(rel) {
            return Err(refuse(format!(
                "{} was replaced by a link while it was being read",
                rel.display()
            )));
        }
        match policy.governing(&real) {
            Some((_, Mode::ReadWrite)) => {}
            Some((_, Mode::ReadOnly)) if !need_rw => {}
            Some((root, Mode::ReadOnly)) => return Err(refuse(read_only_message(&root))),
            None => return Err(refuse(OUTSIDE)),
        }
        let mut buf = Vec::new();
        let read = match max {
            Some(m) => (&mut file).take(m as u64 + 1).read_to_end(&mut buf),
            None => file.read_to_end(&mut buf),
        };
        read.map_err(|e| Error::Io {
            path: rel.display().to_string(),
            source: e,
        })?;
        if max.is_some_and(|m| buf.len() > m) {
            return Err(refuse(format!("{} is unexpectedly large", rel.display())));
        }
        Ok(Some(buf))
    }
}

fn has_git(anchor: &Anchor) -> bool {
    let base = anchor.proc_path();
    std::fs::symlink_metadata(base.join(".git")).is_ok()
        || (base.join("HEAD").is_file()
            && base.join("objects").is_dir()
            && base.join("refs").is_dir())
}

fn parse_gitdir_pointer(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let line = text.lines().next()?.trim();
    line.strip_prefix("gitdir:")
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
}

/// Binds a [`GitReader`] to checked locations: the git dir, the common dir and
/// the work tree are opened and checked once, and git is pointed at exactly
/// those. With `need_rw` all of them must be governed by a read-write root (the
/// identity marker is written into the common dir); without it any allowed root
/// will do (outline).
pub fn open_git_reader(
    anchor: &Anchor,
    policy: &dyn PathPolicy,
    need_rw: bool,
    deadline: &Deadline,
) -> Result<GitReader> {
    let base = anchor.proc_path();
    let gitdir = match std::fs::symlink_metadata(base.join(".git")) {
        Ok(m) if m.file_type().is_symlink() => {
            return Err(refuse(
                "`.git` is a symbolic link; a repository that points elsewhere through a link is not supported",
            ))
        }
        Ok(m) if m.is_dir() => anchor.child_dir(".git", policy, need_rw)?,
        Ok(m) if m.is_file() => {
            // A linked worktree: `.git` is a file `gitdir: <path>`.
            let bytes = anchor
                .read_beneath(Path::new(".git"), policy, need_rw, Some(SMALL))?
                .ok_or_else(|| refuse("`.git` disappeared"))?;
            let target = parse_gitdir_pointer(&bytes).ok_or_else(|| refuse("`.git` is not a valid gitdir pointer"))?;
            let p = Path::new(&target);
            let abs = if p.is_absolute() { p.to_path_buf() } else { anchor.real().join(p) };
            let canonical = std::fs::canonicalize(&abs).map_err(|_| refuse(OUTSIDE))?;
            Anchor::open(&canonical, Some(&canonical), policy, need_rw)?
        }
        Ok(_) => return Err(refuse("`.git` is neither a directory nor a file")),
        Err(_) => anchor.try_clone()?, // a bare repository: the directory itself
    };
    let common = match gitdir.read_beneath(Path::new("commondir"), policy, need_rw, Some(SMALL))? {
        Some(bytes) => {
            let rel = String::from_utf8_lossy(&bytes).trim().to_string();
            let p = Path::new(&rel);
            let abs = if p.is_absolute() {
                p.to_path_buf()
            } else {
                gitdir.real().join(p)
            };
            let canonical = std::fs::canonicalize(&abs).map_err(|_| refuse(OUTSIDE))?;
            Anchor::open(&canonical, Some(&canonical), policy, need_rw)?
        }
        None => gitdir.try_clone()?,
    };
    // Git follows symbolic links inside its own directory (a link in `objects/`
    // or `refs/`, or in `config`, whose error messages quote the file), so a
    // link anywhere in it can redirect reads. Refused, not followed.
    reject_links_in_git_dir(&gitdir)?;
    if common.real() != gitdir.real() {
        reject_links_in_git_dir(&common)?;
    }
    check_alternates(&common, policy, need_rw)?;

    let pin = GitPin {
        git_dir: gitdir.proc_path(),
        common_dir: common.proc_path(),
        keep: vec![gitdir.into_file(), common.into_file()],
    };
    Ok(GitReader::pinned(anchor.proc_path(), *deadline, pin))
}

/// Fails if any entry of the git directory is a symbolic link. Descends into the
/// directories git reads from; `hooks` (never run here) and `logs` (never read)
/// are skipped. The check cannot exclude a link that appears after it; under the
/// kernel sandbox that case is stopped by the kernel, without it the residual
/// risk is documented (ADR-029).
fn reject_links_in_git_dir(dir: &Anchor) -> Result<()> {
    fn walk(base: &Path, rel: &Path, top: bool) -> Result<()> {
        let entries = std::fs::read_dir(base.join(rel)).map_err(|e| Error::Io {
            path: rel.display().to_string(),
            source: e,
        })?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let child = rel.join(&name);
            let kind = entry.file_type().map_err(|e| Error::Io {
                path: child.display().to_string(),
                source: e,
            })?;
            if kind.is_symlink() {
                return Err(refuse(format!(
                    "the git directory contains a symbolic link ({}); a repository that links out of its \
                     own directory is not supported",
                    child.display()
                )));
            }
            if kind.is_dir() {
                let skip = top && (name == "hooks" || name == "logs");
                if !skip {
                    walk(base, &child, false)?;
                }
            }
        }
        Ok(())
    }
    walk(&dir.proc_path(), Path::new(""), true)
}

/// Alternate object stores let a repository read objects from elsewhere. They
/// must lie in the repository's own governing root; in a root that contains a
/// read-only area they are refused, because the file can be changed after this
/// check and the kernel cannot tell the read-only area from the rest.
fn check_alternates(common: &Anchor, policy: &dyn PathPolicy, need_rw: bool) -> Result<()> {
    let Some(bytes) = common.read_beneath(
        Path::new("objects/info/alternates"),
        policy,
        need_rw,
        Some(SMALL),
    )?
    else {
        return Ok(());
    };
    let text = String::from_utf8_lossy(&bytes);
    let entries: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    if entries.is_empty() {
        return Ok(());
    }
    let (repo_root, _) = policy
        .governing(common.real())
        .ok_or_else(|| refuse(OUTSIDE))?;
    if policy.contains_read_only(&repo_root) {
        return Err(refuse(
            "alternate object stores are not supported in a root that contains a read-only area \
             (the file could be changed after the check, and that cannot be excluded)",
        ));
    }
    for entry in entries {
        if entry.starts_with('"') {
            return Err(refuse("quoted alternates entries are not supported"));
        }
        let p = Path::new(entry);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            common.real().join("objects").join(p)
        };
        let canonical = std::fs::canonicalize(&abs)
            .map_err(|_| refuse("an alternate object store cannot be resolved"))?;
        match policy.governing(&canonical) {
            Some((root, mode)) if root == repo_root && (mode == Mode::ReadWrite || !need_rw) => {}
            _ => {
                return Err(refuse(
                    "the repository lists an alternate object store outside its own allowed root",
                ))
            }
        }
    }
    Ok(())
}

/// Ingest of an anchored directory: a git repository, or a plain directory.
pub fn ingest_confined<S: AsRef<str>>(
    store: &dyn Store,
    anchor: &Anchor,
    policy: &dyn PathPolicy,
    selection: &PathSelection,
    scope: GitScope,
    space_names: &[S],
    deadline: &Deadline,
) -> Result<Vec<IngestOutcome>> {
    deadline.check("starting")?;
    if has_git(anchor) {
        let extracted = extract_confined_git(
            &StoreContext(store),
            anchor,
            policy,
            selection,
            scope,
            space_names,
            deadline,
        )?;
        let report = super::extract::persist_git(store, extracted, deadline)?;
        return Ok(vec![IngestOutcome::Git(report)]);
    }
    let space_ids = super::resolve_spaces(store, space_names)?;
    if selection.is_all() {
        return Ok(vec![ingest_directory(
            store, anchor, policy, &space_ids, deadline,
        )?]);
    }
    let total = selection.prefixes().len();
    let mut outcomes = Vec::new();
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
        let child = anchor.child_dir(prefix, policy, true)?;
        outcomes.push(ingest_directory(
            store, &child, policy, &space_ids, deadline,
        )?);
    }
    Ok(outcomes)
}

/// Extraction of an anchored Git repository: everything up to, and not
/// including, the database write (see [`super::extract`]).
pub fn extract_confined_git<S: AsRef<str>>(
    ctx: &dyn ExtractionContext,
    anchor: &Anchor,
    policy: &dyn PathPolicy,
    selection: &PathSelection,
    scope: GitScope,
    space_names: &[S],
    deadline: &Deadline,
) -> Result<ExtractedGit> {
    let reader = open_git_reader(anchor, policy, true, deadline)?;
    super::extract_git_with_reader(
        ctx,
        reader,
        anchor.real(),
        space_names,
        selection,
        scope,
        deadline,
    )
}

fn ingest_directory(
    store: &dyn Store,
    anchor: &Anchor,
    policy: &dyn PathPolicy,
    space_ids: &[Uuid],
    deadline: &Deadline,
) -> Result<IngestOutcome> {
    let extracted = extract_directory(&StoreContext(store), anchor, policy, space_ids, deadline)?;
    super::extract::persist_files(store, extracted, deadline).map(IngestOutcome::Files)
}

/// Extraction of one anchored directory: reads every ingestable file beneath
/// the anchor and parses it; writes nothing.
pub fn extract_directory(
    ctx: &dyn ExtractionContext,
    anchor: &Anchor,
    policy: &dyn PathPolicy,
    space_ids: &[Uuid],
    deadline: &Deadline,
) -> Result<ExtractedFiles> {
    let base = anchor.proc_path();
    let mut paths: Vec<PathBuf> = outline::walk_files(&base)?
        .into_iter()
        .filter(|p| super::is_ingestable(p))
        .collect();
    paths.sort();
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        deadline.check("reading files")?;
        let rel = path
            .strip_prefix(&base)
            .map_err(|_| refuse("walk escaped the anchored directory"))?
            .to_path_buf();
        let bytes = anchor
            .read_beneath(&rel, policy, true, None)?
            .ok_or_else(|| {
                refuse(format!(
                    "{} disappeared while it was being read",
                    rel.display()
                ))
            })?;
        let location = anchor.real().join(&rel).display().to_string();
        files.push(super::observe_bytes(&rel, bytes, location, space_ids)?);
    }
    super::prepare_observed_files(
        ctx,
        anchor.real().display().to_string(),
        IngestScope::Directory,
        files,
        space_ids.to_vec(),
        deadline,
    )
}

/// Outline of an anchored directory, with the repository facts for git.
pub fn scan_confined(
    anchor: &Anchor,
    policy: &dyn PathPolicy,
    depth: u32,
    deadline: &Deadline,
) -> Result<(Outline, Option<RepoInfo>)> {
    let root = anchor.real().display().to_string();
    if has_git(anchor) {
        let reader = open_git_reader(anchor, policy, false, deadline)?;
        let outline = super::scan_git(&reader, root.clone(), depth)?;
        let info = repo_info::repo_info_with_reader(&reader, root).ok();
        return Ok((outline, info));
    }
    Ok((
        super::scan_dir(&anchor.proc_path(), root, depth, deadline)?,
        None,
    ))
}
