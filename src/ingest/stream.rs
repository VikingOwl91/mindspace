//! Internal record stream for extraction results (M7.3b.1).
//!
//! The extraction half of an ingest ([`super::extract`]) can run in a more
//! confined process than the one that owns the database. This module defines the
//! interface between the two: a versioned stream of **typed records** that the
//! producer writes as extraction goes, and a consumer that validates every record
//! as it arrives and only hands back an [`Extracted`] once the whole operation is
//! complete and consistent. Nothing here touches the database, starts a process
//! or is reachable from a tool; it is not used by the in-process ingest.
//!
//! # Wire format
//!
//! A stream is a sequence of frames: `u32` little-endian length, then that many
//! bytes of JSON `{"v":1,"op":"<uuid>","seq":N,"records":[...]}`. A frame carries
//! several records (small records are packed to about [`FRAME_TARGET`] bytes), so
//! the frame budget is spent on bytes, not on record count. Every record is an
//! object with a `"type"` tag; the type decides how it is decoded, there is no
//! trial decoding. Unknown types, versions, operations and sequence numbers are
//! errors.
//!
//! A record whose JSON does not fit a frame (a huge commit message or tree, an
//! assertion over a whole file, ...) is sent as `Spill{record_type,len}` followed by
//! `SpillPiece` records that carry its JSON text in pieces of at most [`CHUNK`]
//! bytes; nothing else may come in between, the declared length is exact, the
//! reassembled record must decode as the declared type, and it is then handled as
//! if it had arrived whole. Beyond [`MAX_RECORD`] bytes the operation is refused;
//! nothing is truncated.
//!
//! Limits apply to the whole operation: [`MAX_FRAME`] per frame, [`MAX_TOTAL`]
//! bytes and [`MAX_FRAMES`] frames in total. The length is checked before the
//! payload is allocated.
//!
//! # Grammar
//!
//! ```text
//! stream  = Begin  ( files | git )  End
//! files   = FilesHeader  { File [Chunk*] [Part*]  [CodeInput [Part*]] }*
//! git     = GitHeader Ref* (Commit|Tree|Tag)* Blob* RefEvent* Remote*
//!           ViewCommits* [CurrentCommits*] [CurrentTags*] body
//! body    = { BlobContent Chunk* { Derivation [Part*] }* [CodeInput [Part*]] | own }*
//! ```
//!
//! Content (file text, blob text) is sent once per blob in `Chunk` records of at
//! most [`CHUNK`] bytes and shared by the derivations and the code input that use
//! it (`content: shared`); the consumer rebuilds the full values. Large vectors
//! (assertions, entities, ...) are split into `Part` records. The sequence of
//! records is checked against the grammar; duplicates are rejected.
//!
//! The memory the consumer holds is the [`Extracted`] it is building plus one
//! content slot; the producer holds the small structural data and one batch of
//! blobs, never the full result.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::core::{
    sha256_hex, AssertionDraft, CodeContextDraft, CodeDerivationDraft, CodeEntityDraft,
    CodeImportDraft, CodeInputDraft, CodeRelationshipDraft, Error, EvidenceDraft, GitBlobData,
    GitCommitData, GitDerivedFile, GitObjectKind, GitObservation, GitRefEventDraft,
    GitRefEventKind, GitRefObservation, GitRemoteDraft, GitTagData, GitTreeData, IngestObservation,
    IngestScope, MemorySpec, ObservedFile, Result, SessionDraft, Uuid,
};

use super::deadline::Deadline;
use super::extract::{
    check_relative, is_oid, short, Extracted, ExtractedFiles, ExtractedGit, ExtractionContext,
    ExtractionLimits,
};

pub const PROTOCOL_VERSION: u32 = 1;
/// Largest frame (the JSON payload, without the 4-byte length).
pub const MAX_FRAME: usize = 1024 * 1024;
/// Largest total of frame bytes (length prefixes included) in one operation.
pub const MAX_TOTAL: u64 = 512 * 1024 * 1024;
/// Most frames in one operation.
pub const MAX_FRAMES: u64 = 100_000;
/// Producers close a frame once it reaches this size.
pub const FRAME_TARGET: usize = 512 * 1024;
/// Content is sent in pieces of at most this many bytes (JSON escaping can
/// multiply it by six, which still fits a frame).
pub const CHUNK: usize = 128 * 1024;
/// A `Part` record is split until its encoding is at most this size.
pub const PART_TARGET: usize = 256 * 1024;
/// The largest single record (as JSON). A record that does not fit a frame is sent
/// as an oversized record in pieces; beyond this size the operation is refused.
pub const MAX_RECORD: usize = 96 * 1024 * 1024;
/// The default limit for one text file or blob that extraction reads (raw bytes,
/// not encoded size). Its JSON form can be several times larger (escaping), which
/// is why [`MAX_RECORD`] is three times this.
pub const DEFAULT_MAX_BLOB: u64 = 32 * 1024 * 1024;

/// What a producer refuses to read.
#[derive(Debug, Clone, Copy)]
pub struct ProducerLimits {
    pub max_blob_bytes: u64,
}

impl Default for ProducerLimits {
    fn default() -> Self {
        Self {
            max_blob_bytes: DEFAULT_MAX_BLOB,
        }
    }
}
/// Blob contents are fetched from Git in batches of at most this many bytes.
const BLOB_BATCH_BYTES: u64 = 8 * 1024 * 1024;
const BLOB_BATCH_COUNT: usize = 512;
/// Structural objects are read from Git in chunks of this many ids.
const OBJECT_CHUNK: usize = 4000;

fn bad<T>(why: impl Into<String>) -> Result<T> {
    Err(Error::InvalidExtraction(format!(
        "record stream: {}",
        why.into()
    )))
}

// ------------------------------------------------------------------ records

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Files,
    Git,
}

/// Where the content of a derivation or code input comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentSource {
    /// `Chunk` records follow this record and make up its content.
    Own { len: u64 },
    /// The content of the current slot (the last `BlobContent`, or the last
    /// record with own content).
    Shared,
}

/// The pieces of the vectors that would make a record too big.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "part", rename_all = "snake_case")]
pub enum Part {
    Evidences { items: Vec<EvidenceDraft> },
    Assertions { items: Vec<AssertionDraft> },
    Memories { items: Vec<MemorySpec> },
    Entities { items: Vec<CodeEntityDraft> },
    Imports { items: Vec<CodeImportDraft> },
    Relationships { items: Vec<CodeRelationshipDraft> },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub records: u64,
    pub files: u64,
    pub refs: u64,
    pub commits: u64,
    pub trees: u64,
    pub tags: u64,
    pub blobs: u64,
    pub derivations: u64,
    pub code_inputs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Record {
    Begin {
        kind: Kind,
    },
    // ---- files
    FilesHeader {
        session: SessionDraft,
        spaces: Vec<Uuid>,
    },
    /// `file.content` and its vectors are empty; `Chunk` and `Part` records follow.
    File {
        file: ObservedFile,
        content_len: u64,
    },
    // ---- git
    GitHeader {
        session: SessionDraft,
        repository_id: Uuid,
        repository_path: String,
        spaces: Vec<Uuid>,
    },
    Ref {
        item: GitRefObservation,
    },
    Commit {
        item: GitCommitData,
    },
    Tree {
        item: GitTreeData,
    },
    Tag {
        item: GitTagData,
    },
    /// Without content; `BlobContent` fills it later for blobs with derivations.
    Blob {
        item: GitBlobData,
    },
    RefEvent {
        item: GitRefEventDraft,
    },
    Remote {
        item: GitRemoteDraft,
    },
    ViewCommits {
        oids: Vec<String>,
    },
    CurrentCommits {
        oids: Vec<String>,
    },
    CurrentTags {
        pairs: Vec<(String, String)>,
    },
    /// Opens the content slot of one blob (which must have been announced by a
    /// `Blob` record); `Chunk` records follow.
    BlobContent {
        oid: String,
        len: u64,
    },
    /// `file.content` and its vectors are empty.
    Derivation {
        file: GitDerivedFile,
        content: ContentSource,
    },
    /// `input.content` and the derivation vectors are empty.
    CodeInput {
        input: CodeInputDraft,
        content: ContentSource,
    },
    // ---- both
    Chunk {
        data: String,
    },
    Part(Part),
    /// Announces a record too big for a frame: `len` bytes of its JSON follow in
    /// `SpillPiece` records, nothing else in between.
    Spill {
        record_type: String,
        len: u64,
    },
    SpillPiece {
        data: String,
    },
    End {
        counts: Counts,
    },
}

impl Record {
    fn name(&self) -> &'static str {
        match self {
            Record::Begin { .. } => "begin",
            Record::FilesHeader { .. } => "files_header",
            Record::File { .. } => "file",
            Record::GitHeader { .. } => "git_header",
            Record::Ref { .. } => "ref",
            Record::Commit { .. } => "commit",
            Record::Tree { .. } => "tree",
            Record::Tag { .. } => "tag",
            Record::Blob { .. } => "blob",
            Record::RefEvent { .. } => "ref_event",
            Record::Remote { .. } => "remote",
            Record::ViewCommits { .. } => "view_commits",
            Record::CurrentCommits { .. } => "current_commits",
            Record::CurrentTags { .. } => "current_tags",
            Record::BlobContent { .. } => "blob_content",
            Record::Derivation { .. } => "derivation",
            Record::CodeInput { .. } => "code_input",
            Record::Chunk { .. } => "chunk",
            Record::Part(_) => "part",
            Record::Spill { .. } => "spill",
            Record::SpillPiece { .. } => "spill_piece",
            Record::End { .. } => "end",
        }
    }
}

// ------------------------------------------------------------------- writer

/// Where a producer sends records.
pub trait RecordSink {
    /// Sends one record already encoded as JSON. `name` is the record's type.
    fn emit_encoded(&mut self, name: &'static str, json: Vec<u8>) -> Result<()>;

    fn emit(&mut self, record: &Record) -> Result<()> {
        let json = serde_json::to_vec(record)
            .map_err(|e| Error::InvalidExtraction(format!("record stream: encode: {e}")))?;
        self.emit_encoded(record.name(), json)
    }

    /// Sends one piece of content (a `chunk` record).
    fn emit_chunk(&mut self, piece: &str) -> Result<()> {
        self.emit(&Record::Chunk {
            data: piece.to_string(),
        })
    }
}

/// Writes frames to a byte stream and enforces the operation's budget.
pub struct FrameWriter<'a, W: Write> {
    out: W,
    op: Uuid,
    seq: u64,
    buf: Vec<u8>,
    in_buf: usize,
    frames: u64,
    bytes: u64,
    counts: Counts,
    cancel: Option<&'a AtomicBool>,
    finished: bool,
}

impl<'a, W: Write> FrameWriter<'a, W> {
    pub fn new(out: W, op: Uuid, cancel: Option<&'a AtomicBool>) -> Self {
        let mut w = Self {
            out,
            op,
            seq: 0,
            buf: Vec::with_capacity(FRAME_TARGET + 4096),
            in_buf: 0,
            frames: 0,
            bytes: 0,
            counts: Counts::default(),
            cancel,
            finished: false,
        };
        w.start_frame();
        w
    }

    fn start_frame(&mut self) {
        self.buf.clear();
        self.buf.extend_from_slice(
            format!(
                "{{\"v\":{PROTOCOL_VERSION},\"op\":\"{}\",\"seq\":{},\"records\":[",
                self.op, self.seq
            )
            .as_bytes(),
        );
        self.in_buf = 0;
    }

    fn flush_frame(&mut self) -> Result<()> {
        if self.in_buf == 0 {
            return Ok(());
        }
        if let Some(c) = self.cancel {
            if c.load(Ordering::Relaxed) {
                return bad("cancelled");
            }
        }
        self.buf.extend_from_slice(b"]}");
        if self.buf.len() > MAX_FRAME {
            return bad(format!(
                "a frame of {} bytes exceeds {MAX_FRAME}",
                self.buf.len()
            ));
        }
        self.frames += 1;
        self.bytes += self.buf.len() as u64 + 4;
        if self.frames > MAX_FRAMES {
            return bad(format!("more than {MAX_FRAMES} frames"));
        }
        if self.bytes > MAX_TOTAL {
            return bad(format!("more than {MAX_TOTAL} bytes in one operation"));
        }
        let io = |e: std::io::Error| Error::Io {
            path: "record stream".into(),
            source: e,
        };
        self.out
            .write_all(&(self.buf.len() as u32).to_le_bytes())
            .map_err(io)?;
        self.out.write_all(&self.buf).map_err(io)?;
        self.seq += 1;
        self.start_frame();
        Ok(())
    }

    /// Writes the closing record and flushes. Call exactly once.
    pub fn finish(mut self) -> Result<WriterStats> {
        if !self.finished {
            let mut counts = self.counts.clone();
            counts.records += 1;
            self.emit(&Record::End { counts })?;
            self.finished = true;
        }
        self.flush_frame()?;
        self.out.flush().map_err(|e| Error::Io {
            path: "record stream".into(),
            source: e,
        })?;
        Ok(WriterStats {
            frames: self.frames,
            bytes: self.bytes,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WriterStats {
    pub frames: u64,
    pub bytes: u64,
}

impl<W: Write> FrameWriter<'_, W> {
    /// Appends one record, written by `write` straight into the frame buffer (no
    /// intermediate allocation or copy). If it does not fit the current frame, the
    /// frame is closed first and the record is written again into a new one.
    fn push_in_place(
        &mut self,
        name: &'static str,
        write: &dyn Fn(&mut Vec<u8>) -> serde_json::Result<()>,
    ) -> Result<()> {
        let start = self.buf.len();
        if self.in_buf > 0 {
            self.buf.push(b',');
        }
        let body = self.buf.len();
        write(&mut self.buf)
            .map_err(|e| Error::InvalidExtraction(format!("record stream: encode: {e}")))?;
        let size = self.buf.len() - body;
        if size + 128 > MAX_FRAME {
            let json = self.buf[body..].to_vec();
            self.buf.truncate(start);
            return self.spill(name, &json);
        }
        if self.in_buf > 0 && self.buf.len() + 3 > FRAME_TARGET {
            self.buf.truncate(start);
            self.flush_frame()?;
            return self.push_in_place(name, write);
        }
        self.in_buf += 1;
        self.count(name);
        Ok(())
    }

    /// Sends a record that does not fit a frame as `Spill` + pieces of its JSON.
    fn spill(&mut self, name: &'static str, json: &[u8]) -> Result<()> {
        if json.len() > MAX_RECORD {
            return bad(format!(
                "a `{name}` record of {} bytes exceeds the largest record ({MAX_RECORD})",
                json.len()
            ));
        }
        // JSON text is valid UTF-8; pieces are cut at character boundaries.
        let text = std::str::from_utf8(json).map_err(|_| {
            Error::InvalidExtraction("record stream: encoded record is not UTF-8".into())
        })?;
        let spill = Record::Spill {
            record_type: name.to_string(),
            len: json.len() as u64,
        };
        self.push_in_place("spill", &|b| serde_json::to_writer(b, &spill))?;
        let mut rest = text;
        while !rest.is_empty() {
            let mut end = rest.len().min(CHUNK);
            while !rest.is_char_boundary(end) {
                end -= 1;
            }
            let (piece, tail) = rest.split_at(end);
            self.push_in_place("spill_piece", &|b| {
                b.extend_from_slice(b"{\"type\":\"spill_piece\",\"data\":");
                serde_json::to_writer(&mut *b, piece)?;
                b.push(b'}');
                Ok(())
            })?;
            rest = tail;
        }
        self.count(name);
        Ok(())
    }

    fn count(&mut self, name: &'static str) {
        if name == "spill" || name == "spill_piece" {
            return;
        }
        let c = &mut self.counts;
        c.records += 1;
        match name {
            "file" => c.files += 1,
            "ref" => c.refs += 1,
            "commit" => c.commits += 1,
            "tree" => c.trees += 1,
            "tag" => c.tags += 1,
            "blob" => c.blobs += 1,
            "derivation" => c.derivations += 1,
            "code_input" => c.code_inputs += 1,
            _ => {}
        }
    }
}

impl<W: Write> RecordSink for FrameWriter<'_, W> {
    fn emit(&mut self, record: &Record) -> Result<()> {
        self.push_in_place(record.name(), &|b| serde_json::to_writer(b, record))
    }

    fn emit_chunk(&mut self, piece: &str) -> Result<()> {
        self.push_in_place("chunk", &|b| {
            b.extend_from_slice(b"{\"type\":\"chunk\",\"data\":");
            serde_json::to_writer(&mut *b, piece)?;
            b.push(b'}');
            Ok(())
        })
    }

    fn emit_encoded(&mut self, name: &'static str, json: Vec<u8>) -> Result<()> {
        // A record that cannot fit a frame of its own is sent in pieces.
        if json.len() + 128 > MAX_FRAME {
            return self.spill(name, &json);
        }
        if self.in_buf > 0 && self.buf.len() + json.len() + 3 > FRAME_TARGET {
            self.flush_frame()?;
        }
        if self.in_buf > 0 {
            self.buf.push(b',');
        }
        self.buf.extend_from_slice(&json);
        self.in_buf += 1;
        self.count(name);
        Ok(())
    }
}

fn emit_chunks(sink: &mut dyn RecordSink, content: &str) -> Result<()> {
    let mut rest = content;
    while !rest.is_empty() {
        let mut end = rest.len().min(CHUNK);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (piece, tail) = rest.split_at(end);
        sink.emit_chunk(piece)?;
        rest = tail;
    }
    Ok(())
}

/// Sends `items` as `Part` records, splitting until each one is small enough.
fn emit_parts<T: Serialize>(
    sink: &mut dyn RecordSink,
    items: Vec<T>,
    make: fn(Vec<T>) -> Part,
) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let record = Record::Part(make(items));
    let json = serde_json::to_vec(&record)
        .map_err(|e| Error::InvalidExtraction(format!("record stream: encode: {e}")))?;
    if json.len() <= PART_TARGET {
        return sink.emit_encoded("part", json);
    }
    // Too big: split in halves (a single item that is still too big is reported
    // by the sink).
    let Record::Part(part) = record else {
        unreachable!()
    };
    macro_rules! halve {
        ($items:expr, $variant:ident) => {{
            let mut a = $items;
            if a.len() < 2 {
                return sink.emit_encoded("part", json);
            }
            let b = a.split_off(a.len() / 2);
            emit_parts(sink, a, |items| Part::$variant { items })?;
            emit_parts(sink, b, |items| Part::$variant { items })
        }};
    }
    match part {
        Part::Evidences { items } => halve!(items, Evidences),
        Part::Assertions { items } => halve!(items, Assertions),
        Part::Memories { items } => halve!(items, Memories),
        Part::Entities { items } => halve!(items, Entities),
        Part::Imports { items } => halve!(items, Imports),
        Part::Relationships { items } => halve!(items, Relationships),
    }
}

fn emit_file(sink: &mut dyn RecordSink, mut file: ObservedFile) -> Result<()> {
    let content = std::mem::take(&mut file.content);
    let (ev, asr, mem) = (
        std::mem::take(&mut file.evidences),
        std::mem::take(&mut file.assertions),
        std::mem::take(&mut file.memories),
    );
    sink.emit(&Record::File {
        file,
        content_len: content.len() as u64,
    })?;
    emit_chunks(sink, &content)?;
    emit_parts(sink, ev, |items| Part::Evidences { items })?;
    emit_parts(sink, asr, |items| Part::Assertions { items })?;
    emit_parts(sink, mem, |items| Part::Memories { items })
}

fn emit_derivation(
    sink: &mut dyn RecordSink,
    mut file: GitDerivedFile,
    shared: bool,
) -> Result<()> {
    let content = std::mem::take(&mut file.content);
    let (ev, asr, mem) = (
        std::mem::take(&mut file.evidences),
        std::mem::take(&mut file.assertions),
        std::mem::take(&mut file.memories),
    );
    let source = if shared {
        ContentSource::Shared
    } else {
        ContentSource::Own {
            len: content.len() as u64,
        }
    };
    sink.emit(&Record::Derivation {
        file,
        content: source,
    })?;
    if !shared {
        emit_chunks(sink, &content)?;
    }
    emit_parts(sink, ev, |items| Part::Evidences { items })?;
    emit_parts(sink, asr, |items| Part::Assertions { items })?;
    emit_parts(sink, mem, |items| Part::Memories { items })
}

fn emit_code_input(
    sink: &mut dyn RecordSink,
    mut input: CodeInputDraft,
    shared: bool,
) -> Result<()> {
    let content = std::mem::take(&mut input.content);
    let (en, im, re) = (
        std::mem::take(&mut input.derivation.entities),
        std::mem::take(&mut input.derivation.imports),
        std::mem::take(&mut input.derivation.relationships),
    );
    let source = if shared {
        ContentSource::Shared
    } else {
        ContentSource::Own {
            len: content.len() as u64,
        }
    };
    sink.emit(&Record::CodeInput {
        input,
        content: source,
    })?;
    if !shared {
        emit_chunks(sink, &content)?;
    }
    emit_parts(sink, en, |items| Part::Entities { items })?;
    emit_parts(sink, im, |items| Part::Imports { items })?;
    emit_parts(sink, re, |items| Part::Relationships { items })
}

// ------------------------------------------------- producers (incremental)

/// Streams the extraction of an anchored directory (the counterpart of
/// [`super::confined::extract_directory`], same files, same order, same parsing)
/// without building the full result.
pub fn stream_directory(
    ctx: &dyn ExtractionContext,
    anchor: &super::confined::Anchor,
    policy: &dyn super::confined::PathPolicy,
    space_ids: &[Uuid],
    deadline: &Deadline,
    limits: &ProducerLimits,
    sink: &mut dyn RecordSink,
) -> Result<()> {
    use super::outline;
    sink.emit(&Record::Begin { kind: Kind::Files })?;
    sink.emit(&Record::FilesHeader {
        session: SessionDraft {
            root: anchor.real().display().to_string(),
            scope: IngestScope::Directory,
            observed_at: crate::core::Ts::now(),
        },
        spaces: space_ids.to_vec(),
    })?;
    let base = anchor.proc_path();
    let mut paths: Vec<std::path::PathBuf> = outline::walk_files(&base)?
        .into_iter()
        .filter(|p| super::is_ingestable(p))
        .collect();
    paths.sort();
    for path in paths {
        deadline.check("reading files")?;
        let rel = path
            .strip_prefix(&base)
            .map_err(|_| Error::InvalidExtraction("walk escaped the anchored directory".into()))?
            .to_path_buf();
        // The size is checked before the file is read, and the read itself is capped
        // (the file may grow in between).
        let too_large = |size: u64| Error::BlobTooLarge {
            path: rel.display().to_string(),
            size,
            limit: limits.max_blob_bytes,
        };
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > limits.max_blob_bytes {
                return Err(too_large(meta.len()));
            }
        }
        let bytes = anchor
            .read_beneath(&rel, policy, true, Some(limits.max_blob_bytes as usize))
            .map_err(|e| match e {
                Error::Storage(m) if m.ends_with("is unexpectedly large") => {
                    too_large(limits.max_blob_bytes + 1)
                }
                other => other,
            })?
            .ok_or_else(|| {
                Error::InvalidExtraction(format!(
                    "{} disappeared while it was being read",
                    rel.display()
                ))
            })?;
        let location = anchor.real().join(&rel).display().to_string();
        let file = super::observe_bytes(&rel, bytes, location, space_ids)?;
        let code = super::code_input_for_file(ctx, &file, deadline)?;
        emit_file(sink, file)?;
        if let Some(input) = code {
            emit_code_input(sink, input, true)?;
        }
    }
    Ok(())
}

/// Streams the extraction of an anchored Git repository (the counterpart of
/// [`super::confined::extract_confined_git`]).
#[allow(clippy::too_many_arguments)]
pub fn stream_confined_git<S: AsRef<str>>(
    ctx: &dyn ExtractionContext,
    anchor: &super::confined::Anchor,
    policy: &dyn super::confined::PathPolicy,
    selection: &super::outline::PathSelection,
    scope: super::GitScope,
    space_names: &[S],
    deadline: &Deadline,
    limits: &ProducerLimits,
    sink: &mut dyn RecordSink,
) -> Result<()> {
    let reader = super::confined::open_git_reader(anchor, policy, true, deadline)?;
    stream_git_with_reader(
        ctx,
        reader,
        anchor.real(),
        space_names,
        selection,
        scope,
        deadline,
        limits,
        sink,
    )
}

/// The streaming twin of [`super::extract_git_with_reader`]: the same steps in the
/// same order with the same results, but blob contents are fetched in batches and
/// sent as soon as their derivations and code input are ready, and structural
/// objects are read in chunks.
#[allow(clippy::too_many_arguments)]
pub fn stream_git_with_reader<S: AsRef<str>>(
    ctx: &dyn ExtractionContext,
    reader: super::git::GitReader,
    repo_path: &Path,
    space_names: &[S],
    selection: &super::outline::PathSelection,
    scope: super::GitScope,
    deadline: &Deadline,
    limits: &ProducerLimits,
    sink: &mut dyn RecordSink,
) -> Result<()> {
    use super::{code, is_ingestable, media_type_of, parse_material, ParsedMaterial};
    let repository_id = reader.repository_id()?;
    let space_ids = super::resolve_spaces_with(ctx, space_names)?;
    let refs = reader.refs()?;
    let views = match scope {
        super::GitScope::Views(v) => v,
        _ => super::GitViews::default(),
    };
    let worktree_heads: Vec<String> = match scope {
        super::GitScope::History => Vec::new(),
        _ => reader.worktree_heads()?,
    };
    let selected: Vec<&GitRefObservation> = refs
        .iter()
        .filter(|r| match scope {
            super::GitScope::History => true,
            _ => {
                r.name == "HEAD"
                    || (views.branches && r.name.starts_with("refs/heads/"))
                    || (views.remotes && r.name.starts_with("refs/remotes/"))
                    || (views.tags && r.name.starts_with("refs/tags/"))
            }
        })
        .collect();
    let mut tips: Vec<String> = selected.iter().map(|r| r.target_oid.clone()).collect();
    tips.extend(worktree_heads.iter().cloned());
    tips.sort();
    tips.dedup();
    if tips.is_empty() && !refs.is_empty() {
        return Err(Error::GitCommand {
            command: "HEAD".into(),
            stderr: "HEAD does not resolve to a commit; use --history to observe all refs".into(),
        });
    }
    deadline.check("discovering git objects")?;
    let reachable = reader.reachable_oids(&tips)?;
    deadline.check("checking git objects")?;
    let checked = reader.batch_check(&reachable)?;
    drop(reachable);
    let mut structural: Vec<String> = Vec::new();
    let mut blob_sizes: Vec<(String, u64)> = Vec::new();
    for (oid, kind, size) in checked {
        if kind == GitObjectKind::Blob {
            blob_sizes.push((oid, size));
        } else {
            structural.push(oid);
        }
    }
    let size_of: HashMap<String, u64> = blob_sizes.iter().cloned().collect();

    // ---- the header, refs
    sink.emit(&Record::Begin { kind: Kind::Git })?;
    sink.emit(&Record::GitHeader {
        session: SessionDraft {
            root: repo_path.display().to_string(),
            scope: IngestScope::GitRepository,
            observed_at: crate::core::Ts::now(),
        },
        repository_id,
        repository_path: repo_path.display().to_string(),
        spaces: space_ids.clone(),
    })?;
    for r in &refs {
        sink.emit(&Record::Ref { item: r.clone() })?;
    }

    // ---- structural objects, in chunks; tags are kept for peeling
    deadline.check("reading git objects")?;
    let mut tags: Vec<GitTagData> = Vec::new();
    let mut known_commits: HashSet<String> = HashSet::new();
    for chunk in structural.chunks(OBJECT_CHUNK) {
        deadline.check("reading git objects")?;
        let (commits, trees, chunk_tags) = reader.objects(chunk)?;
        for c in commits {
            known_commits.insert(c.oid.clone());
            sink.emit(&Record::Commit { item: c })?;
        }
        for t in trees {
            sink.emit(&Record::Tree { item: t })?;
        }
        for t in chunk_tags {
            sink.emit(&Record::Tag { item: t.clone() })?;
            tags.push(t);
        }
    }
    drop(structural);

    // ---- derivation tips
    let mut tip_commits: BTreeSet<String> = selected
        .iter()
        .filter(|r| r.target_kind == GitObjectKind::Commit)
        .map(|r| r.target_oid.clone())
        .collect();
    tip_commits.extend(worktree_heads.iter().cloned());
    let tag_by_oid: HashMap<&str, &GitTagData> = tags.iter().map(|t| (t.oid.as_str(), t)).collect();
    let mut peel_queue: Vec<String> = tags
        .iter()
        .filter(|t| t.target_kind == GitObjectKind::Commit)
        .map(|t| t.target_oid.clone())
        .collect();
    while let Some(oid) = peel_queue.pop() {
        tip_commits.insert(oid.clone());
        if let Some(t) = tag_by_oid.get(oid.as_str()) {
            if t.target_kind == GitObjectKind::Tag {
                peel_queue.push(t.target_oid.clone());
            }
        }
    }

    // ---- which blobs are derived, at which commit and path
    let mut keys: Vec<(String, String, String)> = Vec::new(); // (commit, path, blob)
    for commit in &tip_commits {
        deadline.check("listing files")?;
        for (mode, kind, oid, path) in reader.commit_files(commit)? {
            if kind != "blob" || mode.starts_with("12") {
                continue;
            }
            if !selection.matches(&path) {
                continue;
            }
            if !is_ingestable(Path::new(&path)) && code::language_for(Path::new(&path)).is_none() {
                continue;
            }
            keys.push((commit.clone(), path, oid));
        }
    }
    keys.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let mut by_blob: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (commit, path, blob) in keys {
        by_blob.entry(blob).or_default().push((commit, path));
    }

    // A blob over the limit fails the operation before any content is read.
    for (oid, uses) in &by_blob {
        let size = size_of.get(oid).copied().unwrap_or(0);
        if size > limits.max_blob_bytes {
            return Err(Error::BlobTooLarge {
                path: uses
                    .first()
                    .map(|u| u.1.clone())
                    .unwrap_or_else(|| oid.clone()),
                size,
                limit: limits.max_blob_bytes,
            });
        }
    }

    // ---- releases (small, own content), before the commits are complete
    let mut release_files: Vec<GitDerivedFile> = Vec::new();
    let mut tag_commits: Vec<GitCommitData> = Vec::new();
    let current_tags = if selection.is_all() {
        let (tag_files, tcs) = super::release::derive(&reader, &refs, &space_ids)?;
        tag_commits = tcs;
        let keys = tag_files
            .iter()
            .map(|d| (d.path.clone(), d.blob_oid.clone()))
            .collect::<Vec<_>>();
        release_files = tag_files;
        Some(keys)
    } else {
        None
    };
    for c in tag_commits {
        if known_commits.insert(c.oid.clone()) {
            sink.emit(&Record::Commit { item: c })?;
        }
    }

    // ---- blob rows (without content), ref events, remotes, views
    for (oid, size_bytes) in &blob_sizes {
        sink.emit(&Record::Blob {
            item: GitBlobData {
                oid: oid.clone(),
                size_bytes: *size_bytes,
                content: None,
            },
        })?;
    }
    let current = ctx.git_refs_current(repository_id)?;
    let current_by_name: HashMap<String, &GitRefObservation> =
        current.iter().map(|r| (r.name.clone(), r)).collect();
    let mut ref_events: Vec<GitRefEventDraft> = Vec::new();
    for r in &refs {
        if r.name == "HEAD" && r.symbolic_target.is_some() {
            continue;
        }
        match current_by_name.get(&r.name) {
            None => ref_events.push(GitRefEventDraft {
                name: r.name.clone(),
                kind: GitRefEventKind::Created,
                from_oid: None,
                to_oid: Some(r.target_oid.clone()),
            }),
            Some(prev) if prev.target_oid != r.target_oid => {
                let kind = if prev.target_kind == GitObjectKind::Commit
                    && r.target_kind == GitObjectKind::Commit
                    && reader.is_ancestor(&prev.target_oid, &r.target_oid)?
                {
                    GitRefEventKind::FastForward
                } else {
                    GitRefEventKind::NonFastForward
                };
                ref_events.push(GitRefEventDraft {
                    name: r.name.clone(),
                    kind,
                    from_oid: Some(prev.target_oid.clone()),
                    to_oid: Some(r.target_oid.clone()),
                });
            }
            Some(_) => {}
        }
    }
    for prev in &current {
        if prev.name == "HEAD" && prev.symbolic_target.is_some() {
            continue;
        }
        if !refs.iter().any(|r| r.name == prev.name) {
            ref_events.push(GitRefEventDraft {
                name: prev.name.clone(),
                kind: GitRefEventKind::Deleted,
                from_oid: Some(prev.target_oid.clone()),
                to_oid: None,
            });
        }
    }
    ref_events.sort_by(|a, b| a.name.cmp(&b.name));
    for e in ref_events {
        sink.emit(&Record::RefEvent { item: e })?;
    }
    for (name, raw) in reader.remotes()? {
        let parsed = super::remote::parse(&raw);
        sink.emit(&Record::Remote {
            item: GitRemoteDraft {
                name,
                url: parsed.url,
                slug_key: parsed.slug.as_ref().map(|s| s.key()),
                slug: parsed.slug.as_ref().map(|s| s.short()),
            },
        })?;
    }
    let view: Vec<String> = tip_commits.iter().cloned().collect();
    for part in view.chunks(1000) {
        sink.emit(&Record::ViewCommits {
            oids: part.to_vec(),
        })?;
    }
    if scope != super::GitScope::History {
        sink.emit(&Record::CurrentCommits { oids: Vec::new() })?;
        for part in view.chunks(1000) {
            sink.emit(&Record::CurrentCommits {
                oids: part.to_vec(),
            })?;
        }
    }
    if let Some(keys) = current_tags {
        sink.emit(&Record::CurrentTags { pairs: Vec::new() })?;
        for part in keys.chunks(1000) {
            sink.emit(&Record::CurrentTags {
                pairs: part.to_vec(),
            })?;
        }
    }

    // ---- bodies: blob batches
    let identity = code::identity();
    let mut batch: Vec<String> = Vec::new();
    let mut batch_bytes = 0u64;
    let oids: Vec<String> = by_blob.keys().cloned().collect();
    let mut pending: Vec<Vec<String>> = Vec::new();
    for oid in oids {
        let size = size_of.get(&oid).copied().unwrap_or(0);
        if !batch.is_empty()
            && (batch_bytes + size > BLOB_BATCH_BYTES || batch.len() >= BLOB_BATCH_COUNT)
        {
            pending.push(std::mem::take(&mut batch));
            batch_bytes = 0;
        }
        batch_bytes += size;
        batch.push(oid);
    }
    if !batch.is_empty() {
        pending.push(batch);
    }
    for batch in pending {
        deadline.check("reading file contents")?;
        let contents = reader.blob_contents(&batch)?;
        for blob in contents {
            let Some(content) = blob.content else {
                continue;
            };
            // The declared size was checked; the bytes Git returned are too.
            if content.len() as u64 > limits.max_blob_bytes {
                return Err(Error::BlobTooLarge {
                    path: by_blob
                        .get(&blob.oid)
                        .and_then(|u| u.first())
                        .map(|u| u.1.clone())
                        .unwrap_or_else(|| blob.oid.clone()),
                    size: content.len() as u64,
                    limit: limits.max_blob_bytes,
                });
            }
            let blob_oid = blob.oid;
            let Some(uses) = by_blob.get(&blob_oid) else {
                continue;
            };
            sink.emit(&Record::BlobContent {
                oid: blob_oid.clone(),
                len: content.len() as u64,
            })?;
            emit_chunks(sink, &content)?;
            let content_hash = sha256_hex(content.as_bytes());
            let mut code_input: Option<CodeInputDraft> = None;
            for (commit, path) in uses {
                deadline.check("deriving memories")?;
                let media_type = media_type_of(Path::new(path)).unwrap_or("text/rust");
                let stem = Path::new(path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("untitled")
                    .to_string();
                let mut parsed = if media_type == "text/rust" {
                    ParsedMaterial {
                        title: stem.clone(),
                        evidences: vec![],
                        assertions: vec![],
                        memories: vec![],
                    }
                } else {
                    parse_material(&stem, &content, media_type, &space_ids)
                };
                for a in &mut parsed.assertions {
                    a.extraction = format!("git:{}", a.extraction);
                }
                emit_derivation(
                    sink,
                    GitDerivedFile {
                        source_kind: crate::core::SourceKind::GitBlob,
                        path: path.clone(),
                        blob_oid: blob_oid.clone(),
                        commit_oid: commit.clone(),
                        content_hash: content_hash.clone(),
                        content: String::new(),
                        title: parsed.title,
                        media_type: media_type.to_string(),
                        evidences: parsed.evidences,
                        assertions: parsed.assertions,
                        memories: parsed.memories,
                    },
                    true,
                )?;
                // Code input: one per blob, created at its first source path.
                let Some(language) = code::language_for(Path::new(path)) else {
                    continue;
                };
                deadline.check("parsing source files")?;
                let entry = code_input.get_or_insert_with(|| CodeInputDraft {
                    language,
                    content_hash: content_hash.clone(),
                    content: String::new(),
                    repository_id: Some(repository_id),
                    blob_oid: Some(blob_oid.clone()),
                    contexts: vec![],
                    derivation: if ctx
                        .has_code_derivation(
                            language,
                            &content_hash,
                            Some(repository_id),
                            Some(&blob_oid),
                            &identity,
                        )
                        .unwrap_or(false)
                    {
                        CodeDerivationDraft {
                            identity: code::identity(),
                            status: crate::core::CodeParseStatus::Complete,
                            error_count: 0,
                            entities: vec![],
                            imports: vec![],
                            relationships: vec![],
                        }
                    } else {
                        code::parse(language, &content)
                    },
                });
                entry.contexts.push(CodeContextDraft {
                    path: path.clone(),
                    commit_oid: Some(commit.clone()),
                });
            }
            if let Some(input) = code_input {
                emit_code_input(sink, input, true)?;
            }
        }
    }
    // Release derivations carry their own content.
    for d in release_files {
        emit_derivation(sink, d, false)?;
    }
    Ok(())
}

/// Sends an existing result as a stream (tests, hostile-input construction, and
/// the transport benchmark). Content is sent with each user, not shared.
pub fn emit_extracted(sink: &mut dyn RecordSink, extracted: Extracted) -> Result<()> {
    match extracted {
        Extracted::Files(f) => {
            sink.emit(&Record::Begin { kind: Kind::Files })?;
            sink.emit(&Record::FilesHeader {
                session: f.observation.session,
                spaces: f.observation.spaces,
            })?;
            let mut code: HashMap<String, CodeInputDraft> = f
                .code_inputs
                .into_iter()
                .filter_map(|c| c.contexts.first().map(|x| x.path.clone()).map(|p| (p, c)))
                .collect();
            for file in f.observation.files {
                let input = code.remove(&file.location);
                emit_file(sink, file)?;
                if let Some(input) = input {
                    emit_code_input(sink, input, false)?;
                }
            }
        }
        Extracted::Git(g) => {
            let o = g.observation;
            sink.emit(&Record::Begin { kind: Kind::Git })?;
            sink.emit(&Record::GitHeader {
                session: o.session,
                repository_id: o.repository_id,
                repository_path: o.repository_path,
                spaces: o.spaces,
            })?;
            for item in o.refs {
                sink.emit(&Record::Ref { item })?;
            }
            for item in o.commits {
                sink.emit(&Record::Commit { item })?;
            }
            for item in o.trees {
                sink.emit(&Record::Tree { item })?;
            }
            for item in o.tags {
                sink.emit(&Record::Tag { item })?;
            }
            for item in o.blobs {
                // Content of blobs travels with their derivations.
                sink.emit(&Record::Blob {
                    item: GitBlobData {
                        content: None,
                        ..item
                    },
                })?;
            }
            for item in o.ref_events {
                sink.emit(&Record::RefEvent { item })?;
            }
            for item in o.remotes {
                sink.emit(&Record::Remote { item })?;
            }
            for part in o.view_commits.chunks(1000) {
                sink.emit(&Record::ViewCommits {
                    oids: part.to_vec(),
                })?;
            }
            if let Some(v) = o.current_commits {
                sink.emit(&Record::CurrentCommits { oids: Vec::new() })?;
                for part in v.chunks(1000) {
                    sink.emit(&Record::CurrentCommits {
                        oids: part.to_vec(),
                    })?;
                }
            }
            if let Some(v) = o.current_tags {
                sink.emit(&Record::CurrentTags { pairs: Vec::new() })?;
                for part in v.chunks(1000) {
                    sink.emit(&Record::CurrentTags {
                        pairs: part.to_vec(),
                    })?;
                }
            }
            for d in o.derivations {
                emit_derivation(sink, d, false)?;
            }
            for c in g.code_inputs {
                emit_code_input(sink, c, false)?;
            }
        }
    }
    Ok(())
}

// ----------------------------------------------------------------- consumer

#[derive(Debug, Clone, Copy)]
pub struct StreamLimits {
    pub max_frame: usize,
    pub max_total: u64,
    pub max_frames: u64,
    pub max_record: usize,
    pub extraction: ExtractionLimits,
}

impl Default for StreamLimits {
    fn default() -> Self {
        Self {
            max_frame: MAX_FRAME,
            max_total: MAX_TOTAL,
            max_frames: MAX_FRAMES,
            max_record: MAX_RECORD,
            extraction: ExtractionLimits::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ReadStats {
    pub frames: u64,
    pub bytes: u64,
    pub records: u64,
}

#[derive(Deserialize)]
struct FrameIn {
    v: u32,
    op: Uuid,
    seq: u64,
    records: Vec<Record>,
}

#[derive(Deserialize)]
struct FrameHead {
    v: Option<u32>,
}

/// Reads a whole operation and returns its result, or an error and nothing else.
/// `op` and `expect` are what the requester asked for; `root` is the directory
/// requested (files only). The result is returned only after the closing record
/// arrived and the counts matched.
pub fn consume<R: Read>(
    mut input: R,
    op: Uuid,
    expect: Kind,
    root: Option<&Path>,
    limits: &StreamLimits,
    cancel: Option<&AtomicBool>,
) -> Result<(Extracted, ReadStats)> {
    let mut asm = Assembler::new(expect, root, limits);
    let mut stats = ReadStats::default();
    let mut expected_seq = 0u64;
    let mut payload: Vec<u8> = Vec::new();
    loop {
        if let Some(c) = cancel {
            if c.load(Ordering::Relaxed) {
                return bad("cancelled");
            }
        }
        // length, checked before anything is allocated
        let mut len = [0u8; 4];
        match read_full(&mut input, &mut len)? {
            0 if asm.ended => break,
            0 => return bad("the stream ended before the closing record"),
            n if n < 4 => return bad("the stream ended inside a frame header"),
            _ => {}
        }
        if asm.ended {
            return bad("data after the closing record");
        }
        let len = u32::from_le_bytes(len) as usize;
        if len == 0 || len > limits.max_frame {
            return bad(format!(
                "frame length {len} is outside 1..={}",
                limits.max_frame
            ));
        }
        stats.frames += 1;
        stats.bytes += len as u64 + 4;
        if stats.frames > limits.max_frames {
            return bad(format!("more than {} frames", limits.max_frames));
        }
        if stats.bytes > limits.max_total {
            return bad(format!(
                "more than {} bytes in one operation",
                limits.max_total
            ));
        }
        payload.clear();
        payload.resize(len, 0);
        if read_full(&mut input, &mut payload)? < len {
            return bad("the stream ended inside a frame (truncated)");
        }
        let frame: FrameIn = match serde_json::from_slice(&payload) {
            Ok(f) => f,
            Err(e) => {
                // A frame of another protocol version may not decode at all:
                // say which.
                if let Ok(FrameHead { v: Some(v) }) = serde_json::from_slice(&payload) {
                    if v != PROTOCOL_VERSION {
                        return bad(format!(
                            "unsupported protocol version {v} (this is {PROTOCOL_VERSION})"
                        ));
                    }
                }
                return bad(format!("undecodable frame: {e}"));
            }
        };
        if frame.v != PROTOCOL_VERSION {
            return bad(format!(
                "unsupported protocol version {} (this is {PROTOCOL_VERSION})",
                frame.v
            ));
        }
        if frame.op != op {
            return bad("a frame belongs to another operation");
        }
        if frame.seq != expected_seq {
            return bad(format!(
                "frame {} where {expected_seq} was expected (lost, repeated or reordered)",
                frame.seq
            ));
        }
        expected_seq += 1;
        for record in frame.records {
            stats.records += 1;
            asm.accept(record)?;
        }
    }
    let extracted = asm.finish()?;
    Ok((extracted, stats))
}

/// Reads ahead on a thread: a producer that writes in bursts is not held up by the
/// consumer's decoding, and the consumer is not held up by a slow producer, at a
/// fixed cost of `depth` buffers of [`PREFETCH_BUFFER`] bytes. The thread ends at
/// end of input, on a read error, or when this reader is dropped and the source
/// next returns (a source that blocks forever keeps its thread: close the
/// source, e.g. by killing the peer process).
pub struct Prefetch {
    rx: std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>,
    current: Vec<u8>,
    pos: usize,
    done: bool,
}

pub const PREFETCH_BUFFER: usize = 256 * 1024;

impl Prefetch {
    pub fn new<R: Read + Send + 'static>(mut source: R, depth: usize) -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel(depth.max(1));
        std::thread::spawn(move || loop {
            let mut buf = vec![0u8; PREFETCH_BUFFER];
            match source.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    buf.truncate(n);
                    if tx.send(Ok(buf)).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    let _ = tx.send(Err(e));
                    break;
                }
            }
        });
        Self {
            rx,
            current: Vec::new(),
            pos: 0,
            done: false,
        }
    }
}

impl Read for Prefetch {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.pos >= self.current.len() {
            if self.done {
                return Ok(0);
            }
            match self.rx.recv() {
                Ok(Ok(b)) => {
                    self.current = b;
                    self.pos = 0;
                }
                Ok(Err(e)) => {
                    self.done = true;
                    return Err(e);
                }
                Err(_) => {
                    self.done = true;
                    return Ok(0);
                }
            }
        }
        let n = out.len().min(self.current.len() - self.pos);
        out[..n].copy_from_slice(&self.current[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Like `read_exact` but reports how much was read (EOF is not an error here).
fn read_full<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                return Err(Error::Io {
                    path: "record stream".into(),
                    source: e,
                })
            }
        }
    }
    Ok(n)
}

/// Where the consumer is in the grammar (git operations; files use `Begun`,
/// then `Body`).
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
enum Phase {
    Start,
    Begun,
    Header,
    Refs,
    Objects,
    Blobs,
    RefEvents,
    Remotes,
    Views,
    CurrentCommits,
    CurrentTags,
    Body,
}

/// The content slot: the text that derivations and code inputs share.
struct Slot {
    /// Blob oid (git) or file location (files).
    id: String,
    text: String,
    expected: u64,
    hash: Option<String>,
    /// Set for a blob slot: its row gets the text when the slot is replaced.
    blob_index: Option<usize>,
}

impl Slot {
    fn new(id: String, expected: u64, blob_index: Option<usize>) -> Self {
        let mut s = Self {
            id,
            text: String::new(),
            expected,
            hash: None,
            blob_index,
        };
        if expected == 0 {
            s.hash = Some(sha256_hex(b""));
        }
        s
    }
    fn complete(&self) -> bool {
        self.text.len() as u64 == self.expected
    }
}

enum Owner {
    File(ObservedFile),
    Derivation(GitDerivedFile),
    Code(CodeInputDraft),
}

struct Assembler<'a> {
    expect: Kind,
    root: Option<&'a Path>,
    limits: &'a StreamLimits,
    phase: Phase,
    ended: bool,
    seen: Counts,
    content_bytes: u64,
    session: Option<SessionDraft>,
    spaces: Vec<Uuid>,
    files: Vec<ObservedFile>,
    repository_id: Option<Uuid>,
    repository_path: String,
    refs: Vec<GitRefObservation>,
    commits: Vec<GitCommitData>,
    trees: Vec<GitTreeData>,
    tags: Vec<GitTagData>,
    blobs: Vec<GitBlobData>,
    blob_index: HashMap<String, usize>,
    ref_events: Vec<GitRefEventDraft>,
    remotes: Vec<GitRemoteDraft>,
    view_commits: Vec<String>,
    current_commits: Option<Vec<String>>,
    current_tags: Option<Vec<(String, String)>>,
    derivations: Vec<GitDerivedFile>,
    code_inputs: Vec<CodeInputDraft>,
    keys: HashSet<(&'static str, String)>,
    slot: Option<Slot>,
    owner: Option<Owner>,
    /// An oversized record being received: (record type, announced length, text so far).
    spill: Option<(String, usize, String)>,
}

impl<'a> Assembler<'a> {
    fn new(expect: Kind, root: Option<&'a Path>, limits: &'a StreamLimits) -> Self {
        Self {
            expect,
            root,
            limits,
            phase: Phase::Start,
            ended: false,
            seen: Counts::default(),
            content_bytes: 0,
            session: None,
            spaces: Vec::new(),
            files: Vec::new(),
            repository_id: None,
            repository_path: String::new(),
            refs: Vec::new(),
            commits: Vec::new(),
            trees: Vec::new(),
            tags: Vec::new(),
            blobs: Vec::new(),
            blob_index: HashMap::new(),
            ref_events: Vec::new(),
            remotes: Vec::new(),
            view_commits: Vec::new(),
            current_commits: None,
            current_tags: None,
            derivations: Vec::new(),
            code_inputs: Vec::new(),
            keys: HashSet::new(),
            slot: None,
            owner: None,
            spill: None,
        }
    }

    fn at(&mut self, to: Phase, what: &str) -> Result<()> {
        if to < self.phase || self.phase < Phase::Header && to != Phase::Header {
            return bad(format!("`{what}` out of order (after {:?})", self.phase));
        }
        self.phase = to;
        Ok(())
    }

    fn unique(&mut self, kind: &'static str, key: &str) -> Result<()> {
        if !self.keys.insert((kind, key.to_string())) {
            return bad(format!("duplicate {kind} `{}`", short(key)));
        }
        Ok(())
    }

    fn git(&self, what: &str) -> Result<()> {
        if self.expect != Kind::Git {
            return bad(format!("`{what}` in a files operation"));
        }
        Ok(())
    }

    fn add_content(&mut self, n: u64) -> Result<()> {
        self.content_bytes += n;
        if self.content_bytes > self.limits.extraction.max_content_bytes {
            return bad(format!(
                "{} bytes of content exceed the limit of {}",
                self.content_bytes, self.limits.extraction.max_content_bytes
            ));
        }
        Ok(())
    }

    fn count_check(&self) -> Result<()> {
        let n = self.derivations.len() + self.files.len();
        if n > self.limits.extraction.max_files {
            return bad(format!("{n} files exceed the limit"));
        }
        Ok(())
    }

    fn oid(&self, s: &str, what: &str) -> Result<()> {
        if !is_oid(s) {
            return bad(format!("malformed {what} id `{}`", short(s)));
        }
        Ok(())
    }

    fn path(&self, s: &str) -> Result<()> {
        check_relative(s, &self.limits.extraction)
    }

    /// Replaces the slot; the old one gives its text to its blob row.
    fn replace_slot(&mut self, new: Slot) {
        self.close_slot();
        self.slot = Some(new);
    }

    fn close_slot(&mut self) {
        if let Some(old) = self.slot.take() {
            if let Some(i) = old.blob_index {
                self.blobs[i].content = Some(old.text);
            }
        }
    }

    fn accept(&mut self, record: Record) -> Result<()> {
        if self.ended {
            return bad("a record after the closing record");
        }
        if self.phase == Phase::Start && !matches!(record, Record::Begin { .. }) {
            return bad(format!("`{}` before the opening record", record.name()));
        }
        if let Some((kind, len, text)) = self.spill.as_mut() {
            let Record::SpillPiece { data } = record else {
                return bad(format!(
                    "`{}` inside an oversized `{kind}` record",
                    record.name()
                ));
            };
            if text.len() + data.len() > *len {
                return bad("an oversized record is longer than announced");
            }
            text.push_str(&data);
            if text.len() == *len {
                let (kind, _, text) = self.spill.take().unwrap_or_default();
                let inner: Record = serde_json::from_str(&text).map_err(|e| {
                    Error::InvalidExtraction(format!(
                        "record stream: an oversized `{kind}` record does not decode: {e}"
                    ))
                })?;
                if inner.name() != kind
                    || matches!(inner, Record::Spill { .. } | Record::SpillPiece { .. })
                {
                    return bad(format!(
                        "an oversized record announced as `{kind}` is `{}`",
                        inner.name()
                    ));
                }
                return self.accept_logical(inner);
            }
            return Ok(());
        }
        match record {
            Record::Spill { record_type, len } => {
                if len == 0 || len > self.limits.max_record as u64 {
                    return bad(format!(
                        "an oversized record of {len} bytes is outside 1..={}",
                        self.limits.max_record
                    ));
                }
                self.spill = Some((record_type, len as usize, String::new()));
                Ok(())
            }
            Record::SpillPiece { .. } => bad("a `spill_piece` without a `spill`"),
            other => self.accept_logical(other),
        }
    }

    fn accept_logical(&mut self, record: Record) -> Result<()> {
        self.seen.records += 1;
        let name = record.name();
        if self.phase == Phase::Start && !matches!(record, Record::Begin { .. }) {
            return bad(format!("`{name}` before the opening record"));
        }
        match record {
            Record::Begin { kind } => {
                if self.phase != Phase::Start {
                    return bad("a second opening record");
                }
                if kind != self.expect {
                    return bad(format!(
                        "the operation is {kind:?} but {:?} was requested",
                        self.expect
                    ));
                }
                self.phase = Phase::Begun;
            }
            Record::FilesHeader { session, spaces } => {
                if self.expect != Kind::Files || self.phase != Phase::Begun {
                    return bad("`files_header` out of place");
                }
                if let Some(root) = self.root {
                    if Path::new(&session.root) != root {
                        return bad("the session root is not the requested root");
                    }
                }
                self.session = Some(session);
                self.spaces = spaces;
                self.phase = Phase::Body;
            }
            Record::GitHeader {
                session,
                repository_id,
                repository_path,
                spaces,
            } => {
                if self.expect != Kind::Git || self.phase != Phase::Begun {
                    return bad("`git_header` out of place");
                }
                self.session = Some(session);
                self.repository_id = Some(repository_id);
                self.repository_path = repository_path;
                self.spaces = spaces;
                self.phase = Phase::Header;
            }
            Record::File { file, content_len } => {
                if self.expect != Kind::Files || self.phase != Phase::Body {
                    return bad("`file` out of place");
                }
                self.close_owner()?;
                self.seen.files += 1;
                if file.location.len() > self.limits.extraction.max_path_bytes
                    || file.location.contains('\0')
                {
                    return bad("a file location is too long or contains NUL");
                }
                if let Some(root) = self.root {
                    let loc = Path::new(&file.location);
                    if !loc.starts_with(root)
                        || loc
                            .components()
                            .any(|c| matches!(c, std::path::Component::ParentDir))
                    {
                        return bad(format!("`{}` is outside the root", short(&file.location)));
                    }
                }
                if !file.content.is_empty()
                    || !file.evidences.is_empty()
                    || !file.assertions.is_empty()
                    || !file.memories.is_empty()
                {
                    return bad("a `file` record must not carry content or parts");
                }
                self.unique("file", &file.location)?;
                self.add_content(content_len)?;
                self.replace_slot(Slot::new(file.location.clone(), content_len, None));
                self.owner = Some(Owner::File(file));
                self.count_check()?;
            }
            Record::Ref { item } => {
                self.git("ref")?;
                self.at(Phase::Refs, name)?;
                self.unique("ref", &item.name)?;
                self.oid(&item.target_oid, "ref target")?;
                self.seen.refs += 1;
                self.refs.push(item);
            }
            Record::Commit { item } => {
                self.git("commit")?;
                self.at(Phase::Objects, name)?;
                self.oid(&item.oid, "commit")?;
                self.unique("object", &item.oid)?;
                self.seen.commits += 1;
                self.commits.push(item);
            }
            Record::Tree { item } => {
                self.git("tree")?;
                self.at(Phase::Objects, name)?;
                self.oid(&item.oid, "tree")?;
                self.unique("object", &item.oid)?;
                self.seen.trees += 1;
                self.trees.push(item);
            }
            Record::Tag { item } => {
                self.git("tag")?;
                self.at(Phase::Objects, name)?;
                self.oid(&item.oid, "tag")?;
                self.unique("object", &item.oid)?;
                self.seen.tags += 1;
                self.tags.push(item);
            }
            Record::Blob { item } => {
                self.git("blob")?;
                self.at(Phase::Blobs, name)?;
                self.oid(&item.oid, "blob")?;
                if item.content.is_some() {
                    return bad("a `blob` record must not carry content");
                }
                self.unique("blob", &item.oid)?;
                self.blob_index.insert(item.oid.clone(), self.blobs.len());
                self.seen.blobs += 1;
                self.blobs.push(item);
            }
            Record::RefEvent { item } => {
                self.git("ref_event")?;
                self.at(Phase::RefEvents, name)?;
                self.unique("ref event", &item.name)?;
                self.ref_events.push(item);
            }
            Record::Remote { item } => {
                self.git("remote")?;
                self.at(Phase::Remotes, name)?;
                self.unique("remote", &item.name)?;
                self.remotes.push(item);
            }
            Record::ViewCommits { oids } => {
                self.git("view_commits")?;
                self.at(Phase::Views, name)?;
                for o in &oids {
                    self.oid(o, "view commit")?;
                }
                self.view_commits.extend(oids);
            }
            Record::CurrentCommits { oids } => {
                self.git("current_commits")?;
                self.at(Phase::CurrentCommits, name)?;
                for o in &oids {
                    self.oid(o, "current commit")?;
                }
                self.current_commits
                    .get_or_insert_with(Vec::new)
                    .extend(oids);
            }
            Record::CurrentTags { pairs } => {
                self.git("current_tags")?;
                self.at(Phase::CurrentTags, name)?;
                for (path, blob) in &pairs {
                    self.path(path)?;
                    self.oid(blob, "release blob")?;
                }
                self.current_tags.get_or_insert_with(Vec::new).extend(pairs);
            }
            Record::BlobContent { oid, len } => {
                self.git("blob_content")?;
                self.at(Phase::Body, name)?;
                self.close_owner()?;
                let Some(&idx) = self.blob_index.get(&oid) else {
                    return bad(format!("content for an unannounced blob `{}`", short(&oid)));
                };
                self.unique("blob content", &oid)?;
                self.add_content(len)?;
                self.replace_slot(Slot::new(oid, len, Some(idx)));
            }
            Record::Derivation { file, content } => {
                self.git("derivation")?;
                self.at(Phase::Body, name)?;
                self.close_owner()?;
                self.path(&file.path)?;
                self.oid(&file.blob_oid, "blob")?;
                self.oid(&file.commit_oid, "commit")?;
                self.unique("derivation", &format!("{}\0{}", file.commit_oid, file.path))?;
                if !file.content.is_empty() {
                    return bad("a `derivation` record must not carry content");
                }
                match content {
                    ContentSource::Shared => {
                        let len = self.shared_slot(&file.blob_oid)?;
                        self.add_content(len)?;
                    }
                    ContentSource::Own { len } => {
                        self.add_content(len)?;
                        self.replace_slot(Slot::new(file.blob_oid.clone(), len, None));
                    }
                }
                self.seen.derivations += 1;
                self.owner = Some(Owner::Derivation(file));
                self.count_check()?;
            }
            Record::CodeInput { input, content } => {
                self.at(Phase::Body, name)?;
                self.close_owner()?;
                if !input.content.is_empty()
                    || !input.derivation.entities.is_empty()
                    || !input.derivation.imports.is_empty()
                    || !input.derivation.relationships.is_empty()
                {
                    return bad("a `code_input` record must not carry content or parts");
                }
                for c in &input.contexts {
                    if c.path.len() > self.limits.extraction.max_path_bytes || c.path.contains('\0')
                    {
                        return bad("a code context path is too long or contains NUL");
                    }
                }
                let id = match self.expect {
                    Kind::Git => {
                        let Some(oid) = &input.blob_oid else {
                            return bad("a git `code_input` without a blob id");
                        };
                        self.oid(oid, "blob")?;
                        if input.repository_id != self.repository_id {
                            return bad("a code input of another repository");
                        }
                        for c in &input.contexts {
                            self.path(&c.path)?;
                            let Some(commit) = &c.commit_oid else {
                                return bad("a git code context without a commit");
                            };
                            self.oid(commit, "commit")?;
                            if !self
                                .keys
                                .contains(&("derivation", format!("{commit}\0{}", c.path)))
                            {
                                return bad(format!(
                                    "a code context `{}` without a derivation",
                                    short(&c.path)
                                ));
                            }
                        }
                        oid.clone()
                    }
                    Kind::Files => {
                        let [ctx] = input.contexts.as_slice() else {
                            return bad("a files `code_input` needs exactly one context");
                        };
                        ctx.path.clone()
                    }
                };
                self.unique("code input", &id)?;
                match content {
                    ContentSource::Shared => {
                        let len = self.shared_slot(&id)?;
                        self.add_content(len)?;
                    }
                    ContentSource::Own { len } => {
                        self.add_content(len)?;
                        self.replace_slot(Slot::new(id, len, None));
                    }
                }
                self.seen.code_inputs += 1;
                self.owner = Some(Owner::Code(input));
            }
            Record::Chunk { data } => {
                let Some(slot) = self.slot.as_mut() else {
                    return bad("a `chunk` without an open content slot");
                };
                if slot.complete() {
                    return bad("a `chunk` after the content was complete");
                }
                if (slot.text.len() + data.len()) as u64 > slot.expected {
                    return bad("a `chunk` beyond the announced content length");
                }
                slot.text.push_str(&data);
                if slot.complete() {
                    slot.hash = Some(sha256_hex(slot.text.as_bytes()));
                }
            }
            Record::Part(part) => self.part(part)?,
            Record::Spill { .. } | Record::SpillPiece { .. } => {
                return bad("an oversized record inside an oversized record")
            }
            Record::End { counts } => {
                self.close_owner()?;
                if self.phase < Phase::Header {
                    return bad("the stream ended before its header");
                }
                if let Some(s) = &self.slot {
                    if !s.complete() {
                        return bad("the stream ended inside a content slot");
                    }
                }
                self.close_slot();
                if counts != self.seen {
                    return bad(format!(
                        "the closing counts {counts:?} differ from what arrived {:?}",
                        self.seen
                    ));
                }
                self.ended = true;
            }
        }
        Ok(())
    }

    /// The shared slot must be complete and belong to `id`; returns its length.
    fn shared_slot(&self, id: &str) -> Result<u64> {
        let Some(slot) = &self.slot else {
            return bad("shared content without a content slot");
        };
        if slot.id != id {
            return bad(format!(
                "shared content of `{}` where `{}` is open",
                short(id),
                short(&slot.id)
            ));
        }
        if !slot.complete() {
            return bad("shared content before the content was complete");
        }
        Ok(slot.expected)
    }

    fn part(&mut self, part: Part) -> Result<()> {
        if let Some(s) = &self.slot {
            if !s.complete() {
                return bad("a `part` inside incomplete content");
            }
        }
        match (self.owner.as_mut(), part) {
            (Some(Owner::File(f)), Part::Evidences { items }) => f.evidences.extend(items),
            (Some(Owner::File(f)), Part::Assertions { items }) => f.assertions.extend(items),
            (Some(Owner::File(f)), Part::Memories { items }) => f.memories.extend(items),
            (Some(Owner::Derivation(d)), Part::Evidences { items }) => d.evidences.extend(items),
            (Some(Owner::Derivation(d)), Part::Assertions { items }) => d.assertions.extend(items),
            (Some(Owner::Derivation(d)), Part::Memories { items }) => d.memories.extend(items),
            (Some(Owner::Code(c)), Part::Entities { items }) => c.derivation.entities.extend(items),
            (Some(Owner::Code(c)), Part::Imports { items }) => c.derivation.imports.extend(items),
            (Some(Owner::Code(c)), Part::Relationships { items }) => {
                c.derivation.relationships.extend(items)
            }
            (None, _) => return bad("a `part` without an owner"),
            _ => return bad("a `part` of a kind the owner does not have"),
        }
        Ok(())
    }

    /// Finishes the record whose content and parts have all arrived.
    fn close_owner(&mut self) -> Result<()> {
        let Some(owner) = self.owner.take() else {
            return Ok(());
        };
        let Some(slot) = &self.slot else {
            return bad("an owner without content");
        };
        if !slot.complete() {
            return bad("a record's content is incomplete");
        }
        let hash = slot.hash.clone().unwrap_or_default();
        match owner {
            Owner::File(mut f) => {
                if hash != f.content_hash {
                    return bad(format!(
                        "content hash mismatch for `{}`",
                        short(&f.location)
                    ));
                }
                if f.size_bytes < slot.text.len() as u64 {
                    return bad("size smaller than the content");
                }
                f.content = slot.text.clone();
                self.files.push(f);
            }
            Owner::Derivation(mut d) => {
                if hash != d.content_hash {
                    return bad(format!("content hash mismatch for `{}`", short(&d.path)));
                }
                d.content = slot.text.clone();
                self.derivations.push(d);
            }
            Owner::Code(mut c) => {
                if hash != c.content_hash {
                    return bad("code content hash mismatch");
                }
                c.content = slot.text.clone();
                self.code_inputs.push(c);
            }
        }
        Ok(())
    }

    fn finish(mut self) -> Result<Extracted> {
        if !self.ended {
            return bad("the stream ended before the closing record");
        }
        let session = self
            .session
            .take()
            .ok_or_else(|| Error::InvalidExtraction("record stream: no header".into()))?;
        Ok(match self.expect {
            Kind::Files => Extracted::Files(ExtractedFiles {
                observation: IngestObservation {
                    session,
                    files: self.files,
                    spaces: self.spaces,
                },
                code_inputs: self.code_inputs,
            }),
            Kind::Git => {
                // The producer sends derivations in blob order; the result is
                // ordered as the in-process extraction orders it.
                self.derivations
                    .sort_by(|a, b| (&a.commit_oid, &a.path).cmp(&(&b.commit_oid, &b.path)));
                Extracted::Git(ExtractedGit {
                    observation: GitObservation {
                        session,
                        repository_id: self.repository_id.unwrap_or_default(),
                        repository_path: self.repository_path,
                        refs: self.refs,
                        commits: self.commits,
                        trees: self.trees,
                        blobs: self.blobs,
                        tags: self.tags,
                        derivations: self.derivations,
                        ref_events: self.ref_events,
                        current_tags: self.current_tags,
                        view_commits: self.view_commits,
                        spaces: self.spaces,
                        remotes: self.remotes,
                        current_commits: self.current_commits,
                    },
                    code_inputs: self.code_inputs,
                })
            }
        })
    }
}
