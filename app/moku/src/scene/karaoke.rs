//! The word-by-word highlight on the compositor (the scene half of [`crate::ui::karaoke`]).
//!
//! A karaoke line is two sprites in one container, children of the sprite of the block the line
//! belongs to (so they fade and slide in with it when the panel opens):
//!
//! * the **base**: the line in its dim colour;
//! * the **bright** copy on top, drawn through a `CompositionMaskBrush` whose mask is a linear
//!   gradient (opaque, then a soft edge, then transparent). One scalar `X` in a property set
//!   places the edge (the gradient's start / end points are expressions of it) and a single
//!   `ScalarKeyFrameAnimation` on `X`, whose key frames sit on the word boundaries, moves it from
//!   where the voice is now to the end of the line.
//!
//! Both are rasterised once when the line becomes the sung line; afterwards the compositor does
//! all the work, so playing costs this process nothing. A tree rebuilt for any other reason
//! (a clock tick, a pause button) finds the running sweep by the line's id and leaves it alone;
//! it is restarted only when the player's clock says it is off ([`needs_restart`]): a pause, a
//! resume, a seek, a new rate, or drift.
//!
//! With `appearance.animations` reduced or off (or when the mask cannot be built) the line is
//! drawn bright on its own: the look the lyrics had before the highlight existed.

use std::time::{Duration, Instant};

use moku_core::config::Animations;
use windows::UI::Color;
use windows::UI::Composition::*;
use windows::core::{Interface, Result, h};

use super::{Block, Content, Exiting, Island, KeyedOutcome, ms, v2, v3};
use crate::ui::karaoke::{KaraokeLive, RunClock, needs_restart, plan, start_clock};
use crate::ui::layout::{Live, Op};
use crate::ui::TextSpec;

/// The compositor picks an animation up a frame or two after it is started: the sweep is planned
/// for this far ahead of now, so it is not behind.
const LEAD: Duration = Duration::from_millis(20);

/// One karaoke line on screen.
pub(super) struct KaraokeRun {
    /// Index of the block (sprite) it belongs to, and which of that block's karaoke lines it is.
    block: usize,
    ord: usize,
    spec: KaraokeLive,
    container: ContainerVisual,
    /// Where the container sits inside its block sprite.
    offset: (f32, f32),
    /// Present while the line sweeps (animations on); the property set carries the edge position.
    sweep: Option<Sweep>,
}

struct Sweep {
    props: CompositionPropertySet,
    clock: RunClock,
    feather: f32,
}

fn is_karaoke(l: &Live) -> Option<&KaraokeLive> {
    match l {
        Live::Karaoke(k) => Some(k),
        _ => None,
    }
}

/// The live items that are not karaoke lines (those are kept apart: see [`KaraokeLive::same_line`]).
pub(super) fn others(blocks: &[Block]) -> Vec<Live> {
    blocks.iter().flat_map(|b| b.laid.live.iter().filter(|l| is_karaoke(l).is_none()).cloned()).collect()
}

impl Island {
    /// Snapped top-left of a block's sprite (where `paint_sprite` puts it).
    fn block_origin(&self, b: &Block) -> (f32, f32) {
        let s = self.scale;
        ((b.rect.x * s).floor() / s, (b.rect.y * s).floor() / s)
    }

    fn wants_sweep(&self, k: &KaraokeLive) -> bool {
        self.opts.anim == Animations::Full && !k.segs.is_empty() && k.text_w > 1.0
    }

    /// Bring the karaoke lines of `layer` in line with `content`: keep the running ones, replace
    /// the ones whose line changed (the old one leaves, the new one comes in), drop the gone ones.
    /// `animate`: the layer is already on screen, so changes play their transitions.
    pub(super) fn sync_karaoke(&self, sprites: &[SpriteVisual], runs: &mut Vec<KaraokeRun>, content: &Content, animate: bool) -> Result<KeyedOutcome> {
        let mut out = KeyedOutcome::default();
        let now = Instant::now();
        let mut old: Vec<KaraokeRun> = std::mem::take(runs);
        for (blk, b) in content.blocks.iter().enumerate() {
            let Some(sprite) = sprites.get(blk) else { continue };
            let origin = self.block_origin(b);
            for (ord, k) in b.laid.live.iter().filter_map(is_karaoke).enumerate() {
                let found = old.iter().position(|r| r.block == blk && r.ord == ord).map(|i| old.remove(i));
                let want = self.wants_sweep(k);
                match found {
                    Some(mut run) if run.spec.same_line(k) && run.sweep.is_some() == want => {
                        self.follow(&mut run, k, origin, now)?;
                        runs.push(run);
                    }
                    other => {
                        // Another line in this place (or another motion setting): the old one
                        // goes, the new one comes in.
                        let same = other.as_ref().is_some_and(|r| r.spec.same_line(k));
                        if let Some(run) = other {
                            self.leave(run, sprite, &mut out, animate && !same)?;
                        }
                        let run = self.make_run(sprite, blk, ord, origin, k, now)?;
                        if animate && !same {
                            out.busy_ms = out.busy_ms.max(self.enter(&run)?);
                        }
                        runs.push(run);
                    }
                }
            }
        }
        for run in old {
            if let Some(sprite) = sprites.get(run.block) {
                self.leave(run, sprite, &mut out, animate)?;
            }
        }
        Ok(out)
    }

    fn make_run(&self, sprite: &SpriteVisual, block: usize, ord: usize, origin: (f32, f32), k: &KaraokeLive, now: Instant) -> Result<KaraokeRun> {
        let s = self.scale;
        let at = ((k.r.x * s).floor() / s, (k.r.y * s).floor() / s);
        let offset = (at.0 - origin.0, at.1 - origin.1);
        let container = self.c.CreateContainerVisual()?;
        container.SetOffset(v3(offset.0, offset.1))?;
        let sweeping = self.wants_sweep(k);
        // The base: dim while the line sweeps, bright when it does not.
        let base = self.c.CreateSpriteVisual()?;
        let colour = if sweeping { k.dim } else { k.text.color };
        self.paint_surface(&base, k.r, &[Op::Text { r: k.r, t: TextSpec { color: colour, ..k.text.clone() } }])?;
        container.Children()?.InsertAtTop(&base)?;
        let mut run = KaraokeRun { block, ord, spec: k.clone(), container, offset, sweep: None };
        if sweeping {
            match self.make_sweep(&run, k, now) {
                Ok(sw) => run.sweep = Some(sw),
                Err(e) => {
                    // No mask on this machine: the plain bright line, as before the highlight.
                    log_warn!("karaoke: the sweep could not be built ({e}); showing the line bright");
                    run.container.Children()?.RemoveAll()?;
                    let base = self.c.CreateSpriteVisual()?;
                    self.paint_surface(&base, k.r, &[Op::Text { r: k.r, t: k.text.clone() }])?;
                    run.container.Children()?.InsertAtTop(&base)?;
                }
            }
        }
        sprite.Children()?.InsertAtTop(&run.container)?;
        Ok(run)
    }

    /// The bright copy, its mask and the animation from where the voice is now.
    fn make_sweep(&self, run: &KaraokeRun, k: &KaraokeLive, now: Instant) -> Result<Sweep> {
        let c = &self.c;
        let feather = (k.text.size * 0.8).round().max(6.0);
        let bright = c.CreateSpriteVisual()?;
        self.paint_surface(&bright, k.r, &[Op::Text { r: k.r, t: k.text.clone() }])?;
        let white = |a: u8| Color { A: a, R: 255, G: 255, B: 255 };
        let grad = c.CreateLinearGradientBrush()?;
        grad.SetMappingMode(CompositionMappingMode::Absolute)?;
        grad.SetStartPoint(v2(-feather, 0.0))?;
        grad.SetEndPoint(v2(0.0, 0.0))?;
        let stops = grad.ColorStops()?;
        stops.Append(&c.CreateColorGradientStopWithOffsetAndColor(0.0, white(255))?)?;
        stops.Append(&c.CreateColorGradientStopWithOffsetAndColor(1.0, white(0))?)?;
        let props = c.CreatePropertySet()?;
        props.InsertScalar(h!("X"), -feather)?;
        for (prop, expr) in [(h!("StartPoint"), "Vector2(k.X, 0)".to_string()), (h!("EndPoint"), format!("Vector2(k.X + {feather}, 0)"))] {
            let e = c.CreateExpressionAnimationWithExpression(&windows::core::HSTRING::from(expr))?;
            e.SetReferenceParameter(h!("k"), &props)?;
            grad.StartAnimation(prop, &e)?;
        }
        let mask = c.CreateMaskBrush()?;
        mask.SetSource(&bright.Brush()?)?;
        mask.SetMask(&grad)?;
        bright.SetBrush(&mask)?;
        run.container.Children()?.InsertAtTop(&bright)?;
        let mut sw = Sweep { props, clock: start_clock(&k.anchor, now, LEAD), feather };
        self.restart(&mut sw, k, now)?;
        Ok(sw)
    }

    /// (Re)start the edge's animation from the player's clock: where the voice is now to the end
    /// of the line (or just place it, when paused or done).
    fn restart(&self, sw: &mut Sweep, k: &KaraokeLive, now: Instant) -> Result<()> {
        sw.clock = start_clock(&k.anchor, now, LEAD);
        let p = plan(&k.segs, k.text_w, sw.feather, sw.clock.start_pos, k.anchor.playing, k.anchor.rate);
        let _ = sw.props.StopAnimation(h!("X"));
        sw.props.InsertScalar(h!("X"), p.start)?;
        if !p.frames.is_empty() {
            let a = self.c.CreateScalarKeyFrameAnimation()?;
            for &(t, v) in &p.frames {
                a.InsertKeyFrame(t, v)?;
            }
            a.SetDuration(ms(p.duration_ms))?;
            sw.props.StartAnimation(h!("X"), &a)?;
        }
        Ok(())
    }

    /// The same line again in a new tree: move it if its place moved, restart the sweep only if
    /// the player's clock says so.
    fn follow(&self, run: &mut KaraokeRun, k: &KaraokeLive, origin: (f32, f32), now: Instant) -> Result<()> {
        let s = self.scale;
        let at = ((k.r.x * s).floor() / s, (k.r.y * s).floor() / s);
        let offset = (at.0 - origin.0, at.1 - origin.1);
        if offset != run.offset {
            Self::settle_offset(&run.container.cast::<Visual>()?, offset)?;
            run.offset = offset;
        }
        if let Some(sw) = run.sweep.as_mut() {
            if needs_restart(&sw.clock, &k.anchor, now) {
                self.restart(sw, k, now)?;
            }
        }
        run.spec = k.clone();
        Ok(())
    }

    /// A new sung line fades in and rises a little. Returns how long it plays (ms).
    fn enter(&self, run: &KaraokeRun) -> Result<f32> {
        let v: Visual = run.container.cast()?;
        match self.opts.anim {
            Animations::Off => Ok(0.0),
            Animations::Reduced => {
                self.scalar_frames(&v, "Opacity", &[(0.0, 0.0), (1.0, 1.0)], 150.0, 0.0, None)?;
                Ok(150.0)
            }
            Animations::Full => {
                let ease: CompositionEasingFunction = self.ease_out()?.cast()?;
                self.scalar_frames(&v, "Opacity", &[(0.0, 0.0), (1.0, 1.0)], 190.0, 20.0, Some(&ease))?;
                let y = run.offset.1;
                self.scalar_frames(&v, "Offset.Y", &[(0.0, y + run.spec.rise), (1.0, y)], 260.0, 20.0, Some(&ease))?;
                Ok(280.0)
            }
        }
    }

    /// The old line goes: up a little while it fades (or at once when `animate` is off).
    fn leave(&self, run: KaraokeRun, sprite: &SpriteVisual, out: &mut KeyedOutcome, animate: bool) -> Result<()> {
        let v: Visual = run.container.cast()?;
        let parent: ContainerVisual = sprite.cast()?;
        let dur = match (animate, self.opts.anim) {
            (false, _) | (_, Animations::Off) => 0.0,
            (true, Animations::Reduced) => {
                self.scalar_frames(&v, "Opacity", &[(1.0, 0.0)], 120.0, 0.0, None)?;
                120.0
            }
            (true, Animations::Full) => {
                let ease: CompositionEasingFunction = self.ease_out()?.cast()?;
                self.scalar_frames(&v, "Opacity", &[(1.0, 0.0)], 150.0, 0.0, Some(&ease))?;
                self.scalar_frames(&v, "Offset.Y", &[(1.0, run.offset.1 - run.spec.rise * 0.7)], 190.0, 0.0, Some(&ease))?;
                190.0
            }
        };
        if dur <= 0.0 {
            let _ = parent.Children()?.Remove(&v);
        } else {
            out.busy_ms = out.busy_ms.max(dur);
            out.exiting.push(Exiting { visual: v, parent, until: Instant::now() + Duration::from_millis(dur as u64 + 60) });
        }
        Ok(())
    }
}
