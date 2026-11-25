//! UI language. Indonesian is the default, English is available (`general.language`).
//!
//! Strings are written next to where they are used as a pair: `tr("Jeda", "Pause")`. Core
//! strings live here as constants so they are easy to review; features keep their own pairs in
//! their own files (no shared table to conflict on).

use std::sync::atomic::{AtomicU8, Ordering};

use chrono::{Datelike, Timelike};
use moku_core::config::Language;

static LANG: AtomicU8 = AtomicU8::new(0);

pub fn set_language(l: Language) {
    LANG.store(if l == Language::En { 1 } else { 0 }, Ordering::Relaxed);
}

pub fn is_english() -> bool {
    LANG.load(Ordering::Relaxed) == 1
}

/// Pick the string for the current language. Callable from any thread.
pub fn tr(id: &'static str, en: &'static str) -> &'static str {
    if is_english() { en } else { id }
}

/// Same for owned strings (formatted text).
pub fn tr_s(id: String, en: String) -> String {
    if is_english() { en } else { id }
}

/// A `(id, en)` pair (as used by `TabSpec::label`).
pub fn pair(p: (&'static str, &'static str)) -> &'static str {
    tr(p.0, p.1)
}

// ---- core strings -----------------------------------------------------------------------------

pub const SETTINGS: (&str, &str) = ("Pengaturan", "Settings");
pub const PAUSE: (&str, &str) = ("Jeda", "Pause");
pub const RESUME: (&str, &str) = ("Lanjutkan", "Resume");
pub const QUIT: (&str, &str) = ("Keluar", "Quit");
pub const COMING_SOON: (&str, &str) = ("Segera hadir", "Coming soon");
pub const COMING_SOON_SUB: (&str, &str) = ("Fitur ini sedang dibuat.", "This feature is being built.");
pub const PHONE_TAB: (&str, &str) = ("HP", "Phone");
pub const PHONE_SUB: (&str, &str) = ("Moku Link untuk HP hadir di versi berikutnya.", "Moku Link for phones arrives in a later version.");

const DAYS_ID: [&str; 7] = ["Sen", "Sel", "Rab", "Kam", "Jum", "Sab", "Min"];
const DAYS_EN: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const DAYS_LONG_ID: [&str; 7] = ["Senin", "Selasa", "Rabu", "Kamis", "Jumat", "Sabtu", "Minggu"];
const DAYS_LONG_EN: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const MONTHS_ID: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "Mei", "Jun", "Jul", "Agu", "Sep", "Okt", "Nov", "Des"];
const MONTHS_EN: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTHS_LONG_ID: [&str; 12] = ["Januari", "Februari", "Maret", "April", "Mei", "Juni", "Juli", "Agustus", "September", "Oktober", "November", "Desember"];
const MONTHS_LONG_EN: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// Short weekday, Monday = 0.
pub fn weekday_short(monday0: u32) -> &'static str {
    let i = (monday0 % 7) as usize;
    if is_english() { DAYS_EN[i] } else { DAYS_ID[i] }
}
pub fn weekday_long(monday0: u32) -> &'static str {
    let i = (monday0 % 7) as usize;
    if is_english() { DAYS_LONG_EN[i] } else { DAYS_LONG_ID[i] }
}
/// Short month, January = 1.
pub fn month_short(month1: u32) -> &'static str {
    let i = ((month1 + 11) % 12) as usize;
    if is_english() { MONTHS_EN[i] } else { MONTHS_ID[i] }
}
pub fn month_long(month1: u32) -> &'static str {
    let i = ((month1 + 11) % 12) as usize;
    if is_english() { MONTHS_LONG_EN[i] } else { MONTHS_LONG_ID[i] }
}

/// Clock as in the mockups: `10.24` (Indonesian) / `10:24` (English), 24 h.
pub fn clock<T: Timelike>(t: &T) -> String {
    let sep = if is_english() { ':' } else { '.' };
    format!("{:02}{sep}{:02}", t.hour(), t.minute())
}

/// `Sel, 24 Nov` / `Tue, 24 Nov`.
pub fn date_short<T: Datelike>(d: &T) -> String {
    format!("{}, {} {}", weekday_short(d.weekday().num_days_from_monday()), d.day(), month_short(d.month()))
}
