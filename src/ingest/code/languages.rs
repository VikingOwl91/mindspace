//! Declarations in Go and the ECMAScript-family grammars. A Svelte script
//! uses the same visitor over a parser range in the original component.
use super::*;
use tree_sitter::{Range, Tree};

pub(super) fn tree(
    language: CodeLanguage,
    source: &str,
    included: Option<Range>,
) -> Result<Tree, String> {
    let grammar = match language {
        CodeLanguage::Rust => tree_sitter_rust::LANGUAGE,
        CodeLanguage::Go => tree_sitter_go::LANGUAGE,
        CodeLanguage::JavaScript => tree_sitter_javascript::LANGUAGE,
        CodeLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        CodeLanguage::Tsx => tree_sitter_typescript::LANGUAGE_TSX,
        CodeLanguage::Svelte => tree_sitter_svelte_ng::LANGUAGE,
    };
    let mut parser = Parser::new();
    parser
        .set_language(&grammar.into())
        .map_err(|e| format!("{} grammar: {e}", language.as_str()))?;
    if let Some(range) = included {
        parser
            .set_included_ranges(&[range])
            .map_err(|e| e.to_string())?;
    }
    parser
        .parse(source, None)
        .ok_or_else(|| "the parser returned no tree".into())
}

fn named_child<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    let child = node.named_children(&mut cursor).find(|c| c.kind() == kind);
    child
}

/// Only inline top-level component scripts with a static JS/TS language.
/// A Svelte attribute can contain an expression even inside quotes.
fn script_language(tag: Node, source: &str) -> Option<CodeLanguage> {
    let mut language = CodeLanguage::JavaScript;
    let mut cursor = tag.walk();
    for attr in tag
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "attribute")
    {
        let name = slice(named_child(attr, "attribute_name")?, source);
        if name.eq_ignore_ascii_case("src") {
            return None;
        }
        if !name.eq_ignore_ascii_case("lang") && !name.eq_ignore_ascii_case("type") {
            continue;
        }
        let value = named_child(attr, "attribute_value").or_else(|| {
            named_child(attr, "quoted_attribute_value")
                .and_then(|q| named_child(q, "attribute_value"))
        })?;
        let value = slice(value, source);
        if name.eq_ignore_ascii_case("type") {
            if !matches!(
                value,
                "module" | "application/javascript" | "text/javascript"
            ) {
                return None;
            }
        } else {
            language = match value {
                "ts" | "typescript" => CodeLanguage::TypeScript,
                "js" | "javascript" => CodeLanguage::JavaScript,
                _ => return None,
            };
        }
    }
    Some(language)
}

pub(super) fn svelte_scripts(
    root: Node,
    source: &str,
    out: &mut CodeDerivationDraft,
    budget: &mut Budget,
) -> Result<Result<(), SkipReason>, String> {
    let mut cursor = root.walk();
    for element in root
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "script_element")
    {
        let Some(lang) =
            named_child(element, "start_tag").and_then(|tag| script_language(tag, source))
        else {
            continue;
        };
        let Some(raw) = named_child(element, "raw_text") else {
            continue;
        };
        let script = tree(lang, source, Some(raw.range()))?;
        if script.root_node().has_error() {
            out.error_count = out
                .error_count
                .saturating_add(count_errors(script.root_node()));
            out.status = CodeParseStatus::Recovered;
        }
        if let Err(reason) = walk(script.root_node(), source, out, budget, lang) {
            return Ok(Err(reason));
        }
    }
    Ok(Ok(()))
}

/// Go receiver names come from the receiver parameter's type, independent
/// of the local receiver variable and of pointer / type parameter syntax.
fn receiver(n: Node) -> Option<Node> {
    let param = named_child(n.child_by_field_name("receiver")?, "parameter_declaration")?;
    let mut ty = param.child_by_field_name("type")?;
    loop {
        ty = match ty.kind() {
            "type_identifier" => return Some(ty),
            "pointer_type" => ty.named_child(0)?,
            "generic_type" => ty.child_by_field_name("type")?,
            _ => return None,
        };
    }
}

fn declaration(language: CodeLanguage, n: Node) -> Option<CodeEntityKind> {
    use CodeEntityKind as K;
    if language == CodeLanguage::Go {
        return Some(match n.kind() {
            "function_declaration" => K::Function,
            "method_declaration" | "method_elem" => K::Method,
            "type_alias" => K::TypeAlias,
            "type_spec" => match n.child_by_field_name("type")?.kind() {
                "struct_type" => K::Struct,
                "interface_type" => K::Interface,
                _ => K::TypeDefinition,
            },
            "var_spec" => K::Variable,
            "const_spec" => K::Constant,
            "field_declaration" => K::Field,
            _ => return None,
        });
    }
    if matches!(n.kind(), "property_signature" | "method_signature")
        && n.parent().is_some_and(|p| p.kind() == "object_type")
        && !n
            .parent()?
            .parent()
            .is_some_and(|p| matches!(p.kind(), "type_alias_declaration" | "interface_declaration"))
    {
        // Members of inline types (e.g. a parameter annotation) are not members of the enclosing function.
        return None;
    }
    Some(match n.kind() {
        "function_declaration" | "generator_function_declaration" | "function_signature" => {
            K::Function
        }
        "class_declaration" | "abstract_class_declaration" => K::Class,
        "interface_declaration" => K::Interface,
        "enum_declaration" => K::Enum,
        "internal_module" => K::Module,
        "type_alias_declaration" => K::TypeAlias,
        "method_definition" | "method_signature" | "abstract_method_signature" => K::Method,
        "public_field_definition" | "field_definition" | "property_signature" => K::Field,
        "variable_declarator" => {
            if n.child_by_field_name("name")?.kind() != "identifier" {
                return None;
            }
            match n.child_by_field_name("value").map(|v| v.kind()) {
                Some("arrow_function" | "function_expression" | "generator_function") => {
                    K::Function
                }
                Some("class") => K::Class,
                _ if n
                    .parent()?
                    .child_by_field_name("kind")
                    .is_some_and(|k| k.kind() == "const") =>
                {
                    K::Constant
                }
                _ => K::Variable,
            }
        }
        _ => return None,
    })
}

pub(super) fn visit(
    language: CodeLanguage,
    node: Node,
    source: &str,
    ctx: &Ctx,
    out: &mut CodeDerivationDraft,
    budget: &mut Budget,
) -> Result<Ctx, SkipReason> {
    let Some(kind) = declaration(language, node) else {
        return Ok(ctx.clone());
    };
    let owner = receiver(node)
        .map(|r| slice(r, source))
        .or(ctx.q.as_deref());
    let level = ctx.depth.saturating_add(1);
    let mut next = ctx.clone();
    let mut cursor = node.walk();
    // Go fields, constants and variables can declare several names in one spec.
    // JS field_definition uses 'property' where the TS grammar uses 'name'.
    let field = if node.kind() == "field_definition" {
        "property"
    } else {
        "name"
    };
    for name in node
        .children_by_field_name(field, &mut cursor)
        .filter(|n| n.is_named() && !n.is_missing() && !n.is_error())
    {
        let local = slice(name, source);
        if local.is_empty() || (language == CodeLanguage::Go && local == "_") {
            continue;
        }
        budget.enter(level)?;
        let callable = matches!(kind, CodeEntityKind::Function | CodeEntityKind::Method);
        let function = if node.kind() == "variable_declarator" {
            node.child_by_field_name("value").unwrap_or(node)
        } else {
            node
        };
        let signature = callable.then(|| {
            let end = function
                .child_by_field_name("body")
                .map_or(node.end_byte(), |b| b.start_byte());
            source
                .get(node.start_byte()..end)
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("")
                .trim_end()
        });
        let is_async = callable
            && function
                .children(&mut function.walk())
                .any(|c| c.kind() == "async");
        let start = name.start_byte();
        let path_len = kind.as_str().len() as u64 + 2 + local.len() as u64 + digits(start);
        let qualified_len = local.len() + owner.map_or(0, |p| p.len() + 1);
        budget.charge(
            RECORD
                + local.len() as u64
                + 2 * path_len
                + 2 * qualified_len as u64
                + ctx.parent.as_ref().map_or(0, |p| p.len() as u64)
                + signature.map_or(0, |s| s.len() as u64),
        )?;
        let path = format!("{}:{}@{}", kind.as_str(), local, start);
        let qualified = owner.map_or_else(|| local.to_owned(), |p| format!("{p}.{local}"));
        out.entities.push(CodeEntityDraft {
            kind,
            name: Some(local.to_owned()),
            qualified_name: Some(qualified.clone()),
            syntax_path: path.clone(),
            parent_syntax_path: ctx.parent.as_deref().map(str::to_owned),
            range: range(node),
            signature: signature.map(str::to_owned),
            visibility: None,
            is_async,
        });
        // Parallel names in a Go spec do not enclose each other or own its type.
        if !matches!(node.kind(), "field_declaration" | "const_spec" | "var_spec") {
            next = Ctx::of(Some(path), Some(qualified), level);
        }
    }
    Ok(next)
}
