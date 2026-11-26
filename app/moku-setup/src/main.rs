//! `moku-setup.exe`: per-user installer and uninstaller for Moku. No admin rights, no installer
//! framework: the release binaries are embedded (LZMS) by `build.rs`.
//!
//! ```text
//! moku-setup.exe                      window: install (or update an existing installation)
//! moku-setup.exe /S [/noautostart] [/nolaunch]
//!                                     silent install (autostart on unless /noautostart)
//! moku-setup.exe /S /update           silent update (the island's updater): keeps autostart as is
//! moku-setup.exe --uninstall [/S [/deletedata]]
//!                                     window (asks about user data) or silent (keeps it)
//! ```
//!
//! Exit codes: 0 done (or cancelled), 1 failed, 2 another setup is running, 3 no payload.
//!
//! Where it writes: `moku_core::install::Layout` — `%LOCALAPPDATA%\Programs\Moku`, the Start Menu
//! shortcut, `HKCU\...\Uninstall\Moku`, the `Moku` Run value. `MOKU_INSTALL_ROOT` and
//! `MOKU_REG_ROOT` move all of it (tests); the window then shows "MODE UJI".
//!
//! Test flags: `--preview <ready|update|working|done|error|uninstall|uninstall-done>` shows a
//! state without doing anything; `--go` presses the main button on start.
//! A log goes to `%TEMP%\moku-setup.log`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod diag;
mod install;
mod payload;
mod procs;
mod reg;
mod shortcut;
mod swap;

use std::io::Write;
use std::time::Duration;

use moku_core::install::{ISLAND_EXE, Layout};
use slint::ComponentHandle;

use install::{Autostart, InstallOptions, UninstallOptions};

slint::slint! {
    #[edition = "1.19"]
    import { SetupWindow, Setup } from "../ui/setup.slint";
    import { Theme } from "../../moku-settings/ui/theme.slint";
    export { SetupWindow, Setup, Theme }
}

/// How long running Moku processes get to close their windows before they are terminated.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default, Debug)]
struct Args {
    silent: bool,
    uninstall: bool,
    update: bool,
    no_autostart: bool,
    no_launch: bool,
    delete_data: bool,
    preview: Option<String>,
    go: bool,
}

fn parse_args(args: impl Iterator<Item = String>) -> Args {
    let mut a = Args::default();
    let mut it = args;
    while let Some(arg) = it.next() {
        match arg.to_ascii_lowercase().trim_start_matches(['-', '/']) {
            "s" | "silent" => a.silent = true,
            "uninstall" => a.uninstall = true,
            "update" => a.update = true,
            "noautostart" => a.no_autostart = true,
            "nolaunch" => a.no_launch = true,
            "deletedata" => a.delete_data = true,
            "go" => a.go = true,
            "preview" => a.preview = it.next(),
            _ => {}
        }
    }
    a
}

/// Append a line to `%TEMP%\moku-setup.log`.
pub fn log(msg: &str) {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("moku-setup.log")) {
        let _ = writeln!(f, "{secs} [{}] {msg}", std::process::id());
    }
}

fn main() {
    let args = parse_args(std::env::args().skip(1));
    let layout = Layout::from_env();
    log(&format!("moku-setup {} {:?}", moku_core::VERSION, args));
    if layout.overridden {
        log(&format!("overrides: dir {} key {} run {}", layout.install_dir.display(), layout.uninstall_key, layout.run_key));
    }
    let code = if args.preview.is_some() {
        run_gui(&args, layout)
    } else {
        let Some(_guard) = single_instance() else {
            log("another moku-setup is running");
            std::process::exit(2);
        };
        if args.silent { run_silent(&args, &layout) } else { run_gui(&args, layout) }
    };
    install::run_after_exit();
    std::process::exit(code);
}

struct Guard(windows::Win32::Foundation::HANDLE);
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

fn single_instance() -> Option<Guard> {
    use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::Threading::CreateMutexW;
    // An update started by the island may overlap a setup that is just exiting: wait a little.
    for _ in 0..40 {
        let h = unsafe { CreateMutexW(None, true, windows::core::w!("Local\\MokuSetup.SingleInstance")) }.ok()?;
        if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
            return Some(Guard(h));
        }
        unsafe {
            let _ = CloseHandle(h);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    None
}

fn launch(layout: &Layout) -> bool {
    let exe = layout.exe(ISLAND_EXE);
    match std::process::Command::new(&exe).current_dir(&layout.install_dir).spawn() {
        Ok(_) => true,
        Err(e) => {
            log(&format!("cannot start {}: {e}", exe.display()));
            false
        }
    }
}

fn install_options(args: &Args) -> InstallOptions {
    InstallOptions {
        autostart: if args.update {
            Autostart::Keep
        } else if args.no_autostart {
            Autostart::Off
        } else {
            Autostart::On
        },
        setup_exe: std::env::current_exe().ok(),
        stop_timeout: STOP_TIMEOUT,
        pace: swap::Pace::real(),
    }
}

fn run_silent(args: &Args, layout: &Layout) -> i32 {
    let progress = |_: f32, s: &str| log(s);
    let r = if args.uninstall {
        install::uninstall(layout, &UninstallOptions { delete_data: args.delete_data, stop_timeout: STOP_TIMEOUT }, &progress)
    } else {
        if !payload::present() {
            log("no payload in this build");
            return 3;
        }
        install::install(layout, payload::FILES, &install_options(args), &progress).map(|_| {
            if !args.no_launch {
                launch(layout);
            }
        })
    };
    match r {
        Ok(()) => {
            log("done");
            0
        }
        Err(e) => {
            log(&format!("failed: {e}"));
            1
        }
    }
}

fn fonts() -> (&'static str, &'static str) {
    let dir = std::env::var_os("WINDIR").map(std::path::PathBuf::from).unwrap_or_else(|| r"C:\Windows".into()).join("Fonts");
    let has = |f: &str| dir.join(f).exists();
    (
        if has("SegUIVar.ttf") { "Segoe UI Variable Text" } else { "Segoe UI" },
        if has("SegoeIcons.ttf") { "Segoe Fluent Icons" } else { "Segoe MDL2 Assets" },
    )
}

fn run_gui(args: &Args, layout: Layout) -> i32 {
    let ui = match SetupWindow::new() {
        Ok(u) => u,
        Err(e) => {
            log(&format!("cannot open the window: {e}"));
            return 1;
        }
    };
    let (font, icons) = fonts();
    ui.global::<Theme>().set_font(font.into());
    ui.global::<Theme>().set_icons(icons.into());

    let g = ui.global::<Setup>();
    let old = install::installed_version(&layout);
    let mode = if args.uninstall { 2 } else if old.is_some() { 1 } else { 0 };
    g.set_mode(mode);
    g.set_version(moku_core::VERSION.into());
    g.set_old_version(old.clone().filter(|v| !v.is_empty()).unwrap_or_else(|| "lama".into()).into());
    g.set_install_dir(layout.install_dir.display().to_string().into());
    g.set_data_dir(layout.data_dir.display().to_string().into());
    g.set_test_mode(layout.overridden);
    // An update keeps the user's current choice; a fresh install proposes autostart.
    let autostart_now = reg::get_string(&layout.run_key, moku_core::install::APP_KEY).is_some();
    g.set_autostart(if mode == 1 { autostart_now } else { !args.no_autostart });

    if let Some(p) = args.preview.as_deref() {
        // Screenshot states: nothing is written.
        let (mode, phase) = match p {
            "update" => (1, 0),
            "working" => (0, 1),
            "done" => (0, 2),
            "error" => (0, 3),
            "uninstall" => (2, 0),
            "uninstall-done" => (2, 2),
            _ => (0, 0),
        };
        g.set_mode(mode);
        g.set_phase(phase);
        // A preview shows the real folders of a normal install, never the scratch folders a
        // screenshot run uses (and no "MODE UJI" badge): nothing of this PC ends up in a picture.
        g.set_install_dir(r"C:\Users\Nama\AppData\Local\Programs\Moku".into());
        g.set_data_dir(r"C:\Users\Nama\AppData\Roaming\Moku".into());
        g.set_test_mode(false);
        if mode == 1 {
            g.set_old_version("0.1.0".into());
            g.set_autostart(true);
        }
        g.set_progress(0.62);
        g.set_status("Menyalin moku-settings.exe…".into());
        g.set_error("Folder C:\\Users\\kamu\\AppData\\Local\\Programs\\Moku sedang dipakai. Tutup Moku lalu coba lagi.".into());
        g.set_launched(true);
        let w = ui.as_weak();
        g.on_close(move || {
            let _ = w.upgrade().map(|u| u.hide());
        });
        g.on_start(|| {});
    } else {
        if mode != 2 && !payload::present() {
            g.set_phase(3);
            g.set_error("Pemasang ini dibuat tanpa berkas Moku. Buat dengan app\\tools\\release.ps1.".into());
        }
        wire(&ui, args, layout);
    }

    ui.window().on_close_requested({
        let w = ui.as_weak();
        move || match w.upgrade().map(|u| u.global::<Setup>().get_phase()) {
            Some(1) => slint::CloseRequestResponse::KeepWindowShown,
            _ => slint::CloseRequestResponse::HideWindow,
        }
    });

    if ui.show().is_err() {
        return 1;
    }
    dark_title_bar(&ui);
    app_icons(&ui);
    if args.go {
        let w = ui.as_weak();
        slint::Timer::single_shot(Duration::from_millis(400), move || {
            if let Some(u) = w.upgrade() {
                u.global::<Setup>().invoke_start();
            }
        });
    }
    let _ = slint::run_event_loop();
    match ui.global::<Setup>().get_phase() {
        3 => 1,
        _ => 0,
    }
}

fn wire(ui: &SetupWindow, args: &Args, layout: Layout) {
    let g = ui.global::<Setup>();
    let w = ui.as_weak();
    g.on_close(move || {
        if let Some(u) = w.upgrade() {
            let _ = u.hide();
        }
    });
    let w = ui.as_weak();
    let uninstall = args.uninstall;
    let base = install_options(args);
    let no_launch = args.no_launch;
    g.on_start(move || {
        let Some(u) = w.upgrade() else { return };
        let g = u.global::<Setup>();
        if g.get_phase() != 0 {
            return;
        }
        g.set_phase(1);
        g.set_progress(0.0);
        let delete_data = g.get_delete_data();
        let autostart = if g.get_mode() == 1 && g.get_autostart() == reg::get_string(&layout.run_key, moku_core::install::APP_KEY).is_some() {
            Autostart::Keep
        } else if g.get_autostart() {
            Autostart::On
        } else {
            Autostart::Off
        };
        let opts = InstallOptions { autostart, setup_exe: base.setup_exe.clone(), stop_timeout: base.stop_timeout, pace: base.pace };
        let layout = layout.clone();
        let w = w.clone();
        std::thread::spawn(move || {
            let report = {
                let w = w.clone();
                move |f: f32, s: &str| {
                    log(s);
                    let (w, s) = (w.clone(), s.to_string());
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(u) = w.upgrade() {
                            u.global::<Setup>().set_progress(f);
                            u.global::<Setup>().set_status(s.into());
                        }
                    });
                }
            };
            let r = if uninstall {
                install::uninstall(&layout, &UninstallOptions { delete_data, stop_timeout: STOP_TIMEOUT }, &report)
            } else {
                install::install(&layout, payload::FILES, &opts, &report)
            };
            let launched = r.is_ok() && !uninstall && !no_launch && launch(&layout);
            match &r {
                Ok(()) => log("done"),
                Err(e) => log(&format!("failed: {e}")),
            }
            let _ = slint::invoke_from_event_loop(move || {
                let Some(u) = w.upgrade() else { return };
                let g = u.global::<Setup>();
                g.set_launched(launched);
                match r {
                    Ok(()) => g.set_phase(2),
                    Err(e) => {
                        g.set_error(e.into());
                        g.set_phase(3);
                    }
                }
            });
        });
    });
}

/// Dark caption bar, like the settings app. The winit window only exists once the event loop
/// runs, so this retries on a short timer until it does (a call right after `show()` styled
/// nothing).
fn dark_title_bar(ui: &SetupWindow) {
    fn apply(w: slint::Weak<SetupWindow>, tries: u32) {
        let Some(ui) = w.upgrade() else { return };
        if !set_dark_title_bar(&ui) && tries < 40 {
            slint::Timer::single_shot(std::time::Duration::from_millis(50), move || apply(w, tries + 1));
        }
    }
    apply(ui.as_weak(), 0);
}

/// `false` while the winit window does not exist yet.
fn set_dark_title_bar(ui: &SetupWindow) -> bool {
    use slint::winit_030::WinitWindowAccessor;
    use slint::winit_030::winit::platform::windows::{Color, WindowExtWindows};
    use slint::winit_030::winit::window::Theme as WinTheme;
    ui.window()
        .with_winit_window(|w| {
            w.set_theme(Some(WinTheme::Dark));
            w.set_title_background_color(Some(Color::from_rgb(0x10, 0x16, 0x30)));
            w.set_title_text_color(Color::from_rgb(0xF2, 0xF2, 0xF7));
            w.set_resizable(false);
        })
        .is_some()
}

/// Caption and taskbar icons from the exe's resources (id 1, `assets/icon/moku.ico`): the 16 / 20 /
/// 24 px entries for the title bar and the 32 / 40 / 48 px ones for the taskbar and Alt+Tab, each
/// loaded at the window's scale so that Windows picks the entry drawn for that size.
///
/// Slint creates the winit window only once the event loop runs, so this retries on a short timer
/// until it exists. Without the resource (a build with `MOKU_NO_RESOURCES`) nothing changes.
fn app_icons(ui: &SetupWindow) {
    fn apply(w: slint::Weak<SetupWindow>, tries: u32) {
        let Some(ui) = w.upgrade() else { return };
        if !set_app_icons(&ui) && tries < 40 {
            slint::Timer::single_shot(std::time::Duration::from_millis(50), move || apply(w, tries + 1));
        }
    }
    apply(ui.as_weak(), 0);
}

/// `false` while the winit window does not exist yet.
fn set_app_icons(ui: &SetupWindow) -> bool {
    use slint::winit_030::WinitWindowAccessor;
    use slint::winit_030::winit::dpi::PhysicalSize;
    use slint::winit_030::winit::platform::windows::{IconExtWindows, WindowExtWindows};
    use slint::winit_030::winit::window::Icon;
    let scale = ui.window().scale_factor();
    let px = |logical: f32| {
        let n = (logical * scale).round().max(logical) as u32;
        Some(PhysicalSize::new(n, n))
    };
    ui.window()
        .with_winit_window(|w| {
            if let Ok(small) = Icon::from_resource(1, px(16.0)) {
                w.set_window_icon(Some(small));
            }
            if let Ok(big) = Icon::from_resource(1, px(32.0)) {
                w.set_taskbar_icon(Some(big));
            }
        })
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Args {
        parse_args(s.split_whitespace().map(str::to_string))
    }

    #[test]
    fn command_lines() {
        let a = args("/S /update");
        assert!(a.silent && a.update && !a.uninstall);
        assert_eq!(install_options(&a).autostart, Autostart::Keep, "an update keeps autostart as it is");
        let a = args("--uninstall /S /DeleteData");
        assert!(a.silent && a.uninstall && a.delete_data);
        assert_eq!(install_options(&args("/s")).autostart, Autostart::On);
        assert_eq!(install_options(&args("/S /noautostart /nolaunch")).autostart, Autostart::Off);
        assert!(args("/nolaunch").no_launch);
        assert_eq!(args("--preview done").preview.as_deref(), Some("done"));
        assert!(!args("").silent);
    }
}
