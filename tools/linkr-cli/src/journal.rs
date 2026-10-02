//! Serial journal: bounded ring of received device text for the assistant,
//! with control/escape masking. Port of web/serial_journal.js (128 KiB).
//!
//! Cursors count characters (the JS original counts UTF-16 code units); every
//! cursor handed out stays inside this ring's coordinate system, exactly like
//! the slices in the web client.

pub const JOURNAL_CAPACITY: usize = 128 * 1024;
/// `read` default when no limit is given (JS `limit || 12000`).
pub const JOURNAL_DEFAULT_READ: usize = 12000;
/// Hard page ceiling (JS clamps to 1..=16000).
pub const JOURNAL_MAX_READ: usize = 16000;

#[derive(Debug, Clone)]
pub struct JournalRead {
    pub text: String,
    pub start: u64,
    pub cursor: u64,
    pub latest: u64,
    pub truncated: bool,
    pub updated_at: u64,
}

/// Escape-sequence parse state, mirrored from the JS masking state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Control {
    #[default]
    Text,
    Escape,
    Csi,
    Osc,
    Str,
    Intermediate,
}

/// Streaming UTF-8 decoder with the TextDecoder contract: incomplete tails are
/// kept for the next append, invalid bytes become U+FFFD. Shared with
/// `crate::watch`, which needs the same split-across-chunks behaviour.
#[derive(Default)]
pub struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    pub fn decode(&mut self, input: &[u8]) -> String {
        let mut bytes = std::mem::take(&mut self.pending);
        bytes.extend_from_slice(input);
        let mut out = String::new();
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if b < 0x80 {
                out.push(b as char);
                i += 1;
                continue;
            }
            let (len, min) = match b {
                0xc2..=0xdf => (2, 0x80),
                0xe0..=0xef => (3, 0x800),
                0xf0..=0xf4 => (4, 0x1000),
                _ => {
                    out.push('\u{fffd}');
                    i += 1;
                    continue;
                }
            };
            if i + len > bytes.len() {
                // Incomplete tail: keep it for the next chunk.
                self.pending = bytes[i..].to_vec();
                return out;
            }
            let mut ok = true;
            for k in 1..len {
                if bytes[i + k] & 0xc0 != 0x80 {
                    ok = false;
                    break;
                }
            }
            let mut cp = 0u32;
            if ok {
                cp = match len {
                    2 => ((b as u32 & 0x1f) << 6) | (bytes[i + 1] as u32 & 0x3f),
                    3 => {
                        ((b as u32 & 0x0f) << 12)
                            | ((bytes[i + 1] as u32 & 0x3f) << 6)
                            | (bytes[i + 2] as u32 & 0x3f)
                    }
                    _ => {
                        ((b as u32 & 0x07) << 18)
                            | ((bytes[i + 1] as u32 & 0x3f) << 12)
                            | ((bytes[i + 2] as u32 & 0x3f) << 6)
                            | (bytes[i + 3] as u32 & 0x3f)
                    }
                };
                ok = cp >= min && !(0xd800..=0xdfff).contains(&cp) && cp <= 0x10ffff;
            }
            match ok.then(|| char::from_u32(cp)).flatten() {
                Some(ch) => {
                    out.push(ch);
                    i += len;
                }
                None => {
                    out.push('\u{fffd}');
                    i += 1;
                }
            }
        }
        out
    }
}

#[derive(Default)]
pub struct SerialJournal {
    capacity: usize,
    parts: Vec<String>,
    head: usize,
    length: usize,
    end: u64,
    updated_at: u64,
    control: Control,
    decoder: Utf8Decoder,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl SerialJournal {
    pub fn new() -> Self {
        Self {
            capacity: JOURNAL_CAPACITY,
            ..Self::default()
        }
    }

    /// Same as `new` with a smaller ring (tests).
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            ..Self::default()
        }
    }

    pub fn append_bytes(&mut self, bytes: &[u8]) {
        let text = self.decoder.decode(bytes);
        let visible = self.mask(&text);
        self.end += text.chars().count() as u64;
        if !visible.is_empty() {
            self.parts.push(visible);
            self.length += text.chars().count();
            self.evict();
        }
        self.updated_at = now_ms();
    }

    /// Mask controls as they arrive, before paging or ring eviction can split a
    /// sequence. One placeholder per character preserves all cursors.
    fn mask(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for ch in text.chars() {
            let code = ch as u32;
            let mut keep = false;
            if ch == '\x18' || ch == '\x1a' {
                self.control = Control::Text;
            } else if ch == '\x1b' {
                self.control = Control::Escape;
            } else if matches!(self.control, Control::Osc | Control::Str) {
                if code == 0x9c || (self.control == Control::Osc && ch == '\x07') {
                    self.control = Control::Text;
                }
            } else if code < 0x20 || code == 0x7f {
                keep = matches!(ch, '\t' | '\r' | '\n');
            } else if code == 0x9b {
                self.control = Control::Csi;
            } else if code == 0x9d {
                self.control = Control::Osc;
            } else if matches!(code, 0x90 | 0x98 | 0x9e | 0x9f) {
                self.control = Control::Str;
            } else if (0x80..=0x9f).contains(&code) {
                self.control = Control::Text;
            } else if self.control == Control::Escape {
                if ch == '[' {
                    self.control = Control::Csi;
                } else if ch == ']' {
                    self.control = Control::Osc;
                } else if matches!(ch, 'P' | 'X' | '^' | '_') {
                    self.control = Control::Str;
                } else if (0x20..=0x2f).contains(&code) {
                    self.control = Control::Intermediate;
                } else {
                    self.control = Control::Text;
                }
            } else if self.control == Control::Csi {
                if (0x40..=0x7e).contains(&code) {
                    self.control = Control::Text;
                }
            } else if self.control == Control::Intermediate {
                if (0x30..=0x7e).contains(&code) {
                    self.control = Control::Text;
                }
            } else {
                keep = true;
            }
            out.push(if keep { ch } else { '\0' });
        }
        out
    }

    /// Drop from the head until the window fits: whole chunks first, then the
    /// head of the chunk that straddles the limit. `excess` counts the oldest
    /// characters to lose, so the surviving window keeps its tail.
    fn evict(&mut self) {
        while self.length > self.capacity {
            if self.head >= self.parts.len() {
                self.length = 0;
                break;
            }
            let head_len = self.parts[self.head].chars().count();
            let excess = self.length - self.capacity;
            if head_len <= excess {
                self.length -= head_len;
                self.parts[self.head] = String::new();
                self.head += 1;
            } else {
                self.parts[self.head] = skip_chars(&self.parts[self.head], excess);
                self.length -= excess;
            }
            if self.head > 256 {
                self.parts.drain(..self.head);
                self.head = 0;
            }
        }
    }

    /// A page is collected from the tail of the ring, so a read costs the page
    /// and not the whole window. `from` is an offset into the retained text;
    /// `need` counts back to it, so the head chunk contributes only its tail and
    /// the first char of the result sits exactly at `from` — anything past
    /// `count` is trimmed from the end after.
    fn page(&self, from: usize, count: usize) -> String {
        if count == 0 {
            return String::new();
        }
        let need = match self.length.checked_sub(from) {
            Some(n) => n,
            None => return String::new(),
        };
        if need == 0 {
            return String::new();
        }
        let mut out = String::new();
        let mut remaining = need;
        let mut i = self.parts.len();
        while i > self.head && remaining > 0 {
            i -= 1;
            let part = &self.parts[i];
            if part.is_empty() {
                continue;
            }
            let part_len = part.chars().count();
            let take = if part_len > remaining {
                skip_chars(part, part_len - remaining)
            } else {
                part.clone()
            };
            remaining -= take.chars().count();
            out = format!("{take}{out}");
        }
        take_chars(&out, count)
    }

    pub fn reset(&mut self) {
        self.parts.clear();
        self.head = 0;
        self.length = 0;
        self.end = 0;
        self.updated_at = 0;
        self.control = Control::Text;
        self.decoder = Utf8Decoder::default();
    }

    /// Page text after cursor `after` (default: oldest retained), at most
    /// `limit` chars clamped to 1..=16000.
    pub fn read(&self, after: Option<u64>, limit: usize) -> JournalRead {
        let limit = if limit == 0 {
            JOURNAL_DEFAULT_READ
        } else {
            limit.clamp(1, JOURNAL_MAX_READ)
        };
        let oldest = self.end.saturating_sub(self.length as u64);
        let requested = match after {
            Some(v) => v,
            None => self.end.saturating_sub(limit as u64),
        };
        let start = self.end.min(requested.max(oldest));
        let end = self.end.min(start + limit as u64);
        let mut text = self.page((start - oldest) as usize, (end - start) as usize);
        text = text.replace('\0', "");
        JournalRead {
            text,
            start,
            cursor: end,
            latest: self.end,
            truncated: requested < oldest,
            updated_at: self.updated_at,
        }
    }

    pub fn latest_cursor(&self) -> u64 {
        self.end
    }

    pub fn len(&self) -> usize {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
}

/// Drop `n` characters from the front of a string.
fn skip_chars(text: &str, n: usize) -> String {
    text.char_indices()
        .nth(n)
        .map_or_else(String::new, |(idx, _)| text[idx..].to_string())
}

/// Keep the first `n` characters of a string.
fn take_chars(text: &str, n: usize) -> String {
    match text.char_indices().nth(n) {
        Some((idx, _)) => text[..idx].to_string(),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(j: &SerialJournal) -> String {
        j.read(None, JOURNAL_MAX_READ).text
    }

    #[test]
    fn appends_and_reads_plain_text() {
        let mut j = SerialJournal::new();
        j.append_bytes(b"hello");
        let page = j.read(None, 16000);
        assert_eq!(page.text, "hello");
        assert_eq!(page.start, 0);
        assert_eq!(page.cursor, 5);
        assert_eq!(page.latest, 5);
        assert!(!page.truncated);
        assert_eq!(j.len(), 5);
        assert!(!j.is_empty());
    }

    #[test]
    fn keeps_tab_cr_lf_and_masks_other_controls() {
        let mut j = SerialJournal::new();
        j.append_bytes(b"a\tb\r\nc\x07d\x7f");
        // Masking stores `\0` placeholders so every cursor keeps its position;
        // the page handed to the caller drops them again (serial_journal.js).
        assert_eq!(j.len(), 9);
        assert_eq!(read_all(&j), "a\tb\r\ncd");
    }

    #[test]
    fn masks_csi_escape_sequences() {
        let mut j = SerialJournal::new();
        j.append_bytes(b"ok\x1b[31mred\x1b[0m");
        // The escapes cost one character each, so cursors still line up.
        assert_eq!(j.len(), 14);
        assert_eq!(read_all(&j), "okred");
        assert_eq!(j.read(None, 14).cursor, 14);
    }

    #[test]
    fn masks_osc_until_bel_and_st() {
        let mut j = SerialJournal::new();
        j.append_bytes(b"a\x1b]0;title\x07b");
        assert_eq!(read_all(&j), "ab");
        let mut j = SerialJournal::new();
        j.append_bytes(b"x\x1b]8;;http://a\x1b\\y");
        let text = read_all(&j);
        assert_eq!(text, "xy");
        assert!(!text.contains('8'));
        assert!(!text.contains("http"));
    }

    #[test]
    fn can_resets_escape_state() {
        let mut j = SerialJournal::new();
        // CAN (\x18) aborts a CSI sequence; following text stays visible.
        j.append_bytes(b"\x1b[31\x18ok");
        assert_eq!(j.len(), 7);
        assert_eq!(read_all(&j), "ok");
    }

    #[test]
    fn c1_controls_start_sequences() {
        // A C1 control arrives as U+009B (two UTF-8 bytes), which starts a CSI.
        let mut j = SerialJournal::new();
        j.append_bytes(b"a\xc2\x9b1mb");
        assert_eq!(j.len(), 5);
        assert_eq!(read_all(&j), "ab");
        // A lone 0x9b byte is not valid UTF-8 and becomes U+FFFD, a printable
        // character: the mask only sees what the decoder produced.
        let mut j = SerialJournal::new();
        j.append_bytes(&[b'a', 0x9b, b'1', b'm', b'b']);
        assert_eq!(read_all(&j), "a\u{fffd}1mb");
    }

    #[test]
    fn ring_evicts_oldest_and_reports_truncation() {
        let mut j = SerialJournal::with_capacity(8);
        j.append_bytes(b"abcdefghij");
        assert_eq!(j.len(), 8);
        let page = j.read(None, 100);
        assert_eq!(page.text, "cdefghij");
        assert_eq!(page.start, 2);
        assert_eq!(page.latest, 10);
        assert!(page.truncated);
    }

    #[test]
    fn read_limit_is_clamped() {
        let mut j = SerialJournal::new();
        j.append_bytes(b"0123456789");
        assert_eq!(j.read(None, 0).text.len(), 10); // default 12000 > journal
        assert_eq!(j.read(None, 99999).text.len(), 10);
        // No cursor: the page ends at the newest character (serial_journal.js).
        assert_eq!(j.read(None, 4).text, "6789");
        assert_eq!(j.read(Some(6), 3).text, "678");
        assert_eq!(j.read(Some(6), 3).cursor, 9);
        // 0 clamps to the default, a huge limit clamps to 16000.
        assert_eq!(j.read(Some(0), 0).text, "0123456789");
    }

    #[test]
    fn after_cursor_before_oldest_is_truncated() {
        let mut j = SerialJournal::with_capacity(4);
        j.append_bytes(b"abcdef");
        let page = j.read(Some(1), 2);
        assert!(page.truncated);
        assert_eq!(page.start, 2); // clamped to oldest retained
        assert_eq!(page.text, "cd");
    }

    #[test]
    fn streaming_utf8_survives_split_sequences() {
        let mut j = SerialJournal::new();
        j.append_bytes(&[0xc3]);
        assert_eq!(read_all(&j), "");
        j.append_bytes(&[0xa9]);
        assert_eq!(read_all(&j), "\u{e9}");
        // Invalid byte becomes the replacement character.
        j.append_bytes(&[0xff]);
        assert!(read_all(&j).contains('\u{fffd}'));
    }

    #[test]
    fn reset_clears_everything() {
        let mut j = SerialJournal::new();
        j.append_bytes(b"abc");
        j.reset();
        assert!(j.is_empty());
        assert_eq!(j.latest_cursor(), 0);
        let page = j.read(None, 100);
        assert_eq!(page.text, "");
        assert_eq!(page.updated_at, 0);
    }

    #[test]
    fn masks_do_not_disturb_cursors() {
        let mut j = SerialJournal::new();
        j.append_bytes(b"\x1b[2Jstart");
        // Every input character contributes exactly one cursor unit.
        assert_eq!(j.latest_cursor(), 9);
        let page = j.read(None, 16000);
        assert_eq!(page.cursor, 9);
        assert!(page.text.ends_with("start"));
    }
}
