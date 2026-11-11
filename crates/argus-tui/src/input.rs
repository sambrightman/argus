//! A single-line editor with readline/emacs bindings.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineEditor {
    text: Vec<char>,
    cursor: usize,
    /// Last killed text, for `C-y`.
    kill: Vec<char>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    Changed,
    Moved,
    Submit,
    Cancel,
    Ignored,
}

impl LineEditor {
    pub fn new(s: &str) -> Self {
        let text: Vec<char> = s.chars().collect();
        Self {
            cursor: text.len(),
            text,
            kill: vec![],
        }
    }

    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn handle(&mut self, k: KeyEvent) -> Edit {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        match k.code {
            KeyCode::Enter => Edit::Submit,
            KeyCode::Esc => Edit::Cancel,
            KeyCode::Char('g') if ctrl => Edit::Cancel,
            KeyCode::Char('a') if ctrl => self.go(0),
            KeyCode::Home => self.go(0),
            KeyCode::Char('e') if ctrl => self.go(self.text.len()),
            KeyCode::End => self.go(self.text.len()),
            KeyCode::Char('b') if ctrl => self.go(self.cursor.saturating_sub(1)),
            KeyCode::Left if !alt && !ctrl => self.go(self.cursor.saturating_sub(1)),
            KeyCode::Char('f') if ctrl => self.go((self.cursor + 1).min(self.text.len())),
            KeyCode::Right if !alt && !ctrl => self.go((self.cursor + 1).min(self.text.len())),
            KeyCode::Char('b') if alt => self.go(self.word_left()),
            KeyCode::Left => self.go(self.word_left()),
            KeyCode::Char('f') if alt => self.go(self.word_right()),
            KeyCode::Right => self.go(self.word_right()),
            KeyCode::Backspace if alt || ctrl => self.kill_range(self.word_left(), self.cursor),
            KeyCode::Char('w') if ctrl => self.kill_range(self.word_left(), self.cursor),
            KeyCode::Char('d') if alt => self.kill_range(self.cursor, self.word_right()),
            KeyCode::Backspace | KeyCode::Char('h') if k.code == KeyCode::Backspace || ctrl => {
                if self.cursor == 0 {
                    return Edit::Ignored;
                }
                self.cursor -= 1;
                self.text.remove(self.cursor);
                Edit::Changed
            }
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Char('d') if ctrl => self.delete_forward(),
            KeyCode::Char('k') if ctrl => self.kill_range(self.cursor, self.text.len()),
            KeyCode::Char('u') if ctrl => self.kill_range(0, self.cursor),
            KeyCode::Char('y') if ctrl => {
                let k = self.kill.clone();
                for (i, c) in k.iter().enumerate() {
                    self.text.insert(self.cursor + i, *c);
                }
                self.cursor += k.len();
                if k.is_empty() {
                    Edit::Ignored
                } else {
                    Edit::Changed
                }
            }
            KeyCode::Char('t') if ctrl => {
                // transpose chars before point (at end: last two)
                let n = self.text.len();
                if n < 2 || self.cursor == 0 {
                    return Edit::Ignored;
                }
                let i = if self.cursor == n { n - 1 } else { self.cursor };
                self.text.swap(i - 1, i);
                self.cursor = (i + 1).min(n);
                Edit::Changed
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                self.text.insert(self.cursor, c);
                self.cursor += 1;
                Edit::Changed
            }
            _ => Edit::Ignored,
        }
    }

    fn go(&mut self, to: usize) -> Edit {
        if to == self.cursor {
            return Edit::Ignored;
        }
        self.cursor = to;
        Edit::Moved
    }

    fn delete_forward(&mut self) -> Edit {
        if self.cursor >= self.text.len() {
            return Edit::Ignored;
        }
        self.text.remove(self.cursor);
        Edit::Changed
    }

    fn kill_range(&mut self, from: usize, to: usize) -> Edit {
        if from >= to {
            return Edit::Ignored;
        }
        self.kill = self.text.drain(from..to).collect();
        self.cursor = from;
        Edit::Changed
    }

    fn word_left(&self) -> usize {
        let mut i = self.cursor;
        while i > 0 && !self.text[i - 1].is_alphanumeric() {
            i -= 1;
        }
        while i > 0 && self.text[i - 1].is_alphanumeric() {
            i -= 1;
        }
        i
    }

    fn word_right(&self) -> usize {
        let n = self.text.len();
        let mut i = self.cursor;
        while i < n && !self.text[i].is_alphanumeric() {
            i += 1;
        }
        while i < n && self.text[i].is_alphanumeric() {
            i += 1;
        }
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(c, m)
    }
    fn ch(c: char) -> KeyEvent {
        key(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        key(KeyCode::Char(c), KeyModifiers::CONTROL)
    }
    fn alt(c: char) -> KeyEvent {
        key(KeyCode::Char(c), KeyModifiers::ALT)
    }
    fn typed(s: &str) -> LineEditor {
        let mut e = LineEditor::default();
        for c in s.chars() {
            e.handle(ch(c));
        }
        e
    }

    #[test]
    fn typing_and_motion() {
        let mut e = typed("hello world");
        assert_eq!(e.cursor(), 11);
        e.handle(ctrl('a'));
        assert_eq!(e.cursor(), 0);
        e.handle(alt('f'));
        assert_eq!(e.cursor(), 5);
        e.handle(alt('f'));
        assert_eq!(e.cursor(), 11);
        e.handle(alt('b'));
        assert_eq!(e.cursor(), 6);
        e.handle(ctrl('b'));
        e.handle(ch('!'));
        assert_eq!(e.text(), "hello! world");
    }

    #[test]
    fn kill_and_yank() {
        let mut e = typed("foo bar baz");
        e.handle(ctrl('w'));
        assert_eq!(e.text(), "foo bar ");
        e.handle(ctrl('a'));
        e.handle(ctrl('y'));
        assert_eq!(e.text(), "bazfoo bar ");
        e.handle(ctrl('k'));
        assert_eq!(e.text(), "baz");
        e.handle(ctrl('u'));
        assert_eq!(e.text(), "");
        assert_eq!(e.handle(ctrl('u')), Edit::Ignored, "nothing to kill");
    }

    #[test]
    fn deletion_at_edges_is_ignored() {
        let mut e = LineEditor::default();
        assert_eq!(
            e.handle(key(KeyCode::Backspace, KeyModifiers::NONE)),
            Edit::Ignored
        );
        assert_eq!(e.handle(ctrl('d')), Edit::Ignored);
        let mut e = typed("ab");
        assert_eq!(e.handle(ctrl('d')), Edit::Ignored, "at end");
        e.handle(ctrl('h'));
        assert_eq!(e.text(), "a");
    }

    #[test]
    fn transpose_and_unicode() {
        let mut e = typed("añb");
        e.handle(ctrl('t'));
        assert_eq!(e.text(), "abñ");
        let mut e = typed("x");
        assert_eq!(e.handle(ctrl('t')), Edit::Ignored);
    }

    fn plain(c: KeyCode) -> KeyEvent {
        key(c, KeyModifiers::NONE)
    }

    #[test]
    fn arrows_move_a_char_and_with_a_modifier_a_word() {
        let mut e = typed("hello world");
        assert_eq!(e.handle(plain(KeyCode::Left)), Edit::Moved);
        assert_eq!(e.cursor(), 10);
        e.handle(plain(KeyCode::Home));
        assert_eq!(e.cursor(), 0);
        e.handle(plain(KeyCode::Right));
        assert_eq!(e.cursor(), 1);
        e.handle(ctrl('f'));
        assert_eq!(e.cursor(), 2);
        e.handle(plain(KeyCode::End));
        assert_eq!(e.cursor(), 11);
        assert_eq!(e.handle(plain(KeyCode::Right)), Edit::Ignored, "at end");
        for m in [KeyModifiers::ALT, KeyModifiers::CONTROL] {
            e.handle(ctrl('a'));
            e.handle(key(KeyCode::Right, m));
            assert_eq!(e.cursor(), 5, "{m:?}");
            e.handle(ctrl('e'));
            e.handle(key(KeyCode::Left, m));
            assert_eq!(e.cursor(), 6, "{m:?}");
        }
    }

    #[test]
    fn word_motion_skips_separators_and_stops_at_the_ends() {
        let mut e = typed("foo, bar  ");
        e.handle(alt('b'));
        assert_eq!(e.cursor(), 5, "back over spaces, then the word");
        e.handle(alt('b'));
        assert_eq!(e.cursor(), 0, "back over \", \", then to the start");
        assert_eq!(e.handle(alt('b')), Edit::Ignored, "at the start");
        e.handle(alt('f'));
        e.handle(alt('f'));
        assert_eq!(e.cursor(), 8);
        e.handle(alt('f'));
        assert_eq!(e.cursor(), 10, "over the trailing spaces");
        assert_eq!(e.handle(alt('f')), Edit::Ignored, "at the end");
    }

    #[test]
    fn deleting_words_and_chars() {
        for m in [KeyModifiers::ALT, KeyModifiers::CONTROL] {
            let mut e = typed("foo bar");
            e.handle(key(KeyCode::Backspace, m));
            assert_eq!(e.text(), "foo ", "{m:?}");
        }
        let mut e = typed("foo bar");
        e.handle(plain(KeyCode::Backspace));
        assert_eq!(e.text(), "foo ba", "plain backspace: one char");
        e.handle(ctrl('a'));
        e.handle(alt('d'));
        assert_eq!(e.text(), " ba");
        e.handle(plain(KeyCode::Delete));
        assert_eq!(e.text(), "ba");
        e.handle(ctrl('d'));
        assert_eq!(e.text(), "a");
    }

    #[test]
    fn letters_bound_with_ctrl_type_plainly() {
        assert_eq!(typed("kayak").text(), "kayak");
        let mut e = typed("x");
        assert_eq!(e.handle(alt('y')), Edit::Ignored, "alt keys don't insert");
        assert_eq!(e.text(), "x");
    }

    #[test]
    fn transpose_two_chars_and_in_the_middle() {
        let mut e = typed("ab");
        e.handle(ctrl('t'));
        assert_eq!(e.text(), "ba");
        let mut e = typed("abc");
        e.handle(ctrl('a'));
        e.handle(ctrl('f'));
        e.handle(ctrl('t'));
        assert_eq!(e.text(), "bac");
        assert_eq!(e.cursor(), 2, "after the moved char");
    }

    #[test]
    fn submit_and_cancel() {
        let mut e = typed("x");
        assert_eq!(
            e.handle(key(KeyCode::Enter, KeyModifiers::NONE)),
            Edit::Submit
        );
        assert_eq!(e.handle(ctrl('g')), Edit::Cancel);
        assert_eq!(
            e.handle(key(KeyCode::Esc, KeyModifiers::NONE)),
            Edit::Cancel
        );
        assert_eq!(
            e.handle(ctrl('z')),
            Edit::Ignored,
            "unbound ctrl keys don't insert"
        );
    }
}
