//! Core Audio backend of the mixer: the sessions of the default render device
//! (`IAudioSessionManager2`), with the app's name from its executable's version info and its icon
//! from the helper process (`moku.exe --icons`, see `crate::helper`): the shell's image factory
//! never loads in the island.
//! Runs on the mixer service thread (COM MTA). Every call is best effort: a session that cannot
//! be read is skipped, nothing here panics.

use std::collections::HashMap;
use std::ffi::c_void;

use windows::Win32::Foundation::{CloseHandle, S_OK};
use windows::Win32::Media::Audio::{AudioSessionStateExpired, AudioSessionStateActive, IAudioSessionControl, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator, eConsole, eRender};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::core::{Interface, PCWSTR, PWSTR};

use super::model::{RawSession, process_key};
use super::service::Backend;
use crate::ui::Image;

pub struct Wasapi {
    en: IMMDeviceEnumerator,
    /// Executable path (lower case) to the name shown for it.
    names: HashMap<String, String>,
    icons: HashMap<String, Option<Image>>,
}

impl Wasapi {
    pub fn open() -> Option<Wasapi> {
        let en: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.ok()?;
        Some(Wasapi { en, names: HashMap::new(), icons: HashMap::new() })
    }

    /// The session manager of whatever the default output is right now (it can change).
    fn manager(&self) -> Option<IAudioSessionManager2> {
        unsafe {
            let dev = self.en.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
            dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None).ok()
        }
    }

    fn sessions_of(&self) -> Vec<IAudioSessionControl> {
        let Some(mgr) = self.manager() else { return vec![] };
        unsafe {
            let Ok(list) = mgr.GetSessionEnumerator() else { return vec![] };
            let n = list.GetCount().unwrap_or(0);
            (0..n).filter_map(|i| list.GetSession(i).ok()).collect()
        }
    }

    fn name_for(&mut self, exe: Option<&str>, display: &str) -> String {
        // The app's own name for the session, unless it is a resource reference ("@%SystemRoot%...").
        if !display.is_empty() && !display.starts_with('@') {
            return display.to_string();
        }
        let Some(exe) = exe else { return String::new() };
        self.names.entry(exe.to_lowercase()).or_insert_with(|| file_description(exe).filter(|s| !s.trim().is_empty()).unwrap_or_else(|| stem(exe))).clone()
    }

    /// Ask the helper for the icons of every executable not asked for yet, in one go (one
    /// process for the whole refresh). An app the shell has no icon for, or a failed helper,
    /// is remembered as "no icon": it is not asked for again.
    fn fetch_icons(&mut self, exes: &[&str]) {
        let mut missing: Vec<String> = vec![];
        for e in exes {
            let key = e.to_lowercase();
            if !self.icons.contains_key(&key) && !missing.contains(&key) {
                missing.push(key);
            }
        }
        if missing.is_empty() {
            return;
        }
        let set = crate::helper::run_icons(&missing, std::time::Duration::from_secs(15));
        let mut got: HashMap<String, Image> = HashMap::new();
        if let Some(set) = &set {
            for (path, r) in &set.items {
                if let Some(r) = r {
                    got.insert(path.to_lowercase(), Image::new(r.w as u32, r.h as u32, set.bgra(r).to_vec()));
                }
            }
        }
        for key in missing {
            let img = got.remove(&key);
            self.icons.insert(key, img);
        }
    }

    fn icon_for(&self, exe: &str) -> Option<Image> {
        self.icons.get(&exe.to_lowercase()).cloned().flatten()
    }
}

/// Facts about one session, enough to find it again.
struct Facts {
    c2: IAudioSessionControl2,
    vol: ISimpleAudioVolume,
    key: String,
    exe: Option<String>,
    system: bool,
}

fn facts(ctl: &IAudioSessionControl) -> Option<Facts> {
    let c2: IAudioSessionControl2 = ctl.cast().ok()?;
    let vol: ISimpleAudioVolume = ctl.cast().ok()?;
    // SAFETY: plain COM calls on live interfaces.
    let (system, pid) = unsafe { (c2.IsSystemSoundsSession() == S_OK, c2.GetProcessId().unwrap_or(0)) };
    let exe = if system || pid == 0 { None } else { exe_of(pid) };
    let key = process_key(exe.as_deref(), pid, system);
    Some(Facts { c2, vol, key, exe, system })
}

impl Backend for Wasapi {
    fn sessions(&mut self) -> Vec<RawSession> {
        let mut out = vec![];
        let found: Vec<(IAudioSessionControl, Facts)> = self.sessions_of().into_iter().filter_map(|ctl| facts(&ctl).map(|f| (ctl, f))).collect();
        let exes: Vec<&str> = found.iter().filter_map(|(_, f)| f.exe.as_deref()).collect();
        self.fetch_icons(&exes);
        for (ctl, f) in found {
            // SAFETY: plain COM calls; the display name is CoTaskMem memory we free.
            let (state, display, volume, muted) = unsafe {
                let display = ctl.GetDisplayName().ok().map(|p| {
                    let s = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.0 as *const c_void));
                    s
                });
                (ctl.GetState().ok(), display.unwrap_or_default(), f.vol.GetMasterVolume().unwrap_or(1.0), f.vol.GetMute().map(|m| m.as_bool()).unwrap_or(false))
            };
            let _ = &f.c2;
            let name = if f.system { String::new() } else { self.name_for(f.exe.as_deref(), &display) };
            let icon = f.exe.as_deref().and_then(|e| self.icon_for(e));
            out.push(RawSession {
                key: f.key,
                name,
                system: f.system,
                volume,
                muted,
                active: state == Some(AudioSessionStateActive),
                expired: state == Some(AudioSessionStateExpired),
                icon,
            });
        }
        out
    }

    fn set_volume(&mut self, key: &str, volume: f32) {
        for ctl in self.sessions_of() {
            if let Some(f) = facts(&ctl).filter(|f| f.key == key) {
                // SAFETY: a null event context is allowed.
                let _ = unsafe { f.vol.SetMasterVolume(volume.clamp(0.0, 1.0), std::ptr::null()) };
            }
        }
    }

    fn set_mute(&mut self, key: &str, muted: bool) {
        for ctl in self.sessions_of() {
            if let Some(f) = facts(&ctl).filter(|f| f.key == key) {
                let _ = unsafe { f.vol.SetMute(muted, std::ptr::null()) };
            }
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// "C:\Program Files\Spotify\Spotify.exe" -> "Spotify".
fn stem(path: &str) -> String {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(file).to_string()
}

/// The full path of a process's executable, `None` when it cannot be opened (protected).
fn exe_of(pid: u32) -> Option<String> {
    // SAFETY: the handle is closed again; the buffer is as long as `len` says.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// The block of a version resource that says which language / code page its strings use.
const TRANSLATION_QUERY: &str = "\\VarFileInfo\\Translation";

/// The "language codepage" (`040904b0`) the version block names, if it names one.
fn translation_of(data: &[u8]) -> Option<String> {
    let (mut ptr, mut len) = (std::ptr::null_mut::<c_void>(), 0u32);
    let q = wide(TRANSLATION_QUERY);
    // SAFETY: `data` is a version block; the pointer `VerQueryValueW` returns points into it and
    // is read before it goes out of scope.
    unsafe {
        if VerQueryValueW(data.as_ptr() as *const c_void, PCWSTR(q.as_ptr()), &mut ptr, &mut len).as_bool() && len >= 4 && !ptr.is_null() {
            let t = std::slice::from_raw_parts(ptr as *const u16, 2);
            return Some(format!("{:04x}{:04x}", t[0], t[1]));
        }
    }
    None
}

/// The "File description" of an executable (what Task Manager shows as its name).
fn file_description(path: &str) -> Option<String> {
    let p = wide(path);
    // SAFETY: the version block is a buffer of the size Windows asked for; the pointers
    // `VerQueryValueW` returns point into it and are read before it is dropped.
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(p.as_ptr()), None);
        if size == 0 || size > 1 << 20 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(p.as_ptr()), None, size, data.as_mut_ptr() as *mut c_void).ok()?;
        let (mut ptr, mut len) = (std::ptr::null_mut::<c_void>(), 0u32);
        let lang = translation_of(&data).unwrap_or_else(|| "040904b0".to_string());
        let q = wide(&format!("\\StringFileInfo\\{lang}\\FileDescription"));
        if VerQueryValueW(data.as_ptr() as *const c_void, PCWSTR(q.as_ptr()), &mut ptr, &mut len).as_bool() && len > 1 && !ptr.is_null() {
            let s = std::slice::from_raw_parts(ptr as *const u16, len as usize - 1);
            return Some(String::from_utf16_lossy(s));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The version block's own language list is found (the query used to name
    /// `VarFileInformation`, which does not exist, so every file fell back to US English), and the
    /// description comes from it.
    #[test]
    fn the_translation_query_finds_the_version_blocks_language() {
        assert_eq!(TRANSLATION_QUERY, "\\VarFileInfo\\Translation");
        let sys = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let path = format!(r"{sys}\System32\notepad.exe");
        let p = wide(&path);
        unsafe {
            let size = GetFileVersionInfoSizeW(PCWSTR(p.as_ptr()), None);
            assert!(size > 0, "notepad has a version resource");
            let mut data = vec![0u8; size as usize];
            GetFileVersionInfoW(PCWSTR(p.as_ptr()), None, size, data.as_mut_ptr() as *mut c_void).unwrap();
            let lang = translation_of(&data).expect("the Translation table");
            assert_eq!(lang.len(), 8, "{lang}");
            assert!(lang.chars().all(|c| c.is_ascii_hexdigit()), "{lang}");
        }
        let d = file_description(&path).expect("a file description");
        assert!(!d.trim().is_empty());
    }

    #[test]
    fn executable_stems() {
        assert_eq!(stem(r"C:\Program Files\Spotify\Spotify.exe"), "Spotify");
        assert_eq!(stem("vlc.EXE"), "vlc");
        assert_eq!(stem("noext"), "noext");
    }

    /// Reads the real sessions of this PC (read-only): what the mixer would show. Not run by
    /// default: `cargo test -p moku live_mixer_read -- --ignored --nocapture`.
    #[test]
    #[ignore = "reads the real audio sessions of this PC"]
    fn live_mixer_read() {
        use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        {
            let Some(mut w) = Wasapi::open() else { panic!("no device enumerator") };
            let raw = w.sessions();
            println!("{} raw session(s)", raw.len());
            for s in &raw {
                println!("  key={:?} name={:?} system={} vol={:.2} muted={} active={} expired={} icon={}", s.key, s.name, s.system, s.volume, s.muted, s.active, s.expired, s.icon.is_some());
            }
            let mut id = 0;
            let rows = super::super::model::reduce(&[], &mut id, raw);
            println!("{} row(s): {:?}", rows.len(), rows.iter().map(|r| (&r.name, r.volume, r.muted)).collect::<Vec<_>>());
        }
        unsafe { CoUninitialize() };
    }

    /// Sets and restores the level of the first real (non-system) app session: reads it, moves
    /// it by one percent, reads it back, puts the original value back at once and checks that.
    /// Same for mute. The value is restored even when an assertion fails.
    #[test]
    #[ignore = "briefly changes (and restores) the level of one real app"]
    fn live_mixer_roundtrip_restores_the_level() {
        use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        {
            let Some(mut w) = Wasapi::open() else { panic!("no device enumerator") };
            let Some(target) = w.sessions().into_iter().find(|s| !s.system && !s.expired) else {
                println!("no app has an audio session right now: nothing to round-trip");
                unsafe { CoUninitialize() };
                return;
            };
            let find = |w: &mut Wasapi| w.sessions().into_iter().find(|s| s.key == target.key).expect("session still there");
            let (v0, m0) = (find(&mut w).volume, find(&mut w).muted);
            struct Restore<'a>(&'a mut Wasapi, String, f32, bool);
            impl Drop for Restore<'_> {
                fn drop(&mut self) {
                    self.0.set_volume(&self.1, self.2);
                    self.0.set_mute(&self.1, self.3);
                }
            }
            let guard = Restore(&mut w, target.key.clone(), v0, m0);
            let nudged = if v0 > 0.5 { v0 - 0.01 } else { v0 + 0.01 };
            guard.0.set_volume(&target.key, nudged);
            let after = find(guard.0).volume;
            guard.0.set_volume(&target.key, v0);
            let back = find(guard.0).volume;
            guard.0.set_mute(&target.key, !m0);
            let muted_after = find(guard.0).muted;
            guard.0.set_mute(&target.key, m0);
            let muted_back = find(guard.0).muted;
            println!("volume {v0:.3} -> {after:.3} -> {back:.3}; mute {m0} -> {muted_after} -> {muted_back}");
            assert!((after - nudged).abs() < 0.011, "the new level was applied");
            assert!((back - v0).abs() < 0.011, "the original level is back");
            assert_eq!(muted_after, !m0, "mute was applied");
            assert_eq!(muted_back, m0, "mute is back");
        }
        unsafe { CoUninitialize() };
    }
}
