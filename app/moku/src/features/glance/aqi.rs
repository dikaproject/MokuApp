//! The air-quality chip and warning: Open-Meteo's air-quality API (free, no key),
//! `https://air-quality-api.open-meteo.com/v1/air-quality?latitude=..&longitude=..&current=us_aqi,pm2_5`.
//! The US AQI scale. Pure; the tests run on a live sample saved in `fixtures/`.

pub const BASE_URL: &str = "https://air-quality-api.open-meteo.com/v1/air-quality";
/// The pill appears above this ("Tidak sehat" starts at 151).
pub const ALERT_ABOVE: u32 = 150;
/// The warning is armed again once the air is better than this (so 149 <-> 151 does not repeat it).
pub const REARM_BELOW: u32 = 140;
/// At most one warning in this many seconds, whatever the air does.
pub const MIN_GAP_SECS: i64 = 12 * 3600;

#[derive(Clone, Debug, PartialEq)]
pub struct Aqi {
    pub us_aqi: u32,
    /// Fine particles, µg/m³.
    pub pm25: Option<f64>,
}

pub fn url(lat: f64, lon: f64) -> String {
    format!("{BASE_URL}?latitude={lat:.4}&longitude={lon:.4}&current=us_aqi,pm2_5&timezone=auto")
}

/// Read the `current` block. `None` without a number for the AQI (the API answers `null` for
/// places it has no model for).
pub fn parse(json: &str) -> Option<Aqi> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let cur = v.get("current")?;
    let us_aqi = cur.get("us_aqi")?.as_f64().filter(|a| a.is_finite() && (0.0..=1000.0).contains(a))?.round() as u32;
    let pm25 = cur.get("pm2_5").and_then(|p| p.as_f64()).filter(|p| p.is_finite() && *p >= 0.0);
    Some(Aqi { us_aqi, pm25 })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Good,
    Moderate,
    /// Unhealthy for sensitive groups.
    Sensitive,
    Unhealthy,
    VeryUnhealthy,
    Hazardous,
}

pub fn level(aqi: u32) -> Level {
    match aqi {
        0..=50 => Level::Good,
        51..=100 => Level::Moderate,
        101..=150 => Level::Sensitive,
        151..=200 => Level::Unhealthy,
        201..=300 => Level::VeryUnhealthy,
        _ => Level::Hazardous,
    }
}

/// `(Indonesian, English)`, short enough for a chip.
pub fn label(l: Level) -> (&'static str, &'static str) {
    match l {
        Level::Good => ("Baik", "Good"),
        Level::Moderate => ("Sedang", "Moderate"),
        Level::Sensitive => ("Sensitif", "Sensitive"),
        Level::Unhealthy => ("Tidak sehat", "Unhealthy"),
        Level::VeryUnhealthy => ("Sangat tidak sehat", "Very unhealthy"),
        Level::Hazardous => ("Berbahaya", "Hazardous"),
    }
}

/// When the "unhealthy air" pill is raised: above [`ALERT_ABOVE`], once, until the air has been
/// better than [`REARM_BELOW`], and never twice within [`MIN_GAP_SECS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gate {
    armed: bool,
    last: Option<i64>,
}

impl Default for Gate {
    fn default() -> Gate {
        Gate { armed: true, last: None }
    }
}

impl Gate {
    /// A new reading at `now` (Unix seconds): should the pill be raised?
    pub fn observe(&mut self, aqi: u32, now: i64) -> bool {
        if aqi < REARM_BELOW {
            self.armed = true;
        }
        let spaced = self.last.is_none_or(|t| now - t >= MIN_GAP_SECS || now < t);
        if aqi > ALERT_ABOVE && self.armed && spaced {
            self.armed = false;
            self.last = Some(now);
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // One live sample (Jakarta, 2026-11-25 00:00).
    const SAMPLE: &str = include_str!("fixtures/aqi.json");

    #[test]
    fn the_live_sample_parses() {
        let a = parse(SAMPLE).expect("the live sample");
        assert_eq!((a.us_aqi, a.pm25), (159, Some(114.6)));
        assert_eq!(level(a.us_aqi), Level::Unhealthy);
    }

    #[test]
    fn the_request() {
        assert_eq!(url(-6.2088, 106.8456), "https://air-quality-api.open-meteo.com/v1/air-quality?latitude=-6.2088&longitude=106.8456&current=us_aqi,pm2_5&timezone=auto");
    }

    #[test]
    fn missing_and_odd_values() {
        // No PM2.5 is fine; no AQI is not.
        let a = parse(r#"{"current":{"time":"t","us_aqi":42}}"#).unwrap();
        assert_eq!((a.us_aqi, a.pm25), (42, None));
        assert_eq!(parse(r#"{"current":{"us_aqi":41.6,"pm2_5":null}}"#).map(|a| a.us_aqi), Some(42), "rounded");
        for bad in ["", "{}", "<html>", r#"{"current":{}}"#, r#"{"current":{"us_aqi":null}}"#, r#"{"current":{"us_aqi":"high"}}"#, r#"{"current":{"us_aqi":-3}}"#, r#"{"current":{"us_aqi":5000}}"#, r#"{"error":true,"reason":"x"}"#] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
        // Negative particles are dropped, not shown.
        assert_eq!(parse(r#"{"current":{"us_aqi":10,"pm2_5":-1}}"#).map(|a| a.pm25), Some(None));
    }

    #[test]
    fn the_scale() {
        for (aqi, l) in [(0, Level::Good), (50, Level::Good), (51, Level::Moderate), (100, Level::Moderate), (101, Level::Sensitive), (150, Level::Sensitive), (151, Level::Unhealthy), (200, Level::Unhealthy), (201, Level::VeryUnhealthy), (300, Level::VeryUnhealthy), (301, Level::Hazardous), (999, Level::Hazardous)] {
            assert_eq!(level(aqi), l, "{aqi}");
        }
        assert_eq!(label(Level::Unhealthy), ("Tidak sehat", "Unhealthy"));
    }

    #[test]
    fn the_warning_fires_above_150_once() {
        let mut g = Gate::default();
        assert!(!g.observe(150, 0), "150 is not above 150");
        assert!(g.observe(151, 3600));
        assert!(!g.observe(180, 7200), "already warned");
        assert!(!g.observe(149, 10_800), "still not below the re-arm level");
        assert!(!g.observe(155, 14_400), "so a wobble around 150 does not repeat it");
        assert!(!g.observe(100, 18_000), "re-armed ...");
        assert!(!g.observe(160, 21_600), "... but not twice within 12 hours");
        assert!(g.observe(160, 3600 + MIN_GAP_SECS), "after the gap it warns again (it was armed by the 100)");
    }

    #[test]
    fn a_clock_that_went_back_does_not_block_the_warning_for_ever() {
        let mut g = Gate::default();
        assert!(g.observe(200, 1_000_000));
        assert!(!g.observe(100, 1_000_100));
        assert!(g.observe(200, 500), "now earlier than the last warning: the clock was set back");
    }
}
