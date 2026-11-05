//! Records a new global shortcut from the physical keys pressed, through a low-level keyboard
//! hook. Using virtual-key codes avoids layout problems (Shift+2 is "\"" on a German layout but
//! the stored code must be `Digit2`) and also sees Print Screen and Win-key combinations, which
//! never reach a window.

/// What one key press means while recording a shortcut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A modifier on its own, or a key without modifiers: keep listening.
    Ignore,
    Cancel,
    Done(String),
}

/// Physical key code name for a virtual-key code, in the stored shortcut format.
pub fn key_code(vk: u32) -> Option<String> {
    let name = match vk {
        0x30..=0x39 => format!("Digit{}", vk - 0x30),
        0x41..=0x5A => format!("Key{}", char::from_u32(vk)?),
        0x60..=0x69 => format!("Numpad{}", vk - 0x60),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        _ => {
            return Some(
                match vk {
                    0x2C => "PrintScreen",
                    0x20 => "Space",
                    0x0D => "Enter",
                    0x09 => "Tab",
                    0x08 => "Backspace",
                    0x2D => "Insert",
                    0x2E => "Delete",
                    0x24 => "Home",
                    0x23 => "End",
                    0x21 => "PageUp",
                    0x22 => "PageDown",
                    0x25 => "ArrowLeft",
                    0x26 => "ArrowUp",
                    0x27 => "ArrowRight",
                    0x28 => "ArrowDown",
                    0x13 => "Pause",
                    0x91 => "ScrollLock",
                    0x6A => "NumpadMultiply",
                    0x6B => "NumpadAdd",
                    0x6D => "NumpadSubtract",
                    0x6E => "NumpadDecimal",
                    0x6F => "NumpadDivide",
                    0xBA => "Semicolon",
                    0xBB => "Equal",
                    0xBC => "Comma",
                    0xBD => "Minus",
                    0xBE => "Period",
                    0xBF => "Slash",
                    0xC0 => "Backquote",
                    0xDB => "BracketLeft",
                    0xDC => "Backslash",
                    0xDD => "BracketRight",
                    0xDE => "Quote",
                    _ => return None,
                }
                .to_string(),
            )
        }
    };
    Some(name)
}

fn is_modifier(vk: u32) -> bool {
    matches!(vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

/// Decide what a key press means. A global shortcut without modifiers would take over the key
/// system-wide; Print Screen alone is the one exception, since taking it over is the point.
pub fn classify(vk: u32, ctrl: bool, shift: bool, alt: bool, win: bool) -> Outcome {
    if vk == 0x1B {
        return Outcome::Cancel;
    }
    if is_modifier(vk) {
        return Outcome::Ignore;
    }
    let Some(code) = key_code(vk) else {
        return Outcome::Ignore;
    };
    let mut parts = Vec::new();
    // The Windows key is Super, not Ctrl: mixing them saved Win+X as Ctrl+X.
    if ctrl {
        parts.push("CmdOrCtrl");
    }
    if win {
        parts.push("Super");
    }
    if shift {
        parts.push("Shift");
    }
    if alt {
        parts.push("Alt");
    }
    if parts.is_empty() && code != "PrintScreen" {
        return Outcome::Ignore;
    }
    parts.push(&code);
    Outcome::Done(parts.join("+"))
}

#[cfg(windows)]
mod imp {
    use super::{classify, Outcome};
    use std::cell::RefCell;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT,
        WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN,
    };

    type Done = Box<dyn FnOnce(Option<String>)>;

    thread_local! {
        static ACTIVE: RefCell<Option<(HHOOK, Done)>> = const { RefCell::new(None) };
    }

    fn down(vk: i32) -> bool {
        (unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000) != 0
    }

    unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 {
            let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            let message = wparam.0 as u32;
            // Windows only sends key-up for Print Screen in some configurations.
            let pressed = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
            let print_up = message == WM_KEYUP && info.vkCode == 0x2C;
            if pressed || print_up {
                let outcome = classify(
                    info.vkCode,
                    down(0x11),
                    down(0x10),
                    down(0x12),
                    down(0x5B) || down(0x5C),
                );
                match outcome {
                    Outcome::Ignore => {
                        // Swallow non-modifier keys so they do not type into anything.
                        if !super::is_modifier(info.vkCode) {
                            return LRESULT(1);
                        }
                    }
                    Outcome::Cancel => {
                        finish(None);
                        return LRESULT(1);
                    }
                    Outcome::Done(text) => {
                        finish(Some(text));
                        return LRESULT(1);
                    }
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    fn finish(result: Option<String>) {
        let active = ACTIVE.with(|a| a.borrow_mut().take());
        if let Some((handle, done)) = active {
            unsafe {
                let _ = UnhookWindowsHookEx(handle);
            }
            // Run after the hook returns, outside the hook callback.
            let done = std::cell::Cell::new(Some(done));
            let _ = slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                if let Some(done) = done.take() {
                    done(result);
                }
            });
        }
    }

    /// Start listening; `done` gets the new shortcut, or `None` when cancelled. Must run on the
    /// UI thread, whose message loop drives the hook.
    pub fn start(done: impl FnOnce(Option<String>) + 'static) -> bool {
        stop();
        let module = unsafe { GetModuleHandleW(None) }.ok();
        let handle =
            unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), module.map(Into::into), 0) };
        match handle {
            Ok(h) => {
                ACTIVE.with(|a| *a.borrow_mut() = Some((h, Box::new(done))));
                true
            }
            Err(e) => {
                eprintln!("could not record a shortcut: {e}");
                false
            }
        }
    }

    /// Stop listening without a result (e.g. the field lost focus).
    pub fn stop() {
        if let Some((handle, _)) = ACTIVE.with(|a| a.borrow_mut().take()) {
            unsafe {
                let _ = UnhookWindowsHookEx(handle);
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn start(_: impl FnOnce(Option<String>) + 'static) -> bool {
        false
    }
    pub fn stop() {}
}

pub use imp::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_independent_codes() {
        assert_eq!(key_code(0x32).as_deref(), Some("Digit2"));
        assert_eq!(key_code(0x53).as_deref(), Some("KeyS"));
        assert_eq!(key_code(0x70).as_deref(), Some("F1"));
        assert_eq!(key_code(0x87).as_deref(), Some("F24"));
        assert_eq!(key_code(0x2C).as_deref(), Some("PrintScreen"));
        assert_eq!(key_code(0x63).as_deref(), Some("Numpad3"));
        assert_eq!(key_code(0xFF), None);
    }

    #[test]
    fn shortcuts_need_a_modifier_except_print_screen() {
        assert_eq!(
            classify(0x53, true, true, false, false),
            Outcome::Done("CmdOrCtrl+Shift+KeyS".into())
        );
        assert_eq!(
            classify(0x58, false, false, false, true),
            Outcome::Done("Super+KeyX".into()),
            "Win is Super, not Ctrl"
        );
        assert_eq!(classify(0x53, false, false, false, false), Outcome::Ignore);
        assert_eq!(
            classify(0x2C, false, false, false, false),
            Outcome::Done("PrintScreen".into())
        );
        assert_eq!(
            classify(0x2C, false, true, false, false),
            Outcome::Done("Shift+PrintScreen".into())
        );
        assert_eq!(
            classify(0x10, false, true, false, false),
            Outcome::Ignore,
            "modifier alone"
        );
        assert_eq!(classify(0x1B, true, false, false, false), Outcome::Cancel);
    }

    #[test]
    fn recorded_shortcuts_register() {
        for vk in [0x32, 0x53, 0x70, 0x2C] {
            if let Outcome::Done(s) = classify(vk, true, false, true, false) {
                assert!(s.parse::<global_hotkey::hotkey::HotKey>().is_ok(), "{s}");
            }
        }
    }
}
