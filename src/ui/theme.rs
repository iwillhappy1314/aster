use crate::services::{settings, system_colors};
use gpui::{Rgba, rgb, rgba};
use serde::{Deserialize, Serialize};

/// The set of color palettes Aster ships with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ThemeName {
    #[default]
    AyuLight,
    AyuDark,
    AyuMirage,
}

impl ThemeName {
    /// Returns the human-readable label used in menus.
    pub fn label(self) -> &'static str {
        match self {
            ThemeName::AyuLight => "Ayu Light",
            ThemeName::AyuDark => "Ayu Dark",
            ThemeName::AyuMirage => "Ayu Mirage",
        }
    }

    /// Whether this theme is a dark theme.
    pub fn is_dark(self) -> bool {
        self.colors().is_dark
    }

    /// Resolves the concrete color palette for this theme.
    pub fn colors(self) -> ThemeColors {
        match self {
            ThemeName::AyuLight => ThemeColors {
                bg: rgb(0xfafafa),
                panel: rgb(0xffffff),
                sidebar: rgb(0xf8f8f8),
                panel_alt: rgb(0xf0f0f0),
                code_block_bg: rgb(0xf0f0f0),
                border: rgb(0xe7e7e7),
                text: rgb(0x5c6773),
                muted: rgb(0xabb0b6),
                accent: rgb(0xff9940),
                selection_bg: rgba(0xff994033),
                is_dark: false,
            },
            ThemeName::AyuDark => ThemeColors {
                bg: rgb(0x0b0e14),
                panel: rgb(0x131721),
                sidebar: rgb(0x0e1118),
                panel_alt: rgb(0x1a2029),
                code_block_bg: rgb(0x171b24),
                border: rgb(0x22252e),
                text: rgb(0xbfbdb6),
                muted: rgb(0x565b66),
                accent: rgb(0xe6b450),
                selection_bg: rgba(0xe6b45033),
                is_dark: true,
            },
            ThemeName::AyuMirage => ThemeColors {
                bg: rgb(0x1f2430),
                panel: rgb(0x262b38),
                sidebar: rgb(0x232835),
                panel_alt: rgb(0x2e3442),
                code_block_bg: rgb(0x252b38),
                border: rgb(0x2b3346),
                text: rgb(0xcbccc6),
                muted: rgb(0x707a8c),
                accent: rgb(0xffcc66),
                selection_bg: rgba(0xffcc6633),
                is_dark: true,
            },
        }
    }
}

/// The resolved color palette for a single theme.
pub struct ThemeColors {
    pub bg: Rgba,
    pub panel: Rgba,
    pub sidebar: Rgba,
    pub panel_alt: Rgba,
    pub code_block_bg: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub accent: Rgba,
    pub selection_bg: Rgba,
    pub is_dark: bool,
}

/// Shared Markdown colors used by both the inline editor and rendered preview.
#[derive(Debug, Clone, Copy)]
pub struct MarkdownStyle {
    pub foreground: Rgba,
    pub muted_foreground: Rgba,
    pub background: Rgba,
    pub code_background: Rgba,
    pub border: Rgba,
    pub link: Rgba,
    pub accent: Rgba,
}

/// Namespace for accessing the current theme's colors. The active theme is
/// cached globally and refreshed by [`set_theme`], so these accessors can be
/// called from any render path.
pub struct Theme;

impl Theme {
    /// The currently active theme name.
    pub fn name() -> ThemeName {
        current_theme()
    }

    /// Whether the current theme is dark.
    pub fn is_dark() -> bool {
        current_theme().is_dark()
    }

    pub fn bg() -> Rgba {
        current_colors().bg
    }
    pub fn panel() -> Rgba {
        current_colors().panel
    }
    pub fn sidebar() -> Rgba {
        current_colors().sidebar
    }
    pub fn panel_alt() -> Rgba {
        current_colors().panel_alt
    }
    pub fn code_block_bg() -> Rgba {
        current_colors().code_block_bg
    }
    pub fn border() -> Rgba {
        current_colors().border
    }
    pub fn text() -> Rgba {
        current_colors().text
    }
    pub fn muted() -> Rgba {
        current_colors().muted
    }
    pub fn accent() -> Rgba {
        current_colors().accent
    }
    pub fn selection_bg() -> Rgba {
        system_colors::selection_background(current_colors().selection_bg)
    }
    /// Returns the system control accent for toolbar and navigation active states.
    pub fn control_accent() -> Rgba {
        system_colors::control_accent(current_colors().accent)
    }

    /// Markdown colors shared by the editor's inline styling and the preview renderer.
    pub fn markdown_style() -> MarkdownStyle {
        let colors = current_colors();
        MarkdownStyle {
            foreground: colors.text,
            muted_foreground: colors.muted,
            background: colors.bg,
            code_background: colors.code_block_bg,
            border: colors.border,
            link: Self::control_accent(),
            accent: Self::selection_bg(),
        }
    }
}

fn current_theme() -> ThemeName {
    settings::get_theme_name()
}

fn current_colors() -> ThemeColors {
    current_theme().colors()
}

/// Switches the active theme, persists the choice, and returns whether the new
/// theme is dark (so callers can sync dependent systems like the preview).
pub fn set_theme(name: ThemeName) -> bool {
    settings::set_theme_name(name);
    name.is_dark()
}
