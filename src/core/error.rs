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
}

pub type Result<T> = std::result::Result<T, Error>;
