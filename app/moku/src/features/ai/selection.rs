//! The text the user selected in the program in front, and putting an answer back there.
//!
//! **Reading** (only on the user's own hotkey press): UI Automation first
//! (`TextPattern.GetSelection` on the focused element; a password field is never read). If
//! that finds nothing and `ai.copy_fallback` is on: save the clipboard, send Ctrl+C to the
//! program in front, read what it copied, put the old clipboard back. The fallback is skipped,
//! and Ctrl+C is not sent, when the program in front is a terminal (Ctrl+C there stops a
//! running program), when the clipboard cannot be saved faithfully, or when the foreground is
//! no longer the window the hotkey was pressed in.
//!
//! **Writing** (only on the user's explicit click on "Ganti teks"): put the answer on the
//! clipboard, bring the source window back, send Ctrl+V, restore the old clipboard. Never to a
//! terminal, never when the foreground is not the source window.
//!
//! Key presses are sent with `SendInput` by [`CtrlKey`] and nowhere else; [`Keys`] is a trait
//! so the logic is tested without sending anything, and the real sender can be told to refuse
//! any window that is not one of ours ([`CtrlKey::only_pid`], used by tests).

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation, IUIAutomationTextPattern, UIA_TextPatternId};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId};
use windows::core::PWSTR;

use super::clip::Clipboard;

/// The most characters read from a selection.
pub const MAX_CHARS: usize = 50_000;
/// How long UI Automation may take before we go on without it.
const UIA_BUDGET: Duration = Duration::from_millis(900);
/// How long to wait for a program to put its copy on the clipboard.
const COPY_WAIT: Duration = Duration::from_millis(450);
/// How long to wait for the user to let go of the hotkey's modifier keys.
const MODIFIER_WAIT: Duration = Duration::from_millis(1200);

/// The program in front when the hotkey was pressed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Source {
    pub hwnd: isize,
    pub pid: u32,
    /// Lower-case file name of the program (`chrome.exe`).
    pub exe: String,
    pub class: String,
}

/// Terminals and consoles: Ctrl+C there means "stop", and a paste may run lines.
pub fn is_terminal(s: &Source) -> bool {
    const CLASSES: [&str; 6] = ["consolewindowclass", "cascadia_hosting_window_class", "pseudoconsolewindow", "mintty", "virtualconsoleclass", "windowsterminal"];
    const EXES: [&str; 20] = [
        "windowsterminal.exe", "wt.exe", "cmd.exe", "powershell.exe", "pwsh.exe", "conhost.exe", "openconsole.exe", "wezterm-gui.exe", "alacritty.exe", "mintty.exe", "putty.exe", "kitty.exe", "conemu.exe", "conemu64.exe", "hyper.exe", "tabby.exe",
        "warp.exe", "bash.exe", "wsl.exe", "termius.exe",
    ];
    let class = s.class.to_lowercase();
    CLASSES.iter().any(|c| class == *c) || EXES.contains(&s.exe.to_lowercase().as_str())
}

fn own_program(s: &Source) -> bool {
    s.pid == std::process::id() || s.exe.starts_with("moku")
}

/// The foreground window now. `None` when there is none.
pub fn foreground_source() -> Option<Source> {
    unsafe {
        let h = GetForegroundWindow();
        if h.0.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let mut cls = [0u16; 128];
        let n = GetClassNameW(h, &mut cls).max(0) as usize;
        Some(Source { hwnd: h.0 as isize, pid, exe: exe_name(pid), class: String::from_utf16_lossy(&cls[..n]) })
    }
}

fn exe_name(pid: u32) -> String {
    if pid == 0 {
        return String::new();
    }
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        if r.is_err() {
            return String::new();
        }
        let full = String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]);
        full.rsplit('\\').next().unwrap_or(&full).to_lowercase()
    }
}

// ---- UI Automation -------------------------------------------------------------------------

/// The selected text according to UI Automation, on a helper thread with a time budget. `None`:
/// nothing selected, no TextPattern, a password field, or too slow.
pub fn uia_selected_text() -> Option<String> {
    uia_with_budget(None)
}

/// [`uia_selected_text`] for a given window handle instead of the focused element (tests use
/// their own window; the hotkey path always uses the focused element).
fn uia_with_budget(target: Option<isize>) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("ai-uia".into()).spawn(move || {
        let _ = tx.send(uia_read(target));
    });
    spawned.ok()?;
    rx.recv_timeout(UIA_BUDGET).ok().flatten()
}

fn uia_read(target: Option<isize>) -> Option<String> {
    unsafe {
        let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let r = (|| -> Option<String> {
            let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
            let el = match target {
                Some(h) => uia.ElementFromHandle(HWND(h as *mut _)).ok()?,
                None => uia.GetFocusedElement().ok()?,
            };
            if el.CurrentIsPassword().ok()?.as_bool() {
                return None;
            }
            let pat: IUIAutomationTextPattern = el.GetCurrentPatternAs(UIA_TextPatternId).ok()?;
            let ranges = pat.GetSelection().ok()?;
            let n = ranges.Length().ok()?.clamp(0, 8);
            let mut text = String::new();
            for i in 0..n {
                let r = ranges.GetElement(i).ok()?;
                let s = r.GetText(MAX_CHARS as i32).ok()?;
                text.push_str(&s.to_string());
                if text.chars().count() >= MAX_CHARS {
                    break;
                }
            }
            let text: String = text.chars().take(MAX_CHARS).collect();
            (!text.trim().is_empty()).then_some(text)
        })();
        if com {
            CoUninitialize();
        }
        r
    }
}
// ---- sending keys ----------------------------------------------------------------------------

/// Sends a "copy" or "paste" to the program in front.
pub trait Keys {
    fn copy(&self) -> bool;
    fn paste(&self) -> bool;
}

/// Ctrl+C / Ctrl+V with `SendInput`. It refuses (sends nothing, returns false) unless the window
/// in front is exactly `expect`, and, when `only_pid` is set (tests), belongs to that process.
pub struct CtrlKey {
    pub expect: isize,
    pub only_pid: Option<u32>,
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(vk.0 as i32) < 0 }
}

/// Wait until Ctrl, Alt, Shift and Win are up (the user is still holding the hotkey).
fn modifiers_released(wait: Duration) -> bool {
    let t = Instant::now();
    loop {
        if ![VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN, VK_LCONTROL, VK_RCONTROL, VK_LMENU, VK_RMENU, VK_LSHIFT, VK_RSHIFT].iter().any(|k| key_down(*k)) {
            return true;
        }
        if t.elapsed() > wait {
            return false;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn input(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, time: 0, dwExtraInfo: 0 } },
    }
}

impl CtrlKey {
    fn chord(&self, letter: u8) -> bool {
        let fg = unsafe { GetForegroundWindow() };
        if fg.0 as isize != self.expect || self.expect == 0 {
            return false;
        }
        if let Some(pid) = self.only_pid {
            let mut p = 0u32;
            unsafe { GetWindowThreadProcessId(fg, Some(&mut p)) };
            if p != pid {
                return false;
            }
        }
        if !modifiers_released(MODIFIER_WAIT) {
            return false;
        }
        // The window may have changed while we waited.
        if unsafe { GetForegroundWindow() }.0 as isize != self.expect {
            return false;
        }
        let key = VIRTUAL_KEY(letter as u16);
        let inputs = [input(VK_CONTROL, false), input(key, false), input(key, true), input(VK_CONTROL, true)];
        unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) == inputs.len() as u32 }
    }
}

impl Keys for CtrlKey {
    fn copy(&self) -> bool {
        self.chord(b'C')
    }
    fn paste(&self) -> bool {
        self.chord(b'V')
    }
}

// ---- the copy fallback -----------------------------------------------------------------------

/// Why the fallback did not produce text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// The program in front is a terminal (or our own window).
    Terminal,
    /// The clipboard cannot be saved without losing something: we leave it alone.
    ClipboardNotSavable,
    /// The keys could not be sent (the foreground changed, a modifier stayed down).
    NoKeys,
    /// The program did not copy anything (no selection).
    NothingCopied,
}

/// Save the clipboard, send "copy", read the text, put the clipboard back. The clipboard is
/// restored whenever it was touched, whatever happens.
pub fn copy_fallback(src: &Source, clip: &dyn Clipboard, keys: &dyn Keys, wait: Duration) -> Result<String, Skip> {
    if is_terminal(src) || own_program(src) {
        return Err(Skip::Terminal);
    }
    // Moku's own history ignores every clipboard change until the old content is back (the selection copy
    // is not the user's copy). Dropped on every way out of this function.
    let _hold = crate::features::clipboard::hold::Hold::new();
    let saved = clip.save().ok_or(Skip::ClipboardNotSavable)?;
    let before = clip.sequence();
    if !keys.copy() {
        return Err(Skip::NoKeys);
    }
    let t = Instant::now();
    while clip.sequence() == before {
        if t.elapsed() > wait {
            // Nothing was copied, so the clipboard is as it was.
            return Err(Skip::NothingCopied);
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    let text = clip.read_text();
    clip.restore(&saved);
    match text.map(|t| t.chars().take(MAX_CHARS).collect::<String>()).filter(|t| !t.trim().is_empty()) {
        Some(t) => Ok(t),
        None => Err(Skip::NothingCopied),
    }
}

/// The whole selection lookup for one hotkey press: UI Automation, then (when allowed) the
/// copy fallback. Returns the text and how it was found.
/// `owner` is a window of this process (the island's main window): the clipboard is opened with it,
/// which is what lets us put the user's clipboard back.
pub fn capture(src: &Source, allow_copy: bool, owner: isize) -> Result<(String, Via), Skip> {
    if own_program(src) {
        return Err(Skip::Terminal);
    }
    if let Some(t) = uia_selected_text() {
        return Ok((t, Via::Uia));
    }
    if !allow_copy {
        return Err(Skip::NothingCopied);
    }
    let clip = super::clip::Win { owner };
    let keys = CtrlKey { expect: src.hwnd, only_pid: None };
    copy_fallback(src, &clip, &keys, COPY_WAIT).map(|t| (t, Via::Copy))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Uia,
    Copy,
}

// ---- putting an answer back ------------------------------------------------------------------

/// Why "Ganti teks" did not paste.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteSkip {
    Terminal,
    ClipboardNotSavable,
    ClipboardBusy,
    /// The source window could not be brought back, or is not in front.
    NoSourceWindow,
    NoKeys,
}

/// How [`paste_back`] brings the source window back to the front (a trait so tests need no window).
pub trait Foreground {
    fn current(&self) -> isize;
    fn bring_back(&self, hwnd: isize);
}

pub struct RealForeground;

impl Foreground for RealForeground {
    fn current(&self) -> isize {
        unsafe { GetForegroundWindow().0 as isize }
    }
    fn bring_back(&self, hwnd: isize) {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(HWND(hwnd as *mut _));
        }
    }
}

/// Put `text` on the clipboard, bring `src` back, send Ctrl+V, restore the clipboard. Only call
/// it from an explicit click.
pub fn paste_back(src: &Source, text: &str, clip: &dyn Clipboard, fg: &dyn Foreground, keys: &dyn Keys, settle: Duration) -> Result<(), PasteSkip> {
    if is_terminal(src) || own_program(src) {
        return Err(PasteSkip::Terminal);
    }
    let _hold = crate::features::clipboard::hold::Hold::new();
    let saved = clip.save().ok_or(PasteSkip::ClipboardNotSavable)?;
    if !clip.set_private_text(text) {
        clip.restore(&saved);
        return Err(PasteSkip::ClipboardBusy);
    }
    fg.bring_back(src.hwnd);
    let t = Instant::now();
    while fg.current() != src.hwnd {
        if t.elapsed() > Duration::from_millis(700) {
            clip.restore(&saved);
            return Err(PasteSkip::NoSourceWindow);
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    let sent = keys.paste();
    // Let the program read the clipboard before it is replaced again.
    std::thread::sleep(settle);
    clip.restore(&saved);
    if sent { Ok(()) } else { Err(PasteSkip::NoKeys) }
}

#[cfg(test)]
mod tests {
    use super::super::clip::fake::Fake;
    use super::*;
    use std::cell::{Cell, RefCell};

    fn src(exe: &str, class: &str) -> Source {
        Source { hwnd: 4242, pid: 99999, exe: exe.into(), class: class.into() }
    }

    /// A program that copies `selection` on Ctrl+C.
    struct App<'a> {
        clip: &'a Fake,
        selection: Option<&'a str>,
        sent: Cell<u32>,
        pasted: RefCell<Vec<String>>,
        refuse: bool,
    }

    impl Keys for App<'_> {
        fn copy(&self) -> bool {
            if self.refuse {
                return false;
            }
            self.sent.set(self.sent.get() + 1);
            if let Some(s) = self.selection {
                self.clip.external_copy(s);
            }
            true
        }
        fn paste(&self) -> bool {
            if self.refuse {
                return false;
            }
            self.pasted.borrow_mut().push(self.clip.text().unwrap_or_default());
            true
        }
    }

    fn app<'a>(clip: &'a Fake, selection: Option<&'a str>) -> App<'a> {
        App { clip, selection, sent: Cell::new(0), pasted: RefCell::new(vec![]), refuse: false }
    }

    const FAST: Duration = Duration::from_millis(150);

    #[test]
    fn copy_fallback_reads_the_selection_and_puts_the_old_clipboard_back() {
        let clip = Fake::with_text("punya saya yang penting");
        let a = app(&clip, Some("teks yang dipilih"));
        let got = copy_fallback(&src("notepad.exe", "Notepad"), &clip, &a, FAST);
        assert_eq!(got, Ok("teks yang dipilih".to_string()));
        assert_eq!(a.sent.get(), 1, "one Ctrl+C");
        assert_eq!(clip.text().as_deref(), Some("punya saya yang penting"), "the user's clipboard is back");
        assert_eq!(clip.writes.borrow().as_slice(), ["restore"], "the only write of ours is the restore");
    }

    #[test]
    fn an_empty_clipboard_is_empty_again_afterwards() {
        let clip = Fake::empty();
        let a = app(&clip, Some("x y z"));
        assert_eq!(copy_fallback(&src("notepad.exe", "Notepad"), &clip, &a, FAST), Ok("x y z".into()));
        assert!(clip.content.borrow().formats.is_empty() && clip.text().is_none());
    }

    #[test]
    fn nothing_selected_means_nothing_copied_and_the_clipboard_is_untouched() {
        let clip = Fake::with_text("lama");
        let a = app(&clip, None);
        let t = Instant::now();
        assert_eq!(copy_fallback(&src("notepad.exe", "Notepad"), &clip, &a, FAST), Err(Skip::NothingCopied));
        assert!(t.elapsed() >= FAST && t.elapsed() < Duration::from_secs(2));
        assert_eq!(clip.text().as_deref(), Some("lama"));
        assert!(clip.writes.borrow().is_empty(), "no write at all");
        // A program that copies blank text counts as nothing too, and the clipboard is restored.
        let a = app(&clip, Some("   "));
        assert_eq!(copy_fallback(&src("notepad.exe", "Notepad"), &clip, &a, FAST), Err(Skip::NothingCopied));
        assert_eq!(clip.text().as_deref(), Some("lama"));
    }

    #[test]
    fn terminals_never_get_ctrl_c() {
        for (exe, class) in [("windowsterminal.exe", "CASCADIA_HOSTING_WINDOW_CLASS"), ("cmd.exe", "ConsoleWindowClass"), ("pwsh.exe", ""), ("", "mintty"), ("WezTerm-GUI.exe", ""), ("ALACRITTY.EXE", "")] {
            let clip = Fake::with_text("x");
            let a = app(&clip, Some("y"));
            assert_eq!(copy_fallback(&src(exe, class), &clip, &a, FAST), Err(Skip::Terminal), "{exe} {class}");
            assert_eq!(a.sent.get(), 0, "no key was sent to {exe}");
            assert_eq!(paste_back(&src(exe, class), "z", &clip, &AlwaysFront(4242), &a, Duration::ZERO), Err(PasteSkip::Terminal));
            assert!(a.pasted.borrow().is_empty());
        }
        // Ordinary programs are not terminals.
        for exe in ["chrome.exe", "code.exe", "notepad.exe", "winword.exe", "msedge.exe"] {
            assert!(!is_terminal(&src(exe, "Chrome_WidgetWin_1")), "{exe}");
        }
        // Our own windows are never read or typed into either.
        let mut me = src("moku.exe", "x");
        me.pid = 1;
        assert!(own_program(&me));
        let clip = Fake::with_text("x");
        assert_eq!(copy_fallback(&me, &clip, &app(&clip, Some("y")), FAST), Err(Skip::Terminal));
    }

    #[test]
    fn a_clipboard_that_cannot_be_saved_is_left_alone() {
        let clip = Fake::with_text("gambar atau format lain");
        clip.unsavable.set(true);
        let a = app(&clip, Some("y"));
        assert_eq!(copy_fallback(&src("notepad.exe", "Notepad"), &clip, &a, FAST), Err(Skip::ClipboardNotSavable));
        assert_eq!(a.sent.get(), 0, "Ctrl+C is not even sent: it would destroy what we cannot restore");
        assert!(clip.writes.borrow().is_empty());
        assert_eq!(paste_back(&src("notepad.exe", "x"), "z", &clip, &AlwaysFront(4242), &a, Duration::ZERO), Err(PasteSkip::ClipboardNotSavable));
        assert!(clip.writes.borrow().is_empty() && a.pasted.borrow().is_empty());
    }

    #[test]
    fn when_the_keys_cannot_be_sent_nothing_changes() {
        let clip = Fake::with_text("lama");
        let mut a = app(&clip, Some("y"));
        a.refuse = true;
        assert_eq!(copy_fallback(&src("notepad.exe", "x"), &clip, &a, FAST), Err(Skip::NoKeys));
        assert_eq!(clip.text().as_deref(), Some("lama"));
        assert!(clip.writes.borrow().is_empty());
    }

    struct AlwaysFront(isize);
    impl Foreground for AlwaysFront {
        fn current(&self) -> isize {
            self.0
        }
        fn bring_back(&self, _: isize) {}
    }

    struct NeverFront;
    impl Foreground for NeverFront {
        fn current(&self) -> isize {
            1
        }
        fn bring_back(&self, _: isize) {}
    }

    #[test]
    fn paste_back_puts_the_answer_pastes_and_restores() {
        let clip = Fake::with_text("punya saya");
        let a = app(&clip, None);
        let r = paste_back(&src("notepad.exe", "Notepad"), "jawaban baru", &clip, &AlwaysFront(4242), &a, Duration::from_millis(10));
        assert_eq!(r, Ok(()));
        assert_eq!(a.pasted.borrow().as_slice(), ["jawaban baru"], "the program pasted the answer");
        assert_eq!(clip.text().as_deref(), Some("punya saya"), "and the user's clipboard is back");
        assert_eq!(clip.writes.borrow().as_slice(), ["set:jawaban baru", "restore"]);
    }

    #[test]
    fn paste_back_does_not_paste_when_the_source_window_is_not_in_front() {
        let clip = Fake::with_text("punya saya");
        let a = app(&clip, None);
        let t = Instant::now();
        let r = paste_back(&src("notepad.exe", "Notepad"), "jawaban", &clip, &NeverFront, &a, Duration::from_millis(10));
        assert_eq!(r, Err(PasteSkip::NoSourceWindow));
        assert!(t.elapsed() < Duration::from_secs(3));
        assert!(a.pasted.borrow().is_empty(), "Ctrl+V was not sent to whatever is in front");
        assert_eq!(clip.text().as_deref(), Some("punya saya"), "the clipboard is restored even then");
    }

    // ---- the real key sender refuses every window that is not the expected one -------------

    #[test]
    fn the_real_sender_sends_nothing_unless_the_expected_window_is_in_front() {
        // 0 and a handle that is certainly not the foreground window.
        for expect in [0isize, 1, 0x1234, -1] {
            let k = CtrlKey { expect, only_pid: None };
            assert!(!k.copy(), "expect {expect:#x}");
            assert!(!k.paste(), "expect {expect:#x}");
        }
        // Even the real foreground window is refused when it is not ours (tests pass only_pid).
        let fg = unsafe { GetForegroundWindow() }.0 as isize;
        if fg != 0 {
            let k = CtrlKey { expect: fg, only_pid: Some(0xFFFF_FFF0) };
            assert!(!k.copy() && !k.paste(), "a window of another process is never typed into by a test");
        }
    }

    #[test]
    fn the_foreground_lookup_never_panics() {
        // Only asks Windows which window is in front (no UI Automation on other programs here).
        assert!(foreground_source().is_none_or(|s| s.hwnd != 0));
    }
}
/// Tests against a window of our own (never another program's): a real top-level window with
/// an EDIT control holding "hello world" with "hello" selected, on its own thread with a message
/// loop. They use the REAL clipboard (saved and restored) and flash a small window for a moment,
/// so they are `#[ignore]`d.
/// Run: `cargo test -p moku selection::window_tests -- --ignored --test-threads=1 --nocapture`
#[cfg(test)]
mod window_tests {
    use super::super::clip::{Saved, Win};
    use super::*;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;

    const EM_SETSEL: u32 = 0x00B1;

    /// The rich edit control (set once the window exists); `WM_APP + 1` gives it the keyboard focus on
    /// the window's own thread (SetFocus only works for the calling thread's queue).
    static EDIT: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

    extern "system" fn proc(h: HWND, m: u32, w: windows::Win32::Foundation::WPARAM, l: windows::Win32::Foundation::LPARAM) -> windows::Win32::Foundation::LRESULT {
        unsafe {
            if m == WM_APP + 1 {
                let _ = SetFocus(Some(HWND(EDIT.load(std::sync::atomic::Ordering::SeqCst) as *mut _)));
                return windows::Win32::Foundation::LRESULT(0);
            }
            if m == WM_DESTROY {
                PostQuitMessage(0);
                return windows::Win32::Foundation::LRESULT(0);
            }
            DefWindowProcW(h, m, w, l)
        }
    }

    struct TestWindow {
        top: isize,
        edit: isize,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl TestWindow {
        fn new() -> TestWindow {
            let (tx, rx) = std::sync::mpsc::channel();
            let thread = std::thread::spawn(move || unsafe {
                let inst = GetModuleHandleW(None).unwrap();
                let class = w!("MokuAiSelectionTest");
                let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: inst.into(), lpszClassName: class, ..Default::default() };
                RegisterClassW(&wc);
                let top = CreateWindowExW(WS_EX_TOOLWINDOW, class, w!("Moku AI test window"), WS_OVERLAPPEDWINDOW | WS_VISIBLE, 120, 120, 360, 140, None, None, Some(inst.into()), None).unwrap();
                // Rich Edit supports the UI Automation TextPattern (the classic EDIT control does not).
                let _ = windows::Win32::System::LibraryLoader::LoadLibraryW(w!("Msftedit.dll"));
                let edit = CreateWindowExW(WINDOW_EX_STYLE(0), w!("RICHEDIT50W"), w!("hello world"), WS_CHILD | WS_VISIBLE | WINDOW_STYLE(0x0004), 10, 10, 320, 60, Some(top), None, Some(inst.into()), None).unwrap();
                SendMessageW(edit, EM_SETSEL, Some(windows::Win32::Foundation::WPARAM(0)), Some(windows::Win32::Foundation::LPARAM(5)));
                EDIT.store(edit.0 as isize, std::sync::atomic::Ordering::SeqCst);
                tx.send((top.0 as isize, edit.0 as isize)).unwrap();
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            });
            let (top, edit) = rx.recv().unwrap();
            TestWindow { top, edit, thread: Some(thread) }
        }
    }

    impl Drop for TestWindow {
        fn drop(&mut self) {
            unsafe {
                let _ = PostMessageW(Some(HWND(self.top as *mut _)), WM_CLOSE, windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(0));
            }
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    /// "Copy" for the test: the EDIT control's own WM_COPY message (in this process; no input).
    struct WmCopy(isize);

    impl Keys for WmCopy {
        fn copy(&self) -> bool {
            unsafe { SendMessageW(HWND(self.0 as *mut _), WM_COPY, None, None) };
            true
        }
        fn paste(&self) -> bool {
            false
        }
    }

    #[test]
    #[ignore = "creates a window and uses the real clipboard (saved and restored)"]
    fn uia_reads_the_selection_of_our_own_edit_control() {
        let w = TestWindow::new();
        let got = uia_with_budget(Some(w.edit));
        assert_eq!(got.as_deref(), Some("hello"));
        // Nothing selected: no text.
        unsafe { SendMessageW(HWND(w.edit as *mut _), EM_SETSEL, Some(windows::Win32::Foundation::WPARAM(3)), Some(windows::Win32::Foundation::LPARAM(3))) };
        assert_eq!(uia_with_budget(Some(w.edit)), None);
    }

    #[test]
    #[ignore = "creates a window and uses the real clipboard (saved and restored)"]
    fn the_copy_fallback_against_our_own_window_restores_the_real_clipboard() {
        let w = TestWindow::new();
        let clip = Win { owner: w.top };
        let before = clip.save().expect("the clipboard can be saved (close programs that put huge data on it)");
        // Something known on the clipboard first.
        assert!(clip.set_private_text("jangan hilang \u{65E5}\u{672C}"));
        let marker = clip.save().unwrap();
        let src = Source { hwnd: w.top, pid: 4_000_000, exe: "notepad.exe".into(), class: "Edit".into() };
        let got = copy_fallback(&src, &clip, &WmCopy(w.edit), Duration::from_millis(800));
        let after = clip.save().unwrap();
        // Put the user's own clipboard back before asserting anything.
        clip.restore(&before);
        assert_eq!(got, Ok("hello".to_string()));
        let strip = |s: &Saved| s.formats.iter().filter(|(f, _)| *f < 0xC000).cloned().collect::<Vec<_>>();
        assert_eq!(strip(&after), strip(&marker), "the clipboard was restored exactly");
    }

    #[test]
    #[ignore = "creates a window, may take the foreground, uses the real clipboard"]
    fn ctrl_c_goes_only_to_our_own_window_and_only_when_it_is_in_front() {
        let w = TestWindow::new();
        unsafe {
            let _ = SetForegroundWindow(HWND(w.top as *mut _));
            // Give the control the focus on the window's own thread.
            SendMessageW(HWND(w.top as *mut _), WM_APP + 1, None, None);
        }
        std::thread::sleep(Duration::from_millis(300));
        let fg = unsafe { GetForegroundWindow() }.0 as isize;
        if fg != w.top {
            eprintln!("skipped: Windows did not let the test window take the foreground, so no key is sent");
            return;
        }
        let clip = Win { owner: w.top };
        let before = clip.save().expect("the clipboard can be saved");
        let src = Source { hwnd: w.top, pid: 4_000_000, exe: "notepad.exe".into(), class: "Edit".into() };
        let keys = CtrlKey { expect: w.top, only_pid: Some(std::process::id()) };
        let got = copy_fallback(&src, &clip, &keys, Duration::from_millis(800));
        clip.restore(&before);
        assert_eq!(got, Ok("hello".to_string()));
    }
}
