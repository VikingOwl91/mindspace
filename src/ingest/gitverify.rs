//! M7.3b.5c: independent verification of Git objects received from a worker.
//!
//! A worker that reads a repository may be compromised. The frontend therefore never trusts the
//! *parsed* fields a worker reports; it receives the RAW bytes of the commit and trees, recomputes
//! their object ids, parses them itself and checks how they relate. The worker can then only
//! choose *which* objects to report, never *what an object contains*: an object id binds its bytes.
//!
//! Scope (v1): HEAD. One commit and the trees below its root tree. Parent commits are NOT part of
//! the transport; their ids are kept as explicitly unverified external references. Blobs are
//! checked where their content travels (see the stream assembler); the rest are `(oid, size)`
//! claims, labelled as such.
//!
//! Three states are kept apart in the result ([`VerificationSummary`]):
//! 1. **content verified**: the bytes were received and hash to the id (commit, trees, blobs whose
//!    content travelled);
//! 2. **reference verified**: named by a content-verified tree that is reachable from the HEAD
//!    commit (every blob id, and every subtree, of the closure);
//! 3. **external, not verified**: parent commit ids and gitlink (submodule) ids.
//!
//! SHA-1 only. SHA-256 repositories are refused earlier ([`crate::core::Error::UnsupportedObjectFormat`]);
//! every id here is exactly 40 lowercase hex digits.
//!
//! The SHA-1 below is a plain implementation (no collision detection): no collision-detecting
//! implementation is available to this build without a new dependency. See
//! `docs/m7.3b5c-verified-git-transport-plan.md` section 2.3 and the report of this milestone.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::core::{Error, GitCommitData, GitObjectKind, GitTreeData, Result};

use super::git::{parse_commit, parse_tree};

fn bad<T>(why: impl Into<String>) -> Result<T> {
    Err(Error::InvalidExtraction(format!(
        "git object verification: {}",
        why.into()
    )))
}

// ------------------------------------------------------------------------------------- SHA-1

/// SHA-1 of `data` (FIPS 180-4). Streaming-free: the inputs here are bounded by the limits.
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(data.len() + 72);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    for block in padded.as_chunks::<64>().0.iter() {
        let mut w = [0u32; 80];
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

pub fn hex_encode(bytes: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(D[(b >> 4) as usize] as char);
        s.push(D[(b & 15) as usize] as char);
    }
    s
}

/// Decodes lowercase or uppercase hex; `None` for an odd length or a non-hex digit.
pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if !b.len().is_multiple_of(2) {
        return None;
    }
    let nib = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    b.as_chunks::<2>()
        .0
        .iter()
        .map(|p| Some(nib(p[0])? << 4 | nib(p[1])?))
        .collect()
}

fn kind_name(kind: GitObjectKind) -> &'static str {
    match kind {
        GitObjectKind::Commit => "commit",
        GitObjectKind::Tree => "tree",
        GitObjectKind::Blob => "blob",
        GitObjectKind::Tag => "tag",
    }
}

/// The Git object id of `bytes` as an object of `kind`: SHA-1 of `"<kind> <len>\0" + bytes`.
pub fn object_id(kind: GitObjectKind, bytes: &[u8]) -> String {
    let mut buf = Vec::with_capacity(bytes.len() + 32);
    buf.extend_from_slice(kind_name(kind).as_bytes());
    buf.push(b' ');
    buf.extend_from_slice(bytes.len().to_string().as_bytes());
    buf.push(0);
    buf.extend_from_slice(bytes);
    hex_encode(&sha1(&buf))
}

/// Exactly 40 lowercase hex digits.
pub fn is_sha1_oid(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

// ------------------------------------------------------------------------------------ limits

/// Bounds of one verified HEAD transport. Conservative on purpose: a repository above them is
/// refused (never truncated), and the checks run when a record is accepted AND during traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyLimits {
    /// Raw commit objects: HEAD's commit only.
    pub max_commits: usize,
    /// Raw tree objects.
    pub max_trees: usize,
    /// Entries of one tree, and in all trees together.
    pub max_tree_entries: usize,
    /// `Blob` records (size claims included).
    pub max_blobs: usize,
    /// One raw commit or tree. A tree of ~6,000 entries is about this large.
    pub max_raw_object_bytes: usize,
    /// All raw objects together.
    pub max_total_object_bytes: u64,
    /// Nesting of directories.
    pub max_depth: usize,
    /// Tree entries visited while walking the closure. A tree that names the same subtree many
    /// times is a DAG, and a walk by path expands it: this bounds that expansion.
    pub max_work: u64,
    /// Longest path of the closure.
    pub max_path_bytes: usize,
}

impl Default for VerifyLimits {
    fn default() -> Self {
        Self {
            max_commits: 1,
            max_trees: 20_000,
            max_tree_entries: 200_000,
            max_blobs: 100_000,
            max_raw_object_bytes: 256 * 1024,
            max_total_object_bytes: 64 * 1024 * 1024,
            max_depth: 64,
            max_work: 1_000_000,
            max_path_bytes: 4096,
        }
    }
}

// ------------------------------------------------------------------------------------ result

/// One raw object as received: the claimed id, the claimed kind and the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawObject {
    pub kind: GitObjectKind,
    pub oid: String,
    pub bytes: Vec<u8>,
}

/// One path of the HEAD tree: `(path, mode, blob or gitlink id)`. Reference verified: named by a
/// content-verified tree reachable from the HEAD commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeFile {
    pub path: String,
    pub mode: String,
    pub oid: String,
    pub kind: GitObjectKind,
}

/// What was verified, and how strongly. Reported to the caller, never inferred from a flag the
/// worker sent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VerificationSummary {
    /// Commit and trees whose bytes hash to their ids.
    pub content_verified_objects: u64,
    /// Blobs whose content travelled and hashes to the id.
    pub content_verified_blobs: u64,
    /// Blob ids named by a verified, reachable tree (a superset of the content-verified blobs).
    pub reference_verified_blobs: u64,
    /// Blob records whose content did not travel: `(oid, size)` claims, reference verified only.
    pub unverified_blobs: u64,
    /// Parent commit ids and gitlink ids: named by verified objects, themselves not verified.
    pub external_references: u64,
}

/// What a producer sends for a verified HEAD transport: the HEAD claim, the raw commit and trees,
/// and the `(oid, size)` of every blob the trees name (reference claims until their content is
/// verified).
#[derive(Debug, Clone)]
pub struct HeadClosure {
    pub symbolic: Option<String>,
    pub commit_oid: String,
    pub objects: Vec<RawObject>,
    pub blobs: Vec<(String, u64)>,
}

#[derive(Debug, Clone)]
pub struct VerifiedHead {
    pub commit: GitCommitData,
    /// Every tree of the closure, sorted by id.
    pub trees: Vec<GitTreeData>,
    /// Every path of the HEAD tree that names a blob or a gitlink, sorted by path.
    pub files: Vec<TreeFile>,
    /// Parent ids, in commit order: external, not verified.
    pub external_parents: Vec<String>,
    pub summary: VerificationSummary,
}

// ------------------------------------------------------------------------------- verification

/// A tree entry as the strict parser reads it.
struct Entry {
    mode: String,
    name: String,
    oid: String,
}

fn mode_kind(mode: &str) -> Option<GitObjectKind> {
    match mode {
        "40000" => Some(GitObjectKind::Tree),
        "160000" => Some(GitObjectKind::Commit),
        "100644" | "100755" | "120000" => Some(GitObjectKind::Blob),
        _ => None,
    }
}

/// Strict tree parser (SHA-1, 20-byte entries). Refuses what git itself would: a truncated or
/// unknown-mode entry, an empty or path-like name, a `.git` component, a repeated name, trailing
/// bytes. (Entry ORDER is not enforced: it affects nothing the receiver stores.)
fn parse_tree_strict(oid: &str, bytes: &[u8], max_entries: usize) -> Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let Some(sp) = bytes[pos..]
            .iter()
            .position(|b| *b == b' ')
            .map(|i| pos + i)
        else {
            return bad(format!("tree {oid}: an entry without a mode terminator"));
        };
        let mode = std::str::from_utf8(&bytes[pos..sp])
            .map_err(|_| Error::InvalidExtraction(format!("tree {oid}: a mode is not text")))?;
        if mode_kind(mode).is_none() {
            return bad(format!(
                "tree {oid}: unsupported entry mode `{}`",
                short(mode)
            ));
        }
        let Some(nul) = bytes[sp + 1..]
            .iter()
            .position(|b| *b == 0)
            .map(|i| sp + 1 + i)
        else {
            return bad(format!("tree {oid}: an entry without a name terminator"));
        };
        let name = std::str::from_utf8(&bytes[sp + 1..nul]).map_err(|_| {
            Error::InvalidExtraction(format!("tree {oid}: an entry name is not UTF-8"))
        })?;
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return bad(format!("tree {oid}: invalid entry name `{}`", short(name)));
        }
        if name.eq_ignore_ascii_case(".git") {
            return bad(format!("tree {oid}: an entry named `.git`"));
        }
        let end = nul
            .checked_add(21)
            .ok_or_else(|| Error::InvalidExtraction("tree length overflow".into()))?;
        if end > bytes.len() {
            return bad(format!("tree {oid}: a truncated entry"));
        }
        if entries.len() >= max_entries {
            return bad(format!(
                "tree {oid}: more than {max_entries} entries (limit)"
            ));
        }
        entries.push(Entry {
            mode: mode.to_string(),
            name: name.to_string(),
            oid: hex_encode(&bytes[nul + 1..end]),
        });
        pos = end;
    }
    // git never repeats a name within a tree; a tree that does is not one git wrote, and the
    // ambiguity (which entry does the path name?) is exactly what a hostile worker would want.
    // (Every name is checked, not just neighbours: an unsorted tree could hide a repeat.)
    let mut names: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for e in &entries {
        if !names.insert(e.name.as_str()) {
            return bad(format!("tree {oid}: duplicate entry `{}`", short(&e.name)));
        }
    }
    Ok(entries)
}

fn short(s: &str) -> String {
    s.chars().take(80).collect()
}

/// Checks the headers of a raw commit the way git writes them: `tree`, then `parent`s, then
/// `author` and `committer`. Returns the parent ids.
fn check_commit(oid: &str, bytes: &[u8]) -> Result<(String, Vec<String>)> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Error::InvalidExtraction(format!("commit {oid}: not UTF-8")))?;
    let mut lines = text.split('\n');
    let tree = match lines.next().and_then(|l| l.strip_prefix("tree ")) {
        Some(t) if is_sha1_oid(t) => t.to_string(),
        _ => {
            return bad(format!(
                "commit {oid}: the first header is not a valid `tree`"
            ))
        }
    };
    let mut parents = Vec::new();
    let mut saw_author = false;
    let mut saw_committer = false;
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some(p) = line.strip_prefix("parent ") {
            if saw_author || saw_committer {
                return bad(format!("commit {oid}: a `parent` after `author`"));
            }
            if !is_sha1_oid(p) {
                return bad(format!("commit {oid}: malformed parent id `{}`", short(p)));
            }
            parents.push(p.to_string());
        } else if line.starts_with("author ") {
            saw_author = true;
        } else if line.starts_with("committer ") {
            saw_committer = true;
        } else if line.starts_with("tree ") {
            return bad(format!("commit {oid}: more than one `tree`"));
        }
    }
    if !saw_author || !saw_committer {
        return bad(format!("commit {oid}: missing `author` or `committer`"));
    }
    Ok((tree, parents))
}

/// Verifies the raw objects of a HEAD transport and builds the closure.
///
/// Refuses, with a named reason: a kind other than commit/tree; a size over the limit; an id that
/// is not the hash of the bytes; a duplicate id; a head commit that is missing or not a commit; a
/// malformed commit or tree; a subtree or root tree that was not sent (dangling); a tree entry
/// whose declared mode contradicts the object it names; objects that are not reachable from the
/// head commit (including a second commit, whatever it claims to be); and any limit.
pub fn verify_head(
    head_oid: &str,
    objects: &[RawObject],
    limits: &VerifyLimits,
) -> Result<VerifiedHead> {
    if !is_sha1_oid(head_oid) {
        return bad(format!("malformed head id `{}`", short(head_oid)));
    }
    let mut total: u64 = 0;
    let mut commits = 0usize;
    let mut trees = 0usize;
    let mut by_id: HashMap<&str, &RawObject> = HashMap::with_capacity(objects.len());
    for o in objects {
        if !is_sha1_oid(&o.oid) {
            return bad(format!("malformed object id `{}`", short(&o.oid)));
        }
        match o.kind {
            GitObjectKind::Commit => commits += 1,
            GitObjectKind::Tree => trees += 1,
            other => {
                return bad(format!(
                    "a {} object `{}` is not part of the HEAD transport",
                    kind_name(other),
                    short(&o.oid)
                ))
            }
        }
        if commits > limits.max_commits {
            return bad(format!(
                "more than {} commit object(s) (limit); parents are not transported",
                limits.max_commits
            ));
        }
        if trees > limits.max_trees {
            return bad(format!(
                "more than {} tree objects (limit)",
                limits.max_trees
            ));
        }
        if o.bytes.len() > limits.max_raw_object_bytes {
            return bad(format!(
                "object `{}` is {} bytes (limit {})",
                short(&o.oid),
                o.bytes.len(),
                limits.max_raw_object_bytes
            ));
        }
        total = total
            .checked_add(o.bytes.len() as u64)
            .ok_or_else(|| Error::InvalidExtraction("object byte total overflow".into()))?;
        if total > limits.max_total_object_bytes {
            return bad(format!(
                "more than {} bytes of objects (limit)",
                limits.max_total_object_bytes
            ));
        }
        let actual = object_id(o.kind, &o.bytes);
        if actual != o.oid {
            return bad(format!(
                "the {} claimed as `{}` hashes to `{actual}`",
                kind_name(o.kind),
                short(&o.oid)
            ));
        }
        if by_id.insert(&o.oid, o).is_some() {
            return bad(format!("duplicate object `{}`", short(&o.oid)));
        }
    }

    // ---- the head commit, parsed here from its raw bytes
    let Some(head) = by_id.get(head_oid) else {
        return bad("the head commit was not sent");
    };
    if head.kind != GitObjectKind::Commit {
        return bad("the head id names an object that is not a commit");
    }
    let (root_oid, parents) = check_commit(head_oid, &head.bytes)?;
    let commit = parse_commit(head_oid, &head.bytes)?;
    if commit.tree_oid != root_oid || commit.parents != parents {
        return bad("the commit parser and the strict check disagree");
    }

    // ---- walk the closure by path; shared subtrees are walked again, and counted
    let mut reached_trees: BTreeSet<String> = BTreeSet::new();
    let mut parsed: HashMap<String, Vec<Entry>> = HashMap::new();
    let mut files: Vec<TreeFile> = Vec::new();
    let mut work: u64 = 0;
    let mut entries_total: usize = 0;
    // (tree id, path prefix, depth); explicit stack: recursion would follow the input's depth
    let mut stack: Vec<(String, String, usize)> = vec![(root_oid.clone(), String::new(), 0)];
    while let Some((tid, prefix, depth)) = stack.pop() {
        if depth > limits.max_depth {
            return bad(format!(
                "directories nested deeper than {} (limit)",
                limits.max_depth
            ));
        }
        let Some(raw) = by_id.get(tid.as_str()) else {
            return bad(format!("tree `{}` is named but was not sent", short(&tid)));
        };
        if raw.kind != GitObjectKind::Tree {
            return bad(format!(
                "`{}` is named as a tree but is a {}",
                short(&tid),
                kind_name(raw.kind)
            ));
        }
        reached_trees.insert(tid.clone());
        if !parsed.contains_key(&tid) {
            let e = parse_tree_strict(&tid, &raw.bytes, limits.max_tree_entries)?;
            entries_total = entries_total
                .checked_add(e.len())
                .ok_or_else(|| Error::InvalidExtraction("entry count overflow".into()))?;
            if entries_total > limits.max_tree_entries {
                return bad(format!(
                    "more than {} tree entries in all (limit)",
                    limits.max_tree_entries
                ));
            }
            parsed.insert(tid.clone(), e);
        }
        let entries = &parsed[&tid];
        for e in entries {
            work += 1;
            if work > limits.max_work {
                return bad(format!(
                    "the closure needs more than {} entry visits (limit)",
                    limits.max_work
                ));
            }
            let path = if prefix.is_empty() {
                e.name.clone()
            } else {
                format!("{prefix}/{}", e.name)
            };
            if path.len() > limits.max_path_bytes {
                return bad(format!(
                    "a path over {} bytes (limit)",
                    limits.max_path_bytes
                ));
            }
            match mode_kind(&e.mode) {
                Some(GitObjectKind::Tree) => stack.push((e.oid.clone(), path, depth + 1)),
                Some(kind) => files.push(TreeFile {
                    path,
                    mode: e.mode.clone(),
                    oid: e.oid.clone(),
                    kind,
                }),
                None => return bad("unreachable: modes were checked while parsing"),
            }
        }
    }

    // ---- nothing may be left over: an object nobody names is not part of the closure
    for o in objects {
        let reached = match o.kind {
            GitObjectKind::Commit => o.oid == head_oid,
            _ => reached_trees.contains(&o.oid),
        };
        if !reached {
            return bad(format!(
                "the {} `{}` is not reachable from the head commit",
                kind_name(o.kind),
                short(&o.oid)
            ));
        }
    }

    // ---- results, in a deterministic order
    let mut tree_data: Vec<GitTreeData> = Vec::with_capacity(reached_trees.len());
    for tid in &reached_trees {
        let raw = by_id[tid.as_str()];
        tree_data.push(parse_tree(tid, &raw.bytes)?);
    }
    files.sort_by(|a, b| (&a.path, &a.oid).cmp(&(&b.path, &b.oid)));
    let blob_ids: BTreeSet<&str> = files
        .iter()
        .filter(|f| f.kind == GitObjectKind::Blob)
        .map(|f| f.oid.as_str())
        .collect();
    let gitlinks = files
        .iter()
        .filter(|f| f.kind == GitObjectKind::Commit)
        .count();
    let summary = VerificationSummary {
        content_verified_objects: 1 + reached_trees.len() as u64,
        content_verified_blobs: 0,
        reference_verified_blobs: blob_ids.len() as u64,
        unverified_blobs: blob_ids.len() as u64,
        external_references: (parents.len() + gitlinks) as u64,
    };
    Ok(VerifiedHead {
        commit,
        trees: tree_data,
        files,
        external_parents: parents,
        summary,
    })
}

/// Checks the content of a blob against its id.
pub fn verify_blob(oid: &str, bytes: &[u8]) -> Result<()> {
    let actual = object_id(GitObjectKind::Blob, bytes);
    if actual != oid {
        return bad(format!(
            "the blob claimed as `{}` hashes to `{actual}`",
            short(oid)
        ));
    }
    Ok(())
}

/// Unused-set helper for callers that need the blob ids a verified head references.
pub fn referenced_blobs(head: &VerifiedHead) -> BTreeMap<String, Vec<&TreeFile>> {
    let mut m: BTreeMap<String, Vec<&TreeFile>> = BTreeMap::new();
    for f in head.files.iter().filter(|f| f.kind == GitObjectKind::Blob) {
        m.entry(f.oid.clone()).or_default().push(f);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hexs(b: &[u8]) -> String {
        hex_encode(b)
    }

    #[test]
    fn sha1_known_vectors() {
        assert_eq!(hexs(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hexs(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hexs(&sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        // block boundaries: 55, 56, 63, 64, 65 bytes
        let expect = [
            (55, "c1c8bbdc22796e28c0e15163d20899b65621d65a"),
            (56, "c2db330f6083854c99d4b5bfb6e8f29f201be699"),
            (63, "03f09f5b158a7a8cdad920bddc29b81c18a551f5"),
            (64, "0098ba824b5c16427bd7a1122a5a442a25ec644d"),
            (65, "11655326c708d70319be2610e8a57d9a5b959d3b"),
        ];
        for (n, want) in expect {
            assert_eq!(hexs(&sha1(&vec![b'a'; n])), want, "{n} bytes");
        }
    }

    #[test]
    fn object_ids_match_gits_for_the_well_known_empty_objects() {
        assert_eq!(
            object_id(GitObjectKind::Blob, b""),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        assert_eq!(
            object_id(GitObjectKind::Tree, b""),
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
        );
        assert_eq!(
            object_id(GitObjectKind::Blob, b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }

    #[test]
    fn hex_round_trips_and_refuses_garbage() {
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(hex_decode(&hex_encode(&all)).unwrap(), all);
        assert!(hex_decode("abc").is_none());
        assert!(hex_decode("zz").is_none());
        assert_eq!(hex_decode("AB").unwrap(), vec![0xab]);
    }
}
