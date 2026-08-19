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
}
