//! Per-app volume mixer in the Kontrol tab (v0.2, track H).
//!
//! The chip "Mixer per aplikasi" swaps the tiles for a list of the apps that have an audio
//! session on the default output: icon, name, a slider and a mute button each (sessions of one
//! process are one row). Core Audio is only touched while that view is open on a visible tab:
//! the service (`service.rs`) sleeps on its inbox and the tab's 2 s tick only asks it to refresh
//! while the view is open; a while after the tab is hidden it lets go of Core Audio.
//!
//! * `model.rs`   - raw sessions to rows (add, remove, rename, dedupe by process); pure, tested
//! * `service.rs` - the command-driven loop and its [`service::Backend`] seam; tested with a fake
//! * `wasapi.rs`  - the real backend (`IAudioSessionManager2`, names and icons from the exe)

pub(crate) mod model;
mod service;
mod wasapi;

use std::sync::LazyLock;

use model::{RawSession, Row};
use service::Cmd;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

use crate::app::{App, Redraw, Ui};
use crate::features::media::inbox::Inbox;
use crate::features::volume_osd::Audio;
use crate::i18n::tr;
use crate::registry::*;
use crate::services::ServiceCtx;
use crate::ui::*;

pub(crate) static INBOX: LazyLock<Inbox<Cmd>> = LazyLock::new(Inbox::new);

/// Rows shown at most (the panel body is 360 DIP high).
const MAX_ROWS: usize = 6;

#[derive(Default)]
pub struct Mixer {
    /// The mixer view is shown instead of the tiles.
    pub open: bool,
    rows: Vec<Row>,
    next_id: u32,
    /// A snapshot arrived since the view was opened.
    loaded: bool,
    /// The slider being dragged: (row id, value).
    drag: Option<(u32, f32)>,
    /// Invented rows for screenshots (`control.debug_mixer`, debug only): never refreshed from, and
    /// never written to, the real audio sessions.
    sample: bool,
}

pub fn register(r: &mut Registry) {
    r.add_state(Mixer::default());
    r.add_service(ServiceSpec { name: "mixer", enabled: |c| c.control.enabled && c.power.mixer, run });
}

fn run(ctx: ServiceCtx) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    service::serve(&ctx, &INBOX, wasapi::Wasapi::open, |raw| Ui::post(move |app| on_sessions(app, raw)));
    unsafe { CoUninitialize() };
}

/// The Kontrol tab's 2 s tick: refresh only while the mixer view is open (and it shows the real
/// sessions).
pub fn tick(app: &mut App) {
    let m = app.state::<Mixer>();
    service::tick(m.open && !m.sample, &INBOX);
}

/// The Kontrol tab was hidden: back to the tiles, drop what was held (icons, rows). Core Audio
/// itself is let go of by the control feature's release timer ([`release`]).
pub fn on_hidden(app: &mut App) {
    let fresh = after_hidden(app.state::<Mixer>());
    *app.state_mut::<Mixer>() = fresh;
}

/// Everything is dropped but the id counter: ids are never reused, so an action id from a stale
/// frame can never hit a different app.
fn after_hidden(m: &Mixer) -> Mixer {
    Mixer { next_id: m.next_id, ..Mixer::default() }
}

/// Let go of Core Audio (the tab has been hidden for a while).
pub fn release() {
    INBOX.push(Cmd::Release);
}

fn on_sessions(app: &mut App, raw: Vec<RawSession>) {
    let m = app.state_mut::<Mixer>();
    if !m.open || m.sample {
        return; // a late snapshot for a view that is closed again (or one showing invented rows)
    }
    let rows = model::reduce(&m.rows, &mut m.next_id, raw);
    let changed = rows != m.rows || !m.loaded;
    m.rows = rows;
    m.loaded = true;
    if changed {
        app.request_redraw(Redraw::Tab("control"));
    }
}

/// Actions `control.mixer`, `control.mixer_close`, `control.mixvol:<id>`, `control.mixmute:<id>`.
pub fn on_action(app: &mut App, ev: &ActionEvent) {
    let id = ev.arg().and_then(|a| a.parse::<u32>().ok());
    let m = app.state_mut::<Mixer>();
    match (ev.name(), ev.kind) {
        ("mixer", ActionKind::Click) if app_enabled() => {
            m.open = !m.open;
            m.loaded = false;
            m.drag = None;
            if m.open {
                INBOX.push(Cmd::Refresh);
            } else {
                m.rows.clear();
            }
        }
        ("mixer_close", ActionKind::Click) => {
            m.open = false;
            m.rows.clear();
            m.drag = None;
        }
        ("mixvol", ActionKind::Slide { value, done }) => {
            let sample = m.sample;
            let Some(row) = id.and_then(|id| m.rows.iter_mut().find(|r| r.id == id)) else { return };
            let value = value.clamp(0.0, 1.0);
            row.volume = value;
            let key = row.key.clone();
            // Raising a muted app's level also unmutes it, like the master slider.
            if row.muted && value > 0.005 {
                row.muted = false;
                if !sample {
                    INBOX.push(Cmd::SetMute(key.clone(), false));
                }
            }
            if !sample {
                INBOX.push(Cmd::SetVolume(key, value));
            }
            m.drag = (!done).then_some((row.id, value));
            if done && !sample {
                INBOX.push(Cmd::Refresh);
            }
        }
        ("mixmute", ActionKind::Click) => {
            let sample = m.sample;
            let Some(row) = id.and_then(|id| m.rows.iter_mut().find(|r| r.id == id)) else { return };
            row.muted = !row.muted;
            if !sample {
                INBOX.push(Cmd::SetMute(row.key.clone(), row.muted));
            }
        }
        // Debug only (the name starts with "debug_"): show invented apps, touch no real session.
        ("debug_mixer", ActionKind::Click) => {
            m.open = true;
            m.loaded = true;
            m.sample = true;
            m.drag = None;
            m.rows = sample_rows(&mut m.next_id);
        }
        _ => return,
    }
    app.request_redraw(Redraw::Tab("control"));
}

/// Invented apps with drawn icons (the same tiles the launcher's sample list uses), different
/// levels and one muted: for screenshots that must not show what runs on this PC.
fn sample_rows(next_id: &mut u32) -> Vec<Row> {
    let recs = crate::helper::sample::recs();
    let want: [(&str, f32, bool); 5] = [("Gema Musik", 0.82, false), ("Fajar Browser", 0.55, false), ("Jala Chat", 0.30, false), ("Bintang Player", 1.0, false), ("Kopi Timer", 0.65, true)];
    let mut rows: Vec<Row> = vec![];
    for (name, volume, muted) in want {
        let Some(rec) = recs.iter().find(|r| r.name == name) else { continue };
        *next_id += 1;
        let icon = rec.icon.as_ref().map(|i| Image::new(i.w as u32, i.h as u32, i.bgra.clone()));
        rows.push(Row { id: *next_id, key: format!("sample-{}.exe", rec.detail.trim_end_matches(".exe")), name: name.to_string(), system: false, volume, muted, active: true, icon });
    }
    *next_id += 1;
    rows.push(Row { id: *next_id, key: "sample-system".into(), name: String::new(), system: true, volume: 0.9, muted: false, active: false, icon: None });
    rows
}

/// The chip is only offered when the feature is on (the service runs then).
fn app_enabled() -> bool {
    crate::app::current_config().power.mixer
}

/// The chip row entry that opens the mixer.
pub fn chip(app: &App) -> Option<Node> {
    app.config().power.mixer.then(|| capsule(icons::SLIDERS, tr("Mixer per aplikasi", "App mixer"), "control.mixer"))
}

fn row_name(r: &Row) -> String {
    if r.system {
        tr("Suara sistem", "System sounds").to_string()
    } else if r.name.is_empty() {
        tr("Aplikasi", "App").to_string()
    } else {
        r.name.clone()
    }
}

/// Longest app name drawn (the column is 150 DIP wide; the text ellipsises there). Cutting the
/// string first keeps a pathological name from being measured at full length every frame.
const NAME_CHARS: usize = 60;

fn mixer_row(m: &Mixer, r: &Row, i: usize, accent: Rgba) -> Node {
    let v = m.drag.filter(|(id, _)| *id == r.id).map_or(r.volume, |(_, v)| v);
    let shown = if r.muted { 0.0 } else { v };
    let lead = match &r.icon {
        Some(img) => image(img, 6.0).size(24.0, 24.0),
        None => stack([icon(if r.system { icons::SPEAKER } else { icons::WINDOW }).color(color::TEXT2)]).size(24.0, 24.0),
    };
    let name = row_name(r);
    let name = if name.chars().count() > NAME_CHARS { name.chars().take(NAME_CHARS).collect::<String>() } else { name };
    let glyph = if r.muted || v <= 0.005 { icons::MUTE } else { icons::VOLUME };
    let mute_name = if r.muted { format!("{} {name}", tr("Bunyikan", "Unmute")) } else { format!("{} {name}", tr("Bisukan", "Mute")) };
    // A compact slider (thin track, accent fill, knob): the fill reads against the track at any
    // level. A muted app's is grey.
    let fill = if r.muted { color::TEXT3 } else { accent };
    row([
        lead,
        // One line, an ellipsis where it does not fit.
        text(name).semibold().font_size(12.5).w(150.0).lines(1),
        slider(shown).h(22.0).color(fill).grow(1.0).action(format!("control.mixvol:{}", r.id)),
        text(format!("{}", (shown * 100.0).round() as i32)).font_size(12.0).tertiary().w(28.0).text_right(),
        icon_button(glyph, format!("control.mixmute:{}", r.id)).name(mute_name),
    ])
    .gap(10.0)
    .h(38.0)
    .fill_w()
    .pad_xy(10.0, 0.0)
    .bg(color::FILL)
    .radius(19.0)
    .key(format!("mix.row:{}", r.id))
    .enter(Anim::pop().ms(240.0).delay(35.0 * i as f32))
}

/// The mixer view: a header with the way back, then one row per app.
pub fn view(app: &App) -> Node {
    let m = app.state::<Mixer>();
    let device = app.state::<Audio>().device.clone();
    let head = row([capsule(icons::CHEVRON_LEFT, tr("Kontrol", "Controls"), "control.mixer_close"), label(tr("Mixer per aplikasi", "App mixer")), spacer(), caption(device).max_w(260.0)]).gap(10.0).fill_w();
    let body: Node = if !m.loaded {
        caption("…").pad_xy(0.0, 18.0)
    } else if m.rows.is_empty() {
        col([icon(icons::VOLUME).font_size(22.0).color(color::TEXT2), caption(tr("Belum ada aplikasi yang sedang memakai suara", "No app is using sound right now"))]).align(Align::Center).gap(8.0).fill_w().pad_xy(0.0, 26.0)
    } else {
        let accent = app.base_accent();
        let mut rows: Vec<Node> = m.rows.iter().take(MAX_ROWS).enumerate().map(|(i, r)| mixer_row(m, r, i, accent)).collect();
        if m.rows.len() > MAX_ROWS {
            rows.push(caption(format!("+{} {}", m.rows.len() - MAX_ROWS, tr("aplikasi lain", "more apps"))).pad4(10.0, 0.0, 0.0, 0.0));
        }
        col(rows).gap(5.0).fill_w()
    };
    col([head, body]).gap(10.0).fill_w()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(n: u32) -> Vec<Row> {
        (1..=n).map(|id| Row { id, key: format!("k{id}.exe"), name: format!("App {id}"), system: false, volume: 0.5, muted: false, active: true, icon: None }).collect()
    }

    fn collect_keys(n: &Node, out: &mut Vec<String>) {
        if let Some(k) = n.keyed.as_deref() {
            out.push(k.key.clone());
        }
        if let Kind::Row(c) | Kind::Col(c) | Kind::Stack(c) = &n.kind {
            c.iter().for_each(|c| collect_keys(c, out));
        }
    }

    fn actions(n: &Node, out: &mut Vec<String>) {
        if let Some(a) = &n.action {
            out.push(a.clone());
        }
        if let Kind::Row(c) | Kind::Col(c) | Kind::Stack(c) = &n.kind {
            c.iter().for_each(|c| actions(c, out));
        }
    }

    #[test]
    fn rows_have_stable_keys_and_per_app_actions() {
        let m = Mixer { open: true, rows: rows(3), loaded: true, ..Mixer::default() };
        let node = row([mixer_row(&m, &m.rows[1], 1, color::BLUE)]);
        let (mut keys, mut acts) = (vec![], vec![]);
        collect_keys(&node, &mut keys);
        actions(&node, &mut acts);
        assert_eq!(keys, ["mix.row:2"]);
        assert_eq!(acts, ["control.mixvol:2", "control.mixmute:2"]);
    }

    #[test]
    fn a_muted_row_shows_an_empty_slider_and_the_dragged_value_wins() {
        let mut m = Mixer { open: true, rows: rows(1), loaded: true, ..Mixer::default() };
        m.rows[0].muted = true;
        let find_slider = |n: &Node| -> f32 {
            fn go(n: &Node) -> Option<f32> {
                match &n.kind {
                    Kind::Slider { value, .. } => Some(*value),
                    Kind::Row(c) | Kind::Col(c) | Kind::Stack(c) => c.iter().find_map(go),
                    _ => None,
                }
            }
            go(n).unwrap()
        };
        let acc = color::BLUE;
        assert_eq!(find_slider(&mixer_row(&m, &m.rows[0], 0, acc)), 0.0, "muted reads as zero");
        m.rows[0].muted = false;
        assert_eq!(find_slider(&mixer_row(&m, &m.rows[0], 0, acc)), 0.5);
        m.drag = Some((1, 0.8));
        assert_eq!(find_slider(&mixer_row(&m, &m.rows[0], 0, acc)), 0.8, "while dragging the slider shows the drag value");
        m.drag = Some((2, 0.1));
        assert_eq!(find_slider(&mixer_row(&m, &m.rows[0], 0, acc)), 0.5, "another row's drag does not matter");
    }

    #[test]
    fn hiding_the_tab_drops_everything_but_the_id_counter() {
        let m = Mixer { open: true, rows: rows(2), next_id: 2, loaded: true, drag: Some((1, 0.3)), sample: true };
        let h = after_hidden(&m);
        assert!(!h.open && h.rows.is_empty() && !h.loaded && h.drag.is_none() && !h.sample, "back to the tiles, rows and icons freed");
        assert_eq!(h.next_id, 2, "ids are never reused");
    }

    /// The compact slider is what the mixer draws, and a row's name column is one line wide.
    #[test]
    fn mixer_rows_use_the_compact_slider_and_the_sample_rows_are_invented() {
        let m = Mixer { open: true, rows: rows(1), loaded: true, ..Mixer::default() };
        fn heights(n: &Node, out: &mut Vec<Len>) {
            if matches!(n.kind, Kind::Slider { .. }) {
                out.push(n.h);
            }
            if let Kind::Row(c) | Kind::Col(c) | Kind::Stack(c) = &n.kind {
                c.iter().for_each(|c| heights(c, out));
            }
        }
        let mut hs = vec![];
        heights(&mixer_row(&m, &m.rows[0], 0, color::BLUE), &mut hs);
        assert_eq!(hs, vec![Len::Px(22.0)], "within COMPACT_SLIDER_H");
        let mut id = 0;
        let s = sample_rows(&mut id);
        assert_eq!(s.len(), 6);
        assert!(s.iter().filter(|r| !r.system).all(|r| r.icon.is_some() && r.key.starts_with("sample-")), "invented apps with drawn icons");
        assert!(s.iter().any(|r| r.muted) && s.iter().any(|r| r.system));
        let ids: std::collections::HashSet<u32> = s.iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), s.len(), "unique ids");
    }
}
