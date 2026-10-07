//! TUI settings persistence: `dirs::config_dir()/linkr/tui.json`.
//!
//! Same spirit as the web client's `linkr-*` localStorage keys (WEB_UX_SPEC
//! section 9): font size, Enter mode, local echo, transport, last LAN host and
//! the active view survive restarts. Unknown fields are ignored so an older
//! binary keeps reading a newer file.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::i18n::strings;
use super::state::View;

/// Default xterm font size of the web client (`linkr-font` default 13).
pub const DEFAULT_FONT_SIZE: u8 = 13;
pub const MIN_FONT_SIZE: u8 = 10;
pub const MAX_FONT_SIZE: u8 = 28;

// This file carries no interface text: `label()` answers with the `BLE` /
// `LAN` and `Raw` / `CR` / `LF` / `CRLF` identifiers the parity suites
// compare byte for byte, `TuiSettings` only persists itself, and the
// settings dialog itself is rendered by `dialogs.rs` / `agent_settings.rs`.
// The empty table below is the proof: there is nothing to translate here.
strings! {}

/// `linkr-enter` equivalent: how a line ending is encoded before sending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EnterMode {
    /// Send exactly what the user typed (xterm default).
    #[default]
    Raw,
    Cr,
    Lf,
    Crlf,
}

impl EnterMode {
    pub fn as_str(self) -> &'static str {
        match self {
            EnterMode::Raw => "raw",
            EnterMode::Cr => "cr",
            EnterMode::Lf => "lf",
            EnterMode::Crlf => "crlf",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            EnterMode::Raw => "Raw",
            EnterMode::Cr => "CR",
            EnterMode::Lf => "LF",
            EnterMode::Crlf => "CRLF",
        }
    }

    /// Cycle order used by the palette action `terminal.enter_mode`.
    pub fn next(self) -> Self {
        match self {
            EnterMode::Raw => EnterMode::Cr,
            EnterMode::Cr => EnterMode::Lf,
            EnterMode::Lf => EnterMode::Crlf,
            EnterMode::Crlf => EnterMode::Raw,
        }
    }
}

impl std::str::FromStr for EnterMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "raw" => Ok(EnterMode::Raw),
            "cr" => Ok(EnterMode::Cr),
            "lf" => Ok(EnterMode::Lf),
            "crlf" => Ok(EnterMode::Crlf),
            other => Err(format!("unknown enter mode: {other}")),
        }
    }
}

/// `linkr-transport` equivalent (`ble` | `ws`, stored as `ble` | `lan`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TransportChoice {
    #[default]
    Ble,
    Lan,
}

impl TransportChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            TransportChoice::Ble => "ble",
            TransportChoice::Lan => "lan",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TransportChoice::Ble => "BLE",
            TransportChoice::Lan => "LAN",
        }
    }
}

/// Everything the TUI persists between runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiSettings {
    /// Terminal font size, 10..=28 (web `linkr-font`). The host terminal draws
    /// the glyphs; the TUI uses the value to derive the grid density like the
    /// web client's fit addon does.
    pub font_size: u8,
    /// `linkr-enter`.
    pub enter_mode: EnterMode,
    /// `linkr-echo`.
    pub local_echo: bool,
    /// `linkr-transport`.
    pub transport: TransportChoice,
    /// `linkr-ws-host`.
    pub last_lan_host: String,
    /// The `--address` the CLI was started with, remembered so the sidebar's
    /// Connect dials the same peripheral again instead of the first name
    /// match (the web picks a device from a chooser; the TUI has no address
    /// field of its own).
    pub last_ble_address: String,
    /// `linkr-settings-page` equivalent for the TUI views.
    pub active_view: View,
    /// `linkr-autoscroll` equivalent (web default: pressed).
    pub autoscroll: bool,
    /// `linkr-lang`. English by default so library behaviour (and the tests
    /// that pin it) stay deterministic; a first run with no settings file
    /// picks it up from the locale instead.
    pub lang: super::i18n::Lang,
}

impl Default for TuiSettings {
    fn default() -> Self {
        Self {
            font_size: DEFAULT_FONT_SIZE,
            enter_mode: EnterMode::Raw,
            local_echo: false,
            transport: TransportChoice::Ble,
            last_lan_host: String::new(),
            last_ble_address: String::new(),
            active_view: View::Terminal,
            autoscroll: true,
            lang: super::i18n::Lang::En,
        }
    }
}

impl TuiSettings {
    /// Clamp every value the way the web client clamps its inputs
    /// (font zoom 10..=28, LAN host trimmed).
    pub fn normalized(mut self) -> Self {
        self.font_size = self.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        self.last_lan_host = self.last_lan_host.trim().to_string();
        self.last_ble_address = self.last_ble_address.trim().to_string();
        self
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Parse persisted JSON; malformed files fall back to defaults instead of
    /// failing the whole TUI (the web client does the same for bad localStorage
    /// values).
    pub fn from_json(text: &str) -> Self {
        serde_json::from_str::<Self>(text)
            .unwrap_or_default()
            .normalized()
    }
}

/// `dirs::config_dir()/linkr/tui.json`.
pub fn settings_path() -> PathBuf {
    #[cfg(test)]
    {
        // Tests share one machine — and one `tui.json` — while libtest runs
        // them side by side, so a test that writes it lands in the middle of
        // another test's read-assert-restore window (2 of 5 runs of
        // `cargo test tui::sidebar` failed exactly that way), and both of them
        // leave the developer's own configuration rewritten behind their back.
        // Tests get a file of their own.
        let mut path = std::env::temp_dir();
        path.push(format!("linkr-tui-test-{}.json", std::process::id()));
        path
    }
    #[cfg(not(test))]
    {
        let mut path = dirs::config_dir().unwrap_or_default();
        path.push("linkr");
        path.push("tui.json");
        path
    }
}

/// Everything that touches the settings file inside a test takes this lock,
/// so no test can read a file another test is halfway through rewriting.
///
/// It is re-entrant per thread: a test that already holds it (the sidebar's
/// `SettingsGuard` holds it for its whole run) reaches `save` again through
/// the flush it is testing, and would deadlock on a plain `Mutex`.
#[cfg(test)]
pub(crate) mod file_lock {
    use std::cell::Cell;
    use std::sync::{Mutex, MutexGuard};

    static LOCK: Mutex<()> = Mutex::new(());

    thread_local! {
        static HELD: Cell<u32> = const { Cell::new(0) };
    }

    pub struct FileLock {
        /// `Some` only for the outermost acquire of this thread.
        held: Option<MutexGuard<'static, ()>>,
    }

    impl FileLock {
        pub fn acquire() -> Self {
            HELD.with(|held| {
                if held.get() > 0 {
                    held.set(held.get() + 1);
                    return Self { held: None };
                }
                let guard = LOCK.lock().unwrap_or_else(|err| err.into_inner());
                held.set(1);
                Self { held: Some(guard) }
            })
        }
    }

    impl Drop for FileLock {
        fn drop(&mut self) {
            HELD.with(|held| {
                let left = held.get().saturating_sub(1);
                held.set(left);
                if left == 0 {
                    drop(self.held.take());
                }
            });
        }
    }
}

/// Load settings, substituting defaults when the file is missing or broken.
pub fn load() -> TuiSettings {
    // See `settings_path`: in tests the file belongs to whoever is reading it.
    #[cfg(test)]
    let _lock = file_lock::FileLock::acquire();
    match std::fs::read_to_string(settings_path()) {
        Ok(text) => TuiSettings::from_json(&text),
        // First run: follow the locale the way the web reads
        // `navigator.language`; [`TuiSettings::default`] itself stays English.
        Err(_) => TuiSettings {
            lang: super::i18n::Lang::from_env(),
            ..TuiSettings::default()
        },
    }
}

/// Persist settings; parent directory created on demand. Errors are reported by
/// the caller as a toast, never fatal.
pub fn save(settings: &TuiSettings) -> std::io::Result<()> {
    // See `settings_path`: no test writes while another one is looking.
    #[cfg(test)]
    let _lock = file_lock::FileLock::acquire();
    let path = settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, settings.to_json())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::i18n::Lang;

    #[test]
    fn defaults_match_the_web_client() {
        let s = TuiSettings::default();
        assert_eq!(s.font_size, 13);
        assert_eq!(s.enter_mode, EnterMode::Raw);
        assert!(!s.local_echo);
        assert_eq!(s.transport, TransportChoice::Ble);
        assert!(s.autoscroll);
        assert_eq!(s.active_view, View::Terminal);
        assert_eq!(s.lang, Lang::En);
    }

    #[test]
    fn round_trips_through_json() {
        let s = TuiSettings {
            font_size: 17,
            enter_mode: EnterMode::Crlf,
            local_echo: true,
            transport: TransportChoice::Lan,
            last_lan_host: "192.168.1.50".to_string(),
            last_ble_address: "EE:C7:42:34:48:CF".to_string(),
            active_view: View::Network,
            autoscroll: false,
            lang: Lang::Zh,
        };
        let json = s.to_json();
        assert!(json.contains("\"font_size\": 17"));
        assert_eq!(TuiSettings::from_json(&json), s);
    }

    #[test]
    fn partial_json_fills_missing_fields_with_defaults() {
        let s = TuiSettings::from_json(r#"{"font_size":20}"#);
        assert_eq!(s.font_size, 20);
        assert_eq!(s.enter_mode, EnterMode::Raw);
        assert_eq!(s.transport, TransportChoice::Ble);
        assert_eq!(s.active_view, View::Terminal);
        assert!(s.autoscroll);
        assert_eq!(s.lang, Lang::En, "an old file keeps English");
    }

    /// The persisted key stays `linkr-lang` with the web's own values.
    #[test]
    fn the_language_survives_a_restart() {
        let json = TuiSettings {
            lang: Lang::Zh,
            ..TuiSettings::default()
        }
        .to_json();
        assert!(json.contains("\"lang\": \"zh\""), "{json}");
        assert_eq!(TuiSettings::from_json(&json).lang, Lang::Zh);
    }

    #[test]
    fn unknown_fields_are_ignored_and_broken_files_fall_back() {
        let s = TuiSettings::from_json(r#"{"font_size":14,"future_knob":true}"#);
        assert_eq!(s.font_size, 14);
        assert_eq!(
            TuiSettings::from_json("not json at all"),
            TuiSettings::default()
        );
        assert_eq!(TuiSettings::from_json(""), TuiSettings::default());
    }

    #[test]
    fn font_size_is_clamped_like_the_web_zoom() {
        assert_eq!(
            TuiSettings::from_json(r#"{"font_size":1}"#).font_size,
            MIN_FONT_SIZE
        );
        assert_eq!(
            TuiSettings::from_json(r#"{"font_size":99}"#).font_size,
            MAX_FONT_SIZE
        );
        assert_eq!(TuiSettings::from_json(r#"{"font_size":17}"#).font_size, 17);
    }

    #[test]
    fn enter_mode_and_transport_labels() {
        assert_eq!(EnterMode::Raw.as_str(), "raw");
        assert_eq!(EnterMode::Crlf.label(), "CRLF");
        assert_eq!(EnterMode::Raw.next(), EnterMode::Cr);
        assert_eq!(EnterMode::Crlf.next(), EnterMode::Raw);
        assert_eq!("crlf".parse::<EnterMode>().unwrap(), EnterMode::Crlf);
        assert!("nope".parse::<EnterMode>().is_err());
        assert_eq!(TransportChoice::Lan.label(), "LAN");
        assert_eq!(
            serde_json::to_string(&TransportChoice::Ble).unwrap(),
            "\"ble\""
        );
    }

    #[test]
    fn view_serde_round_trip() {
        for view in [
            View::Terminal,
            View::Diagnostics,
            View::Network,
            View::Assistant,
        ] {
            let json = serde_json::to_string(&view).unwrap();
            assert_eq!(serde_json::from_str::<View>(&json).unwrap(), view);
        }
        assert_eq!(
            serde_json::to_string(&View::Diagnostics).unwrap(),
            "\"diagnostics\""
        );
    }

    /// The settings table is empty on purpose (see the note above the
    /// `strings!` block): the `label()` texts are identifiers and the dialog
    /// wording lives in `dialogs.rs` / `agent_settings.rs`.
    #[test]
    fn every_set_message_is_translated() {
        super::super::i18n::assert_bilingual(ALL);
        assert!(ALL.is_empty(), "settings.rs renders no interface text");
    }
}
