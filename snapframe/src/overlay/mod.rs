//! The selection overlay: one native window per monitor (see `window.rs`), drawn on the CPU
//! (`paint.rs`), behaving as `model.rs` says. Windows, screenshots and buffers only exist while
//! the overlay is open; closing frees all of it.

pub mod layout;
pub mod model;
pub mod paint;
pub mod window;

use std::sync::Arc;
use std::time::Duration;

use crate::capture::{Freeze, MonitorInfo, Rect, WindowRect};
use crate::settings::Settings;
use crate::state::OverlayIntent;
use layout::{Item, Toolbar};
use model::{Action, CaptureMode, Key, LRect, Model, Origin, Shared};
use paint::{Scene, ToolbarScene};
use window::{Cursor, Input, OverlayWindow};

/// What the app must do after overlay input.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    None,
    /// A model action and, for captures and recordings, the virtual-desktop region.
    Model(Action, Option<Rect>),
    /// A toolbar frame rate (fps) or quality level (index into `encode::auto::LEVELS`).
    SetRecording {
        fps: Option<u32>,
        level: Option<usize>,
    },
    OpenSettings,
}

/// What the toolbar knows about the recording settings.
#[derive(Default, Clone)]
pub struct RecordingView {
    pub fps: u32,
    pub encoding: crate::settings::EncodingSettings,
    /// A note when a fixed encoder from settings.json is used instead of automatic.
    pub profile: Option<String>,
}

impl RecordingView {
    pub fn from_settings(s: &Settings) -> Self {
        let auto = crate::encode::auto::is_auto(&s.encoding);
        Self {
            fps: s.fps.clamp(5, 120),
            encoding: s.encoding.clone(),
            profile: (!auto).then(|| format!("Fixed: {}", s.encoding.encoder)),
        }
    }

    fn fps_index(&self) -> Option<u8> {
        layout::FPS_CHOICES
            .iter()
            .position(|f| *f == self.fps)
            .map(|i| i as u8)
    }

    fn quality_index(&self) -> Option<u8> {
        self.profile
            .is_none()
            .then(|| crate::encode::auto::level_index(&self.encoding.preset) as u8)
    }

    /// How a recording of this size would be encoded, e.g. "GPU · NVENC H.264 P5".
    pub fn plan(&self, width: i32, height: i32) -> String {
        use crate::encode::{auto, preflight};
        // Recordings round the region down to even dimensions.
        let (w, h) = ((width.max(2) as u32) & !1, (height.max(2) as u32) & !1);
        let resolved = auto::resolve_here(&self.encoding, w, h, self.fps);
        let mut text = auto::describe(&resolved);
        if preflight::recording(&resolved, w, h, self.fps).is_err() {
            text.push_str(" · too heavy for this profile");
        } else if let Some(caution) = auto::caution(&resolved, w, h, self.fps) {
            text.push_str(" · ");
            text.push_str(caution);
        }
        text
    }
}

struct Surface {
    monitor: MonitorInfo,
    window: OverlayWindow,
    model: Model,
    image: Arc<image::RgbaImage>,
    toolbar: Toolbar,
    hovered: Option<Item>,
    cursor: Cursor,
}

impl Surface {
    fn origin(&self) -> Origin {
        Origin {
            x: self.monitor.x,
            y: self.monitor.y,
            width: self.monitor.width,
            height: self.monitor.height,
        }
    }

    fn scale(&self) -> f64 {
        f64::from(self.monitor.scale).max(0.5)
    }

    fn color_at_cursor(&self) -> (String, [u8; 3]) {
        let (x, y) = self.model.cursor.unwrap_or_default();
        let x = (x.max(0) as u32).min(self.image.width().saturating_sub(1));
        let y = (y.max(0) as u32).min(self.image.height().saturating_sub(1));
        let p = self.image.get_pixel(x, y);
        (
            format!("#{:02x}{:02x}{:02x}", p[0], p[1], p[2]),
            [p[0], p[1], p[2]],
        )
    }
}

#[derive(Default)]
pub struct Overlays {
    surfaces: Vec<Surface>,
    pub shared: Shared,
    pub open: bool,
    recording: RecordingView,
    toast: Option<paint::Toast>,
    toast_timer: slint::Timer,
    /// Primary button down (presses can start on the toolbar).
    pressed: bool,
    /// A toolbar button pressed, acting on release over the same button.
    pressed_item: Option<(u32, Item)>,
}

impl Overlays {
    /// Create one window per frozen monitor, draw it completely, then show it.
    pub fn open(
        &mut self,
        freezes: &[Freeze],
        windows: &[WindowRect],
        cursor: Option<(i32, i32)>,
        settings: &Settings,
        intent: OverlayIntent,
    ) {
        self.surfaces.clear();
        self.shared = Shared {
            mode: if intent == OverlayIntent::Record {
                CaptureMode::Video
            } else {
                CaptureMode::Still
            },
            show_magnifier: settings.show_magnifier,
            pick_screen: false,
        };
        self.recording = RecordingView::from_settings(settings);
        self.toast = None;
        self.pressed = false;
        self.pressed_item = None;
        for f in freezes {
            let m = &f.monitor;
            let scale = f64::from(m.scale).max(0.5);
            let Some(window) =
                OverlayWindow::new(m.id, m.x, m.y, m.width as i32, m.height as i32, scale)
            else {
                eprintln!("could not create the overlay window for {}", m.label);
                continue;
            };
            let origin = Origin {
                x: m.x,
                y: m.y,
                width: m.width,
                height: m.height,
            };
            let mut model = Model::new(m.width as i32, m.height as i32, scale);
            model.windows = model::windows_on_screen(windows, origin);
            model.cursor = cursor
                .filter(|&(x, y)| m.contains(x, y))
                .map(|(x, y)| (x - m.x, y - m.y));
            self.surfaces.push(Surface {
                monitor: m.clone(),
                window,
                model,
                image: f.image.clone(),
                toolbar: Toolbar {
                    bar: LRect::default(),
                    items: Vec::new(),
                    separators: Vec::new(),
                    summary: None,
                },
                hovered: None,
                cursor: Cursor::Cross,
            });
        }
        self.open = !self.surfaces.is_empty();
        let focus = self
            .surfaces
            .iter()
            .position(|s| s.model.cursor.is_some())
            .unwrap_or(0);
        for i in 0..self.surfaces.len() {
            self.render(i);
            self.surfaces[i].window.show(i == focus);
        }
    }

    /// Hide and free everything: windows, screenshots, buffers.
    pub fn close(&mut self) {
        self.open = false;
        self.toast_timer.stop();
        for s in &self.surfaces {
            s.window.hide();
        }
        // Destroy after the current message is handled: closing usually runs inside one of
        // these windows' own window procedure.
        let surfaces = std::mem::take(&mut self.surfaces);
        let surfaces = std::cell::Cell::new(Some(surfaces));
        slint::Timer::single_shot(Duration::ZERO, move || drop(surfaces.take()));
    }

    fn index(&self, id: u32) -> Option<usize> {
        self.surfaces.iter().position(|s| s.monitor.id == id)
    }

    pub fn cursor(&self, id: u32) -> Cursor {
        self.index(id)
            .map(|i| self.surfaces[i].cursor)
            .unwrap_or_default()
    }

    /// Handle one input on monitor `id`.
    pub fn input(&mut self, id: u32, input: Input) -> Command {
        let Some(i) = self.index(id) else {
            return Command::None;
        };
        let command = match input {
            Input::Down(x, y) => self.down(i, x, y),
            Input::Move(x, y) => self.moved(i, x, y),
            Input::Up(x, y) => self.up(i, x, y),
            Input::RightDown => Command::Model(Action::Cancel, None),
            Input::Key(None, _) => {
                let action = self.surfaces[i].model.key(Key::Escape, false, &self.shared);
                self.command(i, action)
            }
            Input::Key(Some(c), ctrl) => {
                let shared = self.shared;
                let action = self.surfaces[i].model.key(Key::Char(c), ctrl, &shared);
                self.command(i, action)
            }
        };
        // The cursor left the other monitors.
        if matches!(input, Input::Move(..) | Input::Down(..)) {
            for j in 0..self.surfaces.len() {
                if j != i
                    && self.surfaces[j].model.cursor.is_some()
                    && !self.surfaces[j].model.is_dragging()
                {
                    self.surfaces[j].model.cursor = None;
                    self.surfaces[j].hovered = None;
                    self.render(j);
                }
            }
        }
        if self.open && self.index(id).is_some() {
            self.render(i);
        }
        command
    }

    fn command(&self, i: usize, action: Action) -> Command {
        let region = match &action {
            Action::Capture(r) | Action::Record(r) | Action::RecordWindow(r, _) => {
                Some(model::to_virtual(*r, self.surfaces[i].origin()))
            }
            _ => None,
        };
        Command::Model(action, region)
    }

    fn toolbar_visible(&self, i: usize) -> bool {
        let view = self.surfaces[i].model.view(&self.shared);
        view.show_bar && !view.bar_dimmed
    }

    fn down(&mut self, i: usize, x: i32, y: i32) -> Command {
        self.pressed = true;
        let id = self.surfaces[i].monitor.id;
        if self.toolbar_visible(i) && self.surfaces[i].toolbar.bar.contains(x, y) {
            if let Some(item) = self.surfaces[i].toolbar.item_at(x, y) {
                self.pressed_item = Some((id, item));
                return Command::None;
            }
            let action = self.surfaces[i].model.pointer_down(x, y, true);
            return self.command(i, action);
        }
        if self.shared.pick_screen {
            self.pressed = false;
            let action = self.surfaces[i].model.take_whole(&self.shared);
            return self.command(i, action);
        }
        let s = &mut self.surfaces[i];
        if let Some(sel) = s.model.selection() {
            if !s.model.busy {
                let handles = layout::handle_rects(sel, s.scale());
                if let Some(h) = handles.iter().position(|r| r.contains(x, y)) {
                    s.model.grab_handle(model::Handle::ALL[h]);
                    return Command::None;
                }
            }
        }
        let action = s.model.pointer_down(x, y, false);
        self.command(i, action)
    }

    fn moved(&mut self, i: usize, x: i32, y: i32) -> Command {
        let over_bar = self.toolbar_visible(i) && self.surfaces[i].toolbar.bar.contains(x, y);
        let pressed = self.pressed;
        let picking = self.shared.pick_screen;
        let s = &mut self.surfaces[i];
        s.hovered = if over_bar {
            s.toolbar.item_at(x, y)
        } else {
            None
        };
        let action = s
            .model
            .pointer_move(x, y, pressed && self.pressed_item.is_none());
        s.cursor = if picking && !over_bar {
            Cursor::Hand
        } else {
            cursor_for(s, over_bar, x, y)
        };
        self.command(i, action)
    }

    fn up(&mut self, i: usize, x: i32, y: i32) -> Command {
        self.pressed = false;
        let id = self.surfaces[i].monitor.id;
        if let Some((pid, item)) = self.pressed_item.take() {
            if pid == id && self.surfaces[i].toolbar.item_at(x, y) == Some(item) {
                return self.click(i, item);
            }
            return Command::None;
        }
        let shared = self.shared;
        let action = self.surfaces[i].model.pointer_up(&shared);
        self.command(i, action)
    }

    fn click(&mut self, i: usize, item: Item) -> Command {
        let mut next = self.shared;
        match item {
            Item::Photo => next.mode = CaptureMode::Still,
            Item::Video => next.mode = CaptureMode::Video,
            Item::Fps(n) => {
                return Command::SetRecording {
                    fps: Some(layout::FPS_CHOICES[usize::from(n)]),
                    level: None,
                }
            }
            Item::Quality(n) => {
                return Command::SetRecording {
                    fps: None,
                    level: Some(usize::from(n)),
                }
            }
            Item::Screen => {
                // One monitor: take it now. Several: let the next click choose one.
                if self.surfaces.len() == 1 {
                    let action = self.surfaces[i].model.take_whole(&self.shared);
                    return self.command(i, action);
                }
                next.pick_screen = !self.shared.pick_screen;
            }
            Item::Settings => return Command::OpenSettings,
            Item::Close => return Command::Model(Action::Cancel, None),
        }
        Command::Model(Action::SetShared(next), None)
    }

    pub fn set_shared(&mut self, next: Shared) {
        self.shared = next;
        self.render_all();
    }

    pub fn set_recording(&mut self, recording: RecordingView) {
        self.recording = recording;
        self.render_all();
    }

    pub fn set_busy(&mut self, busy: bool) {
        for s in &mut self.surfaces {
            s.model.busy = busy;
        }
    }

    pub fn color_at_cursor(&self, id: u32) -> Option<(String, [u8; 3])> {
        self.index(id).map(|i| self.surfaces[i].color_at_cursor())
    }

    /// A notice at the top of every overlay. Errors stay; others vanish after 1.6 s.
    pub fn toast(&mut self, text: &str, error: bool, swatch: Option<[u8; 3]>) {
        self.toast = Some(paint::Toast {
            text: text.into(),
            error,
            swatch,
        });
        self.render_all();
        if !error {
            self.toast_timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(1600),
                || {
                    crate::app::with_app(|a| a.overlay_clear_toast());
                },
            );
        }
    }

    pub fn clear_toast(&mut self) {
        self.toast = None;
        self.render_all();
    }

    fn render_all(&mut self) {
        for i in 0..self.surfaces.len() {
            self.render(i);
        }
    }

    /// Build the scene for monitor `i`, draw what changed and send it to the window.
    fn render(&mut self, i: usize) {
        let shared = self.shared;
        let recording = self.recording.clone();
        let toast = self.toast.clone();
        let s = &mut self.surfaces[i];
        let scale = s.scale();
        let view = s.model.view(&shared);
        let video = shared.mode == CaptureMode::Video;
        let note = recording.profile.clone().unwrap_or_default();
        let summary_w = if video && !note.is_empty() {
            s.window.canvas.measure(&note, 12.0, false, false).0
        } else {
            0
        };
        s.toolbar = layout::toolbar(s.model.width, scale, video, summary_w);
        let hovered = s
            .hovered
            .and_then(|h| s.toolbar.items.iter().position(|(it, _)| *it == h))
            .map(|p| p as u8);

        let mut scene = Scene {
            selection: view.selection,
            handles: view.selection.is_some() && !view.bar_dimmed,
            badge: view
                .selection
                .map(|r| {
                    if video {
                        format!("{} × {} · {}", r.w, r.h, recording.plan(r.w, r.h))
                    } else {
                        format!("{} × {}", r.w, r.h)
                    }
                })
                .unwrap_or_default(),
            hover: view.hovered.as_ref().map(|h| {
                let title = if video && h.hwnd != 0 {
                    format!("{} · {}", h.title, recording.plan(h.rect.w, h.rect.h))
                } else {
                    h.title.clone()
                };
                (h.rect, title)
            }),
            crosshair: view.magnifier.then_some(view.cursor),
            toast,
            ..Scene::default()
        };
        if view.magnifier {
            let (mx, my) = s.model.magnifier_position();
            let (sx, sy, span, cell) = s.model.magnifier_source();
            let (hex, color) = s.color_at_cursor();
            scene.magnifier = Some(paint::Magnifier {
                x: (mx * scale).round() as i32,
                y: (my * scale).round() as i32,
                src: (sx, sy, span),
                cell,
                coords: format!("{}, {}", view.cursor.0, view.cursor.1),
                hex,
                color,
            });
        }
        if view.show_picker {
            let (w, h) = (s.monitor.width, s.monitor.height);
            let action = if video { "record" } else { "capture" };
            let mut text = format!("Click to {action} screen {} · {w} × {h}", s.monitor.id + 1);
            if video {
                text.push_str(" · ");
                text.push_str(&recording.plan(w as i32, h as i32));
            }
            scene.picker = Some(text);
        }
        if view.show_bar {
            scene.toolbar = Some(ToolbarScene {
                layout_key: s.toolbar.bar.w as u64,
                video,
                hovered,
                picking: shared.pick_screen,
                fps: recording.fps_index(),
                quality: recording.quality_index(),
                note,
                dimmed: view.bar_dimmed,
            });
        }
        let toolbar = s.toolbar.clone();
        let dirty = s.window.canvas.render(&scene, &s.image, Some(&toolbar));
        if !dirty.is_empty() {
            s.window.present(&dirty);
        }
    }
}

fn cursor_for(s: &Surface, over_bar: bool, x: i32, y: i32) -> Cursor {
    if over_bar && s.toolbar.item_at(x, y).is_some() {
        return Cursor::Hand;
    }
    if over_bar {
        return Cursor::Arrow;
    }
    if s.model.is_dragging() {
        return Cursor::Cross;
    }
    if let Some(sel) = s.model.selection() {
        let handles = layout::handle_rects(sel, s.scale());
        if let Some(h) = handles.iter().position(|r| r.contains(x, y)) {
            return match model::Handle::ALL[h] {
                model::Handle::NW | model::Handle::SE => Cursor::SizeNwse,
                model::Handle::NE | model::Handle::SW => Cursor::SizeNesw,
                model::Handle::N | model::Handle::S => Cursor::SizeNs,
                model::Handle::E | model::Handle::W => Cursor::SizeWe,
            };
        }
        return Cursor::Arrow;
    }
    Cursor::Cross
}
