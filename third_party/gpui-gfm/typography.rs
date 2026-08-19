//! Shared Markdown typography tokens used by the preview renderer and host editors.

use gpui::{FontWeight, Rems, rems};

/// Returns the canonical font size for a Markdown heading level.
///
/// These values intentionally mirror GPUI's `text_3xl` through `text_sm`
/// helpers so editor and preview remain identical even if the window rem size changes.
pub fn heading_font_size(level: u8) -> Rems {
  match level {
    1 => rems(1.875),
    2 => rems(1.5),
    3 => rems(1.25),
    4 => rems(1.125),
    5 => rems(1.0),
    _ => rems(0.875),
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
    assert_eq!(heading_font_size(1), rems(1.875));
    assert_eq!(heading_font_size(2), rems(1.5));
    assert_eq!(heading_font_size(3), rems(1.25));
    assert_eq!(heading_font_size(4), rems(1.125));
    assert_eq!(heading_font_size(5), rems(1.0));
    assert_eq!(heading_font_size(6), rems(0.875));
  }
}
