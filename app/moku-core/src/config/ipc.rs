section! {
    /// `[ipc]` (named pipe API used by `mokuctl`)
    pub struct Ipc in "ipc" {
        enabled: bool = true,
        ai_cards: bool = true,
        max_notifications: u32 = 50 => super::range(1, 500),
    }
}
