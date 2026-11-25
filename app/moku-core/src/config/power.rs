section! {
    /// `[power]`: the power and audio extras of v0.2.
    pub struct Power in "power" {
        /// Battery outline with a green fill that animates to the current level when AC is
        /// plugged in (off = the plain "Mengisi 48%" pill).
        charging_animation: bool = true,
        /// Bluetooth headset / earbuds pill (name and battery) and the line in the Kontrol tab.
        /// Off = the earbuds service is not started and holds no memory.
        earbuds: bool = true,
        /// Per-app volume mixer in the Kontrol tab.
        mixer: bool = true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_defaults_are_on() {
        let p = Power::default();
        assert!(p.charging_animation && p.earbuds && p.mixer);
        assert_eq!(Power::SECTION, "power");
    }

    #[test]
    fn power_takes_valid_values_and_ignores_bad_ones() {
        let (c, w) = crate::Config::parse("[power]\ncharging_animation = false\nmixer = \"yes\"\n");
        assert!(!c.power.charging_animation);
        assert!(c.power.mixer, "bad type falls back to the default");
        assert!(c.power.earbuds);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].section == "power" && w[0].key == "mixer");
    }
}
