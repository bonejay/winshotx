//! Check WebView2 before attempting to open any app windows.
#[cfg(windows)]
pub fn version() -> Option<String> {
    tauri::webview_version().ok()
}
#[cfg(not(windows))]
pub fn version() -> Option<String> {
    Some(String::new())
}

pub fn aviso() -> (String, String) {
    ("WinShotX needs WebView2".into(),
     "WebView2 is not installed on this machine. Download the free WebView2 Runtime from Microsoft, then start WinShotX again.".into())
}
#[cfg(windows)]
pub fn hay_con_que_pintar() -> bool {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    if version().is_some() {
        return true;
    }
    let (title, body) = aviso();
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(body),
            &HSTRING::from(title),
            MB_OK | MB_ICONERROR,
        );
    }
    false
}
#[cfg(not(windows))]
pub fn hay_con_que_pintar() -> bool {
    true
}
#[cfg(test)]
mod tests {
    #[test]
    fn missing_runtime_message_is_actionable_and_english() {
        let (title, body) = super::aviso();
        assert!(title.contains("WebView2"));
        assert!(body.contains("not installed"));
        assert!(body.contains("Microsoft"));
    }
}
