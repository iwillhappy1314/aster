use crate::services::complex_markdown::{ComplexMarkdownComponent, ComplexMarkdownKind};
use crate::services::syntax::{SyntaxKind, SyntaxSpan};
use std::cmp::Reverse;
use std::ops::Range;
use std::sync::Arc;

/// Determines which source boundary owns a collapsed display position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionAffinity {
    Left,
    Right,
}

/// Describes an interactive replacement rendered in place of Markdown source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectionAtom {
    Bullet,
    Task { checked: bool },
    HorizontalRule,
    Complex(ComplexMarkdownKind),
}

/// Describes how one source range appears in the editor.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ProjectionSegmentKind {
    Visible,
    Hidden,
    Replacement,
    Atom(ProjectionAtom),
}

/// Maps one contiguous source range to its display range.
#[derive(Clone, Debug)]
struct ProjectionSegment {
    source: Range<usize>,
    display: Range<usize>,
    kind: ProjectionSegmentKind,
}

/// Describes one validated source replacement used to construct a projection.
#[derive(Clone, Debug)]
struct ProjectionEdit {
    source: Range<usize>,
    replacement: String,
    atom: Option<ProjectionAtom>,
}

/// Maintains the rendered text and a total source/display position mapping.
#[derive(Clone, Debug)]
pub struct DisplayProjection {
    pub display_text: Arc<str>,
    source_len: usize,
    segments: Vec<ProjectionSegment>,
}

impl DisplayProjection {
    /// Creates a one-to-one projection used by Source mode and safe fallbacks.
    pub fn from_source(source: &str) -> Self {
        Self::from_shared_source(Arc::from(source))
    }

    /// Creates an identity projection without copying an already shared source buffer.
    pub fn from_shared_source(source: Arc<str>) -> Self {
        let source_len = source.len();
        Self {
            display_text: source,
            source_len,
            segments: vec![ProjectionSegment {
                source: 0..source_len,
                display: 0..source_len,
                kind: ProjectionSegmentKind::Visible,
            }],
        }
    }

    /// Creates a live Markdown projection while leaving active source lines editable.
    #[cfg(test)]
    pub fn from_markdown(
        source: &str,
        spans: &[SyntaxSpan],
        active_source_range: Range<usize>,
    ) -> Self {
        let edits = projection_edits(source, spans, &[], &active_source_range);
        Self::from_edits(source, edits)
    }

    /// Creates a live projection that includes revision-matched complex components.
    pub fn from_markdown_with_components(
        source: &str,
        spans: &[SyntaxSpan],
        components: &[ComplexMarkdownComponent],
        active_source_range: Range<usize>,
    ) -> Self {
        let edits = projection_edits(source, spans, components, &active_source_range);
        Self::from_edits(source, edits)
    }

    /// Maps a source byte to the corresponding display byte with right affinity.
    pub fn source_to_display_byte(&self, source_byte: usize) -> usize {
        self.source_to_display_byte_with_affinity(source_byte, ProjectionAffinity::Right)
    }

    /// Maps a source byte to a display byte using explicit collapsed-boundary affinity.
    pub fn source_to_display_byte_with_affinity(
        &self,
        source_byte: usize,
        affinity: ProjectionAffinity,
    ) -> usize {
        let source_byte = source_byte.min(self.source_len);
        let mut boundary = self.display_text.len();

        let first_candidate = self
            .segments
            .partition_point(|segment| segment.source.end < source_byte);
        for segment in &self.segments[first_candidate..] {
            if source_byte < segment.source.start {
                return segment.display.start;
            }
            if source_byte > segment.source.end {
                continue;
            }

            match &segment.kind {
                ProjectionSegmentKind::Visible => {
                    return segment.display.start
                        + source_byte.saturating_sub(segment.source.start);
                }
                ProjectionSegmentKind::Hidden
                | ProjectionSegmentKind::Replacement
                | ProjectionSegmentKind::Atom(_) => {
                    if source_byte == segment.source.start {
                        return segment.display.start;
                    }
                    if source_byte == segment.source.end {
                        boundary = segment.display.end;
                        continue;
                    }
                    return match affinity {
                        ProjectionAffinity::Left => segment.display.start,
                        ProjectionAffinity::Right => segment.display.end,
                    };
                }
            }
        }

        boundary.min(self.display_text.len())
    }

    /// Maps a display byte to the corresponding source byte with right affinity.
    pub fn display_to_source_byte(&self, display_byte: usize) -> usize {
        self.display_to_source_byte_with_affinity(display_byte, ProjectionAffinity::Right)
    }

    /// Maps a display byte to a source byte using explicit replacement affinity.
    pub fn display_to_source_byte_with_affinity(
        &self,
        display_byte: usize,
        affinity: ProjectionAffinity,
    ) -> usize {
        let display_byte = display_byte.min(self.display_text.len());
        let mut boundary = self.source_len;

        let first_candidate = self
            .segments
            .partition_point(|segment| segment.display.end < display_byte);
        for segment in &self.segments[first_candidate..] {
            if display_byte < segment.display.start {
                return segment.source.start;
            }
            if display_byte > segment.display.end {
                continue;
            }

            match &segment.kind {
                ProjectionSegmentKind::Visible => {
                    if display_byte < segment.display.end {
                        return segment.source.start
                            + display_byte.saturating_sub(segment.display.start);
                    }
                    boundary = segment.source.end;
                    continue;
                }
                ProjectionSegmentKind::Hidden
                | ProjectionSegmentKind::Replacement
                | ProjectionSegmentKind::Atom(_) => {
                    if segment.display.is_empty() && display_byte == segment.display.start {
                        return match affinity {
                            ProjectionAffinity::Left => segment.source.start,
                            ProjectionAffinity::Right => segment.source.end,
                        };
                    }
                    if display_byte == segment.display.start {
                        return segment.source.start;
                    }
                    if display_byte == segment.display.end {
                        boundary = segment.source.end;
                        continue;
                    }
                    return match affinity {
                        ProjectionAffinity::Left => segment.source.start,
                        ProjectionAffinity::Right => segment.source.end,
                    };
                }
            }
        }

        boundary.min(self.source_len)
    }

    /// Returns an atom occupying the supplied display position.
    pub fn atom_at_display_byte(
        &self,
        display_byte: usize,
    ) -> Option<(Range<usize>, ProjectionAtom)> {
        let first_candidate = self
            .segments
            .partition_point(|segment| segment.display.end < display_byte);
        self.segments[first_candidate..]
            .iter()
            .take_while(|segment| segment.display.start <= display_byte)
            .find_map(|segment| {
                let ProjectionSegmentKind::Atom(atom) = &segment.kind else {
                    return None;
                };
                (display_byte < segment.display.end).then(|| (segment.source.clone(), atom.clone()))
            })
    }

    /// Returns the complex atom whose replacement begins on the supplied display line.
    pub fn complex_atom_for_display_line(
        &self,
        display_range: &Range<usize>,
    ) -> Option<(Range<usize>, ComplexMarkdownKind)> {
        let first_candidate = self
            .segments
            .partition_point(|segment| segment.display.start < display_range.start);
        self.segments[first_candidate..]
            .iter()
            .take_while(|segment| segment.display.start <= display_range.end)
            .find_map(|segment| {
                let ProjectionSegmentKind::Atom(ProjectionAtom::Complex(kind)) = &segment.kind
                else {
                    return None;
                };
                Some((segment.source.clone(), kind.clone()))
            })
    }

    /// Converts source highlight ranges into visible display ranges.
    pub fn project_range(&self, source_range: &Range<usize>) -> Option<Range<usize>> {
        let start = self
            .source_to_display_byte_with_affinity(source_range.start, ProjectionAffinity::Right);
        let end =
            self.source_to_display_byte_with_affinity(source_range.end, ProjectionAffinity::Left);
        (start < end).then_some(start..end)
    }

    /// Builds a projection from ordered, non-overlapping source edits.
    fn from_edits(source: &str, mut edits: Vec<ProjectionEdit>) -> Self {
        edits.sort_by_key(|edit| (edit.source.start, Reverse(edit.source.end)));
        edits.retain(|edit| {
            edit.source.start <= edit.source.end
                && edit.source.end <= source.len()
                && source.is_char_boundary(edit.source.start)
                && source.is_char_boundary(edit.source.end)
        });

        let mut display_text = String::with_capacity(source.len());
        let mut segments = Vec::new();
        let mut source_cursor = 0usize;

        for edit in edits {
            if edit.source.start < source_cursor {
                continue;
            }
            if source_cursor < edit.source.start {
                let display_start = display_text.len();
                display_text.push_str(&source[source_cursor..edit.source.start]);
                segments.push(ProjectionSegment {
                    source: source_cursor..edit.source.start,
                    display: display_start..display_text.len(),
                    kind: ProjectionSegmentKind::Visible,
                });
            }

            let display_start = display_text.len();
            display_text.push_str(&edit.replacement);
            let kind = if let Some(atom) = edit.atom {
                ProjectionSegmentKind::Atom(atom)
            } else if edit.replacement.is_empty() {
                ProjectionSegmentKind::Hidden
            } else {
                ProjectionSegmentKind::Replacement
            };
            segments.push(ProjectionSegment {
                source: edit.source.clone(),
                display: display_start..display_text.len(),
                kind,
            });
            source_cursor = edit.source.end;
        }

        if source_cursor < source.len() {
            let display_start = display_text.len();
            display_text.push_str(&source[source_cursor..]);
            segments.push(ProjectionSegment {
                source: source_cursor..source.len(),
                display: display_start..display_text.len(),
                kind: ProjectionSegmentKind::Visible,
            });
        }

        if segments.is_empty() {
            return Self::from_source(source);
        }

        Self {
            display_text: Arc::from(display_text),
            source_len: source.len(),
            segments,
        }
    }
}

/// Returns the full source line containing the supplied byte range.
pub fn active_line_range(source: &str, source_range: Range<usize>) -> Range<usize> {
    let start_byte = source_range.start.min(source.len());
    let end_byte = source_range.end.min(source.len()).max(start_byte);
    let start = source[..start_byte]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let end = source[end_byte..]
        .find('\n')
        .map_or(source.len(), |index| end_byte + index);
    start..end
}

/// Generates safe display edits from syntax spans outside the active source line.
fn projection_edits(
    source: &str,
    spans: &[SyntaxSpan],
    components: &[ComplexMarkdownComponent],
    active_source_range: &Range<usize>,
) -> Vec<ProjectionEdit> {
    let mut edits = components
        .iter()
        .filter(|component| !ranges_intersect(&component.range, active_source_range))
        .map(|component| complex_component_replacement(source, component))
        .collect::<Vec<_>>();
    edits.extend(
        spans
            .iter()
            .filter(|span| !ranges_intersect(&span.range, active_source_range))
            .filter_map(|span| match span.kind {
                SyntaxKind::HeadingMarker
                | SyntaxKind::InlineCodeMarker
                | SyntaxKind::LinkTextDelimiter
                | SyntaxKind::LinkUrlDelimiter
                | SyntaxKind::LinkUrl
                | SyntaxKind::EmphasisMarker
                | SyntaxKind::StrikethroughMarker
                | SyntaxKind::QuoteMarker => Some(ProjectionEdit {
                    source: span.range.clone(),
                    replacement: String::new(),
                    atom: None,
                }),
                SyntaxKind::CodeFence => code_fence_replacement(source, &span.range),
                SyntaxKind::ListMarker => unordered_list_replacement(source, &span.range),
                SyntaxKind::TaskMarker => task_replacement(source, &span.range),
                SyntaxKind::HorizontalRule => Some(ProjectionEdit {
                    source: span.range.clone(),
                    replacement: "────────────".to_string(),
                    atom: Some(ProjectionAtom::HorizontalRule),
                }),
                _ => None,
            }),
    );
    edits
}

/// Replaces one complex source range with the display atom used by live mode.
fn complex_component_replacement(
    source: &str,
    component: &ComplexMarkdownComponent,
) -> ProjectionEdit {
    let label = match &component.kind {
        ComplexMarkdownKind::Image { .. } => String::new(),
        ComplexMarkdownKind::Table { .. } => "\u{200B}".to_string(),
        ComplexMarkdownKind::Callout { kind, title, .. } => format!("▣ {kind} · {title}"),
        ComplexMarkdownKind::Footnote { label, text } => text
            .as_ref()
            .map_or_else(|| format!("⁝{label}"), |text| format!("⁝ {label} · {text}")),
        ComplexMarkdownKind::WikiLink { label, .. } => label.clone(),
        ComplexMarkdownKind::Math { formula, block } => {
            if *block {
                format!("∑ {}", formula.lines().collect::<Vec<_>>().join(" "))
            } else {
                formula.clone()
            }
        }
        ComplexMarkdownKind::Mermaid { source } => {
            let diagram_kind = source.lines().next().unwrap_or("diagram").trim();
            format!("◇ Mermaid · {diagram_kind}")
        }
    };
    let component_source = source.get(component.range.clone()).unwrap_or_default();
    let replacement = if matches!(component.kind, ComplexMarkdownKind::Table { .. }) {
        // The native table owns the visual height of every row. Retain only the
        // terminating line break so following Markdown starts on its own line.
        format!(
            "{label}{}",
            if component_source.ends_with('\n') {
                "\n"
            } else {
                ""
            }
        )
    } else {
        let newline_count = component_source
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        format!("{label}{}", "\n".repeat(newline_count))
    };
    ProjectionEdit {
        source: component.range.clone(),
        replacement,
        atom: Some(ProjectionAtom::Complex(component.kind.clone())),
    }
}

/// Hides fence delimiter runs while retaining an opening fence's language label.
fn code_fence_replacement(source: &str, range: &Range<usize>) -> Option<ProjectionEdit> {
    let fence_part = source.get(range.clone())?;
    fence_part
        .as_bytes()
        .first()
        .is_some_and(|byte| matches!(byte, b'`' | b'~'))
        .then(|| ProjectionEdit {
            source: range.clone(),
            replacement: String::new(),
            atom: None,
        })
}

/// Produces a bullet atom for unordered list markers while preserving ordered markers.
fn unordered_list_replacement(source: &str, range: &Range<usize>) -> Option<ProjectionEdit> {
    let marker = source.get(range.clone())?;
    matches!(marker.as_bytes().first(), Some(b'-' | b'*' | b'+')).then(|| ProjectionEdit {
        source: range.clone(),
        replacement: "• ".to_string(),
        atom: Some(ProjectionAtom::Bullet),
    })
}

/// Produces an interactive checkbox atom for a task-list marker.
fn task_replacement(source: &str, range: &Range<usize>) -> Option<ProjectionEdit> {
    let marker = source.get(range.clone())?;
    let checked = marker
        .as_bytes()
        .get(3)
        .is_some_and(|byte| matches!(byte, b'x' | b'X'));
    Some(ProjectionEdit {
        source: range.clone(),
        replacement: if checked { "☑ " } else { "☐ " }.to_string(),
        atom: Some(ProjectionAtom::Task { checked }),
    })
}

/// Returns whether two half-open byte ranges overlap.
fn ranges_intersect(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reproduces the former linear source lookup so indexed results can be compared exactly.
    fn linear_source_lookup(
        projection: &DisplayProjection,
        source_byte: usize,
        affinity: ProjectionAffinity,
    ) -> usize {
        let source_byte = source_byte.min(projection.source_len);
        let mut boundary = projection.display_text.len();
        for segment in &projection.segments {
            if source_byte < segment.source.start {
                return segment.display.start;
            }
            if source_byte > segment.source.end {
                continue;
            }
            match &segment.kind {
                ProjectionSegmentKind::Visible => {
                    return segment.display.start
                        + source_byte.saturating_sub(segment.source.start);
                }
                ProjectionSegmentKind::Hidden
                | ProjectionSegmentKind::Replacement
                | ProjectionSegmentKind::Atom(_) => {
                    if source_byte == segment.source.start {
                        return segment.display.start;
                    }
                    if source_byte == segment.source.end {
                        boundary = segment.display.end;
                        continue;
                    }
                    return match affinity {
                        ProjectionAffinity::Left => segment.display.start,
                        ProjectionAffinity::Right => segment.display.end,
                    };
                }
            }
        }
        boundary.min(projection.display_text.len())
    }

    /// Reproduces the former linear display lookup so indexed results can be compared exactly.
    fn linear_display_lookup(
        projection: &DisplayProjection,
        display_byte: usize,
        affinity: ProjectionAffinity,
    ) -> usize {
        let display_byte = display_byte.min(projection.display_text.len());
        let mut boundary = projection.source_len;
        for segment in &projection.segments {
            if display_byte < segment.display.start {
                return segment.source.start;
            }
            if display_byte > segment.display.end {
                continue;
            }
            match &segment.kind {
                ProjectionSegmentKind::Visible => {
                    if display_byte < segment.display.end {
                        return segment.source.start
                            + display_byte.saturating_sub(segment.display.start);
                    }
                    boundary = segment.source.end;
                    continue;
                }
                ProjectionSegmentKind::Hidden
                | ProjectionSegmentKind::Replacement
                | ProjectionSegmentKind::Atom(_) => {
                    if segment.display.is_empty() && display_byte == segment.display.start {
                        return match affinity {
                            ProjectionAffinity::Left => segment.source.start,
                            ProjectionAffinity::Right => segment.source.end,
                        };
                    }
                    if display_byte == segment.display.start {
                        return segment.source.start;
                    }
                    if display_byte == segment.display.end {
                        boundary = segment.source.end;
                        continue;
                    }
                    return match affinity {
                        ProjectionAffinity::Left => segment.source.start,
                        ProjectionAffinity::Right => segment.source.end,
                    };
                }
            }
        }
        boundary.min(projection.source_len)
    }

    #[test]
    fn shared_source_identity_projection_reuses_the_source_allocation() {
        let source: Arc<str> = Arc::from("large source");
        let projection = DisplayProjection::from_shared_source(source.clone());

        assert!(Arc::ptr_eq(&source, &projection.display_text));
        assert_eq!(projection.source_to_display_byte(7), 7);
        assert_eq!(projection.display_to_source_byte(7), 7);
    }
    use crate::services::syntax::markdown_spans;

    #[test]
    fn identity_projection_maps_every_utf8_boundary() {
        let source = "A中😀B";
        let projection = DisplayProjection::from_source(source);

        for byte in 0..=source.len() {
            if source.is_char_boundary(byte) {
                assert_eq!(projection.source_to_display_byte(byte), byte);
                assert_eq!(projection.display_to_source_byte(byte), byte);
            }
        }
    }

    #[test]
    fn indexed_projection_lookup_matches_linear_boundary_semantics() {
        let source = "# 中😀 title\n- [x] **bold** and [site](https://example.com)\nplain\n";
        let spans = markdown_spans(source);
        let projection =
            DisplayProjection::from_markdown(source, &spans, source.len()..source.len());

        for affinity in [ProjectionAffinity::Left, ProjectionAffinity::Right] {
            for source_byte in 0..=source.len() {
                assert_eq!(
                    projection.source_to_display_byte_with_affinity(source_byte, affinity),
                    linear_source_lookup(&projection, source_byte, affinity),
                    "source byte {source_byte} with {affinity:?}"
                );
            }
            for display_byte in 0..=projection.display_text.len() {
                assert_eq!(
                    projection.display_to_source_byte_with_affinity(display_byte, affinity),
                    linear_display_lookup(&projection, display_byte, affinity),
                    "display byte {display_byte} with {affinity:?}"
                );
            }
        }
    }

    #[test]
    fn live_projection_hides_inactive_inline_markers_and_link_target() {
        let source = "active **bold**\nother **strong** and [site](https://example.com)";
        let spans = markdown_spans(source);
        let active = active_line_range(source, 7..7);
        let projection = DisplayProjection::from_markdown(source, &spans, active);

        assert_eq!(
            projection.display_text.as_ref(),
            "active **bold**\nother strong and site"
        );
    }

    #[test]
    fn collapsed_marker_boundaries_have_deterministic_affinity() {
        let source = "**中**";
        let spans = markdown_spans(source);
        let projection =
            DisplayProjection::from_markdown(source, &spans, source.len()..source.len());

        assert_eq!(projection.display_text.as_ref(), "中");
        assert_eq!(
            projection.source_to_display_byte_with_affinity(1, ProjectionAffinity::Left),
            0
        );
        assert_eq!(
            projection.source_to_display_byte_with_affinity(1, ProjectionAffinity::Right),
            0
        );
        assert_eq!(projection.display_to_source_byte(0), 2);
        assert_eq!(
            projection.display_to_source_byte_with_affinity(0, ProjectionAffinity::Left),
            0
        );
        assert_eq!(
            projection.display_to_source_byte(projection.display_text.len()),
            source.len()
        );
    }

    #[test]
    fn task_and_bullet_markers_become_atoms() {
        let source = "- item\n- [x] done\n";
        let spans = markdown_spans(source);
        let projection =
            DisplayProjection::from_markdown(source, &spans, source.len()..source.len());

        assert_eq!(projection.display_text.as_ref(), "• item\n☑ done\n");
        assert!(matches!(
            projection.atom_at_display_byte(0),
            Some((_, ProjectionAtom::Bullet))
        ));
        let task_display = projection.display_text.find('☑').unwrap();
        assert!(matches!(
            projection.atom_at_display_byte(task_display),
            Some((_, ProjectionAtom::Task { checked: true }))
        ));
    }

    #[test]
    fn active_line_preserves_raw_source_for_direct_editing() {
        let source = "# Title\nnext *line*";
        let spans = markdown_spans(source);
        let active = active_line_range(source, 2..2);
        let projection = DisplayProjection::from_markdown(source, &spans, active);

        assert!(projection.display_text.starts_with("# Title"));
        assert!(projection.display_text.ends_with("next line"));
    }

    #[test]
    fn inactive_code_fence_keeps_its_language_label() {
        let source = "```rust\nfn main() {}\n```\nplain";
        let spans = markdown_spans(source);
        let projection = DisplayProjection::from_markdown(source, &spans, 30..35);

        assert!(projection.display_text.starts_with("rust\nfn main() {}"));
        assert!(!projection.display_text.contains("```"));
    }

    #[test]
    fn table_component_uses_one_display_line_without_row_placeholders() {
        use crate::services::complex_markdown::complex_markdown_components;

        let source = "before\n| A | B |\n| --- | --- |\n| 1 | 2 |\nafter";
        let spans = markdown_spans(source);
        let components = complex_markdown_components(source);
        let projection =
            DisplayProjection::from_markdown_with_components(source, &spans, &components, 0..6);

        let table_byte = projection
            .display_text
            .find('\u{200B}')
            .expect("table atom");
        assert_eq!(projection.display_text.as_ref(), "before\n\u{200B}\nafter");
        assert!(matches!(
            projection.atom_at_display_byte(table_byte),
            Some((
                _,
                ProjectionAtom::Complex(ComplexMarkdownKind::Table { .. })
            ))
        ));
    }

    #[test]
    fn table_component_preserves_an_explicit_following_blank_line() {
        use crate::services::complex_markdown::complex_markdown_components;

        let source = "| A | B |\n| --- | --- |\n| 1 | 2 |\n\nafter";
        let spans = markdown_spans(source);
        let components = complex_markdown_components(source);
        let projection =
            DisplayProjection::from_markdown_with_components(source, &spans, &components, 0..0);

        assert_eq!(projection.display_text.as_ref(), "\u{200B}\n\nafter");
    }

    #[test]
    fn table_atom_does_not_claim_the_following_blank_line() {
        use crate::services::complex_markdown::complex_markdown_components;

        let source = "| A | B |\n| --- | --- |\n| 1 | 2 |\n\nafter";
        let spans = markdown_spans(source);
        let components = complex_markdown_components(source);
        let projection =
            DisplayProjection::from_markdown_with_components(source, &spans, &components, 0..0);
        let blank_line_byte = "\u{200B}\n".len();

        assert!(projection.atom_at_display_byte(blank_line_byte).is_none());
    }

    #[test]
    fn image_component_does_not_render_a_name_label() {
        use crate::services::complex_markdown::complex_markdown_components;

        let source = "before\n![tshirt-2.jpg](assets/tshirt-2.jpg)\nafter";
        let spans = markdown_spans(source);
        let components = complex_markdown_components(source);
        let projection =
            DisplayProjection::from_markdown_with_components(source, &spans, &components, 0..6);

        assert_eq!(projection.display_text.as_ref(), "before\n\nafter");
        assert!(!projection.display_text.contains("tshirt-2.jpg"));
        assert!(matches!(
            projection.atom_at_display_byte("before\n".len()),
            Some((
                _,
                ProjectionAtom::Complex(ComplexMarkdownKind::Image { .. })
            ))
        ));
    }
}
