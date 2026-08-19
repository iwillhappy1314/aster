pub use gpui_component_upstream::{init, notification, theme};
pub use gpui_component_upstream::{ActiveTheme, StyledExt};

/// Compatibility layer for the small subset of `gpui-component::text` used by Aster.
///
/// Aster keeps its existing `TextView::markdown(...).style(...).selectable(...)`
/// call sites, while Markdown rendering is delegated to `gpui-gfm`.
pub mod text {
    use gpui::prelude::*;
    use gpui::{
        App, ElementId, IntoElement, Rems, RenderOnce, SharedString, StyleRefinement, Styled,
        Window, div, rems,
    };
    use gpui_component_upstream::theme::ActiveTheme as _;
    use gpui_gfm::{MarkdownRenderOptions, MarkdownTheme, render_markdown};

    /// Compatibility style used by Aster's existing preview call site.
    ///
    /// `gpui-gfm` owns block spacing itself, so `paragraph_gap` is retained to
    /// keep the old API source-compatible but is not applied a second time.
    #[derive(Clone)]
    pub struct TextViewStyle {
        pub paragraph_gap: Rems,
    }

    impl Default for TextViewStyle {
        fn default() -> Self {
            Self {
                paragraph_gap: rems(1.),
            }
        }
    }

    impl TextViewStyle {
        pub fn paragraph_gap(mut self, gap: Rems) -> Self {
            self.paragraph_gap = gap;
            self
        }
    }

    #[derive(IntoElement)]
    pub struct TextView {
        _id: ElementId,
        markdown: SharedString,
        style: StyleRefinement,
        _text_style: TextViewStyle,
        _selectable: bool,
    }

    impl TextView {
        pub fn markdown(
            id: impl Into<ElementId>,
            markdown: impl Into<SharedString>,
            _window: &mut Window,
            _cx: &mut App,
        ) -> Self {
            Self {
                _id: id.into(),
                markdown: markdown.into(),
                style: StyleRefinement::default(),
                _text_style: TextViewStyle::default(),
                _selectable: false,
            }
        }

        pub fn style(mut self, style: TextViewStyle) -> Self {
            self._text_style = style;
            self
        }

        pub fn selectable(mut self, selectable: bool) -> Self {
            // gpui-gfm renders text with its selectable text element already.
            self._selectable = selectable;
            self
        }
    }

    impl Styled for TextView {
        fn style(&mut self) -> &mut StyleRefinement {
            &mut self.style
        }
    }

    impl RenderOnce for TextView {
        fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
            let theme = cx.theme();
            let markdown_theme = MarkdownTheme {
                foreground: theme.foreground,
                muted_foreground: theme.muted_foreground,
                background: theme.background,
                code_background: theme.muted,
                border: theme.border,
                link: theme.link,
                accent: theme.accent,
                code_font_family: "Menlo".into(),
                is_dark: theme.is_dark(),
            };

            let options = MarkdownRenderOptions::default().with_theme(markdown_theme);
            let content = render_markdown(self.markdown.as_ref(), &options, cx);

            let mut wrapper = div().w_full();
            *wrapper.style() = self.style;
            wrapper.child(content)
        }
    }
}
