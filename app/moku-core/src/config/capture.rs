section! {
    /// `[capture]`
    pub struct Capture in "capture" {
        enabled: bool = true,
        /// Empty = `Pictures\Moku`.
        save_dir: String = String::new(),
        copy_to_clipboard: bool = true,
    }
}
