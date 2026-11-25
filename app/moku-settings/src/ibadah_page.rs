//! The ibadah controls of the "Fitur" page (v0.3, track N): calculation method, Hijri date,
//! Ramadan, puasa sunnah and adzan sound. They are bound to the `IbadahCfg` global of
//! `ui/pages-n.slint`, outside `Form`: this module reads and writes `[ibadah]` and
//! `prayer.method` of config.toml itself with a read-modify-write (`Config::update_file`), so it
//! never overwrites what the main form saved, and saves 400 ms after the last edit.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use moku_core::audiofile;
use moku_core::config::{AdzanSound, CountdownPill, PrayerMethod};
use moku_core::Config;
use slint::ComponentHandle;

use crate::{IbadahCfg, SettingsWindow};

/// The values of the controls.
#[derive(Clone, Debug, PartialEq)]
pub struct Values {
    pub method: i32,
    pub hijri: bool,
    pub hijri_offset: i32,
    pub ramadan: bool,
    pub pill_mode: i32,
    pub imsak_minutes: i32,
    pub sunnah_mt: bool,
    pub sunnah_bidh: bool,
    pub sunnah_arafah: bool,
    pub sunnah_asyura: bool,
    pub sunnah_hour: i32,
    pub adzan: bool,
    pub adzan_prayers: [bool; 5],
    pub sound: i32,
    pub file: String,
    pub volume: i32,
    pub pause_media: bool,
}

impl Values {
    pub fn from_config(c: &Config) -> Values {
        let i = &c.ibadah;
        Values {
            method: match c.prayer.method {
                PrayerMethod::Kemenag => 0,
                PrayerMethod::Jakim => 1,
            },
            hijri: i.hijri,
            hijri_offset: i.hijri_offset,
            ramadan: i.ramadan,
            pill_mode: match i.countdown_pill {
                CountdownPill::Always => 0,
                CountdownPill::Last30 => 1,
                CountdownPill::Off => 2,
            },
            imsak_minutes: i.imsak_minutes as i32,
            sunnah_mt: i.sunnah_monday_thursday,
            sunnah_bidh: i.sunnah_ayyamul_bidh,
            sunnah_arafah: i.sunnah_arafah,
            sunnah_asyura: i.sunnah_asyura,
            sunnah_hour: i.sunnah_hour as i32,
            adzan: i.adzan,
            adzan_prayers: [i.adzan_fajr, i.adzan_dhuhr, i.adzan_asr, i.adzan_maghrib, i.adzan_isha],
            sound: match i.adzan_sound {
                AdzanSound::Chime => 0,
                AdzanSound::File => 1,
            },
            file: i.adzan_file.clone(),
            volume: i.adzan_volume as i32,
            pause_media: i.adzan_pause_media,
        }
    }

    /// Put the values into a config; out-of-range numbers are clamped to what the island accepts.
    pub fn apply(&self, c: &mut Config) {
        c.prayer.method = if self.method == 1 { PrayerMethod::Jakim } else { PrayerMethod::Kemenag };
        let i = &mut c.ibadah;
        i.hijri = self.hijri;
        i.hijri_offset = self.hijri_offset.clamp(-2, 2);
        i.ramadan = self.ramadan;
        i.countdown_pill = match self.pill_mode {
            1 => CountdownPill::Last30,
            2 => CountdownPill::Off,
            _ => CountdownPill::Always,
        };
        i.imsak_minutes = self.imsak_minutes.clamp(0, 30) as u32;
        i.sunnah_monday_thursday = self.sunnah_mt;
        i.sunnah_ayyamul_bidh = self.sunnah_bidh;
        i.sunnah_arafah = self.sunnah_arafah;
        i.sunnah_asyura = self.sunnah_asyura;
        i.sunnah_hour = self.sunnah_hour.clamp(12, 23) as u32;
        i.adzan = self.adzan;
        [i.adzan_fajr, i.adzan_dhuhr, i.adzan_asr, i.adzan_maghrib, i.adzan_isha] = self.adzan_prayers;
        i.adzan_sound = if self.sound == 1 { AdzanSound::File } else { AdzanSound::Chime };
        // An over-long path is not saved (the island would drop the whole field).
        if self.file.chars().count() <= moku_core::config::MAX_AUDIO_PATH_CHARS {
            i.adzan_file = self.file.trim().to_string();
        }
        i.adzan_volume = self.volume.clamp(5, 100) as u32;
        i.adzan_pause_media = self.pause_media;
    }
}

/// Why the chosen file will not be used (Indonesian), or empty: fine, or the chime is chosen.
pub fn file_problem(v: &Values) -> String {
    if v.sound != 1 {
        return String::new();
    }
    let path = PathBuf::from(v.file.trim().trim_matches('"').trim());
    audiofile::check(&path).err().map(|p| p.text().to_string()).unwrap_or_default()
}

fn read(ui: &SettingsWindow) -> Values {
    let g = ui.global::<IbadahCfg>();
    Values {
        method: g.get_method(),
        hijri: g.get_hijri(),
        hijri_offset: g.get_hijri_offset(),
        ramadan: g.get_ramadan(),
        pill_mode: g.get_pill_mode(),
        imsak_minutes: g.get_imsak_minutes(),
        sunnah_mt: g.get_sunnah_mt(),
        sunnah_bidh: g.get_sunnah_bidh(),
        sunnah_arafah: g.get_sunnah_arafah(),
        sunnah_asyura: g.get_sunnah_asyura(),
        sunnah_hour: g.get_sunnah_hour(),
        adzan: g.get_adzan(),
        adzan_prayers: [g.get_adzan_fajr(), g.get_adzan_dhuhr(), g.get_adzan_asr(), g.get_adzan_maghrib(), g.get_adzan_isha()],
        sound: g.get_sound(),
        file: g.get_file().to_string(),
        volume: g.get_volume(),
        pause_media: g.get_pause_media(),
    }
}

fn show(ui: &SettingsWindow, v: &Values) {
    let g = ui.global::<IbadahCfg>();
    g.set_method(v.method);
    g.set_hijri(v.hijri);
    g.set_hijri_offset(v.hijri_offset);
    g.set_ramadan(v.ramadan);
    g.set_pill_mode(v.pill_mode);
    g.set_imsak_minutes(v.imsak_minutes);
    g.set_sunnah_mt(v.sunnah_mt);
    g.set_sunnah_bidh(v.sunnah_bidh);
    g.set_sunnah_arafah(v.sunnah_arafah);
    g.set_sunnah_asyura(v.sunnah_asyura);
    g.set_sunnah_hour(v.sunnah_hour);
    g.set_adzan(v.adzan);
    g.set_adzan_fajr(v.adzan_prayers[0]);
    g.set_adzan_dhuhr(v.adzan_prayers[1]);
    g.set_adzan_asr(v.adzan_prayers[2]);
    g.set_adzan_maghrib(v.adzan_prayers[3]);
    g.set_adzan_isha(v.adzan_prayers[4]);
    g.set_sound(v.sound);
    g.set_file(v.file.clone().into());
    g.set_volume(v.volume);
    g.set_pause_media(v.pause_media);
    g.set_file_problem(file_problem(v).into());
}

/// Fill the section from `config.toml` and save its edits.
pub fn wire(ui: &SettingsWindow, path: PathBuf) {
    let last = Rc::new(RefCell::new(Config::load_from(&path).0));
    show(ui, &Values::from_config(&last.borrow()));
    let g = ui.global::<IbadahCfg>();

    g.on_reload({
        let (ui, last, path) = (ui.as_weak(), last.clone(), path.clone());
        move || {
            *last.borrow_mut() = Config::load_from(&path).0;
            if let Some(ui) = ui.upgrade() {
                show(&ui, &Values::from_config(&last.borrow()));
            }
        }
    });

    let timer = Rc::new(slint::Timer::default());
    g.on_changed({
        let (weak, last, path, timer) = (ui.as_weak(), last.clone(), path.clone(), timer.clone());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let v = read(&ui);
            ui.global::<IbadahCfg>().set_file_problem(file_problem(&v).into());
            let (weak, last, path) = (weak.clone(), last.clone(), path.clone());
            timer.start(slint::TimerMode::SingleShot, Duration::from_millis(400), move || {
                let Some(ui) = weak.upgrade() else { return };
                let v = read(&ui);
                let fallback = last.borrow().clone();
                match Config::update_file(&path, &fallback, |c| v.apply(c)) {
                    Ok(c) => *last.borrow_mut() = c,
                    Err(e) => eprintln!("ibadah settings: {e}"),
                }
            });
        }
    });

    g.on_pick_file({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            if let Some(p) = pick_audio_file() {
                ui.global::<IbadahCfg>().set_file(p.into());
                ui.global::<IbadahCfg>().invoke_changed();
            }
        }
    });
}

/// The standard "Open" dialog, audio files only. `None` when cancelled.
fn pick_audio_file() -> Option<String> {
    use windows::Win32::UI::Controls::Dialogs::{GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW};
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    use windows::core::{PCWSTR, PWSTR};

    // "Audio\0*.wav;*.mp3;...\0All\0*.*\0\0"
    let exts: Vec<String> = audiofile::EXTENSIONS.iter().map(|e| format!("*.{e}")).collect();
    let filter: Vec<u16> = format!("Audio\0{}\0\0", exts.join(";")).encode_utf16().collect();
    let mut buf = vec![0u16; 1024];
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: unsafe { GetForegroundWindow() },
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    if !unsafe { GetOpenFileNameW(&mut ofn) }.as_bool() {
        return None;
    }
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_the_controls() {
        let c = Config::default();
        let v = Values::from_config(&c);
        let mut back = c.clone();
        v.apply(&mut back);
        assert_eq!(back, c, "reading and applying the defaults changes nothing");
    }

    #[test]
    fn every_control_reaches_the_config() {
        let mut c = Config::default();
        let v = Values {
            method: 1,
            hijri: false,
            hijri_offset: -2,
            ramadan: false,
            pill_mode: 1,
            imsak_minutes: 0,
            sunnah_mt: true,
            sunnah_bidh: true,
            sunnah_arafah: true,
            sunnah_asyura: true,
            sunnah_hour: 21,
            adzan: true,
            adzan_prayers: [false, true, false, true, false],
            sound: 1,
            file: "  C:\\Sounds\\adzan.mp3 ".into(),
            volume: 55,
            pause_media: false,
        };
        v.apply(&mut c);
        assert_eq!(c.prayer.method, PrayerMethod::Jakim);
        let i = &c.ibadah;
        assert_eq!((i.hijri, i.hijri_offset, i.ramadan, i.countdown_pill, i.imsak_minutes), (false, -2, false, CountdownPill::Last30, 0));
        assert_eq!((i.sunnah_monday_thursday, i.sunnah_ayyamul_bidh, i.sunnah_arafah, i.sunnah_asyura, i.sunnah_hour), (true, true, true, true, 21));
        assert_eq!((i.adzan, i.adzan_fajr, i.adzan_dhuhr, i.adzan_asr, i.adzan_maghrib, i.adzan_isha), (true, false, true, false, true, false));
        assert_eq!((i.adzan_sound, i.adzan_file.as_str(), i.adzan_volume, i.adzan_pause_media), (AdzanSound::File, "C:\\Sounds\\adzan.mp3", 55, false));
        // And back.
        assert_eq!(Values::from_config(&c).file, "C:\\Sounds\\adzan.mp3");
        assert_eq!(Values::from_config(&c).adzan_prayers, [false, true, false, true, false]);
    }

    #[test]
    fn out_of_range_values_are_clamped_and_the_saved_file_stays_valid() {
        let mut c = Config::default();
        let mut v = Values::from_config(&c);
        (v.hijri_offset, v.imsak_minutes, v.sunnah_hour, v.volume) = (9, 99, 3, 500);
        v.apply(&mut c);
        assert_eq!((c.ibadah.hijri_offset, c.ibadah.imsak_minutes, c.ibadah.sunnah_hour, c.ibadah.adzan_volume), (2, 30, 12, 100));
        // The saved text still parses without warnings (nothing falls back).
        let (back, warnings) = Config::parse(&c.to_toml());
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(back, c);
        // An over-long path is not stored.
        v.file = "a".repeat(moku_core::config::MAX_AUDIO_PATH_CHARS + 1);
        v.apply(&mut c);
        assert!(c.ibadah.adzan_file.is_empty());
    }

    // ---- the real window and `wire`, headless (a clock the test advances by hand) ----

    thread_local! {
        static NOW: std::cell::Cell<Duration> = const { std::cell::Cell::new(Duration::ZERO) };
    }

    struct Headless(Rc<slint::platform::software_renderer::MinimalSoftwareWindow>);

    impl slint::platform::Platform for Headless {
        fn create_window_adapter(&self) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
        fn duration_since_start(&self) -> Duration {
            NOW.with(|n| n.get())
        }
    }

    fn advance(ms: u64) {
        NOW.with(|n| n.set(n.get() + Duration::from_millis(ms)));
        slint::platform::update_timers_and_animations();
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moku-settings-ibadah-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_window_shows_the_file_saves_edits_after_the_debounce_and_reloads() {
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        // set_platform is per thread (every #[test] runs on its own thread).
        let win = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(win))).unwrap();
        NOW.with(|n| n.set(Duration::ZERO));
        let dir = temp_dir("wire");
        let path = dir.join("config.toml");
        let mut seed = Config::default();
        seed.prayer.method = PrayerMethod::Jakim;
        seed.ibadah.hijri_offset = 1;
        seed.ibadah.adzan_sound = AdzanSound::File;
        seed.ibadah.adzan_file = r"C:\definitely\missing\adzan.mp3".into();
        seed.save_to(&path).unwrap();

        let ui = SettingsWindow::new().unwrap();
        wire(&ui, path.clone());
        let g = ui.global::<IbadahCfg>();
        // The window shows what the file says, and why the chosen file will not be used.
        assert_eq!((g.get_method(), g.get_hijri_offset(), g.get_sound()), (1, 1, 1));
        assert!(g.get_file_problem().contains("tidak ditemukan"), "{}", g.get_file_problem());

        // Edits are saved 400 ms after the last one, and only the ibadah keys change.
        g.set_adzan(true);
        g.set_sunnah_mt(true);
        g.set_method(0);
        g.invoke_changed();
        advance(300);
        assert!(!Config::load_from(&path).0.ibadah.adzan, "not saved yet: 300 ms passed");
        g.set_volume(70);
        g.invoke_changed();
        advance(300);
        assert!(!Config::load_from(&path).0.ibadah.adzan, "a second edit restarts the wait");
        advance(150);
        let saved = Config::load_from(&path).0;
        assert!(saved.ibadah.adzan && saved.ibadah.sunnah_monday_thursday);
        assert_eq!((saved.prayer.method, saved.ibadah.adzan_volume, saved.ibadah.hijri_offset), (PrayerMethod::Kemenag, 70, 1));

        // Picking the chime hides the complaint about the file.
        g.set_sound(0);
        g.invoke_changed();
        assert_eq!(g.get_file_problem().as_str(), "");

        // Somebody else (the main form, a hand edit, "Reset ke awal") rewrote the file: reload shows it.
        let mut other = Config::load_from(&path).0;
        other.ibadah.imsak_minutes = 5;
        other.ibadah.countdown_pill = CountdownPill::Off;
        other.save_to(&path).unwrap();
        g.invoke_reload();
        assert_eq!((g.get_imsak_minutes(), g.get_pill_mode()), (5, 2));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_file_problem_is_shown_only_for_a_chosen_file() {
        let mut v = Values::from_config(&Config::default());
        v.file = "C:\\definitely\\missing.mp3".into();
        assert_eq!(file_problem(&v), "", "the chime is chosen: nothing to complain about");
        v.sound = 1;
        assert!(file_problem(&v).contains("tidak ditemukan"));
        v.file = String::new();
        assert!(file_problem(&v).contains("Belum ada file"));
    }
}
