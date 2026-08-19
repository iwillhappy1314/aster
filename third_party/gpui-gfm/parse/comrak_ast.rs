//! comrak AST → Block/Inline IR conversion.
//!
//! This module takes a markdown source string, feeds it to comrak's
//! `parse_document`, and converts the resulting AST into our IR types.

use comrak::{
  Arena, Options,
  nodes::{AstNode, ListType, NodeValue},
  parse_document,
};

use crate::types::*;

use super::html;

/// Parse markdown source with comrak and convert to our block IR.
pub fn parse_comrak(source: &str) -> Vec<Block> {
  let arena = Arena::new();
  let options = comrak_options();
  let root = parse_document(&arena, source, &options);
  let mut blocks = Vec::new();
  let mut centered_div_depth = 0usize;
  let mut centered_div_blocks: Vec<Block> = Vec::new();

  for node in root.children() {
    let mut is_centered_open = false;
    let mut is_centered_close = false;

    if let NodeValue::HtmlBlock(ref html_block) = node.data.borrow().value {
      is_centered_open = html::is_centered_div_open(&html_block.literal);
      is_centered_close = html::is_centered_div_close(&html_block.literal);
    }

    if is_centered_open {
      centered_div_depth = centered_div_depth.saturating_add(1);
      continue;
    }

    if is_centered_close {
      if centered_div_depth > 0 {
        centered_div_depth -= 1;
      }
      if centered_div_depth == 0 && !centered_div_blocks.is_empty() {
        blocks.push(Block::Aligned {
          center: true,
          blocks: std::mem::take(&mut centered_div_blocks),
        });
      }
      continue;
    }

    let node_blocks = blocks_from_node(node);
    if centered_div_depth > 0 {
      centered_div_blocks.extend(node_blocks);
    } else {
      blocks.extend(node_blocks);
    }
  }

  if !centered_div_blocks.is_empty() {
    blocks.push(Block::Aligned {
      center: true,
      blocks: centered_div_blocks,
    });
  }

  blocks
}

/// Configure comrak with GFM extensions.
fn comrak_options<'a>() -> Options<'a> {
  let mut options = Options::default();
  options.extension.strikethrough = true;
  options.extension.table = true;
  options.extension.tasklist = true;
  options.extension.autolink = true;
  options.extension.tagfilter = true;
  options.parse.smart = true;
  options
}

/// Convert a single comrak AST node into our block IR.
fn blocks_from_node<'a>(node: &'a AstNode<'a>) -> Vec<Block> {
  match &node.data.borrow().value {
    NodeValue::Paragraph => {
      let inlines = inlines_from_children(node);
      vec![Block::Paragraph(inlines)]
    }
    NodeValue::Heading(heading) => {
      let inlines = inlines_from_children(node);
      vec![Block::Heading {
        level: heading.level,
        content: inlines,
      }]
    }
    NodeValue::List(list) => {
      let ordered = matches!(list.list_type, ListType::Ordered);
      let start = if ordered {
        Some(list.start as u64)
      } else {
        None
      };
      let items: Vec<ListItem> = node.children().filter_map(list_item_from_node).collect();
      vec![Block::List(List {
        ordered,
        start,
        items,
      })]
    }
    NodeValue::CodeBlock(code) => {
      let lang = code
        .info
        .split_whitespace()
        .next()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
      vec![Block::CodeBlock(CodeBlock {
        lang,
        value: code.literal.clone(),
      })]
    }
    NodeValue::BlockQuote => {
      let children: Vec<Block> = node.children().flat_map(blocks_from_node).collect();
      vec![Block::BlockQuote(children)]
    }
    NodeValue::ThematicBreak => vec![Block::ThematicBreak],
    NodeValue::Table(_) => vec![Block::Table(table_from_node(node))],
    NodeValue::Item(_) => {
      // A list item that somehow appears outside a list — flatten.
      node.children().flat_map(blocks_from_node).collect()
    }
    NodeValue::HtmlBlock(html_block) => {
      let literal = &html_block.literal;
      if html::is_html_comment_only(literal) || html::is_details_close_only(literal) {
        Vec::new()
      } else {
        html_block_to_blocks(literal)
      }
    }
    NodeValue::Text(text) => {
      if text.is_empty() {
        Vec::new()
      } else {
        vec![Block::Paragraph(vec![Inline::Text(text.to_string())])]
      }
    }
    _ => {
      let text = collect_text(node);
      if text.is_empty() {
        Vec::new()
      } else {
        vec![Block::Paragraph(vec![Inline::Text(text)])]
      }
    }
  }
}

/// Convert an HTML block into our block IR.
///
/// Handles special cases:
/// - `<h1>`–`<h6>` → `Block::Heading` (with optional center alignment)
/// - `<p align="center">` → `Block::Aligned` wrapping a paragraph
/// - Everything else → `Block::Paragraph` with parsed inlines
fn html_block_to_blocks(literal: &str) -> Vec<Block> {
  let trimmed = literal.trim();

  // Check for HTML headings: <h1>...<h6>
  if let Some((level, inner)) = html::parse_html_heading(trimmed) {
    let centered = html::is_centered_paragraph(trimmed);
    let content = if inner.is_empty() {
      Vec::new()
    } else {
      vec![Inline::Text(html::decode_html_entities(&inner))]
    };
    let heading = Block::Heading { level, content };
    if centered {
      return vec![Block::Aligned {
        center: true,
        blocks: vec![heading],
      }];
    }
    return vec![heading];
  }

  // Check for centered paragraphs: <p align="center">
  if html::is_centered_paragraph(trimmed) {
    let inlines = html::parse_html_to_inlines(trimmed);
    if !inlines.is_empty() {
      return vec![Block::Aligned {
        center: true,
        blocks: vec![Block::Paragraph(inlines)],
      }];
    }
    return Vec::new();
  }

  // Default: parse to inlines and wrap in paragraph
  let inlines = html::parse_html_to_inlines(literal);
  if inlines.is_empty() {
    Vec::new()
  } else {
    vec![Block::Paragraph(inlines)]
  }
}

/// Convert comrak child nodes into inline IR.
fn inlines_from_children<'a>(node: &'a AstNode<'a>) -> Vec<Inline> {
  let inlines = inlines_from_nodes(node.children());
  merge_adjacent_text(inlines)
}

/// Convert an iterator of comrak AST nodes into inline IR.
fn inlines_from_nodes<'a>(nodes: impl Iterator<Item = &'a AstNode<'a>>) -> Vec<Inline> {
  let mut inlines = Vec::new();
  for node in nodes {
    match &node.data.borrow().value {
      NodeValue::Text(text) => {
        inlines.push(Inline::Text(text.to_string()));
      }
      NodeValue::Code(code) => {
        inlines.push(Inline::Code(code.literal.clone()));
      }
      NodeValue::LineBreak => {
        inlines.push(Inline::HardBreak);
      }
      NodeValue::SoftBreak => {
        inlines.push(Inline::SoftBreak);
      }
      NodeValue::Strong => {
        inlines.push(Inline::Strong(inlines_from_nodes(node.children())));
      }
      NodeValue::Emph => {
        inlines.push(Inline::Emphasis(inlines_from_nodes(node.children())));
      }
      NodeValue::Strikethrough => {
        inlines.push(Inline::Strikethrough(inlines_from_nodes(node.children())));
      }
      NodeValue::Link(link) => {
        let content = inlines_from_nodes(node.children());
        inlines.push(Inline::Link {
          url: link.url.clone(),
          title: if link.title.is_empty() {
            None
          } else {
            Some(link.title.clone())
          },
          content,
        });
      }
      NodeValue::Image(image) => {
        let alt = inline_to_plain_text(&inlines_from_nodes(node.children()));
        inlines.push(Inline::Image {
          url: image.url.clone(),
          title: if image.title.is_empty() {
            None
          } else {
            Some(image.title.clone())
          },
          alt,
          width: None,
          height: None,
          dark_url: None,
          light_url: None,
        });
      }
      NodeValue::HtmlInline(html_str) => {
        let parsed = html::parse_html_to_inlines(html_str);
        if !parsed.is_empty() {
          inlines.extend(parsed);
        }
      }
      NodeValue::TaskItem(_) => {
        // Handled by the parent list item
      }
      _ => {
        let text = collect_text(node);
        if !text.is_empty() {
          inlines.push(Inline::Text(text));
        }
      }
    }
  }
  inlines
}

/// Extract a list item from a comrak AST node.
///
/// In comrak 0.50, task list items replace `Item` — the node value becomes
/// `TaskItem` instead of `Item`. Both must be handled.
fn list_item_from_node<'a>(node: &'a AstNode<'a>) -> Option<ListItem> {
  let value = node.data.borrow().value.clone();
  match &value {
    NodeValue::Item(_) => {
      let blocks: Vec<Block> = node.children().flat_map(blocks_from_node).collect();
      Some(ListItem {
        blocks,
        checked: None,
      })
    }
    NodeValue::TaskItem(task) => {
      let checked = Some(task.symbol.is_some());
      let blocks: Vec<Block> = node.children().flat_map(blocks_from_node).collect();
      Some(ListItem { blocks, checked })
    }
    _ => None,
  }
}

/// Convert a comrak table node into our Table IR.
fn table_from_node<'a>(node: &'a AstNode<'a>) -> Table {
  let mut headers = Vec::new();
  let mut rows = Vec::new();

  for child in node.children() {
    let is_header = matches!(child.data.borrow().value, NodeValue::TableRow(true));
    let cells: Vec<Vec<Inline>> = child
      .children()
      .map(|cell| inlines_from_nodes(cell.children()))
      .collect();

    if is_header {
      headers = cells;
    } else {
      rows.push(cells);
    }
  }

  Table { headers, rows }
}

/// Recursively collect plain text from a comrak node.
fn collect_text<'a>(node: &'a AstNode<'a>) -> String {
  match &node.data.borrow().value {
    NodeValue::Text(text) => text.to_string(),
    NodeValue::Code(code) => code.literal.clone(),
    NodeValue::Paragraph | NodeValue::Heading(_) => {
      inline_to_plain_text(&inlines_from_nodes(node.children()))
    }
    NodeValue::Link(link) => link.url.clone(),
    _ => String::new(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn comrak_options_enables_gfm() {
    let opts = comrak_options();
    assert!(opts.extension.strikethrough);
    assert!(opts.extension.table);
    assert!(opts.extension.tasklist);
    assert!(opts.extension.autolink);
    assert!(opts.extension.tagfilter);
    assert!(opts.parse.smart);
  }

  #[test]
  fn parse_simple_paragraph() {
    let blocks = parse_comrak("Hello world");
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
      Block::Paragraph(inlines) => {
        assert_eq!(inline_to_plain_text(inlines), "Hello world");
      }
      _ => panic!("expected paragraph"),
    }
  }

  #[test]
  fn parse_table_headers_and_rows() {
    let md = "| H1 | H2 |\n|---|---|\n| a | b |\n| c | d |";
    let blocks = parse_comrak(md);
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
      Block::Table(table) => {
        assert_eq!(table.headers.len(), 2);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(inline_to_plain_text(&table.headers[0]), "H1");
      }
      _ => panic!("expected table"),
    }
  }

  #[test]
  fn parse_centered_div() {
    let md = "<div align=\"center\">\n\n**centered bold**\n\n</div>";
    let blocks = parse_comrak(md);
    assert!(
      blocks
        .iter()
        .any(|b| matches!(b, Block::Aligned { center: true, .. })),
      "expected centered aligned block, got: {blocks:?}"
    );
  }

  #[test]
  fn html_comment_filtered() {
    let blocks = parse_comrak("<!-- this is a comment -->");
    assert!(blocks.is_empty(), "comment should produce no blocks");
  }

  #[test]
  fn html_heading_h1() {
    let blocks = parse_comrak("<h1>Big Title</h1>");
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
      Block::Heading { level, content } => {
        assert_eq!(*level, 1);
        assert_eq!(inline_to_plain_text(content), "Big Title");
      }
      _ => panic!("expected Heading, got: {:?}", blocks[0]),
    }
  }

  #[test]
  fn html_heading_centered() {
    let blocks = parse_comrak(r#"<h2 align="center">Centered</h2>"#);
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
      Block::Aligned {
        center: true,
        blocks: inner,
      } => {
        assert_eq!(inner.len(), 1);
        match &inner[0] {
          Block::Heading { level, content } => {
            assert_eq!(*level, 2);
            assert_eq!(inline_to_plain_text(content), "Centered");
          }
          _ => panic!("expected Heading inside Aligned"),
        }
      }
      _ => panic!("expected Aligned, got: {:?}", blocks[0]),
    }
  }

  #[test]
  fn html_centered_paragraph() {
    let blocks = parse_comrak(r#"<p align="center">Centered text</p>"#);
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
      Block::Aligned {
        center: true,
        blocks: inner,
      } => {
        assert_eq!(inner.len(), 1);
        matches!(&inner[0], Block::Paragraph(_));
      }
      _ => panic!("expected Aligned, got: {:?}", blocks[0]),
    }
  }

  #[test]
  fn html_img_block() {
    let blocks = parse_comrak(r#"<img src="logo.png" alt="Logo" width="100">"#);
    assert!(!blocks.is_empty());
    match &blocks[0] {
      Block::Paragraph(inlines) => {
        assert!(inlines.iter().any(|i| matches!(i, Inline::Image { .. })));
      }
      _ => panic!("expected Paragraph with Image"),
    }
  }
}
