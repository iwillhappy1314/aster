//! Height estimation for markdown blocks.
//!
//! Used for virtual scrolling — lets the UI pre-allocate space for markdown
//! content without rendering it. All values are in logical pixels.

use crate::types::*;

// Layout constants (match the render module).
const BASE_BLOCK_GAP_PX: f32 = 8.0;
const HEADING_EXTRA_TOP_MARGIN_PX: f32 = 10.0;
const LIST_ITEM_GAP_PX: f32 = 4.0;
const LIST_LEFT_PADDING_PX: f32 = 10.0;
const LIST_MARKER_GAP_PX: f32 = 4.0;
const INDENT_PER_LEVEL_PX: f32 = 12.0;
const CHAR_WIDTH_PX: f32 = 8.8;
const MIN_WRAP_COLUMNS: usize = 8;
const CODE_BLOCK_VERTICAL_CHROME_PX: f32 = 18.0; // padding top + bottom + border
const CODE_BLOCK_MAX_HEIGHT_PX: f32 = 400.0;

/// Estimate the rendered height of a markdown source string.
pub fn estimate_markdown_height_px(source: &str, wrap_columns: usize, line_height_px: f32) -> f32 {
  let parsed = crate::parse::parse_markdown(source);
  estimate_parsed_markdown_height_px(&parsed, wrap_columns, line_height_px)
}

/// Estimate the rendered height of a pre-parsed markdown document.
pub fn estimate_parsed_markdown_height_px(
  parsed: &ParsedMarkdown,
  wrap_columns: usize,
  line_height_px: f32,
) -> f32 {
  estimate_blocks_height_px(parsed.blocks(), wrap_columns, line_height_px, 0)
}

/// Estimate height of a list of blocks.
fn estimate_blocks_height_px(
  blocks: &[Block],
  wrap_columns: usize,
  line_height_px: f32,
  indent: usize,
) -> f32 {
  if blocks.is_empty() {
    return 0.0;
  }

  let mut total = 0.0f32;
  for (ix, block) in blocks.iter().enumerate() {
    if ix > 0 {
      total += BASE_BLOCK_GAP_PX;
    }
    if ix > 0 && matches!(block, Block::Heading { .. }) {
      total += HEADING_EXTRA_TOP_MARGIN_PX;
    }
    total += estimate_block_height_px(block, wrap_columns, line_height_px, indent);
  }

  total.max(line_height_px)
}

/// Estimate height of a single block.
fn estimate_block_height_px(
  block: &Block,
  wrap_columns: usize,
  line_height_px: f32,
  indent: usize,
) -> f32 {
  match block {
    Block::Paragraph(inlines) => {
      let cols = wrap_columns_for_indent(wrap_columns, indent);
      estimate_inline_lines(inlines, cols) as f32 * line_height_px
    }
    Block::Heading { level, content } => {
      let cols = wrap_columns_for_indent(wrap_columns, indent);
      let lines = estimate_inline_lines(content, cols);
      let scale = heading_scale(*level);
      lines as f32 * (line_height_px * scale)
    }
    Block::List(list) => estimate_list_height_px(list, wrap_columns, line_height_px, indent),
    Block::CodeBlock(code) => {
      let lines = code.value.lines().count().max(1);
      let content_height = lines as f32 * line_height_px + CODE_BLOCK_VERTICAL_CHROME_PX;
      content_height.min(CODE_BLOCK_MAX_HEIGHT_PX)
    }
    Block::BlockQuote(children) => {
      estimate_blocks_height_px(children, wrap_columns, line_height_px, indent + 1)
    }
    Block::ThematicBreak => 1.0 + BASE_BLOCK_GAP_PX,
    Block::Table(table) => estimate_table_height_px(table, line_height_px),
    Block::Details(details) => {
      estimate_details_height_px(details, wrap_columns, line_height_px, indent)
    }
    Block::Aligned { blocks, .. } => {
      estimate_blocks_height_px(blocks, wrap_columns, line_height_px, indent)
    }
  }
}

/// Estimate list height.
fn estimate_list_height_px(
  list: &List,
  wrap_columns: usize,
  line_height_px: f32,
  indent: usize,
) -> f32 {
  if list.items.is_empty() {
    return 0.0;
  }

  let indent_cols =
    ((LIST_LEFT_PADDING_PX + LIST_MARKER_GAP_PX + 14.0) / CHAR_WIDTH_PX).ceil() as usize;
  let item_wrap_columns = wrap_columns_for_indent(wrap_columns, indent)
    .saturating_sub(indent_cols)
    .max(MIN_WRAP_COLUMNS);

  let mut total = 0.0f32;
  for (ix, item) in list.items.iter().enumerate() {
    if ix > 0 {
      total += LIST_ITEM_GAP_PX;
    }
    total += estimate_blocks_height_px(&item.blocks, item_wrap_columns, line_height_px, 0);
  }

  total.max(line_height_px)
}

/// Estimate details height.
fn estimate_details_height_px(
  details: &Details,
  wrap_columns: usize,
  line_height_px: f32,
  indent: usize,
) -> f32 {
  let summary_cols = wrap_columns_for_indent(wrap_columns, indent)
    .saturating_sub(3) // chevron icon width
    .max(MIN_WRAP_COLUMNS);
  let summary_height =
    estimate_inline_lines(&details.summary, summary_cols) as f32 * line_height_px;

  if !details.open {
    return summary_height;
  }

  let body = estimate_blocks_height_px(&details.blocks, wrap_columns, line_height_px, indent + 1);
  summary_height + BASE_BLOCK_GAP_PX + body
}

/// Height of an inline image in a table cell.
const TABLE_IMAGE_HEIGHT_PX: f32 = 18.0;
/// Vertical padding in a table cell.
const TABLE_CELL_PADDING_PX: f32 = 16.0;

/// Estimate table height.
fn estimate_table_height_px(table: &Table, line_height_px: f32) -> f32 {
  let header_height = estimate_table_row_height(&table.headers, line_height_px);
  let mut body_height = 0.0f32;
  for row in &table.rows {
    body_height += estimate_table_row_height(row, line_height_px) + 1.0; // +1 for border
  }
  header_height + 2.0 + body_height // +2 for header border
}

/// Estimate the height of a single table row.
fn estimate_table_row_height(cells: &[Vec<Inline>], line_height_px: f32) -> f32 {
  let mut max_height = line_height_px;
  for cell in cells {
    let cell_height = if cell_contains_image(cell) {
      TABLE_IMAGE_HEIGHT_PX
    } else {
      line_height_px
    };
    max_height = max_height.max(cell_height);
  }
  max_height + TABLE_CELL_PADDING_PX
}

/// Check whether a list of inlines contains an image.
fn cell_contains_image(inlines: &[Inline]) -> bool {
  inlines.iter().any(|inline| match inline {
    Inline::Image { .. } => true,
    Inline::Strong(children)
    | Inline::Emphasis(children)
    | Inline::Strikethrough(children)
    | Inline::Link {
      content: children, ..
    } => cell_contains_image(children),
    _ => false,
  })
}

/// Compute effective wrap columns at a given indent level.
fn wrap_columns_for_indent(base_wrap_columns: usize, indent: usize) -> usize {
  let indent_columns = ((indent as f32 * INDENT_PER_LEVEL_PX) / CHAR_WIDTH_PX).ceil() as usize;
  base_wrap_columns
    .saturating_sub(indent_columns)
    .max(MIN_WRAP_COLUMNS)
}

/// Estimate how many visual lines a set of inlines will occupy.
fn estimate_inline_lines(inlines: &[Inline], wrap_columns: usize) -> usize {
  let text = inline_to_plain_text(inlines);
  if text.is_empty() {
    return 1;
  }

  let mut lines = 0usize;
  for line in text.split('\n') {
    lines += estimate_wrapped_text_lines(line, wrap_columns);
  }
  lines.max(1)
}

/// Estimate how many visual lines a single text line occupies after word-level wrapping.
///
/// Words are split on whitespace boundaries. A word that exceeds the wrap width
/// on its own is force-broken across multiple lines.
fn estimate_wrapped_text_lines(line: &str, wrap_columns: usize) -> usize {
  let wrap_columns = wrap_columns.max(1);
  if line.is_empty() {
    return 1;
  }

  let mut lines = 1usize;
  let mut col = 0usize;

  for word in line.split_whitespace() {
    let word_len = word.chars().count();

    if col == 0 {
      // First word on the line — always place it.
      if word_len > wrap_columns {
        // Word itself is wider than the line; force-break it.
        lines += (word_len - 1) / wrap_columns;
        col = word_len % wrap_columns;
        if col == 0 {
          col = wrap_columns; // exactly fills the last line
        }
      } else {
        col = word_len;
      }
    } else {
      // Subsequent words need a space before them.
      let needed = 1 + word_len; // space + word
      if col + needed > wrap_columns {
        // Wrap to next line.
        lines += 1;
        if word_len > wrap_columns {
          lines += (word_len - 1) / wrap_columns;
          col = word_len % wrap_columns;
          if col == 0 {
            col = wrap_columns;
          }
        } else {
          col = word_len;
        }
      } else {
        col += needed;
      }
    }
  }

  lines
}

/// Font-size scale factor for headings by level.
fn heading_scale(level: u8) -> f32 {
  match level {
    1 => 1.35,
    2 => 1.2,
    3 => 1.05,
    _ => 1.0,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn empty_blocks_height_zero() {
    assert_eq!(estimate_blocks_height_px(&[], 80, 20.0, 0), 0.0);
  }

  #[test]
  fn single_paragraph_height() {
    let blocks = vec![Block::Paragraph(vec![Inline::Text("Hello".into())])];
    let h = estimate_blocks_height_px(&blocks, 80, 20.0, 0);
    assert!(h >= 20.0);
  }

  #[test]
  fn code_block_height_capped() {
    let code = "x\n".repeat(1000);
    let blocks = vec![Block::CodeBlock(CodeBlock {
      lang: None,
      value: code,
    })];
    let h = estimate_blocks_height_px(&blocks, 80, 20.0, 0);
    assert!(h <= CODE_BLOCK_MAX_HEIGHT_PX);
  }

  #[test]
  fn thematic_break_height() {
    let blocks = vec![Block::ThematicBreak];
    let h = estimate_blocks_height_px(&blocks, 80, 20.0, 0);
    assert!(h > 0.0);
    assert!(h <= 20.0);
  }

  #[test]
  fn details_closed_shorter_than_open() {
    let details_closed = Details {
      summary: vec![Inline::Text("Summary".into())],
      blocks: vec![Block::Paragraph(vec![Inline::Text("Body content".into())])],
      open: false,
    };
    let details_open = Details {
      summary: vec![Inline::Text("Summary".into())],
      blocks: vec![Block::Paragraph(vec![Inline::Text("Body content".into())])],
      open: true,
    };

    let h_closed = estimate_details_height_px(&details_closed, 80, 20.0, 0);
    let h_open = estimate_details_height_px(&details_open, 80, 20.0, 0);
    assert!(h_open > h_closed);
  }

  #[test]
  fn wrapping_increases_line_count() {
    let short = estimate_wrapped_text_lines("hello", 80);
    let long = estimate_wrapped_text_lines(&"x".repeat(200), 80);
    assert_eq!(short, 1);
    assert!(long > 1);
  }

  // ------ étape 7: heading scales per level ------

  #[test]
  fn heading_h1_taller_than_h3() {
    let content = vec![Inline::Text("Title".into())];
    let h1 = Block::Heading {
      level: 1,
      content: content.clone(),
    };
    let h3 = Block::Heading {
      level: 3,
      content: content.clone(),
    };
    let height_h1 = estimate_block_height_px(&h1, 80, 20.0, 0);
    let height_h3 = estimate_block_height_px(&h3, 80, 20.0, 0);
    assert!(
      height_h1 > height_h3,
      "h1 ({height_h1}) should be taller than h3 ({height_h3})"
    );
  }

  #[test]
  fn heading_h4_uses_base_line_height() {
    let content = vec![Inline::Text("Title".into())];
    let h4 = Block::Heading {
      level: 4,
      content: content.clone(),
    };
    // scale = 1.0, single line → 1 * 20.0 * 1.0 = 20.0
    let height = estimate_block_height_px(&h4, 80, 20.0, 0);
    assert_eq!(height, 20.0);
  }

  #[test]
  fn heading_scales_are_monotonic() {
    assert!(heading_scale(1) > heading_scale(2));
    assert!(heading_scale(2) > heading_scale(3));
    assert!(heading_scale(3) > heading_scale(4));
    assert_eq!(heading_scale(4), heading_scale(5));
    assert_eq!(heading_scale(5), heading_scale(6));
  }

  // ------ étape 7: word-level wrapping ------

  #[test]
  fn word_wrap_does_not_split_words() {
    // "hello world" at width 8: "hello" (5) fits, then "world" (5) needs 1+5=6 → 5+6=11 > 8
    // So wraps: line1="hello", line2="world" → 2 lines.
    assert_eq!(estimate_wrapped_text_lines("hello world", 8), 2);
  }

  #[test]
  fn word_wrap_keeps_fitting_words_on_same_line() {
    // "a b c" at width 10: a(1) + " b"(2) = 3 + " c"(2) = 5 → fits in 10.
    assert_eq!(estimate_wrapped_text_lines("a b c", 10), 1);
  }

  #[test]
  fn word_wrap_long_word_force_breaks() {
    // "abcdefghij" (10 chars) at width 4 → ceil(10/4) = 3 lines.
    assert_eq!(estimate_wrapped_text_lines("abcdefghij", 4), 3);
  }

  #[test]
  fn word_wrap_empty_line_is_one_line() {
    assert_eq!(estimate_wrapped_text_lines("", 80), 1);
  }

  #[test]
  fn word_wrap_exact_fit() {
    // "abcd efgh" at width 9: "abcd"(4) + " efgh"(5) = 9 → fits exactly.
    assert_eq!(estimate_wrapped_text_lines("abcd efgh", 9), 1);
  }

  // ------ étape 7: table with images ------

  #[test]
  fn table_with_image_cell_uses_image_height() {
    let table = Table {
      headers: vec![
        vec![Inline::Text("Name".into())],
        vec![Inline::Text("Badge".into())],
      ],
      rows: vec![vec![
        vec![Inline::Text("foo".into())],
        vec![Inline::Image {
          url: "https://example.com/badge.png".into(),
          title: None,
          alt: "badge".into(),
          width: None,
          height: None,
          dark_url: None,
          light_url: None,
        }],
      ]],
    };

    let h_with_image = estimate_table_height_px(&table, 20.0);

    // Compare to table without images
    let table_text = Table {
      headers: vec![
        vec![Inline::Text("Name".into())],
        vec![Inline::Text("Badge".into())],
      ],
      rows: vec![vec![
        vec![Inline::Text("foo".into())],
        vec![Inline::Text("bar".into())],
      ]],
    };
    let h_text_only = estimate_table_height_px(&table_text, 20.0);

    // Both should be > 0 and reasonably close (image 18px vs text 20px in this case)
    assert!(h_with_image > 0.0);
    assert!(h_text_only > 0.0);
  }

  #[test]
  fn cell_contains_image_detects_nested() {
    let inlines = vec![Inline::Strong(vec![Inline::Image {
      url: "x".into(),
      title: None,
      alt: "".into(),
      width: None,
      height: None,
      dark_url: None,
      light_url: None,
    }])];
    assert!(cell_contains_image(&inlines));
  }

  #[test]
  fn cell_contains_image_false_for_text() {
    let inlines = vec![Inline::Text("hello".into())];
    assert!(!cell_contains_image(&inlines));
  }
}
