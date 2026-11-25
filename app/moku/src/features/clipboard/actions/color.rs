//! Colours in the clipboard: `#rgb`, `#rrggbb`, `rgb()` / `rgba()` and `hsl()` / `hsla()`, parsed
//! and converted between those three notations. Pure (no Windows API).

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    /// Opacity 0-1 (1 unless the text said otherwise).
    pub a: f32,
}

/// The notation a colour was written in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fmt {
    Hex,
    Rgb,
    Hsl,
}

impl Fmt {
    /// The two notations other than `self`, in the order the UI offers them.
    pub fn others(self) -> [Fmt; 2] {
        match self {
            Fmt::Hex => [Fmt::Rgb, Fmt::Hsl],
            Fmt::Rgb => [Fmt::Hex, Fmt::Hsl],
            Fmt::Hsl => [Fmt::Hex, Fmt::Rgb],
        }
    }

    /// Short label of a button: `HEX`, `RGB`, `HSL`.
    pub fn label(self) -> &'static str {
        match self {
            Fmt::Hex => "HEX",
            Fmt::Rgb => "RGB",
            Fmt::Hsl => "HSL",
        }
    }

    /// The word in an action id: `hex`, `rgb`, `hsl`.
    pub fn verb(self) -> &'static str {
        match self {
            Fmt::Hex => "hex",
            Fmt::Rgb => "rgb",
            Fmt::Hsl => "hsl",
        }
    }

    pub fn from_verb(s: &str) -> Option<Fmt> {
        match s {
            "hex" => Some(Fmt::Hex),
            "rgb" => Some(Fmt::Rgb),
            "hsl" => Some(Fmt::Hsl),
            _ => None,
        }
    }
}

impl Color {
    /// `0xRRGGBB`.
    pub fn rgb_u32(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | self.b as u32
    }

    /// The text of this colour in notation `f`.
    pub fn format(self, f: Fmt) -> String {
        match f {
            Fmt::Hex => {
                if self.a < 1.0 {
                    format!("#{:02X}{:02X}{:02X}{:02X}", self.r, self.g, self.b, (self.a * 255.0).round() as u8)
                } else {
                    format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
                }
            }
            Fmt::Rgb => {
                if self.a < 1.0 {
                    format!("rgba({}, {}, {}, {})", self.r, self.g, self.b, alpha_text(self.a))
                } else {
                    format!("rgb({}, {}, {})", self.r, self.g, self.b)
                }
            }
            Fmt::Hsl => {
                let (h, s, l) = self.hsl();
                if self.a < 1.0 { format!("hsla({h}, {s}%, {l}%, {})", alpha_text(self.a)) } else { format!("hsl({h}, {s}%, {l}%)") }
            }
        }
    }

    /// Hue (degrees), saturation and lightness (percent), rounded.
    pub fn hsl(self) -> (u32, u32, u32) {
        let (r, g, b) = (self.r as f32 / 255.0, self.g as f32 / 255.0, self.b as f32 / 255.0);
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let l = (max + min) / 2.0;
        let d = max - min;
        if d == 0.0 {
            return (0, 0, (l * 100.0).round() as u32);
        }
        let s = d / (1.0 - (2.0 * l - 1.0).abs());
        let h = if max == r {
            ((g - b) / d).rem_euclid(6.0)
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        } * 60.0;
        ((h.round() as u32) % 360, (s * 100.0).round() as u32, (l * 100.0).round() as u32)
    }
}

fn alpha_text(a: f32) -> String {
    let s = format!("{:.2}", a);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn from_hsl(h: f32, s: f32, l: f32, a: f32) -> Color {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color { r: to(r1), g: to(g1), b: to(b1), a }
}

/// One argument: a number, or a number with `%`.
fn arg(tok: &str) -> Option<(f32, bool)> {
    match tok.strip_suffix('%') {
        Some(n) => Some((n.parse::<f32>().ok().filter(|v| v.is_finite())?, true)),
        None => Some((tok.parse::<f32>().ok().filter(|v| v.is_finite())?, false)),
    }
}

fn alpha_arg(tok: &str) -> Option<f32> {
    let (v, pct) = arg(tok)?;
    let a = if pct { v / 100.0 } else { v };
    (0.0..=1.0).contains(&a).then_some(a)
}

/// `255, 128, 0` / `255 128 0 / 50%` → tokens.
fn tokens(inner: &str) -> Vec<&str> {
    inner.split(|c: char| c == ',' || c == '/' || c.is_whitespace()).filter(|t| !t.is_empty()).collect()
}

/// Parse a colour written as `#rgb`, `#rrggbb`, `rgb(…)`, `rgba(…)`, `hsl(…)` or `hsla(…)`
/// (case-insensitive, an optional trailing `;`). Out-of-range components are refused rather than
/// clamped: text like `rgb(300, 0, 0)` is more likely not a colour.
pub fn parse(s: &str) -> Option<(Color, Fmt)> {
    let t = s.trim().trim_end_matches(';').trim();
    if t.len() > 64 {
        return None;
    }
    if let Some(h) = t.strip_prefix('#') {
        if !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        let rgb = match h.len() {
            3 => ((v >> 8 & 0xf) * 17) << 16 | ((v >> 4 & 0xf) * 17) << 8 | (v & 0xf) * 17,
            6 => v,
            _ => return None,
        };
        return Some((Color { r: (rgb >> 16) as u8, g: (rgb >> 8) as u8, b: rgb as u8, a: 1.0 }, Fmt::Hex));
    }
    let lower = t.to_ascii_lowercase();
    let (name, inner) = lower.split_once('(')?;
    let inner = inner.strip_suffix(')')?;
    let toks = tokens(inner);
    if !(toks.len() == 3 || toks.len() == 4) {
        return None;
    }
    let a = match toks.get(3) {
        Some(t) => alpha_arg(t)?,
        None => 1.0,
    };
    match name.trim() {
        "rgb" | "rgba" => {
            let mut c = [0u8; 3];
            for (i, tok) in toks[..3].iter().enumerate() {
                let (v, pct) = arg(tok)?;
                let v = if pct { v * 2.55 } else { v };
                if !(0.0..=255.5).contains(&v) {
                    return None;
                }
                c[i] = v.round().min(255.0) as u8;
            }
            Some((Color { r: c[0], g: c[1], b: c[2], a }, Fmt::Rgb))
        }
        "hsl" | "hsla" => {
            let h: f32 = toks[0].strip_suffix("deg").unwrap_or(toks[0]).parse().ok().filter(|v: &f32| v.is_finite())?;
            let (s, sp) = arg(toks[1])?;
            let (l, lp) = arg(toks[2])?;
            if !sp || !lp || !(0.0..=100.0).contains(&s) || !(0.0..=100.0).contains(&l) || !(-360.0..=720.0).contains(&h) {
                return None;
            }
            Some((from_hsl(h, s / 100.0, l / 100.0, a), Fmt::Hsl))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> Color {
        parse(s).unwrap_or_else(|| panic!("{s} should parse")).0
    }

    #[test]
    fn hex_colours() {
        assert_eq!(parse("#0A84FF"), Some((Color { r: 10, g: 132, b: 255, a: 1.0 }, Fmt::Hex)));
        assert_eq!(c("#0a84ff"), c("#0A84FF"));
        assert_eq!(c("#fa0").rgb_u32(), 0xFFAA00);
        assert_eq!(c("  #fff;  ").rgb_u32(), 0xFFFFFF);
        for bad in ["#12", "#12345", "#GGGGGG", "0A84FF", "#0A84FF00", "#", "color #fff", ""] {
            assert!(parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn rgb_colours_in_every_spelling() {
        let want = Color { r: 10, g: 132, b: 255, a: 1.0 };
        for s in ["rgb(10, 132, 255)", "rgb(10 132 255)", "RGB(10,132,255)", "rgba(10, 132, 255, 1)", "rgb(10 132 255 / 100%)", "rgb( 10 , 132 , 255 );"] {
            assert_eq!(c(s), want, "{s}");
        }
        assert_eq!(c("rgba(10, 132, 255, 0.5)").a, 0.5);
        assert_eq!(c("rgb(100%, 0%, 0%)").rgb_u32(), 0xFF0000);
        assert_eq!(c("rgb(50%, 50%, 50%)").rgb_u32(), 0x808080);
        for bad in ["rgb(300, 0, 0)", "rgb(1, 2)", "rgb(1, 2, 3, 4, 5)", "rgb(a, b, c)", "rgb(1, 2, 3", "rgb(-1, 0, 0)", "rgba(1,2,3,2)", "rgbx(1, 2, 3)"] {
            assert!(parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn hsl_colours_convert_to_rgb() {
        assert_eq!(c("hsl(0, 100%, 50%)").rgb_u32(), 0xFF0000);
        assert_eq!(c("hsl(120 100% 50%)").rgb_u32(), 0x00FF00);
        assert_eq!(c("hsl(240deg, 100%, 50%)").rgb_u32(), 0x0000FF);
        assert_eq!(c("hsl(0, 0%, 100%)").rgb_u32(), 0xFFFFFF);
        assert_eq!(c("hsl(0, 0%, 0%)").rgb_u32(), 0);
        let blue = c("hsl(210, 100%, 52%)");
        assert!((blue.r as i32 - 10).abs() <= 3 && (blue.g as i32 - 133).abs() <= 3 && blue.b == 255, "{blue:?}");
        assert_eq!(c("hsla(0, 100%, 50%, 0.25)").a, 0.25);
        for bad in ["hsl(210, 100, 50)", "hsl(210, 101%, 50%)", "hsl(210, 50%)", "hsl(x, 50%, 50%)"] {
            assert!(parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn conversions() {
        let blue = c("#0A84FF");
        assert_eq!(blue.format(Fmt::Hex), "#0A84FF");
        assert_eq!(blue.format(Fmt::Rgb), "rgb(10, 132, 255)");
        assert_eq!(blue.format(Fmt::Hsl), "hsl(210, 100%, 52%)");
        assert_eq!(c("#FF0000").format(Fmt::Hsl), "hsl(0, 100%, 50%)");
        assert_eq!(c("#FFFFFF").format(Fmt::Hsl), "hsl(0, 0%, 100%)");
        assert_eq!(c("#000").format(Fmt::Rgb), "rgb(0, 0, 0)");
        assert_eq!(c("#808080").format(Fmt::Hsl), "hsl(0, 0%, 50%)");
        // Alpha survives every notation.
        let a = c("rgba(255, 0, 0, 0.5)");
        assert_eq!(a.format(Fmt::Rgb), "rgba(255, 0, 0, 0.5)");
        assert_eq!(a.format(Fmt::Hex), "#FF000080");
        assert_eq!(a.format(Fmt::Hsl), "hsla(0, 100%, 50%, 0.5)");
        // What one notation prints, the parser reads back (within rounding for HSL).
        for src in ["#0A84FF", "#30D158", "#FF453A", "#BF5AF2", "#123456"] {
            let col = c(src);
            assert_eq!(c(&col.format(Fmt::Hex)), col);
            assert_eq!(c(&col.format(Fmt::Rgb)), col);
            let back = c(&col.format(Fmt::Hsl));
            assert!((back.r as i32 - col.r as i32).abs() <= 4 && (back.g as i32 - col.g as i32).abs() <= 4 && (back.b as i32 - col.b as i32).abs() <= 4, "{src}: {back:?}");
        }
    }

    #[test]
    fn the_other_formats_and_verbs() {
        assert_eq!(Fmt::Hex.others(), [Fmt::Rgb, Fmt::Hsl]);
        assert_eq!(Fmt::Rgb.others(), [Fmt::Hex, Fmt::Hsl]);
        assert_eq!(Fmt::Hsl.others(), [Fmt::Hex, Fmt::Rgb]);
        for f in [Fmt::Hex, Fmt::Rgb, Fmt::Hsl] {
            assert_eq!(Fmt::from_verb(f.verb()), Some(f));
        }
        assert_eq!(Fmt::from_verb("cmyk"), None);
        assert_eq!(parse("rgb(1, 2, 3)").unwrap().1, Fmt::Rgb);
        assert_eq!(parse("hsl(1, 2%, 3%)").unwrap().1, Fmt::Hsl);
    }
}
