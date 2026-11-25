//! The charging battery (`Live::Charge`, from `Node::charging`): a level bar that grows from
//! empty to the current level, a glint that crosses it once and a soft green glow behind the
//! gauge. Everything is a compositor animation (key frames on shape geometry, offsets and
//! opacity) started once and finite: the process does no work while it plays and nothing keeps
//! running afterwards. [`plan`] decides the timing from the animation setting; it is pure, so
//! the numbers are unit-tested without a compositor.

use moku_core::config::Animations;
use windows::UI::Color;
use windows::UI::Composition::*;
use windows::core::{Result, h};
use windows_numerics::{Vector2, Vector3};

use super::Island;
use crate::ui::layout::{Op, bolt_points};
use crate::ui::{Rect, Rgba};

/// How long the charging pill stays up (ms); the animation has to fit in it.
pub const PILL_MS: f32 = 2500.0;

/// What the compositor is asked to do (ms from the moment the layer is created).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ChargePlan {
    /// The level bar waits for the pill to grow and fade in.
    pub fill_delay: f32,
    /// Time the level bar takes to grow to its width; 0 = shown at its final width at once.
    pub fill_ms: f32,
    /// Fade-in instead of growing (reduced motion); 0 otherwise.
    pub fade_ms: f32,
    /// The lightning bolt pops in (scale and opacity); 0 = there from the start.
    pub bolt_delay: f32,
    pub bolt_ms: f32,
    /// Number of glints across the level bar, one after the other (0 = none).
    pub glints: u32,
    pub glint_start: f32,
    pub glint_ms: f32,
    /// The glow fades in, breathes `breaths` times and rests at `glow_rest`. With no breaths
    /// it simply sits at `glow_rest`.
    pub glow_in_ms: f32,
    pub glow_delay: f32,
    pub breaths: u32,
    pub breath_ms: f32,
    pub glow_rest: f32,
}

impl ChargePlan {
    /// When the last animation ends (ms).
    pub fn total_ms(&self) -> f32 {
        let fill = (self.fill_delay + self.fill_ms.max(self.fade_ms)).max(self.bolt_delay + self.bolt_ms);
        let glint = if self.glints > 0 { self.glint_start + self.glint_ms * self.glints as f32 } else { 0.0 };
        let glow = if self.breaths > 0 { self.glow_delay + self.glow_in_ms + self.breath_ms * self.breaths as f32 } else { 0.0 };
        fill.max(glint).max(glow)
    }
}

/// Timing for a level bar that grows to `fill` (0-1 of the track) under the animation setting.
pub fn plan(fill: f32, anim: Animations) -> ChargePlan {
    let f = if fill.is_nan() { 0.0 } else { fill.clamp(0.0, 1.0) };
    let still = ChargePlan { fill_delay: 0.0, fill_ms: 0.0, fade_ms: 0.0, bolt_delay: 0.0, bolt_ms: 0.0, glints: 0, glint_start: 0.0, glint_ms: 0.0, glow_in_ms: 0.0, glow_delay: 0.0, breaths: 0, breath_ms: 0.0, glow_rest: 0.7 };
    match anim {
        Animations::Off => still,
        Animations::Reduced => ChargePlan { fade_ms: 150.0, ..still },
        Animations::Full => {
            // Longer fills take a little longer, so 10 % does not crawl and 100 % does not rush.
            let fill_ms = 450.0 + 650.0 * f;
            let fill_delay = 260.0;
            ChargePlan {
                fill_delay,
                fill_ms,
                fade_ms: 0.0,
                bolt_delay: 170.0,
                bolt_ms: 380.0,
                glints: 1,
                glint_start: fill_delay + fill_ms * 0.55,
                glint_ms: 640.0,
                glow_in_ms: 450.0,
                glow_delay: 120.0,
                breaths: 2,
                breath_ms: 950.0,
                glow_rest: 0.8,
            }
        }
    }
}

/// Opacity key frames `(ms, value)` of the glow: fade in, breathe, rest.
pub fn glow_frames(p: &ChargePlan) -> Vec<(f32, f32)> {
    if p.breaths == 0 {
        return vec![(0.0, p.glow_rest)];
    }
    let mut v = vec![(0.0, 0.0), (p.glow_in_ms, 1.0)];
    let mut t = p.glow_in_ms;
    for _ in 0..p.breaths {
        v.push((t + p.breath_ms * 0.5, 0.55));
        t += p.breath_ms;
        v.push((t, 1.0));
    }
    // Settle at the resting level.
    v.push((t + 250.0, p.glow_rest));
    v
}

fn ms(t: f32) -> windows::Foundation::TimeSpan {
    windows::Foundation::TimeSpan { Duration: (t * 10_000.0) as i64 }
}

fn v2(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

fn argb(c: Rgba, k: f32) -> Color {
    let u = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color { A: u(c.a * k), R: u(c.r), G: u(c.g), B: u(c.b) }
}

/// `c` moved toward white by `k` (0 = unchanged).
fn lighten(c: Rgba, k: f32) -> Rgba {
    Rgba { r: c.r + (1.0 - c.r) * k, g: c.g + (1.0 - c.g) * k, b: c.b + (1.0 - c.b) * k, a: c.a }
}

/// `c` moved toward black by `k`.
fn darken(c: Rgba, k: f32) -> Rgba {
    Rgba { r: c.r * (1.0 - k), g: c.g * (1.0 - k), b: c.b * (1.0 - k), a: c.a }
}

impl Island {
    /// The visuals of one `Live::Charge`: the glow goes into `under` (below the pill's drawn
    /// content), the level bar and the glint into `over`.
    #[allow(clippy::too_many_arguments)] // the fields of `Live::Charge`, spelled out
    pub(super) fn make_charge(&self, under: &ContainerVisual, over: &ContainerVisual, r: Rect, inner: Rect, radius: f32, fill_w: f32, bolt_r: Rect, col: Rgba) -> Result<()> {
        let c = &self.c;
        let p = plan(if inner.w > 0.0 { fill_w / inner.w } else { 0.0 }, self.opts.anim);
        let animated = self.opts.anim == Animations::Full;
        let ease = c.CreateCubicBezierEasingFunction(v2(0.2, 0.8), v2(0.2, 1.0))?;

        // ---- soft glow: a radial gradient (the colour fading to nothing) around the gauge.
        let glow = c.CreateSpriteVisual()?;
        let (gw, gh) = (r.w + 60.0, r.h + 38.0);
        glow.SetSize(v2(gw, gh))?;
        glow.SetOffset(Vector3 { X: r.cx() - gw / 2.0, Y: r.cy() - gh / 2.0, Z: 0.0 })?;
        let radial = c.CreateRadialGradientBrush()?;
        radial.SetEllipseCenter(v2(0.5, 0.5))?;
        radial.SetEllipseRadius(v2(0.5, 0.5))?;
        for (o, a) in [(0.0, 0.50), (0.45, 0.20), (1.0, 0.0)] {
            radial.ColorStops()?.Append(&c.CreateColorGradientStopWithOffsetAndColor(o, argb(col, a))?)?;
        }
        glow.SetBrush(&radial)?;
        let frames = glow_frames(&p);
        if animated && frames.len() > 1 {
            let total = frames.last().map_or(1.0, |f| f.0).max(1.0);
            let a = c.CreateScalarKeyFrameAnimation()?;
            for &(t, v) in &frames {
                if t <= 0.0 {
                    a.InsertKeyFrame(0.0, v)?;
                } else {
                    a.InsertKeyFrameWithEasingFunction((t / total).min(1.0), v, &c.CreateCubicBezierEasingFunction(v2(0.3, 0.0), v2(0.3, 1.0))?)?;
                }
            }
            a.SetDuration(ms(total))?;
            a.SetDelayTime(ms(p.glow_delay))?;
            a.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
            glow.StartAnimation(h!("Opacity"), &a)?;
        } else {
            glow.SetOpacity(p.glow_rest)?;
        }
        under.Children()?.InsertAtTop(&glow)?;

        // ---- level bar + glint, clipped to the track (so nothing leaks over the outline).
        let clip_geo = c.CreateRoundedRectangleGeometry()?;
        clip_geo.SetSize(v2(inner.w, inner.h))?;
        clip_geo.SetCornerRadius(v2(radius, radius))?;
        let bar = c.CreateShapeVisual()?;
        bar.SetSize(v2(inner.w, inner.h))?;
        bar.SetOffset(Vector3 { X: inner.x, Y: inner.y, Z: 0.0 })?;
        bar.SetClip(&c.CreateGeometricClipWithGeometry(&clip_geo)?)?;
        let geo = c.CreateRoundedRectangleGeometry()?;
        geo.SetCornerRadius(v2(radius, radius))?;
        geo.SetSize(v2(fill_w, inner.h))?;
        let shape = c.CreateSpriteShapeWithGeometry(&geo)?;
        let grad = c.CreateLinearGradientBrush()?;
        grad.SetStartPoint(v2(0.0, 0.0))?;
        grad.SetEndPoint(v2(1.0, 0.0))?;
        grad.ColorStops()?.Append(&c.CreateColorGradientStopWithOffsetAndColor(0.0, argb(darken(col, 0.12), 1.0))?)?;
        grad.ColorStops()?.Append(&c.CreateColorGradientStopWithOffsetAndColor(1.0, argb(lighten(col, 0.28), 1.0))?)?;
        shape.SetFillBrush(&grad)?;
        bar.Shapes()?.Append(&shape)?;
        if animated && p.fill_ms > 0.0 {
            let a = c.CreateVector2KeyFrameAnimation()?;
            a.InsertKeyFrame(0.0, v2(0.0, inner.h))?;
            a.InsertKeyFrameWithEasingFunction(1.0, v2(fill_w, inner.h), &ease)?;
            a.SetDuration(ms(p.fill_ms))?;
            a.SetDelayTime(ms(p.fill_delay))?;
            a.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
            geo.StartAnimation(h!("Size"), &a)?;
        } else if self.opts.anim == Animations::Reduced && p.fade_ms > 0.0 {
            let a = c.CreateScalarKeyFrameAnimation()?;
            a.InsertKeyFrame(0.0, 0.0)?;
            a.InsertKeyFrame(1.0, 1.0)?;
            a.SetDuration(ms(p.fade_ms))?;
            bar.StartAnimation(h!("Opacity"), &a)?;
        }
        over.Children()?.InsertAtTop(&bar)?;

        // ---- the lightning bolt: rasterised once (a few pixels), popped in by the compositor.
        if bolt_r.w > 0.0 && bolt_r.h > 0.0 {
            let bolt = c.CreateSpriteVisual()?;
            let (bx, by) = self.paint_surface(&bolt, bolt_r, &[Op::Poly { pts: bolt_points(bolt_r), color: Rgba::hex(0xffffff, 1.0) }])?;
            bolt.SetOffset(Vector3 { X: bx, Y: by, Z: 0.0 })?;
            if animated && p.bolt_ms > 0.0 {
                let back = c.CreateCubicBezierEasingFunction(v2(0.34, 1.45), v2(0.64, 1.0))?;
                let sc = c.CreateVector3KeyFrameAnimation()?;
                sc.InsertKeyFrame(0.0, Vector3 { X: 0.3, Y: 0.3, Z: 1.0 })?;
                sc.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: 1.0, Y: 1.0, Z: 1.0 }, &back)?;
                sc.SetDuration(ms(p.bolt_ms))?;
                sc.SetDelayTime(ms(p.bolt_delay))?;
                sc.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
                bolt.StartAnimation(h!("Scale"), &sc)?;
                let op = c.CreateScalarKeyFrameAnimation()?;
                op.InsertKeyFrame(0.0, 0.0)?;
                op.InsertKeyFrameWithEasingFunction(1.0, 1.0, &ease)?;
                op.SetDuration(ms(p.bolt_ms * 0.6))?;
                op.SetDelayTime(ms(p.bolt_delay))?;
                op.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
                bolt.StartAnimation(h!("Opacity"), &op)?;
            } else if self.opts.anim == Animations::Reduced && p.fade_ms > 0.0 {
                let op = c.CreateScalarKeyFrameAnimation()?;
                op.InsertKeyFrame(0.0, 0.0)?;
                op.InsertKeyFrame(1.0, 1.0)?;
                op.SetDuration(ms(p.fade_ms))?;
                bolt.StartAnimation(h!("Opacity"), &op)?;
            }
            over.Children()?.InsertAtTop(&bolt)?;
        }

        if animated && p.glints > 0 && fill_w > 8.0 {
            let band = 18.0;
            let glint = c.CreateShapeVisual()?;
            glint.SetSize(v2(inner.w, inner.h))?;
            glint.SetOffset(Vector3 { X: inner.x, Y: inner.y, Z: 0.0 })?;
            glint.SetClip(&c.CreateGeometricClipWithGeometry(&clip_geo)?)?;
            glint.SetOpacity(0.0)?;
            let rect = c.CreateRectangleGeometry()?;
            rect.SetSize(v2(band, inner.h))?;
            let s = c.CreateSpriteShapeWithGeometry(&rect)?;
            let g = c.CreateLinearGradientBrush()?;
            g.SetStartPoint(v2(0.0, 0.0))?;
            g.SetEndPoint(v2(1.0, 0.0))?;
            for (o, al) in [(0.0, 0.0), (0.5, 0.5), (1.0, 0.0)] {
                g.ColorStops()?.Append(&c.CreateColorGradientStopWithOffsetAndColor(o, Color { A: (al * 255.0) as u8, R: 255, G: 255, B: 255 })?)?;
            }
            s.SetFillBrush(&g)?;
            glint.Shapes()?.Append(&s)?;
            for i in 0..p.glints {
                let start = p.glint_start + p.glint_ms * i as f32;
                let slide = c.CreateVector2KeyFrameAnimation()?;
                slide.InsertKeyFrame(0.0, v2(-band, 0.0))?;
                slide.InsertKeyFrameWithEasingFunction(1.0, v2(fill_w - band * 0.5, 0.0), &c.CreateCubicBezierEasingFunction(v2(0.4, 0.0), v2(0.6, 1.0))?)?;
                slide.SetDuration(ms(p.glint_ms))?;
                slide.SetDelayTime(ms(start))?;
                slide.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
                s.StartAnimation(h!("Offset"), &slide)?;
                let fade = c.CreateScalarKeyFrameAnimation()?;
                fade.InsertKeyFrame(0.0, 0.0)?;
                fade.InsertKeyFrame(0.2, 1.0)?;
                fade.InsertKeyFrame(0.75, 1.0)?;
                fade.InsertKeyFrame(1.0, 0.0)?;
                fade.SetDuration(ms(p.glint_ms))?;
                fade.SetDelayTime(ms(start))?;
                fade.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
                glint.StartAnimation(h!("Opacity"), &fade)?;
            }
            over.Children()?.InsertAtTop(&glint)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_plan_grows_then_glints_and_fits_the_pill() {
        let p = plan(0.48, Animations::Full);
        assert!(p.fill_ms > 0.0 && p.fill_delay > 0.0, "the bar waits for the pill, then grows: {p:?}");
        assert!(p.glints == 1 && p.glint_start > p.fill_delay, "the glint crosses the bar while it finishes: {p:?}");
        assert!(p.breaths > 0);
        assert!(p.total_ms() <= PILL_MS + 100.0, "everything is over about when the pill leaves: {}", p.total_ms());
        // Always finite: no looping forever.
        assert!(p.total_ms().is_finite());
    }

    #[test]
    fn the_bolt_pops_in_early_and_only_with_full_motion() {
        let f = plan(0.48, Animations::Full);
        assert!(f.bolt_ms > 0.0 && f.bolt_delay < f.fill_delay + f.fill_ms, "{f:?}");
        for a in [Animations::Reduced, Animations::Off] {
            assert_eq!(plan(0.48, a).bolt_ms, 0.0, "{a:?}: no pop");
        }
    }

    #[test]
    fn longer_fills_take_longer_but_never_too_long() {
        let (a, b, c) = (plan(0.1, Animations::Full), plan(0.5, Animations::Full), plan(1.0, Animations::Full));
        assert!(a.fill_ms < b.fill_ms && b.fill_ms < c.fill_ms);
        assert!(c.fill_ms <= 1200.0 && a.fill_ms >= 450.0);
        // Out-of-range input is clamped, NaN counts as empty.
        assert_eq!(plan(5.0, Animations::Full), plan(1.0, Animations::Full));
        assert_eq!(plan(f32::NAN, Animations::Full), plan(0.0, Animations::Full));
    }

    #[test]
    fn reduced_fades_and_off_is_instant() {
        let r = plan(0.8, Animations::Reduced);
        assert_eq!((r.fill_ms, r.glints, r.breaths), (0.0, 0, 0));
        assert!(r.fade_ms > 0.0 && r.fade_ms <= 180.0);
        let o = plan(0.8, Animations::Off);
        assert_eq!((o.fill_ms, o.fade_ms, o.glints, o.breaths), (0.0, 0.0, 0, 0));
        assert_eq!(o.total_ms(), 0.0);
        assert!(o.glow_rest > 0.0, "the glow still shows, steady");
    }

    #[test]
    fn glow_fades_in_breathes_and_settles() {
        let p = plan(0.5, Animations::Full);
        let f = glow_frames(&p);
        assert_eq!(f.first(), Some(&(0.0, 0.0)));
        assert!(f.windows(2).all(|w| w[0].0 < w[1].0), "key frames move forward in time: {f:?}");
        assert_eq!(f.last().map(|x| x.1), Some(p.glow_rest));
        assert!(f.iter().any(|&(_, v)| v < 1.0 && v > 0.0), "it breathes");
        // Steady glow when not animating.
        assert_eq!(glow_frames(&plan(0.5, Animations::Off)), vec![(0.0, 0.7)]);
    }
}
