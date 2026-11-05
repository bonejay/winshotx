//! The selection overlay's behaviour, without any window: gestures, keys, window snapping and
//! magnifier placement. Coordinates are physical pixels local to one monitor (0,0 = its top
//! left). The Slint layer only converts to logical pixels and draws what [`View`] says.

use crate::capture::Rect;

/// Below this many logical pixels, a drag counts as a click.
const MIN_DRAG: f64 = 4.0;

/// Magnifier box and caption in logical pixels, plus the gap from the cursor.
pub const LUPA_SIDE: f64 = 160.0;
pub const LUPA_ZOOM: i32 = 4;
const LUPA_WIDTH: f64 = 162.0;
const LUPA_HEIGHT: f64 = 186.0;
const LUPA_GAP: f64 = 18.0;

/// A rectangle in monitor-local physical pixels. Signed width/height are never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl LRect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    /// The rectangle spanned by two corners, in any order.
    pub fn spanning(ax: i32, ay: i32, bx: i32, by: i32) -> Self {
        Self::new(ax.min(bx), ay.min(by), (bx - ax).abs(), (by - ay).abs())
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    /// Clipped to `0..w, 0..h`, or `None` if nothing remains.
    pub fn clip(&self, w: i32, h: i32) -> Option<Self> {
        let x = self.x.max(0);
        let y = self.y.max(0);
        let r = self.right().min(w);
        let b = self.bottom().min(h);
        (r > x && b > y).then(|| Self::new(x, y, r - x, b - y))
    }
}

/// Monitor origin, for converting between local and virtual-desktop coordinates. Monitors left
/// of or above the primary have negative origins; nothing here may clamp them to zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Origin {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Local → virtual desktop. At least 2×2 so encoders never get a zero size.
pub fn to_virtual(r: LRect, o: Origin) -> Rect {
    Rect {
        x: o.x + r.x,
        y: o.y + r.y,
        width: r.w.max(2) as u32,
        height: r.h.max(2) as u32,
    }
}

/// Virtual desktop → local.
pub fn to_local(r: Rect, o: Origin) -> LRect {
    LRect::new(r.x - o.x, r.y - o.y, r.width as i32, r.height as i32)
}

/// A system window that can be snapped to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapWindow {
    pub rect: LRect,
    pub title: String,
    /// HWND, so the window itself can be recorded; 0 if unknown.
    pub hwnd: isize,
}

/// Windows visible on this monitor in local coordinates, keeping Z order (front first). Tiny
/// windows (often invisible system ones) are skipped.
pub fn windows_on_screen(windows: &[crate::capture::WindowRect], o: Origin) -> Vec<SnapWindow> {
    windows
        .iter()
        .filter_map(|w| {
            let rect = to_local(w.rect, o);
            (rect.w > 8 && rect.h > 8 && rect.clip(o.width as i32, o.height as i32).is_some()).then(
                || SnapWindow {
                    rect,
                    title: w.title.clone(),
                    hwnd: w.hwnd,
                },
            )
        })
        .collect()
}

/// The front-most window under a point.
pub fn window_at(windows: &[SnapWindow], x: i32, y: i32) -> Option<&SnapWindow> {
    windows.iter().find(|w| w.rect.contains(x, y))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureMode {
    #[default]
    Still,
    Video,
}

/// What all monitors share: changing it on one screen changes it everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Shared {
    pub mode: CaptureMode,
    pub show_magnifier: bool,
    /// The screen button was pressed with several monitors: a click picks a whole screen.
    pub pick_screen: bool,
}

/// The eight resize handles, clockwise from the top left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    NW,
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
}

impl Handle {
    pub const ALL: [Handle; 8] = [
        Handle::NW,
        Handle::N,
        Handle::NE,
        Handle::E,
        Handle::SE,
        Handle::S,
        Handle::SW,
        Handle::W,
    ];
    /// Where the handle sits on the rectangle, as a fraction of width and height.
    pub fn anchor(self) -> (f64, f64) {
        match self {
            Handle::NW => (0.0, 0.0),
            Handle::N => (0.5, 0.0),
            Handle::NE => (1.0, 0.0),
            Handle::E => (1.0, 0.5),
            Handle::SE => (1.0, 1.0),
            Handle::S => (0.5, 1.0),
            Handle::SW => (0.0, 1.0),
            Handle::W => (0.0, 0.5),
        }
    }
    fn apply(self, base: LRect, x: i32, y: i32) -> LRect {
        let (mut l, mut t, mut r, mut b) = (base.x, base.y, base.right(), base.bottom());
        let (fx, fy) = self.anchor();
        if fx == 0.0 {
            l = x;
        }
        if fx == 1.0 {
            r = x;
        }
        if fy == 0.0 {
            t = y;
        }
        if fy == 1.0 {
            b = y;
        }
        LRect::spanning(l, t, r, b)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Gesture {
    Idle,
    Drawing {
        ox: i32,
        oy: i32,
        /// The window under the press: its rect and HWND.
        candidate: Option<(LRect, isize)>,
    },
    Moving {
        gx: i32,
        gy: i32,
        base: LRect,
    },
    Resizing {
        handle: Handle,
        base: LRect,
    },
}

/// Keys the overlay reacts to, already decoded from the platform event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Escape,
    Char(char),
}

/// What the controller must do after an input.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Nothing beyond redrawing this overlay.
    None,
    /// Screenshot this local region to the clipboard.
    Capture(LRect),
    /// Record this local region.
    Record(LRect),
    /// Record this window by handle; the rect is used for the toolbar position.
    RecordWindow(LRect, isize),
    CopyColor,
    /// Change shared state on every monitor.
    SetShared(Shared),
    Cancel,
}

/// One monitor's overlay.
#[derive(Debug, Clone)]
pub struct Model {
    /// Monitor size in physical pixels.
    pub width: i32,
    pub height: i32,
    /// Physical pixels per logical pixel.
    pub scale: f64,
    pub windows: Vec<SnapWindow>,
    selection: Option<LRect>,
    gesture: Gesture,
    /// The cursor, when it is on this monitor.
    pub cursor: Option<(i32, i32)>,
    /// A press that started on the toolbar's background: a click if released in place, a new
    /// selection if dragged.
    bar_press: Option<(i32, i32)>,
    /// While a capture or recording is being started, input is ignored.
    pub busy: bool,
}

impl Model {
    pub fn new(width: i32, height: i32, scale: f64) -> Self {
        Self {
            width,
            height,
            scale: scale.max(0.5),
            windows: Vec::new(),
            selection: None,
            gesture: Gesture::Idle,
            cursor: None,
            bar_press: None,
            busy: false,
        }
    }

    fn min_drag(&self) -> i32 {
        (MIN_DRAG * self.scale).round() as i32
    }

    /// The selection, once it is big enough to count.
    pub fn selection(&self) -> Option<LRect> {
        let min = self.min_drag();
        self.selection.filter(|s| s.w >= min && s.h >= min)
    }

    pub fn is_dragging(&self) -> bool {
        self.gesture != Gesture::Idle
    }

    fn whole_screen(&self) -> LRect {
        LRect::new(0, 0, self.width, self.height)
    }

    /// This whole monitor in the current mode (the toolbar's screen button).
    pub fn take_whole(&self, shared: &Shared) -> Action {
        self.take(self.whole_screen(), shared)
    }

    fn take(&self, region: LRect, shared: &Shared) -> Action {
        match shared.mode {
            CaptureMode::Still => Action::Capture(region),
            CaptureMode::Video => Action::Record(region),
        }
    }

    fn clamp_point(&self, x: i32, y: i32) -> (i32, i32) {
        (x.clamp(0, self.width), y.clamp(0, self.height))
    }

    /// The window highlighted under the cursor, when nothing else is going on.
    pub fn hovered(&self) -> Option<&SnapWindow> {
        if self.selection().is_some() || self.is_dragging() {
            return None;
        }
        let (x, y) = self.cursor?;
        window_at(&self.windows, x, y)
    }

    /// Mouse button pressed. `on_bar` is a press on the toolbar's background.
    pub fn pointer_down(&mut self, x: i32, y: i32, on_bar: bool) -> Action {
        if self.busy {
            return Action::None;
        }
        let (x, y) = self.clamp_point(x, y);
        self.cursor = Some((x, y));
        if on_bar {
            self.bar_press = Some((x, y));
            return Action::None;
        }
        if let Some(current) = self.selection() {
            if current.contains(x, y) {
                self.gesture = Gesture::Moving {
                    gx: x - current.x,
                    gy: y - current.y,
                    base: current,
                };
                return Action::None;
            }
        }
        self.gesture = Gesture::Drawing {
            ox: x,
            oy: y,
            candidate: window_at(&self.windows, x, y).map(|w| (w.rect, w.hwnd)),
        };
        self.selection = Some(LRect::new(x, y, 0, 0));
        Action::None
    }

    /// A resize handle of the current selection was pressed.
    pub fn grab_handle(&mut self, handle: Handle) {
        if let Some(base) = self.selection() {
            if !self.busy {
                self.gesture = Gesture::Resizing { handle, base };
            }
        }
    }

    /// Pointer moved; `pressed` is whether the primary button is down.
    pub fn pointer_move(&mut self, x: i32, y: i32, pressed: bool) -> Action {
        let (x, y) = self.clamp_point(x, y);
        self.cursor = Some((x, y));
        if let Some((bx, by)) = self.bar_press {
            if !pressed || self.busy {
                self.bar_press = None;
            } else if (x - bx).abs() >= self.min_drag() || (y - by).abs() >= self.min_drag() {
                // Dragging off the toolbar starts a custom selection, with no window snapping.
                self.bar_press = None;
                self.gesture = Gesture::Drawing {
                    ox: bx,
                    oy: by,
                    candidate: None,
                };
                self.selection = Some(LRect::spanning(bx, by, x, y));
            }
            return Action::None;
        }
        match self.gesture {
            Gesture::Idle => {}
            Gesture::Drawing { ox, oy, .. } => self.selection = Some(LRect::spanning(ox, oy, x, y)),
            Gesture::Moving { gx, gy, base } => {
                self.selection = Some(LRect::new(
                    (x - gx).clamp(0, (self.width - base.w).max(0)),
                    (y - gy).clamp(0, (self.height - base.h).max(0)),
                    base.w,
                    base.h,
                ))
            }
            Gesture::Resizing { handle, base } => self.selection = Some(handle.apply(base, x, y)),
        }
        Action::None
    }

    /// Mouse button released.
    pub fn pointer_up(&mut self, shared: &Shared) -> Action {
        self.bar_press = None;
        let gesture = std::mem::replace(&mut self.gesture, Gesture::Idle);
        let Gesture::Drawing { candidate, .. } = gesture else {
            return Action::None;
        };
        if let Some(region) = self.selection() {
            return self.take(region, shared);
        }
        // A plain click: the window underneath (recorded as a window in video mode, so it is
        // followed even when covered), or the whole screen on empty desktop.
        match candidate {
            Some((rect, hwnd)) => {
                self.selection = Some(rect);
                if shared.mode == CaptureMode::Video && hwnd != 0 {
                    Action::RecordWindow(rect, hwnd)
                } else {
                    self.take(rect, shared)
                }
            }
            None => {
                let screen = self.whole_screen();
                self.selection = Some(screen);
                self.take(screen, shared)
            }
        }
    }

    pub fn key(&mut self, key: Key, ctrl: bool, shared: &Shared) -> Action {
        let ch = match key {
            Key::Escape => return Action::Cancel,
            Key::Char(c) => c.to_ascii_lowercase(),
        };
        match ch {
            'a' if ctrl => self.take(self.whole_screen(), shared),
            'c' => Action::CopyColor,
            _ => Action::None,
        }
    }

    pub fn view(&self, shared: &Shared) -> View {
        let picking = shared.pick_screen;
        let selection = self.selection().filter(|_| !picking);
        let hovered = self.hovered().cloned().filter(|_| !picking);
        let here = self.cursor.is_some();
        let magnifier = here
            && !picking
            && shared.show_magnifier
            && (selection.is_none() || matches!(self.gesture, Gesture::Drawing { .. }));
        View {
            selection,
            hovered,
            magnifier,
            cursor: self.cursor.unwrap_or_default(),
            show_bar: here,
            bar_dimmed: self.is_dragging(),
            show_picker: picking,
        }
    }

    /// Where the magnifier box goes (logical pixels) so it never covers the inspected pixel:
    /// right of the cursor, else left; below, else above.
    pub fn magnifier_position(&self) -> (f64, f64) {
        let (cx, cy) = self.cursor.unwrap_or_default();
        let (x, y) = (f64::from(cx) / self.scale, f64::from(cy) / self.scale);
        let (w, h) = (
            f64::from(self.width) / self.scale,
            f64::from(self.height) / self.scale,
        );
        let left = if x + LUPA_GAP + LUPA_WIDTH <= w {
            x + LUPA_GAP
        } else {
            x - LUPA_GAP - LUPA_WIDTH
        };
        let top = if y + LUPA_GAP + LUPA_HEIGHT <= h {
            y + LUPA_GAP
        } else {
            y - LUPA_GAP - LUPA_HEIGHT
        };
        (
            left.clamp(0.0, (w - LUPA_WIDTH).max(0.0)),
            top.clamp(0.0, (h - LUPA_HEIGHT).max(0.0)),
        )
    }

    /// The source rectangle (physical pixels) the magnifier shows, kept inside the image so the
    /// outlined centre pixel stays under the cursor near edges. Returns (x, y, span) and the
    /// cursor's cell inside it.
    pub fn magnifier_source(&self) -> (i32, i32, i32, (i32, i32)) {
        let span = (LUPA_SIDE as i32) / LUPA_ZOOM;
        let half = span / 2;
        let (cx, cy) = self.cursor.unwrap_or_default();
        let cx = cx.clamp(0, (self.width - 1).max(0));
        let cy = cy.clamp(0, (self.height - 1).max(0));
        let sx = (cx - half).clamp(0, (self.width - span).max(0));
        let sy = (cy - half).clamp(0, (self.height - span).max(0));
        (sx, sy, span, (cx - sx, cy - sy))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub selection: Option<LRect>,
    pub hovered: Option<SnapWindow>,
    pub magnifier: bool,
    pub cursor: (i32, i32),
    pub show_bar: bool,
    pub bar_dimmed: bool,
    /// Draw "click to take this screen" on this monitor.
    pub show_picker: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> Shared {
        Shared {
            show_magnifier: true,
            ..Shared::default()
        }
    }

    fn model() -> Model {
        let mut m = Model::new(1920, 1080, 1.0);
        m.windows = vec![
            SnapWindow {
                rect: LRect::new(100, 100, 400, 300),
                title: "Front".into(),
                hwnd: 7,
            },
            SnapWindow {
                rect: LRect::new(0, 0, 1920, 1040),
                title: "Back".into(),
                hwnd: 8,
            },
        ];
        m
    }

    #[test]
    fn dragging_selects_and_release_captures_the_region() {
        let mut m = model();
        let s = shared();
        assert_eq!(m.pointer_down(600, 500, false), Action::None);
        m.pointer_move(400, 450, true);
        assert_eq!(m.selection(), Some(LRect::new(400, 450, 200, 50)));
        assert_eq!(
            m.pointer_up(&s),
            Action::Capture(LRect::new(400, 450, 200, 50))
        );
    }

    #[test]
    fn a_click_records_the_front_window_as_a_window_in_video_mode() {
        let mut m = model();
        let s = Shared {
            mode: CaptureMode::Video,
            ..shared()
        };
        m.pointer_down(150, 150, false);
        m.pointer_move(151, 151, true);
        assert_eq!(
            m.pointer_up(&s),
            Action::RecordWindow(LRect::new(100, 100, 400, 300), 7)
        );
        let mut m = model();
        m.pointer_down(150, 150, false);
        assert_eq!(
            m.pointer_up(&shared()),
            Action::Capture(LRect::new(100, 100, 400, 300)),
            "screenshots take the window's area"
        );
    }

    #[test]
    fn a_click_on_empty_desktop_takes_the_whole_screen() {
        let mut m = Model::new(1920, 1080, 1.0);
        let s = Shared {
            mode: CaptureMode::Video,
            ..shared()
        };
        m.pointer_down(10, 10, false);
        assert_eq!(m.pointer_up(&s), Action::Record(LRect::new(0, 0, 1920, 1080)));
    }

    #[test]
    fn small_drags_count_as_clicks_scaled_by_dpi() {
        let mut m = Model::new(3840, 2160, 2.0);
        m.pointer_down(10, 10, false);
        m.pointer_move(16, 16, true);
        assert_eq!(m.selection(), None, "6 px at 200% is 3 logical px");
        m.pointer_move(18, 18, true);
        assert!(m.selection().is_some());
    }

    #[test]
    fn moving_a_selection_stays_on_screen() {
        let mut m = Model::new(1000, 800, 1.0);
        let s = shared();
        m.pointer_down(100, 100, false);
        m.pointer_move(300, 300, true);
        m.pointer_up(&s);
        m.pointer_down(150, 150, false);
        m.pointer_move(5000, 5000, true);
        assert_eq!(m.selection(), Some(LRect::new(800, 600, 200, 200)));
        assert_eq!(m.pointer_up(&s), Action::None, "moving does not capture");
    }

    #[test]
    fn every_handle_resizes_its_edges_and_flips_cleanly() {
        let base = LRect::new(100, 100, 200, 100);
        assert_eq!(
            Handle::SE.apply(base, 400, 300),
            LRect::new(100, 100, 300, 200)
        );
        assert_eq!(Handle::NW.apply(base, 50, 50), LRect::new(50, 50, 250, 150));
        assert_eq!(
            Handle::N.apply(base, 999, 150),
            LRect::new(100, 150, 200, 50)
        );
        assert_eq!(
            Handle::E.apply(base, 50, 999),
            LRect::new(50, 100, 50, 100),
            "dragged past the left edge"
        );
        let mut m = Model::new(1000, 800, 1.0);
        let s = shared();
        m.pointer_down(100, 100, false);
        m.pointer_move(300, 200, true);
        m.pointer_up(&s);
        m.grab_handle(Handle::S);
        m.pointer_move(0, 400, true);
        assert_eq!(m.selection(), Some(LRect::new(100, 100, 200, 300)));
    }

    #[test]
    fn dragging_from_the_toolbar_starts_a_selection_but_a_click_does_not() {
        let mut m = model();
        let s = shared();
        m.pointer_down(900, 30, true);
        m.pointer_move(901, 31, true);
        assert_eq!(m.pointer_up(&s), Action::None);
        m.pointer_down(900, 30, true);
        m.pointer_move(1100, 200, true);
        assert_eq!(
            m.pointer_up(&s),
            Action::Capture(LRect::new(900, 30, 200, 170))
        );
    }

    #[test]
    fn keys_map_to_actions() {
        let mut m = model();
        let s = shared();
        assert_eq!(m.key(Key::Escape, false, &s), Action::Cancel);
        assert_eq!(m.key(Key::Char('c'), false, &s), Action::CopyColor);
        assert_eq!(
            m.key(Key::Char('A'), true, &s),
            Action::Capture(LRect::new(0, 0, 1920, 1080))
        );
        assert_eq!(m.key(Key::Char('a'), false, &s), Action::None);
        assert_eq!(
            m.take_whole(&Shared {
                mode: CaptureMode::Video,
                ..s
            }),
            Action::Record(LRect::new(0, 0, 1920, 1080))
        );
        for unused in ['v', 'f', 'r', 'w', 'p', '0', '3'] {
            assert_eq!(m.key(Key::Char(unused), false, &s), Action::None, "{unused}");
        }
    }

    #[test]
    fn hover_highlight_hides_while_selecting() {
        let mut m = model();
        let s = shared();
        m.pointer_move(150, 150, false);
        assert_eq!(m.view(&s).hovered.map(|w| w.title), Some("Front".into()));
        m.pointer_down(150, 150, false);
        m.pointer_move(300, 300, true);
        assert!(m.view(&s).hovered.is_none());
        assert!(m.view(&s).bar_dimmed);
        assert!(
            m.view(&s).magnifier,
            "the magnifier follows the cursor while drawing"
        );
    }

    #[test]
    fn busy_ignores_input() {
        let mut m = model();
        m.busy = true;
        m.pointer_down(10, 10, false);
        assert!(!m.is_dragging());
    }

    #[test]
    fn coordinates_round_trip_with_negative_origins() {
        let left = Origin {
            x: -2560,
            y: -200,
            width: 2560,
            height: 1440,
        };
        let r = Rect {
            x: -2000,
            y: 0,
            width: 500,
            height: 300,
        };
        let local = to_local(r, left);
        assert_eq!(local, LRect::new(560, 200, 500, 300));
        assert_eq!(to_virtual(local, left), r);
        assert_eq!(
            to_virtual(LRect::new(5, 5, 1, 0), left).width,
            2,
            "never smaller than 2×2"
        );
    }

    #[test]
    fn windows_are_filtered_to_the_screen_in_z_order() {
        let o = Origin {
            x: 1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let w = |x, title: &str| crate::capture::WindowRect {
            title: title.into(),
            rect: Rect {
                x,
                y: 10,
                width: 300,
                height: 200,
            },
            hwnd: 1,
        };
        let tiny = crate::capture::WindowRect {
            title: "tiny".into(),
            rect: Rect {
                x: 2000,
                y: 10,
                width: 4,
                height: 4,
            },
            hwnd: 2,
        };
        let on = windows_on_screen(
            &[
                w(100, "other screen"),
                w(2000, "here"),
                tiny,
                w(1800, "straddles"),
            ],
            o,
        );
        let titles: Vec<_> = on.iter().map(|w| w.title.as_str()).collect();
        assert_eq!(titles, ["here", "straddles"]);
        assert_eq!(on[1].rect.x, -120);
    }

    #[test]
    fn magnifier_avoids_the_cursor_and_edges() {
        let mut m = Model::new(1920, 1080, 1.0);
        m.cursor = Some((100, 100));
        assert_eq!(m.magnifier_position(), (118.0, 118.0));
        m.cursor = Some((1900, 1070));
        let (x, y) = m.magnifier_position();
        assert!(x + LUPA_WIDTH <= 1900.0 && y + LUPA_HEIGHT <= 1070.0);
        let mut hi = Model::new(3840, 2160, 2.0);
        hi.cursor = Some((200, 200));
        assert_eq!(
            hi.magnifier_position(),
            (118.0, 118.0),
            "logical pixels at 200%"
        );
    }

    #[test]
    fn magnifier_source_stays_inside_the_image() {
        let mut m = Model::new(1920, 1080, 1.0);
        m.cursor = Some((500, 400));
        assert_eq!(m.magnifier_source(), (480, 380, 40, (20, 20)));
        m.cursor = Some((2, 1079));
        assert_eq!(m.magnifier_source(), (0, 1040, 40, (2, 39)));
    }
}
