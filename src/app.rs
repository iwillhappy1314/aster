use crate::commands::{
    About, CloseTab, Copy, Cut, Find, FindNext, FindPrevious, FontSizeDecrease, FontSizeIncrease,
    FontSizeReset, NewFile, NextTab, OpenFile, OutlinePositionLeft, OutlinePositionRight, Paste,
    PreviousTab, Quit, Redo, SaveFile, SaveFileAs, SelectAll, ThemeAyuDark, ThemeAyuLight,
    ThemeAyuMirage, ToggleLiveMarkdown, ToggleOutline, TogglePreview, Undo,
};
use crate::services::assets::AsterAssetSource;
use crate::services::fs::read_to_string;
use crate::services::settings;
use crate::ui::root::RootView;
use crate::ui::theme::{Theme as AsterTheme, ThemeName};
use camino::Utf8PathBuf;
use gpui::{
    App, AppContext, Application, Bounds, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType,
    TitlebarOptions, Window, WindowBounds, WindowOptions, point, px, size,
};
use gpui_component::Root as ComponentRoot;
use gpui_component::notification::NotificationList;
use gpui_component::theme::{Theme as ComponentTheme, ThemeMode};
use rfd::{MessageButtons, MessageDialog, MessageLevel};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use url::Url;

pub fn run() {
    let pending_urls: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
    let pending_urls_for_callback = pending_urls.clone();

    let app = Application::new().with_assets(AsterAssetSource::new());
    app.on_open_urls(move |urls| {
        let mut queue = pending_urls_for_callback
            .lock()
            .expect("open url queue lock poisoned");
        queue.extend(urls);
    });

    app.run(move |cx: &mut App| {
        gpui_component::init(cx);
        sync_component_text_color(cx);

        cx.activate(true);

        cx.bind_keys([
            KeyBinding::new("cmd-n", NewFile, None),
            KeyBinding::new("cmd-o", OpenFile, None),
            KeyBinding::new("ctrl-tab", NextTab, None),
            KeyBinding::new("shift-ctrl-tab", PreviousTab, None),
            KeyBinding::new("cmd-s", SaveFile, None),
            KeyBinding::new("shift-cmd-s", SaveFileAs, None),
            KeyBinding::new("cmd-w", CloseTab, None),
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-z", Undo, None),
            KeyBinding::new("shift-cmd-z", Redo, None),
            KeyBinding::new("cmd-y", Redo, None),
            KeyBinding::new("cmd-x", Cut, None),
            KeyBinding::new("cmd-c", Copy, None),
            KeyBinding::new("cmd-v", Paste, None),
            KeyBinding::new("cmd-a", SelectAll, None),
            KeyBinding::new("cmd-f", Find, None),
            KeyBinding::new("cmd-g", FindNext, None),
            KeyBinding::new("shift-cmd-g", FindPrevious, None),
            KeyBinding::new("cmd-=", FontSizeIncrease, None),
            KeyBinding::new("cmd--", FontSizeDecrease, None),
            KeyBinding::new("cmd-0", FontSizeReset, None),
            KeyBinding::new("shift-cmd-o", ToggleOutline, None),
            KeyBinding::new("shift-cmd-m", ToggleLiveMarkdown, None),
            KeyBinding::new("shift-cmd-p", TogglePreview, None),
        ]);

        cx.set_menus(vec![
            Menu {
                name: "Aster".into(),
                items: vec![
                    MenuItem::action("About Aster", About),
                    MenuItem::separator(),
                    MenuItem::os_submenu("Services", SystemMenuType::Services),
                    MenuItem::separator(),
                    MenuItem::action("Quit Aster", Quit),
                ],
            },
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("New", NewFile),
                    MenuItem::action("Open File or Folder…", OpenFile),
                    MenuItem::separator(),
                    MenuItem::action("Save", SaveFile),
                    MenuItem::action("Save As…", SaveFileAs),
                    MenuItem::separator(),
                    MenuItem::action("Close Tab", CloseTab),
                ],
            },
            Menu {
                name: "Edit".into(),
                items: vec![
                    MenuItem::os_action("Undo", Undo, OsAction::Undo),
                    MenuItem::os_action("Redo", Redo, OsAction::Redo),
                    MenuItem::separator(),
                    MenuItem::os_action("Cut", Cut, OsAction::Cut),
                    MenuItem::os_action("Copy", Copy, OsAction::Copy),
                    MenuItem::os_action("Paste", Paste, OsAction::Paste),
                    MenuItem::separator(),
                    MenuItem::action("Find…", Find),
                    MenuItem::action("Find Next", FindNext),
                    MenuItem::action("Find Previous", FindPrevious),
                    MenuItem::separator(),
                    MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Show or Hide Outline", ToggleOutline),
                    MenuItem::action("Toggle Live Markdown", ToggleLiveMarkdown),
                    MenuItem::submenu(Menu {
                        name: "Outline Position".into(),
                        items: vec![
                            MenuItem::action("Left", OutlinePositionLeft),
                            MenuItem::action("Right", OutlinePositionRight),
                        ],
                    }),
                    MenuItem::action("Toggle Preview", TogglePreview),
                    MenuItem::separator(),
                    MenuItem::action("Next Tab", NextTab),
                    MenuItem::action("Previous Tab", PreviousTab),
                    MenuItem::separator(),
                    MenuItem::submenu(Menu {
                        name: "Theme".into(),
                        items: vec![
                            MenuItem::action("Ayu Light", ThemeAyuLight),
                            MenuItem::action("Ayu Dark", ThemeAyuDark),
                            MenuItem::action("Ayu Mirage", ThemeAyuMirage),
                        ],
                    }),
                    MenuItem::separator(),
                    MenuItem::action("Increase Font Size", FontSizeIncrease),
                    MenuItem::action("Decrease Font Size", FontSizeDecrease),
                    MenuItem::action("Reset Font Size", FontSizeReset),
                ],
            },
        ]);

        cx.on_action(|_: &Quit, cx| {
            let windows = cx.window_stack().unwrap_or_else(|| cx.windows());

            for window in windows.iter().copied() {
                let Some(handle) = window.downcast::<RootView>() else {
                    continue;
                };

                let can_quit = handle
                    .update(cx, |root, window, cx| root.confirm_before_quit(window, cx))
                    .unwrap_or(true);
                if !can_quit {
                    return;
                }
            }

            // Close windows ourselves (bypasses `on_window_should_close`) and then quit.
            for window in windows {
                let _ = window.update(cx, |_, window, _| window.remove_window());
            }

            cx.quit();
        });
        cx.on_action(|_: &CloseTab, cx| {
            let target_window = cx.active_window().or_else(|| {
                cx.window_stack()
                    .and_then(|windows| windows.first().copied())
            });

            if let Some(window) = target_window {
                cx.defer(move |cx| {
                    if let Some(handle) = window.downcast::<RootView>() {
                        let _ = handle.update(cx, |root, window, cx| {
                            root.action_close_tab(window, cx);
                        });
                    }
                });
            }
        });
        cx.on_action(|_: &ThemeAyuLight, cx| apply_theme(ThemeName::AyuLight, cx));
        cx.on_action(|_: &ThemeAyuDark, cx| apply_theme(ThemeName::AyuDark, cx));
        cx.on_action(|_: &ThemeAyuMirage, cx| apply_theme(ThemeName::AyuMirage, cx));
        cx.on_action(|_: &OutlinePositionLeft, cx| {
            apply_outline_position(settings::OutlinePosition::Left, cx)
        });
        cx.on_action(|_: &OutlinePositionRight, cx| {
            apply_outline_position(settings::OutlinePosition::Right, cx)
        });
        cx.on_action(|_: &About, _cx| {
            MessageDialog::new()
                .set_level(MessageLevel::Info)
                .set_title("About Aster")
                .set_description(format!(
                    "Aster\n\nVersion {}\n\nA lightweight Markdown editor built with Rust + GPUI.",
                    env!("CARGO_PKG_VERSION")
                ))
                .set_buttons(MessageButtons::Ok)
                .show();
        });

        let mut startup_paths = std::env::args()
            .skip(1)
            .filter_map(|arg| parse_open_target(&arg));
        let initial_path = startup_paths.next();
        let _ = open_window(cx, initial_path);

        for path in startup_paths {
            open_path_in_active_window_or_new(cx, path);
        }

        let queue = pending_urls.clone();
        cx.to_async()
            .spawn(async move |cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(200))
                        .await;
                    let urls: Vec<String> = {
                        let mut q = queue.lock().expect("open url queue lock poisoned");
                        let mut drained = Vec::new();
                        while let Some(u) = q.pop_front() {
                            drained.push(u);
                        }
                        drained
                    };

                    if urls.is_empty() {
                        continue;
                    }

                    let _ = cx.update(|cx| {
                        for url in urls {
                            if let Some(path) = parse_open_target(&url) {
                                open_path_in_active_window_or_new(cx, path);
                            }
                        }
                    });
                }
            })
            .detach();
    });
}

/// Switches the active theme, syncs the Markdown preview's light/dark mode, and
/// repaints every open window with the new palette.
fn apply_theme(name: ThemeName, cx: &mut App) {
    let is_dark = crate::ui::theme::set_theme(name);
    let mode = if is_dark {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    ComponentTheme::change(mode, None, cx);
    sync_component_text_color(cx);
    for window in cx.windows() {
        let _ = window.update(cx, |_, window, _| window.refresh());
    }
}

/// Keeps native component text aligned with Aster's current editor foreground.
fn sync_component_text_color(cx: &mut App) {
    ComponentTheme::global_mut(cx).foreground = AsterTheme::text().into();
}

/// Persists the Outline side and repaints all open windows immediately.
fn apply_outline_position(position: settings::OutlinePosition, cx: &mut App) {
    settings::set_outline_position(position);
    for window in cx.windows() {
        let _ = window.update(cx, |_, window, _| window.refresh());
    }
}

fn open_window(cx: &mut App, initial_path: Option<Utf8PathBuf>) -> anyhow::Result<()> {
    let window_bounds = settings::get_window_bounds().unwrap_or_else(|| default_window_bounds(cx));

    cx.open_window(
        WindowOptions {
            // A regular window opens at its final bounds without macOS's zoom animation.
            window_bounds: Some(WindowBounds::Windowed(window_bounds)),
            window_min_size: Some(size(px(640.), px(480.))),
            // Interactive custom chrome handles its own native drag and double-click gestures.
            is_movable: !cfg!(target_os = "macos"),
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                // Center the 14px traffic-light buttons within Aster's 38px custom title bar.
                traffic_light_position: Some(point(px(12.), px(12.))),
            }),
            ..Default::default()
        },
        |window, cx| build_root_view(window, cx, initial_path.clone()),
    )?;
    Ok(())
}

/// Returns a centered first-launch window that leaves visible desktop space around it.
fn default_window_bounds(cx: &App) -> Bounds<gpui::Pixels> {
    let display_bounds = Bounds::maximized(None, cx);
    let display_width: f32 = display_bounds.size.width.into();
    let display_height: f32 = display_bounds.size.height.into();
    let width = (display_width - 80.0).clamp(640.0, 1180.0);
    let height = (display_height - 80.0).clamp(480.0, 760.0);

    Bounds::centered(None, size(px(width), px(height)), cx)
}

fn build_root_view(
    window: &mut Window,
    cx: &mut App,
    initial_path: Option<Utf8PathBuf>,
) -> gpui::Entity<ComponentRoot> {
    let has_initial_file = initial_path.as_ref().is_some_and(|path| path.is_file());
    let preview_visible =
        initial_preview_visible(has_initial_file, settings::get_preview_visible());
    let document = cx.new(|_| RootView::new_document());
    let inline_markdown = cx.new(|_| RootView::new_inline_markdown());
    let notifications = cx.new(|cx| NotificationList::new(window, cx));
    let editor_view = cx.new(|_| RootView::build_editor(document.clone(), inline_markdown.clone()));
    let file_explorer_view = cx.new(|_| RootView::build_file_explorer(document.clone()));

    if let Some(path) = initial_path.as_ref().filter(|path| path.is_file()) {
        if let Ok(text) = read_to_string(path) {
            let _ = document.update(cx, |d, cx| {
                d.path = Some(path.clone());
                d.set_text(&text);
                d.save_snapshot();
                cx.notify();
            });
        }
    }

    let root_view = cx.new(|_| {
        RootView::new(
            document,
            inline_markdown,
            editor_view,
            file_explorer_view,
            notifications,
            preview_visible,
        )
    });
    install_should_close_prompt(window, cx, root_view.clone());
    if let Some(directory) = initial_path.filter(|path| path.is_dir()) {
        let _ = root_view.update(cx, |root, cx| {
            root.action_open_path(directory, window, cx);
        });
    }
    cx.new(|cx| ComponentRoot::new(root_view, window, cx))
}

/// Resolves the initial mode: blank documents always edit, existing documents use the preference.
fn initial_preview_visible(has_initial_path: bool, remembered_preview_visible: bool) -> bool {
    has_initial_path && remembered_preview_visible
}

fn install_should_close_prompt(
    window: &mut Window,
    cx: &mut App,
    root: gpui::Entity<RootView>,
) {
    window.on_window_should_close(cx, {
        move |window, cx| {
            root.update(cx, |root, cx| root.confirm_before_quit(window, cx))
        }
    });
}

fn parse_open_target(raw: &str) -> Option<Utf8PathBuf> {
    if let Ok(url) = Url::parse(raw) {
        if url.scheme() == "file" {
            if let Ok(path) = url.to_file_path() {
                return Utf8PathBuf::try_from(path).ok();
            }
        } else {
            return None;
        }
    }

    Utf8PathBuf::try_from(std::path::PathBuf::from(raw)).ok()
}

fn open_path_in_active_window_or_new(cx: &mut App, path: Utf8PathBuf) {
    if let Some(active_window) = cx.active_window() {
        if let Some(handle) = active_window.downcast::<RootView>() {
            let _ = handle.update(cx, |root, window, cx| {
                root.action_open_path(path.clone(), window, cx);
            });
            return;
        }
    }

    let _ = open_window(cx, Some(path));
}

#[cfg(test)]
mod tests {
    use super::initial_preview_visible;

    #[test]
    fn blank_document_always_starts_in_editor_mode() {
        assert!(!initial_preview_visible(false, true));
    }

    #[test]
    fn existing_document_uses_remembered_preview_mode() {
        assert!(initial_preview_visible(true, true));
        assert!(!initial_preview_visible(true, false));
    }
}
