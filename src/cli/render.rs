//! Human and JSON rendering. JSON serializes the exact engine structs, so
//! the two views can never diverge.

use crate::cli::action_label;
use crate::core::{
    CodeDerivation, ConflictGroup, GitObservationReport, IngestScope, Memory, MemoryDetail,
    MemorySummary, ObservationReport, SourceEventKind, Space,
};

fn json<T: serde::Serialize + ?Sized>(value: &T) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("engine structs serialize")
    );
}

pub fn outline(outline: &crate::ingest::outline::Outline, as_json: bool) {
    if as_json {
        json(outline);
        return;
    }
    match &outline.commit {
        Some(c) => println!("{} ({} @ {})", outline.root, outline.source, &c[..c.len().min(12)]),
        None => println!("{} ({})", outline.root, outline.source),
    }
    println!(
        "{:<44} {:>7} {:>9} {:>10}  kinds",
        "path", "files", "ingest", "bytes"
    );
    for n in &outline.nodes {
        let kinds = n
            .kinds
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "{:<44} {:>7} {:>9} {:>10}  {}",
            format!("{}{}", "  ".repeat(n.depth as usize), n.path),
            n.files,
            n.ingestable_files,
            n.bytes,
            kinds
        );
    }
}

pub fn code_hits(hits: &[crate::core::CodeHit], as_json: bool) {
    if as_json {
        json(hits);
        return;
    }
    if hits.is_empty() {
        println!("no definitions found");
        return;
    }
    for (i, h) in hits.iter().enumerate() {
        let name = h
            .qualified_name
            .as_deref()
            .or(h.name.as_deref())
            .unwrap_or("<anonymous>");
        println!(
            "{}. {} {}{}",
            i + 1,
            h.kind.as_str(),
            name,
            if h.current { "" } else { "  [not current]" }
        );
        if let Some(sig) = &h.signature {
            println!("   {}", first_line(sig).trim());
        }
        let at = match (&h.commit_oid, &h.repository_path) {
            (Some(c), Some(r)) => format!("  @ {} in {r}", &c[..c.len().min(12)]),
            _ => String::new(),
        };
        println!("   {}:{}-{}{at}", h.path, h.start_line, h.end_line);
    }
}

pub fn code_structure(items: &[CodeDerivation], as_json: bool) {
    if as_json {
        json(items);
        return;
    }
    for d in items {
        println!(
            "{} {} ({}, {} errors)",
            d.language.as_str(),
            d.content_hash,
            d.status.as_str(),
            d.error_count
        );
        for e in &d.entities {
            println!(
                "  {} {} [{}:{}-{}:{}]",
                e.kind.as_str(),
                e.qualified_name
                    .as_deref()
                    .or(e.name.as_deref())
                    .unwrap_or("<anonymous>"),
                e.range.start_line,
                e.range.start_column,
                e.range.end_line,
                e.range.end_column
            );
        }
        for i in &d.imports {
            println!("  import {}", i.target);
        }
    }
}

pub fn observation_report(report: &ObservationReport, as_json: bool) {
    if as_json {
        json(report);
        return;
    }
    let scope = match report.scope {
        IngestScope::Directory => "directory",
        IngestScope::File => "file",
        IngestScope::GitRepository => "git_repository",
    };
    println!(
        "session {} root {} ({})",
        report.session_id, report.root, scope
    );
    for file in &report.files {
        let outcome = match &file.outcome {
            crate::core::FileOutcome::Unchanged => "unchanged".to_string(),
            crate::core::FileOutcome::Changed => "changed".to_string(),
            crate::core::FileOutcome::Created => "created".to_string(),
            crate::core::FileOutcome::Continued { from_location } => match from_location {
                Some(from) => format!("continued from {from}"),
                None => "continued".to_string(),
            },
        };
        println!(
            "  {} logical {} {}",
            file.location, file.logical_id, outcome
        );
        for action in &file.actions {
            println!(
                "    memory {} \"{}\" {} [{}]",
                action.memory_id,
                action.subject,
                action_label(&action.action),
                action.spaces.join(", ")
            );
        }
    }
    for event in &report.events {
        match &event.kind {
            SourceEventKind::Moved {
                from_location,
                to_location,
            } => println!(
                "  moved: logical {} {} -> {}",
                event.logical_id,
                from_location.as_deref().unwrap_or("?"),
                to_location
            ),
            SourceEventKind::Disappeared { from_location } => println!(
                "  disappeared: logical {} (last seen {})",
                event.logical_id,
                from_location.as_deref().unwrap_or("?")
            ),
        }
    }
}

/// Human and JSON output of one git repository observation. The counters
/// are the deterministic deltas of the session.
pub fn git_observation_report(report: &GitObservationReport, as_json: bool) {
    if as_json {
        json(report);
        return;
    }
    println!(
        "session {} repository {} ({})",
        report.session_id, report.repository_id, report.repository_path
    );
    println!(
        "refs: {} observed, {} added, {} changed, {} removed",
        report.refs_observed, report.refs_added, report.refs_changed, report.refs_removed
    );
    println!(
        "objects: {} discovered, {} new, {} existing",
        report.objects_discovered, report.objects_new, report.objects_existing
    );
    println!(
        "derivations: {} new, {} reused",
        report.derivations_new, report.derivations_reused
    );
    println!(
        "code: {} supported, {} parsed, {} reused, {} entities, {} parse errors",
        report.code.supported_inputs,
        report.code.parsed,
        report.code.reused,
        report.code.entities,
        report.code.parse_errors
    );
    for event in &report.events {
        match event.kind {
            crate::core::GitRefEventKind::Created => {
                println!(
                    "  ref created: {} -> {}",
                    event.ref_name,
                    event.to_oid.as_deref().unwrap_or("?")
                )
            }
            crate::core::GitRefEventKind::FastForward => println!(
                "  ref fast-forward: {} {} -> {}",
                event.ref_name,
                event.from_oid.as_deref().unwrap_or("?"),
                event.to_oid.as_deref().unwrap_or("?")
            ),
            crate::core::GitRefEventKind::NonFastForward => println!(
                "  ref non-fast-forward: {} {} -> {}",
                event.ref_name,
                event.from_oid.as_deref().unwrap_or("?"),
                event.to_oid.as_deref().unwrap_or("?")
            ),
            crate::core::GitRefEventKind::Deleted => {
                println!(
                    "  ref deleted: {} (last at {})",
                    event.ref_name,
                    event.from_oid.as_deref().unwrap_or("?")
                )
            }
        }
    }
    for file in &report.files {
        let outcome = match file.outcome {
            crate::core::GitFileOutcome::New => "new",
            crate::core::GitFileOutcome::Unchanged => "unchanged",
        };
        println!("  {} @ {} ({})", file.path, file.commit_oid, outcome);
        for action in &file.actions {
            println!(
                "    memory {} \"{}\" {} [{}]",
                action.memory_id,
                action.subject,
                action_label(&action.action),
                action.spaces.join(", ")
            );
        }
    }
}

pub fn space_created(space: &Space, as_json: bool) {
    if as_json {
        json(space);
        return;
    }
    println!("space '{}' created ({})", space.name, space.id);
}

pub fn space_list(spaces: &[(Space, u64)], as_json: bool) {
    if as_json {
        let rows: Vec<serde_json::Value> = spaces
            .iter()
            .map(|(s, n)| {
                serde_json::json!({
                    "id": s.id,
                    "name": s.name,
                    "description": s.description,
                    "created_at": s.created_at,
                    "active_memories": n,
                })
            })
            .collect();
        json(&rows);
        return;
    }
    for (space, count) in spaces {
        println!(
            "{}\t{} active memories\t{}",
            space.name, count, space.description
        );
    }
}

pub fn space_linked(from: &Space, to: &Space, as_json: bool) {
    if as_json {
        println!("{{ \"from\": \"{}\", \"to\": \"{}\" }}", from.name, to.name);
        return;
    }
    println!("link created: {} -> {}", from.name, to.name);
}

pub fn search_hits(hits: &[crate::core::SearchHit], as_json: bool) {
    if as_json {
        json(&hits);
        return;
    }
    if hits.is_empty() {
        println!("no results");
        return;
    }
    for (i, hit) in hits.iter().enumerate() {
        println!("{}. [{}] {} ({})", i + 1, hit.score, hit.subject, hit.state);
        println!("   {}", hit.snippet.replace('\n', " "));
        let spaces = hit
            .spaces
            .iter()
            .map(|s| match &s.via {
                Some(via) => format!("{} (via {})", s.name, via),
                None => s.name.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "   spaces: {}",
            if spaces.is_empty() { "-" } else { &spaces }
        );
        println!("   source: {} {}", hit.source_title, hit.source_location);
        println!("   memory: {}", hit.memory_id);
    }
}

pub fn search_outcome(outcome: &crate::retrieval::SearchOutcome, as_json: bool) {
    if as_json {
        json(outcome);
        return;
    }
    print_retrieval_summary(outcome);
    if outcome.hits.is_empty() {
        println!("no results");
        return;
    }
    for (index, result) in outcome.hits.iter().enumerate() {
        let hit = &result.hit;
        println!(
            "{}. [{} {:.6}] {} ({})",
            index + 1,
            result.score_kind.as_str(),
            hit.score,
            hit.subject,
            hit.state
        );
        println!("   {}", hit.snippet.replace('\n', " "));
        println!("   score provenance: {}", render_score_provenance(result));
        let spaces = render_spaces(hit);
        println!("   spaces: {spaces}");
        println!("   source: {} {}", hit.source_title, hit.source_location);
        println!("   memory: {}", hit.memory_id);
    }
}

/// Renders a compact, provenance-preserving answer from deterministic search hits.
pub fn recall_hits(hits: &[crate::core::SearchHit], as_json: bool) {
    if as_json {
        json(&hits);
        return;
    }
    let Some(answer) = hits.first() else {
        println!("no results");
        return;
    };

    println!("answer: {}", answer.subject);
    for line in answer.content.lines() {
        println!("  {line}");
    }
    println!("source: {} {}", answer.source_title, answer.source_location);
    println!("spaces: {}", render_spaces(answer));
    println!("memory: {}", answer.memory_id);

    if hits.len() > 1 {
        println!("related:");
        for (index, hit) in hits.iter().enumerate().skip(1) {
            println!("{}. {}", index + 1, hit.subject);
            println!("   {}", hit.snippet.replace('\n', " "));
            println!("   spaces: {}", render_spaces(hit));
            println!("   source: {} {}", hit.source_title, hit.source_location);
            println!("   memory: {}", hit.memory_id);
        }
    }
}

pub fn recall_outcome(outcome: &crate::retrieval::SearchOutcome, as_json: bool) {
    if as_json {
        json(outcome);
        return;
    }
    print_retrieval_summary(outcome);
    let Some(answer) = outcome.hits.first() else {
        println!("no results");
        return;
    };

    println!("answer: {}", answer.hit.subject);
    for line in answer.hit.content.lines() {
        println!("  {line}");
    }
    println!(
        "score: [{} {:.6}]",
        answer.score_kind.as_str(),
        answer.hit.score
    );
    println!("score provenance: {}", render_score_provenance(answer));
    println!(
        "source: {} {}",
        answer.hit.source_title, answer.hit.source_location
    );
    println!("spaces: {}", render_spaces(&answer.hit));
    println!("memory: {}", answer.hit.memory_id);

    if outcome.hits.len() > 1 {
        println!("related:");
        for (index, result) in outcome.hits.iter().enumerate().skip(1) {
            println!("{}. {}", index + 1, result.hit.subject);
            println!("   {}", result.hit.snippet.replace('\n', " "));
            println!(
                "   score: [{} {:.6}] ({})",
                result.score_kind.as_str(),
                result.hit.score,
                render_score_provenance(result)
            );
            println!("   spaces: {}", render_spaces(&result.hit));
            println!(
                "   source: {} {}",
                result.hit.source_title, result.hit.source_location
            );
            println!("   memory: {}", result.hit.memory_id);
        }
    }
}

fn print_retrieval_summary(outcome: &crate::retrieval::SearchOutcome) {
    println!(
        "retrieval: requested {}, effective {}",
        outcome.requested.as_str(),
        outcome.effective.as_str()
    );
    if let Some(fallback) = &outcome.fallback {
        println!("fallback: {fallback}");
    }
    println!(
        "candidates: lexical {}, semantic {}",
        outcome.lexical_candidates, outcome.semantic_candidates
    );
}

fn render_score_provenance(result: &crate::retrieval::RetrievalHit) -> String {
    let provenance = result.provenance;
    let mut branches = Vec::new();
    if let Some(rank) = provenance.lexical_rank {
        let score = provenance
            .lexical_score
            .map(|score| format!(", score {score:.6}"))
            .unwrap_or_default();
        branches.push(format!("lexical rank {rank}{score}"));
    }
    if let Some(rank) = provenance.semantic_rank {
        let score = provenance
            .semantic_score
            .map(|score| format!(", score {score:.6}"))
            .unwrap_or_default();
        branches.push(format!("semantic rank {rank}{score}"));
    }
    if branches.is_empty() {
        "no retrieval branch ran".to_string()
    } else {
        branches.join("; ")
    }
}

fn render_spaces(hit: &crate::core::SearchHit) -> String {
    let spaces = hit
        .spaces
        .iter()
        .map(|space| match &space.via {
            Some(via) => format!("{} (via {})", space.name, via),
            None => space.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    if spaces.is_empty() {
        "-".to_string()
    } else {
        spaces
    }
}

pub fn inspect(detail: &MemoryDetail, as_json: bool) {
    if as_json {
        json(detail);
        return;
    }
    let m = &detail.memory;
    println!("memory   {} ({}, created {})", m.id, m.state, m.created_at);
    if let Some(sb) = &detail.superseded_by {
        println!("superseded by {} (created {})", sb.memory_id, sb.created_at);
    }
    for s in &detail.supersedes {
        println!("supersedes {} (created {})", s.memory_id, s.created_at);
    }
    if let Some(at) = m.retired_at {
        println!(
            "retired  {} {}",
            at,
            m.retire_reason.as_deref().unwrap_or("")
        );
    }
    println!(
        "assert   {} subject=\"{}\" kind={} extraction={}",
        detail.assertion.id,
        detail.assertion.subject,
        detail.assertion.kind,
        detail.assertion.extraction
    );
    for line in detail.assertion.content.lines() {
        println!("  {line}");
    }
    let e = &detail.evidence;
    println!(
        "evidence {} {} {} lines {}-{}",
        e.id,
        e.kind,
        e.heading_path.as_deref().unwrap_or("-"),
        e.start_line.map_or("-".to_string(), |v| v.to_string()),
        e.end_line.map_or("-".to_string(), |v| v.to_string())
    );
    let s = &detail.source;
    println!(
        "source   {} {} {} sha256:{}",
        s.id,
        s.kind,
        s.location,
        &s.content_hash[..16.min(s.content_hash.len())]
    );
    if let Some(git) = &detail.git {
        println!(
            "git      repo {} blob {} path {} commit {} ({})",
            git.repository_path, git.blob_oid, git.path, git.commit_oid, git.commit_time
        );
        if !git.refs.is_empty() {
            println!("  refs: {}", git.refs.join(", "));
        }
    }
    println!(
        "spaces   {}",
        if detail.spaces.is_empty() {
            "-".to_string()
        } else {
            detail
                .spaces
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
}

pub fn trace(detail: &MemoryDetail, as_json: bool) {
    if as_json {
        json(detail);
        return;
    }
    let m = &detail.memory;
    let a = &detail.assertion;
    let e = &detail.evidence;
    let s = &detail.source;
    println!(
        "memory    {} state={} created={}",
        m.id, m.state, m.created_at
    );
    println!(
        "assertion {} kind={} subject=\"{}\" extraction={} extracted={}",
        a.id, a.kind, a.subject, a.extraction, a.created_at
    );
    println!("  content: {}", first_line(&a.content));
    println!(
        "evidence  {} kind={} heading={} lines={}-{}",
        e.id,
        e.kind,
        e.heading_path.as_deref().unwrap_or("-"),
        e.start_line.map_or("-".to_string(), |v| v.to_string()),
        e.end_line.map_or("-".to_string(), |v| v.to_string())
    );
    println!(
        "source    {} kind={} location={} ingested={}",
        s.id, s.kind, s.location, s.ingested_at
    );
    println!("  sha256:{}", s.content_hash);
    render_lifecycle(&detail.source_lifecycle);
    if let Some(git) = &detail.git {
        println!(
            "git       repo {} blob {} path {} commit {} ({})",
            git.repository_path, git.blob_oid, git.path, git.commit_oid, git.commit_time
        );
        println!("  commit message: {}", first_line(&git.commit_message));
        if !git.refs.is_empty() {
            println!("  refs: {}", git.refs.join(", "));
        }
    }
    println!(
        "spaces    {}",
        detail
            .spaces
            .iter()
            .map(|sp| sp.name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    );
}

pub fn history(subject: &str, memories: &[MemorySummary], as_json: bool) {
    if as_json {
        json(&memories);
        return;
    }
    println!("subject \"{}\": {} memories", subject, memories.len());
    for m in memories {
        let tail = match (m.state, m.superseded_by, m.retire_reason.as_deref()) {
            (crate::core::MemoryState::Superseded, Some(by), _) => {
                format!(" -> {}", by)
            }
            (crate::core::MemoryState::Retired, _, Some(reason)) => {
                format!(" ({reason})")
            }
            _ => String::new(),
        };
        println!(
            "  {} {} {} \"{}\"{}",
            m.state,
            m.memory_id,
            m.created_at,
            first_line(&m.content),
            tail
        );
    }
}

pub fn conflicts(groups: &[ConflictGroup], as_json: bool) {
    if as_json {
        json(&groups);
        return;
    }
    if groups.is_empty() {
        println!("no conflicts");
        return;
    }
    for group in groups {
        println!("{} / \"{}\":", group.space_name, group.subject);
        for m in &group.members {
            println!(
                "  {} {} {} \"{}\"",
                m.memory_id, m.state, m.created_at, m.subject
            );
        }
    }
}

pub fn retired(memory: &Memory) {
    println!(
        "memory {} retired at {}",
        memory.id,
        memory.retired_at.map_or("?".to_string(), |t| t.to_string())
    );
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

fn render_lifecycle(lifecycle: &crate::core::SourceLifecycle) {
    let l = &lifecycle.logical;
    println!(
        "logical   {} state={} current={} created={}",
        l.id,
        l.state,
        l.current_location.as_deref().unwrap_or("-"),
        l.created_at
    );
    for event in &lifecycle.events {
        match &event.kind {
            crate::core::SourceEventKind::Moved {
                from_location,
                to_location,
            } => println!(
                "  moved {} -> {} at {}",
                from_location.as_deref().unwrap_or("?"),
                to_location,
                event.created_at
            ),
            crate::core::SourceEventKind::Disappeared { from_location } => println!(
                "  disappeared from {} at {}",
                from_location.as_deref().unwrap_or("?"),
                event.created_at
            ),
        }
    }
}
