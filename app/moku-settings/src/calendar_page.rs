//! The calendar and download controls of the "Fitur" page (v0.2, track J).
//!
//! They live outside `Form` / `form.rs`: this module reads and writes `[calendar]` and
//! `[downloads]` of `config.toml` itself, with the same read-modify-write as every other save
//! (`Config::update_file`: a hand edit or a save by another part of the window is kept).
//!
//! The calendar addresses are secrets. The UI only ever receives them **masked**
//! (`moku_core::config::mask_url`); the real strings stay in `config.toml` and in [`Model`].

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use moku_core::Config;
use moku_core::config::{check_url, mask_url};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{CalUrl, CalendarCfg, SettingsWindow};

/// The settings behind the controls, without any window: the logic the tests drive.
pub struct Model {
    path: PathBuf,
    /// The config as we last read or wrote it (the fallback when the file is unreadable for a
    /// moment).
    last: Config,
}

impl Model {
    pub fn open(path: PathBuf) -> Model {
        let last = Config::load_from(&path).0;
        Model { path, last }
    }

    /// Read the file again (the page was built, or "Reset ke awal" rewrote it).
    pub fn reload(&mut self) {
        self.last = Config::load_from(&self.path).0;
    }

    pub fn config(&self) -> &Config {
        &self.last
    }

    fn edit(&mut self, change: impl FnOnce(&mut Config)) -> Result<(), String> {
        let cfg = Config::update_file(&self.path, &self.last, change).map_err(|e| format!("Gagal menyimpan: {e}"))?;
        self.last = cfg;
        Ok(())
    }

    pub fn set_enabled(&mut self, on: bool) -> Result<(), String> {
        self.edit(|c| c.calendar.enabled = on)
    }

    pub fn set_minutes(&mut self, minutes: i32) -> Result<(), String> {
        let m = minutes.clamp(1, 60) as u32;
        self.edit(|c| c.calendar.minutes_before = m)
    }

    pub fn set_downloads(&mut self, on: bool) -> Result<(), String> {
        self.edit(|c| c.downloads.enabled = on)
    }

    /// Save a pasted address. The first address also switches the calendar on (nothing else
    /// would make the paste do anything).
    pub fn add_url(&mut self, input: &str) -> Result<(), String> {
        let url = check_url(input, &self.last.calendar.urls).map_err(str::to_string)?;
        self.edit(|c| {
            if c.calendar.urls.is_empty() {
                c.calendar.enabled = true;
            }
            c.calendar.urls.push(url);
        })
    }

    pub fn remove_url(&mut self, index: usize) -> Result<(), String> {
        self.edit(|c| {
            if index < c.calendar.urls.len() {
                c.calendar.urls.remove(index);
            }
        })
    }

    /// The saved addresses for display: masked.
    pub fn masked(&self) -> Vec<String> {
        self.last.calendar.urls.iter().map(|u| mask_url(u)).collect()
    }
}

/// Put the model's state into the window.
fn show(ui: &SettingsWindow, m: &Model) {
    let c = ui.global::<CalendarCfg>();
    let cfg = m.config();
    c.set_enabled(cfg.calendar.enabled);
    c.set_minutes_before(cfg.calendar.minutes_before as i32);
    c.set_downloads_enabled(cfg.downloads.enabled);
    let rows: Vec<CalUrl> = m.masked().into_iter().map(|masked| CalUrl { masked: masked.into() }).collect();
    c.set_urls(ModelRc::new(VecModel::from(rows)));
}

/// Connect the controls to `config.toml` (the real one, or `$MOKU_DATA_DIR`'s).
pub fn wire(ui: &SettingsWindow, path: PathBuf) {
    let model = Rc::new(RefCell::new(Model::open(path)));
    let c = ui.global::<CalendarCfg>();
    show(ui, &model.borrow());

    // Run an edit, then refresh the window; a failure shows under the address field.
    let apply = |model: &Rc<RefCell<Model>>, ui: &slint::Weak<SettingsWindow>, f: &dyn Fn(&mut Model) -> Result<(), String>| -> bool {
        let r = f(&mut model.borrow_mut());
        let Some(ui) = ui.upgrade() else { return r.is_ok() };
        match &r {
            Ok(()) => {
                ui.global::<CalendarCfg>().set_problem("".into());
                show(&ui, &model.borrow());
            }
            Err(e) => ui.global::<CalendarCfg>().set_problem(e.as_str().into()),
        }
        r.is_ok()
    };

    c.on_reload({
        let (model, ui) = (model.clone(), ui.as_weak());
        move || {
            model.borrow_mut().reload();
            if let Some(ui) = ui.upgrade() {
                ui.global::<CalendarCfg>().set_problem("".into());
                show(&ui, &model.borrow());
            }
        }
    });
    c.on_set_enabled({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |on| {
            apply(&model, &ui, &|m| m.set_enabled(on));
        }
    });
    c.on_set_minutes({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |v| {
            apply(&model, &ui, &|m| m.set_minutes(v));
        }
    });
    c.on_set_downloads({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |on| {
            apply(&model, &ui, &|m| m.set_downloads(on));
        }
    });
    c.on_add_url({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |text| apply(&model, &ui, &|m| m.add_url(text.as_str()))
    });
    c.on_remove_url({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |i| {
            apply(&model, &ui, &|m| m.remove_url(i.max(0) as usize));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "https://calendar.google.com/calendar/ical/me%40gmail.com/private-0f9a7c1e2b3d4455/basic.ics";

    fn temp_path(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moku-settings-cal-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("config.toml")
    }

    #[test]
    fn adding_an_address_saves_it_as_typed_and_shows_it_masked() {
        let path = temp_path("add");
        let mut m = Model::open(path.clone());
        assert!(!m.config().calendar.enabled);
        m.add_url(&format!("  {SECRET}  ")).unwrap();
        // On disk: exactly as pasted (trimmed), the calendar switched on by the first address.
        let (disk, w) = Config::load_from(&path);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(disk.calendar.urls, [SECRET]);
        assert!(disk.calendar.enabled);
        // In the window: masked, nothing of the account or token.
        let shown = m.masked();
        assert_eq!(shown, ["https://calendar.google.com/••••••••/basic.ics"]);
        assert!(!shown[0].contains("private") && !shown[0].contains("gmail"));
        // A second address does not flip the switch back on after the user turned it off.
        m.set_enabled(false).unwrap();
        m.add_url("webcal://p01-caldav.example/x.ics").unwrap();
        assert!(!Config::load_from(&path).0.calendar.enabled);
        assert_eq!(Config::load_from(&path).0.calendar.urls.len(), 2);
    }

    #[test]
    fn bad_and_duplicate_addresses_are_refused_with_a_message_and_change_nothing() {
        let path = temp_path("bad");
        let mut m = Model::open(path.clone());
        m.add_url(SECRET).unwrap();
        for (input, part) in [("", "Tempel"), ("http://plain.example/x.ics", "https"), ("not a url", "https://"), (SECRET, "sudah ada")] {
            let e = m.add_url(input).unwrap_err();
            assert!(e.contains(part), "{input:?} -> {e}");
            assert!(!e.contains("private-0f9a"), "messages never echo an address: {e}");
        }
        assert_eq!(Config::load_from(&path).0.calendar.urls, [SECRET]);
    }

    #[test]
    fn removing_and_the_other_controls() {
        let path = temp_path("misc");
        let mut m = Model::open(path.clone());
        m.add_url("https://a.example/one.ics").unwrap();
        m.add_url("https://b.example/two.ics").unwrap();
        m.remove_url(0).unwrap();
        m.remove_url(99).unwrap();
        assert_eq!(Config::load_from(&path).0.calendar.urls, ["https://b.example/two.ics"]);
        m.set_minutes(10).unwrap();
        m.set_minutes(500).unwrap();
        assert_eq!(Config::load_from(&path).0.calendar.minutes_before, 60, "clamped to the schema's range");
        m.set_minutes(-3).unwrap();
        assert_eq!(Config::load_from(&path).0.calendar.minutes_before, 1);
        m.set_downloads(false).unwrap();
        assert!(!Config::load_from(&path).0.downloads.enabled);
    }

    #[test]
    fn other_edits_to_the_file_survive_and_reload_picks_up_outside_changes() {
        let path = temp_path("other");
        let mut m = Model::open(path.clone());
        // The rest of the settings window (or a hand edit) saves something else in between.
        let mut other = Config::default();
        other.island.scale = 1.25;
        other.save_to(&path).unwrap();
        m.add_url("https://a.example/one.ics").unwrap();
        let disk = Config::load_from(&path).0;
        assert_eq!(disk.island.scale, 1.25, "the other edit is kept");
        assert_eq!(disk.calendar.urls.len(), 1);
        // "Reset ke awal" rewrites the file; the page shows it after a reload.
        Config::default().save_to(&path).unwrap();
        m.reload();
        assert!(m.masked().is_empty() && !m.config().calendar.enabled);
    }
}
