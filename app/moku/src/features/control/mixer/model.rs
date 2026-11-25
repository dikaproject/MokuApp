//! The pure part of the per-app mixer: raw audio sessions in, a stable list of rows out.
//! Tested without any audio device.

use crate::ui::Image;

/// One audio session as the service reads it from Core Audio.
#[derive(Clone, Debug, PartialEq)]
pub struct RawSession {
    /// Identity of the *process* (see [`process_key`]): sessions of the same app share it.
    pub key: String,
    /// Display name (empty = none known).
    pub name: String,
    /// The Windows "system sounds" session.
    pub system: bool,
    pub volume: f32,
    pub muted: bool,
    /// Audio is flowing (or about to) as opposed to an open but silent session.
    pub active: bool,
    /// The session is over (Core Audio keeps it listed for a while).
    pub expired: bool,
    pub icon: Option<Image>,
}

/// One row of the mixer: an app.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Stable for as long as the app stays in the list; never reused. Used in action ids.
    pub id: u32,
    pub key: String,
    pub name: String,
    pub system: bool,
    pub volume: f32,
    pub muted: bool,
    pub active: bool,
    pub icon: Option<Image>,
}

/// What identifies an app: its executable's path (so every process of a browser, or a game and
/// its launcher helper in the same folder-and-name, is one row), else the process id.
/// The system-sounds session has its own key.
pub fn process_key(exe: Option<&str>, pid: u32, system: bool) -> String {
    if system {
        return "system-sounds".into();
    }
    match exe {
        Some(p) if !p.is_empty() => p.to_lowercase(),
        _ => format!("pid:{pid}"),
    }
}

/// Merge `raw` into the list. Rows of apps that are still there keep their place and id; new
/// apps are added at the end (active ones first, then by name); apps that are gone (or whose
/// sessions all expired) are dropped; a changed name is taken over. Sessions of one process are
/// one row: the volume is the loudest of them, it is muted only when all are, active when any is.
pub fn reduce(prev: &[Row], next_id: &mut u32, raw: Vec<RawSession>) -> Vec<Row> {
    // Group by process, in first-seen order.
    let mut groups: Vec<Row> = Vec::new();
    let mut muted_all: Vec<bool> = Vec::new();
    for s in raw.into_iter().filter(|s| !s.expired) {
        let vol = if s.volume.is_nan() { 0.0 } else { s.volume.clamp(0.0, 1.0) };
        match groups.iter().position(|g| g.key == s.key) {
            Some(i) => {
                let g = &mut groups[i];
                g.volume = g.volume.max(vol);
                muted_all[i] &= s.muted;
                g.active |= s.active;
                g.system |= s.system;
                if g.name.is_empty() {
                    g.name = s.name;
                }
                if g.icon.is_none() {
                    g.icon = s.icon;
                }
            }
            None => {
                muted_all.push(s.muted);
                groups.push(Row { id: 0, key: s.key, name: s.name, system: s.system, volume: vol, muted: s.muted, active: s.active, icon: s.icon });
            }
        }
    }
    for (g, m) in groups.iter_mut().zip(muted_all) {
        g.muted = m;
    }
    // Rows that were there keep their order (and id); the new ones follow.
    let mut out: Vec<Row> = Vec::with_capacity(groups.len());
    for p in prev {
        if let Some(i) = groups.iter().position(|g| g.key == p.key) {
            let mut g = groups.remove(i);
            g.id = p.id;
            out.push(g);
        }
    }
    groups.sort_by(|a, b| b.active.cmp(&a.active).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.key.cmp(&b.key)));
    for mut g in groups {
        *next_id += 1;
        g.id = *next_id;
        out.push(g);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(key: &str, name: &str, volume: f32, muted: bool, active: bool) -> RawSession {
        RawSession { key: key.into(), name: name.into(), system: false, volume, muted, active, expired: false, icon: None }
    }

    fn names(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn sessions_become_rows_active_first_then_by_name() {
        let mut id = 0;
        let rows = reduce(&[], &mut id, vec![raw("z.exe", "Zoom", 0.5, false, false), raw("s.exe", "Spotify", 0.8, false, true), raw("a.exe", "Arc", 1.0, false, true)]);
        assert_eq!(names(&rows), ["Arc", "Spotify", "Zoom"]);
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(id, 3);
        assert_eq!(rows[1].volume, 0.8);
    }

    #[test]
    fn a_new_app_is_added_at_the_end_and_a_gone_app_is_removed() {
        let mut id = 0;
        let first = reduce(&[], &mut id, vec![raw("a.exe", "Arc", 1.0, false, true), raw("s.exe", "Spotify", 0.8, false, true)]);
        // Spotify closes, Discord starts: Arc keeps its row, Discord follows.
        let second = reduce(&first, &mut id, vec![raw("a.exe", "Arc", 0.9, false, true), raw("d.exe", "Discord", 0.3, false, true)]);
        assert_eq!(names(&second), ["Arc", "Discord"]);
        assert_eq!(second[0].id, first[0].id, "ids are stable");
        assert_eq!(second[0].volume, 0.9, "the level is refreshed");
        assert_eq!(second[1].id, 3, "new ids continue the count");
        // Everything gone: empty list.
        assert!(reduce(&second, &mut id, vec![]).is_empty());
    }

    #[test]
    fn existing_rows_keep_their_order_even_when_the_names_would_sort_differently() {
        let mut id = 0;
        let first = reduce(&[], &mut id, vec![raw("z.exe", "Zoom", 0.5, false, true)]);
        let second = reduce(&first, &mut id, vec![raw("a.exe", "Arc", 0.5, false, true), raw("z.exe", "Zoom", 0.5, false, true)]);
        assert_eq!(names(&second), ["Zoom", "Arc"], "no rows jumping around under the pointer");
    }

    #[test]
    fn a_renamed_app_keeps_its_row() {
        let mut id = 0;
        let first = reduce(&[], &mut id, vec![raw("a.exe", "Arc", 1.0, false, true)]);
        let second = reduce(&first, &mut id, vec![raw("a.exe", "Arc Browser", 1.0, false, true)]);
        assert_eq!(names(&second), ["Arc Browser"]);
        assert_eq!(second[0].id, first[0].id);
    }

    #[test]
    fn sessions_of_one_process_are_one_row() {
        let mut id = 0;
        let rows = reduce(
            &[],
            &mut id,
            vec![raw("chrome.exe", "", 0.2, true, false), raw("chrome.exe", "Chrome", 0.7, false, true), raw("chrome.exe", "", 0.4, true, false), raw("mpv.exe", "mpv", 1.0, false, true)],
        );
        assert_eq!(rows.len(), 2);
        let chrome = rows.iter().find(|r| r.key == "chrome.exe").unwrap();
        assert_eq!(chrome.name, "Chrome", "the first name that is known");
        assert_eq!(chrome.volume, 0.7, "the loudest of its sessions");
        assert!(!chrome.muted, "muted only when every session is");
        assert!(chrome.active);
        // All muted: muted.
        let rows = reduce(&[], &mut id, vec![raw("c.exe", "C", 0.2, true, false), raw("c.exe", "C", 0.4, true, false)]);
        assert!(rows[0].muted && !rows[0].active);
    }

    #[test]
    fn process_keys_ignore_the_pid_when_the_path_is_known() {
        assert_eq!(process_key(Some(r"C:\Apps\Chrome\chrome.exe"), 100, false), process_key(Some(r"c:\apps\chrome\CHROME.EXE"), 200, false));
        assert_ne!(process_key(Some(r"C:\a\x.exe"), 1, false), process_key(Some(r"C:\b\x.exe"), 1, false));
        assert_eq!(process_key(None, 42, false), "pid:42");
        assert_eq!(process_key(Some(""), 42, false), "pid:42");
        assert_eq!(process_key(Some(r"C:\Windows\explorer.exe"), 1, true), "system-sounds");
    }

    #[test]
    fn expired_sessions_are_dropped() {
        let mut id = 0;
        let mut dead = raw("a.exe", "Arc", 1.0, false, false);
        dead.expired = true;
        let rows = reduce(&[], &mut id, vec![dead.clone(), raw("b.exe", "B", 1.0, false, true)]);
        assert_eq!(names(&rows), ["B"]);
        // An expired session next to a live one of the same app does not count.
        let rows = reduce(&[], &mut id, vec![dead, raw("a.exe", "Arc", 0.4, false, true)]);
        assert_eq!(rows[0].volume, 0.4);
    }

    #[test]
    fn levels_are_clamped_and_nan_is_silence() {
        let mut id = 0;
        let rows = reduce(&[], &mut id, vec![raw("a.exe", "A", 3.0, false, true), raw("b.exe", "B", f32::NAN, false, true), raw("c.exe", "C", -1.0, false, true)]);
        assert_eq!((rows[0].volume, rows[1].volume, rows[2].volume), (1.0, 0.0, 0.0));
    }

    #[test]
    fn an_unchanged_snapshot_gives_equal_rows() {
        let mut id = 0;
        let snap = || vec![raw("a.exe", "Arc", 0.5, false, true), raw("b.exe", "B", 0.2, true, false)];
        let first = reduce(&[], &mut id, snap());
        let second = reduce(&first, &mut id, snap());
        assert_eq!(first, second, "so the UI can skip the redraw");
    }

    #[test]
    fn ids_are_never_reused() {
        let mut id = 0;
        let first = reduce(&[], &mut id, vec![raw("a.exe", "A", 1.0, false, true)]);
        let gone = reduce(&first, &mut id, vec![]);
        let back = reduce(&gone, &mut id, vec![raw("a.exe", "A", 1.0, false, true)]);
        assert_ne!(back[0].id, first[0].id);
    }
}
