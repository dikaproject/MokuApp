//! The state machine behind the download pill: pure logic, no files, no clock of its own.
//!
//! Browsers write a download to a temporary name and rename it when it is complete:
//!
//! | browser | while downloading | when done |
//! |---|---|---|
//! | Chrome, Edge, Brave, Opera | `Unconfirmed 123456.crdownload`, then `name.ext.crdownload` | renamed to `name.ext` |
//! | Firefox | `name.ext.part` | renamed to `name.ext` |
//! | Internet Explorer, some tools | `name.ext.partial` | renamed to `name.ext` |
//!
//! The watcher feeds file-system events in; [`Tracker`] says which downloads are running (name,
//! size, speed) and when one finished. A cancelled download deletes its temp file: the pill just
//! goes away. Only names and sizes are ever looked at, never file contents.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// A download younger than this gets no pill (small files finish before anyone could read it).
pub const SHOW_AFTER: Duration = Duration::from_millis(800);
/// A temp file that has not grown for this long is paused or abandoned: no pill (it comes back
/// when it grows again).
pub const STALL_AFTER: Duration = Duration::from_secs(30);
/// Speed is averaged over this long.
const SPEED_WINDOW: Duration = Duration::from_secs(4);
/// A browser that deletes the temp file and creates the final one (instead of renaming) is
/// still a finished download when the final name shows up within this long.
const DELETE_THEN_CREATE: Duration = Duration::from_millis(2000);
/// Downloads tracked at once.
const MAX_ACTIVE: usize = 16;

const TEMP_EXTS: [&str; 3] = [".crdownload", ".part", ".partial"];

/// Does `name` end in a browser's temporary-download extension (and have something before it)?
pub fn is_temp(name: &str) -> bool {
    temp_ext_len(name).is_some_and(|n| name.len() > n)
}

fn temp_ext_len(name: &str) -> Option<usize> {
    TEMP_EXTS.iter().find(|e| name.len() >= e.len() && name.is_char_boundary(name.len() - e.len()) && name[name.len() - e.len()..].eq_ignore_ascii_case(e)).map(|e| e.len())
}

/// The name the file will have when done (`report.pdf.crdownload` → `report.pdf`). `None` while a
/// browser has not chosen one yet (Chrome's `Unconfirmed 123456`).
pub fn display_name(temp: &str) -> Option<String> {
    let stem = &temp[..temp.len() - temp_ext_len(temp)?];
    let stem = stem.trim();
    let unconfirmed = stem.len() > 12 && stem.get(..12).is_some_and(|p| p.eq_ignore_ascii_case("unconfirmed ")) && stem[12..].bytes().all(|b| b.is_ascii_digit());
    (!stem.is_empty() && !unconfirmed).then(|| stem.to_string())
}

/// A plain file name: no folders, nothing the shell would read as something else.
pub fn is_plain_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 260 && !name.contains(['/', '\\', '\0', ':']) && name != "." && name != ".."
}

/// Bytes per second over the last few seconds.
#[derive(Default, Debug)]
pub struct SpeedMeter {
    samples: VecDeque<(Instant, u64)>,
}

impl SpeedMeter {
    pub fn push(&mut self, now: Instant, bytes: u64) {
        self.samples.push_back((now, bytes));
        while self.samples.len() > 2 && self.samples.front().is_some_and(|(t, _)| now.saturating_duration_since(*t) > SPEED_WINDOW) {
            self.samples.pop_front();
        }
        while self.samples.len() > 64 {
            self.samples.pop_front();
        }
    }

    /// `None` until there are two readings at least half a second apart.
    pub fn bytes_per_sec(&self, now: Instant) -> Option<f64> {
        let (t0, b0) = *self.samples.iter().find(|(t, _)| now.saturating_duration_since(*t) <= SPEED_WINDOW)?;
        // The newest reading counts as "now" only if it is recent; a quiet file keeps its last
        // reading and the span up to `now` makes the speed fall to zero.
        let (t1, b1) = *self.samples.back()?;
        let end = now.max(t1);
        let dt = end.saturating_duration_since(t0).as_secs_f64();
        (dt >= 0.5).then(|| b1.saturating_sub(b0) as f64 / dt)
    }
}

/// What the watcher saw, after the glue looked up sizes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ev {
    /// A temp file exists or changed; `size` is its length now.
    Temp { name: String, size: u64 },
    /// A file with an ordinary name appeared or changed.
    File { name: String },
    Renamed { from: String, to: String },
    /// A file was deleted (or a temp file vanished when looked at).
    Gone { name: String },
}

/// A download that completed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finished {
    /// The final file name (no folder).
    pub name: String,
}

#[derive(Debug)]
struct Dl {
    /// Current temp file name.
    temp: String,
    size: u64,
    first: Instant,
    last_change: Instant,
    meter: SpeedMeter,
}

/// One running download, as the pill shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    /// `None` while the browser has not chosen a name.
    pub name: Option<String>,
    pub size: u64,
    pub speed: Option<f64>,
}

#[derive(Default, Debug)]
pub struct Tracker {
    active: Vec<Dl>,
    /// Temp files that were deleted a moment ago: `(final name, when)`.
    gone: Vec<(String, Instant)>,
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

impl Tracker {
    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }

    /// Temp file names to look at again (their size, once a second).
    pub fn polls(&self) -> Vec<String> {
        self.active.iter().map(|d| d.temp.clone()).collect()
    }

    pub fn event(&mut self, ev: Ev, now: Instant) -> Option<Finished> {
        self.gone.retain(|(_, t)| now.saturating_duration_since(*t) <= DELETE_THEN_CREATE);
        match ev {
            Ev::Temp { name, size } => {
                if !is_plain_name(&name) || !is_temp(&name) {
                    return None;
                }
                self.touch(&name, size, now);
                None
            }
            Ev::File { name } => {
                let i = self.gone.iter().position(|(n, _)| same(n, &name))?;
                self.gone.remove(i);
                Some(Finished { name })
            }
            Ev::Renamed { from, to } => {
                if !is_plain_name(&to) {
                    return None;
                }
                let tracked = self.active.iter().position(|d| same(&d.temp, &from));
                match (is_temp(&from), is_temp(&to), tracked) {
                    // `Unconfirmed 1.crdownload` → `name.zip.crdownload`: the same download.
                    (true, true, Some(i)) => {
                        self.active[i].temp = to;
                        None
                    }
                    (true, false, tracked) => {
                        if let Some(i) = tracked {
                            self.active.remove(i);
                        }
                        // Also a download that finished before we ever saw it grow.
                        Some(Finished { name: to })
                    }
                    (false, true, None) if is_plain_name(&from) => {
                        self.touch(&to, 0, now);
                        None
                    }
                    (true, true, None) => {
                        self.touch(&to, 0, now);
                        None
                    }
                    _ => None,
                }
            }
            Ev::Gone { name } => {
                if let Some(i) = self.active.iter().position(|d| same(&d.temp, &name)) {
                    let d = self.active.remove(i);
                    if let Some(n) = display_name(&d.temp) {
                        self.gone.push((n, now));
                    }
                }
                None
            }
        }
    }

    fn touch(&mut self, name: &str, size: u64, now: Instant) {
        let i = match self.active.iter().position(|d| same(&d.temp, name)) {
            Some(i) => i,
            None => {
                if self.active.len() >= MAX_ACTIVE {
                    return;
                }
                self.active.push(Dl { temp: name.to_string(), size, first: now, last_change: now, meter: SpeedMeter::default() });
                self.active.len() - 1
            }
        };
        let d = &mut self.active[i];
        if size != d.size {
            d.last_change = now;
            d.size = size;
        }
        d.meter.push(now, size);
    }

    /// The downloads worth a pill right now: running for a moment, and growing.
    pub fn views(&self, now: Instant) -> Vec<View> {
        self.active
            .iter()
            .filter(|d| now.saturating_duration_since(d.first) >= SHOW_AFTER && now.saturating_duration_since(d.last_change) <= STALL_AFTER)
            .map(|d| View { name: display_name(&d.temp), size: d.size, speed: d.meter.bytes_per_sec(now) })
            .collect()
    }
}

/// `840 KB`, `2,1 MB` (Indonesian decimal comma) / `2.1 MB`.
pub fn fmt_size(bytes: u64, comma: bool) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    let s = if b < K {
        format!("{bytes} B")
    } else if b < K * K {
        format!("{:.0} KB", b / K)
    } else if b < K * K * K {
        format!("{:.1} MB", b / (K * K))
    } else {
        format!("{:.2} GB", b / (K * K * K))
    };
    if comma { s.replace('.', ",") } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1024 * 1024;

    fn t(name: &str, size: u64) -> Ev {
        Ev::Temp { name: name.into(), size }
    }
    fn renamed(from: &str, to: &str) -> Ev {
        Ev::Renamed { from: from.into(), to: to.into() }
    }
    fn gone(name: &str) -> Ev {
        Ev::Gone { name: name.into() }
    }
    fn file(name: &str) -> Ev {
        Ev::File { name: name.into() }
    }
    fn secs(base: Instant, s: f64) -> Instant {
        base + Duration::from_secs_f64(s)
    }

    #[test]
    fn temp_names_of_the_three_browsers_are_recognised() {
        for n in ["a.zip.crdownload", "a.zip.part", "a.zip.partial", "A.ZIP.CRDOWNLOAD", "Unconfirmed 123456.crdownload", "x.PART"] {
            assert!(is_temp(n), "{n}");
        }
        for n in ["a.zip", "a.crdownloads", "part", ".part", ".crdownload", "notes.txt", "", "é", "a.parté"] {
            assert!(!is_temp(n), "{n}");
        }
        assert_eq!(display_name("report.pdf.crdownload").as_deref(), Some("report.pdf"));
        assert_eq!(display_name("report.pdf.part").as_deref(), Some("report.pdf"));
        assert_eq!(display_name("my.file.v2.zip.PARTIAL").as_deref(), Some("my.file.v2.zip"));
        assert_eq!(display_name("Unconfirmed 123456.crdownload"), None, "Chrome has not named it yet");
        assert_eq!(display_name("Unconfirmed notes.crdownload").as_deref(), Some("Unconfirmed notes"), "only Chrome's numeric placeholder is hidden");
        assert_eq!(display_name("plain.txt"), None);
    }

    #[test]
    fn plain_names_only() {
        assert!(is_plain_name("report (1).pdf"));
        for n in ["", "..", ".", "a/b.zip", r"a\b.zip", "C:evil", "a\0b"] {
            assert!(!is_plain_name(n), "{n:?}");
        }
        assert!(!is_plain_name(&"x".repeat(300)));
    }

    #[test]
    fn chrome_flow_unconfirmed_then_named_then_renamed_to_final() {
        let t0 = Instant::now();
        let mut tr = Tracker::default();
        assert_eq!(tr.event(t("Unconfirmed 874531.crdownload", 0), t0), None);
        // Younger than SHOW_AFTER: no pill yet.
        assert!(tr.views(secs(t0, 0.3)).is_empty());
        assert_eq!(tr.event(t("Unconfirmed 874531.crdownload", MB), secs(t0, 1.0)), None);
        let v = tr.views(secs(t0, 1.0));
        assert_eq!(v.len(), 1);
        assert_eq!((v[0].name.clone(), v[0].size), (None, MB), "name unknown yet");
        // Chrome picks the name: the same download continues under its new temp name.
        assert_eq!(tr.event(renamed("Unconfirmed 874531.crdownload", "setup.zip.crdownload"), secs(t0, 1.5)), None);
        assert_eq!(tr.polls(), ["setup.zip.crdownload"]);
        tr.event(t("setup.zip.crdownload", 3 * MB), secs(t0, 2.0));
        let v = tr.views(secs(t0, 2.0));
        assert_eq!((v[0].name.as_deref(), v[0].size), (Some("setup.zip"), 3 * MB));
        // Done: the temp name becomes the final name.
        assert_eq!(tr.event(renamed("setup.zip.crdownload", "setup.zip"), secs(t0, 3.0)), Some(Finished { name: "setup.zip".into() }));
        assert!(tr.is_empty() && tr.views(secs(t0, 3.0)).is_empty(), "the pill is gone");
        // The browser's own follow-up events for the final file are not another download.
        assert_eq!(tr.event(file("setup.zip"), secs(t0, 3.1)), None);
    }

    #[test]
    fn firefox_flow_part_file_renamed_when_done() {
        let t0 = Instant::now();
        let mut tr = Tracker::default();
        tr.event(t("video.mp4.part", 100), t0);
        tr.event(t("video.mp4.part", 5 * MB), secs(t0, 1.0));
        assert_eq!(tr.views(secs(t0, 1.0))[0].name.as_deref(), Some("video.mp4"));
        assert_eq!(tr.event(renamed("video.mp4.part", "video.mp4"), secs(t0, 2.0)), Some(Finished { name: "video.mp4".into() }));
        assert!(tr.is_empty());
    }

    #[test]
    fn partial_flow_and_a_different_final_name() {
        let t0 = Instant::now();
        let mut tr = Tracker::default();
        tr.event(t("data.csv.partial", 10), t0);
        // The browser avoided a clash: the final name differs from the temp name's stem.
        assert_eq!(tr.event(renamed("data.csv.partial", "data (1).csv"), secs(t0, 2.0)), Some(Finished { name: "data (1).csv".into() }));
        assert!(tr.is_empty());
    }

    #[test]
    fn a_cancelled_download_clears_the_pill_and_never_finishes() {
        let t0 = Instant::now();
        let mut tr = Tracker::default();
        tr.event(t("big.iso.crdownload", MB), t0);
        tr.event(t("big.iso.crdownload", 2 * MB), secs(t0, 1.0));
        assert_eq!(tr.views(secs(t0, 1.0)).len(), 1);
        // The user cancels: the temp file is deleted.
        assert_eq!(tr.event(gone("big.iso.crdownload"), secs(t0, 1.5)), None);
        assert!(tr.is_empty());
        assert!(tr.views(secs(t0, 1.6)).is_empty());
        assert!(tr.polls().is_empty());
        // Nothing with that name shows up later as a "finished" download.
        assert_eq!(tr.event(file("big.iso"), secs(t0, 10.0)), None);
        // Deleting something we never tracked is nothing.
        assert_eq!(tr.event(gone("other.zip.crdownload"), secs(t0, 11.0)), None);
        assert_eq!(tr.event(gone("notes.txt"), secs(t0, 11.0)), None);
    }

    #[test]
    fn delete_then_create_within_a_moment_counts_as_finished() {
        // Some tools delete the temp file and write the final one instead of renaming.
        let t0 = Instant::now();
        let mut tr = Tracker::default();
        tr.event(t("tool.exe.part", MB), t0);
        tr.event(t("tool.exe.part", 2 * MB), secs(t0, 1.0));
        tr.event(gone("tool.exe.part"), secs(t0, 2.0));
        assert_eq!(tr.event(file("TOOL.exe"), secs(t0, 2.4)), Some(Finished { name: "TOOL.exe".into() }), "case-insensitive");
        // Only once.
        assert_eq!(tr.event(file("tool.exe"), secs(t0, 2.5)), None);
        // Too late: a cancel, then an unrelated file of the same name much later.
        let mut tr = Tracker::default();
        tr.event(t("tool.exe.part", MB), t0);
        tr.event(gone("tool.exe.part"), secs(t0, 2.0));
        assert_eq!(tr.event(file("tool.exe"), secs(t0, 6.0)), None);
    }

    #[test]
    fn a_download_we_never_saw_growing_still_finishes() {
        let mut tr = Tracker::default();
        assert_eq!(tr.event(renamed("tiny.txt.crdownload", "tiny.txt"), Instant::now()), Some(Finished { name: "tiny.txt".into() }));
        // A rename into a temp name starts tracking.
        let t0 = Instant::now();
        assert_eq!(tr.event(renamed("tiny2.txt", "tiny2.txt.crdownload"), t0), None);
        assert_eq!(tr.polls(), ["tiny2.txt.crdownload"]);
    }

    #[test]
    fn ordinary_files_are_ignored() {
        let t0 = Instant::now();
        let mut tr = Tracker::default();
        assert_eq!(tr.event(t("notes.txt", 5), t0), None, "not a temp name");
        assert_eq!(tr.event(file("notes.txt"), t0), None);
        assert_eq!(tr.event(renamed("a.txt", "b.txt"), t0), None);
        assert_eq!(tr.event(renamed("a.tmp", "a.dat"), t0), None);
        assert_eq!(tr.event(t("../evil.zip.part", 5), t0), None, "no folders in names");
        assert_eq!(tr.event(renamed("x.part", "..\\y"), t0), None);
        assert!(tr.is_empty());
    }

    #[test]
    fn a_paused_download_loses_its_pill_and_gets_it_back_when_it_grows() {
        let t0 = Instant::now();
        let mut tr = Tracker::default();
        tr.event(t("a.zip.crdownload", MB), t0);
        tr.event(t("a.zip.crdownload", 2 * MB), secs(t0, 1.0));
        assert_eq!(tr.views(secs(t0, 20.0)).len(), 1);
        // Polled every second without growth for over STALL_AFTER.
        for s in 2..=40 {
            tr.event(t("a.zip.crdownload", 2 * MB), secs(t0, s as f64));
        }
        assert!(tr.views(secs(t0, 40.0)).is_empty(), "stalled");
        assert!(!tr.is_empty(), "but still tracked");
        tr.event(t("a.zip.crdownload", 3 * MB), secs(t0, 41.0));
        assert_eq!(tr.views(secs(t0, 41.0)).len(), 1);
    }

    #[test]
    fn several_downloads_are_tracked_separately_and_bounded() {
        let t0 = Instant::now();
        let mut tr = Tracker::default();
        tr.event(t("a.zip.crdownload", 1), t0);
        tr.event(t("b.zip.part", 1), t0);
        assert_eq!(tr.views(secs(t0, 1.0)).len(), 2);
        tr.event(renamed("a.zip.crdownload", "a.zip"), secs(t0, 1.0));
        assert_eq!(tr.polls(), ["b.zip.part"]);
        for i in 0..100 {
            tr.event(t(&format!("f{i}.part"), 1), t0);
        }
        assert!(tr.polls().len() <= MAX_ACTIVE);
    }

    #[test]
    fn speed_is_bytes_over_the_last_seconds() {
        let t0 = Instant::now();
        let mut m = SpeedMeter::default();
        assert_eq!(m.bytes_per_sec(t0), None, "no readings");
        m.push(t0, 0);
        assert_eq!(m.bytes_per_sec(secs(t0, 0.1)), None, "too soon to say");
        m.push(secs(t0, 1.0), MB);
        m.push(secs(t0, 2.0), 2 * MB);
        let s = m.bytes_per_sec(secs(t0, 2.0)).unwrap();
        assert!((s - MB as f64).abs() < 1.0, "1 MiB/s, got {s}");
        // Speeding up: the window forgets the slow start.
        m.push(secs(t0, 3.0), 4 * MB);
        m.push(secs(t0, 4.0), 7 * MB);
        m.push(secs(t0, 5.0), 11 * MB);
        let s = m.bytes_per_sec(secs(t0, 5.0)).unwrap();
        assert!(s > 2.0 * MB as f64 && s < 4.5 * MB as f64, "recent speed, got {s}");
        // A stall: the size stops growing, the speed decays to zero.
        for k in 6..=12 {
            m.push(secs(t0, k as f64), 11 * MB);
        }
        assert_eq!(m.bytes_per_sec(secs(t0, 12.0)), Some(0.0));
        // Shrinking files never give a negative speed.
        let mut m = SpeedMeter::default();
        m.push(t0, 10 * MB);
        m.push(secs(t0, 1.0), MB);
        assert_eq!(m.bytes_per_sec(secs(t0, 1.0)), Some(0.0));
    }

    #[test]
    fn the_tracker_reports_speed_in_its_views() {
        let t0 = Instant::now();
        let mut tr = Tracker::default();
        for s in 0..=5 {
            tr.event(t("a.zip.crdownload", s * 2 * MB), secs(t0, s as f64));
        }
        let v = tr.views(secs(t0, 5.0));
        let sp = v[0].speed.unwrap();
        assert!((sp - 2.0 * MB as f64).abs() < MB as f64 * 0.01, "2 MiB/s, got {sp}");
        assert_eq!(v[0].size, 10 * MB);
    }

    #[test]
    fn sizes_are_formatted_for_both_languages() {
        assert_eq!(fmt_size(0, true), "0 B");
        assert_eq!(fmt_size(840 * 1024, true), "840 KB");
        assert_eq!(fmt_size(2_200_000, true), "2,1 MB");
        assert_eq!(fmt_size(2_200_000, false), "2.1 MB");
        assert_eq!(fmt_size(3 * 1024 * MB, false), "3.00 GB");
    }
}
