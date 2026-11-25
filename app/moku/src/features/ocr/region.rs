//! Pure geometry and pixel helpers for the OCR capture: turning a drag into a rectangle, cropping
//! the frozen screenshot, preparing a small region for the recogniser, counting words. No Windows
//! API, so it is all unit-tested.

/// A rectangle in pixels: top-left corner and size.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Smallest side of a selection worth recognising; a plain click (or a twitch) is ignored.
pub const MIN_SIDE: i32 = 8;

/// The rectangle between two drag points (any direction), clamped to `0..bounds_w` x `0..bounds_h`.
/// `None` when what is left is smaller than [`MIN_SIDE`] on either side.
pub fn from_drag(a: (i32, i32), b: (i32, i32), bounds_w: i32, bounds_h: i32) -> Option<Rect> {
    let (x0, x1) = (a.0.min(b.0).clamp(0, bounds_w), a.0.max(b.0).clamp(0, bounds_w));
    let (y0, y1) = (a.1.min(b.1).clamp(0, bounds_h), a.1.max(b.1).clamp(0, bounds_h));
    let r = Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
    (r.w >= MIN_SIDE && r.h >= MIN_SIDE).then_some(r)
}

/// The pixels of `r` from a top-down BGRA image `sw` x `sh`. `None` when `r` is not inside it.
pub fn crop_bgra(src: &[u8], sw: usize, sh: usize, r: Rect) -> Option<Vec<u8>> {
    if r.w <= 0 || r.h <= 0 || r.x < 0 || r.y < 0 || (r.x + r.w) as usize > sw || (r.y + r.h) as usize > sh || src.len() < sw * sh * 4 {
        return None;
    }
    let (x, w) = (r.x as usize, r.w as usize);
    let mut out = Vec::with_capacity(w * r.h as usize * 4);
    for row in r.y as usize..(r.y + r.h) as usize {
        let start = (row * sw + x) * 4;
        out.extend_from_slice(&src[start..start + w * 4]);
    }
    Some(out)
}

/// Double the size with bilinear filtering (text recognises better when small glyphs are bigger).
pub fn upscale2x(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let (nw, nh) = (w * 2, h * 2);
    let mut out = vec![0u8; nw * nh * 4];
    for y in 0..nh {
        // Pixel centres: output y maps to input (y + 0.5) / 2 - 0.5.
        let fy = ((y as f32 + 0.5) / 2.0 - 0.5).clamp(0.0, (h - 1) as f32);
        let (y0, ty) = (fy.floor() as usize, fy.fract());
        let y1 = (y0 + 1).min(h - 1);
        for x in 0..nw {
            let fx = ((x as f32 + 0.5) / 2.0 - 0.5).clamp(0.0, (w - 1) as f32);
            let (x0, tx) = (fx.floor() as usize, fx.fract());
            let x1 = (x0 + 1).min(w - 1);
            for c in 0..4 {
                let p = |xx: usize, yy: usize| src[(yy * w + xx) * 4 + c] as f32;
                let top = p(x0, y0) * (1.0 - tx) + p(x1, y0) * tx;
                let bottom = p(x0, y1) * (1.0 - tx) + p(x1, y1) * tx;
                out[(y * nw + x) * 4 + c] = (top * (1.0 - ty) + bottom * ty).round() as u8;
            }
        }
    }
    out
}

/// Swap light and dark (for light text on a dark background that the recogniser did not read).
pub fn invert(bgra: &mut [u8]) {
    for px in bgra.chunks_exact_mut(4) {
        px[0] = 255 - px[0];
        px[1] = 255 - px[1];
        px[2] = 255 - px[2];
    }
}

/// Is this image mostly dark? (average luma under 110 of 255, sampled)
pub fn is_dark(bgra: &[u8]) -> bool {
    let (mut sum, mut n) = (0u64, 0u64);
    for px in bgra.chunks_exact(4).step_by(7) {
        sum += (px[2] as u64 * 299 + px[1] as u64 * 587 + px[0] as u64 * 114) / 1000;
        n += 1;
    }
    n > 0 && sum / n < 110
}

/// Prepare a cropped region for the recogniser: opaque alpha, and doubled when it is small enough
/// that doubling stays within `max_dim` (the engine's limit). Returns `(pixels, width, height)`.
pub fn prepare(bgra: Vec<u8>, w: u32, h: u32, max_dim: u32) -> (Vec<u8>, u32, u32) {
    let mut px = bgra;
    for p in px.chunks_exact_mut(4) {
        p[3] = 255;
    }
    // Small regions (a line of text, a button) are where accuracy suffers; a big one is fine.
    let small = (w as u64) * (h as u64) <= 900_000;
    if small && w > 0 && h > 0 && w * 2 <= max_dim && h * 2 <= max_dim {
        return (upscale2x(&px, w as usize, h as usize), w * 2, h * 2);
    }
    (px, w, h)
}

/// How many words: runs of non-whitespace characters. Numbers and URLs count as one each.
pub fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

/// The whole text of recognised lines (empty lines dropped, whitespace trimmed).
pub fn join_lines(lines: &[String]) -> String {
    lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drags_in_every_direction_give_the_same_rectangle() {
        let want = Some(Rect { x: 10, y: 20, w: 100, h: 50 });
        for (a, b) in [((10, 20), (110, 70)), ((110, 70), (10, 20)), ((110, 20), (10, 70)), ((10, 70), (110, 20))] {
            assert_eq!(from_drag(a, b, 1920, 1080), want, "{a:?} {b:?}");
        }
    }

    #[test]
    fn selections_are_clamped_and_tiny_ones_dropped() {
        assert_eq!(from_drag((-50, -50), (100, 100), 1920, 1080), Some(Rect { x: 0, y: 0, w: 100, h: 100 }));
        assert_eq!(from_drag((1900, 1000), (2500, 1500), 1920, 1080), Some(Rect { x: 1900, y: 1000, w: 20, h: 80 }));
        assert_eq!(from_drag((5, 5), (5, 5), 100, 100), None, "a click");
        assert_eq!(from_drag((5, 5), (12, 100), 100, 100), None, "7 px wide");
        assert!(from_drag((5, 5), (13, 13), 100, 100).is_some(), "exactly the minimum");
        assert_eq!(from_drag((200, 200), (300, 300), 100, 100), None, "entirely outside");
    }

    fn gradient(w: usize, h: usize) -> Vec<u8> {
        let mut v = vec![];
        for y in 0..h {
            for x in 0..w {
                v.extend([x as u8, y as u8, (x + y) as u8, 255]);
            }
        }
        v
    }

    #[test]
    fn cropping_copies_exactly_the_rectangle() {
        let img = gradient(20, 10);
        let c = crop_bgra(&img, 20, 10, Rect { x: 3, y: 2, w: 4, h: 3 }).unwrap();
        assert_eq!(c.len(), 4 * 3 * 4);
        // Top-left of the crop is pixel (3, 2); bottom-right is (6, 4).
        assert_eq!(&c[..4], &[3, 2, 5, 255]);
        assert_eq!(&c[c.len() - 4..], &[6, 4, 10, 255]);
        for bad in [Rect { x: 18, y: 0, w: 4, h: 2 }, Rect { x: 0, y: 8, w: 2, h: 3 }, Rect { x: -1, y: 0, w: 2, h: 2 }, Rect { x: 0, y: 0, w: 0, h: 2 }] {
            assert!(crop_bgra(&img, 20, 10, bad).is_none(), "{bad:?}");
        }
        assert!(crop_bgra(&img[..40], 20, 10, Rect { x: 0, y: 0, w: 2, h: 2 }).is_none(), "a buffer that is too short");
    }

    #[test]
    fn doubling_keeps_flat_colour_and_smooths_edges() {
        let flat: Vec<u8> = std::iter::repeat_n([10u8, 20, 30, 255], 9).flatten().collect();
        let up = upscale2x(&flat, 3, 3);
        assert_eq!(up.len(), 6 * 6 * 4);
        assert!(up.chunks_exact(4).all(|p| p == [10, 20, 30, 255]));
        // A black / white edge gets in-between greys, and the ends stay pure.
        let edge: Vec<u8> = [0u8, 255].iter().flat_map(|&v| [v, v, v, 255]).collect();
        let up = upscale2x(&edge, 2, 1);
        let vals: Vec<u8> = up.chunks_exact(4).map(|p| p[0]).take(4).collect();
        assert_eq!(vals.len(), 4);
        assert_eq!((vals[0], vals[3]), (0, 255));
        assert!(vals[1] > 0 && vals[1] < 255 && vals[2] > vals[1], "{vals:?}");
    }

    #[test]
    fn preparing_doubles_small_regions_only() {
        let (px, w, h) = prepare([0u8, 0, 0, 7].repeat(40 * 20), 40, 20, 10_000);
        assert_eq!((w, h, px.len()), (80, 40, 80 * 40 * 4));
        assert!(px.chunks_exact(4).all(|p| p[3] == 255), "forced opaque");
        // Over the pixel budget: untouched size.
        let (px, w, h) = prepare(vec![0; 1200 * 900 * 4], 1200, 900, 10_000);
        assert_eq!((w, h, px.len()), (1200, 900, 1200 * 900 * 4));
        // Doubling would exceed the engine limit: untouched.
        let (_, w, h) = prepare(vec![0; 40 * 20 * 4], 40, 20, 60);
        assert_eq!((w, h), (40, 20));
    }

    #[test]
    fn dark_images_are_recognised_and_inverted() {
        let mut dark: Vec<u8> = std::iter::repeat_n([20u8, 20, 20, 255], 100).flatten().collect();
        let light: Vec<u8> = std::iter::repeat_n([240u8, 240, 240, 255], 100).flatten().collect();
        assert!(is_dark(&dark) && !is_dark(&light));
        invert(&mut dark);
        assert_eq!(&dark[..4], &[235, 235, 235, 255], "alpha is left alone");
        assert!(!is_dark(&dark));
    }

    #[test]
    fn counting_and_joining() {
        assert_eq!(word_count("Rapat tim dipindah ke Kamis jam 10.00"), 7);
        assert_eq!(word_count("  a\n b\t\tc "), 3);
        assert_eq!(word_count(""), 0);
        assert_eq!(join_lines(&["  hello ".into(), "".into(), "world".into()]), "hello\nworld");
    }
}
