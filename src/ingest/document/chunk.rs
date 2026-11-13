//! Versioned, byte-preserving chunking. Context is metadata, never copied into input text.
use std::collections::{BTreeMap, BTreeSet};

use crate::core::{DocumentExtractionDraft, Error, NodeKind, Result};
use crate::embeddings::budget::TokenBudget;

pub const CHUNKER_VERSION: &str = "heading_aware@1";
/// Bounds work per tokenizer call, not a token estimate. Every final input is counted.
const WINDOW_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkDraft {
    pub start: u64,
    pub end: u64,
    /// Includes document prefix and special tokens.
    pub tokens: usize,
    /// Section paths containing the entire source range, ordered outermost first.
    pub context: Vec<Vec<u32>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkPlan {
    pub version: String,
    pub budget_identity: String,
    pub chunks: Vec<ChunkDraft>,
}

fn boundary(text: &str, mut end: usize) -> usize {
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// S1 permits partially specified structure. S3 requires ranged sections to form a tree:
/// contradictory overlaps would copy unbounded sibling context into every chunk.
fn validate_section_ranges(draft: &DocumentExtractionDraft) -> Result<()> {
    let ranges: BTreeMap<Vec<u32>, (u64, u64)> = draft
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Section)
        .filter_map(|n| n.text_range.map(|r| (n.path.clone(), r)))
        .collect();
    for (path, &(a, b)) in &ranges {
        for depth in (1..path.len()).rev() {
            if let Some(&(pa, pb)) = ranges.get(&path[..depth]) {
                if a < pa || b > pb {
                    return Err(Error::InvalidDocument(
                        "section range lies outside its ancestor".into(),
                    ));
                }
                break;
            }
        }
    }
    let mut ordered: Vec<_> = ranges.iter().filter(|(_, (a, b))| a < b).collect();
    ordered.sort_by(|(ap, (a, b)), (bp, (c, d))| {
        a.cmp(c)
            .then_with(|| d.cmp(b))
            .then_with(|| ap.len().cmp(&bp.len()))
    });
    let mut open: Vec<(&[u32], u64)> = Vec::new();
    for (path, &(a, b)) in ordered {
        while open.last().is_some_and(|(_, end)| *end <= a) {
            open.pop();
        }
        if let Some((parent, end)) = open.last() {
            if b > *end || !path.starts_with(parent) || path.len() <= parent.len() {
                return Err(Error::InvalidDocument(
                    "overlapping section ranges do not form a hierarchy".into(),
                ));
            }
        }
        open.push((path.as_slice(), b));
    }
    Ok(())
}

/// Partitions ALL canonical bytes, including whitespace and CRLF. No overlap or normalization.
/// Oversized units shrink by halving bounded windows; no monotonic token-count assumption is used.
/// A WordPiece UNK is model behavior, not dropped source text: citations retain every byte.
pub fn chunk_document(draft: &DocumentExtractionDraft, budget: &TokenBudget) -> Result<ChunkPlan> {
    draft.validate()?;
    validate_section_ranges(draft)?;
    let text = &draft.canonical_text;
    let mut section_ends = BTreeSet::new();
    let mut block_ends = BTreeSet::new();
    let mut events = BTreeMap::<usize, Vec<(Vec<u32>, usize)>>::new();
    for node in &draft.nodes {
        if let Some((a, b)) = node.text_range {
            let (a, b) = (a as usize, b as usize);
            if node.kind == NodeKind::Section {
                section_ends.insert(b);
                events.entry(a).or_default().push((node.path.clone(), b));
            } else {
                block_ends.insert(b);
                block_ends.insert(a);
            }
        }
    }
    let mut events = events.into_iter().peekable();
    let mut active = BTreeMap::<Vec<u32>, usize>::new();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = boundary(text, text.len().min(start + WINDOW_BYTES));
        loop {
            if end == start {
                end = start + text[start..].chars().next().unwrap().len_utf8();
                budget.validate_document(&text[start..end])?;
                break;
            }
            if budget.count_document(&text[start..end])? <= budget.limit() {
                break;
            }
            end = boundary(text, start + (end - start) / 2);
        }
        // Prefer section, then block boundaries. Test the shorter candidate too: token counts need
        // not be monotonic (e.g. a long WordPiece token can become UNK).
        let preferred = section_ends
            .range((start + 1)..=end)
            .next_back()
            .copied()
            .or_else(|| block_ends.range((start + 1)..=end).next_back().copied());
        if let Some(p) = preferred {
            if budget.count_document(&text[start..p])? <= budget.limit() {
                end = p;
            }
        } else if end < text.len() {
            // Avoid cutting ordinary words/code tokens when a nearby whitespace boundary exists.
            let floor = start + (end - start) * 3 / 4;
            if let Some((i, c)) = text[start..end]
                .char_indices()
                .rev()
                .find(|(i, c)| start + i >= floor && c.is_whitespace())
            {
                let p = start + i + c.len_utf8();
                if budget.count_document(&text[start..p])? <= budget.limit() {
                    end = p;
                }
            }
        }
        let tokens = budget.validate_document(&text[start..end])?;
        while events.peek().is_some_and(|(at, _)| *at <= start) {
            for (path, stop) in events.next().unwrap().1 {
                active.insert(path, stop);
            }
        }
        active.retain(|_, stop| *stop > start);
        let context = active
            .iter()
            .filter(|(_, stop)| **stop >= end)
            .map(|(path, _)| path.clone())
            .collect();
        chunks.push(ChunkDraft {
            start: start as u64,
            end: end as u64,
            tokens,
            context,
        });
        if end <= start {
            return Err(Error::InvalidDocument("chunker made no progress".into()));
        }
        start = end;
    }
    Ok(ChunkPlan {
        version: CHUNKER_VERSION.into(),
        budget_identity: budget.identity().into(),
        chunks,
    })
}
