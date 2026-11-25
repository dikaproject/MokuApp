//! Named-pipe server on the per-user pipe `moku_core::ipc::pipe_name()` (protocol in
//! `moku_core::ipc`). Only the current user, SYSTEM and administrators may open it, and clients
//! check that this process is run by the same user before they send anything.
//!
//! One listener thread accepts connections; each connection gets its own short-lived thread
//! that reads ONE request line (≤ 64 KB, within 2 s), hands it to the UI thread
//! (`App::handle_request`) together with a [`Reply`], waits for the answer, writes it, closes.
//! The UI thread never blocks on a client: slow, silent or garbage clients only cost their own
//! connection thread, and at most [`MAX_CLIENTS`] are read at once; at most [`MAX_WAITING`]
//! threads wait for an answer (pending asks), further requests get "busy" at once.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use moku_core::ipc::{self, IpcError, MAX_LINE, READ_TIMEOUT_MS, Request, Response};
use windows::Win32::Foundation::*;
use windows::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::*;
use windows::Win32::System::Threading::{GetCurrentProcess, INFINITE, OpenProcessToken, WaitForMultipleObjects, WaitForSingleObject};
use windows::core::{HSTRING, PWSTR};

use crate::app::Ui;
use crate::services::{Event, ServiceCtx};

/// Concurrent connections served; more are answered "busy" and closed.
pub const MAX_CLIENTS: usize = 8;
/// Connection threads waiting for the island's answer at once (pending `ask`s wait minutes);
/// beyond this a request is answered "busy" right away instead of costing another thread.
pub const MAX_WAITING: usize = 64;

/// The way back to a waiting IPC client. Send exactly one response; dropping a `Reply`
/// without answering sends `Error { "no answer" }` (for `Ask`, answer `"dismissed"` instead).
/// [`Reply::is_closed`] tells a feature that keeps a `Reply` (pending `Ask`) that nobody is
/// waiting any more (client disconnected or the connection gave up), so it can drop it.
pub struct Reply(Option<Sender<Response>>, Arc<AtomicBool>);

impl Reply {
    /// True once the client went away (closed its end of the pipe) or the connection thread
    /// stopped waiting. Answering a closed `Reply` is harmless (the answer goes nowhere).
    pub fn is_closed(&self) -> bool {
        self.1.load(Ordering::SeqCst)
    }
    /// The flag behind [`Reply::is_closed`] (connection thread, tests).
    pub(crate) fn closed_flag(&self) -> Arc<AtomicBool> {
        self.1.clone()
    }
    pub fn send(mut self, r: Response) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(r);
        }
    }
    pub fn ok(self) {
        self.send(Response::Ok)
    }
    pub fn error(self, message: impl Into<String>) {
        self.send(Response::Error { message: message.into() })
    }
    /// A reply that goes nowhere (requests made internally, tests).
    pub fn detached() -> Reply {
        Reply(None, Arc::new(AtomicBool::new(true)))
    }
    /// A reply plus the receiving end (tests, internal callers).
    pub fn channel() -> (Reply, std::sync::mpsc::Receiver<Response>) {
        let (tx, rx) = channel();
        (Reply(Some(tx), Arc::new(AtomicBool::new(false))), rx)
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(Response::Error { message: "no answer".into() });
        }
    }
}

struct Pipe(HANDLE);
unsafe impl Send for Pipe {}
impl Drop for Pipe {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// SDDL granting full access to SYSTEM, Administrators and the current user only.
fn security_descriptor() -> Option<PSECURITY_DESCRIPTOR> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        let ok = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr() as _), len, &mut len);
        let _ = CloseHandle(token);
        ok.ok()?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut sid_str = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut sid_str).ok()?;
        let sid = sid_str.to_string().ok()?;
        let _ = LocalFree(Some(HLOCAL(sid_str.0 as _)));
        let sddl = HSTRING::from(format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;{sid})"));
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(&sddl, SDDL_REVISION_1, &mut sd, None).ok()?;
        Some(sd)
    }
}

/// Service entry point (registered by the core as service `"ipc"`, enabled by `ipc.enabled`).
pub fn run(ctx: ServiceCtx) {
    let (name, warning) = ipc::pipe_name_and_warning();
    if let Some(w) = warning {
        log_warn!("pipe: {w}");
    }
    run_on(ctx, &name)
}

/// Serve on another pipe name (integration tests: `\\.\pipe\moku-test-<pid>`, with
/// `ServiceCtx::detached` and `moku_core::ipc::send_to`). Requests are handed to the UI thread
/// via `Ui::post`; with no island running (tests) they are dropped and the client receives
/// `Error { "no answer" }`.
pub fn run_on(ctx: ServiceCtx, pipe_name: &str) {
    run_with(ctx, pipe_name, Arc::new(|req, reply| Ui::post(move |app| app.handle_request(req, reply))))
}

/// Where a decoded request goes: the UI thread in the island, a channel in tests.
type Dispatch = Arc<dyn Fn(Request, Reply) + Send + Sync>;

/// [`run_on`] with a custom dispatcher (tests that answer requests themselves).
fn run_with(ctx: ServiceCtx, pipe_name: &str, dispatch: Dispatch) {
    let active = Arc::new(AtomicUsize::new(0));
    let waiting = Arc::new(AtomicUsize::new(0));
    let sd = security_descriptor();
    let sa = sd.map(|sd| SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd.0, bInheritHandle: false.into() });
    let name = HSTRING::from(pipe_name);
    let mut first = true;
    while !ctx.should_stop() {
        let mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { Default::default() };
        let h = unsafe {
            CreateNamedPipeW(
                &name,
                mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                MAX_LINE as u32,
                MAX_LINE as u32,
                0,
                sa.as_ref().map(|s| s as *const _),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            log_error!("pipe: CreateNamedPipe failed: {:?} (another Moku running?)", windows::core::Error::from_thread());
            if !ctx.sleep(Duration::from_secs(5)) {
                break;
            }
            continue;
        }
        first = false;
        let pipe = Pipe(h);
        let ev = Event::new();
        let mut ov = OVERLAPPED { hEvent: ev.handle(), ..Default::default() };
        let connected = unsafe {
            match ConnectNamedPipe(pipe.0, Some(&mut ov)) {
                Ok(()) => true,
                Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => true,
                Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {
                    let r = WaitForMultipleObjects(&[ev.handle(), ctx.stop_event()], false, INFINITE);
                    if r == WAIT_OBJECT_0 {
                        let mut n = 0;
                        GetOverlappedResult(pipe.0, &ov, &mut n, false).is_ok()
                    } else {
                        let _ = CancelIoEx(pipe.0, Some(&ov));
                        let mut n = 0;
                        let _ = GetOverlappedResult(pipe.0, &ov, &mut n, true);
                        false
                    }
                }
                Err(_) => false,
            }
        };
        if !connected {
            continue;
        }
        let n = active.fetch_add(1, Ordering::SeqCst);
        let a2 = active.clone();
        let w2 = waiting.clone();
        let d2 = dispatch.clone();
        let spawned = std::thread::Builder::new().name("pipe-client".into()).spawn(move || {
            serve(pipe, n >= MAX_CLIENTS, Slot(Some(a2)), w2, d2);
        });
        if spawned.is_err() {
            active.fetch_sub(1, Ordering::SeqCst);
        }
    }
    if let Some(sd) = sd {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(sd.0)));
        }
    }
}

/// Overlapped read with a deadline. Returns bytes read (0 = closed).
fn read_some(pipe: &Pipe, buf: &mut [u8], deadline: Instant) -> Result<usize, IpcError> {
    let ev = Event::new();
    let mut ov = OVERLAPPED { hEvent: ev.handle(), ..Default::default() };
    let mut n = 0u32;
    unsafe {
        match ReadFile(pipe.0, Some(buf), None, Some(&mut ov)) {
            Ok(()) => {
                GetOverlappedResult(pipe.0, &ov, &mut n, false).map_err(|e| IpcError::Io(e.to_string()))?;
                Ok(n as usize)
            }
            Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {
                let left = deadline.saturating_duration_since(Instant::now()).as_millis() as u32;
                if WaitForSingleObject(ev.handle(), left) != WAIT_OBJECT_0 {
                    let _ = CancelIoEx(pipe.0, Some(&ov));
                    let _ = GetOverlappedResult(pipe.0, &ov, &mut n, true);
                    return Err(IpcError::Timeout);
                }
                match GetOverlappedResult(pipe.0, &ov, &mut n, false) {
                    Ok(()) => Ok(n as usize),
                    Err(e) if e.code() == ERROR_BROKEN_PIPE.to_hresult() => Ok(0),
                    Err(e) if e.code() == ERROR_MORE_DATA.to_hresult() => Ok(n as usize),
                    Err(e) => Err(IpcError::Io(e.to_string())),
                }
            }
            Err(e) if e.code() == ERROR_BROKEN_PIPE.to_hresult() => Ok(0),
            Err(e) => Err(IpcError::Io(e.to_string())),
        }
    }
}

fn write_all(pipe: &Pipe, data: &[u8]) {
    let ev = Event::new();
    let mut ov = OVERLAPPED { hEvent: ev.handle(), ..Default::default() };
    let mut n = 0u32;
    unsafe {
        match WriteFile(pipe.0, Some(data), None, Some(&mut ov)) {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {
                if WaitForSingleObject(ev.handle(), 2000) != WAIT_OBJECT_0 {
                    let _ = CancelIoEx(pipe.0, Some(&ov));
                }
                let _ = GetOverlappedResult(pipe.0, &ov, &mut n, true);
            }
            Err(_) => {}
        }
    }
}

/// Read one request line within the deadline, at most MAX_LINE bytes.
fn read_request(pipe: &Pipe) -> Result<String, IpcError> {
    let deadline = Instant::now() + Duration::from_millis(READ_TIMEOUT_MS as u64);
    let mut data: Vec<u8> = Vec::with_capacity(512);
    let mut buf = [0u8; 4096];
    loop {
        let n = read_some(pipe, &mut buf, deadline)?;
        if n == 0 {
            if data.is_empty() {
                return Err(IpcError::Eof);
            }
            break;
        }
        if let Some(i) = buf[..n].iter().position(|&b| b == b'\n') {
            data.extend_from_slice(&buf[..i]);
            if data.len() + 1 > MAX_LINE {
                return Err(IpcError::Oversized);
            }
            break;
        }
        data.extend_from_slice(&buf[..n]);
        if data.len() + 1 > MAX_LINE {
            return Err(IpcError::Oversized);
        }
    }
    String::from_utf8(data).map_err(|_| IpcError::Malformed("not UTF-8".into()))
}

/// How long the connection thread waits for the UI thread to answer.
fn answer_timeout(r: &Request) -> Duration {
    match r {
        Request::Ask { timeout_secs, .. } => Duration::from_secs(timeout_secs.unwrap_or(120).min(3600) as u64 + 30),
        Request::Debug(moku_core::ipc::DebugOp::ScreenshotReady) => Duration::from_secs(10),
        _ => Duration::from_secs(5),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::sync::mpsc::{Receiver, channel};
    use std::time::{Duration, Instant};

    use moku_core::ipc::{self, Request, Response};

    use super::Reply;
    use crate::services::ServiceCtx;

    fn start(tag: &str) -> (String, ServiceCtx, std::thread::JoinHandle<()>) {
        let name = format!(r"\\.\pipe\moku-test-{tag}-{}", std::process::id());
        let ctx = ServiceCtx::detached("ipc-test");
        let (c2, n2) = (ctx.clone(), name.clone());
        let h = std::thread::spawn(move || super::run_on(c2, &n2));
        std::thread::sleep(Duration::from_millis(150));
        (name, ctx, h)
    }

    /// Like `start`, but every decoded request is handed to the test (as the UI thread would).
    fn start_with_sink(tag: &str) -> (String, ServiceCtx, std::thread::JoinHandle<()>, Receiver<(Request, Reply)>) {
        let name = format!(r"\\.\pipe\moku-test-{tag}-{}", std::process::id());
        let ctx = ServiceCtx::detached("ipc-test");
        let (tx, rx) = channel();
        let tx = std::sync::Mutex::new(tx);
        let (c2, n2) = (ctx.clone(), name.clone());
        let h = std::thread::spawn(move || {
            super::run_with(
                c2,
                &n2,
                Arc::new(move |req, reply| {
                    if let Ok(t) = tx.lock() {
                        let _ = t.send((req, reply));
                    }
                }),
            )
        });
        std::thread::sleep(Duration::from_millis(150));
        (name, ctx, h, rx)
    }

    /// Review Focus 5: clients that connect and never write neither block other clients nor
    /// stay connected past the read timeout — even more of them than the connection cap.
    #[test]
    fn pipe_client_never_writes_times_out() {
        let (name, ctx, h) = start("silent");
        let mut silent = std::fs::OpenOptions::new().read(true).write(true).open(&name).unwrap();
        let open = |name: &str| {
            let t = Instant::now();
            loop {
                match std::fs::OpenOptions::new().read(true).write(true).open(name) {
                    Ok(f) => return Some(f),
                    Err(_) if t.elapsed() < Duration::from_millis(500) => std::thread::sleep(Duration::from_millis(5)),
                    Err(_) => return None,
                }
            }
        };
        let mut more: Vec<_> = (0..super::MAX_CLIENTS + 2).filter_map(|_| open(&name)).collect();
        assert_eq!(more.len(), super::MAX_CLIENTS + 2);
        // Another client is served meanwhile (no island in tests → "no answer"/"busy", promptly).
        let t = Instant::now();
        let r = ipc::send_to(&name, &Request::Ping, Some(Duration::from_secs(3))).unwrap();
        assert!(t.elapsed() < Duration::from_secs(1), "served while silent clients are connected");
        assert!(matches!(r, Response::Error { .. }));
        // The silent client is dropped after the 2 s read timeout.
        let t = Instant::now();
        let mut buf = [0u8; 16];
        let n = silent.read(&mut buf).unwrap_or(0);
        assert_eq!(n, 0);
        assert!(t.elapsed() < Duration::from_secs(4));
        // ... and the server still serves normally afterwards.
        more.clear();
        std::thread::sleep(Duration::from_millis(100));
        let r = ipc::send_to(&name, &Request::Ping, Some(Duration::from_secs(3))).unwrap();
        assert_eq!(r, Response::Error { message: "no answer".into() });
        ctx.request_stop();
        h.join().unwrap();
    }

    /// Review Focus 5: garbage, unknown request types, invalid UTF-8, missing fields and
    /// oversized lines get an error line, never reach a handler, and the server keeps serving.
    #[test]
    fn pipe_garbage_line_rejected() {
        let (name, ctx, h, sink) = start_with_sink("garbage");
        let junk: [&[u8]; 4] = [b"this is not json\n", b"{\"type\":\"launch_missiles\"}\n", b"\xff\xfe garbage\n", b"{\"type\":\"notify\"}\n"];
        for j in junk {
            let mut g = std::fs::OpenOptions::new().read(true).write(true).open(&name).unwrap();
            g.write_all(j).unwrap();
            let line = ipc::read_line(&mut g, ipc::MAX_LINE).unwrap();
            let resp = ipc::decode_response(&line).unwrap();
            assert!(matches!(resp, Response::Error { .. }), "{line}");
        }
        // Oversized line (no newline within 64 KB).
        let mut g = std::fs::OpenOptions::new().read(true).write(true).open(&name).unwrap();
        let _ = g.write_all(&vec![b'x'; ipc::MAX_LINE + 10]);
        let line = ipc::read_line(&mut g, ipc::MAX_LINE).unwrap();
        assert!(line.contains("longer than"), "{line}");
        assert!(sink.try_recv().is_err(), "garbage never reaches a handler");
        // A valid request still works.
        let t = std::thread::spawn({
            let name = name.clone();
            move || ipc::send_to(&name, &Request::Ping, Some(Duration::from_secs(3)))
        });
        let (req, reply) = sink.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(req, Request::Ping);
        reply.ok();
        assert_eq!(t.join().unwrap().unwrap(), Response::Ok);
        ctx.request_stop();
        h.join().unwrap();
    }

    /// A pending `Ask` whose client disconnects is flagged closed (so the island drops the
    /// card); an answered `Ask` still reaches a client that waits.
    #[test]
    fn pipe_ask_client_disconnect_closes_reply() {
        let (name, ctx, h, sink) = start_with_sink("askgone");
        let ask = Request::Ask { title: "x".into(), detail: None, command: None, options: vec![], timeout_secs: Some(60), kind: None, agent: None };
        let mut c = std::fs::OpenOptions::new().read(true).write(true).open(&name).unwrap();
        c.write_all(ipc::encode(&ask).as_bytes()).unwrap();
        let (req, reply) = sink.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(req, ask);
        std::thread::sleep(Duration::from_millis(400));
        assert!(!reply.is_closed(), "client still waiting");
        drop(c);
        let t = Instant::now();
        while !reply.is_closed() && t.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(reply.is_closed(), "disconnect detected");
        reply.send(Response::Answer { choice: "approve".into() }); // harmless
        let t = std::thread::spawn({
            let (name, ask) = (name.clone(), ask.clone());
            move || ipc::send_to(&name, &ask, Some(Duration::from_secs(5)))
        });
        let (_, reply) = sink.recv_timeout(Duration::from_secs(3)).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(!reply.is_closed());
        reply.send(Response::Answer { choice: "deny".into() });
        assert_eq!(t.join().unwrap().unwrap(), Response::Answer { choice: "deny".into() });
        ctx.request_stop();
        h.join().unwrap();
    }

    #[test]
    fn waiting_threads_are_capped() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let held: Vec<_> = (0..super::MAX_WAITING).map(|_| super::Waiter::enter(&count).expect("room")).collect();
        assert!(super::Waiter::enter(&count).is_none(), "the 65th waits for nobody");
        drop(held);
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(super::Waiter::enter(&count).is_some());
    }

    /// End to end: with every waiting slot taken by pending asks, the next request is "busy" at
    /// once, and the server answers normally again when they are done.
    #[test]
    fn pipe_waiting_cap_answers_busy() {
        let (name, ctx, h, sink) = start_with_sink("waitcap");
        let ask = Request::Ask { title: "x".into(), detail: None, command: None, options: vec![], timeout_secs: Some(30), kind: None, agent: None };
        // One after the other (a burst of 64 connects would hit ERROR_PIPE_BUSY in the client).
        let mut clients = vec![];
        let mut replies = vec![];
        for _ in 0..super::MAX_WAITING {
            let (name, ask) = (name.clone(), ask.clone());
            clients.push(std::thread::spawn(move || ipc::send_to(&name, &ask, Some(Duration::from_secs(30)))));
            replies.push(sink.recv_timeout(Duration::from_secs(10)).expect("ask reached the handler").1);
        }
        let t = Instant::now();
        assert_eq!(ipc::send_to(&name, &Request::Ping, Some(Duration::from_secs(5))).unwrap(), Response::Error { message: "busy".into() });
        assert!(t.elapsed() < Duration::from_secs(3));
        for r in replies {
            r.send(Response::Answer { choice: "deny".into() });
        }
        for c in clients {
            assert_eq!(c.join().unwrap().unwrap(), Response::Answer { choice: "deny".into() });
        }
        let t2 = std::thread::spawn({
            let name = name.clone();
            move || ipc::send_to(&name, &Request::Ping, Some(Duration::from_secs(5)))
        });
        let (_, reply) = sink.recv_timeout(Duration::from_secs(5)).unwrap();
        reply.ok();
        assert_eq!(t2.join().unwrap().unwrap(), Response::Ok);
        ctx.request_stop();
        h.join().unwrap();
    }

    #[test]
    fn reply_flags() {
        assert!(Reply::detached().is_closed());
        let (r, rx) = Reply::channel();
        assert!(!r.is_closed());
        drop(r);
        assert!(matches!(rx.recv().unwrap(), Response::Error { .. }));
    }
}

/// One unit of the connection cap, released when the request has been read (so a pending
/// Ask waiting minutes for the user does not block other clients) or when dropped.
struct Slot(Option<Arc<AtomicUsize>>);
impl Slot {
    fn release(&mut self) {
        if let Some(a) = self.0.take() {
            a.fetch_sub(1, Ordering::SeqCst);
        }
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        self.release();
    }
}

/// Is the client still connected? (`PeekNamedPipe` fails with `ERROR_BROKEN_PIPE` once the
/// client closed its handle.)
fn client_connected(pipe: &Pipe) -> bool {
    let mut avail = 0u32;
    unsafe { PeekNamedPipe(pipe.0, None, 0, None, Some(&mut avail), None).is_ok() }
}

/// Wait for the UI thread's answer, checking every 250 ms that the client is still there.
/// `None` = the client disconnected (nothing to write). On disconnect or timeout the reply's
/// `closed` flag is set so a feature holding it (pending `Ask`) can drop it.
fn wait_answer(pipe: &Pipe, rx: &Receiver<Response>, wait: Duration, closed: &AtomicBool) -> Option<Response> {
    let deadline = Instant::now() + wait;
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(r) => return Some(r),
            Err(RecvTimeoutError::Disconnected) => return Some(Response::Error { message: "no answer".into() }),
            Err(RecvTimeoutError::Timeout) => {}
        }
        if !client_connected(pipe) {
            closed.store(true, Ordering::SeqCst);
            return None;
        }
        if Instant::now() >= deadline {
            closed.store(true, Ordering::SeqCst);
            return Some(Response::Error { message: "timed out waiting for the island".into() });
        }
    }
}

/// One thread waiting for an answer, counted against [MAX_WAITING] while it lives.
struct Waiter(Arc<AtomicUsize>);
impl Waiter {
    /// None = already [MAX_WAITING] threads wait.
    fn enter(count: &Arc<AtomicUsize>) -> Option<Waiter> {
        let prev = count.fetch_add(1, Ordering::SeqCst);
        if prev >= MAX_WAITING {
            count.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Waiter(count.clone()))
    }
}
impl Drop for Waiter {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn serve(pipe: Pipe, busy: bool, mut slot: Slot, waiting: Arc<AtomicUsize>, dispatch: Dispatch) {
    if busy {
        write_all(&pipe, ipc::encode(&Response::Error { message: "busy".into() }).as_bytes());
        return;
    }
    let read = read_request(&pipe);
    slot.release();
    let resp = match read.and_then(|l| ipc::decode_request(&l)) {
        Ok(req) => match Waiter::enter(&waiting) {
            None => {
                log_warn!("pipe: {MAX_WAITING} requests already wait for an answer; busy");
                Response::Error { message: "busy".into() }
            }
            Some(_waiter) => {
                let wait = answer_timeout(&req);
                let (reply, rx) = Reply::channel();
                let closed = reply.closed_flag();
                dispatch(req, reply);
                match wait_answer(&pipe, &rx, wait, &closed) {
                    Some(r) => r,
                    None => return, // client gone
                }
            }
        },
        Err(IpcError::Eof) | Err(IpcError::Timeout) => return,
        Err(e) => {
            log_warn!("pipe: rejected request: {e}");
            Response::Error { message: e.to_string() }
        }
    };
    write_all(&pipe, ipc::encode(&resp).as_bytes());
    // Let the client read the answer before the handle goes away (bounded: FlushFileBuffers
    // would block forever on a client that never reads).
    let t = Instant::now();
    let mut avail = 0u32;
    while t.elapsed() < Duration::from_millis(1000) {
        let ok = unsafe { PeekNamedPipe(pipe.0, None, 0, None, Some(&mut avail), None) };
        if ok.is_err() {
            break; // client closed
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    unsafe {
        let _ = DisconnectNamedPipe(pipe.0);
    }
}
