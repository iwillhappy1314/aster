//! Selectable text element — wraps `StyledText` and adds click-drag text selection.
//!
//! Selection stays highlighted after mouse-up. Copying is explicit via the
//! context menu (or a host-provided keyboard action), matching normal macOS
//! text-selection behaviour.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, LazyLock, Mutex};

use gpui::{
  AnyElement, App, ClipboardItem, CursorStyle, DispatchPhase, Element, ElementId,
  GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId,
  FocusHandle, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
  SharedString,
  StatefulInteractiveElement, StyledText, TextRun, Window, anchored, deferred, div, prelude::*, px,
};

use super::{
  LinkHandlerFn, MarkdownRenderOptions, SelectionMode, SelectionState, apply_selection_to_runs,
  clamp_to_char_boundary, line_range_at, word_range_at,
};

/// A link range within the text.
#[derive(Clone, Debug)]
pub struct LinkRange {
  /// Byte range of the link text.
  pub range: Range<usize>,
  /// Target URL.
  pub url: String,
}

#[derive(Clone, Debug)]
struct CrossBlockSelection {
  anchor_text_id: usize,
  anchor: usize,
  head_text_id: usize,
  head: usize,
  dragging: bool,
  mode: SelectionMode,
  initial_range: Option<Range<usize>>,
}

#[derive(Clone, Debug)]
struct TextContextMenu {
  owner_text_id: usize,
  position: Point<Pixels>,
  link: Option<String>,
}

#[derive(Default)]
struct CrossBlockState {
  selection: Option<CrossBlockSelection>,
  texts: HashMap<usize, String>,
  context_menu: Option<TextContextMenu>,
}

static CROSS_BLOCK_SELECTIONS: LazyLock<Mutex<HashMap<usize, CrossBlockState>>> =
  LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Copy)]
struct ContextMenuTheme {
  background: Hsla,
  foreground: Hsla,
  muted_foreground: Hsla,
  border: Hsla,
  hover: Hsla,
}

impl ContextMenuTheme {
  fn from_runs(runs: &[TextRun]) -> Self {
    let foreground = runs.first().map(|run| run.color).unwrap_or(Hsla {
      h: 0.0,
      s: 0.0,
      l: 0.15,
      a: 1.0,
    });
    let is_dark = foreground.l > 0.55;
    let background = Hsla {
      h: 0.0,
      s: 0.0,
      l: if is_dark { 0.12 } else { 0.99 },
      a: 1.0,
    };
    let hover = Hsla {
      h: 0.0,
      s: 0.0,
      l: if is_dark { 0.20 } else { 0.93 },
      a: 1.0,
    };
    let muted_foreground = Hsla {
      a: 0.62,
      ..foreground
    };
    let border = Hsla {
      a: if is_dark { 0.24 } else { 0.16 },
      ..foreground
    };

    Self {
      background,
      foreground,
      muted_foreground,
      border,
      hover,
    }
  }
}

/// Public wrapper that adds the preview context menu around the custom
/// selectable element without depending on a separate UI component library.
pub struct SelectableText {
  inner: SelectableTextElement,
  menu_theme: ContextMenuTheme,
}

impl SelectableText {
  pub fn new(
    text: SharedString,
    base_runs: Vec<TextRun>,
    link_ranges: Vec<LinkRange>,
    selection_state: SelectionState,
    focus_handle: Option<FocusHandle>,
    search_query: Option<SharedString>,
    search_highlight_color: Option<Hsla>,
    on_link: Option<Arc<LinkHandlerFn>>,
    text_id: usize,
  ) -> Self {
    register_cross_block_text(&selection_state, text_id, text.as_ref());
    let menu_theme = ContextMenuTheme::from_runs(&base_runs);
    let styled_text = StyledText::new(text.clone()).with_runs(base_runs.clone());
    Self {
      inner: SelectableTextElement {
        text,
        base_runs,
        link_ranges,
        selection_state,
        focus_handle,
        search_query,
        search_highlight_color,
        on_link,
        text_id,
        styled_text,
        last_selection: None,
        last_search_query: None,
      },
      menu_theme,
    }
  }
}

/// The low-level text element. The outer [`SelectableText`] wrapper supplies
/// the preview context menu without changing text layout.
struct SelectableTextElement {
  /// The text content.
  text: SharedString,
  /// Styled text runs (without selection highlight).
  base_runs: Vec<TextRun>,
  /// Clickable link ranges within the text.
  link_ranges: Vec<LinkRange>,
  /// Shared selection state (across all text blocks in the document).
  selection_state: SelectionState,
  /// Host focus target that enables preview keyboard actions.
  focus_handle: Option<FocusHandle>,
  /// Query highlighted within this rendered text block.
  search_query: Option<SharedString>,
  /// Background color applied to query matches.
  search_highlight_color: Option<Hsla>,
  /// Link click handler.
  on_link: Option<Arc<LinkHandlerFn>>,
  /// Unique ID for this text block within the current render pass.
  text_id: usize,
  /// The inner `StyledText` used for layout & painting.
  styled_text: StyledText,
  /// Last selection range applied (used to avoid re-building runs).
  last_selection: Option<Range<usize>>,
  /// Last search query applied to the rendered runs.
  last_search_query: Option<SharedString>,
}

impl SelectableTextElement {
  /// Rebuild the styled text runs if the selection changed.
  fn ensure_runs_up_to_date(&mut self) {
    let selection = cross_block_selection_range(
      &self.selection_state,
      self.text_id,
      self.text.as_ref(),
    );

    if selection == self.last_selection && self.search_query == self.last_search_query {
      return;
    }

    let mut runs = self.base_runs.clone();
    if let (Some(query), Some(color)) = (&self.search_query, self.search_highlight_color) {
      for range in find_matches_case_insensitive(self.text.as_ref(), query.as_ref()) {
        runs = apply_selection_to_runs(runs, range, color);
      }
    }

    let runs = if let Some(ref sel) = selection {
      apply_selection_to_runs(
        runs,
        sel.clone(),
        self.selection_state.selection_color(),
      )
    } else {
      runs
    };

    self.styled_text = StyledText::new(self.text.clone()).with_runs(runs);
    self.last_selection = selection;
    self.last_search_query = self.search_query.clone();
  }
}

impl Element for SelectableTextElement {
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
  ) -> (LayoutId, Self::RequestLayoutState) {
    self.ensure_runs_up_to_date();
    let (layout_id, _) = self
      .styled_text
      .request_layout(None, inspector_id, window, cx);
    (layout_id, ())
  }

  fn prepaint(
    &mut self,
    _id: Option<&GlobalElementId>,
    inspector_id: Option<&InspectorElementId>,
    bounds: gpui::Bounds<gpui::Pixels>,
    _request_layout: &mut Self::RequestLayoutState,
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
    bounds: gpui::Bounds<gpui::Pixels>,
    _request_layout: &mut Self::RequestLayoutState,
    hitbox: &mut Hitbox,
    window: &mut Window,
    cx: &mut App,
  ) {
    // Get the TextLayout from StyledText — available after prepaint.
    let text_layout = self.styled_text.layout().clone();
    let text = self.text.clone();
    let text_id = self.text_id;
    let text_len = text.len();
    let selection_state = self.selection_state.clone();
    let focus_handle = self.focus_handle.clone();
    let on_link = self.on_link.clone();
    let link_ranges = self.link_ranges.clone();

    // Set cursor to pointer if hovering over a link.
    if hitbox.is_hovered(window) {
      let mouse_pos = window.mouse_position();
      if let Ok(index) = text_layout.index_for_position(mouse_pos) {
        let index = clamp_to_char_boundary(text.as_ref(), index.min(text_len));
        if link_ranges.iter().any(|lr| lr.range.contains(&index)) {
          window.set_cursor_style(CursorStyle::PointingHand, hitbox);
        }
      }
    }

    // Mouse-down: set the selection anchor (single click), select word (double), select line (triple).
    let text_for_down = text.clone();
    window.on_mouse_event({
      let hitbox = hitbox.clone();
      let selection_state = selection_state.clone();
      let text_layout = text_layout.clone();
      move |event: &MouseDownEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble
          || event.button != MouseButton::Left
          || !hitbox.is_hovered(window)
        {
          return;
        }

        clear_context_menu(&selection_state);
        if let Some(focus_handle) = &focus_handle {
          focus_handle.focus(window);
        }
        let index = text_layout
          .index_for_position(event.position)
          .unwrap_or_else(|ix| ix);
        let index = clamp_to_char_boundary(text_for_down.as_ref(), index.min(text_len));

        let (anchor, head, mode, initial_range) = match event.click_count {
          2 => {
            let range = word_range_at(text_for_down.as_ref(), index);
            (range.start, range.end, SelectionMode::Word, Some(range))
          }
          3 => {
            let range = line_range_at(text_for_down.as_ref(), index);
            (range.start, range.end, SelectionMode::Line, Some(range))
          }
          _ => (index, index, SelectionMode::Char, None),
        };

        begin_cross_block_selection(
          &selection_state,
          CrossBlockSelection {
            anchor_text_id: text_id,
            anchor,
            head_text_id: text_id,
            head,
            dragging: true,
            mode,
            initial_range,
          },
        );
        window.refresh();
        cx.stop_propagation();
      }
    });

    // Right-click: record the link under the pointer and the window-space menu
    // position. The next render pass draws one lightweight GPUI menu owned by
    // this text block.
    let text_for_context = text.clone();
    window.on_mouse_event({
      let hitbox = hitbox.clone();
      let selection_state = selection_state.clone();
      let text_layout = text_layout.clone();
      let link_ranges = link_ranges.clone();
      move |event: &MouseDownEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble
          || event.button != MouseButton::Right
          || !hitbox.is_hovered(window)
        {
          return;
        }

        let index = text_layout
          .index_for_position(event.position)
          .unwrap_or_else(|ix| ix);
        let index = clamp_to_char_boundary(text_for_context.as_ref(), index.min(text_len));
        let link = link_ranges
          .iter()
          .find(|lr| lr.range.contains(&index))
          .map(|lr| lr.url.clone());
        set_context_menu(
          &selection_state,
          TextContextMenu {
            owner_text_id: text_id,
            position: event.position,
            link,
          },
        );
        window.refresh();
        cx.stop_propagation();
      }
    });

    // Mouse-move: extend the active selection into whichever text block is hovered.
    let text_for_move = text.clone();
    window.on_mouse_event({
      let hitbox = hitbox.clone();
      let selection_state = selection_state.clone();
      let text_layout = text_layout.clone();
      move |event: &MouseMoveEvent, phase, window, _cx| {
        if phase != DispatchPhase::Bubble {
          return;
        }

        let Some(mut active) = active_cross_block_selection(&selection_state) else {
          return;
        };
        if !active.dragging {
          return;
        }

        // Mouse-up can happen outside a text hitbox, so its handler may never
        // finalise the drag. Trust the platform's current button state before
        // extending the selection on a later mouse move.
        if event.pressed_button != Some(MouseButton::Left) {
          active.dragging = false;
          set_cross_block_selection(&selection_state, active);
          return;
        }

        if !hitbox.is_hovered(window) {
          return;
        }

        let index = text_layout
          .index_for_position(event.position)
          .unwrap_or_else(|ix| ix);
        let index = clamp_to_char_boundary(text_for_move.as_ref(), index.min(text_len));
        let updated = selection_with_head(
          active,
          text_id,
          index,
          text_for_move.as_ref(),
          true,
        );
        set_cross_block_selection(&selection_state, updated);
        window.refresh();
      }
    });

    // Mouse-up: finalise the selection in the text block under the pointer.
    let text_for_up = text.clone();
    window.on_mouse_event({
      let hitbox = hitbox.clone();
      let selection_state = selection_state.clone();
      let text_layout = text_layout.clone();
      move |event: &MouseUpEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
          return;
        }

        let Some(active) = active_cross_block_selection(&selection_state) else {
          return;
        };
        if !active.dragging {
          return;
        }

        let index = text_layout
          .index_for_position(event.position)
          .unwrap_or_else(|ix| ix);
        let index = clamp_to_char_boundary(text_for_up.as_ref(), index.min(text_len));
        let updated = selection_with_head(active, text_id, index, text_for_up.as_ref(), false);
        set_cross_block_selection(&selection_state, updated);

        // A click on a link still activates it, but a real selection is left
        // highlighted and does not overwrite the clipboard automatically.
        if cross_block_selected_text(&selection_state).is_none()
          && let Some(link_url) = link_ranges
            .iter()
            .find(|lr| lr.range.contains(&index))
            .map(|lr| lr.url.clone())
        {
          if let Some(handler) = &on_link {
            handler(&link_url, window, cx);
          } else {
            cx.open_url(&link_url);
          }
        }

        window.refresh();
      }
    });

    // Paint the styled text.
    self
      .styled_text
      .paint(None, inspector_id, bounds, &mut (), &mut (), window, cx);
  }
}

fn selection_state_key(selection_state: &SelectionState) -> usize {
  Arc::as_ptr(&selection_state.selection) as usize
}

fn register_cross_block_text(selection_state: &SelectionState, text_id: usize, text: &str) {
  let key = selection_state_key(selection_state);
  let mut all = CROSS_BLOCK_SELECTIONS.lock().unwrap();
  let state = all.entry(key).or_default();
  // Text IDs restart at zero on every render pass. Rebuild only the text registry;
  // the logical selection and context menu can stay alive across repaints.
  if text_id == 0 {
    state.texts.clear();
  }
  state.texts.insert(text_id, text.to_string());
}

fn begin_cross_block_selection(selection_state: &SelectionState, selection: CrossBlockSelection) {
  set_cross_block_selection(selection_state, selection);
}

fn set_cross_block_selection(selection_state: &SelectionState, selection: CrossBlockSelection) {
  let key = selection_state_key(selection_state);
  CROSS_BLOCK_SELECTIONS
    .lock()
    .unwrap()
    .entry(key)
    .or_default()
    .selection = Some(selection);
}

fn set_context_menu(selection_state: &SelectionState, menu: TextContextMenu) {
  let key = selection_state_key(selection_state);
  CROSS_BLOCK_SELECTIONS
    .lock()
    .unwrap()
    .entry(key)
    .or_default()
    .context_menu = Some(menu);
}

fn clear_context_menu(selection_state: &SelectionState) {
  let key = selection_state_key(selection_state);
  if let Some(state) = CROSS_BLOCK_SELECTIONS.lock().unwrap().get_mut(&key) {
    state.context_menu = None;
  }
}

fn context_menu_for(
  selection_state: &SelectionState,
  text_id: usize,
) -> Option<TextContextMenu> {
  let key = selection_state_key(selection_state);
  CROSS_BLOCK_SELECTIONS
    .lock()
    .unwrap()
    .get(&key)
    .and_then(|state| state.context_menu.clone())
    .filter(|menu| menu.owner_text_id == text_id)
}

fn active_cross_block_selection(selection_state: &SelectionState) -> Option<CrossBlockSelection> {
  let key = selection_state_key(selection_state);
  CROSS_BLOCK_SELECTIONS
    .lock()
    .unwrap()
    .get(&key)
    .and_then(|state| state.selection.clone())
}

fn selection_with_head(
  mut active: CrossBlockSelection,
  current_text_id: usize,
  index: usize,
  current_text: &str,
  dragging: bool,
) -> CrossBlockSelection {
  match active.mode {
    SelectionMode::Word => {
      let initial = active
        .initial_range
        .as_ref()
        .expect("word selection needs initial range");
      let current = word_range_at(current_text, index);
      let before_anchor = current_text_id < active.anchor_text_id
        || (current_text_id == active.anchor_text_id && index < initial.start);
      active.anchor = if before_anchor { initial.end } else { initial.start };
      active.head = if before_anchor { current.start } else { current.end };
    }
    SelectionMode::Line => {
      let initial = active
        .initial_range
        .as_ref()
        .expect("line selection needs initial range");
      let current = line_range_at(current_text, index);
      let before_anchor = current_text_id < active.anchor_text_id
        || (current_text_id == active.anchor_text_id && index < initial.start);
      active.anchor = if before_anchor { initial.end } else { initial.start };
      active.head = if before_anchor { current.start } else { current.end };
    }
    SelectionMode::Char => {
      active.head = index;
    }
  }
  active.head_text_id = current_text_id;
  active.dragging = dragging;
  active
}

fn cross_block_selection_range(
  selection_state: &SelectionState,
  text_id: usize,
  text: &str,
) -> Option<Range<usize>> {
  let active = active_cross_block_selection(selection_state)?;
  local_cross_block_range(&active, text_id, text)
}

fn local_cross_block_range(
  active: &CrossBlockSelection,
  text_id: usize,
  text: &str,
) -> Option<Range<usize>> {
  let anchor_point = (active.anchor_text_id, active.anchor);
  let head_point = (active.head_text_id, active.head);
  if anchor_point == head_point {
    return None;
  }

  let ((start_id, start_offset), (end_id, end_offset)) = if anchor_point <= head_point {
    (anchor_point, head_point)
  } else {
    (head_point, anchor_point)
  };
  if text_id < start_id || text_id > end_id {
    return None;
  }

  let text_len = text.len();
  let (start, end) = if start_id == end_id {
    (
      clamp_to_char_boundary(text, start_offset.min(text_len)),
      clamp_to_char_boundary(text, end_offset.min(text_len)),
    )
  } else if text_id == start_id {
    (clamp_to_char_boundary(text, start_offset.min(text_len)), text_len)
  } else if text_id == end_id {
    (0, clamp_to_char_boundary(text, end_offset.min(text_len)))
  } else {
    (0, text_len)
  };

  (start < end).then_some(start..end)
}

fn cross_block_selected_text(selection_state: &SelectionState) -> Option<String> {
  let key = selection_state_key(selection_state);
  let all = CROSS_BLOCK_SELECTIONS.lock().unwrap();
  let state = all.get(&key)?;
  let active = state.selection.as_ref()?;
  if active.anchor_text_id == active.head_text_id && active.anchor == active.head {
    return None;
  }

  let start_id = active.anchor_text_id.min(active.head_text_id);
  let end_id = active.anchor_text_id.max(active.head_text_id);
  let mut parts = Vec::new();
  for text_id in start_id..=end_id {
    let Some(text) = state.texts.get(&text_id) else {
      continue;
    };
    if let Some(range) = local_cross_block_range(active, text_id, text)
      && let Some(slice) = text.get(range)
    {
      parts.push(slice.to_string());
    }
  }

  if parts.is_empty() { None } else { Some(parts.join("\n")) }
}

/// Returns non-overlapping ASCII-case-insensitive matches at UTF-8 boundaries.
fn find_matches_case_insensitive(text: &str, query: &str) -> Vec<Range<usize>> {
  if query.is_empty() || query.len() > text.len() {
    return Vec::new();
  }

  let query = query
    .as_bytes()
    .iter()
    .map(u8::to_ascii_lowercase)
    .collect::<Vec<_>>();
  let bytes = text.as_bytes();
  let mut matches = Vec::new();
  let mut start = 0;

  while start + query.len() <= bytes.len() {
    let end = start + query.len();
    if text.is_char_boundary(start)
      && text.is_char_boundary(end)
      && bytes[start..end]
        .iter()
        .map(u8::to_ascii_lowercase)
        .eq(query.iter().copied())
    {
      matches.push(start..end);
      start = end;
    } else {
      start += 1;
    }
  }

  matches
}

fn preview_search_match_count(selection_state: &SelectionState, query: &str) -> usize {
  let key = selection_state_key(selection_state);
  CROSS_BLOCK_SELECTIONS
    .lock()
    .unwrap()
    .get(&key)
    .map(|state| {
      state
        .texts
        .values()
        .map(|text| find_matches_case_insensitive(text, query).len())
        .sum()
    })
    .unwrap_or_default()
}

fn select_all_cross_block(selection_state: &SelectionState) {
  let key = selection_state_key(selection_state);
  let mut all = CROSS_BLOCK_SELECTIONS.lock().unwrap();
  let Some(state) = all.get_mut(&key) else {
    return;
  };
  let Some(first_id) = state.texts.keys().copied().min() else {
    return;
  };
  let Some(last_id) = state.texts.keys().copied().max() else {
    return;
  };
  let last_len = state.texts.get(&last_id).map_or(0, String::len);

  state.selection = Some(CrossBlockSelection {
    anchor_text_id: first_id,
    anchor: 0,
    head_text_id: last_id,
    head: last_len,
    dragging: false,
    mode: SelectionMode::Char,
    initial_range: None,
  });
}

fn context_menu_item<F>(
  id: &'static str,
  label: &'static str,
  disabled: bool,
  theme: ContextMenuTheme,
  on_click: F,
) -> AnyElement
where
  F: Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
{
  div()
    .id(id)
    .w_full()
    .h(px(30.))
    .px(px(10.))
    .flex()
    .items_center()
    .rounded(px(5.))
    .text_sm()
    .text_color(if disabled {
      theme.muted_foreground
    } else {
      theme.foreground
    })
    .when(!disabled, |this| {
      this
        .cursor_pointer()
        .hover(move |style| style.bg(theme.hover))
        .on_mouse_down(MouseButton::Left, on_click)
    })
    .child(label)
    .into_any_element()
}

fn build_context_menu(
  context: TextContextMenu,
  selection_state: SelectionState,
  on_link: Option<Arc<LinkHandlerFn>>,
  theme: ContextMenuTheme,
) -> AnyElement {
  let selected = cross_block_selected_text(&selection_state);
  let copy_disabled = selected.as_ref().is_none_or(|text| text.is_empty());

  let copy_state = selection_state.clone();
  let copy_item = context_menu_item(
    "preview-context-copy",
    "Copy",
    copy_disabled,
    theme,
    move |_, window, cx| {
      if let Some(text) = cross_block_selected_text(&copy_state)
        && !text.is_empty()
      {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
      }
      clear_context_menu(&copy_state);
      window.refresh();
      cx.stop_propagation();
    },
  );

  let select_all_state = selection_state.clone();
  let select_all_item = context_menu_item(
    "preview-context-select-all",
    "Select All",
    false,
    theme,
    move |_, window, cx| {
      select_all_cross_block(&select_all_state);
      clear_context_menu(&select_all_state);
      window.refresh();
      cx.stop_propagation();
    },
  );

  let mut menu = div()
    .id("preview-context-menu")
    .w(px(180.))
    .p(px(4.))
    .rounded(px(8.))
    .bg(theme.background)
    .border_1()
    .border_color(theme.border)
    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
    .child(copy_item)
    .child(select_all_item);

  if let Some(link_url) = context.link {
    menu = menu.child(
      div()
        .h(px(1.))
        .mx(px(6.))
        .my(px(4.))
        .bg(theme.border),
    );

    let open_url = link_url.clone();
    let open_handler = on_link.clone();
    let open_state = selection_state.clone();
    menu = menu.child(context_menu_item(
      "preview-context-open-link",
      "Open Link",
      false,
      theme,
      move |_, window, cx| {
        if let Some(handler) = &open_handler {
          handler(&open_url, window, cx);
        } else {
          cx.open_url(&open_url);
        }
        clear_context_menu(&open_state);
        window.refresh();
        cx.stop_propagation();
      },
    ));

    let copy_link_state = selection_state.clone();
    menu = menu.child(context_menu_item(
      "preview-context-copy-link",
      "Copy Link",
      false,
      theme,
      move |_, window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(link_url.clone()));
        clear_context_menu(&copy_link_state);
        window.refresh();
        cx.stop_propagation();
      },
    ));
  }

  deferred(
    anchored()
      .position(context.position)
      .snap_to_window_with_margin(px(8.))
      .child(menu),
  )
  .with_priority(2)
  .into_any_element()
}

impl IntoElement for SelectableTextElement {
  type Element = Self;

  fn into_element(self) -> Self::Element {
    self
  }
}

impl IntoElement for SelectableText {
  type Element = AnyElement;

  fn into_element(self) -> Self::Element {
    let text_id = self.inner.text_id;
    let selection_state = self.inner.selection_state.clone();
    let on_link = self.inner.on_link.clone();
    let menu_context = context_menu_for(&selection_state, text_id);

    let mut wrapper = div()
      .id(("preview-selectable-text", text_id))
      .w_full()
      .child(self.inner);

    if let Some(context) = menu_context {
      wrapper = wrapper.child(build_context_menu(
        context,
        selection_state,
        on_link,
        self.menu_theme,
      ));
    }

    wrapper.into_any_element()
  }
}

impl MarkdownRenderOptions {
  /// Selected preview text across all registered Markdown text blocks.
  pub fn selected_preview_text(&self) -> Option<String> {
    cross_block_selected_text(&self.selection_state)
  }

  /// Select every registered preview text block.
  pub fn select_all_preview_text(&self) {
    select_all_cross_block(&self.selection_state);
  }

  /// Returns the number of highlighted matches in the current rendered preview.
  pub fn preview_search_match_count(&self) -> usize {
    self
      .search_query
      .as_deref()
      .map(|query| preview_search_match_count(&self.selection_state, query))
      .unwrap_or_default()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn select_all_spans_registered_blocks() {
    let state = SelectionState::default();
    register_cross_block_text(&state, 0, "first");
    register_cross_block_text(&state, 1, "second");
    select_all_cross_block(&state);
    assert_eq!(cross_block_selected_text(&state).as_deref(), Some("first\nsecond"));
  }

  #[test]
  fn preview_selection_api_returns_all_registered_text() {
    let options = MarkdownRenderOptions::default();
    register_cross_block_text(&options.selection_state, 0, "first");
    register_cross_block_text(&options.selection_state, 1, "second");

    options.select_all_preview_text();

    assert_eq!(
      options.selected_preview_text().as_deref(),
      Some("first\nsecond")
    );
  }

  #[test]
  fn search_matches_ignore_ascii_case_and_preserve_utf8_boundaries() {
    assert_eq!(
      find_matches_case_insensitive("部署 Deploy deploy", "deploy"),
      vec![7..13, 14..20]
    );
  }
}
