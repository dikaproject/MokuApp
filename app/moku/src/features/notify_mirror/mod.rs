//! Feature `notify_mirror` (v0.3, track Q): Windows notifications mirrored on the island.
//!
//! * a service ([`listener`]) subscribes to `UserNotificationListener.NotificationChanged` and
//!   reads each new notification of an **allowlisted app** (`[notify_mirror] apps`, matched by
//!   display name or AppUserModelId); every other app is dropped after reading its name, its
//!   title and text are never read. Off by default;
//! * a pill (priority `NOTIFICATION`, 6 s; 10 s with a code) shows the app's logo, the title and
//!   the text; a **one-time code** found in the text (4-8 digits next to *kode*, *OTP*, ...)
//!   adds "Salin kode": one click puts it on the clipboard marked "do not keep" (Moku's history,
//!   Windows' history and cloud sync skip it);
//! * the text is hidden during a meeting (`privacy.hide_notification_body_in_meetings`), as in
//!   the notification centre; focus mode and "Jangan ganggu" hold the pill (the list still fills);
//! * the Notifikasi tab lists the last few ([`panel_section`]) with the listener's status, and
//!   offers an app that sent something but is not allowed ("izinkan");
//! * **nothing is written to disk and nothing is logged** of a notification's content: the list
//!   is in memory (capped by `max_items`), and "Hapus semua data" empties it;
//! * Moku never removes or clears a Windows notification (`RemoveNotification` /
//!   `ClearNotifications` are not used) and never calls `RequestAccessAsync`.
//!
//! Testing: `MOKU_DEBUG=1` adds the actions `notify_mirror.sample:<wa|otp|long|other|many|clear>`
//! and `notify_mirror.sample_status:<listening|access|package|off>`; with
//! `MOKU_NOTIFY_MIRROR_SAMPLE=1` as well, the real listener never starts (a test island must not
//! read the user's real notifications).

mod listener;
mod model;
mod otp;

use std::collections::HashMap;
use std::time::Duration;

use chrono::Local;

use crate::activity::{Activity, HEADS_UP_SECS, priority};
use crate::app::{App, Redraw};
use crate::i18n::{self, tr, tr_s};
use crate::registry::*;
use crate::ui::*;

use model::{Dropped, Item, Mirror, Raw, Status};

const PILL: &str = "notify_mirror.pill";
/// Rows shown in the Notifikasi tab.
const ROWS: usize = 3;
/// "Tersalin" stays on the button this long.
const COPIED_MS: u64 = 1800;
/// A pill with a code stays longer, so the button can be reached.
const CODE_SECS: f32 = 10.0;
/// App logos kept.
const LOGOS_MAX: usize = 16;

#[derive(Default)]
struct State {
    mirror: Mirror,
    status: Status,
    /// The item the pill shows.
    pill_seq: Option<u64>,
    /// Item whose code was just copied (the button says "Tersalin"), with the token that ends it.
    copied: Option<(u64, u64)>,
    flash_token: u64,
    /// Decoded app logos by AppUserModelId.
    logos: HashMap<String, Image>,
}

pub fn register(r: &mut Registry) {
    r.add_state(State::default());
    r.add_service(ServiceSpec { name: "notify-mirror", enabled: |c| c.notify_mirror.enabled && !sample_only(), run: listener::run });
    r.on_action("notify_mirror", on_action);
    r.on_config(on_config);
    r.on_event(on_event);
}

/// A test island with `MOKU_NOTIFY_MIRROR_SAMPLE=1` never starts the real listener (debug only).
fn sample_only() -> bool {
    crate::app::debug_enabled() && std::env::var("MOKU_NOTIFY_MIRROR_SAMPLE").is_ok_and(|v| v == "1")
}

fn hidden(app: &App) -> bool {
    model::body_hidden(app.meeting().active, app.config().privacy.hide_notification_body_in_meetings)
}

// ---- from the listener (UI thread) --------------------------------------------------------------

/// A notification of an allowed app arrived (the listener, or a sample).
fn ingest(app: &mut App, raw: Raw, logo: Option<Image>) {
    let cfg = app.config_arc();
    if !cfg.notify_mirror.enabled {
        return;
    }
    let aumid = raw.aumid.clone();
    let result = app.state_mut::<State>().mirror.push(raw, &cfg.notify_mirror.apps, cfg.notify_mirror.max_items as usize, Local::now());
    match result {
        Ok(seq) => {
            let st = app.state_mut::<State>();
            if let Some(img) = logo {
                if st.logos.len() >= LOGOS_MAX && !st.logos.contains_key(&aumid) {
                    st.logos.clear();
                }
                st.logos.insert(aumid, img);
            }
            // Focus mode and "Jangan ganggu" hold the pill; the list has it either way.
            if cfg.notify_mirror.pill && !app.focus_hold() && !crate::features::notify::dnd_on(app) {
                show_pill(app, seq);
            }
        }
        Err(Dropped::NotAllowed | Dropped::Duplicate | Dropped::Empty) => {}
    }
    app.request_redraw(Redraw::Tab("notify"));
}

/// An app that is not allowed sent something (its name only).
fn note_app(app: &mut App, name: &str) {
    app.state_mut::<State>().mirror.note_app(name);
    app.request_redraw(Redraw::Tab("notify"));
}

fn set_status(app: &mut App, status: Status) {
    if app.state::<State>().status != status {
        app.state_mut::<State>().status = status;
        app.request_redraw(Redraw::Tab("notify"));
    }
}

fn show_pill(app: &mut App, seq: u64) {
    let secs = if app.state::<State>().mirror.get(seq).is_some_and(|i| i.code.is_some()) { CODE_SECS } else { HEADS_UP_SECS };
    app.state_mut::<State>().pill_seq = Some(seq);
    app.set_activity(Activity::transient(PILL, priority::NOTIFICATION, secs, pill_node).tab("notify"));
}

// ---- config, events, actions --------------------------------------------------------------------

fn on_config(app: &mut App) {
    let cfg = app.config_arc();
    if !cfg.notify_mirror.enabled {
        let st = app.state_mut::<State>();
        let had = !st.mirror.is_empty() || st.status != Status::Off;
        st.mirror.clear();
        st.status = Status::Off;
        st.pill_seq = None;
        st.logos.clear();
        if had {
            app.clear_activity(PILL);
            app.request_redraw(Redraw::All);
        }
        return;
    }
    if sample_only() && app.state::<State>().status == Status::Off {
        app.state_mut::<State>().status = Status::Listening;
    }
    app.state_mut::<State>().mirror.set_cap(cfg.notify_mirror.max_items as usize);
}

fn on_event(app: &mut App, ev: &AppEvent) {
    match ev {
        AppEvent::TabHidden("notify") => app.state_mut::<State>().mirror.mark_all_read(),
        AppEvent::MeetingChanged => app.request_redraw(Redraw::All),
        // "Hapus semua data": a notification is user content, even though it is only in memory.
        AppEvent::DataCleared => {
            let st = app.state_mut::<State>();
            st.mirror.clear();
            st.pill_seq = None;
            st.logos.clear();
            app.clear_activity(PILL);
            app.request_redraw(Redraw::All);
        }
        _ => {}
    }
}

fn on_action(app: &mut App, ev: &ActionEvent) {
    if ev.kind != ActionKind::Click {
        return;
    }
    let seq = ev.arg().and_then(|a| a.parse::<u64>().ok());
    match ev.name() {
        "copy" => {
            if let Some(code) = seq.and_then(|s| app.state::<State>().mirror.get(s)).and_then(|i| i.code.clone()) {
                copy_code(app, seq.unwrap_or(0), &code);
            }
        }
        "dismiss" => {
            if let Some(s) = seq {
                app.state_mut::<State>().mirror.dismiss(s);
                if app.state::<State>().pill_seq == Some(s) {
                    app.clear_activity(PILL);
                }
                app.request_redraw(Redraw::All);
            }
        }
        "clear" => {
            app.state_mut::<State>().mirror.clear_items();
            app.state_mut::<State>().pill_seq = None;
            app.clear_activity(PILL);
            app.request_redraw(Redraw::All);
        }
        "allow" => {
            // The index of the app in the "izinkan" row: its name goes on the allowlist.
            let cfg = app.config_arc();
            let name = ev.arg().and_then(|a| a.parse::<usize>().ok()).and_then(|i| app.state::<State>().mirror.seen_apps(&cfg.notify_mirror.apps).nth(i).cloned());
            if let Some(name) = name {
                if let Err(e) = app.update_config(|c| c.notify_mirror.apps.push(name.clone())) {
                    log_warn!("notify mirror: cannot save the allowlist: {e}");
                }
                app.state_mut::<State>().mirror.forget_seen(&name);
                app.request_redraw(Redraw::All);
            }
        }
        "sample" => sample(app, ev.arg().unwrap_or("")),
        "sample_status" => {
            let s = match ev.arg() {
                Some("listening") => Status::Listening,
                Some("access") => Status::NeedsAccess,
                Some("package") => Status::NeedsPackage,
                _ => Status::Off,
            };
            set_status(app, s);
        }
        _ => {}
    }
}

/// Put the code on the clipboard, marked so that nothing keeps it: Moku's clipboard history
/// (its own marker), Windows' clipboard history and cloud sync (the documented exclusion formats).
fn put_code(owner: windows::Win32::Foundation::HWND, code: &str) -> bool {
    use crate::features::clipboard::sys;
    use windows::Win32::System::Ole::CF_UNICODETEXT;
    let mut utf16: Vec<u8> = code.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    utf16.extend([0, 0]);
    let mut formats = vec![(CF_UNICODETEXT.0 as u32, utf16), (sys::fmt_marker(), vec![1])];
    formats.extend(sys::private_formats());
    sys::set_formats(owner, &formats)
}

fn copy_code(app: &mut App, seq: u64, code: &str) {
    if put_code(app.main_hwnd(), code) {
        let st = app.state_mut::<State>();
        st.flash_token += 1;
        let token = st.flash_token;
        st.copied = Some((seq, token));
        app.after(Duration::from_millis(COPIED_MS), move |app| {
            if app.state::<State>().copied == Some((seq, token)) {
                app.state_mut::<State>().copied = None;
                app.request_redraw(Redraw::All);
            }
        });
    }
    // A failed copy shows nothing new: the button stays, the user can click it again.
    app.request_redraw(Redraw::All);
}

/// `notify_mirror.sample:<kind>` (MOKU_DEBUG only): invented notifications through the real path.
fn sample(app: &mut App, kind: &str) {
    let mk = |app_name: &str, title: &str, body: &str| Raw { win_id: 0, app: app_name.into(), aumid: format!("sample.{}!App", app_name.to_lowercase().replace(' ', "")), title: title.into(), body: body.into() };
    match kind {
        "wa" => ingest(app, mk("WhatsApp", "Nadia", "Meeting-nya jadi jam 3 ya?"), None),
        "otp" => ingest(app, mk("Telegram", "Telegram", "Kode login: 482913. Jangan bagikan kode ini kepada siapa pun."), None),
        "long" => ingest(app, mk("Discord", "Server Komunitas · #umum", &format!("Halo semuanya! {}", "Ini pesan yang sangat panjang sekali. ".repeat(12))), None),
        "other" => ingest(app, mk("Spotify", "Lagu baru", "dari artis favorit"), None),
        "many" => {
            for i in 0..45 {
                ingest(app, mk("Telegram", &format!("Obrolan {i}"), "pesan"), None);
            }
        }
        "clear" => {
            app.state_mut::<State>().mirror.clear();
            app.state_mut::<State>().pill_seq = None;
            app.clear_activity(PILL);
        }
        _ => {}
    }
    app.request_redraw(Redraw::All);
}

// ---- look ---------------------------------------------------------------------------------------

/// The app's logo, or its initial on a colour picked from its name.
fn app_tile(app: &App, it: &Item, size: f32) -> Node {
    if let Some(img) = app.state::<State>().logos.get(&it.aumid) {
        return image(img, (size * 0.24).round()).size(size, size);
    }
    const TINTS: [Rgba; 6] = [color::GREEN, color::BLUE, color::PURPLE, color::ORANGE, color::TEAL, color::PINK];
    let h = it.app.bytes().fold(7usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize));
    let initial: String = it.app.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "?".into());
    stack([text(initial).font_size((size * 0.5).round()).bold().color(color::INK)]).size(size, size).bg(TINTS[h % TINTS.len()]).radius((size * 0.3).round())
}

fn when(it: &Item) -> String {
    let now = Local::now();
    if now.signed_duration_since(it.time).num_seconds() < 60 {
        tr("kini", "now").to_string()
    } else if it.time.date_naive() == now.date_naive() {
        i18n::clock(&it.time)
    } else {
        i18n::date_short(&it.time)
    }
}

fn copy_button(app: &App, it: &Item) -> Option<Node> {
    it.code.as_ref()?;
    let copied = app.state::<State>().copied.is_some_and(|(s, _)| s == it.seq);
    let (glyph, label_text, bg) = if copied { (icons::CHECK, tr("Tersalin", "Copied"), color::GREEN) } else { (icons::COPY, tr("Salin kode", "Copy code"), color::FILL2) };
    let fg = if copied { color::INK } else { color::WHITE };
    Some(row([icon(glyph).font_size(12.0).color(fg), text(label_text).font_size(12.0).semibold().color(fg)]).gap(6.0).pad_xy(12.0, 0.0).h(28.0).bg(bg).capsule().action(format!("notify_mirror.copy:{}", it.seq)).name(tr("Salin kode", "Copy code")))
}

fn texts(app: &App, it: &Item) -> (String, Option<String>) {
    let title = if it.title.is_empty() { it.app.clone() } else { it.title.clone() };
    let body = if hidden(app) {
        Some(tr("Isi disembunyikan selama rapat", "Hidden during the meeting").to_string())
    } else if it.body.is_empty() {
        None
    } else {
        Some(it.body.clone())
    };
    (title, body)
}

/// The pill: "[logo] Nadia / Meeting-nya jadi jam 3 ya?   WhatsApp · kini", plus "Salin kode".
fn pill_node(app: &App) -> Node {
    let s = app.state::<State>();
    let Some(it) = s.pill_seq.and_then(|q| s.mirror.get(q)) else { return empty() };
    let (title, body) = texts(app, it);
    let mut kids = vec![app_tile(app, it, 32.0)];
    let mut lines = vec![text(title).semibold()];
    lines.extend(body.map(|b| text(b).font_size(12.5).secondary()));
    kids.push(col(lines).gap(1.0).max_w(300.0));
    kids.extend(copy_button(app, it));
    kids.push(caption(format!("{} · {}", it.app, when(it))).max_w(150.0));
    row(kids).gap(10.0).h(52.0).pad4(10.0, 0.0, 16.0, 0.0)
}

fn row_node(app: &App, it: &Item) -> Node {
    let (title, body) = texts(app, it);
    let mut lines = vec![row([text(title).semibold().max_w(260.0), caption(format!("{} · {}", it.app, when(it)))]).gap(8.0)];
    lines.extend(body.map(|b| text(b).font_size(12.5).secondary()));
    let mut kids = vec![app_tile(app, it, 30.0), col(lines).gap(2.0).grow(1.0)];
    kids.extend(copy_button(app, it));
    kids.push(stack([icon(icons::CLOSE).font_size(10.0).color(color::TEXT3)]).size(24.0, 24.0).capsule().action(format!("notify_mirror.dismiss:{}", it.seq)).name(tr("Tutup", "Dismiss")));
    row(kids).gap(10.0).fill_w().pad4(10.0, 8.0, 6.0, 8.0).bg(color::FILL).radius(16.0)
}

/// What the status means, for the line under the header (nothing while all is well).
fn status_note(s: Status) -> Option<String> {
    match s {
        Status::Listening | Status::Off => None,
        Status::Starting => Some(tr("Memeriksa izin notifikasi Windows…", "Checking the Windows notification permission…").to_string()),
        Status::NeedsAccess => Some(
            tr(
                "Windows belum mengizinkan Moku membaca notifikasi. Nyalakan di Pengaturan Windows → Privasi & keamanan → Notifikasi. Moku tidak pernah memunculkan jendela izin sendiri.",
                "Windows has not allowed Moku to read notifications. Switch it on in Windows Settings → Privacy & security → Notifications. Moku never opens the permission prompt itself.",
            )
            .to_string(),
        ),
        Status::NeedsPackage => Some(
            tr(
                "Butuh versi Store/MSIX: Windows hanya memberi akses notifikasi ke aplikasi dengan identitas paket. Lihat docs/notifications.md.",
                "Needs the Store/MSIX build: Windows only gives notification access to apps with a package identity. See docs/notifications.md.",
            )
            .to_string(),
        ),
    }
}

/// The Windows-notifications section of the Notifikasi tab (nothing while the feature is off).
pub fn panel_section(app: &App) -> Vec<Node> {
    let cfg = app.config();
    if !cfg.notify_mirror.enabled {
        return vec![];
    }
    let st = app.state::<State>();
    let note = status_note(st.status);
    let seen: Vec<&String> = st.mirror.seen_apps(&cfg.notify_mirror.apps).take(3).collect();
    // Nothing to show: all is well and nothing came yet.
    if st.mirror.is_empty() && note.is_none() && seen.is_empty() {
        return vec![];
    }
    let mut head = vec![label(tr("Notifikasi Windows", "Windows notifications")), spacer()];
    if !st.mirror.is_empty() {
        head.push(row([text(tr("Bersihkan", "Clear")).font_size(11.5).semibold()]).pad_xy(10.0, 0.0).h(24.0).bg(color::FILL2).capsule().action("notify_mirror.clear"));
    }
    let mut rows: Vec<Node> = vec![row(head).fill_w().pad_xy(8.0, 2.0)];
    if let Some(n) = note {
        rows.push(caption(n).lines(3).fill_w().pad_xy(8.0, 0.0));
    }
    rows.extend(st.mirror.iter().take(ROWS).map(|it| row_node(app, it)));
    let more = st.mirror.len().saturating_sub(ROWS);
    if more > 0 {
        rows.push(caption(tr_s(format!("{more} lagi"), format!("{more} more"))).pad_xy(8.0, 0.0));
    }
    if !seen.is_empty() {
        let mut chips = vec![caption(tr("Izinkan notifikasi dari:", "Allow notifications from:"))];
        for (i, name) in seen.iter().enumerate() {
            chips.push(row([icon(icons::ADD).font_size(10.0), text((*name).clone()).font_size(11.5).semibold().max_w(110.0)]).gap(4.0).pad_xy(10.0, 0.0).h(24.0).bg(color::FILL2).capsule().action(format!("notify_mirror.allow:{i}")));
        }
        rows.push(row(chips).gap(6.0).fill_w().pad_xy(8.0, 2.0));
    }
    vec![col(rows).gap(6.0).fill_w().pad_xy(0.0, 6.0).bg(color::FILL).radius(16.0)]
}

/// The real clipboard (saved and restored afterwards), so `#[ignore]`d like the clipboard feature's
/// own live tests. Run: `cargo test -p moku notify_mirror_clipboard_live -- --ignored --test-threads=1`
#[cfg(test)]
mod live {
    use super::*;
    use crate::features::clipboard::sys;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard};
    use windows::Win32::System::Ole::CF_UNICODETEXT;
    use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE};
    use windows::core::w;

    struct Restore {
        backup: sys::Backup,
        owner: HWND,
    }

    impl Drop for Restore {
        fn drop(&mut self) {
            self.backup.restore(self.owner);
            unsafe {
                let _ = DestroyWindow(self.owner);
            }
        }
    }

    #[test]
    #[ignore = "uses the real clipboard (saved and restored); run with --ignored --test-threads=1"]
    fn notify_mirror_clipboard_live_the_code_is_copied_and_marked_do_not_keep() {
        unsafe {
            // A window of ours that can own the clipboard (a message-only STATIC window).
            let owner = CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!("MokuMirrorTest"), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, None, None).expect("window");
            let _restore = Restore { backup: sys::Backup::take(owner), owner };
            assert!(put_code(owner, "482913"));
            assert!(OpenClipboard(Some(owner)).is_ok());
            let text = GetClipboardData(CF_UNICODETEXT.0 as u32).ok().map(|h| {
                let p = windows::Win32::System::Memory::GlobalLock(windows::Win32::Foundation::HGLOBAL(h.0)) as *const u16;
                let mut n = 0;
                while *p.add(n) != 0 {
                    n += 1;
                }
                let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
                let _ = windows::Win32::System::Memory::GlobalUnlock(windows::Win32::Foundation::HGLOBAL(h.0));
                s
            });
            let marked = (IsClipboardFormatAvailable(sys::fmt_marker()).is_ok(), IsClipboardFormatAvailable(sys::fmt_exclude()).is_ok(), IsClipboardFormatAvailable(sys::fmt_can_include()).is_ok());
            let _ = CloseClipboard();
            assert_eq!(text.as_deref(), Some("482913"));
            // Moku's own marker, and both of Windows' "keep this out of history / cloud" formats.
            assert_eq!(marked, (true, true, true));
        }
    }
}
