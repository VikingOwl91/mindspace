//! Standalone S3 benchmark; see s3-scaling.md. No inference or persistent database writes.
fn root() -> std::path::PathBuf {
    std::env::var_os("MINDSPACE_S3_ROOT")
        .map(std::path::PathBuf::from)
        .expect("set MINDSPACE_S3_ROOT")
}
fn main() {
    if std::env::args()
        .nth(1)
        .is_some_and(|s| s.starts_with("sql-"))
    {
        sql_main();
    } else {
        chunk_main();
    }
}
use mindspace::core::Ts;
use mindspace::embeddings::{budget::TokenBudget, manifest::NOMIC_V1_5};
use mindspace::ingest::document::{
    chunk::chunk_document, extract_document_bytes, DocumentIngestOptions, Identity,
};
use std::time::Instant;
fn chunk_main() {
    let args: Vec<String> = std::env::args().collect();
    let kind = &args[1];
    let size: usize = args[2].parse().unwrap();
    let unit=match kind.as_str(){
 "prose"=>"A local document about Rust and memory.\n\n",
 "cjk"=>"漢字中文日本語測試。漢字中文日本語測試。漢字中文日本語測試。\n\n",
 "unbroken"=>"x",
 "markdown"=>"# Heading\n\nWords and Unicode e\u{301} 🦉.\n\n## Subsection\n\n- first item\n- second item\n\n```rust\nlet value = 123;\n```\n\n",
 "tiny_sections"=>"# H\nx\n",
 _=>panic!("unknown fixture")};
    let mut text = unit.repeat(size / unit.len());
    while text.len() < size {
        text.push(' ');
    }
    let budget = TokenBudget::from_file(
        &root().join("tests/fixtures/nomic-tokenizer.json"),
        &NOMIC_V1_5,
    )
    .unwrap();
    let t = Instant::now();
    let draft = extract_document_bytes(
        "/perf/source.md",
        text.as_bytes(),
        Some(if kind == "markdown" || kind == "tiny_sections" {
            "md"
        } else {
            "txt"
        }),
        &DocumentIngestOptions::default(),
        &Identity::default(),
        Ts::now(),
    )
    .unwrap()
    .draft;
    let extraction_ms = t.elapsed().as_secs_f64() * 1000.;
    let before = std::fs::read_to_string("/proc/self/status").unwrap();
    let t = Instant::now();
    let p = chunk_document(&draft, &budget).unwrap();
    let chunk_ms = t.elapsed().as_secs_f64() * 1000.;
    let after = std::fs::read_to_string("/proc/self/status").unwrap();
    let peak = |s: &str| -> u64 {
        s.lines()
            .find(|l| l.starts_with("VmHWM:"))
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap()
    };
    let mut at = 0;
    let mut max_tokens = 0;
    for c in &p.chunks {
        assert_eq!(at, c.start as usize);
        assert!(c.end > c.start);
        let slice = &text[c.start as usize..c.end as usize];
        let n = budget.validate_document(slice).unwrap();
        assert_eq!(c.tokens, n);
        max_tokens = max_tokens.max(n);
        // Context paths are metadata: verify enclosing nodes, never add their text to the payload.
        for path in &c.context {
            let node = draft.nodes.iter().find(|n| n.path == *path).unwrap();
            let (a, b) = node.text_range.unwrap();
            assert!(a <= c.start && b >= c.end);
        }
        at = c.end as usize;
    }
    assert_eq!(at, text.len());
    println!("{{\"kind\":\"{kind}\",\"bytes\":{size},\"nodes\":{},\"chunks\":{},\"max_tokens\":{max_tokens},\"extract_ms\":{extraction_ms:.3},\"chunk_ms\":{chunk_ms:.3},\"peak_before_kib\":{},\"peak_after_kib\":{}}}",draft.nodes.len(),p.chunks.len(),peak(&before),peak(&after));
}

use rusqlite::{params, Connection};

fn sql_main() {
    let a: Vec<String> = std::env::args().collect();
    let n: usize = a[2].parse().unwrap();
    let mut c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE sources(id TEXT PRIMARY KEY,content TEXT); CREATE TABLE doc_extractions(id TEXT PRIMARY KEY,source_id TEXT);").unwrap();
    let text = "x".repeat(n);
    c.execute("INSERT INTO sources VALUES ('s',?1)", [text])
        .unwrap();
    c.execute("INSERT INTO doc_extractions VALUES ('e','s')", [])
        .unwrap();
    let mut sql = std::fs::read_to_string(root().join("migrations/0012_doc_chunks.sql")).unwrap();
    if a[1] == "sql-old" {
        sql = sql.replace("octet_length(s.content)", "length(CAST(s.content AS BLOB))");
    }
    c.execute_batch(&sql).unwrap();
    c.execute(
        "INSERT INTO doc_chunk_sets VALUES ('e','v','b',?1,?2)",
        params!["a".repeat(64), (n / 8192) as i64],
    )
    .unwrap();
    let version: String = c
        .query_row("select sqlite_version()", [], |r| r.get(0))
        .unwrap();
    let start = Instant::now();
    let tx = c.transaction().unwrap();
    {
        let mut s = tx
            .prepare("INSERT INTO doc_chunks VALUES (?1,'e','v','b',?2,?3,?4,1,'[]','synthetic')")
            .unwrap();
        for i in 0..n / 8192 {
            s.execute(params![
                format!("{i:064x}"),
                i as i64,
                (i * 8192) as i64,
                ((i + 1) * 8192) as i64
            ])
            .unwrap();
        }
    }
    tx.commit().unwrap();
    println!(
        "{{\"variant\":\"{}\",\"bytes\":{n},\"chunks\":{},\"sqlite\":\"{version}\",\"ms\":{:.3}}}",
        a[1],
        n / 8192,
        start.elapsed().as_secs_f64() * 1000.
    );
}
