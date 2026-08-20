use std::ops::Range;

/// Semantic token categories for lightweight Markdown syntax highlighting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxKind {
    HeadingMarker,
    HeadingText,
    QuoteMarker,
    ListMarker,
    TaskMarker,
    CodeFence,
    InlineCodeMarker,
    InlineCode,
    LinkTextDelimiter,
    LinkText,
    LinkUrlDelimiter,
    LinkUrl,
    EmphasisMarker,
    EmphasisText,
    StrongText,
}

#[derive(Clone, Debug)]
pub struct SyntaxSpan {
    pub range: Range<usize>,
    pub kind: SyntaxKind,
}

/// Scans Markdown source and returns byte-range spans for syntax highlighting.
///
/// The scanner is intentionally lightweight and single-pass by line to keep
/// editor rendering responsive for large files.
pub fn markdown_spans(source: &str) -> Vec<SyntaxSpan> {
    let mut spans = Vec::new();
    let mut offset = 0usize;
    let mut active_fence: Option<(u8, usize)> = None;

    for raw_line in source.split_inclusive('\n') {
        let line = raw_line.trim_end_matches('\n').trim_end_matches('\r');
        let line_len = line.len();
        if line_len == 0 {
            offset += raw_line.len();
            continue;
        }

        let leading = leading_whitespace_bytes(line);
        let content = &line[leading..];
        let content_start = offset + leading;

        if let Some((active_marker, active_len)) = active_fence {
            if let Some((fence_marker, fence_len)) = fence_prefix(content)
                && fence_marker == active_marker
                && fence_len >= active_len
            {
                push_code_fence_spans(&mut spans, content_start, content, fence_len);
                active_fence = None;
            }
        } else if let Some((fence_marker, fence_len)) = fence_prefix(content) {
            active_fence = Some((fence_marker, fence_len));
            push_code_fence_spans(&mut spans, content_start, content, fence_len);
        } else {
            if let Some((marker_len, _hashes)) = heading_prefix(content) {
                spans.push(SyntaxSpan {
                    range: content_start..(content_start + marker_len),
                    kind: SyntaxKind::HeadingMarker,
                });
                let text_start = content_start + marker_len;
                if text_start < content_start + content.len() {
                    spans.push(SyntaxSpan {
                        range: text_start..(content_start + content.len()),
                        kind: SyntaxKind::HeadingText,
                    });
                }
            }

            if content.starts_with('>') {
                spans.push(SyntaxSpan {
                    range: content_start..(content_start + 1),
                    kind: SyntaxKind::QuoteMarker,
                });
            }

            if let Some(marker_len) = task_marker_len(content) {
                spans.push(SyntaxSpan {
                    range: content_start..(content_start + marker_len),
                    kind: SyntaxKind::TaskMarker,
                });
            } else if let Some(marker_len) = list_marker_len(content) {
                spans.push(SyntaxSpan {
                    range: content_start..(content_start + marker_len),
                    kind: SyntaxKind::ListMarker,
                });
            }

            scan_inline(line, offset, &mut spans);
        }

        offset += raw_line.len();
    }

    spans
}

/// Returns the ATX heading level for each source line outside fenced code blocks.
///
/// The result preserves one entry per `split('\n')` line so it can be paired with
/// the editor's line layout without remapping byte offsets.
pub fn markdown_heading_levels(source: &str) -> Vec<Option<u8>> {
    let mut active_fence: Option<(u8, usize)> = None;

    source
        .split('\n')
        .map(|raw_line| {
            let line = raw_line.trim_end_matches('\r');
            let leading = leading_whitespace_bytes(line);
            let content = &line[leading..];

            if let Some((fence_marker, fence_len)) = fence_prefix(content) {
                if let Some((active_marker, active_len)) = active_fence
                    && fence_marker == active_marker
                    && fence_len >= active_len
                {
                    active_fence = None;
                } else if active_fence.is_none() {
                    active_fence = Some((fence_marker, fence_len));
                }
                None
            } else if active_fence.is_some() {
                None
            } else {
                heading_prefix(content).map(|(_, hashes)| hashes as u8)
            }
        })
        .collect()
}

/// Returns the ATX heading level (1–6) for a single Markdown source line.
/// Leading indentation and trailing line endings are ignored.
pub fn markdown_heading_level(raw_line: &str) -> Option<u8> {
    let line = raw_line.trim_end_matches('\n').trim_end_matches('\r');
    let leading = leading_whitespace_bytes(line);
    let content = &line[leading..];
    heading_prefix(content).map(|(_, hashes)| hashes as u8)
}

fn leading_whitespace_bytes(line: &str) -> usize {
    line.char_indices()
        .find_map(|(idx, ch)| if ch.is_whitespace() { None } else { Some(idx) })
        .unwrap_or(line.len())
}

fn fence_prefix(content: &str) -> Option<(u8, usize)> {
    let marker = *content.as_bytes().first()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }

    let length = content.bytes().take_while(|byte| *byte == marker).count();
    (length >= 3).then_some((marker, length))
}

/// Marks a fenced code delimiter line and its optional language identifier.
fn push_code_fence_spans(
    spans: &mut Vec<SyntaxSpan>,
    content_start: usize,
    content: &str,
    fence_len: usize,
) {
    spans.push(SyntaxSpan {
        range: content_start..(content_start + fence_len),
        kind: SyntaxKind::CodeFence,
    });
    if content.len() > fence_len {
        spans.push(SyntaxSpan {
            range: (content_start + fence_len)..(content_start + content.len()),
            kind: SyntaxKind::CodeFence,
        });
    }
}

fn heading_prefix(content: &str) -> Option<(usize, usize)> {
    let bytes = content.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i] == b'#' {
        i += 1;
    }
    let hash_count = i;
    if hash_count == 0 || hash_count > 6 {
        return None;
    }
    if i < bytes.len() && bytes[i].is_ascii_whitespace() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        return Some((i, hash_count));
    }
    None
}

fn task_marker_len(content: &str) -> Option<usize> {
    let bytes = content.as_bytes();
    if bytes.len() < 6 {
        return None;
    }
    if (bytes[0] == b'-' || bytes[0] == b'*' || bytes[0] == b'+')
        && bytes[1] == b' '
        && bytes[2] == b'['
        && (bytes[3] == b' ' || bytes[3] == b'x' || bytes[3] == b'X')
        && bytes[4] == b']'
        && bytes[5].is_ascii_whitespace()
    {
        return Some(6);
    }
    None
}

fn list_marker_len(content: &str) -> Option<usize> {
    let bytes = content.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'-' || bytes[0] == b'*' || bytes[0] == b'+')
        && bytes[1].is_ascii_whitespace()
    {
        return Some(2);
    }

    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1].is_ascii_whitespace() {
        return Some(i + 2);
    }
    None
}

fn scan_inline(line: &str, line_start: usize, spans: &mut Vec<SyntaxSpan>) {
    let bytes = line.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'`' => {
                if let Some(close) = find_next_byte(bytes, i + 1, b'`') {
                    spans.push(SyntaxSpan {
                        range: (line_start + i)..(line_start + i + 1),
                        kind: SyntaxKind::InlineCodeMarker,
                    });
                    if close > i + 1 {
                        spans.push(SyntaxSpan {
                            range: (line_start + i + 1)..(line_start + close),
                            kind: SyntaxKind::InlineCode,
                        });
                    }
                    spans.push(SyntaxSpan {
                        range: (line_start + close)..(line_start + close + 1),
                        kind: SyntaxKind::InlineCodeMarker,
                    });
                    i = close + 1;
                    continue;
                }
            }
            b'[' => {
                if let Some(close_bracket) = find_next_byte(bytes, i + 1, b']') {
                    let open_paren = close_bracket + 1;
                    if open_paren < bytes.len()
                        && bytes[open_paren] == b'('
                        && let Some(close_paren) = find_next_byte(bytes, open_paren + 1, b')')
                    {
                        spans.push(SyntaxSpan {
                            range: (line_start + i)..(line_start + i + 1),
                            kind: SyntaxKind::LinkTextDelimiter,
                        });
                        if close_bracket > i + 1 {
                            spans.push(SyntaxSpan {
                                range: (line_start + i + 1)..(line_start + close_bracket),
                                kind: SyntaxKind::LinkText,
                            });
                        }
                        spans.push(SyntaxSpan {
                            range: (line_start + close_bracket)..(line_start + close_bracket + 1),
                            kind: SyntaxKind::LinkTextDelimiter,
                        });
                        spans.push(SyntaxSpan {
                            range: (line_start + open_paren)..(line_start + open_paren + 1),
                            kind: SyntaxKind::LinkUrlDelimiter,
                        });
                        if close_paren > open_paren + 1 {
                            spans.push(SyntaxSpan {
                                range: (line_start + open_paren + 1)..(line_start + close_paren),
                                kind: SyntaxKind::LinkUrl,
                            });
                        }
                        spans.push(SyntaxSpan {
                            range: (line_start + close_paren)..(line_start + close_paren + 1),
                            kind: SyntaxKind::LinkUrlDelimiter,
                        });
                        i = close_paren + 1;
                        continue;
                    }
                }
            }
            b'*' | b'_' => {
                let marker = bytes[i];
                let marker_len = if i + 1 < bytes.len() && bytes[i + 1] == marker {
                    2
                } else {
                    1
                };
                if let Some(close) = find_emphasis_close(bytes, i + marker_len, marker, marker_len)
                {
                    spans.push(SyntaxSpan {
                        range: (line_start + i)..(line_start + i + marker_len),
                        kind: SyntaxKind::EmphasisMarker,
                    });
                    if close > i + marker_len {
                        spans.push(SyntaxSpan {
                            range: (line_start + i + marker_len)..(line_start + close),
                            kind: if marker_len == 2 {
                                SyntaxKind::StrongText
                            } else {
                                SyntaxKind::EmphasisText
                            },
                        });
                    }
                    spans.push(SyntaxSpan {
                        range: (line_start + close)..(line_start + close + marker_len),
                        kind: SyntaxKind::EmphasisMarker,
                    });
                    i = close + marker_len;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
}

fn find_next_byte(bytes: &[u8], start: usize, needle: u8) -> Option<usize> {
    bytes[start..]
        .iter()
        .position(|b| *b == needle)
        .map(|pos| start + pos)
}

fn find_emphasis_close(bytes: &[u8], start: usize, marker: u8, marker_len: usize) -> Option<usize> {
    let mut i = start;
    while i + marker_len <= bytes.len() {
        if bytes[i] == marker {
            if marker_len == 1 {
                return Some(i);
            }
            if i + 1 < bytes.len() && bytes[i + 1] == marker {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_are_always_utf8_boundary_aligned() {
        let source = r#"# Launch

dn’t require a patchwork of vendors.

- [x] task item
> quote line
[Aster](https://example.com)
`inline`
"#;

        let spans = markdown_spans(source);
        assert!(!spans.is_empty());

        for span in spans {
            assert!(span.range.start <= span.range.end);
            assert!(span.range.end <= source.len());
            assert!(source.is_char_boundary(span.range.start));
            assert!(source.is_char_boundary(span.range.end));
        }
    }

    #[test]
    fn scanner_detects_core_markdown_tokens() {
        let source =
            "# Heading\n- [x] Done\n[Link](https://example.com)\n`code`\n*italic* **bold**\n";
        let spans = markdown_spans(source);

        assert!(spans.iter().any(|s| s.kind == SyntaxKind::HeadingMarker));
        assert!(spans.iter().any(|s| s.kind == SyntaxKind::TaskMarker));
        assert!(
            spans
                .iter()
                .any(|s| s.kind == SyntaxKind::LinkTextDelimiter)
        );
        assert!(spans.iter().any(|s| s.kind == SyntaxKind::LinkText));
        assert!(spans.iter().any(|s| s.kind == SyntaxKind::LinkUrlDelimiter));
        assert!(spans.iter().any(|s| s.kind == SyntaxKind::LinkUrl));
        assert!(spans.iter().any(|s| s.kind == SyntaxKind::InlineCodeMarker));
        assert!(spans.iter().any(|s| s.kind == SyntaxKind::InlineCode));
        assert!(spans.iter().any(|s| s.kind == SyntaxKind::EmphasisText));
        assert!(spans.iter().any(|s| s.kind == SyntaxKind::StrongText));
    }

    #[test]
    fn heading_level_matches_scanner_rules() {
        assert_eq!(markdown_heading_level("# H1"), Some(1));
        assert_eq!(markdown_heading_level("  ### H3\n"), Some(3));
        assert_eq!(markdown_heading_level("###### H6"), Some(6));
        assert_eq!(markdown_heading_level("####### not a heading"), None);
        assert_eq!(markdown_heading_level("#missing-space"), None);
    }

    #[test]
    fn code_block_comments_are_not_heading_tokens_or_layout_headings() {
        let source = "# Document\n\n```env\n# Mail configuration\nMAIL_HOST=localhost\n```\n\n## After code\n";
        let spans = markdown_spans(source);
        let code_comment_start = source.find("# Mail configuration").unwrap();

        assert!(!spans.iter().any(|span| {
            span.kind == SyntaxKind::HeadingMarker && span.range.start == code_comment_start
        }));
        assert_eq!(
            markdown_heading_levels(source),
            vec![Some(1), None, None, None, None, None, None, Some(2), None]
        );
    }

    #[test]
    fn tilde_code_block_comments_are_not_layout_headings() {
        let source = "~~~text\n# Not a heading\n~~~\n# Heading\n";

        assert_eq!(
            markdown_heading_levels(source),
            vec![None, None, None, Some(1), None]
        );
    }
}
