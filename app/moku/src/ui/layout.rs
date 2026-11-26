//! Measured flex-style layout: `Node` tree → draw ops + hit regions, in DIP.
//!
//! Rows / columns: children keep their measured (or fixed) size on the main axis; children with
//! `grow > 0` or `Len::Fill` share what is left; if the fixed children overflow, auto-sized
//! children are shrunk proportionally (text then ellipsises). Cross axis: `Px`, `Fill` /
//! parent `Align::Stretch` = full, else measured and aligned.

use super::*;

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }
    pub fn inset(&self, i: Insets) -> Rect {
        Rect { x: self.x + i.l, y: self.y + i.t, w: (self.w - i.l - i.r).max(0.0), h: (self.h - i.t - i.b).max(0.0) }
    }
    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect { x: self.x + dx, y: self.y + dy, ..*self }
    }
    pub fn cx(&self) -> f32 {
        self.x + self.w / 2.0
    }
    pub fn cy(&self) -> f32 {
        self.y + self.h / 2.0
    }
}

/// Text measurement (DirectWrite in the app, a fake in tests).
pub trait Measure {
    /// Size of `t` laid out in at most `max_w` DIP (wrapping/ellipsis per `t.max_lines`).
    fn text(&self, t: &TextSpec, max_w: f32) -> (f32, f32);
    /// X of the caret before byte `index` of `t.s` on one unbounded line.
    fn caret_x(&self, t: &TextSpec, index: usize) -> f32 {
        let i = index.min(t.s.len());
        let prefix = TextSpec { s: t.s.get(..i).unwrap_or(&t.s).to_string(), max_lines: 1, ..t.clone() };
        if prefix.s.is_empty() { 0.0 } else { self.text(&prefix, INPUT_MAX_W).0 }
    }
}

/// Width text inside an input is measured in (it scrolls; it never wraps or ellipsises).
pub const INPUT_MAX_W: f32 = 100_000.0;

/// A primitive to draw. Coordinates are DIP in the content's space.
#[derive(Clone, PartialEq, Debug)]
pub enum Op {
    Fill { r: Rect, radius: f32, color: Rgba },
    Stroke { r: Rect, radius: f32, color: Rgba, width: f32, dashed: bool },
    /// Text (or an icon when `t.font == Font::Icons`) inside `r`, vertically centred
    /// (single line) or top-aligned (wrapped).
    Text { r: Rect, t: TextSpec },
    Image { r: Rect, radius: f32, img: Image },
    /// Circular arc, fractions of a full turn clockwise from 12 o'clock.
    Arc { cx: f32, cy: f32, radius: f32, width: f32, from: f32, to: f32, color: Rgba },
    Line { pts: Vec<(f32, f32)>, width: f32, color: Rgba },
    /// Filled polygon (closed), e.g. the lightning bolt.
    Poly { pts: Vec<(f32, f32)>, color: Rgba },
    Logo { r: Rect, alpha: f32 },
    /// Clip following ops to a rounded rect until the matching `PopClip`.
    PushClip { r: Rect, radius: f32 },
    PopClip,
}

/// Things the compositor animates itself (not rasterised).
#[derive(Clone, PartialEq, Debug)]
pub enum Live {
    Bars { r: Rect, playing: bool, color: Rgba },
    /// The turning arc of a [`Kind::Spinner`] (its track is an ordinary op).
    Spinner { r: Rect, color: Rgba, thickness: f32 },
    /// The level bar of a charging battery (`Node::charging`): the compositor grows `fill_w`
    /// inside `inner` (corner `radius`), glints across it, pops the lightning `bolt` in and breathes a soft glow behind `r`.
    Charge { r: Rect, inner: Rect, radius: f32, fill_w: f32, bolt: Rect, color: Rgba },
    /// The caret of a focused [`text_input`]: a thin bar the compositor blinks.
    Caret { r: Rect, color: Rgba },
    /// A [`karaoke`] line: the scene draws it (dim base + bright copy) and the compositor sweeps
    /// the bright copy across. The scene compares it apart from the other live items (its anchor
    /// changes with every report of the player).
    Karaoke(Box<super::karaoke::KaraokeLive>),
}

/// Sliders at most this high (DIP) draw in the compact style (thin track, accent fill, knob).
pub const COMPACT_SLIDER_H: f32 = 28.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HitKind {
    Click,
    /// Press / move report `Slide { value }` (0–1 across the rect).
    Slider,
    /// Press-and-move reports `DragStart`.
    Drag,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Hit {
    pub rect: Rect,
    pub action: String,
    pub kind: HitKind,
    /// Accessible name (`Node::name`), for icon-only controls.
    pub name: Option<String>,
}

/// A keyed node laid out on its own (`Node::key`): its ops are not part of the surrounding
/// block, the scene gives it a sprite of its own and animates it.
#[derive(Clone, PartialEq, Debug)]
pub struct Keyed {
    pub spec: KeySpec,
    /// Where it is, in the same content coordinates as the block.
    pub rect: Rect,
    pub ops: Vec<Op>,
}

/// Output of laying out a tree.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Laid {
    pub ops: Vec<Op>,
    pub live: Vec<Live>,
    pub hits: Vec<Hit>,
    /// Keyed nodes (flattened, parents before their keyed descendants).
    pub keyed: Vec<Keyed>,
}

impl Laid {
    /// The innermost hit region at a point (children come after parents, so search backwards).
    pub fn hit_at(&self, x: f32, y: f32) -> Option<&Hit> {
        self.hits.iter().rev().find(|h| h.rect.contains(x, y))
    }
}

/// Width of a battery's level bar inside a track `inner_w` wide: proportional to `value`
/// (0-1, anything else is clamped, NaN counts as empty), but a battery that is not empty keeps
/// a visible sliver (15 % of the track).
pub fn battery_fill_w(inner_w: f32, value: f32) -> f32 {
    let v = if value.is_nan() { 0.0 } else { value.clamp(0.0, 1.0) };
    if v <= 0.0 {
        return 0.0;
    }
    (inner_w * v).max(inner_w * 0.15).min(inner_w)
}

/// The lightning bolt as a polygon filling `r` (top tip, left kink, right kink, bottom tip).
pub fn bolt_points(r: Rect) -> Vec<(f32, f32)> {
    [(0.62, 0.0), (0.10, 0.56), (0.44, 0.56), (0.30, 1.0), (0.90, 0.40), (0.54, 0.40)].iter().map(|&(x, y)| (r.x + r.w * x, r.y + r.h * y)).collect()
}

/// The parts of a battery drawn in `r`: body outline, terminal nub on the right, the track
/// inside the outline (where the level bar goes) and the corner radii.
struct BatteryGeo {
    outline: Rect,
    stroke: f32,
    body_r: f32,
    nub: Rect,
    inner: Rect,
    inner_r: f32,
}

fn battery_geo(r: Rect) -> BatteryGeo {
    let h = r.h;
    let big = h >= 20.0;
    let nub_w = (h * 0.16).round().max(2.0);
    let nub_h = (h * 0.38).round().max(3.0);
    let body = Rect::new(r.x, r.y, (r.w - nub_w - 1.0).max(h), h);
    let stroke = if big { 1.5 } else { 1.0 };
    let pad = stroke + if big { 2.0 } else { 1.0 };
    let inner = body.inset(Insets { l: pad, t: pad, r: pad, b: pad });
    let body_r = (h * 0.3).min(h / 2.0);
    BatteryGeo {
        outline: body.inset(Insets { l: stroke / 2.0, t: stroke / 2.0, r: stroke / 2.0, b: stroke / 2.0 }),
        stroke,
        body_r,
        nub: Rect::new(r.x + r.w - nub_w, r.cy() - nub_h / 2.0, nub_w, nub_h),
        inner,
        inner_r: (body_r - pad + stroke * 0.5).max(1.5).min(inner.h / 2.0),
    }
}

fn input_text(i: &InputSpec, s: String) -> TextSpec {
    TextSpec { s, font: Font::Text, size: i.size, weight: Weight::Regular, color: i.color, max_lines: 1, align: 0.0 }
}

fn input_line_h(i: &InputSpec) -> f32 {
    (i.size * 1.45).ceil()
}

/// Ops of a text input in `r`: selection, text (scrolled so the caret stays visible), the IME
/// composition underline, clipped to the field; the caret goes to `out.live`.
fn place_input(i: &InputSpec, r: Rect, m: &dyn Measure, out: &mut Laid, a: f32) {
    let t = input_text(i, i.text.clone());
    let lh = input_line_h(i);
    let top = r.cy() - lh / 2.0;
    let x_at = |idx: usize| m.caret_x(&t, idx);
    let cx = x_at(i.caret);
    // Scroll: keep the caret (and 2 DIP after it) inside the field.
    let off = (cx + 2.0 - r.w).max(0.0);
    out.ops.push(Op::PushClip { r: Rect::new(r.x, r.y - 2.0, r.w, r.h + 4.0), radius: 0.0 });
    if let Some((s, e)) = i.selection {
        let (xs, xe) = (x_at(s) - off, x_at(e) - off);
        out.ops.push(Op::Fill { r: Rect::new(r.x + xs, top, (xe - xs).max(1.0), lh), radius: 3.0, color: i.accent.fade(0.45 * a) });
    }
    if i.text.is_empty() {
        if !i.placeholder.is_empty() {
            let p = TextSpec { color: color::TEXT3.fade(a), ..input_text(i, i.placeholder.clone()) };
            out.ops.push(Op::Text { r, t: p });
        }
    } else {
        let w = m.text(&t, INPUT_MAX_W).0;
        let mut tt = t.clone();
        tt.color = i.color.fade(a);
        out.ops.push(Op::Text { r: Rect::new(r.x - off, r.y, w + 8.0, r.h), t: tt });
    }
    if let Some((s, e)) = i.composition {
        let (xs, xe) = (x_at(s) - off, x_at(e) - off);
        out.ops.push(Op::Fill { r: Rect::new(r.x + xs, top + lh - 1.5, (xe - xs).max(1.0), 1.5), radius: 0.0, color: i.color.fade(0.8 * a) });
    }
    out.ops.push(Op::PopClip);
    if i.focused {
        let x = (r.x + cx - off).min(r.x + r.w - 1.5);
        out.live.push(Live::Caret { r: Rect::new(x, top, 1.5, lh), color: i.color.fade(a) });
    }
}

/// A karaoke line: nothing is rasterised with the block; the scene gets the line, where its words
/// are and when they are sung, and draws and animates it.
fn place_karaoke(k: &KaraokeSpec, r: Rect, m: &dyn Measure, out: &mut Laid, a: f32) {
    use super::karaoke::{KaraokeLive, segs_for};
    let text = TextSpec { color: k.text.color.fade(a), max_lines: 1, ..k.text.clone() };
    let full_w = if text.s.is_empty() { 0.0 } else { m.text(&text, INPUT_MAX_W).0 };
    let text_w = full_w.min(r.w);
    let segs = segs_for(&k.words, &|b| m.caret_x(&text, b), r.w, text_w);
    out.live.push(Live::Karaoke(Box::new(KaraokeLive { r, id: k.id.clone(), text, dim: k.dim.fade(a), segs, text_w, anchor: k.anchor, rise: k.rise })));
}

fn is_container(n: &Node) -> bool {
    matches!(n.kind, Kind::Row(_) | Kind::Col(_) | Kind::Stack(_))
}

/// Intrinsic content size (without padding) for leaves.
fn leaf_size(n: &Node, m: &dyn Measure, max_w: f32) -> (f32, f32) {
    match &n.kind {
        Kind::Text(t) => m.text(t, max_w.max(0.0)),
        Kind::Icon { size, .. } => (*size, *size),
        Kind::Image { .. } => (24.0, 24.0),
        Kind::Toggle { .. } => (40.0, 24.0),
        Kind::Slider { .. } => (120.0, 36.0),
        Kind::Progress { thickness, .. } => (0.0, *thickness),
        Kind::Ring { .. } => (20.0, 20.0),
        Kind::Sparkline { .. } => (0.0, 24.0),
        Kind::Bars { .. } => (25.0, 20.0),
        Kind::Spinner { .. } => (20.0, 20.0),
        Kind::Battery { .. } => (26.0, 13.0),
        Kind::Logo => (24.0, 24.0),
        Kind::Input(i) => {
            let s = if i.text.is_empty() { &i.placeholder } else { &i.text };
            let w = if s.is_empty() { 0.0 } else { m.text(&input_text(i, s.clone()), max_w).0 };
            (w + 2.0, input_line_h(i))
        }
        Kind::Karaoke(k) => m.text(&k.text, max_w.max(0.0)),
        Kind::Spacer | Kind::Empty => (0.0, 0.0),
        Kind::Row(_) | Kind::Col(_) | Kind::Stack(_) => (0.0, 0.0),
    }
}

/// Outer size of `n` (padding included) when offered at most `max_w` DIP of width.
pub fn measure(n: &Node, m: &dyn Measure, max_w: f32) -> (f32, f32) {
    let px_w = if let Len::Px(w) = n.w { Some(w) } else { None };
    let px_h = if let Len::Px(h) = n.h { Some(h) } else { None };
    if let (Some(w), Some(h)) = (px_w, px_h) {
        return (w, h);
    }
    let max_w = n.max_w.map_or(max_w, |m| m.min(max_w));
    let outer_max = px_w.unwrap_or(max_w);
    let inner_max = (outer_max - n.pad.l - n.pad.r).max(0.0);
    let (cw, ch) = match &n.kind {
        Kind::Row(c) => {
            let mut w = 0.0f32;
            let mut h = 0.0f32;
            for ch in c {
                let (a, b) = measure(ch, m, inner_max);
                w += a;
                h = h.max(b);
            }
            (w + n.gap * c.len().saturating_sub(1) as f32, h)
        }
        Kind::Col(c) => {
            let mut w = 0.0f32;
            let mut h = 0.0f32;
            for ch in c {
                let (a, b) = measure(ch, m, inner_max);
                w = w.max(a);
                h += b;
            }
            (w, h + n.gap * c.len().saturating_sub(1) as f32)
        }
        Kind::Stack(c) => c.iter().fold((0.0f32, 0.0f32), |(w, h), ch| {
            let (a, b) = measure(ch, m, inner_max);
            (w.max(a), h.max(b))
        }),
        _ => leaf_size(n, m, inner_max),
    };
    let w = px_w.unwrap_or_else(|| (cw + n.pad.l + n.pad.r).min(max_w.max(0.0)));
    let h = px_h.unwrap_or(ch + n.pad.t + n.pad.b);
    (w, h)
}

fn main_len(n: &Node, horizontal: bool) -> Len {
    if horizontal { n.w } else { n.h }
}

fn cross_len(n: &Node, horizontal: bool) -> Len {
    if horizontal { n.h } else { n.w }
}

/// Rects of the direct children of a row / column / stack placed in `rect`.
pub fn child_rects(n: &Node, rect: Rect, m: &dyn Measure) -> Vec<Rect> {
    let inner = rect.inset(n.pad);
    let (children, horizontal) = match &n.kind {
        Kind::Row(c) => (c, true),
        Kind::Col(c) => (c, false),
        Kind::Stack(c) => {
            return c
                .iter()
                .map(|ch| {
                    let (mw, mh) = measure(ch, m, inner.w);
                    let w = match ch.w {
                        Len::Px(w) => w,
                        Len::Fill => inner.w,
                        Len::Auto if n.align == Align::Stretch => inner.w,
                        Len::Auto => mw.min(inner.w),
                    };
                    let h = match ch.h {
                        Len::Px(h) => h,
                        Len::Fill => inner.h,
                        Len::Auto if n.align == Align::Stretch => inner.h,
                        Len::Auto => mh,
                    };
                    let k = match n.align {
                        Align::Start => 0.0,
                        Align::Center | Align::Stretch => 0.5,
                        Align::End => 1.0,
                    };
                    Rect::new(inner.x + (inner.w - w) * k, inner.y + (inner.h - h) * k, w, h)
                })
                .collect();
        }
        _ => return vec![],
    };
    if children.is_empty() {
        return vec![];
    }
    let main_avail = if horizontal { inner.w } else { inner.h };
    let cross_avail = if horizontal { inner.h } else { inner.w };
    let gaps = n.gap * (children.len() - 1) as f32;

    // Pass 1: main-axis sizes.
    #[derive(Clone, Copy, PartialEq)]
    enum S {
        Fixed,
        Auto,
        Flex(f32),
    }
    let mut sizes: Vec<f32> = Vec::with_capacity(children.len());
    let mut kinds: Vec<S> = Vec::with_capacity(children.len());
    for ch in children {
        let ml = main_len(ch, horizontal);
        if let Len::Px(p) = ml {
            sizes.push(p);
            kinds.push(S::Fixed);
        } else if ch.grow > 0.0 || ml == Len::Fill {
            sizes.push(0.0);
            kinds.push(S::Flex(if ch.grow > 0.0 { ch.grow } else { 1.0 }));
        } else {
            let (w, h) = if horizontal { measure(ch, m, main_avail) } else { measure(ch, m, cross_avail) };
            sizes.push(if horizontal { w } else { h });
            kinds.push(S::Auto);
        }
    }
    let fixed: f32 = sizes.iter().sum::<f32>() + gaps;
    let mut leftover = main_avail - fixed;
    if leftover < 0.0 && horizontal {
        // Shrink auto-sized children proportionally (text ellipsises).
        let auto_sum: f32 = sizes.iter().zip(&kinds).filter(|(_, k)| **k == S::Auto).map(|(s, _)| *s).sum();
        if auto_sum > 0.0 {
            let k = ((auto_sum + leftover) / auto_sum).max(0.0);
            for (s, kd) in sizes.iter_mut().zip(&kinds) {
                if *kd == S::Auto {
                    *s *= k;
                }
            }
        }
        leftover = 0.0;
    }
    let leftover = leftover.max(0.0);
    let total_weight: f32 = kinds.iter().map(|k| if let S::Flex(w) = k { *w } else { 0.0 }).sum();
    if total_weight > 0.0 {
        for (s, k) in sizes.iter_mut().zip(&kinds) {
            if let S::Flex(w) = k {
                *s = leftover * w / total_weight;
            }
        }
    }
    let used: f32 = sizes.iter().sum::<f32>() + gaps;
    let free = if total_weight > 0.0 { 0.0 } else { (main_avail - used).max(0.0) };
    let (mut pos, between) = match n.justify {
        Justify::Start => (0.0, 0.0),
        Justify::Center => (free / 2.0, 0.0),
        Justify::End => (free, 0.0),
        Justify::SpaceBetween if children.len() > 1 => (0.0, free / (children.len() - 1) as f32),
        Justify::SpaceBetween => (free / 2.0, 0.0),
    };

    // Pass 2: cross axis and positions.
    let mut out = Vec::with_capacity(children.len());
    for (ch, &main) in children.iter().zip(&sizes) {
        let cl = cross_len(ch, horizontal);
        let cross = match cl {
            Len::Px(p) => p,
            Len::Fill => cross_avail,
            Len::Auto if n.align == Align::Stretch => cross_avail,
            Len::Auto => {
                if horizontal {
                    measure(ch, m, main).1
                } else {
                    measure(ch, m, cross_avail).0.min(cross_avail)
                }
            }
        };
        let k = match n.align {
            Align::Start | Align::Stretch => 0.0,
            Align::Center => 0.5,
            Align::End => 1.0,
        };
        let off = (cross_avail - cross) * k;
        let r = if horizontal {
            Rect::new(inner.x + pos, inner.y + off, main, cross)
        } else {
            Rect::new(inner.x + off, inner.y + pos, cross, main)
        };
        out.push(r);
        pos += main + n.gap + between;
    }
    out
}

/// Lay out `n` at its measured size (offered `max_w`) with its top-left at `(x, y)`.
/// Returns the size used and the ops / hits.
pub fn layout(n: &Node, m: &dyn Measure, x: f32, y: f32, max_w: f32) -> ((f32, f32), Laid) {
    let (w, h) = measure(n, m, max_w);
    let mut out = Laid::default();
    place(n, Rect::new(x, y, w, h), m, &mut out, 1.0);
    ((w, h), out)
}

/// Lay out `n` exactly into `rect`, appending to `out`. `alpha` = inherited opacity.
/// A keyed node (`Node::key`) goes to `out.keyed` with its own ops; its hits still go to
/// `out.hits`, in tree order.
pub fn place(n: &Node, rect: Rect, m: &dyn Measure, out: &mut Laid, alpha: f32) {
    match n.keyed.as_deref() {
        Some(spec) if !spec.key.is_empty() => {
            let mut sub = Laid::default();
            place_node(n, rect, m, &mut sub, alpha);
            out.hits.append(&mut sub.hits);
            out.live.append(&mut sub.live);
            out.keyed.push(Keyed { spec: spec.clone(), rect, ops: sub.ops });
            out.keyed.append(&mut sub.keyed);
        }
        _ => place_node(n, rect, m, out, alpha),
    }
}

fn place_node(n: &Node, rect: Rect, m: &dyn Measure, out: &mut Laid, alpha: f32) {
    let a = alpha * n.opacity;
    let radius = n.radius.min(rect.w.min(rect.h) / 2.0);
    if let Some(bg) = n.bg {
        out.ops.push(Op::Fill { r: rect, radius, color: bg.fade(a) });
    }
    if let Some((c, wdt)) = n.border {
        out.ops.push(Op::Stroke { r: rect, radius, color: c.fade(a), width: wdt, dashed: n.dashed });
    }
    if let Some(id) = &n.action {
        let kind = if matches!(n.kind, Kind::Slider { .. }) {
            HitKind::Slider
        } else if n.draggable {
            HitKind::Drag
        } else {
            HitKind::Click
        };
        out.hits.push(Hit { rect, action: id.clone(), kind, name: n.name.clone() });
    }
    if is_container(n) {
        let children = match &n.kind {
            Kind::Row(c) | Kind::Col(c) | Kind::Stack(c) => c,
            _ => unreachable!(),
        };
        for (ch, r) in children.iter().zip(child_rects(n, rect, m)) {
            place(ch, r, m, out, a);
        }
        return;
    }
    let r = rect.inset(n.pad);
    match &n.kind {
        Kind::Text(t) => {
            let mut t = t.clone();
            t.color = t.color.fade(a);
            out.ops.push(Op::Text { r, t });
        }
        Kind::Icon { glyph, size, color } => {
            let t = TextSpec { s: glyph.clone(), font: Font::Icons, size: *size, weight: Weight::Regular, color: color.fade(a), max_lines: 1, align: 0.5 };
            out.ops.push(Op::Text { r, t });
        }
        Kind::Image { img, radius } => out.ops.push(Op::Image { r, radius: radius.min(r.w.min(r.h) / 2.0), img: img.clone() }),
        Kind::Toggle { on, color } => {
            let t = Rect::new(r.cx() - 20.0, r.cy() - 12.0, 40.0, 24.0);
            let track = if *on { *color } else { Rgba::hex(0xffffff, 0.18) };
            out.ops.push(Op::Fill { r: t, radius: 12.0, color: track.fade(a) });
            let kx = if *on { t.x + t.w - 22.0 } else { t.x + 2.0 };
            out.ops.push(Op::Fill { r: Rect::new(kx, t.y + 2.0, 20.0, 20.0), radius: 10.0, color: color::WHITE.fade(a) });
        }
        // A compact slider (a list row, e.g. the mixer): a thin track, an accent fill and a round
        // knob. The thick Control Center slider fills its whole bar, which at 100 % is one solid
        // white pill with no track to tell from the fill; this one always shows both.
        Kind::Slider { value, color, .. } if r.h <= COMPACT_SLIDER_H => {
            let v = value.clamp(0.0, 1.0);
            let (track_h, knob) = (6.0, 16.0);
            let track = Rect::new(r.x, r.cy() - track_h / 2.0, r.w, track_h);
            let kx = r.x + knob / 2.0 + (r.w - knob).max(0.0) * v;
            out.ops.push(Op::Fill { r: track, radius: track_h / 2.0, color: Rgba::hex(0xffffff, 0.20).fade(a) });
            if v > 0.0 {
                out.ops.push(Op::Fill { r: Rect::new(track.x, track.y, (kx - track.x).max(track_h), track_h), radius: track_h / 2.0, color: color.fade(a) });
            }
            out.ops.push(Op::Fill { r: Rect::new(kx - knob / 2.0, r.cy() - knob / 2.0, knob, knob), radius: knob / 2.0, color: color::WHITE.fade(a) });
        }
        Kind::Slider { value, icon, color } => {
            let rad = r.h / 2.0;
            out.ops.push(Op::Fill { r, radius: rad, color: color::FILL3.fade(a) });
            out.ops.push(Op::PushClip { r, radius: rad });
            // The fill never shrinks below the icon's round cap (a slider at 0 % must not look
            // switched off), and a short dark grip marks where it ends, so the value reads even
            // at 100 % (track fully covered).
            let fw = (r.w * value.clamp(0.0, 1.0)).max(r.h).min(r.w);
            out.ops.push(Op::Fill { r: Rect::new(r.x, r.y, fw, r.h), radius: 0.0, color: color.fade(a) });
            let gh = (r.h * 0.42).round();
            out.ops.push(Op::Fill { r: Rect::new(r.x + fw - 11.0, r.cy() - gh / 2.0, 3.0, gh), radius: 1.5, color: color::INK.fade(a * 0.28) });
            out.ops.push(Op::PopClip);
            if let Some(g) = icon {
                let t = TextSpec { s: g.clone(), font: Font::Icons, size: 14.0, weight: Weight::Regular, color: color::INK.fade(a * 0.85), max_lines: 1, align: 0.5 };
                out.ops.push(Op::Text { r: Rect::new(r.x + 8.0, r.y, 28.0, r.h), t });
            }
        }
        Kind::Progress { value, color, track, thickness } => {
            let t = Rect::new(r.x, r.cy() - thickness / 2.0, r.w, *thickness);
            out.ops.push(Op::Fill { r: t, radius: thickness / 2.0, color: track.fade(a) });
            if *value > 0.0 {
                out.ops.push(Op::Fill { r: Rect::new(t.x, t.y, (t.w * value).max(*thickness), t.h), radius: thickness / 2.0, color: color.fade(a) });
            }
        }
        Kind::Ring { value, color, track, thickness } => {
            let rad = (r.w.min(r.h) - thickness) / 2.0;
            out.ops.push(Op::Arc { cx: r.cx(), cy: r.cy(), radius: rad, width: *thickness, from: 0.0, to: 1.0, color: track.fade(a) });
            if *value > 0.0 {
                out.ops.push(Op::Arc { cx: r.cx(), cy: r.cy(), radius: rad, width: *thickness, from: 0.0, to: *value, color: color.fade(a) });
            }
        }
        Kind::Sparkline { values, color, range } => {
            if values.len() >= 2 {
                let (lo, hi) = range.unwrap_or_else(|| {
                    let lo = values.iter().cloned().fold(f32::INFINITY, f32::min);
                    let hi = values.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                    (lo, hi)
                });
                let span = if (hi - lo).abs() < 1e-6 { 1.0 } else { hi - lo };
                let step = r.w / (values.len() - 1) as f32;
                let pts = values
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (r.x + i as f32 * step, r.y + r.h - 1.0 - ((v - lo) / span).clamp(0.0, 1.0) * (r.h - 2.0)))
                    .collect();
                out.ops.push(Op::Line { pts, width: 1.5, color: color.fade(a) });
            }
        }
        Kind::Bars { playing, color } => out.live.push(Live::Bars { r, playing: *playing, color: color.fade(a) }),
        Kind::Spinner { color, track, thickness } => {
            let rad = (r.w.min(r.h) - thickness) / 2.0;
            out.ops.push(Op::Arc { cx: r.cx(), cy: r.cy(), radius: rad, width: *thickness, from: 0.0, to: 1.0, color: track.fade(a) });
            out.live.push(Live::Spinner { r, color: color.fade(a), thickness: *thickness });
        }
        Kind::Battery { value, color, charging } => {
            let g = battery_geo(r);
            out.ops.push(Op::Stroke { r: g.outline, radius: g.body_r, color: Rgba::hex(0xffffff, 0.55).fade(a), width: g.stroke, dashed: false });
            out.ops.push(Op::Fill { r: g.nub, radius: g.nub.w / 2.0, color: Rgba::hex(0xffffff, 0.55).fade(a) });
            out.ops.push(Op::Fill { r: g.inner, radius: g.inner_r, color: Rgba::hex(0xffffff, 0.10).fade(a) });
            let fill_w = battery_fill_w(g.inner.w, *value);
            if *charging {
                let bh = (g.inner.h - 3.0).clamp(0.0, 16.0);
                let bolt = Rect::new(g.inner.cx() - bh * 0.31, g.inner.cy() - bh / 2.0, bh * 0.62, bh);
                out.live.push(Live::Charge { r, inner: g.inner, radius: g.inner_r, fill_w, bolt, color: color.fade(a) });
            } else if fill_w > 0.0 {
                out.ops.push(Op::Fill { r: Rect::new(g.inner.x, g.inner.y, fill_w, g.inner.h), radius: g.inner_r, color: color.fade(a) });
            }
        }
        Kind::Logo => out.ops.push(Op::Logo { r, alpha: a }),
        Kind::Input(i) => place_input(i, r, m, out, a),
        Kind::Karaoke(k) => place_karaoke(k, r, m, out, a),
        Kind::Spacer | Kind::Empty | Kind::Row(_) | Kind::Col(_) | Kind::Stack(_) => {}
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// 7 DIP per character, line height = 1.3 × size, wraps by characters.
    pub struct Fake;
    impl Measure for Fake {
        fn text(&self, t: &TextSpec, max_w: f32) -> (f32, f32) {
            let natural = t.s.chars().count() as f32 * 7.0;
            let line_h = (t.size * 1.3).round();
            if natural <= max_w || t.max_lines <= 1 {
                return (natural.min(max_w), line_h);
            }
            let lines = ((natural / max_w).ceil() as u32).min(t.max_lines);
            (max_w, line_h * lines as f32)
        }
    }

    #[test]
    fn row_measures_children_gaps_and_padding() {
        let n = row([text("abcd"), icon(icons::PLAY), text("xy")]).gap(10.0).pad_xy(14.0, 0.0);
        let (w, h) = measure(&n, &Fake, 1000.0);
        assert_eq!(w, 14.0 + 28.0 + 10.0 + 14.0 + 10.0 + 14.0 + 14.0);
        assert_eq!(h, 17.0);
    }

    #[test]
    fn grow_takes_leftover_and_justify() {
        let n = row([text("ab").w(20.0), spacer(), text("cd").w(30.0)]).w(200.0).h(36.0);
        let r = child_rects(&n, Rect::new(0.0, 0.0, 200.0, 36.0), &Fake);
        assert_eq!(r[1].w, 150.0);
        assert_eq!(r[2].x, 170.0);
        let n = row([text("ab").w(20.0), text("cd").w(30.0)]).justify(Justify::SpaceBetween);
        let r = child_rects(&n, Rect::new(0.0, 0.0, 200.0, 36.0), &Fake);
        assert_eq!(r[1].x, 170.0);
        // Centred vertically by default.
        assert_eq!(r[0].y, (36.0 - 17.0) / 2.0);
    }

    #[test]
    fn overflowing_text_is_shrunk() {
        let n = row([icon(icons::PLAY).w(20.0), text("a".repeat(100))]);
        let r = child_rects(&n, Rect::new(0.0, 0.0, 120.0, 20.0), &Fake);
        assert_eq!(r[1].w, 100.0);
    }

    #[test]
    fn col_stacks_and_stretches() {
        let n = col([text("abc"), progress(0.5), text("de")]).gap(4.0).align(Align::Stretch);
        let (w, h) = measure(&n, &Fake, 300.0);
        assert_eq!((w, h), (21.0, 17.0 + 4.0 + 3.0 + 4.0 + 17.0));
        let r = child_rects(&n, Rect::new(10.0, 10.0, 300.0, h), &Fake);
        assert_eq!(r[1], Rect::new(10.0, 10.0 + 21.0, 300.0, 3.0));
    }

    #[test]
    fn max_w_caps_auto_width() {
        let n = col([text("a".repeat(50))]).max_w(140.0);
        assert_eq!(measure(&n, &Fake, 1000.0).0, 140.0);
        let n = col([text("abc")]).max_w(140.0);
        assert_eq!(measure(&n, &Fake, 1000.0).0, 21.0);
    }

    #[test]
    fn wrapped_text_grows_in_height() {
        let n = col([text("a".repeat(30)).lines(2)]).w(140.0);
        let (_, h) = measure(&n, &Fake, 1000.0);
        assert_eq!(h, 34.0);
    }

    #[test]
    fn a_compact_slider_shows_a_track_a_fill_and_a_knob_even_at_full() {
        let accent = Rgba::hex(0x0A84FF, 1.0);
        let fills = |v: f32| -> Vec<(Rect, Rgba)> {
            let n = slider(v).w(200.0).h(22.0).color(accent);
            let (_, laid) = layout(&n, &Fake, 0.0, 0.0, 200.0);
            laid.ops.iter().filter_map(|o| if let Op::Fill { r, color, .. } = o { Some((*r, *color)) } else { None }).collect()
        };
        // Full: the track is 6 high and as wide as the slider; the accent fill and the white
        // knob lie on it, so the bar is never one solid colour.
        let f = fills(1.0);
        assert_eq!(f.len(), 3, "track, fill, knob");
        assert_eq!((f[0].0.h, f[0].0.w), (6.0, 200.0));
        assert_eq!(f[1].1, accent, "the fill is the accent colour");
        assert_ne!(f[0].1, f[1].1, "the track differs from the fill");
        assert_eq!((f[2].0.w, f[2].0.h), (16.0, 16.0));
        assert_eq!(f[2].0.x + f[2].0.w, 200.0, "the knob ends at the right edge at 100 %");
        // Half: the fill reaches the knob's centre, the track still shows to its right.
        let f = fills(0.5);
        assert!((f[1].0.x + f[1].0.w - (f[2].0.x + 8.0)).abs() < 0.01);
        assert!(f[1].0.w < f[0].0.w);
        // Zero: no fill at all, the knob sits at the left end.
        let f = fills(0.0);
        assert_eq!(f.len(), 2);
        assert_eq!(f[1].0.x, 0.0);
        // The thick Control Center slider is unchanged (36 high: track + fill + grip).
        let (_, laid) = layout(&slider(0.5).w(200.0), &Fake, 0.0, 0.0, 200.0);
        assert_eq!(laid.ops.iter().filter(|o| matches!(o, Op::Fill { r, .. } if r.h == 36.0)).count(), 2);
    }

    /// The tab bar: whatever the number of enabled tabs, every inactive tab is a slot of the same
    /// width and their icons are evenly spaced (also next to the active capsule).
    #[test]
    fn tab_bar_spacing_is_even_for_any_number_of_tabs() {
        use super::super::widgets::{TabItem, tab_bar};
        let ids = ["media", "today", "health", "control", "shelf", "timer", "clipboard", "capture", "monitor", "notify", "phone"];
        let glyphs = [icons::MUSIC, icons::CALENDAR, icons::CLOUD, icons::SLIDERS, icons::SHELF, icons::TIMER, icons::CLIPBOARD, icons::CAMERA, icons::PULSE, icons::BELL, icons::PHONE];
        for n in 2..=11usize {
            for active in [None, Some(0), Some(n / 2), Some(n - 1)] {
                let tabs: Vec<TabItem> = (0..n).map(|i| TabItem { id: ids[i], glyph: glyphs[i], label: "Label", dimmed: false }).collect();
                let bar = tab_bar(&tabs, active.map(|i| ids[i])).w(610.0);
                let (_, laid) = layout(&bar, &Fake, 0.0, 0.0, 610.0);
                let slots: Vec<(String, Rect)> = laid.hits.iter().filter(|h| h.action.starts_with("tab.") && h.action != "tab.bar").map(|h| (h.action.clone(), h.rect)).collect();
                assert_eq!(slots.len(), n, "n={n}");
                let inactive: Vec<&(String, Rect)> = slots.iter().filter(|(a, _)| Some(format!("tab.{}", ids[active.unwrap_or(99).min(10)])) != Some(a.clone()) || active.is_none()).collect();
                // All inactive slots have the same width.
                let w0 = inactive[0].1.w;
                assert!(inactive.iter().all(|(_, r)| (r.w - w0).abs() < 0.01), "n={n} active={active:?}: unequal slots {:?}", inactive.iter().map(|(_, r)| r.w).collect::<Vec<_>>());
                assert!(w0 >= 30.0, "n={n}: a slot is at least an icon wide ({w0})");
                // Neighbouring slots touch (no gaps) and the bar is filled edge to edge.
                let first = slots.first().unwrap().1;
                let last = slots.last().unwrap().1;
                assert!((first.x - 4.0).abs() < 0.01 && (last.x + last.w - 606.0).abs() < 0.5, "n={n} active={active:?}: {first:?} .. {last:?}");
                for pair in slots.windows(2) {
                    assert!((pair[0].1.x + pair[0].1.w - pair[1].1.x).abs() < 0.01, "n={n}: a gap between tabs");
                }
            }
        }
    }

    #[test]
    fn hits_are_innermost_first() {
        let n = row([text("open").action("demo.open"), icon(icons::PLAY).action("demo.play")]).action("demo.pill").gap(8.0);
        let (_, laid) = layout(&n, &Fake, 0.0, 0.0, 500.0);
        assert_eq!(laid.hit_at(5.0, 5.0).unwrap().action, "demo.open");
        assert_eq!(laid.hit_at(30.0 + 8.0 + 3.0, 5.0).unwrap().action, "demo.play");
        assert_eq!(laid.hits.len(), 3);
        let s = slider(0.3).action("demo.vol");
        let (_, laid) = layout(&s, &Fake, 0.0, 0.0, 200.0);
        assert_eq!(laid.hits[0].kind, HitKind::Slider);
    }

    #[test]
    fn keyed_nodes_get_their_own_ops_and_keep_their_hits() {
        let tile = |k: &str| col([text("file")]).w(40.0).h(20.0).bg(color::FILL).action(format!("demo.f:{k}")).key(format!("t.{k}")).enter(Anim::pop());
        let n = row([tile("a"), text("plain"), tile("b").child(icon(icons::CLOSE).key("t.b.x"))]).gap(10.0);
        let (_, laid) = layout(&n, &Fake, 0.0, 0.0, 500.0);
        // Only the plain text stays in the block's ops.
        assert_eq!(laid.ops.len(), 1);
        assert!(matches!(&laid.ops[0], Op::Text { t, .. } if t.s == "plain"));
        let keys: Vec<&str> = laid.keyed.iter().map(|k| k.spec.key.as_str()).collect();
        assert_eq!(keys, ["t.a", "t.b", "t.b.x"], "parents before their keyed children");
        assert_eq!(laid.keyed[0].rect, Rect::new(0.0, 0.0, 40.0, 20.0));
        assert_eq!(laid.keyed[1].rect.x, 40.0 + 10.0 + 35.0 + 10.0);
        // A keyed node's own ops: background + text; its keyed child is separate.
        assert_eq!(laid.keyed[0].ops.len(), 2);
        assert_eq!(laid.keyed[0].spec.enter, Some(Anim::pop()));
        // Hits stay in the block, in tree order, so clicks and drags work as before.
        assert_eq!(laid.hits.iter().map(|h| h.action.as_str()).collect::<Vec<_>>(), ["demo.f:a", "demo.f:b"]);
        // A spec without a key (enter() alone) is not keyed.
        let (_, laid) = layout(&text("x").enter(Anim::fade()), &Fake, 0.0, 0.0, 100.0);
        assert!(laid.keyed.is_empty());
        assert_eq!(laid.ops.len(), 1);
    }

    #[test]
    fn battery_fill_width_follows_the_level() {
        assert_eq!(battery_fill_w(40.0, 0.0), 0.0);
        assert_eq!(battery_fill_w(40.0, 1.0), 40.0);
        assert_eq!(battery_fill_w(40.0, 0.5), 20.0);
        // Over / under range and NaN never leave the track.
        assert_eq!(battery_fill_w(40.0, 7.0), 40.0);
        assert_eq!(battery_fill_w(40.0, -1.0), 0.0);
        assert_eq!(battery_fill_w(40.0, f32::NAN), 0.0);
        // A nearly empty battery still shows a sliver (at least the round cap).
        assert!(battery_fill_w(40.0, 0.01) >= 6.0);
    }

    #[test]
    fn static_battery_is_all_ops() {
        let (size, laid) = layout(&battery(0.5).size(22.0, 11.0), &Fake, 0.0, 0.0, 100.0);
        assert_eq!(size, (22.0, 11.0));
        assert!(laid.live.is_empty());
        // outline, nub, fill
        assert!(laid.ops.iter().any(|o| matches!(o, Op::Stroke { .. })));
        let fills = laid.ops.iter().filter(|o| matches!(o, Op::Fill { .. })).count();
        assert_eq!(fills, 3, "nub + track + level");
    }

    #[test]
    fn charging_battery_hands_the_fill_to_the_compositor() {
        let n = battery(0.48).size(54.0, 26.0).charging();
        let (_, laid) = layout(&n, &Fake, 0.0, 0.0, 200.0);
        assert_eq!(laid.live.len(), 1);
        let Live::Charge { inner, fill_w, .. } = &laid.live[0] else { panic!("expected a Charge") };
        assert!(inner.w > 30.0 && inner.h > 12.0, "track inside the outline: {inner:?}");
        assert!((fill_w - battery_fill_w(inner.w, 0.48)).abs() < 1e-3);
        // The fill itself is not an op (the compositor draws and animates it): outline, nub, track.
        assert!(laid.ops.iter().any(|o| matches!(o, Op::Stroke { .. })));
        assert_eq!(laid.ops.iter().filter(|o| matches!(o, Op::Fill { .. })).count(), 2, "nub + track");
        // Changing the level changes the live item (so the scene replays), nothing else does.
        let (_, other) = layout(&battery(0.9).size(54.0, 26.0).charging(), &Fake, 0.0, 0.0, 200.0);
        assert_ne!(laid.live, other.live);
        let (_, same) = layout(&n, &Fake, 0.0, 0.0, 200.0);
        assert_eq!(laid.live, same.live);
    }

    #[test]
    fn bolt_polygon_stays_inside_its_box() {
        let r = Rect::new(10.0, 20.0, 9.0, 15.0);
        let pts = bolt_points(r);
        assert_eq!(pts.len(), 6);
        assert!(pts.iter().all(|&(x, y)| x >= r.x && x <= r.x + r.w && y >= r.y && y <= r.y + r.h), "{pts:?}");
        // Top tip at the top edge, bottom tip at the bottom edge.
        assert!(pts.iter().any(|p| (p.1 - r.y).abs() < 1e-4) && pts.iter().any(|p| (p.1 - (r.y + r.h)).abs() < 1e-4));
    }

    #[test]
    fn charging_bolt_sits_in_the_middle_of_the_track() {
        let (_, laid) = layout(&battery(0.5).size(54.0, 26.0).charging(), &Fake, 0.0, 0.0, 200.0);
        let Live::Charge { inner, bolt, .. } = &laid.live[0] else { panic!("expected a Charge") };
        assert!(bolt.x >= inner.x && bolt.y >= inner.y && bolt.x + bolt.w <= inner.x + inner.w && bolt.y + bolt.h <= inner.y + inner.h, "{bolt:?} in {inner:?}");
        assert!((bolt.cx() - inner.cx()).abs() < 0.01 && (bolt.cy() - inner.cy()).abs() < 0.01);
        assert!(bolt.h > bolt.w, "a bolt is taller than wide");
    }

    #[test]
    fn text_input_draws_caret_selection_and_scrolls() {
        let mut e = TextEdit::new();
        e.set_text("abc");
        // Not focused: no caret; the text is drawn, clipped to the field.
        let n = text_input("demo.q", &e).w(100.0);
        let (_, laid) = layout(&n, &Fake, 0.0, 0.0, 300.0);
        assert!(laid.live.is_empty());
        assert!(laid.ops.iter().any(|o| matches!(o, Op::Text { t, .. } if t.s == "abc")));
        assert_eq!(laid.hits[0].action, "demo.q");
        // Focused: a caret after "abc" (7 DIP per char in the fake).
        e.focused = true;
        let (_, laid) = layout(&text_input("demo.q", &e).w(100.0), &Fake, 0.0, 0.0, 300.0);
        assert!(matches!(laid.live[..], [Live::Caret { r, .. }] if r.x == 21.0));
        // Selection: a fill under the selected chars.
        e.select_all();
        let (_, laid) = layout(&text_input("demo.q", &e).w(100.0), &Fake, 0.0, 0.0, 300.0);
        assert!(laid.ops.iter().any(|o| matches!(o, Op::Fill { r, .. } if r.x == 0.0 && r.w == 21.0)));
        // Long text: scrolled so the caret stays inside the 100 DIP field.
        e.set_text(&"x".repeat(40));
        e.focused = true;
        let (_, laid) = layout(&text_input("demo.q", &e).w(100.0), &Fake, 0.0, 0.0, 300.0);
        let Live::Caret { r, .. } = laid.live[0] else { panic!() };
        assert!(r.x <= 100.0 && r.x >= 90.0, "{r:?}");
        assert!(laid.ops.iter().any(|o| matches!(o, Op::Text { r, .. } if r.x < 0.0)));
        // Empty: the placeholder, tertiary.
        let (_, laid) = layout(&text_input("demo.q", &TextEdit::new()).placeholder("Cari").w(100.0), &Fake, 0.0, 0.0, 300.0);
        assert!(laid.ops.iter().any(|o| matches!(o, Op::Text { t, .. } if t.s == "Cari" && t.color == color::TEXT3)));
    }

    #[test]
    fn every_kind_of_live_item_comes_out_of_one_tree_in_order() {
        // Bars (media), the spinner (downloads), the charging battery (power) and the caret of a
        // focused input (launcher) in one row: the scene builds one visual per item, in this order.
        let mut e = TextEdit::new();
        e.set_text("ab");
        e.focused = true;
        let n = row([bars(true), spinner(), battery(0.5).size(54.0, 26.0).charging(), text_input("demo.q", &e).w(100.0)]);
        let (_, laid) = layout(&n, &Fake, 0.0, 0.0, 400.0);
        assert!(matches!(
            laid.live[..],
            [Live::Bars { .. }, Live::Spinner { .. }, Live::Charge { .. }, Live::Caret { .. }]
        ), "{:?}", laid.live);
    }

    fn karaoke_node(s: &str, words: Vec<WordSpan>) -> Node {
        let Kind::Text(spec) = text(s).semibold().kind else { unreachable!() };
        let anchor = Anchor { pos_ms: 0, at: std::time::Instant::now(), playing: true, rate: 1.0 };
        karaoke(KaraokeSpec { id: "track#3".into(), text: spec, dim: color::TEXT3, words, anchor, rise: 6.0 })
    }

    #[test]
    fn a_karaoke_line_measures_like_text_and_hands_its_words_to_the_scene() {
        let words = vec![WordSpan { start_ms: 100, end_ms: 400, from: 0, to: 2 }, WordSpan { start_ms: 400, end_ms: 900, from: 3, to: 6 }, WordSpan { start_ms: 900, end_ms: 1000, from: 7, to: 8 }];
        let n = karaoke_node("ab cde f", words).action("demo.seek");
        // Same size as the plain text (7 DIP per character in the fake).
        assert_eq!(measure(&n, &Fake, 500.0), measure(&text("ab cde f"), &Fake, 500.0));
        let (_, laid) = layout(&n, &Fake, 10.0, 20.0, 500.0);
        // Nothing is rasterised with the block; the scene draws both layers itself.
        assert!(laid.ops.is_empty());
        assert_eq!(laid.hits.len(), 1, "tapping the line still seeks");
        let [Live::Karaoke(k)] = &laid.live[..] else { panic!("one karaoke line: {:?}", laid.live) };
        assert_eq!((k.r.x, k.r.y, k.r.w), (10.0, 20.0, 56.0));
        assert_eq!(k.id, "track#3");
        assert_eq!(k.text_w, 56.0);
        // Words are placed by their byte ranges: x = 7 per char, the last one reaches the end.
        let x: Vec<(f32, f32)> = k.segs.iter().map(|s| (s.x0, s.x1)).collect();
        assert_eq!(x, [(0.0, 14.0), (21.0, 42.0), (49.0, 56.0)]);
        assert_eq!((k.segs[1].t0, k.segs[1].t1), (400, 900));
        assert_eq!(k.dim, color::TEXT3);
        assert_eq!(k.text.color, color::WHITE);
    }

    #[test]
    fn a_karaoke_line_is_capped_at_its_width_and_inherits_opacity() {
        let words = vec![WordSpan { start_ms: 0, end_ms: 500, from: 0, to: 5 }, WordSpan { start_ms: 500, end_ms: 1000, from: 6, to: 11 }];
        let n = col([karaoke_node("hello world", words).opacity(0.5)]).w(40.0);
        let (_, laid) = layout(&n, &Fake, 0.0, 0.0, 40.0);
        let Live::Karaoke(k) = &laid.live[0] else { panic!() };
        assert_eq!(k.text_w, 40.0, "ellipsised at the box");
        assert!(k.segs.iter().all(|s| s.x1 <= 40.0));
        assert_eq!(k.segs.last().unwrap().x1, 40.0);
        assert!((k.text.color.a - 0.5).abs() < 1e-6 && (k.dim.a - color::TEXT3.a * 0.5).abs() < 1e-6, "opacity applies to both layers");
        // No words: still a line (the scene shows it bright), nothing to sweep.
        let (_, laid) = layout(&karaoke_node("la la", vec![]), &Fake, 0.0, 0.0, 200.0);
        let Live::Karaoke(k) = &laid.live[0] else { panic!() };
        assert!(k.segs.is_empty());
    }

    #[test]
    fn two_trees_of_the_same_line_differ_only_in_what_the_player_reported() {
        let words = vec![WordSpan { start_ms: 0, end_ms: 500, from: 0, to: 2 }];
        let live = |n: &Node| -> Box<crate::ui::karaoke::KaraokeLive> {
            let (_, l) = layout(n, &Fake, 0.0, 0.0, 200.0);
            let Live::Karaoke(k) = l.live.into_iter().next().unwrap() else { panic!() };
            k
        };
        let a = live(&karaoke_node("hi", words.clone()));
        let mut spec_b = karaoke_node("hi", words.clone());
        if let Kind::Karaoke(s) = &mut spec_b.kind {
            s.anchor.pos_ms = 9_000;
        }
        let b = live(&spec_b);
        assert_ne!(a, b, "a new anchor makes the trees differ ...");
        assert!(a.same_line(&b), "... but it is the same line, so a running sweep is kept");
        let c = live(&karaoke_node("ho", words));
        assert!(!a.same_line(&c));
    }

    #[test]
    fn anim_builders() {
        assert_eq!(Anim::pop().delay(80.0).total(), 360.0);
        assert_eq!(Anim::into_key("t").ms(300.0).delay(-5.0).total(), 300.0);
        let n = icon(icons::DELETE).key("trash").wiggle(3).name("Kosongkan");
        let s = n.keyed.as_deref().unwrap();
        assert_eq!((s.pulse, s.pulse_kind), (3, Pulse::Wiggle));
        assert_eq!(n.name.as_deref(), Some("Kosongkan"));
    }

    #[test]
    fn equal_trees_compare_equal() {
        let a = row([text("x"), toggle(true)]);
        let b = row([text("x"), toggle(true)]);
        assert_eq!(a, b);
        assert_ne!(a, row([text("x"), toggle(false)]));
    }
}
