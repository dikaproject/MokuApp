section! {
    /// `[clipboard]`
    pub struct Clipboard in "clipboard" {
        enabled: bool = true,
        keep_days: u32 = 7 => super::range(1, 365),
        max_items: u32 = 50 => super::range(5, 500),
        /// Executable names whose clipboard content is never stored.
        ignore_apps: Vec<String> = vec!["KeePassXC.exe".into(), "1Password.exe".into(), "Bitwarden.exe".into()],
        /// Buttons on a clip that is a colour, a link, a sum, JSON or a phone number (QR, open,
        /// the result, pretty-print, WhatsApp, other colour notations).
        smart_actions: bool = true,
        /// Also a short pill when a clip matches strongly (a link, a colour, a clear sum ...).
        smart_pill: bool = true,
        /// One-time codes (OTP) are kept in memory only, never written to disk, and dropped from
        /// the history after 60 seconds.
        otp_guard: bool = true,
    }
}
