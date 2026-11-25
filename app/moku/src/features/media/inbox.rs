//! A command queue for a service thread that sleeps on Win32 events: the service waits on
//! its stop event and the inbox event at once, so it uses no CPU while idle and still stops
//! promptly. Shared by the track-A services (media, lyrics, audio, control).

use std::collections::VecDeque;
use std::sync::Mutex;

use windows::Win32::System::Threading::{INFINITE, ResetEvent, WaitForMultipleObjects};

use crate::services::{Event, ServiceCtx};

/// Commands beyond this are dropped (oldest first) while the service is not draining.
const MAX_QUEUED: usize = 64;

pub struct Inbox<T> {
    q: Mutex<VecDeque<T>>,
    ev: Event,
}

impl<T> Inbox<T> {
    pub fn new() -> Inbox<T> {
        Inbox { q: Mutex::new(VecDeque::new()), ev: Event::new() }
    }

    /// Queue a command (any thread) and wake the service.
    pub fn push(&self, t: T) {
        if let Ok(mut q) = self.q.lock() {
            if q.len() >= MAX_QUEUED {
                q.pop_front();
            }
            q.push_back(t);
        }
        self.ev.set();
    }

    /// Drop everything queued (at service start: commands sent while it was off are stale).
    pub fn clear(&self) {
        if let Ok(mut q) = self.q.lock() {
            q.clear();
        }
    }

    /// Block until commands arrive (→ `Some(batch)`) or the service is stopped (→ `None`).
    pub fn wait(&self, ctx: &ServiceCtx) -> Option<Vec<T>> {
        self.wait_timeout(ctx, None)
    }

    /// Like [`Inbox::wait`], but gives up after `timeout` with `Some(empty batch)`.
    pub fn wait_timeout(&self, ctx: &ServiceCtx, timeout: Option<std::time::Duration>) -> Option<Vec<T>> {
        let deadline = timeout.map(|t| std::time::Instant::now() + t);
        loop {
            if ctx.should_stop() {
                return None;
            }
            // Reset before draining, so a push racing with the drain re-signals the event.
            unsafe {
                let _ = ResetEvent(self.ev.handle());
            }
            let batch: Vec<T> = self.q.lock().map(|mut q| q.drain(..).collect()).unwrap_or_default();
            if !batch.is_empty() {
                return Some(batch);
            }
            let ms = match deadline {
                Some(d) => {
                    let left = d.saturating_duration_since(std::time::Instant::now());
                    if left.is_zero() {
                        return Some(batch);
                    }
                    left.as_millis().min(INFINITE as u128 - 1) as u32
                }
                None => INFINITE,
            };
            unsafe {
                WaitForMultipleObjects(&[ctx.stop_event(), self.ev.handle()], false, ms);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbox_delivers_batches_and_stops() {
        let ctx = ServiceCtx::detached("inbox-test");
        let inbox = std::sync::Arc::new(Inbox::new());
        inbox.push(1);
        inbox.push(2);
        assert_eq!(inbox.wait(&ctx), Some(vec![1, 2]));
        let (i2, c2) = (inbox.clone(), ctx.clone());
        let h = std::thread::spawn(move || i2.wait(&c2));
        std::thread::sleep(std::time::Duration::from_millis(50));
        inbox.push(3);
        assert_eq!(h.join().unwrap(), Some(vec![3]));
        let (i2, c2) = (inbox.clone(), ctx.clone());
        let h = std::thread::spawn(move || i2.wait(&c2));
        std::thread::sleep(std::time::Duration::from_millis(50));
        ctx.request_stop();
        assert_eq!(h.join().unwrap(), None);
    }

    #[test]
    fn inbox_wait_timeout_returns_empty() {
        let ctx = ServiceCtx::detached("inbox-timeout");
        let inbox: Inbox<u8> = Inbox::new();
        let t = std::time::Instant::now();
        assert_eq!(inbox.wait_timeout(&ctx, Some(std::time::Duration::from_millis(60))), Some(vec![]));
        assert!(t.elapsed() >= std::time::Duration::from_millis(55));
        inbox.push(4);
        assert_eq!(inbox.wait_timeout(&ctx, Some(std::time::Duration::from_secs(5))), Some(vec![4]));
    }

    #[test]
    fn inbox_is_bounded() {
        let ctx = ServiceCtx::detached("inbox-bound");
        let inbox = Inbox::new();
        for i in 0..100 {
            inbox.push(i);
        }
        let b = inbox.wait(&ctx).unwrap();
        assert_eq!(b.len(), MAX_QUEUED);
        assert_eq!(b[0], 100 - MAX_QUEUED as i32);
    }
}
