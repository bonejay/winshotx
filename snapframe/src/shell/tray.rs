//! The notification-area icon and its menu.

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Capture,
    Record,
    ScreenshotFolder,
    VideoFolder,
    Settings,
    Quit,
}

const ITEMS: [(&str, &str, TrayAction); 6] = [
    ("capture", "Take a screenshot", TrayAction::Capture),
    ("record", "Start / stop recording", TrayAction::Record),
    (
        "folder",
        "Open screenshot folder",
        TrayAction::ScreenshotFolder,
    ),
    ("video-folder", "Open video folder", TrayAction::VideoFolder),
    ("settings", "Settings", TrayAction::Settings),
    ("quit", "Quit Snapframe", TrayAction::Quit),
];

/// Build the tray icon. Must run on the UI thread. Left click opens the settings.
pub fn build(on_action: impl Fn(TrayAction) + Send + Sync + 'static) -> Option<TrayIcon> {
    let item = |i: usize| MenuItem::with_id(ITEMS[i].0, ITEMS[i].1, true, None);
    let menu = Menu::new();
    let _ = menu.append_items(&[
        &item(0),
        &item(1),
        &PredefinedMenuItem::separator(),
        &item(2),
        &item(3),
        &item(4),
        &PredefinedMenuItem::separator(),
        &item(5),
    ]);
    let on_action = std::sync::Arc::new(on_action);
    let for_menu = on_action.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if let Some((_, _, action)) = ITEMS.iter().find(|(id, _, _)| *id == event.id().0) {
            for_menu(*action);
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            on_action(TrayAction::Settings);
        }
    }));
    let image = image::load_from_memory(include_bytes!("../../assets/icon.png"))
        .ok()?
        .into_rgba8();
    let (w, h) = image.dimensions();
    let icon = tray_icon::Icon::from_rgba(image.into_raw(), w, h).ok()?;
    TrayIconBuilder::new()
        .with_id("snapframe")
        .with_tooltip("Snapframe - Screenshots and video")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_icon(icon)
        .build()
        .map_err(|e| eprintln!("could not create the tray icon: {e}"))
        .ok()
}
