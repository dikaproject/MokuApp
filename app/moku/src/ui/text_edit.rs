//! The editing model behind [`super::text_input`]: one line of text, a caret, a selection and an
//! IME composition. Pure (no Windows API), so every editing rule is unit-tested.
//!
//! The core owns one `TextEdit` per input id (`App::input`) and feeds it the keyboard while that
//! input has focus (`App::focus_input`); features read it to build their UI and get
//! `ActionKind::Edited` / `ActionKind::Key` actions under the input's id.
//!
//! Indices are byte offsets into `text`, always on `char` boundaries.

/// A key the core passes to an input (from `WM_KEYDOWN`, or `mokuctl debug key`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Backspace,
    Delete,
    Enter,
    Escape,
    Tab,
    /// A letter or digit pressed with Ctrl or Alt (lower case), e.g. Ctrl+A, Alt+1.
    Char(char),
}

/// A key with its modifiers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyPress {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl KeyPress {
    pub const fn plain(key: Key) -> KeyPress {
        KeyPress { key, ctrl: false, shift: false, alt: false }
    }
    pub const fn ctrl(key: Key) -> KeyPress {
        KeyPress { key, ctrl: true, shift: false, alt: false }
    }
    pub const fn shift(key: Key) -> KeyPress {
        KeyPress { key, ctrl: false, shift: true, alt: false }
    }

    /// `"down"`, `"enter"`, `"ctrl+backspace"`, `"shift+left"`, `"alt+1"`, `"ctrl+a"` … (the
    /// `mokuctl debug key` syntax). Case-insensitive.
    pub fn parse(s: &str) -> Option<KeyPress> {
        let parts: Vec<String> = s.split('+').map(|p| p.trim().to_ascii_lowercase()).collect();
        let (name, mods) = parts.split_last()?;
        let mut k = KeyPress::plain(Key::Escape);
        for m in mods {
            match m.as_str() {
                "ctrl" | "control" => k.ctrl = true,
                "shift" => k.shift = true,
                "alt" => k.alt = true,
                _ => return None,
            }
        }
        k.key = match name.as_str() {
            "left" => Key::Left,
            "right" => Key::Right,
            "up" => Key::Up,
            "down" => Key::Down,
            "home" => Key::Home,
            "end" => Key::End,
            "pgup" | "pageup" => Key::PageUp,
            "pgdn" | "pagedown" => Key::PageDown,
            "backspace" | "bksp" => Key::Backspace,
            "delete" | "del" => Key::Delete,
            "enter" | "return" => Key::Enter,
            "esc" | "escape" => Key::Escape,
            "tab" => Key::Tab,
            c if c.chars().count() == 1 && c.chars().all(|c| c.is_ascii_alphanumeric()) && (k.ctrl || k.alt) => Key::Char(c.chars().next()?),
            _ => return None,
        };
        Some(k)
    }
}

/// What a key did to the input.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// The text changed (the owner should react, e.g. search again).
    Changed,
    /// Only the caret / selection moved (redraw).
    Moved,
    /// Not an editing key: the owner gets it (Enter, Esc, ↑/↓, Tab, Alt+1 …).
    Unhandled,
}

/// Longest text an input holds (characters).
pub const MAX_CHARS: usize = 512;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextEdit {
    text: String,
    caret: usize,
    /// Selection anchor; equal to `caret` when nothing is selected.
    anchor: usize,
    /// IME composition string and its cursor (byte index into the composition).
    comp: Option<(String, usize)>,
    /// Set by the core while the input has keyboard focus (draws the caret).
    pub focused: bool,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// One line for a paste: line breaks become single spaces (empty lines dropped), other control
/// characters are removed.
pub fn one_line(s: &str) -> String {
    s.lines().map(|l| l.trim_end_matches('\r')).filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join(" ").chars().filter(|c| !c.is_control()).collect()
}

impl TextEdit {
    pub fn new() -> TextEdit {
        TextEdit::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    /// Selected byte range, if any.
    pub fn selection(&self) -> Option<(usize, usize)> {
        (self.anchor != self.caret).then(|| (self.anchor.min(self.caret), self.anchor.max(self.caret)))
    }

    pub fn composition(&self) -> Option<&str> {
        self.comp.as_ref().map(|(s, _)| s.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.comp.is_none()
    }

    /// Replace the whole text; caret at the end, no selection, no composition.
    pub fn set_text(&mut self, s: &str) {
        self.text = one_line(s).chars().take(MAX_CHARS).collect();
        self.caret = self.text.len();
        self.anchor = self.caret;
        self.comp = None;
    }

    pub fn clear(&mut self) {
        self.set_text("");
    }

    fn prev(&self, i: usize) -> usize {
        self.text[..i].char_indices().next_back().map_or(0, |(j, _)| j)
    }

    fn next(&self, i: usize) -> usize {
        self.text[i..].chars().next().map_or(i, |c| i + c.len_utf8())
    }

    /// Start of the word before `i` (Ctrl+Left / Ctrl+Backspace): skip separators, then the word.
    fn word_left(&self, i: usize) -> usize {
        let mut j = i;
        while j > 0 && !is_word(self.text[..j].chars().next_back().unwrap_or(' ')) {
            j = self.prev(j);
        }
        while j > 0 && is_word(self.text[..j].chars().next_back().unwrap_or(' ')) {
            j = self.prev(j);
        }
        j
    }

    /// Start of the next word after `i` (Ctrl+Right / Ctrl+Delete): skip the word, then separators.
    fn word_right(&self, i: usize) -> usize {
        let mut j = i;
        let at = |j: usize| self.text[j..].chars().next();
        while at(j).is_some_and(is_word) {
            j = self.next(j);
        }
        while at(j).is_some_and(|c| !is_word(c)) {
            j = self.next(j);
        }
        j
    }

    fn delete_selection(&mut self) -> bool {
        match self.selection() {
            Some((a, b)) => {
                self.text.replace_range(a..b, "");
                self.caret = a;
                self.anchor = a;
                true
            }
            None => false,
        }
    }

    fn move_to(&mut self, i: usize, extend: bool) {
        self.caret = i;
        if !extend {
            self.anchor = i;
        }
    }

    /// Type text at the caret (replaces the selection). Control characters are dropped.
    pub fn insert(&mut self, s: &str) -> bool {
        let s: String = s.chars().filter(|c| !c.is_control()).collect();
        let had = self.delete_selection();
        let room = MAX_CHARS.saturating_sub(self.text.chars().count());
        let s: String = s.chars().take(room).collect();
        if s.is_empty() {
            return had;
        }
        self.text.insert_str(self.caret, &s);
        self.caret += s.len();
        self.anchor = self.caret;
        true
    }

    /// Ctrl+V: one line (see [`one_line`]) inserted at the caret.
    pub fn paste(&mut self, s: &str) -> bool {
        self.insert(&one_line(s))
    }

    pub fn backspace(&mut self, word: bool) -> bool {
        if self.delete_selection() {
            return true;
        }
        if self.caret == 0 {
            return false;
        }
        let from = if word { self.word_left(self.caret) } else { self.prev(self.caret) };
        self.text.replace_range(from..self.caret, "");
        self.caret = from;
        self.anchor = from;
        true
    }

    pub fn delete(&mut self, word: bool) -> bool {
        if self.delete_selection() {
            return true;
        }
        if self.caret >= self.text.len() {
            return false;
        }
        let to = if word { self.word_right(self.caret) } else { self.next(self.caret) };
        self.text.replace_range(self.caret..to, "");
        true
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    /// IME: the composition string changed (`cursor` = byte index into `s`). An empty `s` ends it.
    pub fn set_composition(&mut self, s: &str, cursor: usize) {
        if s.is_empty() {
            self.comp = None;
            return;
        }
        self.delete_selection();
        let mut c = cursor.min(s.len());
        while !s.is_char_boundary(c) {
            c -= 1;
        }
        self.comp = Some((s.to_string(), c));
    }

    /// IME: the result string — the composition is replaced by it.
    pub fn commit(&mut self, s: &str) -> bool {
        self.comp = None;
        self.insert(s)
    }

    /// What to draw: the text with the composition spliced in at the caret, the caret position
    /// in it, the selection and the composition range (byte ranges of the returned string).
    pub fn display(&self) -> (String, usize, Option<(usize, usize)>, Option<(usize, usize)>) {
        match &self.comp {
            Some((c, cur)) => {
                let mut s = self.text.clone();
                s.insert_str(self.caret, c);
                (s, self.caret + cur, None, Some((self.caret, self.caret + c.len())))
            }
            None => (self.text.clone(), self.caret, self.selection(), None),
        }
    }

    /// Apply an editing key. Keys that are not about editing are [`Outcome::Unhandled`] (the
    /// owner gets them). Ctrl+V is handled by the core (it reads the clipboard, then [`Self::paste`]).
    pub fn apply(&mut self, k: KeyPress) -> Outcome {
        if self.comp.is_some() {
            // The IME owns the keyboard while composing.
            return Outcome::Unhandled;
        }
        let before = (self.text.len(), self.caret, self.anchor);
        let changed = match k.key {
            Key::Backspace if !k.alt => self.backspace(k.ctrl),
            Key::Delete if !k.alt => self.delete(k.ctrl),
            Key::Left if !k.alt => {
                let to = match (self.selection(), k.shift, k.ctrl) {
                    (Some((a, _)), false, false) => a,
                    (_, _, true) => self.word_left(self.caret),
                    _ => self.prev(self.caret),
                };
                self.move_to(to, k.shift);
                false
            }
            Key::Right if !k.alt => {
                let to = match (self.selection(), k.shift, k.ctrl) {
                    (Some((_, b)), false, false) => b,
                    (_, _, true) => self.word_right(self.caret),
                    _ => self.next(self.caret),
                };
                self.move_to(to, k.shift);
                false
            }
            Key::Home if !k.alt => {
                self.move_to(0, k.shift);
                false
            }
            Key::End if !k.alt => {
                self.move_to(self.text.len(), k.shift);
                false
            }
            Key::Char('a') if k.ctrl && !k.alt => {
                self.select_all();
                false
            }
            _ => return Outcome::Unhandled,
        };
        if changed {
            Outcome::Changed
        } else if (self.text.len(), self.caret, self.anchor) != before {
            Outcome::Moved
        } else {
            // A movement key at the edge still belongs to the input (no fall-through to the owner).
            Outcome::Moved
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(s: &str) -> TextEdit {
        let mut e = TextEdit::new();
        e.set_text(s);
        e
    }

    #[test]
    fn insert_and_caret_moves() {
        let mut e = TextEdit::new();
        assert!(e.insert("vsc"));
        assert_eq!((e.text(), e.caret()), ("vsc", 3));
        assert_eq!(e.apply(KeyPress::plain(Key::Left)), Outcome::Moved);
        e.insert("x");
        assert_eq!(e.text(), "vsxc");
        e.apply(KeyPress::plain(Key::Home));
        assert_eq!(e.caret(), 0);
        e.apply(KeyPress::plain(Key::End));
        assert_eq!(e.caret(), 4);
        // Multi-byte characters: the caret never splits a char.
        let mut e = ed("café");
        e.apply(KeyPress::plain(Key::Left));
        assert_eq!(e.caret(), 3);
        e.apply(KeyPress::plain(Key::Right));
        assert_eq!(e.caret(), "café".len());
        // Control characters are not typed.
        let mut e = TextEdit::new();
        e.insert("a\u{8}b\tc");
        assert_eq!(e.text(), "abc");
    }

    #[test]
    fn delete_and_backspace() {
        let mut e = ed("hello");
        assert_eq!(e.apply(KeyPress::plain(Key::Backspace)), Outcome::Changed);
        assert_eq!(e.text(), "hell");
        e.apply(KeyPress::plain(Key::Home));
        assert_eq!(e.apply(KeyPress::plain(Key::Delete)), Outcome::Changed);
        assert_eq!(e.text(), "ell");
        // At the edges nothing changes, and the key is still the input's.
        assert_eq!(e.apply(KeyPress::plain(Key::Backspace)), Outcome::Moved);
        e.apply(KeyPress::plain(Key::End));
        assert_eq!(e.apply(KeyPress::plain(Key::Delete)), Outcome::Moved);
        assert_eq!(e.text(), "ell");
    }

    #[test]
    fn word_delete_and_word_moves() {
        let mut e = ed("timer 10m  pagi");
        assert_eq!(e.apply(KeyPress::ctrl(Key::Backspace)), Outcome::Changed);
        assert_eq!(e.text(), "timer 10m  ");
        e.apply(KeyPress::ctrl(Key::Backspace));
        assert_eq!(e.text(), "timer ");
        e.set_text("one two three");
        e.apply(KeyPress::plain(Key::Home));
        e.apply(KeyPress::ctrl(Key::Right));
        assert_eq!(e.caret(), 4);
        assert_eq!(e.apply(KeyPress::ctrl(Key::Delete)), Outcome::Changed);
        assert_eq!(e.text(), "one three");
        e.apply(KeyPress::plain(Key::End));
        e.apply(KeyPress::ctrl(Key::Left));
        assert_eq!(e.caret(), 4);
    }

    #[test]
    fn selection_replace_and_select_all() {
        let mut e = ed("hello world");
        e.apply(KeyPress::shift(Key::Left));
        e.apply(KeyPress::shift(Key::Left));
        assert_eq!(e.selection(), Some((9, 11)));
        e.insert("k");
        assert_eq!(e.text(), "hello work");
        assert_eq!(e.apply(KeyPress::ctrl(Key::Char('a'))), Outcome::Moved);
        assert_eq!(e.selection(), Some((0, 10)));
        // Left with a selection collapses to its start.
        e.apply(KeyPress::plain(Key::Left));
        assert_eq!((e.selection(), e.caret()), (None, 0));
        e.select_all();
        assert_eq!(e.apply(KeyPress::plain(Key::Backspace)), Outcome::Changed);
        assert_eq!(e.text(), "");
    }

    #[test]
    fn ime_composition_and_commit() {
        let mut e = ed("ab");
        e.apply(KeyPress::plain(Key::Left));
        e.set_composition("にほ", 3);
        let (s, caret, sel, comp) = e.display();
        assert_eq!(s, "aにほb");
        assert_eq!(caret, 1 + 3);
        assert_eq!((sel, comp), (None, Some((1, 1 + "にほ".len()))));
        // Keys belong to the IME while composing.
        assert_eq!(e.apply(KeyPress::plain(Key::Left)), Outcome::Unhandled);
        assert_eq!(e.text(), "ab");
        assert!(e.commit("日本"));
        assert_eq!((e.text(), e.caret()), ("a日本b", 1 + "日本".len()));
        assert_eq!(e.composition(), None);
        // An empty composition ends it without changing the text.
        e.set_composition("x", 1);
        e.set_composition("", 0);
        assert_eq!(e.text(), "a日本b");
    }

    #[test]
    fn paste_strips_newlines() {
        let mut e = ed("x");
        assert!(e.paste("a\r\nb\n\n c\n"));
        assert_eq!(e.text(), "xa b  c");
        assert_eq!(one_line("\n\nonly\n"), "only");
        let mut e = TextEdit::new();
        e.paste(&"z".repeat(MAX_CHARS + 50));
        assert_eq!(e.text().len(), MAX_CHARS);
    }

    #[test]
    fn owner_keys_are_unhandled_and_parse() {
        let mut e = ed("x");
        for k in [Key::Enter, Key::Escape, Key::Up, Key::Down, Key::Tab, Key::PageDown] {
            assert_eq!(e.apply(KeyPress::plain(k)), Outcome::Unhandled, "{k:?}");
        }
        assert_eq!(e.apply(KeyPress { key: Key::Char('1'), ctrl: false, shift: false, alt: true }), Outcome::Unhandled);
        assert_eq!(KeyPress::parse("down"), Some(KeyPress::plain(Key::Down)));
        assert_eq!(KeyPress::parse("Ctrl+Backspace"), Some(KeyPress::ctrl(Key::Backspace)));
        assert_eq!(KeyPress::parse("ctrl+a"), Some(KeyPress::ctrl(Key::Char('a'))));
        assert_eq!(KeyPress::parse("alt+2").map(|k| (k.key, k.alt)), Some((Key::Char('2'), true)));
        assert_eq!(KeyPress::parse("a"), None, "plain letters are typed, not keys");
        assert_eq!(KeyPress::parse("hyper+x"), None);
    }
}
