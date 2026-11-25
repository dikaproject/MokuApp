//! Direct2D / DirectWrite drawing into Composition drawing surfaces.
//! Everything static (text, logo, icons, album art) is rasterised once per DPI into
//! `CompositionDrawingSurface`s; the compositor only moves / fades the resulting sprites.

use windows::Foundation::Size;
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::UI::Composition::{CompositionDrawingSurface, CompositionGraphicsDevice, Compositor};
use windows::Win32::Foundation::{HMODULE, POINT};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Composition::{ICompositionDrawingSurfaceInterop, ICompositorInterop};
use windows::core::{HSTRING, Interface, Result, w};
use windows_numerics::{Matrix3x2, Vector2};

pub type Ctx = ID2D1DeviceContext;

pub struct Gfx {
    factory: ID2D1Factory1,
    dwrite: IDWriteFactory,
    device: CompositionGraphicsDevice,
    /// Font families for Text / Display / Icons, with Windows 10 fallbacks applied.
    families: [HSTRING; 3],
}

pub const fn rgba(hex: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a,
    }
}
pub const WHITE: D2D1_COLOR_F = rgba(0xffffff, 1.0);
pub const WHITE2: D2D1_COLOR_F = rgba(0xffffff, 0.72);
pub const WHITE3: D2D1_COLOR_F = rgba(0xffffff, 0.46);

#[derive(Clone, Copy)]
pub enum Font {
    Text,
    Display,
    Icons,
}

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}
fn v2(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

impl Gfx {
    pub fn new(compositor: &Compositor) -> Result<Self> {
        unsafe {
            let mut d3d = None;
            let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
            // Default: WARP (CPU) device. We only rasterise a few static surfaces once, and WARP
            // keeps GPU driver DLLs out of the process (measured: 25 MB vs 45 MB working set,
            // 12 MB vs 55 MB private). The compositor still animates on the GPU. `--gpu` = hardware.
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
            // Composition draws D2D content through a CompositionGraphicsDevice wrapping our D2D device.
            let device = compositor.cast::<ICompositorInterop>()?.CreateGraphicsDevice(&d2d)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            // Windows 10 has neither Segoe UI Variable nor Segoe Fluent Icons: fall back to
            // Segoe UI / Segoe MDL2 Assets (same code points for the icons we use).
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
            ];
            Ok(Self { factory, dwrite, device, families })
        }
    }

    /// Create a surface of `w`×`h` DIP rasterised at `scale`, and draw into it in DIP coordinates.
    pub fn surface(&self, w: f32, h: f32, scale: f32, draw: impl FnOnce(&Ctx) -> Result<()>) -> Result<CompositionDrawingSurface> {
        let size = Size { Width: (w * scale).ceil(), Height: (h * scale).ceil() };
        let surf = self.device.CreateDrawingSurface(size, DirectXPixelFormat::B8G8R8A8UIntNormalized, DirectXAlphaMode::Premultiplied)?;
        unsafe {
            let interop: ICompositionDrawingSurfaceInterop = surf.cast()?;
            let mut off = POINT::default();
            // BeginDraw hands out a device context into an atlas; `off` is where our pixels start.
            let ctx: ID2D1DeviceContext = interop.BeginDraw(None, &mut off)?;
            ctx.SetDpi(96.0 * scale, 96.0 * scale);
            ctx.SetTransform(&Matrix3x2::translation(off.x as f32 / scale, off.y as f32 / scale));
            ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            ctx.Clear(Some(&rgba(0, 0.0)));
            let r = draw(&ctx);
            interop.EndDraw()?;
            r?;
        }
        Ok(surf)
    }

    /// Run `f` with an extra translation applied (lets item surfaces draw in panel coordinates).
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

    pub fn brush(&self, ctx: &Ctx, c: D2D1_COLOR_F) -> Result<ID2D1SolidColorBrush> {
        unsafe { ctx.CreateSolidColorBrush(&c, None) }
    }

    pub fn linear(&self, ctx: &Ctx, from: (f32, f32), to: (f32, f32), stops: &[(f32, D2D1_COLOR_F)]) -> Result<ID2D1LinearGradientBrush> {
        unsafe {
            let stops: Vec<D2D1_GRADIENT_STOP> = stops.iter().map(|&(position, color)| D2D1_GRADIENT_STOP { position, color }).collect();
            let rt: &ID2D1RenderTarget = ctx;
            let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: v2(from.0, from.1), endPoint: v2(to.0, to.1) };
            ctx.CreateLinearGradientBrush(&props, None, &coll)
        }
    }

    pub fn fill_rrect(&self, ctx: &Ctx, r: D2D_RECT_F, radius: f32, brush: &ID2D1Brush) {
        unsafe { ctx.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, brush) }
    }

    pub fn fill_circle(&self, ctx: &Ctx, cx: f32, cy: f32, r: f32, brush: &ID2D1Brush) {
        unsafe { ctx.FillEllipse(&D2D1_ELLIPSE { point: v2(cx, cy), radiusX: r, radiusY: r }, brush) }
    }

    fn layout(&self, s: &str, font: Font, size: f32, weight: DWRITE_FONT_WEIGHT) -> Result<IDWriteTextLayout> {
        let family = &self.families[font as usize];

        unsafe {
            let fmt = self.dwrite.CreateTextFormat(family, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("id-id"))?;
            fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let text: Vec<u16> = s.encode_utf16().collect();
            let layout = self.dwrite.CreateTextLayout(&text, &fmt, 4096.0, 512.0)?;
            // Tabular figures so the clock / timestamps don't jiggle.
            let typo = self.dwrite.CreateTypography()?;
            typo.AddFontFeature(DWRITE_FONT_FEATURE { nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, parameter: 1 })?;
            layout.SetTypography(&typo, DWRITE_TEXT_RANGE { startPosition: 0, length: text.len() as u32 })?;
            Ok(layout)
        }
    }

    /// Width of a run of text in DIP.
    pub fn measure(&self, s: &str, font: Font, size: f32, weight: DWRITE_FONT_WEIGHT) -> Result<f32> {
        let l = self.layout(s, font, size, weight)?;
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe { l.GetMetrics(&mut m)? };
        Ok(m.widthIncludingTrailingWhitespace)
    }

    /// Draw text with its left edge at `x` and its cap-height centred on `cy`. Returns the width.
    /// `align` 0 = left, 0.5 = centred on x, 1 = right-aligned at x.
    #[allow(clippy::too_many_arguments)]
    pub fn text(&self, ctx: &Ctx, s: &str, font: Font, size: f32, weight: DWRITE_FONT_WEIGHT, color: D2D1_COLOR_F, x: f32, cy: f32, align: f32) -> Result<f32> {
        let l = self.layout(s, font, size, weight)?;
        unsafe {
            let mut m = DWRITE_TEXT_METRICS::default();
            l.GetMetrics(&mut m)?;
            let mut lines = [DWRITE_LINE_METRICS::default(); 1];
            let mut n = 0;
            l.GetLineMetrics(Some(&mut lines), &mut n)?;
            let baseline = lines[0].baseline;
            // Icons are centred on their em box; text on its cap height (~0.7 em for Segoe).
            let top = match font {
                Font::Icons => cy - m.height / 2.0,
                _ => cy + size * 0.35 - baseline,
            };
            let w = m.widthIncludingTrailingWhitespace;
            let brush = self.brush(ctx, color)?;
            ctx.DrawTextLayout(v2(x - w * align, top), &l, &brush, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
            Ok(w)
        }
    }

    fn round_stroke(&self) -> Result<ID2D1StrokeStyle> {
        let props = D2D1_STROKE_STYLE_PROPERTIES {
            startCap: D2D1_CAP_STYLE_ROUND,
            endCap: D2D1_CAP_STYLE_ROUND,
            dashCap: D2D1_CAP_STYLE_ROUND,
            lineJoin: D2D1_LINE_JOIN_ROUND,
            miterLimit: 10.0,
            dashStyle: D2D1_DASH_STYLE_SOLID,
            dashOffset: 0.0,
        };
        let f: &ID2D1Factory = &self.factory;
        unsafe { f.CreateStrokeStyle(&props, None) }
    }

    /// The Moku logo mark (lowercase m on a horizon line) in a `size`×`size` box at (x, y).
    pub fn logo(&self, ctx: &Ctx, x: f32, y: f32, size: f32) -> Result<()> {
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
            let grad = self.linear(ctx, (a.X, 0.0), (b.X, 0.0), &[(0.0, rgba(0x4FD8FF, 1.0)), (0.5, rgba(0x7C6BFF, 1.0)), (1.0, rgba(0xFF5FA2, 1.0))])?;
            ctx.DrawGeometry(&geo, &grad, 9.5 * k, &self.round_stroke()?);
        }
        Ok(())
    }

    /// Album art: rounded square with the orange→pink gradient, optional music-note glyph.
    pub fn art(&self, ctx: &Ctx, x: f32, y: f32, size: f32, radius: f32, note: bool) -> Result<()> {
        let grad = self.linear(ctx, (x, y), (x + size, y + size), &[(0.0, rgba(0xFF8A5C, 1.0)), (1.0, rgba(0xFF3D77, 1.0))])?;
        self.fill_rrect(ctx, rect(x, y, size, size), radius, &grad.cast()?);
        if note {
            let k = size;
            let p = |px: f32, py: f32| v2(x + px * k, y + py * k);
            let white = self.brush(ctx, WHITE)?;
            let sw = 0.042 * k;
            let stroke = self.round_stroke()?;
            unsafe {
                let geo = self.factory.CreatePathGeometry()?;
                let sink = geo.Open()?;
                // stems + beam
                sink.BeginFigure(p(0.42, 0.64), D2D1_FIGURE_BEGIN_HOLLOW);
                sink.AddLine(p(0.42, 0.355));
                sink.AddLine(p(0.655, 0.315));
                sink.AddLine(p(0.655, 0.585));
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
                sink.Close()?;
                ctx.DrawGeometry(&geo, &white, sw, &stroke);
                for (cx, cy) in [(0.36, 0.64), (0.595, 0.585)] {
                    let c = p(cx, cy);
                    ctx.DrawEllipse(&D2D1_ELLIPSE { point: c, radiusX: 0.06 * k, radiusY: 0.06 * k }, &white, sw, None);
                }
            }
        }
        Ok(())
    }

    /// Filled triangle "play" shape pointing right (dir = 1) or left (dir = -1).
    pub fn triangle(&self, ctx: &Ctx, cx: f32, cy: f32, h: f32, dir: f32, brush: &ID2D1Brush) -> Result<()> {
        unsafe {
            let geo = self.factory.CreatePathGeometry()?;
            let sink = geo.Open()?;
            let w = h * 0.8;
            sink.BeginFigure(v2(cx - dir * w / 2.0, cy - h / 2.0), D2D1_FIGURE_BEGIN_FILLED);
            sink.AddLine(v2(cx + dir * w / 2.0, cy));
            sink.AddLine(v2(cx - dir * w / 2.0, cy + h / 2.0));
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close()?;
            ctx.FillGeometry(&geo, brush, None);
        }
        Ok(())
    }
}
