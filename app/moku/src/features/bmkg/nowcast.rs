//! BMKG weather warnings ("nowcast", Peringatan Dini Cuaca): the public RSS feed lists the alerts
//! in force across Indonesia, one `<item>` per alert with the province in the title. Pure; the
//! tests run on a live sample saved in `fixtures/`.
//!
//! The feed is XML and the part we need is small and regular (an `<item>` list of plain text
//! tags), so it is read with a few string searches instead of an XML crate.

use chrono::{DateTime, NaiveDate};

pub const RSS_URL: &str = "https://www.bmkg.go.id/alerts/nowcast/id";

/// Most alerts read from one feed.
const MAX_ITEMS: usize = 300;
/// An alert that does not say when it ends is shown this long after it was issued.
const DEFAULT_VALID_SECS: i64 = 3 * 3600;
/// Most event times / ids remembered as already shown.
pub const MAX_SEEN: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub struct Warning {
    /// `guid` (or the link): the identity of the alert.
    pub id: String,
    /// "Hujan Lebat disertai Petir".
    pub event: String,
    /// "Nusa Tenggara Barat", as BMKG spells it ("Kep. Riau").
    pub province: String,
    /// The full text.
    pub text: String,
    /// Issued, Unix seconds (`pubDate`).
    pub published: i64,
    /// Ends, Unix seconds (from "hingga 25 November 2026, 03:30 WITA"), when the text says.
    pub until: Option<i64>,
}

impl Warning {
    /// Still in force at `now`.
    pub fn active(&self, now: i64) -> bool {
        now < self.until.unwrap_or(self.published + DEFAULT_VALID_SECS)
    }

    /// Does the text name the user's city or district ("khususnya di JEROWARU, LABUHAN BADAS")?
    pub fn mentions(&self, place: &str) -> bool {
        let p = place.trim().to_lowercase();
        p.chars().count() >= 4 && self.text.to_lowercase().contains(&p)
    }
}

/// The text of the first `<name>...</name>` in `block`, entities decoded, CDATA unwrapped.
fn tag(block: &str, name: &str) -> Option<String> {
    let open = format!("<{name}");
    let mut from = 0;
    let start = loop {
        let i = from + block[from..].find(&open)?;
        let after = block.as_bytes().get(i + open.len()).copied();
        // `<title>` and `<title attr>`, not `<titles>`.
        if matches!(after, Some(b'>') | Some(b' ') | Some(b'\n') | Some(b'\r') | Some(b'\t')) {
            break i;
        }
        from = i + open.len();
    };
    let body_start = start + block[start..].find('>')? + 1;
    let end = body_start + block[body_start..].find(&format!("</{name}>"))?;
    Some(unescape(block[body_start..end].trim()))
}

fn unescape(s: &str) -> String {
    let s = s.strip_prefix("<![CDATA[").and_then(|x| x.strip_suffix("]]>")).map(str::to_string).unwrap_or_else(|| {
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(i) = rest.find('&') {
            out.push_str(&rest[..i]);
            rest = &rest[i..];
            let Some(semi) = rest.find(';').filter(|e| *e <= 10) else {
                out.push('&');
                rest = &rest[1..];
                continue;
            };
            let ent = &rest[1..semi];
            let ch = match ent {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => ent.strip_prefix('#').and_then(|n| match n.strip_prefix(['x', 'X']) {
                    Some(h) => u32::from_str_radix(h, 16).ok(),
                    None => n.parse().ok(),
                }).and_then(char::from_u32),
            };
            match ch {
                Some(c) => {
                    out.push(c);
                    rest = &rest[semi + 1..];
                }
                None => {
                    out.push('&');
                    rest = &rest[1..];
                }
            }
        }
        out.push_str(rest);
        out
    });
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn month(name: &str) -> Option<u32> {
    const M: [&str; 12] = ["januari", "februari", "maret", "april", "mei", "juni", "juli", "agustus", "september", "oktober", "november", "desember"];
    let n = name.trim_matches(|c: char| !c.is_alphabetic()).to_lowercase();
    M.iter().position(|m| *m == n).map(|i| i as u32 + 1)
}

/// "... hingga 25 November 2026, 03:30 WITA." -> Unix seconds. WIB is UTC+7, WITA +8, WIT +9.
pub fn parse_until(text: &str) -> Option<i64> {
    let rest = &text[text.rfind("hingga ")? + "hingga ".len()..];
    let mut it = rest.split_whitespace();
    let day: u32 = it.next()?.parse().ok()?;
    let mo = month(it.next()?)?;
    let year: i32 = it.next()?.trim_end_matches(',').parse().ok()?;
    let (h, m) = it.next()?.trim_end_matches(',').split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.trim_end_matches(|c: char| !c.is_ascii_digit()).parse().ok()?);
    let offset = match it.next().map(|z| z.trim_matches(|c: char| !c.is_alphabetic()).to_ascii_uppercase()).as_deref() {
        Some("WITA") => 8,
        Some("WIT") => 9,
        _ => 7,
    };
    let local = NaiveDate::from_ymd_opt(year, mo, day)?.and_hms_opt(h, m, 0)?;
    Some(local.and_utc().timestamp() - offset * 3600)
}

/// "Hujan Lebat disertai Petir di Kep. Riau" -> ("Hujan Lebat disertai Petir", "Kep. Riau").
fn split_title(title: &str) -> (String, String) {
    match title.rfind(" di ") {
        Some(i) => (title[..i].trim().to_string(), title[i + 4..].trim().to_string()),
        None => (title.trim().to_string(), String::new()),
    }
}

/// Every alert in the feed. Items without a title or a usable date are skipped.
pub fn parse_rss(xml: &str) -> Vec<Warning> {
    let mut out = vec![];
    let mut rest = xml;
    while out.len() < MAX_ITEMS {
        let Some(s) = rest.find("<item>") else { break };
        let after = &rest[s + "<item>".len()..];
        let Some(e) = after.find("</item>") else { break };
        let item = &after[..e];
        rest = &after[e + "</item>".len()..];
        let (Some(title), Some(date)) = (tag(item, "title"), tag(item, "pubDate")) else { continue };
        let Ok(published) = DateTime::parse_from_rfc2822(&date).map(|d| d.timestamp()) else { continue };
        let text = tag(item, "description").unwrap_or_default();
        let id = tag(item, "guid").or_else(|| tag(item, "link")).unwrap_or_else(|| title.clone());
        let (event, province) = split_title(&title);
        out.push(Warning { id, event, province, until: parse_until(&text), text, published });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // A live sample of the feed (2026-11-24, 17 alerts).
    const FEED: &str = include_str!("fixtures/nowcast-rss.xml");

    #[test]
    fn the_live_sample_parses() {
        let w = parse_rss(FEED);
        assert_eq!(w.len(), 17, "{} alerts", w.len());
        let ntb = &w[0];
        assert_eq!(ntb.event, "Hujan Lebat disertai Petir");
        assert_eq!(ntb.province, "Nusa Tenggara Barat");
        assert_eq!(ntb.id, "2.49.0.1.360.0.2026.11.24.17.52.010");
        // Wed, 25 Nov 2026 01:10:00 +0800 = 24 Nov 17:10 UTC.
        assert_eq!(ntb.published, NaiveDate::from_ymd_opt(2026, 11, 24).unwrap().and_hms_opt(17, 10, 0).unwrap().and_utc().timestamp());
        // "... hingga 25 November 2026, 03:30 WITA": 03:30 at UTC+8 is 19:30 UTC the day before.
        assert_eq!(ntb.until, Some(NaiveDate::from_ymd_opt(2026, 11, 24).unwrap().and_hms_opt(19, 30, 0).unwrap().and_utc().timestamp()));
        assert!(ntb.text.contains("JEROWARU") && ntb.text.starts_with("Hujan lebat disertai petir akan terjadi"));
        assert!(!ntb.text.contains('\n'), "whitespace is collapsed");
        // The abbreviated province keeps BMKG's spelling here (region.rs reads it).
        assert!(w.iter().any(|x| x.province == "Kep. Riau"));
        // Every alert has an id, a province and a date; the ids are distinct.
        assert!(w.iter().all(|x| !x.id.is_empty() && !x.province.is_empty() && x.published > 0));
        let ids: std::collections::HashSet<&str> = w.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids.len(), w.len());
        // Two alerts for the same province are two alerts.
        assert_eq!(w.iter().filter(|x| x.province == "Sumatera Selatan").count(), 2);
    }

    #[test]
    fn alerts_for_a_province() {
        let w = parse_rss(FEED);
        let riau: Vec<&Warning> = w.iter().filter(|x| moku_core::geo::same(&x.province, "Riau")).collect();
        assert_eq!(riau.len(), 1, "Riau is not Kep. Riau");
        let kepri: Vec<&Warning> = w.iter().filter(|x| moku_core::geo::same(&x.province, "Kepulauan Riau")).collect();
        assert_eq!(kepri.len(), 1);
        assert!(w.iter().all(|x| !moku_core::geo::same(&x.province, "DKI Jakarta")), "none in the sample");
    }

    #[test]
    fn how_long_an_alert_is_in_force() {
        let w = parse_rss(FEED);
        let ntb = &w[0];
        let until = ntb.until.unwrap();
        assert!(ntb.active(until - 1) && !ntb.active(until));
        // Without an end it is shown three hours after it was issued.
        let open = Warning { until: None, ..ntb.clone() };
        assert!(open.active(open.published + 3 * 3600 - 1) && !open.active(open.published + 3 * 3600));
    }

    #[test]
    fn the_end_time_in_each_zone() {
        let u = |s: &str| parse_until(s);
        let utc = |y, mo, d, h, mi| NaiveDate::from_ymd_opt(y, mo, d).unwrap().and_hms_opt(h, mi, 0).unwrap().and_utc().timestamp();
        assert_eq!(u("... dapat berlangsung hingga 25 November 2026, 03:30 WITA."), Some(utc(2026, 11, 24, 19, 30)));
        assert_eq!(u("hingga 1 Januari 2027, 00:15 WIB"), Some(utc(2026, 12, 31, 17, 15)));
        assert_eq!(u("hingga 5 Mei 2027, 12:00 WIT"), Some(utc(2027, 5, 5, 3, 0)));
        assert_eq!(u("hingga 5 Mei 2027, 12:00"), Some(utc(2027, 5, 5, 5, 0)), "no zone: WIB");
        // The last "hingga" counts.
        assert_eq!(u("sejak ... hingga 3 Mei 2027, 10:00 WIB. berlaku hingga 4 Mei 2027, 10:00 WIB"), Some(utc(2027, 5, 4, 3, 0)));
        for bad in ["", "hingga", "hingga besok", "hingga 31 Februari 2027, 10:00 WIB", "hingga 5 Mei 2027, 25:00 WIB", "hingga 5 Foo 2027, 10:00 WIB"] {
            assert_eq!(u(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn mentioning_the_city() {
        let w = parse_rss(FEED);
        assert!(w[0].mentions("Narmada"), "case does not matter");
        assert!(w[0].mentions("  sekotong "));
        assert!(!w[0].mentions("Jakarta"));
        assert!(!w[0].mentions("sek"), "three letters match too much");
        assert!(!w[0].mentions(""));
    }

    #[test]
    fn broken_feeds_give_nothing() {
        for s in ["", "garbage", "<rss></rss>", "<item>", "<item></item>", "<item><title>x</title></item>", "<item><title>x</title><pubDate>yesterday</pubDate></item>"] {
            assert!(parse_rss(s).is_empty(), "{s:?}");
        }
        // One bad item does not hide the next.
        let two = "<item><title>bad</title></item><item><title>Hujan Lebat di Bali</title><pubDate>Wed, 25 Nov 2026 01:10:00 +0800</pubDate><guid>g1</guid></item>";
        let w = parse_rss(two);
        assert_eq!(w.len(), 1);
        assert_eq!((w[0].event.as_str(), w[0].province.as_str(), w[0].id.as_str(), w[0].until), ("Hujan Lebat", "Bali", "g1", None));
    }

    #[test]
    fn entities_and_cdata() {
        let item = "<item><title><![CDATA[Angin Kencang & Petir di Bali]]></title><pubDate>Wed, 25 Nov 2026 01:10:00 +0800</pubDate><description>a &amp; b &lt;c&gt; &#65;&#x42; &quot;q&quot; &bogus; &</description><link>l</link></item>";
        let w = parse_rss(item);
        assert_eq!(w[0].event, "Angin Kencang & Petir");
        assert_eq!(w[0].text, "a & b <c> AB \"q\" &bogus; &");
        assert_eq!(w[0].id, "l", "the link stands in for a missing guid");
        // A tag that only starts with the name is not it.
        assert_eq!(tag("<titles>no</titles><title>yes</title>", "title").as_deref(), Some("yes"));
        assert_eq!(tag("<title>x", "title"), None);
    }

    #[test]
    fn a_huge_feed_is_bounded() {
        let one = "<item><title>H di Bali</title><pubDate>Wed, 25 Nov 2026 01:10:00 +0800</pubDate></item>";
        assert_eq!(parse_rss(&one.repeat(1000)).len(), MAX_ITEMS);
    }
}
