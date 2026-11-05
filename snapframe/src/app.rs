//! The application: owns every window and routes input, shortcuts, tray actions and background
//! results. Lives on the UI thread; other threads reach it through [`on_ui`].

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use slint::ComponentHandle;

use crate::capture::{Freeze, Rect, WindowRect};
use crate::overlay::model::Action;
use crate::overlay::window::{Cursor, Input};
use crate::overlay::{Command, Overlays, RecordingView};
use crate::recorder::{Outcome, RecordOptions, RecorderEvents, RecordingTick};
use crate::settings::Settings;
use crate::shell::hotkeys::{Hotkeys, Shortcut};
use crate::shell::tray::TrayAction;
use crate::state::{AppState, CandadoCaptura, OverlayIntent};
use crate::ui::popups::{Bar, CountdownCtl, Saved};
use crate::ui::settings::{self, SettingsCtl};

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

/// Run `f` with the app, on the UI thread.
pub fn with_app(f: impl FnOnce(&Rc<App>)) {
    let app = APP.with(|a| a.borrow().clone());
    if let Some(app) = app {
        f(&app);
    }
}

/// Run `f` with the app from any thread.
pub fn on_ui(f: impl FnOnce(&Rc<App>) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || with_app(f));
}

/// The recorder reports through this.
struct UiEvents;

impl RecorderEvents for UiEvents {
    fn tick(&self, tick: RecordingTick) {
        on_ui(move |a| a.recording_tick(tick));
    }
    fn finished(&self, outcome: Outcome) {
        on_ui(move |a| a.recording_finished(outcome));
    }
}

/// Margin between hiding the countdown and freezing, so the desktop repaints where it was.
const REPAINT_MARGIN: Duration = Duration::from_millis(120);

pub struct App {
    pub state: Arc<AppState>,
    events: Arc<UiEvents>,
    overlay: RefCell<Overlays>,
    bar: RefCell<Option<Bar>>,
    saved: RefCell<Option<Saved>>,
    countdown: RefCell<Option<CountdownCtl>>,
    settings: RefCell<Option<SettingsCtl>>,
    hotkeys: RefCell<Option<Hotkeys>>,
    tray: RefCell<Option<tray_icon::TrayIcon>>,
    delay: slint::Timer,
    /// Which shortcut field is recording keys: 0 none, 1 screenshot, 2 recording.
    listening: std::cell::Cell<i32>,
}

impl App {
    /// Create the app and its process-wide hooks. Must run on the UI thread before the event
    /// loop starts.
    pub fn install(state: Arc<AppState>) -> Rc<App> {
        let app = Rc::new(App {
            state,
            events: Arc::new(UiEvents),
            overlay: RefCell::default(),
            bar: RefCell::default(),
            saved: RefCell::default(),
            countdown: RefCell::default(),
            settings: RefCell::default(),
            hotkeys: RefCell::default(),
            tray: RefCell::default(),
            delay: slint::Timer::default(),
            listening: std::cell::Cell::new(0),
        });
        APP.with(|a| *a.borrow_mut() = Some(app.clone()));

        *app.hotkeys.borrow_mut() = Hotkeys::new(|which| {
            on_ui(move |a| match which {
                Shortcut::Capture => a.open_capture(OverlayIntent::Capture),
                Shortcut::Record => a.record_shortcut(),
            })
        });
        app.register_hotkeys();
        *app.tray.borrow_mut() =
            crate::shell::tray::build(|action| on_ui(move |a| a.tray_action(action)));
        app
    }

    fn settings_snapshot(&self) -> Settings {
        self.state.settings.read().clone()
    }

    pub fn register_hotkeys(&self) {
        let settings = self.settings_snapshot();
        if let Some(h) = self.hotkeys.borrow_mut().as_mut() {
            let status = h.register(&settings);
            *self.state.shortcuts.write() = status;
        }
    }

    pub fn tray_action(&self, action: TrayAction) {
        match action {
            TrayAction::Capture => self.open_capture(OverlayIntent::Capture),
            TrayAction::Record => self.record_shortcut(),
            TrayAction::ScreenshotFolder | TrayAction::VideoFolder => {
                let s = self.settings_snapshot();
                let dir = if action == TrayAction::ScreenshotFolder {
                    s.save_directory
                } else {
                    s.video_directory
                };
                let _ = crate::platform::open_folder(&PathBuf::from(dir));
            }
            TrayAction::Settings => self.show_settings(),
            TrayAction::Quit => {
                if self.state.is_recording() {
                    let _ = crate::recorder::stop(&self.state, self.events.clone());
                }
                self.tray.borrow_mut().take();
                let _ = slint::quit_event_loop();
            }
        }
    }

    pub fn instance_request(&self, request: crate::shell::instance::Request) {
        use crate::shell::instance::Request;
        match request {
            Request::Show => self.show_settings(),
            Request::Capture => self.open_capture(OverlayIntent::Capture),
            Request::Cancel => self.close_overlays(),
        }
    }

    /// The record shortcut stops a running recording, otherwise opens the overlay in video mode.
    pub fn record_shortcut(&self) {
        if self.state.is_recording() {
            self.stop_recording();
        } else {
            self.open_capture(OverlayIntent::Record);
        }
    }

    // ------------------------------------------------------------------ capture

    /// Entry point of both shortcuts: optional delay, then freeze the screens and show the
    /// overlays.
    pub fn open_capture(&self, intent: OverlayIntent) {
        if self.state.is_recording() || self.overlay.borrow().open {
            return;
        }
        // A second press while a capture is starting is ignored.
        let Some(lock) = self.state.intentar_capturar() else {
            return;
        };
        *self.state.intent.write() = intent;
        let seconds = self.state.settings.read().capture_delay_seconds;
        if seconds == 0 || intent == OverlayIntent::Record {
            freeze(lock);
            return;
        }
        if self.countdown.borrow().is_none() {
            *self.countdown.borrow_mut() = CountdownCtl::new();
        }
        if let Some(c) = self.countdown.borrow_mut().as_mut() {
            c.show(seconds);
        }
        let lock = std::cell::Cell::new(Some(lock));
        self.delay.start(
            slint::TimerMode::SingleShot,
            Duration::from_secs(u64::from(seconds)).saturating_sub(REPAINT_MARGIN),
            move || {
                with_app(|a| {
                    if let Some(c) = a.countdown.borrow_mut().as_mut() {
                        c.hide();
                    }
                });
                let lock = lock.take();
                slint::Timer::single_shot(REPAINT_MARGIN, move || {
                    if let Some(lock) = lock {
                        freeze(lock);
                    }
                });
            },
        );
    }

    /// Input from an overlay window (called from its window procedure).
    pub fn overlay_input(&self, id: u32, input: Input) {
        // Ignore input that arrives while the overlay state is in use (re-entrant messages).
        let command = match self.overlay.try_borrow_mut() {
            Ok(mut o) => o.input(id, input),
            Err(_) => return,
        };
        match command {
            Command::None => {}
            Command::Model(action, region) => self.overlay_result(id, (action, region)),
            Command::SetRecording { fps, level } => self.set_recording_choice(fps, level),
            Command::OpenSettings => self.show_settings(),
        }
    }

    pub fn overlay_cursor(&self, id: u32) -> Cursor {
        self.overlay
            .try_borrow()
            .map(|o| o.cursor(id))
            .unwrap_or_default()
    }

    pub fn overlay_clear_toast(&self) {
        if let Ok(mut o) = self.overlay.try_borrow_mut() {
            o.clear_toast();
        }
    }

    fn show_overlays(
        &self,
        freezes: Vec<Freeze>,
        windows: Vec<WindowRect>,
        cursor: Option<(i32, i32)>,
    ) {
        *self.state.freezes.write() = freezes.clone();
        let settings = self.settings_snapshot();
        let intent = *self.state.intent.read();
        self.overlay
            .borrow_mut()
            .open(&freezes, &windows, cursor, &settings, intent);
    }

    /// Hide the overlays and free the frozen screenshots (tens of MB per screen).
    pub fn close_overlays(&self) {
        self.overlay.borrow_mut().close();
        self.state.freezes.write().clear();
    }

    fn overlay_result(&self, id: u32, (action, region): (Action, Option<Rect>)) {
        match action {
            Action::None => {}
            Action::Cancel => self.close_overlays(),
            Action::SetShared(shared) => self.overlay.borrow_mut().set_shared(shared),
            Action::CopyColor => {
                let color = self.overlay.borrow().color_at_cursor(id);
                if let Some((hex, swatch)) = color {
                    let result = crate::platform::clipboard::copy_text(&hex);
                    let mut overlay = self.overlay.borrow_mut();
                    match result {
                        Ok(()) => overlay.toast(
                            &format!("{} copied", hex.to_uppercase()),
                            false,
                            Some(swatch),
                        ),
                        Err(e) => overlay.toast(&e.to_string(), true, None),
                    }
                }
            }
            Action::Capture(_) => {
                let Some(region) = region else { return };
                // The worker keeps its own reference; closing releases the shared copy.
                let freezes = self.state.freezes.read().clone();
                self.close_overlays();
                let state = self.state.clone();
                std::thread::spawn(move || {
                    report(crate::actions::capture_region(&state, &freezes, region))
                });
            }
            Action::Record(_) => {
                if let Some(region) = region {
                    self.start_recording(region, None);
                }
            }
            Action::RecordWindow(_, hwnd) => {
                if let Some(region) = region {
                    self.start_recording(region, Some(hwnd));
                }
            }
        }
    }

    /// A frame rate or quality level chosen on the toolbar.
    fn set_recording_choice(&self, fps: Option<u32>, level: Option<usize>) {
        match crate::actions::set_recording_choice(&self.state, fps, level) {
            Ok(s) => {
                self.overlay
                    .borrow_mut()
                    .set_recording(RecordingView::from_settings(&s));
                self.refresh_settings();
            }
            Err(e) => self.overlay.borrow_mut().toast(&e.to_string(), true, None),
        }
    }

    // ------------------------------------------------------------------ recording

    fn start_recording(&self, region: Rect, window: Option<isize>) {
        if let Some(s) = self.settings.borrow_mut().as_mut() {
            s.stop_audio_check();
        }
        let settings = self.settings_snapshot();
        let options = RecordOptions {
            audio: settings.record_audio,
            microphone: settings.record_microphone,
            window,
        };
        self.overlay.borrow_mut().set_busy(true);
        let result =
            crate::recorder::start(&self.state, self.events.clone(), region, options.clone());
        self.overlay.borrow_mut().set_busy(false);
        match result {
            Ok(session) => {
                self.close_overlays();
                if self.bar.borrow().is_none() {
                    *self.bar.borrow_mut() = Bar::new(
                        || with_app(|a| a.stop_recording()),
                        || with_app(|a| a.discard_pressed()),
                    );
                }
                if let Some(bar) = self.bar.borrow_mut().as_mut() {
                    bar.show(&session, options.audio, options.microphone);
                }
            }
            Err(e) => self.overlay.borrow_mut().toast(&e.to_string(), true, None),
        }
    }

    pub fn stop_recording(&self) {
        if let Some(bar) = self.bar.borrow().as_ref() {
            bar.set_saving();
        }
        if let Err(e) = crate::recorder::stop(&self.state, self.events.clone()) {
            eprintln!("could not stop the recording: {e}");
        }
    }

    fn discard_pressed(&self) {
        let confirmed = self
            .bar
            .borrow_mut()
            .as_mut()
            .is_some_and(|b| b.discard_pressed());
        if confirmed {
            let _ = crate::recorder::cancel(&self.state, self.events.clone());
        }
    }

    fn recording_tick(&self, tick: RecordingTick) {
        if let Some(bar) = self.bar.borrow_mut().as_mut() {
            bar.tick(&tick);
        }
    }

    fn recording_finished(&self, outcome: Outcome) {
        if let Some(bar) = self.bar.borrow_mut().as_mut() {
            bar.hide();
        }
        match outcome {
            Outcome::Saved(path) => {
                if self.saved.borrow().is_none() {
                    *self.saved.borrow_mut() = Saved::new();
                }
                if let Some(popup) = self.saved.borrow_mut().as_mut() {
                    popup.show(path);
                }
            }
            Outcome::Discarded => {}
            Outcome::Failed(message) => error_dialog("Recording could not be completed", &message),
        }
    }

    // ------------------------------------------------------------------ settings

    pub fn show_settings(&self) {
        self.close_overlays();
        if self.settings.borrow().is_none() {
            let Some(ctl) = SettingsCtl::new() else {
                return;
            };
            wire_settings(&ctl.window);
            *self.settings.borrow_mut() = Some(ctl);
            self.refresh_devices();
            self.check_ffmpeg();
        }
        self.refresh_settings();
        if let Some(ctl) = self.settings.borrow_mut().as_mut() {
            ctl.show();
        }
    }

    /// Write the current settings into the settings window, if it exists.
    pub fn refresh_settings(&self) {
        let s = self.settings_snapshot();
        let status = *self.state.shortcuts.read();
        if let Some(ctl) = self.settings.borrow_mut().as_mut() {
            ctl.load(&s, status);
            ctl.set_meters(&s, None);
            ctl.window.set_listening(self.listening.get());
        }
    }

    /// Save new settings from any source; reports errors in the settings window.
    pub fn save_settings(&self, next: Settings) {
        let previous = self.settings_snapshot();
        match crate::actions::store_settings(&self.state, next) {
            Ok(saved) => {
                self.register_hotkeys();
                let mut error = String::new();
                if previous.start_with_windows != saved.start_with_windows {
                    if let Err(e) = crate::platform::autostart::set(saved.start_with_windows) {
                        error = e.to_string();
                    }
                }
                if previous.encoding.ffmpeg_path != saved.encoding.ffmpeg_path {
                    self.check_ffmpeg();
                }
                if previous.record_audio != saved.record_audio
                    || previous.record_microphone != saved.record_microphone
                    || previous.system_audio_device != saved.system_audio_device
                    || previous.microphone_device != saved.microphone_device
                {
                    if let Some(c) = self.settings.borrow_mut().as_mut() {
                        c.stop_audio_check();
                    }
                }
                self.overlay
                    .borrow_mut()
                    .set_recording(RecordingView::from_settings(&saved));
                self.refresh_settings();
                if let Some(c) = self.settings.borrow().as_ref() {
                    c.window.set_saved(error.is_empty());
                    c.window.set_error(error.into());
                }
            }
            Err(e) => {
                if let Some(c) = self.settings.borrow().as_ref() {
                    c.window.set_saved(false);
                    c.window.set_error(e.to_string().into());
                }
            }
        }
    }

    fn form_changed(&self) {
        let next = {
            let ctl = self.settings.borrow();
            let Some(ctl) = ctl.as_ref() else { return };
            settings::form::from_data(
                &self.settings_snapshot(),
                &ctl.window.get_data(),
                &ctl.lists,
            )
        };
        self.save_settings(next);
    }

    fn check_ffmpeg(&self) {
        let path = self.state.settings.read().encoding.ffmpeg_path.clone();
        if let Some(c) = self.settings.borrow().as_ref() {
            c.window.set_ffmpeg_status("Checking FFmpeg…".into());
        }
        std::thread::spawn(move || {
            let info = crate::encode::ffmpeg::info(&path);
            on_ui(move |a| {
                if let Some(c) = a.settings.borrow_mut().as_mut() {
                    c.set_ffmpeg(&info);
                }
                a.refresh_settings();
            });
        });
    }

    fn refresh_devices(&self) {
        std::thread::spawn(|| {
            let result = crate::audio_devices::list();
            on_ui(move |a| {
                if let Some(c) = a.settings.borrow_mut().as_mut() {
                    c.stop_audio_check();
                    match result {
                        Ok(devices) => {
                            c.devices = devices;
                            c.window.set_audio_error("".into());
                        }
                        Err(e) => c.window.set_audio_error(e.to_string().into()),
                    }
                }
                a.refresh_settings();
            });
        });
    }

    fn toggle_audio_check(&self) {
        let s = self.settings_snapshot();
        let mut guard = self.settings.borrow_mut();
        let Some(c) = guard.as_mut() else { return };
        if c.audio_check.is_some() {
            c.stop_audio_check();
            c.set_meters(&s, None);
            return;
        }
        if self.state.is_recording() {
            c.window
                .set_audio_error("Stop recording before checking audio levels.".into());
            return;
        }
        match crate::audio_devices::start(
            s.record_audio,
            s.record_microphone,
            s.system_audio_device.clone(),
            s.microphone_device.clone(),
        ) {
            Ok(id) => {
                c.audio_check = Some(id.clone());
                c.window.set_checking(true);
                c.window.set_audio_error("".into());
                c.set_meters(&s, None);
                c.audio_timer.start(
                    slint::TimerMode::Repeated,
                    settings::LEVEL_POLL,
                    move || {
                        let levels = crate::audio_devices::levels(&id);
                        with_app(|a| {
                            let s = a.settings_snapshot();
                            let mut guard = a.settings.borrow_mut();
                            let Some(c) = guard.as_mut() else { return };
                            match levels {
                                Ok(l) if l.running => c.set_meters(&s, Some(&l)),
                                Ok(l) => {
                                    c.set_meters(&s, Some(&l));
                                    c.stop_audio_check();
                                }
                                Err(e) => {
                                    c.window.set_audio_error(e.to_string().into());
                                    c.stop_audio_check();
                                }
                            }
                        });
                    },
                );
            }
            Err(e) => c.window.set_audio_error(e.to_string().into()),
        }
    }

    fn test_encoder(&self) {
        let permit = match self.state.begin_encoder_test() {
            Ok(permit) => permit,
            Err(error) => {
                if let Some(c) = self.settings.borrow().as_ref() {
                    c.window.set_test_ok(false);
                    c.window.set_test_result(error.to_string().into());
                }
                return;
            }
        };
        let s = self.settings_snapshot();
        let test_size = self
            .settings
            .borrow()
            .as_ref()
            .map(|c| c.window.get_test_size_index())
            .unwrap_or(0);
        let (width, height) = crate::encode::check::dimensions(test_size);
        let key = settings::test_key(&s, test_size);
        if let Some(c) = self.settings.borrow_mut().as_mut() {
            c.test_key = key.clone();
            c.window.set_testing(true);
            c.window.set_test_result("".into());
        }
        std::thread::spawn(move || {
            let result = crate::encode::ffmpeg::test_encoding(&s.encoding, width, height, s.fps);
            drop(permit);
            on_ui(move |a| {
                let current_size = a
                    .settings
                    .borrow()
                    .as_ref()
                    .map(|c| c.window.get_test_size_index())
                    .unwrap_or(0);
                let current = settings::test_key(&a.settings_snapshot(), current_size);
                let Some(c) = a
                    .settings
                    .borrow_mut()
                    .as_mut()
                    .map(|c| c.window.clone_strong())
                else {
                    return;
                };
                c.set_testing(false);
                if current != key {
                    return;
                }
                match result {
                    Ok(r) => {
                        c.set_test_ok(true);
                        c.set_test_result(format!(
                            "Passed: {} × {} at {} fps with {}; synthetic throughput {:.0} fps with AAC audio. Capture and other apps can still reduce performance.",
                            r.width, r.height, r.fps, r.plan, r.measured_fps
                        ).into());
                    }
                    Err(e) => {
                        c.set_test_ok(false);
                        c.set_test_result(e.to_string().into());
                    }
                }
            });
        });
    }

    fn listen_shortcut(&self, which: i32) {
        crate::shell::keycapture::stop();
        if self.listening.get() == which {
            self.listening.set(0);
            self.register_hotkeys();
            self.refresh_settings();
            return;
        }
        // The global shortcuts are off while listening, so pressing the current combination
        // does not take a screenshot.
        if let Some(h) = self.hotkeys.borrow_mut().as_mut() {
            h.suspend();
        }
        self.listening.set(which);
        let started = crate::shell::keycapture::start(move |result| {
            with_app(|a| {
                a.listening.set(0);
                match result {
                    Some(shortcut) => {
                        let mut next = a.settings_snapshot();
                        if which == 1 {
                            next.capture_shortcut = shortcut;
                        } else {
                            next.record_shortcut = shortcut;
                        }
                        a.save_settings(next);
                    }
                    None => {
                        a.register_hotkeys();
                        a.refresh_settings();
                    }
                }
            });
        });
        if !started {
            self.listening.set(0);
            self.register_hotkeys();
        }
        self.refresh_settings();
    }
}

/// Freeze every monitor on a worker thread, then show the overlays. The window list (for
/// snapping) is gathered in parallel.
fn freeze(lock: CandadoCaptura) {
    std::thread::spawn(move || {
        let cursor = crate::platform::window_style::cursor_position();
        let windows = std::thread::spawn(crate::capture::window_rects);
        let indices: Vec<usize> =
            (0..crate::capture::monitors().map(|m| m.len()).unwrap_or(0)).collect();
        let result = crate::capture::freeze_monitors(&indices);
        // Release the capture GPU device so no driver memory stays allocated between
        // captures (~35 MB); recreating it costs ~100 ms on the next capture.
        crate::capture::liberar();
        let windows = windows.join().unwrap_or_default();
        on_ui(move |a| {
            match result {
                Ok(freezes) => a.show_overlays(freezes, windows, cursor),
                Err(e) => error_dialog("Could not capture the screen", &e.to_string()),
            }
            drop(lock);
        });
    });
}

/// Show background results: errors in a dialog, nothing on success.
fn report(result: crate::error::Result<Option<PathBuf>>) {
    if let Err(e) = result {
        error_dialog("Screenshot failed", &e.to_string());
    }
}

fn error_dialog(title: &str, message: &str) {
    let (title, message) = (title.to_string(), message.to_string());
    std::thread::spawn(move || {
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title(title)
            .set_description(message)
            .show();
    });
}

fn wire_settings(w: &crate::SettingsWindow) {
    w.on_changed(|| with_app(|a| a.form_changed()));
    w.on_listen_shortcut(|which| with_app(|a| a.listen_shortcut(which)));
    w.on_toggle_check(|| with_app(|a| a.toggle_audio_check()));
    w.on_refresh_devices(|| with_app(|a| a.refresh_devices()));
    w.on_test_size_changed(|| {
        with_app(|a| {
            if let Some(c) = a.settings.borrow_mut().as_mut() {
                c.test_key.clear();
                c.window.set_test_result("".into());
                c.window.set_test_ok(false);
            }
        })
    });
    w.on_test_encoder(|| with_app(|a| a.test_encoder()));
    w.on_pick_folder(|which| {
        let folder = rfd::FileDialog::new()
            .set_title("Choose a folder")
            .pick_folder();
        if let Some(folder) = folder {
            with_app(|a| {
                let mut s = a.settings_snapshot();
                let path = folder.to_string_lossy().into_owned();
                if which == 0 {
                    s.save_directory = path;
                } else {
                    s.video_directory = path;
                }
                a.save_settings(s);
            })
        }
    });
    w.on_open_folder(|which| {
        with_app(|a| {
            let s = a.settings_snapshot();
            let dir = if which == 0 {
                s.save_directory
            } else {
                s.video_directory
            };
            if let Err(e) = crate::platform::open_folder(&PathBuf::from(dir)) {
                if let Some(c) = a.settings.borrow().as_ref() {
                    c.window.set_error(e.to_string().into());
                }
            }
        })
    });
    w.on_take_screenshot(|| with_app(|a| a.open_capture(OverlayIntent::Capture)));
    w.on_start_recording(|| with_app(|a| a.record_shortcut()));
    w.on_tab_changed(|_| {
        with_app(|a| {
            if let Some(c) = a.settings.borrow_mut().as_mut() {
                c.stop_audio_check();
            }
            a.refresh_settings();
        })
    });
    w.window().on_close_requested(|| {
        with_app(|a| {
            crate::shell::keycapture::stop();
            if a.listening.replace(0) != 0 {
                a.register_hotkeys();
            }
            if let Some(c) = a.settings.borrow_mut().as_mut() {
                c.stop_audio_check();
            }
        });
        // Free the window entirely once it is hidden; it is rebuilt when opened again.
        slint::Timer::single_shot(Duration::from_millis(200), || {
            with_app(|a| {
                if let Ok(mut s) = a.settings.try_borrow_mut() {
                    if s.as_ref().is_some_and(|c| !c.window.window().is_visible()) {
                        *s = None;
                    }
                }
            })
        });
        slint::CloseRequestResponse::HideWindow
    });
}
