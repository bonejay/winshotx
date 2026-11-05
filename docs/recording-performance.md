# Recording cadence, validation and troubleshooting

## Confirmed causes and fixes

On the test machine (RTX 4080, Ryzen 7 5700X3D, 3840x2160 desktop near 120 Hz), the original
log showed only 55-59 fresh frames each second despite nominal 60 FPS output and no sustained
encoder backpressure. A static MP4 frame-rate label hid repeated source images.

A newly created WGC session reported `MinUpdateInterval.Duration = 160000` in 100 ns units
(16 ms). A 30.013-second capture with that default delivered 1,651 fresh frames, about
55.01 FPS. Setting the interval to zero delivered 1,801, about 60.01 FPS, without capture or
output queue overflow. The property is an eligibility interval, not a periodic frame timer.
The app requests source-driven delivery and owns output pacing. Older Windows returning
`E_NOINTERFACE` continue without the optional property; other failures are reported.

There were additional bottlenecks. With audio enabled, holding video for 150 ms overflowed
a six-frame queue (100 ms at 60 FPS), so video now starts immediately. Original 4K60 BGRA
readback/socket transport also moved about 1.99 GB/s for each full copy, before color
conversion and encoding. CPU/compatibility recording now converts on D3D11 and downloads
NV12, about 0.75 GB/s per full copy. Native NVIDIA recording keeps raw pixels on the GPU.

A GPU-resident FFmpeg `ddagrab` experiment still produced repeated motion frames despite
nominal 60 FPS files. Faster capture rates and output-rate filters did not reliably resolve
it. That implementation was removed. Native capture now retains the latest GPU image and
NVENC submissions follow absolute monotonic deadlines. The [OBS design document](obs-recording-design.md)
records the audit, adaptation, exact support and deliberate simplifications.

## Current routes and ownership

| Selected settings | Capture / conversion | Encoding and transport |
|---|---|---|
| H.264/HEVC NVENC, P1-P7, default adapter, no extra arguments | WGC -> owned GPU BGRA -> limited BT.709 NV12 | Native D3D11 NVENC -> timestamped compressed MPEG-TS bridge -> FFmpeg stream-copy MP4 muxing |
| x264 and other CPU encoders | WGC -> GPU NV12 -> reusable staging/readback | Packed NV12 video socket -> FFmpeg video encoder and MP4 muxer |
| Other GPU encoders, explicit GPU, custom arguments/preset aliases | Same NV12 readback compatibility route | FFmpeg honors the requested encoder/options, including the GPU upload it requires |

Screen, region and selected-window capture share these routes. There is no scene compositor
or preview renderer. The callback only hands off WGC frame handles. GPU conversion, mapping,
bitstream collection, row packing and socket I/O all run outside that callback.

The native branch copies each converted frame into an OBS-style ring of registered NV12
textures and collects bitstreams synchronously on the worker. Below 1440p60's pixel rate it
uses 2 B-frames with explicit PTS/DTS; at 1440p60 and above it stays P-only, because the
B-frame encode bursts made Windows miss capture frames there (see the OBS adaptation). GPU
overload can still cause repeated motion.

Compressed packets carry explicit 90 kHz PTS/DTS/PCR through a video-only MPEG-TS bridge; the
MP4 muxer does not have to infer timestamps from raw H.264/HEVC. This bridge is in memory and
on the local socket only, not a temporary recording file. PCM uses a separate bounded socket
worker. Audio is trimmed/aligned to the first capture instant, with jitter buffering before
silence filling. Stop drains audio/video; no full recording re-encode occurs.

## Bounds and failure behavior

- WGC pool: three frames; callback handoff: two frame handles.
- CPU capture channel: four NV12 buffers; writer FIFO: six frames, or sixteen during the
  first second to absorb codec initialization. Recycled buffer pools retain at most four.
- Native GPU channel: thirty-two compressed packets; writer FIFO is bounded at sixty-four.
  A full encoded-packet channel fails instead of discarding dependent video packets.
- Video socket worker: two buffers; audio socket worker: thirty-two PCM chunks.
- Encoder or muxer progress more than two seconds behind is an error. Socket writes have a
  two-second timeout; start and finalization have ten-second deadlines.

Output dimensions stay fixed and even. A resized source is cropped or padded black, including
NV12's limited-range black. Closing the source stops recording. Delivery callbacks are revoked
and synchronized before pool close/recreation. A process-lifetime implicit COM MTA avoids
unloading native WGC code while callbacks are queued; session images are still released.

The live file is `<name>.recording.mp4` in the selected destination. Successful Stop renames
it to `<name>.mp4`. Failure or interruption retains completed fragments; a damaged final
fragment can be lost. Discard removes only the owned recording. A kill-on-close Windows job
terminates FFmpeg after an app crash. Native GPU Pause is currently unavailable; the toolbar
provides Stop and Discard. CPU recording retains pause/resume support internally.

## Reproduce validation

Use an unlocked interactive Windows desktop and run hardware tests serially. Avoid compiling,
GPU-heavy applications, display sleep or remote-desktop transitions during cadence measurement.
Commands below run from the native crate directory so Cargo uses a consistent configuration:

```powershell
Set-Location snapframe
cargo test --release
cargo test --release obs_style_gpu_and_cpu_recording_smoke -- --ignored --nocapture --test-threads=1
cargo test --release sustained_1080p60_nvenc_with_and_without_audio -- --ignored --nocapture --test-threads=1
cargo test --release sustained_4k60_nvenc_screen_with_audio -- --ignored --nocapture --test-threads=1
cargo test --release sustained_cpu_1080p60_with_audio -- --ignored --nocapture --test-threads=1
cargo test --release live_ -- --ignored --nocapture --test-threads=1 --skip live_crash_helper
```

The fixture presents a native flip-model D3D11 swap chain at display refresh. It draws a
moving stripe and a sixteen-bit visible frame ID. Tests call the public recorder, capture
real system audio where selected, Stop normally, decode the MP4, and read IDs from decoded
pixels. They check duration and require at most 1% repeated motion IDs after a one-second
startup period. This catches a nominal 60 FPS file that repeatedly encodes the same source
image. Tests retain files in the temporary directory printed by the harness.

The smoke matrix covers 4K60 native H.264 NVENC P5 with and without audio, 1080p60 x264
veryfast with and without audio, and 1080p60 native HEVC NVENC with audio. Sustained tests cover
30-second window/screen NVIDIA recording, 60-second 4K NVIDIA screen recording, and
30-second CPU 1080p recording. Encoder tests separately cover live growth, interrupted
fragments, short audio, cancellation, pause/resume and synchronized picture/tone markers.
`generate_icons` generates source assets and is not a validation test.

## Measured results

Measurements are specific to this machine and the GPU-presented fixture, not guarantees
under arbitrary gaming, disk or compositor load. The results below use the production recording paths and decoded output.

| Check | Result |
|---|---|
| Native H.264 NVENC P5, 4K60, WASAPI, 60.005 s | 3,601 decoded frames; 0 repeated IDs among 3,540 steady motion frames |
| Native H.264 NVENC P5, 1080p60, selected window +/- audio and screen + audio | Three 30-second cases; 1,801 frames and 0/1,740 repeated steady motion IDs each |
| CPU x264 veryfast, 1080p60, WASAPI, 30.008 s | 1,801 decoded frames; 0 repeated IDs among 1,740 steady motion frames |
| Five-case smoke: H.264 4K60 +/- audio, CPU 1080p60 +/- audio, HEVC 1080p60 + audio | Each five-second case decoded 301 frames; 0/240 repeated steady motion IDs; live file growth and one final MP4 checked |
| Native H.264 and HEVC at 6 Mbps, synchronized picture/tone | 120/120 decoded video frames each; audio 4.7 ms after picture as played |
| CPU/compatibility picture/tone marker | Audio 12 ms before picture as played |

A/V offsets are measured on the file's timestamps as a player presents them (`showinfo` frame
times against `silencedetect`), not by counting decoded frames and samples, which ignores each
stream's start. Two fixed shifts remain: FFmpeg's MP4 writer starts video one frame late, and
AAC priming starts audio 21.3 ms (1024 samples) late. `-avoid_negative_ts` and
`-use_editlist` do not change either; the net offsets stay below one video frame. There is
no drift by design: video timestamps count frames on the recording clock, and audio chunks
are placed on that same clock by their capture time, padded or trimmed, so a sound card
running slightly fast or slow cannot accumulate an offset.
| Live muxer growth, 1/2/3 seconds | 1,230 / 8,712 / 14,743 bytes before Stop; 95 video frames and AAC decoded afterward |
| Forced encoder termination and parent-process termination | Retained fragments decoded with video and AAC |
| Cancellation, short audio, internal CPU pause/resume | Passed; owned-file deletion and shared active-time duration checked |

The complete serial runtime run passed **17/17** selected hardware/FFmpeg tests in 268 s.
The regular suite passed **137** checks, including encoder ABI layouts, geometry, settings,
timing arithmetic, transport timestamps and preset routing; hardware tests are opt-in.
Clipboard mutation, preview generation and icon generation are excluded from the runtime
recording run. These figures describe the checks performed, including the separate bindings
layout checks; they are not a measure of coverage percentage.

The `dist` release build also passed. The verified build is installed at
`release/slint/Snapframe.exe` and was restarted in background mode. Its SHA-256 matches the
build output. The enabled Windows startup entry now targets that executable with
`--background`, preserving the existing startup preference after the previous build path
was removed.

A separate **4K60 CPU x264 ultrafast plus audio stress attempt did not keep up** on this
machine and reached the bounded audio/encoding timeout during startup. This is not a verified
CPU operating point. Use native NVENC for 4K60 here; the verified CPU baseline is 1080p60.
Reducing readback traffic does not remove x264's CPU cost. Existing user presets are not
silently changed to make a benchmark pass.

## Logs and diagnosis

The normal app log is `%APPDATA%\com.bonejay.winshotx\logs\recording.log`, overwritten for
each recording. Tests use isolated temporary configurations. Fields mean:

| Column | Interpretation |
|---|---|
| arrived / throttled | WGC callbacks and deliberate CPU-path source-rate limiting |
| queued / full | Frames or encoded packets accepted/rejected at capture output |
| received / replaced | Writer input and old waiting CPU frames replaced on overflow |
| fresh / repeat / busy | Submitted fresh images, repeated source images, socket-worker backpressure |
| fresh/output FPS | New image submissions versus all video submissions; source freshness is checked independently by visible IDs |
| delivery gap | Shortest/longest callback gap in that second |
| copy max | Worst worker GPU processing/readback/encode duration, depending on the route |
| capture full | WGC frame handles dropped before processing when the handoff was full |

For missing motion, first confirm the application/source actually updates at the requested
rate. A static desktop legitimately repeats. A full capture handoff or large delivery gaps
point to source/compositor/processing delays; repeated writer overflow or busy counts point
to encoding/transport capacity. Try the native GPU preset for supported NVIDIA hardware, a
faster CPU preset, a lower resolution, or a lower frame rate according to the failing stage.
A source refreshing below 60 Hz cannot supply sixty distinct images per second.

The Settings encoder check (`encode/check.rs`) runs preflight, then encodes three seconds of
generated moving content and AAC audio at 1080p, 1440p or 4K through the production NV12
conversion, native NVENC or FFmpeg encoder, transport and muxer. It passes only above 110% of
the target frame rate and gives up after 20 seconds. It does not read the screen, so it does
not include capture load or prove sustained cadence. The command preview shows the
FFmpeg part only; native capture/conversion/NVENC runs inside the app. Native driver failures
include the NVENC operation and driver status. Completed fragments remain at the reported path.

## Primary references

- [Microsoft MinUpdateInterval](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.graphicscapturesession.minupdateinterval)
- [Pinned OBS video pipeline](https://github.com/obsproject/obs-studio/blob/7a25c3fd8b5eb87c2d2e0606cae36e32014bc6fa/libobs/obs-video.c)
- [Pinned OBS WGC implementation](https://github.com/obsproject/obs-studio/blob/7a25c3fd8b5eb87c2d2e0606cae36e32014bc6fa/libobs-winrt/winrt-capture.cpp)
- [NVIDIA encoder API guide](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-video-encoder-api-prog-guide/index.html)
