//! Code block rendering.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use gpui::{
  AnyElement, App, Bounds, ClipboardItem, Element, ElementId, Font, GlobalElementId, Hitbox,
  HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, MouseButton, Pixels,
  SharedString, StyledText, TextRun, Timer, Window, div, fill, point, prelude::*, px, rgb,
};
use syntect::easy::HighlightLines;
use syntect::highlighting::{
  Color, ScopeSelectors, Style, StyleModifier, Theme, ThemeItem, ThemeSet, ThemeSettings,
};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

use crate::types::CodeBlock;

use super::{CodeSyntaxTheme, MarkdownRenderOptions};
use super::interactive_scrollbar::{
  InteractiveScrollbarAxis, render_interactive_scrollbar, stop_horizontal_scroll_propagation,
};
use super::selectable_text::SelectableText;

const CODE_BLOCK_PADDING_X_PX: f32 = 12.0;
const CODE_BLOCK_PADDING_TOP_PX: f32 = 8.0;
const CODE_BLOCK_PADDING_BOTTOM_PX: f32 = 0.0;
/// Approximate Menlo glyph width at the preview's small text size.
const CODE_BLOCK_CELL_WIDTH_PX: f32 = 8.4;
const COPY_FEEDBACK_DURATION_SECS: u64 = 2;

static COPY_FEEDBACK: LazyLock<Mutex<HashMap<usize, usize>>> =
  LazyLock::new(|| Mutex::new(HashMap::new()));
static COPY_FEEDBACK_GENERATION: AtomicUsize = AtomicUsize::new(1);
static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(load_theme_set);

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
  let code_content_width = code_block_content_width(&display_value);
  let text: SharedString = display_value.clone().into();

  // Language label
  let lang_label = code.lang.as_deref().unwrap_or("");

  // Reuse the same identity already used by the code block's GPUI elements.
  // Parsed Markdown is cached, so this remains stable across the refreshes
  // used to show and clear the lightweight copy feedback.
  let code_block_id = code as *const CodeBlock as usize;

  // Outer container with group for hover-reveal of copy button
  let container_id: SharedString = format!("md-code-container-{code_block_id:x}").into();

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
  let copy_btn_id: SharedString = format!("md-copy-{code_block_id:x}").into();
  let clipboard_value = display_value.clone();
  let hover_bg = theme.border;
  let copied = is_code_block_copied(code_block_id);

  let copy_button = div()
    .id(copy_btn_id)
    .absolute()
    .top_2()
    .right_2()
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
    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
      cx.write_to_clipboard(ClipboardItem::new_string(clipboard_value.clone()));

      let generation = mark_code_block_copied(code_block_id);
      window.refresh();

      window
        .spawn(cx, async move |cx| {
          Timer::after(Duration::from_secs(COPY_FEEDBACK_DURATION_SECS)).await;
          let _ = cx.update(|window, _cx| {
            if clear_code_block_copied(code_block_id, generation) {
              window.refresh();
            }
          });
        })
        .detach();
    })
    .child(if copied { "Copied" } else { "Copy" });

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

  // Code content scrolls horizontally for long lines, but always participates
  // in the page's vertical scrolling rather than creating a nested Y scroller.
  let code_id: SharedString = format!("md-code-{code_block_id:x}").into();
  let scroll_state = options.horizontal_scroll_state(code_block_id);
  let code_font = Font {
    family: theme.code_font_family.clone(),
    features: Default::default(),
    fallbacks: None,
    weight: Default::default(),
    style: Default::default(),
  };

  let mut code_area = div()
    .id(code_id)
    .w_full()
    .min_w_0()
    .px(px(CODE_BLOCK_PADDING_X_PX))
    .pt(px(CODE_BLOCK_PADDING_TOP_PX))
    .pb(px(CODE_BLOCK_PADDING_BOTTOM_PX))
    .text_sm()
    .text_color(theme.foreground)
    .font(code_font.clone())
    .whitespace_nowrap()
    .overflow_x_scroll()
    .scrollbar_width(px(10.0))
    .track_scroll(&scroll_state.handle)
    .on_scroll_wheel(|event, _, cx| stop_horizontal_scroll_propagation(event, cx));
  code_area.style().restrict_scroll_to_axis = Some(true);

  // Keep the indentation-dot renderer when explicitly enabled. The normal Aster
  // preview path uses SelectableText so code supports click-drag selection and
  // copies the selected range on mouse-up just like paragraph text.
  if options.show_indentation_dots {
    let dot_color = theme.muted_foreground.opacity(INDENT_DOT_OPACITY);
    code_area = code_area.child(
      div()
        .min_w(px(code_content_width))
        .flex_none()
        .child(CodeBlockText::new(text, dot_color)),
    );
  } else {
    let text_id = options.selection_state.next_text_id();
    let runs = code_block_text_runs(
      code.lang.as_deref(),
      &display_value,
      code_font,
      theme.foreground,
      theme.code_syntax_theme,
    );
    code_area = code_area.child(
      div()
        .min_w(px(code_content_width))
        .flex_none()
        .child(
          SelectableText::new(
            text,
            runs,
            Vec::new(),
            options.selection_state.clone(),
            options.focus_handle.clone(),
            options.search_query.clone(),
            options.search_highlight_color,
            None,
            text_id,
          )
          .with_intrinsic_width(),
        ),
    );
  }

  // Wrap code area + copy button in a relative container so the button
  // is positioned relative to the code area (below the header).
  let code_wrapper = div()
    .relative()
    .w_full()
    .min_w_0()
    .child(code_area)
    .child(render_interactive_scrollbar(
      InteractiveScrollbarAxis::Horizontal,
      scroll_state.scrollbar,
      scroll_state.handle,
      theme.muted_foreground,
    ))
    .child(copy_button);

  container.child(code_wrapper).into_any_element()
}

fn is_code_block_copied(code_block_id: usize) -> bool {
  COPY_FEEDBACK.lock().unwrap().contains_key(&code_block_id)
}

fn mark_code_block_copied(code_block_id: usize) -> usize {
  let generation = COPY_FEEDBACK_GENERATION.fetch_add(1, Ordering::Relaxed);
  COPY_FEEDBACK
    .lock()
    .unwrap()
    .insert(code_block_id, generation);
  generation
}

fn clear_code_block_copied(code_block_id: usize, generation: usize) -> bool {
  let mut copied = COPY_FEEDBACK.lock().unwrap();
  if copied.get(&code_block_id).copied() != Some(generation) {
    return false;
  }
  copied.remove(&code_block_id);
  true
}

/// Builds colored text runs for a fenced code block, falling back to one run
/// when its language is missing, unknown, or cannot be highlighted.
fn code_block_text_runs(
  language: Option<&str>,
  text: &str,
  font: Font,
  fallback_color: Hsla,
  syntax_theme: CodeSyntaxTheme,
) -> Vec<TextRun> {
  let Some(language) = language.filter(|language| !language.trim().is_empty()) else {
    return plain_code_text_run(text, font, fallback_color);
  };
  let Some(syntax) = code_block_syntax(language) else {
    return plain_code_text_run(text, font, fallback_color);
  };
  let mut highlighter = HighlightLines::new(syntax, code_block_theme(syntax_theme));
  let mut runs = Vec::new();
  for line in LinesWithEndings::from(text) {
    let Ok(regions) = highlighter.highlight_line(line, &SYNTAX_SET) else {
      return plain_code_text_run(text, font, fallback_color);
    };
    for (style, region) in regions {
      if !region.is_empty() {
        runs.push(text_run_from_style(region.len(), font.clone(), style));
      }
    }
  }
  if runs.iter().map(|run| run.len).sum::<usize>() == text.len() {
    runs
  } else {
    plain_code_text_run(text, font, fallback_color)
  }
}

/// Resolves a Markdown fenced-code language identifier to a bundled syntax.
fn code_block_syntax(language: &str) -> Option<&'static SyntaxReference> {
  SYNTAX_SET
    .find_syntax_by_token(language.trim())
    .or_else(|| SYNTAX_SET.find_syntax_by_extension(language.trim()))
}

/// Resolves the selected Aster theme to its bundled Ayu syntax palette.
fn code_block_theme(syntax_theme: CodeSyntaxTheme) -> &'static Theme {
  let theme_name = match syntax_theme {
    CodeSyntaxTheme::AyuLight => "Ayu Light",
    CodeSyntaxTheme::AyuDark => "Ayu Dark",
    CodeSyntaxTheme::AyuMirage => "Ayu Mirage",
  };
  THEME_SET
    .themes
    .get(theme_name)
    .expect("Syntect default theme must be available")
}

/// Loads Syntect defaults together with the three Ayu palettes shipped by Aster.
fn load_theme_set() -> ThemeSet {
  let mut themes = ThemeSet::load_defaults();
  themes.themes.insert(
    "Ayu Light".into(),
    ayu_theme("Ayu Light", 0x5c6166, 0x787b80, 0x86b300, 0xffaa33, 0xfa8d3e, 0xf2ae49, 0x22a4e6, 0xa37acc, 0xf07171),
  );
  themes.themes.insert(
    "Ayu Dark".into(),
    ayu_theme("Ayu Dark", 0xbfbdb6, 0x626a73, 0xc2d94c, 0xe6b450, 0xff8f40, 0xffb454, 0x59c2ff, 0xd2a6ff, 0xf07178),
  );
  themes.themes.insert(
    "Ayu Mirage".into(),
    ayu_theme("Ayu Mirage", 0xcbccc6, 0x5c6773, 0xbae67e, 0xffcc66, 0xffad66, 0xffd173, 0x73d0ff, 0xd4bfff, 0xf07178),
  );
  themes
}

/// Builds the common TextMate scope rules used by an Ayu syntax palette.
fn ayu_theme(
  name: &str,
  foreground: u32,
  comment: u32,
  string: u32,
  constant: u32,
  keyword: u32,
  function: u32,
  type_name: u32,
  parameter: u32,
  variable: u32,
) -> Theme {
  Theme {
    name: Some(name.into()),
    author: Some("Ayu Theme".into()),
    settings: ThemeSettings {
      foreground: Some(syntect_color(foreground)),
      ..Default::default()
    },
    scopes: vec![
      ayu_scope("comment", comment),
      ayu_scope("string", string),
      ayu_scope("constant.numeric, constant.language", constant),
      ayu_scope("keyword, storage", keyword),
      ayu_scope("entity.name.function, variable.function, support.function", function),
      ayu_scope("entity.name.type, support.type, support.class", type_name),
      ayu_scope("variable.parameter, meta.parameter", parameter),
      ayu_scope("variable.member", variable),
      ayu_scope("entity.name.tag", type_name),
      ayu_scope("entity.other.attribute-name", function),
    ],
  }
}

/// Creates one Ayu syntax rule for a comma-separated TextMate scope selector.
fn ayu_scope(selector: &str, color: u32) -> ThemeItem {
  ThemeItem {
    scope: ScopeSelectors::from_str(selector).expect("Ayu scope selector must be valid"),
    style: StyleModifier {
      foreground: Some(syntect_color(color)),
      ..Default::default()
    },
  }
}

/// Converts an RGB hexadecimal literal into Syntect's opaque color type.
fn syntect_color(rgb_value: u32) -> Color {
  Color {
    r: ((rgb_value >> 16) & 0xff) as u8,
    g: ((rgb_value >> 8) & 0xff) as u8,
    b: (rgb_value & 0xff) as u8,
    a: 0xff,
  }
}

/// Creates a plain, single-color code run for the compatibility fallback path.
fn plain_code_text_run(text: &str, font: Font, color: Hsla) -> Vec<TextRun> {
  vec![TextRun {
    len: text.len(),
    font,
    color,
    underline: None,
    strikethrough: None,
    background_color: None,
  }]
}

/// Converts one Syntect highlight region into a GPUI text run.
fn text_run_from_style(length: usize, font: Font, style: Style) -> TextRun {
  TextRun {
    len: length,
    font,
    color: rgb(
      (u32::from(style.foreground.r) << 16)
        | (u32::from(style.foreground.g) << 8)
        | u32::from(style.foreground.b),
    )
    .into(),
    underline: None,
    strikethrough: None,
    background_color: None,
  }
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

/// Estimates the minimum width required by the longest rendered code line.
fn code_block_content_width(text: &str) -> f32 {
  text
    .lines()
    .map(|line| {
      line.chars()
        .map(|ch| if ch.is_ascii() { 1usize } else { 2usize })
        .sum::<usize>() as f32
        * CODE_BLOCK_CELL_WIDTH_PX
    })
    .fold(0.0f32, f32::max)
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
  fn content_width_uses_the_longest_line() {
    assert_eq!(
      code_block_content_width("short\nabcdefghij"),
      10.0 * CODE_BLOCK_CELL_WIDTH_PX
    );
  }

  #[test]
  fn content_width_counts_wide_characters_as_two_cells() {
    assert_eq!(
      code_block_content_width("部署abc"),
      7.0 * CODE_BLOCK_CELL_WIDTH_PX
    );
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

  #[test]
  fn copy_feedback_ignores_stale_generation() {
    let code_block_id = usize::MAX - 1;
    let first = mark_code_block_copied(code_block_id);
    let second = mark_code_block_copied(code_block_id);

    assert!(is_code_block_copied(code_block_id));
    assert!(!clear_code_block_copied(code_block_id, first));
    assert!(is_code_block_copied(code_block_id));
    assert!(clear_code_block_copied(code_block_id, second));
    assert!(!is_code_block_copied(code_block_id));
  }

  #[test]
  fn rust_code_uses_multiple_syntax_colors() {
    let text = "fn main() {\n    println!(\"hello\");\n}";
    let runs = code_block_text_runs(
      Some("rust"),
      text,
      gpui::font("Menlo"),
      rgb(0xffffff).into(),
      CodeSyntaxTheme::AyuLight,
    );
    assert_eq!(runs.iter().map(|run| run.len).sum::<usize>(), text.len());
    assert!(runs.windows(2).any(|pair| pair[0].color != pair[1].color));
  }

  #[test]
  fn ayu_syntax_themes_resolve_to_their_own_palettes() {
    assert_eq!(
      code_block_theme(CodeSyntaxTheme::AyuLight).name.as_deref(),
      Some("Ayu Light")
    );
    assert_eq!(
      code_block_theme(CodeSyntaxTheme::AyuDark).name.as_deref(),
      Some("Ayu Dark")
    );
    assert_eq!(
      code_block_theme(CodeSyntaxTheme::AyuMirage).name.as_deref(),
      Some("Ayu Mirage")
    );
  }

  #[test]
  fn unknown_code_language_uses_plain_fallback() {
    let text = "not highlighted";
    let fallback_color = rgb(0x123456).into();
    let runs = code_block_text_runs(
      Some("not-a-language"),
      text,
      gpui::font("Menlo"),
      fallback_color,
      CodeSyntaxTheme::AyuLight,
    );
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].len, text.len());
    assert_eq!(runs[0].color, fallback_color);
  }

  #[test]
  fn syntax_runs_preserve_utf8_byte_lengths() {
    let text = "// 部署\nfn main() {}";
    let runs = code_block_text_runs(
      Some("rust"),
      text,
      gpui::font("Menlo"),
      rgb(0xffffff).into(),
      CodeSyntaxTheme::AyuMirage,
    );
    assert_eq!(runs.iter().map(|run| run.len).sum::<usize>(), text.len());
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
