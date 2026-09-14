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
    GitBlob,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::File => "file",
            SourceKind::GitBlob => "git_blob",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "file" => Some(SourceKind::File),
            "git_blob" => Some(SourceKind::GitBlob),
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

/// One immutable content version of a logical source: the bytes observed at
/// a location at a transaction time. `location` is where this version was
/// first observed and never changes; the logical source's current location
/// lives in the lifecycle state. Version identity is
/// `(logical_id, content_hash)`: the same bytes of the same logical source
/// are one version, however often they are re-observed or relocated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub id: Uuid,
    pub logical_id: Uuid,
    pub kind: SourceKind,
    pub location: String,
    pub title: String,
    pub content: String,
    pub content_hash: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub ingested_at: Ts,
}

/// Whether a logical source is currently observable at its last known
/// location. Absence is a source-availability fact, not a truth fact:
/// derived memories stay active while a source is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourcePresence {
    Present,
    Absent,
}

impl SourcePresence {
    pub fn as_str(self) -> &'static str {
        match self {
            SourcePresence::Present => "present",
            SourcePresence::Absent => "absent",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "present" => Some(SourcePresence::Present),
            "absent" => Some(SourcePresence::Absent),
            _ => None,
        }
    }
}

impl fmt::Display for SourcePresence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The kind of a logical source identity: a plain file path, or a git
/// (repository, commit, path) material. Distinct from SourceKind, which
/// types content versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogicalKind {
    File,
    GitPath,
}

impl LogicalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LogicalKind::File => "file",
            LogicalKind::GitPath => "git_path",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "file" => Some(LogicalKind::File),
            "git_path" => Some(LogicalKind::GitPath),
            _ => None,
        }
    }
}

impl fmt::Display for LogicalKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The stable identity of a source across moves and renames. This is the
/// mutable lifecycle entity of the source side of the model; versions
/// (Source rows) are immutable. Adapters of future source kinds bring their
/// own deterministic identity signals; location is only an observation
/// attribute, never the identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogicalSource {
    pub id: Uuid,
    pub kind: LogicalKind,
    pub state: SourcePresence,
    /// Last known location. Kept when the source disappears so a
    /// reappearance at the same path can be recognized.
    pub current_location: Option<String>,
    pub created_at: Ts,
}

/// A source lifecycle change recorded in the journal. Only changes are
/// journaled; routine re-observations live in the session records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceEvent {
    pub id: Uuid,
    pub logical_id: Uuid,
    pub kind: SourceEventKind,
    /// The version this event refers to (the continuation point of a move).
    pub version_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub created_at: Ts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceEventKind {
    /// Inferred continuation of one logical source at a new location
    /// (rename, move, or reappearance after absence). `from_location` is
    /// the last known location.
    Moved {
        from_location: Option<String>,
        to_location: String,
    },
    /// A full observation no longer sees the logical source within its
    /// root. Derived memories are NOT retired: disappearance is an
    /// availability fact, not a truth fact.
    Disappeared { from_location: Option<String> },
}

/// The logical identity and lifecycle journal behind one source version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceLifecycle {
    pub logical: LogicalSource,
    pub events: Vec<SourceEvent>,
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

/// The meaning extracted from a piece of evidence by a named rule.
/// Identity is the interpretation itself: the same evidence, rule, subject,
/// and content is the same assertion, no matter how many belief episodes
/// (memories) reference it over time. Immutable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assertion {
    pub id: Uuid,
    pub kind: AssertionKind,
    pub subject: String,
    pub subject_normalized: String,
    pub predicate: Option<String>,
    pub content: String,
    pub content_hash: String,
    pub extraction: String,
    /// Transaction time the interpretation was extracted.
    pub created_at: Ts,
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
/// provenance chain down to the source, plus git provenance when the
/// source version was derived from a git blob.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryDetail {
    pub memory: Memory,
    pub assertion: Assertion,
    pub evidence: Evidence,
    pub source: Source,
    pub source_lifecycle: SourceLifecycle,
    pub spaces: Vec<Space>,
    pub superseded_by: Option<MemorySummary>,
    pub supersedes: Vec<MemorySummary>,
    pub git: Option<GitProvenance>,
}

/// The git provenance of a source version derived from a text blob:
/// repository, blob, path, commit, and the refs that currently point at
/// the commit (ref context is observational, not part of identity).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitProvenance {
    pub repository_path: String,
    pub blob_oid: String,
    pub path: String,
    pub commit_oid: String,
    pub commit_time: Ts,
    pub commit_message: String,
    pub refs: Vec<String>,
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

/// The boundary of one ingest operation. A `Directory` observation covers a
/// whole ingest root and drives the full lifecycle reconciliation (moves,
/// disappearances); a `File` observation is a partial, path-local
/// observation that never marks anything disappeared; a `GitRepository`
/// observation scans committed objects and refs of one git repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestScope {
    Directory,
    File,
    GitRepository,
}

impl IngestScope {
    pub fn as_str(self) -> &'static str {
        match self {
            IngestScope::Directory => "directory",
            IngestScope::File => "file",
            IngestScope::GitRepository => "git_repository",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "directory" => Some(IngestScope::Directory),
            "file" => Some(IngestScope::File),
            "git_repository" => Some(IngestScope::GitRepository),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionDraft {
    pub root: String,
    pub scope: IngestScope,
    pub observed_at: Ts,
}

/// One observed file handed to the store: content identity plus the
/// deterministic parse into evidence and assertion drafts with requested
/// spaces. Parsing happens before the store call; the store owns all
/// identity, lifecycle, and reconciliation decisions.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservedFile {
    pub location: String,
    pub content: String,
    pub content_hash: String,
    pub title: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub evidences: Vec<EvidenceDraft>,
    pub assertions: Vec<AssertionDraft>,
    pub memories: Vec<MemorySpec>,
}

// Syntax-derived facts deliberately do not enter the Evidence/Assertion/
// Memory spine. Positions are half-open bytes plus one-based line/columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeLanguage {
    Rust,
}
impl CodeLanguage {
    pub fn as_str(self) -> &'static str {
        "rust"
    }
    pub fn parse(s: &str) -> Option<Self> {
        (s == "rust").then_some(Self::Rust)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeParseStatus {
    Complete,
    Recovered,
    Failed,
}
impl CodeParseStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Recovered => "recovered",
            Self::Failed => "failed",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "complete" => Some(Self::Complete),
            "recovered" => Some(Self::Recovered),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeEntityKind {
    Module,
    Struct,
    Enum,
    Trait,
    TypeAlias,
    Function,
    Method,
    Constant,
    Static,
    Field,
    Import,
}
impl CodeEntityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Module => "module",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::TypeAlias => "type_alias",
            Self::Function => "function",
            Self::Method => "method",
            Self::Constant => "constant",
            Self::Static => "static",
            Self::Field => "field",
            Self::Import => "import",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "module" => Some(Self::Module),
            "struct" => Some(Self::Struct),
            "enum" => Some(Self::Enum),
            "trait" => Some(Self::Trait),
            "type_alias" => Some(Self::TypeAlias),
            "function" => Some(Self::Function),
            "method" => Some(Self::Method),
            "constant" => Some(Self::Constant),
            "static" => Some(Self::Static),
            "field" => Some(Self::Field),
            "import" => Some(Self::Import),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeRange {
    pub start_byte: u64,
    pub end_byte: u64,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeParserIdentity {
    pub extractor: String,
    pub extractor_version: String,
    pub grammar: String,
    pub grammar_version: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeEntity {
    pub id: Uuid,
    pub kind: CodeEntityKind,
    pub name: Option<String>,
    pub qualified_name: Option<String>,
    pub syntax_path: String,
    pub parent_syntax_path: Option<String>,
    pub range: CodeRange,
    pub signature: Option<String>,
    pub visibility: Option<String>,
    pub is_async: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeImport {
    pub raw: String,
    pub target: String,
    pub alias: Option<String>,
    pub range: CodeRange,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeDerivation {
    pub id: Uuid,
    pub language: CodeLanguage,
    pub identity: CodeParserIdentity,
    pub status: CodeParseStatus,
    pub error_count: u32,
    pub content_hash: String,
    pub entities: Vec<CodeEntity>,
    pub imports: Vec<CodeImport>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct CodeInputDraft {
    pub language: CodeLanguage,
    pub content_hash: String,
    pub content: String,
    pub repository_id: Option<Uuid>,
    pub blob_oid: Option<String>,
    pub contexts: Vec<CodeContextDraft>,
    pub derivation: CodeDerivationDraft,
}
#[derive(Debug, Clone, PartialEq)]
pub struct CodeContextDraft {
    pub path: String,
    pub commit_oid: Option<String>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct CodeDerivationDraft {
    pub identity: CodeParserIdentity,
    pub status: CodeParseStatus,
    pub error_count: u32,
    pub entities: Vec<CodeEntityDraft>,
    pub imports: Vec<CodeImportDraft>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct CodeEntityDraft {
    pub kind: CodeEntityKind,
    pub name: Option<String>,
    pub qualified_name: Option<String>,
    pub syntax_path: String,
    pub parent_syntax_path: Option<String>,
    pub range: CodeRange,
    pub signature: Option<String>,
    pub visibility: Option<String>,
    pub is_async: bool,
}
#[derive(Debug, Clone, PartialEq)]
pub struct CodeImportDraft {
    pub raw: String,
    pub target: String,
    pub alias: Option<String>,
    pub range: CodeRange,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeRelationshipKind { Calls, Imports, Implements }
impl CodeRelationshipKind { pub fn as_str(self)->&'static str {match self {Self::Calls=>"calls",Self::Imports=>"imports",Self::Implements=>"implements"}} pub fn parse(s:&str)->Option<Self>{match s{"calls"=>Some(Self::Calls),"imports"=>Some(Self::Imports),"implements"=>Some(Self::Implements),_=>None}} }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeResolutionState { Resolved, Unresolved, Ambiguous, External }
impl CodeResolutionState { pub fn as_str(self)->&'static str {match self {Self::Resolved=>"resolved",Self::Unresolved=>"unresolved",Self::Ambiguous=>"ambiguous",Self::External=>"external"}} }
#[derive(Debug, Clone, PartialEq)] pub struct CodeRelationshipDraft { pub kind:CodeRelationshipKind, pub source_syntax_path:Option<String>, pub raw_target:String, pub range:CodeRange }
#[derive(Debug, Clone, PartialEq)]
pub struct CodeObservation {
    pub inputs: Vec<CodeInputDraft>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CodeObservationReport {
    pub supported_inputs: u64,
    pub parsed: u64,
    pub reused: u64,
    pub entities: u64,
    pub parse_errors: u64,
}

/// Everything one ingest operation observed, handed to the store in a
/// single call. The store owns the transaction, the lifecycle rules, and
/// the reconciliation rules.
#[derive(Debug, Clone)]
pub struct IngestObservation {
    pub session: SessionDraft,
    pub files: Vec<ObservedFile>,
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
    /// Index into the observed file's `evidences`.
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
    /// Index into the observed file's `assertions`.
    pub assertion_idx: usize,
    pub spaces: Vec<Uuid>,
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

/// How one observed file was classified against known source identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileOutcome {
    /// Same logical source, same content as last observed: nothing to do.
    Unchanged,
    /// An existing logical source observed with changed content (a new
    /// version, or a version that already existed from a flip-flop).
    Changed,
    /// A new logical source was created with its first version.
    Created,
    /// An existing logical source was continued at this path by
    /// deterministic move inference (rename, move, or reappearance).
    /// The content version already existed and is reused.
    Continued { from_location: Option<String> },
}

/// The per-path result of one observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileObservation {
    pub location: String,
    pub logical_id: Uuid,
    pub outcome: FileOutcome,
    /// Memory changes for this logical source, if reconciliation ran.
    pub actions: Vec<MemoryAction>,
}

/// What one ingest operation did, per file and to the lifecycle journal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservationReport {
    pub session_id: Uuid,
    pub root: String,
    pub scope: IngestScope,
    pub files: Vec<FileObservation>,
    pub events: Vec<SourceEvent>,
}

// ---------------------------------------------------------------------------
// Git observation structures
//
// These are plain, git-library-free types: the ingest side reads a git
// repository and hands the store normalized data. Objects are immutable and
// identified by (repository, oid); refs are observations with a
// transaction-time journal.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitObjectKind {
    Commit,
    Tree,
    Blob,
    Tag,
}

impl GitObjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            GitObjectKind::Commit => "commit",
            GitObjectKind::Tree => "tree",
            GitObjectKind::Blob => "blob",
            GitObjectKind::Tag => "tag",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "commit" => Some(GitObjectKind::Commit),
            "tree" => Some(GitObjectKind::Tree),
            "blob" => Some(GitObjectKind::Blob),
            "tag" => Some(GitObjectKind::Tag),
            _ => None,
        }
    }
}

impl fmt::Display for GitObjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitRefObservation {
    pub name: String,
    pub target_oid: String,
    pub target_kind: GitObjectKind,
    /// Set when the ref is symbolic (e.g. HEAD -> refs/heads/main).
    pub symbolic_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitCommitData {
    pub oid: String,
    pub tree_oid: String,
    /// Parent oids in order; a merge has two or more.
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub author_time: Ts,
    pub committer_name: String,
    pub committer_email: String,
    pub commit_time: Ts,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitTreeEntryData {
    pub name: String,
    pub mode: String,
    pub target_oid: String,
    pub target_kind: GitObjectKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitTreeData {
    pub oid: String,
    pub entries: Vec<GitTreeEntryData>,
}

/// A blob object. `content` is present only for text blobs
/// (`.md`, `.markdown`, `.txt`), which are the derivation inputs; other
/// blobs are recorded as identity and size.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitBlobData {
    pub oid: String,
    pub size_bytes: u64,
    pub content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitTagData {
    pub oid: String,
    pub target_oid: String,
    pub target_kind: GitObjectKind,
    pub tagger_name: String,
    pub tagger_email: String,
    pub tag_time: Option<Ts>,
    pub message: String,
}

/// One text file derived at a ref tip: a git blob routed through the
/// existing evidence/assertion pipeline with git extraction rule ids.
#[derive(Debug, Clone)]
pub struct GitDerivedFile {
    pub path: String,
    pub blob_oid: String,
    pub commit_oid: String,
    pub content: String,
    pub content_hash: String,
    pub title: String,
    pub media_type: String,
    pub evidences: Vec<EvidenceDraft>,
    pub assertions: Vec<AssertionDraft>,
    pub memories: Vec<MemorySpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitRefEventKind {
    Created,
    FastForward,
    NonFastForward,
    Deleted,
}

impl GitRefEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            GitRefEventKind::Created => "created",
            GitRefEventKind::FastForward => "fast_forward",
            GitRefEventKind::NonFastForward => "non_fast_forward",
            GitRefEventKind::Deleted => "deleted",
        }
    }
}

/// A computed ref delta handed to the store. Classification happens in the
/// ingest side (which can ask git the ancestor question); the store only
/// journals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitRefEventDraft {
    pub name: String,
    pub kind: GitRefEventKind,
    pub from_oid: Option<String>,
    pub to_oid: Option<String>,
}

/// Everything one git observation saw, handed to the store in a single
/// call. The store owns the transaction, identity decisions, and counters.
#[derive(Debug, Clone)]
pub struct GitObservation {
    pub session: SessionDraft,
    pub repository_id: Uuid,
    pub repository_path: String,
    pub refs: Vec<GitRefObservation>,
    pub commits: Vec<GitCommitData>,
    pub trees: Vec<GitTreeData>,
    pub blobs: Vec<GitBlobData>,
    pub tags: Vec<GitTagData>,
    pub derivations: Vec<GitDerivedFile>,
    pub ref_events: Vec<GitRefEventDraft>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitFileOutcome {
    /// The version (repository, commit, path, content) already existed.
    Unchanged,
    /// The version is new in this session.
    New,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitFileReport {
    pub path: String,
    pub commit_oid: String,
    pub blob_oid: String,
    pub outcome: GitFileOutcome,
    pub actions: Vec<MemoryAction>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitRefEventRecord {
    pub ref_name: String,
    pub kind: GitRefEventKind,
    pub from_oid: Option<String>,
    pub to_oid: Option<String>,
    pub created_at: Ts,
}

/// Deterministic counters and per-file results of one git observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitObservationReport {
    pub session_id: Uuid,
    pub repository_id: Uuid,
    pub repository_path: String,
    pub refs_observed: u64,
    pub refs_added: u64,
    pub refs_changed: u64,
    pub refs_removed: u64,
    pub objects_discovered: u64,
    pub objects_new: u64,
    pub objects_existing: u64,
    pub derivations_new: u64,
    pub derivations_reused: u64,
    /// Syntax-only structural derivations. These counters never include memories.
    pub code: CodeObservationReport,
    pub files: Vec<GitFileReport>,
    pub events: Vec<GitRefEventRecord>,
}

/// What `ingest_path` did: a file/directory observation or a git
/// repository observation.
#[derive(Debug, Clone)]
pub enum IngestOutcome {
    Files(ObservationReport),
    Git(GitObservationReport),
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
