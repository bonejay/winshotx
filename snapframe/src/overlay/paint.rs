//! Draws one overlay into a BGRA back buffer (a GDI DIB section) on the CPU: the frozen
//! screenshot, dimmed outside the selection, then shapes and icons with tiny-skia and text with
//! GDI ClearType. Only rectangles whose content changed are redrawn.
//!
//! The buffer is BGRA but tiny-skia thinks in RGBA, so every colour passed to tiny-skia has red
//! and blue swapped (`c()` does that).

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use resvg::tiny_skia::{
    self, FillRule, Mask, Paint, PathBuilder, Pixmap, PixmapMut, PixmapPaint, Stroke, Transform,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, SIZE};
use windows::Win32::Graphics::Gdi::{
    CombineRgn, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateRectRgn, DeleteDC,
    DeleteObject, GetTextExtentPoint32W, SelectClipRgn, SelectObject, SetBkMode, SetTextColor,
    TextOutW, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, FW_NORMAL, FW_SEMIBOLD, HBITMAP, HDC, HFONT,
    HGDIOBJ, OUT_DEFAULT_PRECIS, RGN_OR, TRANSPARENT,
};

use super::layout::{Item, Toolbar};
use super::model::LRect;

/// Everything one overlay shows, in monitor-local physical pixels.
#[derive(Default)]
pub struct Scene {
    pub selection: Option<LRect>,
    pub handles: bool,
    pub badge: String,
    pub hover: Option<(LRect, String)>,
    pub crosshair: Option<(i32, i32)>,
    pub magnifier: Option<Magnifier>,
    pub toolbar: Option<ToolbarScene>,
    pub toast: Option<Toast>,
    /// "Click to record screen 2 · …" while choosing a whole screen.
    pub picker: Option<String>,
}

#[derive(Hash)]
pub struct Magnifier {
    /// Top-left of the box.
    pub x: i32,
    pub y: i32,
    /// Source square in the screenshot: x, y, side.
    pub src: (i32, i32, i32),
    /// Cursor cell inside the source square.
    pub cell: (i32, i32),
    pub coords: String,
    pub hex: String,
    pub color: [u8; 3],
}

#[derive(Hash)]
pub struct ToolbarScene {
    pub layout_key: u64,
    pub video: bool,
    pub hovered: Option<u8>,
    /// The screen button is waiting for a screen to be chosen.
    pub picking: bool,
    /// Selected frame rate and quality segment, if any.
    pub fps: Option<u8>,
    pub quality: Option<u8>,
    /// Name of an active fixed-encoder profile, shown after the quality choice.
    pub note: String,
    pub dimmed: bool,
}

#[derive(Hash, Clone, Copy)]
pub struct LRectH(pub i32, pub i32, pub i32, pub i32);

impl From<LRect> for LRectH {
    fn from(r: LRect) -> Self {
        Self(r.x, r.y, r.w, r.h)
    }
}

impl From<LRectH> for LRect {
    fn from(r: LRectH) -> Self {
        LRect::new(r.0, r.1, r.2, r.3)
    }
}

#[derive(Hash, Clone)]
pub struct Toast {
    pub text: String,
    pub error: bool,
    pub swatch: Option<[u8; 3]>,
}

/// tiny-skia colour for the BGRA buffer.
fn c(rgba: u32) -> tiny_skia::Color {
    let [r, g, b, a] = rgba.to_be_bytes();
    tiny_skia::Color::from_rgba8(b, g, r, a)
}

fn colorref(rgb: u32) -> COLORREF {
    let [_, r, g, b] = rgb.to_be_bytes();
    COLORREF(u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let k = r * 0.552_284_8;
    let mut p = PathBuilder::new();
    p.move_to(x + r, y);
    p.line_to(x + w - r, y);
    p.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    p.line_to(x + w, y + h - r);
    p.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    p.line_to(x + r, y + h);
    p.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    p.line_to(x, y + r);
    p.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    p.close();
    p.finish()
}

/// The icons the overlay uses.
fn icon_svg(name: &str) -> &'static str {
    match name {
        "camera" => include_str!("../../ui/icons/camera.svg"),
        "video" => include_str!("../../ui/icons/video.svg"),
        "rectangle" => include_str!("../../ui/icons/rectangle-horizontal.svg"),
        "window" => include_str!("../../ui/icons/app-window.svg"),
        "monitor" => include_str!("../../ui/icons/monitor.svg"),
        "settings" => include_str!("../../ui/icons/settings.svg"),
        "x" => include_str!("../../ui/icons/x.svg"),
        "chevron" => include_str!("../../ui/icons/chevron-down.svg"),
        "check" => include_str!("../../ui/icons/check.svg"),
        _ => "",
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Kind {
    Hover,
    Selection,
    Badge,
    CrossH,
    CrossV,
    Magnifier,
    Toast,
    Toolbar,
    Picker,
}

struct Element {
    kind: Kind,
    bounds: LRect,
    key: u64,
}

fn hash<T: Hash>(v: &T) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

pub struct Canvas {
    pub width: i32,
    pub height: i32,
    scale: f32,
    pub dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    mask: Mask,
    fonts: HashMap<(i32, bool, bool), HFONT>,
    icons: HashMap<(&'static str, u32, u32), Pixmap>,
    previous: Vec<Element>,
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            for font in self.fonts.values() {
                let _ = DeleteObject(HGDIOBJ(font.0));
            }
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

impl Canvas {
    pub fn new(width: i32, height: i32, scale: f64) -> Option<Self> {
        unsafe {
            let dc = CreateCompatibleDC(None);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap =
                CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            if bits.is_null() {
                let _ = DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, HGDIOBJ(bitmap.0));
            SetBkMode(dc, TRANSPARENT);
            Some(Self {
                width,
                height,
                scale: scale as f32,
                dc,
                bitmap,
                old,
                bits: bits.cast(),
                mask: Mask::new(width as u32, height as u32)?,
                fonts: HashMap::new(),
                icons: HashMap::new(),
                previous: Vec::new(),
            })
        }
    }

    fn px(&self, v: f32) -> f32 {
        (v * self.scale).round()
    }

    fn pixels(&mut self) -> &mut [u8] {
        unsafe {
            std::slice::from_raw_parts_mut(self.bits, (self.width * self.height * 4) as usize)
        }
    }

    fn font(&mut self, size: f32, bold: bool, mono: bool) -> HFONT {
        let px = (size * self.scale).round() as i32;
        *self.fonts.entry((px, bold, mono)).or_insert_with(|| {
            let face = wide(if mono {
                "Cascadia Mono\0"
            } else {
                "Segoe UI Variable Text\0"
            });
            unsafe {
                CreateFontW(
                    -px,
                    0,
                    0,
                    0,
                    if bold {
                        FW_SEMIBOLD.0 as i32
                    } else {
                        FW_NORMAL.0 as i32
                    },
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    CLEARTYPE_QUALITY,
                    DEFAULT_PITCH.0 as u32,
                    PCWSTR(face.as_ptr()),
                )
            }
        })
    }

    /// Width and height of text in physical pixels.
    pub fn measure(&mut self, text: &str, size: f32, bold: bool, mono: bool) -> (i32, i32) {
        let font = self.font(size, bold, mono);
        let w = wide(text);
        let mut s = SIZE::default();
        unsafe {
            SelectObject(self.dc, HGDIOBJ(font.0));
            let _ = GetTextExtentPoint32W(self.dc, &w, &mut s);
        }
        (s.cx, s.cy)
    }

    fn text(&mut self, x: i32, y: i32, text: &str, size: f32, bold: bool, mono: bool, rgb: u32) {
        let font = self.font(size, bold, mono);
        let w = wide(text);
        unsafe {
            SelectObject(self.dc, HGDIOBJ(font.0));
            SetTextColor(self.dc, colorref(rgb));
            let _ = TextOutW(self.dc, x, y, &w);
        }
    }

    /// Text vertically centred in a box of height `h` starting at `y`.
    fn text_mid(
        &mut self,
        x: i32,
        y: i32,
        h: i32,
        text: &str,
        size: f32,
        bold: bool,
        mono: bool,
        rgb: u32,
    ) {
        let (_, th) = self.measure(text, size, bold, mono);
        self.text(x, y + (h - th) / 2, text, size, bold, mono, rgb);
    }

    fn icon(&mut self, name: &'static str, size: u32, tint: u32) -> Option<&Pixmap> {
        let key = (name, size, tint);
        if !self.icons.contains_key(&key) {
            let tree =
                resvg::usvg::Tree::from_str(icon_svg(name), &resvg::usvg::Options::default())
                    .ok()?;
            let mut pm = Pixmap::new(size, size)?;
            let s = size as f32 / 24.0;
            resvg::render(&tree, Transform::from_scale(s, s), &mut pm.as_mut());
            // `tint` is 0xRRGGBB.
            let [_, r, g, b] = tint.to_be_bytes();
            for p in pm.data_mut().chunks_exact_mut(4) {
                let a = u32::from(p[3]);
                // Premultiplied, red/blue swapped for the BGRA buffer.
                p[0] = (u32::from(b) * a / 255) as u8;
                p[1] = (u32::from(g) * a / 255) as u8;
                p[2] = (u32::from(r) * a / 255) as u8;
            }
            self.icons.insert(key, pm);
        }
        self.icons.get(&key)
    }

    // ------------------------------------------------------------------ element bounds

    fn chip_size(&mut self, text: &str, size: f32, pad: f32) -> (i32, i32) {
        let (w, _) = self.measure(text, size, false, false);
        (w + self.px(pad * 2.0) as i32, self.px(26.0) as i32)
    }

    fn elements(&mut self, s: &Scene, toolbar: Option<&Toolbar>) -> Vec<Element> {
        let mut out = Vec::new();
        let grow = |r: LRect, by: i32| LRect::new(r.x - by, r.y - by, r.w + 2 * by, r.h + 2 * by);
        let chip_h = self.px(26.0) as i32;
        if let Some((r, title)) = &s.hover {
            let (w, _) = self.chip_size(title, 13.0, 8.0);
            let chip = self.hover_chip(*r, w, chip_h);
            let b = bounding(grow(*r, 2), chip);
            out.push(Element {
                kind: Kind::Hover,
                bounds: b,
                key: hash(&(r.x, r.y, r.w, r.h, title)),
            });
        }
        if let Some(r) = s.selection {
            out.push(Element {
                kind: Kind::Selection,
                bounds: grow(r, self.px(9.0) as i32),
                key: hash(&(r.x, r.y, r.w, r.h, s.handles)),
            });
            let (w, h) = self.measure(&s.badge, 13.0, false, true);
            let bw = w + self.px(16.0) as i32;
            let bh = h.max(self.px(24.0) as i32);
            let by = if r.y > self.px(30.0) as i32 {
                r.y - self.px(28.0) as i32
            } else {
                r.y + self.px(6.0) as i32
            };
            out.push(Element {
                kind: Kind::Badge,
                bounds: LRect::new(r.x + 2, by, bw, bh),
                key: hash(&(&s.badge, r.x, by)),
            });
        }
        if let Some((x, y)) = s.crosshair {
            out.push(Element {
                kind: Kind::CrossH,
                bounds: LRect::new(0, y, self.width, 1),
                key: hash(&y),
            });
            out.push(Element {
                kind: Kind::CrossV,
                bounds: LRect::new(x, 0, 1, self.height),
                key: hash(&x),
            });
        }
        if let Some(m) = &s.magnifier {
            let b = LRect::new(m.x, m.y, self.px(162.0) as i32, self.px(186.0) as i32);
            out.push(Element {
                kind: Kind::Magnifier,
                bounds: grow(b, self.px(2.0) as i32),
                key: hash(m),
            });
        }
        if let Some(text) = &s.picker {
            out.push(Element {
                kind: Kind::Picker,
                bounds: LRect::new(0, 0, self.width, self.height),
                key: hash(text),
            });
        }
        if let Some(t) = &s.toast {
            let b = self.toast_rect(t);
            out.push(Element {
                kind: Kind::Toast,
                bounds: b,
                key: hash(t),
            });
        }
        if let (Some(t), Some(layout)) = (&s.toolbar, toolbar) {
            if !t.dimmed {
                out.push(Element {
                    kind: Kind::Toolbar,
                    bounds: grow(layout.bar, 2),
                    key: hash(t),
                });
            }
        }
        out
    }

    fn hover_chip(&self, r: LRect, w: i32, h: i32) -> LRect {
        let above = r.y > self.px(34.0) as i32;
        LRect::new(
            if above {
                r.x
            } else {
                r.x + self.px(6.0) as i32
            },
            if above {
                r.y - self.px(30.0) as i32
            } else {
                r.y + self.px(6.0) as i32
            },
            w,
            h,
        )
    }

    fn toast_rect(&mut self, t: &Toast) -> LRect {
        let (w, _) = self.measure(&t.text, 13.0, false, false);
        let w =
            (w + self.px(if t.swatch.is_some() { 52.0 } else { 32.0 }) as i32).min(self.width - 40);
        let h = self.px(40.0) as i32;
        LRect::new((self.width - w) / 2, self.px(84.0) as i32, w, h)
    }

    // ------------------------------------------------------------------ rendering

    /// Redraw what changed since the last call (everything the first time). Returns the
    /// rectangles that must be sent to the window.
    pub fn render(
        &mut self,
        scene: &Scene,
        image: &image::RgbaImage,
        toolbar: Option<&Toolbar>,
    ) -> Vec<LRect> {
        let elements = self.elements(scene, toolbar);
        let full = self.previous.is_empty() && elements.is_empty() || self.previous.is_empty();
        let mut dirty: Vec<LRect> = Vec::new();
        if full {
            dirty.push(LRect::new(0, 0, self.width, self.height));
        } else {
            for e in &elements {
                match self.previous.iter().find(|p| p.kind == e.kind) {
                    Some(p) if p.bounds == e.bounds && p.key == e.key => {}
                    Some(p) => {
                        dirty.push(p.bounds);
                        dirty.push(e.bounds);
                    }
                    None => dirty.push(e.bounds),
                }
            }
            for p in &self.previous {
                if !elements.iter().any(|e| e.kind == p.kind) {
                    dirty.push(p.bounds);
                }
            }
        }
        // A changed selection changes the dimming inside the old and new rectangles.
        let dirty: Vec<LRect> = merge(
            dirty
                .into_iter()
                .filter_map(|r| r.clip(self.width, self.height))
                .collect(),
        );
        if dirty.is_empty() {
            self.previous = elements;
            return dirty;
        }
        self.paint_background(&dirty, image, scene.selection);
        self.set_clip(&dirty);
        for e in &elements {
            if dirty.iter().any(|d| intersects(*d, e.bounds)) {
                self.draw(e.kind, scene, image, toolbar);
            }
        }
        unsafe {
            SelectClipRgn(self.dc, None);
        }
        self.previous = elements;
        dirty
    }

    fn set_clip(&mut self, dirty: &[LRect]) {
        let mask = &mut self.mask;
        mask.data_mut().fill(0);
        let mut pb = PathBuilder::new();
        for r in dirty {
            if let Some(rect) =
                tiny_skia::Rect::from_xywh(r.x as f32, r.y as f32, r.w as f32, r.h as f32)
            {
                pb.push_rect(rect);
            }
        }
        if let Some(path) = pb.finish() {
            mask.fill_path(&path, FillRule::Winding, false, Transform::identity());
        }
        unsafe {
            let region = CreateRectRgn(0, 0, 0, 0);
            for r in dirty {
                let part = CreateRectRgn(r.x, r.y, r.right(), r.bottom());
                CombineRgn(Some(region), Some(region), Some(part), RGN_OR);
                let _ = DeleteObject(HGDIOBJ(part.0));
            }
            SelectClipRgn(self.dc, Some(region));
            let _ = DeleteObject(HGDIOBJ(region.0));
        }
    }

    /// Copy the screenshot into the dirty rectangles, dimmed outside the selection.
    fn paint_background(
        &mut self,
        dirty: &[LRect],
        image: &image::RgbaImage,
        selection: Option<LRect>,
    ) {
        let width = self.width as usize;
        let src = image.as_raw();
        let iw = image.width() as usize;
        let ih = image.height() as usize;
        let buf = self.pixels();
        for r in dirty {
            for y in r.y..r.bottom() {
                let yu = y as usize;
                let row = &mut buf
                    [(yu * width + r.x as usize) * 4..(yu * width + r.right() as usize) * 4];
                for (i, px) in row.chunks_exact_mut(4).enumerate() {
                    let x = r.x + i as i32;
                    let (sx, sy) = ((x as usize).min(iw - 1), yu.min(ih - 1));
                    let s = &src[(sy * iw + sx) * 4..(sy * iw + sx) * 4 + 4];
                    let lit = selection.is_some_and(|sel| sel.contains(x, y));
                    if lit {
                        px[0] = s[2];
                        px[1] = s[1];
                        px[2] = s[0];
                    } else {
                        // Black at 45% over the screenshot.
                        px[0] = (u16::from(s[2]) * 141 / 256) as u8;
                        px[1] = (u16::from(s[1]) * 141 / 256) as u8;
                        px[2] = (u16::from(s[0]) * 141 / 256) as u8;
                    }
                    px[3] = 255;
                }
            }
        }
    }

    fn with_pixmap(&mut self, f: impl FnOnce(&mut PixmapMut, &Mask)) {
        let (w, h) = (self.width as u32, self.height as u32);
        let bits = self.bits;
        let len = (self.width * self.height * 4) as usize;
        let data = unsafe { std::slice::from_raw_parts_mut(bits, len) };
        if let Some(mut pm) = PixmapMut::from_bytes(data, w, h) {
            f(&mut pm, &self.mask);
        }
    }

    fn fill_round(&mut self, r: LRect, radius: f32, color: u32) {
        let path = rounded(
            r.x as f32,
            r.y as f32,
            r.w as f32,
            r.h as f32,
            self.px(radius),
        );
        let mut paint = Paint::default();
        paint.set_color(c(color));
        paint.anti_alias = true;
        if let Some(path) = path {
            self.with_pixmap(|pm, mask| {
                pm.fill_path(
                    &path,
                    &paint,
                    FillRule::Winding,
                    Transform::identity(),
                    Some(mask),
                )
            });
        }
    }

    fn stroke_round(&mut self, r: LRect, radius: f32, width: f32, color: u32) {
        let half = width * self.scale / 2.0;
        let path = rounded(
            r.x as f32 + half,
            r.y as f32 + half,
            r.w as f32 - 2.0 * half,
            r.h as f32 - 2.0 * half,
            self.px(radius),
        );
        let mut paint = Paint::default();
        paint.set_color(c(color));
        paint.anti_alias = true;
        let stroke = Stroke {
            width: width * self.scale,
            ..Stroke::default()
        };
        if let Some(path) = path {
            self.with_pixmap(|pm, mask| {
                pm.stroke_path(&path, &paint, &stroke, Transform::identity(), Some(mask))
            });
        }
    }

    fn fill_rect(&mut self, r: LRect, color: u32) {
        let mut paint = Paint::default();
        paint.set_color(c(color));
        if let Some(rect) =
            tiny_skia::Rect::from_xywh(r.x as f32, r.y as f32, r.w as f32, r.h as f32)
        {
            self.with_pixmap(|pm, mask| {
                pm.fill_rect(rect, &paint, Transform::identity(), Some(mask))
            });
        }
    }

    fn circle(&mut self, cx: f32, cy: f32, radius: f32, fill: u32, border: u32, border_w: f32) {
        let mut pb = PathBuilder::new();
        pb.push_circle(cx, cy, radius);
        let Some(path) = pb.finish() else { return };
        let mut paint = Paint::default();
        paint.anti_alias = true;
        paint.set_color(c(fill));
        let mut edge = Paint::default();
        edge.anti_alias = true;
        edge.set_color(c(border));
        let stroke = Stroke {
            width: border_w,
            ..Stroke::default()
        };
        self.with_pixmap(|pm, mask| {
            pm.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                Some(mask),
            );
            pm.stroke_path(&path, &edge, &stroke, Transform::identity(), Some(mask));
        });
    }

    fn draw_icon(&mut self, name: &'static str, x: i32, y: i32, size: f32, tint: u32) {
        let px = self.px(size) as u32;
        let Some(icon) = self.icon(name, px, tint).cloned() else {
            return;
        };
        self.with_pixmap(|pm, mask| {
            pm.draw_pixmap(
                x,
                y,
                icon.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                Some(mask),
            );
        });
    }

    /// A dark floating chip like the toolbar's.
    fn chip(&mut self, r: LRect, radius: f32) {
        self.fill_round(r, radius, 0x171717eb);
        self.stroke_round(r, radius, 1.0, 0xffffff1a);
    }

    fn kbd(&mut self, x: i32, y: i32, h: i32, key: &str) -> i32 {
        let (tw, th) = self.measure(key, 12.0, true, false);
        let kh = th + self.px(4.0) as i32;
        let kw = (tw + self.px(10.0) as i32).max(kh);
        let ky = y + (h - kh) / 2;
        let r = LRect::new(x, ky, kw, kh);
        self.fill_round(r, 5.0, 0xffffff1a);
        self.stroke_round(r, 5.0, 1.0, 0xffffff26);
        self.text(
            x + (kw - tw) / 2,
            ky + (kh - th) / 2,
            key,
            12.0,
            true,
            false,
            0xf5f5f5,
        );
        kw
    }

    fn draw(&mut self, kind: Kind, s: &Scene, image: &image::RgbaImage, toolbar: Option<&Toolbar>) {
        let scale = self.scale;
        let p = move |v: f32| (v * scale).round() as i32;
        match kind {
            Kind::Hover => {
                let Some((r, title)) = &s.hover else {
                    return;
                };
                self.fill_rect(*r, 0xffffff0f);
                self.stroke_round(*r, 3.0, 1.0, 0xffffff73);
                if title.is_empty() {
                    return;
                }
                let (tw, _) = self.measure(title, 13.0, false, false);
                let chip = self.hover_chip(*r, tw + p(16.0), p(26.0));
                self.chip(chip, 7.0);
                self.text_mid(
                    chip.x + p(8.0),
                    chip.y,
                    chip.h,
                    title,
                    13.0,
                    false,
                    false,
                    0xe5e5e5,
                );
            }
            Kind::Selection => {
                let Some(r) = s.selection else { return };
                self.stroke_round(
                    LRect::new(r.x - 1, r.y - 1, r.w + 2, r.h + 2),
                    0.0,
                    1.0,
                    0x3b82f6e6,
                );
                if s.handles {
                    let radius = 6.0 * self.scale;
                    for h in super::model::Handle::ALL {
                        let (fx, fy) = h.anchor();
                        let cx = r.x as f32 + r.w as f32 * fx as f32;
                        let cy = r.y as f32 + r.h as f32 * fy as f32;
                        self.circle(
                            cx,
                            cy,
                            radius - 0.75 * self.scale,
                            0xffffffff,
                            0x3b82f6ff,
                            1.5 * self.scale,
                        );
                    }
                }
            }
            Kind::Badge => {
                let Some(r) = s.selection else { return };
                let (w, h) = self.measure(&s.badge, 13.0, false, true);
                let bh = h.max(p(24.0));
                let by = if r.y > p(30.0) {
                    r.y - p(28.0)
                } else {
                    r.y + p(6.0)
                };
                let b = LRect::new(r.x + 2, by, w + p(16.0), bh);
                self.chip(b, 6.0);
                let badge = s.badge.clone();
                self.text_mid(b.x + p(8.0), b.y, b.h, &badge, 13.0, false, true, 0xffffff);
            }
            Kind::CrossH => {
                if let Some((_, y)) = s.crosshair {
                    self.fill_rect(LRect::new(0, y, self.width, 1), 0xffffff40);
                }
            }
            Kind::CrossV => {
                if let Some((x, _)) = s.crosshair {
                    self.fill_rect(LRect::new(x, 0, 1, self.height), 0xffffff40);
                }
            }
            Kind::Magnifier => {
                let Some(m) = &s.magnifier else { return };
                let b = LRect::new(m.x, m.y, p(162.0), p(186.0));
                self.fill_round(b, 9.0, 0x171717f2);
                // The zoomed pixels, nearest-neighbour from the original screenshot.
                let side = p(160.0);
                let (sx, sy, span) = m.src;
                let (ix, iy) = (b.x + p(1.0), b.y + p(1.0));
                let (w, h) = (self.width, self.height);
                let (iw, ih) = (image.width() as i32, image.height() as i32);
                let src = image.as_raw();
                let mask_w = self.mask.width() as i32;
                let len = (self.width * self.height * 4) as usize;
                // The buffer and the clip mask are separate memory; borrow both at once.
                let buf = unsafe { std::slice::from_raw_parts_mut(self.bits, len) };
                let mask = self.mask.data();
                for ty in 0..side {
                    let y = iy + ty;
                    if y < 0 || y >= h {
                        continue;
                    }
                    let py = (sy + ty * span / side).clamp(0, ih - 1);
                    for tx in 0..side {
                        let x = ix + tx;
                        if x < 0 || x >= w || mask[(y * mask_w + x) as usize] == 0 {
                            continue;
                        }
                        let px = (sx + tx * span / side).clamp(0, iw - 1);
                        let si = ((py * iw + px) * 4) as usize;
                        let di = ((y * w + x) * 4) as usize;
                        buf[di] = src[si + 2];
                        buf[di + 1] = src[si + 1];
                        buf[di + 2] = src[si];
                        buf[di + 3] = 255;
                    }
                }
                let cell = side / span;
                let (cx, cy) = (ix + m.cell.0 * side / span, iy + m.cell.1 * side / span);
                self.stroke_round(
                    LRect::new(cx - p(1.0), cy - p(1.0), cell + p(2.0), cell + p(2.0)),
                    0.0,
                    2.0,
                    0x3b82f6ff,
                );
                self.fill_rect(LRect::new(b.x, b.y + p(161.0), b.w, 1), 0xffffff1a);
                self.stroke_round(b, 9.0, 1.0, 0xffffff26);
                let cap_y = b.y + p(162.0);
                let cap_h = p(24.0);
                let coords = m.coords.clone();
                self.text_mid(
                    b.x + p(7.0),
                    cap_y,
                    cap_h,
                    &coords,
                    11.0,
                    false,
                    true,
                    0xd4d4d4,
                );
                let hex = m.hex.clone();
                let (hw, _) = self.measure(&hex, 11.0, false, true);
                let kbd_w = self.measure("C", 12.0, true, false).0 + p(10.0);
                let kbd_w = kbd_w.max(p(18.0));
                let hex_x = b.right() - p(7.0) - kbd_w - p(6.0) - hw;
                self.text_mid(hex_x, cap_y, cap_h, &hex, 11.0, false, true, 0xd4d4d4);
                let sw = LRect::new(
                    hex_x - p(16.0),
                    cap_y + (cap_h - p(10.0)) / 2,
                    p(10.0),
                    p(10.0),
                );
                let [r, g, bl] = m.color;
                self.fill_round(sw, 3.0, u32::from_be_bytes([r, g, bl, 255]));
                self.stroke_round(sw, 3.0, 1.0, 0xffffff33);
                self.kbd(b.right() - p(7.0) - kbd_w, cap_y, cap_h, "C");
            }
            Kind::Picker => {
                let Some(text) = s.picker.clone() else { return };
                let inset = LRect::new(p(3.0), p(3.0), self.width - p(6.0), self.height - p(6.0));
                self.stroke_round(inset, 8.0, 3.0, 0x3b82f6d9);
                let (tw, _) = self.measure(&text, 15.0, false, false);
                let w = tw + p(32.0);
                let chip = LRect::new((self.width - w) / 2, self.height / 2 - p(19.0), w, p(38.0));
                self.fill_round(chip, 19.0, 0x171717eb);
                self.stroke_round(chip, 19.0, 1.0, 0xffffff1a);
                self.text_mid(chip.x + p(16.0), chip.y, chip.h, &text, 15.0, false, false, 0xe5e5e5);
            }
            Kind::Toast => {
                let Some(t) = s.toast.clone() else { return };
                let r = self.toast_rect(&t);
                if t.error {
                    self.fill_round(r, 12.0, 0x450a0ae6);
                    self.stroke_round(r, 12.0, 1.0, 0xef44444d);
                } else {
                    self.chip(r, 12.0);
                }
                let mut x = r.x + p(16.0);
                if let Some([cr, cg, cb]) = t.swatch {
                    let sw = LRect::new(x, r.y + (r.h - p(12.0)) / 2, p(12.0), p(12.0));
                    self.fill_round(sw, 4.0, u32::from_be_bytes([cr, cg, cb, 255]));
                    self.stroke_round(sw, 4.0, 1.0, 0xffffff40);
                    x += p(20.0);
                }
                self.text_mid(
                    x,
                    r.y,
                    r.h,
                    &t.text,
                    13.0,
                    false,
                    false,
                    if t.error { 0xfecaca } else { 0xe5e5e5 },
                );
            }
            Kind::Toolbar => {
                let (Some(t), Some(l)) = (&s.toolbar, toolbar) else {
                    return;
                };
                self.fill_round(l.bar, 16.0, 0x171717eb);
                self.stroke_round(l.bar, 16.0, 1.0, 0xffffff1a);
                for &x in &l.separators {
                    self.fill_rect(
                        LRect::new(x, l.bar.y + (l.bar.h - p(22.0)) / 2, 1.max(p(1.0)), p(22.0)),
                        0xffffff1a,
                    );
                }
                for (i, (item, r)) in l.items.iter().enumerate() {
                    let hovered = t.hovered == Some(i as u8);
                    let active = match item {
                        Item::Photo => !t.video,
                        Item::Video => t.video,
                        Item::Screen => t.picking,
                        _ => false,
                    };
                    let segment = match item {
                        Item::Fps(n) => Some((
                            format!("{} fps", super::layout::FPS_CHOICES[usize::from(*n)]),
                            t.fps == Some(*n),
                        )),
                        Item::Quality(n) => Some((
                            super::layout::QUALITY_LABELS[usize::from(*n)].to_string(),
                            t.quality == Some(*n),
                        )),
                        _ => None,
                    };
                    if let Some((label, selected)) = segment {
                        if selected {
                            self.fill_round(*r, 9.0, 0xffffff26);
                        } else if hovered {
                            self.fill_round(*r, 9.0, 0xffffff12);
                        }
                        let (tw, _) = self.measure(&label, 12.5, selected, false);
                        let color = if selected { 0xffffff } else { 0xa3a3a3 };
                        self.text_mid(
                            r.x + (r.w - tw) / 2,
                            r.y,
                            r.h,
                            &label,
                            12.5,
                            selected,
                            false,
                            color,
                        );
                        continue;
                    }
                    if active {
                        self.fill_round(*r, 11.0, 0xffffff26);
                    } else if hovered {
                        self.fill_round(
                            *r,
                            11.0,
                            if *item == Item::Close {
                                0xef444433
                            } else {
                                0xffffff1a
                            },
                        );
                    }
                    let (name, tint) = match item {
                        Item::Photo => ("camera", 0),
                        Item::Video => ("video", 0),
                        Item::Screen => ("monitor", 0),
                        Item::Settings => ("settings", 0),
                        Item::Close => ("x", 1),
                        Item::Fps(_) | Item::Quality(_) => unreachable!(),
                    };
                    let color = if tint == 1 && hovered {
                        0xfca5a5
                    } else if active {
                        0xffffff
                    } else {
                        0xd4d4d4
                    };
                    let icon = p(19.0);
                    self.draw_icon(
                        name,
                        r.x + (r.w - icon) / 2,
                        r.y + (r.h - icon) / 2,
                        19.0,
                        color,
                    );
                }
                if let Some(sr) = l.summary {
                    let note = t.note.clone();
                    self.text_mid(sr.x, sr.y, sr.h, &note, 12.0, false, false, 0xa3a3a3);
                }
            }
        }
    }
}

fn intersects(a: LRect, b: LRect) -> bool {
    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
}

fn bounding(a: LRect, b: LRect) -> LRect {
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    LRect::new(
        x,
        y,
        a.right().max(b.right()) - x,
        a.bottom().max(b.bottom()) - y,
    )
}

/// Merge overlapping rectangles so nothing is painted twice per frame.
pub fn merge(mut rects: Vec<LRect>) -> Vec<LRect> {
    let mut changed = true;
    while changed {
        changed = false;
        'outer: for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                if intersects(rects[i], rects[j]) {
                    rects[i] = bounding(rects[i], rects[j]);
                    rects.swap_remove(j);
                    changed = true;
                    break 'outer;
                }
            }
        }
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_dirty_rects_merge() {
        let m = merge(vec![
            LRect::new(0, 0, 10, 10),
            LRect::new(5, 5, 10, 10),
            LRect::new(100, 100, 5, 5),
        ]);
        assert_eq!(m.len(), 2);
        assert!(m.contains(&LRect::new(0, 0, 15, 15)));
    }

    #[test]
    fn colours_are_swapped_for_bgra() {
        let col = c(0x3b82f6ff);
        assert_eq!((col.red() * 255.0).round() as u8, 0xf6);
        assert_eq!((col.blue() * 255.0).round() as u8, 0x3b);
        assert_eq!(colorref(0x112233).0, 0x332211);
    }

    /// Renders a sample overlay to `target/overlay-preview.png` for a visual check, without
    /// showing any window.
    #[test]
    #[ignore = "Writes a preview image"]
    fn render_preview() {
        let (w, h) = (1600i32, 900i32);
        let mut img = image::RgbaImage::new(w as u32, h as u32);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgba([
                (40 + x * 120 / w as u32) as u8,
                (60 + y * 100 / h as u32) as u8,
                150,
                255,
            ]);
        }
        let mut canvas = Canvas::new(w, h, 1.0).unwrap();
        let toolbar = super::super::layout::toolbar(w, 1.0, true, 0);
        let scene = Scene {
            picker: Some("Click to record screen 2 · 2560 × 1440 · GPU · NVENC H.264 P5".into()),
            selection: Some(LRect::new(500, 300, 420, 260)),
            handles: true,
            badge: "420 × 260 · GPU · NVENC H.264 P5".into(),
            crosshair: Some((1100, 650)),
            magnifier: Some(Magnifier {
                x: 1118,
                y: 668,
                src: (1080, 630, 40),
                cell: (20, 20),
                coords: "1100, 650".into(),
                hex: "#6ea096".into(),
                color: [110, 160, 150],
            }),
            hover: Some((
                LRect::new(60, 520, 300, 200),
                "Visual Studio Code · GPU · NVENC H.264 P5".into(),
            )),
            toolbar: Some(ToolbarScene {
                layout_key: 1,
                video: true,
                hovered: Some(1),
                picking: false,
                fps: Some(1),
                quality: Some(1),
                note: String::new(),
                dimmed: false,
            }),
            toast: Some(Toast {
                text: "#6EA096 copied".into(),
                error: false,
                swatch: Some([110, 160, 150]),
            }),
        };
        canvas.render(&scene, &img, Some(&toolbar));
        let mut out = image::RgbaImage::new(w as u32, h as u32);
        let px = canvas.pixels().to_vec();
        for (i, p) in out.pixels_mut().enumerate() {
            *p = image::Rgba([px[i * 4 + 2], px[i * 4 + 1], px[i * 4], 255]);
        }
        out.save(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/target/overlay-preview.png"
        ))
        .unwrap();
    }

    #[test]
    fn every_overlay_icon_rasterises() {
        for name in [
            "camera",
            "video",
            "rectangle",
            "window",
            "monitor",
            "settings",
            "x",
            "chevron",
            "check",
        ] {
            let tree =
                resvg::usvg::Tree::from_str(icon_svg(name), &resvg::usvg::Options::default())
                    .unwrap();
            let mut pm = Pixmap::new(24, 24).unwrap();
            resvg::render(&tree, Transform::identity(), &mut pm.as_mut());
            assert!(
                pm.data().chunks_exact(4).any(|p| p[3] > 0),
                "{name} is empty"
            );
        }
    }
}
