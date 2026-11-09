//! Method versus function classification in the Rust extractor (MIND-5).
//!
//! An entity is a `Method` exactly when it is an associated item: its direct
//! container is an `impl` block or a `trait`. The extractor used to decide
//! this by looking for the substring "impl" or "trait" in the *syntax path of
//! the enclosing entity*, and that path contains the enclosing entity's
//! *name*. So a function nested in `fn impl_helper()` or `mod traits` was
//! reported as a method.
//!
//! The tests fall into three groups, kept apart on purpose:
//!
//! 1. MISCLASSIFICATIONS: the syntax is supported and the extractor was wrong.
//!    These failed before the fix (observed, see the commit message).
//! 2. GUARDS: classifications that were already right and must stay right.
//! 3. UNSUPPORTED BY DESIGN: syntax the extractor deliberately does not
//!    interpret (no macro expansion, no `cfg` evaluation). They pin today's
//!    behavior so a change is a conscious decision, not an accident. They are
//!    not defects, and the fix must not "improve" them silently.

use mindspace::core::{CodeDerivationDraft, CodeEntityKind, CodeLanguage, CodeParseStatus};
use mindspace::ingest::code;

fn parse(source: &str) -> CodeDerivationDraft {
    let d = code::parse(CodeLanguage::Rust, source);
    assert_eq!(
        d.status,
        CodeParseStatus::Complete,
        "fixture must parse cleanly:\n{source}"
    );
    d
}

/// The kind of the entities called `name`. A trait declaration and its impl
/// share a name, so several entities are fine as long as they agree.
fn kind_of(d: &CodeDerivationDraft, name: &str) -> CodeEntityKind {
    let kinds: Vec<_> = d
        .entities
        .iter()
        .filter(|e| e.name.as_deref() == Some(name))
        .map(|e| e.kind)
        .collect();
    assert!(!kinds.is_empty(), "no entity named {name}");
    assert!(
        kinds.iter().all(|k| *k == kinds[0]),
        "entities named {name} disagree: {kinds:?}"
    );
    kinds[0]
}

fn assert_kinds(source: &str, expected: &[(&str, CodeEntityKind)]) {
    let d = parse(source);
    for (name, kind) in expected {
        assert_eq!(kind_of(&d, name), *kind, "`{name}` in:\n{source}");
    }
}

use CodeEntityKind::{Function, Method, Module};

// ---------------------------------------------------------------------------
// 1. Misclassifications (failed before the fix)
// ---------------------------------------------------------------------------

#[test]
fn function_nested_in_a_free_function_named_like_impl() {
    assert_kinds(
        "fn impl_helper() { fn inner() {} }",
        &[("impl_helper", Function), ("inner", Function)],
    );
    assert_kinds(
        "fn reimplement() { fn inner2() {} }",
        &[("reimplement", Function), ("inner2", Function)],
    );
}

#[test]
fn free_functions_in_modules_named_like_impl_or_trait() {
    assert_kinds(
        "mod impl_utils { pub fn f1() {} }",
        &[("impl_utils", Module), ("f1", Function)],
    );
    assert_kinds(
        "mod traits { pub fn f2() {} }",
        &[("traits", Module), ("f2", Function)],
    );
    assert_kinds("mod trait_objects { fn f3() {} }", &[("f3", Function)]);
    assert_kinds("mod r#impl { fn f4() {} }", &[("f4", Function)]);
}

#[test]
fn function_nested_in_a_method_whose_name_contains_impl_or_trait() {
    assert_kinds(
        "struct S; impl S { fn trait_name(&self) { fn helper() {} } }",
        &[("trait_name", Method), ("helper", Function)],
    );
    assert_kinds(
        "trait Tr { fn impl_m(&self) { fn nested_in_default() {} } }",
        &[("impl_m", Method), ("nested_in_default", Function)],
    );
}

#[test]
fn function_in_a_closure_inside_a_function_named_like_impl() {
    assert_kinds(
        "fn impl_x() { let c = || { fn deep() {} }; c(); }",
        &[("impl_x", Function), ("deep", Function)],
    );
}

// ---------------------------------------------------------------------------
// 2. Guards (already correct; must stay correct)
// ---------------------------------------------------------------------------

#[test]
fn inherent_and_trait_impl_items_are_methods() {
    assert_kinds(
        "struct S; trait T { fn decl(&self); fn dflt(&self) {} }\n\
         impl S { fn new() -> S { S } fn m(&self) {} }\n\
         impl T for S { fn decl(&self) {} }",
        &[("new", Method), ("m", Method), ("dflt", Method)],
    );
}

#[test]
fn generic_and_bounded_impls_are_methods() {
    assert_kinds(
        "trait Tr { fn go(&self); }\n\
         impl<T: Clone> Tr for Vec<T> where T: Send { fn go(&self) {} }\n\
         struct W<T>(T); impl<T> W<T> { pub async fn a(&self) {} pub const fn c() {} pub unsafe fn u() {} }",
        &[("go", Method), ("a", Method), ("c", Method), ("u", Method)],
    );
}

#[test]
fn impl_inside_a_module_or_a_function_body_still_yields_methods() {
    assert_kinds(
        "mod m { struct A; impl A { fn in_mod(&self) {} } }\n\
         fn make() { struct B; impl B { fn in_body(&self) {} } }",
        &[("in_mod", Method), ("in_body", Method), ("make", Function)],
    );
}

#[test]
fn function_nested_in_a_plainly_named_method_is_a_function() {
    assert_kinds(
        "struct S; impl S { fn run(&self) { fn helper() {} } }",
        &[("run", Method), ("helper", Function)],
    );
}

#[test]
fn bodiless_extern_functions_are_functions_not_methods() {
    assert_kinds(
        r#"extern "C" { fn c_func(x: u32) -> u32; }"#,
        &[("c_func", Function)],
    );
}

#[test]
fn qualified_names_still_use_the_implementing_type() {
    let d =
        parse("trait Store { fn put(&self); } struct Sql; impl Store for Sql { fn put(&self) {} }");
    let q: Vec<_> = d
        .entities
        .iter()
        .filter(|e| e.name.as_deref() == Some("put"))
        .map(|e| e.qualified_name.clone().unwrap())
        .collect();
    assert!(
        q.contains(&"Store::put".to_string()) && q.contains(&"Sql::put".to_string()),
        "{q:?}"
    );
}

/// Malformed code has no ground truth beyond tree-sitter's recovered tree: here
/// the parser closes the impl early, so `after` ends up outside it. This pins
/// the behavior observed with `mind-code@2` (`before` and `broken` stay inside
/// the impl, `after` does not) so that moving from a name-based rule to an
/// ancestor-based one cannot change how broken files are classified.
#[test]
fn classification_of_malformed_code_follows_the_recovered_tree() {
    let d = code::parse(
        CodeLanguage::Rust,
        "struct S;\nimpl S {\n    fn before(&self) {}\n    fn broken(&self {\n    fn after(&self) {}\n}\n",
    );
    assert_ne!(
        d.status,
        CodeParseStatus::Complete,
        "fixture should be malformed"
    );
    assert_eq!(kind_of(&d, "before"), Method);
    assert_eq!(kind_of(&d, "broken"), Method);
    assert_eq!(kind_of(&d, "after"), Function);
}

/// The same name-independent invariant on deliberately corrupted sources:
/// deterministic pseudo-random byte deletions and token insertions into the
/// project's own files. Broken code produces `ERROR` nodes in odd places; the
/// classification must still agree with the entity tree the extractor emits.
#[test]
fn corrupted_real_sources_keep_the_name_independent_invariant() {
    fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                collect(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    collect(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    files.sort();

    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    const TOKENS: [&str; 9] = ["{", "}", "(", ")", ";", "@@", " fn ", " impl ", "->"];
    let (mut checked, mut malformed) = (0u32, 0u32);
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap();
        for _ in 0..6 {
            let mut bytes = src.clone().into_bytes();
            for _ in 0..(1 + next() % 4) {
                let at = (next() as usize) % bytes.len().max(1);
                if next() % 2 == 0 {
                    let end = (at + 1 + (next() as usize) % 12).min(bytes.len());
                    bytes.drain(at..end);
                } else {
                    let t = TOKENS[(next() as usize) % TOKENS.len()].as_bytes();
                    bytes.splice(at..at, t.iter().copied());
                }
            }
            let Ok(variant) = String::from_utf8(bytes) else {
                continue;
            };
            let d = code::parse(CodeLanguage::Rust, &variant);
            if d.status != CodeParseStatus::Complete {
                malformed += 1;
            }
            for e in &d.entities {
                if !matches!(e.kind, Method | Function) {
                    continue;
                }
                let in_container = e
                    .parent_syntax_path
                    .as_deref()
                    .is_some_and(|p| p.starts_with("impl:") || p.starts_with("trait:"));
                assert_eq!(
                    e.kind == Method,
                    in_container,
                    "{}: `{}` is {:?}, parent {:?}",
                    f.display(),
                    e.name.as_deref().unwrap_or("?"),
                    e.kind,
                    e.parent_syntax_path
                );
                checked += 1;
            }
        }
    }
    assert!(
        malformed > 100,
        "corruption produced too few malformed files: {malformed}"
    );
    assert!(checked > 2_000, "oracle saw too little: {checked}");
}

// ---------------------------------------------------------------------------
// 3. Unsupported by design (pinned, not defects)
// ---------------------------------------------------------------------------

/// Macro bodies are token trees; the extractor does not expand macros, so
/// items they would generate are not entities.
#[test]
fn macro_generated_items_are_not_entities() {
    let d = parse(
        "macro_rules! gen { () => { fn generated() {} impl S { fn gen_method(&self) {} } } }\n\
         struct S;\n\
         gen!();",
    );
    for name in ["generated", "gen_method"] {
        assert!(
            d.entities.iter().all(|e| e.name.as_deref() != Some(name)),
            "{name}: macro expansion is not supported, so it must not appear"
        );
    }
}

/// `cfg` attributes are not evaluated: every branch is extracted as written.
#[test]
fn cfg_gated_items_are_extracted_regardless_of_the_condition() {
    assert_kinds(
        "struct S;\n\
         #[cfg(any())] impl S { fn never_compiled(&self) {} }\n\
         #[cfg(test)] mod tests { fn only_in_tests() {} }",
        &[("never_compiled", Method), ("only_in_tests", Function)],
    );
}

// ---------------------------------------------------------------------------
// Independent oracle over a real corpus
// ---------------------------------------------------------------------------

/// Name-independent invariant: an entity is a method exactly when its parent
/// entity is an impl or a trait. The parent's syntax path starts with the
/// parent's *kind* (`impl:`, `trait:`, ...), never with a user-chosen name, so
/// this does not repeat the logic under test. Checked on the project's own
/// sources, which are real, non-synthetic Rust.
#[test]
fn methods_are_exactly_the_items_directly_inside_impl_or_trait_on_real_sources() {
    fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                collect(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    collect(&root.join("src"), &mut files);
    collect(&root.join("tests"), &mut files);
    assert!(files.len() > 20, "corpus too small: {}", files.len());

    let (mut methods, mut functions) = (0u32, 0u32);
    for f in files {
        let src = std::fs::read_to_string(&f).unwrap();
        let d = code::parse(CodeLanguage::Rust, &src);
        for e in &d.entities {
            if !matches!(e.kind, Method | Function) {
                continue;
            }
            let in_assoc_container = e
                .parent_syntax_path
                .as_deref()
                .is_some_and(|p| p.starts_with("impl:") || p.starts_with("trait:"));
            assert_eq!(
                e.kind == Method,
                in_assoc_container,
                "{}: `{}` is {:?} but parent is {:?}",
                f.display(),
                e.name.as_deref().unwrap_or("?"),
                e.kind,
                e.parent_syntax_path
            );
            if e.kind == Method {
                methods += 1
            } else {
                functions += 1
            }
        }
    }
    assert!(
        methods > 200 && functions > 100,
        "oracle saw too little: {methods} methods, {functions} functions"
    );
}

// ---------------------------------------------------------------------------------------------
// Recovered trees (found by `corrupted_real_sources_keep_the_name_independent_invariant`)
// ---------------------------------------------------------------------------------------------

/// A 96-byte reduction (delta debugging against the invariant) of a corrupted copy of
/// `src/ingest/confined.rs`. Tree-sitter's error recovery inserts a `foreign_mod_item` (with a
/// MISSING `extern`) into the `impl` body and `read_beneath` becomes a `function_signature_item`
/// inside it. The traversal context passes through that wrapper, so `parent_syntax_path` was
/// `impl:Anchor@0`, while `is_associated_item` stopped at the wrapper and answered "no": one entity
/// record that contradicted itself (`Function` with an impl parent).
const RECOVERED_FOREIGN_MOD_IN_IMPL: &str =
    include_str!("fixtures/code/recovered_foreign_mod_in_impl.rs");

fn in_container(e: &mindspace::core::CodeEntityDraft) -> bool {
    e.parent_syntax_path
        .as_deref()
        .is_some_and(|p| p.starts_with("impl:") || p.starts_with("trait:"))
}

#[test]
fn a_recovery_wrapper_inside_an_impl_does_not_make_the_record_contradict_itself() {
    let d = code::parse(CodeLanguage::Rust, RECOVERED_FOREIGN_MOD_IN_IMPL);
    // the premise: this really is a recovered tree with the three entities
    assert_eq!(d.status, CodeParseStatus::Recovered);
    let by_name = |n: &str| {
        d.entities
            .iter()
            .find(|e| e.name.as_deref() == Some(n))
            .unwrap_or_else(|| panic!("no entity {n}"))
    };
    for n in ["dup_fd", "proc_path", "read_beneath"] {
        let e = by_name(n);
        assert_eq!(
            e.parent_syntax_path.as_deref(),
            Some("impl:Anchor@0"),
            "{n}"
        );
        // the invariant, entity by entity
        assert_eq!(e.kind == Method, in_container(e), "{n} is {:?}", e.kind);
    }
    // and the decision this implies: the item sits in the impl with no entity in between
    assert_eq!(by_name("read_beneath").kind, Method);
}

/// What must NOT change: for sources that parse, the nearest enclosing entity decides, exactly as
/// before. (The corpus comparison over thousands of real files is recorded in the commit message.)
#[test]
fn associated_items_are_still_decided_by_the_nearest_enclosing_entity() {
    let d = parse(
        r#"
extern "C" { fn ffi(); }
mod m { extern "C" { fn ffi_in_mod(); } fn in_mod() {} }
struct S;
impl S {
    fn method(&self) { fn nested_in_method() {} let _c = || { fn in_closure() {} }; }
    const C: u8 = { fn in_assoc_const() {} 1 };
}
trait T { fn required(); fn provided() { fn nested_in_provided() {} } }
fn free() { fn nested_in_free() {} }
"#,
    );
    for (n, k) in [
        ("ffi", Function),
        ("ffi_in_mod", Function),
        ("in_mod", Function),
        ("method", Method),
        ("nested_in_method", Function),
        ("in_closure", Function),
        ("in_assoc_const", Function),
        ("required", Method),
        ("provided", Method),
        ("nested_in_provided", Function),
        ("free", Function),
        ("nested_in_free", Function),
    ] {
        assert_eq!(kind_of(&d, n), k, "{n}");
    }
}
