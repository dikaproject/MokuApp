//! Feature `earbuds` (v0.2, track H): Bluetooth headset / earbuds pill.
//!
//! When a paired Bluetooth audio device connects, the island shows its name and battery level
//! (headphone glyph, name, battery gauge, percent); a level that drops to 20 / 10 / 5 % gets a
//! warning pill; the Kontrol tab's Bluetooth tile shows the connected device. A device that does
//! not report a level shows no percent. How the level is read, and why, is in `battery.rs`.
//!
//! * `model.rs`   - device list, what a new snapshot means, colours and text (pure, tested)
//! * `battery.rs` - the level from Windows' device-node property store (CfgMgr32)
//! * `watch.rs`   - the service: WinRT `BluetoothDevice` connection events, level re-reads
//!
//! Debug path: `mokuctl debug click earbuds.sample:<connect|connect-nobattery|update:<pct>|low|off>`
//! pushes fake snapshots through the same code as the service.

mod battery;
mod model;
mod watch;

use model::{Device, Event, Warned, battery_color};

use crate::activity::{Activity, priority};
use crate::app::{App, Redraw};
use crate::i18n::tr;
use crate::registry::*;
use crate::ui::*;

/// How long the connect pill stays (it is updated in place when the level arrives late).
const CONNECT_SECS: f32 = 4.0;
const LOW_SECS: f32 = 6.0;
const CONNECT_ID: &str = "earbuds.connected";
const LOW_ID: &str = "earbuds.low";
/// Id of the fake device the debug samples use.
const SAMPLE_ID: &str = "SAMPLE";

#[derive(Default)]
pub struct Earbuds {
    devices: Vec<Device>,
    /// A first snapshot has been seen (before that, nothing pops up).
    baseline: bool,
    warned: Warned,
    /// The device the connect pill is about.
    pill: Option<String>,
}

pub fn register(r: &mut Registry) {
    r.add_state(Earbuds::default());
    r.on_action("earbuds", on_action);
    r.on_config(|app| {
        if !app.config().power.earbuds {
            let st = app.state_mut::<Earbuds>();
            if st.baseline || !st.devices.is_empty() {
                *st = Earbuds::default();
                app.clear_activity(CONNECT_ID);
                app.clear_activity(LOW_ID);
                app.request_redraw(Redraw::All);
            }
        }
    });
    r.add_service(ServiceSpec { name: "earbuds", enabled: |c| c.power.earbuds, run: watch::run });
}

/// "Buds Pro · 82%" for the Kontrol tab's Bluetooth tile, `None` when no audio device is connected.
pub fn connected_summary(app: &App) -> Option<String> {
    model::summary(&app.state::<Earbuds>().devices)
}

/// A snapshot of the audio devices from the service (or a debug sample).
pub(crate) fn on_snapshot(app: &mut App, devices: Vec<Device>) {
    if !app.config().power.earbuds {
        return;
    }
    let st = app.state_mut::<Earbuds>();
    let prev = st.baseline.then(|| st.devices.clone());
    let events = model::reduce(prev.as_deref(), &devices, &mut st.warned);
    st.devices = devices;
    st.baseline = true;
    for e in events {
        match e {
            Event::Connected(id) => {
                app.state_mut::<Earbuds>().pill = Some(id);
                app.set_activity(Activity::transient(CONNECT_ID, priority::MEETING, CONNECT_SECS, connect_pill).tab("control"));
            }
            Event::Disconnected(id) => {
                if app.state::<Earbuds>().pill.as_deref() == Some(&id) {
                    app.clear_activity(CONNECT_ID);
                }
            }
            Event::Low { id, level } => {
                app.set_activity(Activity::transient(LOW_ID, priority::WARNING, LOW_SECS, move |app| low_pill(app, &id, level)).tab("control"));
            }
            // The pill and the tab read the state: a redraw is all an update needs.
            Event::Battery { .. } => {}
        }
    }
    app.request_redraw(Redraw::All);
}

fn device<'a>(app: &'a App, id: &str) -> Option<&'a Device> {
    app.state::<Earbuds>().devices.iter().find(|d| d.id == id)
}

/// Battery gauge and percent, or the word "connected" when the level is unknown.
fn level_nodes(battery: Option<u8>) -> Vec<Node> {
    match battery {
        Some(b) => vec![battery_gauge(b), text(format!("{b}%")).secondary()],
        None => vec![caption(tr("tersambung", "connected"))],
    }
}

fn battery_gauge(percent: u8) -> Node {
    battery(f32::from(percent) / 100.0).size(22.0, 11.0).color(battery_color(percent))
}

fn connect_pill(app: &App) -> Node {
    let st = app.state::<Earbuds>();
    let d = st.pill.as_deref().and_then(|id| device(app, id));
    let (name, level) = d.map_or((String::new(), None), |d| (d.name.clone(), d.battery));
    let mut items = vec![icon(icons::HEADPHONE).color(color::TEAL)];
    if !name.is_empty() {
        items.push(text(name).semibold().max_w(220.0));
    }
    items.extend(level_nodes(level));
    pill(items)
}

fn low_pill(app: &App, id: &str, level: u8) -> Node {
    let name = device(app, id).map(|d| d.name.clone()).unwrap_or_default();
    let now = device(app, id).and_then(|d| d.battery).unwrap_or(level);
    pill([
        icon(icons::HEADPHONE).color(color::RED),
        text(if name.is_empty() { tr("Baterai earbuds", "Earbuds battery").to_string() } else { name }).semibold().max_w(220.0),
        battery_gauge(now),
        text(format!("{now}%")).secondary(),
        caption(tr("lemah", "low")),
    ])
}

fn on_action(app: &mut App, ev: &ActionEvent) {
    if ev.name() != "sample" || !matches!(ev.kind, ActionKind::Click) {
        return;
    }
    let sample = |connected: bool, battery: Option<u8>| Device { id: SAMPLE_ID.into(), name: "Buds Pro".into(), connected, battery };
    let arg = ev.arg().unwrap_or("");
    let (kind, value) = arg.split_once(':').unwrap_or((arg, ""));
    // A known starting point, so the samples go through the real "what changed" logic.
    let ready = |app: &mut App, connected: bool, battery: Option<u8>| {
        let st = app.state_mut::<Earbuds>();
        st.baseline = true;
        st.devices = vec![sample(connected, battery)];
        st.warned = Warned::default();
    };
    match kind {
        "connect" => {
            ready(app, false, None);
            on_snapshot(app, vec![sample(true, Some(82))]);
        }
        "connect-nobattery" => {
            ready(app, false, None);
            on_snapshot(app, vec![sample(true, None)]);
        }
        // `update:<percent>`: the level changes while connected (the pill updates in place).
        "update" => {
            let pct = value.parse::<u8>().unwrap_or(64).min(100);
            on_snapshot(app, vec![sample(true, Some(pct))]);
        }
        "low" => {
            ready(app, true, Some(24));
            on_snapshot(app, vec![sample(true, Some(18))]);
        }
        "off" => {
            on_snapshot(app, vec![sample(false, None)]);
            let st = app.state_mut::<Earbuds>();
            st.devices.retain(|d| d.id != SAMPLE_ID);
            st.pill = None;
            app.clear_activity(CONNECT_ID);
            app.clear_activity(LOW_ID);
            app.request_redraw(Redraw::All);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All text of a node tree, in order.
    fn texts(n: &Node, out: &mut Vec<String>) {
        match &n.kind {
            Kind::Text(t) => out.push(t.s.clone()),
            Kind::Row(c) | Kind::Col(c) | Kind::Stack(c) => c.iter().for_each(|c| texts(c, out)),
            _ => {}
        }
    }

    fn has_battery(n: &Node) -> bool {
        match &n.kind {
            Kind::Battery { .. } => true,
            Kind::Row(c) | Kind::Col(c) | Kind::Stack(c) => c.iter().any(has_battery),
            _ => false,
        }
    }

    #[test]
    fn a_known_level_shows_gauge_and_percent() {
        let nodes = level_nodes(Some(82));
        let row = row(nodes);
        let mut t = vec![];
        texts(&row, &mut t);
        assert_eq!(t, vec!["82%"]);
        assert!(has_battery(&row));
        // The gauge is green until 20 %, then orange, then red.
        let Kind::Battery { value, color, .. } = &battery_gauge(82).kind else { panic!("battery") };
        assert!((value - 0.82).abs() < 1e-6 && *color == color::GREEN);
        let Kind::Battery { color, .. } = &battery_gauge(8).kind else { panic!("battery") };
        assert_eq!(*color, color::RED);
    }

    #[test]
    fn an_unknown_level_shows_no_percent_and_no_gauge() {
        let row = row(level_nodes(None));
        let mut t = vec![];
        texts(&row, &mut t);
        assert_eq!(t.len(), 1, "just the word 'connected': {t:?}");
        assert!(!t[0].contains('%'));
        assert!(!has_battery(&row));
    }
}
