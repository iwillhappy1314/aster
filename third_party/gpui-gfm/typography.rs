//! Shared Markdown typography tokens used by the preview renderer and host editors.

use gpui::FontWeight;

/// The preview renderer's body text size (`text_sm`).
pub const BODY_FONT_SIZE_PX: f32 = 14.0;

/// Returns the canonical font size for a Markdown heading level.
pub fn heading_font_size_px(level: u8) -> f32 {
  match level {
    1 => 30.0,
    2 => 24.0,
    3 => 20.0,
    4 => 18.0,
    5 => 16.0,
    _ => 14.0,
  }
}

/// Returns the canonical font weight for a Markdown heading level.
pub fn heading_font_weight(level: u8) -> FontWeight {
  match level {
    1 => FontWeight::BOLD,
    2 | 3 => FontWeight::SEMIBOLD,
    _ => FontWeight::MEDIUM,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn heading_sizes_match_preview_scale() {
    assert_eq!(heading_font_size_px(1), 30.0);
    assert_eq!(heading_font_size_px(2), 24.0);
    assert_eq!(heading_font_size_px(3), 20.0);
    assert_eq!(heading_font_size_px(4), 18.0);
    assert_eq!(heading_font_size_px(5), 16.0);
    assert_eq!(heading_font_size_px(6), 14.0);
  }
}
