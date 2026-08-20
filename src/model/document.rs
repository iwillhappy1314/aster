use camino::Utf8PathBuf;
use ropey::Rope;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;

use crate::model::undo::{EditChange, EditOperation, UndoHistory};

#[derive(Clone, Debug)]
pub struct EditDelta {
    pub start_char: usize,
    pub old_end_char: usize,
    pub new_end_char: usize,
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
}

#[derive(Clone)]
pub struct DocumentState {
    pub path: Option<Utf8PathBuf>,
    pub rope: Rope,
    pub dirty: bool,
    pub revision: u64,
    pub last_saved_hash: u64,
    pub cursor: usize,
    pub selection: Option<Range<usize>>, // character indices
    pub selection_anchor: Option<usize>, // starting point for shift/drag selections
    /// Cached word count - None means needs recalculation
    word_count_cache: Option<usize>,
    /// Undo/redo history
    pub undo_history: UndoHistory,
    /// Pending edit state for recording operations
    pending_edit: Option<PendingEdit>,
    /// Most recent edit delta, updated on each mutation
    pub last_edit: Option<EditDelta>,
}

/// Temporary state captured before an edit for undo history
#[derive(Clone)]
struct PendingEdit {
    changes: Vec<EditChange>,
    old_cursor: usize,
    old_selection: Option<Range<usize>>,
}

impl DocumentState {
    pub fn new_empty() -> Self {
        Self {
            path: None,
            rope: Rope::new(),
            dirty: false,
            revision: 0,
            last_saved_hash: 0,
            cursor: 0,
            selection: None,
            selection_anchor: None,
            word_count_cache: Some(0),
            undo_history: UndoHistory::default(),
            pending_edit: None,
            last_edit: None,
        }
    }

    pub fn set_text(&mut self, text: &str) {
        let old_chars = self.rope.len_chars();
        let old_bytes = self.rope.len_bytes();
        self.rope = Rope::from_str(text);
        let new_chars = self.rope.len_chars();
        let new_bytes = self.rope.len_bytes();
        self.cursor = self.rope.len_chars();
        self.clear_selection();
        self.bump_revision();
        self.last_edit = Some(EditDelta {
            start_char: 0,
            old_end_char: old_chars,
            new_end_char: new_chars,
            start_byte: 0,
            old_end_byte: old_bytes,
            new_end_byte: new_bytes,
        });
        // Don't compute hash here - save_snapshot will handle dirty state
        // Don't compute word count here - it will be computed lazily
        self.word_count_cache = None;
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    pub fn set_cursor(&mut self, idx: usize) {
        self.cursor = idx.min(self.len_chars());
        self.clear_selection();
    }

    pub fn set_selection(&mut self, anchor: usize, head: usize) {
        let len = self.len_chars();
        let anchor = anchor.min(len);
        let head = head.min(len);
        let (start, end) = if anchor <= head {
            (anchor, head)
        } else {
            (head, anchor)
        };
        self.selection = if start == end {
            None
        } else {
            Some(start..end)
        };
        self.cursor = head;
        self.selection_anchor = Some(anchor);
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.selection_anchor = None;
    }

    pub fn selection_range(&self) -> Option<Range<usize>> {
        self.selection.clone()
    }

    pub fn selection_bytes(&self) -> Option<Range<usize>> {
        self.selection.clone().map(|r| self.char_range_to_bytes(r))
    }

    pub fn delete_selection(&mut self) -> Option<usize> {
        if let Some(range) = self.selection.clone() {
            self.delete_range(range.clone());
            let new_cursor = range.start.min(self.len_chars());
            self.cursor = new_cursor;
            self.clear_selection();
            Some(new_cursor)
        } else {
            None
        }
    }

    pub fn insert(&mut self, char_idx: usize, text: &str) {
        let clamped = char_idx.min(self.rope.len_chars());
        let start_byte = self.rope.char_to_byte(clamped);
        self.rope.insert(clamped, text);

        if !text.is_empty() {
            if let Some(pending) = self.pending_edit.as_mut() {
                pending.changes.push(EditChange::Insert {
                    at: clamped,
                    text: text.to_owned(),
                });
            }
        }

        let new_end_char = clamped.saturating_add(text.chars().count());
        let new_end_byte = start_byte.saturating_add(text.len());
        self.bump_revision();
        self.dirty = true;
        self.last_edit = Some(EditDelta {
            start_char: clamped,
            old_end_char: clamped,
            new_end_char,
            start_byte,
            old_end_byte: start_byte,
            new_end_byte,
        });
        self.clear_selection();
        self.word_count_cache = None; // Invalidate cache
    }

    pub fn delete_range(&mut self, range: Range<usize>) {
        if range.start >= range.end || range.end > self.rope.len_chars() {
            return;
        }
        let start_char = range.start;
        let old_end_char = range.end;
        let start_byte = self.rope.char_to_byte(range.start);
        let old_end_byte = self.rope.char_to_byte(range.end);
        let deleted_text = self.rope.slice(range.clone()).to_string();
        self.rope.remove(range);

        if let Some(pending) = self.pending_edit.as_mut() {
            pending.changes.push(EditChange::Delete {
                at: start_char,
                text: deleted_text,
            });
        }

        self.bump_revision();
        self.dirty = true;
        self.last_edit = Some(EditDelta {
            start_char,
            old_end_char,
            new_end_char: start_char,
            start_byte,
            old_end_byte,
            new_end_byte: start_byte,
        });
        self.cursor = self.cursor.min(self.rope.len_chars());
        self.clear_selection();
        self.word_count_cache = None; // Invalidate cache
    }

    pub fn select_all(&mut self) {
        let len = self.len_chars();
        self.selection = if len == 0 { None } else { Some(0..len) };
        self.selection_anchor = Some(0);
        self.cursor = len;
    }

    pub fn char_to_byte(&self, char_idx: usize) -> usize {
        let clamped = char_idx.min(self.len_chars());
        self.rope.char_to_byte(clamped)
    }

    pub fn byte_to_char(&self, byte_idx: usize) -> usize {
        let clamped = byte_idx.min(self.len_bytes());
        self.rope.byte_to_char(clamped)
    }

    pub fn char_range_to_bytes(&self, range: Range<usize>) -> Range<usize> {
        let start = self.char_to_byte(range.start);
        let end = self.char_to_byte(range.end);
        start..end
    }

    pub fn slice_chars(&self, range: Range<usize>) -> String {
        self.rope.slice(range).to_string()
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn save_snapshot(&mut self) {
        self.last_saved_hash = self.current_hash();
        self.dirty = false;
    }

    fn current_hash(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.rope.hash(&mut h);
        h.finish()
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Get word count, computing it if not cached
    pub fn get_word_count(&mut self) -> usize {
        if let Some(count) = self.word_count_cache {
            count
        } else {
            // Count words by iterating through rope chunks to avoid full string allocation
            let count = self
                .rope
                .chunks()
                .flat_map(|chunk| chunk.split_whitespace())
                .count();
            self.word_count_cache = Some(count);
            count
        }
    }

    // ============ Undo/Redo Methods ============

    /// Begin recording an edit operation - call before making changes
    pub fn begin_edit(&mut self) {
        self.pending_edit = Some(PendingEdit {
            changes: Vec::new(),
            old_cursor: self.cursor,
            old_selection: self.selection.clone(),
        });
    }

    /// Commit the pending edit to history - call after making changes
    pub fn commit_edit(&mut self) {
        if let Some(pending) = self.pending_edit.take() {
            if pending.changes.is_empty() {
                return;
            }

            let op = EditOperation {
                changes: pending.changes,
                old_cursor: pending.old_cursor,
                new_cursor: self.cursor,
                old_selection: pending.old_selection,
                new_selection: self.selection.clone(),
            };
            self.undo_history.push(op);
        }
    }

    /// Undo the last edit operation
    pub fn undo(&mut self) -> bool {
        if let Some(op) = self.undo_history.undo() {
            for change in op.changes.iter().rev() {
                match change {
                    EditChange::Insert { at, text } => {
                        let end = at.saturating_add(text.chars().count());
                        if *at < end && end <= self.rope.len_chars() {
                            self.rope.remove(*at..end);
                        }
                    }
                    EditChange::Delete { at, text } => {
                        self.rope.insert((*at).min(self.rope.len_chars()), text);
                    }
                }
            }
            self.cursor = op.old_cursor.min(self.rope.len_chars());
            self.selection = op.old_selection;
            self.selection_anchor = self.selection.as_ref().map(|r| {
                if self.cursor <= r.start { r.end } else { r.start }
            });
            self.bump_revision();
            self.word_count_cache = None;
            // Update dirty state: dirty if current content differs from saved
            self.dirty = self.current_hash() != self.last_saved_hash;
            true
        } else {
            false
        }
    }

    /// Redo the last undone operation
    pub fn redo(&mut self) -> bool {
        if let Some(op) = self.undo_history.redo() {
            for change in &op.changes {
                match change {
                    EditChange::Insert { at, text } => {
                        self.rope.insert((*at).min(self.rope.len_chars()), text);
                    }
                    EditChange::Delete { at, text } => {
                        let end = at.saturating_add(text.chars().count());
                        if *at < end && end <= self.rope.len_chars() {
                            self.rope.remove(*at..end);
                        }
                    }
                }
            }
            self.cursor = op.new_cursor.min(self.rope.len_chars());
            self.selection = op.new_selection;
            self.selection_anchor = self.selection.as_ref().map(|r| {
                if self.cursor <= r.start { r.end } else { r.start }
            });
            self.bump_revision();
            self.word_count_cache = None;
            // Update dirty state: dirty if current content differs from saved
            self.dirty = self.current_hash() != self.last_saved_hash;
            true
        } else {
            false
        }
    }

    /// Clear undo history (called when opening a new file)
    pub fn clear_undo_history(&mut self) {
        self.undo_history.clear();
        self.pending_edit = None;
    }

    /// Check if undo is available
    #[allow(dead_code)]
    pub fn can_undo(&self) -> bool {
        self.undo_history.can_undo()
    }

    /// Check if redo is available
    #[allow(dead_code)]
    pub fn can_redo(&self) -> bool {
        self.undo_history.can_redo()
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    #[test]
    fn preserves_reverse_selection_direction() {
        let mut doc = DocumentState::new_empty();
        doc.set_text("abcdef");
        doc.set_selection(5, 2);

        assert_eq!(doc.selection_range(), Some(2..5));
        assert_eq!(doc.selection_anchor, Some(5));
        assert_eq!(doc.cursor, 2);

        let anchor = doc.selection_anchor.unwrap();
        doc.set_selection(anchor, 1);
        assert_eq!(doc.selection_range(), Some(1..5));
        assert_eq!(doc.selection_anchor, Some(5));
        assert_eq!(doc.cursor, 1);
    }

    #[test]
    fn undo_redo_insert_uses_delta_changes() {
        let mut doc = DocumentState::new_empty();
        doc.set_text("hello");
        doc.save_snapshot();
        doc.clear_undo_history();

        doc.begin_edit();
        doc.insert(5, " world");
        doc.cursor = 11;
        doc.commit_edit();

        assert_eq!(doc.text(), "hello world");
        assert!(doc.undo());
        assert_eq!(doc.text(), "hello");
        assert!(doc.redo());
        assert_eq!(doc.text(), "hello world");
    }

    #[test]
    fn undo_redo_replacement_restores_selection_text() {
        let mut doc = DocumentState::new_empty();
        doc.set_text("hello world");
        doc.save_snapshot();
        doc.clear_undo_history();
        doc.set_selection(6, 11);

        doc.begin_edit();
        doc.delete_selection();
        let at = doc.cursor;
        doc.insert(at, "Aster");
        doc.cursor = at + 5;
        doc.commit_edit();

        assert_eq!(doc.text(), "hello Aster");
        assert!(doc.undo());
        assert_eq!(doc.text(), "hello world");
        assert!(doc.redo());
        assert_eq!(doc.text(), "hello Aster");
    }
}
