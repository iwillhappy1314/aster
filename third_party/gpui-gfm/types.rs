//! Core data types for the GFM intermediate representation.
//!
//! The parsing layer converts markdown source into a tree of [`Block`] and [`Inline`] nodes.
//! The rendering layer walks this tree to produce GPUI elements.

use std::sync::Arc;

// ---------------------------------------------------------------------------
// Block-level nodes
// ---------------------------------------------------------------------------

/// A block-level markdown node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
  /// A paragraph containing inline content.
  Paragraph(Vec<Inline>),

  /// A heading with level (1–6) and inline content.
  Heading { level: u8, content: Vec<Inline> },

  /// An ordered or unordered list.
  List(List),

  /// A fenced or indented code block.
  CodeBlock(CodeBlock),

  /// A block quote containing nested blocks.
  BlockQuote(Vec<Block>),

  /// A thematic break (`---`, `***`, `___`).
  ThematicBreak,

  /// A GFM pipe table.
  Table(Table),

  /// An HTML `<details>/<summary>` collapsible section.
  Details(Details),

  /// Content wrapped in a centered `<div>`.
  Aligned { center: bool, blocks: Vec<Block> },
}

/// A fenced or indented code block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeBlock {
  /// The language hint from the info string (e.g. `rust`, `js`).
  pub lang: Option<String>,
  /// The raw code content.
  pub value: String,
}

/// A GFM pipe table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
  /// Header cells — each cell is a list of inlines.
  pub headers: Vec<Vec<Inline>>,
  /// Body rows — each row is a list of cells, each cell a list of inlines.
  pub rows: Vec<Vec<Vec<Inline>>>,
}

/// An HTML `<details>` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Details {
  /// The `<summary>` content as inlines.
  pub summary: Vec<Inline>,
  /// The body content as blocks.
  pub blocks: Vec<Block>,
  /// Whether the `<details>` tag had the `open` attribute.
  pub open: bool,
}

/// A list (ordered or unordered).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct List {
  /// `true` for ordered lists (`1.`, `2.`, …).
  pub ordered: bool,
  /// The starting number for ordered lists.
  pub start: Option<u64>,
  /// The list items.
  pub items: Vec<ListItem>,
}

/// A single list item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
  /// The block content of this item.
  pub blocks: Vec<Block>,
  /// Task-list checkbox state: `Some(true)` = checked, `Some(false)` = unchecked, `None` = no checkbox.
  pub checked: Option<bool>,
}

// ---------------------------------------------------------------------------
// Inline nodes
// ---------------------------------------------------------------------------

/// An inline markdown node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
  /// Plain text.
  Text(String),

  /// A hyperlink.
  Link {
    url: String,
    title: Option<String>,
    content: Vec<Inline>,
  },

  /// An image.
  Image {
    url: String,
    title: Option<String>,
    alt: String,
    width: Option<String>,
    height: Option<String>,
    /// URL for dark-mode variant (from `<picture>` element).
    dark_url: Option<String>,
    /// URL for light-mode variant (from `<picture>` element).
    light_url: Option<String>,
  },

  /// Inline code span.
  Code(String),

  /// A soft line break (rendered as a space).
  SoftBreak,

  /// A hard line break (rendered as a newline).
  HardBreak,

  /// Bold / strong emphasis.
  Strong(Vec<Inline>),

  /// Italic emphasis.
  Emphasis(Vec<Inline>),

  /// ~~Strikethrough~~ text.
  Strikethrough(Vec<Inline>),
}

// ---------------------------------------------------------------------------
// Parsed document wrapper
// ---------------------------------------------------------------------------

/// A parsed markdown document, ready for rendering or height estimation.
///
/// The inner block list is wrapped in an `Arc` so that cloning is cheap and
/// the parsed result can be shared across frames without re-parsing.
#[derive(Clone, Debug)]
pub struct ParsedMarkdown {
  pub(crate) blocks: Arc<Vec<Block>>,
}

impl ParsedMarkdown {
  /// Create a new parsed document from a list of blocks.
  pub fn new(blocks: Vec<Block>) -> Self {
    Self {
      blocks: Arc::new(blocks),
    }
  }

  /// Access the block list.
  pub fn blocks(&self) -> &[Block] {
    &self.blocks
  }
}

/// Splits unusually large top-level render units while preserving their Markdown styling.
///
/// Virtual scrolling cannot bound layout memory when an entire document parses as one paragraph,
/// code block, list, or quote. This creates equivalent smaller IR blocks for that large-file path.
pub fn virtualize_parsed_markdown(parsed: &ParsedMarkdown, max_block_bytes: usize) -> ParsedMarkdown {
  let max_block_bytes = max_block_bytes.max(256);
  ParsedMarkdown::new(
    parsed
      .blocks()
      .iter()
      .cloned()
      .flat_map(|block| split_block(block, max_block_bytes))
      .collect(),
  )
}

fn split_block(block: Block, max_bytes: usize) -> Vec<Block> {
  if block_payload_len(&block) <= max_bytes {
    return vec![block];
  }

  match block {
    Block::Paragraph(inlines) => chunk_inlines(inlines, max_bytes)
      .into_iter()
      .map(Block::Paragraph)
      .collect(),
    Block::Heading { level, content } => chunk_inlines(content, max_bytes)
      .into_iter()
      .enumerate()
      .map(|(index, content)| {
        if index == 0 {
          Block::Heading { level, content }
        } else {
          Block::Paragraph(content)
        }
      })
      .collect(),
    Block::CodeBlock(code) => chunk_string(code.value, max_bytes)
      .into_iter()
      .map(|value| {
        Block::CodeBlock(CodeBlock {
          lang: code.lang.clone(),
          value,
        })
      })
      .collect(),
    Block::List(list) => {
      let mut split_items = Vec::new();
      for item in list.items {
        let blocks = item
          .blocks
          .into_iter()
          .flat_map(|block| split_block(block, max_bytes))
          .collect::<Vec<_>>();
        for blocks in chunk_blocks(blocks, max_bytes) {
          split_items.push(ListItem {
            blocks,
            checked: item.checked,
          });
        }
      }
      let mut result = Vec::new();
      let mut consumed = 0u64;
      for items in chunk_list_items(split_items, max_bytes) {
        let item_count = items.len() as u64;
        result.push(Block::List(List {
          ordered: list.ordered,
          start: list.start.map(|start| start + consumed),
          items,
        }));
        consumed += item_count;
      }
      result
    }
    Block::BlockQuote(blocks) => chunk_blocks(
      blocks
        .into_iter()
        .flat_map(|block| split_block(block, max_bytes))
        .collect(),
      max_bytes,
    )
    .into_iter()
    .map(Block::BlockQuote)
    .collect(),
    Block::Details(details) => {
      let summaries = chunk_inlines(details.summary, max_bytes);
      let mut bodies = chunk_blocks(
        details
        .blocks
        .into_iter()
        .flat_map(|block| split_block(block, max_bytes))
        .collect::<Vec<_>>(),
        max_bytes,
      );
      if bodies.is_empty() {
        bodies.push(Vec::new());
      }
      let part_count = summaries.len().max(bodies.len());
      (0..part_count)
        .map(|index| {
          Block::Details(Details {
            summary: summaries.get(index).cloned().unwrap_or_default(),
            blocks: bodies.get(index).cloned().unwrap_or_default(),
            open: details.open,
          })
        })
        .collect()
    }
    Block::Aligned { center, blocks } => chunk_blocks(
      blocks
        .into_iter()
        .flat_map(|block| split_block(block, max_bytes))
        .collect(),
      max_bytes,
    )
    .into_iter()
    .map(|blocks| Block::Aligned { center, blocks })
    .collect(),
    Block::Table(table) => split_table(table, max_bytes),
    other => vec![other],
  }
}

fn chunk_inlines(inlines: Vec<Inline>, max_bytes: usize) -> Vec<Vec<Inline>> {
  let atoms = inlines
    .into_iter()
    .flat_map(|inline| split_inline(inline, max_bytes))
    .collect::<Vec<_>>();
  let mut chunks = Vec::new();
  let mut current = Vec::new();
  let mut current_len = 0usize;
  for inline in atoms {
    let len = inline_payload_len(&inline).max(1);
    if !current.is_empty() && current_len + len > max_bytes {
      chunks.push(std::mem::take(&mut current));
      current_len = 0;
    }
    current_len += len;
    current.push(inline);
  }
  if !current.is_empty() {
    chunks.push(current);
  }
  chunks
}

fn split_inline(inline: Inline, max_bytes: usize) -> Vec<Inline> {
  match inline {
    Inline::Text(text) => chunk_string(text, max_bytes)
      .into_iter()
      .map(Inline::Text)
      .collect(),
    Inline::Code(text) => chunk_string(text, max_bytes)
      .into_iter()
      .map(Inline::Code)
      .collect(),
    Inline::Strong(children) => chunk_inlines(children, max_bytes)
      .into_iter()
      .map(Inline::Strong)
      .collect(),
    Inline::Emphasis(children) => chunk_inlines(children, max_bytes)
      .into_iter()
      .map(Inline::Emphasis)
      .collect(),
    Inline::Strikethrough(children) => chunk_inlines(children, max_bytes)
      .into_iter()
      .map(Inline::Strikethrough)
      .collect(),
    Inline::Link {
      url,
      title,
      content,
    } => chunk_inlines(content, max_bytes)
      .into_iter()
      .map(|content| Inline::Link {
        url: url.clone(),
        title: title.clone(),
        content,
      })
      .collect(),
    other => vec![other],
  }
}

fn chunk_string(text: String, max_bytes: usize) -> Vec<String> {
  if text.len() <= max_bytes {
    return vec![text];
  }
  let mut chunks = Vec::new();
  let mut start = 0usize;
  while start < text.len() {
    let mut end = (start + max_bytes).min(text.len());
    while end > start && !text.is_char_boundary(end) {
      end -= 1;
    }
    if end == start {
      end = text[start..]
        .char_indices()
        .nth(1)
        .map_or(text.len(), |(offset, _)| start + offset);
    }
    if let Some(newline) = text[start..end].rfind('\n')
      && newline >= max_bytes / 2
    {
      end = start + newline + 1;
    }
    chunks.push(text[start..end].to_string());
    start = end;
  }
  chunks
}

fn chunk_blocks(blocks: Vec<Block>, max_bytes: usize) -> Vec<Vec<Block>> {
  chunk_by_size(blocks, max_bytes, block_payload_len)
}

fn chunk_list_items(items: Vec<ListItem>, max_bytes: usize) -> Vec<Vec<ListItem>> {
  chunk_by_size(items, max_bytes, |item| {
    item.blocks.iter().map(block_payload_len).sum()
  })
}

fn split_table(table: Table, max_bytes: usize) -> Vec<Block> {
  let column_count = table
    .rows
    .iter()
    .fold(table.headers.len(), |count, row| count.max(row.len()))
    .max(1);
  let cell_max = (max_bytes / column_count).max(256);
  let mut header_rows = split_table_row(table.headers, cell_max);
  let headers = header_rows
    .first()
    .cloned()
    .unwrap_or_else(|| vec![Vec::new(); column_count]);
  let mut rows = header_rows.drain(1..).collect::<Vec<_>>();
  rows.extend(
    table
      .rows
      .into_iter()
      .flat_map(|row| split_table_row(row, cell_max)),
  );

  let mut result = Vec::new();
  let mut current_rows = Vec::new();
  let mut current_len = 0usize;
  for row in rows {
    let row_len = row.iter().flatten().map(inline_payload_len).sum::<usize>();
    if !current_rows.is_empty() && current_len + row_len > max_bytes {
      result.push(Block::Table(Table {
        headers: headers.clone(),
        rows: std::mem::take(&mut current_rows),
      }));
      current_len = 0;
    }
    current_len += row_len;
    current_rows.push(row);
  }
  if !current_rows.is_empty() || result.is_empty() {
    result.push(Block::Table(Table {
      headers,
      rows: current_rows,
    }));
  }
  result
}

fn split_table_row(row: Vec<Vec<Inline>>, cell_max: usize) -> Vec<Vec<Vec<Inline>>> {
  let cells = row
    .into_iter()
    .map(|cell| {
      let chunks = chunk_inlines(cell, cell_max);
      if chunks.is_empty() {
        vec![Vec::new()]
      } else {
        chunks
      }
    })
    .collect::<Vec<_>>();
  let part_count = cells.iter().map(Vec::len).max().unwrap_or(1);
  (0..part_count)
    .map(|part| {
      cells
        .iter()
        .map(|cell| cell.get(part).cloned().unwrap_or_default())
        .collect()
    })
    .collect()
}

fn chunk_by_size<T>(items: Vec<T>, max_bytes: usize, size: impl Fn(&T) -> usize) -> Vec<Vec<T>> {
  let mut chunks = Vec::new();
  let mut current = Vec::new();
  let mut current_len = 0usize;
  for item in items {
    let len = size(&item).max(1);
    if !current.is_empty() && current_len + len > max_bytes {
      chunks.push(std::mem::take(&mut current));
      current_len = 0;
    }
    current_len += len;
    current.push(item);
  }
  if !current.is_empty() {
    chunks.push(current);
  }
  chunks
}

fn block_payload_len(block: &Block) -> usize {
  match block {
    Block::Paragraph(inlines) | Block::Heading { content: inlines, .. } => {
      inlines.iter().map(inline_payload_len).sum()
    }
    Block::List(list) => list
      .items
      .iter()
      .flat_map(|item| item.blocks.iter())
      .map(block_payload_len)
      .sum(),
    Block::CodeBlock(code) => code.value.len(),
    Block::BlockQuote(blocks) | Block::Aligned { blocks, .. } => {
      blocks.iter().map(block_payload_len).sum()
    }
    Block::Table(table) => table
      .headers
      .iter()
      .chain(table.rows.iter().flatten())
      .flatten()
      .map(inline_payload_len)
      .sum(),
    Block::Details(details) => {
      details.summary.iter().map(inline_payload_len).sum::<usize>()
        + details.blocks.iter().map(block_payload_len).sum::<usize>()
    }
    Block::ThematicBreak => 1,
  }
}

fn inline_payload_len(inline: &Inline) -> usize {
  match inline {
    Inline::Text(text) | Inline::Code(text) => text.len(),
    Inline::Link { content, .. }
    | Inline::Strong(content)
    | Inline::Emphasis(content)
    | Inline::Strikethrough(content) => content.iter().map(inline_payload_len).sum(),
    Inline::Image { alt, .. } => alt.len().max(1),
    Inline::SoftBreak | Inline::HardBreak => 1,
  }
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// Flatten inlines into plain text (for alt text, height estimation, etc.).
pub fn inline_to_plain_text(inlines: &[Inline]) -> String {
  let mut text = String::new();
  inline_to_plain_text_inner(inlines, &mut text);
  text
}

fn inline_to_plain_text_inner(inlines: &[Inline], out: &mut String) {
  for inline in inlines {
    match inline {
      Inline::Text(value) => out.push_str(value),
      Inline::Code(value) => out.push_str(value),
      Inline::SoftBreak => out.push(' '),
      Inline::HardBreak => out.push('\n'),
      Inline::Strong(children) | Inline::Emphasis(children) | Inline::Strikethrough(children) => {
        inline_to_plain_text_inner(children, out);
      }
      Inline::Link { content, .. } => {
        inline_to_plain_text_inner(content, out);
      }
      Inline::Image { alt, .. } => out.push_str(alt),
    }
  }
}

/// Merge adjacent `Inline::Text` nodes to reduce allocations during rendering.
pub fn merge_adjacent_text(inlines: Vec<Inline>) -> Vec<Inline> {
  let mut merged: Vec<Inline> = Vec::with_capacity(inlines.len());
  for inline in inlines {
    match (&mut merged.last_mut(), &inline) {
      (Some(Inline::Text(existing)), Inline::Text(new_text)) => {
        existing.push_str(new_text);
      }
      _ => merged.push(inline),
    }
  }
  merged
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn inline_to_plain_text_basic() {
    let inlines = vec![
      Inline::Text("Hello ".into()),
      Inline::Strong(vec![Inline::Text("world".into())]),
      Inline::Text("!".into()),
    ];
    assert_eq!(inline_to_plain_text(&inlines), "Hello world!");
  }

  #[test]
  fn inline_to_plain_text_with_breaks() {
    let inlines = vec![
      Inline::Text("line1".into()),
      Inline::HardBreak,
      Inline::Text("line2".into()),
    ];
    assert_eq!(inline_to_plain_text(&inlines), "line1\nline2");
  }

  #[test]
  fn merge_adjacent_text_nodes() {
    let inlines = vec![
      Inline::Text("a".into()),
      Inline::Text("b".into()),
      Inline::Code("c".into()),
      Inline::Text("d".into()),
    ];
    let merged = merge_adjacent_text(inlines);
    assert_eq!(
      merged,
      vec![
        Inline::Text("ab".into()),
        Inline::Code("c".into()),
        Inline::Text("d".into()),
      ]
    );
  }

  #[test]
  fn virtualized_ir_splits_one_oversized_styled_paragraph() {
    let parsed = ParsedMarkdown::new(vec![Block::Paragraph(vec![Inline::Strong(vec![
      Inline::Text("中".repeat(1_000)),
    ])])]);
    let virtualized = virtualize_parsed_markdown(&parsed, 256);

    assert!(virtualized.blocks().len() > 1);
    assert!(
      virtualized
        .blocks()
        .iter()
        .all(|block| block_payload_len(block) <= 256)
    );
  }
}
