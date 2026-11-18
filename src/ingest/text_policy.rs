//! Byte-exact text intake shared by filesystem and Git readers.
use crate::core::{Error, ObservedFile, Result, SkippedFile, SkippedGitBlob, TextSkipReason};
use std::{fs::File, io::Read};

pub const MAX_TEXT_BYTES: u64 = 32 * 1024 * 1024;
pub(crate) const REVISION: &str = "text-v2";
pub(super) type FileCandidate = std::result::Result<ObservedFile, SkippedFile>;

pub(super) fn decode(bytes: Vec<u8>) -> Option<String> {
    String::from_utf8(bytes).ok()
}

fn reason(size: u64, limit: u64) -> TextSkipReason {
    if size > limit {
        TextSkipReason::TooLarge { limit }
    } else {
        TextSkipReason::InvalidUtf8
    }
}

pub(super) fn git_skip(path: &str, commit: &str, blob: &str, size: u64, limit: u64) -> SkippedFile {
    SkippedFile {
        retained_previous_version: None,
        path: path.into(),
        size_bytes: size,
        reason: reason(size, limit),
        git: Some(SkippedGitBlob {
            commit_oid: commit.into(),
            blob_oid: blob.into(),
        }),
    }
}

pub(super) fn sort_skips(skips: &mut [SkippedFile]) {
    skips.sort_by(|a, b| {
        (a.git.as_ref().map(|g| &g.commit_oid), &a.path)
            .cmp(&(b.git.as_ref().map(|g| &g.commit_oid), &b.path))
    });
}

/// Inspect the opened descriptor before reading and cap the read if it grows.
pub(super) fn read_file(
    file: File,
    path: &str,
    limit: u64,
) -> Result<std::result::Result<String, SkippedFile>> {
    let limit = limit.min(MAX_TEXT_BYTES);
    let io = |source| Error::Io {
        path: path.into(),
        source,
    };
    let size = file.metadata().map_err(io)?.len();
    let skip = |size| SkippedFile {
        retained_previous_version: None,
        path: path.into(),
        size_bytes: size,
        reason: reason(size, limit),
        git: None,
    };
    if size > limit {
        return Ok(Err(skip(size)));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).map_err(io)?;
    let size = bytes.len() as u64;
    Ok(if size > limit {
        Err(skip(size))
    } else {
        decode(bytes).ok_or_else(|| skip(size))
    })
}
