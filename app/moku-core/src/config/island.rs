use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IslandStyle {
    /// Floating glass pill 8 DIP below the screen top.
    Dynamic,
    /// Attached to the top edge, like a notch.
    Notch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenOn {
    /// Pointer rests on the pill for `hover_delay_ms` (a click also opens).
    Hover,
    /// Only a click opens the panel.
    Click,
}

/// Where along the top edge of its monitor the island rests (`island.anchor`); `offset_x` then
/// nudges it from there. The island opens from its resting centre, so it never gets closer to a
/// side than half a panel (see `shell::monitors::shift_px`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Anchor {
    Left,
    Center,
    Right,
}

/// What the island does when the pointer heads for the area it covers without aiming at it
/// (`island.move_aside`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MoveAside {
    Off,
    /// Fades to 30 % in place and lets clicks through.
    Fade,
    /// Shifts sideways (away from the pointer) and fades a little; clicks go through.
    Slide,
}

/// Which monitor the island lives on, parsed from `island.monitor` ([`Island::monitor_choice`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonitorChoice {
    /// The primary monitor.
    Primary,
    /// The monitor the pointer is on (moves there after 300 ms, see `shell::monitors::Follow`).
    Cursor,
    /// One island per monitor. Not implemented: behaves like [`MonitorChoice::Primary`].
    All,
    /// One monitor by device name (`\\.\DISPLAY2`); the primary one when it is not connected.
    Named(String),
}

section! {
    /// `[island]`
    pub struct Island in "island" {
        style: IslandStyle = IslandStyle::Dynamic,
        /// Extra scale on top of the monitor DPI.
        scale: f32 = 1.0 => super::range(0.75, 1.30),
        open_on: OpenOn = OpenOn::Hover,
        hover_delay_ms: u32 = 400 => super::range(0, 3000),
        hide_fullscreen: bool = true,
        /// `"primary"`, `"cursor"` (follow the pointer), `"all"` (not implemented yet: primary)
        /// or a device name such as `\\.\DISPLAY2`.
        monitor: String = "primary".into() => monitor_name,
        /// Resting place along the top edge: left, centre or right of the monitor.
        anchor: Anchor = Anchor::Center,
        /// Fine-tune from `anchor`, in DIP (positive = right). Clamped so the island stays on its monitor.
        offset_x: i32 = 0 => super::range(-2000, 2000),
        /// Step out of the way of a pointer that heads for the area under the island.
        move_aside: MoveAside = MoveAside::Off,
        /// Stay hidden until the pointer touches the top edge (or something new arrives).
        hide_until_edge: bool = false,
        /// Process names (`chrome.exe`) of apps in front of which the island hides.
        hide_apps: Vec<String> = vec![],
        /// Apps whose media sessions are ignored: their `SourceAppUserModelId` (`Spotify.exe`) or a
        /// part of it (`spotify`).
        media_ignore_apps: Vec<String> = vec![],
    }
}

fn monitor_name(s: &String) -> Result<(), String> {
    if s.trim().is_empty() { Err("is empty".into()) } else { Ok(()) }
}

impl Island {
    /// `monitor` as a choice (case-insensitive keywords; anything else is a device name).
    pub fn monitor_choice(&self) -> MonitorChoice {
        let m = self.monitor.trim();
        match m.to_ascii_lowercase().as_str() {
            "" | "primary" => MonitorChoice::Primary,
            "cursor" | "follow" => MonitorChoice::Cursor,
            "all" => MonitorChoice::All,
            _ => MonitorChoice::Named(m.to_string()),
        }
    }

    /// Does anything need the pointer position (polling)?
    pub fn watches_pointer(&self) -> bool {
        self.monitor_choice() == MonitorChoice::Cursor || self.move_aside != MoveAside::Off || self.hide_until_edge
    }
}

#[cfg(test)]
mod tests {
    use super::super::Config;
    use super::*;

    #[test]
    fn monitor_values_parse_into_choices() {
        let mk = |s: &str| Island { monitor: s.into(), ..Island::default() }.monitor_choice();
        assert_eq!(mk("primary"), MonitorChoice::Primary);
        assert_eq!(mk("  Primary "), MonitorChoice::Primary);
        assert_eq!(mk("cursor"), MonitorChoice::Cursor);
        assert_eq!(mk("Follow"), MonitorChoice::Cursor);
        assert_eq!(mk("ALL"), MonitorChoice::All);
        assert_eq!(mk(r"\\.\DISPLAY2"), MonitorChoice::Named(r"\\.\DISPLAY2".into()));
    }

    #[test]
    fn polite_fields_default_to_the_old_behaviour() {
        let i = Island::default();
        assert_eq!((i.monitor_choice(), i.anchor, i.offset_x, i.move_aside, i.hide_until_edge), (MonitorChoice::Primary, Anchor::Center, 0, MoveAside::Off, false));
        assert!(i.hide_apps.is_empty() && i.media_ignore_apps.is_empty());
        assert!(!i.watches_pointer(), "nothing polls the pointer by default");
    }

    #[test]
    fn polite_fields_round_trip_and_bad_values_fall_back() {
        let text = "[island]\nmonitor = \"\\\\\\\\.\\\\DISPLAY2\"\nanchor = \"right\"\noffset_x = -40\nmove_aside = \"slide\"\nhide_until_edge = true\nhide_apps = [\"chrome.exe\", \"Code.exe\"]\nmedia_ignore_apps = [\"Spotify.exe\"]\n";
        let (c, w) = Config::parse(text);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.island.monitor, r"\\.\DISPLAY2");
        assert_eq!((c.island.anchor, c.island.offset_x, c.island.move_aside), (Anchor::Right, -40, MoveAside::Slide));
        assert!(c.island.hide_until_edge && c.island.watches_pointer());
        assert_eq!(c.island.hide_apps, vec!["chrome.exe", "Code.exe"]);
        assert_eq!(c.island.media_ignore_apps, vec!["Spotify.exe"]);
        // Written and read again: the same.
        let (back, w) = Config::parse(&c.to_toml());
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(back, c);

        let (c, w) = Config::parse("[island]\nmonitor = \"\"\nanchor = \"middle\"\noffset_x = 99999\nmove_aside = 3\n");
        assert_eq!(w.len(), 4, "{w:?}");
        assert_eq!(c.island, Island::default());
    }
}
