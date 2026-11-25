//! Feature `phone_send` (v0.3, track R): send a Shelf file to the phone, or receive files and text
//! from it, over the local Wi-Fi, by scanning a QR code. No phone app is needed.
//!
//! * "Kirim ke HP" on a Shelf file, or "Terima dari HP" in the Shelf tab, starts ONE temporary HTTP
//!   server ([`server`]) on the real LAN adapter ([`adapter`]) and shows its address as a QR code.
//! * The phone opens the page ([`page`]); the first device with the one-time token owns the session
//!   ([`session`]); nothing moves until the user presses "Terima" on the island.
//! * Received files land in `Downloads\Moku` and on the Shelf; a received text is shown with
//!   "Salin" (copied only when the user presses it).
//!
//! Nothing runs while no session is open: no thread, no socket, no memory beyond this code. The
//! server only exists between "start" and the end of the session (10 minutes at most, shorter after
//! a finished transfer). It is plain HTTP on the local network; the UI says so, and the README and
//! `docs/phone-send.md` explain the Windows Firewall prompt (it is the user's to answer).
//!
//! Testing: with `MOKU_DEBUG=1`, `MOKU_PHONE_BIND=127.0.0.1` makes the server listen on loopback
//! instead of the LAN adapter (only loopback is accepted), `MOKU_PHONE_DIR` replaces the receive
//! folder, and the island writes the session address to `<data dir>\phone-debug.txt` for the test
//! client. `phone.sample:<...>` draws the UI with sample data.

pub mod adapter;
pub mod http;
pub mod multipart;
pub mod names;
pub mod page;
pub mod server;
pub mod session;

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

use self::adapter::Adapter;
use self::server::{Event, Limits, Server};
use self::session::{Mode, OfferSummary, SendFile};
use crate::activity::{Activity, priority};
use crate::app::{App, Redraw, TimerId, Ui};
use crate::i18n::{self, tr, tr_s};
use crate::registry::*;
use crate::ui::*;

/// Width of the approval card in the pill (the AI card's width).
const CARD_W: f32 = 380.0;
/// How long the panel is held open for the QR code while nobody has connected yet.
const HOLD_SECS: u64 = 90;
/// After this long without a phone, the card suggests checking the Firewall.
const FIREWALL_HINT_SECS: u64 = 20;
/// Text pill lifetime.
const TEXT_PILL_SECS: f32 = 60.0;
/// The sample address shown in screenshots (never a real one).
const SAMPLE_URL: &str = "http://192.168.1.23:50123/Qm9rdS1SLXBob25lLXNlbmQ/";
const SAMPLE_IP: &str = "192.168.1.23";

// ---- where files go, and which address to listen on ---------------------------------------------------------------

fn known_downloads() -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.filter(|s| !s.is_empty()).map(PathBuf::from)
    }
}

/// The receive folder: the test override (debug only), else `Downloads\Moku`.
fn pick_receive_dir(over: Option<std::ffi::OsString>, debug: bool, downloads: Option<PathBuf>, fallback: PathBuf) -> PathBuf {
    if let Some(o) = over.filter(|o| debug && !o.is_empty()) {
        return PathBuf::from(o);
    }
    downloads.map(|d| d.join("Moku")).unwrap_or(fallback)
}

/// Where received files go. Never the user's real Downloads in a test: `MOKU_PHONE_DIR` (debug only).
pub fn receive_dir() -> PathBuf {
    pick_receive_dir(std::env::var_os("MOKU_PHONE_DIR"), crate::app::debug_enabled(), known_downloads(), moku_core::paths::data_dir().join("Received"))
}

#[derive(Debug, PartialEq, Eq)]
pub enum BindError {
    NoNetwork,
    /// The only usable network is one Windows calls Public: refused unless the user confirms.
    PublicNetwork,
}

/// The address to listen on and the adapter it belongs to. In debug mode `MOKU_PHONE_BIND` may name a
/// LOOPBACK address (and nothing else): it can never make the server listen on the LAN or on every
/// interface. A network Windows calls **Public** is refused ([`BindError::PublicNetwork`]) unless
/// `allow_public` (the user pressed "Tetap lanjutkan" for this session); a Private cable is used
/// instead of a Public Wi-Fi without asking.
pub fn choose_bind(list: &[Adapter], prefer: Option<&str>, debug: bool, env_bind: Option<&str>, allow_public: bool) -> Result<(IpAddr, Option<usize>), BindError> {
    if debug && let Some(ip) = env_bind.and_then(|v| v.trim().parse::<IpAddr>().ok()).filter(|ip| ip.is_loopback()) {
        return Ok((ip, None));
    }
    match adapter::pick(list, prefer, allow_public) {
        Some(i) => Ok((IpAddr::V4(list[i].ip), Some(i))),
        None if adapter::only_public(list) => Err(BindError::PublicNetwork),
        None => Err(BindError::NoNetwork),
    }
}

// ---- state --------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dir {
    /// The PC sends ("Kirim ke HP").
    ToPhone,
    /// The phone sends ("Terima dari HP").
    FromPhone,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RowState {
    Waiting,
    Active,
    Done,
    Failed,
}

#[derive(Clone, Debug, PartialEq)]
struct Row {
    name: String,
    size: u64,
    done: u64,
    state: RowState,
}

struct Live {
    /// Stale events (an earlier session's) are recognised by this.
    id: u64,
    dir: Dir,
    server: Option<Server>,
    url: String,
    qr: Option<Image>,
    /// "Wi-Fi · 192.168.1.23"
    net_label: String,
    /// More than one adapter to choose from.
    can_switch: bool,
    adapter_name: Option<String>,
    /// The user confirmed a Public network for this session ("Tetap lanjutkan").
    allow_public: bool,
    started: Instant,
    remaining: u64,
    device: Option<IpAddr>,
    pending: Option<OfferSummary>,
    rows: Vec<Row>,
    text_wanted: bool,
    batch_done: bool,
    received: u32,
    holding: bool,
    /// Test sessions have no server.
    sample: bool,
    tick: Option<TimerId>,
}

#[derive(Default)]
struct Phone {
    live: Option<Live>,
    next_id: u64,
    /// The last text from the phone (in memory only; shown in a pill until used or dismissed).
    text: Option<String>,
    /// The last adapter the user chose (by name), kept for the next session.
    prefer: Option<String>,
    /// The network list is being read for a session that was asked for (no second start meanwhile).
    starting: bool,
    /// A session waits for "Tetap lanjutkan" (the network is Public).
    public_wait: Option<Mode>,
}

pub fn register(r: &mut Registry) {
    r.add_state(Phone::default());
    r.on_action("phone", on_action);
    r.on_config(|app| {
        if !app.config().phone.enabled && app.state::<Phone>().live.is_some() {
            end(app, Ending::Stopped);
        }
    });
    r.on_event(|app, ev| match ev {
        // Nobody can see the cards while paused: stop listening.
        AppEvent::Paused(true) | AppEvent::DataCleared if app.state::<Phone>().live.is_some() => end(app, Ending::Stopped),
        AppEvent::DataCleared => app.state_mut::<Phone>().text = None,
        _ => {}
    });
    r.on_exit(|app| {
        if app.state::<Phone>().live.is_some() {
            end(app, Ending::Silent);
        }
    });
}

/// Is the feature on (the Shelf shows its buttons)?
pub fn enabled(app: &App) -> bool {
    app.config().phone.enabled
}

fn hide_names(app: &App) -> bool {
    app.meeting().active && app.config().privacy.hide_notification_body_in_meetings
}

fn fmt_size(bytes: u64) -> String {
    super::shelf::fmt_size(bytes, i18n::is_english())
}

// ---- starting -----------------------------------------------------------------------------------------------------

/// "Kirim ke HP" on Shelf files (folders are skipped: only files can be sent).
pub fn start_send(app: &mut App, paths: Vec<String>) {
    if !enabled(app) {
        return;
    }
    let mut files = vec![];
    for p in paths {
        let path = PathBuf::from(&p);
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if !meta.is_file() {
            continue;
        }
        let name = names::sanitize(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        files.push(SendFile { path, name, size: meta.len() });
    }
    if files.is_empty() {
        return notice(app, tr("Pilih file, bukan folder", "Pick a file, not a folder"));
    }
    start(app, Mode::Send(files));
}

/// "Terima dari HP".
pub fn start_receive(app: &mut App) {
    if enabled(app) {
        start(app, Mode::Receive);
    }
}

fn notice(app: &mut App, msg: &'static str) {
    app.alert("phone.notice", priority::HEADS_UP, 5.0, move |_| pill([icon(icons::PHONE).color(color::ORANGE), text(msg).semibold()]));
}

/// Read the adapters and their Windows network category on a worker thread (the Network List Manager
/// is COM: never the UI thread), then run `then` on the UI thread with the list.
fn with_adapters(app: &mut App, then: impl FnOnce(&mut App, Vec<Adapter>) + Send + 'static) {
    app.state_mut::<Phone>().starting = true;
    let spawned = std::thread::Builder::new().name("phone-net".into()).spawn(move || {
        let list = adapter::list_with_categories();
        Ui::post(move |app| {
            app.state_mut::<Phone>().starting = false;
            then(app, list);
        });
    });
    if spawned.is_err() {
        app.state_mut::<Phone>().starting = false;
    }
}

fn start(app: &mut App, mode: Mode) {
    start_with(app, mode, false);
}

fn start_with(app: &mut App, mode: Mode, allow_public: bool) {
    if app.state::<Phone>().starting {
        return;
    }
    app.state_mut::<Phone>().public_wait = None;
    app.clear_activity("phone.public");
    with_adapters(app, move |app, list| begin(app, mode, list, allow_public));
}

/// The network is Public: say so, and wait for the user ("Tetap lanjutkan" starts it for this session).
fn ask_public(app: &mut App, mode: Mode) {
    log_info!("phone: refused, the network is Public");
    app.state_mut::<Phone>().public_wait = Some(mode);
    app.set_activity(Activity::persistent("phone.public", priority::NOTIFICATION, public_card).tab("shelf").bubble(|_| stack([icon(icons::SHIELD).color(color::ORANGE)])));
    // Not for ever: an unanswered question is a "no" after a minute.
    app.after(Duration::from_secs(60), |app| {
        if app.state_mut::<Phone>().public_wait.take().is_some() {
            app.clear_activity("phone.public");
            app.request_redraw(Redraw::All);
        }
    });
    app.request_redraw(Redraw::All);
}

/// "Tetap lanjutkan": the user accepts the Public network for this session only.
fn continue_public(app: &mut App) {
    let Some(mode) = app.state_mut::<Phone>().public_wait.take() else { return };
    app.clear_activity("phone.public");
    log_info!("phone: the user continued on a Public network (this session only)");
    start_with(app, mode, true);
}

/// "Batal".
fn cancel_public(app: &mut App) {
    app.state_mut::<Phone>().public_wait = None;
    app.clear_activity("phone.public");
}

/// The refusal as a card (the pill): why, and the two answers.
fn public_card(app: &App) -> Node {
    if app.state::<Phone>().public_wait.is_none() {
        return empty();
    }
    let btn = |label: &str, primary: bool, action: &str| {
        let t = text(label.to_string()).semibold().text_center();
        let (t, bg) = if primary { (t.color(color::INK), color::WHITE) } else { (t, color::FILL2) };
        row([t]).justify(Justify::Center).h(32.0).grow(1.0).bg(bg).capsule().action(action.to_string())
    };
    let tile = stack([icon(icons::SHIELD).font_size(15.0).color(color::INK)]).size(30.0, 30.0).bg(color::ORANGE).radius(9.0);
    let texts = col([
        text(tr("Jaringan publik: kirim ke HP dimatikan demi keamanan", "Public network: sending to a phone is off for your safety")).semibold().lines(2),
        text(tr("Windows menandai Wi-Fi ini Publik, jadi perangkat lain di sana bisa mencoba membuka halaman transfer. Pindah ke jaringan Pribadi, atau lanjutkan hanya untuk sesi ini.", "Windows marks this Wi-Fi Public, so other devices there could try the transfer page. Switch to a Private network, or continue for this session only."))
            .font_size(12.0)
            .secondary()
            .lines(4),
    ])
    .gap(2.0)
    .grow(1.0);
    col([row([tile, texts]).gap(10.0).fill_w(), row([btn(tr("Batal", "Cancel"), false, "phone.public_no"), btn(tr("Tetap lanjutkan", "Continue anyway"), true, "phone.public_ok")]).gap(8.0).fill_w()]).gap(10.0).pad(12.0).w(CARD_W)
}

/// Runs on the UI thread with the adapter list (and categories) read by [`with_adapters`].
fn begin(app: &mut App, mode: Mode, list: Vec<Adapter>, allow_public: bool) {
    if app.state::<Phone>().live.is_some() {
        end(app, Ending::Silent);
    }
    let prefer = app.state::<Phone>().prefer.clone();
    let env = std::env::var("MOKU_PHONE_BIND").ok();
    let (ip, adapter_idx) = match choose_bind(&list, prefer.as_deref(), crate::app::debug_enabled(), env.as_deref(), allow_public) {
        Ok(x) => x,
        Err(BindError::NoNetwork) => return notice(app, tr("Tidak ada jaringan Wi-Fi atau LAN yang bisa dipakai", "No usable Wi-Fi or LAN network")),
        Err(BindError::PublicNetwork) => return ask_public(app, mode),
    };
    let net_label = match adapter_idx {
        Some(i) => adapter::label(&list[i]),
        None => format!("{ip} (uji)"),
    };
    let adapter_name = adapter_idx.map(|i| list[i].name.clone());
    let can_switch = adapter::candidates(&list, allow_public).len() > 1;
    let id = {
        let p = app.state_mut::<Phone>();
        p.next_id += 1;
        p.next_id
    };
    let dir = if matches!(mode, Mode::Send(_)) { Dir::ToPhone } else { Dir::FromPhone };
    let rows: Vec<Row> = match &mode {
        Mode::Send(files) => files.iter().map(|f| Row { name: f.name.clone(), size: f.size, done: 0, state: RowState::Waiting }).collect(),
        Mode::Receive => vec![],
    };
    let notify = move |e: Event| Ui::post(move |app| on_server_event(app, id, e));
    let server = match Server::start(SocketAddr::new(ip, 0), mode, receive_dir(), Limits::default(), notify) {
        Ok(s) => s,
        Err(e) => {
            log_warn!("phone: could not start the server ({:?})", e.kind());
            return notice(app, tr("Tidak bisa memulai server transfer", "Could not start the transfer server"));
        }
    };
    let url = server.url();
    let qr = crate::ui::qr::qr_image(&url, 8);
    if crate::app::debug_enabled() && adapter_idx.is_none() {
        // For the test client only (debug mode, loopback): never in a normal run.
        let _ = std::fs::write(moku_core::paths::data_dir().join("phone-debug.txt"), &url);
    }
    log_info!("phone: session started ({})", if dir == Dir::ToPhone { "send" } else { "receive" });
    let tick = app.every(Duration::from_secs(1), on_tick);
    let live = Live {
        id,
        dir,
        server: Some(server),
        url,
        qr,
        net_label,
        can_switch,
        adapter_name,
        allow_public,
        started: Instant::now(),
        remaining: session::TTL.as_secs(),
        device: None,
        pending: None,
        rows,
        text_wanted: false,
        batch_done: false,
        received: 0,
        holding: false,
        sample: false,
        tick: Some(tick),
    };
    install(app, live);
}

/// Make `live` the session: show it (panel on the Shelf tab, held open for the QR code, and the pill).
fn install(app: &mut App, mut live: Live) {
    live.holding = true;
    app.state_mut::<Phone>().live = Some(live);
    app.hold_panel(true);
    app.set_activity(Activity::persistent("phone.session", priority::PROGRESS, session_pill).tab("shelf").bubble(|app| stack([icon(icons::PHONE).color(app.base_accent())])));
    app.open_panel(Some("shelf"));
    // The hold is not for ever: after a while (or when a phone connects) the panel is free again.
    app.after(Duration::from_secs(HOLD_SECS), release_hold);
    app.request_redraw(Redraw::All);
}

fn release_hold(app: &mut App) {
    if let Some(l) = app.state_mut::<Phone>().live.as_mut()
        && std::mem::take(&mut l.holding)
    {
        app.hold_panel(false);
    }
}

// ---- ending -------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ending {
    /// The user pressed stop (or the feature was turned off).
    Stopped,
    /// Expired, denied, or timed out waiting for the card.
    Session(session::EndReason),
    /// No pill, no redraw (starting another session, quitting).
    Silent,
}

fn end(app: &mut App, why: Ending) {
    let Some(mut live) = app.state_mut::<Phone>().live.take() else { return };
    if let Some(t) = live.tick.take() {
        app.cancel_timer(t);
    }
    if let Some(mut s) = live.server.take() {
        if matches!(why, Ending::Session(session::EndReason::Denied | session::EndReason::ApprovalTimeout)) {
            // The phone's page asks once more: let it hear "declined" before the door closes.
            app.after(Duration::from_secs(6), move |_| drop(s));
        } else {
            s.stop();
        }
    }
    if std::mem::take(&mut live.holding) {
        app.hold_panel(false);
    }
    for id in ["phone.session", "phone.approve"] {
        app.clear_activity(id);
    }
    if why != Ending::Silent {
        log_info!("phone: session ended ({why:?})");
        let received = live.received;
        let dir = live.dir;
        let completed = live.rows.iter().any(|r| r.state == RowState::Done);
        // A short word about how it ended: what arrived (once), or why nothing did.
        if completed {
            if !live.batch_done {
                app.alert("phone.done", priority::OSD, 8.0, move |_| done_pill(dir, received.max(1)));
            }
        } else {
            match why {
                Ending::Session(session::EndReason::Denied) => notice(app, tr("Transfer ditolak", "Transfer declined")),
                Ending::Session(session::EndReason::ApprovalTimeout) => notice(app, tr("Tidak dijawab: transfer dibatalkan", "No answer: transfer cancelled")),
                Ending::Session(session::EndReason::Expired) if live.device.is_none() => notice(app, tr("Tidak ada HP yang tersambung: sesi berakhir", "No phone connected: session ended")),
                _ => {}
            }
        }
    }
    app.request_redraw(Redraw::All);
}

// ---- events from the server ----------------------------------------------------------------------------------------

fn live_mut<'a>(app: &'a mut App, id: u64) -> Option<&'a mut Live> {
    app.state_mut::<Phone>().live.as_mut().filter(|l| l.id == id)
}

fn on_server_event(app: &mut App, id: u64, ev: Event) {
    if live_mut(app, id).is_none() {
        return;
    }
    match ev {
        Event::Connected(ip) => {
            if let Some(l) = live_mut(app, id) {
                l.device = Some(ip);
            }
            release_hold(app);
        }
        Event::Offer(sum) => {
            let from_phone = live_mut(app, id).is_some_and(|l| l.dir == Dir::FromPhone);
            if let Some(l) = live_mut(app, id) {
                if from_phone {
                    l.rows = sum.files.iter().map(|(n, s)| Row { name: n.clone(), size: *s, done: 0, state: RowState::Waiting }).collect();
                    l.text_wanted = sum.text;
                    l.batch_done = false;
                }
                l.device = Some(sum.ip);
                l.pending = sum.needs_approval.then(|| sum.clone());
            }
            if sum.needs_approval {
                app.set_activity(Activity::persistent("phone.approve", priority::DUE, approval_pill).tab("shelf").bubble(|_| stack([icon(icons::PHONE).color(color::ORANGE)])));
                // The pill is hidden while the panel is open: show the card in the panel instead.
                if app.is_panel_open() {
                    app.open_panel(Some("shelf"));
                    if let Some(l) = live_mut(app, id)
                        && !std::mem::replace(&mut l.holding, true)
                    {
                        app.hold_panel(true);
                    }
                }
            }
        }
        Event::Progress { upload: _, index, done, total } => {
            if let Some(l) = live_mut(app, id)
                && let Some(r) = l.rows.get_mut(index)
            {
                r.done = done;
                r.size = r.size.max(total);
                if r.state != RowState::Done {
                    r.state = RowState::Active;
                }
            }
        }
        Event::FileSaved { index, path } => {
            if let Some(l) = live_mut(app, id) {
                if let Some(r) = l.rows.get_mut(index) {
                    r.state = RowState::Done;
                    r.done = r.size;
                }
                l.received += 1;
            }
            super::shelf::add_received(app, path.to_string_lossy().into_owned());
        }
        Event::UploadFailed { index } => {
            if let Some(r) = live_mut(app, id).and_then(|l| l.rows.get_mut(index)) {
                r.state = RowState::Failed;
            }
        }
        Event::Downloaded { index } => {
            if let Some(r) = live_mut(app, id).and_then(|l| l.rows.get_mut(index)) {
                r.state = RowState::Done;
                r.done = r.size;
            }
        }
        Event::Text(t) => {
            app.state_mut::<Phone>().text = Some(t);
            app.set_activity(Activity::transient("phone.text", priority::NOTIFICATION, TEXT_PILL_SECS, text_pill));
            if let Some(l) = live_mut(app, id) {
                l.text_wanted = false;
            }
        }
    }
    app.request_redraw(Redraw::All);
}

/// Once a second while a session lives: the clock, the card's countdown, the end.
fn on_tick(app: &mut App) {
    let now = Instant::now();
    let (reason, finished_now) = {
        let Some(live) = app.state_mut::<Phone>().live.as_mut() else { return };
        let Some(server) = &live.server else { return };
        let (reason, remaining, batch) = server.shared().with(|s| (s.tick(now).or(s.ended()), s.remaining(now).as_secs(), s.batch_finished()));
        live.remaining = remaining;
        let all_done = match live.dir {
            Dir::FromPhone => batch,
            Dir::ToPhone => !live.rows.is_empty() && live.rows.iter().all(|r| r.state == RowState::Done),
        };
        let first = all_done && !live.batch_done;
        live.batch_done |= all_done;
        (reason, first.then(|| (live.dir, live.received.max(1))))
    };
    // A transfer that just finished says so once.
    if let Some((dir, n)) = finished_now {
        app.alert("phone.done", priority::OSD, 8.0, move |_| done_pill(dir, n));
    }
    if let Some(reason) = reason {
        return end(app, Ending::Session(reason));
    }
    app.request_redraw(Redraw::Pill);
    app.request_redraw(Redraw::Tab("shelf"));
}

// ---- actions ---------------------------------------------------------------------------------------------------------------

fn on_action(app: &mut App, ev: &ActionEvent) {
    if ev.kind != ActionKind::Click {
        return;
    }
    match ev.name() {
        "receive" => start_receive(app),
        "stop" => end(app, Ending::Stopped),
        "accept" => answer(app, true),
        "deny" => answer(app, false),
        "adapter" => switch_adapter(app),
        "public_ok" => continue_public(app),
        "public_no" => cancel_public(app),
        "open_folder" => open_folder(&receive_dir()),
        "copy_text" => {
            if let Some(t) = app.state::<Phone>().text.clone() {
                if !app.set_clipboard_text(&t) {
                    log_warn!("phone: could not copy the text");
                }
                app.clear_activity("phone.text");
            }
        }
        "text_to_shelf" => text_to_shelf(app),
        "dismiss_text" => {
            app.state_mut::<Phone>().text = None;
            app.clear_activity("phone.text");
        }
        "sample" => {
            if let Some(a) = ev.arg() {
                sample(app, a);
            }
        }
        _ => return,
    }
    app.request_redraw(Redraw::All);
}

fn answer(app: &mut App, yes: bool) {
    let Some(live) = app.state_mut::<Phone>().live.as_mut() else { return };
    live.pending = None;
    if let Some(s) = &live.server {
        s.shared().with(|s| if yes { s.approve() } else { s.deny() });
    }
    app.clear_activity("phone.approve");
    if !yes {
        end(app, Ending::Session(session::EndReason::Denied));
    }
}

/// Move the session to the next usable adapter (only before a phone connected).
fn switch_adapter(app: &mut App) {
    let Some(live) = app.state::<Phone>().live.as_ref() else { return };
    if live.device.is_some() || live.sample {
        return;
    }
    let (current, allow_public, id) = (live.adapter_name.clone(), live.allow_public, live.id);
    let mode = match live.server.as_ref().map(|s| s.shared().with(|s| s.mode().clone())) {
        Some(m) => m,
        None => return,
    };
    if app.state::<Phone>().starting {
        return;
    }
    with_adapters(app, move |app, list| {
        // The session may have ended or changed while the list was read.
        if live_mut(app, id).is_none() {
            return;
        }
        let cands = adapter::candidates(&list, allow_public);
        if cands.len() < 2 {
            return;
        }
        let pos = cands.iter().position(|&i| Some(&list[i].name) == current.as_ref()).unwrap_or(0);
        let next = list[cands[(pos + 1) % cands.len()]].name.clone();
        app.state_mut::<Phone>().prefer = Some(next);
        end(app, Ending::Silent);
        begin(app, mode, list, allow_public);
    });
}

fn open_folder(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    unsafe {
        let _ = ShellExecuteW(None, w!("open"), &HSTRING::from(dir.as_os_str()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// "Shelf" on the text pill: the text becomes a `.txt` file in the receive folder (written off the UI
/// thread) and lands on the Shelf.
fn text_to_shelf(app: &mut App) {
    let Some(t) = app.state::<Phone>().text.clone() else { return };
    app.clear_activity("phone.text");
    let dir = receive_dir();
    let spawned = std::thread::Builder::new().name("phone-text".into()).spawn(move || {
        let _ = std::fs::create_dir_all(&dir);
        let base = tr("Teks dari HP.txt", "Text from phone.txt");
        if let Ok((mut f, path)) = names::create_unique(&dir, |n| names::numbered(base, n)) {
            use std::io::Write;
            if f.write_all(t.as_bytes()).is_ok() {
                drop(f);
                let p = path.to_string_lossy().into_owned();
                Ui::post(move |app| super::shelf::add_received(app, p));
            } else {
                drop(f);
                let _ = std::fs::remove_file(&path);
            }
        }
    });
    if spawned.is_err() {
        log_warn!("phone: could not start the text writer");
    }
}

// ---- sample data for screenshots (debug only: the action name starts with "sample") ------------------------

fn sample(app: &mut App, what: &str) {
    if app.state::<Phone>().live.is_some() {
        end(app, Ending::Silent);
    }
    let mut live = Live {
        id: 0,
        dir: Dir::FromPhone,
        server: None,
        url: SAMPLE_URL.into(),
        qr: crate::ui::qr::qr_image(SAMPLE_URL, 8),
        net_label: format!("Wi-Fi \u{b7} {SAMPLE_IP}"),
        can_switch: true,
        adapter_name: None,
        allow_public: false,
        started: Instant::now(),
        remaining: 9 * 60 + 41,
        device: None,
        pending: None,
        rows: vec![],
        text_wanted: false,
        batch_done: false,
        received: 0,
        holding: false,
        sample: true,
        tick: None,
    };
    let ip: IpAddr = SAMPLE_IP.parse().unwrap_or(IpAddr::from([192, 168, 1, 23]));
    let rows = |states: &[(&str, u64, u64, RowState)]| states.iter().map(|(n, s, d, st)| Row { name: n.to_string(), size: *s, done: *d, state: *st }).collect::<Vec<_>>();
    match what {
        "send" => {
            live.dir = Dir::ToPhone;
            live.rows = rows(&[("Laporan-Q4.pdf", 2_202_009, 0, RowState::Waiting)]);
        }
        "connected" => live.device = Some(ip),
        "card" => {
            live.device = Some(ip);
            live.pending = Some(OfferSummary { ip, files: vec![("IMG_2041.jpg".into(), 3_400_000), ("IMG_2042.jpg".into(), 2_900_000)], text: false, needs_approval: true });
            live.rows = rows(&[("IMG_2041.jpg", 3_400_000, 0, RowState::Waiting), ("IMG_2042.jpg", 2_900_000, 0, RowState::Waiting)]);
        }
        "card-risky" => {
            live.device = Some(ip);
            live.pending = Some(OfferSummary { ip, files: vec![("setup.exe".into(), 8_100_000), ("catatan.txt".into(), 1200)], text: true, needs_approval: true });
        }
        "progress" => {
            live.device = Some(ip);
            live.rows = rows(&[("IMG_2041.jpg", 3_400_000, 3_400_000, RowState::Done), ("IMG_2042.jpg", 2_900_000, 1_300_000, RowState::Active), ("VID_0007.mp4", 48_000_000, 0, RowState::Waiting)]);
            live.received = 1;
        }
        "done" => {
            live.device = Some(ip);
            live.rows = rows(&[("IMG_2041.jpg", 3_400_000, 3_400_000, RowState::Done), ("IMG_2042.jpg", 2_900_000, 2_900_000, RowState::Done)]);
            live.received = 2;
            live.batch_done = true;
            live.remaining = 58;
        }
        "text" => {
            app.state_mut::<Phone>().text = Some("Alamat rumah: Jl. Melati No. 12, Bandung. Kode pintu 4821.".into());
            app.set_activity(Activity::transient("phone.text", priority::NOTIFICATION, TEXT_PILL_SECS, text_pill));
            return app.request_redraw(Redraw::All);
        }
        "done-pill" => {
            app.alert("phone.done", priority::OSD, 30.0, |_| done_pill(Dir::FromPhone, 3));
            return app.request_redraw(Redraw::All);
        }
        // The question a Public network raises ("Tetap lanjutkan").
        "public" => return ask_public(app, Mode::Receive),
        _ => {}
    }
    let pending = live.pending.is_some();
    install(app, live);
    if pending {
        app.set_activity(Activity::persistent("phone.approve", priority::DUE, approval_pill).tab("shelf"));
    }
}

// ---- look ----------------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Waiting,
    Connected,
    Asking,
    Moving,
    Done,
}

fn phase(l: &Live) -> Phase {
    if l.pending.is_some() {
        Phase::Asking
    } else if l.batch_done {
        Phase::Done
    } else if l.rows.iter().any(|r| r.state == RowState::Active) {
        Phase::Moving
    } else if l.device.is_some() {
        Phase::Connected
    } else {
        Phase::Waiting
    }
}

fn clock(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn progress_of(l: &Live) -> f32 {
    let total: u64 = l.rows.iter().map(|r| r.size).sum();
    if total == 0 {
        return 0.0;
    }
    let done: u64 = l.rows.iter().map(|r| if r.state == RowState::Done { r.size } else { r.done.min(r.size) }).sum();
    (done as f32 / total as f32).clamp(0.0, 1.0)
}

fn count_text(n: usize) -> String {
    tr_s(format!("{n} file"), format!("{n} file{}", if n == 1 { "" } else { "s" }))
}

/// "HP 192.168.1.23 ingin mengirim 2 file" / "... ingin mengunduh laporan.pdf".
fn card_title(app: &App, l: &Live, s: &OfferSummary) -> String {
    let ip = s.ip;
    match l.dir {
        Dir::FromPhone => {
            let what = match (s.files.len(), s.text) {
                (0, _) => tr("teks", "a text").to_string(),
                (n, false) => count_text(n),
                (n, true) => tr_s(format!("{} dan teks", count_text(n)), format!("{} and a text", count_text(n))),
            };
            tr_s(format!("HP {ip} ingin mengirim {what}"), format!("Phone {ip} wants to send {what}"))
        }
        Dir::ToPhone => {
            let what = match s.files.as_slice() {
                [(n, _)] if !hide_names(app) => n.clone(),
                f => count_text(f.len()),
            };
            tr_s(format!("HP {ip} ingin mengunduh {what}"), format!("Phone {ip} wants to download {what}"))
        }
    }
}

fn card_detail(app: &App, s: &OfferSummary) -> String {
    let total: u64 = s.files.iter().map(|(_, z)| *z).sum();
    let mut d = if total > 0 { fmt_size(total) } else { String::new() };
    if let Some((first, _)) = s.files.first()
        && !hide_names(app)
    {
        if !d.is_empty() {
            d.push_str(" \u{b7} ");
        }
        d.push_str(first);
        if s.files.len() > 1 {
            d.push_str(&format!(" +{}", s.files.len() - 1));
        }
    }
    d
}

fn approval_card(app: &App, l: &Live, s: &OfferSummary) -> Node {
    let tile = stack([icon(icons::PHONE).font_size(15.0).color(color::INK)]).size(30.0, 30.0).bg(color::ORANGE).radius(9.0);
    let mut texts = col([text(card_title(app, l, s)).semibold().lines(2)]).gap(1.0).grow(1.0);
    let detail = card_detail(app, s);
    if !detail.is_empty() {
        texts = texts.child(text(detail).font_size(12.0).secondary());
    }
    let risky = l.dir == Dir::FromPhone && s.files.iter().any(|(n, _)| crate::safety::is_risky(n));
    let mut kids = vec![row([tile, texts]).gap(10.0).fill_w()];
    if risky {
        kids.push(row([icon(icons::WARNING).font_size(12.0).color(color::ORANGE), text(tr("Ada program atau skrip. Moku tidak pernah menjalankannya.", "Includes a program or script. Moku never runs it.")).font_size(12.0).color(color::ORANGE).lines(2).grow(1.0)]).gap(6.0).fill_w());
    }
    let btn = |label: &str, primary: bool, action: &str| {
        let t = text(label.to_string()).semibold().text_center();
        let (t, bg) = if primary { (t.color(color::INK), color::WHITE) } else { (t, color::FILL2) };
        row([t]).justify(Justify::Center).h(32.0).grow(1.0).bg(bg).capsule().action(action.to_string())
    };
    kids.push(row([btn(tr("Tolak", "Decline"), false, "phone.deny"), btn(tr("Terima", "Accept"), true, "phone.accept")]).gap(8.0).fill_w());
    col(kids).gap(10.0).fill_w()
}

fn approval_pill(app: &App) -> Node {
    let st = app.state::<Phone>();
    match st.live.as_ref().and_then(|l| l.pending.as_ref().map(|s| (l, s))) {
        Some((l, s)) => approval_card(app, l, s).w(CARD_W).pad(12.0),
        None => empty(),
    }
}

fn session_pill(app: &App) -> Node {
    let Some(l) = app.state::<Phone>().live.as_ref() else { return empty() };
    let accent = app.base_accent();
    let stop = circle_button(icons::CLOSE, 24.0, "phone.stop").name(tr("Batalkan transfer HP", "Cancel the phone transfer"));
    match phase(l) {
        Phase::Moving => {
            let done = l.rows.iter().filter(|r| r.state == RowState::Done).count();
            let title = match l.dir {
                Dir::FromPhone => tr_s(format!("Menerima dari HP \u{b7} {}/{}", done, l.rows.len()), format!("Receiving from phone \u{b7} {}/{}", done, l.rows.len())),
                Dir::ToPhone => tr("Mengirim ke HP\u{2026}", "Sending to phone\u{2026}").to_string(),
            };
            pill([ring(progress_of(l)).color(accent), text(title).semibold(), caption(format!("{}%", (progress_of(l) * 100.0).round() as u32)), stop])
        }
        Phase::Done => pill([
            stack([icon(icons::CHECK).font_size(10.0).color(color::INK)]).size(20.0, 20.0).bg(color::GREEN).capsule(),
            text(tr("Transfer selesai", "Transfer complete")).semibold(),
            caption(clock(l.remaining)),
            stop,
        ]),
        Phase::Connected | Phase::Asking => pill([icon(icons::PHONE).color(accent), text(tr("HP tersambung", "Phone connected")).semibold(), caption(clock(l.remaining)), stop]),
        Phase::Waiting => pill([icon(icons::PHONE).color(accent), text(tr("Menunggu HP memindai\u{2026}", "Waiting for the phone to scan\u{2026}")).semibold(), caption(clock(l.remaining)), stop]),
    }
}

fn done_pill(dir: Dir, n: u32) -> Node {
    let title = match dir {
        Dir::FromPhone => tr_s(format!("Diterima dari HP \u{b7} {}", count_text(n as usize)), format!("Received from phone \u{b7} {}", count_text(n as usize))),
        Dir::ToPhone => tr("Terkirim ke HP", "Sent to phone").to_string(),
    };
    let mut kids = vec![stack([icon(icons::CHECK).font_size(10.0).color(color::INK)]).size(20.0, 20.0).bg(color::GREEN).capsule(), text(title).semibold()];
    if dir == Dir::FromPhone {
        kids.push(capsule(icons::FOLDER, tr("Folder", "Folder"), "phone.open_folder").name(tr("Buka folder", "Open the folder")));
    }
    pill(kids)
}

fn text_pill(app: &App) -> Node {
    let Some(t) = app.state::<Phone>().text.as_ref() else { return empty() };
    let preview = if hide_names(app) { tr("Teks dari HP", "Text from phone").to_string() } else { t.split_whitespace().collect::<Vec<_>>().join(" ") };
    pill([
        icon(icons::MESSAGE).color(app.base_accent()),
        col([text(tr("Teks dari HP", "Text from phone")).semibold(), text(preview).font_size(12.0).secondary()]).gap(1.0).grow(1.0),
        capsule(icons::COPY, tr("Salin", "Copy"), "phone.copy_text"),
        circle_button(icons::SHELF, 26.0, "phone.text_to_shelf").name(tr("Simpan ke Shelf", "Save to Shelf")),
        circle_button(icons::CLOSE, 26.0, "phone.dismiss_text").name(tr("Tutup", "Dismiss")),
    ])
    .max_w(CARD_W + 80.0)
}

/// The Shelf tab body while a session lives (`None` otherwise).
pub fn panel(app: &App) -> Option<Node> {
    let l = app.state::<Phone>().live.as_ref()?;
    let accent = app.base_accent();
    let title = match l.dir {
        Dir::FromPhone => tr("Terima dari HP", "Receive from phone"),
        Dir::ToPhone => tr("Kirim ke HP", "Send to phone"),
    };
    let head = row([icon(icons::PHONE).color(accent), text(title).font_size(typo::CARD_TITLE).semibold(), spacer(), caption(tr_s(format!("Berlaku {}", clock(l.remaining)), format!("Valid for {}", clock(l.remaining))))]).gap(8.0).fill_w();
    let note = row([icon(icons::LOCK).font_size(11.0).color(color::TEXT3), caption(tr("Tautan memakai HTTP biasa di jaringanmu: jangan dipakai di Wi-Fi umum.", "The link is plain HTTP on your network: do not use it on public Wi-Fi.")).lines(2).grow(1.0)]).gap(6.0).fill_w();
    let body = match phase(l) {
        Phase::Asking => match &l.pending {
            Some(s) => approval_card(app, l, s),
            None => empty(),
        },
        Phase::Moving | Phase::Done => transfer_list(app, l),
        Phase::Waiting | Phase::Connected => qr_block(app, l),
    };
    Some(col([head, body, note]).gap(10.0).fill_w())
}

fn qr_block(app: &App, l: &Live) -> Node {
    let side = l.qr.as_ref().map(|q| q.0.width as f32 / 2.0).unwrap_or(164.0);
    let code = match &l.qr {
        Some(img) => image(img, 14.0).size(side, side),
        None => stack([icon(icons::LINK)]).size(164.0, 164.0).bg(color::FILL).radius(14.0),
    };
    let mut right = vec![];
    right.push(text(match l.dir {
        Dir::FromPhone => tr("Pindai dengan kamera HP", "Scan with the phone's camera"),
        Dir::ToPhone => tr("Pindai untuk mengunduh", "Scan to download"),
    })
    .semibold());
    right.push(caption(tr("HP dan PC harus di Wi-Fi yang sama. Tidak perlu aplikasi.", "The phone and PC must be on the same Wi-Fi. No app needed.")).lines(2));
    if l.dir == Dir::ToPhone && !l.rows.is_empty() {
        let name = match l.rows.as_slice() {
            [r] if !hide_names(app) => format!("{} \u{b7} {}", r.name, fmt_size(r.size)),
            rows => count_text(rows.len()),
        };
        right.push(row([icon(icons::DOCUMENT).font_size(12.0).color(color::TEXT2), text(name).font_size(12.0).secondary().grow(1.0)]).gap(6.0).fill_w());
    }
    right.push(spacer());
    let phase = phase(l);
    if phase == Phase::Connected {
        let ip = l.device.map(|i| i.to_string()).unwrap_or_default();
        right.push(row([icon(icons::CHECK).font_size(12.0).color(color::GREEN), text(tr_s(format!("HP {ip} tersambung"), format!("Phone {ip} connected"))).font_size(12.0).color(color::GREEN)]).gap(6.0));
    } else if l.started.elapsed().as_secs() >= FIREWALL_HINT_SECS && !l.sample {
        right.push(row([icon(icons::INFO).font_size(12.0).color(color::ORANGE), text(tr("Belum tersambung? Izinkan Moku di Windows Firewall (jaringan Pribadi).", "Not connecting? Allow Moku in Windows Firewall (private networks).")).font_size(12.0).color(color::ORANGE).lines(2).grow(1.0)]).gap(6.0));
    }
    let mut net = vec![icon(icons::WIFI).font_size(12.0).color(color::TEXT2), text(l.net_label.clone()).font_size(12.0).secondary()];
    if l.can_switch && l.device.is_none() {
        net.push(capsule(icons::SYNC, tr("Ganti", "Switch"), "phone.adapter").name(tr("Ganti jaringan", "Switch network")));
    }
    net.push(spacer());
    net.push(capsule(icons::CLOSE, tr("Batal", "Cancel"), "phone.stop"));
    right.push(row(net).gap(6.0).fill_w());
    row([code, col(right).gap(6.0).grow(1.0).align(Align::Start).h(side)]).gap(16.0).fill_w().align(Align::Start)
}

fn transfer_list(app: &App, l: &Live) -> Node {
    let accent = app.base_accent();
    let hide = hide_names(app);
    let mut kids: Vec<Node> = vec![];
    for (i, r) in l.rows.iter().take(4).enumerate() {
        let label = if hide { tr_s(format!("File {}", i + 1), format!("File {}", i + 1)) } else { r.name.clone() };
        let lead = match r.state {
            RowState::Done => stack([icon(icons::CHECK).font_size(10.0).color(color::INK)]).size(20.0, 20.0).bg(color::GREEN).capsule(),
            RowState::Failed => stack([icon(icons::CLOSE).font_size(10.0).color(color::INK)]).size(20.0, 20.0).bg(color::RED).capsule(),
            RowState::Active => ring(if r.size > 0 { r.done as f32 / r.size as f32 } else { 0.0 }).color(accent),
            RowState::Waiting => stack([icon(icons::DOCUMENT).font_size(11.0).color(color::TEXT3)]).size(20.0, 20.0),
        };
        let sub = match r.state {
            RowState::Active if r.size > 0 => format!("{} / {}", fmt_size(r.done), fmt_size(r.size)),
            RowState::Failed => tr("Gagal", "Failed").to_string(),
            _ => fmt_size(r.size),
        };
        kids.push(row([lead, text(label).semibold().font_size(12.5).grow(1.0), caption(sub)]).gap(10.0).fill_w().pad_xy(10.0, 6.0).bg(color::FILL).radius(12.0));
    }
    if l.rows.len() > 4 {
        kids.push(caption(tr_s(format!("+{} lagi", l.rows.len() - 4), format!("+{} more", l.rows.len() - 4))).pad_xy(6.0, 0.0));
    }
    let status = if l.batch_done {
        row([icon(icons::CHECK).font_size(12.0).color(color::GREEN), text(tr("Selesai. Bisa kirim lagi sampai sesi berakhir.", "Done. You can send more until the session ends.")).font_size(12.0).color(color::GREEN).grow(1.0), capsule(icons::FOLDER, tr("Folder", "Folder"), "phone.open_folder"), capsule(icons::CLOSE, tr("Tutup", "Close"), "phone.stop")]).gap(8.0).fill_w()
    } else {
        row([progress(progress_of(l)).color(accent).grow(1.0), caption(format!("{}%", (progress_of(l) * 100.0).round() as u32)), capsule(icons::CLOSE, tr("Batal", "Cancel"), "phone.stop")]).gap(10.0).fill_w()
    };
    kids.push(status);
    col(kids).gap(6.0).fill_w()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::phone_send::adapter::{Category, IF_ETHERNET, IF_WIFI};
    use std::net::Ipv4Addr;

    fn wifi() -> Adapter {
        Adapter { name: "Wi-Fi".into(), description: "Intel(R) Wi-Fi 6 AX201".into(), guid: String::new(), category: Category::Private, if_type: IF_WIFI, up: true, ip: Ipv4Addr::new(192, 168, 1, 23), has_gateway: true, metric: 35 }
    }
    fn vpn() -> Adapter {
        Adapter { name: "VPN".into(), description: "WireGuard Tunnel".into(), guid: String::new(), category: Category::Private, if_type: IF_ETHERNET, up: true, ip: Ipv4Addr::new(10, 66, 66, 2), has_gateway: true, metric: 1 }
    }

    #[test]
    fn the_server_listens_on_the_lan_adapter_by_default() {
        let (ip, idx) = choose_bind(&[vpn(), wifi()], None, false, None, false).unwrap();
        assert_eq!((ip, idx), (IpAddr::V4(Ipv4Addr::new(192, 168, 1, 23)), Some(1)));
        // The user's earlier choice wins while it still qualifies.
        let mut eth = wifi();
        eth.name = "Ethernet".into();
        eth.if_type = IF_ETHERNET;
        eth.ip = Ipv4Addr::new(192, 168, 1, 40);
        assert_eq!(choose_bind(&[wifi(), eth], Some("Ethernet"), false, None, false).unwrap().1, Some(1));
    }

    #[test]
    fn a_public_network_is_refused_and_asked_about_once_per_session() {
        let public = |mut a: Adapter| {
            a.category = Category::Public;
            a
        };
        let cafe = [public(wifi())];
        // The decision with fake adapters: refused, with the reason (the pill asks "Tetap lanjutkan").
        assert_eq!(choose_bind(&cafe, None, false, None, false), Err(BindError::PublicNetwork));
        assert_eq!(choose_bind(&cafe, Some("Wi-Fi"), false, None, false), Err(BindError::PublicNetwork));
        // Confirmed for this session: the Public adapter is used.
        assert_eq!(choose_bind(&cafe, None, false, None, true).unwrap(), (IpAddr::V4(Ipv4Addr::new(192, 168, 1, 23)), Some(0)));
        // Unknown and Domain are not Public.
        for c in [Category::Unknown, Category::Domain, Category::Private] {
            let mut a = wifi();
            a.category = c;
            assert_eq!(choose_bind(&[a], None, false, None, false).unwrap().1, Some(0), "{c:?}");
        }
        // Public Wi-Fi + Private cable: the cable, no question.
        let mut eth = wifi();
        eth.name = "Ethernet".into();
        eth.if_type = IF_ETHERNET;
        eth.ip = Ipv4Addr::new(192, 168, 1, 40);
        assert_eq!(choose_bind(&[public(wifi()), eth], None, false, None, false).unwrap().1, Some(1));
        // Nothing usable is "no network", not "public".
        assert_eq!(choose_bind(&[public(vpn())], None, false, None, false), Err(BindError::NoNetwork));
        // The loopback test override is not a network: no question.
        assert!(choose_bind(&cafe, None, true, Some("127.0.0.1"), false).unwrap().0.is_loopback());
    }

    #[test]
    fn no_network_means_no_server() {
        assert_eq!(choose_bind(&[], None, false, None, false), Err(BindError::NoNetwork));
        assert_eq!(choose_bind(&[vpn()], None, true, None, false), Err(BindError::NoNetwork));
    }

    #[test]
    fn the_test_override_can_only_ever_name_loopback_and_only_in_debug() {
        let lan = [wifi()];
        // Debug + loopback: used.
        assert_eq!(choose_bind(&lan, None, true, Some("127.0.0.1"), false).unwrap(), (IpAddr::from([127, 0, 0, 1]), None));
        assert_eq!(choose_bind(&lan, None, true, Some(" 127.0.0.2 "), false).unwrap().0, IpAddr::from([127, 0, 0, 2]));
        // Not in debug: ignored, the LAN adapter is used.
        assert_eq!(choose_bind(&lan, None, false, Some("127.0.0.1"), false).unwrap().1, Some(0));
        // Anything that is not loopback is ignored even in debug: never the LAN, never every interface.
        for bad in ["0.0.0.0", "192.168.1.23", "10.0.0.1", "::", "8.8.8.8", "localhost", "", "127.0.0.1:80", "not an ip"] {
            let got = choose_bind(&lan, None, true, Some(bad), false).unwrap();
            assert_eq!(got.1, Some(0), "{bad:?} must not be honoured");
        }
        // IPv6 loopback is loopback too.
        assert!(choose_bind(&lan, None, true, Some("::1"), false).unwrap().0.is_loopback());
    }

    #[test]
    fn the_receive_folder_override_needs_debug_and_the_default_is_downloads_moku() {
        let dl = Some(PathBuf::from(r"C:\Users\x\Downloads"));
        let fb = PathBuf::from(r"C:\data\Received");
        assert_eq!(pick_receive_dir(Some("D:\\scratch".into()), true, dl.clone(), fb.clone()), PathBuf::from("D:\\scratch"));
        assert_eq!(pick_receive_dir(Some("D:\\scratch".into()), false, dl.clone(), fb.clone()), PathBuf::from(r"C:\Users\x\Downloads\Moku"), "ignored in a normal island");
        assert_eq!(pick_receive_dir(Some("".into()), true, dl.clone(), fb.clone()), PathBuf::from(r"C:\Users\x\Downloads\Moku"));
        assert_eq!(pick_receive_dir(None, false, dl, fb.clone()), PathBuf::from(r"C:\Users\x\Downloads\Moku"));
        assert_eq!(pick_receive_dir(None, false, None, fb.clone()), fb, "no Downloads folder: the app's own data folder");
    }

    #[test]
    fn the_real_downloads_folder_is_found_without_touching_it() {
        assert!(known_downloads().is_some_and(|d| d.is_absolute()));
    }

    #[test]
    fn the_clock_and_progress_read_well() {
        assert_eq!(clock(600), "10:00");
        assert_eq!(clock(61), "1:01");
        assert_eq!(clock(0), "0:00");
    }
}
