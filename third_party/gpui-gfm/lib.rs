//! gpui-gfm — GitHub Flavored Markdown renderer for GPUI.
//!
//! # Architecture
//!
//! - [`types`] — Intermediate representation (Block / Inline).
//! - [`parse`] — Markdown → IR (comrak-based with details/HTML pre-processing).
//! - [`render`] — IR → GPUI elements.
//! - [`estimate`] — Height estimation for virtual scrolling.
//! - [`github`] — GitHub-specific utilities (blob line references, etc.).
//! - [`cache`] — LRU cache for parsed markdown documents.

pub mod cache;
pub mod estimate;
pub mod github;
pub mod parse;
pub mod render;
pub mod types;

// Re-export main public API.
pub use cache::MarkdownCache;
pub use github::{GithubCodeReferencePreview, GithubIssueReferenceContext};
pub use parse::{parse_gfm, parse_markdown};
pub use render::{
  DetailsState, ImageLoaderFn, ListItemView, MarkdownRenderOptions, MarkdownTheme, RenderOverrides,
  render_markdown, render_markdown_cached, render_parsed_markdown,
};
pub use types::ParsedMarkdown;
