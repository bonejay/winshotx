//! Platform-specific helpers: Explorer, clipboard, desktop icons, and so on.
//!
//! The explicit `return`s inside `cfg` blocks are intentional, so that adding a line after one
//! platform's block cannot silently change the return value.
#![allow(clippy::needless_return)]

pub(crate) mod autostart;
pub(crate) mod marco;
pub(crate) mod clipboard;
pub(crate) mod printscreen;
pub(crate) mod sonido;
pub(crate) mod webview;
pub(crate) mod window_style;

use std::path::Path;

/// Open a folder in Explorer, creating it first if needed.
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

