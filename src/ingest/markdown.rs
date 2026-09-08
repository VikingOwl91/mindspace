//! Deterministic markdown/text parsing into evidence fragments.
//!
//! Markdown is split into flat sections: each heading opens a section that
//! runs until the next heading of any level. Text before the first heading
//! becomes a preamble fragment. A file with no headings is one document
//! fragment. A `.txt` file is one document fragment. Parsing is pure and
//! produces the same fragments for the same bytes, every time.

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

use crate::core::EvidenceKind;

/// Identifier of the deterministic rule that produced an assertion.
pub const EXTRACTION_SECTION: &str = "markdown:section-heading";
pub const EXTRACTION_PREAMBLE: &str = "markdown:preamble";
pub const EXTRACTION_DOCUMENT: &str = "markdown:document";
pub const EXTRACTION_TEXT: &str = "text:document";

#[derive(Debug, Clone, PartialEq)]
pub struct Fragment {
    pub kind: EvidenceKind,
    pub heading_path: Option<String>,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
    pub content: String,
    pub subject: String,
    pub extraction: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedMarkdown {
    pub title: String,
    pub fragments: Vec<Fragment>,
}

struct Section {
    leaf: String,
    path: String,
    content_start: usize,
    heading_start: usize,
}

/// Parses markdown into (title, fragments).
///
/// Title is the text of the first heading of any level, falling back to
/// `fallback_title`. Section subjects are the leaf heading texts; the
/// preamble subject is the title; a heading-less document's subject is the
/// fallback title.
pub fn parse_markdown(text: &str, fallback_title: &str) -> ParsedMarkdown {
    let line_starts = line_starts(text);
    let mut fragments: Vec<Fragment> = Vec::new();
    let mut title: Option<String> = None;
    let mut stack: Vec<(HeadingLevel, String)> = Vec::new();
    let mut current: Option<Section> = None;
    let mut pending: Option<(HeadingLevel, usize, String)> = None;
    let mut first_heading_start: Option<usize> = None;

    for (ev, range) in Parser::new(text).into_offset_iter() {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                if let Some(section) = current.take() {
                    close_section(text, &line_starts, section, range.start, &mut fragments);
                }
                if first_heading_start.is_none() {
                    first_heading_start = Some(range.start);
                }
                pending = Some((level, range.start, String::new()));
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, start, txt)) = pending.take() {
                    let txt = txt.split_whitespace().collect::<Vec<_>>().join(" ");
                    if title.is_none() {
                        title = Some(txt.clone());
                    }
                    while stack.last().is_some_and(|(l, _)| *l >= level) {
                        stack.pop();
                    }
                    stack.push((level, txt.clone()));
                    let path = stack
                        .iter()
                        .map(|(_, t)| t.as_str())
                        .collect::<Vec<_>>()
                        .join(" > ");
                    current = Some(Section {
                        leaf: txt,
                        path,
                        content_start: range.end,
                        heading_start: start,
                    });
                }
            }
            Event::Text(t) | Event::Code(t) => {
                if let Some((_, _, buf)) = pending.as_mut() {
                    buf.push_str(&t);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some((_, _, buf)) = pending.as_mut() {
                    buf.push(' ');
                }
            }
            _ => {}
        }
    }
    if let Some(section) = current.take() {
        close_section(text, &line_starts, section, text.len(), &mut fragments);
    }

    // Preamble: text before the first heading. Without headings the whole
    // file is one document fragment.
    match first_heading_start {
        Some(first) => {
            let preamble = &text[..first];
            if !preamble.trim().is_empty() {
                let subject = title.clone().unwrap_or_else(|| fallback_title.to_string());
                fragments.insert(
                    0,
                    Fragment {
                        kind: EvidenceKind::Document,
                        heading_path: None,
                        start_line: Some(1),
                        end_line: Some(line_of(&line_starts, last_non_ws(preamble))),
                        content: preamble.trim().to_string(),
                        subject,
                        extraction: EXTRACTION_PREAMBLE,
                    },
                );
            }
        }
        None => {
            if !text.trim().is_empty() {
                fragments.push(Fragment {
                    kind: EvidenceKind::Document,
                    heading_path: None,
                    start_line: Some(1),
                    end_line: Some(line_of(&line_starts, last_non_ws(text))),
                    content: text.trim().to_string(),
                    subject: fallback_title.to_string(),
                    extraction: EXTRACTION_DOCUMENT,
                });
            }
        }
    }

    ParsedMarkdown {
        title: title.unwrap_or_else(|| fallback_title.to_string()),
        fragments,
    }
}

/// A plain-text file is one document fragment.
pub fn parse_text(text: &str, subject: &str) -> Vec<Fragment> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let line_starts = line_starts(text);
    vec![Fragment {
        kind: EvidenceKind::Document,
        heading_path: None,
        start_line: Some(1),
        end_line: Some(line_of(&line_starts, last_non_ws(text))),
        content: text.trim().to_string(),
        subject: subject.to_string(),
        extraction: EXTRACTION_TEXT,
    }]
}

fn close_section(
    text: &str,
    line_starts: &[usize],
    section: Section,
    end_byte: usize,
    fragments: &mut Vec<Fragment>,
) {
    let raw = &text[section.content_start..end_byte];
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return;
    }
    let end_line = raw
        .char_indices()
        .rev()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(i, _)| line_of(line_starts, section.content_start + i));
    fragments.push(Fragment {
        kind: EvidenceKind::Section,
        heading_path: Some(section.path),
        start_line: Some(line_of(line_starts, section.heading_start)),
        end_line,
        content: trimmed.to_string(),
        subject: section.leaf,
        extraction: EXTRACTION_SECTION,
    });
}

/// Absolute byte offset of the last non-whitespace character in `text`.
fn last_non_ws(text: &str) -> usize {
    text.char_indices()
        .rev()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// 1-based line number of `byte` within the document.
fn line_of(line_starts: &[usize], byte: usize) -> u32 {
    line_starts.partition_point(|&p| p <= byte) as u32
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subjects(p: &ParsedMarkdown) -> Vec<String> {
        p.fragments.iter().map(|f| f.subject.clone()).collect()
    }

    #[test]
    fn splits_flat_sections_with_paths() {
        let md = "# Top\n\ntop text\n\n## Sub\n\nsub text\n\n# Other\n\nother text\n";
        let p = parse_markdown(md, "stem");
        assert_eq!(p.title, "Top");
        assert_eq!(subjects(&p), vec!["Top", "Sub", "Other"]);
        assert_eq!(p.fragments[0].heading_path.as_deref(), Some("Top"));
        assert_eq!(p.fragments[1].heading_path.as_deref(), Some("Top > Sub"));
        assert_eq!(p.fragments[2].heading_path.as_deref(), Some("Other"));
        assert_eq!(p.fragments[1].content, "sub text");
    }

    #[test]
    fn section_content_runs_until_next_heading_of_any_level() {
        let md = "# A\n\nline one\n\n### Deep\n\ndeep text\n\nmore\n\n# B\n\nb text\n";
        let p = parse_markdown(md, "stem");
        assert_eq!(subjects(&p), vec!["A", "Deep", "B"]);
        assert_eq!(p.fragments[0].content, "line one");
        assert_eq!(p.fragments[1].content, "deep text\n\nmore");
        assert_eq!(p.fragments[1].heading_path.as_deref(), Some("A > Deep"));
    }

    #[test]
    fn preamble_becomes_document_fragment() {
        let md = "Intro paragraph about the document.\n\n# Head\n\nbody\n";
        let p = parse_markdown(md, "stem");
        assert_eq!(p.fragments.len(), 2);
        assert_eq!(p.fragments[0].kind, EvidenceKind::Document);
        assert_eq!(p.fragments[0].heading_path, None);
        assert_eq!(p.fragments[0].subject, "Head"); // preamble subject = title
        assert_eq!(
            p.fragments[0].content,
            "Intro paragraph about the document."
        );
    }

    #[test]
    fn no_headings_yields_single_document() {
        let md = "just a plain markdown file\nwith two lines\n";
        let p = parse_markdown(md, "plain-stem");
        assert_eq!(p.title, "plain-stem");
        assert_eq!(p.fragments.len(), 1);
        assert_eq!(p.fragments[0].subject, "plain-stem");
        assert_eq!(p.fragments[0].extraction, EXTRACTION_DOCUMENT);
    }

    #[test]
    fn code_fences_stay_in_section_content() {
        let md = "# Code\n\n```rust\nfn main() {}\n```\n\nafter\n";
        let p = parse_markdown(md, "stem");
        assert_eq!(p.fragments.len(), 1);
        assert!(p.fragments[0].content.contains("fn main() {}"));
        assert!(p.fragments[0].content.contains("after"));
    }

    #[test]
    fn setext_headings_are_headings() {
        let md = "Title\n=====\n\nbody\n";
        let p = parse_markdown(md, "stem");
        assert_eq!(p.title, "Title");
        assert_eq!(subjects(&p), vec!["Title"]);
        assert_eq!(p.fragments[0].content, "body");
    }

    #[test]
    fn duplicate_headings_give_two_fragments_same_subject() {
        let md = "# Note\n\nfirst\n\n# Note\n\nsecond\n";
        let p = parse_markdown(md, "stem");
        assert_eq!(p.fragments.len(), 2);
        assert_eq!(p.fragments[0].subject, "Note");
        assert_eq!(p.fragments[1].subject, "Note");
        assert_ne!(p.fragments[0].content, p.fragments[1].content);
    }

    #[test]
    fn empty_sections_are_skipped() {
        let md = "# Empty\n\n# Full\n\ncontent\n";
        let p = parse_markdown(md, "stem");
        assert_eq!(p.fragments.len(), 1);
        assert_eq!(p.fragments[0].subject, "Full");
    }

    #[test]
    fn line_numbers_are_one_based() {
        let md = "# A\n\ntext\n";
        let p = parse_markdown(md, "stem");
        assert_eq!(p.fragments[0].start_line, Some(1));
        assert_eq!(p.fragments[0].end_line, Some(3));
    }

    #[test]
    fn text_file_is_one_fragment() {
        let frags = parse_text("plain\nlines\n", "notes");
        assert_eq!(frags.len(), 1);
        assert_eq!(frags[0].kind, EvidenceKind::Document);
        assert_eq!(frags[0].subject, "notes");
        assert_eq!(frags[0].extraction, EXTRACTION_TEXT);
        assert_eq!(frags[0].end_line, Some(2));
    }

    #[test]
    fn inline_code_in_heading_text_is_kept() {
        let md = "# Use `mind` daily\n\nbody\n";
        let p = parse_markdown(md, "stem");
        assert_eq!(p.title, "Use mind daily");
    }
}
