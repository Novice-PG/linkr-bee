//! Keyboard encoding for the terminal pane — byte parity with
//! `web/terminal_keys.js` (WEB_UX_SPEC section 2.4).
//!
//! The web client's accessory bar (`terminalKeySequence`, `controlInput`,
//! `applyInputModifiers`) produces raw byte sequences for the target shell.
//! The TUI must send exactly the same bytes for the same logical key so that
//! scripts behave identically whether the user clicks a key bar button in the
//! browser or presses a key in the terminal UI.

use crossterm::event::{KeyCode, KeyModifiers};

use super::settings::EnterMode;

/// One-shot modifier latch, mirroring the web key bar's
/// `shiftPending`/`controlPending`/`altPending` state: armed for exactly one
/// key press, then reset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StickyMods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl StickyMods {
    pub fn is_empty(self) -> bool {
        !self.shift && !self.ctrl && !self.alt
    }

    pub fn toggle_ctrl(mut self) -> Self {
        self.ctrl = !self.ctrl;
        self
    }
}

/// Modifier parameter for CSI sequences: `1 + shift(1) + alt(2) + ctrl(4)`.
/// Exactly the arithmetic in `terminalKeySequence`.
pub fn modifier_param(shift: bool, alt: bool, ctrl: bool) -> u8 {
    1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl)
}

/// Normal-to-shifted ASCII map from `applyInputModifiers`.
pub const SHIFT_NORMAL: &str = "`1234567890-=[]\\;',./";
/// Shifted counterpart of [`SHIFT_NORMAL`] (same order).
pub const SHIFT_SHIFTED: &str = "~!@#$%^&*()_+{}|:\"<>?";

/// Port of `controlInput`: a single character to its control byte.
/// `space` → NUL, `?` → DEL, `@_` range masked with `&0x1f`.
pub fn control_input(data: char) -> char {
    let mut code = data as u32;
    if (0x61..=0x7a).contains(&code) {
        code -= 0x20;
    }
    match data {
        ' ' => '\0',
        '?' => '\x7f',
        _ if (0x40..=0x5f).contains(&code) => char::from_u32(code & 0x1f).unwrap_or(data),
        _ => data,
    }
}

/// Port of `applyInputModifiers`: shift map first, then control byte, then an
/// Alt (ESC) prefix. Only single ASCII keystrokes are targeted.
pub fn apply_input_modifiers(data: char, mods: StickyMods) -> String {
    if data as u32 > 0x7f {
        return data.to_string();
    }
    let mut out = data;
    if mods.shift {
        out = if let Some(index) = SHIFT_NORMAL.find(data) {
            SHIFT_SHIFTED.chars().nth(index).unwrap_or(data)
        } else {
            data.to_ascii_uppercase()
        };
    }
    if mods.ctrl {
        out = control_input(out);
    }
    let mut text = out.to_string();
    if mods.alt {
        text.insert(0, '\x1b');
    }
    text
}

fn crossterm_to_sticky(mods: KeyModifiers) -> StickyMods {
    StickyMods {
        shift: mods.contains(KeyModifiers::SHIFT),
        ctrl: mods.contains(KeyModifiers::CONTROL),
        alt: mods.contains(KeyModifiers::ALT),
    }
}

/// Encode one crossterm key press into the bytes the target shell should see.
///
/// `app_cursor` is the DECCKM state tracked by the VT parser: it switches
/// cursor keys and Home/End between `ESC [ x` and `ESC O x`. `sticky` carries
/// the key bar's one-shot modifiers. Returns `None` when the key is not a
/// terminal key (the caller decides whether it is a TUI binding instead).
pub fn encode_key(
    code: KeyCode,
    modifiers: KeyModifiers,
    app_cursor: bool,
    sticky: StickyMods,
) -> Option<Vec<u8>> {
    let physical = crossterm_to_sticky(modifiers);
    let shift = physical.shift || sticky.shift;
    let alt = physical.alt || sticky.alt;
    let ctrl = physical.ctrl || sticky.ctrl;
    let modifier = modifier_param(shift, alt, ctrl);
    let pressed = |base: StickyMods, key: StickyMods| StickyMods {
        shift: base.shift || key.shift,
        ctrl: base.ctrl || key.ctrl,
        alt: base.alt || key.alt,
    };
    let _ = pressed;

    // Cursor keys (and Home/End) share the CSI 1;<mod> form.
    const CURSOR: [(KeyCode, char); 6] = [
        (KeyCode::Up, 'A'),
        (KeyCode::Down, 'B'),
        (KeyCode::Right, 'C'),
        (KeyCode::Left, 'D'),
        (KeyCode::Home, 'H'),
        (KeyCode::End, 'F'),
    ];
    for (want, letter) in CURSOR {
        if code == want {
            if modifier > 1 {
                return Some(format!("\x1b[1;{modifier}{letter}").into_bytes());
            }
            let lead = if app_cursor { 'O' } else { '[' };
            return Some(format!("\x1b{lead}{letter}").into_bytes());
        }
    }

    // tilde keys: Delete=3, PageUp=5, PageDown=6 (+ Insert=2 used by xterm).
    const TILDE: [(KeyCode, u8); 4] = [
        (KeyCode::Delete, 3),
        (KeyCode::PageUp, 5),
        (KeyCode::PageDown, 6),
        (KeyCode::Insert, 2),
    ];
    for (want, number) in TILDE {
        if code == want {
            return Some(if modifier > 1 {
                format!("\x1b[{number};{modifier}~").into_bytes()
            } else {
                format!("\x1b[{number}~").into_bytes()
            });
        }
    }

    // Function keys F7..F12. F1..F6 are TUI bindings (help, views, the
    // transfer view), so none of them reaches the target: an F-key the
    // interface has taken is absent from this table, and absent means
    // `return None`.
    const FKEYS: [(u8, &str); 6] = [
        (7, "\x1b[18~"),
        (8, "\x1b[19~"),
        (9, "\x1b[20~"),
        (10, "\x1b[21~"),
        (11, "\x1b[23~"),
        (12, "\x1b[24~"),
    ];
    if let KeyCode::F(n) = code {
        if let Some((_, seq)) = FKEYS.iter().find(|(want, _)| *want == n) {
            let mut bytes = seq.as_bytes().to_vec();
            if alt {
                bytes.insert(0, 0x1b);
            }
            return Some(bytes);
        }
        return None;
    }

    match code {
        KeyCode::Tab => {
            // crossterm reports Shift+Tab as BackTab; the JS key bar sends CSI Z.
            Some(b"\t".to_vec())
        }
        KeyCode::BackTab => Some(b"\x1b[Z".to_vec()),
        KeyCode::Esc => Some(alt_prefixed("\x1b", alt)),
        KeyCode::Enter => Some(alt_prefixed("\r", alt)),
        KeyCode::Backspace => Some(alt_prefixed("\x7f", alt)),
        KeyCode::Char(c) => {
            // A physical Ctrl chord on a printable key is the control byte.
            let mods = StickyMods {
                shift: sticky.shift,
                ctrl,
                alt,
            };
            if physical.ctrl && !sticky.shift {
                // Physical keyboard: crossterm already decoded the shifted
                // glyph, so only ctrl/alt transform it (parity with the key bar
                // where Ctrl-X is produced from the plain letter).
                let mut out = String::new();
                out.push(control_input(c));
                if alt {
                    out.insert(0, '\x1b');
                }
                return Some(out.into_bytes());
            }
            Some(apply_input_modifiers(c, mods).into_bytes())
        }
        _ => None,
    }
}

fn alt_prefixed(seq: &str, alt: bool) -> Vec<u8> {
    let mut bytes = seq.as_bytes().to_vec();
    if alt {
        bytes.insert(0, 0x1b);
    }
    bytes
}

/// Apply the `--enter` translation before sending a line. Uses the shared
/// `protocol::validate::translate_enter`; the local fallback (identical to
/// `web/app.js::normalizeEnter`) keeps the TUI alive while that workstream
/// code is still landing.
pub fn translate_enter(data: &[u8], mode: EnterMode) -> Vec<u8> {
    if mode == EnterMode::Raw {
        return data.to_vec();
    }
    let mode_str = mode.as_str();
    match std::panic::catch_unwind(|| crate::protocol::validate::translate_enter(data, mode_str)) {
        Ok(translated) => translated,
        Err(_) => fallback_translate_enter(data, mode_str),
    }
}

fn fallback_translate_enter(data: &[u8], mode: &str) -> Vec<u8> {
    match mode {
        "raw" => return data.to_vec(),
        "cr" | "lf" | "crlf" => {}
        _ => return data.to_vec(),
    }
    let mut normalized = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == b'\r' && data.get(i + 1) == Some(&b'\n') {
            normalized.push(b'\n');
            i += 2;
        } else if data[i] == b'\r' || data[i] == b'\n' {
            normalized.push(b'\n');
            i += 1;
        } else {
            normalized.push(data[i]);
            i += 1;
        }
    }
    let replacement: &[u8] = match mode {
        "cr" => b"\r",
        "lf" => b"\n",
        _ => b"\r\n",
    };
    let mut out = Vec::with_capacity(normalized.len());
    for byte in normalized {
        if byte == b'\n' {
            out.extend_from_slice(replacement);
        } else {
            out.push(byte);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(code: KeyCode, mods: KeyModifiers) -> Option<Vec<u8>> {
        encode_key(code, mods, false, StickyMods::default())
    }

    fn text(code: KeyCode, mods: KeyModifiers) -> Option<String> {
        enc(code, mods).map(|b| String::from_utf8(b).unwrap())
    }

    // --- controlInput parity -------------------------------------------------

    #[test]
    fn control_input_matches_the_js_port() {
        assert_eq!(control_input('c') as u32, 0x03);
        assert_eq!(control_input('l') as u32, 0x0c);
        assert_eq!(control_input('z') as u32, 0x1a);
        assert_eq!(control_input('C') as u32, 0x03);
        assert_eq!(control_input('@') as u32, 0x00);
        assert_eq!(control_input('[') as u32, 0x1b);
        assert_eq!(control_input('\\') as u32, 0x1c);
        assert_eq!(control_input(']') as u32, 0x1d);
        assert_eq!(control_input('^') as u32, 0x1e);
        assert_eq!(control_input('_') as u32, 0x1f);
        assert_eq!(control_input(' '), '\0');
        assert_eq!(control_input('?'), '\x7f');
        // Outside the @.._ range the character passes through.
        assert_eq!(control_input('1'), '1');
        assert_eq!(control_input('/'), '/');
    }

    #[test]
    fn ctrl_letter_on_a_physical_keyboard_is_the_control_byte() {
        assert_eq!(
            enc(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(vec![0x03])
        );
        assert_eq!(
            enc(KeyCode::Char('d'), KeyModifiers::CONTROL),
            Some(vec![0x04])
        );
        assert_eq!(
            enc(KeyCode::Char(' '), KeyModifiers::CONTROL),
            Some(vec![0x00])
        );
    }

    // --- applyInputModifiers parity ------------------------------------------

    #[test]
    fn shift_map_matches_the_js_port() {
        assert_eq!(
            SHIFT_NORMAL, "`1234567890-=[]\\;',./",
            "normal map drifted from terminal_keys.js"
        );
        assert_eq!(SHIFT_SHIFTED, "~!@#$%^&*()_+{}|:\"<>?");
        assert_eq!(SHIFT_NORMAL.chars().count(), SHIFT_SHIFTED.chars().count());
        let sticky = StickyMods {
            shift: true,
            ctrl: false,
            alt: false,
        };
        assert_eq!(apply_input_modifiers('-', sticky), "_");
        assert_eq!(apply_input_modifiers('1', sticky), "!");
        assert_eq!(apply_input_modifiers('a', sticky), "A");
        assert_eq!(apply_input_modifiers(';', sticky), ":");
        let alt = StickyMods {
            shift: false,
            ctrl: false,
            alt: true,
        };
        assert_eq!(apply_input_modifiers('x', alt), "\x1bx");
        let ctrl_alt = StickyMods {
            shift: false,
            ctrl: true,
            alt: true,
        };
        assert_eq!(apply_input_modifiers('c', ctrl_alt), "\x1b\x03");
    }

    // --- terminalKeySequence parity ------------------------------------------

    #[test]
    fn arrow_keys_use_csi_or_application_form() {
        assert_eq!(text(KeyCode::Up, KeyModifiers::NONE), Some("\x1b[A".into()));
        assert_eq!(
            text(KeyCode::Down, KeyModifiers::NONE),
            Some("\x1b[B".into())
        );
        assert_eq!(
            text(KeyCode::Right, KeyModifiers::NONE),
            Some("\x1b[C".into())
        );
        assert_eq!(
            text(KeyCode::Left, KeyModifiers::NONE),
            Some("\x1b[D".into())
        );
        assert_eq!(
            text(KeyCode::Up, KeyModifiers::CONTROL),
            Some("\x1b[1;5A".into())
        );
        assert_eq!(
            text(KeyCode::Down, KeyModifiers::SHIFT),
            Some("\x1b[1;2B".into())
        );
        assert_eq!(
            text(KeyCode::Right, KeyModifiers::ALT),
            Some("\x1b[1;3C".into())
        );
        assert_eq!(
            text(KeyCode::Left, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
            Some("\x1b[1;6D".into())
        );
        // Application cursor mode (DECCKM) switches the lead byte.
        let app = encode_key(KeyCode::Up, KeyModifiers::NONE, true, StickyMods::default())
            .map(|b| String::from_utf8(b).unwrap());
        assert_eq!(app, Some("\x1bOA".into()));
    }

    #[test]
    fn home_and_end_follow_the_cursor_key_rule() {
        assert_eq!(
            text(KeyCode::Home, KeyModifiers::NONE),
            Some("\x1b[H".into())
        );
        assert_eq!(
            text(KeyCode::End, KeyModifiers::NONE),
            Some("\x1b[F".into())
        );
        assert_eq!(
            text(KeyCode::Home, KeyModifiers::CONTROL),
            Some("\x1b[1;5H".into())
        );
        let app = encode_key(
            KeyCode::End,
            KeyModifiers::NONE,
            true,
            StickyMods::default(),
        )
        .map(|b| String::from_utf8(b).unwrap());
        assert_eq!(app, Some("\x1bOF".into()));
    }

    #[test]
    fn tilde_keys_and_modified_forms() {
        assert_eq!(
            text(KeyCode::Delete, KeyModifiers::NONE),
            Some("\x1b[3~".into())
        );
        assert_eq!(
            text(KeyCode::PageUp, KeyModifiers::NONE),
            Some("\x1b[5~".into())
        );
        assert_eq!(
            text(KeyCode::PageDown, KeyModifiers::NONE),
            Some("\x1b[6~".into())
        );
        assert_eq!(
            text(KeyCode::Insert, KeyModifiers::NONE),
            Some("\x1b[2~".into())
        );
        assert_eq!(
            text(KeyCode::Delete, KeyModifiers::CONTROL),
            Some("\x1b[3;5~".into())
        );
        assert_eq!(
            text(KeyCode::PageUp, KeyModifiers::CONTROL),
            Some("\x1b[5;5~".into())
        );
        assert_eq!(
            text(KeyCode::PageDown, KeyModifiers::CONTROL | KeyModifiers::ALT),
            Some("\x1b[6;7~".into())
        );
    }

    #[test]
    fn special_keys_match_the_js_sequence_table() {
        assert_eq!(text(KeyCode::Tab, KeyModifiers::NONE), Some("\t".into()));
        assert_eq!(
            text(KeyCode::BackTab, KeyModifiers::SHIFT),
            Some("\x1b[Z".into())
        );
        assert_eq!(text(KeyCode::Esc, KeyModifiers::NONE), Some("\x1b".into()));
        assert_eq!(text(KeyCode::Enter, KeyModifiers::NONE), Some("\r".into()));
        assert_eq!(
            text(KeyCode::Backspace, KeyModifiers::NONE),
            Some("\x7f".into())
        );
        // Alt prefixes ESC.
        assert_eq!(
            text(KeyCode::Enter, KeyModifiers::ALT),
            Some("\x1b\r".into())
        );
        assert_eq!(
            text(KeyCode::Char('x'), KeyModifiers::ALT),
            Some("\x1bx".into())
        );
    }

    #[test]
    fn symbol_keys_pass_through_plain() {
        for symbol in ['/', '|', '~', '\\', '`', '-'] {
            assert_eq!(
                text(KeyCode::Char(symbol), KeyModifiers::NONE),
                Some(symbol.to_string())
            );
        }
    }

    #[test]
    fn sticky_modifiers_arm_for_exactly_one_key() {
        let mut sticky = StickyMods::default();
        sticky = sticky.toggle_ctrl();
        assert!(sticky.ctrl);
        let bytes = encode_key(KeyCode::Char('c'), KeyModifiers::NONE, false, sticky).unwrap();
        assert_eq!(bytes, vec![0x03]);
        sticky = sticky.toggle_ctrl();
        assert!(!sticky.ctrl);
        assert!(sticky.is_empty());
    }

    #[test]
    fn function_keys_beyond_the_tui_bindings_reach_the_target() {
        assert_eq!(
            text(KeyCode::F(7), KeyModifiers::NONE),
            Some("\x1b[18~".into())
        );
        assert_eq!(
            text(KeyCode::F(12), KeyModifiers::NONE),
            Some("\x1b[24~".into())
        );
        // F1..F6 are TUI bindings (help, views, file transfer) and never
        // reach the UART — the transfer view's own F6 included, so a run
        // cannot be started by a key press meant for the target shell.
        assert_eq!(enc(KeyCode::F(1), KeyModifiers::NONE), None);
        assert_eq!(enc(KeyCode::F(4), KeyModifiers::NONE), None);
        assert_eq!(enc(KeyCode::F(6), KeyModifiers::NONE), None);
        // Mouse/unknown keys are ignored.
        assert_eq!(enc(KeyCode::Null, KeyModifiers::NONE), None);
    }

    // --- byte parity against the JS source file -------------------------------

    fn load_js() -> String {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/terminal_keys.js");
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
    }

    #[test]
    fn js_source_declares_the_sequences_we_emit() {
        let src = load_js();
        // CSI modifier form for cursor keys.
        assert!(
            src.contains("\\x1b[1;${modifier}${cursor[key]}"),
            "cursor modifier form changed in terminal_keys.js"
        );
        // Application cursor form.
        assert!(
            src.contains("`\\x1b${applicationCursorKeys ? \"O\" : \"[\"}${cursor[key]}`"),
            "application cursor form changed in terminal_keys.js"
        );
        // The literal sequence table.
        assert!(
            src.contains(r#"Tab: "\t", Escape: "\x1b", Enter: "\r", Backspace: "\x7f""#),
            "special key table changed in terminal_keys.js"
        );
        assert!(src.contains(r#"Delete: "\x1b[3~", PageUp: "\x1b[5~", PageDown: "\x1b[6~""#));
        assert!(
            src.contains(r#"return "\x1b[Z";"#),
            "shift-tab form changed"
        );
        assert!(
            src.contains(r#"`\x1b[${tilde[key]};${modifier}~`"#),
            "tilde modifier form changed"
        );
        // The modifier parameter arithmetic.
        assert!(
            src.contains("const modifier = 1 + (modifiers.shift ? 1 : 0)"),
            "modifier arithmetic changed in terminal_keys.js"
        );
        // The ASCII shift maps.
        assert!(
            src.contains(r#"const normal = "`1234567890-=[]\\;',./";"#),
            "normal shift map changed in terminal_keys.js"
        );
        assert!(
            src.contains(r#"const shifted = '~!@#$%^&*()_+{}|:"<>?';"#),
            "shifted map changed in terminal_keys.js"
        );
        // controlInput rules.
        assert!(src.contains(r#"if (data === " ") return "\x00";"#));
        assert!(src.contains(r#"if (data === "?") return "\x7f";"#));
        assert!(src.contains("return code >= 0x40 && code <= 0x5f"));
    }

    #[test]
    fn our_shift_maps_are_byte_identical_to_the_js_literals() {
        let src = load_js();
        let normal_line = src
            .lines()
            .find(|l| l.contains("const normal ="))
            .expect("normal map line");
        let shifted_line = src
            .lines()
            .find(|l| l.contains("const shifted ="))
            .expect("shifted map line");
        // Extract the quoted JS literal and decode the JS escapes we care about
        // (\\ and \').
        fn js_string_literal(line: &str) -> String {
            let start = line.find(['\'', '"']).expect("opening quote");
            let quote = line.as_bytes()[start];
            let rest = &line[start + 1..];
            let end = rest.find(quote as char).expect("closing quote");
            let raw = &rest[..end];
            raw.replace("\\\\", "\\").replace("\\'", "'")
        }
        assert_eq!(js_string_literal(normal_line), SHIFT_NORMAL);
        assert_eq!(js_string_literal(shifted_line), SHIFT_SHIFTED);
    }

    // --- enter translation ----------------------------------------------------

    #[test]
    fn enter_translation_matches_the_web_normalize_enter() {
        // The web client: raw keeps the text; other modes rewrite every line end.
        assert_eq!(translate_enter(b"help\n", EnterMode::Raw), b"help\n");
        assert_eq!(translate_enter(b"help\n", EnterMode::Cr), b"help\r");
        assert_eq!(translate_enter(b"help\n", EnterMode::Lf), b"help\n");
        assert_eq!(translate_enter(b"help\n", EnterMode::Crlf), b"help\r\n");
        assert_eq!(translate_enter(b"a\r\nb\rc\n", EnterMode::Cr), b"a\rb\rc\r");
        assert_eq!(
            translate_enter(b"a\r\nb\rc\n", EnterMode::Crlf),
            b"a\r\nb\r\nc\r\n"
        );
    }
}
