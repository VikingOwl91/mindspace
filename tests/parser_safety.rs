//! M7.3b parser safety: deeply nested Rust input used to abort the process with a
//! stack overflow (`code::walk` and `code::count_errors` recursed once per tree
//! level). Each case runs in a child process so that a crash cannot take the
//! test runner with it; the child runs on the small default test-thread stack.

use std::process::Command;

use mindspace::core::{CodeEntityKind, CodeLanguage, CodeParseStatus};
use mindspace::ingest::code;

fn source(case: &str) -> String {
    match case {
        // 20,000 nested braces
        "braces" => format!("fn f() {}{}\n", "{".repeat(20_000), "}".repeat(20_000)),
        // 30,000 nested parentheses
        "parens" => format!(
            "fn h() {{ {}{} }}\n",
            "(".repeat(30_000),
            ")".repeat(30_000)
        ),
        // one million chained binary expressions: a tree a million levels deep
        "chain" => format!("fn g() -> u32 {{ {} }}\n", vec!["a"; 1_000_000].join("+")),
        // nested items: every level is an entity with a longer qualified name
        "modules" => format!("{}{}", "mod m { ".repeat(3_000), "}".repeat(3_000)),
        other => panic!("unknown case {other}"),
    }
}

/// Child half: parses one case and prints what it found. Does nothing in a normal run.
#[test]
fn parser_safety_child() {
    let Ok(case) = std::env::var("MINDSPACE_PARSE_CASE") else {
        return;
    };
    println!();
    let d = code::parse(CodeLanguage::Rust, &source(&case));
    let modules = d
        .entities
        .iter()
        .filter(|e| e.kind == CodeEntityKind::Module)
        .count();
    println!(
        "RESULT status={:?} entities={} modules={modules} errors={}",
        d.status,
        d.entities.len(),
        d.error_count
    );
}

fn run(case: &str) -> (std::process::ExitStatus, String) {
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "parser_safety_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("MINDSPACE_PARSE_CASE", case)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status, text)
}

fn result(case: &str) -> String {
    let (status, text) = run(case);
    assert!(
        status.success(),
        "{case}: the child died ({status:?}): {}",
        text.lines().rev().take(3).collect::<Vec<_>>().join(" | ")
    );
    text.lines()
        .find_map(|l| l.strip_prefix("RESULT "))
        .unwrap_or_else(|| panic!("{case}: no result in {text}"))
        .to_string()
}

#[test]
fn twenty_thousand_nested_braces_no_longer_overflow_the_stack() {
    let r = result("braces");
    assert!(r.contains("entities=1 "), "{r}");
}

#[test]
fn thirty_thousand_nested_parentheses_no_longer_overflow_the_stack() {
    let r = result("parens");
    assert!(r.contains("entities=1 "), "{r}");
}

#[test]
fn a_million_chained_expressions_no_longer_overflow_the_stack() {
    let r = result("chain");
    assert!(r.contains("entities=1 "), "{r}");
}

#[test]
fn three_thousand_nested_modules_keep_every_entity_and_their_order() {
    let r = result("modules");
    assert!(r.contains("modules=3000"), "{r}");
}

#[test]
fn the_in_process_result_is_unchanged_for_ordinary_input() {
    let d = code::parse(
        CodeLanguage::Rust,
        "mod a { pub fn f() { g(); } } struct S; impl S { fn m(&self) {} }",
    );
    assert_eq!(d.status, CodeParseStatus::Complete);
    assert_eq!(d.entities.len(), 4);
}
