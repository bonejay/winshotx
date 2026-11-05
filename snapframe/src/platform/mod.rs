//! Platform-specific helpers: Explorer, clipboard, desktop icons, and so on.
//!
//! The explicit `return`s inside `cfg` blocks are intentional, so that adding a line after one
//! platform's block cannot silently change the return value.
#![allow(clippy::needless_return)]

pub(crate) mod autostart;
pub(crate) mod clipboard;
pub(crate) mod marco;
pub(crate) mod printscreen;
pub(crate) mod sonido;
pub(crate) mod window_style;

use std::path::Path;

/// Open a folder in Explorer, creating it first if needed.
/// Open a saved recording with the default video player, or select it in Explorer.
pub fn open_recording(path: &Path, reveal: bool) -> crate::error::Result<()> {
    let es_video = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("mp4"));
    if !es_video || !path.is_file() {
        return Err(crate::error::AppError::Msg(
            "The recording no longer exists.".into(),
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let wide = |s: &std::ffi::OsStr| s.encode_wide().chain([0]).collect::<Vec<u16>>();
        if reveal {
            let mut arg = std::ffi::OsString::from("/select,\"");
            arg.push(path.as_os_str());
            arg.push("\"");
            let (exe, arg) = (wide("explorer".as_ref()), wide(&arg));
            unsafe {
                ShellExecuteW(
                    None,
                    PCWSTR::null(),
                    PCWSTR(exe.as_ptr()),
                    PCWSTR(arg.as_ptr()),
                    PCWSTR::null(),
                    SW_SHOWNORMAL,
                );
            }
        } else {
            let (verb, file) = (wide("open".as_ref()), wide(path.as_os_str()));
            let r = unsafe {
                ShellExecuteW(
                    None,
                    PCWSTR(verb.as_ptr()),
                    PCWSTR(file.as_ptr()),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    SW_SHOWNORMAL,
                )
            };
            if r.0 as isize <= 32 {
                return Err(crate::error::AppError::Msg(
                    "No video player is associated with MP4 files.".into(),
                ));
            }
        }
        return Ok(());
    }
    #[cfg(not(windows))]
    Err(crate::error::AppError::Unsupported)
}

pub fn open_folder(path: &Path) -> crate::error::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer")
            .creation_flags(0x0800_0000)
            .arg(path)
            .spawn()?;
        return Ok(());
    }
    #[cfg(not(windows))]
    Err(crate::error::AppError::Unsupported)
}

pub(crate) mod child_job;
