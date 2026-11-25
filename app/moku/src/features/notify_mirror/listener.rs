//! The only code that touches `Windows.UI.Notifications.Management.UserNotificationListener`.
//!
//! Spike result (full text in `app/docs/notifications.md`): from this unpackaged exe
//! `GetAccessStatus` answers (`Allowed` on the development machine, with no consent entry for the
//! exe at all), but Microsoft documents the API with a manifest capability, so whether Windows
//! really delivers notifications to a process without package identity is decided at run time:
//!
//! * `GetAccessStatus` is the ONLY call that decides whether to try. **`RequestAccessAsync` (the
//!   consent prompt) is never called, anywhere in Moku**: when Windows has not allowed the app,
//!   [`Status::NeedsAccess`] tells the user where to switch it on themselves;
//! * when the API itself fails and the process has no package identity, the status is
//!   [`Status::NeedsPackage`] ("butuh versi Store/MSIX", `docs/notifications.md` for the sparse
//!   package);
//! * only `NotificationChanged` events with `Added` are handled (the notifications already on
//!   screen when the service starts are never read), and a notification of an app that is not on
//!   the allowlist is dropped after reading its app name: its title and text are never read.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::thread::Thread;
use std::time::{Duration, Instant};

use windows::Foundation::{Size, TypedEventHandler};
use windows::Storage::Streams::{DataReader, IInputStream};
use windows::UI::Notifications::Management::{UserNotificationListener, UserNotificationListenerAccessStatus};
use windows::UI::Notifications::{KnownNotificationBindings, UserNotificationChangedEventArgs, UserNotificationChangedKind};
use windows::Win32::Foundation::ERROR_TIMEOUT;
use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::core::Interface;

use super::model::{Raw, Status, allowed};
use crate::app::Ui;
use crate::services::ServiceCtx;
use crate::ui::Image;

/// What [`access_status`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// The user allowed this app in Windows Settings → Privacy & security → Notifications.
    Allowed,
    /// Denied (by the user, or because the process has no package identity / capability).
    Denied,
    /// Never asked.
    Unspecified,
    /// The API itself failed (no package identity: the activation or the call returns an error).
    /// The HRESULT is kept for diagnostics, never shown raw to the user.
    Failed(i32),
}

/// `GetAccessStatus` only. Never shows a prompt. Call from a thread that is in an apartment
/// (the island's service threads are MTA).
pub fn access_status() -> Access {
    let listener = match UserNotificationListener::Current() {
        Ok(l) => l,
        Err(e) => return Access::Failed(e.code().0),
    };
    match listener.GetAccessStatus() {
        Ok(UserNotificationListenerAccessStatus::Allowed) => Access::Allowed,
        Ok(UserNotificationListenerAccessStatus::Denied) => Access::Denied,
        Ok(_) => Access::Unspecified,
        Err(e) => Access::Failed(e.code().0),
    }
}

/// Does this process have a package identity (MSIX, or a sparse package registered for it)?
pub fn has_package_identity() -> bool {
    // APPMODEL_ERROR_NO_PACKAGE (15700) = a plain unpackaged exe; anything else (success, or
    // "buffer too small" for a packaged process) means there is an identity.
    const NO_PACKAGE: u32 = 15700;
    let mut len = 0u32;
    let rc = unsafe { GetCurrentPackageFullName(&mut len, None) };
    rc.0 != NO_PACKAGE
}

/// What a status means for the user.
pub fn status_for(access: Access, identity: bool) -> Status {
    match access {
        Access::Allowed => Status::Listening,
        Access::Denied | Access::Unspecified => Status::NeedsAccess,
        // The API failed: without an identity that is the documented requirement, with one it is
        // an access problem we can only describe as "not allowed".
        Access::Failed(_) if !identity => Status::NeedsPackage,
        Access::Failed(_) => Status::NeedsAccess,
    }
}

// ---- the queue between the WinRT handler and the service thread ---------------------------------

/// Notification ids waiting to be read. The event handler (a thread-pool thread) only pushes
/// and wakes the service thread; reading and waiting happen on the service thread.
struct Queue {
    ids: Mutex<VecDeque<u32>>,
    thread: Thread,
}

/// At most this many ids wait; a flood drops the oldest.
const QUEUE_MAX: usize = 64;

impl Queue {
    fn push(&self, id: u32) {
        if let Ok(mut q) = self.ids.lock() {
            q.push_back(id);
            while q.len() > QUEUE_MAX {
                q.pop_front();
            }
        }
        self.thread.unpark();
    }

    fn pop(&self) -> Option<u32> {
        self.ids.lock().ok()?.pop_front()
    }
}

/// The subscription to `NotificationChanged`; dropping it removes the handler.
struct Subscription {
    listener: UserNotificationListener,
    token: i64,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let _ = self.listener.RemoveNotificationChanged(self.token);
    }
}

fn subscribe(queue: &Arc<Queue>) -> windows::core::Result<Subscription> {
    let listener = UserNotificationListener::Current()?;
    let q = queue.clone();
    let token = listener.NotificationChanged(&TypedEventHandler::<UserNotificationListener, UserNotificationChangedEventArgs>::new(move |_, args| {
        // Only the id of a notification that was added; nothing is read here.
        if let Some(a) = args.as_ref()
            && a.ChangeKind().is_ok_and(|k| k == UserNotificationChangedKind::Added)
            && let Ok(id) = a.UserNotificationId()
        {
            q.push(id);
        }
        Ok(())
    }))?;
    Ok(Subscription { listener, token })
}

// ---- reading one notification -----------------------------------------------------------------

/// Longest wait for one WinRT async call (see `media/session.rs`: this thread must stay stoppable).
const OP_TIMEOUT: Duration = Duration::from_secs(5);
/// Biggest app logo stream read (a notification icon is a few KB).
const LOGO_MAX_BYTES: u32 = 512 * 1024;
/// App logos kept (decoded, 48 px).
const LOGOS_MAX: usize = 24;

/// Wait for a WinRT async operation on this MTA thread, at most [`OP_TIMEOUT`].
macro_rules! wait_op {
    ($op:expr) => {{
        let op = $op;
        let start = Instant::now();
        let mut nap = 1u64;
        loop {
            // AsyncStatus: 0 = Started, 1 = Completed, 2 = Canceled, 3 = Error.
            if op.Status()?.0 != 0 {
                break op.GetResults();
            }
            if start.elapsed() > OP_TIMEOUT {
                let _ = op.Cancel();
                break Err(windows::core::Error::from(ERROR_TIMEOUT.to_hresult()));
            }
            std::thread::sleep(Duration::from_millis(nap));
            nap = (nap * 2).min(16);
        }
    }};
}

/// App name, AppUserModelId and logo source of notification `id`; the logo is decoded only for
/// an app that is allowed and not seen before.
fn app_of(listener: &UserNotificationListener, id: u32) -> windows::core::Result<(windows::UI::Notifications::UserNotification, String, String)> {
    let un = listener.GetNotification(id)?;
    let info = un.AppInfo()?;
    let app = info.DisplayInfo()?.DisplayName()?.to_string();
    let aumid = info.AppUserModelId().map(|s| s.to_string()).unwrap_or_default();
    Ok((un, app, aumid))
}

/// The title (first text element) and body (the rest, joined by a space) of the toast.
fn texts_of(un: &windows::UI::Notifications::UserNotification) -> windows::core::Result<(String, String)> {
    let binding = un.Notification()?.Visual()?.GetBinding(&KnownNotificationBindings::ToastGeneric()?)?;
    let elements = binding.GetTextElements()?;
    let mut title = String::new();
    let mut body: Vec<String> = vec![];
    for i in 0..elements.Size()? {
        let t = elements.GetAt(i)?.Text()?.to_string();
        if i == 0 {
            title = t;
        } else if !t.trim().is_empty() {
            body.push(t);
        }
    }
    Ok((title, body.join(" ")))
}

fn read_logo(un: &windows::UI::Notifications::UserNotification) -> windows::core::Result<Image> {
    let reference = un.AppInfo()?.DisplayInfo()?.GetLogo(Size { Width: 48.0, Height: 48.0 })?;
    let st = wait_op!(reference.OpenReadAsync()?)?;
    let size = st.Size()?;
    if size == 0 || size > LOGO_MAX_BYTES as u64 {
        return Err(windows::core::Error::empty());
    }
    let dr = DataReader::CreateDataReader(&st.cast::<IInputStream>()?)?;
    let n = wait_op!(dr.LoadAsync(size as u32)?)?;
    let mut buf = vec![0u8; n as usize];
    dr.ReadBytes(&mut buf)?;
    crate::imaging::decode(&buf, 48)
}

/// Service-thread bookkeeping.
#[derive(Default)]
struct Worker {
    /// Decoded logos by AppUserModelId (`None` = this app has none we could read).
    logos: HashMap<String, Option<Image>>,
    /// Names of not-allowed apps already reported to the UI thread.
    reported: HashSet<String>,
}

impl Worker {
    /// Handle notification `id`: drop it when its app is not allowed (only the app name was
    /// read), else read its text and logo and hand it to the UI thread.
    fn handle(&mut self, ctx: &ServiceCtx, listener: &UserNotificationListener, id: u32) {
        let Ok((un, app, aumid)) = app_of(listener, id) else { return };
        let cfg = ctx.config();
        if !allowed(&cfg.notify_mirror.apps, &app, &aumid) {
            // Only the app's name goes on (once), so the tab can offer "izinkan".
            if self.reported.len() < 64 && self.reported.insert(app.to_lowercase()) {
                let name = app.clone();
                Ui::post(move |app| super::note_app(app, &name));
            }
            return;
        }
        let Ok((title, body)) = texts_of(&un) else { return };
        let logo = match self.logos.get(&aumid) {
            Some(l) => l.clone(),
            None => {
                let l = read_logo(&un).ok();
                if self.logos.len() >= LOGOS_MAX {
                    self.logos.clear();
                }
                self.logos.insert(aumid.clone(), l.clone());
                l
            }
        };
        let raw = Raw { win_id: id, app, aumid, title, body };
        Ui::post(move |app| super::ingest(app, raw, logo));
    }
}

// ---- the service ------------------------------------------------------------------------------

/// How often the access is looked at again (the user may switch it in Windows Settings).
const RECHECK: Duration = Duration::from_secs(60);

/// Service `notify-mirror` (enabled by `[notify_mirror] enabled`).
pub fn run(ctx: ServiceCtx) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    serve(&ctx);
    unsafe {
        CoUninitialize();
    }
}

fn post_status(status: Status) {
    Ui::post(move |app| super::set_status(app, status));
}

fn serve(ctx: &ServiceCtx) {
    let queue = Arc::new(Queue { ids: Mutex::new(VecDeque::new()), thread: std::thread::current() });
    let identity = has_package_identity();
    let mut worker = Worker::default();
    let mut sub: Option<Subscription> = None;
    let mut last: Option<Status> = None;
    let mut next_check = Instant::now();
    post_status(Status::Starting);
    while !ctx.should_stop() {
        if Instant::now() >= next_check {
            next_check = Instant::now() + RECHECK;
            let access = access_status();
            let mut status = status_for(access, identity);
            if status == Status::Listening {
                if sub.is_none() {
                    match subscribe(&queue) {
                        Ok(s) => sub = Some(s),
                        // Allowed on paper, but the event cannot be had: the documented
                        // requirement (a package identity) is the usual reason.
                        Err(e) => {
                            log_warn!("notify mirror: cannot subscribe ({:#010x}, identity: {identity})", e.code().0 as u32);
                            status = if identity { Status::NeedsAccess } else { Status::NeedsPackage };
                        }
                    }
                }
            } else {
                sub = None;
            }
            if last != Some(status) {
                log_info!("notify mirror: {status:?}");
                last = Some(status);
                post_status(status);
            }
        }
        if let Some(s) = &sub {
            while let Some(id) = queue.pop() {
                worker.handle(ctx, &s.listener, id);
                if ctx.should_stop() {
                    return;
                }
            }
        }
        // Woken by a new notification (the handler unparks us) or by a stop; else by the recheck.
        std::thread::park_timeout(next_check.saturating_duration_since(Instant::now()).max(Duration::from_millis(50)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};

    /// The spike. Run `cargo test -p moku spike_ -- --nocapture` and read the lines. This test
    /// process is a plain unpackaged exe, exactly like moku.exe. It only reads the access status
    /// (never `RequestAccessAsync`, never a notification).
    #[test]
    fn spike_access_status_of_an_unpackaged_process() {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        let a = access_status();
        let identity = has_package_identity();
        println!("SPIKE unpackaged UserNotificationListener.GetAccessStatus = {a:?}; package identity = {identity}");
        if let Access::Failed(h) = a {
            println!("SPIKE hresult = {:#010x}", h as u32);
        }
        // Recorded, not asserted: the answer depends on the machine (Windows version, the master
        // switch in Settings). On the dev machine (Windows 11 build 26300) it was `Allowed` with
        // no per-app consent entry at all, which still proves nothing about whether notifications
        // are delivered to a process without package identity (see docs/notifications.md).
        assert!(matches!(a, Access::Allowed | Access::Denied | Access::Unspecified | Access::Failed(_)));
        // `cargo test` runs a plain exe.
        assert!(!identity, "the test process is unpackaged");
        println!("SPIKE status the island would show = {:?}", status_for(a, identity));
    }

    #[test]
    fn status_for_each_access_result() {
        assert_eq!(status_for(Access::Allowed, false), Status::Listening);
        assert_eq!(status_for(Access::Allowed, true), Status::Listening);
        assert_eq!(status_for(Access::Denied, false), Status::NeedsAccess);
        assert_eq!(status_for(Access::Unspecified, true), Status::NeedsAccess);
        // The API fails: no identity = the documented requirement; with one, an access problem.
        assert_eq!(status_for(Access::Failed(-2147024891), false), Status::NeedsPackage);
        assert_eq!(status_for(Access::Failed(-2147024891), true), Status::NeedsAccess);
    }

    #[test]
    fn the_queue_is_bounded_and_wakes_the_service() {
        let q = Queue { ids: Mutex::new(VecDeque::new()), thread: std::thread::current() };
        for i in 0..200 {
            q.push(i);
        }
        assert_eq!(q.ids.lock().unwrap().len(), QUEUE_MAX);
        assert_eq!(q.pop(), Some(200 - QUEUE_MAX as u32), "the oldest were dropped");
        while q.pop().is_some() {}
        assert_eq!(q.pop(), None);
    }
}
