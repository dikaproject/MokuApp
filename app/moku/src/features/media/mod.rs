//! Feature `media` — Task 2 (track A).
//!
//! Music from the Windows media session (GSMTC): the music pill (art, title, artist, bars that
//! move only while playing; lyric mode with `media.lyrics_in_pill`), the Musik panel (art,
//! lyrics, seek bar, prev / play / next, output device) and the album colour as accent
//! (`appearance.follow_album_color`).
//!
//! * `session.rs` — the GSMTC service thread (events → [`model::Update`]s).
//! * `model.rs`   — the pure state reducer (tested).
//! * `inbox.rs`   — event-driven command queue shared by the track-A services.

pub mod inbox;
pub mod model;
mod session;

use std::time::{Duration, Instant};

use model::{Media, Update, fmt_time};
use session::Cmd;

use crate::activity::{Activity, priority};
use crate::app::{App, Redraw, TimerId};
use crate::features::{lyrics, volume_osd};
use crate::i18n::{tr, tr_s};
use crate::registry::*;
use crate::ui::*;

/// Activity id of the music pill.
pub const PILL_ID: &str = "media.music";

pub struct MediaState {
    pub m: Media,
    /// Seek bar position while it is being dragged.
    scrub: Option<f32>,
    /// Lyric lines (vs. album info) in the panel — the "Lirik" chip.
    show_lyrics: bool,
    /// Pending "open the panel" after a single click on the pill (cancelled by a double-click).
    click_timer: Option<TimerId>,
}

impl Default for MediaState {
    fn default() -> Self {
        MediaState { m: Media::default(), scrub: None, show_lyrics: true, click_timer: None }
    }
}

pub fn register(r: &mut Registry) {
    r.add_state(MediaState::default());
    r.add_tab(TabSpec {
        id: "media",
        order: order::MEDIA,
        icon: icons::MUSIC,
        label: ("Musik", "Music"),
        enabled: |c| c.media.enabled,
        build: panel,
        // Timeline at 1 s only while the Musik tab is visible.
        tick: Some((Duration::from_secs(1), |app| {
            session::INBOX.push(Cmd::Timeline);
            app.request_redraw(Redraw::Tab("media"));
        })),
    });
    r.on_action("media", on_action);
    r.on_config(|app| {
        if !app.config().media.enabled && app.state::<MediaState>().m.app.is_some() {
            apply(app, Update::SessionGone);
        }
        sync(app);
    });
    r.add_service(ServiceSpec { name: "media", enabled: |c| c.media.enabled, run: session::run });
}

/// Choose the session to show again (`island.media_ignore_apps` changed).
pub fn repick() {
    session::INBOX.push(Cmd::Session);
}

/// Apply an update from the session service (UI thread).
pub fn apply(app: &mut App, u: Update) {
    // A late post from a service that was just switched off must not bring the pill back.
    if !app.config().media.enabled && u != Update::SessionGone {
        return;
    }
    let changed = app.state_mut::<MediaState>().m.apply(u);
    sync(app);
    if changed.track {
        lyrics::track_changed(app);
    } else {
        lyrics::reschedule(app);
    }
}

/// Pill and accent from the state.
fn sync(app: &mut App) {
    let m = &app.state::<MediaState>().m;
    let has = m.has_music();
    let accent = if app.config().appearance.follow_album_color { m.accent() } else { None };
    if has {
        if !app.has_activity(PILL_ID) {
            app.set_activity(Activity::persistent(PILL_ID, priority::MUSIC, music_pill).tab("media").bubble(|app| art_node(app, 24.0, 7.0)));
        }
    } else {
        app.clear_activity(PILL_ID);
    }
    app.set_accent_override(accent);
    app.request_redraw(Redraw::All);
}

fn on_action(app: &mut App, ev: &ActionEvent) {
    match (ev.name(), ev.kind) {
        ("pill", ActionKind::Click) => {
            // Wait for a possible double-click before opening the panel.
            if app.state::<MediaState>().click_timer.is_none() {
                let ms = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() }.clamp(150, 600);
                let id = app.after(Duration::from_millis(ms as u64 + 20), |app| {
                    app.state_mut::<MediaState>().click_timer = None;
                    // Like any click on the pill: the tab the user last chose (Musik when
                    // nothing was chosen yet), not always Musik.
                    app.open_panel(None);
                });
                app.state_mut::<MediaState>().click_timer = Some(id);
            }
        }
        ("pill", ActionKind::DoubleClick) => {
            if let Some(t) = app.state_mut::<MediaState>().click_timer.take() {
                app.cancel_timer(t);
            }
            toggle(app);
        }
        ("pill" | "mute", ActionKind::Scroll(n)) => volume_osd::nudge(n * 0.02),
        ("mute", ActionKind::Click) => volume_osd::toggle_mute(),
        ("toggle", ActionKind::Click) => toggle(app),
        ("next", ActionKind::Click) => session::INBOX.push(Cmd::Next),
        ("prev", ActionKind::Click) => session::INBOX.push(Cmd::Prev),
        ("seek", ActionKind::Slide { value, done }) => {
            if done {
                app.state_mut::<MediaState>().scrub = None;
                let dur = app.state::<MediaState>().m.timeline.dur_ms;
                seek(app, (value.clamp(0.0, 1.0) as f64 * dur as f64) as u64);
            } else {
                app.state_mut::<MediaState>().scrub = Some(value);
            }
            app.request_redraw(Redraw::Tab("media"));
        }
        ("seek_ms", ActionKind::Click) => {
            if let Some(ms) = ev.arg().and_then(|a| a.parse().ok()) {
                seek(app, ms);
            }
        }
        // The buttons under the lyrics: "Bukan lagu ini", the timing offset, the web search.
        (n, ActionKind::Click) if n.starts_with("lyrics_") => lyrics::on_action(app, n, ev.arg()),
        ("lyrics", ActionKind::Click) => {
            let s = app.state_mut::<MediaState>();
            s.show_lyrics = !s.show_lyrics;
            app.request_redraw(Redraw::Tab("media"));
        }
        ("output", ActionKind::Click) => app.open_panel(Some("control")),
        // Debug/demo path: feed sample data through the reducer (`mokuctl debug click media.sample`).
        ("sample", ActionKind::Click) => sample(app),
        // Simulate the player going away, then let the service pick the real session again.
        ("sample_off", ActionKind::Click) => {
            apply(app, Update::SessionGone);
            session::INBOX.push(Cmd::Reset);
        }
        _ => {}
    }
}

/// Play/pause, with the state flipped right away so the button and bars react instantly.
fn toggle(app: &mut App) {
    let now = Instant::now();
    let m = &mut app.state_mut::<MediaState>().m;
    if !m.has_music() {
        return;
    }
    m.timeline.pos_ms = m.position_ms(now);
    m.timeline.at = Some(now);
    m.playing = !m.playing;
    session::INBOX.push(Cmd::Toggle);
    app.request_redraw(Redraw::All);
    lyrics::reschedule(app);
}

fn seek(app: &mut App, ms: u64) {
    let m = &mut app.state_mut::<MediaState>().m;
    if !m.controls.seek {
        return;
    }
    m.timeline.pos_ms = ms;
    m.timeline.at = Some(Instant::now());
    session::INBOX.push(Cmd::Seek(ms));
    app.request_redraw(Redraw::All);
    lyrics::reschedule(app);
}

/// "Spotify.exe" / "Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic" → "Spotify" / "ZuneMusic".
pub fn app_name(aumid: &str) -> String {
    let s = aumid.rsplit('!').next().unwrap_or(aumid);
    let s = s.rsplit(['\\', '/']).next().unwrap_or(s);
    let s = s.strip_suffix(".exe").or_else(|| s.strip_suffix(".EXE")).unwrap_or(s);
    let s = s.split('_').next().unwrap_or(s);
    let s = s.rsplit('.').next().unwrap_or(s);
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

// ---- UI ----------------------------------------------------------------------------------------

fn art_node(app: &App, size: f32, radius: f32) -> Node {
    match &app.state::<MediaState>().m.art {
        Some(img) => image(img, radius).size(size, size),
        None => stack([icon(icons::MUSIC).font_size((size * 0.42).round()).color(color::TEXT2)]).size(size, size).bg(color::FILL2).radius(radius),
    }
}

/// Bars colour: the album colour when there is one, else the mockup pink.
fn bar_color(app: &App) -> Rgba {
    app.state::<MediaState>().m.art_color.unwrap_or(color::PINK)
}

fn music_pill(app: &App) -> Node {
    let m = &app.state::<MediaState>().m;
    let Some(t) = &m.track else { return pill([icon(icons::MUSIC)]) };
    let content = match lyrics::pill_lines(app) {
        // "Lirik berjalan": the sung line (its words light up in time), the next one dimmed.
        Some((cur, next)) => {
            let mut r = row([art_node(app, 24.0, 7.0), sung_line(app, &cur, typo::BODY, 4.0, None).max_w(320.0)]);
            if !next.is_empty() {
                r = r.child(text(next).tertiary().max_w(170.0));
            }
            r
        }
        None => {
            let mut r = row([art_node(app, 24.0, 7.0), text(t.title.clone()).semibold().max_w(230.0)]);
            if !t.artist.is_empty() {
                r = r.child(text(t.artist.clone()).secondary().max_w(150.0));
            }
            r.child(gap_px(3.0)).child(bars(m.playing).color(bar_color(app)))
        }
    };
    content.h(36.0).pad4(8.0, 0.0, 12.0, 0.0).gap(8.0).action("media.pill")
}

/// The playback clock the word highlight follows: the player's last report of its position (a
/// position being scrubbed counts as paused there). It only changes when the player reports
/// something, so redraws in between leave a running sweep alone.
fn lyric_anchor(st: &MediaState, scrub: Option<u64>) -> Anchor {
    let m = &st.m;
    match (scrub, m.timeline.at) {
        (Some(p), _) => Anchor { pos_ms: p, at: Instant::now(), playing: false, rate: 1.0 },
        (None, Some(at)) => Anchor { pos_ms: m.timeline.pos_ms, at, playing: m.playing, rate: m.timeline.speed() },
        (None, None) => Anchor { pos_ms: m.timeline.pos_ms, at: Instant::now(), playing: false, rate: 1.0 },
    }
}

/// The sung line: its words light up in time with the music (`media.lyrics_word_highlight`; the
/// scene sweeps a bright copy over the dim one on the compositor, and shows it simply bright
/// when animations are reduced or off). A line with nothing to light up is plain bright text.
fn sung_line(app: &App, c: &lyrics::Current, size: f32, rise: f32, scrub: Option<u64>) -> Node {
    let plain = || text(c.text.clone()).font_size(size).semibold();
    if !app.config().media.lyrics_word_highlight || c.words.is_empty() {
        return plain();
    }
    let Kind::Text(spec) = plain().kind else { unreachable!("text() is a Text") };
    karaoke(KaraokeSpec { id: lyrics::line_id(app, c.idx), text: spec, dim: color::TEXT3, words: c.words.clone(), anchor: lyric_anchor(app.state::<MediaState>(), scrub), rise })
}

/// Three lyric lines (previous / current / next) or a status line, always three rows high so
/// the panel doesn't jump.
fn lyric_block(app: &App, pos: u64) -> Node {
    let st = app.state::<MediaState>();
    let ls = app.state::<lyrics::LyricsState>();
    let scrub = st.scrub.map(|_| pos);
    let line = |s: Option<(u64, String)>, big: bool| -> Node {
        let Some((t, s)) = s else { return text(" ").font_size(if big { 15.0 } else { typo::BODY }) };
        let s = if s.is_empty() { "♪".to_string() } else { s };
        let n = if big { text(s).font_size(15.0).semibold() } else { text(s).tertiary() };
        n.fill_w().action(format!("media.seek_ms:{t}"))
    };
    let status = |s: &str| col([text(" "), text(s.to_string()).tertiary().font_size(typo::CAPTION), text(" ")]).gap(4.0).fill_w();
    let body = if !st.show_lyrics || !app.config().media.lyrics {
        let from = st.m.app.as_deref().map(app_name).unwrap_or_default();
        status(&tr_s(format!("Diputar dari {from}"), format!("Playing from {from}")))
    } else {
        match &ls.status {
            lyrics::Status::Ready(lyrics::lrclib::Found::Synced(l)) => {
                let (p, c, n) = lyrics::around(l, pos);
                let cur = match lyrics::current(l, pos) {
                    Some(cur) if !cur.text.is_empty() => sung_line(app, &cur, 15.0, 7.0, scrub).fill_w().action(format!("media.seek_ms:{}", cur.t)),
                    _ => line(c, true),
                };
                col([line(p, false), cur, line(n, false)]).gap(4.0).fill_w()
            }
            lyrics::Status::Ready(lyrics::lrclib::Found::Instrumental) => status(tr("♪ Instrumental", "♪ Instrumental")),
            lyrics::Status::Loading => status(tr("Mencari lirik…", "Looking for lyrics…")),
            lyrics::Status::Failed => status(tr("Lirik tidak bisa dimuat", "Couldn't load lyrics")),
            _ => status(tr("Lirik tidak tersedia", "No synced lyrics")),
        }
    };
    body.pad4(0.0, 6.0, 0.0, 2.0)
}

/// `s` cut to `max` characters with an ellipsis.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out = out.trim_end().to_string();
    out.push('\u{2026}');
    out
}

/// A quiet text button of the row under the lyrics: tertiary text, the accent only while it is "on".
fn quiet(label: &str, action: &str, on: Option<Rgba>) -> Node {
    let t = text(label.to_string()).font_size(typo::CAPTION);
    let t = match on {
        Some(c) => t.color(c),
        None => t.tertiary(),
    };
    row([t]).h(26.0).pad4(0.0, 0.0, 10.0, 0.0).action(action.to_string())
}

/// Two quiet rows under the lyric block, there while lyrics are on and the track has a title (the
/// rows keep their height whatever they say, so the panel does not jump): which LRCLIB entry the
/// lyrics are ("Lirik: Artist – Title (pilihanmu)"), and the buttons to correct them: timing, "Bukan
/// lagu ini", and a web search the user's own browser opens.
fn lyric_footer(app: &App) -> Option<Node> {
    let st = app.state::<MediaState>();
    let has_title = st.m.track.as_ref().is_some_and(|t| !t.title.is_empty());
    if !has_title || !st.show_lyrics || !app.config().media.lyrics {
        return None;
    }
    let f = app.state::<lyrics::LyricsState>().footer(has_title);
    let accent = app.accent();
    // The caption: LRCLIB's own name for the entry, cut to what fits.
    let room = 70usize.saturating_sub(7 + if f.user { 12 } else { 0 } + if f.offset_ms != 0 { 11 } else { 0 });
    let mut cap = row([caption(match &f.caption {
        Some(c) => format!("{}{}", tr("Lirik: ", "Lyrics: "), clip(c, room)),
        None => " ".to_string(),
    })]);
    if f.caption.is_some() && f.user {
        cap = cap.child(caption(tr(" (pilihanmu)", " (your pick)")));
    }
    if f.caption.is_some() && f.offset_ms != 0 {
        cap = cap.child(text(format!("  \u{b7}  {}", lyrics::choices::fmt_offset(f.offset_ms))).font_size(typo::CAPTION).color(accent));
    }
    let mut keys: Vec<Node> = Vec::new();
    if f.can_shift {
        keys.push(quiet(tr("\u{2212}0,5 s", "\u{2212}0.5 s"), "media.lyrics_earlier", None));
        keys.push(quiet(tr("+0,5 s", "+0.5 s"), "media.lyrics_later", None));
    }
    let next = match f.button {
        lyrics::view::Button::WrongSong => Some(tr("Bukan lagu ini", "Wrong song")),
        lyrics::view::Button::TryOther => Some(tr("Coba kecocokan lain", "Try another match")),
        lyrics::view::Button::Restart => Some(tr("Coba lagi dari awal", "Start over")),
        lyrics::view::Button::None => None,
    };
    if let Some(l) = next {
        keys.push(quiet(l, "media.lyrics_next", None));
    }
    keys.push(spacer());
    if f.web {
        keys.push(row([icon(icons::SEARCH).font_size(12.0).color(color::TEXT3)]).h(26.0).pad4(0.0, 0.0, 5.0, 0.0));
        keys.push(quiet("Genius", "media.lyrics_web:genius", None));
        keys.push(quiet("Musixmatch", "media.lyrics_web:musixmatch", None));
    }
    Some(col([cap.fill_w().h(16.0), row(keys).fill_w().h(26.0)]).fill_w().pad4(0.0, 0.0, 0.0, 2.0))
}

fn panel(app: &App) -> Node {
    let st = app.state::<MediaState>();
    let m = &st.m;
    let Some(t) = m.track.as_ref().filter(|_| m.has_music()) else {
        return placeholder(icons::MUSIC, tr("Tidak ada musik", "Nothing playing"), tr("Putar lagu di aplikasi apa pun — Spotify, browser, Media Player.", "Play something in any app — Spotify, a browser, Media Player."));
    };
    let now = Instant::now();
    let pos = match st.scrub {
        Some(v) => (v as f64 * m.timeline.dur_ms as f64) as u64,
        None => m.position_ms(now),
    };
    let frac = if m.timeline.dur_ms > 0 { pos as f32 / m.timeline.dur_ms as f32 } else { 0.0 };
    let audio = app.state::<volume_osd::Audio>();

    let subtitle = match (t.artist.is_empty(), t.album.is_empty()) {
        (false, false) => format!("{} · {}", t.artist, t.album),
        (false, true) => t.artist.clone(),
        _ => t.album.clone(),
    };
    let mut head = row([col([title(t.title.clone()), text(subtitle).secondary()]).gap(2.0).grow(1.0)]).align(Align::Start).gap(10.0).fill_w();
    if !audio.device.is_empty() {
        let glyph = if audio.headphones { icons::HEADPHONE } else { icons::SPEAKER };
        head = head.child(capsule(glyph, audio.device.clone(), "media.output").h(26.0).max_w(170.0));
    }

    // Thin progress bar with an invisible slider on top for seeking.
    let bar = progress(frac).thickness(4.0).track(Rgba::hex(0xffffff, 0.22));
    let mut seek_bar = stack([bar]).h(18.0).grow(1.0);
    if m.controls.seek && m.timeline.dur_ms > 0 {
        seek_bar = seek_bar.child(slider(frac).h(18.0).opacity(0.0).action("media.seek"));
    }
    let times = row([caption(fmt_time(pos)), seek_bar, caption(if m.timeline.dur_ms > 0 { fmt_time(m.timeline.dur_ms) } else { "--:--".into() })]).gap(10.0).fill_w();

    let dim = |n: Node, on: bool| if on { n } else { n.opacity(0.35) };
    let play_glyph = icons::solid(if m.playing { icons::PAUSE } else { icons::PLAY });
    let lyric_chip = {
        let c = capsule(icons::MUSIC, tr("Lirik", "Lyrics"), "media.lyrics");
        if st.show_lyrics && app.config().media.lyrics { c.bg(app.accent()) } else { c }
    };
    let vol_glyph = if audio.muted { icons::MUTE } else { icons::VOLUME };
    let controls = row([
        lyric_chip,
        spacer(),
        dim(icon_button(icons::solid(icons::PREV), "media.prev"), m.controls.prev),
        dim(stack([icon(play_glyph).font_size(16.0).color(color::INK)]).size(46.0, 46.0).bg(color::WHITE).capsule().action("media.toggle"), m.controls.play_pause),
        dim(icon_button(icons::solid(icons::NEXT), "media.next"), m.controls.next),
        spacer(),
        circle_button(vol_glyph, 30.0, "media.mute"),
    ])
    .gap(18.0)
    .fill_w();

    let mut right = vec![head, lyric_block(app, pos)];
    right.extend(lyric_footer(app));
    right.extend([times, controls]);
    let right = col(right).gap(6.0).grow(1.0);
    col([row([art_node(app, 156.0, 28.0), right]).gap(16.0).align(Align::Start).fill_w()])
}

// ---- sample data (debug) -----------------------------------------------------------------------

const SAMPLE_LRC: &str = "[00:00.00]\n[00:20.00]lampu kota memudar di kaca jendela\n[00:26.00]dan kota pun perlahan terlelap\n[00:32.00]di bawah lampu jalan kita bicara\n[00:38.00]tentang esok yang belum bernama\n[01:20.00]lampu kota memudar di kaca jendela\n[01:26.00]dan kota pun perlahan terlelap\n[01:32.00]di bawah lampu jalan kita bicara\n[01:38.00]tentang esok yang belum bernama";

/// Gradient square like the mockup's art (orange → pink).
fn sample_art() -> Image {
    let n = 128u32;
    let mut px = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let t = (x + y) as f32 / (2 * (n - 1)) as f32;
            let l = |p: f32, q: f32| (p + (q - p) * t).round() as u8;
            px.extend_from_slice(&[l(92.0, 119.0), l(138.0, 61.0), 255, 255]);
        }
    }
    Image::new(n, n, px)
}

fn sample(app: &mut App) {
    let track = model::Track { title: "Midnight Drive".into(), artist: "Kaia".into(), album: "Neon Atlas".into() };
    let key = track.key();
    apply(app, Update::Session { app: "Moku".into() });
    apply(app, Update::Track(track));
    apply(app, Update::Art { key: key.clone(), art: Some(sample_art()) });
    apply(app, Update::Playback { playing: true, controls: model::Controls { prev: true, next: true, play_pause: true, seek: true } });
    apply(app, Update::Timeline(model::Timeline { pos_ms: 84_500, dur_ms: 227_000, at: Some(Instant::now()), rate: 0.0 }));
    lyrics::inject_sample(app, key, SAMPLE_LRC, lyrics::DemoMode::Auto);
}

/// Lyrics for `--state lyrics|lyrics-panel`: lines with known times, so the word highlight can be
/// looked at and measured. Plain lines (the highlight is estimated), one with real word times
/// (enhanced LRC), a long gap after a short line, a sung-word-less break, CJK, a long line.
const DEMO_LRC: &str = "[00:00.00]
[00:03.00]lampu kota memudar di kaca jendela
[00:07.50]dan kota pun perlahan terlelap
[00:12.00]di bawah lampu jalan kita bicara
[00:16.50]tentang esok yang belum bernama
[00:21.00]<00:21.00>Hel<00:21.40>lo <00:21.90>from <00:22.40>the <00:22.90>other <00:23.60>side<00:25.20>
[00:26.00]oh
[00:38.00]
[00:40.00]夜に駆ける 沈むように溶けてゆく
[00:45.00]sebuah baris yang sangat panjang sekali untuk melihat bagaimana sorotan bergerak melintasi banyak kata
[00:52.00]lampu kota memudar di kaca jendela
[00:56.50]dan kota pun perlahan terlelap
[01:01.00]di bawah lampu jalan kita bicara
[01:05.50]tentang esok yang belum bernama";

/// `--state lyrics` / `lyrics-panel`: a fake player at `start_ms` (playing) with [`DEMO_LRC`]. The
/// fixed modes run no media service, so nothing replaces it. Not used in normal runs.
pub fn demo_lyrics(app: &mut App, start_ms: u64, mode: lyrics::DemoMode) {
    let track = model::Track { title: "Midnight Drive".into(), artist: "Kaia".into(), album: "Neon Atlas".into() };
    let key = track.key();
    apply(app, Update::Session { app: "Moku".into() });
    apply(app, Update::Track(track));
    apply(app, Update::Art { key: key.clone(), art: Some(sample_art()) });
    apply(app, Update::Playback { playing: true, controls: model::Controls { prev: true, next: true, play_pause: true, seek: true } });
    apply(app, Update::Timeline(model::Timeline { pos_ms: start_ms, dur_ms: 227_000, at: Some(Instant::now()), rate: 0.0 }));
    lyrics::inject_sample(app, key, DEMO_LRC, mode);
}

#[cfg(test)]
mod tests {
    use super::app_name;

    #[test]
    fn player_names() {
        assert_eq!(app_name("Spotify.exe"), "Spotify");
        assert_eq!(app_name("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"), "ZuneMusic");
        assert_eq!(app_name("MSEdge"), "MSEdge");
        assert_eq!(app_name(""), "");
    }
}
