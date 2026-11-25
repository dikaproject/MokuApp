section! {
    /// `[update]`
    pub struct Update in "update" {
        /// Look for a newer release on GitHub at most once a day (`features/update`). Off by
        /// itself while `moku_core::release::REPO` is still the placeholder.
        check: bool = true,
    }
}
