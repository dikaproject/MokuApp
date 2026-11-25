//! Image decode / encode via WIC, shared by features (album art, clipboard thumbnails,
//! captures, shelf previews). Works on any thread that has COM initialised (the UI thread is
//! an STA; in a service call `CoInitializeEx(None, COINIT_MULTITHREADED)` first). Decoding a
//! large image takes milliseconds: do it in a service / worker thread, then `Ui::post` the
//! resulting [`Image`].

use windows::Win32::Foundation::HGLOBAL;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::StructuredStorage::{CreateStreamOnHGlobal, GetHGlobalFromStream};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, STATFLAG_NONAME, STATSTG};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::core::{GUID, Result};

use crate::ui::Image;

fn factory() -> Result<IWICImagingFactory> {
    unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
}

/// Size that fits `w`×`h` into `max`×`max` keeping the aspect ratio (never upscales).
pub fn fit(w: u32, h: u32, max: u32) -> (u32, u32) {
    if w <= max && h <= max {
        return (w.max(1), h.max(1));
    }
    let k = max as f64 / w.max(h) as f64;
    (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1))
}

fn to_image(f: &IWICImagingFactory, src: &IWICBitmapSource, max_px: u32) -> Result<Image> {
    unsafe {
        let (mut w, mut h) = (0, 0);
        src.GetSize(&mut w, &mut h)?;
        let (tw, th) = fit(w, h, max_px);
        let scaled: IWICBitmapSource = if (tw, th) != (w, h) {
            let s = f.CreateBitmapScaler()?;
            s.Initialize(src, tw, th, WICBitmapInterpolationModeFant)?;
            s.into()
        } else {
            src.clone()
        };
        let conv = f.CreateFormatConverter()?;
        conv.Initialize(&scaled, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
        let mut buf = vec![0u8; (tw * th * 4) as usize];
        conv.CopyPixels(std::ptr::null(), tw * 4, &mut buf)?;
        Ok(Image::new(tw, th, buf))
    }
}

/// Decode PNG / JPEG / BMP / GIF (first frame) / … bytes into a premultiplied BGRA image,
/// downscaled to fit `max_px` × `max_px`.
pub fn decode(bytes: &[u8], max_px: u32) -> Result<Image> {
    unsafe {
        let f = factory()?;
        let stream = f.CreateStream()?;
        stream.InitializeFromMemory(bytes)?;
        let dec = f.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
        let frame = dec.GetFrame(0)?;
        to_image(&f, &frame.into(), max_px)
    }
}

/// Wrap raw 32-bit pixels (e.g. from GDI `GetDIBits` or the clipboard) as an image, downscaled
/// to fit `max_px`. `premultiplied` = the alpha is already premultiplied.
pub fn from_bgra(width: u32, height: u32, stride: u32, pixels: &[u8], premultiplied: bool, max_px: u32) -> Result<Image> {
    unsafe {
        let f = factory()?;
        let fmt: GUID = if premultiplied { GUID_WICPixelFormat32bppPBGRA } else { GUID_WICPixelFormat32bppBGRA };
        let bmp = f.CreateBitmapFromMemory(width, height, &fmt, stride, pixels)?;
        to_image(&f, &bmp.into(), max_px)
    }
}

/// Encode an image as PNG bytes (straight alpha, as PNG requires).
pub fn encode_png(img: &Image) -> Result<Vec<u8>> {
    let b = &img.0;
    let mut straight = b.bgra.clone();
    for p in straight.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    unsafe {
        let f = factory()?;
        let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true)?;
        let enc = f.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        enc.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.ok_or_else(windows::core::Error::empty)?;
        frame.Initialize(None)?;
        frame.SetSize(b.width, b.height)?;
        let mut fmt = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut fmt)?;
        frame.WritePixels(b.height, b.width * 4, &straight)?;
        frame.Commit()?;
        enc.Commit()?;
        // The HGLOBAL may be larger than what was written: take the stream's own length.
        let mut stat = STATSTG::default();
        stream.Stat(&mut stat, STATFLAG_NONAME)?;
        let hg = GetHGlobalFromStream(&stream)?;
        let size = (stat.cbSize as usize).min(GlobalSize(hg));
        let p = GlobalLock(hg) as *const u8;
        if p.is_null() {
            return Err(windows::core::Error::from_thread());
        }
        let out = std::slice::from_raw_parts(p, size).to_vec();
        let _ = GlobalUnlock(hg);
        Ok(out)
    }
}

/// Encode as PNG and write to `path`.
pub fn save_png(img: &Image, path: &std::path::Path) -> std::io::Result<()> {
    let bytes = encode_png(img).map_err(std::io::Error::other)?;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

    #[test]
    fn png_roundtrip_and_downscale() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        // 64×32 opaque red / half-transparent blue halves.
        let (w, h) = (64u32, 32u32);
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let p = if x < 32 { [0, 0, 255, 255] } else { [128, 0, 0, 128] }; // premultiplied
                px[i..i + 4].copy_from_slice(&p);
            }
        }
        let img = Image::new(w, h, px);
        let png = encode_png(&img).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        // Ends exactly with the IEND chunk (no trailing HGLOBAL slack).
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
        let back = decode(&png, 256).unwrap();
        assert_eq!((back.0.width, back.0.height), (64, 32));
        assert_eq!(&back.0.bgra[0..4], &[0, 0, 255, 255]);
        let half = decode(&png, 16).unwrap();
        assert_eq!((half.0.width, half.0.height), (16, 8));
        assert_eq!(fit(4000, 3000, 128), (128, 96));
        assert_eq!(fit(10, 10, 128), (10, 10));
        assert!(decode(b"not an image", 64).is_err());
    }
}
