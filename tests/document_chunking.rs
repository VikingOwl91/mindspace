#![cfg(feature = "embeddings")]
use mindspace::core::{DocumentExtractionDraft, Ts};
use mindspace::embeddings::{budget::TokenBudget, manifest::NOMIC_V1_5};
use mindspace::ingest::document::{
    chunk::chunk_document, extract_document_bytes, DocumentIngestOptions, Identity,
};

fn budget() -> TokenBudget {
    TokenBudget::from_file(
        std::path::Path::new("tests/fixtures/nomic-tokenizer.json"),
        &NOMIC_V1_5,
    )
    .unwrap()
}
fn draft(text: &str, hint: &str) -> DocumentExtractionDraft {
    extract_document_bytes(
        "/chunks/test.md",
        text.as_bytes(),
        Some(hint),
        &DocumentIngestOptions::default(),
        &Identity::default(),
        Ts::now(),
    )
    .unwrap()
    .draft
}
fn check(d: &DocumentExtractionDraft, b: &TokenBudget) -> usize {
    let p = chunk_document(d, b).unwrap();
    assert_eq!(p, chunk_document(d, b).unwrap());
    let mut at = 0;
    let mut reconstructed = String::new();
    for c in &p.chunks {
        assert_eq!(c.start as usize, at);
        assert!(c.end > c.start);
        let s = &d.canonical_text[c.start as usize..c.end as usize];
        assert_eq!(c.tokens, b.validate_document(s).unwrap());
        reconstructed.push_str(s);
        for path in &c.context {
            let n = d.nodes.iter().find(|n| &n.path == path).unwrap();
            let (a, z) = n.text_range.unwrap();
            assert!(a <= c.start && z >= c.end);
        }
        at = c.end as usize;
    }
    assert_eq!(at, d.canonical_text.len());
    assert_eq!(reconstructed, d.canonical_text);
    p.chunks.len()
}
#[test]
fn edge_cases_preserve_every_byte() {
    let b = budget();
    let cases = [
        "a".into(),
        "word ".repeat(10_000),
        "漢字中文日本語 ".repeat(3000),
        "👩🏽‍💻 e\u{301} עברית العربية क क्ष \u{200b}\u{202e}hidden\u{202c}\u{2066}safe\u{2069}\r\n"
            .repeat(1500),
        "x".repeat(80_000),
        "а".repeat(50_000),
        format!(
            "# Outer\nintro\n\n## Inner\n{}\n### Deep\nend\n",
            "paragraph\n\n".repeat(1500)
        ),
        format!(
            "# Code\r\n```rust\r\n{}\r\n```\r\n",
            "let variable = 123;\r\n".repeat(2500)
        ),
        format!("# Items\n{}", "- one\n  - nested item\n".repeat(2500)),
        (0..2000)
            .map(|i| format!("# Section {i}\ntext\n\n"))
            .collect(),
    ];
    for text in cases {
        check(&draft(&text, "md"), &b);
    }
    // S2 refuses empty input, but the core model permits empty or whitespace canonical text.
    for text in ["", " \t\r\n  "] {
        let mut d = draft("placeholder", "txt");
        d.canonical_text = text.into();
        d.nodes.clear();
        d.source_bytes_hash = mindspace::core::sha256_hex(text.as_bytes());
        d.source_size = text.len() as u64;
        assert_eq!(check(&d, &b), usize::from(!text.is_empty()));
    }
}
#[test]
fn generated_coverage_no_gaps_or_overlap() {
    let b = budget();
    let pieces = [
        "# H\n",
        "## Inner\n",
        "text e\u{301} ",
        "漢字 ",
        "🦉",
        "\r\n\r\n",
        "- item\n",
        "```\nfoo();\n```\n",
        "\u{202e}\u{200b}",
    ];
    let mut seed = 7381u64;
    for _ in 0..100 {
        let mut text = String::new();
        for _ in 0..1500 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            text.push_str(pieces[(seed >> 32) as usize % pieces.len()]);
        }
        check(&draft(&text, "md"), &b);
    }
}
#[test]
fn structural_boundaries_and_context_are_used() {
    let b = budget();
    let text = format!(
        "# Outer\n\n## Nested\n\n{}\n\n## Next\nlast\n",
        "word ".repeat(3000)
    );
    let d = draft(&text, "md");
    let p = chunk_document(&d, &b).unwrap();
    check(&d, &b);
    assert!(p.chunks.len() > 1);
    assert!(p.chunks.iter().any(|c| !c.context.is_empty()));
    assert!(p.chunks.iter().any(|c| d
        .nodes
        .iter()
        .any(|n| n.text_range.is_some_and(|(_, end)| end == c.end))));
}

#[test]
#[ignore = "resource measurement; run explicitly in release"]
fn measure_near_input_limit() {
    let b = budget();
    for (label, unit) in [
        ("prose", "A local document about Rust and memory.\n\n"),
        (
            "CJK",
            "漢字中文日本語測試。漢字中文日本語測試。漢字中文日本語測試。\n\n",
        ),
        ("uninterrupted", "x"),
    ] {
        let mut text = unit.repeat(16 * 1024 * 1024 / unit.len());
        while text.len() < 16 * 1024 * 1024 {
            text.push(' ');
        }
        let t = std::time::Instant::now();
        let d = draft(&text, "txt");
        let extract = t.elapsed();
        let t = std::time::Instant::now();
        let whole_tokens = b.count_document(&text).unwrap();
        let tokenize = t.elapsed();
        let t = std::time::Instant::now();
        let p = chunk_document(&d, &b).unwrap();
        eprintln!("MEASURE {label}: bytes={} whole_tokens={whole_tokens} extract={extract:?} whole_tokenize={tokenize:?} chunks={} chunk_time={:?} metadata_json_bytes={} embedding_inputs={} encoded_tokens={}",text.len(),p.chunks.len(),t.elapsed(),serde_json::to_vec(&p.chunks.iter().map(|c| (c.start,c.end,c.tokens,&c.context)).collect::<Vec<_>>()).unwrap().len(),p.chunks.len(),p.chunks.iter().map(|c|c.tokens).sum::<usize>());
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            eprintln!(
                "MEASURE peak {}",
                status
                    .lines()
                    .find(|s| s.starts_with("VmHWM:"))
                    .unwrap_or("unknown")
            );
        }
    }
}

#[test]
fn contradictory_section_ranges_are_rejected_without_amplifying_context() {
    use mindspace::core::{AnchorPrecision, DocNodeDraft, NodeKind};
    let b = budget();
    let node = |path: &[u32], range| DocNodeDraft {
        path: path.into(),
        kind: NodeKind::Section,
        text_range: Some(range),
        page: None,
        bbox: None,
        precision: AnchorPrecision::ByteExact,
    };
    let mut d = draft(&"word ".repeat(200), "txt");
    for nodes in [
        vec![node(&[1], (0, 1000)), node(&[2], (0, 1000))],
        vec![node(&[1], (0, 600)), node(&[2], (400, 1000))],
        vec![node(&[1], (0, 400)), node(&[1, 1], (500, 900))],
        (1..=256).map(|i| node(&[i], (0, 1000))).collect(),
    ] {
        d.nodes = nodes;
        d.validate().unwrap(); // Valid S1 drafts; reject specifically at the S3 boundary.
        assert!(matches!(
            chunk_document(&d, &b),
            Err(mindspace::core::Error::InvalidDocument(_))
        ));
    }
    for nodes in [
        vec![node(&[1], (0, 1000)), node(&[1, 1], (0, 1000))],
        vec![node(&[1], (0, 500)), node(&[2], (500, 1000))],
        vec![node(&[1], (0, 1000)), node(&[1, 1], (400, 400))],
    ] {
        d.nodes = nodes;
        check(&d, &b);
    }
}

#[test]
fn structural_context_is_metadata_and_augmented_payloads_need_their_own_budget() {
    let b = budget();
    let d = draft(&format!("# Heading\n\n{}", "hello ".repeat(2042)), "md");
    let plan = chunk_document(&d, &b).unwrap();
    assert!(plan.chunks.iter().any(|c| !c.context.is_empty()));
    for c in plan.chunks {
        assert_eq!(
            c.tokens,
            b.validate_document(&d.canonical_text[c.start as usize..c.end as usize])
                .unwrap()
        );
    }
    let exact = "hello ".repeat(2042);
    assert_eq!(b.validate_document(&exact).unwrap(), 2048);
    assert!(b.validate_document(&format!("heading {exact}")).is_err());
}
