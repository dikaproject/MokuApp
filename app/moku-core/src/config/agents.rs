section! {
    /// `[agents]` the coding-agent hub (v0.3, track Q): Codex CLI and Gemini CLI next to Claude Code
    /// (`[claude]` keeps the Claude switches). Status comes from `mokuctl agent-hook`, which needs `[ipc]`.
    pub struct Agents in "agents" {
        /// Session pills for OpenAI Codex CLI (`mokuctl agent-hook --agent codex`).
        codex: bool = true,
        /// Session pills for Gemini CLI (`mokuctl agent-hook --agent gemini`).
        gemini: bool = true,
        /// Apply the "always allow this command" rules and offer "Selalu izinkan perintah ini" on
        /// shell-command permission cards. Off by default (opt-in): a rule approves exactly the
        /// command it was made from, never other arguments. Rules are listed in Settings and live
        /// in `agent-rules.json`; with this off none of them acts and the card does not offer one.
        always_allow: bool = false,
    }
}
