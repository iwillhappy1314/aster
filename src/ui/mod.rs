pub mod editor;
mod editor_projection;
mod editor_text_layout;
pub mod file_explorer;
pub mod root;
pub mod text_utils;
pub mod theme;

/// Documents above this size use bounded, visible-only rendering paths.
///
/// The threshold is deliberately shared by the editor and preview so a tab never
/// builds both of their full-document layout trees for the same large source.
pub(crate) const VIRTUALIZED_DOCUMENT_BYTES: usize = 128 * 1024;
