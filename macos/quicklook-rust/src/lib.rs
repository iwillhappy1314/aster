//! C ABI bridge used by the macOS Quick Look extension.
//!
//! Keep this layer intentionally tiny: Markdown parsing and HTML rendering live
//! in `gpui-gfm`, so Aster and Finder Quick Look share one parser and one IR.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

const QUICK_LOOK_CSS: &str = r#"
:root {
  color-scheme: light dark;
  --bg: #fafafa;
  --fg: #4d4d4c;
  --muted: #8a8a87;
  --border: #d7d7d4;
  --panel: #f0f0ed;
  --code-bg: #efefec;
  --link: #399ee6;
  --quote: #a0a1a7;
}

@media (prefers-color-scheme: dark) {
  :root {
    --bg: #0f1419;
    --fg: #e6e1cf;
    --muted: #8a9199;
    --border: #343f4c;
    --panel: #151a1f;
    --code-bg: #191f26;
    --link: #59c2ff;
    --quote: #707a8c;
  }
}

* { box-sizing: border-box; }
html, body { margin: 0; padding: 0; background: var(--bg); color: var(--fg); }
body.aster-markdown {
  max-width: 980px;
  margin: 0 auto;
  padding: 28px 36px 56px;
  font: 15px/1.65 -apple-system, BlinkMacSystemFont, "SF Pro Text", "Helvetica Neue", sans-serif;
  overflow-wrap: break-word;
  -webkit-font-smoothing: antialiased;
}

h1, h2, h3, h4, h5, h6 { line-height: 1.28; margin: 1.35em 0 .55em; font-weight: 650; }
h1 { font-size: 2em; border-bottom: 1px solid var(--border); padding-bottom: .28em; }
h2 { font-size: 1.55em; border-bottom: 1px solid var(--border); padding-bottom: .22em; }
h3 { font-size: 1.28em; }
h4 { font-size: 1.1em; }
p, ul, ol, blockquote, pre, table, details { margin: .8em 0; }
ul, ol { padding-left: 1.8em; }
li > p { margin: .25em 0; }
.task-list-item { list-style: none; margin-left: -1.25em; }
.task-list-item > input { margin: 0 .5em 0 0; vertical-align: .05em; }

a { color: var(--link); text-decoration: none; }
a:hover { text-decoration: underline; }
blockquote { border-left: 3px solid var(--quote); padding: .05em 1em; color: var(--muted); }
blockquote > :first-child { margin-top: .25em; }
blockquote > :last-child { margin-bottom: .25em; }
hr { border: 0; border-top: 1px solid var(--border); margin: 1.5em 0; }

code, pre { font-family: Menlo, Monaco, "SFMono-Regular", Consolas, monospace; }
code { background: var(--code-bg); border-radius: 4px; padding: .14em .35em; font-size: .9em; }
pre { background: var(--code-bg); border: 1px solid var(--border); border-radius: 7px; padding: 13px 15px; overflow: auto; }
pre code { background: transparent; padding: 0; font-size: 12.5px; line-height: 1.55; white-space: pre; }

.table-scroll { overflow-x: auto; }
table { width: 100%; border-collapse: collapse; border-spacing: 0; }
th, td { border: 1px solid var(--border); padding: 7px 10px; text-align: left; vertical-align: top; }
th { background: var(--panel); font-weight: 600; }
tr:nth-child(even) td { background: var(--panel); }

img { max-width: 100%; height: auto; }
picture { display: inline-block; max-width: 100%; }
details { border: 1px solid var(--border); border-radius: 6px; padding: .55em .8em; }
summary { cursor: default; font-weight: 600; }
.aster-align-center { text-align: center; }
.aster-align-center img { margin-left: auto; margin-right: auto; }
"#;

/// Render Markdown to a complete HTML document.
///
/// The returned pointer is owned by Rust and must be released with
/// [`aster_string_free`]. A null pointer means the input was invalid UTF-8 or
/// the output could not be represented as a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn aster_markdown_to_html(markdown: *const c_char) -> *mut c_char {
  if markdown.is_null() {
    return std::ptr::null_mut();
  }

  // SAFETY: the caller promises a valid, NUL-terminated C string.
  let markdown = match unsafe { CStr::from_ptr(markdown) }.to_str() {
    Ok(value) => value,
    Err(_) => return std::ptr::null_mut(),
  };

  let html = gpui_gfm::render_markdown_html_document(markdown, QUICK_LOOK_CSS);
  match CString::new(html.replace('\0', "\u{fffd}")) {
    Ok(value) => value.into_raw(),
    Err(_) => std::ptr::null_mut(),
  }
}

/// Release a string previously returned by [`aster_markdown_to_html`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn aster_string_free(value: *mut c_char) {
  if value.is_null() {
    return;
  }

  // SAFETY: the pointer was allocated by CString::into_raw in this library.
  drop(unsafe { CString::from_raw(value) });
}
