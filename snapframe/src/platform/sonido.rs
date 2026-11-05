//! Shutter click played after a capture when enabled.
//!
//! The WAV is embedded with `include_bytes!` and played with `PlaySoundW`, which only accepts
//! PCM WAV; the test checks the header.

/// The embedded shutter sound.
const CLIC: &[u8] = include_bytes!("../../assets/obturador.wav");

/// Play the shutter sound without waiting. Errors are ignored: a missing click must never fail
/// a capture.
pub fn obturador() {
    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

        // SND_MEMORY: the first argument points at the WAV bytes, not a path. The buffer is
        // `'static`.
        unsafe {
            let _ = PlaySoundW(
                PCWSTR(CLIC.as_ptr() as *const u16),
                None,
                // NODEFAULT: stay silent instead of playing the system beep if the WAV cannot
                // play.
                SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
            );
        }
    }
}
