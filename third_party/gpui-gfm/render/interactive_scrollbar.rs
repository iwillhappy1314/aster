//! Reusable custom scrollbar rendering and window-level drag interaction.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
  AnyElement, App, Bounds, Hsla, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
  ScrollDelta, ScrollHandle, ScrollWheelEvent, Window, canvas, fill, point, prelude::*, px, size,
};

const SCROLLBAR_TRACK_SIZE_PX: f32 = 10.0;
const SCROLLBAR_THUMB_SIZE_PX: f32 = 6.0;
const SCROLLBAR_EDGE_INSET_PX: f32 = 2.0;
const SCROLLBAR_MIN_THUMB_LENGTH_PX: f32 = 48.0;

/// Axis controlled by an interactive scrollbar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InteractiveScrollbarAxis {
  /// Scrolls content from left to right.
  Horizontal,
  /// Scrolls content from top to bottom.
  Vertical,
}

/// Persistent hover and drag state for one custom scrollbar.
#[derive(Clone, Default)]
pub struct InteractiveScrollbarState {
  inner: Rc<Cell<InteractiveScrollbarStateInner>>,
}

impl InteractiveScrollbarState {
  /// Returns whether the scrollbar thumb is currently being dragged.
  pub fn is_dragging(&self) -> bool {
    self.inner.get().drag.is_some()
  }
}

#[derive(Clone, Copy, Default)]
struct InteractiveScrollbarStateInner {
  hovered: bool,
  drag: Option<ScrollbarDrag>,
}

#[derive(Clone, Copy)]
struct ScrollbarDrag {
  pointer_in_thumb: Pixels,
}

#[derive(Clone, Copy)]
struct ScrollbarGeometry {
  track: Bounds<Pixels>,
  thumb_hit: Bounds<Pixels>,
  thumb_fill: Bounds<Pixels>,
  thumb_length: Pixels,
  max_offset: Pixels,
}

/// Renders a 6px auto-hidden scrollbar backed by a GPUI [`ScrollHandle`].
pub fn render_interactive_scrollbar(
  axis: InteractiveScrollbarAxis,
  state: InteractiveScrollbarState,
  scroll_handle: ScrollHandle,
  color: Hsla,
) -> AnyElement {
  let paint_state = state.clone();
  let down_state = state.clone();
  let move_state = state.clone();
  let up_state = state;
  let paint_handle = scroll_handle.clone();
  let down_handle = scroll_handle.clone();
  let move_handle = scroll_handle;

  canvas(
    move |_, _, _| {},
    move |bounds: Bounds<Pixels>, (), window: &mut Window, _cx: &mut App| {
      let paint_geometry = scrollbar_geometry(axis, bounds, &paint_handle);
      let view_id = window.current_view();

      window.on_mouse_event({
        let state = down_state.clone();
        let handle = down_handle.clone();
        move |event: &MouseDownEvent, phase, _, cx| {
          if !phase.bubble() {
            return;
          }
          let Some(geometry) = scrollbar_geometry(axis, bounds, &handle) else {
            return;
          };
          if !geometry.track.contains(&event.position) {
            return;
          }

          cx.stop_propagation();
          if geometry.thumb_hit.contains(&event.position) {
            let pointer_in_thumb = axis_coordinate(axis, event.position)
              - axis_origin(axis, geometry.thumb_hit);
            let mut inner = state.inner.get();
            inner.drag = Some(ScrollbarDrag { pointer_in_thumb });
            state.inner.set(inner);
            cx.notify(view_id);
          } else {
            set_scroll_offset_for_pointer(
              axis,
              &handle,
              geometry,
              axis_coordinate(axis, event.position),
              geometry.thumb_length / 2.,
            );
            cx.notify(view_id);
          }
        }
      });

      window.on_mouse_event({
        let state = move_state.clone();
        let handle = move_handle.clone();
        move |event: &MouseMoveEvent, _, _, cx| {
          let mut inner = state.inner.get();
          let hovered = bounds.contains(&event.position);
          if inner.hovered != hovered {
            inner.hovered = hovered;
            state.inner.set(inner);
            cx.notify(view_id);
          }

          let Some(drag) = inner.drag else {
            return;
          };
          if !event.dragging() {
            return;
          }
          let Some(geometry) = scrollbar_geometry(axis, bounds, &handle) else {
            return;
          };

          cx.stop_propagation();
          set_scroll_offset_for_pointer(
            axis,
            &handle,
            geometry,
            axis_coordinate(axis, event.position),
            drag.pointer_in_thumb,
          );
          cx.notify(view_id);
        }
      });

      window.on_mouse_event({
        let state = up_state.clone();
        move |_: &MouseUpEvent, phase, _, cx| {
          if !phase.bubble() {
            return;
          }
          let mut inner = state.inner.get();
          if inner.drag.take().is_some() {
            state.inner.set(inner);
            cx.stop_propagation();
            cx.notify(view_id);
          }
        }
      });

      let Some(geometry) = paint_geometry else {
        return;
      };
      let interaction = paint_state.inner.get();
      if !interaction.hovered && interaction.drag.is_none() {
        return;
      }

      window.paint_quad(fill(geometry.thumb_fill, color));
    },
  )
  .absolute()
  .inset_0()
  .into_any_element()
}

/// Stops a horizontal gesture from also reaching the document's vertical scroller.
pub(crate) fn stop_horizontal_scroll_propagation(event: &ScrollWheelEvent, cx: &mut App) {
  let (delta_x, delta_y) = scroll_delta_components(event.delta);
  if is_horizontal_scroll_intent(event.modifiers.shift, delta_x, delta_y) {
    cx.stop_propagation();
  }
}

/// Returns the track and thumb geometry for a scrollable viewport.
fn scrollbar_geometry(
  axis: InteractiveScrollbarAxis,
  bounds: Bounds<Pixels>,
  handle: &ScrollHandle,
) -> Option<ScrollbarGeometry> {
  let viewport_length = axis_size(axis, handle.bounds());
  let max_offset = match axis {
    InteractiveScrollbarAxis::Horizontal => handle.max_offset().width,
    InteractiveScrollbarAxis::Vertical => handle.max_offset().height,
  };
  let track_length = axis_size(axis, bounds);
  if viewport_length <= px(0.) || max_offset <= px(0.) || track_length <= px(0.) {
    return None;
  }

  let content_length = viewport_length + max_offset;
  let thumb_length = (viewport_length / content_length * track_length)
    .max(px(SCROLLBAR_MIN_THUMB_LENGTH_PX))
    .min(track_length);
  let travel = track_length - thumb_length;
  let offset = axis_coordinate(axis, handle.offset());
  let progress = (-offset / max_offset).clamp(0., 1.);
  let thumb_start = travel * progress;

  let track = match axis {
    InteractiveScrollbarAxis::Horizontal => Bounds {
      origin: point(bounds.left(), bounds.bottom() - px(SCROLLBAR_TRACK_SIZE_PX)),
      size: size(bounds.size.width, px(SCROLLBAR_TRACK_SIZE_PX)),
    },
    InteractiveScrollbarAxis::Vertical => Bounds {
      origin: point(bounds.right() - px(SCROLLBAR_TRACK_SIZE_PX), bounds.top()),
      size: size(px(SCROLLBAR_TRACK_SIZE_PX), bounds.size.height),
    },
  };

  let thumb_hit = match axis {
    InteractiveScrollbarAxis::Horizontal => Bounds {
      origin: point(track.left() + thumb_start, track.top()),
      size: size(thumb_length, track.size.height),
    },
    InteractiveScrollbarAxis::Vertical => Bounds {
      origin: point(track.left(), track.top() + thumb_start),
      size: size(track.size.width, thumb_length),
    },
  };
  let thumb_fill = match axis {
    InteractiveScrollbarAxis::Horizontal => Bounds {
      origin: point(
        thumb_hit.left(),
        track.bottom() - px(SCROLLBAR_EDGE_INSET_PX + SCROLLBAR_THUMB_SIZE_PX),
      ),
      size: size(thumb_length, px(SCROLLBAR_THUMB_SIZE_PX)),
    },
    InteractiveScrollbarAxis::Vertical => Bounds {
      origin: point(
        track.right() - px(SCROLLBAR_EDGE_INSET_PX + SCROLLBAR_THUMB_SIZE_PX),
        thumb_hit.top(),
      ),
      size: size(px(SCROLLBAR_THUMB_SIZE_PX), thumb_length),
    },
  };

  Some(ScrollbarGeometry {
    track,
    thumb_hit,
    thumb_fill,
    thumb_length,
    max_offset,
  })
}

/// Maps one pointer coordinate to a clamped negative GPUI scroll offset.
fn set_scroll_offset_for_pointer(
  axis: InteractiveScrollbarAxis,
  handle: &ScrollHandle,
  geometry: ScrollbarGeometry,
  pointer: Pixels,
  pointer_in_thumb: Pixels,
) {
  let travel = axis_size(axis, geometry.track) - geometry.thumb_length;
  if travel <= px(0.) {
    return;
  }
  let progress = ((pointer - pointer_in_thumb - axis_origin(axis, geometry.track)) / travel)
    .clamp(0., 1.);
  let next = -geometry.max_offset * progress;
  let current = handle.offset();
  handle.set_offset(match axis {
    InteractiveScrollbarAxis::Horizontal => point(next, current.y),
    InteractiveScrollbarAxis::Vertical => point(current.x, next),
  });
}

/// Returns the coordinate along the scrollbar's active axis.
fn axis_coordinate(axis: InteractiveScrollbarAxis, point: Point<Pixels>) -> Pixels {
  match axis {
    InteractiveScrollbarAxis::Horizontal => point.x,
    InteractiveScrollbarAxis::Vertical => point.y,
  }
}

/// Returns the origin coordinate along the scrollbar's active axis.
fn axis_origin(axis: InteractiveScrollbarAxis, bounds: Bounds<Pixels>) -> Pixels {
  axis_coordinate(axis, bounds.origin)
}

/// Returns a bounds length along the scrollbar's active axis.
fn axis_size(axis: InteractiveScrollbarAxis, bounds: Bounds<Pixels>) -> Pixels {
  match axis {
    InteractiveScrollbarAxis::Horizontal => bounds.size.width,
    InteractiveScrollbarAxis::Vertical => bounds.size.height,
  }
}

/// Extracts comparable X/Y values without depending on the delta's unit.
fn scroll_delta_components(delta: ScrollDelta) -> (f32, f32) {
  match delta {
    ScrollDelta::Pixels(delta) => (f32::from(delta.x), f32::from(delta.y)),
    ScrollDelta::Lines(delta) => (delta.x, delta.y),
  }
}

/// Returns whether a wheel event belongs to a nested horizontal scroller.
fn is_horizontal_scroll_intent(shift: bool, delta_x: f32, delta_y: f32) -> bool {
  shift || delta_x.abs() > delta_y.abs()
}

#[cfg(test)]
mod tests {
  use super::is_horizontal_scroll_intent;

  #[test]
  fn shift_wheel_is_horizontal_even_when_the_raw_delta_is_vertical() {
    assert!(is_horizontal_scroll_intent(true, 0., 12.));
  }

  #[test]
  fn touchpad_horizontal_delta_is_horizontal_without_shift() {
    assert!(is_horizontal_scroll_intent(false, 12., 2.));
  }

  #[test]
  fn ordinary_vertical_wheel_is_not_intercepted() {
    assert!(!is_horizontal_scroll_intent(false, 0., 12.));
  }
}
