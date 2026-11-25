//! Feature `system` — Task 3 (track B).
//!
//! System pills (priorities from the plan):
//!
//! * **Battery** (65): plugged / unplugged / low (20, 10, 5 %). `GetSystemPowerStatus`, woken by
//!   `WM_POWERBROADCAST` and `RegisterPowerSettingNotification`; nothing at all on a PC without
//!   a battery.
//! * **Caps Lock** (OSD 70, 1.5 s): Raw Input on a message-only window, reacting to `VK_CAPITAL`
//!   only, then `GetKeyState`. No other key is ever stored or logged, no `WH_KEYBOARD_LL` hook.
//! * **Internet** (65): `INetworkListManager` connectivity events: lost / restored.
//!
//! Caps Lock and the network events share one service thread (`service.rs`); battery runs on the
//! UI thread from window messages. Debug path for states that can't be produced on demand:
//! `mokuctl debug click system.sample:<caps-on|caps-off|battery-low|battery-plugged[:<percent>]|battery-plugged-idle|battery-unplugged|net-lost|net-back>`.

mod charging;
mod service;

use std::time::Duration;

use windows::Win32::Foundation::{HANDLE, LPARAM, WPARAM};
use windows::Win32::System::Power::{GetSystemPowerStatus, HPOWERNOTIFY, RegisterPowerSettingNotification, SYSTEM_POWER_STATUS, UnregisterPowerSettingNotification};
use windows::Win32::System::SystemServices::{GUID_ACDC_POWER_SOURCE, GUID_BATTERY_PERCENTAGE_REMAINING};
use windows::Win32::UI::WindowsAndMessaging::{DEVICE_NOTIFY_WINDOW_HANDLE, PBT_APMPOWERSTATUSCHANGE, PBT_POWERSETTINGCHANGE, WM_POWERBROADCAST};

use crate::activity::{HEADS_UP_SECS, OSD_SECS, priority};
use crate::app::App;
use crate::i18n::{tr, tr_s};
use crate::registry::*;
use crate::ui::*;

pub fn register(r: &mut Registry) {
    r.add_state(System::default());
    r.on_action("system", on_action);
    r.on_config(on_config);
    r.on_start(on_start);
    r.on_exit(on_exit);
    r.on_message(WM_POWERBROADCAST, on_power);
    r.add_service(ServiceSpec { name: "system", enabled: |c| c.system.caps_lock || c.system.network, run: service::run });
}

// ---- battery model (pure, tested) ---------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Battery {
    pub percent: u8,
    /// On AC power.
    pub plugged: bool,
    pub charging: bool,
}

/// From `SYSTEM_POWER_STATUS` fields: `None` when there is no battery or its state is unknown.
pub fn battery_from_status(ac_line: u8, flag: u8, percent: u8) -> Option<Battery> {
    // BatteryFlag: 128 = no system battery, 255 = unknown; 8 = charging. Percent 255 = unknown.
    if flag & 128 != 0 || flag == 255 || percent > 100 {
        return None;
    }
    Some(Battery { percent, plugged: ac_line == 1, charging: flag & 8 != 0 })
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BatteryEvent {
    Plugged,
    Unplugged,
    /// Dropped to this level (20, 10 or 5).
    Low(u8),
}

/// The lowest level already warned about since the last time the battery was healthy / plugged.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct LowWarned(pub Option<u8>);

fn low_level(percent: u8) -> Option<u8> {
    [5, 10, 20].into_iter().find(|&l| percent <= l)
}

/// What the change from `prev` to `cur` deserves a pill for. No events without a previous
/// reading (nothing at startup). A low warning replaces the "unplugged" pill when both apply.
pub fn battery_events(prev: Option<Battery>, cur: Battery, warned: &mut LowWarned) -> Vec<BatteryEvent> {
    if cur.plugged || cur.percent > 20 {
        warned.0 = None;
    }
    let Some(prev) = prev else { return vec![] };
    if !cur.plugged
        && let Some(l) = low_level(cur.percent)
        && warned.0.is_none_or(|w| l < w)
    {
        warned.0 = Some(l);
        return vec![BatteryEvent::Low(l)];
    }
    match (prev.plugged, cur.plugged) {
        (false, true) => vec![BatteryEvent::Plugged],
        (true, false) => vec![BatteryEvent::Unplugged],
        _ => vec![],
    }
}

fn read_battery() -> Option<Battery> {
    let mut s = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut s).ok()? };
    battery_from_status(s.ACLineStatus, s.BatteryFlag, s.BatteryLifePercent)
}

// ---- state --------------------------------------------------------------------------------------

#[derive(Default)]
struct System {
    battery: Option<Battery>,
    warned: LowWarned,
    power_notify: Vec<HPOWERNOTIFY>,
    caps_on: Option<bool>,
    /// Internet connectivity as last reported (`None` until the first report).
    net: Option<bool>,
    /// Bumped on every "lost" report: a delayed alert only fires if no newer report arrived.
    net_gen: u32,
    /// A "lost" pill was shown, so "restored" deserves one too.
    net_lost_shown: bool,
    /// Which service features were on when the service last started (`on_config` restarts it on change).
    svc: Option<(bool, bool)>,
}

// ---- battery ------------------------------------------------------------------------------------

fn on_start(app: &mut App) {
    let hwnd = app.main_hwnd();
    let st = app.state_mut::<System>();
    st.battery = read_battery();
    if st.battery.is_none() {
        return; // desktop PC: nothing to watch
    }
    for guid in [GUID_ACDC_POWER_SOURCE, GUID_BATTERY_PERCENTAGE_REMAINING] {
        match unsafe { RegisterPowerSettingNotification(HANDLE(hwnd.0), &guid, DEVICE_NOTIFY_WINDOW_HANDLE) } {
            Ok(h) => st.power_notify.push(h),
            Err(e) => log_warn!("system: power notification failed: {e}"),
        }
    }
}

fn on_exit(app: &mut App) {
    for h in std::mem::take(&mut app.state_mut::<System>().power_notify) {
        unsafe {
            let _ = UnregisterPowerSettingNotification(h);
        }
    }
}

/// `WM_POWERBROADCAST`. The `lparam` of `PBT_POWERSETTINGCHANGE` points at data that is only
/// valid during the message, and handlers may run deferred, so it is never read: the status is
/// simply read again.
fn on_power(app: &mut App, wp: WPARAM, _lp: LPARAM) {
    if matches!(wp.0 as u32, PBT_APMPOWERSTATUSCHANGE | PBT_POWERSETTINGCHANGE) {
        check_battery(app);
    }
}

fn check_battery(app: &mut App) {
    let Some(cur) = read_battery() else { return };
    let enabled = app.config().system.battery;
    let st = app.state_mut::<System>();
    let events = battery_events(st.battery, cur, &mut st.warned);
    st.battery = Some(cur);
    if enabled {
        for e in events {
            show_battery(app, cur, e);
        }
    }
}

fn battery_glyph(percent: u8) -> String {
    // Segoe MDL2 Assets: Battery0..Battery9 = E850..E859, Battery10 = E83F.
    let cp = if percent >= 100 { 0xE83F } else { 0xE850 + (percent / 10) as u32 };
    char::from_u32(cp).unwrap_or('\u{E83F}').to_string()
}

fn show_battery(app: &mut App, b: Battery, e: BatteryEvent) {
    let kind = charging::pill_kind(e, b);
    let animated = charging::animates(kind, app.config().power.charging_animation);
    app.alert("system.battery", priority::WARNING, charging::show_secs(kind, animated), move |_| battery_pill(b, e, animated));
}

/// The pill of a battery event. `animated`: the plugged-in pill shows the battery gauge with
/// the charging animation (bigger than a normal pill, so the island visibly grows for it).
fn battery_pill(b: Battery, e: BatteryEvent, animated: bool) -> Node {
    let pct = format!("{}%", b.percent);
    match e {
        BatteryEvent::Plugged => {
            let word = if b.charging {
                tr("Mengisi", "Charging")
            } else if b.percent >= 100 {
                tr("Terisi penuh", "Fully charged")
            } else {
                tr("Terhubung ke listrik", "Plugged in")
            };
            if animated {
                return charging_pill(b, word, pct);
            }
            pill([icon(icons::LIGHTNING).color(color::GREEN), text(word).semibold(), text(pct).secondary()])
        }
        BatteryEvent::Unplugged => pill([icon(&battery_glyph(b.percent)), text(tr("Baterai", "Battery")).semibold(), text(pct).secondary(), caption(tr("tidak mengisi", "on battery"))]),
        BatteryEvent::Low(_) => pill([icon(&battery_glyph(b.percent)).color(color::RED), text(tr_s(format!("Baterai {pct}"), format!("Battery {pct}"))).semibold(), caption(tr("lemah", "low"))]),
    }
}

/// Battery outline, level bar growing to the current level, glint, soft glow and a lightning
/// bolt that pops in: all of it compositor animations (`Node::charging`), nothing to drive.
fn charging_pill(b: Battery, word: &str, pct: String) -> Node {
    row([battery(charging::fill_target(b.percent)).size(54.0, 26.0).charging(), text(word).semibold(), text(pct).secondary()]).h(44.0).pad4(14.0, 0.0, 18.0, 0.0).gap(10.0)
}

// ---- Caps Lock ----------------------------------------------------------------------------------

/// Posted by the input thread after Caps Lock was pressed (`on` = the state after the key).
fn caps_changed(app: &mut App, on: bool) {
    app.state_mut::<System>().caps_on = Some(on);
    if app.config().system.caps_lock {
        show_caps(app, on);
    }
}

fn show_caps(app: &mut App, on: bool) {
    app.alert("system.caps", priority::OSD, OSD_SECS, move |_| {
        // Segoe MDL2 "Up" arrow stands in for the mockup's caps-lock arrow.
        pill([icon("\u{E74A}").color(if on { color::TEAL } else { color::TEXT3 }), text(if on { tr("Caps Lock aktif", "Caps Lock on") } else { tr("Caps Lock mati", "Caps Lock off") }).semibold()])
    });
}

// ---- internet -----------------------------------------------------------------------------------

/// Posted by the network thread: the first report is the baseline (no pill), later changes
/// raise the pills. "Lost" waits 1.5 s so a flap that heals at once stays silent.
fn net_changed(app: &mut App, connected: bool) {
    let st = app.state_mut::<System>();
    let prev = st.net.replace(connected);
    if prev.is_none() || prev == Some(connected) || !app.config().system.network {
        return;
    }
    let st = app.state_mut::<System>();
    st.net_gen = st.net_gen.wrapping_add(1);
    let gen_now = st.net_gen;
    if connected {
        if std::mem::take(&mut st.net_lost_shown) {
            show_net(app, true);
        }
    } else {
        app.after(Duration::from_millis(1500), move |app| {
            let st = app.state_mut::<System>();
            if st.net == Some(false) && st.net_gen == gen_now {
                st.net_lost_shown = true;
                show_net(app, false);
            }
        });
    }
}

fn show_net(app: &mut App, connected: bool) {
    app.alert("system.net", priority::WARNING, if connected { 3.0 } else { HEADS_UP_SECS }, move |_| {
        if connected {
            pill([icon(icons::WIFI).color(color::GREEN), text(tr("Internet tersambung kembali", "Internet is back")).semibold()])
        } else {
            pill([icon(icons::WIFI).color(color::RED), text(tr("Internet terputus", "Internet lost")).semibold(), text(tr("menyambung ulang…", "reconnecting…")).secondary()])
        }
    });
}

// ---- config + debug samples ---------------------------------------------------------------------

fn on_config(app: &mut App) {
    let c = &app.config().system;
    let now = (c.caps_lock, c.network);
    let st = app.state_mut::<System>();
    let changed = st.svc.is_some_and(|old| old != now);
    st.svc = Some(now);
    if changed {
        // The input / network watchers are only set up for the features that are on.
        app.restart_service("system");
    }
}

fn on_action(app: &mut App, ev: &ActionEvent) {
    if ev.name() != "sample" || !matches!(ev.kind, ActionKind::Click) {
        return;
    }
    let b = |percent, plugged| Battery { percent, plugged, charging: plugged && percent < 100 };
    match ev.arg() {
        Some("caps-on") => show_caps(app, true),
        Some("caps-off") => show_caps(app, false),
        Some("battery-low") => show_battery(app, b(12, false), BatteryEvent::Low(20)),
        // `battery-plugged` (48 %) or `battery-plugged:<percent>`: the charging pill. With a level
        // of 100 it is the "full" pill; `battery-plugged-idle` is plugged in but not charging.
        Some(a) if a == "battery-plugged" || a.starts_with("battery-plugged:") => {
            let pct = a.split_once(':').and_then(|(_, n)| n.parse::<u8>().ok()).unwrap_or(48).min(100);
            show_battery(app, b(pct, true), BatteryEvent::Plugged)
        }
        Some("battery-plugged-idle") => show_battery(app, Battery { percent: 80, plugged: true, charging: false }, BatteryEvent::Plugged),
        Some("battery-unplugged") => show_battery(app, b(83, false), BatteryEvent::Unplugged),
        Some("net-lost") => show_net(app, false),
        Some("net-back") => show_net(app, true),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_from_status_fields() {
        // No system battery (flag 128) / unknown (255) -> nothing to show.
        assert_eq!(battery_from_status(1, 128, 255), None);
        assert_eq!(battery_from_status(255, 255, 255), None);
        // Charging on AC.
        assert_eq!(battery_from_status(1, 8, 48), Some(Battery { percent: 48, plugged: true, charging: true }));
        // AC, full (not charging).
        assert_eq!(battery_from_status(1, 1, 100), Some(Battery { percent: 100, plugged: true, charging: false }));
        // On battery.
        assert_eq!(battery_from_status(0, 2, 83), Some(Battery { percent: 83, plugged: false, charging: false }));
        // Percent unknown (255) -> no reading.
        assert_eq!(battery_from_status(0, 2, 255), None);
    }

    fn b(percent: u8, plugged: bool) -> Battery {
        Battery { percent, plugged, charging: plugged && percent < 100 }
    }

    #[test]
    fn battery_events_plug_and_unplug() {
        let mut w = LowWarned::default();
        assert_eq!(battery_events(Some(b(50, false)), b(50, true), &mut w), vec![BatteryEvent::Plugged]);
        assert_eq!(battery_events(Some(b(50, true)), b(50, false), &mut w), vec![BatteryEvent::Unplugged]);
        // Percent change alone: nothing.
        assert!(battery_events(Some(b(50, false)), b(49, false), &mut w).is_empty());
        // First reading: nothing (no alert at startup).
        assert!(battery_events(None, b(50, false), &mut w).is_empty());
    }

    #[test]
    fn battery_events_low_levels_fire_once() {
        let mut w = LowWarned::default();
        assert!(battery_events(Some(b(22, false)), b(21, false), &mut w).is_empty());
        assert_eq!(battery_events(Some(b(21, false)), b(20, false), &mut w), vec![BatteryEvent::Low(20)]);
        assert!(battery_events(Some(b(20, false)), b(19, false), &mut w).is_empty(), "already warned at 20");
        assert_eq!(battery_events(Some(b(11, false)), b(10, false), &mut w), vec![BatteryEvent::Low(10)]);
        assert_eq!(battery_events(Some(b(6, false)), b(5, false), &mut w), vec![BatteryEvent::Low(5)]);
        // Plugging in resets the warnings; unplugging while still low warns again.
        assert_eq!(battery_events(Some(b(5, false)), b(5, true), &mut w), vec![BatteryEvent::Plugged]);
        assert_eq!(battery_events(Some(b(5, true)), b(5, false), &mut w), vec![BatteryEvent::Low(5)]);
    }

    #[test]
    fn battery_events_no_low_warning_while_charging() {
        let mut w = LowWarned::default();
        assert!(battery_events(Some(b(10, true)), b(9, true), &mut w).is_empty());
    }

    /// The battery gauge of a pill, if it has one: (level, charging animation).
    fn gauge(n: &Node) -> Option<(f32, bool)> {
        match &n.kind {
            Kind::Battery { value, charging, .. } => Some((*value, *charging)),
            Kind::Row(c) | Kind::Col(c) | Kind::Stack(c) => c.iter().find_map(gauge),
            _ => None,
        }
    }

    #[test]
    fn plugged_pill_carries_the_animated_gauge_at_the_current_level() {
        let node = battery_pill(b(48, true), BatteryEvent::Plugged, true);
        assert_eq!(gauge(&node), Some((0.48, true)));
        // A bigger pill than the standard 36 DIP one, so the island visibly grows for it.
        assert_eq!(node.h, Len::Px(44.0));
        // Setting off, or any other event: the plain pills, no gauge.
        assert_eq!(gauge(&battery_pill(b(48, true), BatteryEvent::Plugged, false)), None);
        assert_eq!(gauge(&battery_pill(b(83, false), BatteryEvent::Unplugged, false)), None);
        assert_eq!(gauge(&battery_pill(b(12, false), BatteryEvent::Low(20), false)), None);
        // Full battery: the bar goes all the way.
        assert_eq!(gauge(&battery_pill(Battery { percent: 100, plugged: true, charging: false }, BatteryEvent::Plugged, true)), Some((1.0, true)));
    }

    #[test]
    fn battery_glyphs_follow_the_level() {
        assert_eq!(battery_glyph(0), "\u{E850}");
        assert_eq!(battery_glyph(12), "\u{E851}");
        assert_eq!(battery_glyph(99), "\u{E859}");
        assert_eq!(battery_glyph(100), "\u{E83F}");
    }
}
