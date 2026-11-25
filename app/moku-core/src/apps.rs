//! Apps by process name, shared by the island and the settings app.
//!
//! * the per-app rules, as pure matching: the island hides in front of listed apps
//!   (`island.hide_apps`, process names), and ignores the media sessions of listed apps
//!   (`island.media_ignore_apps`, matched against the session's `SourceAppUserModelId`);
//! * which app is in front ([`foreground_exe`]), for the hide rule and for the settings app's
//!   "Aplikasi aktif" button.

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::Threading::{GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetForegroundWindow, GetWindowThreadProcessId};
use windows::core::{BOOL, PWSTR};

/// `C:\Apps\Chrome.EXE` / `chrome.exe` / ` Chrome ` -> `chrome`.
pub fn exe_stem(s: &str) -> String {
    let name = s.trim().rsplit(['\\', '/']).next().unwrap_or("").trim();
    let lower = name.to_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

/// Is the process `exe` (a file name or a full path) one of `list`? Case-insensitive, `.exe` optional.
pub fn exe_matches(exe: &str, list: &[String]) -> bool {
    let me = exe_stem(exe);
    !me.is_empty() && list.iter().any(|a| exe_stem(a) == me)
}

/// Should the media session of the app `app_id` (`Spotify.exe`, `chrome.exe`,
/// `5319275A.WhatsAppDesktop_cv1g1gvanyjgm!App`) be ignored?
///
/// A rule matches when it is the app's executable name (`.exe` optional), or one of the words of
/// the id (split at anything that is not a letter or digit), or the start of such a word when it
/// has at least 4 letters: `spotify` matches `SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify`,
/// `whatsapp` matches `...WhatsAppDesktop...`; `art` matches nothing it is not.
pub fn media_ignored(app_id: &str, list: &[String]) -> bool {
    let id = app_id.trim();
    if id.is_empty() {
        return false;
    }
    let lower = id.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let stem = exe_stem(id);
    list.iter().map(|r| exe_stem(r)).filter(|r| !r.is_empty()).any(|r| {
        r == stem || words.iter().any(|w| *w == r || (r.chars().count() >= 4 && w.starts_with(r.as_str())))
    })
}

// ---- Windows -----------------------------------------------------------------------------------

/// File name of the process `pid` (`chrome.exe`), if it can be read (protected processes cannot).
pub fn process_name(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        let full = String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]);
        Some(full.rsplit('\\').next().unwrap_or(&full).to_string())
    }
}

/// The first child window of `w` owned by another process than `host`: a UWP app lives in a
/// child of the window `ApplicationFrameHost.exe` shows.
fn foreign_child_pid(w: HWND, host: u32) -> Option<u32> {
    struct Search {
        host: u32,
        found: u32,
    }
    unsafe extern "system" fn each(child: HWND, lp: LPARAM) -> BOOL {
        let s = unsafe { &mut *(lp.0 as *mut Search) };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(child, Some(&mut pid)) };
        if pid != 0 && pid != s.host {
            s.found = pid;
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut s = Search { host, found: 0 };
    unsafe {
        let _ = EnumChildWindows(Some(w), Some(each), LPARAM(&mut s as *mut Search as isize));
    }
    (s.found != 0).then_some(s.found)
}

/// The process name of the foreground window (for a UWP app, the app's own process, not
/// `ApplicationFrameHost.exe`). `None` when there is none, it cannot be read, or the window is
/// this process's own.
pub fn foreground_exe() -> Option<String> {
    unsafe {
        let w = GetForegroundWindow();
        if w.is_invalid() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(w, Some(&mut pid));
        if pid == 0 || pid == GetCurrentProcessId() {
            return None;
        }
        let name = process_name(pid)?;
        if name.eq_ignore_ascii_case("ApplicationFrameHost.exe") {
            return Some(foreign_child_pid(w, pid).and_then(process_name).unwrap_or(name));
        }
        Some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn exe_names_match_case_insensitively_with_or_without_exe_and_path() {
        let rules = l(&["Chrome.exe", " code ", r"C:\Games\Valorant.EXE"]);
        for exe in ["chrome.exe", "CHROME.EXE", "chrome", r"C:\Program Files\Google\Chrome\chrome.exe", "Code.exe", "valorant.exe"] {
            assert!(exe_matches(exe, &rules), "{exe}");
        }
        for exe in ["chromium.exe", "chrome2.exe", "explorer.exe", "", "  ", ".exe"] {
            assert!(!exe_matches(exe, &rules), "{exe:?}");
        }
        assert!(!exe_matches("chrome.exe", &[]), "no rules, no match");
        assert!(!exe_matches("a.exe", &l(&["", "  ", ".exe"])), "empty rules never match");
    }

    #[test]
    fn the_name_of_this_process_is_readable_and_is_not_the_foreground_app() {
        let me = process_name(std::process::id()).expect("our own process name");
        assert!(me.to_lowercase().ends_with(".exe") && !me.contains('\\'), "{me}");
        // The test harness is a rule target like any other.
        assert!(exe_matches(&me, &[me.to_uppercase()]));
        // `foreground_exe` never reports this process, whoever has the focus (it may be None).
        if let Some(fg) = foreground_exe() {
            assert!(!fg.is_empty());
        }
    }

    #[test]
    fn media_rules_match_the_source_app_id() {
        let spotify = l(&["Spotify.exe"]);
        assert!(media_ignored("Spotify.exe", &spotify));
        assert!(media_ignored("spotify.exe", &spotify));
        assert!(media_ignored("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify", &l(&["spotify"])));
        assert!(media_ignored("Spotify.exe", &l(&["spotify"])), "the .exe is optional on both sides");
        assert!(media_ignored("5319275A.WhatsAppDesktop_cv1g1gvanyjgm!App", &l(&["whatsapp"])), "prefix of a word");
        assert!(media_ignored("chrome.exe", &l(&["Chrome"])));
        assert!(media_ignored("MSEdge", &l(&["msedge.exe"])));
        assert!(media_ignored("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic", &l(&["zunemusic"])));
    }

    #[test]
    fn media_rules_do_not_match_by_accident() {
        // A short rule must be a whole word; a longer one may be the start of a word only.
        assert!(!media_ignored("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic", &l(&["art"])));
        assert!(!media_ignored("chrome.exe", &l(&["rom"])));
        assert!(media_ignored("chrome.exe", &l(&["chrom"])), "a prefix of a word of 4+ letters counts");
        assert!(!media_ignored("Spotify.exe", &l(&["potify"])), "never the middle of a word");
        assert!(!media_ignored("vlc.exe", &l(&["vl"])));
        assert!(media_ignored("vlc.exe", &l(&["vlc"])), "short but whole");
        assert!(!media_ignored("", &l(&["spotify"])));
        assert!(!media_ignored("Spotify.exe", &[]));
        assert!(!media_ignored("Spotify.exe", &l(&["", " "])));
    }
}
