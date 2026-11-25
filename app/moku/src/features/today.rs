//! Feature `today` — Task 4 (track C), extended in v0.3 (track P).
//!
//! The Today panel: date and week strip, a local to-do list (`todo.json`), the weather from
//! Open-Meteo for the configured city, Indonesian holidays, glance chips (exchange rate, air
//! quality) and the latest BMKG alerts.
//!
//! * To-do: `%APPDATA%\Moku\todo.json` (via `moku_core::paths`, so `MOKU_DATA_DIR` applies), a
//!   JSON array of `{id, text, done, due?, fired?}`. Written atomically; a corrupted file reads as
//!   an empty list (the bad file is kept next to it as `todo.corrupt.json`), never a crash.
//!   Items are added with the "Tambah to-do" field (typed into the panel through the core's text
//!   input), `mokuctl todo add` or the launcher's `todo`; all three read a due time out of the
//!   words ([`due`]: "rapat jam 3", "besok 09:00", "in 2 hours"). A to-do with a due time raises
//!   a reminder pill (priority 85) with "Selesai" and "Tunda 10 m" ([`remind`]). Edit with the
//!   pencil or a double-click, delete with ×.
//! * Weather: a service thread asks Open-Meteo for the current temperature, weather code and the
//!   hourly rain probability every 30 minutes (only while the feature and `today.weather` are
//!   on). With no `latitude`/`longitude` in the config the city is geocoded first ([`place`]). The
//!   result reaches the UI through `Ui::post`; a "rain within the hour" heads-up is raised when the
//!   next hours look wet.
//! * Holidays ([`holiday`]), BMKG alerts (`features::bmkg`) and glance chips (`features::glance`)
//!   have their own modules and only draw into this tab.
//! * Nothing here blocks the UI thread: network in the service, one small file write per change.

mod due;
mod holiday;
pub(crate) mod place;
mod remind;

use std::path::Path;
use std::time::Duration;

use chrono::{Datelike, Days, Local, NaiveDateTime, TimeZone};
use moku_core::config::Units;
use moku_core::ipc::Request;
use serde::{Deserialize, Serialize};

use crate::activity::{Activity, HEADS_UP_SECS, priority};
use crate::app::{App, Redraw, TimerId};
use crate::i18n::{self, tr, tr_s};
use crate::ipc_server::Reply;
use crate::registry::*;
use crate::services::ServiceCtx;
use crate::ui::text_edit::Key;
use crate::ui::*;

/// Weather refresh period.
const REFRESH: Duration = Duration::from_secs(30 * 60);
/// Retry period after a failed request (grows up to [`REFRESH`]).
const RETRY: Duration = Duration::from_secs(2 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Rain probability (%) that triggers the heads-up.
const RAIN_ALERT_PCT: u8 = 60;
/// Bounds for the to-do list (a pipe client could flood it).
const MAX_TODOS: usize = 200;
const MAX_TODO_CHARS: usize = 300;
/// Rows of the to-do card; more are summarised as "+N lagi".
const VISIBLE_TODOS: usize = 4;
/// With the weather card (and its chips) above, the card has room for one row less.
const VISIBLE_TODOS_WITH_WEATHER: usize = 3;
/// The sample rain pill stays long enough to screenshot.
const SAMPLE_ALERT_SECS: f32 = 45.0;
/// Pencil (Segoe Fluent Icons `Edit`).
const PENCIL: &str = "\u{E70F}";
/// The due reminder pill (plan: priority 85, under a timer that is done).
const DUE_PRIORITY: u8 = 85;
/// The text inputs of the card: their edits arrive as actions `today.add` / `today.edit`.
const ADD_INPUT: &str = "today.add";
const EDIT_INPUT: &str = "today.edit";

// ---- to-do ------------------------------------------------------------------------------------

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Todo {
    /// Stable id for the click action (assigned when missing).
    #[serde(default)]
    pub id: u64,
    pub text: String,
    #[serde(default)]
    pub done: bool,
    /// When to remind (Unix seconds), read from the words ("rapat jam 3") or set by "Tunda".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<i64>,
    /// The reminder for `due` was shown (or dismissed): it is not raised again.
    #[serde(default, skip_serializing_if = "is_false")]
    pub fired: bool,
}

/// Parse `todo.json` text. `None` = not usable as a list at all; individual broken entries are
/// dropped. Missing / duplicate ids are repaired.
pub fn parse_todos(text: &str) -> Option<Vec<Todo>> {
    let text = text.trim_start_matches('\u{feff}').trim();
    if text.is_empty() {
        return Some(vec![]);
    }
    let serde_json::Value::Array(items) = serde_json::from_str::<serde_json::Value>(text).ok()? else { return None };
    let mut out: Vec<Todo> = items.into_iter().filter_map(|v| serde_json::from_value::<Todo>(v).ok()).filter(|t| !t.text.trim().is_empty()).collect();
    out.truncate(MAX_TODOS);
    let mut next = out.iter().map(|t| t.id).max().unwrap_or(0) + 1;
    let mut seen = std::collections::HashSet::new();
    for t in &mut out {
        if t.id == 0 || !seen.insert(t.id) {
            t.id = next;
            seen.insert(next);
            next += 1;
        }
    }
    Some(out)
}

/// Load the list. Missing file → empty. Unreadable / corrupted → empty, with the bad file kept
/// as `todo.corrupt.json` so nothing is lost silently.
pub fn load_todos(path: &Path) -> Vec<Todo> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                log_warn!("today: cannot read todo.json: {e}");
            }
            return vec![];
        }
    };
    match parse_todos(&String::from_utf8_lossy(&bytes)) {
        Some(v) => v,
        None => {
            log_warn!("today: todo.json is corrupted; starting with an empty list");
            let _ = std::fs::rename(path, path.with_file_name("todo.corrupt.json"));
            vec![]
        }
    }
}

/// The file's bytes.
fn todos_json(todos: &[Todo]) -> Vec<u8> {
    serde_json::to_vec_pretty(todos).unwrap_or_default()
}

/// Write the list atomically (temp file + rename).
pub fn save_todos(path: &Path, todos: &[Todo]) -> std::io::Result<()> {
    crate::persist::write_atomic(path, &todos_json(todos))
}

/// Append an item. Returns its id, or why it was refused.
pub fn add_todo(todos: &mut Vec<Todo>, text: &str) -> Result<u64, &'static str> {
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return Err("empty text");
    }
    if todos.len() >= MAX_TODOS {
        return Err("to-do list is full");
    }
    let id = todos.iter().map(|t| t.id).max().unwrap_or(0) + 1;
    todos.push(Todo { id, text: text.chars().take(MAX_TODO_CHARS).collect(), done: false, due: None, fired: false });
    Ok(id)
}

/// Flip an item's done flag. Returns whether it exists.
pub fn toggle_todo(todos: &mut [Todo], id: u64) -> bool {
    match todos.iter_mut().find(|t| t.id == id) {
        Some(t) => {
            t.done = !t.done;
            true
        }
        None => false,
    }
}


// ---- weather ----------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sky {
    Clear,
    PartlyCloudy,
    Cloudy,
    Fog,
    Drizzle,
    Rain,
    Snow,
    Thunder,
}

/// WMO weather code (Open-Meteo `weather_code`) → sky and a label.
pub fn describe(code: i64) -> (Sky, (&'static str, &'static str)) {
    match code {
        0 => (Sky::Clear, ("Cerah", "Clear")),
        1 => (Sky::Clear, ("Cerah berawan tipis", "Mostly clear")),
        2 => (Sky::PartlyCloudy, ("Berawan sebagian", "Partly cloudy")),
        3 => (Sky::Cloudy, ("Berawan", "Overcast")),
        45 | 48 => (Sky::Fog, ("Berkabut", "Fog")),
        51 | 53 | 55 => (Sky::Drizzle, ("Gerimis", "Drizzle")),
        56 | 57 => (Sky::Drizzle, ("Gerimis dingin", "Freezing drizzle")),
        61 => (Sky::Rain, ("Hujan ringan", "Light rain")),
        63 => (Sky::Rain, ("Hujan sedang", "Rain")),
        65 => (Sky::Rain, ("Hujan lebat", "Heavy rain")),
        66 | 67 => (Sky::Rain, ("Hujan es", "Freezing rain")),
        71 | 73 | 75 | 77 => (Sky::Snow, ("Salju", "Snow")),
        80 => (Sky::Rain, ("Hujan lokal ringan", "Light showers")),
        81 => (Sky::Rain, ("Hujan lokal", "Showers")),
        82 => (Sky::Rain, ("Hujan lokal deras", "Heavy showers")),
        85 | 86 => (Sky::Snow, ("Hujan salju", "Snow showers")),
        95 => (Sky::Thunder, ("Badai petir", "Thunderstorm")),
        96 | 99 => (Sky::Thunder, ("Badai petir & es", "Thunderstorm with hail")),
        _ => (Sky::Cloudy, ("Berawan", "Cloudy")),
    }
}

/// Glyph and colour for a sky. Segoe Fluent has no weather set, so this is sun / cloud / bolt
/// (the mockup draws a cloud for rain too).
pub fn sky_icon(sky: Sky, night: bool) -> (&'static str, Rgba) {
    match sky {
        Sky::Clear if night => (icons::MOON, color::PURPLE),
        Sky::Clear => (icons::BRIGHTNESS, color::YELLOW),
        Sky::PartlyCloudy => (icons::CLOUD, color::TEXT2),
        Sky::Cloudy | Sky::Fog => (icons::CLOUD, color::TEXT2),
        Sky::Drizzle | Sky::Rain => (icons::CLOUD, color::TEAL),
        Sky::Snow => (icons::CLOUD, color::WHITE),
        Sky::Thunder => (icons::LIGHTNING, color::ORANGE),
    }
}

/// Rain codes: it is wet right now (no "rain soon" heads-up then).
pub fn is_wet(code: i64) -> bool {
    matches!(code, 51..=67 | 80..=82 | 95..=99)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Weather {
    pub temp_c: f64,
    pub code: i64,
    /// Local hour of the reading (for the day / night icon).
    pub hour: u32,
    /// `15.00`-style time of the reading.
    pub at: String,
    /// Highest rain probability (%) of the current and the next hourly slot.
    pub rain_pct: Option<u8>,
    /// The next hourly slot (`2026-11-24T15:00`), to raise the heads-up once per hour.
    pub next_slot: Option<String>,
}

/// Parse an Open-Meteo forecast response. `None` if the essentials (temperature, code) are
/// missing; the hourly part is optional.
pub fn parse_forecast(json: &str) -> Option<Weather> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let cur = v.get("current")?;
    let temp_c = cur.get("temperature_2m")?.as_f64()?;
    let code = cur.get("weather_code")?.as_i64()?;
    let time = cur.get("time").and_then(|t| t.as_str()).unwrap_or("");
    // "2026-11-24T14:45" → hour 14, "14.45"
    let hm = time.split('T').nth(1).unwrap_or("");
    let (hour, minute) = {
        let mut it = hm.split(':').map(|p| p.parse::<u32>().ok());
        (it.next().flatten().unwrap_or(12), it.next().flatten().unwrap_or(0))
    };
    let at = if hm.is_empty() { String::new() } else { i18n::tr_s(format!("{hour:02}.{minute:02}"), format!("{hour:02}:{minute:02}")) };

    let (mut rain_pct, mut next_slot) = (None, None);
    if let (Some(times), Some(probs)) = (
        v.pointer("/hourly/time").and_then(|t| t.as_array()),
        v.pointer("/hourly/precipitation_probability").and_then(|t| t.as_array()),
    ) {
        // Times are ISO local strings in one format, so string order is time order.
        let next = times.iter().position(|t| t.as_str().is_some_and(|t| t > time));
        if let Some(i) = next {
            let at_i = |i: usize| probs.get(i).and_then(|p| p.as_f64()).map(|p| p.clamp(0.0, 100.0) as u8);
            let this_slot = i.checked_sub(1).and_then(at_i);
            rain_pct = match (this_slot, at_i(i)) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
            next_slot = times[i].as_str().map(str::to_string);
        }
    }
    Some(Weather { temp_c, code, hour, at, rain_pct, next_slot })
}

/// Parse the first geocoding result: `(latitude, longitude)`.
pub fn parse_geocode(json: &str) -> Option<(f64, f64)> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let r = v.get("results")?.as_array()?.first()?;
    let (lat, lon) = (r.get("latitude")?.as_f64()?, r.get("longitude")?.as_f64()?);
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

pub fn forecast_url(lat: f64, lon: f64) -> String {
    format!("https://api.open-meteo.com/v1/forecast?latitude={lat:.4}&longitude={lon:.4}&current=temperature_2m,weather_code&hourly=precipitation_probability&timezone=auto")
}

pub fn geocode_url(city: &str) -> String {
    format!("https://geocoding-api.open-meteo.com/v1/search?name={}&count=1", crate::net::encode(city))
}

/// `27°`, in °F for imperial. Never `-0°`.
pub fn fmt_temp(c: f64, units: Units) -> String {
    let v = if units == Units::Imperial { c * 9.0 / 5.0 + 32.0 } else { c };
    let r = v.round();
    format!("{}°", if r == 0.0 { 0.0 } else { r })
}


// ---- to-do: add with a due time, edit, delete, snooze -----------------------------------------

/// Local wall-clock time of a Unix time.
fn local_of(secs: i64) -> Option<NaiveDateTime> {
    Local.timestamp_opt(secs, 0).single().map(|d| d.naive_local())
}

/// Unix time of a local wall-clock time (the earlier one when the clock was set back).
fn unix_of(dt: NaiveDateTime) -> Option<i64> {
    Local.from_local_datetime(&dt).earliest().map(|d| d.timestamp())
}

fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn now_local() -> NaiveDateTime {
    Local::now().naive_local()
}

/// Append an item, reading a due time out of its words (`rapat jam 3` -> text `rapat`, due
/// 15:00). Returns its id, or why it was refused.
pub fn add_with_due(todos: &mut Vec<Todo>, text: &str, now: NaiveDateTime) -> Result<u64, &'static str> {
    let capped: String = text.chars().take(MAX_TODO_CHARS * 2).collect();
    let parsed = due::parse(&capped, now);
    let id = add_todo(todos, &parsed.title)?;
    if let (Some(d), Some(t)) = (parsed.due.and_then(unix_of), todos.last_mut()) {
        t.due = Some(d);
    }
    Ok(id)
}

/// Replace an item's text; a due time in the new words replaces the old one (and arms the
/// reminder again), no time in them keeps it. Returns whether the item exists.
pub fn edit_todo(todos: &mut [Todo], id: u64, text: &str, now: NaiveDateTime) -> Result<bool, &'static str> {
    let capped: String = text.chars().take(MAX_TODO_CHARS * 2).collect();
    let parsed = due::parse(&capped, now);
    let title: String = parsed.title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return Err("empty text");
    }
    let Some(t) = todos.iter_mut().find(|t| t.id == id) else { return Ok(false) };
    t.text = title.chars().take(MAX_TODO_CHARS).collect();
    if let Some(d) = parsed.due.and_then(unix_of) {
        t.due = Some(d);
        t.fired = false;
    }
    Ok(true)
}

/// Remove an item. Returns whether it existed.
pub fn delete_todo(todos: &mut Vec<Todo>, id: u64) -> bool {
    let n = todos.len();
    todos.retain(|t| t.id != id);
    todos.len() != n
}

/// Forget an item's due time.
pub fn clear_due(todos: &mut [Todo], id: u64) -> bool {
    match todos.iter_mut().find(|t| t.id == id) {
        Some(t) => {
            t.due = None;
            t.fired = false;
            true
        }
        None => false,
    }
}

/// "Tunda": remind again `minutes` from `now`.
pub fn snooze_todo(todos: &mut [Todo], id: u64, now: i64, minutes: i64) -> bool {
    match todos.iter_mut().find(|t| t.id == id) {
        Some(t) => {
            t.due = Some(now + minutes * 60);
            t.fired = false;
            true
        }
        None => false,
    }
}

/// The reminder was shown or dismissed: do not raise it again for this due time.
pub fn mark_fired(todos: &mut [Todo], id: u64) -> bool {
    match todos.iter_mut().find(|t| t.id == id) {
        Some(t) => {
            t.fired = true;
            true
        }
        None => false,
    }
}

/// What the words would become: the text and the label of the due time (`Hari ini 15.00`), when
/// there is one. For the preview under the field and in the launcher.
pub fn preview(text: &str) -> Option<(String, String)> {
    let now = now_local();
    let p = due::parse(text, now);
    p.due.map(|d| (p.title, due::label(d, now, i18n::is_english())))
}

// ---- feature state ----------------------------------------------------------------------------

#[derive(Default)]
pub struct TodayState {
    todos: Vec<Todo>,
    loaded: bool,
    weather: Option<Weather>,
    /// The weather service failed on its last try.
    weather_failed: bool,
    /// Geocoded position of `today.city`: `(city as configured, lat, lon)`.
    geo: Option<(String, f64, f64)>,
    /// What the service was started for, to restart it when the place changes.
    svc_key: Option<(String, Option<u64>, Option<u64>)>,
    /// Hourly slot the rain heads-up was last raised for.
    rain_slot: Option<String>,
    /// The to-do whose reminder pill is showing, and since when (Unix seconds).
    reminding: Option<(u64, i64)>,
    remind_timer: Option<TimerId>,
    /// The to-do being edited in place.
    editing: Option<u64>,
    /// `hold_panel(true)` is in force: a text input of the card has the keyboard.
    holding: bool,
}

pub fn register(r: &mut Registry) {
    r.add_state(TodayState::default());
    r.add_tab(TabSpec {
        id: "today",
        order: order::TODAY,
        icon: icons::CALENDAR,
        label: ("Hari ini", "Today"),
        enabled: |c| c.today.enabled,
        build: panel,
        tick: None,
    });
    r.on_action("today", on_action);
    r.on_config(on_config);
    r.on_event(|app, ev| match ev {
        // "Hapus semua data": the settings app deletes todo.json right after.
        AppEvent::DataCleared => {
            app.cancel_save("today");
            let st = app.state_mut::<TodayState>();
            st.todos.clear();
            st.loaded = true;
            st.reminding = None;
            st.editing = None;
            app.clear_activity("today.due");
            holiday::on_data_cleared(app);
            reschedule(app);
            app.request_redraw(Redraw::Tab("today"));
        }
        // The keyboard goes back with the panel.
        AppEvent::PanelClosed => leave_inputs(app),
        AppEvent::TabHidden("today") => leave_inputs(app),
        _ => {}
    });
    r.add_ipc_handler(IpcKind::Todo, on_todo);
    r.add_service(ServiceSpec { name: "weather", enabled: |c| c.today.enabled && c.today.weather, run: weather_service });
    holiday::register(r);
}

/// The position of the configured city, for features that need it (prayer): the config's own
/// latitude/longitude, else what the weather service geocoded.
pub fn geocoded(app: &App) -> Option<(String, f64, f64)> {
    let t = &app.config().today;
    if let (Some(lat), Some(lon)) = (t.latitude, t.longitude) {
        return Some((t.city.clone(), lat, lon));
    }
    // What a service geocoded (the process-wide cache), else what it handed to the UI thread.
    if let Some((lat, lon)) = place::cached(&t.city) {
        return Some((t.city.clone(), lat, lon));
    }
    app.state::<TodayState>().geo.clone().filter(|g| g.0 == t.city)
}

/// Read-only view for "Tanya AI" (`features/ai`), asked only when the user turned that source on:
/// the to-dos that are not done yet, each as one line with its due time when it has one (see
/// [`ai_todo_line`]), and the weather as `(°C, WMO code)`. Reads the list from disk when the tab
/// never loaded it.
pub fn ai_view(app: &App) -> (Vec<String>, Option<(f64, i64)>) {
    let st = app.state::<TodayState>();
    let todos = if st.loaded { st.todos.clone() } else { load_todos(&moku_core::paths::todo_file()) };
    let (now, english) = (Local::now(), i18n::is_english());
    let open = todos.into_iter().filter(|t| !t.done).map(|t| ai_todo_line(&t.text, t.due, now, english)).collect();
    (open, st.weather.as_ref().map(|w| (w.temp_c, w.code)))
}

/// The longest line the AI context keeps per to-do (`features/ai/context.rs` cuts at 80).
const AI_TODO_CHARS: usize = 80;

/// One open to-do as the AI reads it: the title, then the due time in brackets when it has one,
/// `Kirim laporan (tenggat 09:00)` for today and `... (due 2026-11-26 09:00)` for another day (the
/// context already tells it today's date). The title is cut first, so a long one never pushes the
/// due time out of the context's own length cap. Pure.
pub(crate) fn ai_todo_line(text: &str, due: Option<i64>, now: chrono::DateTime<Local>, english: bool) -> String {
    let title = moku_core::aimem::clean(text);
    let Some(due) = due.and_then(|d| Local.timestamp_opt(d, 0).single()) else {
        return title;
    };
    let when = if due.date_naive() == now.date_naive() { due.format("%H:%M").to_string() } else { due.format("%Y-%m-%d %H:%M").to_string() };
    let suffix = format!(" ({} {when})", if english { "due" } else { "tenggat" });
    let room = AI_TODO_CHARS.saturating_sub(suffix.chars().count());
    let title = if title.chars().count() > room { format!("{}…", title.chars().take(room.saturating_sub(1)).collect::<String>()) } else { title };
    format!("{title}{suffix}")
}

fn ensure_loaded(app: &mut App) {
    let st = app.state_mut::<TodayState>();
    if !st.loaded {
        st.loaded = true;
        st.todos = load_todos(&moku_core::paths::todo_file());
    }
}

/// Save the list soon (debounced, written off the UI thread; flushed on exit).
fn save(app: &mut App) {
    app.save_later("today", |app| Some((moku_core::paths::todo_file(), todos_json(&app.state::<TodayState>().todos))));
}

fn on_config(app: &mut App) {
    ensure_loaded(app);
    let t = app.config().today.clone();
    let key = (t.city.clone(), t.latitude.map(f64::to_bits), t.longitude.map(f64::to_bits));
    let st = app.state_mut::<TodayState>();
    let first = st.svc_key.is_none();
    let moved = st.svc_key.as_ref().is_some_and(|k| *k != key);
    st.svc_key = Some(key);
    if moved {
        st.weather = None;
        st.weather_failed = false;
        st.geo = None;
        st.rain_slot = None;
    }
    if !t.enabled || !t.weather {
        st.weather = None;
        st.weather_failed = false;
    }
    if moved && !first {
        app.restart_service("weather");
    }
    holiday::on_config(app);
    reschedule(app);
    app.request_redraw(Redraw::Tab("today"));
}

/// Add a to-do from another feature (the launcher's `todo <text>`); saved like any other, with
/// its due time read from the words.
pub fn add(app: &mut App, text: &str) -> Result<(), &'static str> {
    add_text(app, text).map(|_| ())
}

fn add_text(app: &mut App, text: &str) -> Result<u64, &'static str> {
    ensure_loaded(app);
    let id = add_with_due(&mut app.state_mut::<TodayState>().todos, text, now_local())?;
    save(app);
    reschedule(app);
    app.request_redraw(Redraw::Tab("today"));
    Ok(id)
}

fn on_todo(app: &mut App, req: Request, reply: Reply) {
    let Request::Todo { add } = req else { return };
    match add_text(app, &add) {
        Ok(_) => reply.ok(),
        Err(e) => reply.error(e),
    }
}

// ---- actions ----------------------------------------------------------------------------------

fn on_action(app: &mut App, ev: &ActionEvent) {
    let id = ev.arg().and_then(|a| a.parse::<u64>().ok());
    match (ev.name(), ev.kind) {
        // The "Tambah to-do" field.
        ("add", ActionKind::Click) => {
            cancel_edit(app);
            begin_input(app, ADD_INPUT);
        }
        ("add", ActionKind::Edited) => app.request_redraw(Redraw::Tab("today")),
        ("add", ActionKind::Key(k)) => match k.key {
            Key::Enter => commit_add(app),
            Key::Escape => {
                app.input_mut(ADD_INPUT).clear();
                end_input(app, true);
                app.request_redraw(Redraw::Tab("today"));
            }
            _ => {}
        },
        // The user went elsewhere: the draft stays, the keyboard is already released.
        ("add", ActionKind::Blur) => {
            end_input(app, false);
            app.request_redraw(Redraw::Tab("today"));
        }
        // Editing a row.
        ("edit", ActionKind::Click) => begin_input(app, EDIT_INPUT),
        ("edit", ActionKind::Edited) => app.request_redraw(Redraw::Tab("today")),
        ("edit", ActionKind::Key(k)) => match k.key {
            Key::Enter => save_edit(app),
            Key::Escape => cancel_edit(app),
            _ => {}
        },
        ("edit", ActionKind::Blur) => {
            app.state_mut::<TodayState>().editing = None;
            end_input(app, false);
            app.request_redraw(Redraw::Tab("today"));
        }
        ("edit_start", ActionKind::Click) | ("row", ActionKind::DoubleClick) => {
            if let Some(id) = id {
                start_edit(app, id);
            }
        }
        ("edit_save", ActionKind::Click) => save_edit(app),
        ("edit_cancel", ActionKind::Click) => cancel_edit(app),
        ("clear_due", ActionKind::Click) => {
            if let Some(id) = app.state::<TodayState>().editing {
                ensure_loaded(app);
                if clear_due(&mut app.state_mut::<TodayState>().todos, id) {
                    save(app);
                    reschedule(app);
                    app.request_redraw(Redraw::Tab("today"));
                }
            }
        }
        ("todo", ActionKind::Click) => {
            if let Some(id) = id {
                ensure_loaded(app);
                if toggle_todo(&mut app.state_mut::<TodayState>().todos, id) {
                    save(app);
                    reschedule(app);
                    app.request_redraw(Redraw::Tab("today"));
                }
            }
        }
        ("del", ActionKind::Click) => {
            if let Some(id) = id {
                ensure_loaded(app);
                if app.state::<TodayState>().editing == Some(id) {
                    cancel_edit(app);
                }
                if delete_todo(&mut app.state_mut::<TodayState>().todos, id) {
                    save(app);
                    reschedule(app);
                    app.request_redraw(Redraw::Tab("today"));
                }
            }
        }
        ("clear_done", ActionKind::Click) => {
            ensure_loaded(app);
            app.state_mut::<TodayState>().todos.retain(|t| !t.done);
            save(app);
            reschedule(app);
            app.request_redraw(Redraw::Tab("today"));
        }
        // The reminder pill.
        ("due_done", ActionKind::Click) => finish_reminder(app, Finish::Done),
        ("due_snooze", ActionKind::Click) => finish_reminder(app, Finish::Snooze),
        ("due_dismiss", ActionKind::Click) => finish_reminder(app, Finish::Dismiss),
        // Test hook (`mokuctl debug click today.sample_rain`): feeds a wet forecast through the
        // same path as a real response.
        ("sample_rain", ActionKind::Click) => {
            let w = Weather { temp_c: 27.0, code: 3, hour: 14, at: tr("14.45", "14:45").into(), rain_pct: Some(80), next_slot: Some(format!("sample-{}", Local::now().timestamp())) };
            apply_weather(app, w, SAMPLE_ALERT_SECS);
        }
        // Test hook: a to-do that is due right now, through the real reminder path.
        ("sample_due", ActionKind::Click) => {
            ensure_loaded(app);
            let now = unix_now();
            let st = app.state_mut::<TodayState>();
            if let Ok(id) = add_todo(&mut st.todos, tr("Kirim laporan mingguan", "Send the weekly report")) {
                if let Some(t) = st.todos.iter_mut().find(|t| t.id == id) {
                    t.due = Some(now);
                }
            }
            save(app);
            reschedule(app);
            app.request_redraw(Redraw::Tab("today"));
        }
        // Test hook: invented holidays (`today.sample_holiday`, `:today`).
        ("sample_holiday", ActionKind::Click) => holiday::sample(app, ev.arg().unwrap_or("eve")),
        _ => {}
    }
}

// ---- the text inputs of the card --------------------------------------------------------------

/// Give an input the keyboard and keep the panel open while it has it.
fn begin_input(app: &mut App, id: &str) {
    app.focus_input(id);
    let st = app.state_mut::<TodayState>();
    if !st.holding {
        st.holding = true;
        app.hold_panel(true);
    }
    app.request_redraw(Redraw::Tab("today"));
}

/// Let go of the keyboard (`blur`: it is still ours; false when the core already released it) and
/// of the panel hold; the panel closes now if the pointer is outside.
fn end_input(app: &mut App, blur: bool) {
    if blur {
        app.blur_input();
    }
    let st = app.state_mut::<TodayState>();
    if st.holding {
        st.holding = false;
        app.hold_panel(false);
    }
}

/// The panel (or the tab) went away: no edit stays open, the keyboard goes back.
fn leave_inputs(app: &mut App) {
    let ours = app.focused_input().is_some_and(|i| i == ADD_INPUT || i == EDIT_INPUT);
    let st = app.state_mut::<TodayState>();
    st.editing = None;
    if ours || st.holding {
        end_input(app, ours);
    }
}

fn commit_add(app: &mut App) {
    let text = app.input(ADD_INPUT).text().to_string();
    if text.trim().is_empty() {
        return;
    }
    // A full list keeps the draft (the field tells why through the unchanged text).
    if add_text(app, &text).is_ok() {
        app.input_mut(ADD_INPUT).clear();
    }
    app.request_redraw(Redraw::Tab("today"));
}

fn start_edit(app: &mut App, id: u64) {
    ensure_loaded(app);
    let Some(text) = app.state::<TodayState>().todos.iter().find(|t| t.id == id).map(|t| t.text.clone()) else { return };
    app.state_mut::<TodayState>().editing = Some(id);
    app.input_mut(EDIT_INPUT).set_text(&text);
    begin_input(app, EDIT_INPUT);
}

fn save_edit(app: &mut App) {
    let Some(id) = app.state::<TodayState>().editing else { return };
    let text = app.input(EDIT_INPUT).text().to_string();
    ensure_loaded(app);
    if edit_todo(&mut app.state_mut::<TodayState>().todos, id, &text, now_local()).is_ok_and(|found| found) {
        save(app);
        reschedule(app);
    }
    app.state_mut::<TodayState>().editing = None;
    end_input(app, true);
    app.request_redraw(Redraw::Tab("today"));
}

fn cancel_edit(app: &mut App) {
    if app.state_mut::<TodayState>().editing.take().is_none() {
        return;
    }
    end_input(app, true);
    app.request_redraw(Redraw::Tab("today"));
}

// ---- reminders --------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Finish {
    Done,
    Snooze,
    Dismiss,
}

/// Selesai / Tunda 10 m / the cross on the pill.
fn finish_reminder(app: &mut App, how: Finish) {
    ensure_loaded(app);
    let Some((id, _)) = app.state_mut::<TodayState>().reminding.take() else { return };
    let now = unix_now();
    let st = app.state_mut::<TodayState>();
    match how {
        Finish::Done => {
            if let Some(t) = st.todos.iter_mut().find(|t| t.id == id) {
                t.done = true;
                t.fired = true;
            }
        }
        Finish::Snooze => {
            snooze_todo(&mut st.todos, id, now, remind::SNOOZE_MIN);
        }
        Finish::Dismiss => {
            mark_fired(&mut st.todos, id);
        }
    }
    app.clear_activity("today.due");
    save(app);
    reschedule(app);
    app.request_redraw(Redraw::All);
}

/// Bring the reminder pill and the wake-up timer in line with the list and the clock. Runs on the
/// UI thread after anything that changes either (a to-do added, edited, done, the timer itself).
fn reschedule(app: &mut App) {
    if let Some(t) = app.state_mut::<TodayState>().remind_timer.take() {
        app.cancel_timer(t);
    }
    if !app.config().today.enabled {
        app.state_mut::<TodayState>().reminding = None;
        app.clear_activity("today.due");
        return;
    }
    ensure_loaded(app);
    let now = unix_now();
    let st = app.state_mut::<TodayState>();
    // The pill that has been up too long counts as dismissed; one whose to-do was finished,
    // deleted or moved to another time goes away.
    if let Some((id, since)) = st.reminding {
        let still = st.todos.iter().any(|t| t.id == id && !t.done && !t.fired && t.due.is_some_and(|d| d <= now));
        if !still {
            st.reminding = None;
        } else if now - since >= remind::SHOW_SECS {
            mark_fired(&mut st.todos, id);
            st.reminding = None;
            save(app);
        }
    }
    let st = app.state_mut::<TodayState>();
    if st.reminding.is_none() {
        if let Some(id) = remind::pick(&st.todos, now) {
            st.reminding = Some((id, now));
        }
    }
    let showing = st.reminding;
    let next = remind::next_in(&st.todos, now);
    match showing {
        Some(_) => {
            if !app.has_activity("today.due") {
                app.set_activity(Activity::persistent("today.due", DUE_PRIORITY, due_pill).tab("today").bubble(|_| stack([icon(icons::BELL).color(color::ORANGE)])));
            }
        }
        None => app.clear_activity("today.due"),
    }
    // Nothing due and nothing on screen: no timer at all. Adding or editing a to-do calls `reschedule` again.
    if let Some(wait) = next_wake(next, showing.map(|(_, since)| since), now) {
        let t = app.after(Duration::from_secs(wait as u64), reschedule);
        app.state_mut::<TodayState>().remind_timer = Some(t);
    }
    app.request_redraw(Redraw::Pill);
}

/// Seconds until the reminder timer must wake the UI: for the next to-do that falls due (`next`, seconds from
/// now), and while a reminder pill is up for it to be counted as dismissed (`showing_since`). Never more than
/// [`remind::MAX_WAIT_SECS`] (a clock change or a sleep is looked at again then) and never less than a second.
/// `None` when nothing is due and nothing is showing: no timer is armed.
fn next_wake(next: Option<i64>, showing_since: Option<i64>, now: i64) -> Option<i64> {
    let showing = showing_since.map(|since| since + remind::SHOW_SECS - now);
    let wait = match (next, showing) {
        (Some(n), Some(s)) => n.min(s),
        (Some(n), None) => n,
        (None, Some(s)) => s,
        (None, None) => return None,
    };
    Some(wait.clamp(1, remind::MAX_WAIT_SECS))
}

/// The to-do's words, unless the privacy setting hides content during a meeting.
fn shown_text(app: &App, text: &str) -> String {
    if app.meeting().active && app.config().privacy.hide_notification_body_in_meetings { tr("Pengingat to-do", "To-do reminder").to_string() } else { text.to_string() }
}

fn due_pill(app: &App) -> Node {
    let st = app.state::<TodayState>();
    let Some(t) = st.reminding.and_then(|(id, _)| st.todos.iter().find(|t| t.id == id)) else { return empty() };
    let when = t.due.and_then(local_of).map(|d| i18n::clock(&d)).unwrap_or_default();
    let body = col([text(shown_text(app, &t.text)).semibold().max_w(250.0), caption(format!("To-do · {when}"))]).gap(0.0);
    row([
        icon(icons::BELL).color(color::ORANGE),
        body,
        button_primary(tr("Selesai", "Done"), "today.due_done"),
        button(tr("Tunda 10 m", "Snooze 10 m"), "today.due_snooze"),
        icon_button(icons::CLOSE, "today.due_dismiss").name(tr("Tutup", "Dismiss")),
    ])
    .gap(10.0)
    .h(50.0)
    .pad4(14.0, 0.0, 4.0, 0.0)
}

/// A new reading from the service (or the sample hook).
fn apply_weather(app: &mut App, w: Weather, alert_secs: f32) {
    let heads_up = app.config().today.weather && w.rain_pct.is_some_and(|p| p >= RAIN_ALERT_PCT) && !is_wet(w.code) && w.next_slot.is_some();
    let st = app.state_mut::<TodayState>();
    let fresh_slot = st.rain_slot != w.next_slot;
    if heads_up && fresh_slot {
        st.rain_slot = w.next_slot.clone();
    }
    let (pct, temp) = (w.rain_pct.unwrap_or(0), w.temp_c);
    st.weather = Some(w);
    st.weather_failed = false;
    if heads_up && fresh_slot {
        let units = app.config().today.units;
        let a = Activity::transient("today.rain", priority::HEADS_UP, alert_secs, move |_| {
            pill([icon(icons::CLOUD).color(color::TEAL), text(tr("Hujan sebentar lagi", "Rain soon")).semibold(), caption(format!("{pct}%")), text(fmt_temp(temp, units)).secondary()])
        })
        .tab("today");
        app.set_activity(a);
    }
    app.request_redraw(Redraw::Tab("today"));
}

// ---- weather service --------------------------------------------------------------------------

fn weather_service(ctx: ServiceCtx) {
    let mut fails = 0u32;
    loop {
        let wait = match fetch(&ctx) {
            Ok(()) => {
                fails = 0;
                REFRESH
            }
            Err(e) => {
                fails += 1;
                log_warn!("weather: {e}");
                ctx.post(|app| {
                    app.state_mut::<TodayState>().weather_failed = true;
                    app.request_redraw(Redraw::Tab("today"));
                });
                (RETRY * 2u32.saturating_pow(fails - 1)).min(REFRESH)
            }
        };
        if !ctx.sleep(wait) {
            return;
        }
    }
}

fn fetch(ctx: &ServiceCtx) -> Result<(), String> {
    let (lat, lon) = place::resolve_and_publish(ctx)?;
    let body = crate::net::get_text(&forecast_url(lat, lon), HTTP_TIMEOUT)?;
    let w = parse_forecast(&body).ok_or("unexpected forecast response")?;
    ctx.post(move |app| apply_weather(app, w, HEADS_UP_SECS));
    Ok(())
}

// ---- panel ------------------------------------------------------------------------------------

fn checkbox(done: bool) -> Node {
    if done {
        stack([icon(icons::CHECK).font_size(10.0).color(color::INK)]).size(18.0, 18.0).bg(color::GREEN).capsule()
    } else {
        gap_px(18.0).capsule().border(color::TEXT3)
    }
}

/// A small glyph button of a to-do row (22 x 24 target).
fn row_button(glyph: &str, action: String, name: &str) -> Node {
    stack([icon(glyph).font_size(12.0).color(color::TEXT3)]).size(22.0, 24.0).action(action).name(name)
}

/// The label of a to-do's due time and its colour: red when overdue, orange within the hour.
fn due_text(t: &Todo, now: i64) -> Option<(String, Rgba)> {
    let d = t.due?;
    let label = due::label(local_of(d)?, local_of(now)?, i18n::is_english());
    let c = if t.done {
        color::TEXT3
    } else if remind::overdue(t, now) {
        color::RED
    } else if d - now < 3600 {
        color::ORANGE
    } else {
        color::TEXT3
    };
    Some((label, c))
}

fn todo_row(t: &Todo, now: i64) -> Node {
    let c = if t.done { color::TEXT3 } else { color::WHITE };
    let mut body = vec![text(t.text.clone()).color(c)];
    let due = due_text(t, now);
    if let Some((label, lc)) = &due {
        body.push(caption(label.clone()).color(*lc));
    }
    row([
        stack([checkbox(t.done)]).size(24.0, 24.0).action(format!("today.todo:{}", t.id)).name(tr("Selesai", "Done")),
        col(body).gap(0.0).grow(1.0).action(format!("today.row:{}", t.id)),
        row_button(PENCIL, format!("today.edit_start:{}", t.id), tr("Ubah", "Edit")),
        row_button(icons::CLOSE, format!("today.del:{}", t.id), tr("Hapus", "Delete")),
    ])
    .gap(4.0)
    .fill_w()
    .h(if due.is_some() { 34.0 } else { 26.0 })
}

/// The row being edited: the field with Simpan / Batal, and what the reminder is now.
fn edit_row(app: &App, t: &Todo, now: i64) -> Node {
    let field = text_input(EDIT_INPUT, app.input(EDIT_INPUT)).font_size(13.0).accent(app.base_accent()).grow(1.0).action(EDIT_INPUT);
    let mut kids = vec![
        row([field, icon_button(icons::CHECK, "today.edit_save").name(tr("Simpan", "Save")), icon_button(icons::CLOSE, "today.edit_cancel").name(tr("Batal", "Cancel"))])
            .gap(0.0)
            .fill_w()
            .pad4(10.0, 1.0, 0.0, 1.0)
            .bg(color::FILL2)
            .radius(10.0),
    ];
    // What the typed words say, else what the reminder is now.
    let typed = preview(app.input(EDIT_INPUT).text()).map(|(_, label)| label);
    match (typed, due_text(t, now)) {
        (Some(label), _) => kids.push(caption(tr_s(format!("Pengingat baru: {label}"), format!("New reminder: {label}"))).color(app.base_accent())),
        (None, Some((label, _))) => kids.push(row([caption(tr_s(format!("Pengingat: {label}"), format!("Reminder: {label}"))), spacer(), text(tr("Hapus waktu", "Remove time")).font_size(11.0).tertiary().pad_xy(4.0, 2.0).action("today.clear_due")]).fill_w()),
        (None, None) => kids.push(caption(tr("Tambah waktu: jam 3, besok 09:00, 2 jam lagi", "Add a time: 3pm, tomorrow 9am, in 2 hours"))),
    }
    col(kids).gap(2.0).fill_w()
}

/// "+ Tambah to-do" with a live preview of the due time it understood.
fn add_field(app: &App) -> Node {
    let edit = app.input(ADD_INPUT);
    let focused = app.focused_input() == Some(ADD_INPUT);
    let accent = app.base_accent();
    let field = row([
        icon(icons::ADD).font_size(12.0).color(if focused { accent } else { color::TEXT2 }),
        text_input(ADD_INPUT, edit).placeholder(tr("Tambah to-do…", "Add a to-do…")).font_size(13.0).accent(accent).grow(1.0),
    ])
    .gap(8.0)
    .fill_w()
    .pad_xy(10.0, 5.0)
    .bg(color::FILL2)
    .radius(10.0)
    .action(ADD_INPUT);
    let hint = if edit.is_empty() {
        focused.then(|| caption(tr("mis. rapat jam 3 · besok 09:00 · 2 jam lagi", "e.g. meeting 3pm · tomorrow 9am · in 2 hours")))
    } else {
        preview(edit.text()).map(|(_, label)| caption(tr_s(format!("Pengingat: {label}"), format!("Reminder: {label}"))).color(accent))
    };
    match hint {
        Some(h) => col([field, h]).gap(3.0).fill_w(),
        None => field,
    }
}

fn todo_card(app: &App, visible: usize) -> Node {
    let st = app.state::<TodayState>();
    let now = unix_now();
    let mut pending: Vec<&Todo> = st.todos.iter().filter(|t| !t.done).collect();
    let done: Vec<&Todo> = st.todos.iter().filter(|t| t.done).collect();
    let any_done = !done.is_empty();
    pending.extend(done);
    let mut kids = vec![row([label(tr("To-do hari ini", "To-dos today")), spacer(), if any_done { text(tr("Hapus selesai", "Clear done")).font_size(11.0).tertiary().pad_xy(4.0, 2.0).action("today.clear_done") } else { empty() }]).fill_w()];
    if pending.is_empty() {
        // A friendly empty state, so the column never looks broken.
        kids.push(
            row([
                stack([icon(icons::CHECK).font_size(13.0).color(color::GREEN)]).size(28.0, 28.0).bg(color::GREEN.alpha(0.16)).capsule(),
                col([text(tr("Semua beres", "All clear")).semibold(), caption(tr("Belum ada tugas untuk hari ini", "Nothing on your list today"))]).gap(1.0).grow(1.0),
            ])
            .gap(10.0)
            .fill_w()
            .pad_xy(0.0, 4.0),
        );
    }
    let hidden = pending.len().saturating_sub(visible);
    for t in pending.into_iter().take(visible) {
        kids.push(if st.editing == Some(t.id) { edit_row(app, t, now) } else { todo_row(t, now) });
    }
    if hidden > 0 {
        kids.push(caption(tr_s(format!("+{hidden} lainnya"), format!("+{hidden} more"))));
    }
    kids.push(add_field(app));
    col(kids).gap(4.0).fill_w().bg(color::FILL).radius(typo::RADIUS_CARD).pad(14.0).align(Align::Stretch)
}

fn weather_card(app: &App) -> Node {
    let st = app.state::<TodayState>();
    let cfg = &app.config().today;
    let city = cfg.city.clone();
    let body = match &st.weather {
        Some(w) => {
            let (sky, name) = describe(w.code);
            let night = !(6..18).contains(&w.hour);
            let (glyph, c) = sky_icon(sky, night);
            let sub = if w.at.is_empty() { format!("{city} · {}", i18n::pair(name).to_lowercase()) } else { format!("{city} · {} {}", i18n::pair(name).to_lowercase(), w.at) };
            row([icon(glyph).font_size(26.0).color(c), col([text(fmt_temp(w.temp_c, cfg.units)).font_size(26.0).bold().display(), caption(sub)]).gap(0.0).grow(1.0)]).gap(12.0)
        }
        None => {
            let msg = if st.weather_failed { tr("Cuaca tidak tersedia", "Weather unavailable") } else { tr("Memuat cuaca…", "Loading weather…") };
            row([icon(icons::CLOUD).font_size(26.0).color(color::TEXT3), col([text(msg).secondary(), caption(city)]).gap(1.0)]).gap(12.0)
        }
    };
    // The glance chips (exchange rate, air quality) sit under the weather, in the same card.
    let inner = match super::glance::chips(app) {
        Some(chips) => col([body.fill_w(), chips]).gap(8.0).fill_w().align(Align::Stretch),
        None => body.fill_w(),
    };
    inner.bg(color::FILL).radius(typo::RADIUS_CARD).pad_xy(14.0, 12.0)
}

/// What goes under the weekday and the date: the Hijri date (feature `hijri`, track N; nothing while it
/// is switched off), then the holiday badge.
fn header_extras(app: &App) -> Vec<Node> {
    super::hijri::header_line(app).into_iter().chain(holiday::badge(app)).collect()
}

/// What the left column of the tab may spend: the date card with `rows` agenda rows (and the "next day off"
/// line when `next`), then the BMKG card.
fn left_column(app: &App, rows: usize, next: bool, alerts: Option<Node>) -> Node {
    let mut left = vec![date_card(app, rows, next).grow(1.0)];
    left.extend(alerts);
    col(left).gap(10.0).grow(1.0).align(Align::Stretch)
}

fn date_card(app: &App, agenda_rows: usize, next: bool) -> Node {
    let today = Local::now().date_naive();
    let monday = today.checked_sub_days(Days::new(today.weekday().num_days_from_monday() as u64)).unwrap_or(today);
    let accent = app.base_accent();
    let cells = (0..7u64).map(|i| {
        let d = monday.checked_add_days(Days::new(i)).unwrap_or(monday);
        let is_today = d == today;
        let letter: String = i18n::weekday_short(i as u32).chars().take(1).collect();
        let num = text(d.day().to_string()).semibold().color(if is_today { color::WHITE } else { color::TEXT2 }).text_center();
        let num = if is_today { stack([num]).size(28.0, 28.0).bg(accent).capsule() } else { stack([num]).size(28.0, 28.0) };
        col([caption(letter), num]).gap(4.0).align(Align::Center).grow(1.0)
    });
    let mut kids = vec![caption(i18n::weekday_long(today.weekday().num_days_from_monday())), title(format!("{} {}", today.day(), i18n::month_long(today.month())))];
    kids.extend(header_extras(app));
    kids.push(gap_px(6.0));
    kids.push(row(cells).fill_w());
    // Today's agenda (calendar feature) under the week strip, as in the mockup.
    if let Some(agenda) = super::calendar::today_section(app, agenda_rows) {
        kids.push(gap_px(8.0));
        kids.push(agenda);
    }
    if next && let Some(line) = holiday::next_line(app) {
        kids.push(gap_px(6.0));
        kids.push(line);
    }
    col(kids).gap(2.0).bg(color::FILL).radius(typo::RADIUS_CARD).pad(14.0).align(Align::Stretch)
}

/// What gives way, in this order, until a column fits: the "next day off" line (the least urgent), then
/// agenda rows (down to none: only "+N lainnya"), then to-do rows. `fits(rows, next, todos)` says whether
/// that choice fits. Pure (the caller measures), so the order is tested. Returns `(agenda rows, next line,
/// to-do rows)`.
pub fn fit_panel(max_rows: usize, max_todos: usize, fits: impl Fn(usize, bool, usize) -> bool) -> (usize, bool, usize) {
    let (mut rows, mut next, mut todos) = (max_rows, true, max_todos);
    while !fits(rows, next, todos) {
        if next {
            next = false;
        } else if rows > 0 {
            rows -= 1;
        } else if todos > 1 {
            todos -= 1;
        } else {
            break;
        }
    }
    (rows, next, todos)
}

/// Width of one of the two columns of the tab (610 DIP body, 12 DIP between).
const COLUMN_W: f32 = (610.0 - 12.0) / 2.0;

fn panel(app: &App) -> Node {
    let weather = app.config().today.weather;
    let right = |todos: usize| {
        let mut right = vec![];
        if weather {
            right.push(weather_card(app));
        } else if let Some(chips) = super::glance::chips(app) {
            right.push(chips.fill_w().bg(color::FILL).radius(typo::RADIUS_CARD).pad_xy(14.0, 10.0));
        }
        right.push(todo_card(app, todos));
        col(right).gap(10.0).grow(1.0).align(Align::Stretch)
    };
    // The panel body is 360 DIP high at most and clips what is below: a Hijri line, a holiday badge, three
    // agenda rows and a BMKG card on the left, weather, chips and to-dos on the right do not all fit. Measure
    // with the real text layout and let the agenda rows, the "next day off" line and the to-do rows give way.
    let max_todos = if weather { VISIBLE_TODOS_WITH_WEATHER } else { VISIBLE_TODOS };
    let fits = |rows: usize, next: bool, todos: usize| {
        let left = left_column(app, rows, next, super::bmkg::section(app));
        app.measure_height(&left, COLUMN_W) <= crate::scene::PANEL_BODY_MAX_H && app.measure_height(&right(todos), COLUMN_W) <= crate::scene::PANEL_BODY_MAX_H
    };
    let (rows, next, todos) = fit_panel(super::calendar::VISIBLE_ROWS, max_todos, fits);
    col([row([left_column(app, rows, next, super::bmkg::section(app)), right(todos)]).gap(12.0).fill_w().align(Align::Stretch)])
}

#[cfg(test)]
#[path = "today/todo_tests.rs"]
mod todo_tests;

#[cfg(test)]
mod wake_tests {
    use super::*;

    #[test]
    fn the_reminder_timer_is_armed_only_for_something_that_is_due() {
        // No due to-do, nothing on screen: no timer, however long the app runs.
        assert_eq!(next_wake(None, None, 1_000), None);
        // The next one in 90 s: wake then. In two hours: wake at the cap, to look at the clock again.
        assert_eq!(next_wake(Some(90), None, 1_000), Some(90));
        assert_eq!(next_wake(Some(7_200), None, 1_000), Some(remind::MAX_WAIT_SECS));
        // Already due (0 or negative): at once, but never a busy loop.
        assert_eq!(next_wake(Some(0), None, 1_000), Some(1));
        assert_eq!(next_wake(Some(-30), None, 1_000), Some(1));
        // A pill on screen counts as dismissed after SHOW_SECS: wake then, or for the next due one if sooner.
        assert_eq!(next_wake(None, Some(1_000), 1_100), Some(remind::SHOW_SECS - 100).map(|s| s.min(remind::MAX_WAIT_SECS)));
        assert_eq!(next_wake(Some(30), Some(1_000), 1_100), Some(30));
        assert_eq!(next_wake(None, Some(1_000), 1_000 + remind::SHOW_SECS + 5), Some(1), "overdue: look now");
    }
}

#[cfg(test)]
mod fit_tests {
    use super::*;

    /// The worst case of the tab with the heights it has at 100 %: Hijri line, holiday badge, three agenda
    /// rows, a tsunami card from BMKG on the left; weather (with chips) and four to-dos on the right.
    /// Left = 214 (the date card without the agenda, the BMKG card and the gaps) + the agenda block (label 20,
    /// 39 per row, a 21 "+N lainnya" row) + 18 for the "next day off" line; right = 150 + 40 per to-do.
    fn worst_left(rows: usize, hidden: usize, next: bool) -> f32 {
        214.0 + 20.0 + 39.0 * rows as f32 + if hidden > 0 { 21.0 } else { 0.0 } + if next { 18.0 } else { 0.0 }
    }
    fn worst_right(todos: usize) -> f32 {
        150.0 + 40.0 * todos as f32
    }
    const ROOM: f32 = 360.0;

    #[test]
    fn nothing_gives_way_while_everything_fits() {
        assert_eq!(fit_panel(3, 4, |_, _, _| true), (3, true, 4));
        // A quiet day: no holiday, no BMKG, a short agenda.
        assert_eq!(fit_panel(3, 4, |rows, next, todos| 150.0 + 39.0 * rows as f32 + if next { 18.0 } else { 0.0 } <= ROOM && worst_right(todos) <= ROOM), (3, true, 4));
    }

    #[test]
    fn the_worst_case_gives_up_the_next_day_off_line_first_and_nothing_is_clipped() {
        // 3 rows and the "next day off" line would be 214 + 20 + 117 + 18 = 369 > 360: the line goes (351).
        let fits = |rows: usize, next: bool, todos: usize| worst_left(rows, 3 - rows, next) <= ROOM && worst_right(todos) <= ROOM;
        let (rows, next, todos) = fit_panel(3, 5, fits);
        assert_eq!((rows, next), (3, false), "the least urgent line gives way before the agenda");
        assert!(worst_left(rows, 3 - rows, next) <= ROOM);
        // A little tighter (a wrapped holiday name): the agenda gives up a row, shown as "+N lainnya".
        let tighter = |rows: usize, next: bool, todos: usize| worst_left(rows, 3 - rows, next) + 20.0 <= ROOM && worst_right(todos) <= ROOM;
        assert_eq!(fit_panel(3, 5, tighter).0, 2);
        assert_eq!(todos, 5, "to-dos fit with the weather card: 150 + 40 x 5 = 350");
        // With the weather card taller (chips), the to-dos give way too, one row at a time.
        let tall = |todos: usize| 190.0 + 40.0 * todos as f32;
        let (_, _, todos) = fit_panel(3, 5, |rows, next, t| worst_left(rows, 3 - rows, next) <= ROOM && tall(t) <= ROOM);
        assert_eq!(todos, 4, "190 + 40 x 4 = 350");
    }

    #[test]
    fn the_order_is_the_next_day_off_line_then_rows_then_to_dos_and_it_always_ends() {
        // Nothing fits until the agenda is gone and the "next day off" line too.
        let (rows, next, _) = fit_panel(3, 4, |rows, next, _| rows == 0 && !next);
        assert_eq!((rows, next), (0, false));
        // The order of the steps taken.
        let seen = std::cell::RefCell::new(vec![]);
        let _ = fit_panel(2, 3, |r, n, t| {
            seen.borrow_mut().push((r, n, t));
            false
        });
        assert_eq!(*seen.borrow(), vec![(2, true, 3), (2, false, 3), (1, false, 3), (0, false, 3), (0, false, 2), (0, false, 1)]);
        // Nothing can ever make it fit: it stops at the smallest choice instead of looping.
        assert_eq!(fit_panel(3, 4, |_, _, _| false), (0, false, 1));
        assert_eq!(fit_panel(0, 1, |_, _, _| false), (0, false, 1));
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moku-today-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_ai_sees_open_todos_with_their_due_time() {
        let at = |y, mo, d, h, mi| Local.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap();
        let now = at(2026, 11, 25, 8, 0);
        let ts = |dt: chrono::DateTime<Local>| Some(dt.timestamp());
        // No due time: just the title (cleaned), as before.
        assert_eq!(ai_todo_line("  Beli\tkopi ", None, now, false), "Beli kopi");
        // Today: the clock only. Another day: the date as well. The word follows the language.
        assert_eq!(ai_todo_line("Kirim laporan", ts(at(2026, 11, 25, 9, 0)), now, false), "Kirim laporan (tenggat 09:00)");
        assert_eq!(ai_todo_line("Send report", ts(at(2026, 11, 26, 15, 30)), now, true), "Send report (due 2026-11-26 15:30)");
        // A long title is cut, never the due time, and the line stays within the context's cap.
        let long = ai_todo_line(&"x".repeat(300), ts(at(2026, 11, 26, 9, 0)), now, true);
        assert!(long.ends_with("(due 2026-11-26 09:00)") && long.contains('…'), "{long}");
        assert!(long.chars().count() <= AI_TODO_CHARS, "{}", long.chars().count());
        // An unusable timestamp is no due time.
        assert_eq!(ai_todo_line("a", Some(i64::MAX), now, true), "a");
    }

    #[test]
    fn todo_roundtrip() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("todo.json");
        assert!(load_todos(&path).is_empty(), "no file = empty list");

        let mut list = vec![];
        let a = add_todo(&mut list, "  Bayar   tagihan\tinternet ").unwrap();
        let b = add_todo(&mut list, "Review PR #214").unwrap();
        assert_eq!(list[0].text, "Bayar tagihan internet", "whitespace is normalised");
        assert_ne!(a, b);
        assert!(toggle_todo(&mut list, a));
        assert!(!toggle_todo(&mut list, 999));
        save_todos(&path, &list).unwrap();
        assert!(!path.with_extension("json.tmp").exists(), "temp file is renamed away");

        let back = load_todos(&path);
        assert_eq!(back, list);
        assert!(back[0].done && !back[1].done);

        // Toggling twice restores; a new item's id is the highest id + 1.
        let mut back = back;
        toggle_todo(&mut back, a);
        assert!(!back[0].done);
        back.retain(|t| t.id != b);
        assert_eq!(add_todo(&mut back, "baru").unwrap(), a + 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupted_todo_file_is_an_empty_list_not_a_crash() {
        let dir = temp_dir("corrupt");
        let path = dir.join("todo.json");
        std::fs::write(&path, "{ this is not json ]").unwrap();
        assert!(load_todos(&path).is_empty());
        assert!(dir.join("todo.corrupt.json").exists(), "the bad file is kept aside");
        assert!(!path.exists());
        // The list works again afterwards.
        let mut l = vec![];
        add_todo(&mut l, "ok").unwrap();
        save_todos(&path, &l).unwrap();
        assert_eq!(load_todos(&path).len(), 1);

        // Wrong shape, binary junk, BOM, empty file.
        std::fs::write(&path, r#"{"items": []}"#).unwrap();
        assert!(load_todos(&path).is_empty());
        std::fs::write(&path, [0xffu8, 0xfe, 0x00, 0x12]).unwrap();
        assert!(load_todos(&path).is_empty());
        std::fs::write(&path, "\u{feff}[]").unwrap();
        assert_eq!(load_todos(&path), vec![]);
        std::fs::write(&path, "").unwrap();
        assert_eq!(load_todos(&path), vec![]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn todo_file_with_bad_entries_keeps_the_good_ones_and_repairs_ids() {
        let parsed = parse_todos(r#"[{"text":"a","done":true,"id":4}, 7, {"text":""}, {"nope":1}, {"text":"b"}, {"text":"c","id":4}]"#).unwrap();
        assert_eq!(parsed.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert!(parsed[0].done);
        let ids: std::collections::HashSet<u64> = parsed.iter().map(|t| t.id).collect();
        assert_eq!(ids.len(), 3, "ids are unique: {parsed:?}");
        assert!(ids.iter().all(|i| *i > 0));
    }

    #[test]
    fn todo_add_is_bounded() {
        let mut l = vec![];
        assert!(add_todo(&mut l, "   ").is_err());
        let long = "x".repeat(10_000);
        add_todo(&mut l, &long).unwrap();
        assert_eq!(l[0].text.chars().count(), MAX_TODO_CHARS);
        for i in 0..MAX_TODOS {
            let _ = add_todo(&mut l, &format!("t{i}"));
        }
        assert_eq!(l.len(), MAX_TODOS);
        assert!(add_todo(&mut l, "one too many").is_err());
    }

    #[test]
    fn weather_code_to_icon() {
        assert_eq!(describe(0).0, Sky::Clear);
        assert_eq!(sky_icon(describe(0).0, false).0, icons::BRIGHTNESS);
        assert_eq!(sky_icon(Sky::Clear, true).0, icons::MOON, "clear at night is a moon");
        assert_eq!(describe(2).0, Sky::PartlyCloudy);
        assert_eq!(describe(3).0, Sky::Cloudy);
        assert_eq!(describe(45).0, Sky::Fog);
        for code in [51, 53, 55, 56, 57] {
            assert_eq!(describe(code).0, Sky::Drizzle, "{code}");
        }
        for code in [61, 63, 65, 66, 67, 80, 81, 82] {
            assert_eq!(describe(code).0, Sky::Rain, "{code}");
            assert_eq!(sky_icon(Sky::Rain, false).0, icons::CLOUD);
        }
        for code in [71, 73, 75, 77, 85, 86] {
            assert_eq!(describe(code).0, Sky::Snow, "{code}");
        }
        for code in [95, 96, 99] {
            assert_eq!(describe(code).0, Sky::Thunder, "{code}");
        }
        assert_eq!(sky_icon(Sky::Thunder, false).0, icons::LIGHTNING);
        // Anything else is a cloud, never a panic.
        assert_eq!(describe(1234).0, Sky::Cloudy);
        assert_eq!(describe(-1).0, Sky::Cloudy);
        // Labels as in the mockup ("hujan ringan"), in both languages.
        assert_eq!(describe(61).1, ("Hujan ringan", "Light rain"));
        assert!(is_wet(61) && is_wet(95) && !is_wet(3) && !is_wet(0));
    }

    const FORECAST: &str = r#"{
        "latitude": -6.25, "longitude": 106.875, "timezone": "Asia/Jakarta",
        "current": {"time": "2026-11-24T14:45", "interval": 900, "temperature_2m": 27.3, "weather_code": 3},
        "hourly": {"time": ["2026-11-24T13:00","2026-11-24T14:00","2026-11-24T15:00","2026-11-24T16:00"],
                   "precipitation_probability": [10, 20, 70, 40]}
    }"#;

    #[test]
    fn forecast_parses_current_and_next_hour_rain() {
        let w = parse_forecast(FORECAST).unwrap();
        assert_eq!(w.temp_c, 27.3);
        assert_eq!(w.code, 3);
        assert_eq!(w.hour, 14);
        assert_eq!(w.at, "14.45");
        assert_eq!(w.rain_pct, Some(70), "max of the current (14:00) and the next (15:00) slot");
        assert_eq!(w.next_slot.as_deref(), Some("2026-11-24T15:00"));
        assert_eq!(fmt_temp(w.temp_c, Units::Metric), "27°");
        assert_eq!(fmt_temp(w.temp_c, Units::Imperial), "81°");
        assert_eq!(fmt_temp(-0.2, Units::Metric), "0°");
    }

    #[test]
    fn forecast_tolerates_missing_or_odd_data() {
        // No hourly block: still a reading, no rain info.
        let w = parse_forecast(r#"{"current":{"time":"2026-11-24T09:00","temperature_2m":20,"weather_code":0}}"#).unwrap();
        assert_eq!((w.rain_pct, w.next_slot), (None, None));
        // Nulls in the probabilities, hourly starting after now.
        let w = parse_forecast(r#"{"current":{"time":"2026-11-24T09:10","temperature_2m":20.0,"weather_code":0},"hourly":{"time":["2026-11-24T10:00"],"precipitation_probability":[null]}}"#).unwrap();
        assert_eq!(w.rain_pct, None);
        // Garbage / missing essentials.
        assert!(parse_forecast("").is_none());
        assert!(parse_forecast("<html>").is_none());
        assert!(parse_forecast(r#"{"current":{"temperature_2m":20}}"#).is_none());
        assert!(parse_forecast(r#"{"error":true,"reason":"nope"}"#).is_none());
    }

    #[test]
    fn geocoding_and_urls() {
        assert_eq!(parse_geocode(r#"{"results":[{"name":"Jakarta","latitude":-6.21462,"longitude":106.84513}]}"#), Some((-6.21462, 106.84513)));
        assert_eq!(parse_geocode(r#"{"generationtime_ms":0.5}"#), None, "city not found has no results");
        assert_eq!(parse_geocode(r#"{"results":[{"latitude":123.0,"longitude":1.0}]}"#), None);
        assert_eq!(geocode_url("Kota Bandung"), "https://geocoding-api.open-meteo.com/v1/search?name=Kota%20Bandung&count=1");
        assert_eq!(
            forecast_url(-6.2088, 106.8456),
            "https://api.open-meteo.com/v1/forecast?latitude=-6.2088&longitude=106.8456&current=temperature_2m,weather_code&hourly=precipitation_probability&timezone=auto"
        );
    }
}
