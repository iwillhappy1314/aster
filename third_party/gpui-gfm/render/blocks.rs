//! Block-level rendering.

use gpui::{AnyElement, App, MouseButton, SharedString, div, prelude::*, px};

use crate::types::*;

use super::ListItemView;
use super::MarkdownRenderOptions;
use super::code_block::render_code_block;
use super::image::{is_block_image, render_block_image};
use super::inline::render_inline_text;
use super::table::render_table;

/// Render a list of blocks as a vertical stack.
pub fn render_blocks(
  blocks: &[Block],
  options: &MarkdownRenderOptions,
  indent: usize,
  cx: &App,
) -> AnyElement {
  let mut container = div().flex().flex_col().gap_2();

  for (ix, block) in blocks.iter().enumerate() {
    let block_element = render_block(block, options, indent, cx);
    let block_element = if ix > 0 && matches!(block, Block::Heading { .. }) {
      div().mt(px(10.0)).child(block_element).into_any_element()
    } else {
      block_element
    };
    container = container.child(block_element);
  }

  if indent > 0 {
    container = container.pl(px(12.0 * indent as f32));
  }

  container.into_any_element()
}

/// Render a single block.
fn render_block(
  block: &Block,
  options: &MarkdownRenderOptions,
  indent: usize,
  cx: &App,
) -> AnyElement {
  let theme = options.theme();

  match block {
    Block::Paragraph(inlines) => {
      if is_block_image(inlines) {
        render_block_image(inlines, options, cx)
      } else {
        let el = div()
          .whitespace_normal()
          .text_sm()
          .text_color(theme.foreground)
          .child(render_inline_text(inlines, options, cx))
          .into_any_element();
        if let Some(override_fn) = options.overrides.paragraph.as_ref() {
          override_fn(el, cx)
        } else {
          el
        }
      }
    }

    Block::Heading { level, content } => {
      let el = div().text_color(theme.foreground);
      let el = match level {
        1 => el.text_3xl().font_weight(gpui::FontWeight::BOLD),
        2 => el.text_2xl().font_weight(gpui::FontWeight::SEMIBOLD),
        3 => el.text_xl().font_weight(gpui::FontWeight::SEMIBOLD),
        4 => el.text_lg().font_weight(gpui::FontWeight::MEDIUM),
        5 => el.text_base().font_weight(gpui::FontWeight::MEDIUM),
        _ => el.text_sm().font_weight(gpui::FontWeight::MEDIUM),
      };
      let el = el
        .child(render_inline_text(content, options, cx))
        .into_any_element();
      if let Some(override_fn) = options.overrides.heading.as_ref() {
        override_fn(*level, el, cx)
      } else {
        el
      }
    }

    Block::List(list) => render_list(list, options, indent, cx),

    Block::CodeBlock(code) => {
      if let Some(override_fn) = options.overrides.code_block.as_ref() {
        override_fn(code, cx)
      } else {
        render_code_block(code, options, cx)
      }
    }

    Block::BlockQuote(children) => {
      let el = div()
        .border_l_2()
        .border_color(theme.muted_foreground)
        .pl(px(8.0))
        .child(render_blocks(children, options, indent + 1, cx))
        .into_any_element();
      if let Some(override_fn) = options.overrides.block_quote.as_ref() {
        override_fn(el, cx)
      } else {
        el
      }
    }

    Block::ThematicBreak => {
      if let Some(override_fn) = options.overrides.thematic_break.as_ref() {
        override_fn(cx)
      } else {
        div()
          .h(px(1.0))
          .bg(theme.border)
          .rounded_md()
          .into_any_element()
      }
    }

    Block::Table(table) => {
      if let Some(override_fn) = options.overrides.table.as_ref() {
        override_fn(table, cx)
      } else {
        render_table(table, options, cx)
      }
    }

    Block::Details(details) => render_details(details, options, indent, cx),

    Block::Aligned { center, blocks } => {
      if *center {
        let mut aligned = div().flex().flex_col().w_full().min_w_0().gap_2();
        for block in blocks {
          aligned = aligned.child(
            div()
              .w_full()
              .min_w_0()
              .text_center()
              .child(render_block(block, options, indent, cx)),
          );
        }
        aligned.into_any_element()
      } else {
        render_blocks(blocks, options, indent, cx)
      }
    }
  }
}

/// Render a list.
fn render_list(
  list: &List,
  options: &MarkdownRenderOptions,
  _indent: usize,
  cx: &App,
) -> AnyElement {
  let theme = options.theme();
  let mut container = div()
    .flex()
    .flex_col()
    .w_full()
    .min_w_0()
    .gap_1()
    .pl(px(10.0));
  let start = list.start.unwrap_or(1);

  for (ix, item) in list.items.iter().enumerate() {
    let bullet = if list.ordered {
      format!("{}.", start + ix as u64)
    } else if item.checked == Some(true) {
      "☑".to_string()
    } else if item.checked == Some(false) {
      "☐".to_string()
    } else {
      "•".to_string()
    };

    let item_content = render_list_item_blocks(&item.blocks, options, cx);

    let row = if let Some(override_fn) = options.overrides.list_item.as_ref() {
      override_fn(
        ListItemView {
          bullet,
          checked: item.checked,
          content: item_content,
        },
        cx,
      )
    } else {
      div()
        .flex()
        .items_start()
        .w_full()
        .min_w_0()
        .child(
          div()
            .flex_none()
            .text_sm()
            .text_color(theme.foreground)
            .pr(px(4.0))
            .child(bullet),
        )
        .child(div().min_w_0().flex_1().child(item_content))
        .into_any_element()
    };

    container = container.child(row);
  }

  let el = container.into_any_element();
  if let Some(override_fn) = options.overrides.list.as_ref() {
    override_fn(el, cx)
  } else {
    el
  }
}

/// Render the blocks within a list item.
fn render_list_item_blocks(
  blocks: &[Block],
  options: &MarkdownRenderOptions,
  cx: &App,
) -> AnyElement {
  let mut container = div().flex().flex_col().w_full().min_w_0().gap_2();
  for block in blocks {
    container = container.child(render_block(block, options, 0, cx));
  }
  container.into_any_element()
}

/// Render a `<details>` block with interactive toggle.
///
/// Click the summary line to open/close. State persists across re-renders
/// via `MarkdownRenderOptions::details_state`.
fn render_details(
  details: &Details,
  options: &MarkdownRenderOptions,
  indent: usize,
  cx: &App,
) -> AnyElement {
  let theme = options.theme();
  let details_id = options.details_state.next_id();
  let is_open = options.details_state.is_open(details_id, details.open);
  let chevron = if is_open { "▼ " } else { "▶ " };

  // Build a stable element ID for the clickable summary.
  let toggle_id: SharedString = format!("gfm-details-{details_id}").into();

  let toggle_state = options.details_state.clone();
  let default_open = details.open;

  let summary_el = div()
    .id(toggle_id)
    .flex()
    .items_center()
    .gap_1()
    .text_sm()
    .font_weight(gpui::FontWeight::MEDIUM)
    .text_color(theme.foreground)
    .cursor_pointer()
    .child(chevron)
    .child(render_inline_text(&details.summary, options, cx))
    .on_mouse_down(MouseButton::Left, move |_, window, _cx| {
      toggle_state.toggle(details_id, default_open);
      window.refresh();
    });

  let mut container = div().flex().flex_col().gap_2().child(summary_el);

  // Render body if open
  if is_open {
    container = container.child(div().pl(px(16.0)).child(render_blocks(
      &details.blocks,
      options,
      indent + 1,
      cx,
    )));
  }

  container.into_any_element()
}
