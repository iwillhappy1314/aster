use crate::services::complex_markdown::{ComplexMarkdownKind, MarkdownTableCell};
use crate::services::syntax::{MarkdownLineKind, markdown_heading_levels, markdown_line_kinds};
use crate::ui::editor_projection::DisplayProjection;
use crate::ui::theme::Theme;
use gpui::{
    AnyElement, App, Entity, HighlightStyle, ImageSource, MouseButton, ObjectFit, Pixels, Point,
    StyledText, Window, deferred, div, img, phi, prelude::*, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::Sizable as _;
use gpui_gfm::{heading_font_size, heading_font_weight};
use std::ops::Range;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const EMPTY_LINE_PLACEHOLDER: &str = "\u{200B}";
const LINE_NUMBER_GUTTER_GAP: f32 = 12.0;
const LINE_NUMBER_GUTTER_LEFT_INSET: f32 = 4.0;
const MIN_LINE_NUMBER_GUTTER_WIDTH: f32 = 16.0;
const MONOSPACE_DIGIT_WIDTH_RATIO: f32 = 0.6;

/// Receives source ranges when a rendered complex component is clicked.
pub type ComplexComponentClickHandler =
    Arc<dyn Fn(Range<usize>, &mut Window, &mut App) + Send + Sync>;

/// Receives a table cell when inline editing should begin.
pub type TableCellClickHandler =
    Arc<dyn Fn(MarkdownTableCell, &mut Window, &mut App) + Send + Sync>;

/// Identifies the native input currently replacing one rendered table cell.
#[derive(Clone)]
pub struct ActiveTableCellEditor {
    pub source_range: Range<usize>,
    pub input: Entity<InputState>,
    pub layout_text: String,
    pub input_visible: bool,
}

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
        let line_index = self
            .lines
            .partition_point(|line| line.display_range.end < index);
        self.lines.get(line_index).or_else(|| self.lines.last())
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
    pub left_padding: f32,
}

/// Builds one `StyledText` per source line so headings can use their canonical
/// preview font size while the rest of the editor keeps the configured body size.
pub fn render_editor_text(
    display_text: &str,
    source_text: &str,
    projection: &DisplayProjection,
    highlights: &[(Range<usize>, HighlightStyle)],
    body_font_size: f32,
    document_directory: Option<&Path>,
    selected_component: Option<&Range<usize>>,
    on_component_click: ComplexComponentClickHandler,
    active_table_cell: Option<&ActiveTableCellEditor>,
    on_table_cell_click: TableCellClickHandler,
) -> EditorTextRender {
    let display_ranges = display_line_ranges(display_text);
    let heading_levels = markdown_heading_levels(source_text);
    let line_kinds = markdown_line_kinds(source_text);
    let line_number_gutter_width = line_number_gutter_width(display_ranges.len(), body_font_size);
    let left_padding =
        LINE_NUMBER_GUTTER_LEFT_INSET + line_number_gutter_width + LINE_NUMBER_GUTTER_GAP;

    let mut container = div().relative().w_full().min_w_0().flex().flex_col();
    let mut layouts = Vec::with_capacity(display_ranges.len());
    let mut highlight_start = 0usize;

    debug_assert!(
        highlights
            .windows(2)
            .all(|pair| pair[0].0.start <= pair[1].0.start)
    );

    for (line_index, display_range) in display_ranges.iter().enumerate() {
        while highlight_start < highlights.len()
            && highlights[highlight_start].0.end <= display_range.start
        {
            highlight_start += 1;
        }

        let line_text = &display_text[display_range.clone()];
        let rendered_text = if line_text.is_empty() {
            EMPTY_LINE_PLACEHOLDER.to_string()
        } else {
            line_text.to_string()
        };
        let line_highlights = if line_text.is_empty() {
            Vec::new()
        } else {
            highlights_for_line(highlights, display_range, highlight_start)
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
        let line_kind = line_kinds
            .get(line_index)
            .copied()
            .unwrap_or(MarkdownLineKind::Paragraph);
        let complex_component = projection.complex_atom_for_display_line(display_range);

        let mut line = div().relative().w_full().min_w_0();
        if let Some(level) = heading_level {
            line = line
                .text_size(heading_font_size(level))
                .font_weight(heading_font_weight(level));
        } else {
            line = line.text_size(px(body_font_size));
        }
        line = match line_kind {
            MarkdownLineKind::Quote => line.border_l_2().border_color(Theme::muted()).pl(px(12.)),
            MarkdownLineKind::CodeFence | MarkdownLineKind::CodeBody => {
                line.bg(Theme::code_block_bg()).px(px(12.))
            }
            MarkdownLineKind::List { indent } | MarkdownLineKind::Task { indent } => {
                line.pl(px((indent as f32) * 4.0))
            }
            MarkdownLineKind::Paragraph | MarkdownLineKind::HorizontalRule => line,
        };
        line = line.child(styled);
        if let Some((source_range, kind)) = complex_component
            && is_block_component(&kind)
        {
            let is_table = matches!(kind, ComplexMarkdownKind::Table { .. });
            let is_selected = !is_table && selected_component == Some(&source_range);
            let click_range = source_range.clone();
            let click_handler = on_component_click.clone();
            line = if is_table {
                line.mb(px(8.))
            } else {
                line.mt(px(4.))
                    .mb(px(8.))
                    .p(px(10.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(if is_selected {
                        Theme::accent()
                    } else {
                        Theme::border()
                    })
                    .bg(Theme::panel_alt())
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        click_handler(click_range.clone(), window, cx);
                        cx.stop_propagation();
                    })
            };
            if let Some(detail) = render_complex_component_detail(
                &kind,
                document_directory,
                active_table_cell,
                &on_table_cell_click,
                body_font_size,
            ) {
                line = line.child(detail);
            }
            if is_selected {
                line = line.child(
                    div()
                        .mt(px(8.))
                        .text_xs()
                        .text_color(Theme::accent())
                        .child("Selected · click again to edit source"),
                );
            }
        }
        line = line.child(
            div()
                .absolute()
                .top_0()
                .right_full()
                .mr(px(LINE_NUMBER_GUTTER_GAP))
                .w(px(line_number_gutter_width))
                .text_right()
                .text_size(px(body_font_size))
                .text_color(crate::ui::theme::Theme::muted())
                .child((line_index + 1).to_string()),
        );

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
        left_padding,
    }
}

/// Returns whether a complex component owns an entire visual block.
fn is_block_component(kind: &ComplexMarkdownKind) -> bool {
    matches!(
        kind,
        ComplexMarkdownKind::Image { .. }
            | ComplexMarkdownKind::Table { .. }
            | ComplexMarkdownKind::Callout { .. }
            | ComplexMarkdownKind::Footnote { text: Some(_), .. }
            | ComplexMarkdownKind::Math { block: true, .. }
            | ComplexMarkdownKind::Mermaid { .. }
    )
}

/// Builds the native GPUI detail view shown below a complex component label.
fn render_complex_component_detail(
    kind: &ComplexMarkdownKind,
    document_directory: Option<&Path>,
    active_table_cell: Option<&ActiveTableCellEditor>,
    on_table_cell_click: &TableCellClickHandler,
    body_font_size: f32,
) -> Option<AnyElement> {
    match kind {
        ComplexMarkdownKind::Image { alt, target } => {
            let label = if alt.trim().is_empty() {
                "Image".to_string()
            } else {
                alt.clone()
            };
            let loading_label = label.clone();
            let fallback_label = format!("Unable to load image · {target}");
            Some(
                img(resolve_image_source(target, document_directory))
                    .mt(px(8.))
                    .w_full()
                    .max_h(px(420.))
                    .object_fit(ObjectFit::Contain)
                    .with_loading(move || component_message(&format!("Loading {loading_label}…")))
                    .with_fallback(move || component_message(&fallback_label))
                    .into_any_element(),
            )
        }
        ComplexMarkdownKind::Table { headers, rows } => {
            let column_count = headers
                .len()
                .max(rows.iter().map(Vec::len).max().unwrap_or(0))
                .max(1);
            let mut table = div()
                .w_full()
                .overflow_hidden()
                .border_1()
                .border_color(Theme::border());
            let all_rows = std::iter::once(headers).chain(rows.iter());
            for (row_index, row) in all_rows.enumerate() {
                let mut row_element = div().flex().w_full();
                for column in 0..column_count {
                    let cell = row.get(column).cloned();
                    let mut cell_element = div()
                        .relative()
                        .occlude()
                        .min_w(px(80.))
                        .flex_1()
                        .px(px(10.))
                        .py(px(7.))
                        .when(column + 1 < column_count, |cell| {
                            cell.border_r_1().border_color(Theme::border())
                        })
                        .when(row_index == 0, |cell| {
                            cell.bg(Theme::selection_bg())
                                .font_weight(gpui::FontWeight::BOLD)
                        });
                    if let Some(cell) = cell {
                        if active_table_cell
                            .is_some_and(|active| active.source_range == cell.source_range)
                        {
                            let input = active_table_cell.expect("active table cell").input.clone();
                            let input_visible = active_table_cell
                                .expect("active table cell")
                                .input_visible;
                            cell_element = cell_element
                                // Preserve the cell's normal wrapped-text height while the
                                // input completes its first width-aware layout behind it.
                                .child(
                                    div()
                                        .w_full()
                                        .when(input_visible, |snapshot| snapshot.invisible())
                                        .child(
                                            active_table_cell
                                                .expect("active table cell")
                                                .layout_text
                                                .clone(),
                                        ),
                                )
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .right_0()
                                        .bottom_0()
                                        .left_0()
                                        .flex()
                                        .px(px(6.))
                                        .pt(px(7.))
                                        .when(!input_visible, |editor| editor.opacity(0.0))
                                        .child(
                                            Input::new(&input)
                                                .xsmall()
                                                .h_full()
                                                .appearance(false)
                                                .bordered(false)
                                                .focus_bordered(false)
                                                .text_size(px(body_font_size))
                                                .line_height(phi()),
                                        ),
                                );
                        } else {
                            let handler = on_table_cell_click.clone();
                            let clicked_cell = cell.clone();
                            cell_element = cell_element
                                .cursor_text()
                                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                                    handler(clicked_cell.clone(), window, cx);
                                    cx.stop_propagation();
                                })
                                .child(div().w_full().child(cell.text));
                        }
                    }
                    row_element = row_element.child(cell_element);
                }
                table = table.child(row_element.when(row_index > 0, |row| {
                    row.border_t_1().border_color(Theme::border())
                }));
            }
            Some(table.into_any_element())
        }
        ComplexMarkdownKind::Callout { kind, body, .. } => Some(
            div()
                .mt(px(8.))
                .pl(px(10.))
                .border_l_2()
                .border_color(Theme::accent())
                .text_color(Theme::text())
                .child(if body.trim().is_empty() {
                    format!("{kind} callout")
                } else {
                    body.clone()
                })
                .into_any_element(),
        ),
        ComplexMarkdownKind::Footnote {
            text: Some(text), ..
        } => Some(
            div()
                .mt(px(6.))
                .text_sm()
                .text_color(Theme::muted())
                .child(text.clone())
                .into_any_element(),
        ),
        ComplexMarkdownKind::Math {
            formula,
            block: true,
        } => Some(
            deferred(
                div()
                    .mt(px(8.))
                    .w_full()
                    .flex()
                    .justify_center()
                    .font_family("Times New Roman")
                    .text_size(px(19.))
                    .child(if latex_formula_is_balanced(formula) {
                        latex_display_text(formula)
                    } else {
                        "Unable to render formula · edit source".to_string()
                    }),
            )
            .with_priority(1)
            .into_any_element(),
        ),
        ComplexMarkdownKind::Mermaid { source } => Some(
            deferred(render_mermaid_summary(source))
                .with_priority(1)
                .into_any_element(),
        ),
        ComplexMarkdownKind::Footnote { text: None, .. }
        | ComplexMarkdownKind::WikiLink { .. }
        | ComplexMarkdownKind::Math { block: false, .. } => None,
    }
}

/// Performs a cheap validation before presenting a LaTeX formula as rendered content.
fn latex_formula_is_balanced(formula: &str) -> bool {
    let mut depth = 0isize;
    for character in formula.chars() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

/// Converts common LaTeX notation into readable native text without executing external tools.
fn latex_display_text(formula: &str) -> String {
    let mut rendered = formula.trim().to_string();
    for (command, symbol) in [
        ("\\alpha", "α"),
        ("\\beta", "β"),
        ("\\gamma", "γ"),
        ("\\delta", "δ"),
        ("\\theta", "θ"),
        ("\\lambda", "λ"),
        ("\\pi", "π"),
        ("\\sigma", "σ"),
        ("\\omega", "ω"),
        ("\\times", "×"),
        ("\\cdot", "·"),
        ("\\leq", "≤"),
        ("\\geq", "≥"),
        ("\\neq", "≠"),
        ("\\approx", "≈"),
        ("\\infty", "∞"),
        ("\\sum", "∑"),
        ("\\sqrt", "√"),
    ] {
        rendered = rendered.replace(command, symbol);
    }
    while let Some(start) = rendered.find("\\frac{") {
        let numerator_start = start + "\\frac".len();
        let Some((numerator, after_numerator)) = braced_group(&rendered, numerator_start) else {
            break;
        };
        let Some((denominator, after_denominator)) = braced_group(&rendered, after_numerator)
        else {
            break;
        };
        rendered.replace_range(
            start..after_denominator,
            &format!("({numerator})⁄({denominator})"),
        );
    }
    rendered = replace_script_digits(&rendered, '^', true);
    rendered = replace_script_digits(&rendered, '_', false);
    rendered.replace(['{', '}'], "")
}

/// Reads one balanced `{...}` group and returns its content plus the following byte.
fn braced_group(value: &str, start: usize) -> Option<(String, usize)> {
    if value.as_bytes().get(start) != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    for (relative, character) in value[start..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let end = start + relative;
                    return Some((value[start + 1..end].to_string(), end + 1));
                }
            }
            _ => {}
        }
    }
    None
}

/// Replaces numeric superscript or subscript notation with Unicode glyphs when available.
fn replace_script_digits(value: &str, marker: char, superscript: bool) -> String {
    let mut output = String::new();
    let characters = value.chars().collect::<Vec<_>>();
    let mut index = 0usize;
    while index < characters.len() {
        if characters[index] == marker {
            let (digits, consumed) = if characters.get(index + 1) == Some(&'{') {
                let end = characters[index + 2..]
                    .iter()
                    .position(|character| *character == '}')
                    .map(|relative| index + 2 + relative);
                end.map_or((String::new(), 0), |end| {
                    (characters[index + 2..end].iter().collect(), end - index)
                })
            } else {
                characters
                    .get(index + 1)
                    .filter(|character| character.is_ascii_digit())
                    .map_or((String::new(), 0), |character| (character.to_string(), 1))
            };
            if !digits.is_empty() {
                let converted = digits
                    .chars()
                    .map(|digit| script_digit(digit, superscript))
                    .collect::<Option<String>>();
                if let Some(converted) = converted {
                    output.push_str(&converted);
                    index += consumed + 1;
                    continue;
                }
            }
        }
        output.push(characters[index]);
        index += 1;
    }
    output
}

/// Maps one ASCII digit to its superscript or subscript equivalent.
fn script_digit(digit: char, superscript: bool) -> Option<char> {
    let table = if superscript {
        ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹']
    } else {
        ['₀', '₁', '₂', '₃', '₄', '₅', '₆', '₇', '₈', '₉']
    };
    digit.to_digit(10).map(|index| table[index as usize])
}

/// Resolves remote, absolute and document-relative image sources consistently with Preview.
fn resolve_image_source(target: &str, document_directory: Option<&Path>) -> ImageSource {
    if let Ok(url) = url::Url::parse(target) {
        if url.scheme() == "file"
            && let Ok(path) = url.to_file_path()
        {
            return path.into();
        }
        return target.to_string().into();
    }
    let target_path = Path::new(target);
    let resolved = if target_path.is_absolute() {
        target_path.to_path_buf()
    } else {
        document_directory
            .map(|directory| directory.join(target_path))
            .unwrap_or_else(|| PathBuf::from(target_path))
    };
    resolved.into()
}

/// Produces a consistent loading or failure message without hiding the image source.
fn component_message(message: &str) -> AnyElement {
    div()
        .w_full()
        .h(px(96.))
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(Theme::muted())
        .child(message.to_string())
        .into_any_element()
}

/// Produces a deterministic native summary for Mermaid source without executing scripts.
fn render_mermaid_summary(source: &str) -> AnyElement {
    let edges = source
        .lines()
        .filter(|line| line.contains("-->") || line.contains("---") || line.contains("==>"))
        .take(8)
        .filter_map(parse_mermaid_edge)
        .collect::<Vec<_>>();
    let mut diagram = div()
        .mt(px(8.))
        .w_full()
        .p(px(10.))
        .rounded(px(6.))
        .bg(Theme::panel())
        .text_sm();
    if edges.is_empty() {
        diagram = diagram.child(component_message(
            "Mermaid source is not a supported flow diagram",
        ));
    } else {
        for (from, to) in edges {
            diagram = diagram.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .py(px(3.))
                    .text_color(Theme::text())
                    .child(
                        div()
                            .min_w(px(96.))
                            .px(px(9.))
                            .py(px(5.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(Theme::accent())
                            .text_center()
                            .child(from),
                    )
                    .child(div().text_color(Theme::accent()).child("→"))
                    .child(
                        div()
                            .min_w(px(96.))
                            .px(px(9.))
                            .py(px(5.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(Theme::accent())
                            .text_center()
                            .child(to),
                    ),
            );
        }
    }
    diagram.into_any_element()
}

/// Parses one basic Mermaid flow edge into human-readable node labels.
fn parse_mermaid_edge(line: &str) -> Option<(String, String)> {
    let (left, right) = ["-->", "==>", "---"]
        .iter()
        .find_map(|marker| line.split_once(marker))?;
    let right = right
        .trim()
        .strip_prefix('|')
        .and_then(|value| value.split_once('|').map(|(_, node)| node))
        .unwrap_or(right);
    Some((mermaid_node_label(left), mermaid_node_label(right)))
}

/// Removes Mermaid node identifiers and delimiters while retaining their visible label.
fn mermaid_node_label(value: &str) -> String {
    let value = value.trim();
    for (open, close) in [('[', ']'), ('(', ')'), ('{', '}')] {
        if let Some(start) = value.find(open)
            && let Some(end) = value.rfind(close)
            && start < end
        {
            return value[start + 1..end].trim_matches(['"', '\'']).to_string();
        }
    }
    value
        .split_ascii_whitespace()
        .next()
        .unwrap_or(value)
        .trim_matches([';', '"', '\''])
        .to_string()
}

/// Calculates the gutter width needed to render the largest visible line number.
fn line_number_gutter_width(line_count: usize, body_font_size: f32) -> f32 {
    let digit_count = line_count.max(1).to_string().len() as f32;
    (digit_count * body_font_size * MONOSPACE_DIGIT_WIDTH_RATIO + 4.0)
        .max(MIN_LINE_NUMBER_GUTTER_WIDTH)
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
    start_at: usize,
) -> Vec<(Range<usize>, HighlightStyle)> {
    highlights
        .iter()
        .skip(start_at)
        .take_while(|(range, _)| range.start < line.end)
        .filter_map(|(range, style)| {
            let start = range.start.max(line.start);
            let end = range.end.min(line.end);
            (start < end).then(|| ((start - line.start)..(end - line.start), style.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_ranges_keep_empty_lines_and_trailing_line() {
        assert_eq!(
            display_line_ranges("a\n\nb\n"),
            vec![0..1, 2..2, 3..4, 5..5]
        );
    }

    #[test]
    fn line_number_gutter_expands_for_larger_line_numbers() {
        assert_eq!(line_number_gutter_width(9, 16.0), 16.0);
        assert_eq!(line_number_gutter_width(10, 16.0), 23.2);
        assert_eq!(line_number_gutter_width(1_000, 16.0), 42.4);
    }

    #[test]
    fn line_highlights_are_clipped_and_rebased() {
        let highlights = vec![(2..8, HighlightStyle::default())];
        let result = highlights_for_line(&highlights, &(5..10), 0);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, 0..3);
    }

    #[test]
    fn line_highlights_start_from_rolling_cursor() {
        let highlights = vec![
            (0..2, HighlightStyle::default()),
            (10..14, HighlightStyle::default()),
            (30..34, HighlightStyle::default()),
        ];
        let result = highlights_for_line(&highlights, &(11..13), 1);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, 0..2);
    }

    #[test]
    fn latex_validation_rejects_unbalanced_groups() {
        assert!(latex_formula_is_balanced(r"\frac{a}{b}"));
        assert!(!latex_formula_is_balanced(r"\frac{a}{b"));
        assert!(!latex_formula_is_balanced("a}"));
    }

    #[test]
    fn latex_preview_converts_fractions_symbols_and_scripts() {
        assert_eq!(
            latex_display_text(r"\frac{a^2+b^2}{c_2}=\pi"),
            "(a²+b²)⁄(c₂)=π"
        );
    }

    #[test]
    fn mermaid_edges_render_visible_node_labels() {
        assert_eq!(
            parse_mermaid_edge("A[Source] --> B(Component)"),
            Some(("Source".to_string(), "Component".to_string()))
        );
    }
}
