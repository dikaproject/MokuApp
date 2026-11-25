//! The mirrored notifications (pure logic, no UI, no Windows API): the allowlist, one-time-code
//! detection, hiding the text during a meeting, and the in-memory list with its cap.
//!
//! Nothing here is ever written to disk. The list lives in memory only and is capped
//! (`[notify_mirror] max_items`), so a chat that floods cannot grow it without bound, and what
//! is not on the allowlist is dropped on arrival: only the app's name is kept (to offer "izinkan").

use std::collections::VecDeque;

use chrono::{DateTime, Local};

use super::otp;

/// Longest title / body kept (characters).
pub const MAX_TITLE: usize = 100;
pub const MAX_BODY: usize = 280;
/// Names of apps that sent a notification but are not allowed, kept to offer them in the tab.
pub const MAX_SEEN: usize = 8;
/// Windows notification ids remembered, so one notification that is reported twice shows once.
const DEDUPE: usize = 64;

/// What the listener read from one Windows notification.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Raw {
    /// Windows' id of the notification (0 for a sample).
    pub win_id: u32,
    /// The app's display name (`AppInfo.DisplayInfo.DisplayName`), e.g. `WhatsApp`.
    pub app: String,
    /// The app's AppUserModelId, e.g. `5319275A.WhatsAppDesktop_cv1g1gvanyjgm!App`.
    pub aumid: String,
    pub title: String,
    pub body: String,
}

/// One mirrored notification as kept in memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub seq: u64,
    pub app: String,
    pub aumid: String,
    pub title: String,
    pub body: String,
    /// A one-time code found in the text (digits only).
    pub code: Option<String>,
    pub time: DateTime<Local>,
    pub unread: bool,
}

fn norm(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Is the app on the allowlist? An entry matches the app's display name or its AppUserModelId as
/// a whole, ignoring case and surrounding spaces (`Teams` is not `Steam`, `Mail` is not `Gmail`).
pub fn allowed(apps: &[String], app: &str, aumid: &str) -> bool {
    let (a, id) = (norm(app), norm(aumid));
    apps.iter().map(|e| norm(e)).any(|e| !e.is_empty() && (e == a || e == id))
}

/// One line of at most `max` characters: control characters become spaces, the rest is cut.
fn clip(s: &str, max: usize) -> String {
    let one: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let t = one.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() <= max {
        return t;
    }
    let mut out: String = t.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Why a notification was not added.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dropped {
    /// The app is not on the allowlist.
    NotAllowed,
    /// The same Windows notification was already shown.
    Duplicate,
    /// No text at all.
    Empty,
}

/// Is the text visible? Hidden while a meeting runs and the privacy setting asks for it (the same
/// rule as the notification centre: the title stays, the text goes).
pub fn body_hidden(meeting_active: bool, hide_setting: bool) -> bool {
    meeting_active && hide_setting
}

/// The mirrored notifications, newest first.
#[derive(Default)]
pub struct Mirror {
    items: VecDeque<Item>,
    next_seq: u64,
    seen_ids: VecDeque<u32>,
    /// Apps that sent something while not on the allowlist (names only), newest first.
    seen_apps: VecDeque<String>,
}

impl Mirror {
    /// Add a notification if its app is on the allowlist. The list is then cut to `cap` (the
    /// oldest go). Returns the new item's `seq`.
    pub fn push(&mut self, raw: Raw, apps: &[String], cap: usize, now: DateTime<Local>) -> Result<u64, Dropped> {
        if !allowed(apps, &raw.app, &raw.aumid) {
            self.note_app(&raw.app);
            return Err(Dropped::NotAllowed);
        }
        if raw.win_id != 0 {
            if self.seen_ids.contains(&raw.win_id) {
                return Err(Dropped::Duplicate);
            }
            self.seen_ids.push_front(raw.win_id);
            self.seen_ids.truncate(DEDUPE);
        }
        // The code is looked for in the full text, before it is cut, and **masked** in what is kept: the
        // pill and the panel show dots like the clipboard does; only "Salin kode" has the real digits.
        let code = otp::find_code(&format!("{}\n{}", raw.title, raw.body));
        let (shown_title, shown_body) = match &code {
            Some(c) => (otp::mask(&raw.title, c), otp::mask(&raw.body, c)),
            None => (raw.title.clone(), raw.body.clone()),
        };
        let title = clip(&shown_title, MAX_TITLE);
        let body = clip(&shown_body, MAX_BODY);
        if title.is_empty() && body.is_empty() {
            return Err(Dropped::Empty);
        }
        self.next_seq += 1;
        let seq = self.next_seq;
        let app = clip(&raw.app, 40);
        self.items.push_front(Item { seq, app, aumid: raw.aumid, title, body, code, time: now, unread: true });
        self.set_cap(cap);
        Ok(seq)
    }

    /// Remember the name of an app that is not allowed (newest first, no repeats, at most
    /// [`MAX_SEEN`]). Names only: nothing of the notification.
    pub fn note_app(&mut self, app: &str) {
        let name = clip(app, 40);
        if name.is_empty() || self.seen_apps.iter().any(|a| norm(a) == norm(&name)) {
            return;
        }
        self.seen_apps.push_front(name);
        self.seen_apps.truncate(MAX_SEEN);
    }

    /// Apps seen but not on the allowlist, newest first, without those that were allowed since.
    pub fn seen_apps<'a>(&'a self, apps: &'a [String]) -> impl Iterator<Item = &'a String> {
        self.seen_apps.iter().filter(move |a| !allowed(apps, a, ""))
    }

    pub fn forget_seen(&mut self, app: &str) {
        self.seen_apps.retain(|a| norm(a) != norm(app));
    }

    /// Drop the oldest items beyond `cap` (at least 1 is kept).
    pub fn set_cap(&mut self, cap: usize) {
        self.items.truncate(cap.max(1));
    }

    pub fn get(&self, seq: u64) -> Option<&Item> {
        self.items.iter().find(|i| i.seq == seq)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Item> {
        self.items.iter()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn unread(&self) -> usize {
        self.items.iter().filter(|i| i.unread).count()
    }

    pub fn mark_all_read(&mut self) {
        for i in &mut self.items {
            i.unread = false;
        }
    }

    pub fn dismiss(&mut self, seq: u64) -> Option<Item> {
        let i = self.items.iter().position(|i| i.seq == seq)?;
        self.items.remove(i)
    }

    /// Everything, the seen names included ("Hapus semua data", "Bersihkan").
    pub fn clear(&mut self) {
        self.items.clear();
        self.seen_ids.clear();
        self.seen_apps.clear();
    }

    /// Only the notifications (the "Bersihkan" button): the seen names stay.
    pub fn clear_items(&mut self) {
        self.items.clear();
    }
}

/// What the listener reports about itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Status {
    /// Off in the settings.
    #[default]
    Off,
    /// Switched on, the service is checking.
    Starting,
    /// Subscribed; notifications of allowed apps appear.
    Listening,
    /// Windows has not allowed this app to read notifications (Settings → Privacy → Notifications).
    NeedsAccess,
    /// Windows refuses the API to a program without a package identity: this needs the
    /// Store / MSIX build (`docs/notifications.md`).
    NeedsPackage,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Local> {
        Local::now()
    }

    fn apps(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn raw(app: &str, title: &str, body: &str) -> Raw {
        Raw { win_id: 0, app: app.into(), aumid: format!("{app}.id!App"), title: title.into(), body: body.into() }
    }

    #[test]
    fn the_allowlist_matches_whole_names_ignoring_case() {
        let list = apps(&["WhatsApp", " telegram desktop ", "5319275A.WhatsAppDesktop_cv1g1gvanyjgm!App", ""]);
        assert!(allowed(&list, "WhatsApp", ""));
        assert!(allowed(&list, "whatsapp", "x"));
        assert!(allowed(&list, "  Telegram Desktop", ""));
        // The AppUserModelId matches too (an app whose display name changes).
        assert!(allowed(&list, "WhatsApp Beta", "5319275a.whatsappdesktop_cv1g1gvanyjgm!app"));
        // Whole names only: no substring, no prefix.
        assert!(!allowed(&list, "WhatsApp Business", "other"));
        assert!(!allowed(&list, "Telegram", ""));
        assert!(!allowed(&apps(&["Teams"]), "Steam", ""));
        assert!(!allowed(&apps(&["Mail"]), "Gmail", ""));
        // An empty entry or an empty list allows nothing, not even an app without a name.
        assert!(!allowed(&list, "", ""));
        assert!(!allowed(&apps(&[]), "WhatsApp", ""));
        assert!(!allowed(&apps(&["", "  "]), "", ""));
    }

    #[test]
    fn only_allowed_apps_are_kept_and_other_names_are_only_noted() {
        let mut m = Mirror::default();
        let list = apps(&["WhatsApp"]);
        assert_eq!(m.push(raw("Spotify", "Lagu baru", "dari artis"), &list, 30, now()), Err(Dropped::NotAllowed));
        assert_eq!(m.push(raw("Spotify", "Lagu lain", ""), &list, 30, now()), Err(Dropped::NotAllowed));
        assert!(m.is_empty(), "nothing of a dropped notification is kept");
        let ok = m.push(raw("WhatsApp", "Nadia", "Meeting-nya jadi jam 3 ya?"), &list, 30, now()).unwrap();
        let it = m.get(ok).unwrap();
        assert_eq!((it.app.as_str(), it.title.as_str(), it.body.as_str(), it.code.as_deref(), it.unread), ("WhatsApp", "Nadia", "Meeting-nya jadi jam 3 ya?", None, true));
        // The name of the dropped app is remembered once, so the tab can offer "izinkan"; its text is not.
        let seen: Vec<&String> = m.seen_apps(&list).collect();
        assert_eq!(seen, vec!["Spotify"]);
        // Once allowed, it is no longer offered.
        assert_eq!(m.seen_apps(&apps(&["WhatsApp", "spotify"])).count(), 0);
        m.forget_seen("SPOTIFY");
        assert_eq!(m.seen_apps(&list).count(), 0);
    }

    #[test]
    fn seen_app_names_are_bounded_and_unique() {
        let mut m = Mirror::default();
        for i in 0..30 {
            m.note_app(&format!("App {i}"));
            // The same name again (any case) is not added twice.
            m.note_app(&format!("APP {i}"));
        }
        let seen: Vec<&String> = m.seen_apps(&[]).collect();
        assert_eq!(seen.len(), MAX_SEEN);
        assert_eq!(seen[0], "App 29", "newest first");
        assert_eq!(seen.iter().filter(|a| a.as_str() == "App 0").count(), 0, "the oldest fell out");
        m.note_app("   ");
        assert_eq!(m.seen_apps(&[]).count(), MAX_SEEN);
    }

    #[test]
    fn the_list_is_capped_in_memory_newest_first() {
        let mut m = Mirror::default();
        let list = apps(&["Telegram"]);
        let mut seqs = vec![];
        for i in 0..50 {
            seqs.push(m.push(raw("Telegram", &format!("Chat {i}"), "hai"), &list, 30, now()).unwrap());
        }
        assert_eq!(m.len(), 30, "the cap");
        assert_eq!(m.iter().next().unwrap().title, "Chat 49", "newest first");
        assert_eq!(m.iter().last().unwrap().title, "Chat 20", "the 20 oldest are gone");
        assert!(m.get(seqs[0]).is_none() && m.get(seqs[49]).is_some());
        assert!(seqs.windows(2).all(|w| w[0] < w[1]), "seq only grows");
        // A smaller cap (the setting changed) cuts at once; there is always one left.
        m.set_cap(5);
        assert_eq!(m.len(), 5);
        m.set_cap(0);
        assert_eq!(m.len(), 1);
        // Clear empties everything.
        m.clear();
        assert!(m.is_empty());
        assert_eq!(m.seen_apps(&[]).count(), 0);
    }

    #[test]
    fn text_is_one_line_and_cut() {
        let mut m = Mirror::default();
        let list = apps(&["Discord"]);
        let long = "x".repeat(1000);
        let s = m.push(raw("Discord", &format!("  Server\n#umum {long}"), &format!("baris 1\r\nbaris 2\t{long}")), &list, 30, now()).unwrap();
        let it = m.get(s).unwrap();
        assert_eq!(it.title.chars().count(), MAX_TITLE);
        assert_eq!(it.body.chars().count(), MAX_BODY);
        assert!(it.title.ends_with('…') && it.body.starts_with("baris 1 baris 2 x"));
        assert!(!it.title.contains('\n') && !it.body.contains(['\n', '\r', '\t']));
        // No text at all: nothing to show.
        assert_eq!(m.push(raw("Discord", "  ", "\n"), &list, 30, now()), Err(Dropped::Empty));
        // Title only, body only: both fine.
        assert!(m.push(raw("Discord", "Pengguna bergabung", ""), &list, 30, now()).is_ok());
        assert!(m.push(raw("Discord", "", "hanya isi"), &list, 30, now()).is_ok());
    }

    #[test]
    fn one_windows_notification_shows_once() {
        let mut m = Mirror::default();
        let list = apps(&["Outlook"]);
        let r = Raw { win_id: 77, ..raw("Outlook", "Rapat", "jam 10") };
        assert!(m.push(r.clone(), &list, 30, now()).is_ok());
        assert_eq!(m.push(r.clone(), &list, 30, now()), Err(Dropped::Duplicate));
        // Another id, or an id-less sample, is new.
        assert!(m.push(Raw { win_id: 78, ..r.clone() }, &list, 30, now()).is_ok());
        assert!(m.push(Raw { win_id: 0, ..r.clone() }, &list, 30, now()).is_ok());
        assert!(m.push(Raw { win_id: 0, ..r }, &list, 30, now()).is_ok(), "id 0 is never a duplicate");
        // Only the last DEDUPE ids are remembered.
        for id in 100..300 {
            let _ = m.push(Raw { win_id: id, ..raw("Outlook", "x", "y") }, &list, 30, now());
        }
        assert!(m.push(Raw { win_id: 77, ..raw("Outlook", "again", "y") }, &list, 30, now()).is_ok());
    }

    #[test]
    fn a_one_time_code_is_found_in_the_full_text_and_kept() {
        let mut m = Mirror::default();
        let list = apps(&["Telegram", "WhatsApp"]);
        let s = m.push(raw("Telegram", "Telegram", "Login code: 31415. Do not give this code to anyone."), &list, 30, now()).unwrap();
        assert_eq!(m.get(s).unwrap().code.as_deref(), Some("31415"));
        let s = m.push(raw("WhatsApp", "WhatsApp", "Kode verifikasi WhatsApp Anda: 123-456"), &list, 30, now()).unwrap();
        assert_eq!(m.get(s).unwrap().code.as_deref(), Some("123456"));
        // The code in the title works too, and a code far into a long body (past the cut) is found.
        let s = m.push(raw("Telegram", "482913 adalah kode OTP Anda", ""), &list, 30, now()).unwrap();
        assert_eq!(m.get(s).unwrap().code.as_deref(), Some("482913"));
        let far = format!("{} kode OTP 665544", "lorem ".repeat(80));
        let s = m.push(raw("Telegram", "Bank", &far), &list, 30, now()).unwrap();
        assert_eq!(m.get(s).unwrap().code.as_deref(), Some("665544"));
        // A chat message with a number is not a code.
        let s = m.push(raw("WhatsApp", "Nadia", "Transfer Rp 150000 ya, nomor pesanan 4821"), &list, 30, now()).unwrap();
        assert_eq!(m.get(s).unwrap().code, None);
    }

    #[test]
    fn a_code_is_masked_in_what_the_pill_and_the_panel_show() {
        let mut m = Mirror::default();
        let list = apps(&["Telegram", "WhatsApp"]);
        let dots = |n: usize| "\u{2022}".repeat(n);
        // The body: dots (one per digit), the real digits only in `code` (what "Salin kode" copies).
        let s = m.push(raw("Telegram", "Telegram", "Login code: 31415. Do not give this code to anyone."), &list, 30, now()).unwrap();
        let it = m.get(s).unwrap();
        assert_eq!(it.body, format!("Login code: {}. Do not give this code to anyone.", dots(5)));
        assert_eq!(it.code.as_deref(), Some("31415"));
        assert!(!format!("{} {}", it.title, it.body).contains("31415"));
        // Two groups, in the title, and a code repeated in title and body.
        let s = m.push(raw("WhatsApp", "123-456 adalah kode OTP Anda", "Kode verifikasi 123-456, jangan bagikan"), &list, 30, now()).unwrap();
        let it = m.get(s).unwrap();
        assert_eq!(it.title, format!("{} adalah kode OTP Anda", dots(6)));
        assert_eq!(it.body, format!("Kode verifikasi {}, jangan bagikan", dots(6)));
        assert_eq!(it.code.as_deref(), Some("123456"));
        // A code far into a long body (past the cut) is not kept in the clear either.
        let far = format!("{} kode OTP 665544", "lorem ".repeat(80));
        let s = m.push(raw("Telegram", "Bank", &far), &list, 30, now()).unwrap();
        assert!(!m.get(s).unwrap().body.contains("665544"));
        assert_eq!(m.get(s).unwrap().code.as_deref(), Some("665544"));
        // A message without a code is untouched, other numbers included.
        let s = m.push(raw("WhatsApp", "Nadia", "Transfer Rp 150000 ya, nomor pesanan 4821"), &list, 30, now()).unwrap();
        assert_eq!(m.get(s).unwrap().body, "Transfer Rp 150000 ya, nomor pesanan 4821");
        // A notification that is only a code still has something to show (dots), not "empty".
        let s = m.push(raw("Telegram", "", "Login code: 48291"), &list, 30, now()).unwrap();
        assert_eq!(m.get(s).unwrap().body, format!("Login code: {}", dots(5)));
    }

    #[test]
    fn dismiss_unread_and_read_marks() {
        let mut m = Mirror::default();
        let list = apps(&["Teams"]);
        let a = m.push(raw("Teams", "A", "1"), &list, 30, now()).unwrap();
        let b = m.push(raw("Teams", "B", "2"), &list, 30, now()).unwrap();
        assert_eq!(m.unread(), 2);
        m.mark_all_read();
        assert_eq!(m.unread(), 0);
        assert_eq!(m.dismiss(a).map(|i| i.title), Some("A".into()));
        assert!(m.dismiss(a).is_none());
        assert_eq!((m.len(), m.get(b).is_some()), (1, true));
        m.note_app("Other");
        m.clear_items();
        assert_eq!((m.len(), m.seen_apps(&list).count()), (0, 1), "the Bersihkan button keeps the seen names");
    }

    #[test]
    fn the_text_is_hidden_only_in_a_meeting_with_the_setting_on() {
        assert!(body_hidden(true, true));
        assert!(!body_hidden(true, false), "the setting is off");
        assert!(!body_hidden(false, true), "no meeting");
        assert!(!body_hidden(false, false));
    }
}
