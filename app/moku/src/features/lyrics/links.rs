//! "Cari di Genius / Musixmatch": a search page of a lyrics site, opened in the user's own browser
//! when they press the button. Moku fetches nothing from these sites and sends them nothing: the
//! address is only handed to the shell (`ShellExecute`), like any link the user opens. Genius has
//! no timed lyrics and its text is not licensed for reuse, and Musixmatch needs a key that cannot
//! live in an open-source app, so they are not lookup sources, just a shortcut for reading.

use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

use super::title;
use crate::net;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Site {
    Genius,
    Musixmatch,
}

impl Site {
    /// The `arg` of the action (`media.lyrics_web:genius`).
    pub fn of(arg: &str) -> Option<Site> {
        match arg {
            "genius" => Some(Site::Genius),
            "musixmatch" => Some(Site::Musixmatch),
            _ => None,
        }
    }
}

/// "Artist Title" of the track as it should be searched for: the cleaned first artist and the bare
/// title (no "(Official Video)", no guests). Empty when there is no title.
pub fn search_text(artist: &str, title_: &str) -> String {
    let c = title::clean(title_, artist);
    if c.core.is_empty() {
        return String::new();
    }
    let a = title::first_artist(&c.artist);
    [a.as_str(), c.core.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" ")
}

/// The search page for `site`, or `None` when there is nothing to search for.
pub fn url(site: Site, artist: &str, title_: &str) -> Option<String> {
    let q = search_text(artist, title_);
    if q.is_empty() {
        return None;
    }
    let q = net::encode(&q);
    Some(match site {
        Site::Genius => format!("https://genius.com/search?q={q}"),
        Site::Musixmatch => format!("https://www.musixmatch.com/search/{q}"),
    })
}

/// Hand `url` to the shell (the default browser). Only the two https addresses of [`url`].
pub fn open(url: &str) -> bool {
    let ok = (url.starts_with("https://genius.com/search?q=") || url.starts_with("https://www.musixmatch.com/search/")) && !url.chars().any(|c| c.is_control() || c == ' ');
    if !ok {
        return false;
    }
    let h = unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(url), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    h.0 as isize > 32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_search_pages_carry_the_cleaned_artist_and_title_url_encoded() {
        assert_eq!(url(Site::Genius, "Crayon Case", "because,").as_deref(), Some("https://genius.com/search?q=Crayon%20Case%20because"));
        assert_eq!(url(Site::Musixmatch, "Crayon Case", "because,").as_deref(), Some("https://www.musixmatch.com/search/Crayon%20Case%20because"));
        // Upload noise, channel suffixes and guests are left out; "&" and non-ASCII are encoded.
        assert_eq!(url(Site::Genius, "Tulus - Topic", "Hati-Hati di Jalan [Official Lyric Video]").as_deref(), Some("https://genius.com/search?q=Tulus%20Hati-Hati%20di%20Jalan"));
        assert_eq!(url(Site::Genius, "Beyoncé & Jay-Z", "Crazy in Love (feat. X)").as_deref(), Some("https://genius.com/search?q=Beyonc%C3%A9%20Crazy%20in%20Love"));
        assert_eq!(url(Site::Musixmatch, "", "Coldplay - Yellow (Lyrics)").as_deref(), Some("https://www.musixmatch.com/search/Coldplay%20Yellow"));
        // A character that could end the query or start another one never survives raw.
        let u = url(Site::Genius, "A", "x&y=1#z?w").unwrap();
        assert!(u.matches('?').count() == 1 && !u.contains('&') && !u.contains('#') && !u.contains(' '), "{u}");
        // Nothing to search for: no link.
        assert_eq!(url(Site::Genius, "Someone", ""), None);
        assert_eq!(url(Site::Musixmatch, "", ""), None);
    }

    #[test]
    fn only_the_two_known_sites_are_named_by_an_action() {
        assert_eq!(Site::of("genius"), Some(Site::Genius));
        assert_eq!(Site::of("musixmatch"), Some(Site::Musixmatch));
        for bad in ["", "Genius", "https://evil.example", "genius.com", "google"] {
            assert_eq!(Site::of(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn only_those_search_pages_are_ever_opened() {
        // (`open` itself is not called here: it would start a browser.)
        for site in [Site::Genius, Site::Musixmatch] {
            let u = url(site, "A", "B").unwrap();
            assert!(u.starts_with("https://"), "{u}");
        }
        assert!(!open("http://genius.com/search?q=x"));
        assert!(!open("https://evil.example/search?q=x"));
        assert!(!open("file:///C:/Windows/notepad.exe"));
        assert!(!open("https://genius.com/search?q=a b"));
    }
}
