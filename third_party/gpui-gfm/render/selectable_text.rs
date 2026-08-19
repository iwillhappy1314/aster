//! Selectable text element — wraps `StyledText` and adds click-drag text selection.
//!
//! When the user drags across the text the selected range is highlighted and
//! copied to the system clipboard on mouse-up.

use std::ops::Range;
use std::sync::Arc;

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
    let selection = self
      .selection_state
      .selection_range_for(self.text_id, self.text.as_ref());

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

        match event.click_count {
          2 => {
            // Double click → select word.
            let range = word_range_at(text_for_down.as_ref(), index);
            selection_state.update_with_mode(
              text_id,
              range.start,
              range.end,
              true,
              SelectionMode::Word,
              Some(range),
            );
          }
          3 => {
            // Triple click → select line.
            let range = line_range_at(text_for_down.as_ref(), index);
            selection_state.update_with_mode(
              text_id,
              range.start,
              range.end,
              true,
              SelectionMode::Line,
              Some(range),
            );
          }
          _ => {
            // Single click → character-level anchor.
            selection_state.update_with_mode(
              text_id,
              index,
              index,
              true,
              SelectionMode::Char,
              None,
            );
          }
        }

        window.refresh();
        cx.stop_propagation();
      }
    });

    // Mouse-move: extend the selection while dragging.
    // For word/line mode, extend to the full word/line at the cursor position
    // while keeping the original anchor word/line intact.
    let text_for_move = text.clone();
    window.on_mouse_event({
      let selection_state = selection_state.clone();
      let text_layout = text_layout.clone();
      move |event: &MouseMoveEvent, phase, window, _cx| {
        if phase != DispatchPhase::Bubble {
          return;
        }

        if let Some(active) = selection_state.selection_for(text_id) {
          if active.dragging {
            let index = text_layout
              .index_for_position(event.position)
              .unwrap_or_else(|ix| ix);
            let index = clamp_to_char_boundary(text_for_move.as_ref(), index.min(text_len));

            let (new_anchor, new_head) = match active.mode {
              SelectionMode::Word => {
                let initial = active.initial_range.as_ref().unwrap();
                let current_word = word_range_at(text_for_move.as_ref(), index);
                if index < initial.start {
                  // Dragging before initial word → anchor at end of initial, head at start of current.
                  (initial.end, current_word.start)
                } else {
                  // Dragging after or within initial word → anchor at start of initial, head at end of current.
                  (initial.start, current_word.end)
                }
              }
              SelectionMode::Line => {
                let initial = active.initial_range.as_ref().unwrap();
                let current_line = line_range_at(text_for_move.as_ref(), index);
                if index < initial.start {
                  (initial.end, current_line.start)
                } else {
                  (initial.start, current_line.end)
                }
              }
              SelectionMode::Char => (active.anchor, index),
            };

            selection_state.update(text_id, new_anchor, new_head, true);
            window.refresh();
          }
        }
      }
    });

    // Mouse-up: finalise selection → copy to clipboard, or handle link click.
    let text_for_up = text.clone();
    let text_for_copy = text.clone();
    window.on_mouse_event({
      let hitbox = hitbox.clone();
      let selection_state = selection_state.clone();
      let text_layout = text_layout.clone();
      move |event: &MouseUpEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble {
          return;
        }

        let Some(active) = selection_state.selection_for(text_id) else {
          return;
        };
        if !active.dragging {
          return;
        }

        let index = text_layout
          .index_for_position(event.position)
          .unwrap_or_else(|ix| ix);
        let index = clamp_to_char_boundary(text_for_up.as_ref(), index.min(text_len));

        // Snap to word/line boundaries — same logic as mouse-move.
        let (final_anchor, final_head) = match active.mode {
          SelectionMode::Word => {
            let initial = active.initial_range.as_ref().unwrap();
            let current_word = word_range_at(text_for_up.as_ref(), index);
            if index < initial.start {
              (initial.end, current_word.start)
            } else {
              (initial.start, current_word.end)
            }
          }
          SelectionMode::Line => {
            let initial = active.initial_range.as_ref().unwrap();
            let current_line = line_range_at(text_for_up.as_ref(), index);
            if index < initial.start {
              (initial.end, current_line.start)
            } else {
              (initial.start, current_line.end)
            }
          }
          SelectionMode::Char => (active.anchor, index),
        };

        // Stop dragging.
        selection_state.update(text_id, final_anchor, final_head, false);

        // Check if there's a non-empty selection.
        if let Some(selected) = selection_state.selected_text(text_id, text_for_copy.as_ref()) {
          if !selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(selected));
          }
        } else if hitbox.is_hovered(window) {
          // Empty selection (click) → handle link.
          if let Some(link_url) = link_ranges
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

impl IntoElement for SelectableText {
  type Element = Self;

  fn into_element(self) -> Self::Element {
    self
  }
}
