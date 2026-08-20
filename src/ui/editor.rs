use crate::commands::{Copy, Cut, Find, FindNext, FindPrevious, Paste, Redo, SelectAll, Undo};
use crate::model::document::DocumentState;
use crate::model::inline_markdown::InlineMarkdownState;
use crate::services::settings;
use crate::services::syntax::{SyntaxKind, SyntaxSpan};
use crate::ui::editor_text_layout::{EditorTextLayout, render_editor_text};
use crate::ui::text_utils::ellipsize_chars;
use crate::ui::theme::{MarkdownStyle, Theme};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, FontWeight, HighlightStyle,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    ParentElement, Pixels, Point, Render, ScrollHandle, StatefulInteractiveElement, Styled,
    UTF16Selection, Window, anchored, canvas, combine_highlights, deferred, div,
    fill, point, px, size,
};
use gpui_gfm::{
    InteractiveScrollbarAxis, InteractiveScrollbarState, render_interactive_scrollbar,
};
use std::ops::Range;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

const CARET_WIDTH: f32 = 2.0;
const HEAVY_DOCUMENT_BYTES: usize = 64 * 1024;

pub type OutlineViewportCallback = Arc<dyn Fn(usize, &mut App)>;

struct SearchCache {
    revision: u64,
    query: String,
    matches: Vec<Range<usize>>,
}

#[derive(Clone, Debug)]
struct ProjectionSegment {
    source: Range<usize>,
    display_start: usize,
    hidden: bool,
}

#[derive(Clone, Debug)]
struct DisplayProjection {
    display_text: String,
    source_len: usize,
    segments: Vec<ProjectionSegment>,
}

impl DisplayProjection {
    /// Creates the editor's identity source projection.
    ///
    /// Edit mode intentionally displays every Markdown byte, so cursor, selection,
    /// search, and IME offsets all use the original document source directly.
    fn from_source(source: &str) -> Self {
        let source_len = source.len();
        Self {
            display_text: source.to_string(),
            source_len,
            segments: vec![ProjectionSegment {
                source: 0..source_len,
                display_start: 0,
                hidden: false,
            }],
        }
    }

    fn source_to_display_byte(&self, source_byte: usize) -> usize {
        let source_byte = source_byte.min(self.source_len);
        let segment_index = self
            .segments
            .partition_point(|segment| segment.source.end < source_byte);
        let Some(segment) = self.segments.get(segment_index) else {
            return self.display_text.len();
        };

        if source_byte < segment.source.start || segment.hidden {
            return segment.display_start;
        }

        segment.display_start + (source_byte - segment.source.start)
    }

    fn display_to_source_byte(&self, display_byte: usize) -> usize {
        let display_byte = display_byte.min(self.display_text.len());
        let mut mapped = self.source_len;

        for segment in &self.segments {
            if display_byte < segment.display_start {
                return segment.source.start;
            }

            if segment.hidden {
                // Hidden Markdown syntax occupies no display width. When the caret lands exactly
                // on that boundary, bias it to the right so typing after a rendered link happens
                // after `](...)` rather than inside the hidden Markdown syntax.
                if display_byte == segment.display_start {
                    mapped = segment.source.end;
                }
                continue;
            }

            let display_end = segment.display_start + segment.source.len();
            if display_byte < display_end {
                return segment.source.start + (display_byte - segment.display_start);
            }
            if display_byte == display_end {
                mapped = segment.source.end;
                continue;
            }
        }

        mapped.min(self.source_len)
    }

    fn project_highlights(
        &self,
        highlights: Vec<(Range<usize>, HighlightStyle)>,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        highlights
            .into_iter()
            .filter_map(|(range, style)| {
                let start = self.source_to_display_byte(range.start);
                let end = self.source_to_display_byte(range.end);
                if start < end {
                    Some((start..end, style))
                } else {
                    None
                }
            })
            .collect()
    }
}

pub struct EditorView {
    document: Entity<DocumentState>,
    inline_markdown: Entity<InlineMarkdownState>,
    focus_handle: Option<FocusHandle>,
    caret_visible: bool,
    blink_task: Option<gpui::Task<()>>,
    scroll_handle: ScrollHandle,
    /// Hover and drag state for the editor's custom vertical scrollbar.
    scrollbar_state: InteractiveScrollbarState,
    /// Cached text with revision to avoid repeated rope-to-string conversions.
    cached_text: Option<(u64, Arc<str>)>,
    /// Cached source/display projection for the current document + inline parse revisions.
    cached_projection: Option<(u64, u64, Arc<DisplayProjection>)>,
    /// Character range currently owned by the platform IME composition.
    marked_range: Option<Range<usize>>,
    /// Keeps the full IME composition as one undo transaction.
    ime_edit_active: bool,
    /// Latest laid-out editor text used to position the platform IME candidate window.
    input_layout: Option<EditorTextLayout>,
    /// Latest source/display mapping paired with `input_layout`.
    input_projection: Option<Arc<DisplayProjection>>,
    /// Find panel state.
    search_active: bool,
    search_query: String,
    search_current_match: usize,
    cached_search: Option<SearchCache>,
    /// Byte offset that should be revealed after next layout.
    pending_scroll_to_byte: Option<usize>,
    /// Outline target that should be aligned near the top after layout.
    pending_outline_reveal_byte: Option<usize>,
    /// Reports the source byte crossing the editor viewport activation line.
    on_outline_viewport_change: Option<OutlineViewportCallback>,
    /// Window-space position of the editor's lightweight right-click menu.
    context_menu_position: Option<Point<Pixels>>,
}

impl EditorView {
    pub fn new(
        document: Entity<DocumentState>,
        inline_markdown: Entity<InlineMarkdownState>,
    ) -> Self {
        Self {
            document,
            inline_markdown,
            focus_handle: None,
            caret_visible: true,
            blink_task: None,
            scroll_handle: ScrollHandle::new(),
            scrollbar_state: InteractiveScrollbarState::default(),
            cached_text: None,
            cached_projection: None,
            marked_range: None,
            ime_edit_active: false,
            input_layout: None,
            input_projection: None,
            search_active: false,
            search_query: String::new(),
            search_current_match: 0,
            cached_search: None,
            pending_scroll_to_byte: None,
            pending_outline_reveal_byte: None,
            on_outline_viewport_change: None,
            context_menu_position: None,
        }
    }

    fn start_cursor_blink(&mut self, cx: &mut Context<Self>) {
        if self.blink_task.is_some() {
            return;
        }
        let entity = cx.entity();
        self.blink_task = Some(cx.spawn(async move |_editor, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let _ = entity.update(cx, |view, cx| {
                    let should_blink = {
                        view.document.read(cx).rope.len_bytes() <= HEAVY_DOCUMENT_BYTES
                    };

                    if should_blink {
                        view.caret_visible = !view.caret_visible;
                        cx.notify();
                    } else if !view.caret_visible {
                        // Large documents keep a steady caret so the blink timer cannot
                        // force a full editor render every 500 ms.
                        view.caret_visible = true;
                        cx.notify();
                    }
                });
            }
        }));
    }

    /// Scrolls the editor so the current cursor is visible.
    pub fn reveal_cursor(&mut self, cx: &mut Context<Self>) {
        let byte = self
            .document
            .read(cx)
            .char_to_byte(self.document.read(cx).cursor);
        self.pending_scroll_to_byte = Some(byte);
        cx.notify();
    }

    /// Aligns an Outline target to a stable top inset using its real text layout Y.
    pub fn reveal_outline(&mut self, byte: usize, cx: &mut Context<Self>) {
        self.pending_scroll_to_byte = None;
        self.pending_outline_reveal_byte = Some(byte);
        cx.notify();
    }

    pub fn set_on_outline_viewport_change(&mut self, callback: OutlineViewportCallback) {
        self.on_outline_viewport_change = Some(callback);
    }

    fn selection_highlights(&self, doc: &DocumentState) -> Vec<(Range<usize>, HighlightStyle)> {
        doc.selection_bytes().map_or_else(Vec::new, |range| {
            vec![(
                range,
                HighlightStyle {
                    background_color: Some(hsla_with_alpha(Theme::selection_bg(), 0.18)),
                    ..Default::default()
                },
            )]
        })
    }

    fn inline_syntax_highlights(
        &self,
        spans: &[SyntaxSpan],
        markdown_style: MarkdownStyle,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        spans
            .iter()
            .map(|span| (span.range.clone(), syntax_style(span.kind, markdown_style)))
            .collect()
    }

    fn current_text_and_revision(&mut self, cx: &mut Context<Self>) -> (Arc<str>, u64) {
        let revision = self.document.read(cx).revision;
        if let Some((cached_revision, cached)) = &self.cached_text
            && *cached_revision == revision
        {
            return (cached.clone(), revision);
        }

        let text = Arc::<str>::from(self.document.read(cx).text());
        self.cached_text = Some((revision, text.clone()));
        (text, revision)
    }

    fn invalidate_search_cache(&mut self) {
        self.cached_search = None;
    }

    fn ensure_search_cache<'a>(&'a mut self, text: &str, revision: u64) -> &'a SearchCache {
        let query = self.search_query.clone();
        let should_recompute = self
            .cached_search
            .as_ref()
            .is_none_or(|cache| cache.revision != revision || cache.query != query);

        if should_recompute {
            let matches = find_all_matches_case_insensitive(text, &query);
            self.cached_search = Some(SearchCache {
                revision,
                query,
                matches,
            });
        }

        self.cached_search
            .as_ref()
            .expect("search cache should be initialized")
    }

    fn search_highlights(
        &mut self,
        text: &str,
        revision: u64,
    ) -> (Vec<(Range<usize>, HighlightStyle)>, usize) {
        if !self.search_active || self.search_query.is_empty() {
            return (Vec::new(), 0);
        }

        let matches = self.ensure_search_cache(text, revision).matches.clone();
        let count = matches.len();
        let style = HighlightStyle {
            background_color: Some(hsla_with_alpha(gpui::rgb(0xffd66b), 0.42)),
            ..Default::default()
        };

        (
            matches.into_iter().map(|range| (range, style)).collect(),
            count,
        )
    }

    fn activate_search(&mut self, cx: &mut Context<Self>) {
        self.search_active = true;

        if self.search_query.is_empty() {
            let seed_query = {
                let doc = self.document.read(cx);
                doc.selection_range().map(|range| doc.slice_chars(range))
            };

            if let Some(seed) = seed_query {
                if !seed.trim().is_empty() && !seed.contains('\n') {
                    self.search_query = seed;
                }
            }
        }

        self.search_current_match = 0;
        self.invalidate_search_cache();
        self.select_current_search_match(cx);
        cx.notify();
    }

    fn close_search(&mut self, cx: &mut Context<Self>) {
        self.search_active = false;
        cx.notify();
    }

    fn select_current_search_match(&mut self, cx: &mut Context<Self>) {
        if self.search_query.is_empty() {
            return;
        }

        let (text, revision) = self.current_text_and_revision(cx);
        let matches = self
            .ensure_search_cache(text.as_ref(), revision)
            .matches
            .clone();
        let match_range = {
            let total = matches.len();
            if total == 0 {
                None
            } else {
                if self.search_current_match >= total {
                    self.search_current_match = 0;
                }
                matches.get(self.search_current_match).cloned()
            }
        };

        if let Some(range) = match_range {
            let _ = self.document.update(cx, |doc, cx| {
                let start = doc.byte_to_char(range.start);
                let end = doc.byte_to_char(range.end);
                doc.set_selection(start, end);
                cx.notify();
            });
            self.pending_scroll_to_byte = Some(range.start);
        }
    }

    fn jump_search(&mut self, cx: &mut Context<Self>, forward: bool) {
        if self.search_query.is_empty() {
            return;
        }

        let (text, revision) = self.current_text_and_revision(cx);
        let total_matches = self
            .ensure_search_cache(text.as_ref(), revision)
            .matches
            .len();
        if total_matches == 0 {
            return;
        }

        if forward {
            self.search_current_match = (self.search_current_match + 1) % total_matches;
        } else if self.search_current_match == 0 {
            self.search_current_match = total_matches - 1;
        } else {
            self.search_current_match -= 1;
        }

        self.select_current_search_match(cx);
        cx.notify();
    }

    fn handle_search_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.to_lowercase();
        let modifiers = event.keystroke.modifiers;
        let is_cmd = modifiers.platform || modifiers.control;
        let shift = modifiers.shift;

        if key == "escape" {
            self.close_search(cx);
            return;
        }

        if key == "enter" || key == "return" {
            self.jump_search(cx, !shift);
            return;
        }

        if key == "backspace" {
            pop_last_char(&mut self.search_query);
            self.search_current_match = 0;
            self.invalidate_search_cache();
            self.select_current_search_match(cx);
            cx.notify();
            return;
        }

        if is_cmd {
            return;
        }

        if let Some(raw) = &event.keystroke.key_char {
            if raw != "\n" && raw != "\r" && !raw.is_empty() {
                self.search_query.push_str(raw);
                self.search_current_match = 0;
                self.invalidate_search_cache();
                self.select_current_search_match(cx);
                cx.notify();
            }
        }
    }
}

fn char_index_to_utf16(doc: &DocumentState, char_index: usize) -> usize {
    doc.rope
        .chars()
        .take(char_index.min(doc.len_chars()))
        .map(char::len_utf16)
        .sum()
}

fn utf16_to_char_index(doc: &DocumentState, utf16_offset: usize) -> usize {
    let mut utf16_count = 0usize;
    let mut char_count = 0usize;

    for ch in doc.rope.chars() {
        if utf16_count >= utf16_offset {
            break;
        }

        let next = utf16_count.saturating_add(ch.len_utf16());
        if next > utf16_offset {
            break;
        }

        utf16_count = next;
        char_count += 1;
    }

    char_count
}

fn char_range_to_utf16(doc: &DocumentState, range: &Range<usize>) -> Range<usize> {
    char_index_to_utf16(doc, range.start)..char_index_to_utf16(doc, range.end)
}

fn utf16_range_to_chars(doc: &DocumentState, range: &Range<usize>) -> Range<usize> {
    let start = utf16_to_char_index(doc, range.start);
    let end = utf16_to_char_index(doc, range.end).max(start);
    start..end
}

fn utf16_offset_to_char_in_str(text: &str, utf16_offset: usize) -> usize {
    let mut utf16_count = 0usize;
    let mut char_count = 0usize;

    for ch in text.chars() {
        if utf16_count >= utf16_offset {
            break;
        }

        let next = utf16_count.saturating_add(ch.len_utf16());
        if next > utf16_offset {
            break;
        }

        utf16_count = next;
        char_count += 1;
    }

    char_count
}

impl EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let doc = self.document.read(cx);
        let range = utf16_range_to_chars(&doc, &range_utf16);
        adjusted_range.replace(char_range_to_utf16(&doc, &range));
        Some(doc.slice_chars(range))
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let doc = self.document.read(cx);
        let range = doc
            .selection_range()
            .unwrap_or_else(|| doc.cursor..doc.cursor);
        let reversed = doc
            .selection_anchor
            .is_some_and(|anchor| doc.selection.is_some() && doc.cursor < anchor);

        Some(UTF16Selection {
            range: char_range_to_utf16(&doc, &range),
            reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let doc = self.document.read(cx);
        self.marked_range
            .as_ref()
            .map(|range| char_range_to_utf16(&doc, range))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.ime_edit_active {
            let _ = self.document.update(cx, |doc, cx_doc| {
                doc.commit_edit();
                cx_doc.notify();
            });
        }
        self.ime_edit_active = false;
        self.marked_range = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = {
            let doc = self.document.read(cx);
            range_utf16
                .as_ref()
                .map(|range| utf16_range_to_chars(&doc, range))
                .or_else(|| self.marked_range.clone())
                .or_else(|| doc.selection_range())
                .unwrap_or_else(|| doc.cursor..doc.cursor)
        };
        let continuing_ime = self.ime_edit_active;

        let _ = self.document.update(cx, |doc, cx_doc| {
            if !continuing_ime {
                doc.begin_edit();
            }

            let start = target.start.min(doc.len_chars());
            let end = target.end.min(doc.len_chars()).max(start);
            if start < end {
                doc.delete_range(start..end);
            }
            if !text.is_empty() {
                doc.insert(start, text);
            }
            doc.set_cursor(start.saturating_add(text.chars().count()));
            doc.commit_edit();
            cx_doc.notify();
        });

        self.ime_edit_active = false;
        self.marked_range = None;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = {
            let doc = self.document.read(cx);
            range_utf16
                .as_ref()
                .map(|range| utf16_range_to_chars(&doc, range))
                .or_else(|| self.marked_range.clone())
                .or_else(|| doc.selection_range())
                .unwrap_or_else(|| doc.cursor..doc.cursor)
        };

        let inserted_chars = new_text.chars().count();
        let relative_selection = new_selected_range_utf16
            .as_ref()
            .map(|range| {
                let start = utf16_offset_to_char_in_str(new_text, range.start);
                let end = utf16_offset_to_char_in_str(new_text, range.end).max(start);
                start..end
            })
            .unwrap_or(inserted_chars..inserted_chars);
        let start_new_transaction = !self.ime_edit_active;
        let mut new_marked_range = None;

        let _ = self.document.update(cx, |doc, cx_doc| {
            if start_new_transaction {
                doc.begin_edit();
            }

            let start = target.start.min(doc.len_chars());
            let end = target.end.min(doc.len_chars()).max(start);
            if start < end {
                doc.delete_range(start..end);
            }
            if !new_text.is_empty() {
                doc.insert(start, new_text);
                new_marked_range = Some(start..start.saturating_add(inserted_chars));
            }

            let selection_start = start
                .saturating_add(relative_selection.start.min(inserted_chars))
                .min(doc.len_chars());
            let selection_end = start
                .saturating_add(relative_selection.end.min(inserted_chars))
                .min(doc.len_chars());
            if selection_start == selection_end {
                doc.set_cursor(selection_end);
            } else {
                doc.set_selection(selection_start, selection_end);
            }
            cx_doc.notify();
        });

        self.ime_edit_active = true;
        self.marked_range = new_marked_range;
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.input_layout.as_ref()?;
        let projection = self.input_projection.as_ref()?;
        let doc = self.document.read(cx);
        let range = utf16_range_to_chars(&doc, &range_utf16);
        let source_byte = doc.char_to_byte(range.end);
        let display_byte = projection.source_to_display_byte(source_byte);
        let position = std::panic::catch_unwind(AssertUnwindSafe(|| {
            layout.position_for_index(display_byte)
        }))
        .ok()
        .flatten()?;
        let line_height = std::panic::catch_unwind(AssertUnwindSafe(|| {
            layout.line_height_for_index(display_byte)
        }))
        .ok()?;
        if line_height <= px(0.) {
            return None;
        }

        Some(Bounds {
            origin: position,
            size: size(px(1.), line_height),
        })
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let layout = self.input_layout.as_ref()?;
        let projection = self.input_projection.as_ref()?;
        let display_byte = std::panic::catch_unwind(AssertUnwindSafe(|| {
            layout.index_for_position(point)
        }))
        .ok()
        .map(|result| match result {
            Ok(index) => index,
            Err(index) => index,
        })?;
        let source_byte = projection.display_to_source_byte(display_byte);
        let doc = self.document.read(cx);
        let char_index = doc.byte_to_char(source_byte);
        Some(char_index_to_utf16(&doc, char_index))
    }
}

fn source_byte_at_viewport_activation_line(
    scroll_handle: &ScrollHandle,
    text_layout: &EditorTextLayout,
    projection: &DisplayProjection,
) -> Option<usize> {
    let viewport = scroll_handle.bounds();
    if viewport.size.height <= px(0.) {
        return None;
    }

    let activation_point = point(viewport.left() + px(32.), viewport.top() + px(32.));
    let display_byte = std::panic::catch_unwind(AssertUnwindSafe(|| {
        text_layout.index_for_position(activation_point)
    }))
    .ok()
    .map(|result| match result {
        Ok(index) => index,
        Err(index) => index,
    })?;

    Some(projection.display_to_source_byte(display_byte))
}

fn reveal_source_byte_after_layout(
    scroll_handle: &ScrollHandle,
    text_layout: &EditorTextLayout,
    projection: &DisplayProjection,
    source_byte: usize,
    align_to_top: bool,
    window: &mut Window,
) {
    let display_byte = projection.source_to_display_byte(source_byte);
    let Some(target_pos) = std::panic::catch_unwind(AssertUnwindSafe(|| {
        text_layout.position_for_index(display_byte)
    }))
    .ok()
    .flatten() else {
        return;
    };

    let line_height = std::panic::catch_unwind(AssertUnwindSafe(|| {
        text_layout.line_height_for_index(display_byte)
    }))
    .ok()
    .unwrap_or(px(0.));
    if line_height <= px(0.) {
        return;
    }

    let viewport = scroll_handle.bounds();
    if viewport.size.height <= px(0.) {
        return;
    }

    let max = scroll_handle.max_offset();
    let current = scroll_handle.offset();
    let mut next = current;

    if align_to_top {
        let desired_top = viewport.top() + px(24.);
        next.y = current.y + (desired_top - target_pos.y);
    } else {
        let padding = px(28.);
        let visible_top = viewport.top() + padding;
        let visible_bottom = viewport.bottom() - padding;
        let target_bottom = target_pos.y + line_height;

        if target_pos.y < visible_top {
            next.y = current.y + (visible_top - target_pos.y);
        } else if target_bottom > visible_bottom {
            next.y = current.y - (target_bottom - visible_bottom);
        } else {
            return;
        }
    }

    next.y = next.y.clamp(-max.height, px(0.));
    if next.y != current.y {
        scroll_handle.set_offset(next);
        window.refresh();
    }
}

fn editor_context_menu_item<F>(
    id: &'static str,
    label: &'static str,
    disabled: bool,
    on_click: F,
) -> AnyElement
where
    F: Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
{
    div()
        .id(id)
        .w_full()
        .h(px(30.))
        .px(px(10.))
        .flex()
        .items_center()
        .rounded(px(5.))
        .text_sm()
        .text_color(if disabled { Theme::muted() } else { Theme::text() })
        .when(!disabled, |this| {
            this.cursor_pointer()
                .hover(|style| style.bg(Theme::panel_alt()))
                .on_mouse_down(MouseButton::Left, on_click)
        })
        .child(label)
        .into_any_element()
}

fn close_editor_context_menu(editor: &Entity<EditorView>, cx: &mut App) {
    let _ = editor.update(cx, |view, cx| {
        view.context_menu_position = None;
        cx.notify();
    });
}

fn build_editor_context_menu(
    position: Point<Pixels>,
    has_selection: bool,
    editor: Entity<EditorView>,
) -> AnyElement {
    let cut_editor = editor.clone();
    let cut = editor_context_menu_item("editor-context-cut", "Cut", !has_selection, move |_, window, cx| {
        window.dispatch_action(Box::new(Cut), cx);
        close_editor_context_menu(&cut_editor, cx);
        cx.stop_propagation();
    });

    let copy_editor = editor.clone();
    let copy = editor_context_menu_item(
        "editor-context-copy",
        "Copy",
        !has_selection,
        move |_, window, cx| {
            window.dispatch_action(Box::new(Copy), cx);
            close_editor_context_menu(&copy_editor, cx);
            cx.stop_propagation();
        },
    );

    let paste_editor = editor.clone();
    let paste = editor_context_menu_item(
        "editor-context-paste",
        "Paste",
        false,
        move |_, window, cx| {
            window.dispatch_action(Box::new(Paste), cx);
            close_editor_context_menu(&paste_editor, cx);
            cx.stop_propagation();
        },
    );

    let select_all_editor = editor.clone();
    let select_all = editor_context_menu_item(
        "editor-context-select-all",
        "Select All",
        false,
        move |_, window, cx| {
            window.dispatch_action(Box::new(SelectAll), cx);
            close_editor_context_menu(&select_all_editor, cx);
            cx.stop_propagation();
        },
    );

    let menu = div()
        .id("editor-context-menu")
        .w(px(180.))
        .p(px(4.))
        .rounded(px(8.))
        .bg(Theme::panel())
        .border_1()
        .border_color(Theme::border())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(cut)
        .child(copy)
        .child(paste)
        .child(
            div()
                .h(px(1.))
                .mx(px(6.))
                .my(px(4.))
                .bg(Theme::border()),
        )
        .child(select_all);

    deferred(
        anchored()
            .position(position)
            .snap_to_window_with_margin(px(8.))
            .child(menu),
    )
    .with_priority(2)
    .into_any_element()
}

impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle
            .clone()
            .expect("focus handle should be initialized during render")
    }
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.start_cursor_blink(cx);
        let focus_handle = self
            .focus_handle
            .get_or_insert_with(|| {
                let handle = cx.focus_handle();
                handle.focus(window);
                handle
            })
            .clone();

        let is_focused = focus_handle.is_focused(window);
        let markdown_style = Theme::markdown_style();

        // Use cached text if revision hasn't changed to avoid O(n) rope-to-string conversion.
        let (text_owned, doc_revision) = {
            let doc = self.document.read(cx);
            let rev = doc.revision;
            if let Some((cached_rev, ref text)) = self.cached_text {
                if cached_rev == rev {
                    (text.clone(), rev)
                } else {
                    (Arc::<str>::from(doc.text()), rev)
                }
            } else {
                (Arc::<str>::from(doc.text()), rev)
            }
        };

        // Update cache if needed (after releasing the read borrow).
        if self.cached_text.as_ref().map(|(r, _)| *r) != Some(doc_revision) {
            self.cached_text = Some((doc_revision, text_owned.clone()));
        }

        let doc = self.document.read(cx);
        let cursor_source_byte = doc.char_to_byte(doc.cursor);
        let show_caret = doc.selection.is_none();
        let has_selection = doc.selection_range().is_some();
        let draw_caret = show_caret && is_focused && self.caret_visible;
        let (inline_spans, inline_revision) = {
            let inline = self.inline_markdown.read(cx);
            (inline.spans.clone(), inline.source_revision)
        };
        // Large documents are parsed asynchronously, so their spans can temporarily
        // describe the previous document revision. Never apply stale byte ranges to
        // the current source string.
        let inline_spans_are_current = inline_revision == doc_revision;
        let render_spans: &[SyntaxSpan] = if inline_spans_are_current {
            inline_spans.as_ref().as_slice()
        } else {
            &[]
        };
        let projection = if let Some((cached_doc_revision, cached_inline_revision, cached)) =
            &self.cached_projection
            && *cached_doc_revision == doc_revision
            && *cached_inline_revision == inline_revision
        {
            cached.clone()
        } else {
            let projection = Arc::new(DisplayProjection::from_source(text_owned.as_ref()));
            self.cached_projection = Some((doc_revision, inline_revision, projection.clone()));
            projection
        };
        let cursor_display_byte = projection.source_to_display_byte(cursor_source_byte);
        let syntax_highlights = if inline_spans_are_current {
            projection.project_highlights(self.inline_syntax_highlights(render_spans, markdown_style))
        } else {
            Vec::new()
        };
        let (search_highlights, search_match_count) =
            self.search_highlights(text_owned.as_ref(), doc_revision);
        let search_highlights = projection.project_highlights(search_highlights);
        let selection_highlights = projection.project_highlights(self.selection_highlights(&doc));
        drop(doc);

        let syntax_and_search = if search_highlights.is_empty() {
            syntax_highlights
        } else {
            combine_highlights(syntax_highlights, search_highlights).collect()
        };

        let all_highlights = if selection_highlights.is_empty() {
            syntax_and_search
        } else {
            combine_highlights(syntax_and_search, selection_highlights).collect()
        };

        let safe_highlights = sanitize_highlights(&projection.display_text, all_highlights);
        let editor_text = render_editor_text(
            &projection.display_text,
            text_owned.as_ref(),
            &safe_highlights,
            settings::get_font_size(),
        );
        let editor_left_padding = editor_text.left_padding;
        let text_layout = editor_text.layout.clone();
        let editor_text_element = editor_text.element;
        self.input_layout = Some(text_layout.clone());
        self.input_projection = Some(projection.clone());
        let text_layout_for_sync = text_layout.clone();
        let text_layout_for_caret = text_layout.clone();
        let pending_reveal = self
            .pending_outline_reveal_byte
            .take()
            .map(|byte| (byte, true))
            .or_else(|| self.pending_scroll_to_byte.take().map(|byte| (byte, false)));
        let outline_callback = self.on_outline_viewport_change.clone();
        let outline_scroll_handle = self.scroll_handle.clone();
        let outline_projection = projection.clone();
        let editor_scroll_handle = self.scroll_handle.clone();
        let editor_scrollbar_state = self.scrollbar_state.clone();
        let input_enabled = !self.search_active;
        let input_focus_handle = focus_handle.clone();
        let input_entity = cx.entity();
        let context_menu = self
            .context_menu_position
            .map(|position| build_editor_context_menu(position, has_selection, cx.entity()));

        let search_match_display = if search_match_count == 0 {
            0
        } else {
            self.search_current_match.min(search_match_count - 1) + 1
        };

        div()
            .relative()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                if this.context_menu_position.take().is_some() {
                    cx.notify();
                }
            }))
            .child(
                div()
                    .id("editor_scroll")
                    .relative()
                    .size_full()
                    .bg(Theme::panel())
                    .pl(px(editor_left_padding))
                    .pr(px(32.))
                    .py(px(24.))
                    .text_size(px(settings::get_font_size()))
                    .text_color(markdown_style.foreground)
                    .font_family("Menlo")
                    .overflow_y_scroll()
                    .overflow_x_hidden()
                    .scrollbar_width(px(10.))
                    .track_scroll(&self.scroll_handle)
                    .track_focus(&focus_handle)
                    .on_action({
                        let doc_handle = self.document.clone();
                        move |_: &SelectAll, _window: &mut Window, cx_app: &mut App| {
                            let _ = doc_handle.update(cx_app, |doc, cx| {
                                doc.select_all();
                                cx.notify();
                            });
                        }
                    })
                    .on_action({
                        let doc_handle = self.document.clone();
                        move |_: &Copy, _window: &mut Window, cx_app: &mut App| {
                            if let Some(selection) =
                                doc_handle.read_with(cx_app, |d, _| d.selection_range())
                            {
                                let text =
                                    doc_handle.read_with(cx_app, |d, _| d.slice_chars(selection));
                                cx_app.write_to_clipboard(ClipboardItem::new_string(text));
                            }
                        }
                    })
                    .on_action({
                        let doc_handle = self.document.clone();
                        move |_: &Cut, _window: &mut Window, cx_app: &mut App| {
                            let selection = doc_handle
                                .read_with(cx_app, |d, _| d.selection_range())
                                .unwrap_or_else(|| 0..0);
                            if selection.start == selection.end {
                                return;
                            }

                            let text = doc_handle
                                .read_with(cx_app, |d, _| d.slice_chars(selection.clone()));
                            cx_app.write_to_clipboard(ClipboardItem::new_string(text));
                            let _ = doc_handle.update(cx_app, |doc, cx| {
                                doc.begin_edit();
                                doc.delete_selection();
                                doc.commit_edit();
                                cx.notify();
                            });
                        }
                    })
                    .on_action({
                        let doc_handle = self.document.clone();
                        move |_: &Paste, _window: &mut Window, cx_app: &mut App| {
                            let Some(item) = cx_app.read_from_clipboard() else {
                                return;
                            };
                            let Some(text) = item.text() else {
                                return;
                            };
                            let _ = doc_handle.update(cx_app, |doc, cx| {
                                doc.begin_edit();
                                doc.delete_selection();
                                let insert_at = doc.cursor;
                                doc.insert(insert_at, &text);
                                doc.cursor = insert_at.saturating_add(text.chars().count());
                                doc.commit_edit();
                                cx.notify();
                            });
                        }
                    })
                    .on_action({
                        let doc_handle = self.document.clone();
                        move |_: &Undo, _window: &mut Window, cx_app: &mut App| {
                            let _ = doc_handle.update(cx_app, |doc, cx| {
                                if doc.undo() {
                                    cx.notify();
                                }
                            });
                        }
                    })
                    .on_action({
                        let doc_handle = self.document.clone();
                        move |_: &Redo, _window: &mut Window, cx_app: &mut App| {
                            let _ = doc_handle.update(cx_app, |doc, cx| {
                                if doc.redo() {
                                    cx.notify();
                                }
                            });
                        }
                    })
                    .on_action({
                        let focus_handle = focus_handle.clone();
                        cx.listener(move |this, _: &Find, window, cx| {
                            focus_handle.focus(window);
                            this.activate_search(cx);
                        })
                    })
                    .on_action(cx.listener(|this, _: &FindNext, _window, cx| {
                        if !this.search_active {
                            this.activate_search(cx);
                        } else {
                            this.jump_search(cx, true);
                        }
                    }))
                    .on_action(cx.listener(|this, _: &FindPrevious, _window, cx| {
                        if !this.search_active {
                            this.activate_search(cx);
                        } else {
                            this.jump_search(cx, false);
                        }
                    }))
                    .on_mouse_down(MouseButton::Right, {
                        let focus_handle = focus_handle.clone();
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            focus_handle.focus(window);
                            this.context_menu_position = Some(event.position);
                            cx.notify();
                        })
                    })
                    .on_mouse_down(MouseButton::Left, {
                        let focus_handle = focus_handle.clone();
                        let doc_handle = self.document.clone();
                        let layout_for_event = text_layout.clone();
                        let projection_for_event = projection.clone();
                        move |event: &MouseDownEvent, window: &mut Window, cx_app: &mut App| {
                            focus_handle.focus(window);
                            let _ = doc_handle.update(cx_app, |doc, cx| {
                                let byte_idx = std::panic::catch_unwind(AssertUnwindSafe(|| {
                                    layout_for_event.index_for_position(event.position)
                                }))
                                .ok()
                                .map(|res| match res {
                                    Ok(ix) => ix,
                                    Err(ix) => ix,
                                });
                                let source_byte = byte_idx
                                    .map(|b| projection_for_event.display_to_source_byte(b));
                                if let Some(byte_idx) = source_byte.map(|b| doc.byte_to_char(b)) {
                                    if event.modifiers.shift {
                                        let anchor = doc.selection_anchor.unwrap_or(doc.cursor);
                                        doc.set_selection(anchor, byte_idx);
                                    } else {
                                        doc.set_cursor(byte_idx);
                                    }
                                    cx.notify();
                                }
                            });
                        }
                    })
                    .on_mouse_move({
                        let doc_handle = self.document.clone();
                        let layout_for_event = text_layout.clone();
                        let projection_for_event = projection.clone();
                        move |event: &MouseMoveEvent, _window: &mut Window, cx_app: &mut App| {
                            if !event.dragging() {
                                return;
                            }
                            let _ = doc_handle.update(cx_app, |doc, cx| {
                                let byte_idx = std::panic::catch_unwind(AssertUnwindSafe(|| {
                                    layout_for_event.index_for_position(event.position)
                                }))
                                .ok()
                                .map(|res| match res {
                                    Ok(ix) => ix,
                                    Err(ix) => ix,
                                });
                                let source_byte = byte_idx
                                    .map(|b| projection_for_event.display_to_source_byte(b));
                                if let Some(byte_idx) = source_byte.map(|b| doc.byte_to_char(b)) {
                                    let anchor = doc.selection_anchor.unwrap_or(doc.cursor);
                                    doc.set_selection(anchor, byte_idx);
                                    cx.notify();
                                }
                            });
                        }
                    })
                    .on_key_down({
                        let focus = focus_handle.clone();
                        cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                            if !focus.is_focused(window) {
                                return;
                            }

                            let key = event.keystroke.key.to_lowercase();
                            let modifiers = event.keystroke.modifiers;
                            let is_cmd = modifiers.platform || modifiers.control;
                            let shift = modifiers.shift;

                            if is_cmd && key == "f" {
                                this.activate_search(cx);
                                return;
                            }

                            if is_cmd && key == "g" {
                                if !this.search_active {
                                    this.activate_search(cx);
                                } else {
                                    this.jump_search(cx, !shift);
                                }
                                return;
                            }

                            if this.search_active {
                                this.handle_search_key(event, cx);
                                return;
                            }

                            if is_cmd {
                                return;
                            }

                            if key == "pageup" || key == "pagedown" {
                                let max = this.scroll_handle.max_offset();
                                let offset = this.scroll_handle.offset();
                                let bounds = this.scroll_handle.bounds();
                                let page = bounds.size.height;
                                if page > px(0.) {
                                    let amount = page * 0.9;
                                    let delta = if key == "pagedown" { -amount } else { amount };
                                    let mut new_offset = offset;
                                    new_offset.y =
                                        (new_offset.y + delta).clamp(-max.height, px(0.));
                                    this.scroll_handle
                                        .set_offset(point(new_offset.x, new_offset.y));
                                    cx.notify();
                                    window.refresh();
                                }
                                return;
                            }

                            let _ = this.document.update(cx, |doc, cx_doc| {
                                let len = doc.rope.len_chars();
                                match key.as_str() {
                                    "backspace" => {
                                        doc.begin_edit();
                                        if doc.delete_selection().is_some() {
                                            doc.commit_edit();
                                            cx_doc.notify();
                                            return;
                                        }
                                        if doc.cursor > 0 && len > 0 {
                                            let start = doc.cursor.saturating_sub(1);
                                            doc.delete_range(start..doc.cursor);
                                            doc.cursor = start;
                                            doc.commit_edit();
                                            cx_doc.notify();
                                        }
                                    }
                                    "delete" => {
                                        doc.begin_edit();
                                        if doc.delete_selection().is_some() {
                                            doc.commit_edit();
                                            cx_doc.notify();
                                            return;
                                        }
                                        if doc.cursor < len {
                                            let end = (doc.cursor + 1).min(len);
                                            doc.delete_range(doc.cursor..end);
                                            doc.commit_edit();
                                            cx_doc.notify();
                                        }
                                    }
                                    "enter" | "return" => {
                                        doc.begin_edit();
                                        doc.delete_selection();
                                        doc.insert(doc.cursor, "\n");
                                        doc.cursor += 1;
                                        doc.commit_edit();
                                        cx_doc.notify();
                                    }
                                    "left" | "arrowleft" => {
                                        if shift {
                                            let anchor = doc.selection_anchor.unwrap_or(doc.cursor);
                                            if doc.cursor > 0 {
                                                doc.set_selection(anchor, doc.cursor - 1);
                                                cx_doc.notify();
                                            }
                                        } else if doc.cursor > 0 {
                                            doc.cursor -= 1;
                                            doc.clear_selection();
                                            cx_doc.notify();
                                        }
                                    }
                                    "right" | "arrowright" => {
                                        if shift {
                                            let anchor = doc.selection_anchor.unwrap_or(doc.cursor);
                                            if doc.cursor < len {
                                                doc.set_selection(anchor, doc.cursor + 1);
                                                cx_doc.notify();
                                            }
                                        } else if doc.cursor < len {
                                            doc.cursor += 1;
                                            doc.clear_selection();
                                            cx_doc.notify();
                                        }
                                    }
                                    "up" | "arrowup" => {
                                        let cursor = doc.cursor.min(len);
                                        let line_idx = doc.rope.char_to_line(cursor);
                                        if line_idx == 0 {
                                            return;
                                        }
                                        let line_start = doc.rope.line_to_char(line_idx);
                                        let col = cursor.saturating_sub(line_start);
                                        let target_line = line_idx - 1;
                                        let target_start = doc.rope.line_to_char(target_line);
                                        let target_len = doc.rope.line(target_line).len_chars();
                                        let max_col = if target_line + 1 < doc.rope.len_lines() {
                                            target_len.saturating_sub(1)
                                        } else {
                                            target_len
                                        };
                                        let new_cursor = target_start + col.min(max_col);

                                        if shift {
                                            let anchor = doc.selection_anchor.unwrap_or(cursor);
                                            doc.set_selection(anchor, new_cursor);
                                        } else {
                                            doc.cursor = new_cursor;
                                            doc.clear_selection();
                                        }
                                        cx_doc.notify();
                                    }
                                    "down" | "arrowdown" => {
                                        let cursor = doc.cursor.min(len);
                                        let line_idx = doc.rope.char_to_line(cursor);
                                        if line_idx + 1 >= doc.rope.len_lines() {
                                            return;
                                        }
                                        let line_start = doc.rope.line_to_char(line_idx);
                                        let col = cursor.saturating_sub(line_start);
                                        let target_line = line_idx + 1;
                                        let target_start = doc.rope.line_to_char(target_line);
                                        let target_len = doc.rope.line(target_line).len_chars();
                                        let max_col = if target_line + 1 < doc.rope.len_lines() {
                                            target_len.saturating_sub(1)
                                        } else {
                                            target_len
                                        };
                                        let new_cursor = target_start + col.min(max_col);

                                        if shift {
                                            let anchor = doc.selection_anchor.unwrap_or(cursor);
                                            doc.set_selection(anchor, new_cursor);
                                        } else {
                                            doc.cursor = new_cursor;
                                            doc.clear_selection();
                                        }
                                        cx_doc.notify();
                                    }
                                    _ => {}
                                }
                            });
                        })
                    })
                    .child(
                        div().relative().w_full().child(editor_text_element).child(
                            canvas(
                                move |_, window: &mut Window, cx: &mut App| {
                                    let had_pending_reveal = pending_reveal.is_some();
                                    if let Some((source_byte, align_to_top)) = pending_reveal {
                                        reveal_source_byte_after_layout(
                                            &outline_scroll_handle,
                                            &text_layout_for_sync,
                                            outline_projection.as_ref(),
                                            source_byte,
                                            align_to_top,
                                            window,
                                        );
                                    }

                                    if !had_pending_reveal
                                        && let Some(callback) = outline_callback.as_ref()
                                        && let Some(source_byte) =
                                            source_byte_at_viewport_activation_line(
                                                &outline_scroll_handle,
                                                &text_layout_for_sync,
                                                outline_projection.as_ref(),
                                            )
                                    {
                                        callback(source_byte, cx);
                                    }
                                },
                                move |bounds: Bounds<_>,
                                      (),
                                      window: &mut Window,
                                      cx: &mut App| {
                                    if input_enabled {
                                        window.handle_input(
                                            &input_focus_handle,
                                            ElementInputHandler::new(bounds, input_entity.clone()),
                                            cx,
                                        );
                                    }

                                    let caret_pos =
                                        std::panic::catch_unwind(AssertUnwindSafe(|| {
                                            text_layout_for_caret
                                                .position_for_index(cursor_display_byte)
                                        }))
                                        .ok()
                                        .flatten();
                                    let Some(caret_pos) = caret_pos else {
                                        return;
                                    };

                                    let line_height =
                                        std::panic::catch_unwind(AssertUnwindSafe(|| {
                                            text_layout_for_caret
                                                .line_height_for_index(cursor_display_byte)
                                        }))
                                        .ok()
                                        .unwrap_or(px(0.));
                                    if line_height <= px(0.) {
                                        return;
                                    }

                                    window.paint_quad(fill(
                                        Bounds {
                                            origin: point(bounds.left(), caret_pos.y),
                                            size: size(bounds.size.width, line_height),
                                        },
                                        hsla_with_alpha(Theme::selection_bg(), 0.14),
                                    ));

                                    if !draw_caret {
                                        return;
                                    }

                                    window.paint_quad(fill(
                                        Bounds {
                                            origin: point(caret_pos.x, caret_pos.y),
                                            size: size(px(CARET_WIDTH), line_height),
                                        },
                                        Theme::accent(),
                                    ));
                                },
                            )
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full(),
                        ),
                    )
                    .when(self.search_active, |this| {
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
                                        .child(if self.search_query.is_empty() {
                                            "Type to search".to_string()
                                        } else {
                                            ellipsize_chars(&self.search_query, 80)
                                        }),
                                )
                                .child(div().text_xs().text_color(Theme::muted()).child(format!(
                                    "{}/{}",
                                    search_match_display, search_match_count
                                ))),
                        )
                    }),
            )
            .children(context_menu)
            .child(
                render_interactive_scrollbar(
                    InteractiveScrollbarAxis::Vertical,
                    editor_scrollbar_state,
                    editor_scroll_handle,
                    Theme::muted().into(),
                ),
            )
    }
}

/// Returns source-code syntax colors without applying Markdown rich-text rendering.
fn syntax_style(kind: SyntaxKind, markdown_style: MarkdownStyle) -> HighlightStyle {
    match kind {
        SyntaxKind::HeadingMarker => HighlightStyle {
            color: Some(markdown_style.muted_foreground.into()),
            ..Default::default()
        },
        SyntaxKind::HeadingText => HighlightStyle {
            color: Some(markdown_style.foreground.into()),
            ..Default::default()
        },
        SyntaxKind::QuoteMarker => HighlightStyle {
            color: Some(markdown_style.muted_foreground.into()),
            ..Default::default()
        },
        SyntaxKind::ListMarker | SyntaxKind::TaskMarker => HighlightStyle {
            color: Some(markdown_style.foreground.into()),
            ..Default::default()
        },
        SyntaxKind::CodeFence => HighlightStyle {
            color: Some(markdown_style.muted_foreground.into()),
            ..Default::default()
        },
        SyntaxKind::InlineCodeMarker | SyntaxKind::InlineCode => HighlightStyle {
            color: Some(markdown_style.foreground.into()),
            ..Default::default()
        },
        SyntaxKind::LinkTextDelimiter | SyntaxKind::LinkUrlDelimiter => HighlightStyle {
            color: Some(markdown_style.muted_foreground.into()),
            ..Default::default()
        },
        SyntaxKind::LinkText => HighlightStyle {
            color: Some(markdown_style.link.into()),
            ..Default::default()
        },
        SyntaxKind::LinkUrl => HighlightStyle {
            color: Some(markdown_style.muted_foreground.into()),
            ..Default::default()
        },
        SyntaxKind::EmphasisMarker => HighlightStyle {
            color: Some(markdown_style.muted_foreground.into()),
            ..Default::default()
        },
        SyntaxKind::EmphasisText | SyntaxKind::StrongText => HighlightStyle::default(),
    }
}

fn pop_last_char(s: &mut String) {
    if let Some((idx, _)) = s.char_indices().next_back() {
        s.truncate(idx);
    }
}

fn find_all_matches_case_insensitive(haystack: &str, needle: &str) -> Vec<Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }

    let hay_bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    if needle_bytes.len() > hay_bytes.len() {
        return Vec::new();
    }

    let needle_folded = needle_bytes
        .iter()
        .map(u8::to_ascii_lowercase)
        .collect::<Vec<_>>();

    let mut matches = Vec::new();
    let mut start = 0usize;

    while start + needle_folded.len() <= hay_bytes.len() {
        if !haystack.is_char_boundary(start) {
            start += 1;
            continue;
        }

        let end = start + needle_folded.len();
        if !haystack.is_char_boundary(end) {
            start += 1;
            continue;
        }

        let mut matched = true;
        for idx in 0..needle_folded.len() {
            if hay_bytes[start + idx].to_ascii_lowercase() != needle_folded[idx] {
                matched = false;
                break;
            }
        }

        if matched {
            matches.push(start..end);
            start = end;
        } else {
            start += 1;
        }
    }

    matches
}

fn hsla_with_alpha(color: gpui::Rgba, alpha: f32) -> gpui::Hsla {
    let mut hsla: gpui::Hsla = color.into();
    hsla.a = alpha;
    hsla
}

fn sanitize_highlights(
    text: &str,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let len = text.len();
    let mut sanitized = highlights
        .into_iter()
        .filter_map(|(range, style)| {
            if range.start >= range.end || range.start >= len {
                return None;
            }

            let mut start = range.start.min(len);
            let mut end = range.end.min(len);

            while start > 0 && !text.is_char_boundary(start) {
                start -= 1;
            }
            while end < len && !text.is_char_boundary(end) {
                end += 1;
            }

            if start < end && text.is_char_boundary(start) && text.is_char_boundary(end) {
                Some((start..end, style))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    sanitized.sort_by_key(|(range, _)| (range.start, range.end));
    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_matches_ascii_case_insensitive() {
        let text = "Hello hello HeLLo";
        let matches = find_all_matches_case_insensitive(text, "hello");
        assert_eq!(matches, vec![0..5, 6..11, 12..17]);
    }

    #[test]
    fn find_matches_respect_utf8_boundaries() {
        let text = "dn’t require a patchwork; dn’t repeat";
        let matches = find_all_matches_case_insensitive(text, "dn’t");
        assert_eq!(matches.len(), 2);
        for range in matches {
            assert!(text.is_char_boundary(range.start));
            assert!(text.is_char_boundary(range.end));
        }
    }

    #[test]
    fn sanitize_highlights_repairs_non_boundary_ranges() {
        let text = "dn’t require";
        let raw = vec![(0..3, HighlightStyle::default())];
        let sanitized = sanitize_highlights(text, raw);
        assert_eq!(sanitized.len(), 1);
        let range = &sanitized[0].0;
        assert_eq!(range.start, 0);
        assert_eq!(range.end, 5);
        assert!(text.is_char_boundary(range.start));
        assert!(text.is_char_boundary(range.end));
    }

    #[test]
    fn display_projection_preserves_all_markdown_source() {
        let source = "# Title\n2. **bold** [link](https://example.com)";
        let projection = DisplayProjection::from_source(source);
        assert_eq!(projection.display_text, source);
    }

    #[test]
    fn display_projection_preserves_heading_marker_and_following_space() {
        let source = "# Hi";
        let projection = DisplayProjection::from_source(source);
        assert_eq!(projection.display_text, source);
    }

    #[test]
    fn display_projection_maps_link_end_to_source_end() {
        let source = "![cola-2.png](<assets/cola-2.png>)";
        let projection = DisplayProjection::from_source(source);

        assert_eq!(projection.display_text, source);
        assert_eq!(
            projection.display_to_source_byte(projection.display_text.len()),
            source.len()
        );
    }

    #[test]
    fn display_projection_maps_each_source_byte_identically() {
        let source = "[Google](https://google.com) next";
        let projection = DisplayProjection::from_source(source);

        for byte in 0..=source.len() {
            assert_eq!(projection.source_to_display_byte(byte), byte);
            assert_eq!(projection.display_to_source_byte(byte), byte);
        }
    }

    #[test]
    fn display_projection_keeps_utf8_source_intact() {
        let source = "# 中文😀";
        let projection = DisplayProjection::from_source(source);
        assert_eq!(projection.display_text, source);
    }

    #[test]
    fn utf16_mapping_handles_cjk_and_surrogate_pairs() {
        let mut doc = DocumentState::new_empty();
        doc.set_text("A中😀B");

        assert_eq!(char_index_to_utf16(&doc, 0), 0);
        assert_eq!(char_index_to_utf16(&doc, 1), 1);
        assert_eq!(char_index_to_utf16(&doc, 2), 2);
        assert_eq!(char_index_to_utf16(&doc, 3), 4);
        assert_eq!(char_index_to_utf16(&doc, 4), 5);

        assert_eq!(utf16_to_char_index(&doc, 0), 0);
        assert_eq!(utf16_to_char_index(&doc, 1), 1);
        assert_eq!(utf16_to_char_index(&doc, 2), 2);
        assert_eq!(utf16_to_char_index(&doc, 3), 2);
        assert_eq!(utf16_to_char_index(&doc, 4), 3);
        assert_eq!(utf16_to_char_index(&doc, 5), 4);
    }

    #[test]
    fn marked_selection_utf16_offsets_are_relative_to_new_text() {
        assert_eq!(utf16_offset_to_char_in_str("中😀文", 0), 0);
        assert_eq!(utf16_offset_to_char_in_str("中😀文", 1), 1);
        assert_eq!(utf16_offset_to_char_in_str("中😀文", 3), 2);
        assert_eq!(utf16_offset_to_char_in_str("中😀文", 4), 3);
    }
}
