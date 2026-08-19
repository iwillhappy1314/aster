use crate::services::syntax::markdown_heading_level;
use gpui::{AnyElement, HighlightStyle, Pixels, Point, StyledText, div, prelude::*, px};
use gpui_gfm::{heading_font_size, heading_font_weight};
use std::ops::Range;
use std::panic::AssertUnwindSafe;

const EMPTY_LINE_PLACEHOLDER: &str = "\u{200B}";

#[derive(Clone)]
struct EditorLineLayout {
    display_range: Range<usize>,
    layout: gpui::TextLayout,
}

/// Aggregates independently-sized line layouts behind the same byte/position
/// mapping operations the editor previously performed on one `TextLayout`.
#[derive(Clone, Default)]
pub struct EditorTextLayout {
    lines: Vec<EditorLineLayout>,
    display_len: usize,
}

impl EditorTextLayout {
    pub fn index_for_position(&self, position: Point<Pixels>) -> Result<usize, usize> {
        let Some(line) = self.closest_line(position) else {
            return Ok(0);
        };

        let line_len = line.display_range.len();
        if line_len == 0 {
            return Ok(line.display_range.start);
        }

        match line.layout.index_for_position(position) {
            Ok(index) => Ok(line.display_range.start + index.min(line_len)),
            Err(index) => Err(line.display_range.start + index.min(line_len)),
        }
    }

    pub fn position_for_index(&self, index: usize) -> Option<Point<Pixels>> {
        let line = self.line_for_index(index)?;
        let local_index = index
            .saturating_sub(line.display_range.start)
            .min(line.display_range.len());
        line.layout.position_for_index(local_index)
    }

    pub fn line_height_for_index(&self, index: usize) -> Pixels {
        self.line_for_index(index)
            .map(|line| line.layout.line_height())
            .unwrap_or(px(0.))
    }

    fn line_for_index(&self, index: usize) -> Option<&EditorLineLayout> {
        let index = index.min(self.display_len);
        self.lines
            .iter()
            .find(|line| index <= line.display_range.end)
            .or_else(|| self.lines.last())
    }

    fn closest_line(&self, position: Point<Pixels>) -> Option<&EditorLineLayout> {
        let mut closest: Option<(&EditorLineLayout, Pixels)> = None;

        for line in &self.lines {
            let bounds = line.layout.bounds();
            let distance = if position.y < bounds.top() {
                bounds.top() - position.y
            } else if position.y > bounds.bottom() {
                position.y - bounds.bottom()
            } else {
                px(0.)
            };

            if distance == px(0.) {
                return Some(line);
            }

            if closest
                .as_ref()
                .is_none_or(|(_, closest_distance)| distance < *closest_distance)
            {
                closest = Some((line, distance));
            }
        }

        closest.map(|(line, _)| line)
    }
}

pub struct EditorTextRender {
    pub element: AnyElement,
    pub layout: EditorTextLayout,
}

/// Builds one `StyledText` per source line so headings can use their canonical
/// preview font size while the rest of the editor keeps the configured body size.
pub fn render_editor_text(
    display_text: &str,
    source_text: &str,
    highlights: &[(Range<usize>, HighlightStyle)],
    body_font_size: f32,
) -> EditorTextRender {
    let display_ranges = display_line_ranges(display_text);
    let heading_levels = source_text
        .split('\n')
        .map(markdown_heading_level)
        .collect::<Vec<_>>();

    let mut container = div().relative().w_full().min_w_0().flex().flex_col();
    let mut layouts = Vec::with_capacity(display_ranges.len());

    for (line_index, display_range) in display_ranges.iter().enumerate() {
        let line_text = &display_text[display_range.clone()];
        let rendered_text = if line_text.is_empty() {
            EMPTY_LINE_PLACEHOLDER.to_string()
        } else {
            line_text.to_string()
        };
        let line_highlights = if line_text.is_empty() {
            Vec::new()
        } else {
            highlights_for_line(highlights, display_range)
        };

        let mut styled = StyledText::new(rendered_text.clone());
        if !line_highlights.is_empty() {
            styled = std::panic::catch_unwind(AssertUnwindSafe(|| {
                StyledText::new(rendered_text.clone()).with_highlights(line_highlights)
            }))
            .unwrap_or_else(|_| StyledText::new(rendered_text));
        }

        let layout = styled.layout().clone();
        let heading_level = heading_levels.get(line_index).copied().flatten();

        let mut line = div().w_full().min_w_0();
        if let Some(level) = heading_level {
            line = line
                .text_size(heading_font_size(level))
                .font_weight(heading_font_weight(level));
        } else {
            line = line.text_size(px(body_font_size));
        }
        line = line.child(styled);

        container = container.child(line);
        layouts.push(EditorLineLayout {
            display_range: display_range.clone(),
            layout,
        });
    }

    EditorTextRender {
        element: container.into_any_element(),
        layout: EditorTextLayout {
            lines: layouts,
            display_len: display_text.len(),
        },
    }
}

fn display_line_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0usize;

    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            ranges.push(start..index);
            start = index + 1;
        }
    }

    ranges.push(start..text.len());
    ranges
}

fn highlights_for_line(
    highlights: &[(Range<usize>, HighlightStyle)],
    line: &Range<usize>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    highlights
        .iter()
        .filter_map(|(range, style)| {
            let start = range.start.max(line.start);
            let end = range.end.min(line.end);
            (start < end).then(|| {
                (
                    (start - line.start)..(end - line.start),
                    style.clone(),
                )
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_ranges_keep_empty_lines_and_trailing_line() {
        assert_eq!(display_line_ranges("a\n\nb\n"), vec![0..1, 2..2, 3..4, 5..5]);
    }

    #[test]
    fn line_highlights_are_clipped_and_rebased() {
        let highlights = vec![(2..8, HighlightStyle::default())];
        let result = highlights_for_line(&highlights, &(5..10));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, 0..3);
    }
}
