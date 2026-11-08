//! M7.3b.1 resource probe: where does memory go when a Rust file is parsed?
//! usage: parse_probe <file> <tree|full>
fn kb(key: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        .unwrap_or(0)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let text = std::fs::read_to_string(&a[1]).unwrap();
    println!(
        "input {} MiB, rss start {} MiB",
        text.len() >> 20,
        kb("VmRSS:") >> 10
    );
    let t = std::time::Instant::now();
    if a[2] == "tree" {
        let mut p = tree_sitter::Parser::new();
        p.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let tree = p.parse(&text, None).unwrap();
        println!(
            "tree-sitter tree only: {} ms, peak rss {} MiB, nodes (root child_count {})",
            t.elapsed().as_millis(),
            kb("VmHWM:") >> 10,
            tree.root_node().child_count()
        );
    } else if a[2] == "policy" {
        let o = mindspace::ingest::code::parse_with_policy(
            mindspace::core::CodeLanguage::Rust,
            &text,
            &mindspace::ingest::code::ParsePolicy::default(),
        );
        println!(
            "parse_with_policy(p1): {} ms, peak rss {} MiB, entities {} rels {} skipped {:?} accounted {} B",
            t.elapsed().as_millis(),
            kb("VmHWM:") >> 10,
            o.derivation.entities.len(),
            o.derivation.relationships.len(),
            o.skipped.map(|r| r.label()),
            o.accounted_bytes
        );
    } else {
        let d = mindspace::ingest::code::parse(mindspace::core::CodeLanguage::Rust, &text);
        println!(
            "code::parse: {} ms, peak rss {} MiB, entities {} imports {} rels {}",
            t.elapsed().as_millis(),
            kb("VmHWM:") >> 10,
            d.entities.len(),
            d.imports.len(),
            d.relationships.len()
        );
    }
}
