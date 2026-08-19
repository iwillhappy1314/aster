use crate::commands::{
    FontSizeDecrease, FontSizeIncrease, FontSizeReset, NewFile, OpenFile, OutlinePositionLeft,
    OutlinePositionRight, SaveFile, SaveFileAs, ToggleOutline, TogglePreview,
};
use crate::model::document::DocumentState;
use crate::model::inline_markdown::InlineMarkdownState;
use crate::services::fs::{
    pick_open_markdown_path_async, pick_save_path_async, read_to_string, write_atomic,
};
use crate::services::inline_markdown::compute_inline_spans;
use crate::services::settings::{self, OutlinePosition, Settings};
use crate::services::tasks::Debouncer;
use crate::ui::editor::EditorView;
use crate::ui::file_explorer::FileExplorerView;
use crate::ui::theme::Theme;

use camino::Utf8PathBuf;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Bounds, Context, Entity, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, ParentElement, Render, ScrollHandle, StatefulInteractiveElement, Styled,
    Window, canvas, div, fill, point, px, size,
};
use gpui_component::notification::NotificationList;
use gpui_gfm::{
    MarkdownCache, MarkdownRenderOptions, MarkdownTheme, render_markdown_blocks_cached,
};
use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use std::time::Duration;

const INLINE_SYNC_PARSE_MAX_BYTES: usize = 64 * 1024;

pub struct RootView {
    document: Entity<DocumentState>,
    inline_markdown: Entity<InlineMarkdownState>,
    editor_view: Entity<crate::ui::editor::EditorView>,
    file_explorer_view: Entity<crate::ui::file_explorer::FileExplorerView>,
    notifications: Entity<NotificationList>,
    inline_debounce: Debouncer<RootView>,
    /// Highest document revision for which an inline parse has been scheduled.
    scheduled_inline_revision: u64,
    /// Cached document text to avoid O(n) rope-to-string conversion every frame
    cached_doc_text: Option<(u64, String)>,
    /// Whether we've installed the outline-click reveal callback.
    outline_reveal_installed: bool,
    /// Current font size in points (8-32)
    font_size: f32,
    /// Current sidebar width in pixels
    sidebar_width: f32,
    /// Whether we're currently resizing the sidebar
    resizing_sidebar: bool,
    /// Whether the optional document outline is visible.
    outline_visible: bool,
    /// Whether the document is currently shown in read-only preview mode.
    preview_visible: bool,
    /// Scroll state shared by the preview content and its right-edge indicator.
    preview_scroll_handle: ScrollHandle,
    /// Whether the preview's right-edge scroll indicator is currently visible.
    preview_scroll_indicator_visible: bool,
    /// Monotonic revision used to prevent an older hide timer from hiding a newer scroll event.
    preview_scroll_indicator_revision: u64,
    /// Persistent gpui-gfm state so selection and interactive Markdown survive re-renders.
    preview_markdown_options: MarkdownRenderOptions,
    /// Parsed Markdown cache reused by the preview across GPUI render passes.
    preview_markdown_cache: MarkdownCache,
    /// Direct preview child indices for headings, in Outline order.
    preview_heading_child_indices: std::sync::Arc<std::sync::Mutex<Vec<usize>>>,
}

impl RootView {
    pub fn new(
        document: Entity<DocumentState>,
        inline_markdown: Entity<InlineMarkdownState>,
        editor_view: Entity<crate::ui::editor::EditorView>,
        file_explorer_view: Entity<crate::ui::file_explorer::FileExplorerView>,
        notifications: Entity<NotificationList>,
    ) -> Self {
        Self {
            document,
            inline_markdown,
            editor_view,
            file_explorer_view,
            notifications,
            inline_debounce: Debouncer::new(Duration::from_millis(35)),
            scheduled_inline_revision: 0,
            cached_doc_text: None,
            outline_reveal_installed: false,
            font_size: settings::get_font_size(),
            sidebar_width: 300.0,
            resizing_sidebar: false,
            outline_visible: false,
            preview_visible: true,
            preview_scroll_handle: ScrollHandle::new(),
            preview_scroll_indicator_visible: false,
            preview_scroll_indicator_revision: 0,
            preview_markdown_options: MarkdownRenderOptions::default(),
            preview_markdown_cache: MarkdownCache::default(),
            preview_heading_child_indices: Default::default(),
        }
    }

    pub fn new_document() -> DocumentState {
        DocumentState::new_empty()
    }

    pub fn new_inline_markdown() -> InlineMarkdownState {
        InlineMarkdownState::new()
    }

    pub fn build_editor(
        document: Entity<DocumentState>,
        inline_markdown: Entity<InlineMarkdownState>,
    ) -> crate::ui::editor::EditorView {
        EditorView::new(document, inline_markdown)
    }

    pub fn build_file_explorer(
        document: Entity<DocumentState>,
    ) -> crate::ui::file_explorer::FileExplorerView {
        FileExplorerView::new(document)
    }

    fn save_document(&mut self, cx: &mut Context<Self>, force_save_as: bool) {
        let current_path = self.document.read(cx).path.clone();

        // If we have a path and not forcing save-as, save directly
        if !force_save_as {
            if let Some(path) = current_path {
                self.do_save_to_path_sync(path, cx);
                return;
            }
        }

        // Need to show file picker - use async dialog
        let receiver = pick_save_path_async(cx, current_path.as_ref());

        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(path))) = receiver.await {
                if let Ok(mut utf8_path) = Utf8PathBuf::try_from(path) {
                    if utf8_path.extension().is_none() {
                        utf8_path.set_extension("md");
                    }

                    // Read document contents and write synchronously
                    let contents_result =
                        this.update(&mut *cx, |this, cx| this.document.read(cx).text());

                    if let Ok(contents) = contents_result {
                        if write_atomic(&utf8_path, &contents).is_ok() {
                            let _ = this.update(&mut *cx, |this, cx| {
                                let _ = this.document.update(cx, |d, cx| {
                                    d.path = Some(utf8_path.clone());
                                    d.save_snapshot();
                                    cx.notify();
                                });
                                cx.add_recent_document(utf8_path.as_std_path());
                                // Note: Notifications require window context, skipping in async
                            });
                        }
                    }
                }
            }
        })
        .detach();
    }

    /// Synchronous save for when we have a path and window context
    fn do_save_to_path_sync(&mut self, mut path: Utf8PathBuf, cx: &mut Context<Self>) {
        if path.extension().is_none() {
            path.set_extension("md");
        }

        let contents = self.document.read(cx).text();
        match write_atomic(&path, &contents) {
            Ok(()) => {
                let _ = self.document.update(cx, |d, cx| {
                    d.path = Some(path.clone());
                    d.save_snapshot();
                    cx.notify();
                });
                cx.add_recent_document(path.as_std_path());
                // Skip notification here too - simplifies and avoids window context issues
            }
            Err(_err) => {
                // Silently fail for async context - no window for notification
            }
        }
    }

    fn confirm_can_discard_changes(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
        prompt: &str,
    ) -> bool {
        let is_dirty = self.document.read(cx).dirty;
        if !is_dirty {
            return true;
        }

        let choice = MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title("Unsaved changes")
            .set_description(prompt)
            .set_buttons(MessageButtons::YesNoCancelCustom(
                "Save".to_string(),
                "Don't Save".to_string(),
                "Cancel".to_string(),
            ))
            .show();

        let save_sync = |this: &mut Self, cx: &mut Context<Self>| -> bool {
            // Only save synchronously if we have an existing path
            let current_path = this.document.read(cx).path.clone();
            if let Some(path) = current_path {
                this.do_save_to_path_sync(path, cx);
                true
            } else {
                // No path - need async dialog, cancel for now
                // Start async save in background
                this.save_document(cx, false);
                false
            }
        };

        match choice {
            MessageDialogResult::Ok | MessageDialogResult::Yes => save_sync(self, cx),
            MessageDialogResult::No => true,
            MessageDialogResult::Custom(label) => match label.as_str() {
                "Save" => save_sync(self, cx),
                "Don't Save" => true,
                _ => false,
            },
            _ => false,
        }
    }

    pub fn open_path(
        &mut self,
        path: &camino::Utf8PathBuf,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_path_internal(path, cx);
    }

    /// Internal open path that doesn't require window - for async context
    fn open_path_internal(&mut self, path: &camino::Utf8PathBuf, cx: &mut Context<Self>) {
        match read_to_string(path) {
            Ok(text) => {
                let _ = self.document.update(cx, |d, cx| {
                    d.path = Some(path.clone());
                    d.set_text(&text);
                    d.clear_undo_history();
                    d.save_snapshot();
                    cx.notify();
                });
                cx.add_recent_document(path.as_std_path());
            }
            Err(_err) => {
                // Silently fail for async context - no window for notification
            }
        }
    }

    fn action_new_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.confirm_can_discard_changes(window, cx, "Save changes before creating a new file?")
        {
            return;
        }

        let _ = self.document.update(cx, |d, cx| {
            d.path = None;
            d.set_text("");
            d.clear_undo_history();
            d.save_snapshot();
            cx.notify();
        });
        // No notification for new file - only save gets a notification
    }

    fn action_open_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.confirm_can_discard_changes(
            window,
            cx,
            "Save changes before opening another file?",
        ) {
            return;
        }

        let picker = pick_open_markdown_path_async();
        cx.spawn(async move |this, cx| {
            if let Some(utf8_path) = picker.await {
                let _ = this.update(&mut *cx, |this, cx| {
                    this.open_path_internal(&utf8_path, cx);
                });
            }
        })
        .detach();
    }

    pub fn action_open_path(
        &mut self,
        path: camino::Utf8PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.confirm_can_discard_changes(
            window,
            cx,
            "Save changes before opening another file?",
        ) {
            return;
        }
        self.open_path(&path, window, cx);
    }

    pub fn confirm_before_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let can_quit =
            self.confirm_can_discard_changes(window, cx, "Save changes before quitting?");
        if can_quit {
            self.persist_window_bounds(window);
        }
        can_quit
    }

    fn action_save(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.save_document(cx, false);
    }

    fn action_save_as(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.save_document(cx, true);
    }

    pub fn action_close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.confirm_can_discard_changes(window, cx, "Save changes before closing?") {
            return;
        }
        self.persist_window_bounds(window);
        window.remove_window();
    }

    /// Persists the current window geometry before an application-initiated close.
    fn persist_window_bounds(&self, window: &Window) {
        settings::set_window_bounds(window.bounds());
    }

    /// Toggles the optional document outline without changing the document state.
    fn toggle_outline(&mut self, cx: &mut Context<Self>) {
        self.outline_visible = !self.outline_visible;
        self.resizing_sidebar = false;
        cx.notify();
    }

    /// Switches between the rendered preview and the editable Markdown source view.
    fn toggle_preview(&mut self, cx: &mut Context<Self>) {
        self.preview_visible = !self.preview_visible;
        cx.notify();
    }

    /// Shows the preview scroll indicator briefly after a wheel-scroll event.
    fn reveal_preview_scroll_indicator(&mut self, cx: &mut Context<Self>) {
        self.preview_scroll_indicator_visible = true;
        self.preview_scroll_indicator_revision =
            self.preview_scroll_indicator_revision.wrapping_add(1);
        let revision = self.preview_scroll_indicator_revision;
        let entity = cx.entity();

        cx.spawn(async move |_view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1_200))
                .await;
            let _ = entity.update(cx, |view, cx| {
                if view.preview_scroll_indicator_revision == revision {
                    view.preview_scroll_indicator_visible = false;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn sync_preview_outline_active(
        &mut self,
        heading_child_indices: &[usize],
        cx: &mut Context<Self>,
    ) {
        if heading_child_indices.is_empty() {
            let _ = self.file_explorer_view.update(cx, |view, cx| {
                view.set_active_outline(None, cx);
            });
            return;
        }

        let viewport = self.preview_scroll_handle.bounds();
        let offset = self.preview_scroll_handle.offset();
        let activation_line = viewport.top() + px(32.);
        let mut active = Some(0usize);

        for (ordinal, child_index) in heading_child_indices.iter().copied().enumerate() {
            let Some(bounds) = self.preview_scroll_handle.bounds_for_item(child_index) else {
                continue;
            };
            let painted_top = bounds.top() + offset.y;
            if painted_top <= activation_line {
                active = Some(ordinal);
            } else {
                break;
            }
        }

        let _ = self.file_explorer_view.update(cx, |view, cx| {
            view.set_active_outline(active, cx);
        });
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Install the Outline reveal callback once. The editor uses TextLayout's
        // exact Y position; preview uses ScrollHandle's real direct-child bounds.
        if !self.outline_reveal_installed {
            self.outline_reveal_installed = true;
            let editor = self.editor_view.clone();
            let preview_scroll = self.preview_scroll_handle.clone();
            let preview_heading_indices = self.preview_heading_child_indices.clone();
            let callback: crate::ui::file_explorer::RevealCallback =
                std::sync::Arc::new(move |heading_ordinal, byte_start, cx: &mut App| {
                    let _ = editor.update(cx, |editor, cx| editor.reveal_outline(byte_start, cx));

                    let child_index = preview_heading_indices
                        .lock()
                        .ok()
                        .and_then(|indices| indices.get(heading_ordinal).copied());
                    if let Some(child_index) = child_index {
                        if let Some(child_bounds) = preview_scroll.bounds_for_item(child_index) {
                            let viewport = preview_scroll.bounds();
                            let max = preview_scroll.max_offset();
                            let current = preview_scroll.offset();
                            let mut next = current;
                            next.y = (viewport.top() + px(24.) - child_bounds.top())
                                .clamp(-max.height, px(0.));
                            preview_scroll.set_offset(next);
                        } else {
                            preview_scroll.scroll_to_top_of_item(child_index);
                        }
                    }
                });
            let _ = self
                .file_explorer_view
                .update(cx, |view, _| view.set_on_reveal(callback));

            let file_explorer = self.file_explorer_view.clone();
            let viewport_callback: crate::ui::editor::OutlineViewportCallback =
                std::sync::Arc::new(move |byte_start, cx: &mut App| {
                    let _ = file_explorer.update(cx, |view, cx| {
                        view.set_active_outline_for_byte(byte_start, cx);
                    });
                });
            let _ = self.editor_view.update(cx, |view, _| {
                view.set_on_outline_viewport_change(viewport_callback);
            });
        }

        let (doc_path, doc_dirty, doc_revision) = {
            self.document
                .update(cx, |doc, _| (doc.path.clone(), doc.dirty, doc.revision))
        };

        // Use cached text if revision hasn't changed to avoid O(n) rope conversion
        let doc_text = if let Some((cached_rev, ref text)) = self.cached_doc_text {
            if cached_rev == doc_revision {
                text.clone()
            } else {
                let text = self.document.read(cx).text();
                self.cached_doc_text = Some((doc_revision, text.clone()));
                text
            }
        } else {
            let text = self.document.read(cx).text();
            self.cached_doc_text = Some((doc_revision, text.clone()));
            text
        };
        let inline_rev = self.inline_markdown.read(cx).source_revision;

        if doc_revision != inline_rev && self.scheduled_inline_revision < doc_revision {
            self.scheduled_inline_revision = doc_revision;
            let last_edit = self.document.read(cx).last_edit.clone();
            let target_rev = doc_revision;
            if doc_text.len() <= INLINE_SYNC_PARSE_MAX_BYTES {
                // Small/medium notes: parse inline to avoid style flicker between keystrokes.
                let parsed = compute_inline_spans(&doc_text, last_edit.as_ref());
                let _ = self.inline_markdown.update(cx, |state, cx| {
                    if target_rev >= state.source_revision {
                        state.spans = std::sync::Arc::new(parsed.spans);
                        state.source_revision = target_rev;
                        state.parse_millis = parsed.parse_millis;
                        cx.notify();
                    } else {
                        state.dropped_updates = state.dropped_updates.saturating_add(1);
                    }
                });
            } else {
                // Large notes: debounce and parse in background to protect typing latency.
                let text = doc_text.clone();
                let inline_markdown = self.inline_markdown.clone();
                self.inline_debounce.schedule(cx, move |_, cx| {
                    let text = text.clone();
                    let last_edit = last_edit.clone();
                    let inline_markdown = inline_markdown.clone();
                    cx.spawn(async move |_, cx| {
                        let parsed = cx
                            .background_executor()
                            .spawn(async move { compute_inline_spans(&text, last_edit.as_ref()) })
                            .await;
                        let _ = inline_markdown.update(cx, |state, cx| {
                            if target_rev >= state.source_revision {
                                state.spans = std::sync::Arc::new(parsed.spans);
                                state.source_revision = target_rev;
                                state.parse_millis = parsed.parse_millis;
                                cx.notify();
                            } else {
                                state.dropped_updates = state.dropped_updates.saturating_add(1);
                            }
                        });
                    })
                    .detach();
                });
            }
        }

        // Use size_full() instead of explicit pixel dimensions to ensure proper layout

        let window_title = {
            let name = doc_path
                .as_ref()
                .and_then(|p| p.file_name())
                .unwrap_or("untitled.md");
            let dirty = if doc_dirty { " •" } else { "" };
            format!("{name}{dirty} — Aster")
        };
        window.set_window_title(&window_title);

        let outline_position = settings::get_outline_position();
        let resize_line_color = if self.resizing_sidebar {
            gpui::rgba(0x2d7fd299)
        } else {
            Theme::border()
        };
        let outline_toggle_color = if self.outline_visible {
            Theme::accent()
        } else {
            Theme::muted()
        };
        let mode_toggle_color = if self.preview_visible {
            Theme::accent()
        } else {
            Theme::muted()
        };

        let top_chrome = div()
            .id("window-chrome")
            .h(px(38.))
            .w_full()
            .bg(Theme::bg())
            .flex()
            .items_center()
            .justify_end()
            .gap_3()
            .pr(px(12.))
            .flex_shrink_0()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, window, _| {
                    window.start_window_move();
                }),
            )
            .child(
                div()
                    .id("outline-toggle")
                    .p(px(5.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|this| this.bg(Theme::panel_alt()).text_color(Theme::text()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.toggle_outline(cx);
                        }),
                    )
                    .child(
                        div()
                            .w(px(16.))
                            .h(px(16.))
                            .flex()
                            .when(outline_position == OutlinePosition::Right, |this| {
                                this.flex_row_reverse()
                            })
                            .rounded(px(3.))
                            .border_1()
                            .border_color(outline_toggle_color)
                            .child(
                                div()
                                    .w(px(5.))
                                    .h_full()
                                    .when(outline_position == OutlinePosition::Left, |this| {
                                        this.border_r_1()
                                    })
                                    .when(outline_position == OutlinePosition::Right, |this| {
                                        this.border_l_1()
                                    })
                                    .border_color(outline_toggle_color),
                            )
                            .child(div().flex_1()),
                    ),
            )
            .child(
                div()
                    .id("preview-toggle")
                    .p(px(5.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|this| this.bg(Theme::panel_alt()).text_color(Theme::text()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.toggle_preview(cx);
                        }),
                    )
                    // Use native geometry instead of an asset-backed SVG so the control is
                    // always visible in the packaged application.
                    .child(
                        div()
                            .w(px(16.))
                            .h(px(16.))
                            .rounded(px(3.))
                            .border_1()
                            .border_color(mode_toggle_color)
                            .px(px(3.))
                            .flex()
                            .flex_col()
                            .justify_center()
                            .gap(px(2.))
                            .child(div().w_full().h(px(1.)).bg(mode_toggle_color))
                            .child(div().w_full().h(px(1.)).bg(mode_toggle_color))
                            .child(div().w(px(6.)).h(px(1.)).bg(mode_toggle_color)),
                    ),
            );

        div()
            .relative()
            .flex()
            .flex_col()
            .bg(Theme::bg())
            .text_color(Theme::text())
            .size_full()
            .on_action(cx.listener(|this, _: &NewFile, window, cx| {
                this.action_new_file(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenFile, window, cx| {
                this.action_open_file(window, cx);
            }))
            .on_action(cx.listener(|this, _: &SaveFile, window, cx| {
                this.action_save(window, cx);
            }))
            .on_action(cx.listener(|this, _: &SaveFileAs, window, cx| {
                this.action_save_as(window, cx);
            }))
            .on_action(cx.listener(|this, _: &FontSizeIncrease, _window, cx| {
                this.font_size =
                    Settings::clamp_font_size(this.font_size + Settings::FONT_SIZE_STEP);
                settings::set_font_size(this.font_size);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FontSizeDecrease, _window, cx| {
                this.font_size =
                    Settings::clamp_font_size(this.font_size - Settings::FONT_SIZE_STEP);
                settings::set_font_size(this.font_size);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FontSizeReset, _window, cx| {
                this.font_size = Settings::DEFAULT_FONT_SIZE;
                settings::set_font_size(this.font_size);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OutlinePositionLeft, _window, cx| {
                settings::set_outline_position(OutlinePosition::Left);
                this.resizing_sidebar = false;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OutlinePositionRight, _window, cx| {
                settings::set_outline_position(OutlinePosition::Right);
                this.resizing_sidebar = false;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleOutline, _window, cx| {
                this.toggle_outline(cx);
            }))
            .on_action(cx.listener(|this, _: &TogglePreview, _window, cx| {
                this.toggle_preview(cx);
            }))
            // Handle sidebar resize drag at root level so we don't lose events
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                if !this.resizing_sidebar {
                    return;
                }
                let pointer_x: f32 = event.position.x.into();
                let new_width = match settings::get_outline_position() {
                    OutlinePosition::Left => pointer_x,
                    OutlinePosition::Right => {
                        let window_width: f32 = window.bounds().size.width.into();
                        window_width - pointer_x
                    }
                };
                let clamped = new_width.clamp(100.0, 400.0);
                this.sidebar_width = clamped;
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.resizing_sidebar {
                        this.resizing_sidebar = false;
                        cx.notify();
                    }
                }),
            )
            .child(top_chrome)
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .min_w(px(0.))
                    .flex()
                    .flex_row()
                    .when(outline_position == OutlinePosition::Right, |this| {
                        this.flex_row_reverse()
                    })
                    .when(self.outline_visible, |this| {
                        this.child({
                            // Keep the sidebar width in sync with the resize state.
                            let file_explorer = self.file_explorer_view.clone();
                            let width = self.sidebar_width;
                            let _ = file_explorer.update(cx, |view, cx| {
                                view.set_width(width, cx);
                            });
                            file_explorer
                        })
                        .child(
                            div()
                                .id("sidebar-resize-handle")
                                .w(px(1.))
                                .h_full()
                                .cursor_col_resize()
                                .bg(resize_line_color)
                                .hover(|s| s.bg(gpui::rgba(0x2d7fd24d)))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                        this.resizing_sidebar = true;
                                        cx.notify();
                                    }),
                                ),
                        )
                    })
                    .child(if self.preview_visible {
                        let preview_scroll_handle = self.preview_scroll_handle.clone();
                        let has_multiple_lines = doc_text.lines().nth(1).is_some();
                        let show_preview_scroll_indicator = self.preview_scroll_indicator_visible;
                        let markdown_style = Theme::markdown_style();
                        let markdown_theme = MarkdownTheme {
                            foreground: markdown_style.foreground.into(),
                            muted_foreground: markdown_style.muted_foreground.into(),
                            background: markdown_style.background.into(),
                            code_background: markdown_style.code_background.into(),
                            border: markdown_style.border.into(),
                            link: markdown_style.link.into(),
                            accent: markdown_style.accent.into(),
                            code_font_family: "Menlo".into(),
                            is_dark: Theme::is_dark(),
                        };
                        let markdown_options = self
                            .preview_markdown_options
                            .clone()
                            .with_theme(markdown_theme);
                        let markdown_blocks = render_markdown_blocks_cached(
                            &doc_text,
                            &markdown_options,
                            &mut self.preview_markdown_cache,
                            cx,
                        );
                        let heading_child_indices = markdown_blocks.heading_child_indices.clone();
                        if let Ok(mut indices) = self.preview_heading_child_indices.lock() {
                            *indices = heading_child_indices.clone();
                        }
                        self.sync_preview_outline_active(&heading_child_indices, cx);
                        div()
                            .relative()
                            .flex_1()
                            .min_h(px(0.))
                            .min_w(px(0.))
                            .child(
                                div()
                                    .id("preview-scroll")
                                    .size_full()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .pl(px(32.))
                                    .pr(px(32.))
                                    .py(px(24.))
                                    .overflow_y_scroll()
                                    .overflow_x_hidden()
                                    .track_scroll(&self.preview_scroll_handle)
                                    .on_scroll_wheel(cx.listener(
                                        |this, _: &gpui::ScrollWheelEvent, _, cx| {
                                            this.reveal_preview_scroll_indicator(cx);
                                        },
                                    ))
                                    .children(markdown_blocks.elements),
                            )
                            // Match the editor's indicator: it is rendered above the scrolling
                            // content and fixed to the view's right edge.
                            .child(
                                canvas(
                                    move |_, _, _| {},
                                    move |bounds: Bounds<_>,
                                          (),
                                          window: &mut Window,
                                          _cx: &mut App| {
                                        if !show_preview_scroll_indicator {
                                            return;
                                        }
                                        let viewport_height =
                                            preview_scroll_handle.bounds().size.height;
                                        let max_offset = preview_scroll_handle.max_offset().height;
                                        if viewport_height <= px(0.)
                                            || (max_offset <= px(0.) && !has_multiple_lines)
                                        {
                                            return;
                                        }

                                        let content_height = viewport_height + max_offset;
                                        let thumb_height = if max_offset > px(0.) {
                                            (viewport_height / content_height * bounds.size.height)
                                                .max(px(48.))
                                                .min(bounds.size.height)
                                        } else {
                                            px(48.).min(bounds.size.height)
                                        };
                                        let travel = bounds.size.height - thumb_height;
                                        let progress = if max_offset > px(0.) {
                                            (-preview_scroll_handle.offset().y / max_offset)
                                                .clamp(0., 1.)
                                        } else {
                                            0.
                                        };
                                        let top = bounds.origin.y + travel * progress;

                                        window.paint_quad(fill(
                                            Bounds {
                                                origin: point(bounds.right() - px(8.), top),
                                                size: size(px(6.), thumb_height),
                                            },
                                            Theme::muted(),
                                        ));
                                    },
                                )
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full(),
                            )
                            .into_any_element()
                    } else {
                        div()
                            .flex_1()
                            .min_h(px(0.))
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(self.editor_view.clone())
                            .into_any_element()
                    }),
            )
            .child(self.notifications.clone())
    }
}
