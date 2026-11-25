//! The temporary HTTP server of one transfer session.
//!
//! * One listener on ONE address (the LAN adapter's IPv4 in real use, `127.0.0.1` in tests and with
//!   the debug override), on a random port. No other thread exists while no session runs.
//! * Every connection is a short-lived thread (at most [`Limits::max_conns`] at once, [`Limits::max_per_ip`] per address; one request
//!   per connection). A connection from another IP than the device that owns the session is refused
//!   before anything is read.
//! * Routing is by literal segments after the token: `/<token>/` (the page), `state`, `offer`,
//!   `up/<n>`, `text`, `f/<n>`. There is no file serving by path and no directory listing: a download
//!   is an index into the list the PC offered, an upload an index into the list the phone announced
//!   and the user approved.
//! * Every request is checked in this order: `Host` equals the bound address (DNS rebinding), the
//!   token (constant time), expiry, the one-device lock, then, for `POST`, a same-origin `Origin`, the
//!   `X-Moku` header (a cross-site page can not send it without a preflight, which is never answered)
//!   and the approval the user gave on the island.
//! * Uploads stream through the multipart parser into `<name>.mokupart` in the receive folder and
//!   are renamed to their final, never existing, name only when complete; anything else removes the
//!   half-written file. Sizes are capped by what was announced (and 2 GiB).
//! * Slow clients are cut off: 10 s for the request head, 30 s without a byte for a body or a download.
//!
//! The session rules themselves (token, expiry, lock, approval) live in [`session`](super::session).

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::http::{self, HttpError, Method, Request};
use super::multipart::{self, MpError, PushError};
use super::names;
use super::page;
use super::session::{self, Mode, OfferSummary, Reject, Session};

/// Biggest `offer` body (JSON).
const MAX_OFFER_BODY: u64 = 64 * 1024;
/// What a multipart body may add to the file itself (boundary lines, part headers).
const MULTIPART_OVERHEAD: u64 = 64 * 1024;
/// Largest piece read from or written to a socket at once.
const CHUNK: usize = 64 * 1024;
/// How often a running transfer tells the island how far it is.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The whole request head must arrive within this.
    pub head: Duration,
    /// A body or a download may go this long without a byte moving.
    pub idle: Duration,
    pub write: Duration,
    pub max_conns: usize,
    /// One IP may hold this many of them (a browser opens up to six; a flood from one address can not take all).
    pub max_per_ip: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { head: Duration::from_secs(10), idle: Duration::from_secs(30), write: Duration::from_secs(30), max_conns: 8, max_per_ip: 6 }
    }
}

/// What the island is told (from server threads; the receiver posts it to the UI thread).
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The first valid request: this device now owns the session.
    Connected(IpAddr),
    /// The phone announced a transfer (`needs_approval`: show the card).
    Offer(OfferSummary),
    /// Bytes moved: `upload` or download of file `index`.
    Progress { upload: bool, index: usize, done: u64, total: u64 },
    /// A file arrived and has its final name.
    FileSaved { index: usize, path: PathBuf },
    UploadFailed { index: usize },
    /// A text arrived (shown, never logged).
    Text(String),
    /// The phone downloaded file `index` completely.
    Downloaded { index: usize },
}

pub struct Shared {
    session: Mutex<Session>,
    pub stop: AtomicBool,
    pub recv_dir: PathBuf,
    /// "ip:port" as the phone writes it in the `Host` header.
    pub host: String,
    pub limits: Limits,
    notify: Box<dyn Fn(Event) + Send + Sync>,
    slots: Mutex<Slots>,
    /// Tests pretend the next connections come from this IP.
    #[cfg(test)]
    peer_override: Mutex<Option<IpAddr>>,
}

impl Shared {
    /// Run `f` on the session (never hold the lock across I/O).
    pub fn with<R>(&self, f: impl FnOnce(&mut Session) -> R) -> R {
        let mut g = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut g)
    }

    fn emit(&self, e: Event) {
        (self.notify)(e);
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

pub struct Server {
    sh: Arc<Shared>,
    addr: SocketAddr,
    join: Option<JoinHandle<()>>,
}

impl Server {
    /// Bind `bind` (port 0 = random) and start accepting. The caller decides the address: the LAN
    /// adapter's, or loopback for tests.
    pub fn start(bind: SocketAddr, mode: Mode, recv_dir: PathBuf, limits: Limits, notify: impl Fn(Event) + Send + Sync + 'static) -> io::Result<Server> {
        let listener = TcpListener::bind(bind)?;
        let addr = listener.local_addr()?;
        let token = session::new_token().map_err(io::Error::other)?;
        let sh = Arc::new(Shared {
            session: Mutex::new(Session::new(token, mode, Instant::now())),
            stop: AtomicBool::new(false),
            recv_dir,
            host: addr.to_string(),
            limits,
            notify: Box::new(notify),
            slots: Mutex::new(Slots::default()),
            #[cfg(test)]
            peer_override: Mutex::new(None),
        });
        let sh2 = sh.clone();
        let join = std::thread::Builder::new().name("phone-send".into()).spawn(move || accept_loop(listener, sh2))?;
        Ok(Server { sh, addr, join: Some(join) })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.sh
    }

    /// What the QR code carries.
    pub fn url(&self) -> String {
        let token = self.sh.with(|s| s.token().to_string());
        format!("http://{}/{}/", self.addr, token)
    }

    /// Stop accepting, tell running connections to give up, and wait for the accept thread.
    pub fn stop(&mut self) {
        self.sh.stop.store(true, Ordering::Relaxed);
        self.sh.with(|s| s.stop());
        // The accept call is blocking: a connection to ourselves wakes it.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(500));
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Open connections: in all, and per address.
#[derive(Default)]
struct Slots {
    total: usize,
    per_ip: HashMap<IpAddr, usize>,
}

impl Shared {
    /// Take a connection slot for `ip`, unless the server or that address is at its limit.
    fn acquire(&self, ip: IpAddr) -> bool {
        let mut s = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        let mine = s.per_ip.get(&ip).copied().unwrap_or(0);
        if s.total >= self.limits.max_conns || mine >= self.limits.max_per_ip {
            return false;
        }
        s.total += 1;
        s.per_ip.insert(ip, mine + 1);
        true
    }

    fn release(&self, ip: IpAddr) {
        let mut s = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        s.total = s.total.saturating_sub(1);
        if let Some(n) = s.per_ip.get_mut(&ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                s.per_ip.remove(&ip);
            }
        }
    }
}

/// A taken slot, given back when the connection thread ends.
struct ConnSlot(Arc<Shared>, IpAddr);
impl Drop for ConnSlot {
    fn drop(&mut self) {
        self.0.release(self.1);
    }
}

fn accept_loop(listener: TcpListener, sh: Arc<Shared>) {
    loop {
        let (stream, peer) = match listener.accept() {
            Ok(x) => x,
            Err(_) if sh.stopped() => break,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
        };
        if sh.stopped() {
            break;
        }
        #[allow(unused_mut)]
        let mut peer_ip = peer.ip();
        #[cfg(test)]
        if let Some(ip) = *sh.peer_override.lock().unwrap_or_else(PoisonError::into_inner) {
            peer_ip = ip;
        }
        // One device only: everybody else is turned away before a thread or a byte is spent on them.
        if sh.with(|s| s.device()).is_some_and(|d| d != peer_ip) {
            quick_refuse(stream, 403, "device");
            continue;
        }
        if !sh.acquire(peer_ip) {
            quick_refuse(stream, 503, "busy");
            continue;
        }
        let sh2 = sh.clone();
        let spawned = std::thread::Builder::new().name("phone-conn".into()).stack_size(512 * 1024).spawn(move || {
            let slot = ConnSlot(sh2, peer_ip);
            serve_connection(&slot.0, peer_ip, stream);
        });
        if spawned.is_err() {
            sh.release(peer_ip);
        }
    }
}

/// A tiny answer without handling the request. Closing a socket with unread data resets the
/// connection and the answer may never arrive, so a little of the request is read first (briefly:
/// this runs on the accept thread).
fn quick_refuse(mut stream: TcpStream, status: u16, code: &str) {
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_read_timeout(Some(Duration::from_millis(25)));
    let mut sink = [0u8; 4096];
    let _ = stream.read(&mut sink);
    let body = format!("{{\"ok\":false,\"error\":\"{code}\"}}");
    let _ = http::write_simple(&mut stream, status, "application/json", body.as_bytes(), &[]);
    let _ = stream.shutdown(Shutdown::Write);
}

/// Reads the request head, but gives up the moment the server is stopped.
struct StopReader<'a> {
    stream: &'a mut TcpStream,
    sh: &'a Shared,
}

impl Read for StopReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.sh.stopped() {
            return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "stopped"));
        }
        self.stream.read(out)
    }
}

// ---- one connection ---------------------------------------------------------------------------------

struct Fail {
    status: u16,
    code: &'static str,
}

const fn fail(status: u16, code: &'static str) -> Fail {
    Fail { status, code }
}

fn map_reject(r: Reject) -> Fail {
    match r {
        Reject::BadToken | Reject::NotFound | Reject::WrongMode => fail(404, "not_found"),
        Reject::Expired => fail(410, "ended"),
        Reject::OtherDevice => fail(403, "device"),
        Reject::NotApproved => fail(403, "not_approved"),
        Reject::BadOffer(_) => fail(400, "bad_offer"),
        Reject::Conflict => fail(409, "busy"),
    }
}

fn reply_json(stream: &mut TcpStream, status: u16, body: &str) -> io::Result<()> {
    http::write_simple(stream, status, "application/json", body.as_bytes(), &[])
}

fn reply_fail(stream: &mut TcpStream, f: Fail) -> io::Result<()> {
    reply_json(stream, f.status, &format!("{{\"ok\":false,\"error\":\"{}\"}}", f.code))
}

/// Close politely: say we are done writing, then swallow what the client is still sending (a
/// little), so closing with unread data does not turn into a reset that wipes our answer.
fn finish(stream: &mut TcpStream) {
    let _ = stream.shutdown(Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    let until = Instant::now() + Duration::from_millis(1500);
    let mut left = 2 * 1024 * 1024usize;
    let mut buf = [0u8; 8192];
    while left > 0 && Instant::now() < until {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => left = left.saturating_sub(n),
        }
    }
}

pub(crate) fn serve_connection(sh: &Shared, peer: IpAddr, mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let _ = stream.set_write_timeout(Some(sh.limits.write));
    let _ = stream.set_nodelay(true);
    let head = http::read_head(&mut StopReader { stream: &mut stream, sh }, Instant::now() + sh.limits.head);
    let (req, lead) = match head {
        Ok(x) => x,
        // An empty connection (a browser's spare) or a broken one: nothing to say.
        Err(HttpError::Closed) | Err(HttpError::Io(_)) => return,
        Err(e) => {
            let _ = reply_fail(&mut stream, fail(e.status(), "bad_request"));
            finish(&mut stream);
            return;
        }
    };
    if let Err(f) = route(sh, peer, &req, lead, &mut stream) {
        let _ = reply_fail(&mut stream, f);
    }
    finish(&mut stream);
}

fn is_token_shape(s: &str) -> bool {
    s.len() == 22 && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

fn parse_index(s: &str) -> Result<usize, Fail> {
    if s.is_empty() || s.len() > 4 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(fail(404, "not_found"));
    }
    s.parse().map_err(|_| fail(404, "not_found"))
}

fn route(sh: &Shared, peer: IpAddr, req: &Request, lead: Vec<u8>, stream: &mut TcpStream) -> Result<(), Fail> {
    // The page must have been reached by the address the server is bound to (not a name that was
    // pointed at it later).
    if !req.header("host").is_some_and(|h| h.eq_ignore_ascii_case(&sh.host)) {
        return Err(fail(403, "host"));
    }
    let segs: Vec<&str> = req.path[1..].split('/').collect();
    let tok = segs[0];
    if !is_token_shape(tok) {
        return Err(fail(404, "not_found"));
    }
    let rest = &segs[1..];
    let now = Instant::now();
    let (admitted, locked_now) = sh.with(|s| {
        let had = s.device().is_some();
        let r = s.admit(tok, peer, now);
        (r, !had && s.device().is_some())
    });
    match admitted {
        Ok(()) => {}
        // The phone's page asks for the state one last time after the session ended (declined, no
        // answer, expired): it may hear how it ended. Everything else is gone.
        Err(Reject::Expired) if req.method == Method::Get && rest == ["state"] => {
            let j = sh.with(|s| s.state_json());
            return reply_json(stream, 200, &j).map_err(|_| fail(500, "io"));
        }
        Err(r) => return Err(map_reject(r)),
    }
    if locked_now {
        sh.emit(Event::Connected(peer));
    }
    match (req.method, rest) {
        (Method::Get | Method::Head, []) => {
            // The page uses relative links: it must live under a slash.
            let loc = format!("/{tok}/");
            http::write_head(stream, 302, "text/plain", 0, &[("Location", loc.as_str())]).map_err(|_| fail(500, "io"))
        }
        (Method::Get | Method::Head, [""]) => {
            let files = sh.with(|s| match s.mode() {
                Mode::Send(f) => Some(f.iter().map(|x| (x.name.clone(), x.size)).collect::<Vec<_>>()),
                Mode::Receive => None,
            });
            let html = page::html(files.as_deref());
            if req.method == Method::Head {
                http::write_head(stream, 200, "text/html; charset=utf-8", html.len() as u64, &[]).map_err(|_| fail(500, "io"))
            } else {
                http::write_simple(stream, 200, "text/html; charset=utf-8", html.as_bytes(), &[]).map_err(|_| fail(500, "io"))
            }
        }
        (Method::Get, ["state"]) => {
            same_origin_fetch(req)?;
            let j = sh.with(|s| s.state_json());
            reply_json(stream, 200, &j).map_err(|_| fail(500, "io"))
        }
        (Method::Post, ["offer"]) => {
            post_checks(sh, req)?;
            offer(sh, peer, req, lead, stream)
        }
        (Method::Post, ["up", n]) => {
            post_checks(sh, req)?;
            upload(sh, req, lead, stream, parse_index(n)?)
        }
        (Method::Post, ["text"]) => {
            post_checks(sh, req)?;
            text(sh, req, lead, stream)
        }
        (Method::Get | Method::Head, ["f", n]) => download(sh, req, stream, parse_index(n)?),
        (_, [] | [""] | ["state"] | ["offer"] | ["up", _] | ["text"] | ["f", _]) => Err(fail(405, "method")),
        _ => Err(fail(404, "not_found")),
    }
}

/// A `fetch` from the page itself: when the browser says where it comes from, it must be us.
fn same_origin_fetch(req: &Request) -> Result<(), Fail> {
    match req.header("sec-fetch-site") {
        Some("same-origin") | None => Ok(()),
        Some(_) => Err(fail(403, "origin")),
    }
}

/// What a `POST` must carry: our own origin, the custom header, a known length.
fn post_checks(sh: &Shared, req: &Request) -> Result<(), Fail> {
    same_origin_fetch(req)?;
    if let Some(o) = req.header("origin")
        && !o.eq_ignore_ascii_case(&format!("http://{}", sh.host))
    {
        return Err(fail(403, "origin"));
    }
    if req.header("x-moku") != Some("1") {
        return Err(fail(403, "header"));
    }
    if req.content_length.is_none() {
        return Err(fail(411, "length"));
    }
    Ok(())
}

/// `Expect: 100-continue` (curl, some HTTP libraries): say go ahead once the request was accepted.
fn go_ahead(req: &Request, stream: &mut TcpStream) {
    if req.header("expect").is_some_and(|e| e.eq_ignore_ascii_case("100-continue")) {
        let _ = stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
    }
}

fn read_all(body: &mut impl Read, len: u64) -> Result<Vec<u8>, Fail> {
    let mut out = Vec::with_capacity(len.min(64 * 1024) as usize);
    body.take(len).read_to_end(&mut out).map_err(map_read_error)?;
    Ok(out)
}

fn map_read_error(e: io::Error) -> Fail {
    match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => fail(408, "timeout"),
        io::ErrorKind::UnexpectedEof => fail(400, "truncated"),
        _ => fail(400, "read"),
    }
}

// ---- offer / text ------------------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct FileIn {
    name: String,
    size: u64,
}

#[derive(serde::Deserialize)]
struct OfferIn {
    #[serde(default)]
    files: Vec<FileIn>,
    #[serde(default)]
    text: bool,
}

fn offer(sh: &Shared, peer: IpAddr, req: &Request, lead: Vec<u8>, stream: &mut TcpStream) -> Result<(), Fail> {
    let len = req.content_length.unwrap_or(0);
    if len > MAX_OFFER_BODY {
        return Err(fail(413, "size"));
    }
    go_ahead(req, stream);
    let stop = || sh.stopped();
    let mut body = http::Body::new(lead, stream, len, sh.limits.idle, &stop);
    let raw = read_all(&mut body, len)?;
    let now = Instant::now();
    let summary = match sh.with(|s| matches!(s.mode(), Mode::Receive)) {
        true => {
            let parsed: OfferIn = serde_json::from_slice(&raw).map_err(|_| fail(400, "bad_offer"))?;
            let files: Vec<(String, u64)> = parsed.files.into_iter().map(|f| (f.name, f.size)).collect();
            sh.with(|s| s.offer(peer, &files, parsed.text, now)).map_err(map_reject)?
        }
        false => sh.with(|s| s.request_download(peer, now)).map_err(map_reject)?,
    };
    sh.emit(Event::Offer(summary));
    let j = sh.with(|s| s.state_json());
    reply_json(stream, 200, &j).map_err(|_| fail(500, "io"))
}

fn text(sh: &Shared, req: &Request, lead: Vec<u8>, stream: &mut TcpStream) -> Result<(), Fail> {
    let len = req.content_length.unwrap_or(0);
    if len > session::MAX_TEXT as u64 {
        return Err(fail(413, "size"));
    }
    sh.with(|s| s.take_text()).map_err(map_reject)?;
    go_ahead(req, stream);
    let stop = || sh.stopped();
    let mut body = http::Body::new(lead, stream, len, sh.limits.idle, &stop);
    let raw = read_all(&mut body, len)?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    sh.with(|s| s.finish_text(Instant::now()));
    sh.emit(Event::Text(text));
    reply_json(stream, 200, "{\"ok\":true}").map_err(|_| fail(500, "io"))
}

// ---- upload ----------------------------------------------------------------------------------------------

/// Ends an upload however it ends: the half-written file goes away and the session is told.
struct UploadGuard<'a> {
    sh: &'a Shared,
    idx: usize,
    ok: bool,
    tmp: Option<PathBuf>,
}

impl Drop for UploadGuard<'_> {
    fn drop(&mut self) {
        if let Some(t) = self.tmp.take() {
            let _ = fs::remove_file(t);
        }
        let (idx, ok) = (self.idx, self.ok);
        self.sh.with(|s| s.finish_upload(idx, ok, Instant::now()));
        if !ok {
            self.sh.emit(Event::UploadFailed { index: idx });
        }
    }
}

enum SinkError {
    TooBig,
    Io(io::Error),
}

fn upload(sh: &Shared, req: &Request, lead: Vec<u8>, stream: &mut TcpStream, idx: usize) -> Result<(), Fail> {
    let boundary = req.header("content-type").and_then(multipart::boundary_from_content_type).ok_or(fail(415, "type"))?;
    let len = req.content_length.unwrap_or(0);
    let inc = sh.with(|s| s.start_upload(idx)).map_err(map_reject)?;
    let mut guard = UploadGuard { sh, idx, ok: false, tmp: None };
    // The body is the file plus a little framing: anything bigger than that is refused unread.
    if len > inc.size.saturating_add(MULTIPART_OVERHEAD) {
        return Err(fail(413, "size"));
    }
    fs::create_dir_all(&sh.recv_dir).map_err(|_| fail(500, "folder"))?;
    let (mut file, tmp) = names::create_unique(&sh.recv_dir, |n| format!("{}.mokupart", names::numbered(&inc.name, n))).map_err(disk_error)?;
    guard.tmp = Some(tmp.clone());
    go_ahead(req, stream);

    let stop = || sh.stopped();
    let mut body = http::Body::new(lead, stream, len, sh.limits.idle, &stop);
    let mut parser = multipart::Parser::new(&boundary);
    let mut buf = vec![0u8; CHUNK];
    let (mut written, mut have_file, mut writing) = (0u64, false, false);
    let mut last = Instant::now();
    loop {
        let n = body.read(&mut buf).map_err(map_read_error)?;
        if n == 0 {
            break;
        }
        parser
            .push(&buf[..n], &mut |ev| -> Result<(), SinkError> {
                match ev {
                    multipart::Event::Part(info) => {
                        writing = !have_file && info.filename.is_some();
                        have_file |= writing;
                    }
                    multipart::Event::Data(d) if writing => {
                        if written + d.len() as u64 > inc.size {
                            return Err(SinkError::TooBig);
                        }
                        file.write_all(d).map_err(SinkError::Io)?;
                        written += d.len() as u64;
                    }
                    multipart::Event::Data(_) => {}
                    multipart::Event::End => writing = false,
                }
                Ok(())
            })
            .map_err(|e| match e {
                PushError::Parse(MpError::HeadTooLarge) => fail(431, "head"),
                PushError::Parse(_) => fail(400, "multipart"),
                PushError::Sink(SinkError::TooBig) => fail(413, "size"),
                PushError::Sink(SinkError::Io(e)) => disk_error(e),
            })?;
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            sh.emit(Event::Progress { upload: true, index: idx, done: written, total: inc.size });
        }
    }
    parser.finish().map_err(|_| fail(400, "truncated"))?;
    if !have_file {
        return Err(fail(400, "no_file"));
    }
    file.flush().map_err(disk_error)?;
    drop(file);
    // Final name: the first free one. A file is never replaced.
    let mut final_path = None;
    for n in 1..=10_000u32 {
        let target = sh.recv_dir.join(names::numbered(&inc.name, n));
        match names::move_no_replace(&tmp, &target) {
            Ok(()) => {
                final_path = Some(target);
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(disk_error(e)),
        }
    }
    let path = final_path.ok_or(fail(500, "folder"))?;
    // Mark-of-the-Web: this file came over the network from another device, so Windows treats it like
    // a browser download (SmartScreen, Protected View). Not fatal: a FAT / exFAT drive has no streams.
    if let Err(e) = names::mark_from_internet(&path) {
        crate::log_warn!("phone: received file has no Mark-of-the-Web ({:?})", e.kind());
    }
    guard.tmp = None;
    guard.ok = true;
    drop(guard);
    sh.emit(Event::Progress { upload: true, index: idx, done: written, total: inc.size });
    sh.emit(Event::FileSaved { index: idx, path });
    reply_json(stream, 200, "{\"ok\":true}").map_err(|_| fail(500, "io"))
}

fn disk_error(e: io::Error) -> Fail {
    // ERROR_DISK_FULL (112) / ERROR_HANDLE_DISK_FULL (39)
    match e.raw_os_error() {
        Some(112) | Some(39) => fail(507, "disk_full"),
        _ => fail(500, "disk"),
    }
}

// ---- download -------------------------------------------------------------------------------------------

struct DownloadGuard<'a> {
    sh: &'a Shared,
    ok: bool,
}

impl Drop for DownloadGuard<'_> {
    fn drop(&mut self) {
        let ok = self.ok;
        self.sh.with(|s| s.finish_download(ok, Instant::now()));
    }
}

fn download(sh: &Shared, req: &Request, stream: &mut TcpStream, idx: usize) -> Result<(), Fail> {
    // Only a file the PC offered, only by its position in that list, only after "Terima".
    let f = sh.with(|s| s.start_download(idx)).map_err(map_reject)?;
    let mut guard = DownloadGuard { sh, ok: false };
    let mut file = File::open(&f.path).map_err(|_| fail(404, "gone"))?;
    let meta = file.metadata().map_err(|_| fail(404, "gone"))?;
    if !meta.is_file() {
        return Err(fail(404, "gone"));
    }
    let total = meta.len();
    let disposition = names::content_disposition(&f.name);
    http::write_head(stream, 200, "application/octet-stream", total, &[("Content-Disposition", disposition.as_str()), ("Accept-Ranges", "none")]).map_err(|_| fail(500, "io"))?;
    if req.method == Method::Head {
        return Ok(());
    }
    let mut buf = vec![0u8; CHUNK];
    let mut sent = 0u64;
    let mut last = Instant::now();
    while sent < total {
        if sh.stopped() {
            return Ok(());
        }
        let want = buf.len().min((total - sent) as usize);
        let n = file.read(&mut buf[..want]).map_err(|_| fail(500, "disk"))?;
        if n == 0 {
            // The file got shorter than announced: the client sees a short download, not a good one.
            return Ok(());
        }
        // A client that stops reading hits the write timeout here.
        if stream.write_all(&buf[..n]).is_err() {
            return Ok(());
        }
        sent += n as u64;
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            sh.emit(Event::Progress { upload: false, index: idx, done: sent, total });
        }
    }
    let _ = stream.flush();
    guard.ok = true;
    drop(guard);
    sh.emit(Event::Progress { upload: false, index: idx, done: total, total });
    sh.emit(Event::Downloaded { index: idx });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    use crate::features::phone_send::session::{FileStatus, SendFile};

    // ---- a test client on loopback ---------------------------------------------------------------------

    struct Resp {
        status: u16,
        head: String,
        body: Vec<u8>,
    }

    impl Resp {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.body).into_owned()
        }
        fn has(&self, header_line: &str) -> bool {
            self.head.lines().any(|l| l.eq_ignore_ascii_case(header_line))
        }
    }

    fn parse_response(raw: &[u8]) -> Resp {
        let end = raw.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4).unwrap_or(raw.len());
        let head = String::from_utf8_lossy(&raw[..end]).into_owned();
        let status = head.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        Resp { status, head, body: raw[end..].to_vec() }
    }

    /// Send `pieces` (with a pause between them), then read until the server closes.
    fn exchange(addr: SocketAddr, pieces: &[&[u8]], pause: Duration) -> Resp {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        for p in pieces {
            if s.write_all(p).is_err() {
                break;
            }
            if !pause.is_zero() {
                std::thread::sleep(pause);
            }
        }
        let mut raw = vec![];
        let _ = s.read_to_end(&mut raw);
        parse_response(&raw)
    }

    fn request(method: &str, path: &str, host: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
        let mut r = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\n").into_bytes();
        for (k, v) in headers {
            r.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
        }
        if method == "POST" {
            r.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
        }
        r.extend_from_slice(b"\r\n");
        r.extend_from_slice(body);
        r
    }

    /// Poll `cond` every 2 ms until it holds or `timeout` passes (a test never sleeps a guessed time to wait
    /// for another thread: it waits for the thing itself, with a deadline). Returns whether it held.
    fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if cond() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    struct Rig {
        server: Server,
        dir: PathBuf,
        events: mpsc::Receiver<Event>,
        token: String,
    }

    impl Rig {
        fn new(tag: &str, mode: Mode, limits: Limits) -> Rig {
            let dir = std::env::temp_dir().join(format!("moku-phone-srv-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            let server = Server::start("127.0.0.1:0".parse().unwrap(), mode, dir.join("recv"), limits, move |e| {
                let _ = tx.lock().unwrap().send(e);
            })
            .unwrap();
            let token = server.sh.with(|s| s.token().to_string());
            Rig { server, dir, events: rx, token }
        }
        fn recv(tag: &str) -> Rig {
            Rig::new(tag, Mode::Receive, Limits::default())
        }
        fn addr(&self) -> SocketAddr {
            self.server.addr()
        }
        fn host(&self) -> String {
            self.server.addr().to_string()
        }
        fn path(&self, rest: &str) -> String {
            format!("/{}/{rest}", self.token)
        }
        fn get(&self, rest: &str) -> Resp {
            exchange(self.addr(), &[&request("GET", &self.path(rest), &self.host(), &[("X-Moku", "1")], b"")], Duration::ZERO)
        }
        fn post(&self, rest: &str, headers: &[(&str, &str)], body: &[u8]) -> Resp {
            let mut h = vec![("X-Moku", "1")];
            h.extend_from_slice(headers);
            exchange(self.addr(), &[&request("POST", &self.path(rest), &self.host(), &h, body)], Duration::ZERO)
        }
        fn approve(&self) {
            self.server.sh.with(|s| s.approve());
        }
        fn events(&self) -> Vec<Event> {
            self.events.try_iter().collect()
        }
        fn pretend_ip(&self, ip: &str) {
            *self.server.sh.peer_override.lock().unwrap() = Some(ip.parse().unwrap());
        }
        fn offer(&self, files: &[(&str, u64)], text: bool) -> Resp {
            let j = serde_json::json!({ "files": files.iter().map(|(n, s)| serde_json::json!({"name": n, "size": s})).collect::<Vec<_>>(), "text": text });
            self.post("offer", &[("Content-Type", "application/json")], j.to_string().as_bytes())
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            self.server.stop();
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    const BOUNDARY: &str = "----MokuTestBoundary7MA4YWxk";

    fn multipart_body(filename: &str, data: &[u8]) -> Vec<u8> {
        let mut b = format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n").into_bytes();
        b.extend_from_slice(data);
        b.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
        b
    }

    fn content_type() -> String {
        format!("multipart/form-data; boundary={BOUNDARY}")
    }

    fn noise(n: usize, seed: u32) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                (x >> 24) as u8
            })
            .collect()
    }

    fn recv_files(r: &Rig) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(r.dir.join("recv")).map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
        v.sort();
        v
    }

    // ---- tests: what the server will and will not answer ----------------------------------------------------

    #[test]
    fn it_listens_where_it_was_told_and_nowhere_else() {
        let r = Rig::recv("bind");
        assert!(r.addr().ip().is_loopback());
        assert_ne!(r.addr().port(), 0);
        assert!(r.server.url().starts_with(&format!("http://127.0.0.1:{}/", r.addr().port())));
        assert!(r.server.url().ends_with(&format!("/{}/", r.token)));
        // Two servers get two ports and two tokens.
        let r2 = Rig::recv("bind2");
        assert_ne!(r.addr().port(), r2.addr().port());
        assert_ne!(r.token, r2.token);
    }

    #[test]
    fn the_page_needs_the_token_and_nothing_else_is_served() {
        let r = Rig::recv("page");
        let ok = r.get("");
        assert_eq!(ok.status, 200);
        assert!(ok.has("Content-Type: text/html; charset=utf-8"));
        assert!(ok.text().contains("var MODE = \"receive\""));
        for h in ["Cache-Control: no-store", "X-Content-Type-Options: nosniff", "Referrer-Policy: no-referrer", "X-Frame-Options: DENY"] {
            assert!(ok.has(h), "{h}");
        }
        assert!(!ok.head.to_lowercase().contains("\r\nserver:"), "no server banner");
        // Without the token, with a wrong one, with a prefix of it, with it in the wrong place.
        let wrong = "A".repeat(22);
        let paths = [
            "/".to_string(),
            "/index.html".to_string(),
            "/favicon.ico".to_string(),
            format!("/{wrong}/"),
            format!("/{}/", &r.token[..21]),
            format!("/{}x/", r.token),
            format!("/x/{}/", r.token),
            format!("//{}/", r.token),
            format!("/%2e%2e/{}/", r.token),
            "/..".to_string(),
            "/../../Windows/win.ini".to_string(),
            "/C:/Windows/win.ini".to_string(),
        ];
        for p in &paths {
            let resp = exchange(r.addr(), &[&request("GET", p, &r.host(), &[], b"")], Duration::ZERO);
            assert_eq!(resp.status, 404, "{p}");
            assert!(!resp.text().contains("Moku"), "{p}: no page, no hint");
        }
    }

    #[test]
    fn there_are_no_directory_listings_and_no_file_by_path() {
        let r = Rig::recv("nolist");
        r.get(""); // takes the lock
        for rest in ["f/0", "f/", "f", "f/../..", "f/0/../1", "up", "files", "files/", "dir/", "../", "..%2f", ".", "f/%2e%2e", "f/00000", "f/-1", "f/1e3", "recv/", "state/x"] {
            let resp = r.get(rest);
            assert!(matches!(resp.status, 404 | 405), "{rest}: {}", resp.status);
            assert!(!resp.text().contains("Index of"), "{rest}");
        }
        // The token without its slash goes to the page that does.
        let resp = exchange(r.addr(), &[&request("GET", &format!("/{}", r.token), &r.host(), &[], b"")], Duration::ZERO);
        assert_eq!(resp.status, 302);
        assert!(resp.has(&format!("Location: /{}/", r.token)));
    }

    #[test]
    fn a_request_for_another_host_name_is_refused() {
        let r = Rig::recv("host");
        for host in ["evil.example", "evil.example:80", "localhost", &format!("localhost:{}", r.addr().port()), "127.0.0.1", "127.0.0.2:1"] {
            let resp = exchange(r.addr(), &[&request("GET", &r.path(""), host, &[], b"")], Duration::ZERO);
            assert_eq!(resp.status, 403, "{host}");
        }
        // No Host at all.
        let resp = exchange(r.addr(), &[format!("GET {} HTTP/1.0\r\n\r\n", r.path("")).as_bytes()], Duration::ZERO);
        assert_eq!(resp.status, 403);
        // The real one, in any letter case, is fine.
        let resp = exchange(r.addr(), &[&request("GET", &r.path(""), &r.host().to_uppercase(), &[], b"")], Duration::ZERO);
        assert_eq!(resp.status, 200);
    }

    #[test]
    fn a_post_needs_our_origin_our_header_and_a_length() {
        let r = Rig::recv("post");
        let offer = br#"{"files":[{"name":"a.txt","size":3}],"text":false}"#;
        let host = r.host();
        let send = |headers: &[(&str, &str)]| exchange(r.addr(), &[&request("POST", &r.path("offer"), &host, headers, offer)], Duration::ZERO);
        assert_eq!(send(&[]).status, 403, "no X-Moku header: a cross-site form post can not add one");
        assert_eq!(send(&[("X-Moku", "1"), ("Origin", "http://evil.example")]).status, 403);
        assert_eq!(send(&[("X-Moku", "1"), ("Origin", "null")]).status, 403);
        assert_eq!(send(&[("X-Moku", "1"), ("Sec-Fetch-Site", "cross-site")]).status, 403);
        assert_eq!(send(&[("X-Moku", "yes")]).status, 403);
        // No length: 411 (a chunked body is refused earlier, at the head).
        let no_len = format!("POST {} HTTP/1.1\r\nHost: {host}\r\nX-Moku: 1\r\n\r\n", r.path("offer"));
        assert_eq!(exchange(r.addr(), &[no_len.as_bytes()], Duration::ZERO).status, 411);
        let chunked = format!("POST {} HTTP/1.1\r\nHost: {host}\r\nX-Moku: 1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n", r.path("offer"));
        assert_eq!(exchange(r.addr(), &[chunked.as_bytes()], Duration::ZERO).status, 501);
        // The good one: same origin, header, and Sec-Fetch-Site as a browser sends it.
        let origin = format!("http://{host}");
        assert_eq!(send(&[("X-Moku", "1"), ("Origin", origin.as_str()), ("Sec-Fetch-Site", "same-origin")]).status, 200);
        // Nothing but GET / HEAD / POST on the right routes.
        let resp = exchange(r.addr(), &[&request("GET", &r.path("offer"), &host, &[], b"")], Duration::ZERO);
        assert_eq!(resp.status, 405);
        let resp = exchange(r.addr(), &[&request("PUT", &r.path(""), &host, &[], b"")], Duration::ZERO);
        assert_eq!(resp.status, 405);
        let resp = exchange(r.addr(), &[&request("POST", &r.path(""), &host, &[("X-Moku", "1")], b"x")], Duration::ZERO);
        assert_eq!(resp.status, 405);
    }

    #[test]
    fn only_the_first_device_gets_in() {
        let r = Rig::recv("lock");
        r.pretend_ip("192.168.1.23");
        let first = r.get("");
        assert_eq!(first.status, 200);
        assert!(r.events().contains(&Event::Connected("192.168.1.23".parse().unwrap())));
        // Another phone with the very same URL: refused, before anything is read.
        r.pretend_ip("192.168.1.99");
        for rest in ["", "state", "f/0"] {
            let resp = r.get(rest);
            assert_eq!(resp.status, 403, "{rest}");
            assert!(resp.text().contains("device"));
        }
        // It can not even announce anything.
        assert_eq!(r.offer(&[("x.txt", 1)], false).status, 403);
        assert_eq!(r.server.sh.with(|s| s.approval()), session::Approval::None);
        // The owner is unaffected.
        r.pretend_ip("192.168.1.23");
        assert_eq!(r.get("state").status, 200);
    }

    #[test]
    fn an_ended_session_answers_gone() {
        let r = Rig::recv("ended");
        assert_eq!(r.get("").status, 200);
        r.server.sh.with(|s| s.stop());
        assert_eq!(r.get("").status, 410);
        // Only the state poll still answers, so the page can say how it ended.
        let st = r.get("state");
        assert_eq!(st.status, 200);
        assert!(st.text().contains("\"ended\""), "{}", st.text());
        assert_eq!(r.offer(&[("a", 1)], false).status, 410);
    }

    // ---- receiving --------------------------------------------------------------------------------------------

    #[test]
    fn a_file_is_received_only_after_the_user_approves() {
        let r = Rig::recv("recv");
        let data = noise(300_000, 11);
        let o = r.offer(&[("../../Windows/evil.txt", data.len() as u64), ("b.bin", 5)], false);
        assert_eq!(o.status, 200, "{}", o.text());
        assert!(o.text().contains("\"pending\""));
        let ev = r.events();
        let Some(Event::Offer(sum)) = ev.iter().find(|e| matches!(e, Event::Offer(_))) else { panic!("{ev:?}") };
        assert!(sum.needs_approval);
        assert_eq!(sum.files[0].0, "evil.txt", "the card shows the sanitised name");
        let ct = content_type();
        let body = multipart_body("whatever.name", &data);
        // Before "Terima": refused, nothing on disk.
        let early = r.post("up/0", &[("Content-Type", &ct)], &body);
        assert_eq!(early.status, 403);
        assert!(recv_files(&r).is_empty());
        r.approve();
        assert!(r.get("state").text().contains("\"allowed\""));
        let ok = r.post("up/0", &[("Content-Type", &ct)], &body);
        assert_eq!(ok.status, 200, "{}", ok.text());
        assert_eq!(recv_files(&r), ["evil.txt"], "named after the sanitised OFFER, in the receive folder, no .mokupart left");
        assert_eq!(fs::read(r.dir.join("recv").join("evil.txt")).unwrap(), data);
        let saved: Vec<PathBuf> = r.events().into_iter().filter_map(|e| if let Event::FileSaved { path, .. } = e { Some(path) } else { None }).collect();
        assert_eq!(saved, [r.dir.join("recv").join("evil.txt")]);
        // Mark-of-the-Web: a file from a phone is "from the Internet" for SmartScreen and Protected View.
        let mut stream = saved[0].as_os_str().to_os_string();
        stream.push(":Zone.Identifier");
        assert_eq!(fs::read_to_string(PathBuf::from(stream)).unwrap(), "[ZoneTransfer]
ZoneId=3
");
        // The second announced file, and the state the page polls.
        assert_eq!(r.post("up/1", &[("Content-Type", &ct)], &multipart_body("b.bin", b"12345")).status, 200);
        assert_eq!(r.get("state").text(), r#"{"files":["done","done"],"state":"allowed","text":false}"#);
        // The same index again, or one that was never announced.
        assert_eq!(r.post("up/0", &[("Content-Type", &ct)], &body).status, 409);
        assert_eq!(r.post("up/2", &[("Content-Type", &ct)], &body).status, 404);
        assert_eq!(recv_files(&r), ["b.bin", "evil.txt"]);
    }

    #[test]
    fn the_body_may_arrive_in_tiny_pieces_with_the_boundary_split() {
        let r = Rig::recv("pieces");
        let data = noise(5000, 5);
        r.offer(&[("p.bin", 5000)], false);
        r.approve();
        let body = multipart_body("p.bin", &data);
        let head = request("POST", &r.path("up/0"), &r.host(), &[("X-Moku", "1"), ("Content-Type", &content_type())], &[]);
        // The head says Content-Length: 0 (built with an empty body): rebuild it with the real length.
        let head = String::from_utf8(head).unwrap().replace("Content-Length: 0", &format!("Content-Length: {}", body.len()));
        let mut pieces: Vec<&[u8]> = vec![head.as_bytes()];
        pieces.extend(body.chunks(97));
        let resp = exchange(r.addr(), &pieces, Duration::from_micros(200));
        assert_eq!(resp.status, 200, "{}", resp.text());
        assert_eq!(fs::read(r.dir.join("recv").join("p.bin")).unwrap(), data);
    }

    #[test]
    fn a_name_that_exists_gets_a_number_and_the_old_file_is_untouched() {
        let r = Rig::recv("dup");
        fs::create_dir_all(r.dir.join("recv")).unwrap();
        fs::write(r.dir.join("recv").join("photo.jpg"), b"precious").unwrap();
        r.offer(&[("photo.jpg", 3)], false);
        r.approve();
        let resp = r.post("up/0", &[("Content-Type", &content_type())], &multipart_body("photo.jpg", b"new"));
        assert_eq!(resp.status, 200);
        assert_eq!(fs::read(r.dir.join("recv").join("photo.jpg")).unwrap(), b"precious");
        assert_eq!(fs::read(r.dir.join("recv").join("photo (2).jpg")).unwrap(), b"new");
    }

    #[test]
    fn a_truncated_upload_leaves_nothing_behind() {
        let r = Rig::recv("trunc");
        let data = noise(100_000, 3);
        r.offer(&[("t.bin", 100_000)], false);
        r.approve();
        let body = multipart_body("t.bin", &data);
        let head = request("POST", &r.path("up/0"), &r.host(), &[("X-Moku", "1"), ("Content-Type", &content_type())], &[]);
        let head = String::from_utf8(head).unwrap().replace("Content-Length: 0", &format!("Content-Length: {}", body.len()));
        // Half the body, then the phone walks out of Wi-Fi range (the connection closes).
        {
            let mut s = TcpStream::connect(r.addr()).unwrap();
            s.write_all(head.as_bytes()).unwrap();
            s.write_all(&body[..body.len() / 2]).unwrap();
        }
        // The server notices, removes the partial file and marks the file failed.
        assert!(wait_until(Duration::from_secs(5), || r.server.sh.with(|s| s.incoming()[0].status) == FileStatus::Failed), "the failure was never noticed");
        assert!(recv_files(&r).is_empty(), "no final file and no .mokupart: {:?}", recv_files(&r));
        assert!(r.events().iter().any(|e| matches!(e, Event::UploadFailed { index: 0 })));
        // The phone can try that file again.
        let resp = r.post("up/0", &[("Content-Type", &content_type())], &body);
        assert_eq!(resp.status, 200);
        assert_eq!(fs::read(r.dir.join("recv").join("t.bin")).unwrap(), data);
    }

    #[test]
    fn a_closing_boundary_that_never_comes_is_a_truncated_upload_too() {
        let r = Rig::recv("noend");
        r.offer(&[("n.bin", 10)], false);
        r.approve();
        // Complete Content-Length, but the multipart message has no closing delimiter.
        let body = format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"n.bin\"\r\n\r\n0123456789").into_bytes();
        let resp = r.post("up/0", &[("Content-Type", &content_type())], &body);
        assert_eq!(resp.status, 400);
        assert!(recv_files(&r).is_empty());
    }

    #[test]
    fn more_bytes_than_announced_are_refused() {
        let r = Rig::recv("big");
        r.offer(&[("a.bin", 1000), ("b.bin", 1000)], false);
        r.approve();
        let ct = content_type();
        // Declared length far beyond the announcement: refused without reading it.
        let resp = exchange(r.addr(), &[&request("POST", &r.path("up/0"), &r.host(), &[("X-Moku", "1"), ("Content-Type", &ct), ("Expect-Not", "x")], &vec![b'x'; 70_000])], Duration::ZERO);
        assert_eq!(resp.status, 413);
        // Within the framing allowance, but more file data than announced.
        let resp = r.post("up/1", &[("Content-Type", &ct)], &multipart_body("b.bin", &noise(1500, 1)));
        assert_eq!(resp.status, 413);
        assert!(recv_files(&r).is_empty(), "{:?}", recv_files(&r));
        // Less than announced is fine (the announcement is a cap).
        assert_eq!(r.post("up/1", &[("Content-Type", &ct)], &multipart_body("b.bin", &noise(500, 2))).status, 200);
    }

    #[test]
    fn upload_framing_errors_get_clear_answers() {
        let r = Rig::recv("frame");
        r.offer(&[("a.bin", 100)], false);
        r.approve();
        let ct = content_type();
        let good = multipart_body("a.bin", b"data");
        assert_eq!(r.post("up/0", &[], &good).status, 415, "no content type");
        assert_eq!(r.post("up/0", &[("Content-Type", "application/json")], &good).status, 415);
        assert_eq!(r.post("up/0", &[("Content-Type", "multipart/form-data")], &good).status, 415, "no boundary");
        // A part that is not a file.
        let no_file = format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"x\"\r\n\r\nhello\r\n--{BOUNDARY}--\r\n").into_bytes();
        assert_eq!(r.post("up/0", &[("Content-Type", &ct)], &no_file).status, 400);
        // Huge part headers.
        let huge = format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n\r\nx\r\n--{BOUNDARY}--\r\n", "a".repeat(20_000)).into_bytes();
        assert_eq!(r.post("up/0", &[("Content-Type", &ct)], &huge).status, 431);
        assert!(recv_files(&r).is_empty());
        // And after all that the file can still be sent properly.
        assert_eq!(r.post("up/0", &[("Content-Type", &ct)], &good).status, 200);
    }

    #[test]
    fn announcements_are_validated() {
        let r = Rig::recv("announce");
        assert_eq!(r.post("offer", &[], b"not json").status, 400);
        assert_eq!(r.post("offer", &[], br#"{"files":[{"name":"a","size":-1}]}"#).status, 400);
        assert_eq!(r.post("offer", &[], br#"{"files":[{"name":"a"}]}"#).status, 400);
        assert_eq!(r.post("offer", &[], b"{}").status, 400, "nothing offered");
        assert_eq!(r.offer(&[("a", session::MAX_FILE + 1)], false).status, 400);
        let too_many: Vec<(String, u64)> = (0..session::MAX_FILES + 1).map(|i| (format!("f{i}"), 1)).collect();
        let refs: Vec<(&str, u64)> = too_many.iter().map(|(n, s)| (n.as_str(), *s)).collect();
        assert_eq!(r.offer(&refs, false).status, 400);
        // An oversized announcement body.
        assert_eq!(r.post("offer", &[], &vec![b' '; (MAX_OFFER_BODY + 1) as usize]).status, 413);
        assert_eq!(r.server.sh.with(|s| s.approval()), session::Approval::None, "none of them opened an approval");
        assert_eq!(r.offer(&[("ok.txt", 1)], false).status, 200);
    }

    #[test]
    fn a_denied_phone_is_out_for_good() {
        let r = Rig::recv("deny");
        r.offer(&[("a.bin", 4)], false);
        r.server.sh.with(|s| s.deny());
        // The phone's page can still hear that it was declined; nothing else is served.
        let st = r.get("state");
        assert_eq!(st.status, 200);
        assert!(st.text().contains("\"denied\""), "{}", st.text());
        assert_eq!(r.get("").status, 410);
        assert_eq!(r.post("up/0", &[("Content-Type", &content_type())], &multipart_body("a.bin", b"data")).status, 410);
        assert!(recv_files(&r).is_empty());
    }

    // ---- text ------------------------------------------------------------------------------------------------

    #[test]
    fn a_text_is_delivered_once_after_approval() {
        let r = Rig::recv("text");
        let msg = "Halo dari HP \u{1F44B}\nbaris dua";
        assert_eq!(r.post("text", &[("Content-Type", "text/plain")], msg.as_bytes()).status, 403, "nothing announced, nothing approved");
        r.offer(&[], true);
        assert_eq!(r.post("text", &[("Content-Type", "text/plain")], msg.as_bytes()).status, 403, "announced but not approved");
        r.approve();
        assert_eq!(r.post("text", &[("Content-Type", "text/plain")], &vec![b'x'; session::MAX_TEXT + 1]).status, 413);
        assert_eq!(r.post("text", &[("Content-Type", "text/plain")], msg.as_bytes()).status, 200);
        assert!(r.events().contains(&Event::Text(msg.to_string())));
        assert_eq!(r.post("text", &[("Content-Type", "text/plain")], b"again").status, 409, "one text per announcement");
        // Invalid UTF-8 does not break anything: it is replaced.
        r.offer(&[], true);
        assert_eq!(r.post("text", &[], b"ok \xff\xfe").status, 200);
        assert!(r.events().iter().any(|e| matches!(e, Event::Text(t) if t.starts_with("ok "))));
    }

    // ---- sending ----------------------------------------------------------------------------------------------

    fn send_rig(tag: &str, files: &[(&str, Vec<u8>)]) -> Rig {
        let dir = std::env::temp_dir().join(format!("moku-phone-srcfiles-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let list: Vec<SendFile> = files
            .iter()
            .map(|(n, d)| {
                let p = dir.join(n);
                fs::write(&p, d).unwrap();
                SendFile { path: p, name: names::sanitize(n), size: d.len() as u64 }
            })
            .collect();
        Rig::new(tag, Mode::Send(list), Limits::default())
    }

    #[test]
    fn a_chosen_file_is_downloaded_after_approval_and_nothing_else_is() {
        let data = noise(700_000, 21);
        let r = send_rig("send", &[("laporan \u{e9}.pdf", data.clone()), ("b.txt", b"bee".to_vec())]);
        // The page lists exactly the offered files.
        let page = r.get("").text();
        assert!(page.contains("var MODE = \"send\""));
        assert!(page.contains("laporan \u{e9}.pdf") && page.contains("b.txt"));
        assert!(!page.contains(r.dir.to_string_lossy().as_ref()), "the PC's folders are not shown");
        // Before "Terima": no download.
        assert_eq!(r.get("f/0").status, 403);
        let o = r.post("offer", &[], b"{}");
        assert_eq!(o.status, 200);
        let ev = r.events();
        let Some(Event::Offer(sum)) = ev.iter().find(|e| matches!(e, Event::Offer(_))) else { panic!("{ev:?}") };
        assert_eq!(sum.files.len(), 2);
        assert_eq!(r.get("f/0").status, 403, "still waiting");
        r.approve();
        let resp = r.get("f/0");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, data);
        assert!(resp.has("Content-Type: application/octet-stream"));
        assert!(resp.has(&format!("Content-Length: {}", data.len())));
        assert!(resp.has("Content-Disposition: attachment; filename=\"laporan _.pdf\"; filename*=UTF-8''laporan%20%C3%A9.pdf"), "{}", resp.head);
        assert!(resp.has("X-Content-Type-Options: nosniff"));
        assert_eq!(r.get("f/1").body, b"bee");
        assert_eq!(r.get("f/2").status, 404, "only what was offered");
        assert!(r.events().iter().any(|e| matches!(e, Event::Downloaded { index: 0 })));
        // The session is short-lived once the download is done.
        assert!(r.server.sh.with(|s| s.remaining(Instant::now())) <= session::LINGER_SEND);
        // A HEAD gets the headers only.
        let head = exchange(r.addr(), &[&request("HEAD", &r.path("f/1"), &r.host(), &[], b"")], Duration::ZERO);
        assert_eq!((head.status, head.body.len()), (200, 0));
        assert!(head.has("Content-Length: 3"));
    }

    #[test]
    fn a_receive_session_serves_no_downloads_and_a_send_session_takes_no_uploads() {
        let r = Rig::recv("modes");
        r.offer(&[("a", 1)], false);
        r.approve();
        assert_eq!(r.get("f/0").status, 404);
        let s = send_rig("modes2", &[("a.txt", b"x".to_vec())]);
        s.post("offer", &[], b"{}");
        s.approve();
        assert_eq!(s.post("up/0", &[("Content-Type", &content_type())], &multipart_body("a.txt", b"x")).status, 404);
        assert_eq!(s.offer(&[("a", 1)], false).status, 200, "an announcement in a send session is just a download request");
        assert_eq!(s.post("text", &[], b"x").status, 404);
    }

    #[test]
    fn a_file_that_vanished_is_not_found() {
        let r = send_rig("gone", &[("g.txt", b"here".to_vec())]);
        r.post("offer", &[], b"{}");
        r.approve();
        let path = r.server.sh.with(|s| match s.mode() {
            Mode::Send(f) => f[0].path.clone(),
            Mode::Receive => unreachable!(),
        });
        fs::remove_file(path).unwrap();
        assert_eq!(r.get("f/0").status, 404);
        // The failed attempt did not leave the session thinking a transfer is still running.
        assert!(r.server.sh.with(|s| s.is_expired(Instant::now() + Duration::from_secs(700))));
    }

    // ---- limits -----------------------------------------------------------------------------------------------------

    #[test]
    fn huge_heads_and_slow_heads_are_cut_off() {
        let r = Rig::new("limits", Mode::Receive, Limits { head: Duration::from_millis(500), idle: Duration::from_millis(500), write: Duration::from_secs(5), max_conns: 8, max_per_ip: 8 });
        let mut big = format!("GET {} HTTP/1.1\r\nHost: {}\r\nX-Big: ", r.path(""), r.host()).into_bytes();
        big.extend(std::iter::repeat_n(b'a', 100_000));
        let resp = exchange(r.addr(), &[&big], Duration::ZERO);
        assert_eq!(resp.status, 431);
        // A client that sends one byte and then nothing.
        let started = Instant::now();
        let resp = exchange(r.addr(), &[b"G"], Duration::from_secs(2));
        assert!(resp.status == 408 || resp.status == 0, "{}", resp.status);
        assert!(started.elapsed() < Duration::from_secs(4), "cut off after the head deadline, not held for ever");
        // A body that stops half way.
        r.offer(&[("s.bin", 1000)], false);
        r.approve();
        let head = request("POST", &r.path("up/0"), &r.host(), &[("X-Moku", "1"), ("Content-Type", &content_type())], &[]);
        let head = String::from_utf8(head).unwrap().replace("Content-Length: 0", "Content-Length: 1200");
        let started = Instant::now();
        let mut s = TcpStream::connect(r.addr()).unwrap();
        s.write_all(head.as_bytes()).unwrap();
        s.write_all(b"--").unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut raw = vec![];
        let _ = s.read_to_end(&mut raw);
        let resp = parse_response(&raw);
        assert_eq!(resp.status, 408);
        assert!(started.elapsed() < Duration::from_secs(4));
        assert!(recv_files(&r).is_empty());
    }

    #[test]
    fn connections_beyond_the_cap_are_turned_away() {
        let r = Rig::new("cap", Mode::Receive, Limits { head: Duration::from_secs(3), idle: Duration::from_secs(3), write: Duration::from_secs(3), max_conns: 3, max_per_ip: 3 });
        // Three connections that say nothing hold the three slots ...
        let holders: Vec<TcpStream> = (0..3).map(|_| TcpStream::connect(r.addr()).unwrap()).collect();
        let slots = |r: &Rig| r.server.sh.slots.lock().unwrap_or_else(PoisonError::into_inner).total;
        assert!(wait_until(Duration::from_secs(5), || slots(&r) == 3), "the server took the three connections ({})", slots(&r));
        // ... the fourth is refused at once.
        let resp = exchange(r.addr(), &[&request("GET", &r.path(""), &r.host(), &[], b"")], Duration::ZERO);
        assert_eq!(resp.status, 503);
        drop(holders);
        assert!(wait_until(Duration::from_secs(5), || slots(&r) == 0), "the slots were given back ({})", slots(&r));
        assert_eq!(r.get("").status, 200, "slots are given back");
    }

    #[test]
    fn one_address_can_not_take_every_slot() {
        let r = Rig::new("perip", Mode::Receive, Limits { head: Duration::from_secs(3), idle: Duration::from_secs(3), write: Duration::from_secs(3), max_conns: 8, max_per_ip: 2 });
        // A flood from one address: two idle connections hold its two slots, the third is turned away.
        r.pretend_ip("10.0.0.5");
        let holders: Vec<TcpStream> = (0..2).map(|_| TcpStream::connect(r.addr()).unwrap()).collect();
        let slots = |r: &Rig| r.server.sh.slots.lock().unwrap_or_else(PoisonError::into_inner).total;
        assert!(wait_until(Duration::from_secs(5), || slots(&r) == 2), "the server took the two connections ({})", slots(&r));
        let resp = exchange(r.addr(), &[&request("GET", &r.path(""), &r.host(), &[], b"")], Duration::ZERO);
        assert_eq!(resp.status, 503);
        // Everybody else still gets through while the flood goes on.
        r.pretend_ip("10.0.0.6");
        assert_eq!(r.get("").status, 200);
        drop(holders);
    }

    #[test]
    fn stopping_closes_the_door_even_with_an_idle_connection_open() {
        let mut r = Rig::recv("stop");
        let addr = r.addr();
        let _idle = TcpStream::connect(addr).unwrap();
        let started = Instant::now();
        r.server.stop();
        assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
        assert!(TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_err(), "the port is closed");
    }
}
