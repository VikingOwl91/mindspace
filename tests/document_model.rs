//! M8 S1: document identity, revisions, extraction identity, citation anchors, retention.
//!
//! No document is parsed here: drafts are built by hand, standing in for the adapters of later
//! slices. What is tested is the persistent foundation: identities, atomicity, append-only storage,
//! the honesty of anchors and citations, and that nothing existing moved (memories, producer
//! families, the file lifecycle, retrieval, migration of an existing database).

#[path = "support/schema_fixture.rs"]
mod schema_fixture;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::RwLock;

use mindspace::core::{
    params_hash, sha256_hex, Anchor, AnchorPrecision, BBox, CitationStatus, DocNodeDraft,
    DocumentExtractionDraft, Error, LiveObservation, NodeKind, SearchQuery, Ts, Uuid,
};
use mindspace::storage::Store;
use mindspace::{initialize, SqliteStore};

/// The commit-fault hooks (`FAIL_COMMIT`, `LOSE_ACK`) are process-global one-shot switches that ANY
/// commit in this process consumes. Every test therefore holds this lock shared (inside `Db`), and the
/// tests that arm a hook hold it exclusively, so an armed fault can only hit the commit it was meant for.
static HOOKS: RwLock<()> = RwLock::new(());

fn exclusive() -> std::sync::RwLockWriteGuard<'static, ()> {
    HOOKS.write().unwrap_or_else(|e| e.into_inner())
}

struct Db {
    _t: tempfile::TempDir,
    path: PathBuf,
    _shared: Option<std::sync::RwLockReadGuard<'static, ()>>,
}

impl Db {
    fn new() -> Self {
        let guard = HOOKS.read().unwrap_or_else(|e| e.into_inner());
        let mut db = Db::new_unlocked();
        db._shared = Some(guard);
        db
    }
    /// For the tests that already hold [`exclusive`].
    fn new_unlocked() -> Self {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("mind.db");
        initialize(&path).unwrap();
        Db {
            _t: t,
            path,
            _shared: None,
        }
    }
    fn open(&self) -> SqliteStore {
        SqliteStore::open(&self.path).unwrap()
    }
    fn raw(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.path).unwrap()
    }
    /// Every table's row count: the "nothing else changed" fingerprint.
    fn counts(&self) -> BTreeMap<String, i64> {
        let c = self.raw();
        let tables: Vec<String> = c
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '%_fts%' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect();
        tables
            .into_iter()
            .map(|t| {
                let n = c
                    .query_row(&format!("SELECT COUNT(*) FROM \"{t}\""), [], |r| r.get(0))
                    .unwrap();
                (t, n)
            })
            .collect()
    }
    fn count(&self, table: &str) -> i64 {
        self.counts()[table]
    }
    fn revisions(&self) -> i64 {
        self.count("m6_revisions")
    }
}

const LOC: &str = "/archive/report.pdf";
const TEXT: &str = "Title\nFirst paragraph with ü.\nSecond.\n";

fn pdf_bytes(tag: &str) -> Vec<u8> {
    format!("%PDF-1.7 synthetic {tag}").into_bytes()
}

fn params() -> String {
    params_hash(&serde_json::json!({"mode": "text", "columns": false}))
}

/// A PDF-like draft: text is NOT the source, nodes carry pages and boxes.
fn draft(bytes: &[u8], text: &str, extractor_version: &str) -> DocumentExtractionDraft {
    let title_end = text.find('\n').unwrap_or(0) as u64;
    let nodes = vec![
        DocNodeDraft {
            path: vec![1],
            kind: NodeKind::Page,
            text_range: None,
            page: Some(1),
            bbox: None,
            precision: AnchorPrecision::PageOnly,
        },
        DocNodeDraft {
            path: vec![1, 1],
            kind: NodeKind::Heading,
            text_range: Some((0, title_end)),
            page: Some(1),
            bbox: Some(BBox {
                x0: 72.0,
                y0: 700.5,
                x1: 300.25,
                y1: 716.0,
            }),
            precision: AnchorPrecision::NodeExact,
        },
        DocNodeDraft {
            path: vec![1, 2],
            kind: NodeKind::Paragraph,
            text_range: Some((title_end + 1, text.len() as u64)),
            page: Some(1),
            bbox: None,
            precision: AnchorPrecision::NodeExact,
        },
    ];
    DocumentExtractionDraft {
        location: LOC.to_string(),
        title: "Report".to_string(),
        media_type: "application/pdf".to_string(),
        format: "pdf".to_string(),
        format_hint: Some("pdf".to_string()),
        detector_version: "1".to_string(),
        extractor_id: "synthetic-pdf".to_string(),
        extractor_version: extractor_version.to_string(),
        params_hash: params(),
        source_bytes_hash: sha256_hex(bytes),
        source_size: bytes.len() as u64,
        text_is_source: false,
        canonical_text: text.to_string(),
        nodes,
        retain_bytes: None,
        observed_at: Ts::now(),
    }
}

/// A Markdown-like draft: the canonical text IS the file; byte ranges are exact.
fn text_draft(location: &str, text: &str) -> DocumentExtractionDraft {
    let bytes = text.as_bytes();
    DocumentExtractionDraft {
        location: location.to_string(),
        title: "Notes".to_string(),
        media_type: "text/markdown".to_string(),
        format: "text".to_string(),
        format_hint: Some("md".to_string()),
        detector_version: "1".to_string(),
        extractor_id: "synthetic-text".to_string(),
        extractor_version: "1".to_string(),
        params_hash: params(),
        source_bytes_hash: sha256_hex(bytes),
        source_size: bytes.len() as u64,
        text_is_source: true,
        canonical_text: text.to_string(),
        nodes: vec![DocNodeDraft {
            path: vec![1],
            kind: NodeKind::Paragraph,
            text_range: Some((0, bytes.len() as u64)),
            page: None,
            bbox: None,
            precision: AnchorPrecision::ByteExact,
        }],
        retain_bytes: None,
        observed_at: Ts::now(),
    }
}

fn anchor_for(
    d: &DocumentExtractionDraft,
    logical: Uuid,
    path: &[u32],
    range: Option<(u64, u64)>,
    page: Option<u32>,
    bbox: Option<BBox>,
    precision: AnchorPrecision,
) -> Anchor {
    Anchor::new(
        logical,
        &d.source_bytes_hash,
        &d.detector_version,
        &d.extractor_id,
        &d.extractor_version,
        &d.params_hash,
        path.to_vec(),
        range,
        page,
        bbox,
        precision,
    )
    .unwrap()
}

fn heading_anchor(d: &DocumentExtractionDraft, logical: Uuid) -> Anchor {
    anchor_for(
        d,
        logical,
        &[1, 1],
        Some((0, 5)),
        Some(1),
        Some(BBox {
            x0: 72.0,
            y0: 700.5,
            x1: 300.25,
            y1: 716.0,
        }),
        AnchorPrecision::NodeExact,
    )
}

// =========================================================================== anchors (pure)

fn sample_anchors() -> Vec<Anchor> {
    let d = draft(&pdf_bytes("a"), TEXT, "1");
    let l = Uuid::now_v7();
    vec![
        heading_anchor(&d, l),
        anchor_for(
            &d,
            l,
            &[1, 2],
            Some((6, 20)),
            None,
            None,
            AnchorPrecision::ByteExact,
        ),
        anchor_for(
            &d,
            l,
            &[],
            Some((0, 5)),
            None,
            None,
            AnchorPrecision::ByteExact,
        ),
        anchor_for(&d, l, &[3], None, None, None, AnchorPrecision::NodeExact),
        anchor_for(&d, l, &[], None, Some(7), None, AnchorPrecision::PageOnly),
        anchor_for(&d, l, &[], None, None, None, AnchorPrecision::None),
        anchor_for(
            &d,
            l,
            &[4, 1, 9],
            None,
            Some(2),
            Some(BBox {
                x0: 0.1,
                y0: 0.2,
                x1: 12.345679,
                y1: 3.4028235e38,
            }),
            AnchorPrecision::NodeExact,
        ),
    ]
}

#[test]
fn anchors_round_trip_through_text_without_losing_identity_or_precision() {
    for a in sample_anchors() {
        let s = a.to_string();
        let back: Anchor = s.parse().unwrap_or_else(|e| panic!("{s}: {e}"));
        assert_eq!(back, a, "{s}");
        assert_eq!(back.precision, a.precision);
        assert_eq!(back.to_string(), s, "one spelling per anchor");
        // an anchor is bound to its extraction identity
        assert_eq!(back.extraction_key(), a.extraction_key());
    }
}

#[test]
fn malformed_and_non_canonical_anchors_are_refused() {
    let good = sample_anchors()[0].to_string();
    let l = Uuid::now_v7().to_string();
    let h = "a".repeat(64);
    let base = format!("mind-doc:v1:{l}:{h}:1:x@1:{h}#");
    let cases: Vec<(String, &str)> = vec![
        (good.replace("mind-doc:v1:", "mind-doc:v2:"), "version"),
        (format!("{good} "), "trailing space"),
        (good.replace(";q=node_exact", ";q=NODE_EXACT"), "case"),
        (
            good.replace(&h_of(&good), &"A".repeat(64)),
            "uppercase hash",
        ),
        (format!("{base}1.2"), "no precision"),
        (format!("{base}01;q=node_exact"), "leading zero"),
        (
            format!("{base}1;q=node_exact;q=node_exact"),
            "duplicate field",
        ),
        (format!("{base}1;q=node_exact;x=1"), "unknown field"),
        (
            format!("{base}1;p=1;q=node_exact"),
            "non-canonical field order",
        ),
        (
            format!("{base}-;q=byte_exact"),
            "byte_exact without a range",
        ),
        (format!("{base}-;q=byte_exact;t=9-3"), "inverted range"),
        (
            format!("{base}1;q=page_only;p=1"),
            "page_only naming a node",
        ),
        (format!("{base}-;q=page_only"), "page_only without a page"),
        (format!("{base}-;q=page_only;p=0"), "page zero"),
        (
            format!("{base}1;q=node_exact;b=1,2,3,4"),
            "box without a page",
        ),
        (
            format!("{base}1;q=node_exact;p=1;b=3,2,1,4"),
            "inverted box",
        ),
        (format!("{base}1;q=node_exact;p=1;b=NaN,2,3,4"), "NaN box"),
        (format!("{base}-;q=node_exact"), "node_exact without a node"),
        (format!("{base}-;q=none;t=1-2"), "none with a location"),
        (
            format!("mind-doc:v1:not-a-uuid:{h}:x@1:{h}#-;q=none"),
            "bad uuid",
        ),
        (
            format!("mind-doc:v1:{l}:{}:x@1:{h}#-;q=none", "g".repeat(64)),
            "bad hash",
        ),
        (
            format!("mind-doc:v1:{l}:{h}:1:x:{h}#-;q=none"),
            "no version",
        ),
        (format!("mind-doc:v1:{l}:{h}:1:x@1#-;q=none"), "no params"),
        ("x".repeat(3000), "oversized"),
    ];
    for (s, why) in cases {
        assert!(s.parse::<Anchor>().is_err(), "{why}: accepted {s}");
    }
}

fn h_of(anchor: &str) -> String {
    anchor.split(':').nth(3).unwrap().to_string()
}

#[test]
fn precision_can_be_downgraded_but_never_raised_or_invented() {
    let d = draft(&pdf_bytes("a"), TEXT, "1");
    let l = Uuid::now_v7();
    let full = heading_anchor(&d, l); // node_exact with range, page and box
                                      // downgrade keeps what the lower level may carry, drops the rest
    let page = full.downgrade(AnchorPrecision::PageOnly).unwrap();
    assert_eq!(
        (page.page, page.text_range, page.bbox),
        (Some(1), None, None)
    );
    assert!(page.node_path.is_empty());
    let none = full.downgrade(AnchorPrecision::None).unwrap();
    assert_eq!(none.precision, AnchorPrecision::None);
    assert!(none.page.is_none() && none.text_range.is_none() && none.bbox.is_none());
    // a downgraded anchor still names the same extraction
    assert_eq!(page.extraction_key(), full.extraction_key());
    // never raised, even where the shape would allow it: a node_exact anchor that carries a range
    // is not thereby byte_exact
    let ranged = anchor_for(
        &d,
        l,
        &[1, 2],
        Some((6, 20)),
        None,
        None,
        AnchorPrecision::NodeExact,
    );
    assert!(ranged.downgrade(AnchorPrecision::ByteExact).is_err());
    // never raised
    assert!(none.downgrade(AnchorPrecision::NodeExact).is_err());
    assert!(page.downgrade(AnchorPrecision::NodeExact).is_err());
    // never invented: a byte range with no page cannot become page_only
    let bytes = anchor_for(
        &d,
        l,
        &[1, 2],
        Some((6, 20)),
        None,
        None,
        AnchorPrecision::ByteExact,
    );
    assert!(bytes.downgrade(AnchorPrecision::PageOnly).is_err());
    assert!(anchor_for(
        &d,
        l,
        &[],
        Some((0, 4)),
        None,
        None,
        AnchorPrecision::ByteExact
    )
    .downgrade(AnchorPrecision::NodeExact)
    .is_err());
    let node = bytes.downgrade(AnchorPrecision::NodeExact).unwrap();
    assert_eq!(node.text_range, Some((6, 20)));
}

// ================================================================= identities and persistence

#[test]
fn a_document_revision_is_stored_with_its_four_identities_in_one_recording_revision() {
    let db = Db::new();
    let store = db.open();
    let before = db.counts();
    let rev0 = db.revisions();
    let d = draft(&pdf_bytes("a"), TEXT, "1");
    let r = store.record_document_extraction(d.clone()).unwrap();
    assert!(r.document_created && r.revision_created && r.extraction_created);
    assert!(
        !r.bytes_retained && !r.bytes_retained_now,
        "original bytes are not kept by default"
    );
    assert_eq!(r.nodes, 3);
    assert_eq!(db.revisions(), rev0 + 1, "exactly one recording revision");
    let after = db.counts();
    let mut changed: BTreeMap<&str, i64> = BTreeMap::new();
    for (t, n) in &after {
        let delta = n - before[t];
        if delta != 0 {
            changed.insert(t.as_str(), delta);
        }
    }
    assert_eq!(changed["logical_sources"], 1);
    assert_eq!(changed["sources"], 1);
    assert_eq!(changed["doc_extractions"], 1);
    assert_eq!(changed["doc_nodes"], 3);
    assert_eq!(changed["m6_revisions"], 1);
    // nothing else: no evidence, assertions, memories, sessions, blobs...
    for t in [
        "evidence",
        "assertions",
        "memories",
        "ingest_sessions",
        "source_observations",
        "doc_blobs",
        "assertion_embeddings",
        "code_inputs",
    ] {
        assert!(!changed.contains_key(t), "{t} must not change: {changed:?}");
    }
    let c = db.raw();
    let (kind, state, loc): (String, String, String) = c
        .query_row(
            "SELECT kind, state, current_location FROM logical_sources WHERE id=?1",
            [r.logical_id.to_string()],
            |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (kind.as_str(), state.as_str(), loc.as_str()),
        ("document", "present", LOC)
    );
    let (skind, content, chash): (String, String, String) = c
        .query_row(
            "SELECT kind, content, content_hash FROM sources WHERE id=?1",
            [r.source_id.to_string()],
            |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?)),
        )
        .unwrap();
    assert_eq!(skind, "document");
    assert_eq!(content, TEXT);
    // the canonical-text hash and the original-bytes hash are different things
    assert_eq!(chash, sha256_hex(TEXT.as_bytes()));
    let list = store.list_document_extractions(r.logical_id).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].source_bytes_hash, d.source_bytes_hash);
    assert_eq!(list[0].text_hash, chash);
    assert_ne!(list[0].source_bytes_hash, list[0].text_hash);
    assert_eq!(
        (
            list[0].extractor_id.as_str(),
            list[0].extractor_version.as_str()
        ),
        ("synthetic-pdf", "1")
    );
    assert_eq!(list[0].detector_version, "1");
    assert_eq!(list[0].params_hash, d.params_hash);
    assert!(!list[0].bytes_retained && !list[0].text_is_source);
}

#[test]
fn an_identical_repeat_changes_nothing_and_consumes_no_revision() {
    let db = Db::new();
    let store = db.open();
    let d = draft(&pdf_bytes("a"), TEXT, "1");
    let first = store.record_document_extraction(d.clone()).unwrap();
    let counts = db.counts();
    let again = store.record_document_extraction(d.clone()).unwrap();
    assert_eq!(db.counts(), counts, "no row of any table changed");
    assert!(!again.document_created && !again.revision_created && !again.extraction_created);
    assert_eq!(
        (again.extraction_id, again.logical_id, again.source_id),
        (first.extraction_id, first.logical_id, first.source_id)
    );
    // and a reopened handle sees the same
    drop(store);
    let store = db.open();
    store.record_document_extraction(d).unwrap();
    assert_eq!(db.counts(), counts);
}

#[test]
fn equal_text_with_different_original_bytes_are_two_extractions_of_one_revision() {
    let db = Db::new();
    let store = db.open();
    let a = draft(&pdf_bytes("export-1"), TEXT, "1");
    let mut b = draft(&pdf_bytes("export-2"), TEXT, "1");
    // different geometry for the same words
    b.nodes[1].bbox = Some(BBox {
        x0: 10.0,
        y0: 20.0,
        x1: 99.0,
        y1: 40.0,
    });
    let ra = store.record_document_extraction(a.clone()).unwrap();
    let rb = store.record_document_extraction(b.clone()).unwrap();
    assert_eq!(ra.logical_id, rb.logical_id, "one document");
    assert_eq!(
        ra.source_id, rb.source_id,
        "one revision: the canonical text is the same"
    );
    assert!(!rb.revision_created && rb.extraction_created);
    assert_ne!(
        ra.extraction_id, rb.extraction_id,
        "two extractions: the original bytes differ"
    );
    assert_eq!(db.count("sources"), 1);
    assert_eq!(db.count("doc_extractions"), 2);
    // each anchor resolves against ITS bytes, and only against them
    let anchor_a = heading_anchor(&a, ra.logical_id);
    let cited = store.resolve_document_anchor(&anchor_a).unwrap().unwrap();
    assert_eq!(cited.extraction_id, ra.extraction_id);
    let anchor_b = anchor_for(
        &b,
        rb.logical_id,
        &[1, 1],
        Some((0, 5)),
        Some(1),
        b.nodes[1].bbox,
        AnchorPrecision::NodeExact,
    );
    assert_eq!(
        store
            .resolve_document_anchor(&anchor_b)
            .unwrap()
            .unwrap()
            .extraction_id,
        rb.extraction_id
    );
    // a's geometry attached to b's bytes is refused, not silently accepted
    let mixed = anchor_for(
        &b,
        rb.logical_id,
        &[1, 1],
        Some((0, 5)),
        Some(1),
        a.nodes[1].bbox,
        AnchorPrecision::NodeExact,
    );
    assert!(matches!(
        store.resolve_document_anchor(&mixed),
        Err(Error::InvalidAnchor(_))
    ));
}

#[test]
fn the_same_bytes_with_a_new_extractor_version_or_parameters_add_an_extraction_not_a_revision() {
    let db = Db::new();
    let store = db.open();
    let v1 = draft(&pdf_bytes("a"), TEXT, "1");
    let r1 = store.record_document_extraction(v1.clone()).unwrap();
    let mut v2 = draft(&pdf_bytes("a"), TEXT, "2");
    v2.nodes.pop(); // a different extractor sees less
    let r2 = store.record_document_extraction(v2.clone()).unwrap();
    assert_eq!(r1.source_id, r2.source_id);
    assert!(r2.extraction_created && !r2.revision_created);
    let mut p2 = draft(&pdf_bytes("a"), TEXT, "1");
    p2.params_hash = params_hash(&serde_json::json!({"mode": "layout"}));
    let r3 = store.record_document_extraction(p2).unwrap();
    assert!(r3.extraction_created);
    assert_eq!(db.count("doc_extractions"), 3);
    // history is stable: the first extraction and its anchors still resolve, unchanged
    let old = heading_anchor(&v1, r1.logical_id);
    let c = store.resolve_document_anchor(&old).unwrap().unwrap();
    assert_eq!(c.extraction_id, r1.extraction_id);
    assert_eq!(c.text.as_deref(), Some("Title"));
    // the old extractor's node does not exist in version 2's tree: that anchor says so
    let gone = anchor_for(
        &v2,
        r2.logical_id,
        &[1, 2],
        Some((6, 20)),
        None,
        None,
        AnchorPrecision::ByteExact,
    );
    assert!(matches!(
        store.resolve_document_anchor(&gone),
        Err(Error::InvalidAnchor(_))
    ));
}

#[test]
fn an_extractor_that_answers_differently_for_one_identity_is_refused_not_merged() {
    let db = Db::new();
    let store = db.open();
    let d = draft(&pdf_bytes("a"), TEXT, "1");
    store.record_document_extraction(d.clone()).unwrap();
    let counts = db.counts();
    // same identity, different structure
    let mut other = d.clone();
    other.nodes[1].precision = AnchorPrecision::PageOnly;
    other.nodes[1].text_range = None;
    other.nodes[1].bbox = None;
    assert!(
        matches!(store.record_document_extraction(other), Err(Error::InvalidDocument(m)) if m.contains("structure"))
    );
    // same identity, different canonical text
    let mut text = d.clone();
    text.canonical_text = "Title\nAnother reading.\n".to_string();
    text.nodes[2].text_range = Some((6, 22));
    assert!(
        matches!(store.record_document_extraction(text), Err(Error::InvalidDocument(m)) if m.contains("canonical text"))
    );
    assert_eq!(
        db.counts(),
        counts,
        "the stored extraction was neither replaced nor extended"
    );
}

#[test]
fn a_new_revision_of_a_document_keeps_the_old_one_and_its_citations() {
    let db = Db::new();
    let store = db.open();
    let old = draft(&pdf_bytes("v1"), TEXT, "1");
    let r_old = store.record_document_extraction(old.clone()).unwrap();
    let new_text = "Title\nFirst paragraph, edited.\nSecond.\n";
    let new = draft(&pdf_bytes("v2"), new_text, "1");
    let r_new = store.record_document_extraction(new.clone()).unwrap();
    assert_eq!(
        r_old.logical_id, r_new.logical_id,
        "same location = same document"
    );
    assert_ne!(r_old.source_id, r_new.source_id);
    assert!(r_new.revision_created && !r_new.document_created);
    assert_eq!(db.count("sources"), 2);
    // the same node, cited without an explicit range, quotes each revision's own paragraph
    let a_old = anchor_for(
        &old,
        r_old.logical_id,
        &[1, 2],
        None,
        None,
        None,
        AnchorPrecision::NodeExact,
    );
    let a_new = anchor_for(
        &new,
        r_new.logical_id,
        &[1, 2],
        None,
        None,
        None,
        AnchorPrecision::NodeExact,
    );
    let c_old = store.resolve_document_anchor(&a_old).unwrap().unwrap();
    let c_new = store.resolve_document_anchor(&a_new).unwrap().unwrap();
    assert_eq!(c_old.text.as_deref(), Some(&TEXT[6..]));
    assert_eq!(c_new.text.as_deref(), Some(&new_text[6..]));
    assert_ne!(
        c_old.text, c_new.text,
        "each citation quotes its own revision"
    );
    // an anchor of one revision never resolves against the other
    let crossed = anchor_for(
        &old,
        r_old.logical_id,
        &[1, 2],
        None,
        None,
        None,
        AnchorPrecision::NodeExact,
    );
    assert_eq!(
        store
            .resolve_document_anchor(&crossed)
            .unwrap()
            .unwrap()
            .extraction_id,
        r_old.extraction_id
    );
    assert_eq!(
        store
            .list_document_extractions(r_old.logical_id)
            .unwrap()
            .len(),
        2
    );
}

// ============================================================================ rejected input

#[test]
fn rejected_drafts_store_nothing_and_consume_no_revision() {
    let db = Db::new();
    let store = db.open();
    let counts = db.counts();
    let good = draft(&pdf_bytes("a"), TEXT, "1");
    let mut bad: Vec<(&str, DocumentExtractionDraft)> = Vec::new();
    let mut m = |why, f: &dyn Fn(&mut DocumentExtractionDraft)| {
        let mut d = good.clone();
        f(&mut d);
        bad.push((why, d));
    };
    m("short hash", &|d| d.source_bytes_hash = "abc".into());
    m("upper hash", &|d| d.source_bytes_hash = "A".repeat(64));
    m("params hash", &|d| d.params_hash = "zz".into());
    m("NUL in text", &|d| d.canonical_text = "a\0b".into());
    m("extractor id", &|d| d.extractor_id = "bad id@".into());
    m("extractor version", &|d| d.extractor_version = "1:2".into());
    m("empty location", &|d| d.location = String::new());
    m("range past the text", &|d| {
        d.nodes[2].text_range = Some((0, 9999))
    });
    m("range splits a character", &|d| {
        let at = TEXT.find('ü').unwrap() as u64 + 1; // inside the two-byte ü
        d.nodes[2].text_range = Some((at, at + 2));
    });
    m("inverted range", &|d| d.nodes[2].text_range = Some((9, 3)));
    m("duplicate path", &|d| d.nodes[2].path = vec![1, 1]);
    m("orphan node", &|d| d.nodes[2].path = vec![7, 7]);
    m("empty path", &|d| d.nodes[0].path = vec![]);
    m("byte_exact on a node with no range", &|d| {
        d.nodes[0].precision = AnchorPrecision::ByteExact;
    });
    m("page_only without page", &|d| {
        d.nodes[0].page = None;
    });
    m("bbox without page", &|d| {
        d.nodes[2].page = None;
        d.nodes[2].bbox = Some(BBox {
            x0: 1.0,
            y0: 1.0,
            x1: 2.0,
            y1: 2.0,
        });
    });
    m("inverted bbox", &|d| {
        d.nodes[1].bbox = Some(BBox {
            x0: 9.0,
            y0: 1.0,
            x1: 2.0,
            y1: 2.0,
        })
    });
    m("NaN bbox", &|d| {
        d.nodes[1].bbox = Some(BBox {
            x0: f32::NAN,
            y0: 1.0,
            x1: 2.0,
            y1: 2.0,
        })
    });
    m("page zero", &|d| d.nodes[0].page = Some(0));
    m("none claiming a page", &|d| {
        d.nodes[0].precision = AnchorPrecision::None
    });
    m("text_is_source mismatch", &|d| d.text_is_source = true);
    m("retained bytes of another file", &|d| {
        d.retain_bytes = Some(pdf_bytes("other"))
    });
    m("retained bytes of the wrong size", &|d| {
        let mut b = pdf_bytes("a");
        b.push(0);
        d.source_size = b.len() as u64;
        d.retain_bytes = Some(b);
    });
    for (why, d) in bad {
        let r = store.record_document_extraction(d);
        assert!(matches!(r, Err(Error::InvalidDocument(_))), "{why}: {r:?}");
        assert_eq!(db.counts(), counts, "{why}: rows changed");
    }
}

// ============================================================================ fault injection

fn fault_trigger(db: &Db, table: &str, when: &str) {
    db.raw()
        .execute_batch(&format!(
            "CREATE TRIGGER fault BEFORE INSERT ON {table} {when} \
             BEGIN SELECT RAISE(ABORT, 'injected fault'); END;"
        ))
        .unwrap();
}

#[test]
fn a_failure_at_any_row_leaves_no_partial_document() {
    // each fault hits a different point of the one transaction
    for (table, when) in [
        ("sources", ""),
        ("doc_extractions", ""),
        ("doc_nodes", "WHEN NEW.path = '1.2'"),
        ("doc_nodes", "WHEN NEW.path = '1'"),
        ("doc_blobs", ""),
    ] {
        let db = Db::new();
        let store = db.open();
        let counts = db.counts();
        let mut d = draft(&pdf_bytes("a"), TEXT, "1");
        d.retain_bytes = Some(pdf_bytes("a"));
        fault_trigger(&db, table, when);
        let err = store.record_document_extraction(d.clone()).unwrap_err();
        assert!(err.to_string().contains("injected fault"), "{table}: {err}");
        assert_eq!(db.counts(), counts, "{table} {when}: partial rows remain");
        // the handle is still usable and a retry after the fault is removed succeeds in full
        db.raw().execute_batch("DROP TRIGGER fault").unwrap();
        let r = store.record_document_extraction(d).unwrap();
        assert!(r.extraction_created && r.bytes_retained_now);
        assert_eq!(db.count("doc_nodes"), 3);
        assert_eq!(db.count("doc_blobs"), 1);
        assert_eq!(
            db.revisions(),
            counts["m6_revisions"] + 1,
            "{table}: failed attempts cost no revision"
        );
    }
}

#[test]
fn a_vetoed_commit_rolls_back_and_a_lost_acknowledgement_is_reported_as_unknown() {
    let _g = exclusive();
    let db = Db::new_unlocked();
    let store = db.open();
    let counts = db.counts();
    let d = draft(&pdf_bytes("a"), TEXT, "1");
    mindspace::storage::sqlite::gate::FAIL_COMMIT.store(true, std::sync::atomic::Ordering::SeqCst);
    let e = store.record_document_extraction(d.clone()).unwrap_err();
    assert!(
        !matches!(e, Error::CommitOutcomeUnknown { .. }),
        "a veto is a definite rollback: {e}"
    );
    assert_eq!(db.counts(), counts);
    // the handle stays usable
    let ok = store.record_document_extraction(d.clone()).unwrap();
    assert!(ok.extraction_created);
    // a lost acknowledgement: the data IS committed, the caller is told it cannot know
    let db2 = Db::new_unlocked();
    let store2 = db2.open();
    mindspace::storage::sqlite::gate::LOSE_ACK.store(true, std::sync::atomic::Ordering::SeqCst);
    let e = store2.record_document_extraction(d.clone()).unwrap_err();
    assert!(matches!(e, Error::CommitOutcomeUnknown { .. }), "{e}");
    assert_eq!(
        db2.count("doc_extractions"),
        1,
        "committed although unacknowledged"
    );
    // a retry after reopening is idempotent
    drop(store2);
    let store2 = db2.open();
    let again = store2.record_document_extraction(d).unwrap();
    assert!(!again.extraction_created);
    assert_eq!(db2.count("doc_extractions"), 1);
}

// =============================================================================== append-only

#[test]
fn stored_documents_cannot_be_rewritten_and_the_schema_refuses_inconsistent_rows() {
    let db = Db::new();
    let store = db.open();
    let r = store
        .record_document_extraction(draft(&pdf_bytes("a"), TEXT, "1"))
        .unwrap();
    let c = db.raw();
    for sql in [
        "UPDATE doc_extractions SET format='x'",
        "DELETE FROM doc_extractions",
        "UPDATE doc_nodes SET kind='page'",
        "DELETE FROM doc_nodes",
    ] {
        let e = c.execute(sql, []).unwrap_err().to_string();
        assert!(e.contains("append-only"), "{sql}: {e}");
    }
    // an extraction can only point at a document source of its own logical source
    let md_logical = Uuid::now_v7().to_string();
    let insert = |source: &str, logical: &str| {
        c.execute(
            "INSERT INTO doc_extractions (id, logical_id, source_id, source_bytes_hash, source_size, text_is_source, \
             format, detector_version, extractor_id, extractor_version, params_hash, structure_hash, node_count, created_at) \
             VALUES (?1,?2,?3,?4,0,0,'pdf','d','e','1',?4,?4,0,0)",
            rusqlite::params![Uuid::now_v7().to_string(), logical, source, "b".repeat(64)],
        )
    };
    let e = insert(&r.source_id.to_string(), &md_logical)
        .unwrap_err()
        .to_string();
    assert!(e.contains("document source"), "{e}");
    // node ranges beyond the text, and precision without its backing facts
    let node = |extra: &str| {
        c.execute(
            &format!(
                "INSERT INTO doc_nodes (extraction_id, path, kind, text_start, text_end, page_no, precision) VALUES ('{}', '9', 'paragraph', {extra})",
                r.extraction_id
            ),
            [],
        )
    };
    assert!(node("0, 99999, NULL, 'byte_exact'")
        .unwrap_err()
        .to_string()
        .contains("outside the canonical text"));
    assert!(
        node("NULL, NULL, NULL, 'byte_exact'").is_err(),
        "byte_exact needs a range"
    );
    assert!(
        node("NULL, NULL, NULL, 'page_only'").is_err(),
        "page_only needs a page"
    );
    assert!(node("0, 3, 2, 'none'").is_err(), "none carries nothing");
    assert!(node("3, 1, NULL, 'node_exact'").is_err(), "inverted range");
}

// ================================================================================== retention

#[test]
fn original_bytes_are_kept_only_on_request_and_erasure_leaves_the_text_citation() {
    let db = Db::new();
    let store = db.open();
    let bytes = pdf_bytes("keep-me");
    let mut d = draft(&bytes, TEXT, "1");
    d.retain_bytes = Some(bytes.clone());
    let r = store.record_document_extraction(d.clone()).unwrap();
    assert!(r.bytes_retained && r.bytes_retained_now);
    assert_eq!(db.count("doc_blobs"), 1);
    let a = heading_anchor(&d, r.logical_id);
    let res = store
        .resolve_document_anchor(&a)
        .unwrap()
        .unwrap()
        .resolve(None);
    assert_eq!(
        res.original,
        CitationStatus::BytesResolved,
        "retained bytes need no live check"
    );
    assert_eq!(
        res.statuses(),
        vec![CitationStatus::TextResolved, CitationStatus::BytesResolved]
    );
    assert_eq!(res.text.as_deref(), Some("Title"));
    assert_eq!(
        res.source_precision,
        Some(AnchorPrecision::NodeExact),
        "geometry may be claimed: the original is verified"
    );
    // erasure: text and anchors stay, byte-level resolution goes, and the report says so
    assert!(store.erase_document_bytes(&d.source_bytes_hash).unwrap());
    assert!(
        !store.erase_document_bytes(&d.source_bytes_hash).unwrap(),
        "nothing left to erase"
    );
    assert_eq!(db.count("doc_blobs"), 0);
    assert_eq!(db.count("doc_extractions"), 1);
    let res = store
        .resolve_document_anchor(&a)
        .unwrap()
        .unwrap()
        .resolve(None);
    assert_eq!(res.original, CitationStatus::SourceUnavailable);
    assert!(res.limitation.as_deref().unwrap().contains("not retained"));
    assert_eq!(
        res.text.as_deref(),
        Some("Title"),
        "the text citation survives erasure"
    );
    assert_eq!(
        res.source_precision, None,
        "no geometric claim without the original"
    );
    // bytes can be retained later for an existing extraction, without adding another extraction
    let again = store.record_document_extraction(d.clone()).unwrap();
    assert!(!again.extraction_created && again.bytes_retained_now && again.bytes_retained);
    assert_eq!(db.count("doc_extractions"), 1);
}

#[test]
fn retained_bytes_are_verified_on_every_resolution() {
    let db = Db::new();
    let store = db.open();
    let bytes = pdf_bytes("x");
    let mut d = draft(&bytes, TEXT, "1");
    d.retain_bytes = Some(bytes.clone());
    let r = store.record_document_extraction(d.clone()).unwrap();
    let a = heading_anchor(&d, r.logical_id);
    // tamper with the stored blob (replace the row with other content under the same key)
    let c = db.raw();
    c.execute("DELETE FROM doc_blobs", []).unwrap();
    c.execute(
        "INSERT INTO doc_blobs (bytes_hash, size, bytes, created_at) VALUES (?1, 3, X'414243', 0)",
        [&d.source_bytes_hash],
    )
    .unwrap();
    let res = store
        .resolve_document_anchor(&a)
        .unwrap()
        .unwrap()
        .resolve(None);
    assert_ne!(
        res.original,
        CitationStatus::BytesResolved,
        "a corrupt blob must not count as retained"
    );
    assert_eq!(res.original, CitationStatus::SourceUnavailable);
    assert!(res.limitation.as_deref().unwrap().contains("do not match"));
    // blobs are never updated in place
    assert!(c
        .execute("UPDATE doc_blobs SET size = 3", [])
        .unwrap_err()
        .to_string()
        .contains("never updated"));
}

// ================================================================== citations over time

#[test]
fn a_citation_reports_replacement_deletion_and_unavailability_of_the_original_explicitly() {
    let db = Db::new();
    let store = db.open();
    let files = tempfile::tempdir().unwrap();
    let path = files.path().join("doc.bin");
    let original = pdf_bytes("original");
    std::fs::write(&path, &original).unwrap();
    let mut d = draft(&original, TEXT, "1");
    d.location = path.to_str().unwrap().to_string();
    let r = store.record_document_extraction(d.clone()).unwrap();
    let a = heading_anchor(&d, r.logical_id);
    let live = |p: &std::path::Path| match std::fs::read(p) {
        Ok(b) => LiveObservation::Hash(sha256_hex(&b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => LiveObservation::Missing,
        Err(e) => LiveObservation::Unavailable(e.to_string()),
    };
    let cite = || store.resolve_document_anchor(&a).unwrap().unwrap();

    // present and unchanged: verified against the cited hash, nothing retained
    let res = cite().resolve(Some(&live(&path)));
    assert_eq!(res.original, CitationStatus::SourcePresentVerified);
    assert_eq!(
        res.statuses(),
        vec![
            CitationStatus::TextResolved,
            CitationStatus::SourcePresentVerified
        ]
    );
    assert_eq!(res.source_precision, Some(AnchorPrecision::NodeExact));

    // never checked: unavailable, with the reason, and no claim about the original
    let res = cite().resolve(None);
    assert_eq!(res.original, CitationStatus::SourceUnavailable);
    assert!(res.limitation.is_some());
    assert_eq!(res.source_precision, None);
    assert_eq!(
        res.text.as_deref(),
        Some("Title"),
        "the extracted text is still there"
    );

    // replaced by other content: the citation still resolves to its own text and says the file changed
    std::fs::write(&path, pdf_bytes("replacement")).unwrap();
    let res = cite().resolve(Some(&live(&path)));
    assert_eq!(res.original, CitationStatus::SourceChanged);
    assert_eq!(res.text.as_deref(), Some("Title"));
    assert_eq!(
        res.source_precision, None,
        "geometry of a file that changed is not claimed"
    );
    assert!(!res.original.original_verified());

    // deleted
    std::fs::remove_file(&path).unwrap();
    let res = cite().resolve(Some(&live(&path)));
    assert_eq!(res.original, CitationStatus::SourceMissing);
    assert_eq!(res.text.as_deref(), Some("Title"));

    // unreadable
    let res = cite().resolve(Some(&LiveObservation::Unavailable(
        "permission denied".into(),
    )));
    assert_eq!(res.original, CitationStatus::SourceUnavailable);
    assert_eq!(res.limitation.as_deref(), Some("permission denied"));
    // a hash is a comparison, never retrieval: nothing above produced original bytes
    assert_eq!(db.count("doc_blobs"), 0);
}

#[test]
fn byte_exact_claims_about_the_original_need_the_text_to_be_the_original() {
    let db = Db::new();
    let store = db.open();
    // a text document: canonical text == file
    let t = text_draft("/notes/a.txt", "alpha beta gamma\n");
    let rt = store.record_document_extraction(t.clone()).unwrap();
    let at = anchor_for(
        &t,
        rt.logical_id,
        &[],
        Some((6, 10)),
        None,
        None,
        AnchorPrecision::ByteExact,
    );
    let c = store.resolve_document_anchor(&at).unwrap().unwrap();
    assert_eq!(c.text.as_deref(), Some("beta"));
    let verified = LiveObservation::Hash(t.source_bytes_hash.clone());
    assert_eq!(
        c.clone().resolve(Some(&verified)).source_precision,
        Some(AnchorPrecision::ByteExact)
    );
    assert_eq!(
        c.resolve(None).source_precision,
        None,
        "text survives; the original is unchecked"
    );
    // a PDF-like document: byte offsets of the canonical text are not offsets in the file
    let d = draft(&pdf_bytes("a"), TEXT, "1");
    let rd = store.record_document_extraction(d.clone()).unwrap();
    let ad = anchor_for(
        &d,
        rd.logical_id,
        &[1, 2],
        Some((6, 20)),
        None,
        None,
        AnchorPrecision::ByteExact,
    );
    let c = store.resolve_document_anchor(&ad).unwrap().unwrap();
    assert_eq!(c.text.as_deref(), Some(&TEXT[6..20]));
    let verified = LiveObservation::Hash(d.source_bytes_hash.clone());
    assert_eq!(
        c.resolve(Some(&verified)).source_precision,
        Some(AnchorPrecision::NodeExact),
        "byte_exact is exact in the canonical text only: downgraded for the original"
    );
}

#[test]
fn an_anchor_resolves_only_against_the_revision_and_extractor_it_names() {
    let db = Db::new();
    let store = db.open();
    let d = draft(&pdf_bytes("a"), TEXT, "1");
    let r = store.record_document_extraction(d.clone()).unwrap();
    let ok = heading_anchor(&d, r.logical_id);
    assert!(store.resolve_document_anchor(&ok).unwrap().is_some());
    // wrong revision (other bytes), other extractor version, other parameters, other document
    let mut wrong_bytes = ok.clone();
    wrong_bytes.source_bytes_hash = sha256_hex(b"another file");
    let mut wrong_version = ok.clone();
    wrong_version.extractor_version = "2".into();
    let mut wrong_detector = ok.clone();
    wrong_detector.detector_version = "2".into();
    let mut wrong_params = ok.clone();
    wrong_params.params_hash = params_hash(&serde_json::json!({"x": 1}));
    let mut wrong_detector = ok.clone();
    wrong_detector.detector_version = "2".into();
    let mut wrong_doc = ok.clone();
    wrong_doc.logical_id = Uuid::now_v7();
    for (why, a) in [
        ("bytes", wrong_bytes),
        ("extractor version", wrong_version),
        ("detector version", wrong_detector),
        ("params", wrong_params),
        ("document", wrong_doc),
    ] {
        assert!(
            store.resolve_document_anchor(&a).unwrap().is_none(),
            "{why}: resolved against another identity"
        );
    }
    // within the right extraction, an anchor may not claim more than is stored
    let l = r.logical_id;
    let refused = [
        (
            "missing node",
            anchor_for(&d, l, &[9, 9], None, None, None, AnchorPrecision::NodeExact),
        ),
        // the page node stores no text range and no box
        (
            "range on a node without one",
            anchor_for(
                &d,
                l,
                &[1],
                Some((0, 3)),
                Some(1),
                None,
                AnchorPrecision::NodeExact,
            ),
        ),
        (
            "fabricated box",
            anchor_for(
                &d,
                l,
                &[1, 2],
                None,
                Some(1),
                Some(BBox {
                    x0: 1.0,
                    y0: 1.0,
                    x1: 2.0,
                    y1: 2.0,
                }),
                AnchorPrecision::NodeExact,
            ),
        ),
        (
            "other page",
            anchor_for(
                &d,
                l,
                &[1, 1],
                Some((0, 5)),
                Some(2),
                d.nodes[1].bbox,
                AnchorPrecision::NodeExact,
            ),
        ),
        (
            "page nobody stores",
            anchor_for(&d, l, &[], None, Some(42), None, AnchorPrecision::PageOnly),
        ),
        (
            "range outside the node",
            anchor_for(
                &d,
                l,
                &[1, 1],
                Some((0, 12)),
                Some(1),
                d.nodes[1].bbox,
                AnchorPrecision::NodeExact,
            ),
        ),
        (
            "range beyond the text",
            anchor_for(
                &d,
                l,
                &[],
                Some((0, 9999)),
                None,
                None,
                AnchorPrecision::ByteExact,
            ),
        ),
        ("range inside a character", {
            let at = TEXT.find('ü').unwrap() as u64 + 1;
            anchor_for(
                &d,
                l,
                &[],
                Some((at, at + 2)),
                None,
                None,
                AnchorPrecision::ByteExact,
            )
        }),
        // the page node only supports page_only: byte_exact there would be invented
        (
            "precision above the node's",
            anchor_for(
                &d,
                l,
                &[1],
                Some((0, 3)),
                None,
                None,
                AnchorPrecision::ByteExact,
            ),
        ),
    ];
    for (why, a) in refused {
        let r = store.resolve_document_anchor(&a);
        assert!(matches!(r, Err(Error::InvalidAnchor(_))), "{why}: {r:?}");
    }
    // a downgraded anchor of the same extraction still resolves, and says less
    let page = ok.downgrade(AnchorPrecision::PageOnly).unwrap();
    let c = store.resolve_document_anchor(&page).unwrap().unwrap();
    assert_eq!(c.text, None, "a page-only citation quotes nothing");
    assert_eq!(c.anchor.precision, AnchorPrecision::PageOnly);
}

// ============================================================================ compatibility

fn search_texts(store: &SqliteStore, q: &str) -> Vec<String> {
    mindspace::retrieval::search(
        store,
        SearchQuery {
            text: q.to_string(),
            space_ids: None,
            limit: 20,
            at: None,
            include_superseded: false,
            include_retired: false,
        },
    )
    .unwrap()
    .into_iter()
    .map(|h| format!("{}|{}", h.subject, h.content))
    .collect()
}

#[test]
fn documents_do_not_enter_the_file_lifecycle_memories_or_retrieval() {
    let db = Db::new();
    let store = db.open();
    let space = store.create_space("p", "").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::write(root.join("a.md"), "# Alpha\n\nzebra content here\n").unwrap();
    mindspace::ingest::ingest_path(&store, &root, &["p"]).unwrap();
    let families = |db: &Db| -> Vec<String> {
        db.raw()
            .prepare("SELECT DISTINCT producer_family FROM memories ORDER BY 1")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect()
    };
    let fam_before = families(&db);
    let hits_before = search_texts(&store, "zebra");
    assert!(!hits_before.is_empty());

    // documents under the same root, and one at the SAME location as a markdown file
    let mut pdf = draft(&pdf_bytes("a"), TEXT, "1");
    pdf.location = root.join("report.pdf").to_str().unwrap().to_string();
    let mut shadow = draft(&pdf_bytes("b"), "zebra shadow text\n", "1");
    shadow.location = root.join("a.md").to_str().unwrap().to_string();
    shadow.nodes.truncate(1);
    let rp = store.record_document_extraction(pdf).unwrap();
    let rs = store.record_document_extraction(shadow).unwrap();
    let mem_before = db.count("memories");
    let assertions_before = db.count("assertions");

    // directory and file re-ingests of that root: documents are neither absented nor moved nor reconciled
    mindspace::ingest::ingest_path(&store, &root, &["p"]).unwrap();
    mindspace::ingest::ingest_path(&store, &root.join("a.md"), &["p"]).unwrap();
    std::fs::write(
        root.join("a.md"),
        "# Alpha\n\nzebra content here, changed\n",
    )
    .unwrap();
    mindspace::ingest::ingest_path(&store, &root, &["p"]).unwrap();
    let c = db.raw();
    for id in [rp.logical_id, rs.logical_id] {
        let (kind, state): (String, String) = c
            .query_row(
                "SELECT kind, state FROM logical_sources WHERE id=?1",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((kind.as_str(), state.as_str()), ("document", "present"));
        let events: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM source_events WHERE logical_id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            events, 0,
            "no move or disappearance may be inferred for a document"
        );
        let obs: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM source_observations WHERE logical_id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(obs, 0);
        let versions: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sources WHERE logical_id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(versions, 1, "no revision was added by a file ingest");
    }
    // the markdown path still belongs to its own logical source and its own family
    assert_eq!(families(&db), fam_before);
    assert_eq!(families(&db), vec!["file:legacy".to_string()]);
    assert!(db.count("memories") >= mem_before);
    assert!(db.count("assertions") >= assertions_before);
    // search finds the markdown content and never the document text
    let hits = search_texts(&store, "zebra");
    assert!(
        hits.iter().all(|h| !h.contains("shadow")),
        "document text must not be searchable in S1: {hits:?}"
    );
    assert!(hits.iter().any(|h| h.contains("changed")));
    let _ = space;
}

#[test]
fn an_existing_database_migrates_and_keeps_everything_it_had() {
    let db = Db::new();
    {
        let store = db.open();
        store.create_space("p", "").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("n.md"), "# Notes\n\nzebra before the migration\n").unwrap();
        std::fs::write(root.join("l.rs"), "pub fn lion() {}\n").unwrap();
        mindspace::ingest::ingest_path(&store, &root, &["p"]).unwrap();
    }
    let before_counts = db.counts();
    let before_hits = search_texts(&db.open(), "zebra");
    assert!(!before_hits.is_empty());
    // make it a schema-9 database: no document tables, no migration-10 record
    let c = db.raw();
    schema_fixture::remove_empty_ownership_metadata(&c);
    c.execute_batch(
        "DELETE FROM schema_migrations WHERE version IN (10, 11, 12);
         DROP TABLE doc_chunks; DROP TABLE doc_chunk_sets; DROP TABLE doc_nodes; DROP TABLE doc_extractions; DROP TABLE doc_blobs;
         DROP INDEX uq_logical_documents_location;",
    )
    .unwrap();
    let max: i64 = c
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(max, 9);
    drop(c);
    // opening migrates
    let store = db.open();
    let after = db.counts();
    for (t, n) in &before_counts {
        assert_eq!(after.get(t), Some(n), "table {t} changed by the migration");
    }
    assert_eq!(after["doc_extractions"], 0);
    assert_eq!(after["doc_nodes"], 0);
    assert_eq!(after["doc_blobs"], 0);
    let max: i64 = db
        .raw()
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(max, 17);
    assert_eq!(
        search_texts(&store, "zebra"),
        before_hits,
        "retrieval is unchanged"
    );
    store.verify_fts_index().unwrap();
    let ok: String = db
        .raw()
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
    // and the new tables work
    let r = store
        .record_document_extraction(draft(&pdf_bytes("a"), TEXT, "1"))
        .unwrap();
    assert!(r.extraction_created);
    // re-opening an already migrated database changes nothing
    let counts = db.counts();
    drop(store);
    let _ = db.open();
    assert_eq!(db.counts(), counts);
}

#[test]
fn the_recording_revision_counter_moves_once_per_new_document_and_never_for_reads_or_erasure() {
    let db = Db::new();
    let store = db.open();
    let rev = |s: &SqliteStore| s.temporal_history_capabilities().unwrap().current_revision;
    let r0 = rev(&store);
    let mut d = draft(&pdf_bytes("a"), TEXT, "1");
    d.retain_bytes = Some(pdf_bytes("a"));
    let r = store.record_document_extraction(d.clone()).unwrap();
    assert_eq!(rev(&store), r0 + 1);
    let a = heading_anchor(&d, r.logical_id);
    store.resolve_document_anchor(&a).unwrap();
    store.list_document_extractions(r.logical_id).unwrap();
    store.erase_document_bytes(&d.source_bytes_hash).unwrap();
    assert_eq!(
        rev(&store),
        r0 + 1,
        "reads and erasure are not recorded writes"
    );
    store.record_document_extraction(d).unwrap(); // re-retain: a write
    assert_eq!(rev(&store), r0 + 2);
}

// ============================================================================ review gates

#[test]
fn the_lifecycle_view_of_a_document_source_works_like_any_other_source() {
    let db = Db::new();
    let store = db.open();
    let r = store
        .record_document_extraction(draft(&pdf_bytes("a"), TEXT, "1"))
        .unwrap();
    let life = store
        .get_source_lifecycle(r.logical_id)
        .expect("a document source must be readable through the lifecycle view")
        .expect("it exists");
    assert_eq!(life.logical.id, r.logical_id);
    assert_eq!(life.logical.kind.as_str(), "document");
    assert_eq!(life.logical.current_location.as_deref(), Some(LOC));
    assert!(life.events.is_empty());
}

#[test]
fn a_new_detector_version_is_a_new_extraction_and_old_anchors_keep_resolving() {
    let db = Db::new();
    let store = db.open();
    let d1 = draft(&pdf_bytes("a"), TEXT, "1");
    let r1 = store.record_document_extraction(d1.clone()).unwrap();
    let mut d2 = d1.clone();
    d2.detector_version = "2".to_string();
    let r2 = store.record_document_extraction(d2.clone()).unwrap();
    assert!(
        r2.extraction_created && !r2.revision_created,
        "same bytes, same text: only the extraction is new"
    );
    assert_ne!(r1.extraction_id, r2.extraction_id);
    assert_eq!(db.count("doc_extractions"), 2);
    // repeating either is idempotent
    let counts = db.counts();
    store.record_document_extraction(d1.clone()).unwrap();
    store.record_document_extraction(d2.clone()).unwrap();
    assert_eq!(db.counts(), counts);
    // each anchor names its detector and resolves to its own extraction
    let a1 = heading_anchor(&d1, r1.logical_id);
    let a2 = heading_anchor(&d2, r2.logical_id);
    assert_ne!(a1.to_string(), a2.to_string());
    assert_eq!(
        store
            .resolve_document_anchor(&a1)
            .unwrap()
            .unwrap()
            .extraction_id,
        r1.extraction_id
    );
    assert_eq!(
        store
            .resolve_document_anchor(&a2)
            .unwrap()
            .unwrap()
            .extraction_id,
        r2.extraction_id
    );
}

#[test]
fn free_text_fields_are_bounded_and_one_document_exists_per_location() {
    let db = Db::new();
    let store = db.open();
    let counts = db.counts();
    for (why, f) in [
        (
            "huge title",
            Box::new(|d: &mut DocumentExtractionDraft| d.title = "t".repeat(5000))
                as Box<dyn Fn(&mut DocumentExtractionDraft)>,
        ),
        ("NUL title", Box::new(|d| d.title = "a\0b".into())),
        (
            "empty media type",
            Box::new(|d| d.media_type = String::new()),
        ),
        (
            "huge media type",
            Box::new(|d| d.media_type = "m".repeat(300)),
        ),
        (
            "format with a space",
            Box::new(|d| d.format = "p d f".into()),
        ),
        (
            "format hint with a colon",
            Box::new(|d| d.format_hint = Some("a:b".into())),
        ),
        (
            "detector with a separator",
            Box::new(|d| d.detector_version = "a@b".into()),
        ),
    ] {
        let mut d = draft(&pdf_bytes("a"), TEXT, "1");
        f(&mut d);
        assert!(
            matches!(
                store.record_document_extraction(d),
                Err(Error::InvalidDocument(_))
            ),
            "{why}"
        );
        assert_eq!(db.counts(), counts, "{why}");
    }
    // the location is unique among documents at the schema level: a UNIQUE, PARTIAL index
    let (unique, partial): (i64, i64) = db
        .raw()
        .query_row(
            "SELECT \"unique\", partial FROM pragma_index_list('logical_sources') WHERE name='uq_logical_documents_location'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((unique, partial), (1, 1));
    let sql: String = db
        .raw()
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='uq_logical_documents_location'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        sql.contains("'document'"),
        "it covers only documents: {sql}"
    );
}

#[test]
fn erasing_bytes_is_by_content_and_never_lowers_what_the_text_citation_says_nor_raises_original_claims(
) {
    let db = Db::new();
    let store = db.open();
    // a text document whose canonical text IS the file: byte_exact, and retained
    let t = "alpha beta gamma\n";
    let mut d = text_draft("/notes/a.txt", t);
    d.retain_bytes = Some(t.as_bytes().to_vec());
    let r = store.record_document_extraction(d.clone()).unwrap();
    // a second document with byte-identical content, at another location, WITHOUT opting in
    let other = text_draft("/notes/copy.txt", t);
    let ro = store.record_document_extraction(other.clone()).unwrap();
    assert_eq!(db.count("doc_blobs"), 1, "one blob per content, shared");
    let a = anchor_for(
        &d,
        r.logical_id,
        &[],
        Some((6, 10)),
        None,
        None,
        AnchorPrecision::ByteExact,
    );
    let ao = anchor_for(
        &other,
        ro.logical_id,
        &[],
        Some((6, 10)),
        None,
        None,
        AnchorPrecision::ByteExact,
    );
    // byte_exact is a representation claim; availability of the original is separate
    let before = store
        .resolve_document_anchor(&a)
        .unwrap()
        .unwrap()
        .resolve(None);
    assert_eq!(before.original, CitationStatus::BytesResolved);
    assert_eq!(before.source_precision, Some(AnchorPrecision::ByteExact));
    // identical bytes are the same retained content for both documents (content addressing)
    let shared = store
        .resolve_document_anchor(&ao)
        .unwrap()
        .unwrap()
        .resolve(None);
    assert_eq!(shared.original, CitationStatus::BytesResolved);
    // erasing by content removes it for both; extractions and anchors survive
    assert!(store.erase_document_bytes(&d.source_bytes_hash).unwrap());
    for (anchor, who) in [(&a, "retaining"), (&ao, "copy")] {
        let after = store
            .resolve_document_anchor(anchor)
            .unwrap()
            .unwrap()
            .resolve(None);
        assert_eq!(
            after.text.as_deref(),
            Some("beta"),
            "{who}: the text citation survives"
        );
        assert_eq!(
            after.anchor.precision,
            AnchorPrecision::ByteExact,
            "{who}: the anchor itself is unchanged"
        );
        assert_eq!(after.original, CitationStatus::SourceUnavailable, "{who}");
        assert_eq!(
            after.source_precision, None,
            "{who}: byte_exact must not imply the original still exists"
        );
    }
    assert_eq!(db.count("doc_extractions"), 2);
    // only an independent live check can restore a claim about the original
    let live = LiveObservation::Hash(d.source_bytes_hash.clone());
    let again = store
        .resolve_document_anchor(&a)
        .unwrap()
        .unwrap()
        .resolve(Some(&live));
    assert_eq!(again.original, CitationStatus::SourcePresentVerified);
    assert_eq!(again.source_precision, Some(AnchorPrecision::ByteExact));
}

#[test]
fn multibyte_text_is_cited_on_character_boundaries_only() {
    let db = Db::new();
    let store = db.open();
    // 1-, 2-, 3- and 4-byte characters
    let text = "aä€😀z";
    assert_eq!(text.len(), 1 + 2 + 3 + 4 + 1);
    let mut d = text_draft("/notes/unicode.txt", text);
    d.nodes[0].text_range = Some((0, text.len() as u64));
    let r = store.record_document_extraction(d.clone()).unwrap();
    let quote = |a: u64, b: u64| {
        let anchor = anchor_for(
            &d,
            r.logical_id,
            &[],
            Some((a, b)),
            None,
            None,
            AnchorPrecision::ByteExact,
        );
        store.resolve_document_anchor(&anchor)
    };
    assert_eq!(quote(0, 1).unwrap().unwrap().text.as_deref(), Some("a"));
    assert_eq!(quote(1, 3).unwrap().unwrap().text.as_deref(), Some("ä"));
    assert_eq!(quote(3, 6).unwrap().unwrap().text.as_deref(), Some("€"));
    assert_eq!(quote(6, 10).unwrap().unwrap().text.as_deref(), Some("😀"));
    assert_eq!(quote(0, 11).unwrap().unwrap().text.as_deref(), Some(text));
    // an empty range is fine on a boundary, and refused inside a character
    assert_eq!(quote(6, 6).unwrap().unwrap().text.as_deref(), Some(""));
    assert!(matches!(quote(5, 5), Err(Error::InvalidAnchor(_))));
    // every offset inside a multibyte character is refused, as start and as end
    for inside in [2u64, 4, 5, 7, 8, 9] {
        assert!(
            matches!(quote(inside, 11), Err(Error::InvalidAnchor(_))),
            "start {inside}"
        );
        assert!(
            matches!(quote(0, inside), Err(Error::InvalidAnchor(_))),
            "end {inside}"
        );
    }
    // a draft whose node range splits the 4-byte character is rejected before any write
    let counts = db.counts();
    let mut bad = text_draft("/notes/unicode2.txt", text);
    bad.nodes[0].text_range = Some((0, 8));
    assert!(matches!(
        store.record_document_extraction(bad),
        Err(Error::InvalidDocument(_))
    ));
    assert_eq!(db.counts(), counts);
}

#[test]
fn extreme_node_paths_are_stored_and_one_level_deeper_is_refused() {
    let db = Db::new();
    let store = db.open();
    let node = |path: Vec<u32>| DocNodeDraft {
        path,
        kind: NodeKind::Section,
        text_range: None,
        page: None,
        bbox: None,
        precision: AnchorPrecision::NodeExact,
    };
    // 64 levels of the largest ordinal: the longest path the schema allows
    let mut nodes = Vec::new();
    for depth in 1..=64usize {
        nodes.push(node(vec![u32::MAX; depth]));
    }
    let mut d = text_draft("/notes/deep.txt", "x");
    d.nodes = nodes;
    let r = store.record_document_extraction(d.clone()).unwrap();
    assert_eq!(r.nodes, 64);
    let deepest = anchor_for(
        &d,
        r.logical_id,
        &vec![u32::MAX; 64],
        None,
        None,
        None,
        AnchorPrecision::NodeExact,
    );
    assert!(store.resolve_document_anchor(&deepest).unwrap().is_some());
    let text: String = deepest.to_string();
    assert_eq!(text.parse::<Anchor>().unwrap(), deepest);
    let mut too_deep = text_draft("/notes/deeper.txt", "x");
    too_deep.nodes = (1..=65usize).map(|n| node(vec![1; n])).collect();
    let before = db.counts();
    assert!(matches!(
        store.record_document_extraction(too_deep),
        Err(Error::InvalidDocument(_))
    ));
    assert_eq!(db.counts(), before);
}

#[test]
fn concurrent_writers_of_the_same_extraction_produce_exactly_one() {
    let db = Db::new();
    let d = draft(&pdf_bytes("race"), TEXT, "1");
    let results: Vec<_> = (0..6)
        .map(|_| {
            let path = db.path.clone();
            let d = d.clone();
            std::thread::spawn(move || {
                let store = SqliteStore::open(&path).unwrap();
                store.record_document_extraction(d)
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect();
    let created = results
        .iter()
        .filter(|r| matches!(r, Ok(r) if r.extraction_created))
        .count();
    let failed: Vec<_> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    assert_eq!(created, 1, "exactly one writer created it: {results:?}");
    assert!(
        failed.is_empty(),
        "the others see it already there instead of failing: {failed:?}"
    );
    assert_eq!(db.count("doc_extractions"), 1);
    assert_eq!(db.count("logical_sources"), 1);
    assert_eq!(db.count("sources"), 1);
    assert_eq!(db.count("doc_nodes"), 3);
}
