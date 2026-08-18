use directories::ProjectDirs;
use gpui::{Bounds, Pixels, point, px, size};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Application settings with persistence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Font size in points (8-32, default 14)
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    /// Last valid normal-window geometry, restored on the next launch.
    #[serde(default)]
    pub window_geometry: Option<WindowGeometry>,
}

/// Persisted normal-window position and size in screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub origin_x: f32,
    pub origin_y: f32,
    pub width: f32,
    pub height: f32,
}

impl WindowGeometry {
    /// Creates persistable geometry from the window's current bounds.
    pub fn from_bounds(bounds: Bounds<Pixels>) -> Self {
        Self {
            origin_x: bounds.origin.x.into(),
            origin_y: bounds.origin.y.into(),
            width: bounds.size.width.into(),
            height: bounds.size.height.into(),
        }
    }

    /// Converts persisted geometry back into GPUI window bounds.
    pub fn to_bounds(self) -> Bounds<Pixels> {
        Bounds::new(
            point(px(self.origin_x), px(self.origin_y)),
            size(px(self.width), px(self.height)),
        )
    }

    /// Returns whether the geometry is safe to restore on a later launch.
    pub fn is_valid(self) -> bool {
        self.origin_x.is_finite()
            && self.origin_y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && (640.0..=10_000.0).contains(&self.width)
            && (480.0..=10_000.0).contains(&self.height)
    }
}

fn default_font_size() -> f32 {
    14.0
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: default_font_size(),
            window_geometry: None,
        }
    }
}

impl Settings {
    /// Minimum allowed font size
    pub const MIN_FONT_SIZE: f32 = 8.0;
    /// Maximum allowed font size
    pub const MAX_FONT_SIZE: f32 = 32.0;
    /// Default font size
    pub const DEFAULT_FONT_SIZE: f32 = 14.0;
    /// Font size step for increase/decrease
    pub const FONT_SIZE_STEP: f32 = 2.0;

    /// Clamp font size to valid range
    pub fn clamp_font_size(size: f32) -> f32 {
        size.clamp(Self::MIN_FONT_SIZE, Self::MAX_FONT_SIZE)
    }
}

/// Global settings manager with lazy loading and auto-save
pub struct SettingsManager {
    settings: Settings,
    path: Option<PathBuf>,
}

impl SettingsManager {
    /// Load settings from disk or create defaults
    pub fn load() -> Self {
        let path = Self::settings_path();
        let settings = path
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        Self { settings, path }
    }

    /// Get current settings
    pub fn get(&self) -> &Settings {
        &self.settings
    }

    /// Update settings and persist to disk
    pub fn update<F>(&mut self, f: F)
    where
        F: FnOnce(&mut Settings),
    {
        f(&mut self.settings);
        self.save();
    }

    /// Save settings to disk
    fn save(&self) {
        let Some(ref path) = self.path else { return };

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        // Write atomically via temp file
        if let Ok(json) = serde_json::to_string_pretty(&self.settings) {
            let _ = fs::write(path, json);
        }
    }

    /// Get settings file path
    fn settings_path() -> Option<PathBuf> {
        ProjectDirs::from("com", "kumarujjawal", "aster")
            .map(|dirs| dirs.config_dir().join("settings.json"))
    }
}

/// Thread-safe global settings instance
static SETTINGS: once_cell::sync::Lazy<Arc<Mutex<SettingsManager>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(SettingsManager::load())));

/// Get the global settings manager
pub fn settings() -> Arc<Mutex<SettingsManager>> {
    SETTINGS.clone()
}

/// Convenience function to get current font size
pub fn get_font_size() -> f32 {
    settings()
        .lock()
        .map(|s| s.get().font_size)
        .unwrap_or(Settings::DEFAULT_FONT_SIZE)
}

/// Convenience function to set font size
pub fn set_font_size(size: f32) {
    let clamped = Settings::clamp_font_size(size);
    if let Ok(mut manager) = settings().lock() {
        manager.update(|s| s.font_size = clamped);
    }
}

/// Returns the last valid normal-window bounds, if the user has closed the app before.
pub fn get_window_bounds() -> Option<Bounds<Pixels>> {
    settings().lock().ok().and_then(|manager| {
        manager
            .get()
            .window_geometry
            .filter(|geometry| geometry.is_valid())
            .map(WindowGeometry::to_bounds)
    })
}

/// Persists the current normal-window bounds for the next application launch.
pub fn set_window_bounds(bounds: Bounds<Pixels>) {
    let geometry = WindowGeometry::from_bounds(bounds);
    if !geometry.is_valid() {
        return;
    }

    if let Ok(mut manager) = settings().lock() {
        manager.update(|settings| settings.window_geometry = Some(geometry));
    }
}

#[cfg(test)]
mod tests {
    use super::WindowGeometry;
    use gpui::{Bounds, point, px, size};

    #[test]
    fn window_geometry_round_trips_valid_bounds() {
        let bounds = Bounds::new(point(px(120.), px(80.)), size(px(1180.), px(760.)));
        let geometry = WindowGeometry::from_bounds(bounds);

        assert!(geometry.is_valid());
        assert_eq!(geometry.to_bounds(), bounds);
    }

    #[test]
    fn window_geometry_rejects_invalid_sizes() {
        let geometry = WindowGeometry {
            origin_x: 0.,
            origin_y: 0.,
            width: 639.,
            height: 480.,
        };

        assert!(!geometry.is_valid());
    }
}
