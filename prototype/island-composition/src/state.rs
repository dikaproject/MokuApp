/// The island's visual states.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Idle = 0,
    Hover = 1,
    Music = 2,
    Panel = 3,
}

pub const ALL: [State; 4] = [State::Idle, State::Hover, State::Music, State::Panel];

/// Panel size in DIP. Pill widths are measured from their text (see `Island::measure_sizes`).
pub const PANEL: (f32, f32) = (640.0, 276.0);
pub const PANEL_RADIUS: f32 = 38.0;

/// Corner radius as a function of height: a full pill until the shape is panel-sized.
/// (The same rule is an ExpressionAnimation in the compositor: `Min(p.H / 2, 38)`.)
pub fn radius(h: f32) -> f32 {
    (h / 2.0).min(PANEL_RADIUS)
}

impl State {
    pub fn parse(s: &str) -> Option<State> {
        Some(match s {
            "idle" => State::Idle,
            "hover" => State::Hover,
            "music" => State::Music,
            "music-panel" => State::Panel,
            _ => return None,
        })
    }
}

/// Spring for a transition, per the motion table in SPEC.md: (damping ratio, settle time ms).
/// Every transition is a spring (hover too: a critically damped 120 ms spring reads as an
/// ease-out), so an interrupted transition can always hand its velocity to the next one.
pub fn motion(from: State, to: State) -> (f32, f32) {
    match (from, to) {
        (_, State::Hover) => (1.0, 120.0),
        (_, State::Panel) => (0.82, 350.0),
        (State::Panel, _) => (1.0, 280.0),
        _ => (0.75, 400.0),
    }
}
