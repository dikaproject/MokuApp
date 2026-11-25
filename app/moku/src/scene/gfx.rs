//! Direct2D / DirectWrite drawing into Composition drawing surfaces, plus text measurement for
//! the UI kit's layout. Surfaces are rasterised only when their content changes; the
//! compositor moves / fades the resulting sprites.

use std::cell::RefCell;
use std::collections::HashMap;

use windows::Foundation::Size;
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::UI::Composition::{CompositionDrawingSurface, CompositionGraphicsDevice, Compositor};
use windows::Win32::Foundation::{HMODULE, POINT};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Composition::{ICompositionDrawingSurfaceInterop, ICompositorInterop};
use windows::core::{HSTRING, Interface, Result, w};
use windows_numerics::{Matrix3x2, Vector2};

use crate::ui::layout::{Measure, Op};
use crate::ui::{Font, Rect, Rgba, TextSpec, Weight};

pub type Ctx = ID2D1DeviceContext;

fn v2(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

fn d2c(c: Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r, g: c.g, b: c.b, a: c.a }
}

fn d2r(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F { left: r.x, top: r.y, right: r.x + r.w, bottom: r.y + r.h }
}

fn weight(w: Weight) -> DWRITE_FONT_WEIGHT {
    match w {
        Weight::Regular => DWRITE_FONT_WEIGHT_NORMAL,
        Weight::Medium => DWRITE_FONT_WEIGHT_MEDIUM,
        Weight::Semibold => DWRITE_FONT_WEIGHT_SEMI_BOLD,
        Weight::Bold => DWRITE_FONT_WEIGHT_BOLD,
    }
}

pub struct Gfx {
    factory: ID2D1Factory1,
    dwrite: IDWriteFactory,
    device: CompositionGraphicsDevice,
    d2d: ID2D1Device,
    dxgi: IDXGIDevice,
    /// Font families for Text / Display / Icons / Mono, with Windows 10 fallbacks applied.
    families: [HSTRING; 4],
    formats: RefCell<HashMap<(Font, u32, Weight, bool), IDWriteTextFormat>>,
    measures: RefCell<HashMap<(TextSpecKey, u32), (f32, f32)>>,
}

#[derive(PartialEq, Eq, Hash)]
struct TextSpecKey(String, Font, u32, Weight, u32);

impl Gfx {
    pub fn new(compositor: &Compositor) -> Result<Self> {
        unsafe {
            let mut d3d = None;
            let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
            // Default: WARP (CPU) device. We only rasterise surfaces when content changes, and
            // WARP keeps GPU driver DLLs out of the process (prototype: 25 MB vs 45 MB working
            // set). The compositor still animates on the GPU. `--gpu` = hardware.
            let gpu = std::env::args().any(|a| a == "--gpu");
            let hw = if !gpu {
                Err(windows::core::Error::empty())
            } else {
                D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), flags, None, D3D11_SDK_VERSION, Some(&mut d3d), None, None)
            };
            if hw.is_err() {
                D3D11CreateDevice(None, D3D_DRIVER_TYPE_WARP, HMODULE::default(), flags, None, D3D11_SDK_VERSION, Some(&mut d3d), None, None)?;
            }
            let dxgi: IDXGIDevice = d3d.unwrap().cast()?;
            let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let d2d = factory.CreateDevice(&dxgi)?;
            let device = compositor.cast::<ICompositorInterop>()?.CreateGraphicsDevice(&d2d)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            // Windows 10 has neither Segoe UI Variable nor Segoe Fluent Icons: fall back to
            // Segoe UI / Segoe MDL2 Assets (same code points for the icons).
            let mut coll = None;
            dwrite.GetSystemFontCollection(&mut coll, false)?;
            let coll = coll.unwrap();
            let pick = |want: &str, fallback: &str| {
                let (mut index, mut exists) = (0u32, windows::core::BOOL(0));
                let name = HSTRING::from(want);
                let _ = coll.FindFamilyName(&name, &mut index, &mut exists);
                if exists.as_bool() { name } else { HSTRING::from(fallback) }
            };
            let families = [
                pick("Segoe UI Variable Text", "Segoe UI"),
                pick("Segoe UI Variable Display", "Segoe UI"),
                pick("Segoe Fluent Icons", "Segoe MDL2 Assets"),
                pick("Cascadia Mono", "Consolas"),
            ];
            crate::ui::icons::FLUENT.store(families[2] == HSTRING::from("Segoe Fluent Icons"), std::sync::atomic::Ordering::Relaxed);
            Ok(Self { factory, dwrite, device, d2d, dxgi, families, formats: RefCell::default(), measures: RefCell::default() })
        }
    }

    /// Give back memory held for surfaces and drawing resources that are no longer used (after
    /// the panel closed): the composition surface atlas, Direct2D's resource caches and the
    /// D3D (WARP) device's internal buffers. These are real frees, not page eviction; anything
    /// needed again is recreated on the next draw.
    pub fn trim(&self) {
        // Text formats and measurements of the panel's strings: rebuilt on demand (a few µs each).
        *self.measures.borrow_mut() = HashMap::new();
        *self.formats.borrow_mut() = HashMap::new();
        let _ = self.device.Trim();
        unsafe {
            self.d2d.ClearResources(0);
            // IDXGIDevice3::Trim wants a cleared immediate context first (D2D sets its own state
            // again on the next BeginDraw).
            if let (Ok(d3), Ok(dev)) = (self.dxgi.cast::<windows::Win32::Graphics::Dxgi::IDXGIDevice3>(), self.dxgi.cast::<ID3D11Device>())
                && let Ok(ctx) = dev.GetImmediateContext()
            {
                ctx.ClearState();
                d3.Trim();
            }
        }
    }
    /// Create a surface of `w`×`h` DIP rasterised at `scale`, and draw into it in DIP coordinates.
    pub fn surface(&self, w: f32, h: f32, scale: f32, draw: impl FnOnce(&Ctx) -> Result<()>) -> Result<CompositionDrawingSurface> {
        let size = Size { Width: (w * scale).ceil().max(1.0), Height: (h * scale).ceil().max(1.0) };
        let surf = self.device.CreateDrawingSurface(size, DirectXPixelFormat::B8G8R8A8UIntNormalized, DirectXAlphaMode::Premultiplied)?;
        unsafe {
            let interop: ICompositionDrawingSurfaceInterop = surf.cast()?;
            let mut off = POINT::default();
            // BeginDraw hands out a device context into an atlas; `off` is where our pixels start.
            let ctx: ID2D1DeviceContext = interop.BeginDraw(None, &mut off)?;
            ctx.SetDpi(96.0 * scale, 96.0 * scale);
            ctx.SetTransform(&Matrix3x2::translation(off.x as f32 / scale, off.y as f32 / scale));
            ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            ctx.Clear(Some(&D2D1_COLOR_F::default()));
            let r = draw(&ctx);
            interop.EndDraw()?;
            r?;
        }
        Ok(surf)
    }

    /// Run `f` with an extra translation applied.
    pub fn translated(&self, ctx: &Ctx, dx: f32, dy: f32, f: impl FnOnce() -> Result<()>) -> Result<()> {
        unsafe {
            let mut m = Matrix3x2::default();
            ctx.GetTransform(&mut m);
            let mut t = m;
            t.M31 += dx;
            t.M32 += dy;
            ctx.SetTransform(&t);
            let r = f();
            ctx.SetTransform(&m);
            r
        }
    }

    pub fn clear_measure_cache(&self) {
        let mut m = self.measures.borrow_mut();
        if m.len() > 2000 {
            m.clear();
        }
    }

    fn format(&self, font: Font, size: f32, w: Weight, wrap: bool) -> Result<IDWriteTextFormat> {
        let key = (font, size.to_bits(), w, wrap);
        if let Some(f) = self.formats.borrow().get(&key) {
            return Ok(f.clone());
        }
        let family = &self.families[font as usize];
        let fmt = unsafe {
            let fmt = self.dwrite.CreateTextFormat(family, None, weight(w), DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("id-id"))?;
            fmt.SetWordWrapping(if wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP })?;
            let sign = self.dwrite.CreateEllipsisTrimmingSign(&fmt)?;
            let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            fmt.SetTrimming(&trimming, &sign)?;
            fmt
        };
        self.formats.borrow_mut().insert(key, fmt.clone());
        Ok(fmt)
    }

    /// A text layout for `t` limited to `max_w` (and `max_lines` lines).
    fn layout(&self, t: &TextSpec, max_w: f32) -> Result<IDWriteTextLayout> {
        let wrap = t.max_lines > 1;
        let fmt = self.format(t.font, t.size, t.weight, wrap)?;
        unsafe {
            let text: Vec<u16> = t.s.encode_utf16().collect();
            let max_h = if wrap { (t.size * 1.34 * t.max_lines as f32).ceil() + 1.0 } else { t.size * 4.0 };
            let layout = self.dwrite.CreateTextLayout(&text, &fmt, max_w.max(1.0), max_h)?;
            if t.font != Font::Icons {
                // Tabular figures so clocks / timers don't jiggle.
                let typo = self.dwrite.CreateTypography()?;
                typo.AddFontFeature(DWRITE_FONT_FEATURE { nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, parameter: 1 })?;
                layout.SetTypography(&typo, DWRITE_TEXT_RANGE { startPosition: 0, length: text.len() as u32 })?;
            }
            Ok(layout)
        }
    }

    fn measure_text(&self, t: &TextSpec, max_w: f32) -> Result<(f32, f32)> {
        let l = self.layout(t, max_w)?;
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe { l.GetMetrics(&mut m)? };
        let w = m.widthIncludingTrailingWhitespace.min(max_w).ceil();
        let h = if t.max_lines > 1 { m.height.min(t.size * 1.34 * t.max_lines as f32 + 1.0) } else { m.height };
        Ok((w, h.ceil()))
    }

    // ---- painting ops --------------------------------------------------------------------

    fn brush(&self, ctx: &Ctx, c: Rgba) -> Result<ID2D1SolidColorBrush> {
        unsafe { ctx.CreateSolidColorBrush(&d2c(c), None) }
    }

    fn stroke_style(&self, dashed: bool) -> Result<ID2D1StrokeStyle> {
        let props = D2D1_STROKE_STYLE_PROPERTIES {
            startCap: D2D1_CAP_STYLE_ROUND,
            endCap: D2D1_CAP_STYLE_ROUND,
            dashCap: D2D1_CAP_STYLE_ROUND,
            lineJoin: D2D1_LINE_JOIN_ROUND,
            miterLimit: 10.0,
            dashStyle: if dashed { D2D1_DASH_STYLE_DASH } else { D2D1_DASH_STYLE_SOLID },
            dashOffset: 0.0,
        };
        let f: &ID2D1Factory = &self.factory;
        unsafe { f.CreateStrokeStyle(&props, None) }
    }

    /// Draw a list of layout ops.
    pub fn paint(&self, ctx: &Ctx, ops: &[Op]) -> Result<()> {
        for op in ops {
            self.paint_op(ctx, op)?;
        }
        Ok(())
    }

    fn paint_op(&self, ctx: &Ctx, op: &Op) -> Result<()> {
        unsafe {
            match op {
                Op::Fill { r, radius, color } => {
                    let b = self.brush(ctx, *color)?;
                    if *radius > 0.0 {
                        ctx.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: d2r(*r), radiusX: *radius, radiusY: *radius }, &b);
                    } else {
                        ctx.FillRectangle(&d2r(*r), &b);
                    }
                }
                Op::Stroke { r, radius, color, width, dashed } => {
                    let b = self.brush(ctx, *color)?;
                    let i = width / 2.0;
                    let rr = Rect::new(r.x + i, r.y + i, r.w - 2.0 * i, r.h - 2.0 * i);
                    let rad = (radius - i).max(0.0);
                    ctx.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: d2r(rr), radiusX: rad, radiusY: rad }, &b, *width, &self.stroke_style(*dashed)?);
                }
                Op::Text { r, t } => self.text(ctx, *r, t)?,
                Op::Image { r, radius, img } => {
                    let bmp = &img.0;
                    if bmp.width == 0 || bmp.height == 0 {
                        return Ok(());
                    }
                    let props = D2D1_BITMAP_PROPERTIES1 {
                        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                        dpiX: 96.0,
                        dpiY: 96.0,
                        bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                        colorContext: std::mem::ManuallyDrop::new(None),
                    };
                    let d2bmp = ctx.CreateBitmap(D2D_SIZE_U { width: bmp.width, height: bmp.height }, Some(bmp.bgra.as_ptr() as _), bmp.width * 4, &props)?;
                    let bp = D2D1_BITMAP_BRUSH_PROPERTIES1 {
                        extendModeX: D2D1_EXTEND_MODE_CLAMP,
                        extendModeY: D2D1_EXTEND_MODE_CLAMP,
                        interpolationMode: D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                    };
                    let brush = ctx.CreateBitmapBrush(&d2bmp, Some(&bp), None)?;
                    let (sx, sy) = (r.w / bmp.width as f32, r.h / bmp.height as f32);
                    brush.SetTransform(&Matrix3x2 { M11: sx, M12: 0.0, M21: 0.0, M22: sy, M31: r.x, M32: r.y });
                    ctx.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: d2r(*r), radiusX: *radius, radiusY: *radius }, &brush);
                }
                Op::Arc { cx, cy, radius, width, from, to, color } => {
                    let b = self.brush(ctx, *color)?;
                    let sweep = (to - from).clamp(0.0, 1.0);
                    if sweep >= 0.999 {
                        ctx.DrawEllipse(&D2D1_ELLIPSE { point: v2(*cx, *cy), radiusX: *radius, radiusY: *radius }, &b, *width, None);
                    } else if sweep > 0.0 {
                        let pt = |f: f32| {
                            let a = f * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                            v2(cx + radius * a.cos(), cy + radius * a.sin())
                        };
                        let geo = self.factory.CreatePathGeometry()?;
                        let sink = geo.Open()?;
                        sink.BeginFigure(pt(*from), D2D1_FIGURE_BEGIN_HOLLOW);
                        sink.AddArc(&D2D1_ARC_SEGMENT {
                            point: pt(from + sweep),
                            size: D2D_SIZE_F { width: *radius, height: *radius },
                            rotationAngle: 0.0,
                            sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                            arcSize: if sweep > 0.5 { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                        });
                        sink.EndFigure(D2D1_FIGURE_END_OPEN);
                        sink.Close()?;
                        ctx.DrawGeometry(&geo, &b, *width, &self.stroke_style(false)?);
                    }
                }
                Op::Line { pts, width, color } => {
                    if pts.len() < 2 {
                        return Ok(());
                    }
                    let b = self.brush(ctx, *color)?;
                    let geo = self.factory.CreatePathGeometry()?;
                    let sink = geo.Open()?;
                    sink.BeginFigure(v2(pts[0].0, pts[0].1), D2D1_FIGURE_BEGIN_HOLLOW);
                    for p in &pts[1..] {
                        sink.AddLine(v2(p.0, p.1));
                    }
                    sink.EndFigure(D2D1_FIGURE_END_OPEN);
                    sink.Close()?;
                    ctx.DrawGeometry(&geo, &b, *width, &self.stroke_style(false)?);
                }
                Op::Poly { pts, color } => {
                    if pts.len() < 3 {
                        return Ok(());
                    }
                    let b = self.brush(ctx, *color)?;
                    let geo = self.factory.CreatePathGeometry()?;
                    let sink = geo.Open()?;
                    sink.BeginFigure(v2(pts[0].0, pts[0].1), D2D1_FIGURE_BEGIN_FILLED);
                    for p in &pts[1..] {
                        sink.AddLine(v2(p.0, p.1));
                    }
                    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    sink.Close()?;
                    ctx.FillGeometry(&geo, &b, None);
                    // A hairline in the same colour rounds the corners (round joins).
                    ctx.DrawGeometry(&geo, &b, 0.8, &self.stroke_style(false)?);
                }
                Op::Logo { r, alpha } => self.logo(ctx, r.x, r.y, r.w.min(r.h), *alpha)?,
                Op::PushClip { r, radius } => {
                    let f: &ID2D1Factory = &self.factory;
                    let geo = f.CreateRoundedRectangleGeometry(&D2D1_ROUNDED_RECT { rect: d2r(*r), radiusX: *radius, radiusY: *radius })?;
                    let params = D2D1_LAYER_PARAMETERS1 {
                        contentBounds: D2D_RECT_F { left: f32::MIN, top: f32::MIN, right: f32::MAX, bottom: f32::MAX },
                        geometricMask: std::mem::ManuallyDrop::new(Some(geo.cast()?)),
                        maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                        maskTransform: Matrix3x2::identity(),
                        opacity: 1.0,
                        opacityBrush: std::mem::ManuallyDrop::new(None),
                        layerOptions: D2D1_LAYER_OPTIONS1_NONE,
                    };
                    ctx.PushLayer(&params, None);
                    // The ManuallyDrop'd geometry reference is released by D2D's own AddRef/Release
                    // pairing; drop our extra one.
                    let _ = std::mem::ManuallyDrop::into_inner(params.geometricMask);
                }
                Op::PopClip => ctx.PopLayer(),
            }
        }
        Ok(())
    }

    /// Single line: vertically centred on the cap height in `r`, aligned per `t.align`.
    /// Icons: centred on their em box. Wrapped: top-aligned.
    fn text(&self, ctx: &Ctx, r: Rect, t: &TextSpec) -> Result<()> {
        if t.s.is_empty() {
            return Ok(());
        }
        let l = self.layout(t, r.w.max(1.0))?;
        unsafe {
            l.SetTextAlignment(if t.align >= 0.99 {
                DWRITE_TEXT_ALIGNMENT_TRAILING
            } else if t.align >= 0.49 {
                DWRITE_TEXT_ALIGNMENT_CENTER
            } else {
                DWRITE_TEXT_ALIGNMENT_LEADING
            })?;
            let mut m = DWRITE_TEXT_METRICS::default();
            l.GetMetrics(&mut m)?;
            let top = if t.font == Font::Icons {
                r.cy() - m.height / 2.0
            } else if t.max_lines > 1 {
                r.y
            } else {
                let mut lines = [DWRITE_LINE_METRICS::default(); 1];
                let mut n = 0;
                let _ = l.GetLineMetrics(Some(&mut lines), &mut n);
                r.cy() + t.size * 0.35 - lines[0].baseline
            };
            let brush = self.brush(ctx, t.color)?;
            ctx.DrawTextLayout(v2(r.x, top), &l, &brush, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
        }
        Ok(())
    }

    fn linear(&self, ctx: &Ctx, from: (f32, f32), to: (f32, f32), stops: &[(f32, Rgba)]) -> Result<ID2D1LinearGradientBrush> {
        unsafe {
            let stops: Vec<D2D1_GRADIENT_STOP> = stops.iter().map(|&(position, c)| D2D1_GRADIENT_STOP { position, color: d2c(c) }).collect();
            let rt: &ID2D1RenderTarget = ctx;
            let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: v2(from.0, from.1), endPoint: v2(to.0, to.1) };
            ctx.CreateLinearGradientBrush(&props, None, &coll)
        }
    }

    /// The Moku logo mark (lowercase m on a horizon line) in a `size`×`size` box at (x, y).
    pub fn logo(&self, ctx: &Ctx, x: f32, y: f32, size: f32, alpha: f32) -> Result<()> {
        let k = size / 100.0;
        let p = |px: f32, py: f32| v2(x + px * k, y + py * k);
        unsafe {
            let geo = self.factory.CreatePathGeometry()?;
            let sink = geo.Open()?;
            sink.BeginFigure(p(10.0, 70.0), D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddLine(p(25.0, 70.0));
            sink.AddLine(p(25.0, 51.0));
            let arc = |to| D2D1_ARC_SEGMENT {
                point: to,
                size: D2D_SIZE_F { width: 12.5 * k, height: 12.5 * k },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: D2D1_ARC_SIZE_SMALL,
            };
            sink.AddArc(&arc(p(50.0, 51.0)));
            sink.AddLine(p(50.0, 70.0));
            sink.AddLine(p(50.0, 51.0));
            sink.AddArc(&arc(p(75.0, 51.0)));
            sink.AddLine(p(75.0, 70.0));
            sink.AddLine(p(90.0, 70.0));
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close()?;
            let a = p(10.0, 0.0);
            let b = p(90.0, 0.0);
            let grad = self.linear(
                ctx,
                (a.X, 0.0),
                (b.X, 0.0),
                &[(0.0, Rgba::hex(0x4FD8FF, alpha)), (0.5, Rgba::hex(0x7C6BFF, alpha)), (1.0, Rgba::hex(0xFF5FA2, alpha))],
            )?;
            ctx.DrawGeometry(&geo, &grad, 9.5 * k, &self.stroke_style(false)?);
        }
        Ok(())
    }
}

impl Measure for Gfx {
    fn text(&self, t: &TextSpec, max_w: f32) -> (f32, f32) {
        let key = (TextSpecKey(t.s.clone(), t.font, t.size.to_bits(), t.weight, t.max_lines), max_w.to_bits());
        if let Some(v) = self.measures.borrow().get(&key) {
            return *v;
        }
        let v = self.measure_text(t, max_w).unwrap_or((0.0, t.size * 1.3));
        self.measures.borrow_mut().insert(key, v);
        v
    }

    /// DirectWrite hit-testing (exact, kerning included) instead of measuring a prefix.
    fn caret_x(&self, t: &TextSpec, index: usize) -> f32 {
        let i = index.min(t.s.len());
        if i == 0 {
            return 0.0;
        }
        let pos = t.s.get(..i).map_or(0, |p| p.encode_utf16().count()) as u32;
        let one = TextSpec { max_lines: 1, ..t.clone() };
        let Ok(l) = self.layout(&one, crate::ui::layout::INPUT_MAX_W) else { return 0.0 };
        let (mut x, mut y) = (0.0f32, 0.0f32);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            if l.HitTestTextPosition(pos, false, &mut x, &mut y, &mut m).is_err() {
                return 0.0;
            }
        }
        x
    }
}
