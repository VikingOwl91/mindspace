use thiserror::Error;

/// Errors for the whole engine. Messages are actionable by contract: they
/// name the offending object and spell out the fixing command where one
/// exists.
#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Refused(Refusal),

    #[error("{0}")]
    InvalidArgument(String),

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

    /// A document was refused before anything was stored (the reason is machine-readable).
    #[error("document refused ({}): {detail}", reason.as_str())]
    DocumentRefused {
        reason: super::document::DocumentRefusal,
        detail: String,
    },

    /// A citation anchor is malformed, or claims more than the stored extraction supports.
    #[error("invalid citation anchor: {0}")]
    InvalidAnchor(String),

    /// An extraction result failed validation (it is dropped whole).
    #[error("invalid extraction result: {0}")]
    InvalidExtraction(String),

    #[error("invalid extraction result: record stream: {0}")]
    TruncatedExtraction(String),

    /// Legacy strict-reader error. Ordinary ingestion reports per-path text exclusions.
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

    /// An ingest hit its time limit before committing the current root (or between
    /// independent ingest roots). The message says what was and was not kept.
    #[error("{message}")]
    IngestTimeout { message: String, writing: bool },

    #[error("ingest stopped before COMMIT, but SQLite rollback could not be confirmed ({detail}). {prior_roots} earlier selected roots were committed. Restart the server or reopen the database, then repeat the ingest.")]
    IngestRollbackUnconfirmed { detail: String, prior_roots: usize },

    #[error("cannot establish repository identity for {path}: {reason}")]
    RepositoryIdentity { path: String, reason: String },

    #[error("unsupported file format '{path}'; supported formats: .md, .markdown, .txt, .rs, .go, .js, .jsx, .mjs, .cjs, .ts, .mts, .cts, .tsx, .svelte")]
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

/// Stable refusal categories. Only the component that makes a decision assigns its kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RefusalKind {
    RootNotAuthorized,
    ReadOnlyRoot,
    BrokerUnavailable,
    Busy,
    ReplayRefused,
    IdentityMissing,
    UnsupportedRepository,
    NotARepository,
    GitUnavailable,
    UnsupportedObjectFormat,
    MissingLocalObject,
    TooManyFiles,
    TooLarge,
    ConfinementNotVerified,
    Timeout,
    NotEnabled,
    InvalidArgument,
    Revoked,
    CommitNotAuthorized,
    #[default]
    #[serde(other)]
    Refused,
}

impl RefusalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Refused => "refused",
            Self::RootNotAuthorized => "root_not_authorized",
            Self::ReadOnlyRoot => "read_only_root",
            Self::BrokerUnavailable => "broker_unavailable",
            Self::Busy => "busy",
            Self::ReplayRefused => "replay_refused",
            Self::IdentityMissing => "identity_missing",
            Self::UnsupportedRepository => "unsupported_repository",
            Self::NotARepository => "not_a_repository",
            Self::GitUnavailable => "git_unavailable",
            Self::UnsupportedObjectFormat => "unsupported_object_format",
            Self::MissingLocalObject => "missing_local_object",
            Self::TooManyFiles => "too_many_files",
            Self::TooLarge => "too_large",
            Self::ConfinementNotVerified => "confinement_not_verified",
            Self::Timeout => "timeout",
            Self::NotEnabled => "not_enabled",
            Self::InvalidArgument => "invalid_argument",
            Self::Revoked => "revoked",
            Self::CommitNotAuthorized => "commit_not_authorized",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct Refusal {
    pub kind: RefusalKind,
    pub detail: String,
}

impl Refusal {
    pub fn new(kind: RefusalKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl From<String> for Refusal {
    fn from(detail: String) -> Self {
        Self::new(RefusalKind::Refused, detail)
    }
}

impl From<&str> for Refusal {
    fn from(detail: &str) -> Self {
        Self::new(RefusalKind::Refused, detail)
    }
}

impl Error {
    pub fn refused(kind: RefusalKind, detail: impl Into<String>) -> Self {
        Self::Refused(Refusal::new(kind, detail))
    }

    pub(crate) fn read_only_root(root: &std::path::Path) -> Self {
        Self::refused(
            RefusalKind::ReadOnlyRoot,
            format!(
                "path is inside the read-only root {root}; ingesting needs a read-write root \
                 (start the server with --allow-root {root}:rw)",
                root = root.display()
            ),
        )
    }

    pub fn refusal_kind(&self) -> Option<RefusalKind> {
        match self {
            Self::Refused(r) => Some(r.kind),
            Self::InvalidArgument(_) => Some(RefusalKind::InvalidArgument),
            Self::UnsupportedObjectFormat { .. } => Some(RefusalKind::UnsupportedObjectFormat),
            Self::GitUnavailable { .. } => Some(RefusalKind::GitUnavailable),
            _ => None,
        }
    }
}
