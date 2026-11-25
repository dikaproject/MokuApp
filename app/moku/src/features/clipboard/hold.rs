//! "Moku is borrowing the clipboard": the history ignores what changes meanwhile.
//!
//! Tanya AI's Ctrl+C fallback saves the clipboard, makes the program in front copy the selection,
//! reads it and puts the old content back (`ai::selection::copy_fallback`); "Ganti teks" puts the
//! answer on it, pastes and restores. Moku's *own* writes carry a private marker and are never
//! recorded, but the program's copy of the selection does not, so without this the selected
//! text could land in the clipboard history (and a one-time code, or a password the user selected,
//! would get past the history's rules).
//!
//! [`Hold::new`] starts a hold and dropping it ends it. While a hold is on, and for every change up to
//! the clipboard sequence number at the moment it ended (the debounced listener sees a change a few
//! milliseconds late, and by then the clipboard is already restored), the listener
//! ([`covers`]) records nothing.
//!
//! What this does **not** cover: other clipboard watchers. Windows' own clipboard history (Win+V) and
//! cloud clipboard see the selection copy too: the fallback is only used when UI Automation gave
//! no text, the clipboard is back to what it was within a fraction of a second, and Windows' history
//! does not honour a hold. README ("Tanya AI") says so.

use std::sync::atomic::{AtomicU32, Ordering};

/// The counter and the last sequence number a hold covered.
pub struct Gate {
    holds: AtomicU32,
    released_seq: AtomicU32,
}

impl Gate {
    pub const fn new() -> Gate {
        Gate { holds: AtomicU32::new(0), released_seq: AtomicU32::new(0) }
    }

    pub fn enter(&self) {
        self.holds.fetch_add(1, Ordering::SeqCst);
    }

    /// A hold ended; the clipboard's sequence number is `seq` now (everything up to it was ours).
    pub fn leave(&self, seq: u32) {
        self.released_seq.fetch_max(seq, Ordering::SeqCst);
        let _ = self.holds.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| Some(n.saturating_sub(1)));
    }

    /// Must the change with sequence number `seq` be ignored? True while a hold is on, and for every
    /// change a hold already covered.
    pub fn covers(&self, seq: u32) -> bool {
        self.holds.load(Ordering::SeqCst) > 0 || seq <= self.released_seq.load(Ordering::SeqCst)
    }
}

static GATE: Gate = Gate::new();

/// A hold on the real clipboard's history; ends when dropped.
pub struct Hold(());

impl Hold {
    pub fn new() -> Hold {
        GATE.enter();
        Hold(())
    }
}

impl Default for Hold {
    fn default() -> Hold {
        Hold::new()
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        GATE.leave(super::sys::sequence());
    }
}

/// Is the history to ignore the change with this sequence number?
pub fn covers(seq: u32) -> bool {
    GATE.covers(seq)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_held_by_default() {
        let g = Gate::new();
        assert!(!g.covers(1));
        assert!(!g.covers(u32::MAX));
    }

    #[test]
    fn a_hold_covers_everything_until_it_ends_and_then_the_changes_it_saw() {
        let g = Gate::new();
        g.enter();
        // While the program copies the selection and Moku restores the clipboard: everything is ignored.
        assert!(g.covers(100) && g.covers(101) && g.covers(5000));
        // The hold ends when the clipboard is at sequence 102 (the restore was 102).
        g.leave(102);
        // The debounced notifications of 101 / 102 arrive afterwards: still ours.
        assert!(g.covers(101) && g.covers(102));
        // The user copies something later: recorded as usual.
        assert!(!g.covers(103) && !g.covers(200));
    }

    #[test]
    fn holds_nest_and_never_go_below_zero() {
        let g = Gate::new();
        g.enter();
        g.enter();
        g.leave(10);
        assert!(g.covers(999), "one hold is still on");
        g.leave(12);
        assert!(!g.covers(13));
        // An unbalanced leave does not wrap the counter around into "held for ever".
        g.leave(14);
        assert!(!g.covers(20));
    }

    #[test]
    fn an_older_release_never_uncovers_what_a_later_one_covered() {
        let g = Gate::new();
        g.enter();
        g.leave(500);
        g.enter();
        g.leave(300);
        assert!(g.covers(400), "the highest covered sequence number stays");
        assert!(!g.covers(501));
    }

    #[test]
    fn the_guard_holds_the_real_gate_while_it_lives() {
        // The static gate is shared by the whole test run: only look at it while a guard is alive.
        let h = Hold::new();
        assert!(covers(u32::MAX));
        drop(h);
    }
}
