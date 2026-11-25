//! Moku settings app (`moku-settings.exe`): a Slint window (software renderer) that edits
//! `%APPDATA%\Moku\config.toml`; the island watches that file and hot-reloads it.
//!
//! * `form.rs`     – the binding layer: UI form <-> `moku_core::Config` (unit-tested, no window)
//! * `persist.rs`  – load / save through the core's atomic writer
//! * `app.rs`      – the callbacks behind the window (debounced save, autostart, wipe, onboarding)
//! * `autostart.rs`, `data.rs`, `system.rs` – registry Run key, "delete all data", Windows helpers
//! * `ui/*.slint`  – the window (sidebar + 12 pages) and the 6-step onboarding
//!
//! Flags (for testing): `--section <umum|island|tampilan|fitur|notifikasi|mokulink|pintasan|
//! plugin|privasi|ai|memori|tentang>` opens that page; `--onboarding [--step 1..6]` shows the onboarding;
//! `--confirm <delete|reset>` opens that confirmation dialog; `--size WxH` sets the window size.
//! `MOKU_DATA_DIR` redirects the config / data folder; `MOKU_AUTOSTART_KEY` the Run key.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai_page;
mod app;
mod autostart;
mod claude;
mod calendar_page;
mod data;
mod form;
mod ibadah_page;
mod launcher_page;
// The island's WinHTTP client, included as is (the "Tes koneksi" button of the AI page uses it).
#[allow(dead_code, unused_imports)]
#[path = "../../moku/src/net.rs"]
mod net;
mod ocr_page;
mod phone_page;
mod persist;
mod placement_page;
mod power;
mod q_page;
mod system;
mod today_page;
mod update_page;

use slint::ComponentHandle;

slint::slint! {
    #[edition = "1.19"]
    import { SettingsWindow, Cfg, Theme, Form, Problems, ClaudeCfg, CalendarCfg, CalUrl, PowerCfg, LauncherCfg, PlacementCfg, IbadahCfg, AgentsCfg, AgentRule, MirrorCfg, MirrorApp, TodayCfg, OcrCfg, AiCfg, AiFact, PhoneCfg } from "../ui/app.slint";
    import { UpdateCfg } from "../ui/pages-i.slint";
    export { SettingsWindow, Cfg, Theme, Form, Problems, ClaudeCfg, CalendarCfg, CalUrl, UpdateCfg, PowerCfg, LauncherCfg, PlacementCfg, IbadahCfg, AgentsCfg, AgentRule, MirrorCfg, MirrorApp, TodayCfg, OcrCfg, AiCfg, AiFact, PhoneCfg }
}

/// Page order in the sidebar (also the numbers accepted by `--section`).
const SECTIONS: [&str; 12] = ["umum", "island", "tampilan", "fitur", "notifikasi", "mokulink", "pintasan", "plugin", "privasi", "ai", "memori", "tentang"];

/// The sidebar index of "Tentang" (the last page: where the window lands after the onboarding was replayed).
pub const ABOUT_PAGE: i32 = 11;

#[derive(Default)]
struct Args {
    onboarding: bool,
    /// 0-based.
    step: i32,
    section: Option<i32>,
    confirm: i32,
    size: Option<(f32, f32)>,
    scroll: f32,
    /// Screenshots: show a sample data / config path (`C:\Users\Nama\...`) instead of the real one.
    sample_paths: bool,
}

fn parse_args() -> Args {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--onboarding" => a.onboarding = true,
            "--step" => {
                if let Some(n) = it.next().and_then(|v| v.parse::<i32>().ok()) {
                    a.step = (n - 1).clamp(0, 5);
                }
            }
            "--section" => {
                if let Some(v) = it.next() {
                    let v = v.to_lowercase();
                    a.section = SECTIONS.iter().position(|s| *s == v).map(|i| i as i32).or_else(|| v.parse::<i32>().ok().filter(|n| (0..12).contains(n)));
                }
            }
            "--confirm" => {
                a.confirm = match it.next().as_deref() {
                    Some("delete") => 1,
                    Some("reset") => 2,
                    _ => 0,
                }
            }
            "--sample-paths" => a.sample_paths = true,
            "--scroll" => a.scroll = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0),
            "--size" => {
                if let Some((w, h)) = it.next().as_deref().and_then(|v| v.split_once('x')) {
                    if let (Ok(w), Ok(h)) = (w.parse::<f32>(), h.parse::<f32>()) {
                        a.size = Some((w.clamp(640.0, 3000.0), h.clamp(480.0, 3000.0)));
                    }
                }
            }
            _ => {}
        }
    }
    a
}

fn main() -> Result<(), slint::PlatformError> {
    let args = parse_args();
    let _instance = match system::single_instance() {
        Ok(g) => g,
        Err(()) => return Ok(()),
    };
    // The data folder gets its `.moku-data` marker (what the uninstaller needs to delete it).
    let _ = moku_core::paths::ensure_data_dir();

    let (store, config, warnings) = persist::Store::open_default();
    let ui = SettingsWindow::new()?;

    // Fonts: Segoe UI Variable / Segoe Fluent Icons when installed.
    let (font, icons) = system::fonts();
    let theme = ui.global::<Theme>();
    theme.set_font(font.into());
    theme.set_icons(icons.into());

    // Which view opens: a `--section` forces the settings; otherwise the onboarding shows on the
    // first run (onboarding.completed = false) or with `--onboarding`.
    let entry_onboarding = args.section.is_none() && (args.onboarding || !config.onboarding.completed);
    let st = app::wire(&ui, store, &config, app::Env::real(), entry_onboarding);
    claude::wire(&ui, moku_core::paths::config_file());
    calendar_page::wire(&ui, moku_core::paths::config_file());
    update_page::wire(&ui, &config);
    power::wire(&ui, moku_core::paths::config_file());
    launcher_page::wire(&ui, &config);
    placement_page::wire(&ui, moku_core::paths::config_file());
    ibadah_page::wire(&ui, moku_core::paths::config_file());
    q_page::wire(&ui, moku_core::paths::config_file());
    today_page::wire(&ui, moku_core::paths::config_file());
    ocr_page::wire(&ui, &config);
    ai_page::wire(&ui, moku_core::paths::config_file());
    phone_page::wire(&ui, moku_core::paths::config_file());

    let cfg = ui.global::<Cfg>();
    cfg.set_step(args.step);
    cfg.set_page(args.section.unwrap_or(0));
    cfg.set_confirm(args.confirm);
    if args.sample_paths {
        // Display only (screenshots): nothing of this PC's folders in a picture.
        cfg.set_data_dir(r"C:\Users\Nama\AppData\Roaming\Moku".into());
        cfg.set_config_path(r"C:\Users\Nama\AppData\Roaming\Moku\config.toml".into());
    }
    theme.set_start_scroll(args.scroll);
    if let Some((w, h)) = args.size {
        ui.window().set_size(slint::LogicalSize::new(w, h));
    }
    if !warnings.is_empty() {
        app::toast(&ui, &st, &format!("config.toml: {} nilai tidak valid diganti ke bawaan", warnings.len()), 5000);
    }

    ui.show()?;
    dark_title_bar(&ui);
    app_icons(&ui);
    let r = ui.run();
    drop(st);
    r
}

/// Dark caption bar to match the window (Windows 11 also takes the colour).
///
/// Slint creates the winit window only once the event loop runs, so right after `show()` there is
/// no window to style yet (the call used to do nothing): like the icons, this retries on a short
/// timer until the window exists.
fn dark_title_bar(ui: &SettingsWindow) {
    fn apply(w: slint::Weak<SettingsWindow>, tries: u32) {
        let Some(ui) = w.upgrade() else { return };
        if !set_dark_title_bar(&ui) && tries < 40 {
            slint::Timer::single_shot(std::time::Duration::from_millis(50), move || apply(w, tries + 1));
        }
    }
    apply(ui.as_weak(), 0);
}

/// `false` while the winit window does not exist yet.
fn set_dark_title_bar(ui: &SettingsWindow) -> bool {
    use slint::winit_030::WinitWindowAccessor;
    use slint::winit_030::winit::platform::windows::{Color, WindowExtWindows};
    use slint::winit_030::winit::window::Theme as WinTheme;
    ui.window()
        .with_winit_window(|w| {
            w.set_theme(Some(WinTheme::Dark));
            w.set_title_background_color(Some(Color::from_rgb(0x10, 0x16, 0x30)));
            w.set_title_text_color(Color::from_rgb(0xF2, 0xF2, 0xF7));
        })
        .is_some()
}

/// Caption and taskbar icons from the exe's resources (id 1, `assets/icon/moku.ico`): the 16 / 20 /
/// 24 px entries for the title bar and the 32 / 40 / 48 px ones for the taskbar and Alt+Tab, each
/// loaded at the window's scale so that Windows picks the entry drawn for that size.
///
/// Slint creates the winit window only once the event loop runs, so this retries on a short timer
/// until it exists. Without the resource (a build with `MOKU_NO_RESOURCES`) nothing changes.
fn app_icons(ui: &SettingsWindow) {
    fn apply(w: slint::Weak<SettingsWindow>, tries: u32) {
        let Some(ui) = w.upgrade() else { return };
        if !set_app_icons(&ui) && tries < 40 {
            slint::Timer::single_shot(std::time::Duration::from_millis(50), move || apply(w, tries + 1));
        }
    }
    apply(ui.as_weak(), 0);
}

/// `false` while the winit window does not exist yet.
fn set_app_icons(ui: &SettingsWindow) -> bool {
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
