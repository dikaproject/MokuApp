//! A streaming `multipart/form-data` parser (RFC 7578 / 2046), fed chunk by chunk.
//!
//! Nothing is buffered beyond one chunk plus the length of the delimiter: file data goes out through
//! [`Event::Data`] as soon as it is known not to be the start of the next delimiter, so a 2 GB
//! upload costs the same memory as a 2 KB one. The delimiter may be split across any number of
//! reads; the tests cut the same body at every position.
//!
//! Limits: a part's header block is at most [`MAX_PART_HEAD`] bytes, a request has at most
//! [`MAX_PARTS`] parts, junk before the first boundary is at most [`MAX_PREAMBLE`] bytes. A body that
//! ends before the closing delimiter is [`MpError::Truncated`] ([`Parser::finish`]): the caller deletes
//! the half-written file.

/// Header block of one part (name, filename, type).
pub const MAX_PART_HEAD: usize = 8 * 1024;
pub const MAX_PARTS: usize = 32;
/// Bytes tolerated before the first boundary.
pub const MAX_PREAMBLE: usize = 64 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PartInfo {
    /// The form field (`name="file"`).
    pub name: Option<String>,
    /// The client's file name. Never used as a path: callers sanitise it.
    pub filename: Option<String>,
    pub content_type: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Event<'a> {
    /// A part begins.
    Part(PartInfo),
    /// Bytes of the current part (never empty).
    Data(&'a [u8]),
    /// The current part ended.
    End,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MpError {
    HeadTooLarge,
    TooManyParts,
    Malformed(&'static str),
    /// The body ended before the closing delimiter.
    Truncated,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PushError<E> {
    Parse(MpError),
    /// The sink (the caller's handler) refused: disk full, size cap ...
    Sink(E),
}

/// The `boundary` of a `Content-Type: multipart/form-data; boundary=...` value (1-70 characters
/// of the RFC 2046 set, without spaces).
pub fn boundary_from_content_type(ct: &str) -> Option<String> {
    let mut parts = split_params(ct).into_iter();
    let first = parts.next()?;
    if !first.trim().eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    for p in parts {
        let Some((k, v)) = p.split_once('=') else { continue };
        if k.trim().eq_ignore_ascii_case("boundary") {
            let v = unquote(v.trim());
            let ok = !v.is_empty() && v.len() <= 70 && v.bytes().all(|b| b.is_ascii_alphanumeric() || b"'()+_,-./:=?".contains(&b));
            return ok.then_some(v);
        }
    }
    None
}

/// Split a header value at `;` outside double quotes.
fn split_params(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for c in s.chars() {
        if quoted {
            cur.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
        } else if c == '"' {
            quoted = true;
            cur.push(c);
        } else if c == ';' {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

/// `"a \"b\""` -> `a "b"`; an unquoted value is returned as is.
fn unquote(v: &str) -> String {
    let Some(inner) = v.strip_prefix('"') else { return v.to_string() };
    let mut out = String::new();
    let mut escaped = false;
    for c in inner.chars() {
        if escaped {
            out.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            break;
        } else {
            out.push(c);
        }
    }
    out
}

/// `UTF-8''%E6%97%A5.txt` (RFC 5987) -> the decoded name.
fn decode_ext_value(v: &str) -> Option<String> {
    let rest = v.strip_prefix("UTF-8''").or_else(|| v.strip_prefix("utf-8''"))?;
    let b = rest.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            // Two hex digits must follow.
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn parse_part_head(block: &[u8]) -> Result<PartInfo, MpError> {
    let mut info = PartInfo::default();
    let mut star: Option<String> = None;
    if block.is_empty() {
        return Ok(info);
    }
    for line in block.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            return Err(MpError::Malformed("blank line in part headers"));
        }
        let text = String::from_utf8_lossy(line);
        let (name, value) = text.split_once(':').ok_or(MpError::Malformed("part header without colon"))?;
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-disposition" => {
                for p in split_params(value).into_iter().skip(1) {
                    let Some((k, v)) = p.split_once('=') else { continue };
                    match k.trim().to_ascii_lowercase().as_str() {
                        "name" => info.name = Some(unquote(v.trim())),
                        "filename" => info.filename = Some(unquote(v.trim())),
                        "filename*" => star = decode_ext_value(&unquote(v.trim())),
                        _ => {}
                    }
                }
            }
            "content-type" => info.content_type = Some(value.to_string()),
            _ => {}
        }
    }
    if star.is_some() {
        info.filename = star;
    }
    Ok(info)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Before the first delimiter (anything there is ignored).
    Preamble,
    /// Right after a delimiter: `--` (the end) or optional blanks and CRLF (another part).
    AfterDelim,
    Headers,
    Body,
    /// Closing delimiter seen; whatever follows (the epilogue) is ignored.
    Done,
}

pub struct Parser {
    /// CRLF `--` boundary
    delim: Vec<u8>,
    buf: Vec<u8>,
    state: State,
    parts: usize,
    preamble: usize,
}

/// First position of `needle` in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    let first = *needle.first()?;
    let last_start = hay.len().checked_sub(needle.len())?;
    let mut i = 0;
    while i <= last_start {
        // Jump to the next candidate first byte.
        i += hay[i..=last_start].iter().position(|&b| b == first)?;
        if &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

impl Parser {
    pub fn new(boundary: &str) -> Parser {
        let mut delim = b"\r\n--".to_vec();
        delim.extend_from_slice(boundary.as_bytes());
        // The first delimiter has no CRLF before it unless there is a preamble: pretend there is one.
        Parser { delim, buf: b"\r\n".to_vec(), state: State::Preamble, parts: 0, preamble: 0 }
    }

    /// Bytes held back right now (at most a chunk plus the delimiter length).
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// Feed the next bytes of the body.
    pub fn push<E>(&mut self, chunk: &[u8], sink: &mut impl FnMut(Event<'_>) -> Result<(), E>) -> Result<(), PushError<E>> {
        if self.state == State::Done {
            return Ok(());
        }
        self.buf.extend_from_slice(chunk);
        loop {
            match self.state {
                State::Done => {
                    self.buf.clear();
                    return Ok(());
                }
                State::Preamble => match find(&self.buf, &self.delim) {
                    Some(i) => {
                        self.buf.drain(..i + self.delim.len());
                        self.state = State::AfterDelim;
                    }
                    None => {
                        let keep = self.delim.len() - 1;
                        if self.buf.len() > keep {
                            let drop = self.buf.len() - keep;
                            self.preamble += drop;
                            self.buf.drain(..drop);
                            if self.preamble > MAX_PREAMBLE {
                                return Err(PushError::Parse(MpError::Malformed("no boundary")));
                            }
                        }
                        return Ok(());
                    }
                },
                State::AfterDelim => {
                    if self.buf.len() < 2 {
                        return Ok(());
                    }
                    if self.buf.starts_with(b"--") {
                        self.buf.clear();
                        self.state = State::Done;
                        continue;
                    }
                    match find(&self.buf, b"\r\n") {
                        Some(p) if self.buf[..p].iter().all(|&b| b == b' ' || b == b'\t') => {
                            self.buf.drain(..p + 2);
                            self.state = State::Headers;
                        }
                        Some(_) => return Err(PushError::Parse(MpError::Malformed("text after boundary"))),
                        None if self.buf.len() > 256 => return Err(PushError::Parse(MpError::Malformed("text after boundary"))),
                        None => return Ok(()),
                    }
                }
                State::Headers => {
                    // An empty header block is just the blank line.
                    let (end, skip) = if self.buf.starts_with(b"\r\n") {
                        (0, 2)
                    } else if self.buf.len() < 2 {
                        return Ok(());
                    } else {
                        match find(&self.buf, b"\r\n\r\n") {
                            Some(p) => (p, p + 4),
                            None if self.buf.len() > MAX_PART_HEAD => return Err(PushError::Parse(MpError::HeadTooLarge)),
                            None => return Ok(()),
                        }
                    };
                    if end > MAX_PART_HEAD {
                        return Err(PushError::Parse(MpError::HeadTooLarge));
                    }
                    let info = parse_part_head(&self.buf[..end]).map_err(PushError::Parse)?;
                    self.buf.drain(..skip);
                    self.parts += 1;
                    if self.parts > MAX_PARTS {
                        return Err(PushError::Parse(MpError::TooManyParts));
                    }
                    self.state = State::Body;
                    sink(Event::Part(info)).map_err(PushError::Sink)?;
                }
                State::Body => match find(&self.buf, &self.delim) {
                    Some(i) => {
                        if i > 0 {
                            sink(Event::Data(&self.buf[..i])).map_err(PushError::Sink)?;
                        }
                        self.buf.drain(..i + self.delim.len());
                        self.state = State::AfterDelim;
                        sink(Event::End).map_err(PushError::Sink)?;
                    }
                    None => {
                        // Everything except a possible start of the delimiter is data.
                        let safe = self.buf.len().saturating_sub(self.delim.len() - 1);
                        if safe > 0 {
                            sink(Event::Data(&self.buf[..safe])).map_err(PushError::Sink)?;
                            self.buf.drain(..safe);
                        }
                        return Ok(());
                    }
                },
            }
        }
    }

    /// The body is over: it must have ended with the closing delimiter.
    pub fn finish(&self) -> Result<(), MpError> {
        if self.state == State::Done { Ok(()) } else { Err(MpError::Truncated) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: &str = "----WebKitFormBoundaryAbC123";

    /// What a sink saw, with consecutive data events joined.
    #[derive(Debug, Default, PartialEq)]
    struct Seen {
        parts: Vec<(PartInfo, Vec<u8>)>,
        ended: usize,
    }

    fn run(chunks: impl IntoIterator<Item = Vec<u8>>) -> (Seen, Result<(), PushError<()>>, Result<(), MpError>) {
        let mut p = Parser::new(B);
        let mut seen = Seen::default();
        let mut res = Ok(());
        for c in chunks {
            let r = p.push(&c, &mut |e: Event<'_>| -> Result<(), ()> {
                match e {
                    Event::Part(i) => seen.parts.push((i, vec![])),
                    Event::Data(d) => {
                        assert!(!d.is_empty(), "empty data event");
                        seen.parts.last_mut().expect("data before part").1.extend_from_slice(d)
                    }
                    Event::End => seen.ended += 1,
                }
                Ok(())
            });
            if r.is_err() {
                res = r;
                break;
            }
        }
        let fin = p.finish();
        (seen, res, fin)
    }

    fn chunked(body: &[u8], n: usize) -> Vec<Vec<u8>> {
        body.chunks(n).map(|c| c.to_vec()).collect()
    }

    fn part(name: &str, filename: Option<&str>, data: &[u8]) -> Vec<u8> {
        let mut v = format!("--{B}\r\nContent-Disposition: form-data; name=\"{name}\"").into_bytes();
        if let Some(f) = filename {
            v.extend_from_slice(format!("; filename=\"{f}\"\r\nContent-Type: application/octet-stream").as_bytes());
        }
        v.extend_from_slice(b"\r\n\r\n");
        v.extend_from_slice(data);
        v.extend_from_slice(b"\r\n");
        v
    }

    fn body(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut v: Vec<u8> = parts.concat();
        v.extend_from_slice(format!("--{B}--\r\n").as_bytes());
        v
    }

    /// Deterministic pseudo-random bytes.
    fn noise(n: usize, seed: u32) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                (x >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn one_file_part() {
        let data = b"line1\r\nline2\r\n\r\n--not-the-boundary\r\n\x00\x01binary".to_vec();
        let b = body(&[part("file", Some("a b.jpg"), &data)]);
        let (seen, r, fin) = run([b]);
        assert!(r.is_ok() && fin.is_ok());
        assert_eq!(seen.ended, 1);
        assert_eq!(seen.parts.len(), 1);
        let (info, got) = &seen.parts[0];
        assert_eq!((info.name.as_deref(), info.filename.as_deref(), info.content_type.as_deref()), (Some("file"), Some("a b.jpg"), Some("application/octet-stream")));
        assert_eq!(got, &data);
    }

    #[test]
    fn several_parts_in_order_with_preamble_and_epilogue() {
        let mut b = b"this is the preamble, ignored\r\nmore\r\n".to_vec();
        b.extend(body(&[part("note", None, b"hello"), part("file", Some("x.txt"), b"FILE"), part("empty", None, b"")]));
        b.extend_from_slice(b"epilogue junk --- ignored");
        let (seen, r, fin) = run([b]);
        assert!(r.is_ok() && fin.is_ok(), "{r:?} {fin:?}");
        assert_eq!(seen.ended, 3);
        let got: Vec<(Option<&str>, &[u8])> = seen.parts.iter().map(|(i, d)| (i.name.as_deref(), d.as_slice())).collect();
        assert_eq!(got, [(Some("note"), &b"hello"[..]), (Some("file"), &b"FILE"[..]), (Some("empty"), &b""[..])]);
    }

    #[test]
    fn the_result_is_the_same_whatever_the_chunk_size() {
        let tricky: Vec<u8> = [
            noise(3000, 1),
            b"\r\n--".to_vec(),
            b"\r\n------WebKitFormBoundaryAbC12".to_vec(), // one byte short of a delimiter
            b"x".to_vec(),
            b"------WebKitFormBoundaryAbC123".to_vec(), // delimiter text without the leading CRLF: data
            noise(500, 2),
            b"\r".to_vec(),
        ]
        .concat();
        let b = body(&[part("a", Some("a.bin"), &tricky), part("b", None, b"second")]);
        let (want, r, fin) = run([b.clone()]);
        assert!(r.is_ok() && fin.is_ok());
        assert_eq!(want.parts[0].1, tricky);
        for n in (1..=40).chain([63, 64, 65, 100, 255, 1000, 4096]) {
            let (got, r, fin) = run(chunked(&b, n));
            assert!(r.is_ok() && fin.is_ok(), "chunk {n}: {r:?} {fin:?}");
            assert_eq!(got, want, "chunk {n}");
        }
    }

    #[test]
    fn every_two_way_split_of_the_delimiter_works() {
        // Cut the body at each single position: the delimiter (and the boundary line) straddles reads.
        let b = body(&[part("f", Some("f.bin"), &noise(200, 9)), part("g", None, b"tail")]);
        let (want, _, _) = run([b.clone()]);
        for cut in 0..=b.len() {
            let (got, r, fin) = run([b[..cut].to_vec(), b[cut..].to_vec()]);
            assert!(r.is_ok() && fin.is_ok(), "cut {cut}: {r:?} {fin:?}");
            assert_eq!(got, want, "cut {cut}");
        }
    }

    #[test]
    fn a_truncated_body_is_reported_at_every_cut() {
        let b = body(&[part("f", Some("f.bin"), &noise(300, 3))]);
        let end = b.len();
        // The closing "--" ends the body: what follows is optional CRLF.
        let complete_from = end - 2;
        for cut in 0..end {
            let (_, r, fin) = run([b[..cut].to_vec()]);
            assert!(r.is_ok(), "cut {cut}: {r:?}");
            if cut < complete_from {
                assert_eq!(fin, Err(MpError::Truncated), "cut {cut}");
            } else {
                assert_eq!(fin, Ok(()), "cut {cut} is after the closing delimiter");
            }
        }
        // And the empty body.
        assert_eq!(run([vec![]]).2, Err(MpError::Truncated));
    }

    #[test]
    fn huge_part_headers_are_refused() {
        // A header block that never ends.
        let mut b = format!("--{B}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"").into_bytes();
        b.extend(std::iter::repeat_n(b'a', 50_000));
        let (_, r, _) = run(chunked(&b, 1000));
        assert_eq!(r, Err(PushError::Parse(MpError::HeadTooLarge)));
        // One that ends, but only after the limit.
        let mut b = format!("--{B}\r\nContent-Disposition: form-data; name=\"x\"; filename=\"").into_bytes();
        b.extend(std::iter::repeat_n(b'a', MAX_PART_HEAD + 10));
        b.extend_from_slice(b"\"\r\n\r\ndata\r\n");
        let (_, r, _) = run([b]);
        assert_eq!(r, Err(PushError::Parse(MpError::HeadTooLarge)));
        // Just under the limit is fine.
        let mut b = format!("--{B}\r\nContent-Disposition: form-data; name=\"x\"; filename=\"").into_bytes();
        b.extend(std::iter::repeat_n(b'a', MAX_PART_HEAD - 200));
        b.extend_from_slice(b"\"\r\n\r\ndata\r\n");
        b.extend_from_slice(format!("--{B}--").as_bytes());
        let (seen, r, fin) = run([b]);
        assert!(r.is_ok() && fin.is_ok());
        assert_eq!(seen.parts[0].1, b"data");
    }

    #[test]
    fn too_many_parts_and_a_missing_boundary_are_refused() {
        let many: Vec<Vec<u8>> = (0..=MAX_PARTS).map(|i| part(&format!("p{i}"), None, b"x")).collect();
        let (_, r, _) = run([body(&many)]);
        assert_eq!(r, Err(PushError::Parse(MpError::TooManyParts)));
        let ok: Vec<Vec<u8>> = (0..MAX_PARTS).map(|i| part(&format!("p{i}"), None, b"x")).collect();
        let (seen, r, fin) = run([body(&ok)]);
        assert!(r.is_ok() && fin.is_ok());
        assert_eq!(seen.parts.len(), MAX_PARTS);
        // Endless junk without any boundary.
        let (_, r, _) = run(chunked(&noise(200_000, 5), 4096));
        assert_eq!(r, Err(PushError::Parse(MpError::Malformed("no boundary"))));
    }

    #[test]
    fn malformed_framing_is_refused() {
        // Text right after the boundary.
        let (_, r, _) = run([format!("--{B}XX\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nx\r\n--{B}--").into_bytes()]);
        assert_eq!(r, Err(PushError::Parse(MpError::Malformed("text after boundary"))));
        // A header line without a colon.
        let (_, r, _) = run([format!("--{B}\r\nnonsense\r\n\r\nx\r\n--{B}--").into_bytes()]);
        assert_eq!(r, Err(PushError::Parse(MpError::Malformed("part header without colon"))));
        // Blanks after the boundary are tolerated ("transport padding").
        let (seen, r, fin) = run([format!("--{B}  \t\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nx\r\n--{B}--").into_bytes()]);
        assert!(r.is_ok() && fin.is_ok());
        assert_eq!(seen.parts[0].1, b"x");
    }

    #[test]
    fn a_part_without_headers_and_an_empty_file() {
        let b = format!("--{B}\r\n\r\nraw data\r\n--{B}\r\nContent-Disposition: form-data; name=\"f\"; filename=\"empty.txt\"\r\n\r\n\r\n--{B}--\r\n").into_bytes();
        let (seen, r, fin) = run([b]);
        assert!(r.is_ok() && fin.is_ok(), "{r:?}");
        assert_eq!(seen.parts[0].0, PartInfo::default());
        assert_eq!(seen.parts[0].1, b"raw data");
        assert_eq!(seen.parts[1].0.filename.as_deref(), Some("empty.txt"));
        assert!(seen.parts[1].1.is_empty());
    }

    #[test]
    fn a_big_body_streams_with_a_small_buffer() {
        let data = noise(3 * 1024 * 1024, 77);
        let b = body(&[part("file", Some("big.bin"), &data)]);
        let mut p = Parser::new(B);
        let mut got = Vec::with_capacity(data.len());
        let mut peak = 0;
        for c in b.chunks(64 * 1024) {
            p.push(c, &mut |e: Event<'_>| -> Result<(), ()> {
                if let Event::Data(d) = e {
                    got.extend_from_slice(d);
                }
                Ok(())
            })
            .unwrap();
            peak = peak.max(p.buffered());
        }
        p.finish().unwrap();
        assert_eq!(got, data);
        assert!(peak < 1024, "only the possible start of a delimiter is held back ({peak})");
    }

    #[test]
    fn a_failing_sink_stops_the_parse() {
        let b = body(&[part("f", Some("f"), b"data")]);
        let mut p = Parser::new(B);
        let r = p.push(&b, &mut |e: Event<'_>| if matches!(e, Event::Data(_)) { Err("disk full") } else { Ok(()) });
        assert_eq!(r, Err(PushError::Sink("disk full")));
    }

    #[test]
    fn content_disposition_variants() {
        let cases: &[(&str, Option<&str>, Option<&str>)] = &[
            (r#"form-data; name="file"; filename="a.jpg""#, Some("file"), Some("a.jpg")),
            (r#"form-data; name=file; filename=a.jpg"#, Some("file"), Some("a.jpg")),
            (r#"form-data; name="f"; filename="semi;colon \"quoted\" back\\slash.txt""#, Some("f"), Some(r#"semi;colon "quoted" back\slash.txt"#)),
            (r#"form-data; name="f"; filename="../../etc/passwd""#, Some("f"), Some("../../etc/passwd")),
            ("form-data; name=\"f\"; filename=\"\u{65E5}\u{672C}.txt\"", Some("f"), Some("\u{65E5}\u{672C}.txt")),
            (r#"form-data; name="f"; filename="old.txt"; filename*=UTF-8''%E6%97%A5%E6%9C%AC.txt"#, Some("f"), Some("\u{65E5}\u{672C}.txt")),
            (r#"FORM-DATA; NAME="F"; FILENAME="U.TXT""#, Some("F"), Some("U.TXT")),
            (r#"form-data; name="text""#, Some("text"), None),
            (r#"form-data"#, None, None),
        ];
        for (cd, name, filename) in cases {
            let b = format!("--{B}\r\nContent-Disposition: {cd}\r\n\r\nx\r\n--{B}--").into_bytes();
            let (seen, r, fin) = run([b]);
            assert!(r.is_ok() && fin.is_ok(), "{cd}: {r:?}");
            assert_eq!((seen.parts[0].0.name.as_deref(), seen.parts[0].0.filename.as_deref()), (*name, *filename), "{cd}");
        }
        // Bad percent escapes in filename* are ignored (the plain filename stays).
        let b = format!("--{B}\r\nContent-Disposition: form-data; name=\"f\"; filename=\"keep.txt\"; filename*=UTF-8''%ZZ\r\n\r\nx\r\n--{B}--").into_bytes();
        let (seen, _, _) = run([b]);
        assert_eq!(seen.parts[0].0.filename.as_deref(), Some("keep.txt"));
    }

    #[test]
    fn the_boundary_is_read_from_the_content_type() {
        assert_eq!(boundary_from_content_type("multipart/form-data; boundary=----WebKitFormBoundaryAbC123").as_deref(), Some("----WebKitFormBoundaryAbC123"));
        assert_eq!(boundary_from_content_type(r#"multipart/form-data; charset=utf-8; boundary="abc def""#), None, "spaces are not accepted");
        assert_eq!(boundary_from_content_type(r#"Multipart/Form-Data; Boundary="abc-123""#).as_deref(), Some("abc-123"));
        assert_eq!(boundary_from_content_type("multipart/form-data; boundary=---------------------------9051914041544843365972754266").as_deref().map(str::len), Some(55));
        let max = "a".repeat(70);
        assert_eq!(boundary_from_content_type(&format!("multipart/form-data; boundary={max}")).as_deref(), Some(max.as_str()));
        for bad in ["", "multipart/form-data", "multipart/form-data; boundary=", &format!("multipart/form-data; boundary={}", "a".repeat(71)), "text/plain; boundary=abc", "multipart/mixed; boundary=abc", "multipart/form-data; boundary=a\"b", "multipart/form-data; boundary=a\r\nb", "multipart/form-data; boundary=a<b"] {
            assert_eq!(boundary_from_content_type(bad), None, "{bad:?}");
        }
    }
}
