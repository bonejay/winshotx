//! Frame-pipeline counters for diagnosing frame rate problems. Every stage counts what it does;
//! the writer logs one line per second to `%APPDATA%\com.bonejay.winshotx\logs\recording.log`
//! (overwritten by each recording).
//!
//! Stages: Windows delivers a frame (`arrived`) → the frame-rate limiter drops or keeps it
//! (`throttled`) → it is queued for the writer (`queued`, or `queue_full` if the writer was
//! behind) → the writer takes it (`received`; `replaced` if a newer one arrived before it was
//! written) → it is written once (`fresh`) or the last frame is repeated (`repeat`) → the
//! encoder was busy (`busy`).

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Stats {
    pub arrived: AtomicU64,
    pub throttled: AtomicU64,
    pub queued: AtomicU64,
    pub queue_full: AtomicU64,
    /// WGC frame handles dropped before readback when its bounded worker queue is full.
    pub capture_full: AtomicU64,
    pub capture_error: parking_lot::Mutex<Option<String>>,
    pub first_frame: parking_lot::Mutex<Option<Instant>>,
    pub received: AtomicU64,
    pub replaced: AtomicU64,
    pub fresh: AtomicU64,
    pub repeat: AtomicU64,
    /// Native GPU submissions which reused the previous captured image.
    pub source_repeat: AtomicU64,
    pub busy: AtomicU64,
    /// Shortest and longest gap between Windows deliveries this second, in microseconds.
    pub gap_min_us: AtomicU64,
    pub gap_max_us: AtomicU64,
    /// Longest GPU readback and CPU copy on the dedicated capture worker.
    pub copy_max_us: AtomicU64,
}

impl Stats {
    pub fn new() -> Self {
        let s = Self::default();
        s.gap_min_us.store(u64::MAX, Relaxed);
        s
    }

    /// Record a delivery gap.
    pub fn gap(&self, gap: Duration) {
        let us = gap.as_micros() as u64;
        self.gap_min_us.fetch_min(us, Relaxed);
        self.gap_max_us.fetch_max(us, Relaxed);
    }
}

#[derive(Default, Clone, Copy)]
struct Snapshot([u64; 11]);

impl Snapshot {
    fn take(s: &Stats) -> Self {
        Self([
            s.arrived.load(Relaxed),
            s.throttled.load(Relaxed),
            s.queued.load(Relaxed),
            s.queue_full.load(Relaxed),
            s.received.load(Relaxed),
            s.replaced.load(Relaxed),
            s.fresh.load(Relaxed),
            s.repeat.load(Relaxed),
            s.busy.load(Relaxed),
            s.capture_full.load(Relaxed),
            s.source_repeat.load(Relaxed),
        ])
    }
}

/// Writes one line per second while recording.
pub struct Log {
    file: Option<std::fs::File>,
    last: Snapshot,
    since: Instant,
    started: Instant,
}

impl Log {
    pub fn start(header: &str) -> Self {
        let dir = crate::settings::config_dir().join("logs");
        let file = std::fs::create_dir_all(&dir)
            .and_then(|_| std::fs::File::create(dir.join("recording.log")))
            .ok();
        let mut log = Self {
            file,
            last: Snapshot::default(),
            since: Instant::now(),
            started: Instant::now(),
        };
        log.line(header);
        log.line("sec  arrived throttled queued full | received replaced | fresh repeat busy | fresh/output fps | delivery gap ms | copy max ms | capture full");
        log
    }

    fn line(&mut self, text: &str) {
        if let Some(f) = self.file.as_mut() {
            let _ = writeln!(f, "{text}");
        }
    }

    /// Log if a second has passed.
    pub fn tick(&mut self, stats: &Stats, fps: u32) {
        if self.since.elapsed() < Duration::from_secs(1) {
            return;
        }
        let secs = self.since.elapsed().as_secs_f64();
        self.since = Instant::now();
        let now = Snapshot::take(stats);
        let d: Vec<u64> = now.0.iter().zip(self.last.0).map(|(a, b)| a - b).collect();
        self.last = now;
        let (min, max) = (
            stats.gap_min_us.swap(u64::MAX, Relaxed),
            stats.gap_max_us.swap(0, Relaxed),
        );
        let gaps = if min == u64::MAX {
            "-".into()
        } else {
            format!("{:.1}..{:.1}", min as f64 / 1000.0, max as f64 / 1000.0)
        };
        let copy = stats.copy_max_us.swap(0, Relaxed) as f64 / 1000.0;
        let out = (d[6] + d[7]) as f64 / secs;
        let repeats = d[7] + d[10];
        let fresh_count = d[6].saturating_sub(d[10]);
        let fresh = fresh_count as f64 / secs;
        let line = format!(
            "{:>3}  {:>7} {:>9} {:>6} {:>4} | {:>8} {:>8} | {:>5} {:>6} {:>4} | {fresh:>5.1}/{out:>5.1} (target {fps}) | {gaps} | {copy:.1} | {}",
            self.started.elapsed().as_secs(),
            d[0], d[1], d[2], d[3], d[4], d[5], fresh_count, repeats, d[8], d[9]
        );
        self.line(&line);
    }
}
