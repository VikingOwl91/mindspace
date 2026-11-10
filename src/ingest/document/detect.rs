//! Format detection for the text adapter (M8 S2).
//!
//! Content decides, the file name only hints. The detector answers two different questions:
//!
//! 1. **Is this something the text adapter may read at all?** Binary signatures, NUL bytes, other
//!    encodings, invalid UTF-8 and control characters are refused with an explicit reason; known
//!    formats this slice does not parse (PDF, ZIP/OOXML, HTML, images...) are refused BY NAME instead
//!    of being read as accidental text.
//! 2. **Which dialect is the text?** Markdown or plain. Plain prose is also valid Markdown, so this is
//!    an interpretation, decided by the hint AND the content, and reported with its certainty.
//!
//! The detector is versioned ([`DETECTOR_VERSION`]); any change of its decisions bumps it.

use crate::core::{DocumentRefusal, Error, Result};

/// Part of every extraction identity this detector routes.
pub const DETECTOR_VERSION: &str = "1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Markdown,
    Plain,
}

impl Dialect {
    pub fn as_str(self) -> &'static str {
        match self {
            Dialect::Markdown => "markdown",
            Dialect::Plain => "plain",
        }
    }
}

/// How firmly the dialect is known. `Ambiguous` is a genuine "the evidence disagrees": the
/// conservative reading (plain: no structure invented) is used and the case is reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialectCertainty {
    /// The file-name hint and the content agree.
    HintAndContentAgree,
    /// Only the hint says Markdown; the content shows no Markdown syntax (plain prose is valid
    /// Markdown, and parses identically).
    HintOnly,
    /// No usable hint; the content alone decides.
    ContentOnly,
    /// The hint says plain text but the content has Markdown headings or fences.
    Ambiguous,
}

impl DialectCertainty {
    pub fn as_str(self) -> &'static str {
        match self {
            DialectCertainty::HintAndContentAgree => "hint_and_content_agree",
            DialectCertainty::HintOnly => "hint_only",
            DialectCertainty::ContentOnly => "content_only",
            DialectCertainty::Ambiguous => "ambiguous",
        }
    }
}

/// What the detector found. Reported by the ingest call; the dialect also survives in the stored
/// extraction identity (extractor id), the hint in `format_hint`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    /// Always `"text"`: the physical format the adapter accepts.
    pub physical: &'static str,
    pub dialect: Dialect,
    pub certainty: DialectCertainty,
    /// The extension-derived hint as given (lower case), if any. Never trusted on its own.
    pub hint: Option<String>,
    /// Markdown syntax found in the content (`atx_heading`, `fence`, `list_item`, `blockquote`).
    pub signals: Vec<&'static str>,
    /// A UTF-8 byte-order mark was present and is not part of the canonical text.
    pub bom_stripped: bool,
}

fn refuse<T>(reason: DocumentRefusal, detail: impl Into<String>) -> Result<T> {
    Err(Error::DocumentRefused {
        reason,
        detail: detail.into(),
    })
}

/// Formats recognised by their first bytes and refused by name.
fn signature_name(bytes: &[u8]) -> Option<&'static str> {
    let head = &bytes[..bytes.len().min(1024)];
    let starts = |sig: &[u8]| head.starts_with(sig);
    // Text-like signatures count only where a file would start with them (after an optional
    // byte-order mark and blanks): prose that merely QUOTES "%PDF-" or "GIF89a" is prose. A real PDF
    // with leading junk is binary a few bytes later and is refused by the NUL/UTF-8/control checks.
    let lead = head.strip_prefix(b"\xef\xbb\xbf").unwrap_or(head);
    let lead = lead
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .map(|i| &lead[i..])
        .unwrap_or(&[]);
    if lead.starts_with(b"%PDF-") {
        return Some("pdf");
    }
    if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") {
        return Some("zip container (docx, xlsx, pptx, jar, ...)");
    }
    if starts(b"\x89PNG") {
        return Some("png image");
    }
    if starts(b"\xff\xd8\xff") {
        return Some("jpeg image");
    }
    // a GIF header continues with a binary screen descriptor; a text that merely begins with the
    // six characters does not
    if (starts(b"GIF87a") || starts(b"GIF89a"))
        && head[6..head.len().min(16)]
            .iter()
            .any(|&b| b == 0 || b >= 0x80)
    {
        return Some("gif image");
    }
    if starts(b"\x7fELF") {
        return Some("elf executable");
    }
    if starts(b"\x1f\x8b") {
        return Some("gzip data");
    }
    if starts(b"\xd0\xcf\x11\xe0") {
        return Some("legacy office (OLE) document");
    }
    if starts(b"{\\rtf") {
        return Some("rtf document");
    }
    // HTML: the first non-whitespace token (after an optional BOM)
    let body = head.strip_prefix(b"\xef\xbb\xbf").unwrap_or(head);
    let trimmed = body
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .map(|i| &body[i..])
        .unwrap_or(&[]);
    let lower: Vec<u8> = trimmed
        .iter()
        .take(16)
        .map(u8::to_ascii_lowercase)
        .collect();
    if lower.starts_with(b"<!doctype html") || lower.starts_with(b"<html") {
        return Some("html document");
    }
    None
}

/// Validates the bytes and returns the canonical text (BOM removed) with the detection.
pub fn detect(bytes: &[u8], hint: Option<&str>) -> Result<(String, Detection)> {
    if bytes.is_empty() {
        return refuse(DocumentRefusal::Empty, "the file is empty");
    }
    if let Some(name) = signature_name(bytes) {
        return refuse(
            DocumentRefusal::UnsupportedFormat,
            format!("{name}: this adapter reads UTF-8 text and Markdown only"),
        );
    }
    // UTF-32 first: its little-endian mark begins with the UTF-16 one
    if bytes.starts_with(&[0xff, 0xfe, 0x00, 0x00])
        || bytes.starts_with(&[0x00, 0x00, 0xfe, 0xff])
        || bytes.starts_with(&[0xff, 0xfe])
        || bytes.starts_with(&[0xfe, 0xff])
    {
        return refuse(
            DocumentRefusal::UnsupportedEncoding,
            "UTF-16/UTF-32 byte-order mark: only UTF-8 is read",
        );
    }
    if let Some(at) = bytes.iter().position(|&b| b == 0) {
        return refuse(
            DocumentRefusal::Binary,
            format!("NUL byte at offset {at}: binary content"),
        );
    }
    let (body, bom_stripped) = match bytes.strip_prefix(b"\xef\xbb\xbf") {
        Some(rest) => (rest, true),
        None => (bytes, false),
    };
    let text = match std::str::from_utf8(body) {
        Ok(t) => t,
        Err(e) => {
            let at = e.valid_up_to() + if bom_stripped { 3 } else { 0 };
            return refuse(
                DocumentRefusal::NotUtf8,
                format!("invalid UTF-8 at byte offset {at}"),
            );
        }
    };
    if let Some((i, c)) = text.char_indices().find(|&(_, c)| {
        (c.is_control() && c as u32 <= 0x1f && !matches!(c, '\t' | '\n' | '\r' | '\x0c'))
            || c == '\x7f'
    }) {
        let at = i + if bom_stripped { 3 } else { 0 };
        return refuse(
            DocumentRefusal::ControlCharacters,
            format!("control character U+{:04X} at byte offset {at}", c as u32),
        );
    }
    if text.trim().is_empty() {
        return refuse(DocumentRefusal::Empty, "the file contains only whitespace");
    }

    let hint = hint
        .map(str::to_ascii_lowercase)
        .filter(|h| !h.is_empty() && h.len() <= 32 && h.bytes().all(|b| b.is_ascii_alphanumeric()));
    let signals = markdown_signals(text);
    let strong = signals
        .iter()
        .any(|s| matches!(*s, "atx_heading" | "fence"));
    let any = !signals.is_empty();
    let (dialect, certainty) = match hint.as_deref() {
        Some("md" | "markdown") => (
            Dialect::Markdown,
            if any {
                DialectCertainty::HintAndContentAgree
            } else {
                DialectCertainty::HintOnly
            },
        ),
        Some("txt" | "text") => (
            Dialect::Plain,
            if strong {
                DialectCertainty::Ambiguous
            } else {
                DialectCertainty::HintAndContentAgree
            },
        ),
        // any other (or no) hint says nothing about the dialect
        _ => {
            if strong {
                (Dialect::Markdown, DialectCertainty::ContentOnly)
            } else if any {
                (Dialect::Plain, DialectCertainty::Ambiguous)
            } else {
                (Dialect::Plain, DialectCertainty::ContentOnly)
            }
        }
    };
    Ok((
        text.to_string(),
        Detection {
            physical: "text",
            dialect,
            certainty,
            hint,
            signals,
            bom_stripped,
        },
    ))
}

/// Distinct Markdown constructs present in the text. Fenced regions are skipped for the line-based
/// signals (a `# comment` inside a fence is code, not a heading).
fn markdown_signals(text: &str) -> Vec<&'static str> {
    let mut found: Vec<&'static str> = Vec::new();
    let add = |s: &'static str, found: &mut Vec<&'static str>| {
        if !found.contains(&s) {
            found.push(s);
        }
    };
    let mut fence: Option<(char, usize)> = None;
    for line in text.lines() {
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent > 3 {
            continue; // indented code, or continuation: no block syntax here
        }
        let rest = &line[indent..];
        if let Some((ch, n)) = fence {
            let run = rest.chars().take_while(|&c| c == ch).count();
            if run >= n && rest[run..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        let first = rest.chars().next();
        if let Some(ch @ ('`' | '~')) = first {
            let run = rest.chars().take_while(|&c| c == ch).count();
            if run >= 3 && !(ch == '`' && rest[run..].contains('`')) {
                fence = Some((ch, run));
                add("fence", &mut found);
                continue;
            }
        }
        if first == Some('#') {
            let hashes = rest.chars().take_while(|&c| c == '#').count();
            let after = &rest[hashes..];
            if (1..=6).contains(&hashes) && (after.is_empty() || after.starts_with([' ', '\t'])) {
                add("atx_heading", &mut found);
                continue;
            }
        }
        if first == Some('>') {
            add("blockquote", &mut found);
            continue;
        }
        let bullet = matches!(first, Some('-' | '*' | '+'))
            && rest[1..].starts_with([' ', '\t'])
            && !rest[1..].trim().is_empty();
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        let ordered = (1..=9).contains(&digits)
            && matches!(rest[digits..].chars().next(), Some('.' | ')'))
            && rest[digits + 1..].starts_with([' ', '\t'])
            && !rest[digits + 1..].trim().is_empty();
        if bullet || ordered {
            add("list_item", &mut found);
        }
    }
    found
}
