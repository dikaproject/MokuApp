//! Feature `calendar` (v0.2, track J): the agenda from iCal (ICS) addresses.
//!
//! * **Source**: ICS addresses the user pastes into Settings (Google "Secret address in iCal
//!   format", Outlook "Publish calendar", any `.ics`, or a local `file:` path); several are
//!   allowed, no account and no OAuth. The address is a secret: it is stored as typed in
//!   `config.toml`, shown masked in Settings, and **never logged**; neither are event titles.
//! * **Fetch**: a service thread loads the addresses every 15 minutes with [`crate::net`] (the
//!   only network traffic of this feature, only while it is on), parses them ([`ics`]) and expands
//!   the next 14 days. The result (a few hundred small records) goes to the UI thread; the raw
//!   text is dropped.
//! * **Heads-up**: `calendar.minutes_before` (default 5) before a meeting a pill "Rapat 5 menit
//!   lagi · title" appears (priority 75) with "Gabung" when the event has a Meet, Zoom or Teams
//!   link. A single UI timer wakes for the next alert; nothing polls.
//! * **Agenda**: today's events are shown in the "Hari ini" tab ([`today_section`]).

pub(crate) mod ics;
mod links;
mod rrule;
mod tz;

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Local, NaiveTime, TimeZone};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

use self::ics::{HORIZON_DAYS, Occurrence};
use crate::activity::Activity;
use crate::app::{App, Redraw, TimerId};
use crate::i18n::{self, tr, tr_s};
use crate::registry::*;
use crate::services::ServiceCtx;
use crate::ui::*;

/// Fetch period (plan).
const REFRESH: Duration = Duration::from_secs(15 * 60);
/// First retry after a failed round (grows up to [`REFRESH`]).
const RETRY: Duration = Duration::from_secs(2 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
/// Largest ICS file read from disk (the network limit of `net` is the same).
const MAX_FILE: u64 = crate::net::MAX_BODY as u64;
/// The heads-up pill (plan: priority 75, between the volume pill and notifications).
const ALERT_PRIORITY: u8 = 75;
/// The heads-up stays this long after the start ("Rapat dimulai").
const GRACE_SECS: i64 = 120;
/// Longest sleep between two looks at the agenda (guards against clock jumps and sleep).
const MAX_WAIT_SECS: i64 = 600;
/// Rows of the agenda in the Hari ini tab at most; more are summarised as "+N lainnya". The tab gives
/// fewer when the card would not fit (a holiday, the Hijri date, a BMKG card ...): see `today::fit_panel`.
pub const VISIBLE_ROWS: usize = 3;

#[derive(Default)]
struct CalendarState {
    /// Occurrences from the start of the day of the last fetch to 14 days ahead, by start.
    agenda: Vec<Occurrence>,
    /// A fetch worked at least once (for the empty states).
    loaded: bool,
    /// The last round failed for every address.
    failed: bool,
    /// Heads-ups the user closed, by occurrence key.
    dismissed: HashSet<u64>,
    /// The occurrence the heads-up pill shows.
    alerting: Option<u64>,
    timer: Option<TimerId>,
    /// What the service was started for (a hash of the addresses), to restart it on change.
    svc_key: Option<u64>,
}

pub fn register(r: &mut Registry) {
    r.add_state(CalendarState::default());
    r.on_action("calendar", on_action);
    r.on_config(on_config);
    r.on_event(|app, ev| {
        // "Hapus semua data": drop what we hold. The addresses are settings and stay, so the
        // service starts again and the agenda comes back at once instead of 15 minutes later.
        if *ev == AppEvent::DataCleared {
            clear(app);
            app.restart_service("calendar");
        }
    });
    r.add_service(ServiceSpec { name: "calendar", enabled: |c| c.calendar.enabled && !c.calendar.urls.is_empty(), run: service });
}

/// Read-only view for "Tanya AI" (`features/ai`), asked only when the user turned that source on:
/// today's events as `(all_day, start, end, title)` (Unix seconds), in order. Empty while the
/// calendar is off.
pub fn ai_events(app: &App) -> Vec<(bool, i64, i64, String)> {
    if !app.config().calendar.enabled {
        return vec![];
    }
    let now = unix_now();
    let (start, end) = day_bounds(now);
    today_rows(&app.state::<CalendarState>().agenda, start, end, now).into_iter().map(|(o, _)| (o.all_day, o.start, o.end, o.title.clone())).collect()
}
fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// FNV-1a of the addresses (never stored anywhere else, never logged).
fn urls_key(urls: &[String]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in urls.iter().flat_map(|u| u.bytes().chain([0])) {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn clear(app: &mut App) {
    let st = app.state_mut::<CalendarState>();
    st.agenda.clear();
    st.loaded = false;
    st.failed = false;
    st.dismissed.clear();
    st.alerting = None;
    if let Some(t) = st.timer.take() {
        app.cancel_timer(t);
    }
    app.clear_activity("calendar.soon");
    app.request_redraw(Redraw::All);
}

fn on_config(app: &mut App) {
    let cfg = app.config().calendar.clone();
    let key = urls_key(&cfg.urls);
    let on = cfg.enabled && !cfg.urls.is_empty();
    let st = app.state_mut::<CalendarState>();
    let changed = st.svc_key.is_some_and(|k| k != key);
    let first = st.svc_key.is_none();
    st.svc_key = Some(key);
    if !on {
        // Off (or nothing to read): forget what we had.
        if !st.agenda.is_empty() || st.alerting.is_some() || st.loaded || st.failed {
            clear(app);
        }
        return;
    }
    if changed {
        clear(app);
        app.restart_service("calendar");
    } else if !first {
        // `minutes_before` may have changed.
        reschedule(app);
        app.request_redraw(Redraw::Tab("today"));
    }
}

// ---- sources --------------------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum Source {
    Https(String),
    File(PathBuf),
    Unsupported,
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Is this a network (UNC) path, or any other `\\...` / `//...` form (device paths, `\\?\UNC\`)? Opening one
/// makes Windows contact that host and send the user's credentials (NTLM) to it, so a calendar
/// source never reads one: only local drives.
fn is_network_path(p: &str) -> bool {
    p.starts_with("\\\\") || p.starts_with("//")
}

/// `C:\...` or `C:/...`: a path on a drive letter.
fn has_drive(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

/// What an address in the settings means: an `https:` (or `webcal:`) address, a local file given
/// as `file:` URL or a Windows path (handy for an exported `.ics`; no network involved), or
/// nothing usable. Plain `http:` is refused: the address is a secret.
fn classify(url: &str) -> Source {
    let u = url.trim();
    let lower = u.to_ascii_lowercase();
    // webcal(s):// is how calendar apps hand out an https address.
    for (prefix, len) in [("webcals://", 10), ("webcal://", 9)] {
        if lower.starts_with(prefix) {
            return if u.len() > len { Source::Https(format!("https://{}", &u[len..])) } else { Source::Unsupported };
        }
    }
    if lower.starts_with("https://") && u.len() > 8 {
        return Source::Https(u.to_string());
    }
    if lower.starts_with("file:") {
        // file:///C:/a/b.ics, file:C:/a/b.ics, file://localhost/C:/a/b.ics. A file://server/share/...
        // address is a network path (UNC): never.
        let mut p = pct_decode(&u[5..]);
        if p.starts_with("///") {
            p.drain(..3);
        } else if p.len() >= 12 && p[..12].eq_ignore_ascii_case("//localhost/") {
            p.drain(..12);
        }
        let p = p.replace('/', "\\");
        // A path on a drive letter (or a plain relative one), never one that starts with a
        // separator: that is UNC (`file://server/share`, `file:////server/share`), a device path
        // or the root of whatever the current drive is.
        let local = has_drive(&p) || !(p.is_empty() || p.starts_with('\\'));
        return if local { Source::File(PathBuf::from(p)) } else { Source::Unsupported };
    }
    if has_drive(u) {
        return Source::File(PathBuf::from(u));
    }
    Source::Unsupported
}

/// The text of one address. Errors never contain the address.
fn load(url: &str) -> Result<String, String> {
    match classify(url) {
        Source::Https(u) => crate::net::get_text(&u, HTTP_TIMEOUT),
        Source::File(p) => {
            if is_network_path(&p.to_string_lossy()) {
                return Err("network paths are not accepted (use a local file or an https: address)".into());
            }
            let len = std::fs::metadata(&p).map_err(|e| e.to_string())?.len();
            if len > MAX_FILE {
                return Err("file too large".into());
            }
            let bytes = std::fs::read(&p).map_err(|e| e.to_string())?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        }
        Source::Unsupported => Err("unsupported address (use https:, webcal: or a local file)".into()),
    }
}

/// Read local time of a Unix time.
fn local_of(secs: i64) -> Option<DateTime<Local>> {
    Local.timestamp_opt(secs, 0).single()
}

/// Local midnight (Unix seconds) at the start of the day containing `now`, and of the next day.
fn day_bounds(now: i64) -> (i64, i64) {
    let day = local_of(now).map(|t| t.date_naive()).unwrap_or_else(|| Local::now().date_naive());
    let next = day.succ_opt().unwrap_or(day);
    (ics::system_local(day.and_time(NaiveTime::MIN)), ics::system_local(next.and_time(NaiveTime::MIN)))
}

/// The result of one round over all addresses.
#[derive(Debug, PartialEq)]
struct Fetched {
    agenda: Vec<Occurrence>,
    ok: usize,
    failed: usize,
}

/// Load, parse and expand every address. `local` reads wall-clock times as local time.
fn fetch_all(urls: &[String], now: i64, local: &dyn Fn(chrono::NaiveDateTime) -> i64, loader: &dyn Fn(&str) -> Result<String, String>) -> Fetched {
    let (from, _) = day_bounds(now);
    let to = now + HORIZON_DAYS * 86_400;
    let mut f = Fetched { agenda: vec![], ok: 0, failed: 0 };
    for (i, url) in urls.iter().enumerate() {
        match loader(url) {
            Ok(text) => {
                f.ok += 1;
                f.agenda.extend(ics::occurrences(&ics::parse(&text), from, to, local));
            }
            Err(e) => {
                f.failed += 1;
                // The index and the reason only: never the address.
                log_warn!("calendar: address {} could not be loaded: {e}", i + 1);
            }
        }
    }
    f.agenda.sort_by(|a, b| (a.start, &a.title, a.key).cmp(&(b.start, &b.title, b.key)));
    f.agenda.dedup_by_key(|o| o.key);
    f.agenda.truncate(ics::MAX_OCCURRENCES);
    f
}

fn service(ctx: ServiceCtx) {
    let mut fails = 0u32;
    loop {
        let urls = ctx.config().calendar.urls.clone();
        let f = fetch_all(&urls, unix_now(), &ics::system_local, &load);
        let wait = if f.ok == 0 && f.failed > 0 {
            fails += 1;
            ctx.post(|app| {
                let st = app.state_mut::<CalendarState>();
                st.failed = true;
                app.request_redraw(Redraw::Tab("today"));
            });
            (RETRY * 2u32.saturating_pow(fails - 1)).min(REFRESH)
        } else {
            fails = 0;
            log_info!("calendar: {} of {} address(es) loaded, {} event instance(s) in the next {HORIZON_DAYS} days", f.ok, urls.len(), f.agenda.len());
            let agenda = f.agenda;
            ctx.post(move |app| set_agenda(app, agenda));
            REFRESH
        };
        if !ctx.sleep(wait) {
            return;
        }
    }
}

fn set_agenda(app: &mut App, agenda: Vec<Occurrence>) {
    if !app.config().calendar.enabled {
        return;
    }
    let st = app.state_mut::<CalendarState>();
    st.dismissed.retain(|k| agenda.iter().any(|o| o.key == *k));
    st.agenda = agenda;
    st.loaded = true;
    st.failed = false;
    reschedule(app);
    app.request_redraw(Redraw::Tab("today"));
}

// ---- the heads-up ---------------------------------------------------------------------------------

/// The meeting to warn about now: the earliest timed one that starts within `lead` seconds (or
/// started less than [`GRACE_SECS`] ago) and was not closed by the user.
fn pick_alert<'a>(agenda: &'a [Occurrence], now: i64, lead: i64, dismissed: &HashSet<u64>) -> Option<&'a Occurrence> {
    agenda.iter().filter(|o| !o.all_day && !dismissed.contains(&o.key) && o.start - lead <= now && now < o.start + GRACE_SECS).min_by_key(|o| o.start)
}

/// Seconds until the next heads-up should appear (`None` = nothing upcoming).
fn next_alert_in(agenda: &[Occurrence], now: i64, lead: i64, dismissed: &HashSet<u64>) -> Option<i64> {
    agenda.iter().filter(|o| !o.all_day && !dismissed.contains(&o.key) && o.start - lead > now).map(|o| o.start - lead - now).min()
}

/// Whole minutes shown in the pill: 5 minutes and 1 second ahead still reads "6 menit".
fn minutes_left(start: i64, now: i64) -> i64 {
    let r = start - now;
    if r <= 0 { 0 } else { (r + 59) / 60 }
}

/// When to look again while a heads-up is showing: right when its minute changes, or when it
/// ends.
fn tick_in(start: i64, now: i64) -> i64 {
    let r = start - now;
    let until_minute = if r > 0 { (r - 1) % 60 + 1 } else { i64::MAX };
    until_minute.min(start + GRACE_SECS - now).max(1)
}

/// Bring the pill and the timer in line with the agenda and the clock. Runs on the UI thread
/// after anything that changes either (a fetch, a dismissal, a config change, the timer itself).
fn reschedule(app: &mut App) {
    if let Some(t) = app.state_mut::<CalendarState>().timer.take() {
        app.cancel_timer(t);
    }
    let (enabled, lead) = (app.config().calendar.enabled, app.config().calendar.minutes_before as i64 * 60);
    if !enabled {
        return;
    }
    let now = unix_now();
    let st = app.state::<CalendarState>();
    let next = match pick_alert(&st.agenda, now, lead, &st.dismissed) {
        Some(o) => {
            let (key, start) = (o.key, o.start);
            app.state_mut::<CalendarState>().alerting = Some(key);
            app.set_activity(Activity::persistent("calendar.soon", ALERT_PRIORITY, meeting_pill).tab("today").bubble(|_| stack([icon(icons::CALENDAR).color(color::BLUE)])));
            app.request_redraw(Redraw::Pill);
            tick_in(start, now)
        }
        None => {
            if app.state_mut::<CalendarState>().alerting.take().is_some() || app.has_activity("calendar.soon") {
                app.clear_activity("calendar.soon");
                app.request_redraw(Redraw::Pill);
            }
            let st = app.state::<CalendarState>();
            match next_alert_in(&st.agenda, now, lead, &st.dismissed) {
                Some(s) => s.min(MAX_WAIT_SECS),
                None => MAX_WAIT_SECS,
            }
        }
    };
    let t = app.after(Duration::from_secs(next.clamp(1, MAX_WAIT_SECS) as u64), reschedule);
    app.state_mut::<CalendarState>().timer = Some(t);
}

/// The title, unless the privacy setting hides content during a meeting.
fn shown_title(app: &App, title: &str) -> Option<String> {
    let hide = app.meeting().active && app.config().privacy.hide_notification_body_in_meetings;
    (!hide && !title.is_empty()).then(|| title.to_string())
}

fn meeting_pill(app: &App) -> Node {
    let st = app.state::<CalendarState>();
    let Some(o) = st.alerting.and_then(|k| st.agenda.iter().find(|o| o.key == k)) else { return empty() };
    let m = minutes_left(o.start, unix_now());
    let when = if m == 0 { tr("Rapat dimulai", "Meeting started").to_string() } else { tr_s(format!("Rapat {m} menit lagi"), format!("Meeting in {m} min")) };
    let mut kids = vec![icon(icons::CALENDAR).color(color::BLUE), text(when).semibold()];
    if let Some(t) = shown_title(app, &o.title) {
        kids.push(caption("·"));
        kids.push(text(t).secondary().max_w(260.0));
    }
    if o.link.is_some() {
        kids.push(button_primary(tr("Gabung", "Join"), "calendar.join"));
    }
    kids.push(icon_button(icons::CLOSE, "calendar.dismiss").name(tr("Tutup", "Dismiss")));
    pill(kids).pad4(14.0, 0.0, 4.0, 0.0)
}

// ---- actions --------------------------------------------------------------------------------------

fn on_action(app: &mut App, ev: &ActionEvent) {
    if ev.kind != ActionKind::Click {
        return;
    }
    let key = ev.arg().and_then(|a| a.parse::<u64>().ok()).or_else(|| app.state::<CalendarState>().alerting);
    match ev.name() {
        "join" => {
            let url = key.and_then(|k| app.state::<CalendarState>().agenda.iter().find(|o| o.key == k)).and_then(|o| o.link.as_ref()).map(|l| l.url.clone());
            if let Some(url) = url {
                // Only links that `links` recognised as Meet, Zoom or Teams are ever stored.
                if !shell_open(&url) {
                    log_warn!("calendar: could not open the meeting link");
                }
                if let Some(k) = key {
                    app.state_mut::<CalendarState>().dismissed.insert(k);
                }
                app.close_panel();
                reschedule(app);
                app.request_redraw(Redraw::Tab("today"));
            }
        }
        "dismiss" => {
            if let Some(k) = key {
                app.state_mut::<CalendarState>().dismissed.insert(k);
            }
            reschedule(app);
        }
        _ => {}
    }
}

/// The link to hand to the shell: only `https:` (any letter case of the scheme: `HTTPS://` works
/// too), written back with the scheme in lower case; nothing with spaces or control characters.
fn open_target(url: &str) -> Option<String> {
    let u = url.trim();
    let head = u.get(..8).filter(|h| h.eq_ignore_ascii_case("https://"))?;
    let _ = head;
    if u.len() == 8 || u.chars().any(|c| c.is_control() || c == ' ') {
        return None;
    }
    Some(format!("https://{}", &u[8..]))
}

fn shell_open(url: &str) -> bool {
    let Some(url) = open_target(url) else { return false };
    let h = unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(url.as_str()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    h.0 as isize > 32
}

// ---- the agenda in the Hari ini tab ---------------------------------------------------------------

/// Today's events, all-day first, then by start; with whether each has ended.
fn today_rows(agenda: &[Occurrence], day_start: i64, day_end: i64, now: i64) -> Vec<(&Occurrence, bool)> {
    // An event with no length (a deadline) counts as over an hour after its time.
    let over = |o: &Occurrence| !o.all_day && (if o.end > o.start { o.end } else { o.start + 3600 }) <= now;
    let mut rows: Vec<(&Occurrence, bool)> = agenda.iter().filter(|o| o.start < day_end && o.end.max(o.start + 1) > day_start).map(|o| (o, over(o))).collect();
    rows.sort_by_key(|(o, _)| (!o.all_day, o.start));
    rows
}

fn time_range(o: &Occurrence) -> String {
    if o.all_day {
        return tr("Sepanjang hari", "All day").to_string();
    }
    let Some(s) = local_of(o.start) else { return String::new() };
    let range = match local_of(o.end) {
        Some(e) if o.end > o.start => format!("{} – {}", i18n::clock(&s), i18n::clock(&e)),
        _ => i18n::clock(&s),
    };
    match &o.link {
        Some(l) => format!("{range} · {}", l.service.label()),
        None => range,
    }
}

const BARS: [Rgba; 5] = [color::BLUE, color::YELLOW, color::PINK, color::GREEN, color::PURPLE];

fn agenda_row(i: usize, o: &Occurrence, ended: bool) -> Node {
    let title = if o.title.is_empty() { tr("(Tanpa judul)", "(No title)").to_string() } else { o.title.clone() };
    let mut kids = vec![
        gap_px(3.0).h(30.0).bg(BARS[i % BARS.len()]).radius(1.5),
        col([text(title).semibold(), caption(time_range(o))]).gap(1.0).grow(1.0),
    ];
    if o.link.is_some() && !ended {
        kids.push(capsule(icons::VIDEO, tr("Gabung", "Join"), format!("calendar.join:{}", o.key)));
    }
    row(kids).gap(8.0).fill_w().opacity(if ended { 0.5 } else { 1.0 })
}

/// The agenda block of the Hari ini card, or `None` while the calendar is off. Cheap and pure:
/// it runs on every redraw of the tab.
pub fn today_section(app: &App, max_rows: usize) -> Option<Node> {
    let cfg = &app.config().calendar;
    if !cfg.enabled {
        return None;
    }
    let st = app.state::<CalendarState>();
    let now = unix_now();
    let (start, end) = day_bounds(now);
    let rows = today_rows(&st.agenda, start, end, now);
    let mut kids: Vec<Node> = vec![label(tr("Agenda", "Agenda"))];
    if rows.is_empty() {
        let msg = if cfg.urls.is_empty() {
            tr("Tambahkan alamat kalender di Pengaturan", "Add a calendar address in Settings")
        } else if st.failed && !st.loaded {
            tr("Kalender tidak bisa dimuat", "Calendar could not be loaded")
        } else if !st.loaded {
            tr("Memuat kalender…", "Loading calendar…")
        } else {
            tr("Tidak ada acara hari ini", "Nothing scheduled today")
        };
        kids.push(caption(msg));
    }
    let max_rows = max_rows.min(VISIBLE_ROWS);
    let hidden = rows.len().saturating_sub(max_rows);
    // Rows that are over come last, so the room goes to what is still ahead.
    let mut rows = rows;
    rows.sort_by_key(|(_, ended)| *ended);
    for (i, (o, ended)) in rows.into_iter().take(max_rows).enumerate() {
        kids.push(agenda_row(i, o, ended));
    }
    if hidden > 0 {
        kids.push(caption(tr_s(format!("+{hidden} lainnya"), format!("+{hidden} more"))));
    }
    Some(col(kids).gap(6.0).fill_w().align(Align::Stretch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::calendar::links::{MeetingLink, Service};

    fn occ(key: u64, start: i64, mins: i64) -> Occurrence {
        Occurrence { key, title: format!("e{key}"), start, end: start + mins * 60, all_day: false, link: None }
    }

    fn none() -> HashSet<u64> {
        HashSet::new()
    }

    // ---- addresses ----

    #[test]
    fn join_links_open_with_https_in_any_letter_case_and_nothing_else() {
        assert_eq!(open_target("https://meet.google.com/abc-defg-hij").as_deref(), Some("https://meet.google.com/abc-defg-hij"));
        assert_eq!(open_target("HTTPS://Meet.Google.com/abc").as_deref(), Some("https://Meet.Google.com/abc"), "the scheme is normalised, the rest kept");
        assert_eq!(open_target("  Https://teams.microsoft.com/l/meetup-join/x?y=1 ").as_deref(), Some("https://teams.microsoft.com/l/meetup-join/x?y=1"));
        for bad in ["http://meet.google.com/x", "file:///C:/x.exe", "javascript:alert(1)", "ms-msdt:/id", "https://", "https://a b", "https://a\nb", "HTTP://x", "", "meet.google.com/x"] {
            assert_eq!(open_target(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn addresses_are_classified() {
        assert_eq!(classify("https://calendar.example/ical/x/basic.ics"), Source::Https("https://calendar.example/ical/x/basic.ics".into()));
        assert_eq!(classify("  HTTPS://Outlook.example/owa/calendar/x.ics  "), Source::Https("HTTPS://Outlook.example/owa/calendar/x.ics".into()));
        assert_eq!(classify("webcal://p01-caldav.example/x.ics"), Source::Https("https://p01-caldav.example/x.ics".into()));
        assert_eq!(classify("WEBCAL://host/x.ics"), Source::Https("https://host/x.ics".into()));
        assert_eq!(classify("file:///C:/Users/Me/My%20Cal/work.ics"), Source::File(PathBuf::from(r"C:\Users\Me\My Cal\work.ics")));
        assert_eq!(classify("file:C:/cal.ics"), Source::File(PathBuf::from(r"C:\cal.ics")));
        assert_eq!(classify("file://localhost/C:/cal.ics"), Source::File(PathBuf::from(r"C:\cal.ics")));
        assert_eq!(classify(r"D:\exports\cal.ics"), Source::File(PathBuf::from(r"D:\exports\cal.ics")));
        // Network (UNC) paths would make Windows contact that host: never a calendar source.
        for unc in ["file://server/share/cal.ics", r"\\nas\cal\work.ics", "//nas/cal/work.ics", "file:////nas/cal/work.ics", r"\\?\UNC\nas\cal\work.ics", r"\\.\pipe\x", "file://nas/cal/work.ics"] {
            assert_eq!(classify(unc), Source::Unsupported, "{unc}");
        }
        assert!(load(r"\\127.0.0.1\share\x.ics").is_err());
        for bad in ["", "http://plain.example/x.ics", "ftp://x/y.ics", "javascript:alert(1)", "calendar.ics", "https://", "file:", "C:"] {
            assert_eq!(classify(bad), Source::Unsupported, "{bad}");
        }
    }

    #[test]
    fn odd_text_is_never_a_panic() {
        for s in ["日本語", "ééééé", "file:日本語.ics", "webcal://日本語/カレンダー.ics", "https://é", "\u{1F600}\u{1F600}\u{1F600}", "C:\\é\\x.ics", "%é%é"] {
            let _ = classify(s);
            let _ = pct_decode(s);
        }
        assert_eq!(classify("file:日本語.ics"), Source::File(PathBuf::from("日本語.ics")));
    }

    #[test]
    fn percent_decoding_is_forgiving() {
        assert_eq!(pct_decode("a%20b%2Fc"), "a b/c");
        assert_eq!(pct_decode("100%"), "100%");
        assert_eq!(pct_decode("%zz%4"), "%zz%4");
        assert_eq!(pct_decode("%C3%A9"), "é");
    }

    #[test]
    fn errors_never_contain_the_address() {
        let secret = "https://calendar.example/ical/PRIVATE-TOKEN-123/basic.ics";
        let e = load("http://calendar.example/ical/PRIVATE-TOKEN-123/basic.ics").unwrap_err();
        assert!(!e.contains("PRIVATE-TOKEN"), "{e}");
        let e = load(r"C:\no\such\PRIVATE-TOKEN-123\file.ics").unwrap_err();
        assert!(!e.contains("PRIVATE-TOKEN"), "{e}");
        let _ = secret;
    }

    #[test]
    fn reading_a_local_ics_file_gives_the_agenda() {
        let dir = std::env::temp_dir().join(format!("moku-cal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("work.ics");
        std::fs::write(&file, "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:1\r\nSUMMARY:Sprint review\r\nDTSTART:20261125T093000Z\r\nDTEND:20261125T104500Z\r\nLOCATION:https://meet.google.com/abc-defg-hij\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n").unwrap();
        let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 24).unwrap().and_hms_opt(12, 0, 0).unwrap().and_utc().timestamp();
        let local = |n: chrono::NaiveDateTime| n.and_utc().timestamp() - 7 * 3600;
        let urls = vec![format!("file:///{}", file.display().to_string().replace('\\', "/")), "https://".into(), r"C:\definitely\missing.ics".into()];
        let f = fetch_all(&urls, now, &local, &load);
        assert_eq!((f.ok, f.failed), (1, 2));
        assert_eq!(f.agenda.len(), 1);
        assert_eq!(f.agenda[0].title, "Sprint review");
        assert_eq!(f.agenda[0].link.as_ref().map(|l| l.service), Some(Service::Meet));
        // The same feed twice (two addresses, one calendar) shows each event once.
        let twice = vec![urls[0].clone(), file.display().to_string()];
        assert_eq!(fetch_all(&twice, now, &local, &load).agenda.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hostile_or_huge_file_is_just_a_failed_address() {
        let dir = std::env::temp_dir().join(format!("moku-cal-big-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let big = dir.join("big.ics");
        std::fs::write(&big, vec![b'x'; MAX_FILE as usize + 1]).unwrap();
        assert!(load(&big.display().to_string()).is_err());
        let junk = dir.join("junk.ics");
        std::fs::write(&junk, [0xffu8, 0xfe, 0x00, 0x42, 0x13]).unwrap();
        let f = fetch_all(&[junk.display().to_string()], 0, &|n| n.and_utc().timestamp(), &load);
        assert_eq!((f.ok, f.failed, f.agenda.len()), (1, 0, 0), "readable but empty");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_service_key_changes_with_the_addresses_only() {
        let a = vec!["https://a/x.ics".to_string()];
        let b = vec!["https://a/x.ics".to_string(), "https://b/y.ics".to_string()];
        assert_eq!(urls_key(&a), urls_key(&a.clone()));
        assert_ne!(urls_key(&a), urls_key(&b));
        assert_ne!(urls_key(&["ab".to_string(), "c".to_string()]), urls_key(&["a".to_string(), "bc".to_string()]));
    }

    // ---- the heads-up ----

    #[test]
    fn the_alert_appears_minutes_before_the_start_and_stays_a_little_after() {
        let now = 10_000;
        let lead = 5 * 60;
        let agenda = vec![occ(1, now + 6 * 60, 30), occ(2, now + 3 * 60, 30)];
        // 6 min ahead is too early; the one 3 min ahead qualifies.
        assert_eq!(pick_alert(&agenda, now, lead, &none()).map(|o| o.key), Some(2));
        assert_eq!(pick_alert(&agenda[..1], now, lead, &none()).map(|o| o.key), None);
        // From exactly `lead` before.
        assert_eq!(pick_alert(&agenda[..1], now + 60, lead, &none()).map(|o| o.key), Some(1));
        // It is still shown just after the start, and gone after the grace.
        assert!(pick_alert(&agenda[..1], now + 6 * 60 + GRACE_SECS - 1, lead, &none()).is_some());
        assert!(pick_alert(&agenda[..1], now + 6 * 60 + GRACE_SECS, lead, &none()).is_none());
        // Closed by the user: skipped, the next one takes over.
        let mut d = none();
        d.insert(2);
        assert_eq!(pick_alert(&agenda, now + 60, lead, &d).map(|o| o.key), Some(1));
        // Two at once: the earliest first.
        assert_eq!(pick_alert(&agenda, now + 60, lead, &none()).map(|o| o.key), Some(2));
    }

    #[test]
    fn all_day_events_never_alert() {
        let mut o = occ(1, 100, 1440);
        o.all_day = true;
        assert!(pick_alert(&[o.clone()], 100, 300, &none()).is_none());
        assert_eq!(next_alert_in(&[o], 0, 300, &none()), None);
    }

    #[test]
    fn the_timer_wakes_for_the_next_alert() {
        let lead = 300;
        let agenda = vec![occ(1, 1000, 30), occ(2, 5000, 30), occ(3, 200, 30)];
        // At t=0: the earliest start-lead after now is event 3 at 200-300 < 0 → already due; the next not-due one:
        assert_eq!(next_alert_in(&agenda, 0, lead, &none()), Some(700), "event 1 at 1000 - 300");
        assert_eq!(next_alert_in(&agenda, 800, lead, &none()), Some(3900), "event 2");
        assert_eq!(next_alert_in(&agenda, 4800, lead, &none()), None, "nothing upcoming (event 2 is due)");
        let mut d = none();
        d.insert(1);
        assert_eq!(next_alert_in(&agenda, 0, lead, &d), Some(4700), "closed ones are skipped");
    }

    #[test]
    fn minutes_round_up_and_the_pill_ticks_on_the_minute() {
        assert_eq!(minutes_left(1000, 700), 5);
        assert_eq!(minutes_left(1000, 701), 5);
        assert_eq!(minutes_left(1000, 699), 6);
        assert_eq!(minutes_left(1000, 940), 1);
        assert_eq!(minutes_left(1000, 999), 1);
        assert_eq!(minutes_left(1000, 1000), 0);
        assert_eq!(minutes_left(1000, 2000), 0);
        // 4:30 left: the text changes from "5 menit" to "4 menit" in 30 s … at 4:00 exactly it
        // stays "4" until 3:00, so the next look is in 60 s.
        assert_eq!(tick_in(1000, 1000 - 270), 30);
        assert_eq!(tick_in(1000, 1000 - 240), 60);
        assert_eq!(tick_in(1000, 1000 - 241), 1);
        // After the start: until the end of the grace period.
        assert_eq!(tick_in(1000, 1000), GRACE_SECS);
        assert_eq!(tick_in(1000, 1000 + 100), GRACE_SECS - 100);
        assert_eq!(tick_in(1000, 1000 + GRACE_SECS + 5), 1, "never zero");
    }

    // ---- the agenda ----

    #[test]
    fn todays_rows_are_ordered_and_clipped_to_the_day() {
        let (ds, de) = (1_000_000, 1_086_400);
        let mut allday = occ(1, ds, 1440);
        allday.all_day = true;
        let agenda = vec![
            occ(10, ds - 7200, 60),           // yesterday evening
            occ(2, ds + 3600 * 5, 45),        // today 05:00
            occ(3, ds + 3600 * 2, 30),        // today 02:00
            occ(4, ds - 1800, 120),           // started yesterday, still running at midnight
            occ(5, de, 30),                   // tomorrow 00:00
            allday,
        ];
        let now = ds + 3600 * 3;
        let rows = today_rows(&agenda, ds, de, now);
        let keys: Vec<u64> = rows.iter().map(|(o, _)| o.key).collect();
        assert_eq!(keys, [1, 4, 3, 2], "all-day first, then by start; yesterday's and tomorrow's are out");
        let ended: Vec<bool> = rows.iter().map(|(_, e)| *e).collect();
        assert_eq!(ended, [false, true, true, false], "02:00-02:30 is over at 03:00, 05:00 is ahead");
    }

    #[test]
    fn a_link_makes_a_join_button_in_the_row() {
        let mut o = occ(7, 0, 30);
        o.link = Some(MeetingLink { url: "https://meet.google.com/aaa-bbbb-ccc".into(), service: Service::Meet });
        let tree = |n: Node| format!("{n:?}");
        assert!(tree(agenda_row(0, &o, false)).contains("calendar.join:7"));
        assert!(!tree(agenda_row(0, &occ(8, 0, 30), false)).contains("calendar.join"), "no link, no button");
        assert!(!tree(agenda_row(0, &o, true)).contains("calendar.join"), "no button for an event that is over");
        assert!(time_range(&o).ends_with("· Meet"), "{}", time_range(&o));
    }

    #[test]
    fn time_ranges_use_the_clock_format() {
        let mut o = occ(1, 0, 0);
        o.all_day = true;
        assert!(!time_range(&o).is_empty());
        let s = chrono::NaiveDate::from_ymd_opt(2026, 11, 25).unwrap().and_hms_opt(10, 30, 0).unwrap();
        let start = ics::system_local(s);
        let r = time_range(&occ(1, start, 45));
        assert!(r.contains("10") && r.contains("11") && r.contains('–'), "{r}");
        assert_eq!(time_range(&occ(1, start, 0)).len(), 5, "a single time for an event with no length");
    }
}
