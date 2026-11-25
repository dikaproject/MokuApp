use serde::{Deserialize, Serialize};

/// When the Ramadan countdown pill (imsak / buka puasa) is on the island.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CountdownPill {
    /// All day while fasting; it becomes prominent in the last 30 minutes.
    Always,
    /// Only in the last 30 minutes before Imsak and before Maghrib.
    Last30,
    Off,
}

/// Which sound the adzan plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdzanSound {
    /// The gentle chime Moku generates itself (nothing is bundled).
    Chime,
    /// The user's own audio file (`adzan_file`); a missing or unusable file falls back to the chime.
    File,
}

/// Longest audio file path accepted.
pub const MAX_PATH_CHARS: usize = 1024;

fn path_ok(s: &String) -> Result<(), String> {
    if s.chars().count() > MAX_PATH_CHARS {
        return Err(format!("path longer than {MAX_PATH_CHARS} characters"));
    }
    Ok(())
}

section! {
    /// `[ibadah]`: Hijri date, Ramadan pack, puasa sunnah reminders and adzan audio (v0.3, track N).
    /// The prayer times themselves are `[prayer]`; the Ramadan countdown and the adzan need
    /// `prayer.enabled`.
    pub struct Ibadah in "ibadah" {
        /// Show the Hijri date (Hari ini header, Sehat tab).
        hijri: bool = true,
        /// Days added before the Hijri date is looked up (Kemenag's published months, else the tabular calendar): for a region or an organisation that starts a month on another day.
        hijri_offset: i32 = 0 => super::range(-2, 2),
        /// Detect Ramadan from the Hijri date and show the imsak / buka puasa countdown.
        ramadan: bool = true,
        countdown_pill: CountdownPill = CountdownPill::Always,
        /// Imsak is this many minutes before Fajr; 0 hides Imsak.
        imsak_minutes: u32 = 10 => super::range(0, 30),
        /// Senin dan Kamis: a reminder the evening before.
        sunnah_monday_thursday: bool = false,
        /// Ayyamul Bidh (13, 14, 15 of each Hijri month).
        sunnah_ayyamul_bidh: bool = false,
        sunnah_arafah: bool = false,
        /// Tasu'a and Asyura (9 and 10 Muharram).
        sunnah_asyura: bool = false,
        /// Local hour (24 h clock) of the evening reminder.
        sunnah_hour: u32 = 20 => super::range(12, 23),
        /// Play a sound at prayer time (the pill is shown either way).
        adzan: bool = false,
        adzan_fajr: bool = true,
        adzan_dhuhr: bool = true,
        adzan_asr: bool = true,
        adzan_maghrib: bool = true,
        adzan_isha: bool = true,
        adzan_sound: AdzanSound = AdzanSound::Chime,
        /// Audio file for `adzan_sound = "file"` (wav, mp3, m4a, aac, wma, flac).
        adzan_file: String = String::new() => path_ok,
        /// Percent of the player's volume.
        adzan_volume: u32 = 40 => super::range(5, 100),
        /// Pause media that is playing before the adzan and resume it afterwards (only what Moku paused).
        adzan_pause_media: bool = true,
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Config;

    #[test]
    fn defaults_are_quiet() {
        let c = Config::default().ibadah;
        assert!(c.hijri && c.ramadan);
        assert!(!c.adzan, "no sound unless the user turns it on");
        assert!(!(c.sunnah_monday_thursday || c.sunnah_ayyamul_bidh || c.sunnah_arafah || c.sunnah_asyura), "sunnah reminders are opt-in");
        assert_eq!((c.hijri_offset, c.imsak_minutes, c.adzan_volume, c.sunnah_hour), (0, 10, 40, 20));
    }

    #[test]
    fn invalid_values_fall_back() {
        let (c, w) = Config::parse("[ibadah]\nhijri_offset = 5\nadzan_volume = 0\nsunnah_hour = 3\ncountdown_pill = \"sometimes\"\nadzan_sound = \"file\"\n");
        assert_eq!(w.len(), 4, "{w:?}");
        let i = c.ibadah;
        assert_eq!((i.hijri_offset, i.adzan_volume, i.sunnah_hour), (0, 40, 20));
        assert_eq!(i.countdown_pill, super::CountdownPill::Always);
        assert_eq!(i.adzan_sound, super::AdzanSound::File, "valid neighbours are still taken");
    }

    #[test]
    fn enums_and_malaysia_method_roundtrip() {
        let (c, w) = Config::parse("[prayer]\nmethod = \"jakim\"\n[ibadah]\ncountdown_pill = \"last30\"\nhijri_offset = -2\n");
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.prayer.method, crate::config::PrayerMethod::Jakim);
        assert_eq!((c.ibadah.countdown_pill, c.ibadah.hijri_offset), (super::CountdownPill::Last30, -2));
        let (back, w2) = Config::parse(&c.to_toml());
        assert!(w2.is_empty(), "{w2:?}");
        assert_eq!(back, c);
    }

    #[test]
    fn overlong_path_is_dropped() {
        let long = "a".repeat(super::MAX_PATH_CHARS + 1);
        let (c, w) = Config::parse(&format!("[ibadah]\nadzan_file = \"{long}\"\n"));
        assert_eq!(w.len(), 1);
        assert!(c.ibadah.adzan_file.is_empty());
    }
}
