//! Human and JSON rendering. JSON serializes the exact engine structs, so
//! the two views can never diverge.

use crate::cli::action_label;
use crate::core::{
    ConflictGroup, GitObservationReport, IngestScope, Memory, MemoryDetail, MemorySummary,
    ObservationReport, SourceEventKind, Space,
};

fn json<T: serde::Serialize>(value: &T) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("engine structs serialize")
    );
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
