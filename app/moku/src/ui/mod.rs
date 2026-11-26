//! The UI kit: a small declarative widget tree for pills and panels.
//!
//! Features build a [`Node`] tree from their state (`fn(&App) -> Node`); the core lays it out
//! ([`layout`]), turns it into draw ops, and rasterises it into composition surfaces with
//! Direct2D — but only when the tree differs from the previous one (trees are `PartialEq`).
//! Widgets with an action id become hit regions; clicks / scroll / slider drags on them are
//! delivered to the feature that owns the id prefix (`"media.play"` → feature `media`).
//!
//! All sizes are DIP. Colours/sizes follow the style foundations (`pratinjau/00-Ringkasan.png`):
//! see [`color`], [`typo`] and [`widgets`].

pub mod icons;
pub mod karaoke;
pub mod layout;
pub mod qr;
pub mod text_edit;
pub mod widgets;

use std::fmt;
use std::sync::Arc;

pub use karaoke::{Anchor, KaraokeSpec, WordSpan, karaoke};
pub use layout::Rect;
pub use qr::qr;
pub use text_edit::TextEdit;
pub use widgets::*;

/// Straight-alpha colour, components 0–1.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    /// `Rgba::hex(0x0A84FF, 1.0)`.
    pub const fn hex(rgb: u32, a: f32) -> Rgba {
        Rgba { r: ((rgb >> 16) & 0xff) as f32 / 255.0, g: ((rgb >> 8) & 0xff) as f32 / 255.0, b: (rgb & 0xff) as f32 / 255.0, a }
    }
    pub const fn alpha(self, a: f32) -> Rgba {
        Rgba { a, ..self }
    }
    /// Multiply the alpha (used for `Node::opacity`).
    pub fn fade(self, k: f32) -> Rgba {
        Rgba { a: self.a * k, ..self }
    }
    pub fn from_rgb(c: moku_core::color::Rgb) -> Rgba {
        Rgba::hex(c.0, 1.0)
    }
}

/// Palette from the style foundations. Text is white 100 / 72 / 46 %; colour only for status
/// and for what is active.
pub mod color {
    use super::Rgba;
    pub const WHITE: Rgba = Rgba::hex(0xffffff, 1.0);
    /// Secondary text (72 %).
    pub const TEXT2: Rgba = Rgba::hex(0xffffff, 0.72);
    /// Tertiary text (46 %).
    pub const TEXT3: Rgba = Rgba::hex(0xffffff, 0.46);
    /// Control / card fills on the panel glass (white 8 %, 14 %, 16 %).
    pub const FILL: Rgba = Rgba::hex(0xffffff, 0.08);
    pub const FILL2: Rgba = Rgba::hex(0xffffff, 0.14);
    pub const FILL3: Rgba = Rgba::hex(0xffffff, 0.16);
    /// Dark ink on white buttons.
    pub const INK: Rgba = Rgba::hex(0x0A0A12, 1.0);
    pub const BLUE: Rgba = Rgba::hex(0x0A84FF, 1.0);
    pub const GREEN: Rgba = Rgba::hex(0x30D158, 1.0);
    pub const ORANGE: Rgba = Rgba::hex(0xFF9F0A, 1.0);
    pub const RED: Rgba = Rgba::hex(0xFF453A, 1.0);
    pub const YELLOW: Rgba = Rgba::hex(0xFFD60A, 1.0);
    pub const PURPLE: Rgba = Rgba::hex(0xBF5AF2, 1.0);
    pub const TEAL: Rgba = Rgba::hex(0x64D2FF, 1.0);
    pub const MINT: Rgba = Rgba::hex(0x63E6BE, 1.0);
    /// Music bars / music accent from the mockup.
    pub const PINK: Rgba = Rgba::hex(0xFF375F, 1.0);
    pub const TRANSPARENT: Rgba = Rgba::hex(0, 0.0);
}

/// Type scale from the style foundations (DIP).
pub mod typo {
    /// "Judul panel 20 Bold".
    pub const PANEL_TITLE: f32 = 20.0;
    /// "Judul kartu 16 SemiBold".
    pub const CARD_TITLE: f32 = 16.0;
    /// "Isi 13 Regular" — main text in island and panel.
    pub const BODY: f32 = 13.0;
    /// "Keterangan 11.5 Regular" — secondary info.
    pub const CAPTION: f32 = 11.5;
    /// "Label 10.5 SemiBold KAPITAL".
    pub const LABEL: f32 = 10.5;
    /// Radii: panel 38, card 22, buttons are capsules. Min click target 28.
    pub const RADIUS_PANEL: f32 = 38.0;
    pub const RADIUS_CARD: f32 = 22.0;
    pub const MIN_TARGET: f32 = 28.0;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Font {
    /// Segoe UI Variable Text (→ Segoe UI on Windows 10).
    Text,
    /// Segoe UI Variable Display, for big numbers and titles.
    Display,
    /// Segoe Fluent Icons (→ Segoe MDL2 Assets). See [`icons`].
    Icons,
    /// Cascadia Mono (→ Consolas), for commands.
    Mono,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Weight {
    Regular,
    Medium,
    Semibold,
    Bold,
}

/// Size along one axis.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Len {
    /// Measured from content.
    Auto,
    /// Fixed DIP.
    Px(f32),
    /// Take all the space the parent offers on this axis.
    Fill,
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Insets {
    pub l: f32,
    pub t: f32,
    pub r: f32,
    pub b: f32,
}

/// Cross-axis alignment of children in a row / column (and both axes in a stack).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Align {
    Start,
    Center,
    End,
    Stretch,
}

/// Main-axis distribution of children when nothing grows.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Justify {
    Start,
    Center,
    End,
    SpaceBetween,
}

#[derive(Clone, PartialEq, Debug)]
pub struct TextSpec {
    pub s: String,
    pub font: Font,
    pub size: f32,
    pub weight: Weight,
    pub color: Rgba,
    /// 1 = single line with ellipsis; n > 1 = wrap to at most n lines (last one ellipsised).
    pub max_lines: u32,
    /// Horizontal alignment inside its box: 0 left, 0.5 centre, 1 right.
    pub align: f32,
}

/// A BGRA bitmap, **premultiplied** alpha, rows tightly packed (`width * 4` bytes).
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// Shared image handle. Two `Image`s are equal only if they are the *same* allocation, so
/// keep the `Image` in your state and reuse it; creating a new one forces a redraw.
#[derive(Clone)]
pub struct Image(pub Arc<Bitmap>);

impl Image {
    pub fn new(width: u32, height: u32, bgra: Vec<u8>) -> Image {
        debug_assert_eq!(bgra.len(), (width * height * 4) as usize);
        Image(Arc::new(Bitmap { width, height, bgra }))
    }
    /// Average colour (for album-art accents), ignoring transparent pixels.
    pub fn average(&self) -> Rgba {
        let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
        for px in self.0.bgra.chunks_exact(4).step_by(7) {
            if px[3] > 16 {
                b += px[0] as u64;
                g += px[1] as u64;
                r += px[2] as u64;
                n += 1;
            }
        }
        if n == 0 {
            return color::BLUE;
        }
        Rgba { r: (r / n) as f32 / 255.0, g: (g / n) as f32 / 255.0, b: (b / n) as f32 / 255.0, a: 1.0 }
    }
}

impl PartialEq for Image {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl fmt::Debug for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Image({}x{})", self.0.width, self.0.height)
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Kind {
    Row(Vec<Node>),
    Col(Vec<Node>),
    /// Children on top of each other, each aligned inside the stack by `align`.
    Stack(Vec<Node>),
    Text(TextSpec),
    /// One glyph from [`icons`].
    Icon { glyph: String, size: f32, color: Rgba },
    Image { img: Image, radius: f32 },
    /// On/off switch (40×24); green when on.
    Toggle { on: bool, color: Rgba },
    /// Thick slider (Control Center style), value 0–1. Delivers `ActionKind::Slide`.
    Slider { value: f32, icon: Option<String>, color: Rgba },
    /// Thin progress bar, value 0–1.
    Progress { value: f32, color: Rgba, track: Rgba, thickness: f32 },
    /// Circular progress ring, value 0–1 (clockwise from 12 o'clock).
    Ring { value: f32, color: Rgba, track: Rgba, thickness: f32 },
    /// Line chart of values (auto-scaled to min..max unless `range` given).
    Sparkline { values: Vec<f32>, color: Rgba, range: Option<(f32, f32)> },
    /// Five audio bars animated by the compositor while `playing` (no CPU per frame).
    Bars { playing: bool, color: Rgba },
    /// Indeterminate ring: a short arc turning on a faint track, animated by the compositor
    /// (no CPU per frame) unless animations are off. For progress with no known percentage.
    Spinner { color: Rgba, track: Rgba, thickness: f32 },
    /// Battery gauge: outline, terminal and a level bar of `value` (0-1). With `charging` the
    /// level bar is drawn by the compositor and plays the charging animation (see
    /// [`Node::charging`]).
    Battery { value: f32, color: Rgba, charging: bool },
    /// The Moku logo mark.
    Logo,
    /// One-line text field ([`text_input`]): text, selection, IME composition, and a caret that
    /// the compositor blinks while the input has keyboard focus.
    Input(InputSpec),
    /// One line whose words light up in time with the music (see [`karaoke`]): a dim base and a
    /// bright copy the compositor reveals from left to right.
    Karaoke(KaraokeSpec),
    /// Flexible empty space (grows along the parent's main axis).
    Spacer,
    Empty,
}

/// What a [`text_input`] draws (a snapshot of its [`TextEdit`]).
#[derive(Clone, PartialEq, Debug)]
pub struct InputSpec {
    /// Text with the IME composition spliced in.
    pub text: String,
    /// Caret byte index into `text`.
    pub caret: usize,
    pub selection: Option<(usize, usize)>,
    /// IME composition byte range (underlined).
    pub composition: Option<(usize, usize)>,
    /// Draw the caret (the input has keyboard focus).
    pub focused: bool,
    /// Shown (tertiary) while the text is empty.
    pub placeholder: String,
    pub size: f32,
    pub color: Rgba,
    /// Selection highlight.
    pub accent: Rgba,
}

/// A compositor animation for a keyed element appearing or leaving (see [`Node::key`]).
/// Durations and delays are ms. The scene plays nothing when Windows "Animation effects" are
/// off or `appearance.animations = "off"`, and only fades when it is `"reduced"`.
#[derive(Clone, PartialEq, Debug)]
pub enum Anim {
    /// Opacity only.
    Fade { ms: f32, delay: f32 },
    /// Opacity 0→1, scale `from_scale`→1 (slight overshoot) and a slide of `dy` DIP up (enter);
    /// reversed for an exit.
    Pop { ms: f32, delay: f32, from_scale: f32, dy: f32 },
    /// Exit only: fly on a slight curve into the keyed element `target` (its rect in the new
    /// tree), shrinking to 0.2 and fading out. Without that target it falls back to a fade.
    Into { target: String, ms: f32, delay: f32 },
}

impl Anim {
    pub fn fade() -> Anim {
        Anim::Fade { ms: 180.0, delay: 0.0 }
    }
    /// The "new tile" entrance: 280 ms, scale 0.6→1, 14 DIP up.
    pub fn pop() -> Anim {
        Anim::Pop { ms: 280.0, delay: 0.0, from_scale: 0.6, dy: 14.0 }
    }
    /// Suck into the keyed element `target` (e.g. a trash button), 340 ms.
    pub fn into_key(target: impl Into<String>) -> Anim {
        Anim::Into { target: target.into(), ms: 340.0, delay: 0.0 }
    }
    pub fn delay(mut self, d: f32) -> Anim {
        match &mut self {
            Anim::Fade { delay, .. } | Anim::Pop { delay, .. } | Anim::Into { delay, .. } => *delay = d.max(0.0),
        }
        self
    }
    pub fn ms(mut self, m: f32) -> Anim {
        match &mut self {
            Anim::Fade { ms, .. } | Anim::Pop { ms, .. } | Anim::Into { ms, .. } => *ms = m.max(1.0),
        }
        self
    }
    /// Delay + duration (ms).
    pub fn total(&self) -> f32 {
        match self {
            Anim::Fade { ms, delay } | Anim::Pop { ms, delay, .. } | Anim::Into { ms, delay, .. } => ms + delay,
        }
    }
}

/// A one-shot emphasis played when [`KeySpec::pulse`] changes between two trees.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Pulse {
    /// Scale 1 → 1.12 → 0.97 → 1 (≈ 360 ms): "this one was already here".
    #[default]
    Pop,
    /// Rotate ±14° a few times with a small scale-up (≈ 420 ms): a trash can that just ate.
    Wiggle,
}

/// Identity and motion of a keyed element (see [`Node::key`]).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct KeySpec {
    /// Unique among the keyed elements of one pill / panel body.
    pub key: String,
    /// Played when the key was not in the previous tree (default: a short fade).
    pub enter: Option<Anim>,
    /// Played when the key is gone from the new tree (default: a short fade).
    pub exit: Option<Anim>,
    /// Bump to play `pulse_kind` once.
    pub pulse: u32,
    pub pulse_kind: Pulse,
    /// Play `exit` now while the node keeps its place in the layout (nothing is drawn there).
    pub leaving: bool,
}

/// One widget. Build with the free functions ([`row`], [`text`], ...) and chain modifiers.
#[derive(Clone, PartialEq, Debug)]
pub struct Node {
    pub kind: Kind,
    pub w: Len,
    pub h: Len,
    /// Upper bound for an auto width (text inside ellipsises / wraps at it).
    pub max_w: Option<f32>,
    /// Share of the leftover main-axis space in a row / column (0 = none).
    pub grow: f32,
    pub pad: Insets,
    pub gap: f32,
    pub align: Align,
    pub justify: Justify,
    pub bg: Option<Rgba>,
    /// Corner radius; `f32::INFINITY` = capsule / circle.
    pub radius: f32,
    pub border: Option<(Rgba, f32)>,
    pub dashed: bool,
    pub opacity: f32,
    /// Action id delivered on click / scroll / slide, e.g. `"media.play"`, `"clipboard.copy:17"`.
    pub action: Option<String>,
    /// Press-and-move on this node delivers `ActionKind::DragStart` instead of a click.
    pub draggable: bool,
    /// Accessible name of an icon-only control (carried into its hit region).
    pub name: Option<String>,
    /// Stable identity: rasterised as its own sprite, animated on enter / exit / move.
    pub keyed: Option<Box<KeySpec>>,
}

impl Node {
    /// Does any node *below* this one carry an action (a button, a link, a slider)? The root's own
    /// action does not count: a pill with only a root action (the music pill's scroll and
    /// double-click) has nothing to press.
    pub fn has_inner_action(&self) -> bool {
        fn kids(n: &Node) -> &[Node] {
            match &n.kind {
                Kind::Row(v) | Kind::Col(v) | Kind::Stack(v) => v,
                _ => &[],
            }
        }
        fn any(n: &Node) -> bool {
            n.action.is_some() || kids(n).iter().any(any)
        }
        kids(self).iter().any(any)
    }

    pub fn new(kind: Kind) -> Node {
        Node {
            kind,
            w: Len::Auto,
            h: Len::Auto,
            max_w: None,
            grow: 0.0,
            pad: Insets::default(),
            gap: 0.0,
            align: Align::Center,
            justify: Justify::Start,
            bg: None,
            radius: 0.0,
            border: None,
            dashed: false,
            opacity: 1.0,
            action: None,
            draggable: false,
            name: None,
            keyed: None,
        }
    }

    // ---- size & placement ---------------------------------------------------------------
    pub fn w(mut self, w: f32) -> Node {
        self.w = Len::Px(w);
        self
    }
    pub fn h(mut self, h: f32) -> Node {
        self.h = Len::Px(h);
        self
    }
    pub fn size(self, w: f32, h: f32) -> Node {
        self.w(w).h(h)
    }
    /// Cap the auto width (e.g. a pill's text column): content wider than this ellipsises.
    pub fn max_w(mut self, w: f32) -> Node {
        self.max_w = Some(w);
        self
    }
    pub fn fill_w(mut self) -> Node {
        self.w = Len::Fill;
        self
    }
    pub fn fill_h(mut self) -> Node {
        self.h = Len::Fill;
        self
    }
    /// Take a share of the parent's leftover main-axis space.
    pub fn grow(mut self, weight: f32) -> Node {
        self.grow = weight;
        self
    }
    pub fn pad(mut self, all: f32) -> Node {
        self.pad = Insets { l: all, t: all, r: all, b: all };
        self
    }
    pub fn pad_xy(mut self, x: f32, y: f32) -> Node {
        self.pad = Insets { l: x, t: y, r: x, b: y };
        self
    }
    pub fn pad4(mut self, l: f32, t: f32, r: f32, b: f32) -> Node {
        self.pad = Insets { l, t, r, b };
        self
    }
    pub fn gap(mut self, g: f32) -> Node {
        self.gap = g;
        self
    }
    pub fn align(mut self, a: Align) -> Node {
        self.align = a;
        self
    }
    pub fn justify(mut self, j: Justify) -> Node {
        self.justify = j;
        self
    }
    /// Centre children on both axes.
    pub fn center(self) -> Node {
        self.align(Align::Center).justify(Justify::Center)
    }

    // ---- decoration ---------------------------------------------------------------------
    pub fn bg(mut self, c: Rgba) -> Node {
        self.bg = Some(c);
        self
    }
    pub fn radius(mut self, r: f32) -> Node {
        self.radius = r;
        self
    }
    /// Fully rounded ends (capsule / circle).
    pub fn capsule(self) -> Node {
        self.radius(f32::INFINITY)
    }
    pub fn border(mut self, c: Rgba) -> Node {
        self.border = Some((c, 1.0));
        self
    }
    pub fn dashed(mut self) -> Node {
        self.dashed = true;
        self
    }
    pub fn opacity(mut self, a: f32) -> Node {
        self.opacity = a;
        self
    }

    // ---- interaction --------------------------------------------------------------------
    pub fn action(mut self, id: impl Into<String>) -> Node {
        self.action = Some(id.into());
        self
    }
    pub fn draggable(mut self) -> Node {
        self.draggable = true;
        self
    }
    /// Accessible name for an icon-only control ("Kosongkan Shelf").
    pub fn name(mut self, label: impl Into<String>) -> Node {
        self.name = Some(label.into());
        self
    }

    // ---- keyed motion (compositor animations, no CPU per frame) ---------------------------
    fn spec(&mut self) -> &mut KeySpec {
        self.keyed.get_or_insert_with(Default::default)
    }
    /// Give the node a stable identity. A keyed node becomes its own sprite: when its key is new
    /// it plays `enter`, when it is gone it plays `exit` (the sprite is removed afterwards), and
    /// when it is at another position than in the previous tree it glides there.
    pub fn key(mut self, key: impl Into<String>) -> Node {
        self.spec().key = key.into();
        self
    }
    pub fn enter(mut self, a: Anim) -> Node {
        self.spec().enter = Some(a);
        self
    }
    pub fn exit(mut self, a: Anim) -> Node {
        self.spec().exit = Some(a);
        self
    }
    /// Play a [`Pulse::Pop`] each time `generation` changes (keep a counter in your state).
    pub fn pulse(mut self, generation: u32) -> Node {
        let s = self.spec();
        s.pulse = generation;
        s.pulse_kind = Pulse::Pop;
        self
    }
    /// Leave now with this tree's `exit` animation, but keep the node's space in the layout (so
    /// nothing else moves while it flies off). Drop the node from the tree once the exit is over.
    pub fn leaving(mut self) -> Node {
        self.spec().leaving = true;
        self
    }
    /// Play a [`Pulse::Wiggle`] each time `generation` changes.
    pub fn wiggle(mut self, generation: u32) -> Node {
        let s = self.spec();
        s.pulse = generation;
        s.pulse_kind = Pulse::Wiggle;
        self
    }

    // ---- text / glyph / graphics styling (ignored by other kinds) ------------------------
    fn text_mut(&mut self) -> Option<&mut TextSpec> {
        match &mut self.kind {
            Kind::Text(t) => Some(t),
            _ => None,
        }
    }
    /// Text or icon size.
    pub fn font_size(mut self, s: f32) -> Node {
        match &mut self.kind {
            Kind::Text(t) => t.size = s,
            Kind::Icon { size, .. } => *size = s,
            Kind::Input(i) => i.size = s,
            _ => {}
        }
        self
    }
    /// Text shown in an empty [`text_input`].
    pub fn placeholder(mut self, s: impl Into<String>) -> Node {
        if let Kind::Input(i) = &mut self.kind {
            i.placeholder = s.into();
        }
        self
    }
    /// Selection colour of a [`text_input`] (drawn at 45 %).
    pub fn accent(mut self, c: Rgba) -> Node {
        if let Kind::Input(i) = &mut self.kind {
            i.accent = c;
        }
        self
    }
    pub fn weight(mut self, w: Weight) -> Node {
        if let Some(t) = self.text_mut() {
            t.weight = w;
        }
        self
    }
    pub fn bold(self) -> Node {
        self.weight(Weight::Bold)
    }
    pub fn semibold(self) -> Node {
        self.weight(Weight::Semibold)
    }
    pub fn medium(self) -> Node {
        self.weight(Weight::Medium)
    }
    /// Foreground colour of text, icon, progress, ring, sparkline, bars, slider, toggle.
    pub fn color(mut self, c: Rgba) -> Node {
        match &mut self.kind {
            Kind::Text(t) => t.color = c,
            Kind::Icon { color, .. }
            | Kind::Progress { color, .. }
            | Kind::Ring { color, .. }
            | Kind::Sparkline { color, .. }
            | Kind::Bars { color, .. }
            | Kind::Spinner { color, .. }
            | Kind::Battery { color, .. }
            | Kind::Slider { color, .. }
            | Kind::Toggle { color, .. } => *color = c,
            Kind::Input(i) => i.color = c,
            _ => {}
        }
        self
    }
    /// Secondary text colour (white 72 %).
    pub fn secondary(self) -> Node {
        self.color(color::TEXT2)
    }
    /// Tertiary text colour (white 46 %).
    pub fn tertiary(self) -> Node {
        self.color(color::TEXT3)
    }
    pub fn display(mut self) -> Node {
        if let Some(t) = self.text_mut() {
            t.font = Font::Display;
        }
        self
    }
    pub fn mono(mut self) -> Node {
        if let Some(t) = self.text_mut() {
            t.font = Font::Mono;
        }
        self
    }
    /// Wrap to at most `n` lines (needs a width limit from the parent or `.w()`).
    pub fn lines(mut self, n: u32) -> Node {
        if let Some(t) = self.text_mut() {
            t.max_lines = n.max(1);
        }
        self
    }
    pub fn text_center(mut self) -> Node {
        if let Some(t) = self.text_mut() {
            t.align = 0.5;
        }
        self
    }
    pub fn text_right(mut self) -> Node {
        if let Some(t) = self.text_mut() {
            t.align = 1.0;
        }
        self
    }
    /// Track colour of progress / ring.
    pub fn track(mut self, c: Rgba) -> Node {
        if let Kind::Progress { track, .. } | Kind::Ring { track, .. } | Kind::Spinner { track, .. } = &mut self.kind {
            *track = c;
        }
        self
    }
    /// Stroke thickness of progress / ring.
    pub fn thickness(mut self, t: f32) -> Node {
        if let Kind::Progress { thickness, .. } | Kind::Ring { thickness, .. } | Kind::Spinner { thickness, .. } = &mut self.kind {
            *thickness = t;
        }
        self
    }
    /// Slider icon (glyph from [`icons`]).
    pub fn slider_icon(mut self, g: &str) -> Node {
        if let Kind::Slider { icon, .. } = &mut self.kind {
            *icon = Some(g.into());
        }
        self
    }
    /// Battery gauge only: play the charging animation. The level bar grows from empty to its
    /// value with a glint across it and a soft glow behind the gauge; all of it runs in the
    /// compositor and finishes by itself (nothing loops forever). `appearance.animations =
    /// "reduced"` fades the level bar in, `"off"` shows it at once.
    pub fn charging(mut self) -> Node {
        if let Kind::Battery { charging, .. } = &mut self.kind {
            *charging = true;
        }
        self
    }
    /// Fixed value range for a sparkline.
    pub fn range(mut self, lo: f32, hi: f32) -> Node {
        if let Kind::Sparkline { range, .. } = &mut self.kind {
            *range = Some((lo, hi));
        }
        self
    }

    /// Add a child to a row / column / stack.
    pub fn child(mut self, n: Node) -> Node {
        if let Kind::Row(c) | Kind::Col(c) | Kind::Stack(c) = &mut self.kind {
            c.push(n);
        }
        self
    }
}

// ---- constructors ---------------------------------------------------------------------------

/// Horizontal container (children centred vertically by default).
pub fn row(children: impl IntoIterator<Item = Node>) -> Node {
    Node::new(Kind::Row(children.into_iter().collect()))
}
/// Vertical container (children left-aligned by default).
pub fn col(children: impl IntoIterator<Item = Node>) -> Node {
    Node::new(Kind::Col(children.into_iter().collect())).align(Align::Start)
}
/// Overlay container.
pub fn stack(children: impl IntoIterator<Item = Node>) -> Node {
    Node::new(Kind::Stack(children.into_iter().collect()))
}
/// Body text: 13 Regular, white, single line with ellipsis.
pub fn text(s: impl Into<String>) -> Node {
    Node::new(Kind::Text(TextSpec { s: s.into(), font: Font::Text, size: typo::BODY, weight: Weight::Regular, color: color::WHITE, max_lines: 1, align: 0.0 }))
}
/// Caption: 11.5 Regular, tertiary.
pub fn caption(s: impl Into<String>) -> Node {
    text(s).font_size(typo::CAPTION).tertiary()
}
/// Section label: 10.5 SemiBold capitals, tertiary.
pub fn label(s: impl AsRef<str>) -> Node {
    text(s.as_ref().to_uppercase()).font_size(typo::LABEL).semibold().tertiary()
}
/// Panel title: 20 Bold Display.
pub fn title(s: impl Into<String>) -> Node {
    text(s).font_size(typo::PANEL_TITLE).bold().display()
}
/// Icon glyph (see [`icons`]), 14 DIP white.
pub fn icon(glyph: &str) -> Node {
    Node::new(Kind::Icon { glyph: glyph.into(), size: 14.0, color: color::WHITE })
}
/// Bitmap, scaled to the node size (set `.size(w, h)`), optional rounded corners.
pub fn image(img: &Image, radius: f32) -> Node {
    Node::new(Kind::Image { img: img.clone(), radius })
}
pub fn toggle(on: bool) -> Node {
    Node::new(Kind::Toggle { on, color: color::GREEN })
}
pub fn slider(value: f32) -> Node {
    Node::new(Kind::Slider { value: value.clamp(0.0, 1.0), icon: None, color: Rgba::hex(0xffffff, 0.92) }).fill_w().h(36.0)
}
pub fn progress(value: f32) -> Node {
    Node::new(Kind::Progress { value: value.clamp(0.0, 1.0), color: color::WHITE, track: Rgba::hex(0xffffff, 0.25), thickness: 3.0 }).fill_w()
}
pub fn ring(value: f32) -> Node {
    Node::new(Kind::Ring { value: value.clamp(0.0, 1.0), color: color::ORANGE, track: Rgba::hex(0xffffff, 0.16), thickness: 3.0 }).size(20.0, 20.0)
}
/// Indeterminate progress ring (20 px, a turning arc): downloads and other work of unknown length.
pub fn spinner() -> Node {
    Node::new(Kind::Spinner { color: color::BLUE, track: Rgba::hex(0xffffff, 0.16), thickness: 2.5 }).size(20.0, 20.0)
}
pub fn sparkline(values: Vec<f32>) -> Node {
    Node::new(Kind::Sparkline { values, color: color::GREEN, range: None }).fill_w().h(24.0)
}
pub fn bars(playing: bool) -> Node {
    Node::new(Kind::Bars { playing, color: color::PINK }).size(25.0, 20.0)
}
/// Battery gauge (26 x 13 unless sized), `value` 0-1, green. See [`Node::charging`].
pub fn battery(value: f32) -> Node {
    Node::new(Kind::Battery { value: if value.is_nan() { 0.0 } else { value.clamp(0.0, 1.0) }, color: color::GREEN, charging: false })
}
pub fn logo(size: f32) -> Node {
    Node::new(Kind::Logo).size(size, size)
}
/// One-line text field showing `edit` (the core's [`TextEdit`] for input `id`: `app.input(id)`).
/// Fills the width; 13 DIP text (`.font_size`), `.placeholder(..)`, `.accent(..)` (selection).
/// Its action id is `id`: give it keyboard focus with `app.focus_input(id)`; edits and keys then
/// arrive as `ActionKind::Edited` / `ActionKind::Key` / `ActionKind::Blur` on `id`.
/// The caret is a compositor sprite that blinks by itself (no CPU per frame).
pub fn text_input(id: &str, edit: &TextEdit) -> Node {
    let (text, caret, selection, composition) = edit.display();
    let spec = InputSpec { text, caret, selection, composition, focused: edit.focused, placeholder: String::new(), size: typo::BODY, color: color::WHITE, accent: color::BLUE };
    Node::new(Kind::Input(spec)).fill_w().action(id)
}
/// Flexible space along the parent's main axis.
pub fn spacer() -> Node {
    Node::new(Kind::Spacer).grow(1.0)
}
/// Fixed gap of `px` DIP on both axes.
pub fn gap_px(px: f32) -> Node {
    Node::new(Kind::Empty).size(px, px)
}
pub fn empty() -> Node {
    Node::new(Kind::Empty).size(0.0, 0.0)
}
