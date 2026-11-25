section! {
    /// `[reminders]`
    pub struct Reminders in "reminders" {
        water: bool = false,
        water_interval_min: u32 = 60 => super::range(5, 480),
        water_target: u32 = 8 => super::range(1, 30),
        eye_break: bool = false,
        eye_interval_min: u32 = 20 => super::range(5, 240),
    }
}
