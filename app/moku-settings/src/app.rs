//! The glue between the Slint window and the rest: every callback the UI can invoke
//! (`Cfg.commit()`, `Cfg.set-autostart()`, `Cfg.delete-data()` ...) is implemented here.
//!
//! Everything the glue touches outside the window (registry key, `moku.exe` path, data folder)
//! comes in through [`Env`], so the tests drive the real handlers against temporary locations
//! with a headless window and a fake clock; see `tests` below.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use moku_core::Config;
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::persist::Store;
use crate::{Cfg, Form, SettingsWindow, Theme, autostart, data, form, system};

/// How long edits wait for more edits before they are written.
pub const SAVE_DEBOUNCE: Duration = Duration::from_millis(300);

/// Where the glue reads and writes outside the window.
pub struct Env {
    /// Registry key (under HKCU) that holds the autostart value.
    pub autostart_key: String,
    /// What the autostart value points to.
    pub moku_exe: PathBuf,
    /// The data folder ("Hapus semua data Moku", "Buka folder").
    pub data_dir: PathBuf,
}

impl Env {
    /// The real locations (honouring `MOKU_DATA_DIR` and `MOKU_AUTOSTART_KEY`).
    pub fn real() -> Env {
        Env { autostart_key: autostart::run_key(), moku_exe: autostart::moku_exe(), data_dir: moku_core::paths::data_dir() }
    }
}

pub struct State {
    store: Store,
    env: Env,
    save_timer: Timer,
    toast_timer: Timer,
    /// The window was opened straight into the onboarding (first run or `--onboarding`):
    /// finishing it closes the window instead of showing the settings.
    entry_onboarding: bool,
}

pub type Shared = Rc<RefCell<State>>;

/// Fill the window from `config` and connect all callbacks.
pub fn wire(ui: &SettingsWindow, mut store: Store, config: &Config, env: Env, entry_onboarding: bool) -> Shared {
    let cfg = ui.global::<Cfg>();

    // Autostart shows what the registry really says, and the saved setting is brought in line
    // with it (the Run value can be changed outside Moku). A missing config.toml is left alone:
    // the first save carries the registry state anyway.
    let mut form = Form::from_config(config);
    form.general_autostart = autostart::is_enabled(&env.autostart_key);
    if config.general.autostart != form.general_autostart && store.exists() {
        let on = form.general_autostart;
        if let Err(e) = store.update(|c| c.general.autostart = on) {
            eprintln!("autostart: cannot save config.toml: {e}");
        }
    }
    cfg.set_autostart_granted(form.general_autostart);
    cfg.set_f(form.clone());
    refresh_derived(ui, &form);

    cfg.set_version(moku_core::VERSION.into());
    cfg.set_data_dir(env.data_dir.display().to_string().into());
    cfg.set_config_path(env.data_dir.join("config.toml").display().to_string().into());
    match system::find_doc() {
        Some(p) => {
            cfg.set_doc_path(p.display().to_string().into());
            cfg.set_doc_found(true);
        }
        None => {
            cfg.set_doc_path(format!("docs/{}", system::DOC_NAME).into());
            cfg.set_doc_found(false);
        }
    }
    cfg.set_onboarding(entry_onboarding);

    let st: Shared = Rc::new(RefCell::new(State { store, env, save_timer: Timer::default(), toast_timer: Timer::default(), entry_onboarding }));

    // A control changed: validate for live feedback, then save after 300 ms of quiet.
    cfg.on_commit(handler(ui, &st, |ui, st| on_edit(ui, st)));

    // Autostart is a registry value as well as a setting.
    cfg.on_set_autostart({
        let (ui, st) = (ui.as_weak(), st.clone());
        move |on| {
            let Some(ui) = ui.upgrade() else { return };
            let r = {
                let s = st.borrow();
                if on { autostart::enable(&s.env.autostart_key, &s.env.moku_exe) } else { autostart::disable(&s.env.autostart_key) }
            };
            match r {
                Ok(()) => {
                    let cfg = ui.global::<Cfg>();
                    let mut f = cfg.get_f();
                    f.general_autostart = on;
                    cfg.set_f(f);
                    cfg.set_autostart_granted(on);
                    on_edit(&ui, &st);
                }
                Err(e) => toast(&ui, &st, &format!("Gagal mengubah autostart: {e}"), 4000),
            }
        }
    });

    // "Hapus semua data Moku" (the confirmation dialog is in the UI).
    cfg.on_delete_data(handler(ui, &st, |ui, st| {
        flush(ui, st, false); // config.toml is final before we wipe around it
        // A running island holds clipboard / shelf / to-do data in memory and would write it
        // back later: it drops it (and finishes its pending writes) before we delete the files.
        let notice = data::notify_island(|req| moku_core::ipc::send(req, Some(std::time::Duration::from_secs(5))));
        let dir = st.borrow().env.data_dir.clone();
        let mut msg = match data::delete_data_in(&dir) {
            Ok(d) if d.failed == 0 => format!("Data dihapus ({} item). Pengaturan tetap ada.", d.removed),
            Ok(d) => format!("{} item dihapus, {} sedang dipakai dan dilewati", d.removed, d.failed),
            Err(e) => format!("Gagal menghapus data: {e}"),
        };
        if matches!(notice, data::IslandNotice::Failed(_)) {
            msg.push_str(" Moku tidak menjawab; mulai ulang Moku agar datanya ikut bersih.");
        }
        toast(ui, st, &msg, 5000);
    }));

    // "Reset ke awal".
    cfg.on_reset_settings(handler(ui, &st, |ui, st| {
        st.borrow().save_timer.stop();
        let r = st.borrow_mut().store.reset();
        match r {
            Ok(config) => {
                let cfg = ui.global::<Cfg>();
                let mut f = Form::from_config(&config);
                f.general_autostart = autostart::is_enabled(&st.borrow().env.autostart_key);
                cfg.set_f(f.clone());
                cfg.set_epoch(cfg.get_epoch() + 1); // rebuild the pages so text fields reload
                refresh_derived(ui, &f);
                on_edit(ui, st);
                toast(ui, st, "Pengaturan dikembalikan ke awal", 2500);
            }
            Err(e) => toast(ui, st, &format!("Gagal menyimpan: {e}"), 4000),
        }
    }));

    cfg.on_open_data_folder(handler(ui, &st, |_, st| {
        let dir = st.borrow().env.data_dir.clone();
        if std::fs::create_dir_all(&dir).is_ok() {
            system::open_path(&dir);
        }
    }));
    cfg.on_open_doc(handler(ui, &st, |ui, st| {
        if system::find_doc().is_some_and(|p| system::open_path(&p)) {
            return;
        }
        toast(ui, st, "Panduan tidak ditemukan di samping aplikasi", 3500);
    }));

    // Onboarding.
    cfg.on_start_onboarding(handler(ui, &st, |ui, st| {
        let cfg = ui.global::<Cfg>();
        cfg.set_autostart_granted(autostart::is_enabled(&st.borrow().env.autostart_key));
        cfg.set_step(0);
        cfg.set_onboarding(true);
    }));
    cfg.on_finish_onboarding(handler(ui, &st, |ui, st| {
        let cfg = ui.global::<Cfg>();
        let form = cfg.get_f();
        st.borrow().save_timer.stop();
        let r = st.borrow_mut().store.finish_onboarding(&form);
        if let Err(e) = r {
            toast(ui, st, &format!("Gagal menyimpan: {e}"), 4000);
            return;
        }
        if st.borrow().entry_onboarding {
            let _ = ui.hide();
        } else {
            cfg.set_onboarding(false);
            cfg.set_page(crate::ABOUT_PAGE);
        }
    }));

    // Closing the window saves anything still waiting for the debounce.
    ui.window().on_close_requested({
        let (ui, st) = (ui.as_weak(), st.clone());
        move || {
            if let Some(ui) = ui.upgrade() {
                flush(&ui, &st, false);
            }
            slint::CloseRequestResponse::HideWindow
        }
    });
    st
}

/// Wrap a callback body: upgrade the weak window, then run it.
fn handler(ui: &SettingsWindow, st: &Shared, f: impl Fn(&SettingsWindow, &Shared) + 'static) -> impl Fn() + 'static {
    let (ui, st) = (ui.as_weak(), st.clone());
    move || {
        if let Some(ui) = ui.upgrade() {
            f(&ui, &st);
        }
    }
}

/// Things derived from the form that the UI cannot compute itself: the accent colour and the
/// keycap chips of the toggle hotkey.
fn refresh_derived(ui: &SettingsWindow, form: &Form) {
    if let Some(rgb) = moku_core::color::Rgb::parse(&form.appearance_accent) {
        ui.global::<Theme>().set_accent(slint::Color::from_rgb_u8(rgb.r(), rgb.g(), rgb.b()));
    }
    if moku_core::hotkey::Hotkey::parse(form.general_hotkey_toggle.trim()).is_ok() {
        let parts: Vec<SharedString> = form::hotkey_parts(&form.general_hotkey_toggle).into_iter().map(SharedString::from).collect();
        ui.global::<Cfg>().set_hotkey_parts(ModelRc::new(VecModel::from(parts)));
    }
}

/// A control changed.
fn on_edit(ui: &SettingsWindow, st: &Shared) {
    let cfg = ui.global::<Cfg>();
    let form = cfg.get_f();
    let mut scratch = st.borrow().store.current().clone();
    cfg.set_problems(form.apply_to(&mut scratch));
    refresh_derived(ui, &form);

    let (ui, st2) = (ui.as_weak(), st.clone());
    st.borrow().save_timer.start(TimerMode::SingleShot, SAVE_DEBOUNCE, move || {
        if let Some(ui) = ui.upgrade() {
            flush(&ui, &st2, true);
        }
    });
}

/// Save the current form now.
pub fn flush(ui: &SettingsWindow, st: &Shared, announce: bool) {
    let cfg = ui.global::<Cfg>();
    let form = cfg.get_f();
    st.borrow().save_timer.stop();
    let r = st.borrow_mut().store.save(&form);
    match r {
        Ok(saved) => {
            cfg.set_problems(saved.problems);
            if announce && saved.wrote {
                toast(ui, st, "Tersimpan", 1200);
            }
        }
        Err(e) => toast(ui, st, &format!("Gagal menyimpan: {e}"), 4000),
    }
}

/// Show a message at the bottom right for `ms` milliseconds.
pub fn toast(ui: &SettingsWindow, st: &Shared, msg: &str, ms: u64) {
    ui.global::<Cfg>().set_toast(msg.into());
    let ui = ui.as_weak();
    st.borrow().toast_timer.start(TimerMode::SingleShot, Duration::from_millis(ms), move || {
        if let Some(ui) = ui.upgrade() {
            ui.global::<Cfg>().set_toast("".into());
        }
    });
}

#[cfg(test)]
mod tests {
    //! The real window and the real handlers, headless: a software-renderer window that never
    //! shows, and a clock the test advances by hand so the 300 ms debounce is deterministic.
    use std::cell::Cell;
    use std::path::Path;
    use std::time::Duration;

    use slint::Model;
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    use slint::platform::{PointerEventButton, Platform, WindowAdapter, WindowEvent, update_timers_and_animations};

    use super::*;

    thread_local! {
        static NOW: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    }

    struct Headless(Rc<MinimalSoftwareWindow>);

    impl Platform for Headless {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
        fn duration_since_start(&self) -> Duration {
            NOW.with(|n| n.get())
        }
    }

    /// Move the fake clock and run due timers.
    fn advance(ms: u64) {
        NOW.with(|n| n.set(n.get() + Duration::from_millis(ms)));
        update_timers_and_animations();
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moku-settings-glue-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    struct Rig {
        ui: SettingsWindow,
        win: Rc<MinimalSoftwareWindow>,
        st: Shared,
        dir: PathBuf,
        key: String,
    }

    impl Rig {
        fn new(name: &str, seed: Option<&Config>, entry_onboarding: bool) -> Rig {
            // set_platform is per thread (every #[test] runs on its own thread).
            let win = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
            slint::platform::set_platform(Box::new(Headless(win.clone()))).unwrap();
            NOW.with(|n| n.set(Duration::ZERO));
            let dir = temp_dir(name);
            if let Some(c) = seed {
                c.save_to(&dir.join("config.toml")).unwrap();
            }
            let key = format!(r"Software\MokuSettingsTest\Run-glue-{name}-{}", std::process::id());
            let (store, config, _) = Store::open(dir.join("config.toml"));
            let ui = SettingsWindow::new().unwrap();
            let env = Env { autostart_key: key.clone(), moku_exe: PathBuf::from(r"C:\Test Apps\Moku\moku.exe"), data_dir: dir.clone() };
            let st = wire(&ui, store, &config, env, entry_onboarding);
            Rig { ui, win, st, dir, key }
        }

        fn cfg(&self) -> Cfg<'_> {
            self.ui.global::<Cfg>()
        }

        /// What an edit in the window does: change `f`, then call `Cfg.commit()`.
        fn edit(&self, f: impl FnOnce(&mut Form)) {
            let mut form = self.cfg().get_f();
            f(&mut form);
            self.cfg().set_f(form);
            self.cfg().invoke_commit();
        }

        fn disk(&self) -> Config {
            Config::load_from(&self.dir.join("config.toml")).0
        }

        fn config_exists(&self) -> bool {
            self.dir.join("config.toml").exists()
        }

        /// Show the window at its default 1000x700 and lay it out, like the real one.
        fn open(&self) {
            self.ui.window().set_size(slint::PhysicalSize::new(1000, 700));
            self.ui.show().unwrap();
            self.paint();
        }

        /// Render a frame (instantiates conditional items, settles layout).
        fn paint(&self) {
            let mut buf = vec![slint::Rgb8Pixel::default(); 1000 * 700];
            self.win.draw_if_needed(|r| {
                r.render(&mut buf, 1000);
            });
        }

        fn pointer(&self, ev: impl FnOnce(slint::LogicalPosition) -> WindowEvent, x: f32, y: f32) {
            self.ui.window().dispatch_event(ev(slint::LogicalPosition::new(x, y)));
        }

        /// A left click at window (client) coordinates, delivered to the headless window only.
        fn click(&self, x: f32, y: f32) {
            self.pointer(|position| WindowEvent::PointerMoved { position }, x, y);
            self.pointer(|position| WindowEvent::PointerPressed { position, button: PointerEventButton::Left }, x, y);
            self.pointer(|position| WindowEvent::PointerReleased { position, button: PointerEventButton::Left }, x, y);
            self.paint();
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            autostart::delete_test_key(&self.key);
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn glue_edit_is_saved_after_the_300ms_debounce() {
        let r = Rig::new("debounce", None, false);
        assert!(!r.config_exists());

        r.edit(|f| f.island_style = 1);
        advance(200);
        assert!(!r.config_exists(), "not saved yet: only 200 ms passed");

        // A second edit restarts the wait.
        r.edit(|f| f.media_lyrics = false);
        advance(200);
        assert!(!r.config_exists(), "the timer restarted with the second edit");

        advance(150);
        assert!(r.config_exists(), "350 ms after the last edit it is written");
        let c = r.disk();
        assert_eq!(c.island.style, moku_core::config::IslandStyle::Notch);
        assert!(!c.media.lyrics);
        assert_eq!(r.cfg().get_toast(), "Tersimpan");
        advance(1300);
        assert_eq!(r.cfg().get_toast(), "", "the toast disappears again");
    }

    #[test]
    fn glue_bad_hotkey_shows_a_problem_and_is_not_saved() {
        let r = Rig::new("hotkey", None, false);
        r.edit(|f| {
            f.general_hotkey_toggle = "Alt+Nope".into();
            f.monitor_enabled = true;
        });
        assert!(!r.cfg().get_problems().hotkey_toggle.is_empty(), "feedback is immediate");
        advance(350);
        let c = r.disk();
        assert_eq!(c.general.hotkey_toggle, "Alt+M");
        assert!(c.monitor.enabled);

        r.edit(|f| f.general_hotkey_toggle = "Ctrl+Shift+K".into());
        assert!(r.cfg().get_problems().hotkey_toggle.is_empty());
        advance(350);
        assert_eq!(r.disk().general.hotkey_toggle, "Ctrl+Shift+K");
        let parts: Vec<_> = r.cfg().get_hotkey_parts().iter().collect();
        assert_eq!(parts, vec!["Ctrl", "Shift", "K"]);
    }

    #[test]
    fn glue_accent_change_updates_the_theme_colour() {
        let r = Rig::new("accent", None, false);
        r.edit(|f| f.appearance_accent = "#30D158".into());
        assert_eq!(r.ui.global::<Theme>().get_accent(), slint::Color::from_rgb_u8(0x30, 0xD1, 0x58));
    }

    #[test]
    fn glue_autostart_toggle_writes_the_run_value_and_the_setting() {
        let r = Rig::new("autostart", None, false);
        assert!(!autostart::is_enabled(&r.key));

        r.cfg().invoke_set_autostart(true);
        assert_eq!(autostart::current(&r.key).as_deref(), Some(r#""C:\Test Apps\Moku\moku.exe""#));
        assert!(r.cfg().get_f().general_autostart && r.cfg().get_autostart_granted());
        advance(350);
        assert!(r.disk().general.autostart);

        r.cfg().invoke_set_autostart(false);
        assert!(!autostart::is_enabled(&r.key));
        advance(350);
        assert!(!r.disk().general.autostart);
        assert!(!r.cfg().get_autostart_granted());
    }

    #[test]
    fn glue_start_syncs_saved_autostart_with_the_registry() {
        // Saved "on", but the Run value is missing (test key): the window shows off and the
        // file is corrected; other values are untouched.
        let mut c = Config::default();
        c.general.autostart = true;
        c.island.scale = 1.25;
        let r = Rig::new("autostart-sync", Some(&c), false);
        assert!(!r.cfg().get_f().general_autostart);
        let d = r.disk();
        assert!(!d.general.autostart, "config follows the registry");
        assert_eq!(d.island.scale, 1.25);
        // (No config file yet → nothing is written: see glue_edit_is_saved_after_the_300ms_debounce.)
    }

    #[test]
    fn glue_delete_all_data_keeps_config_and_reports() {
        let mut c = Config::default();
        c.island.scale = 1.25;
        let r = Rig::new("delete", Some(&c), false);
        std::fs::write(r.dir.join("todo.json"), "[]").unwrap();
        std::fs::create_dir_all(r.dir.join("clipboard")).unwrap();
        std::fs::write(r.dir.join("clipboard").join("index.json"), "{}").unwrap();

        // A pending, unsaved edit is written before the wipe and survives it.
        r.edit(|f| f.shelf_enabled = false);
        r.cfg().invoke_delete_data();

        let mut left: Vec<String> = std::fs::read_dir(&r.dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, vec!["config.toml"]);
        let c = r.disk();
        assert_eq!(c.island.scale, 1.25);
        assert!(!c.shelf.enabled);
        assert!(r.cfg().get_toast().starts_with("Data dihapus (2 item)"), "{}", r.cfg().get_toast());
    }

    #[test]
    fn glue_reset_restores_defaults_and_rebuilds_the_pages() {
        let mut c = Config::default();
        c.island.scale = 1.3;
        c.timer.focus_minutes = 90;
        c.onboarding.completed = true;
        let r = Rig::new("reset", Some(&c), false);
        assert_eq!(r.cfg().get_f().island_scale, 130);
        let epoch = r.cfg().get_epoch();

        r.cfg().invoke_reset_settings();
        advance(350);
        let f = r.cfg().get_f();
        assert_eq!((f.island_scale, f.timer_focus), (100, 25));
        assert_eq!(r.cfg().get_epoch(), epoch + 1);
        let c = r.disk();
        assert_eq!((c.island.scale, c.timer.focus_minutes), (1.0, 25));
        assert!(c.onboarding.completed, "onboarding stays done");
    }

    #[test]
    fn glue_finishing_first_run_onboarding_saves_choices_and_closes() {
        let r = Rig::new("onboard-first", None, true);
        assert!(r.cfg().get_onboarding());
        r.ui.show().unwrap();
        r.cfg().set_step(5);

        // The user picked Notch, switched weather off, typed a city and allowed autostart.
        r.edit(|f| {
            f.island_style = 1;
            f.today_weather = false;
            f.today_city = "Bandung".into();
        });
        r.cfg().invoke_set_autostart(true);
        r.cfg().invoke_finish_onboarding();

        let c = r.disk();
        assert!(c.onboarding.completed);
        assert_eq!(c.island.style, moku_core::config::IslandStyle::Notch);
        assert!(!c.today.weather);
        assert_eq!(c.today.city, "Bandung");
        assert!(c.general.autostart);
        assert!(autostart::is_enabled(&r.key));
        assert!(!r.ui.window().is_visible(), "first-run onboarding closes the window");
    }

    #[test]
    fn glue_replayed_onboarding_returns_to_the_settings() {
        let mut c = Config::default();
        c.onboarding.completed = true;
        let r = Rig::new("onboard-replay", Some(&c), false);
        assert!(!r.cfg().get_onboarding());

        r.cfg().invoke_start_onboarding();
        assert!(r.cfg().get_onboarding());
        assert_eq!(r.cfg().get_step(), 0);
        r.cfg().invoke_finish_onboarding();
        assert!(!r.cfg().get_onboarding());
        assert_eq!(r.cfg().get_page(), crate::ABOUT_PAGE);
        assert!(r.disk().onboarding.completed);
    }

    #[test]
    fn glue_closing_the_window_flushes_a_pending_edit() {
        let r = Rig::new("close", None, false);
        r.ui.show().unwrap();
        r.edit(|f| f.control_enabled = false);
        assert!(!r.config_exists());
        r.ui.window().dispatch_event(slint::platform::WindowEvent::CloseRequested);
        assert!(!r.disk().control.enabled);
        let _ = &r.st;
    }

    #[test]
    fn glue_starts_from_the_loaded_config() {
        let mut c = Config::default();
        c.today.city = "Medan".into();
        c.island.scale = 0.8;
        c.clipboard.ignore_apps = vec!["A.exe".into(), "B.exe".into()];
        let r = Rig::new("load", Some(&c), false);
        let f = r.cfg().get_f();
        assert_eq!((f.today_city.as_str(), f.island_scale, f.clipboard_ignore.as_str()), ("Medan", 80, "A.exe, B.exe"));
        assert_eq!(r.cfg().get_config_path().as_str(), Path::new(&r.dir).join("config.toml").display().to_string());
    }

    // ---- real pointer input, delivered to the headless window (coordinates are window client
    // pixels at the default 1000x700, read off the screenshots in prototype/results/v01).

    const SIDEBAR_X: f32 = 59.0;
    const NAV_PRIVASI_Y: f32 = 397.0;
    const NAV_AI_Y: f32 = 436.0;
    const NAV_MEMORI_Y: f32 = 475.0;
    const NAV_TENTANG_Y: f32 = 514.0;
    const NAV_UMUM_Y: f32 = 85.0;

    #[test]
    fn ui_clicking_the_sidebar_switches_pages() {
        let r = Rig::new("nav", None, false);
        r.open();
        assert_eq!(r.cfg().get_page(), 0);
        r.click(SIDEBAR_X, NAV_PRIVASI_Y);
        assert_eq!(r.cfg().get_page(), 8);
        // Tanya AI and Memori & Privasi come right after Privasi, Tentang is last (12 entries).
        r.click(SIDEBAR_X, NAV_AI_Y);
        assert_eq!(r.cfg().get_page(), 9);
        r.click(SIDEBAR_X, NAV_MEMORI_Y);
        assert_eq!(r.cfg().get_page(), 10);
        r.click(SIDEBAR_X, NAV_TENTANG_Y);
        assert_eq!(r.cfg().get_page(), crate::ABOUT_PAGE);
        r.click(SIDEBAR_X, NAV_UMUM_Y);
        assert_eq!(r.cfg().get_page(), 0);
    }

    #[test]
    fn ui_island_page_clicks_change_and_save_settings() {
        let r = Rig::new("island-clicks", None, false);
        r.cfg().set_page(1);
        r.open();

        r.click(936.0, 618.0); // toggle "Sembunyikan saat layar penuh"
        assert!(!r.cfg().get_f().island_hide_fullscreen);
        r.click(926.0, 305.0); // segment "Notch"
        assert_eq!(r.cfg().get_f().island_style, 1);
        r.click(933.0, 512.0); // segment "Klik" (the delay row below it goes away)
        assert_eq!(r.cfg().get_f().island_open_on, 1);

        advance(350);
        let c = r.disk();
        assert!(!c.island.hide_fullscreen);
        assert_eq!(c.island.style, moku_core::config::IslandStyle::Notch);
        assert_eq!(c.island.open_on, moku_core::config::OpenOn::Click);
    }

    #[test]
    fn ui_slider_press_and_drag_set_the_island_scale() {
        let r = Rig::new("slider", None, false);
        r.cfg().set_page(1);
        r.open();
        let (left, right, y) = (519.0 + 9.0, 519.0 + 190.0 - 9.0, 357.0); // thumb centres at both ends

        r.click(left, y);
        assert_eq!(r.cfg().get_f().island_scale, 75);
        r.click(right, y);
        assert_eq!(r.cfg().get_f().island_scale, 130);

        // Press at the left end and drag to 60 % of the track: 75 + 0.6 * 55 = 108 -> step 5 -> 110.
        // (The page's Flickable holds a press back for 100 ms to see if it is a scroll.)
        r.pointer(|position| WindowEvent::PointerMoved { position }, left, y);
        r.pointer(|position| WindowEvent::PointerPressed { position, button: PointerEventButton::Left }, left, y);
        advance(150);
        assert_eq!(r.cfg().get_f().island_scale, 75, "the press itself sets the value");
        r.pointer(|position| WindowEvent::PointerMoved { position }, left + 0.6 * (right - left), y);
        r.pointer(|position| WindowEvent::PointerReleased { position, button: PointerEventButton::Left }, left + 0.6 * (right - left), y);
        assert_eq!(r.cfg().get_f().island_scale, 110);
        advance(350);
        assert_eq!(r.disk().island.scale, 1.1);
    }

    #[test]
    fn ui_onboarding_walkthrough_by_clicks() {
        let r = Rig::new("walkthrough", None, true);
        r.open();
        assert_eq!((r.cfg().get_onboarding(), r.cfg().get_step()), (true, 0));

        r.click(950.0, 654.0); // Mulai
        assert_eq!(r.cfg().get_step(), 1);
        r.click(657.0, 358.0); // style card "Notch"
        r.click(565.0, 490.0); // glass "Pekat"
        let f = r.cfg().get_f();
        assert_eq!((f.island_style, f.appearance_material), (1, 1));
        r.click(937.0, 654.0); // Lanjut -> Fitur
        assert_eq!(r.cfg().get_step(), 2);
        r.click(937.0, 654.0); // Lanjut -> Izin
        assert_eq!(r.cfg().get_step(), 3);

        assert!(!autostart::is_enabled(&r.key));
        r.click(741.0, 334.0); // "Izinkan" (autostart)
        assert!(autostart::is_enabled(&r.key), "the Run value was written (test key)");
        assert!(r.cfg().get_autostart_granted());

        r.click(937.0, 654.0); // Lanjut -> Tanya AI (opsional)
        assert_eq!(r.cfg().get_step(), 4);
        r.click(937.0, 654.0); // Lanjut -> Siap
        assert_eq!(r.cfg().get_step(), 5);
        assert!(!r.config_exists() || !r.disk().onboarding.completed);
        r.click(904.0, 654.0); // Mulai pakai Moku
        let c = r.disk();
        assert!(c.onboarding.completed);
        assert_eq!(c.island.style, moku_core::config::IslandStyle::Notch);
        assert_eq!(c.appearance.material, moku_core::config::Material::Black);
        assert!(c.general.autostart);
        assert!(!r.ui.window().is_visible(), "first-run onboarding closes the window");
    }

    fn key(r: &Rig, text: impl Into<slint::SharedString>) {
        let text = text.into();
        r.ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        r.ui.window().dispatch_event(WindowEvent::KeyReleased { text });
    }

    #[test]
    fn ui_typing_a_hotkey_validates_live_and_saves_only_valid_ones() {
        let r = Rig::new("typing", None, false);
        r.cfg().set_page(6); // Pintasan
        r.open();
        r.click(855.0, 168.0); // the "Buka atau tutup island" field gets the focus
        key(&r, slint::platform::Key::End);
        for _ in 0..12 {
            key(&r, slint::platform::Key::Backspace);
        }
        assert_eq!(r.cfg().get_f().general_hotkey_toggle, "");
        assert!(!r.cfg().get_problems().hotkey_toggle.is_empty(), "an empty box is not a hotkey");

        for ch in "Ctrl+Alt+K".chars() {
            key(&r, ch.to_string());
        }
        assert_eq!(r.cfg().get_f().general_hotkey_toggle, "Ctrl+Alt+K");
        assert!(r.cfg().get_problems().hotkey_toggle.is_empty());
        advance(350);
        assert_eq!(r.disk().general.hotkey_toggle, "Ctrl+Alt+K");

        // Keep typing something that is not a hotkey: the file keeps the last valid one.
        for ch in "+Nope".chars() {
            key(&r, ch.to_string());
        }
        assert!(!r.cfg().get_problems().hotkey_toggle.is_empty());
        advance(350);
        assert_eq!(r.disk().general.hotkey_toggle, "Ctrl+Alt+K");
    }}
