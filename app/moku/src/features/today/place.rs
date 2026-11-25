//! The Hari ini city as coordinates, shared by the services that need a place (weather, BMKG
//! alerts, air quality): the config's own latitude/longitude, else the Open-Meteo geocoding of the
//! city name, asked once per city name for the whole process. **A failed lookup is remembered too**, so
//! the services that poll every few minutes do not ask again for every poll: a city Open-Meteo does not
//! know stays unknown until the city name changes, and a network error is not retried for
//! [`RETRY_AFTER`] (the PC may simply be offline right now).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use moku_core::config::Today;

use crate::services::ServiceCtx;

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// A network error is not retried for this long.
const RETRY_AFTER: Duration = Duration::from_secs(10 * 60);

/// What the geocoder said about a city.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    Found(f64, f64),
    /// Open-Meteo answered and does not know the city: final until the name changes.
    Unknown,
    /// The request failed (offline, a server error): try again after this moment.
    Failed(Instant),
}

/// What the cache knows about `city`: the answer, or that it has to ask.
#[derive(Clone, Debug, PartialEq)]
pub enum Lookup {
    Known(Result<(f64, f64), String>),
    Ask,
}

/// Pure: what to do for `city` given the last answer (`cache` = the city it was for and the answer) at `now`.
pub fn lookup(cache: Option<&(String, Answer)>, city: &str, now: Instant) -> Lookup {
    match cache {
        Some((c, Answer::Found(lat, lon))) if c == city => Lookup::Known(Ok((*lat, *lon))),
        Some((c, Answer::Unknown)) if c == city => Lookup::Known(Err("city not found".into())),
        Some((c, Answer::Failed(until))) if c == city && now < *until => Lookup::Known(Err("geocoding failed recently".into())),
        _ => Lookup::Ask,
    }
}

/// The last answer, with the city it is for. Held while a geocoding request runs, so services that start
/// together ask Open-Meteo once, not once each.
static CACHE: Mutex<Option<(String, Answer)>> = Mutex::new(None);

pub struct Place {
    pub lat: f64,
    pub lon: f64,
    /// This call asked the geocoder (the caller shares the result with the UI thread).
    pub fresh: bool,
}

/// The position of `t.city`. Blocking (network): services only.
pub fn resolve(t: &Today) -> Result<Place, String> {
    if let (Some(lat), Some(lon)) = (t.latitude, t.longitude) {
        return Ok(Place { lat, lon, fresh: false });
    }
    let mut cache = CACHE.lock().map_err(|_| "place cache poisoned".to_string())?;
    match lookup(cache.as_ref(), &t.city, Instant::now()) {
        Lookup::Known(Ok((lat, lon))) => return Ok(Place { lat, lon, fresh: false }),
        Lookup::Known(Err(e)) => return Err(e),
        Lookup::Ask => {}
    }
    let body = match crate::net::get_text(&super::geocode_url(&t.city), HTTP_TIMEOUT) {
        Ok(b) => b,
        Err(e) => {
            *cache = Some((t.city.clone(), Answer::Failed(Instant::now() + RETRY_AFTER)));
            return Err(e);
        }
    };
    let Some((lat, lon)) = super::parse_geocode(&body) else {
        *cache = Some((t.city.clone(), Answer::Unknown));
        return Err("city not found".into());
    };
    *cache = Some((t.city.clone(), Answer::Found(lat, lon)));
    Ok(Place { lat, lon, fresh: true })
}

/// [`resolve`] for the current config of a service, handing a fresh geocoding to the UI thread
/// (the prayer times may be waiting for it).
pub fn resolve_and_publish(ctx: &ServiceCtx) -> Result<(f64, f64), String> {
    let cfg = ctx.config();
    let t = &cfg.today;
    let p = resolve(t)?;
    if p.fresh {
        let g = (t.city.clone(), p.lat, p.lon);
        ctx.post(move |app| {
            app.state_mut::<super::TodayState>().geo = Some(g);
            crate::features::prayer::refresh(app);
        });
    }
    Ok((p.lat, p.lon))
}

/// What the process-wide cache holds for `city`. Never blocks (the UI thread uses it): while a
/// geocoding request is running there is nothing yet.
pub fn cached(city: &str) -> Option<(f64, f64)> {
    let cache = CACHE.try_lock().ok()?;
    match cache.as_ref() {
        Some((c, Answer::Found(lat, lon))) if c == city => Some((*lat, *lon)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_found_city_is_asked_once_and_a_failure_is_remembered_until_the_city_changes() {
        let now = Instant::now();
        // Nothing known: ask.
        assert_eq!(lookup(None, "Jakarta", now), Lookup::Ask);
        // Found: never asked again for that city, asked for another.
        let found = ("Jakarta".to_string(), Answer::Found(-6.2, 106.8));
        assert_eq!(lookup(Some(&found), "Jakarta", now), Lookup::Known(Ok((-6.2, 106.8))));
        assert_eq!(lookup(Some(&found), "Surabaya", now), Lookup::Ask);
        // A city the geocoder does not know: final, for any later time, until the name changes.
        let unknown = ("Kotafiktif".to_string(), Answer::Unknown);
        for later in [now, now + Duration::from_secs(3600), now + Duration::from_secs(86_400)] {
            assert_eq!(lookup(Some(&unknown), "Kotafiktif", later), Lookup::Known(Err("city not found".into())), "{later:?}");
        }
        assert_eq!(lookup(Some(&unknown), "Jakarta", now), Lookup::Ask, "the city changed");
        // A network error: not retried at once (a service polling every 2 minutes would hammer the geocoder),
        // retried after the back-off.
        let failed = ("Jakarta".to_string(), Answer::Failed(now + RETRY_AFTER));
        assert!(matches!(lookup(Some(&failed), "Jakarta", now + Duration::from_secs(120)), Lookup::Known(Err(_))));
        assert!(matches!(lookup(Some(&failed), "Jakarta", now + Duration::from_secs(599)), Lookup::Known(Err(_))));
        assert_eq!(lookup(Some(&failed), "Jakarta", now + RETRY_AFTER), Lookup::Ask);
        assert_eq!(lookup(Some(&failed), "Bandung", now), Lookup::Ask, "another city is asked at once");
    }
}
