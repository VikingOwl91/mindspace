//! Tree-sitter based, byte-only Rust structure extraction. No repository code
//! is loaded or executed; parser types end at this module boundary.

use crate::core::{
    CodeDerivationDraft, CodeEntityDraft, CodeEntityKind, CodeImportDraft, CodeLanguage,
    CodeParseStatus, CodeParserIdentity, CodeRange, CodeRelationshipDraft, CodeRelationshipKind,
};
use std::path::Path;
use tree_sitter::{Node, Parser};

pub const IDENTITY: (&str, &str, &str, &str) = ("mind-code", "2", "tree-sitter-rust", "0.24.2");
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
    walk(tree.root_node(), source, None, None, &mut out);
    out
}
fn count_errors(n: Node) -> u32 {
    let mut count = u32::from(n.is_error() || n.is_missing());
    let mut c = n.walk();
    for x in n.children(&mut c) {
        count += count_errors(x)
    }
    count
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
fn walk(
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
            walk(child, s, impl_parent.clone(), impl_q.clone(), out);
        }
        return;
    }
    let kind = match n.kind() {
        "mod_item" => Some(CodeEntityKind::Module),
        "struct_item" => Some(CodeEntityKind::Struct),
        "enum_item" => Some(CodeEntityKind::Enum),
        "trait_item" => Some(CodeEntityKind::Trait),
        "type_item" => Some(CodeEntityKind::TypeAlias),
        "function_item" | "function_signature_item" => Some(
            if parent
                .as_deref()
                .is_some_and(|p| p.contains("impl") || p.contains("trait"))
            {
                CodeEntityKind::Method
            } else {
                CodeEntityKind::Function
            },
        ),
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
        walk(ch, s, next_parent.clone(), next_q.clone(), out)
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
