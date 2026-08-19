//! `<details>` segment splitting.
//!
//! comrak doesn't parse `<details>/<summary>` natively — it treats them as
//! HTML blocks. We pre-split the source into segments so that `<details>`
//! blocks are extracted before comrak sees them, and their body content is
//! recursively parsed as markdown.

/// A segment of the source — either plain markdown or a `<details>` block.
pub enum Segment {
  Markdown(String),
  Details {
    summary: Option<String>,
    body: String,
    open: bool,
  },
}

/// Split source into segments, extracting top-level `<details>` blocks.
///
/// This is fence-aware: `<details>` inside fenced code blocks is not treated
/// as a real HTML tag.
pub fn split_details_segments(source: &str) -> Vec<Segment> {
  let mut segments = Vec::new();
  let mut buffer = String::new();
  let mut fence: Option<(char, usize)> = None;
  let mut lines = source.lines();
  let mut pending_line: Option<String> = None;

  while let Some(line) = pending_line
    .take()
    .or_else(|| lines.next().map(|l| l.to_string()))
  {
    update_fence_state(&line, &mut fence);

    if fence.is_none() {
      if let Some(start_idx) = find_details_start(&line) {
        let (prefix, rest) = line.split_at(start_idx);
        if !prefix.is_empty() {
          buffer.push_str(prefix);
          buffer.push('\n');
        }

        if !buffer.is_empty() {
          segments.push(Segment::Markdown(std::mem::take(&mut buffer)));
        }

        let open = has_open_attribute(rest);
        let mut details_lines = Vec::new();
        let mut details_fence: Option<(char, usize)> = None;
        let mut depth = 0isize;

        let (first_part, trailing, new_depth) = split_details_line(rest, depth);
        depth = new_depth;
        details_lines.push(first_part);

        if depth > 0 {
          while depth > 0 {
            let Some(next_line) = lines.next() else {
              break;
            };
            let next_line = next_line.to_string();
            update_fence_state(&next_line, &mut details_fence);
            if details_fence.is_some() {
              details_lines.push(next_line);
              continue;
            }

            let (part, trail, new_depth) = split_details_line(&next_line, depth);
            depth = new_depth;
            details_lines.push(part);
            if depth == 0 {
              if let Some(t) = trail {
                pending_line = Some(t);
              }
              break;
            }
          }
        } else if let Some(t) = trailing {
          pending_line = Some(t);
        }

        let details_source = details_lines.join("\n");
        if let Some((summary, body)) = parse_details_block(&details_source) {
          segments.push(Segment::Details {
            summary,
            body,
            open,
          });
        } else {
          segments.push(Segment::Markdown(details_source));
        }
        continue;
      }
    }

    buffer.push_str(&line);
    buffer.push('\n');
  }

  if !buffer.is_empty() {
    segments.push(Segment::Markdown(buffer));
  }

  segments
}

/// Track fenced code block state so we don't parse `<details>` inside fences.
fn update_fence_state(line: &str, fence: &mut Option<(char, usize)>) {
  let trimmed = line.trim_start();
  let mut chars = trimmed.chars();
  let Some(first) = chars.next() else {
    return;
  };
  if first != '`' && first != '~' {
    return;
  }
  let mut count = 1usize;
  for ch in chars {
    if ch == first {
      count += 1;
    } else {
      break;
    }
  }
  if count < 3 {
    return;
  }
  match fence {
    None => {
      *fence = Some((first, count));
    }
    Some((fence_char, fence_len)) if *fence_char == first && count >= *fence_len => {
      *fence = None;
    }
    _ => {}
  }
}

/// Find the byte offset of `<details` in a line (case-insensitive).
fn find_details_start(line: &str) -> Option<usize> {
  // Use eq_ignore_ascii_case instead of to_ascii_lowercase() to avoid allocation.
  let haystack = line.as_bytes();
  let needle = b"<details";
  if haystack.len() < needle.len() {
    return None;
  }
  for i in 0..=(haystack.len() - needle.len()) {
    if haystack[i..i + needle.len()].eq_ignore_ascii_case(needle) {
      return Some(i);
    }
  }
  None
}

/// Check if a `<details ...>` tag line contains `open`.
fn has_open_attribute(line: &str) -> bool {
  let end = line.find('>').unwrap_or(line.len());
  let tag = &line[..end];
  tag
    .split_whitespace()
    .any(|part| part.eq_ignore_ascii_case("open") || part.starts_with("open="))
}

/// Split a line tracking `<details>` / `</details>` depth.
/// Returns (part, optional_trailing, new_depth).
fn split_details_line(line: &str, mut depth: isize) -> (String, Option<String>, isize) {
  let bytes = line.as_bytes();
  let len = bytes.len();
  let mut idx = 0usize;
  let mut split_at: Option<usize> = None;

  while idx < len {
    let remaining = &line[idx..];
    let _next_open = remaining
      .bytes()
      .zip(b"<details".iter())
      .enumerate()
      .find(|_| {
        // Use case-insensitive find
        false
      });
    // Simpler approach: find both case-insensitively
    let next_open_pos = find_tag_ci(remaining, "<details").map(|p| idx + p);
    let next_close_pos = find_tag_ci(remaining, "</details").map(|p| idx + p);

    let (next_pos, is_open) = match (next_open_pos, next_close_pos) {
      (None, None) => break,
      (Some(pos), None) => (pos, true),
      (None, Some(pos)) => (pos, false),
      (Some(o), Some(c)) => {
        if o <= c {
          (o, true)
        } else {
          (c, false)
        }
      }
    };

    if is_open {
      depth += 1;
      idx = next_pos + "<details".len();
      continue;
    }

    depth -= 1;
    let close_end = match line[next_pos..].find('>') {
      Some(rel) => next_pos + rel + 1,
      None => next_pos + "</details".len(),
    };
    idx = close_end;
    if depth <= 0 {
      depth = 0;
      split_at = Some(close_end);
      break;
    }
  }

  if let Some(end) = split_at {
    let (part, trailing) = line.split_at(end);
    let trailing = if trailing.is_empty() {
      None
    } else {
      Some(trailing.to_string())
    };
    return (part.to_string(), trailing, depth);
  }

  (line.to_string(), None, depth)
}

/// Case-insensitive byte search for a tag prefix.
fn find_tag_ci(haystack: &str, needle: &str) -> Option<usize> {
  let h = haystack.as_bytes();
  let n = needle.as_bytes();
  if h.len() < n.len() {
    return None;
  }
  for i in 0..=(h.len() - n.len()) {
    if h[i..i + n.len()].eq_ignore_ascii_case(n) {
      return Some(i);
    }
  }
  None
}

/// Parse a `<details>` block into (summary, body).
pub fn parse_details_block(source: &str) -> Option<(Option<String>, String)> {
  let start = find_tag_ci(source, "<details")?;
  let open_tag_end = source[start..].find('>')? + start;
  let end = find_last_details_close_end(source).unwrap_or(source.len());
  if end <= open_tag_end {
    return None;
  }

  let inner = &source[open_tag_end + 1..end];
  let (summary, body) = extract_summary(inner);
  Some((summary, body))
}

/// Find the byte offset of the end of the last `</details>` tag.
fn find_last_details_close_end(source: &str) -> Option<usize> {
  let bytes = source.as_bytes();
  let needle = b"</details";
  let mut last_pos = None;

  for i in 0..bytes.len() {
    if i + needle.len() <= bytes.len() && bytes[i..i + needle.len()].eq_ignore_ascii_case(needle) {
      // Find the closing >
      let close_end = source[i..]
        .find('>')
        .map(|r| i + r + 1)
        .unwrap_or(i + needle.len());
      last_pos = Some(close_end);
    }
  }

  last_pos
}

/// Split inner details content into summary and body.
fn extract_summary(inner: &str) -> (Option<String>, String) {
  let trimmed = inner.trim();

  // Look for <summary>...</summary>
  let summary_start = find_tag_ci(trimmed, "<summary");
  let summary_end_tag = find_tag_ci(trimmed, "</summary");

  if let (Some(s_start), Some(s_end)) = (summary_start, summary_end_tag) {
    let tag_content_start = trimmed[s_start..].find('>').map(|r| s_start + r + 1);
    if let Some(content_start) = tag_content_start {
      let summary_text = trimmed[content_start..s_end].trim().to_string();
      let after_summary_end = trimmed[s_end..]
        .find('>')
        .map(|r| s_end + r + 1)
        .unwrap_or(s_end);
      let body = trimmed[after_summary_end..].trim().to_string();
      let summary = if summary_text.is_empty() {
        None
      } else {
        Some(strip_html_tags(&summary_text))
      };
      return (summary, body);
    }
  }

  (None, trimmed.to_string())
}

/// Strip HTML tags from a string, keeping only text content.
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn find_details_start_case_insensitive() {
    assert_eq!(find_details_start("<details>"), Some(0));
    assert_eq!(find_details_start("<DETAILS>"), Some(0));
    assert_eq!(find_details_start("  <Details open>"), Some(2));
    assert!(find_details_start("no tag here").is_none());
  }

  #[test]
  fn has_open_attribute_works() {
    assert!(has_open_attribute("<details open>"));
    assert!(has_open_attribute("<details OPEN>"));
    assert!(!has_open_attribute("<details>"));
  }

  #[test]
  fn split_simple_details() {
    let source = "<details>\n<summary>Title</summary>\n\nBody\n</details>";
    let segments = split_details_segments(source);
    assert_eq!(segments.len(), 1);
    assert!(matches!(&segments[0], Segment::Details { .. }));
  }

  #[test]
  fn details_inside_fence_not_split() {
    let source = "```\n<details>\n</details>\n```";
    let segments = split_details_segments(source);
    assert_eq!(segments.len(), 1);
    assert!(matches!(&segments[0], Segment::Markdown(_)));
  }

  #[test]
  fn strip_html_tags_basic() {
    assert_eq!(strip_html_tags("<b>hello</b>"), "hello");
    assert_eq!(strip_html_tags("<em><h4>Title</h4></em>"), "Title");
    assert_eq!(strip_html_tags("no tags"), "no tags");
  }

  #[test]
  fn parse_details_block_basic() {
    let src = "<details>\n<summary>Sum</summary>\n\nbody text\n</details>";
    let (summary, body) = parse_details_block(src).unwrap();
    assert_eq!(summary.as_deref(), Some("Sum"));
    assert!(body.contains("body text"));
  }
}
