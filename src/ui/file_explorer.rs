use crate::model::document::DocumentState;
use crate::services::fs::is_markdown_path;
use crate::services::settings::{self, SidebarSectionOrder};
use crate::ui::text_utils::ellipsize_chars;
use crate::ui::theme::Theme;
use camino::{Utf8Path, Utf8PathBuf};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, Entity, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, Render, ScrollHandle, StatefulInteractiveElement, Styled, Window, div, px,
};
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use std::fs;
use std::sync::Arc;
use std::time::SystemTime;

/// Describes one heading rendered in the document Outline.
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

/// Callback invoked when a Markdown file is selected from the FILES list.
pub type OpenFileCallback = Arc<dyn Fn(Utf8PathBuf, &mut App)>;

/// Caches the Markdown files found in one directory until its metadata changes.
struct MarkdownFilesCache {
    directory: Utf8PathBuf,
    modified: Option<SystemTime>,
    files: Vec<Utf8PathBuf>,
}

/// Renders the current document Outline and sibling Markdown files in the sidebar.
pub struct FileExplorerView {
    document: Entity<DocumentState>,
    outline_scroll_handle: ScrollHandle,
    files_scroll_handle: ScrollHandle,
    width: f32,
    cached_outline: Option<(u64, Vec<OutlineItem>)>,
    cached_markdown_files: Option<MarkdownFilesCache>,
    /// Explicit folder opened by the user, shared across document tabs in the window.
    workspace_directory: Option<Utf8PathBuf>,
    /// Currently active heading in document order.
    active_outline_ordinal: Option<usize>,
    /// Called when an outline entry is clicked so the host can scroll the
    /// editor/preview to that section.
    on_reveal: Option<RevealCallback>,
    /// Called when a Markdown file is clicked so the host can open its tab.
    on_open_file: Option<OpenFileCallback>,
}

impl FileExplorerView {
    /// Creates a sidebar bound to one document entity.
    pub fn new(document: Entity<DocumentState>) -> Self {
        Self {
            document,
            outline_scroll_handle: ScrollHandle::new(),
            files_scroll_handle: ScrollHandle::new(),
            width: 200.0,
            cached_outline: None,
            cached_markdown_files: None,
            workspace_directory: None,
            active_outline_ordinal: None,
            on_reveal: None,
            on_open_file: None,
        }
    }

    /// Updates the rendered sidebar width after a divider drag.
    pub fn set_width(&mut self, width: f32, cx: &mut gpui::Context<Self>) {
        self.width = width;
        cx.notify();
    }

    /// Selects an Outline entry and keeps it visible in the Outline scroll area.
    pub fn set_active_outline(&mut self, ordinal: Option<usize>, cx: &mut Context<Self>) {
        if self.active_outline_ordinal != ordinal {
            self.active_outline_ordinal = ordinal;
            if let Some(ordinal) = ordinal {
                self.outline_scroll_handle.scroll_to_item(ordinal);
            }
            cx.notify();
        }
    }

    /// Selects the last heading whose source byte offset precedes the viewport position.
    pub fn set_active_outline_for_byte(&mut self, byte: usize, cx: &mut Context<Self>) {
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

        self.set_active_outline(active_outline_ordinal(&outline_items, byte), cx);
    }

    /// Registers the callback invoked when an outline entry is selected.
    pub fn set_on_reveal(&mut self, callback: RevealCallback) {
        self.on_reveal = Some(callback);
    }

    /// Registers the callback invoked when a Markdown file is selected.
    pub fn set_on_open_file(&mut self, callback: OpenFileCallback) {
        self.on_open_file = Some(callback);
    }

    /// Sets the folder shown by FILES instead of deriving it from the active document.
    pub fn set_workspace_directory(
        &mut self,
        directory: Option<Utf8PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if self.workspace_directory != directory {
            self.workspace_directory = directory;
            self.cached_markdown_files = None;
            cx.notify();
        }
    }

    /// Returns the cached sibling Markdown files, refreshing after directory changes.
    fn markdown_files(&mut self, cx: &App) -> Vec<Utf8PathBuf> {
        let directory = self.workspace_directory.clone().or_else(|| {
            self.document
                .read(cx)
                .path
                .as_ref()
                .and_then(|path| path.parent())
                .map(Utf8Path::to_path_buf)
        });
        let Some(directory) = directory else {
            self.cached_markdown_files = None;
            return Vec::new();
        };
        let modified = fs::metadata(directory.as_std_path())
            .and_then(|metadata| metadata.modified())
            .ok();

        if let Some(cache) = &self.cached_markdown_files
            && cache.directory == directory
            && cache.modified == modified
        {
            return cache.files.clone();
        }

        let files = list_markdown_files(&directory);
        self.cached_markdown_files = Some(MarkdownFilesCache {
            directory,
            modified,
            files: files.clone(),
        });
        files
    }

    /// Builds a sidebar section label and its optional order-swap button.
    fn render_section_header(
        &mut self,
        label: &'static str,
        show_swap_button: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .px(px(10.))
            .py(px(6.))
            .text_xs()
            .font_weight(gpui::FontWeight::BOLD)
            .text_color(Theme::muted())
            .child(label)
            .when(show_swap_button, |this| {
                this.child(
                    div()
                        .id("swap-sidebar-sections")
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(20.))
                        .h(px(20.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .font_weight(gpui::FontWeight::NORMAL)
                        .hover(|this| this.bg(Theme::panel_alt()))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|_, _: &MouseDownEvent, _, cx| {
                                settings::toggle_sidebar_section_order();
                                cx.notify();
                            }),
                        )
                        .child("⇅"),
                )
            })
            .into_any_element()
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
        let current_path = self.document.read(cx).path.clone();
        let markdown_files = self.markdown_files(cx);
        let show_files = should_show_files(&markdown_files, self.workspace_directory.is_some());
        let section_order = settings::get_sidebar_section_order();

        let outline_elements: Vec<_> = outline_items
            .into_iter()
            .map(|item| {
                let ordinal = item.ordinal;
                let level = item.level;
                let title = item.title;
                let byte_start = item.byte_start;
                let is_active = self.active_outline_ordinal == Some(ordinal);
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
                    .when(is_active, |this| this.bg(Theme::panel_alt()))
                    .hover(|this| this.bg(Theme::panel_alt()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.set_active_outline(Some(ordinal), cx);
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
                                    .w(px(if is_active { 6. } else { 4. }))
                                    .h(px(if is_active { 6. } else { 4. }))
                                    .rounded_full()
                                    .bg(if is_active {
                                        Theme::control_accent()
                                    } else {
                                        Theme::muted()
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .line_height(px(20.))
                            .overflow_hidden()
                            .flex_1()
                            .text_color(if is_active {
                                Theme::control_accent()
                            } else {
                                Theme::text()
                            })
                            .child(ellipsize_chars(&title, 64)),
                    )
            })
            .collect();

        let file_elements: Vec<_> = markdown_files
            .into_iter()
            .enumerate()
            .map(|(index, path)| {
                let file_name = path.file_name().unwrap_or("Untitled").to_string();
                let is_active = current_path.as_ref() == Some(&path);
                let on_open_file = self.on_open_file.clone();
                div()
                    .id(("markdown-file-entry", index))
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .px(px(10.))
                    .py(px(4.))
                    .cursor_pointer()
                    .when(is_active, |this| this.bg(Theme::panel_alt()))
                    .hover(|this| this.bg(Theme::panel_alt()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _: &MouseDownEvent, _, cx| {
                            if let Some(on_open_file) = on_open_file.clone() {
                                on_open_file(path.clone(), cx);
                            }
                        }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(if is_active {
                                Theme::control_accent()
                            } else {
                                Theme::muted()
                            })
                            .child("M↓"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_sm()
                            .text_color(if is_active {
                                Theme::control_accent()
                            } else {
                                Theme::text()
                            })
                            .child(ellipsize_chars(&file_name, 48)),
                    )
            })
            .collect();

        let outline_section = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(80.))
            .child(self.render_section_header(
                "OUTLINE",
                show_files && section_order == SidebarSectionOrder::OutlineFirst,
                cx,
            ))
            .child(
                div()
                    .id("outline-scroll")
                    .flex_1()
                    .min_h(px(80.))
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
            .into_any_element();

        let files_section = show_files.then(|| {
            div()
                .flex()
                .flex_col()
                .flex_shrink_0()
                .child(self.render_section_header(
                    "FILES",
                    section_order == SidebarSectionOrder::FilesFirst,
                    cx,
                ))
                .child(
                    div()
                        .id("files-scroll")
                        .max_h(px(240.))
                        .flex_shrink_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.files_scroll_handle)
                        .when(file_elements.is_empty(), |this| {
                            this.child(
                                div()
                                    .px(px(10.))
                                    .py(px(8.))
                                    .text_sm()
                                    .text_color(Theme::muted())
                                    .child("No Markdown files"),
                            )
                        })
                        .children(file_elements),
                )
                .into_any_element()
        });

        let divider = || {
            div()
                .h(px(1.))
                .mx(px(10.))
                .bg(Theme::border())
                .into_any_element()
        };

        let sections = match (section_order, files_section) {
            (SidebarSectionOrder::FilesFirst, Some(files)) => {
                vec![files, divider(), outline_section]
            }
            (_, Some(files)) => vec![outline_section, divider(), files],
            (_, None) => vec![outline_section],
        };

        div()
            .flex()
            .flex_col()
            .h_full()
            .w(px(self.width))
            .bg(Theme::sidebar())
            .flex_shrink_0()
            .children(sections)
    }
}

/// Returns supported Markdown files directly inside the supplied directory.
pub(crate) fn list_markdown_files(directory: &Utf8Path) -> Vec<Utf8PathBuf> {
    let Ok(entries) = fs::read_dir(directory.as_std_path()) else {
        return Vec::new();
    };
    let mut files = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_file()))
        .filter_map(|entry| Utf8PathBuf::from_path_buf(entry.path()).ok())
        .filter(is_markdown_path)
        .collect::<Vec<_>>();

    files.sort_by(|left, right| {
        left.file_name()
            .unwrap_or_default()
            .to_lowercase()
            .cmp(&right.file_name().unwrap_or_default().to_lowercase())
            .then_with(|| left.cmp(right))
    });
    files
}

/// Returns whether the FILES section has useful sibling documents to display.
fn should_show_files(files: &[Utf8PathBuf], has_explicit_workspace: bool) -> bool {
    has_explicit_workspace || files.len() > 1
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

fn active_outline_ordinal(items: &[OutlineItem], byte: usize) -> Option<usize> {
    if items.is_empty() {
        return None;
    }

    Some(
        items
            .iter()
            .rposition(|item| item.byte_start <= byte)
            .unwrap_or(0),
    )
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
    use std::fs;

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
    fn active_outline_follows_last_heading_before_viewport_byte() {
        let text = "# Top\n\nbody\n\n## Child\n\nmore\n\n### Leaf\n";
        let items = parse_outline_items(text);
        let child = text.find("## Child").unwrap();
        let leaf = text.find("### Leaf").unwrap();

        assert_eq!(active_outline_ordinal(&items, 0), Some(0));
        assert_eq!(active_outline_ordinal(&items, child - 1), Some(0));
        assert_eq!(active_outline_ordinal(&items, child), Some(1));
        assert_eq!(active_outline_ordinal(&items, leaf), Some(2));
    }

    #[test]
    fn lists_supported_markdown_files_in_name_order() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("zeta.md"), "# Zeta").unwrap();
        fs::write(directory.path().join("Alpha.MD"), "# Alpha").unwrap();
        fs::write(directory.path().join("notes.markdown"), "# Notes").unwrap();
        fs::write(directory.path().join("image.png"), "not markdown").unwrap();
        fs::create_dir(directory.path().join("nested.md")).unwrap();
        let directory = Utf8Path::from_path(directory.path()).unwrap();

        let names = list_markdown_files(directory)
            .into_iter()
            .map(|path| path.file_name().unwrap().to_string())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["Alpha.MD", "notes.markdown", "zeta.md"]);
    }

    #[test]
    fn files_section_shows_multiple_siblings_or_an_explicit_workspace() {
        assert!(!should_show_files(&[], false));
        assert!(!should_show_files(&[Utf8PathBuf::from("only.md")], false));
        assert!(should_show_files(&[], true));
        assert!(should_show_files(
            &[
                Utf8PathBuf::from("first.md"),
                Utf8PathBuf::from("second.md"),
            ],
            false
        ));
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
