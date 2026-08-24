use crate::commands::{Copy, Cut, Find, FindNext, FindPrevious, Paste, Redo, SelectAll, Undo};
use crate::model::document::DocumentState;
use crate::model::inline_markdown::InlineMarkdownState;
use crate::services::complex_markdown::{
    ComplexMarkdownKind, MarkdownTableCell, complex_markdown_components, serialize_table_cell_value,
};
use crate::services::settings;
use crate::services::syntax::{FencedCodeBlock, SyntaxKind, SyntaxSpan, fenced_code_blocks};
use crate::ui::VIRTUALIZED_DOCUMENT_BYTES;
use crate::ui::editor_projection::{DisplayProjection, ProjectionAtom, active_line_range};
use crate::ui::editor_text_layout::{
    ActiveTableCellEditor, CodeBlockHoverHandler, ComplexComponentClickHandler,
    EDITOR_HORIZONTAL_PADDING, EditorTextLayout, LargeDocumentSourceSnapshot,
    TableCellClickHandler, component_repeat_click_edits_source, render_editor_text,
    render_virtualized_editor_text, virtual_editor_row_ranges,
};
use crate::ui::text_utils::ellipsize_chars;
use crate::ui::theme::{MarkdownStyle, Theme, ThemeName};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext, Bounds, ClipboardItem, Context, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, FontStyle, FontWeight, HighlightStyle,
    InteractiveElement, IntoElement, KeyDownEvent, ListAlignment, ListOffset, ListState,
    MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement, Pixels, Point, Render,
    ScrollHandle, StatefulInteractiveElement, StrikethroughStyle, Styled, Subscription,
    UTF16Selection, Window, anchored, canvas, combine_highlights, deferred, div, fill, point, px,
    size,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_gfm::{
    CodeSyntaxTheme, InteractiveScrollbarAxis, InteractiveScrollbarState, code_syntax_highlights,
    render_interactive_list_scrollbar, render_interactive_scrollbar,
};
use std::ops::Range;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

const CARET_WIDTH: f32 = 2.0;
const EDITOR_SCROLLBAR_WIDTH: f32 = 10.0;
const HEAVY_DOCUMENT_BYTES: usize = 64 * 1024;

pub type OutlineViewportCallback = Arc<dyn Fn(usize, &mut App)>;
pub type ComplexLinkCallback = Arc<dyn Fn(String, &mut App)>;

struct SearchCache {
    revision: u64,
    query: String,
    matches: Vec<Range<usize>>,
}

/// Caches projected syntax colors that otherwise require processing the whole document per frame.
struct LargeDocumentHighlightCache {
    document_revision: u64,
    inline_revision: u64,
    projection: Arc<DisplayProjection>,
    theme: ThemeName,
    highlights: Arc<Vec<(Range<usize>, HighlightStyle)>>,
}

/// Owns one native wrapping cell input and the open document edit it controls.
struct TableCellEdit {
    source_range: Range<usize>,
    input: Entity<InputState>,
    layout_text: String,
    input_visible: bool,
    scroll_offset: Point<Pixels>,
    input_reveal_task: Option<gpui::Task<()>>,
    scroll_restore_task: Option<gpui::Task<()>>,
    _subscription: Subscription,
}

/// Describes the editable prefix of one structured Markdown line.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MarkdownLinePrefix {
    prefix_chars: usize,
    continuation: String,
    content_is_empty: bool,
}

/// Parses list, task, ordered-list, or quote prefixes from one source line.
fn markdown_line_prefix(line: &str) -> Option<MarkdownLinePrefix> {
    let line = line.trim_end_matches(['\n', '\r']);
    let indent_bytes = line.bytes().take_while(|byte| *byte == b' ').count();
    let indent = &line[..indent_bytes];
    let content = &line[indent_bytes..];

    if content.starts_with("> ") {
        return Some(MarkdownLinePrefix {
            prefix_chars: indent.chars().count() + 2,
            continuation: format!("{indent}> "),
            content_is_empty: content[2..].trim().is_empty(),
        });
    }

    if content.len() >= 6
        && matches!(content.as_bytes()[0], b'-' | b'*' | b'+')
        && content.as_bytes()[1] == b' '
        && content.as_bytes()[2] == b'['
        && matches!(content.as_bytes()[3], b' ' | b'x' | b'X')
        && content.as_bytes()[4] == b']'
        && content.as_bytes()[5] == b' '
    {
        let marker = content.as_bytes()[0] as char;
        return Some(MarkdownLinePrefix {
            prefix_chars: indent.chars().count() + 6,
            continuation: format!("{indent}{marker} [ ] "),
            content_is_empty: content[6..].trim().is_empty(),
        });
    }

    if content.len() >= 2
        && matches!(content.as_bytes()[0], b'-' | b'*' | b'+')
        && content.as_bytes()[1] == b' '
    {
        let marker = content.as_bytes()[0] as char;
        return Some(MarkdownLinePrefix {
            prefix_chars: indent.chars().count() + 2,
            continuation: format!("{indent}{marker} "),
            content_is_empty: content[2..].trim().is_empty(),
        });
    }

    let digits = content.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0
        && content.as_bytes().get(digits) == Some(&b'.')
        && content.as_bytes().get(digits + 1) == Some(&b' ')
    {
        let number = content[..digits].parse::<usize>().ok()?;
        return Some(MarkdownLinePrefix {
            prefix_chars: indent.chars().count() + digits + 2,
            continuation: format!("{indent}{}. ", number.saturating_add(1)),
            content_is_empty: content[digits + 2..].trim().is_empty(),
        });
    }

    None
}

/// Chooses the source range that must remain literal in the live Markdown projection.
fn projection_active_source_range(
    cursor_line: Range<usize>,
    hovered_code_block: Option<Range<usize>>,
    table_cell_editing: bool,
    source_len: usize,
) -> Range<usize> {
    if table_cell_editing {
        source_len..source_len
    } else {
        hovered_code_block.unwrap_or(cursor_line)
    }
}

/// Returns the complete fenced block containing an insertion point or intersecting a selection.
fn code_block_range_for_source_range(
    blocks: &[FencedCodeBlock],
    source_range: &Range<usize>,
) -> Option<Range<usize>> {
    let candidate = blocks.partition_point(|block| block.source_range.end <= source_range.start);
    blocks.get(candidate).and_then(|block| {
        let matches = if source_range.is_empty() {
            block.source_range.start <= source_range.start
                && source_range.start < block.source_range.end
        } else {
            block.source_range.start < source_range.end
        };
        matches.then(|| block.source_range.clone())
    })
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
    cached_projection: Option<(u64, u64, Range<usize>, bool, Arc<DisplayProjection>)>,
    /// Whether Markdown syntax is rendered as an editable live presentation.
    live_markdown: bool,
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
    /// Opens wiki-link and footnote targets without coupling the editor to tab management.
    on_complex_link: Option<ComplexLinkCallback>,
    /// Window-space position of the editor's lightweight right-click menu.
    context_menu_position: Option<Point<Pixels>>,
    /// Source range of the complex component selected before entering source editing.
    selected_component: Option<Range<usize>>,
    /// Document-order index of the fenced-code block currently under the pointer.
    hovered_code_block_index: Option<usize>,
    /// Native inline editor for the currently active Markdown table cell.
    table_cell_edit: Option<TableCellEdit>,
    /// Variable-height list used only by the bounded large-document source renderer.
    large_document_list: ListState,
    /// Revision and line ranges paired with `large_document_list`.
    large_document_lines: Option<(u64, f32, bool, bool, Arc<str>, Arc<Vec<Range<usize>>>)>,
    /// Exact editor viewport width supplied by the root layout.
    large_document_viewport_width: Option<f32>,
    /// Viewport width used when the virtual list last seeded every row height.
    large_document_measured_width_bits: Option<u32>,
    /// Source-derived metadata keyed only by document revision.
    large_document_source_snapshot: Option<(u64, Arc<LargeDocumentSourceSnapshot>)>,
    /// Full projected syntax colors reused while scrolling an unchanged large document.
    large_document_highlights: Option<LargeDocumentHighlightCache>,
    /// Last cursor line revealed by the virtual list.
    large_document_cursor_line: Option<usize>,
    /// Lets event handlers choose the list scroll API instead of the legacy `ScrollHandle`.
    large_document_active: bool,
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
            live_markdown: settings::get_live_markdown_enabled(),
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
            on_complex_link: None,
            context_menu_position: None,
            selected_component: None,
            hovered_code_block_index: None,
            table_cell_edit: None,
            large_document_list: ListState::new(0, ListAlignment::Top, px(800.)).measure_all(),
            large_document_lines: None,
            large_document_viewport_width: None,
            large_document_measured_width_bits: None,
            large_document_source_snapshot: None,
            large_document_highlights: None,
            large_document_cursor_line: None,
            large_document_active: false,
        }
    }

    /// Releases layout and projection caches that are unnecessary while preview mode is visible.
    pub fn release_large_document_render_cache(&mut self) {
        if !self.large_document_active {
            return;
        }
        self.large_document_list.reset(0);
        self.large_document_lines = None;
        self.large_document_measured_width_bits = None;
        self.large_document_source_snapshot = None;
        self.large_document_highlights = None;
        self.large_document_cursor_line = None;
        self.large_document_active = false;
        self.input_layout = None;
        self.input_projection = None;
        self.cached_projection = None;
        self.cached_text = None;
    }

    /// Updates the exact width available to the editor without scheduling an extra render.
    pub fn set_large_document_viewport_width(&mut self, width: f32) {
        self.large_document_viewport_width = Some(width.max(1.0));
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
                    let should_blink =
                        { view.document.read(cx).rope.len_bytes() <= HEAVY_DOCUMENT_BYTES };

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

    /// Installs the host callback used by command-click navigation components.
    pub fn set_on_complex_link(&mut self, callback: ComplexLinkCallback) {
        self.on_complex_link = Some(callback);
    }

    /// Returns whether this editor is using the live Markdown presentation.
    pub fn live_markdown_enabled(&self) -> bool {
        self.live_markdown
    }

    /// Updates the editable Markdown presentation without changing document bytes.
    pub fn set_live_markdown_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.live_markdown != enabled {
            self.finish_table_cell_edit(cx);
            self.live_markdown = enabled;
            self.cached_projection = None;
            cx.notify();
        }
    }

    /// Begins a single undo transaction for direct editing of one table cell.
    fn begin_table_cell_edit(
        &mut self,
        cell: MarkdownTableCell,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(active) = &self.table_cell_edit
            && active.source_range == cell.source_range
        {
            active.input.update(cx, |input, cx| input.focus(window, cx));
            return;
        }

        self.finish_table_cell_edit(cx);
        let layout_text = cell.text.clone();
        let scroll_offset = self.scroll_handle.offset();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line()
                .soft_wrap(true)
                .default_value(cell.text)
        });
        let subscription = cx.subscribe(&input, |editor, input, event, cx| match event {
            InputEvent::Change => {
                let value = input.read(cx).value().to_string();
                editor.update_table_cell_value(&value, cx);
            }
            InputEvent::PressEnter { .. } | InputEvent::Blur => {
                editor.finish_table_cell_edit(cx);
            }
            InputEvent::Focus => {}
        });
        let _ = self.document.update(cx, |doc, _| doc.begin_edit());
        self.selected_component = None;
        let source_start = cell.source_range.start;
        self.table_cell_edit = Some(TableCellEdit {
            source_range: cell.source_range,
            input: input.clone(),
            layout_text,
            input_visible: false,
            scroll_offset,
            input_reveal_task: None,
            scroll_restore_task: None,
            _subscription: subscription,
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        let entity = cx.entity();
        let reveal_task = cx.spawn(async move |_editor, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(32))
                .await;
            let _ = entity.update(cx, |editor, cx| {
                if let Some(active) = &mut editor.table_cell_edit
                    && active.source_range.start == source_start
                {
                    active.input_visible = true;
                    cx.notify();
                }
            });
        });
        if let Some(active) = &mut self.table_cell_edit {
            active.input_reveal_task = Some(reveal_task);
        }
        self.cached_projection = None;
        cx.notify();
    }

    /// Replaces only the active table cell's content bytes while preserving table syntax.
    fn update_table_cell_value(&mut self, value: &str, cx: &mut Context<Self>) {
        let Some(active) = &self.table_cell_edit else {
            return;
        };
        let serialized = serialize_table_cell_value(value);
        let source_range = active.source_range.clone();
        let source_start = source_range.start;
        let scroll_offset = active.scroll_offset;
        let _ = self.document.update(cx, |doc, doc_cx| {
            let start = doc.byte_to_char(source_range.start);
            let end = doc.byte_to_char(source_range.end);
            if start < end {
                doc.delete_range(start..end);
            }
            if !serialized.is_empty() {
                doc.insert(start, &serialized);
            }
            doc.set_cursor(start + serialized.chars().count());
            doc_cx.notify();
        });
        if let Some(active) = &mut self.table_cell_edit {
            active.source_range.end = active.source_range.start + serialized.len();
        }
        self.scroll_handle.set_offset(scroll_offset);
        let entity = cx.entity();
        let restore_task = cx.spawn(async move |_editor, cx| {
            for delay in [Duration::from_millis(16), Duration::from_millis(80)] {
                cx.background_executor().timer(delay).await;
                let _ = entity.update(cx, |editor, cx| {
                    if editor
                        .table_cell_edit
                        .as_ref()
                        .is_some_and(|active| active.source_range.start == source_start)
                    {
                        editor.scroll_handle.set_offset(scroll_offset);
                        cx.notify();
                    }
                });
            }
        });
        if let Some(active) = &mut self.table_cell_edit {
            active.scroll_restore_task = Some(restore_task);
        }
        self.cached_projection = None;
        cx.notify();
    }

    /// Commits the active cell edit as one document undo operation.
    fn finish_table_cell_edit(&mut self, cx: &mut Context<Self>) {
        if self.table_cell_edit.take().is_none() {
            return;
        }
        let _ = self.document.update(cx, |doc, doc_cx| {
            doc.commit_edit();
            doc_cx.notify();
        });
        self.cached_projection = None;
        cx.notify();
    }

    /// Selects a component and optionally enters raw source editing on a repeated click.
    fn activate_complex_component(
        &mut self,
        source_range: Range<usize>,
        repeat_click_edits_source: bool,
        cx: &mut Context<Self>,
    ) {
        if self.selected_component.as_ref() == Some(&source_range) {
            if !repeat_click_edits_source {
                return;
            }
            let _ = self.document.update(cx, |doc, cx| {
                let start = doc.byte_to_char(source_range.start);
                let end = doc.byte_to_char(source_range.end);
                doc.set_selection(start, end);
                cx.notify();
            });
            self.selected_component = None;
            self.cached_projection = None;
        } else {
            self.selected_component = Some(source_range);
        }
        cx.notify();
    }

    /// Updates whole-block source presentation as pointer hover enters or leaves a code block.
    fn set_code_block_hovered(
        &mut self,
        code_block_index: usize,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        let next = if hovered {
            Some(code_block_index)
        } else if self.hovered_code_block_index == Some(code_block_index) {
            None
        } else {
            return;
        };
        if self.hovered_code_block_index != next {
            self.hovered_code_block_index = next;
            self.cached_projection = None;
            cx.notify();
        }
    }

    /// Deletes the selected component as one undoable source transaction.
    fn delete_selected_component(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(source_range) = self.selected_component.take() else {
            return false;
        };
        let _ = self.document.update(cx, |doc, cx| {
            let start = doc.byte_to_char(source_range.start);
            let end = doc.byte_to_char(source_range.end);
            doc.begin_edit();
            doc.delete_range(start..end);
            doc.set_cursor(start);
            doc.commit_edit();
            cx.notify();
        });
        self.cached_projection = None;
        cx.notify();
        true
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

    /// Builds source-relative syntax colors for fenced code in Live Markdown.
    fn fenced_code_highlights(
        &self,
        source: &str,
        blocks: &[FencedCodeBlock],
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let syntax_theme = match Theme::name() {
            crate::ui::theme::ThemeName::AyuLight => CodeSyntaxTheme::AyuLight,
            crate::ui::theme::ThemeName::AyuDark => CodeSyntaxTheme::AyuDark,
            crate::ui::theme::ThemeName::AyuMirage => CodeSyntaxTheme::AyuMirage,
        };

        blocks
            .iter()
            .filter_map(|block| {
                block
                    .language
                    .clone()
                    .map(|language| (block.content_range.clone(), language))
            })
            .flat_map(|(content_range, language)| {
                let content = &source[content_range.clone()];
                code_syntax_highlights(&language, content, syntax_theme)
                    .into_iter()
                    .map(move |highlight| {
                        (
                            (content_range.start + highlight.range.start)
                                ..(content_range.start + highlight.range.end),
                            HighlightStyle {
                                color: Some(highlight.color.into()),
                                ..Default::default()
                            },
                        )
                    })
            })
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
        let position =
            std::panic::catch_unwind(AssertUnwindSafe(|| layout.position_for_index(display_byte)))
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
        let display_byte =
            std::panic::catch_unwind(AssertUnwindSafe(|| layout.index_for_position(point)))
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
        .text_color(if disabled {
            Theme::muted()
        } else {
            Theme::text()
        })
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
    let cut = editor_context_menu_item(
        "editor-context-cut",
        "Cut",
        !has_selection,
        move |_, window, cx| {
            window.dispatch_action(Box::new(Cut), cx);
            close_editor_context_menu(&cut_editor, cx);
            cx.stop_propagation();
        },
    );

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
        .child(div().h(px(1.)).mx(px(6.)).my(px(4.)).bg(Theme::border()))
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

/// Builds the block-layout scroll viewport used by the editor document.
///
/// This must remain a normal block container. A flex column asks GPUI to size the projected
/// Markdown child from its pre-collapse intrinsic height, which creates blank scroll overflow.
fn editor_scroll_container() -> gpui::Stateful<gpui::Div> {
    div().id("editor_scroll").relative().size_full()
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
        let virtualized_document = text_owned.len() > VIRTUALIZED_DOCUMENT_BYTES;
        let was_large_document = self.large_document_active;
        self.large_document_active = virtualized_document;
        if virtualized_document && !was_large_document {
            // Do not retain the last full live-Markdown projection when the document crosses
            // into the bounded-memory rendering path.
            self.cached_projection = None;
        } else if !virtualized_document && was_large_document {
            // ListState retains per-item measurements. Release those along with the row-range
            // cache as soon as the document no longer needs virtualization.
            self.large_document_list.reset(0);
            self.large_document_lines = None;
            self.large_document_measured_width_bits = None;
            self.large_document_cursor_line = None;
            self.large_document_source_snapshot = None;
            self.large_document_highlights = None;
        }

        let doc = self.document.read(cx);
        let cursor_source_byte = doc.char_to_byte(doc.cursor);
        let cursor_or_selection_range = doc
            .selection_bytes()
            .unwrap_or(cursor_source_byte..cursor_source_byte);
        let active_source_range =
            active_line_range(text_owned.as_ref(), cursor_or_selection_range.clone());
        let large_source_snapshot = virtualized_document.then(|| {
            if let Some((revision, snapshot)) = &self.large_document_source_snapshot
                && *revision == doc_revision
            {
                return snapshot.clone();
            }
            let snapshot = Arc::new(LargeDocumentSourceSnapshot::new(text_owned.as_ref()));
            self.large_document_source_snapshot = Some((doc_revision, snapshot.clone()));
            snapshot
        });
        let code_blocks = large_source_snapshot
            .as_ref()
            .map(|snapshot| snapshot.code_blocks.clone())
            .unwrap_or_else(|| Arc::new(fenced_code_blocks(text_owned.as_ref())));
        let editing_code_block =
            code_block_range_for_source_range(&code_blocks, &cursor_or_selection_range);
        let hovered_code_block = self.hovered_code_block_index.and_then(|index| {
            code_blocks
                .get(index)
                .map(|block| block.source_range.clone())
        });
        let projection_active_range = projection_active_source_range(
            active_source_range,
            hovered_code_block.or(editing_code_block),
            self.table_cell_edit.is_some(),
            text_owned.len(),
        );
        let show_caret = doc.selection.is_none();
        let has_selection = doc.selection_range().is_some();
        let draw_caret =
            show_caret && is_focused && self.table_cell_edit.is_none() && self.caret_visible;
        let (inline_spans, complex_components, inline_revision) = {
            let inline = self.inline_markdown.read(cx);
            (
                inline.spans.clone(),
                inline.components.clone(),
                inline.source_revision,
            )
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
        let current_cell_components = (self.table_cell_edit.is_some() && !inline_spans_are_current)
            .then(|| complex_markdown_components(text_owned.as_ref()));
        let render_components = current_cell_components
            .as_ref()
            .unwrap_or(complex_components.as_ref());
        let projection_data_is_current =
            inline_spans_are_current || current_cell_components.is_some();
        let projection = if let Some((
            cached_doc_revision,
            cached_inline_revision,
            cached_active_range,
            cached_live_markdown,
            cached,
        )) = &self.cached_projection
            && *cached_doc_revision == doc_revision
            && *cached_inline_revision == inline_revision
            && *cached_active_range == projection_active_range
            && *cached_live_markdown == self.live_markdown
        {
            cached.clone()
        } else {
            let projection = Arc::new(if self.live_markdown && projection_data_is_current {
                DisplayProjection::from_markdown_with_components(
                    text_owned.as_ref(),
                    render_spans,
                    render_components,
                    projection_active_range.clone(),
                )
            } else {
                DisplayProjection::from_source(text_owned.as_ref())
            });
            self.cached_projection = Some((
                doc_revision,
                inline_revision,
                projection_active_range,
                self.live_markdown,
                projection.clone(),
            ));
            projection
        };
        let cursor_display_byte = projection.source_to_display_byte(cursor_source_byte);
        let theme_name = Theme::name();
        let syntax_highlights = if virtualized_document
            && let Some(cache) = &self.large_document_highlights
            && cache.document_revision == doc_revision
            && cache.inline_revision == inline_revision
            && cache.theme == theme_name
            && Arc::ptr_eq(&cache.projection, &projection)
        {
            cache.highlights.clone()
        } else {
            let highlights = if inline_spans_are_current {
                let mut source_highlights =
                    self.inline_syntax_highlights(render_spans, markdown_style);
                if self.live_markdown {
                    source_highlights.extend(
                        self.fenced_code_highlights(text_owned.as_ref(), code_blocks.as_slice()),
                    );
                }
                project_editor_highlights(projection.as_ref(), source_highlights)
            } else {
                Vec::new()
            };
            let highlights = Arc::new(sanitize_highlights(&projection.display_text, highlights));
            if virtualized_document {
                self.large_document_highlights = Some(LargeDocumentHighlightCache {
                    document_revision: doc_revision,
                    inline_revision,
                    projection: projection.clone(),
                    theme: theme_name,
                    highlights: highlights.clone(),
                });
            }
            highlights
        };
        let (search_highlights, search_match_count) =
            self.search_highlights(text_owned.as_ref(), doc_revision);
        let search_highlights = project_editor_highlights(projection.as_ref(), search_highlights);
        let selection_highlights =
            project_editor_highlights(projection.as_ref(), self.selection_highlights(&doc));

        let safe_highlights = if search_highlights.is_empty() && selection_highlights.is_empty() {
            syntax_highlights
        } else {
            let syntax_and_search = if search_highlights.is_empty() {
                syntax_highlights.as_ref().clone()
            } else {
                combine_highlights(syntax_highlights.as_ref().clone(), search_highlights).collect()
            };
            let all_highlights = if selection_highlights.is_empty() {
                syntax_and_search
            } else {
                combine_highlights(syntax_and_search, selection_highlights).collect()
            };
            Arc::new(sanitize_highlights(
                &projection.display_text,
                all_highlights,
            ))
        };
        let document_directory = doc
            .path
            .as_ref()
            .and_then(|path| path.parent())
            .map(|path| path.as_std_path().to_path_buf());
        let component_editor = cx.entity();
        let component_focus_handle = focus_handle.clone();
        let on_component_click: ComplexComponentClickHandler =
            Arc::new(move |source_range, repeat_click_edits_source, window, cx| {
                component_focus_handle.focus(window);
                let _ = component_editor.update(cx, |editor, cx| {
                    editor.activate_complex_component(source_range, repeat_click_edits_source, cx);
                });
            });
        let code_block_editor = cx.entity();
        let on_code_block_hover: CodeBlockHoverHandler =
            Arc::new(move |code_block_index, hovered, cx| {
                let _ = code_block_editor.update(cx, |editor, cx| {
                    editor.set_code_block_hovered(code_block_index, hovered, cx);
                });
            });
        let table_cell_editor = cx.entity();
        let on_table_cell_click: TableCellClickHandler = Arc::new(move |cell, window, cx| {
            let _ = table_cell_editor.update(cx, |editor, cx| {
                editor.begin_table_cell_edit(cell, window, cx);
            });
        });
        let active_table_cell = self
            .table_cell_edit
            .as_ref()
            .map(|edit| ActiveTableCellEditor {
                source_range: edit.source_range.clone(),
                input: edit.input.clone(),
                layout_text: edit.layout_text.clone(),
                input_visible: edit.input_visible,
            });
        let editor_font_size = settings::get_font_size();
        let show_line_numbers = settings::get_line_numbers_visible();
        let mut virtual_line_ranges_for_pointer = None;
        let editor_text = if virtualized_document {
            let large_style_changed = self.large_document_lines.as_ref().is_some_and(
                |(_, font_size, line_numbers, _, _, _)| {
                    *font_size != editor_font_size || *line_numbers != show_line_numbers
                },
            );
            let large_presentation_changed = self
                .large_document_lines
                .as_ref()
                .is_some_and(|(_, _, _, live_markdown, _, _)| *live_markdown != self.live_markdown);
            let cached_lines = self.large_document_lines.as_ref().and_then(
                |(revision, font_size, line_numbers, live_markdown, display_text, ranges)| {
                    (*revision == doc_revision
                        && *font_size == editor_font_size
                        && *line_numbers == show_line_numbers
                        && *live_markdown == self.live_markdown
                        && Arc::ptr_eq(display_text, &projection.display_text))
                    .then(|| ranges.clone())
                },
            );
            let viewport_width = self
                .large_document_viewport_width
                .unwrap_or_else(|| f32::from(window.bounds().size.width))
                .max(1.0);
            let viewport_width_bits = viewport_width.to_bits();
            let width_changed =
                self.large_document_measured_width_bits != Some(viewport_width_bits);
            let mut seed_estimated_heights = false;
            let line_ranges = cached_lines.unwrap_or_else(|| {
                let ranges = Arc::new(virtual_editor_row_ranges(projection.display_text.as_ref()));
                let old_count = self.large_document_list.item_count();
                let new_count = ranges.len();
                if old_count == 0 || large_style_changed || large_presentation_changed {
                    let previous_offset = self.large_document_list.logical_scroll_top();
                    self.large_document_list.reset(new_count);
                    if old_count > 0 {
                        self.large_document_list.scroll_to(previous_offset);
                    }
                    seed_estimated_heights = true;
                } else if old_count == new_count {
                    let changed_lines = self.document.read(cx).last_edit.as_ref().map(|edit| {
                        let start = line_index_for_byte(ranges.as_slice(), edit.start_byte);
                        let end = line_index_for_byte(ranges.as_slice(), edit.new_end_byte)
                            .saturating_add(1)
                            .min(new_count);
                        start..end.max(start.saturating_add(1).min(new_count))
                    });
                    let changed_lines = changed_lines.unwrap_or(0..new_count.min(1));
                    let replacement_count = changed_lines.end - changed_lines.start;
                    self.large_document_list
                        .splice(changed_lines, replacement_count);
                } else {
                    let changed_line = self
                        .document
                        .read(cx)
                        .last_edit
                        .as_ref()
                        .map(|edit| line_index_for_byte(ranges.as_slice(), edit.start_byte))
                        .unwrap_or(0)
                        .min(old_count);
                    if new_count > old_count {
                        let added = new_count - old_count;
                        let insertion = (changed_line + 1).min(old_count);
                        self.large_document_list.splice(insertion..insertion, added);
                    } else {
                        let removed = old_count - new_count;
                        let removal_start = (changed_line + 1).min(old_count);
                        let removal_end = (removal_start + removed).min(old_count);
                        self.large_document_list
                            .splice(removal_start..removal_end, 0);
                    }
                    if changed_line < self.large_document_list.item_count() {
                        self.large_document_list
                            .splice(changed_line..changed_line + 1, 1);
                    }
                }
                self.large_document_lines = Some((
                    doc_revision,
                    editor_font_size,
                    show_line_numbers,
                    self.live_markdown,
                    projection.display_text.clone(),
                    ranges.clone(),
                ));
                ranges
            });
            if width_changed && !seed_estimated_heights {
                let previous_offset = self.large_document_list.logical_scroll_top();
                let item_count = self.large_document_list.item_count();
                self.large_document_list.reset(item_count);
                if item_count > 0 {
                    self.large_document_list.scroll_to(previous_offset);
                }
                seed_estimated_heights = true;
            }
            if seed_estimated_heights {
                self.large_document_measured_width_bits = Some(viewport_width_bits);
            }
            virtual_line_ranges_for_pointer = Some(line_ranges.clone());
            render_virtualized_editor_text(
                projection.display_text.clone(),
                text_owned.clone(),
                projection.clone(),
                line_ranges,
                safe_highlights,
                editor_font_size,
                show_line_numbers,
                document_directory.clone(),
                large_source_snapshot.expect("large source snapshot must exist"),
                on_code_block_hover,
                self.selected_component.clone(),
                on_component_click,
                active_table_cell,
                on_table_cell_click,
                self.large_document_list.clone(),
                viewport_width,
                seed_estimated_heights,
            )
        } else {
            render_editor_text(
                &projection.display_text,
                text_owned.as_ref(),
                projection.as_ref(),
                safe_highlights.as_slice(),
                editor_font_size,
                show_line_numbers,
                document_directory.as_deref(),
                code_blocks.as_slice(),
                on_code_block_hover,
                self.selected_component.as_ref(),
                on_component_click,
                active_table_cell.as_ref(),
                on_table_cell_click,
            )
        };
        let editor_left_padding = editor_text.left_padding;
        let text_layout = editor_text.layout.clone();
        let editor_text_element = editor_text.element;
        self.input_layout = Some(text_layout.clone());
        self.input_projection = Some(projection.clone());
        let text_layout_for_sync = text_layout.clone();
        let text_layout_for_active_line = text_layout.clone();
        let text_layout_for_caret = text_layout.clone();
        let pending_reveal = self
            .pending_outline_reveal_byte
            .take()
            .map(|byte| (byte, true))
            .or_else(|| self.pending_scroll_to_byte.take().map(|byte| (byte, false)));
        if virtualized_document
            && let Some((_, _, _, _, _, line_ranges)) = self.large_document_lines.as_ref()
        {
            if let Some((source_byte, _)) = pending_reveal {
                let line_index = line_index_for_byte(line_ranges.as_slice(), source_byte);
                self.large_document_list.scroll_to(ListOffset {
                    item_ix: line_index,
                    offset_in_item: px(0.),
                });
                self.large_document_cursor_line = Some(line_index);
            } else {
                let cursor_line = line_index_for_byte(line_ranges.as_slice(), cursor_display_byte);
                if self.large_document_cursor_line != Some(cursor_line) {
                    self.large_document_list.scroll_to_reveal_item(cursor_line);
                    self.large_document_cursor_line = Some(cursor_line);
                }
            }

            if let Some(callback) = self.on_outline_viewport_change.clone() {
                let ranges = line_ranges.clone();
                self.large_document_list
                    .set_scroll_handler(move |event, _, cx| {
                        let source_byte = ranges
                            .get(event.visible_range.start)
                            .map(|range| range.start)
                            .unwrap_or(0);
                        callback(source_byte, cx);
                    });
            }
        }
        let pending_reveal = (!virtualized_document).then_some(pending_reveal).flatten();
        let outline_callback = self.on_outline_viewport_change.clone();
        let outline_scroll_handle = self.scroll_handle.clone();
        let outline_projection = projection.clone();
        let editor_scroll_handle = self.scroll_handle.clone();
        let editor_scrollbar_state = self.scrollbar_state.clone();
        let input_enabled = !self.search_active;
        let input_focus_handle = focus_handle.clone();
        let input_entity = cx.entity();
        let selected_component_for_actions = self.selected_component.clone();
        let context_menu = self
            .context_menu_position
            .map(|position| build_editor_context_menu(position, has_selection, cx.entity()));

        let search_match_display = if search_match_count == 0 {
            0
        } else {
            self.search_current_match.min(search_match_count - 1) + 1
        };

        let editor_overlay = div()
            .absolute()
            .top_0()
            .right(px(EDITOR_HORIZONTAL_PADDING + EDITOR_SCROLLBAR_WIDTH))
            .bottom_0()
            .left(px(editor_left_padding))
            .child(
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

                        if !virtualized_document
                            && !had_pending_reveal
                            && let Some(callback) = outline_callback.as_ref()
                            && let Some(source_byte) = source_byte_at_viewport_activation_line(
                                &outline_scroll_handle,
                                &text_layout_for_sync,
                                outline_projection.as_ref(),
                            )
                        {
                            callback(source_byte, cx);
                        }
                    },
                    move |bounds: Bounds<_>, (), window: &mut Window, cx: &mut App| {
                        if input_enabled {
                            window.handle_input(
                                &input_focus_handle,
                                ElementInputHandler::new(bounds, input_entity.clone()),
                                cx,
                            );
                        }

                        let caret_pos = std::panic::catch_unwind(AssertUnwindSafe(|| {
                            text_layout_for_active_line
                                .visible_position_for_index(cursor_display_byte)
                        }))
                        .ok()
                        .flatten();
                        let Some(caret_pos) = caret_pos else {
                            return;
                        };

                        let line_height = std::panic::catch_unwind(AssertUnwindSafe(|| {
                            text_layout_for_active_line
                                .visible_line_height_for_index(cursor_display_byte)
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
                    },
                )
                .size_full(),
            )
            .child(
                canvas(
                    |_, _, _| {},
                    move |_, (), window: &mut Window, _cx: &mut App| {
                        if !draw_caret {
                            return;
                        }
                        let caret_pos = std::panic::catch_unwind(AssertUnwindSafe(|| {
                            text_layout_for_caret.visible_position_for_index(cursor_display_byte)
                        }))
                        .ok()
                        .flatten();
                        let Some(caret_pos) = caret_pos else {
                            return;
                        };
                        let line_height = std::panic::catch_unwind(AssertUnwindSafe(|| {
                            text_layout_for_caret.visible_line_height_for_index(cursor_display_byte)
                        }))
                        .ok()
                        .unwrap_or(px(0.));
                        if line_height <= px(0.) {
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
                .size_full(),
            );

        div()
            .relative()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.context_menu_position.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .child(
                editor_scroll_container()
                    .bg(Theme::panel())
                    .pl(px(editor_left_padding))
                    .pr(px(EDITOR_HORIZONTAL_PADDING))
                    .text_size(px(editor_font_size))
                    .text_color(markdown_style.foreground)
                    .font_family("Menlo")
                    .overflow_x_hidden()
                    .scrollbar_width(px(EDITOR_SCROLLBAR_WIDTH))
                    .when(!virtualized_document, |this| {
                        this.py(px(24.))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll_handle)
                    })
                    .when(virtualized_document, |this| this.overflow_hidden())
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
                        let selected_component = selected_component_for_actions.clone();
                        move |_: &Copy, _window: &mut Window, cx_app: &mut App| {
                            if let Some(source_range) = selected_component.clone() {
                                let text = doc_handle.read_with(cx_app, |doc, _| {
                                    let start = doc.byte_to_char(source_range.start);
                                    let end = doc.byte_to_char(source_range.end);
                                    doc.slice_chars(start..end)
                                });
                                cx_app.write_to_clipboard(ClipboardItem::new_string(text));
                                return;
                            }
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
                        let selected_component = selected_component_for_actions.clone();
                        move |_: &Cut, _window: &mut Window, cx_app: &mut App| {
                            if let Some(source_range) = selected_component.clone() {
                                let text = doc_handle.read_with(cx_app, |doc, _| {
                                    let start = doc.byte_to_char(source_range.start);
                                    let end = doc.byte_to_char(source_range.end);
                                    doc.slice_chars(start..end)
                                });
                                cx_app.write_to_clipboard(ClipboardItem::new_string(text));
                                let _ = doc_handle.update(cx_app, |doc, cx| {
                                    let start = doc.byte_to_char(source_range.start);
                                    let end = doc.byte_to_char(source_range.end);
                                    doc.begin_edit();
                                    doc.delete_range(start..end);
                                    doc.set_cursor(start);
                                    doc.commit_edit();
                                    cx.notify();
                                });
                                return;
                            }
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
                        let editor_handle = cx.entity();
                        let line_ranges_for_pointer = virtual_line_ranges_for_pointer.clone();
                        let complex_link_callback = self.on_complex_link.clone();
                        let layout_for_event = text_layout.clone();
                        let projection_for_event = projection.clone();
                        move |event: &MouseDownEvent, window: &mut Window, cx_app: &mut App| {
                            focus_handle.focus(window);
                            let byte_idx = std::panic::catch_unwind(AssertUnwindSafe(|| {
                                layout_for_event.index_for_position(event.position)
                            }))
                            .ok()
                            .map(|result| match result {
                                Ok(index) => index,
                                Err(index) => index,
                            });
                            if let Some((source_range, ProjectionAtom::Complex(kind))) = byte_idx
                                .and_then(|byte| projection_for_event.atom_at_display_byte(byte))
                            {
                                // Native table cells own their mouse handling so a click can
                                // begin inline editing without revealing the whole table source.
                                if matches!(kind, ComplexMarkdownKind::Table { .. }) {
                                    return;
                                }
                                let repeat_click_edits_source =
                                    component_repeat_click_edits_source(&kind);
                                if event.modifiers.platform
                                    && let Some(callback) = complex_link_callback.as_ref()
                                {
                                    match &kind {
                                        ComplexMarkdownKind::WikiLink { target, .. } => {
                                            callback(target.clone(), cx_app);
                                            return;
                                        }
                                        ComplexMarkdownKind::Footnote { label, .. } => {
                                            callback(format!("^{}", label), cx_app);
                                            return;
                                        }
                                        _ => {}
                                    }
                                }
                                let _ = editor_handle.update(cx_app, |editor, cx| {
                                    editor.activate_complex_component(
                                        source_range,
                                        repeat_click_edits_source,
                                        cx,
                                    );
                                });
                                return;
                            }
                            let source_byte = byte_idx
                                .map(|byte| projection_for_event.display_to_source_byte(byte));
                            let cursor_line = source_byte.and_then(|byte| {
                                line_ranges_for_pointer
                                    .as_ref()
                                    .map(|ranges| line_index_for_byte(ranges.as_slice(), byte))
                            });
                            let _ = editor_handle.update(cx_app, |editor, cx| {
                                if let Some(cursor_line) = cursor_line {
                                    editor.large_document_cursor_line = Some(cursor_line);
                                }
                                if editor.selected_component.take().is_some() {
                                    cx.notify();
                                }
                            });
                            let _ = doc_handle.update(cx_app, |doc, cx| {
                                if let Some((source_range, ProjectionAtom::Task { checked })) =
                                    byte_idx.and_then(|byte| {
                                        projection_for_event.atom_at_display_byte(byte)
                                    })
                                {
                                    let marker_byte = source_range.start.saturating_add(3);
                                    let marker_char = doc.byte_to_char(marker_byte);
                                    doc.begin_edit();
                                    doc.delete_range(marker_char..marker_char.saturating_add(1));
                                    doc.insert(marker_char, if checked { " " } else { "x" });
                                    doc.set_cursor(doc.byte_to_char(source_range.end));
                                    doc.commit_edit();
                                    cx.notify();
                                    return;
                                }
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
                        let editor_handle = cx.entity();
                        let line_ranges_for_pointer = virtual_line_ranges_for_pointer.clone();
                        let layout_for_event = text_layout.clone();
                        let projection_for_event = projection.clone();
                        move |event: &MouseMoveEvent, _window: &mut Window, cx_app: &mut App| {
                            if !event.dragging() {
                                return;
                            }
                            let byte_idx = std::panic::catch_unwind(AssertUnwindSafe(|| {
                                layout_for_event.index_for_position(event.position)
                            }))
                            .ok()
                            .map(|result| match result {
                                Ok(index) => index,
                                Err(index) => index,
                            });
                            let source_byte = byte_idx
                                .map(|byte| projection_for_event.display_to_source_byte(byte));
                            if let Some(cursor_line) = source_byte.and_then(|byte| {
                                line_ranges_for_pointer
                                    .as_ref()
                                    .map(|ranges| line_index_for_byte(ranges.as_slice(), byte))
                            }) {
                                let _ = editor_handle.update(cx_app, |editor, _| {
                                    editor.large_document_cursor_line = Some(cursor_line);
                                });
                            }
                            let _ = doc_handle.update(cx_app, |doc, cx| {
                                if let Some(byte_idx) =
                                    source_byte.map(|byte| doc.byte_to_char(byte))
                                {
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

                            if key == "escape" && this.selected_component.take().is_some() {
                                cx.notify();
                                return;
                            }

                            if matches!(key.as_str(), "backspace" | "delete")
                                && this.delete_selected_component(cx)
                            {
                                return;
                            }

                            if is_cmd {
                                return;
                            }

                            if key == "pageup" || key == "pagedown" {
                                if this.large_document_active {
                                    let page =
                                        this.large_document_list.viewport_bounds().size.height;
                                    if page > px(0.) {
                                        let amount = page * 0.9;
                                        this.large_document_list.scroll_by(if key == "pagedown" {
                                            amount
                                        } else {
                                            -amount
                                        });
                                        cx.notify();
                                        window.refresh();
                                    }
                                    return;
                                }
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
                                        let cursor = doc.cursor.min(len);
                                        let line_index = doc.rope.char_to_line(cursor);
                                        let line_start = doc.rope.line_to_char(line_index);
                                        let line = doc.rope.line(line_index).to_string();
                                        if let Some(prefix) = markdown_line_prefix(&line)
                                            && cursor == line_start + prefix.prefix_chars
                                        {
                                            doc.delete_range(line_start..cursor);
                                            doc.cursor = line_start;
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
                                        if doc.delete_selection().is_none() {
                                            let cursor = doc.cursor.min(len);
                                            let line_index = doc.rope.char_to_line(cursor);
                                            let line_start = doc.rope.line_to_char(line_index);
                                            let line = doc.rope.line(line_index).to_string();
                                            if let Some(prefix) = markdown_line_prefix(&line)
                                                && cursor >= line_start + prefix.prefix_chars
                                            {
                                                if prefix.content_is_empty {
                                                    doc.delete_range(
                                                        line_start
                                                            ..line_start + prefix.prefix_chars,
                                                    );
                                                    doc.cursor = line_start;
                                                } else {
                                                    let insertion =
                                                        format!("\n{}", prefix.continuation);
                                                    doc.insert(cursor, &insertion);
                                                    doc.cursor += insertion.chars().count();
                                                }
                                                doc.commit_edit();
                                                cx_doc.notify();
                                                return;
                                            }
                                        }
                                        doc.insert(doc.cursor, "\n");
                                        doc.cursor += 1;
                                        doc.commit_edit();
                                        cx_doc.notify();
                                    }
                                    "tab" => {
                                        doc.begin_edit();
                                        let cursor = doc.cursor.min(len);
                                        let line_index = doc.rope.char_to_line(cursor);
                                        let line_start = doc.rope.line_to_char(line_index);
                                        if shift {
                                            let line = doc.rope.line(line_index).to_string();
                                            let removable = line
                                                .bytes()
                                                .take(2)
                                                .take_while(|byte| *byte == b' ')
                                                .count();
                                            if removable > 0 {
                                                doc.delete_range(
                                                    line_start..line_start + removable,
                                                );
                                                doc.cursor = cursor.saturating_sub(removable);
                                            }
                                        } else {
                                            doc.insert(line_start, "  ");
                                            doc.cursor = cursor.saturating_add(2);
                                        }
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
                    .child(editor_text_element)
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
            .child(editor_overlay)
            .children(context_menu)
            .child(if virtualized_document {
                render_interactive_list_scrollbar(
                    editor_scrollbar_state,
                    self.large_document_list.clone(),
                    Theme::muted().into(),
                )
            } else {
                render_interactive_scrollbar(
                    InteractiveScrollbarAxis::Vertical,
                    editor_scrollbar_state,
                    editor_scroll_handle,
                    Theme::muted().into(),
                )
            })
    }
}

/// Resolves a byte offset to the virtual source row that owns it.
fn line_index_for_byte(lines: &[Range<usize>], byte: usize) -> usize {
    lines
        .partition_point(|line| line.start <= byte)
        .saturating_sub(1)
        .min(lines.len().saturating_sub(1))
}

/// Converts source-based highlights into ranges visible in the current projection.
fn project_editor_highlights(
    projection: &DisplayProjection,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    highlights
        .into_iter()
        .filter_map(|(range, style)| projection.project_range(&range).map(|range| (range, style)))
        .collect()
}

/// Returns syntax colors and inline typography for the editable Markdown presentation.
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
        SyntaxKind::InlineCodeMarker => HighlightStyle {
            color: Some(markdown_style.foreground.into()),
            ..Default::default()
        },
        SyntaxKind::InlineCode => HighlightStyle {
            color: Some(markdown_style.foreground.into()),
            background_color: Some(markdown_style.code_background.into()),
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
        SyntaxKind::EmphasisText => HighlightStyle {
            font_style: Some(FontStyle::Italic),
            ..Default::default()
        },
        SyntaxKind::StrongText => HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        },
        SyntaxKind::StrikethroughMarker => HighlightStyle {
            color: Some(markdown_style.muted_foreground.into()),
            ..Default::default()
        },
        SyntaxKind::StrikethroughText => HighlightStyle {
            strikethrough: Some(StrikethroughStyle {
                thickness: px(1.),
                ..Default::default()
            }),
            ..Default::default()
        },
        SyntaxKind::HorizontalRule => HighlightStyle {
            color: Some(markdown_style.muted_foreground.into()),
            ..Default::default()
        },
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
    use crate::ui::editor_text_layout::editor_line_ranges;

    #[test]
    fn virtual_line_lookup_handles_boundaries_and_trailing_empty_lines() {
        let lines = editor_line_ranges("a\n\nb\n");

        assert_eq!(line_index_for_byte(&lines, 0), 0);
        assert_eq!(line_index_for_byte(&lines, 2), 1);
        assert_eq!(line_index_for_byte(&lines, 3), 2);
        assert_eq!(line_index_for_byte(&lines, 5), 3);

        let split_line = vec![0..16_384, 16_384..20_000];
        assert_eq!(line_index_for_byte(&split_line, 16_384), 1);
    }

    #[test]
    fn editor_scroll_uses_block_layout_to_avoid_intrinsic_height_overflow() {
        let mut scroll = editor_scroll_container();

        assert_eq!(scroll.style().display, None);
    }

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
        assert_eq!(projection.display_text.as_ref(), source);
    }

    #[test]
    fn display_projection_preserves_heading_marker_and_following_space() {
        let source = "# Hi";
        let projection = DisplayProjection::from_source(source);
        assert_eq!(projection.display_text.as_ref(), source);
    }

    #[test]
    fn display_projection_maps_link_end_to_source_end() {
        let source = "![cola-2.png](<assets/cola-2.png>)";
        let projection = DisplayProjection::from_source(source);

        assert_eq!(projection.display_text.as_ref(), source);
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
        assert_eq!(projection.display_text.as_ref(), source);
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

    #[test]
    fn structured_line_prefixes_produce_expected_continuations() {
        assert_eq!(markdown_line_prefix("- item\n").unwrap().continuation, "- ");
        assert_eq!(
            markdown_line_prefix("  - [x] done").unwrap().continuation,
            "  - [ ] "
        );
        assert_eq!(
            markdown_line_prefix("9. item").unwrap().continuation,
            "10. "
        );
        assert_eq!(markdown_line_prefix("> quote").unwrap().continuation, "> ");
        assert!(markdown_line_prefix("- ").unwrap().content_is_empty);
    }

    #[test]
    fn editable_code_highlights_preserve_utf8_boundaries() {
        let text = "fn main() { println!(\"你好\"); }";
        let highlights = code_syntax_highlights("rust", text, CodeSyntaxTheme::AyuLight);

        assert!(!highlights.is_empty());
        assert_eq!(highlights.last().unwrap().range.end, text.len());
        assert!(highlights.iter().all(|highlight| {
            text.is_char_boundary(highlight.range.start)
                && text.is_char_boundary(highlight.range.end)
        }));
    }

    #[test]
    fn hovered_code_block_reveals_the_whole_block_instead_of_only_the_cursor_line() {
        let cursor_line = 18..30;
        let hovered_block = 7..42;

        assert_eq!(
            projection_active_source_range(cursor_line, Some(hovered_block.clone()), false, 100,),
            hovered_block
        );
        assert_eq!(
            projection_active_source_range(18..30, Some(7..42), true, 100),
            100..100
        );
    }

    #[test]
    fn cursor_inside_code_block_keeps_the_whole_block_source_visible() {
        let source = "before\n```rust\nfn main() {}\n```\nafter";
        let blocks = fenced_code_blocks(source);
        let cursor_byte = source.find("main").unwrap();

        assert_eq!(
            code_block_range_for_source_range(&blocks, &(cursor_byte..cursor_byte)),
            Some(blocks[0].source_range.clone())
        );
    }
}
