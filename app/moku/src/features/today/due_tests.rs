//! The due-time parser's table of phrases (test module of `due`, see `due.rs`).
//!
//! "Now" is fixed: Wednesday 25 November 2026, 10:00, unless a case says otherwise.

use chrono::{NaiveDate, NaiveDateTime};

use super::*;

/// The local zone of the table: Jakarta (UTC+7, WIB).
const WIB: i32 = 7 * 3600;

fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, mo, d).unwrap().and_hms_opt(h, mi, 0).unwrap()
}

/// Wednesday 2026-11-25 10:00.
fn now() -> NaiveDateTime {
    at(2026, 11, 25, 10, 0)
}

#[track_caller]
fn check(input: &str, now: NaiveDateTime, title: &str, due: Option<NaiveDateTime>) {
    let p = parse_zoned(input, now, WIB);
    assert_eq!((p.title.as_str(), p.due), (title, due), "input {input:?} at {now}");
}

#[test]
fn the_fixed_now_is_a_wednesday() {
    use chrono::Datelike;
    assert_eq!(now().weekday(), chrono::Weekday::Wed);
}

#[test]
fn clock_times_in_indonesian() {
    let n = now();
    // "jam 3" at 10:00 is 15:00 (03:00 is the middle of the night: not meant).
    check("rapat jam 3", n, "rapat", Some(at(2026, 11, 25, 15, 0)));
    check("jam 15.30 telepon ibu", n, "telepon ibu", Some(at(2026, 11, 25, 15, 30)));
    check("telepon ibu jam 15:30", n, "telepon ibu", Some(at(2026, 11, 25, 15, 30)));
    check("pukul 16.45 ambil paket", n, "ambil paket", Some(at(2026, 11, 25, 16, 45)));
    check("ambil paket pk 17.00", n, "ambil paket", Some(at(2026, 11, 25, 17, 0)));
    check("rapat jam 15.30 WIB", n, "rapat", Some(at(2026, 11, 25, 15, 30)));
    // The hour then the part of the day.
    check("makan jam 8 malam", n, "makan", Some(at(2026, 11, 25, 20, 0)));
    check("sholat jam 3 sore", n, "sholat", Some(at(2026, 11, 25, 15, 0)));
    check("olahraga jam 6 pagi", n, "olahraga", Some(at(2026, 11, 26, 6, 0)));
    check("olahraga jam 3 pagi", n, "olahraga", Some(at(2026, 11, 26, 3, 0)));
    check("makan jam 12 siang", n, "makan", Some(at(2026, 11, 25, 12, 0)));
    check("makan jam 1 siang", n, "makan", Some(at(2026, 11, 25, 13, 0)));
    check("tidur jam 12 malam", n, "tidur", Some(at(2026, 11, 26, 0, 0)));
    check("bangun jam 2 malam", n, "bangun", Some(at(2026, 11, 26, 2, 0)));
    // "setengah 4" is 03:30.
    check("rapat jam setengah 4", n, "rapat", Some(at(2026, 11, 25, 15, 30)));
    check("rapat setengah 8 malam", n, "rapat", Some(at(2026, 11, 25, 19, 30)));
    // The part of the day first.
    check("nanti malam jam 9 telepon", n, "telepon", Some(at(2026, 11, 25, 21, 0)));
}

#[test]
fn the_ambiguous_hour() {
    // Rule: a bare hour 1-12 is the next time that clock shows, ignoring 00:00-05:59 unless it is
    // night now; with a date it is the likelier one (7-12 = morning, 1-6 = afternoon).
    check("rapat jam 3", at(2026, 11, 25, 10, 0), "rapat", Some(at(2026, 11, 25, 15, 0)));
    check("rapat jam 3", at(2026, 11, 25, 16, 0), "rapat", Some(at(2026, 11, 26, 15, 0)));
    check("rapat jam 3", at(2026, 11, 25, 2, 0), "rapat", Some(at(2026, 11, 25, 3, 0)));
    check("rapat jam 3", at(2026, 11, 25, 5, 59), "rapat", Some(at(2026, 11, 25, 15, 0)));
    check("jam 9 rapat", at(2026, 11, 25, 8, 0), "rapat", Some(at(2026, 11, 25, 9, 0)));
    check("jam 9 rapat", at(2026, 11, 25, 10, 0), "rapat", Some(at(2026, 11, 25, 21, 0)));
    check("jam 9 rapat", at(2026, 11, 25, 22, 0), "rapat", Some(at(2026, 11, 26, 9, 0)));
    check("jam 8 rapat", at(2026, 11, 25, 20, 0), "rapat", Some(at(2026, 11, 26, 8, 0)));
    check("jam 12 makan", at(2026, 11, 25, 10, 0), "makan", Some(at(2026, 11, 25, 12, 0)));
    check("jam 12 makan", at(2026, 11, 25, 13, 0), "makan", Some(at(2026, 11, 26, 12, 0)));
    check("jam 10 makan", at(2026, 11, 25, 10, 0), "makan", Some(at(2026, 11, 25, 22, 0)));
    check("jam 11 makan", at(2026, 11, 25, 10, 0), "makan", Some(at(2026, 11, 25, 11, 0)));
    // With a date.
    check("besok jam 3 rapat", now(), "rapat", Some(at(2026, 11, 26, 15, 0)));
    check("besok jam 6 rapat", now(), "rapat", Some(at(2026, 11, 26, 18, 0)));
    check("besok jam 7 rapat", now(), "rapat", Some(at(2026, 11, 26, 7, 0)));
    check("besok jam 9 rapat", now(), "rapat", Some(at(2026, 11, 26, 9, 0)));
    check("besok jam 12 makan", now(), "makan", Some(at(2026, 11, 26, 12, 0)));
    // A leading zero, an hour above 12, or an hour of 10+ with minutes is a 24-hour time.
    check("rapat 03:00", at(2026, 11, 25, 10, 0), "rapat", Some(at(2026, 11, 26, 3, 0)));
    check("rapat jam 03.00", at(2026, 11, 25, 10, 0), "rapat", Some(at(2026, 11, 26, 3, 0)));
    check("rapat jam 10.30", at(2026, 11, 25, 9, 0), "rapat", Some(at(2026, 11, 25, 10, 30)));
    check("rapat jam 13", at(2026, 11, 25, 9, 0), "rapat", Some(at(2026, 11, 25, 13, 0)));
    // The time of today that has passed means tomorrow's.
    check("jam 15.30 telepon", at(2026, 11, 25, 16, 0), "telepon", Some(at(2026, 11, 26, 15, 30)));
    check("jam 15.30 telepon", at(2026, 11, 25, 15, 30), "telepon", Some(at(2026, 11, 26, 15, 30)));
    // A single digit hour with minutes is ambiguous too.
    check("rapat 3:30", at(2026, 11, 25, 10, 0), "rapat", Some(at(2026, 11, 25, 15, 30)));
    check("rapat besok 9.30", now(), "rapat", Some(at(2026, 11, 26, 9, 30)));
}

#[test]
fn dates_in_indonesian() {
    let n = now();
    check("kirim laporan besok 09:00", n, "kirim laporan", Some(at(2026, 11, 26, 9, 0)));
    check("bayar listrik besok", n, "bayar listrik", Some(at(2026, 11, 26, 9, 0)));
    check("besok bayar listrik", n, "bayar listrik", Some(at(2026, 11, 26, 9, 0)));
    check("service motor lusa", n, "service motor", Some(at(2026, 11, 27, 9, 0)));
    check("service motor lusa jam 4 sore", n, "service motor", Some(at(2026, 11, 27, 16, 0)));
    check("meeting senin depan", n, "meeting", Some(at(2026, 11, 30, 9, 0)));
    check("meeting senin", n, "meeting", Some(at(2026, 11, 30, 9, 0)));
    check("meeting hari senin", n, "meeting", Some(at(2026, 11, 30, 9, 0)));
    check("meeting senin jam 3 sore", n, "meeting", Some(at(2026, 11, 30, 15, 0)));
    check("ke gereja hari minggu", n, "ke gereja", Some(at(2026, 11, 29, 9, 0)));
    // Today is a Wednesday: "rabu" is the next one, unless it is for a time still ahead today.
    check("arisan rabu", n, "arisan", Some(at(2026, 12, 2, 9, 0)));
    check("arisan rabu depan", n, "arisan", Some(at(2026, 12, 2, 9, 0)));
    check("arisan rabu jam 3", n, "arisan", Some(at(2026, 11, 25, 15, 0)));
    check("arisan rabu jam 9 pagi", n, "arisan", Some(at(2026, 12, 2, 9, 0)));
    check("kumpul jumat", n, "kumpul", Some(at(2026, 11, 27, 9, 0)));
    check("kumpul jumat depan", n, "kumpul", Some(at(2026, 12, 4, 9, 0)));
    check("kumpul jum'at", n, "kumpul", Some(at(2026, 11, 27, 9, 0)));
    check("kumpul sabtu", n, "kumpul", Some(at(2026, 11, 28, 9, 0)));
    check("kuras bak minggu depan", n, "kuras bak", Some(at(2026, 11, 30, 9, 0)));
    check("rabu", n, "rabu", Some(at(2026, 12, 2, 9, 0)));
    // Dates by number.
    check("bayar cicilan tanggal 28", n, "bayar cicilan", Some(at(2026, 11, 28, 9, 0)));
    check("bayar cicilan tgl 28", n, "bayar cicilan", Some(at(2026, 11, 28, 9, 0)));
    check("bayar cicilan tgl 5", n, "bayar cicilan", Some(at(2026, 12, 5, 9, 0)));
    check("bayar cicilan tgl 25", n, "bayar cicilan", Some(at(2026, 12, 25, 9, 0)));
    check("bayar pajak 28 desember", n, "bayar pajak", Some(at(2026, 12, 28, 9, 0)));
    check("bayar pajak 3 november", n, "bayar pajak", Some(at(2027, 11, 3, 9, 0)));
    check("bayar pajak 1 jan jam 8 pagi", n, "bayar pajak", Some(at(2027, 1, 1, 8, 0)));
    check("tgl 31 tutup buku", at(2026, 11, 25, 10, 0), "tutup buku", Some(at(2026, 12, 31, 9, 0)));
    // 31 does not exist in February/April/...: the next month that has it.
    check("tgl 31 tutup buku", at(2027, 1, 31, 10, 0), "tutup buku", Some(at(2027, 3, 31, 9, 0)));
    // Today: with a time, or the end of the working day.
    check("kirim faktur hari ini", n, "kirim faktur", Some(at(2026, 11, 25, 17, 0)));
    check("kirim faktur hari ini", at(2026, 11, 25, 18, 0), "kirim faktur hari ini", None);
    check("hari ini jam 4 sore kirim faktur", n, "kirim faktur", Some(at(2026, 11, 25, 16, 0)));
}

#[test]
fn parts_of_the_day_in_indonesian() {
    let n = now();
    check("telepon dokter nanti malam", n, "telepon dokter", Some(at(2026, 11, 25, 20, 0)));
    check("telepon dokter malam ini", n, "telepon dokter", Some(at(2026, 11, 25, 20, 0)));
    check("jemur baju nanti siang", n, "jemur baju", Some(at(2026, 11, 25, 12, 0)));
    check("jemur baju nanti sore", n, "jemur baju", Some(at(2026, 11, 25, 16, 0)));
    check("besok pagi lari", n, "lari", Some(at(2026, 11, 26, 8, 0)));
    check("lari besok pagi", n, "lari", Some(at(2026, 11, 26, 8, 0)));
    check("rapat besok siang", n, "rapat", Some(at(2026, 11, 26, 12, 0)));
    check("rapat besok sore", n, "rapat", Some(at(2026, 11, 26, 16, 0)));
    check("makan besok malam", n, "makan", Some(at(2026, 11, 26, 20, 0)));
    check("rapat lusa pagi", n, "rapat", Some(at(2026, 11, 27, 8, 0)));
    check("rapat senin pagi", n, "rapat", Some(at(2026, 11, 30, 8, 0)));
    // The default time of the part of the day has passed, but the part has not: soon, not tomorrow.
    check("telepon nanti malam", at(2026, 11, 25, 21, 0), "telepon", Some(at(2026, 11, 25, 21, 30)));
    check("telepon pagi ini", at(2026, 11, 25, 10, 0), "telepon", Some(at(2026, 11, 25, 10, 30)));
    // Over: tomorrow's.
    check("telepon nanti pagi", at(2026, 11, 25, 13, 0), "telepon", Some(at(2026, 11, 26, 8, 0)));
    // A part of the day alone is not a time.
    check("makan malam sama ibu", n, "makan malam sama ibu", None);
}

#[test]
fn relative_times() {
    let n = now();
    check("kirim email 30 menit lagi", n, "kirim email", Some(at(2026, 11, 25, 10, 30)));
    check("kirim email 2 jam lagi", n, "kirim email", Some(at(2026, 11, 25, 12, 0)));
    check("kirim email dalam 2 jam", n, "kirim email", Some(at(2026, 11, 25, 12, 0)));
    check("kirim email sejam lagi", n, "kirim email", Some(at(2026, 11, 25, 11, 0)));
    check("kirim email setengah jam lagi", n, "kirim email", Some(at(2026, 11, 25, 10, 30)));
    check("kirim email 1,5 jam lagi", n, "kirim email", Some(at(2026, 11, 25, 11, 30)));
    check("kirim email 3 hari lagi", n, "kirim email", Some(at(2026, 11, 28, 10, 0)));
    check("kirim email 10 mnt lagi", n, "kirim email", Some(at(2026, 11, 25, 10, 10)));
    check("5 menit lagi minum obat", n, "minum obat", Some(at(2026, 11, 25, 10, 5)));
    check("call mom in 2 hours", n, "call mom", Some(at(2026, 11, 25, 12, 0)));
    check("call mom in 30 minutes", n, "call mom", Some(at(2026, 11, 25, 10, 30)));
    check("call mom in 45 min", n, "call mom", Some(at(2026, 11, 25, 10, 45)));
    check("call mom in an hour", n, "call mom", Some(at(2026, 11, 25, 11, 0)));
    check("call mom in half an hour", n, "call mom", Some(at(2026, 11, 25, 10, 30)));
    check("call mom in 2 days", n, "call mom", Some(at(2026, 11, 27, 10, 0)));
    check("call mom in 1 week", n, "call mom", Some(at(2026, 12, 2, 10, 0)));
    check("call mom in 1.5 hours", n, "call mom", Some(at(2026, 11, 25, 11, 30)));
    // The seconds of "now" are dropped: 10:00:42 + 30 minutes is 10:30.
    let with_secs = NaiveDate::from_ymd_opt(2026, 11, 25).unwrap().and_hms_opt(10, 0, 42).unwrap();
    check("kirim email 30 menit lagi", with_secs, "kirim email", Some(at(2026, 11, 25, 10, 30)));
    // A relative time wins over anything else, which stays in the title.
    check("kirim email 30 menit lagi besok", n, "kirim email besok", Some(at(2026, 11, 25, 10, 30)));
    // Not a duration from now.
    check("belajar 3 jam", n, "belajar 3 jam", None);
    check("belajar selama 2 jam", n, "belajar selama 2 jam", None);
    check("tunggu 0 menit lagi", n, "tunggu 0 menit lagi", None);
    check("tunggu 100000 jam lagi", n, "tunggu 100000 jam lagi", None);
}

#[test]
fn english_phrases() {
    let n = now();
    check("submit report tomorrow 9am", n, "submit report", Some(at(2026, 11, 26, 9, 0)));
    check("submit report tomorrow", n, "submit report", Some(at(2026, 11, 26, 9, 0)));
    check("tomorrow submit report", n, "submit report", Some(at(2026, 11, 26, 9, 0)));
    check("pay rent day after tomorrow", n, "pay rent", Some(at(2026, 11, 27, 9, 0)));
    check("pay rent at 3pm", n, "pay rent", Some(at(2026, 11, 25, 15, 0)));
    check("pay rent at 3:30 pm", n, "pay rent", Some(at(2026, 11, 25, 15, 30)));
    check("pay rent 3pm", n, "pay rent", Some(at(2026, 11, 25, 15, 0)));
    check("pay rent 9am", n, "pay rent", Some(at(2026, 11, 26, 9, 0)));
    check("pay rent at 9 am", n, "pay rent", Some(at(2026, 11, 26, 9, 0)));
    check("pay rent at 12am", n, "pay rent", Some(at(2026, 11, 26, 0, 0)));
    check("pay rent at 12pm", n, "pay rent", Some(at(2026, 11, 25, 12, 0)));
    check("pay rent at 15:30", n, "pay rent", Some(at(2026, 11, 25, 15, 30)));
    check("meeting at 3", n, "meeting", Some(at(2026, 11, 25, 15, 0)));
    check("meeting at 3 tomorrow", n, "meeting", Some(at(2026, 11, 26, 15, 0)));
    check("tomorrow at 9", n, "tomorrow at 9", Some(at(2026, 11, 26, 9, 0)));
    check("gym friday 5pm", n, "gym", Some(at(2026, 11, 27, 17, 0)));
    check("gym on friday at 5pm", n, "gym", Some(at(2026, 11, 27, 17, 0)));
    check("gym this friday", n, "gym", Some(at(2026, 11, 27, 9, 0)));
    check("gym next friday", n, "gym", Some(at(2026, 12, 4, 9, 0)));
    check("review next monday", n, "review", Some(at(2026, 11, 30, 9, 0)));
    check("review monday", n, "review", Some(at(2026, 11, 30, 9, 0)));
    check("review next week", n, "review", Some(at(2026, 11, 30, 9, 0)));
    check("call dad tonight", n, "call dad", Some(at(2026, 11, 25, 20, 0)));
    check("call dad tonight at 9", n, "call dad", Some(at(2026, 11, 25, 21, 0)));
    check("call dad this evening", n, "call dad", Some(at(2026, 11, 25, 18, 0)));
    check("call dad this afternoon", n, "call dad", Some(at(2026, 11, 25, 15, 0)));
    check("call dad tomorrow morning", n, "call dad", Some(at(2026, 11, 26, 8, 0)));
    check("call dad tomorrow evening", n, "call dad", Some(at(2026, 11, 26, 18, 0)));
    check("call dad tomorrow night", n, "call dad", Some(at(2026, 11, 26, 20, 0)));
    check("lunch at noon", n, "lunch", Some(at(2026, 11, 25, 12, 0)));
    check("backup at midnight", n, "backup", Some(at(2026, 11, 26, 0, 0)));
    check("renew licence dec 5", n, "renew licence", Some(at(2026, 12, 5, 9, 0)));
    check("renew licence on 5 december at 2pm", n, "renew licence", Some(at(2026, 12, 5, 14, 0)));
    check("call mom today", n, "call mom", Some(at(2026, 11, 25, 17, 0)));
    // Words that look like a part of the day or a weekday but are not used as one.
    check("night shift handover", n, "night shift handover", None);
    check("buy sunscreen", n, "buy sunscreen", None);
}

#[test]
fn no_time_means_no_due() {
    let n = now();
    for s in [
        "beli kopi",
        "rapat",
        "Review PR #214",
        "jam tangan baru",
        "versi 1.20 rilis",
        "ratio 3:1",
        "baca bab 3.5",
        "jam 25",
        "jam 3.75",
        "pukul 24:30",
        "at 13pm",
        "tgl 32",
        "tgl 0",
        "35 desember",
        "kirim ke 2 toko",
        "jam",
        "pukul",
        "in 2",
        "in",
        "minggu ini",
        "",
        "   ",
    ] {
        let p = parse(s, n);
        assert_eq!(p.due, None, "{s:?} must not get a due time");
        assert_eq!(p.title, s.split_whitespace().collect::<Vec<_>>().join(" "), "{s:?} keeps its text");
    }
}

#[test]
fn the_title_keeps_what_is_not_a_time() {
    let n = now();
    // Case and the rest of the words are kept; the time words are cut out; connectors at the ends go.
    check("Rapat Tim Produk jam 3", n, "Rapat Tim Produk", Some(at(2026, 11, 25, 15, 0)));
    check("  rapat   jam  3  ", n, "rapat", Some(at(2026, 11, 25, 15, 0)));
    check("rapat pada jam 3", n, "rapat", Some(at(2026, 11, 25, 15, 0)));
    check("rapat di kantor jam 3", n, "rapat di kantor", Some(at(2026, 11, 25, 15, 0)));
    check("rapat jam 3, bawa laptop", n, "rapat bawa laptop", Some(at(2026, 11, 25, 15, 0)));
    check("jam 3 rapat tim", n, "rapat tim", Some(at(2026, 11, 25, 15, 0)));
    check("beli 3 jam tangan besok", n, "beli 3 jam tangan", Some(at(2026, 11, 26, 9, 0)));
    // Only the time: the text stays as typed, the due time still applies.
    check("jam 3", n, "jam 3", Some(at(2026, 11, 25, 15, 0)));
    check("besok", n, "besok", Some(at(2026, 11, 26, 9, 0)));
    check("tomorrow 9am", n, "tomorrow 9am", Some(at(2026, 11, 26, 9, 0)));
    // A second date or time is not eaten.
    check("kirim besok lusa", n, "kirim lusa", Some(at(2026, 11, 26, 9, 0)));
}

#[test]
fn hostile_and_odd_input_never_panics() {
    let n = now();
    for s in [
        "jam 99999999999999999999",
        "😀 jam 3 😀",
        "jam ٣",
        "tgl 99999999999 desember",
        "in 99999999999999999999999 hours",
        "setengah",
        "setengah 99",
        "jam setengah",
        "@",
        "at @ 3",
        ":::",
        "...",
        "10:",
        ":30",
        "a.m.",
        "pm",
        "am pm am pm",
        "nanti nanti nanti",
        "depan depan",
        "next next next",
        "in in in",
        "jum'at jum'at",
        "12.60",
        "23:59",
    ] {
        let _ = parse(s, n);
    }
    let long = "jam 3 ".repeat(5000);
    let p = parse(&long, n);
    assert!(p.due.is_some());
    // The date at the end of the supported range does not overflow.
    let end = NaiveDate::from_ymd_opt(262_142, 12, 31).unwrap().and_hms_opt(23, 59, 0).unwrap();
    let _ = parse("besok jam 3", end);
    let _ = parse("30 menit lagi", end);
    let _ = parse("senin depan", end);
}

#[test]
fn labels_for_the_list() {
    let n = now();
    let l = |d: NaiveDateTime, en: bool| label(d, n, en);
    assert_eq!(l(at(2026, 11, 25, 15, 0), false), "Hari ini 15.00");
    assert_eq!(l(at(2026, 11, 25, 15, 0), true), "Today 15:00");
    assert_eq!(l(at(2026, 11, 26, 9, 5), false), "Besok 09.05");
    assert_eq!(l(at(2026, 11, 26, 9, 5), true), "Tomorrow 09:05");
    assert_eq!(l(at(2026, 11, 30, 9, 0), false), "Sen 09.00");
    assert_eq!(l(at(2026, 11, 30, 9, 0), true), "Mon 09:00");
    assert_eq!(l(at(2026, 12, 5, 14, 30), false), "5 Des 14.30");
    assert_eq!(l(at(2026, 12, 5, 14, 30), true), "5 Dec 14:30");
    assert_eq!(l(at(2027, 1, 5, 14, 30), false), "5 Jan 2027 14.30");
    assert_eq!(l(at(2026, 11, 25, 8, 0), false), "Hari ini 08.00");
    assert_eq!(l(at(2026, 11, 24, 8, 0), false), "Kemarin 08.00");
    assert_eq!(l(at(2026, 11, 20, 8, 0), false), "20 Nov 08.00");
}


// ---- midnight, 24:00 and the time zones (v0.3 fix) ----------------------------------------------

/// A table row of `check` with a zone other than Jakarta.
#[track_caller]
fn check_in(zone_hours: i32, input: &str, now: NaiveDateTime, title: &str, due: Option<NaiveDateTime>) {
    let p = parse_zoned(input, now, zone_hours * 3600);
    assert_eq!((p.title.as_str(), p.due), (title, due), "input {input:?} at {now} in UTC+{zone_hours}");
}

#[test]
fn midnight_belongs_to_the_next_day_when_a_day_is_named() {
    let n = now(); // Wednesday 25 November 2026, 10:00
    // Tonight's midnight is 00:00 tomorrow, not the 00:00 that already passed this morning.
    check("kirim laporan malam ini jam 12", n, "kirim laporan", Some(at(2026, 11, 26, 0, 0)));
    // "besok jam 12 malam": the midnight that ends tomorrow.
    check("kirim laporan besok jam 12 malam", n, "kirim laporan", Some(at(2026, 11, 27, 0, 0)));
    check("kirim laporan besok malam jam 12", n, "kirim laporan", Some(at(2026, 11, 27, 0, 0)));
    // Any night hour 0-5 is the next day's small hours when a day is named.
    check("bangun besok jam 2 malam", n, "bangun", Some(at(2026, 11, 27, 2, 0)));
    check("tidur jumat jam 1 malam", n, "tidur", Some(at(2026, 11, 28, 1, 0)));
    // No day named: simply the next time the clock shows (unchanged).
    check("tidur jam 12 malam", n, "tidur", Some(at(2026, 11, 26, 0, 0)));
    check("bangun jam 2 malam", n, "bangun", Some(at(2026, 11, 26, 2, 0)));
    // Evening hours are not touched: 11 malam is 23:00 of the day itself.
    check("besok jam 11 malam telepon", n, "telepon", Some(at(2026, 11, 26, 23, 0)));
    check("lari malam ini jam 9", n, "lari", Some(at(2026, 11, 25, 21, 0)));
    // Typed in the small hours, "malam ini jam 2" is the 02:00 that is coming; the midnight still is tomorrow's.
    check("belajar malam ini jam 2", at(2026, 11, 25, 1, 0), "belajar", Some(at(2026, 11, 25, 2, 0)));
    check("belajar malam ini jam 12", at(2026, 11, 25, 1, 0), "belajar", Some(at(2026, 11, 26, 0, 0)));
}

#[test]
fn twelve_noon_and_the_bare_twelve_stay_as_they_were() {
    let n = now();
    check("makan jam 12 siang", n, "makan", Some(at(2026, 11, 25, 12, 0)));
    check("makan besok jam 12 siang", n, "makan", Some(at(2026, 11, 26, 12, 0)));
    check("makan jam 12", n, "makan", Some(at(2026, 11, 25, 12, 0)));
    check("makan jam 12", at(2026, 11, 25, 13, 0), "makan", Some(at(2026, 11, 26, 12, 0)));
    check("makan besok jam 12", n, "makan", Some(at(2026, 11, 26, 12, 0)));
}

#[test]
fn twenty_four_hundred_is_midnight_of_the_next_day() {
    let n = now();
    check("deadline besok 24:00", n, "deadline", Some(at(2026, 11, 27, 0, 0)));
    check("deadline besok jam 24.00", n, "deadline", Some(at(2026, 11, 27, 0, 0)));
    check("deadline hari ini 24:00", n, "deadline", Some(at(2026, 11, 26, 0, 0)));
    check("deadline senin 24:00", n, "deadline", Some(at(2026, 12, 1, 0, 0)));
    // No day: the next midnight.
    check("deadline 24:00", n, "deadline", Some(at(2026, 11, 26, 0, 0)));
    // Not a time: 24:30, 25:00 and 24 with an am/pm stay plain text.
    for t in ["skor 24:30", "skor 25:00", "ratio 24:01"] {
        check(t, n, t, None);
    }
}

#[test]
fn wib_wita_and_wit_are_converted_to_the_local_zone() {
    let n = now(); // 10:00 in Jakarta (UTC+7)
    // In WIB itself nothing moves.
    check("rapat jam 15.30 WIB", n, "rapat", Some(at(2026, 11, 25, 15, 30)));
    // WITA is one hour ahead of WIB: 15:00 WITA is 14:00 WIB; WIT is two ahead.
    check("rapat jam 3 sore WITA", n, "rapat", Some(at(2026, 11, 25, 14, 0)));
    check("rapat jam 15.00 wit", n, "rapat", Some(at(2026, 11, 25, 13, 0)));
    check("rapat besok 09:00 WITA", n, "rapat", Some(at(2026, 11, 26, 8, 0)));
    // The zone's own clock decides the ambiguous hour and "today": at 10:00 WIB it is already 11:00 WITA, so
    // the next "jam 11" in WITA is 23:00 WITA (22:00 WIB); 12:00 WITA is in an hour (11:00 WIB).
    check("rapat jam 11 WITA", n, "rapat", Some(at(2026, 11, 25, 22, 0)));
    check("rapat jam 12 WITA", n, "rapat", Some(at(2026, 11, 25, 11, 0)));
    // besok 24:00 WIT = 27 Nov 00:00 WIT = 26 Nov 22:00 WIB.
    check("rapat besok 24:00 WIT", n, "rapat", Some(at(2026, 11, 26, 22, 0)));
    // A PC in another zone: Jakarta's WIB is 7 hours ahead of UTC.
    // (at 10:00 UTC it is already 17:00 in Jakarta, so the next 15:30 WIB is tomorrow's: 08:30 UTC)
    check_in(0, "rapat jam 15.30 WIB", n, "rapat", Some(at(2026, 11, 26, 8, 30)));
    check_in(8, "rapat jam 15.30 WIB", at(2026, 11, 25, 11, 0), "rapat", Some(at(2026, 11, 25, 16, 30)));
    check_in(8, "rapat jam 15.30 WITA", at(2026, 11, 25, 11, 0), "rapat", Some(at(2026, 11, 25, 15, 30)));
    // A zone word that is not after a clock time is just a word.
    check("kirim ke kantor WIB hari ini", n, "kirim ke kantor WIB", Some(at(2026, 11, 25, 17, 0)));
    // Relative times have no zone.
    check("bayar 2 jam lagi", n, "bayar", Some(at(2026, 11, 25, 12, 0)));
}
