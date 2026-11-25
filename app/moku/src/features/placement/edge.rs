//! Hide until the top edge: the island stays out of sight until the pointer touches the top edge
//! of its monitor near it, or something new arrives (a notification, a timer, a new music
//! session), or the user opens the panel. It hides again once the pointer has been away for a
//! while. "Hide when idle" (`island.hide_when_idle`) is the same machine with a different `busy`:
//! anything in the pill keeps it up, and it hides [`IDLE_HOLD_MS`] after the last activity. Pure and
//! unit-tested; fed with the same pointer samples as move-aside.

use super::aside::Sample;
use crate::shell::monitors::IRect;

/// The pointer must rest on the top edge this long (ms): a flick across the top reveals nothing.
pub const DWELL_MS: u64 = 120;
/// A new arrival keeps the island up at least this long (ms).
pub const NEWS_HOLD_MS: u64 = 4000;
/// After the edge revealed it, it stays at least this long (ms).
pub const TOUCH_HOLD_MS: u64 = 1500;
/// After the panel closed / something urgent went, it stays at least this long (ms).
pub const BUSY_HOLD_MS: u64 = 1200;
/// In "hide when idle": after the last activity (a pill about to be replaced, a track change) it stays
/// at least this long (ms), so the island does not blink between two pills.
pub const IDLE_HOLD_MS: u64 = 2000;
/// Away from the island this long (ms) before it hides again.
pub const FAR_MS: u64 = 900;
/// The reveal zone along the top edge reaches this many DIP beyond the island's sides ...
pub const REACH: f32 = 100.0;
/// ... and is at least this wide (DIP), centred on the island.
pub const MIN_ZONE: f32 = 240.0;
/// "Near the island" for staying up: the island grown by this many DIP.
pub const NEAR: f32 = 60.0;

/// Everything one step needs.
#[derive(Clone, Copy, Debug)]
pub struct In {
    pub s: Sample,
    /// The island's resting rectangle (where it shows when it is up).
    pub home: IRect,
    /// Its monitor.
    pub mon: IRect,
    pub scale: f32,
    /// Something new arrived since the last step.
    pub news: bool,
    /// The panel / a sheet is open or something urgent is on: never hide.
    pub busy: bool,
    /// How long (ms) it stays up after `busy` ended: [`BUSY_HOLD_MS`], or [`IDLE_HOLD_MS`] in "hide when idle".
    pub busy_hold_ms: u64,
    /// Another monitor sits above this one at the island: the pointer leaves through the top edge
    /// at once, so the reveal band is [`ABOVE_BAND_PX`] thick instead of one pixel.
    pub above: bool,
}

/// The thickness of the top reveal band (physical px) when a monitor is above (at least).
pub const ABOVE_BAND_PX: i32 = 6;

/// Is another monitor above `mon`, over the reveal zone `zone` (x range)? A monitor counts when its
/// bottom edge is at or above `mon`'s top and it shares some of the zone's width.
pub fn monitor_above(all: &[IRect], mon: IRect, zone: (i32, i32)) -> bool {
    all.iter().any(|m| *m != mon && m.b <= mon.t && m.r > zone.0 && m.l < zone.1)
}

/// The top-edge reveal zone: the island's span, grown by [`REACH`], at least [`MIN_ZONE`] wide.
pub fn zone_x(home: IRect, scale: f32) -> (i32, i32) {
    let reach = (REACH * scale).round() as i32;
    let min_w = (MIN_ZONE * scale).round() as i32;
    let (l, r) = (home.l - reach, home.r + reach);
    if r - l >= min_w {
        (l, r)
    } else {
        let c = (home.l + home.r) / 2;
        (c - min_w / 2, c + min_w / 2)
    }
}

#[derive(Debug)]
pub struct Edge {
    shown: bool,
    top_since: Option<u64>,
    far_since: Option<u64>,
    hold_until: u64,
}

impl Default for Edge {
    /// Starts hidden: turning the option on (or starting with it on) hides the island at once.
    fn default() -> Self {
        Edge { shown: false, top_since: None, far_since: None, hold_until: 0 }
    }
}

impl Edge {
    pub fn shown(&self) -> bool {
        self.shown
    }

    /// The option is off: the island is simply shown.
    pub fn disable(&mut self) {
        *self = Edge { shown: true, ..Edge::default() };
    }

    pub fn step(&mut self, i: In) -> bool {
        let (s, t) = (i.s, i.s.t);
        if i.busy {
            self.shown = true;
            self.hold_until = self.hold_until.max(t + i.busy_hold_ms);
            self.top_since = None;
            self.far_since = None;
            return true;
        }
        if i.news {
            self.shown = true;
            self.hold_until = self.hold_until.max(t + NEWS_HOLD_MS);
        }
        let (zl, zr) = zone_x(i.home, i.scale);
        let edge_px = if i.above { (i.scale.round() as i32).max(ABOVE_BAND_PX) } else { (i.scale.round() as i32).max(1) };
        let in_top = s.x >= zl && s.x < zr && s.y >= i.mon.t - 2 && s.y <= i.mon.t + edge_px;
        if in_top {
            let since = *self.top_since.get_or_insert(t);
            if t.saturating_sub(since) >= DWELL_MS {
                self.shown = true;
                self.hold_until = self.hold_until.max(t + TOUCH_HOLD_MS);
            }
        } else {
            self.top_since = None;
        }
        if self.shown {
            let near = (NEAR * i.scale).round() as i32;
            if in_top || i.home.inflate(near, near).contains(s.x, s.y) {
                self.far_since = None;
            } else {
                self.far_since.get_or_insert(t);
            }
            if t >= self.hold_until && self.far_since.is_some_and(|f| t.saturating_sub(f) >= FAR_MS) {
                self.shown = false;
                self.far_since = None;
            }
        }
        self.shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MON: IRect = IRect::new(0, 0, 1920, 1080);
    const HOME: IRect = IRect::new(900, 8, 1020, 44);

    fn input(x: i32, y: i32, t: u64) -> In {
        In { s: Sample { x, y, t, down: false }, home: HOME, mon: MON, scale: 1.0, news: false, busy: false, busy_hold_ms: BUSY_HOLD_MS, above: false }
    }

    #[test]
    fn it_starts_hidden_and_a_far_pointer_changes_nothing() {
        let mut e = Edge::default();
        assert!(!e.shown());
        for t in (0..3000).step_by(100) {
            assert!(!e.step(input(300, 500, t)));
        }
    }

    #[test]
    fn resting_on_the_top_edge_near_the_island_reveals_it() {
        let mut e = Edge::default();
        assert!(!e.step(input(960, 0, 0)));
        assert!(!e.step(input(960, 0, 100)), "119 ms is a flick, not a touch");
        assert!(!e.step(input(960, 0, 119)));
        assert!(e.step(input(960, 0, 120)));
        // The zone reaches 100 DIP beyond the island's sides.
        let mut e = Edge::default();
        for t in (0..400).step_by(40) {
            e.step(input(820, 0, t));
        }
        assert!(e.shown(), "x = 820 is inside 900 - 100 ... 1020 + 100");
    }

    #[test]
    fn a_flick_across_the_top_or_a_touch_far_from_the_island_reveals_nothing() {
        let mut e = Edge::default();
        // Sweeping along the top edge: never 120 ms in the zone.
        for (i, x) in [700, 800, 900, 1000, 1100, 1200].iter().enumerate() {
            assert!(!e.step(input(*x, 0, i as u64 * 20)));
        }
        assert!(!e.step(input(1300, 0, 120)));
        // On the edge at the far left: outside the zone, however long.
        let mut e = Edge::default();
        for t in (0..1000).step_by(50) {
            assert!(!e.step(input(100, 0, t)));
        }
        // Close to the top, but not on it (taskbar-less browsers have their tabs there).
        let mut e = Edge::default();
        for t in (0..1000).step_by(50) {
            assert!(!e.step(input(960, 30, t)));
        }
    }

    #[test]
    fn after_a_reveal_it_hides_when_the_pointer_has_been_away_for_a_while() {
        let mut e = Edge::default();
        for t in (0..200).step_by(40) {
            e.step(input(960, 0, t));
        }
        assert!(e.shown());
        // The pointer drops to the page at t = 200. The last touch was at t = 160, so the 1500 ms
        // hold lasts until 1660; 900 ms away have long passed by then.
        assert!(e.step(input(960, 400, 200)));
        assert!(e.step(input(960, 400, 1000)));
        assert!(e.step(input(960, 400, 1659)), "hold not over");
        assert!(!e.step(input(960, 400, 1660)), "hold over and away for 1460 ms");
        // Hovering the island for a long time, then leaving: it waits 900 ms from the leaving.
        let mut e = Edge::default();
        for t in (0..200).step_by(40) {
            e.step(input(960, 0, t));
        }
        for t in (200..3000).step_by(100) {
            assert!(e.step(input(960, 30, t)));
        }
        assert!(e.step(input(960, 400, 3000)));
        assert!(e.step(input(960, 400, 3899)));
        assert!(!e.step(input(960, 400, 3900)));
    }

    #[test]
    fn a_pointer_on_the_revealed_island_keeps_it_up() {
        let mut e = Edge::default();
        for t in (0..200).step_by(40) {
            e.step(input(960, 0, t));
        }
        for t in (200..10_000).step_by(100) {
            assert!(e.step(input(960, 30, t)), "hovering just under the island: t = {t}");
        }
    }

    #[test]
    fn news_shows_it_for_a_while_without_the_pointer() {
        let mut e = Edge::default();
        let mut i = input(300, 500, 1000);
        i.news = true;
        assert!(e.step(i));
        assert!(e.step(input(300, 500, 3000)));
        assert!(e.step(input(300, 500, 4999)), "4 s hold");
        assert!(!e.step(input(300, 500, 5000)), "hold over, and the pointer was away all along");
        // A second arrival while it is up extends the hold.
        let mut e = Edge::default();
        let mut i = input(300, 500, 0);
        i.news = true;
        e.step(i);
        i.s.t = 3000;
        e.step(i);
        assert!(e.step(input(300, 500, 6999)));
        assert!(!e.step(input(300, 500, 7000)));
    }

    #[test]
    fn a_monitor_above_gets_a_wider_reveal_band() {
        let above_mon = IRect::new(0, -1080, 1920, 0);
        let side_mon = IRect::new(1920, 0, 3840, 1080);
        let zone = zone_x(HOME, 1.0);
        assert!(monitor_above(&[MON, above_mon], MON, zone));
        assert!(!monitor_above(&[MON, side_mon], MON, zone), "a neighbour at the side is not above");
        assert!(!monitor_above(&[MON], MON, zone), "itself is not above");
        // Above but far to the left of the reveal zone: the pointer does not cross there.
        assert!(!monitor_above(&[MON, IRect::new(-1920, -1080, -100, 0)], MON, zone));
        // Without a monitor above the band is one pixel: y = 4 does not touch it ...
        let mut e = Edge::default();
        for t in (0..400).step_by(40) {
            e.step(input(960, 4, t));
        }
        assert!(!e.shown());
        // ... with one above, 6 px are enough to dwell in.
        let mut e = Edge::default();
        for t in (0..400).step_by(40) {
            let mut i = input(960, 4, t);
            i.above = true;
            e.step(i);
        }
        assert!(e.shown());
        // At 200 % the one-pixel row is already 2 px; the band is still 6.
        let mut e = Edge::default();
        let home = IRect::new(1800, 16, 2040, 88);
        for t in (0..400).step_by(40) {
            let mut i = In { s: Sample { x: 1920, y: 5, t, down: false }, home, mon: IRect::new(0, 0, 3840, 2160), scale: 2.0, news: false, busy: false, busy_hold_ms: BUSY_HOLD_MS, above: true };
            i.above = true;
            e.step(i);
        }
        assert!(e.shown());
    }

    #[test]
    fn busy_never_hides_and_releases_after_a_short_hold() {
        let mut e = Edge::default();
        let mut i = input(300, 500, 0);
        i.busy = true;
        assert!(e.step(i));
        i.s.t = 60_000;
        assert!(e.step(i), "an open panel stays up however long");
        // Closed: the hold (1200 ms) and the 900 ms away.
        assert!(e.step(input(300, 500, 60_100)));
        assert!(e.step(input(300, 500, 61_000)));
        assert!(!e.step(input(300, 500, 61_300)));
    }

    #[test]
    fn disabling_shows_the_island_and_the_zone_scales() {
        let mut e = Edge::default();
        e.disable();
        assert!(e.shown());
        // 200 %: the edge row is 2 px, the zone 2x wider.
        let home = IRect::new(1800, 16, 2040, 88);
        let mut e = Edge::default();
        let i = |x, y, t| In { s: Sample { x, y, t, down: false }, home, mon: IRect::new(0, 0, 3840, 2160), scale: 2.0, news: false, busy: false, busy_hold_ms: BUSY_HOLD_MS, above: false };
        for t in (0..200).step_by(40) {
            e.step(i(1920, 2, t));
        }
        assert!(e.shown(), "y = 2 is the edge row at 200 %");
        assert_eq!(zone_x(home, 2.0), (1600, 2240));
        assert_eq!(zone_x(IRect::new(900, 8, 1020, 44), 1.0), (800, 1120));
        // A tiny island still gets a usable zone.
        assert_eq!(zone_x(IRect::new(950, 8, 970, 44), 1.0), (840, 1080));
    }

    #[test]
    fn idle_mode_keeps_it_up_for_two_seconds_after_the_last_activity() {
        let idle = |x, y, t| In { busy_hold_ms: IDLE_HOLD_MS, ..input(x, y, t) };
        let mut e = Edge::default();
        // Something is going on (music): shown at once, however far the pointer is.
        let mut i = idle(300, 500, 0);
        i.busy = true;
        assert!(e.step(i));
        i.s.t = 30_000;
        assert!(e.step(i), "stays up for as long as it lasts");
        // It ends at t = 30 000: the hold (2 s) decides, the pointer has been away all along.
        assert!(e.step(idle(300, 500, 30_100)));
        assert!(e.step(idle(300, 500, 31_000)));
        assert!(e.step(idle(300, 500, 31_999)), "2 s hold not over");
        assert!(!e.step(idle(300, 500, 32_000)), "idle for 2 s: hidden again");
        // And it stays hidden while idle.
        for t in (32_000..40_000).step_by(200) {
            assert!(!e.step(idle(300, 500, t)));
        }
    }

    #[test]
    fn idle_mode_does_not_flicker_when_one_pill_replaces_another() {
        let idle = |t, busy| In { busy, busy_hold_ms: IDLE_HOLD_MS, ..input(300, 500, t) };
        let mut e = Edge::default();
        assert!(e.step(idle(0, true)));
        // A track change / the next pill: idle for a moment (about 1.5 s), then busy again.
        for t in (100..1600).step_by(100) {
            assert!(e.step(idle(t, false)), "t = {t}");
        }
        assert!(e.step(idle(1700, true)));
        // The hold starts over from the last busy sample.
        for t in (1800..3700).step_by(100) {
            assert!(e.step(idle(t, false)), "t = {t}");
        }
        assert!(!e.step(idle(3700, false)));
    }

    #[test]
    fn idle_mode_waits_for_the_pointer_to_leave_and_reveals_like_the_edge_mode() {
        let idle = |x, y, t| In { busy_hold_ms: IDLE_HOLD_MS, ..input(x, y, t) };
        // Idle and hidden: resting on the top edge near the island reveals it (same dwell).
        let mut e = Edge::default();
        assert!(!e.step(idle(960, 0, 0)));
        assert!(!e.step(idle(960, 0, 119)));
        assert!(e.step(idle(960, 0, 120)));
        // Hovering it keeps it up; away from it, it hides after the hold and the away time.
        for t in (200..5000).step_by(100) {
            assert!(e.step(idle(960, 30, t)));
        }
        assert!(e.step(idle(960, 400, 5000)));
        assert!(e.step(idle(960, 400, 5899)));
        assert!(!e.step(idle(960, 400, 5900)));
        // An activity that ends while the pointer is on the island: it stays until the pointer leaves.
        let mut e = Edge::default();
        let mut i = idle(960, 30, 0);
        i.busy = true;
        assert!(e.step(i));
        for t in (100..10_000).step_by(100) {
            assert!(e.step(idle(960, 30, t)), "pointer near, t = {t}");
        }
        assert!(e.step(idle(960, 400, 10_000)));
        assert!(!e.step(idle(960, 400, 10_900)));
    }
}
