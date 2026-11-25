section! {
    /// `[bmkg]`: alerts from BMKG, Indonesia's meteorological and geophysical agency (v0.3).
    pub struct Bmkg in "bmkg" {
        /// Earthquakes (`autogempa.json` and `gempadirasakan.json`, every 2 minutes while on).
        /// On by default for Indonesian users, off elsewhere.
        quake: bool = crate::locale::default_on_for_indonesia(),
        /// A quake closer than this to the city of the Hari ini tab raises a pill.
        radius_km: u32 = 300 => super::range(50, 3000),
        /// A quake of at least this magnitude raises a pill anywhere in Indonesia.
        big_magnitude: f64 = 6.0 => super::range(4.0, 9.0),
        /// Weather warnings (nowcast RSS for your province, every 10 minutes while on).
        nowcast: bool = false,
        /// The province the warnings are for. Empty = worked out from the city.
        province: String = String::new() => province_ok,
    }
}

fn province_ok(s: &String) -> Result<(), String> {
    if s.chars().count() > 60 || s.chars().any(char::is_control) { Err("not a province name".into()) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use crate::Config;

    #[test]
    fn bmkg_defaults_and_round_trip() {
        let c = Config::default();
        assert_eq!((c.bmkg.radius_km, c.bmkg.big_magnitude), (300, 6.0));
        assert!(!c.bmkg.nowcast && c.bmkg.province.is_empty());
        let mut c = c;
        c.bmkg.quake = true;
        c.bmkg.nowcast = true;
        c.bmkg.radius_km = 150;
        c.bmkg.big_magnitude = 5.5;
        c.bmkg.province = "Jawa Barat".into();
        let (back, w) = Config::parse(&c.to_toml());
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(back, c);
    }

    #[test]
    fn bmkg_bad_values_fall_back() {
        let (c, w) = Config::parse(r#"
[bmkg]
radius_km = 5
big_magnitude = 12.0
nowcast = true
province = "a\u000Ab"
"#);
        assert_eq!((c.bmkg.radius_km, c.bmkg.big_magnitude), (300, 6.0));
        assert!(c.bmkg.nowcast, "valid neighbours are kept: {w:?}");
        assert_eq!(w.len(), 3, "{w:?}");
    }
}
