//! Reading git repositories through the system git binary, plumbing only.
//!
//! Deliberate choice (ADR-014): the system `git` is used with stable,
//! machine-readable plumbing formats (`for-each-ref --format`, `cat-file
//! --batch`, `rev-list --objects --no-object-names`, `ls-tree -z`,
//! `merge-base --is-ancestor`). Localized human output is never parsed.
//! Repository contents are untrusted input: only the object database is read,
//! never the working tree.
//!
//! Hardening (MIND-2, ADR-014 addendum): every invocation goes through
//! [`git_command`], which pins the execution context instead of trusting the
//! caller's environment or the repository's configuration:
//!
//! - All inherited `GIT_*` variables are removed. `GIT_DIR` would redirect
//!   reads to another repository; `GIT_CONFIG_*`, `GIT_EXTERNAL_DIFF`,
//!   `GIT_SSH_COMMAND` and friends inject behavior.
//! - User and system config are not read, so a user's `safe.directory = *`
//!   cannot lift Git's ownership check for an untrusted directory.
//! - `GIT_NO_REPLACE_OBJECTS=1`: `refs/replace/*` would make Git return bytes
//!   that do not hash to the recorded object id.
//! - Hooks, fsmonitor, alternate-refs commands, pagers and all network
//!   protocols are switched off, and optional locks are not taken.
//!
//! Residual risk: the repository's own `.git/config` is still read (Git has no
//! way around that), and commit-graph, alternates and shallow files are
//! honored as written.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use uuid::Uuid;

use super::deadline::Deadline;

use crate::core::{
    Error, GitBlobData, GitCommitData, GitObjectKind, GitRefObservation, GitTagData, GitTreeData,
    GitTreeEntryData, Result, Ts,
};

/// True when the path looks like a git repository: it has a `.git` entry
/// (directory or gitdir-pointer file) or the markers of a bare repository.
/// Pure filesystem detection; git availability is checked on open.
pub fn looks_like_git_repository(path: &Path) -> bool {
    if path.join(".git").exists() {
        return true;
    }
    path.join("HEAD").is_file() && path.join("objects").is_dir() && path.join("refs").is_dir()
}

/// One worktree as listed by `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: String,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    /// Its directory is gone; git would remove the entry with `worktree prune`.
    pub prunable: bool,
    pub locked: bool,
    pub is_main: bool,
}

pub struct GitReader {
    /// The path git commands run in (working tree or bare repository).
    path: PathBuf,
    /// Canonical common dir; holds the repository identity marker.
    common_dir: PathBuf,
    /// Time limit for every git process this reader starts.
    deadline: Deadline,
}

impl GitReader {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_deadline(path, Deadline::none())
    }

    /// Like [`open`](Self::open); every git process of the reader is killed
    /// when `deadline` expires.
    pub fn open_with_deadline(path: &Path, deadline: Deadline) -> Result<Self> {
        let out = run(path, &deadline, &["rev-parse", "--git-common-dir"], None)?;
        let raw = String::from_utf8_lossy(&out).trim().to_string();
        if raw.is_empty() {
            return Err(Error::GitCommand {
                command: "rev-parse --git-common-dir".into(),
                stderr: "empty git-common-dir".into(),
            });
        }
        let common = Path::new(&raw);
        let common_dir = if common.is_absolute() {
            common.to_path_buf()
        } else {
            path.join(common)
        };
        let common_dir = std::fs::canonicalize(&common_dir).map_err(|e| Error::Io {
            path: common_dir.display().to_string(),
            source: e,
        })?;
        Ok(Self {
            path: path.to_path_buf(),
            common_dir,
            deadline,
        })
    }

    /// Reads or creates the repository identity marker in the common dir.
    /// The marker is Mindspace-local identity: it survives moving the
    /// repository, while a fresh clone starts without it and gets its own.
    /// Refusing to guess: if the marker cannot be persisted, ingestion
    /// fails instead of silently creating a second identity.
    pub fn repository_id(&self) -> Result<Uuid> {
        let marker = self.common_dir.join("mindspace-repository-id");
        match std::fs::read_to_string(&marker) {
            Ok(s) => {
                let id = s.trim();
                Uuid::parse_str(id).map_err(|_| Error::RepositoryIdentity {
                    path: marker.display().to_string(),
                    reason: "marker exists but is not a UUID".into(),
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let id = Uuid::now_v7();
                std::fs::write(&marker, id.to_string()).map_err(|e| Error::RepositoryIdentity {
                    path: marker.display().to_string(),
                    reason: format!("cannot write marker: {e}"),
                })?;
                Ok(id)
            }
            Err(e) => Err(Error::RepositoryIdentity {
                path: marker.display().to_string(),
                reason: format!("cannot read marker: {e}"),
            }),
        }
    }

    /// All refs (heads, remotes, tags, and HEAD), sorted by name. HEAD is
    /// included with its resolved target; its symbolic target is recorded
    /// when it points at a branch.
    pub fn refs(&self) -> Result<Vec<GitRefObservation>> {
        let out = run(
            &self.path,
            &self.deadline,
            &[
                "for-each-ref",
                "--format=%(refname)%00%(objectname)%00%(objecttype)%00%(symref)",
            ],
            None,
        )?;
        let text = String::from_utf8_lossy(&out);
        let mut refs: Vec<GitRefObservation> = Vec::new();
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            let mut parts = line.split('\u{0}');
            let name = parts.next().unwrap_or_default().to_string();
            let target_oid = parts.next().unwrap_or_default().to_string();
            let type_str = parts.next().unwrap_or_default().to_string();
            let symbolic = parts.next().unwrap_or_default().to_string();
            let target_kind = GitObjectKind::parse(&type_str).ok_or_else(|| Error::GitCommand {
                command: "for-each-ref".into(),
                stderr: format!("unknown object type '{type_str}' for ref '{name}'"),
            })?;
            refs.push(GitRefObservation {
                name,
                target_oid,
                target_kind,
                symbolic_target: if symbolic.is_empty() {
                    None
                } else {
                    Some(symbolic)
                },
            });
        }

        // HEAD: resolved target, plus the symbolic branch when attached.
        if let Ok(head_oid) = run(&self.path, &self.deadline, &["rev-parse", "HEAD"], None) {
            let oid = String::from_utf8_lossy(&head_oid).trim().to_string();
            let symbolic_target = match run(
                &self.path,
                &self.deadline,
                &["symbolic-ref", "-q", "HEAD"],
                None,
            ) {
                Ok(s) => {
                    let t = String::from_utf8_lossy(&s).trim().to_string();
                    if t.is_empty() {
                        None
                    } else {
                        Some(t)
                    }
                }
                // Detached HEAD: symbolic-ref exits non-zero with no output.
                Err(Error::GitCommand { .. }) => None,
                Err(e) => return Err(e),
            };
            let target_kind = if symbolic_target.is_some() {
                GitObjectKind::Commit
            } else {
                // Detached or otherwise unusual: ask git for the type.
                let checked = self.batch_check(std::slice::from_ref(&oid))?;
                checked
                    .first()
                    .map(|(_, k, _)| *k)
                    .unwrap_or(GitObjectKind::Commit)
            };
            refs.push(GitRefObservation {
                name: "HEAD".to_string(),
                target_oid: oid,
                target_kind,
                symbolic_target,
            });
        }
        // An unborn branch (no commits yet) has no HEAD target; nothing to
        // observe for it.
        refs.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(refs)
    }

    /// Every object reachable from the given tips (commits, trees, blobs,
    /// annotated tags), deduplicated and sorted. One git call.
    pub fn reachable_oids(&self, tips: &[String]) -> Result<Vec<String>> {
        if tips.is_empty() {
            return Ok(Vec::new());
        }
        let mut args: Vec<&str> = vec!["rev-list", "--objects", "--no-object-names"];
        args.extend(tips.iter().map(|s| s.as_str()));
        let out = run(&self.path, &self.deadline, &args, None)?;
        let text = String::from_utf8_lossy(&out);
        let mut set = std::collections::BTreeSet::new();
        for line in text.lines() {
            let oid = line.trim();
            if !oid.is_empty() {
                set.insert(oid.to_string());
            }
        }
        Ok(set.into_iter().collect())
    }

    /// Types and sizes for a list of oids. One git call.
    pub fn batch_check(&self, oids: &[String]) -> Result<Vec<(String, GitObjectKind, u64)>> {
        if oids.is_empty() {
            return Ok(Vec::new());
        }
        let input: Vec<u8> = oids.join("\n").into_bytes();
        let out = run(
            &self.path,
            &self.deadline,
            &["cat-file", "--batch-check"],
            Some(&input),
        )?;
        let text = String::from_utf8_lossy(&out);
        let mut result = Vec::with_capacity(oids.len());
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            let mut parts = line.rsplitn(3, ' ');
            let size = parts.next().unwrap_or_default();
            let kind = parts.next().unwrap_or_default();
            let oid = parts.next().unwrap_or_default();
            if kind == "missing" {
                continue;
            }
            let parsed = GitObjectKind::parse(kind).ok_or_else(|| Error::GitCommand {
                command: "cat-file --batch-check".into(),
                stderr: format!("unknown object type '{kind}' for '{oid}'"),
            })?;
            result.push((
                oid.to_string(),
                parsed,
                size.parse::<u64>().map_err(|_| Error::GitCommand {
                    command: "cat-file --batch-check".into(),
                    stderr: format!("bad size '{size}' for '{oid}'"),
                })?,
            ));
        }
        Ok(result)
    }

    /// Raw object contents for a list of oids. One git call.
    fn batch_raw(&self, oids: &[String]) -> Result<Vec<(String, GitObjectKind, Vec<u8>)>> {
        if oids.is_empty() {
            return Ok(Vec::new());
        }
        let input: Vec<u8> = oids.join("\n").into_bytes();
        let out = run(
            &self.path,
            &self.deadline,
            &["cat-file", "--batch"],
            Some(&input),
        )?;
        parse_batch_stream(&out)
    }

    /// Parses every commit, tree, and annotated tag in the object set.
    pub fn objects(
        &self,
        oids: &[String],
    ) -> Result<(Vec<GitCommitData>, Vec<GitTreeData>, Vec<GitTagData>)> {
        let checked = self.batch_check(oids)?;
        let structural: Vec<String> = checked
            .iter()
            .filter(|(_, k, _)| !matches!(k, GitObjectKind::Blob))
            .map(|(oid, _, _)| oid.clone())
            .collect();
        let raw = self.batch_raw(&structural)?;
        let mut commits = Vec::new();
        let mut trees = Vec::new();
        let mut tags = Vec::new();
        for (oid, kind, bytes) in raw {
            match kind {
                GitObjectKind::Commit => commits.push(parse_commit(&oid, &bytes)?),
                GitObjectKind::Tree => trees.push(parse_tree(&oid, &bytes)?),
                GitObjectKind::Tag => tags.push(parse_tag(&oid, &bytes)?),
                GitObjectKind::Blob => {}
            }
        }
        Ok((commits, trees, tags))
    }

    /// Blob contents by oid. One git call.
    pub fn blob_contents(&self, oids: &[String]) -> Result<Vec<GitBlobData>> {
        let checked = self.batch_check(oids)?;
        let sizes: std::collections::HashMap<String, u64> = checked
            .into_iter()
            .map(|(oid, _, size)| (oid, size))
            .collect();
        let raw = self.batch_raw(oids)?;
        Ok(raw
            .into_iter()
            .map(|(oid, _, bytes)| GitBlobData {
                content: Some(String::from_utf8_lossy(&bytes).into_owned()),
                size_bytes: sizes.get(&oid).copied().unwrap_or(bytes.len() as u64),
                oid,
            })
            .collect())
    }

    /// All entries of one commit's tree, recursively, as
    /// `(mode, kind, oid, path)` in ls-tree order. One git call per tip.
    /// Symlinks and gitlinks are returned with their true modes so callers
    /// can exclude them.
    pub fn commit_files(&self, commit: &str) -> Result<Vec<(String, String, String, String)>> {
        let out = run(
            &self.path,
            &self.deadline,
            &["ls-tree", "-r", "-z", commit],
            None,
        )?;
        let mut result = Vec::new();
        for record in String::from_utf8_lossy(&out).split('\0') {
            if record.is_empty() {
                continue;
            }
            let (meta, path) = match record.split_once('\t') {
                Some((m, p)) => (m, p),
                None => continue,
            };
            let mut parts = meta.split(' ');
            let mode = parts.next().unwrap_or_default().to_string();
            let kind = parts.next().unwrap_or_default().to_string();
            let oid = parts.next().unwrap_or_default().to_string();
            result.push((mode, kind, oid, path.to_string()));
        }
        Ok(result)
    }

    /// The commit HEAD resolves to. Errors for an unborn branch.
    pub fn head_commit(&self) -> Result<String> {
        let out = run(
            &self.path,
            &self.deadline,
            &["rev-parse", "--verify", "HEAD^{commit}"],
            None,
        )?;
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }

    /// HEAD commits of every live worktree of this repository (the main one
    /// and linked ones), sorted. Bare entries, prunable worktrees whose
    /// directory is gone, and unborn HEADs are skipped. One git call.
    pub fn worktree_heads(&self) -> Result<Vec<String>> {
        let mut heads = std::collections::BTreeSet::new();
        for w in self.worktrees()? {
            if w.bare || w.prunable {
                continue;
            }
            if let Some(h) = w.head.filter(|h| h.chars().any(|c| c != '0')) {
                heads.insert(h);
            }
        }
        Ok(heads.into_iter().collect())
    }

    /// Every worktree of this repository as git lists them; the first entry
    /// is the main worktree. One git call.
    pub fn worktrees(&self) -> Result<Vec<Worktree>> {
        let out = run(
            &self.path,
            &self.deadline,
            &["worktree", "list", "--porcelain"],
            None,
        )?;
        let text = String::from_utf8_lossy(&out);
        let mut result = Vec::new();
        for (i, block) in text.split("\n\n").enumerate() {
            let mut w = Worktree {
                path: String::new(),
                head: None,
                branch: None,
                detached: false,
                bare: false,
                prunable: false,
                locked: false,
                is_main: i == 0,
            };
            for line in block.lines() {
                if let Some(p) = line.strip_prefix("worktree ") {
                    w.path = p.to_string();
                } else if let Some(h) = line.strip_prefix("HEAD ") {
                    w.head = Some(h.trim().to_string());
                } else if let Some(b) = line.strip_prefix("branch ") {
                    w.branch = Some(b.trim().trim_start_matches("refs/heads/").to_string());
                } else if line == "detached" {
                    w.detached = true;
                } else if line == "bare" {
                    w.bare = true;
                } else if line.starts_with("prunable") {
                    w.prunable = true;
                } else if line.starts_with("locked") {
                    w.locked = true;
                }
            }
            if !w.path.is_empty() {
                result.push(w);
            }
        }
        Ok(result)
    }

    /// Configured remotes as `(name, raw url)`, sorted. The raw URL may
    /// contain credentials: callers must pass it through
    /// `ingest::remote::parse` before storing or showing it.
    pub fn remotes(&self) -> Result<Vec<(String, String)>> {
        let out = run_codes(
            &self.path,
            &self.deadline,
            &["config", "--null", "--get-regexp", r"^remote\..*\.url$"],
            None,
            &[0, 1],
        )?;
        let mut remotes = Vec::new();
        for record in String::from_utf8_lossy(&out).split('\0') {
            let Some((key, value)) = record.split_once('\n') else {
                continue;
            };
            let Some(name) = key
                .strip_prefix("remote.")
                .and_then(|k| k.strip_suffix(".url"))
            else {
                continue;
            };
            remotes.push((name.to_string(), value.to_string()));
        }
        remotes.sort();
        remotes.dedup();
        Ok(remotes)
    }

    /// The repository identity if a marker already exists; never creates one.
    pub fn existing_repository_id(&self) -> Result<Option<Uuid>> {
        let marker = self.common_dir.join("mindspace-repository-id");
        match std::fs::read_to_string(&marker) {
            Ok(s) => Uuid::parse_str(s.trim())
                .map(Some)
                .map_err(|_| Error::RepositoryIdentity {
                    path: marker.display().to_string(),
                    reason: "marker exists but is not a UUID".into(),
                }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::RepositoryIdentity {
                path: marker.display().to_string(),
                reason: format!("cannot read marker: {e}"),
            }),
        }
    }

    /// Blob entries of one commit's tree as `(path, size)`, symlinks and
    /// gitlinks excluded. One git call; reads the object database only.
    pub fn commit_blob_sizes(&self, commit: &str) -> Result<Vec<(String, u64)>> {
        let out = run(
            &self.path,
            &self.deadline,
            &["ls-tree", "-r", "-l", "-z", commit],
            None,
        )?;
        let mut result = Vec::new();
        for record in String::from_utf8_lossy(&out).split('\0') {
            let Some((meta, path)) = record.split_once('\t') else {
                continue;
            };
            let mut parts = meta.split_whitespace();
            let (mode, kind, _oid, size) = (
                parts.next().unwrap_or_default(),
                parts.next().unwrap_or_default(),
                parts.next(),
                parts.next().unwrap_or_default(),
            );
            if kind != "blob" || mode.starts_with("12") {
                continue;
            }
            result.push((path.to_string(), size.parse::<u64>().unwrap_or(0)));
        }
        Ok(result)
    }

    /// Deterministic ancestor check: is `old` an ancestor of `new`?
    /// Exit code 0 means provably yes; exit 1 means provably no.
    pub fn is_ancestor(&self, old: &str, new: &str) -> Result<bool> {
        let mut cmd = git_command(&self.path);
        cmd.args(["merge-base", "--is-ancestor", old, new])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = cmd.spawn().map_err(|e| Error::GitUnavailable {
            detail: format!("cannot run git: {e}"),
        })?;
        let out = collect_output(child, None, &self.deadline, "merge-base")?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            code => Err(Error::GitCommand {
                command: format!("merge-base --is-ancestor {old} {new}"),
                stderr: format!(
                    "exit code {code:?}: {}",
                    String::from_utf8_lossy(&out.stderr)
                ),
            }),
        }
    }
}

/// Environment variables that must never reach git: every `GIT_*` entry of the
/// caller is dropped, then the pinned values below are set.
const PINNED_ENV: &[(&str, &str)] = &[
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_NO_REPLACE_OBJECTS", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("GIT_PAGER", "cat"),
    ("GIT_PROTOCOL_FROM_USER", "0"),
    ("GIT_ATTR_NOSYSTEM", "1"),
];

/// `-c` overrides that disable the known config-driven ways for a read-only plumbing
/// command to run a program or touch the network.
const PINNED_CONFIG: &[&str] = &[
    "core.fsmonitor=false",
    "core.hooksPath=/dev/null",
    "core.alternateRefsCommand=",
    "core.pager=cat",
    "core.askPass=",
    "protocol.allow=never",
    "gc.auto=0",
    "maintenance.auto=false",
];

/// The single place a `git` process is created. `dir` becomes the working
/// directory; discovery of the repository is therefore based on it alone.
fn git_command(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
    for (k, v) in PINNED_ENV {
        cmd.env(k, v);
    }
    for c in PINNED_CONFIG {
        cmd.arg("-c").arg(c);
    }
    cmd.current_dir(dir);
    cmd
}

fn run(path: &Path, deadline: &Deadline, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
    run_codes(path, deadline, args, stdin, &[0])
}

/// Like [`run`], but exit codes in `ok_codes` count as success (for commands
/// such as `git config --get-regexp`, which exit 1 when nothing matches).
fn run_codes(
    path: &Path,
    deadline: &Deadline,
    args: &[&str],
    stdin: Option<&[u8]>,
    ok_codes: &[i32],
) -> Result<Vec<u8>> {
    deadline.check(&format!("starting `git {}`", args.first().unwrap_or(&"")))?;
    let mut cmd = git_command(path);
    cmd.args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().map_err(|e| Error::GitUnavailable {
        detail: format!("cannot run git: {e}"),
    })?;
    let out = collect_output(child, stdin, deadline, args.first().unwrap_or(&""))?;
    if !out.status.code().is_some_and(|c| ok_codes.contains(&c)) {
        return Err(Error::GitCommand {
            command: format!("git {}", args.join(" ")),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(out.stdout)
}

/// Feeds `stdin`, drains stdout and stderr, and waits for the process.
///
/// Without a limit this is the plain blocking path. With a limit, the process
/// is polled and killed when the limit expires, which is what turns a hung git
/// into a clear error instead of a silent stall. The reader threads are then
/// detached rather than joined: a grandchild holding a pipe open must not keep
/// the caller waiting. None of the git commands used here starts long-lived
/// children (unverified for every git version).
fn collect_output(
    mut child: Child,
    stdin: Option<&[u8]>,
    deadline: &Deadline,
    command: &str,
) -> Result<Output> {
    let unreadable = |e: std::io::Error| Error::GitUnavailable {
        detail: format!("cannot read git output: {e}"),
    };
    // Feed stdin from a separate thread while this thread drains stdout and
    // stderr. Writing all input first deadlocks once both pipe buffers fill
    // (git blocks writing output, we block writing input): that happened for
    // any repository with more than a few thousand objects.
    if deadline.limit().is_none() {
        let pipe = if stdin.is_some() {
            child.stdin.take()
        } else {
            None
        };
        return std::thread::scope(|scope| {
            if let (Some(input), Some(mut pipe)) = (stdin, pipe) {
                scope.spawn(move || {
                    // A broken pipe means git exited early; its status and
                    // stderr are reported by the caller, so the error is moot.
                    let _ = pipe.write_all(input);
                });
            }
            child.wait_with_output()
        })
        .map_err(unreadable);
    }

    fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    }
    let out_reader = drain(child.stdout.take());
    let err_reader = drain(child.stderr.take());
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let input = input.to_vec();
        std::thread::spawn(move || {
            let _ = pipe.write_all(&input);
        });
    }
    let mut nap = Duration::from_millis(1);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => return Err(unreadable(e)),
        }
        if deadline.expired() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(deadline.error(
                &format!("waiting for `git {command}`"),
                "the database was not changed",
            ));
        }
        let left = deadline.remaining().unwrap_or(nap);
        std::thread::sleep(nap.min(left.max(Duration::from_millis(1))));
        nap = (nap * 2).min(Duration::from_millis(20));
    };
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// Parses `cat-file --batch` output: a stream of
/// `<oid> <type> <size>\n<raw bytes>\n`.
fn parse_batch_stream(out: &[u8]) -> Result<Vec<(String, GitObjectKind, Vec<u8>)>> {
    let mut result = Vec::new();
    let mut pos = 0;
    while pos < out.len() {
        let Some(nl) = out[pos..].iter().position(|b| *b == b'\n').map(|i| pos + i) else {
            break;
        };
        let header = String::from_utf8_lossy(&out[pos..nl]).into_owned();
        let mut parts = header.split_whitespace();
        let oid = parts.next().unwrap_or_default().to_string();
        let kind_str = parts.next().unwrap_or_default();
        if oid.is_empty() || kind_str == "missing" {
            pos = nl + 1;
            continue;
        }
        let kind = GitObjectKind::parse(kind_str).ok_or_else(|| Error::GitCommand {
            command: "cat-file --batch".into(),
            stderr: format!("unknown object type '{kind_str}' for '{oid}'"),
        })?;
        let size: usize =
            parts
                .next()
                .unwrap_or_default()
                .parse()
                .map_err(|_| Error::GitCommand {
                    command: "cat-file --batch".into(),
                    stderr: format!("bad size in header '{header}'"),
                })?;
        let start = nl + 1;
        let end = start + size;
        if end > out.len() {
            return Err(Error::GitCommand {
                command: "cat-file --batch".into(),
                stderr: format!("truncated batch output at '{header}'"),
            });
        }
        result.push((oid, kind, out[start..end].to_vec()));
        pos = end + 1; // trailing newline after the payload
    }
    Ok(result)
}

/// Splits a person header (`Name <email> timestamp tz`) into its parts.
fn parse_person(line: &str) -> (String, String, Option<Ts>) {
    let open = line.find('<');
    let close = line.find('>');
    let (name, rest) = match open {
        Some(i) => (line[..i].trim().to_string(), &line[i..]),
        None => (line.trim().to_string(), ""),
    };
    let (email, tail) = match (open, close) {
        (Some(o), Some(c)) if c > o => (line[o + 1..c].to_string(), &line[c + 1..]),
        _ => (String::new(), rest),
    };
    let mut tail_parts = tail.split_whitespace();
    let time = tail_parts
        .next()
        .and_then(|s| s.parse::<i64>().ok())
        .map(|secs| Ts(secs.saturating_mul(1_000_000)));
    (name, email, time)
}

/// Parses a raw commit object: headers until a blank line (skipping
/// continuation lines of multi-line headers such as gpgsig), then message.
pub fn parse_commit(oid: &str, bytes: &[u8]) -> Result<GitCommitData> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::GitCommand {
        command: format!("parse commit {oid}"),
        stderr: "commit object is not UTF-8".into(),
    })?;
    let mut tree_oid = String::new();
    let mut parents = Vec::new();
    let mut author = (String::new(), String::new(), None);
    let mut committer = (String::new(), String::new(), None);
    let mut message = String::new();
    let mut in_message = false;
    for line in text.lines() {
        if in_message {
            message.push_str(line);
            message.push('\n');
            continue;
        }
        if line.is_empty() {
            in_message = true;
            continue;
        }
        if line.starts_with(' ') {
            continue; // continuation of a multi-line header (gpgsig, ...)
        }
        if let Some(v) = line.strip_prefix("tree ") {
            tree_oid = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("parent ") {
            parents.push(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("author ") {
            author = parse_person(v);
        } else if let Some(v) = line.strip_prefix("committer ") {
            committer = parse_person(v);
        }
    }
    Ok(GitCommitData {
        oid: oid.to_string(),
        tree_oid,
        parents,
        author_name: author.0,
        author_email: author.1,
        author_time: author.2.unwrap_or(Ts(0)),
        committer_name: committer.0,
        committer_email: committer.1,
        commit_time: committer.2.unwrap_or(Ts(0)),
        message: message.trim_end().to_string(),
    })
}

/// Parses a raw tree object: `mode SP name NUL 20-byte oid` entries.
pub fn parse_tree(oid: &str, bytes: &[u8]) -> Result<GitTreeData> {
    let mut entries = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let Some(sp) = bytes[pos..]
            .iter()
            .position(|b| *b == b' ')
            .map(|i| pos + i)
        else {
            break;
        };
        let mode = String::from_utf8_lossy(&bytes[pos..sp]).into_owned();
        let Some(nul) = bytes[sp + 1..]
            .iter()
            .position(|b| *b == 0)
            .map(|i| sp + 1 + i)
        else {
            break;
        };
        let name = String::from_utf8_lossy(&bytes[sp + 1..nul]).into_owned();
        if nul + 21 > bytes.len() {
            return Err(Error::GitCommand {
                command: format!("parse tree {oid}"),
                stderr: "truncated tree entry".into(),
            });
        }
        let target = &bytes[nul + 1..nul + 21];
        let target_oid = target
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let target_kind = if mode.starts_with("40") || mode.starts_with("04") {
            GitObjectKind::Tree
        } else if mode.starts_with("16") {
            GitObjectKind::Commit
        } else {
            GitObjectKind::Blob
        };
        entries.push(GitTreeEntryData {
            name,
            mode,
            target_oid,
            target_kind,
        });
        pos = nul + 21;
    }
    Ok(GitTreeData {
        oid: oid.to_string(),
        entries,
    })
}

/// Parses a raw annotated tag object.
pub fn parse_tag(oid: &str, bytes: &[u8]) -> Result<GitTagData> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::GitCommand {
        command: format!("parse tag {oid}"),
        stderr: "tag object is not UTF-8".into(),
    })?;
    let mut target_oid = String::new();
    let mut target_kind = GitObjectKind::Commit;
    let mut name = String::new();
    let mut tagger = (String::new(), String::new(), None);
    let mut message = String::new();
    let mut in_message = false;
    for line in text.lines() {
        if in_message {
            message.push_str(line);
            message.push('\n');
            continue;
        }
        if line.is_empty() {
            in_message = true;
            continue;
        }
        if line.starts_with(' ') {
            continue;
        }
        if let Some(v) = line.strip_prefix("object ") {
            target_oid = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("type ") {
            target_kind = GitObjectKind::parse(v.trim()).unwrap_or(GitObjectKind::Commit);
        } else if let Some(v) = line.strip_prefix("tag ") {
            name = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("tagger ") {
            tagger = parse_person(v);
        }
    }
    let _ = name;
    Ok(GitTagData {
        oid: oid.to_string(),
        target_oid,
        target_kind,
        tagger_name: tagger.0,
        tagger_email: tagger.1,
        tag_time: tagger.2,
        message: message.trim_end().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_batch_stream() {
        let payload = b"hello";
        let mut out = Vec::new();
        out.extend_from_slice(format!("abc123 blob {}\n", payload.len()).as_bytes());
        out.extend_from_slice(payload);
        out.push(b'\n');
        let parsed = parse_batch_stream(&out).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "abc123");
        assert_eq!(parsed[0].1, GitObjectKind::Blob);
        assert_eq!(parsed[0].2, b"hello".to_vec());
    }

    #[test]
    fn parses_commit_with_gpgsig_and_parents() {
        let raw = b"tree 0123456789012345678901234567890123456789\n\
                    parent 1111111111111111111111111111111111111111\n\
                    parent 2222222222222222222222222222222222222222\n\
                    author A U Thor <author@example.com> 1700000000 +0100\n\
                    committer C O Mitter <committer@example.com> 1700000001 +0100\n\
                    gpgsig -----BEGIN PGP SIGNATURE-----\n \
                    line two\n\
                    \n\
                    merge commit body\n";
        let c = parse_commit("abc", raw).unwrap();
        assert_eq!(c.parents.len(), 2);
        assert_eq!(c.tree_oid, "0123456789012345678901234567890123456789");
        assert_eq!(c.author_name, "A U Thor");
        assert_eq!(c.author_email, "author@example.com");
        assert_eq!(c.author_time, Ts(1_700_000_000_000_000));
        assert_eq!(c.committer_email, "committer@example.com");
        assert!(c.message.contains("merge commit body"));
    }

    #[test]
    fn parses_tree_entries() {
        let mut raw: Vec<u8> = Vec::new();
        raw.extend_from_slice(b"100644 README.md");
        raw.push(0);
        raw.extend_from_slice(&[0xab; 20]);
        raw.extend_from_slice(b"40000 src");
        raw.push(0);
        raw.extend_from_slice(&[0xcd; 20]);
        let t = parse_tree("abc", &raw).unwrap();
        assert_eq!(t.entries.len(), 2);
        assert_eq!(t.entries[0].name, "README.md");
        assert_eq!(t.entries[0].target_kind, GitObjectKind::Blob);
        assert_eq!(t.entries[0].target_oid, "ab".repeat(20));
        assert_eq!(t.entries[1].target_kind, GitObjectKind::Tree);
    }

    /// Regression: input and output both far larger than a pipe buffer
    /// (64 KiB) used to deadlock `run` because stdin was written fully
    /// before stdout was read.
    #[test]
    fn run_handles_input_and_output_larger_than_pipe_buffers() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let st = Command::new("git")
                .current_dir(dir.path())
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(args)
                .output()
                .unwrap();
            assert!(st.status.success(), "git {args:?}: {:?}", st.stderr);
            String::from_utf8(st.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q"]);
        std::fs::write(dir.path().join("a.md"), "# a\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        let oid = git(&["rev-parse", "HEAD"]);

        let n = 30_000;
        let input = vec![oid.as_str(); n].join("\n");
        let (tx, rx) = std::sync::mpsc::channel();
        let path = dir.path().to_path_buf();
        std::thread::spawn(move || {
            let out = run(
                &path,
                &Deadline::none(),
                &["cat-file", "--batch-check"],
                Some(input.as_bytes()),
            );
            let _ = tx.send(out);
        });
        let out = rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("run deadlocked")
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out).lines().count(), n);
    }

    #[test]
    fn parses_annotated_tag() {
        let raw = b"object 1111111111111111111111111111111111111111\n\
                    type commit\n\
                    tag v1.0\n\
                    tagger T Agger <tag@example.com> 1700000000 +0000\n\
                    \n\
                    release one\n";
        let t = parse_tag("abc", raw).unwrap();
        assert_eq!(t.target_oid, "1111111111111111111111111111111111111111");
        assert_eq!(t.target_kind, GitObjectKind::Commit);
        assert_eq!(t.tagger_email, "tag@example.com");
        assert_eq!(t.tag_time, Some(Ts(1_700_000_000_000_000)));
        assert_eq!(t.message, "release one");
    }
}
