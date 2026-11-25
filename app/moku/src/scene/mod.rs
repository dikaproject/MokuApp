//! The Windows.UI.Composition visual tree and its animations (grown from the prototype).
//!
//! All geometry is driven by one `CompositionPropertySet` (`p`) holding W, H (main shape), T
//! (0 = pill, 1 = panel) and B (split-bubble diameter, 0 = hidden). Only those scalars are
//! animated, always by springs; every size, clip, corner radius, rim, shadow and offset is
//! bound to them with ExpressionAnimations, so the morph runs inside the compositor (DWM) with
//! no per-frame work in this process.
//!
//! Content arrives as laid-out [`Content`] (from the UI kit). Each [`Block`] becomes one sprite
//! whose surface is rasterised only when the block's ops change.

pub mod charge;
pub mod gfx;
pub mod reduce;
pub mod spring;

use std::time::{Duration, Instant};

use moku_core::config::{Animations, Material};
use windows::Foundation::{IReference, PropertyValue, TimeSpan};
use windows::UI::Color;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::*;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::core::{HSTRING, Interface, Result, h};
use windows_numerics::{Vector2, Vector3};

use crate::ui::layout::{Keyed, Laid, Live, Op};
use crate::ui::{Anim, KeySpec, Pulse, Rect};
use gfx::Gfx;
use spring::{Spring, motion::Motion};

/// Visual window size in DIP: widest/tallest shape plus room for the shadow.
pub const WIN_W: f32 = 784.0;
pub const WIN_H: f32 = 540.0;
/// Gap between the screen top and the island (dynamic style).
pub const TOP: f32 = 8.0;
/// Notch style: the shape extends this far above the screen edge so its top corners are hidden.
pub const NOTCH_EXTRA: f32 = 20.0;
pub const BUBBLE: f32 = 36.0;
pub const BUBBLE_GAP: f32 = 6.0;
pub const PANEL_W: f32 = 640.0;
pub const PANEL_RADIUS: f32 = 38.0;
/// Panel body max height (tab bar and paddings excluded).
pub const PANEL_BODY_MAX_H: f32 = 360.0;

/// Corner radius as a function of height: a full pill until the shape is panel-sized.
pub fn radius(h: f32) -> f32 {
    (h / 2.0).min(PANEL_RADIUS)
}

/// One rasterised piece of content (one sprite).
#[derive(Clone, PartialEq, Debug)]
pub struct Block {
    /// Position / size in content coordinates (DIP).
    pub rect: Rect,
    /// Ops in content coordinates.
    pub laid: Laid,
}

/// What the main shape (or the bubble) shows.
#[derive(Clone, PartialEq, Debug)]
pub struct Content {
    /// Identity: same key â†’ update in place; new key â†’ cross-fade.
    pub key: String,
    pub size: (f32, f32),
    pub blocks: Vec<Block>,
    pub panel: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SceneOpts {
    pub blur: bool,
    pub material: Material,
    pub notch: bool,
    pub anim: Animations,
}

/// Physical-pixel hit rectangles relative to the visual window.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct HitShape {
    pub main: (i32, i32, i32, i32),
    pub main_radius: i32,
    /// Extra height hidden above the screen top (notch style), physical px.
    pub notch: i32,
    pub bubble: Option<(i32, i32, i32, i32)>,
}

fn v2(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}
fn v3(x: f32, y: f32) -> Vector3 {
    Vector3 { X: x, Y: y, Z: 0.0 }
}
fn ms(t: f32) -> TimeSpan {
    TimeSpan { Duration: (t * 10_000.0) as i64 }
}
fn color(hex: u32, a: f32) -> Color {
    Color { A: (a * 255.0).round() as u8, R: (hex >> 16) as u8, G: (hex >> 8) as u8, B: hex as u8 }
}
fn boxed(v: f32) -> Result<IReference<f32>> {
    PropertyValue::CreateSingle(v)?.cast()
}
fn delay_of(a: &Anim) -> f32 {
    match a {
        Anim::Fade { delay, .. } | Anim::Pop { delay, .. } | Anim::Into { delay, .. } => *delay,
    }
}

struct Layer {
    key: String,
    container: ContainerVisual,
    /// Parent of the sprites (a width gate for panels).
    inner: ContainerVisual,
    /// Children of `inner`, bottom to top: glows that sit behind the content (`under_root`), the
    /// block sprites, compositor-animated live items (`live_root`: bars, spinner, charging battery, text caret) and
    /// the keyed sprites.
    under_root: ContainerVisual,
    blocks_root: ContainerVisual,
    live_root: ContainerVisual,
    keyed_root: ContainerVisual,
    sprites: Vec<SpriteVisual>,
    keyed: Vec<KeyedSprite>,
    content: Content,
    /// How many `Live` items have visuals in `under_root` / `live_root` (bars, spinner, charging
    /// battery, text caret; they are removed together by `clear_live`, so no handles are kept).
    live_n: usize,
}

/// The sprite of one keyed node (`Node::key`), kept across trees by its key.
struct KeyedSprite {
    sprite: SpriteVisual,
    /// Snapped top-left in content coordinates (the sprite's resting offset).
    at: (f32, f32),
    rect: Rect,
    ops: Vec<Op>,
    spec: KeySpec,
    /// Index of the block it came from (stagger when a panel opens).
    block: usize,
}

/// A keyed sprite playing its exit animation; removed from `parent` once `until` has passed.
struct Exiting {
    visual: Visual,
    parent: ContainerVisual,
    until: Instant,
}

/// What a keyed diff left to the island: exits to collect and how long things will move.
#[derive(Default)]
struct KeyedOutcome {
    exiting: Vec<Exiting>,
    busy_ms: f32,
}

pub struct Island {
    c: Compositor,
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    pub gfx: Gfx,
    props: CompositionPropertySet,
    /// Running springs for W, H, T, B.
    springs: [Spring; 4],
    body: Option<ContainerVisual>,
    bubble_body: Option<ContainerVisual>,
    main: Option<Layer>,
    bubble: Option<Layer>,
    retiring: Vec<(ContainerVisual, Instant)>,
    /// Keyed sprites playing an exit animation.
    exiting: Vec<Exiting>,
    /// A panel layer was retired: trim drawing memory once it is gone (see [`Island::gc`]).
    trim_pending: bool,
    /// Off-tree visuals that must stay alive (the shadow mask source).
    keep: Vec<Visual>,
    scale: f32,
    opts: SceneOpts,
    /// Targets of W, H, T, B.
    target: [f32; 4],
    busy_until: Instant,
    /// Where the island is relative to its resting place ([`Island::set_away`]).
    away: reduce::Away,
}

const KEYS: [&str; 4] = ["W", "H", "T", "B"];

impl Island {
    pub fn new(hwnd: HWND, scale: f32, opts: SceneOpts) -> Result<Self> {
        let c = Compositor::new()?;
        // Hosts the visual tree directly on our HWND (needs a DispatcherQueue on this thread).
        let target = unsafe { c.cast::<ICompositorDesktopInterop>()?.CreateDesktopWindowTarget(hwnd, true)? };
        let root = c.CreateContainerVisual()?;
        target.SetRoot(&root)?;
        let gfx = Gfx::new(&c)?;
        let props = c.CreatePropertySet()?;
        let mut me = Self {
            c,
            _target: target,
            root,
            gfx,
            props,
            springs: [Spring::rest(0.0); 4],
            body: None,
            bubble_body: None,
            main: None,
            bubble: None,
            retiring: vec![],
            exiting: vec![],
            trim_pending: false,
            keep: vec![],
            scale,
            opts,
            target: [120.0, 36.0, 0.0, 0.0],
            busy_until: Instant::now(),
            away: reduce::Away::Home,
        };
        me.build()?;
        Ok(me)
    }

    pub fn away(&self) -> reduce::Away {
        self.away
    }

    /// Step the whole island out of the way (or back): its opacity and horizontal offset move to
    /// the end state of [`reduce::away_plan`] on the compositor, from wherever they are now (an
    /// interrupted move continues). Moves the visuals only; the caller lets clicks through.
    pub fn set_away(&mut self, away: reduce::Away, anim: Animations) -> Result<()> {
        let plan = reduce::away_plan(away, anim);
        let root: Visual = self.root.cast()?;
        let x = plan.dx * self.scale;
        if plan.ms <= 0.0 {
            for p in [h!("Opacity"), h!("Offset.X")] {
                let _ = root.StopAnimation(p);
            }
            root.SetOpacity(plan.opacity)?;
            root.SetOffset(v3(x, 0.0))?;
        } else {
            let ease: CompositionEasingFunction = self.ease_out()?.cast()?;
            self.scalar_frames(&root, "Opacity", &[(1.0, plan.opacity)], plan.ms, 0.0, Some(&ease))?;
            self.scalar_frames(&root, "Offset.X", &[(1.0, x)], plan.ms, 0.0, Some(&ease))?;
            self.busy_until = self.busy_until.max(Instant::now() + Duration::from_millis(plan.ms as u64 + 100));
        }
        self.away = away;
        Ok(())
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn opts(&self) -> SceneOpts {
        self.opts
    }

    /// Rebuild everything for a new scale / options, re-showing the current content instantly.
    pub fn rebuild(&mut self, scale: f32, opts: SceneOpts) -> Result<()> {
        self.scale = scale;
        self.opts = opts;
        let main = self.main.take().map(|l| l.content);
        let bubble = self.bubble.take().map(|l| l.content);
        self.build()?;
        if let Some(m) = main {
            let shape = (self.target[0], self.target[1]);
            let anim = self.opts.anim;
            self.opts.anim = Animations::Off;
            let r = self.present(m, shape, bubble, spring::motion::PILL);
            self.opts.anim = anim;
            r?;
        }
        Ok(())
    }

    /// True while a transition may still be moving.
    pub fn animating(&self) -> bool {
        Instant::now() < self.busy_until
    }

    /// Hit-test shapes for the *target* geometry (physical px, relative to the visual window).
    pub fn hit_shape(&self) -> HitShape {
        let s = self.scale;
        let [w, h, _, b] = self.target;
        let gap = b.min(BUBBLE_GAP);
        let top = if self.opts.notch { 0.0 } else { TOP };
        let x = ((WIN_W - w - b - gap) * 0.5 * s).round() as i32;
        let y = (top * s).round() as i32;
        let (pw, ph) = ((w * s).round() as i32, (h * s).round() as i32);
        let notch = if self.opts.notch { (NOTCH_EXTRA * s).round() as i32 } else { 0 };
        let rad = (radius(h + if self.opts.notch { NOTCH_EXTRA } else { 0.0 }) * s).round() as i32;
        let bubble = (b > 0.5).then(|| {
            let bx = x + pw + (gap * s).round() as i32;
            let by = y + ((h - b) * 0.5 * s).round() as i32;
            let bs = (b * s).round() as i32;
            (bx, by, bs, bs)
        });
        HitShape { main: (x, y, pw, ph), main_radius: rad, notch, bubble }
    }

    // ---- expression helpers -------------------------------------------------------------

    fn bind<T: Interface>(&self, obj: &T, prop: &str, expr: &str) -> Result<()> {
        let a = self.c.CreateExpressionAnimationWithExpression(&HSTRING::from(expr))?;
        a.SetReferenceParameter(h!("p"), &self.props)?;
        a.SetScalarParameter(h!("s"), self.scale)?;
        obj.cast::<CompositionObject>()?.StartAnimation(&HSTRING::from(prop), &a)
    }

    fn notch(&self) -> f32 {
        if self.opts.notch { NOTCH_EXTRA } else { 0.0 }
    }

    /// Height expression of the drawn shape (taller than H by the notch extra).
    fn hh(&self) -> String {
        format!("(p.H + {})", self.notch())
    }

    /// Rounded rectangle that tracks the main shape (size snapped to physical pixels), inset by
    /// `inset` DIP.
    fn shape_geometry(&self, inset: f32) -> Result<CompositionRoundedRectangleGeometry> {
        let g = self.c.CreateRoundedRectangleGeometry()?;
        g.SetOffset(v2(inset, inset))?;
        let i2 = 2.0 * inset;
        let hh = self.hh();
        self.bind(&g, "Size", &format!("Vector2(Round(p.W * s) / s - {i2}, Round({hh} * s) / s - {i2})"))?;
        let r = format!("Min({hh} * 0.5, {PANEL_RADIUS}) - {inset}");
        self.bind(&g, "CornerRadius", &format!("Vector2({r}, {r})"))?;
        Ok(g)
    }

    fn bubble_geometry(&self, inset: f32) -> Result<CompositionRoundedRectangleGeometry> {
        let g = self.c.CreateRoundedRectangleGeometry()?;
        g.SetOffset(v2(inset, inset))?;
        let i2 = 2.0 * inset;
        self.bind(&g, "Size", &format!("Vector2(Max(p.B - {i2}, 0), Max(p.B - {i2}, 0))"))?;
        self.bind(&g, "CornerRadius", &format!("Vector2(Max(p.B * 0.5 - {inset}, 0), Max(p.B * 0.5 - {inset}, 0))"))?;
        Ok(g)
    }

    fn rim(&self, geo: CompositionRoundedRectangleGeometry, px: f32) -> Result<ShapeVisual> {
        let c = &self.c;
        let rim = c.CreateShapeVisual()?;
        let rs = c.CreateSpriteShapeWithGeometry(&geo)?;
        let grad = c.CreateLinearGradientBrush()?;
        grad.SetStartPoint(v2(0.3, 0.0))?;
        grad.SetEndPoint(v2(0.7, 1.0))?;
        for (o, a) in [(0.0, 0.55), (0.5, 0.18), (1.0, 0.25)] {
            grad.ColorStops()?.Append(&c.CreateColorGradientStopWithOffsetAndColor(o, color(0xffffff, a))?)?;
        }
        rs.SetStrokeBrush(&grad)?;
        rs.SetStrokeThickness(px)?;
        rim.Shapes()?.Append(&rs)?;
        Ok(rim)
    }

    fn tint(&self) -> Result<SpriteVisual> {
        let tint = self.c.CreateSpriteVisual()?;
        tint.SetRelativeSizeAdjustment(v2(1.0, 1.0))?;
        let (hex, a0, a1) = match (self.opts.material, self.opts.blur) {
            (Material::Black, _) => (0x000000, 0.90, 0.95),
            // Spec: 47 % / 52 % over a real blur. Without blur the tint carries legibility on
            // its own: 72 % pill, 88 % panel.
            (Material::Dark, true) => (0x0B0B14, 0.47, 0.52),
            (Material::Dark, false) => (0x0B0B14, 0.72, 0.88),
        };
        tint.SetBrush(&self.c.CreateColorBrushWithColor(color(hex, 1.0))?)?;
        self.bind(&tint, "Opacity", &format!("Lerp({a0}, {a1}, p.T)"))?;
        Ok(tint)
    }

    // ---- static tree --------------------------------------------------------------------

    fn build(&mut self) -> Result<()> {
        let c = &self.c.clone();
        let s = self.scale;
        self.root.Children()?.RemoveAll()?;
        self.retiring.clear();
        self.exiting.clear();
        self.root.SetScale(Vector3 { X: s, Y: s, Z: 1.0 })?;
        for (i, k) in KEYS.iter().enumerate() {
            let k = HSTRING::from(*k);
            let _ = self.props.StopAnimation(&k);
            self.props.InsertScalar(&k, self.target[i])?;
            self.springs[i] = Spring::rest(self.target[i]);
        }
        let n = self.notch();
        let top = if self.opts.notch { 0.0 } else { TOP };
        let hh = self.hh();

        let island = c.CreateContainerVisual()?;
        self.bind(&island, "Offset", &format!("Vector3(Round(({WIN_W} - p.W - p.B - Min(p.B, {BUBBLE_GAP})) * 0.5 * s) / s, {}, 0)", top - n))?;
        self.root.Children()?.InsertAtTop(&island)?;
        let bind_size = |v: &Visual| self.bind(v, "Size", &format!("Vector2(p.W, {hh})"));

        // Shadow: a brush-less SpriteVisual whose DropShadow takes its shape from a mask (a
        // CompositionVisualSurface of an off-tree ShapeVisual of the morphing rounded rect), so
        // nothing opaque is drawn under the translucent glass.
        let caster = c.CreateShapeVisual()?;
        bind_size(&caster.cast()?)?;
        let cs = c.CreateSpriteShapeWithGeometry(&self.shape_geometry(0.0)?)?;
        cs.SetFillBrush(&c.CreateColorBrushWithColor(color(0, 1.0))?)?;
        caster.Shapes()?.Append(&cs)?;
        let mask_surface = c.CreateVisualSurface()?;
        mask_surface.SetSourceVisual(&caster)?;
        self.bind(&mask_surface, "SourceSize", &format!("Vector2(p.W, {hh})"))?;
        let mask = c.CreateSurfaceBrushWithSurface(&mask_surface)?;
        mask.SetStretch(CompositionStretch::Fill)?;
        let shadow = c.CreateDropShadow()?;
        shadow.SetColor(color(0, 1.0))?;
        shadow.SetMask(&mask)?;
        self.bind(&shadow, "BlurRadius", "Lerp(30, 60, p.T)")?;
        self.bind(&shadow, "Offset", "Vector3(0, Lerp(10, 24, p.T), 0)")?;
        self.bind(&shadow, "Opacity", "Lerp(0.42, 0.5, p.T)")?;
        let shadow_visual = c.CreateSpriteVisual()?;
        bind_size(&shadow_visual.cast()?)?;
        shadow_visual.SetShadow(&shadow)?;
        island.Children()?.InsertAtTop(&shadow_visual)?;
        let keep: Visual = caster.cast()?;

        // Glass body, clipped to the morphing rounded rectangle.
        let body = c.CreateContainerVisual()?;
        bind_size(&body.cast()?)?;
        body.SetClip(&c.CreateGeometricClipWithGeometry(&self.shape_geometry(0.0)?)?)?;
        if self.opts.blur {
            // DWM's blurred "host backdrop" (what acrylic uses); needs DWMWA_USE_HOSTBACKDROPBRUSH.
            let backdrop = c.CreateSpriteVisual()?;
            backdrop.SetRelativeSizeAdjustment(v2(1.0, 1.0))?;
            backdrop.SetBrush(&c.CreateHostBackdropBrush()?)?;
            body.Children()?.InsertAtTop(&backdrop)?;
        }
        body.Children()?.InsertAtTop(&self.tint()?)?;
        island.Children()?.InsertAtTop(&body)?;

        // Rim: a stroke of whole physical pixels (â‰¥ 1 px, â‰ˆ 1 DIP) on pixel centres, so it stays
        // sharp at 125 % / 175 %. Near-vertical gradient: white 55 % top â†’ 18 % â†’ 25 % bottom.
        let px = s.round().max(1.0) / s;
        let rim = self.rim(self.shape_geometry(px / 2.0)?, px)?;
        bind_size(&rim.cast()?)?;
        island.Children()?.InsertAtTop(&rim)?;

        // Split bubble: a second small glass circle to the right of the main shape.
        let bub = c.CreateContainerVisual()?;
        self.bind(
            &bub,
            "Offset",
            &format!(
                "Vector3(Round(({WIN_W} - p.W - p.B - Min(p.B, {BUBBLE_GAP})) * 0.5 * s) / s + Round(p.W * s) / s + Min(p.B, {BUBBLE_GAP}), {top} + Round((p.H - p.B) * 0.5 * s) / s, 0)"
            ),
        )?;
        self.bind(&bub, "Size", "Vector2(p.B, p.B)")?;
        self.bind(&bub, "Opacity", &format!("Clamp(p.B / {BUBBLE}, 0, 1)"))?;
        let bub_body = c.CreateContainerVisual()?;
        self.bind(&bub_body, "Size", "Vector2(p.B, p.B)")?;
        bub_body.SetClip(&c.CreateGeometricClipWithGeometry(&self.bubble_geometry(0.0)?)?)?;
        bub_body.Children()?.InsertAtTop(&self.tint()?)?;
        bub.Children()?.InsertAtTop(&bub_body)?;
        let brim = self.rim(self.bubble_geometry(px / 2.0)?, px)?;
        self.bind(&brim, "Size", "Vector2(p.B, p.B)")?;
        bub.Children()?.InsertAtTop(&brim)?;
        self.root.Children()?.InsertAtTop(&bub)?;

        self.keep = vec![keep];
        self.body = Some(body);
        self.bubble_body = Some(bub_body);
        Ok(())
    }

    // ---- content ------------------------------------------------------------------------

    /// A sprite showing `block`, drawn once.
    fn sprite(&self, block: &crate::scene::Block) -> Result<SpriteVisual> {
        let v = self.c.CreateSpriteVisual()?;
        self.paint_sprite(&v, block)?;
        Ok(v)
    }

    fn paint_sprite(&self, v: &SpriteVisual, block: &Block) -> Result<()> {
        let (x, y) = self.paint_surface(v, block.rect, &block.laid.ops)?;
        v.SetOffset(v3(x, y))
    }

    /// Rasterise `ops` (content coordinates) into `v`'s brush, sized to `r` snapped to whole
    /// pixels; returns the snapped top-left (the offset the sprite belongs at).
    fn paint_surface(&self, v: &SpriteVisual, r: Rect, ops: &[Op]) -> Result<(f32, f32)> {
        let s = self.scale;
        // Snap the sprite to whole pixels so the surface maps 1:1 and text stays crisp.
        let (x, y) = ((r.x * s).floor() / s, (r.y * s).floor() / s);
        let (w, h) = (((r.x + r.w) * s).ceil() / s - x, ((r.y + r.h) * s).ceil() / s - y);
        let gfx = &self.gfx;
        let surf = gfx.surface(w.max(1.0 / s), h.max(1.0 / s), s, |ctx| gfx.translated(ctx, -x, -y, || gfx.paint(ctx, ops)))?;
        let brush = self.c.CreateSurfaceBrushWithSurface(&surf)?;
        brush.SetStretch(CompositionStretch::Fill)?;
        v.SetBrush(&brush)?;
        v.SetSize(v2(w, h))?;
        // Scale / rotation (pulses, enter / exit) turn around the centre.
        v.SetCenterPoint(Vector3 { X: w / 2.0, Y: h / 2.0, Z: 0.0 })?;
        Ok((x, y))
    }

    // ---- keyed sprites -------------------------------------------------------------------

    /// A new keyed sprite at rest in `parent` (no animation).
    fn keyed_sprite(&self, parent: &ContainerVisual, k: &Keyed, block: usize) -> Result<KeyedSprite> {
        let sprite = self.c.CreateSpriteVisual()?;
        let at = self.paint_surface(&sprite, k.rect, &k.ops)?;
        sprite.SetOffset(v3(at.0, at.1))?;
        parent.Children()?.InsertAtTop(&sprite)?;
        Ok(KeyedSprite { sprite, at, rect: k.rect, ops: k.ops.clone(), spec: k.spec.clone(), block })
    }

    fn ease_out(&self) -> Result<CubicBezierEasingFunction> {
        self.c.CreateCubicBezierEasingFunction(v2(0.2, 0.8), v2(0.2, 1.0))
    }

    /// Scalar key frames on `prop` (`frames`: progress â†’ value), optional delay.
    fn scalar_frames(&self, v: &Visual, prop: &str, frames: &[(f32, f32)], dur: f32, delay: f32, ease: Option<&CompositionEasingFunction>) -> Result<()> {
        let a = self.c.CreateScalarKeyFrameAnimation()?;
        for &(t, val) in frames {
            match ease {
                Some(e) if t > 0.0 => a.InsertKeyFrameWithEasingFunction(t, val, e)?,
                _ => a.InsertKeyFrame(t, val)?,
            }
        }
        a.SetDuration(ms(dur))?;
        a.SetDelayTime(ms(delay))?;
        a.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
        v.StartAnimation(&HSTRING::from(prop), &a)
    }

    fn vector_frames(&self, v: &Visual, prop: &str, frames: &[(f32, Vector3)], dur: f32, delay: f32, ease: Option<&CompositionEasingFunction>) -> Result<()> {
        let a = self.c.CreateVector3KeyFrameAnimation()?;
        for &(t, val) in frames {
            match ease {
                Some(e) if t > 0.0 => a.InsertKeyFrameWithEasingFunction(t, val, e)?,
                _ => a.InsertKeyFrame(t, val)?,
            }
        }
        a.SetDuration(ms(dur))?;
        a.SetDelayTime(ms(delay))?;
        a.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
        v.StartAnimation(&HSTRING::from(prop), &a)
    }

    /// Put a keyed sprite at its resting offset now, cancelling any move in flight.
    fn settle_offset(v: &Visual, at: (f32, f32)) -> Result<()> {
        for p in [h!("Offset"), h!("Offset.X"), h!("Offset.Y")] {
            let _ = v.StopAnimation(p);
        }
        v.SetOffset(v3(at.0, at.1))
    }

    /// A key that was not in the previous tree. Returns how long it plays (ms).
    fn play_enter(&self, ks: &KeyedSprite) -> Result<f32> {
        let a = ks.spec.enter.clone().unwrap_or_else(Anim::fade);
        let v: Visual = ks.sprite.cast()?;
        match self.opts.anim {
            Animations::Off => return Ok(0.0),
            Animations::Reduced => {
                let (dur, delay) = (a.total() - delay_of(&a), delay_of(&a));
                self.scalar_frames(&v, "Opacity", &[(0.0, 0.0), (1.0, 1.0)], dur.min(180.0), delay, None)?;
                return Ok(dur.min(180.0) + delay);
            }
            Animations::Full => {}
        }
        let ease: CompositionEasingFunction = self.ease_out()?.cast()?;
        match &a {
            Anim::Pop { ms: dur, delay, from_scale, dy } => {
                let back: CompositionEasingFunction = self.c.CreateCubicBezierEasingFunction(v2(0.34, 1.45), v2(0.64, 1.0))?.cast()?;
                self.scalar_frames(&v, "Opacity", &[(0.0, 0.0), (1.0, 1.0)], dur * 0.6, *delay, Some(&ease))?;
                let s0 = Vector3 { X: *from_scale, Y: *from_scale, Z: 1.0 };
                self.vector_frames(&v, "Scale", &[(0.0, s0), (1.0, Vector3 { X: 1.0, Y: 1.0, Z: 1.0 })], *dur, *delay, Some(&back))?;
                self.scalar_frames(&v, "Offset.Y", &[(0.0, ks.at.1 + dy), (1.0, ks.at.1)], *dur, *delay, Some(&ease))?;
            }
            Anim::Fade { ms: dur, delay } | Anim::Into { ms: dur, delay, .. } => {
                self.scalar_frames(&v, "Opacity", &[(0.0, 0.0), (1.0, 1.0)], *dur, *delay, Some(&ease))?;
            }
        }
        Ok(a.total())
    }

    /// A key that is gone: animate it out (`target` = centre of the element an `Anim::Into`
    /// flies into). Returns how long it plays (ms); 0 = remove it now.
    fn play_exit(&self, ks: &KeyedSprite, target: Option<(f32, f32)>) -> Result<f32> {
        let a = ks.spec.exit.clone().unwrap_or_else(|| Anim::fade().ms(160.0));
        let v: Visual = ks.sprite.cast()?;
        match self.opts.anim {
            Animations::Off => return Ok(0.0),
            Animations::Reduced => {
                let delay = delay_of(&a);
                self.scalar_frames(&v, "Opacity", &[(1.0, 0.0)], 160.0, delay, None)?;
                return Ok(160.0 + delay);
            }
            Animations::Full => {}
        }
        let ease: CompositionEasingFunction = self.ease_out()?.cast()?;
        match (&a, target) {
            (Anim::Into { ms: dur, delay, .. }, Some((tx, ty))) => {
                Self::settle_offset(&v, ks.at)?;
                let size = ks.sprite.Size()?;
                // Quadratic curve from here to the target, bowed upwards a little, sampled with
                // an ease-in (things accelerate into the bin).
                let p0 = (ks.at.0, ks.at.1);
                let p2 = (tx - size.X / 2.0, ty - size.Y / 2.0);
                let dist = ((p2.0 - p0.0).powi(2) + (p2.1 - p0.1).powi(2)).sqrt();
                let p1 = ((p0.0 + p2.0) / 2.0, p0.1.min(p2.1) - (dist * 0.12).min(28.0));
                let frames: Vec<(f32, Vector3)> = (0..=6)
                    .map(|i| {
                        let t = i as f32 / 6.0;
                        let u = t * (0.55 + 0.45 * t); // gentle ease-in, ends at 1
                        let q = 1.0 - u;
                        let x = q * q * p0.0 + 2.0 * q * u * p1.0 + u * u * p2.0;
                        let y = q * q * p0.1 + 2.0 * q * u * p1.1 + u * u * p2.1;
                        (t, v3(x, y))
                    })
                    .collect();
                self.vector_frames(&v, "Offset", &frames, *dur, *delay, None)?;
                // Shrinks steadily on the way, stays opaque until it is almost in, then vanishes.
                let shrink: CompositionEasingFunction = self.c.CreateCubicBezierEasingFunction(v2(0.4, 0.0), v2(0.7, 0.9))?.cast()?;
                let s1 = Vector3 { X: 0.2, Y: 0.2, Z: 1.0 };
                self.vector_frames(&v, "Scale", &[(0.0, Vector3 { X: 1.0, Y: 1.0, Z: 1.0 }), (1.0, s1)], *dur, *delay, Some(&shrink))?;
                self.scalar_frames(&v, "Opacity", &[(0.0, 1.0), (0.72, 1.0), (1.0, 0.0)], *dur, *delay, None)?;
            }
            (Anim::Pop { ms: dur, delay, from_scale, dy }, _) => {
                let s1 = Vector3 { X: *from_scale, Y: *from_scale, Z: 1.0 };
                self.vector_frames(&v, "Scale", &[(1.0, s1)], *dur, *delay, Some(&ease))?;
                self.scalar_frames(&v, "Offset.Y", &[(1.0, ks.at.1 + dy)], *dur, *delay, Some(&ease))?;
                self.scalar_frames(&v, "Opacity", &[(1.0, 0.0)], dur * 0.8, *delay, Some(&ease))?;
            }
            (a, _) => {
                let (dur, delay) = (a.total() - delay_of(a), delay_of(a));
                self.scalar_frames(&v, "Opacity", &[(1.0, 0.0)], dur, delay, Some(&ease))?;
            }
        }
        Ok(a.total())
    }

    /// Glide a keyed sprite from wherever it is now to `to` (springs per axis, so a move that
    /// is interrupted continues from where it was).
    fn play_move(&self, ks: &KeyedSprite, to: (f32, f32)) -> Result<f32> {
        let v: Visual = ks.sprite.cast()?;
        if self.opts.anim != Animations::Full {
            Self::settle_offset(&v, to)?;
            return Ok(0.0);
        }
        let _ = v.StopAnimation(h!("Offset"));
        // Same model as the morph springs: period = Î¶Â·settle / 4 (see `start_springs`).
        let (damping, settle) = (0.86f32, 320.0f32);
        for (prop, val) in [(h!("Offset.X"), to.0), (h!("Offset.Y"), to.1)] {
            let a = self.c.CreateSpringScalarAnimation()?;
            a.SetDampingRatio(damping)?;
            a.SetPeriod(ms(damping * settle / 4.0))?;
            a.SetFinalValue(&boxed(val)?)?;
            v.StartAnimation(prop, &a)?;
        }
        Ok(settle)
    }

    fn play_pulse(&self, ks: &KeyedSprite) -> Result<f32> {
        if self.opts.anim != Animations::Full {
            return Ok(0.0);
        }
        let v: Visual = ks.sprite.cast()?;
        let one = Vector3 { X: 1.0, Y: 1.0, Z: 1.0 };
        let sc = |k: f32| Vector3 { X: k, Y: k, Z: 1.0 };
        let ease: CompositionEasingFunction = self.ease_out()?.cast()?;
        Ok(match ks.spec.pulse_kind {
            Pulse::Pop => {
                self.vector_frames(&v, "Scale", &[(0.0, one), (0.35, sc(1.12)), (0.7, sc(0.97)), (1.0, one)], 360.0, 0.0, Some(&ease))?;
                360.0
            }
            Pulse::Wiggle => {
                self.scalar_frames(&v, "RotationAngleInDegrees", &[(0.0, 0.0), (0.18, -14.0), (0.38, 12.0), (0.58, -8.0), (0.78, 5.0), (1.0, 0.0)], 420.0, 0.0, Some(&ease))?;
                self.vector_frames(&v, "Scale", &[(0.0, one), (0.3, sc(1.16)), (1.0, one)], 420.0, 0.0, Some(&ease))?;
                420.0
            }
        })
    }

    /// Match the keyed nodes of `content` against `layer`'s keyed sprites by key: repaint the
    /// changed ones, glide the moved ones, pulse, animate new keys in and gone keys out.
    fn diff_keyed(&self, layer: &mut Layer, content: &Content) -> Result<KeyedOutcome> {
        let mut out = KeyedOutcome::default();
        let new: Vec<(usize, &Keyed)> = content.blocks.iter().enumerate().flat_map(|(i, b)| b.laid.keyed.iter().map(move |k| (i, k))).collect();
        let mut old: std::collections::HashMap<String, KeyedSprite> = std::collections::HashMap::new();
        for ks in layer.keyed.drain(..) {
            // A key used twice: keep the first, take the other off the tree (never orphaned).
            if old.contains_key(&ks.spec.key) {
                let _ = layer.keyed_root.Children()?.Remove(&ks.sprite);
                continue;
            }
            old.insert(ks.spec.key.clone(), ks);
        }
        let mut next = Vec::with_capacity(new.len());
        let mut gone = vec![];
        for &(block, k) in &new {
            match old.remove(&k.spec.key) {
                // Leaving: exit with this tree's spec (its place in the layout stays empty).
                Some(mut ks) if k.spec.leaving => {
                    ks.spec = k.spec.clone();
                    gone.push(ks);
                }
                // Already gone (or never shown): nothing to draw.
                None if k.spec.leaving => {}
                Some(mut ks) => {
                    let at = if ks.ops != k.ops || ks.rect.w != k.rect.w || ks.rect.h != k.rect.h {
                        let at = self.paint_surface(&ks.sprite, k.rect, &k.ops)?;
                        ks.ops = k.ops.clone();
                        at
                    } else {
                        let s = self.scale;
                        ((k.rect.x * s).floor() / s, (k.rect.y * s).floor() / s)
                    };
                    if at != ks.at {
                        out.busy_ms = out.busy_ms.max(self.play_move(&ks, at)?);
                        ks.at = at;
                    }
                    let pulse = k.spec.pulse != ks.spec.pulse;
                    ks.rect = k.rect;
                    ks.spec = k.spec.clone();
                    ks.block = block;
                    if pulse {
                        out.busy_ms = out.busy_ms.max(self.play_pulse(&ks)?);
                    }
                    next.push(ks);
                }
                None if next.iter().any(|n: &KeyedSprite| n.spec.key == k.spec.key) => {} // duplicate key in one tree
                None => {
                    let ks = self.keyed_sprite(&layer.keyed_root, k, block)?;
                    out.busy_ms = out.busy_ms.max(self.play_enter(&ks)?);
                    next.push(ks);
                }
            }
        }
        for ks in old.into_values().chain(gone) {
            let target = match &ks.spec.exit {
                Some(Anim::Into { target, .. }) => new.iter().find(|(_, k)| k.spec.key == *target).map(|(_, k)| (k.rect.cx(), k.rect.cy())),
                _ => None,
            };
            let dur = self.play_exit(&ks, target)?;
            let visual: Visual = ks.sprite.cast()?;
            if dur <= 0.0 {
                let _ = layer.keyed_root.Children()?.Remove(&visual);
            } else {
                out.busy_ms = out.busy_ms.max(dur);
                out.exiting.push(Exiting { visual, parent: layer.keyed_root.clone(), until: Instant::now() + Duration::from_millis(dur as u64 + 60) });
            }
        }
        layer.keyed = next;
        Ok(out)
    }

    /// A text caret: a bar snapped to whole pixels, blinking in the compositor (Windows caret
    /// blink time; steady when animations are off or the user turned blinking off).
    fn make_caret(&self, layer: &ContainerVisual, r: Rect, col: crate::ui::Rgba) -> Result<()> {
        let s = self.scale;
        let v = self.c.CreateSpriteVisual()?;
        v.SetBrush(&self.c.CreateColorBrushWithColor(Color { A: (col.a * 255.0) as u8, R: (col.r * 255.0) as u8, G: (col.g * 255.0) as u8, B: (col.b * 255.0) as u8 })?)?;
        let w = (r.w * s).round().max(1.0) / s;
        v.SetSize(v2(w, (r.h * s).round() / s))?;
        v.SetOffset(v3((r.x * s).round() / s, (r.y * s).round() / s))?;
        let blink = unsafe { windows::Win32::UI::WindowsAndMessaging::GetCaretBlinkTime() };
        if self.opts.anim != Animations::Off && blink != 0 && blink != u32::MAX {
            let half = blink.clamp(200, 1200) as f32;
            let a = self.c.CreateScalarKeyFrameAnimation()?;
            let lin = self.c.CreateLinearEasingFunction()?;
            // On for `half` ms, off for `half` ms (≈ 10 ms edges); starts visible, so a caret
            // re-created after typing or moving is solid first.
            for (t, o) in [(0.0, 1.0), (0.49, 1.0), (0.5, 0.0), (0.99, 0.0), (1.0, 1.0)] {
                a.InsertKeyFrameWithEasingFunction(t, o, &lin)?;
            }
            a.SetDuration(ms(2.0 * half))?;
            a.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
            v.StartAnimation(h!("Opacity"), &a)?;
        }
        layer.Children()?.InsertAtTop(&v)?;
        Ok(())
    }

    /// Visuals for the live items of a layer (`under`: behind the content, `over`: above it).
    fn make_live(&self, under: &ContainerVisual, over: &ContainerVisual, live: &[Live]) -> Result<usize> {
        for l in live {
            let (r, playing, col) = match l {
                Live::Bars { r, playing, color } => (r, playing, color),
                Live::Spinner { r, color, thickness } => {
                    self.make_spinner(over, r, color, *thickness)?;
                    continue;
                }
                Live::Caret { r, color } => {
                    self.make_caret(over, *r, *color)?;
                    continue;
                }
                Live::Charge { r, inner, radius, fill_w, bolt, color } => {
                    self.make_charge(under, over, *r, *inner, *radius, *fill_w, *bolt, *color)?;
                    continue;
                }
            };
            let bars = self.c.CreateShapeVisual()?;
            bars.SetSize(v2(r.w, r.h))?;
            bars.SetOffset(v3(r.x, r.y))?;
            let brush = self.c.CreateColorBrushWithColor(Color {
                A: (col.a * 255.0) as u8,
                R: (col.r * 255.0) as u8,
                G: (col.g * 255.0) as u8,
                B: (col.b * 255.0) as u8,
            })?;
            let k = r.h / 20.0;
            let step = r.w / 5.0;
            for (i, bar_h) in [9.0f32, 15.0, 19.0, 12.0, 16.0].into_iter().enumerate() {
                let bh = bar_h * k;
                let x = i as f32 * step + (step - 3.0) / 2.0;
                let geo = self.c.CreateRoundedRectangleGeometry()?;
                geo.SetSize(v2(3.0, bh))?;
                geo.SetCornerRadius(v2(1.5, 1.5))?;
                geo.SetOffset(v2(x, (r.h - bh) / 2.0))?;
                let shape = self.c.CreateSpriteShapeWithGeometry(&geo)?;
                shape.SetFillBrush(&brush)?;
                shape.SetCenterPoint(v2(x + 1.5, r.h / 2.0))?;
                if *playing && self.opts.anim != Animations::Off {
                    // Looping key-frame animation that runs entirely in the compositor.
                    let a = self.c.CreateVector2KeyFrameAnimation()?;
                    let lo = [0.45, 0.6, 0.4, 0.7, 0.5][i];
                    a.InsertKeyFrame(0.0, v2(1.0, 1.0))?;
                    a.InsertKeyFrame(0.5, v2(1.0, lo))?;
                    a.InsertKeyFrame(1.0, v2(1.0, 1.0))?;
                    a.SetDuration(ms([620.0, 840.0, 700.0, 930.0, 760.0][i]))?;
                    a.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
                    shape.StartAnimation(h!("Scale"), &a)?;
                } else if !*playing {
                    shape.SetScale(v2(1.0, 0.35))?;
                }
                bars.Shapes()?.Append(&shape)?;
            }
            over.Children()?.InsertAtTop(&bars)?;
        }
        Ok(live.len())
    }

    fn clear_live(layer: &mut Layer) -> Result<()> {
        layer.under_root.Children()?.RemoveAll()?;
        layer.live_root.Children()?.RemoveAll()?;
        layer.live_n = 0;
        Ok(())
    }

    /// A short arc (28 % of a circle) turning once every 0.9 s on the compositor thread.
    fn make_spinner(&self, layer: &ContainerVisual, r: &crate::ui::Rect, col: &crate::ui::Rgba, thickness: f32) -> Result<()> {
        let sv = self.c.CreateShapeVisual()?;
        sv.SetSize(v2(r.w, r.h))?;
        sv.SetOffset(v3(r.x, r.y))?;
        let brush = self.c.CreateColorBrushWithColor(Color { A: (col.a * 255.0) as u8, R: (col.r * 255.0) as u8, G: (col.g * 255.0) as u8, B: (col.b * 255.0) as u8 })?;
        let rad = ((r.w.min(r.h) - thickness) / 2.0).max(1.0);
        let geo = self.c.CreateEllipseGeometry()?;
        geo.SetCenter(v2(r.w / 2.0, r.h / 2.0))?;
        geo.SetRadius(v2(rad, rad))?;
        geo.SetTrimStart(0.0)?;
        geo.SetTrimEnd(0.28)?;
        let shape = self.c.CreateSpriteShapeWithGeometry(&geo)?;
        shape.SetStrokeBrush(&brush)?;
        shape.SetStrokeThickness(thickness)?;
        shape.SetStrokeStartCap(CompositionStrokeCap::Round)?;
        shape.SetStrokeEndCap(CompositionStrokeCap::Round)?;
        shape.SetCenterPoint(v2(r.w / 2.0, r.h / 2.0))?;
        if self.opts.anim != Animations::Off {
            // Looping linear rotation: runs entirely in the compositor.
            let a = self.c.CreateScalarKeyFrameAnimation()?;
            let linear = self.c.CreateLinearEasingFunction()?;
            a.InsertKeyFrameWithEasingFunction(1.0, 360.0, &linear)?;
            a.SetDuration(ms(900.0))?;
            a.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
            shape.StartAnimation(h!("RotationAngleInDegrees"), &a)?;
        }
        sv.Shapes()?.Append(&shape)?;
        layer.Children()?.InsertAtTop(&sv)?;
        Ok(())
    }

    fn content_offset_expr(&self, content: &Content, bubble: bool) -> String {
        let (w, h) = content.size;
        if bubble {
            return format!("Vector3(Round((p.B - {w}) * 0.5 * s) / s, Round((p.B - {h}) * 0.5 * s) / s, 0)");
        }
        let n = self.notch();
        let y = if content.panel { format!("{n}") } else { format!("Round((p.H - {h}) * 0.5 * s) / s + {n}") };
        format!("Vector3(Round((p.W - {w}) * 0.5 * s) / s, {y}, 0)")
    }

    fn new_layer(&self, content: Content, bubble: bool) -> Result<Layer> {
        let container = self.c.CreateContainerVisual()?;
        container.SetSize(v2(content.size.0, content.size.1))?;
        self.bind(&container, "Offset", &self.content_offset_expr(&content, bubble))?;
        let inner = self.c.CreateContainerVisual()?;
        if content.panel {
            // Panel blocks stay hidden until the shape is â‰¥ 90 % wide, so nothing is shown
            // clipped at the edges (on open, close, and interrupted transitions).
            let w = content.size.0;
            self.bind(&inner, "Opacity", &format!("Clamp((p.W - {}) / {}, 0, 1)", w * 0.9, w * 0.06))?;
        }
        container.Children()?.InsertAtTop(&inner)?;
        let (under_root, blocks_root, live_root, keyed_root) = (self.c.CreateContainerVisual()?, self.c.CreateContainerVisual()?, self.c.CreateContainerVisual()?, self.c.CreateContainerVisual()?);
        for root in [&under_root, &blocks_root, &live_root, &keyed_root] {
            inner.Children()?.InsertAtTop(root)?;
        }
        let mut sprites = vec![];
        let mut keyed = vec![];
        for (i, b) in content.blocks.iter().enumerate() {
            let v = self.sprite(b)?;
            blocks_root.Children()?.InsertAtTop(&v)?;
            sprites.push(v);
            for k in b.laid.keyed.iter().filter(|k| !k.spec.leaving) {
                keyed.push(self.keyed_sprite(&keyed_root, k, i)?);
            }
        }
        let live: Vec<Live> = content.blocks.iter().flat_map(|b| b.laid.live.clone()).collect();
        let live_n = self.make_live(&under_root, &live_root, &live)?;
        Ok(Layer { key: content.key.clone(), container, inner, under_root, blocks_root, live_root, keyed_root, sprites, keyed, content, live_n })
    }

    /// Same key: redraw only the blocks that changed; diff the keyed sprites by key.
    fn update_layer(&self, layer: &mut Layer, content: Content, bubble: bool) -> Result<KeyedOutcome> {
        if layer.content == content {
            return Ok(KeyedOutcome::default());
        }
        if content.size != layer.content.size || content.panel != layer.content.panel {
            layer.container.SetSize(v2(content.size.0, content.size.1))?;
            self.bind(&layer.container, "Offset", &self.content_offset_expr(&content, bubble))?;
        }
        if content.blocks.len() == layer.sprites.len() {
            for (i, b) in content.blocks.iter().enumerate() {
                if layer.content.blocks[i].rect != b.rect || layer.content.blocks[i].laid.ops != b.laid.ops {
                    self.paint_sprite(&layer.sprites[i], b)?;
                }
            }
        } else {
            layer.blocks_root.Children()?.RemoveAll()?;
            layer.sprites.clear();
            Self::clear_live(layer)?;
            for b in &content.blocks {
                let v = self.sprite(b)?;
                layer.blocks_root.Children()?.InsertAtTop(&v)?;
                layer.sprites.push(v);
            }
        }
        let outcome = self.diff_keyed(layer, &content)?;
        let old_live: Vec<Live> = layer.content.blocks.iter().flat_map(|b| b.laid.live.clone()).collect();
        let live: Vec<Live> = content.blocks.iter().flat_map(|b| b.laid.live.clone()).collect();
        if old_live != live || layer.live_n == 0 && !live.is_empty() {
            Self::clear_live(layer)?;
            layer.live_n = self.make_live(&layer.under_root, &layer.live_root, &live)?;
        }
        layer.content = content;
        Ok(outcome)
    }

    fn fade(&self, v: &Visual, from: Option<f32>, to: f32, dur: f32, delay: f32) -> Result<()> {
        if self.opts.anim == Animations::Off {
            v.StopAnimation(h!("Opacity"))?;
            return v.SetOpacity(to);
        }
        let a = self.c.CreateScalarKeyFrameAnimation()?;
        if let Some(f) = from {
            a.InsertKeyFrame(0.0, f)?;
        }
        let ease = self.c.CreateCubicBezierEasingFunction(v2(0.2, 0.0), v2(0.0, 1.0))?;
        a.InsertKeyFrameWithEasingFunction(1.0, to, &ease)?;
        a.SetDuration(ms(dur))?;
        a.SetDelayTime(ms(delay))?;
        a.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
        v.StartAnimation(h!("Opacity"), &a)
    }

    fn retire(&mut self, layer: Layer, parent_is_bubble: bool) -> Result<()> {
        if layer.content.panel {
            self.trim_pending = true;
        }
        let v: Visual = layer.container.cast()?;
        if self.opts.anim == Animations::Off {
            let parent = if parent_is_bubble { &self.bubble_body } else { &self.body };
            if let Some(p) = parent {
                let _ = p.Children()?.Remove(&v);
            }
            return Ok(());
        }
        self.fade(&v, None, 0.0, 110.0, 0.0)?;
        self.retiring.push((layer.container, Instant::now()));
        Ok(())
    }

    /// Remove a layer from the tree right away (no fade). Best effort.
    fn discard(&self, layer: &Layer, parent_is_bubble: bool) {
        let parent = if parent_is_bubble { &self.bubble_body } else { &self.body };
        if let (Some(p), Ok(v)) = (parent, layer.container.cast::<Visual>())
            && let Ok(ch) = p.Children()
        {
            let _ = ch.Remove(&v);
        }
    }

    /// Retired layers or a pending trim are waiting for [`Island::gc`].
    pub fn needs_gc(&self) -> bool {
        !self.retiring.is_empty() || !self.exiting.is_empty() || self.trim_pending
    }

    /// Drop retired layers whose fade-out is over (their sprites and surfaces go with them).
    /// After a panel layer is gone, give the drawing memory back ([`Gfx::trim`]), so a closed
    /// panel does not keep its surfaces. Called on every present and by the app's GC timer.
    pub fn gc(&mut self) -> Result<()> {
        self.gc_layers()?;
        if self.retiring.is_empty() && self.trim_pending && self.main.as_ref().is_none_or(|l| !l.content.panel) {
            self.trim_pending = false;
            self.gfx.trim();
        }
        Ok(())
    }

    fn gc_layers(&mut self) -> Result<()> {
        let now = Instant::now();
        let (body, bub) = (self.body.clone(), self.bubble_body.clone());
        let mut keep = vec![];
        for (v, t) in self.retiring.drain(..) {
            if now.duration_since(t) > Duration::from_millis(500) {
                let vis: Visual = v.cast()?;
                for p in [&body, &bub].into_iter().flatten() {
                    let _ = p.Children()?.Remove(&vis);
                }
            } else {
                keep.push((v, t));
            }
        }
        self.retiring = keep;
        // Keyed sprites whose exit animation is over (their layer may be gone already: removing
        // from a detached parent is harmless).
        let (done, still): (Vec<Exiting>, Vec<Exiting>) = self.exiting.drain(..).partition(|e| now >= e.until);
        self.exiting = still;
        for e in done {
            let _ = e.parent.Children()?.Remove(&e.visual);
        }
        Ok(())
    }

    /// Show `main` in a shape of size `shape` (â‰¥ content size for pills; the content is
    /// centred), and `bubble` in the split bubble. `motion` = spring for the size change.
    pub fn present(&mut self, main: Content, shape: (f32, f32), bubble: Option<Content>, motion: Motion) -> Result<()> {
        self.gc()?;
        let to_panel = main.panel;
        let from_panel = self.main.as_ref().is_some_and(|l| l.content.panel);
        let body = self.body.clone().expect("built");

        // Main content.
        match self.main.take() {
            Some(mut l) if l.key == main.key && l.content.panel == main.panel => {
                // On error the layer is half-updated: take it off the tree (not just forget it,
                // which would leave an orphaned visual on screen); the next present rebuilds it.
                match self.update_layer(&mut l, main, false) {
                    Ok(o) => {
                        self.main = Some(l);
                        self.keyed_outcome(o);
                    }
                    Err(e) => {
                        self.discard(&l, false);
                        return Err(e);
                    }
                }
            }
            old => {
                if let Some(o) = old {
                    self.retire(o, false)?;
                }
                let layer = self.new_layer(main, false)?;
                let v: Visual = layer.container.cast()?;
                body.Children()?.InsertAtTop(&v)?;
                if to_panel && self.opts.anim == Animations::Full && !from_panel {
                    // Panel blocks: fade + 8 DIP slide up, 40 ms stagger, starting about when
                    // the width gate opens.
                    for (i, sp) in layer.sprites.iter().enumerate() {
                        let y = sp.Offset()?.Y;
                        let delay = 130.0 + 40.0 * i as f32;
                        self.fade(&sp.cast()?, Some(0.0), 1.0, 220.0, delay)?;
                        let slide = self.c.CreateScalarKeyFrameAnimation()?;
                        slide.InsertKeyFrame(0.0, y + 8.0)?;
                        let ease = self.c.CreateCubicBezierEasingFunction(v2(0.2, 0.8), v2(0.2, 1.0))?;
                        slide.InsertKeyFrameWithEasingFunction(1.0, y, &ease)?;
                        slide.SetDuration(ms(300.0))?;
                        slide.SetDelayTime(ms(delay))?;
                        slide.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
                        sp.StartAnimation(h!("Offset.Y"), &slide)?;
                    }
                    // Keyed sprites come in with the block they belong to.
                    let ease: CompositionEasingFunction = self.ease_out()?.cast()?;
                    for ks in &layer.keyed {
                        let delay = 130.0 + 40.0 * ks.block as f32;
                        let kv: Visual = ks.sprite.cast()?;
                        self.fade(&kv, Some(0.0), 1.0, 220.0, delay)?;
                        self.scalar_frames(&kv, "Offset.Y", &[(0.0, ks.at.1 + 8.0), (1.0, ks.at.1)], 300.0, delay, Some(&ease))?;
                    }
                } else {
                    // Coming out of the panel, wait for the shape to shrink before showing the pill.
                    let delay = if from_panel && !to_panel { 150.0 } else if to_panel { 40.0 } else { 60.0 };
                    let dur = if self.opts.anim == Animations::Reduced { 150.0 } else { 200.0 };
                    self.fade(&v, Some(0.0), 1.0, dur, delay)?;
                }
                self.main = Some(layer);
            }
        }

        // Bubble (hidden while the panel is open).
        let bubble = bubble.filter(|_| !to_panel);
        let bub_parent = self.bubble_body.clone().expect("built");
        let b_target = if bubble.is_some() { BUBBLE } else { 0.0 };
        match (self.bubble.take(), bubble) {
            (Some(mut l), Some(b)) if l.key == b.key => match self.update_layer(&mut l, b, true) {
                Ok(o) => {
                    self.bubble = Some(l);
                    self.keyed_outcome(o);
                }
                Err(e) => {
                    self.discard(&l, true);
                    return Err(e);
                }
            },
            (old, new) => {
                if let Some(o) = old {
                    self.retire(o, true)?;
                }
                if let Some(b) = new {
                    let layer = self.new_layer(b, true)?;
                    let v: Visual = layer.container.cast()?;
                    bub_parent.Children()?.InsertAtTop(&v)?;
                    self.fade(&v, Some(0.0), 1.0, 200.0, 80.0)?;
                    self.bubble = Some(layer);
                }
            }
        }

        let targets = [shape.0, shape.1, if to_panel { 1.0 } else { 0.0 }, b_target];
        if targets != self.target {
            self.start_springs(targets, motion)?;
        }
        Ok(())
    }

    fn keyed_outcome(&mut self, o: KeyedOutcome) {
        self.exiting.extend(o.exiting);
        if o.busy_ms > 0.0 {
            self.busy_until = self.busy_until.max(Instant::now() + Duration::from_millis(o.busy_ms as u64 + 100));
        }
    }

    /// Retarget W, H, T, B with springs, handing over the velocity of any spring still in
    /// flight so interrupted transitions don't hitch.
    fn start_springs(&mut self, targets: [f32; 4], motion: Motion) -> Result<()> {
        self.target = targets;
        // "Kurangi" gets a short spring without overshoot (the content fades); "Mati" jumps.
        let Some((damping, settle_ms)) = reduce::spring(self.opts.anim, motion) else {
            for (i, k) in KEYS.iter().enumerate() {
                let k = HSTRING::from(*k);
                let _ = self.props.StopAnimation(&k);
                self.props.InsertScalar(&k, targets[i])?;
                self.springs[i] = Spring::rest(targets[i]);
            }
            self.busy_until = self.busy_until.max(Instant::now() + Duration::from_millis(250));
            return Ok(());
        };
        // Settle time (2 % band) â‰ˆ 4 / (Î¶Â·Ï‰). Composition's `Period` behaves as 1/Ï‰ (measured),
        // so period = Î¶Â·settle / 4.
        let period = damping * settle_ms / 4.0;
        let omega = 1000.0 / period;
        for (i, k) in KEYS.iter().enumerate() {
            let (x, v) = self.springs[i].now();
            if (x - targets[i]).abs() < 0.01 && v.abs() < 0.01 {
                continue;
            }
            let a = self.c.CreateSpringScalarAnimation()?;
            a.SetDampingRatio(damping)?;
            a.SetPeriod(ms(period))?;
            a.SetInitialVelocity(v)?;
            a.SetFinalValue(&boxed(targets[i])?)?;
            self.props.StartAnimation(&HSTRING::from(*k), &a)?;
            self.springs[i] = Spring { start: Instant::now(), x0: x, v0: v, target: targets[i], zeta: damping, omega };
        }
        self.busy_until = self.busy_until.max(Instant::now() + Duration::from_millis((settle_ms * 1.6) as u64 + 300));
        Ok(())
    }
}
