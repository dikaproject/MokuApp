section! {
    /// `[timer]`
    pub struct Timer in "timer" {
        enabled: bool = true,
        focus_minutes: u32 = 25 => super::range(1, 180),
        break_minutes: u32 = 5 => super::range(1, 60),
        hold_notifications_in_focus: bool = true,
    }
}
