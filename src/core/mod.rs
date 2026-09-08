pub mod error;

pub use error::{Error, Result};

use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write as _;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub use uuid::Uuid;

// ---------------------------------------------------------------------------
// Timestamps
// ---------------------------------------------------------------------------

/// A point in time: microseconds since the Unix epoch, UTC.
///
/// Stored as an integer everywhere; rendered as RFC 3339 at the edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ts(pub i64);

impl Ts {
    pub fn now() -> Self {
        let d = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is before the Unix epoch");
        Ts(i64::try_from(d.as_micros()).unwrap_or(i64::MAX))
    }

    pub fn parse(s: &str) -> Result<Self> {
        let dt = OffsetDateTime::parse(s, &Rfc3339).map_err(|_| Error::InvalidTime {
            value: s.to_string(),
        })?;
        Ok(Ts((dt.unix_timestamp_nanos() / 1000) as i64))
    }

    fn datetime(self) -> Option<OffsetDateTime> {
        OffsetDateTime::from_unix_timestamp_nanos((self.0 as i128) * 1000).ok()
    }
}

impl fmt::Display for Ts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.datetime() {
            Some(dt) => f.write_str(&dt.format(&Rfc3339).map_err(|_| fmt::Error)?),
            None => write!(f, "{}", self.0),
        }
    }
}

impl Serialize for Ts {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Ts {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ts::parse(&s).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Content hashing and subject normalization
// ---------------------------------------------------------------------------

/// Lowercase hex SHA-256 of `bytes`. Content hashes identify sources,
/// evidence, and assertion text; nothing else identifies content.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// The matching key for subjects: whitespace-collapsed and lowercased.
/// Two assertions with the same normalized subject talk about the same
/// thing and can therefore conflict or supersede each other.
pub fn normalize_subject(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    File,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::File => "file",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "file" => Some(SourceKind::File),
            _ => None,
        }
    }
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Document,
    Section,
}

impl EvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EvidenceKind::Document => "document",
            EvidenceKind::Section => "section",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "document" => Some(EvidenceKind::Document),
            "section" => Some(EvidenceKind::Section),
            _ => None,
        }
    }
}

impl fmt::Display for EvidenceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertionKind {
    Claim,
    Event,
    Relation,
}

impl AssertionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AssertionKind::Claim => "claim",
            AssertionKind::Event => "event",
            AssertionKind::Relation => "relation",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claim" => Some(AssertionKind::Claim),
            "event" => Some(AssertionKind::Event),
            "relation" => Some(AssertionKind::Relation),
            _ => None,
        }
    }
}

impl fmt::Display for AssertionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryState {
    Active,
    Superseded,
    Retired,
}

impl MemoryState {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryState::Active => "active",
            MemoryState::Superseded => "superseded",
            MemoryState::Retired => "retired",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(MemoryState::Active),
            "superseded" => Some(MemoryState::Superseded),
            "retired" => Some(MemoryState::Retired),
            _ => None,
        }
    }
}

impl fmt::Display for MemoryState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Domain objects
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Space {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub created_at: Ts,
}

/// A directed link `from_space -> to_space`: `from_space` imports or uses
/// knowledge from `to_space`. No reverse visibility is implied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpaceLink {
    pub from_space: Uuid,
    pub to_space: Uuid,
    pub created_at: Ts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub id: Uuid,
    pub kind: SourceKind,
    pub location: String,
    pub title: String,
    pub content: String,
    pub content_hash: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub ingested_at: Ts,
}

/// A span of a source that asserts something: a whole document, a section,
/// a preamble. Evidence is immutable and never destroyed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub id: Uuid,
    pub source_id: Uuid,
    pub kind: EvidenceKind,
    pub heading_path: Option<String>,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
    pub ordinal: u32,
    pub content: String,
    pub content_hash: String,
}

/// The meaning extracted from a piece of evidence. Immutable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assertion {
    pub id: Uuid,
    pub kind: AssertionKind,
    pub subject: String,
    pub subject_normalized: String,
    pub predicate: Option<String>,
    pub content: String,
    pub extraction: String,
    pub evidence_id: Uuid,
}

/// The lifecycle wrapper around one assertion. This is the only mutable
/// object in the model: state transitions record when knowledge changed,
/// while the knowledge itself (assertion + evidence + source) never does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    pub id: Uuid,
    pub assertion_id: Uuid,
    pub state: MemoryState,
    pub created_at: Ts,
    pub superseded_by: Option<Uuid>,
    pub superseded_at: Option<Ts>,
    pub retired_at: Option<Ts>,
    pub retire_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

/// A memory plus its assertion and provenance summary, for listings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemorySummary {
    pub memory_id: Uuid,
    pub state: MemoryState,
    pub created_at: Ts,
    pub superseded_by: Option<Uuid>,
    pub superseded_at: Option<Ts>,
    pub retired_at: Option<Ts>,
    pub retire_reason: Option<String>,
    pub assertion_id: Uuid,
    pub kind: AssertionKind,
    pub subject: String,
    pub subject_normalized: String,
    pub content: String,
    pub extraction: String,
    pub spaces: Vec<String>,
    pub source_location: String,
    pub source_title: String,
}

/// Everything the engine knows about one memory, including the full
/// provenance chain down to the source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryDetail {
    pub memory: Memory,
    pub assertion: Assertion,
    pub evidence: Evidence,
    pub source: Source,
    pub spaces: Vec<Space>,
    pub superseded_by: Option<MemorySummary>,
    pub supersedes: Vec<MemorySummary>,
}

/// A space membership as it contributed to a search hit. `via` records the
/// directed traversal path that made the space reachable, e.g.
/// "coding -> linux"; `None` means the memory is a direct member.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpaceMatch {
    pub name: String,
    pub via: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub memory_id: Uuid,
    pub subject: String,
    pub content: String,
    pub snippet: String,
    pub score: f64,
    pub state: MemoryState,
    pub created_at: Ts,
    pub extraction: String,
    pub spaces: Vec<SpaceMatch>,
    pub source_title: String,
    pub source_location: String,
}

/// A raw search hit as the store returns it: rank and snippet come from the
/// store's lexical index; spaces are plain names, decorated by retrieval.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHitRaw {
    pub memory_id: Uuid,
    pub subject: String,
    pub content: String,
    pub snippet: String,
    pub score: f64,
    pub state: MemoryState,
    pub created_at: Ts,
    pub extraction: String,
    pub space_names: Vec<String>,
    pub source_title: String,
    pub source_location: String,
}

/// A structured search request. `space_ids: Some(vec![])` means "no spaces
/// requested" and matches nothing; `None` means unfiltered.
#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub text: String,
    pub space_ids: Option<Vec<Uuid>>,
    pub limit: u32,
    pub at: Option<Ts>,
    pub include_superseded: bool,
    pub include_retired: bool,
}

/// Two or more active memories with the same normalized subject that are
/// direct members of the same space. Conflicts coexist; the engine never
/// collapses them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConflictGroup {
    pub space_name: String,
    pub subject: String,
    pub members: Vec<MemorySummary>,
}

// ---------------------------------------------------------------------------
// Ingest structures
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct SourceDraft {
    pub kind: SourceKind,
    pub location: String,
    pub title: String,
    pub content: String,
    pub content_hash: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub ingested_at: Ts,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvidenceDraft {
    pub kind: EvidenceKind,
    pub heading_path: Option<String>,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
    pub ordinal: u32,
    pub content: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AssertionDraft {
    /// Index into the bundle's `evidences`.
    pub evidence_idx: usize,
    pub kind: AssertionKind,
    pub subject: String,
    pub subject_normalized: String,
    pub predicate: Option<String>,
    pub content: String,
    pub extraction: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemorySpec {
    /// Index into the bundle's `assertions`.
    pub assertion_idx: usize,
    pub spaces: Vec<Uuid>,
}

/// Everything one ingestion contributes, handed to the store in a single
/// call. The store owns the transaction and the reconciliation rules.
#[derive(Debug, Clone)]
pub struct IngestBundle {
    pub source: SourceDraft,
    pub evidences: Vec<EvidenceDraft>,
    pub assertions: Vec<AssertionDraft>,
    pub memories: Vec<MemorySpec>,
}

/// What happened to one memory during an ingestion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryAction {
    pub memory_id: Uuid,
    pub subject: String,
    pub action: MemoryActionKind,
    /// The memory's space memberships after the ingestion.
    pub spaces: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemoryActionKind {
    Created,
    Reused { spaces_added: Vec<String> },
    Superseded { old_memory_id: Uuid },
    Unchanged,
    Retired,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngestRecord {
    pub source: Source,
    pub source_reused: bool,
    pub evidence_count: usize,
    pub assertion_count: usize,
    pub actions: Vec<MemoryAction>,
}

/// Active-memory counts per space, keyed by space name, for listings.
pub type SpaceCounts = BTreeMap<String, u64>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_subject_whitespace_and_case() {
        assert_eq!(
            normalize_subject("  Rust   ownership rules "),
            "rust ownership rules"
        );
        assert_eq!(normalize_subject("Rust\n\townership"), "rust ownership");
    }

    #[test]
    fn sha256_is_lowercase_hex() {
        let h = sha256_hex(b"hello");
        assert_eq!(
            h,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert!(h
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn ts_round_trips_through_rfc3339() {
        let t = Ts(1_788_000_000_123_456);
        let s = t.to_string();
        assert_eq!(Ts::parse(&s).unwrap(), t);
    }

    #[test]
    fn ts_parse_rejects_garbage() {
        assert!(Ts::parse("not a time").is_err());
    }

    #[test]
    fn states_round_trip_through_strings() {
        for s in [
            MemoryState::Active,
            MemoryState::Superseded,
            MemoryState::Retired,
        ] {
            assert_eq!(MemoryState::parse(s.as_str()), Some(s));
        }
        assert_eq!(MemoryState::parse("other"), None);
    }
}
