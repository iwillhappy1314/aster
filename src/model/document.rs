use std::ops::Range;
use std::path::PathBuf;

use ropey::Rope;

use crate::model::undo::{EditOperation, UndoHistory};

#[derive(Clone)]
pub struct DocumentState {
    path: Option<PathBuf>,
    rope: Rope,
    cursor: usize,
    selection: Option<Range<usize>>,
    selection_anchor: Option<usize>,
    dirty: bool,
    undo_history: UndoHistory,
    revision: u64,
    word_count_cache: Option<(u64, usize)>,
    saved_text: String,
}

impl DocumentState {
    pub fn new_empty() -> Self {
        Self {
            path: None,
            rope: Rope::new(),
            cursor: 0,
            selection: None,
            selection_anchor: None,
            dirty: false,
            undo_history: UndoHistory::default(),
            revision: 0,
            word_count_cache: None,
            saved_text: String::new(),
        }
    }

    pub fn from_text(text: &str, path: Option<PathBuf>) -> Self {
        Self {
            path,
            rope: Rope::from_str(text),
            cursor: 0,
            selection: None,
            selection_anchor: None,
            dirty: false,
            undo_history: UndoHistory::default(),
            revision: 0,
            word_count_cache: None,
            saved_text: text.to_string(),
        }
    }

    pub fn path(&self) -> Option<&PathBuf> {
        self.path.as_ref()
    }

    pub fn set_path(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn set_cursor(&mut self, cursor: usize) {
        self.cursor = cursor.min(self.len_chars());
        self.clear_selection();
    }

    pub fn selection_range(&self) -> Option<Range<usize>> {
        self.selection.clone()
    }

    pub fn selection_bytes(&self) -> Option<Range<usize>> {
        self.selection.as_ref().map(|selection| {
            let start = self.rope.char_to_byte(selection.start);
            let end = self.rope.char_to_byte(selection.end);
            start..end
        })
    }

    pub fn selection_anchor(&self) -> Option<usize> {
        self.selection_anchor
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
        // Keep logical selection direction separate from the normalized range.
        self.cursor = head;
        self.selection_anchor = Some(anchor);
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.selection_anchor = None;
    }

    pub fn has_selection(&self) -> bool {
        self.selection.is_some()
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection.as_ref().map(|range| {
            self.rope.slice(range.clone()).to_string()
        })
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_saved(&mut self) {
        self.saved_text = self.text();
        self.dirty = false;
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.word_count_cache = None;
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn set_text(&mut self, text: &str) {
        let old_text = self.text();
        let old_cursor = self.cursor;
        let old_selection = self.selection.clone();

        self.rope = Rope::from_str(text);
        self.cursor = self.cursor.min(self.len_chars());
        self.clear_selection();
        self.mark_dirty();
        self.bump_revision();

        let op = EditOperation {
            old_text,
            new_text: text.to_string(),
            old_cursor,
            new_cursor: self.cursor,
            old_selection,
            new_selection: self.selection.clone(),
        };
        self.undo_history.push(op);
    }

    pub fn insert_text(&mut self, text: &str) {
        let old_text = self.text();
        let old_cursor = self.cursor;
        let old_selection = self.selection.clone();

        if let Some(selection) = self.selection.clone() {
            self.rope.remove(selection.clone());
            self.cursor = selection.start;
        }

        self.rope.insert(self.cursor, text);
        self.cursor += text.chars().count();
        self.clear_selection();
        self.mark_dirty();
        self.bump_revision();

        let op = EditOperation {
            old_text,
            new_text: self.text(),
            old_cursor,
            new_cursor: self.cursor,
            old_selection,
            new_selection: self.selection.clone(),
        };
        self.undo_history.push(op);
    }

    pub fn delete_backward(&mut self) {
        let old_text = self.text();
        let old_cursor = self.cursor;
        let old_selection = self.selection.clone();

        if let Some(selection) = self.selection.clone() {
            self.rope.remove(selection.clone());
            self.cursor = selection.start;
            self.clear_selection();
        } else if self.cursor > 0 {
            self.rope.remove(self.cursor - 1..self.cursor);
            self.cursor -= 1;
        } else {
            return;
        }

        self.mark_dirty();
        self.bump_revision();

        let op = EditOperation {
            old_text,
            new_text: self.text(),
            old_cursor,
            new_cursor: self.cursor,
            old_selection,
            new_selection: self.selection.clone(),
        };
        self.undo_history.push(op);
    }

    pub fn delete_forward(&mut self) {
        let old_text = self.text();
        let old_cursor = self.cursor;
        let old_selection = self.selection.clone();

        if let Some(selection) = self.selection.clone() {
            self.rope.remove(selection.clone());
            self.cursor = selection.start;
            self.clear_selection();
        } else if self.cursor < self.len_chars() {
            self.rope.remove(self.cursor..self.cursor + 1);
        } else {
            return;
        }

        self.mark_dirty();
        self.bump_revision();

        let op = EditOperation {
            old_text,
            new_text: self.text(),
            old_cursor,
            new_cursor: self.cursor,
            old_selection,
            new_selection: self.selection.clone(),
        };
        self.undo_history.push(op);
    }

    pub fn get_word_count(&mut self) -> usize {
        if let Some((revision, count)) = self.word_count_cache {
            if revision == self.revision {
                return count;
            }
        }

        let count = self
            .rope
            .chars()
            .collect::<String>()
            .split_whitespace()
            .count();
        self.word_count_cache = Some((self.revision, count));
        count
    }

    pub fn byte_to_char(&self, byte: usize) -> usize {
        self.rope.byte_to_char(byte.min(self.rope.len_bytes()))
    }

    pub fn char_to_byte(&self, char_index: usize) -> usize {
        self.rope.char_to_byte(char_index.min(self.rope.len_chars()))
    }

    pub fn undo(&mut self) -> bool {
        if let Some(op) = self.undo_history.undo() {
            self.rope = Rope::from_str(&op.old_text);
            self.cursor = op.old_cursor.min(self.rope.len_chars());
            self.selection = op.old_selection;
            self.selection_anchor = self.selection.as_ref().map(|r| {
                if self.cursor <= r.start { r.end } else { r.start }
            });
            self.bump_revision();
            self.word_count_cache = None;
            self.dirty = self.text() != self.saved_text;
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some(op) = self.undo_history.redo() {
            self.rope = Rope::from_str(&op.new_text);
            self.cursor = op.new_cursor.min(self.rope.len_chars());
            self.selection = op.new_selection;
            self.selection_anchor = self.selection.as_ref().map(|r| {
                if self.cursor <= r.start { r.end } else { r.start }
            });
            self.bump_revision();
            self.word_count_cache = None;
            self.dirty = self.text() != self.saved_text;
            true
        } else {
            false
        }
    }

    pub fn can_undo(&self) -> bool {
        self.undo_history.can_undo()
    }

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
}
