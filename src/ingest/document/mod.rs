//! Explicit ingestion of Markdown and plain-text DOCUMENTS (M8 S2).
//!
//! This is NOT the legacy `ingest_path`. It stores a document (a logical source of kind `document`),
//! its revision (the canonical text), one extraction (original-bytes hash + detector + extractor +
//! parameters) and its structure nodes, through the S1 model, and nothing else: **no evidence, no
//! assertions, no memories, no embeddings, no search index entries**. It is reached only by calling
//! [`ingest_document_file`]; the directory ingest, the file ingest, the CLI and the MCP tools do not
//! call it.
//!
//! Coexistence with legacy ingestion (docs/m8-s2-text-documents.md): the same file may be a legacy
//! `file` source (with `file:legacy` memories) and a `document` source at once. They are different
//! logical sources that never reconcile with each other, and the document path creates no memory, so
//! two competing active memory sets cannot arise. Existing legacy memories are never rewritten or
//! retired by this path, and old Markdown/text sources are not migrated.

pub mod detect;
pub mod structure;

use std::io::Read;
use std::path::Path;

use crate::core::{
    params_hash, sha256_hex, Anchor, AnchorPrecision, DocNodeDraft, DocumentExtractionDraft,
    DocumentExtractionReport, DocumentRefusal, Error, Result, Ts, MAX_NODES,
};
use crate::storage::Store;

pub use detect::{detect, Detection, Dialect, DialectCertainty, DETECTOR_VERSION};
pub use structure::MAX_STRUCT_DEPTH;

/// Largest file the text adapter reads.
pub const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;
/// Part of every extraction identity this adapter produces; bumped when its output changes.
pub const EXTRACTOR_VERSION: &str = "1";

/// The versions that, with the bytes and the parameters, identify an extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub detector_version: String,
    pub extractor_version: String,
}

impl Default for Identity {
    fn default() -> Self {
        Identity {
            detector_version: DETECTOR_VERSION.to_string(),
            extractor_version: EXTRACTOR_VERSION.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DocumentIngestOptions {
    /// Refuse files larger than this (never truncate).
    pub max_input_bytes: u64,
    /// Keep the original bytes (content-addressed). Off by default: only the canonical text is kept.
    pub retain_original: bool,
}

impl Default for DocumentIngestOptions {
    fn default() -> Self {
        DocumentIngestOptions {
            max_input_bytes: MAX_INPUT_BYTES,
            retain_original: false,
        }
    }
}

/// A validated extraction that has not been stored yet.
#[derive(Debug, Clone)]
pub struct Extraction {
    pub draft: DocumentExtractionDraft,
    pub detection: Detection,
}

/// What an ingestion did.
#[derive(Debug, Clone)]
pub struct DocumentIngestReport {
    pub persisted: DocumentExtractionReport,
    pub detection: Detection,
    /// SHA-256 of the original bytes.
    pub source_bytes_hash: String,
    /// SHA-256 of the canonical text (the revision's identity).
    pub canonical_hash: String,
    pub extractor_id: String,
    pub extractor_version: String,
    pub detector_version: String,
    pub params_hash: String,
    pub location: String,
    /// The structure that was stored.
    pub nodes: Vec<DocNodeDraft>,
}

impl DocumentIngestReport {
    /// A citation anchor for a node (or a range inside it), bound to THIS extraction. Precision
    /// `byte_exact` needs a range; the store checks the claim when the anchor is resolved.
    pub fn anchor(
        &self,
        path: &[u32],
        range: Option<(u64, u64)>,
        precision: AnchorPrecision,
    ) -> Result<Anchor> {
        Anchor::new(
            self.persisted.logical_id,
            &self.source_bytes_hash,
            &self.detector_version,
            &self.extractor_id,
            &self.extractor_version,
            &self.params_hash,
            path.to_vec(),
            range,
            None,
            None,
            precision,
        )
    }
}

fn io_err(path: &Path, e: std::io::Error) -> Error {
    Error::Io {
        path: path.display().to_string(),
        source: e,
    }
}

fn refused<T>(reason: DocumentRefusal, detail: impl Into<String>) -> Result<T> {
    Err(Error::DocumentRefused {
        reason,
        detail: detail.into(),
    })
}

/// Builds the validated draft for bytes already in memory. Pure and deterministic: no clock except
/// `observed_at`, which is recorded and takes no part in any identity.
pub fn extract_document_bytes(
    location: &str,
    bytes: &[u8],
    hint: Option<&str>,
    options: &DocumentIngestOptions,
    identity: &Identity,
    observed_at: Ts,
) -> Result<Extraction> {
    if bytes.len() as u64 > options.max_input_bytes {
        return refused(
            DocumentRefusal::TooLarge,
            format!(
                "{} bytes exceed the limit of {}",
                bytes.len(),
                options.max_input_bytes
            ),
        );
    }
    let (text, detection) = detect::detect(bytes, hint)?;
    let structure = match detection.dialect {
        Dialect::Markdown => structure::markdown(&text, MAX_NODES)?,
        Dialect::Plain => structure::plain(&text, MAX_NODES)?,
    };
    let title = structure
        .first_heading
        .clone()
        .filter(|t| !t.is_empty())
        .or_else(|| {
            Path::new(location)
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "untitled".to_string());
    let title: String = title.chars().take(200).collect();
    let (extractor_id, media_type) = match detection.dialect {
        Dialect::Markdown => ("markdown", "text/markdown"),
        Dialect::Plain => ("plaintext", "text/plain"),
    };
    // everything that changes what the extractor produces, and nothing else
    let params = params_hash(&serde_json::json!({
        "dialect": detection.dialect.as_str(),
        "bom_stripped": detection.bom_stripped,
        "max_struct_depth": MAX_STRUCT_DEPTH,
    }));
    let draft = DocumentExtractionDraft {
        location: location.to_string(),
        title,
        media_type: media_type.to_string(),
        format: detection.physical.to_string(),
        format_hint: detection.hint.clone(),
        detector_version: identity.detector_version.clone(),
        extractor_id: extractor_id.to_string(),
        extractor_version: identity.extractor_version.clone(),
        params_hash: params,
        source_bytes_hash: sha256_hex(bytes),
        source_size: bytes.len() as u64,
        // with a BOM the canonical text is not the original file: offsets differ by its length
        text_is_source: !detection.bom_stripped,
        canonical_text: text,
        nodes: structure.nodes,
        retain_bytes: options.retain_original.then(|| bytes.to_vec()),
        observed_at,
    };
    draft.validate()?;
    Ok(Extraction { draft, detection })
}

/// Reads at most `limit` bytes of a regular file; a larger file is refused, never truncated.
fn read_regular_file(path: &Path, limit: u64) -> Result<(String, Vec<u8>)> {
    let canonical = std::fs::canonicalize(path).map_err(|e| io_err(path, e))?;
    let location = match canonical.to_str() {
        Some(l) => l.to_string(),
        None => {
            return refused(
                DocumentRefusal::UnsupportedPath,
                "the path is not valid UTF-8",
            )
        }
    };
    // Look BEFORE opening: opening a named pipe for reading blocks until a writer appears. The
    // open itself is non-blocking as well, for a path that changes between the look and the open.
    let before = std::fs::metadata(&canonical).map_err(|e| io_err(path, e))?;
    if !before.is_file() {
        return refused(
            DocumentRefusal::NotARegularFile,
            format!("{} is not a regular file", canonical.display()),
        );
    }
    let mut open = std::fs::OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = open.open(&canonical).map_err(|e| io_err(path, e))?;
    let meta = file.metadata().map_err(|e| io_err(path, e))?;
    if !meta.is_file() {
        return refused(
            DocumentRefusal::NotARegularFile,
            format!("{} is not a regular file", canonical.display()),
        );
    }
    if meta.len() > limit {
        return refused(
            DocumentRefusal::TooLarge,
            format!("{} bytes exceed the limit of {limit}", meta.len()),
        );
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_err(path, e))?;
    if bytes.len() as u64 > limit {
        return refused(
            DocumentRefusal::TooLarge,
            format!("the file grew beyond the limit of {limit} bytes while it was read"),
        );
    }
    Ok((location, bytes))
}

/// Ingests one Markdown or plain-text file as a document. The only entry point of this slice.
///
/// One call is one atomic store write (one recording revision); an identical repeat writes nothing.
/// Every refusal happens before any write.
pub fn ingest_document_file(
    store: &dyn Store,
    path: &Path,
    options: &DocumentIngestOptions,
) -> Result<DocumentIngestReport> {
    ingest_document_file_as(store, path, options, &Identity::default())
}

/// [`ingest_document_file`] with explicit detector/extractor versions (tests of identity handling).
#[doc(hidden)]
pub fn ingest_document_file_as(
    store: &dyn Store,
    path: &Path,
    options: &DocumentIngestOptions,
    identity: &Identity,
) -> Result<DocumentIngestReport> {
    let (location, bytes) = read_regular_file(path, options.max_input_bytes)?;
    let hint = path.extension().and_then(|e| e.to_str());
    let Extraction { draft, detection } =
        extract_document_bytes(&location, &bytes, hint, options, identity, Ts::now())?;
    let nodes = draft.nodes.clone();
    let report = DocumentIngestReport {
        persisted: DocumentExtractionReport {
            extraction_id: crate::core::Uuid::nil(),
            logical_id: crate::core::Uuid::nil(),
            source_id: crate::core::Uuid::nil(),
            document_created: false,
            revision_created: false,
            extraction_created: false,
            nodes: 0,
            bytes_retained_now: false,
            bytes_retained: false,
        },
        detection,
        source_bytes_hash: draft.source_bytes_hash.clone(),
        canonical_hash: draft.text_hash(),
        extractor_id: draft.extractor_id.clone(),
        extractor_version: draft.extractor_version.clone(),
        detector_version: draft.detector_version.clone(),
        params_hash: draft.params_hash.clone(),
        location: draft.location.clone(),
        nodes,
    };
    let persisted = store.record_document_extraction(draft)?;
    Ok(DocumentIngestReport {
        persisted,
        ..report
    })
}
