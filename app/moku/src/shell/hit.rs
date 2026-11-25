//! Where the input ("hit") window goes and what part of it answers the pointer. Pure (physical
//! pixels, the virtual desktop's coordinates) and unit-tested; `Shell::place_hit` only turns the
//! result into `SetWindowPos` / `SetWindowRgn`.
//!
//! The island floats [`crate::scene::TOP`] below the top edge of its monitor (dynamic style), but
//! the pointer stops *at* the edge: a flick up to the island ends on the top row, in the gap above
//! the pill. The input window therefore reaches up to the monitor's top edge over the width of
//! each shape (the "reach"), so a pointer pushed against the edge is on the island.

use super::monitors::IRect;
use crate::scene::HitShape;

/// One piece of the input window's region, in the window's own pixels (`r` / `b` exclusive).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Part {
    /// A rounded rectangle; `radius` is the corner radius.
    Round { l: i32, t: i32, r: i32, b: i32, radius: i32 },
    Ellipse { l: i32, t: i32, r: i32, b: i32 },
    Rect { l: i32, t: i32, r: i32, b: i32 },
}

/// The input window for one island shape.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// The window's rectangle on the virtual desktop.
    pub window: IRect,
    /// The island's own rectangle (shape + bubble, without the reach): its resting place.
    pub home: IRect,
    /// How far the window reaches above the island's shapes (px): the shapes' origin is this far
    /// below the window's top.
    pub reach: i32,
    /// The region, the main shape first.
    pub parts: Vec<Part>,
}

/// Lay out the input window. `origin` is the visual window's top-left, `mon_top` the top edge of
/// the island's monitor, `hs` the shapes relative to the visual window.
pub fn layout(origin: (i32, i32), mon_top: i32, hs: &HitShape) -> Layout {
    let (mx, my, mw, mh) = hs.main;
    let right = match hs.bubble {
        Some((bx, _, bw, _)) => bx + bw,
        None => mx + mw,
    };
    let height = hs.bubble.map(|(_, by, _, bh)| (by + bh).max(my + mh)).unwrap_or(my + mh) - my;
    let (x, y) = (origin.0 + mx, origin.1 + my);
    let reach = (y - mon_top).max(0);
    let home = IRect::new(x, y, x + (right - mx), y + height);
    let window = IRect::new(home.l, home.t - reach, home.r, home.b);

    // The main shape (the notch style's extra height hangs above the window and is clipped), then
    // the strips up to the monitor's edge over each shape's width.
    let mut parts = vec![Part::Round { l: 0, t: reach - hs.notch, r: mw + 1, b: reach + mh + 1, radius: hs.main_radius }];
    if reach > 0 {
        parts.push(Part::Rect { l: 0, t: 0, r: mw, b: reach });
    }
    if let Some((bx, by, bw, bh)) = hs.bubble {
        parts.push(Part::Ellipse { l: bx - mx, t: reach + by - my, r: bx - mx + bw + 1, b: reach + by - my + bh + 1 });
        if reach > 0 {
            parts.push(Part::Rect { l: bx - mx, t: 0, r: bx - mx + bw, b: reach + by - my });
        }
    }
    Layout { window, home, reach, parts }
}

/// A pointer position in the input window's client pixels as a position relative to the island's
/// shapes, in DIP (the origin of everything the hit tests use). Negative `y` = in the reach above.
pub fn shape_dip(client: (i32, i32), reach: i32, scale: f32) -> (f32, f32) {
    (client.0 as f32 / scale, (client.1 - reach) as f32 / scale)
}

#[cfg(test)]
impl Part {
    /// Is the pixel (x, y) in the part? (Corners follow the ellipse GDI draws.)
    fn contains(&self, x: i32, y: i32) -> bool {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        match *self {
            Part::Rect { l, t, r, b } => x >= l && x < r && y >= t && y < b,
            Part::Ellipse { l, t, r, b } => {
                let (cx, cy, a, c) = ((l + r) as f32 / 2.0, (t + b) as f32 / 2.0, (r - l) as f32 / 2.0, (b - t) as f32 / 2.0);
                ((px - cx) / a).powi(2) + ((py - cy) / c).powi(2) <= 1.0
            }
            Part::Round { l, t, r, b, radius } => {
                if x < l || x >= r || y < t || y >= b {
                    return false;
                }
                let rad = (radius as f32).min((r - l) as f32 / 2.0).min((b - t) as f32 / 2.0);
                // Distance into the corner square, if the pixel is in one.
                let dx = (l as f32 + rad - px).max(px - (r as f32 - rad)).max(0.0);
                let dy = (t as f32 + rad - py).max(py - (b as f32 - rad)).max(0.0);
                dx * dx + dy * dy <= rad * rad
            }
        }
    }
}

#[cfg(test)]
impl Layout {
    /// Does the island answer a pointer at (x, y) on the virtual desktop?
    fn hits(&self, x: i32, y: i32) -> bool {
        self.window.contains(x, y) && self.parts.iter().any(|p| p.contains(x - self.window.l, y - self.window.t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pill of an idle island at 100 % (the numbers of a real run): a 94 x 36 DIP pill, TOP = 8
    /// below the monitor's top, centred in the 784 DIP wide visual window.
    fn pill() -> HitShape {
        HitShape { main: (345, 8, 94, 36), main_radius: 18, notch: 0, bubble: None }
    }

    /// The visual window of a monitor at (0, 0), 1920 wide: centred.
    const ORIGIN: (i32, i32) = (568, 0);

    #[test]
    fn a_pointer_on_the_top_row_over_the_pill_is_on_the_island() {
        // The bug: the window started at the pill's top (y = 8), so a pointer flicked up against
        // the screen's edge (y = 0 .. 7) was over the app behind and the island never opened.
        let l = layout(ORIGIN, 0, &pill());
        let cx = ORIGIN.0 + 345 + 47;
        for y in 0..8 {
            assert!(l.hits(cx, y), "y = {y} over the pill's middle");
        }
        assert!(l.hits(cx, 8) && l.hits(cx, 26) && l.hits(cx, 43), "the pill itself still answers");
        assert!(!l.hits(cx, 44), "and it ends where it did");
        assert_eq!(l.window.t, 0, "the window starts at the monitor's top edge");
        assert_eq!(l.reach, 8);
    }

    #[test]
    fn the_reach_is_only_as_wide_as_the_pill() {
        // A maximised app's title bar and its buttons beside the pill stay clickable.
        let l = layout(ORIGIN, 0, &pill());
        let (left, right) = (ORIGIN.0 + 345, ORIGIN.0 + 345 + 94);
        for y in 0..8 {
            assert!(l.hits(left, y) && l.hits(right - 1, y), "the pill's own columns at y = {y}");
            assert!(!l.hits(left - 1, y) && !l.hits(right, y), "one pixel beside the pill at y = {y}");
            assert!(!l.hits(left - 200, y) && !l.hits(right + 200, y));
        }
        assert_eq!((l.window.l, l.window.r), (left, right));
        // Nothing above the monitor's edge is claimed (another monitor may sit there).
        assert!(!l.hits(left + 40, -1));
    }

    #[test]
    fn below_the_old_window_nothing_changed() {
        // Same shape, no reach (the monitor's top is where the pill's top is): the old window.
        let old = layout(ORIGIN, 8, &pill());
        assert_eq!(old.reach, 0);
        let new = layout(ORIGIN, 0, &pill());
        assert_eq!(new.home, old.home, "the island's resting rectangle is the pill, not the reach");
        for y in old.window.t..old.window.b {
            for x in old.window.l - 5..old.window.r + 5 {
                assert_eq!(new.hits(x, y), old.hits(x, y), "({x}, {y})");
            }
        }
    }

    #[test]
    fn the_round_corners_of_the_pill_stay_see_through() {
        let l = layout(ORIGIN, 0, &pill());
        let left = ORIGIN.0 + 345;
        assert!(!l.hits(left, 8), "the top-left corner of the pill's box is cut off by its radius");
        assert!(!l.hits(left, 43));
        assert!(l.hits(left + 18, 8));
        // The reach is a plain strip: its own corners are not rounded.
        assert!(l.hits(left, 7));
    }

    #[test]
    fn it_follows_the_scale_and_the_monitor() {
        // 125 %: TOP = 10 px, a 118 x 45 px pill.
        let hs = HitShape { main: (431, 10, 118, 45), main_radius: 23, notch: 0, bubble: None };
        let l = layout((470, 0), 0, &hs);
        assert_eq!((l.reach, l.window.t, l.window.l, l.window.r), (10, 0, 901, 1019));
        assert!(l.hits(960, 0) && l.hits(960, 9) && !l.hits(900, 0));
        // 75 %: TOP = 6 px.
        let hs = HitShape { main: (258, 6, 70, 27), main_radius: 14, notch: 0, bubble: None };
        assert_eq!(layout((0, 0), 0, &hs).reach, 6);
        // A second monitor to the right, taller at the top (its top is -200): the edge is its own.
        let l = layout((1920 + 568, -200), -200, &pill());
        assert_eq!((l.reach, l.window.t), (8, -200));
        assert!(l.hits(1920 + 568 + 345 + 10, -200) && !l.hits(1920 + 568 + 345 + 10, -201));
        // A monitor below another one: its top edge is y = 1080, and that is where the window starts
        // (the pointer crossing into the monitor above is not claimed).
        let l = layout((568, 1080), 1080, &pill());
        assert_eq!((l.reach, l.window.t), (8, 1080));
        assert!(l.hits(568 + 345 + 10, 1080) && !l.hits(568 + 345 + 10, 1079));
    }

    #[test]
    fn it_follows_the_anchor_and_the_offset() {
        // The visual window shifted 300 px to the right (anchor right / offset_x): the strip moves with it.
        let l = layout((ORIGIN.0 + 300, 0), 0, &pill());
        let left = ORIGIN.0 + 300 + 345;
        assert!(l.hits(left + 10, 0) && !l.hits(ORIGIN.0 + 345 + 10, 0));
    }

    #[test]
    fn the_notch_style_has_no_gap_and_no_reach() {
        // The notch hangs from the top edge (its shape starts at y = 0): nothing to reach.
        let hs = HitShape { main: (345, 0, 94, 36), main_radius: 28, notch: 20, bubble: None };
        let l = layout(ORIGIN, 0, &hs);
        assert_eq!(l.reach, 0);
        assert_eq!(l.window, l.home);
        assert_eq!(l.parts, vec![Part::Round { l: 0, t: -20, r: 95, b: 37, radius: 28 }]);
        assert!(l.hits(ORIGIN.0 + 345 + 47, 0));
    }

    #[test]
    fn the_split_bubble_is_reached_too() {
        // A 36 px bubble 6 px right of the pill; the same top as the pill.
        let hs = HitShape { main: (300, 8, 94, 36), main_radius: 18, notch: 0, bubble: Some((400, 8, 36, 36)) };
        let l = layout(ORIGIN, 0, &hs);
        let (pill_mid, bubble_mid, gap) = (ORIGIN.0 + 300 + 47, ORIGIN.0 + 400 + 18, ORIGIN.0 + 397);
        for y in 0..8 {
            assert!(l.hits(pill_mid, y) && l.hits(bubble_mid, y), "y = {y}");
            assert!(!l.hits(gap, y), "the gap between them is not claimed at y = {y}");
        }
        assert_eq!((l.window.l, l.window.r), (ORIGIN.0 + 300, ORIGIN.0 + 436));
        assert_eq!(l.home, IRect::new(ORIGIN.0 + 300, 8, ORIGIN.0 + 436, 44));
        // A bubble smaller than the pill sits lower: the reach above it ends at its own top.
        let hs = HitShape { main: (300, 8, 94, 40), main_radius: 20, notch: 0, bubble: Some((400, 10, 36, 36)) };
        let l = layout(ORIGIN, 0, &hs);
        assert!(l.hits(bubble_mid, 0) && l.hits(bubble_mid, 9) && l.hits(bubble_mid, 28));
        assert!(!l.hits(gap, 0));
    }

    #[test]
    fn an_open_panel_reaches_the_top_edge_across_its_width_and_only_that() {
        // 640 x 400 panel: the pointer pushed against the edge over it does not close it.
        let hs = HitShape { main: (72, 8, 640, 400), main_radius: 28, notch: 0, bubble: None };
        let l = layout(ORIGIN, 0, &hs);
        let left = ORIGIN.0 + 72;
        assert!(l.hits(left + 300, 0) && l.hits(left + 639, 3));
        assert!(!l.hits(left - 1, 0) && !l.hits(left + 640, 0));
        assert_eq!(l.window.h(), 408);
    }

    #[test]
    fn a_click_maps_to_the_same_shape_position_with_or_without_the_reach() {
        // 100 %, no reach: the old mapping.
        assert_eq!(shape_dip((47, 18), 0, 1.0), (47.0, 18.0));
        // With 8 px of reach the pill's middle is 8 px lower in the window, and the same for the shape.
        assert_eq!(shape_dip((47, 26), 8, 1.0), (47.0, 18.0));
        // At 125 %: 10 px of reach, 1.25 px per DIP.
        assert_eq!(shape_dip((125, 10 + 25), 10, 1.25), (100.0, 20.0));
        // In the reach itself the shape's y is negative (above the pill's box, so no button is hit).
        assert!(shape_dip((47, 0), 8, 1.0).1 < 0.0);
    }
}
