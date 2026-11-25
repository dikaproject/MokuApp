//! The Windows.UI.Composition visual tree and its animations.
//!
//! All geometry is driven by one `CompositionPropertySet` (`p`) holding W, H and T (0 = pill,
//! 1 = panel). Only those three scalars are animated, always by springs; every size, clip,
//! corner radius, rim, shadow and offset is bound to them with ExpressionAnimations, so the
//! whole morph runs inside the system compositor (DWM) with no per-frame work in this process.

use std::time::Instant;

use windows::Foundation::{IReference, PropertyValue, TimeSpan};
use windows::UI::Color;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::*;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::core::{HSTRING, Interface, Result, h};
use windows_numerics::{Vector2, Vector3};

use crate::gfx::*;
use crate::state::{ALL, PANEL, PANEL_RADIUS, State, motion};

/// Window size in DIP: widest state plus room for the shadow.
pub const WIN_W: f32 = 784.0;
pub const WIN_H: f32 = 380.0;
/// Gap between the screen top and the island.
pub const TOP: f32 = 8.0;

const BOLD: DWRITE_FONT_WEIGHT = DWRITE_FONT_WEIGHT_BOLD;
const SEMI: DWRITE_FONT_WEIGHT = DWRITE_FONT_WEIGHT_SEMI_BOLD;
const REG: DWRITE_FONT_WEIGHT = DWRITE_FONT_WEIGHT_NORMAL;

/// Pill layout constants (DIP).
const PAD: f32 = 14.0;
const LOGO: f32 = 24.0; // logo box; its ink is ~21.5 wide, ~9.8 tall (≈ cap height of 13 px text)
const LOGO_INK_L: f32 = 0.0525; // ink starts 5.25 % into the box (x 10 − half the stroke)
const LOGO_INK_W: f32 = 0.895;
const LOGO_INK_CY: f32 = 0.5425; // vertical centre of the ink
const BARS_W: f32 = 25.0;

/// Expression for the corner radius (inset by `i`): a full pill until panel-sized.
fn radius_expr(i: f32) -> String {
    format!("Min(p.H * 0.5, {PANEL_RADIUS}) - {i}")
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

/// CPU-side model of a running compositor spring, used only to know the current velocity when
/// a transition is interrupted (the compositor can't be asked). Same model as Composition's
/// natural-motion spring with `Period` = 1/ω (measured, see `start_springs`).
#[derive(Clone, Copy)]
struct Spring {
    start: Instant,
    x0: f32,
    v0: f32,
    target: f32,
    zeta: f32,
    omega: f32,
}

impl Spring {
    fn rest(x: f32) -> Self {
        Spring { start: Instant::now(), x0: x, v0: 0.0, target: x, zeta: 1.0, omega: 1.0 }
    }

    /// (value, velocity per second) now.
    fn now(&self) -> (f32, f32) {
        let t = self.start.elapsed().as_secs_f32();
        let (z, w, a, v0) = (self.zeta, self.omega, self.x0 - self.target, self.v0);
        let (d, v) = if z < 0.999 {
            let wd = w * (1.0 - z * z).sqrt();
            let b = (v0 + z * w * a) / wd;
            let (e, c, s) = ((-z * w * t).exp(), (wd * t).cos(), (wd * t).sin());
            (e * (a * c + b * s), e * ((b * wd - z * w * a) * c - (a * wd + z * w * b) * s))
        } else {
            let e = (-w * t).exp();
            (e * (a + (v0 + w * a) * t), e * (v0 - w * (v0 + w * a) * t))
        };
        (self.target + d, v)
    }
}

pub struct Island {
    c: Compositor,
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    gfx: Gfx,
    props: CompositionPropertySet,
    /// Running springs for W, H, T.
    springs: [Spring; 3],
    sizes: [(f32, f32); 4],
    contents: Vec<ContainerVisual>,
    panel_items: Vec<(SpriteVisual, f32)>,
    bars: Vec<CompositionSpriteShape>,
    pub state: State,
    /// Off-tree visuals that must stay alive (the shadow mask source).
    keep: Vec<Visual>,
    scale: f32,
    blur: bool,
}

impl Island {
    pub fn new(hwnd: HWND, scale: f32, state: State, blur: bool) -> Result<Self> {
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
            springs: [Spring::rest(0.0); 3],
            sizes: [PANEL; 4],
            contents: vec![],
            panel_items: vec![],
            bars: vec![],
            state,
            keep: vec![],
            scale,
            blur,
        };
        me.measure_sizes()?;
        me.build()?;
        Ok(me)
    }

    pub fn size(&self, st: State) -> (f32, f32) {
        self.sizes[st as usize]
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn set_scale(&mut self, scale: f32) -> Result<()> {
        self.scale = scale;
        self.build()
    }

    /// Pill widths follow their content: even 14 DIP side padding, 10 DIP gaps.
    fn measure_sizes(&mut self) -> Result<()> {
        let g = &self.gfx;
        let even = |w: f32| (w / 2.0).ceil() * 2.0;
        let time = g.measure("10.24", Font::Text, 13.0, SEMI)?;
        let ink = LOGO * LOGO_INK_W;
        self.sizes[State::Idle as usize] = (even(PAD + ink + 8.0 + time + PAD), 36.0);
        let date = g.measure("Sel, 24 Nov", Font::Text, 13.0, REG)?;
        self.sizes[State::Hover as usize] = (even(PAD + ink + 10.0 + time + 10.0 + date + 10.0 + 12.0 + PAD), 40.0);
        let title = g.measure("Midnight Drive", Font::Text, 13.0, SEMI)?;
        let artist = g.measure("Kaia", Font::Text, 13.0, REG)?;
        self.sizes[State::Music as usize] = (even(42.0 + title + 7.0 + artist + 10.0 + BARS_W + 12.0), 36.0);
        self.sizes[State::Panel as usize] = PANEL;
        Ok(())
    }

    // ---- expression helpers -------------------------------------------------------------

    fn bind<T: Interface>(&self, obj: &T, prop: &str, expr: &str) -> Result<()> {
        let a = self.c.CreateExpressionAnimationWithExpression(&HSTRING::from(expr))?;
        a.SetReferenceParameter(h!("p"), &self.props)?;
        a.SetScalarParameter(h!("s"), self.scale)?;
        obj.cast::<CompositionObject>()?.StartAnimation(&HSTRING::from(prop), &a)
    }

    fn bind_size<T: Interface>(&self, obj: &T) -> Result<()> {
        self.bind(obj, "Size", "Vector2(p.W, p.H)")
    }

    /// Rounded rectangle that tracks the island shape (size snapped to physical pixels), inset
    /// by `inset` DIP.
    fn shape_geometry(&self, inset: f32) -> Result<CompositionRoundedRectangleGeometry> {
        let g = self.c.CreateRoundedRectangleGeometry()?;
        g.SetOffset(v2(inset, inset))?;
        let i2 = 2.0 * inset;
        self.bind(&g, "Size", &format!("Vector2(Round(p.W * s) / s - {i2}, Round(p.H * s) / s - {i2})"))?;
        let r = radius_expr(inset);
        self.bind(&g, "CornerRadius", &format!("Vector2({r}, {r})"))?;
        Ok(g)
    }

    /// A sprite showing a D2D-drawn surface at (x, y) with size (w, h) in its parent's DIP space.
    /// `draw` receives coordinates relative to the parent (the sprite's own offset is undone).
    fn sprite(&self, x: f32, y: f32, w: f32, h: f32, draw: impl FnOnce(&Gfx, &Ctx) -> Result<()>) -> Result<SpriteVisual> {
        let s = self.scale;
        // Snap the sprite to whole pixels so the surface maps 1:1 and text stays crisp.
        let (w, h) = ((w * s).ceil() / s, (h * s).ceil() / s);
        let (x, y) = ((x * s).round() / s, (y * s).round() / s);
        let gfx = &self.gfx;
        let surf = gfx.surface(w, h, s, |ctx| gfx.translated(ctx, -x, -y, || draw(gfx, ctx)))?;
        let brush = self.c.CreateSurfaceBrushWithSurface(&surf)?;
        brush.SetStretch(CompositionStretch::Fill)?;
        let v = self.c.CreateSpriteVisual()?;
        v.SetBrush(&brush)?;
        v.SetSize(v2(w, h))?;
        v.SetOffset(v3(x, y))?;
        Ok(v)
    }

    // ---- tree ---------------------------------------------------------------------------

    fn build(&mut self) -> Result<()> {
        let c = &self.c.clone();
        let s = self.scale;
        self.root.Children()?.RemoveAll()?;
        self.root.SetScale(Vector3 { X: s, Y: s, Z: 1.0 })?;

        let (w, h) = self.size(self.state);
        let t = (self.state == State::Panel) as u8 as f32;
        for (i, (k, v)) in [("W", w), ("H", h), ("T", t)].into_iter().enumerate() {
            let k = HSTRING::from(k);
            let _ = self.props.StopAnimation(&k);
            self.props.InsertScalar(&k, v)?;
            self.springs[i] = Spring::rest(v);
        }

        let island = c.CreateContainerVisual()?;
        self.bind(&island, "Offset", &format!("Vector3(Round(({WIN_W} - p.W) * 0.5 * s) / s, {TOP}, 0)"))?;
        self.root.Children()?.InsertAtTop(&island)?;

        // Shadow: a brush-less SpriteVisual whose DropShadow takes its shape from a mask. The mask
        // is a CompositionVisualSurface rendering an (off-tree) ShapeVisual of the morphing
        // rounded rect, so the shadow follows the shape while nothing opaque is drawn under the
        // translucent glass. (A LayerVisual + opaque caster would make the glass opaque.)
        let caster = c.CreateShapeVisual()?;
        self.bind_size(&caster)?;
        let cs = c.CreateSpriteShapeWithGeometry(&self.shape_geometry(0.0)?)?;
        cs.SetFillBrush(&c.CreateColorBrushWithColor(color(0, 1.0))?)?;
        caster.Shapes()?.Append(&cs)?;
        let mask_surface = c.CreateVisualSurface()?;
        mask_surface.SetSourceVisual(&caster)?;
        self.bind(&mask_surface, "SourceSize", "Vector2(p.W, p.H)")?;
        let mask = c.CreateSurfaceBrushWithSurface(&mask_surface)?;
        mask.SetStretch(CompositionStretch::Fill)?;
        let shadow = c.CreateDropShadow()?;
        shadow.SetColor(color(0, 1.0))?;
        shadow.SetMask(&mask)?;
        self.bind(&shadow, "BlurRadius", "Lerp(30, 60, p.T)")?;
        self.bind(&shadow, "Offset", "Vector3(0, Lerp(10, 24, p.T), 0)")?;
        self.bind(&shadow, "Opacity", "Lerp(0.42, 0.5, p.T)")?;
        let shadow_visual = c.CreateSpriteVisual()?;
        self.bind_size(&shadow_visual)?;
        shadow_visual.SetShadow(&shadow)?;
        island.Children()?.InsertAtTop(&shadow_visual)?;
        self.keep = vec![caster.cast()?];

        // Glass body, clipped to the morphing rounded rectangle.
        let body = c.CreateContainerVisual()?;
        self.bind_size(&body)?;
        body.SetClip(&c.CreateGeometricClipWithGeometry(&self.shape_geometry(0.0)?)?)?;
        if self.blur {
            // DWM's blurred "host backdrop" (what acrylic uses); needs DWMWA_USE_HOSTBACKDROPBRUSH.
            let backdrop = c.CreateSpriteVisual()?;
            backdrop.SetRelativeSizeAdjustment(v2(1.0, 1.0))?;
            backdrop.SetBrush(&c.CreateHostBackdropBrush()?)?;
            body.Children()?.InsertAtTop(&backdrop)?;
        }
        let tint = c.CreateSpriteVisual()?;
        tint.SetRelativeSizeAdjustment(v2(1.0, 1.0))?;
        tint.SetBrush(&c.CreateColorBrushWithColor(color(0x0B0B14, 1.0))?)?;
        // Spec: 47 % / 52 % tint over a real blur. Without blur the tint must carry legibility on
        // its own: 72 % for the pill, 88 % for the panel (busy backgrounds show through sharply).
        let (a0, a1) = if self.blur { (0.47, 0.52) } else { (0.72, 0.88) };
        self.bind(&tint, "Opacity", &format!("Lerp({a0}, {a1}, p.T)"))?;
        body.Children()?.InsertAtTop(&tint)?;

        self.contents.clear();
        self.panel_items.clear();
        self.bars.clear();
        for st in ALL {
            let cont = self.build_content(st)?;
            cont.SetOpacity(if st == self.state { 1.0 } else { 0.0 })?;
            body.Children()?.InsertAtTop(&cont)?;
            self.contents.push(cont);
        }
        if self.state == State::Music {
            self.start_bars()?;
        }
        island.Children()?.InsertAtTop(&body)?;

        // Rim: a stroke of whole physical pixels (≥ 1 px, ≈ 1 DIP) on pixel centres, so it stays
        // sharp at 125 % / 175 %. Near-vertical gradient: white 55 % top → 18 % → 25 % bottom.
        let px = s.round().max(1.0) / s;
        let rim = c.CreateShapeVisual()?;
        self.bind_size(&rim)?;
        let rs = c.CreateSpriteShapeWithGeometry(&self.shape_geometry(px / 2.0)?)?;
        let grad = c.CreateLinearGradientBrush()?;
        grad.SetStartPoint(v2(0.3, 0.0))?;
        grad.SetEndPoint(v2(0.7, 1.0))?;
        for (o, a) in [(0.0, 0.55), (0.5, 0.18), (1.0, 0.25)] {
            grad.ColorStops()?.Append(&c.CreateColorGradientStopWithOffsetAndColor(o, color(0xffffff, a))?)?;
        }
        rs.SetStrokeBrush(&grad)?;
        rs.SetStrokeThickness(px)?;
        rim.Shapes()?.Append(&rs)?;
        island.Children()?.InsertAtTop(&rim)?;
        Ok(())
    }

    fn build_content(&mut self, st: State) -> Result<ContainerVisual> {
        let (w, h) = self.size(st);
        let cont = self.c.CreateContainerVisual()?;
        cont.SetSize(v2(w, h))?;
        let cy = h / 2.0;
        let g = &self.gfx;
        // Logo box position so its ink starts at x and is vertically centred on cy.
        let logo = |g: &Gfx, ctx: &Ctx, x: f32| g.logo(ctx, x - LOGO * LOGO_INK_L, cy - LOGO * LOGO_INK_CY, LOGO);
        match st {
            State::Idle | State::Hover => {
                let tx = PAD + LOGO * LOGO_INK_W + if st == State::Idle { 8.0 } else { 10.0 };
                let v = self.sprite(0.0, 0.0, w, h, |g, ctx| {
                    logo(g, ctx, PAD)?;
                    let tw = g.text(ctx, "10.24", Font::Text, 13.0, SEMI, WHITE, tx, cy, 0.0)?;
                    if st == State::Hover {
                        g.text(ctx, "Sel, 24 Nov", Font::Text, 13.0, REG, WHITE2, tx + tw + 10.0, cy, 0.0)?;
                        g.text(ctx, "\u{E70D}", Font::Icons, 12.0, REG, WHITE2, w - PAD - 6.0, cy + 0.5, 0.5)?;
                    }
                    Ok(())
                })?;
                cont.Children()?.InsertAtTop(&v)?;
            }
            State::Music => {
                let title_w = g.measure("Midnight Drive", Font::Text, 13.0, SEMI)?;
                let v = self.sprite(0.0, 0.0, w, h, |g, ctx| {
                    g.art(ctx, 8.0, 6.0, 24.0, 7.0, false)?;
                    g.text(ctx, "Midnight Drive", Font::Text, 13.0, SEMI, WHITE, 42.0, cy, 0.0)?;
                    g.text(ctx, "Kaia", Font::Text, 13.0, REG, WHITE2, 42.0 + title_w + 7.0, cy, 0.0)?;
                    Ok(())
                })?;
                cont.Children()?.InsertAtTop(&v)?;
                // Audio bars are composition shapes so they can bounce on the compositor.
                let bars = self.c.CreateShapeVisual()?;
                let bh = 20.0;
                bars.SetSize(v2(BARS_W, bh))?;
                bars.SetOffset(v3(w - 12.0 - BARS_W, (h - bh) / 2.0))?;
                let brush = self.c.CreateColorBrushWithColor(color(0xFF375F, 1.0))?;
                for (i, bar_h) in [9.0f32, 15.0, 19.0, 12.0, 16.0].into_iter().enumerate() {
                    let x = i as f32 * 5.5;
                    let geo = self.c.CreateRoundedRectangleGeometry()?;
                    geo.SetSize(v2(3.0, bar_h))?;
                    geo.SetCornerRadius(v2(1.5, 1.5))?;
                    geo.SetOffset(v2(x, (bh - bar_h) / 2.0))?;
                    let shape = self.c.CreateSpriteShapeWithGeometry(&geo)?;
                    shape.SetFillBrush(&brush)?;
                    shape.SetCenterPoint(v2(x + 1.5, bh / 2.0))?;
                    bars.Shapes()?.Append(&shape)?;
                    self.bars.push(shape);
                }
                cont.Children()?.InsertAtTop(&bars)?;
            }
            State::Panel => {
                // Panel blocks live in a "gate" whose opacity is tied to the live width: they stay
                // hidden until the shape is ≥ 90 % wide, so nothing is shown clipped at the edges
                // (on open and on close, and also if a transition is interrupted).
                let gate = self.c.CreateContainerVisual()?;
                self.bind(&gate, "Opacity", &format!("Clamp((p.W - {}) / {}, 0, 1)", PANEL.0 * 0.9, PANEL.0 * 0.06))?;
                self.build_panel(&gate)?;
                cont.Children()?.InsertAtTop(&gate)?;
            }
        }
        // Pills stay centred in the morphing shape; the panel is anchored to the top edge.
        let y = if st == State::Panel { "0".to_string() } else { format!("Round((p.H - {h}) * 0.5 * s) / s") };
        self.bind(&cont, "Offset", &format!("Vector3(Round((p.W - {w}) * 0.5 * s) / s, {y}, 0)"))?;
        Ok(cont)
    }

    /// Expanded music panel, 640×276 DIP, laid out from ref/panel-musik.png (which is 2×).
    /// Each block is its own sprite so the blocks can fade/slide in with a stagger.
    fn build_panel(&mut self, cont: &ContainerVisual) -> Result<()> {
        let white = |a: f32| rgba(0xffffff, a);
        let mut items: Vec<SpriteVisual> = vec![];

        // Tab bar
        items.push(self.sprite(15.0, 15.0, 610.0, 38.0, |g, ctx| {
            g.fill_rrect(ctx, rect(15.0, 15.0, 610.0, 38.0), 19.0, &g.brush(ctx, white(0.08))?.cast()?);
            g.fill_rrect(ctx, rect(18.5, 19.0, 80.0, 30.0), 15.0, &g.brush(ctx, white(0.16))?.cast()?);
            g.text(ctx, "\u{EC4F}", Font::Icons, 14.0, REG, WHITE, 34.0, 34.0, 0.5)?;
            g.text(ctx, "Musik", Font::Text, 13.0, SEMI, WHITE, 45.0, 34.0, 0.0)?;
            let icons = ["\u{E787}", "\u{E9E9}", "\u{E715}", "\u{E823}", "\u{E77F}", "\u{E722}", "\u{E8EA}", "\u{E9D9}", "\u{E721}"];
            for (i, icon) in icons.iter().enumerate() {
                g.text(ctx, icon, Font::Icons, 15.0, REG, white(0.85), 142.0 + 58.0 * i as f32, 34.0, 0.5)?;
            }
            Ok(())
        })?);

        // Album art
        items.push(self.sprite(15.0, 67.0, 156.0, 156.0, |g, ctx| g.art(ctx, 15.0, 67.0, 156.0, 28.0, true))?);

        // Title, artist, output device chip
        items.push(self.sprite(187.0, 62.0, 437.0, 50.0, |g, ctx| {
            g.text(ctx, "Midnight Drive", Font::Display, 20.0, BOLD, WHITE, 187.0, 78.5, 0.0)?;
            g.text(ctx, "Kaia · Neon Atlas", Font::Text, 13.0, REG, WHITE2, 187.0, 101.5, 0.0)?;
            let label_w = g.measure("Buds Pro", Font::Text, 12.0, SEMI)?;
            let chip_w = (label_w + 40.0).round();
            let x = 624.0 - chip_w;
            g.fill_rrect(ctx, rect(x, 67.0, chip_w, 26.0), 13.0, &g.brush(ctx, white(0.14))?.cast()?);
            g.text(ctx, "\u{E7F6}", Font::Icons, 13.0, REG, WHITE, x + 16.0, 80.0, 0.5)?;
            g.text(ctx, "Buds Pro", Font::Text, 12.0, SEMI, WHITE, x + 27.0, 80.0, 0.0)?;
            Ok(())
        })?);

        // Lyrics
        items.push(self.sprite(187.0, 116.0, 437.0, 66.0, |g, ctx| {
            g.text(ctx, "lampu kota memudar di kaca jendela", Font::Text, 13.0, REG, WHITE3, 187.0, 128.0, 0.0)?;
            g.text(ctx, "dan kota pun perlahan terlelap", Font::Text, 15.0, SEMI, WHITE, 187.0, 149.5, 0.0)?;
            g.text(ctx, "di bawah lampu jalan kita bicara", Font::Text, 13.0, REG, WHITE3, 187.0, 171.0, 0.0)?;
            Ok(())
        })?);

        // Progress
        items.push(self.sprite(187.0, 186.0, 437.0, 22.0, |g, ctx| {
            g.text(ctx, "1:24", Font::Text, 11.5, REG, WHITE3, 187.0, 196.5, 0.0)?;
            g.text(ctx, "3:47", Font::Text, 11.5, REG, WHITE3, 624.0, 196.5, 1.0)?;
            g.fill_rrect(ctx, rect(219.5, 195.0, 372.0, 3.0), 1.5, &g.brush(ctx, white(0.25))?.cast()?);
            g.fill_rrect(ctx, rect(219.5, 195.0, 137.5, 3.0), 1.5, &g.brush(ctx, WHITE)?.cast()?);
            Ok(())
        })?);

        // Controls
        items.push(self.sprite(187.0, 212.0, 437.0, 50.0, |g, ctx| {
            let cy = 237.0;
            let chip = g.brush(ctx, white(0.14))?.cast()?;
            g.fill_rrect(ctx, rect(187.0, 222.0, 64.0, 30.0), 15.0, &chip);
            g.text(ctx, "\u{EC4F}", Font::Icons, 13.0, REG, WHITE, 202.0, cy, 0.5)?;
            g.text(ctx, "Lirik", Font::Text, 13.0, SEMI, WHITE, 211.0, cy, 0.0)?;
            let wb = g.brush(ctx, WHITE)?.cast()?;
            // previous: bar + left triangle; next: right triangle + bar
            g.fill_rrect(ctx, rect(359.5, cy - 6.5, 2.5, 13.0), 1.0, &wb);
            g.triangle(ctx, 368.5, cy, 12.0, -1.0, &wb)?;
            g.triangle(ctx, 476.0, cy, 12.0, 1.0, &wb)?;
            g.fill_rrect(ctx, rect(482.5, cy - 6.5, 2.5, 13.0), 1.0, &wb);
            // play / pause
            g.fill_circle(ctx, 422.5, cy, 23.0, &wb);
            let dark = g.brush(ctx, rgba(0x0A0A12, 1.0))?.cast()?;
            g.fill_rrect(ctx, rect(416.5, cy - 7.5, 4.0, 15.0), 1.2, &dark);
            g.fill_rrect(ctx, rect(424.5, cy - 7.5, 4.0, 15.0), 1.2, &dark);
            // volume
            g.fill_circle(ctx, 609.0, cy, 15.0, &chip);
            g.text(ctx, "\u{E767}", Font::Icons, 14.0, REG, WHITE, 609.0, cy, 0.5)?;
            Ok(())
        })?);

        for v in items {
            cont.Children()?.InsertAtTop(&v)?;
            let y = v.Offset()?.Y;
            self.panel_items.push((v, y));
        }
        Ok(())
    }

    // ---- animation ----------------------------------------------------------------------

    /// Retarget W, H, T with springs, handing over the velocity of any spring still in flight
    /// so interrupted transitions (e.g. hover → panel mid-way) don't hitch.
    fn start_springs(&mut self, targets: [f32; 3], damping: f32, settle_ms: f32) -> Result<()> {
        // Settle time (2 % band) ≈ 4 / (ζ·ω). Composition's `Period` behaves as 1/ω (measured:
        // treating it as 2π/ω made a 350 ms spring take ~2 s), so period = ζ·settle / 4.
        let period = damping * settle_ms / 4.0;
        let omega = 1000.0 / period;
        for (i, k) in ["W", "H", "T"].into_iter().enumerate() {
            let (x, v) = self.springs[i].now();
            let a = self.c.CreateSpringScalarAnimation()?;
            a.SetDampingRatio(damping)?;
            a.SetPeriod(ms(period))?;
            a.SetInitialVelocity(v)?;
            a.SetFinalValue(&boxed(targets[i])?)?;
            self.props.StartAnimation(&HSTRING::from(k), &a)?;
            self.springs[i] = Spring { start: Instant::now(), x0: x, v0: v, target: targets[i], zeta: damping, omega };
        }
        Ok(())
    }

    fn fade(&self, v: &Visual, from: Option<f32>, to: f32, dur: f32, delay: f32) -> Result<()> {
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

    pub fn set_state(&mut self, to: State) -> Result<()> {
        let from = self.state;
        if from == to {
            return Ok(());
        }
        self.state = to;
        let (damping, settle) = motion(from, to);
        let (w, h) = self.size(to);
        self.start_springs([w, h, (to == State::Panel) as u8 as f32], damping, settle)?;

        // Cross-fade content.
        self.fade(&self.contents[from as usize].cast()?, None, 0.0, 110.0, 0.0)?;
        let target: Visual = self.contents[to as usize].cast()?;
        if to == State::Panel {
            target.StopAnimation(h!("Opacity"))?;
            target.SetOpacity(1.0)?;
            // Panel blocks: fade + 8 DIP slide up, 40 ms stagger, starting about when the width
            // gate opens.
            for (i, (v, y)) in self.panel_items.iter().enumerate() {
                let delay = 130.0 + 40.0 * i as f32;
                self.fade(&v.cast()?, Some(0.0), 1.0, 220.0, delay)?;
                let slide = self.c.CreateScalarKeyFrameAnimation()?;
                slide.InsertKeyFrame(0.0, y + 8.0)?;
                let ease = self.c.CreateCubicBezierEasingFunction(v2(0.2, 0.8), v2(0.2, 1.0))?;
                slide.InsertKeyFrameWithEasingFunction(1.0, *y, &ease)?;
                slide.SetDuration(ms(300.0))?;
                slide.SetDelayTime(ms(delay))?;
                slide.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
                v.StartAnimation(h!("Offset.Y"), &slide)?;
            }
        } else {
            // Coming out of the panel, wait for the shape to shrink a bit before showing the pill.
            let delay = if from == State::Panel { 150.0 } else { 60.0 };
            self.fade(&target, None, 1.0, 200.0, delay)?;
        }

        if to == State::Music {
            self.start_bars()?;
        } else if from == State::Music {
            self.stop_bars()?;
        }
        Ok(())
    }

    /// Bouncing audio bars: looping key-frame animations that run entirely in the compositor.
    fn start_bars(&self) -> Result<()> {
        for (i, bar) in self.bars.iter().enumerate() {
            let a = self.c.CreateVector2KeyFrameAnimation()?;
            let lo = [0.45, 0.6, 0.4, 0.7, 0.5][i];
            a.InsertKeyFrame(0.0, v2(1.0, 1.0))?;
            a.InsertKeyFrame(0.5, v2(1.0, lo))?;
            a.InsertKeyFrame(1.0, v2(1.0, 1.0))?;
            a.SetDuration(ms([620.0, 840.0, 700.0, 930.0, 760.0][i]))?;
            a.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
            bar.StartAnimation(h!("Scale"), &a)?;
        }
        Ok(())
    }

    fn stop_bars(&self) -> Result<()> {
        for bar in &self.bars {
            bar.StopAnimation(h!("Scale"))?;
            bar.SetScale(v2(1.0, 1.0))?;
        }
        Ok(())
    }
}
