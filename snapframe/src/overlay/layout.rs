//! Where the overlay toolbar and its buttons go, in monitor-local physical pixels. Pure, so
//! hit testing and drawing share one source of truth and it can be tested without a window.

use super::model::LRect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Photo,
    Video,
    /// Target frame rate choice, index into [`FPS_CHOICES`].
    Fps(u8),
    /// Quality level, index into `encode::auto::LEVELS`.
    Quality(u8),
    /// Take the whole monitor at once.
    Screen,
    Settings,
    Close,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Toolbar {
    pub bar: LRect,
    pub items: Vec<(Item, LRect)>,
    /// x of each separator line's centre.
    pub separators: Vec<i32>,
    /// Where the note about a fixed-encoder profile goes, when one is active.
    pub summary: Option<LRect>,
}

// Logical-pixel metrics, matching the previous design.
const TOP: f64 = 24.0;
const HEIGHT: f64 = 48.0;
const PAD: f64 = 6.0;
const BUTTON: f64 = 36.0;
const GAP: f64 = 2.0;
const SEPARATOR: f64 = 9.0;
const SEGMENT_H: f64 = 32.0;
const FPS_W: f64 = 54.0;
const QUALITY_W: f64 = 62.0;

/// Frame rates offered on the toolbar.
pub const FPS_CHOICES: [u32; 2] = [30, 60];
pub const QUALITY_LABELS: [&str; 3] = crate::encode::auto::LEVEL_LABELS;

/// Lay out the toolbar centred at the top. In video mode it has the frame rate and quality
/// choices; `summary_width` is the measured width of the fixed-profile note (0 when none).
pub fn toolbar(screen_width: i32, scale: f64, video: bool, summary_width: i32) -> Toolbar {
    let px = |v: f64| (v * scale).round() as i32;
    let mut items = Vec::new();
    let mut separators = Vec::new();
    let mut summary = None;
    let top = px(TOP);
    let height = px(HEIGHT);
    let button_y = top + px(PAD);
    let mut x = px(PAD);
    let button = |item: Item, x: &mut i32, items: &mut Vec<(Item, LRect)>| {
        items.push((item, LRect::new(*x, button_y, px(BUTTON), px(BUTTON))));
        *x += px(BUTTON) + px(GAP);
    };
    let separator = |x: &mut i32, separators: &mut Vec<i32>| {
        separators.push(*x + px(SEPARATOR) / 2 - px(GAP) / 2);
        *x += px(SEPARATOR);
    };
    button(Item::Photo, &mut x, &mut items);
    button(Item::Video, &mut x, &mut items);
    separator(&mut x, &mut separators);
    if video {
        let y = top + (height - px(SEGMENT_H)) / 2;
        let segments =
            |items: &mut Vec<(Item, LRect)>, x: &mut i32, n: u8, w: f64, f: fn(u8) -> Item| {
                *x += px(2.0);
                for i in 0..n {
                    items.push((f(i), LRect::new(*x, y, px(w), px(SEGMENT_H))));
                    *x += px(w);
                }
                *x += px(2.0);
            };
        segments(
            &mut items,
            &mut x,
            FPS_CHOICES.len() as u8,
            FPS_W,
            Item::Fps,
        );
        separator(&mut x, &mut separators);
        segments(
            &mut items,
            &mut x,
            QUALITY_LABELS.len() as u8,
            QUALITY_W,
            Item::Quality,
        );
        if summary_width > 0 {
            x += px(6.0);
            summary = Some(LRect::new(x, top, summary_width, height));
            x += summary_width + px(6.0);
        }
        separator(&mut x, &mut separators);
    }
    button(Item::Screen, &mut x, &mut items);
    separator(&mut x, &mut separators);
    button(Item::Settings, &mut x, &mut items);
    button(Item::Close, &mut x, &mut items);
    let width = x - px(GAP) + px(PAD);
    let left = (screen_width - width) / 2;
    // Everything was laid out from 0; shift to the centred position.
    for (_, r) in &mut items {
        r.x += left;
    }
    for s in &mut separators {
        *s += left;
    }
    if let Some(s) = &mut summary {
        s.x += left;
    }
    Toolbar {
        bar: LRect::new(left, top, width, height),
        items,
        separators,
        summary,
    }
}

impl Toolbar {
    pub fn item_at(&self, x: i32, y: i32) -> Option<Item> {
        self.items
            .iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(i, _)| *i)
    }

    #[cfg(test)]
    pub fn rect_of(&self, item: Item) -> Option<LRect> {
        self.items.iter().find(|(i, _)| *i == item).map(|(_, r)| *r)
    }
}

/// Where a resize handle is pressed: a square around each anchor, larger than the drawn dot so
/// it is easy to grab.
pub fn handle_rects(sel: LRect, scale: f64) -> [LRect; 8] {
    let size = (16.0 * scale).round() as i32;
    super::model::Handle::ALL.map(|h| {
        let (fx, fy) = h.anchor();
        let cx = sel.x + (f64::from(sel.w) * fx).round() as i32;
        let cy = sel.y + (f64::from(sel.h) * fy).round() as i32;
        LRect::new(cx - size / 2, cy - size / 2, size, size)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolbar_is_centred_and_ordered() {
        let t = toolbar(1920, 1.0, false, 0);
        let order: Vec<Item> = t.items.iter().map(|(i, _)| *i).collect();
        assert_eq!(
            order,
            [
                Item::Photo,
                Item::Video,
                Item::Screen,
                Item::Settings,
                Item::Close
            ]
        );
        assert!((t.bar.x * 2 + t.bar.w - 1920).abs() <= 1, "centred");
        assert_eq!(t.bar.y, 24);
        assert_eq!(t.separators.len(), 2);
        assert!(t.summary.is_none());
        // Buttons sit inside the bar and do not overlap.
        for w in t.items.windows(2) {
            assert!(w[0].1.right() <= w[1].1.x);
        }
        assert!(t.items.last().unwrap().1.right() <= t.bar.right());
    }

    #[test]
    fn video_mode_adds_frame_rate_and_quality_segments() {
        let t = toolbar(3072, 1.5, true, 0);
        let fps: Vec<_> = (0..2).map(|i| t.rect_of(Item::Fps(i)).unwrap()).collect();
        let quality: Vec<_> = (0..3)
            .map(|i| t.rect_of(Item::Quality(i)).unwrap())
            .collect();
        assert_eq!(fps[0].right(), fps[1].x, "segments touch");
        assert!(fps[1].right() < quality[0].x);
        assert!(quality[2].right() < t.rect_of(Item::Screen).unwrap().x);
        assert_eq!(t.separators.len(), 4);
        assert!(t.summary.is_none());
        let with_note = toolbar(3072, 1.5, true, 120);
        let note = with_note.summary.unwrap();
        assert!(note.x > with_note.rect_of(Item::Quality(2)).unwrap().right());
        assert!(note.right() < with_note.rect_of(Item::Screen).unwrap().x);
        assert!(toolbar(3072, 1.5, false, 0).rect_of(Item::Fps(0)).is_none());
    }

    #[test]
    fn hit_testing_finds_buttons_but_not_gaps() {
        let t = toolbar(1920, 1.0, false, 0);
        let close = t.rect_of(Item::Close).unwrap();
        assert_eq!(t.item_at(close.x + 5, close.y + 5), Some(Item::Close));
        assert_eq!(
            t.item_at(t.bar.x + 1, t.bar.y + 1),
            None,
            "bar padding is background"
        );
    }

    #[test]
    fn handles_surround_their_anchors() {
        let r = handle_rects(LRect::new(100, 100, 200, 100), 1.0);
        assert!(r[0].contains(100, 100));
        assert!(r[4].contains(300, 200));
        assert!(r[1].contains(200, 100));
    }
}
