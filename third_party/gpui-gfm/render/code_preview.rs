//! Code reference preview card rendering.
//!
//! Renders a [`GithubCodeReferencePreview`] as a bordered card with a header
//! (file label + line range) and a scrollable code snippet body.

use gpui::{
  AnyElement, App, Font, FontStyle, FontWeight, MouseButton, SharedString, div, prelude::*, px,
};

use crate::github::{GithubCodeReferencePreview, short_github_reference};

use super::MarkdownRenderOptions;
use super::code_block::{CodeBlockText, INDENT_DOT_OPACITY};

/// Render a code reference preview as a card element.
pub fn render_code_reference_card(
  preview: &GithubCodeReferencePreview,
  options: &MarkdownRenderOptions,
  _cx: &App,
) -> AnyElement {
  let theme = options.theme();
  let url = preview.url.clone();
  let on_link = options.on_link.clone();

  // Build labels.
  let file_label = format!("{}/{}", preview.repo, preview.path);
  let line_label = if preview.start_line == preview.end_line {
    format!(
      "Line {} in {}",
      preview.start_line,
      short_github_reference(&preview.reference)
    )
  } else {
    format!(
      "Lines {}-{} in {}",
      preview.start_line,
      preview.end_line,
      short_github_reference(&preview.reference)
    )
  };

  // Build snippet rows.
  let code_font = Font {
    family: theme.code_font_family.clone(),
    weight: FontWeight::NORMAL,
    style: FontStyle::Normal,
    ..Default::default()
  };

  // Build snippet body: line-number gutter + code text (with optional indentation dots).
  let code_text: SharedString = preview.snippets.join("\n").into();

  // Line number gutter.
  let mut gutter = div()
    .flex()
    .flex_col()
    .flex_shrink_0()
    .gap(px(2.0))
    .min_w(px(28.0));

  if preview.snippets.is_empty() {
    gutter = gutter.child(
      div()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.muted_foreground)
        .child(preview.start_line.to_string()),
    );
  } else {
    for (offset, _) in preview.snippets.iter().enumerate() {
      let line_number = preview.start_line + offset;
      gutter = gutter.child(
        div()
          .flex()
          .justify_end()
          .text_xs()
          .font_weight(FontWeight::MEDIUM)
          .text_color(theme.muted_foreground)
          .child(line_number.to_string()),
      );
    }
  }

  // Code column — uses CodeBlockText for indentation dots when enabled.
  let code_child: gpui::AnyElement = if options.show_indentation_dots {
    let dot_color = theme.muted_foreground.opacity(INDENT_DOT_OPACITY);
    CodeBlockText::new(code_text, dot_color).into_any_element()
  } else {
    div()
      .font(code_font.clone())
      .text_sm()
      .whitespace_nowrap()
      .text_color(theme.foreground)
      .child(code_text)
      .into_any_element()
  };

  let snippet_body = div().flex().gap_2().child(gutter).child(
    div()
      .flex_1()
      .min_w_0()
      .font(code_font)
      .text_sm()
      .whitespace_nowrap()
      .text_color(theme.foreground)
      .child(code_child),
  );

  // Build the card.
  div()
    .my_1()
    .border_1()
    .border_color(theme.border)
    .rounded_md()
    .overflow_hidden()
    // Header: file label + line range, clickable.
    .child(
      div()
        .id("code-preview-header")
        .group("code-preview-header")
        .bg(theme.code_background)
        .border_b_1()
        .border_color(theme.border)
        .px(px(12.0))
        .py(px(6.0))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
          cx.stop_propagation();
          if let Some(handler) = &on_link {
            handler(url.as_ref(), window, cx);
          } else {
            cx.open_url(url.as_ref());
          }
        })
        .child(
          div()
            .flex()
            .flex_col()
            .child(
              div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.link)
                .group_hover("code-preview-header", |s| s.underline())
                .child(file_label),
            )
            .child(
              div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(line_label),
            ),
        ),
    )
    // Body: scrollable code snippet.
    .child(
      div()
        .id("code-preview-body")
        .w_full()
        .min_w_0()
        .max_h(px(400.0))
        .overflow_scroll()
        .px(px(12.0))
        .pt(px(8.0))
        .pb(px(8.0))
        .bg(theme.background)
        .child(snippet_body),
    )
    .into_any_element()
}
