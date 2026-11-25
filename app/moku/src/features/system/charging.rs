//! The charging pill: which pill a battery event gets, how long it stays and where the level
//! bar ends. Pure, so the transitions are unit-tested; the drawing is `Node::charging`.

use super::{Battery, BatteryEvent};
use crate::activity::HEADS_UP_SECS;

/// Which battery pill is shown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PillKind {
    /// AC plugged in and charging.
    Charging,
    /// AC plugged in, battery full.
    Full,
    /// AC plugged in but not charging (charge limit, battery saver).
    Plugged,
    /// AC unplugged: a short plain pill.
    Unplugged,
    /// Dropped to 20 / 10 / 5 %.
    Low,
}

/// How long the charging pill stays (the animation is made to fit in it).
pub const CHARGING_SECS: f32 = 2.5;

pub fn pill_kind(e: BatteryEvent, b: Battery) -> PillKind {
    match e {
        BatteryEvent::Plugged if b.charging => PillKind::Charging,
        BatteryEvent::Plugged if b.percent >= 100 => PillKind::Full,
        BatteryEvent::Plugged => PillKind::Plugged,
        BatteryEvent::Unplugged => PillKind::Unplugged,
        BatteryEvent::Low(_) => PillKind::Low,
    }
}

/// The pills that show the battery gauge with the charging animation (when the setting is on).
pub fn animates(kind: PillKind, enabled: bool) -> bool {
    enabled && matches!(kind, PillKind::Charging | PillKind::Full | PillKind::Plugged)
}

pub fn show_secs(kind: PillKind, animated: bool) -> f32 {
    match kind {
        PillKind::Charging | PillKind::Full | PillKind::Plugged if animated => CHARGING_SECS,
        PillKind::Charging | PillKind::Full | PillKind::Plugged => 4.0,
        PillKind::Unplugged => 2.5,
        PillKind::Low => HEADS_UP_SECS,
    }
}

/// Where the level bar ends, as a fraction of the track (0-1).
pub fn fill_target(percent: u8) -> f32 {
    f32::from(percent.min(100)) / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::system::{Battery, BatteryEvent, LowWarned, battery_events};

    fn b(percent: u8, plugged: bool, charging: bool) -> Battery {
        Battery { percent, plugged, charging }
    }

    #[test]
    fn fill_target_is_the_level() {
        assert_eq!(fill_target(48), 0.48);
        assert_eq!(fill_target(100), 1.0);
        assert_eq!(fill_target(0), 0.0);
        assert_eq!(fill_target(250), 1.0, "a bad reading never overshoots the track");
    }

    #[test]
    fn plugging_in_picks_the_charging_pill() {
        assert_eq!(pill_kind(BatteryEvent::Plugged, b(48, true, true)), PillKind::Charging);
        assert_eq!(pill_kind(BatteryEvent::Plugged, b(100, true, false)), PillKind::Full);
        // On AC but not charging (charge limit, battery saver): connected, no "Mengisi".
        assert_eq!(pill_kind(BatteryEvent::Plugged, b(80, true, false)), PillKind::Plugged);
        assert_eq!(pill_kind(BatteryEvent::Unplugged, b(80, false, false)), PillKind::Unplugged);
        assert_eq!(pill_kind(BatteryEvent::Low(20), b(19, false, false)), PillKind::Low);
    }

    #[test]
    fn only_the_plugged_pills_animate_and_only_when_enabled() {
        for k in [PillKind::Charging, PillKind::Full, PillKind::Plugged] {
            assert!(animates(k, true), "{k:?}");
            assert!(!animates(k, false), "{k:?} with the setting off is the plain pill");
        }
        for k in [PillKind::Unplugged, PillKind::Low] {
            assert!(!animates(k, true), "{k:?} is plain");
        }
    }

    #[test]
    fn durations_the_charging_pill_is_2_5_s_and_unplugging_is_short() {
        assert_eq!(show_secs(PillKind::Charging, true), 2.5);
        assert_eq!(show_secs(PillKind::Full, true), 2.5);
        // Without the animation it is the old plain pill.
        assert_eq!(show_secs(PillKind::Charging, false), 4.0);
        assert!(show_secs(PillKind::Unplugged, true) <= 3.0);
        assert!(show_secs(PillKind::Low, true) > show_secs(PillKind::Unplugged, true), "a low warning stays longer");
    }

    #[test]
    fn plug_unplug_sequence_walks_through_the_states() {
        let mut warned = LowWarned::default();
        let mut prev = Some(b(50, false, false));
        let mut seen = vec![];
        for cur in [b(50, true, true), b(51, true, true), b(100, true, false), b(100, false, false), b(19, false, false), b(19, true, true)] {
            for e in battery_events(prev, cur, &mut warned) {
                seen.push(pill_kind(e, cur));
            }
            prev = Some(cur);
        }
        // plug (charging), a % change is silent, full is silent, unplug, low, plug in again.
        assert_eq!(seen, vec![PillKind::Charging, PillKind::Unplugged, PillKind::Low, PillKind::Charging]);
    }
}
