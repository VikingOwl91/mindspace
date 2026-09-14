//! Tree-sitter based, byte-only Rust structure extraction. No repository code
//! is loaded or executed; parser types end at this module boundary.

use crate::core::{
    CodeDerivationDraft, CodeEntityDraft, CodeEntityKind, CodeImportDraft, CodeLanguage,
    CodeParseStatus, CodeParserIdentity, CodeRange,
};
use std::path::Path;
use tree_sitter::{Node, Parser};

pub const IDENTITY: (&str, &str, &str, &str) = ("mind-code", "1", "tree-sitter-rust", "0.24.2");
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
        };
    }
    let Some(tree) = parser.parse(source, None) else {
        return CodeDerivationDraft {
            identity,
            status: CodeParseStatus::Failed,
            error_count: 1,
            entities: vec![],
            imports: vec![],
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
    if n.kind() == "impl_item" {
        let mut cursor = n.walk();
        let type_name = n
            .children(&mut cursor)
            .find(|child| matches!(child.kind(), "type_identifier" | "generic_type"))
            .map(|child| text(child, s))
            .unwrap_or_else(|| "impl".to_string());
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
        "function_item" => Some(
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
            let target = raw
                .trim()
                .trim_start_matches("use")
                .trim()
                .trim_end_matches(';')
                .trim()
                .to_string();
            out.imports.push(CodeImportDraft {
                raw: raw.clone(),
                target: target.clone(),
                alias: None,
                range: range(n),
            });
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
}
