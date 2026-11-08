//! The isolated extraction worker (M7.3b.2a): the smallest vertical slice.
//!
//! A parent process (the future MCP frontend) starts `mind extraction-worker` with a verified root
//! directory descriptor and a channel, the worker confines itself (Landlock read access beneath
//! the root only, socket-deny seccomp, no database, no grant store, no network), extracts one
//! directory into the bounded record stream ([`crate::ingest::stream`]) and the parent validates
//! the stream before anything is persisted. A worker that fails, hangs or is killed costs the
//! parent nothing but an error.
//!
//! Not wired into any tool: the in-process ingest is unchanged until this slice has passed its
//! gate. Directory extraction only; the context exchange (`has_code_derivation`), Git, grants and
//! the broker are later slices.

pub mod client;
#[cfg(all(unix, feature = "spawn-gate"))]
pub mod gate;
pub mod run;

use serde::{Deserialize, Serialize};

use crate::core::Uuid;
use crate::ingest::code::ParsePolicy;

/// Channel and root descriptors in the worker.
pub const CHANNEL_FD: i32 = 3;
pub const ROOT_FD: i32 = 4;
pub const PROTOCOL: u32 = 1;
/// The request is one JSON line of at most this size.
pub const MAX_REQUEST: usize = 64 * 1024;
/// The hello is one JSON line of at most this size.
pub const MAX_HELLO: usize = 16 * 1024;

/// What the parent asks for. The worker never chooses its own authority: the root is the
/// descriptor it was given, and `root` only names it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Request {
    pub v: u32,
    pub op: Uuid,
    /// The canonical path of the directory behind descriptor 4 (recorded in the result).
    pub root: String,
    pub spaces: Vec<Uuid>,
    pub max_blob_bytes: u64,
    pub max_source_bytes: u64,
    pub max_entity_depth: u32,
    pub max_output_bytes: u64,
}

impl Request {
    pub fn parse_policy(&self) -> ParsePolicy {
        ParsePolicy {
            max_source_bytes: self.max_source_bytes,
            max_entity_depth: self.max_entity_depth,
            max_output_bytes: self.max_output_bytes,
        }
    }
}

/// The worker's first line: who it is and how it is confined.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hello {
    pub v: u32,
    pub pid: u32,
    pub sandbox: crate::sandbox::WorkerSandbox,
}
