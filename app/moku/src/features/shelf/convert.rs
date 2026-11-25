//! "Ubah ke...": convert a Shelf file next to where it is.
//!
//! * **Images** (PNG, JPG, BMP, TIFF, GIF, WebP, HEIC, AVIF) become JPG, PNG or WebP through WIC. The
//!   picture is decoded, turned upright by its EXIF orientation (JPEG / TIFF), converted and written
//!   in bands of about 4 MB, so a 50-megapixel photo never needs its whole self in memory and the
//!   tile can show a real progress ring. Transparency is composited on white for JPG. WebP is offered
//!   only when a WIC WebP *encoder* exists (it often does not); HEIC / AVIF / WebP *decoding* needs
//!   the Microsoft Store extensions, and when Windows has none the error says which one
//!   ([`ConvertError::MissingCodec`]) so the island can explain it.
//! * **Audio and video** become MP3, M4A or MP4 through `Windows.Media.Transcoding.MediaTranscoder`
//!   (Media Foundation), with its own progress.
//! * **The output** is `name (Moku).ext` in the source's folder (or the receive folder when that is
//!   read-only), `name (Moku 2).ext` when that exists, and so on: the name is created with `create_new`
//!   before anything is written, so no existing file is ever replaced. A failed or cancelled
//!   conversion removes the file it created, and only that one.
//!
//! Everything here blocks: call it from a worker thread that has COM initialised (MTA).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Variant::VT_UI2;
use windows::core::{GUID, HSTRING, Interface, w};

use crate::features::phone_send::names;

/// What to convert to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Jpg,
    Png,
    Webp,
    Mp3,
    M4a,
    Mp4,
}

impl Target {
    pub fn ext(self) -> &'static str {
        match self {
            Target::Jpg => "jpg",
            Target::Png => "png",
            Target::Webp => "webp",
            Target::Mp3 => "mp3",
            Target::M4a => "m4a",
            Target::Mp4 => "mp4",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Target::Jpg => "JPG",
            Target::Png => "PNG",
            Target::Webp => "WebP",
            Target::Mp3 => "MP3",
            Target::M4a => "M4A",
            Target::Mp4 => "MP4",
        }
    }

    /// The argument of the `shelf.convert` action.
    pub fn id(self) -> &'static str {
        self.ext()
    }

    pub fn from_id(s: &str) -> Option<Target> {
        [Target::Jpg, Target::Png, Target::Webp, Target::Mp3, Target::M4a, Target::Mp4].into_iter().find(|t| t.id() == s)
    }

    pub fn is_image(self) -> bool {
        matches!(self, Target::Jpg | Target::Png | Target::Webp)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Audio,
    Video,
}

fn ext_of(path: &str) -> String {
    let name = path.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or("");
    match name.rsplit_once('.') {
        Some((stem, e)) if !stem.is_empty() => e.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// What kind of media a file name says it is (by extension only; nothing is opened).
pub fn kind_of(path: &str) -> Option<Kind> {
    match ext_of(path).as_str() {
        "png" | "jpg" | "jpeg" | "jfif" | "bmp" | "tif" | "tiff" | "gif" | "webp" | "heic" | "heif" | "avif" => Some(Kind::Image),
        "mp3" | "wav" | "m4a" | "aac" | "wma" | "flac" | "ogg" | "oga" | "opus" => Some(Kind::Audio),
        "mp4" | "m4v" | "mov" | "mkv" | "avi" | "wmv" | "webm" | "flv" | "3gp" => Some(Kind::Video),
        _ => None,
    }
}

/// The formats offered for a file: never its own format; WebP only when an encoder exists.
pub fn targets_for(path: &str, webp_encoder: bool) -> Vec<Target> {
    let ext = ext_of(path);
    let same = |t: Target| match t {
        Target::Jpg => matches!(ext.as_str(), "jpg" | "jpeg" | "jfif"),
        other => ext == other.ext(),
    };
    let all: &[Target] = match kind_of(path) {
        Some(Kind::Image) => &[Target::Jpg, Target::Png, Target::Webp],
        Some(Kind::Audio) => &[Target::Mp3, Target::M4a],
        Some(Kind::Video) => &[Target::Mp4, Target::M4a, Target::Mp3],
        None => &[],
    };
    all.iter().copied().filter(|t| !same(*t) && (*t != Target::Webp || webp_encoder)).collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConvertError {
    /// Windows lacks a codec: "HEIF", "AVIF", "WebP" (Store extensions) or "codec" (media).
    MissingCodec(&'static str),
    /// The file could not be read as the kind of file it claims to be.
    BadSource,
    /// No encoder for the target (e.g. WebP).
    NoEncoder(&'static str),
    /// The user cancelled.
    Cancelled,
    /// Anything else (an HRESULT in hex; never a file name).
    Failed(String),
}

fn failed(e: windows::core::Error) -> ConvertError {
    ConvertError::Failed(format!("0x{:08X}", e.code().0 as u32))
}

fn io_failed(e: io::Error) -> ConvertError {
    ConvertError::Failed(format!("io {:?}", e.kind()))
}

pub type Progress = Arc<dyn Fn(f32) + Send + Sync>;

// ---- output name -----------------------------------------------------------------------------------------

/// Create the (empty) output file for `src` converted to `target`: `name (Moku).ext`, numbered when it
/// exists, next to the source or, when that folder refuses, in `fallback_dir`. Returns its path.
pub fn create_output(src: &Path, target: Target, fallback_dir: &Path) -> io::Result<PathBuf> {
    let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let make = |n: u32| names::moku_variant(&name, Some(target.ext()), n);
    let dir = src.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    match names::create_unique(dir, make) {
        Ok((_, p)) => Ok(p),
        Err(e) if matches!(e.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem | io::ErrorKind::NotFound) || e.raw_os_error() == Some(19) => {
            fs::create_dir_all(fallback_dir)?;
            names::create_unique(fallback_dir, make).map(|(_, p)| p)
        }
        Err(e) => Err(e),
    }
}

// ---- codecs available -------------------------------------------------------------------------------------------

fn factory() -> windows::core::Result<IWICImagingFactory> {
    unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
}

/// Does Windows have a WIC WebP encoder? (Only the decoder ships with the Store extension.)
pub fn webp_encoder_available() -> bool {
    factory().is_ok_and(|f| unsafe { f.CreateEncoder(&GUID_ContainerFormatWebp, std::ptr::null()) }.is_ok())
}

/// Does Windows have a HEIF decoder (the "HEIF Image Extensions")?
pub fn heif_decoder_available() -> bool {
    factory().is_ok_and(|f| unsafe { f.CreateDecoder(&GUID_ContainerFormatHeif, std::ptr::null()) }.is_ok())
}

const WINCODEC_ERR_UNKNOWNIMAGEFORMAT: u32 = 0x8898_2F07;
const WINCODEC_ERR_COMPONENTNOTFOUND: u32 = 0x8898_2F50;
const WINCODEC_ERR_COMPONENTINITIALIZEFAILURE: u32 = 0x8898_2F8B;

/// A decoder failure: for the formats that need an extension, "no decoder" is the likely reason.
fn decode_error(e: windows::core::Error, src: &Path) -> ConvertError {
    let code = e.code().0 as u32;
    let missing = matches!(code, WINCODEC_ERR_UNKNOWNIMAGEFORMAT | WINCODEC_ERR_COMPONENTNOTFOUND | WINCODEC_ERR_COMPONENTINITIALIZEFAILURE);
    match (ext_of(&src.to_string_lossy()).as_str(), missing) {
        ("heic" | "heif", true) => ConvertError::MissingCodec("HEIF"),
        ("avif", true) => ConvertError::MissingCodec("AVIF"),
        ("webp", true) => ConvertError::MissingCodec("WebP"),
        _ if missing => ConvertError::BadSource,
        _ => failed(e),
    }
}

// ---- images ---------------------------------------------------------------------------------------------------------

/// EXIF orientation (1-8) of a JPEG / TIFF frame; 1 when there is none.
fn exif_orientation(dec: &IWICBitmapDecoder, frame: &IWICBitmapFrameDecode) -> u16 {
    unsafe {
        let Ok(container) = dec.GetContainerFormat() else { return 1 };
        let path = if container == GUID_ContainerFormatJpeg {
            w!("/app1/ifd/{ushort=274}")
        } else if container == GUID_ContainerFormatTiff {
            w!("/ifd/{ushort=274}")
        } else {
            return 1;
        };
        let Ok(reader) = frame.GetMetadataQueryReader() else { return 1 };
        let mut pv = PROPVARIANT::default();
        if reader.GetMetadataByName(path, &mut pv).is_err() {
            return 1;
        }
        let v = if pv.Anonymous.Anonymous.vt == VT_UI2 { pv.Anonymous.Anonymous.Anonymous.uiVal } else { 1 };
        let _ = PropVariantClear(&mut pv);
        if (1..=8).contains(&v) { v } else { 1 }
    }
}

/// The WIC transform that turns a picture with this EXIF orientation upright.
fn orientation_transform(o: u16) -> Option<WICBitmapTransformOptions> {
    match o {
        2 => Some(WICBitmapTransformFlipHorizontal),
        3 => Some(WICBitmapTransformRotate180),
        4 => Some(WICBitmapTransformFlipVertical),
        5 => Some(WICBitmapTransformOptions(WICBitmapTransformRotate270.0 | WICBitmapTransformFlipHorizontal.0)),
        6 => Some(WICBitmapTransformRotate90),
        7 => Some(WICBitmapTransformOptions(WICBitmapTransformRotate90.0 | WICBitmapTransformFlipHorizontal.0)),
        8 => Some(WICBitmapTransformRotate270),
        _ => None,
    }
}

/// Premultiplied BGRA -> BGR on white (what a JPEG can hold).
fn composite_on_white(bgra: &[u8], out: &mut Vec<u8>) {
    out.clear();
    for p in bgra.chunks_exact(4) {
        let inv = 255 - p[3] as u16;
        for c in &p[..3] {
            out.push((*c as u16 + inv).min(255) as u8);
        }
    }
}

fn convert_image(src: &Path, out: &Path, target: Target, progress: &Progress, cancel: &AtomicBool) -> Result<(), ConvertError> {
    unsafe {
        let f = factory().map_err(failed)?;
        let dec = f.CreateDecoderFromFilename(&HSTRING::from(src.as_os_str()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).map_err(|e| decode_error(e, src))?;
        let frame = dec.GetFrame(0).map_err(|e| decode_error(e, src))?;
        let mut source: IWICBitmapSource = frame.cast().map_err(failed)?;
        if let Some(opts) = orientation_transform(exif_orientation(&dec, &frame)) {
            let rot = f.CreateBitmapFlipRotator().map_err(failed)?;
            rot.Initialize(&source, opts).map_err(failed)?;
            source = rot.cast().map_err(failed)?;
        }
        let (mut width, mut height) = (0u32, 0u32);
        source.GetSize(&mut width, &mut height).map_err(failed)?;
        if width == 0 || height == 0 || (target == Target::Jpg && (width > 65_500 || height > 65_500)) {
            return Err(ConvertError::BadSource);
        }

        let container = match target {
            Target::Jpg => GUID_ContainerFormatJpeg,
            Target::Png => GUID_ContainerFormatPng,
            Target::Webp => GUID_ContainerFormatWebp,
            _ => return Err(ConvertError::Failed("not an image target".into())),
        };
        let enc = f.CreateEncoder(&container, std::ptr::null()).map_err(|_| ConvertError::NoEncoder(target.label()))?;
        let stream = f.CreateStream().map_err(failed)?;
        stream.InitializeFromFilename(&HSTRING::from(out.as_os_str()), GENERIC_WRITE.0).map_err(failed)?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache).map_err(failed)?;
        let mut fe = None;
        enc.CreateNewFrame(&mut fe, std::ptr::null_mut()).map_err(failed)?;
        let fe = fe.ok_or_else(|| ConvertError::Failed("no frame".into()))?;
        fe.Initialize(None).map_err(failed)?;
        fe.SetSize(width, height).map_err(failed)?;
        let (mut dx, mut dy) = (96.0f64, 96.0f64);
        if source.GetResolution(&mut dx, &mut dy).is_ok() && dx > 0.0 && dy > 0.0 {
            let _ = fe.SetResolution(dx, dy);
        }
        // JPEG takes 24-bit BGR (we composite transparency on white ourselves); the others take BGRA.
        let wanted = if target == Target::Jpg { GUID_WICPixelFormat24bppBGR } else { GUID_WICPixelFormat32bppBGRA };
        let mut got: GUID = wanted;
        fe.SetPixelFormat(&mut got).map_err(failed)?;

        if got != wanted {
            // An encoder that wants something else: let WIC convert, in one go.
            let conv = f.CreateFormatConverter().map_err(failed)?;
            conv.Initialize(&source, &got, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).map_err(failed)?;
            fe.WriteSource(&conv, std::ptr::null()).map_err(failed)?;
            progress(1.0);
        } else {
            // Pixels as 32-bit BGRA: premultiplied for JPEG (so it can be composited), straight for the rest.
            let conv = f.CreateFormatConverter().map_err(failed)?;
            let band_format = if target == Target::Jpg { GUID_WICPixelFormat32bppPBGRA } else { GUID_WICPixelFormat32bppBGRA };
            conv.Initialize(&source, &band_format, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).map_err(failed)?;
            let stride = width.checked_mul(4).ok_or(ConvertError::BadSource)?;
            let rows = (BAND_BYTES / stride).clamp(1, height);
            let mut inbuf = vec![0u8; (stride * rows) as usize];
            let mut rgb: Vec<u8> = Vec::new();
            let mut y = 0u32;
            while y < height {
                if cancel.load(Ordering::Relaxed) {
                    return Err(ConvertError::Cancelled);
                }
                let n = rows.min(height - y);
                let rect = WICRect { X: 0, Y: y as i32, Width: width as i32, Height: n as i32 };
                let part = &mut inbuf[..(stride * n) as usize];
                conv.CopyPixels(&rect, stride, part).map_err(failed)?;
                if target == Target::Jpg {
                    composite_on_white(part, &mut rgb);
                    fe.WritePixels(n, width * 3, &rgb).map_err(failed)?;
                } else {
                    fe.WritePixels(n, stride, part).map_err(failed)?;
                }
                y += n;
                progress(y as f32 / height as f32);
            }
        }
        fe.Commit().map_err(failed)?;
        enc.Commit().map_err(failed)?;
    }
    Ok(())
}

/// How much picture is read and written at a time.
const BAND_BYTES: u32 = 4 * 1024 * 1024;

// ---- audio / video ------------------------------------------------------------------------------------------------------

fn convert_media(src: &Path, out: &Path, target: Target, progress: &Progress, cancel: &Arc<AtomicBool>) -> Result<(), ConvertError> {
    use windows_future::AsyncActionProgressHandler;
    use windows::Media::MediaProperties::{AudioEncodingQuality, MediaEncodingProfile, VideoEncodingQuality};
    use windows::Media::Transcoding::{MediaTranscoder, TranscodeFailureReason};
    use windows::Storage::StorageFile;

    let profile = match target {
        Target::Mp4 => MediaEncodingProfile::CreateMp4(VideoEncodingQuality::Auto),
        Target::M4a => MediaEncodingProfile::CreateM4a(AudioEncodingQuality::High),
        Target::Mp3 => MediaEncodingProfile::CreateMp3(AudioEncodingQuality::High),
        _ => return Err(ConvertError::Failed("not a media target".into())),
    }
    .map_err(failed)?;
    let src_file = StorageFile::GetFileFromPathAsync(&HSTRING::from(src.as_os_str())).map_err(failed)?.join().map_err(|_| ConvertError::BadSource)?;
    // The output file exists (we created it empty, uniquely): the transcoder writes into it.
    let dst_file = StorageFile::GetFileFromPathAsync(&HSTRING::from(out.as_os_str())).map_err(failed)?.join().map_err(failed)?;
    let transcoder = MediaTranscoder::new().map_err(failed)?;
    let prepared = transcoder.PrepareFileTranscodeAsync(&src_file, &dst_file, &profile).map_err(failed)?.join().map_err(failed)?;
    if !prepared.CanTranscode().map_err(failed)? {
        let reason = prepared.FailureReason().map_err(failed)?;
        return Err(if reason == TranscodeFailureReason::CodecNotFound {
            ConvertError::MissingCodec("codec")
        } else if reason == TranscodeFailureReason::InvalidProfile {
            ConvertError::NoEncoder(target.label())
        } else {
            ConvertError::BadSource
        });
    }
    let op = prepared.TranscodeAsync().map_err(failed)?;
    let (p, c) = (progress.clone(), cancel.clone());
    op.SetProgress(&AsyncActionProgressHandler::new(move |sender, value| {
        let v: f64 = *value;
        p((v / 100.0).clamp(0.0, 1.0) as f32);
        if c.load(Ordering::Relaxed)
            && let Some(op) = sender.as_ref()
        {
            let _ = op.Cancel();
        }
        Ok(())
    }))
    .map_err(failed)?;
    match op.join() {
        Ok(()) if !cancel.load(Ordering::Relaxed) => {
            progress(1.0);
            Ok(())
        }
        Ok(()) => Err(ConvertError::Cancelled),
        Err(_) if cancel.load(Ordering::Relaxed) => Err(ConvertError::Cancelled),
        Err(e) => Err(failed(e)),
    }
}

// ---- the whole job ---------------------------------------------------------------------------------------------------------

/// Convert `src` to `target`. The output is created next to the source (or in `fallback_dir`); on any
/// failure or cancel the file this call created is removed. Blocking; needs COM on this thread.
pub fn convert(src: &Path, target: Target, fallback_dir: &Path, progress: &Progress, cancel: &Arc<AtomicBool>) -> Result<PathBuf, ConvertError> {
    let out = create_output(src, target, fallback_dir).map_err(io_failed)?;
    let result = if target.is_image() { convert_image(src, &out, target, progress, cancel) } else { convert_media(src, &out, target, progress, cancel) };
    match result {
        Ok(()) => Ok(out),
        Err(e) => {
            // Only the file we made a moment ago.
            let _ = fs::remove_file(&out);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imaging;
    use crate::ui::Image;
    use std::sync::Mutex;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

    fn com() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moku-convert-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn quiet() -> Progress {
        Arc::new(|_| {})
    }

    fn no_cancel() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    /// A 96x64 test picture: a smooth colour gradient (survives JPEG well).
    fn gradient() -> Image {
        let (w, h) = (96u32, 64u32);
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                px.extend_from_slice(&[(x * 255 / (w - 1)) as u8, (y * 255 / (h - 1)) as u8, 128, 255]);
            }
        }
        Image::new(w, h, px)
    }

    fn write_png(dir: &Path, name: &str, img: &Image) -> PathBuf {
        let p = dir.join(name);
        imaging::save_png(img, &p).unwrap();
        p
    }

    fn near(a: &[u8], b: &[u8], tol: i32) -> bool {
        a.iter().zip(b).all(|(x, y)| (*x as i32 - *y as i32).abs() <= tol)
    }

    // ---- what is offered --------------------------------------------------------------------------------------

    #[test]
    fn formats_offered_per_kind_never_the_files_own() {
        use Target::*;
        assert_eq!(targets_for(r"C:\x\a.png", true), [Jpg, Webp]);
        assert_eq!(targets_for(r"C:\x\a.PNG", false), [Jpg], "no WebP without an encoder");
        assert_eq!(targets_for(r"C:\x\a.jpg", true), [Png, Webp]);
        assert_eq!(targets_for(r"C:\x\a.jpeg", false), [Png]);
        assert_eq!(targets_for(r"C:\x\a.heic", false), [Jpg, Png]);
        assert_eq!(targets_for(r"C:\x\a.webp", true), [Jpg, Png]);
        assert_eq!(targets_for(r"C:\x\a.bmp", false), [Jpg, Png]);
        assert_eq!(targets_for(r"C:\x\a.wav", true), [Mp3, M4a]);
        assert_eq!(targets_for(r"C:\x\a.mp3", true), [M4a]);
        assert_eq!(targets_for(r"C:\x\a.m4a", true), [Mp3]);
        assert_eq!(targets_for(r"C:\x\a.mov", true), [Mp4, M4a, Mp3]);
        assert_eq!(targets_for(r"C:\x\a.mp4", true), [M4a, Mp3]);
        assert!(targets_for(r"C:\x\a.pdf", true).is_empty());
        assert!(targets_for(r"C:\x\noext", true).is_empty());
        assert!(targets_for(r"C:\x\.png", true).is_empty(), "a dotfile is no extension");
        assert_eq!(kind_of("a.HEIC"), Some(Kind::Image));
        assert_eq!(kind_of("a.FLAC"), Some(Kind::Audio));
        assert_eq!(kind_of("a.MKV"), Some(Kind::Video));
        for t in [Jpg, Png, Webp, Mp3, M4a, Mp4] {
            assert_eq!(Target::from_id(t.id()), Some(t));
        }
        assert_eq!(Target::from_id("exe"), None);
    }

    // ---- output names ---------------------------------------------------------------------------------------------

    #[test]
    fn the_output_is_named_with_moku_and_never_replaces_anything() {
        let dir = scratch("names");
        let src = dir.join("photo.png");
        fs::write(&src, b"x").unwrap();
        let fb = dir.join("fallback");
        let a = create_output(&src, Target::Jpg, &fb).unwrap();
        assert_eq!(a.file_name().unwrap(), "photo (Moku).jpg");
        assert_eq!(fs::metadata(&a).unwrap().len(), 0, "created empty, to be filled");
        fs::write(&a, b"first result").unwrap();
        let b = create_output(&src, Target::Jpg, &fb).unwrap();
        assert_eq!(b.file_name().unwrap(), "photo (Moku 2).jpg");
        let c = create_output(&src, Target::Jpg, &fb).unwrap();
        assert_eq!(c.file_name().unwrap(), "photo (Moku 3).jpg");
        assert_eq!(fs::read(&a).unwrap(), b"first result", "an earlier result is untouched");
        // Another target is another name; the source is never touched.
        assert_eq!(create_output(&src, Target::Webp, &fb).unwrap().file_name().unwrap(), "photo (Moku).webp");
        assert_eq!(fs::read(&src).unwrap(), b"x");
        // A name with dots and spaces.
        let src2 = dir.join("My Song.v2.wav");
        fs::write(&src2, b"x").unwrap();
        assert_eq!(create_output(&src2, Target::M4a, &fb).unwrap().file_name().unwrap(), "My Song.v2 (Moku).m4a");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_that_refuses_falls_back() {
        let dir = scratch("fallback");
        // The source "folder" does not exist (a removed drive): the output goes to the fallback folder.
        let src = dir.join("missing-dir").join("a.png");
        let fb = dir.join("fb");
        let out = create_output(&src, Target::Jpg, &fb).unwrap();
        assert_eq!(out, fb.join("a (Moku).jpg"));
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- images through WIC --------------------------------------------------------------------------------------------

    #[test]
    fn png_to_jpg_and_back_round_trips() {
        com();
        let dir = scratch("roundtrip");
        let img = gradient();
        let png = write_png(&dir, "grad.png", &img);
        let seen = Arc::new(Mutex::new(vec![]));
        let s2 = seen.clone();
        let progress: Progress = Arc::new(move |p| s2.lock().unwrap().push(p));
        let jpg = convert(&png, Target::Jpg, &dir.join("fb"), &progress, &no_cancel()).unwrap();
        assert_eq!(jpg.file_name().unwrap(), "grad (Moku).jpg");
        let bytes = fs::read(&jpg).unwrap();
        assert_eq!(&bytes[..3], &[0xFF, 0xD8, 0xFF], "a JPEG");
        let seen = seen.lock().unwrap().clone();
        assert!(!seen.is_empty() && (seen.last().unwrap() - 1.0).abs() < 1e-6, "progress ends at 100 %: {seen:?}");
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "and never goes backwards");
        // Same size, colours close (JPEG is lossy).
        let back = imaging::decode(&bytes, 1000).unwrap();
        assert_eq!((back.0.width, back.0.height), (96, 64));
        for (x, y) in [(10u32, 10u32), (50, 30), (90, 60), (0, 0), (95, 63)] {
            let i = ((y * 96 + x) * 4) as usize;
            assert!(near(&back.0.bgra[i..i + 3], &img.0.bgra[i..i + 3], 14), "pixel {x},{y}: {:?} vs {:?}", &back.0.bgra[i..i + 4], &img.0.bgra[i..i + 4]);
            assert_eq!(back.0.bgra[i + 3], 255);
        }
        // And JPG -> PNG is lossless from what the JPEG holds.
        let png2 = convert(&jpg, Target::Png, &dir.join("fb"), &quiet(), &no_cancel()).unwrap();
        assert_eq!(png2.file_name().unwrap(), "grad (Moku) (Moku).png");
        let pbytes = fs::read(&png2).unwrap();
        assert_eq!(&pbytes[1..4], b"PNG");
        let decoded = imaging::decode(&pbytes, 1000).unwrap();
        assert_eq!((decoded.0.width, decoded.0.height), (96, 64));
        assert_eq!(decoded.0.bgra, back.0.bgra, "PNG holds exactly the JPEG's pixels");
        // The source files are untouched.
        assert_eq!(fs::read(&png).unwrap(), fs::read(dir.join("grad.png")).unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn transparency_is_composited_on_white_for_jpg_and_kept_for_png() {
        com();
        let dir = scratch("alpha");
        // Left half: 50 % transparent red (premultiplied BGRA 0,0,128,128); right half: opaque green.
        let (w, h) = (32u32, 16u32);
        let mut px = vec![];
        for _y in 0..h {
            for x in 0..w {
                px.extend_from_slice(if x < 16 { &[0, 0, 128, 128] } else { &[0, 200, 0, 255] });
            }
        }
        let src = write_png(&dir, "a.png", &Image::new(w, h, px));
        let jpg = convert(&src, Target::Jpg, &dir.join("fb"), &quiet(), &no_cancel()).unwrap();
        let back = imaging::decode(&fs::read(&jpg).unwrap(), 100).unwrap();
        // Red at half alpha over white = (255, 127, 127), not red over black.
        let left = &back.0.bgra[((8 * w + 4) * 4) as usize..][..4];
        assert!(near(&left[..3], &[127, 127, 255], 14), "{left:?}");
        assert_eq!(left[3], 255);
        // Opaque pixels are what they were.
        let right = &back.0.bgra[((8 * w + 28) * 4) as usize..][..4];
        assert!(near(&right[..3], &[0, 200, 0], 14), "{right:?}");
        // To PNG (a copy of the same PNG into BMP first is not needed): alpha stays.
        let bmp_like = convert(&src, Target::Png, &dir.join("fb"), &quiet(), &no_cancel());
        let png = bmp_like.unwrap();
        let again = imaging::decode(&fs::read(&png).unwrap(), 100).unwrap();
        let l = &again.0.bgra[((8 * w + 4) * 4) as usize..][..4];
        assert!(near(l, &[0, 0, 128, 128], 2), "{l:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A JPEG with an EXIF orientation tag, built by hand: the picture's own bytes plus one APP1 segment.
    fn with_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
        let mut tiff = vec![];
        tiff.extend_from_slice(b"MM\0\x2A\0\0\0\x08"); // big endian, IFD at 8
        tiff.extend_from_slice(&1u16.to_be_bytes()); // one entry
        tiff.extend_from_slice(&0x0112u16.to_be_bytes()); // Orientation
        tiff.extend_from_slice(&3u16.to_be_bytes()); // SHORT
        tiff.extend_from_slice(&1u32.to_be_bytes());
        tiff.extend_from_slice(&orientation.to_be_bytes());
        tiff.extend_from_slice(&[0, 0]);
        tiff.extend_from_slice(&0u32.to_be_bytes()); // no next IFD
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
        out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&app1);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    #[test]
    fn a_rotated_phone_photo_comes_out_upright() {
        com();
        let dir = scratch("orient");
        // 96x64 landscape, tagged "rotate 90 clockwise to view": the upright picture is 64x96.
        let jpg0 = convert(&write_png(&dir, "p.png", &gradient()), Target::Jpg, &dir.join("fb"), &quiet(), &no_cancel()).unwrap();
        let plain = fs::read(&jpg0).unwrap();
        for (tag, want) in [(1u16, (96u32, 64u32)), (3, (96, 64)), (6, (64, 96)), (8, (64, 96))] {
            let tagged = dir.join(format!("tag{tag}.jpg"));
            fs::write(&tagged, with_orientation(&plain, tag)).unwrap();
            let png = convert(&tagged, Target::Png, &dir.join("fb"), &quiet(), &no_cancel()).unwrap();
            let img = imaging::decode(&fs::read(&png).unwrap(), 1000).unwrap();
            assert_eq!((img.0.width, img.0.height), want, "orientation {tag}");
        }
        // Rotated 90 clockwise: the original top-left (blue-ish, low x / low y) ends up top-right.
        let tagged = dir.join("tag6 (Moku).png");
        let img = imaging::decode(&fs::read(tagged).unwrap(), 1000).unwrap();
        let orig = imaging::decode(&plain, 1000).unwrap();
        let o = &orig.0.bgra[0..3];
        let top_right = &img.0.bgra[(63 * 4)..(63 * 4 + 3)];
        assert!(near(o, top_right, 4), "{o:?} vs {top_right:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn webp_works_when_windows_can_encode_it_and_is_not_offered_when_it_cannot() {
        com();
        let dir = scratch("webp");
        let png = write_png(&dir, "g.png", &gradient());
        if webp_encoder_available() {
            let out = convert(&png, Target::Webp, &dir.join("fb"), &quiet(), &no_cancel()).unwrap();
            assert_eq!(out.file_name().unwrap(), "g (Moku).webp");
            let b = fs::read(&out).unwrap();
            assert_eq!(&b[..4], b"RIFF");
            assert_eq!(&b[8..12], b"WEBP");
        } else {
            eprintln!("note: no WIC WebP encoder on this machine; the WebP target is hidden");
            let e = convert(&png, Target::Webp, &dir.join("fb"), &quiet(), &no_cancel()).unwrap_err();
            assert_eq!(e, ConvertError::NoEncoder("WebP"));
            assert!(fs::read_dir(&dir).unwrap().all(|f| !f.unwrap().file_name().to_string_lossy().contains("Moku")), "the empty output was removed");
        }
        assert_eq!(targets_for("a.png", webp_encoder_available()).contains(&Target::Webp), webp_encoder_available());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_undecodable_file_fails_cleanly_and_a_missing_heif_extension_is_named() {
        com();
        let dir = scratch("bad");
        let junk = dir.join("junk.png");
        fs::write(&junk, b"this is not a picture at all").unwrap();
        let e = convert(&junk, Target::Jpg, &dir.join("fb"), &quiet(), &no_cancel()).unwrap_err();
        assert!(matches!(e, ConvertError::BadSource | ConvertError::Failed(_)), "{e:?}");
        // A "HEIC" that is not one: with the HEIF extension it is a bad file, without it the extension is named.
        let heic = dir.join("photo.heic");
        fs::write(&heic, b"\0\0\0\x18ftypheic\0\0\0\0heicmif1").unwrap();
        let e = convert(&heic, Target::Jpg, &dir.join("fb"), &quiet(), &no_cancel()).unwrap_err();
        if heif_decoder_available() {
            assert!(matches!(e, ConvertError::BadSource | ConvertError::Failed(_) | ConvertError::MissingCodec(_)), "{e:?}");
        } else {
            assert_eq!(e, ConvertError::MissingCodec("HEIF"), "{e:?}");
        }
        // Nothing was left behind, and the sources are as they were.
        let names: Vec<String> = fs::read_dir(&dir).unwrap().map(|f| f.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert!(names.iter().all(|n| !n.contains("(Moku)")), "{names:?}");
        assert_eq!(fs::read(&junk).unwrap(), b"this is not a picture at all");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_decoder_is_named_after_the_file_type() {
        // WINCODEC_ERR_COMPONENTNOTFOUND / UNKNOWNIMAGEFORMAT / COMPONENTINITIALIZEFAILURE are "no codec".
        let err = |code: u32| windows::core::Error::from_hresult(windows::core::HRESULT(code as i32));
        for code in [0x8898_2F50u32, 0x8898_2F07, 0x8898_2F8B] {
            assert_eq!(decode_error(err(code), Path::new(r"C:\x\IMG_1.HEIC")), ConvertError::MissingCodec("HEIF"));
            assert_eq!(decode_error(err(code), Path::new("a.heif")), ConvertError::MissingCodec("HEIF"));
            assert_eq!(decode_error(err(code), Path::new("a.avif")), ConvertError::MissingCodec("AVIF"));
            assert_eq!(decode_error(err(code), Path::new("a.webp")), ConvertError::MissingCodec("WebP"));
            // A PNG that no decoder understands is just a bad file.
            assert_eq!(decode_error(err(code), Path::new("a.png")), ConvertError::BadSource);
        }
        // Any other failure is reported as it is (the code only, never a name).
        assert_eq!(decode_error(err(0x8000_4005), Path::new("a.heic")), ConvertError::Failed("0x80004005".into()));
    }

    #[test]
    fn cancelling_removes_the_half_written_output() {
        com();
        let dir = scratch("cancel");
        let png = write_png(&dir, "c.png", &gradient());
        let cancel = Arc::new(AtomicBool::new(true));
        let e = convert(&png, Target::Jpg, &dir.join("fb"), &quiet(), &cancel).unwrap_err();
        assert_eq!(e, ConvertError::Cancelled);
        assert!(!dir.join("c (Moku).jpg").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- audio through MediaTranscoder ------------------------------------------------------------------------------------------

    /// One second of a 440 Hz tone: 16-bit mono PCM at 44.1 kHz in a WAV container.
    fn tone_wav() -> Vec<u8> {
        let rate = 44_100u32;
        let samples: Vec<i16> = (0..rate).map(|i| ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 12_000.0) as i16).collect();
        let data_len = (samples.len() * 2) as u32;
        let mut w = Vec::new();
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data_len).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes()); // PCM
        w.extend_from_slice(&1u16.to_le_bytes()); // mono
        w.extend_from_slice(&rate.to_le_bytes());
        w.extend_from_slice(&(rate * 2).to_le_bytes());
        w.extend_from_slice(&2u16.to_le_bytes());
        w.extend_from_slice(&16u16.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            w.extend_from_slice(&s.to_le_bytes());
        }
        w
    }

    /// Transcode the generated WAV. A machine without the codec (a stripped-down Windows) skips the
    /// check, loudly; every other failure is a failure.
    fn transcode_tone(target: Target) {
        com();
        let dir = scratch(&format!("audio-{}", target.ext()));
        let src = dir.join("tone.wav");
        fs::write(&src, tone_wav()).unwrap();
        // An earlier result with the plain Moku name must survive.
        let taken = dir.join(format!("tone (Moku).{}", target.ext()));
        fs::write(&taken, b"mine").unwrap();
        let seen = Arc::new(Mutex::new(vec![]));
        let s2 = seen.clone();
        let progress: Progress = Arc::new(move |p| s2.lock().unwrap().push(p));
        match convert(&src, target, &dir.join("fb"), &progress, &no_cancel()) {
            Ok(out) => {
                assert_eq!(out.file_name().unwrap().to_string_lossy(), format!("tone (Moku 2).{}", target.ext()));
                let b = fs::read(&out).unwrap();
                assert!(b.len() > 1000, "{} bytes", b.len());
                match target {
                    Target::M4a | Target::Mp4 => assert_eq!(&b[4..8], b"ftyp", "an MP4-family container"),
                    Target::Mp3 => assert!(&b[..3] == b"ID3" || (b[0] == 0xFF && b[1] & 0xE0 == 0xE0), "an MP3 stream: {:02X?}", &b[..4]),
                    _ => unreachable!(),
                }
                assert_eq!(fs::read(&taken).unwrap(), b"mine", "the existing file was not replaced");
                let seen = seen.lock().unwrap().clone();
                assert!((seen.last().copied().unwrap_or(0.0) - 1.0).abs() < 1e-6, "progress reaches 100 %: {seen:?}");
                assert!(seen.windows(2).all(|w| w[0] <= w[1]));
            }
            Err(ConvertError::MissingCodec(_) | ConvertError::NoEncoder(_)) => {
                eprintln!("SKIPPED: this Windows has no {} encoder for MediaTranscoder", target.label());
                assert!(!dir.join(format!("tone (Moku 2).{}", target.ext())).exists(), "a failed run leaves nothing");
            }
            Err(e) => panic!("{e:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_generated_wav_becomes_m4a() {
        transcode_tone(Target::M4a);
    }

    #[test]
    fn a_generated_wav_becomes_mp3() {
        transcode_tone(Target::Mp3);
    }
}
