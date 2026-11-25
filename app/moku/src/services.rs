//! Background services: plain threads that the core starts and stops with the config.
//!
//! A service never touches UI objects; it sends results to the UI thread with
//! [`ServiceCtx::post`] (= [`crate::app::Ui::post`]). Stopping sets a flag, signals
//! [`ServiceCtx::stop_event`], unparks the thread and posts `WM_QUIT` to it, so whichever way
//! the service waits (`ctx.sleep`, `WaitForMultipleObjects` on the stop event, or
//! `ctx.run_message_loop`) it returns promptly. A service must return when asked to stop.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use moku_core::Config;
use windows::Win32::Foundation::{CloseHandle, HANDLE, LPARAM, WPARAM};
use windows::Win32::System::Threading::{CreateEventW, GetCurrentThreadId, SetEvent};
use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, TranslateMessage, WM_QUIT, WM_USER};

use crate::app::{App, Ui};
use crate::registry::ServiceSpec;

/// A manual-reset Win32 event, shareable across threads.
pub struct Event(HANDLE);
unsafe impl Send for Event {}
unsafe impl Sync for Event {}

impl Event {
    pub fn new() -> Event {
        Event(unsafe { CreateEventW(None, true, false, None).unwrap_or_default() })
    }
    pub fn handle(&self) -> HANDLE {
        self.0
    }
    pub fn set(&self) {
        unsafe {
            let _ = SetEvent(self.0);
        }
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Handed to a service's `run` function on its own thread.
#[derive(Clone)]
pub struct ServiceCtx {
    pub name: &'static str,
    stop: Arc<AtomicBool>,
    event: Arc<Event>,
    tid: Arc<AtomicU32>,
}

impl ServiceCtx {
    /// A context not managed by the core (tests, ad-hoc threads). Stop it with [`request_stop`].
    ///
    /// [`request_stop`]: ServiceCtx::request_stop
    pub fn detached(name: &'static str) -> ServiceCtx {
        ServiceCtx { name, stop: Arc::new(AtomicBool::new(false)), event: Arc::new(Event::new()), tid: Arc::new(AtomicU32::new(0)) }
    }

    /// Ask the service using this context to stop (what the core does on disable / exit).
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.event.set();
        let tid = self.tid.load(Ordering::SeqCst);
        if tid != 0 {
            unsafe {
                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
    }

    pub fn should_stop(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Sleep up to `d`; returns `false` (early) if the service is being stopped.
    pub fn sleep(&self, d: Duration) -> bool {
        let end = Instant::now() + d;
        while !self.should_stop() {
            let now = Instant::now();
            if now >= end {
                return true;
            }
            std::thread::park_timeout(end - now);
        }
        false
    }

    /// Manual-reset event signalled on stop, for `WaitForMultipleObjects` loops.
    pub fn stop_event(&self) -> HANDLE {
        self.event.handle()
    }

    /// Run `f` on the UI thread with the app (same as `Ui::post`).
    pub fn post(&self, f: impl FnOnce(&mut App) + Send + 'static) {
        Ui::post(f);
    }

    /// The current config (a snapshot; services that care about changes can re-read it or
    /// be restarted by their feature's `on_config`).
    pub fn config(&self) -> Arc<Config> {
        crate::app::current_config()
    }

    /// Pump this thread's Win32 messages (for message-only windows, COM/WinRT callbacks on an
    /// STA, `RegisterDeviceNotification`, ...) until the service is stopped.
    pub fn run_message_loop(&self) {
        unsafe {
            let mut msg = MSG::default();
            // Make sure the queue exists before checking the flag, so a stop that arrives in
            // between is not lost (PostThreadMessage needs a queue).
            let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
            if self.should_stop() {
                return;
            }
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

struct Running {
    ctx: ServiceCtx,
    thread: std::thread::Thread,
    handle: std::thread::JoinHandle<()>,
}

impl Running {
    fn stop(&self) {
        self.ctx.request_stop();
        self.thread.unpark();
    }
}

/// The set of running services (UI thread).
#[derive(Default)]
pub struct Services {
    running: HashMap<&'static str, Running>,
    /// Threads asked to stop that may still be finishing; a restart of the same service waits
    /// for its old thread (on the new thread, so the UI never blocks) before running again.
    stopping: HashMap<&'static str, std::thread::JoinHandle<()>>,
}

/// How long a restarted service waits for its previous thread to return.
const RESTART_WAIT: Duration = Duration::from_secs(2);

/// Wait (bounded) for a previous thread of the same service. Returns whether it finished.
fn wait_for(old: Option<std::thread::JoinHandle<()>>, budget: Duration) -> bool {
    let Some(h) = old else { return true };
    let end = Instant::now() + budget;
    while !h.is_finished() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    if h.is_finished() {
        let _ = h.join();
        true
    } else {
        false
    }
}

impl Services {
    /// Start services whose `enabled(cfg)` is true and that are not running; stop the others.
    pub fn sync(&mut self, specs: &[ServiceSpec], cfg: &Config) {
        for s in specs {
            let want = (s.enabled)(cfg);
            let has = self.running.contains_key(s.name);
            if want && !has {
                self.start(s);
            } else if !want && has {
                if let Some(r) = self.running.remove(s.name) {
                    log_info!("service {} stopping", s.name);
                    self.retire(r);
                }
            }
        }
    }

    /// Stop and start one service again (e.g. after its settings changed).
    pub fn restart(&mut self, specs: &[ServiceSpec], name: &str, cfg: &Config) {
        if let Some(r) = self.running.remove(name) {
            self.retire(r);
        }
        self.sync(specs, cfg);
    }

    /// Ask a running service to stop and remember its thread until it has returned.
    fn retire(&mut self, r: Running) {
        r.stop();
        self.stopping.retain(|_, h| !h.is_finished());
        self.stopping.insert(r.ctx.name, r.handle);

    }

    fn start(&mut self, s: &ServiceSpec) {
        let ctx = ServiceCtx { name: s.name, stop: Arc::new(AtomicBool::new(false)), event: Arc::new(Event::new()), tid: Arc::new(AtomicU32::new(0)) };
        let c2 = ctx.clone();
        let run = s.run;
        let old = self.stopping.remove(s.name);
        let spawned = std::thread::Builder::new().name(format!("svc-{}", s.name)).spawn(move || {
            c2.tid.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
            // Never run two instances of one service at once (a restart right after a stop).
            if !wait_for(old, RESTART_WAIT) {
                log_warn!("service {}: previous thread still running; starting anyway", c2.name);
            }
            if c2.should_stop() {
                return;
            }
            run(c2);
        });
        match spawned {
            Ok(h) => {
                log_info!("service {} started", s.name);
                self.running.insert(s.name, Running { ctx, thread: h.thread().clone(), handle: h });
            }
            Err(e) => log_error!("service {} failed to start: {e}", s.name),
        }
    }

    pub fn stop_all(&mut self) {
        for (_, r) in self.running.drain() {
            r.stop();
        }
    }

    /// Stop everything and wait up to `budget` in total for the threads to finish (exit path,
    /// so services can complete a file write). Threads still running after that are abandoned.
    pub fn stop_all_and_join(&mut self, budget: Duration) {
        let all: Vec<Running> = self.running.drain().map(|(_, r)| r).collect();
        for r in &all {
            r.stop();
        }
        let end = Instant::now() + budget;
        let handles = all.into_iter().map(|r| (r.ctx.name, r.handle)).chain(self.stopping.drain());
        for (name, h) in handles {
            if !wait_for(Some(h), end.saturating_duration_since(Instant::now())) {
                log_warn!("service {name} did not stop in time");
            }
        }
    }

    pub fn names(&self) -> Vec<&'static str> {
        let mut v: Vec<_> = self.running.keys().copied().collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    static LIVE: AtomicUsize = AtomicUsize::new(0);
    static MAX: AtomicUsize = AtomicUsize::new(0);
    static STARTS: AtomicUsize = AtomicUsize::new(0);

    /// Slow to stop: keeps running 80 ms after it was asked to.
    fn slow(ctx: ServiceCtx) {
        let n = LIVE.fetch_add(1, Ordering::SeqCst) + 1;
        MAX.fetch_max(n, Ordering::SeqCst);
        STARTS.fetch_add(1, Ordering::SeqCst);
        while ctx.sleep(Duration::from_millis(5)) {}
        std::thread::sleep(Duration::from_millis(80));
        LIVE.fetch_sub(1, Ordering::SeqCst);
    }

    #[test]
    fn restart_never_runs_two_instances_at_once() {
        let specs = [ServiceSpec { name: "slow-test", enabled: |_| true, run: slow }];
        let cfg = Config::default();
        let mut s = Services::default();
        s.sync(&specs, &cfg);
        std::thread::sleep(Duration::from_millis(20));
        for _ in 0..3 {
            s.restart(&specs, "slow-test", &cfg);
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(150));
        s.stop_all_and_join(Duration::from_secs(2));
        assert_eq!(MAX.load(Ordering::SeqCst), 1, "old and new thread overlapped");
        assert_eq!(LIVE.load(Ordering::SeqCst), 0);
        assert!(STARTS.load(Ordering::SeqCst) >= 2);
    }
}