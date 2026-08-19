//! HTML parsing for inline HTML that comrak passes through.
//!
//! Uses the [`tl`] crate for proper DOM parsing instead of artisanal byte
//! scanning.  We handle the small set of tags that GitHub READMEs use:
//! `<img>`, `<br>`, `<picture>/<source>`, `<a>`, `<p>`, `<sub>`, `<sup>`,
//! `<h1>`-`<h6>`, centered `<div>`, and HTML entities.

use crate::types::Inline;

// -- Helpers ---------------------------------------------------------------

/// Get an attribute value from a `tl` tag.
fn attr(tag: &tl::HTMLTag, name: &str) -> Option<String> {
  tag
    .attributes()
    .get(name)
    .flatten()
    .map(|v| decode_html_entities(&v.as_utf8_str()))
}

/// Check if a tag has `align="center"` or `style` containing
/// `text-align: center`.
fn is_center_aligned(tag: &tl::HTMLTag) -> bool {
  if let Some(a) = attr(tag, "align") {
    if a.eq_ignore_ascii_case("center") {
      return true;
    }
  }
  if let Some(s) = attr(tag, "style") {
    let norm: String = s
      .chars()
      .filter(|c| !c.is_whitespace())
      .collect::<String>()
      .to_ascii_lowercase();
    if norm.contains("text-align:center") {
      return true;
    }
  }
  false
}

/// Parse an HTML string into a `tl` DOM.
fn parse_dom(html: &str) -> Option<tl::VDom<'_>> {
  tl::parse(html, tl::ParserOptions::default()).ok()
}

/// Get the tag name as a lowercase `String`.
fn tag_lower(tag: &tl::HTMLTag) -> String {
  tag.name().as_utf8_str().to_ascii_lowercase()
}

/// Build an `Inline::Image` from a `tl` `<img>` tag.
fn img_from_tag(tag: &tl::HTMLTag) -> Option<Inline> {
  let src = attr(tag, "src")?;
  Some(Inline::Image {
    url: src,
    title: None,
    alt: attr(tag, "alt").unwrap_or_default(),
    width: attr(tag, "width"),
    height: attr(tag, "height"),
    dark_url: None,
    light_url: None,
  })
}

/// Find all tags with a given name in a DOM.
fn find_tags<'a>(dom: &'a tl::VDom<'a>, name: &str) -> Vec<&'a tl::HTMLTag<'a>> {
  dom
    .nodes()
    .iter()
    .filter_map(|n| n.as_tag())
    .filter(|t| tag_lower(t) == name)
    .collect::<Vec<_>>()
}

/// Check if a tag name string is a heading (`h1`..`h6`).
fn heading_level(name: &str) -> Option<u8> {
  if name.len() == 2 && name.starts_with('h') {
    name[1..].parse::<u8>().ok().filter(|l| (1..=6).contains(l))
  } else {
    None
  }
}

// -- Public API (unchanged signatures) -------------------------------------

/// Decode common HTML entities.
pub fn decode_html_entities(text: &str) -> String {
  text
    .replace("&amp;", "&")
    .replace("&lt;", "<")
    .replace("&gt;", ">")
    .replace("&quot;", "\"")
    .replace("&#39;", "'")
    .replace("&apos;", "'")
    .replace("&nbsp;", "\u{00A0}")
}

/// Parse a `<summary>` text into inlines.  Strips HTML tags and returns
/// a single `Inline::Text`.
pub fn summary_inlines_from_text(text: &str) -> Vec<Inline> {
  let stripped = if let Some(dom) = parse_dom(text) {
    let parser = dom.parser();
    dom
      .children()
      .iter()
      .filter_map(|h| h.get(parser))
      .map(|n| n.inner_text(parser))
      .collect::<Vec<_>>()
      .join("")
  } else {
    strip_html_tags(text)
  };
  let trimmed = stripped.trim();
  if trimmed.is_empty() {
    vec![Inline::Text("Details".to_string())]
  } else {
    vec![Inline::Text(trimmed.to_string())]
  }
}

/// Check if an HTML block is only a comment (`<!-- ... -->`).
pub fn is_html_comment_only(html: &str) -> bool {
  let trimmed = html.trim();
  trimmed.starts_with("<!--") && trimmed.ends_with("-->") && !trimmed[4..].contains("<!--")
}

/// Check if an HTML block is only a `</details>` closing tag.
pub fn is_details_close_only(html: &str) -> bool {
  let trimmed = html.trim();
  let lower = trimmed.to_ascii_lowercase();
  let stripped: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
  stripped == "</details>" || stripped == "</details>\n"
}

/// Check if an HTML block opens a centered div.
pub fn is_centered_div_open(html: &str) -> bool {
  let Some(dom) = parse_dom(html) else {
    return false;
  };
  find_tags(&dom, "div")
    .first()
    .is_some_and(|t| is_center_aligned(t))
}

/// Check if an HTML block closes a `</div>`.
pub fn is_centered_div_close(html: &str) -> bool {
  let trimmed = html.trim();
  trimmed.eq_ignore_ascii_case("</div>")
}

/// Check if an HTML block is a heading tag (`<h1>` through `<h6>`).
/// Returns `Some((level, inner_text))` if it matches.
pub fn parse_html_heading(html: &str) -> Option<(u8, String)> {
  let dom = parse_dom(html)?;
  let parser = dom.parser();
  for node in dom.nodes().iter() {
    if let Some(tag) = node.as_tag() {
      let name = tag_lower(tag);
      if let Some(level) = heading_level(&name) {
        let text = tag.inner_text(parser).trim().to_string();
        return Some((level, text));
      }
    }
  }
  None
}

/// Check if an HTML block has a centered `<p>` or `<h1>`-`<h6>`.
pub fn is_centered_paragraph(html: &str) -> bool {
  let Some(dom) = parse_dom(html) else {
    return false;
  };
  for tag in dom.nodes().iter().filter_map(|n| n.as_tag()) {
    let name = tag_lower(tag);
    let is_relevant = name == "p" || heading_level(&name).is_some();
    if is_relevant && is_center_aligned(tag) {
      return true;
    }
  }
  false
}

/// Parse inline HTML fragments into our Inline IR.
///
/// Handles:
/// - `<img>` -> `Inline::Image`
/// - `<br>` -> `Inline::HardBreak`
/// - `<picture>` with `<source media="prefers-color-scheme">` -> themed image
/// - `<a>` wrapping `<img>` -> `Inline::Link` with image content
/// - `<sub>`, `<sup>` -> degraded to text
/// - Other tags -> stripped to text content
pub fn parse_html_to_inlines(html: &str) -> Vec<Inline> {
  let trimmed = html.trim();
  if trimmed.is_empty() {
    return Vec::new();
  }

  let Some(dom) = parse_dom(trimmed) else {
    return fallback_strip(trimmed);
  };

  // <picture> -- try first so the generic walker doesn't grab <img>
  // out of context
  if let Some(img) = parse_picture(&dom) {
    return vec![img];
  }

  // Walk top-level children and convert to inlines
  let inlines = walk_children(dom.children(), &dom);
  if !inlines.is_empty() {
    return inlines;
  }

  fallback_strip(trimmed)
}

// -- DOM walkers -----------------------------------------------------------

/// Walk a list of node handles and convert them into `Inline`s.
fn walk_children(handles: &[tl::NodeHandle], dom: &tl::VDom) -> Vec<Inline> {
  let parser = dom.parser();
  let mut inlines = Vec::new();

  for handle in handles {
    let Some(node) = handle.get(parser) else {
      continue;
    };

    match node {
      tl::Node::Raw(text) => {
        let t = decode_html_entities(&text.as_utf8_str()).trim().to_string();
        if !t.is_empty() {
          inlines.push(Inline::Text(t));
        }
      }
      tl::Node::Tag(tag) => {
        let name = tag_lower(tag);
        match name.as_str() {
          "img" => {
            if let Some(img) = img_from_tag(tag) {
              inlines.push(img);
            }
          }
          n if n == "br" || n == "br/" => {
            inlines.push(Inline::HardBreak);
          }
          "a" => {
            if let Some(href) = attr(tag, "href") {
              let inner = walk_children(tag.children().top().as_slice(), dom);
              if !inner.is_empty() {
                inlines.push(Inline::Link {
                  url: href,
                  title: None,
                  content: inner,
                });
              }
            } else {
              inlines.extend(walk_children(tag.children().top().as_slice(), dom));
            }
          }
          "sub" | "sup" => {
            let text = tag.inner_text(parser).trim().to_string();
            if !text.is_empty() {
              inlines.push(Inline::Text(decode_html_entities(&text)));
            }
          }
          "picture" => {
            if let Some(img) = parse_picture(dom) {
              inlines.push(img);
            }
          }
          // Container tags -- recurse
          "p" | "div" | "span" | "em" | "strong" | "b" | "i" | "u" | "code" => {
            inlines.extend(walk_children(tag.children().top().as_slice(), dom));
          }
          n if heading_level(n).is_some() => {
            let text = tag.inner_text(parser).trim().to_string();
            if !text.is_empty() {
              inlines.push(Inline::Text(decode_html_entities(&text)));
            }
          }
          // Unknown -- keep text children
          _ => {
            inlines.extend(walk_children(tag.children().top().as_slice(), dom));
          }
        }
      }
      tl::Node::Comment(_) => {}
    }
  }

  inlines
}

/// Parse a `<picture>` element with `<source>` tags for dark/light URLs.
fn parse_picture(dom: &tl::VDom) -> Option<Inline> {
  if find_tags(dom, "picture").is_empty() {
    return None;
  }

  let img_tag = find_tags(dom, "img").into_iter().next()?;
  let mut img = img_from_tag(img_tag)?;

  let mut dark: Option<String> = None;
  let mut light: Option<String> = None;

  for source in find_tags(dom, "source") {
    if let Some(media) = attr(source, "media") {
      let ml = media.to_ascii_lowercase();
      if let Some(srcset) = attr(source, "srcset") {
        let url = srcset
          .split(',')
          .next()
          .unwrap_or(&srcset)
          .trim()
          .split_whitespace()
          .next()
          .unwrap_or(&srcset);
        if ml.contains("dark") {
          dark = Some(url.to_string());
        } else if ml.contains("light") {
          light = Some(url.to_string());
        }
      }
    }
  }

  if let Inline::Image {
    dark_url: ref mut d,
    light_url: ref mut l,
    ..
  } = img
  {
    *d = dark;
    *l = light;
  }

  Some(img)
}

// -- Fallback --------------------------------------------------------------

/// Simple tag stripper used as last resort.
fn strip_html_tags(input: &str) -> String {
  let mut result = String::with_capacity(input.len());
  let mut in_tag = false;
  for ch in input.chars() {
    match ch {
      '<' => in_tag = true,
      '>' => in_tag = false,
      _ if !in_tag => result.push(ch),
      _ => {}
    }
  }
  result
}

/// Fallback: strip all tags, return text if non-empty.
fn fallback_strip(html: &str) -> Vec<Inline> {
  let text = strip_html_tags(html);
  let text = text.trim();
  if text.is_empty() {
    Vec::new()
  } else {
    vec![Inline::Text(decode_html_entities(text))]
  }
}

// -- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parse_img_tag() {
    let html = r#"<img src="https://example.com/img.png" alt="Logo" width="100">"#;
    let inlines = parse_html_to_inlines(html);
    assert_eq!(inlines.len(), 1);
    match &inlines[0] {
      Inline::Image {
        url,
        alt,
        width,
        height,
        ..
      } => {
        assert_eq!(url, "https://example.com/img.png");
        assert_eq!(alt, "Logo");
        assert_eq!(width.as_deref(), Some("100"));
        assert!(height.is_none());
      }
      _ => panic!("expected Image"),
    }
  }

  #[test]
  fn br_tag() {
    for input in &["<br>", "<br/>", "<BR />"] {
      let inlines = parse_html_to_inlines(input);
      assert_eq!(inlines.len(), 1, "failed for: {input}");
      assert!(
        matches!(inlines[0], Inline::HardBreak),
        "failed for: {input}"
      );
    }
  }

  #[test]
  fn is_comment() {
    assert!(is_html_comment_only("<!-- hello -->"));
    assert!(is_html_comment_only("<!--\nmultiline\n-->"));
    assert!(!is_html_comment_only("<p>not a comment</p>"));
  }

  #[test]
  fn centered_div_detection() {
    assert!(is_centered_div_open(r#"<div align="center">"#));
    assert!(is_centered_div_open(r#"<div style="text-align: center">"#));
    assert!(!is_centered_div_open(r#"<div class="foo">"#));
  }

  #[test]
  fn html_entity_decoding() {
    assert_eq!(decode_html_entities("&amp; &lt; &gt;"), "& < >");
    assert_eq!(decode_html_entities("&quot;hi&quot;"), r#""hi""#);
  }

  #[test]
  fn summary_inlines_strips_tags() {
    let inlines = summary_inlines_from_text("<em><h4>Cargo Audit</h4></em>");
    assert_eq!(inlines.len(), 1);
    match &inlines[0] {
      Inline::Text(t) => assert_eq!(t, "Cargo Audit"),
      _ => panic!("expected Text"),
    }
  }

  #[test]
  fn parse_picture_dark_light() {
    let html = r#"<picture>
  <source media="(prefers-color-scheme: dark)" srcset="dark.png">
  <source media="(prefers-color-scheme: light)" srcset="light.png">
  <img src="fallback.png" alt="Logo" width="200">
</picture>"#;
    let inlines = parse_html_to_inlines(html);
    assert_eq!(inlines.len(), 1);
    match &inlines[0] {
      Inline::Image {
        url,
        alt,
        width,
        dark_url,
        light_url,
        ..
      } => {
        assert_eq!(url, "fallback.png");
        assert_eq!(alt, "Logo");
        assert_eq!(width.as_deref(), Some("200"));
        assert_eq!(dark_url.as_deref(), Some("dark.png"));
        assert_eq!(light_url.as_deref(), Some("light.png"));
      }
      _ => panic!("expected Image"),
    }
  }

  #[test]
  fn parse_picture_single_source() {
    let html = r#"<picture>
  <source media="(prefers-color-scheme: dark)" srcset="dark.svg">
  <img src="default.svg" alt="Logo">
</picture>"#;
    let inlines = parse_html_to_inlines(html);
    assert_eq!(inlines.len(), 1);
    match &inlines[0] {
      Inline::Image { url, dark_url, .. } => {
        assert_eq!(url, "default.svg");
        assert_eq!(dark_url.as_deref(), Some("dark.svg"));
      }
      _ => panic!("expected Image, got: {:?}", inlines[0]),
    }
  }

  #[test]
  fn parse_link_wrapping_img() {
    let html = r#"<a href="https://example.com"><img src="badge.svg" alt="Badge"></a>"#;
    let inlines = parse_html_to_inlines(html);
    assert_eq!(inlines.len(), 1);
    match &inlines[0] {
      Inline::Link { url, content, .. } => {
        assert_eq!(url, "https://example.com");
        assert_eq!(content.len(), 1);
        assert!(
          matches!(&content[0], Inline::Image { url, alt, .. } if url == "badge.svg" && alt == "Badge")
        );
      }
      _ => panic!("expected Link, got: {:?}", inlines[0]),
    }
  }

  #[test]
  fn parse_sub_sup_in_block() {
    let html = "<p>H<sub>2</sub>O is water. E=mc<sup>2</sup></p>";
    let inlines = parse_html_to_inlines(html);
    let text: String = inlines
      .iter()
      .map(|i| match i {
        Inline::Text(t) => t.as_str(),
        _ => "",
      })
      .collect();
    assert!(text.contains('H'));
    assert!(text.contains('2'));
    assert!(text.contains('O'));
  }

  #[test]
  fn parse_html_heading_h1() {
    let result = parse_html_heading("<h1>Hello World</h1>");
    assert_eq!(result, Some((1, "Hello World".to_string())));
  }

  #[test]
  fn parse_html_heading_h3_centered() {
    let html = r#"<h3 align="center">Title</h3>"#;
    let result = parse_html_heading(html);
    assert_eq!(result, Some((3, "Title".to_string())));
    assert!(is_centered_paragraph(html));
  }

  #[test]
  fn parse_html_heading_with_tags() {
    let html = "<h2><em>Styled</em> heading</h2>";
    let result = parse_html_heading(html);
    assert_eq!(result, Some((2, "Styled heading".to_string())));
  }

  #[test]
  fn centered_paragraph_detection() {
    assert!(is_centered_paragraph(r#"<p align="center">text</p>"#));
    assert!(is_centered_paragraph(
      r#"<p style="text-align: center">text</p>"#
    ));
    assert!(!is_centered_paragraph(r#"<p>text</p>"#));
  }

  #[test]
  fn multi_tag_block_with_br() {
    let html = "Hello<br>World";
    let inlines = parse_html_to_inlines(html);
    assert!(inlines.len() >= 3); // Text, HardBreak, Text
    assert!(inlines.iter().any(|i| matches!(i, Inline::HardBreak)));
  }

  #[test]
  fn multiple_images_in_block() {
    let html = r#"<img src="a.png" alt="A"> <img src="b.png" alt="B">"#;
    let inlines = parse_html_to_inlines(html);
    let img_count = inlines
      .iter()
      .filter(|i| matches!(i, Inline::Image { .. }))
      .count();
    assert_eq!(img_count, 2);
  }

  #[test]
  fn link_wrapping_text() {
    let html = r#"<a href="https://example.com">Click here</a>"#;
    let inlines = parse_html_to_inlines(html);
    assert_eq!(inlines.len(), 1);
    match &inlines[0] {
      Inline::Link { url, content, .. } => {
        assert_eq!(url, "https://example.com");
        assert_eq!(content.len(), 1);
        assert!(matches!(&content[0], Inline::Text(t) if t == "Click here"));
      }
      _ => panic!("expected Link"),
    }
  }
}
