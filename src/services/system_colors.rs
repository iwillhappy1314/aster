use gpui::Rgba;

/// Returns the platform selection background, or the supplied theme color when unavailable.
pub fn selection_background(fallback: Rgba) -> Rgba {
    platform_selection_background().unwrap_or(fallback)
}

/// Returns the platform control accent, which matches macOS automatic folder color.
pub fn control_accent(fallback: Rgba) -> Rgba {
    platform_control_accent().unwrap_or(fallback)
}

#[cfg(target_os = "macos")]
/// Resolves the user's macOS text-selection color in the sRGB color space.
fn platform_selection_background() -> Option<Rgba> {
    use objc2_app_kit::NSColor;

    color_in_s_rgb(&NSColor::selectedTextBackgroundColor())
}

#[cfg(target_os = "macos")]
/// Resolves the user's macOS control accent, used by automatic Finder folders.
fn platform_control_accent() -> Option<Rgba> {
    use objc2_app_kit::NSColor;

    color_in_s_rgb(&NSColor::controlAccentColor())
}

#[cfg(target_os = "macos")]
/// Converts a dynamic AppKit color to sRGB components suitable for GPUI rendering.
fn color_in_s_rgb(color: &objc2_app_kit::NSColor) -> Option<Rgba> {
    use objc2_app_kit::NSColorSpace;

    let s_rgb = NSColorSpace::sRGBColorSpace();
    let color = color.colorUsingColorSpace(&s_rgb)?;

    Some(Rgba {
        r: color.redComponent() as f32,
        g: color.greenComponent() as f32,
        b: color.blueComponent() as f32,
        a: color.alphaComponent() as f32,
    })
}

#[cfg(not(target_os = "macos"))]
/// Leaves non-macOS builds on their configured application theme color.
fn platform_selection_background() -> Option<Rgba> {
    None
}

#[cfg(not(target_os = "macos"))]
/// Leaves non-macOS builds on their configured application theme color.
fn platform_control_accent() -> Option<Rgba> {
    None
}

#[cfg(test)]
mod tests {
    use super::selection_background;
    use gpui::rgb;

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn keeps_the_theme_color_when_the_platform_has_no_system_color() {
        let fallback = rgb(0x123456);
        assert_eq!(selection_background(fallback), fallback);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn resolves_a_valid_macos_system_highlight_color() {
        let color = selection_background(rgb(0x123456));
        assert!((0.0..=1.0).contains(&color.r));
        assert!((0.0..=1.0).contains(&color.g));
        assert!((0.0..=1.0).contains(&color.b));
        assert!((0.0..=1.0).contains(&color.a));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn resolves_a_valid_macos_control_accent_color() {
        let color = super::control_accent(rgb(0x123456));
        assert!((0.0..=1.0).contains(&color.r));
        assert!((0.0..=1.0).contains(&color.g));
        assert!((0.0..=1.0).contains(&color.b));
        assert_eq!(color.a, 1.0);
    }
}
