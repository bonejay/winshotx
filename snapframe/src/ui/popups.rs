//! Controllers for the small floating windows: recording toolbar, "Recording saved" popup and
//! the capture-delay countdown.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use slint::ComponentHandle;

use super::format::{self, RollingFps};
use crate::platform::window_style as ws;
use crate::recorder::{RecordingTick, SessionInfo};
use crate::{Countdown, RecorderBar, SavedPopup};

fn place(window: &slint::Window, (x, y, w, h): (i32, i32, u32, u32)) {
    window.set_position(slint::PhysicalPosition::new(x, y));
    window.set_size(slint::PhysicalSize::new(w, h));
}

/// Float a window above everything without taking focus or appearing in the taskbar.
fn float(window: &slint::Window, no_focus: bool) {
    if let Some(hwnd) = crate::shell::hwnd(window) {
        if no_focus {
            ws::never_focus(hwnd);
        } else {
            ws::tool_window(hwnd);
        }
        ws::rounded_corners(hwnd);
        ws::no_transitions(hwnd);
        ws::topmost_no_activate(hwnd);
    }
}

/// How long the discard button stays armed waiting for the second click.
const DISCARD_ARMED: Duration = Duration::from_secs(4);

pub struct Bar {
    pub window: RecorderBar,
    fps: RollingFps,
    target_fps: u32,
    disarm: slint::Timer,
    detail: slint::Timer,
}

/// How long the bar shows which encoder was chosen before switching to live numbers.
const DETAIL_VISIBLE: Duration = Duration::from_secs(5);

impl Bar {
    pub fn new(on_stop: impl Fn() + 'static, on_discard: impl Fn() + 'static) -> Option<Self> {
        let window = RecorderBar::new().ok()?;
        window.on_stop(on_stop);
        window.on_discard(on_discard);
        let weak = window.as_weak();
        window.on_drag(move || {
            use slint::winit_030::WinitWindowAccessor;
            if let Some(w) = weak.upgrade() {
                w.window().with_winit_window(|ww| {
                    let _ = ww.drag_window();
                });
            }
        });
        Some(Self {
            window,
            fps: RollingFps::default(),
            target_fps: 0,
            disarm: slint::Timer::default(),
            detail: slint::Timer::default(),
        })
    }

    /// Show under the recorded region, kept out of the recording itself.
    pub fn show(&mut self, session: &SessionInfo, audio: bool, microphone: bool) {
        let region = session.region;
        self.fps = RollingFps::default();
        self.target_fps = session.fps;
        let w = &self.window;
        w.set_elapsed("0:00".into());
        w.set_saving(false);
        w.set_armed(false);
        w.set_size("".into());
        w.set_fps("".into());
        w.set_behind(false);
        let (hardware, encoder) = crate::encode::auto::describe_parts(&session.encoding);
        w.set_engine(hardware.into());
        w.set_detail(
            format!(
                "{encoder} · {}×{} · {} fps",
                region.width, region.height, session.fps
            )
            .into(),
        );
        let weak = w.as_weak();
        self.detail
            .start(slint::TimerMode::SingleShot, DETAIL_VISIBLE, move || {
                if let Some(w) = weak.upgrade() {
                    w.set_detail("".into());
                }
            });
        w.set_audio(audio);
        w.set_microphone(microphone);
        let (cx, cy) = region.center();
        let pos = format::bar_position(region, ws::monitor_at(cx, cy));
        place(w.window(), pos);
        let _ = w.show();
        place(w.window(), pos);
        if let Some(hwnd) = crate::shell::hwnd(w.window()) {
            ws::exclude_from_capture(hwnd, true);
        }
        float(w.window(), false);
    }

    pub fn tick(&mut self, t: &RecordingTick) {
        let w = &self.window;
        w.set_elapsed(format::duration(t.elapsed_ms).into());
        w.set_saving(t.saving);
        w.set_audio(t.audio);
        w.set_microphone(t.microphone);
        w.set_audio_level(t.audio_level);
        w.set_microphone_level(t.microphone_level);
        w.set_size(
            if t.bytes > 0 {
                format::bytes(t.bytes)
            } else {
                String::new()
            }
            .into(),
        );
        if let Some(fps) = self.fps.push(t.elapsed_ms, t.frames) {
            w.set_fps(format!("{fps} fps").into());
            // Output frames are written at the target rate unless encoding falls behind.
            w.set_behind(fps * 10 < self.target_fps * 9);
        }
    }

    /// First press arms; the second within four seconds returns `true` (discard).
    pub fn discard_pressed(&mut self) -> bool {
        if self.window.get_saving() {
            return false;
        }
        if self.window.get_armed() {
            self.disarm.stop();
            return true;
        }
        self.window.set_armed(true);
        let weak = self.window.as_weak();
        self.disarm
            .start(slint::TimerMode::SingleShot, DISCARD_ARMED, move || {
                if let Some(w) = weak.upgrade() {
                    w.set_armed(false);
                }
            });
        false
    }

    pub fn set_saving(&self) {
        self.window.set_saving(true);
    }

    pub fn hide(&mut self) {
        self.disarm.stop();
        self.detail.stop();
        let _ = self.window.hide();
    }
}

/// How long the saved popup stays unless the pointer is over it.
const SAVED_VISIBLE: Duration = Duration::from_secs(8);

pub struct Saved {
    pub window: SavedPopup,
    path: Rc<std::cell::RefCell<PathBuf>>,
    timer: slint::Timer,
}

impl Saved {
    pub fn new() -> Option<Self> {
        let window = SavedPopup::new().ok()?;
        let path: Rc<std::cell::RefCell<PathBuf>> = Rc::default();
        let open =
            |reveal: bool, path: Rc<std::cell::RefCell<PathBuf>>, weak: slint::Weak<SavedPopup>| {
                move || {
                    let Some(w) = weak.upgrade() else { return };
                    match crate::platform::open_recording(&path.borrow(), reveal) {
                        Ok(()) => {
                            let _ = w.hide();
                        }
                        Err(e) => w.set_error(e.to_string().into()),
                    }
                }
            };
        window.on_play(open(false, path.clone(), window.as_weak()));
        window.on_reveal(open(true, path.clone(), window.as_weak()));
        let weak = window.as_weak();
        window.on_dismiss(move || {
            if let Some(w) = weak.upgrade() {
                let _ = w.hide();
            }
        });
        Some(Self {
            window,
            path,
            timer: slint::Timer::default(),
        })
    }

    pub fn show(&mut self, path: PathBuf) {
        let w = &self.window;
        w.set_name(
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into(),
        );
        w.set_error("".into());
        *self.path.borrow_mut() = path;
        let monitor = ws::monitor_at(0, 0);
        let _ = w.show();
        if let Some(m) = monitor {
            place(w.window(), format::corner_position(m, (380.0, 84.0)));
        }
        float(w.window(), true);
        // Close after 8 s without the pointer over it.
        let weak = w.as_weak();
        let idle = Rc::new(Cell::new(Duration::ZERO));
        const STEP: Duration = Duration::from_millis(250);
        self.timer.start(slint::TimerMode::Repeated, STEP, move || {
            let Some(w) = weak.upgrade() else { return };
            if w.get_hovered() {
                idle.set(Duration::ZERO);
            } else {
                idle.set(idle.get() + STEP);
            }
            if idle.get() >= SAVED_VISIBLE || !w.window().is_visible() {
                let _ = w.hide();
            }
        });
    }
}

pub struct CountdownCtl {
    pub window: Countdown,
    timer: slint::Timer,
}

impl CountdownCtl {
    pub fn new() -> Option<Self> {
        Some(Self {
            window: Countdown::new().ok()?,
            timer: slint::Timer::default(),
        })
    }

    /// Show the seconds left on the cursor's screen. It never takes focus, so an open menu
    /// stays open for the capture.
    pub fn show(&mut self, seconds: u32) {
        let w = &self.window;
        w.set_seconds(seconds as i32);
        let (cx, cy) = ws::cursor_position().unwrap_or_default();
        let _ = w.show();
        if let Some(m) = ws::monitor_at(cx, cy) {
            place(w.window(), format::centred(m, (132.0, 132.0)));
        }
        float(w.window(), true);
        let weak = w.as_weak();
        self.timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(1),
            move || {
                if let Some(w) = weak.upgrade() {
                    w.set_seconds((w.get_seconds() - 1).max(0));
                }
            },
        );
    }

    pub fn hide(&mut self) {
        self.timer.stop();
        let _ = self.window.hide();
        self.window.set_seconds(0);
    }
}
