use crate::commands::{
    About, CloseTab, Copy, Cut, Find, FindNext, FindPrevious, FontSizeDecrease, FontSizeIncrease,
    FontSizeReset, NewFile, NextTab, OpenFile, OutlinePositionLeft, OutlinePositionRight, Paste,
    PreviousTab, Quit, Redo, SaveFile, SaveFileAs, SelectAll, ThemeAyuDark, ThemeAyuLight,
    ThemeAyuMirage, ToggleLineNumbers, ToggleLiveMarkdown, ToggleOutline, TogglePreview, Undo,
};
use crate::services::assets::AsterAssetSource;
use crate::services::fs::read_to_string;
use crate::services::settings;
use crate::ui::root::RootView;
use crate::ui::theme::{Theme as AsterTheme, ThemeName};
use camino::Utf8PathBuf;
use gpui::{
    AnyWindowHandle, App, AppContext, Application, Bounds, Entity, KeyBinding, Menu, MenuItem,
    OsAction, SystemMenuType, TitlebarOptions, Window, WindowBounds, WindowOptions, point, px,
    size,
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
                    MenuItem::action("Show or Hide Line Numbers", ToggleLineNumbers),
                    MenuItem::action("Toggle Markdown Source Mode", ToggleLiveMarkdown),
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
                            MenuItem::action(ThemeName::AyuLight.label(), ThemeAyuLight),
                            MenuItem::action(ThemeName::AyuDark.label(), ThemeAyuDark),
                            MenuItem::action(ThemeName::AyuMirage.label(), ThemeAyuMirage),
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
                let Some(root_view) = component_root_view::<RootView>(&window, cx) else {
                    continue;
                };

                let can_quit = window
                    .update(cx, |_, window, cx| {
                        root_view.update(cx, |root, cx| root.confirm_before_quit(window, cx))
                    })
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
                    if let Some(root_view) = component_root_view::<RootView>(&window, cx) {
                        let _ = window.update(cx, |_, window, cx| {
                            root_view.update(cx, |root, cx| {
                                root.action_close_tab(window, cx);
                            });
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

        let startup_paths = std::env::args()
            .skip(1)
            .filter_map(|arg| parse_open_target(&arg))
            .collect::<VecDeque<_>>();
        let has_startup_paths = !startup_paths.is_empty();
        if has_startup_paths {
            open_startup_paths(cx, startup_paths);
        }

        let queue = pending_urls.clone();
        cx.to_async()
            .spawn(async move |cx| {
                if !has_startup_paths {
                    // macOS delivers files opened from Finder through `on_open_urls` shortly
                    // after application activation. Keep the first window pending briefly so
                    // the requested document can become that window instead of following an
                    // already-created blank window.
                    cx.background_executor()
                        .timer(Duration::from_millis(200))
                        .await;
                    let paths = drain_pending_open_paths(&queue);
                    let _ = cx.update(|cx| open_startup_paths(cx, paths));
                }

                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(200))
                        .await;
                    let paths = drain_pending_open_paths(&queue);
                    if paths.is_empty() {
                        continue;
                    }

                    let _ = cx.update(|cx| {
                        for path in paths {
                            open_path_in_active_window_or_new(cx, path);
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

fn install_should_close_prompt(window: &mut Window, cx: &mut App, root: gpui::Entity<RootView>) {
    window.on_window_should_close(cx, {
        move |window, cx| root.update(cx, |root, cx| root.confirm_before_quit(window, cx))
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

/// Drains Finder/open-event URLs and converts supported file URLs into local paths.
fn drain_pending_open_paths(queue: &Arc<Mutex<VecDeque<String>>>) -> VecDeque<Utf8PathBuf> {
    let mut queue = queue.lock().expect("open url queue lock poisoned");
    queue
        .drain(..)
        .filter_map(|url| parse_open_target(&url))
        .collect()
}

/// Opens exactly one startup window, using the first requested path when available.
fn open_startup_paths(cx: &mut App, mut paths: VecDeque<Utf8PathBuf>) {
    let initial_path = take_initial_startup_path(&mut paths);
    let _ = open_window(cx, initial_path);
    for path in paths {
        open_path_in_active_window_or_new(cx, path);
    }
}

/// Removes the path that must populate the single initial application window.
fn take_initial_startup_path(paths: &mut VecDeque<Utf8PathBuf>) -> Option<Utf8PathBuf> {
    paths.pop_front()
}

/// Returns the application view wrapped by a gpui-component window root.
fn component_root_view<T: 'static>(window: &AnyWindowHandle, cx: &App) -> Option<Entity<T>> {
    let component_root = window.downcast::<ComponentRoot>()?;
    component_root
        .read(cx)
        .ok()?
        .view()
        .clone()
        .downcast::<T>()
        .ok()
}

fn open_path_in_active_window_or_new(cx: &mut App, path: Utf8PathBuf) {
    let candidate_windows = cx
        .active_window()
        .into_iter()
        .chain(cx.windows())
        .collect::<Vec<_>>();
    for candidate_window in candidate_windows {
        if let Some(root_view) = component_root_view::<RootView>(&candidate_window, cx) {
            let _ = candidate_window.update(cx, |_, window, cx| {
                root_view.update(cx, |root, cx| {
                    root.action_open_path(path.clone(), window, cx);
                });
            });
            return;
        }
    }

    let _ = open_window(cx, Some(path));
}

#[cfg(test)]
mod tests {
    use super::{
        component_root_view, drain_pending_open_paths, initial_preview_visible,
        take_initial_startup_path,
    };
    use camino::Utf8PathBuf;
    use gpui::{AnyWindowHandle, AppContext, IntoElement, Render, TestAppContext, Window, div};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    /// Minimal inner view used to exercise the real gpui-component window wrapper.
    struct WrappedTestView;

    impl Render for WrappedTestView {
        /// Renders an empty view used to verify component-root unwrapping.
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            div()
        }
    }

    #[test]
    fn blank_document_always_starts_in_editor_mode() {
        assert!(!initial_preview_visible(false, true));
    }

    #[test]
    fn existing_document_uses_remembered_preview_mode() {
        assert!(initial_preview_visible(true, true));
        assert!(!initial_preview_visible(true, false));
    }

    #[test]
    fn finder_open_event_becomes_the_initial_window_path() {
        let path = Utf8PathBuf::from("/tmp/finder-open.md");
        let queue = Arc::new(Mutex::new(VecDeque::from([url::Url::from_file_path(
            path.as_std_path(),
        )
        .unwrap()
        .to_string()])));
        let mut paths = drain_pending_open_paths(&queue);

        assert_eq!(take_initial_startup_path(&mut paths), Some(path));
        assert!(paths.is_empty());
        assert!(queue.lock().unwrap().is_empty());
    }

    #[gpui::test]
    fn component_root_exposes_its_wrapped_application_view(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(|window, cx| {
            let wrapped_view = cx.new(|_| WrappedTestView);
            gpui_component::Root::new(wrapped_view, window, cx)
        });
        let window: AnyWindowHandle = window.into();

        cx.read(|cx| {
            assert!(window.downcast::<WrappedTestView>().is_none());
            assert!(component_root_view::<WrappedTestView>(&window, cx).is_some());
        });
    }
}
