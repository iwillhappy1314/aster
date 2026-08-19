//! Markdown rendering — converts parsed blocks/inlines into GPUI elements.

pub mod blocks;
pub mod code_block;
pub mod code_preview;
pub mod image;
pub mod inline;
pub mod selectable_text;
pub mod table;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::{AnyElement, App, Hsla, SharedString, div, prelude::*};

use crate::github::GithubCodeReferencePreview;
use crate::github::GithubIssueReferenceContext;
use crate::types::{CodeBlock, ParsedMarkdown, Table};

/// A link click handler.
pub type LinkHandlerFn = dyn Fn(&str, &mut gpui::Window, &mut App) + Send + Sync;

// ---------------------------------------------------------------------------
// Render override function signatures
// ---------------------------------------------------------------------------

/// Override for paragraph rendering.
///
/// Receives the default-rendered paragraph element and the app context.
/// Return a replacement element.
pub type ParagraphRenderFn = dyn Fn(AnyElement, &App) -> AnyElement + Send + Sync;

/// Override for heading rendering.
///
/// Receives the heading level (1–6), the default-rendered element, and the app context.
pub type HeadingRenderFn = dyn Fn(u8, AnyElement, &App) -> AnyElement + Send + Sync;

/// Override for code block rendering.
///
/// Receives the raw [`CodeBlock`] data (language hint + code string) and the app context.
/// The override is responsible for building the entire element from scratch.
pub type CodeBlockRenderFn = dyn Fn(&CodeBlock, &App) -> AnyElement + Send + Sync;

/// Override for list rendering.
///
/// Receives the default-rendered list container element and the app context.
pub type ListRenderFn = dyn Fn(AnyElement, &App) -> AnyElement + Send + Sync;

/// Override for individual list item rendering.
///
/// Receives a [`ListItemView`] with the bullet string, checkbox state, and
/// rendered content element.
pub type ListItemRenderFn = dyn Fn(ListItemView, &App) -> AnyElement + Send + Sync;

/// Override for block quote rendering.
///
/// Receives the default-rendered block quote element and the app context.
pub type BlockQuoteRenderFn = dyn Fn(AnyElement, &App) -> AnyElement + Send + Sync;

/// Override for thematic break rendering.
///
/// Receives only the app context — no default element (since the default is trivial).
pub type ThematicBreakRenderFn = dyn Fn(&App) -> AnyElement + Send + Sync;

/// Override for table rendering.
///
/// Receives the raw [`Table`] data and the app context.
/// The override is responsible for building the entire element from scratch.
pub type TableRenderFn = dyn Fn(&Table, &App) -> AnyElement + Send + Sync;

/// Data passed to the list item render override.
pub struct ListItemView {
  /// The bullet or marker string (`"•"`, `"1."`, `"☑"`, `"☐"`, …).
  pub bullet: String,
  /// Task-list checkbox state: `Some(true)` = checked, `Some(false)` = unchecked, `None` = no checkbox.
  pub checked: Option<bool>,
  /// The rendered content of the list item.
  pub content: AnyElement,
}

/// Optional closures that override the default rendering for each block type.
///
/// When a closure is `Some`, it replaces the default renderer entirely.
/// When `None`, the default built-in renderer is used.
#[derive(Clone, Default)]
pub struct RenderOverrides {
  pub paragraph: Option<Arc<ParagraphRenderFn>>,
  pub heading: Option<Arc<HeadingRenderFn>>,
  pub code_block: Option<Arc<CodeBlockRenderFn>>,
  pub list: Option<Arc<ListRenderFn>>,
  pub list_item: Option<Arc<ListItemRenderFn>>,
  pub block_quote: Option<Arc<BlockQuoteRenderFn>>,
  pub thematic_break: Option<Arc<ThematicBreakRenderFn>>,
  pub table: Option<Arc<TableRenderFn>>,
}

/// What to do after a link click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkAction {
  /// Open the link with the default handler.
  Open,
  /// The handler already dealt with the link.
  Handled,
}

/// Persistent state for `<details>` toggle (open/close).
///
/// Shared via `Arc` so that click callbacks can mutate it.
#[derive(Clone, Default)]
pub struct DetailsState {
  /// Maps a details block ID → open/closed.
  open_map: Arc<Mutex<HashMap<usize, bool>>>,
  /// Counter for assigning unique IDs to details blocks during rendering.
  counter: Arc<AtomicUsize>,
}

impl DetailsState {
  /// Reset the counter before a new render pass (so IDs are stable).
  pub fn reset_counter(&self) {
    self.counter.store(0, Ordering::Relaxed);
  }

  /// Get the next details block ID.
  pub fn next_id(&self) -> usize {
    self.counter.fetch_add(1, Ordering::Relaxed)
  }

  /// Check if a details block is open, defaulting to `default_open`.
  pub fn is_open(&self, id: usize, default_open: bool) -> bool {
    let mut map = self.open_map.lock().unwrap();
    *map.entry(id).or_insert(default_open)
  }

  /// Toggle a details block's state, returning the new state.
  pub fn toggle(&self, id: usize, default_open: bool) -> bool {
    let mut map = self.open_map.lock().unwrap();
    let current = *map.entry(id).or_insert(default_open);
    let next = !current;
    map.insert(id, next);
    next
  }
}

// ---------------------------------------------------------------------------
// Text selection state
// ---------------------------------------------------------------------------

/// Selection granularity — how mouse-down click count maps to selection extent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionMode {
  /// Single click — character-level selection.
  Char,
  /// Double click — word-level selection.
  Word,
  /// Triple click — line-level selection.
  Line,
}

/// Currently active text selection (drag in progress or completed).
#[derive(Clone, Debug)]
pub struct ActiveSelection {
  /// Which text block the selection is in.
  pub text_id: usize,
  /// Byte offset of the anchor (where mouse-down occurred).
  pub anchor: usize,
  /// Byte offset of the head (current mouse position).
  pub head: usize,
  /// Whether the user is currently dragging.
  pub dragging: bool,
  /// Selection granularity (char / word / line).
  pub mode: SelectionMode,
  /// The initially-selected range from the click point (word or line range).
  /// Used to keep the anchor range stable while extending during drag.
  pub initial_range: Option<std::ops::Range<usize>>,
}

/// Persistent state for text selection across all rendered text blocks.
///
/// Shared via `Arc` so that mouse-event callbacks can mutate it.
/// Works like [`DetailsState`] — create once, pass to every render call,
/// and it retains state across re-renders.
#[derive(Clone, Default)]
pub struct SelectionState {
  /// The currently active selection, if any.
  selection: Arc<Mutex<Option<ActiveSelection>>>,
  /// Counter for assigning unique text-block IDs during rendering.
  counter: Arc<AtomicUsize>,
  /// Selection highlight background colour.
  /// Falls back to a semi-transparent blue if not set.
  selection_color: Arc<Mutex<Option<Hsla>>>,
}

impl SelectionState {
  /// Reset the text-block counter before a new render pass.
  pub fn reset_counter(&self) {
    self.counter.store(0, Ordering::Relaxed);
  }

  /// Get the next text-block ID.
  pub fn next_text_id(&self) -> usize {
    self.counter.fetch_add(1, Ordering::Relaxed)
  }

  /// Set the selection highlight colour.
  pub fn set_selection_color(&self, color: Hsla) {
    *self.selection_color.lock().unwrap() = Some(color);
  }

  /// Get the selection colour (defaults to semi-transparent blue).
  pub fn selection_color(&self) -> Hsla {
    self.selection_color.lock().unwrap().unwrap_or(Hsla {
      h: 0.58,
      s: 0.6,
      l: 0.5,
      a: 0.3,
    })
  }

  /// Update the selection state.
  pub fn update(&self, text_id: usize, anchor: usize, head: usize, dragging: bool) {
    let mut sel = self.selection.lock().unwrap();
    // Preserve existing mode + initial_range when just extending.
    let (mode, initial_range) = sel
      .as_ref()
      .filter(|s| s.text_id == text_id)
      .map(|s| (s.mode, s.initial_range.clone()))
      .unwrap_or((SelectionMode::Char, None));
    *sel = Some(ActiveSelection {
      text_id,
      anchor,
      head,
      dragging,
      mode,
      initial_range,
    });
  }

  /// Update the selection state with explicit mode and initial range.
  pub fn update_with_mode(
    &self,
    text_id: usize,
    anchor: usize,
    head: usize,
    dragging: bool,
    mode: SelectionMode,
    initial_range: Option<std::ops::Range<usize>>,
  ) {
    *self.selection.lock().unwrap() = Some(ActiveSelection {
      text_id,
      anchor,
      head,
      dragging,
      mode,
      initial_range,
    });
  }

  /// Clear the selection.
  pub fn clear(&self) {
    *self.selection.lock().unwrap() = None;
  }

  /// Get the current selection state for a specific text block.
  pub fn selection_for(&self, text_id: usize) -> Option<ActiveSelection> {
    self
      .selection
      .lock()
      .unwrap()
      .as_ref()
      .filter(|s| s.text_id == text_id)
      .cloned()
  }

  /// Get the current dragging state (any text block).
  pub fn is_dragging(&self) -> bool {
    self
      .selection
      .lock()
      .unwrap()
      .as_ref()
      .is_some_and(|s| s.dragging)
  }

  /// Get the normalised byte range of the current selection for a given text block.
  ///
  /// Returns `None` if there's no selection or the selection is in a different block.
  pub fn selection_range_for(&self, text_id: usize, text: &str) -> Option<std::ops::Range<usize>> {
    let sel = self.selection.lock().unwrap();
    let active = sel.as_ref()?;
    if active.text_id != text_id || active.anchor == active.head {
      return None;
    }
    let text_len = text.len();
    let start = clamp_to_char_boundary(text, active.anchor.min(active.head).min(text_len));
    let end = clamp_to_char_boundary(text, active.anchor.max(active.head).min(text_len));
    if start >= end { None } else { Some(start..end) }
  }

  /// Extract the selected text for a given text block.
  pub fn selected_text(&self, text_id: usize, text: &str) -> Option<String> {
    let range = self.selection_range_for(text_id, text)?;
    text.get(range).map(|s| s.to_string())
  }
}

/// Clamp a byte index to a valid UTF-8 char boundary.
///
/// If `index` falls in the middle of a multi-byte character it is rounded
/// down to the start of that character.
pub fn clamp_to_char_boundary(text: &str, index: usize) -> usize {
  if index >= text.len() {
    return text.len();
  }
  // Walk backward until we hit a char boundary.
  let mut i = index;
  while !text.is_char_boundary(i) && i > 0 {
    i -= 1;
  }
  i
}

/// Find the byte range of the word at the given byte index.
///
/// A "word" is a contiguous run of alphanumeric/underscore characters.
/// If the index is on a non-word character the range covers that single character.
pub fn word_range_at(text: &str, index: usize) -> std::ops::Range<usize> {
  let index = clamp_to_char_boundary(text, index.min(text.len()));
  if index >= text.len() {
    return text.len()..text.len();
  }

  let ch = text[index..].chars().next().unwrap();
  let is_word_char = |c: char| c.is_alphanumeric() || c == '_';

  if is_word_char(ch) {
    // Scan backward.
    let mut start = index;
    while start > 0 {
      let prev = clamp_to_char_boundary(text, start - 1);
      if prev == start {
        break;
      }
      if text[prev..].chars().next().map_or(false, is_word_char) {
        start = prev;
      } else {
        break;
      }
    }
    // Scan forward.
    let mut end = index;
    while end < text.len() {
      let c = text[end..].chars().next().unwrap();
      if is_word_char(c) {
        end += c.len_utf8();
      } else {
        break;
      }
    }
    start..end
  } else {
    // Non-word character — select just that character.
    index..(index + ch.len_utf8())
  }
}

/// Find the byte range of the line at the given byte index.
///
/// A "line" runs from the previous `\n` (exclusive) to the next `\n` (inclusive of content, exclusive of the newline itself).
pub fn line_range_at(text: &str, index: usize) -> std::ops::Range<usize> {
  let index = index.min(text.len());
  let start = text[..index].rfind('\n').map_or(0, |pos| pos + 1);
  let end = text[index..]
    .find('\n')
    .map_or(text.len(), |pos| index + pos);
  start..end
}

/// Apply a selection highlight to existing text runs.
///
/// Splits runs at selection boundaries and adds a background colour to the
/// selected portion.
pub fn apply_selection_to_runs(
  runs: Vec<gpui::TextRun>,
  selection: std::ops::Range<usize>,
  selection_color: Hsla,
) -> Vec<gpui::TextRun> {
  let mut updated = Vec::new();
  let mut offset = 0usize;
  for run in runs {
    let run_start = offset;
    let run_end = offset + run.len;
    offset = run_end;

    // No overlap — keep as-is.
    if selection.end <= run_start || selection.start >= run_end {
      updated.push(run);
      continue;
    }

    let overlap_start = selection.start.max(run_start);
    let overlap_end = selection.end.min(run_end);

    // Prefix before selection.
    if overlap_start > run_start {
      let mut prefix = run.clone();
      prefix.len = overlap_start - run_start;
      updated.push(prefix);
    }

    // Selected portion.
    let mut selected = run.clone();
    selected.len = overlap_end - overlap_start;
    selected.background_color = Some(selection_color);
    updated.push(selected);

    // Suffix after selection.
    if overlap_end < run_end {
      let mut suffix = run.clone();
      suffix.len = run_end - overlap_end;
      updated.push(suffix);
    }
  }
  updated
}

/// Theme colors for markdown rendering.
///
/// Provide sensible defaults so rendering works out of the box without
/// depending on `gpui_component::ActiveTheme`.
#[derive(Clone, Debug)]
pub struct MarkdownTheme {
  pub foreground: Hsla,
  pub muted_foreground: Hsla,
  pub background: Hsla,
  pub code_background: Hsla,
  pub border: Hsla,
  pub link: Hsla,
  pub accent: Hsla,
  /// Font family for code blocks and inline code.
  pub code_font_family: SharedString,
  /// Whether this is a dark theme (used for dark/light image URL selection).
  pub is_dark: bool,
}

impl Default for MarkdownTheme {
  fn default() -> Self {
    Self::dark()
  }
}

impl MarkdownTheme {
  /// A reasonable dark theme.
  pub fn dark() -> Self {
    Self {
      foreground: Hsla {
        h: 0.0,
        s: 0.0,
        l: 0.93,
        a: 1.0,
      },
      muted_foreground: Hsla {
        h: 0.0,
        s: 0.0,
        l: 0.6,
        a: 1.0,
      },
      background: Hsla {
        h: 0.0,
        s: 0.0,
        l: 0.1,
        a: 1.0,
      },
      code_background: Hsla {
        h: 0.0,
        s: 0.0,
        l: 0.15,
        a: 1.0,
      },
      border: Hsla {
        h: 0.0,
        s: 0.0,
        l: 0.25,
        a: 1.0,
      },
      link: Hsla {
        h: 0.58,
        s: 0.8,
        l: 0.65,
        a: 1.0,
      },
      accent: Hsla {
        h: 0.58,
        s: 0.7,
        l: 0.5,
        a: 1.0,
      },
      code_font_family: "Menlo".into(),
      is_dark: true,
    }
  }

  /// A reasonable light theme.
  pub fn light() -> Self {
    Self {
      foreground: Hsla {
        h: 0.0,
        s: 0.0,
        l: 0.13,
        a: 1.0,
      },
      muted_foreground: Hsla {
        h: 0.0,
        s: 0.0,
        l: 0.45,
        a: 1.0,
      },
      background: Hsla {
        h: 0.0,
        s: 0.0,
        l: 1.0,
        a: 1.0,
      },
      code_background: Hsla {
        h: 0.0,
        s: 0.0,
        l: 0.96,
        a: 1.0,
      },
      border: Hsla {
        h: 0.0,
        s: 0.0,
        l: 0.85,
        a: 1.0,
      },
      link: Hsla {
        h: 0.58,
        s: 0.8,
        l: 0.45,
        a: 1.0,
      },
      accent: Hsla {
        h: 0.58,
        s: 0.7,
        l: 0.5,
        a: 1.0,
      },
      code_font_family: "Menlo".into(),
      is_dark: false,
    }
  }
}

/// A function that provides a custom [`gpui::ImageSource`] for a resolved URL.
///
/// When set on [`MarkdownRenderOptions`], this is called instead of using
/// `gpui::img(url)` directly. Useful for auth headers, custom caching, etc.
pub type ImageLoaderFn = dyn Fn(&str) -> gpui::ImageSource + Send + Sync;

/// Options for rendering markdown.
#[derive(Clone, Default)]
pub struct MarkdownRenderOptions {
  /// Link click handler.
  pub on_link: Option<Arc<LinkHandlerFn>>,
  /// Theme colors.
  pub theme: Option<MarkdownTheme>,
  /// Whether code blocks render at full height (no scroll cap).
  pub expand_code_blocks: bool,
  /// Base URL for resolving relative image paths.
  pub image_base_url: Option<SharedString>,
  /// Context for auto-linking GitHub issue references (`#123`).
  ///
  /// When set, bare `#123` patterns in text are converted to clickable links
  /// pointing to `https://github.com/{owner}/{repo}/issues/{num}`.
  pub github_issue_reference_context: Option<GithubIssueReferenceContext>,
  /// Custom image source provider.
  ///
  /// When set, this function is called with the resolved image URL to produce
  /// a [`gpui::ImageSource`]. When `None`, `gpui::img(url)` is used directly
  /// (which requires an HTTP client registered on the GPUI `App`).
  pub image_loader: Option<Arc<ImageLoaderFn>>,
  /// Persistent state for `<details>` toggle.
  ///
  /// Created automatically on first use. Persists across re-renders so
  /// toggle state is maintained.
  pub details_state: DetailsState,
  /// Code reference preview cards.
  ///
  /// A map from GitHub blob URL → preview data. When a standalone URL line
  /// in the markdown source matches a key, it is replaced by a card showing
  /// the file label, line range, and code snippet.
  pub github_code_reference_previews: Option<Arc<HashMap<Arc<str>, GithubCodeReferencePreview>>>,
  /// Optional render overrides for each block type.
  ///
  /// When a closure is set, it replaces the default built-in renderer.
  pub overrides: RenderOverrides,
  /// Show small dots at leading whitespace positions in code blocks.
  ///
  /// When enabled, each leading space in a code block line is rendered
  /// with a faint dot, similar to "Show Indentation Guides" in editors.
  pub show_indentation_dots: bool,
  /// Internal: persistent state for text selection
  pub(crate) selection_state: SelectionState,
}

impl MarkdownRenderOptions {
  pub fn with_on_link(mut self, handler: Arc<LinkHandlerFn>) -> Self {
    self.on_link = Some(handler);
    self
  }

  pub fn with_theme(mut self, theme: MarkdownTheme) -> Self {
    self.theme = Some(theme);
    self
  }

  pub fn with_expanded_code_blocks(mut self) -> Self {
    self.expand_code_blocks = true;
    self
  }

  pub fn with_image_base_url(mut self, url: impl Into<SharedString>) -> Self {
    self.image_base_url = Some(url.into());
    self
  }

  pub fn with_github_issue_context(
    mut self,
    owner: impl Into<Arc<str>>,
    repo: impl Into<Arc<str>>,
  ) -> Self {
    self.github_issue_reference_context = Some(GithubIssueReferenceContext {
      owner: owner.into(),
      repo: repo.into(),
    });
    self
  }

  pub fn with_image_loader(mut self, loader: Arc<ImageLoaderFn>) -> Self {
    self.image_loader = Some(loader);
    self
  }

  pub fn with_github_code_reference_previews(
    mut self,
    previews: Arc<HashMap<Arc<str>, GithubCodeReferencePreview>>,
  ) -> Self {
    self.github_code_reference_previews = Some(previews);
    self
  }

  pub fn with_overrides(mut self, overrides: RenderOverrides) -> Self {
    self.overrides = overrides;
    self
  }

  pub fn with_indentation_dots(mut self) -> Self {
    self.show_indentation_dots = true;
    self
  }

  /// Get the theme, falling back to dark theme default.
  pub fn theme(&self) -> &MarkdownTheme {
    self.theme.as_ref().unwrap_or(&DEFAULT_DARK_THEME)
  }
}

static DEFAULT_DARK_THEME: std::sync::LazyLock<MarkdownTheme> =
  std::sync::LazyLock::new(MarkdownTheme::dark);

/// Render a markdown source string to a GPUI element.
pub fn render_markdown(source: &str, options: &MarkdownRenderOptions, cx: &App) -> AnyElement {
  render_markdown_impl(source, options, None, cx)
}

/// Render a markdown source string to a GPUI element, with LRU caching.
///
/// Identical to [`render_markdown`] but reuses cached parse results when the
/// same source string is rendered again (e.g. on every re-render of a view).
pub fn render_markdown_cached(
  source: &str,
  options: &MarkdownRenderOptions,
  cache: &mut crate::cache::MarkdownCache,
  cx: &App,
) -> AnyElement {
  render_markdown_impl(source, options, Some(cache), cx)
}

fn render_markdown_impl(
  source: &str,
  options: &MarkdownRenderOptions,
  cache: Option<&mut crate::cache::MarkdownCache>,
  cx: &App,
) -> AnyElement {
  // If code reference previews are provided, split the source at matching
  // URL lines and render each segment separately.
  if let Some(previews) = &options.github_code_reference_previews {
    if !previews.is_empty() {
      return render_markdown_with_previews(source, previews, options, cache, cx);
    }
  }
  let parsed = match cache {
    Some(c) => c.get_or_parse(source),
    None => crate::parse::parse_markdown(source),
  };
  render_parsed_markdown(&parsed, options, cx)
}

/// Render markdown with code reference preview cards replacing matching URL lines.
fn render_markdown_with_previews(
  source: &str,
  previews: &HashMap<Arc<str>, GithubCodeReferencePreview>,
  options: &MarkdownRenderOptions,
  mut cache: Option<&mut crate::cache::MarkdownCache>,
  cx: &App,
) -> AnyElement {
  use crate::github::{MarkdownPreviewSegment, split_markdown_preview_segments};

  let segments = split_markdown_preview_segments(source, previews);
  let has_previews = segments
    .iter()
    .any(|s| matches!(s, MarkdownPreviewSegment::Preview(_)));

  if !has_previews {
    let parsed = match cache {
      Some(ref mut c) => c.get_or_parse(source),
      None => crate::parse::parse_markdown(source),
    };
    return render_parsed_markdown(&parsed, options, cx);
  }

  // Reset counters ONCE for the entire document so that text-block IDs
  // are unique across all segments (avoids colliding IDs that would cause
  // multiple blocks to highlight simultaneously).
  options.details_state.reset_counter();
  options.selection_state.reset_counter();

  let mut container = div().flex().flex_col();
  for segment in &segments {
    match segment {
      MarkdownPreviewSegment::Markdown(markdown) => {
        if !markdown.is_empty() {
          let parsed = match cache {
            Some(ref mut c) => c.get_or_parse(markdown),
            None => crate::parse::parse_markdown(markdown),
          };
          // Use render_blocks directly — counters already reset above.
          container = container.child(blocks::render_blocks(parsed.blocks(), options, 0, cx));
        }
      }
      MarkdownPreviewSegment::Preview(preview) => {
        container = container.child(code_preview::render_code_reference_card(
          preview, options, cx,
        ));
      }
    }
  }
  container.into_any_element()
}

/// Render a pre-parsed markdown document to a GPUI element.
pub fn render_parsed_markdown(
  parsed: &ParsedMarkdown,
  options: &MarkdownRenderOptions,
  cx: &App,
) -> AnyElement {
  // Reset the details ID counter so IDs are stable across re-renders.
  options.details_state.reset_counter();
  // Reset the selection text-block counter so IDs are stable.
  options.selection_state.reset_counter();
  blocks::render_blocks(parsed.blocks(), options, 0, cx)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn details_state_default_closed() {
    let state = DetailsState::default();
    // Default to false when not explicitly set
    assert!(!state.is_open(0, false));
  }

  #[test]
  fn details_state_default_open() {
    let state = DetailsState::default();
    // Respects default_open = true
    assert!(state.is_open(0, true));
  }

  #[test]
  fn details_state_toggle() {
    let state = DetailsState::default();
    // Initially closed (default_open = false)
    assert!(!state.is_open(0, false));
    // Toggle → open
    state.toggle(0, false);
    assert!(state.is_open(0, false));
    // Toggle → closed again
    state.toggle(0, false);
    assert!(!state.is_open(0, false));
  }

  #[test]
  fn details_state_toggle_from_open() {
    let state = DetailsState::default();
    // Initially open (default_open = true)
    assert!(state.is_open(0, true));
    // Toggle → closed
    state.toggle(0, true);
    assert!(!state.is_open(0, true));
  }

  #[test]
  fn details_state_independent_ids() {
    let state = DetailsState::default();
    state.toggle(0, false); // id 0 → open
    // id 1 should still be at default (closed)
    assert!(state.is_open(0, false));
    assert!(!state.is_open(1, false));
  }

  #[test]
  fn details_state_counter_increments() {
    let state = DetailsState::default();
    assert_eq!(state.next_id(), 0);
    assert_eq!(state.next_id(), 1);
    assert_eq!(state.next_id(), 2);
  }

  #[test]
  fn details_state_counter_resets() {
    let state = DetailsState::default();
    assert_eq!(state.next_id(), 0);
    assert_eq!(state.next_id(), 1);
    state.reset_counter();
    assert_eq!(state.next_id(), 0);
  }

  #[test]
  fn details_state_persists_across_resets() {
    let state = DetailsState::default();
    let id = state.next_id();
    state.toggle(id, false); // open
    assert!(state.is_open(id, false));
    // Reset counter (simulating a re-render)
    state.reset_counter();
    // State should persist
    assert!(state.is_open(0, false));
  }

  // --- clamp_to_char_boundary tests ---

  #[test]
  fn clamp_ascii() {
    assert_eq!(clamp_to_char_boundary("hello", 0), 0);
    assert_eq!(clamp_to_char_boundary("hello", 3), 3);
    assert_eq!(clamp_to_char_boundary("hello", 5), 5);
  }

  #[test]
  fn clamp_beyond_length() {
    assert_eq!(clamp_to_char_boundary("hi", 10), 2);
  }

  #[test]
  fn clamp_multibyte() {
    let text = "aé"; // 'a' = 1 byte, 'é' = 2 bytes → total 3 bytes
    assert_eq!(clamp_to_char_boundary(text, 0), 0);
    assert_eq!(clamp_to_char_boundary(text, 1), 1);
    // index 2 is in the middle of 'é' (bytes 1..3) → clamp to 1
    assert_eq!(clamp_to_char_boundary(text, 2), 1);
    assert_eq!(clamp_to_char_boundary(text, 3), 3);
  }

  #[test]
  fn clamp_emoji() {
    let text = "🦀"; // 4 bytes
    assert_eq!(clamp_to_char_boundary(text, 0), 0);
    assert_eq!(clamp_to_char_boundary(text, 1), 0);
    assert_eq!(clamp_to_char_boundary(text, 2), 0);
    assert_eq!(clamp_to_char_boundary(text, 3), 0);
    assert_eq!(clamp_to_char_boundary(text, 4), 4);
  }

  #[test]
  fn clamp_empty_string() {
    assert_eq!(clamp_to_char_boundary("", 0), 0);
    assert_eq!(clamp_to_char_boundary("", 5), 0);
  }

  // --- SelectionState tests ---

  #[test]
  fn selection_state_default() {
    let state = SelectionState::default();
    assert!(!state.is_dragging());
    assert!(state.selection_for(0).is_none());
    assert!(state.selection_range_for(0, "hello").is_none());
  }

  #[test]
  fn selection_state_counter() {
    let state = SelectionState::default();
    assert_eq!(state.next_text_id(), 0);
    assert_eq!(state.next_text_id(), 1);
    state.reset_counter();
    assert_eq!(state.next_text_id(), 0);
  }

  #[test]
  fn selection_state_update_and_query() {
    let state = SelectionState::default();
    state.update(0, 2, 5, true);
    assert!(state.is_dragging());
    let sel = state.selection_for(0).unwrap();
    assert_eq!(sel.anchor, 2);
    assert_eq!(sel.head, 5);
    assert!(sel.dragging);
    // Wrong text_id → None.
    assert!(state.selection_for(1).is_none());
  }

  #[test]
  fn selection_state_range() {
    let state = SelectionState::default();
    state.update(0, 2, 5, false);
    assert_eq!(state.selection_range_for(0, "hello world"), Some(2..5));
    // Reversed (head < anchor) should normalise.
    state.update(0, 5, 2, false);
    assert_eq!(state.selection_range_for(0, "hello world"), Some(2..5));
  }

  #[test]
  fn selection_state_empty_range() {
    let state = SelectionState::default();
    // anchor == head → None (no selection).
    state.update(0, 3, 3, false);
    assert!(state.selection_range_for(0, "hello").is_none());
  }

  #[test]
  fn selection_state_selected_text() {
    let state = SelectionState::default();
    state.update(0, 6, 11, false);
    assert_eq!(
      state.selected_text(0, "hello world"),
      Some("world".to_string())
    );
  }

  #[test]
  fn selection_state_clear() {
    let state = SelectionState::default();
    state.update(0, 0, 5, true);
    assert!(state.is_dragging());
    state.clear();
    assert!(!state.is_dragging());
    assert!(state.selection_for(0).is_none());
  }

  #[test]
  fn selection_state_color_default() {
    let state = SelectionState::default();
    let color = state.selection_color();
    // Default is semi-transparent blue.
    assert!(color.a < 1.0);
  }

  #[test]
  fn selection_state_custom_color() {
    let state = SelectionState::default();
    let red = Hsla {
      h: 0.0,
      s: 1.0,
      l: 0.5,
      a: 0.4,
    };
    state.set_selection_color(red);
    let color = state.selection_color();
    assert_eq!(color.h, 0.0);
    assert_eq!(color.a, 0.4);
  }

  // --- apply_selection_to_runs tests ---

  #[test]
  fn apply_selection_no_overlap() {
    let runs = vec![gpui::TextRun {
      len: 5,
      ..Default::default()
    }];
    let result = apply_selection_to_runs(
      runs,
      10..15,
      Hsla {
        h: 0.0,
        s: 0.0,
        l: 0.5,
        a: 0.5,
      },
    );
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].len, 5);
    assert!(result[0].background_color.is_none());
  }

  #[test]
  fn apply_selection_full_overlap() {
    let runs = vec![gpui::TextRun {
      len: 5,
      ..Default::default()
    }];
    let sel_color = Hsla {
      h: 0.0,
      s: 0.0,
      l: 0.5,
      a: 0.5,
    };
    let result = apply_selection_to_runs(runs, 0..5, sel_color);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].len, 5);
    assert!(result[0].background_color.is_some());
  }

  #[test]
  fn apply_selection_partial_overlap_start() {
    let runs = vec![gpui::TextRun {
      len: 10,
      ..Default::default()
    }];
    let sel_color = Hsla {
      h: 0.0,
      s: 0.0,
      l: 0.5,
      a: 0.5,
    };
    let result = apply_selection_to_runs(runs, 0..3, sel_color);
    // Should split into: selected(0..3) + unselected(3..10)
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].len, 3);
    assert!(result[0].background_color.is_some());
    assert_eq!(result[1].len, 7);
    assert!(result[1].background_color.is_none());
  }

  #[test]
  fn apply_selection_partial_overlap_middle() {
    let runs = vec![gpui::TextRun {
      len: 10,
      ..Default::default()
    }];
    let sel_color = Hsla {
      h: 0.0,
      s: 0.0,
      l: 0.5,
      a: 0.5,
    };
    let result = apply_selection_to_runs(runs, 3..7, sel_color);
    // Should split into: prefix(0..3) + selected(3..7) + suffix(7..10)
    assert_eq!(result.len(), 3);
    assert_eq!(result[0].len, 3);
    assert!(result[0].background_color.is_none());
    assert_eq!(result[1].len, 4);
    assert!(result[1].background_color.is_some());
    assert_eq!(result[2].len, 3);
    assert!(result[2].background_color.is_none());
  }

  #[test]
  fn apply_selection_across_multiple_runs() {
    let runs = vec![
      gpui::TextRun {
        len: 5,
        ..Default::default()
      },
      gpui::TextRun {
        len: 5,
        ..Default::default()
      },
    ];
    let sel_color = Hsla {
      h: 0.0,
      s: 0.0,
      l: 0.5,
      a: 0.5,
    };
    let result = apply_selection_to_runs(runs, 3..8, sel_color);
    // Run 0 (0..5): prefix(0..3) + selected(3..5)
    // Run 1 (5..10): selected(5..8) + suffix(8..10)
    assert_eq!(result.len(), 4);
    assert_eq!(result[0].len, 3);
    assert!(result[0].background_color.is_none());
    assert_eq!(result[1].len, 2);
    assert!(result[1].background_color.is_some());
    assert_eq!(result[2].len, 3);
    assert!(result[2].background_color.is_some());
    assert_eq!(result[3].len, 2);
    assert!(result[3].background_color.is_none());
  }

  // --- word_range_at tests ---

  #[test]
  fn word_range_simple() {
    assert_eq!(word_range_at("hello world", 0), 0..5);
    assert_eq!(word_range_at("hello world", 2), 0..5);
    assert_eq!(word_range_at("hello world", 4), 0..5);
    assert_eq!(word_range_at("hello world", 6), 6..11);
  }

  #[test]
  fn word_range_on_space() {
    // On the space between words → selects just the space.
    assert_eq!(word_range_at("hello world", 5), 5..6);
  }

  #[test]
  fn word_range_with_underscore() {
    assert_eq!(word_range_at("foo_bar baz", 2), 0..7);
  }

  #[test]
  fn word_range_at_end() {
    assert_eq!(word_range_at("hello", 5), 5..5);
  }

  #[test]
  fn word_range_multibyte() {
    // "café lait" — é is 2 bytes.
    let text = "café lait";
    assert_eq!(word_range_at(text, 0), 0..5); // "café"
    assert_eq!(word_range_at(text, 6), 6..10); // "lait"
  }

  #[test]
  fn word_range_punctuation() {
    // Punctuation is a single non-word character.
    assert_eq!(word_range_at("a.b", 1), 1..2); // just "."
  }

  // --- line_range_at tests ---

  #[test]
  fn line_range_single_line() {
    assert_eq!(line_range_at("hello world", 3), 0..11);
  }

  #[test]
  fn line_range_multi_line() {
    let text = "first\nsecond\nthird";
    assert_eq!(line_range_at(text, 0), 0..5); // "first"
    assert_eq!(line_range_at(text, 3), 0..5); // still "first"
    assert_eq!(line_range_at(text, 6), 6..12); // "second"
    assert_eq!(line_range_at(text, 13), 13..18); // "third"
  }

  #[test]
  fn line_range_at_newline() {
    // Index at the newline itself → belongs to the first line (ends before \n).
    let text = "abc\ndef";
    assert_eq!(line_range_at(text, 3), 0..3); // at the \n → line "abc"
  }

  #[test]
  fn line_range_at_end() {
    assert_eq!(line_range_at("abc\n", 4), 4..4); // empty last line
  }

  // --- Selection-aware rendering tests (require gpui::test) ---

  #[gpui::test]
  fn selection_state_renders_without_panic(cx: &mut gpui::TestAppContext) {
    let options = MarkdownRenderOptions::default();
    cx.update(|cx| {
      let _ = crate::render::render_markdown("Hello **bold** world.\n", &options, cx);
    });
  }

  #[gpui::test]
  fn selection_state_with_links_renders(cx: &mut gpui::TestAppContext) {
    let options = MarkdownRenderOptions::default().with_on_link(Arc::new(|_url, _window, _cx| {}));
    cx.update(|cx| {
      let _ =
        crate::render::render_markdown("Click [here](https://example.com) now.\n", &options, cx);
    });
  }

  #[gpui::test]
  fn selection_state_counter_resets_on_render(cx: &mut gpui::TestAppContext) {
    let options = MarkdownRenderOptions::default();
    let sel = options.selection_state.clone();
    cx.update(|cx| {
      let _ = crate::render::render_markdown("Para one.\n\nPara two.\n", &options, cx);
    });
    // After render, the counter was incremented for each text block.
    let count_after_first = sel.next_text_id();
    // Reset and render again — counter should restart at 0.
    sel.reset_counter();
    cx.update(|cx| {
      let _ = crate::render::render_markdown("Para one.\n\nPara two.\n", &options, cx);
    });
    let count_after_second = sel.next_text_id();
    // Both renders should produce the same number of text blocks.
    assert_eq!(count_after_first, count_after_second);
  }

  #[gpui::test]
  fn selection_ids_unique_across_preview_segments(cx: &mut gpui::TestAppContext) {
    use std::collections::HashMap;
    use std::sync::Arc;

    let preview_url: Arc<str> = "https://github.com/owner/repo/blob/abc123/file.rs#L1-L3".into();
    let mut previews = HashMap::new();
    previews.insert(
      preview_url.clone(),
      crate::github::GithubCodeReferencePreview {
        url: preview_url,
        repo: "owner/repo".into(),
        path: "file.rs".into(),
        reference: "abc123".into(),
        start_line: 1,
        end_line: 3,
        snippets: vec!["fn main() {}".into()],
      },
    );

    let options =
      MarkdownRenderOptions::default().with_github_code_reference_previews(Arc::new(previews));
    let sel = options.selection_state.clone();

    // Source has text before and after the preview URL line.
    let source =
      "Para before.\n\nhttps://github.com/owner/repo/blob/abc123/file.rs#L1-L3\n\nPara after.\n";

    cx.update(|cx| {
      let _ = crate::render::render_markdown(source, &options, cx);
    });

    // There are 2 text blocks (one per markdown segment), each should have
    // a unique text_id. The next_text_id call tells us the counter reached
    // at least 2.
    let next = sel.next_text_id();
    assert!(
      next >= 2,
      "Expected at least 2 unique text_ids across segments, got {next}"
    );
  }

  // --- Render override tests (require gpui::test) ---

  use std::sync::atomic::{AtomicUsize, Ordering};

  #[gpui::test]
  fn override_heading_is_called(cx: &mut gpui::TestAppContext) {
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    let options = MarkdownRenderOptions {
      overrides: RenderOverrides {
        heading: Some(Arc::new(move |level, el, _cx| {
          counter_clone.fetch_add(1, Ordering::Relaxed);
          assert!(level >= 1 && level <= 6);
          el
        })),
        ..Default::default()
      },
      ..Default::default()
    };
    cx.update(|cx| {
      let _ = crate::render::render_markdown("# Hello\n\n## World\n", &options, cx);
    });
    assert_eq!(counter.load(Ordering::Relaxed), 2);
  }

  #[gpui::test]
  fn override_paragraph_is_called(cx: &mut gpui::TestAppContext) {
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    let options = MarkdownRenderOptions {
      overrides: RenderOverrides {
        paragraph: Some(Arc::new(move |el, _cx| {
          counter_clone.fetch_add(1, Ordering::Relaxed);
          el
        })),
        ..Default::default()
      },
      ..Default::default()
    };
    cx.update(|cx| {
      let _ =
        crate::render::render_markdown("First paragraph.\n\nSecond paragraph.\n", &options, cx);
    });
    assert_eq!(counter.load(Ordering::Relaxed), 2);
  }

  #[gpui::test]
  fn override_code_block_is_called(cx: &mut gpui::TestAppContext) {
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    let options = MarkdownRenderOptions {
      overrides: RenderOverrides {
        code_block: Some(Arc::new(move |code, _cx| {
          counter_clone.fetch_add(1, Ordering::Relaxed);
          assert_eq!(code.lang.as_deref(), Some("rust"));
          assert!(code.value.contains("fn main"));
          div().into_any_element()
        })),
        ..Default::default()
      },
      ..Default::default()
    };
    cx.update(|cx| {
      let _ = crate::render::render_markdown("```rust\nfn main() {}\n```\n", &options, cx);
    });
    assert_eq!(counter.load(Ordering::Relaxed), 1);
  }

  #[gpui::test]
  fn override_thematic_break_is_called(cx: &mut gpui::TestAppContext) {
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    let options = MarkdownRenderOptions {
      overrides: RenderOverrides {
        thematic_break: Some(Arc::new(move |_cx| {
          counter_clone.fetch_add(1, Ordering::Relaxed);
          div().into_any_element()
        })),
        ..Default::default()
      },
      ..Default::default()
    };
    cx.update(|cx| {
      let _ = crate::render::render_markdown("Above\n\n---\n\nBelow\n", &options, cx);
    });
    assert_eq!(counter.load(Ordering::Relaxed), 1);
  }

  #[gpui::test]
  fn override_block_quote_is_called(cx: &mut gpui::TestAppContext) {
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    let options = MarkdownRenderOptions {
      overrides: RenderOverrides {
        block_quote: Some(Arc::new(move |el, _cx| {
          counter_clone.fetch_add(1, Ordering::Relaxed);
          el
        })),
        ..Default::default()
      },
      ..Default::default()
    };
    cx.update(|cx| {
      let _ = crate::render::render_markdown("> Quote text\n", &options, cx);
    });
    assert_eq!(counter.load(Ordering::Relaxed), 1);
  }

  #[gpui::test]
  fn override_table_is_called(cx: &mut gpui::TestAppContext) {
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    let options = MarkdownRenderOptions {
      overrides: RenderOverrides {
        table: Some(Arc::new(move |table, _cx| {
          counter_clone.fetch_add(1, Ordering::Relaxed);
          assert_eq!(table.headers.len(), 2);
          div().into_any_element()
        })),
        ..Default::default()
      },
      ..Default::default()
    };
    cx.update(|cx| {
      let _ = crate::render::render_markdown("| A | B |\n|---|---|\n| 1 | 2 |\n", &options, cx);
    });
    assert_eq!(counter.load(Ordering::Relaxed), 1);
  }

  #[gpui::test]
  fn override_list_is_called(cx: &mut gpui::TestAppContext) {
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    let options = MarkdownRenderOptions {
      overrides: RenderOverrides {
        list: Some(Arc::new(move |el, _cx| {
          counter_clone.fetch_add(1, Ordering::Relaxed);
          el
        })),
        ..Default::default()
      },
      ..Default::default()
    };
    cx.update(|cx| {
      let _ = crate::render::render_markdown("- A\n- B\n- C\n", &options, cx);
    });
    assert_eq!(counter.load(Ordering::Relaxed), 1);
  }

  #[gpui::test]
  fn override_list_item_is_called(cx: &mut gpui::TestAppContext) {
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    let options = MarkdownRenderOptions {
      overrides: RenderOverrides {
        list_item: Some(Arc::new(move |item, _cx| {
          counter_clone.fetch_add(1, Ordering::Relaxed);
          assert_eq!(item.bullet, "•");
          item.content
        })),
        ..Default::default()
      },
      ..Default::default()
    };
    cx.update(|cx| {
      let _ = crate::render::render_markdown("- One\n- Two\n", &options, cx);
    });
    assert_eq!(counter.load(Ordering::Relaxed), 2);
  }

  #[gpui::test]
  fn no_overrides_uses_defaults(cx: &mut gpui::TestAppContext) {
    // No overrides set → should render without panic.
    let options = MarkdownRenderOptions::default();
    cx.update(|cx| {
      let _ = crate::render::render_markdown(
        "# Title\n\nText.\n\n> Quote\n\n---\n\n```rs\ncode\n```\n\n- A\n- B\n\n| H |\n|---|\n| V |\n",
        &options,
        cx,
      );
    });
  }
}
