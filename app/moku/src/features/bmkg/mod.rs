//! Feature `bmkg` (v0.3, track P): earthquake and weather alerts from BMKG, Indonesia's
//! meteorological and geophysical agency. Public data, no key and no account.
//!
//! * **Earthquakes** (`bmkg.quake`): `autogempa.json` and `gempadirasakan.json` every 2 minutes
//!   (two requests, far under BMKG's 60 per minute). A quake raises a pill when it is within
//!   `bmkg.radius_km` of the Hari ini city, or of magnitude `bmkg.big_magnitude` (6) or more
//!   anywhere; the pill shows the magnitude, where, the distance, the time, "Sumber: BMKG" and
//!   BMKG's tsunami line in red when it announces one. **A tsunami warning is its own activity**
//!   (`bmkg.tsunami`, persistent, priority 95): a later quake, however strong, can never replace or
//!   downgrade it, it is kept visible over a full-screen app and over `island.hide_apps`
//!   (`App::force_visible`), and it stays until the user closes it. A quake is identified by its event time, so it is shown once ([`quake`]); the times already shown
//!   are kept in `bmkg-seen.json`, so restarting does not repeat them. Quakes older than three
//!   hours raise nothing.
//! * **Weather warnings** (`bmkg.nowcast`): the nowcast RSS every 10 minutes; the alerts for the
//!   user's province (`bmkg.province`, else worked out from the city) raise a pill once each
//!   ([`nowcast`]).
//! * **Hari ini**: [`section`] shows the latest qualifying quake and the warnings in force, with
//!   the "Sumber: BMKG" credit; nothing is drawn when there is nothing to say.
//! * Both services are off when their switch is off: no request, no memory.
//!
//! Test hooks (`MOKU_DEBUG=1`): `mokuctl debug click bmkg.sample_quake[:near|:big|:tsunami]` and
//! `bmkg.sample_warning` feed invented data through the real code (nothing is saved).

mod nowcast;
mod quake;

use std::collections::BTreeSet;
use std::time::Duration;

use chrono::{Local, TimeZone};
use moku_core::geo;

use self::nowcast::Warning;
use self::quake::{Alert, Quake, Rules, Why};
use crate::activity::Activity;
use crate::app::{App, Redraw};
use crate::features::today;
use crate::i18n::{self, tr, tr_s};
use crate::registry::*;
use crate::services::ServiceCtx;
use crate::ui::*;

const QUAKE_EVERY: Duration = Duration::from_secs(2 * 60);
const NOWCAST_EVERY: Duration = Duration::from_secs(10 * 60);
/// A failing feed is asked less often, up to this.
const MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
/// Above the volume OSD and below a finished timer / to-do reminder.
const QUAKE_PRIORITY: u8 = 88;
/// A big quake far from the user's city (the "M 6 anywhere" rule): still news, but not an emergency.
const FAR_PRIORITY: u8 = 66;
/// A quake with a tsunami warning: over everything except an AI card.
const TSUNAMI_PRIORITY: u8 = 95;
/// Weather warnings sit between the meeting dots and a "wifi lost".
const WARNING_PRIORITY: u8 = 62;
/// The activity id of the tsunami warning (apart from `bmkg.quake`, so no later quake replaces it).
const TSUNAMI_ID: &str = "bmkg.tsunami";
const QUAKE_SECS: f32 = 30.0;
const WARNING_SECS: f32 = 20.0;
/// Test-hook pills stay long enough to screenshot.
const SAMPLE_SECS: f32 = 45.0;
const SEEN_FILE: &str = "bmkg-seen.json";
/// The Hari ini card shows a quake this long after it happened.
const CARD_AGE_SECS: i64 = 24 * 3600;

#[derive(Default)]
pub struct BmkgState {
    /// `bmkg-seen.json` was read.
    loaded: bool,
    seen_quakes: BTreeSet<i64>,
    /// Alert ids, oldest first.
    seen_warnings: Vec<String>,
    /// The last poll of the two earthquake feeds, newest first.
    quakes: Vec<Quake>,
    quake_failed: bool,
    /// The quake the pill shows.
    alert: Option<Alert>,
    /// The tsunami warning the (separate, persistent) tsunami pill shows.
    tsunami: Option<Alert>,
    /// One `force_visible(true)` is outstanding for the tsunami pill.
    forcing: bool,
    /// The alerts in force for the province, newest first.
    warnings: Vec<Warning>,
    /// The warning the pill shows.
    warning: Option<Warning>,
    /// Invented data from a test hook is on screen: do not save it.
    sample: bool,
}

pub fn register(r: &mut Registry) {
    r.add_state(BmkgState::default());
    r.on_action("bmkg", on_action);
    r.on_config(on_config);
    r.on_event(|app, ev| {
        // "Hapus semua data": the settings app deletes bmkg-seen.json right after.
        if *ev == AppEvent::DataCleared {
            app.cancel_save("bmkg");
            *app.state_mut::<BmkgState>() = BmkgState { loaded: true, ..Default::default() };
            release_tsunami(app);
            app.clear_activity("bmkg.quake");
            app.clear_activity("bmkg.warning");
            app.request_redraw(Redraw::Tab("today"));
        }
    });
    r.add_service(ServiceSpec { name: "bmkg-quake", enabled: |c| c.bmkg.quake, run: quake_service });
    r.add_service(ServiceSpec { name: "bmkg-nowcast", enabled: |c| c.bmkg.nowcast, run: nowcast_service });
}

fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn on_config(app: &mut App) {
    let c = app.config().bmkg.clone();
    if app.state::<BmkgState>().sample {
        return;
    }
    if !c.quake {
        let st = app.state_mut::<BmkgState>();
        st.quakes.clear();
        st.quake_failed = false;
        st.alert = None;
        app.clear_activity("bmkg.quake");
        release_tsunami(app);
    }
    if !c.nowcast {
        let st = app.state_mut::<BmkgState>();
        st.warnings.clear();
        st.warning = None;
        app.clear_activity("bmkg.warning");
    }
    app.request_redraw(Redraw::Tab("today"));
}

// ---- what was already shown -------------------------------------------------------------------

fn seen_path() -> std::path::PathBuf {
    moku_core::paths::data_file(SEEN_FILE)
}

fn ensure_loaded(app: &mut App) {
    if app.state::<BmkgState>().loaded {
        return;
    }
    let (quakes, warnings) = std::fs::read_to_string(seen_path()).map(|t| parse_seen(&t)).unwrap_or_default();
    let st = app.state_mut::<BmkgState>();
    st.loaded = true;
    st.seen_quakes = quakes;
    st.seen_warnings = warnings;
}

/// `{"quakes":[event times],"warnings":[ids]}`; anything unreadable is "nothing shown yet".
fn parse_seen(text: &str) -> (BTreeSet<i64>, Vec<String>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}')) else { return Default::default() };
    let quakes = v.get("quakes").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|t| t.as_i64()).collect()).unwrap_or_default();
    let warnings = v.get("warnings").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect()).unwrap_or_default();
    (quakes, warnings)
}

fn seen_json(quakes: &BTreeSet<i64>, warnings: &[String]) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "quakes": quakes, "warnings": warnings })).unwrap_or_default()
}

fn save(app: &mut App) {
    if app.state::<BmkgState>().sample {
        return;
    }
    app.save_later("bmkg", |app| {
        let st = app.state::<BmkgState>();
        Some((seen_path(), seen_json(&st.seen_quakes, &st.seen_warnings)))
    });
}

// ---- earthquakes ------------------------------------------------------------------------------

fn quake_service(ctx: ServiceCtx) {
    let mut fails = 0u32;
    loop {
        // Where the user is, for the distance. Asked once per city name; a failure only means no distance.
        if let Err(e) = today::place::resolve_and_publish(&ctx) {
            log_warn!("bmkg: no position for the distance: {e}");
        }
        let wait = match poll_quakes() {
            Ok(list) => {
                fails = 0;
                ctx.post(move |app| apply_quakes(app, list, false));
                QUAKE_EVERY
            }
            Err(e) => {
                fails += 1;
                log_warn!("bmkg: earthquakes: {e}");
                ctx.post(|app| {
                    app.state_mut::<BmkgState>().quake_failed = true;
                    app.request_redraw(Redraw::Tab("today"));
                });
                (QUAKE_EVERY * 2u32.saturating_pow(fails.min(6) - 1)).min(MAX_BACKOFF)
            }
        };
        if !ctx.sleep(wait) {
            return;
        }
    }
}

/// Both feeds; one failing is fine, both is an error.
fn poll_quakes() -> Result<Vec<Quake>, String> {
    let auto = crate::net::get_text(quake::AUTOGEMPA_URL, HTTP_TIMEOUT).map(|t| quake::parse_autogempa(&t));
    let felt = crate::net::get_text(quake::DIRASAKAN_URL, HTTP_TIMEOUT).map(|t| quake::parse_dirasakan(&t));
    match (auto, felt) {
        (Err(a), Err(f)) => Err(format!("{a}; {f}")),
        (a, f) => Ok(quake::merge(a.unwrap_or(None), f.unwrap_or_default())),
    }
}

fn rules(app: &App) -> Rules {
    let c = &app.config().bmkg;
    Rules { radius_km: f64::from(c.radius_km), big_magnitude: c.big_magnitude }
}

/// A poll came back (UI thread). `forced`: a test hook, shown whatever the switch says.
fn apply_quakes(app: &mut App, quakes: Vec<Quake>, forced: bool) {
    if !forced && !app.config().bmkg.quake {
        return;
    }
    ensure_loaded(app);
    let home = today::geocoded(app).map(|(_, lat, lon)| (lat, lon));
    let rules = rules(app);
    let now = unix_now();
    let st = app.state_mut::<BmkgState>();
    let alerts = quake::new_alerts(&quakes, home, &rules, &st.seen_quakes, now);
    st.quakes = quakes;
    st.quake_failed = false;
    if !alerts.is_empty() {
        if !forced {
            quake::remember(&mut st.seen_quakes, alerts.iter().map(|a| a.quake.time));
        }
        let (tsunami, top) = pills_for(&alerts);
        // The tsunami warning first, and on its own activity: the ordinary quake pill below can
        // not take its place.
        if let Some(t) = tsunami.cloned() {
            show_tsunami(app, t);
        }
        if let Some(a) = top.cloned() {
            show_quake(app, a, if forced { SAMPLE_SECS } else { QUAKE_SECS });
        }
        save(app);
    }
    app.request_redraw(Redraw::Tab("today"));
}

fn is_tsunami(a: &Alert) -> bool {
    a.quake.potential.as_deref().is_some_and(quake::tsunami_potential)
}

/// Which alerts get a pill: the strongest tsunami warning (its own pill) and the strongest of the
/// rest (the ordinary quake pill). `alerts` is strongest first.
fn pills_for(alerts: &[Alert]) -> (Option<&Alert>, Option<&Alert>) {
    (alerts.iter().find(|a| is_tsunami(a)), alerts.iter().find(|a| !is_tsunami(a)))
}

/// The ordinary quake pill (transient). A tsunami warning goes to [`show_tsunami`].
fn show_quake(app: &mut App, alert: Alert, secs: f32) {
    if is_tsunami(&alert) {
        return show_tsunami(app, alert);
    }
    let priority = if alert.why == Why::Near { QUAKE_PRIORITY } else { FAR_PRIORITY };
    app.state_mut::<BmkgState>().alert = Some(alert);
    app.set_activity(Activity::transient("bmkg.quake", priority, secs, quake_pill).tab("today").bubble(|_| stack([icon(icons::WARNING).color(color::RED)])));
    app.request_redraw(Redraw::Pill);
}

/// The tsunami pill: persistent until closed, kept visible over a full-screen app and over the
/// apps the user listed in `island.hide_apps` (`force_visible`), and never replaced by another quake.
fn show_tsunami(app: &mut App, alert: Alert) {
    let st = app.state_mut::<BmkgState>();
    st.tsunami = Some(alert);
    if !std::mem::replace(&mut st.forcing, true) {
        app.force_visible(true);
    }
    app.set_activity(Activity::persistent(TSUNAMI_ID, TSUNAMI_PRIORITY, tsunami_pill).tab("today").bubble(|_| stack([icon(icons::WARNING).color(color::RED)])));
    app.request_redraw(Redraw::Pill);
}

/// The user closed the tsunami pill (or the data was cleared): release the forced visibility.
fn release_tsunami(app: &mut App) {
    app.state_mut::<BmkgState>().tsunami = None;
    app.clear_activity(TSUNAMI_ID);
    if std::mem::take(&mut app.state_mut::<BmkgState>().forcing) {
        app.force_visible(false);
    }
}

/// `M 5.3`.
fn magnitude(m: f64) -> String {
    format!("M {m:.1}")
}

fn magnitude_color(m: f64) -> Rgba {
    if m >= 6.0 {
        color::RED
    } else if m >= 5.0 {
        color::ORANGE
    } else {
        color::YELLOW
    }
}

/// `120 km`, `8,5 km` (a decimal comma in Indonesian).
fn km_text(km: f64) -> String {
    if km >= 10.0 { format!("{} km", km.round() as i64) } else { i18n::tr_s(format!("{km:.1} km").replace('.', ","), format!("{km:.1} km")) }
}

fn clock_of(secs: i64) -> String {
    Local.timestamp_opt(secs, 0).single().map(|d| i18n::clock(&d)).unwrap_or_default()
}

/// "120 km dari Jakarta" / "tidak dekat"; nothing when the position is not known.
fn distance_text(km: Option<f64>, city: &str) -> Option<String> {
    km.map(|km| tr_s(format!("{} dari {city}", km_text(km)), format!("{} from {city}", km_text(km))))
}

fn quake_pill(app: &App) -> Node {
    match &app.state::<BmkgState>().alert {
        Some(a) => alert_pill(app, a),
        None => empty(),
    }
}

fn tsunami_pill(app: &App) -> Node {
    match &app.state::<BmkgState>().tsunami {
        Some(a) => alert_pill(app, a),
        None => empty(),
    }
}

/// One quake as the pill; a tsunami warning has the red line and a close button.
fn alert_pill(app: &App, a: &Alert) -> Node {
    let q = &a.quake;
    let tsunami = is_tsunami(a);
    let c = magnitude_color(q.magnitude);
    let city = &app.config().today.city;
    let mut facts: Vec<String> = distance_text(a.km, city).into_iter().collect();
    facts.push(clock_of(q.time));
    if let Some(d) = q.depth_km {
        facts.push(tr_s(format!("kedalaman {d:.0} km"), format!("depth {d:.0} km")));
    }
    facts.push(tr("Sumber: BMKG", "Source: BMKG").to_string());
    let mut lines = vec![
        row([text(tr("Gempa", "Earthquake")).semibold(), text(magnitude(q.magnitude)).bold().color(c), caption("·"), text(q.region.clone()).secondary().max_w(300.0)]).gap(6.0),
        caption(facts.join(" · ")),
    ];
    if let Some(p) = &q.potential {
        // BMKG's own words; red when it announces a tsunami.
        lines.push(if tsunami { text(p.clone()).font_size(12.0).semibold().color(color::RED) } else { caption(p.clone()) });
    }
    let mut kids = vec![stack([icon(icons::WARNING).font_size(15.0).color(c)]).size(30.0, 30.0).bg(c.alpha(0.18)).capsule(), col(lines).gap(1.0)];
    if tsunami {
        kids.push(icon_button(icons::CLOSE, "bmkg.dismiss").name(tr("Tutup", "Dismiss")));
    }
    row(kids).gap(10.0).pad4(10.0, 8.0, if tsunami { 4.0 } else { 16.0 }, 8.0)
}

// ---- weather warnings -------------------------------------------------------------------------

fn nowcast_service(ctx: ServiceCtx) {
    let mut fails = 0u32;
    loop {
        let wait = match crate::net::get_text(nowcast::RSS_URL, HTTP_TIMEOUT) {
            Ok(xml) => {
                fails = 0;
                let list = nowcast::parse_rss(&xml);
                ctx.post(move |app| apply_warnings(app, list, false));
                NOWCAST_EVERY
            }
            Err(e) => {
                fails += 1;
                log_warn!("bmkg: weather warnings: {e}");
                (NOWCAST_EVERY * 2u32.saturating_pow(fails.min(4) - 1)).min(MAX_BACKOFF * 2)
            }
        };
        if !ctx.sleep(wait) {
            return;
        }
    }
}

/// The province the warnings are for: the setting, else the city's.
fn province(app: &App) -> Option<&'static str> {
    let c = &app.config().bmkg;
    if !c.province.trim().is_empty() {
        return geo::canonical(&c.province);
    }
    geo::province_of_city(&app.config().today.city, today::geocoded(app).map(|(_, lat, lon)| (lat, lon)))
}

/// A feed came back (UI thread).
fn apply_warnings(app: &mut App, all: Vec<Warning>, forced: bool) {
    if !forced && !app.config().bmkg.nowcast {
        return;
    }
    ensure_loaded(app);
    let prov = province(app);
    let now = unix_now();
    let mut mine: Vec<Warning> = all.into_iter().filter(|w| w.active(now) && prov.is_some_and(|p| geo::same(&w.province, p))).collect();
    mine.sort_by(|a, b| b.published.cmp(&a.published));
    let st = app.state_mut::<BmkgState>();
    let fresh: Vec<Warning> = mine.iter().filter(|w| !st.seen_warnings.contains(&w.id)).cloned().collect();
    st.warnings = mine;
    if let Some(top) = fresh.first().cloned() {
        if !forced {
            st.seen_warnings.extend(fresh.iter().map(|w| w.id.clone()));
            let excess = st.seen_warnings.len().saturating_sub(nowcast::MAX_SEEN);
            st.seen_warnings.drain(..excess);
        }
        show_warning(app, top, if forced { SAMPLE_SECS } else { WARNING_SECS });
        save(app);
    }
    app.request_redraw(Redraw::Tab("today"));
}

fn show_warning(app: &mut App, w: Warning, secs: f32) {
    app.state_mut::<BmkgState>().warning = Some(w);
    app.set_activity(Activity::transient("bmkg.warning", WARNING_PRIORITY, secs, warning_pill).tab("today").bubble(|_| stack([icon(icons::CLOUD).color(color::ORANGE)])));
    app.request_redraw(Redraw::Pill);
}

/// "Berlaku hingga 03.30" from the end time, when the alert says one.
fn until_text(w: &Warning) -> Option<String> {
    w.until.map(|u| tr_s(format!("Berlaku hingga {}", clock_of(u)), format!("Until {}", clock_of(u))))
}

fn warning_pill(app: &App) -> Node {
    let st = app.state::<BmkgState>();
    let Some(w) = &st.warning else { return empty() };
    let city = &app.config().today.city;
    let mut facts: Vec<String> = until_text(w).into_iter().collect();
    if w.mentions(city) {
        facts.push(tr_s(format!("termasuk {city}"), format!("includes {city}")));
    }
    facts.push(tr("Sumber: BMKG", "Source: BMKG").to_string());
    row([
        stack([icon(icons::CLOUD).font_size(15.0).color(color::ORANGE)]).size(30.0, 30.0).bg(color::ORANGE.alpha(0.18)).capsule(),
        col([
            row([text(tr("Peringatan dini", "Weather warning")).semibold(), caption("·"), text(w.event.clone()).secondary().max_w(300.0)]).gap(6.0),
            caption(facts.join(" · ")),
        ])
        .gap(1.0),
    ])
    .gap(10.0)
    .pad4(10.0, 8.0, 16.0, 8.0)
}

// ---- Hari ini ---------------------------------------------------------------------------------

/// The BMKG card of the Hari ini tab: the latest quake that counts for the user (in the last 24
/// hours) and the warnings in force, with the credit. `None` when there is nothing to say.
pub fn section(app: &App) -> Option<Node> {
    let st = app.state::<BmkgState>();
    let cfg = &app.config().bmkg;
    let now = unix_now();
    let home = today::geocoded(app).map(|(_, lat, lon)| (lat, lon));
    let rules = rules(app);
    let latest = (cfg.quake || st.sample).then(|| st.quakes.iter().filter(|q| now - q.time <= CARD_AGE_SECS).find_map(|q| quake::qualify(q, home, &rules))).flatten();
    let warnings: Vec<&Warning> = if cfg.nowcast || st.sample { st.warnings.iter().filter(|w| w.active(now)).take(2).collect() } else { vec![] };
    if latest.is_none() && warnings.is_empty() {
        return None;
    }
    let city = &app.config().today.city;
    let mut kids = vec![label(tr("BMKG", "BMKG"))];
    if let Some(a) = latest {
        let q = &a.quake;
        let c = magnitude_color(q.magnitude);
        let mut facts: Vec<String> = distance_text(a.km, city).into_iter().collect();
        facts.push(clock_of(q.time));
        let tsunami = q.potential.as_deref().is_some_and(quake::tsunami_potential);
        kids.push(
            row([
                text(magnitude(q.magnitude)).bold().color(c),
                col([text(q.region.clone()).font_size(12.0), caption(facts.join(" · "))]).gap(0.0).grow(1.0),
            ])
            .gap(8.0)
            .fill_w(),
        );
        if let Some(felt) = &q.felt {
            kids.push(caption(tr_s(format!("Dirasakan: {felt}"), format!("Felt: {felt}"))));
        }
        if tsunami {
            kids.push(text(q.potential.clone().unwrap_or_default()).font_size(11.5).semibold().color(color::RED));
        }
    }
    for w in warnings {
        kids.push(row([icon(icons::CLOUD).font_size(13.0).color(color::ORANGE), col([text(w.event.clone()).font_size(12.0), caption(until_text(w).unwrap_or_default())]).gap(0.0).grow(1.0)]).gap(8.0).fill_w());
    }
    kids.push(caption(tr("Sumber: BMKG", "Source: BMKG")));
    Some(col(kids).gap(4.0).fill_w().bg(color::FILL).radius(typo::RADIUS_CARD).pad(14.0).align(Align::Stretch))
}

// ---- actions and test hooks -------------------------------------------------------------------

fn on_action(app: &mut App, ev: &ActionEvent) {
    if ev.kind != ActionKind::Click {
        return;
    }
    match ev.name() {
        "dismiss" => {
            release_tsunami(app);
            app.clear_activity("bmkg.quake");
            app.request_redraw(Redraw::Pill);
        }
        "sample_quake" => sample_quake(app, ev.arg().unwrap_or("near")),
        "sample_warning" => sample_warning(app),
        _ => {}
    }
}

/// An invented quake through the real thresholds: `near` (about 90 km from the Hari ini city,
/// M 5.1), `big` (M 6.4 far away), `tsunami` (M 7.1 with a tsunami line).
fn sample_quake(app: &mut App, kind: &str) {
    let (lat, lon) = today::geocoded(app).map(|(_, la, lo)| (la, lo)).unwrap_or((-6.2088, 106.8456));
    let (dlat, dlon, mag, region, potential): (f64, f64, f64, &str, Option<&str>) = match kind {
        "big" => (-9.0, 8.0, 6.4, "laut 112 km BaratDaya Maluku Barat Daya", Some("Tidak berpotensi tsunami")),
        "tsunami" => (-4.0, -3.0, 7.1, "laut 210 km BaratDaya Kepulauan Mentawai", Some("Berpotensi tsunami")),
        _ => (-0.8, -0.1, 5.1, "laut 48 km BaratDaya Sukabumi, Jawa Barat", Some("Tidak berpotensi tsunami")),
    };
    let q = Quake { time: unix_now() - 120, lat: lat + dlat, lon: lon + dlon, magnitude: mag, depth_km: Some(10.0), region: region.to_string(), potential: potential.map(str::to_string), felt: None };
    app.state_mut::<BmkgState>().sample = true;
    apply_quakes(app, vec![q], true);
}

/// An invented weather warning for the user's province.
fn sample_warning(app: &mut App) {
    let prov = province(app).unwrap_or("DKI Jakarta");
    let now = unix_now();
    let city = app.config().today.city.clone();
    let w = Warning {
        id: format!("sample-{now}"),
        event: "Hujan Lebat disertai Petir".into(),
        province: prov.to_string(),
        text: format!("Hujan lebat disertai petir akan terjadi di sebagian wilayah {prov}, khususnya di {}.", city.to_uppercase()),
        published: now - 300,
        until: Some(now + 2 * 3600),
    };
    app.state_mut::<BmkgState>().sample = true;
    apply_warnings(app, vec![w], true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seen_file_round_trips_and_survives_junk() {
        let quakes: BTreeSet<i64> = [3, 1, 2].into_iter().collect();
        let warnings = vec!["a".to_string(), "b".to_string()];
        assert_eq!(parse_seen(&String::from_utf8(seen_json(&quakes, &warnings)).unwrap()), (quakes, warnings));
        for junk in ["", "{", "[]", "null", r#"{"quakes":"x","warnings":5}"#, "\u{feff}{}"] {
            assert_eq!(parse_seen(junk), (BTreeSet::new(), vec![]), "{junk:?}");
        }
        // Wrong element types are skipped one by one.
        let (q, w) = parse_seen(r#"{"quakes":[1,"x",2.5,3],"warnings":["a",7,"b"]}"#);
        assert_eq!(q.into_iter().collect::<Vec<_>>(), [1, 3]);
        assert_eq!(w, ["a", "b"]);
    }

    fn alert(time: i64, magnitude: f64, potential: Option<&str>, why: Why) -> Alert {
        Alert { quake: Quake { time, lat: 0.0, lon: 0.0, magnitude, depth_km: Some(10.0), region: "x".into(), potential: potential.map(str::to_string), felt: None }, km: None, why }
    }

    #[test]
    fn a_tsunami_warning_and_a_later_quake_get_separate_pills() {
        let tsunami = alert(100, 7.1, Some("Berpotensi tsunami"), Why::Big);
        let later = alert(200, 5.2, Some("Tidak berpotensi tsunami"), Why::Near);
        // Strongest first, as `new_alerts` sorts: both get a pill, each on its own activity.
        let both = [tsunami.clone(), later.clone()];
        let (t, top) = pills_for(&both);
        assert_eq!(t.map(|a| a.quake.time), Some(100));
        assert_eq!(top.map(|a| a.quake.time), Some(200));
        // A weaker tsunami warning behind a stronger ordinary quake is still the tsunami pill.
        let strong = alert(300, 6.8, None, Why::Big);
        let list = [strong.clone(), tsunami.clone()];
        let (t, top) = pills_for(&list);
        assert_eq!(t.map(|a| a.quake.time), Some(100));
        assert_eq!(top.map(|a| a.quake.time), Some(300));
        // No tsunami: only the ordinary pill; only a tsunami: only the tsunami pill.
        assert!(pills_for(&[later]).0.is_none());
        assert!(pills_for(&[tsunami]).1.is_none());
        assert_eq!(pills_for(&[]), (None, None));
        // The two ids differ, so setting one activity never replaces the other.
        assert_ne!(TSUNAMI_ID, "bmkg.quake");
        assert!(TSUNAMI_PRIORITY > QUAKE_PRIORITY && TSUNAMI_PRIORITY > FAR_PRIORITY);
    }

    #[test]
    fn texts_for_the_pill() {
        assert_eq!(magnitude(5.0), "M 5.0");
        assert_eq!(magnitude(7.14), "M 7.1");
        assert_eq!(km_text(120.4), "120 km");
        assert_eq!(km_text(120.6), "121 km");
        // Under 10 km one decimal, with the language's decimal mark.
        let near = km_text(8.5);
        assert!(near == "8,5 km" || near == "8.5 km", "{near}");
        assert!(magnitude_color(6.0) == color::RED && magnitude_color(5.0) == color::ORANGE && magnitude_color(4.0) == color::YELLOW);
        assert_eq!(distance_text(None, "Jakarta"), None);
        assert!(distance_text(Some(120.0), "Jakarta").is_some_and(|t| t.contains("120 km") && t.contains("Jakarta")));
    }
}
