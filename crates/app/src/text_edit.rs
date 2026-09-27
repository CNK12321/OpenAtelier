//! Editing a title where it's drawn: a cursor and a selection over the real, rendered
//! text (effects and all), no text box on top. This is the editing itself — keys, typing,
//! copy, cut and paste applied to the text — kept apart from the drawing (`viewer.rs`)
//! so it can be tested. Positions are character indices into the title's text.

use eframe::egui::{Event, Key};

/// The cursor, and where the selection started (the same place: nothing selected).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caret {
    pub cursor: usize,
    pub anchor: usize,
}

impl Caret {
    pub fn at(i: usize) -> Self {
        Caret { cursor: i, anchor: i }
    }

    pub fn all(text: &str) -> Self {
        Caret { cursor: text.chars().count(), anchor: 0 }
    }

    /// The selected characters, `start..end`.
    pub fn range(&self) -> (usize, usize) {
        (self.cursor.min(self.anchor), self.cursor.max(self.anchor))
    }

    pub fn is_empty(&self) -> bool {
        self.cursor == self.anchor
    }
}

/// What an event did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// The text changed.
    pub changed: bool,
    /// Text for the clipboard (copy or cut).
    pub copied: Option<String>,
}

/// The word around character `i` (for a double-click).
pub fn word_at(text: &str, i: usize) -> Caret {
    let chars: Vec<char> = text.chars().collect();
    let i = i.min(chars.len());
    let kind = |c: char| if c.is_alphanumeric() || c == '_' || c == '\'' { 1 } else if c.is_whitespace() { 0 } else { 2 };
    let Some(&c) = chars.get(i).or_else(|| i.checked_sub(1).and_then(|j| chars.get(j))) else { return Caret::at(i) };
    let k = kind(c);
    let mut a = i.min(chars.len().saturating_sub(1));
    while a > 0 && kind(chars[a - 1]) == k {
        a -= 1;
    }
    let mut b = i;
    while b < chars.len() && kind(chars[b]) == k {
        b += 1;
    }
    Caret { anchor: a, cursor: b.max(a) }
}

/// Applies one input event to `text` at `caret`. `vertical(i, down)` is where the cursor
/// goes from `i` on the line above or below (the layout knows; `None` at the top or
/// bottom). Events it doesn't handle are left alone.
pub fn apply(text: &mut String, caret: &mut Caret, event: &Event, vertical: &dyn Fn(usize, bool) -> Option<usize>) -> Outcome {
    let mut chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    caret.cursor = caret.cursor.min(len);
    caret.anchor = caret.anchor.min(len);
    let mut out = Outcome::default();
    let selected = |c: &Caret, chars: &[char]| -> String {
        let (a, b) = c.range();
        chars[a..b].iter().collect()
    };
    // Replaces the selection with `s`.
    let insert = |chars: &mut Vec<char>, caret: &mut Caret, s: &str| {
        let (a, b) = caret.range();
        chars.splice(a..b, s.chars());
        *caret = Caret::at(a + s.chars().count());
    };
    match event {
        Event::Text(s) => {
            let s: String = s.chars().filter(|c| !c.is_control()).collect();
            if !s.is_empty() {
                insert(&mut chars, caret, &s);
                out.changed = true;
            }
        }
        Event::Paste(s) => {
            let s = s.replace("\r\n", "\n").replace('\r', "\n");
            insert(&mut chars, caret, &s);
            out.changed = true;
        }
        Event::Copy => {
            if !caret.is_empty() {
                out.copied = Some(selected(caret, &chars));
            }
        }
        Event::Cut => {
            if !caret.is_empty() {
                out.copied = Some(selected(caret, &chars));
                insert(&mut chars, caret, "");
                out.changed = true;
            }
        }
        Event::Key { key, pressed: true, modifiers, .. } => {
            let (shift, word) = (modifiers.shift, modifiers.command || modifiers.alt);
            let line_start = |i: usize| chars[..i].iter().rposition(|c| *c == '\n').map_or(0, |p| p + 1);
            let line_end = |i: usize| chars[i..].iter().position(|c| *c == '\n').map_or(len, |p| i + p);
            let prev_word = |mut i: usize| {
                while i > 0 && chars[i - 1].is_whitespace() {
                    i -= 1;
                }
                while i > 0 && !chars[i - 1].is_whitespace() {
                    i -= 1;
                }
                i
            };
            let next_word = |mut i: usize| {
                while i < len && chars[i].is_whitespace() {
                    i += 1;
                }
                while i < len && !chars[i].is_whitespace() {
                    i += 1;
                }
                i
            };
            let move_to = |caret: &mut Caret, to: usize| {
                caret.cursor = to;
                if !shift {
                    caret.anchor = to;
                }
            };
            let (a, b) = caret.range();
            match key {
                // Without Shift, an arrow first collapses the selection to its side.
                Key::ArrowLeft if !shift && !caret.is_empty() && !word => move_to(caret, a),
                Key::ArrowRight if !shift && !caret.is_empty() && !word => move_to(caret, b),
                Key::ArrowLeft => move_to(caret, if word { prev_word(caret.cursor) } else { caret.cursor.saturating_sub(1) }),
                Key::ArrowRight => move_to(caret, if word { next_word(caret.cursor) } else { (caret.cursor + 1).min(len) }),
                Key::ArrowUp => {
                    let to = vertical(caret.cursor, false).unwrap_or(0);
                    move_to(caret, to);
                }
                Key::ArrowDown => {
                    let to = vertical(caret.cursor, true).unwrap_or(len);
                    move_to(caret, to);
                }
                Key::Home => move_to(caret, if modifiers.command { 0 } else { line_start(caret.cursor) }),
                Key::End => move_to(caret, if modifiers.command { len } else { line_end(caret.cursor) }),
                Key::A if modifiers.command => *caret = Caret { anchor: 0, cursor: len },
                Key::Backspace => {
                    if caret.is_empty() && caret.cursor > 0 {
                        caret.anchor = if word { prev_word(caret.cursor) } else { caret.cursor - 1 };
                    }
                    if !caret.is_empty() {
                        insert(&mut chars, caret, "");
                        out.changed = true;
                    }
                }
                Key::Delete => {
                    if caret.is_empty() && caret.cursor < len {
                        caret.anchor = if word { next_word(caret.cursor) } else { caret.cursor + 1 };
                    }
                    if !caret.is_empty() {
                        insert(&mut chars, caret, "");
                        out.changed = true;
                    }
                }
                // A new line (Ctrl+Enter finishes instead; the viewer handles that).
                Key::Enter if !modifiers.command => {
                    insert(&mut chars, caret, "\n");
                    out.changed = true;
                }
                _ => {}
            }
        }
        _ => {}
    }
    if out.changed {
        *text = chars.into_iter().collect();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Modifiers;

    fn key(key: Key, modifiers: Modifiers) -> Event {
        Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    fn run(text: &str, caret: Caret, events: &[Event]) -> (String, Caret, Vec<String>) {
        let (mut text, mut caret, mut copied) = (text.to_string(), caret, Vec::new());
        // Lines as the layout would answer: same column on the line above/below.
        for e in events {
            let snapshot = text.clone();
            let vertical = move |i: usize, down: bool| -> Option<usize> {
                let starts: Vec<usize> = std::iter::once(0).chain(snapshot.chars().enumerate().filter(|(_, c)| *c == '\n').map(|(k, _)| k + 1)).collect();
                let line = starts.iter().rposition(|s| *s <= i)?;
                let col = i - starts[line];
                let to = if down { line + 1 } else { line.checked_sub(1)? };
                let start = *starts.get(to)?;
                let end = starts.get(to + 1).map_or(snapshot.chars().count(), |s| s - 1);
                Some((start + col).min(end))
            };
            let out = apply(&mut text, &mut caret, e, &vertical);
            copied.extend(out.copied);
        }
        (text, caret, copied)
    }

    #[test]
    fn typing_replaces_the_selection_and_moves_on() {
        let (t, c, _) = run("Hello", Caret::all("Hello"), &[Event::Text("Hi".into()), Event::Text(" there".into())]);
        assert_eq!((t.as_str(), c), ("Hi there", Caret::at(8)));
        // Accents and emoji are one character each.
        let (t, c, _) = run("né", Caret::at(2), &[Event::Text("🎬".into()), key(Key::Backspace, Modifiers::NONE), key(Key::Backspace, Modifiers::NONE)]);
        assert_eq!((t.as_str(), c), ("n", Caret::at(1)));
    }

    #[test]
    fn keys_move_select_and_delete() {
        let none = Modifiers::NONE;
        let (t, c, _) = run("one two three", Caret::at(13), &[key(Key::ArrowLeft, Modifiers::COMMAND), key(Key::ArrowLeft, Modifiers::SHIFT | Modifiers::COMMAND)]);
        assert_eq!((t.as_str(), c.range()), ("one two three", (4, 8)));
        let (t, _, _) = run("one two three", Caret::at(7), &[key(Key::Backspace, Modifiers::COMMAND)]);
        assert_eq!(t, "one  three");
        let (t, c, _) = run("ab\ncd", Caret::at(1), &[key(Key::ArrowDown, none), key(Key::Delete, none), key(Key::Home, none), key(Key::Enter, none)]);
        assert_eq!((t.as_str(), c), ("ab\n\nc", Caret::at(4)));
        let (_, c, _) = run("ab\ncd", Caret::at(5), &[key(Key::ArrowUp, Modifiers::SHIFT)]);
        assert_eq!(c.range(), (2, 5));
        let (_, c, _) = run("abc", Caret { anchor: 0, cursor: 2 }, &[key(Key::ArrowLeft, none)]);
        assert_eq!(c, Caret::at(0), "an arrow collapses a selection to its side");
        let (_, c, _) = run("abc", Caret::at(1), &[key(Key::A, Modifiers::COMMAND)]);
        assert_eq!(c.range(), (0, 3));
    }

    #[test]
    fn copy_cut_and_paste() {
        let (t, c, copied) = run("Hello world", Caret { anchor: 6, cursor: 11 }, &[Event::Copy, Event::Cut, Event::Paste("big\r\nworld".into())]);
        assert_eq!(copied, ["world", "world"]);
        assert_eq!((t.as_str(), c), ("Hello big\nworld", Caret::at(15)));
        // Nothing selected: nothing to copy, and cut does nothing.
        let (t, _, copied) = run("abc", Caret::at(1), &[Event::Copy, Event::Cut]);
        assert!(copied.is_empty());
        assert_eq!(t, "abc");
    }

    #[test]
    fn double_click_takes_the_word() {
        assert_eq!(word_at("say hello there", 6).range(), (4, 9));
        assert_eq!(word_at("say hello", 9).range(), (4, 9), "at the end: the last word");
        assert_eq!(word_at("a  b", 2).range(), (1, 3), "between words: the space");
    }
}
