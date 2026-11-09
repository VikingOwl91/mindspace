//! The context exchange (M7.3b.2c): the only database knowledge the worker ever gets.
//!
//! The worker has no database, so it cannot ask "was this blob already parsed?". The answer is
//! exchanged in two bounded, versioned, operation-bound JSON lines (half duplex, before the record
//! stream starts):
//!
//! ```text
//! parent -> worker   Request   v, op, root, spaces, limits           (authority is the fd, not this)
//! worker -> parent   Manifest  v, op, entries[{language, hash}]      (what it would have to parse)
//! parent -> worker   Context   v, op, identities[..], known[[hash, identity index]..]
//! worker -> parent   record stream (frames bound to op)
//! ```
//!
//! The manifest is produced by a first pass over the directory in which every blob counts as
//! "known" (so nothing is parsed and no output is kept); the parent probes its store for exactly
//! those hashes under the current parser identity and the skip identities of the operation's policy
//! and returns only the hits. The second pass answers `has_code_derivation` from that set, so the
//! decisions equal the in-process ingest's, query by query. A file that changes between the passes
//! has another hash, is not in the set and is parsed: the worst case is a re-parse.
//!
//! What this is not: an authorization decision. The worker learns one bit per hash it itself
//! computed (a worker could probe for the existence of derivations of content it can read: it
//! learns nothing about content it cannot read). Which root, which spaces and which limits apply is
//! decided by the parent alone and verified in the result.

use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::core::{CodeLanguage, CodeParserIdentity, Error, Result, Uuid};
use crate::ingest::code::{self, ParsePolicy};
use crate::ingest::extract::StoreContext;

use super::PROTOCOL;

/// Source files in one operation: the extraction's own file limit.
pub const MAX_MANIFEST_ENTRIES: usize = 100_000;
/// Identities in one context: the current parser identity and the three skip identities.
pub const MAX_IDENTITIES: usize = 4;
/// Known `(hash, identity)` pairs: every entry may be known under every identity.
pub const MAX_KNOWN: usize = MAX_MANIFEST_ENTRIES * MAX_IDENTITIES;
/// Spaces in one request.
pub const MAX_SPACES: usize = 64;
/// One JSON line each, newline included. A hash entry is about 85 bytes.
pub const MAX_MANIFEST_LINE: usize = 16 * 1024 * 1024;
pub const MAX_CONTEXT_LINE: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct ManifestEntry {
    pub language: CodeLanguage,
    /// Lowercase hex SHA-256 of the file's content.
    pub hash: String,
}

/// Worker to parent: the distinct source blobs of this directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub v: u32,
    pub op: Uuid,
    pub entries: Vec<ManifestEntry>,
}

/// Parent to worker: which of those blobs already have which derivation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub v: u32,
    pub op: Uuid,
    pub identities: Vec<CodeParserIdentity>,
    /// `[hash, index into identities]`, each pair at most once.
    pub known: Vec<(String, u8)>,
}

pub fn valid_hash(h: &str) -> bool {
    h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn bad<T>(m: impl Into<String>) -> std::result::Result<T, String> {
    Err(m.into())
}

/// The identities an operation with this policy can ask about, the current parser identity first.
pub fn identities_for(policy: &ParsePolicy) -> Vec<CodeParserIdentity> {
    let mut v = vec![code::identity()];
    v.extend(policy.reasons().into_iter().map(code::skip_identity));
    v
}

/// The spaces of a request: bounded, no duplicates.
pub fn validate_spaces(spaces: &[Uuid]) -> std::result::Result<(), String> {
    if spaces.len() > MAX_SPACES {
        return bad(format!("{} spaces (at most {MAX_SPACES})", spaces.len()));
    }
    let mut seen = HashSet::new();
    if !spaces.iter().all(|s| seen.insert(*s)) {
        return bad("a space is named twice");
    }
    Ok(())
}

impl Manifest {
    /// Strict check on the parent's side: right version, this operation, bounded, well-formed and
    /// free of duplicates.
    pub fn validate(&self, op: Uuid) -> std::result::Result<(), String> {
        if self.v != PROTOCOL {
            return bad(format!("manifest protocol {}", self.v));
        }
        if self.op != op {
            return bad("the manifest belongs to another operation");
        }
        if self.entries.len() > MAX_MANIFEST_ENTRIES {
            return bad(format!(
                "{} manifest entries (at most {MAX_MANIFEST_ENTRIES})",
                self.entries.len()
            ));
        }
        let mut seen = HashSet::with_capacity(self.entries.len());
        for e in &self.entries {
            if !valid_hash(&e.hash) {
                return bad("a manifest hash is not a lowercase SHA-256");
            }
            if !seen.insert((e.language, e.hash.as_str())) {
                return bad("a manifest entry is repeated");
            }
        }
        Ok(())
    }
}

impl Context {
    /// Strict check on the worker's side. `policy` is the one of its own request: the context may
    /// only mention identities that policy can produce, in any order, each once.
    pub fn validate(&self, op: Uuid, policy: &ParsePolicy) -> std::result::Result<(), String> {
        if self.v != PROTOCOL {
            return bad(format!("context protocol {}", self.v));
        }
        if self.op != op {
            return bad("the context belongs to another operation");
        }
        if self.identities.len() > MAX_IDENTITIES {
            return bad("too many identities");
        }
        let allowed = identities_for(policy);
        let mut seen = Vec::new();
        for i in &self.identities {
            if !allowed.contains(i) {
                return bad("the context names an identity this operation cannot ask about");
            }
            if seen.contains(&i) {
                return bad("an identity is listed twice");
            }
            seen.push(i);
        }
        if self.known.len() > MAX_KNOWN {
            return bad(format!(
                "{} known derivations (at most {MAX_KNOWN})",
                self.known.len()
            ));
        }
        let mut pairs = HashSet::with_capacity(self.known.len());
        for (h, i) in &self.known {
            if !valid_hash(h) {
                return bad("a known hash is not a lowercase SHA-256");
            }
            if usize::from(*i) >= self.identities.len() {
                return bad("a known derivation names a missing identity");
            }
            if !pairs.insert((h.as_str(), *i)) {
                return bad("a known derivation is repeated");
            }
        }
        Ok(())
    }
}

/// What the parent asks its database. Read-only by construction; an error is an error (the
/// in-process directory ingest is strict in the same way).
pub trait ContextSource {
    fn space_exists(&self, id: Uuid) -> Result<bool>;
    fn derivation_known(
        &self,
        language: CodeLanguage,
        content_hash: &str,
        identity: &CodeParserIdentity,
    ) -> Result<bool>;
}

impl ContextSource for StoreContext<'_> {
    fn space_exists(&self, id: Uuid) -> Result<bool> {
        Ok(self.0.list_spaces()?.iter().any(|s| s.id == id))
    }
    fn derivation_known(
        &self,
        language: CodeLanguage,
        content_hash: &str,
        identity: &CodeParserIdentity,
    ) -> Result<bool> {
        use crate::ingest::extract::ExtractionContext;
        self.has_code_derivation(language, content_hash, None, None, identity)
    }
}

/// Builds the parent's answer to a validated manifest: the hits, nothing else.
pub fn answer(
    src: &dyn ContextSource,
    op: Uuid,
    policy: &ParsePolicy,
    manifest: &Manifest,
) -> Result<Context> {
    let identities = identities_for(policy);
    let mut known = Vec::new();
    for e in &manifest.entries {
        // the order of the in-process rule: the current identity first; skip identities matter only
        // when it is unknown (the worker never asks otherwise)
        if src.derivation_known(e.language, &e.hash, &identities[0])? {
            known.push((e.hash.clone(), 0u8));
            continue;
        }
        for (i, id) in identities.iter().enumerate().skip(1) {
            if src.derivation_known(e.language, &e.hash, id)? {
                known.push((e.hash.clone(), i as u8));
            }
        }
    }
    Ok(Context {
        v: PROTOCOL,
        op,
        identities,
        known,
    })
}

/// Checks the worker's result against what the parent asked for and told it, before anything is
/// persisted. The root is checked by the stream consumer; this adds the spaces, the identities and
/// the provenance of every reuse placeholder.
///
/// Placeholder provenance (the index-poisoning invariant): a code input may be declared `reused`
/// only for a `(content hash, identity)` pair that appears in `sent`, the context this very
/// operation transmitted. A reuse claim for anything else (an unsent blob, another identity, a
/// pair known only to another operation) is refused, and so is a freshly produced derivation that
/// contradicts what was announced as known. Emptiness is never evidence of reuse.
///
/// What this does not establish: that a *fresh* derivation is what the parser would have produced.
/// See `docs/m7.3b-context-exchange.md` (residual trust boundary).
pub fn check_result(
    files: &crate::ingest::extract::ExtractedFiles,
    op: Uuid,
    spaces: &[Uuid],
    policy: &ParsePolicy,
    sent: &Context,
) -> std::result::Result<(), String> {
    if sent.op != op {
        return bad("the context belongs to another operation");
    }
    if files.observation.spaces != spaces {
        return bad("the result names other spaces than the request");
    }
    let allowed = identities_for(policy);
    let current = &allowed[0];
    let known: HashSet<(&str, &CodeParserIdentity)> = sent
        .known
        .iter()
        .filter_map(|(h, i)| {
            sent.identities
                .get(usize::from(*i))
                .map(|id| (h.as_str(), id))
        })
        .collect();
    if files.reused.iter().any(|i| *i >= files.code_inputs.len()) {
        return bad("a reuse declaration points at no code input");
    }
    // one decision per content: equal content must get the same identity and provenance
    let mut decided: std::collections::HashMap<&str, (&CodeParserIdentity, bool)> =
        std::collections::HashMap::new();
    for (n, input) in files.code_inputs.iter().enumerate() {
        let id = &input.derivation.identity;
        if !allowed.contains(id) {
            return bad("a derivation carries an identity this operation cannot produce");
        }
        let hash = input.content_hash.as_str();
        let reused = files.reused.contains(&n);
        if reused {
            if !known.contains(&(hash, id)) {
                return bad("a reused derivation the parent did not announce for this operation");
            }
            let d = &input.derivation;
            if !(d.entities.is_empty() && d.imports.is_empty() && d.relationships.is_empty()) {
                return bad("a reused derivation carries structure");
            }
        } else if known.contains(&(hash, id)) || known.contains(&(hash, current)) {
            return bad("a fresh derivation for content announced as known");
        }
        if let Some(prev) = decided.insert(hash, (id, reused)) {
            if prev != (id, reused) {
                return bad("conflicting derivations for the same content");
            }
        }
        // a skip derivation must be one the policy would produce for this content
        if let Some(reason) = code::skip_reason_of(id) {
            let oversized = input.content.len() as u64 > policy.max_source_bytes;
            let source_reason =
                reason.starts_with(&format!("{}:source_bytes", code::POLICY_REVISION));
            if source_reason != oversized {
                return bad("a skip derivation does not fit the size of its source");
            }
        }
    }
    Ok(())
}

/// First pass of the worker: every blob counts as known (nothing is parsed), and each distinct
/// `(language, hash)` is recorded.
#[derive(Default)]
pub struct Collector {
    entries: std::cell::RefCell<BTreeSet<ManifestEntry>>,
}

impl Collector {
    pub fn into_entries(self) -> Vec<ManifestEntry> {
        self.entries.into_inner().into_iter().collect()
    }
}

impl crate::ingest::extract::ExtractionContext for Collector {
    fn space_id(&self, name: &str) -> Result<Uuid> {
        Err(Error::InvalidExtraction(format!(
            "the worker cannot resolve the space `{name}`"
        )))
    }
    fn git_refs_current(&self, _: Uuid) -> Result<Vec<crate::core::GitRefObservation>> {
        Ok(Vec::new())
    }
    fn has_code_derivation(
        &self,
        language: CodeLanguage,
        content_hash: &str,
        _: Option<Uuid>,
        _: Option<&str>,
        _: &CodeParserIdentity,
    ) -> Result<bool> {
        let mut set = self.entries.borrow_mut();
        set.insert(ManifestEntry {
            language,
            hash: content_hash.to_string(),
        });
        if set.len() > MAX_MANIFEST_ENTRIES {
            return Err(Error::InvalidExtraction(format!(
                "more than {MAX_MANIFEST_ENTRIES} source files: too many for the context exchange"
            )));
        }
        Ok(true)
    }
}

/// Second pass of the worker: the parent's answer, and nothing else, is what is "known".
pub struct KnownSet(HashSet<(String, CodeParserIdentity)>);

impl KnownSet {
    /// From a context that passed [`Context::validate`].
    pub fn new(context: Context) -> Self {
        let ids = context.identities;
        Self(
            context
                .known
                .into_iter()
                .map(|(h, i)| (h, ids[usize::from(i)].clone()))
                .collect(),
        )
    }
}

impl crate::ingest::extract::ExtractionContext for KnownSet {
    fn space_id(&self, name: &str) -> Result<Uuid> {
        Err(Error::InvalidExtraction(format!(
            "the worker cannot resolve the space `{name}`"
        )))
    }
    fn git_refs_current(&self, _: Uuid) -> Result<Vec<crate::core::GitRefObservation>> {
        Ok(Vec::new())
    }
    fn has_code_derivation(
        &self,
        _: CodeLanguage,
        content_hash: &str,
        _: Option<Uuid>,
        _: Option<&str>,
        identity: &CodeParserIdentity,
    ) -> Result<bool> {
        Ok(self
            .0
            .contains(&(content_hash.to_string(), identity.clone())))
    }
}
