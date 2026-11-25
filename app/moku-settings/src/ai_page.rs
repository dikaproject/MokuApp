//! v0.3 track M: the "Tanya AI" and "Memori & Privasi" pages (`ui/pages-m.slint`, global
//! `AiCfg`) <-> `[ai]` of `config.toml`, the API key in Windows Credential Manager, and the
//! encrypted memory file `ai-memory.bin`.
//!
//! * `[ai]` is saved with a read-modify-write (`Config::update_file`), debounced, so it never
//!   overwrites what the main form saved. Values that cannot be used (a base URL that would send
//!   the key over plain http to another host, a hotkey that is taken) are shown as a problem and
//!   not saved.
//! * The API key goes straight from the field into Credential Manager (`moku_core::secrets`,
//!   one key per provider host) and is never shown again or written anywhere else.
//! * The memory is read and written through `moku_core::aimem`; this window never caches it, so
//!   the island and this window always agree. A file that cannot be opened is reported, kept as
//!   it is, and only an explicit "Hapus semua" removes it.
//! * "Tes koneksi" asks the provider for its model list (no tokens used) with the shared WinHTTP
//!   client (`net.rs` of the island, included as is).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use moku_core::config::Ai;
use moku_core::hotkey::{self, Hotkey, Slot};
use moku_core::{Config, ai, aimem, secrets};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{AiCfg, AiFact, SettingsWindow};

/// The values of the two pages.
#[derive(Clone, Debug, PartialEq)]
pub struct Values {
    pub enabled: bool,
    pub hotkey: String,
    pub copy_fallback: bool,
    pub preset: i32,
    pub base_url: String,
    pub model: String,
    pub ctx_todos: bool,
    pub ctx_calendar: bool,
    pub ctx_timer: bool,
    pub ctx_weather: bool,
    pub send_all_local: bool,
    pub redact: bool,
    pub memory_paused: bool,
    pub history: bool,
    /// 0 = 7 days, 1 = 30 days.
    pub retention: i32,
}

/// The index of the "Kustom" chip (after the presets).
pub const CUSTOM: i32 = ai::PRESETS.len() as i32;

impl Values {
    pub fn from_config(c: &Config) -> Values {
        let a = &c.ai;
        let preset = ai::PRESETS.iter().position(|p| p.id == a.preset).map(|i| i as i32).unwrap_or(CUSTOM);
        Values {
            enabled: a.enabled,
            hotkey: a.hotkey.clone(),
            copy_fallback: a.copy_fallback,
            preset,
            base_url: a.base_url.clone(),
            model: a.model.clone(),
            ctx_todos: a.ctx_todos,
            ctx_calendar: a.ctx_calendar,
            ctx_timer: a.ctx_timer,
            ctx_weather: a.ctx_weather,
            send_all_local: a.send_all_local,
            redact: a.redact,
            memory_paused: a.memory_paused,
            history: a.history,
            retention: if a.retention_days >= 30 { 1 } else { 0 },
        }
    }
}

/// Why a value cannot be used (Indonesian), or `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Problems {
    pub base_url: Option<String>,
    pub hotkey: Option<String>,
    pub model: Option<String>,
}

pub fn problems(v: &Values, cfg: &Config) -> Problems {
    let mut p = Problems::default();
    if let Err(e) = ai::parse_base_url(&v.base_url) {
        p.base_url = Some(e);
    }
    match Hotkey::parse(v.hotkey.trim()) {
        Err(_) => p.hotkey = Some("Pakai Ctrl, Alt, Shift, atau Win, lalu satu tombol".into()),
        // Another feature (panel, quit, launcher, OCR) already uses this combination.
        Ok(_) => p.hotkey = hotkey::clash(cfg, Slot::Ai, v.hotkey.trim()).map(hotkey::clash_message),
    }
    if v.model.trim().is_empty() || v.model.len() > 200 || v.model.chars().any(|c| c.is_control()) {
        p.model = Some("Isi nama model".into());
    }
    p
}

/// Apply the page to a config. A value with a problem keeps its old setting.
pub fn apply(v: &Values, cfg: &mut Config) {
    let p = problems(v, cfg);
    let a: &mut Ai = &mut cfg.ai;
    a.enabled = v.enabled;
    a.copy_fallback = v.copy_fallback;
    a.ctx_todos = v.ctx_todos;
    a.ctx_calendar = v.ctx_calendar;
    a.ctx_timer = v.ctx_timer;
    a.ctx_weather = v.ctx_weather;
    a.send_all_local = v.send_all_local;
    a.redact = v.redact;
    a.memory_paused = v.memory_paused;
    a.history = v.history;
    a.retention_days = if v.retention == 1 { 30 } else { 7 };
    if p.base_url.is_none() {
        a.base_url = v.base_url.trim().to_string();
        // The preset follows the address: a chip is "on" only while the address is its own.
        a.preset = ai::preset_for_url(&a.base_url).map(|p| p.id.to_string()).unwrap_or_else(|| "custom".into());
    }
    if p.model.is_none() {
        a.model = v.model.trim().to_string();
    }
    if p.hotkey.is_none() {
        a.hotkey = v.hotkey.trim().to_string();
    }
}

/// The preset index for an address (the "Kustom" chip when it is none of them).
pub fn preset_index(base_url: &str) -> i32 {
    ai::preset_for_url(base_url).and_then(|p| ai::PRESETS.iter().position(|x| x.id == p.id)).map(|i| i as i32).unwrap_or(CUSTOM)
}

/// What the key field says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyStatus {
    pub text: String,
    pub has_key: bool,
}

pub fn key_status(base_url: &str) -> KeyStatus {
    let Some(name) = ai::key_name(base_url) else {
        return KeyStatus { text: "Isi alamat yang benar dulu, kunci disimpan per penyedia".into(), has_key: false };
    };
    let host = ai::host_label(base_url).unwrap_or_default();
    let has = secrets::exists(&name);
    let text = match (has, ai::is_local(base_url)) {
        (true, _) => format!("Tersimpan di Windows Credential Manager untuk {host} (Moku/{name}). Tidak ditampilkan lagi."),
        (false, true) => format!("Tidak perlu kunci untuk {host} di PC ini; isi hanya kalau servermu memintanya."),
        (false, false) => format!("Belum ada kunci untuk {host}. Setiap penyedia punya kunci sendiri."),
    };
    KeyStatus { text, has_key: has }
}

// ---- the model (no window) ---------------------------------------------------------------------

pub struct Model {
    config_path: PathBuf,
    mem_path: PathBuf,
    last: Config,
}

/// The memory as the page shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct MemView {
    pub status: aimem::Status,
    pub facts: Vec<aimem::Fact>,
    pub history: usize,
}

impl Model {
    pub fn open(config_path: PathBuf, mem_path: PathBuf) -> Model {
        let last = Config::load_from(&config_path).0;
        Model { config_path, mem_path, last }
    }

    pub fn reload(&mut self) {
        self.last = Config::load_from(&self.config_path).0;
    }

    pub fn config(&self) -> &Config {
        &self.last
    }

    pub fn save(&mut self, v: &Values) -> Result<(), String> {
        let cfg = Config::update_file(&self.config_path, &self.last, |c| apply(v, c)).map_err(|e| format!("Gagal menyimpan: {e}"))?;
        self.last = cfg;
        Ok(())
    }

    // ---- the key ----

    pub fn save_key(&self, base_url: &str, key: &str) -> Result<(), String> {
        let name = ai::key_name(base_url).ok_or("Isi alamat penyedia yang benar dulu")?;
        let key = key.trim();
        if key.is_empty() {
            return Err("Tempel kunci dulu".into());
        }
        if key.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err("Kunci tidak boleh berisi spasi atau baris baru".into());
        }
        if key.encode_utf16().count() > secrets::MAX_UNITS {
            return Err("Kunci terlalu panjang".into());
        }
        secrets::write(&name, key).map_err(|_| "Gagal menyimpan ke Windows Credential Manager".to_string())
    }

    pub fn delete_key(&self, base_url: &str) -> Result<bool, String> {
        let name = ai::key_name(base_url).ok_or("Alamat penyedia tidak valid")?;
        secrets::delete(&name).map_err(|_| "Gagal menghapus kunci".to_string())
    }

    // ---- the memory ----

    /// Read the memory. History past its retention is swept away here too (the island does it on
    /// every write; this covers a PC where it has not written for a while).
    pub fn memory(&self, now: i64) -> MemView {
        match aimem::load(&self.mem_path) {
            Ok(mut m) => {
                let days = self.last.ai.retention_days;
                if m.history.iter().any(|e| e.ts < now - days as i64 * 86_400) && aimem::update(&self.mem_path, |mm| mm.sweep(now, days)).is_ok() {
                    m = aimem::load(&self.mem_path).unwrap_or(m);
                }
                MemView { status: aimem::Status::Ready { facts: m.facts.len(), history: m.history.len() }, history: m.history.len(), facts: m.facts }
            }
            Err(aimem::LoadError::Missing) => MemView { status: aimem::Status::Empty, facts: vec![], history: 0 },
            Err(aimem::LoadError::Unavailable(w)) => MemView { status: aimem::Status::Unavailable(w), facts: vec![], history: 0 },
        }
    }

    fn fact_id(&self, index: usize) -> Result<u64, String> {
        aimem::load(&self.mem_path).ok().and_then(|m| m.facts.get(index).map(|f| f.id)).ok_or_else(|| "Catatan itu sudah tidak ada".to_string())
    }

    pub fn add_fact(&self, text: &str, now: i64) -> Result<(), String> {
        if self.last.ai.memory_paused {
            return Err("Memori sedang dijeda".into());
        }
        match aimem::update(&self.mem_path, |m| m.add_fact(text, now))? {
            Ok(_) => Ok(()),
            Err(e) => Err(e.message().into()),
        }
    }

    pub fn edit_fact(&self, index: usize, text: &str) -> Result<(), String> {
        let id = self.fact_id(index)?;
        match aimem::update(&self.mem_path, |m| m.edit_fact(id, text))? {
            Ok(()) => Ok(()),
            Err(e) => Err(e.message().into()),
        }
    }

    pub fn delete_fact(&self, index: usize) -> Result<(), String> {
        let id = self.fact_id(index)?;
        aimem::update(&self.mem_path, |m| m.delete_fact(id)).map(|_| ())
    }

    pub fn clear_history(&self) -> Result<(), String> {
        aimem::update(&self.mem_path, |m| m.clear_history())
    }

    /// "Hapus semua": the memory file goes, readable or not.
    pub fn wipe(&self) -> Result<(), String> {
        aimem::delete_file(&self.mem_path).map(|_| ())
    }

    /// Write the export (plain JSON) into `dir`; returns the file.
    pub fn export(&self, dir: &Path, now: i64) -> Result<PathBuf, String> {
        let m = aimem::load_or_new(&self.mem_path).map_err(|_| "Memori tidak bisa dibuka, jadi tidak ada yang bisa diekspor".to_string())?;
        std::fs::create_dir_all(dir).map_err(|e| format!("Folder tujuan tidak bisa dibuat ({})", e.kind()))?;
        let stamp = stamp_of(now);
        let mut path = dir.join(format!("Moku-memori-{stamp}.json"));
        let mut n = 2;
        while path.exists() {
            path = dir.join(format!("Moku-memori-{stamp}-{n}.json"));
            n += 1;
        }
        std::fs::write(&path, m.export_json(now)).map_err(|e| format!("Gagal menulis file ({})", e.kind()))?;
        Ok(path)
    }
}

// ---- dates ---------------------------------------------------------------------------------------

/// (year, month 1-12, day) of a day count since 1970-01-01 (Howard Hinnant's civil-from-days).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Seconds to add to UTC to get this PC's local time (the Windows time-zone setting).
fn local_offset_secs() -> i64 {
    use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    let mut tzi = TIME_ZONE_INFORMATION::default();
    let id = unsafe { GetTimeZoneInformation(&mut tzi) };
    let extra = match id {
        1 => tzi.StandardBias,
        2 => tzi.DaylightBias,
        _ => 0,
    };
    -((tzi.Bias + extra) as i64) * 60
}

/// `25 Nov 2026` in local time.
pub fn date_label(ts: i64) -> String {
    const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "Mei", "Jun", "Jul", "Agu", "Sep", "Okt", "Nov", "Des"];
    let (y, m, d) = civil((ts + local_offset_secs()).div_euclid(86_400));
    format!("{d} {} {y}", M[(m as usize - 1).min(11)])
}

/// `20261125-143005` in local time (file names).
pub fn stamp_of(ts: i64) -> String {
    let local = ts + local_offset_secs();
    let (y, m, d) = civil(local.div_euclid(86_400));
    let s = local.rem_euclid(86_400);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", s / 3600, s % 3600 / 60, s % 60)
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// The Downloads folder (`MOKU_EXPORT_DIR` overrides it, for tests).
fn downloads_dir() -> (PathBuf, bool) {
    if let Some(d) = std::env::var_os("MOKU_EXPORT_DIR").filter(|d| !d.is_empty()) {
        return (PathBuf::from(d), true);
    }
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    let known = unsafe {
        let r = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None);
        r.ok().map(|p| {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as *const _));
            s
        })
    };
    let dir = known.filter(|s| !s.is_empty()).map(PathBuf::from).or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Downloads"))).unwrap_or_else(std::env::temp_dir);
    (dir, false)
}

// ---- the connection test --------------------------------------------------------------------------

/// Ask the provider for its model list. `Ok` is the message to show, `Err` why not.
pub fn test_connection(cfg: &Config) -> Result<String, String> {
    use crate::net::{Event, Options};
    let a = &cfg.ai;
    let url = ai::models_url(&a.base_url)?;
    let host = ai::host_label(&a.base_url).unwrap_or_default();
    let key = ai::key_name(&a.base_url).and_then(|n| secrets::read(&n).ok().flatten());
    if key.is_none() && !ai::is_local(&a.base_url) {
        return Err(format!("Belum ada kunci untuk {host}: isi dan Simpan dulu"));
    }
    let auth = key.as_ref().map(|k| format!("Bearer {k}"));
    let mut headers: Vec<(&str, &str)> = vec![("Accept", "application/json")];
    if let Some(a) = &auth {
        headers.push(("Authorization", a.as_str()));
    }
    let opts = Options {
        method: "GET",
        headers: &headers,
        body: None,
        connect_timeout: Duration::from_secs(10),
        read_timeout: Duration::from_secs(15),
        total_timeout: Some(Duration::from_secs(25)),
        // The key must never follow a redirect to another host.
        follow_redirects: false,
        cancel: None,
    };
    let mut body: Vec<u8> = vec![];
    let status = crate::net::request(&url, &opts, &mut |e| {
        if let Event::Data(d) = e {
            if body.len() < 1 << 20 {
                body.extend_from_slice(d);
            }
        }
        true
    })
    .map_err(|e| if e == crate::net::TIMED_OUT { "Penyedia terlalu lama menjawab".to_string() } else { format!("Tidak bisa terhubung ke {host}. Cek internet dan alamatnya") })?;
    if !(200..300).contains(&status) {
        return Err(ai::status_message(status).to_string());
    }
    let ids = ai::parse_model_ids(&String::from_utf8_lossy(&body));
    let n = ids.len();
    Ok(match (n, ai::model_listed(&ids, &a.model)) {
        (0, _) => format!("Tersambung ke {host}"),
        (n, true) => format!("Tersambung ke {host} · {n} model · \"{}\" tersedia", a.model),
        (n, false) => format!("Tersambung ke {host} · {n} model, tetapi \"{}\" tidak ada di daftar (cek nama model)", a.model),
    })
}

// ---- the window --------------------------------------------------------------------------------

fn read(ui: &SettingsWindow) -> Values {
    let g = ui.global::<AiCfg>();
    Values {
        enabled: g.get_enabled(),
        hotkey: g.get_hotkey().to_string(),
        copy_fallback: g.get_copy_fallback(),
        preset: g.get_preset(),
        base_url: g.get_base_url().to_string(),
        model: g.get_model().to_string(),
        ctx_todos: g.get_ctx_todos(),
        ctx_calendar: g.get_ctx_calendar(),
        ctx_timer: g.get_ctx_timer(),
        ctx_weather: g.get_ctx_weather(),
        send_all_local: g.get_send_all_local(),
        redact: g.get_redact(),
        memory_paused: g.get_memory_paused(),
        history: g.get_history(),
        retention: g.get_retention(),
    }
}

fn presets_rows() -> (Vec<slint::SharedString>, Vec<slint::SharedString>) {
    let mut names: Vec<slint::SharedString> = ai::PRESETS.iter().map(|p| p.name.into()).collect();
    names.push("Kustom".into());
    let b = names.split_off(4);
    (names, b)
}

fn show_provider(ui: &SettingsWindow, v: &Values, cfg: &Config) {
    let g = ui.global::<AiCfg>();
    let p = problems(v, cfg);
    g.set_url_problem(p.base_url.clone().unwrap_or_default().into());
    g.set_hotkey_problem(p.hotkey.clone().unwrap_or_default().into());
    g.set_preset(preset_index(&v.base_url));
    let usable = p.base_url.is_none();
    g.set_host(if usable { ai::host_label(&v.base_url).unwrap_or_default() } else { "(alamat belum benar)".into() }.into());
    g.set_local(usable && ai::is_local(&v.base_url));
    let ks = key_status(&v.base_url);
    g.set_key_status(ks.text.into());
    g.set_has_key(ks.has_key);
}

fn show_memory(ui: &SettingsWindow, m: &Model) {
    let g = ui.global::<AiCfg>();
    let view = m.memory(now());
    let facts: Vec<AiFact> = view.facts.iter().map(|f| AiFact { text: f.text.clone().into(), date: date_label(f.ts).into() }).collect();
    g.set_facts(ModelRc::new(VecModel::from(facts)));
    g.set_history_count(view.history as i32);
    match &view.status {
        aimem::Status::Empty => {
            g.set_memory_broken(false);
            g.set_memory_status("Memori masih kosong.".into());
        }
        aimem::Status::Ready { facts, history } => {
            g.set_memory_broken(false);
            g.set_memory_status(format!("{facts} catatan dan {history} riwayat, terenkripsi di PC ini.").into());
        }
        aimem::Status::Unavailable(_) => {
            g.set_memory_broken(true);
            g.set_memory_problem("Tidak bisa dibuka (rusak, diubah, atau dibuat oleh akun Windows lain). Filenya tidak diubah.".into());
        }
    }
}

fn show_all(ui: &SettingsWindow, m: &Model) {
    let g = ui.global::<AiCfg>();
    let cfg = m.config();
    let v = Values::from_config(cfg);
    g.set_enabled(v.enabled);
    g.set_hotkey(v.hotkey.clone().into());
    g.set_copy_fallback(v.copy_fallback);
    g.set_preset(preset_index(&v.base_url));
    g.set_base_url(v.base_url.clone().into());
    g.set_model(v.model.clone().into());
    g.set_ctx_todos(v.ctx_todos);
    g.set_ctx_calendar(v.ctx_calendar);
    g.set_ctx_timer(v.ctx_timer);
    g.set_ctx_weather(v.ctx_weather);
    g.set_send_all_local(v.send_all_local);
    g.set_redact(v.redact);
    g.set_memory_paused(v.memory_paused);
    g.set_history(v.history);
    g.set_retention(v.retention);
    show_provider(ui, &v, cfg);
    show_memory(ui, m);
}

/// Connect the pages to `config.toml`, Credential Manager and the memory file.
pub fn wire(ui: &SettingsWindow, config_path: PathBuf) {
    let model = Rc::new(RefCell::new(Model::open(config_path, aimem::default_path())));
    let g = ui.global::<AiCfg>();
    let (a, b) = presets_rows();
    g.set_presets_a(ModelRc::new(VecModel::from(a)));
    g.set_presets_b(ModelRc::new(VecModel::from(b)));
    g.set_note_id(ai::PRIVACY_NOTE_ID.into());
    g.set_note_en(ai::PRIVACY_NOTE_EN.into());
    show_all(ui, &model.borrow());

    let timer = Rc::new(slint::Timer::default());

    g.on_reload({
        let (model, ui) = (model.clone(), ui.as_weak());
        move || {
            model.borrow_mut().reload();
            if let Some(ui) = ui.upgrade() {
                let g = ui.global::<AiCfg>();
                g.set_key_problem("".into());
                g.set_confirm_wipe(false);
                g.set_editing(-1);
                show_all(&ui, &model.borrow());
            }
        }
    });

    // Any edit of the provider / feature / context: show the problems now, save a moment later.
    let save_soon = {
        let (model, timer, ui) = (model.clone(), timer.clone(), ui.as_weak());
        Rc::new(move || {
            let Some(w) = ui.upgrade() else { return };
            let v = read(&w);
            show_provider(&w, &v, model.borrow().config());
            let (model, ui) = (model.clone(), ui.clone());
            timer.start(slint::TimerMode::SingleShot, Duration::from_millis(400), move || {
                let Some(w) = ui.upgrade() else { return };
                let v = read(&w);
                if let Err(e) = model.borrow_mut().save(&v) {
                    eprintln!("ai settings: {e}");
                }
                show_provider(&w, &v, model.borrow().config());
            });
        })
    };
    g.on_changed({
        let s = save_soon.clone();
        move || s()
    });
    g.on_pick_preset({
        let (s, ui) = (save_soon.clone(), ui.as_weak());
        move |i| {
            let Some(w) = ui.upgrade() else { return };
            let g = w.global::<AiCfg>();
            g.set_preset(i);
            match ai::PRESETS.get(i.max(0) as usize) {
                Some(p) => {
                    g.set_base_url(p.base_url.into());
                    g.set_model(p.model.into());
                }
                // "Kustom": the address is typed by the user.
                None => g.set_base_url("".into()),
            }
            g.set_test_result("".into());
            s();
        }
    });

    g.on_save_key({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |text| {
            let Some(w) = ui.upgrade() else { return false };
            let g = w.global::<AiCfg>();
            let url = g.get_base_url().to_string();
            let r = model.borrow().save_key(&url, text.as_str());
            match r {
                Ok(()) => {
                    g.set_key_problem("".into());
                    let ks = key_status(&url);
                    g.set_key_status(ks.text.into());
                    g.set_has_key(ks.has_key);
                    true
                }
                Err(e) => {
                    g.set_key_problem(e.into());
                    false
                }
            }
        }
    });
    g.on_delete_key({
        let (model, ui) = (model.clone(), ui.as_weak());
        move || {
            let Some(w) = ui.upgrade() else { return };
            let g = w.global::<AiCfg>();
            let url = g.get_base_url().to_string();
            match model.borrow().delete_key(&url) {
                Ok(_) => g.set_key_problem("".into()),
                Err(e) => g.set_key_problem(e.into()),
            }
            let ks = key_status(&url);
            g.set_key_status(ks.text.into());
            g.set_has_key(ks.has_key);
        }
    });
    g.on_test({
        let (model, ui) = (model.clone(), ui.as_weak());
        move || {
            let Some(w) = ui.upgrade() else { return };
            let g = w.global::<AiCfg>();
            let v = read(&w);
            if let Some(e) = problems(&v, model.borrow().config()).base_url {
                g.set_test_ok(false);
                g.set_test_result(e.into());
                return;
            }
            // Test exactly what is on screen: save it first.
            if let Err(e) = model.borrow_mut().save(&v) {
                g.set_test_result(e.into());
                return;
            }
            g.set_testing(true);
            g.set_test_result("".into());
            let cfg = model.borrow().config().clone();
            let weak = ui.clone();
            let spawned = std::thread::Builder::new().name("ai-test".into()).spawn(move || {
                let r = test_connection(&cfg);
                let _ = weak.upgrade_in_event_loop(move |w| {
                    let g = w.global::<AiCfg>();
                    g.set_testing(false);
                    match r {
                        Ok(m) => {
                            g.set_test_ok(true);
                            g.set_test_result(m.into());
                        }
                        Err(e) => {
                            g.set_test_ok(false);
                            g.set_test_result(e.into());
                        }
                    }
                });
            });
            if spawned.is_err() {
                g.set_testing(false);
            }
        }
    });

    // The memory.
    let memory_changed = {
        let (model, ui) = (model.clone(), ui.as_weak());
        Rc::new(move |notice: String| {
            let Some(w) = ui.upgrade() else { return };
            show_memory(&w, &model.borrow());
            w.global::<AiCfg>().set_memory_notice(notice.into());
        })
    };
    g.on_add_fact({
        let (model, mc) = (model.clone(), memory_changed.clone());
        move |text| match model.borrow().add_fact(text.as_str(), now()) {
            Ok(()) => {
                mc(String::new());
                true
            }
            Err(e) => {
                mc(e);
                false
            }
        }
    });
    g.on_edit_fact({
        let (model, mc) = (model.clone(), memory_changed.clone());
        move |i, text| match model.borrow().edit_fact(i.max(0) as usize, text.as_str()) {
            Ok(()) => {
                mc(String::new());
                true
            }
            Err(e) => {
                mc(e);
                false
            }
        }
    });
    g.on_delete_fact({
        let (model, mc) = (model.clone(), memory_changed.clone());
        move |i| {
            let r = model.borrow().delete_fact(i.max(0) as usize);
            mc(r.err().unwrap_or_default());
        }
    });
    g.on_clear_history({
        let (model, mc) = (model.clone(), memory_changed.clone());
        move || {
            let r = model.borrow().clear_history();
            mc(r.err().unwrap_or_else(|| "Riwayat dihapus".into()));
        }
    });
    g.on_wipe_memory({
        let (model, mc) = (model.clone(), memory_changed.clone());
        move || {
            let r = model.borrow().wipe();
            mc(r.err().unwrap_or_else(|| "Semua memori dihapus".into()));
        }
    });
    g.on_export_memory({
        let (model, mc) = (model.clone(), memory_changed.clone());
        move || {
            let (dir, test_mode) = downloads_dir();
            match model.borrow().export(&dir, now()) {
                Ok(p) => {
                    mc(format!("Diekspor ke {}", p.display()));
                    if !test_mode {
                        // Show the file in Explorer.
                        let _ = std::process::Command::new("explorer.exe").arg(format!("/select,{}", p.display())).spawn();
                    }
                }
                Err(e) => mc(e),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn dirs(name: &str) -> (PathBuf, PathBuf) {
        let d = std::env::temp_dir().join(format!("moku-settings-ai-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        (d.join("config.toml"), d.join(aimem::FILE_NAME))
    }

    fn values() -> Values {
        Values::from_config(&Config::default())
    }

    /// The tests marked `#[ignore]` below use the real Windows Credential Manager (a test target,
    /// removed again by the `Key` guard) or DPAPI (the memory file). They are not CI-safe: run them
    /// on purpose with `MOKU_TEST_SYSTEM=1 cargo test -p moku-settings -- --ignored`.
    fn system_switch() {
        assert_eq!(std::env::var("MOKU_TEST_SYSTEM").as_deref(), Ok("1"), "set MOKU_TEST_SYSTEM=1 to run the tests that use the real Credential Manager or DPAPI");
    }

    /// A key target for a test host that is removed again when the guard drops.
    struct Key(String);

    impl Key {
        fn new() -> Key {
            static N: AtomicU32 = AtomicU32::new(0);
            Key(format!("https://moku-test-{}-{}.example.com/v1", std::process::id(), N.fetch_add(1, Ordering::Relaxed)))
        }
    }

    impl Drop for Key {
        fn drop(&mut self) {
            if let Some(n) = ai::key_name(&self.0) {
                let _ = secrets::delete(&n);
            }
        }
    }

    #[test]
    fn defaults_show_the_first_preset_and_a_round_trip_keeps_everything() {
        let c = Config::default();
        let v = Values::from_config(&c);
        assert_eq!((v.preset, v.enabled, v.retention), (0, false, 0));
        let mut c2 = Config::default();
        let mut w = v.clone();
        w.enabled = true;
        w.ctx_todos = true;
        w.history = true;
        w.retention = 1;
        w.memory_paused = true;
        w.redact = false;
        w.hotkey = " Ctrl+Alt+K ".into();
        apply(&w, &mut c2);
        let back = Values::from_config(&c2);
        assert_eq!((back.enabled, back.ctx_todos, back.history, back.retention, back.memory_paused, back.redact), (true, true, true, 1, true, false));
        assert_eq!(back.hotkey, "Ctrl+Alt+K");
    }

    #[test]
    fn presets_fill_the_address_and_the_chip_follows_the_address() {
        for (i, p) in ai::PRESETS.iter().enumerate() {
            assert_eq!(preset_index(p.base_url), i as i32, "{}", p.id);
        }
        assert_eq!(preset_index("https://gw.example.com/v1"), CUSTOM);
        let mut c = Config::default();
        let mut v = values();
        v.base_url = "http://localhost:11434/v1".into();
        v.model = "llama3.2".into();
        apply(&v, &mut c);
        assert_eq!((c.ai.preset.as_str(), c.ai.base_url.as_str()), ("ollama", "http://localhost:11434/v1"));
        v.base_url = "https://my-gateway.example.com/v1".into();
        apply(&v, &mut c);
        assert_eq!(c.ai.preset, "custom");
        let (a, b) = presets_rows();
        assert_eq!(a.len() + b.len(), ai::PRESETS.len() + 1);
        assert_eq!(b.last().map(|s| s.as_str()), Some("Kustom"));
    }

    #[test]
    fn unusable_values_are_reported_and_not_saved() {
        let c = Config::default();
        let mut v = values();
        v.base_url = "http://api.openai.com/v1".into();
        v.hotkey = "Alt+M".into();
        v.model = "  ".into();
        let p = problems(&v, &c);
        assert!(p.base_url.as_deref().is_some_and(|e| e.contains("localhost")), "{p:?}");
        assert_eq!(p.hotkey.as_deref(), Some("Sudah dipakai pintasan buka/tutup island"));
        assert!(p.model.is_some());
        let mut c2 = c.clone();
        v.enabled = true;
        apply(&v, &mut c2);
        assert!(c2.ai.enabled, "the valid part is kept");
        assert_eq!((c2.ai.base_url.as_str(), c2.ai.hotkey.as_str(), c2.ai.model.as_str()), ("https://api.openai.com/v1", "Ctrl+Alt+A", "gpt-4o-mini"), "the bad ones are not");
        v.hotkey = "Alt+Space".into();
        assert_eq!(problems(&v, &c).hotkey.as_deref(), Some("Sudah dipakai peluncur"));
        v.hotkey = "ctrl+alt+t".into();
        assert_eq!(problems(&v, &c).hotkey.as_deref(), Some("Sudah dipakai OCR"), "the OCR hotkey (on by default) is taken, in any spelling");
        v.hotkey = "Space".into();
        assert!(problems(&v, &c).hotkey.is_some());
        v.hotkey = "Ctrl+Alt+J".into();
        assert_eq!(problems(&v, &c).hotkey, None);
    }

    #[test]
    fn saving_keeps_edits_the_rest_of_the_window_made() {
        let (cfg_path, mem_path) = dirs("save");
        let mut m = Model::open(cfg_path.clone(), mem_path);
        let mut other = Config::default();
        other.island.scale = 1.25;
        other.save_to(&cfg_path).unwrap();
        let mut v = values();
        v.enabled = true;
        m.save(&v).unwrap();
        let disk = Config::load_from(&cfg_path).0;
        assert!(disk.ai.enabled);
        assert_eq!(disk.island.scale, 1.25, "the other edit survives");
    }

    #[test]
    #[ignore = "uses the real Windows Credential Manager or DPAPI: run with MOKU_TEST_SYSTEM=1 --ignored"]
    fn the_key_goes_to_credential_manager_per_host_and_never_into_the_config() {
        system_switch();
        let (cfg_path, mem_path) = dirs("key");
        let m = Model::open(cfg_path.clone(), mem_path);
        let k = Key::new();
        assert!(!key_status(&k.0).has_key);
        assert!(key_status(&k.0).text.contains("Belum ada kunci"));
        m.save_key(&k.0, "  sk-test-abcdef123456  ").unwrap();
        let st = key_status(&k.0);
        assert!(st.has_key && st.text.contains("Tersimpan") && !st.text.contains("sk-test"), "{} / read: {:?}", st.text, secrets::read(&ai::key_name(&k.0).unwrap()).map(|v| v.is_some()));
        assert_eq!(secrets::read(&ai::key_name(&k.0).unwrap()).unwrap().as_deref(), Some("sk-test-abcdef123456"), "trimmed");
        // Another host has no key: switching provider never reuses it.
        assert!(!key_status("https://openrouter.ai/api/v1").has_key || secrets::exists("ai/openrouter.ai"), "only a key the user really saved for that host");
        let other = Key::new();
        assert!(!key_status(&other.0).has_key);
        // The config file has no trace of it.
        let mut cfg = Config::default();
        apply(&values(), &mut cfg);
        cfg.save_to(&cfg_path).unwrap();
        assert!(!std::fs::read_to_string(&cfg_path).unwrap().contains("sk-test"));
        // Bad input is refused without touching the store.
        for bad in ["", "   ", "a b", "x\ny", &"k".repeat(secrets::MAX_UNITS + 1)] {
            assert!(m.save_key(&other.0, bad).is_err(), "{bad:.10}");
        }
        assert!(m.save_key("not a url", "sk-x").is_err());
        assert!(!key_status(&other.0).has_key);
        // Delete.
        assert_eq!(m.delete_key(&k.0), Ok(true));
        assert_eq!(m.delete_key(&k.0), Ok(false));
        assert!(!key_status(&k.0).has_key);
        // A local provider needs no key.
        assert!(key_status("http://localhost:11434/v1").text.contains("Tidak perlu kunci") || key_status("http://localhost:11434/v1").has_key);
    }

    #[test]
    #[ignore = "uses the real Windows Credential Manager or DPAPI: run with MOKU_TEST_SYSTEM=1 --ignored"]
    fn facts_can_be_added_edited_deleted_and_the_file_is_encrypted() {
        system_switch();
        let (cfg_path, mem_path) = dirs("facts");
        let m = Model::open(cfg_path, mem_path.clone());
        assert_eq!(m.memory(1_000).status, aimem::Status::Empty);
        m.add_fact("Panggil saya Rani", 100).unwrap();
        m.add_fact("Saya tinggal di Surabaya", 200).unwrap();
        assert_eq!(m.add_fact("panggil saya rani", 300), Err("Sudah ada di memori".into()));
        assert_eq!(m.add_fact("  ", 300), Err("Tulis dulu apa yang perlu diingat".into()));
        let v = m.memory(1_000);
        assert_eq!(v.facts.iter().map(|f| f.text.as_str()).collect::<Vec<_>>(), ["Panggil saya Rani", "Saya tinggal di Surabaya"]);
        m.edit_fact(1, "Saya tinggal di Surabaya").unwrap();
        assert_eq!(m.edit_fact(0, "saya tinggal di surabaya"), Err("Sudah ada di memori".into()));
        assert_eq!(m.edit_fact(9, "x"), Err("Catatan itu sudah tidak ada".into()));
        assert_eq!(m.memory(1_000).facts[1].text, "Saya tinggal di Surabaya");
        m.delete_fact(0).unwrap();
        assert_eq!(m.memory(1_000).facts.len(), 1);
        assert!(m.delete_fact(5).is_err());
        let raw = String::from_utf8_lossy(&std::fs::read(&mem_path).unwrap()).to_string();
        assert!(!raw.contains("Surabaya") && !raw.contains("Rani"), "encrypted on disk");
    }

    #[test]
    #[ignore = "uses the real Windows Credential Manager or DPAPI: run with MOKU_TEST_SYSTEM=1 --ignored"]
    fn a_paused_memory_takes_no_new_facts() {
        system_switch();
        let (cfg_path, mem_path) = dirs("paused");
        let mut m = Model::open(cfg_path, mem_path);
        let mut v = values();
        v.memory_paused = true;
        m.save(&v).unwrap();
        assert_eq!(m.add_fact("sesuatu", 1), Err("Memori sedang dijeda".into()));
        assert_eq!(m.memory(10).status, aimem::Status::Empty);
        v.memory_paused = false;
        m.save(&v).unwrap();
        assert!(m.add_fact("sesuatu", 1).is_ok());
    }

    #[test]
    #[ignore = "uses the real Windows Credential Manager or DPAPI: run with MOKU_TEST_SYSTEM=1 --ignored"]
    fn history_retention_and_clearing() {
        system_switch();
        let (cfg_path, mem_path) = dirs("history");
        let mut m = Model::open(cfg_path, mem_path.clone());
        let now = 1_800_000_000i64;
        let day = 86_400;
        aimem::update(&mem_path, |mem| {
            mem.add_history("ask", "lama", "a", "h", now - 20 * day);
            mem.add_history("ask", "baru", "a", "h", now - day);
            mem.add_fact("catatan", now).unwrap();
        })
        .unwrap();
        // 7 days (default): the old entry is swept when the page reads the memory.
        let v = m.memory(now);
        assert_eq!(v.history, 1);
        assert_eq!(v.facts.len(), 1, "facts never expire");
        // 30 days keeps more, but never brings back what was swept.
        let mut vals = values();
        vals.retention = 1;
        m.save(&vals).unwrap();
        assert_eq!(m.memory(now).history, 1);
        m.clear_history().unwrap();
        assert_eq!((m.memory(now).history, m.memory(now).facts.len()), (0, 1));
    }

    #[test]
    #[ignore = "uses the real Windows Credential Manager or DPAPI: run with MOKU_TEST_SYSTEM=1 --ignored"]
    fn export_is_plain_json_and_never_overwrites() {
        system_switch();
        let (cfg_path, mem_path) = dirs("export");
        let m = Model::open(cfg_path, mem_path.clone());
        m.add_fact("Panggil saya Rani", 100).unwrap();
        let dir = mem_path.parent().unwrap().join("out");
        let p1 = m.export(&dir, 1_800_000_000).unwrap();
        let p2 = m.export(&dir, 1_800_000_000).unwrap();
        assert_ne!(p1, p2, "a second export in the same second gets its own file");
        let v: serde_json_lite::Probe = serde_json_lite::probe(&std::fs::read_to_string(&p1).unwrap());
        assert!(v.has_fact && v.kind_ok, "{v:?}");
        assert!(p1.file_name().unwrap().to_string_lossy().starts_with("Moku-memori-"));
        // An unreadable memory exports nothing.
        std::fs::write(&mem_path, b"garbage").unwrap();
        assert!(m.export(&dir, 1_800_000_001).is_err());
    }

    /// No serde_json here: the settings app has no such dependency, and the test only needs two facts.
    mod serde_json_lite {
        #[derive(Debug)]
        pub struct Probe {
            pub has_fact: bool,
            pub kind_ok: bool,
        }
        pub fn probe(s: &str) -> Probe {
            Probe { has_fact: s.contains("Panggil saya Rani"), kind_ok: s.contains("\"kind\": \"tanya-ai-memory\"") }
        }
    }

    #[test]
    #[ignore = "uses the real Windows Credential Manager or DPAPI: run with MOKU_TEST_SYSTEM=1 --ignored"]
    fn a_damaged_memory_is_reported_kept_and_only_removed_on_request() {
        system_switch();
        let (cfg_path, mem_path) = dirs("broken");
        let m = Model::open(cfg_path, mem_path.clone());
        m.add_fact("penting", 1).unwrap();
        let mut bytes = std::fs::read(&mem_path).unwrap();
        let n = bytes.len();
        bytes[n - 5] ^= 0x42;
        std::fs::write(&mem_path, &bytes).unwrap();
        assert!(matches!(m.memory(10).status, aimem::Status::Unavailable(_)));
        assert!(m.add_fact("baru", 2).is_err(), "nothing is written over it");
        assert!(m.delete_fact(0).is_err());
        assert_eq!(std::fs::read(&mem_path).unwrap(), bytes, "the file is exactly as it was");
        m.wipe().unwrap();
        assert!(!mem_path.exists());
        assert_eq!(m.memory(10).status, aimem::Status::Empty);
        m.add_fact("mulai baru", 3).unwrap();
        assert_eq!(m.memory(10).facts.len(), 1);
    }

    #[test]
    fn dates_are_civil_and_local_looking() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_453), (2025, 12, 31));
        assert_eq!(civil(20_417), (2025, 11, 25));
        assert_eq!(civil(11_016), (2000, 2, 29));
        let s = stamp_of(1_800_000_000);
        assert_eq!(s.len(), 15, "{s}");
        assert!(date_label(1_800_000_000).contains("2027"), "{}", date_label(1_800_000_000));
    }

    // ---- the connection test against a local server -----------------------------------------------

    fn cfg_for(base: &str, model: &str) -> Config {
        let mut c = Config::default();
        c.ai.base_url = base.into();
        c.ai.model = model.into();
        c
    }

    #[test]
    fn the_connection_test_reports_models_and_problems() {
        use crate::net::mock::{Script, Server};
        let _s = crate::net::test_lock();
        let srv = Server::start(|s| {
            if s.path == "/v1/models" {
                Script::json(200, r#"{"data":[{"id":"llama3.2"},{"id":"qwen2.5"}]}"#)
            } else {
                Script::json(404, "{}")
            }
        });
        let ok = test_connection(&cfg_for(&srv.base(), "llama3.2")).unwrap();
        assert!(ok.contains("2 model") && ok.contains("tersedia"), "{ok}");
        let missing = test_connection(&cfg_for(&srv.base(), "gpt-9")).unwrap();
        assert!(missing.contains("tidak ada di daftar"), "{missing}");
        let seen = srv.requests();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].header("authorization"), None, "a localhost provider is asked without a key");
        // Wrong path: 404 message. Nothing listening: a short network message.
        let e = test_connection(&cfg_for(&format!("http://127.0.0.1:{}/wrong", srv.addr.port()), "m")).unwrap_err();
        assert!(e.contains("base URL"), "{e}");
        let e = test_connection(&cfg_for("http://127.0.0.1:9/v1", "m")).unwrap_err();
        assert!(e.contains("Tidak bisa terhubung"), "{e}");
        // A remote provider with no key is refused before anything is sent.
        let e = test_connection(&cfg_for("https://api.moku-test-nokey.example.com/v1", "m")).unwrap_err();
        assert!(e.contains("Belum ada kunci"), "{e}");
        // 401 / redirect.
        let s401 = Server::start(|_| Script::json(401, r#"{"error":{"message":"bad key"}}"#));
        assert!(test_connection(&cfg_for(&s401.base(), "m")).unwrap_err().contains("Kunci API ditolak"));
        let other = Server::start(|_| Script::json(200, "{}"));
        let target = other.url("/v1/models");
        let redir = Server::start(move |_| Script::redirect(&target));
        assert!(test_connection(&cfg_for(&redir.base(), "m")).unwrap_err().contains("pengalihan"));
        assert!(other.requests().is_empty(), "never followed");
    }
}
