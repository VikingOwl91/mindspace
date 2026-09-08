pub mod markdown;

use std::path::{Path, PathBuf};

use crate::core::{
    normalize_subject, sha256_hex, AssertionDraft, Error, EvidenceDraft, IngestObservation,
    IngestScope, MemorySpec, ObservationReport, ObservedFile, Result, SessionDraft, Ts,
};
use crate::storage::Store;

/// Ingests a file or a directory as one observation.
///
/// A file is a partial (file-scope) observation: path-local continuity only.
/// A directory is a full observation of that root: it also infers moves and
/// records disappearances against the previous full observation of the same
/// root. Spaces must already exist and apply to every ingested file.
pub fn ingest_path<S: AsRef<str>>(
    store: &dyn Store,
    path: &Path,
    space_names: &[S],
) -> Result<ObservationReport> {
    let canonical = std::fs::canonicalize(path).map_err(|e| Error::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    let metadata = std::fs::metadata(&canonical).map_err(|e| Error::Io {
        path: canonical.display().to_string(),
        source: e,
    })?;

    // A single file must fail on its format before spaces are resolved.
    if metadata.is_file() {
        media_type_of(&canonical)?;
    }

    // Spaces must pre-exist; the first missing one fails the ingest.
    let mut space_ids = Vec::with_capacity(space_names.len());
    for name in space_names {
        space_ids.push(store.get_space(name.as_ref())?.id);
    }

    let (root, scope, files) = if metadata.is_dir() {
        (
            canonical.display().to_string(),
            IngestScope::Directory,
            observe_directory(&canonical, &space_ids)?,
        )
    } else {
        let file = observe_file(&canonical, &space_ids)?;
        (
            canonical.display().to_string(),
            IngestScope::File,
            vec![file],
        )
    };

    let observation = IngestObservation {
        session: SessionDraft {
            root,
            scope,
            observed_at: Ts::now(),
        },
        files,
    };
    store.record_observation(observation)
}

/// Single-file ingest: a partial observation (kept for API compatibility
/// with the v0 tests and simple workflows).
pub fn ingest_file<S: AsRef<str>>(
    store: &dyn Store,
    path: &Path,
    space_names: &[S],
) -> Result<ObservationReport> {
    ingest_path(store, path, space_names)
}

/// Recursively walks one directory, deterministic order, no ignore rules,
/// symlinks skipped. Only `.md`, `.markdown`, and `.txt` files take part in
/// the observed set.
fn observe_directory(root: &Path, space_ids: &[crate::core::Uuid]) -> Result<Vec<ObservedFile>> {
    let mut paths: Vec<PathBuf> = Vec::new();
    collect_files(root, &mut paths)?;
    paths.sort();
    paths.iter().map(|p| observe_file(p, space_ids)).collect()
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(dir).map_err(|e| Error::Io {
        path: dir.display().to_string(),
        source: e,
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::Io {
            path: dir.display().to_string(),
            source: e,
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| Error::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        // Deterministic and cycle-safe: symlinks (files and directories)
        // are skipped entirely.
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_files(&path, out)?;
        } else if is_ingestable(&path) {
            out.push(path);
        }
    }
    Ok(())
}

fn is_ingestable(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("md" | "markdown" | "txt")
    )
}

fn media_type_of(path: &Path) -> Result<&'static str> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("md" | "markdown") => Ok("text/markdown"),
        Some("txt") => Ok("text/plain"),
        _ => Err(Error::UnsupportedFormat {
            path: path.display().to_string(),
        }),
    }
}

/// Reads and deterministically parses one file into an observed file.
fn observe_file(path: &Path, space_ids: &[crate::core::Uuid]) -> Result<ObservedFile> {
    let media_type = media_type_of(path)?;
    let bytes = std::fs::read(path).map_err(|e| Error::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    let text = String::from_utf8(bytes.clone()).map_err(|_| Error::InvalidEncoding {
        path: path.display().to_string(),
    })?;
    let location = std::fs::canonicalize(path)
        .map_err(|e| Error::Io {
            path: path.display().to_string(),
            source: e,
        })?
        .display()
        .to_string();

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled")
        .to_string();

    let (title, fragments) = if media_type == "text/markdown" {
        let parsed = markdown::parse_markdown(&text, &stem);
        (parsed.title, parsed.fragments)
    } else {
        (stem.clone(), markdown::parse_text(&text, &stem))
    };

    let mut evidences = Vec::with_capacity(fragments.len());
    let mut assertions = Vec::with_capacity(fragments.len());
    let mut memories = Vec::with_capacity(fragments.len());
    for (i, f) in fragments.iter().enumerate() {
        evidences.push(EvidenceDraft {
            kind: f.kind,
            heading_path: f.heading_path.clone(),
            start_line: f.start_line,
            end_line: f.end_line,
            ordinal: i as u32,
            content: f.content.clone(),
            content_hash: sha256_hex(f.content.as_bytes()),
        });
        assertions.push(AssertionDraft {
            evidence_idx: i,
            kind: crate::core::AssertionKind::Claim,
            subject: f.subject.clone(),
            subject_normalized: normalize_subject(&f.subject),
            predicate: None,
            content: f.content.clone(),
            extraction: f.extraction.to_string(),
        });
        memories.push(MemorySpec {
            assertion_idx: i,
            spaces: space_ids.to_vec(),
        });
    }

    Ok(ObservedFile {
        location,
        content: text,
        content_hash: sha256_hex(&bytes),
        title,
        media_type: media_type.to_string(),
        size_bytes: bytes.len() as u64,
        evidences,
        assertions,
        memories,
    })
}
