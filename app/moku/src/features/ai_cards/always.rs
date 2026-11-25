//! "Selalu izinkan perintah ini": what the island does with the rules of
//! `moku_core::agent_rules` (pure logic and the rules cache, no UI).
//!
//! * [`auto_allow`]: a card that arrives is answered `approve` at once, without a card, only when
//!   **every** condition holds: the feature is on (it is off by default), the ask says it is a
//!   shell command (`kind`) of a known agent, and a rule the user created for that agent in Moku
//!   matches the command exactly (one plain command line). Anything else gets its card as before.
//!   A rule that appeared in the file without Moku writing it is pending and does not act.
//! * [`offer`]: the rule the card offers to create for a command ("Selalu izinkan perintah ini",
//!   with the words it would cover), or nothing.
//! * [`Cache`]: the rules file is read once and again only when the file changed (checked with
//!   one `stat` per file per ask; the settings app edits the same file), and tells when rules
//!   edited outside Moku are waiting for the user ([`Cache::news`]).
//!
//! Moku never writes to Claude Code's, Codex's or Gemini's own settings.

use std::path::Path;
use std::time::SystemTime;

use moku_core::agent_rules::{self, Review, Rules};

/// The only kind of command a rule applies to.
const SHELL: &str = "shell";

fn known_agent(agent: Option<&str>) -> Option<&str> {
    agent.filter(|a| agent_rules::AGENTS.contains(a))
}

/// The prefix of the rule that allows this ask without a card, if there is one.
pub fn auto_allow(enabled: bool, kind: Option<&str>, agent: Option<&str>, command: Option<&str>, rules: &Rules) -> Option<String> {
    if !enabled || kind != Some(SHELL) {
        return None;
    }
    rules.allows(known_agent(agent)?, command?).map(|r| r.command.clone())
}

/// The prefix the card offers as a new rule (`None`: no button).
pub fn offer(enabled: bool, kind: Option<&str>, agent: Option<&str>, command: Option<&str>) -> Option<String> {
    if !enabled || kind != Some(SHELL) {
        return None;
    }
    known_agent(agent)?;
    agent_rules::suggest(command?)
}

/// What a file looked like when it was read: modification time and length.
type Stamp = Option<(Option<SystemTime>, u64)>;

fn stamp_of(path: &Path) -> Stamp {
    std::fs::metadata(path).ok().map(|m| (m.modified().ok(), m.len()))
}

/// The rules, re-read only when the file (or Moku's copy of it) changed.
#[derive(Default)]
pub struct Cache {
    review: Review,
    stamp: (Stamp, Stamp),
    loaded: bool,
    /// The pending rules the user was already told about.
    announced: Rules,
}

impl Cache {
    fn refresh(&mut self, path: &Path) {
        let now = (stamp_of(path), stamp_of(&agent_rules::trusted_path(path)));
        if !self.loaded || now != self.stamp {
            self.review = agent_rules::review(path);
            self.stamp = now;
            self.loaded = true;
        }
    }

    /// The rules that act, as the file has them now. Two `stat`s, and a read only when one
    /// changed (or on the first call). A missing or damaged file is no rules; rules edited outside
    /// Moku (pending) are not among them.
    pub fn current(&mut self, path: &Path) -> &Rules {
        self.refresh(path);
        &self.review.active
    }

    /// True once for each new set of pending rules (the file was edited outside Moku): the caller
    /// shows "Aturan izin diubah di luar Moku". Reads the file like [`Cache::current`].
    pub fn news(&mut self, path: &Path) -> bool {
        self.refresh(path);
        if self.review.pending.is_empty() {
            self.announced = Rules::default();
            false
        } else if self.review.pending != self.announced {
            self.announced = self.review.pending.clone();
            true
        } else {
            false
        }
    }

    /// Forget what was read ("Hapus semua data").
    pub fn clear(&mut self) {
        *self = Cache::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(list: &[(&str, &str)]) -> Rules {
        let mut r = Rules::default();
        for (a, p) in list {
            r.add(a, p).unwrap();
        }
        r
    }

    #[test]
    fn nothing_is_auto_allowed_unless_a_rule_lists_it() {
        let r = rules(&[("claude", "npm test")]);
        let ask = |en, kind, agent, cmd| auto_allow(en, kind, agent, cmd, &r);
        // The listed command of the listed agent, from a shell-command card: allowed.
        assert_eq!(ask(true, Some("shell"), Some("claude"), Some("npm test")).as_deref(), Some("npm test"));
        // Exactly that command: not with an argument (the review's `--script-shell` case) ...
        assert_eq!(ask(true, Some("shell"), Some("claude"), Some("npm test --script-shell=./x.cmd")), None);
        assert_eq!(ask(true, Some("shell"), Some("claude"), Some("npm test -- --watch")), None);
        // No rules at all: nothing, whatever the card is.
        assert_eq!(auto_allow(true, Some("shell"), Some("claude"), Some("npm test"), &Rules::default()), None);
        // Not listed.
        assert_eq!(ask(true, Some("shell"), Some("claude"), Some("npm install")), None);
        assert_eq!(ask(true, Some("shell"), Some("claude"), Some("npm testing")), None);
        assert_eq!(ask(true, Some("shell"), Some("claude"), Some("npm test && rm -rf /")), None);
        // Listed for another agent.
        assert_eq!(ask(true, Some("shell"), Some("codex"), Some("npm test")), None);
        // The feature is off (the default): every rule is dormant.
        assert_eq!(ask(false, Some("shell"), Some("claude"), Some("npm test")), None);
        // The card does not say it is a shell command (an old client, a file path, a URL).
        assert_eq!(ask(true, None, Some("claude"), Some("npm test")), None);
        assert_eq!(ask(true, Some("file"), Some("claude"), Some("npm test")), None);
        // No agent, an unknown agent, no command.
        assert_eq!(ask(true, Some("shell"), None, Some("npm test")), None);
        assert_eq!(ask(true, Some("shell"), Some("vim"), Some("npm test")), None);
        assert_eq!(ask(true, Some("shell"), Some("claude"), None), None);
    }

    #[test]
    fn the_card_offers_a_rule_only_for_a_plain_shell_command_of_a_known_agent() {
        let o = |en, kind, agent, cmd| offer(en, kind, agent, cmd);
        assert_eq!(o(true, Some("shell"), Some("codex"), Some("git status --short")).as_deref(), Some("git status --short"));
        assert_eq!(o(true, Some("shell"), Some("claude"), Some("npm run build")).as_deref(), Some("npm run build"));
        assert_eq!(o(false, Some("shell"), Some("codex"), Some("git status")), None);
        assert_eq!(o(true, None, Some("codex"), Some("git status")), None);
        assert_eq!(o(true, Some("shell"), None, Some("git status")), None);
        assert_eq!(o(true, Some("shell"), Some("codex"), Some("git status && git push")), None);
        assert_eq!(o(true, Some("shell"), Some("codex"), Some("bash -c x")), None);
        assert_eq!(o(true, Some("shell"), Some("codex"), Some("find . -delete")), None);
        assert_eq!(o(true, Some("shell"), Some("codex"), None), None);
    }

    #[test]
    fn the_cache_rereads_only_a_changed_file() {
        let dir = std::env::temp_dir().join(format!("moku-ai-always-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("agent-rules.json");
        let mut c = Cache::default();
        // No file: no rules, and still none while it stays missing.
        assert!(c.current(&p).is_empty());
        assert!(c.current(&p).is_empty());
        // The island (or the settings app) writes a rule: the next ask sees it.
        agent_rules::update_file(&p, |r| r.add("claude", "npm test")).unwrap().1.unwrap();
        assert_eq!(c.current(&p).len(), 1);
        assert!(c.current(&p).allows("claude", "npm test").is_some());
        // The settings app removes it: gone at the next ask.
        agent_rules::update_file(&p, |r| r.remove(0)).unwrap();
        assert!(c.current(&p).is_empty());
        // The file is deleted ("Hapus semua data"): no rules.
        agent_rules::update_file(&p, |r| r.add("codex", "cargo test")).unwrap().1.unwrap();
        assert_eq!(c.current(&p).len(), 1);
        std::fs::remove_file(&p).unwrap();
        assert!(c.current(&p).is_empty());
        // A damaged file is no rules (never a panic, never everything allowed).
        std::fs::write(&p, "{ not json").unwrap();
        assert!(c.current(&p).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rule_edited_in_from_outside_is_ignored_and_announced_once() {
        let dir = std::env::temp_dir().join(format!("moku-ai-always-outside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("agent-rules.json");
        let mut c = Cache::default();
        assert!(!c.news(&p), "no file, nothing to say");
        agent_rules::update_file(&p, |r| r.add("claude", "npm test")).unwrap().1.unwrap();
        assert!(!c.news(&p), "Moku's own write is not news");
        assert!(c.current(&p).allows("claude", "npm test").is_some());
        // Another program adds a rule to the file.
        let mut edited = agent_rules::load(&p);
        edited.add("claude", "git status").unwrap();
        // Make sure the length changes (the cache compares the modification time and the length).
        std::fs::write(&p, edited.to_json()).unwrap();
        assert!(c.news(&p), "the first look at the new rule");
        assert!(!c.news(&p), "the same pending rule is announced once");
        assert!(c.current(&p).allows("claude", "git status").is_none(), "ignored");
        assert!(c.current(&p).allows("claude", "npm test").is_some());
        // The user confirms in Settings: it acts, nothing more to say.
        agent_rules::accept_pending(&p).unwrap();
        assert!(c.current(&p).allows("claude", "git status").is_some());
        assert!(!c.news(&p));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
