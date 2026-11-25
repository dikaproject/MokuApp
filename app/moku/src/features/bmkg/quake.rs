//! BMKG earthquake feeds: parsing, distance, thresholds and de-duplication. Pure (no clock, no
//! network, no UI); the tests run on the live samples saved in `fixtures/`.
//!
//! Two public feeds, both small JSON files: `autogempa.json` (the latest quake of M5 or more, with
//! the tsunami "Potensi") and `gempadirasakan.json` (the latest felt quakes, with where they were
//! felt). A quake is identified by its event time, so the same event in both feeds, or in two
//! polls, is one quake.

use std::collections::BTreeSet;

use chrono::DateTime;

pub const AUTOGEMPA_URL: &str = "https://data.bmkg.go.id/DataMKG/TEWS/autogempa.json";
pub const DIRASAKAN_URL: &str = "https://data.bmkg.go.id/DataMKG/TEWS/gempadirasakan.json";

/// A quake older than this when it is first seen raises no pill (the island was off, or the
/// network was down: it is news from hours ago).
pub const MAX_AGE_SECS: i64 = 3 * 3600;
/// Most quakes kept from the two feeds.
const MAX_QUAKES: usize = 40;
/// Most event times remembered as already shown.
pub const MAX_SEEN: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub struct Quake {
    /// Event time (the `DateTime` field, UTC), Unix seconds. The identity of the quake.
    pub time: i64,
    pub lat: f64,
    pub lon: f64,
    pub magnitude: f64,
    pub depth_km: Option<f64>,
    /// `Wilayah`: where, in BMKG's words ("Pusat gempa berada di laut 98 km BaratDaya Melonguane").
    pub region: String,
    /// `Potensi`: the tsunami line of the autogempa feed.
    pub potential: Option<String>,
    /// `Dirasakan`: where it was felt (felt feed).
    pub felt: Option<String>,
}

fn text(v: &serde_json::Value, key: &str) -> Option<String> {
    let s = v.get(key)?.as_str()?.trim();
    (!s.is_empty() && s != "-").then(|| s.to_string())
}

/// `"1.44 LU"`, `"2.29 LS"`, `"131.96 BT"`, `"98.7 BB"` -> signed degrees.
fn hemisphere(s: &str) -> Option<f64> {
    let mut it = s.split_whitespace();
    let n: f64 = it.next()?.replace(',', ".").parse().ok()?;
    Some(match it.next()?.to_ascii_uppercase().as_str() {
        "LU" | "BT" => n,
        "LS" | "BB" => -n,
        _ => return None,
    })
}

/// The position of an event: `Coordinates` ("-2.29,140.30", signed), else `Lintang` / `Bujur`.
fn position(v: &serde_json::Value) -> Option<(f64, f64)> {
    let from_pair = text(v, "Coordinates").and_then(|c| {
        let (a, b) = c.split_once(',')?;
        Some((a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?))
    });
    let (lat, lon) = from_pair.or_else(|| Some((hemisphere(&text(v, "Lintang")?)?, hemisphere(&text(v, "Bujur")?)?)))?;
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

fn quake(v: &serde_json::Value) -> Option<Quake> {
    let time = DateTime::parse_from_rfc3339(&text(v, "DateTime")?).ok()?.timestamp();
    let (lat, lon) = position(v)?;
    let magnitude: f64 = text(v, "Magnitude")?.replace(',', ".").parse().ok().filter(|m: &f64| m.is_finite() && (0.0..=10.0).contains(m))?;
    let depth_km = text(v, "Kedalaman").and_then(|d| d.split_whitespace().next().and_then(|n| n.replace(',', ".").parse().ok()));
    let region = text(v, "Wilayah").unwrap_or_default();
    let region = region.strip_prefix("Pusat gempa berada di ").map(str::to_string).unwrap_or(region);
    Some(Quake { time, lat, lon, magnitude, depth_km, region, potential: text(v, "Potensi"), felt: text(v, "Dirasakan") })
}

/// `autogempa.json`: one quake (`Infogempa.gempa` is an object).
pub fn parse_autogempa(json: &str) -> Option<Quake> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    quake(v.pointer("/Infogempa/gempa")?)
}

/// `gempadirasakan.json`: the felt quakes (`Infogempa.gempa` is an array); broken entries are skipped.
pub fn parse_dirasakan(json: &str) -> Vec<Quake> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return vec![] };
    v.pointer("/Infogempa/gempa").and_then(|g| g.as_array()).map(|a| a.iter().filter_map(quake).collect()).unwrap_or_default()
}

/// Both feeds as one list, newest first, one entry per event time (a quake that is in both keeps
/// the tsunami line of one and the felt line of the other).
pub fn merge(auto: Option<Quake>, felt: Vec<Quake>) -> Vec<Quake> {
    let mut all: Vec<Quake> = vec![];
    for q in auto.into_iter().chain(felt) {
        match all.iter_mut().find(|o| o.time == q.time) {
            Some(o) => {
                if o.potential.is_none() {
                    o.potential = q.potential;
                }
                if o.felt.is_none() {
                    o.felt = q.felt;
                }
            }
            None => all.push(q),
        }
    }
    all.sort_by(|a, b| b.time.cmp(&a.time));
    all.truncate(MAX_QUAKES);
    all
}

pub use moku_core::geo::haversine_km;

/// Does the `Potensi` line announce a tsunami? ("Berpotensi tsunami"; not "Tidak berpotensi
/// tsunami", "tak", "tdk", "bukan", "tanpa potensi", "no tsunami", and not the "dirasakan" notice.)
/// A negation anywhere in the line, as a whole word, means no warning.
pub fn tsunami_potential(potential: &str) -> bool {
    let p = potential.to_lowercase();
    if !p.contains("tsunami") {
        return false;
    }
    const NEGATIONS: &[&str] = &["tidak", "tdk", "tak", "bukan", "tanpa", "no", "not", "non", "without"];
    !p.split(|c: char| !c.is_alphabetic()).any(|w| NEGATIONS.contains(&w))
}

/// When a quake is worth a pill.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rules {
    /// Closer than this to the user's city (km).
    pub radius_km: f64,
    /// Or at least this strong, anywhere in the feeds.
    pub big_magnitude: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    Near,
    Big,
}

/// A quake that qualifies: how far it is from `home` (when known) and why it counts.
#[derive(Clone, Debug, PartialEq)]
pub struct Alert {
    pub quake: Quake,
    pub km: Option<f64>,
    pub why: Why,
}

pub fn qualify(q: &Quake, home: Option<(f64, f64)>, rules: &Rules) -> Option<Alert> {
    let km = home.map(|(la, lo)| haversine_km(la, lo, q.lat, q.lon));
    let why = if km.is_some_and(|d| d <= rules.radius_km) {
        Why::Near
    } else if q.magnitude >= rules.big_magnitude {
        Why::Big
    } else {
        return None;
    };
    Some(Alert { quake: q.clone(), km, why })
}

/// Fresh enough to announce: not older than [`MAX_AGE_SECS`], not from the future (a slow clock
/// is forgiven ten minutes).
pub fn fresh(q: &Quake, now: i64) -> bool {
    now - q.time <= MAX_AGE_SECS && q.time - now <= 600
}

/// The quakes to announce now: qualifying, fresh and not announced before (`seen` holds event
/// times), the strongest first (then the newest).
pub fn new_alerts(quakes: &[Quake], home: Option<(f64, f64)>, rules: &Rules, seen: &BTreeSet<i64>, now: i64) -> Vec<Alert> {
    let mut out: Vec<Alert> = quakes.iter().filter(|q| fresh(q, now) && !seen.contains(&q.time)).filter_map(|q| qualify(q, home, rules)).collect();
    out.sort_by(|a, b| b.quake.magnitude.total_cmp(&a.quake.magnitude).then(b.quake.time.cmp(&a.quake.time)));
    out
}

/// Add event times to the set of announced ones, forgetting the oldest beyond [`MAX_SEEN`].
pub fn remember(seen: &mut BTreeSet<i64>, times: impl IntoIterator<Item = i64>) {
    seen.extend(times);
    while seen.len() > MAX_SEEN {
        let oldest = *seen.iter().next().unwrap_or(&0);
        seen.remove(&oldest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // One live sample of each feed (2026-11-24).
    const AUTO: &str = include_str!("fixtures/autogempa.json");
    const FELT: &str = include_str!("fixtures/gempadirasakan.json");

    const JAKARTA: (f64, f64) = (-6.2088, 106.8456);

    fn rules() -> Rules {
        Rules { radius_km: 300.0, big_magnitude: 6.0 }
    }

    fn q(time: i64, lat: f64, lon: f64, mag: f64) -> Quake {
        Quake { time, lat, lon, magnitude: mag, depth_km: Some(10.0), region: "x".into(), potential: None, felt: None }
    }

    #[test]
    fn the_autogempa_sample_parses() {
        let a = parse_autogempa(AUTO).expect("the live sample");
        assert_eq!(a.magnitude, 5.3);
        assert_eq!((a.lat, a.lon), (1.44, 131.96));
        assert_eq!(a.depth_km, Some(10.0));
        assert_eq!(a.time, DateTime::parse_from_rfc3339("2026-11-23T11:24:32+00:00").unwrap().timestamp());
        assert_eq!(a.region, "241 km TimurLaut RAJAAMPAT-PAPUABRTDY");
        assert_eq!(a.potential.as_deref(), Some("Tidak berpotensi tsunami"));
        assert_eq!(a.felt, None, "'-' means nobody reported it");
    }

    #[test]
    fn the_felt_sample_parses_every_entry() {
        let l = parse_dirasakan(FELT);
        assert!(l.len() >= 10, "the live feed lists about 15 quakes: {}", l.len());
        let first = &l[0];
        assert_eq!(first.magnitude, 5.4);
        assert_eq!((first.lat, first.lon), (3.12, 126.57));
        assert_eq!(first.region, "laut 98 km BaratDaya Melonguane", "the 'Pusat gempa berada di' lead is cut");
        assert_eq!(first.felt.as_deref(), Some("II Tahuna"));
        assert_eq!(first.potential, None, "the felt feed has no tsunami line");
        // Southern latitudes are negative.
        let sentani = l.iter().find(|q| q.region.contains("Sentani")).unwrap();
        assert_eq!((sentani.lat, sentani.lon), (-2.29, 140.30));
        // The M7.2 of the sample.
        assert!(l.iter().any(|q| q.magnitude == 7.2 && q.depth_km == Some(125.0)));
        // Newest first as in the file; every event has its own time.
        let times: std::collections::BTreeSet<i64> = l.iter().map(|q| q.time).collect();
        assert_eq!(times.len(), l.len());
    }

    #[test]
    fn broken_feeds_give_nothing() {
        for s in ["", "<html>", "{}", r#"{"Infogempa":{}}"#, r#"{"Infogempa":{"gempa":[]}}"#, r#"{"Infogempa":{"gempa":[{"Magnitude":"5"}]}}"#, r#"{"Infogempa":{"gempa":{"DateTime":"x"}}}"#] {
            assert_eq!(parse_autogempa(s), None, "{s:?}");
            assert!(parse_dirasakan(s).is_empty(), "{s:?}");
        }
        // One bad entry does not hide the good ones.
        let l = parse_dirasakan(r#"{"Infogempa":{"gempa":[{"x":1},{"DateTime":"2026-11-22T07:54:39+00:00","Coordinates":"3.12,126.57","Magnitude":"5.4"}]}}"#);
        assert_eq!(l.len(), 1);
        // Absurd values are dropped.
        let bad = |extra: &str| format!(r#"{{"Infogempa":{{"gempa":{{"DateTime":"2026-11-22T07:54:39+00:00",{extra}}}}}}}"#);
        assert_eq!(parse_autogempa(&bad(r#""Coordinates":"95,10","Magnitude":"5""#)), None, "latitude out of range");
        assert_eq!(parse_autogempa(&bad(r#""Coordinates":"1,10","Magnitude":"15""#)), None, "magnitude out of range");
        assert_eq!(parse_autogempa(&bad(r#""Coordinates":"1,10","Magnitude":"NaN""#)), None);
        assert!(parse_autogempa(&bad(r#""Coordinates":"1,10","Magnitude":"5,5""#)).is_some_and(|q| q.magnitude == 5.5), "a decimal comma is read");
    }

    #[test]
    fn coordinates_fall_back_to_the_hemisphere_fields() {
        let j = r#"{"Infogempa":{"gempa":{"DateTime":"2026-11-22T07:54:39+00:00","Lintang":"2.29 LS","Bujur":"140.30 BT","Magnitude":"4.8"}}}"#;
        let q = parse_autogempa(j).unwrap();
        assert_eq!((q.lat, q.lon), (-2.29, 140.30));
        let west = r#"{"Infogempa":{"gempa":{"DateTime":"2026-11-22T07:54:39+00:00","Lintang":"1.0 LU","Bujur":"98.7 BB","Magnitude":"4.8"}}}"#;
        assert_eq!(parse_autogempa(west).map(|q| q.lon), Some(-98.7));
    }

    #[test]
    fn haversine_known_distances() {
        // Jakarta - Bandung is about 116 km, Jakarta - Surabaya about 665 km, Jakarta - Medan about 1,420 km.
        let bandung = haversine_km(JAKARTA.0, JAKARTA.1, -6.9175, 107.6191);
        assert!((bandung - 116.0).abs() < 4.0, "{bandung}");
        let surabaya = haversine_km(JAKARTA.0, JAKARTA.1, -7.2575, 112.7521);
        assert!((surabaya - 665.0).abs() < 8.0, "{surabaya}");
        let medan = haversine_km(JAKARTA.0, JAKARTA.1, 3.5952, 98.6722);
        assert!((medan - 1_420.0).abs() < 25.0, "{medan}");
        // Zero, symmetric, one degree of latitude is about 111.2 km, antipodes about half the circumference.
        assert_eq!(haversine_km(1.0, 2.0, 1.0, 2.0), 0.0);
        assert!((haversine_km(1.0, 2.0, -5.0, 100.0) - haversine_km(-5.0, 100.0, 1.0, 2.0)).abs() < 1e-9);
        assert!((haversine_km(0.0, 0.0, 1.0, 0.0) - 111.2).abs() < 0.2);
        assert!((haversine_km(0.0, 0.0, 0.0, 180.0) - 20_015.0).abs() < 20.0);
        assert!(haversine_km(90.0, 0.0, -90.0, 0.0).is_finite());
    }

    #[test]
    fn thresholds_near_big_and_neither() {
        let r = rules();
        // 1 degree of latitude is 111 km: 2.5 degrees is ~278 km (inside), 2.8 degrees ~311 km (outside).
        let near = q(0, JAKARTA.0 - 2.5, JAKARTA.1, 4.1);
        let just_out = q(0, JAKARTA.0 - 2.8, JAKARTA.1, 4.1);
        assert_eq!(qualify(&near, Some(JAKARTA), &r).map(|a| a.why), Some(Why::Near));
        assert_eq!(qualify(&just_out, Some(JAKARTA), &r), None);
        let km = qualify(&near, Some(JAKARTA), &r).and_then(|a| a.km).unwrap();
        assert!((km - 278.0).abs() < 3.0, "{km}");
        // M6 anywhere in Indonesia, but not M5.9.
        let far = q(0, 1.44, 131.96, 6.0);
        assert_eq!(qualify(&far, Some(JAKARTA), &r).map(|a| a.why), Some(Why::Big));
        assert_eq!(qualify(&q(0, 1.44, 131.96, 5.9), Some(JAKARTA), &r), None);
        // Near and big: near (the pill leads with the distance).
        assert_eq!(qualify(&q(0, JAKARTA.0, JAKARTA.1, 6.5), Some(JAKARTA), &r).map(|a| a.why), Some(Why::Near));
        // The position of the user is unknown (city not found): only the big ones.
        assert_eq!(qualify(&near, None, &r), None);
        let big = qualify(&far, None, &r).unwrap();
        assert_eq!((big.why, big.km), (Why::Big, None));
        // The radius and the magnitude are the user's.
        let wide = Rules { radius_km: 500.0, big_magnitude: 5.0 };
        assert!(qualify(&just_out, Some(JAKARTA), &wide).is_some());
        assert!(qualify(&q(0, 1.44, 131.96, 5.0), Some(JAKARTA), &wide).is_some());
    }

    #[test]
    fn the_live_samples_through_the_thresholds() {
        let quakes = merge(parse_autogempa(AUTO), parse_dirasakan(FELT));
        let now = quakes[0].time + 60;
        // From Jakarta nothing in the sample is within 300 km; only the M7.2 qualifies (as "big").
        let a = new_alerts(&quakes, Some(JAKARTA), &rules(), &BTreeSet::new(), now);
        assert!(a.iter().all(|x| x.why == Why::Big && x.quake.magnitude >= 6.0), "{a:?}");
        // ... but that one is 4 days old in the sample: not fresh.
        assert!(a.is_empty() || a.iter().all(|x| fresh(&x.quake, now)));
        // From Jayapura (near the Sentani quake) the felt quake there qualifies as near.
        let jayapura = (-2.5337, 140.7181);
        let sentani_time = quakes.iter().find(|q| q.region.contains("Sentani")).unwrap().time;
        let a = new_alerts(&quakes, Some(jayapura), &rules(), &BTreeSet::new(), sentani_time + 600);
        assert!(a.iter().any(|x| x.why == Why::Near && x.quake.region.contains("Sentani")), "{a:?}");
    }

    #[test]
    fn a_quake_is_announced_once() {
        let now = 1_000_000;
        let a = q(now - 100, JAKARTA.0 - 1.0, JAKARTA.1, 4.5);
        let b = q(now - 50, JAKARTA.0 - 1.5, JAKARTA.1, 5.0);
        let list = vec![b.clone(), a.clone()];
        let mut seen = BTreeSet::new();
        let first = new_alerts(&list, Some(JAKARTA), &rules(), &seen, now);
        assert_eq!(first.iter().map(|x| x.quake.time).collect::<Vec<_>>(), [b.time, a.time], "the stronger one first");
        remember(&mut seen, first.iter().map(|x| x.quake.time));
        // The next poll returns the same events: nothing new. A new one appears alone.
        assert!(new_alerts(&list, Some(JAKARTA), &rules(), &seen, now + 120).is_empty());
        let c = q(now + 100, JAKARTA.0, JAKARTA.1 + 1.0, 3.9);
        let list = vec![c.clone(), b, a];
        let next = new_alerts(&list, Some(JAKARTA), &rules(), &seen, now + 200);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].quake.time, c.time);
    }

    #[test]
    fn old_quakes_raise_nothing() {
        let now = 10 * 86_400;
        let old = q(now - MAX_AGE_SECS - 1, JAKARTA.0, JAKARTA.1, 5.0);
        let edge = q(now - MAX_AGE_SECS, JAKARTA.0, JAKARTA.1, 5.0);
        let future = q(now + 601, JAKARTA.0, JAKARTA.1, 5.0);
        let skewed = q(now + 599, JAKARTA.0, JAKARTA.1, 5.0);
        assert!(!fresh(&old, now) && fresh(&edge, now) && !fresh(&future, now) && fresh(&skewed, now));
        assert!(new_alerts(&[old], Some(JAKARTA), &rules(), &BTreeSet::new(), now).is_empty());
    }

    #[test]
    fn the_same_event_in_both_feeds_is_one_quake() {
        let mut a = q(500, 1.0, 2.0, 5.2);
        a.potential = Some("Tidak berpotensi tsunami".into());
        let mut f = q(500, 1.0, 2.0, 5.2);
        f.felt = Some("III Padang".into());
        let other = q(400, 3.0, 4.0, 4.4);
        let m = merge(Some(a), vec![other.clone(), f]);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].time, 500, "newest first");
        assert_eq!((m[0].potential.as_deref(), m[0].felt.as_deref()), (Some("Tidak berpotensi tsunami"), Some("III Padang")));
        assert_eq!(m[1], other);
        // No autogempa (that feed failed): the felt ones still work.
        assert_eq!(merge(None, vec![other.clone()]), vec![other]);
        assert!(merge(None, vec![]).is_empty());
    }

    #[test]
    fn the_tsunami_line() {
        assert!(tsunami_potential("Berpotensi tsunami"));
        assert!(tsunami_potential("Gempa ini berpotensi tsunami untuk diteruskan pada masyarakat"));
        assert!(!tsunami_potential("Tidak berpotensi tsunami"));
        assert!(!tsunami_potential("tidak berpotensi tsunami"));
        for no in ["TIDAK BERPOTENSI TSUNAMI", "Tidak berpotensi  tsunami.", "Gempa ini tdk berpotensi tsunami", "Tak berpotensi tsunami", "Bukan gempa yang berpotensi tsunami", "Tanpa potensi tsunami", "No tsunami potential", "Not likely to cause a tsunami", "tidak\tberpotensi\ntsunami"] {
            assert!(!tsunami_potential(no), "{no:?}");
        }
        // The word "tidak" inside another word is not a negation ("potensial", "tsunami" itself have none).
        assert!(tsunami_potential("Berpotensi tsunami, segera menjauhi pantai"));
        assert!(tsunami_potential("BERPOTENSI TSUNAMI"));
        assert!(!tsunami_potential("Gempa ini dirasakan untuk diteruskan pada masyarakat"));
        assert!(!tsunami_potential(""));
    }

    #[test]
    fn the_seen_set_is_bounded_and_forgets_the_oldest() {
        let mut s = BTreeSet::new();
        remember(&mut s, 0..(MAX_SEEN as i64 + 10));
        assert_eq!(s.len(), MAX_SEEN);
        assert!(!s.contains(&0) && s.contains(&(MAX_SEEN as i64 + 9)));
    }
}
