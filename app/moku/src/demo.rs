//! Prototype parity content for `--state music|music-panel` and `--demo`: the "Midnight Drive"
//! music pill and panel from the mockups, built with the UI kit. Not used in normal runs.
//! (Also a worked example of a pill and a panel body.)

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::activity::{Activity, priority};
use crate::app::App;
use crate::ui::*;

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Demo content replaces the `media` tab body while active.
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

pub fn enable() {
    ACTIVE.store(true, Ordering::Relaxed);
}

/// Album art placeholder: the orange → pink gradient square from the mockup.
fn art() -> &'static Image {
    static ART: OnceLock<Image> = OnceLock::new();
    ART.get_or_init(|| {
        let n = 64u32;
        let mut px = Vec::with_capacity((n * n * 4) as usize);
        let (a, b) = ((0xFFu32, 0x8Au32, 0x5Cu32), (0xFFu32, 0x3Du32, 0x77u32));
        for y in 0..n {
            for x in 0..n {
                let t = (x + y) as f32 / (2 * (n - 1)) as f32;
                let l = |p: u32, q: u32| (p as f32 + (q as f32 - p as f32) * t).round() as u8;
                px.extend_from_slice(&[l(a.2, b.2), l(a.1, b.1), l(a.0, b.0), 255]);
            }
        }
        Image::new(n, n, px)
    })
}

fn art_node(size: f32, radius: f32, note: bool) -> Node {
    let mut s = stack([image(art(), radius).size(size, size)]);
    if note {
        s = s.child(icon(icons::MUSIC).font_size((size * 0.42).round()));
    }
    s.size(size, size)
}

/// The music pill: art, title, artist, live bars.
pub fn show_music(app: &mut App) {
    let a = Activity::persistent("demo.music", priority::MUSIC, |_| {
        row([art_node(24.0, 7.0, false), text("Midnight Drive").semibold(), text("Kaia").secondary(), gap_px(3.0), bars(true)])
            .h(36.0)
            .pad4(8.0, 0.0, 12.0, 0.0)
            .gap(8.0)
            .action("demo.pill")
    })
    .tab("media")
    .bubble(|_| art_node(24.0, 7.0, false));
    app.set_activity(a);
}

/// The music panel body (610 wide).
pub fn panel(app: &App) -> Node {
    let _ = app;
    let controls = row([
        capsule(icons::MUSIC, "Lirik", "demo.lyrics"),
        spacer(),
        icon_button(icons::solid(icons::PREV), "demo.prev"),
        stack([icon(icons::solid(icons::PAUSE)).font_size(16.0).color(color::INK)]).size(46.0, 46.0).bg(color::WHITE).capsule().action("demo.play"),
        icon_button(icons::solid(icons::NEXT), "demo.next"),
        spacer(),
        circle_button(icons::VOLUME, 30.0, "demo.volume"),
    ])
    .gap(18.0)
    .fill_w();
    let right = col([
        row([
            col([title("Midnight Drive"), text("Kaia · Neon Atlas").secondary()]).gap(2.0).grow(1.0),
            capsule(icons::HEADPHONE, "Buds Pro", "demo.output").h(26.0),
        ])
        .align(Align::Start)
        .fill_w(),
        col([
            text("lampu kota memudar di kaca jendela").tertiary(),
            text("dan kota pun perlahan terlelap").font_size(15.0).semibold(),
            text("di bawah lampu jalan kita bicara").tertiary(),
        ])
        .gap(4.0)
        .pad4(0.0, 6.0, 0.0, 2.0),
        row([caption("1:24"), progress(0.37).grow(1.0), caption("3:47")]).gap(10.0).fill_w(),
        controls,
    ])
    .gap(6.0)
    .grow(1.0);
    col([row([art_node(156.0, 28.0, true), right]).gap(16.0).align(Align::Start).fill_w()])
}
