//! Markdown parsing — converts source text into the block/inline IR.
//!
//! The pipeline:
//! 1. [`details::split_details_segments`] — pre-scan for `<details>` blocks that
//!    comrak doesn't handle natively.
//! 2. [`comrak_ast::parse_comrak`] — comrak AST → [`Block`]/[`Inline`] tree.
//! 3. [`html::parse_html_inlines`] — lightweight HTML tag parser for inline HTML
//!    that comrak passes through (images, `<br>`, `<picture>`, etc.).

mod comrak_ast;
pub mod details;
pub mod html;

use crate::types::{Block, Details, ParsedMarkdown};

use details::{Segment, split_details_segments};

/// Parse a GFM markdown source string into a list of blocks.
///
/// This is the top-level parsing entry point. It handles `<details>` blocks
/// that comrak doesn't parse natively by pre-splitting the source, then
/// delegates each segment to comrak.
pub fn parse_gfm(source: &str) -> Vec<Block> {
  let mut blocks = Vec::new();
  for segment in split_details_segments(source) {
    match segment {
      Segment::Markdown(markdown) => {
        blocks.extend(comrak_ast::parse_comrak(&markdown));
      }
      Segment::Details {
        summary,
        body,
        open,
      } => {
        let summary_inlines =
          html::summary_inlines_from_text(summary.as_deref().unwrap_or("Details"));
        let body_blocks = parse_gfm(&body);
        blocks.push(Block::Details(Details {
          summary: summary_inlines,
          blocks: body_blocks,
          open,
        }));
      }
    }
  }
  blocks
}

/// Parse markdown source into a [`ParsedMarkdown`] wrapper (Arc'd block list).
pub fn parse_markdown(source: &str) -> ParsedMarkdown {
  ParsedMarkdown::new(parse_gfm(source))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::types::Inline;

  #[test]
  fn parse_empty() {
    let blocks = parse_gfm("");
    assert!(blocks.is_empty());
  }

  #[test]
  fn parse_paragraph() {
    let blocks = parse_gfm("Hello world");
    assert_eq!(blocks.len(), 1);
    assert!(matches!(&blocks[0], Block::Paragraph(_)));
  }

  #[test]
  fn parse_heading() {
    let blocks = parse_gfm("# Title\n\n## Subtitle");
    assert_eq!(blocks.len(), 2);
    assert!(matches!(&blocks[0], Block::Heading { level: 1, .. }));
    assert!(matches!(&blocks[1], Block::Heading { level: 2, .. }));
  }

  #[test]
  fn parse_code_block() {
    let blocks = parse_gfm("```rust\nfn main() {}\n```");
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
      Block::CodeBlock(cb) => {
        assert_eq!(cb.lang.as_deref(), Some("rust"));
        assert_eq!(cb.value, "fn main() {}\n");
      }
      other => panic!("expected CodeBlock, got {other:?}"),
    }
  }

  #[test]
  fn parse_unordered_list() {
    let blocks = parse_gfm("- a\n- b\n- c");
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
      Block::List(list) => {
        assert!(!list.ordered);
        assert_eq!(list.items.len(), 3);
      }
      other => panic!("expected List, got {other:?}"),
    }
  }

  #[test]
  fn parse_ordered_list() {
    let blocks = parse_gfm("1. first\n2. second");
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
      Block::List(list) => {
        assert!(list.ordered);
        assert_eq!(list.start, Some(1));
        assert_eq!(list.items.len(), 2);
      }
      other => panic!("expected List, got {other:?}"),
    }
  }

  #[test]
  fn parse_task_list() {
    let blocks = parse_gfm("- [x] done\n- [ ] todo");
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
      Block::List(list) => {
        assert_eq!(list.items[0].checked, Some(true));
        assert_eq!(list.items[1].checked, Some(false));
      }
      other => panic!("expected List, got {other:?}"),
    }
  }

  #[test]
  fn parse_blockquote() {
    let blocks = parse_gfm("> quoted text");
    assert_eq!(blocks.len(), 1);
    assert!(matches!(&blocks[0], Block::BlockQuote(_)));
  }

  #[test]
  fn parse_thematic_break() {
    let blocks = parse_gfm("above\n\n---\n\nbelow");
    assert_eq!(blocks.len(), 3);
    assert!(matches!(&blocks[1], Block::ThematicBreak));
  }

  #[test]
  fn parse_table() {
    let md = "| A | B |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |";
    let blocks = parse_gfm(md);
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
      Block::Table(table) => {
        assert_eq!(table.headers.len(), 2);
        assert_eq!(table.rows.len(), 2);
      }
      other => panic!("expected Table, got {other:?}"),
    }
  }

  #[test]
  fn parse_inline_formatting() {
    let blocks = parse_gfm("**bold** *italic* ~~strike~~ `code`");
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
      Block::Paragraph(inlines) => {
        let has_strong = inlines.iter().any(|i| matches!(i, Inline::Strong(_)));
        let has_emphasis = inlines.iter().any(|i| matches!(i, Inline::Emphasis(_)));
        let has_strike = inlines
          .iter()
          .any(|i| matches!(i, Inline::Strikethrough(_)));
        let has_code = inlines.iter().any(|i| matches!(i, Inline::Code(_)));
        assert!(has_strong, "missing Strong");
        assert!(has_emphasis, "missing Emphasis");
        assert!(has_strike, "missing Strikethrough");
        assert!(has_code, "missing Code");
      }
      other => panic!("expected Paragraph, got {other:?}"),
    }
  }

  #[test]
  fn parse_link() {
    let blocks = parse_gfm("[click](https://example.com)");
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
      Block::Paragraph(inlines) => {
        assert!(inlines.iter().any(|i| matches!(i, Inline::Link { .. })));
      }
      other => panic!("expected Paragraph, got {other:?}"),
    }
  }

  #[test]
  fn parse_image() {
    let blocks = parse_gfm("![alt text](https://example.com/img.png)");
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
      Block::Paragraph(inlines) => {
        assert!(inlines.iter().any(|i| matches!(i, Inline::Image { .. })));
      }
      other => panic!("expected Paragraph, got {other:?}"),
    }
  }

  #[test]
  fn parse_details_block() {
    let md = "<details>\n<summary>Click me</summary>\n\nHidden content\n</details>";
    let blocks = parse_gfm(md);
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
      Block::Details(details) => {
        assert!(!details.open);
        assert!(!details.blocks.is_empty());
      }
      other => panic!("expected Details, got {other:?}"),
    }
  }

  #[test]
  fn parse_details_open() {
    let md = "<details open>\n<summary>Open</summary>\n\nVisible\n</details>";
    let blocks = parse_gfm(md);
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
      Block::Details(details) => {
        assert!(details.open);
      }
      other => panic!("expected Details, got {other:?}"),
    }
  }

  #[test]
  fn parse_nested_details() {
    let md = "\
<details>
<summary>Outer</summary>

<details>
<summary>Inner</summary>

Inner body
</details>

Outer body
</details>";
    let blocks = parse_gfm(md);
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
      Block::Details(outer) => {
        assert!(outer.blocks.iter().any(|b| matches!(b, Block::Details(_))));
      }
      other => panic!("expected Details, got {other:?}"),
    }
  }

  #[test]
  fn parse_html_comment_ignored() {
    let md = "before\n\n<!-- comment -->\n\nafter";
    let blocks = parse_gfm(md);
    // Comment block should not appear as visible content
    let text: String = blocks
      .iter()
      .filter_map(|b| match b {
        Block::Paragraph(inlines) => Some(crate::types::inline_to_plain_text(inlines)),
        _ => None,
      })
      .collect::<Vec<_>>()
      .join(" ");
    assert!(text.contains("before"));
    assert!(text.contains("after"));
    assert!(!text.contains("comment"));
  }

  #[test]
  fn parsed_markdown_arc_sharing() {
    let p1 = parse_markdown("# Test");
    let p2 = p1.clone();
    // Both point to same allocation
    assert!(std::sync::Arc::ptr_eq(&p1.blocks, &p2.blocks));
  }

  #[test]
  fn parse_picture_element() {
    let md = r#"<picture>
  <source media="(prefers-color-scheme: dark)" srcset="dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="light.svg">
  <img src="default.svg" alt="Logo">
</picture>"#;
    let blocks = parse_gfm(md);
    assert!(!blocks.is_empty(), "picture should produce blocks");
    // Should find an Image inline with dark/light URLs
    let has_themed_image = blocks.iter().any(|b| match b {
      Block::Paragraph(inlines) => inlines.iter().any(|i| match i {
        Inline::Image {
          dark_url,
          light_url,
          ..
        } => dark_url.is_some() || light_url.is_some(),
        _ => false,
      }),
      _ => false,
    });
    assert!(
      has_themed_image,
      "expected Image with dark/light URLs, got: {blocks:?}"
    );
  }

  #[test]
  fn parse_html_heading_block() {
    let md = "<h1>HTML Heading</h1>\n\nSome text";
    let blocks = parse_gfm(md);
    let has_heading = blocks
      .iter()
      .any(|b| matches!(b, Block::Heading { level: 1, .. }));
    assert!(has_heading, "expected h1 heading, got: {blocks:?}");
  }

  #[test]
  fn parse_centered_html_heading() {
    let md = r#"<h2 align="center">Centered Title</h2>"#;
    let blocks = parse_gfm(md);
    let has_centered_heading = blocks.iter().any(|b| matches!(
      b,
      Block::Aligned { center: true, blocks } if blocks.iter().any(|b| matches!(b, Block::Heading { level: 2, .. }))
    ));
    assert!(
      has_centered_heading,
      "expected centered h2, got: {blocks:?}"
    );
  }

  #[test]
  fn parse_centered_paragraph() {
    let md = r#"<p align="center">Centered content</p>"#;
    let blocks = parse_gfm(md);
    let has_centered = blocks
      .iter()
      .any(|b| matches!(b, Block::Aligned { center: true, .. }));
    assert!(has_centered, "expected centered paragraph, got: {blocks:?}");
  }

  #[test]
  fn parse_linked_html_image() {
    // When <a><img></a> is alone on a line, comrak treats <a> as inline HTML
    // (not block-level), so the link wrapper is lost but the image is extracted.
    // The parse_link_wrapping_image function handles the case where the full
    // HTML block is available (e.g. inside a <div> or <p>).
    let md = r#"<a href="https://example.com"><img src="badge.svg" alt="Badge"></a>"#;
    let blocks = parse_gfm(md);
    let has_image = blocks.iter().any(|b| match b {
      Block::Paragraph(inlines) => inlines.iter().any(|i| matches!(i, Inline::Image { .. })),
      _ => false,
    });
    assert!(has_image, "expected Image, got: {blocks:?}");
  }

  #[test]
  fn parse_linked_image_in_centered_div() {
    // Inside an HTML block (like centered div), link wrapping image IS preserved
    let md = r#"<div align="center">

<a href="https://example.com"><img src="badge.svg" alt="Badge"></a>

</div>"#;
    let blocks = parse_gfm(md);
    // Should have an Aligned block containing the image
    let has_content = blocks
      .iter()
      .any(|b| matches!(b, Block::Aligned { center: true, .. }));
    assert!(
      has_content,
      "expected centered aligned block, got: {blocks:?}"
    );
  }
}
