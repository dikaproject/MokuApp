//! The media state and its reducer (pure: no Windows calls), so the session logic can be
//! tested without a player. The GSMTC service turns OS events into [`Update`]s; the UI thread
//! applies them with [`Media::apply`] and then syncs the pill / accent from the result.

use std::time::Instant;

use crate::ui::{Image, Rgba};

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
}

impl Track {
    /// Identity of a track (art and lyrics belong to a key).
    pub fn key(&self) -> String {
        format!("{}\u{1f}{}\u{1f}{}", self.title, self.artist, self.album)
    }
}

/// What the current player allows.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Controls {
    pub prev: bool,
    pub next: bool,
    pub play_pause: bool,
    pub seek: bool,
}

/// Playback position as reported by the player, anchored at `at`.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Timeline {
    pub pos_ms: u64,
    pub dur_ms: u64,
    pub at: Option<Instant>,
}

/// One change from the media session service.
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    /// No media session left (player closed): back to idle.
    SessionGone,
    /// The current session is the one of this app (AUMID). A different app resets the track.
    Session { app: String },
    Track(Track),
    /// Album art for the track with this key (`None` = the track has no art).
    Art { key: String, art: Option<Image> },
    Playback { playing: bool, controls: Controls },
    Timeline(Timeline),
}

/// What changed in an `apply` that the glue code has to act on.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Changed {
    /// The track identity changed (or the session went away): refetch lyrics etc.
    pub track: bool,
    /// The album colour changed.
    pub color: bool,
}

#[derive(Default)]
pub struct Media {
    pub app: Option<String>,
    pub track: Option<Track>,
    pub art: Option<Image>,
    /// Vivid colour derived from the art (accent while `follow_album_color`).
    pub art_color: Option<Rgba>,
    pub playing: bool,
    pub controls: Controls,
    pub timeline: Timeline,
}

impl Media {
    pub fn apply(&mut self, u: Update) -> Changed {
        let mut ch = Changed::default();
        let old_color = self.art_color;
        match u {
            Update::SessionGone => {
                ch.track = self.track.is_some() || self.app.is_some();
                *self = Media::default();
            }
            Update::Session { app } => {
                if self.app.as_deref() != Some(app.as_str()) {
                    // Another player took over: nothing of the old one may stay on screen.
                    ch.track = self.track.is_some();
                    *self = Media { app: Some(app), ..Media::default() };
                }
            }
            Update::Track(t) => {
                let same = self.track.as_ref().map(Track::key) == Some(t.key());
                if !same {
                    ch.track = true;
                    self.art = None;
                    self.art_color = None;
                    self.timeline = Timeline::default();
                }
                self.track = Some(t);
            }
            Update::Art { key, art } => {
                // Art that arrives late for a previous track is dropped.
                if self.track.as_ref().map(Track::key).as_deref() == Some(key.as_str()) {
                    self.art_color = art.as_ref().and_then(album_color);
                    self.art = art;
                }
            }
            Update::Playback { playing, controls } => {
                self.playing = playing;
                self.controls = controls;
            }
            Update::Timeline(t) => self.timeline = t,
        }
        ch.color = self.art_color != old_color;
        ch
    }

    /// Something to show: a session with a titled track.
    pub fn has_music(&self) -> bool {
        self.app.is_some() && self.track.as_ref().is_some_and(|t| !t.title.trim().is_empty())
    }

    /// Accent override wanted by the music (None = use the configured accent).
    pub fn accent(&self) -> Option<Rgba> {
        if self.has_music() { self.art_color } else { None }
    }

    /// Current position, extrapolated from the last report while playing.
    pub fn position_ms(&self, now: Instant) -> u64 {
        let t = &self.timeline;
        let mut p = t.pos_ms;
        if self.playing {
            if let Some(at) = t.at {
                p += now.saturating_duration_since(at).as_millis() as u64;
            }
        }
        if t.dur_ms > 0 { p.min(t.dur_ms) } else { p }
    }

    /// 0–1 progress (0 when the duration is unknown).
    pub fn progress(&self, now: Instant) -> f32 {
        if self.timeline.dur_ms == 0 { 0.0 } else { self.position_ms(now) as f32 / self.timeline.dur_ms as f32 }
    }
}

/// Turn an average album colour into an accent that reads on the dark glass: keep the hue,
/// lift saturation and value. Near-grey art gives `None` (keep the configured accent).
pub fn vivid(c: Rgba) -> Option<Rgba> {
    let (r, g, b) = (c.r, c.g, c.b);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    if max <= 0.0 || d / max < 0.12 {
        return None;
    }
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    let s = (d / max).clamp(0.55, 0.85);
    let v = max.clamp(0.75, 0.95);
    // HSV → RGB.
    let cc = v * s;
    let x = cc * (1.0 - ((h % 2.0) - 1.0).abs());
    let m = v - cc;
    let (r1, g1, b1) = match h as u32 {
        0 => (cc, x, 0.0),
        1 => (x, cc, 0.0),
        2 => (0.0, cc, x),
        3 => (0.0, x, cc),
        4 => (x, 0.0, cc),
        _ => (cc, 0.0, x),
    };
    Some(Rgba { r: r1 + m, g: g1 + m, b: b1 + m, a: 1.0 })
}

/// Accent from album art: the dominant *saturated* hue (a 12-bin hue histogram weighted by
/// saturation × value), made vivid. A plain average fails on colourful art — orange and blue
/// average to grey. Mostly grey / dark art gives `None`.
pub fn album_color(img: &Image) -> Option<Rgba> {
    const BINS: usize = 12;
    let mut w = [0f32; BINS];
    let mut sum = [[0f32; 3]; BINS];
    let mut total = 0f32;
    for px in img.0.bgra.chunks_exact(4).step_by(3) {
        if px[3] < 128 {
            continue;
        }
        let (r, g, b) = (px[2] as f32 / 255.0, px[1] as f32 / 255.0, px[0] as f32 / 255.0);
        let max = r.max(g).max(b);
        let d = max - r.min(g).min(b);
        total += 1.0;
        if max < 0.2 || d / max.max(1e-6) < 0.25 {
            continue;
        }
        let h = if max == r {
            ((g - b) / d).rem_euclid(6.0)
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        let bin = ((h / 6.0 * BINS as f32) as usize).min(BINS - 1);
        let k = (d / max) * max;
        w[bin] += k;
        for (s, c) in sum[bin].iter_mut().zip([r, g, b]) {
            *s += c * k;
        }
    }
    let (bin, &best) = w.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    // Ignore art where colourful pixels are a speck (weight per pixel < 4 %).
    if total == 0.0 || best / total < 0.04 {
        return None;
    }
    let [r, g, b] = sum[bin].map(|s| s / best);
    vivid(Rgba { r, g, b, a: 1.0 })
}

/// `m:ss` (or `h:mm:ss`).
pub fn fmt_time(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn solid(rgb: u32) -> Image {
        let px = [(rgb & 0xff) as u8, ((rgb >> 8) & 0xff) as u8, (rgb >> 16) as u8, 255];
        Image::new(4, 4, px.repeat(16))
    }

    fn track(t: &str) -> Track {
        Track { title: t.into(), artist: "Kaia".into(), album: "Neon Atlas".into() }
    }

    fn playing_state() -> Media {
        let mut m = Media::default();
        m.apply(Update::Session { app: "Spotify.exe".into() });
        m.apply(Update::Track(track("Midnight Drive")));
        m.apply(Update::Art { key: track("Midnight Drive").key(), art: Some(solid(0xFF3D77)) });
        m.apply(Update::Playback { playing: true, controls: Controls { prev: true, next: true, play_pause: true, seek: true } });
        m.apply(Update::Timeline(Timeline { pos_ms: 84_000, dur_ms: 227_000, at: Some(Instant::now()) }));
        m
    }

    /// "Hide when idle" asks the activity list: the music pill is there (priority MUSIC) exactly while
    /// `has_music()` is, so a playing and a paused player keep the island up and a gone session lets it hide.
    #[test]
    fn music_keeps_the_idle_island_up_while_it_has_a_pill_and_releases_it_with_the_session() {
        let idle_island_up = |m: &Media| {
            let top = if m.has_music() { crate::activity::priority::MUSIC } else { crate::activity::priority::IDLE };
            crate::features::placement::pure_idle_busy(false, top)
        };
        let mut m = Media::default();
        assert!(!idle_island_up(&m), "no session: idle");
        m.apply(Update::Session { app: "Spotify.exe".into() });
        assert!(!idle_island_up(&m), "a session without a titled track has no pill yet");
        m.apply(Update::Track(track("Midnight Drive")));
        m.apply(Update::Playback { playing: true, controls: Controls::default() });
        assert!(idle_island_up(&m), "playing");
        m.apply(Update::Playback { playing: false, controls: Controls::default() });
        assert!(idle_island_up(&m), "paused, the player is still open: its pill stays");
        m.apply(Update::SessionGone);
        assert!(!idle_island_up(&m), "the player went away: nothing in the pill, the island may hide");
    }

    #[test]
    fn media_session_gone_returns_to_idle() {
        let mut m = playing_state();
        assert!(m.has_music());
        assert!(m.art.is_some());
        assert!(m.accent().is_some());
        let ch = m.apply(Update::SessionGone);
        assert!(ch.track && ch.color);
        assert!(!m.has_music(), "no pill without a session");
        assert!(m.art.is_none() && m.track.is_none() && m.app.is_none());
        assert!(!m.playing);
        assert_eq!(m.accent(), None, "accent override released");
        // A second "gone" is a no-op.
        assert_eq!(m.apply(Update::SessionGone), Changed::default());
    }

    #[test]
    fn track_change_drops_stale_art() {
        let mut m = playing_state();
        let old_key = track("Midnight Drive").key();
        let ch = m.apply(Update::Track(track("Second Song")));
        assert!(ch.track && ch.color);
        assert!(m.art.is_none(), "old art must not stay on the new track");
        // Late art for the previous track is ignored.
        m.apply(Update::Art { key: old_key, art: Some(solid(0x00FF00)) });
        assert!(m.art.is_none());
        // The same track again does not reset anything.
        m.apply(Update::Art { key: track("Second Song").key(), art: Some(solid(0x0A84FF)) });
        assert!(!m.apply(Update::Track(track("Second Song"))).track);
        assert!(m.art.is_some());
    }

    #[test]
    fn other_player_resets_and_untitled_is_idle() {
        let mut m = playing_state();
        let ch = m.apply(Update::Session { app: "chrome.exe".into() });
        assert!(ch.track);
        assert!(!m.has_music() && m.art.is_none());
        m.apply(Update::Track(Track::default()));
        assert!(!m.has_music(), "a session without a title shows nothing");
        // Same app again keeps the state.
        m.apply(Update::Track(track("x")));
        m.apply(Update::Session { app: "chrome.exe".into() });
        assert!(m.has_music());
    }

    #[test]
    fn position_extrapolates_only_while_playing() {
        let mut m = playing_state();
        let at = Instant::now();
        m.apply(Update::Timeline(Timeline { pos_ms: 10_000, dur_ms: 20_000, at: Some(at) }));
        assert_eq!(m.position_ms(at + Duration::from_secs(3)), 13_000);
        assert_eq!(m.position_ms(at + Duration::from_secs(60)), 20_000, "clamped to the duration");
        m.playing = false;
        assert_eq!(m.position_ms(at + Duration::from_secs(3)), 10_000);
        assert!((m.progress(at) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn album_color_picks_dominant_hue() {
        // 60 % orange, 40 % blue: the average is greyish, the dominant hue is orange.
        let (o, b) = ([0x20u8, 0x90, 0xFF, 255], [0xFFu8, 0x80, 0x20, 255]);
        let mut px = Vec::new();
        for i in 0..100 {
            px.extend_from_slice(if i % 5 < 3 { &o } else { &b });
        }
        let c = album_color(&Image::new(10, 10, px)).unwrap();
        assert!(c.r > c.g && c.g > c.b, "orange expected, got {c:?}");
        assert_eq!(album_color(&solid(0x7F7F7F)), None, "grey art keeps the configured accent");
        assert_eq!(album_color(&solid(0x050505)), None);
    }

    #[test]
    fn vivid_colour_and_time_format() {
        assert_eq!(vivid(Rgba::hex(0x808080, 1.0)), None);
        let c = vivid(Rgba::hex(0x402010, 1.0)).unwrap();
        assert!(c.r > c.g && c.g > c.b && c.r >= 0.74);
        assert_eq!(fmt_time(84_000), "1:24");
        assert_eq!(fmt_time(3_725_000), "1:02:05");
    }
}
