use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Material {
    /// Dark glass tint (#0B0B14).
    Dark,
    /// Near-opaque black.
    Black,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Animations {
    Full,
    /// Springs replaced by short fades.
    Reduced,
    Off,
}

fn colour(s: &String) -> Result<(), String> {
    crate::color::Rgb::parse(s).map(|_| ()).ok_or_else(|| format!("{s:?} is not a #RRGGBB colour"))
}

section! {
    /// `[appearance]`
    pub struct Appearance in "appearance" {
        material: Material = Material::Dark,
        /// `#RRGGBB`.
        accent: String = "#0A84FF".into() => colour,
        /// Let the music feature tint the accent from the album art.
        follow_album_color: bool = true,
        animations: Animations = Animations::Full,
    }
}

impl Appearance {
    /// The accent as a colour (always valid after loading).
    pub fn accent_rgb(&self) -> crate::color::Rgb {
        crate::color::Rgb::parse(&self.accent).unwrap_or(crate::color::Rgb(0x0A84FF))
    }
}
