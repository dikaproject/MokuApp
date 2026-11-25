//! CPU-side model of a running compositor spring, used only to know the current value and
//! velocity when a transition is interrupted (the compositor can't be asked). Same model as
//! Composition's natural-motion spring with `Period` = 1/ω (measured in the prototype).

use std::time::Instant;

#[derive(Clone, Copy)]
pub struct Spring {
    pub start: Instant,
    pub x0: f32,
    pub v0: f32,
    pub target: f32,
    pub zeta: f32,
    pub omega: f32,
}

impl Spring {
    pub fn rest(x: f32) -> Self {
        Spring { start: Instant::now(), x0: x, v0: 0.0, target: x, zeta: 1.0, omega: 1.0 }
    }

    /// (value, velocity per second) now.
    pub fn now(&self) -> (f32, f32) {
        let t = self.start.elapsed().as_secs_f32();
        let (z, w, a, v0) = (self.zeta, self.omega, self.x0 - self.target, self.v0);
        if a == 0.0 && v0 == 0.0 {
            return (self.target, 0.0);
        }
        let (d, v) = if z < 0.999 {
            let wd = w * (1.0 - z * z).sqrt();
            let b = (v0 + z * w * a) / wd;
            let (e, c, s) = ((-z * w * t).exp(), (wd * t).cos(), (wd * t).sin());
            (e * (a * c + b * s), e * ((b * wd - z * w * a) * c - (a * wd + z * w * b) * s))
        } else {
            let e = (-w * t).exp();
            (e * (a + (v0 + w * a) * t), e * (v0 - w * (v0 + w * a) * t))
        };
        (self.target + d, v)
    }
}

/// Motion presets (damping ratio, settle time ms) from the motion table in SPEC.md.
pub mod motion {
    pub type Motion = (f32, f32);
    /// Pointer enters: ease-out ~120 ms (critically damped spring).
    pub const HOVER: Motion = (1.0, 120.0);
    /// Open panel.
    pub const OPEN: Motion = (0.82, 350.0);
    /// Close panel.
    pub const CLOSE: Motion = (1.0, 280.0);
    /// Pill ↔ pill state change.
    pub const PILL: Motion = (0.75, 400.0);
    /// Notification arrives: spring with a small bounce.
    pub const ALERT: Motion = (0.7, 450.0);
}
