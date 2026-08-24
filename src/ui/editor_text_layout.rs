use crate::services::complex_markdown::{ComplexMarkdownKind, MarkdownTableCell};
use crate::services::syntax::{
    FencedCodeBlock, MarkdownLineKind, markdown_heading_levels, markdown_line_kinds,
};
use crate::ui::editor_projection::DisplayProjection;
use crate::ui::theme::Theme;
use gpui::{
    AnyElement, App, Bounds, Entity, HighlightStyle, ImageSource, MouseButton, ObjectFit, Pixels,
    Point, StyledText, Window, deferred, div, img, prelude::*, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::Sizable as _;
use gpui_gfm::{heading_font_size, heading_font_weight};
use std::ops::Range;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const EMPTY_LINE_PLACEHOLDER: &str = "\u{200B}";
const BODY_TEXT_LINE_HEIGHT_RATIO: f32 = 1.75;
const CODE_BLOCK_TOP_MARGIN: f32 = 8.0;
const CODE_BLOCK_TOP_PADDING: f32 = 12.0;
pub(crate) const EDITOR_HORIZONTAL_PADDING: f32 = 32.0;
const LIST_ITEM_BOTTOM_MARGIN: f32 = 4.0;
const LINE_NUMBER_GUTTER_GAP: f32 = 12.0;
const LINE_NUMBER_GUTTER_LEFT_INSET: f32 = 4.0;
const MIN_LINE_NUMBER_GUTTER_WIDTH: f32 = 16.0;
const MONOSPACE_DIGIT_WIDTH_RATIO: f32 = 0.6;
const TABLE_CELL_HORIZONTAL_PADDING: f32 = 10.0;
const TABLE_CELL_LINE_HEIGHT_RATIO: f32 = 1.618_034;
const TABLE_CELL_VERTICAL_PADDING: f32 = 7.0;

/// Receives source ranges when a rendered complex component is clicked.
pub type ComplexComponentClickHandler =
    Arc<dyn Fn(Range<usize>, bool, &mut Window, &mut App) + Send + Sync>;

/// Receives the stable document-order index when hover enters or leaves a code block.
pub type CodeBlockHoverHandler = Arc<dyn Fn(usize, bool, &mut App) + Send + Sync>;

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
    prepainted_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
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
        line.prepainted_bounds.lock().ok()?.as_ref()?;
        let local_index = index
            .saturating_sub(line.display_range.start)
            .min(line.display_range.len());
        line.layout.position_for_index(local_index)
    }

    pub fn line_height_for_index(&self, index: usize) -> Pixels {
        self.line_for_index(index)
            .filter(|line| {
                line.prepainted_bounds
                    .lock()
                    .is_ok_and(|bounds| bounds.is_some())
            })
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
            let Some(bounds) = line
                .prepainted_bounds
                .lock()
                .ok()
                .and_then(|bounds| *bounds)
            else {
                continue;
            };
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
    show_line_numbers: bool,
    document_directory: Option<&Path>,
    code_blocks: &[FencedCodeBlock],
    on_code_block_hover: CodeBlockHoverHandler,
    selected_component: Option<&Range<usize>>,
    on_component_click: ComplexComponentClickHandler,
    active_table_cell: Option<&ActiveTableCellEditor>,
    on_table_cell_click: TableCellClickHandler,
) -> EditorTextRender {
    let display_ranges = display_line_ranges(display_text);
    let heading_levels = markdown_heading_levels(source_text);
    let line_kinds = markdown_line_kinds(source_text);
    let source_line_starts = source_line_starts(source_text);
    let (line_number_gutter_width, left_padding) = line_number_gutter_layout(
        show_line_numbers,
        display_ranges.len(),
        body_font_size,
    );

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
        let source_byte = projection.display_to_source_byte(display_range.start);
        let source_line_index = source_line_index_for_byte(
            &source_line_starts,
            source_text.len(),
            source_byte,
        );
        let heading_level = heading_levels
            .get(source_line_index)
            .copied()
            .flatten();
        let line_kind = line_kinds
            .get(source_line_index)
            .copied()
            .unwrap_or(MarkdownLineKind::Paragraph);
        let source_line_start = source_line_starts
            .get(source_line_index)
            .copied()
            .unwrap_or(source_byte);
        let code_block_index = code_blocks
            .iter()
            .enumerate()
            .find(|block| {
                block.1.source_range.start <= source_byte
                    && source_byte < block.1.source_range.end
            })
            .map(|(index, _)| index);
        let starts_code_block = code_block_starts_on_source_line(
            code_blocks,
            code_block_index,
            source_line_start,
        );
        let complex_component = projection.complex_atom_for_display_line(display_range);
        let hides_component_label = complex_component
            .as_ref()
            .is_some_and(|(_, kind)| {
                matches!(
                    kind,
                    ComplexMarkdownKind::Image { .. } | ComplexMarkdownKind::Table { .. }
                )
            });

        let prepainted_bounds = Arc::new(Mutex::new(None));
        let mut line = editor_display_line(line_index, prepainted_bounds.clone());
        if let Some(level) = heading_level {
            line = line
                .text_size(heading_font_size(level))
                .font_weight(heading_font_weight(level));
        } else {
            line = line
                .text_size(px(body_font_size))
                .line_height(px(body_text_line_height(body_font_size)));
        }
        line = match line_kind {
            MarkdownLineKind::Quote => line.border_l_2().border_color(Theme::muted()).pl(px(12.)),
            MarkdownLineKind::CodeFence | MarkdownLineKind::CodeBody => {
                style_code_block_line(line, starts_code_block)
            }
            MarkdownLineKind::List { indent } | MarkdownLineKind::Task { indent } => {
                style_list_item_line(line, indent)
            }
            MarkdownLineKind::Paragraph | MarkdownLineKind::HorizontalRule => line,
        };
        if let Some(code_block_index) = code_block_index {
            let hover_handler = on_code_block_hover.clone();
            line = line.on_hover(move |hovered, _window, cx| {
                hover_handler(code_block_index, *hovered, cx);
            });
        }
        line = if hides_component_label {
            line.child(hidden_projection_layout_anchor(styled))
        } else {
            line.child(styled)
        };
        if let Some((source_range, kind)) = complex_component
            && is_block_component(&kind)
        {
            let is_table = matches!(kind, ComplexMarkdownKind::Table { .. });
            let is_selected = !is_table && selected_component == Some(&source_range);
            let uses_border = component_uses_border(&kind);
            let uses_panel = component_uses_panel(&kind);
            let repeat_click_edits_source = component_repeat_click_edits_source(&kind);
            let click_range = source_range.clone();
            let click_handler = on_component_click.clone();
            let source_click_range = source_range.clone();
            let source_click_handler = on_component_click.clone();
            line = if is_table {
                line.mb(px(8.))
            } else {
                line.mt(px(4.))
                    .mb(px(8.))
                    .when(uses_panel, |component| {
                        component
                            .p(px(10.))
                            .rounded(px(8.))
                            .bg(Theme::panel_alt())
                    })
                    .when(uses_border, |component| {
                        component.border_1().border_color(if is_selected {
                            Theme::accent()
                        } else {
                            Theme::border()
                        })
                    })
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        click_handler(
                            click_range.clone(),
                            repeat_click_edits_source,
                            window,
                            cx,
                        );
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
                line = if component_reveals_source_when_selected(&kind) {
                    line.child(
                        div()
                            .mt(px(8.))
                            .text_size(px(body_font_size))
                            .text_color(Theme::text())
                            .cursor_text()
                            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                                source_click_handler(
                                    source_click_range.clone(),
                                    true,
                                    window,
                                    cx,
                                );
                                cx.stop_propagation();
                            })
                            .child(
                                source_text
                                    .get(source_range.clone())
                                    .unwrap_or_default()
                                    .trim_end_matches(['\r', '\n'])
                                    .to_string(),
                            ),
                    )
                } else {
                    line.child(
                        div()
                            .mt(px(8.))
                            .text_xs()
                            .text_color(Theme::accent())
                            .child("Selected · click again to edit source"),
                    )
                };
            }
        }
        if show_line_numbers {
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
        }

        container = container.child(line);
        layouts.push(EditorLineLayout {
            display_range: display_range.clone(),
            layout,
            prepainted_bounds,
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

/// Returns whether a rendered component should use the shared outline treatment.
fn component_uses_border(kind: &ComplexMarkdownKind) -> bool {
    !matches!(kind, ComplexMarkdownKind::Image { .. })
}

/// Builds a stretchable editor row that preserves its measured block height while scrolling.
///
/// Width remains automatic so the parent flex column can stretch every row to the viewport;
/// a percentage width here would resolve against each row's intrinsic Markdown content.
fn editor_display_line(
    line_index: usize,
    prepainted_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .on_children_prepainted(move |children, _, _| {
            let first_child = children.first().copied();
            if let Ok(mut bounds) = prepainted_bounds.lock() {
                *bounds = first_child;
            }
        })
        .id(("editor-display-line", line_index))
        .relative()
        .min_w_0()
        .flex_shrink_0()
}

/// Keeps a collapsed component's text layout available for pointer mapping without showing it.
///
/// Images and tables replace their projection label with a native component. The editor still
/// needs that label's `TextLayout` to map clicks on every later line, so the anchor participates
/// in prepaint while absolute positioning prevents it from changing component height.
fn hidden_projection_layout_anchor(styled: StyledText) -> gpui::Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .invisible()
        .child(styled)
}

/// Returns whether a rendered component should use shared panel background and padding.
fn component_uses_panel(kind: &ComplexMarkdownKind) -> bool {
    !matches!(kind, ComplexMarkdownKind::Image { .. })
}

/// Returns whether first-click selection should expose source below the rendered component.
fn component_reveals_source_when_selected(kind: &ComplexMarkdownKind) -> bool {
    matches!(kind, ComplexMarkdownKind::Image { .. })
}

/// Holds the rendered text and byte ranges that should receive inline-code styling.
#[derive(Debug, PartialEq, Eq)]
struct TableCellDisplay {
    text: String,
    inline_code_ranges: Vec<Range<usize>>,
}

/// Projects Markdown code spans in a table cell into delimiter-free display text.
fn table_cell_display(source: &str) -> TableCellDisplay {
    let mut text = String::with_capacity(source.len());
    let mut inline_code_ranges = Vec::new();
    let bytes = source.as_bytes();
    let mut plain_start = 0;
    let mut cursor = 0;

    while cursor < bytes.len() {
        if bytes[cursor] != b'`' {
            cursor += 1;
            continue;
        }

        let delimiter_start = cursor;
        while cursor < bytes.len() && bytes[cursor] == b'`' {
            cursor += 1;
        }
        let delimiter_len = cursor - delimiter_start;
        let content_start = cursor;
        let mut candidate = cursor;
        let mut closing_delimiter = None;

        while candidate < bytes.len() {
            if bytes[candidate] != b'`' {
                candidate += 1;
                continue;
            }

            let run_start = candidate;
            while candidate < bytes.len() && bytes[candidate] == b'`' {
                candidate += 1;
            }
            if candidate - run_start == delimiter_len {
                closing_delimiter = Some((run_start, candidate));
                break;
            }
        }

        let Some((closing_start, closing_end)) = closing_delimiter else {
            continue;
        };

        text.push_str(&source[plain_start..delimiter_start]);
        let normalized = normalized_code_span_text(&source[content_start..closing_start]);
        let range_start = text.len();
        text.push_str(&normalized);
        inline_code_ranges.push(range_start..text.len());
        plain_start = closing_end;
        cursor = closing_end;
    }

    text.push_str(&source[plain_start..]);
    TableCellDisplay {
        text,
        inline_code_ranges,
    }
}

/// Applies CommonMark's whitespace normalization to rendered code-span content.
fn normalized_code_span_text(source: &str) -> String {
    let normalized = source.replace(['\n', '\r'], " ");
    if normalized.starts_with(' ')
        && normalized.ends_with(' ')
        && !normalized.chars().all(|character| character == ' ')
    {
        normalized[1..normalized.len() - 1].to_string()
    } else {
        normalized
    }
}

/// Builds styled table-cell text using the editor's shared inline-code colors.
fn render_table_cell_text(source: &str) -> StyledText {
    let display = table_cell_display(source);
    let markdown_style = Theme::markdown_style();
    let highlights = display.inline_code_ranges.into_iter().map(|range| {
        (
            range,
            HighlightStyle {
                color: Some(markdown_style.foreground.into()),
                background_color: Some(markdown_style.code_background.into()),
                ..Default::default()
            },
        )
    });

    StyledText::new(display.text).with_highlights(highlights)
}

/// Returns the shared pixel line height used by table display and inline editing.
fn table_cell_line_height(body_font_size: f32) -> f32 {
    body_font_size * TABLE_CELL_LINE_HEIGHT_RATIO
}

/// Returns the explicit line height used by wrapping body text in live Markdown.
fn body_text_line_height(body_font_size: f32) -> f32 {
    body_font_size * BODY_TEXT_LINE_HEIGHT_RATIO
}

/// Applies shared code-block styling and the opening line's internal top spacing.
fn style_code_block_line(
    line: gpui::Stateful<gpui::Div>,
    starts_code_block: bool,
) -> gpui::Stateful<gpui::Div> {
    line.when(starts_code_block, |line| {
        line.mt(px(CODE_BLOCK_TOP_MARGIN))
            .pt(px(CODE_BLOCK_TOP_PADDING))
    })
    .bg(Theme::code_block_bg())
    .px(px(12.))
}

/// Applies list indentation and a small inter-item gap without changing wrapped line height.
fn style_list_item_line(
    line: gpui::Stateful<gpui::Div>,
    indent: usize,
) -> gpui::Stateful<gpui::Div> {
    line.pl(px((indent as f32) * 4.0))
        .mb(px(LIST_ITEM_BOTTOM_MARGIN))
}

/// Returns whether the source line begins the referenced fenced code block.
fn code_block_starts_on_source_line(
    code_blocks: &[FencedCodeBlock],
    code_block_index: Option<usize>,
    source_line_start: usize,
) -> bool {
    code_block_index
        .and_then(|index| code_blocks.get(index))
        .is_some_and(|block| block.source_range.start == source_line_start)
}

/// Returns whether a second click should replace a component preview with editable source.
pub(crate) fn component_repeat_click_edits_source(kind: &ComplexMarkdownKind) -> bool {
    !matches!(kind, ComplexMarkdownKind::Image { .. })
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
                    .w_full()
                    .max_h(px(420.))
                    .object_fit(ObjectFit::Contain)
                    .with_loading(move || component_message(&format!("Loading {loading_label}…")))
                    .with_fallback(move || component_message(&fallback_label))
                    .into_any_element(),
            )
        }
        ComplexMarkdownKind::Table { headers, rows } => {
            let cell_line_height = table_cell_line_height(body_font_size);
            let column_count = headers
                .len()
                .max(rows.iter().map(Vec::len).max().unwrap_or(0))
                .max(1);
            let mut table = div()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
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
                        // Keep cell clicks away from the source editor while allowing the
                        // outer editor scroll container to receive wheel events over tables.
                        .block_mouse_except_scroll()
                        .min_w(px(80.))
                        .flex_1()
                        .px(px(TABLE_CELL_HORIZONTAL_PADDING))
                        .py(px(TABLE_CELL_VERTICAL_PADDING))
                        .line_height(px(cell_line_height))
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
                                        // The multiline input reserves 10px on its right for
                                        // caret scrolling. Use the cell's own right padding for
                                        // that reserve so editing and display wrap identically.
                                        .pl(px(TABLE_CELL_HORIZONTAL_PADDING))
                                        .pt(px(TABLE_CELL_VERTICAL_PADDING))
                                        .when(!input_visible, |editor| editor.opacity(0.0))
                                        .child(
                                            Input::new(&input)
                                                .xsmall()
                                                .px(px(0.))
                                                .h_full()
                                                .appearance(false)
                                                .bordered(false)
                                                .focus_bordered(false)
                                                .text_size(px(body_font_size))
                                                .line_height(px(cell_line_height)),
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
                                .child(div().w_full().child(render_table_cell_text(&cell.text)));
                        }
                    }
                    row_element = row_element.child(cell_element);
                }
                table = table.child(row_element.when(row_index > 0, |row| {
                    row.border_t_1().border_color(Theme::border())
                }));
            }
            Some(
                div()
                    .w_full()
                    .min_w_0()
                    .child(table)
                    .into_any_element(),
            )
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

/// Returns the line-number column width and editor inset for the active preference.
fn line_number_gutter_layout(
    show_line_numbers: bool,
    line_count: usize,
    body_font_size: f32,
) -> (f32, f32) {
    if !show_line_numbers {
        return (0.0, EDITOR_HORIZONTAL_PADDING);
    }

    let gutter_width = line_number_gutter_width(line_count, body_font_size);
    let left_padding = LINE_NUMBER_GUTTER_LEFT_INSET + gutter_width + LINE_NUMBER_GUTTER_GAP;
    (gutter_width, left_padding)
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

/// Returns the byte offset at which every source line begins.
fn source_line_starts(source: &str) -> Vec<usize> {
    let mut starts = Vec::with_capacity(source.bytes().filter(|byte| *byte == b'\n').count() + 1);
    starts.push(0);
    starts.extend(
        source
            .bytes()
            .enumerate()
            .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
    );
    starts
}

/// Resolves a source byte to its source-line ordinal using precomputed line starts.
fn source_line_index_for_byte(
    line_starts: &[usize],
    source_len: usize,
    source_byte: usize,
) -> usize {
    let source_byte = source_byte.min(source_len);
    line_starts
        .partition_point(|line_start| *line_start <= source_byte)
        .saturating_sub(1)
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
    fn hidden_line_numbers_keep_symmetric_content_padding_without_a_gutter() {
        assert_eq!(
            line_number_gutter_layout(false, 1_000, 16.0),
            (0.0, EDITOR_HORIZONTAL_PADDING)
        );
    }

    #[test]
    fn visible_line_numbers_keep_the_existing_gutter_inset() {
        assert_eq!(line_number_gutter_layout(true, 10, 16.0), (23.2, 39.2));
    }

    #[test]
    fn editor_display_lines_preserve_wrapped_block_height_without_intrinsic_width() {
        let mut line = editor_display_line(0, Arc::new(Mutex::new(None)));

        assert_eq!(line.style().flex_shrink, Some(0.0));
        assert_eq!(line.style().size.width, None);
    }

    #[test]
    fn pointer_mapping_skips_lines_that_have_not_been_prepainted() {
        let layout = StyledText::new("offscreen").layout().clone();
        let editor_layout = EditorTextLayout {
            lines: vec![EditorLineLayout {
                display_range: 0..9,
                layout,
                prepainted_bounds: Arc::new(Mutex::new(None)),
            }],
            display_len: 9,
        };

        assert!(editor_layout.closest_line(Point::default()).is_none());
        assert!(editor_layout.position_for_index(0).is_none());
        assert_eq!(editor_layout.line_height_for_index(0), px(0.));
    }

    #[test]
    fn collapsed_table_does_not_shift_heading_style_onto_later_code_comments() {
        use crate::services::complex_markdown::complex_markdown_components;
        use crate::services::syntax::{markdown_spans, markdown_heading_levels};

        let source = "| A | B |\n| --- | --- |\n| x | y |\n# Real heading\n```python\n# comment\n```\n";
        let spans = markdown_spans(source);
        let components = complex_markdown_components(source);
        let projection = DisplayProjection::from_markdown_with_components(
            source,
            &spans,
            &components,
            source.len()..source.len(),
        );
        let comment_display_byte = projection.display_text.find("# comment").unwrap();
        let display_line_index = projection.display_text[..comment_display_byte]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        let source_byte = projection.display_to_source_byte(comment_display_byte);
        let source_line_index = source_line_index_for_byte(
            &source_line_starts(source),
            source.len(),
            source_byte,
        );

        assert_eq!(display_line_index, 3);
        assert_eq!(markdown_heading_levels(source)[display_line_index], Some(1));
        assert_eq!(source_line_index, 5);
        assert_eq!(markdown_heading_levels(source)[source_line_index], None);
        assert_eq!(markdown_line_kinds(source)[source_line_index], MarkdownLineKind::CodeBody);
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

    #[test]
    fn image_preview_has_no_component_border_and_reveals_source_when_selected() {
        let image = ComplexMarkdownKind::Image {
            alt: "Logo".to_string(),
            target: "logo.png".to_string(),
        };

        assert!(!component_uses_border(&image));
        assert!(!component_uses_panel(&image));
        assert!(component_reveals_source_when_selected(&image));
        assert!(!component_repeat_click_edits_source(&image));

        let callout = ComplexMarkdownKind::Callout {
            kind: "NOTE".to_string(),
            title: "Note".to_string(),
            body: "Body".to_string(),
        };
        assert!(component_uses_border(&callout));
        assert!(component_uses_panel(&callout));
        assert!(!component_reveals_source_when_selected(&callout));
        assert!(component_repeat_click_edits_source(&callout));
    }

    #[test]
    fn table_cell_line_height_scales_consistently_with_body_font_size() {
        assert_eq!(table_cell_line_height(16.0), 25.888_544);
        assert_eq!(table_cell_line_height(20.0), 32.360_68);
    }

    #[test]
    fn table_cell_display_hides_inline_code_delimiters_and_tracks_utf8_ranges() {
        let display = table_cell_display("中文主站 `order.pinkeman.com` + `agent.pinkeman.com`");

        assert_eq!(
            display.text,
            "中文主站 order.pinkeman.com + agent.pinkeman.com"
        );
        assert_eq!(display.inline_code_ranges, vec![13..31, 34..52]);
    }

    #[test]
    fn table_cell_display_supports_matching_multi_backtick_delimiters() {
        let display = table_cell_display("prefix ``a ` b`` suffix");

        assert_eq!(display.text, "prefix a ` b suffix");
        assert_eq!(display.inline_code_ranges, vec![7..12]);
    }

    #[test]
    fn table_cell_display_keeps_unclosed_backticks_literal() {
        let display = table_cell_display("`production_en");

        assert_eq!(display.text, "`production_en");
        assert!(display.inline_code_ranges.is_empty());
    }

    #[test]
    fn body_text_line_height_leaves_space_between_wrapped_inline_backgrounds() {
        assert_eq!(body_text_line_height(16.0), 28.0);
        assert_eq!(body_text_line_height(20.0), 35.0);
    }

    #[test]
    fn list_items_use_the_same_spacious_body_line_height() {
        let kinds = markdown_line_kinds("- item with `code`\n  - nested `code`\n");

        assert!(matches!(kinds[0], MarkdownLineKind::List { .. }));
        assert!(matches!(kinds[1], MarkdownLineKind::List { .. }));
        assert_eq!(body_text_line_height(16.0), 28.0);
    }

    #[test]
    fn list_items_add_block_spacing_between_inline_code_backgrounds() {
        let mut line = style_list_item_line(div().id("list-item-line"), 2);

        assert_eq!(
            line.style().margin.bottom,
            Some(px(LIST_ITEM_BOTTOM_MARGIN).into())
        );
        assert_eq!(line.style().padding.left, Some(px(8.0).into()));
    }

    #[test]
    fn only_the_opening_fence_receives_code_block_top_spacing() {
        let source = "before\n```text\ncode\n```\nafter";
        let blocks = crate::services::syntax::fenced_code_blocks(source);

        assert!(code_block_starts_on_source_line(&blocks, Some(0), 7));
        assert!(!code_block_starts_on_source_line(&blocks, Some(0), 15));
        assert!(!code_block_starts_on_source_line(&blocks, None, 7));
    }

    #[test]
    fn only_the_opening_fence_receives_internal_code_block_top_padding() {
        let mut opening_line = style_code_block_line(div().id("opening-code-line"), true);
        let mut body_line = style_code_block_line(div().id("body-code-line"), false);

        assert_eq!(
            opening_line.style().padding.top,
            Some(px(CODE_BLOCK_TOP_PADDING).into())
        );
        assert_eq!(body_line.style().padding.top, Default::default());
    }
}
