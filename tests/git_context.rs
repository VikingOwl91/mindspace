//! M7.3b.5d: the Git context exchange and the provenance of reuse placeholders.
//!
//! The parent announces which `(blob id, parser identity)` pairs already have a derivation; a
//! worker may send an explicit placeholder only for a pair this operation announced. These tests
//! build honest verified results and then forge, replay and age the context around them. They
//! need neither cgroups nor a worker process.

#![allow(clippy::field_reassign_with_default)]

use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use mindspace::core::{CodeLanguage, CodeParserIdentity, Result, Uuid};
use mindspace::ingest::code::{self, ParsePolicy};
use mindspace::ingest::confined_git::{ConfinedGitReader, GitExecutable};
use mindspace::ingest::deadline::Deadline;
use mindspace::ingest::extract::{Extracted, ExtractedGit, StoreContext};
use mindspace::ingest::gitverify::VerifyLimits;
use mindspace::ingest::outline::PathSelection;
use mindspace::ingest::stream::{consume, emit_verified, FrameWriter, Kind, Record, StreamLimits};
use mindspace::ingest::{extract_git_repository_selected_with_deadline, GitScope};
use mindspace::worker::context::{
    answer_git, check_result_git, identities_for, ContextSource, GitContext, GitManifest,
    GitManifestEntry,
};
use mindspace::worker::PROTOCOL;
use mindspace::{initialize, open_existing};

const GIT: &str = "/usr/bin/git";

struct Fx {
    _t: tempfile::TempDir,
    g: ExtractedGit,
    op: Uuid,
    repo_id: Uuid,
    space: Uuid,
    head: String,
    /// blob ids of the Rust sources (the code inputs), in code-input order
    blobs: Vec<String>,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new(GIT)
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@e",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// An honest verified result in which EVERY code input is an announced placeholder, with the
/// context that announced them.
fn fixture() -> Option<(Fx, GitContext)> {
    if !Path::new(GIT).exists() {
        return None;
    }
    let t = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(t.path()).unwrap();
    let repo = base.join("r");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("src/lib.rs"), "pub mod util;\npub fn run() {}\n").unwrap();
    std::fs::write(repo.join("src/util.rs"), "pub fn helper() {}\n").unwrap();
    std::fs::write(repo.join("README.md"), "# R\n\nalpha\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "one"]);
    let dbp = base.join("m.db");
    initialize(&dbp).unwrap();
    let store = open_existing(&dbp).unwrap();
    mindspace::storage::Store::create_space(&store, "p", "").unwrap();
    let space = mindspace::storage::Store::get_space(&store, "p")
        .unwrap()
        .id;
    let extracted = extract_git_repository_selected_with_deadline(
        &StoreContext(&store),
        &repo,
        &["p"],
        &PathSelection::all(),
        GitScope::Head,
        &Deadline::none(),
    )
    .unwrap();
    let repo_id = extracted.observation.repository_id;
    let closure = {
        use std::os::fd::AsFd;
        let f = std::fs::File::open(&repo).unwrap();
        let exe = GitExecutable::resolve(Path::new(GIT)).unwrap();
        ConfinedGitReader::open(f.as_fd(), &exe, Deadline::none())
            .unwrap()
            .head_closure(&VerifyLimits::default())
            .unwrap()
    };
    let head = closure.commit_oid.clone();
    let blobs: Vec<String> = extracted
        .code_inputs
        .iter()
        .map(|c| c.blob_oid.clone().unwrap())
        .collect();
    let all: BTreeSet<usize> = (0..extracted.code_inputs.len()).collect();
    struct Collect(Vec<Record>);
    impl mindspace::ingest::stream::RecordSink for Collect {
        fn emit_encoded(&mut self, _: &'static str, json: Vec<u8>) -> Result<()> {
            self.0.push(serde_json::from_slice(&json).unwrap());
            Ok(())
        }
    }
    let mut c = Collect(Vec::new());
    emit_verified(&mut c, extracted, &closure, &all).unwrap();
    let op = Uuid::now_v7();
    let mut bytes = Vec::new();
    {
        let mut w = FrameWriter::new(&mut bytes, op, None);
        for r in &c.0 {
            if !matches!(r, Record::End { .. }) {
                mindspace::ingest::stream::RecordSink::emit(&mut w, r).unwrap();
            }
        }
        w.finish().unwrap();
    }
    let (got, _) = consume(
        Cursor::new(bytes),
        op,
        Kind::GitVerified,
        None,
        &StreamLimits::default(),
        None,
    )
    .unwrap();
    let Extracted::Git(g) = got else { panic!() };
    assert_eq!(g.reused.len(), blobs.len());
    let ctx = GitContext {
        v: PROTOCOL,
        op,
        repository_id: repo_id,
        head: head.clone(),
        identities: identities_for(&ParsePolicy::unlimited()),
        known: blobs.iter().map(|b| (b.clone(), 0u8)).collect(),
    };
    Some((
        Fx {
            _t: t,
            g,
            op,
            repo_id,
            space,
            head,
            blobs,
        },
        ctx,
    ))
}

fn policy() -> ParsePolicy {
    ParsePolicy::unlimited()
}

fn check(f: &Fx, g: &ExtractedGit, ctx: &GitContext) -> std::result::Result<(), String> {
    check_result_git(g, f.op, f.repo_id, &[f.space], &policy(), ctx)
}

// ------------------------------------------------------------------------- honest and forged

#[test]
fn an_announced_placeholder_is_accepted_and_every_forgery_of_its_provenance_is_not() {
    let Some((f, ctx)) = fixture() else { return };
    check(&f, &f.g, &ctx).unwrap();

    // nothing announced: every placeholder is a forgery
    let mut none = ctx.clone();
    none.known.clear();
    assert!(check(&f, &f.g, &none)
        .unwrap_err()
        .contains("did not announce"));

    // only one announced: the other placeholder is refused
    let mut one = ctx.clone();
    one.known.truncate(1);
    assert!(check(&f, &f.g, &one)
        .unwrap_err()
        .contains("did not announce"));

    // announced under another identity (an older extractor): not the pair the placeholder claims
    let mut stale = ctx.clone();
    let mut old: CodeParserIdentity = code::identity();
    old.extractor_version = "3".into();
    stale.identities = vec![old];
    assert!(
        check(&f, &f.g, &stale).is_err(),
        "a @3 announcement covered a @4 placeholder"
    );

    // a placeholder that carries structure is not a placeholder
    let mut carrying = f.g.clone();
    let i = *carrying.reused.iter().next().unwrap();
    carrying.code_inputs[i].derivation.entities = vec![mindspace::core::CodeEntityDraft {
        kind: mindspace::core::CodeEntityKind::Function,
        name: Some("x".into()),
        qualified_name: None,
        signature: None,
        syntax_path: "function:x@0".into(),
        parent_syntax_path: None,
        range: mindspace::core::CodeRange {
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 1,
            start_column: 0,
            end_column: 1,
        },
        visibility: None,
        is_async: false,
    }];
    assert!(check(&f, &carrying, &ctx)
        .unwrap_err()
        .contains("carries structure"));

    // a fresh derivation for a blob that WAS announced (the worker re-sent what it was told it
    // did not have to send, with different content)
    let mut fresh = f.g.clone();
    fresh.reused.clear();
    assert!(check(&f, &fresh, &ctx)
        .unwrap_err()
        .contains("announced as known"));

    // a reuse declaration that points nowhere
    let mut dangling = f.g.clone();
    dangling.reused.insert(99);
    assert!(check(&f, &dangling, &ctx)
        .unwrap_err()
        .contains("no code input"));
}

#[test]
fn the_context_is_bound_to_the_operation_the_repository_the_head_and_the_spaces() {
    let Some((f, ctx)) = fixture() else { return };
    // another operation's context, replayed
    let mut other_op = ctx.clone();
    other_op.op = Uuid::now_v7();
    assert!(check(&f, &f.g, &other_op)
        .unwrap_err()
        .contains("another operation"));
    // another repository's context
    let mut other_repo = ctx.clone();
    other_repo.repository_id = Uuid::now_v7();
    assert!(check(&f, &f.g, &other_repo)
        .unwrap_err()
        .contains("another repository"));
    // a result that names another repository than the broker bound
    let mut lying = f.g.clone();
    lying.observation.repository_id = Uuid::now_v7();
    assert!(check(&f, &lying, &ctx)
        .unwrap_err()
        .contains("another repository"));
    for c in lying.code_inputs.iter_mut() {
        c.repository_id = Some(lying.observation.repository_id);
    }
    assert!(check(&f, &lying, &ctx)
        .unwrap_err()
        .contains("another repository"));
    // other spaces
    assert!(
        check_result_git(&f.g, f.op, f.repo_id, &[Uuid::now_v7()], &policy(), &ctx)
            .unwrap_err()
            .contains("other spaces")
    );
    // another head than the manifest named
    let mut other_head = ctx.clone();
    other_head.head = "ab".repeat(20);
    assert!(check(&f, &f.g, &other_head)
        .unwrap_err()
        .contains("another head"));
    // a code input of another repository
    let mut foreign = f.g.clone();
    foreign.code_inputs[0].repository_id = Some(Uuid::now_v7());
    assert!(check(&f, &foreign, &ctx)
        .unwrap_err()
        .contains("another repository"));
}

// -------------------------------------------------------------------- the two messages

fn entry(blob: &str) -> GitManifestEntry {
    GitManifestEntry {
        language: CodeLanguage::Rust,
        blob_oid: blob.into(),
    }
}

#[test]
fn manifests_and_contexts_are_validated_strictly_on_both_sides() {
    let Some((f, ctx)) = fixture() else { return };
    let m = GitManifest {
        v: PROTOCOL,
        op: f.op,
        repository_id: f.repo_id,
        head: f.head.clone(),
        entries: f.blobs.iter().map(|b| entry(b)).collect(),
    };
    m.validate(f.op, f.repo_id).unwrap();
    // the broker's identity decides, not the manifest's
    assert!(m
        .validate(f.op, Uuid::now_v7())
        .unwrap_err()
        .contains("another repository"));
    assert!(m
        .validate(Uuid::now_v7(), f.repo_id)
        .unwrap_err()
        .contains("another operation"));
    let mut bad = m.clone();
    bad.head = "xyz".into();
    assert!(bad.validate(f.op, f.repo_id).is_err());
    let mut bad = m.clone();
    bad.entries.push(entry(&f.blobs[0]));
    assert!(bad
        .validate(f.op, f.repo_id)
        .unwrap_err()
        .contains("repeated"));
    let mut bad = m.clone();
    bad.entries.push(entry(&"A".repeat(40)));
    assert!(bad.validate(f.op, f.repo_id).is_err());
    let mut bad = m.clone();
    bad.entries.push(entry(&"a".repeat(64))); // a SHA-256 id
    assert!(bad.validate(f.op, f.repo_id).is_err());
    // unknown fields are refused
    let mut v = serde_json::to_value(&m).unwrap();
    v["extra"] = 1.into();
    assert!(serde_json::from_value::<GitManifest>(v).is_err());

    // the worker's view of the context
    ctx.validate(f.op, f.repo_id, &f.head, &policy()).unwrap();
    assert!(ctx
        .validate(Uuid::now_v7(), f.repo_id, &f.head, &policy())
        .is_err());
    assert!(ctx
        .validate(f.op, Uuid::now_v7(), &f.head, &policy())
        .is_err());
    assert!(ctx
        .validate(f.op, f.repo_id, &"cd".repeat(20), &policy())
        .is_err());
    let mut bad = ctx.clone();
    bad.known.push(bad.known[0].clone());
    assert!(bad
        .validate(f.op, f.repo_id, &f.head, &policy())
        .unwrap_err()
        .contains("repeated"));
    let mut bad = ctx.clone();
    bad.known.push((f.blobs[0].clone(), 9));
    assert!(bad
        .validate(f.op, f.repo_id, &f.head, &policy())
        .unwrap_err()
        .contains("missing identity"));
    // an identity this operation cannot produce (a stale extractor version)
    let mut bad = ctx.clone();
    let mut old = code::identity();
    old.extractor_version = "3".into();
    bad.identities[0] = old; // replaced: the list is already at its maximum
    let e = bad
        .validate(f.op, f.repo_id, &f.head, &policy())
        .unwrap_err();
    assert!(e.contains("cannot ask about"), "{e}");
}

/// A source that "knows" a fixed set, to see what the parent announces.
struct Knows(Vec<(Uuid, String, CodeParserIdentity)>);

impl ContextSource for Knows {
    fn space_exists(&self, _: Uuid) -> Result<bool> {
        Ok(true)
    }
    fn derivation_known(&self, _: CodeLanguage, _: &str, _: &CodeParserIdentity) -> Result<bool> {
        Ok(false)
    }
    fn git_derivation_known(
        &self,
        _: CodeLanguage,
        repo: Uuid,
        blob: &str,
        id: &CodeParserIdentity,
    ) -> Result<bool> {
        Ok(self
            .0
            .iter()
            .any(|(r, b, i)| *r == repo && b == blob && i == id))
    }
}

#[test]
fn the_parent_announces_only_what_it_has_for_this_repository_and_this_extractor() {
    let Some((f, _)) = fixture() else { return };
    let now = code::identity();
    let mut old = now.clone();
    old.extractor_version = "3".into();
    let other_repo = Uuid::now_v7();
    let src = Knows(vec![
        (f.repo_id, f.blobs[0].clone(), now.clone()), // announced
        (f.repo_id, f.blobs[1].clone(), old), // an older extractor's: NOT announced as current
        (other_repo, f.blobs[1].clone(), now), // another repository's: NOT announced
    ]);
    let m = GitManifest {
        v: PROTOCOL,
        op: f.op,
        repository_id: f.repo_id,
        head: f.head.clone(),
        entries: f.blobs.iter().map(|b| entry(b)).collect(),
    };
    let c = answer_git(&src, f.op, &policy(), &m).unwrap();
    assert_eq!(c.known, vec![(f.blobs[0].clone(), 0)]);
    assert_eq!(
        (c.op, c.repository_id, c.head.as_str()),
        (f.op, f.repo_id, f.head.as_str())
    );
    // the default source (no Git knowledge) announces nothing
    struct Nothing;
    impl ContextSource for Nothing {
        fn space_exists(&self, _: Uuid) -> Result<bool> {
            Ok(true)
        }
        fn derivation_known(
            &self,
            _: CodeLanguage,
            _: &str,
            _: &CodeParserIdentity,
        ) -> Result<bool> {
            Ok(true)
        }
    }
    assert!(answer_git(&Nothing, f.op, &policy(), &m)
        .unwrap()
        .known
        .is_empty());
}

#[allow(dead_code)]
fn unused(_: PathBuf) {}

/// The directory form of the broker's answer must not change on the wire because Git exists: the
/// repository identity is serialized only when there is one.
#[test]
fn the_directory_ready_message_has_no_repository_identity_key() {
    use mindspace::worker::broker::{AuthoritySource, Cleanup, FromBroker, Observed};
    let observed = Observed {
        pid: 1,
        seccomp: "2".into(),
        no_new_privs: "1".into(),
        tracer_pid: "0".into(),
        cap_eff: "0".into(),
        cgroup: None,
        cleanup: Cleanup::Cgroup,
    };
    let ready = |id: Option<Uuid>| FromBroker::Ready {
        v: 1,
        op: Uuid::nil(),
        spaces: vec![],
        root: "/r".into(),
        authority: AuthoritySource::Static,
        observed: observed.clone(),
        repository_id: id,
    };
    let dir = serde_json::to_value(ready(None)).unwrap();
    assert!(dir.get("repository_id").is_none(), "{dir}");
    let git = serde_json::to_value(ready(Some(Uuid::nil()))).unwrap();
    assert!(git.get("repository_id").is_some());
    // and an older message without the key still parses
    let back: FromBroker = serde_json::from_value(dir).unwrap();
    assert!(matches!(
        back,
        FromBroker::Ready {
            repository_id: None,
            ..
        }
    ));
}
