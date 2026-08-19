//! HTML renderer for the shared Markdown IR.
//!
//! This module deliberately depends only on [`crate::types`], so non-GPUI
//! consumers (for example the macOS Quick Look extension) can reuse the exact
//! same parser and Markdown semantics without linking GPUI.

use crate::types::{Block, Inline, List, ParsedMarkdown, Table};
use std::fmt::Write as _;

/// Render a parsed Markdown document into an HTML fragment.
pub fn render_html(parsed: &ParsedMarkdown) -> String {
  render_blocks(parsed.blocks())
}

/// Parse Markdown with the shared parser and render it into an HTML fragment.
pub fn render_markdown_html(source: &str) -> String {
  let parsed = crate::parse::parse_markdown(source);
  render_html(&parsed)
}

/// Render a complete standalone HTML document suitable for a `WKWebView`.
///
/// `extra_css` is embedded into the document rather than linked as a resource,
/// which keeps the Quick Look bridge self-contained and avoids resource URL
/// bookkeeping inside the extension sandbox.
pub fn render_markdown_html_document(source: &str, extra_css: &str) -> String {
  let body = render_markdown_html(source);
  let mut out = String::with_capacity(body.len() + extra_css.len() + 512);
  out.push_str("<!doctype html><html><head><meta charset=\"utf-8\">");
  out.push_str("<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">");
  out.push_str("<style>");
  out.push_str(extra_css);
  out.push_str("</style></head><body class=\"aster-markdown\">");
  out.push_str(&body);
  out.push_str("</body></html>");
  out
}

fn render_blocks(blocks: &[Block]) -> String {
  let mut out = String::new();
  for block in blocks {
    render_block(block, &mut out);
  }
  out
}

fn render_block(block: &Block, out: &mut String) {
  match block {
    Block::Paragraph(inlines) => {
      out.push_str("<p>");
      render_inlines(inlines, out);
      out.push_str("</p>");
    }
    Block::Heading { level, content } => {
      let level = (*level).clamp(1, 6);
      let _ = write!(out, "<h{level}>");
      render_inlines(content, out);
      let _ = write!(out, "</h{level}>");
    }
    Block::List(list) => render_list(list, out),
    Block::CodeBlock(code) => {
      out.push_str("<pre><code");
      if let Some(lang) = code.lang.as_deref().filter(|lang| !lang.trim().is_empty()) {
        out.push_str(" class=\"language-");
        escape_attr_into(lang.trim(), out);
        out.push('"');
      }
      out.push('>');
      escape_text_into(&code.value, out);
      out.push_str("</code></pre>");
    }
    Block::BlockQuote(blocks) => {
      out.push_str("<blockquote>");
      for block in blocks {
        render_block(block, out);
      }
      out.push_str("</blockquote>");
    }
    Block::ThematicBreak => out.push_str("<hr>"),
    Block::Table(table) => render_table(table, out),
    Block::Details(details) => {
      if details.open {
        out.push_str("<details open>");
      } else {
        out.push_str("<details>");
      }
      out.push_str("<summary>");
      render_inlines(&details.summary, out);
      out.push_str("</summary>");
      for block in &details.blocks {
        render_block(block, out);
      }
      out.push_str("</details>");
    }
    Block::Aligned { center, blocks } => {
      if *center {
        out.push_str("<div class=\"aster-align-center\">");
      } else {
        out.push_str("<div>");
      }
      for block in blocks {
        render_block(block, out);
      }
      out.push_str("</div>");
    }
  }
}

fn render_list(list: &List, out: &mut String) {
  if list.ordered {
    out.push_str("<ol");
    if let Some(start) = list.start.filter(|start| *start != 1) {
      let _ = write!(out, " start=\"{start}\"");
    }
    out.push('>');
  } else {
    out.push_str("<ul>");
  }

  for item in &list.items {
    if item.checked.is_some() {
      out.push_str("<li class=\"task-list-item\">");
      match item.checked {
        Some(true) => out.push_str("<input type=\"checkbox\" disabled checked>"),
        Some(false) => out.push_str("<input type=\"checkbox\" disabled>"),
        None => {}
      }
    } else {
      out.push_str("<li>");
    }

    if item.checked.is_some() {
      if let Some((first, rest)) = item.blocks.split_first() {
        if let Block::Paragraph(inlines) = first {
          render_inlines(inlines, out);
        } else {
          render_block(first, out);
        }
        for block in rest {
          render_block(block, out);
        }
      }
    } else {
      for block in &item.blocks {
        render_block(block, out);
      }
    }
    out.push_str("</li>");
  }

  if list.ordered {
    out.push_str("</ol>");
  } else {
    out.push_str("</ul>");
  }
}

fn render_table(table: &Table, out: &mut String) {
  out.push_str("<div class=\"table-scroll\"><table><thead><tr>");
  for header in &table.headers {
    out.push_str("<th>");
    render_inlines(header, out);
    out.push_str("</th>");
  }
  out.push_str("</tr></thead><tbody>");

  for row in &table.rows {
    out.push_str("<tr>");
    for cell in row {
      out.push_str("<td>");
      render_inlines(cell, out);
      out.push_str("</td>");
    }
    out.push_str("</tr>");
  }

  out.push_str("</tbody></table></div>");
}

fn render_inlines(inlines: &[Inline], out: &mut String) {
  for inline in inlines {
    match inline {
      Inline::Text(value) => escape_text_into(value, out),
      Inline::Link {
        url,
        title,
        content,
      } => {
        out.push_str("<a href=\"");
        escape_attr_into(url, out);
        out.push('"');
        if let Some(title) = title.as_deref().filter(|title| !title.is_empty()) {
          out.push_str(" title=\"");
          escape_attr_into(title, out);
          out.push('"');
        }
        out.push_str(" rel=\"noreferrer noopener\">");
        render_inlines(content, out);
        out.push_str("</a>");
      }
      Inline::Image {
        url,
        title,
        alt,
        width,
        height,
        dark_url,
        light_url,
      } => {
        let has_picture_sources = dark_url.is_some() || light_url.is_some();
        if has_picture_sources {
          out.push_str("<picture>");
          if let Some(dark_url) = dark_url {
            out.push_str("<source media=\"(prefers-color-scheme: dark)\" srcset=\"");
            escape_attr_into(dark_url, out);
            out.push_str("\">");
          }
          if let Some(light_url) = light_url {
            out.push_str("<source media=\"(prefers-color-scheme: light)\" srcset=\"");
            escape_attr_into(light_url, out);
            out.push_str("\">");
          }
        }

        out.push_str("<img src=\"");
        escape_attr_into(url, out);
        out.push_str("\" alt=\"");
        escape_attr_into(alt, out);
        out.push('"');
        if let Some(title) = title.as_deref().filter(|title| !title.is_empty()) {
          out.push_str(" title=\"");
          escape_attr_into(title, out);
          out.push('"');
        }
        if let Some(width) = width.as_deref().filter(|value| safe_dimension(value)) {
          out.push_str(" width=\"");
          escape_attr_into(width, out);
          out.push('"');
        }
        if let Some(height) = height.as_deref().filter(|value| safe_dimension(value)) {
          out.push_str(" height=\"");
          escape_attr_into(height, out);
          out.push('"');
        }
        out.push('>');

        if has_picture_sources {
          out.push_str("</picture>");
        }
      }
      Inline::Code(value) => {
        out.push_str("<code>");
        escape_text_into(value, out);
        out.push_str("</code>");
      }
      Inline::SoftBreak => out.push(' '),
      Inline::HardBreak => out.push_str("<br>"),
      Inline::Strong(children) => {
        out.push_str("<strong>");
        render_inlines(children, out);
        out.push_str("</strong>");
      }
      Inline::Emphasis(children) => {
        out.push_str("<em>");
        render_inlines(children, out);
        out.push_str("</em>");
      }
      Inline::Strikethrough(children) => {
        out.push_str("<del>");
        render_inlines(children, out);
        out.push_str("</del>");
      }
    }
  }
}

fn safe_dimension(value: &str) -> bool {
  !value.is_empty()
    && value
      .bytes()
      .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'%' | b'p' | b'x' | b'e' | b'm'))
}

fn escape_text_into(value: &str, out: &mut String) {
  for ch in value.chars() {
    match ch {
      '&' => out.push_str("&amp;"),
      '<' => out.push_str("&lt;"),
      '>' => out.push_str("&gt;"),
      _ => out.push(ch),
    }
  }
}

fn escape_attr_into(value: &str, out: &mut String) {
  for ch in value.chars() {
    match ch {
      '&' => out.push_str("&amp;"),
      '<' => out.push_str("&lt;"),
      '>' => out.push_str("&gt;"),
      '"' => out.push_str("&quot;"),
      '\'' => out.push_str("&#39;"),
      _ => out.push(ch),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn renders_shared_gfm_nodes() {
    let source = r#"# Title

- [x] done
- [ ] todo

| A | B |
|---|---|
| 1 | 2 |

```rust
fn main() {}
```
"#;
    let html = render_markdown_html(source);
    assert!(html.contains("<h1>Title</h1>"));
    assert!(html.contains("type=\"checkbox\" disabled checked"));
    assert!(html.contains("<table>"));
    assert!(html.contains("class=\"language-rust\""));
  }

  #[test]
  fn escapes_text_and_attributes() {
    let html = render_markdown_html(r#"[<unsafe>](https://example.com/?a=1&b=\"2\")"#);
    assert!(html.contains("&lt;unsafe&gt;"));
    assert!(html.contains("&amp;"));
  }

  #[test]
  fn renders_picture_variants() {
    let source = r#"<picture>
<source media="(prefers-color-scheme: dark)" srcset="dark.svg">
<source media="(prefers-color-scheme: light)" srcset="light.svg">
<img src="default.svg" alt="Logo">
</picture>"#;
    let html = render_markdown_html(source);
    assert!(html.contains("<picture>"));
    assert!(html.contains("dark.svg"));
    assert!(html.contains("light.svg"));
  }
}
