//! The UI side of the smart clipboard actions (the detectors are in `actions/`): the buttons on
//! a row, the short pill when a new clip matches strongly, the QR card, and what each button does.
//!
//! Nothing here logs clipboard content. Text is read from the item's file (or, for a one-time
//! code, from memory) only when a button is pressed.

use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

use super::actions::{self, Act, color as colour};
use super::model::ClipItem;
use super::{Clipboard, Flash, QrView, copy_string, dir, store};
use crate::activity::{Activity, HEADS_UP_SECS};
use crate::app::{App, Redraw};
use crate::i18n::{self, tr, tr_s};
use crate::registry::ActionKind;
use crate::ui::{self, *};

/// Between `WARNING` (65) and `OSD` (70): above music and a running timer, below real alerts.
const PRIORITY: u8 = 66;
/// Glyph of the calculator (Segoe Fluent / MDL2 "Calculator"), as in the launcher.
const CALC: &str = "\u{E8EF}";

/// One button of a clip.
#[derive(Clone, Debug, PartialEq)]
pub struct Chip {
    pub label: String,
    /// The word after the key in `clipboard.act:<key>:<verb>`.
    pub verb: &'static str,
}

/// The buttons a clip offers (empty for plain text, images and one-time codes: the row itself
/// copies those).
pub fn chips(it: &ClipItem) -> Vec<Chip> {
    let chip = |label: &str, verb: &'static str| Chip { label: label.to_string(), verb };
    match it.act {
        Some(Act::Color) => match colour::parse(&it.preview) {
            Some((_, f)) => f.others().iter().map(|o| chip(o.label(), o.verb())).collect(),
            None => vec![],
        },
        Some(Act::Url) => {
            let mut v = vec![];
            if it.bytes as usize <= crate::ui::qr::MAX_BYTES {
                v.push(chip("QR", "qr"));
            }
            v.push(chip(tr("Buka", "Open"), "open"));
            v
        }
        Some(Act::Math) => vec![chip(tr("Salin hasil", "Copy result"), "result")],
        Some(Act::Json) => vec![chip(tr("Rapikan", "Format"), "pretty")],
        Some(Act::Phone) => vec![chip("WhatsApp", "wa")],
        Some(Act::Otp) | None => vec![],
    }
}

/// What the row's caption says instead of the plain kind ("Teks"), for clips with an action.
pub fn caption_for(it: &ClipItem, comma: bool) -> Option<String> {
    match it.act? {
        Act::Math => {
            let v = actions::math(&it.preview)?;
            Some(format!("= {}", actions::math_text(v, comma, true)))
        }
        Act::Phone => actions::phone_wa(&it.preview).map(|wa| format!("{} · {}", tr("Nomor HP", "Phone number"), actions::phone_display(&wa))),
        Act::Json => Some("JSON".to_string()),
        Act::Url => Some(format!("{} · {}", tr("Tautan", "Link"), actions::host_of(&it.preview))),
        Act::Color | Act::Otp => None,
    }
}

fn chip_node(c: &Chip, key: u64) -> Node {
    row([text(c.label.clone()).font_size(11.5).semibold()]).pad_xy(10.0, 0.0).h(26.0).bg(color::FILL2).capsule().action(format!("clipboard.act:{key}:{}", c.verb))
}

/// The row's buttons as one node (empty when smart actions are off).
pub fn chip_row(app: &App, it: &ClipItem) -> Option<Node> {
    if !app.config().clipboard.smart_actions {
        return None;
    }
    let cs = chips(it);
    (!cs.is_empty()).then(|| row(cs.iter().map(|c| chip_node(c, it.key))).gap(6.0))
}

// ---- the pill -----------------------------------------------------------------------------------

/// Show the short pill for a new, strongly matching clip (when the user wants pills, and not
/// during a meeting or focus mode).
pub fn show_pill(app: &mut App, key: u64) {
    let cfg = &app.config().clipboard;
    if !cfg.smart_actions || !cfg.smart_pill || app.meeting().active || app.focus_hold() {
        return;
    }
    let secs = if app.state::<Clipboard>().hist.get(key).is_some_and(|i| i.is_otp()) { 5.0 } else { HEADS_UP_SECS };
    app.set_activity(Activity::transient("clipboard.smart", PRIORITY, secs, move |app| pill_node(app, key)).tab("clipboard"));
}

fn pill_node(app: &App, key: u64) -> Node {
    let Some(it) = app.state::<Clipboard>().hist.get(key) else {
        return pill([icon(icons::CLIPBOARD).color(color::TEXT3), text("…")]);
    };
    let btns: Vec<Node> = chips(it).iter().map(|c| chip_node(c, key)).collect();
    let (lead, label): (Node, Node) = match it.act {
        Some(Act::Color) => {
            let rgb = colour::parse(&it.preview).map(|(c, _)| c.rgb_u32()).unwrap_or(0x808080);
            (empty().size(20.0, 20.0).bg(Rgba::hex(rgb, 1.0)).radius(6.0).border(Rgba::hex(0xffffff, 0.3)), text(it.preview.clone()).semibold().max_w(220.0))
        }
        Some(Act::Url) => (icon(icons::LINK).color(color::GREEN), text(actions::host_of(&it.preview)).semibold().max_w(220.0)),
        Some(Act::Math) => {
            let comma = !i18n::is_english();
            let shown = actions::math(&it.preview).map(|v| format!("= {}", actions::math_text(v, comma, true))).unwrap_or_default();
            (icon(CALC).color(color::ORANGE), text(shown).semibold().max_w(220.0))
        }
        Some(Act::Json) => (icon(icons::CODE).color(color::TEAL), text("JSON").semibold()),
        Some(Act::Phone) => {
            let shown = actions::phone_wa(&it.preview).map(|wa| actions::phone_display(&wa)).unwrap_or_default();
            (icon(icons::PHONE).color(color::GREEN), text(shown).semibold())
        }
        Some(Act::Otp) => (icon(icons::LOCK).color(color::ORANGE), text(tr("Kode OTP ditandai", "One-time code marked")).semibold()),
        None => (icon(icons::CLIPBOARD).color(color::TEXT3), text(it.preview.clone())),
    };
    let mut kids = vec![lead, label];
    if it.is_otp() {
        kids.push(caption(tr("hilang dalam 60 dtk", "gone in 60 s")));
        kids.push(chip_node(&Chip { label: tr("Salin", "Copy").to_string(), verb: "copy" }, key));
    }
    kids.extend(btns);
    pill(kids)
}

// ---- the QR card --------------------------------------------------------------------------------

pub fn qr_view(q: &QrView) -> Node {
    let header = row([
        label(tr("Kode QR", "QR code")),
        caption(actions::host_of(&q.text)),
        spacer(),
        capsule(icons::CHEVRON_LEFT, tr("Kembali", "Back"), "clipboard.qr_back"),
    ])
    .gap(8.0)
    .fill_w()
    .h(30.0);
    let side = col([
        text(q.text.clone()).font_size(12.0).lines(4),
        caption(tr("Arahkan kamera HP ke kode ini", "Point your phone's camera at this code")),
        row([capsule(icons::COPY, tr("Salin tautan", "Copy link"), format!("clipboard.copy:{}", q.key)), capsule(icons::LINK, tr("Buka", "Open"), format!("clipboard.act:{}:open", q.key))]).gap(8.0),
    ])
    .gap(10.0)
    .grow(1.0);
    col([header, row([ui::qr(&q.text), side]).gap(16.0).fill_w()]).gap(10.0).fill_w()
}

// ---- what a button does -------------------------------------------------------------------------

/// The text of a clip: from its file, or from memory for a one-time code.
fn text_of(app: &App, it: &ClipItem) -> Option<String> {
    if it.is_otp() {
        return app.state::<Clipboard>().secrets.get(&it.key).cloned();
    }
    store::read_text(&dir(), it.key)
}

/// `ShellExecute` on a URI. Only ever called with a validated `http(s)` address.
fn open_uri(uri: &str) -> bool {
    let h = unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(uri), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    h.0 as isize > 32
}

/// A button was pressed: `arg` is `"<key>:<verb>"`.
pub fn act(app: &mut App, arg: &str) {
    let Some((k, verb)) = arg.split_once(':') else { return };
    let Ok(key) = k.parse::<u64>() else { return };
    let Some(item) = app.state::<Clipboard>().hist.get(key).cloned() else { return };
    let Some(text) = text_of(app, &item) else {
        return super::set_flash(app, key, Flash::Failed, 2500);
    };
    match verb {
        "hex" | "rgb" | "hsl" => {
            let (Some(f), Some((c, _))) = (colour::Fmt::from_verb(verb), colour::parse(&text)) else { return };
            let label = f.label();
            copy_string(app, key, c.format(f), Flash::As(label));
        }
        "result" => match actions::math(&text) {
            // What goes on the clipboard has no thousands grouping, and the decimal mark of the UI language.
            Some(v) => copy_string(app, key, actions::math_text(v, !i18n::is_english(), false), Flash::Msg("Hasil disalin", "Result copied")),
            None => super::set_flash(app, key, Flash::Failed, 2500),
        },
        "pretty" => match actions::json::pretty(&text) {
            Some(p) => copy_string(app, key, p, Flash::Msg("Dirapikan dan disalin", "Formatted and copied")),
            None => super::set_flash(app, key, Flash::Failed, 2500),
        },
        "qr" => match actions::url(&text) {
            Some(u) if actions::qr_fits(&u) => open_qr(app, key, u),
            _ => super::set_flash(app, key, Flash::Msg("Terlalu panjang untuk QR", "Too long for a QR code"), 2500),
        },
        "open" => match actions::url(&text) {
            Some(u) if open_uri(&u) => super::set_flash(app, key, Flash::Msg("Dibuka di peramban", "Opened in the browser"), 1800),
            _ => super::set_flash(app, key, Flash::Failed, 2500),
        },
        "wa" => match actions::phone_wa(&text) {
            Some(wa) if open_uri(&actions::wa_link(&wa)) => super::set_flash(app, key, Flash::Msg("WhatsApp dibuka", "WhatsApp opened"), 1800),
            _ => super::set_flash(app, key, Flash::Failed, 2500),
        },
        "copy" => app.dispatch_action(&format!("clipboard.copy:{key}"), ActionKind::Click),
        _ => {}
    }
}

/// Show the QR code of `url` in the clipboard tab.
fn open_qr(app: &mut App, key: u64, url: String) {
    app.state_mut::<Clipboard>().qr = Some(QrView { key, text: url });
    if app.is_tab_visible("clipboard") {
        app.request_redraw(Redraw::Tab("clipboard"));
    } else {
        app.open_panel(Some("clipboard"));
        app.request_redraw(Redraw::All);
    }
}

/// "Kembali": back to the list; the bitmap and the URL are dropped.
pub fn close_qr(app: &mut App) {
    app.state_mut::<Clipboard>().qr = None;
    crate::ui::qr::clear_cache();
    app.request_redraw(Redraw::Tab("clipboard"));
}

/// The copy of a row's flash text.
pub fn flash_text(f: Flash) -> Option<String> {
    match f {
        Flash::Copied => Some(tr("Disalin ke clipboard", "Copied to clipboard").to_string()),
        Flash::Failed => None,
        Flash::Msg(id, en) => Some(tr_s(id.to_string(), en.to_string())),
        Flash::As(label) => Some(tr_s(format!("Disalin sebagai {label}"), format!("Copied as {label}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::clipboard::model::ClipItem;

    fn item(text: &str, act: Option<Act>) -> ClipItem {
        let mut it = ClipItem::text(text, false, 1);
        it.act = act;
        it
    }

    fn labels(it: &ClipItem) -> Vec<String> {
        chips(it).into_iter().map(|c| c.label).collect()
    }

    fn verbs(it: &ClipItem) -> Vec<&'static str> {
        chips(it).into_iter().map(|c| c.verb).collect()
    }

    #[test]
    fn colour_rows_offer_the_other_two_notations() {
        assert_eq!(labels(&item("#0A84FF", Some(Act::Color))), ["RGB", "HSL"]);
        assert_eq!(labels(&item("rgb(10, 132, 255)", Some(Act::Color))), ["HEX", "HSL"]);
        assert_eq!(verbs(&item("hsl(210, 100%, 52%)", Some(Act::Color))), ["hex", "rgb"]);
    }

    #[test]
    fn link_row_has_qr_only_when_the_url_fits() {
        let u = item("https://moku.example/a", Some(Act::Url));
        assert_eq!(verbs(&u), ["qr", "open"]);
        let mut long = item(&format!("https://moku.example/{}", "a".repeat(300)), Some(Act::Url));
        long.bytes = 330;
        assert_eq!(verbs(&long), ["open"], "no QR for a link over 213 bytes, but it can still be opened");
    }

    #[test]
    fn other_actions() {
        assert_eq!(verbs(&item("12*(3+4)", Some(Act::Math))), ["result"]);
        assert_eq!(verbs(&item("{\"a\":1}", Some(Act::Json))), ["pretty"]);
        assert_eq!(verbs(&item("0812 3456 7890", Some(Act::Phone))), ["wa"]);
        assert!(chips(&item("halo", None)).is_empty());
        assert!(chips(&ClipItem::otp("482913", 5)).is_empty(), "a code is copied by its row");
    }

    #[test]
    fn row_captions() {
        assert_eq!(caption_for(&item("12*(3+4)", Some(Act::Math)), true).as_deref(), Some("= 84"));
        assert_eq!(caption_for(&item("1000*1,5", Some(Act::Math)), true).as_deref(), Some("= 1.500"));
        assert_eq!(caption_for(&item("1000*1.5", Some(Act::Math)), false).as_deref(), Some("= 1,500"));
        assert_eq!(caption_for(&item("{\"a\":1}", Some(Act::Json)), true).as_deref(), Some("JSON"));
        let phone = caption_for(&item("0812-3456-7890", Some(Act::Phone)), true).unwrap();
        assert!(phone.ends_with("+62 812-3456-7890"), "{phone}");
        let url = caption_for(&item("https://github.com/moku-app/moku", Some(Act::Url)), true).unwrap();
        assert!(url.ends_with("github.com"), "{url}");
        assert_eq!(caption_for(&item("#fff", Some(Act::Color)), true), None);
        assert_eq!(caption_for(&item("halo", None), true), None);
    }

    #[test]
    fn flash_texts() {
        assert_eq!(flash_text(Flash::Copied).as_deref(), Some("Disalin ke clipboard"));
        assert_eq!(flash_text(Flash::Failed), None);
        assert_eq!(flash_text(Flash::As("RGB")).as_deref(), Some("Disalin sebagai RGB"));
        assert_eq!(flash_text(Flash::Msg("Dibuka", "Opened")).as_deref(), Some("Dibuka"));
    }
}
