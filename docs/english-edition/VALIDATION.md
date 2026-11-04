# English edition validation

The fork uses a native Windows tray menu, English-only controls, screenshots, and on-demand video recording. GIF, background recording, replay, and the entire editor UI/backend have been removed. Recordings are encoded live by FFmpeg into a fragmented MP4 in the save folder.

## Verified locally

- Tests inherited from the original repository were removed; only tests for this fork remain.
- 16 frontend tests passed (settings, audio sources, shortcut field). TypeScript type check passed.
- Rust: 14 unit tests passed locally, 12 hardware/FFmpeg tests ignored by default. `cargo test --lib` runs `record::stream_encoder::tests` (frame clock without drift, audio gap/overlap/bound handling, legacy-setting migration, fragmented-MP4 muxing enforced after custom arguments), plus the audio-device, capture-pacing, and settings tests.
- Hardware/FFmpeg tests for the live pipeline are ignored by default; run them with `cargo test --lib -- --ignored live_`. They cover file growth during recording and rename on stop, playable fragments after killing FFmpeg, playable video+AAC after killing the recording process, discard deleting only the owned partial file, audio EOF before short video, picture/tone sync under 50 ms, pause exclusion with a static source, and 720p with real WASAPI audio.
- Audio setup enumerated three local endpoints, opened the default playback device and microphone, stopped after frontend polling ceased, and rejected a nonexistent endpoint without fallback.
- Three capture-pacing tests cover 120 fps from 144 Hz and 240 Hz source schedules and delivery jitter. The raw-frame queue holds at most two frames. The recording bar counts delivered capture frames and shows the size of the file being written.
- Settings use 16 px labels, 15 px controls, and 14 px explanatory text. Native select popups have explicit dark/light background, text, and disabled-option colors.
- Browser visual checks use mocked Tauri responses at 1100 × 840. Screenshots show implemented layout, not hardware capture or encoder performance.
- Prior native startup checks confirmed both full-screen capture overlays hidden while idle, with only the settings window visible.

## Behavior and limits

Encoder tests are manual and generate a half-second 720p clip with a tone; they do not access capture/audio devices. They use the selected settings and time out after 20 seconds. Changes invalidate the visible result; an older asynchronous result cannot validate newer settings.

120 fps remains a target. Sustained full-resolution capture and vendor-specific GPU encoding have not been benchmarked. Live encoding must keep up in real time: falling more than two seconds behind stops the recording with an error and keeps the fragments already written.

Each recording reads encoding and save preferences when it starts. Errors show FFmpeg's last output and the path of the retained `.recording.mp4` file. Audio arriving more than two seconds late fails the recording instead of drifting.

The executable is unsigned. Upstream signed automatic updates are disabled; this fork has its own application identifier and temporary storage.

Final release restart (2026-11-04): PID 18992 remained responsive with WinShotX Settings visible. Win32 enumeration confirmed both 1728 × 3072 and 3072 × 1728 capture overlays invisible while idle.
