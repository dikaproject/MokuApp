//! A server-sent-events parser (the streaming format of `/chat/completions`).
//!
//! Bytes go in as they arrive, in any pieces (a chunk may end in the middle of a line, of a
//! UTF-8 character, or between the `\r` and the `\n` of a line break); finished events come out.
//! Follows the WHATWG rules that matter here: lines end with `\n`, `\r\n` or `\r`; a blank line
//! ends an event; `data:` lines are joined with `\n`; lines that start with `:` are comments
//! (OpenRouter sends `: OPENROUTER PROCESSING` keep-alives); other fields are ignored.
//! `data: [DONE]` is the end marker of the OpenAI protocol.
//!
//! Hostile input cannot grow the buffers: a single line over [`MAX_LINE`] or an event over
//! [`MAX_EVENT`] ends the parse with [`Event::TooBig`], and nothing more is accepted.

/// Longest line kept while waiting for its end.
pub const MAX_LINE: usize = 256 * 1024;
/// Longest event (all its `data:` lines together).
pub const MAX_EVENT: usize = 512 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The data of one event.
    Data(String),
    /// `data: [DONE]`.
    Done,
    /// A line or an event was bigger than the limits; the parser stopped.
    TooBig,
}

#[derive(Default)]
pub struct Parser {
    line: Vec<u8>,
    /// The data lines of the event being read.
    data: String,
    has_data: bool,
    /// The previous byte was `\r` (a following `\n` belongs to the same line break).
    after_cr: bool,
    started: bool,
    dead: bool,
}

impl Parser {
    pub fn new() -> Parser {
        Parser::default()
    }

    /// Feed bytes; returns the events they completed.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Event> {
        let mut out = vec![];
        for &b in bytes {
            if self.dead {
                break;
            }
            if self.after_cr {
                self.after_cr = false;
                if b == b'\n' {
                    continue;
                }
            }
            match b {
                b'\r' => {
                    self.end_line(&mut out);
                    self.after_cr = true;
                }
                b'\n' => self.end_line(&mut out),
                _ => {
                    self.line.push(b);
                    if self.line.len() > MAX_LINE {
                        self.die(&mut out);
                    }
                }
            }
        }
        out
    }

    /// The stream ended. A last event that was not followed by a blank line is delivered too
    /// (servers that just close the connection); the half of an event that was cut off simply
    /// is not valid data for the caller to use.
    pub fn finish(&mut self) -> Vec<Event> {
        let mut out = vec![];
        if self.dead {
            return out;
        }
        if !self.line.is_empty() {
            self.end_line(&mut out);
        }
        self.dispatch(&mut out);
        out
    }

    fn die(&mut self, out: &mut Vec<Event>) {
        self.dead = true;
        self.line = Vec::new();
        self.data = String::new();
        out.push(Event::TooBig);
    }

    fn dispatch(&mut self, out: &mut Vec<Event>) {
        if self.has_data {
            let d = std::mem::take(&mut self.data);
            self.has_data = false;
            out.push(if d.trim() == "[DONE]" { Event::Done } else { Event::Data(d) });
        }
    }

    fn end_line(&mut self, out: &mut Vec<Event>) {
        let raw = std::mem::take(&mut self.line);
        let mut line = String::from_utf8_lossy(&raw).into_owned();
        if !self.started {
            self.started = true;
            if let Some(rest) = line.strip_prefix('\u{feff}') {
                line = rest.to_string();
            }
        }
        if line.is_empty() {
            self.dispatch(out);
            return;
        }
        if line.starts_with(':') {
            return; // comment / keep-alive
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line.as_str(), ""),
        };
        if field == "data" {
            if self.has_data {
                self.data.push('\n');
            }
            self.data.push_str(value);
            self.has_data = true;
            if self.data.len() > MAX_EVENT {
                self.die(out);
            }
        }
        // `event:`, `id:`, `retry:` and unknown fields do not matter for chat completions.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(chunks: &[&[u8]]) -> Vec<Event> {
        let mut p = Parser::new();
        let mut out = vec![];
        for c in chunks {
            out.extend(p.push(c));
        }
        out.extend(p.finish());
        out
    }

    fn d(s: &str) -> Event {
        Event::Data(s.into())
    }

    #[test]
    fn simple_events_and_done() {
        let ev = all(&[b"data: {\"a\":1}\n\ndata: {\"a\":2}\n\ndata: [DONE]\n\n"]);
        assert_eq!(ev, [d("{\"a\":1}"), d("{\"a\":2}"), Event::Done]);
    }

    #[test]
    fn any_chunking_gives_the_same_events() {
        let stream = "data: {\"x\":\"é日本\"}\r\n\r\n: keep-alive\r\n\r\ndata: one\r\ndata: two\r\n\r\nevent: ping\r\ndata: [DONE]\r\n\r\n".as_bytes();
        let whole = all(&[stream]);
        assert_eq!(whole, [d("{\"x\":\"é日本\"}"), d("one\ntwo"), Event::Done]);
        // Every possible split point, and one byte at a time (this cuts UTF-8 characters and
        // CRLF pairs in the middle).
        for cut in 0..=stream.len() {
            assert_eq!(all(&[&stream[..cut], &stream[cut..]]), whole, "split at {cut}");
        }
        let singles: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(all(&singles), whole);
    }

    #[test]
    fn line_endings_cr_lf_and_crlf() {
        for nl in ["\n", "\r\n", "\r"] {
            let s = format!("data: a{nl}data: b{nl}{nl}data: c{nl}{nl}");
            assert_eq!(all(&[s.as_bytes()]), [d("a\nb"), d("c")], "{nl:?}");
        }
        // A CR at the end of one chunk and the LF at the start of the next is ONE line break.
        assert_eq!(all(&[b"data: a\r", b"\n\r", b"\ndata: b\n\n"]), [d("a"), d("b")]);
    }

    #[test]
    fn multi_line_data_comments_and_other_fields() {
        let s = ": OPENROUTER PROCESSING\n\nid: 7\nevent: message\nretry: 100\ndata: {\"a\":\ndata: 1}\ndata:{\"nospace\":true}\n\nwhatever\ndata\n\n";
        let ev = all(&[s.as_bytes()]);
        // `data: ` strips ONE space, `data:` none; other fields and comments do not matter; a line
        // with no colon is a field with an empty value (`data` alone is an event with empty data).
        assert_eq!(ev, [d("{\"a\":\n1}\n{\"nospace\":true}"), d("")]);
    }
    #[test]
    fn an_event_without_a_final_blank_line_is_still_delivered() {
        assert_eq!(all(&[b"data: {\"a\":1}\n\ndata: {\"a\":2}"]), [d("{\"a\":1}"), d("{\"a\":2}")]);
        assert_eq!(all(&[b"data: [DONE]"]), [Event::Done]);
        assert_eq!(all(&[b"data: x\n"]), [d("x")]);
        assert!(all(&[b""]).is_empty());
        assert!(all(&[b": only a comment\n"]).is_empty());
    }

    #[test]
    fn utf8_bom_and_invalid_bytes_do_not_break_it() {
        assert_eq!(all(&["\u{feff}data: hi\n\n".as_bytes()]), [d("hi")]);
        let ev = all(&[b"data: a\xffb\xc3\n\n"]);
        assert_eq!(ev.len(), 1);
        match &ev[0] {
            Event::Data(s) => assert!(s.starts_with('a') && s.contains('b') && s.contains('\u{fffd}'), "{s:?}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_huge_line_ends_the_parse_without_growing_without_bound() {
        let mut p = Parser::new();
        let chunk = vec![b'a'; 64 * 1024];
        let mut seen = vec![];
        for _ in 0..40 {
            seen.extend(p.push(&chunk));
        }
        assert_eq!(seen, [Event::TooBig], "one TooBig, then silence");
        assert!(p.line.len() <= MAX_LINE + 1 && p.line.capacity() < 2 * MAX_LINE, "the buffer was released");
        assert!(p.push(b"data: late\n\n").is_empty(), "nothing is accepted after TooBig");
        assert!(p.finish().is_empty());
    }

    #[test]
    fn a_huge_event_made_of_many_lines_ends_the_parse() {
        let mut p = Parser::new();
        let line = format!("data: {}\n", "b".repeat(60 * 1024));
        let mut seen = vec![];
        for _ in 0..20 {
            seen.extend(p.push(line.as_bytes()));
        }
        assert_eq!(seen, [Event::TooBig]);
        // A big but allowed event still works.
        let ok = format!("data: {}\n\n", "c".repeat(MAX_LINE - 100));
        assert_eq!(all(&[ok.as_bytes()]).len(), 1);
    }

    #[test]
    fn done_must_be_the_whole_data() {
        assert_eq!(all(&[b"data: [DONE]\n\n"]), [Event::Done]);
        assert_eq!(all(&[b"data:[DONE]\n\n"]), [Event::Done]);
        assert_eq!(all(&[b"data: [DONE] extra\n\n"]), [d("[DONE] extra")]);
        assert_eq!(all(&[b"data: {\"text\":\"[DONE]\"}\n\n"]), [d("{\"text\":\"[DONE]\"}")]);
    }
}
