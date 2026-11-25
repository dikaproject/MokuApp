//! Track P: the Hari ini extras of the Fitur page (`[today].holidays`, `[bmkg]`, `[glance]`),
//! bound to the `TodayCfg` global of `ui/pages-p.slint`. Saved with a read-modify-write of
//! config.toml (`Config::update_file`), so it never overwrites what the main form saves, and
//! debounced while a field is typed in.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use moku_core::Config;
use moku_core::geo;
use slint::ComponentHandle;

use crate::{SettingsWindow, TodayCfg};

/// The values of the page.
#[derive(Clone, Debug, PartialEq)]
pub struct Values {
    pub holidays: bool,
    pub quake: bool,
    pub radius_km: i32,
    pub big_magnitude: i32,
    pub nowcast: bool,
    pub province: String,
    pub rate: bool,
    pub base: String,
    pub quote: String,
    pub aqi: bool,
    pub aqi_alert: bool,
}

pub fn read(c: &Config) -> Values {
    Values {
        holidays: c.today.holidays,
        quake: c.bmkg.quake,
        radius_km: c.bmkg.radius_km as i32,
        big_magnitude: c.bmkg.big_magnitude.round() as i32,
        nowcast: c.bmkg.nowcast,
        province: c.bmkg.province.clone(),
        rate: c.glance.rate,
        base: c.glance.base.clone(),
        quote: c.glance.quote.clone(),
        aqi: c.glance.aqi,
        aqi_alert: c.glance.aqi_alert,
    }
}

/// A currency code as typed: three letters, in any case -> upper case. `None` otherwise.
pub fn currency(s: &str) -> Option<String> {
    let t = s.trim();
    (t.len() == 3 && t.bytes().all(|b| b.is_ascii_alphabetic())).then(|| t.to_ascii_uppercase())
}

/// Why the pair cannot be used (Indonesian), or `None`.
pub fn pair_problem(base: &str, quote: &str) -> Option<String> {
    if currency(base).is_none() || currency(quote).is_none() {
        return Some("Pakai kode 3 huruf, mis. USD dan IDR".into());
    }
    None
}

/// What the province field means: `(note under the field, typed name is unknown)`. With an empty
/// field the note says what the city gives.
pub fn province_note(province: &str, city: &str) -> (String, bool) {
    let p = province.trim();
    if p.is_empty() {
        return match geo::province_of_city(city, None) {
            Some(found) => (format!("Kosong = otomatis dari kota ({city}): {found}"), false),
            None => ("Kosong = otomatis dari kota. Kotamu tidak ada di daftar: isi provinsinya di sini".to_string(), false),
        };
    }
    match geo::canonical(p) {
        Some(name) => (format!("Peringatan untuk {name}"), false),
        None => (String::new(), true),
    }
}

/// Apply the page to a config. A currency code or a province that is not usable keeps the old value.
pub fn apply(v: &Values, cfg: &mut Config) {
    cfg.today.holidays = v.holidays;
    cfg.bmkg.quake = v.quake;
    cfg.bmkg.radius_km = v.radius_km.clamp(50, 3000) as u32;
    cfg.bmkg.big_magnitude = f64::from(v.big_magnitude.clamp(4, 9));
    cfg.bmkg.nowcast = v.nowcast;
    let p = v.province.trim();
    if p.is_empty() || geo::canonical(p).is_some() {
        // Saved as the proper spelling, so `config.toml` reads cleanly.
        cfg.bmkg.province = geo::canonical(p).map(str::to_string).unwrap_or_default();
    }
    cfg.glance.rate = v.rate;
    if let (Some(b), Some(q)) = (currency(&v.base), currency(&v.quote)) {
        cfg.glance.base = b;
        cfg.glance.quote = q;
    }
    cfg.glance.aqi = v.aqi;
    cfg.glance.aqi_alert = v.aqi_alert;
}

fn show(ui: &SettingsWindow, v: &Values, city: &str) {
    let g = ui.global::<TodayCfg>();
    g.set_holidays(v.holidays);
    g.set_quake(v.quake);
    g.set_radius_km(v.radius_km);
    g.set_big_magnitude(v.big_magnitude);
    g.set_nowcast(v.nowcast);
    g.set_province(v.province.clone().into());
    let (note, bad) = province_note(&v.province, city);
    g.set_province_note(note.into());
    g.set_province_bad(bad);
    g.set_rate(v.rate);
    g.set_base(v.base.clone().into());
    g.set_quote(v.quote.clone().into());
    g.set_pair_problem(pair_problem(&v.base, &v.quote).unwrap_or_default().into());
    g.set_aqi(v.aqi);
    g.set_aqi_alert(v.aqi_alert);
}

fn read_ui(ui: &SettingsWindow) -> Values {
    let g = ui.global::<TodayCfg>();
    Values {
        holidays: g.get_holidays(),
        quake: g.get_quake(),
        radius_km: g.get_radius_km(),
        big_magnitude: g.get_big_magnitude(),
        nowcast: g.get_nowcast(),
        province: g.get_province().to_string(),
        rate: g.get_rate(),
        base: g.get_base().to_string(),
        quote: g.get_quote().to_string(),
        aqi: g.get_aqi(),
        aqi_alert: g.get_aqi_alert(),
    }
}

/// Fill the page from the config file at `path` and save its edits.
pub fn wire(ui: &SettingsWindow, path: std::path::PathBuf) {
    let g = ui.global::<TodayCfg>();
    let city = Rc::new(RefCell::new(String::new()));
    g.on_reload({
        let (weak, path, city) = (ui.as_weak(), path.clone(), city.clone());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let (c, _) = Config::load_from(&path);
            *city.borrow_mut() = c.today.city.clone();
            show(&ui, &read(&c), &c.today.city);
        }
    });
    let timer = Rc::new(slint::Timer::default());
    g.on_changed({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let v = read_ui(&ui);
            // The notes follow the typing at once; the file is written when the typing pauses.
            let g = ui.global::<TodayCfg>();
            let (note, bad) = province_note(&v.province, &city.borrow());
            g.set_province_note(note.into());
            g.set_province_bad(bad);
            g.set_pair_problem(pair_problem(&v.base, &v.quote).unwrap_or_default().into());
            let (weak, path) = (weak.clone(), path.clone());
            timer.start(slint::TimerMode::SingleShot, Duration::from_millis(400), move || {
                let Some(ui) = weak.upgrade() else { return };
                let v = read_ui(&ui);
                if let Err(e) = Config::update_file(&path, &Config::default(), |c| apply(&v, c)) {
                    eprintln!("today settings: {e}");
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("moku-settings-today-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("config.toml")
    }

    #[test]
    fn the_page_reads_and_applies_every_field() {
        let mut c = Config::default();
        c.today.holidays = true;
        let v = Values {
            holidays: false,
            quake: true,
            radius_km: 150,
            big_magnitude: 5,
            nowcast: true,
            province: "kep. riau".into(),
            rate: true,
            base: "eur".into(),
            quote: " jpy ".into(),
            aqi: true,
            aqi_alert: false,
        };
        apply(&v, &mut c);
        assert!(!c.today.holidays && c.bmkg.quake && c.bmkg.nowcast && c.glance.rate && c.glance.aqi && !c.glance.aqi_alert);
        assert_eq!((c.bmkg.radius_km, c.bmkg.big_magnitude), (150, 5.0));
        assert_eq!(c.bmkg.province, "Kepulauan Riau", "saved with the proper spelling");
        assert_eq!((c.glance.base.as_str(), c.glance.quote.as_str()), ("EUR", "JPY"));
        // What was written reads back the same.
        let (back, w) = Config::parse(&c.to_toml());
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(read(&back), Values { province: "Kepulauan Riau".into(), base: "EUR".into(), quote: "JPY".into(), ..v });
    }

    #[test]
    fn an_unusable_pair_or_province_keeps_the_old_value() {
        let mut c = Config::default();
        c.bmkg.province = "Jawa Barat".into();
        let mut v = read(&c);
        v.base = "US".into();
        v.quote = "RUPIAH".into();
        v.province = "Narnia".into();
        v.radius_km = 99_999;
        v.big_magnitude = 99;
        apply(&v, &mut c);
        assert_eq!((c.glance.base.as_str(), c.glance.quote.as_str()), ("USD", "IDR"));
        assert_eq!(c.bmkg.province, "Jawa Barat");
        assert_eq!((c.bmkg.radius_km, c.bmkg.big_magnitude), (3000, 9.0), "numbers are clamped into the schema's range");
        // An empty province means "from the city".
        v.province = "  ".into();
        apply(&v, &mut c);
        assert_eq!(c.bmkg.province, "");
    }

    #[test]
    fn notes_for_the_province_and_the_pair() {
        assert_eq!(province_note("Narnia", "Jakarta"), (String::new(), true));
        assert_eq!(province_note("jabar", "Jakarta"), ("Peringatan untuk Jawa Barat".to_string(), false));
        let (auto, bad) = province_note("", "Bandung");
        assert!(!bad && auto.contains("Jawa Barat") && auto.contains("Bandung"), "{auto}");
        let (unknown, bad) = province_note("", "Atlantis");
        assert!(!bad && unknown.contains("isi provinsinya"), "{unknown}");
        assert_eq!(pair_problem("USD", "IDR"), None);
        assert_eq!(pair_problem("usd", " idr "), None, "case and spaces are forgiven");
        assert!(pair_problem("US", "IDR").is_some() && pair_problem("USD", "RUPIAH").is_some() && pair_problem("U5D", "IDR").is_some() && pair_problem("", "").is_some());
    }

    #[test]
    fn saving_touches_only_this_pages_keys() {
        let path = temp("only-mine");
        let mut other = Config::default();
        other.island.scale = 1.25;
        other.launcher.hotkey = "Ctrl+Alt+K".into();
        other.save_to(&path).unwrap();
        let mut v = read(&other);
        v.quake = true;
        v.radius_km = 120;
        Config::update_file(&path, &Config::default(), |c| apply(&v, c)).unwrap();
        let (back, w) = Config::load_from(&path);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!((back.island.scale, back.launcher.hotkey.as_str()), (1.25, "Ctrl+Alt+K"), "the other writers' edits survive");
        assert!(back.bmkg.quake);
        assert_eq!(back.bmkg.radius_km, 120);
    }
}
