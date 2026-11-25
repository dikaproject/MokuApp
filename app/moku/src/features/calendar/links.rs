//! Finding a video-call link (Google Meet, Zoom, Microsoft Teams) in an event's text.
//!
//! The link is opened with `ShellExecute` when the user presses "Gabung", so only `https` links
//! whose host is one of the known services are ever returned: an address inside a calendar file
//! is data from outside, and nothing else in it is ever launched.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    Meet,
    Zoom,
    Teams,
}

impl Service {
    /// The name shown next to the time of an event.
    pub fn label(self) -> &'static str {
        match self {
            Service::Meet => "Meet",
            Service::Zoom => "Zoom",
            Service::Teams => "Teams",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingLink {
    pub url: String,
    pub service: Service,
}

/// Bare hosts that count as a link even without `https://` in front (Google writes
/// `meet.google.com/abc-defg-hij` in some descriptions).
const BARE: &[&str] = &["meet.google.com/", "zoom.us/", "teams.microsoft.com/", "teams.live.com/"];

fn host_service(host: &str) -> Option<Service> {
    let h = host.to_ascii_lowercase();
    match h.as_str() {
        "meet.google.com" => Some(Service::Meet),
        "teams.microsoft.com" | "teams.live.com" => Some(Service::Teams),
        "zoom.us" => Some(Service::Zoom),
        _ if h.ends_with(".zoom.us") || h.ends_with(".zoomgov.com") || h == "zoomgov.com" => Some(Service::Zoom),
        _ => None,
    }
}

/// `https://host[:port]/path` → (host, has a path). Rejects user-info (`https://zoom.us@evil/`),
/// other ports and anything that is not plain `https`.
fn split_https(url: &str) -> Option<(&str, bool)> {
    let rest = url.get(..8).filter(|p| p.eq_ignore_ascii_case("https://")).map(|_| &url[8..])?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.is_empty() || authority.contains(['@', ' ', '\\']) {
        return None;
    }
    let host = match authority.rsplit_once(':') {
        Some((h, "443")) => h,
        Some(_) => return None,
        None => authority,
    };
    let path = &rest[end..];
    Some((host, path.len() > 1))
}

/// Characters that end a URL inside running text, HTML or Markdown.
fn is_end(c: char) -> bool {
    c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '<' | '>' | ')' | ']' | '}' | '\\' | '|' | '`')
}

/// Every candidate address in `text`, in order: `http(s)://…` runs and bare known hosts.
fn candidates(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut starts: Vec<usize> = vec![];
    for pat in ["https://", "http://"] {
        starts.extend(lower.match_indices(pat).map(|(i, _)| i));
    }
    for pat in BARE {
        for (i, _) in lower.match_indices(pat) {
            // A bare host only when it starts a word (not `evil-zoom.us/`, not `x.zoom.us/`).
            let before = lower[..i].chars().next_back();
            if before.is_none_or(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '/' | '@' | ':'))) {
                starts.push(i);
            }
        }
    }
    starts.sort_unstable();
    starts.dedup();
    let mut out = vec![];
    for s in starts {
        let rest = &text[s..];
        let end = rest.find(is_end).unwrap_or(rest.len());
        let raw = rest[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', '*']);
        if raw.is_empty() {
            continue;
        }
        let lraw = raw.to_ascii_lowercase();
        out.push(if lraw.starts_with("http://") {
            // A plain-http meeting link is upgraded, never opened as is.
            format!("https://{}", &raw[7..])
        } else if lraw.starts_with("https://") {
            format!("https://{}", &raw[8..])
        } else {
            format!("https://{raw}")
        });
    }
    out
}

/// The first meeting link in `texts` (searched in the order given).
pub fn find(texts: &[&str]) -> Option<MeetingLink> {
    for t in texts {
        // Event texts are small; a pathological one is not worth scanning in full.
        let t = if t.len() > 32 * 1024 { &t[..t.floor_char_boundary(32 * 1024)] } else { t };
        for url in candidates(t) {
            if url.len() > 1024 {
                continue;
            }
            if let Some((host, true)) = split_https(&url) {
                if let Some(service) = host_service(host) {
                    return Some(MeetingLink { url, service });
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(texts: &[&str]) -> Option<String> {
        find(texts).map(|l| l.url)
    }

    #[test]
    fn finds_google_meet_zoom_and_teams() {
        assert_eq!(find(&["https://meet.google.com/abc-defg-hij"]), Some(MeetingLink { url: "https://meet.google.com/abc-defg-hij".into(), service: Service::Meet }));
        let z = find(&["https://us02web.zoom.us/j/81234567890?pwd=AbCdEf"]).unwrap();
        assert_eq!((z.service, z.url.as_str()), (Service::Zoom, "https://us02web.zoom.us/j/81234567890?pwd=AbCdEf"));
        let t = find(&["https://teams.microsoft.com/l/meetup-join/19%3ameeting_abc%40thread.v2/0?context=%7b%7d"]).unwrap();
        assert_eq!(t.service, Service::Teams);
        assert_eq!(find(&["https://teams.live.com/meet/9876543210"]).unwrap().service, Service::Teams);
        assert_eq!(find(&["https://company.zoom.us/my/room"]).unwrap().service, Service::Zoom);
        assert_eq!(find(&["https://HTTPS-less.example", "HTTPS://MEET.GOOGLE.COM/aaa-bbbb-ccc"]).unwrap().service, Service::Meet);
    }

    #[test]
    fn finds_links_inside_running_text_and_markup() {
        let desc = "Join with Google Meet: https://meet.google.com/abc-defg-hij.\nOr call +1 555 0100, PIN: 123 456#";
        assert_eq!(url(&[desc]).as_deref(), Some("https://meet.google.com/abc-defg-hij"));
        let outlook = "<a href=\"https://teams.microsoft.com/l/meetup-join/xyz/0\">Join the meeting now</a><br>";
        assert_eq!(url(&[outlook]).as_deref(), Some("https://teams.microsoft.com/l/meetup-join/xyz/0"));
        assert_eq!(url(&["(see https://zoom.us/j/123456789)"]).as_deref(), Some("https://zoom.us/j/123456789"));
        assert_eq!(url(&["Link: <https://zoom.us/j/55>, thanks"]).as_deref(), Some("https://zoom.us/j/55"));
        // Bare host, no scheme.
        assert_eq!(url(&["Meet: meet.google.com/abc-defg-hij"]).as_deref(), Some("https://meet.google.com/abc-defg-hij"));
        assert_eq!(url(&["zoom.us/j/777"]).as_deref(), Some("https://zoom.us/j/777"));
        // Plain http is upgraded.
        assert_eq!(url(&["http://zoom.us/j/888"]).as_deref(), Some("https://zoom.us/j/888"));
    }

    #[test]
    fn searches_the_texts_in_order_and_skips_other_links() {
        assert_eq!(url(&["Room 4B", "https://docs.example.com/agenda https://zoom.us/j/1", "https://meet.google.com/aaa-bbbb-ccc"]).as_deref(), Some("https://zoom.us/j/1"));
        assert_eq!(url(&["https://meet.google.com/first", "https://zoom.us/j/2"]).as_deref(), Some("https://meet.google.com/first"));
        assert_eq!(find(&["Kantor, lantai 3", "", "https://example.com/meet"]), None);
        assert_eq!(find(&[]), None);
    }

    #[test]
    fn lookalike_hosts_and_unsafe_urls_are_rejected() {
        for bad in [
            "https://evil.example/zoom.us/j/123",
            "https://zoom.us.evil.example/j/123",
            "https://notzoom.us/j/123",
            "https://zoom.us@evil.example/j/123",
            "https://meet.google.com.evil.example/abc",
            "https://evilmeet.google.com/abc",
            "https://zoom.us:8443/j/1",
            "https://zoom.us/",
            "https://zoom.us",
            "ftp://zoom.us/j/1",
            "javascript:alert(1)//zoom.us/j/1",
            "file:///C:/Windows/System32/calc.exe",
            "https://teams.microsoft.com.evil.example/l/meetup-join/1",
            "evil-zoom.us/j/1",
            "https://zoom.us\\@evil.example/j/1",
        ] {
            assert_eq!(find(&[bad]), None, "{bad}");
        }
    }

    #[test]
    fn huge_or_odd_text_is_safe() {
        assert_eq!(find(&[&"x".repeat(200_000)]), None);
        let long = format!("{} https://zoom.us/j/9", "é".repeat(40_000));
        assert_eq!(find(&[&long]), None, "beyond the scanned prefix");
        assert_eq!(find(&["https://", "http://", "https://zoom.us/j/\u{0}"]).map(|l| l.service), Some(Service::Zoom));
        let _ = find(&["https://\u{1F600}.zoom.us/j/1"]);
    }
}
