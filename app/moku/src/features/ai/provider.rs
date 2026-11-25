//! The provider call: an OpenAI-compatible `POST <base>/chat/completions` with `stream: true`,
//! read as server-sent events through [`crate::net::request`].
//!
//! * The only address ever contacted is the one the user configured (`ai.base_url`); redirects
//!   are not followed, so the key never travels to another host; the key goes only in the
//!   `Authorization` header (and is not sent at all when there is none, e.g. Ollama).
//! * The answer is capped at [`MAX_ANSWER_CHARS`]: a provider (or something pretending to be
//!   one) that sends endless text is cut off, as is one whose events are enormous.
//! * Nothing of the question, the answer, the key or the URL is logged; errors carry only a
//!   short category and the HTTP status.
//! * A server that ignores `stream: true` and answers with one JSON document works too.

use std::time::Duration;

use super::prompts::Msg;
use super::sse;
use crate::net::{self, Cancel, Event as NetEvent, Options};

/// The longest answer kept, in characters (about 6000 tokens).
pub const MAX_ANSWER_CHARS: usize = 24_000;
/// How much of an error body is read.
const MAX_ERROR_BODY: usize = 16 * 1024;
/// How much of the start of the body is kept to recognise a non-streamed answer.
const MAX_RAW_HEAD: usize = 256 * 1024;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// First token of a big local model can take a while.
const READ_TIMEOUT: Duration = Duration::from_secs(90);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(240);

/// Where to send it.
pub struct Provider<'a> {
    pub base_url: &'a str,
    pub model: &'a str,
    /// The API key; `None` for providers that need none.
    pub key: Option<&'a str>,
}

/// What went wrong, without any user content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    Cancelled,
    /// The settings cannot be used (a bad base URL); already a message.
    Config(String),
    /// Connection, TLS or timeout: a short technical text.
    Network(String),
    Http { status: u32, detail: Option<String> },
    /// An error object inside a stream that started fine.
    Provider(String),
    /// The stream broke the size limits.
    TooBig,
    /// The provider answered 200 and sent nothing.
    Empty,
}

impl Failure {
    /// The text for the card.
    pub fn message(&self, en: bool) -> String {
        let t = |id: &str, e: &str| if en { e.to_string() } else { id.to_string() };
        match self {
            Failure::Cancelled => t("Dibatalkan", "Cancelled"),
            Failure::Config(m) => m.clone(),
            Failure::Network(e) if e == net::TIMED_OUT || e.contains("12002") || e.contains("timed out") => t("Penyedia terlalu lama menjawab", "The provider took too long to answer"),
            Failure::Network(_) => t("Tidak bisa terhubung ke penyedia. Cek internet dan alamatnya di Pengaturan", "Could not reach the provider. Check your internet and the address in Settings"),
            Failure::Http { status, detail } => {
                let base = if en { status_en(*status).to_string() } else { moku_core::ai::status_message(*status).to_string() };
                match detail {
                    Some(d) if !d.is_empty() => format!("{base}: {d}"),
                    _ => base,
                }
            }
            Failure::Provider(m) => format!("{}: {m}", t("Penyedia mengirim kesalahan", "The provider reported an error")),
            Failure::TooBig => t("Jawaban penyedia tidak wajar besar; dihentikan", "The provider's answer was unreasonably large; stopped"),
            Failure::Empty => t("Penyedia tidak mengirim jawaban", "The provider sent no answer"),
        }
    }

    /// Is it because of the key (so Settings is the place to go)?
    pub fn is_key_problem(&self) -> bool {
        matches!(self, Failure::Http { status: 401 | 403, .. })
    }
}

fn status_en(status: u32) -> &'static str {
    match status {
        200..=299 => "Connected",
        400 => "The provider rejected the request (check the model name)",
        401 | 403 => "API key rejected (wrong, expired, or no access)",
        404 => "Address or model not found (check the base URL and the model name)",
        408 | 504 => "The provider took too long to answer",
        429 => "Too many requests or quota used up",
        500..=599 => "The provider is having problems",
        300..=399 => "The address redirects; Moku does not follow redirects. Use the exact address",
        _ => "The provider answered with an error",
    }
}

/// What a call produced. `failure` is set when it did not end well; `text` is whatever arrived.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    pub text: String,
    /// The answer was cut at [`MAX_ANSWER_CHARS`].
    pub truncated: bool,
    pub finish: Option<String>,
    pub failure: Option<Failure>,
}

/// The JSON body of the request.
pub fn request_body(model: &str, messages: &[Msg]) -> String {
    let msgs: Vec<serde_json::Value> = messages.iter().map(|m| serde_json::json!({"role": m.role, "content": m.content})).collect();
    serde_json::json!({"model": model, "stream": true, "messages": msgs}).to_string()
}

/// What one streamed JSON event carried.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    pub finish: Option<String>,
    pub error: Option<String>,
}

fn error_text(e: &serde_json::Value) -> String {
    let s = match e {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(o) => o.get("message").and_then(|m| m.as_str()).map(str::to_string).or_else(|| o.get("code").map(|c| c.to_string())).unwrap_or_else(|| "error".into()),
        other => other.to_string(),
    };
    sanitize(&s)
}

/// One line, no control characters, at most 200 characters, secrets hidden.
fn sanitize(s: &str) -> String {
    let one = moku_core::aimem::clean(s);
    let cut: String = one.chars().take(200).collect();
    super::redact::redact(&cut).text
}

/// Parse one event's JSON: the text delta, a finish reason, or an error. Anything it cannot
/// read is an empty chunk.
pub fn parse_chunk(data: &str) -> Chunk {
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(data) else { return Chunk::default() };
    // Some providers wrap an error in an array.
    if let serde_json::Value::Array(a) = &v {
        match a.first() {
            Some(first) => v = first.clone(),
            None => return Chunk::default(),
        }
    }
    let mut c = Chunk::default();
    if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
        c.error = Some(error_text(e));
        return c;
    }
    let Some(choice) = v.get("choices").and_then(|c| c.as_array()).and_then(|c| c.first()) else { return c };
    c.finish = choice.get("finish_reason").and_then(|f| f.as_str()).map(str::to_string);
    // A streamed delta, a whole message, or the old completions text.
    let content = choice.pointer("/delta/content").or_else(|| choice.pointer("/message/content")).or_else(|| choice.get("text"));
    c.text = match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts.iter().filter_map(|p| p.get("text").and_then(|t| t.as_str())).collect::<Vec<_>>().concat(),
        _ => String::new(),
    };
    c
}

/// The message of an error body (`{"error":{"message":…}}`, `{"message":…}`, `[{"error":…}]`, or
/// a short plain text), cleaned up for display.
pub fn error_detail(body: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(body);
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(t) {
        if let serde_json::Value::Array(a) = &v {
            v = a.first()?.clone();
        }
        let e = v.get("error").filter(|e| !e.is_null()).or_else(|| v.get("message"))?;
        let s = error_text(e);
        return (!s.is_empty()).then_some(s);
    }
    // Plain text (but not an HTML error page).
    if t.starts_with('<') || t.len() > 400 {
        return None;
    }
    let s = sanitize(t);
    (!s.is_empty()).then_some(s)
}

struct Run<'a> {
    on_text: &'a mut dyn FnMut(&str),
    out: Outcome,
    status: u32,
    parser: sse::Parser,
    err_body: Vec<u8>,
    raw_head: Vec<u8>,
    events: bool,
    stop: bool,
    chars: usize,
}

impl Run<'_> {
    fn add_text(&mut self, t: &str) {
        if t.is_empty() {
            return;
        }
        self.events = true;
        let room = MAX_ANSWER_CHARS - self.chars;
        let n = t.chars().count();
        let piece: String = if n > room {
            self.out.truncated = true;
            self.stop = true;
            t.chars().take(room).collect()
        } else {
            t.to_string()
        };
        self.chars += piece.chars().count();
        self.out.text.push_str(&piece);
        if !piece.is_empty() {
            (self.on_text)(&piece);
        }
    }

    fn event(&mut self, e: sse::Event) {
        match e {
            sse::Event::Done => self.stop = true,
            sse::Event::TooBig => {
                self.out.failure = Some(Failure::TooBig);
                self.stop = true;
            }
            sse::Event::Data(d) => {
                let c = parse_chunk(&d);
                if let Some(err) = c.error {
                    self.out.failure = Some(Failure::Provider(err));
                    self.stop = true;
                    return;
                }
                if c.finish.is_some() {
                    self.out.finish = c.finish;
                }
                self.add_text(&c.text);
            }
        }
    }
}

/// Make the call. `on_text` gets each new piece of the answer as it arrives (on this thread).
/// Blocking: run it on a worker thread; `cancel` stops it from another.
pub fn run(p: &Provider, messages: &[Msg], cancel: &Cancel, on_text: &mut dyn FnMut(&str)) -> Outcome {
    let url = match moku_core::ai::chat_url(p.base_url) {
        Ok(u) => u,
        Err(e) => return Outcome { failure: Some(Failure::Config(e)), ..Default::default() },
    };
    let body = request_body(p.model, messages);
    let auth = p.key.map(|k| format!("Bearer {k}"));
    let mut headers: Vec<(&str, &str)> = vec![("Content-Type", "application/json"), ("Accept", "text/event-stream")];
    if let Some(a) = &auth {
        headers.push(("Authorization", a.as_str()));
    }
    let opts = Options {
        method: "POST",
        headers: &headers,
        body: Some(body.as_bytes()),
        connect_timeout: CONNECT_TIMEOUT,
        read_timeout: READ_TIMEOUT,
        total_timeout: Some(TOTAL_TIMEOUT),
        follow_redirects: false,
        cancel: Some(cancel),
    };
    let mut run = Run { on_text, out: Outcome::default(), status: 0, parser: sse::Parser::new(), err_body: vec![], raw_head: vec![], events: false, stop: false, chars: 0 };
    let result = net::request(&url, &opts, &mut |ev| {
        match ev {
            NetEvent::Status(s) => run.status = s,
            NetEvent::Data(d) => {
                if !(200..300).contains(&run.status) {
                    let room = MAX_ERROR_BODY.saturating_sub(run.err_body.len());
                    run.err_body.extend_from_slice(&d[..d.len().min(room)]);
                } else {
                    if !run.events && run.raw_head.len() < MAX_RAW_HEAD {
                        let room = MAX_RAW_HEAD - run.raw_head.len();
                        run.raw_head.extend_from_slice(&d[..d.len().min(room)]);
                    }
                    for e in run.parser.push(d) {
                        run.event(e);
                        if run.stop {
                            break;
                        }
                    }
                }
            }
        }
        !run.stop
    });
    let mut out = std::mem::take(&mut run.out);
    match result {
        Err(e) => {
            out.failure = Some(if e == net::CANCELLED { Failure::Cancelled } else { Failure::Network(e) });
            out
        }
        Ok(status) if !(200..300).contains(&status) => {
            out.failure = Some(Failure::Http { status, detail: error_detail(&run.err_body) });
            out
        }
        Ok(_) => {
            run.out = out;
            if !run.stop {
                for e in run.parser.finish() {
                    run.event(e);
                    if run.stop {
                        break;
                    }
                }
            }
            if run.out.text.is_empty() && run.out.failure.is_none() {
                // Not a stream at all? One JSON document is fine.
                let c = parse_chunk(&String::from_utf8_lossy(&run.raw_head));
                match c.error {
                    Some(e) => run.out.failure = Some(Failure::Provider(e)),
                    None if !c.text.is_empty() => {
                        run.out.finish = c.finish;
                        let t = c.text;
                        run.add_text(&t);
                    }
                    None => run.out.failure = Some(Failure::Empty),
                }
            }
            run.out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::mock::{Script, Server, Step};
    use std::sync::Arc;

    fn msgs() -> Vec<Msg> {
        vec![Msg::system("sys"), Msg::user("halo")]
    }

    fn delta(t: &str) -> Vec<u8> {
        format!("data: {{\"choices\":[{{\"delta\":{{\"content\":{}}},\"finish_reason\":null}}]}}\n\n", serde_json::to_string(t).unwrap()).into_bytes()
    }

    fn done() -> Vec<u8> {
        b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_vec()
    }

    fn call(srv: &Server, key: Option<&str>) -> (Outcome, Vec<String>) {
        let p = Provider { base_url: &srv.base(), model: "test-model", key };
        let mut pieces = vec![];
        let out = run(&p, &msgs(), &Cancel::new(), &mut |s| pieces.push(s.to_string()));
        (out, pieces)
    }

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        crate::net::test_lock()
    }

    // ---- parsing -------------------------------------------------------------------------

    #[test]
    fn chunks_deltas_errors_and_odd_shapes() {
        assert_eq!(parse_chunk(r#"{"choices":[{"delta":{"content":"Hai"},"finish_reason":null}]}"#), Chunk { text: "Hai".into(), finish: None, error: None });
        assert_eq!(parse_chunk(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#), Chunk { text: String::new(), finish: Some("stop".into()), error: None });
        assert_eq!(parse_chunk(r#"{"choices":[{"delta":{"content":null}}]}"#).text, "");
        assert_eq!(parse_chunk(r#"{"choices":[]}"#), Chunk::default(), "a usage-only chunk");
        assert_eq!(parse_chunk(r#"{"choices":[{"delta":{"reasoning_content":"mikir…","content":"Jawab"}}]}"#).text, "Jawab", "reasoning text is not shown");
        // The whole message (non-streamed), the old text field, content as parts.
        assert_eq!(parse_chunk(r#"{"choices":[{"message":{"role":"assistant","content":"Utuh"},"finish_reason":"stop"}]}"#).text, "Utuh");
        assert_eq!(parse_chunk(r#"{"choices":[{"text":"lama"}]}"#).text, "lama");
        assert_eq!(parse_chunk(r#"{"choices":[{"delta":{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}}]}"#).text, "ab");
        // Errors: object, string, array-wrapped.
        assert_eq!(parse_chunk(r#"{"error":{"message":"rate limited","type":"x"}}"#).error.as_deref(), Some("rate limited"));
        assert_eq!(parse_chunk(r#"{"error":"boom"}"#).error.as_deref(), Some("boom"));
        assert_eq!(parse_chunk(r#"[{"error":{"code":429,"message":"quota"}}]"#).error.as_deref(), Some("quota"));
        assert_eq!(parse_chunk(r#"{"error":{"code":500}}"#).error.as_deref(), Some("500"));
        // Rubbish is an empty chunk, never a panic.
        for bad in ["", "not json", "{", "[]", "null", "42", "\"x\"", r#"{"choices":"x"}"#, r#"{"choices":[1]}"#, &"[".repeat(5000)] {
            assert_eq!(parse_chunk(bad), Chunk::default(), "{bad:.20}");
        }
    }

    #[test]
    fn error_bodies_are_read_cleaned_and_never_html() {
        assert_eq!(error_detail(br#"{"error":{"message":"Incorrect API key provided: sk-abc********1234.","type":"invalid_request_error"}}"#).as_deref(), Some("Incorrect API key provided: sk-abc********1234."));
        assert_eq!(error_detail(br#"{"message":"No auth"}"#).as_deref(), Some("No auth"));
        assert_eq!(error_detail(br#"[{"error":{"message":"API key not valid"}}]"#).as_deref(), Some("API key not valid"));
        assert_eq!(error_detail(b"Unauthorized").as_deref(), Some("Unauthorized"));
        assert_eq!(error_detail(b"<html><body>502 Bad Gateway</body></html>"), None);
        assert_eq!(error_detail(b""), None);
        assert_eq!(error_detail(b"   "), None);
        // Long, multi-line and secret-bearing messages are cut, flattened and hidden.
        let long = format!("{{\"error\":{{\"message\":\"line1\\nline2 {} sk-proj-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789\"}}}}", "x".repeat(400));
        let d = error_detail(long.as_bytes()).unwrap();
        assert!(d.chars().count() <= 200 && !d.contains('\n'), "{d}");
        let d = error_detail(br#"{"error":{"message":"bad key sk-proj-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789 sorry"}}"#).unwrap();
        assert_eq!(d, "bad key [disamarkan: token] sorry");
    }

    #[test]
    fn the_request_body_is_plain_openai() {
        let b: serde_json::Value = serde_json::from_str(&request_body("gpt-4o-mini", &msgs())).unwrap();
        assert_eq!(b["model"], "gpt-4o-mini");
        assert_eq!(b["stream"], true);
        assert_eq!(b["messages"][0], serde_json::json!({"role": "system", "content": "sys"}));
        assert_eq!(b["messages"][1], serde_json::json!({"role": "user", "content": "halo"}));
        assert_eq!(b.as_object().unwrap().len(), 3, "only model, stream and messages: {b}");
        // Quotes, newlines and unicode survive.
        let b: serde_json::Value = serde_json::from_str(&request_body("m", &[Msg::user("a \"q\"\n日本 🔑")])).unwrap();
        assert_eq!(b["messages"][0]["content"], "a \"q\"\n日本 🔑");
    }

    #[test]
    fn failure_messages_in_both_languages() {
        assert_eq!(Failure::Http { status: 401, detail: None }.message(false), "Kunci API ditolak (salah, kedaluwarsa, atau tidak punya akses)");
        assert!(Failure::Http { status: 401, detail: Some("invalid".into()) }.message(true).ends_with(": invalid"));
        assert!(Failure::Network("timeout".into()).message(false).contains("lama"));
        assert!(Failure::Network("connect failed".into()).message(true).contains("Could not reach"));
        assert!(Failure::Provider("quota".into()).message(false).contains("quota"));
        assert!(Failure::Http { status: 401, detail: None }.is_key_problem() && !Failure::Empty.is_key_problem());
        for f in [Failure::Cancelled, Failure::TooBig, Failure::Empty, Failure::Config("x".into())] {
            assert!(!f.message(false).is_empty() && !f.message(true).is_empty());
        }
    }

    // ---- the whole call against a local server --------------------------------------------

    #[test]
    fn a_full_streamed_request() {
        let _s = serial();
        let mut steps = vec![];
        // The first event is split across two HTTP chunks; one chunk holds two events.
        let first = delta("Halo");
        steps.push(Step::Send(first[..20].to_vec()));
        steps.push(Step::Sleep(30));
        steps.push(Step::Send(first[20..].to_vec()));
        steps.push(Step::Send([delta(", "), delta("dunia")].concat()));
        steps.push(Step::Send(b": keep-alive\n\n".to_vec()));
        steps.push(Step::Send(done()));
        let srv = Server::start(move |_| Script::sse(steps.clone()));
        let (out, pieces) = call(&srv, Some("sk-test-key"));
        assert_eq!(out.failure, None);
        assert_eq!(out.text, "Halo, dunia");
        assert_eq!(out.finish.as_deref(), Some("stop"));
        assert!(!out.truncated);
        assert_eq!(pieces.concat(), "Halo, dunia");
        assert_eq!(pieces.len(), 3, "one callback per delta: {pieces:?}");
        // What the server saw: exactly one request, to the configured path, with the key.
        let seen = srv.requests();
        assert_eq!(seen.len(), 1);
        let r = &seen[0];
        assert_eq!((r.method.as_str(), r.path.as_str()), ("POST", "/v1/chat/completions"));
        assert_eq!(r.header("authorization"), Some("Bearer sk-test-key"));
        assert_eq!(r.header("content-type"), Some("application/json"));
        assert_eq!(r.header("accept"), Some("text/event-stream"));
        let body: serde_json::Value = serde_json::from_str(&r.body_text()).unwrap();
        assert_eq!((body["model"].as_str(), body["stream"].as_bool()), (Some("test-model"), Some(true)));
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        assert!(!r.body_text().contains("sk-test-key"), "the key is only in the header");
    }

    #[test]
    fn without_a_key_there_is_no_authorization_header() {
        let _s = serial();
        let srv = Server::start(|_| Script::sse(vec![Step::Send(delta("ok")), Step::Send(done())]));
        let (out, _) = call(&srv, None);
        assert_eq!(out.text, "ok");
        assert_eq!(srv.requests()[0].header("authorization"), None);
    }

    #[test]
    fn a_stream_without_done_and_with_a_last_event_without_a_blank_line_still_works() {
        let _s = serial();
        let srv = Server::start(|_| Script::sse(vec![Step::Send(delta("satu ")), Step::Send(b"data: {\"choices\":[{\"delta\":{\"content\":\"dua\"}}]}".to_vec())]));
        let (out, _) = call(&srv, None);
        assert_eq!((out.text.as_str(), out.failure), ("satu dua", None));
    }

    #[test]
    fn http_errors_become_failures_with_a_short_detail() {
        let _s = serial();
        for (status, body, want_status) in [
            (401, r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error"}}"#, 401),
            (429, r#"{"error":{"message":"You exceeded your current quota"}}"#, 429),
            (404, r#"{"error":{"message":"The model `x` does not exist"}}"#, 404),
            (500, "<html>oops</html>", 500),
            (503, "", 503),
        ] {
            let b = body.to_string();
            let srv = Server::start(move |_| Script::json(status, &b));
            let (out, pieces) = call(&srv, Some("k"));
            assert!(pieces.is_empty() && out.text.is_empty());
            match out.failure {
                Some(Failure::Http { status: s, detail }) => {
                    assert_eq!(s, want_status);
                    assert_eq!(detail.is_some(), body.starts_with('{'), "{status}: {detail:?}");
                }
                other => panic!("{status}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_redirect_is_an_error_and_never_followed() {
        let _s = serial();
        let other = Server::start(|_| Script::json(200, "{}"));
        let target = other.url("/stolen");
        let srv = Server::start(move |_| Script::redirect(&target));
        let (out, _) = call(&srv, Some("sk-secret"));
        assert!(matches!(out.failure, Some(Failure::Http { status: 302, .. })), "{:?}", out.failure);
        assert!(other.requests().is_empty(), "the key never reached the other host");
    }

    #[test]
    fn an_error_inside_a_stream_keeps_what_came_before() {
        let _s = serial();
        let srv = Server::start(|_| Script::sse(vec![Step::Send(delta("Setengah ")), Step::Send(b"data: {\"error\":{\"message\":\"upstream failed\"}}\n\n".to_vec()), Step::Send(delta("tidak muncul"))]));
        let (out, pieces) = call(&srv, None);
        assert_eq!(out.text, "Setengah ");
        assert_eq!(out.failure, Some(Failure::Provider("upstream failed".into())));
        assert_eq!(pieces.concat(), "Setengah ");
    }

    #[test]
    fn a_non_streaming_answer_and_an_empty_answer() {
        let _s = serial();
        let srv = Server::start(|_| Script::json(200, r#"{"id":"x","choices":[{"message":{"role":"assistant","content":"Jawaban utuh"},"finish_reason":"stop"}]}"#));
        let (out, pieces) = call(&srv, None);
        assert_eq!((out.text.as_str(), out.failure.clone(), out.finish.as_deref()), ("Jawaban utuh", None, Some("stop")));
        assert_eq!(pieces, ["Jawaban utuh"]);
        let srv = Server::start(|_| Script::json(200, r#"{"error":{"message":"model overloaded"}}"#));
        assert_eq!(call(&srv, None).0.failure, Some(Failure::Provider("model overloaded".into())));
        let srv = Server::start(|_| Script::sse(vec![Step::Send(done())]));
        assert_eq!(call(&srv, None).0.failure, Some(Failure::Empty));
        let srv = Server::start(|_| Script::json(200, "just words"));
        assert_eq!(call(&srv, None).0.failure, Some(Failure::Empty));
    }

    /// A provider (or an impostor) that never stops: the answer is cut at the limit, the call
    /// returns early, and memory stays bounded.
    #[test]
    fn a_huge_stream_is_cut_at_the_answer_limit() {
        let _s = serial();
        let piece = "x".repeat(4000);
        let steps: Vec<Step> = (0..4000).map(|_| Step::Send(delta(&piece))).collect(); // 16 MB of text
        let srv = Server::start(move |_| Script::sse(steps.clone()));
        let t = std::time::Instant::now();
        let (out, pieces) = call(&srv, None);
        assert!(out.truncated, "{:?}", out.failure);
        assert_eq!(out.text.chars().count(), MAX_ANSWER_CHARS);
        assert_eq!(out.failure, None, "being cut is not an error");
        assert_eq!(pieces.concat().chars().count(), MAX_ANSWER_CHARS);
        assert!(t.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn one_enormous_event_or_line_is_refused() {
        let _s = serial();
        // One event of 2 MB in a single data line.
        let big = format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{}\"}}}}]}}\n\n", "y".repeat(2 * 1024 * 1024));
        let b2 = big.into_bytes();
        let srv = Server::start(move |_| Script::sse(vec![Step::Send(delta("ok ")), Step::Send(b2.clone())]));
        let (out, _) = call(&srv, None);
        assert_eq!(out.failure, Some(Failure::TooBig));
        assert_eq!(out.text, "ok ", "what came before is kept");
        // A "line" that never ends.
        let srv = Server::start(|_| Script::sse((0..40).map(|_| Step::Send(vec![b'q'; 64 * 1024])).collect()));
        assert_eq!(call(&srv, None).0.failure, Some(Failure::TooBig));
    }

    #[test]
    fn hostile_bytes_in_the_stream_do_not_break_it() {
        let _s = serial();
        let mut junk = vec![0xffu8, 0xfe, 0x00, b'd', b'a', b't', b'a', b':', b' ', 0xc3, b'\n', b'\n'];
        junk.extend(b"data: {{{{ not json\n\n");
        junk.extend(b"data: ".iter().chain("[".repeat(10_000).as_bytes()));
        junk.extend(b"\n\n");
        let srv = Server::start(move |_| Script::sse(vec![Step::Send(junk.clone()), Step::Send(delta("selamat")), Step::Send(done())]));
        let (out, _) = call(&srv, None);
        assert_eq!((out.text.as_str(), out.failure), ("selamat", None));
    }

    #[test]
    fn a_cut_off_connection_keeps_the_partial_answer() {
        let _s = serial();
        let srv = Server::start(|_| Script::sse(vec![Step::Send(delta("sebagian ")), Step::Send(delta("jawaban")), Step::Cut]));
        let (out, _) = call(&srv, None);
        // The server hung up without the final chunk: the text so far stays, and it is an error.
        assert_eq!(out.text, "sebagian jawaban");
        assert!(matches!(out.failure, Some(Failure::Network(_))), "{:?}", out.failure);
    }

    #[test]
    fn cancel_stops_a_stalled_provider_quickly_and_keeps_the_partial_answer() {
        let _s = serial();
        let srv = Server::start(|_| Script::sse(vec![Step::Send(delta("mulai ")), Step::Hang]));
        let cancel = Arc::new(Cancel::new());
        let (c2, base) = (cancel.clone(), srv.base());
        // What the worker has received so far, where the test can see it.
        let seen = Arc::new(std::sync::Mutex::new(String::new()));
        let seen2 = seen.clone();
        let worker = std::thread::spawn(move || {
            let p = Provider { base_url: &base, model: "m", key: None };
            let mut got = String::new();
            let out = run(&p, &msgs(), &c2, &mut |s| {
                got.push_str(s);
                seen2.lock().unwrap().push_str(s);
            });
            (out, got)
        });
        // Cancel when the first piece has really arrived (the provider then stalls), not after a guessed time.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !seen.lock().unwrap().contains("mulai ") {
            assert!(std::time::Instant::now() < deadline, "the first piece never arrived");
            std::thread::sleep(Duration::from_millis(2));
        }
        let t = std::time::Instant::now();
        cancel.cancel();
        let (out, got) = worker.join().unwrap();
        assert!(t.elapsed() < Duration::from_secs(3));
        assert_eq!(out.failure, Some(Failure::Cancelled));
        assert_eq!((out.text.as_str(), got.as_str()), ("mulai ", "mulai "));
    }

    #[test]
    fn bad_settings_fail_before_anything_is_sent() {
        let _s = serial();
        let srv = Server::start(|_| Script::json(200, "{}"));
        for base in ["http://api.openai.com/v1", "ftp://x", "", "https://user:pw@x.com", "not a url"] {
            let p = Provider { base_url: base, model: "m", key: Some("k") };
            let out = run(&p, &msgs(), &Cancel::new(), &mut |_| {});
            assert!(matches!(out.failure, Some(Failure::Config(_))), "{base:?}: {:?}", out.failure);
        }
        assert!(srv.requests().is_empty());
        // A closed port is a network failure, not a hang.
        let p = Provider { base_url: "http://127.0.0.1:9/v1", model: "m", key: None };
        assert!(matches!(run(&p, &msgs(), &Cancel::new(), &mut |_| {}).failure, Some(Failure::Network(_))));
    }
}
