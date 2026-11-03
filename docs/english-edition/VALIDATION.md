# English edition validation

The fork replaces the custom tray WebView with a native Windows menu, provides English-only capture controls, and removes the background recorder and replay buffer.

## Checks

- Frontend: 258 tests passed.
- TypeScript and production Vite build passed.
- Rust: `cargo check --tests` passed, including the integration-test targets.
- FFmpeg argument tests cover software quality, audio codec, NVIDIA encoder/device selection, bitrate, custom arguments containing spaces, and invalid settings.
- Settings tests cover legacy language migration, rejection of saved replay enablement, and encoding preference serialization.
- Missing-WebView2 messaging is tested in English.
- The optimized Windows executable is built through the Tauri CLI, embedding the production frontend.
- Browser visual checks use mocked Tauri responses at 1100 × 840. The saved screenshots demonstrate the implemented UI; they do not verify desktop capture or GPU encoding.
- Native startup was verified after removing the obsolete updater initialization: the process stays running and the settings window is visible.
- Win32 window enumeration on the actual two-monitor desktop confirms both full-screen capture overlays are hidden while idle. Capture cancellation now calls `hide()` rather than leaving visible windows parked outside the desktop.
- All 15 window-positioning tests pass, including the wider recording toolbar on narrow and negatively positioned monitors.
- Settings use 17 px labels, 16 px controls, and 15 px supporting text. Other capture and editor text was enlarged as well; marketing labels and slogans were removed.

## Limits

FFmpeg is not installed on the build machine. Real FFmpeg exports and vendor-specific GPU encoders need testing on a machine with a suitable FFmpeg build, GPU, and driver. The native recorder still uses Windows Media Foundation for live H.264 capture/previews; FFmpeg encoding runs at export time.

The removed custom tray renderer cannot produce its old blank white window. Native tray rendering and recording should still be exercised interactively on the target Windows setup, including multi-monitor and DPI behavior.

A live smoke test using the release executable and a localhost WebView debugging endpoint was blocked by automatic approval review with the reason "blocked by policy". It did not run; no real native recording/export result is claimed.

The executable is unsigned. Upstream signed automatic updates are disabled; this fork has its own application identifier and temporary storage.
