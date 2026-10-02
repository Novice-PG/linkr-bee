//! TUI settings persistence: `dirs::config_dir()/linkr/tui.json`.
//!
//! Same spirit as the web client's `linkr-*` localStorage keys (WEB_UX_SPEC
//! section 9): font size, Enter mode, local echo, transport, last LAN host and
//! the active view survive restarts. Unknown fields are ignored so an older
//! binary keeps reading a newer file.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::state::View;

/// Default xterm font size of the web client (`linkr-font` default 13).
pub const DEFAULT_FONT_SIZE: u8 = 13;
pub const MIN_FONT_SIZE: u8 = 10;
pub const MAX_FONT_SIZE: u8 = 28;

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
    /// `linkr-settings-page` equivalent for the TUI views.
    pub active_view: View,
    /// `linkr-autoscroll` equivalent (web default: pressed).
    pub autoscroll: bool,
}

impl Default for TuiSettings {
    fn default() -> Self {
        Self {
            font_size: DEFAULT_FONT_SIZE,
            enter_mode: EnterMode::Raw,
            local_echo: false,
            transport: TransportChoice::Ble,
            last_lan_host: String::new(),
            active_view: View::Terminal,
            autoscroll: true,
        }
    }
}

impl TuiSettings {
    /// Clamp every value the way the web client clamps its inputs
    /// (font zoom 10..=28, LAN host trimmed).
    pub fn normalized(mut self) -> Self {
        self.font_size = self.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        self.last_lan_host = self.last_lan_host.trim().to_string();
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
    let mut path = dirs::config_dir().unwrap_or_default();
    path.push("linkr");
    path.push("tui.json");
    path
}

/// Load settings, substituting defaults when the file is missing or broken.
pub fn load() -> TuiSettings {
    match std::fs::read_to_string(settings_path()) {
        Ok(text) => TuiSettings::from_json(&text),
        Err(_) => TuiSettings::default(),
    }
}

/// Persist settings; parent directory created on demand. Errors are reported by
/// the caller as a toast, never fatal.
pub fn save(settings: &TuiSettings) -> std::io::Result<()> {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, settings.to_json())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_web_client() {
        let s = TuiSettings::default();
        assert_eq!(s.font_size, 13);
        assert_eq!(s.enter_mode, EnterMode::Raw);
        assert!(!s.local_echo);
        assert_eq!(s.transport, TransportChoice::Ble);
        assert!(s.autoscroll);
        assert_eq!(s.active_view, View::Terminal);
    }

    #[test]
    fn round_trips_through_json() {
        let s = TuiSettings {
            font_size: 17,
            enter_mode: EnterMode::Crlf,
            local_echo: true,
            transport: TransportChoice::Lan,
            last_lan_host: "192.168.1.50".to_string(),
            active_view: View::Network,
            autoscroll: false,
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
}
