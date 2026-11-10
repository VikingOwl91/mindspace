//! M8 S2: Markdown and plain-text DOCUMENT ingestion through the S1 model.
//!
//! The path under test is `mindspace::ingest::document::ingest_document_file`; the legacy path is
//! `mindspace::ingest::ingest_path`. The two are compared where they can meet (the same file).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use mindspace::core::{
    sha256_hex, AnchorPrecision, CitationStatus, DocumentRefusal, Error, LiveObservation,
    SearchQuery, Ts,
};
use mindspace::ingest::document::{
    detect, extract_document_bytes, ingest_document_file, ingest_document_file_as, Dialect,
    DialectCertainty, DocumentIngestOptions, Identity, MAX_STRUCT_DEPTH,
};
use mindspace::storage::Store;
use mindspace::{initialize, SqliteStore};

/// Commit-fault hooks are process-global one-shot switches: tests that arm one hold this exclusively,
/// every other test holds it shared (inside `Env`).
static HOOKS: RwLock<()> = RwLock::new(());

struct Env {
    _t: tempfile::TempDir,
    db: PathBuf,
    dir: PathBuf,
    _shared: Option<std::sync::RwLockReadGuard<'static, ()>>,
}

impl Env {
    fn new() -> Self {
        let guard = HOOKS.read().unwrap_or_else(|e| e.into_inner());
        let mut e = Env::unlocked();
        e._shared = Some(guard);
        e
    }
    fn unlocked() -> Self {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let db = root.join("mind.db");
        let dir = root.join("files");
        std::fs::create_dir_all(&dir).unwrap();
        initialize(&db).unwrap();
        Env {
            _t: t,
            db,
            dir,
            _shared: None,
        }
    }
    fn open(&self) -> SqliteStore {
        SqliteStore::open(&self.db).unwrap()
    }
    fn file(&self, name: &str, content: impl AsRef<[u8]>) -> PathBuf {
        let p = self.dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }
    fn raw(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.db).unwrap()
    }
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
    fn count(&self, t: &str) -> i64 {
        self.counts()[t]
    }
}

fn opts() -> DocumentIngestOptions {
    DocumentIngestOptions::default()
}

fn refusal(
    r: mindspace::Result<mindspace::ingest::document::DocumentIngestReport>,
) -> DocumentRefusal {
    match r {
        Err(Error::DocumentRefused { reason, .. }) => reason,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// (path, kind, quoted text) of every node.
fn dump(
    nodes: &[mindspace::core::DocNodeDraft],
    text: &str,
) -> Vec<(String, &'static str, String)> {
    let mut v: Vec<_> = nodes
        .iter()
        .map(|n| {
            let (a, b) = n.text_range.unwrap();
            (
                n.path
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join("."),
                n.kind.as_str(),
                text[a as usize..b as usize].to_string(),
            )
        })
        .collect();
    v.sort_by_key(|(p, _, _)| {
        nodes.iter().position(|n| {
            n.path
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(".")
                == *p
        })
    });
    v
}

const MD: &str = "Intro paragraph.\n\n# Title\n\nPara one.\n\n- item a\n  - nested\n- item b\n\n```rust\nlet x = 1;\n```\n\n## Sub\n\nPara two \u{fc}\u{1f600}.\n\n# Second\nTail.\n";

// ================================================================================ happy paths

#[test]
fn markdown_becomes_a_sectioned_tree_with_exact_ranges() {
    let env = Env::new();
    let store = env.open();
    let p = env.file("notes.md", MD);
    let r = ingest_document_file(&store, &p, &opts()).unwrap();
    assert_eq!(r.detection.dialect, Dialect::Markdown);
    assert_eq!(r.detection.certainty, DialectCertainty::HintAndContentAgree);
    assert!(r.persisted.document_created && r.persisted.extraction_created);
    let got = dump(&r.nodes, MD);
    let expected: Vec<(&str, &str, &str)> = vec![
        ("1", "paragraph", "Intro paragraph."),
        ("2", "section", "# Title\n\nPara one.\n\n- item a\n  - nested\n- item b\n\n```rust\nlet x = 1;\n```\n\n## Sub\n\nPara two \u{fc}\u{1f600}."),
        ("2.1", "heading", "# Title"),
        ("2.2", "paragraph", "Para one."),
        ("2.3", "list", "- item a\n  - nested\n- item b"),
        ("2.3.1", "list_item", "- item a\n  - nested"),
        ("2.3.1.1", "list", "- nested"),
        ("2.3.1.1.1", "list_item", "- nested"),
        ("2.3.2", "list_item", "- item b"),
        ("2.4", "code_block", "```rust\nlet x = 1;\n```"),
        ("2.5", "section", "## Sub\n\nPara two \u{fc}\u{1f600}."),
        ("2.5.1", "heading", "## Sub"),
        ("2.5.2", "paragraph", "Para two \u{fc}\u{1f600}."),
        ("3", "section", "# Second\nTail."),
        ("3.1", "heading", "# Second"),
        ("3.2", "paragraph", "Tail."),
    ];
    let got_refs: Vec<(&str, &str, &str)> = got
        .iter()
        .map(|(a, b, c)| (a.as_str(), *b, c.as_str()))
        .collect();
    assert_eq!(got_refs, expected);
    // stored: the canonical text is the file, nodes and the title come from the document
    let c = env.raw();
    let (title, content, media): (String, String, String) = c
        .query_row(
            "SELECT title, content, media_type FROM sources WHERE id=?1",
            [r.persisted.source_id.to_string()],
            |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (title.as_str(), content.as_str(), media.as_str()),
        ("Title", MD, "text/markdown")
    );
    assert_eq!(env.count("doc_nodes"), 16);
    let info = &store
        .list_document_extractions(r.persisted.logical_id)
        .unwrap()[0];
    assert_eq!(
        (
            info.extractor_id.as_str(),
            info.format.as_str(),
            info.format_hint.as_deref()
        ),
        ("markdown", "text", Some("md"))
    );
    assert!(info.text_is_source);
}

#[test]
fn plain_text_is_paragraphs_and_nothing_else() {
    let env = Env::new();
    let store = env.open();
    let text = "\n\nFirst line\nsecond line  \r\n\r\n\r\n   indented para   \n\t\nThird";
    let p = env.file("a.txt", text);
    let r = ingest_document_file(&store, &p, &opts()).unwrap();
    assert_eq!(r.detection.dialect, Dialect::Plain);
    let got = dump(&r.nodes, text);
    assert_eq!(
        got.iter()
            .map(|(p, k, t)| (p.as_str(), *k, t.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("1", "paragraph", "First line\nsecond line"),
            ("2", "paragraph", "   indented para"),
            ("3", "paragraph", "Third"),
        ]
    );
    // no headings, sections, pages, tables or geometry were invented
    assert!(r
        .nodes
        .iter()
        .all(|n| n.kind.as_str() == "paragraph" && n.page.is_none() && n.bbox.is_none()));
    assert!(r
        .nodes
        .iter()
        .all(|n| n.precision == AnchorPrecision::ByteExact));
    // the title falls back to the file name
    let title: String = env
        .raw()
        .query_row(
            "SELECT title FROM sources WHERE id=?1",
            [r.persisted.source_id.to_string()],
            |x| x.get(0),
        )
        .unwrap();
    assert_eq!(title, "a");
}

#[test]
fn headings_of_every_form_open_sections_and_skipped_levels_nest_under_the_nearest() {
    let env = Env::new();
    let store = env.open();
    let text = "Setext H1\n=========\n\ntext\n\n### Deep first\n\nx\n\nSetext H2\n---------\n\ny\n\n# Back\n\n####### not a heading\n";
    let p = env.file("h.md", text);
    let r = ingest_document_file(&store, &p, &opts()).unwrap();
    let got = dump(&r.nodes, text);
    let find = |path: &str| {
        got.iter()
            .find(|(p, _, _)| p == path)
            .unwrap_or_else(|| panic!("{path} in {got:?}"))
    };
    assert_eq!(find("1").1, "section");
    assert_eq!(find("1.1").2, "Setext H1\n=========");
    // an H3 after an H1 nests directly under the H1 section
    assert_eq!(find("1.3").1, "section");
    assert_eq!(find("1.3.1").2, "### Deep first");
    // the setext H2 closes the H3 section and nests under the H1
    assert_eq!(find("1.4").1, "section");
    assert_eq!(find("1.4.1").2, "Setext H2\n---------");
    assert_eq!(find("2").1, "section");
    assert_eq!(
        find("2.2").2,
        "####### not a heading",
        "seven hashes is a paragraph"
    );
}

// ================================================================================ unicode

#[test]
fn offsets_are_utf8_byte_offsets_that_slice_to_the_exact_text() {
    let env = Env::new();
    let store = env.open();
    let text = "# \u{65e5}\u{672c}\u{8a9e}\u{306e}\u{898b}\u{51fa}\u{3057} \u{1f600}\n\n\u{4f60}\u{597d}, w\u{f6}rld \u{20ac}5 \u{1f1e9}\u{1f1ea}.\n\n- \u{43f}\u{443}\u{43d}\u{43a}\u{442}\n";
    let p = env.file("u.md", text);
    let r = ingest_document_file(&store, &p, &opts()).unwrap();
    for n in &r.nodes {
        let (a, b) = n.text_range.unwrap();
        assert!(
            text.is_char_boundary(a as usize) && text.is_char_boundary(b as usize),
            "{:?}",
            n.path
        );
    }
    // each node round-trips through an anchor and the store, quoting exactly its text
    for n in &r.nodes {
        let (a, b) = n.text_range.unwrap();
        let anchor = r
            .anchor(&n.path, Some((a, b)), AnchorPrecision::ByteExact)
            .unwrap();
        let again = anchor.to_string().parse().unwrap();
        assert_eq!(anchor, again);
        let cited = store.resolve_document_anchor(&anchor).unwrap().unwrap();
        assert_eq!(cited.text.as_deref(), Some(&text[a as usize..b as usize]));
        // the original is the canonical text, so a verified original supports byte_exact
        let live = LiveObservation::Hash(sha256_hex(text.as_bytes()));
        let res = cited.resolve(Some(&live));
        assert_eq!(res.original, CitationStatus::SourcePresentVerified);
        assert_eq!(res.source_precision, Some(AnchorPrecision::ByteExact));
    }
    let title: String = env
        .raw()
        .query_row(
            "SELECT title FROM sources WHERE id=?1",
            [r.persisted.source_id.to_string()],
            |x| x.get(0),
        )
        .unwrap();
    assert_eq!(
        title,
        "\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{898b}\u{51fa}\u{3057} \u{1f600}"
    );
}

#[test]
fn a_byte_order_mark_is_not_part_of_the_canonical_text_and_the_original_claim_is_downgraded() {
    let env = Env::new();
    let store = env.open();
    let body = "# T\n\nbody \u{e4}\n";
    let mut bytes = vec![0xef, 0xbb, 0xbf];
    bytes.extend_from_slice(body.as_bytes());
    let p = env.file("bom.md", &bytes);
    let r = ingest_document_file(&store, &p, &opts()).unwrap();
    assert!(r.detection.bom_stripped);
    let info = &store
        .list_document_extractions(r.persisted.logical_id)
        .unwrap()[0];
    assert!(!info.text_is_source, "the canonical text is not the file");
    assert_eq!(info.source_size, bytes.len() as u64);
    assert_eq!(r.source_bytes_hash, sha256_hex(&bytes));
    assert_eq!(r.canonical_hash, sha256_hex(body.as_bytes()));
    // offsets are into the canonical text
    let h = &r.nodes[1];
    assert_eq!(h.kind.as_str(), "heading");
    assert_eq!(
        &body[h.text_range.unwrap().0 as usize..h.text_range.unwrap().1 as usize],
        "# T"
    );
    // with the original verified, a byte range of the canonical text is NOT a byte range of the file
    let (a, b) = h.text_range.unwrap();
    let anchor = r
        .anchor(&h.path, Some((a, b)), AnchorPrecision::ByteExact)
        .unwrap();
    let cited = store.resolve_document_anchor(&anchor).unwrap().unwrap();
    let res = cited.resolve(Some(&LiveObservation::Hash(sha256_hex(&bytes))));
    assert_eq!(res.source_precision, Some(AnchorPrecision::NodeExact));
    // the same file without the mark: same canonical text, different bytes = a new extraction, one revision
    let q = env.file("nobom.md", body);
    let r2 = ingest_document_file(&store, &q, &opts()).unwrap();
    assert_ne!(
        r.persisted.logical_id, r2.persisted.logical_id,
        "different locations are different documents"
    );
    assert_eq!(r.canonical_hash, r2.canonical_hash);
    assert_ne!(r.source_bytes_hash, r2.source_bytes_hash);
    // the same location with the mark added later: one document, one revision, two extractions
    std::fs::write(&q, &bytes).unwrap();
    let r3 = ingest_document_file(&store, &q, &opts()).unwrap();
    assert_eq!(r3.persisted.logical_id, r2.persisted.logical_id);
    assert_eq!(
        r3.persisted.source_id, r2.persisted.source_id,
        "the canonical text is unchanged: same revision"
    );
    assert!(!r3.persisted.revision_created && r3.persisted.extraction_created);
    assert_ne!(r3.persisted.extraction_id, r2.persisted.extraction_id);
}

// ============================================================================ refusals

fn refuses_with(name: &str, content: &[u8], reason: DocumentRefusal) {
    let env = Env::new();
    let store = env.open();
    let p = env.file(name, content);
    let before = env.counts();
    assert_eq!(
        refusal(ingest_document_file(&store, &p, &opts())),
        reason,
        "{name}"
    );
    assert_eq!(env.counts(), before, "{name}: a refusal stores nothing");
}

#[test]
fn malformed_unsupported_empty_and_binary_inputs_are_refused_before_any_write() {
    refuses_with("empty.md", b"", DocumentRefusal::Empty);
    refuses_with("blank.txt", b" \n\t\r\n  ", DocumentRefusal::Empty);
    refuses_with("only-bom.md", b"\xef\xbb\xbf", DocumentRefusal::Empty);
    refuses_with("latin1.txt", b"caf\xe9 au lait\n", DocumentRefusal::NotUtf8);
    refuses_with("trunc.md", b"# T\n\n\xe2\x82", DocumentRefusal::NotUtf8);
    refuses_with("overlong.txt", b"a\xc0\xafb", DocumentRefusal::NotUtf8);
    refuses_with("surrogate.txt", b"a\xed\xa0\x80b", DocumentRefusal::NotUtf8);
    refuses_with("nul.txt", b"abc\0def", DocumentRefusal::Binary);
    refuses_with("ctl.txt", b"abc\x07def", DocumentRefusal::ControlCharacters);
    refuses_with(
        "esc.txt",
        b"\x1b[31mred\x1b[0m",
        DocumentRefusal::ControlCharacters,
    );
    refuses_with("del.txt", b"abc\x7fdef", DocumentRefusal::ControlCharacters);
    refuses_with(
        "u16.txt",
        b"\xff\xfeh\0i\0",
        DocumentRefusal::UnsupportedEncoding,
    );
    refuses_with(
        "u16be.txt",
        b"\xfe\xff\0h\0i",
        DocumentRefusal::UnsupportedEncoding,
    );
    refuses_with(
        "u32.txt",
        b"\xff\xfe\0\0h\0\0\0",
        DocumentRefusal::UnsupportedEncoding,
    );
    refuses_with(
        "x.md",
        b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n",
        DocumentRefusal::UnsupportedFormat,
    );
    refuses_with(
        "x.txt",
        b"PK\x03\x04\x14\0\0\0",
        DocumentRefusal::UnsupportedFormat,
    );
    refuses_with(
        "x.md",
        b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR",
        DocumentRefusal::UnsupportedFormat,
    );
    refuses_with(
        "x.md",
        b"\xff\xd8\xff\xe0\0\x10JFIF",
        DocumentRefusal::UnsupportedFormat,
    );
    refuses_with(
        "x.txt",
        b"\x7fELF\x02\x01\x01",
        DocumentRefusal::UnsupportedFormat,
    );
    refuses_with(
        "x.md",
        b"\x1f\x8b\x08\0\0\0\0\0",
        DocumentRefusal::UnsupportedFormat,
    );
    refuses_with(
        "x.md",
        b"{\\rtf1\\ansi hello}",
        DocumentRefusal::UnsupportedFormat,
    );
    refuses_with(
        "x.txt",
        b"<!DOCTYPE html>\n<html><body>hi</body></html>",
        DocumentRefusal::UnsupportedFormat,
    );
    refuses_with(
        "x.md",
        b"  \n<HTML>\n<body>",
        DocumentRefusal::UnsupportedFormat,
    );
}

#[test]
fn the_first_invalid_byte_is_named() {
    let err = detect::detect(b"ok \xff here", None).unwrap_err();
    assert!(err.to_string().contains("offset 3"), "{err}");
    let err = detect::detect(b"\xef\xbb\xbfok \xff", None).unwrap_err();
    assert!(
        err.to_string().contains("offset 6"),
        "offsets count the byte-order mark: {err}"
    );
}

#[test]
fn directories_missing_files_and_oversized_files_are_refused_never_truncated() {
    let env = Env::new();
    let store = env.open();
    let before = env.counts();
    assert_eq!(
        refusal(ingest_document_file(&store, &env.dir, &opts())),
        DocumentRefusal::NotARegularFile
    );
    assert!(matches!(
        ingest_document_file(&store, &env.dir.join("missing.md"), &opts()),
        Err(Error::Io { .. })
    ));
    let big = env.file("big.txt", vec![b'a'; 2048]);
    let small = DocumentIngestOptions {
        max_input_bytes: 2047,
        ..opts()
    };
    assert_eq!(
        refusal(ingest_document_file(&store, &big, &small)),
        DocumentRefusal::TooLarge
    );
    // exactly at the limit is accepted
    let ok = DocumentIngestOptions {
        max_input_bytes: 2048,
        ..opts()
    };
    ingest_document_file(&store, &big, &ok).unwrap();
    // a node explosion is refused as such, not silently cut
    let mut many = String::new();
    for _ in 0..(mindspace::core::MAX_NODES + 1) {
        many.push_str("a\n\n");
    }
    let r = extract_document_bytes(
        "/x/many.txt",
        many.as_bytes(),
        Some("txt"),
        &opts(),
        &Identity::default(),
        Ts::now(),
    );
    assert!(matches!(
        r,
        Err(Error::DocumentRefused {
            reason: DocumentRefusal::TooManyNodes,
            ..
        })
    ));
    assert_eq!(
        env.count("doc_extractions"),
        before["doc_extractions"] + 1,
        "only the accepted file was stored"
    );
    // an unreadable symlink target / device is not a regular file
    #[cfg(unix)]
    assert_eq!(
        refusal(ingest_document_file(
            &store,
            Path::new("/dev/null"),
            &opts()
        )),
        DocumentRefusal::NotARegularFile
    );
}

// ===================================================================== format and dialect

#[test]
fn extensions_hint_but_content_decides() {
    let md = "# Heading\n\ntext\n";
    let plain = "just some prose\nwith two lines\n";
    let weak = "- one\n- two\n";
    let cases: Vec<(&str, &str, Dialect, DialectCertainty)> = vec![
        (
            "a.md",
            md,
            Dialect::Markdown,
            DialectCertainty::HintAndContentAgree,
        ),
        (
            "a.markdown",
            plain,
            Dialect::Markdown,
            DialectCertainty::HintOnly,
        ),
        (
            "a.MD",
            md,
            Dialect::Markdown,
            DialectCertainty::HintAndContentAgree,
        ),
        (
            "a.txt",
            plain,
            Dialect::Plain,
            DialectCertainty::HintAndContentAgree,
        ),
        (
            "a.txt",
            weak,
            Dialect::Plain,
            DialectCertainty::HintAndContentAgree,
        ),
        // a .txt full of Markdown headings: the evidence disagrees; the conservative reading is used
        ("a.txt", md, Dialect::Plain, DialectCertainty::Ambiguous),
        // no usable hint: content only
        (
            "README",
            md,
            Dialect::Markdown,
            DialectCertainty::ContentOnly,
        ),
        (
            "a.rst",
            md,
            Dialect::Markdown,
            DialectCertainty::ContentOnly,
        ),
        (
            "a.pdf",
            plain,
            Dialect::Plain,
            DialectCertainty::ContentOnly,
        ),
        ("data", weak, Dialect::Plain, DialectCertainty::Ambiguous),
        ("data", plain, Dialect::Plain, DialectCertainty::ContentOnly),
        (
            "a.md",
            weak,
            Dialect::Markdown,
            DialectCertainty::HintAndContentAgree,
        ),
        (
            "a.md",
            "```\ncode only\n```\n",
            Dialect::Markdown,
            DialectCertainty::HintAndContentAgree,
        ),
    ];
    for (name, content, dialect, certainty) in cases {
        let env = Env::new();
        let store = env.open();
        let p = env.file(name, content);
        let r = ingest_document_file(&store, &p, &opts()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            (r.detection.dialect, r.detection.certainty),
            (dialect, certainty),
            "{name}: {content:?}"
        );
        assert_eq!(
            r.extractor_id,
            if dialect == Dialect::Markdown {
                "markdown"
            } else {
                "plaintext"
            }
        );
    }
    // a heading inside a fence is code, not Markdown evidence
    let (_, d) = detect::detect(b"```\n# not a heading\n```\n", Some("txt")).unwrap();
    assert_eq!(
        d.certainty,
        DialectCertainty::Ambiguous,
        "the fence itself is the signal"
    );
    let (_, d) = detect::detect(b"    # indented code\n", Some("txt")).unwrap();
    assert_eq!(
        (d.dialect, d.certainty),
        (Dialect::Plain, DialectCertainty::HintAndContentAgree)
    );
    // the hint is recorded as given, normalised; it never overrides binary detection (see refusals)
    let (_, d) = detect::detect(b"x", Some("MD")).unwrap();
    assert_eq!(d.hint.as_deref(), Some("md"));
}

#[test]
fn the_same_bytes_under_another_extension_are_a_distinct_extraction_not_a_silent_reuse() {
    let env = Env::new();
    let store = env.open();
    let md = "# Heading\n\ntext\n";
    let a = env.file("x.md", md);
    let r1 = ingest_document_file(&store, &a, &opts()).unwrap();
    // renaming changes the dialect decision, which changes the extractor's parameters and identity
    let b = env.dir.join("x.txt");
    std::fs::rename(&a, &b).unwrap();
    let r2 = ingest_document_file(&store, &b, &opts()).unwrap();
    assert_ne!(
        r1.persisted.logical_id, r2.persisted.logical_id,
        "another location is another document"
    );
    assert_eq!(r2.detection.dialect, Dialect::Plain);
    assert_eq!(
        (r1.extractor_id.as_str(), r2.extractor_id.as_str()),
        ("markdown", "plaintext")
    );
    assert_ne!(
        r1.nodes.len(),
        r2.nodes.len(),
        "plain text claims no headings"
    );
}

// ================================================================== identity and persistence

#[test]
fn repeat_ingestion_is_idempotent_and_consumes_no_revision() {
    let env = Env::new();
    let store = env.open();
    let p = env.file("n.md", MD);
    let first = ingest_document_file(&store, &p, &opts()).unwrap();
    let counts = env.counts();
    let again = ingest_document_file(&store, &p, &opts()).unwrap();
    assert_eq!(env.counts(), counts, "no row of any table changed");
    assert!(
        !again.persisted.document_created
            && !again.persisted.revision_created
            && !again.persisted.extraction_created
    );
    assert_eq!(first.persisted.extraction_id, again.persisted.extraction_id);
    assert_eq!(first.nodes, again.nodes, "extraction is deterministic");
    // also through a path spelled differently (the location is the canonical path)
    let weird = env.dir.join(".").join("n.md");
    let third = ingest_document_file(&store, &weird, &opts()).unwrap();
    assert_eq!(third.persisted.extraction_id, first.persisted.extraction_id);
    assert_eq!(env.counts(), counts);
}

#[test]
fn changed_content_adds_a_revision_and_an_extraction_and_keeps_the_old_citations() {
    let env = Env::new();
    let store = env.open();
    let p = env.file("n.md", "# One\n\nfirst version\n");
    let r1 = ingest_document_file(&store, &p, &opts()).unwrap();
    let old_anchor = r1
        .anchor(&paragraph_path(&r1), None, AnchorPrecision::NodeExact)
        .unwrap();
    std::fs::write(&p, "# One\n\nsecond version, longer\n").unwrap();
    let r2 = ingest_document_file(&store, &p, &opts()).unwrap();
    assert_eq!(r1.persisted.logical_id, r2.persisted.logical_id);
    assert!(
        r2.persisted.revision_created
            && r2.persisted.extraction_created
            && !r2.persisted.document_created
    );
    assert_ne!(r1.canonical_hash, r2.canonical_hash);
    assert_eq!(
        store
            .list_document_extractions(r1.persisted.logical_id)
            .unwrap()
            .len(),
        2
    );
    // the old citation still quotes the old text, the new one the new
    let new_anchor = r2
        .anchor(&paragraph_path(&r2), None, AnchorPrecision::NodeExact)
        .unwrap();
    let c_old = store.resolve_document_anchor(&old_anchor).unwrap().unwrap();
    let c_new = store.resolve_document_anchor(&new_anchor).unwrap().unwrap();
    assert_eq!(c_old.text.as_deref(), Some("first version"));
    assert_eq!(c_new.text.as_deref(), Some("second version, longer"));
    // going back to the old content reuses the old revision and extraction
    std::fs::write(&p, "# One\n\nfirst version\n").unwrap();
    let r3 = ingest_document_file(&store, &p, &opts()).unwrap();
    assert_eq!(r3.persisted.extraction_id, r1.persisted.extraction_id);
    assert!(!r3.persisted.extraction_created && !r3.persisted.revision_created);
    // the old original no longer matches the live file's... it does again
    let res = c_old.resolve(Some(&LiveObservation::Hash(sha256_hex(
        b"# One\n\nfirst version\n",
    ))));
    assert_eq!(res.original, CitationStatus::SourcePresentVerified);
}

#[test]
fn a_new_detector_or_extractor_version_is_a_new_extraction_of_the_same_revision() {
    let env = Env::new();
    let store = env.open();
    let p = env.file("n.md", MD);
    let base = ingest_document_file(&store, &p, &opts()).unwrap();
    let e2 = Identity {
        extractor_version: "2".into(),
        ..Identity::default()
    };
    let d2 = Identity {
        detector_version: "2".into(),
        ..Identity::default()
    };
    let r_e = ingest_document_file_as(&store, &p, &opts(), &e2).unwrap();
    let r_d = ingest_document_file_as(&store, &p, &opts(), &d2).unwrap();
    for r in [&r_e, &r_d] {
        assert!(r.persisted.extraction_created && !r.persisted.revision_created);
        assert_eq!(r.persisted.source_id, base.persisted.source_id);
        assert_eq!(r.persisted.logical_id, base.persisted.logical_id);
        assert_ne!(r.persisted.extraction_id, base.persisted.extraction_id);
    }
    assert_eq!(
        store
            .list_document_extractions(base.persisted.logical_id)
            .unwrap()
            .len(),
        3
    );
    // repeating any of them is idempotent
    let counts = env.counts();
    ingest_document_file_as(&store, &p, &opts(), &e2).unwrap();
    ingest_document_file_as(&store, &p, &opts(), &d2).unwrap();
    ingest_document_file(&store, &p, &opts()).unwrap();
    assert_eq!(env.counts(), counts);
    // each version's anchors resolve to their own extraction only
    let a_base = base
        .anchor(&base.nodes[0].path, None, AnchorPrecision::NodeExact)
        .unwrap();
    let a_e = r_e
        .anchor(&r_e.nodes[0].path, None, AnchorPrecision::NodeExact)
        .unwrap();
    assert_ne!(a_base.to_string(), a_e.to_string());
    assert_eq!(
        store
            .resolve_document_anchor(&a_base)
            .unwrap()
            .unwrap()
            .extraction_id,
        base.persisted.extraction_id
    );
    assert_eq!(
        store
            .resolve_document_anchor(&a_e)
            .unwrap()
            .unwrap()
            .extraction_id,
        r_e.persisted.extraction_id
    );
}

#[test]
fn the_same_bytes_at_two_locations_are_two_documents() {
    let env = Env::new();
    let store = env.open();
    let a = env.file("a.md", MD);
    let b = env.file("b.md", MD);
    let ra = ingest_document_file(&store, &a, &opts()).unwrap();
    let rb = ingest_document_file(&store, &b, &opts()).unwrap();
    assert_ne!(ra.persisted.logical_id, rb.persisted.logical_id);
    assert_ne!(ra.persisted.source_id, rb.persisted.source_id);
    assert_eq!(ra.source_bytes_hash, rb.source_bytes_hash);
    assert_eq!(ra.nodes, rb.nodes);
    // anchors name their own document
    let x = ra
        .anchor(&ra.nodes[0].path, None, AnchorPrecision::NodeExact)
        .unwrap();
    let y = rb
        .anchor(&rb.nodes[0].path, None, AnchorPrecision::NodeExact)
        .unwrap();
    assert_ne!(x.to_string(), y.to_string());
    assert_eq!(
        store
            .resolve_document_anchor(&x)
            .unwrap()
            .unwrap()
            .extraction_id,
        ra.persisted.extraction_id
    );
    assert_eq!(
        store
            .resolve_document_anchor(&y)
            .unwrap()
            .unwrap()
            .extraction_id,
        rb.persisted.extraction_id
    );
}

#[test]
fn original_bytes_are_kept_only_when_asked() {
    let env = Env::new();
    let store = env.open();
    let p = env.file("n.md", MD);
    let r = ingest_document_file(&store, &p, &opts()).unwrap();
    assert_eq!(
        env.count("doc_blobs"),
        0,
        "default: nothing but the canonical text"
    );
    assert!(!r.persisted.bytes_retained);
    let keep = DocumentIngestOptions {
        retain_original: true,
        ..opts()
    };
    let r2 = ingest_document_file(&store, &p, &keep).unwrap();
    assert!(!r2.persisted.extraction_created && r2.persisted.bytes_retained_now);
    assert_eq!(env.count("doc_blobs"), 1);
    let anchor = r2
        .anchor(&r2.nodes[0].path, None, AnchorPrecision::NodeExact)
        .unwrap();
    let res = store
        .resolve_document_anchor(&anchor)
        .unwrap()
        .unwrap()
        .resolve(None);
    assert_eq!(res.original, CitationStatus::BytesResolved);
    std::fs::remove_file(&p).unwrap();
    let res = store
        .resolve_document_anchor(&anchor)
        .unwrap()
        .unwrap()
        .resolve(Some(&LiveObservation::Missing));
    assert_eq!(
        res.original,
        CitationStatus::BytesResolved,
        "retained bytes outlive the file"
    );
}

// ================================================================== legacy coexistence

fn families(env: &Env) -> Vec<String> {
    env.raw()
        .prepare("SELECT producer_family || ':' || state || ':' || n FROM (SELECT producer_family, state, COUNT(*) AS n FROM memories GROUP BY producer_family, state) ORDER BY 1")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .flatten()
        .collect()
}

fn hits(store: &SqliteStore, q: &str) -> Vec<String> {
    mindspace::retrieval::search(
        store,
        SearchQuery {
            text: q.to_string(),
            space_ids: None,
            limit: 50,
            at: None,
            include_superseded: true,
            include_retired: true,
        },
    )
    .unwrap()
    .into_iter()
    .map(|h| format!("{}|{}|{:?}", h.subject, h.content, h.state))
    .collect()
}

#[test]
fn a_file_ingested_both_ways_never_gets_two_competing_memory_sets() {
    for legacy_first in [true, false] {
        let env = Env::new();
        let store = env.open();
        store.create_space("p", "").unwrap();
        let p = env.file("both.md", "# Zebra\n\nzebra facts live here\n");
        let legacy =
            |store: &SqliteStore| mindspace::ingest::ingest_path(store, &p, &["p"]).unwrap();
        let document = |store: &SqliteStore| ingest_document_file(store, &p, &opts()).unwrap();
        if legacy_first {
            legacy(&store);
        } else {
            document(&store);
        }
        let mem_after_first = (
            env.count("memories"),
            env.count("assertions"),
            env.count("evidence"),
        );
        if legacy_first {
            document(&store);
        } else {
            legacy(&store);
        }
        let fam = families(&env);
        assert_eq!(fam.len(), 1, "one producer family only: {fam:?}");
        assert!(fam[0].starts_with("file:legacy:active:"), "{fam:?}");
        // the document path added no memory, assertion or evidence; the legacy path added only its own
        let now = (
            env.count("memories"),
            env.count("assertions"),
            env.count("evidence"),
        );
        if legacy_first {
            assert_eq!(
                now, mem_after_first,
                "document ingestion changed the legacy memory set"
            );
        } else {
            assert_eq!(
                mem_after_first,
                (0, 0, 0),
                "document ingestion alone creates no memory"
            );
        }
        // two logical sources at one location, of different kinds, never merged
        let kinds: Vec<String> = env
            .raw()
            .prepare("SELECT kind || ':' || state FROM logical_sources ORDER BY kind")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(
            kinds,
            vec!["document:present".to_string(), "file:present".to_string()]
        );
        // an edit + legacy re-ingest supersedes only legacy memories; a document re-ingest adds a revision
        std::fs::write(&p, "# Zebra\n\nzebra facts changed\n").unwrap();
        let before_doc = env.count("doc_extractions");
        legacy(&store);
        document(&store);
        assert_eq!(env.count("doc_extractions"), before_doc + 1);
        let fam = families(&env);
        assert!(fam.iter().all(|f| f.starts_with("file:legacy:")), "{fam:?}");
        let active: i64 = env
            .raw()
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE state='active'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            active, 1,
            "one active memory (the legacy parse has one section), from the legacy path only"
        );
        let superseded: i64 = env
            .raw()
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE state='superseded'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            superseded, 1,
            "the edit superseded the earlier legacy memory; documents touched nothing"
        );
        // search sees legacy memories, never document text
        let h = hits(&store, "zebra");
        assert!(h.iter().any(|x| x.contains("changed")), "{h:?}");
    }
}

#[test]
fn directory_ingest_neither_sees_nor_disturbs_documents_and_documents_never_enter_search() {
    let env = Env::new();
    let store = env.open();
    store.create_space("p", "").unwrap();
    env.file("legacy.md", "# L\n\nlion content\n");
    let doc = env.file("doc.md", "# D\n\nzebra only in the document\n");
    ingest_document_file(&store, &doc, &opts()).unwrap();
    let hits_before = hits(&store, "lion");
    let mem_before = env.count("memories");
    // the directory contains doc.md too, so legacy ingests it as a file: that is the legacy path's own
    // business and gives it its own legacy memories
    mindspace::ingest::ingest_path(&store, &env.dir, &["p"]).unwrap();
    let doc_state: String = env
        .raw()
        .query_row(
            "SELECT state FROM logical_sources WHERE kind='document'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(doc_state, "present");
    let events: i64 = env.raw().query_row("SELECT COUNT(*) FROM source_events WHERE logical_id IN (SELECT id FROM logical_sources WHERE kind='document')", [], |r| r.get(0)).unwrap();
    assert_eq!(events, 0);
    assert!(env.count("memories") > mem_before);
    assert!(hits(&store, "lion").len() >= hits_before.len());
    // remove the file and re-ingest the directory: legacy marks ITS source absent, the document stays
    std::fs::remove_file(&doc).unwrap();
    mindspace::ingest::ingest_path(&store, &env.dir, &["p"]).unwrap();
    let states: Vec<String> = env
        .raw()
        .prepare("SELECT kind || ':' || state FROM logical_sources ORDER BY 1")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .flatten()
        .collect();
    assert!(
        states.contains(&"document:present".to_string()),
        "{states:?}"
    );
    assert!(states.contains(&"file:absent".to_string()), "{states:?}");
}

#[test]
fn default_mcp_tools_and_retrieval_are_unchanged() {
    // ONE environment: the shared hook lock must not be taken twice by one thread (a waiting writer would deadlock it)
    let env = Env::new();
    let names = mcp_tool_names(&env.db);
    assert!(
        names.len() >= 10,
        "the default tool list is served: {names:?}"
    );
    assert!(names.iter().all(|n| !n.contains("document")), "{names:?}");
    for isolated in ["repo_ingest_isolated", "repo_ingest_git_isolated"] {
        assert!(
            !names.iter().any(|n| n == isolated),
            "opt-in tools stay hidden by default"
        );
    }
    // search results are identical before and after documents exist
    let store = env.open();
    store.create_space("p", "").unwrap();
    let p = env.file("a.md", "# Alpha\n\nzebra content here\n");
    mindspace::ingest::ingest_path(&store, &p, &["p"]).unwrap();
    let before = hits(&store, "zebra");
    ingest_document_file(
        &store,
        &env.file("b.md", "# Beta\n\nzebra in a document\n"),
        &opts(),
    )
    .unwrap();
    ingest_document_file(&store, &env.file("c.txt", "zebra again\n"), &opts()).unwrap();
    assert_eq!(hits(&store, "zebra"), before);
    assert_eq!(
        mindspace::retrieval::search(
            &store,
            SearchQuery {
                text: "document".into(),
                space_ids: None,
                limit: 10,
                at: None,
                include_superseded: false,
                include_retired: false
            }
        )
        .unwrap()
        .len(),
        0
    );
}

// ============================================================================== atomicity

#[test]
fn a_failure_mid_write_leaves_nothing_and_a_retry_succeeds() {
    let env = Env::new();
    let store = env.open();
    let p = env.file("n.md", MD);
    let counts = env.counts();
    env.raw()
        .execute_batch("CREATE TRIGGER fault BEFORE INSERT ON doc_nodes WHEN NEW.path = '2.3' BEGIN SELECT RAISE(ABORT, 'injected fault'); END;")
        .unwrap();
    let err = ingest_document_file(&store, &p, &opts()).unwrap_err();
    assert!(err.to_string().contains("injected fault"), "{err}");
    assert_eq!(
        env.counts(),
        counts,
        "no partial document, no recording revision"
    );
    env.raw().execute_batch("DROP TRIGGER fault").unwrap();
    let r = ingest_document_file(&store, &p, &opts()).unwrap();
    assert!(r.persisted.extraction_created);
    assert_eq!(env.count("doc_nodes"), 16);
    assert_eq!(env.count("m6_revisions"), counts["m6_revisions"] + 1);
}

#[test]
fn a_vetoed_commit_rolls_back_and_a_lost_acknowledgement_stays_unknown() {
    let _g = HOOKS.write().unwrap_or_else(|e| e.into_inner());
    let env = Env::unlocked();
    let store = env.open();
    let p = env.file("n.md", MD);
    let counts = env.counts();
    mindspace::storage::sqlite::gate::FAIL_COMMIT.store(true, std::sync::atomic::Ordering::SeqCst);
    let e = ingest_document_file(&store, &p, &opts()).unwrap_err();
    assert!(!matches!(e, Error::CommitOutcomeUnknown { .. }), "{e}");
    assert_eq!(env.counts(), counts);
    // lost acknowledgement: committed, but the caller is told it cannot know
    mindspace::storage::sqlite::gate::LOSE_ACK.store(true, std::sync::atomic::Ordering::SeqCst);
    let e = ingest_document_file(&store, &p, &opts()).unwrap_err();
    assert!(
        matches!(e, Error::CommitOutcomeUnknown { .. }),
        "an unknown commit stays unknown: {e}"
    );
    assert_eq!(
        env.count("doc_extractions"),
        1,
        "the data is committed although unacknowledged"
    );
    drop(store);
    let again = ingest_document_file(&env.open(), &p, &opts()).unwrap();
    assert!(
        !again.persisted.extraction_created,
        "the retry finds it and adds nothing"
    );
    assert_eq!(env.count("doc_extractions"), 1);
}

// =========================================================================== structure laws

/// A tiny deterministic generator (no dependency).
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

#[test]
fn generated_documents_always_yield_a_valid_ordered_tree() {
    generated_documents(42, 400);
}

/// Heavier run with other seeds: `cargo test --test document_ingest -- --ignored`.
#[test]
#[ignore]
fn generated_documents_heavy() {
    for seed in 1..=8u64 {
        generated_documents(seed * 7919, 1500);
    }
}

fn generated_documents(seed: u64, cases: usize) {
    let tokens: &[&str] = &[
        "# ",
        "## ",
        "### ",
        "#",
        "- ",
        "* ",
        "1. ",
        "> ",
        "```",
        "~~~",
        "    ",
        "\t",
        "\n",
        "\n\n",
        "\r\n",
        "a",
        "bc",
        " ",
        "\u{fc}",
        "\u{20ac}",
        "\u{1f600}",
        "\u{65e5}",
        "=====",
        "-----",
        "[x](y)",
        "*e*",
        "`c`",
        "<b>",
        "| a | b |",
        "|---|---|",
        "[^1]",
        "***",
        "  \n",
    ];
    let mut rng = Lcg(seed);
    let mut accepted = 0;
    for case in 0..cases {
        let n = (rng.next() % 120) as usize + 1;
        let mut s = String::new();
        for _ in 0..n {
            s.push_str(tokens[(rng.next() as usize) % tokens.len()]);
        }
        for hint in [Some("md"), Some("txt"), None] {
            let r = extract_document_bytes(
                "/g/x",
                s.as_bytes(),
                hint,
                &opts(),
                &Identity::default(),
                Ts::now(),
            );
            let ex = match r {
                Ok(e) => e,
                Err(Error::DocumentRefused { reason, .. }) => {
                    assert!(
                        matches!(
                            reason,
                            DocumentRefusal::Empty | DocumentRefusal::UnsupportedFormat
                        ),
                        "case {case}: {reason:?} for {s:?}"
                    );
                    continue;
                }
                Err(e) => panic!("case {case} {s:?}: {e}"),
            };
            accepted += 1;
            ex.draft
                .validate()
                .unwrap_or_else(|e| panic!("case {case} {s:?}: {e}"));
            let by_path: BTreeMap<Vec<u32>, (usize, usize, &str)> = ex
                .draft
                .nodes
                .iter()
                .map(|n| {
                    let (a, b) = n.text_range.unwrap();
                    (n.path.clone(), (a as usize, b as usize, n.kind.as_str()))
                })
                .collect();
            let mut last_end_of_parent: BTreeMap<Vec<u32>, usize> = BTreeMap::new();
            for n in &ex.draft.nodes {
                let (a, b, kind) = by_path[&n.path];
                assert!(a <= b && b <= s.len(), "case {case}");
                // inside the parent, after the previous sibling
                let parent: Vec<u32> = n.path[..n.path.len() - 1].to_vec();
                if let Some((pa, pb, _)) = by_path.get(&parent) {
                    assert!(
                        *pa <= a && b <= *pb,
                        "case {case}: {:?} {a}..{b} outside parent {pa}..{pb} for {s:?}",
                        n.path
                    );
                }
                let prev = last_end_of_parent.entry(parent).or_insert(0);
                assert!(
                    a >= *prev || *prev == 0,
                    "case {case}: siblings overlap at {:?} for {s:?}",
                    n.path
                );
                *prev = b;
                // sections nest at most once per heading level, far below the depth cap: the branch of the
                // builder for a depth-suppressed section can never be reached
                if kind == "section" {
                    assert!(n.path.len() <= 6, "case {case}: section path {:?}", n.path);
                }
                // a section starts with its heading
                if kind == "section" {
                    let mut hp = n.path.clone();
                    hp.push(1);
                    let (ha, _, hk) = by_path[&hp];
                    assert_eq!(hk, "heading", "case {case}");
                    assert_eq!(ha, a, "case {case}: section and heading start together");
                }
                assert!(n.path.len() <= MAX_STRUCT_DEPTH);
            }
            // deterministic
            let again = extract_document_bytes(
                "/g/x",
                s.as_bytes(),
                hint,
                &opts(),
                &Identity::default(),
                Ts::now(),
            )
            .unwrap();
            assert_eq!(ex.draft.structure_hash(), again.draft.structure_hash());
        }
    }
    assert!(
        accepted > 600,
        "the generator must exercise the parser: {accepted}"
    );
}

#[test]
fn pathologically_nested_markdown_is_bounded_not_a_crash() {
    // 500 nested list levels, 500 nested quotes, a heading inside a quote, an unclosed fence
    let mut s = String::new();
    for i in 0..500 {
        s.push_str(&"  ".repeat(i.min(60)));
        s.push_str("- item\n");
    }
    s.push_str(&"> ".repeat(500));
    s.push_str("quoted\n\n> # quoted heading\n\n```\nnever closed\n");
    let ex = extract_document_bytes(
        "/g/deep.md",
        s.as_bytes(),
        Some("md"),
        &opts(),
        &Identity::default(),
        Ts::now(),
    )
    .unwrap();
    ex.draft.validate().unwrap();
    assert!(ex
        .draft
        .nodes
        .iter()
        .all(|n| n.path.len() <= MAX_STRUCT_DEPTH));
    assert!(
        !ex.draft.nodes.iter().any(|n| n.kind.as_str() == "heading"),
        "a heading inside a quote is not a section break"
    );
    // an unclosed fence is a code block to the end of the text
    let last = ex.draft.nodes.last().unwrap();
    assert_eq!(last.kind.as_str(), "code_block");
    // nothing under a suppressed (too deep) ancestor leaks up to the root: the document root holds
    // exactly the one top-level list and the final code block
    let roots: Vec<&str> = ex
        .draft
        .nodes
        .iter()
        .filter(|n| n.path.len() == 1)
        .map(|n| n.kind.as_str())
        .collect();
    assert_eq!(roots, vec!["list", "code_block"], "{roots:?}");
    // and every node lies inside its parent
    let by_path: BTreeMap<Vec<u32>, (u64, u64)> = ex
        .draft
        .nodes
        .iter()
        .map(|n| (n.path.clone(), n.text_range.unwrap()))
        .collect();
    for n in &ex.draft.nodes {
        if n.path.len() > 1 {
            let (pa, pb) = by_path[&n.path[..n.path.len() - 1]];
            let (a, b) = n.text_range.unwrap();
            assert!(pa <= a && b <= pb, "{:?}", n.path);
        }
    }
}

#[test]
fn paragraphs_of_a_loose_list_stay_inside_their_items() {
    let env = Env::new();
    let store = env.open();
    let text = "Intro\n\n1. first\n\n   second paragraph of the first item\n\n2. other\n\n> quoted paragraph\n>\n> - quoted list\n";
    let p = env.file("loose.md", text);
    let r = ingest_document_file(&store, &p, &opts()).unwrap();
    let got = dump(&r.nodes, text);
    assert_eq!(
        got.iter()
            .map(|(p, k, t)| (p.as_str(), *k, t.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("1", "paragraph", "Intro"),
            (
                "2",
                "list",
                "1. first\n\n   second paragraph of the first item\n\n2. other"
            ),
            (
                "2.1",
                "list_item",
                "1. first\n\n   second paragraph of the first item"
            ),
            ("2.2", "list_item", "2. other"),
        ],
        "no paragraph nodes inside items, nothing from the block quote"
    );
}

#[test]
fn nothing_under_a_depth_suppressed_item_escapes_to_the_root() {
    // five enclosing sections shift the parity so that a list ITEM (not only a list) reaches the cap
    let mut s = String::from("# a\n\n## b\n\n### c\n\n#### d\n\n##### e\n\n");
    for i in 0..60 {
        s.push_str(&"  ".repeat(i));
        s.push_str("- item\n");
    }
    s.push_str("\n```\nafter the list\n```\n");
    let ex = extract_document_bytes(
        "/g/deep2.md",
        s.as_bytes(),
        Some("md"),
        &opts(),
        &Identity::default(),
        Ts::now(),
    )
    .unwrap();
    ex.draft.validate().unwrap();
    assert!(ex
        .draft
        .nodes
        .iter()
        .all(|n| n.path.len() <= MAX_STRUCT_DEPTH));
    assert!(
        ex.draft
            .nodes
            .iter()
            .any(|n| n.path.len() == MAX_STRUCT_DEPTH),
        "the cap was actually reached"
    );
    let roots: Vec<&str> = ex
        .draft
        .nodes
        .iter()
        .filter(|n| n.path.len() == 1)
        .map(|n| n.kind.as_str())
        .collect();
    assert_eq!(
        roots,
        vec!["section"],
        "only the first-level section is at the root: {roots:?}"
    );
    let by_path: BTreeMap<Vec<u32>, (u64, u64)> = ex
        .draft
        .nodes
        .iter()
        .map(|n| (n.path.clone(), n.text_range.unwrap()))
        .collect();
    for n in ex.draft.nodes.iter().filter(|n| n.path.len() > 1) {
        let (pa, pb) = by_path[&n.path[..n.path.len() - 1]];
        let (a, b) = n.text_range.unwrap();
        assert!(pa <= a && b <= pb, "{:?} outside its parent", n.path);
    }
}

// ================================================================= review: counterexamples

#[cfg(unix)]
#[test]
fn a_named_pipe_is_refused_without_blocking() {
    let env = Env::new();
    let store = env.open();
    let fifo = env.dir.join("pipe.md");
    let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: plain mkfifo(2) on a path inside the test's temporary directory.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let (tx, rx) = std::sync::mpsc::channel();
    let path = fifo.clone();
    let db = env.db.clone();
    std::thread::spawn(move || {
        let store = SqliteStore::open(&db).unwrap();
        let _ = tx.send(
            ingest_document_file(&store, &path, &DocumentIngestOptions::default()).map(|_| ()),
        );
    });
    let r = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("opening a named pipe must not block the ingest forever");
    assert!(
        matches!(
            r,
            Err(Error::DocumentRefused {
                reason: DocumentRefusal::NotARegularFile,
                ..
            })
        ),
        "{r:?}"
    );
    drop(store);
}

#[test]
fn ordinary_text_that_merely_mentions_a_binary_signature_is_text() {
    let env = Env::new();
    let store = env.open();
    for (name, text) in [
        (
            "pdf.md",
            "# Notes\n\nEvery PDF file starts with %PDF- followed by a version.\n",
        ),
        ("gif.txt", "GIF89a and GIF87a are the two GIF versions.\n"),
        ("gif8.txt", "GIF8 is a prefix of both of them.\n"),
        (
            "junk.txt",
            "preamble\n%PDF-1.4 is only quoted here, the rest is prose\n",
        ),
    ] {
        let p = env.file(name, text);
        let r = ingest_document_file(&store, &p, &opts()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(r.detection.physical, "text", "{name}");
    }
    // real headers are still refused by name: at the very start, also after a byte-order mark or blanks
    for (name, bytes) in [
        ("a.txt", b"%PDF-1.4\n1 0 obj\n".to_vec()),
        ("b.txt", b"\xef\xbb\xbf%PDF-1.7\n".to_vec()),
        ("c.txt", b"\n\n  %PDF-1.7\n".to_vec()),
        ("d.txt", b"GIF89a\x01\x00\x01\x00".to_vec()),
        ("e.txt", b"GIF87a\x01\x00".to_vec()),
    ] {
        let p = env.file(name, &bytes);
        assert_eq!(
            refusal(ingest_document_file(&store, &p, &opts())),
            DocumentRefusal::UnsupportedFormat,
            "{name}"
        );
    }
}

/// The doc_nodes table, its trigger and nothing else, exactly as migration 0010 defines them.
fn schema_ten_doc_nodes() -> Vec<String> {
    let sql = include_str!("../migrations/0010_documents.sql");
    let between = |from: &str, to: &str| -> String {
        let a = sql.find(from).unwrap_or_else(|| panic!("{from}"));
        let b = a + sql[a..].find(to).unwrap_or_else(|| panic!("{to}")) + to.len();
        sql[a..b].to_string()
    };
    vec![
        between("CREATE TABLE doc_nodes (", ") WITHOUT ROWID;"),
        between("CREATE TRIGGER doc_nodes_range_inside_text", "END;"),
        between("CREATE TRIGGER doc_nodes_append_only_u", "END;"),
        between("CREATE TRIGGER doc_nodes_append_only_d", "END;"),
    ]
}

type Row = Vec<rusqlite::types::Value>;

fn all_rows(c: &rusqlite::Connection, table: &str) -> Vec<Row> {
    let mut stmt = c
        .prepare(&format!("SELECT * FROM {table} ORDER BY 1, 2"))
        .unwrap();
    let n = stmt.column_count();
    stmt.query_map([], |r| {
        (0..n)
            .map(|i| r.get::<_, rusqlite::types::Value>(i))
            .collect::<std::result::Result<Row, _>>()
    })
    .unwrap()
    .flatten()
    .collect()
}

/// A database exactly at schema 10 holding documents with every kind of node migration 10 allows
/// (pages, boxes, byte ranges, node-only entries), built with the real 0010 definition of `doc_nodes`.
fn populated_schema_ten(env: &Env) -> (Vec<Row>, Vec<Row>, Vec<mindspace::core::Anchor>) {
    use mindspace::core::{BBox, DocNodeDraft, DocumentExtractionDraft, NodeKind};
    let store = env.open();
    let text = "Title\nFirst paragraph \u{fc}.\nSecond.\n";
    let bytes = b"%PDF synthetic bytes".to_vec();
    let node = |path: Vec<u32>, kind, range, page, bbox, precision| DocNodeDraft {
        path,
        kind,
        text_range: range,
        page,
        bbox,
        precision,
    };
    let draft = DocumentExtractionDraft {
        location: "/archive/report.pdf".into(),
        title: "Report".into(),
        media_type: "application/pdf".into(),
        format: "pdf".into(),
        format_hint: Some("pdf".into()),
        detector_version: "1".into(),
        extractor_id: "synthetic-pdf".into(),
        extractor_version: "1".into(),
        params_hash: mindspace::core::params_hash(&serde_json::json!({"k": 1})),
        source_bytes_hash: sha256_hex(&bytes),
        source_size: bytes.len() as u64,
        text_is_source: false,
        canonical_text: text.to_string(),
        nodes: vec![
            node(
                vec![1],
                NodeKind::Page,
                None,
                Some(1),
                None,
                AnchorPrecision::PageOnly,
            ),
            node(
                vec![1, 1],
                NodeKind::Heading,
                Some((0, 5)),
                Some(1),
                Some(BBox {
                    x0: 72.0,
                    y0: 700.5,
                    x1: 300.25,
                    y1: 716.0,
                }),
                AnchorPrecision::NodeExact,
            ),
            node(
                vec![1, 2],
                NodeKind::Paragraph,
                Some((6, text.len() as u64 - 1)),
                Some(1),
                None,
                AnchorPrecision::ByteExact,
            ),
            node(
                vec![2],
                NodeKind::Section,
                None,
                None,
                None,
                AnchorPrecision::NodeExact,
            ),
            node(
                vec![2, 1],
                NodeKind::Table,
                None,
                Some(2),
                None,
                AnchorPrecision::PageOnly,
            ),
            node(
                vec![2, 1, 1],
                NodeKind::Cell,
                Some((0, 5)),
                None,
                None,
                AnchorPrecision::ByteExact,
            ),
        ],
        retain_bytes: None,
        observed_at: Ts::now(),
    };
    let report = store.record_document_extraction(draft.clone()).unwrap();
    // a second document in the same database
    let mut other = draft.clone();
    other.location = "/archive/other.pdf".into();
    store.record_document_extraction(other).unwrap();
    let anchors: Vec<_> = draft
        .nodes
        .iter()
        .map(|n| {
            // a page-only anchor names a page and no node; every other precision names the node
            let (path, page, precision) = match n.precision {
                AnchorPrecision::PageOnly => (vec![], n.page, AnchorPrecision::PageOnly),
                AnchorPrecision::ByteExact => (n.path.clone(), None, AnchorPrecision::NodeExact),
                p => (n.path.clone(), None, p),
            };
            mindspace::core::Anchor::new(
                report.logical_id,
                &draft.source_bytes_hash,
                "1",
                "synthetic-pdf",
                "1",
                &draft.params_hash,
                path,
                None,
                page,
                None,
                precision,
            )
            .unwrap()
        })
        .collect();
    drop(store);
    // revert to schema 10: the real 0010 doc_nodes, same rows, migration 11 forgotten
    let c = env.raw();
    let rows = all_rows(&c, "doc_nodes");
    let stmts = schema_ten_doc_nodes();
    c.execute_batch(&format!(
        "CREATE TABLE doc_nodes_saved AS SELECT * FROM doc_nodes;
         DROP TABLE doc_nodes;
         {}
         INSERT INTO doc_nodes SELECT * FROM doc_nodes_saved;
         DROP TABLE doc_nodes_saved;
         {} {} {}
         DROP TABLE doc_chunks; DROP TABLE doc_chunk_sets; DELETE FROM schema_migrations WHERE version IN (11, 12);",
        stmts[0], stmts[1], stmts[2], stmts[3]
    ))
    .unwrap();
    let extractions = all_rows(&c, "doc_extractions");
    assert_eq!(
        all_rows(&c, "doc_nodes"),
        rows,
        "the schema-10 copy holds the same rows"
    );
    (rows, extractions, anchors)
}

#[test]
fn migration_eleven_upgrades_a_populated_schema_ten_database_without_losing_anything() {
    let env = Env::new();
    let (nodes_before, extractions_before, anchors) = populated_schema_ten(&env);
    assert_eq!(nodes_before.len(), 12);
    let max: i64 = env
        .raw()
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(max, 10);
    let triggers_before: Vec<String> = env.raw().prepare("SELECT name FROM sqlite_master WHERE type='trigger' AND tbl_name='doc_nodes' ORDER BY 1").unwrap().query_map([], |r| r.get(0)).unwrap().flatten().collect();
    // opening applies migration 11
    let store = env.open();
    let c = env.raw();
    assert_eq!(
        all_rows(&c, "doc_nodes"),
        nodes_before,
        "every node survives unchanged, column for column"
    );
    assert_eq!(
        all_rows(&c, "doc_extractions"),
        extractions_before,
        "extractions are untouched"
    );
    let max: i64 = c
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(max, 12);
    // foreign keys: the migration ran with them off, so nothing checked them. Check now.
    let violations: i64 = c
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
    let fk: i64 = c.query_row("SELECT COUNT(*) FROM pragma_foreign_key_list('doc_nodes') WHERE \"table\"='doc_extractions'", [], |r| r.get(0)).unwrap();
    assert_eq!(fk, 1, "doc_nodes still references doc_extractions");
    let ok: String = c
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
    // triggers and the primary key index are the same set; no helper table is left behind
    let triggers_after: Vec<String> = c.prepare("SELECT name FROM sqlite_master WHERE type='trigger' AND tbl_name='doc_nodes' ORDER BY 1").unwrap().query_map([], |r| r.get(0)).unwrap().flatten().collect();
    assert_eq!(triggers_after, triggers_before);
    let tables: Vec<String> = c
        .prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'doc_%' ORDER BY 1",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(
        tables,
        vec![
            "doc_blobs",
            "doc_chunk_sets",
            "doc_chunks",
            "doc_extractions",
            "doc_nodes"
        ]
    );
    let pk: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('doc_nodes') WHERE pk > 0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pk, 2);
    // append-only and the range trigger are still enforced
    for sql in ["UPDATE doc_nodes SET kind='page'", "DELETE FROM doc_nodes"] {
        assert!(
            c.execute(sql, [])
                .unwrap_err()
                .to_string()
                .contains("append-only"),
            "{sql}"
        );
    }
    let ext: String = c
        .query_row(
            "SELECT id FROM doc_extractions ORDER BY created_at LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let too_far = c.execute(&format!("INSERT INTO doc_nodes (extraction_id, path, kind, text_start, text_end, precision) VALUES ('{ext}', '9', 'paragraph', 0, 99999, 'byte_exact')"), []).unwrap_err().to_string();
    assert!(too_far.contains("outside the canonical text"), "{too_far}");
    // the new vocabulary is accepted, the old rules still hold
    c.execute(&format!("INSERT INTO doc_nodes (extraction_id, path, kind, text_start, text_end, precision) VALUES ('{ext}', '9', 'code_block', 0, 3, 'byte_exact')"), []).unwrap();
    assert!(c.execute(&format!("INSERT INTO doc_nodes (extraction_id, path, kind, precision) VALUES ('{ext}', '10', 'code_block', 'byte_exact')"), []).is_err(), "byte_exact still needs a range");
    assert!(c.execute(&format!("INSERT INTO doc_nodes (extraction_id, path, kind, precision) VALUES ('{ext}', '11', 'diagram', 'none')"), []).is_err(), "unknown kinds are still refused");
    // existing citations resolve after the upgrade
    for a in &anchors {
        let cited = store
            .resolve_document_anchor(a)
            .unwrap()
            .unwrap_or_else(|| panic!("{a}"));
        assert_eq!(cited.anchor, *a);
    }
    // reopening neither repeats nor changes the migration
    let snapshot = (
        all_rows(&c, "doc_nodes"),
        all_rows(&c, "schema_migrations").len(),
    );
    drop(store);
    drop(c);
    let again = env.open();
    let c = env.raw();
    assert_eq!(
        (
            all_rows(&c, "doc_nodes"),
            all_rows(&c, "schema_migrations").len()
        ),
        snapshot
    );
    drop(again);
}

#[test]
fn a_migration_that_fails_late_rolls_back_to_the_exact_schema_ten_state() {
    let env = Env::new();
    let (nodes_before, extractions_before, _) = populated_schema_ten(&env);
    let c = env.raw();
    // sabotage a LATE statement of migration 11: after the new table was created, filled and the old
    // one dropped, RENAME re-parses the schema and fails on a view that mentions the vanished table
    c.execute_batch("CREATE VIEW sabotage AS SELECT * FROM doc_nodes;")
        .unwrap();
    let schema_sabotaged: Vec<(String, String)> = c
        .prepare("SELECT type || ':' || name, COALESCE(sql, '') FROM sqlite_master ORDER BY 1")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .flatten()
        .collect();
    drop(c);
    let failed = SqliteStore::open(&env.db);
    let msg = match failed {
        Err(e) => e.to_string(),
        Ok(_) => panic!("the sabotaged migration must fail"),
    };
    assert!(
        msg.contains("sabotage") || msg.contains("doc_nodes"),
        "{msg}"
    );
    let c = env.raw();
    // atomic: the database is byte-for-byte the state before the attempt
    let schema_after: Vec<(String, String)> = c
        .prepare("SELECT type || ':' || name, COALESCE(sql, '') FROM sqlite_master ORDER BY 1")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(
        schema_after, schema_sabotaged,
        "no half-migrated schema, no leftover helper table"
    );
    assert_eq!(all_rows(&c, "doc_nodes"), nodes_before);
    assert_eq!(all_rows(&c, "doc_extractions"), extractions_before);
    let max: i64 = c
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(max, 10, "migration 11 is not recorded");
    // the old definition is still in force
    assert!(c.execute("INSERT INTO doc_nodes (extraction_id, path, kind, text_start, text_end, precision) SELECT id, '9', 'code_block', 0, 1, 'byte_exact' FROM doc_extractions LIMIT 1", []).is_err());
    // remove the sabotage: the retry migrates cleanly
    c.execute_batch("DROP VIEW sabotage").unwrap();
    drop(c);
    let store = env.open();
    let c = env.raw();
    assert_eq!(all_rows(&c, "doc_nodes"), nodes_before);
    let max: i64 = c
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(max, 12);
    let violations: i64 = c
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
    drop(store);
}

/// The tool names a default `mind mcp` server lists (a real process, JSON-RPC over stdio).
fn mcp_tool_names(db: &Path) -> Vec<String> {
    use std::io::{BufRead, Write};
    use std::process::{Command, Stdio};
    let mut child = Command::new(env!("CARGO_BIN_EXE_mind"))
        .arg("--db")
        .arg(db)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-06-18","capabilities":{{}},"clientInfo":{{"name":"t","version":"0"}}}}}}"#).unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list"}}"#).unwrap();
    drop(stdin);
    let out = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut names = Vec::new();
    for line in out.lines().map_while(|l| l.ok()) {
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        if v["id"] == 2 {
            names = v["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string())
                .collect();
        }
    }
    let _ = child.wait();
    names
}

fn paragraph_path(r: &mindspace::ingest::document::DocumentIngestReport) -> Vec<u32> {
    r.nodes
        .iter()
        .find(|n| n.kind.as_str() == "paragraph")
        .expect("a paragraph")
        .path
        .clone()
}

#[test]
fn crlf_and_lf_yield_the_same_tree_with_each_files_own_offsets() {
    let env = Env::new();
    let store = env.open();
    let lf = "# Title\n\nPara one.\nstill one.\n\n- a\n- b\n\n```\ncode\n```\n\n## Sub\n\nTail\n";
    let crlf = lf.replace('\n', "\r\n");
    let a = ingest_document_file(&store, &env.file("lf.md", lf), &opts()).unwrap();
    let b = ingest_document_file(&store, &env.file("crlf.md", &crlf), &opts()).unwrap();
    let shape =
        |r: &mindspace::ingest::document::DocumentIngestReport| -> Vec<(Vec<u32>, &'static str)> {
            r.nodes
                .iter()
                .map(|n| (n.path.clone(), n.kind.as_str()))
                .collect()
        };
    assert_eq!(shape(&a), shape(&b));
    // each file's offsets slice its OWN canonical text; no carriage return leaks into a node's end
    for (r, text) in [(&a, lf.to_string()), (&b, crlf.clone())] {
        for n in &r.nodes {
            let (x, y) = n.text_range.unwrap();
            let q = &text[x as usize..y as usize];
            assert!(
                !q.ends_with('\r') && !q.ends_with('\n') && !q.ends_with(' '),
                "{:?} {q:?}",
                n.path
            );
        }
    }
    assert_ne!(
        a.canonical_hash, b.canonical_hash,
        "line endings are content: nothing is normalised"
    );
    // plain text too
    let t = ingest_document_file(
        &store,
        &env.file("p.txt", "one\r\ntwo\r\n\r\nthree\r\n"),
        &opts(),
    )
    .unwrap();
    let ranges: Vec<(u64, u64)> = t.nodes.iter().map(|n| n.text_range.unwrap()).collect();
    assert_eq!(ranges, vec![(0, 8), (12, 17)]);
}

#[test]
fn heading_like_lines_inside_fences_and_indented_code_are_code() {
    let text = "Intro\n\n```\n# not a heading\n## nor this\n```\n\n~~~sh\n# comment\n~~~\n\n    # indented code\n\n# Real\n";
    let ex = extract_document_bytes(
        "/g/f.md",
        text.as_bytes(),
        Some("md"),
        &opts(),
        &Identity::default(),
        Ts::now(),
    )
    .unwrap();
    let got = dump(&ex.draft.nodes, text);
    let kinds: Vec<(&str, &str)> = got.iter().map(|(p, k, _)| (p.as_str(), *k)).collect();
    assert_eq!(
        kinds,
        vec![
            ("1", "paragraph"),
            ("2", "code_block"),
            ("3", "code_block"),
            ("4", "code_block"),
            ("5", "section"),
            ("5.1", "heading"),
        ],
        "{got:?}"
    );
    assert_eq!(got[1].2, "```\n# not a heading\n## nor this\n```");
    assert_eq!(
        got[3].2, "# indented code",
        "an indented code block is quoted without its indentation"
    );
}
