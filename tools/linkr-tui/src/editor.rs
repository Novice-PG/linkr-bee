//! Small, Unicode-aware editing shared by the assistant's text fields.
//! Positions are byte boundaries; movement and deletion operate on graphemes.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor(Option<usize>);

pub struct Window {
    pub before: String,
    pub caret: String,
    pub after: String,
    start: usize,
    marker: usize,
}

impl Cursor {
    /// Place the caret in the visible horizontal window without splitting a grapheme.
    pub fn click(&mut self, text: &str, width: usize, column: usize) {
        let window = self.window(text, width);
        let column = column.min(width).saturating_sub(window.marker);
        let mut used = 0;
        let mut at = window.start;
        for (index, glyph) in text[window.start..].grapheme_indices(true) {
            if used + glyph.width() > column {
                break;
            }
            used += glyph.width();
            at = window.start + index + glyph.len();
        }
        self.0 = Some(at);
    }

    pub fn at(byte: usize) -> Self {
        Self(Some(byte))
    }

    pub fn end(&mut self) {
        self.0 = None;
    }

    pub fn position(&self, text: &str) -> usize {
        let wanted = self.0.unwrap_or(text.len()).min(text.len());
        text.grapheme_indices(true)
            .map(|(i, _)| i)
            .chain([text.len()])
            .take_while(|i| *i <= wanted)
            .last()
            .unwrap_or(0)
    }

    pub fn insert(&mut self, text: &mut String, value: &str) {
        let at = self.position(text);
        text.insert_str(at, value);
        self.0 = Some(at + value.len());
    }

    pub fn handle(&mut self, text: &mut String, key: KeyEvent) -> bool {
        let at = self.position(text);
        let previous = text[..at]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i);
        let next = text[at..]
            .graphemes(true)
            .next()
            .map_or(at, |g| at + g.len());
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, ctrl) {
            (KeyCode::Left, false) => self.0 = Some(previous),
            (KeyCode::Right, false) => self.0 = Some(next),
            (KeyCode::Home, _) | (KeyCode::Char('a'), true) => self.0 = Some(0),
            (KeyCode::End, _) | (KeyCode::Char('e'), true) => self.end(),
            (KeyCode::Backspace, _) => {
                text.replace_range(previous..at, "");
                self.0 = Some(previous);
            }
            (KeyCode::Delete, _) => {
                text.replace_range(at..next, "");
                self.0 = Some(at);
            }
            (KeyCode::Char('u'), true) => {
                text.replace_range(..at, "");
                self.0 = Some(0);
            }
            (KeyCode::Char('k'), true) => {
                text.truncate(at);
                self.end();
            }
            (KeyCode::Char('w'), true) => {
                let start = text[..at]
                    .trim_end()
                    .char_indices()
                    .rev()
                    .find(|(_, ch)| ch.is_whitespace())
                    .map_or(0, |(i, ch)| i + ch.len_utf8());
                text.replace_range(start..at, "");
                self.0 = Some(start);
            }
            (KeyCode::Char(ch), false) => self.insert(text, &ch.to_string()),
            _ => return false,
        }
        true
    }

    /// Keep the insertion point on screen, including while editing long URLs.
    pub fn window(&self, text: &str, width: usize) -> Window {
        let at = self.position(text);
        if width == 0 {
            return Window {
                before: String::new(),
                caret: String::new(),
                after: String::new(),
                start: at,
                marker: 0,
            };
        }
        let glyph = text[at..].graphemes(true).next().unwrap_or(" ");
        let caret = if glyph.width() <= width { glyph } else { " " }.to_string();
        let budget = width.saturating_sub(caret.width());
        let mut start = at;
        for (i, _) in text[..at].grapheme_indices(true).rev() {
            if text[i..at].width() + usize::from(i > 0) > budget {
                break;
            }
            start = i;
        }
        let before = format!(
            "{}{}",
            if start > 0 && budget > 0 { "<" } else { "" },
            &text[start..at]
        );
        let mut remaining = width.saturating_sub(before.width() + caret.width());
        let mut after = String::new();
        if at < text.len() {
            for g in text[at + glyph.len()..].graphemes(true) {
                if g.width() > remaining {
                    break;
                }
                after.push_str(g);
                remaining -= g.width();
            }
        }
        Window {
            before,
            caret,
            after,
            start,
            marker: usize::from(start > 0 && budget > 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn clicks_follow_the_visible_window_and_keep_unicode_intact() {
        let mut cursor = Cursor::at(0);
        let mut text = "中e\u{301}👩‍💻abc".to_string();
        cursor.click(&text, 20, 3);
        cursor.insert(&mut text, "X");
        assert_eq!(text, "中e\u{301}X👩‍💻abc");
        cursor.end();
        let text = "0123456789TAIL";
        let start = cursor.window(text, 6).start;
        assert!(start > 0);
        cursor.click(text, 6, 1);
        assert_eq!(cursor.position(text), start);
        cursor.click(text, 6, 6);
        assert!(text.is_char_boundary(cursor.position(text)));
    }

    #[test]
    fn inserts_and_deletes_at_the_caret() {
        let mut c = Cursor::default();
        let mut t = "abc".to_string();
        c.handle(&mut t, key(KeyCode::Left));
        c.handle(&mut t, key(KeyCode::Char('X')));
        assert_eq!(t, "abXc");
        c.handle(&mut t, key(KeyCode::Delete));
        assert_eq!(t, "abX");
        c.handle(&mut t, key(KeyCode::Home));
        c.insert(&mut t, "中");
        assert_eq!(t, "中abX");
    }

    #[test]
    fn grapheme_deletion_preserves_unicode_boundaries() {
        let mut c = Cursor::default();
        let mut t = "中e\u{301}👩‍💻".to_string();
        c.handle(&mut t, key(KeyCode::Backspace));
        assert_eq!(t, "中e\u{301}");
        c.handle(&mut t, key(KeyCode::Backspace));
        assert_eq!(t, "中");
        c.handle(&mut t, key(KeyCode::Left));
        c.handle(&mut t, key(KeyCode::Delete));
        assert!(t.is_empty());
    }

    #[test]
    fn viewport_follows_the_caret_and_fits_wide_characters() {
        let text = "0123456789中文TAIL";
        let mut c = Cursor::default();
        for width in 1..20 {
            let v = c.window(text, width);
            assert!(v.before.width() + v.caret.width() + v.after.width() <= width);
        }
        assert!(c.window(text, 10).before.ends_with("TAIL"));
        c.handle(&mut text.to_string(), key(KeyCode::Home));
        assert_eq!(c.window(text, 10).caret, "0");
    }
}
