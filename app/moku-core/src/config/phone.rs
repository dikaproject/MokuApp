section! {
    /// `[phone]`: "Kirim ke HP" / "Terima dari HP" (`features/phone_send`): a temporary HTTP
    /// server on the LAN while a transfer is open. Nothing listens until the user starts one.
    pub struct Phone in "phone" {
        enabled: bool = true,
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Config;

    #[test]
    fn phone_section_defaults_and_parse() {
        assert!(Config::default().phone.enabled);
        let (c, w) = Config::parse("[phone]\nenabled = false\n");
        assert!(w.is_empty(), "{w:?}");
        assert!(!c.phone.enabled);
        let (c, w) = Config::parse("[phone]\nenabled = \"nope\"\n");
        assert_eq!(w.len(), 1);
        assert!(c.phone.enabled, "a bad value falls back to the default");
    }
}
