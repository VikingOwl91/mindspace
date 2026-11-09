//! M7.3b.5c: verified Git-object transport.
//!
//! A Git worker may be compromised. The frontend receives the RAW commit and tree objects,
//! recomputes their ids, parses them itself and checks how they relate; it never trusts parsed
//! fields. These tests (a) accept a real repository's transport and compare the result with the
//! independent in-process ingestion, (b) fabricate hostile objects with CORRECT hashes, so each
//! test reaches the check it names instead of failing at the hash, and (c) tamper with the stream.

#![allow(clippy::field_reassign_with_default)]

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use mindspace::core::{GitObjectKind, Uuid};
use mindspace::ingest::confined_git::{ConfinedGitReader, GitExecutable};
use mindspace::ingest::deadline::Deadline;
use mindspace::ingest::extract::{persist_git, Extracted, ExtractedGit, StoreContext};
use mindspace::ingest::gitverify::{
    hex_encode, object_id, verify_blob, verify_head, HeadClosure, RawObject, VerifyLimits,
};
use mindspace::ingest::outline::PathSelection;
use mindspace::ingest::stream::{
    consume, emit_verified, FrameWriter, Kind, ReadStats, Record, RecordSink, StreamLimits,
};
use mindspace::ingest::{extract_git_repository_selected_with_deadline, GitScope};
use mindspace::storage::Store;
use mindspace::{initialize, open_existing, SqliteStore};

const GIT: &str = "/usr/bin/git";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git available");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// ------------------------------------------------------------------------------------ fixture

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    db_path: PathBuf,
    extracted: ExtractedGit,
    closure: HeadClosure,
    parent: String,
}

fn fixture() -> Option<Fixture> {
    if !Path::new(GIT).exists() {
        eprintln!("SKIP: {GIT} not found");
        return None;
    }
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let repo = base.join("repo");
    for d in ["src/inner", "docs", "vendor"] {
        std::fs::create_dir_all(repo.join(d)).unwrap();
    }
    git(&repo, &["init", "-q", "-b", "main"]);
    let w = |rel: &str, text: &str| std::fs::write(repo.join(rel), text).unwrap();
    w("README.md", "# Readme\n\nalpha\n");
    w("docs/guide.md", "# Guide\n\nbeta text\n");
    w(
        "src/lib.rs",
        "pub mod util;\npub fn run() { util::helper(); }\n",
    );
    w("src/util.rs", "pub fn helper() {}\n");
    w("src/inner/deep.rs", "pub fn deep() {}\n");
    w("empty.txt", "");
    w("blob.bin", "\u{0}\u{1}binary\u{2}");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "one"]);
    let parent = git(&repo, &["rev-parse", "HEAD"]);
    w("src/util.rs", "pub fn helper() {}\npub fn extra() {}\n");
    w("run.sh", "#!/bin/sh\necho hi\n");
    std::fs::set_permissions(
        repo.join("run.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    std::os::unix::fs::symlink("README.md", repo.join("link")).unwrap();
    git(&repo, &["add", "-A"]);
    // a gitlink (submodule entry): named by the tree, never followed
    git(
        &repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{parent},vendor/sub"),
        ],
    );
    git(&repo, &["commit", "-q", "-m", "two"]);

    let db_path = base.join("mind.db");
    initialize(&db_path).unwrap();
    open_existing(&db_path)
        .unwrap()
        .create_space("p", "")
        .unwrap();
    let store = open_existing(&db_path).unwrap();
    let extracted = extract_git_repository_selected_with_deadline(
        &StoreContext(&store),
        &repo,
        &["p"],
        &PathSelection::all(),
        GitScope::Head,
        &Deadline::none(),
    )
    .unwrap();
    drop(store);
    let closure = closure_of(&repo, &VerifyLimits::default());
    Some(Fixture {
        _tmp: tmp,
        repo,
        db_path,
        extracted,
        closure,
        parent,
    })
}

fn closure_of(repo: &Path, limits: &VerifyLimits) -> HeadClosure {
    use std::os::fd::AsFd;
    let f = std::fs::File::open(repo).unwrap();
    let exe = GitExecutable::resolve(Path::new(GIT)).unwrap();
    ConfinedGitReader::open(f.as_fd(), &exe, Deadline::none())
        .unwrap()
        .head_closure(limits)
        .unwrap()
}

impl Fixture {
    fn store(&self) -> SqliteStore {
        open_existing(&self.db_path).unwrap()
    }
    /// The records of the honest transport.
    fn records(&self) -> Vec<Record> {
        let mut c = Collect(Vec::new());
        emit_verified(
            &mut c,
            self.extracted.clone(),
            &self.closure,
            &Default::default(),
        )
        .unwrap();
        c.0
    }
    /// The records with the code input at `index` sent as a reused-derivation placeholder.
    fn records_with_placeholder(&self, index: usize) -> Vec<Record> {
        let mut c = Collect(Vec::new());
        emit_verified(
            &mut c,
            self.extracted.clone(),
            &self.closure,
            &[index].into_iter().collect(),
        )
        .unwrap();
        c.0
    }
    fn head(&self) -> &str {
        &self.closure.commit_oid
    }
}

/// A sink that keeps the records, to be tampered with and re-encoded.
struct Collect(Vec<Record>);

impl RecordSink for Collect {
    fn emit_encoded(&mut self, _: &'static str, json: Vec<u8>) -> mindspace::core::Result<()> {
        self.0.push(serde_json::from_slice(&json).unwrap());
        Ok(())
    }
}

fn encode(op: Uuid, records: &[Record]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut w = FrameWriter::new(&mut out, op, None);
        for r in records {
            if matches!(r, Record::End { .. }) {
                continue;
            }
            w.emit(r).unwrap();
        }
        w.finish().unwrap();
    }
    out
}

fn receive(
    records: &[Record],
    limits: &StreamLimits,
) -> mindspace::core::Result<(Extracted, ReadStats)> {
    let op = Uuid::now_v7();
    let bytes = encode(op, records);
    consume(
        Cursor::new(bytes),
        op,
        Kind::GitVerified,
        None,
        limits,
        None,
    )
}

fn rejected(records: &[Record], needle: &str) {
    let e = receive(records, &StreamLimits::default())
        .err()
        .unwrap_or_else(|| panic!("a hostile stream was accepted (wanted `{needle}`)"))
        .to_string();
    assert!(e.contains(needle), "wanted `{needle}`, got: {e}");
}

// ------------------------------------------------------------ fabricating hostile objects

fn commit_bytes(tree: &str, parents: &[&str]) -> Vec<u8> {
    let mut s = format!("tree {tree}\n");
    for p in parents {
        s.push_str(&format!("parent {p}\n"));
    }
    s.push_str("author a <a@b> 1 +0000\ncommitter a <a@b> 1 +0000\n\nmsg\n");
    s.into_bytes()
}

/// `entries`: (mode, name, 40-digit id)
fn tree_bytes(entries: &[(&str, &str, &str)]) -> Vec<u8> {
    let mut b = Vec::new();
    for (mode, name, id) in entries {
        b.extend_from_slice(mode.as_bytes());
        b.push(b' ');
        b.extend_from_slice(name.as_bytes());
        b.push(0);
        b.extend_from_slice(&decode(id));
    }
    b
}

fn decode(h: &str) -> Vec<u8> {
    mindspace::ingest::gitverify::hex_decode(h).unwrap()
}

fn obj(kind: GitObjectKind, bytes: Vec<u8>) -> RawObject {
    RawObject {
        kind,
        oid: object_id(kind, &bytes),
        bytes,
    }
}

const BLOB_A: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"; // the empty blob
const ZERO: &str = "0000000000000000000000000000000000000000";

fn must_fail(head: &str, objects: &[RawObject], needle: &str) {
    let e = verify_head(head, objects, &VerifyLimits::default())
        .err()
        .unwrap_or_else(|| panic!("hostile objects were accepted (wanted `{needle}`)"))
        .to_string();
    assert!(e.contains(needle), "wanted `{needle}`, got: {e}");
}

/// A minimal valid graph: commit -> root tree -> {a (empty blob)}.
fn tiny() -> (RawObject, RawObject) {
    let tree = obj(GitObjectKind::Tree, tree_bytes(&[("100644", "a", BLOB_A)]));
    let commit = obj(GitObjectKind::Commit, commit_bytes(&tree.oid, &[]));
    (commit, tree)
}

// ================================================================================== acceptance

#[test]
fn an_honest_transport_is_accepted_and_agrees_with_the_independent_in_process_ingestion() {
    let Some(f) = fixture() else { return };
    let (got, stats) = receive(&f.records(), &StreamLimits::default()).unwrap();
    let Extracted::Git(g) = got else { panic!() };
    let o = &g.observation;
    let reference = &f.extracted.observation;
    // the head commit, parsed by the RECEIVER from raw bytes, equals what git reports
    let want = reference
        .commits
        .iter()
        .find(|c| c.oid == f.head())
        .unwrap();
    assert_eq!(&o.commits, std::slice::from_ref(want));
    assert_eq!(o.commits[0].parents, vec![f.parent.clone()]);
    // every tree of the closure equals the in-process parse of the same tree
    assert!(o.trees.len() >= 4, "root, src, src/inner, docs, vendor");
    for t in &o.trees {
        let r = reference.trees.iter().find(|r| r.oid == t.oid).unwrap();
        assert_eq!(t, r);
    }
    // the blobs: the same ids and sizes git reports (and only the ones HEAD's trees name)
    let mut blobs: Vec<(String, u64)> = o
        .blobs
        .iter()
        .map(|b| (b.oid.clone(), b.size_bytes))
        .collect();
    blobs.sort();
    let mut want_blobs: Vec<(String, u64)> = f.closure.blobs.clone();
    want_blobs.sort();
    assert_eq!(blobs, want_blobs);
    for (oid, size) in &blobs {
        let r = reference.blobs.iter().find(|b| &b.oid == oid).unwrap();
        assert_eq!(r.size_bytes, *size);
    }
    // derivations and code inputs are those of the in-process ingestion at HEAD
    assert_eq!(o.derivations.len(), reference.derivations.len());
    assert_eq!(g.code_inputs.len(), f.extracted.code_inputs.len());
    // what the observation says about scope: HEAD only
    assert_eq!(o.view_commits, vec![f.head().to_string()]);
    assert_eq!(o.current_commits, Some(vec![f.head().to_string()]));
    assert_eq!(o.current_tags, None);
    assert!(o.tags.is_empty() && o.ref_events.is_empty());
    assert_eq!(o.refs.len(), 1);
    assert_eq!(o.refs[0].name, "HEAD");
    assert_eq!(
        o.refs[0].symbolic_target.as_deref(),
        Some("refs/heads/main")
    );
    // what was verified, and how strongly
    let v = stats.verification.unwrap();
    assert_eq!(v.content_verified_objects, 1 + o.trees.len() as u64);
    assert_eq!(v.reference_verified_blobs, blobs.len() as u64);
    assert!(v.content_verified_blobs >= 4, "{v:?}");
    assert!(
        v.unverified_blobs >= 2,
        "binary, empty and symlink blobs: {v:?}"
    );
    assert_eq!(
        v.content_verified_blobs + v.unverified_blobs,
        v.reference_verified_blobs
    );
    assert_eq!(v.external_references, 2, "one parent and one gitlink");
}

#[test]
fn a_verified_transport_persists_the_same_code_structure_as_the_in_process_ingestion() {
    let Some(f) = fixture() else { return };
    let (got, _) = receive(&f.records(), &StreamLimits::default()).unwrap();
    let Extracted::Git(g) = got else { panic!() };
    // verified path -> database A; in-process (HEAD scope) -> database B
    let dir = tempfile::tempdir().unwrap();
    // both databases start as copies of the fixture's, so the space ids in the observations exist
    let mk = |name: &str| -> (PathBuf, SqliteStore) {
        let p = dir.path().join(name);
        rusqlite::Connection::open(&f.db_path)
            .unwrap()
            .execute("VACUUM INTO ?1", [p.to_str().unwrap()])
            .unwrap();
        let s = open_existing(&p).unwrap();
        (p, s)
    };
    let (pa, a) = mk("a.db");
    let (pb, b) = mk("b.db");
    let r0 = revisions(&pa);
    persist_git(&a, g, &Deadline::none()).unwrap();
    assert_eq!(
        revisions(&pa) - r0,
        1,
        "one logical ingestion, one revision"
    );
    persist_git(&b, f.extracted.clone(), &Deadline::none()).unwrap();
    let q = |p: &Path, sql: &str| -> Vec<String> {
        let c = rusqlite::Connection::open(p).unwrap();
        let mut v: Vec<String> = c
            .prepare(sql)
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .flatten()
            .collect();
        v.sort();
        v
    };
    for sql in [
        "SELECT kind||'|'||name||'|'||syntax_path FROM code_entities",
        "SELECT state FROM code_relationship_resolutions",
        "SELECT language||'|'||status||'|'||error_count FROM code_derivations",
        "SELECT path||'|'||ifnull(commit_oid,'') FROM code_contexts",
        "SELECT commit_oid FROM git_views",
        "SELECT content_hash FROM code_inputs",
    ] {
        assert_eq!(q(&pa, sql), q(&pb, sql), "{sql}");
    }
    // the verified path stored the HEAD commit's tree closure exactly
    let head_tree: Vec<String> = q(&pa, "SELECT tree_oid FROM git_commits");
    assert_eq!(head_tree.len(), 1);
    assert_eq!(a.git_refs_current(g_repo_id(&pa)).unwrap().len(), 1);
}

fn g_repo_id(p: &Path) -> Uuid {
    let c = rusqlite::Connection::open(p).unwrap();
    let id: String = c
        .query_row("SELECT repository_id FROM git_commits LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    Uuid::parse_str(&id).unwrap()
}

fn revisions(p: &Path) -> i64 {
    rusqlite::Connection::open(p)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM m6_revisions", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn the_receiver_hashes_like_git() {
    let Some(f) = fixture() else { return };
    // every object of the real repository: id recomputed here == the id git has
    for o in &f.closure.objects {
        assert_eq!(object_id(o.kind, &o.bytes), o.oid);
    }
    // and a spread of blob contents against `git hash-object`
    let samples: Vec<Vec<u8>> = vec![
        vec![],
        b"x".to_vec(),
        vec![0; 55],
        vec![b'a'; 56],
        vec![b'b'; 63],
        vec![0xff; 64],
        vec![b'c'; 65],
        (0..=255).collect(),
        (0..10_000u32).flat_map(|i| i.to_le_bytes()).collect(),
    ];
    for s in samples {
        let mut child = Command::new("git")
            .args(["hash-object", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), &s).unwrap();
        let out = child.wait_with_output().unwrap();
        let want = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(
            object_id(GitObjectKind::Blob, &s),
            want,
            "{} bytes",
            s.len()
        );
        verify_blob(&want, &s).unwrap();
        assert!(verify_blob(&want, b"other").is_err());
    }
}

// ================================================================================== adversarial

#[test]
fn a_wrong_object_id_is_refused() {
    let (commit, tree) = tiny();
    let mut forged = tree.clone();
    forged.oid = BLOB_A.into(); // claims to be another object
    must_fail(&commit.oid, &[commit.clone(), forged], "hashes to");
    // the commit under the id of its parent
    let mut c2 = commit.clone();
    c2.oid = "1".repeat(40);
    must_fail(&c2.oid.clone(), &[c2, tree], "hashes to");
}

#[test]
fn a_wrong_object_type_is_refused() {
    let (commit, tree) = tiny();
    // the same bytes announced as another type hash differently
    let mut as_tree = commit.clone();
    as_tree.kind = GitObjectKind::Tree;
    must_fail(&commit.oid, &[as_tree, tree.clone()], "hashes to");
    // blobs and tags do not belong to a HEAD transport at all
    let blob = obj(GitObjectKind::Blob, b"x".to_vec());
    must_fail(
        &commit.oid,
        &[commit.clone(), tree.clone(), blob],
        "not part of the HEAD",
    );
    let tag = obj(GitObjectKind::Tag, b"object x\n".to_vec());
    must_fail(
        &commit.oid,
        &[commit.clone(), tree.clone(), tag],
        "not part of the HEAD",
    );
    // the head id names a tree
    must_fail(&tree.oid, std::slice::from_ref(&tree), "not a commit");
}

#[test]
fn a_wrong_size_or_a_non_hex_body_is_refused_at_the_stream() {
    let Some(f) = fixture() else { return };
    let mut r = f.records();
    let i = r
        .iter()
        .position(|x| matches!(x, Record::RawObject { .. }))
        .unwrap();
    // declared size smaller than the content
    if let Record::RawObject { size, .. } = &mut r[i] {
        *size -= 1;
    }
    rejected(&r, "declared size");
    let mut r = f.records();
    if let Record::RawObject { hex, .. } = &mut r[i] {
        let n = hex.len();
        hex.replace_range(n - 2.., "zz");
    }
    rejected(&r, "not hexadecimal");
    // a size larger than the limit is refused before the body is decoded
    let mut r = f.records();
    if let Record::RawObject { size, hex, .. } = &mut r[i] {
        *size = 10_000_000;
        hex.clear();
    }
    rejected(&r, "exceeds the limit");
}

#[test]
fn malformed_tree_entries_are_refused() {
    // each tree is self-consistent (correct hash) and referenced by a well-formed commit
    let cases: Vec<(&str, Vec<u8>)> = vec![
        (
            "unsupported entry mode",
            tree_bytes(&[("100666", "a", BLOB_A)]),
        ),
        (
            "invalid entry name",
            tree_bytes(&[("100644", "a/b", BLOB_A)]),
        ),
        (
            "invalid entry name",
            tree_bytes(&[("100644", "..", BLOB_A)]),
        ),
        ("invalid entry name", tree_bytes(&[("100644", "", BLOB_A)])),
        ("`.git`", tree_bytes(&[("100644", ".GIT", BLOB_A)])),
        (
            "duplicate entry",
            tree_bytes(&[("100644", "a", BLOB_A), ("100644", "a", BLOB_A)]),
        ),
        // a repeat that is not a neighbour (the tree is not in git's order)
        (
            "duplicate entry",
            tree_bytes(&[
                ("100644", "a", BLOB_A),
                ("100644", "b", BLOB_A),
                ("100644", "a", BLOB_A),
            ]),
        ),
        ("truncated entry", {
            let mut b = tree_bytes(&[("100644", "a", BLOB_A)]);
            b.truncate(b.len() - 3);
            b
        }),
        ("mode terminator", b"100644".to_vec()),
        ("name terminator", b"100644 a".to_vec()),
    ];
    for (needle, bytes) in cases {
        let tree = obj(GitObjectKind::Tree, bytes);
        let commit = obj(GitObjectKind::Commit, commit_bytes(&tree.oid, &[]));
        must_fail(&commit.oid, &[commit.clone(), tree], needle);
    }
    // malformed commits
    let tree = obj(GitObjectKind::Tree, tree_bytes(&[("100644", "a", BLOB_A)]));
    let commits: Vec<(&str, Vec<u8>)> = vec![
        ("`tree`", b"parent x\n\nm\n".to_vec()),
        ("`tree`", format!("tree {}\n", "g".repeat(40)).into_bytes()),
        (
            "more than one `tree`",
            format!(
                "tree {0}\ntree {0}\nauthor a <a@b> 1 +0000\ncommitter a <a@b> 1 +0000\n\nm\n",
                tree.oid
            )
            .into_bytes(),
        ),
        (
            "malformed parent",
            format!(
                "tree {}\nparent xyz\nauthor a <a@b> 1 +0000\ncommitter a <a@b> 1 +0000\n\nm\n",
                tree.oid
            )
            .into_bytes(),
        ),
        (
            "missing `author`",
            format!("tree {}\ncommitter a <a@b> 1 +0000\n\nm\n", tree.oid).into_bytes(),
        ),
        (
            "`parent` after `author`",
            format!(
                "tree {}\nauthor a <a@b> 1 +0000\nparent {ZERO}\ncommitter a <a@b> 1 +0000\n\nm\n",
                tree.oid
            )
            .into_bytes(),
        ),
        ("not UTF-8", vec![0xff, 0xfe]),
    ];
    for (needle, bytes) in commits {
        let commit = obj(GitObjectKind::Commit, bytes);
        must_fail(&commit.oid, &[commit.clone(), tree.clone()], needle);
    }
}

#[test]
fn an_entry_whose_mode_contradicts_the_object_it_names_is_refused() {
    // a directory entry that names a tree id which was sent as a commit
    let sub = obj(GitObjectKind::Commit, commit_bytes(BLOB_A, &[]));
    let root = obj(GitObjectKind::Tree, tree_bytes(&[("40000", "d", &sub.oid)]));
    let head = obj(GitObjectKind::Commit, commit_bytes(&root.oid, &[]));
    // (two commit objects are only possible with a raised limit; with the default the second is
    // refused first, which is the stronger refusal)
    let mut l = VerifyLimits::default();
    l.max_commits = 2;
    let e = verify_head(&head.oid, &[head.clone(), root.clone(), sub.clone()], &l)
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("named as a tree"), "{e}");
    must_fail(&head.oid, &[head.clone(), root, sub], "commit object");
}

#[test]
fn a_wrong_tree_to_blob_binding_is_refused() {
    let Some(f) = fixture() else { return };
    // the derivation of one path claims another path's blob
    let mut r = f.records();
    let ds: Vec<usize> = r
        .iter()
        .enumerate()
        .filter(|(_, x)| matches!(x, Record::Derivation { .. }))
        .map(|(i, _)| i)
        .collect();
    assert!(ds.len() >= 2);
    if let Record::Derivation { file, .. } = &mut r[ds[0]] {
        file.path = "docs/not-in-the-tree.md".into();
    }
    rejected(&r, "does not have there");
    // a code input whose context path holds a different blob
    let mut r = f.records();
    let ci = r
        .iter()
        .position(|x| matches!(x, Record::CodeInput { .. }))
        .unwrap();
    if let Record::CodeInput { input, .. } = &mut r[ci] {
        input.contexts[0].path = "README.md".into();
    }
    rejected(&r, "does not have there");
    // content that does not hash to the blob id the tree names (a lie about WHICH blob it is)
    let mut r = f.records();
    let ci = r
        .iter()
        .position(|x| matches!(x, Record::CodeInput { .. }))
        .unwrap();
    if let Record::CodeInput { input, .. } = &mut r[ci] {
        input.blob_oid = Some(BLOB_A.into());
    }
    let e = receive(&r, &StreamLimits::default())
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("hashes to") || e.contains("names a blob"), "{e}");
}

#[test]
fn content_that_matches_its_own_sha256_but_not_the_blob_id_is_refused() {
    // The stream's own integrity check (SHA-256 of the content) can be satisfied by a worker that
    // changes the content and recomputes it. Only the Git blob id, which the VERIFIED tree names,
    // binds the content to the repository.
    let Some(f) = fixture() else { return };
    let mut r = f.records();
    let d = r
        .iter()
        .position(|x| matches!(x, Record::Derivation { .. }))
        .unwrap();
    let chunk = (d + 1..r.len())
        .find(|k| matches!(r[*k], Record::Chunk { .. }))
        .unwrap();
    let mut text = String::new();
    if let Record::Chunk { data } = &mut r[chunk] {
        data.push_str("\nSLIPPED IN");
        text = data.clone();
    }
    if let Record::Derivation { file, content } = &mut r[d] {
        file.content_hash = mindspace::core::sha256_hex(text.as_bytes());
        *content = mindspace::ingest::stream::ContentSource::Own {
            len: text.len() as u64,
        };
    }
    let e = receive(&r, &StreamLimits::default())
        .expect_err("changed content with a matching SHA-256 was accepted")
        .to_string();
    assert!(e.contains("hashes to"), "{e}");
}

#[test]
fn a_dangling_internal_reference_is_refused() {
    let Some(f) = fixture() else { return };
    // drop one subtree from the stream: the parent tree names it
    let mut r = f.records();
    let head_tree = f
        .closure
        .objects
        .iter()
        .find(|o| o.kind == GitObjectKind::Tree)
        .unwrap()
        .oid
        .clone();
    let victim = f
        .closure
        .objects
        .iter()
        .rev()
        .find(|o| o.kind == GitObjectKind::Tree && o.oid != head_tree)
        .unwrap()
        .oid
        .clone();
    r.retain(|x| !matches!(x, Record::RawObject { oid, .. } if *oid == victim));
    rejected(&r, "was not sent");
    // without the head commit
    let mut r = f.records();
    r.retain(|x| {
        !matches!(
            x,
            Record::RawObject {
                kind: GitObjectKind::Commit,
                ..
            }
        )
    });
    rejected(&r, "head commit was not sent");
}

#[test]
fn fabricated_parents_are_external_unverified_references_never_verified_history() {
    // a commit may name any parent id; it is kept as an unverified external reference
    let tree = obj(GitObjectKind::Tree, tree_bytes(&[("100644", "a", BLOB_A)]));
    let fake_parent = "ab".repeat(20);
    let commit = obj(
        GitObjectKind::Commit,
        commit_bytes(&tree.oid, &[&fake_parent]),
    );
    let v = verify_head(
        &commit.oid,
        &[commit.clone(), tree.clone()],
        &VerifyLimits::default(),
    )
    .unwrap();
    assert_eq!(v.external_parents, vec![fake_parent.clone()]);
    assert_eq!(v.summary.external_references, 1);
    assert_eq!(v.summary.content_verified_objects, 2);
    // but a parent commit OBJECT is not part of the transport, whatever it claims to be
    let parent = obj(GitObjectKind::Commit, commit_bytes(&tree.oid, &[]));
    let child = obj(
        GitObjectKind::Commit,
        commit_bytes(&tree.oid, &[&parent.oid]),
    );
    must_fail(
        &child.oid,
        &[child.clone(), parent, tree.clone()],
        "commit object",
    );
    // a malformed parent id
    let bad = obj(
        GitObjectKind::Commit,
        format!(
            "tree {}\nparent {}\nauthor a <a@b> 1 +0000\ncommitter a <a@b> 1 +0000\n\nm\n",
            tree.oid,
            "G".repeat(40)
        )
        .into_bytes(),
    );
    must_fail(&bad.oid, &[bad.clone(), tree], "malformed parent");
}

#[test]
fn objects_outside_the_head_closure_are_refused() {
    let (commit, tree) = tiny();
    // a valid tree that nothing names
    let stray = obj(GitObjectKind::Tree, tree_bytes(&[("100644", "z", BLOB_A)]));
    must_fail(
        &commit.oid,
        &[commit.clone(), tree.clone(), stray],
        "not reachable",
    );
    // a second, unrelated commit
    let other = obj(GitObjectKind::Commit, commit_bytes(&tree.oid, &[ZERO]));
    must_fail(
        &commit.oid,
        &[commit.clone(), tree.clone(), other],
        "commit object",
    );
    // at the stream: an extra blob row, and a blob row for something no tree names
    let Some(f) = fixture() else { return };
    let mut r = f.records();
    let bi = r
        .iter()
        .position(|x| matches!(x, Record::Blob { .. }))
        .unwrap();
    let Record::Blob { item } = r[bi].clone() else {
        panic!()
    };
    let mut extra = item;
    extra.oid = "cd".repeat(20);
    r.insert(bi, Record::Blob { item: extra });
    rejected(&r, "not named by any verified tree");
    // a missing blob row
    let mut r = f.records();
    r.remove(bi);
    rejected(&r, "blob record(s)");
}

/// What verification does NOT establish, pinned so that the claim cannot drift: a history that
/// never existed in the authorized repository, with correct hashes, is accepted. Hash consistency
/// (the bytes are what the ids say), reachability (everything hangs off the reported HEAD) and
/// the authenticity of that HEAD (it is the repository's real HEAD) are three different things;
/// only the first two are checked here.
#[test]
fn a_self_consistent_invented_history_is_accepted_and_claims_nothing_about_authenticity() {
    let blob = b"invented\n";
    let blob_id = object_id(GitObjectKind::Blob, blob);
    let tree = obj(
        GitObjectKind::Tree,
        tree_bytes(&[("100644", "x.txt", &blob_id)]),
    );
    let commit = obj(
        GitObjectKind::Commit,
        commit_bytes(&tree.oid, &[&"cd".repeat(20)]),
    );
    let v = verify_head(
        &commit.oid,
        &[commit.clone(), tree],
        &VerifyLimits::default(),
    )
    .unwrap();
    // it is internally consistent and reachable ...
    assert_eq!(v.summary.content_verified_objects, 2);
    assert_eq!(v.summary.reference_verified_blobs, 1);
    // ... and the blob's content, if it travels, is bound to the id the verified tree names ...
    verify_blob(&blob_id, blob).unwrap();
    assert!(verify_blob(&blob_id, b"other").is_err());
    // ... but nothing here knows whether this repository ever had that commit: the parent is an
    // unverified external reference, and the head id was supplied by the sender.
    assert_eq!(v.summary.external_references, 1);
    assert_eq!(v.commit.oid, commit.oid);
}

#[test]
fn conflicting_duplicate_ids_are_refused() {
    let (commit, tree) = tiny();
    // the same id twice, with the same bytes or different ones
    must_fail(
        &commit.oid,
        &[commit.clone(), tree.clone(), tree.clone()],
        "duplicate object",
    );
    let mut forged = tree.clone();
    forged.bytes = tree_bytes(&[("100644", "evil", BLOB_A)]);
    must_fail(
        &commit.oid,
        &[commit.clone(), tree.clone(), forged],
        "hashes to",
    );
    // at the stream
    let Some(f) = fixture() else { return };
    let mut r = f.records();
    let i = r
        .iter()
        .position(|x| {
            matches!(
                x,
                Record::RawObject {
                    kind: GitObjectKind::Tree,
                    ..
                }
            )
        })
        .unwrap();
    let dup = r[i].clone();
    r.insert(i, dup);
    rejected(&r, "duplicate object");
}

#[test]
fn cycles_and_unbounded_expansion_are_refused() {
    // a tree cannot contain its own id (the id is the hash of the bytes), so a "cycle" is a
    // claimed id that does not match
    let id = "ee".repeat(20);
    let looping = RawObject {
        kind: GitObjectKind::Tree,
        oid: id.clone(),
        bytes: tree_bytes(&[("40000", "self", &id)]),
    };
    let head = obj(GitObjectKind::Commit, commit_bytes(&id, &[]));
    must_fail(&head.oid, &[head.clone(), looping], "hashes to");
    // depth: a chain one deeper than the limit
    let mut limits = VerifyLimits::default();
    limits.max_depth = 5;
    let chain = |n: usize| -> Vec<RawObject> {
        let mut objs: Vec<RawObject> = Vec::new();
        let mut child = obj(GitObjectKind::Tree, tree_bytes(&[("100644", "f", BLOB_A)]));
        objs.push(child.clone());
        for _ in 0..n {
            child = obj(
                GitObjectKind::Tree,
                tree_bytes(&[("40000", "d", &child.oid)]),
            );
            objs.push(child.clone());
        }
        let head = obj(GitObjectKind::Commit, commit_bytes(&child.oid, &[]));
        objs.push(head);
        objs
    };
    let at = chain(5); // root at depth 0, leaf at depth 5: allowed
    let head = at.last().unwrap().oid.clone();
    verify_head(&head, &at, &limits).unwrap();
    let over = chain(6);
    let head = over.last().unwrap().oid.clone();
    let e = verify_head(&head, &over, &limits)
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("nested deeper"), "{e}");
    // expansion: each level names the level below twice, so the closure has 2^n paths with n+1
    // distinct objects; the work limit stops the walk
    let mut objs: Vec<RawObject> = Vec::new();
    let mut child = obj(GitObjectKind::Tree, tree_bytes(&[("100644", "f", BLOB_A)]));
    objs.push(child.clone());
    for _ in 0..30 {
        child = obj(
            GitObjectKind::Tree,
            tree_bytes(&[("40000", "a", &child.oid), ("40000", "b", &child.oid)]),
        );
        objs.push(child.clone());
    }
    let head = obj(GitObjectKind::Commit, commit_bytes(&child.oid, &[]));
    objs.push(head.clone());
    let mut limits = VerifyLimits::default();
    limits.max_depth = 64;
    limits.max_work = 5_000;
    let e = verify_head(&head.oid, &objs, &limits)
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("entry visits"), "{e}");
}

#[test]
fn sha256_ids_are_refused_everywhere_in_the_transport() {
    let sha256 = "a".repeat(64);
    let e = verify_head(&sha256, &[], &VerifyLimits::default())
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("malformed head id"), "{e}");
    let (commit, tree) = tiny();
    let mut long = tree.clone();
    long.oid = sha256.clone();
    must_fail(&commit.oid, &[commit.clone(), long], "malformed object id");
    // a 64-digit entry id cannot even be expressed in a SHA-1 tree: the bytes would not parse
    let Some(f) = fixture() else { return };
    let mut r = f.records();
    for x in r.iter_mut() {
        if let Record::Head { commit_oid, .. } = x {
            *commit_oid = sha256.clone();
        }
    }
    rejected(&r, "malformed head id");
}

#[test]
fn unverified_record_families_and_misplaced_records_are_refused() {
    let Some(f) = fixture() else { return };
    // parsed-object records have no place in a verified transport
    let reference = &f.extracted.observation;
    for extra in [
        Record::Commit {
            item: reference.commits[0].clone(),
        },
        Record::Tree {
            item: reference.trees[0].clone(),
        },
        Record::Ref {
            item: reference.refs[0].clone(),
        },
    ] {
        let mut r = f.records();
        let at = r
            .iter()
            .position(|x| matches!(x, Record::RawObject { .. }))
            .unwrap();
        r.insert(at, extra);
        rejected(&r, "verified Git transport");
    }
    // raw objects outside a verified transport, and a second head
    let mut r = f.records();
    let i = r
        .iter()
        .position(|x| matches!(x, Record::Head { .. }))
        .unwrap();
    let dup = r[i].clone();
    r.insert(i + 1, dup);
    rejected(&r, "second `head`");
    let mut r = f.records();
    let h = r
        .iter()
        .position(|x| matches!(x, Record::Head { .. }))
        .unwrap();
    r.remove(h);
    rejected(&r, "before the `head` record");
    // a raw_object announced in an ordinary Git operation
    let op = Uuid::now_v7();
    let mut r = f.records();
    if let Record::Begin { kind } = &mut r[0] {
        *kind = Kind::Git;
    }
    let bytes = encode(op, &r);
    let e = consume(
        Cursor::new(bytes),
        op,
        Kind::Git,
        None,
        &StreamLimits::default(),
        None,
    )
    .err()
    .unwrap()
    .to_string();
    assert!(e.contains("outside a verified"), "{e}");
}

// ============================================================================ placeholders

#[test]
fn a_reused_placeholder_needs_content_verified_in_the_same_stream() {
    let Some(f) = fixture() else { return };
    let rust = f
        .extracted
        .code_inputs
        .iter()
        .position(|c| !c.derivation.entities.is_empty())
        .unwrap();
    // honest: a placeholder (no parts) whose content still travels and hashes to its blob id
    let r = f.records_with_placeholder(rust);
    assert!(r
        .iter()
        .any(|x| matches!(x, Record::CodeInput { reused: true, .. })));
    let (got, _) = receive(&r, &StreamLimits::default()).unwrap();
    let Extracted::Git(g) = got else { panic!() };
    assert!(g.code_inputs[rust].derivation.entities.is_empty());

    // the same marker in an ordinary Git operation stays refused
    let op = Uuid::now_v7();
    let mut plain = f.records_with_placeholder(rust);
    if let Record::Begin { kind } = &mut plain[0] {
        *kind = Kind::Git;
    }
    let e = consume(
        Cursor::new(encode(op, &plain)),
        op,
        Kind::Git,
        None,
        &StreamLimits::default(),
        None,
    )
    .err()
    .unwrap()
    .to_string();
    assert!(
        e.contains("outside a verified") || e.contains("reused derivation"),
        "{e}"
    );

    // content that is not the blob: one byte appended to the bytes the placeholder rides on
    let mut r = f.records_with_placeholder(rust);
    let ci = r
        .iter()
        .position(|x| matches!(x, Record::CodeInput { .. }))
        .unwrap();
    let chunk = (ci..r.len())
        .find(|k| matches!(r[*k], Record::Chunk { .. }))
        .unwrap();
    if let Record::Chunk { data } = &mut r[chunk] {
        data.pop();
        data.push('#');
    }
    let e = receive(&r, &StreamLimits::default())
        .err()
        .unwrap()
        .to_string();
    assert!(!e.is_empty(), "tampered placeholder content accepted");

    // a placeholder for a blob no verified tree names, and one at a path holding another blob
    let mut r = f.records_with_placeholder(rust);
    let ci = r
        .iter()
        .position(|x| matches!(x, Record::CodeInput { .. }))
        .unwrap();
    if let Record::CodeInput { input, .. } = &mut r[ci] {
        input.blob_oid = Some("ab".repeat(20));
    }
    assert!(receive(&r, &StreamLimits::default()).is_err());
    let mut r = f.records_with_placeholder(rust);
    let ci = r
        .iter()
        .position(|x| matches!(x, Record::CodeInput { .. }))
        .unwrap();
    if let Record::CodeInput { input, .. } = &mut r[ci] {
        input.contexts[0].path = "src/util.rs".into(); // another blob lives there
    }
    let e = receive(&r, &StreamLimits::default())
        .err()
        .unwrap()
        .to_string();
    assert!(
        e.contains("does not have there") || e.contains("hashes to"),
        "{e}"
    );

    // a placeholder is not verified because a stored object with that id exists: the receiver has
    // no store to ask, and an id with no matching content in this stream is refused
    let mut r = f.records_with_placeholder(rust);
    let ci = r
        .iter()
        .position(|x| matches!(x, Record::CodeInput { .. }))
        .unwrap();
    if let Record::CodeInput { content, .. } = &mut r[ci] {
        *content = mindspace::ingest::stream::ContentSource::Shared;
    }
    assert!(receive(&r, &StreamLimits::default()).is_err());
}

#[test]
fn unverified_blob_sizes_stay_labelled_unverified_and_content_sizes_are_checked() {
    let Some(f) = fixture() else { return };
    let (_, stats) = receive(&f.records(), &StreamLimits::default()).unwrap();
    let v = stats.verification.unwrap();
    assert!(v.unverified_blobs >= 1 && v.content_verified_blobs >= 1);
    // a size claim for a blob whose content travelled must equal the content
    let mut r = f.records();
    let bi = r
        .iter()
        .enumerate()
        .filter_map(|(i, x)| match x {
            Record::Blob { item } if item.size_bytes > 0 => Some(i),
            _ => None,
        })
        .find(|i| {
            // a blob with a derivation: its oid is a derivation's
            let Record::Blob { item } = &r[*i] else {
                return false;
            };
            r.iter()
                .any(|x| matches!(x, Record::Derivation { file, .. } if file.blob_oid == item.oid))
        })
        .unwrap();
    if let Record::Blob { item } = &mut r[bi] {
        item.size_bytes += 1;
    }
    rejected(&r, "size recorded");
}

// ================================================================================= limits

#[test]
fn limits_are_enforced_at_the_exact_boundary_and_one_over() {
    let (commit, tree) = tiny();
    let base = VerifyLimits::default();
    let ok = |l: &VerifyLimits| verify_head(&commit.oid, &[commit.clone(), tree.clone()], l);
    ok(&base).unwrap();
    // raw object size: the tree is N bytes
    let n = tree.bytes.len();
    let mut l = base;
    l.max_raw_object_bytes = n.max(commit.bytes.len());
    ok(&l).unwrap();
    l.max_raw_object_bytes = n.max(commit.bytes.len()) - 1;
    assert!(ok(&l).err().unwrap().to_string().contains("limit"));
    // total bytes
    let mut l = base;
    l.max_total_object_bytes = (n + commit.bytes.len()) as u64;
    ok(&l).unwrap();
    l.max_total_object_bytes -= 1;
    assert!(ok(&l)
        .err()
        .unwrap()
        .to_string()
        .contains("bytes of objects"));
    // commits: one is allowed, two are not
    let mut l = base;
    l.max_commits = 0;
    assert!(ok(&l).err().unwrap().to_string().contains("commit object"));
    // trees
    let mut l = base;
    l.max_trees = 1;
    ok(&l).unwrap();
    l.max_trees = 0;
    assert!(ok(&l).err().unwrap().to_string().contains("tree objects"));
    // entries of one tree and in all
    let mut l = base;
    l.max_tree_entries = 1;
    ok(&l).unwrap();
    l.max_tree_entries = 0;
    assert!(ok(&l).err().unwrap().to_string().contains("entries"));
    // work
    let mut l = base;
    l.max_work = 1;
    ok(&l).unwrap();
    l.max_work = 0;
    assert!(ok(&l).err().unwrap().to_string().contains("entry visits"));
    // path length: the path is "a" (1 byte)
    let mut l = base;
    l.max_path_bytes = 1;
    ok(&l).unwrap();
    l.max_path_bytes = 0;
    assert!(ok(&l).err().unwrap().to_string().contains("path"));
    // depth
    let mut l = base;
    l.max_depth = 0;
    ok(&l).unwrap();
}

#[test]
fn stream_limits_apply_at_record_acceptance() {
    let Some(f) = fixture() else { return };
    let r = f.records();
    let blobs = r
        .iter()
        .filter(|x| matches!(x, Record::Blob { .. }))
        .count();
    let trees = f
        .closure
        .objects
        .iter()
        .filter(|o| o.kind == GitObjectKind::Tree)
        .count();
    let mut l = StreamLimits::default();
    l.verify.max_blobs = blobs;
    l.verify.max_trees = trees;
    receive(&r, &l).unwrap();
    l.verify.max_blobs = blobs - 1;
    let e = receive(&r, &l).err().unwrap().to_string();
    assert!(e.contains("blob records"), "{e}");
    l.verify.max_blobs = blobs;
    l.verify.max_trees = trees - 1;
    let e = receive(&r, &l).err().unwrap().to_string();
    assert!(e.contains("tree objects"), "{e}");
    // the largest raw object of the fixture: the exact size is accepted, one under it is not
    let biggest = f
        .closure
        .objects
        .iter()
        .map(|o| o.bytes.len())
        .max()
        .unwrap();
    let mut l = StreamLimits::default();
    l.verify.max_raw_object_bytes = biggest;
    receive(&r, &l).unwrap();
    l.verify.max_raw_object_bytes = biggest - 1;
    let e = receive(&r, &l).err().unwrap().to_string();
    assert!(e.contains("exceeds the limit"), "{e}");
    // total object bytes
    let total: u64 = f.closure.objects.iter().map(|o| o.bytes.len() as u64).sum();
    let mut l = StreamLimits::default();
    l.verify.max_total_object_bytes = total;
    receive(&r, &l).unwrap();
    l.verify.max_total_object_bytes = total - 1;
    let e = receive(&r, &l).err().unwrap().to_string();
    assert!(e.contains("bytes of objects"), "{e}");
}

#[test]
fn the_producer_stops_early_at_its_limits() {
    let Some(f) = fixture() else { return };
    use std::os::fd::AsFd;
    let file = std::fs::File::open(&f.repo).unwrap();
    let exe = GitExecutable::resolve(Path::new(GIT)).unwrap();
    let reader = ConfinedGitReader::open(file.as_fd(), &exe, Deadline::none()).unwrap();
    let mut l = VerifyLimits::default();
    l.max_trees = 2;
    let e = reader.head_closure(&l).err().unwrap().to_string();
    assert!(e.contains("too large"), "{e}");
    let mut l = VerifyLimits::default();
    l.max_tree_entries = 3;
    assert!(reader.head_closure(&l).is_err());
    let mut l = VerifyLimits::default();
    l.max_depth = 1;
    assert!(reader.head_closure(&l).is_err());
    let mut l = VerifyLimits::default();
    l.max_blobs = 2;
    assert!(reader.head_closure(&l).is_err());
    let mut l = VerifyLimits::default();
    l.max_raw_object_bytes = 10;
    assert!(reader.head_closure(&l).is_err());
    reader.head_closure(&VerifyLimits::default()).unwrap();
}

// ============================================================ interruption and persistence

#[test]
fn truncated_and_interrupted_streams_are_refused() {
    let Some(f) = fixture() else { return };
    let op = Uuid::now_v7();
    let bytes = encode(op, &f.records());
    // cut at every frame boundary and in the middle of frames
    let mut cuts: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        cuts.push(i);
        cuts.push(i + 2);
        let len = u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
        cuts.push(i + 4 + len / 2);
        i += 4 + len;
    }
    // and every ~2% of the stream
    cuts.extend((1..50).map(|k| bytes.len() * k / 50));
    cuts.retain(|c| *c < bytes.len());
    assert!(cuts.len() >= 40);
    for cut in cuts {
        let r = consume(
            Cursor::new(bytes[..cut].to_vec()),
            op,
            Kind::GitVerified,
            None,
            &StreamLimits::default(),
            None,
        );
        assert!(
            r.is_err(),
            "a stream cut at {cut} of {} was accepted",
            bytes.len()
        );
    }
    // the whole stream still works
    consume(
        Cursor::new(bytes),
        op,
        Kind::GitVerified,
        None,
        &StreamLimits::default(),
        None,
    )
    .unwrap();
}

#[test]
fn a_rejected_transport_stores_nothing_and_an_accepted_one_is_one_revision() {
    let Some(f) = fixture() else { return };
    let dump = |p: &Path| -> BTreeMap<String, i64> {
        let c = rusqlite::Connection::open(p).unwrap();
        let tables: Vec<String> = c
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect();
        tables
            .into_iter()
            .map(|t| {
                let n: i64 = c
                    .query_row(&format!("SELECT COUNT(*) FROM \"{t}\""), [], |r| r.get(0))
                    .unwrap();
                (t, n)
            })
            .collect()
    };
    let before = dump(&f.db_path);
    // every kind of hostile stream: nothing reaches the store, because nothing is returned
    let mut hostile: Vec<Vec<Record>> = Vec::new();
    let mut r = f.records();
    let i = r
        .iter()
        .position(|x| matches!(x, Record::RawObject { .. }))
        .unwrap();
    if let Record::RawObject { oid, .. } = &mut r[i] {
        *oid = "ff".repeat(20);
    }
    hostile.push(r);
    let mut r = f.records();
    r.retain(|x| {
        !matches!(
            x,
            Record::RawObject {
                kind: GitObjectKind::Commit,
                ..
            }
        )
    });
    hostile.push(r);
    for r in hostile {
        let Ok(_) = receive(&r, &StreamLimits::default()) else {
            continue;
        };
        panic!("a hostile stream was accepted");
    }
    assert_eq!(before, dump(&f.db_path));
    // an accepted one persists atomically, as one revision, and again changes no derived row
    let (got, _) = receive(&f.records(), &StreamLimits::default()).unwrap();
    let Extracted::Git(g) = got else { panic!() };
    let r0 = revisions(&f.db_path);
    persist_git(&f.store(), g.clone(), &Deadline::none()).unwrap();
    assert_eq!(revisions(&f.db_path) - r0, 1);
    let once = dump(&f.db_path);
    persist_git(&f.store(), g, &Deadline::none()).unwrap();
    let twice = dump(&f.db_path);
    for t in [
        "code_inputs",
        "code_derivations",
        "code_entities",
        "git_commits",
        "git_trees",
    ] {
        assert_eq!(once[t], twice[t], "{t}");
    }
    let _ = hex_encode(&[0]);
}
