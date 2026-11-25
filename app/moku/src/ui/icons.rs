//! Segoe Fluent Icons code points (the same code points exist in Segoe MDL2 Assets, the
//! Windows 10 fallback). Use with [`super::icon`]. Features may use other glyphs as string
//! literals (`"\u{E7C8}"`) — check them in Character Map with "Segoe MDL2 Assets" so they
//! also render on Windows 10.

pub const MUSIC: &str = "\u{EC4F}";
pub const CALENDAR: &str = "\u{E787}";
pub const SLIDERS: &str = "\u{E9E9}";
/// Inbox tray (the Shelf tab, its empty state). Not the envelope: that reads as mail.
pub const SHELF: &str = "\u{EC59}";
pub const TIMER: &str = "\u{E823}";
pub const STOPWATCH: &str = "\u{E916}";
pub const CLIPBOARD: &str = "\u{E77F}";
pub const CAMERA: &str = "\u{E722}";
pub const PHONE: &str = "\u{E8EA}";
pub const PULSE: &str = "\u{E9D9}";
pub const SEARCH: &str = "\u{E721}";
pub const BELL: &str = "\u{EA8F}";
pub const SETTINGS: &str = "\u{E713}";

pub const PLAY: &str = "\u{E768}";
pub const PAUSE: &str = "\u{E769}";
pub const PREV: &str = "\u{E892}";
pub const NEXT: &str = "\u{E893}";
pub const VOLUME: &str = "\u{E767}";
pub const MUTE: &str = "\u{E74F}";
pub const HEADPHONE: &str = "\u{E7F6}";
pub const SPEAKER: &str = "\u{E7F5}";
pub const MIC: &str = "\u{E720}";
pub const MIC_OFF: &str = "\u{EC54}";
pub const WEBCAM: &str = "\u{E8B8}";

pub const WIFI: &str = "\u{E701}";
pub const BLUETOOTH: &str = "\u{E702}";
pub const BRIGHTNESS: &str = "\u{E706}";
pub const MOON: &str = "\u{E708}";
pub const GLOBE: &str = "\u{E774}";
pub const CLOUD: &str = "\u{E753}";
pub const BATTERY: &str = "\u{E83F}";
pub const LIGHTNING: &str = "\u{E945}";
pub const LOCK: &str = "\u{E72E}";
pub const SHIELD: &str = "\u{EA18}";
pub const WARNING: &str = "\u{E7BA}";
pub const INFO: &str = "\u{E946}";
pub const EYE: &str = "\u{E890}";

pub const CHEVRON_DOWN: &str = "\u{E70D}";
pub const CHEVRON_UP: &str = "\u{E70E}";
pub const CHEVRON_LEFT: &str = "\u{E76B}";
pub const CHEVRON_RIGHT: &str = "\u{E76C}";
pub const ADD: &str = "\u{E710}";
pub const CLOSE: &str = "\u{E711}";
pub const CHECK: &str = "\u{E73E}";
pub const PIN: &str = "\u{E718}";
pub const UNPIN: &str = "\u{E77A}";
pub const DELETE: &str = "\u{E74D}";
pub const COPY: &str = "\u{E8C8}";
pub const FOLDER: &str = "\u{E8B7}";
pub const DOCUMENT: &str = "\u{E8A5}";
pub const PHOTO: &str = "\u{E91B}";
pub const VIDEO: &str = "\u{E714}";
pub const DOWNLOAD: &str = "\u{E896}";
pub const UPLOAD: &str = "\u{E898}";
pub const CROP: &str = "\u{E7A8}";
pub const RECORD: &str = "\u{E7C8}";
pub const DESKTOP: &str = "\u{E7F4}";
/// An app window (Tangkap → Jendela; `DESKTOP` is the whole screen).
pub const WINDOW: &str = "\u{E737}";
pub const CODE: &str = "\u{E943}";
pub const MESSAGE: &str = "\u{E8BD}";
pub const LINK: &str = "\u{E71B}";
pub const SYNC: &str = "\u{E895}";

/// "Segoe Fluent Icons" is installed (Windows 11); set once by the drawing code. Without it the
/// icons come from Segoe MDL2 Assets, which has no solid transport glyphs.
pub static FLUENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The solid variant of a transport glyph (play, pause, previous, next) where the font has one
/// (Segoe Fluent Icons: PlaySolid F5B0, PauseSolid F8AE, PreviousSolid F8AC, NextSolid F8AD);
/// the outline glyph otherwise.
pub fn solid(glyph: &str) -> &str {
    if !FLUENT.load(std::sync::atomic::Ordering::Relaxed) {
        return glyph;
    }
    match glyph {
        PLAY => "\u{F5B0}",
        PAUSE => "\u{F8AE}",
        PREV => "\u{F8AC}",
        NEXT => "\u{F8AD}",
        g => g,
    }
}

/// Map a short name (as sent over IPC, e.g. `mokuctl notify --icon download`) to a glyph.
/// Unknown names that are a single character are used as the glyph itself; else `BELL`.
pub fn by_name(name: &str) -> &str {
    match name.to_ascii_lowercase().as_str() {
        "music" => MUSIC,
        "calendar" => CALENDAR,
        "timer" => TIMER,
        "clipboard" => CLIPBOARD,
        "camera" => CAMERA,
        "phone" => PHONE,
        "bell" | "notification" => BELL,
        "settings" => SETTINGS,
        "download" => DOWNLOAD,
        "upload" => UPLOAD,
        "check" | "done" | "success" => CHECK,
        "warning" | "warn" => WARNING,
        "error" | "close" => CLOSE,
        "info" => INFO,
        "code" | "terminal" => CODE,
        "message" | "chat" => MESSAGE,
        "mic" => MIC,
        "lock" => LOCK,
        "shield" => SHIELD,
        "sync" => SYNC,
        "document" | "file" => DOCUMENT,
        "folder" => FOLDER,
        "photo" | "image" => PHOTO,
        "lightning" | "ai" => LIGHTNING,
        _ if name.chars().count() == 1 => name,
        _ => BELL,
    }
}
