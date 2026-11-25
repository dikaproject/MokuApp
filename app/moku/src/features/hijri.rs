//! The Hijri date and the puasa sunnah calendar (v0.3, track N).
//!
//! * **Kemenag's published months first.** A real month starts when the government says so (the
//!   sidang isbat, and the Kalender Hijriah Indonesia that Bimas Islam publishes from hisab), and that
//!   can differ by a day from arithmetic. [`KEMENAG_MONTH_STARTS`] holds the first day of every
//!   month from 1 Muharram 1447 to 1 Muharram 1449 (27 Jun 2025 to 6 Jun 2027), each with its source;
//!   a date inside that range is read from it. Outside the range the **tabular civil Islamic
//!   calendar** (30-year cycle, 11 leap years, epoch Friday 16 July 622 Julian) takes over: pure
//!   integer arithmetic, no network, within +-1 day of Kemenag's months (a test measures it).
//! * `ibadah.hijri_offset` (-2..+2 days) is applied **on top** of both, for regional differences
//!   (Muhammadiyah, a local sidang): the date is shifted first, then looked up.
//! * The table must be extended every year when Kemenag publishes the next calendar (add the new
//!   months at the end, keep the sources, run the tests: they check that the months are consecutive
//!   and 29 or 30 days long).
//! * The Hijri day really starts at Maghrib: [`display_date`] switches the shown date then.
//!   Everything that is about a *fasting day* (Ramadan, sunnah fasts) uses the civil date of the
//!   daytime instead.
//! * Puasa sunnah: Senin / Kamis, Ayyamul Bidh (13-15), Arafah (9 Dzulhijjah), Tasu'a and Asyura
//!   (9-10 Muharram). Never in Ramadan and never on the forbidden days (1 Syawal, 10-13
//!   Dzulhijjah).

use chrono::{Datelike, Days, NaiveDate, Weekday};

use crate::app::App;
use crate::i18n::{tr, tr_s};
use crate::ui::*;

/// A date of the Hijri calendar (`month` 1 = Muharram .. 12 = Dzulhijjah).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hijri {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl Hijri {
    pub fn is_ramadan(self) -> bool {
        self.month == RAMADAN
    }
}

pub const RAMADAN: u32 = 9;
const SYAWAL: u32 = 10;
const DZULHIJJAH: u32 = 12;
const MUHARRAM: u32 = 1;

/// `NaiveDate::num_days_from_ce` of 1 Muharram 1 AH (civil epoch, JD 1948439.5).
const EPOCH_CE_DAYS: i64 = 227_015;
/// Days in 30 Hijri years (19 x 354 + 11 x 355).
const CYCLE_DAYS: i64 = 10_631;
/// Leap years of the 30-year cycle (Hijri year modulo 30).
const LEAP_IN_CYCLE: [i64; 11] = [2, 5, 7, 10, 13, 16, 18, 21, 24, 26, 29];

pub fn is_leap(year: i32) -> bool {
    LEAP_IN_CYCLE.contains(&(year as i64).rem_euclid(30))
}

pub fn year_days(year: i32) -> u32 {
    if is_leap(year) { 355 } else { 354 }
}

/// 30 days in odd months, 29 in even ones; the last month has 30 in a leap year.
pub fn month_days(year: i32, month: u32) -> u32 {
    if month % 2 == 1 || (month == 12 && is_leap(year)) { 30 } else { 29 }
}

/// Days from 1 Muharram of year 1 to 1 Muharram of `year` (negative before year 1).
fn days_before_year(year: i32) -> i64 {
    let y0 = year as i64 - 1;
    let cycles = y0.div_euclid(30);
    let in_cycle = y0.rem_euclid(30);
    cycles * CYCLE_DAYS + (1..=in_cycle).map(|y| if LEAP_IN_CYCLE.contains(&y) { 355 } else { 354 }).sum::<i64>()
}

/// The first day of each Hijri month in Kemenag's calendar: `(Hijri year, month 1..=12, (Gregorian
/// year, month, day))`. Consecutive months, so a month ends where the next one starts; the last
/// entry only closes the range. Sources (all read 2026-11-25; the Kalender Hijriah Indonesia is
/// published by Ditjen Bimas Islam, Kementerian Agama RI, and decided by the sidang isbat):
///
/// * 1447 H, 1 Muharram - Safar - Rabiul Awal - Rabiul Akhir - Jumadil Awal - Jumadil Akhir: Kemenag's
///   2025 calendar as reported by detik.com ("Kalender Hijriah Hari 26 Juli 2025", "1 Rabiul Awal 2025
///   Jatuh pada Tanggal Berapa": 1 Rabiul Awal Mon 25 Aug 2025, Maulid Fri 5 Sep 2025), Akurat.co
///   ("Kalender Islam" 15 Sep, 23 Oct 2025), Kompas.tv and nu.or.id (LF PBNU, "Istikmal ... 22 November
///   2025"); 1 Muharram 1447 = Fri 27 Jun 2025 is the national holiday.
/// * 1447 H, Rajab - Dzulhijjah and 1448 H, Muharram - Rajab: Kemenag's Kalender Hijriah Indonesia
///   2026 (goodnewsfromindonesia.id 2025-12-07 "Kalender Hijriah 2026 PDF Kemenag", Liputan6 "Kalender
///   Hijriah 1447-1448 H", detik.com "Kalender Hijriah 1447-1448 H", Katadata "Kalender Islam November
///   2026"), with the two sidang isbat decisions: 1 Ramadan 1447 = Thu 19 Feb 2026 (isbat 17 Feb 2026,
///   setneg.go.id) and 1 Syawal 1447 = Sat 21 Mar 2026 (isbat 19 Mar 2026, announced by the Menteri
///   Agama; nu.or.id, Antara). 10 Dzulhijjah 1447 = Idul Adha Wed 27 May 2026.
/// * 1448 H, Sya'ban - Dzulhijjah and 1449 H, Muharram: Kemenag's Kalender Hijriah Indonesia 2027 (as
///   listed by mamikos.com, which cites detik.com and Kemenag Aceh's PDF; Metro TV agrees on 8 Feb and
///   10 Mar 2027). These months are **hisab**: the sidang isbat decides them near the time and can move
///   one by a day, which `ibadah.hijri_offset` is for.
pub const KEMENAG_MONTH_STARTS: &[(i32, u32, (i32, u32, u32))] = &[
    (1447, 1, (2025, 6, 27)),   // Tahun Baru Islam 1447, Fri 27 Jun 2025 (national holiday; Kemenag 2025 calendar)
    (1447, 2, (2025, 7, 26)),   // Kemenag 2025 calendar: 1 Safar = Sat 26 Jul 2025 (detik.com "Kalender Hijriah Hari 26 Juli 2025")
    (1447, 3, (2025, 8, 25)),   // 1 Rabiul Awal = Mon 25 Aug 2025; Maulid Fri 5 Sep 2025 (detik.com, Kemenag)
    (1447, 4, (2025, 9, 23)),   // 1 Rabiul Akhir = Tue 23 Sep 2025 (Kemenag 2025 calendar via Akurat.co; NU data hilal)
    (1447, 5, (2025, 10, 23)),  // 1 Jumadil Awal = Thu 23 Oct 2025 (Kemenag via Akurat.co)
    (1447, 6, (2025, 11, 22)),  // 1 Jumadil Akhir = Sat 22 Nov 2025 (Kemenag calendar; nu.or.id istikmal, Kompas.tv)
    (1447, 7, (2025, 12, 21)),  // 1 Rajab = Sun 21 Dec 2025 (Kemenag Kalender Hijriah Indonesia 2026; GNFI, Liputan6)
    (1447, 8, (2026, 1, 20)),   // 1 Sya'ban = Tue 20 Jan 2026 (Kemenag calendar 2026; GNFI 2025-12-07, Liputan6)
    (1447, 9, (2026, 2, 19)),   // 1 Ramadan = Thu 19 Feb 2026 (sidang isbat 17 Feb 2026, setneg.go.id)
    (1447, 10, (2026, 3, 21)),  // 1 Syawal = Sat 21 Mar 2026 (sidang isbat 19 Mar 2026, Menteri Agama; nu.or.id, Antara)
    (1447, 11, (2026, 4, 19)),  // 1 Dzulqa'dah = Sun 19 Apr 2026 (Kemenag calendar 2026; GNFI daily calendar, detik.com)
    (1447, 12, (2026, 5, 18)),  // 1 Dzulhijjah = Mon 18 May 2026; Idul Adha Wed 27 May 2026 (Kemenag calendar 2026)
    (1448, 1, (2026, 6, 16)),   // Tahun Baru Islam 1448, Tue 16 Jun 2026 (Kemenag calendar 2026; detik.com, Liputan6)
    (1448, 2, (2026, 7, 16)),   // 1 Safar = Thu 16 Jul 2026 (Kemenag calendar 2026; GNFI)
    (1448, 3, (2026, 8, 14)),   // 1 Rabiul Awal = Fri 14 Aug 2026 (Kemenag calendar 2026; Liputan6, GNFI)
    (1448, 4, (2026, 9, 13)),   // 1 Rabiul Akhir = Sun 13 Sep 2026 (Kemenag calendar 2026; GNFI daily calendar)
    (1448, 5, (2026, 10, 12)),  // 1 Jumadil Awal = Mon 12 Oct 2026 (Kemenag calendar 2026; GNFI)
    (1448, 6, (2026, 11, 11)),  // 1 Jumadil Akhir = Wed 11 Nov 2026 (Kemenag calendar 2026; Katadata "Kalender Islam November 2026")
    (1448, 7, (2026, 12, 10)),  // 1 Rajab = Thu 10 Dec 2026 (Kemenag calendar 2026; Liputan6, GNFI)
    (1448, 8, (2027, 1, 9)),    // 1 Sya'ban = Sat 9 Jan 2027 (Kemenag Kalender Hijriah Indonesia 2027, hisab; mamikos.com listing)
    (1448, 9, (2027, 2, 8)),    // 1 Ramadan = Mon 8 Feb 2027 (Kemenag 2027 calendar, hisab; Metro TV agrees; the isbat decides)
    (1448, 10, (2027, 3, 10)),  // 1 Syawal = Wed 10 Mar 2027 (Kemenag 2027 calendar, hisab; Metro TV agrees; the isbat decides)
    (1448, 11, (2027, 4, 8)),   // 1 Dzulqa'dah = Thu 8 Apr 2027 (Kemenag 2027 calendar, hisab)
    (1448, 12, (2027, 5, 8)),   // 1 Dzulhijjah = Sat 8 May 2027 (Kemenag 2027 calendar, hisab)
    (1449, 1, (2027, 6, 6)),    // 1 Muharram 1449 = Sun 6 Jun 2027 (Kemenag 2027 calendar, hisab): closes 1448, outside the table
];

/// The date in Kemenag's table, or `None` outside its range (before 1 Muharram 1447 or from
/// 1 Muharram 1449 on).
fn kemenag_date(date: NaiveDate) -> Option<Hijri> {
    let day = |e: &(i32, u32, (i32, u32, u32))| NaiveDate::from_ymd_opt(e.2.0, e.2.1, e.2.2);
    KEMENAG_MONTH_STARTS.windows(2).find_map(|w| {
        let (from, next) = (day(&w[0])?, day(&w[1])?);
        (date >= from && date < next).then(|| Hijri { year: w[0].0, month: w[0].1, day: (date - from).num_days() as u32 + 1 })
    })
}

/// The Hijri date of a civil date, `offset_days` added first (the user's correction): Kemenag's
/// published month when the shifted date is inside [`KEMENAG_MONTH_STARTS`], else the tabular calendar.
pub fn from_date(date: NaiveDate, offset_days: i32) -> Hijri {
    let shifted = date.checked_add_signed(chrono::Duration::days(offset_days as i64)).unwrap_or(date);
    kemenag_date(shifted).unwrap_or_else(|| tabular(date, offset_days))
}

/// The tabular civil Islamic date of a civil date, `offset_days` added first. What [`from_date`]
/// uses outside Kemenag's table; also the arithmetic the tests measure the table against.
pub fn tabular(date: NaiveDate, offset_days: i32) -> Hijri {
    let n = date.num_days_from_ce() as i64 + offset_days as i64 - EPOCH_CE_DAYS;
    let cycle = n.div_euclid(CYCLE_DAYS);
    let mut rem = n.rem_euclid(CYCLE_DAYS);
    let mut year = cycle * 30;
    for y in 1..=30i64 {
        let len = if LEAP_IN_CYCLE.contains(&y) { 355 } else { 354 };
        if rem < len {
            year += y;
            break;
        }
        rem -= len;
    }
    let year = year as i32;
    let mut month = 1;
    while month < 12 && rem >= month_days(year, month) as i64 {
        rem -= month_days(year, month) as i64;
        month += 1;
    }
    Hijri { year, month, day: rem as u32 + 1 }
}

/// The civil date of a Hijri date (no offset). `None` outside chrono's range or for a day that
/// the month does not have.
pub fn to_date(h: Hijri) -> Option<NaiveDate> {
    if !(1..=12).contains(&h.month) || h.day == 0 || h.day > month_days(h.year, h.month) {
        return None;
    }
    let before_month: i64 = (1..h.month).map(|m| month_days(h.year, m) as i64).sum();
    let ce = EPOCH_CE_DAYS + days_before_year(h.year) + before_month + h.day as i64 - 1;
    NaiveDate::from_num_days_from_ce_opt(i32::try_from(ce).ok()?)
}

/// The date to *show* at `secs` seconds after midnight: the Hijri day begins at Maghrib, so from
/// Maghrib on it is already tomorrow's date. Without Maghrib (prayer times off) it changes at
/// midnight.
pub fn display_date(date: NaiveDate, secs: f64, maghrib_secs: Option<f64>, offset_days: i32) -> Hijri {
    let after = maghrib_secs.is_some_and(|m| secs >= m);
    from_date(date, offset_days + after as i32)
}

pub fn month_name(month: u32) -> &'static str {
    const ID: [&str; 12] = ["Muharram", "Safar", "Rabiul Awal", "Rabiul Akhir", "Jumadil Awal", "Jumadil Akhir", "Rajab", "Sya'ban", "Ramadan", "Syawal", "Dzulqa'dah", "Dzulhijjah"];
    const EN: [&str; 12] = ["Muharram", "Safar", "Rabi' al-awwal", "Rabi' al-thani", "Jumada al-awwal", "Jumada al-thani", "Rajab", "Sha'ban", "Ramadan", "Shawwal", "Dhu al-Qi'dah", "Dhu al-Hijjah"];
    let i = (month.clamp(1, 12) - 1) as usize;
    tr(ID[i], EN[i])
}

/// `14 Jumadil Akhir 1448 H` / `14 Jumada al-thani 1448 AH`.
pub fn format(h: Hijri) -> String {
    tr_s(format!("{} {} {} H", h.day, month_name(h.month), h.year), format!("{} {} {} AH", h.day, month_name(h.month), h.year))
}

// ---- puasa sunnah -----------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fast {
    Monday,
    Thursday,
    AyyamulBidh,
    Arafah,
    Tasua,
    Asyura,
}

impl Fast {
    pub fn name(self) -> &'static str {
        match self {
            Fast::Monday => tr("Puasa Senin", "Monday fast"),
            Fast::Thursday => tr("Puasa Kamis", "Thursday fast"),
            Fast::AyyamulBidh => tr("Puasa Ayyamul Bidh", "Ayyamul Bidh fast"),
            Fast::Arafah => tr("Puasa Arafah", "Day of Arafah fast"),
            Fast::Tasua => tr("Puasa Tasu'a", "Tasu'a fast"),
            Fast::Asyura => tr("Puasa Asyura", "Ashura fast"),
        }
    }
}

/// Which sunnah fasts the user wants to be reminded of.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub monday_thursday: bool,
    pub ayyamul_bidh: bool,
    pub arafah: bool,
    pub asyura: bool,
}

impl Flags {
    pub fn any(self) -> bool {
        self.monday_thursday || self.ayyamul_bidh || self.arafah || self.asyura
    }
}

/// Days on which fasting is forbidden: 1 Syawal (Idul Fitri) and 10-13 Dzulhijjah (Idul Adha and
/// the days of tasyrik).
pub fn is_forbidden(h: Hijri) -> bool {
    (h.month == SYAWAL && h.day == 1) || (h.month == DZULHIJJAH && (10..=13).contains(&h.day))
}

/// The sunnah fasts that fall on the daytime of `date`, whose Hijri date is `h` (offset already
/// applied). Most specific first. Never in Ramadan (everybody fasts) and never on a forbidden day.
pub fn fasts_on(date: NaiveDate, h: Hijri, flags: Flags) -> Vec<Fast> {
    if h.is_ramadan() || is_forbidden(h) {
        return vec![];
    }
    let mut out = vec![];
    if flags.arafah && h.month == DZULHIJJAH && h.day == 9 {
        out.push(Fast::Arafah);
    }
    if flags.asyura && h.month == MUHARRAM && h.day == 9 {
        out.push(Fast::Tasua);
    }
    if flags.asyura && h.month == MUHARRAM && h.day == 10 {
        out.push(Fast::Asyura);
    }
    if flags.ayyamul_bidh && (13..=15).contains(&h.day) {
        out.push(Fast::AyyamulBidh);
    }
    if flags.monday_thursday {
        match date.weekday() {
            Weekday::Mon => out.push(Fast::Monday),
            Weekday::Thu => out.push(Fast::Thursday),
            _ => {}
        }
    }
    out
}

/// What to remind of on the evening of `today`: the sunnah fasts of tomorrow.
pub fn reminder_for(today: NaiveDate, offset_days: i32, flags: Flags) -> Vec<Fast> {
    let Some(tomorrow) = today.checked_add_days(Days::new(1)) else { return vec![] };
    fasts_on(tomorrow, from_date(tomorrow, offset_days), flags)
}

// ---- header line ------------------------------------------------------------------------------

/// The Hijri date as it is shown now, or `None` while `ibadah.hijri` is off.
pub fn shown_now(app: &App) -> Option<Hijri> {
    let cfg = &app.config().ibadah;
    if !cfg.hijri {
        return None;
    }
    let now = super::prayer::local_now();
    let date = now.naive_date()?;
    Some(display_date(date, now.secs, super::prayer::maghrib_secs(app), cfg.hijri_offset))
}

/// A one-line caption (moon + date) for the "Hari ini" header; `None` while off.
pub fn header_line(app: &App) -> Option<Node> {
    let h = shown_now(app)?;
    Some(row([icon(icons::MOON).font_size(11.0).color(color::YELLOW), caption(format(h))]).gap(5.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn h(year: i32, month: u32, day: u32) -> Hijri {
        Hijri { year, month, day }
    }

    /// Days between two civil dates.
    fn gap(a: NaiveDate, b: NaiveDate) -> i64 {
        (a - b).num_days().abs()
    }

    /// The tabular arithmetic is never more than a day away from Kemenag's published months.
    #[test]
    fn the_tabular_calendar_is_within_a_day_of_kemenags_months() {
        for &(y, m, (gy, gm, gd)) in KEMENAG_MONTH_STARTS {
            let tab = to_date(h(y, m, 1)).unwrap();
            assert!(gap(tab, d(gy, gm, gd)) <= 1, "1 {} {y}: tabular says {tab}, Kemenag {gy}-{gm}-{gd}", month_name(m));
        }
    }

    /// The table is data: consecutive months, 29 or 30 days each, in order, from 1447 to 1449.
    #[test]
    fn the_kemenag_table_is_consecutive_and_covers_1447_and_1448() {
        let t = KEMENAG_MONTH_STARTS;
        assert_eq!((t[0].0, t[0].1), (1447, 1));
        assert_eq!((t[t.len() - 1].0, t[t.len() - 1].1), (1449, 1), "the last entry closes 1448");
        for w in t.windows(2) {
            let (a, b) = (w[0], w[1]);
            let next = if a.1 == 12 { (a.0 + 1, 1) } else { (a.0, a.1 + 1) };
            assert_eq!((b.0, b.1), next, "{a:?} then {b:?}: a month is missing");
            let len = (d(b.2.0, b.2.1, b.2.2) - d(a.2.0, a.2.1, a.2.2)).num_days();
            assert!((29..=30).contains(&len), "{} {} has {len} days", month_name(a.1), a.0);
        }
        // Every month of 1447 and 1448 is there (24 months + the closing 1 Muharram 1449).
        assert_eq!(t.len(), 25);
    }

    #[test]
    fn kemenags_months_are_used_inside_the_table() {
        // The dates of the brief, checked against the sources in KEMENAG_MONTH_STARTS's comment.
        assert_eq!(from_date(d(2026, 2, 19), 0), h(1447, 9, 1), "1 Ramadan 1447: sidang isbat 17 Feb 2026");
        assert_eq!(from_date(d(2026, 2, 18), 0), h(1447, 8, 30), "the day before is 30 Sya'ban (the tabular calendar says Ramadan)");
        assert_eq!(from_date(d(2026, 3, 21), 0), h(1447, 10, 1), "1 Syawal 1447: sidang isbat 19 Mar 2026");
        assert_eq!(from_date(d(2026, 3, 20), 0), h(1447, 9, 30));
        assert_eq!(from_date(d(2026, 6, 16), 0), h(1448, 1, 1), "1 Muharram 1448");
        assert_eq!(from_date(d(2026, 5, 27), 0), h(1447, 12, 10), "Idul Adha 1447");
        // Today (Wed 25 Nov 2026): 1 Jumadil Akhir 1448 was 11 Nov, so this is the 15th.
        assert_eq!(from_date(d(2026, 11, 25), 0), h(1448, 6, 15));
        assert_eq!(from_date(d(2026, 11, 24), 0), h(1448, 6, 14), "Kemenag: 14 Jumadil Akhir 1448");
        // The tabular calendar is a day off here: that is what the table is for.
        assert_eq!(tabular(d(2026, 11, 24), 0), h(1448, 6, 13));
        assert_eq!(tabular(d(2026, 2, 18), 0), h(1447, 9, 1), "tabular: one day early");
        // The first and the last day of the range.
        assert_eq!(from_date(d(2025, 6, 27), 0), h(1447, 1, 1));
        assert_eq!(from_date(d(2027, 6, 5), 0), h(1448, 12, 29));
        // Every day of the range has a valid date and the days count on by one.
        let mut date = d(2025, 6, 27);
        let mut prev: Option<Hijri> = None;
        while date < d(2027, 6, 6) {
            let hj = from_date(date, 0);
            assert!((1..=12).contains(&hj.month) && (1..=30).contains(&hj.day), "{date}: {hj:?}");
            if let Some(p) = prev {
                assert!(hj == h(p.year, p.month, p.day + 1) || (hj.day == 1 && (hj.month == p.month + 1 || (hj.month == 1 && p.month == 12 && hj.year == p.year + 1))), "{date}: {p:?} then {hj:?}");
            }
            prev = Some(hj);
            date = date.checked_add_days(Days::new(1)).unwrap();
        }
    }

    #[test]
    fn outside_the_table_the_tabular_calendar_and_the_offset_still_work() {
        // Before 27 Jun 2025 and from 6 Jun 2027: the tabular date.
        for day in [d(2025, 6, 26), d(2025, 1, 1), d(2027, 6, 6), d(2030, 3, 1), d(1990, 1, 1)] {
            assert_eq!(from_date(day, 0), tabular(day, 0), "{day}");
            assert_eq!(from_date(day, 2), tabular(day, 2), "{day} +2");
            assert_eq!(from_date(day, -2), tabular(day, -2), "{day} -2");
        }
        // The offset applies to the table as well (the date is shifted, then looked up): Muhammadiyah's
        // 1 Ramadan 1447 was Wed 18 Feb 2026, one day before Kemenag's.
        assert_eq!(from_date(d(2026, 2, 18), 1), h(1447, 9, 1));
        assert_eq!(from_date(d(2026, 2, 19), -1), h(1447, 8, 30));
        // Across the end of the table the shifted date decides.
        assert_eq!(from_date(d(2027, 6, 5), 1), tabular(d(2027, 6, 5), 1));
    }

    #[test]
    fn today_25_nov_2026_matches_kemenag_without_any_offset() {
        // Kemenag: 1 Jumadil Akhir 1448 = 11 Nov 2026 -> 25 Nov = 15 Jumadil Akhir 1448 H.
        let today = from_date(d(2026, 11, 25), 0);
        assert_eq!(today, h(1448, 6, 15));
        assert_eq!(format(today), "15 Jumadil Akhir 1448 H");
    }

    #[test]
    fn the_epoch_and_a_well_known_date() {
        // 1 Muharram 1 AH = Friday 16 July 622 Julian = 19 July 622 in the proleptic Gregorian calendar.
        assert_eq!(to_date(h(1, 1, 1)), Some(d(622, 7, 19)));
        assert_eq!(from_date(d(622, 7, 19), 0), h(1, 1, 1));
        assert_eq!(d(622, 7, 19).weekday(), Weekday::Fri);
        // 1 Muharram 1447 (Kemenag: Friday 27 June 2025).
        assert_eq!(tabular(d(2025, 6, 27), 0), h(1447, 1, 1));
    }

    #[test]
    fn year_and_month_lengths() {
        let cycle: u32 = (1..=30).map(year_days).sum();
        assert_eq!(cycle as i64, CYCLE_DAYS);
        assert_eq!((1..=30).filter(|y| is_leap(*y)).count(), 11);
        for y in [1445, 1446, 1447, 1448, 1449] {
            let months: u32 = (1..=12).map(|m| month_days(y, m)).sum();
            assert_eq!(months, year_days(y), "year {y}");
        }
        // 1447 mod 30 = 7 is a leap year (Dzulhijjah 30 days); 1446 mod 30 = 6 is not.
        assert_eq!((month_days(1447, 12), month_days(1446, 12)), (30, 29));
    }

    #[test]
    fn conversion_round_trips_over_two_centuries() {
        let mut date = d(1900, 1, 1);
        let end = d(2100, 12, 31);
        let mut prev: Option<Hijri> = None;
        while date <= end {
            let hj = tabular(date, 0);
            assert!((1..=12).contains(&hj.month) && hj.day >= 1 && hj.day <= month_days(hj.year, hj.month), "{date}: {hj:?}");
            assert_eq!(to_date(hj), Some(date), "{date} -> {hj:?}");
            if let Some(p) = prev {
                // Consecutive days are consecutive Hijri days.
                let next_day = if p.day < month_days(p.year, p.month) { h(p.year, p.month, p.day + 1) } else if p.month < 12 { h(p.year, p.month + 1, 1) } else { h(p.year + 1, 1, 1) };
                assert_eq!(hj, next_day, "{date}");
            }
            prev = Some(hj);
            date = date.checked_add_days(Days::new(1)).unwrap();
        }
    }

    #[test]
    fn offset_moves_the_date_by_whole_days() {
        let day = d(2026, 11, 24);
        for off in -2..=2i64 {
            let shifted = if off >= 0 { day.checked_add_days(Days::new(off as u64)) } else { day.checked_sub_days(Days::new((-off) as u64)) }.unwrap();
            assert_eq!(from_date(day, off as i32), from_date(shifted, 0), "offset {off}");
        }
    }

    #[test]
    fn invalid_hijri_dates_do_not_convert() {
        assert_eq!(to_date(h(1448, 13, 1)), None);
        assert_eq!(to_date(h(1448, 2, 30)), None, "Safar has 29 days");
        assert_eq!(to_date(h(1448, 1, 0)), None);
    }

    #[test]
    fn hijri_day_changes_at_maghrib() {
        let day = d(2026, 11, 24);
        let maghrib = 17.0 * 3600.0 + 55.0 * 60.0;
        let before = display_date(day, maghrib - 1.0, Some(maghrib), 0);
        let after = display_date(day, maghrib, Some(maghrib), 0);
        assert_eq!(before, h(1448, 6, 14));
        assert_eq!(after, h(1448, 6, 15));
        // Prayer times off: midnight rule.
        assert_eq!(display_date(day, 23.0 * 3600.0, None, 0), h(1448, 6, 14));
    }

    #[test]
    fn ramadan_is_detected_from_kemenags_months_and_the_offset() {
        // Kemenag: 1 Ramadan 1447 = 19 Feb 2026, 1 Syawal = 21 Mar 2026 (no offset needed).
        assert!(!from_date(d(2026, 2, 18), 0).is_ramadan());
        assert!(from_date(d(2026, 2, 19), 0).is_ramadan());
        assert_eq!(from_date(d(2026, 3, 20), 0), h(1447, 9, 30));
        assert!(!from_date(d(2026, 3, 21), 0).is_ramadan());
        // The offset moves it for a region that started a day earlier.
        assert!(from_date(d(2026, 2, 18), 1).is_ramadan());
        assert!(!from_date(d(2026, 2, 19), -1).is_ramadan(), "-1: 19 Feb is still 30 Sya'ban");
        // A month away from Ramadan, whatever the offset.
        assert!((-2..=2).all(|o| !from_date(d(2026, 11, 24), o).is_ramadan()));
        // Ramadan lasts 29 or 30 days (Kemenag's 1447: 30), and so does the tabular one outside the table.
        let days = (0..40).filter(|i| from_date(d(2026, 2, 10).checked_add_days(Days::new(*i)).unwrap(), 0).is_ramadan()).count();
        assert!((29..=30).contains(&days), "{days}");
        let tab = (0..40).filter(|i| from_date(d(2029, 12, 28).checked_add_days(Days::new(*i)).unwrap(), 0).is_ramadan()).count();
        assert!((29..=30).contains(&tab), "{tab}");
    }

    #[test]
    fn month_names_cover_the_year() {
        for m in 1..=12 {
            assert!(!month_name(m).is_empty());
        }
        assert_eq!(month_name(6), "Jumadil Akhir");
        assert_eq!(month_name(9), "Ramadan");
        assert_eq!(format(h(1448, 6, 14)), "14 Jumadil Akhir 1448 H");
    }

    const ALL: Flags = Flags { monday_thursday: true, ayyamul_bidh: true, arafah: true, asyura: true };

    #[test]
    fn monday_and_thursday() {
        // 23 Nov 2026 is a Monday, 26 Nov a Thursday, 24 Nov a Tuesday.
        assert_eq!(fasts_on(d(2026, 11, 23), h(1448, 6, 5), ALL), vec![Fast::Monday]);
        assert_eq!(fasts_on(d(2026, 11, 26), h(1448, 6, 5), ALL), vec![Fast::Thursday]);
        assert_eq!(fasts_on(d(2026, 11, 24), h(1448, 6, 5), ALL), vec![]);
        let off = Flags { monday_thursday: false, ..ALL };
        assert_eq!(fasts_on(d(2026, 11, 23), h(1448, 6, 5), off), vec![]);
    }

    #[test]
    fn ayyamul_bidh_13_to_15() {
        for day in 13..=15 {
            assert_eq!(fasts_on(d(2026, 11, 24), h(1448, 6, day), ALL), vec![Fast::AyyamulBidh], "day {day}");
        }
        assert_eq!(fasts_on(d(2026, 11, 24), h(1448, 6, 12), ALL), vec![]);
        assert_eq!(fasts_on(d(2026, 11, 24), h(1448, 6, 16), ALL), vec![]);
        // On a Monday both apply, the Ayyamul Bidh first.
        assert_eq!(fasts_on(d(2026, 11, 23), h(1448, 6, 14), ALL), vec![Fast::AyyamulBidh, Fast::Monday]);
        // Kemenag's list for Jumadil Akhir 1448 is 23-25 Nov 2026 (the table, no offset).
        let on: Vec<u32> = (20..=28).filter(|n| !fasts_on(d(2026, 11, *n), from_date(d(2026, 11, *n), 0), Flags { ayyamul_bidh: true, ..Flags::default() }).is_empty()).collect();
        assert_eq!(on, vec![23, 24, 25]);
    }

    #[test]
    fn arafah_tasua_asyura() {
        assert_eq!(fasts_on(d(2026, 5, 26), h(1447, 12, 9), ALL), vec![Fast::Arafah]);
        assert_eq!(fasts_on(d(2026, 6, 24), h(1448, 1, 9), ALL), vec![Fast::Tasua]);
        // 25 Jun 2026 is a Thursday: both apply, the special day first.
        assert_eq!(fasts_on(d(2026, 6, 25), h(1448, 1, 10), ALL), vec![Fast::Asyura, Fast::Thursday]);
        let none = Flags::default();
        assert_eq!(fasts_on(d(2026, 6, 25), h(1448, 1, 10), none), vec![]);
        // Kemenag: Asyura (9-10 Muharram 1448) = 24-25 Jun 2026, straight from the table.
        assert_eq!(fasts_on(d(2026, 6, 24), from_date(d(2026, 6, 24), 0), ALL), vec![Fast::Tasua]);
        assert_eq!(fasts_on(d(2026, 6, 25), from_date(d(2026, 6, 25), 0), ALL), vec![Fast::Asyura, Fast::Thursday]);
        // Arafah 1447: Kemenag 26 May 2026.
        assert_eq!(fasts_on(d(2026, 5, 26), from_date(d(2026, 5, 26), 0), ALL), vec![Fast::Arafah]);
        assert_eq!(from_date(d(2026, 5, 27), 0), h(1447, 12, 10), "Idul Adha, Kemenag: 27 May 2026");
    }

    #[test]
    fn no_sunnah_fast_in_ramadan_or_on_forbidden_days() {
        // Ramadan: a Monday that is also the 14th is simply a Ramadan day.
        assert_eq!(fasts_on(d(2026, 3, 2), h(1447, 9, 13), ALL), vec![]);
        // 1 Syawal, even on a Monday or Thursday.
        assert!(is_forbidden(h(1447, 10, 1)));
        assert_eq!(fasts_on(d(2026, 3, 23), h(1447, 10, 1), ALL), vec![]);
        // Idul Adha and tasyrik (10-13 Dzulhijjah): the 13th would be Ayyamul Bidh and a Thursday.
        for day in 10..=13 {
            assert!(is_forbidden(h(1447, 12, day)), "{day}");
            assert_eq!(fasts_on(d(2026, 5, 28), h(1447, 12, day), ALL), vec![], "{day}");
        }
        assert!(!is_forbidden(h(1447, 12, 14)));
        assert_eq!(fasts_on(d(2026, 6, 1), h(1447, 12, 15), ALL), vec![Fast::AyyamulBidh, Fast::Monday]);
        assert!(!is_forbidden(h(1447, 10, 2)));
    }

    #[test]
    fn the_reminder_is_the_evening_before() {
        // Sunday 22 Nov 2026 evening -> Monday 23 Nov (12 or 13 Jumadil Akhir).
        let mon = Flags { monday_thursday: true, ..Flags::default() };
        assert_eq!(reminder_for(d(2026, 11, 22), 0, mon), vec![Fast::Monday]);
        assert_eq!(reminder_for(d(2026, 11, 23), 0, mon), vec![], "Monday evening: Tuesday is no fast day");
        assert_eq!(reminder_for(d(2026, 11, 25), 0, mon), vec![Fast::Thursday], "Wednesday evening");
        // Kemenag's date: Monday 23 Nov is 13 Jumadil Akhir, Ayyamul Bidh too; one day earlier it is not.
        assert_eq!(reminder_for(d(2026, 11, 22), 0, ALL), vec![Fast::AyyamulBidh, Fast::Monday]);
        assert_eq!(reminder_for(d(2026, 11, 22), -1, ALL), vec![Fast::Monday]);
        assert!(!Flags::default().any() && ALL.any());
        assert_eq!(reminder_for(d(2026, 11, 22), 0, Flags::default()), vec![]);
        // Weekday arithmetic around the month end.
        assert_eq!(reminder_for(d(2026, 11, 30), 0, Flags { monday_thursday: true, ..Flags::default() }), vec![], "Monday 30 Nov evening: Tuesday");
        assert_eq!(reminder_for(d(2026, 11, 29), 0, mon), vec![Fast::Monday], "Sunday 29 Nov evening");
    }
}
