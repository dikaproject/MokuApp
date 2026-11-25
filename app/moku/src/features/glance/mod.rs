//! Feature `glance` (v0.3, track P): small facts for the Hari ini tab, as chips under the weather.
//!
//! * **Exchange rate** (`glance.rate`): `USD/IDR 18.229` for the configured pair (default
//!   USD -> IDR), from Frankfurter (ECB rates) every 6 hours ([`rate`]).
//! * **Air quality** (`glance.aqi`): the US AQI with its level, from Open-Meteo for the Hari ini
//!   city, every hour ([`aqi`]); with `glance.aqi_alert` a pill when the air goes above 150
//!   ("Tidak sehat"), once, with a gap so a wobbling reading does not repeat it.
//! * Each service is off when its switch is off: no request, no memory.
//!
//! Test hooks (`MOKU_DEBUG=1`): `mokuctl debug click glance.sample_aqi[:<aqi>]` (default 159) and
//! `glance.sample_rate` show invented readings through the real code.

mod aqi;
mod rate;

use std::time::Duration;

use self::aqi::{Aqi, Gate};
use self::rate::Rate;
use crate::activity::Activity;
use crate::app::{App, Redraw};
use crate::features::today;
use crate::i18n::{self, tr, tr_s};
use crate::registry::*;
use crate::services::ServiceCtx;
use crate::ui::*;

const RATE_EVERY: Duration = Duration::from_secs(6 * 3600);
const AQI_EVERY: Duration = Duration::from_secs(3600);
const RETRY: Duration = Duration::from_secs(5 * 60);
const MAX_RETRY: Duration = Duration::from_secs(3600);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
/// Above a running timer (50) and a progress pill (55), below warnings (65).
const AQI_PRIORITY: u8 = 58;
const AQI_SECS: f32 = 20.0;
/// Test-hook pills stay long enough to screenshot.
const SAMPLE_SECS: f32 = 45.0;

#[derive(Default)]
pub struct GlanceState {
    rate: Option<Rate>,
    rate_failed: bool,
    aqi: Option<Aqi>,
    aqi_failed: bool,
    gate: Gate,
    /// `(pair, place)` the services were started for: restart them when the settings change.
    rate_key: Option<(String, String)>,
    aqi_key: Option<(String, Option<u64>, Option<u64>)>,
    /// Invented readings from a test hook are on screen.
    sample: bool,
}

pub fn register(r: &mut Registry) {
    r.add_state(GlanceState::default());
    r.on_action("glance", on_action);
    r.on_config(on_config);
    r.on_event(|app, ev| {
        // Nothing is saved: "Hapus semua data" only drops what is in memory.
        if *ev == AppEvent::DataCleared {
            *app.state_mut::<GlanceState>() = GlanceState::default();
            app.clear_activity("glance.aqi");
            app.request_redraw(Redraw::Tab("today"));
        }
    });
    r.add_service(ServiceSpec { name: "glance-rate", enabled: |c| c.glance.rate, run: rate_service });
    r.add_service(ServiceSpec { name: "glance-aqi", enabled: |c| c.glance.aqi, run: aqi_service });
}

fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn on_config(app: &mut App) {
    let g = app.config().glance.clone();
    let t = app.config().today.clone();
    let rate_key = (g.base.clone(), g.quote.clone());
    let aqi_key = (t.city.clone(), t.latitude.map(f64::to_bits), t.longitude.map(f64::to_bits));
    let st = app.state_mut::<GlanceState>();
    if st.sample {
        return;
    }
    let rate_moved = st.rate_key.as_ref().is_some_and(|k| *k != rate_key);
    let aqi_moved = st.aqi_key.as_ref().is_some_and(|k| *k != aqi_key);
    let (rate_first, aqi_first) = (st.rate_key.is_none(), st.aqi_key.is_none());
    st.rate_key = Some(rate_key);
    st.aqi_key = Some(aqi_key);
    if !g.rate || rate_moved {
        st.rate = None;
        st.rate_failed = false;
    }
    if !g.aqi || aqi_moved {
        st.aqi = None;
        st.aqi_failed = false;
        st.gate = Gate::default();
    }
    if !g.aqi {
        app.clear_activity("glance.aqi");
    }
    if g.rate && rate_moved && !rate_first {
        app.restart_service("glance-rate");
    }
    if g.aqi && aqi_moved && !aqi_first {
        app.restart_service("glance-aqi");
    }
    app.request_redraw(Redraw::Tab("today"));
}

// ---- services ---------------------------------------------------------------------------------

fn rate_service(ctx: ServiceCtx) {
    let mut fails = 0u32;
    loop {
        let wait = match fetch_rate(&ctx) {
            Ok(()) => {
                fails = 0;
                RATE_EVERY
            }
            Err(e) => {
                fails += 1;
                log_warn!("glance: exchange rate: {e}");
                ctx.post(|app| {
                    app.state_mut::<GlanceState>().rate_failed = true;
                    app.request_redraw(Redraw::Tab("today"));
                });
                (RETRY * 2u32.saturating_pow(fails.min(5) - 1)).min(MAX_RETRY)
            }
        };
        if !ctx.sleep(wait) {
            return;
        }
    }
}

fn fetch_rate(ctx: &ServiceCtx) -> Result<(), String> {
    let cfg = ctx.config();
    let (base, quote) = (cfg.glance.base.clone(), cfg.glance.quote.clone());
    if base == quote {
        // 1 USD = 1 USD: nothing to ask.
        let r = Rate { base, quote, value: 1.0 };
        ctx.post(move |app| apply_rate(app, r));
        return Ok(());
    }
    let body = crate::net::get_text(&rate::url(&base, &quote), HTTP_TIMEOUT)?;
    let r = rate::parse(&body, &base, &quote).ok_or("unexpected exchange rate response")?;
    ctx.post(move |app| apply_rate(app, r));
    Ok(())
}

fn apply_rate(app: &mut App, r: Rate) {
    if !app.config().glance.rate {
        return;
    }
    // A late answer for a pair the user has since changed.
    let g = &app.config().glance;
    if (&r.base, &r.quote) != (&g.base, &g.quote) {
        return;
    }
    let st = app.state_mut::<GlanceState>();
    st.rate = Some(r);
    st.rate_failed = false;
    app.request_redraw(Redraw::Tab("today"));
}

fn aqi_service(ctx: ServiceCtx) {
    let mut fails = 0u32;
    loop {
        let wait = match fetch_aqi(&ctx) {
            Ok(()) => {
                fails = 0;
                AQI_EVERY
            }
            Err(e) => {
                fails += 1;
                log_warn!("glance: air quality: {e}");
                ctx.post(|app| {
                    app.state_mut::<GlanceState>().aqi_failed = true;
                    app.request_redraw(Redraw::Tab("today"));
                });
                (RETRY * 2u32.saturating_pow(fails.min(5) - 1)).min(MAX_RETRY)
            }
        };
        if !ctx.sleep(wait) {
            return;
        }
    }
}

fn fetch_aqi(ctx: &ServiceCtx) -> Result<(), String> {
    let (lat, lon) = today::place::resolve_and_publish(ctx)?;
    let body = crate::net::get_text(&aqi::url(lat, lon), HTTP_TIMEOUT)?;
    let a = aqi::parse(&body).ok_or("unexpected air quality response")?;
    ctx.post(move |app| apply_aqi(app, a, false));
    Ok(())
}

/// A reading came back (UI thread). `forced`: a test hook, shown whatever the switch says.
fn apply_aqi(app: &mut App, a: Aqi, forced: bool) {
    if !forced && !app.config().glance.aqi {
        return;
    }
    let alert_on = app.config().glance.aqi_alert;
    let now = unix_now();
    let level = a.us_aqi;
    let st = app.state_mut::<GlanceState>();
    st.aqi = Some(a);
    st.aqi_failed = false;
    let raise = st.gate.observe(level, now) && (alert_on || forced);
    if raise {
        let secs = if forced { SAMPLE_SECS } else { AQI_SECS };
        app.set_activity(Activity::transient("glance.aqi", AQI_PRIORITY, secs, aqi_pill).tab("today").bubble(|app| stack([icon(icons::CLOUD).color(aqi_color(app))])));
        app.request_redraw(Redraw::Pill);
    }
    app.request_redraw(Redraw::Tab("today"));
}

// ---- drawing ----------------------------------------------------------------------------------

fn level_color(l: aqi::Level) -> Rgba {
    match l {
        aqi::Level::Good => color::GREEN,
        aqi::Level::Moderate => color::YELLOW,
        aqi::Level::Sensitive => color::ORANGE,
        aqi::Level::Unhealthy => color::RED,
        aqi::Level::VeryUnhealthy => color::PURPLE,
        aqi::Level::Hazardous => color::PINK,
    }
}

fn aqi_color(app: &App) -> Rgba {
    app.state::<GlanceState>().aqi.as_ref().map(|a| level_color(aqi::level(a.us_aqi))).unwrap_or(color::TEXT3)
}

fn aqi_pill(app: &App) -> Node {
    let st = app.state::<GlanceState>();
    let Some(a) = &st.aqi else { return empty() };
    let l = aqi::level(a.us_aqi);
    let c = level_color(l);
    let mut kids = vec![icon(icons::CLOUD).color(c), text(tr("Kualitas udara tidak sehat", "Unhealthy air quality")).semibold(), text(format!("AQI {}", a.us_aqi)).bold().color(c)];
    if let Some(pm) = a.pm25 {
        kids.push(caption(tr_s(format!("PM2,5 {pm:.0} µg/m³"), format!("PM2.5 {pm:.0} µg/m³"))));
    }
    kids.push(caption(app.config().today.city.clone()));
    pill(kids)
}

/// A chip: an optional coloured dot, then the words.
fn chip(lead: Option<Node>, words: Node) -> Node {
    row(lead.into_iter().chain([words])).gap(6.0).pad_xy(10.0, 0.0).h(26.0).bg(color::FILL2).capsule()
}

fn dot(c: Rgba) -> Node {
    gap_px(8.0).capsule().bg(c)
}

/// The chips of the Hari ini card, or `None` when both are off.
pub fn chips(app: &App) -> Option<Node> {
    let g = &app.config().glance;
    let st = app.state::<GlanceState>();
    let english = i18n::is_english();
    let mut out: Vec<Node> = vec![];
    if g.rate || (st.sample && st.rate.is_some()) {
        let words = match &st.rate {
            Some(r) => text(rate::chip_text(r, english)).font_size(12.0).semibold(),
            None if st.rate_failed => text(format!("1 {} = —", g.base)).font_size(12.0).tertiary(),
            None => text(format!("1 {} = …", g.base)).font_size(12.0).tertiary(),
        };
        out.push(chip(None, words));
    }
    if g.aqi || (st.sample && st.aqi.is_some()) {
        let c = match &st.aqi {
            Some(a) => {
                let l = aqi::level(a.us_aqi);
                chip(Some(dot(level_color(l))), row([text(format!("AQI {}", a.us_aqi)).font_size(12.0).semibold(), caption(i18n::pair(aqi::label(l)))]).gap(5.0))
            }
            None if st.aqi_failed => chip(Some(dot(color::TEXT3)), text("AQI —").font_size(12.0).tertiary()),
            None => chip(Some(dot(color::TEXT3)), text("AQI …").font_size(12.0).tertiary()),
        };
        out.push(c);
    }
    (!out.is_empty()).then(|| row(out).gap(6.0).fill_w())
}

// ---- test hooks -------------------------------------------------------------------------------

fn on_action(app: &mut App, ev: &ActionEvent) {
    if ev.kind != ActionKind::Click {
        return;
    }
    match ev.name() {
        "sample_aqi" => {
            let level = ev.arg().and_then(|a| a.parse::<u32>().ok()).unwrap_or(159);
            app.state_mut::<GlanceState>().sample = true;
            // The reading arrives the way a real one does, so the pill follows the same rules.
            app.state_mut::<GlanceState>().gate = Gate::default();
            apply_aqi(app, Aqi { us_aqi: level, pm25: Some(114.6) }, true);
        }
        "sample_rate" => {
            let g = app.config().glance.clone();
            let st = app.state_mut::<GlanceState>();
            st.sample = true;
            st.rate = Some(Rate { base: g.base, quote: g.quote, value: 18_229.0 });
            app.request_redraw(Redraw::Tab("today"));
        }
        _ => {}
    }
}
