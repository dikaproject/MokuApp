//! Feature `control` — Task 2 (track A).
//!
//! The Kontrol tab (Control Center style): Wi-Fi / Bluetooth radio tiles, dark mode,
//! keep-awake, meeting mode (dispatches `"meeting.toggle"`, reads `app.meeting()`), volume and
//! brightness sliders (brightness hidden when the panel has none), output device chip.
//! System calls run on the `control` service (`system.rs`); it loads nothing until the tab is
//! shown, then refreshes every 2 s while the tab is visible.

mod mixer;
mod system;

use std::time::Duration;

use system::{Cmd, INBOX, Kind, Report};
use windows::Win32::System::Power::{ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState};
use windows::Win32::UI::WindowsAndMessaging::{SW_SHOWNORMAL, WM_SETTINGCHANGE};

use crate::app::{App, Redraw};
use crate::features::{earbuds, notify};
use crate::features::volume_osd::{self, Audio};
use crate::i18n::tr;
use crate::registry::*;
use crate::ui::*;

#[derive(Default)]
struct Control {
    /// `None` = not known yet / no such radio.
    wifi: Option<bool>,
    bt: Option<bool>,
    radios_known: bool,
    /// 0–100; `None` with `brightness_known` = not supported (slider hidden).
    brightness: Option<u8>,
    brightness_known: bool,
    dark: bool,
    awake: bool,
    /// Slider values while being dragged.
    vol_drag: Option<f32>,
    bright_drag: Option<f32>,
    /// Pending "release the system objects" after the tab was hidden.
    release: Option<crate::app::TimerId>,
}

/// How long the radio / WMI objects outlive the hidden tab (a quick tab switch keeps them).
const RELEASE_AFTER: Duration = Duration::from_secs(10);

pub fn register(r: &mut Registry) {
    r.add_state(Control::default());
    r.add_tab(TabSpec {
        id: "control",
        order: order::CONTROL,
        icon: icons::SLIDERS,
        label: ("Kontrol", "Controls"),
        enabled: |c| c.control.enabled,
        build: panel,
        tick: Some((Duration::from_secs(2), |app| {
            INBOX.push(Cmd::Refresh);
            mixer::tick(app);
        })),
    });
    r.on_action("control", on_action);
    mixer::register(r);
    r.add_service(ServiceSpec { name: "control", enabled: |c| c.control.enabled, run: system::run });
    r.on_start(|app| {
        app.state_mut::<Control>().dark = system::read_dark();
    });
    r.on_message(WM_SETTINGCHANGE, |app, _, _| {
        // Theme switched elsewhere (Settings app, or our own broadcast): cheap registry read.
        let dark = system::read_dark();
        if app.state::<Control>().dark != dark {
            app.state_mut::<Control>().dark = dark;
            app.request_redraw(Redraw::Tab("control"));
        }
    });
    r.on_event(|app, e| match e {
        AppEvent::MeetingChanged => app.request_redraw(Redraw::Tab("control")),
        AppEvent::TabShown("control") => {
            if let Some(t) = app.state_mut::<Control>().release.take() {
                app.cancel_timer(t);
            }
        }
        AppEvent::TabHidden("control") => {
            // Back to the tiles; the mixer's rows and icons are freed now, Core Audio later.
            mixer::on_hidden(app);
            if let Some(t) = app.state_mut::<Control>().release.take() {
                app.cancel_timer(t);
            }
            let t = app.after(RELEASE_AFTER, |app| {
                app.state_mut::<Control>().release = None;
                INBOX.push(Cmd::Release);
                mixer::release();
            });
            app.state_mut::<Control>().release = Some(t);
        }
        _ => {}
    });
    r.on_config(|app| {
        if !app.config().control.enabled && app.state::<Control>().awake {
            set_awake(app, false);
        }
    });
    r.on_exit(|app| {
        if app.state::<Control>().awake {
            set_awake(app, false);
        }
    });
}

fn on_report(app: &mut App, r: Report) {
    let c = app.state_mut::<Control>();
    match r {
        Report::Radios { wifi, bt } => {
            c.wifi = wifi;
            c.bt = bt;
            c.radios_known = true;
        }
        Report::Brightness(b) => {
            c.brightness = b;
            c.brightness_known = true;
        }
        Report::Dark(d) => c.dark = d,
    }
    app.request_redraw(Redraw::Tab("control"));
}

/// Keep the PC and display awake (per thread: the UI thread lives as long as the island).
fn set_awake(app: &mut App, on: bool) {
    let flags = if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED } else { ES_CONTINUOUS };
    let ok = unsafe { SetThreadExecutionState(flags) }.0 != 0;
    if ok {
        app.state_mut::<Control>().awake = on;
    } else {
        log_warn!("control: SetThreadExecutionState failed");
    }
    app.request_redraw(Redraw::Tab("control"));
}

fn on_action(app: &mut App, ev: &ActionEvent) {
    if ev.name().starts_with("mix") || ev.name() == "debug_mixer" {
        return mixer::on_action(app, ev);
    }
    match (ev.name(), ev.kind) {
        ("wifi", ActionKind::Click) => {
            if let Some(on) = app.state::<Control>().wifi {
                INBOX.push(Cmd::SetRadio(Kind::Wifi, !on));
            }
        }
        ("bluetooth", ActionKind::Click) => {
            if let Some(on) = app.state::<Control>().bt {
                INBOX.push(Cmd::SetRadio(Kind::Bluetooth, !on));
            }
        }
        ("dark", ActionKind::Click) => {
            let c = app.state_mut::<Control>();
            c.dark = !c.dark;
            INBOX.push(Cmd::SetDark(c.dark));
            app.request_redraw(Redraw::Tab("control"));
        }
        ("awake", ActionKind::Click) => {
            let on = !app.state::<Control>().awake;
            set_awake(app, on);
        }
        // Do-not-disturb belongs to the notification centre (`notify.dnd`); this is its Control Center tile.
        ("dnd", ActionKind::Click) => {
            app.dispatch_action("notify.dnd", ActionKind::Click);
            app.request_redraw(Redraw::Tab("control"));
        }
        ("meeting", ActionKind::Click) => {
            // Owned by the `meeting` feature (Task 3).
            app.dispatch_action("meeting.toggle", ActionKind::Click);
            app.request_redraw(Redraw::Tab("control"));
        }
        ("volume", ActionKind::Slide { value, done }) => {
            app.state_mut::<Control>().vol_drag = (!done).then_some(value);
            app.state_mut::<Audio>().volume = value;
            volume_osd::set_volume(value);
            app.request_redraw(Redraw::Tab("control"));
        }
        ("volume", ActionKind::Scroll(n)) => volume_osd::nudge(n * 0.02),
        ("brightness", ActionKind::Slide { value, done }) => {
            let c = app.state_mut::<Control>();
            c.bright_drag = (!done).then_some(value);
            c.brightness = Some((value * 100.0).round() as u8);
            INBOX.push(Cmd::SetBrightness((value * 100.0).round() as u8));
            app.request_redraw(Redraw::Tab("control"));
        }
        ("output", ActionKind::Click) => app.run_outside(|| unsafe {
            // Windows sound settings (choose the output device there).
            windows::Win32::UI::Shell::ShellExecuteW(None, windows::core::w!("open"), windows::core::w!("ms-settings:sound"), None, None, SW_SHOWNORMAL);
        }),
        _ => {}
    }
}

/// The half-filled circle ("contrast") for the dark mode tile; the moon is do-not-disturb's.
const DARK_GLYPH: &str = "\u{E793}";

/// The Bluetooth tile is tinted when the radio is on or a headset is connected.
fn bt_tile_on(radio: Option<bool>, headset_connected: bool) -> bool {
    radio == Some(true) || headset_connected
}

fn on_off(on: bool) -> &'static str {
    if on { tr("Aktif", "On") } else { tr("Mati", "Off") }
}

fn radio_status(known: bool, v: Option<bool>) -> &'static str {
    match (known, v) {
        (false, _) => "…",
        (true, None) => tr("Tidak ada", "Not available"),
        (true, Some(on)) => on_off(on),
    }
}

fn panel(app: &App) -> Node {
    if app.state::<mixer::Mixer>().open {
        return mixer::view(app);
    }
    let c = app.state::<Control>();
    let a = app.state::<Audio>();
    // System toggles use the standard accent blue; the album colour is for music UI only.
    let accent = app.base_accent();
    let meet = app.meeting();

    let radio_tile = |glyph: &str, name: &str, status: String, known: bool, v: Option<bool>, action: &str| {
        let t = toggle_tile(glyph, name.to_string(), status, v == Some(true), accent, action.to_string());
        if known && v.is_none() { t.opacity(0.45) } else { t }
    };
    // A connected headset is named on the tile ("Buds Pro · 82%"), whatever the radio query says,
    // and the tile is tinted when Bluetooth is on OR a headset is connected.
    let headset = earbuds::connected_summary(app);
    let bt_on = bt_tile_on(c.bt, headset.is_some());
    let bt_status = headset.unwrap_or_else(|| radio_status(c.radios_known, c.bt).to_string());
    let mut bt = radio_tile(icons::BLUETOOTH, "Bluetooth", bt_status, c.radios_known, c.bt, "control.bluetooth");
    if bt_on && c.bt != Some(true) {
        // Connected although the radio query says off / unknown: tinted like an "on" tile.
        bt = toggle_tile(icons::BLUETOOTH, "Bluetooth", earbuds::connected_summary(app).unwrap_or_default(), true, accent, "control.bluetooth");
    }
    // 3 + 3 like the mockup (Wi-Fi, Bluetooth, Jangan ganggu / Mode gelap, Tetap menyala, Mode rapat):
    // every tile shares its row equally.
    let dnd = notify::dnd_on(app);
    let row1 = row([
        radio_tile(icons::WIFI, "Wi-Fi", radio_status(c.radios_known, c.wifi).to_string(), c.radios_known, c.wifi, "control.wifi"),
        bt,
        toggle_tile(icons::MOON, tr("Jangan ganggu", "Do not disturb"), on_off(dnd), dnd, color::PURPLE, "control.dnd"),
    ])
    .gap(10.0)
    .fill_w();

    let meet_status = if meet.active {
        meet.app_name.clone().unwrap_or_else(|| tr("Aktif", "On").to_string())
    } else {
        tr("Mati", "Off").to_string()
    };
    let row2 = row([
        toggle_tile(DARK_GLYPH, tr("Mode gelap", "Dark mode"), on_off(c.dark), c.dark, color::BLUE, "control.dark"),
        toggle_tile("\u{E7E8}", tr("Tetap menyala", "Keep awake"), on_off(c.awake), c.awake, color::ORANGE, "control.awake"),
        toggle_tile(icons::MIC, tr("Mode rapat", "Meeting mode"), meet_status, meet.active, color::GREEN, "control.meeting"),
    ])
    .gap(10.0)
    .fill_w();

    let vol = c.vol_drag.unwrap_or(if a.muted { 0.0 } else { a.volume });
    let vol_glyph = if a.muted || vol <= 0.005 { icons::MUTE } else { icons::VOLUME };
    let mut sliders = row([slider(vol).slider_icon(vol_glyph).grow(1.0).action("control.volume")]).gap(10.0).fill_w();
    if let Some(b) = c.brightness {
        let v = c.bright_drag.unwrap_or(b as f32 / 100.0);
        sliders = sliders.child(slider(v).slider_icon(icons::BRIGHTNESS).grow(1.0).action("control.brightness"));
    }

    let out_glyph = if a.headphones { icons::HEADPHONE } else { icons::SPEAKER };
    let device = if a.device.is_empty() { tr("tidak ada", "none").to_string() } else { a.device.clone() };
    let mut chips = row([capsule(out_glyph, format!("{} {device}", tr("Keluaran:", "Output:")), "control.output").max_w(360.0)]).gap(8.0).fill_w();
    if let Some(mix) = mixer::chip(app) {
        chips = chips.child(mix);
    }
    chips = chips.child(spacer());

    col([row1, row2, sliders, chips]).gap(10.0).fill_w()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bluetooth_tile_is_tinted_when_on_or_connected() {
        assert!(bt_tile_on(Some(true), false), "radio on");
        assert!(bt_tile_on(Some(false), true), "a headset is connected although the radio query says off");
        assert!(bt_tile_on(None, true), "unknown radio, headset connected");
        assert!(!bt_tile_on(Some(false), false));
        assert!(!bt_tile_on(None, false));
    }
}
