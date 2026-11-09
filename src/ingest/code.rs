//! Tree-sitter based, byte-only Rust structure extraction. No repository code
//! is loaded or executed; parser types end at this module boundary.

use crate::core::{
    CodeDerivationDraft, CodeEntityDraft, CodeEntityKind, CodeImportDraft, CodeLanguage,
    CodeParseStatus, CodeParserIdentity, CodeRange, CodeRelationshipDraft, CodeRelationshipKind,
};
use std::path::Path;
use tree_sitter::{Node, Parser};

pub const IDENTITY: (&str, &str, &str, &str) = ("mind-code", "4", "tree-sitter-rust", "0.24.2");
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
/// Revision of the structural-parse policy. It is part of every skip identity, so a change in
/// what the budgets count is a new revision and re-evaluates skipped files.
pub const POLICY_REVISION: &str = "p1";
/// Fixed allowance, in budget bytes, for every stored record (entity, import, relationship).
const RECORD: u64 = 64;

/// Resource budgets for structural parsing (design: `docs/design-m7.3b-structural-parse-policy.md`).
/// They bound the size and nesting of the structural *output*; they do not bound the memory of the
/// parse tree or the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsePolicy {
    /// Longest source, in bytes, that is parsed (`len > max` is skipped).
    pub max_source_bytes: u64,
    /// Deepest nesting of entities and `impl` blocks (`depth > max` is skipped).
    pub max_entity_depth: u32,
    /// Budget bytes for everything the derivation stores.
    pub max_output_bytes: u64,
}

impl Default for ParsePolicy {
    /// The provisional `p1` defaults: 2 MiB, depth 256, 32 MiB.
    fn default() -> Self {
        Self {
            max_source_bytes: 2 * 1024 * 1024,
            max_entity_depth: 256,
            max_output_bytes: 32 * 1024 * 1024,
        }
    }
}

impl ParsePolicy {
    /// No limits (tests and reference comparisons).
    pub const fn unlimited() -> Self {
        Self {
            max_source_bytes: u64::MAX,
            max_entity_depth: u32::MAX,
            max_output_bytes: u64::MAX,
        }
    }
    /// The skip reasons that this policy can produce, with its current limits.
    pub fn reasons(&self) -> [SkipReason; 3] {
        [
            SkipReason::SourceBytes {
                limit: self.max_source_bytes,
            },
            SkipReason::EntityDepth {
                limit: self.max_entity_depth,
            },
            SkipReason::OutputBytes {
                limit: self.max_output_bytes,
            },
        ]
    }
}

/// Why a file was not parsed structurally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    SourceBytes { limit: u64 },
    EntityDepth { limit: u32 },
    OutputBytes { limit: u64 },
}

impl SkipReason {
    /// `source_bytes>2097152`: reason and limit that applied.
    pub fn label(&self) -> String {
        match self {
            Self::SourceBytes { limit } => format!("source_bytes>{limit}"),
            Self::EntityDepth { limit } => format!("entity_depth>{limit}"),
            Self::OutputBytes { limit } => format!("output_bytes>{limit}"),
        }
    }
}

/// The identity of a skip row: the parser identity with the policy revision and reason appended to
/// the extractor version, e.g. `4+skip(p1:source_bytes>2097152)`. Deterministic, and different
/// from the identity of a real derivation (`identity()`).
pub fn skip_identity(reason: SkipReason) -> CodeParserIdentity {
    let mut id = identity();
    id.extractor_version = format!("{}+skip({POLICY_REVISION}:{})", IDENTITY.1, reason.label());
    id
}

/// The reason text (`p1:source_bytes>2097152`) of a skip identity, `None` for a real derivation.
pub fn skip_reason_of(identity: &CodeParserIdentity) -> Option<String> {
    let v = &identity.extractor_version;
    let start = v.find("+skip(")? + "+skip(".len();
    let end = v[start..].find(')')? + start;
    Some(v[start..end].to_string())
}

/// A skip row: status `failed` (an existing status older binaries decode), no entities.
pub fn skip_derivation(reason: SkipReason) -> CodeDerivationDraft {
    CodeDerivationDraft {
        identity: skip_identity(reason),
        status: CodeParseStatus::Failed,
        error_count: 0,
        entities: vec![],
        imports: vec![],
        relationships: vec![],
    }
}

/// What `parse_with_policy` produced.
#[derive(Debug, Clone)]
pub struct ParseOutcome {
    /// The derivation to store: the parse, or the skip row when `skipped` is set.
    pub derivation: CodeDerivationDraft,
    pub skipped: Option<SkipReason>,
    /// Budget bytes charged by the walk (what a limit of exactly this value still accepts).
    pub accounted_bytes: u64,
    /// The parser could not run (setup error or no tree). Not a property of the content: it must
    /// not be stored as a derivation.
    pub transient: Option<String>,
}

/// Parses without limits (`ParsePolicy::unlimited()`): the behaviour of `mind-code@3`.
pub fn parse(language: CodeLanguage, source: &str) -> CodeDerivationDraft {
    parse_with_policy(language, source, &ParsePolicy::unlimited()).derivation
}

/// Parses under a policy. A source over `max_source_bytes` is not handed to the parser; the
/// depth and output budgets are enforced while the tree is walked, before the allocation they
/// guard; a violation discards the whole structural result.
pub fn parse_with_policy(
    language: CodeLanguage,
    source: &str,
    policy: &ParsePolicy,
) -> ParseOutcome {
    let failed = |transient: Option<String>| ParseOutcome {
        derivation: CodeDerivationDraft {
            identity: identity(),
            status: CodeParseStatus::Failed,
            error_count: 1,
            entities: vec![],
            imports: vec![],
            relationships: vec![],
        },
        skipped: None,
        accounted_bytes: 0,
        transient,
    };
    let skipped = |reason: SkipReason, accounted: u64| ParseOutcome {
        derivation: skip_derivation(reason),
        skipped: Some(reason),
        accounted_bytes: accounted,
        transient: None,
    };
    if language != CodeLanguage::Rust {
        return failed(None);
    }
    if source.len() as u64 > policy.max_source_bytes {
        return skipped(
            SkipReason::SourceBytes {
                limit: policy.max_source_bytes,
            },
            0,
        );
    }
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .is_err()
    {
        return failed(Some("the Rust grammar could not be loaded".into()));
    }
    let Some(tree) = parser.parse(source, None) else {
        return failed(Some("the parser returned no tree".into()));
    };
    let mut out = CodeDerivationDraft {
        identity: identity(),
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
    let mut budget = Budget {
        used: 0,
        limit: policy.max_output_bytes,
        max_depth: policy.max_entity_depth,
    };
    match walk(tree.root_node(), source, &mut out, &mut budget) {
        Ok(()) => ParseOutcome {
            derivation: out,
            skipped: None,
            accounted_bytes: budget.used,
            transient: None,
        },
        // The partial entity list is dropped with `out`.
        Err(reason) => skipped(reason, budget.used),
    }
}

/// The running total of budget bytes. `charge` is called with the size of an allocation before it
/// is made.
struct Budget {
    used: u64,
    limit: u64,
    max_depth: u32,
}

impl Budget {
    fn charge(&mut self, bytes: u64) -> Result<(), SkipReason> {
        self.used = self.used.saturating_add(bytes);
        if self.used > self.limit {
            Err(SkipReason::OutputBytes { limit: self.limit })
        } else {
            Ok(())
        }
    }
    fn enter(&self, depth: u32) -> Result<(), SkipReason> {
        if depth > self.max_depth {
            Err(SkipReason::EntityDepth {
                limit: self.max_depth,
            })
        } else {
            Ok(())
        }
    }
}

fn node_len(n: Node) -> u64 {
    n.end_byte().saturating_sub(n.start_byte()) as u64
}

/// The source text of a node, borrowed (empty where the range is not on a character boundary,
/// like `Node::utf8_text(..).unwrap_or("")`).
fn slice<'a>(n: Node, s: &'a str) -> &'a str {
    s.get(n.start_byte()..n.end_byte()).unwrap_or("")
}

fn digits(mut n: usize) -> u64 {
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
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
#[cfg(test)]
fn text(n: Node, s: &str) -> String {
    n.utf8_text(s.as_bytes()).unwrap_or("").to_string()
}
#[cfg(test)]
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
/// The nearest enclosing *entity-defining* node decides, which is exactly how the traversal derives
/// `parent_syntax_path`: nodes that are not entities (blocks, declaration lists, closures, and
/// the wrappers tree-sitter's error recovery inserts, such as `ERROR` or a MISSING-`extern`
/// `foreign_mod_item`) share their parent's context. So the walk goes up through every such node
/// and stops at the first impl, trait or other entity. In a tree that parsed cleanly the only
/// node between an associated item and its impl or trait is the `declaration_list` of its body,
/// and the result is the same as before; in a recovered tree the answer now agrees with the
/// parent path of the same record. (Until `tests/code_classification.rs` found a counterexample
/// the walk looked through `declaration_list` only; a recovery wrapper in an impl body then gave
/// a record that said "function" and "my parent is an impl" at once.)
fn is_associated_item(n: Node) -> bool {
    let mut cur = n.parent();
    while let Some(p) = cur {
        match p.kind() {
            "impl_item" | "trait_item" => return true,
            // the entity kinds of `visit`: an item below one of these belongs to it, not to an
            // impl further out
            "mod_item"
            | "struct_item"
            | "enum_item"
            | "type_item"
            | "function_item"
            | "function_signature_item"
            | "const_item"
            | "static_item"
            | "field_declaration"
            | "use_declaration" => return false,
            _ => cur = p.parent(),
        }
    }
    false
}
/// The part of the traversal that belongs to one node: records what the node itself contributes
/// and returns the context its children are visited in. Every string that is stored or kept as
/// context is charged to the budget before it is allocated, and node text is borrowed rather than
/// copied wherever only a prefix or a first line is needed. (`mind-code@3` did this recursively,
/// which overflowed the stack on deeply nested input; the order of visits and every value of an
/// accepted input are unchanged.)
fn visit(
    n: Node,
    s: &str,
    ctx: &Ctx,
    out: &mut CodeDerivationDraft,
    b: &mut Budget,
) -> Result<Ctx, SkipReason> {
    let parent: Option<&str> = ctx.parent.as_deref();
    let parent_q: Option<&str> = ctx.q.as_deref();
    let parent_len = parent.map_or(0, |p| p.len() as u64);
    if n.kind() == "call_expression" {
        let function = n.child_by_field_name("function");
        // the callee text is copied: charge its length first (a long method chain copies the
        // whole receiver chain for every call)
        b.charge(RECORD + function.map_or(0, node_len) + parent_len)?;
        let raw_target = function
            .map(|x| slice(x, s).to_string())
            .unwrap_or_default();
        out.relationships.push(CodeRelationshipDraft {
            kind: CodeRelationshipKind::Calls,
            source_syntax_path: parent.map(str::to_string),
            raw_target,
            range: range(n),
        });
    }
    if n.kind() == "impl_item" {
        let level = ctx.depth.saturating_add(1);
        b.enter(level)?;
        let trait_node = n.child_by_field_name("trait");
        let type_node = n.child_by_field_name("type");
        if let Some(t) = trait_node {
            b.charge(RECORD + node_len(t) + parent_len)?;
        }
        if let Some(t) = type_node {
            b.charge(RECORD + node_len(t) + parent_len)?;
        }
        if let Some(t) = trait_node {
            out.relationships.push(CodeRelationshipDraft {
                kind: CodeRelationshipKind::ImplementsTrait,
                source_syntax_path: parent.map(str::to_string),
                raw_target: slice(t, s).to_string(),
                range: range(n),
            });
        }
        if let Some(t) = type_node {
            out.relationships.push(CodeRelationshipDraft {
                kind: CodeRelationshipKind::ImplementsType,
                source_syntax_path: parent.map(str::to_string),
                raw_target: slice(t, s).to_string(),
                range: range(n),
            });
        }
        // The `type` field is the implementing type; for `impl Trait for Type` the first type
        // identifier would be the trait.
        let type_name: &str = type_node.map_or("impl", |t| slice(t, s));
        let start = n.start_byte();
        b.charge(5 + type_name.len() as u64 + 1 + digits(start))?;
        b.charge(parent_q.map_or(0, |p| p.len() as u64 + 2) + type_name.len() as u64)?;
        let impl_parent = Some(format!("impl:{type_name}@{start}"));
        let impl_q = Some(
            parent_q
                .map(|p| format!("{p}::{type_name}"))
                .unwrap_or_else(|| type_name.to_string()),
        );
        return Ok(Ctx::of(impl_parent, impl_q, level));
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
    let Some(k) = kind else {
        // Children of a node that is not an entity share its parent's context.
        return Ok(ctx.clone());
    };
    let level = ctx.depth.saturating_add(1);
    b.enter(level)?;
    let raw: &str = slice(n, s);
    let start = n.start_byte();
    if k == CodeEntityKind::Import {
        // entity record + `signature` (a copy of the declaration) + `syntax_path`
        b.charge(RECORD + raw.len() as u64 + 7 + digits(start) + parent_len)?;
        // per target: an import (raw copy, target, alias) and a relationship (target, parent)
        let per_target = raw.len() as u64 + 2 * RECORD + parent_len;
        let targets = rust_use_targets_with(raw, &mut |target_len, alias_len| {
            b.charge(per_target + 2 * target_len + alias_len)
        })?;
        for (target, alias, tstart, tend) in targets {
            let mut target_range = range(n);
            target_range.start_byte += tstart as u64;
            target_range.end_byte = target_range.start_byte + (tend - tstart) as u64;
            // The declaration can be single-line or formatted.  Its full line/column span
            // remains the honest normalized location; byte subranges provide distinct immutable
            // identities for a grouped `use` without pretending to resolve it.
            out.imports.push(CodeImportDraft {
                raw: raw.to_string(),
                target: target.clone(),
                alias,
                range: target_range.clone(),
            });
            out.relationships.push(CodeRelationshipDraft {
                kind: CodeRelationshipKind::Imports,
                source_syntax_path: parent.map(str::to_string),
                raw_target: target,
                range: target_range,
            });
        }
        out.entities.push(CodeEntityDraft {
            kind: k,
            name: None,
            qualified_name: None,
            syntax_path: format!("import@{start}"),
            parent_syntax_path: parent.map(str::to_string),
            range: range(n),
            signature: Some(raw.to_string()),
            visibility: None,
            is_async: false,
        });
        // an import adds no level of its own
        return Ok(ctx.clone());
    }
    // name (copied), syntax path (stored + kept as context), qualified name (stored + kept as
    // context), parent path (stored), signature line (stored), record allowance
    let name_node = n.child_by_field_name("name");
    let name_len = name_node.map_or(0, node_len);
    let local: Option<&str> = name_node.map(|x| slice(x, s)).filter(|x| !x.is_empty());
    let local_len = local.map_or(0, |l| l.len() as u64);
    let path_len = k.as_str().len() as u64 + 1 + local_len + 1 + digits(start);
    let q_len = local.map_or(0, |l| {
        parent_q.map_or(0, |p| p.len() as u64 + 2) + l.len() as u64
    });
    let signature: Option<&str> = (k == CodeEntityKind::Function || k == CodeEntityKind::Method)
        .then(|| raw.lines().next().unwrap_or(""));
    b.charge(
        RECORD
            + name_len
            + 2 * path_len
            + 2 * q_len
            + parent_len
            + signature.map_or(0, |l| l.len() as u64),
    )?;
    let path = format!("{}:{}@{}", k.as_str(), local.unwrap_or_default(), start);
    let q = local.map(|x| {
        parent_q
            .map(|p| format!("{p}::{x}"))
            .unwrap_or_else(|| x.to_string())
    });
    let visibility = raw
        .trim_start()
        .starts_with("pub")
        .then_some("public".into());
    let async_ =
        raw.trim_start().starts_with("async ") || raw.trim_start().starts_with("pub async ");
    out.entities.push(CodeEntityDraft {
        kind: k,
        name: local.map(str::to_string),
        qualified_name: q.clone(),
        syntax_path: path.clone(),
        parent_syntax_path: parent.map(str::to_string),
        range: range(n),
        signature: signature.map(str::to_string),
        visibility,
        is_async: async_,
    });
    Ok(Ctx::of(Some(path), q, level))
}

/// What a node's descendants inherit: the syntax path and qualified name of the closest enclosing
/// entity (shared, not copied, between siblings) and how many entity/`impl` levels enclose them.
#[derive(Clone)]
struct Ctx {
    parent: Option<std::rc::Rc<str>>,
    q: Option<std::rc::Rc<str>>,
    depth: u32,
}

impl Ctx {
    fn none() -> Self {
        Self {
            parent: None,
            q: None,
            depth: 0,
        }
    }
    fn of(parent: Option<String>, q: Option<String>, depth: u32) -> Self {
        Self {
            parent: parent.map(Into::into),
            q: q.map(Into::into),
            depth,
        }
    }
}

/// Pre-order traversal of every node (named and anonymous) with an explicit, heap-allocated stack
/// of contexts: the depth is limited by memory, not by the thread's stack. Stops at the first
/// budget violation.
fn walk(
    root: Node,
    s: &str,
    out: &mut CodeDerivationDraft,
    b: &mut Budget,
) -> Result<(), SkipReason> {
    let mut cursor = root.walk();
    // stack[d] is the context the children of the node at depth d inherit
    let mut stack: Vec<Ctx> = vec![visit(root, s, &Ctx::none(), out, b)?];
    loop {
        if cursor.goto_first_child() {
            let inherited = stack.last().cloned().unwrap_or_else(Ctx::none);
            let next = visit(cursor.node(), s, &inherited, out, b)?;
            stack.push(next);
            continue;
        }
        loop {
            stack.pop();
            if cursor.goto_next_sibling() {
                let inherited = stack.last().cloned().unwrap_or_else(Ctx::none);
                let next = visit(cursor.node(), s, &inherited, out, b)?;
                stack.push(next);
                break;
            }
            if !cursor.goto_parent() {
                return Ok(());
            }
        }
    }
}

/// Normalizes only the literal Rust `use_declaration` text already selected by
/// Tree-sitter.  This is deliberately not a source parser: grammar selection
/// and declaration boundaries come from Tree-sitter; this just splits its
/// deterministic path-list surface into independently storable facts.
type UseTarget = (String, Option<String>, usize, usize);

/// The unlimited form (`mind-code@3`), kept for the reference comparison in the tests.
#[allow(dead_code)]
fn rust_use_targets(raw: &str) -> Vec<UseTarget> {
    rust_use_targets_with(raw, &mut |_, _| Ok(())).unwrap_or_default()
}

/// Splits a `use` declaration into its targets. `charge(target_len, alias_len)` is called before
/// each target is allocated; its error stops the split.
fn rust_use_targets_with(
    raw: &str,
    charge: &mut dyn FnMut(u64, u64) -> Result<(), SkipReason>,
) -> Result<Vec<UseTarget>, SkipReason> {
    let declaration = raw.trim();
    let Some(body) = declaration
        .strip_prefix("use")
        .map(str::trim)
        .and_then(|body| body.strip_suffix(';'))
        .map(str::trim)
    else {
        return Ok(vec![]);
    };
    let base_offset = raw.find(body).unwrap_or(0);
    if let Some((prefix, rest)) = body.split_once("::{") {
        if let Some(items) = rest.strip_suffix('}') {
            let prefix = prefix.trim_end_matches("::").trim();
            let items_offset = body.find('{').unwrap_or(0) + 1;
            let mut offset = 0;
            let mut targets = Vec::new();
            for raw_item in items.split(',') {
                let leading = raw_item.len() - raw_item.trim_start().len();
                let item_offset = base_offset + items_offset + offset + leading;
                offset += raw_item.len() + 1;
                let item = raw_item.trim();
                let (item_path, alias) = item
                    .rsplit_once(" as ")
                    .map_or((item, None), |(path, alias)| {
                        (path.trim(), Some(alias.trim()))
                    });
                charge(
                    (prefix.len() + 2 + item_path.len()) as u64,
                    alias.map_or(0, |a| a.len() as u64),
                )?;
                targets.push((
                    format!("{prefix}::{item_path}"),
                    alias.map(str::to_string),
                    item_offset,
                    item_offset + item_path.len(),
                ));
            }
            return Ok(targets);
        }
    }
    let target = body.trim();
    let (target, alias) = target
        .rsplit_once(" as ")
        .map_or((target, None), |(path, alias)| {
            (path.trim(), Some(alias.trim()))
        });
    if target.is_empty() {
        return Ok(vec![]);
    }
    charge(target.len() as u64, alias.map_or(0, |a| a.len() as u64))?;
    Ok(vec![(
        target.to_string(),
        alias.map(str::to_string),
        base_offset,
        base_offset + target.len(),
    )])
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
        // under the default policy every input of this corpus is accepted and equal
        let policed = parse_with_policy(CodeLanguage::Rust, source, &ParsePolicy::default());
        assert!(
            policed.skipped.is_none(),
            "{what}: skipped {:?}",
            policed.skipped
        );
        assert_eq!(policed.derivation, old, "{what}: default policy");
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

    fn with(source_bytes: u64, depth: u32, output: u64) -> ParsePolicy {
        ParsePolicy {
            max_source_bytes: source_bytes,
            max_entity_depth: depth,
            max_output_bytes: output,
        }
    }

    #[test]
    fn the_default_policy_is_p1() {
        let p = ParsePolicy::default();
        assert_eq!(p.max_source_bytes, 2_097_152);
        assert_eq!(p.max_entity_depth, 256);
        assert_eq!(p.max_output_bytes, 33_554_432);
    }

    #[test]
    fn a_source_exactly_at_the_limit_parses_and_one_byte_more_is_skipped() {
        let base = "fn f() {}";
        for limit in [base.len() as u64 + 40, 2_097_152] {
            let at = format!("{base}{}", " ".repeat(limit as usize - base.len()));
            assert_eq!(at.len() as u64, limit);
            let ok = parse_with_policy(CodeLanguage::Rust, &at, &with(limit, 256, u64::MAX));
            assert!(ok.skipped.is_none() && !ok.derivation.entities.is_empty());
            let over = format!("{at} ");
            let o = parse_with_policy(CodeLanguage::Rust, &over, &with(limit, 256, u64::MAX));
            assert_eq!(o.skipped, Some(SkipReason::SourceBytes { limit }));
            assert!(o.derivation.entities.is_empty());
        }
    }

    #[test]
    fn nesting_is_limited_at_the_configured_depth() {
        let nest = |d: usize| format!("{}{}", "mod m { ".repeat(d), "}".repeat(d));
        for (d, skipped) in [(255, false), (256, false), (257, true), (20_000, true)] {
            let o = parse_with_policy(CodeLanguage::Rust, &nest(d), &with(u64::MAX, 256, u64::MAX));
            assert_eq!(o.skipped.is_some(), skipped, "depth {d}");
            if skipped {
                assert_eq!(o.skipped, Some(SkipReason::EntityDepth { limit: 256 }));
                assert!(o.derivation.entities.is_empty(), "no partial entities");
            } else {
                assert_eq!(o.derivation.entities.len(), d);
            }
        }
        // impl blocks and functions are levels as well; an import is one level but adds none
        let o = parse_with_policy(
            CodeLanguage::Rust,
            "impl A { fn g() { fn h() {} } }",
            &with(u64::MAX, 3, u64::MAX),
        );
        assert!(o.skipped.is_none(), "impl, g, h are 3 levels");
        let o = parse_with_policy(
            CodeLanguage::Rust,
            "impl A { fn g() { fn h() {} } }",
            &with(u64::MAX, 2, u64::MAX),
        );
        assert_eq!(o.skipped, Some(SkipReason::EntityDepth { limit: 2 }));
        let o = parse_with_policy(
            CodeLanguage::Rust,
            "mod a { use x::y; }",
            &with(u64::MAX, 2, u64::MAX),
        );
        assert!(o.skipped.is_none());
        let o = parse_with_policy(
            CodeLanguage::Rust,
            "mod a { use x::y; }",
            &with(u64::MAX, 1, u64::MAX),
        );
        assert!(o.skipped.is_some(), "the import is at level 2");
    }

    #[test]
    fn the_output_budget_is_exact_at_the_boundary_and_discards_everything() {
        let source = "use a::{b, c as d};\nmod m { pub struct S { x: u8 } impl S { pub fn f(&self) { g(h()); } } }";
        let full = parse_with_policy(CodeLanguage::Rust, source, &ParsePolicy::unlimited());
        let used = full.accounted_bytes;
        assert!(used > 0);
        let exact = parse_with_policy(CodeLanguage::Rust, source, &with(u64::MAX, 256, used));
        assert!(exact.skipped.is_none());
        assert_eq!(exact.derivation, full.derivation);
        let short = parse_with_policy(CodeLanguage::Rust, source, &with(u64::MAX, 256, used - 1));
        assert_eq!(
            short.skipped,
            Some(SkipReason::OutputBytes { limit: used - 1 })
        );
        let d = short.derivation;
        assert!(d.entities.is_empty() && d.imports.is_empty() && d.relationships.is_empty());
        assert_eq!(d.status, CodeParseStatus::Failed);
        assert_eq!(d.error_count, 0);
    }

    #[test]
    fn long_method_chains_and_wide_callees_are_charged_by_callee_length() {
        let chain = |n: usize| format!("fn f() {{ a{} }}", ".b()".repeat(n));
        let p = ParsePolicy::default();
        assert!(parse_with_policy(CodeLanguage::Rust, &chain(1_000), &p)
            .skipped
            .is_none());
        // each call copies its whole receiver chain: 10,000 calls would retain about 200 MiB
        let o = parse_with_policy(CodeLanguage::Rust, &chain(10_000), &p);
        assert_eq!(
            o.skipped,
            Some(SkipReason::OutputBytes {
                limit: p.max_output_bytes
            })
        );
        assert!(o.derivation.relationships.is_empty());
        // wide callee: a long field path called once is one copy, accepted
        let wide = format!("fn f() {{ {}() }}", vec!["x"; 50_000].join("."));
        assert!(parse_with_policy(CodeLanguage::Rust, &wide, &p)
            .skipped
            .is_none());
    }

    #[test]
    fn large_identifiers_and_import_lists_are_charged_before_they_are_copied() {
        let big = "n".repeat(1 << 20);
        let src = format!("mod a {{ mod b {{ fn {big}() {{}} }} }}");
        let o = parse_with_policy(CodeLanguage::Rust, &src, &ParsePolicy::default());
        assert!(o.skipped.is_none());
        let f = o
            .derivation
            .entities
            .iter()
            .find(|e| e.name.as_deref() == Some(big.as_str()))
            .unwrap();
        assert!(f.qualified_name.as_deref().unwrap().len() > big.len());
        let tight = parse_with_policy(CodeLanguage::Rust, &src, &with(u64::MAX, 256, 3 << 20));
        assert!(matches!(
            tight.skipped,
            Some(SkipReason::OutputBytes { .. })
        ));
        // a use list repeats its raw text for every target: quadratic output
        let items: Vec<String> = (0..10_000).map(|i| format!("item_number_{i}")).collect();
        let list = format!("use a::{{{}}};", items.join(", "));
        let o = parse_with_policy(CodeLanguage::Rust, &list, &ParsePolicy::default());
        assert!(
            matches!(o.skipped, Some(SkipReason::OutputBytes { .. })),
            "{:?}",
            o.skipped
        );
        let small = format!("use a::{{{}}};", items[..50].join(", "));
        assert_eq!(
            parse_with_policy(CodeLanguage::Rust, &small, &ParsePolicy::default())
                .derivation
                .imports
                .len(),
            50
        );
    }

    #[test]
    fn skip_identities_are_deterministic_and_distinct_from_real_ones() {
        let r = SkipReason::SourceBytes { limit: 2_097_152 };
        assert_eq!(
            skip_identity(r).extractor_version,
            "4+skip(p1:source_bytes>2097152)"
        );
        assert_eq!(skip_identity(r), skip_identity(r));
        assert_ne!(skip_identity(r), identity());
        assert_ne!(
            skip_identity(r),
            skip_identity(SkipReason::SourceBytes { limit: 4_194_304 })
        );
        assert_eq!(
            skip_reason_of(&skip_identity(r)).as_deref(),
            Some("p1:source_bytes>2097152")
        );
        assert_eq!(skip_reason_of(&identity()), None);
        let d = skip_derivation(SkipReason::EntityDepth { limit: 256 });
        assert_eq!((d.status, d.error_count), (CodeParseStatus::Failed, 0));
        assert_eq!(
            skip_identity(SkipReason::OutputBytes { limit: 7 }).extractor_version,
            "4+skip(p1:output_bytes>7)"
        );
    }
}
