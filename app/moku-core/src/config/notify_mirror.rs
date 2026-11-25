section! {
    /// `[notify_mirror]` Windows notifications mirrored on the island (v0.3, track Q), through
    /// `UserNotificationListener`. Off by default; only the listed apps are shown, nothing is
    /// ever saved (the list lives in memory), and the notification text is never logged.
    pub struct NotifyMirror in "notify_mirror" {
        enabled: bool = false,
        /// Apps whose notifications are mirrored: matched, ignoring case, against the app's
        /// display name or its AppUserModelId, as a whole. Everything else is dropped on arrival.
        apps: Vec<String> = vec!["WhatsApp".into(), "Telegram".into(), "Telegram Desktop".into(), "Discord".into(), "Outlook".into(), "Microsoft Teams".into(), "Teams".into(), "Signal".into()],
        /// A pill for each new mirrored notification (the list in the Notifikasi tab always fills).
        pill: bool = true,
        /// Notifications kept in memory (oldest dropped).
        max_items: u32 = 30 => super::range(5, 200),
    }
}
