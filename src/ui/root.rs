use crate::commands::{
    Copy, Find, FontSizeDecrease, FontSizeIncrease, FontSizeReset, NewFile, NextTab, OpenFile,
    OutlinePositionLeft, OutlinePositionRight, PreviousTab, Quit, SaveFile, SaveFileAs, SelectAll,
    ToggleLineNumbers, ToggleLiveMarkdown, ToggleOutline, TogglePreview,
};
use crate::model::document::DocumentState;
use crate::model::inline_markdown::InlineMarkdownState;
use crate::services::document_persistence::{
    DocumentSaveOutcome, DocumentSaveSnapshot, DocumentWriteReceipt, LoadedDocumentSnapshot,
    apply_document_save, read_document_snapshot, write_document_snapshot,
};
use crate::services::fs::{pick_open_path_async, pick_save_path_async};
use crate::services::inline_markdown::compute_inline_spans;
use crate::services::settings::{self, OutlinePosition, Settings};
use crate::services::tasks::Debouncer;
use crate::ui::editor::EditorView;
use crate::ui::file_explorer::{FileExplorerView, list_markdown_files};
use crate::ui::theme::{Theme, ThemeName};
use crate::ui::VIRTUALIZED_DOCUMENT_BYTES;

use camino::Utf8PathBuf;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext, Bounds, ClickEvent, ClipboardItem, Context,
    ElementInputHandler, Entity, EntityInputHandler, ExternalPaths, FocusHandle, FontWeight,
    InteractiveElement, IntoElement, KeyDownEvent, ListAlignment, ListOffset, ListState,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Render,
    ScrollHandle, StatefulInteractiveElement, Styled, UTF16Selection, Window, canvas, div, list,
    px, size,
};
use gpui_component::notification::NotificationList;
use gpui_gfm::{
    CodeSyntaxTheme, InteractiveScrollbarAxis, InteractiveScrollbarState, MarkdownRenderOptions,
    MarkdownTheme, ParsedMarkdown, parse_markdown,
    parsed_markdown_heading_indices, render_interactive_list_scrollbar,
    render_interactive_scrollbar, render_parsed_markdown_block, render_parsed_markdown_blocks,
    virtualize_parsed_markdown,
};
use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const INLINE_SYNC_PARSE_MAX_BYTES: usize = 64 * 1024;
const PREVIEW_VIRTUAL_BLOCK_MAX_BYTES: usize = 16 * 1024;
/// Height reserved for macOS native titlebar controls.
const NATIVE_TITLEBAR_HEIGHT: f32 = 38.0;
/// Width of the draggable divider rendered beside the document Outline.
const SIDEBAR_DIVIDER_WIDTH: f32 = 1.0;
/// Width kept clear for macOS traffic-light controls when no left sidebar is visible.
const TRAFFIC_LIGHT_SAFE_WIDTH: f32 = 88.0;
/// Width reserved for each interactive control in the custom title bar.
const TITLEBAR_CONTROL_WIDTH: f32 = NATIVE_TITLEBAR_HEIGHT;
/// Maximum width available to a document title inside a full-width tab.
const DOCUMENT_TAB_TITLE_MAX_WIDTH: f32 = 172.0;
/// Font size used by document titles in the custom tab bar.
const DOCUMENT_TAB_TITLE_FONT_SIZE: f32 = 14.0;

#[cfg(target_os = "macos")]
thread_local! {
    /// Retains the original mouse-down event until pointer movement confirms a window drag.
    static TITLEBAR_MOUSE_DOWN_EVENT: std::cell::RefCell<Option<objc2::rc::Retained<objc2_app_kit::NSEvent>>> =
        const { std::cell::RefCell::new(None) };
}

/// Remembers a possible native window drag without consuming a normal click or double-click.
#[cfg(target_os = "macos")]
fn prepare_titlebar_window_drag() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    let event = NSApplication::sharedApplication(main_thread).currentEvent();
    TITLEBAR_MOUSE_DOWN_EVENT.with(|stored_event| {
        *stored_event.borrow_mut() = event;
    });
}

/// Starts native movement after pointer motion confirms the user's drag intent.
#[cfg(target_os = "macos")]
fn start_titlebar_window_drag(window: &Window) {
    use objc2_app_kit::NSView;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Some(event) = TITLEBAR_MOUSE_DOWN_EVENT.with(|stored_event| stored_event.borrow_mut().take())
    else {
        return;
    };
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    let ns_view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    let Some(ns_window) = ns_view.window() else {
        return;
    };
    ns_window.performWindowDragWithEvent(&event);
}

/// Clears a pending titlebar drag when the pointer is released without moving.
#[cfg(target_os = "macos")]
fn clear_pending_titlebar_window_drag() {
    TITLEBAR_MOUSE_DOWN_EVENT.with(|stored_event| {
        stored_event.borrow_mut().take();
    });
}

/// Prepares native titlebar dragging on platforms where GPUI exposes it directly.
#[cfg(not(target_os = "macos"))]
fn prepare_titlebar_window_drag() {}

/// Starts platform-managed movement after pointer motion confirms drag intent.
#[cfg(not(target_os = "macos"))]
fn start_titlebar_window_drag(window: &Window) {
    window.start_window_move();
}

/// Clears platform-specific pending drag state when none is required.
#[cfg(not(target_os = "macos"))]
fn clear_pending_titlebar_window_drag() {}

/// Returns the active tab after closing one tab from a collection of `tab_count` tabs.
fn tab_index_after_close(tab_count: usize, closed_index: usize) -> Option<usize> {
    (tab_count > 1).then(|| closed_index.min(tab_count - 2))
}

/// Returns the next tab index, wrapping at the end of the collection.
fn next_tab_index(active_index: usize, tab_count: usize) -> usize {
    if tab_count == 0 {
        0
    } else {
        (active_index + 1) % tab_count
    }
}

/// Returns the previous tab index, wrapping at the start of the collection.
fn previous_tab_index(active_index: usize, tab_count: usize) -> usize {
    if tab_count == 0 {
        0
    } else {
        (active_index + tab_count - 1) % tab_count
    }
}

/// Moves one vector item to a target index and returns its resulting index.
fn move_vec_item<T>(items: &mut Vec<T>, from_index: usize, target_index: usize) -> Option<usize> {
    if from_index >= items.len() {
        return None;
    }
    if from_index == target_index {
        return Some(from_index);
    }

    let item = items.remove(from_index);
    let resulting_index = target_index.min(items.len());
    items.insert(resulting_index, item);
    Some(resulting_index)
}

/// Describes the gesture assigned to a mouse-down inside the empty titlebar region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlankTitlebarMouseAction {
    PrepareWindowDrag,
    PerformSystemDoubleClick,
    Ignore,
}

/// Classifies a path supplied by a picker, Finder, or drag-and-drop operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenTargetKind {
    Directory,
    MarkdownFile,
    Unsupported,
}

/// Identifies the exact close operation that should resume after an asynchronous save.
#[derive(Clone, Copy)]
enum CloseContinuation {
    Tab {
        tab_id: usize,
        window: AnyWindowHandle,
    },
    Window {
        window: AnyWindowHandle,
    },
    QuitApp,
}

/// Owns one immutable save request while it is active or waiting behind another write.
#[derive(Clone)]
struct PendingDocumentSave {
    document: Entity<DocumentState>,
    snapshot: DocumentSaveSnapshot,
    path: Utf8PathBuf,
    continuation: Option<CloseContinuation>,
}

/// Starts an idle operation immediately or coalesces waiting work to its newest request.
fn queue_latest_operation<T>(
    in_flight: &mut bool,
    pending: &mut Option<T>,
    request: T,
) -> Option<T> {
    if *in_flight {
        *pending = Some(request);
        None
    } else {
        *in_flight = true;
        Some(request)
    }
}

/// Returns whether a completed save is safe to follow with a destructive close action.
fn save_outcome_allows_close(outcome: &DocumentSaveOutcome) -> bool {
    matches!(
        outcome,
        DocumentSaveOutcome::Saved {
            document_unchanged: true
        }
    )
}

/// Returns the supported open behavior for a filesystem path.
fn classify_open_target(path: &Utf8PathBuf) -> OpenTargetKind {
    if path.is_dir() {
        OpenTargetKind::Directory
    } else if path.is_file() && is_markdown_path(path.as_std_path()) {
        OpenTargetKind::MarkdownFile
    } else {
        OpenTargetKind::Unsupported
    }
}

/// Maps the platform click sequence to native empty-titlebar behavior.
fn blank_titlebar_mouse_action(click_count: usize) -> BlankTitlebarMouseAction {
    match click_count {
        1 => BlankTitlebarMouseAction::PrepareWindowDrag,
        2 => BlankTitlebarMouseAction::PerformSystemDoubleClick,
        _ => BlankTitlebarMouseAction::Ignore,
    }
}

/// Builds an interactive titlebar surface that preserves native drag and double-click behavior.
fn titlebar_window_drag_surface(id: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .on_mouse_down(
            MouseButton::Left,
            |event: &MouseDownEvent, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
                match blank_titlebar_mouse_action(event.click_count) {
                    BlankTitlebarMouseAction::PrepareWindowDrag => {
                        prepare_titlebar_window_drag();
                    }
                    BlankTitlebarMouseAction::PerformSystemDoubleClick => {
                        clear_pending_titlebar_window_drag();
                        window.titlebar_double_click();
                    }
                    BlankTitlebarMouseAction::Ignore => {}
                }
            },
        )
        .on_mouse_move(|event: &MouseMoveEvent, window, cx| {
            if event.pressed_button == Some(MouseButton::Left) {
                cx.stop_propagation();
                start_titlebar_window_drag(window);
            }
        })
        .on_mouse_up(
            MouseButton::Left,
            |_: &MouseUpEvent, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
                clear_pending_titlebar_window_drag();
            },
        )
}

/// Returns the file name displayed in the custom title bar, including its unsaved marker.
fn document_title(path: Option<&Utf8PathBuf>, is_dirty: bool) -> String {
    let name = path
        .and_then(|path| path.file_name())
        .unwrap_or("untitled.md");
    let dirty = if is_dirty { " •" } else { "" };

    format!("{name}{dirty}")
}

/// Truncates a document title using the active UI font and appends an ellipsis when required.
fn truncated_document_title(title: &str, window: &Window) -> String {
    let text_style = window.text_style();
    let mut runs = vec![text_style.to_run(title.len())];

    window
        .text_system()
        .line_wrapper(text_style.font(), px(DOCUMENT_TAB_TITLE_FONT_SIZE))
        .truncate_line(
            title.to_string().into(),
            px(DOCUMENT_TAB_TITLE_MAX_WIDTH),
            "…",
            &mut runs,
        )
        .to_string()
}

/// Owns the independent editor entities and display mode associated with one tab.
struct DocumentTab {
    id: usize,
    document: Entity<DocumentState>,
    inline_markdown: Entity<InlineMarkdownState>,
    editor_view: Entity<EditorView>,
    file_explorer_view: Entity<FileExplorerView>,
    preview_visible: bool,
    /// Path being read for this placeholder tab, cleared when loading finishes or is abandoned.
    loading_path: Option<Utf8PathBuf>,
    /// Whether a background write currently owns this tab's persistence queue.
    save_in_flight: bool,
    /// Target currently being written, used by Save while a Save As request is still pending.
    active_save_path: Option<Utf8PathBuf>,
    /// Newest save request waiting behind the active write.
    pending_save: Option<PendingDocumentSave>,
}

/// Identifies the document tab currently participating in a GPUI drag operation.
#[derive(Clone)]
struct DocumentTabDrag {
    tab_id: usize,
    title: String,
}

/// Draws a compact label beside the pointer while a document tab is being dragged.
struct DocumentTabDragPreview {
    title: String,
}

impl Render for DocumentTabDragPreview {
    /// Renders the dragged tab title without any document content or actions.
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(220.))
            .px(px(12.))
            .py(px(7.))
            .overflow_hidden()
            .rounded(px(6.))
            .border_1()
            .border_color(Theme::border())
            .bg(Theme::panel_alt())
            .text_sm()
            .text_color(Theme::text())
            .opacity(0.88)
            .child(self.title.clone())
    }
}

pub struct RootView {
    tabs: Vec<DocumentTab>,
    active_tab_index: usize,
    next_tab_id: usize,
    document: Entity<DocumentState>,
    inline_markdown: Entity<InlineMarkdownState>,
    editor_view: Entity<crate::ui::editor::EditorView>,
    file_explorer_view: Entity<crate::ui::file_explorer::FileExplorerView>,
    /// Explicit directory used by every tab's FILES section in this window.
    workspace_directory: Option<Utf8PathBuf>,
    notifications: Entity<NotificationList>,
    inline_debounce: Debouncer<RootView>,
    /// Highest document revision for which an inline parse has been scheduled.
    scheduled_inline_revision: u64,
    /// Cached document text to avoid O(n) rope-to-string conversion every frame
    cached_doc_text: Option<(u64, Arc<str>)>,
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
    /// Variable-height list used by large Markdown previews.
    preview_list_state: ListState,
    /// Persistent gpui-gfm state so selection and interactive Markdown survive re-renders.
    preview_markdown_options: MarkdownRenderOptions,
    /// Parsed tree keyed by document revision, including sources too large for the old LRU.
    preview_parsed_markdown: Option<(u64, ParsedMarkdown)>,
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
        let initial_tab = DocumentTab {
            id: 0,
            document: document.clone(),
            inline_markdown: inline_markdown.clone(),
            editor_view: editor_view.clone(),
            file_explorer_view: file_explorer_view.clone(),
            preview_visible,
            loading_path: None,
            save_in_flight: false,
            active_save_path: None,
            pending_save: None,
        };

        Self {
            tabs: vec![initial_tab],
            active_tab_index: 0,
            next_tab_id: 1,
            document,
            inline_markdown,
            editor_view,
            file_explorer_view,
            workspace_directory: None,
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
            preview_list_state: ListState::new(0, ListAlignment::Top, px(800.)),
            preview_markdown_options: MarkdownRenderOptions::default(),
            preview_parsed_markdown: None,
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

    /// Creates the independent GPUI entities used by a new document tab.
    fn create_document_tab(
        &mut self,
        path: Option<Utf8PathBuf>,
        text: &str,
        preview_visible: bool,
        cx: &mut Context<Self>,
    ) -> DocumentTab {
        let document = cx.new(|_| DocumentState::new_empty());
        if path.is_some() || !text.is_empty() {
            let _ = document.update(cx, |document, cx| {
                document.path = path;
                document.set_text(text);
                document.clear_undo_history();
                document.save_snapshot();
                cx.notify();
            });
        }
        let inline_markdown = cx.new(|_| InlineMarkdownState::new());
        let editor_view = cx.new(|_| EditorView::new(document.clone(), inline_markdown.clone()));
        let file_explorer_view = cx.new(|_| FileExplorerView::new(document.clone()));
        if let Some(directory) = self.workspace_directory.clone() {
            let _ = file_explorer_view.update(cx, |view, cx| {
                view.set_workspace_directory(Some(directory), cx);
            });
        }
        let id = self.next_tab_id;
        self.next_tab_id = self.next_tab_id.saturating_add(1);

        DocumentTab {
            id,
            document,
            inline_markdown,
            editor_view,
            file_explorer_view,
            preview_visible,
            loading_path: None,
            save_in_flight: false,
            active_save_path: None,
            pending_save: None,
        }
    }

    /// Activates a tab and points the window's editor, preview and outline at its entities.
    fn activate_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        if index == self.active_tab_index {
            return;
        }

        if self.active_tab_index < self.tabs.len() {
            self.tabs[self.active_tab_index].preview_visible = self.preview_visible;
        }

        let tab = &self.tabs[index];
        self.active_tab_index = index;
        self.document = tab.document.clone();
        self.inline_markdown = tab.inline_markdown.clone();
        self.editor_view = tab.editor_view.clone();
        self.file_explorer_view = tab.file_explorer_view.clone();
        self.preview_visible = tab.preview_visible;
        self.reset_active_tab_render_state();
        cx.notify();
    }

    /// Clears render caches that must never be shared between document tabs.
    fn reset_active_tab_render_state(&mut self) {
        self.scheduled_inline_revision = 0;
        self.cached_doc_text = None;
        self.outline_reveal_installed = false;
        self.preview_find_active = false;
        self.preview_find_query.clear();
        self.preview_find_cursor = 0;
        self.preview_find_marked_range = None;
        self.preview_scroll_handle = ScrollHandle::new();
        self.preview_scrollbar_state = InteractiveScrollbarState::default();
        self.preview_list_state = ListState::new(0, ListAlignment::Top, px(800.));
        self.preview_markdown_options = MarkdownRenderOptions::default();
        self.preview_parsed_markdown = None;
        self.preview_heading_child_indices = Default::default();
        self.focus_preview_on_render = self.preview_visible;
    }

    /// Adds and activates a blank source-editing tab.
    fn add_blank_tab(&mut self, cx: &mut Context<Self>) {
        let tab = self.create_document_tab(None, "", false, cx);
        self.tabs.push(tab);
        self.activate_tab(self.tabs.len() - 1, cx);
    }

    /// Opens a Markdown file in a new tab, or activates its existing tab.
    fn open_path_in_tab(&mut self, path: &Utf8PathBuf, cx: &mut Context<Self>) {
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| {
                tab.document.read(cx).path.as_ref() == Some(path)
                    || tab.loading_path.as_ref() == Some(path)
            })
        {
            self.activate_tab(index, cx);
            return;
        }

        let mut tab = self.create_document_tab(None, "", settings::get_preview_visible(), cx);
        tab.loading_path = Some(path.clone());
        let tab_id = tab.id;
        let document = tab.document.clone();
        let expected_revision = document.read(cx).revision;
        self.tabs.push(tab);
        self.activate_tab(self.tabs.len() - 1, cx);
        self.spawn_document_load(tab_id, document, expected_revision, path.clone(), cx);
    }

    /// Loads a startup path into the existing initial tab without blocking window creation.
    pub fn load_initial_path(&mut self, path: Utf8PathBuf, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.first_mut() else {
            return;
        };
        if tab.document.read(cx).revision != 0 || tab.loading_path.is_some() {
            return;
        }

        tab.loading_path = Some(path.clone());
        let tab_id = tab.id;
        let document = tab.document.clone();
        let expected_revision = document.read(cx).revision;
        cx.notify();
        self.spawn_document_load(tab_id, document, expected_revision, path, cx);
    }

    /// Reads and parses a document on the background executor before applying pure loaded data.
    fn spawn_document_load(
        &mut self,
        tab_id: usize,
        document: Entity<DocumentState>,
        expected_revision: u64,
        path: Utf8PathBuf,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let path_for_read = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { read_document_snapshot(&path_for_read) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.complete_document_load(
                    tab_id,
                    &document,
                    expected_revision,
                    path,
                    result,
                    cx,
                );
            });
        })
        .detach();
    }

    /// Applies a loaded Rope only when its placeholder tab and expected revision still exist.
    fn complete_document_load(
        &mut self,
        tab_id: usize,
        document: &Entity<DocumentState>,
        expected_revision: u64,
        path: Utf8PathBuf,
        result: crate::error::AppResult<LoadedDocumentSnapshot>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.tabs.iter().position(|tab| {
            tab.id == tab_id
                && tab.document.entity_id() == document.entity_id()
                && tab.loading_path.as_ref() == Some(&path)
        }) else {
            return;
        };
        self.tabs[index].loading_path = None;

        match result {
            Ok(loaded) if document.read(cx).revision == expected_revision => {
                document.update(cx, |document, cx| {
                    document.load_rope(path.clone(), loaded.rope, loaded.content_hash);
                    cx.notify();
                });
                cx.add_recent_document(path.as_std_path());
            }
            Ok(_) => {}
            Err(error) => {
                MessageDialog::new()
                    .set_level(MessageLevel::Error)
                    .set_title("Open failed")
                    .set_description(format!("Could not open '{}'.\n\n{error}", path))
                    .set_buttons(MessageButtons::Ok)
                    .show();
            }
        }
        cx.notify();
    }

    /// Saves the active document, capturing its entity and revision before any picker opens.
    fn save_document(&mut self, cx: &mut Context<Self>, force_save_as: bool) {
        let document = self.document.clone();
        let snapshot = DocumentSaveSnapshot::capture(document.read(cx));

        let queued_path = self
            .tabs
            .iter()
            .find(|tab| tab.document.entity_id() == document.entity_id())
            .and_then(|tab| {
                tab.pending_save
                    .as_ref()
                    .map(|pending| pending.path.clone())
                    .or_else(|| tab.active_save_path.clone())
            });
        if !force_save_as
            && let Some(path) = queued_path.or_else(|| snapshot.original_path.clone())
        {
            self.enqueue_document_save(
                PendingDocumentSave {
                    document,
                    snapshot,
                    path,
                    continuation: None,
                },
                cx,
            );
            return;
        }

        self.start_save_as(document, snapshot, None, cx);
    }

    /// Normalizes a selected save path to the default Markdown extension.
    fn normalize_save_path(mut path: Utf8PathBuf) -> Utf8PathBuf {
        if path.extension().is_none() {
            path.set_extension("md");
        }
        path
    }

    /// Starts one save or replaces the waiting request with the newest immutable snapshot.
    fn enqueue_document_save(
        &mut self,
        mut request: PendingDocumentSave,
        cx: &mut Context<Self>,
    ) {
        request.path = Self::normalize_save_path(request.path);
        let Some(index) = self.tabs.iter().position(|tab| {
            tab.document.entity_id() == request.document.entity_id()
        }) else {
            return;
        };

        if request.continuation.is_none() {
            request.continuation = self.tabs[index]
                .pending_save
                .as_ref()
                .and_then(|pending| pending.continuation);
        }
        let tab = &mut self.tabs[index];
        let request_to_start = queue_latest_operation(
            &mut tab.save_in_flight,
            &mut tab.pending_save,
            request,
        );
        if let Some(request) = request_to_start.as_ref() {
            tab.active_save_path = Some(request.path.clone());
        }
        cx.notify();
        if let Some(request) = request_to_start {
            self.spawn_document_save(request, cx);
        }
    }

    /// Writes one snapshot on the background executor and serially advances its document queue.
    fn spawn_document_save(
        &mut self,
        request: PendingDocumentSave,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let path = request.path.clone();
            let snapshot = request.snapshot.clone();
            let result = cx
                .background_executor()
                .spawn(async move { write_document_snapshot(&path, &snapshot) })
                .await;
            let continuation = this
                .update(cx, |this, cx| {
                    this.complete_document_save(request, result, cx)
                })
                .ok()
                .flatten();

            let Some(continuation) = continuation else {
                return;
            };
            match continuation {
                CloseContinuation::Tab { tab_id, window } => {
                    let this = this.clone();
                    let _ = cx.update_window(window, move |_, window, cx| {
                        let _ = this.update(cx, |this, cx| {
                            this.close_tab_without_confirmation(tab_id, window, cx);
                        });
                    });
                }
                CloseContinuation::Window { window } => {
                    let this = this.clone();
                    let _ = cx.update_window(window, move |_, window, cx| {
                        let can_close = this
                            .update(cx, |this, cx| this.confirm_before_window_close(window, cx))
                            .unwrap_or(false);
                        if can_close {
                            window.remove_window();
                        }
                    });
                }
                CloseContinuation::QuitApp => {
                    let _ = cx.update(|cx| cx.dispatch_action(&Quit));
                }
            }
        })
        .detach();
    }

    /// Applies a background write, starts the newest queued save, and returns a safe close intent.
    fn complete_document_save(
        &mut self,
        request: PendingDocumentSave,
        result: crate::error::AppResult<DocumentWriteReceipt>,
        cx: &mut Context<Self>,
    ) -> Option<CloseContinuation> {
        let index = self.tabs.iter().position(|tab| {
            tab.document.entity_id() == request.document.entity_id()
        })?;

        let outcome = match result {
            Ok(receipt) => {
                let document_unchanged = request.document.update(cx, |document, cx| {
                    let unchanged = apply_document_save(
                        document,
                        request.path.clone(),
                        &request.snapshot,
                        receipt,
                    );
                    cx.notify();
                    unchanged
                });
                cx.add_recent_document(request.path.as_std_path());
                DocumentSaveOutcome::Saved { document_unchanged }
            }
            Err(error) => DocumentSaveOutcome::Failed {
                path: Some(request.path.clone()),
                message: error.to_string(),
            },
        };
        self.present_save_failure(&outcome);

        if let Some(mut pending) = self.tabs[index].pending_save.take() {
            if pending.continuation.is_none() {
                pending.continuation = request.continuation;
            }
            self.tabs[index].active_save_path = Some(pending.path.clone());
            self.spawn_document_save(pending, cx);
            cx.notify();
            return None;
        }

        self.tabs[index].save_in_flight = false;
        self.tabs[index].active_save_path = None;
        cx.notify();
        save_outcome_allows_close(&outcome)
            .then_some(request.continuation)
            .flatten()
    }

    /// Shows a visible error for a failed save while leaving the document dirty.
    fn present_save_failure(&self, outcome: &DocumentSaveOutcome) {
        let DocumentSaveOutcome::Failed { path, message } = outcome else {
            return;
        };
        let target = path
            .as_ref()
            .map(|path| format!("'{}'", path))
            .unwrap_or_else(|| "the selected file".to_string());
        MessageDialog::new()
            .set_level(MessageLevel::Error)
            .set_title("Save failed")
            .set_description(format!("Could not save {target}.\n\n{message}"))
            .set_buttons(MessageButtons::Ok)
            .show();
    }

    /// Starts Save As for a captured document and optionally resumes its close intent.
    fn start_save_as(
        &mut self,
        document: Entity<DocumentState>,
        snapshot: DocumentSaveSnapshot,
        continuation: Option<CloseContinuation>,
        cx: &mut Context<Self>,
    ) {
        let receiver = pick_save_path_async(cx, snapshot.original_path.as_ref());

        cx.spawn(async move |this, cx| {
            match receiver.await {
                Ok(Ok(Some(path))) => match Utf8PathBuf::try_from(path) {
                    Ok(path) => {
                        let _ = this.update(cx, |this, cx| {
                            this.enqueue_document_save(
                                PendingDocumentSave {
                                    document,
                                    snapshot,
                                    path,
                                    continuation,
                                },
                                cx,
                            );
                        });
                    }
                    Err(path) => {
                        let outcome = DocumentSaveOutcome::Failed {
                            path: None,
                            message: format!(
                                "The selected path is not valid UTF-8: {}",
                                path.as_path().display()
                            ),
                        };
                        let _ = this.update(cx, |this, _| this.present_save_failure(&outcome));
                    }
                },
                Ok(Ok(None)) => {
                    let outcome = DocumentSaveOutcome::Cancelled;
                    let _ = this.update(cx, |this, _| this.present_save_failure(&outcome));
                }
                Ok(Err(error)) => {
                    let outcome = DocumentSaveOutcome::Failed {
                        path: None,
                        message: error.to_string(),
                    };
                    let _ = this.update(cx, |this, _| this.present_save_failure(&outcome));
                }
                Err(error) => {
                    let outcome = DocumentSaveOutcome::Failed {
                        path: None,
                        message: error.to_string(),
                    };
                    let _ = this.update(cx, |this, _| this.present_save_failure(&outcome));
                }
            }
        })
        .detach();
    }

    /// Resolves one captured document before executing a potentially destructive close intent.
    fn confirm_can_discard_changes(
        &mut self,
        document: Entity<DocumentState>,
        cx: &mut Context<Self>,
        prompt: &str,
        continuation: CloseContinuation,
    ) -> bool {
        let is_dirty = document.read(cx).dirty;
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

        let save = |this: &mut Self, cx: &mut Context<Self>| -> bool {
            let snapshot = DocumentSaveSnapshot::capture(document.read(cx));
            let queued_path = this
                .tabs
                .iter()
                .find(|tab| tab.document.entity_id() == document.entity_id())
                .and_then(|tab| {
                    tab.pending_save
                        .as_ref()
                        .map(|pending| pending.path.clone())
                        .or_else(|| tab.active_save_path.clone())
                });
            if let Some(path) = queued_path.or_else(|| snapshot.original_path.clone()) {
                this.enqueue_document_save(
                    PendingDocumentSave {
                        document,
                        snapshot,
                        path,
                        continuation: Some(continuation),
                    },
                    cx,
                );
                false
            } else {
                this.start_save_as(document, snapshot, Some(continuation), cx);
                false
            }
        };

        match choice {
            MessageDialogResult::Ok | MessageDialogResult::Yes => save(self, cx),
            MessageDialogResult::No => true,
            MessageDialogResult::Custom(label) => match label.as_str() {
                "Save" => save(self, cx),
                "Don't Save" => true,
                _ => false,
            },
            _ => false,
        }
    }

    /// Opens a directory as the window's FILES context and selects its first Markdown file.
    fn open_directory(&mut self, directory: &Utf8PathBuf, cx: &mut Context<Self>) {
        self.workspace_directory = Some(directory.clone());
        for tab in &self.tabs {
            let directory = directory.clone();
            let _ = tab.file_explorer_view.update(cx, |view, cx| {
                view.set_workspace_directory(Some(directory), cx);
            });
        }

        if let Some(first_file) = list_markdown_files(directory).into_iter().next() {
            self.open_path_in_tab(&first_file, cx);
        } else {
            cx.notify();
        }
    }

    /// Opens a supported file or folder without requiring window access.
    fn open_path_internal(&mut self, path: &camino::Utf8PathBuf, cx: &mut Context<Self>) {
        match classify_open_target(path) {
            OpenTargetKind::Directory => self.open_directory(path, cx),
            OpenTargetKind::MarkdownFile => self.open_path_in_tab(path, cx),
            OpenTargetKind::Unsupported => {}
        }
    }

    /// Opens a wiki-link target or reveals a footnote/heading in the active Markdown document.
    fn open_complex_markdown_target(&mut self, target: &str, cx: &mut Context<Self>) {
        if let Some(label) = target.strip_prefix('^') {
            self.reveal_markdown_anchor(&format!("^{label}"), cx);
            return;
        }
        if url::Url::parse(target).is_ok() || target.starts_with("//") {
            cx.open_url(target);
            return;
        }

        let (file_target, anchor) = target
            .split_once('#')
            .map_or((target, None), |(file, anchor)| (file, Some(anchor)));
        if !file_target.trim().is_empty() {
            let current_directory = self
                .document
                .read(cx)
                .path
                .as_ref()
                .and_then(|path| path.parent())
                .map(ToOwned::to_owned)
                .or_else(|| self.workspace_directory.clone());
            if let Some(directory) = current_directory {
                let mut path = directory.join(file_target.trim());
                if path.extension().is_none() {
                    path.set_extension("md");
                }
                if path.is_file() {
                    self.open_path_in_tab(&path, cx);
                } else {
                    return;
                }
            }
        }
        if let Some(anchor) = anchor.filter(|anchor| !anchor.is_empty()) {
            self.reveal_markdown_anchor(anchor, cx);
        }
    }

    /// Reveals an ATX heading or footnote definition without changing document source.
    fn reveal_markdown_anchor(&mut self, anchor: &str, cx: &mut Context<Self>) {
        let source = self.document.read(cx).text();
        let normalized_anchor = normalize_markdown_anchor(anchor.trim_start_matches('#'));
        let mut offset = 0usize;
        let target_byte = source.split_inclusive('\n').find_map(|raw_line| {
            let line = raw_line.trim_end_matches(['\n', '\r']).trim_start();
            let heading = line
                .strip_prefix('#')
                .map(|rest| rest.trim_start_matches('#').trim());
            let footnote = line
                .strip_prefix("[^")
                .and_then(|rest| rest.split_once("]:").map(|(label, _)| format!("^{label}")));
            let matches = heading
                .is_some_and(|title| normalize_markdown_anchor(title) == normalized_anchor)
                || footnote
                    .as_deref()
                    .is_some_and(|label| normalize_markdown_anchor(label) == normalized_anchor);
            let current = offset;
            offset += raw_line.len();
            matches.then_some(current)
        });
        let Some(target_byte) = target_byte else {
            return;
        };
        let _ = self.document.update(cx, |doc, cx| {
            doc.set_cursor(doc.byte_to_char(target_byte));
            cx.notify();
        });
        let _ = self.editor_view.update(cx, |editor, cx| {
            editor.reveal_outline(target_byte, cx);
        });
    }

    /// Creates a blank source-editing tab without disturbing open documents.
    fn action_new_file(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.add_blank_tab(cx);
    }

    /// Opens the native file-or-folder picker and handles its selected target.
    fn action_open_file(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let picker = pick_open_path_async(cx);
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picker.await
                && let Some(path) = paths.into_iter().next()
                && let Ok(utf8_path) = Utf8PathBuf::try_from(path)
            {
                let _ = this.update(&mut *cx, |this, cx| {
                    this.open_path_internal(&utf8_path, cx);
                });
            }
        })
        .detach();
    }

    /// Opens an externally supplied Markdown path using the same tab de-duplication rules.
    pub fn action_open_path(
        &mut self,
        path: camino::Utf8PathBuf,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_path_internal(&path, cx);
    }

    fn handle_dropped_paths(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(directory) = paths.paths().iter().find(|path| path.is_dir()) {
            if let Ok(path) = Utf8PathBuf::try_from(directory.clone()) {
                self.action_open_path(path, window, cx);
            }
            return;
        }

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

    /// Resolves every dirty tab before allowing its window or the application to close.
    pub fn confirm_before_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let documents = self
            .tabs
            .iter()
            .map(|tab| tab.document.clone())
            .collect::<Vec<_>>();
        for document in documents {
            let title = document_title(
                document.read(cx).path.as_ref(),
                document.read(cx).dirty,
            );
            if !self.confirm_can_discard_changes(
                document,
                cx,
                &format!("Save changes to {title} before quitting?"),
                CloseContinuation::QuitApp,
            ) {
                return false;
            }
        }
        self.persist_window_bounds(window);
        true
    }

    /// Resolves every dirty tab before allowing one native window to close.
    pub fn confirm_before_window_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let window_handle = window.window_handle();
        let documents = self
            .tabs
            .iter()
            .map(|tab| tab.document.clone())
            .collect::<Vec<_>>();
        for document in documents {
            let title = document_title(
                document.read(cx).path.as_ref(),
                document.read(cx).dirty,
            );
            if !self.confirm_can_discard_changes(
                document,
                cx,
                &format!("Save changes to {title} before closing?"),
                CloseContinuation::Window {
                    window: window_handle,
                },
            ) {
                return false;
            }
        }
        self.persist_window_bounds(window);
        true
    }

    fn action_save(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.save_document(cx, false);
    }

    fn action_save_as(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.save_document(cx, true);
    }

    /// Handles the Close Tab command for the active document.
    pub fn action_close_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab(self.active_tab_index, window, cx);
    }

    /// Closes one tab after resolving its unsaved document, closing the window if it was last.
    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }

        self.activate_tab(index, cx);
        let tab_id = self.tabs[index].id;
        let document = self.tabs[index].document.clone();
        let title = document_title(
            document.read(cx).path.as_ref(),
            document.read(cx).dirty,
        );
        if !self.confirm_can_discard_changes(
            document,
            cx,
            &format!("Save changes to {title} before closing?"),
            CloseContinuation::Tab {
                tab_id,
                window: window.window_handle(),
            },
        ) {
            return;
        }

        self.close_tab_without_confirmation(tab_id, window, cx);
    }

    /// Closes the exact tab named by an earlier close intent without prompting again.
    fn close_tab_without_confirmation(
        &mut self,
        tab_id: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };

        if self.tabs.len() == 1 {
            self.persist_window_bounds(window);
            window.remove_window();
            return;
        }

        let active_tab_id = self.tabs.get(self.active_tab_index).map(|tab| tab.id);
        let fallback_index = tab_index_after_close(self.tabs.len(), index)
            .expect("multiple tabs always have a successor");

        self.tabs.remove(index);
        let next_index = active_tab_id
            .filter(|active_tab_id| *active_tab_id != tab_id)
            .and_then(|active_tab_id| self.tabs.iter().position(|tab| tab.id == active_tab_id))
            .unwrap_or(fallback_index);
        self.active_tab_index = usize::MAX;
        self.activate_tab(next_index, cx);
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

    /// Switches the active editor between Source and Live Markdown presentation.
    fn toggle_live_markdown(&mut self, cx: &mut Context<Self>) {
        let enabled = !self.editor_view.read(cx).live_markdown_enabled();
        settings::set_live_markdown_enabled(enabled);
        let _ = self.editor_view.update(cx, |editor, cx| {
            editor.set_live_markdown_enabled(enabled, cx);
        });
        cx.notify();
    }

    /// Toggles source line numbers for every editor tab without changing document content.
    fn toggle_line_numbers(&mut self, cx: &mut Context<Self>) {
        let visible = !settings::get_line_numbers_visible();
        settings::set_line_numbers_visible(visible);
        for tab in &self.tabs {
            let _ = tab.editor_view.update(cx, |_editor, cx| cx.notify());
        }
        cx.notify();
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

    /// Activates the next tab in document order, wrapping at the end.
    fn activate_next_tab(&mut self, cx: &mut Context<Self>) {
        self.activate_tab(next_tab_index(self.active_tab_index, self.tabs.len()), cx);
    }

    /// Activates the previous tab in document order, wrapping at the start.
    fn activate_previous_tab(&mut self, cx: &mut Context<Self>) {
        self.activate_tab(previous_tab_index(self.active_tab_index, self.tabs.len()), cx);
    }

    /// Moves a document tab to a drop target while preserving the active document entity.
    fn move_tab_to_index(
        &mut self,
        dragged_tab_id: usize,
        target_index: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(from_index) = self
            .tabs
            .iter()
            .position(|tab| tab.id == dragged_tab_id)
        else {
            return;
        };
        let active_tab_id = self.tabs.get(self.active_tab_index).map(|tab| tab.id);
        if move_vec_item(&mut self.tabs, from_index, target_index).is_none() {
            return;
        }
        if let Some(active_tab_id) = active_tab_id
            && let Some(active_index) = self.tabs.iter().position(|tab| tab.id == active_tab_id)
        {
            self.active_tab_index = active_index;
        }
        cx.notify();
    }

    /// Renders the Outline visibility control inside the custom title bar.
    fn render_titlebar_outline_toggle(
        &self,
        outline_position: OutlinePosition,
        outline_toggle_color: gpui::Rgba,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("titlebar-outline-toggle")
            .absolute()
            .top_0()
            .size(px(TITLEBAR_CONTROL_WIDTH))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|this| this.bg(Theme::panel()))
            .when(outline_position == OutlinePosition::Left, |this| {
                this.left(px(TRAFFIC_LIGHT_SAFE_WIDTH))
            })
            .when(outline_position == OutlinePosition::Right, |this| {
                this.right_0()
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    window.prevent_default();
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
            )
            .into_any_element()
    }

    /// Renders the editor and preview mode control inside the custom title bar.
    fn render_titlebar_mode_toggle(
        &self,
        outline_position: OutlinePosition,
        has_right_sidebar: bool,
        mode_toggle_color: gpui::Rgba,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let right_offset = if outline_position == OutlinePosition::Right {
            if has_right_sidebar {
                self.sidebar_width + SIDEBAR_DIVIDER_WIDTH
            } else {
                TITLEBAR_CONTROL_WIDTH
            }
        } else {
            0.0
        };

        div()
            .id("titlebar-preview-toggle")
            .absolute()
            .top_0()
            .right(px(right_offset))
            .size(px(TITLEBAR_CONTROL_WIDTH))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|this| this.bg(Theme::panel()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    window.prevent_default();
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
            )
            .into_any_element()
    }

    /// Renders the interactive document tabs and new-tab button in the custom title bar.
    fn render_tab_bar(
        &self,
        outline_position: OutlinePosition,
        has_left_sidebar: bool,
        has_right_sidebar: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let left_inset = if outline_position == OutlinePosition::Left {
            if has_left_sidebar {
                (self.sidebar_width + SIDEBAR_DIVIDER_WIDTH)
                    .max(TRAFFIC_LIGHT_SAFE_WIDTH + TITLEBAR_CONTROL_WIDTH)
            } else {
                TRAFFIC_LIGHT_SAFE_WIDTH + TITLEBAR_CONTROL_WIDTH
            }
        } else {
            TRAFFIC_LIGHT_SAFE_WIDTH
        };
        let right_inset = TITLEBAR_CONTROL_WIDTH
            + if outline_position == OutlinePosition::Right {
                if has_right_sidebar {
                    self.sidebar_width + SIDEBAR_DIVIDER_WIDTH
                } else {
                    TITLEBAR_CONTROL_WIDTH
                }
            } else {
                0.0
            };
        let tab_snapshots = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let document = tab.document.read(cx);
                let mut title = tab.loading_path.as_ref().map_or_else(
                    || document_title(document.path.as_ref(), document.dirty),
                    |path| format!("{} — Opening…", path.file_name().unwrap_or("document.md")),
                );
                if tab.save_in_flight {
                    title.push_str(" — Saving…");
                }
                (
                    index,
                    tab.id,
                    title,
                )
            })
            .collect::<Vec<_>>();

        let tabs = tab_snapshots.into_iter().map(|(index, id, title)| {
            let is_active = index == self.active_tab_index;
            let display_title = truncated_document_title(&title, window);
            let drag = DocumentTabDrag {
                tab_id: id,
                title: title.clone(),
            };
            div()
                .id(("document-tab", id))
                .group("document-tab")
                .h_full()
                .min_w(px(132.))
                .max_w(px(220.))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.))
                .pl(px(12.))
                .pr(px(8.))
                .border_r_1()
                .border_color(Theme::border())
                .text_sm()
                .text_color(if is_active { Theme::text() } else { Theme::muted() })
                .when(is_active, |this| this.bg(Theme::bg()))
                .when(!is_active, |this| {
                    this.bg(Theme::panel_alt()).hover(|style| style.bg(Theme::panel()))
                })
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                    this.activate_tab(index, cx);
                }))
                .on_drag(drag, |drag, _, _, cx| {
                    cx.stop_propagation();
                    cx.new(|_| DocumentTabDragPreview {
                        title: drag.title.clone(),
                    })
                })
                .drag_over::<DocumentTabDrag>(move |style, drag, _, _| {
                    if drag.tab_id == id {
                        style
                    } else {
                        style
                            .border_l_2()
                            .border_color(Theme::control_accent())
                    }
                })
                .on_drop(cx.listener(
                    move |this, drag: &DocumentTabDrag, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                        this.move_tab_to_index(drag.tab_id, index, cx);
                    },
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .child(display_title),
                )
                .child(
                    div()
                        .id(("close-document-tab", id))
                        .size(px(20.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .text_sm()
                        .text_color(Theme::muted())
                        .when(!is_active, |this| {
                            this.invisible()
                                .group_hover("document-tab", |style| style.visible())
                        })
                        .hover(|style| style.bg(Theme::panel()))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                window.prevent_default();
                                cx.stop_propagation();
                                if event.click_count == 1 {
                                    this.close_tab(index, window, cx);
                                }
                            }),
                        )
                        .child("×"),
                )
        });

        div()
            .id("document-tab-bar")
            .h_full()
            .min_w(px(0.))
            .flex()
            .items_center()
            .overflow_x_scroll()
            .ml(px(left_inset))
            .mr(px(right_inset))
            .children(tabs)
            .child(
                div()
                    .id("new-document-tab")
                    .size(px(NATIVE_TITLEBAR_HEIGHT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xl()
                    .text_color(Theme::muted())
                    .cursor_pointer()
                    .hover(|style| style.bg(Theme::panel()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                            if event.click_count == 1 {
                                this.add_blank_tab(cx);
                            }
                        }),
                    )
                    .child("+"),
            )
            .child(
                titlebar_window_drag_surface("titlebar-window-drag-region")
                    .h_full()
                    .min_w(px(24.))
                    .flex_1()
                    .drag_over::<DocumentTabDrag>(|style, _, _, _| {
                        style.bg(Theme::panel())
                    })
                    .on_drop(cx.listener(
                        |this, drag: &DocumentTabDrag, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                            this.move_tab_to_index(drag.tab_id, usize::MAX, cx);
                        },
                    )),
            )
            .into_any_element()
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

/// Normalizes heading text to the anchor form used by wiki-link navigation.
fn normalize_markdown_anchor(value: &str) -> String {
    value
        .trim()
        .trim_matches('#')
        .chars()
        .flat_map(char::to_lowercase)
        .map(|character| {
            if character.is_whitespace() || character == '_' {
                '-'
            } else {
                character
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
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

/// Counts non-overlapping ASCII-case-insensitive matches without allocating lowercase copies.
fn preview_find_match_count(text: &str, query: &str) -> usize {
    if query.is_empty() || query.len() > text.len() {
        return 0;
    }
    let query = query.as_bytes();
    let bytes = text.as_bytes();
    let mut count = 0usize;
    let mut start = 0usize;
    while start + query.len() <= bytes.len() {
        let end = start + query.len();
        if text.is_char_boundary(start)
            && text.is_char_boundary(end)
            && bytes[start..end]
                .iter()
                .map(u8::to_ascii_lowercase)
                .eq(query.iter().map(u8::to_ascii_lowercase))
        {
            count += 1;
            start = end;
        } else {
            start += 1;
        }
    }
    count
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
        // Install the Outline reveal callback once. Small previews use direct-child
        // bounds; virtual previews jump to the heading's top-level block index.
        if !self.outline_reveal_installed {
            self.outline_reveal_installed = true;
            let editor = self.editor_view.clone();
            let preview_scroll = self.preview_scroll_handle.clone();
            let preview_list = self.preview_list_state.clone();
            let preview_heading_indices = self.preview_heading_child_indices.clone();
            let callback: crate::ui::file_explorer::RevealCallback =
                std::sync::Arc::new(move |heading_ordinal, byte_start, cx: &mut App| {
                    let _ = editor.update(cx, |editor, cx| editor.reveal_outline(byte_start, cx));

                    let child_index = preview_heading_indices
                        .lock()
                        .ok()
                        .and_then(|indices| indices.get(heading_ordinal).copied());
                    if let Some(child_index) = child_index {
                        preview_list.scroll_to(ListOffset {
                            item_ix: child_index,
                            offset_in_item: px(0.),
                        });
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

            let root = cx.weak_entity();
            let open_file_callback: crate::ui::file_explorer::OpenFileCallback =
                std::sync::Arc::new(move |path, cx: &mut App| {
                    let _ = root.update(cx, |root, cx| {
                        root.open_path_internal(&path, cx);
                    });
                });
            let _ = self
                .file_explorer_view
                .update(cx, |view, _| view.set_on_open_file(open_file_callback));

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

            let root = cx.weak_entity();
            let complex_link_callback: crate::ui::editor::ComplexLinkCallback =
                std::sync::Arc::new(move |target, cx: &mut App| {
                    let _ = root.update(cx, |root, cx| {
                        root.open_complex_markdown_target(&target, cx);
                    });
                });
            let _ = self.editor_view.update(cx, |view, _| {
                view.set_on_complex_link(complex_link_callback);
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
                let text = Arc::<str>::from(self.document.read(cx).text());
                self.cached_doc_text = Some((doc_revision, text.clone()));
                text
            }
        } else {
            let text = Arc::<str>::from(self.document.read(cx).text());
            self.cached_doc_text = Some((doc_revision, text.clone()));
            text
        };
        if !self.preview_visible && self.preview_parsed_markdown.take().is_some() {
            // A hidden preview must not pin the parsed tree, list measurements, or the
            // selectable-text registry while the editor is showing the same large source.
            self.preview_list_state.reset(0);
            self.preview_markdown_options = MarkdownRenderOptions::default();
            if let Ok(mut indices) = self.preview_heading_child_indices.lock() {
                indices.clear();
            }
        }
        let inline_rev = self.inline_markdown.read(cx).source_revision;

        if doc_revision != inline_rev && self.scheduled_inline_revision < doc_revision {
            self.scheduled_inline_revision = doc_revision;
            let last_edit = self.document.read(cx).last_edit.clone();
            let target_rev = doc_revision;
            if doc_text.len() > VIRTUALIZED_DOCUMENT_BYTES {
                // The large editor intentionally renders literal source lines. Publishing an
                // empty, revision-current result prevents a redundant whole-document inline
                // parse and keeps stale ranges out of the editor.
                let _ = self.inline_markdown.update(cx, |state, cx| {
                    state.spans = Default::default();
                    state.components = Default::default();
                    state.source_revision = target_rev;
                    state.parse_millis = 0;
                    cx.notify();
                });
            } else if doc_text.len() <= INLINE_SYNC_PARSE_MAX_BYTES {
                // Small/medium notes: parse inline to avoid style flicker between keystrokes.
                let parsed = compute_inline_spans(&doc_text, last_edit.as_ref());
                let _ = self.inline_markdown.update(cx, |state, cx| {
                    if target_rev >= state.source_revision {
                        state.spans = std::sync::Arc::new(parsed.spans);
                        state.components = std::sync::Arc::new(parsed.components);
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
                                state.components = std::sync::Arc::new(parsed.components);
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
            Theme::navigation_accent()
        } else {
            Theme::muted()
        };
        let mode_toggle_color = if self.preview_visible {
            Theme::navigation_accent()
        } else {
            Theme::muted()
        };
        let resize_line_color = if self.resizing_sidebar {
            gpui::rgba(0x2d7fd299)
        } else {
            Theme::border()
        };
        let has_left_sidebar = self.outline_visible && outline_position == OutlinePosition::Left;
        let has_right_sidebar = self.outline_visible && outline_position == OutlinePosition::Right;
        let tab_bar = self.render_tab_bar(
            outline_position,
            has_left_sidebar,
            has_right_sidebar,
            window,
            cx,
        );
        let outline_toggle =
            self.render_titlebar_outline_toggle(outline_position, outline_toggle_color, cx);
        let mode_toggle = self.render_titlebar_mode_toggle(
            outline_position,
            has_right_sidebar,
            mode_toggle_color,
            cx,
        );
        let top_chrome = div()
            .id("window-chrome")
            .relative()
            .h(px(NATIVE_TITLEBAR_HEIGHT))
            .w_full()
            .bg(Theme::panel_alt())
            .border_b_1()
            .border_color(Theme::border())
            .flex_shrink_0()
            .when(has_left_sidebar, |this| {
                this.child(
                    titlebar_window_drag_surface("left-sidebar-titlebar-background")
                        .absolute()
                        .top_0()
                        .left_0()
                        .h_full()
                        .w(px(self.sidebar_width + SIDEBAR_DIVIDER_WIDTH))
                        .bg(Theme::sidebar())
                        .border_r_1()
                        .border_color(Theme::border()),
                )
            })
            .when(has_right_sidebar, |this| {
                this.child(
                    div()
                        .id("right-sidebar-titlebar-background")
                        .absolute()
                        .top_0()
                        .right_0()
                        .h_full()
                        .w(px(self.sidebar_width + SIDEBAR_DIVIDER_WIDTH))
                        .bg(Theme::sidebar())
                        .border_l_1()
                        .border_color(Theme::border()),
                )
            })
            .child(tab_bar)
            .child(outline_toggle)
            .child(mode_toggle);

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
            .on_action(cx.listener(|this, _: &NextTab, _window, cx| {
                this.activate_next_tab(cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousTab, _window, cx| {
                this.activate_previous_tab(cx);
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
            .on_action(cx.listener(|this, _: &ToggleLiveMarkdown, _window, cx| {
                this.toggle_live_markdown(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleLineNumbers, _window, cx| {
                this.toggle_line_numbers(cx);
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
                                .w(px(SIDEBAR_DIVIDER_WIDTH))
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
                        let preview_virtualized = doc_text.len() > VIRTUALIZED_DOCUMENT_BYTES;
                        let parsed_changed = self
                            .preview_parsed_markdown
                            .as_ref()
                            .is_none_or(|(revision, _)| *revision != doc_revision);
                        if parsed_changed {
                            let parsed = parse_markdown(doc_text.as_ref());
                            let parsed = if preview_virtualized {
                                virtualize_parsed_markdown(
                                    &parsed,
                                    PREVIEW_VIRTUAL_BLOCK_MAX_BYTES,
                                )
                            } else {
                                parsed
                            };
                            self.preview_parsed_markdown = Some((doc_revision, parsed));
                        }
                        let parsed = self
                            .preview_parsed_markdown
                            .as_ref()
                            .map(|(_, parsed)| parsed.clone())
                            .unwrap_or_else(|| parse_markdown(""));
                        let heading_child_indices = parsed_markdown_heading_indices(&parsed);
                        if let Ok(mut indices) = self.preview_heading_child_indices.lock() {
                            *indices = heading_child_indices.clone();
                        }

                        let (preview_content, preview_scrollbar) = if preview_virtualized {
                            if parsed_changed {
                                let block_count = parsed.blocks().len();
                                self.preview_list_state.reset(block_count);
                            }
                            let explorer = self.file_explorer_view.clone();
                            let headings = heading_child_indices.clone();
                            self.preview_list_state.set_scroll_handler(move |event, _, cx| {
                                let first_visible = event.visible_range.start;
                                let active = (!headings.is_empty()).then(|| {
                                    headings
                                        .partition_point(|block| *block <= first_visible)
                                        .saturating_sub(1)
                                });
                                let _ = explorer.update(cx, |view, cx| {
                                    view.set_active_outline(active, cx);
                                });
                            });

                            let list_parsed = parsed.clone();
                            let list_options = markdown_options.clone();
                            let content = div()
                                .id("preview-virtual-scroll")
                                .size_full()
                                .overflow_hidden()
                                .track_focus(&preview_focus_handle)
                                .on_key_down(cx.listener({
                                    let preview_focus_handle = preview_focus_handle.clone();
                                    move |this, event: &KeyDownEvent, window, cx| {
                                        if preview_focus_handle.is_focused(window) {
                                            this.handle_preview_find_key(event, cx);
                                        }
                                    }
                                }))
                                .child(
                                    list(self.preview_list_state.clone(), move |index, _, cx| {
                                        render_parsed_markdown_block(
                                            &list_parsed,
                                            index,
                                            &list_options,
                                            cx,
                                        )
                                    })
                                    .size_full()
                                    .pl(px(32.))
                                    .pr(px(32.))
                                    .py(px(24.)),
                                )
                                .into_any_element();
                            let scrollbar = render_interactive_list_scrollbar(
                                preview_scrollbar_state,
                                self.preview_list_state.clone(),
                                Theme::muted().into(),
                            );
                            (content, scrollbar)
                        } else {
                            let markdown_blocks =
                                render_parsed_markdown_blocks(&parsed, &markdown_options, cx);
                            self.sync_preview_outline_active(
                                &markdown_blocks.heading_child_indices,
                                cx,
                            );
                            let content = div()
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
                                .on_key_down(cx.listener({
                                    let preview_focus_handle = preview_focus_handle.clone();
                                    move |this, event: &KeyDownEvent, window, cx| {
                                        if preview_focus_handle.is_focused(window) {
                                            this.handle_preview_find_key(event, cx);
                                        }
                                    }
                                }))
                                .children(markdown_blocks.elements)
                                .into_any_element();
                            let scrollbar = render_interactive_scrollbar(
                                InteractiveScrollbarAxis::Vertical,
                                preview_scrollbar_state,
                                preview_scroll_handle,
                                Theme::muted().into(),
                            );
                            (content, scrollbar)
                        };
                        let preview_find_active = self.preview_find_active;
                        let preview_find_query = self.preview_find_query.clone();
                        let preview_find_count = if preview_virtualized {
                            preview_find_match_count(doc_text.as_ref(), &self.preview_find_query)
                        } else {
                            markdown_options.preview_search_match_count()
                        };
                        let preview_input_focus_handle = preview_focus_handle.clone();
                        let preview_input_entity = preview_input_entity.clone();
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
                            .child(preview_content)
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
                            .child(preview_scrollbar)
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

#[cfg(test)]
mod tests {
    use super::{
        BlankTitlebarMouseAction, OpenTargetKind, RootView, blank_titlebar_mouse_action,
        classify_open_target, document_title, move_vec_item, next_tab_index, previous_tab_index,
        normalize_markdown_anchor, preview_find_char_range_to_utf16, preview_find_match_count,
        queue_latest_operation, save_outcome_allows_close, tab_index_after_close,
        utf16_offset_to_char_offset,
    };
    use crate::model::document::DocumentState;
    use crate::model::inline_markdown::InlineMarkdownState;
    use crate::services::document_persistence::DocumentSaveOutcome;
    use crate::ui::editor::EditorView;
    use crate::ui::file_explorer::FileExplorerView;
    use camino::Utf8PathBuf;
    use gpui::{AppContext, Context, TestAppContext, Window};
    use gpui_component::notification::NotificationList;

    #[test]
    fn large_preview_find_count_is_all_document_and_non_overlapping() {
        assert_eq!(preview_find_match_count("Hello hello HELLO", "hello"), 3);
        assert_eq!(preview_find_match_count("aaaa", "aa"), 2);
        assert_eq!(preview_find_match_count("中文中文", "中文"), 2);
    }

    /// Builds a real root view for deterministic foreground/background persistence tests.
    fn build_test_root(window: &mut Window, cx: &mut Context<RootView>) -> RootView {
        let document = cx.new(|_| DocumentState::new_empty());
        let inline_markdown = cx.new(|_| InlineMarkdownState::new());
        let editor_view = cx.new(|_| EditorView::new(document.clone(), inline_markdown.clone()));
        let file_explorer_view = cx.new(|_| FileExplorerView::new(document.clone()));
        let notifications = cx.new(|cx| NotificationList::new(window, cx));
        RootView::new(
            document,
            inline_markdown,
            editor_view,
            file_explorer_view,
            notifications,
            false,
        )
    }

    #[test]
    fn document_title_uses_file_name_and_unsaved_marker() {
        let path = Utf8PathBuf::from("/notes/project-plan.md");

        assert_eq!(document_title(Some(&path), true), "project-plan.md •");
    }

    #[test]
    fn classifies_folder_markdown_and_unsupported_open_targets() {
        let directory = tempfile::tempdir().unwrap();
        let markdown_path = directory.path().join("notes.md");
        let text_path = directory.path().join("notes.txt");
        std::fs::write(&markdown_path, "# Notes").unwrap();
        std::fs::write(&text_path, "Notes").unwrap();

        assert_eq!(
            classify_open_target(&Utf8PathBuf::from_path_buf(directory.path().to_path_buf()).unwrap()),
            OpenTargetKind::Directory
        );
        assert_eq!(
            classify_open_target(&Utf8PathBuf::from_path_buf(markdown_path).unwrap()),
            OpenTargetKind::MarkdownFile
        );
        assert_eq!(
            classify_open_target(&Utf8PathBuf::from_path_buf(text_path).unwrap()),
            OpenTargetKind::Unsupported
        );
    }

    #[test]
    fn closing_a_tab_selects_the_next_or_previous_neighbor() {
        assert_eq!(tab_index_after_close(3, 0), Some(0));
        assert_eq!(tab_index_after_close(3, 1), Some(1));
        assert_eq!(tab_index_after_close(3, 2), Some(1));
        assert_eq!(tab_index_after_close(1, 0), None);
    }

    #[test]
    fn close_continues_only_after_saving_an_unchanged_snapshot() {
        assert!(save_outcome_allows_close(&DocumentSaveOutcome::Saved {
            document_unchanged: true,
        }));
        assert!(!save_outcome_allows_close(&DocumentSaveOutcome::Saved {
            document_unchanged: false,
        }));
        assert!(!save_outcome_allows_close(&DocumentSaveOutcome::Cancelled));
        assert!(!save_outcome_allows_close(&DocumentSaveOutcome::Failed {
            path: None,
            message: "permission denied".to_string(),
        }));
    }

    #[test]
    fn save_queue_runs_one_request_and_coalesces_waiting_work_to_the_latest() {
        let mut in_flight = false;
        let mut pending = None;

        assert_eq!(
            queue_latest_operation(&mut in_flight, &mut pending, 1),
            Some(1)
        );
        assert_eq!(
            queue_latest_operation(&mut in_flight, &mut pending, 2),
            None
        );
        assert_eq!(
            queue_latest_operation(&mut in_flight, &mut pending, 3),
            None
        );
        assert_eq!((in_flight, pending), (true, Some(3)));
    }

    #[gpui::test]
    fn queued_background_saves_leave_the_newest_snapshot_on_disk(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let directory = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(directory.path().join("queued-save.md")).unwrap();
        let (root, cx) = cx.add_window_view(build_test_root);

        root.update(cx, |root, cx| {
            let document = root.document.clone();
            document.update(cx, |document, _| {
                document.path = Some(path.clone());
                document.insert(0, "older");
            });
            root.save_document(cx, false);
            document.update(cx, |document, _| {
                document.insert(document.len_chars(), " newer");
            });
            root.save_document(cx, false);

            assert!(root.tabs[0].save_in_flight);
            assert!(root.tabs[0].pending_save.is_some());
        });
        cx.run_until_parked();

        assert_eq!(std::fs::read_to_string(path).unwrap(), "older newer");
        root.update(cx, |root, cx| {
            assert!(!root.tabs[0].save_in_flight);
            assert!(root.tabs[0].pending_save.is_none());
            assert!(!root.document.read(cx).dirty);
        });
    }

    #[gpui::test]
    fn closing_a_loading_tab_discards_its_background_result(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let directory = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(directory.path().join("large.md")).unwrap();
        std::fs::write(&path, "0123456789abcdef\n".repeat(640 * 1024)).unwrap();
        let (root, cx) = cx.add_window_view(build_test_root);

        cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                root.open_path_in_tab(&path, cx);
                let loading_tab_id = root.tabs[root.active_tab_index].id;
                root.add_blank_tab(cx);
                root.close_tab_without_confirmation(loading_tab_id, window, cx);
                assert_eq!(root.tabs.len(), 2);
            });
        });
        cx.run_until_parked();

        root.update(cx, |root, cx| {
            assert_eq!(root.tabs.len(), 2);
            assert!(root.tabs.iter().all(|tab| {
                tab.loading_path.as_ref() != Some(&path)
                    && tab.document.read(cx).path.as_ref() != Some(&path)
            }));
        });
    }

    #[test]
    fn tab_navigation_wraps_in_both_directions() {
        assert_eq!(next_tab_index(0, 3), 1);
        assert_eq!(next_tab_index(2, 3), 0);
        assert_eq!(previous_tab_index(0, 3), 2);
        assert_eq!(previous_tab_index(2, 3), 1);
    }

    #[test]
    fn moving_a_tab_preserves_expected_document_order() {
        let mut tabs = vec![0, 1, 2, 3];

        assert_eq!(move_vec_item(&mut tabs, 0, 2), Some(2));
        assert_eq!(tabs, vec![1, 2, 0, 3]);
        assert_eq!(move_vec_item(&mut tabs, 3, 0), Some(0));
        assert_eq!(tabs, vec![3, 1, 2, 0]);
    }

    #[test]
    fn blank_titlebar_distinguishes_drag_and_system_double_click() {
        assert_eq!(
            blank_titlebar_mouse_action(1),
            BlankTitlebarMouseAction::PrepareWindowDrag
        );
        assert_eq!(
            blank_titlebar_mouse_action(2),
            BlankTitlebarMouseAction::PerformSystemDoubleClick
        );
        assert_eq!(
            blank_titlebar_mouse_action(3),
            BlankTitlebarMouseAction::Ignore
        );
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

    #[test]
    fn markdown_anchor_normalization_matches_wiki_heading_rules() {
        assert_eq!(normalize_markdown_anchor("Hello World"), "hello-world");
        assert_eq!(normalize_markdown_anchor("  ## 中文 标题 ##  "), "中文-标题");
        assert_eq!(normalize_markdown_anchor("already--linked"), "already-linked");
    }
}
