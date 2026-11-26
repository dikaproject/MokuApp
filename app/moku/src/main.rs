//! Moku island (`moku.exe`).
//!
//! Flags (testing): `--state <idle|hover|music|music-panel|lyrics|lyrics-panel|panel>` (fixed state for
//! screenshots; with `panel`, `--tab <id>` picks the tab; `lyrics*`: a fake player with timed lyrics), `--demo` (loops the prototype states), `--blur`
//! (HostBackdropBrush glass), `--gpu` (hardware D3D for rasterising).
//! Normal run: no flags. Quit: tray → Keluar, or Ctrl+Alt+Shift+Q.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// The core exposes an API surface for the feature modules; parts of it are unused until the
// feature tracks land.
#![allow(dead_code)]

#[macro_use]
mod log;

mod activity;
mod app;
mod config_watch;
mod demo;
mod features;
mod helper;
mod hotkeys;
mod i18n;
mod imaging;
mod ipc_server;
mod net;
mod persist;
mod reentrant;
mod registry;
mod safety;
mod scene;
mod services;
mod shell;
mod ui;

use app::{App, Mode, RunMode};
use moku_core::Config;
use windows::Win32::Foundation::*;
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::System::WinRT::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

struct Args {
    run_mode: RunMode,
    state: Option<String>,
    tab: Option<String>,
}

fn parse_args() -> Args {
    let args: Vec<String> = std::env::args().collect();
    let (mut state, mut tab, mut demo) = (None, None, false);
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--state" => {
                i += 1;
                state = args.get(i).cloned();
            }
            "--tab" => {
                i += 1;
                tab = args.get(i).cloned();
            }
            "--demo" => demo = true,
            _ => {}
        }
        i += 1;
    }
    let run_mode = if demo {
        RunMode::Demo
    } else if state.is_some() {
        RunMode::Fixed
    } else {
        RunMode::Interactive
    };
    Args { run_mode, state, tab }
}

/// Another instance running? Ask it to open its panel and return true.
fn already_running() -> bool {
    unsafe {
        let _m = CreateMutexW(None, false, w!("Local\\MokuIsland.SingleInstance"));
        if GetLastError() != ERROR_ALREADY_EXISTS {
            // Keep the mutex for the life of the process.
            std::mem::forget(_m);
            return false;
        }
        if let Ok(h) = FindWindowW(shell::CLASS_VISUAL, PCWSTR::null()) {
            let _ = PostMessageW(Some(h), app::WM_APP_OPEN, WPARAM(0), LPARAM(0));
        }
        true
    }
}

fn run() -> Result<()> {
    let args = parse_args();
    if args.run_mode == RunMode::Interactive && already_running() {
        return Ok(());
    }
    if args.run_mode == RunMode::Interactive {
        // Creates the data folder with its `.moku-data` marker (the uninstaller deletes a data
        // folder only when it has one).
        let _ = moku_core::paths::ensure_data_dir();
    }
    log::init();
    log_info!("moku {} starting ({:?})", moku_core::VERSION, args.run_mode);

    let (config, warnings) = Config::load_or_default();
    for w in &warnings {
        log_warn!("{w}");
    }
    if args.run_mode == RunMode::Interactive && !moku_core::paths::config_file().exists() {
        if let Err(e) = config.save() {
            log_warn!("cannot write default config: {e}");
        }
    }

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // Composition needs a DispatcherQueue on the UI thread; it is pumped by our message loop.
        // This also makes the UI thread a COM STA.
        let _dq = CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })?;
        // OLE (drag & drop for the shelf, clipboard) on the UI thread.
        let _ = OleInitialize(None);

        let mut start_mode = Mode::Pill;
        if matches!(args.state.as_deref(), Some("music") | Some("music-panel")) || args.run_mode == RunMode::Demo {
            demo::enable();
        }
        match args.state.as_deref() {
            Some("hover") => start_mode = Mode::Hover,
            Some("music-panel") | Some("panel") | Some("lyrics-panel") => start_mode = Mode::Panel,
            _ => {}
        }

        let mons = shell::monitors::enumerate();
        let target = shell::Target::resolve(&config, &mons, None);
        let scale = target.dpi * config.island.scale;
        let mut sh = shell::Shell::create(args.run_mode, scale, target, mons)?;
        let island = scene::Island::new(sh.visual, scale, App::scene_opts(&config))?;
        let mut reg = registry::Registry::default();
        features::register_all(&mut reg);
        sh.start(&config, args.run_mode);
        let app = App::new(reg, config, sh, island, args.run_mode, start_mode);
        app::install(app);
        app::with_app(|a| {
            if matches!(args.state.as_deref(), Some("music") | Some("music-panel")) {
                demo::show_music(a);
            }
            a.start();
            // `--state lyrics|lyrics-panel`: a fake player with timed lyrics (`MOKU_DEMO_POS_MS` = where it starts).
            if matches!(args.state.as_deref(), Some("lyrics") | Some("lyrics-panel")) {
                let start = std::env::var("MOKU_DEMO_POS_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(1_000);
                features::media::demo_lyrics(a, start);
            }
            if let Some(t) = args.tab.as_deref() {
                a.open_panel(Some(t));
            } else if matches!(args.state.as_deref(), Some("music-panel") | Some("lyrics-panel")) {
                a.open_panel(Some("media"));
            }
            a.flush();
        });

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        log_info!("moku exiting");
        app::uninstall();
    }
    Ok(())
}

fn main() {
    // Helper-process modes (launcher index, icons): no island, no single-instance check, no log.
    // The island starts itself with these flags so the shell's heavy libraries stay out of it.
    if let Some(code) = helper::run_if_requested() {
        std::process::exit(code);
    }
    // Release builds abort on panic: make sure the reason reaches moku.log first.
    log::install_panic_hook();
    if let Err(e) = run() {
        log_error!("fatal: {e:?}");
        let text: Vec<u16> = format!("{e:?}\0").encode_utf16().collect();
        unsafe {
            MessageBoxW(None, PCWSTR(text.as_ptr()), w!("Moku"), MB_ICONERROR);
        }
    }
}
