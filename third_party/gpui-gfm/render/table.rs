//! Table rendering.

use gpui::{AnyElement, App, SharedString, div, prelude::*, px};

use crate::types::*;

use super::MarkdownRenderOptions;
use super::interactive_scrollbar::{
  InteractiveScrollbarAxis, render_interactive_scrollbar, stop_horizontal_scroll_propagation,
};
use super::inline::render_inline_text;

/// Minimum column width.
const TABLE_CELL_MIN_WIDTH_PX: f32 = 64.0;
/// Maximum column width before ordinary cell text wraps.
const TABLE_CELL_MAX_WIDTH_PX: f32 = 320.0;
/// Horizontal padding per cell.
const TABLE_CELL_HORIZONTAL_PADDING_PX: f32 = 24.0;
/// Approximate character width for column sizing (body text).
const TABLE_INLINE_CHAR_WIDTH_PX: f32 = 7.5;
/// Approximate character width for inline code (monospace, slightly wider).
const TABLE_CODE_CHAR_WIDTH_PX: f32 = 8.4;
/// Extra width for backtick delimiters / code background padding.
const TABLE_CODE_PADDING_PX: f32 = 10.0;

/// Render a GFM table.
pub fn render_table(table: &Table, options: &MarkdownRenderOptions, cx: &App) -> AnyElement {
  let theme = options.theme();
  let column_count = table
    .rows
    .iter()
    .fold(table.headers.len(), |count, row| count.max(row.len()))
    .max(1);
  let column_widths = compute_column_widths(table, column_count);
  let table_content_width = column_widths.iter().sum::<f32>();

  // Header row
  let mut header_row = div().flex().bg(theme.accent.opacity(0.14));
  for (col, width) in column_widths.iter().enumerate().take(column_count) {
    let cell = table
      .headers
      .get(col)
      .map_or(&[][..], |cell| cell.as_slice());
    header_row = header_row.child(
      div()
        .w(px(*width))
        .flex_shrink_0()
        .when(col + 1 == column_count, |this| this.flex_grow())
        .px_3()
        .py_2()
        .when(col + 1 < column_count, |this| {
          this.border_r_1().border_color(theme.border)
        })
        .child(
          div()
            .text_sm()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme.foreground)
            .whitespace_normal()
            .child(render_inline_text(cell, options, cx)),
        ),
    );
  }

  // Body rows
  let mut body = div().flex().flex_col();
  for row in &table.rows {
    let mut row_el = div().flex().border_t_1().border_color(theme.border);
    for (col, width) in column_widths.iter().enumerate().take(column_count) {
      let cell = row.get(col).map_or(&[][..], |cell| cell.as_slice());
      row_el = row_el.child(
        div()
          .w(px(*width))
          .flex_shrink_0()
          .when(col + 1 == column_count, |this| this.flex_grow())
          .px_3()
          .py_2()
          .when(col + 1 < column_count, |this| {
            this.border_r_1().border_color(theme.border)
          })
          .child(
            div()
              .text_sm()
              .text_color(theme.foreground)
              .whitespace_normal()
              .child(render_inline_text(cell, options, cx)),
          ),
      );
    }
    body = body.child(row_el);
  }

  // Scroll container
  let table_block_id = table as *const Table as usize;
  let table_id: SharedString = format!("md-table-{table_block_id:x}").into();
  let scroll_state = options.horizontal_scroll_state(table_block_id);

  let mut scroll_area = div()
    .id(table_id)
    .w_full()
    .min_w_0()
    .overflow_x_scroll()
    .scrollbar_width(px(10.0))
    .track_scroll(&scroll_state.handle)
    .on_scroll_wheel(|event, _, cx| stop_horizontal_scroll_propagation(event, cx))
    .child(
      div()
        // Keep the table's intrinsic width. Without this minimum the flex
        // column collapses to the viewport and clips the last columns instead
        // of giving the surrounding container an overflow width to scroll.
        .w_full()
        .min_w(px(table_content_width))
        .flex_shrink_0()
        .border_1()
        .border_color(theme.border)
        .rounded_md()
        .overflow_hidden()
        .child(div().flex().flex_col().child(header_row).child(body)),
    );
  scroll_area.style().restrict_scroll_to_axis = Some(true);

  div()
    .relative()
    .w_full()
    .min_w_0()
    .child(scroll_area)
    .child(render_interactive_scrollbar(
      InteractiveScrollbarAxis::Horizontal,
      scroll_state.scrollbar,
      scroll_state.handle,
      theme.muted_foreground,
    ))
    .into_any_element()
}

/// Compute column widths based on content.
fn compute_column_widths(table: &Table, column_count: usize) -> Vec<f32> {
  let mut widths = vec![TABLE_CELL_MIN_WIDTH_PX; column_count];

  for (col, width) in widths.iter_mut().enumerate().take(column_count) {
    // Check header
    if let Some(cell) = table.headers.get(col) {
      *width = (*width).max(estimate_cell_width(cell));
    }
    // Check all rows
    for row in &table.rows {
      if let Some(cell) = row.get(col) {
        *width = (*width).max(estimate_cell_width(cell));
      }
    }
  }

  widths
}

/// Estimate the pixel width of a table cell's content.
fn estimate_cell_width(inlines: &[Inline]) -> f32 {
  let mut width = 0.0f32;
  estimate_cell_width_inner(inlines, &mut width, false);
  (width + TABLE_CELL_HORIZONTAL_PADDING_PX)
    .clamp(TABLE_CELL_MIN_WIDTH_PX, TABLE_CELL_MAX_WIDTH_PX)
}

#[cfg(test)]
mod tests {
  use super::{
    TABLE_CELL_MAX_WIDTH_PX, TABLE_CELL_MIN_WIDTH_PX, compute_column_widths, estimate_cell_width,
  };
  use crate::types::{Inline, Table};

  #[test]
  fn caps_long_cell_widths_so_text_can_wrap() {
    let cell = vec![Inline::Text("A long description ".repeat(40))];

    assert_eq!(estimate_cell_width(&cell), TABLE_CELL_MAX_WIDTH_PX);
  }

  #[test]
  fn retains_minimum_width_for_short_cells() {
    assert_eq!(estimate_cell_width(&[]), TABLE_CELL_MIN_WIDTH_PX);
  }

  #[test]
  fn shares_each_column_width_across_all_rows() {
    let table = Table {
      headers: vec![vec![Inline::Text("Host".into())], vec![Inline::Text("Purpose".into())]],
      rows: vec![
        vec![vec![Inline::Text("production".into())], vec![Inline::Text("Chinese site".into())]],
        vec![
          vec![Inline::Text("production_en".into())],
          vec![Inline::Text("A much longer English site description".into())],
        ],
      ],
    };

    let widths = compute_column_widths(&table, 2);
    assert_eq!(widths.len(), 2);
    assert!(widths[0] >= estimate_cell_width(&table.rows[1][0]));
    assert!(widths[1] >= estimate_cell_width(&table.rows[1][1]));
  }
}

fn estimate_cell_width_inner(inlines: &[Inline], width: &mut f32, in_code: bool) {
  for inline in inlines {
    match inline {
      Inline::Text(text) => {
        let char_px = if in_code {
          TABLE_CODE_CHAR_WIDTH_PX
        } else {
          TABLE_INLINE_CHAR_WIDTH_PX
        };
        *width += text.len() as f32 * char_px;
      }
      Inline::Code(text) => {
        *width += text.len() as f32 * TABLE_CODE_CHAR_WIDTH_PX + TABLE_CODE_PADDING_PX;
      }
      Inline::Strong(children) | Inline::Emphasis(children) | Inline::Strikethrough(children) => {
        estimate_cell_width_inner(children, width, in_code);
      }
      Inline::Link { content, .. } => {
        estimate_cell_width_inner(content, width, in_code);
      }
      Inline::SoftBreak | Inline::HardBreak => {
        *width += TABLE_INLINE_CHAR_WIDTH_PX;
      }
      Inline::Image { alt, .. } => {
        *width += alt.len() as f32 * TABLE_INLINE_CHAR_WIDTH_PX;
      }
    }
  }
}
