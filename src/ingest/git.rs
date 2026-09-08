//! Reading git repositories through the system git binary, plumbing only.
//!
//! Deliberate choice (ADR-014): the system `git` is used with stable,
//! machine-readable plumbing formats (`for-each-ref --format`, `cat-file
//! --batch`, `rev-list --objects --no-object-names`, `ls-tree -z`,
//! `merge-base --is-ancestor`). Localized human output is never parsed.
//! All commands are read-only; no hook, script, or working-tree state is
//! involved. Repository contents are untrusted input: only the object
//! database is read, never the working tree.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use uuid::Uuid;

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

pub struct GitReader {
    /// The path git commands run in (working tree or bare repository).
    path: PathBuf,
    /// Canonical common dir; holds the repository identity marker.
    common_dir: PathBuf,
}

impl GitReader {
    pub fn open(path: &Path) -> Result<Self> {
        let out = run(path, &["rev-parse", "--git-common-dir"], None)?;
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
        if let Ok(head_oid) = run(&self.path, &["rev-parse", "HEAD"], None) {
            let oid = String::from_utf8_lossy(&head_oid).trim().to_string();
            let symbolic_target = match run(&self.path, &["symbolic-ref", "-q", "HEAD"], None) {
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
        let out = run(&self.path, &args, None)?;
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
        let out = run(&self.path, &["cat-file", "--batch-check"], Some(&input))?;
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
        let out = run(&self.path, &["cat-file", "--batch"], Some(&input))?;
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
        let out = run(&self.path, &["ls-tree", "-r", "-z", commit], None)?;
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

    /// Deterministic ancestor check: is `old` an ancestor of `new`?
    /// Exit code 0 means provably yes; exit 1 means provably no.
    pub fn is_ancestor(&self, old: &str, new: &str) -> Result<bool> {
        let out = Command::new("git")
            .current_dir(&self.path)
            .args(["merge-base", "--is-ancestor", old, new])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| Error::GitUnavailable {
                detail: format!("cannot run git: {e}"),
            })?;
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

fn run(path: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.current_dir(path)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| Error::GitUnavailable {
        detail: format!("cannot run git: {e}"),
    })?;
    if let Some(input) = stdin {
        if let Some(mut pipe) = child.stdin.take() {
            let _ = pipe.write_all(input);
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| Error::GitUnavailable {
            detail: format!("cannot read git output: {e}"),
        })?;
    if !out.status.success() {
        return Err(Error::GitCommand {
            command: format!("git {}", args.join(" ")),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(out.stdout)
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
