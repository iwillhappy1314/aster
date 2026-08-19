use crate::model::document::DocumentState;
use crate::ui::text_utils::ellipsize_chars;
use crate::ui::theme::Theme;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Context, Entity, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, Render, ScrollHandle, StatefulInteractiveElement, Styled, Window, div, px,
};
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use std::sync::Arc;

#[derive(Clone, Debug)]
struct OutlineItem {
    ordinal: usize,
    level: u32,
    title: String,
    byte_start: usize,
}

/// Callback invoked when an outline entry is selected, receiving the heading's
/// byte offset in the document.
pub type RevealCallback = Arc<dyn Fn(usize, usize, &mut App)>;

pub struct FileExplorerView {
    document: Entity<DocumentState>,
    outline_scroll_handle: ScrollHandle,
    width: f32,
    cached_outline: Option<(u64, Vec<OutlineItem>)>,
    /// Called when an outline entry is clicked so the host can scroll the
    /// editor/preview to that section.
    on_reveal: Option<RevealCallback>,
}

impl FileExplorerView {
    pub fn new(document: Entity<DocumentState>) -> Self {
        Self {
            document,
            outline_scroll_handle: ScrollHandle::new(),
            width: 200.0,
            cached_outline: None,
            on_reveal: None,
        }
    }

    pub fn set_width(&mut self, width: f32, cx: &mut gpui::Context<Self>) {
        self.width = width;
        cx.notify();
    }

    /// Registers the callback invoked when an outline entry is selected.
    pub fn set_on_reveal(&mut self, callback: RevealCallback) {
        self.on_reveal = Some(callback);
    }
}

impl Render for FileExplorerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let doc_revision = self.document.read(cx).revision;
        let outline_items = if let Some((cached_revision, items)) = &self.cached_outline {
            if *cached_revision == doc_revision {
                items.clone()
            } else {
                let text = self.document.read(cx).text();
                let parsed = parse_outline_items(&text);
                self.cached_outline = Some((doc_revision, parsed.clone()));
                parsed
            }
        } else {
            let text = self.document.read(cx).text();
            let parsed = parse_outline_items(&text);
            self.cached_outline = Some((doc_revision, parsed.clone()));
            parsed
        };
        let has_outline = !outline_items.is_empty();
        let document = self.document.clone();

        let outline_elements: Vec<_> = outline_items
            .into_iter()
            .map(|item| {
                let ordinal = item.ordinal;
                let level = item.level;
                let title = item.title;
                let byte_start = item.byte_start;
                let indent = (level.saturating_sub(1) as f32) * 10.0;
                let document = document.clone();
                let on_reveal = self.on_reveal.clone();
                div()
                    .id(("outline-entry", ordinal))
                    .flex()
                    .items_start()
                    .gap(px(6.))
                    .pl(px(8. + indent))
                    .pr(px(8.))
                    .py(px(3.))
                    .cursor_pointer()
                    .hover(|this| this.bg(Theme::panel_alt()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_this, _: &MouseDownEvent, _, cx| {
                            let _ = document.update(cx, |doc, cx| {
                                let cursor = doc.byte_to_char(byte_start);
                                doc.set_cursor(cursor);
                                cx.notify();
                            });
                            if let Some(on_reveal) = on_reveal.clone() {
                                on_reveal(ordinal, byte_start, cx);
                            }
                        }),
                    )
                    .child(
                        div()
                            .h(px(20.))
                            .flex()
                            .items_center()
                            .flex_shrink_0()
                            .child(
                                div()
                                    .w(px(4.))
                                    .h(px(4.))
                                    .rounded_full()
                                    .bg(Theme::accent()),
                            ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .line_height(px(20.))
                            .overflow_hidden()
                            .flex_1()
                            .text_color(Theme::text())
                            .child(ellipsize_chars(&title, 64)),
                    )
            })
            .collect();

        div()
            .flex()
            .flex_col()
            .h_full()
            .w(px(self.width))
            .bg(Theme::sidebar())
            .flex_shrink_0()
            .child(
                div()
                    .px(px(10.))
                    .py(px(6.))
                    .text_xs()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(Theme::muted())
                    .child("OUTLINE"),
            )
            .child(
                div()
                    .id("outline-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.outline_scroll_handle)
                    .when(has_outline, |this| this.children(outline_elements))
                    .when(!has_outline, |this| {
                        this.child(
                            div()
                                .px(px(10.))
                                .py(px(8.))
                                .text_sm()
                                .text_color(Theme::muted())
                                .child("No headings"),
                        )
                    }),
            )
    }
}

fn parse_outline_items(text: &str) -> Vec<OutlineItem> {
    let mut items = Vec::new();
    let mut current_heading: Option<(HeadingLevel, usize, String)> = None;

    for (event, source_range) in Parser::new(text).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current_heading = Some((level, source_range.start, String::new()));
            }
            Event::Text(value) | Event::Code(value) => {
                if let Some((_, _, title)) = current_heading.as_mut() {
                    title.push_str(value.as_ref());
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some((_, _, title)) = current_heading.as_mut()
                    && !title.ends_with(' ')
                {
                    title.push(' ');
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, byte_start, title)) = current_heading.take() {
                    let title = title.trim().to_string();
                    if !title.is_empty() {
                        items.push(OutlineItem {
                            ordinal: items.len(),
                            level: heading_level_number(level),
                            title,
                            byte_start,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    items
}

fn heading_level_number(level: HeadingLevel) -> u32 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multilevel_headings_in_document_order() {
        let text = "# Top\n\n## Child\n\n### Leaf\n";
        let items = parse_outline_items(text);

        assert_eq!(items.len(), 3);
        assert_eq!(items[0].ordinal, 0);
        assert_eq!(items[0].level, 1);
        assert_eq!(items[0].title, "Top");
        assert_eq!(items[0].byte_start, text.find("# Top").unwrap());
        assert_eq!(items[1].ordinal, 1);
        assert_eq!(items[1].level, 2);
        assert_eq!(items[1].title, "Child");
        assert_eq!(items[1].byte_start, text.find("## Child").unwrap());
        assert_eq!(items[2].ordinal, 2);
        assert_eq!(items[2].level, 3);
        assert_eq!(items[2].title, "Leaf");
        assert_eq!(items[2].byte_start, text.find("### Leaf").unwrap());
    }

    #[test]
    fn ignores_heading_like_lines_inside_fenced_code() {
        let text = "# Real\n\n```md\n## Not an outline heading\n```\n\n## Next\n";
        let items = parse_outline_items(text);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Real");
        assert_eq!(items[1].title, "Next");
        assert_eq!(items[1].ordinal, 1);
    }

    #[test]
    fn includes_setext_headings_without_shifting_following_ordinals() {
        let text = "Document title\n==============\n\n## Section\n";
        let items = parse_outline_items(text);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].level, 1);
        assert_eq!(items[0].title, "Document title");
        assert_eq!(items[0].byte_start, 0);
        assert_eq!(items[1].level, 2);
        assert_eq!(items[1].title, "Section");
        assert_eq!(items[1].ordinal, 1);
    }
}
