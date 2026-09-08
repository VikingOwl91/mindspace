pub mod markdown;

use std::path::Path;

use crate::core::{
    normalize_subject, sha256_hex, AssertionDraft, Error, EvidenceDraft, IngestBundle,
    IngestRecord, MemorySpec, Result, SourceDraft, SourceKind, Ts,
};
use crate::storage::Store;

/// Ingests one file into the store: deterministic parse, evidence and
/// assertion drafts, requested spaces, and the reconciliation transaction.
/// Spaces must already exist.
pub fn ingest_file<S: AsRef<str>>(
    store: &dyn Store,
    path: &Path,
    space_names: &[S],
) -> Result<IngestRecord> {
    let media_type = match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("md" | "markdown") => "text/markdown",
        Some("txt") => "text/plain",
        _ => {
            return Err(Error::UnsupportedFormat {
                path: path.display().to_string(),
            })
        }
    };

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

    // Spaces must pre-exist; first missing one fails the ingest.
    let mut space_ids = Vec::with_capacity(space_names.len());
    for name in space_names {
        space_ids.push(store.get_space(name.as_ref())?.id);
    }

    let source = SourceDraft {
        kind: SourceKind::File,
        location,
        title,
        content: text,
        content_hash: sha256_hex(&bytes),
        media_type: media_type.to_string(),
        size_bytes: bytes.len() as u64,
        ingested_at: Ts::now(),
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
            spaces: space_ids.clone(),
        });
    }

    store.record_ingestion(IngestBundle {
        source,
        evidences,
        assertions,
        memories,
    })
}
