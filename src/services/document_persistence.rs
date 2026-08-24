use crate::error::AppResult;
use crate::model::document::DocumentState;
use crate::services::fs::write_atomic;
use camino::{Utf8Path, Utf8PathBuf};

/// Immutable content and identity captured for one document save attempt.
#[derive(Clone, Debug)]
pub struct DocumentSaveSnapshot {
    pub contents: String,
    pub revision: u64,
    pub content_hash: u64,
    pub original_path: Option<Utf8PathBuf>,
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
            contents: document.text(),
            revision: document.revision,
            content_hash: document.content_hash(),
            original_path: document.path.clone(),
        }
    }
}

/// Writes one immutable document snapshot to its target path.
pub fn write_document_snapshot(path: &Utf8Path, snapshot: &DocumentSaveSnapshot) -> AppResult<()> {
    write_atomic(path.as_std_path(), snapshot.contents.as_bytes())
}

/// Applies a successful save only to its originating document entity.
pub fn apply_document_save(
    document: &mut DocumentState,
    path: Utf8PathBuf,
    snapshot: &DocumentSaveSnapshot,
) -> bool {
    document.path = Some(path);
    document.record_saved_snapshot(snapshot.revision, snapshot.content_hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_snapshot_updates_path_and_clears_dirty() {
        let mut document = DocumentState::new_empty();
        document.insert(0, "saved text");
        let snapshot = DocumentSaveSnapshot::capture(&document);
        let path = Utf8PathBuf::from("notes.md");

        let unchanged = apply_document_save(&mut document, path.clone(), &snapshot);

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

        let unchanged =
            apply_document_save(&mut document, Utf8PathBuf::from("notes.md"), &snapshot);

        assert_eq!(
            (unchanged, document.dirty, document.last_saved_hash),
            (false, true, snapshot.content_hash)
        );
    }

    #[test]
    fn applying_a_save_never_changes_another_document() {
        let mut originating_document = DocumentState::new_empty();
        originating_document.insert(0, "document A");
        let snapshot = DocumentSaveSnapshot::capture(&originating_document);
        let mut active_document = DocumentState::new_empty();
        active_document.insert(0, "document B");

        apply_document_save(
            &mut originating_document,
            Utf8PathBuf::from("document-a.md"),
            &snapshot,
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
