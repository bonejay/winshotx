# English edition validation

The fork uses a native Windows tray menu, English-only controls, screenshots, and on-demand video recording. GIF, background recording, replay, and the entire editor UI/backend have been removed. Recordings save directly as MP4.

## Verified locally

- 133 frontend tests passed, including audio endpoint selection and signal meters, CRF slider/field synchronization, manual encoder tests and result invalidation, Shift+Print Screen keyup handling, independent default selection modes, and removal of editor actions.
- Production TypeScript/Vite build passed. Rust `cargo check --tests` passed.
- Native unit tests: 133 passed, 27 hardware/manual tests ignored by default.
- Real direct-save integration test passed for native H.264, copying a streamed recording, and FFmpeg H.264 with AAC audio. Invalid output settings fail while retaining the temporary source.
- Real FFmpeg tests passed using `C:\Program Files (x86)\SVP 4\utils\ffmpeg.exe` (FFmpeg 7.1.1): H.264/AAC at 120 fps with CRF 20, paths containing spaces, the new 1280 × 720 manual encoder check, and an invalid-preset failure. Native Media Foundation also encoded 120 fps at 320 × 180.
- Audio setup enumerated three local endpoints, opened the default playback device and microphone, stopped after frontend polling ceased, and rejected a nonexistent endpoint without fallback.
- Three capture-pacing tests cover 120 fps from 144 Hz and 240 Hz source schedules and delivery jitter. The raw-frame queue holds at most two frames. The recording bar counts processed capture frames, independently of cache deduplication.
- Settings use 16 px labels, 15 px controls, and 14 px explanatory text. Native select popups have explicit dark/light background, text, and disabled-option colors.
- Browser visual checks use mocked Tauri responses at 1100 × 840. Screenshots show implemented layout, not hardware capture or encoder performance.
- Prior native startup checks confirmed both full-screen capture overlays hidden while idle, with only the settings window visible.

## Behavior and limits

Encoder tests are manual and generate a half-second 720p clip with a tone; they do not access capture/audio devices. They use the selected settings and time out after 20 seconds. Changes invalidate the visible result; an older asynchronous result cannot validate newer settings.

120 fps remains a target. Sustained full-resolution capture and vendor-specific GPU encoding have not been benchmarked. FFmpeg saving uses a native H.264 intermediate; its final quality is set by CRF/CQ/QP or target bitrate, not the hidden built-in percentage setting.

Each recording keeps its encoding/save preferences from startup. Cache clearing is blocked during saving. Save errors show the reason and retained source directory; failed sessions are excluded from automatic stale-cache deletion.

The executable is unsigned. Upstream signed automatic updates are disabled; this fork has its own application identifier and temporary storage.

Final release restart (2026-11-04): PID 18992 remained responsive with WinShotX Settings visible. Win32 enumeration confirmed both 1728 × 3072 and 3072 × 1728 capture overlays invisible while idle.
