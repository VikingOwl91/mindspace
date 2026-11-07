//! Releases from git tags, deterministically.
//!
//! Every `refs/tags/*` ref becomes one document-shaped memory: "Release
//! <name>", the tag message (or, for lightweight tags, the tagged commit's
//! message), the tagger or author, the date, and the tagged commit. No
//! markdown parsing is applied to the message, so changelog headings inside
//! a tag do not create colliding subjects across releases.
//!
//! The derivation rides the normal source -> evidence -> assertion -> memory
//! pipeline with the extraction rule `git:tag:release`. Its virtual path is
//! `/refs/tags/<name>`: a leading slash can never be a git tree path, so it
//! cannot collide with a real file. The commit stored for the version is the
//! peeled target commit; the blob oid is the tag object (annotated) or the
//! commit (lightweight), so a moved tag is a new version.

use std::collections::{BTreeSet, HashMap};

use uuid::Uuid;

use crate::core::{
    normalize_subject, sha256_hex, AssertionDraft, AssertionKind, EvidenceDraft, EvidenceKind,
    GitCommitData, GitDerivedFile, GitObjectKind, GitRefObservation, GitTagData, MemorySpec,
    Result, SourceKind,
};

use super::git::GitReader;

pub const EXTRACTION: &str = "git:tag:release";
pub const PATH_PREFIX: &str = "/refs/tags/";
const MAX_PEEL: usize = 8;

/// Release derivations for every tag ref, plus the (peeled) target commits
/// they refer to. Commit rows are needed so provenance can be loaded.
pub fn derive(
    reader: &GitReader,
    refs: &[GitRefObservation],
    space_ids: &[Uuid],
) -> Result<(Vec<GitDerivedFile>, Vec<GitCommitData>)> {
    let tag_refs: Vec<&GitRefObservation> = refs
        .iter()
        .filter(|r| r.name.starts_with("refs/tags/"))
        .collect();
    let mut tags: HashMap<String, GitTagData> = HashMap::new();
    let mut commits: HashMap<String, GitCommitData> = HashMap::new();

    let mut pending: BTreeSet<String> = tag_refs.iter().map(|r| r.target_oid.clone()).collect();
    for _ in 0..MAX_PEEL {
        if pending.is_empty() {
            break;
        }
        let oids: Vec<String> = pending.iter().cloned().collect();
        let (found_commits, _trees, found_tags) = reader.objects(&oids)?;
        for c in found_commits {
            commits.insert(c.oid.clone(), c);
        }
        let mut next = BTreeSet::new();
        for t in found_tags {
            if matches!(t.target_kind, GitObjectKind::Tag | GitObjectKind::Commit) {
                next.insert(t.target_oid.clone());
            }
            tags.insert(t.oid.clone(), t);
        }
        pending = next
            .into_iter()
            .filter(|o| !tags.contains_key(o) && !commits.contains_key(o))
            .collect();
    }

    let mut files = Vec::new();
    let mut used: BTreeSet<String> = BTreeSet::new();
    for r in tag_refs {
        let name = r.name.trim_start_matches("refs/tags/");
        // Peel: outermost annotated tag object (if any) down to a commit.
        let outer = (r.target_kind == GitObjectKind::Tag)
            .then(|| tags.get(&r.target_oid))
            .flatten();
        let mut commit_oid = match r.target_kind {
            GitObjectKind::Commit => Some(r.target_oid.clone()),
            _ => None,
        };
        let mut cur = outer;
        for _ in 0..MAX_PEEL {
            let Some(t) = cur else { break };
            match t.target_kind {
                GitObjectKind::Commit => {
                    commit_oid = Some(t.target_oid.clone());
                    break;
                }
                GitObjectKind::Tag => cur = tags.get(&t.target_oid),
                _ => break,
            }
        }
        // Tags of trees or blobs, or with unreadable targets, are not releases.
        let Some(commit) = commit_oid.as_ref().and_then(|o| commits.get(o)) else {
            continue;
        };
        if r.target_kind == GitObjectKind::Tag && outer.is_none() {
            continue;
        }

        let (message, who_label, who_name, who_email, date) = match outer {
            Some(t) => (
                t.message.as_str(),
                "Tagger",
                t.tagger_name.as_str(),
                t.tagger_email.as_str(),
                t.tag_time.unwrap_or(commit.commit_time),
            ),
            None => (
                commit.message.as_str(),
                "Author",
                commit.author_name.as_str(),
                commit.author_email.as_str(),
                commit.author_time,
            ),
        };
        let subject = format!("Release {name}");
        let mut content = format!("{subject}\n");
        if !message.trim().is_empty() {
            content.push_str(&format!("\n{}\n", message.trim()));
        }
        content.push_str(&format!(
            "\nTag: {name} ({})\n{who_label}: {who_name} <{who_email}>\nDate: {date}\nCommit: {}\n",
            if outer.is_some() { "annotated" } else { "lightweight" },
            commit.oid
        ));
        if outer.is_some() {
            content.push_str(&format!("Tag object: {}\n", r.target_oid));
        }
        let content_hash = sha256_hex(content.as_bytes());
        files.push(GitDerivedFile {
            source_kind: SourceKind::GitTag,
            path: format!("{PATH_PREFIX}{name}"),
            blob_oid: r.target_oid.clone(),
            commit_oid: commit.oid.clone(),
            content_hash: content_hash.clone(),
            title: subject.clone(),
            media_type: "text/plain".to_string(),
            evidences: vec![EvidenceDraft {
                kind: EvidenceKind::Document,
                heading_path: None,
                start_line: Some(1),
                end_line: Some(content.lines().count() as u32),
                ordinal: 0,
                content: content.clone(),
                content_hash,
            }],
            assertions: vec![AssertionDraft {
                evidence_idx: 0,
                kind: AssertionKind::Claim,
                subject_normalized: normalize_subject(&subject),
                subject,
                predicate: None,
                content: content.clone(),
                extraction: EXTRACTION.to_string(),
            }],
            memories: vec![MemorySpec {
                assertion_idx: 0,
                spaces: space_ids.to_vec(),
            }],
            content,
        });
        used.insert(commit.oid.clone());
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let used_commits = used
        .into_iter()
        .filter_map(|o| commits.remove(&o))
        .collect();
    Ok((files, used_commits))
}
