//! Composite widgets built from the primitives, styled per the mockups. Every function returns
//! a plain [`Node`], so they can be further modified (`.w()`, `.action()`, ...).

use super::*;

/// Standard pill frame: 36 DIP tall, 14 DIP side padding, 8 DIP gaps, children centred.
/// Use for activity pills: `pill([icon(..), text(..)])`.
pub fn pill(children: impl IntoIterator<Item = Node>) -> Node {
    row(children).h(36.0).pad_xy(14.0, 0.0).gap(8.0)
}

/// Capsule button with a label (fill white 14 %), e.g. "Kosongkan".
pub fn button(label_text: impl Into<String>, action: impl Into<String>) -> Node {
    row([text(label_text).semibold()]).pad_xy(14.0, 0.0).h(30.0).bg(color::FILL2).capsule().action(action)
}

/// White capsule button with dark label (primary action, e.g. "Setujui").
pub fn button_primary(label_text: impl Into<String>, action: impl Into<String>) -> Node {
    row([text(label_text).semibold().color(color::INK)]).pad_xy(14.0, 0.0).h(30.0).bg(color::WHITE).capsule().action(action)
}

/// Accent-filled capsule button (e.g. "Kirim ke HP", "+1 gelas").
pub fn button_accent(label_text: impl Into<String>, accent: Rgba, action: impl Into<String>) -> Node {
    row([text(label_text).semibold()]).pad_xy(14.0, 0.0).h(30.0).bg(accent).capsule().action(action)
}

/// Chip with icon and label (e.g. "♫ Lirik", "🎧 Buds Pro"); 26–30 DIP tall.
pub fn capsule(glyph: &str, label_text: impl Into<String>, action: impl Into<String>) -> Node {
    row([icon(glyph).font_size(13.0), text(label_text).font_size(12.0).semibold()])
        .gap(6.0)
        .pad_xy(12.0, 0.0)
        .h(28.0)
        .bg(color::FILL2)
        .capsule()
        .action(action)
}

/// Round button with a glyph (fill white 14 %). `size` = diameter.
pub fn circle_button(glyph: &str, size: f32, action: impl Into<String>) -> Node {
    stack([icon(glyph).font_size((size * 0.45).round())]).size(size, size).bg(color::FILL2).capsule().action(action)
}

/// Bare glyph with a 28 DIP click target.
pub fn icon_button(glyph: &str, action: impl Into<String>) -> Node {
    stack([icon(glyph).font_size(15.0)]).size(typo::MIN_TARGET, typo::MIN_TARGET).action(action)
}

/// Card: white 8 % fill, radius 22, padding 14, vertical content.
pub fn card(children: impl IntoIterator<Item = Node>) -> Node {
    col(children).bg(color::FILL).radius(typo::RADIUS_CARD).pad(14.0).gap(6.0)
}

/// Rounded square with a white glyph on a colour (list-row leading tile), 28 DIP.
pub fn tile(glyph: &str, bg: Rgba) -> Node {
    stack([icon(glyph).font_size(14.0)]).size(28.0, 28.0).bg(bg).radius(8.0)
}

/// List row: leading node, title + subtitle, trailing node. Fills the width.
/// `list_row(tile(icons::LINK, color::GREEN), "github.com/moku", "Tautan · 14 menit lalu", empty())`
pub fn list_row(leading: Node, title_text: impl Into<String>, subtitle: impl Into<String>, trailing: Node) -> Node {
    let sub: String = subtitle.into();
    let mut texts = col([text(title_text).semibold()]).gap(1.0).grow(1.0);
    if !sub.is_empty() {
        texts = texts.child(caption(sub));
    }
    row([leading, texts, trailing]).gap(10.0).fill_w().pad_xy(10.0, 7.0).bg(color::FILL).radius(14.0)
}

/// Control Center tile (Wi-Fi, Bluetooth, Mode gelap...): circle icon (accent when on),
/// title and status line. Grows to share the row.
pub fn toggle_tile(glyph: &str, title_text: impl Into<String>, status: impl Into<String>, on: bool, accent: Rgba, action: impl Into<String>) -> Node {
    let circle = stack([icon(glyph).font_size(14.0)]).size(32.0, 32.0).bg(if on { accent } else { color::FILL2 }).capsule();
    row([circle, col([text(title_text).semibold().font_size(12.5), caption(status)]).gap(1.0)])
        .gap(10.0)
        .pad_xy(8.0, 0.0)
        .h(48.0)
        .grow(1.0)
        .bg(color::FILL)
        .radius(24.0)
        .action(action)
}

/// "Segera hadir" placeholder body for tabs whose feature is not built yet.
pub fn placeholder(glyph: &str, heading: impl Into<String>, sub: impl Into<String>) -> Node {
    col([icon(glyph).font_size(28.0).color(color::TEXT2), text(heading).font_size(typo::CARD_TITLE).semibold(), caption(sub)])
        .align(Align::Center)
        .gap(8.0)
        .fill_w()
        .pad_xy(0.0, 36.0)
}

/// One entry of the panel's tab bar (built by the core from registered tabs).
pub struct TabItem<'a> {
    pub id: &'a str,
    pub glyph: &'a str,
    pub label: &'a str,
    pub dimmed: bool,
}

/// Tab bar per the mockup: 610 × 38 track (white 8 %), the active tab as a white 16 % capsule
/// with icon + label, the others as icons. Each tab's action is `"tab.<id>"`; the track is
/// `"tab.bar"`, so the wheel anywhere on it switches tabs.
pub fn tab_bar(tabs: &[TabItem], active: Option<&str>) -> Node {
    // Every inactive tab is a slot of the same width (they share what the active capsule leaves,
    // `grow`), its icon centred in it: the spacing is even whatever the number of enabled tabs
    // (2 or 11), instead of icons spread to the two ends with unequal gaps next to the capsule.
    let items = tabs.iter().map(|t| {
        let a = format!("tab.{}", t.id);
        if Some(t.id) == active {
            row([icon(t.glyph).font_size(14.0), text(t.label).semibold()]).gap(6.0).pad_xy(14.0, 0.0).h(30.0).bg(color::FILL3).capsule().action(a)
        } else {
            let c = if t.dimmed { color::TEXT3 } else { Rgba::hex(0xffffff, 0.85) };
            stack([icon(t.glyph).font_size(15.0).color(c)]).h(30.0).grow(1.0).action(a)
        }
    });
    row(items).fill_w().h(38.0).pad_xy(4.0, 4.0).bg(color::FILL).capsule().action("tab.bar")
}

/// Round trash-can button (Segoe Fluent Icons "Delete", same code point in MDL2 Assets) with an
/// accessible name. Give it a `.key(..)` to make it the target of `Anim::into_key` and
/// `.wiggle(n)` it after things flew in.
pub fn trash_button(action: impl Into<String>, name: impl Into<String>) -> Node {
    circle_button(icons::DELETE, 30.0, action).name(name)
}
