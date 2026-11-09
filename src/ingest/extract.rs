//! The seam between reading a repository and writing the database (M7.3b,
//! design note `docs/design-m7.3b-dynamic-workers.md`).
//!
//! An ingest has two phases:
//!
//!  * **Extraction** reads files or Git objects, parses them, and produces an
//!    [`Extracted`] value. It touches the database only through the read-only
//!    [`ExtractionContext`] (space ids, the previous ref state, "is this blob
//!    already parsed"). The trait has no write method, so extraction cannot
//!    write: that is a compile-time property, and a test checks the rows.
//!  * **Persistence** ([`persist`]) hands the value to the store. It is the only
//!    place that writes, in the same order and with the same deadline check as
//!    before the split.
//!
//! In-process ingest is `persist(extract(..))` and behaves as it always did. The
//! split exists so that a later, more confined process can do the extraction
//! while the process that owns the database persists it; the types therefore
//! derive serde, are moved (not copied) between the phases, and can be checked by
//! [`Extracted::validate`], which treats the value as untrusted input.
//!
//! No IPC protocol exists yet, and none of this is used across processes.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::{
    sha256_hex, CodeInputDraft, CodeLanguage, CodeObservation, CodeParserIdentity, Error,
    GitObservation, GitRefObservation, IngestObservation, IngestOutcome, Result, Uuid,
};
use crate::storage::Store;

use super::deadline::Deadline;

/// Everything extraction may ask the database. Read-only by construction.
pub trait ExtractionContext {
    /// The id of an existing space (an error if it does not exist).
    fn space_id(&self, name: &str) -> Result<Uuid>;
    /// The ref state recorded by the previous observation of a repository.
    fn git_refs_current(&self, repository_id: Uuid) -> Result<Vec<GitRefObservation>>;
    /// Whether this blob was already parsed with this parser identity.
    fn has_code_derivation(
        &self,
        language: CodeLanguage,
        content_hash: &str,
        repository_id: Option<Uuid>,
        blob_oid: Option<&str>,
        identity: &CodeParserIdentity,
    ) -> Result<bool>;
}

/// The context of in-process ingest: the store, used for reads only.
pub struct StoreContext<'a>(pub &'a dyn Store);

impl ExtractionContext for StoreContext<'_> {
    fn space_id(&self, name: &str) -> Result<Uuid> {
        Ok(self.0.get_space(name)?.id)
    }
    fn git_refs_current(&self, repository_id: Uuid) -> Result<Vec<GitRefObservation>> {
        self.0.git_refs_current(repository_id)
    }
    fn has_code_derivation(
        &self,
        language: CodeLanguage,
        content_hash: &str,
        repository_id: Option<Uuid>,
        blob_oid: Option<&str>,
        identity: &CodeParserIdentity,
    ) -> Result<bool> {
        self.0
            .has_code_derivation(language, content_hash, repository_id, blob_oid, identity)
    }
}

/// A file or directory observation, ready to persist.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedFiles {
    pub observation: IngestObservation,
    /// Parsed code, recorded after the observation (empty: nothing to record).
    pub code_inputs: Vec<CodeInputDraft>,
    /// Indexes into `code_inputs` that a record stream declared as reused derivations
    /// (placeholders for derivations the requester announced as known). Empty for in-process
    /// extraction. Provenance only: persistence ignores it, `worker::context::check_result` checks it.
    #[serde(default)]
    pub reused: std::collections::BTreeSet<usize>,
}

/// A Git repository observation, ready to persist.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedGit {
    pub observation: GitObservation,
    pub code_inputs: Vec<CodeInputDraft>,
}

// The size difference is a few hundred bytes of struct headers; the content lives on the heap,
// and a value is moved once from extraction to persistence.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Extracted {
    Files(ExtractedFiles),
    Git(ExtractedGit),
}

/// Writes an extraction to the database. The deadline is checked once, right
/// before the first write: the last point where an ingest can still stop
/// cleanly. After it the writes run to their end.
pub fn persist(
    store: &dyn Store,
    extracted: Extracted,
    deadline: &Deadline,
) -> Result<IngestOutcome> {
    match extracted {
        Extracted::Files(f) => persist_files(store, f, deadline).map(IngestOutcome::Files),
        Extracted::Git(g) => persist_git(store, g, deadline).map(IngestOutcome::Git),
    }
}

pub fn persist_files(
    store: &dyn Store,
    extracted: ExtractedFiles,
    deadline: &Deadline,
) -> Result<crate::core::ObservationReport> {
    persist_files_gated(store, extracted, deadline, &|| Ok(()))
}

/// As [`persist_files`], with a decision taken right before the COMMIT: if `before_commit` fails
/// the whole ingestion is rolled back and its error returned.
pub fn persist_files_gated(
    store: &dyn Store,
    extracted: ExtractedFiles,
    deadline: &Deadline,
    before_commit: &dyn Fn() -> Result<()>,
) -> Result<crate::core::ObservationReport> {
    // The last point where the limit can stop the ingest cleanly.
    deadline.check("preparing to write")?;
    // One transaction for the whole logical ingestion (observation + derivations): see
    // `Store::record_files_ingest`. (The Git path below is still separate writes.)
    let has_code = !extracted.code_inputs.is_empty();
    let code = has_code.then_some(CodeObservation {
        inputs: extracted.code_inputs,
    });
    store.record_files_ingest(extracted.observation, code, before_commit)
}

pub fn persist_git(
    store: &dyn Store,
    extracted: ExtractedGit,
    deadline: &Deadline,
) -> Result<crate::core::GitObservationReport> {
    let repository_id = extracted.observation.repository_id;
    // Sorted, as the ordered set it was derived from.
    let views = extracted.observation.view_commits.clone();
    // The last point where the limit can stop the ingest cleanly.
    deadline.check("preparing to write")?;
    let mut report = store.record_git_observation(extracted.observation)?;
    if !extracted.code_inputs.is_empty() {
        report.code = store.record_code_observation(CodeObservation {
            inputs: extracted.code_inputs,
        })?;
        for commit in views {
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

// ------------------------------------------------------------- validation

/// Bounds for one extraction result. The defaults are provisional (design note,
/// section 4b) and will be tuned by measurement.
#[derive(Debug, Clone, Copy)]
pub struct ExtractionLimits {
    /// Files (or derivations) in one result.
    pub max_files: usize,
    /// Total bytes of file content in one result.
    pub max_content_bytes: u64,
    /// Longest path or location.
    pub max_path_bytes: usize,
}

impl Default for ExtractionLimits {
    fn default() -> Self {
        Self {
            max_files: 100_000,
            max_content_bytes: 512 * 1024 * 1024,
            max_path_bytes: 4096,
        }
    }
}

fn reject<T>(why: impl Into<String>) -> Result<T> {
    Err(Error::InvalidExtraction(why.into()))
}

/// Release derivations are keyed by a virtual path `/refs/tags/<name>`; it is not
/// a file system path and is the only absolute form accepted.
const RELEASE_PREFIX: &str = "/refs/tags/";

/// A repository-relative path: not empty, not absolute, no `..`, no NUL (or the
/// virtual release path, with the same rules for the rest).
pub(crate) fn check_relative(path: &str, limits: &ExtractionLimits) -> Result<()> {
    if path.is_empty() || path.len() > limits.max_path_bytes || path.contains('\0') {
        return reject(format!("bad path length or NUL in `{}`", short(path)));
    }
    let path = path.strip_prefix(RELEASE_PREFIX).unwrap_or(path);
    if path.is_empty() {
        return reject("an empty release name");
    }
    let p = Path::new(path);
    if p.is_absolute()
        || p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return reject(format!(
            "path `{}` is absolute or climbs out of the root",
            short(path)
        ));
    }
    Ok(())
}

pub(crate) fn short(s: &str) -> String {
    s.chars().take(80).collect()
}

pub(crate) fn is_oid(s: &str) -> bool {
    matches!(s.len(), 40 | 64)
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Extracted {
    /// Bytes of file content held (the part that dominates memory).
    pub fn content_bytes(&self) -> u64 {
        match self {
            Extracted::Files(f) => {
                f.observation
                    .files
                    .iter()
                    .map(|x| x.content.len() as u64)
                    .sum::<u64>()
                    + code_bytes(&f.code_inputs)
            }
            Extracted::Git(g) => {
                g.observation
                    .derivations
                    .iter()
                    .map(|d| d.content.len() as u64)
                    .sum::<u64>()
                    + g.observation
                        .blobs
                        .iter()
                        .map(|b| b.content.as_ref().map_or(0, |c| c.len() as u64))
                        .sum::<u64>()
                    + code_bytes(&g.code_inputs)
            }
        }
    }

    /// Checks a result that came from outside the process as untrusted data:
    /// sizes against `limits`, content hashes recomputed from the content, paths
    /// relative (Git) or beneath `root` (files), object ids well formed, code
    /// hashes consistent. A failing result is dropped whole; nothing is repaired.
    pub fn validate(&self, root: Option<&Path>, limits: &ExtractionLimits) -> Result<()> {
        if self.content_bytes() > limits.max_content_bytes {
            return reject(format!(
                "{} bytes of content exceed the limit of {}",
                self.content_bytes(),
                limits.max_content_bytes
            ));
        }
        match self {
            Extracted::Files(f) => {
                if f.observation.files.len() > limits.max_files {
                    return reject(format!(
                        "{} files exceed the limit",
                        f.observation.files.len()
                    ));
                }
                if let Some(root) = root {
                    if Path::new(&f.observation.session.root) != root {
                        return reject("the session root is not the requested root");
                    }
                }
                for file in &f.observation.files {
                    if file.location.len() > limits.max_path_bytes || file.location.contains('\0') {
                        return reject("a file location is too long or contains NUL");
                    }
                    if let Some(root) = root {
                        let loc = Path::new(&file.location);
                        if !loc.starts_with(root)
                            || loc
                                .components()
                                .any(|c| matches!(c, std::path::Component::ParentDir))
                        {
                            return reject(format!(
                                "`{}` is outside the root",
                                short(&file.location)
                            ));
                        }
                    }
                    if sha256_hex(file.content.as_bytes()) != file.content_hash {
                        return reject(format!(
                            "content hash mismatch for `{}`",
                            short(&file.location)
                        ));
                    }
                    if file.size_bytes < file.content.len() as u64 {
                        return reject("size smaller than the content");
                    }
                }
                check_code(&f.code_inputs, limits)
            }
            Extracted::Git(g) => {
                let o = &g.observation;
                if o.derivations.len() > limits.max_files {
                    return reject(format!(
                        "{} derivations exceed the limit",
                        o.derivations.len()
                    ));
                }
                for d in &o.derivations {
                    check_relative(&d.path, limits)?;
                    if !is_oid(&d.blob_oid) || !is_oid(&d.commit_oid) {
                        return reject(format!("malformed object id for `{}`", short(&d.path)));
                    }
                    if sha256_hex(d.content.as_bytes()) != d.content_hash {
                        return reject(format!("content hash mismatch for `{}`", short(&d.path)));
                    }
                }
                for b in &o.blobs {
                    if !is_oid(&b.oid) {
                        return reject("malformed blob id");
                    }
                }
                for c in &o.view_commits {
                    if !is_oid(c) {
                        return reject("malformed commit id in the views");
                    }
                }
                check_code(&g.code_inputs, limits)
            }
        }
    }
}

fn code_bytes(inputs: &[CodeInputDraft]) -> u64 {
    inputs.iter().map(|c| c.content.len() as u64).sum()
}

fn check_code(inputs: &[CodeInputDraft], limits: &ExtractionLimits) -> Result<()> {
    for c in inputs {
        if sha256_hex(c.content.as_bytes()) != c.content_hash {
            return reject("code content hash mismatch");
        }
        if let Some(oid) = &c.blob_oid {
            if !is_oid(oid) {
                return reject("malformed blob id in code input");
            }
        }
        for ctx in &c.contexts {
            if ctx.path.len() > limits.max_path_bytes || ctx.path.contains('\0') {
                return reject("a code context path is too long or contains NUL");
            }
        }
    }
    Ok(())
}
