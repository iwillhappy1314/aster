use std::ops::Range;

/// Describes one complex Markdown component that can replace source in Live Markdown mode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComplexMarkdownComponent {
    pub range: Range<usize>,
    pub kind: ComplexMarkdownKind,
}

/// Contains one table cell's visible content and exact source byte range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownTableCell {
    pub text: String,
    pub source_range: Range<usize>,
}

/// Contains the presentation data needed to render one complex Markdown component.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ComplexMarkdownKind {
    Image {
        alt: String,
        target: String,
    },
    Table {
        headers: Vec<MarkdownTableCell>,
        rows: Vec<Vec<MarkdownTableCell>>,
    },
    Callout {
        kind: String,
        title: String,
        body: String,
    },
    Footnote {
        label: String,
        text: Option<String>,
    },
    WikiLink {
        label: String,
        target: String,
    },
    Math {
        formula: String,
        block: bool,
    },
    Mermaid {
        source: String,
    },
}

/// Tracks one source line without allocating a second copy of the document.
#[derive(Clone, Debug)]
struct SourceLine<'a> {
    start: usize,
    end: usize,
    content: &'a str,
}

/// Parses complex Markdown components into immutable, source-relative ranges.
pub fn complex_markdown_components(source: &str) -> Vec<ComplexMarkdownComponent> {
    let lines = source_lines(source);
    let mut components = Vec::new();
    let mut excluded_ranges = Vec::new();
    let mut line_index = 0usize;

    while line_index < lines.len() {
        let line = &lines[line_index];
        let trimmed = line.content.trim();

        if let Some((marker, marker_len, language)) = fence_opening(trimmed) {
            let closing = find_fence_closing(&lines, line_index + 1, marker, marker_len);
            let end_index = closing.unwrap_or(lines.len().saturating_sub(1));
            let range = line.start..lines[end_index].end;
            if language.eq_ignore_ascii_case("mermaid") {
                let content_start = line.end;
                let content_end = closing
                    .map(|index| lines[index].start)
                    .unwrap_or(source.len());
                components.push(ComplexMarkdownComponent {
                    range: range.clone(),
                    kind: ComplexMarkdownKind::Mermaid {
                        source: source[content_start.min(content_end)..content_end]
                            .trim_end_matches(['\n', '\r'])
                            .to_string(),
                    },
                });
            }
            excluded_ranges.push(range);
            line_index = end_index.saturating_add(1);
            continue;
        }

        if trimmed == "$$" {
            let closing =
                (line_index + 1..lines.len()).find(|index| lines[*index].content.trim() == "$$");
            let end_index = closing.unwrap_or(lines.len().saturating_sub(1));
            let content_start = line.end;
            let content_end = closing
                .map(|index| lines[index].start)
                .unwrap_or(source.len());
            let range = line.start..lines[end_index].end;
            components.push(ComplexMarkdownComponent {
                range: range.clone(),
                kind: ComplexMarkdownKind::Math {
                    formula: source[content_start.min(content_end)..content_end]
                        .trim_end_matches(['\n', '\r'])
                        .to_string(),
                    block: true,
                },
            });
            excluded_ranges.push(range);
            line_index = end_index.saturating_add(1);
            continue;
        }

        if let Some((kind, title)) = callout_header(trimmed) {
            let mut end_index = line_index;
            while end_index + 1 < lines.len()
                && lines[end_index + 1].content.trim_start().starts_with('>')
            {
                end_index += 1;
            }
            let body = lines[line_index + 1..=end_index]
                .iter()
                .map(|line| {
                    line.content
                        .trim_start()
                        .strip_prefix('>')
                        .unwrap_or(line.content)
                        .trim_start()
                })
                .collect::<Vec<_>>()
                .join("\n");
            let range = line.start..lines[end_index].end;
            components.push(ComplexMarkdownComponent {
                range: range.clone(),
                kind: ComplexMarkdownKind::Callout { kind, title, body },
            });
            excluded_ranges.push(range);
            line_index = end_index.saturating_add(1);
            continue;
        }

        if line_index + 1 < lines.len()
            && looks_like_table_row(line.content)
            && is_table_separator(lines[line_index + 1].content)
        {
            let mut end_index = line_index + 1;
            while end_index + 1 < lines.len() && looks_like_table_row(lines[end_index + 1].content)
            {
                end_index += 1;
            }
            let headers = parse_table_cells(line.content, line.start);
            let rows = if end_index >= line_index + 2 {
                lines[line_index + 2..=end_index]
                    .iter()
                    .map(|line| parse_table_cells(line.content, line.start))
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let range = line.start..lines[end_index].end;
            components.push(ComplexMarkdownComponent {
                range: range.clone(),
                kind: ComplexMarkdownKind::Table { headers, rows },
            });
            excluded_ranges.push(range);
            line_index = end_index.saturating_add(1);
            continue;
        }

        if let Some((alt, target)) = parse_standalone_image(trimmed) {
            let range = line.start..line.end;
            components.push(ComplexMarkdownComponent {
                range: range.clone(),
                kind: ComplexMarkdownKind::Image { alt, target },
            });
            excluded_ranges.push(range);
            line_index += 1;
            continue;
        }

        if let Some((label, text)) = parse_footnote_definition(trimmed) {
            let range = line.start..line.end;
            components.push(ComplexMarkdownComponent {
                range: range.clone(),
                kind: ComplexMarkdownKind::Footnote {
                    label,
                    text: Some(text),
                },
            });
            excluded_ranges.push(range);
        }

        line_index += 1;
    }

    for line in &lines {
        if range_is_excluded(line.start..line.end, &excluded_ranges) {
            continue;
        }
        scan_inline_components(line, &mut components);
    }

    components.sort_by_key(|component| (component.range.start, component.range.end));
    components
}

/// Splits a document into byte-addressable lines, retaining line terminators in each range.
fn source_lines(source: &str) -> Vec<SourceLine<'_>> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for raw_line in source.split_inclusive('\n') {
        let content = raw_line.trim_end_matches('\n').trim_end_matches('\r');
        lines.push(SourceLine {
            start,
            end: start + raw_line.len(),
            content,
        });
        start += raw_line.len();
    }
    if source.is_empty() || start < source.len() || source.ends_with('\n') {
        lines.push(SourceLine {
            start,
            end: source.len(),
            content: &source[start..],
        });
    }
    lines
}

/// Recognizes a Markdown fence and returns its marker, length and language label.
fn fence_opening(line: &str) -> Option<(u8, usize, &str)> {
    let bytes = line.as_bytes();
    let marker = *bytes.first()?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let marker_len = bytes.iter().take_while(|byte| **byte == marker).count();
    (marker_len >= 3).then(|| (marker, marker_len, line[marker_len..].trim()))
}

/// Finds the matching closing fence without interpreting its body as Markdown.
fn find_fence_closing(
    lines: &[SourceLine<'_>],
    start: usize,
    marker: u8,
    marker_len: usize,
) -> Option<usize> {
    (start..lines.len()).find(|index| {
        let trimmed = lines[*index].content.trim();
        let bytes = trimmed.as_bytes();
        bytes.first() == Some(&marker)
            && bytes.iter().take_while(|byte| **byte == marker).count() >= marker_len
    })
}

/// Parses an Obsidian-style callout header such as `> [!NOTE] Optional title`.
fn callout_header(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix('>')?.trim_start().strip_prefix("[!")?;
    let close = rest.find(']')?;
    let kind = rest[..close].trim();
    if kind.is_empty() {
        return None;
    }
    let title = rest[close + 1..].trim();
    Some((
        kind.to_ascii_uppercase(),
        if title.is_empty() {
            kind.to_ascii_lowercase()
        } else {
            title.to_string()
        },
    ))
}

/// Returns whether a line is a plausible GFM pipe-table row.
fn looks_like_table_row(line: &str) -> bool {
    line.contains('|') && parse_table_cells(line, 0).len() >= 2
}

/// Validates the table delimiter row while allowing alignment colons and whitespace.
fn is_table_separator(line: &str) -> bool {
    let cells = parse_table_cells(line, 0);
    cells.len() >= 2
        && cells.iter().all(|cell| {
            let trimmed = cell.text.trim_matches(':');
            trimmed.len() >= 3 && trimmed.bytes().all(|byte| byte == b'-')
        })
}

/// Parses table cells without splitting escaped pipes or pipes inside inline code.
fn parse_table_cells(line: &str, line_start: usize) -> Vec<MarkdownTableCell> {
    let content_start = line.len() - line.trim_start().len();
    let content_end = line.trim_end().len();
    let mut start = content_start;
    let mut end = content_end;
    if line[start..end].starts_with('|') {
        start += 1;
    }
    if start < end && line[start..end].ends_with('|') {
        end -= 1;
    }
    let mut cells = Vec::new();
    let mut escaped = false;
    let mut in_code = false;
    let mut cell_start = start;
    for (offset, character) in line[start..end].char_indices() {
        let byte = start + offset;
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '`' {
            in_code = !in_code;
        } else if character == '|' && !in_code {
            cells.push(table_cell(line, line_start, cell_start..byte));
            cell_start = byte + character.len_utf8();
        }
    }
    cells.push(table_cell(line, line_start, cell_start..end));
    cells
}

/// Trims cell padding while retaining a source range suitable for local replacement.
fn table_cell(line: &str, line_start: usize, range: Range<usize>) -> MarkdownTableCell {
    let value = &line[range.clone()];
    let source_text = value.trim();
    let leading = value.len() - value.trim_start().len();
    let content_start = range.start + leading;
    let content_end = content_start + source_text.len();
    let source_range = (line_start + content_start)..(line_start + content_end);
    MarkdownTableCell {
        text: display_table_cell_value(source_text),
        source_range,
    }
}

/// Converts structural pipe escapes to the content shown inside a rendered cell.
fn display_table_cell_value(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\\' && characters.peek() == Some(&'|') {
            result.push(characters.next().expect("escaped pipe"));
        } else {
            result.push(character);
        }
    }
    result
}

/// Serializes one edited table cell without allowing it to change table structure.
pub fn serialize_table_cell_value(value: &str) -> String {
    let value = value.replace(['\n', '\r'], " ");
    let mut result = String::with_capacity(value.len());
    let mut escaped = false;
    let mut in_code = false;
    for character in value.chars() {
        if escaped {
            result.push(character);
            escaped = false;
        } else if character == '\\' {
            result.push(character);
            escaped = true;
        } else if character == '`' {
            result.push(character);
            in_code = !in_code;
        } else if character == '|' && !in_code {
            result.push_str("\\|");
        } else {
            result.push(character);
        }
    }
    result
}

/// Parses a standalone Markdown image while keeping unsupported syntax in source form.
fn parse_standalone_image(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("![")?;
    let delimiter = rest.find("](")?;
    if !rest.ends_with(')') {
        return None;
    }
    let alt = rest[..delimiter].replace("\\]", "]");
    let destination = rest[delimiter + 2..rest.len() - 1].trim();
    let target = if let Some(target) = destination.strip_prefix('<') {
        target.strip_suffix('>')?.to_string()
    } else {
        destination.split_ascii_whitespace().next()?.to_string()
    };
    (!target.is_empty()).then_some((alt, target))
}

/// Parses a footnote definition and leaves continuation lines as ordinary source.
fn parse_footnote_definition(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("[^")?;
    let delimiter = rest.find("]:")?;
    let label = rest[..delimiter].trim();
    (!label.is_empty()).then(|| (label.to_string(), rest[delimiter + 2..].trim().to_string()))
}

/// Scans wiki links, footnote references and inline math outside block components.
fn scan_inline_components(line: &SourceLine<'_>, components: &mut Vec<ComplexMarkdownComponent>) {
    let bytes = line.content.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"[[")
            && let Some(close) = line.content[index + 2..].find("]]")
        {
            let end = index + 2 + close;
            let inner = &line.content[index + 2..end];
            let (target, label) = inner
                .split_once('|')
                .map_or((inner, inner), |(target, label)| (target, label));
            if !target.trim().is_empty() {
                components.push(ComplexMarkdownComponent {
                    range: (line.start + index)..(line.start + end + 2),
                    kind: ComplexMarkdownKind::WikiLink {
                        label: label.trim().to_string(),
                        target: target.trim().to_string(),
                    },
                });
            }
            index = end + 2;
            continue;
        }

        if bytes[index..].starts_with(b"[^")
            && let Some(close) = line.content[index + 2..].find(']')
        {
            let end = index + 2 + close;
            let label = line.content[index + 2..end].trim();
            if !label.is_empty() {
                components.push(ComplexMarkdownComponent {
                    range: (line.start + index)..(line.start + end + 1),
                    kind: ComplexMarkdownKind::Footnote {
                        label: label.to_string(),
                        text: None,
                    },
                });
            }
            index = end + 1;
            continue;
        }

        if bytes[index] == b'$'
            && bytes.get(index + 1) != Some(&b'$')
            && let Some(close) = line.content[index + 1..].find('$')
        {
            let end = index + 1 + close;
            let formula = line.content[index + 1..end].trim();
            if !formula.is_empty() {
                components.push(ComplexMarkdownComponent {
                    range: (line.start + index)..(line.start + end + 1),
                    kind: ComplexMarkdownKind::Math {
                        formula: formula.to_string(),
                        block: false,
                    },
                });
            }
            index = end + 1;
            continue;
        }

        index += 1;
    }
}

/// Returns whether a candidate inline range overlaps a previously parsed block component.
fn range_is_excluded(range: Range<usize>, excluded: &[Range<usize>]) -> bool {
    excluded
        .iter()
        .any(|item| range.start < item.end && item.start < range.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_images_tables_and_escaped_pipes() {
        let source = "![Logo](images/logo.png)\n\n| Name | Value |\n| --- | ---: |\n| pipe | `a|b` |\n| escaped | a\\|b |\n";
        let components = complex_markdown_components(source);

        assert!(matches!(
            &components[0].kind,
            ComplexMarkdownKind::Image { target, .. } if target == "images/logo.png"
        ));
        assert!(matches!(
            &components[1].kind,
            ComplexMarkdownKind::Table { headers, rows }
                if headers.len() == 2 && rows.len() == 2 && rows[0][1].text == "`a|b`"
        ));
    }

    #[test]
    fn table_cells_retain_exact_editable_source_ranges() {
        let source = "  | Name | 值 |\n| --- | --- |\n| alpha | a\\|b |\n";
        let components = complex_markdown_components(source);
        let ComplexMarkdownKind::Table { headers, rows } = &components[0].kind else {
            panic!("expected table");
        };

        assert_eq!(&source[headers[0].source_range.clone()], "Name");
        assert_eq!(&source[headers[1].source_range.clone()], "值");
        assert_eq!(&source[rows[0][1].source_range.clone()], "a\\|b");
        assert_eq!(rows[0][1].text, "a|b");
    }

    /// Verifies that a whitespace-only table cell remains editable without panicking.
    #[test]
    fn whitespace_only_table_cell_has_empty_content_and_source_range() {
        let source = "| Name | Value |\n| --- | --- |\n| test |   |\n";
        let components = complex_markdown_components(source);
        let ComplexMarkdownKind::Table { rows, .. } = &components[0].kind else {
            panic!("expected table");
        };
        let empty_cell = &rows[0][1];

        assert_eq!(
            (
                empty_cell.text.as_str(),
                &source[empty_cell.source_range.clone()]
            ),
            ("", "")
        );
    }

    #[test]
    fn table_cell_serialization_preserves_inline_code_and_escapes_structure_pipes() {
        assert_eq!(
            serialize_table_cell_value("left | `code|pipe`\nright"),
            "left \\| `code|pipe` right"
        );
    }

    #[test]
    fn header_only_table_is_safe_and_has_no_body_rows() {
        let source = "| Name | Value |\n| --- | --- |\n";
        let components = complex_markdown_components(source);

        assert!(matches!(
            &components[0].kind,
            ComplexMarkdownKind::Table { rows, .. } if rows.is_empty()
        ));
    }

    #[test]
    fn parses_callouts_footnotes_wiki_links_and_math() {
        let source = "> [!NOTE] Read this\n> Callout body\n\nSee [[guide.md|Guide]] and [^one] with $x^2$.\n[^one]: Footnote text\n\n$$\na^2+b^2=c^2\n$$\n";
        let components = complex_markdown_components(source);

        assert!(components.iter().any(|component| matches!(
            &component.kind,
            ComplexMarkdownKind::Callout { kind, .. } if kind == "NOTE"
        )));
        assert!(components.iter().any(|component| matches!(
            &component.kind,
            ComplexMarkdownKind::WikiLink { target, .. } if target == "guide.md"
        )));
        assert_eq!(
            components
                .iter()
                .filter(|component| matches!(component.kind, ComplexMarkdownKind::Footnote { .. }))
                .count(),
            2
        );
        assert_eq!(
            components
                .iter()
                .filter(|component| matches!(component.kind, ComplexMarkdownKind::Math { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn mermaid_fences_are_components_and_other_fences_are_excluded() {
        let source = "```mermaid\ngraph TD\nA-->B\n```\n```md\n[[not-a-link]]\n```\n";
        let components = complex_markdown_components(source);

        assert_eq!(components.len(), 1);
        assert!(matches!(
            &components[0].kind,
            ComplexMarkdownKind::Mermaid { source } if source.contains("A-->B")
        ));
    }

    #[test]
    fn malformed_components_remain_unparsed_source() {
        let source = "![missing](\n| no | separator |\n[[broken\n$broken\n";
        assert!(complex_markdown_components(source).is_empty());
    }
}
