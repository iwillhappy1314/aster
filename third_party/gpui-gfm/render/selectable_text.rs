//! Selectable text element — wraps `StyledText` and adds click-drag text selection.
//!
//! When the user drags across the text the selected range is highlighted and
//! copied to the system clipboard on mouse-up.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, LazyLock, Mutex};

use gpui::{
  App, ClipboardItem, CursorStyle, DispatchPhase, Element, ElementId, GlobalElementId, Hitbox,
  HitboxBehavior, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent,
  MouseMoveEvent, MouseUpEvent, SharedString, StyledText, TextRun, Window,
};

use super::{
  LinkHandlerFn, SelectionMode, SelectionState, apply_selection_to_runs, clamp_to_char_boundary,
  line_range_at, word_range_at,
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

#[derive(Default)]
struct CrossBlockState {
  selection: Option<CrossBlockSelection>,
  texts: HashMap<usize, String>,
}

static CROSS_BLOCK_SELECTIONS: LazyLock<Mutex<HashMap<usize, CrossBlockState>>> =
  LazyLock::new(|| Mutex::new(HashMap::new()));

/// A text element that supports click-drag selection and clipboard copy.
///
/// Delegates layout and painting to [`StyledText`] but intercepts mouse events
/// in the `paint` phase to track selection state.
pub struct SelectableText {
  /// The text content.
  text: SharedString,
  /// Styled text runs (without selection highlight).
  base_runs: Vec<TextRun>,
  /// Clickable link ranges within the text.
  link_ranges: Vec<LinkRange>,
  /// Shared selection state (across all text blocks in the document).
  selection_state: SelectionState,
  /// Link click handler.
  on_link: Option<Arc<LinkHandlerFn>>,
  /// Unique ID for this text block within the current render pass.
  text_id: usize,
  /// The inner `StyledText` used for layout & painting.
  styled_text: StyledText,
  /// Last selection range applied (used to avoid re-building runs).
  last_selection: Option<Range<usize>>,
}

impl SelectableText {
  pub fn new(
    text: SharedString,
    base_runs: Vec<TextRun>,
    link_ranges: Vec<LinkRange>,
    selection_state: SelectionState,
    on_link: Option<Arc<LinkHandlerFn>>,
    text_id: usize,
  ) -> Self {
    register_cross_block_text(&selection_state, text_id, text.as_ref());
    let styled_text = StyledText::new(text.clone()).with_runs(base_runs.clone());
    Self {
      text,
      base_runs,
      link_ranges,
      selection_state,
      on_link,
      text_id,
      styled_text,
      last_selection: None,
    }
  }

  /// Rebuild the styled text runs if the selection changed.
  fn ensure_runs_up_to_date(&mut self) {
    let selection = cross_block_selection_range(
      &self.selection_state,
      self.text_id,
      self.text.as_ref(),
    );

    if selection == self.last_selection {
      return;
    }

    let runs = if let Some(ref sel) = selection {
      apply_selection_to_runs(
        self.base_runs.clone(),
        sel.clone(),
        self.selection_state.selection_color(),
      )
    } else {
      self.base_runs.clone()
    };

    self.styled_text = StyledText::new(self.text.clone()).with_runs(runs);
    self.last_selection = selection;
  }
}

impl Element for SelectableText {
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

    // Mouse-move: extend the active selection into whichever text block is hovered.
    let text_for_move = text.clone();
    window.on_mouse_event({
      let hitbox = hitbox.clone();
      let selection_state = selection_state.clone();
      let text_layout = text_layout.clone();
      move |event: &MouseMoveEvent, phase, window, _cx| {
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

        if let Some(selected) = cross_block_selected_text(&selection_state) {
          if !selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(selected));
          }
        } else if let Some(link_url) = link_ranges
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
  // the logical selection can stay alive across repaints.
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
    if let Some(range) = local_cross_block_range(active, text_id, text) {
      if let Some(slice) = text.get(range) {
        parts.push(slice.to_string());
      }
    }
  }

  if parts.is_empty() { None } else { Some(parts.join("\n")) }
}

impl IntoElement for SelectableText {
  type Element = Self;

  fn into_element(self) -> Self::Element {
    self
  }
}
