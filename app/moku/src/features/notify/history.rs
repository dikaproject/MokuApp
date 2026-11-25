//! Notification history (pure logic, no UI): newest first, capped, coalesced by key.

use std::collections::VecDeque;

use chrono::{DateTime, Local};

/// Longest title / body kept (characters). The pipe already caps a request at 64 KB; this keeps
/// a flood of huge notifications from holding megabytes in the history.
pub const MAX_TITLE: usize = 120;
pub const MAX_BODY: usize = 400;

/// One notification as kept in the notification centre.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// Unique per item, stable while it lives (action ids: `notify.dismiss:<seq>`).
    pub seq: u64,
    /// Coalescing key: the client's `id`, or `title:<title>` for an id-less progress.
    pub key: Option<String>,
    pub title: String,
    pub body: Option<String>,
    /// Icon name as sent (`"download"`, a glyph, ...).
    pub icon: Option<String>,
    /// 0–1 while a progress runs; `Some(1.0)` once it finished.
    pub progress: Option<f32>,
    pub time: DateTime<Local>,
    pub unread: bool,
}

impl Item {
    /// A progress that is still running.
    pub fn running(&self) -> bool {
        self.progress.is_some_and(|p| p < 1.0)
    }
}

/// What a client sent (already validated by the protocol).
#[derive(Clone, Debug, Default)]
pub struct Incoming {
    pub id: Option<String>,
    pub title: String,
    pub body: Option<String>,
    pub icon: Option<String>,
    pub progress: Option<f32>,
}

impl Incoming {
    /// Same `id` coalesces; a progress without id coalesces by title (each update of
    /// `mokuctl progress --title x` replaces the previous one).
    pub fn key(&self) -> Option<String> {
        match (&self.id, self.progress) {
            (Some(id), _) if !id.is_empty() => Some(id.clone()),
            (_, Some(_)) => Some(format!("title:{}", self.title)),
            _ => None,
        }
    }
}

#[derive(Default)]
pub struct History {
    items: VecDeque<Item>,
    next_seq: u64,
}

/// Clamp a client-sent progress to 0–1 (NaN → 0).
pub fn clean_progress(p: Option<f32>) -> Option<f32> {
    p.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 })
}

fn clip(s: &str, max: usize) -> String {
    let one_line: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let t = one_line.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    let mut out: String = t.chars().take(max - 1).collect();
    out.push('…');
    out
}

impl History {
    /// Add (or coalesce) a notification. Returns the item's `seq` and whether an existing item
    /// was replaced. The replaced item moves to the front (it is the newest event) and keeps
    /// its `seq`. The history is then capped to `cap` (oldest dropped).
    pub fn push(&mut self, n: Incoming, cap: usize, now: DateTime<Local>) -> (u64, bool) {
        let key = n.key();
        let title = clip(&n.title, MAX_TITLE);
        let body = n.body.as_deref().map(|b| clip(b, MAX_BODY)).filter(|b| !b.is_empty());
        let progress = clean_progress(n.progress);
        let existing = key.as_ref().and_then(|k| self.items.iter().position(|i| i.key.as_ref() == Some(k)));
        let (seq, replaced) = match existing.and_then(|i| self.items.remove(i)) {
            Some(old) => (old.seq, true),
            None => {
                self.next_seq += 1;
                (self.next_seq, false)
            }
        };
        self.items.push_front(Item { seq, key, title, body, icon: n.icon, progress, time: now, unread: true });
        self.set_cap(cap);
        (seq, replaced)
    }

    /// Drop the oldest items beyond `cap` (at least 1 is kept).
    pub fn set_cap(&mut self, cap: usize) {
        self.items.truncate(cap.max(1));
    }

    pub fn dismiss(&mut self, seq: u64) -> Option<Item> {
        let i = self.items.iter().position(|i| i.seq == seq)?;
        self.items.remove(i)
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn get(&self, seq: u64) -> Option<&Item> {
        self.items.iter().find(|i| i.seq == seq)
    }


    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Item> {
        self.items.iter()
    }

    pub fn unread(&self) -> usize {
        self.items.iter().filter(|i| i.unread).count()
    }

    pub fn mark_all_read(&mut self) {
        for i in &mut self.items {
            i.unread = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(id: Option<&str>, title: &str, progress: Option<f32>) -> Incoming {
        Incoming { id: id.map(Into::into), title: title.into(), body: Some("b".into()), icon: None, progress }
    }

    #[test]
    fn notify_coalesces_by_id() {
        let now = Local::now();
        let mut h = History::default();
        let (a, ra) = h.push(n(Some("ci"), "Build", None), 50, now);
        let (b, _) = h.push(n(None, "Other", None), 50, now);
        assert!(!ra);
        // Same id → replaces, keeps seq, moves to the front with the new content.
        let (a2, ra2) = h.push(Incoming { body: Some("selesai".into()), ..n(Some("ci"), "Build ok", None) }, 50, now);
        assert_eq!((a2, ra2), (a, true));
        assert_eq!(h.len(), 2);
        let first = h.iter().next().unwrap();
        assert_eq!((first.seq, first.title.as_str(), first.body.as_deref()), (a, "Build ok", Some("selesai")));
        // No id → never coalesced.
        let (c, rc) = h.push(n(None, "Other", None), 50, now);
        assert!(!rc && c != b);
        assert_eq!(h.len(), 3);
        // Progress without id coalesces by title; updates in place.
        let (p1, _) = h.push(n(None, "moku-setup.exe", Some(0.2)), 50, now);
        let (p2, rp) = h.push(n(None, "moku-setup.exe", Some(0.64)), 50, now);
        assert_eq!((p1, rp), (p2, true));
        assert_eq!(h.get(p1).unwrap().progress, Some(0.64));
        assert!(h.get(p1).unwrap().running());
        let (p3, _) = h.push(n(None, "moku-setup.exe", Some(7.0)), 50, now);
        assert_eq!(p3, p1);
        assert_eq!(h.get(p1).unwrap().progress, Some(1.0));
        assert!(!h.get(p1).unwrap().running());
        assert_eq!(h.len(), 4);
        // A progress with an id coalesces with a plain notify of the same id (finish message).
        let (d1, _) = h.push(n(Some("dl"), "Unduh", Some(0.5)), 50, now);
        let (d2, rd) = h.push(n(Some("dl"), "Unduhan selesai", None), 50, now);
        assert_eq!((d1, rd), (d2, true));
        assert_eq!(h.get(d1).unwrap().progress, None);
        // NaN progress never panics.
        assert_eq!(clean_progress(Some(f32::NAN)), Some(0.0));
    }

    #[test]
    fn notify_cap() {
        let now = Local::now();
        let mut h = History::default();
        // A flood of 1000 notifications keeps only the newest `cap`.
        for i in 0..1000 {
            h.push(n(None, &format!("n{i}"), None), 50, now);
        }
        assert_eq!(h.len(), 50);
        assert_eq!(h.iter().next().unwrap().title, "n999");
        assert_eq!(h.iter().last().unwrap().title, "n950");
        assert_eq!(h.unread(), 50);
        // A flood on one id stays one item.
        for i in 0..1000 {
            h.push(n(Some("same"), &format!("s{i}"), None), 50, now);
        }
        assert_eq!(h.len(), 50);
        assert_eq!(h.iter().filter(|i| i.key.as_deref() == Some("same")).count(), 1);
        // Lowering the cap (config reload) truncates; 0 still keeps one.
        h.set_cap(3);
        assert_eq!(h.len(), 3);
        h.set_cap(0);
        assert_eq!(h.len(), 1);
        // Huge fields are clipped; control characters flattened.
        let big = Incoming { title: "t\n".repeat(1000), body: Some("x".repeat(60_000)), ..Default::default() };
        let (s, _) = h.push(big, 50, now);
        let it = h.get(s).unwrap();
        assert_eq!(it.title.chars().count(), MAX_TITLE);
        assert!(!it.title.contains('\n'));
        assert_eq!(it.body.as_ref().unwrap().chars().count(), MAX_BODY);
        // Dismiss / clear / read.
        assert!(h.dismiss(s).is_some());
        assert!(h.dismiss(s).is_none());
        h.mark_all_read();
        assert_eq!(h.unread(), 0);
        h.clear();
        assert!(h.is_empty());
    }
}
