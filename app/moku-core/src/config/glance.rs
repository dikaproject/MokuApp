/// A currency code as Frankfurter takes it: three letters.
fn currency_ok(s: &String) -> Result<(), String> {
    if s.len() == 3 && s.bytes().all(|b| b.is_ascii_uppercase()) { Ok(()) } else { Err("not a 3-letter currency code".into()) }
}

section! {
    /// `[glance]`: small facts in the Hari ini tab (v0.3): an exchange rate and the air quality.
    pub struct Glance in "glance" {
        /// Exchange rate chip from Frankfurter (ECB rates), every 6 hours while on.
        rate: bool = false,
        /// What one unit of `base` is worth in `quote`.
        base: String = "USD".into() => currency_ok,
        quote: String = "IDR".into() => currency_ok,
        /// Air quality chip (US AQI) from Open-Meteo for the Hari ini city, every hour while on.
        aqi: bool = false,
        /// A pill when the AQI goes above 150 ("Tidak sehat").
        aqi_alert: bool = true,
    }
}

#[cfg(test)]
mod tests {
    use crate::Config;

    #[test]
    fn glance_defaults_and_round_trip() {
        let c = Config::default();
        assert!(!c.glance.rate && !c.glance.aqi && c.glance.aqi_alert);
        assert_eq!((c.glance.base.as_str(), c.glance.quote.as_str()), ("USD", "IDR"));
        let mut c = c;
        c.glance.rate = true;
        c.glance.aqi = true;
        c.glance.base = "EUR".into();
        c.glance.quote = "USD".into();
        let (back, w) = Config::parse(&c.to_toml());
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(back, c);
    }

    #[test]
    fn currency_codes_are_checked() {
        let (c, w) = Config::parse("[glance]\nbase = \"usd\"\nquote = \"RUPIAH\"\nrate = true\n");
        assert_eq!((c.glance.base.as_str(), c.glance.quote.as_str()), ("USD", "IDR"));
        assert!(c.glance.rate);
        assert_eq!(w.len(), 2, "{w:?}");
    }
}
