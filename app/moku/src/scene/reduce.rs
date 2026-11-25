//! Reduce motion, as pure functions: how Moku's own setting (`appearance.animations`: Normal /
//! Kurangi / Mati) combines with Windows' "Animation effects", what each level does to the
//! shape's springs, and what the island's "away" moves (move aside, hide until the top edge)
//! look like at each level. [`super::Island`] only applies the result.
//!
//! * **Normal** (`Full`): springs as designed; away moves fade and slide.
//! * **Kurangi** (`Reduced`): no overshoot (critically damped), settle time at most
//!   [`REDUCED_SETTLE_MS`]; content fades; nothing slides (an away move is a short fade).
//! * **Mati** (`Off`): everything is instant, nothing moves. Also what Windows' "Animation
//!   effects" off means: Moku's setting can only reduce motion further, never bring it back.

use moku_core::config::Animations;

use super::spring::motion::Motion;

/// Longest spring settle time under "Kurangi" (ms).
pub const REDUCED_SETTLE_MS: f32 = 160.0;
/// Opacity of the island when it steps aside in place (30 %).
pub const FADE_OPACITY: f32 = 0.30;
/// Opacity of a sideways-shifted island.
pub const SLIDE_OPACITY: f32 = 0.55;
/// Duration of an away move at full motion (ms).
pub const AWAY_MS: f32 = 220.0;
/// Duration of an away move under "Kurangi" (ms).
pub const AWAY_REDUCED_MS: f32 = 120.0;

/// The setting in effect: Windows "Animation effects" off (`windows_effects == false`) wins.
pub fn effective(setting: Animations, windows_effects: bool) -> Animations {
    if windows_effects { setting } else { Animations::Off }
}

/// The spring to use for a shape change at `anim`, or `None` to jump straight to the target.
pub fn spring(anim: Animations, m: Motion) -> Option<Motion> {
    match anim {
        Animations::Full => Some(m),
        // Critically damped: no overshoot, and quick.
        Animations::Reduced => Some((1.0, m.1.min(REDUCED_SETTLE_MS))),
        Animations::Off => None,
    }
}

/// Where the island is relative to its resting place.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Away {
    /// At rest, fully visible.
    Home,
    /// In place at [`FADE_OPACITY`].
    Fade,
    /// Shifted `dx` DIP (positive = right), at [`SLIDE_OPACITY`].
    Slide { dx: f32 },
    /// Invisible (hidden until the top edge).
    Hide,
}

impl Away {
    pub fn is_home(self) -> bool {
        self == Away::Home
    }

    /// Short name for `debug state`.
    pub fn name(self) -> &'static str {
        match self {
            Away::Home => "home",
            Away::Fade => "fade",
            Away::Slide { .. } => "slide",
            Away::Hide => "hide",
        }
    }
}

/// The end state of an away move and how long it takes (`ms == 0`: at once).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct AwayPlan {
    pub opacity: f32,
    /// Horizontal shift in DIP.
    pub dx: f32,
    pub ms: f32,
}

/// What an away move looks like at `anim`: only full motion slides; "Kurangi" fades quickly in
/// place, "Mati" jumps (no movement, no animation).
pub fn away_plan(away: Away, anim: Animations) -> AwayPlan {
    let (opacity, dx) = match away {
        Away::Home => (1.0, 0.0),
        Away::Fade => (FADE_OPACITY, 0.0),
        Away::Slide { dx } => (SLIDE_OPACITY, dx),
        Away::Hide => (0.0, 0.0),
    };
    match anim {
        Animations::Full => AwayPlan { opacity, dx, ms: AWAY_MS },
        Animations::Reduced => AwayPlan { opacity: if matches!(away, Away::Slide { .. }) { FADE_OPACITY } else { opacity }, dx: 0.0, ms: AWAY_REDUCED_MS },
        Animations::Off => AwayPlan { opacity: if matches!(away, Away::Slide { .. }) { FADE_OPACITY } else { opacity }, dx: 0.0, ms: 0.0 },
    }
}

#[cfg(test)]
mod tests {
    use super::super::spring::motion;
    use super::*;

    const ALL: [Motion; 5] = [motion::HOVER, motion::OPEN, motion::CLOSE, motion::PILL, motion::ALERT];

    #[test]
    fn windows_animation_effects_off_always_wins() {
        for s in [Animations::Full, Animations::Reduced, Animations::Off] {
            assert_eq!(effective(s, false), Animations::Off, "Windows off: {s:?}");
            assert_eq!(effective(s, true), s, "Windows on: Moku's own setting");
        }
    }

    #[test]
    fn normal_keeps_the_designed_springs() {
        for m in ALL {
            assert_eq!(spring(Animations::Full, m), Some(m));
        }
        // The designed springs do overshoot (that is what "Kurangi" removes).
        assert!(ALL.iter().any(|m| m.0 < 1.0));
    }

    #[test]
    fn kurangi_has_no_overshoot_and_is_shorter() {
        for m in ALL {
            let (zeta, settle) = spring(Animations::Reduced, m).expect("a spring");
            assert!(zeta >= 1.0, "critically damped or more: {m:?} -> {zeta}");
            assert!(settle <= REDUCED_SETTLE_MS, "{m:?} -> {settle}");
            assert!(settle <= m.1, "never longer than designed: {m:?} -> {settle}");
        }
        // A quick preset (hover, 120 ms) is not stretched to the cap.
        assert_eq!(spring(Animations::Reduced, motion::HOVER), Some((1.0, 120.0)));
        assert_eq!(spring(Animations::Reduced, motion::OPEN), Some((1.0, REDUCED_SETTLE_MS)));
    }

    #[test]
    fn mati_jumps() {
        for m in ALL {
            assert_eq!(spring(Animations::Off, m), None);
        }
    }

    #[test]
    fn away_moves_by_level() {
        let slide = Away::Slide { dx: 140.0 };
        // Normal: fade and slide animate.
        assert_eq!(away_plan(Away::Fade, Animations::Full), AwayPlan { opacity: 0.30, dx: 0.0, ms: AWAY_MS });
        assert_eq!(away_plan(slide, Animations::Full), AwayPlan { opacity: SLIDE_OPACITY, dx: 140.0, ms: AWAY_MS });
        assert_eq!(away_plan(Away::Home, Animations::Full), AwayPlan { opacity: 1.0, dx: 0.0, ms: AWAY_MS });
        assert_eq!(away_plan(Away::Hide, Animations::Full).opacity, 0.0);
        // Kurangi: nothing slides, a short fade.
        let p = away_plan(slide, Animations::Reduced);
        assert_eq!((p.dx, p.opacity, p.ms), (0.0, FADE_OPACITY, AWAY_REDUCED_MS));
        // Mati: no animation at all, and nothing moves.
        for a in [Away::Home, Away::Fade, slide, Away::Hide] {
            let p = away_plan(a, Animations::Off);
            assert_eq!((p.ms, p.dx), (0.0, 0.0), "{a:?}");
        }
        assert_eq!(away_plan(slide, Animations::Off).opacity, FADE_OPACITY);
        // Coming home is the same at every level: fully visible.
        for anim in [Animations::Full, Animations::Reduced, Animations::Off] {
            let p = away_plan(Away::Home, anim);
            assert_eq!((p.opacity, p.dx), (1.0, 0.0));
        }
    }
}
