//! Which to-do's reminder should be showing, and when to look again. Pure (the clock is an
//! argument), so the schedule is testable without an island.
//!
//! A to-do with a due time raises a reminder pill (priority 85) when that time comes: with
//! "Selesai" and "Tunda 10 m". The pill stays until the user acts on it or [`SHOW_SECS`] pass
//! (then it counts as dismissed). A reminder shows once per due time: `fired` is saved with the
//! to-do, so restarting the island does not raise it again, and "Tunda" re-arms it.

use super::Todo;

/// A reminder that is older than this when the island starts (or wakes) is not raised any more:
/// the to-do just shows as overdue in the list.
pub const MAX_OVERDUE_SECS: i64 = 12 * 3600;
/// The pill goes away by itself after this long.
pub const SHOW_SECS: i64 = 15 * 60;
/// "Tunda 10 m".
pub const SNOOZE_MIN: i64 = 10;
/// Longest sleep between two looks at the list (guards against clock jumps and sleep).
pub const MAX_WAIT_SECS: i64 = 600;

/// Is this to-do waiting for its reminder to be shown?
fn pending(t: &Todo, now: i64) -> Option<i64> {
    let due = t.due?;
    (!t.done && !t.fired && due <= now && now - due <= MAX_OVERDUE_SECS).then_some(due)
}

/// The to-do whose reminder should be showing: the one that has been due the longest.
pub fn pick(todos: &[Todo], now: i64) -> Option<u64> {
    todos.iter().filter_map(|t| pending(t, now).map(|d| (d, t.id))).min().map(|(_, id)| id)
}

/// Seconds until the next to-do becomes due (`None` = none upcoming).
pub fn next_in(todos: &[Todo], now: i64) -> Option<i64> {
    todos.iter().filter(|t| !t.done && !t.fired).filter_map(|t| t.due).filter(|d| *d > now).map(|d| d - now).min()
}

/// Is a to-do overdue (past its time, not done)? For the red time in the list.
pub fn overdue(t: &Todo, now: i64) -> bool {
    !t.done && t.due.is_some_and(|d| d < now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn todo(id: u64, due: Option<i64>) -> Todo {
        Todo { id, text: format!("t{id}"), done: false, due, fired: false }
    }

    #[test]
    fn the_longest_waiting_reminder_comes_first() {
        let l = vec![todo(1, Some(900)), todo(2, Some(500)), todo(3, None), todo(4, Some(5000))];
        assert_eq!(pick(&l, 400), None, "nothing due yet");
        assert_eq!(pick(&l, 500), Some(2), "due exactly now");
        assert_eq!(pick(&l, 1000), Some(2));
        let mut l = l;
        l[1].fired = true;
        assert_eq!(pick(&l, 1000), Some(1), "a fired reminder is not raised again");
        l[0].done = true;
        assert_eq!(pick(&l, 1000), None, "a done to-do is not reminded");
        assert_eq!(pick(&[], 1000), None);
    }

    #[test]
    fn old_reminders_are_not_raised_after_a_long_absence() {
        let l = vec![todo(1, Some(1000))];
        assert_eq!(pick(&l, 1000 + MAX_OVERDUE_SECS), Some(1));
        assert_eq!(pick(&l, 1000 + MAX_OVERDUE_SECS + 1), None, "the island was off for more than 12 hours");
        assert!(overdue(&l[0], 1000 + MAX_OVERDUE_SECS + 1), "but it is overdue in the list");
    }

    #[test]
    fn the_next_wake_up() {
        let mut l = vec![todo(1, Some(1300)), todo(2, Some(1100)), todo(3, None), todo(4, Some(100))];
        assert_eq!(next_in(&l, 1000), Some(100));
        assert_eq!(next_in(&l, 1100), Some(200), "one that is due now is not 'next'");
        assert_eq!(next_in(&l, 2000), None);
        l[1].done = true;
        assert_eq!(next_in(&l, 1000), Some(300), "done to-dos do not wake the island");
        l[0].fired = true;
        assert_eq!(next_in(&l, 1000), None);
    }

    #[test]
    fn overdue_means_past_and_not_done() {
        let mut t = todo(1, Some(100));
        assert!(!overdue(&t, 100) && overdue(&t, 101));
        t.done = true;
        assert!(!overdue(&t, 500));
        assert!(!overdue(&todo(2, None), 500));
    }
}
