//! Live raw BGRA/PCM -> fragmented MP4. No frame or audio files are spooled.
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::encode::ffmpeg;
use crate::error::{AppError, Result};
use crate::settings::EncodingSettings;

#[derive(Debug, Clone, Copy)]
pub struct AudioFormat {
    pub channels: u16,
    pub sample_rate: u32,
}

#[derive(Debug, Clone)]
pub struct OutputPaths {
    pub partial: PathBuf,
    pub complete: PathBuf,
}

impl OutputPaths {
    pub fn reserve(directory: &Path) -> Result<Self> {
        std::fs::create_dir_all(directory)?;
        let base = format!(
            "winshotx-{}-{}",
            crate::archivos::marca_de_tiempo(),
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let paths = Self {
            partial: directory.join(format!("{base}.recording.mp4")),
            complete: directory.join(format!("{base}.mp4")),
        };
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&paths.partial)?;
        Ok(paths)
    }
}

fn listener() -> Result<TcpListener> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

fn accept(listener: TcpListener, abort: &AtomicBool) -> Result<TcpStream> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if abort.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err(AppError::Msg(
                "FFmpeg did not open its recording input in time.".into(),
            ));
        }
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false)?;
                stream.set_nodelay(true)?;
                stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                return Ok(stream);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Native mode is migrated to x264; the old percentage was an implicit bitrate.
pub fn streaming_options(options: &EncodingSettings) -> EncodingSettings {
    let mut value = options.clone();
    if value.engine == "native" {
        value.encoder = "libx264".into();
        value.preset = "veryfast".into();
        value.rate_control = if value.bitrate().is_some() {
            "bitrate"
        } else {
            "crf"
        }
        .into();
        value.crf = 20;
        value.extra_args = "[]".into();
        value.gpu.clear();
    }
    value.engine = "ffmpeg".into();
    value
}

pub fn arguments(
    options: &EncodingSettings,
    width: u32,
    height: u32,
    fps: u32,
    audio: Option<AudioFormat>,
    video_url: &str,
    audio_url: Option<&str>,
    destination: &Path,
) -> Result<Vec<String>> {
    ffmpeg::validate(options)?;
    if width < 2 || height < 2 || width % 2 != 0 || height % 2 != 0 || !(5..=120).contains(&fps) {
        return Err(AppError::Msg(
            "Choose an even recording size and a frame rate between 5 and 120.".into(),
        ));
    }
    let mut args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "warning".into(),
        "-nostdin".into(),
        "-y".into(),
        "-thread_queue_size".into(),
        "2".into(),
        "-probesize".into(),
        "32".into(),
        "-analyzeduration".into(),
        "0".into(),
        "-f".into(),
        "rawvideo".into(),
        "-pixel_format".into(),
        "bgra".into(),
        "-video_size".into(),
        format!("{width}x{height}"),
        "-framerate".into(),
        fps.to_string(),
        "-i".into(),
        video_url.into(),
    ];
    if let Some(format) = audio {
        args.extend([
            "-thread_queue_size".into(),
            "32".into(),
            "-probesize".into(),
            "32".into(),
            "-analyzeduration".into(),
            "0".into(),
            "-f".into(),
            "s16le".into(),
            "-ar".into(),
            format.sample_rate.to_string(),
            "-ac".into(),
            format.channels.to_string(),
            "-i".into(),
            audio_url
                .ok_or_else(|| AppError::Msg("Missing audio input".into()))?
                .into(),
        ]);
    }
    args.extend(["-map".into(), "0:v:0".into()]);
    if audio.is_some() {
        args.extend(["-map".into(), "1:a:0".into()]);
    }
    args.extend(ffmpeg::codec_args(options)?);
    args.extend(["-g".into(), fps.to_string(), "-bf".into(), "0".into()]);
    if options.encoder == "libx264" {
        args.extend(["-tune".into(), "zerolatency".into()]);
    }
    args.extend(serde_json::from_str::<Vec<String>>(&options.extra_args)?);
    // These final muxing options are managed, even when custom codec flags are set.
    // A conventional MP4 moov written only at close would defeat crash recovery.
    args.extend([
        "-movflags".into(),
        "+frag_keyframe+empty_moov+default_base_moof".into(),
        "-frag_duration".into(),
        "1000000".into(),
        "-flush_packets".into(),
        "1".into(),
        "-max_interleave_delta".into(),
        "1000000".into(),
        "-f".into(),
        "mp4".into(),
        destination.to_string_lossy().into_owned(),
    ]);
    Ok(args)
}

struct VideoPipe {
    tx: Option<mpsc::SyncSender<Vec<u8>>>,
    thread: Option<JoinHandle<Result<()>>>,
    socket: TcpStream,
    recycled: Arc<Mutex<Vec<Vec<u8>>>>,
}

struct AudioPipe {
    tx: Option<mpsc::SyncSender<Vec<u8>>>,
    thread: Option<JoinHandle<Result<()>>>,
}

pub struct Encoder {
    child: Child,
    video: Option<VideoPipe>,
    audio: Option<AudioPipe>,
    abort: Arc<AtomicBool>,
    stderr: Arc<Mutex<VecDeque<u8>>>,
    stderr_thread: Option<JoinHandle<()>>,
    paths: OutputPaths,
    frame_bytes: usize,
    finished: bool,
}

impl Encoder {
    pub fn start(
        paths: OutputPaths,
        options: &EncodingSettings,
        width: u32,
        height: u32,
        fps: u32,
        audio: Option<AudioFormat>,
    ) -> Result<Self> {
        let video_listener = listener()?;
        let audio_listener = if audio.is_some() {
            Some(listener()?)
        } else {
            None
        };
        let url = |l: &TcpListener| -> Result<String> {
            Ok(format!("tcp://{}?tcp_nodelay=1", l.local_addr()?))
        };
        let video_url = url(&video_listener)?;
        let audio_url = audio_listener.as_ref().map(url).transpose()?;
        let args = arguments(
            options,
            width,
            height,
            fps,
            audio,
            &video_url,
            audio_url.as_deref(),
            &paths.partial,
        )?;
        let mut child = ffmpeg::command(&options.ffmpeg_path)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                AppError::Msg(format!(
                    "Could not start FFmpeg: {e}. Set its executable in Recording settings."
                ))
            })?;
        let mut pipe = child.stderr.take().unwrap();
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        let log = stderr.clone();
        let stderr_thread = std::thread::spawn(move || {
            let mut bytes = [0; 4096];
            while let Ok(count) = pipe.read(&mut bytes) {
                if count == 0 {
                    break;
                }
                let mut log = log.lock().unwrap();
                log.extend(&bytes[..count]);
                while log.len() > 16_384 {
                    log.pop_front();
                }
            }
        });
        let abort = Arc::new(AtomicBool::new(false));
        let audio = audio_listener.map(|listener| {
            let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(32);
            let abort = abort.clone();
            let thread = std::thread::spawn(move || {
                let mut socket = accept(listener, &abort)?;
                while !abort.load(Ordering::Acquire) {
                    match rx.recv_timeout(Duration::from_millis(20)) {
                        Ok(data) => socket.write_all(&data)?,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                let _ = socket.shutdown(Shutdown::Write);
                Ok(())
            });
            AudioPipe {
                tx: Some(tx),
                thread: Some(thread),
            }
        });
        let mut encoder = Self {
            child,
            video: None,
            audio,
            abort,
            stderr,
            stderr_thread: Some(stderr_thread),
            paths,
            frame_bytes: width as usize * height as usize * 4,
            finished: false,
        };
        // Detect early command/codec errors rather than waiting for a socket deadline.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            encoder.check()?;
            match video_listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false)?;
                    stream.set_nodelay(true)?;
                    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                    let socket = stream.try_clone()?;
                    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(2);
                    let recycled = Arc::new(Mutex::new(Vec::new()));
                    let pool = recycled.clone();
                    let abort = encoder.abort.clone();
                    let thread = std::thread::spawn(move || {
                        let mut stream = stream;
                        while !abort.load(Ordering::Acquire) {
                            match rx.recv_timeout(Duration::from_millis(20)) {
                                Ok(mut bytes) => {
                                    stream.write_all(&bytes)?;
                                    bytes.clear();
                                    pool.lock().unwrap().push(bytes);
                                }
                                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            }
                        }
                        let _ = stream.shutdown(Shutdown::Write);
                        Ok(())
                    });
                    encoder.video = Some(VideoPipe {
                        tx: Some(tx),
                        thread: Some(thread),
                        socket,
                        recycled,
                    });
                    return Ok(encoder);
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                _ => return Err(encoder.failure("FFmpeg did not connect to its video input.")),
            }
        }
    }

    fn failure(&self, message: &str) -> AppError {
        let bytes: Vec<u8> = self.stderr.lock().unwrap().iter().copied().collect();
        AppError::Msg(format!(
            "{message}\n{}\n\nRecording retained at {}",
            String::from_utf8_lossy(&bytes).trim(),
            self.paths.partial.display()
        ))
    }

    pub fn check(&mut self) -> Result<()> {
        if let Some(status) = self.child.try_wait()? {
            return Err(self.failure(&format!("FFmpeg exited unexpectedly ({status}).")));
        }
        if self
            .video
            .as_ref()
            .filter(|v| v.tx.is_some())
            .and_then(|v| v.thread.as_ref())
            .is_some_and(|t| t.is_finished())
        {
            return Err(self.failure("Video could not be sent to the encoder."));
        }
        if self
            .audio
            .as_ref()
            .filter(|a| a.tx.is_some())
            .and_then(|a| a.thread.as_ref())
            .is_some_and(|t| t.is_finished())
        {
            return Err(self.failure("Audio could not be sent to the encoder."));
        }
        Ok(())
    }

    pub fn try_video(&mut self, bgra: &[u8]) -> Result<bool> {
        if bgra.len() != self.frame_bytes {
            return Err(self.failure("Capture frame size changed unexpectedly."));
        }
        self.check()?;
        let video = self.video.as_ref().unwrap();
        let mut buffer = video.recycled.lock().unwrap().pop().unwrap_or_default();
        buffer.clear();
        buffer.extend_from_slice(bgra);
        match video.tx.as_ref().unwrap().try_send(buffer) {
            Ok(()) => Ok(true),
            Err(mpsc::TrySendError::Full(mut buffer)) => {
                buffer.clear();
                video.recycled.lock().unwrap().push(buffer);
                Ok(false)
            }
            Err(_) => Err(self.failure("The video encoder stopped accepting frames.")),
        }
    }
    pub fn video(&mut self, bgra: &[u8]) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !self.try_video(bgra)? {
            if Instant::now() >= deadline {
                return Err(self.failure(
                    "The video encoder could not keep up. Try a faster preset or lower frame rate.",
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }

    pub fn audio(&mut self, pcm: Vec<u8>) -> Result<()> {
        if self.audio.is_some() {
            let mut pending = pcm;
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                self.check()?;
                match self
                    .audio
                    .as_ref()
                    .unwrap()
                    .tx
                    .as_ref()
                    .unwrap()
                    .try_send(pending)
                {
                    Ok(()) => break,
                    Err(mpsc::TrySendError::Full(data)) if Instant::now() < deadline => {
                        pending = data;
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    _ => return Err(self.failure(
                        "Audio encoding fell behind or stopped. Try a faster encoder or preset.",
                    )),
                }
            }
        }
        Ok(())
    }

    pub fn bytes(&self) -> u64 {
        std::fs::metadata(&self.paths.partial)
            .map(|m| m.len())
            .unwrap_or(0)
    }
    pub fn path(&self) -> &Path {
        &self.paths.partial
    }
    pub fn end_audio(&mut self) {
        if let Some(audio) = self.audio.as_mut() {
            audio.tx.take();
        }
    }

    pub fn finish(self) -> Result<PathBuf> {
        self.finish_by(Instant::now() + Duration::from_secs(10))
    }

    pub(crate) fn finish_by(mut self, deadline: Instant) -> Result<PathBuf> {
        if let Some(video) = self.video.as_mut() {
            video.tx.take();
        }
        self.end_audio();
        loop {
            let video_ready = self
                .video
                .as_ref()
                .and_then(|v| v.thread.as_ref())
                .is_none_or(|t| t.is_finished());
            let audio_ready = self
                .audio
                .as_ref()
                .and_then(|a| a.thread.as_ref())
                .is_none_or(|t| t.is_finished());
            if video_ready && audio_ready {
                break;
            }
            if Instant::now() >= deadline {
                return Err(self.failure(
                    "Encoder inputs did not finish in time. Completed fragments are retained.",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Some(thread) = self.video.as_mut().and_then(|v| v.thread.take()) {
            thread
                .join()
                .map_err(|_| self.failure("The video writer stopped unexpectedly."))?
                .map_err(|e| self.failure(&format!("Video finalization failed: {e}")))?;
        }
        if let Some(thread) = self.audio.as_mut().and_then(|a| a.thread.take()) {
            thread
                .join()
                .map_err(|_| self.failure("The audio writer stopped unexpectedly."))?
                .map_err(|e| self.failure(&format!("Audio finalization failed: {e}")))?;
        }
        loop {
            if let Some(status) = self.child.try_wait()? {
                if let Some(thread) = self.stderr_thread.take() {
                    let _ = thread.join();
                }
                if !status.success() {
                    return Err(self.failure("FFmpeg could not finalize the recording."));
                }
                break;
            }
            if Instant::now() >= deadline {
                return Err(self
                    .failure("FFmpeg did not finish in time. Completed fragments are retained."));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if self.bytes() < 100 {
            return Err(self.failure("No video was encoded."));
        }
        // Windows rename fails if the destination exists; never overwrite a completed clip.
        if self.paths.complete.exists() {
            return Err(self.failure("The final recording filename is already in use."));
        }
        std::fs::rename(&self.paths.partial, &self.paths.complete)?;
        self.finished = true;
        Ok(self.paths.complete.clone())
    }

    pub fn cancel(mut self) {
        self.shutdown();
        let _ = std::fs::remove_file(&self.paths.partial);
    }

    fn shutdown(&mut self) {
        self.abort.store(true, Ordering::Release);
        if let Some(video) = self.video.as_mut() {
            video.tx.take();
            let _ = video.socket.shutdown(Shutdown::Both);
        }
        if !self.finished {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if let Some(thread) = self.video.as_mut().and_then(|v| v.thread.take()) {
            let _ = thread.join();
        }
        if let Some(audio) = self.audio.as_mut() {
            audio.tx.take();
            if let Some(thread) = audio.thread.take() {
                let _ = thread.join();
            }
        }
        if let Some(thread) = self.stderr_thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// PCM is placed on the shared active-time sample clock. Late/overlapping packets
/// are trimmed, gaps become silence, and pending raw audio is capped at two seconds.
pub struct AudioTimeline {
    pub cursor: u64,
    format: AudioFormat,
    pending: VecDeque<u8>,
}

impl AudioTimeline {
    pub fn new(format: AudioFormat) -> Self {
        Self {
            cursor: 0,
            format,
            pending: VecDeque::new(),
        }
    }
    pub fn frames_at(&self, time: Duration) -> u64 {
        (time.as_nanos() * self.format.sample_rate as u128 / 1_000_000_000) as u64
    }
    pub fn push(&mut self, start: u64, data: &[u8]) -> Result<()> {
        let align = self.format.channels as usize * 2;
        let end = self.cursor + (self.pending.len() / align) as u64;
        let skipped = end.saturating_sub(start).min((data.len() / align) as u64) as usize * align;
        let gap = start.saturating_sub(end) as usize * align;
        if self.pending.len() + gap + data.len() - skipped
            > self.format.sample_rate as usize * align * 2
        {
            return Err(AppError::Msg(
                "Audio capture fell more than two seconds behind the recording clock.".into(),
            ));
        }
        self.pending.extend(std::iter::repeat_n(0, gap));
        self.pending.extend(&data[skipped..]);
        Ok(())
    }
    pub fn next(&mut self, target: u64) -> Vec<u8> {
        let frames = target
            .saturating_sub(self.cursor)
            .min(self.format.sample_rate as u64 / 100);
        let count = frames as usize * self.format.channels as usize * 2;
        let mut data: Vec<u8> = self
            .pending
            .drain(..count.min(self.pending.len()))
            .collect();
        data.resize(count, 0);
        self.cursor += frames;
        data
    }
}

pub fn frames_at(time: Duration, fps: u32) -> u64 {
    (time.as_nanos() * fps as u128).div_ceil(1_000_000_000) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fractional_frame_clock_has_no_cumulative_drift() {
        assert_eq!(frames_at(Duration::from_secs(3600), 120), 432000);
        assert_eq!(frames_at(Duration::from_millis(1050), 30), 32);
    }
    #[test]
    fn audio_gaps_overlaps_and_bounds() {
        let mut timeline = AudioTimeline::new(AudioFormat {
            channels: 1,
            sample_rate: 1000,
        });
        timeline.push(2, &[1, 0, 2, 0, 3, 0]).unwrap();
        timeline.push(4, &[9, 0, 4, 0]).unwrap();
        assert_eq!(
            timeline.next(7),
            vec![0, 0, 0, 0, 1, 0, 2, 0, 3, 0, 4, 0, 0, 0]
        );
        timeline.push(3, &[8, 0, 8, 0]).unwrap();
        assert_eq!(timeline.next(9), vec![0, 0, 0, 0]);
        assert!(timeline.push(10000, &[1, 0]).is_err());
    }
    #[test]
    fn legacy_native_preferences_become_explicit_live_encoding_settings() {
        let mut legacy = EncodingSettings::default();
        legacy.engine = "native".into();
        let migrated = streaming_options(&legacy);
        assert_eq!(migrated.engine, "ffmpeg");
        assert_eq!(migrated.encoder, "libx264");
        assert_eq!(migrated.preset, "veryfast");
        assert_eq!(migrated.rate_control, "crf");
        legacy.bitrate_mbps = 12.0;
        assert_eq!(streaming_options(&legacy).rate_control, "bitrate");
    }
    #[test]
    fn live_args_keep_codec_options_but_enforce_recoverable_muxing() {
        let mut options = streaming_options(&EncodingSettings::default());
        options.extra_args = r#"["-movflags", "+faststart", "-crf", "18"]"#.into();
        let args = arguments(
            &options,
            1920,
            1080,
            120,
            None,
            "tcp://127.0.0.1:1",
            None,
            Path::new("test.recording.mp4"),
        )
        .unwrap();
        assert!(!args.contains(&"-vf".into()));
        let flags: Vec<_> = args.windows(2).filter(|w| w[0] == "-movflags").collect();
        assert!(flags.last().unwrap()[1].contains("empty_moov"));
        assert_eq!(args.last().unwrap(), "test.recording.mp4");
    }

    fn live_options() -> EncodingSettings {
        let mut options = streaming_options(&EncodingSettings::default());
        options.preset = "ultrafast".into();
        options
    }
    fn scratch() -> PathBuf {
        std::env::temp_dir().join(format!("winshotx-live-test-{}", uuid::Uuid::new_v4()))
    }
    fn decode(path: &Path, audio: bool) -> String {
        let mut cmd = ffmpeg::command(&live_options().ffmpeg_path);
        cmd.args(["-hide_banner", "-i"])
            .arg(path)
            .args(["-map", "0:v:0"]);
        if audio {
            cmd.args(["-map", "0:a:0"]);
        }
        let output = cmd
            .args(["-progress", "pipe:1", "-f", "null", "-"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }
    fn send_clip(encoder: &mut Encoder, count: u32, audio: bool) -> Vec<u64> {
        let started = Instant::now();
        let mut growth = Vec::new();
        if audio {
            encoder.audio(vec![0; 4800 * 2 * 2]).unwrap();
        }
        for i in 0..count {
            let frame = vec![(i % 200) as u8; 320 * 180 * 4];
            encoder.video(&frame).unwrap();
            if audio && i >= 3 {
                encoder.audio(vec![0; 48000 / 30 * 2 * 2]).unwrap();
            }
            let next = Duration::from_secs_f64((i + 1) as f64 / 30.0);
            if let Some(wait) = next.checked_sub(started.elapsed()) {
                std::thread::sleep(wait);
            }
            if i % 30 == 29 {
                growth.push(encoder.bytes());
            }
        }
        growth
    }
    #[test]
    #[ignore = "Runs installed FFmpeg and real-time sockets"]
    fn live_file_grows_with_audio_then_only_renames_on_stop() {
        let dir = scratch();
        let paths = OutputPaths::reserve(&dir).unwrap();
        let mut encoder = Encoder::start(
            paths.clone(),
            &live_options(),
            320,
            180,
            30,
            Some(AudioFormat {
                channels: 2,
                sample_rate: 48000,
            }),
        )
        .unwrap();
        let growth = send_clip(&mut encoder, 95, true);
        assert!(
            growth[1] > 1000 && growth[2] > growth[1],
            "File did not grow live: {growth:?}"
        );
        assert!(!paths.complete.exists());
        let output = encoder.finish().unwrap();
        assert_eq!(output, paths.complete);
        assert!(!paths.partial.exists());
        let decoded = decode(&output, true);
        assert!(decoded.contains("frame=95"), "{decoded}");
        assert!(decoded.contains("Audio: aac"), "{decoded}");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "Unexpected spool files"
        );
        println!("live file sizes at 1/2/3 seconds: {growth:?}; 95 frames and AAC decoded");
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    #[ignore = "Force-kills an owned FFmpeg child to test fragment recovery"]
    fn live_killed_encoder_keeps_playable_fragments() {
        let dir = scratch();
        let paths = OutputPaths::reserve(&dir).unwrap();
        let mut encoder =
            Encoder::start(paths.clone(), &live_options(), 320, 180, 30, None).unwrap();
        send_clip(&mut encoder, 95, false);
        encoder.child.kill().unwrap();
        encoder.child.wait().unwrap();
        drop(encoder);
        assert!(paths.partial.exists());
        assert!(!paths.complete.exists());
        let decoded = decode(&paths.partial, false);
        assert!(!decoded.contains("frame=0\n"), "{decoded}");
        assert!(std::fs::metadata(&paths.partial).unwrap().len() > 1000);
        println!(
            "Interrupted recording decoded successfully: {}",
            paths.partial.display()
        );
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    #[ignore = "Runs installed FFmpeg"]
    fn live_cancel_deletes_only_the_owned_partial_recording() {
        let dir = scratch();
        let paths = OutputPaths::reserve(&dir).unwrap();
        std::fs::write(dir.join("keep.mp4"), b"keep").unwrap();
        let mut encoder =
            Encoder::start(paths.clone(), &live_options(), 320, 180, 30, None).unwrap();
        send_clip(&mut encoder, 5, false);
        encoder.cancel();
        assert!(!paths.partial.exists());
        assert!(!paths.complete.exists());
        assert_eq!(std::fs::read(dir.join("keep.mp4")).unwrap(), b"keep");
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    #[ignore = "Invoked only by the forced-parent-interruption test"]
    fn live_crash_helper() {
        let Some(dir) = std::env::var_os("WINSHOTX_CRASH_TEST_DIR") else {
            return;
        };
        let dir = PathBuf::from(dir);
        let paths = OutputPaths::reserve(&dir).unwrap();
        let mut encoder = Encoder::start(
            paths.clone(),
            &live_options(),
            320,
            180,
            30,
            Some(AudioFormat {
                channels: 2,
                sample_rate: 48000,
            }),
        )
        .unwrap();
        std::fs::write(
            dir.join("output-path.txt"),
            paths.partial.to_string_lossy().as_bytes(),
        )
        .unwrap();
        send_clip(&mut encoder, 600, true);
        encoder.finish().unwrap();
    }
    #[test]
    #[ignore = "Kills an owned recording test process to simulate an application crash"]
    fn live_parent_crash_keeps_video_and_audio_playable() {
        let dir = scratch();
        std::fs::create_dir_all(&dir).unwrap();
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        let mut helper = cmd
            .args([
                "--exact",
                "record::stream_encoder::tests::live_crash_helper",
                "--ignored",
                "--nocapture",
            ])
            .env("WINSHOTX_CRASH_TEST_DIR", &dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(12);
        let partial = loop {
            let path = std::fs::read_to_string(dir.join("output-path.txt"))
                .ok()
                .map(PathBuf::from);
            if let Some(path) = path {
                if std::fs::metadata(&path)
                    .map(|m| m.len() > 4000)
                    .unwrap_or(false)
                {
                    break path;
                }
            }
            if Instant::now() >= deadline || helper.try_wait().unwrap().is_some() {
                let _ = helper.kill();
                let _ = helper.wait();
                panic!("The recording helper failed to write a fragment");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        helper.kill().unwrap();
        helper.wait().unwrap();
        // No Rust destructors ran in the recording process. The sockets close at OS
        // teardown; FFmpeg may drain them, but playable fragments already existed.
        std::thread::sleep(Duration::from_millis(400));
        let decoded = decode(&partial, true);
        assert!(decoded.contains("Audio: aac"), "{decoded}");
        assert_eq!(partial.extension().unwrap(), "mp4");
        assert!(!decoded.contains("frame=0\n"), "{decoded}");
        println!("Video and AAC audio decoded after forced parent-process termination");
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    #[ignore = "Runs installed FFmpeg with audio EOF before a short video stream"]
    fn live_stop_with_less_than_one_audio_packet_does_not_hang() {
        let dir = scratch();
        let paths = OutputPaths::reserve(&dir).unwrap();
        let mut encoder = Encoder::start(
            paths.clone(),
            &live_options(),
            1280,
            720,
            30,
            Some(AudioFormat {
                channels: 2,
                sample_rate: 48000,
            }),
        )
        .unwrap();
        encoder.audio(vec![0; 192 * 4]).unwrap();
        encoder.end_audio();
        encoder.video(&vec![50; 1280 * 720 * 4]).unwrap();
        let output = encoder.finish().unwrap();
        let decoded = decode(&output, true);
        assert!(decoded.contains("frame=1"));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    #[ignore = "Encodes and decodes a synchronized picture/tone marker with installed FFmpeg"]
    fn live_audio_and_video_markers_stay_synchronized() {
        let dir = scratch();
        let paths = OutputPaths::reserve(&dir).unwrap();
        let options = live_options();
        let mut encoder = Encoder::start(paths, &options, 320, 180, 30, Some(AudioFormat { channels: 2, sample_rate: 48000 })).unwrap();
        let mut pcm = Vec::new();
        for sample in 0..96000 {
            let value = if sample >= 38400 { ((sample as f64 * 440.0 * std::f64::consts::TAU / 48000.0).sin() * 12000.0) as i16 } else { 0 };
            pcm.extend_from_slice(&value.to_le_bytes()); pcm.extend_from_slice(&value.to_le_bytes());
        }
        encoder.audio(pcm).unwrap(); encoder.end_audio();
        for frame in 0..60 {
            encoder.video(&vec![if frame >= 24 { 230 } else { 30 }; 320*180*4]).unwrap();
        }
        let path = encoder.finish().unwrap();
        let video = ffmpeg::command(&options.ffmpeg_path).args(["-v", "error", "-i"]).arg(&path)
            .args(["-map", "0:v:0", "-pix_fmt", "gray", "-f", "rawvideo", "-"]).output().unwrap();
        assert!(video.status.success());
        let marker = video.stdout.chunks_exact(320*180).position(|frame| frame[0] > 130).unwrap(); // midpoint of 30 and 230
        // Decoding to constant frame rate can repeat the first frame once (AAC priming shifts
        // the second frame's timestamp by ~21 ms), so the marker lands on frame 24 or 25.
        assert!((24..=25).contains(&marker), "marker at frame {marker}");
        let audio = ffmpeg::command(&options.ffmpeg_path).args(["-v", "error", "-i"]).arg(&path)
            .args(["-map", "0:a:0", "-f", "s16le", "-"]).output().unwrap();
        assert!(audio.status.success());
        let onset = audio.stdout.chunks_exact(4).position(|sample| i16::from_le_bytes([sample[0], sample[1]]).unsigned_abs() > 2000).unwrap();
        let skew_ms = (onset as f64 / 48.0 - marker as f64 * 1000.0 / 30.0).abs();
        assert!(skew_ms < 50.0, "Picture/tone skew: {skew_ms}ms");
        println!("Decoded picture/tone alignment: {skew_ms:.1}ms");
        let _ = std::fs::remove_dir_all(dir);
    }
}
