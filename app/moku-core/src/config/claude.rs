section! {
    /// `[claude]` Claude Code live status (`mokuctl claude-hook`, IPC `session`). Needs `[ipc]`.
    pub struct Claude in "claude" {
        /// Session pills ("Bekerja…", "Butuh input", "Selesai") and the list in Notifikasi.
        enabled: bool = true,
        /// The short "✓ Selesai · 3m 12s" pill when a turn ends.
        done_pill: bool = true,
        /// A session with no event for this long is dropped (Claude Code closed without `SessionEnd`).
        stale_minutes: u32 = 30 => super::range(5, 1440),
    }
}
