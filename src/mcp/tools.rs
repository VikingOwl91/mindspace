use serde_json::{json, Value};
use std::str::FromStr;

use crate::core::{normalize_subject, Error, ManualMemoryDraft, Result, SearchQuery, Ts, Uuid};
use crate::resolve_memory_id;
use crate::retrieval;
use crate::retrieval::semantic::SemanticCache;
use crate::storage::Store;

use super::repo_tools::{self, McpConfig};

pub fn definitions() -> Vec<Value> {
    vec![
        tool(
            "memory_create",
            "Create an explicit manual memory with honest manual provenance. Repeating the same subject and content is idempotent; new spaces are added to the existing active memory.",
            json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string", "minLength": 1 },
                    "subject": { "type": "string", "minLength": 1, "description": "Defaults to the first non-empty line of content." },
                    "spaces": { "type": "array", "items": { "type": "string", "minLength": 1 }, "minItems": 1 }
                },
                "required": ["content", "spaces"],
                "additionalProperties": false
            }),
        ),
        tool(
            "memory_update",
            "Supersede an active manually created memory with a new immutable assertion and memory episode. Document-derived memories are rejected. Unchanged content and subject is a no-op except for adding missing spaces.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "minLength": 4 },
                    "content": { "type": "string", "minLength": 1 },
                    "subject": { "type": "string", "minLength": 1, "description": "Defaults to the target's current subject." },
                    "spaces": { "type": "array", "items": { "type": "string", "minLength": 1 }, "minItems": 1, "description": "Defaults to the target's current spaces." }
                },
                "required": ["id", "content"],
                "additionalProperties": false
            }),
        ),
        tool(
            "memory_retire",
            "Retire an active memory without deleting its evidence or history.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "minLength": 4 },
                    "reason": { "type": "string" }
                },
                "required": ["id"],
                "additionalProperties": false
            }),
        ),
        tool(
            "memory_get",
            "Return a memory, its assertion, evidence, source, spaces, and lifecycle history.",
            json!({
                "type": "object",
                "properties": { "id": { "type": "string", "minLength": 4 } },
                "required": ["id"],
                "additionalProperties": false
            }),
        ),
        tool(
            "memory_search",
            "Search memories. Omit mode for legacy lexical results; select lexical, semantic, hybrid, or auto for a mode-aware outcome. Semantic modes use only an installed and indexed local model. Optional spaces use directed space traversal; at replaces current-state filtering with the transaction-time view.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "minLength": 1 },
                    "spaces": { "type": "array", "items": { "type": "string", "minLength": 1 }, "minItems": 1 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
                    "at": { "type": "string", "format": "date-time" },
                    "include_superseded": { "type": "boolean", "default": false },
                    "include_retired": { "type": "boolean", "default": false },
                    "mode": {
                        "type": "string",
                        "enum": ["lexical", "semantic", "hybrid", "auto"],
                        "description": "Optional retrieval mode. Omit to preserve the legacy response and lexical search."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        ),
        tool(
            "code_search",
            "Find definitions (functions, methods, structs, traits, ...) in ingested source code by name, qualified name, signature, or path. Every whitespace-separated term must match. Results are locations with line ranges, not memories; they carry no confidence. By default only definitions in a current view are returned (HEAD and worktrees for git sources, the latest version of a present file); all_versions includes stale ones, marked current=false.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "minLength": 1 },
                    "kinds": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["module","struct","enum","trait","type_alias","function","method","constant","static","field","import"] }
                    },
                    "spaces": { "type": "array", "items": { "type": "string", "minLength": 1 }, "minItems": 1, "description": "Only code attached to these spaces or spaces they import." },
                    "repository": { "type": "string", "minLength": 1, "description": "owner/name slug, repository path substring, or id prefix." },
                    "path": { "type": "string", "minLength": 1, "description": "Path prefix, repository-relative for git sources." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
                    "all_versions": { "type": "boolean", "default": false }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        ),
        tool(
            "repo_outline",
            "Outline a directory or git repository without ingesting it: file counts, bytes, ingestable files, and file kinds per directory down to `depth`. Git sources are read at HEAD. Use it first to choose which directories to ingest. Only absolute paths inside the server's allowed roots (read-only or read-write) are accepted; with no allowed roots the tool is disabled.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "minLength": 1, "description": "Absolute path of a directory or git repository root." },
                    "depth": { "type": "integer", "minimum": 0, "maximum": 4, "default": 2 }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        ),
        tool(
            "repo_ingest",
            "Ingest a directory or git repository into existing spaces. Pass `paths` (repository-relative directories or files, from repo_outline) to ingest only those; an unscoped ingest of a very large tree is refused. Git: HEAD and live worktrees are interpreted by default, tags always become releases, and `branches`/`remotes`/`tags` add more ref tips (`history` interprets every tip). It writes to the database and, for git, a repository identity marker in the repo's .git directory. Only absolute paths inside a read-write allowed root are accepted; read-only roots can be outlined but not ingested. A deadline (`--ingest-timeout`) applies to the preparation only: it stops an ingest that is still preparing without changing the database, but the database write, once started, runs to its end. The result reports `elapsed_ms`, `limit_ms` and `over_limit` (true: the call finished after the limit, the ingest is complete). Use `paths` to keep it small.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "minLength": 1, "description": "Absolute path of a directory or git repository root." },
                    "spaces": { "type": "array", "items": { "type": "string", "minLength": 1 }, "minItems": 1 },
                    "paths": { "type": "array", "items": { "type": "string", "minLength": 1 }, "description": "Repository-relative paths to ingest." },
                    "history": { "type": "boolean", "default": false },
                    "branches": { "type": "boolean", "default": false },
                    "remotes": { "type": "boolean", "default": false },
                    "tags": { "type": "boolean", "default": false }
                },
                "required": ["path", "spaces"],
                "additionalProperties": false
            }),
        ),
        tool(
            "repo_request_access",
            "Ask the human for access to a directory. This only RECORDS a request and returns a request id: it never grants anything, and the allowed roots of this server do not change. The human decides outside this conversation (`mind grant approve <id>`) and the server must be restarted before the directory is usable. Do not retry in a loop and do not try to approve the request yourself.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "minLength": 1, "description": "Absolute path of the directory you would like to use." },
                    "access": { "type": "string", "enum": ["ro", "rw"], "default": "ro", "description": "ro: outline only; rw: also ingest. Ask for the least you need." },
                    "reason": { "type": "string", "description": "Short explanation shown to the human (at most 300 characters)." }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        ),
        tool(
            "memory_history",
            "List all memory episodes for a normalized subject, oldest first.",
            json!({
                "type": "object",
                "properties": { "subject": { "type": "string", "minLength": 1 } },
                "required": ["subject"],
                "additionalProperties": false
            }),
        ),
        tool(
            "memory_conflicts",
            "List active memories with the same normalized subject in the same space.",
            empty_object_schema(),
        ),
        tool(
            "memory_add_spaces",
            "Add existing spaces to an active memory without duplicating it.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "minLength": 4 },
                    "spaces": { "type": "array", "items": { "type": "string", "minLength": 1 }, "minItems": 1 }
                },
                "required": ["id", "spaces"],
                "additionalProperties": false
            }),
        ),
        tool(
            "space_create",
            "Create a knowledge space. Space names are case-sensitive and must be unique.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "minLength": 1 },
                    "description": { "type": "string" }
                },
                "required": ["name"],
                "additionalProperties": false
            }),
        ),
        tool(
            "space_list",
            "List spaces and counts of active memories.",
            empty_object_schema(),
        ),
        tool(
            "space_link",
            "Create a directed link: the from space imports knowledge from the to space.",
            json!({
                "type": "object",
                "properties": {
                    "from": { "type": "string", "minLength": 1 },
                    "to": { "type": "string", "minLength": 1 }
                },
                "required": ["from", "to"],
                "additionalProperties": false
            }),
        ),
    ]
}

pub fn contains(name: &str) -> bool {
    definitions()
        .iter()
        .any(|definition| definition["name"].as_str() == Some(name))
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema
    })
}

fn empty_object_schema() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

pub fn call_result(
    store: &dyn Store,
    config: &McpConfig,
    semantic: &mut SemanticCache,
    name: &str,
    arguments: &Value,
) -> Value {
    match call_tool(store, config, semantic, name, arguments) {
        Ok(structured) => {
            let text = serde_json::to_string(&structured)
                .unwrap_or_else(|error| format!("cannot serialize result: {error}"));
            json!({
                "content": [{ "type": "text", "text": text }],
                "structuredContent": structured,
                "isError": false
            })
        }
        Err(error) => json!({
            "content": [{ "type": "text", "text": error }],
            "isError": true
        }),
    }
}

fn call_tool(
    store: &dyn Store,
    config: &McpConfig,
    semantic: &mut SemanticCache,
    name: &str,
    args: &Value,
) -> std::result::Result<Value, String> {
    validate_arguments(name, args)?;
    let obj = args.as_object().ok_or("arguments must be an object")?;
    let result: Result<Value> = (|| match name {
        "memory_create" => {
            let spaces = resolve_spaces(store, string_array(obj, "spaces")?)?;
            let report = store.record_manual_write(ManualMemoryDraft {
                subject: optional_string(obj, "subject")?.map(str::to_string),
                content: string(obj, "content")?.to_string(),
                spaces,
                supersedes: None,
            })?;
            serde_json::to_value(report).map_err(|e| Error::Storage(e.to_string()))
        }
        "memory_update" => {
            let requested_id = string(obj, "id")?;
            let id = resolve_memory_id(store, requested_id)?;
            let detail = store
                .get_memory_detail(id)?
                .ok_or_else(|| Error::MemoryNotFound {
                    prefix: requested_id.to_string(),
                })?;
            let spaces = match obj.get("spaces") {
                Some(_) => resolve_spaces(store, string_array(obj, "spaces")?)?,
                None => detail.spaces.iter().map(|space| space.id).collect(),
            };
            let report = store.record_manual_write(ManualMemoryDraft {
                subject: Some(
                    optional_string(obj, "subject")?
                        .unwrap_or(&detail.assertion.subject)
                        .to_string(),
                ),
                content: string(obj, "content")?.to_string(),
                spaces,
                supersedes: Some(id),
            })?;
            serde_json::to_value(report).map_err(|e| Error::Storage(e.to_string()))
        }
        "memory_retire" => {
            let requested_id = string(obj, "id")?;
            let id = resolve_memory_id(store, requested_id)?;
            let memory = store.retire_memory(
                id,
                optional_string(obj, "reason")?.map(str::to_string),
                Ts::now(),
            )?;
            serde_json::to_value(memory).map_err(|e| Error::Storage(e.to_string()))
        }
        "memory_get" => {
            let requested_id = string(obj, "id")?;
            let id = resolve_memory_id(store, requested_id)?;
            let detail = store
                .get_memory_detail(id)?
                .ok_or_else(|| Error::MemoryNotFound {
                    prefix: requested_id.to_string(),
                })?;
            serde_json::to_value(detail).map_err(|e| Error::Storage(e.to_string()))
        }
        "memory_search" => {
            let spaces = match obj.get("spaces") {
                Some(_) => Some(resolve_spaces(store, string_array(obj, "spaces")?)?),
                None => None,
            };
            let at = optional_string(obj, "at")?.map(Ts::parse).transpose()?;
            let query = SearchQuery {
                text: string(obj, "query")?.to_string(),
                space_ids: spaces,
                limit: optional_u32(obj, "limit")?.unwrap_or(20),
                at,
                include_superseded: optional_bool(obj, "include_superseded")?.unwrap_or(false),
                include_retired: optional_bool(obj, "include_retired")?.unwrap_or(false),
            };
            match optional_string(obj, "mode")? {
                None => {
                    let hits = retrieval::search(store, query)?;
                    Ok(json!({ "hits": hits }))
                }
                Some(value) => {
                    let mode = retrieval::SearchMode::from_str(value).map_err(Error::Storage)?;
                    let outcome = semantic.search(store, query, mode)?;
                    serde_json::to_value(outcome).map_err(|e| Error::Storage(e.to_string()))
                }
            }
        }
        "code_search" => {
            let kinds: Vec<&str> = match obj.get("kinds") {
                Some(_) => string_array(obj, "kinds")?,
                None => Vec::new(),
            };
            let hits = retrieval::search_code(
                store,
                crate::core::CodeSearchQuery {
                    text: string(obj, "query")?.to_string(),
                    kinds: retrieval::parse_code_kinds(&kinds)?,
                    repository: optional_string(obj, "repository")?.map(str::to_string),
                    path_prefix: optional_string(obj, "path")?.map(str::to_string),
                    space_ids: match obj.get("spaces") {
                        Some(_) => Some(resolve_spaces(store, string_array(obj, "spaces")?)?),
                        None => None,
                    },
                    limit: optional_u32(obj, "limit")?.unwrap_or(20),
                    all_versions: optional_bool(obj, "all_versions")?.unwrap_or(false),
                },
            )?;
            Ok(json!({ "hits": hits }))
        }
        "repo_outline" => repo_tools::outline(config, obj),
        "repo_ingest" => repo_tools::ingest(store, config, obj),
        "repo_request_access" => repo_tools::request_access(
            config,
            string(obj, "path")?,
            optional_string(obj, "access")?,
            optional_string(obj, "reason")?,
        ),
        "memory_history" => {
            let subject = normalize_subject(string(obj, "subject")?);
            if subject.is_empty() {
                return Err(Error::EmptyQuery);
            }
            let memories = store.list_memories_by_subject(&subject)?;
            Ok(json!({ "memories": memories }))
        }
        "memory_conflicts" => {
            let conflicts = store.list_conflicts()?;
            Ok(json!({ "conflicts": conflicts }))
        }
        "memory_add_spaces" => {
            let requested_id = string(obj, "id")?;
            let id = resolve_memory_id(store, requested_id)?;
            let spaces = resolve_spaces(store, string_array(obj, "spaces")?)?;
            let added = store.add_memory_spaces(id, spaces)?;
            Ok(json!({ "memory_id": id, "spaces_added": added }))
        }
        "space_create" => {
            let space = store.create_space(
                string(obj, "name")?,
                optional_string(obj, "description")?.unwrap_or(""),
            )?;
            serde_json::to_value(space).map_err(|e| Error::Storage(e.to_string()))
        }
        "space_list" => {
            let spaces = store
                .list_spaces_with_counts()?
                .into_iter()
                .map(|(space, active_memory_count)| {
                    json!({ "space": space, "active_memory_count": active_memory_count })
                })
                .collect::<Vec<_>>();
            Ok(json!({ "spaces": spaces }))
        }
        "space_link" => {
            let from = store.get_space(string(obj, "from")?)?;
            let to = store.get_space(string(obj, "to")?)?;
            let link = store.link_spaces(from.id, to.id)?;
            serde_json::to_value(link).map_err(|e| Error::Storage(e.to_string()))
        }
        _ => Err(Error::Storage(format!("unknown tool: {name}"))),
    })();
    result.map_err(|error| error.to_string())
}

fn validate_arguments(name: &str, args: &Value) -> std::result::Result<(), String> {
    let Some(obj) = args.as_object() else {
        return Err("arguments must be an object".to_string());
    };
    let (allowed, required): (&[&str], &[&str]) = match name {
        "memory_create" => (&["content", "subject", "spaces"], &["content", "spaces"]),
        "memory_update" => (&["id", "content", "subject", "spaces"], &["id", "content"]),
        "memory_retire" => (&["id", "reason"], &["id"]),
        "memory_get" => (&["id"], &["id"]),
        "memory_search" => (
            &[
                "query",
                "spaces",
                "limit",
                "at",
                "include_superseded",
                "include_retired",
                "mode",
            ],
            &["query"],
        ),
        "code_search" => (
            &[
                "query",
                "kinds",
                "spaces",
                "repository",
                "path",
                "limit",
                "all_versions",
            ],
            &["query"],
        ),
        "repo_outline" => (&["path", "depth"], &["path"]),
        "repo_ingest" => (
            &[
                "path", "spaces", "paths", "history", "branches", "remotes", "tags",
            ],
            &["path", "spaces"],
        ),
        "repo_request_access" => (&["path", "access", "reason"], &["path"]),
        "memory_history" => (&["subject"], &["subject"]),
        "memory_conflicts" | "space_list" => (&[], &[]),
        "memory_add_spaces" => (&["id", "spaces"], &["id", "spaces"]),
        "space_create" => (&["name", "description"], &["name"]),
        "space_link" => (&["from", "to"], &["from", "to"]),
        _ => return Err(format!("unknown tool: {name}")),
    };
    for key in obj.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("unexpected argument '{key}'"));
        }
    }
    for key in required {
        if !obj.contains_key(*key) {
            return Err(format!("missing required argument '{key}'"));
        }
    }
    if let Some(value) = obj.get("depth") {
        if !value.as_u64().is_some_and(|d| d <= 4) {
            return Err("argument 'depth' must be an integer from 0 to 4".to_string());
        }
    }
    if let Some(value) = obj.get("paths") {
        let ok = value.as_array().is_some_and(|a| {
            a.iter()
                .all(|v| v.as_str().is_some_and(|s| !s.trim().is_empty()))
        });
        if !ok {
            return Err("argument 'paths' must be an array of non-empty strings".to_string());
        }
    }
    for key in ["history", "branches", "remotes", "tags"] {
        if obj.get(key).is_some_and(|v| !v.is_boolean()) {
            return Err(format!("argument '{key}' must be a boolean"));
        }
    }
    for key in ["repository", "path"] {
        if let Some(value) = obj.get(key) {
            if value.as_str().is_none_or(|s| s.trim().is_empty()) {
                return Err(format!("argument '{key}' must be a non-empty string"));
            }
        }
    }
    if let Some(value) = obj.get("kinds") {
        let Some(values) = value.as_array() else {
            return Err("argument 'kinds' must be an array of strings".to_string());
        };
        if values.iter().any(|v| !v.is_string()) {
            return Err("argument 'kinds' entries must be strings".to_string());
        }
    }
    if let Some(value) = obj.get("all_versions") {
        if !value.is_boolean() {
            return Err("argument 'all_versions' must be a boolean".to_string());
        }
    }
    for key in [
        "content", "subject", "id", "query", "at", "name", "from", "to",
    ] {
        if let Some(value) = obj.get(key) {
            let Some(value) = value.as_str() else {
                return Err(format!("argument '{key}' must be a string"));
            };
            if ["content", "subject", "query", "name", "from", "to"].contains(&key)
                && value.trim().is_empty()
            {
                return Err(format!("argument '{key}' cannot be empty"));
            }
            if key == "id" && value.len() < 4 {
                return Err("argument 'id' must be at least 4 characters".to_string());
            }
        }
    }
    if let Some(value) = obj.get("mode") {
        let Some(value) = value.as_str() else {
            return Err("argument 'mode' must be a string".to_string());
        };
        retrieval::SearchMode::from_str(value)
            .map_err(|error| format!("argument 'mode' {error}"))?;
    }
    if let Some(value) = obj.get("description") {
        if !value.is_string() {
            return Err("argument 'description' must be a string".to_string());
        }
    }
    if let Some(value) = obj.get("reason") {
        if !value.is_string() {
            return Err("argument 'reason' must be a string".to_string());
        }
    }
    for key in ["spaces"] {
        if let Some(value) = obj.get(key) {
            let Some(values) = value.as_array() else {
                return Err(format!("argument '{key}' must be an array of strings"));
            };
            if values.is_empty() {
                return Err(format!("argument '{key}' must contain at least one space"));
            }
            for entry in values {
                if entry.as_str().is_none_or(|s| s.trim().is_empty()) {
                    return Err(format!(
                        "argument '{key}' entries must be non-empty strings"
                    ));
                }
            }
        }
    }
    if let Some(value) = obj.get("limit") {
        let Some(limit) = value.as_u64() else {
            return Err("argument 'limit' must be an integer from 1 to 100".to_string());
        };
        if !(1..=100).contains(&limit) {
            return Err("argument 'limit' must be an integer from 1 to 100".to_string());
        }
    }
    for key in ["include_superseded", "include_retired"] {
        if obj.get(key).is_some_and(|value| !value.is_boolean()) {
            return Err(format!("argument '{key}' must be a boolean"));
        }
    }
    Ok(())
}

fn string<'a>(obj: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a str> {
    obj.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Storage(format!("missing or invalid string argument '{key}'")))
}

fn optional_string<'a>(
    obj: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>> {
    obj.get(key)
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| Error::Storage(format!("argument '{key}' must be a string")))
        })
        .transpose()
}

fn string_array<'a>(obj: &'a serde_json::Map<String, Value>, key: &str) -> Result<Vec<&'a str>> {
    obj.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Storage(format!("missing or invalid array argument '{key}'")))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| Error::Storage(format!("argument '{key}' entries must be strings")))
        })
        .collect()
}

fn optional_u32(obj: &serde_json::Map<String, Value>, key: &str) -> Result<Option<u32>> {
    obj.get(key)
        .map(|value| {
            value
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| {
                    Error::Storage(format!("argument '{key}' must be a positive integer"))
                })
        })
        .transpose()
}

fn optional_bool(obj: &serde_json::Map<String, Value>, key: &str) -> Result<Option<bool>> {
    obj.get(key)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| Error::Storage(format!("argument '{key}' must be a boolean")))
        })
        .transpose()
}

fn resolve_spaces(store: &dyn Store, names: Vec<&str>) -> Result<Vec<Uuid>> {
    names
        .into_iter()
        .map(|name| store.get_space(name).map(|space| space.id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::sqlite::SqliteStore;

    #[test]
    fn registers_all_tools_with_object_schemas() {
        let definitions = definitions();
        assert_eq!(definitions.len(), 15);
        for tool in definitions {
            assert!(tool["inputSchema"]["type"].as_str() == Some("object"));
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }
    }

    #[test]
    fn tool_error_is_returned_as_mcp_tool_result() {
        let store = SqliteStore::open_in_memory().unwrap();
        let result = call_result(
            &store,
            &McpConfig::none(),
            &mut SemanticCache::default(),
            "memory_create",
            &json!({"content":"x"}),
        );
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("missing required argument 'spaces'"));
    }
}
