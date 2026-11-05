//! Global shortcuts. Stored strings use physical key codes (`CmdOrCtrl+Shift+KeyS`,
//! `Shift+PrintScreen`), which `global-hotkey` parses directly.

use std::sync::Arc;

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use parking_lot::Mutex;

use crate::settings::Settings;
use crate::state::ShortcutStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    Capture,
    Record,
}

pub struct Hotkeys {
    manager: GlobalHotKeyManager,
    registered: Vec<(HotKey, Shortcut)>,
    /// Hotkey ids the event handler (another thread) maps back to shortcuts.
    ids: Arc<Mutex<Vec<(u32, Shortcut)>>>,
}

impl Hotkeys {
    /// Must be created on the UI thread: Windows delivers hotkeys to its message loop.
    pub fn new(on_press: impl Fn(Shortcut) + Send + Sync + 'static) -> Option<Self> {
        let manager = GlobalHotKeyManager::new()
            .map_err(|e| eprintln!("global shortcuts are unavailable: {e}"))
            .ok()?;
        let ids: Arc<Mutex<Vec<(u32, Shortcut)>>> = Arc::default();
        let lookup = ids.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.state() != HotKeyState::Pressed {
                return;
            }
            let which = lookup
                .lock()
                .iter()
                .find(|(id, _)| *id == event.id())
                .map(|(_, s)| *s);
            if let Some(which) = which {
                on_press(which);
            }
        }));
        Some(Self {
            manager,
            registered: Vec::new(),
            ids,
        })
    }

    /// Unregister everything, e.g. while the settings page records a new shortcut so pressing
    /// the current one does not take a screenshot.
    pub fn suspend(&mut self) {
        for (hotkey, _) in self.registered.drain(..) {
            let _ = self.manager.unregister(hotkey);
        }
        self.ids.lock().clear();
    }

    /// (Re)register both shortcuts and report which ones Windows accepted. Always unregisters
    /// first, so calling it again retries combinations another app has since released.
    pub fn register(&mut self, settings: &Settings) -> ShortcutStatus {
        self.suspend();
        if [&settings.capture_shortcut, &settings.record_shortcut]
            .iter()
            .any(|s| s.contains("PrintScreen"))
        {
            crate::platform::printscreen::release_from_snipping_tool();
        }
        let mut status = ShortcutStatus::default();
        for (text, which) in [
            (&settings.capture_shortcut, Shortcut::Capture),
            (&settings.record_shortcut, Shortcut::Record),
        ] {
            let ok = match text.parse::<HotKey>() {
                Ok(hotkey) => match self.manager.register(hotkey) {
                    Ok(()) => {
                        self.registered.push((hotkey, which));
                        true
                    }
                    Err(e) => {
                        eprintln!("could not register {text}: {e}");
                        false
                    }
                },
                Err(e) => {
                    eprintln!("invalid shortcut {text}: {e}");
                    false
                }
            };
            match which {
                Shortcut::Capture => status.capture = ok,
                Shortcut::Record => status.record = ok,
            }
        }
        *self.ids.lock() = self.registered.iter().map(|(h, s)| (h.id(), *s)).collect();
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stored_shortcut_formats_parse() {
        for s in [
            "PrintScreen",
            "Shift+PrintScreen",
            "CmdOrCtrl+Shift+KeyS",
            "Ctrl+Shift+2",
            "Super+Alt+Digit5",
            "Shift+F9",
        ] {
            assert!(s.parse::<HotKey>().is_ok(), "{s}");
        }
    }
}
