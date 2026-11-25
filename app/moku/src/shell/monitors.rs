//! Which monitor the island lives on, and where along its top edge.
//!
//! Everything but [`enumerate`] / [`cursor_pos`] is pure (physical pixels, the virtual desktop's
//! coordinates) and unit-tested: choosing a monitor from `island.monitor`, the 300 ms debounce
//! of "follow the cursor", and the clamp of `island.anchor` + `island.offset_x`.

use moku_core::config::{Anchor, MonitorChoice};
use windows::Win32::Foundation::{LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
use windows::core::BOOL;

use crate::scene::{PANEL_W, WIN_H, WIN_W};

/// The pointer must stay on another monitor this long before the island follows it.
pub const FOLLOW_DEBOUNCE_MS: u64 = 300;
/// The island opens from its resting centre: it keeps half a panel plus this many DIP from the
/// sides of its monitor, so an open panel always fits.
pub const EDGE_MARGIN: f32 = 12.0;

/// A rectangle in physical pixels, `r` / `b` exclusive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IRect {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

impl IRect {
    pub const fn new(l: i32, t: i32, r: i32, b: i32) -> IRect {
        IRect { l, t, r, b }
    }

    pub fn from_rect(r: RECT) -> IRect {
        IRect { l: r.left, t: r.top, r: r.right, b: r.bottom }
    }

    pub fn to_rect(self) -> RECT {
        RECT { left: self.l, top: self.t, right: self.r, bottom: self.b }
    }

    pub fn w(&self) -> i32 {
        self.r - self.l
    }

    pub fn h(&self) -> i32 {
        self.b - self.t
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }

    pub fn inflate(&self, dx: i32, dy: i32) -> IRect {
        IRect { l: self.l - dx, t: self.t - dy, r: self.r + dx, b: self.b + dy }
    }

    pub fn center(&self) -> (f32, f32) {
        ((self.l + self.r) as f32 / 2.0, (self.t + self.b) as f32 / 2.0)
    }

    /// Distance from (x, y) to the nearest point of the rectangle (0 inside).
    pub fn dist(&self, x: f32, y: f32) -> f32 {
        let dx = (self.l as f32 - x).max(0.0).max(x - self.r as f32);
        let dy = (self.t as f32 - y).max(0.0).max(y - self.b as f32);
        (dx * dx + dy * dy).sqrt()
    }
}

/// One connected monitor.
#[derive(Clone, Debug, PartialEq)]
pub struct Mon {
    /// Device name, `\\.\DISPLAY2`.
    pub name: String,
    /// Monitor rectangle on the virtual desktop (physical px).
    pub rect: IRect,
    /// Effective DPI / 96.
    pub dpi: f32,
    pub primary: bool,
}

/// The monitor containing (x, y), else the nearest one.
pub fn monitor_at(mons: &[Mon], x: i32, y: i32) -> Option<usize> {
    if let Some(i) = mons.iter().position(|m| m.rect.contains(x, y)) {
        return Some(i);
    }
    let d = |m: &Mon| m.rect.dist(x as f32, y as f32);
    (0..mons.len()).min_by(|&a, &b| d(&mons[a]).total_cmp(&d(&mons[b])))
}

fn primary_index(mons: &[Mon]) -> Option<usize> {
    mons.iter().position(|m| m.primary).or(if mons.is_empty() { None } else { Some(0) })
}

/// Index of the monitor the island belongs on. `follow` is the monitor it currently follows
/// ([`Follow::settled`]); `cursor` the pointer, used while there is none yet.
pub fn choose(choice: &MonitorChoice, mons: &[Mon], cursor: Option<(i32, i32)>, follow: Option<&str>) -> Option<usize> {
    match choice {
        MonitorChoice::Primary | MonitorChoice::All => primary_index(mons),
        MonitorChoice::Named(n) => mons.iter().position(|m| m.name.eq_ignore_ascii_case(n)).or_else(|| primary_index(mons)),
        MonitorChoice::Cursor => follow
            .and_then(|f| mons.iter().position(|m| m.name.eq_ignore_ascii_case(f)))
            .or_else(|| cursor.and_then(|(x, y)| monitor_at(mons, x, y)))
            .or_else(|| primary_index(mons)),
    }
}

/// "Follow the cursor" with a debounce: the island moves to another monitor only after the
/// pointer has stayed on it for [`FOLLOW_DEBOUNCE_MS`] without a break, so crossing a monitor
/// on the way somewhere else moves nothing.
#[derive(Clone, Debug, Default)]
pub struct Follow {
    settled: Option<String>,
    candidate: Option<(String, u64)>,
}

impl Follow {
    pub fn new(settled: Option<&str>) -> Follow {
        Follow { settled: settled.map(str::to_string), candidate: None }
    }

    /// The monitor the island follows now.
    pub fn settled(&self) -> Option<&str> {
        self.settled.as_deref()
    }

    pub fn settle(&mut self, name: &str) {
        self.settled = Some(name.to_string());
        self.candidate = None;
    }

    /// Drop the monitor the pointer was waiting on (the clock the times came from changed).
    pub fn forget_candidate(&mut self) {
        self.candidate = None;
    }

    /// Feed the monitor the pointer is on at `now_ms`. `Some(name)` = move the island there now.
    pub fn observe(&mut self, now_ms: u64, under_cursor: &str) -> Option<String> {
        if self.settled.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(under_cursor)) {
            self.candidate = None;
            return None;
        }
        match &self.candidate {
            Some((c, since)) if c.eq_ignore_ascii_case(under_cursor) => {
                if now_ms.saturating_sub(*since) >= FOLLOW_DEBOUNCE_MS {
                    self.settled = Some(under_cursor.to_string());
                    self.candidate = None;
                    return self.settled.clone();
                }
            }
            _ => self.candidate = Some((under_cursor.to_string(), now_ms)),
        }
        None
    }
}

/// Horizontal shift (physical px) of the island's resting centre from the centre of a monitor
/// `mon_w` px wide at `scale` px per DIP: `anchor` (left / centre / right, as far as the island
/// may go) plus `offset_dip`, clamped so that an open panel (+ [`EDGE_MARGIN`]) stays on the
/// monitor. A monitor too narrow for a panel keeps the island centred.
pub fn shift_px(mon_w: i32, scale: f32, anchor: Anchor, offset_dip: i32) -> i32 {
    let lim = (mon_w as f32 / 2.0 - (PANEL_W / 2.0 + EDGE_MARGIN) * scale).max(0.0);
    let base = match anchor {
        Anchor::Left => -lim,
        Anchor::Center => 0.0,
        Anchor::Right => lim,
    };
    (base + offset_dip as f32 * scale).clamp(-lim, lim).round() as i32
}

/// The visual window's rectangle: the island's resting centre is the monitor's centre + `shift`,
/// the window's top is the monitor's top.
pub fn visual_rect(mon: IRect, scale: f32, shift: i32) -> IRect {
    let (w, h) = ((WIN_W * scale).round() as i32, (WIN_H * scale).round() as i32);
    let x = mon.l + (mon.w() - w) / 2 + shift;
    IRect { l: x, t: mon.t, r: x + w, b: mon.t + h }
}

// ---- Win32 -------------------------------------------------------------------------------------

/// The connected monitors, primary first.
pub fn enumerate() -> Vec<Mon> {
    unsafe extern "system" fn each(h: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
        let out = unsafe { &mut *(lp.0 as *mut Vec<Mon>) };
        let mut info = MONITORINFOEXW { monitorInfo: MONITORINFO { cbSize: size_of::<MONITORINFOEXW>() as u32, ..Default::default() }, ..Default::default() };
        if unsafe { GetMonitorInfoW(h, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) }.as_bool() {
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = unsafe { GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
            let n = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
            out.push(Mon {
                name: String::from_utf16_lossy(&info.szDevice[..n]),
                rect: IRect::from_rect(info.monitorInfo.rcMonitor),
                dpi: dx.max(48) as f32 / 96.0,
                primary: info.monitorInfo.dwFlags & 1 != 0,
            });
        }
        BOOL(1)
    }
    let mut v: Vec<Mon> = vec![];
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut v as *mut Vec<Mon> as isize));
    }
    v.sort_by_key(|m| !m.primary);
    v
}

/// The pointer, in virtual-desktop pixels.
pub fn cursor_pos() -> Option<(i32, i32)> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p) }.ok().map(|_| (p.x, p.y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(name: &str, l: i32, t: i32, w: i32, h: i32, primary: bool) -> Mon {
        Mon { name: name.into(), rect: IRect::new(l, t, l + w, t + h), dpi: 1.0, primary }
    }

    /// DISPLAY1 primary 1920x1080 at the origin, DISPLAY2 to its right, DISPLAY3 to its left
    /// (negative coordinates, as Windows lays them out).
    fn desk() -> Vec<Mon> {
        vec![mon(r"\\.\DISPLAY1", 0, 0, 1920, 1080, true), mon(r"\\.\DISPLAY2", 1920, 0, 2560, 1440, false), mon(r"\\.\DISPLAY3", -1280, -120, 1280, 1024, false)]
    }

    #[test]
    fn primary_and_all_pick_the_primary_monitor() {
        let m = desk();
        assert_eq!(choose(&MonitorChoice::Primary, &m, Some((3000, 10)), None), Some(0));
        assert_eq!(choose(&MonitorChoice::All, &m, None, None), Some(0), "'all' is not implemented: primary");
        // Not first in the list: the flag decides.
        let m2 = vec![m[1].clone(), m[0].clone()];
        assert_eq!(choose(&MonitorChoice::Primary, &m2, None, None), Some(1));
        // No primary flag at all: the first one. No monitors: nothing.
        let none = vec![mon("a", 0, 0, 10, 10, false)];
        assert_eq!(choose(&MonitorChoice::Primary, &none, None, None), Some(0));
        assert_eq!(choose(&MonitorChoice::Primary, &[], None, None), None);
    }

    #[test]
    fn a_named_monitor_is_found_by_device_name_or_falls_back_to_primary() {
        let m = desk();
        assert_eq!(choose(&MonitorChoice::Named(r"\\.\DISPLAY2".into()), &m, None, None), Some(1));
        assert_eq!(choose(&MonitorChoice::Named(r"\\.\display3".into()), &m, None, None), Some(2), "case-insensitive");
        assert_eq!(choose(&MonitorChoice::Named(r"\\.\DISPLAY9".into()), &m, None, None), Some(0), "unplugged: primary");
    }

    #[test]
    fn follow_cursor_uses_the_settled_monitor_then_the_pointer_then_primary() {
        let m = desk();
        // The island is on DISPLAY2 already: the pointer being elsewhere does not move it by itself.
        assert_eq!(choose(&MonitorChoice::Cursor, &m, Some((10, 10)), Some(r"\\.\DISPLAY2")), Some(1));
        // Nothing settled yet: where the pointer is (also on a monitor with negative coordinates).
        assert_eq!(choose(&MonitorChoice::Cursor, &m, Some((-100, 5)), None), Some(2));
        assert_eq!(choose(&MonitorChoice::Cursor, &m, Some((2000, 5)), None), Some(1));
        // The settled monitor was unplugged: the pointer's; no pointer: primary.
        assert_eq!(choose(&MonitorChoice::Cursor, &m, Some((2000, 5)), Some("gone")), Some(1));
        assert_eq!(choose(&MonitorChoice::Cursor, &m, None, Some("gone")), Some(0));
    }

    #[test]
    fn monitor_at_picks_the_containing_or_nearest_monitor() {
        let m = desk();
        assert_eq!(monitor_at(&m, 5, 5), Some(0));
        assert_eq!(monitor_at(&m, 1919, 1079), Some(0));
        assert_eq!(monitor_at(&m, 1920, 0), Some(1));
        assert_eq!(monitor_at(&m, -1, -1), Some(2));
        // In the gap below DISPLAY1 / left of DISPLAY2's bottom: nearest.
        assert_eq!(monitor_at(&m, 100, 1500), Some(0));
        assert_eq!(monitor_at(&m, 4000, 1600), Some(1));
        assert_eq!(monitor_at(&[], 0, 0), None);
    }

    #[test]
    fn follow_cursor_waits_300_ms_on_the_new_monitor() {
        let mut f = Follow::new(Some("A"));
        // Staying on A never moves anything.
        for t in (0..2000).step_by(100) {
            assert_eq!(f.observe(t, "A"), None);
        }
        // To B: nothing until it has been there 300 ms.
        assert_eq!(f.observe(2000, "B"), None);
        assert_eq!(f.observe(2100, "B"), None);
        assert_eq!(f.observe(2299, "B"), None);
        assert_eq!(f.observe(2300, "B"), Some("B".into()));
        assert_eq!(f.settled(), Some("B"));
        // Settled: more of B changes nothing.
        assert_eq!(f.observe(2400, "B"), None);
    }

    #[test]
    fn crossing_a_monitor_on_the_way_elsewhere_moves_nothing() {
        let mut f = Follow::new(Some("A"));
        // A -> B for 200 ms -> back to A: the timer is gone; B again starts over.
        assert_eq!(f.observe(0, "B"), None);
        assert_eq!(f.observe(200, "B"), None);
        assert_eq!(f.observe(250, "A"), None);
        assert_eq!(f.observe(300, "B"), None);
        assert_eq!(f.observe(500, "B"), None, "only 200 ms since the new visit began");
        assert_eq!(f.observe(600, "B"), Some("B".into()));
        // Through C to somewhere else: each change restarts the debounce.
        let mut f = Follow::new(Some("A"));
        assert_eq!(f.observe(0, "B"), None);
        assert_eq!(f.observe(250, "C"), None);
        assert_eq!(f.observe(400, "C"), None);
        assert_eq!(f.observe(550, "C"), Some("C".into()));
    }

    #[test]
    fn forgetting_the_candidate_restarts_the_debounce() {
        let mut f = Follow::new(Some("A"));
        assert_eq!(f.observe(5000, "B"), None);
        f.forget_candidate();
        // A new clock (times start again near zero): the wait starts afresh instead of never ending.
        assert_eq!(f.observe(0, "B"), None);
        assert_eq!(f.observe(300, "B"), Some("B".into()));
    }

    #[test]
    fn first_observation_without_a_settled_monitor_also_debounces() {
        let mut f = Follow::new(None);
        assert_eq!(f.observe(0, "A"), None);
        assert_eq!(f.observe(300, "A"), Some("A".into()));
        f.settle("Z");
        assert_eq!(f.observe(400, "z"), None, "device names compare case-insensitively");
    }

    #[test]
    fn offset_is_clamped_to_keep_an_open_panel_on_the_monitor() {
        // 1920 px at 100 %: half a panel (320) + 12 DIP margin = 332 -> the centre may move +-628.
        assert_eq!(shift_px(1920, 1.0, Anchor::Center, 0), 0);
        assert_eq!(shift_px(1920, 1.0, Anchor::Left, 0), -628);
        assert_eq!(shift_px(1920, 1.0, Anchor::Right, 0), 628);
        // The offset nudges from the anchor (positive = right).
        assert_eq!(shift_px(1920, 1.0, Anchor::Left, 100), -528);
        assert_eq!(shift_px(1920, 1.0, Anchor::Right, -100), 528);
        assert_eq!(shift_px(1920, 1.0, Anchor::Center, -40), -40);
        // Far too much: clamped on both sides, whatever the anchor.
        assert_eq!(shift_px(1920, 1.0, Anchor::Center, 5000), 628);
        assert_eq!(shift_px(1920, 1.0, Anchor::Center, -5000), -628);
        assert_eq!(shift_px(1920, 1.0, Anchor::Left, -300), -628);
        assert_eq!(shift_px(1920, 1.0, Anchor::Right, 300), 628);
    }

    #[test]
    fn offset_scales_with_dpi_and_survives_tiny_monitors() {
        // 150 %: the monitor is 2880 px wide, the offset DIP count 1.5 px each.
        assert_eq!(shift_px(2880, 1.5, Anchor::Center, 100), 150);
        assert_eq!(shift_px(2880, 1.5, Anchor::Left, 0), -942);
        // A monitor narrower than a panel: nowhere to go, the island stays centred.
        assert_eq!(shift_px(600, 1.0, Anchor::Right, 50), 0);
        assert_eq!(shift_px(0, 1.0, Anchor::Left, -50), 0);
    }

    #[test]
    fn the_visual_window_hangs_from_the_top_of_its_monitor() {
        let r = visual_rect(IRect::new(0, 0, 1920, 1080), 1.0, 0);
        assert_eq!((r.l, r.t, r.w(), r.h()), (568, 0, 784, 540));
        // Shifted left by the largest allowed amount: the shape (a 640 panel in the 784 window)
        // stays on the monitor; only 60 px of the transparent shadow margin hang over.
        let r = visual_rect(IRect::new(0, 0, 1920, 1080), 1.0, -628);
        assert_eq!(r.l, -60);
        assert!(r.l + 72 >= 12, "the panel's left edge is on the monitor");
        // A monitor with negative coordinates and a different DPI.
        let r = visual_rect(IRect::new(-2880, -200, 0, 1480), 1.5, 150);
        assert_eq!((r.t, r.w(), r.h()), (-200, 1176, 810));
        assert_eq!(r.l, -2880 + (2880 - 1176) / 2 + 150);
    }

    #[test]
    fn rect_helpers() {
        let r = IRect::new(10, 20, 110, 60);
        assert_eq!((r.w(), r.h()), (100, 40));
        assert!(r.contains(10, 20) && r.contains(109, 59) && !r.contains(110, 30) && !r.contains(50, 60));
        assert_eq!(r.inflate(5, 5), IRect::new(5, 15, 115, 65));
        assert_eq!(r.center(), (60.0, 40.0));
        assert_eq!(r.dist(60.0, 40.0), 0.0);
        assert_eq!(r.dist(0.0, 40.0), 10.0);
        assert_eq!(r.dist(113.0, 64.0), 5.0, "3-4-5 from the corner");
        assert_eq!(IRect::from_rect(r.to_rect()), r);
    }
}
