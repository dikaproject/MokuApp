section! {
    /// `[media]`
    pub struct Media in "media" {
        enabled: bool = true,
        lyrics: bool = true,
        lyrics_in_pill: bool = false,
        /// Word-by-word highlight of the sung line (Musik panel and the lyric pill); the compositor
        /// sweeps it, so it costs no CPU. Off = the whole line is bright.
        lyrics_word_highlight: bool = true,
    }
}
