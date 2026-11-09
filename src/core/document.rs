//! Document model (M8 S1): identities, structured citation anchors, precision and availability.
//!
//! This module is pure: it parses nothing from files and touches no database. It defines
//!
//! * the four identities a document has (see [`DocumentExtractionDraft`]): the *document* (a logical
//!   source), a *revision* (its canonical text), the *original bytes* (hash only, unless explicitly
//!   retained) and the *extraction* (bytes + extractor identity);
//! * [`Anchor`], a typed, serializable source location bound to one extraction, with an honest
//!   [`AnchorPrecision`]; and
//! * [`CitationStatus`] / [`Resolution`], how far a citation can be followed once the original may
//!   have changed or vanished.
//!
//! Nothing here invents a location: a page, a bounding box or a text range exists in an anchor only if
//! it was supplied, and an anchor can be *downgraded* but never upgraded.

use std::fmt::Write as _;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::{sha256_hex, Error, Result, Ts, Uuid};

/// Largest canonical text one extraction may carry (design 7: 32 MiB).
pub const MAX_CANONICAL_TEXT_BYTES: usize = 32 * 1024 * 1024;
/// Most nodes one extraction may carry (design 7).
pub const MAX_NODES: usize = 500_000;
/// Deepest node path.
pub const MAX_NODE_DEPTH: usize = 64;
/// Largest original file that may be retained (opt-in).
pub const MAX_RETAINED_BYTES: u64 = 64 * 1024 * 1024;

fn invalid_doc<T>(why: impl Into<String>) -> Result<T> {
    Err(Error::InvalidDocument(why.into()))
}

fn invalid_anchor<T>(why: impl Into<String>) -> Result<T> {
    Err(Error::InvalidAnchor(why.into()))
}

/// True for 64 lowercase hexadecimal characters (a SHA-256 digest).
pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Hash of extraction parameters: SHA-256 over their canonical JSON (`serde_json` objects are
/// key-sorted, so equal parameters hash equally whatever the construction order).
pub fn params_hash(params: &serde_json::Value) -> String {
    sha256_hex(params.to_string().as_bytes())
}

fn ident_ok(s: &str, max: usize, extra: &[u8]) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || extra.contains(&b))
}

/// `[A-Za-z0-9._-]{1,64}`: no separator the anchor syntax uses (`@ : # ;`).
pub fn is_extractor_id(s: &str) -> bool {
    ident_ok(s, 64, b"._-")
}

/// `[A-Za-z0-9._+-]{1,32}`.
pub fn is_version(s: &str) -> bool {
    ident_ok(s, 32, b"._+-")
}

// ------------------------------------------------------------------------------- precision

/// How exactly a location is known. Ordered: `ByteExact` > `NodeExact` > `PageOnly` > `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorPrecision {
    /// A byte range of the canonical text.
    ByteExact,
    /// A node of the structure tree (optionally with a text range, a page and a bounding box).
    NodeExact,
    /// A page number only.
    PageOnly,
    /// The extraction is identified, nothing inside it is claimed.
    None,
}

impl AnchorPrecision {
    pub fn as_str(self) -> &'static str {
        match self {
            AnchorPrecision::ByteExact => "byte_exact",
            AnchorPrecision::NodeExact => "node_exact",
            AnchorPrecision::PageOnly => "page_only",
            AnchorPrecision::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "byte_exact" => Some(AnchorPrecision::ByteExact),
            "node_exact" => Some(AnchorPrecision::NodeExact),
            "page_only" => Some(AnchorPrecision::PageOnly),
            "none" => Some(AnchorPrecision::None),
            _ => None,
        }
    }

    /// Larger is more precise.
    pub fn rank(self) -> u8 {
        match self {
            AnchorPrecision::ByteExact => 3,
            AnchorPrecision::NodeExact => 2,
            AnchorPrecision::PageOnly => 1,
            AnchorPrecision::None => 0,
        }
    }
}

/// A bounding box in the parser's page coordinates (points). Finite, `x0 <= x1`, `y0 <= y1`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BBox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl BBox {
    pub fn validate(&self) -> Result<()> {
        let all = [self.x0, self.y0, self.x1, self.y1];
        if !all.iter().all(|v| v.is_finite()) {
            return invalid_anchor("a bounding box coordinate is not finite");
        }
        if self.x0 > self.x1 || self.y0 > self.y1 {
            return invalid_anchor("a bounding box is inverted");
        }
        Ok(())
    }
}

/// Node kinds of the structure tree (design 6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Page,
    Section,
    Heading,
    Paragraph,
    List,
    ListItem,
    Table,
    Row,
    Cell,
    Figure,
    Caption,
    Footnote,
    Reference,
    Appendix,
}

impl NodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeKind::Page => "page",
            NodeKind::Section => "section",
            NodeKind::Heading => "heading",
            NodeKind::Paragraph => "paragraph",
            NodeKind::List => "list",
            NodeKind::ListItem => "list_item",
            NodeKind::Table => "table",
            NodeKind::Row => "row",
            NodeKind::Cell => "cell",
            NodeKind::Figure => "figure",
            NodeKind::Caption => "caption",
            NodeKind::Footnote => "footnote",
            NodeKind::Reference => "reference",
            NodeKind::Appendix => "appendix",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "page" => NodeKind::Page,
            "section" => NodeKind::Section,
            "heading" => NodeKind::Heading,
            "paragraph" => NodeKind::Paragraph,
            "list" => NodeKind::List,
            "list_item" => NodeKind::ListItem,
            "table" => NodeKind::Table,
            "row" => NodeKind::Row,
            "cell" => NodeKind::Cell,
            "figure" => NodeKind::Figure,
            "caption" => NodeKind::Caption,
            "footnote" => NodeKind::Footnote,
            "reference" => NodeKind::Reference,
            "appendix" => NodeKind::Appendix,
            _ => return None,
        })
    }
}

/// The shape a stated precision requires of the facts stored next to it. `has_path` is whether the
/// location names a node. Used for nodes (always with a path) and anchors alike.
fn check_shape(
    precision: AnchorPrecision,
    has_path: bool,
    text_range: Option<(u64, u64)>,
    page: Option<u32>,
    bbox: Option<&BBox>,
    node: bool,
) -> std::result::Result<(), String> {
    if let Some((a, b)) = text_range {
        if a > b {
            return Err("the text range is inverted".into());
        }
    }
    if page == Some(0) {
        return Err("page numbers start at 1".into());
    }
    if bbox.is_some() && page.is_none() {
        return Err("a bounding box needs the page it lies on".into());
    }
    match precision {
        AnchorPrecision::ByteExact => {
            if text_range.is_none() {
                return Err("byte_exact requires a text range".into());
            }
        }
        AnchorPrecision::NodeExact => {
            if !has_path {
                return Err("node_exact requires a node path".into());
            }
        }
        AnchorPrecision::PageOnly => {
            if page.is_none() {
                return Err("page_only requires a page".into());
            }
            if text_range.is_some() || bbox.is_some() {
                return Err("page_only carries no text range or bounding box".into());
            }
            if !node && has_path {
                return Err("page_only names no node".into());
            }
        }
        AnchorPrecision::None => {
            if text_range.is_some() || page.is_some() || bbox.is_some() {
                return Err("precision none carries no location".into());
            }
            if !node && has_path {
                return Err("precision none names no node".into());
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------ anchors

/// A source location bound to ONE extraction: document, original-bytes hash, extractor identity and
/// parameters. A citation therefore cannot silently resolve against another revision.
///
/// Canonical text form (one string per anchor; parsing rejects any other spelling):
/// `mind-doc:v1:<logical-uuid>:<bytes-sha256>:<detector-version>:<extractor>@<version>:<params-sha256>#<path>;q=<precision>[;t=<a>-<b>][;p=<page>][;b=<x0>,<y0>,<x1>,<y1>]`
/// where `<path>` is `-` (no node) or dot-separated ordinals.
#[derive(Debug, Clone, PartialEq)]
pub struct Anchor {
    pub logical_id: Uuid,
    pub source_bytes_hash: String,
    /// The format detector that routed the bytes to the extractor; part of the extraction identity.
    pub detector_version: String,
    pub extractor_id: String,
    pub extractor_version: String,
    pub params_hash: String,
    /// Ordinal path of the node; empty = no node named.
    pub node_path: Vec<u32>,
    /// UTF-8 byte range `[start, end)` of the canonical text.
    pub text_range: Option<(u64, u64)>,
    pub page: Option<u32>,
    pub bbox: Option<BBox>,
    pub precision: AnchorPrecision,
}

impl Anchor {
    /// Checks every invariant; the only way to build an anchor from parts.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        logical_id: Uuid,
        source_bytes_hash: &str,
        detector_version: &str,
        extractor_id: &str,
        extractor_version: &str,
        params_hash: &str,
        node_path: Vec<u32>,
        text_range: Option<(u64, u64)>,
        page: Option<u32>,
        bbox: Option<BBox>,
        precision: AnchorPrecision,
    ) -> Result<Self> {
        let a = Anchor {
            logical_id,
            source_bytes_hash: source_bytes_hash.to_string(),
            detector_version: detector_version.to_string(),
            extractor_id: extractor_id.to_string(),
            extractor_version: extractor_version.to_string(),
            params_hash: params_hash.to_string(),
            node_path,
            text_range,
            page,
            bbox,
            precision,
        };
        a.validate()?;
        Ok(a)
    }

    pub fn validate(&self) -> Result<()> {
        if !is_sha256_hex(&self.source_bytes_hash) {
            return invalid_anchor("the original-bytes hash is not a SHA-256 digest");
        }
        if !is_sha256_hex(&self.params_hash) {
            return invalid_anchor("the parameters hash is not a SHA-256 digest");
        }
        if !is_extractor_id(&self.extractor_id)
            || !is_version(&self.extractor_version)
            || !is_version(&self.detector_version)
        {
            return invalid_anchor("the detector or extractor identity has an invalid spelling");
        }
        if self.node_path.len() > MAX_NODE_DEPTH {
            return invalid_anchor("the node path is too deep");
        }
        if let Some(b) = &self.bbox {
            b.validate()?;
        }
        check_shape(
            self.precision,
            !self.node_path.is_empty(),
            self.text_range,
            self.page,
            self.bbox.as_ref(),
            false,
        )
        .or_else(invalid_anchor)
    }

    /// The same location with LESS precision. Information is dropped, never invented: asking for a
    /// precision the anchor cannot back (a page it never had) is an error, and so is an upgrade.
    pub fn downgrade(&self, to: AnchorPrecision) -> Result<Anchor> {
        if to.rank() > self.precision.rank() {
            return invalid_anchor(format!(
                "cannot raise precision from {} to {}",
                self.precision.as_str(),
                to.as_str()
            ));
        }
        let mut a = self.clone();
        a.precision = to;
        match to {
            AnchorPrecision::ByteExact => {}
            AnchorPrecision::NodeExact => {
                if a.node_path.is_empty() {
                    return invalid_anchor(
                        "this anchor names no node, node_exact cannot be claimed",
                    );
                }
            }
            AnchorPrecision::PageOnly => {
                if a.page.is_none() {
                    return invalid_anchor("this anchor has no page, page_only cannot be claimed");
                }
                a.node_path.clear();
                a.text_range = None;
                a.bbox = None;
            }
            AnchorPrecision::None => {
                a.node_path.clear();
                a.text_range = None;
                a.page = None;
                a.bbox = None;
            }
        }
        a.validate()?;
        Ok(a)
    }

    /// The identity of the extraction this anchor is bound to.
    pub fn extraction_key(&self) -> (Uuid, &str, &str, &str, &str, &str) {
        (
            self.logical_id,
            &self.source_bytes_hash,
            &self.detector_version,
            &self.extractor_id,
            &self.extractor_version,
            &self.params_hash,
        )
    }
}

impl std::fmt::Display for Anchor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = format!(
            "mind-doc:v1:{}:{}:{}:{}@{}:{}#",
            self.logical_id,
            self.source_bytes_hash,
            self.detector_version,
            self.extractor_id,
            self.extractor_version,
            self.params_hash
        );
        if self.node_path.is_empty() {
            out.push('-');
        } else {
            let parts: Vec<String> = self.node_path.iter().map(u32::to_string).collect();
            out.push_str(&parts.join("."));
        }
        let _ = write!(out, ";q={}", self.precision.as_str());
        if let Some((a, b)) = self.text_range {
            let _ = write!(out, ";t={a}-{b}");
        }
        if let Some(p) = self.page {
            let _ = write!(out, ";p={p}");
        }
        if let Some(b) = &self.bbox {
            let _ = write!(out, ";b={},{},{},{}", b.x0, b.y0, b.x1, b.y1);
        }
        f.write_str(&out)
    }
}

impl FromStr for Anchor {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        if s.len() > 2048 {
            return invalid_anchor("the anchor is too long");
        }
        let rest = s
            .strip_prefix("mind-doc:v1:")
            .ok_or_else(|| Error::InvalidAnchor("not a mind-doc v1 anchor".into()))?;
        let (head, tail) = rest
            .split_once('#')
            .ok_or_else(|| Error::InvalidAnchor("the anchor has no '#' part".into()))?;
        let parts: Vec<&str> = head.split(':').collect();
        if parts.len() != 5 {
            return invalid_anchor(
                "the anchor head must be <logical>:<bytes>:<detector>:<extractor@version>:<params>",
            );
        }
        let logical_id = Uuid::parse_str(parts[0])
            .map_err(|_| Error::InvalidAnchor("the document id is not a UUID".into()))?;
        let (extractor_id, extractor_version) = parts[3].split_once('@').ok_or_else(|| {
            Error::InvalidAnchor("the extractor identity needs '@<version>'".into())
        })?;
        let mut fields = tail.split(';');
        let path = fields.next().unwrap_or("");
        let node_path: Vec<u32> = if path == "-" {
            Vec::new()
        } else {
            path.split('.')
                .map(|p| {
                    p.parse::<u32>()
                        .map_err(|_| Error::InvalidAnchor("a node path ordinal is invalid".into()))
                })
                .collect::<Result<_>>()?
        };
        let mut precision = None;
        let (mut text_range, mut page, mut bbox) = (None, None, None);
        for field in fields {
            let (key, value) = field
                .split_once('=')
                .ok_or_else(|| Error::InvalidAnchor("malformed anchor field".into()))?;
            let dup = || Error::InvalidAnchor(format!("duplicate anchor field '{key}'"));
            match key {
                "q" => {
                    if precision
                        .replace(
                            AnchorPrecision::parse(value)
                                .ok_or_else(|| Error::InvalidAnchor("unknown precision".into()))?,
                        )
                        .is_some()
                    {
                        return Err(dup());
                    }
                }
                "t" => {
                    let (a, b) = value
                        .split_once('-')
                        .ok_or_else(|| Error::InvalidAnchor("malformed text range".into()))?;
                    let r = (
                        a.parse::<u64>()
                            .map_err(|_| Error::InvalidAnchor("malformed text range".into()))?,
                        b.parse::<u64>()
                            .map_err(|_| Error::InvalidAnchor("malformed text range".into()))?,
                    );
                    if text_range.replace(r).is_some() {
                        return Err(dup());
                    }
                }
                "p" => {
                    let p = value
                        .parse::<u32>()
                        .map_err(|_| Error::InvalidAnchor("malformed page".into()))?;
                    if page.replace(p).is_some() {
                        return Err(dup());
                    }
                }
                "b" => {
                    let v: Vec<f32> = value
                        .split(',')
                        .map(|x| {
                            x.parse::<f32>()
                                .map_err(|_| Error::InvalidAnchor("malformed bounding box".into()))
                        })
                        .collect::<Result<_>>()?;
                    if v.len() != 4 {
                        return invalid_anchor("a bounding box has four coordinates");
                    }
                    let b = BBox {
                        x0: v[0],
                        y0: v[1],
                        x1: v[2],
                        y1: v[3],
                    };
                    if bbox.replace(b).is_some() {
                        return Err(dup());
                    }
                }
                other => return invalid_anchor(format!("unknown anchor field '{other}'")),
            }
        }
        let precision = precision
            .ok_or_else(|| Error::InvalidAnchor("the anchor states no precision (q=)".into()))?;
        let anchor = Anchor::new(
            logical_id,
            parts[1],
            parts[2],
            extractor_id,
            extractor_version,
            parts[4],
            node_path,
            text_range,
            page,
            bbox,
            precision,
        )?;
        // one spelling per anchor: anything that does not re-render identically is refused
        if anchor.to_string() != s {
            return invalid_anchor("the anchor is not in canonical form");
        }
        Ok(anchor)
    }
}

// ------------------------------------------------------------------------------ availability

/// The outcomes of following a citation. `TextResolved` concerns the retained canonical text and is
/// independent of the others, which concern the ORIGINAL file; a [`Resolution`] reports one of each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CitationStatus {
    /// The anchored range exists in the retained canonical text of the cited extraction.
    TextResolved,
    /// The original bytes are retained and were re-verified against the cited hash.
    BytesResolved,
    /// Not retained, but the file at its location hashes to the cited hash right now.
    SourcePresentVerified,
    /// The file at its location exists and hashes differently.
    SourceChanged,
    /// The file at its location does not exist.
    SourceMissing,
    /// Not retained, and the file could not be checked (not looked at, unreadable, not permitted).
    SourceUnavailable,
}

impl CitationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CitationStatus::TextResolved => "text_resolved",
            CitationStatus::BytesResolved => "bytes_resolved",
            CitationStatus::SourcePresentVerified => "source_present_verified",
            CitationStatus::SourceChanged => "source_changed",
            CitationStatus::SourceMissing => "source_missing",
            CitationStatus::SourceUnavailable => "source_unavailable",
        }
    }

    /// True when the original bytes of this exact revision were checked.
    pub fn original_verified(self) -> bool {
        matches!(
            self,
            CitationStatus::BytesResolved | CitationStatus::SourcePresentVerified
        )
    }
}

/// What the caller observed about the live file. The store never reads the filesystem: the caller
/// decides how (and under which authority) to look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveObservation {
    /// The file exists; this is the SHA-256 of its current bytes.
    Hash(String),
    Missing,
    Unavailable(String),
}

/// How the retained original bytes of a citation's revision stand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainedBytes {
    Absent,
    /// Present and re-hashed to the cited digest.
    Verified,
    /// A row exists but its content does not hash to its key.
    Corrupt,
}

/// The stored side of a citation, read without touching the filesystem.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredCitation {
    pub anchor: Anchor,
    pub extraction_id: Uuid,
    pub format: String,
    /// The precision the stored node (or the extraction, for node-less anchors) supports.
    pub stored_precision: AnchorPrecision,
    /// Whether the canonical text IS the original text (byte offsets mean the same in both).
    pub text_is_source: bool,
    /// The byte range that was resolved in the canonical text, if the anchor or its node has one.
    pub text_range: Option<(u64, u64)>,
    /// The text at that range.
    pub text: Option<String>,
    pub retained: RetainedBytes,
    /// Where the logical source lived last (a hint, never retention).
    pub locator: String,
}

/// A citation followed as far as it can be.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    pub anchor: Anchor,
    pub extraction_id: Uuid,
    /// The quoted text and its range in the canonical text (always from the database).
    pub text: Option<String>,
    pub text_range: Option<(u64, u64)>,
    /// Exactly one of `BytesResolved`, `SourcePresentVerified`, `SourceChanged`, `SourceMissing`,
    /// `SourceUnavailable`.
    pub original: CitationStatus,
    /// Why `original` is `SourceUnavailable`, when it is.
    pub limitation: Option<String>,
    /// What may be claimed about positions in the ORIGINAL file. `None` whenever the original of this
    /// revision was not verified: with only extracted text left, no byte, page or box claim is made.
    pub source_precision: Option<AnchorPrecision>,
}

impl Resolution {
    /// All statuses that hold: `TextResolved` (when the text was found) and the original's.
    pub fn statuses(&self) -> Vec<CitationStatus> {
        let mut v = Vec::new();
        if self.text.is_some() || self.text_range.is_some() {
            v.push(CitationStatus::TextResolved);
        }
        v.push(self.original);
        v
    }
}

/// The precision that may be claimed about the original file: nothing without a verified original;
/// with one, the stored precision, except that byte offsets in the canonical text mean byte offsets
/// in the original only when the canonical text IS the original text.
pub fn source_precision_claim(
    stored: AnchorPrecision,
    text_is_source: bool,
    has_node: bool,
    original: CitationStatus,
) -> Option<AnchorPrecision> {
    if !original.original_verified() {
        return None;
    }
    Some(match stored {
        AnchorPrecision::ByteExact if text_is_source => AnchorPrecision::ByteExact,
        AnchorPrecision::ByteExact if has_node => AnchorPrecision::NodeExact,
        AnchorPrecision::ByteExact => AnchorPrecision::None,
        other => other,
    })
}

impl StoredCitation {
    /// Combines what is stored with what the caller observed of the live file.
    pub fn resolve(self, live: Option<&LiveObservation>) -> Resolution {
        let (original, limitation) = match self.retained {
            RetainedBytes::Verified => (CitationStatus::BytesResolved, None),
            retained => {
                let corrupt = (retained == RetainedBytes::Corrupt).then(|| {
                    "the retained bytes do not match their hash and are ignored".to_string()
                });
                match live {
                    None => (
                        CitationStatus::SourceUnavailable,
                        Some(corrupt.unwrap_or_else(|| {
                            "the original is not retained and the live file was not checked".into()
                        })),
                    ),
                    Some(LiveObservation::Hash(h)) if *h == self.anchor.source_bytes_hash => {
                        (CitationStatus::SourcePresentVerified, None)
                    }
                    Some(LiveObservation::Hash(_)) => (CitationStatus::SourceChanged, None),
                    Some(LiveObservation::Missing) => (CitationStatus::SourceMissing, None),
                    Some(LiveObservation::Unavailable(why)) => {
                        (CitationStatus::SourceUnavailable, Some(why.clone()))
                    }
                }
            }
        };
        let source_precision = source_precision_claim(
            self.stored_precision.min_rank(self.anchor.precision),
            self.text_is_source,
            !self.anchor.node_path.is_empty(),
            original,
        );
        Resolution {
            anchor: self.anchor,
            extraction_id: self.extraction_id,
            text: self.text,
            text_range: self.text_range,
            original,
            limitation,
            source_precision,
        }
    }
}

impl AnchorPrecision {
    /// The lower of two precisions.
    pub fn min_rank(self, other: AnchorPrecision) -> AnchorPrecision {
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }
}

// --------------------------------------------------------------------------------------- drafts

/// One node of the structure tree, as an adapter reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct DocNodeDraft {
    /// Ordinal path from the root; never empty. A parent's path is a prefix of its children's.
    pub path: Vec<u32>,
    pub kind: NodeKind,
    pub text_range: Option<(u64, u64)>,
    pub page: Option<u32>,
    pub bbox: Option<BBox>,
    pub precision: AnchorPrecision,
}

/// A document revision and one extraction of it, ready to persist. Nothing is parsed here: the
/// adapter that produced it (a later slice) supplies text, hashes and nodes.
#[derive(Debug, Clone)]
pub struct DocumentExtractionDraft {
    /// Where the document lives (its identity within the database: one logical source per location).
    pub location: String,
    pub title: String,
    pub media_type: String,
    /// Detected physical format (`pdf`, `docx`, `html`, `text`, ...).
    pub format: String,
    /// The extension-derived hint, recorded but never trusted.
    pub format_hint: Option<String>,
    pub detector_version: String,
    pub extractor_id: String,
    pub extractor_version: String,
    /// SHA-256 of the extraction parameters ([`params_hash`]).
    pub params_hash: String,
    /// SHA-256 of the original file bytes.
    pub source_bytes_hash: String,
    pub source_size: u64,
    /// True when `canonical_text` is byte-for-byte the original file (byte offsets coincide).
    pub text_is_source: bool,
    pub canonical_text: String,
    pub nodes: Vec<DocNodeDraft>,
    /// Explicit opt-in: keep the original bytes, content-addressed. `None` (the default) keeps only
    /// the hash. Must hash to `source_bytes_hash` and have `source_size` bytes.
    pub retain_bytes: Option<Vec<u8>>,
    pub observed_at: Ts,
}

impl DocumentExtractionDraft {
    /// SHA-256 of the canonical text: the revision's content identity.
    pub fn text_hash(&self) -> String {
        sha256_hex(self.canonical_text.as_bytes())
    }

    /// SHA-256 over everything an adapter decides: format, whether text is the source, and every
    /// node in path order. Two extractions with one identity must have one structure hash.
    pub fn structure_hash(&self) -> String {
        let mut nodes: Vec<&DocNodeDraft> = self.nodes.iter().collect();
        nodes.sort_by(|a, b| a.path.cmp(&b.path));
        let mut s = format!(
            "format={}\ntext_is_source={}\n",
            self.format, self.text_is_source
        );
        for n in nodes {
            let path: Vec<String> = n.path.iter().map(u32::to_string).collect();
            let _ = writeln!(
                s,
                "{}|{}|{:?}|{:?}|{:?}|{}",
                path.join("."),
                n.kind.as_str(),
                n.text_range,
                n.page,
                n.bbox.map(|b| (
                    b.x0.to_bits(),
                    b.y0.to_bits(),
                    b.x1.to_bits(),
                    b.y1.to_bits()
                )),
                n.precision.as_str()
            );
        }
        sha256_hex(s.as_bytes())
    }

    /// Validates everything that can be validated without a database. A draft that fails is rejected
    /// whole: nothing is stored.
    pub fn validate(&self) -> Result<()> {
        if self.location.is_empty() || self.location.len() > 4096 || self.location.contains('\0') {
            return invalid_doc("the location is empty, too long or contains NUL");
        }
        for (what, v, ok) in [
            (
                "format",
                &self.format,
                !self.format.is_empty() && self.format.len() <= 32,
            ),
            (
                "detector version",
                &self.detector_version,
                is_version(&self.detector_version),
            ),
            (
                "extractor id",
                &self.extractor_id,
                is_extractor_id(&self.extractor_id),
            ),
            (
                "extractor version",
                &self.extractor_version,
                is_version(&self.extractor_version),
            ),
        ] {
            if !ok {
                return invalid_doc(format!("the {what} '{v}' has an invalid spelling"));
            }
        }
        if !is_extractor_id(&self.format) {
            return invalid_doc("the format has an invalid spelling");
        }
        if let Some(h) = &self.format_hint {
            if !is_extractor_id(h) {
                return invalid_doc("the format hint has an invalid spelling");
            }
        }
        if self.title.len() > 4096
            || self.media_type.is_empty()
            || self.media_type.len() > 255
            || self.title.contains('\0')
            || self.media_type.contains('\0')
        {
            return invalid_doc("the title or media type is too long, empty or contains NUL");
        }
        if !is_sha256_hex(&self.source_bytes_hash) || !is_sha256_hex(&self.params_hash) {
            return invalid_doc("a hash is not a lowercase SHA-256 digest");
        }
        let text = self.canonical_text.as_bytes();
        if text.len() > MAX_CANONICAL_TEXT_BYTES {
            return invalid_doc("the canonical text exceeds the limit");
        }
        if text.contains(&0) {
            return invalid_doc("the canonical text contains NUL");
        }
        if self.nodes.len() > MAX_NODES {
            return invalid_doc("too many nodes");
        }
        if self.text_is_source {
            if self.canonical_text.len() as u64 != self.source_size {
                return invalid_doc("text_is_source, but the text and the original differ in size");
            }
            if self.text_hash() != self.source_bytes_hash {
                return invalid_doc(
                    "text_is_source, but the text does not hash to the original bytes",
                );
            }
        }
        if let Some(bytes) = &self.retain_bytes {
            if bytes.len() as u64 > MAX_RETAINED_BYTES {
                return invalid_doc("the original is larger than the retention limit");
            }
            if bytes.len() as u64 != self.source_size {
                return invalid_doc("the retained bytes do not have the declared size");
            }
            if sha256_hex(bytes) != self.source_bytes_hash {
                return invalid_doc("the retained bytes do not hash to the declared original hash");
            }
        }
        let mut paths: std::collections::BTreeSet<&[u32]> = std::collections::BTreeSet::new();
        for n in &self.nodes {
            if n.path.is_empty() || n.path.len() > MAX_NODE_DEPTH {
                return invalid_doc("a node path is empty or too deep");
            }
            if !paths.insert(&n.path) {
                return invalid_doc("two nodes have the same path");
            }
            if let Some(b) = &n.bbox {
                b.validate()
                    .map_err(|e| Error::InvalidDocument(e.to_string()))?;
            }
            check_shape(
                n.precision,
                true,
                n.text_range,
                n.page,
                n.bbox.as_ref(),
                true,
            )
            .or_else(|why| invalid_doc(format!("node {:?}: {why}", n.path)))?;
            if let Some((a, b)) = n.text_range {
                let (a, b) = (a as usize, b as usize);
                if b > self.canonical_text.len()
                    || !self.canonical_text.is_char_boundary(a)
                    || !self.canonical_text.is_char_boundary(b)
                {
                    return invalid_doc(format!(
                        "node {:?}: the text range is outside the text or splits a character",
                        n.path
                    ));
                }
            }
        }
        for n in &self.nodes {
            if n.path.len() > 1 && !paths.contains(&n.path[..n.path.len() - 1]) {
                return invalid_doc(format!("node {:?} has no parent node", n.path));
            }
        }
        Ok(())
    }
}

/// What persisting an extraction did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentExtractionReport {
    pub extraction_id: Uuid,
    pub logical_id: Uuid,
    pub source_id: Uuid,
    /// The document (logical source) was created by this call.
    pub document_created: bool,
    /// A new revision (canonical text) was created by this call.
    pub revision_created: bool,
    /// The extraction was created by this call; `false` = it already existed unchanged.
    pub extraction_created: bool,
    pub nodes: u64,
    /// The original bytes were stored by this call.
    pub bytes_retained_now: bool,
    /// Original bytes for this hash are present after the call.
    pub bytes_retained: bool,
}

/// One stored extraction, as listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentExtractionInfo {
    pub extraction_id: Uuid,
    pub logical_id: Uuid,
    pub source_id: Uuid,
    pub source_bytes_hash: String,
    pub text_hash: String,
    pub source_size: u64,
    pub text_is_source: bool,
    pub format: String,
    pub format_hint: Option<String>,
    pub detector_version: String,
    pub extractor_id: String,
    pub extractor_version: String,
    pub params_hash: String,
    pub node_count: u64,
    pub created_at: Ts,
    pub bytes_retained: bool,
}
