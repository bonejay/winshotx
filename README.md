# Snapframe

Native Windows screenshots and screen recording, written in Rust. Settings and popups use
Slint's software renderer; the selection overlay uses Win32 and CPU drawing. Original
authorship and the [MIT license](LICENSE) from [Mun1to/winshotx](https://github.com/Mun1to/winshotx)
are preserved.

## Build and run

Requires Windows 10/11, Rust 1.92 or newer, Visual Studio C++ build tools and the Windows SDK.
Recording also requires FFmpeg with the selected encoder. Set its executable in Settings.

```powershell
cargo run --manifest-path snapframe/Cargo.toml
cargo test --manifest-path snapframe/Cargo.toml
powershell -File snapframe/release.ps1
```

The shipped executable is `release/slint/Snapframe.exe`. For a faster optimized development
build, use `cargo build --release --manifest-path snapframe/Cargo.toml`.

## Capture and recording

Print Screen opens the screenshot overlay; Shift+Print Screen opens recording. Drag an
area, click a window (in video mode it is recorded as a window, followed even when covered),
or use the toolbar screen button (with several monitors, then click the one you want), click
empty desktop or press Ctrl+A for the whole screen. Esc cancels; C copies the
colour under the magnifier.
Shortcuts, capture cursor, save folders and audio devices are configurable. Screenshots
always go to the clipboard and can also be saved as PNG.

Windows Graphics Capture supplies GPU textures for screens, regions and windows. NVIDIA
H.264/HEVC recording converts those textures to NV12 on the GPU and encodes them with the
driver's NVENC API. CPU recording downloads GPU-converted NV12 for x264. FFmpeg muxes video
and WASAPI PCM audio into a fragmented MP4 directly in the destination folder. Stop finalizes
and renames `<name>.recording.mp4` to `<name>.mp4`; interrupted recordings retain their
completed fragments. Discard removes the current recording. No raw video or audio files
are spooled.

The Windows callback queues frame handles; a dedicated worker handles GPU processing.
Native NVENC uses absolute monotonic deadlines independent of capture arrival, and retains
the latest image when the source stops changing. Only compressed packets reach its local
video socket. The CPU/compatibility path uses reusable NV12 readback buffers and independent
video/audio socket workers. Queues are bounded; sustained delays produce an error and retain
completed fragments. A Windows job ends FFmpeg when the app exits or crashes.

The native NVIDIA path supports H.264/HEVC, presets P1-P7 and constant quality or target
bitrate. Like OBS it uses 2 B-frames (no lookahead) below the pixel rate of 1440p60; larger
recordings stay P-only, because the B-frame encode bursts delayed screen capture there.
Explicit GPU selection, custom FFmpeg arguments and
other encoders use the NV12 compatibility path so their settings remain effective.
The CPU branch supports pause/resume internally; the native GPU branch currently exposes
Stop and Discard. See [the OBS adaptation](docs/obs-recording-design.md) for exact differences.

A requested 60 FPS does not guarantee 60 distinct screen updates: display refresh, source
animation and compositor delivery still matter. Diagnostic logs distinguish fresh FPS from
output FPS and report dropped frames at each queue. Logs live at
`%APPDATA%\com.bonejay.winshotx\logs\recording.log`; `settings.json` is in the same
parent folder.

In video mode the toolbar has two choices: **30 fps / 60 fps** and **Small / Balanced /
Sharp** quality. Both are saved and used again next time. Levels are tuned for mostly desktop
content where file size matters more than detail: constant quality keeps static screens tiny,
and a peak bitrate scaled to the pixel rate keeps occasional video from ballooning the file.
The encoder is picked automatically once the size of the selection is known:

| | Small | Balanced | Sharp |
|---|---|---|---|
| Peak bitrate at 4K60 / 1080p60 / 1080p30 | 10 / 2.5 / 1.2 Mbps | 17 / 4.4 / 2.2 Mbps | 30 / 7.5 / 3.7 Mbps |
| NVIDIA GPU available (H.264 NVENC) | P4, CQ 34 | P5, CQ 30 | P6, CQ 25 |
| CPU x264, up to 1080p30 | `faster`, CRF 32 | `medium`, CRF 28 | `slow`, CRF 23 |
| CPU x264, above 1080p30 | `veryfast` | `fast` | `medium` |
| CPU x264, from 1440p60 | `superfast` | `veryfast` | `faster` |
| CPU x264, from 4K60 | `ultrafast` | `superfast` | `veryfast` |

NVENC availability is detected once at startup by opening a real encode session; NVENC is
a separate chip, so GPU load from games barely affects it. For x264 the preset moves one
step slower on CPUs with 16+ threads, one step faster with 4 or fewer, and one step faster
when overall CPU use (sampled every 2 s) is 60% or more, never beyond the preflight limits
below.

While selecting, the size label, hovered window and screen picker show the choice (e.g.
`1280 × 720 · CPU · x264 fast`) and warn when a CPU choice may drop frames. The recording
bar shows a GPU/CPU chip and, for five seconds, the encoder, size and frame rate; then live
fps, amber when encoding falls below 90% of the target.

There are no profiles. To force a specific encoder, edit `encoding` in `settings.json`
(e.g. `"encoder": "libx264", "preset": "medium"`); the toolbar then shows `Fixed: libx264`,
and clicking a quality level returns to automatic.

Before recording, testing or previewing a command, the app rejects settings that cannot work
live: odd or oversized dimensions, frame rates outside 5-120, and CPU presets too slow for
the pixel rate (for x264: slower than `slow` above 1080p30, `medium` from 1440p60, and
`veryfast` from 4K60; x265 and SVT-AV1 have stricter limits, libaom-av1 is refused above
1080p30). The message names the setting to change.

**Test encoder** (Settings → Recording) encodes three seconds of generated motion plus audio
at a chosen size (1080p, 1440p or 4K) through the real conversion, encoder and muxer, and
passes only at 110% of the target frame rate. It does not read the screen, so capture load
is not included. Recording is blocked while a test runs. Advanced FFmpeg options accept a
JSON array of video encoding options, such as `["-crf", "18"]`; inputs, codec, frame rate,
size, filters, audio, duration, destination and MP4 muxing flags are managed by the app and
rejected there.

## Validation

Hardware checks are opt-in and must run serially on an unlocked desktop:

```powershell
Set-Location snapframe
cargo test --release -- --ignored --nocapture --test-threads=1 `
  --skip generate_icons --skip render_preview `
  --skip copied_screenshot_pastes_as_a_png_file --skip live_crash_helper
```

The sustained tests open a GPU-animated window with visible frame IDs, record through the
public recorder, and decode the result to detect repeated motion frames. Frame count alone
does not establish smooth capture. See [architecture](docs/slint-architecture.md),
[implementation and OBS source audit](docs/obs-recording-design.md), and
[diagnostics, commands and measured results](docs/recording-performance.md).
