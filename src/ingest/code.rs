//! Tree-sitter based, byte-only Rust structure extraction. No repository code
//! is loaded or executed; parser types end at this module boundary.

use crate::core::{
    CodeDerivationDraft, CodeEntityDraft, CodeEntityKind, CodeImportDraft, CodeLanguage,
    CodeParseStatus, CodeParserIdentity, CodeRange, CodeRelationshipDraft, CodeRelationshipKind,
};
use std::path::Path;
use tree_sitter::{Node, Parser};

pub const IDENTITY: (&str, &str, &str, &str) = ("mind-code", "3", "tree-sitter-rust", "0.24.2");
pub fn identity() -> CodeParserIdentity {
    CodeParserIdentity {
        extractor: IDENTITY.0.into(),
        extractor_version: IDENTITY.1.into(),
        grammar: IDENTITY.2.into(),
        grammar_version: IDENTITY.3.into(),
    }
}
pub fn language_for(path: &Path) -> Option<CodeLanguage> {
    (path.extension()?.to_str()? == "rs").then_some(CodeLanguage::Rust)
}
pub fn parse(language: CodeLanguage, source: &str) -> CodeDerivationDraft {
    let identity = identity();
    if language != CodeLanguage::Rust {
        return CodeDerivationDraft {
            identity,
            status: CodeParseStatus::Failed,
            error_count: 1,
            entities: vec![],
            imports: vec![],
            relationships: vec![],
        };
    }
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .is_err()
    {
        return CodeDerivationDraft {
            identity,
            status: CodeParseStatus::Failed,
            error_count: 1,
            entities: vec![],
            imports: vec![],
            relationships: vec![],
        };
    }
    let Some(tree) = parser.parse(source, None) else {
        return CodeDerivationDraft {
            identity,
            status: CodeParseStatus::Failed,
            error_count: 1,
            entities: vec![],
            imports: vec![],
            relationships: vec![],
        };
    };
    let mut out = CodeDerivationDraft {
        identity,
        status: if tree.root_node().has_error() {
            CodeParseStatus::Recovered
        } else {
            CodeParseStatus::Complete
        },
        error_count: count_errors(tree.root_node()),
        entities: vec![],
        imports: vec![],
        relationships: vec![],
    };
    walk(tree.root_node(), source, &mut out);
    out
}
/// Number of `ERROR` and missing nodes in the tree. Iterative: a tree can be as
/// deep as the input is long, so recursion would be bounded by the thread's stack.
fn count_errors(root: Node) -> u32 {
    let mut count = 0u32;
    let mut cursor = root.walk();
    loop {
        let n = cursor.node();
        count += u32::from(n.is_error() || n.is_missing());
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            // back at the root after its last child: done
            if !cursor.goto_parent() {
                return count;
            }
        }
    }
}
fn range(n: Node) -> CodeRange {
    let a = n.start_position();
    let b = n.end_position();
    CodeRange {
        start_byte: n.start_byte() as u64,
        end_byte: n.end_byte() as u64,
        start_line: a.row as u32 + 1,
        start_column: a.column as u32 + 1,
        end_line: b.row as u32 + 1,
        end_column: b.column as u32 + 1,
    }
}
fn text(n: Node, s: &str) -> String {
    n.utf8_text(s.as_bytes()).unwrap_or("").to_string()
}
fn name(n: Node, s: &str) -> Option<String> {
    n.child_by_field_name("name")
        .map(|x| text(x, s))
        .filter(|x| !x.is_empty())
}
/// True when the item is an associated item: its direct container is an `impl`
/// block or a `trait`. Decided from the syntax tree, never from names:
/// `mind-code@2` looked for "impl"/"trait" in the enclosing entity's syntax
/// path, which contains that entity's *name*, so a function nested in
/// `fn impl_helper()` or `mod traits` was reported as a method.
///
/// Only `declaration_list` (the body of an impl or trait) is looked through;
/// any other ancestor, such as a function body, a module, a closure or a parser
/// `ERROR` node, means the item is not associated. Looking through `ERROR` was
/// considered and left out: no corrupted variant of the project's own sources
/// (600+ malformed files, 3,000+ entities, `tests/code_classification.rs`)
/// needed it, so it would be untested code.
fn is_associated_item(n: Node) -> bool {
    let mut cur = n.parent();
    while let Some(p) = cur {
        match p.kind() {
            "declaration_list" => cur = p.parent(),
            "impl_item" | "trait_item" => return true,
            _ => return false,
        }
    }
    false
}
/// The part of the traversal that belongs to one node: records what the node
/// itself contributes and returns the context its children are visited in.
/// (`mind-code@3` did this recursively, which overflowed the stack on deeply
/// nested input; the order of visits and every value are unchanged.)
fn visit(n: Node, s: &str, ctx: &Ctx, out: &mut CodeDerivationDraft) -> Ctx {
    let parent: Option<String> = ctx.parent.as_deref().map(str::to_string);
    let parent_q: Option<String> = ctx.q.as_deref().map(str::to_string);
    if n.kind() == "call_expression" {
        let raw_target = n
            .child_by_field_name("function")
            .map(|x| text(x, s))
            .unwrap_or_default();
        out.relationships.push(CodeRelationshipDraft {
            kind: CodeRelationshipKind::Calls,
            source_syntax_path: parent.clone(),
            raw_target,
            range: range(n),
        });
    }
    if n.kind() == "impl_item" {
        let trait_target = n.child_by_field_name("trait").map(|node| text(node, s));
        let type_target = n.child_by_field_name("type").map(|node| text(node, s));
        if let Some(target) = trait_target {
            out.relationships.push(CodeRelationshipDraft {
                kind: CodeRelationshipKind::ImplementsTrait,
                source_syntax_path: parent.clone(),
                raw_target: target,
                range: range(n),
            });
        }
        if let Some(target) = type_target.clone() {
            out.relationships.push(CodeRelationshipDraft {
                kind: CodeRelationshipKind::ImplementsType,
                source_syntax_path: parent.clone(),
                raw_target: target,
                range: range(n),
            });
        }
        // The `type` field is the implementing type; for `impl Trait for
        // Type` the first type identifier would be the trait.
        let type_name = type_target.unwrap_or_else(|| "impl".to_string());
        let impl_parent = Some(format!("impl:{type_name}@{}", n.start_byte()));
        let impl_q = Some(
            parent_q
                .as_ref()
                .map(|p| format!("{p}::{type_name}"))
                .unwrap_or(type_name),
        );
        return Ctx::of(impl_parent, impl_q);
    }
    let kind = match n.kind() {
        "mod_item" => Some(CodeEntityKind::Module),
        "struct_item" => Some(CodeEntityKind::Struct),
        "enum_item" => Some(CodeEntityKind::Enum),
        "trait_item" => Some(CodeEntityKind::Trait),
        "type_item" => Some(CodeEntityKind::TypeAlias),
        "function_item" | "function_signature_item" => Some(if is_associated_item(n) {
            CodeEntityKind::Method
        } else {
            CodeEntityKind::Function
        }),
        "const_item" => Some(CodeEntityKind::Constant),
        "static_item" => Some(CodeEntityKind::Static),
        "field_declaration" => Some(CodeEntityKind::Field),
        "use_declaration" => Some(CodeEntityKind::Import),
        _ => None,
    };
    let mut next_parent = parent.clone();
    let mut next_q = parent_q.clone();
    if let Some(k) = kind {
        if k == CodeEntityKind::Import {
            let raw = text(n, s);
            for (target, alias, start, end) in rust_use_targets(&raw) {
                let mut target_range = range(n);
                target_range.start_byte += start as u64;
                target_range.end_byte = target_range.start_byte + (end - start) as u64;
                // The declaration can be single-line or formatted.  Its full
                // line/column span remains the honest normalized location;
                // byte subranges provide distinct immutable identities for a
                // grouped `use` without pretending to resolve it.
                out.imports.push(CodeImportDraft {
                    raw: raw.clone(),
                    target: target.clone(),
                    alias,
                    range: target_range.clone(),
                });
                out.relationships.push(CodeRelationshipDraft {
                    kind: CodeRelationshipKind::Imports,
                    source_syntax_path: parent.clone(),
                    raw_target: target,
                    range: target_range,
                });
            }
            out.entities.push(CodeEntityDraft {
                kind: k,
                name: None,
                qualified_name: None,
                syntax_path: format!("import@{}", n.start_byte()),
                parent_syntax_path: parent.clone(),
                range: range(n),
                signature: Some(raw),
                visibility: None,
                is_async: false,
            });
        } else {
            let local = name(n, s);
            let path = format!(
                "{}:{}@{}",
                k.as_str(),
                local.clone().unwrap_or_default(),
                n.start_byte()
            );
            let q = local.as_ref().map(|x| {
                parent_q
                    .as_ref()
                    .map(|p| format!("{p}::{x}"))
                    .unwrap_or_else(|| x.clone())
            });
            let raw = text(n, s);
            let visibility = raw
                .trim_start()
                .starts_with("pub")
                .then_some("public".into());
            let async_ = raw.trim_start().starts_with("async ")
                || raw.trim_start().starts_with("pub async ");
            out.entities.push(CodeEntityDraft {
                kind: k,
                name: local,
                qualified_name: q.clone(),
                syntax_path: path.clone(),
                parent_syntax_path: parent.clone(),
                range: range(n),
                signature: (k == CodeEntityKind::Function || k == CodeEntityKind::Method)
                    .then_some(raw.lines().next().unwrap_or("").to_string()),
                visibility,
                is_async: async_,
            });
            next_parent = Some(path);
            next_q = q;
        }
    }
    // Children of a node that is not an entity share its parent's context.
    if kind.is_none() {
        return ctx.clone();
    }
    Ctx::of(next_parent, next_q)
}

/// What a node's descendants inherit: the syntax path and qualified name of the
/// closest enclosing entity (shared, not copied, between siblings).
#[derive(Clone)]
struct Ctx {
    parent: Option<std::rc::Rc<str>>,
    q: Option<std::rc::Rc<str>>,
}

impl Ctx {
    fn none() -> Self {
        Self {
            parent: None,
            q: None,
        }
    }
    fn of(parent: Option<String>, q: Option<String>) -> Self {
        Self {
            parent: parent.map(Into::into),
            q: q.map(Into::into),
        }
    }
}

/// Pre-order traversal of every node (named and anonymous) with an explicit,
/// heap-allocated stack of contexts: the depth is limited by memory, not by the
/// thread's stack.
fn walk(root: Node, s: &str, out: &mut CodeDerivationDraft) {
    let mut cursor = root.walk();
    // stack[d] is the context the children of the node at depth d inherit
    let mut stack: Vec<Ctx> = vec![visit(root, s, &Ctx::none(), out)];
    loop {
        if cursor.goto_first_child() {
            let inherited = stack.last().cloned().unwrap_or_else(Ctx::none);
            let next = visit(cursor.node(), s, &inherited, out);
            stack.push(next);
            continue;
        }
        loop {
            stack.pop();
            if cursor.goto_next_sibling() {
                let inherited = stack.last().cloned().unwrap_or_else(Ctx::none);
                let next = visit(cursor.node(), s, &inherited, out);
                stack.push(next);
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

/// Normalizes only the literal Rust `use_declaration` text already selected by
/// Tree-sitter.  This is deliberately not a source parser: grammar selection
/// and declaration boundaries come from Tree-sitter; this just splits its
/// deterministic path-list surface into independently storable facts.
fn rust_use_targets(raw: &str) -> Vec<(String, Option<String>, usize, usize)> {
    let declaration = raw.trim();
    let Some(body) = declaration
        .strip_prefix("use")
        .map(str::trim)
        .and_then(|body| body.strip_suffix(';'))
        .map(str::trim)
    else {
        return vec![];
    };
    let base_offset = raw.find(body).unwrap_or(0);
    let expand = |target: &str, offset: usize| {
        let target = target.trim();
        let (target, alias) = target
            .rsplit_once(" as ")
            .map_or((target, None), |(path, alias)| {
                (path.trim(), Some(alias.trim().to_string()))
            });
        (!target.is_empty()).then_some((target.to_string(), alias, offset, offset + target.len()))
    };
    if let Some((prefix, rest)) = body.split_once("::{") {
        if let Some(items) = rest.strip_suffix('}') {
            let prefix = prefix.trim_end_matches("::").trim();
            let items_offset = body.find('{').unwrap_or(0) + 1;
            let mut offset = 0;
            return items
                .split(',')
                .map(|raw_item| {
                    let leading = raw_item.len() - raw_item.trim_start().len();
                    let item_offset = base_offset + items_offset + offset + leading;
                    offset += raw_item.len() + 1;
                    let item = raw_item.trim();
                    let (item_path, alias) = item
                        .rsplit_once(" as ")
                        .map_or((item, None), |(path, alias)| {
                            (path.trim(), Some(alias.trim().to_string()))
                        });
                    let target = format!("{prefix}::{item_path}");
                    (target, alias, item_offset, item_offset + item_path.len())
                })
                .collect();
        }
    }
    expand(body, base_offset).into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The recursive implementation of `mind-code@3`, kept as the reference the
    /// iterative traversal is compared with. Test-only: it overflows the stack on
    /// deeply nested input, which is why it was replaced.
    mod reference {
        use super::super::*;
        fn count_errors_recursive(n: Node) -> u32 {
            let mut count = u32::from(n.is_error() || n.is_missing());
            let mut c = n.walk();
            for x in n.children(&mut c) {
                count += count_errors_recursive(x)
            }
            count
        }
        fn walk_recursive(
            n: Node,
            s: &str,
            parent: Option<String>,
            parent_q: Option<String>,
            out: &mut CodeDerivationDraft,
        ) {
            if n.kind() == "call_expression" {
                let raw_target = n
                    .child_by_field_name("function")
                    .map(|x| text(x, s))
                    .unwrap_or_default();
                out.relationships.push(CodeRelationshipDraft {
                    kind: CodeRelationshipKind::Calls,
                    source_syntax_path: parent.clone(),
                    raw_target,
                    range: range(n),
                });
            }
            if n.kind() == "impl_item" {
                let trait_target = n.child_by_field_name("trait").map(|node| text(node, s));
                let type_target = n.child_by_field_name("type").map(|node| text(node, s));
                if let Some(target) = trait_target {
                    out.relationships.push(CodeRelationshipDraft {
                        kind: CodeRelationshipKind::ImplementsTrait,
                        source_syntax_path: parent.clone(),
                        raw_target: target,
                        range: range(n),
                    });
                }
                if let Some(target) = type_target.clone() {
                    out.relationships.push(CodeRelationshipDraft {
                        kind: CodeRelationshipKind::ImplementsType,
                        source_syntax_path: parent.clone(),
                        raw_target: target,
                        range: range(n),
                    });
                }
                // The `type` field is the implementing type; for `impl Trait for
                // Type` the first type identifier would be the trait.
                let type_name = type_target.unwrap_or_else(|| "impl".to_string());
                let impl_parent = Some(format!("impl:{type_name}@{}", n.start_byte()));
                let impl_q = Some(
                    parent_q
                        .as_ref()
                        .map(|p| format!("{p}::{type_name}"))
                        .unwrap_or(type_name),
                );
                let mut cursor = n.walk();
                for child in n.children(&mut cursor) {
                    walk_recursive(child, s, impl_parent.clone(), impl_q.clone(), out);
                }
                return;
            }
            let kind = match n.kind() {
                "mod_item" => Some(CodeEntityKind::Module),
                "struct_item" => Some(CodeEntityKind::Struct),
                "enum_item" => Some(CodeEntityKind::Enum),
                "trait_item" => Some(CodeEntityKind::Trait),
                "type_item" => Some(CodeEntityKind::TypeAlias),
                "function_item" | "function_signature_item" => Some(if is_associated_item(n) {
                    CodeEntityKind::Method
                } else {
                    CodeEntityKind::Function
                }),
                "const_item" => Some(CodeEntityKind::Constant),
                "static_item" => Some(CodeEntityKind::Static),
                "field_declaration" => Some(CodeEntityKind::Field),
                "use_declaration" => Some(CodeEntityKind::Import),
                _ => None,
            };
            let mut next_parent = parent.clone();
            let mut next_q = parent_q.clone();
            if let Some(k) = kind {
                if k == CodeEntityKind::Import {
                    let raw = text(n, s);
                    for (target, alias, start, end) in rust_use_targets(&raw) {
                        let mut target_range = range(n);
                        target_range.start_byte += start as u64;
                        target_range.end_byte = target_range.start_byte + (end - start) as u64;
                        // The declaration can be single-line or formatted.  Its full
                        // line/column span remains the honest normalized location;
                        // byte subranges provide distinct immutable identities for a
                        // grouped `use` without pretending to resolve it.
                        out.imports.push(CodeImportDraft {
                            raw: raw.clone(),
                            target: target.clone(),
                            alias,
                            range: target_range.clone(),
                        });
                        out.relationships.push(CodeRelationshipDraft {
                            kind: CodeRelationshipKind::Imports,
                            source_syntax_path: parent.clone(),
                            raw_target: target,
                            range: target_range,
                        });
                    }
                    out.entities.push(CodeEntityDraft {
                        kind: k,
                        name: None,
                        qualified_name: None,
                        syntax_path: format!("import@{}", n.start_byte()),
                        parent_syntax_path: parent.clone(),
                        range: range(n),
                        signature: Some(raw),
                        visibility: None,
                        is_async: false,
                    });
                } else {
                    let local = name(n, s);
                    let path = format!(
                        "{}:{}@{}",
                        k.as_str(),
                        local.clone().unwrap_or_default(),
                        n.start_byte()
                    );
                    let q = local.as_ref().map(|x| {
                        parent_q
                            .as_ref()
                            .map(|p| format!("{p}::{x}"))
                            .unwrap_or_else(|| x.clone())
                    });
                    let raw = text(n, s);
                    let visibility = raw
                        .trim_start()
                        .starts_with("pub")
                        .then_some("public".into());
                    let async_ = raw.trim_start().starts_with("async ")
                        || raw.trim_start().starts_with("pub async ");
                    out.entities.push(CodeEntityDraft {
                        kind: k,
                        name: local,
                        qualified_name: q.clone(),
                        syntax_path: path.clone(),
                        parent_syntax_path: parent.clone(),
                        range: range(n),
                        signature: (k == CodeEntityKind::Function || k == CodeEntityKind::Method)
                            .then_some(raw.lines().next().unwrap_or("").to_string()),
                        visibility,
                        is_async: async_,
                    });
                    next_parent = Some(path);
                    next_q = q;
                }
            }
            let mut c = n.walk();
            for ch in n.children(&mut c) {
                walk_recursive(ch, s, next_parent.clone(), next_q.clone(), out)
            }
        }

        pub fn parse(source: &str) -> CodeDerivationDraft {
            let mut parser = Parser::new();
            parser
                .set_language(&tree_sitter_rust::LANGUAGE.into())
                .unwrap();
            let tree = parser.parse(source, None).unwrap();
            let mut out = CodeDerivationDraft {
                identity: identity(),
                status: if tree.root_node().has_error() {
                    CodeParseStatus::Recovered
                } else {
                    CodeParseStatus::Complete
                },
                error_count: count_errors_recursive(tree.root_node()),
                entities: vec![],
                imports: vec![],
                relationships: vec![],
            };
            walk_recursive(tree.root_node(), source, None, None, &mut out);
            out
        }
    }

    /// Compares both implementations. The reference recurses, so it runs on a
    /// thread with a large stack; the iterative one is also run on the small test
    /// stack, which is the point.
    fn same(source: &str, what: &str) {
        let new = parse(CodeLanguage::Rust, source);
        let src = source.to_string();
        let old = std::thread::Builder::new()
            .stack_size(512 * 1024 * 1024)
            .spawn(move || reference::parse(&src))
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(new, old, "{what}");
    }

    fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                rust_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    #[test]
    fn the_iterative_traversal_equals_the_recursive_reference_on_real_sources() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        rust_files(&root.join("src"), &mut files);
        rust_files(&root.join("tests"), &mut files);
        assert!(files.len() > 50, "{} files", files.len());
        let mut entities = 0;
        for f in &files {
            let text = std::fs::read_to_string(f).unwrap();
            same(&text, &f.display().to_string());
            entities += parse(CodeLanguage::Rust, &text).entities.len();
            // corrupted variants: cut at ten places (on a character boundary)
            for k in 1..10 {
                let mut cut = text.len() * k / 10;
                while !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                same(&text[..cut], &format!("{} cut at {cut}", f.display()));
            }
        }
        assert!(entities > 3000, "{entities} entities compared");
    }

    #[test]
    fn the_iterative_traversal_equals_the_reference_on_nested_input() {
        let nest =
            |open: &str, close: &str, n: usize| format!("{}{}", open.repeat(n), close.repeat(n));
        same(&format!("fn f() {}", nest("{", "}", 300)), "braces");
        same(&format!("fn f() {{ {} }}", nest("(", ")", 300)), "parens");
        same(
            &format!("fn f() -> u32 {{ {} }}", vec!["a"; 400].join("+")),
            "chain",
        );
        same(
            &format!("{}{}", "mod m { ".repeat(120), "}".repeat(120)),
            "modules",
        );
        same(
            &format!("{}{}", "impl A { fn g() { ".repeat(60), "} }".repeat(60)),
            "impls",
        );
        same(
            &format!("{}{}", "fn h() { ".repeat(150), "}".repeat(150)),
            "nested functions",
        );
        same(
            &"use a::{b as c, d};\nfn x() { y(z()); }\n".repeat(200),
            "many items",
        );
        same(
            "impl Trait for Type<T> where T: Copy { fn m(&self) { self.n(); } }",
            "impl",
        );
        same("", "empty");
        same("fn broken(", "broken");
    }

    #[test]
    fn captures_nested_rust_and_imports() {
        let source="use crate::store::Store;\nmod config { pub struct Config { x: u8 } impl Config { pub async fn load() {} } }";
        let parsed = parse(CodeLanguage::Rust, source);
        assert_eq!(parsed.status, CodeParseStatus::Complete);
        assert!(parsed
            .entities
            .iter()
            .any(|e| e.kind == CodeEntityKind::Struct && e.name.as_deref() == Some("Config")));
        assert!(parsed
            .entities
            .iter()
            .any(|e| e.kind == CodeEntityKind::Method
                && e.qualified_name.as_deref() == Some("config::Config::load")));
        assert_eq!(parsed.imports[0].target, "crate::store::Store");
    }
    #[test]
    fn malformed_rust_is_recovered() {
        assert_eq!(
            parse(CodeLanguage::Rust, "fn broken(").status,
            CodeParseStatus::Recovered
        );
    }

    #[test]
    fn captures_inherent_and_trait_impl_sides_independently() {
        let parsed = parse(
            CodeLanguage::Rust,
            "trait Loadable {} struct Config; impl Config {} impl Loadable for Config {}",
        );
        let sides: Vec<_> = parsed
            .relationships
            .iter()
            .filter(|relationship| {
                matches!(
                    relationship.kind,
                    CodeRelationshipKind::ImplementsTrait | CodeRelationshipKind::ImplementsType
                )
            })
            .map(|relationship| (relationship.kind, relationship.raw_target.as_str()))
            .collect();
        assert_eq!(
            sides,
            vec![
                (CodeRelationshipKind::ImplementsType, "Config"),
                (CodeRelationshipKind::ImplementsTrait, "Loadable"),
                (CodeRelationshipKind::ImplementsType, "Config"),
            ]
        );
    }

    #[test]
    fn splits_aliased_and_grouped_use_paths_into_distinct_observations() {
        let parsed = parse(CodeLanguage::Rust, "use crate::foo::{Bar as Renamed, Baz};");
        assert_eq!(
            parsed
                .imports
                .iter()
                .map(|import| (import.target.as_str(), import.alias.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("crate::foo::Bar", Some("Renamed")),
                ("crate::foo::Baz", None),
            ]
        );
        assert_ne!(
            parsed.relationships[0].range.start_byte,
            parsed.relationships[1].range.start_byte
        );
    }

    #[test]
    fn trait_method_declarations_are_indexed_as_methods() {
        let parsed = parse(
            CodeLanguage::Rust,
            "pub trait Store { fn record_manual_write(&self, d: u8) -> u8; fn with_body(&self) {} }",
        );
        let find = |name: &str| {
            parsed
                .entities
                .iter()
                .find(|e| e.name.as_deref() == Some(name))
                .unwrap_or_else(|| panic!("{name} missing"))
        };
        assert_eq!(find("record_manual_write").kind, CodeEntityKind::Method);
        assert_eq!(
            find("record_manual_write").qualified_name.as_deref(),
            Some("Store::record_manual_write")
        );
        assert!(find("record_manual_write")
            .signature
            .as_deref()
            .unwrap()
            .starts_with("fn record_manual_write"));
        assert_eq!(find("with_body").kind, CodeEntityKind::Method);
    }

    #[test]
    fn trait_impl_methods_are_named_after_the_implementing_type() {
        let parsed = parse(
            CodeLanguage::Rust,
            "trait Store { fn put(&self); } struct SqliteStore; impl Store for SqliteStore { fn put(&self) {} }",
        );
        let names: Vec<_> = parsed
            .entities
            .iter()
            .filter(|e| e.name.as_deref() == Some("put"))
            .map(|e| e.qualified_name.clone().unwrap())
            .collect();
        assert!(names.contains(&"Store::put".to_string()), "{names:?}");
        assert!(names.contains(&"SqliteStore::put".to_string()), "{names:?}");
        assert_eq!(names.len(), 2);
    }
}
