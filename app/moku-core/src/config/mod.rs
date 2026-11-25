//! `%APPDATA%\Moku\config.toml`: the whole settings schema.
//!
//! Every field has a default. Unknown keys are ignored. A value of the wrong type or out of
//! range falls back to its default and produces a [`Warning`] (never an error), so a hand-edited
//! or half-written file can never stop the island. A missing file means "all defaults".
//!
//! One struct per TOML section, one file per section (`general.rs`, `island.rs`, ...). Each is
//! declared with the `section!` macro below, which generates the struct, its `Default`, and a
//! per-field tolerant loader with optional validation.

use std::fmt;
use std::io;
use std::path::Path;

use serde::Serialize;

/// Declares one config section. Syntax per field: `name: Type = default => check,` where the
/// optional `check` is a `Fn(&Type) -> Result<(), String>` (see [`range`], [`opt_range`]).
macro_rules! section {
    (
        $(#[$meta:meta])*
        pub struct $name:ident in $key:literal {
            $(
                $(#[$fmeta:meta])*
                $field:ident : $ty:ty = $default:expr $(=> $check:expr)?
            ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, serde::Serialize)]
        pub struct $name {
            $( $(#[$fmeta])* pub $field: $ty, )*
        }

        impl Default for $name {
            fn default() -> Self {
                Self { $( $field: $default, )* }
            }
        }

        impl $name {
            /// TOML section name.
            pub const SECTION: &'static str = $key;

            #[allow(unused_mut, unused_variables)]
            pub(crate) fn from_toml(v: Option<&toml::Value>, warnings: &mut Vec<$crate::config::Warning>) -> Self {
                let mut out = Self::default();
                let Some(v) = v else { return out };
                let Some(t) = v.as_table() else {
                    warnings.push($crate::config::Warning::new($key, "", "is not a table; using defaults"));
                    return out;
                };
                $(
                    if let Some(fv) = t.get(stringify!($field)) {
                        match fv.clone().try_into::<$ty>() {
                            Ok(x) => {
                                let ok: Result<(), String> = Ok(());
                                $( let ok = ok.and_then(|_| ($check)(&x)); )?
                                match ok {
                                    Ok(()) => out.$field = x,
                                    Err(m) => warnings.push($crate::config::Warning::new($key, stringify!($field), &format!("{m}; using default"))),
                                }
                            }
                            Err(e) => warnings.push($crate::config::Warning::new(
                                $key,
                                stringify!($field),
                                &format!("invalid value ({}); using default", e.to_string().trim()),
                            )),
                        }
                    }
                )*
                out
            }
        }
    };
}

mod agents;
mod ai;
mod appearance;
mod bmkg;
mod calendar;
mod capture;
mod claude;
mod clipboard;
mod control;
mod downloads;
mod general;
mod glance;
mod ibadah;
mod ipc;
mod island;
mod launcher;
mod media;
mod monitor;
mod notify_mirror;
mod ocr;
mod onboarding;
mod phone;
mod power;
mod prayer;
mod privacy;
mod reminders;
mod shelf;
mod system;
mod timer;
mod today;
mod update;
mod volume_osd;

pub use agents::Agents;
pub use ai::Ai;
pub use appearance::{Animations, Appearance, Material};
pub use bmkg::Bmkg;
pub use calendar::{Calendar, check_url, mask_url};
pub use capture::Capture;
pub use claude::Claude;
pub use clipboard::Clipboard;
pub use control::Control;
pub use downloads::Downloads;
pub use general::{General, Language};
pub use glance::Glance;
pub use ibadah::{AdzanSound, CountdownPill, Ibadah, MAX_PATH_CHARS as MAX_AUDIO_PATH_CHARS};
pub use ipc::Ipc;
pub use island::{Anchor, Island, IslandStyle, MonitorChoice, MoveAside, OpenOn};
pub use launcher::Launcher;
pub use media::Media;
pub use monitor::Monitor;
pub use notify_mirror::NotifyMirror;
pub use ocr::Ocr;
pub use onboarding::Onboarding;
pub use phone::Phone;
pub use power::Power;
pub use prayer::{Prayer, PrayerMethod};
pub use privacy::Privacy;
pub use reminders::Reminders;
pub use shelf::Shelf;
pub use system::System;
pub use timer::Timer;
pub use today::{Today, Units};
pub use update::Update;
pub use volume_osd::VolumeOsd;

/// A value in the file that could not be used. The default was used instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub section: String,
    pub key: String,
    pub message: String,
}

impl Warning {
    pub fn new(section: &str, key: &str, message: &str) -> Self {
        Warning { section: section.into(), key: key.into(), message: message.into() }
    }
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.section.is_empty(), self.key.is_empty()) {
            (true, _) => write!(f, "config: {}", self.message),
            (false, true) => write!(f, "config [{}]: {}", self.section, self.message),
            (false, false) => write!(f, "config {}.{}: {}", self.section, self.key, self.message),
        }
    }
}

/// Validation helper: inclusive numeric range.
pub(crate) fn range<T: PartialOrd + fmt::Display + Copy>(lo: T, hi: T) -> impl Fn(&T) -> Result<(), String> {
    move |v| if *v >= lo && *v <= hi { Ok(()) } else { Err(format!("{v} is outside {lo}..{hi}")) }
}

/// Validation helper: inclusive range for an optional value (`None` is always fine).
pub(crate) fn opt_range<T: PartialOrd + fmt::Display + Copy>(lo: T, hi: T) -> impl Fn(&Option<T>) -> Result<(), String> {
    move |v| match v {
        Some(x) => range(lo, hi)(x),
        None => Ok(()),
    }
}

/// The whole configuration. `Config::default()` is the shipped default.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Config {
    pub general: General,
    pub island: Island,
    pub appearance: Appearance,
    pub media: Media,
    pub volume_osd: VolumeOsd,
    pub control: Control,
    pub today: Today,
    pub timer: Timer,
    pub clipboard: Clipboard,
    pub capture: Capture,
    pub shelf: Shelf,
    pub monitor: Monitor,
    pub system: System,
    pub reminders: Reminders,
    pub prayer: Prayer,
    pub ipc: Ipc,
    pub privacy: Privacy,
    pub onboarding: Onboarding,
    pub claude: Claude,
    pub calendar: Calendar,
    pub downloads: Downloads,
    pub update: Update,
    pub power: Power,
    pub launcher: Launcher,
    pub ibadah: Ibadah,
    pub agents: Agents,
    pub notify_mirror: NotifyMirror,
    pub bmkg: Bmkg,
    pub glance: Glance,
    pub ocr: Ocr,
    pub ai: Ai,
    pub phone: Phone,
}

impl Config {
    /// Parse TOML text tolerantly. Syntax errors give all defaults plus one warning.
    pub fn parse(text: &str) -> (Config, Vec<Warning>) {
        let mut w = vec![];
        let root: toml::Table = match text.parse() {
            Ok(t) => t,
            Err(e) => {
                w.push(Warning::new("", "", &format!("cannot parse config.toml ({}); using defaults", e.to_string().trim())));
                return (Config::default(), w);
            }
        };
        let g = |k: &str| root.get(k);
        let c = Config {
            general: General::from_toml(g(General::SECTION), &mut w),
            island: Island::from_toml(g(Island::SECTION), &mut w),
            appearance: Appearance::from_toml(g(Appearance::SECTION), &mut w),
            media: Media::from_toml(g(Media::SECTION), &mut w),
            volume_osd: VolumeOsd::from_toml(g(VolumeOsd::SECTION), &mut w),
            control: Control::from_toml(g(Control::SECTION), &mut w),
            today: Today::from_toml(g(Today::SECTION), &mut w),
            timer: Timer::from_toml(g(Timer::SECTION), &mut w),
            clipboard: Clipboard::from_toml(g(Clipboard::SECTION), &mut w),
            capture: Capture::from_toml(g(Capture::SECTION), &mut w),
            shelf: Shelf::from_toml(g(Shelf::SECTION), &mut w),
            monitor: Monitor::from_toml(g(Monitor::SECTION), &mut w),
            system: System::from_toml(g(System::SECTION), &mut w),
            reminders: Reminders::from_toml(g(Reminders::SECTION), &mut w),
            prayer: Prayer::from_toml(g(Prayer::SECTION), &mut w),
            ipc: Ipc::from_toml(g(Ipc::SECTION), &mut w),
            privacy: Privacy::from_toml(g(Privacy::SECTION), &mut w),
            onboarding: Onboarding::from_toml(g(Onboarding::SECTION), &mut w),
            claude: Claude::from_toml(g(Claude::SECTION), &mut w),
            calendar: Calendar::from_toml(g(Calendar::SECTION), &mut w),
            downloads: Downloads::from_toml(g(Downloads::SECTION), &mut w),
            update: Update::from_toml(g(Update::SECTION), &mut w),
            power: Power::from_toml(g(Power::SECTION), &mut w),
            launcher: Launcher::from_toml(g(Launcher::SECTION), &mut w),
            ibadah: Ibadah::from_toml(g(Ibadah::SECTION), &mut w),
            agents: Agents::from_toml(g(Agents::SECTION), &mut w),
            notify_mirror: NotifyMirror::from_toml(g(NotifyMirror::SECTION), &mut w),
            bmkg: Bmkg::from_toml(g(Bmkg::SECTION), &mut w),
            glance: Glance::from_toml(g(Glance::SECTION), &mut w),
            ocr: Ocr::from_toml(g(Ocr::SECTION), &mut w),
            ai: Ai::from_toml(g(Ai::SECTION), &mut w),
            phone: Phone::from_toml(g(Phone::SECTION), &mut w),
        };
        (c, w)
    }

    /// Load from a file. Missing file → defaults with no warnings; unreadable or not TOML →
    /// defaults + warning.
    pub fn load_from(path: &Path) -> (Config, Vec<Warning>) {
        match Config::try_load_from(path) {
            Ok(r) => r,
            Err(w) => (Config::default(), vec![w]),
        }
    }

    /// Like [`Config::load_from`], but a file that exists and cannot be read or parsed as a
    /// whole is an `Err` (so a running app can keep its last good config instead of resetting
    /// everything while the user is mid-edit). Missing file → `Ok(defaults)`; bad individual
    /// values → `Ok` with defaults for those fields + warnings.
    pub fn try_load_from(path: &Path) -> Result<(Config, Vec<Warning>), Warning> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                // Tolerate a UTF-8 BOM (Notepad, PowerShell 5 `Out-File`).
                let text = text.trim_start_matches('\u{feff}');
                if let Err(e) = text.parse::<toml::Table>() {
                    return Err(Warning::new("", "", &format!("cannot parse config.toml ({}); using defaults", e.to_string().trim())));
                }
                Ok(Config::parse(text))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok((Config::default(), vec![])),
            Err(e) => Err(Warning::new("", "", &format!("cannot read {} ({e}); using defaults", path.display()))),
        }
    }

    /// Load `%APPDATA%\Moku\config.toml` (see [`crate::paths::config_file`]).
    pub fn load_or_default() -> (Config, Vec<Warning>) {
        Config::load_from(&crate::paths::config_file())
    }

    /// The TOML text for this config (all sections, all fields).
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// Save to `%APPDATA%\Moku\config.toml`, creating the folder if needed.
    pub fn save(&self) -> io::Result<()> {
        self.save_to(&crate::paths::config_file())
    }

    /// Save atomically: write a temp file next to `path`, then rename over it, so a reader
    /// (the island's watcher) never sees a half-written file.
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, self.to_toml())?;
        std::fs::rename(&tmp, path)
    }

    /// Read-modify-write of the file at `path`: re-read it (so edits another process — the
    /// settings app, a hand edit — saved since `fallback` was loaded are kept), apply `change`,
    /// and save atomically. A file that cannot be read or parsed as a whole right now falls back
    /// to `fallback`. The write is skipped when nothing changed. Returns the config now on disk.
    pub fn update_file(path: &Path, fallback: &Config, change: impl FnOnce(&mut Config)) -> io::Result<Config> {
        let disk = match Config::try_load_from(path) {
            Ok((c, _)) => c,
            Err(_) => fallback.clone(),
        };
        let mut cfg = disk.clone();
        change(&mut cfg);
        if cfg != disk || !path.exists() {
            cfg.save_to(path)?;
        }
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("moku-core-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn config_defaults_roundtrip() {
        let c = Config::default();
        let text = c.to_toml();
        let (back, warnings) = Config::parse(&text);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(back, c);
        // Spot-check the documented defaults.
        assert_eq!(c.general.hotkey_toggle, "Alt+M");
        assert_eq!(c.island.scale, 1.0);
        assert_eq!(c.appearance.accent, "#0A84FF");
        assert_eq!(c.clipboard.ignore_apps.len(), 3);
        assert!(!c.monitor.enabled);
        assert_eq!(c.prayer.latitude, -6.2088);
        assert_eq!(c.today.latitude, None);
    }

    #[test]
    fn config_invalid_values_fall_back() {
        let (c, w) = Config::parse("[island]\nscale = 9\n[media]\nenabled = \"yes\"\nlyrics = false\n");
        assert_eq!(w.len(), 2, "{w:?}");
        assert_eq!(c.island.scale, 1.0);
        assert!(c.media.enabled);
        // Valid neighbours are still taken.
        assert!(!c.media.lyrics);
        assert!(w.iter().any(|x| x.section == "island" && x.key == "scale"));
        assert!(w.iter().any(|x| x.section == "media" && x.key == "enabled"));
    }

    #[test]
    fn config_invalid_enum_hotkey_and_colour_fall_back() {
        let (c, w) = Config::parse(
            "[island]\nstyle = \"bubble\"\n[general]\nhotkey_toggle = \"Alt+Nope\"\n[appearance]\naccent = \"blue\"\n",
        );
        assert_eq!(w.len(), 3, "{w:?}");
        assert_eq!(c.island.style, IslandStyle::Dynamic);
        assert_eq!(c.general.hotkey_toggle, "Alt+M");
        assert_eq!(c.appearance.accent, "#0A84FF");
    }

    #[test]
    fn config_unknown_keys_ignored_and_int_for_float() {
        let (c, w) = Config::parse("[island]\nscale = 1\nwhatever = 3\n[nonsense]\nx = 1\n");
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.island.scale, 1.0);
    }

    #[test]
    fn config_syntax_error_gives_defaults() {
        let (c, w) = Config::parse("[island\nscale = ");
        assert_eq!(c, Config::default());
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn config_try_load_distinguishes_broken_file() {
        let d = temp_dir("broken");
        let p = d.join("config.toml");
        // Missing → defaults (Ok).
        assert_eq!(Config::try_load_from(&p).unwrap().0, Config::default());
        // Half-written / syntax error → Err, so a running island can keep its last good config.
        std::fs::write(&p, "[island\nscale = ").unwrap();
        assert!(Config::try_load_from(&p).is_err());
        // Field-level problems are not file-level: Ok with warnings.
        std::fs::write(&p, "[island]\nscale = 9\n").unwrap();
        let (c, w) = Config::try_load_from(&p).unwrap();
        assert_eq!((c.island.scale, w.len()), (1.0, 1));
    }

    #[test]
    fn update_file_keeps_edits_made_by_another_writer() {
        let d = temp_dir("update");
        let p = d.join("config.toml");
        // The island loaded the defaults; then the settings app saved a different scale.
        let stale = Config::default();
        let mut other = Config::default();
        other.island.scale = 1.25;
        other.save_to(&p).unwrap();
        // A feature toggles its own key through the read-modify-write helper.
        let now = Config::update_file(&p, &stale, |c| c.timer.hold_notifications_in_focus = false).unwrap();
        let (back, _) = Config::load_from(&p);
        assert_eq!(back, now);
        assert_eq!(back.island.scale, 1.25, "the other writer's edit survives");
        assert!(!back.timer.hold_notifications_in_focus);
        // A broken file: build on the fallback (the in-memory copy) instead of the defaults.
        std::fs::write(&p, "[island\nscale = ").unwrap();
        let mut mem = Config::default();
        mem.island.scale = 1.2;
        let now = Config::update_file(&p, &mem, |c| c.timer.focus_minutes = 50).unwrap();
        assert_eq!((now.island.scale, now.timer.focus_minutes), (1.2, 50));
        assert_eq!(Config::load_from(&p).0, now);
    }

    #[test]
    fn config_missing_file_gives_defaults() {
        let d = temp_dir("missing");
        let (c, w) = Config::load_from(&d.join("config.toml"));
        assert_eq!(c, Config::default());
        assert!(w.is_empty());
    }

    #[test]
    fn config_save_and_load_file() {
        let d = temp_dir("save");
        let p = d.join("sub").join("config.toml");
        let mut c = Config::default();
        c.today.latitude = Some(-6.2);
        c.today.longitude = Some(106.8);
        c.island.scale = 1.25;
        c.save_to(&p).unwrap();
        let (back, w) = Config::load_from(&p);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(back, c);
        // BOM tolerated.
        let text = format!("\u{feff}{}", std::fs::read_to_string(&p).unwrap());
        std::fs::write(&p, text).unwrap();
        assert_eq!(Config::load_from(&p).0, c);
    }
}
