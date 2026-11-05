# Snapframe in pure Rust (Slint)

Native Slint windows handle settings, recording controls and popups. Win32 windows provide
the selection overlay; capture, encoding, audio and clipboard services run independently of the UI.

## Crate layout (`snapframe/`)

```
snapframe/
  build.rs              compiles ui/app.slint, embeds icon + PerMonitorV2 DPI manifest
  release.ps1           shipped build (profile `dist`) → release/slint/Snapframe.exe
  ui/                   Slint markup: theme, icons (Lucide), widgets, popups
                        (recording bar, saved popup, countdown), settings, app (exports)
  src/
    main.rs             single instance, settings load, starts the event loop
    app.rs              App: owns every window, routes shortcuts/tray/results (UI thread)
    actions.rs          screenshot delivery, toolbar fps/quality choice, settings save
    capture/ encode/ record/ recorder.rs audio_devices.rs settings.rs state.rs platform/
                        UI-independent services; the recorder reports
                        through the `RecorderEvents` trait
    encode/preflight.rs shared checks before recording, tests and command preview:
                        managed FFmpeg options, size/fps, CPU preset vs. pixel rate
    encode/check.rs     Test encoder: synthetic throughput at 1080p/1440p/4K
    encode/auto.rs      automatic encoding: encoder/preset/quality from selection size,
                        fps, quality level, NVENC, CPU threads and load
    overlay/model.rs    pure overlay state machine + geometry (unit tested)
    overlay/window.rs   native Win32 layered window per monitor
    overlay/paint.rs    CPU drawing and dirty rectangles
    record/capture_session.rs free-threaded WGC and native GPU output clock
    record/nv12.rs nvenc.rs GPU conversion, native NVENC and optional CPU readback
    ui/popups.rs        recording bar, saved popup, countdown controllers
    ui/settings.rs      Settings ⇄ form conversion (unit tested) + window controller
    ui/format.rs        formatting, shortcut labels, window placement, rolling fps
    shell/              tray, global hotkeys, single instance, low-level key capture
```

## Principles

- **Logic in Rust, presentation in Slint.** `.slint` files only lay out properties and forward
  callbacks. Anything with rules (gestures, snapping, key handling, validation, preset
  summaries) lives in plain Rust structs that are unit tested without a window.
- **One UI thread.** All Slint objects live on the event-loop thread. Background work
  (freezing screens, recording, FFmpeg checks, audio levels) runs on worker threads and reports
  back through `slint::invoke_from_event_loop` with a `Command`/event enum, never by touching
  UI objects directly.
- **Core knows nothing about the UI.** The recorder reports through a `RecorderEvents` sink
  trait, so it is testable and reusable.
- **Shared overlay state.** All monitors' overlays read one `OverlayController`, so mode /
  target / full-screen changes apply everywhere without event broadcasting.
- **Window ownership is explicit.** Settings and popups are reused; native overlay windows
  and their screenshot buffers are destroyed when selection closes.
- Settings stay in `%APPDATA%\com.bonejay.winshotx\settings.json`, so existing settings load.

## Feature parity checklist (and how each is tested)

| Feature | Test |
|---|---|
| Drag an area, move, 8 resize handles; a click takes the window under the cursor (recorded as a window in video mode) or the whole screen on empty desktop | `model` unit tests |
| Window mode (hover highlight + title, click selects), Z-order hit test | `model` unit tests |
| Whole-screen mode, screen picker numbers, keys 1–9, 0 = all screens stitched | `model` tests + manual |
| Keys: Ctrl+A (whole screen under the cursor), C (copy colour), Esc | `model` key tests |
| Magnifier (pixel grid, coordinates, hex), crosshair, placement away from edges | `model` tests (placement, sampling) + manual |
| Dimension badge in physical px, dim outside selection, last-region outline | manual |
| Top bar: Photo/Video, 30/60 fps, Low/Medium/High quality, whole screen, Settings, Close | `layout` and `auto` tests + preview render |
| Multi-monitor: one overlay per monitor, coordinate conversion with negative origins and DPI | geometry unit tests (ported from `pantallas.ts`) |
| Capture delay countdown on the cursor's screen | manual |
| Recording bar: timer, MP4, audio/mic level meters, size, fps, Stop, two-step Discard, Esc | rolling-fps + meter unit tests, manual |
| Bar placement under the region, clamped to the monitor | ported placement tests |
| Saved popup: click = play, folder, close, auto-hide unless hovered | manual |
| Settings: every field, autosave, shortcut recording (suspends global hotkeys), audio check, FFmpeg info, encoder check, folder pickers, theme, autostart, sound | settings-model unit tests + Slint testing-backend tests |
| Tray menu, global shortcuts, single instance (`--capture`, `--cancel`) | manual |
| Clipboard PNG + file drop, recording pipeline, audio clock | existing Rust tests (incl. `--ignored` hardware tests) |

Run everything with `cargo test` (and `cargo test -- --ignored` on a machine with FFmpeg and
an audio device). `snapframe/release.ps1` builds the shipped exe into `release/slint/`.

Build speed: `rust-lld` linker (`.cargo/config.toml`), line-tables-only debug info, thin LTO
for `--release`; full LTO only in the `dist` profile.

## Overlay rendering: native window, CPU drawing (decision)

Measured with two monitors (3072×1728 + 1234×2194), private memory:

| Approach | Idle | Overlay open | After closing |
|---|---|---|---|
| Slint + OpenGL (FemtoVG) | 54 MB | 752 MB | 460 MB (driver keeps it) |
| Slint + software renderer | 56 MB | 312 MB | 122 MB |
| **Win32 layered window + CPU drawing (chosen)** | ~55 MB | ~idle + 2 × screen size | ~idle |

Alternatives considered:
- *Full rewrite in raw Win32/GDI+ (Greenshot/ShareX style)*: lowest idle RAM, but the settings
  window would need hand-made controls that look dated. Capture, encoding and audio code is
  already good; only the UI layer is the problem, so that is all that changes.
- *Direct2D / DirectComposition*: smooth and GPU-accelerated, but brings GPU driver memory back.

Design (`src/overlay/`):
- One `WS_POPUP` **layered** window per monitor (`WS_EX_LAYERED | WS_EX_TOOLWINDOW | TOPMOST`),
  updated with `UpdateLayeredWindowIndirect` + `ULW_OPAQUE` and a dirty rectangle. The window's
  content is set *before* it becomes visible, so no blank/black frame can appear; DWM show/hide
  transitions are disabled.
- Memory per monitor: the frozen screenshot (kept once, RGBA, also used for cropping) plus one
  BGRA back buffer (a DIB section). Both are freed and the windows destroyed when the overlay
  closes. No GPU context at all.
- Drawing (`paint.rs`): only dirty rectangles are redrawn (cursor lines, magnifier, selection,
  toolbar): copy screenshot rows → dim outside the selection → shapes and icons with
  `tiny-skia` (anti-aliased, Lucide SVGs rasterised once with `resvg`) → text with GDI ClearType.
  The magnifier samples the original screenshot with nearest-neighbour scaling.
- Layout (`layout.rs`) and behaviour (`model.rs`) are pure and unit tested; `window.rs` only
  translates Win32 messages (mouse, keys, cursor shape) into model calls.
- Runs on the UI thread: winit's message loop dispatches our window class too. Window
  procedures only forward posted input messages and never hold app state across Win32 calls.
- Everything else (settings, recording bar, popups) stays Slint, on the software renderer, so
  no GPU memory is held anywhere.

## Recording cadence

See [recording diagnostics](recording-performance.md) for the Windows capture interval,
readback worker, lifecycle rules and sustained 60 FPS validation.


## Recording implementation

Screens, regions and selected windows share the native WGC service. Native NVIDIA H.264/HEVC
keeps capture and conversion surfaces on D3D11, then sends compressed packets to FFmpeg for
muxing. CPU and advanced compatibility modes download GPU-converted NV12 and encode in
FFmpeg. WASAPI audio remains independent and uses the first captured frame as its epoch.
The core owns lifecycle, bounded queues, recovery and diagnostics; UI events only display
progress and final outcomes. Long-running finalization stays off the UI thread.

See [the OBS source audit and adaptation](obs-recording-design.md) for exact support,
simplifications and the file map; [recording diagnostics](recording-performance.md) documents
reproduction commands, the visible-motion fixture and measured hardware results.
