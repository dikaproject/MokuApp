//! Move aside: step out of the way of a pointer that is heading for the area under the island
//! without aiming *at* it (to the browser's address bar, a tab, a window button).
//!
//! The decision is cursor velocity and direction only ([`classify`], a pure function of the
//! last few pointer samples and the island's rectangle); [`Aside`] adds the hysteresis (when to
//! come back). Nothing here touches the system, so the whole behaviour is unit-tested with
//! simulated pointer paths, and the live island is driven with `mokuctl debug cursor`.
//!
//! The rule, per sample (distances in DIP, scaled to px by [`Params::for_scale`]):
//!
//! * the pointer is on the island: stay. Closer than [`NEAR`], moving at least [`SLOW`] and
//!   heading at the island (its path, extended, meets the island grown by [`TOL`]): look at
//!   how it is aimed; everything else is ignored: far away, creeping, moving away, passing
//!   elsewhere;
//! * if the path goes through the middle of the island ([`AIM`] of its shadow: *aimed*), a
//!   pointer that slows down ([`DECEL`]) or is not fast ([`FAST`]) wants the island (stay). A fast
//!   pointer that keeps its speed is on its way *through* it, to whatever is behind it: step
//!   aside, but only at the last moment ([`LEAD_AIMED_S`] before it arrives), because a pointer
//!   that is homing in only shows that it brakes late in its approach;
//! * if the path only grazes the island (along it, under it, next to it), nobody is aiming at it:
//!   step aside early ([`LEAD_GRAZE_S`]).
//!
//! Stepping aside is cheap to undo (see [`Aside`]), and the island lets clicks through at once,
//! so a wrong guess costs a short fade, not a lost click.

use crate::shell::monitors::IRect;

/// A pointer reading: position (physical px), time (ms), left button held.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub x: i32,
    pub y: i32,
    pub t: u64,
    pub down: bool,
}

/// Which way the island shifts when it slides: away from the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// Where the island is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Home,
    Away(Side),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing to do (far, moving away, passing elsewhere, creeping).
    Ignore,
    /// The pointer is on the island or is homing in on it: stay.
    Target,
    /// The pointer is going past or through the island: step aside.
    Aside,
}

// Thresholds in DIP (and DIP / s); `Params::for_scale` turns them into px.
pub const NEAR: f32 = 240.0;
pub const SLOW: f32 = 140.0;
pub const FAST: f32 = 650.0;
/// An aimed, fast pointer steps the island aside only when it would arrive within this (s) ...
pub const LEAD_AIMED_S: f32 = 0.12;
/// ... a pointer that only grazes it, within this (s).
pub const LEAD_GRAZE_S: f32 = 0.40;
pub const TOL: f32 = 14.0;
pub const AIM: f32 = 0.55;
/// Slower than this fraction of its earlier speed counts as slowing down.
pub const DECEL: f32 = 0.8;
/// Pointer history used for the velocity (ms).
pub const WINDOW_MS: u64 = 140;
/// While away: the pointer is "still here" inside the island grown by this many DIP.
pub const ZONE: f32 = 60.0;
/// While away: the pointer is resting when slower than this (DIP / s).
pub const REST: f32 = 60.0;
/// While away: the pointer must be outside the zone this long before the island comes back.
pub const LEAVE_MS: u64 = 400;
/// While away: resting on the island's own spot this long without a click brings it back (the
/// user wanted the island after all).
pub const REST_MS: u64 = 450;

#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub near: f32,
    pub slow: f32,
    pub fast: f32,
    pub tol: f32,
    pub zone: f32,
    pub rest: f32,
}

impl Params {
    pub fn for_scale(s: f32) -> Params {
        Params { near: NEAR * s, slow: SLOW * s, fast: FAST * s, tol: TOL * s, zone: ZONE * s, rest: REST * s }
    }
}

/// Velocity (px / s) of the pointer over the last [`WINDOW_MS`], and whether it is slowing down.
struct Motion {
    vx: f32,
    vy: f32,
    speed: f32,
    decel: bool,
}

fn motion(hist: &[Sample], slow: f32) -> Option<Motion> {
    let last = *hist.last()?;
    let from = hist.iter().position(|s| last.t.saturating_sub(s.t) <= WINDOW_MS)?;
    let win = &hist[from..];
    let first = *win.first()?;
    let dt = last.t.saturating_sub(first.t) as f32 / 1000.0;
    if win.len() < 2 || dt < 0.012 {
        return None;
    }
    let (vx, vy) = ((last.x - first.x) as f32 / dt, (last.y - first.y) as f32 / dt);
    let speed = (vx * vx + vy * vy).sqrt();
    let mut decel = false;
    if win.len() >= 3 {
        let mid = win[win.len() / 2];
        let (d1, d2) = (mid.t.saturating_sub(first.t) as f32 / 1000.0, last.t.saturating_sub(mid.t) as f32 / 1000.0);
        if d1 >= 0.008 && d2 >= 0.008 {
            let v1 = (((mid.x - first.x) as f32).powi(2) + ((mid.y - first.y) as f32).powi(2)).sqrt() / d1;
            let v2 = (((last.x - mid.x) as f32).powi(2) + ((last.y - mid.y) as f32).powi(2)).sqrt() / d2;
            decel = v1 > slow && v2 < v1 * DECEL;
        }
    }
    Some(Motion { vx, vy, speed, decel })
}

/// Distance along the unit direction `dir` from `p` to where the ray first meets `r` (0 when `p`
/// is inside), or `None` when it never does.
fn ray_entry(p: (f32, f32), dir: (f32, f32), r: &IRect) -> Option<f32> {
    let (mut t0, mut t1) = (0.0f32, f32::INFINITY);
    for (o, d, lo, hi) in [(p.0, dir.0, r.l as f32, r.r as f32), (p.1, dir.1, r.t as f32, r.b as f32)] {
        if d.abs() < 1e-6 {
            if o < lo || o > hi {
                return None;
            }
        } else {
            let (a, b) = ((lo - o) / d, (hi - o) / d);
            let (near, far) = if a < b { (a, b) } else { (b, a) };
            t0 = t0.max(near);
            t1 = t1.min(far);
            if t0 > t1 {
                return None;
            }
        }
    }
    Some(t0)
}

/// What the pointer (its recent `hist`, newest last) is up to relative to the island at `home`.
pub fn classify(hist: &[Sample], home: IRect, p: &Params) -> Verdict {
    let Some(last) = hist.last() else { return Verdict::Ignore };
    let pos = (last.x as f32, last.y as f32);
    let d = home.dist(pos.0, pos.1);
    if d <= 2.0 {
        return Verdict::Target;
    }
    if d > p.near {
        return Verdict::Ignore;
    }
    let Some(m) = motion(hist, p.slow) else { return Verdict::Ignore };
    if m.speed < p.slow {
        return Verdict::Ignore;
    }
    let dir = (m.vx / m.speed, m.vy / m.speed);
    // Moving away from the island (or alongside it, never closer)?
    let q = (pos.0.clamp(home.l as f32, home.r as f32), pos.1.clamp(home.t as f32, home.b as f32));
    let to = ((q.0 - pos.0) / d, (q.1 - pos.1) / d);
    if m.vx * to.0 + m.vy * to.1 <= 0.0 {
        return Verdict::Ignore;
    }
    // Does the path reach the island (a little grown)?
    let tol = p.tol.round() as i32;
    let Some(entry) = ray_entry(pos, dir, &home.inflate(tol, tol)) else { return Verdict::Ignore };
    let arrives_in = entry / m.speed;
    // Aimed at the middle of the island, or only grazing it?
    let c = home.center();
    let (cx, cy) = (c.0 - pos.0, c.1 - pos.1);
    let off = (dir.0 * cy - dir.1 * cx).abs();
    let shadow = (home.w() as f32 / 2.0) * dir.1.abs() + (home.h() as f32 / 2.0) * dir.0.abs();
    let aimed = off <= AIM * shadow;
    if aimed && (m.decel || m.speed < p.fast) {
        return Verdict::Target;
    }
    if arrives_in > if aimed { LEAD_AIMED_S } else { LEAD_GRAZE_S } {
        return Verdict::Ignore;
    }
    Verdict::Aside
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Home,
    Away { side: Side, clicked: bool, out_since: Option<u64>, rest_since: Option<u64> },
}

/// The move-aside state machine: feed it every pointer sample.
#[derive(Debug)]
pub struct Aside {
    hist: Vec<Sample>,
    phase: Phase,
    /// The last verdict, for `debug state`.
    pub last: Verdict,
}

impl Default for Aside {
    fn default() -> Self {
        Aside { hist: Vec::with_capacity(12), phase: Phase::Home, last: Verdict::Ignore }
    }
}

impl Aside {
    pub fn state(&self) -> State {
        match self.phase {
            Phase::Home => State::Home,
            Phase::Away { side, .. } => State::Away(side),
        }
    }

    /// Forget everything (feature switched off, or the island moved to another monitor).
    pub fn reset(&mut self) {
        self.hist.clear();
        self.phase = Phase::Home;
        self.last = Verdict::Ignore;
    }

    fn push(&mut self, s: Sample) {
        match self.hist.last_mut() {
            Some(l) if s.t <= l.t => *l = Sample { t: l.t, ..s },
            _ => self.hist.push(s),
        }
        let cut = s.t.saturating_sub(400);
        self.hist.retain(|h| h.t >= cut);
    }

    /// One pointer sample. `home` = the island's resting rectangle, `eligible` = false while
    /// moving aside would be wrong (a panel is open, something urgent is shown).
    pub fn step(&mut self, s: Sample, home: IRect, scale: f32, eligible: bool) -> State {
        self.push(s);
        let p = Params::for_scale(scale);
        if !eligible {
            self.phase = Phase::Home;
            self.last = Verdict::Ignore;
            return State::Home;
        }
        match self.phase {
            Phase::Home => {
                // A button held while the pointer moves is a drag (a window, a selection, a file on
                // its way to the Shelf): never a reason to step aside.
                self.last = if s.down { Verdict::Ignore } else { classify(&self.hist, home, &p) };
                if self.last == Verdict::Aside {
                    let side = if (s.x as f32) < home.center().0 { Side::Right } else { Side::Left };
                    self.phase = Phase::Away { side, clicked: false, out_since: None, rest_since: None };
                }
            }
            Phase::Away { side, mut clicked, mut out_since, mut rest_since } => {
                let zone = home.inflate(p.zone.round() as i32, p.zone.round() as i32);
                let near = zone.contains(s.x, s.y);
                if near && s.down {
                    clicked = true;
                }
                out_since = if near { None } else { out_since.or(Some(s.t)) };
                let resting = motion(&self.hist, p.slow).is_none_or(|m| m.speed < p.rest);
                let on_spot = home.inflate(8, 8).contains(s.x, s.y);
                rest_since = if resting && on_spot { rest_since.or(Some(s.t)) } else { None };
                let back = out_since.is_some_and(|o| s.t.saturating_sub(o) >= LEAVE_MS) || (!clicked && rest_since.is_some_and(|r| s.t.saturating_sub(r) >= REST_MS));
                if back {
                    self.phase = Phase::Home;
                    // Start over: the old samples would look like another approach.
                    self.hist.clear();
                    self.hist.push(s);
                } else {
                    self.phase = Phase::Away { side, clicked, out_since, rest_since };
                }
            }
        }
        self.state()
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// A 1920x1080 monitor at 100 %: the idle pill, 120 x 36 DIP, 8 DIP below the top edge.
    const HOME: IRect = IRect::new(900, 8, 1020, 44);
    const P: fn() -> Params = || Params::for_scale(1.0);

    /// Samples every `dt` ms along the given positions.
    fn pts(p: &[(i32, i32)], dt: u64) -> Vec<Sample> {
        p.iter().enumerate().map(|(i, &(x, y))| Sample { x, y, t: i as u64 * dt, down: false }).collect()
    }

    /// Samples every `dt` ms along a straight line from `from` towards `to`, one per entry of
    /// `speeds` (px / s) plus the starting point.
    fn path(from: (f32, f32), to: (f32, f32), speeds: &[f32], dt: u64) -> Vec<Sample> {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let len = (dx * dx + dy * dy).sqrt();
        let (ux, uy) = (dx / len, dy / len);
        let (mut x, mut y, mut t) = (from.0, from.1, 0u64);
        let mut out = vec![Sample { x: x.round() as i32, y: y.round() as i32, t, down: false }];
        for v in speeds {
            let step = v * dt as f32 / 1000.0;
            x += ux * step;
            y += uy * step;
            t += dt;
            out.push(Sample { x: x.round() as i32, y: y.round() as i32, t, down: false });
        }
        out
    }

    fn verdict(h: &[Sample]) -> Verdict {
        classify(h, HOME, &P())
    }

    // ---- heading INTO the island: stay --------------------------------------------------------

    #[test]
    fn a_pointer_that_brakes_on_its_way_to_the_island_is_aiming_at_it() {
        // From below, straight up its middle, braking from a flick (1200 -> 450 px/s).
        let h = pts(&[(960, 300), (960, 262), (960, 228), (960, 198), (960, 172), (960, 150), (960, 132), (960, 118)], 30);
        assert_eq!(verdict(&h), Verdict::Target);
        // And at no point on the way is it taken for a pass-through.
        for n in 2..=h.len() {
            assert_ne!(verdict(&h[..n]), Verdict::Aside, "after {n} samples");
        }
    }

    #[test]
    fn a_calm_pointer_heading_at_the_middle_is_not_dodged_even_without_slowing() {
        // Steady 420 px/s, under the fast threshold: someone aiming at the island.
        let h = path((960.0, 200.0), (960.0, 44.0), &[420.0; 5], 30);
        assert_eq!(verdict(&h), Verdict::Target);
        // From the side along its row, 300 px/s.
        let h = path((760.0, 26.0), (900.0, 26.0), &[300.0; 5], 30);
        assert_eq!(verdict(&h), Verdict::Target);
    }

    #[test]
    fn a_pointer_already_on_the_island_is_the_target() {
        let on = Sample { x: 960, y: 20, t: 200, down: false };
        assert_eq!(verdict(&[Sample { x: 700, y: 26, t: 170, down: false }, on]), Verdict::Target);
        assert_eq!(verdict(&[on]), Verdict::Target);
    }

    // ---- going PAST / THROUGH it: step aside --------------------------------------------------

    #[test]
    fn a_fast_pointer_that_keeps_its_speed_through_the_island_steps_it_aside() {
        // Flicking up the island's middle at ~1400 px/s towards whatever is behind it.
        let h = path((960.0, 230.0), (960.0, 44.0), &[1400.0; 3], 30);
        assert_eq!(verdict(&h), Verdict::Aside);
        // Not before it is about to arrive: still 200 px away it could be homing in.
        let far = path((960.0, 380.0), (960.0, 44.0), &[1400.0; 3], 30);
        assert_ne!(verdict(&far), Verdict::Aside);
    }

    #[test]
    fn a_braking_flick_is_told_from_a_steady_one() {
        let steady = path((960.0, 230.0), (960.0, 44.0), &[1400.0; 3], 30);
        assert_eq!(verdict(&steady), Verdict::Aside);
        // The same flick that slows down over the last stretch.
        let brakes = path((960.0, 230.0), (960.0, 44.0), &[1500.0, 1400.0, 1100.0, 700.0, 450.0], 30);
        assert_eq!(verdict(&brakes), Verdict::Target);
    }

    #[test]
    fn heading_for_the_address_bar_along_the_top_steps_aside() {
        // Moving right along the toolbar just under the island (its bottom is at 44), towards an
        // address bar that continues beside it: the path grazes the island.
        let h = path((700.0, 50.0), (1200.0, 50.0), &[750.0; 5], 30);
        assert_eq!(verdict(&h), Verdict::Aside);
        // From the right, going left.
        let h = path((1300.0, 50.0), (800.0, 50.0), &[750.0; 5], 30);
        assert_eq!(verdict(&h), Verdict::Aside);
    }

    #[test]
    fn grazing_the_island_from_below_at_an_angle_steps_aside() {
        // Heading up and to the right, clipping the island's lower left corner.
        let h = path((760.0, 190.0), (1000.0, 40.0), &[700.0; 5], 30);
        assert_eq!(verdict(&h), Verdict::Aside);
    }

    #[test]
    fn flying_through_the_island_along_its_row_steps_aside_unless_it_brakes() {
        let h = path((700.0, 26.0), (1100.0, 26.0), &[1100.0; 4], 30);
        assert_eq!(verdict(&h), Verdict::Aside);
        let h = path((700.0, 26.0), (1100.0, 26.0), &[1100.0, 1000.0, 800.0, 500.0, 300.0], 30);
        assert_eq!(verdict(&h), Verdict::Target);
    }

    // ---- everything else: leave it alone ------------------------------------------------------

    #[test]
    fn far_away_moving_away_passing_elsewhere_or_creeping_is_ignored() {
        // Far.
        assert_eq!(verdict(&path((960.0, 700.0), (960.0, 44.0), &[1400.0; 5], 30)), Verdict::Ignore);
        assert_eq!(verdict(&path((500.0, 50.0), (1200.0, 50.0), &[300.0; 5], 30)), Verdict::Ignore);
        // Near but heading away.
        assert_eq!(verdict(&path((960.0, 120.0), (960.0, 700.0), &[1200.0; 5], 30)), Verdict::Ignore);
        // Near, along a path that passes well below the island (y = 100, its bottom is 44).
        assert_eq!(verdict(&path((700.0, 100.0), (1200.0, 100.0), &[900.0; 5], 30)), Verdict::Ignore);
        // Near, aimed at the island but creeping (resting hand).
        assert_eq!(verdict(&path((960.0, 120.0), (960.0, 44.0), &[90.0; 5], 30)), Verdict::Ignore);
        // Heading to a point beside the island's side, not at it (x = 1200 vs right edge 1020).
        assert_eq!(verdict(&path((1200.0, 250.0), (1200.0, 44.0), &[900.0; 5], 30)), Verdict::Ignore);
        // One sample, or no time between samples: no velocity yet.
        assert_eq!(verdict(&[Sample { x: 960, y: 100, t: 0, down: false }]), Verdict::Ignore);
        assert_eq!(verdict(&[]), Verdict::Ignore);
        let same = Sample { x: 960, y: 100, t: 5, down: false };
        assert_eq!(verdict(&[Sample { t: 0, ..same }, same]), Verdict::Ignore);
    }

    #[test]
    fn a_slow_grazing_pointer_has_time_and_is_left_alone() {
        // 150 px/s along the toolbar, 75 px from the island: 0.4 s away is the limit.
        assert_ne!(verdict(&path((600.0, 50.0), (1200.0, 50.0), &[300.0; 5], 30)), Verdict::Aside, "too early");
        assert_eq!(verdict(&path((780.0, 50.0), (1200.0, 50.0), &[300.0; 5], 30)), Verdict::Aside);
    }

    #[test]
    fn thresholds_scale_with_the_display() {
        // The same 1000 px/s: fast at 100 % (650 px/s), calm at 200 % (1300 px/s).
        let home = IRect::new(1800, 16, 2040, 88);
        let h = path((1920.0, 330.0), (1920.0, 88.0), &[1000.0; 4], 30);
        assert_eq!(classify(&h, home, &Params::for_scale(1.0)), Verdict::Aside);
        assert_eq!(classify(&h, home, &Params::for_scale(2.0)), Verdict::Target);
    }

    // ---- the state machine --------------------------------------------------------------------

    /// Feed `samples` (t in ms) to a machine; returns the state after each one.
    fn run(a: &mut Aside, samples: &[Sample]) -> Vec<State> {
        samples.iter().map(|s| a.step(*s, HOME, 1.0, true)).collect()
    }

    fn at(x: i32, y: i32, t: u64) -> Sample {
        Sample { x, y, t, down: false }
    }

    /// A fast flick through the island from the left along its row (ends at t = 150).
    fn flick() -> Vec<Sample> {
        path((700.0, 26.0), (1100.0, 26.0), &[1100.0; 5], 30)
    }

    #[test]
    fn it_steps_aside_for_a_flick_and_away_from_the_pointer() {
        let mut a = Aside::default();
        let states = run(&mut a, &flick());
        assert_eq!(states.first(), Some(&State::Home));
        assert_eq!(states.last(), Some(&State::Away(Side::Right)), "the pointer came from the left: shift right");
        assert_eq!(a.last, Verdict::Aside);
        // From the right: shift left.
        let mut a = Aside::default();
        let h = path((1300.0, 26.0), (900.0, 26.0), &[1100.0; 5], 30);
        assert_eq!(*run(&mut a, &h).last().unwrap(), State::Away(Side::Left));
    }

    #[test]
    fn it_does_not_step_aside_during_a_drag() {
        // The same flick with the left button held (dragging a window, a selection, a file): ignored.
        let mut a = Aside::default();
        let held: Vec<Sample> = flick().into_iter().map(|s| Sample { down: true, ..s }).collect();
        let states = run(&mut a, &held);
        assert!(states.iter().all(|s| *s == State::Home), "{states:?}");
        assert_eq!(a.last, Verdict::Ignore);
        // Let go in the middle of it: from then on it dodges again.
        let mut a = Aside::default();
        let mut mixed = flick();
        for s in mixed.iter_mut().take(3) {
            s.down = true;
        }
        let states = run(&mut a, &mixed);
        assert!(states[..3].iter().all(|s| *s == State::Home));
        // (the samples before the release counted as history, but none of them was a verdict)
        let _ = states;
        let after = run(&mut a, &flick().iter().map(|s| Sample { t: s.t + 1000, ..*s }).collect::<Vec<_>>());
        assert!(matches!(after.last(), Some(State::Away(_))), "{after:?}");
    }

    #[test]
    fn it_does_not_step_aside_for_a_pointer_that_homes_in() {
        let mut a = Aside::default();
        let h = pts(&[(960, 300), (960, 262), (960, 228), (960, 198), (960, 172), (960, 150), (960, 132), (960, 118)], 30);
        assert!(run(&mut a, &h).iter().all(|s| *s == State::Home));
        assert_eq!(a.last, Verdict::Target);
    }

    #[test]
    fn it_comes_back_400_ms_after_the_pointer_leaves_the_zone() {
        let mut a = Aside::default();
        run(&mut a, &flick());
        assert!(matches!(a.state(), State::Away(_)));
        // The pointer carries on to the far side, well outside the zone (60 DIP).
        assert!(matches!(a.step(at(1400, 300, 1000), HOME, 1.0, true), State::Away(_)), "just left: not yet");
        assert!(matches!(a.step(at(1400, 300, 1399), HOME, 1.0, true), State::Away(_)));
        assert_eq!(a.step(at(1400, 300, 1400), HOME, 1.0, true), State::Home);
    }

    #[test]
    fn a_short_trip_out_of_the_zone_and_back_keeps_it_away() {
        let mut a = Aside::default();
        run(&mut a, &flick());
        a.step(at(1200, 150, 1000), HOME, 1.0, true); // outside the zone (x 840..1080, y -52..104)
        a.step(at(1060, 60, 1200), HOME, 1.0, true); // back inside: the timer is gone
        assert!(matches!(a.step(at(1200, 150, 1500), HOME, 1.0, true), State::Away(_)));
        assert!(matches!(a.step(at(1200, 150, 1850), HOME, 1.0, true), State::Away(_)), "only 350 ms since this visit outside");
        assert_eq!(a.step(at(1200, 150, 1900), HOME, 1.0, true), State::Home);
    }

    #[test]
    fn resting_on_the_islands_spot_brings_it_back_after_450_ms() {
        let mut a = Aside::default();
        run(&mut a, &flick());
        let rest = |t| at(960, 26, t);
        assert!(matches!(a.step(rest(300), HOME, 1.0, true), State::Away(_)));
        for t in (330..750).step_by(30) {
            assert!(matches!(a.step(rest(t), HOME, 1.0, true), State::Away(_)), "t={t}");
        }
        assert_eq!(a.step(rest(750), HOME, 1.0, true), State::Home, "the user wanted the island");
    }

    #[test]
    fn resting_just_below_the_island_is_not_its_spot() {
        // The pointer stopped at the address bar under the island (y = 56, the island ends at 44).
        let mut a = Aside::default();
        run(&mut a, &flick());
        a.step(at(960, 56, 300), HOME, 1.0, true);
        for t in (330..3000).step_by(30) {
            assert!(matches!(a.step(at(960, 56, t), HOME, 1.0, true), State::Away(_)), "t={t}");
        }
    }

    #[test]
    fn a_click_while_away_means_the_target_is_underneath_so_it_stays_away() {
        let mut a = Aside::default();
        run(&mut a, &flick());
        // Rests on the spot, presses the button (a click on the address bar under the island) ...
        a.step(at(960, 26, 300), HOME, 1.0, true);
        a.step(Sample { down: true, ..at(960, 26, 340) }, HOME, 1.0, true);
        a.step(at(960, 26, 380), HOME, 1.0, true);
        // ... and keeps resting there: no return.
        for t in (400..3000).step_by(50) {
            assert!(matches!(a.step(at(960, 26, t), HOME, 1.0, true), State::Away(_)), "t={t}");
        }
        // Leaving the zone for 400 ms brings it back.
        a.step(at(1300, 400, 3100), HOME, 1.0, true);
        assert_eq!(a.step(at(1300, 400, 3500), HOME, 1.0, true), State::Home);
    }

    #[test]
    fn ineligible_means_home_at_once_and_no_dodging() {
        let mut a = Aside::default();
        run(&mut a, &flick());
        assert!(matches!(a.state(), State::Away(_)));
        // A panel opened (hotkey) or something urgent appeared: back immediately.
        assert_eq!(a.step(at(960, 26, 300), HOME, 1.0, false), State::Home);
        // And while ineligible a flick does nothing.
        let mut a = Aside::default();
        let states: Vec<State> = flick().iter().map(|s| a.step(*s, HOME, 1.0, false)).collect();
        assert!(states.iter().all(|s| *s == State::Home));
    }

    #[test]
    fn after_coming_back_the_old_path_does_not_trigger_again() {
        let mut a = Aside::default();
        run(&mut a, &flick());
        a.step(at(1400, 300, 1000), HOME, 1.0, true);
        assert_eq!(a.step(at(1400, 300, 1400), HOME, 1.0, true), State::Home);
        // The pointer rests far away: nothing.
        for t in (1430..2000).step_by(30) {
            assert_eq!(a.step(at(1400, 300, t), HOME, 1.0, true), State::Home);
        }
    }

    #[test]
    fn duplicate_timestamps_do_not_break_the_history() {
        let mut a = Aside::default();
        a.step(at(100, 100, 10), HOME, 1.0, true);
        a.step(at(110, 100, 10), HOME, 1.0, true);
        a.step(at(120, 100, 5), HOME, 1.0, true); // time went backwards: replaces the newest
        assert_eq!(a.state(), State::Home);
        assert!(a.hist.len() <= 2);
    }
}
