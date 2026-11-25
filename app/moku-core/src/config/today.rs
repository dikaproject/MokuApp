use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Units {
    Metric,
    Imperial,
}

section! {
    /// `[today]`
    pub struct Today in "today" {
        enabled: bool = true,
        weather: bool = true,
        city: String = "Jakarta".into(),
        /// Filled from geocoding `city` when missing.
        #[serde(skip_serializing_if = "Option::is_none")]
        latitude: Option<f64> = None => super::opt_range(-90.0, 90.0),
        #[serde(skip_serializing_if = "Option::is_none")]
        longitude: Option<f64> = None => super::opt_range(-180.0, 180.0),
        units: Units = Units::Metric,
        /// Indonesian public holidays and cuti bersama (the public Google holiday calendar),
        /// shown in Hari ini, with a "Besok libur" pill the evening before. On by default when the
        /// Windows locale is Indonesian.
        holidays: bool = crate::locale::default_on_for_indonesia(),
    }
}
