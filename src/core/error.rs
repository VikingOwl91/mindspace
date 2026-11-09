use thiserror::Error;

/// Errors for the whole engine. Messages are actionable by contract: they
/// name the offending object and spell out the fixing command where one
/// exists.
#[derive(Debug, Error)]
pub enum Error {
    #[error("storage error: {0}")]
    Storage(String),

    #[error("database at {path} is not initialized; run: mind init")]
    NotInitialized { path: String },

    #[error(
        "cannot determine where to keep data: {reason}; set MINDSPACE_DB (or pass --db) for \
         the database and XDG_DATA_HOME or HOME for models"
    )]
    DataDirUnresolved { reason: String },

    /// `mind setup` stopped before or while registering a client; the text says what
    /// was (not) changed and what to do next.
    #[error("{0}")]
    Setup(String),

    /// A document extraction failed validation or contradicts a stored one (nothing was stored).
    #[error("invalid document extraction: {0}")]
    InvalidDocument(String),

    /// A citation anchor is malformed, or claims more than the stored extraction supports.
    #[error("invalid citation anchor: {0}")]
    InvalidAnchor(String),

    /// An extraction result failed validation (it is dropped whole).
    #[error("invalid extraction result: {0}")]
    InvalidExtraction(String),

    /// A text file or blob that extraction would read is larger than the configured
    /// limit. The operation fails; nothing is truncated or skipped.
    #[error("`{path}` is {size} bytes, over the limit of {limit} bytes for one text file or blob; exclude it with --path or raise the limit")]
    BlobTooLarge { path: String, size: u64, limit: u64 },

    /// A problem with the repository grant store or a grant command (M7.3a).
    #[error("{0}")]
    Grant(String),

    #[error("space '{name}' already exists")]
    SpaceExists { name: String },

    #[error("space '{name}' does not exist; run: mind space create {name}")]
    SpaceNotFound { name: String },

    #[error("link from '{from}' to '{to}' already exists")]
    LinkExists { from: String, to: String },

    #[error("a space cannot link to itself: '{name}'")]
    SelfLink { name: String },

    #[error("git is not available: {detail}")]
    GitUnavailable { detail: String },

    #[error("git command failed: {command}: {stderr}")]
    GitCommand { command: String, stderr: String },

    /// The repository's object format is not SHA-1. Object ids, the tree parser and the stored
    /// model are 40-digit SHA-1 throughout; a SHA-256 repository would be misread, not rejected.
    #[error(
        "unsupported repository: object format `{format}` is not supported \
         (only SHA-1 repositories can be ingested)"
    )]
    UnsupportedObjectFormat { format: String },

    /// An ingest hit its time limit before it wrote anything (or between
    /// independent ingest roots). The message says what was and was not kept.
    #[error("{message}")]
    IngestTimeout { message: String },

    #[error("cannot establish repository identity for {path}: {reason}")]
    RepositoryIdentity { path: String, reason: String },

    #[error("unsupported file format '{path}'; supported formats: .md, .markdown, .txt")]
    UnsupportedFormat { path: String },

    #[error("file '{path}' is not valid UTF-8")]
    InvalidEncoding { path: String },

    #[error("cannot access '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("no memory matches '{prefix}' (ids may be given as full UUIDs or unambiguous prefixes of at least 4 characters)")]
    MemoryNotFound { prefix: String },

    #[error("id prefix '{prefix}' is ambiguous; candidates: {candidates}")]
    AmbiguousPrefix { prefix: String, candidates: String },

    #[error("invalid time '{value}'; expected RFC 3339, e.g. 2026-01-31T12:00:00Z")]
    InvalidTime { value: String },

    #[error("invalid search query: {0}")]
    QuerySyntax(String),

    #[error("empty query")]
    EmptyQuery,

    #[error("memory {id} is {state}; only active memories can be retired")]
    NotRetirable { id: String, state: String },

    #[error("space name cannot be empty")]
    EmptyName,

    #[error("memory content cannot be empty")]
    EmptyContent,

    #[error("memory subject cannot be empty")]
    EmptySubject,

    #[error("at least one existing space is required for a manual memory")]
    SpacesRequired,

    #[error("space id {id} does not exist")]
    SpaceIdNotFound { id: String },

    #[error("memory {id} is {state}; only active memories can be superseded or assigned spaces")]
    MemoryNotActive { id: String, state: String },

    #[error(
        "memory {id} was not created manually; document-derived memories cannot be updated here"
    )]
    ManualUpdateNotAllowed { id: String },

    #[error("database is busy: {detail}")]
    DatabaseBusy { detail: String },

    #[error("MCP transport error: {detail}")]
    McpTransport { detail: String },

    #[error("invalid embedding vector: {detail}")]
    InvalidVector { detail: String },

    #[error("requested retrieval mode unavailable: {reason}")]
    SemanticUnavailable { reason: String },

    #[error("temporal history unavailable for {dimension} at revision {revision}: {reason}")]
    TemporalHistoryUnavailable {
        revision: i64,
        dimension: String,
        reason: String,
    },

    #[error("SQLite connection was discarded after an uncertain transaction outcome")]
    ConnectionDiscarded,

    /// The COMMIT failed in a way that does not say whether it took effect (an I/O error, a full
    /// disk...). Every OTHER error from a write method means nothing was committed; this one
    /// means "unknown": the data may or may not be there, and the connection is discarded.
    #[error("the outcome of the database commit is unknown: {detail}")]
    CommitOutcomeUnknown { detail: String },

    #[error("invalid path '{path}': {reason}")]
    InvalidPath { path: String, reason: String },

    #[error("FTS index is inconsistent with canonical assertions: {detail}")]
    FtsIndexInconsistent { detail: String },
}

pub type Result<T> = std::result::Result<T, Error>;
