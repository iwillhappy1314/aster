//! Code block rendering.

use gpui::{
  AnyElement, App, Bounds, ClipboardItem, Element, ElementId, Font, GlobalElementId, Hitbox,
  HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, MouseButton, Pixels,
  SharedString, StyledText, Window, div, fill, point, prelude::*, px,
};

use crate::types::CodeBlock;

use super::MarkdownRenderOptions;

/// Maximum height (px) before vertical scroll kicks in.
const CODE_BLOCK_MAX_HEIGHT_PX: f32 = 500.0;
const CODE_BLOCK_PADDING_X_PX: f32 = 12.0;
const CODE_BLOCK_PADDING_TOP_PX: f32 = 8.0;
const CODE_BLOCK_PADDING_BOTTOM_PX: f32 = 8.0;

// Indentation dots
const INDENT_DOT_SIZE_PX: f32 = 2.0;
pub(crate) const INDENT_DOT_OPACITY: f32 = 0.45;
const INDENT_DOT_MIN_SPACING_PX: f32 = 5.0;
const INDENT_DOT_MAX_RENDER_COUNT: usize = 600;
const INDENT_DOT_DISABLE_ABOVE_TEXT_LEN: usize = 20_000;

/// Render a code block.
pub fn render_code_block(
  code: &CodeBlock,
  options: &MarkdownRenderOptions,
  _cx: &App,
) -> AnyElement {
  let theme = options.theme();

  // Prepare display text: strip trailing newline
  let display_value = code_block_display_value(code);
  let text: SharedString = display_value.clone().into();

  // Language label
  let lang_label = code.lang.as_deref().unwrap_or("");

  // Outer container with group for hover-reveal of copy button
  let container_id: SharedString =
    format!("md-code-container-{:x}", code as *const CodeBlock as usize).into();

  let mut container = div()
    .id(container_id)
    .group("code-block")
    .w_full()
    .min_w_0()
    .rounded_md()
    .border_1()
    .border_color(theme.border)
    .bg(theme.code_background)
    .overflow_hidden();

  // Copy button — positioned inside the code area wrapper (below header)
  let copy_btn_id: SharedString = format!("md-copy-{:x}", code as *const CodeBlock as usize).into();
  let clipboard_value = display_value.clone();
  let hover_bg = theme.border;

  let copy_button = div()
    .id(copy_btn_id)
    .absolute()
    .top_1()
    .right_1()
    .px_2()
    .py(px(2.0))
    .rounded_md()
    .text_xs()
    .text_color(theme.muted_foreground)
    .bg(theme.code_background)
    .border_1()
    .border_color(theme.border)
    .cursor_pointer()
    .opacity(0.0)
    .group_hover("code-block", |s| s.opacity(1.0))
    .hover(move |s| s.bg(hover_bg))
    .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
      cx.write_to_clipboard(ClipboardItem::new_string(clipboard_value.clone()));
    })
    .child("Copy");

  // Language header if present
  if !lang_label.is_empty() {
    container = container.child(
      div()
        .px(px(CODE_BLOCK_PADDING_X_PX))
        .py_1()
        .text_xs()
        .text_color(theme.muted_foreground)
        .border_b_1()
        .border_color(theme.border)
        .child(lang_label.to_string()),
    );
  }

  // Code content — needs an id to support scrolling
  let code_id: SharedString = format!("md-code-{:x}", code as *const CodeBlock as usize).into();
  let code_font = Font {
    family: theme.code_font_family.clone(),
    ..Default::default()
  };

  let mut code_area = div()
    .id(code_id)
    .px(px(CODE_BLOCK_PADDING_X_PX))
    .pt(px(CODE_BLOCK_PADDING_TOP_PX))
    .pb(px(CODE_BLOCK_PADDING_BOTTOM_PX))
    .text_sm()
    .text_color(theme.foreground)
    .font(code_font)
    .whitespace_nowrap()
    .overflow_x_scroll();

  // Cap height and enable Y scroll (no-op for short blocks).
  // Stop scroll-wheel propagation so the parent container doesn't scroll
  // simultaneously (scroll chaining).
  if !options.expand_code_blocks {
    code_area = code_area
      .max_h(px(CODE_BLOCK_MAX_HEIGHT_PX))
      .overflow_y_scroll()
      .on_scroll_wheel(|_, _, cx| {
        cx.stop_propagation();
      });
  }

  // Build the code text child — use CodeBlockText when indentation dots are enabled,
  // otherwise plain SharedString for simplicity.
  if options.show_indentation_dots {
    let dot_color = theme.muted_foreground.opacity(INDENT_DOT_OPACITY);
    code_area = code_area.child(CodeBlockText::new(text, dot_color));
  } else {
    code_area = code_area.child(text);
  }

  // Wrap code area + copy button in a relative container so the button
  // is positioned relative to the code area (below the header).
  let code_wrapper = div().relative().child(code_area).child(copy_button);

  container.child(code_wrapper).into_any_element()
}

/// Prepare the display text for a code block.
fn code_block_display_value(code: &CodeBlock) -> String {
  let mut value = code.value.clone();
  // Strip single trailing newline (comrak always adds one)
  if value.ends_with('\n') {
    value.pop();
  }
  // Expand tabs to 4 spaces
  value = expand_tabs(&value);
  value
}

/// Expand tab characters to spaces (4-space tab stops).
fn expand_tabs(text: &str) -> String {
  let mut result = String::with_capacity(text.len());
  let mut col = 0usize;
  for ch in text.chars() {
    match ch {
      '\t' => {
        let spaces = 4 - (col % 4);
        for _ in 0..spaces {
          result.push(' ');
        }
        col += spaces;
      }
      '\n' => {
        result.push('\n');
        col = 0;
      }
      _ => {
        result.push(ch);
        col += 1;
      }
    }
  }
  result
}

// ---------------------------------------------------------------------------
// CodeBlockText — custom Element that renders text with indentation dots.
// ---------------------------------------------------------------------------

/// A text element that paints faint dots at leading-space positions in code.
pub(crate) struct CodeBlockText {
  text: SharedString,
  styled_text: StyledText,
  dot_indices: Vec<usize>,
  dot_color: Hsla,
}

impl CodeBlockText {
  pub(crate) fn new(text: SharedString, dot_color: Hsla) -> Self {
    let dot_indices = collect_indentation_dot_indices(text.as_ref());
    let styled_text = StyledText::new(text.clone());
    Self {
      text,
      styled_text,
      dot_indices,
      dot_color,
    }
  }
}

impl Element for CodeBlockText {
  type RequestLayoutState = ();
  type PrepaintState = Hitbox;

  fn id(&self) -> Option<ElementId> {
    None
  }

  fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
    None
  }

  fn request_layout(
    &mut self,
    _id: Option<&GlobalElementId>,
    inspector_id: Option<&InspectorElementId>,
    window: &mut Window,
    cx: &mut App,
  ) -> (LayoutId, ()) {
    let (layout_id, _) = self
      .styled_text
      .request_layout(None, inspector_id, window, cx);
    (layout_id, ())
  }

  fn prepaint(
    &mut self,
    _id: Option<&GlobalElementId>,
    inspector_id: Option<&InspectorElementId>,
    bounds: Bounds<Pixels>,
    _state: &mut (),
    window: &mut Window,
    cx: &mut App,
  ) -> Hitbox {
    self
      .styled_text
      .prepaint(None, inspector_id, bounds, &mut (), window, cx);
    window.insert_hitbox(bounds, HitboxBehavior::Normal)
  }

  fn paint(
    &mut self,
    _id: Option<&GlobalElementId>,
    inspector_id: Option<&InspectorElementId>,
    bounds: Bounds<Pixels>,
    _state: &mut (),
    _hitbox: &mut Hitbox,
    window: &mut Window,
    cx: &mut App,
  ) {
    let text_layout = self.styled_text.layout().clone();

    // Paint the text itself.
    self
      .styled_text
      .paint(None, inspector_id, bounds, &mut (), &mut (), window, cx);

    // Paint indentation dots.
    if self.dot_indices.is_empty() {
      return;
    }

    let text_len = self.text.len();
    let dot_size = px(INDENT_DOT_SIZE_PX);
    let dot_radius = dot_size / 2.;
    let line_height = text_layout.line_height();
    let min_spacing = px(INDENT_DOT_MIN_SPACING_PX);
    let mut last_drawn: Option<(usize, Pixels)> = None;

    for &ix in &self.dot_indices {
      if ix + 1 > text_len {
        continue;
      }
      let Some(start) = text_layout.position_for_index(ix) else {
        continue;
      };
      let Some(end) = text_layout.position_for_index(ix + 1) else {
        continue;
      };
      let cell_width = end.x - start.x;
      if cell_width <= px(0.) {
        continue;
      }

      let dot_center_x = start.x + cell_width / 2.;
      if let Some((last_ix, last_center_x)) = last_drawn {
        if ix == last_ix + 1 && dot_center_x - last_center_x < min_spacing {
          continue;
        }
      }

      let dot_x = dot_center_x - dot_size / 2.;
      let dot_y = start.y + (line_height - dot_size) / 2.;
      window.paint_quad(
        fill(
          Bounds::from_corners(
            point(dot_x, dot_y),
            point(dot_x + dot_size, dot_y + dot_size),
          ),
          self.dot_color,
        )
        .corner_radii(dot_radius),
      );
      last_drawn = Some((ix, dot_center_x));
    }
  }
}

impl IntoElement for CodeBlockText {
  type Element = Self;

  fn into_element(self) -> Self::Element {
    self
  }
}

// ---------------------------------------------------------------------------
// Indentation-dot index collection.
// ---------------------------------------------------------------------------

/// Collect byte indices of leading spaces in non-blank lines.
///
/// Tabs and blank lines (lines with only whitespace) are skipped.
/// Returns at most [`INDENT_DOT_MAX_RENDER_COUNT`] indices, evenly sampled.
fn collect_indentation_dot_indices(text: &str) -> Vec<usize> {
  if text.len() > INDENT_DOT_DISABLE_ABOVE_TEXT_LEN || !text.contains(' ') {
    return Vec::new();
  }

  let mut indices = Vec::new();
  let mut leading_spaces = Vec::new();
  let mut saw_non_whitespace = false;
  let mut in_leading_indent = true;

  for (ix, ch) in text.char_indices() {
    match ch {
      '\n' | '\r' => {
        if saw_non_whitespace {
          indices.extend_from_slice(&leading_spaces);
        }
        leading_spaces.clear();
        saw_non_whitespace = false;
        in_leading_indent = true;
      }
      ' ' if in_leading_indent => {
        leading_spaces.push(ix);
      }
      ' ' => {}
      '\t' if in_leading_indent => {
        in_leading_indent = false;
      }
      '\t' => {}
      _ => {
        saw_non_whitespace = true;
        in_leading_indent = false;
      }
    }
  }

  // Handle last line (no trailing newline).
  if saw_non_whitespace {
    indices.extend_from_slice(&leading_spaces);
  }

  limit_indentation_dot_indices(indices)
}

/// Cap the number of dot indices to avoid excessive rendering.
fn limit_indentation_dot_indices(indices: Vec<usize>) -> Vec<usize> {
  if indices.len() <= INDENT_DOT_MAX_RENDER_COUNT {
    return indices;
  }

  let step = indices.len().div_ceil(INDENT_DOT_MAX_RENDER_COUNT);
  indices.into_iter().step_by(step).collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn strips_trailing_newline() {
    let code = CodeBlock {
      lang: Some("rust".into()),
      value: "fn main() {}\n".into(),
    };
    assert_eq!(code_block_display_value(&code), "fn main() {}");
  }

  #[test]
  fn expands_tabs() {
    assert_eq!(expand_tabs("\tfoo"), "    foo");
    assert_eq!(expand_tabs("a\tb"), "a   b");
    assert_eq!(expand_tabs("ab\tc"), "ab  c");
    assert_eq!(expand_tabs("abc\td"), "abc d");
    assert_eq!(expand_tabs("abcd\te"), "abcd    e");
  }

  #[test]
  fn preserves_content_without_trailing_newline() {
    let code = CodeBlock {
      lang: None,
      value: "no newline".into(),
    };
    assert_eq!(code_block_display_value(&code), "no newline");
  }

  #[test]
  fn clipboard_content_matches_display() {
    // The clipboard should get the same content as what's displayed
    let code = CodeBlock {
      lang: Some("rust".into()),
      value: "fn main() {\n\tprintln!(\"hello\");\n}\n".into(),
    };
    let display = code_block_display_value(&code);
    // Trailing newline stripped, tabs expanded
    assert_eq!(display, "fn main() {\n    println!(\"hello\");\n}");
  }

  // ------ indentation dot tests ------

  #[test]
  fn indent_dots_empty_text() {
    assert!(collect_indentation_dot_indices("").is_empty());
  }

  #[test]
  fn indent_dots_no_spaces() {
    assert!(collect_indentation_dot_indices("abc\ndef").is_empty());
  }

  #[test]
  fn indent_dots_blank_lines_skipped() {
    // Lines with only spaces are blank → no dots
    let text = "   \n   \n";
    assert!(collect_indentation_dot_indices(text).is_empty());
  }

  #[test]
  fn indent_dots_simple_indent() {
    let text = "  hello";
    let indices = collect_indentation_dot_indices(text);
    assert_eq!(indices, vec![0, 1]);
  }

  #[test]
  fn indent_dots_multi_line() {
    let text = "fn main() {\n    println!();\n}";
    let indices = collect_indentation_dot_indices(text);
    // 4 leading spaces on line 2, starting at byte 13
    assert_eq!(indices, vec![12, 13, 14, 15]);
  }

  #[test]
  fn indent_dots_mixed_blank_and_content() {
    let text = "  x\n   \n  y";
    let indices = collect_indentation_dot_indices(text);
    // "  x" → indices 0,1 ; "   " blank → skip ; "  y" → indices 8,9
    assert_eq!(indices, vec![0, 1, 8, 9]);
  }

  #[test]
  fn indent_dots_disabled_for_large_text() {
    let big = " ".repeat(INDENT_DOT_DISABLE_ABOVE_TEXT_LEN + 1) + "x";
    assert!(collect_indentation_dot_indices(&big).is_empty());
  }

  #[test]
  fn indent_dots_limit_caps() {
    // Create text with many leading spaces
    let mut text = String::new();
    for _ in 0..200 {
      text.push_str("      code\n");
    }
    let indices = collect_indentation_dot_indices(&text);
    assert!(indices.len() <= INDENT_DOT_MAX_RENDER_COUNT);
  }

  #[test]
  fn limit_returns_all_when_under_max() {
    let indices = vec![0, 1, 2, 3, 4];
    assert_eq!(limit_indentation_dot_indices(indices.clone()), indices);
  }
}
