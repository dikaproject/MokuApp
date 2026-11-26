//! Word-by-word highlight of one line of text ([`karaoke`] node): the pure part.
//!
//! The scene shows the line twice: a dim base and a bright copy on top, revealed from left to
//! right by a soft-edged mask that the **compositor** moves (a key-frame animation, no per-frame
//! work in this process). This module holds everything that does not need Windows: the node's
//! data, the word → x-position mapping, the highlight position over time, the key-frame plan
//! for "from here to the end of the line" and the decision when a running sweep has to be
//! restarted. All of it is unit-tested.

use std::time::{Duration, Instant};

use super::{Node, Kind, Rgba, TextSpec};

/// One word of the sung line: when it is sung (media time, ms) and where it is in the text
/// (byte range).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WordSpan {
    pub start_ms: u64,
    pub end_ms: u64,
    pub from: usize,
    pub to: usize,
}

/// The playback clock a sweep follows: the player's position `pos_ms` as of `at`, moving at
/// `rate`× while `playing`. Only the player's own reports change it (a new timeline, pause, seek),
/// so two trees built from the same reports hold equal anchors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    pub pos_ms: u64,
    pub at: Instant,
    pub playing: bool,
    pub rate: f32,
}

impl Anchor {
    /// The media position (ms) at `now`.
    pub fn pos_at(&self, now: Instant) -> f64 {
        let mut p = self.pos_ms as f64;
        if self.playing {
            let dt = if now >= self.at { now.duration_since(self.at).as_secs_f64() } else { -self.at.duration_since(now).as_secs_f64() };
            p += dt * 1000.0 * self.rate.max(0.0) as f64;
        }
        p.max(0.0)
    }
}

/// What [`karaoke`] needs.
#[derive(Clone, Debug, PartialEq)]
pub struct KaraokeSpec {
    /// Identity of the sung line (track + line index): the same id keeps a running sweep running
    /// across redraws of the tree.
    pub id: String,
    /// The text in its **bright** (already sung) colour; font, size, weight as for [`text`](super::text).
    pub text: TextSpec,
    /// Colour of the part not sung yet.
    pub dim: Rgba,
    pub words: Vec<WordSpan>,
    pub anchor: Anchor,
    /// DIP the line rises while it fades in when it becomes the sung line.
    pub rise: f32,
}

/// A line of text whose words light up in time with the music. The node lays out like [`text`](super::text)
/// (one line, ellipsis at its width); the scene draws it, dim and bright, and sweeps the bright copy
/// across on the compositor. With animations reduced or off the whole line is simply bright.
pub fn karaoke(spec: KaraokeSpec) -> Node {
    Node::new(Kind::Karaoke(spec))
}

/// One sung word in layout terms: media ms and DIP x inside the node.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seg {
    pub t0: u64,
    pub t1: u64,
    pub x0: f32,
    pub x1: f32,
}

/// What layout hands the scene (see [`super::layout::Live::Karaoke`]).
#[derive(Clone, Debug, PartialEq)]
pub struct KaraokeLive {
    pub r: super::Rect,
    pub id: String,
    /// Bright text (alpha folded in).
    pub text: TextSpec,
    pub dim: Rgba,
    pub segs: Vec<Seg>,
    /// Width of the drawn text (≤ `r.w`).
    pub text_w: f32,
    pub anchor: Anchor,
    pub rise: f32,
}

impl KaraokeLive {
    /// Equal apart from the anchor and the position: the same sweep, nothing to repaint.
    pub fn same_line(&self, o: &KaraokeLive) -> bool {
        self.id == o.id && self.text == o.text && self.dim == o.dim && self.segs == o.segs && self.text_w == o.text_w && self.r.w == o.r.w && self.r.h == o.r.h && self.rise == o.rise
    }
}

/// Map word byte ranges to x positions. `x_of(byte)` is the x of the caret before that byte of
/// the line laid out on one unbounded line; `box_w` the width the text is drawn in (longer text
/// ellipsises there, so x is capped), `text_w` the drawn width. The first word starts at 0, the
/// last ends at `text_w`, x never decreases.
pub fn segs_for(words: &[WordSpan], x_of: &dyn Fn(usize) -> f32, box_w: f32, text_w: f32) -> Vec<Seg> {
    let cap = box_w.max(0.0).min(text_w.max(0.0));
    let cap = if cap.is_finite() { cap } else { 0.0 };
    let x_of = |b: usize| {
        let v = x_of(b);
        if v.is_finite() { v } else { 0.0 }
    };
    let n = words.len();
    let mut out: Vec<Seg> = Vec::with_capacity(n);
    for (i, w) in words.iter().enumerate() {
        let prev_x1 = out.last().map_or(0.0, |s: &Seg| s.x1);
        let x0 = if i == 0 { 0.0 } else { x_of(w.from).clamp(prev_x1, cap) };
        let x1 = if i + 1 == n { cap } else { x_of(w.to).clamp(x0, cap) };
        // Times never go backwards (the plan relies on it).
        let t0 = w.start_ms.max(out.last().map_or(0, |s: &Seg| s.t1));
        out.push(Seg { t0, t1: w.end_ms.max(t0), x0, x1 });
    }
    out
}

/// How the highlight eases inside one word: half linear, half smoothstep, so it follows the
/// voice (a little slower at the edges of a word) without ever stopping between words.
pub fn ease(u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    0.5 * u + 0.5 * (u * u * (3.0 - 2.0 * u))
}

/// Where the lit part of the line ends (DIP x, in the text) at media time `t_ms`: inside a word
/// it moves from the word's start to its end; between words it moves on at a steady pace; before
/// the first word it waits at the start, after the last it stays at the end.
pub fn x_at(segs: &[Seg], t_ms: f64) -> f32 {
    let Some(first) = segs.first() else { return 0.0 };
    let mut prev: Option<&Seg> = None;
    for s in segs {
        let (t0, t1) = (s.t0 as f64, s.t1 as f64);
        if t_ms < t0 {
            return match prev {
                Some(p) => {
                    let (a, b) = (p.t1 as f64, t0);
                    let k = if b > a { ((t_ms - a) / (b - a)).clamp(0.0, 1.0) as f32 } else { 1.0 };
                    p.x1 + (s.x0 - p.x1) * k
                }
                None => first.x0,
            };
        }
        if t_ms < t1 {
            let u = ((t_ms - t0) / (t1 - t0)) as f32;
            return s.x0 + (s.x1 - s.x0) * ease(u);
        }
        prev = Some(s);
    }
    prev.map_or(0.0, |p| p.x1)
}

/// Position of the soft edge's left end for a lit width `x`: the bright part ends at the edge's
/// start and the edge fades out over `feather` DIP. Mapped so that `x = 0` lights nothing (the
/// edge is entirely left of the text) and `x = text_w` lights everything (entirely right of it).
pub fn edge(x: f32, text_w: f32, feather: f32) -> f32 {
    let w = text_w.max(1.0);
    x + feather * (x / w - 1.0)
}

/// Where the mask edge starts and how it moves from there (all a pure function of the clock).
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// The edge position right now.
    pub start: f32,
    /// `(normalised time 0..=1, edge position)`, strictly increasing in time, first at 0 with
    /// `start`, last at 1. Empty = nothing moves (paused, finished, or no words): stay at `start`.
    pub frames: Vec<(f32, f32)>,
    /// Real time the frames span, ms.
    pub duration_ms: f32,
}

/// Sub-samples of the ease inside a word (the compositor interpolates linearly between them).
const SUB: u32 = 4;
/// At most this many key frames per line.
const MAX_FRAMES: usize = 360;

/// Plan the sweep from media position `pos_ms` to the end of the line.
pub fn plan(segs: &[Seg], text_w: f32, feather: f32, pos_ms: f64, playing: bool, rate: f32) -> Plan {
    let e = |x: f32| edge(x, text_w, feather);
    let Some(last) = segs.last() else { return Plan { start: e(text_w), frames: vec![], duration_ms: 0.0 } };
    let t_end = last.t1 as f64;
    let start = e(x_at(segs, pos_ms));
    if !playing || rate <= 0.0 || pos_ms >= t_end - 1.0 {
        // Frozen where the voice is (or already done: `x_at` is the end then).
        return Plan { start, frames: vec![], duration_ms: 0.0 };
    }
    let sub = if segs.len() as u32 * SUB + 2 > MAX_FRAMES as u32 { 1 } else { SUB };
    // (media time, x) of every point where the highlight changes pace, after `pos_ms`.
    let mut pts: Vec<(f64, f32)> = Vec::with_capacity(segs.len() * (sub as usize + 1) + 2);
    let mut prev_end: Option<(f64, f32)> = None;
    for s in segs {
        let (t0, t1) = (s.t0 as f64, s.t1 as f64);
        // A word that starts where the last one ended needs no point of its own.
        if prev_end != Some((t0, s.x0)) {
            pts.push((t0, s.x0));
        }
        prev_end = Some((t1, s.x1));
        for k in 1..sub {
            let u = k as f32 / sub as f32;
            pts.push((t0 + (t1 - t0) * u as f64, s.x0 + (s.x1 - s.x0) * ease(u)));
        }
        pts.push((t1, s.x1));
    }
    let mut frames_ms: Vec<(f64, f32)> = vec![(pos_ms, start)];
    for (t, x) in pts {
        let last_t = frames_ms.last().map_or(pos_ms, |f| f.0);
        if t <= pos_ms {
            continue;
        }
        // Equal times (a word that starts where the last one ended) get one ms between them.
        let t = if t <= last_t { last_t + 1.0 } else { t };
        frames_ms.push((t, e(x)));
    }
    let t_last = frames_ms.last().map_or(pos_ms, |f| f.0);
    let span = (t_last - pos_ms).max(1.0);
    let frames = frames_ms.iter().map(|&(t, v)| (((t - pos_ms) / span) as f32, v)).collect();
    Plan { start, frames, duration_ms: (span / rate as f64) as f32 }
}

/// The edge position `plan` predicts at `ms` real milliseconds after it starts (what the
/// compositor shows then); for tests and for freezing a sweep exactly where it is.
pub fn plan_value_at(p: &Plan, ms: f32) -> f32 {
    if p.frames.is_empty() || p.duration_ms <= 0.0 {
        return p.start;
    }
    let k = (ms / p.duration_ms).clamp(0.0, 1.0);
    for w in p.frames.windows(2) {
        let ((k0, v0), (k1, v1)) = (w[0], w[1]);
        if k <= k1 {
            let u = if k1 > k0 { (k - k0) / (k1 - k0) } else { 1.0 };
            return v0 + (v1 - v0) * u;
        }
    }
    p.frames.last().map_or(p.start, |f| f.1)
}

/// The clock a running sweep was started on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunClock {
    /// Media position (ms) at `started`.
    pub start_pos: f64,
    pub started: Instant,
    pub playing: bool,
    pub rate: f32,
}

impl RunClock {
    pub fn pos_at(&self, now: Instant) -> f64 {
        if !self.playing {
            return self.start_pos;
        }
        let dt = if now >= self.started { now.duration_since(self.started).as_secs_f64() } else { -self.started.duration_since(now).as_secs_f64() };
        self.start_pos + dt * 1000.0 * self.rate.max(0.0) as f64
    }
}

/// A sweep may be off the player's clock by this much before it is restarted (players report
/// their position with some jitter; restarting on every report would make the highlight twitch).
pub const DRIFT_MS: f64 = 140.0;

/// Does a running sweep have to be restarted for the player's current `anchor`? Yes after a
/// pause / resume, a rate change, a seek, or when it has drifted off the clock.
pub fn needs_restart(run: &RunClock, a: &Anchor, now: Instant) -> bool {
    if run.playing != a.playing || (run.rate - a.rate).abs() > 0.001 {
        return true;
    }
    let tol = if a.playing { DRIFT_MS } else { 3.0 };
    (run.pos_at(now) - a.pos_at(now)).abs() > tol
}

/// The clock for a sweep started now for `anchor`. The compositor takes a frame or two to pick
/// an animation up, so it starts `lead` ahead.
pub fn start_clock(a: &Anchor, now: Instant, lead: Duration) -> RunClock {
    let started = now + lead;
    RunClock { start_pos: a.pos_at(started), started, playing: a.playing, rate: a.rate }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "ab cde f" → 3 words at 7 DIP per char, one per 1000 ms starting at 10 000.
    fn demo() -> (Vec<Seg>, f32) {
        let words = [
            WordSpan { start_ms: 10_000, end_ms: 11_000, from: 0, to: 2 },
            WordSpan { start_ms: 11_000, end_ms: 12_000, from: 3, to: 6 },
            WordSpan { start_ms: 12_000, end_ms: 13_000, from: 7, to: 8 },
        ];
        let x = |b: usize| b as f32 * 7.0;
        (segs_for(&words, &x, 200.0, 56.0), 56.0)
    }

    #[test]
    fn segs_map_words_to_x_and_cover_the_text() {
        let (s, w) = demo();
        assert_eq!(s.len(), 3);
        assert_eq!((s[0].x0, s[0].x1), (0.0, 14.0));
        assert_eq!((s[1].x0, s[1].x1), (21.0, 42.0));
        assert_eq!((s[2].x0, s[2].x1), (49.0, w), "the last word ends at the end of the text");
        // x never goes back, times never go back.
        assert!(s.windows(2).all(|p| p[0].x1 <= p[1].x0 && p[0].t1 <= p[1].t0));
    }

    #[test]
    fn segs_are_capped_at_the_box_width() {
        let words = [WordSpan { start_ms: 0, end_ms: 100, from: 0, to: 5 }, WordSpan { start_ms: 100, end_ms: 200, from: 6, to: 11 }];
        let s = segs_for(&words, &|b| b as f32 * 10.0, 40.0, 110.0);
        assert_eq!((s[0].x0, s[0].x1), (0.0, 40.0));
        assert_eq!((s[1].x0, s[1].x1), (40.0, 40.0), "words after the ellipsis add no width");
    }

    #[test]
    fn segs_survive_nonsense_measurements() {
        let words = [WordSpan { start_ms: 0, end_ms: 100, from: 0, to: 2 }, WordSpan { start_ms: 100, end_ms: 200, from: 3, to: 5 }];
        let s = segs_for(&words, &|_| f32::NAN, f32::NAN, f32::INFINITY);
        assert!(s.iter().all(|s| s.x0.is_finite() && s.x1.is_finite() && s.x0 <= s.x1));
        let s = segs_for(&words, &|b| -(b as f32), 50.0, 40.0);
        assert!(s.iter().all(|s| s.x0 >= 0.0 && s.x1 <= 40.0 && s.x0 <= s.x1), "{s:?}");
        assert!(segs_for(&[], &|b| b as f32, 10.0, 10.0).is_empty());
    }

    #[test]
    fn segs_repair_backwards_times() {
        let words = [WordSpan { start_ms: 500, end_ms: 400, from: 0, to: 2 }, WordSpan { start_ms: 100, end_ms: 200, from: 3, to: 5 }];
        let s = segs_for(&words, &|b| b as f32, 100.0, 5.0);
        assert!(s[0].t1 >= s[0].t0 && s[1].t0 >= s[0].t1 && s[1].t1 >= s[1].t0, "{s:?}");
    }

    #[test]
    fn highlight_position_over_time() {
        let (s, w) = demo();
        assert_eq!(x_at(&s, 0.0), 0.0, "before the line: nothing lit");
        assert_eq!(x_at(&s, 10_000.0), 0.0);
        // Half way through the first word the eased position is exactly half (symmetric ease).
        assert!((x_at(&s, 10_500.0) - 7.0).abs() < 1e-4);
        assert!((x_at(&s, 11_000.0) - 21.0).abs() < 1e-4, "start of the second word");
        assert_eq!(x_at(&s, 13_000.0), w);
        assert_eq!(x_at(&s, 99_000.0), w, "after the line: everything lit");
        // Never goes backwards.
        let mut prev = -1.0;
        for t in (9_000..14_000).step_by(7) {
            let x = x_at(&s, t as f64);
            assert!(x >= prev, "t={t}");
            prev = x;
        }
    }

    #[test]
    fn highlight_crosses_a_pause_between_words_steadily() {
        let segs = [Seg { t0: 0, t1: 1000, x0: 0.0, x1: 10.0 }, Seg { t0: 2000, t1: 3000, x0: 20.0, x1: 30.0 }];
        assert_eq!(x_at(&segs, 1000.0), 10.0);
        assert!((x_at(&segs, 1500.0) - 15.0).abs() < 1e-4, "linear across the gap");
        assert_eq!(x_at(&segs, 2000.0), 20.0);
        // Zero-length words (0 ms) are skipped without dividing by zero.
        let z = [Seg { t0: 5, t1: 5, x0: 0.0, x1: 4.0 }, Seg { t0: 5, t1: 5, x0: 4.0, x1: 8.0 }];
        for t in [0.0, 5.0, 6.0] {
            assert!(x_at(&z, t).is_finite());
        }
        assert_eq!(x_at(&z, 6.0), 8.0);
        assert_eq!(x_at(&[], 5.0), 0.0);
    }

    #[test]
    fn edge_lights_nothing_at_the_start_and_everything_at_the_end() {
        assert_eq!(edge(0.0, 100.0, 12.0), -12.0, "the soft edge is wholly left of the text");
        assert_eq!(edge(100.0, 100.0, 12.0), 100.0, "and wholly right of it at the end");
        assert!(edge(50.0, 100.0, 12.0) < 50.0 && edge(50.0, 100.0, 12.0) > 40.0);
        // Degenerate widths do not divide by zero.
        assert!(edge(0.0, 0.0, 12.0).is_finite());
    }

    #[test]
    fn plan_starts_where_the_voice_is() {
        let (s, w) = demo();
        let feather = 12.0;
        // Opening the panel 1.5 s into the line: the second word, half way (x = 31.5).
        let p = plan(&s, w, feather, 11_500.0, true, 1.0);
        assert!((p.start - edge(31.5, w, feather)).abs() < 1e-3, "{}", p.start);
        assert_eq!(p.frames[0], (0.0, p.start), "the first key frame is the current position");
        assert_eq!(p.frames.last().unwrap().0, 1.0);
        assert!((p.frames.last().unwrap().1 - edge(w, w, feather)).abs() < 1e-3, "ends with everything lit");
        assert!((p.duration_ms - 1500.0).abs() < 1.0, "1.5 s of the line are left: {}", p.duration_ms);
        // Key frames are strictly increasing in time and never go backwards in value.
        assert!(p.frames.windows(2).all(|f| f[0].0 < f[1].0 && f[0].1 <= f[1].1 + 1e-4), "{:?}", p.frames);
        // What the compositor will show along the way equals the model.
        for ms in [0.0, 250.0, 700.0, 1000.0, 1499.0] {
            let want = edge(x_at(&s, 11_500.0 + ms as f64), w, feather);
            assert!((plan_value_at(&p, ms) - want).abs() < 0.35, "ms={ms}: {} vs {want}", plan_value_at(&p, ms));
        }
    }

    #[test]
    fn plan_from_the_start_covers_the_whole_line() {
        let (s, w) = demo();
        let p = plan(&s, w, 12.0, 10_000.0, true, 1.0);
        assert_eq!(p.start, edge(0.0, w, 12.0));
        assert!((p.duration_ms - 3000.0).abs() < 1.0);
        // A position before the line is the same as its start.
        let early = plan(&s, w, 12.0, 4_000.0, true, 1.0);
        assert_eq!(early.start, p.start);
        assert!((early.duration_ms - 9000.0).abs() < 1.0, "it waits, then sweeps: {}", early.duration_ms);
    }

    #[test]
    fn paused_means_frozen_exactly_where_it_is() {
        let (s, w) = demo();
        let p = plan(&s, w, 12.0, 11_500.0, false, 1.0);
        assert!(p.frames.is_empty() && p.duration_ms == 0.0);
        assert!((p.start - edge(31.5, w, 12.0)).abs() < 1e-3);
        // Resuming from the same position gives the plan with frames again, same start.
        let r = plan(&s, w, 12.0, 11_500.0, true, 1.0);
        assert_eq!(r.start, p.start);
        assert!(!r.frames.is_empty());
    }

    #[test]
    fn finished_or_empty_lines_are_still() {
        let (s, w) = demo();
        let done = plan(&s, w, 12.0, 20_000.0, true, 1.0);
        assert!(done.frames.is_empty());
        assert!((done.start - w).abs() < 1e-3, "everything lit");
        let none = plan(&[], 50.0, 12.0, 0.0, true, 1.0);
        assert!(none.frames.is_empty() && (none.start - 50.0).abs() < 1e-3);
        // A zero or negative rate never animates (and never divides by zero).
        assert!(plan(&s, w, 12.0, 10_100.0, true, 0.0).frames.is_empty());
    }

    #[test]
    fn rate_scales_the_real_duration() {
        let (s, w) = demo();
        let p = plan(&s, w, 12.0, 10_000.0, true, 2.0);
        assert!((p.duration_ms - 1500.0).abs() < 1.0, "twice as fast: {}", p.duration_ms);
        let slow = plan(&s, w, 12.0, 10_000.0, true, 0.5);
        assert!((slow.duration_ms - 6000.0).abs() < 1.0);
    }

    #[test]
    fn a_long_line_stays_within_the_frame_budget() {
        let n = 200u64;
        let segs: Vec<Seg> = (0..n).map(|i| Seg { t0: i * 40, t1: i * 40 + 40, x0: i as f32, x1: i as f32 + 1.0 }).collect();
        let p = plan(&segs, n as f32, 12.0, 0.0, true, 1.0);
        assert!(p.frames.len() <= MAX_FRAMES + 2, "{}", p.frames.len());
        assert!(p.frames.windows(2).all(|f| f[0].0 < f[1].0));
    }

    #[test]
    fn words_that_touch_stay_strictly_ordered_in_time() {
        // Word 2 starts the instant word 1 ends, 5 DIP further on: the jump gets its own 1 ms.
        let segs = [Seg { t0: 0, t1: 1000, x0: 0.0, x1: 10.0 }, Seg { t0: 1000, t1: 2000, x0: 15.0, x1: 25.0 }];
        let p = plan(&segs, 25.0, 12.0, 0.0, true, 1.0);
        assert!(p.frames.windows(2).all(|f| f[0].0 < f[1].0), "{:?}", p.frames);
    }

    #[test]
    fn anchor_extrapolates_only_while_playing() {
        let t0 = Instant::now();
        let a = Anchor { pos_ms: 5_000, at: t0, playing: true, rate: 1.0 };
        let later = t0 + Duration::from_millis(1500);
        assert!((a.pos_at(later) - 6_500.0).abs() < 0.5);
        assert!((Anchor { rate: 1.5, ..a }.pos_at(later) - 7_250.0).abs() < 0.5);
        assert_eq!(Anchor { playing: false, ..a }.pos_at(later), 5_000.0);
        assert_eq!(a.pos_at(t0), 5_000.0);
    }

    #[test]
    fn a_sweep_is_restarted_after_pause_seek_rate_change_or_drift_only() {
        let t0 = Instant::now();
        let a = Anchor { pos_ms: 5_000, at: t0, playing: true, rate: 1.0 };
        let run = start_clock(&a, t0, Duration::from_millis(20));
        assert!((run.start_pos - 5_020.0).abs() < 0.5, "starts a frame ahead");
        let now = t0 + Duration::from_millis(900);
        // The same anchor re-sent: keep.
        assert!(!needs_restart(&run, &a, now));
        // The player re-reports its position a little differently (jitter): keep.
        let jitter = Anchor { pos_ms: 5_900 + 60, at: now, ..a };
        assert!(!needs_restart(&run, &jitter, now));
        // A seek: restart.
        let seek = Anchor { pos_ms: 42_000, at: now, ..a };
        assert!(needs_restart(&run, &seek, now));
        // Pause: restart (freeze). And resume from the frozen position.
        let paused = Anchor { pos_ms: 5_900, at: now, playing: false, rate: 1.0 };
        assert!(needs_restart(&run, &paused, now));
        let frozen = start_clock(&paused, now, Duration::ZERO);
        assert!(!needs_restart(&frozen, &paused, now + Duration::from_secs(30)), "paused for 30 s: still frozen");
        let resumed = Anchor { playing: true, at: now + Duration::from_secs(30), ..paused };
        assert!(needs_restart(&frozen, &resumed, now + Duration::from_secs(30)));
        // Rate change.
        assert!(needs_restart(&run, &Anchor { rate: 1.25, ..a }, now));
    }
}
