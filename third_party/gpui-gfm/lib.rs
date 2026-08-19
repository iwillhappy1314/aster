//! gpui-gfm — GitHub Flavored Markdown parser and renderers.
//!
//! # Architecture
//!
//! - [`types`] — Intermediate representation (Block / Inline).
//! - [`parse`] — Markdown → IR (comrak-based with details/HTML pre-processing).
//! - [`html`] — IR → HTML, available without GPUI via the `html` feature.
//! - [`render`] — IR → GPUI elements (`gpui-render`, enabled by default).
//! - [`estimate`] — Height estimation for virtual scrolling.
//! - [`github`] — GitHub-specific utilities (blob line references, etc.).
//! - [`cache`] — LRU cache for parsed markdown documents.

pub mod cache;
pub mod estimate;
pub mod github;
#[cfg(feature = "html")]
pub mod html;
pub mod parse;
#[cfg(feature = "gpui-render")]
pub mod render;
pub mod types;

pub use cache::MarkdownCache;
pub use github::{GithubCodeReferencePreview, GithubIssueReferenceContext};
#[cfg(feature = "html")]
pub use html::{render_html, render_markdown_html, render_markdown_html_document};
pub use parse::{parse_gfm, parse_markdown};
#[cfg(feature = "gpui-render")]
pub use render::{
  DetailsState, ImageLoaderFn, ListItemView, MarkdownRenderOptions, MarkdownTheme, RenderOverrides,
  RenderedMarkdownBlocks, render_markdown, render_markdown_blocks_cached, render_markdown_cached,
  render_parsed_markdown, render_parsed_markdown_blocks,
};
pub use types::ParsedMarkdown;
