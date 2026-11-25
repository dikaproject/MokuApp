//! Text rendered into a memory bitmap with GDI. Used by the tests (a known image of text for the
//! recogniser) and by the `MOKU_DEBUG` path that stands in for the frozen screenshot, so a live
//! run never captures anything of the user's screen. Nothing here touches the screen.

use std::ffi::c_void;

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::*;
use windows::core::w;

/// `0xRRGGBB` to a GDI `COLORREF` (`0x00BBGGRR`).
fn colorref(rgb: u32) -> COLORREF {
    COLORREF(((rgb & 0xff) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 0xff))
}

/// A top-down, opaque BGRA image of `w` x `h` filled with `bg` (`0xRRGGBB`), with every
/// `(text, x, y)` of `lines` drawn in `fg` at `font_px` pixels (Segoe UI). Empty on a GDI failure.
pub fn text_image(w: u32, h: u32, bg: u32, fg: u32, font_px: i32, lines: &[(&str, i32, i32)]) -> Vec<u8> {
    if w == 0 || h == 0 || w > 8192 || h > 8192 {
        return vec![];
    }
    unsafe {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w as i32, biHeight: -(h as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let mut out = vec![];
        if let Ok(bmp) = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
            && !bits.is_null()
        {
            let old = SelectObject(dc, bmp.into());
            let brush = CreateSolidBrush(colorref(bg));
            FillRect(dc, &RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 }, brush);
            let _ = DeleteObject(brush.into());
            let font = CreateFontW(-font_px, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, ANTIALIASED_QUALITY, (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32, w!("Segoe UI"));
            let old_font = SelectObject(dc, font.into());
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, colorref(fg));
            for (text, x, y) in lines {
                let wide: Vec<u16> = text.encode_utf16().collect();
                let _ = TextOutW(dc, *x, *y, &wide);
            }
            let _ = GdiFlush();
            out = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
            for px in out.chunks_exact_mut(4) {
                px[3] = 255;
            }
            SelectObject(dc, old_font);
            SelectObject(dc, old);
            let _ = DeleteObject(font.into());
            let _ = DeleteObject(bmp.into());
        }
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        out
    }
}

/// The stand-in for a frozen screen in `MOKU_DEBUG` runs: a light "document" with a few lines of
/// Indonesian and English text (all invented). Image coordinates of the lines are
/// [`SAMPLE_LINES`].
pub fn sample_screen(w: u32, h: u32) -> Vec<u8> {
    let lines: Vec<(&str, i32, i32)> = SAMPLE_LINES.iter().map(|(t, x, y)| (*t, *x, *y)).collect();
    text_image(w, h, 0xF4F6FA, 0x1B1F2A, 30, &lines)
}

/// `(text, x, y)` of the text in [`sample_screen`]: the first two lines are Indonesian, the next
/// two English.
pub const SAMPLE_LINES: [(&str, i32, i32); 4] = [
    ("Rapat tim dipindah ke Kamis jam 10.00", 40, 40),
    ("Tolong kabari yang lain sebelum makan siang", 40, 100),
    ("The quick brown fox jumps over the lazy dog", 40, 200),
    ("Invoice total 1250 paid on Friday morning", 40, 260),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_images_are_opaque_and_contain_ink() {
        let img = text_image(400, 80, 0xFFFFFF, 0x000000, 28, &[("Hello world", 10, 10)]);
        assert_eq!(img.len(), 400 * 80 * 4);
        assert!(img.chunks_exact(4).all(|p| p[3] == 255));
        let dark = img.chunks_exact(4).filter(|p| p[0] < 128).count();
        let light = img.chunks_exact(4).filter(|p| p[0] > 240).count();
        assert!(dark > 200, "the glyphs left ink ({dark} dark pixels)");
        assert!(light > 400 * 80 / 2, "most of the page is background");
        // The corner far from the text is exactly the background colour.
        assert_eq!(&img[img.len() - 4..], &[255, 255, 255, 255]);
    }

    #[test]
    fn colours_are_rgb_not_bgr() {
        let img = text_image(8, 8, 0x336699, 0, 12, &[]);
        assert_eq!(&img[..4], &[0x99, 0x66, 0x33, 255], "BGRA of #336699");
    }

    #[test]
    fn silly_sizes_give_nothing() {
        assert!(text_image(0, 10, 0, 0, 12, &[]).is_empty());
        assert!(text_image(10, 0, 0, 0, 12, &[]).is_empty());
        assert!(text_image(100_000, 10, 0, 0, 12, &[]).is_empty());
    }
}
