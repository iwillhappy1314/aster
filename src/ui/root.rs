use crate::commands::{
    Copy, Find, FontSizeDecrease, FontSizeIncrease, FontSizeReset, NewFile, OpenFile,
    OutlinePositionLeft, OutlinePositionRight, SaveFile, SaveFileAs, SelectAll, ToggleOutline,
    TogglePreview,
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
use crate::ui::theme::{Theme, ThemeName};

use camino::Utf8PathBuf;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, Entity, EntityInputHandler,
    ExternalPaths, FocusHandle, FontWeight, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement, Pixels, Render,
    ScrollHandle, StatefulInteractiveElement, Styled, UTF16Selection, Window, canvas, div, px,
    size,
};
use gpui_component::notification::NotificationList;
use gpui_gfm::{
    CodeSyntaxTheme, InteractiveScrollbarAxis, InteractiveScrollbarState, MarkdownCache,
    MarkdownRenderOptions, MarkdownTheme, render_interactive_scrollbar,
    render_markdown_blocks_cached,
};
use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

const INLINE_SYNC_PARSE_MAX_BYTES: usize = 64 * 1024;
/// Height reserved for macOS native titlebar controls.
const NATIVE_TITLEBAR_HEIGHT: f32 = 38.0;

/// Returns the file name displayed in the custom title bar, including its unsaved marker.
fn document_title(path: Option<&Utf8PathBuf>, is_dirty: bool) -> String {
    let name = path
        .and_then(|path| path.file_name())
        .unwrap_or("untitled.md");
    let dirty = if is_dirty { " •" } else { "" };

    format!("{name}{dirty}")
}

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
    /// Focus target that keeps preview keyboard actions available to macOS.
    preview_focus_handle: Option<FocusHandle>,
    /// Whether the preview should receive focus after the next render.
    focus_preview_on_render: bool,
    /// Whether the preview Find panel is visible.
    preview_find_active: bool,
    /// Query currently highlighted in rendered preview text.
    preview_find_query: String,
    /// Caret position in the Find query, measured in Unicode scalar values.
    preview_find_cursor: usize,
    /// Text currently under composition by the system input method.
    preview_find_marked_range: Option<Range<usize>>,
    /// Scroll state shared by the preview content and its right-edge indicator.
    preview_scroll_handle: ScrollHandle,
    /// Hover and drag state for the preview's interactive vertical scrollbar.
    preview_scrollbar_state: InteractiveScrollbarState,
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
        preview_visible: bool,
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
            outline_visible: settings::get_outline_visible(),
            preview_visible,
            preview_focus_handle: None,
            focus_preview_on_render: true,
            preview_find_active: false,
            preview_find_query: String::new(),
            preview_find_cursor: 0,
            preview_find_marked_range: None,
            preview_scroll_handle: ScrollHandle::new(),
            preview_scrollbar_state: InteractiveScrollbarState::default(),
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
                self.restore_remembered_preview_mode();
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

        self.force_editor_mode_for_new_file(cx);

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

    fn handle_dropped_paths(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(markdown_path) = paths.paths().iter().find(|path| is_markdown_path(path)) {
            if let Ok(path) = Utf8PathBuf::try_from(markdown_path.clone()) {
                self.action_open_path(path, window, cx);
            }
            return;
        }

        let document_path = self.document.read(cx).path.clone();
        let snippets = paths
            .paths()
            .iter()
            .map(|path| {
                let stored_path = store_dropped_file(path, document_path.as_ref());
                markdown_for_dropped_file(&stored_path, document_path.as_ref())
            })
            .collect::<Vec<_>>();

        if snippets.is_empty() {
            return;
        }

        let insertion = snippets.join("\n");
        let _ = self.document.update(cx, |doc, cx| {
            doc.begin_edit();
            doc.delete_selection();
            let insert_at = doc.cursor;
            doc.insert(insert_at, &insertion);
            doc.cursor = insert_at.saturating_add(insertion.chars().count());
            doc.commit_edit();
            cx.notify();
        });
        let _ = self
            .editor_view
            .update(cx, |editor, cx| editor.reveal_cursor(cx));
        // The preview is rendered by RootView rather than EditorView, so ensure a drop
        // immediately repaints both modes and refreshes the cached document revision.
        cx.notify();
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
        settings::set_outline_visible(self.outline_visible);
        self.resizing_sidebar = false;
        cx.notify();
    }

    /// Switches between the rendered preview and the editable Markdown source view.
    fn toggle_preview(&mut self, cx: &mut Context<Self>) {
        self.preview_visible = !self.preview_visible;
        settings::set_preview_visible(self.preview_visible);
        self.focus_preview_on_render = self.preview_visible;
        cx.notify();
    }

    /// Forces a newly created document into source editing without changing the saved preference.
    fn force_editor_mode_for_new_file(&mut self, cx: &mut Context<Self>) {
        self.preview_visible = false;
        self.preview_find_active = false;
        self.focus_preview_on_render = false;
        cx.notify();
    }

    /// Restores the saved mode when an existing document is opened in this window.
    fn restore_remembered_preview_mode(&mut self) {
        self.preview_visible = settings::get_preview_visible();
        self.preview_find_active = false;
        self.focus_preview_on_render = self.preview_visible;
    }

    /// Opens the Find panel while keeping the rendered Markdown preview visible.
    fn activate_preview_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.preview_find_active = true;
        self.preview_find_cursor = self.preview_find_query.chars().count();
        if let Some(focus_handle) = &self.preview_focus_handle {
            focus_handle.focus(window);
        }
        cx.notify();
    }

    /// Handles non-text preview Find keyboard commands.
    fn handle_preview_find_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.to_lowercase();
        let modifiers = event.keystroke.modifiers;
        let is_command = modifiers.platform || modifiers.control;

        if key == "escape" {
            self.preview_find_active = false;
            cx.notify();
            return;
        }

        if !self.preview_find_active || is_command {
            return;
        }
    }

    /// Replaces part of the Find query and keeps cursor/composition ranges in character offsets.
    fn replace_preview_find_query(
        &mut self,
        target: Range<usize>,
        text: &str,
        marked: bool,
    ) {
        let query_len = self.preview_find_query.chars().count();
        let start = target.start.min(query_len);
        let end = target.end.min(query_len).max(start);
        let start_byte = char_offset_to_byte(&self.preview_find_query, start);
        let end_byte = char_offset_to_byte(&self.preview_find_query, end);
        self.preview_find_query.replace_range(start_byte..end_byte, text);

        let inserted_chars = text.chars().count();
        self.preview_find_cursor = start.saturating_add(inserted_chars);
        self.preview_find_marked_range = marked.then_some(start..start.saturating_add(inserted_chars));
    }

    /// Selects all text rendered in the Markdown preview.
    fn select_all_preview_text(&mut self, cx: &mut Context<Self>) {
        self.preview_markdown_options.select_all_preview_text();
        cx.notify();
    }

    /// Copies the Markdown preview's current text selection to the system clipboard.
    fn copy_preview_selection(&self, cx: &mut Context<Self>) {
        let Some(text) = self.preview_markdown_options.selected_preview_text() else {
            return;
        };

        cx.write_to_clipboard(ClipboardItem::new_string(text));
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

fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "md" | "markdown" | "mdown" | "mkd"
            )
        })
}

fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png"
                    | "jpg"
                    | "jpeg"
                    | "gif"
                    | "webp"
                    | "bmp"
                    | "tif"
                    | "tiff"
                    | "heic"
                    | "heif"
                    | "svg"
                    | "avif"
                    | "ico"
            )
        })
}

fn store_dropped_file(path: &Path, document_path: Option<&Utf8PathBuf>) -> PathBuf {
    let Some(document_dir) = document_path.and_then(|path| path.parent()) else {
        return path.to_path_buf();
    };
    let Some(file_name) = path.file_name().map(|name| name.to_string_lossy().into_owned()) else {
        return path.to_path_buf();
    };

    let assets_dir = document_dir.as_std_path().join("assets");

    // A file already inside this document's assets folder can be referenced directly.
    if path.parent().is_some_and(|parent| parent == assets_dir.as_path()) {
        return path.to_path_buf();
    }

    if fs::create_dir_all(&assets_dir).is_err() {
        return path.to_path_buf();
    }

    let destination = unique_asset_path(&assets_dir, &file_name);
    match fs::copy(path, &destination) {
        Ok(_) => destination,
        Err(_) => path.to_path_buf(),
    }
}

fn unique_asset_path(assets_dir: &Path, file_name: &str) -> PathBuf {
    let original = assets_dir.join(file_name);
    if !original.exists() {
        return original;
    }

    let file_path = Path::new(file_name);
    let stem = file_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("file");
    let extension = file_path.extension().and_then(|ext| ext.to_str());

    for index in 2u32.. {
        let candidate_name = match extension {
            Some(extension) if !extension.is_empty() => {
                format!("{stem}-{index}.{extension}")
            }
            _ => format!("{stem}-{index}"),
        };
        let candidate = assets_dir.join(candidate_name);
        if !candidate.exists() {
            return candidate;
        }
    }

    unreachable!("asset suffix search is unbounded")
}

fn markdown_for_dropped_file(path: &Path, document_path: Option<&Utf8PathBuf>) -> String {
    let label = escape_markdown_label(
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file"),
    );
    let target = markdown_drop_target(path, document_path);

    if is_image_path(path) {
        format!("![{label}](<{target}>)")
    } else {
        format!("[{label}](<{target}>)")
    }
}

fn markdown_drop_target(path: &Path, document_path: Option<&Utf8PathBuf>) -> String {
    let rendered_path = document_path
        .and_then(|document_path| document_path.parent())
        .and_then(|base_dir| relative_path_from(base_dir.as_std_path(), path))
        .unwrap_or_else(|| path.to_path_buf());

    rendered_path
        .to_string_lossy()
        .replace('\\', "/")
        .replace('>', "%3E")
}

fn relative_path_from(base_dir: &Path, target: &Path) -> Option<PathBuf> {
    if base_dir.is_absolute() != target.is_absolute() {
        return None;
    }

    let base = base_dir.components().collect::<Vec<_>>();
    let target = target.components().collect::<Vec<_>>();
    let common = base
        .iter()
        .zip(target.iter())
        .take_while(|(left, right)| left == right)
        .count();

    if common == 0 {
        return None;
    }

    let mut relative = PathBuf::new();
    for _ in common..base.len() {
        relative.push("..");
    }
    for component in target.iter().skip(common) {
        relative.push(component.as_os_str());
    }

    Some(relative)
}

fn escape_markdown_label(label: &str) -> String {
    label
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

fn preview_image_source(source: &str, document_dir: &Path) -> gpui::ImageSource {
    if let Ok(url) = url::Url::parse(source) {
        if url.scheme() == "file" {
            if let Ok(path) = url.to_file_path() {
                return path.into();
            }
        }
        return source.to_string().into();
    }

    if source.starts_with("//") {
        return source.to_string().into();
    }

    let source_path = Path::new(source);
    let resolved = if source_path.is_absolute() {
        source_path.to_path_buf()
    } else {
        document_dir.join(source_path)
    };
    resolved.into()
}

fn open_preview_link(target: &str, document_dir: &Path, cx: &mut App) {
    if target.starts_with('#') || target.starts_with("//") || url::Url::parse(target).is_ok() {
        cx.open_url(target);
        return;
    }

    let target_path = Path::new(target);
    let resolved = if target_path.is_absolute() {
        target_path.to_path_buf()
    } else {
        document_dir.join(target_path)
    };

    if let Ok(file_url) = url::Url::from_file_path(resolved) {
        cx.open_url(file_url.as_str());
    } else {
        cx.open_url(target);
    }
}

/// Converts a UTF-16 offset to a character offset without splitting multi-unit characters.
fn utf16_offset_to_char_offset(text: &str, utf16_offset: usize) -> usize {
    let mut utf16_count = 0usize;
    let mut char_count = 0usize;

    for character in text.chars() {
        let next = utf16_count.saturating_add(character.len_utf16());
        if next > utf16_offset {
            break;
        }
        utf16_count = next;
        char_count += 1;
    }

    char_count
}

/// Converts a character offset to the corresponding byte offset in UTF-8 text.
fn char_offset_to_byte(text: &str, char_offset: usize) -> usize {
    text.char_indices()
        .nth(char_offset)
        .map_or(text.len(), |(byte_offset, _)| byte_offset)
}

/// Converts a character range in the Find query to a UTF-16 range for macOS input APIs.
fn preview_find_char_range_to_utf16(text: &str, range: &Range<usize>) -> Range<usize> {
    let start = text
        .chars()
        .take(range.start.min(text.chars().count()))
        .map(char::len_utf16)
        .sum();
    let end = text
        .chars()
        .take(range.end.min(text.chars().count()))
        .map(char::len_utf16)
        .sum();
    start..end
}

/// Implements macOS text-input callbacks for the preview Find query, including IME composition.
impl EntityInputHandler for RootView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = utf16_offset_to_char_offset(&self.preview_find_query, range_utf16.start)
            ..utf16_offset_to_char_offset(&self.preview_find_query, range_utf16.end);
        adjusted_range.replace(preview_find_char_range_to_utf16(
            &self.preview_find_query,
            &range,
        ));
        let start_byte = char_offset_to_byte(&self.preview_find_query, range.start);
        let end_byte = char_offset_to_byte(&self.preview_find_query, range.end);
        Some(self.preview_find_query[start_byte..end_byte].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let cursor = self.preview_find_cursor.min(self.preview_find_query.chars().count());
        let range = preview_find_char_range_to_utf16(&self.preview_find_query, &(cursor..cursor));
        Some(UTF16Selection {
            range,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.preview_find_marked_range
            .as_ref()
            .map(|range| preview_find_char_range_to_utf16(&self.preview_find_query, range))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.preview_find_marked_range = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = range_utf16
            .as_ref()
            .map(|range| {
                utf16_offset_to_char_offset(&self.preview_find_query, range.start)
                    ..utf16_offset_to_char_offset(&self.preview_find_query, range.end)
            })
            .or_else(|| self.preview_find_marked_range.clone())
            .unwrap_or(self.preview_find_cursor..self.preview_find_cursor);
        self.replace_preview_find_query(target, text, false);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = range_utf16
            .as_ref()
            .map(|range| {
                utf16_offset_to_char_offset(&self.preview_find_query, range.start)
                    ..utf16_offset_to_char_offset(&self.preview_find_query, range.end)
            })
            .or_else(|| self.preview_find_marked_range.clone())
            .unwrap_or(self.preview_find_cursor..self.preview_find_cursor);
        self.replace_preview_find_query(target, new_text, true);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(Bounds {
            origin: element_bounds.origin,
            size: size(px(1.), px(1.)),
        })
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(preview_find_char_range_to_utf16(
            &self.preview_find_query,
            &(self.preview_find_cursor..self.preview_find_cursor),
        )
        .start)
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let preview_focus_handle = self
            .preview_focus_handle
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        let preview_input_entity = cx.entity();
        if self.preview_visible && self.focus_preview_on_render {
            preview_focus_handle.focus(window);
            self.focus_preview_on_render = false;
        }
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

        let document_title = document_title(doc_path.as_ref(), doc_dirty);
        let window_title = format!("{document_title} — Aster");
        window.set_window_title(&window_title);

        let outline_position = settings::get_outline_position();
        let outline_toggle_color = if self.outline_visible {
            Theme::control_accent()
        } else {
            Theme::muted()
        };
        let mode_toggle_color = if self.preview_visible {
            Theme::control_accent()
        } else {
            Theme::muted()
        };
        let resize_line_color = if self.resizing_sidebar {
            gpui::rgba(0x2d7fd299)
        } else {
            Theme::border()
        };
        let top_chrome = div()
            .id("window-chrome")
            .relative()
            // Keep document content clear of the macOS window controls and provide a safe
            // drag area that contains no application controls.
            .h(px(NATIVE_TITLEBAR_HEIGHT))
            .w_full()
            .bg(Theme::bg())
            .flex_shrink_0()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, event: &MouseDownEvent, window, _| {
                if event.click_count == 1 {
                    window.start_window_move();
                }
            }),
            )
            .child(
                div()
                    .id("window-title")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(Theme::muted())
                    .child(document_title),
            );

        let floating_controls = div()
            .id("floating-view-controls")
            .absolute()
            .right(px(16.))
            .bottom(px(16.))
            .p(px(4.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .rounded(px(8.))
            .border_1()
            .border_color(Theme::border())
            .bg(Theme::panel_alt())
            .occlude()
            .child(
                div()
                    .id("floating-outline-toggle")
                    .size(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.))
                    .cursor_pointer()
                    .hover(|this| this.bg(Theme::bg()))
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
                            .child(div().w(px(5.)).h_full().bg(outline_toggle_color))
                            .child(div().flex_1()),
                    ),
            )
            .child(
                div()
                    .id("floating-preview-toggle")
                    .size(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.))
                    .cursor_pointer()
                    .hover(|this| this.bg(Theme::bg()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.toggle_preview(cx);
                        }),
                    )
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
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.handle_dropped_paths(paths, window, cx);
            }))
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
            .on_action(cx.listener(|this, _: &Find, window, cx| {
                if this.preview_visible {
                    this.activate_preview_find(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _window, cx| {
                if this.preview_visible {
                    this.select_all_preview_text(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Copy, _window, cx| {
                if this.preview_visible {
                    this.copy_preview_selection(cx);
                }
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
                        let preview_scrollbar_state = self.preview_scrollbar_state.clone();
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
                            code_syntax_theme: match Theme::name() {
                                ThemeName::AyuLight => CodeSyntaxTheme::AyuLight,
                                ThemeName::AyuDark => CodeSyntaxTheme::AyuDark,
                                ThemeName::AyuMirage => CodeSyntaxTheme::AyuMirage,
                            },
                        };
                        let mut preview_find_color: gpui::Hsla = gpui::rgb(0xffd66b).into();
                        preview_find_color.a = 0.42;
                        let mut markdown_options = self
                            .preview_markdown_options
                            .clone()
                            .with_theme(markdown_theme)
                            .with_focus_handle(preview_focus_handle.clone())
                            .with_search_highlight(
                                self.preview_find_query.clone(),
                                preview_find_color,
                            );
                        markdown_options.set_selection_color(Theme::selection_bg().into());
                        if let Some(document_dir) = doc_path
                            .as_ref()
                            .and_then(|path| path.parent())
                            .map(|path| path.as_std_path().to_path_buf())
                        {
                            let image_base = document_dir.clone();
                            markdown_options = markdown_options.with_image_loader(
                                std::sync::Arc::new(move |source| {
                                    preview_image_source(source, &image_base)
                                }),
                            );

                            let link_base = document_dir;
                            markdown_options = markdown_options.with_on_link(
                                std::sync::Arc::new(move |target, _window, cx| {
                                    open_preview_link(target, &link_base, cx);
                                }),
                            );
                        }
                        let markdown_blocks = render_markdown_blocks_cached(
                            &doc_text,
                            &markdown_options,
                            &mut self.preview_markdown_cache,
                            cx,
                        );
                        let preview_find_active = self.preview_find_active;
                        let preview_find_query = self.preview_find_query.clone();
                        let preview_find_count = markdown_options.preview_search_match_count();
                        let preview_input_focus_handle = preview_focus_handle.clone();
                        let preview_input_entity = preview_input_entity.clone();
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
                            .child(canvas(
                                |_, _window: &mut Window, _cx: &mut App| {},
                                move |bounds: Bounds<_>, (), window: &mut Window, cx: &mut App| {
                                    if preview_find_active {
                                        window.handle_input(
                                            &preview_input_focus_handle,
                                            ElementInputHandler::new(
                                                bounds,
                                                preview_input_entity.clone(),
                                            ),
                                            cx,
                                        );
                                    }
                                },
                            ))
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
                                    .track_focus(&preview_focus_handle)
                                    .on_key_down(cx.listener(
                                        move |this, event: &KeyDownEvent, window, cx| {
                                            if preview_focus_handle.is_focused(window) {
                                                this.handle_preview_find_key(event, cx);
                                            }
                                        },
                                    ))
                                    .children(markdown_blocks.elements),
                            )
                            .when(preview_find_active, |this| {
                                this.child(
                                    div()
                                        .absolute()
                                        .top(px(8.))
                                        .right(px(12.))
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .px(px(10.))
                                        .py(px(6.))
                                        .rounded(px(6.))
                                        .bg(Theme::panel_alt())
                                        .border_1()
                                        .border_color(Theme::border())
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(FontWeight::BOLD)
                                                .text_color(Theme::muted())
                                                .child("FIND"),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .max_w(px(300.))
                                                .overflow_hidden()
                                                .text_color(Theme::text())
                                                .child(if preview_find_query.is_empty() {
                                                    "Type to search".to_string()
                                                } else {
                                                    preview_find_query
                                                }),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(Theme::muted())
                                                .child(preview_find_count.to_string()),
                                        ),
                                )
                            })
                            .child(
                                render_interactive_scrollbar(
                                    InteractiveScrollbarAxis::Vertical,
                                    preview_scrollbar_state,
                                    preview_scroll_handle,
                                    Theme::muted().into(),
                                ),
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
            .child(floating_controls)
            .child(self.notifications.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::{document_title, preview_find_char_range_to_utf16, utf16_offset_to_char_offset};
    use camino::Utf8PathBuf;

    #[test]
    fn document_title_uses_file_name_and_unsaved_marker() {
        let path = Utf8PathBuf::from("/notes/project-plan.md");

        assert_eq!(document_title(Some(&path), true), "project-plan.md •");
    }

    #[test]
    fn document_title_uses_default_name_for_new_documents() {
        assert_eq!(document_title(None, false), "untitled.md");
    }

    #[test]
    fn preview_find_utf16_mapping_preserves_chinese_character_boundaries() {
        let query = "中文🙂";

        assert_eq!(utf16_offset_to_char_offset(query, 1), 1);
        assert_eq!(utf16_offset_to_char_offset(query, 2), 2);
        assert_eq!(utf16_offset_to_char_offset(query, 3), 2);
        assert_eq!(
            preview_find_char_range_to_utf16(query, &(1..3)),
            1..4
        );
    }
}
