//! A `RefCell<Option<T>>` for state owned by the UI thread that window procedures re-enter.
//!
//! Win32 re-enters the window procedure from inside our own calls (modal loops, synchronous
//! cross-process COM in an STA, `SendMessage`). Then the state is already borrowed. Instead of
//! dropping that work (a lost flush would freeze the island), [`Reentrant::run_or_defer`]
//! queues it and the outer borrower runs it before releasing the state.

use std::cell::RefCell;
use std::collections::VecDeque;

type Job<T> = Box<dyn FnOnce(&mut T)>;

pub struct Reentrant<T> {
    cell: RefCell<Option<T>>,
    queue: RefCell<VecDeque<Job<T>>>,
}

impl<T> Reentrant<T> {
    pub const fn new() -> Self {
        Reentrant { cell: RefCell::new(None), queue: RefCell::new(VecDeque::new()) }
    }

    pub fn set(&self, v: Option<T>) -> Option<T> {
        std::mem::replace(&mut *self.cell.borrow_mut(), v)
    }

    pub fn is_busy(&self) -> bool {
        self.cell.try_borrow_mut().is_err()
    }

    /// Run `f` now; `None` if the value is absent or already borrowed (re-entrant call).
    /// Work deferred by re-entrant calls during `f` runs before this returns.
    pub fn run<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let mut g = self.cell.try_borrow_mut().ok()?;
        let t = g.as_mut()?;
        let r = f(t);
        loop {
            let job = self.queue.borrow_mut().pop_front();
            match job {
                Some(j) => j(t),
                None => break,
            }
        }
        Some(r)
    }

    /// Run `f` now, or — if we are inside another borrow — right after that borrow's work.
    /// Returns false only if there is no value at all (app not created / already gone).
    pub fn run_or_defer(&self, f: impl FnOnce(&mut T) + 'static) -> bool {
        if self.is_busy() {
            self.queue.borrow_mut().push_back(Box::new(f));
            return true;
        }
        self.run(f).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        static S: Reentrant<Vec<u32>> = const { Reentrant::new() };
    }

    #[test]
    fn reentrant_work_is_deferred_not_dropped() {
        S.with(|s| {
            s.set(Some(vec![]));
            let r = s.run(|v| {
                v.push(1);
                // Re-entrant call while borrowed: plain `run` fails, `run_or_defer` queues.
                assert!(S.with(|s| s.run(|v| v.push(99))).is_none());
                assert!(S.with(|s| s.run_or_defer(|v| v.push(2))));
                v.push(3);
                v.len()
            });
            assert_eq!(r, Some(2));
            assert_eq!(s.run(|v| v.clone()).unwrap(), vec![1, 3, 2]);
            // Not busy → runs immediately.
            assert!(s.run_or_defer(|v| v.push(4)));
            assert_eq!(s.run(|v| v.len()), Some(4));
            s.set(None);
            assert!(!s.run_or_defer(|v| v.push(5)));
        });
    }
}
