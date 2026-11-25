//! A deliberately small HTTP/1.1 request reader and response writer, with hard limits.
//!
//! The phone page is the only client, so only what it needs is accepted: `GET`, `HEAD` and `POST`
//! with a `Content-Length` body; one request per connection (`Connection: close`). Everything else
//! is refused, and the limits are explicit:
//!
//! * the request line plus headers are at most [`MAX_HEAD`] bytes, at most [`MAX_HEADERS`] headers,
//!   the target at most [`MAX_TARGET`] bytes, a header value at most [`MAX_HEADER_VALUE`];
//! * the whole head must arrive before a deadline (a client that trickles bytes is cut off);
//! * line ends are CRLF only, header names are tokens, no obsolete line folding, no `Transfer-Encoding`
//!   (chunked bodies are the classic request-smuggling door), `Content-Length` is digits only and may
//!   not be repeated with different values;
//! * a body is read through [`Body`]: exactly `Content-Length` bytes, a truncated body is an error and a
//!   connection that stops sending is cut off after an idle limit.
//!
//! No `Server` header, no directory listings, no file serving by path: routing is done elsewhere.

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

/// Request line + headers, including the final blank line.
pub const MAX_HEAD: usize = 8 * 1024;
pub const MAX_HEADERS: usize = 32;
pub const MAX_TARGET: usize = 1024;
pub const MAX_HEADER_NAME: usize = 64;
pub const MAX_HEADER_VALUE: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Post,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub method: Method,
    /// The target without its query. Never decoded: the router matches literal segments only.
    pub path: String,
    pub query: String,
    pub http11: bool,
    /// Lower-case names.
    headers: Vec<(String, String)>,
    pub content_length: Option<u64>,
}

impl Request {
    /// The value of a header (`name` in lower case).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HttpError {
    /// The peer closed before a whole head arrived (an empty connection is normal: browsers open spare ones).
    Closed,
    /// Too slow.
    Timeout,
    /// 431.
    HeadTooLarge,
    /// 400.
    Malformed(&'static str),
    /// 405.
    Method,
    /// 501.
    Unsupported(&'static str),
    Io(io::ErrorKind),
}

impl HttpError {
    /// The status to answer with.
    pub fn status(&self) -> u16 {
        match self {
            HttpError::Closed | HttpError::Io(_) => 400,
            HttpError::Timeout => 408,
            HttpError::HeadTooLarge => 431,
            HttpError::Malformed(_) => 400,
            HttpError::Method => 405,
            HttpError::Unsupported(_) => 501,
        }
    }
}

fn is_token(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Read one request head. Returns the parsed request and the bytes already read that belong to the
/// body. `r` should have a short read timeout (a second or so): `WouldBlock` / `TimedOut` just make
/// the loop look at `deadline` again.
pub fn read_head<R: Read>(r: &mut R, deadline: Instant) -> Result<(Request, Vec<u8>), HttpError> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut tmp = [0u8; 2048];
    loop {
        if let Some(p) = find_head_end(&buf) {
            if p + 4 > MAX_HEAD {
                return Err(HttpError::HeadTooLarge);
            }
            let req = parse_head(&buf[..p])?;
            return Ok((req, buf[p + 4..].to_vec()));
        }
        // No end yet: the head is already too long, or the next read would make it so.
        if buf.len() >= MAX_HEAD {
            return Err(HttpError::HeadTooLarge);
        }
        if Instant::now() >= deadline {
            return Err(HttpError::Timeout);
        }
        // Never read more than the head may still take (+3 bytes: the terminator can straddle).
        let room = (MAX_HEAD + 4 - buf.len()).min(tmp.len());
        match r.read(&mut tmp[..room]) {
            Ok(0) => return Err(HttpError::Closed),
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => continue,
            Err(e) => return Err(HttpError::Io(e.kind())),
        }
    }
}

fn parse_head(head: &[u8]) -> Result<Request, HttpError> {
    // Only CRLF ends a line: a bare CR or LF anywhere is an attempt to confuse a parser.
    let mut lines: Vec<&[u8]> = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < head.len() {
        match head[i] {
            b'\r' if head.get(i + 1) == Some(&b'\n') => {
                lines.push(&head[start..i]);
                i += 2;
                start = i;
                continue;
            }
            b'\r' | b'\n' => return Err(HttpError::Malformed("bare CR or LF")),
            _ => {}
        }
        i += 1;
    }
    lines.push(&head[start..]);
    let mut it = lines.into_iter();
    let line = it.next().unwrap_or_default();
    let (method, target, version) = {
        let mut parts = line.split(|&b| b == b' ');
        let (m, t, v) = (parts.next(), parts.next(), parts.next());
        if parts.next().is_some() {
            return Err(HttpError::Malformed("request line"));
        }
        match (m, t, v) {
            (Some(m), Some(t), Some(v)) if !m.is_empty() && !t.is_empty() => (m, t, v),
            _ => return Err(HttpError::Malformed("request line")),
        }
    };
    let method = match method {
        b"GET" => Method::Get,
        b"HEAD" => Method::Head,
        b"POST" => Method::Post,
        m if m.iter().all(|&b| is_token(b)) => return Err(HttpError::Method),
        _ => return Err(HttpError::Malformed("method")),
    };
    let http11 = match version {
        b"HTTP/1.1" => true,
        b"HTTP/1.0" => false,
        _ => return Err(HttpError::Malformed("version")),
    };
    if target.len() > MAX_TARGET {
        return Err(HttpError::HeadTooLarge);
    }
    if target[0] != b'/' || target.iter().any(|&b| b <= b' ' || b == 0x7f || b == b'#' || b >= 0x80) {
        return Err(HttpError::Malformed("target"));
    }
    let target = std::str::from_utf8(target).map_err(|_| HttpError::Malformed("target"))?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut content_length: Option<u64> = None;
    for line in it {
        if headers.len() >= MAX_HEADERS {
            return Err(HttpError::HeadTooLarge);
        }
        // Obsolete line folding (a line starting with a space) is refused.
        let colon = line.iter().position(|&b| b == b':').ok_or(HttpError::Malformed("header without colon"))?;
        let (name, value) = (&line[..colon], &line[colon + 1..]);
        if name.is_empty() || !name.iter().all(|&b| is_token(b)) {
            return Err(HttpError::Malformed("header name"));
        }
        if name.len() > MAX_HEADER_NAME || value.len() > MAX_HEADER_VALUE {
            return Err(HttpError::HeadTooLarge);
        }
        if value.iter().any(|&b| (b < b' ' && b != b'\t') || b == 0x7f) {
            return Err(HttpError::Malformed("header value"));
        }
        let name = String::from_utf8_lossy(name).to_ascii_lowercase();
        let value = String::from_utf8_lossy(value).trim_matches([' ', '\t']).to_string();
        match name.as_str() {
            "transfer-encoding" => return Err(HttpError::Unsupported("transfer-encoding")),
            "content-length" => {
                if value.is_empty() || value.len() > 19 || !value.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(HttpError::Malformed("content-length"));
                }
                let n: u64 = value.parse().map_err(|_| HttpError::Malformed("content-length"))?;
                if content_length.is_some_and(|c| c != n) {
                    return Err(HttpError::Malformed("conflicting content-length"));
                }
                content_length = Some(n);
            }
            "host" if headers.iter().any(|(n, _)| n == "host") => return Err(HttpError::Malformed("two hosts")),
            _ => {}
        }
        headers.push((name, value));
    }
    if http11 && !headers.iter().any(|(n, _)| n == "host") {
        return Err(HttpError::Malformed("no host"));
    }
    Ok(Request { method, path: path.to_string(), query: query.to_string(), http11, headers, content_length })
}

/// A request body: `lead` (bytes read together with the head) followed by the rest of the stream,
/// exactly `len` bytes. A connection that closes early is an error (`UnexpectedEof`), one that goes
/// quiet for `idle` is an error (`TimedOut`), and so is `stop()` turning true.
pub struct Body<'a, R: Read> {
    lead: Vec<u8>,
    lead_pos: usize,
    inner: &'a mut R,
    remaining: u64,
    idle: Duration,
    stop: &'a dyn Fn() -> bool,
}

impl<'a, R: Read> Body<'a, R> {
    pub fn new(lead: Vec<u8>, inner: &'a mut R, len: u64, idle: Duration, stop: &'a dyn Fn() -> bool) -> Body<'a, R> {
        // Bytes beyond the announced length (a pipelined next request) are not ours.
        let mut lead = lead;
        lead.truncate(len.min(usize::MAX as u64) as usize);
        Body { lead, lead_pos: 0, inner, remaining: len, idle, stop }
    }

    pub fn remaining(&self) -> u64 {
        self.remaining
    }
}

impl<R: Read> Read for Body<'_, R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 || out.is_empty() {
            return Ok(0);
        }
        let want = out.len().min(self.remaining.min(usize::MAX as u64) as usize);
        if self.lead_pos < self.lead.len() {
            let n = want.min(self.lead.len() - self.lead_pos);
            out[..n].copy_from_slice(&self.lead[self.lead_pos..self.lead_pos + n]);
            self.lead_pos += n;
            self.remaining -= n as u64;
            return Ok(n);
        }
        let quiet_since = Instant::now();
        loop {
            match self.inner.read(&mut out[..want]) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "truncated body")),
                Ok(n) => {
                    self.remaining -= n as u64;
                    return Ok(n);
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                    if (self.stop)() || quiet_since.elapsed() >= self.idle {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "idle"));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }
}

// ---- responses ------------------------------------------------------------------------------------------

pub fn reason(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        200 => "OK",
        204 => "No Content",
        302 => "Found",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        507 => "Insufficient Storage",
        _ => "Error",
    }
}

/// The response head. Every response carries the same defensive headers: nothing cached, no type
/// sniffing, no referrer, never framed, and a content policy that lets the page run its own inline
/// script and talk to its own origin only.
pub fn write_head(w: &mut impl Write, status: u16, content_type: &str, content_length: u64, extra: &[(&str, &str)]) -> io::Result<()> {
    let mut h = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {content_length}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\nCross-Origin-Resource-Policy: same-origin\r\nContent-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'\r\n",
        reason(status)
    );
    for (n, v) in extra {
        // Header values come from our own code, but a stray line break must never become a header.
        debug_assert!(!n.contains(['\r', '\n']) && !v.contains(['\r', '\n']));
        h.push_str(&format!("{n}: {}\r\n", v.replace(['\r', '\n'], " ")));
    }
    h.push_str("\r\n");
    w.write_all(h.as_bytes())
}

/// A small complete response.
pub fn write_simple(w: &mut impl Write, status: u16, content_type: &str, body: &[u8], extra: &[(&str, &str)]) -> io::Result<()> {
    write_head(w, status, content_type, body.len() as u64, extra)?;
    w.write_all(body)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hands out `data` in pieces of at most `chunk` bytes, then EOF.
    struct Chunked<'a> {
        data: &'a [u8],
        chunk: usize,
    }
    impl Read for Chunked<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let n = self.chunk.min(out.len()).min(self.data.len());
            out[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }

    /// Always "no data yet".
    struct Quiet;
    impl Read for Quiet {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            std::thread::sleep(Duration::from_millis(2));
            Err(io::ErrorKind::TimedOut.into())
        }
    }

    /// One byte every few ms (a slowloris).
    struct Trickle {
        n: usize,
    }
    impl Read for Trickle {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            std::thread::sleep(Duration::from_millis(3));
            self.n += 1;
            out[0] = if self.n % 2 == 0 { b'a' } else { b'b' };
            Ok(1)
        }
    }

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }
    fn head(data: &[u8]) -> Result<(Request, Vec<u8>), HttpError> {
        read_head(&mut Chunked { data, chunk: usize::MAX }, soon())
    }
    fn get(path: &str) -> Vec<u8> {
        format!("GET {path} HTTP/1.1\r\nHost: 192.168.1.5:50000\r\n\r\n").into_bytes()
    }

    const CHROME: &[u8] = b"GET /abc/ HTTP/1.1\r\nHost: 192.168.1.5:50000\r\nConnection: keep-alive\r\nUpgrade-Insecure-Requests: 1\r\nUser-Agent: Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Mobile Safari/537.36\r\nAccept: text/html,application/xhtml+xml\r\nAccept-Encoding: gzip, deflate\r\nAccept-Language: id-ID,id;q=0.9\r\n\r\n";

    #[test]
    fn a_browser_get_parses() {
        let (r, lead) = head(CHROME).unwrap();
        assert_eq!((r.method, r.path.as_str(), r.query.as_str(), r.http11), (Method::Get, "/abc/", "", true));
        assert_eq!(r.header("host"), Some("192.168.1.5:50000"));
        assert!(r.header("user-agent").unwrap().contains("Pixel 8"));
        assert_eq!(r.content_length, None);
        assert!(lead.is_empty());
    }

    #[test]
    fn a_post_with_a_body_start_returns_the_lead() {
        let raw = b"POST /tok/up/0 HTTP/1.1\r\nHost: h\r\nContent-Type: multipart/form-data; boundary=xyz\r\nContent-Length: 11\r\n\r\nhello world and more";
        let (r, lead) = head(raw).unwrap();
        assert_eq!((r.method, r.content_length), (Method::Post, Some(11)));
        assert_eq!(r.header("content-type"), Some("multipart/form-data; boundary=xyz"));
        assert_eq!(lead, b"hello world and more");
        // Names are case-insensitive, values are trimmed.
        let (r, _) = head(b"GET / HTTP/1.1\r\nHOST:\t x  \r\nX-A:y\r\n\r\n").unwrap();
        assert_eq!((r.header("host"), r.header("x-a")), (Some("x"), Some("y")));
    }

    #[test]
    fn the_query_is_split_off_and_never_decoded() {
        let (r, _) = head(&get("/a/b?x=1&y=%2e%2e")).unwrap();
        assert_eq!((r.path.as_str(), r.query.as_str()), ("/a/b", "x=1&y=%2e%2e"));
        let (r, _) = head(&get("/a%2f..%2fb")).unwrap();
        assert_eq!(r.path, "/a%2f..%2fb", "the router sees the literal text and finds no such route");
    }

    #[test]
    fn the_head_is_the_same_however_the_bytes_arrive() {
        let want = head(CHROME).unwrap();
        for chunk in [1, 2, 3, 5, 7, 16, 64, 1000] {
            let got = read_head(&mut Chunked { data: CHROME, chunk }, soon()).unwrap();
            assert_eq!(got, want, "chunk {chunk}");
        }
        // The blank line itself split across reads (\r | \n\r | \n).
        let raw = b"GET / HTTP/1.0\r\nX: y\r\n\r\nBODY";
        for chunk in 1..=raw.len() {
            let mut src = Chunked { data: raw, chunk };
            let (r, lead) = read_head(&mut src, soon()).unwrap();
            assert_eq!((r.path.as_str(), r.http11), ("/", false), "chunk {chunk}");
            // What came with the head plus what is still in the stream is exactly the body.
            let mut body = lead;
            body.extend_from_slice(src.data);
            assert_eq!(body, b"BODY", "chunk {chunk}");
        }
    }

    #[test]
    fn a_huge_head_is_refused_without_reading_it_all() {
        // No end of head, just a flood of header bytes.
        let mut flood = b"GET / HTTP/1.1\r\nHost: h\r\nX-Big: ".to_vec();
        flood.extend(std::iter::repeat_n(b'a', 100_000));
        let mut r = Chunked { data: &flood, chunk: 500 };
        assert_eq!(read_head(&mut r, soon()).unwrap_err(), HttpError::HeadTooLarge);
        assert!(r.data.len() > 90_000, "stopped reading near the limit, not after 100 KB ({} left)", r.data.len());
        // A head that ends, but only after MAX_HEAD bytes.
        let mut big = b"GET / HTTP/1.1\r\nHost: h\r\n".to_vec();
        while big.len() < MAX_HEAD {
            big.extend_from_slice(b"X-Pad: 0123456789\r\n");
        }
        big.extend_from_slice(b"\r\n");
        assert_eq!(head(&big).unwrap_err(), HttpError::HeadTooLarge);
        // One header value over its own limit, in an otherwise small head.
        let raw = format!("GET / HTTP/1.1\r\nHost: h\r\nX: {}\r\n\r\n", "v".repeat(MAX_HEADER_VALUE + 1));
        assert_eq!(head(raw.as_bytes()).unwrap_err(), HttpError::HeadTooLarge);
        // Too many headers.
        let mut many = String::from("GET / HTTP/1.1\r\nHost: h\r\n");
        for i in 0..MAX_HEADERS {
            many.push_str(&format!("X-{i}: v\r\n"));
        }
        many.push_str("\r\n");
        assert_eq!(head(many.as_bytes()).unwrap_err(), HttpError::HeadTooLarge);
        // A target over its limit.
        let long = format!("/{}", "a".repeat(MAX_TARGET));
        assert_eq!(head(&get(&long)).unwrap_err(), HttpError::HeadTooLarge);
        // Just inside the limits is fine.
        let ok = format!("/{}", "a".repeat(MAX_TARGET - 1));
        assert!(head(&get(&ok)).is_ok());
        let mut just = String::from("GET / HTTP/1.1\r\nHost: h\r\n");
        for i in 0..MAX_HEADERS - 1 {
            just.push_str(&format!("X-{i}: v\r\n"));
        }
        just.push_str("\r\n");
        assert!(head(just.as_bytes()).is_ok());
    }

    #[test]
    fn a_truncated_head_is_closed_not_parsed() {
        for cut in [0, 1, 10, CHROME.len() / 2, CHROME.len() - 2, CHROME.len() - 1] {
            assert_eq!(head(&CHROME[..cut]).unwrap_err(), HttpError::Closed, "cut at {cut}");
        }
    }

    #[test]
    fn slow_clients_time_out() {
        // Never sends anything.
        let started = Instant::now();
        assert_eq!(read_head(&mut Quiet, Instant::now() + Duration::from_millis(60)).unwrap_err(), HttpError::Timeout);
        assert!(started.elapsed() < Duration::from_secs(2));
        // Trickles bytes forever: still cut off at the deadline, long before the size limit.
        let started = Instant::now();
        assert_eq!(read_head(&mut Trickle { n: 0 }, Instant::now() + Duration::from_millis(100)).unwrap_err(), HttpError::Timeout);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn malformed_requests_are_refused() {
        let cases: &[(&str, &[u8])] = &[
            ("no version", b"GET /\r\nHost: h\r\n\r\n"),
            ("two spaces", b"GET  / HTTP/1.1\r\nHost: h\r\n\r\n"),
            ("extra token", b"GET / HTTP/1.1 x\r\nHost: h\r\n\r\n"),
            ("absolute form", b"GET http://h/ HTTP/1.1\r\nHost: h\r\n\r\n"),
            ("no leading slash", b"GET abc HTTP/1.1\r\nHost: h\r\n\r\n"),
            ("asterisk", b"GET * HTTP/1.1\r\nHost: h\r\n\r\n"),
            ("nul in target", b"GET /a\0b HTTP/1.1\r\nHost: h\r\n\r\n"),
            ("fragment", b"GET /a#b HTTP/1.1\r\nHost: h\r\n\r\n"),
            ("high byte in target", b"GET /\xC3\xA9 HTTP/1.1\r\nHost: h\r\n\r\n"),
            ("http 2", b"GET / HTTP/2.0\r\nHost: h\r\n\r\n"),
            ("http 0.9", b"GET / HTTP/0.9\r\nHost: h\r\n\r\n"),
            ("no host on 1.1", b"GET / HTTP/1.1\r\nX: y\r\n\r\n"),
            ("two hosts", b"GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n"),
            ("bare LF", b"GET / HTTP/1.1\nHost: h\n\n\r\n\r\n"),
            ("bare CR in a value", b"GET / HTTP/1.1\r\nHost: h\rX: y\r\n\r\n"),
            ("no colon", b"GET / HTTP/1.1\r\nHost: h\r\nnonsense\r\n\r\n"),
            ("space before colon", b"GET / HTTP/1.1\r\nHost : h\r\n\r\n"),
            ("line folding", b"GET / HTTP/1.1\r\nHost: h\r\n continued\r\n\r\n"),
            ("empty name", b"GET / HTTP/1.1\r\nHost: h\r\n: v\r\n\r\n"),
            ("control in value", b"GET / HTTP/1.1\r\nHost: h\r\nX: a\x01b\r\n\r\n"),
            ("nul in value", b"GET / HTTP/1.1\r\nHost: h\r\nX: a\0b\r\n\r\n"),
        ];
        for (what, raw) in cases {
            let e = head(raw).unwrap_err();
            assert!(matches!(e, HttpError::Malformed(_)), "{what}: {e:?}");
            assert_eq!(e.status(), 400);
        }
        // HTTP/1.0 needs no Host.
        assert!(head(b"GET / HTTP/1.0\r\n\r\n").is_ok());
    }

    #[test]
    fn only_get_head_and_post_are_served() {
        for m in ["PUT", "DELETE", "OPTIONS", "TRACE", "CONNECT", "PATCH", "PROPFIND", "get", "Post"] {
            let raw = format!("{m} / HTTP/1.1\r\nHost: h\r\n\r\n");
            let e = head(raw.as_bytes()).unwrap_err();
            assert_eq!((e.clone(), e.status()), (HttpError::Method, 405), "{m}");
        }
        for m in ["GET", "HEAD", "POST"] {
            assert!(head(format!("{m} / HTTP/1.1\r\nHost: h\r\n\r\n").as_bytes()).is_ok(), "{m}");
        }
    }

    #[test]
    fn content_length_is_strict_and_chunked_is_refused() {
        let with = |v: &str| head(format!("POST / HTTP/1.1\r\nHost: h\r\nContent-Length: {v}\r\n\r\n").as_bytes());
        assert_eq!(with("0").unwrap().0.content_length, Some(0));
        assert_eq!(with(" 42 ").unwrap().0.content_length, Some(42));
        assert_eq!(with("2147483648").unwrap().0.content_length, Some(2_147_483_648));
        for bad in ["", "+5", "-1", "1e3", "0x10", "5 5", "5,5", "99999999999999999999", "１２", "five"] {
            assert!(matches!(with(bad).unwrap_err(), HttpError::Malformed(_)), "{bad:?}");
        }
        // The same length twice is harmless; two different ones are a smuggling attempt.
        assert!(head(b"POST / HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\nContent-Length: 5\r\n\r\n").is_ok());
        assert!(matches!(head(b"POST / HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\nContent-Length: 6\r\n\r\n").unwrap_err(), HttpError::Malformed(_)));
        // Transfer-Encoding is never accepted, alone or next to Content-Length.
        let e = head(b"POST / HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n").unwrap_err();
        assert_eq!((e.clone(), e.status()), (HttpError::Unsupported("transfer-encoding"), 501));
        assert!(matches!(head(b"POST / HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap_err(), HttpError::Unsupported(_)));
        assert!(matches!(head(b"POST / HTTP/1.1\r\nHost: h\r\ntransfer-encoding : chunked\r\n\r\n").unwrap_err(), HttpError::Malformed(_)));
    }

    // ---- body -------------------------------------------------------------------------------------

    fn read_all(b: &mut impl Read) -> io::Result<Vec<u8>> {
        let mut out = vec![];
        let mut buf = [0u8; 7];
        loop {
            match b.read(&mut buf)? {
                0 => return Ok(out),
                n => out.extend_from_slice(&buf[..n]),
            }
        }
    }

    #[test]
    fn the_body_is_exactly_content_length_bytes() {
        let never = || false;
        // Lead plus stream; bytes after the length (a pipelined request) are not ours.
        let mut s = Chunked { data: b"456789EXTRA-NEXT-REQUEST", chunk: 3 };
        let mut b = Body::new(b"123".to_vec(), &mut s, 9, Duration::from_secs(1), &never);
        assert_eq!(read_all(&mut b).unwrap(), b"123456789");
        assert_eq!(b.remaining(), 0);
        // The lead alone can cover it, and is cut at the length.
        let mut s = Chunked { data: b"", chunk: 3 };
        let mut b = Body::new(b"abcdefgh".to_vec(), &mut s, 5, Duration::from_secs(1), &never);
        assert_eq!(read_all(&mut b).unwrap(), b"abcde");
        // Zero length: nothing to read.
        let mut s = Chunked { data: b"zzz", chunk: 3 };
        let mut b = Body::new(vec![], &mut s, 0, Duration::from_secs(1), &never);
        assert_eq!(read_all(&mut b).unwrap(), b"");
    }

    #[test]
    fn a_truncated_body_is_an_error() {
        let never = || false;
        let mut s = Chunked { data: b"only-this", chunk: 4 };
        let mut b = Body::new(b"xx".to_vec(), &mut s, 100, Duration::from_secs(1), &never);
        let e = read_all(&mut b).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn a_body_that_goes_quiet_times_out_and_stop_ends_it() {
        let never = || false;
        let started = Instant::now();
        let mut q = Quiet;
        let mut b = Body::new(vec![], &mut q, 10, Duration::from_millis(40), &never);
        assert_eq!(read_all(&mut b).unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
        let stop = || true;
        let mut q = Quiet;
        let mut b = Body::new(vec![], &mut q, 10, Duration::from_secs(60), &stop);
        assert_eq!(read_all(&mut b).unwrap_err().kind(), io::ErrorKind::TimedOut, "a stopped server does not wait out the idle limit");
    }

    // ---- responses ----------------------------------------------------------------------------------

    #[test]
    fn responses_carry_the_defensive_headers_and_no_server_banner() {
        let mut out = vec![];
        write_simple(&mut out, 404, "text/plain; charset=utf-8", b"nope", &[]).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("HTTP/1.1 404 Not Found\r\n"));
        for h in ["Content-Length: 4\r\n", "Connection: close\r\n", "Cache-Control: no-store\r\n", "X-Content-Type-Options: nosniff\r\n", "Referrer-Policy: no-referrer\r\n", "X-Frame-Options: DENY\r\n", "Content-Security-Policy: default-src 'none'"] {
            assert!(s.contains(h), "{h}");
        }
        assert!(!s.to_lowercase().contains("server:"));
        assert!(s.ends_with("\r\n\r\nnope"));
        // Extra headers cannot smuggle a line break.
        let mut out = vec![];
        write_head(&mut out, 200, "text/plain", 0, &[("X-A", "b")]).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("X-A: b\r\n"));
    }
}
