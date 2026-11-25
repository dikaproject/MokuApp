//! Small pure pixel helpers shared by the clipboard, capture and shelf panels.

use crate::ui::Image;

/// Centre-crop `img` to the aspect ratio `aw:ah` (so a thumbnail can fill a square / wide tile
/// without being stretched). Premultiplied BGRA in, premultiplied BGRA out.
pub fn crop_to_aspect(img: &Image, aw: u32, ah: u32) -> Image {
    let b = &img.0;
    let (w, h) = (b.width.max(1), b.height.max(1));
    let (aw, ah) = (aw.max(1) as u64, ah.max(1) as u64);
    let (cw, ch) = if (w as u64) * ah > (h as u64) * aw {
        ((((h as u64) * aw / ah).max(1)) as u32, h)
    } else {
        (w, (((w as u64) * ah / aw).max(1)) as u32)
    };
    let (cw, ch) = (cw.min(w), ch.min(h));
    if (cw, ch) == (w, h) {
        return img.clone();
    }
    let (x0, y0) = ((w - cw) / 2, (h - ch) / 2);
    let mut out = Vec::with_capacity((cw * ch * 4) as usize);
    for y in 0..ch {
        let start = (((y0 + y) * w + x0) * 4) as usize;
        out.extend_from_slice(&b.bgra[start..start + (cw * 4) as usize]);
    }
    Image::new(cw, ch, out)
}

/// A hand-edited JSON file saved by an old Notepad starts with a UTF-8 byte order mark, which
/// `serde_json` rejects: skip it.
pub fn strip_utf8_bom(b: &[u8]) -> &[u8] {
    b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b)
}

/// A `CF_DIB` block (40-byte header + bottom-up 32-bit rows) from premultiplied BGRA pixels.
/// Transparent areas are composited over white, because DIB consumers ignore the alpha byte.
pub fn dib_from_premul_bgra(w: u32, h: u32, px: &[u8]) -> Vec<u8> {
    let stride = (w * 4) as usize;
    let mut out = Vec::with_capacity(40 + stride * h as usize);
    out.extend(40u32.to_le_bytes());
    out.extend((w as i32).to_le_bytes());
    out.extend((h as i32).to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(32u16.to_le_bytes());
    out.extend(0u32.to_le_bytes()); // BI_RGB
    out.extend(((stride * h as usize) as u32).to_le_bytes());
    out.extend([0u8; 16]); // pixels per metre ×2, colours used / important
    for y in (0..h as usize).rev() {
        let row = &px[y * stride..(y + 1) * stride];
        for p in row.chunks_exact(4) {
            let a = p[3] as u32;
            if a == 255 {
                out.extend_from_slice(p);
            } else {
                let bg = 255 - a;
                out.extend([(p[0] as u32 + bg).min(255) as u8, (p[1] as u32 + bg).min(255) as u8, (p[2] as u32 + bg).min(255) as u8, 255]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32) -> Image {
        let mut v = Vec::new();
        for y in 0..h {
            for x in 0..w {
                v.extend([x as u8, y as u8, 0, 255]);
            }
        }
        Image::new(w, h, v)
    }

    #[test]
    fn json_files_saved_with_a_byte_order_mark_still_load() {
        assert_eq!(strip_utf8_bom(b"\xEF\xBB\xBF{\"a\":1}"), b"{\"a\":1}");
        assert_eq!(strip_utf8_bom(b"{\"a\":1}"), b"{\"a\":1}");
        assert_eq!(strip_utf8_bom(b""), b"");
        let dir = std::env::temp_dir().join(format!("moku-bom-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let idx = dir.join("index.json");
        let mut json = b"\xEF\xBB\xBF".to_vec();
        json.extend(br#"{"version":1,"items":[{"key":7,"kind":"text","ts":5,"preview":"halo"}]}"#);
        std::fs::write(&idx, json).unwrap();
        let h = super::super::model::History::load(&idx);
        assert_eq!(h.items.len(), 1);
        assert_eq!(h.items[0].preview, "halo");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crop_wide_image_to_square_keeps_the_centre() {
        let sq = crop_to_aspect(&gradient(4, 2), 1, 1);
        assert_eq!((sq.0.width, sq.0.height), (2, 2));
        // Columns 1..3 of the source.
        assert_eq!(&sq.0.bgra[0..4], &[1, 0, 0, 255]);
        assert_eq!(&sq.0.bgra[4..8], &[2, 0, 0, 255]);
        assert_eq!(&sq.0.bgra[8..12], &[1, 1, 0, 255]);
    }

    #[test]
    fn crop_tall_image_and_noop() {
        let wide = crop_to_aspect(&gradient(2, 6), 2, 1);
        assert_eq!((wide.0.width, wide.0.height), (2, 1));
        assert_eq!(&wide.0.bgra[0..4], &[0, 2, 0, 255], "centre rows");
        let same = gradient(3, 3);
        let c = crop_to_aspect(&same, 1, 1);
        assert_eq!(c, same, "already the right shape: the same allocation is reused");
        // Degenerate aspect arguments do not panic or divide by zero.
        let _ = crop_to_aspect(&gradient(5, 5), 0, 0);
    }

    #[test]
    fn dib_is_bottom_up_and_composited_over_white() {
        // Row 0 (top): opaque red then fully transparent; row 1: half-transparent black.
        let px = [0, 0, 255, 255, 0, 0, 0, 0, 0, 0, 0, 128, 0, 0, 0, 128];
        let dib = dib_from_premul_bgra(2, 2, &px);
        assert_eq!(dib.len(), 40 + 16);
        assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(dib[14..16].try_into().unwrap()), 32);
        // First stored row is the bottom row of the image (half-transparent black → mid grey-ish).
        assert_eq!(&dib[40..44], &[127, 127, 127, 255]);
        // Then the top row: red stays red, transparent becomes white.
        assert_eq!(&dib[48..52], &[0, 0, 255, 255]);
        assert_eq!(&dib[52..56], &[255, 255, 255, 255]);
        // And the whole block parses with the model's own DIB reader.
        let info = super::super::model::dib_info(&dib).unwrap();
        assert_eq!((info.width, info.height, info.pixel_offset), (2, 2, 40));
    }
}
