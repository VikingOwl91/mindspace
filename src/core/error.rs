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

    #[error("invalid path '{path}': {reason}")]
    InvalidPath { path: String, reason: String },

    #[error("FTS index is inconsistent with canonical assertions: {detail}")]
    FtsIndexInconsistent { detail: String },
}

pub type Result<T> = std::result::Result<T, Error>;
