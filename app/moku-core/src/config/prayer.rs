use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrayerMethod {
    /// Kemenag RI: Fajr 20°, Isha 18°, Asr shadow 1, +2 min ihtiyat.
    Kemenag,
    /// Malaysia (JAKIM): Fajr 18°, Isha 18°, Asr shadow 1, +2 min ihtiyat.
    Jakim,
}

section! {
    /// `[prayer]` (optional module, off by default)
    pub struct Prayer in "prayer" {
        enabled: bool = false,
        city: String = "Jakarta".into(),
        latitude: f64 = -6.2088 => super::range(-90.0, 90.0),
        longitude: f64 = 106.8456 => super::range(-180.0, 180.0),
        method: PrayerMethod = PrayerMethod::Kemenag,
        minutes_before: u32 = 10 => super::range(0, 120),
    }
}
