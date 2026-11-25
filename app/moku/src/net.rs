//! Minimal HTTP(S) over WinHTTP (system TLS and proxy settings, no extra crates).
//! Blocking: call it from a service or worker thread, never from the UI thread.
//!
//! * [`get`] / [`get_text`]: a whole GET (weather, lyrics, calendars).
//! * [`request`]: the generic call: any method, headers, a body, and the response handed to a
//!   callback piece by piece as it arrives (the server-sent events of "Tanya AI"). A [`Cancel`]
//!   token aborts a call that is waiting for data, from another thread. Redirects can be turned
//!   off (a key in a header must never follow a redirect to another host), and a loopback
//!   address is never sent through the system proxy.
//!
//! Allowed hosts (README "Network"): Open-Meteo (weather, geocoding, air quality), LRCLIB (lyrics),
//! calendar.google.com (the public holiday calendar), data.bmkg.go.id and www.bmkg.go.id (BMKG),
//! api.frankfurter.dev (exchange rate), the user's own calendar addresses and, for "Tanya AI", only
//! the base URL the user configured; the updater has its own fetch. Each only while its feature is
//! enabled. This file never logs a URL or a header.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Networking::WinHttp::*;
use windows::core::{HSTRING, PCWSTR};

/// Largest response body accepted by [`get`].
pub const MAX_BODY: usize = 4 * 1024 * 1024;

/// The error text of a call stopped with [`Cancel::cancel`].
pub const CANCELLED: &str = "cancelled";
/// The error text of a call that ran past its `total_timeout`.
pub const TIMED_OUT: &str = "timeout";

struct H(*mut core::ffi::c_void);
impl Drop for H {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// The process-wide WinHTTP session: (handle, requests using it).
static SESSION: std::sync::Mutex<(usize, usize)> = std::sync::Mutex::new((0, 0));

/// Use of the shared WinHTTP session by one request. Weather and lyrics running at the same
/// time share one session; when the last request in flight is done the session is closed, so
/// an idle island holds no WinHTTP resolver / proxy / TLS state (requests are minutes apart).
struct SessionUse(*mut core::ffi::c_void);

impl SessionUse {
    fn acquire() -> Result<SessionUse, String> {
        let mut g = SESSION.lock().map_err(|_| "WinHTTP session lock poisoned".to_string())?;
        if g.0 == 0 {
            // A failed open is not cached: the next request tries again.
            g.0 = unsafe { WinHttpOpen(&HSTRING::from(concat!("Moku/", env!("CARGO_PKG_VERSION"))), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0) } as usize;
            if g.0 == 0 {
                return Err("WinHttpOpen failed".into());
            }
        }
        g.1 += 1;
        Ok(SessionUse(g.0 as *mut core::ffi::c_void))
    }
}

impl Drop for SessionUse {
    fn drop(&mut self) {
        if let Ok(mut g) = SESSION.lock() {
            g.1 = g.1.saturating_sub(1);
            if g.1 == 0 && g.0 != 0 {
                unsafe {
                    let _ = WinHttpCloseHandle(g.0 as *mut core::ffi::c_void);
                }
                g.0 = 0;
            }
        }
    }
}

/// Is a WinHTTP session open right now (tests)?
fn session_open() -> bool {
    SESSION.lock().map(|g| g.0 != 0).unwrap_or(false)
}

// ---- cancel -----------------------------------------------------------------------------------

/// Lets another thread stop a [`request`] that is waiting for the network: `cancel()` closes the
/// request's WinHTTP handle, which wakes the blocked read, and the call returns
/// `Err(CANCELLED)`. Make one per call (a used token stays cancelled).
#[derive(Default)]
pub struct Cancel {
    flag: AtomicBool,
    /// The raw request handle while the call is in flight (0 = none).
    req: AtomicUsize,
}

impl Cancel {
    pub fn new() -> Cancel {
        Cancel::default()
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Stop the call (now or before it starts). Safe to call from any thread, any number of times.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        // Whoever swaps the handle out closes it: us here, or the call when it finishes first.
        let h = self.req.swap(0, Ordering::SeqCst);
        if h != 0 {
            unsafe {
                let _ = WinHttpCloseHandle(h as *mut core::ffi::c_void);
            }
        }
    }
}

/// The request handle of one call: registered with its [`Cancel`], closed exactly once.
struct Req<'a> {
    raw: *mut core::ffi::c_void,
    cancel: Option<&'a Cancel>,
}

impl<'a> Req<'a> {
    fn new(raw: *mut core::ffi::c_void, cancel: Option<&'a Cancel>) -> Req<'a> {
        if let Some(c) = cancel {
            c.req.store(raw as usize, Ordering::SeqCst);
        }
        Req { raw, cancel }
    }
}

impl Drop for Req<'_> {
    fn drop(&mut self) {
        let h = match self.cancel {
            Some(c) => c.req.swap(0, Ordering::SeqCst),
            None => self.raw as usize,
        };
        if h != 0 {
            unsafe {
                let _ = WinHttpCloseHandle(h as *mut core::ffi::c_void);
            }
        }
    }
}

// ---- the generic request ------------------------------------------------------------------------

/// How to make a [`request`].
#[derive(Clone, Copy)]
pub struct Options<'a> {
    pub method: &'a str,
    /// Extra request headers. Names must be plain tokens and values single-line (checked).
    pub headers: &'a [(&'a str, &'a str)],
    pub body: Option<&'a [u8]>,
    /// Resolve, connect and send timeout.
    pub connect_timeout: Duration,
    /// The longest wait for the next piece of the answer.
    pub read_timeout: Duration,
    /// Give up with [`TIMED_OUT`] when the whole call takes longer (checked between reads).
    pub total_timeout: Option<Duration>,
    /// Follow HTTP redirects. Off for anything that carries a secret in a header.
    pub follow_redirects: bool,
    pub cancel: Option<&'a Cancel>,
}

impl<'a> Options<'a> {
    /// A plain GET with one timeout for each step, and a total of **three times** that: a server that trickles
    /// a byte every few seconds can not keep a service thread for ever.
    pub fn get(timeout: Duration) -> Options<'a> {
        Options { method: "GET", headers: &[], body: None, connect_timeout: timeout, read_timeout: timeout, total_timeout: Some(timeout.saturating_mul(3)), follow_redirects: true, cancel: None }
    }
}

/// What a [`request`] hands to its callback.
pub enum Event<'a> {
    /// The HTTP status, once, before any data.
    Status(u32),
    /// The next piece of the body (as read: no particular size, not aligned to anything).
    Data(&'a [u8]),
}

/// Is this host the PC itself (never sent through a proxy)?
fn is_loopback(host: &str) -> bool {
    moku_core::ai::is_local_host(host)
}

/// `Name: value\r\n` lines as UTF-16. A header that could smuggle another one (a line break or a
/// NUL in the name or value) is an error, never sent.
fn header_block(headers: &[(&str, &str)]) -> Result<Vec<u16>, String> {
    let mut s = String::new();
    for (n, v) in headers {
        let name_ok = !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if !name_ok || v.chars().any(|c| matches!(c, '\r' | '\n' | '\0')) {
            return Err("invalid request header".into());
        }
        s.push_str(n);
        s.push_str(": ");
        s.push_str(v);
        s.push_str("\r\n");
    }
    Ok(s.encode_utf16().collect())
}

/// Make a request and hand the answer to `sink` as it arrives: first `Event::Status`, then
/// `Event::Data` pieces. `sink` returns `true` to go on and `false` to stop reading (not an
/// error). Returns the HTTP status. Errors are short human-readable strings that never contain
/// the URL, a header or the body; [`CANCELLED`] and [`TIMED_OUT`] are fixed texts.
pub fn request(url: &str, o: &Options, sink: &mut dyn FnMut(Event<'_>) -> bool) -> Result<u32, String> {
    let cancelled = || o.cancel.is_some_and(|c| c.is_cancelled());
    if cancelled() {
        return Err(CANCELLED.into());
    }
    // WinHttpCrackUrl reads a NUL-terminated string when the length is 0: never pass it nothing.
    if url.trim().is_empty() {
        return Err("bad url".into());
    }
    let headers = header_block(o.headers)?;
    let ms =|d: Duration| d.as_millis().clamp(1, i32::MAX as u128) as i32;
    // A failed WinHTTP call after a cancel is the cancel, not a network error.
    let fail = |what: &str, e: windows::core::Error| if cancelled() { CANCELLED.to_string() } else { format!("{what}: {e}") };
    let started = Instant::now();
    unsafe {
        let wurl: Vec<u16> = url.encode_utf16().collect();
        let mut host = vec![0u16; 256];
        let mut path = vec![0u16; 4096];
        let mut uc = URL_COMPONENTS {
            dwStructSize: size_of::<URL_COMPONENTS>() as u32,
            lpszHostName: windows::core::PWSTR(host.as_mut_ptr()),
            dwHostNameLength: host.len() as u32,
            lpszUrlPath: windows::core::PWSTR(path.as_mut_ptr()),
            dwUrlPathLength: path.len() as u32,
            ..Default::default()
        };
        // Path buffer also receives the query when lpszExtraInfo is not given.
        WinHttpCrackUrl(&wurl, 0, &mut uc).map_err(|_| "bad url".to_string())?;
        if uc.nScheme != WINHTTP_INTERNET_SCHEME_HTTPS && uc.nScheme != WINHTTP_INTERNET_SCHEME_HTTP {
            return Err("bad url".into());
        }
        let host_s = String::from_utf16_lossy(&host[..uc.dwHostNameLength as usize]);
        let path_s = String::from_utf16_lossy(&path[..uc.dwUrlPathLength as usize]);
        let secure = uc.nScheme == WINHTTP_INTERNET_SCHEME_HTTPS;

        // Declared before the connection and request handles, so it is dropped (and the
        // session possibly closed) after them.
        let session = SessionUse::acquire()?;
        let conn = H(WinHttpConnect(session.0, &HSTRING::from(host_s.as_str()), uc.nPort, 0));
        if conn.0.is_null() {
            return Err("connect failed".into());
        }
        let flags = if secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let raw = WinHttpOpenRequest(conn.0, &HSTRING::from(o.method), &HSTRING::from(path_s), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), flags);
        if raw.is_null() {
            return Err("open request failed".into());
        }
        let req = Req::new(raw, o.cancel);
        if cancelled() {
            return Err(CANCELLED.into());
        }
        let _ = WinHttpSetTimeouts(req.raw, ms(o.connect_timeout), ms(o.connect_timeout), ms(o.connect_timeout), ms(o.read_timeout));
        if is_loopback(&host_s) {
            // The PC itself: never through the system proxy / PAC script.
            let info = WINHTTP_PROXY_INFO { dwAccessType: WINHTTP_ACCESS_TYPE_NO_PROXY, ..Default::default() };
            let bytes = std::slice::from_raw_parts(&info as *const WINHTTP_PROXY_INFO as *const u8, size_of::<WINHTTP_PROXY_INFO>());
            let _ = WinHttpSetOption(Some(req.raw as *const _), WINHTTP_OPTION_PROXY, Some(bytes));
        }
        if !o.follow_redirects {
            let _ = WinHttpSetOption(Some(req.raw as *const _), WINHTTP_OPTION_REDIRECT_POLICY, Some(&WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_le_bytes()));
        }
        let (bptr, blen) = match o.body {
            Some(b) => (Some(b.as_ptr() as *const core::ffi::c_void), b.len() as u32),
            None => (None, 0),
        };
        let hdrs = if headers.is_empty() { None } else { Some(headers.as_slice()) };
        WinHttpSendRequest(req.raw, hdrs, bptr, blen, blen, 0).map_err(|e| fail("send", e))?;
        WinHttpReceiveResponse(req.raw, std::ptr::null_mut()).map_err(|e| fail("receive", e))?;
        let mut status = 0u32;
        let mut len = size_of::<u32>() as u32;
        let _ = WinHttpQueryHeaders(
            req.raw,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut len,
            std::ptr::null_mut(),
        );
        if !sink(Event::Status(status)) {
            return Ok(status);
        }
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            if cancelled() {
                return Err(CANCELLED.into());
            }
            if o.total_timeout.is_some_and(|t| started.elapsed() > t) {
                return Err(TIMED_OUT.into());
            }
            // `WinHttpReadData` alone waits until its whole buffer is full (or the answer ends), so
            // a stream would arrive in one lump at the end: ask what is available first (that
            // returns as soon as ANY data has arrived) and read exactly that much.
            let mut avail = 0u32;
            WinHttpQueryDataAvailable(req.raw, &mut avail).map_err(|e| fail("read", e))?;
            if avail == 0 {
                break;
            }
            let want = (avail as usize).min(buf.len());
            let mut got = 0u32;
            WinHttpReadData(req.raw, buf.as_mut_ptr() as _, want as u32, &mut got).map_err(|e| fail("read", e))?;
            if got == 0 {
                break;
            }
            if !sink(Event::Data(&buf[..got as usize])) {
                break;
            }
        }
        Ok(status)
    }
}

// ---- GET -----------------------------------------------------------------------------------------

/// GET `url`; returns (HTTP status, body). Errors are human-readable strings.
pub fn get(url: &str, timeout: Duration) -> Result<(u32, Vec<u8>), String> {
    let mut body: Vec<u8> = Vec::new();
    let mut too_big = false;
    let status = request(url, &Options::get(timeout), &mut |e| match e {
        Event::Status(_) => true,
        Event::Data(d) => {
            if body.len() + d.len() > MAX_BODY {
                too_big = true;
                return false;
            }
            body.extend_from_slice(d);
            true
        }
    })?;
    if too_big {
        return Err("response too large".into());
    }
    Ok((status, body))
}

/// GET and decode as UTF-8 text; non-2xx statuses are errors.
pub fn get_text(url: &str, timeout: Duration) -> Result<String, String> {
    let (status, body) = get(url, timeout)?;
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}"));
    }
    String::from_utf8(body).map_err(|_| "response is not UTF-8".into())
}

/// Percent-encode a query component (`city`, `track_name`, ...).
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The WinHTTP session is one shared process-wide object whose closing `session_closed_when_idle`
/// observes, so every test that touches the network (here and in `features/ai`) takes this lock.
#[cfg(test)]
pub fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
    L.lock().unwrap_or_else(|p| p.into_inner())
}

/// A tiny HTTP server on 127.0.0.1 for tests (no external network): it records every request and
/// answers from a script, with chunked bodies, pauses, a connection that hangs, and one that
/// cuts off half way. Used by the `net` tests and by `features/ai` (SSE, provider calls).
#[cfg(test)]
pub mod mock {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// One request the server saw.
    #[derive(Clone, Debug, Default)]
    pub struct Seen {
        pub method: String,
        pub path: String,
        pub headers: Vec<(String, String)>,
        pub body: Vec<u8>,
    }

    impl Seen {
        pub fn header(&self, name: &str) -> Option<&str> {
            self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
        }

        pub fn body_text(&self) -> String {
            String::from_utf8_lossy(&self.body).to_string()
        }
    }

    /// What the server does after the status line and headers.
    #[derive(Clone, Debug)]
    pub enum Step {
        /// Send these bytes as one HTTP chunk.
        Send(Vec<u8>),
        Sleep(u64),
        /// Keep the connection open and silent until the server stops.
        Hang,
        /// Drop the connection without the final chunk (a cut-off stream).
        Cut,
    }

    #[derive(Clone, Debug)]
    pub struct Script {
        pub status: u16,
        pub headers: Vec<(String, String)>,
        /// Chunked body steps. Ignored when `plain` is set.
        pub steps: Vec<Step>,
        /// A body with a `Content-Length`, no chunking.
        pub plain: Option<Vec<u8>>,
    }

    impl Script {
        pub fn sse(steps: Vec<Step>) -> Script {
            Script { status: 200, headers: vec![("Content-Type".into(), "text/event-stream".into())], steps, plain: None }
        }

        pub fn json(status: u16, body: &str) -> Script {
            Script { status, headers: vec![("Content-Type".into(), "application/json".into())], steps: vec![], plain: Some(body.as_bytes().to_vec()) }
        }

        pub fn redirect(to: &str) -> Script {
            Script { status: 302, headers: vec![("Location".into(), to.into())], steps: vec![], plain: Some(vec![]) }
        }
    }

    pub type Handler = Arc<dyn Fn(&Seen) -> Script + Send + Sync>;

    pub struct Server {
        pub addr: SocketAddr,
        pub seen: Arc<Mutex<Vec<Seen>>>,
        stop: Arc<AtomicBool>,
        join: Option<std::thread::JoinHandle<()>>,
    }

    impl Server {
        pub fn start(handler: impl Fn(&Seen) -> Script + Send + Sync + 'static) -> Server {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1");
            let addr = listener.local_addr().unwrap();
            let seen = Arc::new(Mutex::new(vec![]));
            let stop = Arc::new(AtomicBool::new(false));
            let handler: Handler = Arc::new(handler);
            let (s2, st2) = (seen.clone(), stop.clone());
            let join = std::thread::spawn(move || {
                for conn in listener.incoming() {
                    if st2.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(conn) = conn else { continue };
                    let (h, s, st) = (handler.clone(), s2.clone(), st2.clone());
                    std::thread::spawn(move || serve(conn, h, s, st));
                }
            });
            Server { addr, seen, stop, join: Some(join) }
        }

        pub fn url(&self, path: &str) -> String {
            format!("http://127.0.0.1:{}{path}", self.addr.port())
        }

        /// The base URL a provider setting would hold.
        pub fn base(&self) -> String {
            self.url("/v1")
        }

        pub fn requests(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            // Wake the accept loop.
            let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(500));
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
        }
    }

    fn read_request(conn: &mut TcpStream) -> Option<Seen> {
        conn.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        let mut buf: Vec<u8> = vec![];
        let mut tmp = [0u8; 4096];
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i;
            }
            let n = conn.read(&mut tmp).ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.len() > 1 << 20 {
                return None;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let mut lines = head.split("\r\n");
        let mut first = lines.next()?.split(' ');
        let (method, path) = (first.next()?.to_string(), first.next()?.to_string());
        let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':').map(|(n, v)| (n.trim().to_string(), v.trim().to_string()))).collect();
        let want: usize = headers.iter().find(|(n, _)| n.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
        let mut body = buf[head_end + 4..].to_vec();
        while body.len() < want {
            let n = conn.read(&mut tmp).ok()?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&tmp[..n]);
        }
        Some(Seen { method, path, headers, body })
    }

    fn serve(mut conn: TcpStream, handler: Handler, seen: Arc<Mutex<Vec<Seen>>>, stop: Arc<AtomicBool>) {
        let Some(req) = read_request(&mut conn) else { return };
        seen.lock().unwrap().push(req.clone());
        let script = handler(&req);
        let mut head = format!("HTTP/1.1 {} X\r\nConnection: close\r\n", script.status);
        for (n, v) in &script.headers {
            head.push_str(&format!("{n}: {v}\r\n"));
        }
        match &script.plain {
            Some(b) => {
                head.push_str(&format!("Content-Length: {}\r\n\r\n", b.len()));
                let _ = conn.write_all(head.as_bytes());
                let _ = conn.write_all(b);
                return;
            }
            None => head.push_str("Transfer-Encoding: chunked\r\n\r\n"),
        }
        if conn.write_all(head.as_bytes()).is_err() {
            return;
        }
        for step in &script.steps {
            match step {
                Step::Send(b) => {
                    let mut c = format!("{:X}\r\n", b.len()).into_bytes();
                    c.extend_from_slice(b);
                    c.extend_from_slice(b"\r\n");
                    if conn.write_all(&c).is_err() || conn.flush().is_err() {
                        return;
                    }
                }
                Step::Sleep(ms) => std::thread::sleep(Duration::from_millis(*ms)),
                Step::Hang => {
                    while !stop.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(20));
                        // A client that went away ends the wait early.
                        if conn.write_all(b"").is_err() {
                            return;
                        }
                    }
                    return;
                }
                Step::Cut => return,
            }
        }
        let _ = conn.write_all(b"0\r\n\r\n");
    }
}

#[cfg(test)]
mod tests {
    use super::mock::{Script, Server, Step};
    use super::*;
    use std::sync::Arc;

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        test_lock()
    }

    #[test]
    fn percent_encode() {
        assert_eq!(super::encode("Kaia & Co/ü"), "Kaia%20%26%20Co%2F%C3%BC");
    }

    /// The session is shared while requests overlap and closed when the last one is done.
    #[test]
    fn session_closed_when_idle() {
        let _s = serial();
        let a = super::SessionUse::acquire().unwrap();
        let b = super::SessionUse::acquire().unwrap();
        assert_eq!(a.0, b.0, "shared");
        drop(a);
        assert!(super::session_open(), "still used by b");
        drop(b);
        assert!(!super::session_open(), "closed when idle");
        // A request to nowhere (no network needed: the port is closed) also leaves it closed.
        let _ = super::get("http://127.0.0.1:9/x", std::time::Duration::from_millis(500));
        assert!(!super::session_open());
    }

    fn collect(url: &str, o: &Options) -> (Result<u32, String>, Vec<u8>, Vec<usize>) {
        let (mut body, mut sizes) = (vec![], vec![]);
        let r = request(url, o, &mut |e| {
            if let Event::Data(d) = e {
                body.extend_from_slice(d);
                sizes.push(d.len());
            }
            true
        });
        (r, body, sizes)
    }

    #[test]
    fn get_and_post_against_the_local_server() {
        let _s = serial();
        let srv = Server::start(|s| match (s.method.as_str(), s.path.as_str()) {
            ("GET", "/hello") => Script::json(200, "{\"hi\":1}"),
            ("POST", "/echo") => Script::json(200, &s.body_text()),
            _ => Script::json(404, "{}"),
        });
        assert_eq!(get_text(&srv.url("/hello"), Duration::from_secs(5)).unwrap(), "{\"hi\":1}");
        assert_eq!(get(&srv.url("/nope"), Duration::from_secs(5)).unwrap().0, 404);
        assert_eq!(get_text(&srv.url("/nope"), Duration::from_secs(5)).unwrap_err(), "HTTP 404");
        let o = Options { method: "POST", headers: &[("Content-Type", "application/json"), ("Authorization", "Bearer sk-test")], body: Some(b"{\"a\":[1,2,3]}"), ..Options::get(Duration::from_secs(5)) };
        let (r, body, _) = collect(&srv.url("/echo"), &o);
        assert_eq!(r, Ok(200));
        assert_eq!(body, b"{\"a\":[1,2,3]}");
        let seen = srv.requests();
        let post = seen.iter().find(|s| s.method == "POST").unwrap();
        assert_eq!(post.header("authorization"), Some("Bearer sk-test"));
        assert_eq!(post.header("content-type"), Some("application/json"));
        assert_eq!(post.header("content-length"), Some("13"));
    }

    /// Pieces arrive as the server sends them (not all at the end), and a sink can stop early.
    #[test]
    fn a_chunked_body_is_delivered_piece_by_piece() {
        let _s = serial();
        let srv = Server::start(|_| Script::sse(vec![Step::Send(b"one\n".to_vec()), Step::Sleep(150), Step::Send(b"two\n".to_vec()), Step::Sleep(150), Step::Send(b"three\n".to_vec())]));
        let t0 = Instant::now();
        let mut arrivals: Vec<(String, u128)> = vec![];
        let r = request(&srv.url("/s"), &Options::get(Duration::from_secs(5)), &mut |e| {
            if let Event::Data(d) = e {
                arrivals.push((String::from_utf8_lossy(d).to_string(), t0.elapsed().as_millis()));
            }
            true
        });
        assert_eq!(r, Ok(200));
        let all: String = arrivals.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(all, "one\ntwo\nthree\n");
        assert!(arrivals.len() >= 3, "streamed in pieces: {arrivals:?}");
        assert!(arrivals[0].1 + 150 < arrivals.last().unwrap().1, "the first piece did not wait for the last: {arrivals:?}");
        // The sink can stop after the first piece.
        let mut n = 0;
        let r = request(&srv.url("/s"), &Options::get(Duration::from_secs(5)), &mut |e| {
            if let Event::Data(_) = e {
                n += 1;
                return false;
            }
            true
        });
        assert_eq!((r, n), (Ok(200), 1));
    }

    #[test]
    fn get_refuses_a_response_over_the_limit() {
        let _s = serial();
        let big = Arc::new(vec![b'x'; MAX_BODY + 1000]);
        let b2 = big.clone();
        let srv = Server::start(move |_| Script { status: 200, headers: vec![], steps: vec![], plain: Some((*b2).clone()) });
        assert_eq!(get(&srv.url("/big"), Duration::from_secs(10)).unwrap_err(), "response too large");
    }

    /// A redirect is not followed when asked not to (the key in a header must not travel on).
    #[test]
    fn redirects_can_be_refused() {
        let _s = serial();
        let other = Server::start(|_| Script::json(200, "{\"leaked\":true}"));
        let target = other.url("/stolen");
        let srv = Server::start(move |_| Script::redirect(&target));
        let o = Options { follow_redirects: false, headers: &[("Authorization", "Bearer sk-test")], ..Options::get(Duration::from_secs(5)) };
        let (r, body, _) = collect(&srv.url("/r"), &o);
        assert_eq!(r, Ok(302), "the 302 itself is the answer");
        assert!(body.is_empty());
        assert!(other.requests().is_empty(), "the second server never saw the key");
        // With redirects on (the default for get) the other server is reached.
        let (r, body, _) = collect(&srv.url("/r"), &Options::get(Duration::from_secs(5)));
        assert_eq!(r, Ok(200));
        assert_eq!(body, b"{\"leaked\":true}");
    }

    #[test]
    fn a_header_that_could_smuggle_another_is_never_sent() {
        let _s = serial();
        let srv = Server::start(|_| Script::json(200, "{}"));
        for bad in [("Authorization", "Bearer x\r\nX-Evil: 1"), ("Authorization", "Bearer x\ny"), ("Bad Name", "v"), ("", "v"), ("X\r\nEvil", "v"), ("X", "a\0b")] {
            let hs = [bad];
            let o = Options { headers: &hs, ..Options::get(Duration::from_secs(5)) };
            let (r, _, _) = collect(&srv.url("/"), &o);
            assert_eq!(r, Err("invalid request header".into()), "{bad:?}");
        }
        assert!(srv.requests().is_empty(), "nothing reached the server");
    }

    #[test]
    fn bad_urls_and_unreachable_hosts_are_errors_not_panics() {
        let _s = serial();
        for u in ["", "nonsense", "ftp://x.com/a", "file:///C:/x", "http://", "https://[::1"] {
            assert!(request(u, &Options::get(Duration::from_millis(300)), &mut |_| true).is_err(), "{u:?}");
        }
        // A closed port.
        assert!(get("http://127.0.0.1:9/x", Duration::from_millis(800)).is_err());
    }

    /// `cancel()` from another thread wakes a read that is waiting for data.
    #[test]
    fn cancel_wakes_a_stalled_read() {
        let _s = serial();
        let srv = Server::start(|_| Script::sse(vec![Step::Send(b"first\n".to_vec()), Step::Hang]));
        let cancel = Arc::new(Cancel::new());
        let c2 = cancel.clone();
        let url = srv.url("/hang");
        // What the worker has received so far, where the test can see it.
        let arrived = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let arrived2 = arrived.clone();
        let worker = std::thread::spawn(move || {
            let o = Options { cancel: Some(&c2), read_timeout: Duration::from_secs(30), ..Options::get(Duration::from_secs(30)) };
            let mut got = vec![];
            let r = request(&url, &o, &mut |e| {
                if let Event::Data(d) = e {
                    got.extend_from_slice(d);
                    arrived2.store(got.len(), std::sync::atomic::Ordering::SeqCst);
                }
                true
            });
            (r, got, Instant::now())
        });
        // Cancel once the first line has really arrived (the server then hangs), not after a guessed time.
        let deadline = Instant::now() + Duration::from_secs(5);
        while arrived.load(std::sync::atomic::Ordering::SeqCst) < b"first\n".len() {
            assert!(Instant::now() < deadline, "the first line never arrived");
            std::thread::sleep(Duration::from_millis(2));
        }
        let t = Instant::now();
        cancel.cancel();
        let (r, got, done) = worker.join().unwrap();
        assert_eq!(r, Err(CANCELLED.to_string()));
        assert_eq!(got, b"first\n", "what arrived before the cancel was delivered");
        assert!(done.duration_since(t) < Duration::from_secs(3), "the call ended promptly after cancel, not after its 30 s timeout");
        // A token cancelled before the call stops it before anything is sent.
        let c = Cancel::new();
        c.cancel();
        let o = Options { cancel: Some(&c), ..Options::get(Duration::from_secs(5)) };
        assert_eq!(request(&srv.url("/x"), &o, &mut |_| true), Err(CANCELLED.to_string()));
        c.cancel(); // twice is fine
        assert!(!session_open(), "the session is closed again");
    }

    #[test]
    fn a_silent_server_hits_the_read_timeout_and_a_long_stream_the_total_timeout() {
        let _s = serial();
        let srv = Server::start(|_| Script::sse(vec![Step::Hang]));
        let o = Options { read_timeout: Duration::from_millis(400), ..Options::get(Duration::from_secs(5)) };
        let t = Instant::now();
        let r = request(&srv.url("/silent"), &o, &mut |_| true);
        assert!(r.is_err(), "{r:?}");
        // The 400 ms read timeout ended it, not the 15 s total one. The bound is loose on purpose:
        // CI runners are slow and run many tests at once.
        let took = t.elapsed();
        assert!(took < Duration::from_secs(12), "took {took:?}: {r:?}");
        // A server that trickles forever is cut by the total timeout.
        let steps: Vec<Step> = (0..40).flat_map(|_| [Step::Send(b"x".to_vec()), Step::Sleep(50)]).collect();
        let srv = Server::start(move |_| Script::sse(steps.clone()));
        let o = Options { total_timeout: Some(Duration::from_millis(400)), ..Options::get(Duration::from_secs(5)) };
        assert_eq!(request(&srv.url("/slow"), &o, &mut |_| true), Err(TIMED_OUT.to_string()));
    }

    #[test]
    fn a_plain_get_has_a_total_timeout_of_three_times_its_read_timeout() {
        let o = Options::get(Duration::from_secs(5));
        assert_eq!((o.connect_timeout, o.read_timeout, o.total_timeout), (Duration::from_secs(5), Duration::from_secs(5), Some(Duration::from_secs(15))));
        assert_eq!(Options::get(Duration::from_millis(300)).total_timeout, Some(Duration::from_millis(900)));
        // And it is enforced: a server that sends a byte every 50 ms never trips the 300 ms read timeout, but
        // `get` gives up after 900 ms in all.
        let _s = serial();
        let steps: Vec<Step> = (0..200).flat_map(|_| [Step::Send(b"x".to_vec()), Step::Sleep(50)]).collect();
        let srv = Server::start(move |_| Script::sse(steps.clone()));
        let t = Instant::now();
        let r = super::get(&srv.url("/trickle"), Duration::from_millis(300));
        assert_eq!(r, Err(TIMED_OUT.to_string()));
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    }

    #[test]
    fn loopback_requests_bypass_the_proxy_setting() {
        assert!(is_loopback("127.0.0.1") && is_loopback("localhost") && is_loopback("::1"));
        assert!(!is_loopback("api.openai.com") && !is_loopback("127.0.0.1.evil.com"));
    }

    /// Network check against an allowed host; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn https_get_open_meteo() {
        let t = super::get_text("https://geocoding-api.open-meteo.com/v1/search?name=Jakarta&count=1", std::time::Duration::from_secs(10)).unwrap();
        assert!(t.contains("Jakarta"), "{t}");
    }
}
