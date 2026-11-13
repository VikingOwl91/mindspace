//! Deterministic structure of Markdown and plain text (M8 S2).
//!
//! Every node's range is a half-open UTF-8 byte range of the CANONICAL text, produced by the parser's
//! own offsets (`pulldown-cmark`'s offset iterator for Markdown, line scanning for plain text), so it
//! is `byte_exact` there. Nothing is invented: plain text has paragraphs and nothing else; Markdown
//! has sections, headings, paragraphs, lists, list items and code blocks. Tables, block quotes, HTML
//! blocks and rules are not nodes: their text stays inside the enclosing node's range.
//!
//! The tree: headings open nested `section` nodes (a section runs to the next heading of the same or
//! a higher level); blocks before the first heading are children of the document root. Paths are
//! 1-based sibling ordinals. Nesting deeper than [`MAX_STRUCT_DEPTH`] is not emitted (its text stays
//! inside the ancestor's range).

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::core::{AnchorPrecision, DocNodeDraft, DocumentRefusal, Error, NodeKind, Result};

/// Deepest node path this adapter emits.
pub const MAX_STRUCT_DEPTH: usize = 32;

/// What the structure pass produced.
pub struct Structure {
    pub nodes: Vec<DocNodeDraft>,
    /// Text of the first heading (Markdown only), whitespace-collapsed.
    pub first_heading: Option<String>,
}

struct Builder {
    nodes: Vec<DocNodeDraft>,
    /// Child counters: `counts[i]` for node `i`; `root_count` for the root.
    counts: Vec<u32>,
    root_count: u32,
    max_nodes: usize,
}

impl Builder {
    fn new(max_nodes: usize) -> Self {
        Builder {
            nodes: Vec::new(),
            counts: Vec::new(),
            root_count: 0,
            max_nodes,
        }
    }

    /// Adds a node under `parent` (`None` = root). Returns its index, or `None` if the depth limit
    /// suppresses it. Errors only on the node limit.
    fn add(
        &mut self,
        parent: Option<usize>,
        kind: NodeKind,
        range: (usize, usize),
    ) -> Result<Option<usize>> {
        let mut path = match parent {
            Some(p) => self.nodes[p].path.clone(),
            None => Vec::new(),
        };
        if path.len() >= MAX_STRUCT_DEPTH {
            return Ok(None);
        }
        if self.nodes.len() >= self.max_nodes {
            return Err(Error::DocumentRefused {
                reason: DocumentRefusal::TooManyNodes,
                detail: format!("more than {} structure nodes", self.max_nodes),
            });
        }
        let ordinal = match parent {
            Some(p) => {
                self.counts[p] += 1;
                self.counts[p]
            }
            None => {
                self.root_count += 1;
                self.root_count
            }
        };
        path.push(ordinal);
        self.nodes.push(DocNodeDraft {
            path,
            kind,
            text_range: Some((range.0 as u64, range.1 as u64)),
            page: None,
            bbox: None,
            precision: AnchorPrecision::ByteExact,
        });
        self.counts.push(0);
        Ok(Some(self.nodes.len() - 1))
    }

    fn set_end(&mut self, idx: usize, end: usize) {
        if let Some((a, _)) = self.nodes[idx].text_range {
            self.nodes[idx].text_range = Some((a, (end as u64).max(a)));
        }
    }
}

/// The range with trailing whitespace (line ends included) removed.
fn trimmed(text: &str, start: usize, end: usize) -> (usize, usize) {
    let end = start + text[start..end].trim_end().len();
    (start, end)
}

#[derive(Clone, Copy)]
enum Frame {
    /// Between the start and the end of a heading.
    InHeading,
    List(Option<usize>),
    Item(Option<usize>),
    /// Inside a block quote or any other container whose blocks are not nodes.
    Opaque,
}

pub fn markdown(text: &str, max_nodes: usize) -> Result<Structure> {
    let mut b = Builder::new(max_nodes);
    let mut first_heading: Option<String> = None;
    // open sections: (heading level, node index)
    let mut sections: Vec<(u8, Option<usize>)> = Vec::new();
    let mut frames: Vec<Frame> = Vec::new();
    let mut heading_text: Option<String> = None;

    // Where a block goes: `Some(None)` = the document root, `Some(Some(i))` = under node `i`,
    // `None` = nowhere (inside a quote, or under an ancestor the depth limit suppressed).
    fn section_parent(sections: &[(u8, Option<usize>)]) -> Option<Option<usize>> {
        match sections.last() {
            None => Some(None),
            Some((_, Some(i))) => Some(Some(*i)),
            Some((_, None)) => None,
        }
    }
    let block_parent =
        |frames: &[Frame], sections: &[(u8, Option<usize>)]| -> Option<Option<usize>> {
            match frames.last() {
                Some(Frame::Opaque | Frame::List(_)) => None,
                Some(Frame::Item(i)) => i.map(Some),
                _ => section_parent(sections),
            }
        };

    for (event, range) in Parser::new_ext(text, Options::empty()).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let lvl = match level {
                    HeadingLevel::H1 => 1u8,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
                if matches!(
                    frames.last(),
                    Some(Frame::Opaque | Frame::Item(_) | Frame::List(_))
                ) {
                    // a heading inside a quote or list item is content, not a section break
                    frames.push(Frame::Opaque);
                    continue;
                }
                // close sections of the same or a deeper level at this heading's start
                while sections.last().is_some_and(|s| s.0 >= lvl) {
                    let (_, node) = sections.pop().expect("checked");
                    if let Some(i) = node {
                        let (_, end) = trimmed(
                            text,
                            b.nodes[i].text_range.map_or(0, |r| r.0 as usize),
                            range.start,
                        );
                        b.set_end(i, end);
                    }
                }
                let section = match section_parent(&sections) {
                    Some(parent) => b.add(parent, NodeKind::Section, (range.start, text.len()))?,
                    None => None,
                };
                sections.push((lvl, section));
                let (hs, he) = trimmed(text, range.start, range.end);
                if section.is_some() {
                    b.add(section, NodeKind::Heading, (hs, he))?;
                }
                frames.push(Frame::InHeading);
                heading_text = Some(String::new());
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(Frame::InHeading) = frames.last() {
                    frames.pop();
                    if let Some(t) = heading_text.take() {
                        if first_heading.is_none() {
                            first_heading =
                                Some(t.split_whitespace().collect::<Vec<_>>().join(" "));
                        }
                    }
                } else if let Some(Frame::Opaque) = frames.last() {
                    frames.pop();
                }
            }
            Event::Text(t) | Event::Code(t) => {
                if let Some(h) = heading_text.as_mut() {
                    h.push_str(&t);
                }
            }
            Event::Start(Tag::Paragraph) => {
                if let Some(parent) = block_parent(&frames, &sections) {
                    // paragraphs inside list items stay inside the item
                    if !matches!(frames.last(), Some(Frame::Item(_))) {
                        let (s, e) = trimmed(text, range.start, range.end);
                        b.add(parent, NodeKind::Paragraph, (s, e))?;
                    }
                }
            }
            Event::Start(Tag::List(_)) => match block_parent(&frames, &sections) {
                Some(parent) => {
                    let (s, e) = trimmed(text, range.start, range.end);
                    let node = b.add(parent, NodeKind::List, (s, e))?;
                    frames.push(Frame::List(node));
                }
                None => frames.push(Frame::Opaque),
            },
            Event::End(TagEnd::List(_)) => {
                frames.pop();
            }
            Event::Start(Tag::Item) => match frames.last() {
                Some(Frame::List(Some(list))) => {
                    let (s, e) = trimmed(text, range.start, range.end);
                    let node = b.add(Some(*list), NodeKind::ListItem, (s, e))?;
                    frames.push(Frame::Item(node));
                }
                _ => frames.push(Frame::Opaque),
            },
            Event::End(TagEnd::Item) => {
                frames.pop();
            }
            Event::Start(Tag::CodeBlock(_)) => {
                if let Some(parent) = block_parent(&frames, &sections) {
                    let (s, e) = trimmed(text, range.start, range.end);
                    b.add(parent, NodeKind::CodeBlock, (s, e))?;
                }
                frames.push(Frame::Opaque);
            }
            Event::End(TagEnd::CodeBlock) => {
                frames.pop();
            }
            Event::Start(Tag::BlockQuote(_)) => frames.push(Frame::Opaque),
            Event::End(TagEnd::BlockQuote(_)) => {
                frames.pop();
            }
            Event::Start(Tag::HtmlBlock) => frames.push(Frame::Opaque),
            Event::End(TagEnd::HtmlBlock) => {
                frames.pop();
            }
            _ => {}
        }
    }
    // sections still open run to the end of the text, without trailing whitespace
    while let Some((_, node)) = sections.pop() {
        if let Some(i) = node {
            let (_, end) = trimmed(
                text,
                b.nodes[i].text_range.map_or(0, |r| r.0 as usize),
                text.len(),
            );
            b.set_end(i, end);
        }
    }
    Ok(Structure {
        nodes: b.nodes,
        first_heading,
    })
}

/// Plain text: paragraphs separated by blank lines. No other structure is claimed.
pub fn plain(text: &str, max_nodes: usize) -> Result<Structure> {
    let mut b = Builder::new(max_nodes);
    let mut start: Option<usize> = None;
    let mut last_end = 0usize;
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        if content.trim().is_empty() {
            if let Some(s) = start.take() {
                b.add(None, NodeKind::Paragraph, (s, last_end))?;
            }
        } else {
            if start.is_none() {
                start = Some(offset);
            }
            last_end = offset + content.trim_end().len();
        }
        offset += line.len();
    }
    if let Some(s) = start {
        b.add(None, NodeKind::Paragraph, (s, last_end))?;
    }
    Ok(Structure {
        nodes: b.nodes,
        first_heading: None,
    })
}
