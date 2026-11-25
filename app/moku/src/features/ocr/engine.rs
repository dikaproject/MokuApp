//! The recogniser: `Windows.Media.Ocr.OcrEngine`, which ships with Windows and needs the OCR
//! language feature of at least one language to be installed (Settings > Time & language >
//! Language & region > a language > "Optical character recognition"). Moku never installs one.
//!
//! Everything here blocks (`IAsyncOperation::get`), so it runs on a worker thread with COM
//! initialised, never on the UI thread. The engine is created per request and dropped after it:
//! an idle island holds nothing of it.
//!
//! Privacy: the recognised text is returned to the caller and never logged; an error carries only
//! an HRESULT.

use std::time::{Duration, Instant};

use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::Win32::Foundation::ERROR_TIMEOUT;
use windows::core::HSTRING;

use super::region;

/// The longest a recognition may take (a full 4K screen is a second or two).
const TIMEOUT: Duration = Duration::from_secs(20);

/// What went wrong, without any recognised text in it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum OcrError {
    /// No OCR language is installed (or none could be created).
    NoLanguage,
    /// The region is larger than the engine accepts.
    TooBig(u32),
    /// The engine or the bitmap failed; `HRESULT` for the log.
    Failed(i32),
}

/// The result of one recognition.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Recognized {
    /// One entry per recognised line, top to bottom (empty lines dropped).
    pub lines: Vec<String>,
    /// The language tag the engine used (`id-ID`).
    pub language: String,
}

impl Recognized {
    pub fn text(&self) -> String {
        region::join_lines(&self.lines)
    }
}

/// The OCR languages installed on this PC, as BCP-47 tags (`en-US`, `id-ID`). Empty when none, or
/// when the API is unavailable.
pub fn available_languages() -> Vec<String> {
    OcrEngine::AvailableRecognizerLanguages().map(|v| v.into_iter().filter_map(|l| l.LanguageTag().ok().map(|t| t.to_string())).collect()).unwrap_or_default()
}

/// The installed tag that matches `want`: the same tag (case-insensitive), else one with the same
/// primary language (`en-GB` for `en-US`).
pub fn find_tag(available: &[String], want: &str) -> Option<String> {
    let want = want.trim();
    if want.is_empty() {
        return None;
    }
    if let Some(t) = available.iter().find(|t| t.eq_ignore_ascii_case(want)) {
        return Some(t.clone());
    }
    let primary = |t: &str| t.split('-').next().unwrap_or("").to_ascii_lowercase();
    let p = primary(want);
    available.iter().find(|t| primary(t) == p).cloned()
}

/// The languages to try, in order: the one in the settings, Indonesian, English.
pub fn candidates(preferred: &str) -> Vec<String> {
    let mut v: Vec<String> = vec![];
    for t in [preferred.trim(), "id-ID", "en-US"] {
        if !t.is_empty() && !v.iter().any(|x| x.eq_ignore_ascii_case(t)) {
            v.push(t.to_string());
        }
    }
    v
}

/// The first installed language of [`candidates`], by [`find_tag`].
pub fn pick_language(available: &[String], preferred: &str) -> Option<String> {
    candidates(preferred).iter().find_map(|c| find_tag(available, c))
}

fn hr(e: &windows::core::Error) -> i32 {
    e.code().0
}

/// An engine for the best installed language, with its tag: the settings' language, Indonesian,
/// English, then whatever the user's profile languages give.
fn engine_for(preferred: &str) -> Option<(OcrEngine, String)> {
    let available = available_languages();
    if let Some(tag) = pick_language(&available, preferred)
        && let Ok(lang) = Language::CreateLanguage(&HSTRING::from(tag.as_str()))
        && let Ok(engine) = OcrEngine::TryCreateFromLanguage(&lang)
    {
        return Some((engine, tag));
    }
    let engine = OcrEngine::TryCreateFromUserProfileLanguages().ok()?;
    let tag = engine.RecognizerLanguage().and_then(|l| l.LanguageTag()).map(|t| t.to_string()).unwrap_or_default();
    Some((engine, tag))
}

/// Run the engine over opaque BGRA pixels. Lines come back top to bottom.
fn run(engine: &OcrEngine, bgra: &[u8], w: u32, h: u32) -> Result<Vec<String>, OcrError> {
    let fail = |e: windows::core::Error| OcrError::Failed(hr(&e));
    let writer = DataWriter::new().map_err(fail)?;
    writer.WriteBytes(bgra).map_err(fail)?;
    let buffer = writer.DetachBuffer().map_err(fail)?;
    let bitmap = SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Bgra8, w as i32, h as i32).map_err(fail)?;
    // Poll instead of `join()`: a recogniser that never answers must not hang the worker for good.
    let op = engine.RecognizeAsync(&bitmap).map_err(fail)?;
    let start = Instant::now();
    // AsyncStatus: 0 = Started, 1 = Completed, 2 = Canceled, 3 = Error.
    while op.Status().map_err(fail)?.0 == 0 {
        if start.elapsed() > TIMEOUT {
            let _ = op.Cancel();
            return Err(OcrError::Failed(ERROR_TIMEOUT.to_hresult().0));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let result = op.GetResults().map_err(fail)?;
    let lines = result.Lines().map_err(fail)?;
    Ok(lines.into_iter().filter_map(|l| l.Text().ok().map(|t| t.to_string())).map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect())
}

/// Recognise the text in top-down BGRA pixels (`w` x `h`). Small regions are doubled first; when
/// nothing is found in a dark region it is tried once more with the colours inverted (light text
/// on a dark background). Blocking: call from a worker thread (MTA).
pub fn recognise(bgra: Vec<u8>, w: u32, h: u32, preferred: &str) -> Result<Recognized, OcrError> {
    if bgra.len() < (w as usize) * (h as usize) * 4 || w == 0 || h == 0 {
        return Err(OcrError::Failed(0));
    }
    let (engine, language) = engine_for(preferred).ok_or(OcrError::NoLanguage)?;
    let max = OcrEngine::MaxImageDimension().unwrap_or(2600);
    if w > max || h > max {
        return Err(OcrError::TooBig(max));
    }
    let (mut px, pw, ph) = region::prepare(bgra, w, h, max);
    let mut lines = run(&engine, &px, pw, ph)?;
    if lines.is_empty() && region::is_dark(&px) {
        region::invert(&mut px);
        lines = run(&engine, &px, pw, ph)?;
    }
    Ok(Recognized { lines, language })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

    fn tags(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn languages_are_picked_by_preference() {
        let both = tags(&["en-US", "id-ID"]);
        assert_eq!(pick_language(&both, "").as_deref(), Some("id-ID"), "Indonesian first");
        assert_eq!(pick_language(&both, "en-US").as_deref(), Some("en-US"), "the setting wins");
        assert_eq!(pick_language(&tags(&["en-US"]), "").as_deref(), Some("en-US"));
        assert_eq!(pick_language(&tags(&["id-ID"]), "").as_deref(), Some("id-ID"));
        // Same primary language is good enough; the order of the list does not matter.
        assert_eq!(pick_language(&tags(&["de-DE", "en-GB"]), "").as_deref(), Some("en-GB"));
        assert_eq!(pick_language(&tags(&["EN-us", "ID-id"]), "id-id").as_deref(), Some("ID-id"));
        // Nothing we prefer: none (the caller then asks the profile languages).
        assert_eq!(pick_language(&tags(&["de-DE", "fr-FR"]), ""), None);
        assert_eq!(pick_language(&tags(&["de-DE"]), "de-DE").as_deref(), Some("de-DE"), "unless the setting names it");
        assert_eq!(pick_language(&[], "en-US"), None);
    }

    #[test]
    fn candidate_order_has_no_duplicates() {
        assert_eq!(candidates(""), ["id-ID", "en-US"]);
        assert_eq!(candidates("en-US"), ["en-US", "id-ID"]);
        assert_eq!(candidates(" id-id "), ["id-id", "en-US"]);
        assert_eq!(candidates("fr-FR"), ["fr-FR", "id-ID", "en-US"]);
        assert_eq!(find_tag(&tags(&["en-US"]), "  "), None);
    }

    fn com() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
    }

    /// Prints what this PC has, so a run shows it with `--nocapture`.
    #[test]
    fn ocr_lists_the_installed_languages() {
        com();
        let langs = available_languages();
        eprintln!("OCR languages installed here: {langs:?}");
        // Whatever is installed, asking for it again finds it.
        for l in &langs {
            assert_eq!(find_tag(&langs, l).as_deref(), Some(l.as_str()));
        }
    }

    /// Share of `expected` words (lower case, punctuation trimmed) that appear in `got`.
    fn recall(expected: &str, got: &str) -> f32 {
        let norm = |s: &str| s.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()).filter(|w| !w.is_empty()).collect::<Vec<_>>();
        let (e, g) = (norm(expected), norm(got));
        e.iter().filter(|w| g.contains(w)).count() as f32 / e.len().max(1) as f32
    }

    /// The test image of rendered text. It needs an OCR language (this PC has en-US); on a machine
    /// without one the OCR tests print "skipped" and pass, instead of failing for a missing feature.
    #[test]
    fn ocr_reads_text_rendered_into_an_image() {
        com();
        if available_languages().is_empty() {
            eprintln!("skipped: no OCR language is installed on this PC (see the engine module docs)");
            return;
        }
        let (w, h) = (900u32, 200u32);
        let lines = [("The quick brown fox jumps over the lazy dog", 20, 20), ("Invoice total 1250 paid on Friday morning", 20, 100)];
        let img = super::super::sample::text_image(w, h, 0xFFFFFF, 0x000000, 32, &lines);
        let r = recognise(img, w, h, "en-US").expect("recognised");
        let text = r.text();
        assert_eq!(r.language, "en-US");
        assert!(r.lines.len() >= 2, "two lines of text: {:?}", r.lines.len());
        let want = "The quick brown fox jumps over the lazy dog Invoice total 1250 paid on Friday morning";
        assert!(recall(want, &text) >= 0.85, "recall {:.2}: {text}", recall(want, &text));
        assert!(text.contains("1250"), "digits are read: {text}");
    }

    /// Indonesian text read with the language that is installed (English on this PC): the Latin
    /// alphabet is the same, so the words still come out.
    #[test]
    fn ocr_reads_indonesian_text_with_whatever_latin_engine_exists() {
        com();
        if available_languages().is_empty() {
            eprintln!("skipped: no OCR language is installed on this PC");
            return;
        }
        let img = super::super::sample::text_image(900, 100, 0xFFFFFF, 0x111111, 32, &[("Rapat tim dipindah ke Kamis jam 10.00", 20, 20)]);
        let r = recognise(img, 900, 100, "").expect("recognised");
        let text = r.text();
        let want = "Rapat tim dipindah ke Kamis jam 10.00";
        assert!(recall(want, &text) >= 0.7, "recall {:.2}: {text}", recall(want, &text));
    }

    /// Light text on a dark background, and a small region (it is doubled first).
    #[test]
    fn ocr_reads_dark_mode_and_small_text() {
        com();
        if available_languages().is_empty() {
            eprintln!("skipped: no OCR language is installed on this PC");
            return;
        }
        let dark = super::super::sample::text_image(700, 90, 0x1E1E24, 0xF0F0F0, 30, &[("Meeting moved to Thursday at ten", 20, 20)]);
        let r = recognise(dark, 700, 90, "en-US").expect("recognised");
        assert!(recall("Meeting moved to Thursday at ten", &r.text()) >= 0.7, "dark: {}", r.text());
        // 15 px text in a 330 x 40 region.
        let small = super::super::sample::text_image(330, 40, 0xFFFFFF, 0x000000, 15, &[("Small print stays readable", 8, 8)]);
        let r = recognise(small, 330, 40, "en-US").expect("recognised");
        assert!(recall("Small print stays readable", &r.text()) >= 0.7, "small: {}", r.text());
    }

    #[test]
    fn a_blank_image_has_no_text() {
        com();
        if available_languages().is_empty() {
            eprintln!("skipped: no OCR language is installed on this PC");
            return;
        }
        let img = super::super::sample::text_image(300, 100, 0xFFFFFF, 0, 20, &[]);
        let r = recognise(img, 300, 100, "").expect("a blank page is not an error");
        assert!(r.lines.is_empty(), "{:?}", r.lines);
        assert_eq!(r.text(), "");
    }

    #[test]
    fn nonsense_input_is_an_error_not_a_crash() {
        com();
        assert!(recognise(vec![], 10, 10, "").is_err());
        assert!(recognise(vec![0; 16], 0, 4, "").is_err());
        // A valid blank image: no text (or no language), never a crash.
        let _ = recognise(vec![0; 64], 4, 4, "");
    }
}
