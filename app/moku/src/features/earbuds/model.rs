//! The pure part of the earbuds feature: the device list, what a new snapshot of it means
//! (connected, disconnected, battery changed, low) and the text / colour helpers.
//! Tested without any Bluetooth hardware.

use std::collections::HashMap;

use crate::ui::{Rgba, color};

/// A Bluetooth audio device (headset, earbuds, headphones, speaker) that is paired.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    /// Stable id (the Bluetooth address as 12 hex digits).
    pub id: String,
    pub name: String,
    pub connected: bool,
    /// Percent. `None` = the device does not report a level (or Windows has none yet).
    pub battery: Option<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Connected(String),
    Disconnected(String),
    /// The reading changed while connected (`None` = it became unknown).
    Battery { id: String, level: Option<u8> },
    /// Dropped to 20, 10 or 5 %.
    Low { id: String, level: u8 },
}

/// The lowest level already warned about, per device, since it was last healthy / reconnected.
#[derive(Default, Debug)]
pub struct Warned(HashMap<String, u8>);

fn low_level(percent: u8) -> Option<u8> {
    [5, 10, 20].into_iter().find(|&l| percent <= l)
}

/// What the change from `prev` to `next` means. No events for the first snapshot (nothing
/// pops up at startup for headphones that were connected long ago).
pub fn reduce(prev: Option<&[Device]>, next: &[Device], warned: &mut Warned) -> Vec<Event> {
    let mut out = vec![];
    for d in next {
        // Healthy or gone: the next drop warns again.
        if !d.connected || d.battery.is_none_or(|b| b > 20) {
            warned.0.remove(&d.id);
        }
        let Some(prev) = prev else {
            if let Some(l) = d.battery.filter(|_| d.connected).and_then(low_level) {
                warned.0.insert(d.id.clone(), l); // already low at startup: no warning for it
            }
            continue;
        };
        let was = prev.iter().find(|w| w.id == d.id);
        let was_connected = was.is_some_and(|w| w.connected);
        match (was_connected, d.connected) {
            (false, true) => {
                out.push(Event::Connected(d.id.clone()));
                // The connect pill shows a low level itself; no second warning for it.
                if let Some(l) = d.battery.and_then(low_level) {
                    warned.0.insert(d.id.clone(), l);
                }
            }
            (true, false) => out.push(Event::Disconnected(d.id.clone())),
            (true, true) => {
                let before = was.and_then(|w| w.battery);
                if before != d.battery {
                    out.push(Event::Battery { id: d.id.clone(), level: d.battery });
                    if let Some(l) = d.battery.and_then(low_level)
                        && warned.0.get(&d.id).is_none_or(|&w| l < w)
                    {
                        warned.0.insert(d.id.clone(), l);
                        out.push(Event::Low { id: d.id.clone(), level: l });
                    }
                }
            }
            (false, false) => {}
        }
    }
    // A connected device that vanished from the list (unpaired) is a disconnect.
    if let Some(prev) = prev {
        for w in prev.iter().filter(|w| w.connected && !next.iter().any(|d| d.id == w.id)) {
            warned.0.remove(&w.id);
            out.push(Event::Disconnected(w.id.clone()));
        }
    }
    out
}

/// Battery colour: green, orange at 20 % and below, red at 10 % and below.
pub fn battery_color(percent: u8) -> Rgba {
    match percent {
        0..=10 => color::RED,
        11..=20 => color::ORANGE,
        _ => color::GREEN,
    }
}

/// One line for the Kontrol tab's Bluetooth tile: "Buds Pro · 82%", "Buds Pro" when the level
/// is unknown, "Buds Pro · 82% +1" with more connected devices. `None` = nothing connected.
pub fn summary(devices: &[Device]) -> Option<String> {
    let mut it = devices.iter().filter(|d| d.connected);
    let first = it.next()?;
    let more = it.count();
    let mut s = first.name.clone();
    if let Some(b) = first.battery {
        s.push_str(&format!(" · {b}%"));
    }
    if more > 0 {
        s.push_str(&format!(" +{more}"));
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str, connected: bool, battery: Option<u8>) -> Device {
        Device { id: id.into(), name: format!("Dev {id}"), connected, battery }
    }

    fn run(prev: &[Device], next: &[Device]) -> Vec<Event> {
        reduce(Some(prev), next, &mut Warned::default())
    }

    #[test]
    fn the_first_snapshot_is_silent() {
        assert!(reduce(None, &[dev("A", true, Some(80))], &mut Warned::default()).is_empty());
    }

    #[test]
    fn connect_and_disconnect() {
        let off = [dev("A", false, None)];
        let on = [dev("A", true, Some(82))];
        assert_eq!(run(&off, &on), vec![Event::Connected("A".into())]);
        assert_eq!(run(&on, &off), vec![Event::Disconnected("A".into())]);
        assert!(run(&on, &on).is_empty(), "nothing changed");
        assert!(run(&off, &off).is_empty());
    }

    #[test]
    fn a_new_paired_device_that_is_connected_counts_as_connecting() {
        assert_eq!(run(&[], &[dev("B", true, None)]), vec![Event::Connected("B".into())]);
        assert!(run(&[], &[dev("B", false, None)]).is_empty());
    }

    #[test]
    fn an_unpaired_connected_device_is_a_disconnect() {
        assert_eq!(run(&[dev("A", true, Some(50))], &[]), vec![Event::Disconnected("A".into())]);
    }

    #[test]
    fn battery_changes_are_reported_while_connected_only() {
        let a = [dev("A", true, Some(82))];
        assert_eq!(run(&a, &[dev("A", true, Some(80))]), vec![Event::Battery { id: "A".into(), level: Some(80) }]);
        // The reading went away (device stopped reporting).
        assert_eq!(run(&a, &[dev("A", true, None)]), vec![Event::Battery { id: "A".into(), level: None }]);
        // A reading arriving after connect (no level at first).
        assert_eq!(run(&[dev("A", true, None)], &a), vec![Event::Battery { id: "A".into(), level: Some(82) }]);
        // Disconnected devices keep their last level quietly.
        assert!(run(&[dev("A", false, Some(60))], &[dev("A", false, Some(55))]).is_empty());
    }

    #[test]
    fn low_levels_warn_once_each_and_reset_when_healthy() {
        let mut w = Warned::default();
        let mut prev = vec![dev("A", true, Some(25))];
        let mut step = |p: &mut Vec<Device>, b: u8| {
            let next = vec![dev("A", true, Some(b))];
            let e = reduce(Some(p), &next, &mut w);
            *p = next;
            e
        };
        let lows = |e: Vec<Event>| e.into_iter().filter_map(|e| if let Event::Low { level, .. } = e { Some(level) } else { None }).collect::<Vec<_>>();
        assert!(lows(step(&mut prev, 21)).is_empty());
        assert_eq!(lows(step(&mut prev, 20)), vec![20]);
        assert!(lows(step(&mut prev, 19)).is_empty(), "already warned at 20");
        assert_eq!(lows(step(&mut prev, 10)), vec![10]);
        assert_eq!(lows(step(&mut prev, 5)), vec![5]);
        // Charged back above 20: the next drop warns again.
        assert!(lows(step(&mut prev, 60)).is_empty());
        assert_eq!(lows(step(&mut prev, 20)), vec![20]);
    }

    #[test]
    fn connecting_while_low_does_not_warn_twice() {
        let mut w = Warned::default();
        let off = [dev("A", false, Some(12))];
        let on = [dev("A", true, Some(12))];
        let e = reduce(Some(&off), &on, &mut w);
        assert_eq!(e, vec![Event::Connected("A".into())], "the connect pill shows the level itself");
        // ...and a further small drop does not repeat the 20 % warning, only the 10 % one.
        let e = reduce(Some(&on), &[dev("A", true, Some(10))], &mut w);
        assert!(e.contains(&Event::Low { id: "A".into(), level: 10 }), "{e:?}");
    }

    #[test]
    fn battery_colours() {
        assert_eq!(battery_color(100), color::GREEN);
        assert_eq!(battery_color(21), color::GREEN);
        assert_eq!(battery_color(20), color::ORANGE);
        assert_eq!(battery_color(11), color::ORANGE);
        assert_eq!(battery_color(10), color::RED);
        assert_eq!(battery_color(0), color::RED);
    }

    #[test]
    fn summary_line_for_the_kontrol_tile() {
        assert_eq!(summary(&[]), None);
        assert_eq!(summary(&[dev("A", false, Some(50))]), None, "disconnected devices are not shown");
        let mut a = dev("A", true, Some(82));
        a.name = "Buds Pro".into();
        assert_eq!(summary(std::slice::from_ref(&a)).as_deref(), Some("Buds Pro · 82%"));
        let mut unknown = a.clone();
        unknown.battery = None;
        assert_eq!(summary(&[unknown]).as_deref(), Some("Buds Pro"), "no level, no percent");
        assert_eq!(summary(&[a, dev("B", true, Some(10))]).as_deref(), Some("Buds Pro · 82% +1"));
    }
}
