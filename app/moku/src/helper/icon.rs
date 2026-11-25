//! Shell icons for the helper process: `IShellItemImageFactory` pulls in the shell's heavy
//! libraries (windows.storage, thumbnail handlers, the image factory), so this only ever runs
//! in the short-lived helper, never in the island.

use std::ffi::c_void;

use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Shell::{IShellItem, IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY};
use windows::core::{HSTRING, Interface};

use super::pack::{ICON_PX, Icon, downscale};

/// A COM apartment for the current thread (released when dropped).
pub struct Com(bool);

impl Com {
    pub fn init() -> Com {
        Com(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

/// The icon the shell shows for `parsing_name` (a file path, a `.lnk`, `shell:AppsFolder\<AUMID>`),
/// at [`ICON_PX`] (a bigger one the shell hands back is shrunk). Icon only, no thumbnail.
/// Blocking; needs COM on the thread ([`Com`]).
pub fn shell_icon(parsing_name: &str) -> Option<Icon> {
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(parsing_name), None).ok()?;
        let f: IShellItemImageFactory = item.cast().ok()?;
        let hbmp = f.GetImage(SIZE { cx: ICON_PX as i32, cy: ICON_PX as i32 }, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK).ok()?;
        let icon = bitmap_to_icon(hbmp);
        let _ = DeleteObject(hbmp.into());
        icon
    }
}

unsafe fn bitmap_to_icon(hbmp: HBITMAP) -> Option<Icon> {
    unsafe {
        let mut bm = BITMAP::default();
        if GetObjectW(hbmp.into(), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut c_void)) == 0 {
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
        if w <= 0 || h <= 0 || w > 512 || h > 512 {
            return None;
        }
        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        let dc = GetDC(None);
        let got = GetDIBits(dc, hbmp, 0, h as u32, Some(px.as_mut_ptr() as *mut c_void), &mut bi, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        if got != h {
            return None;
        }
        let px = premultiply(px);
        let (tw, th, bgra) = downscale(&px, w as u32, h as u32, ICON_PX, ICON_PX);
        Some(Icon { w: tw as u16, h: th as u16, bgra })
    }
}

/// Shell bitmaps are usually premultiplied; a pixel brighter than its alpha means straight alpha,
/// and no alpha at all means an opaque (old-style) icon.
pub fn premultiply(mut px: Vec<u8>) -> Vec<u8> {
    if px.chunks_exact(4).all(|p| p[3] == 0) {
        px.chunks_exact_mut(4).for_each(|p| p[3] = 255);
        return px;
    }
    let straight = px.chunks_exact(4).any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]);
    if straight {
        for p in px.chunks_exact_mut(4) {
            let a = p[3] as u32;
            for c in &mut p[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
    }
    px
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_alpha_is_premultiplied_premultiplied_is_kept_alpha_less_is_opaque() {
        assert_eq!(premultiply(vec![200, 100, 50, 128]), vec![100, 50, 25, 128]);
        assert_eq!(premultiply(vec![100, 50, 25, 128]), vec![100, 50, 25, 128]);
        assert_eq!(premultiply(vec![1, 2, 3, 0]), vec![1, 2, 3, 255]);
    }

    /// A real shell icon (reads only), in this test process, which is a helper in miniature.
    #[test]
    fn the_shell_gives_an_icon_for_a_system_file() {
        let _com = Com::init();
        let sys = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let icon = shell_icon(&format!(r"{sys}\System32\notepad.exe")).expect("an icon");
        assert!(icon.w > 0 && icon.w <= 48 && icon.bgra.len() == icon.w as usize * icon.h as usize * 4);
        assert!(icon.bgra.chunks_exact(4).any(|p| p[3] > 0), "not fully transparent");
    }
}
