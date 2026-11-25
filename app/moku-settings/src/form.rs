//! The binding layer: [`Form`] (the struct declared in `ui/form.slint`, one field per control)
//! to and from [`moku_core::Config`]. No window is needed, so it is fully unit-tested.
//!
//! `from_config` fills the form from a loaded config. `apply_to` writes the form's values onto a
//! config (usually a fresh read of `config.toml`, so keys the UI does not manage survive) and
//! reports values it could not use; those leave the config field unchanged.

use moku_core::Config;
use moku_core::color::Rgb;
use moku_core::config::{Animations, IslandStyle, Language, Material, OpenOn, Units};
use moku_core::hotkey::{self, Hotkey, Slot};
use slint::SharedString;

use crate::{Form, Problems};

/// Split a comma / semicolon / newline separated list ("KeePassXC.exe, 1Password.exe").
pub fn parse_list(s: &str) -> Vec<String> {
    s.split([',', ';', '\n']).map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()
}

/// The inverse of [`parse_list`] for display.
pub fn join_list(v: &[String]) -> String {
    v.join(", ")
}

/// "Alt+M" -> ["Alt", "M"] for the keycap chips.
pub fn hotkey_parts(s: &str) -> Vec<String> {
    s.split('+').map(str::trim).filter(|p| !p.is_empty()).map(String::from).collect()
}

fn clamp(v: i32, lo: u32, hi: u32) -> u32 {
    v.clamp(lo as i32, hi as i32) as u32
}

fn pick<T: Copy>(idx: i32, all: &[T], keep: T) -> T {
    usize::try_from(idx).ok().and_then(|i| all.get(i).copied()).unwrap_or(keep)
}

fn index_of<T: PartialEq>(all: &[T], v: &T) -> i32 {
    all.iter().position(|x| x == v).unwrap_or(0) as i32
}

const LANGS: [Language; 2] = [Language::Id, Language::En];
const STYLES: [IslandStyle; 2] = [IslandStyle::Dynamic, IslandStyle::Notch];
const OPEN_ON: [OpenOn; 2] = [OpenOn::Hover, OpenOn::Click];
const MATERIALS: [Material; 2] = [Material::Dark, Material::Black];
const ANIMATIONS: [Animations; 3] = [Animations::Full, Animations::Reduced, Animations::Off];
const UNITS: [Units; 2] = [Units::Metric, Units::Imperial];

fn ss(s: impl AsRef<str>) -> SharedString {
    SharedString::from(s.as_ref())
}

impl Form {
    pub fn from_config(c: &Config) -> Form {
        Form {
            general_language: index_of(&LANGS, &c.general.language),
            general_autostart: c.general.autostart,
            general_hotkey_toggle: ss(&c.general.hotkey_toggle),
            general_hotkey_quit: ss(&c.general.hotkey_quit),

            island_style: index_of(&STYLES, &c.island.style),
            island_scale: (c.island.scale * 100.0).round() as i32,
            island_open_on: index_of(&OPEN_ON, &c.island.open_on),
            island_hover_delay: c.island.hover_delay_ms as i32,
            island_hide_fullscreen: c.island.hide_fullscreen,

            appearance_material: index_of(&MATERIALS, &c.appearance.material),
            appearance_accent: ss(c.appearance.accent_rgb().to_hex()),
            appearance_follow_album: c.appearance.follow_album_color,
            appearance_animations: index_of(&ANIMATIONS, &c.appearance.animations),

            media_enabled: c.media.enabled,
            media_lyrics: c.media.lyrics,
            media_lyrics_in_pill: c.media.lyrics_in_pill,
            volume_osd_enabled: c.volume_osd.enabled,
            control_enabled: c.control.enabled,
            today_enabled: c.today.enabled,
            today_weather: c.today.weather,
            today_city: ss(&c.today.city),
            today_units: index_of(&UNITS, &c.today.units),
            timer_enabled: c.timer.enabled,
            timer_focus: c.timer.focus_minutes as i32,
            timer_break: c.timer.break_minutes as i32,
            timer_hold: c.timer.hold_notifications_in_focus,
            clipboard_enabled: c.clipboard.enabled,
            clipboard_keep_days: c.clipboard.keep_days as i32,
            clipboard_max_items: c.clipboard.max_items as i32,
            clipboard_ignore: ss(join_list(&c.clipboard.ignore_apps)),
            capture_enabled: c.capture.enabled,
            capture_save_dir: ss(&c.capture.save_dir),
            capture_copy: c.capture.copy_to_clipboard,
            shelf_enabled: c.shelf.enabled,
            monitor_enabled: c.monitor.enabled,

            system_battery: c.system.battery,
            system_caps_lock: c.system.caps_lock,
            system_network: c.system.network,
            system_privacy_dots: c.system.privacy_dots,
            system_meeting_mode: c.system.meeting_mode,

            reminders_water: c.reminders.water,
            reminders_water_interval: c.reminders.water_interval_min as i32,
            reminders_water_target: c.reminders.water_target as i32,
            reminders_eye: c.reminders.eye_break,
            reminders_eye_interval: c.reminders.eye_interval_min as i32,
            prayer_enabled: c.prayer.enabled,
            prayer_city: ss(&c.prayer.city),
            prayer_lat: ss(c.prayer.latitude.to_string()),
            prayer_lon: ss(c.prayer.longitude.to_string()),
            prayer_minutes_before: c.prayer.minutes_before as i32,

            ipc_enabled: c.ipc.enabled,
            ipc_ai_cards: c.ipc.ai_cards,
            ipc_max_notifications: c.ipc.max_notifications as i32,
            privacy_hide_body: c.privacy.hide_notification_body_in_meetings,
        }
    }

    /// Write the form onto `cfg`. Text that is not valid (hotkey, colour, coordinates) is
    /// reported in the returned [`Problems`] and the config keeps its current value for it.
    pub fn apply_to(&self, cfg: &mut Config) -> Problems {
        let mut p = Problems::default();

        // --- general
        cfg.general.language = pick(self.general_language, &LANGS, cfg.general.language);
        cfg.general.autostart = self.general_autostart;
        let toggle = Hotkey::parse(self.general_hotkey_toggle.trim());
        let quit = Hotkey::parse(self.general_hotkey_quit.trim());
        match (&toggle, &quit) {
            (Err(e), _) => p.hotkey_toggle = ss(e),
            (_, Err(e)) => p.hotkey_quit = ss(e),
            (Ok(a), Ok(b)) if a == b => p.hotkey_quit = ss("Sama dengan tombol buka/tutup"),
            _ => {}
        }
        // Neither may be a key another feature (launcher, OCR, Tanya AI) already uses.
        if toggle.is_ok()
            && p.hotkey_toggle.is_empty()
            && let Some(other) = hotkey::clash(cfg, Slot::Panel, self.general_hotkey_toggle.trim())
        {
            p.hotkey_toggle = ss(&hotkey::clash_message(other));
        }
        if quit.is_ok()
            && p.hotkey_quit.is_empty()
            && let Some(other) = hotkey::clash(cfg, Slot::Quit, self.general_hotkey_quit.trim())
        {
            p.hotkey_quit = ss(&hotkey::clash_message(other));
        }
        if toggle.is_ok() && p.hotkey_toggle.is_empty() {
            cfg.general.hotkey_toggle = self.general_hotkey_toggle.trim().to_string();
        }
        if quit.is_ok() && p.hotkey_quit.is_empty() {
            cfg.general.hotkey_quit = self.general_hotkey_quit.trim().to_string();
        }

        // --- island
        cfg.island.style = pick(self.island_style, &STYLES, cfg.island.style);
        cfg.island.scale = self.island_scale.clamp(75, 130) as f32 / 100.0;
        cfg.island.open_on = pick(self.island_open_on, &OPEN_ON, cfg.island.open_on);
        cfg.island.hover_delay_ms = clamp(self.island_hover_delay, 0, 3000);
        cfg.island.hide_fullscreen = self.island_hide_fullscreen;

        // --- appearance
        cfg.appearance.material = pick(self.appearance_material, &MATERIALS, cfg.appearance.material);
        match Rgb::parse(&self.appearance_accent) {
            Some(rgb) => cfg.appearance.accent = rgb.to_hex(),
            None => p.accent = ss("Pakai format #RRGGBB, mis. #0A84FF"),
        }
        cfg.appearance.follow_album_color = self.appearance_follow_album;
        cfg.appearance.animations = pick(self.appearance_animations, &ANIMATIONS, cfg.appearance.animations);

        // --- features
        cfg.media.enabled = self.media_enabled;
        cfg.media.lyrics = self.media_lyrics;
        cfg.media.lyrics_in_pill = self.media_lyrics_in_pill;
        cfg.volume_osd.enabled = self.volume_osd_enabled;
        cfg.control.enabled = self.control_enabled;
        cfg.today.enabled = self.today_enabled;
        cfg.today.weather = self.today_weather;
        let city = self.today_city.trim();
        // An empty box means "still typing": keep the old city. A new city invalidates the
        // coordinates the island geocoded for the old one, so it looks the new one up.
        if !city.is_empty() && city != cfg.today.city {
            cfg.today.city = city.to_string();
            cfg.today.latitude = None;
            cfg.today.longitude = None;
        }
        cfg.today.units = pick(self.today_units, &UNITS, cfg.today.units);
        cfg.timer.enabled = self.timer_enabled;
        cfg.timer.focus_minutes = clamp(self.timer_focus, 1, 180);
        cfg.timer.break_minutes = clamp(self.timer_break, 1, 60);
        cfg.timer.hold_notifications_in_focus = self.timer_hold;
        cfg.clipboard.enabled = self.clipboard_enabled;
        cfg.clipboard.keep_days = clamp(self.clipboard_keep_days, 1, 365);
        cfg.clipboard.max_items = clamp(self.clipboard_max_items, 5, 500);
        cfg.clipboard.ignore_apps = parse_list(&self.clipboard_ignore);
        cfg.capture.enabled = self.capture_enabled;
        cfg.capture.save_dir = self.capture_save_dir.trim().to_string();
        cfg.capture.copy_to_clipboard = self.capture_copy;
        cfg.shelf.enabled = self.shelf_enabled;
        cfg.monitor.enabled = self.monitor_enabled;

        // --- system pills
        cfg.system.battery = self.system_battery;
        cfg.system.caps_lock = self.system_caps_lock;
        cfg.system.network = self.system_network;
        cfg.system.privacy_dots = self.system_privacy_dots;
        cfg.system.meeting_mode = self.system_meeting_mode;

        // --- reminders + prayer
        cfg.reminders.water = self.reminders_water;
        cfg.reminders.water_interval_min = clamp(self.reminders_water_interval, 5, 480);
        cfg.reminders.water_target = clamp(self.reminders_water_target, 1, 30);
        cfg.reminders.eye_break = self.reminders_eye;
        cfg.reminders.eye_interval_min = clamp(self.reminders_eye_interval, 5, 240);
        cfg.prayer.enabled = self.prayer_enabled;
        let pcity = self.prayer_city.trim();
        if !pcity.is_empty() {
            cfg.prayer.city = pcity.to_string();
        }
        match parse_coord(&self.prayer_lat, 90.0) {
            Ok(v) => cfg.prayer.latitude = v,
            Err(m) => p.prayer_lat = ss(m),
        }
        match parse_coord(&self.prayer_lon, 180.0) {
            Ok(v) => cfg.prayer.longitude = v,
            Err(m) => p.prayer_lon = ss(m),
        }
        cfg.prayer.minutes_before = clamp(self.prayer_minutes_before, 0, 120);

        // --- ipc + privacy
        cfg.ipc.enabled = self.ipc_enabled;
        cfg.ipc.ai_cards = self.ipc_ai_cards;
        cfg.ipc.max_notifications = clamp(self.ipc_max_notifications, 1, 500);
        cfg.privacy.hide_notification_body_in_meetings = self.privacy_hide_body;
        p
    }
}

/// A latitude / longitude typed by hand ("-6.2088", "106,8456").
fn parse_coord(s: &str, limit: f64) -> Result<f64, String> {
    let v: f64 = s.trim().replace(',', ".").parse().map_err(|_| "Bukan angka".to_string())?;
    if !v.is_finite() || v.abs() > limit {
        return Err(format!("Harus antara -{limit} dan {limit}"));
    }
    Ok(v)
}

impl Problems {
    /// True when nothing was rejected.
    pub fn is_clear(&self) -> bool {
        self.hotkey_toggle.is_empty() && self.hotkey_quit.is_empty() && self.accent.is_empty() && self.prayer_lat.is_empty() && self.prayer_lon.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_from_default_config_applies_back_unchanged() {
        let c = Config::default();
        let mut back = Config::default();
        let p = Form::from_config(&c).apply_to(&mut back);
        assert!(p.is_clear());
        assert_eq!(back, c);
    }

    #[test]
    fn form_maps_every_control_to_its_config_field() {
        let mut f = Form::from_config(&Config::default());
        f.general_language = 1;
        f.general_autostart = false;
        f.general_hotkey_toggle = " Ctrl+Alt+K ".into();
        f.general_hotkey_quit = "Ctrl+Alt+Shift+X".into();
        f.island_style = 1;
        f.island_scale = 115;
        f.island_open_on = 1;
        f.island_hover_delay = 900;
        f.island_hide_fullscreen = false;
        f.appearance_material = 1;
        f.appearance_accent = "#bf5af2".into();
        f.appearance_follow_album = false;
        f.appearance_animations = 2;
        f.media_enabled = false;
        f.media_lyrics = false;
        f.media_lyrics_in_pill = true;
        f.volume_osd_enabled = false;
        f.control_enabled = false;
        f.today_enabled = false;
        f.today_weather = false;
        f.today_city = "Bandung".into();
        f.today_units = 1;
        f.timer_enabled = false;
        f.timer_focus = 50;
        f.timer_break = 10;
        f.timer_hold = false;
        f.clipboard_enabled = false;
        f.clipboard_keep_days = 30;
        f.clipboard_max_items = 120;
        f.clipboard_ignore = "KeePassXC.exe; Foo.exe,\nBar.exe ,".into();
        f.capture_enabled = false;
        f.capture_save_dir = r" D:\Shots ".into();
        f.capture_copy = false;
        f.shelf_enabled = false;
        f.monitor_enabled = true;
        f.system_battery = false;
        f.system_caps_lock = false;
        f.system_network = false;
        f.system_privacy_dots = false;
        f.system_meeting_mode = false;
        f.reminders_water = true;
        f.reminders_water_interval = 45;
        f.reminders_water_target = 10;
        f.reminders_eye = true;
        f.reminders_eye_interval = 30;
        f.prayer_enabled = true;
        f.prayer_city = "Makassar".into();
        f.prayer_lat = "-5,1477".into();
        f.prayer_lon = "119.4327".into();
        f.prayer_minutes_before = 15;
        f.ipc_enabled = false;
        f.ipc_ai_cards = false;
        f.ipc_max_notifications = 80;
        f.privacy_hide_body = false;

        let mut c = Config::default();
        let p = f.apply_to(&mut c);
        assert!(p.is_clear());

        assert_eq!(c.general.language, Language::En);
        assert!(!c.general.autostart);
        assert_eq!(c.general.hotkey_toggle, "Ctrl+Alt+K");
        assert_eq!(c.general.hotkey_quit, "Ctrl+Alt+Shift+X");
        assert_eq!(c.island.style, IslandStyle::Notch);
        assert_eq!(c.island.scale, 1.15);
        assert_eq!(c.island.open_on, OpenOn::Click);
        assert_eq!(c.island.hover_delay_ms, 900);
        assert!(!c.island.hide_fullscreen);
        assert_eq!(c.appearance.material, Material::Black);
        assert_eq!(c.appearance.accent, "#BF5AF2");
        assert!(!c.appearance.follow_album_color);
        assert_eq!(c.appearance.animations, Animations::Off);
        assert!(!c.media.enabled && !c.media.lyrics && c.media.lyrics_in_pill);
        assert!(!c.volume_osd.enabled && !c.control.enabled);
        assert!(!c.today.enabled && !c.today.weather);
        assert_eq!(c.today.city, "Bandung");
        assert_eq!(c.today.units, Units::Imperial);
        assert!(!c.timer.enabled && !c.timer.hold_notifications_in_focus);
        assert_eq!((c.timer.focus_minutes, c.timer.break_minutes), (50, 10));
        assert!(!c.clipboard.enabled);
        assert_eq!((c.clipboard.keep_days, c.clipboard.max_items), (30, 120));
        assert_eq!(c.clipboard.ignore_apps, vec!["KeePassXC.exe", "Foo.exe", "Bar.exe"]);
        assert!(!c.capture.enabled && !c.capture.copy_to_clipboard);
        assert_eq!(c.capture.save_dir, r"D:\Shots");
        assert!(!c.shelf.enabled && c.monitor.enabled);
        assert!(!c.system.battery && !c.system.caps_lock && !c.system.network && !c.system.privacy_dots && !c.system.meeting_mode);
        assert!(c.reminders.water && c.reminders.eye_break);
        assert_eq!((c.reminders.water_interval_min, c.reminders.water_target, c.reminders.eye_interval_min), (45, 10, 30));
        assert!(c.prayer.enabled);
        assert_eq!(c.prayer.city, "Makassar");
        assert_eq!((c.prayer.latitude, c.prayer.longitude, c.prayer.minutes_before), (-5.1477, 119.4327, 15));
        assert!(!c.ipc.enabled && !c.ipc.ai_cards);
        assert_eq!(c.ipc.max_notifications, 80);
        assert!(!c.privacy.hide_notification_body_in_meetings);

        // And back: the form built from that config shows the same values (accent normalised).
        let f2 = Form::from_config(&c);
        assert_eq!(f2.appearance_accent, "#BF5AF2");
        assert_eq!(f2.island_scale, 115);
        assert_eq!(f2.prayer_lat, "-5.1477");
        assert_eq!(f2.clipboard_ignore, "KeePassXC.exe, Foo.exe, Bar.exe");
        let mut again = Config::default();
        assert!(f2.apply_to(&mut again).is_clear());
        assert_eq!(again, c);
    }

    #[test]
    fn a_key_another_feature_uses_is_refused_for_the_panel_and_quit_hotkeys() {
        let mut c = Config::default();
        let mut f = Form::from_config(&c);
        f.general_hotkey_toggle = "alt+space".into();
        f.general_hotkey_quit = "Ctrl+Alt+T".into();
        let p = f.apply_to(&mut c);
        assert_eq!(p.hotkey_toggle.as_str(), "Sudah dipakai peluncur");
        assert_eq!(p.hotkey_quit.as_str(), "Sudah dipakai OCR");
        assert_eq!((c.general.hotkey_toggle.as_str(), c.general.hotkey_quit.as_str()), ("Alt+M", "Ctrl+Alt+Shift+Q"), "nothing is saved");
        // A switched-off feature holds no key.
        c.ocr.enabled = false;
        let p = f.apply_to(&mut c);
        assert!(p.hotkey_quit.is_empty());
        assert_eq!(c.general.hotkey_quit, "Ctrl+Alt+T");
    }

    #[test]
    fn form_keeps_old_value_and_reports_a_problem_for_bad_text() {
        let mut c = Config::default();
        let mut f = Form::from_config(&c);
        f.general_hotkey_toggle = "Alt+Nope".into();
        f.appearance_accent = "blue".into();
        f.prayer_lat = "abc".into();
        f.prayer_lon = "200".into();
        let p = f.apply_to(&mut c);
        assert!(!p.hotkey_toggle.is_empty() && !p.accent.is_empty() && !p.prayer_lat.is_empty() && !p.prayer_lon.is_empty());
        assert!(p.hotkey_quit.is_empty());
        assert_eq!(c.general.hotkey_toggle, "Alt+M");
        assert_eq!(c.appearance.accent, "#0A84FF");
        assert_eq!(c.prayer.latitude, -6.2088);
        assert_eq!(c.prayer.longitude, 106.8456);

        // Two identical hotkeys: the second one is refused.
        let mut f = Form::from_config(&Config::default());
        f.general_hotkey_quit = "alt+m".into();
        let mut c = Config::default();
        let p = f.apply_to(&mut c);
        assert!(!p.hotkey_quit.is_empty());
        assert_eq!(c.general.hotkey_quit, "Ctrl+Alt+Shift+Q");
    }

    #[test]
    fn form_clamps_numbers_into_the_config_ranges() {
        let mut f = Form::from_config(&Config::default());
        f.island_scale = 500;
        f.island_hover_delay = -5;
        f.timer_focus = 0;
        f.clipboard_max_items = 100_000;
        f.reminders_water_interval = 1;
        f.island_style = 99; // unknown index: keep the current style
        let mut c = Config::default();
        f.apply_to(&mut c);
        assert_eq!(c.island.scale, 1.3);
        assert_eq!(c.island.hover_delay_ms, 0);
        assert_eq!(c.timer.focus_minutes, 1);
        assert_eq!(c.clipboard.max_items, 500);
        assert_eq!(c.reminders.water_interval_min, 5);
        assert_eq!(c.island.style, IslandStyle::Dynamic);
        // The result still survives the core loader without warnings.
        let (back, w) = Config::parse(&c.to_toml());
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(back, c);
    }

    #[test]
    fn changing_the_city_clears_geocoded_coordinates_but_other_edits_keep_them() {
        let mut c = Config::default();
        c.today.latitude = Some(-6.2);
        c.today.longitude = Some(106.8);
        let mut f = Form::from_config(&c);
        f.media_lyrics = false;
        f.apply_to(&mut c);
        assert_eq!((c.today.latitude, c.today.longitude), (Some(-6.2), Some(106.8)));

        f.today_city = "Surabaya".into();
        f.apply_to(&mut c);
        assert_eq!(c.today.city, "Surabaya");
        assert_eq!((c.today.latitude, c.today.longitude), (None, None));

        // An emptied box (mid-typing) changes nothing.
        f.today_city = "  ".into();
        f.apply_to(&mut c);
        assert_eq!(c.today.city, "Surabaya");
    }

    #[test]
    fn list_and_hotkey_helpers() {
        assert_eq!(parse_list(" a.exe ,, b.exe;c.exe\n"), vec!["a.exe", "b.exe", "c.exe"]);
        assert_eq!(parse_list("   "), Vec::<String>::new());
        assert_eq!(join_list(&["a".into(), "b".into()]), "a, b");
        assert_eq!(hotkey_parts("Ctrl+Alt+Shift+Q"), vec!["Ctrl", "Alt", "Shift", "Q"]);
        assert_eq!(hotkey_parts(""), Vec::<String>::new());
    }
}
