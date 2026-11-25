//! JSON in the clipboard: detection and pretty-printing ("Rapikan").
//!
//! The printer works on the text, not on a parsed value: object keys keep their order and
//! numbers and string escapes stay exactly as written (a parsed-and-printed value would sort the
//! keys and could change big numbers). The text is validated first (`serde_json`), so the printer
//! only ever sees well-formed JSON. Pure (no Windows API).

/// Largest text the action looks at (the clipboard keeps at most 100 KB per item anyway).
pub const MAX_BYTES: usize = 256 * 1024;

/// Is `text` a JSON object or array (not a bare number or string) of at most [`MAX_BYTES`]?
pub fn is_container(text: &str) -> bool {
    let t = text.trim();
    t.len() >= 2
        && t.len() <= MAX_BYTES
        && ((t.starts_with('{') && t.ends_with('}')) || (t.starts_with('[') && t.ends_with(']')))
        && serde_json::from_str::<serde::de::IgnoredAny>(t).is_ok()
}

/// The text re-indented with two spaces, one member per line. `None` when it is not a JSON object
/// or array.
pub fn pretty(text: &str) -> Option<String> {
    let t = text.trim();
    if !is_container(t) {
        return None;
    }
    let mut out = String::with_capacity(t.len() * 2);
    let mut depth = 0usize;
    let mut chars = t.chars().peekable();
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        out.extend(std::iter::repeat_n(' ', depth * 2));
    };
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                out.push('"');
                while let Some(s) = chars.next() {
                    out.push(s);
                    match s {
                        '\\' => out.extend(chars.next()),
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '{' | '[' => {
                out.push(c);
                let close = if c == '{' { '}' } else { ']' };
                while chars.peek().is_some_and(|n| n.is_whitespace()) {
                    chars.next();
                }
                if chars.peek() == Some(&close) {
                    // An empty container stays `{}` / `[]`.
                    out.extend(chars.next());
                } else {
                    depth += 1;
                    newline(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(',');
                newline(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    Some(out)
}

/// Does "Rapikan" have anything to do? True for valid JSON whose pretty form differs from the
/// text (a minified blob, ragged indentation).
pub fn needs_pretty(text: &str) -> bool {
    pretty(text).is_some_and(|p| p != text.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containers_only() {
        for ok in ["{}", "[]", r#"{"a":1}"#, " [1, 2, 3] ", r#"{"a":{"b":[1,{"c":null}]},"d":"x"}"#, "[\n  1\n]"] {
            assert!(is_container(ok), "{ok:?}");
        }
        for bad in ["", "{", "}", "123", r#""text""#, "true", "null", "{a:1}", "{'a':1}", r#"{"a":1,}"#, "[1,2", r#"{"a":1} trailing"#, "{} {}", "hello {world}", "[1, 2, 3"] {
            assert!(!is_container(bad), "{bad:?}");
        }
        let big = format!("[{}]", vec!["1"; 200_000].join(","));
        assert!(big.len() > MAX_BYTES && !is_container(&big), "over the size cap");
    }

    #[test]
    fn pretty_printing_keeps_order_and_literals() {
        let src = r#"{"zeta":1,"alpha":[1,2.50,{"k":"v"}],"big":12345678901234567890123,"s":"a,b:{c}\"q\\","e":{},"a":[],"n":null,"t":true}"#;
        let want = "{\n  \"zeta\": 1,\n  \"alpha\": [\n    1,\n    2.50,\n    {\n      \"k\": \"v\"\n    }\n  ],\n  \"big\": 12345678901234567890123,\n  \"s\": \"a,b:{c}\\\"q\\\\\",\n  \"e\": {},\n  \"a\": [],\n  \"n\": null,\n  \"t\": true\n}";
        assert_eq!(pretty(src).as_deref(), Some(want));
        // Idempotent: pretty text is already pretty.
        assert_eq!(pretty(want).as_deref(), Some(want));
        assert!(!needs_pretty(want));
        assert!(needs_pretty(src));
    }

    #[test]
    fn small_cases() {
        assert_eq!(pretty("[1,2]").as_deref(), Some("[\n  1,\n  2\n]"));
        assert_eq!(pretty("  { }  ").as_deref(), Some("{}"));
        assert_eq!(pretty("[ [ ] , { } ]").as_deref(), Some("[\n  [],\n  {}\n]"));
        assert_eq!(pretty(r#"{"a" : 1 , "b":[ ]}"#).as_deref(), Some("{\n  \"a\": 1,\n  \"b\": []\n}"));
        // Unicode and escapes inside strings are untouched.
        let esc = format!("{}u00e9", '\\');
        let src = format!("[\"caf{esc} \\\" \u{e9} \u{1F600}\"]");
        let want = format!("[\n  \"caf{esc} \\\" \u{e9} \u{1F600}\"\n]");
        assert_eq!(pretty(&src).as_deref(), Some(want.as_str()));
        assert_eq!(pretty("not json"), None);
        assert_eq!(pretty("42"), None);
        assert!(!needs_pretty("{}"), "already minimal");
    }

    #[test]
    fn the_output_parses_back_to_the_same_value() {
        let src = r#"{"b":[1,2,{"x":"y z"}],"a":{"n":-1.5e3,"s":"\n\t"}}"#;
        let a: serde_json::Value = serde_json::from_str(src).unwrap();
        let b: serde_json::Value = serde_json::from_str(&pretty(src).unwrap()).unwrap();
        assert_eq!(a, b);
    }
}
