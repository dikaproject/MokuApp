//! `#RRGGBB` colours used in the config.

/// 0xRRGGBB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u32);

impl Rgb {
    /// `#RRGGBB` or `RRGGBB` (case-insensitive).
    pub fn parse(s: &str) -> Option<Rgb> {
        let h = s.trim().trim_start_matches('#');
        if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(h, 16).ok().map(Rgb)
    }

    pub fn to_hex(self) -> String {
        format!("#{:06X}", self.0 & 0xFFFFFF)
    }

    pub fn r(self) -> u8 {
        (self.0 >> 16) as u8
    }
    pub fn g(self) -> u8 {
        (self.0 >> 8) as u8
    }
    pub fn b(self) -> u8 {
        self.0 as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_parse() {
        assert_eq!(Rgb::parse("#0A84FF"), Some(Rgb(0x0A84FF)));
        assert_eq!(Rgb::parse("ff453a"), Some(Rgb(0xFF453A)));
        assert_eq!(Rgb::parse("#FFF"), None);
        assert_eq!(Rgb::parse("blue"), None);
        assert_eq!(Rgb(0x0A84FF).to_hex(), "#0A84FF");
    }
}
