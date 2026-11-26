//! One module per feature. Each owns its state, UI, services and handlers, and registers them
//! in `register`. Feature tracks edit only their own file(s); this file only lists them.

pub mod adzan;
pub mod agents;
pub mod ai;
pub mod ai_cards;
pub mod bmkg;
pub mod calendar;
pub mod capture;
pub mod clipboard;
pub mod control;
pub mod downloads;
pub mod earbuds;
pub mod glance;
pub mod hijri;
pub mod launcher;
pub mod lyrics;
pub mod media;
pub mod meeting;
pub mod monitor;
pub mod notify;
pub mod notify_mirror;
pub mod ocr;
pub mod phone_send;
pub mod placement;
pub mod prayer;
pub mod privacy;
pub mod ramadan;
pub mod reminders;
pub mod shelf;
pub mod system;
pub mod timer;
pub mod today;
pub mod update;
pub mod volume_osd;

use crate::i18n;
use crate::registry::*;
use crate::ui::*;

/// Called once at startup, before the island exists.
pub fn register_all(r: &mut Registry) {
    core(r);
    media::register(r);
    lyrics::register(r);
    volume_osd::register(r);
    control::register(r);
    system::register(r);
    earbuds::register(r);
    privacy::register(r);
    meeting::register(r);
    monitor::register(r);
    timer::register(r);
    reminders::register(r);
    prayer::register(r);
    adzan::register(r);
    ramadan::register(r);
    today::register(r);
    bmkg::register(r);
    glance::register(r);
    calendar::register(r);
    downloads::register(r);
    clipboard::register(r);
    capture::register(r);
    ocr::register(r);
    shelf::register(r);
    phone_send::register(r);
    notify::register(r);
    notify_mirror::register(r);
    ai_cards::register(r);
    ai::register(r);
    agents::register(r);
    update::register(r);
    launcher::register(r);
    placement::register(r);
}

/// Core registrations: the pipe server, the config watcher, the "HP" tab placeholder.
fn core(r: &mut Registry) {
    r.add_service(ServiceSpec { name: "ipc", enabled: |c| c.ipc.enabled, run: crate::ipc_server::run });
    r.add_service(ServiceSpec { name: "config-watch", enabled: |_| true, run: crate::config_watch::run });
    // Moku Link (phone) is deferred. The tab used to show dimmed with a "segera hadir" card; it is **not in the
    // tab bar any more** (`enabled: false`), since sending to a phone lives under Shelf ("Kirim ke HP",
    // `features/phone_send`). The spec stays so the id and its placeholder card exist for the day Moku Link comes.
    r.add_tab(TabSpec {
        id: "phone",
        order: order::PHONE,
        icon: icons::PHONE,
        label: i18n::PHONE_TAB,
        enabled: |_| false,
        build: |_| placeholder(icons::PHONE, i18n::pair(i18n::COMING_SOON), i18n::pair(i18n::PHONE_SUB)),
        tick: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::IpcKind;

    /// Every feature registers without a clash (a duplicate tab / action prefix / IPC handler panics
    /// at startup), and the coding-agent hub and the notification mirror are really wired in.
    #[test]
    fn all_features_register_and_the_agent_hub_and_the_mirror_are_wired() {
        let mut r = Registry::default();
        register_all(&mut r);
        for prefix in ["agents", "ai", "notify", "notify_mirror"] {
            assert!(r.actions.contains_key(prefix), "no action handler for {prefix}.*");
        }
        for kind in [IpcKind::Ask, IpcKind::Session, IpcKind::Notify, IpcKind::Update] {
            assert!(r.ipc.contains_key(&kind), "no IPC handler for {kind:?}");
        }
        assert!(r.services.iter().any(|s| s.name == "notify-mirror"), "the mirror's service is not registered");
        // Off by default: the listener only starts when the user switches the mirror on.
        let cfg = moku_core::Config::default();
        let svc = r.services.iter().find(|s| s.name == "notify-mirror").unwrap();
        assert!(!(svc.enabled)(&cfg));
        let mut on = cfg.clone();
        on.notify_mirror.enabled = true;
        if std::env::var_os("MOKU_NOTIFY_MIRROR_SAMPLE").is_none() {
            assert!((svc.enabled)(&on), "switching it on starts the service");
        }
    }
}
