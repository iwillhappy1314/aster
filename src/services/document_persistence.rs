use crate::error::AppResult;
use crate::model::document::DocumentState;
use crate::services::fs::{read_to_string, write_atomic};
use camino::{Utf8Path, Utf8PathBuf};
use ropey::Rope;

/// Immutable content and identity captured for one document save attempt.
#[derive(Clone, Debug)]
pub struct DocumentSaveSnapshot {
    pub rope: Rope,
    pub revision: u64,
    pub original_path: Option<Utf8PathBuf>,
}

/// Pure data produced after a document snapshot has been durably written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentWriteReceipt {
    pub content_hash: u64,
}

/// Pure data read and parsed on a background thread before entering GPUI state.
#[derive(Clone, Debug)]
pub struct LoadedDocumentSnapshot {
    pub rope: Rope,
    pub content_hash: u64,
}

/// Describes every terminal state of a document save request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentSaveOutcome {
    Saved {
        document_unchanged: bool,
    },
    Cancelled,
    /// Reserved for the external-modification guard implemented in phase 4.
    #[allow(dead_code)]
    Conflict {
        path: Utf8PathBuf,
    },
    Failed {
        path: Option<Utf8PathBuf>,
        message: String,
    },
}

impl DocumentSaveSnapshot {
    /// Captures the exact content that a save attempt must write to disk.
    pub fn capture(document: &DocumentState) -> Self {
        Self {
            rope: document.rope.clone(),
            revision: document.revision,
            original_path: document.path.clone(),
        }
    }

    /// Converts the captured Rope to bytes only inside the background write task.
    pub fn contents(&self) -> String {
        self.rope.to_string()
    }

    /// Computes the captured Rope hash without reading live document state.
    pub fn content_hash(&self) -> u64 {
        DocumentState::rope_content_hash(&self.rope)
    }
}

/// Writes one immutable document snapshot and returns the hash persisted to disk.
pub fn write_document_snapshot(
    path: &Utf8Path,
    snapshot: &DocumentSaveSnapshot,
) -> AppResult<DocumentWriteReceipt> {
    let contents = snapshot.contents();
    let content_hash = snapshot.content_hash();
    write_atomic(path.as_std_path(), contents.as_bytes())?;
    Ok(DocumentWriteReceipt { content_hash })
}

/// Reads, decodes, and parses one document without accessing GPUI entity state.
pub fn read_document_snapshot(path: &Utf8PathBuf) -> AppResult<LoadedDocumentSnapshot> {
    let contents = read_to_string(path)?;
    let rope = Rope::from_str(&contents);
    let content_hash = DocumentState::rope_content_hash(&rope);
    Ok(LoadedDocumentSnapshot { rope, content_hash })
}

/// Applies a successful save only to its originating document entity.
pub fn apply_document_save(
    document: &mut DocumentState,
    path: Utf8PathBuf,
    snapshot: &DocumentSaveSnapshot,
    receipt: DocumentWriteReceipt,
) -> bool {
    document.path = Some(path);
    document.record_saved_snapshot(snapshot.revision, receipt.content_hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the same receipt a completed background write would return.
    fn receipt_for(snapshot: &DocumentSaveSnapshot) -> DocumentWriteReceipt {
        DocumentWriteReceipt {
            content_hash: snapshot.content_hash(),
        }
    }

    #[test]
    fn matching_snapshot_updates_path_and_clears_dirty() {
        let mut document = DocumentState::new_empty();
        document.insert(0, "saved text");
        let snapshot = DocumentSaveSnapshot::capture(&document);
        let path = Utf8PathBuf::from("notes.md");

        let receipt = receipt_for(&snapshot);
        let unchanged = apply_document_save(&mut document, path.clone(), &snapshot, receipt);

        assert_eq!(
            (unchanged, document.dirty, document.path),
            (true, false, Some(path))
        );
    }

    #[test]
    fn newer_edits_remain_dirty_after_older_snapshot_is_saved() {
        let mut document = DocumentState::new_empty();
        document.insert(0, "saved text");
        let snapshot = DocumentSaveSnapshot::capture(&document);
        document.insert(document.len_chars(), " plus newer edit");

        let receipt = receipt_for(&snapshot);
        let unchanged = apply_document_save(
            &mut document,
            Utf8PathBuf::from("notes.md"),
            &snapshot,
            receipt,
        );

        assert_eq!(
            (unchanged, document.dirty, document.last_saved_hash),
            (false, true, receipt.content_hash)
        );
    }

    #[test]
    fn applying_a_save_never_changes_another_document() {
        let mut originating_document = DocumentState::new_empty();
        originating_document.insert(0, "document A");
        let snapshot = DocumentSaveSnapshot::capture(&originating_document);
        let mut active_document = DocumentState::new_empty();
        active_document.insert(0, "document B");

        let receipt = receipt_for(&snapshot);
        apply_document_save(
            &mut originating_document,
            Utf8PathBuf::from("document-a.md"),
            &snapshot,
            receipt,
        );

        assert_eq!(
            (
                originating_document.path,
                active_document.path.clone(),
                active_document.text(),
                active_document.dirty,
            ),
            (
                Some(Utf8PathBuf::from("document-a.md")),
                None,
                "document B".to_string(),
                true,
            )
        );
    }
}
