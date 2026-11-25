//! v0.3 track Q: the coding-agent hub and the notification mirror blocks of the Notifikasi page
//! (`ui/pages-q.slint`, globals `AgentsCfg` and `MirrorCfg`).
//!
//! They live outside `Form`: this module reads and writes `[agents]` and `[notify_mirror]` of
//! `config.toml` with the same read-modify-write as every other save (`Config::update_file`), and
//! the "always allow" rules in `agent-rules.json` with `moku_core::agent_rules::update_file`. The
//! island re-reads the rules at its next permission card, so a removal here is in force at once.
//! Rules that appeared in the file without Moku writing them are listed apart ("Diubah di luar
//! Moku") and only act after "Percayai" here.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use moku_core::Config;
use moku_core::agent_rules::{self, AGENTS, Mode, Rule};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{AgentRule, AgentsCfg, MirrorApp, MirrorCfg, SettingsWindow};

/// Longest app name on the allowlist, and most apps.
const APP_NAME_MAX: usize = 60;
const APPS_MAX: usize = 40;

/// The settings behind the controls, without any window: the logic the tests drive.
pub struct Model {
    config: PathBuf,
    rules: PathBuf,
    /// The config as we last read or wrote it (the fallback when the file is unreadable for a moment).
    last: Config,
}

impl Model {
    pub fn open(config: PathBuf, rules: PathBuf) -> Model {
        let last = Config::load_from(&config).0;
        Model { config, rules, last }
    }

    pub fn reload(&mut self) {
        self.last = Config::load_from(&self.config).0;
    }

    pub fn config(&self) -> &Config {
        &self.last
    }

    fn edit(&mut self, change: impl FnOnce(&mut Config)) -> Result<(), String> {
        let cfg = Config::update_file(&self.config, &self.last, change).map_err(|e| format!("Gagal menyimpan: {e}"))?;
        self.last = cfg;
        Ok(())
    }

    pub fn set_codex(&mut self, on: bool) -> Result<(), String> {
        self.edit(|c| c.agents.codex = on)
    }

    pub fn set_gemini(&mut self, on: bool) -> Result<(), String> {
        self.edit(|c| c.agents.gemini = on)
    }

    pub fn set_always_allow(&mut self, on: bool) -> Result<(), String> {
        self.edit(|c| c.agents.always_allow = on)
    }

    pub fn set_mirror(&mut self, on: bool) -> Result<(), String> {
        self.edit(|c| c.notify_mirror.enabled = on)
    }

    pub fn set_mirror_pill(&mut self, on: bool) -> Result<(), String> {
        self.edit(|c| c.notify_mirror.pill = on)
    }

    // ---- "always allow" rules ------------------------------------------------------------------

    /// The rules that act, in the order they were added.
    pub fn rules(&self) -> Vec<Rule> {
        agent_rules::load(&self.rules).iter().cloned().collect()
    }

    /// The rules in the file that Moku did not write: ignored until [`Model::accept_pending`].
    pub fn pending_rules(&self) -> Vec<Rule> {
        agent_rules::review(&self.rules).pending.iter().cloned().collect()
    }

    /// Add a rule typed by hand for agent number `agent` (index into [`AGENTS`]): exactly that
    /// command. The checks are the core's: one plain command line, no shell or interpreter, no
    /// program that runs other programs.
    pub fn add_rule(&mut self, agent: usize, command: &str) -> Result<(), String> {
        let agent = AGENTS.get(agent).ok_or("Agen tidak dikenal.")?;
        let (_, result) = agent_rules::update_file(&self.rules, |r| r.add(agent, command)).map_err(|e| format!("Gagal menyimpan: {e}"))?;
        result.map(|_| ()).map_err(|e| e.message().to_string())
    }

    /// Remove the active rule number `index` (as listed by [`Model::rules`]).
    pub fn remove_rule(&mut self, index: usize) -> Result<(), String> {
        let Some(rule) = self.rules().get(index).cloned() else { return Ok(()) };
        agent_rules::update_file(&self.rules, |r| {
            let at = r.iter().position(|x| *x == rule);
            if let Some(i) = at {
                r.remove(i);
            }
        })
        .map(|_| ())
        .map_err(|e| format!("Gagal menyimpan: {e}"))
    }

    /// "Percayai": the rules edited in outside Moku act from now on.
    pub fn accept_pending(&mut self) -> Result<(), String> {
        agent_rules::accept_pending(&self.rules).map(|_| ()).map_err(|e| format!("Gagal menyimpan: {e}"))
    }

    /// "Buang": delete the rules edited in outside Moku.
    pub fn discard_pending(&mut self) -> Result<(), String> {
        agent_rules::discard_pending(&self.rules).map(|_| ()).map_err(|e| format!("Gagal menyimpan: {e}"))
    }

    // ---- the notification mirror's allowlist -----------------------------------------------------

    pub fn apps(&self) -> &[String] {
        &self.last.notify_mirror.apps
    }

    pub fn add_app(&mut self, input: &str) -> Result<(), String> {
        let name = input.split_whitespace().collect::<Vec<_>>().join(" ");
        if name.is_empty() {
            return Err("Tulis nama aplikasinya, misalnya WhatsApp.".into());
        }
        if name.chars().count() > APP_NAME_MAX {
            return Err("Nama terlalu panjang.".into());
        }
        if name.chars().any(|c| c.is_control()) {
            return Err("Nama tidak boleh berisi karakter kontrol.".into());
        }
        if self.last.notify_mirror.apps.iter().any(|a| a.trim().to_lowercase() == name.to_lowercase()) {
            return Err("Aplikasi itu sudah ada di daftar.".into());
        }
        if self.last.notify_mirror.apps.len() >= APPS_MAX {
            return Err("Daftar penuh. Hapus yang tidak dipakai.".into());
        }
        self.edit(|c| c.notify_mirror.apps.push(name))
    }

    pub fn remove_app(&mut self, index: usize) -> Result<(), String> {
        self.edit(|c| {
            if index < c.notify_mirror.apps.len() {
                c.notify_mirror.apps.remove(index);
            }
        })
    }
}

fn show_agents(ui: &SettingsWindow, m: &Model) {
    let g = ui.global::<AgentsCfg>();
    let c = m.config();
    g.set_codex(c.agents.codex);
    g.set_gemini(c.agents.gemini);
    g.set_always_allow(c.agents.always_allow);
    let row = |r: Rule| AgentRule { agent: r.agent.into(), command: r.command.into(), prefix_mode: r.mode == Mode::Prefix };
    g.set_rules(ModelRc::new(VecModel::from(m.rules().into_iter().map(row).collect::<Vec<_>>())));
    g.set_pending(ModelRc::new(VecModel::from(m.pending_rules().into_iter().map(row).collect::<Vec<_>>())));
}

fn show_mirror(ui: &SettingsWindow, m: &Model) {
    let g = ui.global::<MirrorCfg>();
    let c = m.config();
    g.set_enabled(c.notify_mirror.enabled);
    g.set_pill(c.notify_mirror.pill);
    let rows: Vec<MirrorApp> = m.apps().iter().map(|a| MirrorApp { name: a.clone().into() }).collect();
    g.set_apps(ModelRc::new(VecModel::from(rows)));
}

/// Connect the controls to `config.toml` and `agent-rules.json` (the real ones, or `$MOKU_DATA_DIR`'s).
pub fn wire(ui: &SettingsWindow, config: PathBuf) {
    let model = Rc::new(RefCell::new(Model::open(config, agent_rules::path())));
    show_agents(ui, &model.borrow());
    show_mirror(ui, &model.borrow());

    // Run an edit, then refresh the window; a failure shows under the field it belongs to.
    let agents = |model: &Rc<RefCell<Model>>, ui: &slint::Weak<SettingsWindow>, f: &dyn Fn(&mut Model) -> Result<(), String>| -> bool {
        let r = f(&mut model.borrow_mut());
        let Some(ui) = ui.upgrade() else { return r.is_ok() };
        ui.global::<AgentsCfg>().set_problem(r.as_ref().err().map(|e| e.as_str()).unwrap_or("").into());
        show_agents(&ui, &model.borrow());
        r.is_ok()
    };
    let mirror = |model: &Rc<RefCell<Model>>, ui: &slint::Weak<SettingsWindow>, f: &dyn Fn(&mut Model) -> Result<(), String>| -> bool {
        let r = f(&mut model.borrow_mut());
        let Some(ui) = ui.upgrade() else { return r.is_ok() };
        ui.global::<MirrorCfg>().set_problem(r.as_ref().err().map(|e| e.as_str()).unwrap_or("").into());
        show_mirror(&ui, &model.borrow());
        r.is_ok()
    };

    let a = ui.global::<AgentsCfg>();
    a.on_reload({
        let (model, ui) = (model.clone(), ui.as_weak());
        move || {
            model.borrow_mut().reload();
            if let Some(ui) = ui.upgrade() {
                ui.global::<AgentsCfg>().set_problem("".into());
                show_agents(&ui, &model.borrow());
            }
        }
    });
    a.on_set_codex({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |on| {
            agents(&model, &ui, &|m| m.set_codex(on));
        }
    });
    a.on_set_gemini({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |on| {
            agents(&model, &ui, &|m| m.set_gemini(on));
        }
    });
    a.on_set_always_allow({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |on| {
            agents(&model, &ui, &|m| m.set_always_allow(on));
        }
    });
    a.on_add_rule({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |agent, text| agents(&model, &ui, &|m| m.add_rule(agent.max(0) as usize, text.as_str()))
    });
    a.on_remove_rule({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |i| {
            agents(&model, &ui, &|m| m.remove_rule(i.max(0) as usize));
        }
    });
    a.on_accept_pending({
        let (model, ui) = (model.clone(), ui.as_weak());
        move || {
            agents(&model, &ui, &|m| m.accept_pending());
        }
    });
    a.on_discard_pending({
        let (model, ui) = (model.clone(), ui.as_weak());
        move || {
            agents(&model, &ui, &|m| m.discard_pending());
        }
    });

    let g = ui.global::<MirrorCfg>();
    g.on_reload({
        let (model, ui) = (model.clone(), ui.as_weak());
        move || {
            model.borrow_mut().reload();
            if let Some(ui) = ui.upgrade() {
                ui.global::<MirrorCfg>().set_problem("".into());
                show_mirror(&ui, &model.borrow());
            }
        }
    });
    g.on_set_enabled({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |on| {
            mirror(&model, &ui, &|m| m.set_mirror(on));
        }
    });
    g.on_set_pill({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |on| {
            mirror(&model, &ui, &|m| m.set_mirror_pill(on));
        }
    });
    g.on_add_app({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |text| mirror(&model, &ui, &|m| m.add_app(text.as_str()))
    });
    g.on_remove_app({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |i| {
            mirror(&model, &ui, &|m| m.remove_app(i.max(0) as usize));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_model(name: &str) -> (Model, PathBuf, PathBuf) {
        let d = std::env::temp_dir().join(format!("moku-settings-q-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let (c, r) = (d.join("config.toml"), d.join("agent-rules.json"));
        (Model::open(c.clone(), r.clone()), c, r)
    }

    #[test]
    fn agent_switches_save_only_their_own_keys() {
        let (mut m, path, _) = temp_model("switches");
        assert_eq!((m.config().agents.codex, m.config().agents.always_allow), (true, false), "always allow is opt-in");
        // Another writer's value survives.
        let mut other = Config::default();
        other.island.scale = 1.25;
        other.save_to(&path).unwrap();
        m.set_codex(false).unwrap();
        m.set_always_allow(true).unwrap();
        let disk = Config::load_from(&path).0;
        assert_eq!((disk.agents.codex, disk.agents.gemini, disk.agents.always_allow), (false, true, true));
        assert_eq!(disk.island.scale, 1.25);
        // The mirror: off by default, switched on and its pill off, nothing else touched.
        assert!(!m.config().notify_mirror.enabled);
        m.set_mirror(true).unwrap();
        m.set_mirror_pill(false).unwrap();
        let disk = Config::load_from(&path).0;
        assert_eq!((disk.notify_mirror.enabled, disk.notify_mirror.pill), (true, false));
        assert!(!disk.agents.codex);
    }

    #[test]
    fn rules_are_added_with_the_cores_checks_and_removed() {
        let (mut m, _, rules) = temp_model("rules");
        assert!(m.rules().is_empty());
        m.add_rule(0, "  npm   test ").unwrap();
        m.add_rule(1, "cargo build").unwrap();
        let list = m.rules();
        assert_eq!(list.iter().map(|r| (r.agent.as_str(), r.command.as_str())).collect::<Vec<_>>(), [("claude", "npm test"), ("codex", "cargo build")]);
        // The file is the core's format: the island reads exactly this, and a rule is that command only.
        assert!(agent_rules::load(&rules).allows("claude", "npm test").is_some());
        assert!(agent_rules::load(&rules).allows("claude", "npm test --watch").is_none());
        assert!(agent_rules::load(&rules).allows("claude", "npm test && rm -rf /").is_none());
        // Refused, each with a message, nothing saved.
        for (agent, text, part) in [(0, "", "Tulis"), (0, "npm test && rm -rf /", "gabungan"), (0, "bash -c", "luas"), (0, "find . -delete", "luas"), (0, "npm test", "sudah ada"), (7, "npm test", "tidak dikenal")] {
            let e = m.add_rule(agent, text).unwrap_err();
            assert!(e.contains(part), "{text:?}: {e}");
        }
        assert_eq!(m.rules().len(), 2);
        m.remove_rule(0).unwrap();
        assert_eq!(m.rules().iter().map(|r| r.command.clone()).collect::<Vec<_>>(), ["cargo build"]);
        // A stale index changes nothing.
        m.remove_rule(9).unwrap();
        assert_eq!(m.rules().len(), 1);
        assert!(agent_rules::load(&rules).allows("claude", "npm test").is_none(), "removed rules stop acting");
    }

    #[test]
    fn rules_edited_in_outside_moku_wait_for_a_decision() {
        let (mut m, _, rules) = temp_model("pending");
        m.add_rule(0, "npm test").unwrap();
        // A program adds a rule to the file.
        let mut edited = agent_rules::load(&rules);
        edited.add("claude", "git status").unwrap();
        std::fs::write(&rules, edited.to_json()).unwrap();
        assert_eq!(m.rules().len(), 1);
        assert_eq!(m.pending_rules().iter().map(|r| r.command.as_str()).collect::<Vec<_>>(), ["git status"]);
        assert!(agent_rules::load(&rules).allows("claude", "git status").is_none());
        // Removing an active rule by its listed number still removes that one while another waits.
        m.add_rule(1, "cargo test").unwrap();
        m.remove_rule(0).unwrap();
        assert_eq!(m.rules().iter().map(|r| r.command.as_str()).collect::<Vec<_>>(), ["cargo test"]);
        assert_eq!(m.pending_rules().len(), 1, "the pending rule is still waiting");
        // "Percayai".
        m.accept_pending().unwrap();
        assert!(m.pending_rules().is_empty());
        assert!(agent_rules::load(&rules).allows("claude", "git status").is_some());
        // "Buang": a second outside edit is thrown away.
        let mut edited = agent_rules::load(&rules);
        edited.add("claude", "git log").unwrap();
        std::fs::write(&rules, edited.to_json()).unwrap();
        assert_eq!(m.pending_rules().len(), 1);
        m.discard_pending().unwrap();
        assert!(m.pending_rules().is_empty());
        assert!(agent_rules::load(&rules).allows("claude", "git log").is_none());
        assert_eq!(m.rules().len(), 2);
    }

    #[test]
    fn the_notification_allowlist_is_edited_as_names() {
        let (mut m, path, _) = temp_model("apps");
        assert!(m.apps().iter().any(|a| a == "WhatsApp"), "the defaults");
        m.add_app("  Signal   Desktop ").unwrap();
        assert!(Config::load_from(&path).0.notify_mirror.apps.iter().any(|a| a == "Signal Desktop"));
        for (input, part) in [("", "Tulis"), ("   ", "Tulis"), ("whatsapp", "sudah ada"), ("Signal desktop", "sudah ada"), (&"x".repeat(80), "panjang"), ("a\u{7}b", "kontrol")] {
            let e = m.add_app(input).unwrap_err();
            assert!(e.contains(part), "{input:?}: {e}");
        }
        let n = m.apps().len();
        m.remove_app(0).unwrap();
        assert_eq!(m.apps().len(), n - 1);
        m.remove_app(500).unwrap();
        assert_eq!(m.apps().len(), n - 1);
        // The list is bounded.
        for i in 0..APPS_MAX {
            let _ = m.add_app(&format!("App {i}"));
        }
        assert_eq!(m.apps().len(), APPS_MAX);
        assert!(m.add_app("One more").unwrap_err().contains("penuh"));
    }
}
