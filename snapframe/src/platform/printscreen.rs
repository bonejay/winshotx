//! Taking the Print Screen key from the Snipping Tool.
//!
//! Windows 11 gives Print Screen to the Snipping Tool by default
//! (`HKCU\Control Panel\Keyboard\PrintScreenKeyForSnippingEnabled`). While that is on, our
//! global shortcut registers fine but never receives the key, so the value is set to 0
//! whenever one of our shortcuts uses Print Screen.

#[cfg(windows)]
pub fn release_from_snipping_tool() {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD};

    let zero: u32 = 0;
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            w!(r"Control Panel\Keyboard"),
            w!("PrintScreenKeyForSnippingEnabled"),
            REG_DWORD.0,
            Some(std::ptr::from_ref(&zero).cast()),
            4,
        )
    };
    if status.is_err() {
        eprintln!("could not take Print Screen from the Snipping Tool: {status:?}");
    }
}

#[cfg(not(windows))]
pub fn release_from_snipping_tool() {}
