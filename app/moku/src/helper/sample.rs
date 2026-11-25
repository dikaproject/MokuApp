//! A fixed list of invented apps with drawn icons, for screenshots (`MOKU_DEBUG=1` and
//! `MOKU_LAUNCHER_SAMPLE=1`): the real Start Menu is never needed, so a screenshot cannot show
//! which apps the developer has installed.

use super::pack::{ICON_PX, Icon, Kind, Rec};

/// (name, detail, colour top, colour bottom)
const APPS: &[(&str, &str, u32, u32)] = &[
    ("Aurora Notes", "aurora.exe", 0xFF9F0A, 0xFF6A00),
    ("Bintang Player", "bintang.exe", 0xBF5AF2, 0x7B2FD6),
    ("Citra Editor", "citra.exe", 0x30D158, 0x12994A),
    ("Dawai Terminal", "dawai.exe", 0x48484A, 0x1C1C1E),
    ("Embun Kalender", "embun.exe", 0xFF453A, 0xC9302C),
    ("Fajar Browser", "fajar.exe", 0x64D2FF, 0x0A84FF),
    ("Gema Musik", "gema.exe", 0xFF375F, 0xD1224A),
    ("Harmoni Mixer", "harmoni.exe", 0x63E6BE, 0x1FA68A),
    ("Intan Foto", "intan.exe", 0xFFD60A, 0xF2A100),
    ("Jala Chat", "jala.exe", 0x5E5CE6, 0x3634A3),
    ("Kopi Timer", "kopi.exe", 0xAC8E68, 0x7A6141),
    ("Lentera Reader", "lentera.exe", 0xFFD60A, 0xFF9F0A),
    ("Mentari Cuaca", "mentari.exe", 0x64D2FF, 0x0A84FF),
    ("Nusa Peta", "nusa.exe", 0x30D158, 0x0A84FF),
    ("Ombak Catatan", "ombak.exe", 0x0A84FF, 0x5E5CE6),
];

fn hex(c: u32) -> [f32; 3] {
    [(c >> 16 & 0xff) as f32, (c >> 8 & 0xff) as f32, (c & 0xff) as f32]
}

/// Coverage (0-1) of a rounded square of half-size `half` and corner radius `r`, centred at 0.
fn rounded_square(x: f32, y: f32, half: f32, r: f32) -> f32 {
    let (qx, qy) = (x.abs() - (half - r), y.abs() - (half - r));
    let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
    (0.5 - d).clamp(0.0, 1.0)
}

/// Distance-based coverage of the glyph in the middle (0 = none): a disc, a ring, a play
/// triangle or two bars, picked by `shape`.
fn glyph(shape: usize, x: f32, y: f32) -> f32 {
    let d = match shape % 4 {
        0 => (x * x + y * y).sqrt() - 9.0,
        1 => ((x * x + y * y).sqrt() - 9.0).abs() - 2.5,
        2 => {
            // Triangle pointing right: inside when x in -7..9 and |y| below the slope.
            let slope = (9.0 - x) * 0.62;
            (y.abs() - slope).max(-7.0 - x)
        }
        _ => {
            let bar = |cx: f32| (x - cx).abs().max(y.abs() * 0.5) - 3.2;
            bar(-5.5).min(bar(5.5)).max((y.abs() - 9.0).min(1.0))
        }
    };
    (0.5 - d).clamp(0.0, 1.0)
}

/// A 48 px premultiplied BGRA tile: a gradient rounded square with a white glyph.
fn tile(i: usize, top: u32, bottom: u32) -> Icon {
    let px = ICON_PX as usize;
    let (a, b) = (hex(top), hex(bottom));
    let mut bgra = vec![0u8; px * px * 4];
    for yy in 0..px {
        for xx in 0..px {
            let (x, y) = (xx as f32 + 0.5 - px as f32 / 2.0, yy as f32 + 0.5 - px as f32 / 2.0);
            let cover = rounded_square(x, y, 22.0, 10.0);
            let t = (yy as f32 / px as f32).clamp(0.0, 1.0);
            let g = glyph(i, x, y) * 0.92;
            let o = (yy * px + xx) * 4;
            for c in 0..3 {
                let base = a[2 - c] * (1.0 - t) + b[2 - c] * t;
                let v = base * (1.0 - g) + 255.0 * g;
                bgra[o + c] = (v * cover).round() as u8;
            }
            bgra[o + 3] = (cover * 255.0).round() as u8;
        }
    }
    Icon { w: px as u16, h: px as u16, bgra }
}

pub fn recs() -> Vec<Rec<Icon>> {
    APPS.iter()
        .enumerate()
        .map(|(i, (name, detail, top, bottom))| Rec {
            kind: Kind::Link,
            name: name.to_string(),
            detail: detail.to_string(),
            target: format!(r"C:\Sample\Start Menu\{name}.lnk"),
            icon: Some(tile(i, *top, *bottom)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sample_list_is_drawn_and_encodes() {
        let recs = recs();
        assert_eq!(recs.len(), APPS.len());
        for r in &recs {
            let icon = r.icon.as_ref().unwrap();
            // Opaque in the middle, transparent in the corner, premultiplied (channels <= alpha).
            let px = |x: usize, y: usize| &icon.bgra[(y * 48 + x) * 4..][..4];
            assert_eq!(px(24, 6)[3], 255, "{}", r.name);
            assert_eq!(px(0, 0)[3], 0, "{}", r.name);
            assert!(icon.bgra.chunks_exact(4).all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]));
        }
        assert!(super::super::pack::Index::decode(super::super::pack::encode_index(&recs)).is_some());
    }
}
