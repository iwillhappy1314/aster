//! Image rendering — inline badges and block-level images.

use gpui::{AnyElement, App, Hsla, ObjectFit, SharedString, StyledImage, div, img, prelude::*, px};

use crate::types::*;

use super::MarkdownRenderOptions;
use super::inline::resolve_url;

/// Maximum height for inline images (badges, shields, etc.) in pixels.
const INLINE_IMAGE_HEIGHT_PX: f32 = 18.0;

/// Maximum height for block images before they start scrolling.
const BLOCK_IMAGE_MAX_HEIGHT_PX: f32 = 500.0;

// ---------------------------------------------------------------------------
// Dimension parsing
// ---------------------------------------------------------------------------

/// A parsed image dimension.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ImageDimension {
  /// Absolute pixel value.
  Pixels(f32),
  /// Fraction of parent (e.g. 0.85 for 85%).
  Fraction(f32),
}

/// Parse a dimension hint like `"200px"`, `"200"`, or `"85%"`.
///
/// Returns `None` for empty, missing, or unparseable values.
pub fn parse_image_dimension(hint: Option<&str>) -> Option<ImageDimension> {
  let hint = hint.map(str::trim).filter(|h| !h.is_empty())?;
  let lower = hint.to_ascii_lowercase();

  // Percentage: "85%"
  if let Some(pct) = lower.strip_suffix('%') {
    if let Ok(value) = pct.trim().parse::<f32>() {
      if value.is_finite() && value > 0.0 {
        return Some(ImageDimension::Fraction((value / 100.0).min(1.0)));
      }
    }
    return None;
  }

  // Pixels: "200px" or plain "200"
  let px_str = lower.strip_suffix("px").unwrap_or(&lower).trim();
  if let Ok(value) = px_str.parse::<f32>() {
    if value.is_finite() && value > 0.0 {
      return Some(ImageDimension::Pixels(value));
    }
  }

  None
}

// ---------------------------------------------------------------------------
// Theme-aware URL selection
// ---------------------------------------------------------------------------

/// Select the appropriate image URL based on the current theme.
///
/// When `is_dark` is true and `dark_url` is available, use `dark_url`.
/// When `is_dark` is false and `light_url` is available, use `light_url`.
/// Otherwise fall back to the main `url`.
pub fn select_image_url<'a>(
  url: &'a str,
  dark_url: Option<&'a str>,
  light_url: Option<&'a str>,
  is_dark: bool,
) -> &'a str {
  let themed = if is_dark { dark_url } else { light_url };
  themed
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .unwrap_or(url)
}

// ---------------------------------------------------------------------------
// Block image detection
// ---------------------------------------------------------------------------

/// Check if a paragraph contains only a single image (possibly wrapped in a link).
///
/// Such paragraphs are rendered as full-width block images instead of inline.
pub fn is_block_image(inlines: &[Inline]) -> bool {
  if inlines.len() != 1 {
    return false;
  }
  match &inlines[0] {
    Inline::Image { .. } => true,
    Inline::Link { content, .. } => {
      content.len() == 1 && matches!(&content[0], Inline::Image { .. })
    }
    _ => false,
  }
}

/// Extract the image data from a single-image paragraph.
///
/// Returns `(url, alt, width, height, dark_url, light_url, optional_link_url)`.
fn extract_block_image(inlines: &[Inline]) -> Option<BlockImageData<'_>> {
  if inlines.len() != 1 {
    return None;
  }
  match &inlines[0] {
    Inline::Image {
      url,
      alt,
      width,
      height,
      dark_url,
      light_url,
      ..
    } => Some(BlockImageData {
      url,
      alt,
      width: width.as_deref(),
      height: height.as_deref(),
      dark_url: dark_url.as_deref(),
      light_url: light_url.as_deref(),
      link_url: None,
    }),
    Inline::Link {
      url: link_url,
      content,
      ..
    } => {
      if content.len() != 1 {
        return None;
      }
      if let Inline::Image {
        url,
        alt,
        width,
        height,
        dark_url,
        light_url,
        ..
      } = &content[0]
      {
        Some(BlockImageData {
          url,
          alt,
          width: width.as_deref(),
          height: height.as_deref(),
          dark_url: dark_url.as_deref(),
          light_url: light_url.as_deref(),
          link_url: Some(link_url.as_str()),
        })
      } else {
        None
      }
    }
    _ => None,
  }
}

struct BlockImageData<'a> {
  url: &'a str,
  alt: &'a str,
  width: Option<&'a str>,
  height: Option<&'a str>,
  dark_url: Option<&'a str>,
  light_url: Option<&'a str>,
  link_url: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render a block-level image (full-width paragraph that is just one image).
pub fn render_block_image(
  inlines: &[Inline],
  options: &MarkdownRenderOptions,
  _cx: &App,
) -> AnyElement {
  let data = match extract_block_image(inlines) {
    Some(d) => d,
    None => return div().into_any_element(),
  };

  let theme = options.theme();
  let themed_url = select_image_url(data.url, data.dark_url, data.light_url, theme.is_dark);
  let resolved_url = resolve_url(themed_url, options);

  let label = if data.alt.trim().is_empty() {
    "image".to_string()
  } else {
    data.alt.trim().to_string()
  };

  let mut image = build_image(&resolved_url, options)
    .max_h(px(BLOCK_IMAGE_MAX_HEIGHT_PX))
    .object_fit(ObjectFit::Contain);

  // Apply explicit dimensions.
  image = apply_dimensions(image, data.width, data.height);

  // Loading/fallback placeholders.
  let loading_label = label.clone();
  let image = image
    .with_loading(move || render_image_placeholder(&loading_label))
    .with_fallback(move || render_image_placeholder(&label));

  // Optionally wrap in a clickable link.
  if let Some(link_url) = data.link_url {
    let resolved_link = resolve_url(link_url, options);
    let on_link = options.on_link.clone();
    let link_id: SharedString = "gfm-block-img-link".into();

    let mut wrapper = div().id(link_id).cursor_pointer().child(image);

    if let Some(handler) = on_link {
      let url = resolved_link.clone();
      wrapper = wrapper.on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
        handler(&url, window, cx);
      });
    } else {
      wrapper = wrapper.on_mouse_down(gpui::MouseButton::Left, move |_, _window, cx| {
        cx.open_url(&resolved_link);
      });
    }

    wrapper.into_any_element()
  } else {
    image.into_any_element()
  }
}

/// Render an inline image (badge/shield style — fixed 18px height).
pub fn render_inline_image(
  url: &str,
  alt: &str,
  width: Option<&str>,
  height: Option<&str>,
  dark_url: Option<&str>,
  light_url: Option<&str>,
  options: &MarkdownRenderOptions,
) -> AnyElement {
  let theme = options.theme();
  let themed_url = select_image_url(url, dark_url, light_url, theme.is_dark);
  let resolved = resolve_url(themed_url, options);

  let label = if alt.trim().is_empty() {
    "image".to_string()
  } else {
    alt.trim().to_string()
  };

  let mut image = build_image(&resolved, options)
    .h(px(INLINE_IMAGE_HEIGHT_PX))
    .object_fit(ObjectFit::Contain);

  // If explicit dimensions are given, override the default inline height.
  image = apply_dimensions(image, width, height);

  let loading_label = label.clone();
  image
    .with_loading(move || render_image_placeholder(&loading_label))
    .with_fallback(move || render_image_placeholder(&label))
    .into_any_element()
}

/// Build a gpui `Img` from a resolved URL, using the custom image loader if set.
fn build_image(resolved_url: &str, options: &MarkdownRenderOptions) -> gpui::Img {
  if let Some(loader) = &options.image_loader {
    let source = loader(resolved_url);
    img(source)
  } else {
    img(resolved_url)
  }
}

/// Apply width/height dimension hints to an image element.
fn apply_dimensions(mut image: gpui::Img, width: Option<&str>, height: Option<&str>) -> gpui::Img {
  if let Some(dim) = parse_image_dimension(width) {
    image = match dim {
      ImageDimension::Pixels(v) => image.w(px(v)),
      ImageDimension::Fraction(v) => image.w(gpui::relative(v)),
    };
  }
  if let Some(dim) = parse_image_dimension(height) {
    image = match dim {
      ImageDimension::Pixels(v) => image.h(px(v)),
      ImageDimension::Fraction(v) => image.h(gpui::relative(v)),
    };
  }
  image
}

/// Render a compact placeholder badge for loading/error states.
pub fn render_image_placeholder(label: &str) -> AnyElement {
  let text = label.trim();
  let text = if text.is_empty() { "image" } else { text };

  div()
    .h(px(INLINE_IMAGE_HEIGHT_PX))
    .px_2()
    .rounded_sm()
    .flex()
    .items_center()
    .bg(Hsla {
      h: 220.0 / 360.0,
      s: 0.18,
      l: 0.58,
      a: 1.0,
    })
    .text_xs()
    .text_color(Hsla {
      h: 0.0,
      s: 0.0,
      l: 1.0,
      a: 1.0,
    })
    .child(text.to_string())
    .into_any_element()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
  use super::*;

  // --- parse_image_dimension ---

  #[test]
  fn parse_pixels_with_suffix() {
    assert_eq!(
      parse_image_dimension(Some("200px")),
      Some(ImageDimension::Pixels(200.0))
    );
  }

  #[test]
  fn parse_pixels_without_suffix() {
    assert_eq!(
      parse_image_dimension(Some("150")),
      Some(ImageDimension::Pixels(150.0))
    );
  }

  #[test]
  fn parse_percentage() {
    assert_eq!(
      parse_image_dimension(Some("85%")),
      Some(ImageDimension::Fraction(0.85))
    );
  }

  #[test]
  fn parse_percentage_clamped() {
    assert_eq!(
      parse_image_dimension(Some("150%")),
      Some(ImageDimension::Fraction(1.0))
    );
  }

  #[test]
  fn parse_empty_returns_none() {
    assert_eq!(parse_image_dimension(Some("")), None);
    assert_eq!(parse_image_dimension(None), None);
  }

  #[test]
  fn parse_invalid_returns_none() {
    assert_eq!(parse_image_dimension(Some("abc")), None);
    assert_eq!(parse_image_dimension(Some("-10px")), None);
    assert_eq!(parse_image_dimension(Some("0px")), None);
  }

  #[test]
  fn parse_whitespace_trimmed() {
    assert_eq!(
      parse_image_dimension(Some("  42px  ")),
      Some(ImageDimension::Pixels(42.0))
    );
  }

  // --- select_image_url ---

  #[test]
  fn select_dark_url_when_dark() {
    let url = select_image_url("default.png", Some("dark.png"), Some("light.png"), true);
    assert_eq!(url, "dark.png");
  }

  #[test]
  fn select_light_url_when_light() {
    let url = select_image_url("default.png", Some("dark.png"), Some("light.png"), false);
    assert_eq!(url, "light.png");
  }

  #[test]
  fn fallback_to_main_url() {
    let url = select_image_url("default.png", None, None, true);
    assert_eq!(url, "default.png");
  }

  #[test]
  fn empty_themed_url_falls_back() {
    let url = select_image_url("default.png", Some(""), None, true);
    assert_eq!(url, "default.png");
  }

  // --- is_block_image ---

  #[test]
  fn single_image_is_block() {
    let inlines = vec![Inline::Image {
      url: "x.png".into(),
      title: None,
      alt: "x".into(),
      width: None,
      height: None,
      dark_url: None,
      light_url: None,
    }];
    assert!(is_block_image(&inlines));
  }

  #[test]
  fn linked_image_is_block() {
    let inlines = vec![Inline::Link {
      url: "https://example.com".into(),
      title: None,
      content: vec![Inline::Image {
        url: "x.png".into(),
        title: None,
        alt: "x".into(),
        width: None,
        height: None,
        dark_url: None,
        light_url: None,
      }],
    }];
    assert!(is_block_image(&inlines));
  }

  #[test]
  fn text_plus_image_not_block() {
    let inlines = vec![
      Inline::Text("hello ".into()),
      Inline::Image {
        url: "x.png".into(),
        title: None,
        alt: "x".into(),
        width: None,
        height: None,
        dark_url: None,
        light_url: None,
      },
    ];
    assert!(!is_block_image(&inlines));
  }

  #[test]
  fn plain_text_not_block_image() {
    let inlines = vec![Inline::Text("hello".into())];
    assert!(!is_block_image(&inlines));
  }
}
