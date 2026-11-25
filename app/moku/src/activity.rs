//! What the pill shows. Features publish *activities*; the highest priority wins (ties → most
//! recently added); transient ones (alerts, OSD) expire on their own and the pill falls back to
//! whatever is underneath. The highest *persistent* activity other than the winner is shown as
//! the small split bubble next to the pill.
//!
//! Priorities (see the plan): [`priority`].

use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::app::App;
use crate::ui::Node;

/// Priority table from the plan (higher wins).
pub mod priority {
    pub const AI_CARD: u8 = 100;
    /// Timer finished / reminder due.
    pub const DUE: u8 = 90;
    pub const NOTIFICATION: u8 = 80;
    /// Volume / brightness / Caps Lock OSD — transient, [`super::OSD_SECS`].
    pub const OSD: u8 = 70;
    /// Internet lost / battery low.
    pub const WARNING: u8 = 65;
    /// Meeting mic / camera.
    pub const MEETING: u8 = 60;
    /// Recording-like progress from IPC.
    pub const PROGRESS: u8 = 55;
    /// Timer / focus running.
    pub const TIMER: u8 = 50;
    pub const MUSIC: u8 = 40;
    /// Weather / prayer heads-up — transient, [`super::HEADS_UP_SECS`].
    pub const HEADS_UP: u8 = 30;
    pub const IDLE: u8 = 0;
}

/// Standard transient durations.
pub const OSD_SECS: f32 = 1.5;
pub const HEADS_UP_SECS: f32 = 6.0;
pub const NOTIFICATION_SECS: f32 = 5.0;

/// Builds a pill (or bubble) tree from the app state. Called on the UI thread whenever the
/// pill may need redrawing; must be cheap and side-effect free.
pub type Build = Rc<dyn Fn(&App) -> Node>;

/// Something the pill can show.
#[derive(Clone)]
pub struct Activity {
    /// Unique id, by convention `"<feature>.<what>"` (e.g. `"media.music"`). Setting an
    /// activity with an existing id replaces it in place.
    pub id: String,
    pub priority: u8,
    /// `None` = persistent until cleared; `Some(d)` = expires `d` after it was (re)set.
    pub duration: Option<Duration>,
    /// Tab opened when the pill (or its bubble) is clicked outside any action widget.
    pub tab: Option<&'static str>,
    /// Pill content. Use [`crate::ui::pill`] for the standard frame.
    pub pill: Build,
    /// Compact content (≈ 36 × 36) for the split bubble; `None` = a generic dot.
    pub bubble: Option<Build>,
}

impl Activity {
    pub fn persistent(id: impl Into<String>, priority: u8, pill: impl Fn(&App) -> Node + 'static) -> Activity {
        Activity { id: id.into(), priority, duration: None, tab: None, pill: Rc::new(pill), bubble: None }
    }

    /// `secs` is clamped to 0.1 s – 24 h (NaN / negative / infinite values from IPC included).
    pub fn transient(id: impl Into<String>, priority: u8, secs: f32, pill: impl Fn(&App) -> Node + 'static) -> Activity {
        Activity { id: id.into(), priority, duration: Some(transient_duration(secs)), tab: None, pill: Rc::new(pill), bubble: None }
    }

    pub fn tab(mut self, tab: &'static str) -> Activity {
        self.tab = Some(tab);
        self
    }

    pub fn bubble(mut self, f: impl Fn(&App) -> Node + 'static) -> Activity {
        self.bubble = Some(Rc::new(f));
        self
    }

    pub fn is_persistent(&self) -> bool {
        self.duration.is_none()
    }
}

/// Never panics, whatever a pipe client sends.
pub fn transient_duration(secs: f32) -> Duration {
    let s = if secs.is_finite() { secs.clamp(0.1, 86_400.0) } else { NOTIFICATION_SECS };
    Duration::from_millis((s * 1000.0).round() as u64)
}

/// Pure priority logic (no UI), generic so it can be unit-tested.
pub struct Entry<T> {
    pub id: String,
    pub priority: u8,
    pub expires: Option<Instant>,
    seq: u64,
    /// [`Stack::clock`] when this entry last became *news*: when it was added, or when a
    /// transient one was set again (a new notification in the same slot). Updating a persistent
    /// entry in place (music track change, timer tick, progress) is not news.
    pub changed: u64,
    pub data: T,
}

pub struct Stack<T> {
    entries: Vec<Entry<T>>,
    seq: u64,
    clock: u64,
}

impl<T> Default for Stack<T> {
    fn default() -> Self {
        Stack { entries: vec![], seq: 0, clock: 0 }
    }
}

impl<T> Stack<T> {
    /// Insert or replace by id. A replaced entry keeps its age (tie-break order) but gets the
    /// new priority, expiry and data.
    pub fn set(&mut self, id: &str, priority: u8, expires: Option<Instant>, data: T) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.id == id) {
            e.priority = priority;
            if expires.is_some() {
                self.clock += 1;
                e.changed = self.clock;
            }
            e.expires = expires;
            e.data = data;
            return;
        }
        self.seq += 1;
        self.clock += 1;
        self.entries.push(Entry { id: id.to_string(), priority, expires, seq: self.seq, changed: self.clock, data });
    }

    /// Monotonic counter of "news" (see [`Entry::changed`]): remember it at some moment, and
    /// `entry.changed > remembered` tells whether the entry is newer than that moment.
    pub fn clock(&self) -> u64 {
        self.clock
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let n = self.entries.len();
        self.entries.retain(|e| e.id != id);
        n != self.entries.len()
    }

    pub fn get(&self, id: &str) -> Option<&Entry<T>> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Drop expired entries. Returns true if anything was removed.
    pub fn expire(&mut self, now: Instant) -> bool {
        let n = self.entries.len();
        self.entries.retain(|e| e.expires.is_none_or(|t| t > now));
        n != self.entries.len()
    }

    pub fn next_expiry(&self) -> Option<Instant> {
        self.entries.iter().filter_map(|e| e.expires).min()
    }

    fn rank(e: &Entry<T>) -> (u8, u64) {
        (e.priority, e.seq)
    }

    /// The winner: highest priority, ties → most recently added.
    pub fn top(&self) -> Option<&Entry<T>> {
        self.entries.iter().max_by_key(|e| Self::rank(e))
    }

    /// The split bubble: the best *persistent* entry that is not the winner.
    pub fn split(&self) -> Option<&Entry<T>> {
        let top = self.top()?;
        self.entries.iter().filter(|e| e.id != top.id && e.expires.is_none()).max_by_key(|e| Self::rank(e))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entry<T>> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_highest_priority_wins() {
        let mut s: Stack<()> = Stack::default();
        s.set("idle", priority::IDLE, None, ());
        s.set("music", priority::MUSIC, None, ());
        s.set("timer", priority::TIMER, None, ());
        assert_eq!(s.top().unwrap().id, "timer");
        // Tie → most recent.
        s.set("timer2", priority::TIMER, None, ());
        assert_eq!(s.top().unwrap().id, "timer2");
        // Replacing keeps age: updating "timer" does not make it newer than "timer2".
        s.set("timer", priority::TIMER, None, ());
        assert_eq!(s.top().unwrap().id, "timer2");
        assert!(s.remove("timer2"));
        assert_eq!(s.top().unwrap().id, "timer");
        assert!(!s.remove("nope"));
    }

    #[test]
    fn transient_alert_expires_and_restores() {
        let t0 = Instant::now();
        let mut s: Stack<()> = Stack::default();
        s.set("idle", priority::IDLE, None, ());
        s.set("music", priority::MUSIC, None, ());
        s.set("osd", priority::OSD, Some(t0 + Duration::from_millis(1500)), ());
        assert_eq!(s.top().unwrap().id, "osd");
        assert_eq!(s.next_expiry(), Some(t0 + Duration::from_millis(1500)));
        assert!(!s.expire(t0 + Duration::from_millis(1000)));
        assert_eq!(s.top().unwrap().id, "osd");
        // Re-alert extends.
        s.set("osd", priority::OSD, Some(t0 + Duration::from_millis(2500)), ());
        assert!(!s.expire(t0 + Duration::from_millis(2000)));
        assert!(s.expire(t0 + Duration::from_millis(2600)));
        assert_eq!(s.top().unwrap().id, "music");
        assert_eq!(s.next_expiry(), None);
    }

    #[test]
    fn news_clock_counts_new_entries_and_repeated_transients_only() {
        let t0 = Instant::now();
        let mut s: Stack<()> = Stack::default();
        s.set("music", priority::MUSIC, None, ());
        let mark = s.clock();
        // A persistent update in place (track change, timer tick) is not news...
        s.set("music", priority::MUSIC, None, ());
        assert_eq!(s.get("music").unwrap().changed, mark);
        assert_eq!(s.clock(), mark);
        // ...a new entry is, and so is a transient set again (another notification).
        s.set("notif", priority::NOTIFICATION, Some(t0 + Duration::from_secs(5)), ());
        let first = s.get("notif").unwrap().changed;
        assert!(first > mark);
        s.set("notif", priority::NOTIFICATION, Some(t0 + Duration::from_secs(5)), ());
        assert!(s.get("notif").unwrap().changed > first);
    }

    #[test]
    fn transient_duration_never_panics() {
        assert_eq!(transient_duration(-1.0), Duration::from_millis(100));
        assert_eq!(transient_duration(f32::NAN), Duration::from_secs(5));
        assert_eq!(transient_duration(f32::INFINITY), Duration::from_secs(5));
        assert_eq!(transient_duration(1e30), Duration::from_secs(86_400));
        assert_eq!(transient_duration(1.5), Duration::from_millis(1500));
    }

    #[test]
    fn split_shows_second_persistent() {
        let t0 = Instant::now();
        let mut s: Stack<()> = Stack::default();
        s.set("idle", priority::IDLE, None, ());
        s.set("music", priority::MUSIC, None, ());
        // Only music + idle: the bubble is idle? No: idle (0) is persistent but meaningless —
        // the core never registers idle in the stack, so with one activity there is no split.
        s.remove("idle");
        assert!(s.split().is_none());
        s.set("timer", priority::TIMER, None, ());
        assert_eq!(s.top().unwrap().id, "timer");
        assert_eq!(s.split().unwrap().id, "music");
        // A transient on top keeps the best persistent below it in the bubble...
        s.set("notif", priority::NOTIFICATION, Some(t0 + Duration::from_secs(5)), ());
        assert_eq!(s.top().unwrap().id, "notif");
        assert_eq!(s.split().unwrap().id, "timer");
        // ...and transients never become the bubble.
        s.set("osd", priority::OSD, Some(t0 + Duration::from_secs(1)), ());
        assert_eq!(s.split().unwrap().id, "timer");
    }
}
