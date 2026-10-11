//! LAN access-token store: the CLI half of `web/lan_token_store.js`.
//!
//! The web client keeps `{tokens, hosts}` in localStorage and fills its token
//! field from it — `selectDevice()` right after the BLE handshake,
//! `selectHost()` when the host is restored or edited — then sends whatever
//! the field holds in the access handshake. This module is that store for the
//! TUI and for `--lan`, written to `config/linkr/lan_tokens.json`; the payload
//! is the web store's own shape, so a file written by either client reads in
//! the other.
//!
//! The token itself never reaches the interface: `@s?` answers with
//! `… token=<32 hex>` and [`redact_secrets`] blanks it in every line the CLI
//! prints (web `redactSecrets`: display and export are redacted, parsing reads
//! the raw text).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Line prefix of the bridge's `@s?` answer (web `handleSocketStatusLine`).
const SOCKET_STATUS: &str = "OK ws=";
/// Length of a LAN token: 128 bits as lowercase hex.
const TOKEN_LEN: usize = 32;

/// The web store's `linkr-lan-tokens-v1` payload: `{tokens, hosts}`.
///
/// `tokens` maps `device:<id>` to a token, `hosts` maps a lowercased URL host
/// to the device key that owns it — "device identity owns the token, host
/// aliases only select a device".
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenStore {
    #[serde(default)]
    tokens: BTreeMap<String, String>,
    #[serde(default)]
    hosts: BTreeMap<String, String>,
}

/// `dirs::config_dir()/linkr/lan_tokens.json`, the store's localStorage
/// counterpart.
pub fn store_path() -> PathBuf {
    let mut path = dirs::config_dir().unwrap_or_default();
    path.push("linkr");
    path.push("lan_tokens.json");
    path
}

fn device_key(device_id: &str) -> String {
    format!("device:{device_id}")
}

fn is_whitespace(ch: char) -> bool {
    ch.is_whitespace()
}

fn is_hex(text: &str, lower_only: bool) -> bool {
    text.len() == TOKEN_LEN
        && text.bytes().all(|byte| match byte {
            b'0'..=b'9' => true,
            b'a'..=b'f' => true,
            b'A'..=b'F' => !lower_only,
            _ => false,
        })
}

/// The web store's `lanHostKey`: the URL host of `host`, lowercased, with the
/// default `ws://…/ws` supplied when the text carries no scheme. `""` when
/// there is no host to name (the web's `try/catch` around `new URL`).
pub fn lan_host_key(host: &str) -> String {
    let candidate = if host.starts_with("ws://") || host.starts_with("wss://") {
        host.to_string()
    } else {
        format!("ws://{host}/ws")
    };
    let Some((_, rest)) = candidate.split_once("://") else {
        return String::new();
    };
    // `URL.host` is the authority up to the path, port included; credentials
    // never appear in a LAN host here.
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// The token an `@s?` reply carries: `Some("")` when the bridge reports
/// `token=none` (authentication off) and `None` when the line says nothing
/// about a token. Raw text in, exact web parse (`OK ws=` prefix, `token=`
/// delimited by whitespace).
pub fn parse_socket_status_line(line: &str) -> Option<&str> {
    let value = line.trim();
    if !value.starts_with(SOCKET_STATUS) {
        return None;
    }
    let mut search = 0usize;
    while let Some(offset) = value[search..].find("token=") {
        let at = search + offset;
        let body = at + "token=".len();
        let end = value[body..]
            .find(is_whitespace)
            .map_or(value.len(), |relative| body + relative);
        let candidate = &value[body..end];
        let starts_word = at == 0 || value[..at].chars().next_back().is_some_and(is_whitespace);
        if starts_word && (candidate == "none" || is_hex(candidate, true)) {
            return Some(if candidate == "none" { "" } else { candidate });
        }
        search = body;
    }
    None
}

/// The token a whole reply reports: the **last** status line that carries one
/// (web `handleRxLine` runs the parser per line and every match overwrites the
/// field, so a later line wins). `Some("")` when that line says `token=none`
/// — the bridge running without authentication — and `None` when no line
/// carries a token at all.
pub fn token_from_reply<'a, I>(lines: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
    I::IntoIter: DoubleEndedIterator,
{
    lines.into_iter().rev().find_map(parse_socket_status_line)
}

/// The command that reads the token, or `None` when the device does not offer
/// it: web `hasManagementCapability(MGMT_CAP_WEBSOCKET)` gates `@s?` the same
/// way, and a bridge that never advertised itself cannot be asked.
pub fn socket_query_command(capabilities: u32) -> Option<&'static str> {
    (capabilities & crate::protocol::mgmt::MGMT_CAP_WEBSOCKET != 0).then_some("@s?")
}

/// Whether a fresh capture may replace what the token field shows. The web
/// keeps `dirty` for exactly this — `if (!dirty) value = token` — so a value
/// the store does not hold was typed by the user and stays.
pub fn may_replace_field(field: &str, stored: Option<&str>) -> bool {
    field.is_empty() || Some(field) == stored
}

/// Web `redactSecrets`: replace `token=<32 hex>` with `token=<redacted>` in
/// display and export. The pattern needs whitespace (or the end of the text)
/// on both sides, so a shorter number or a run-up of hex stays visible.
pub fn redact_secrets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0usize;
    let mut search = 0usize;
    while let Some(offset) = text[search..].find("token=") {
        let at = search + offset;
        let body = at + "token=".len();
        let end = text[body..]
            .find(is_whitespace)
            .map_or(text.len(), |relative| body + relative);
        let starts_word = at == 0 || text[..at].chars().next_back().is_some_and(is_whitespace);
        let ends_word = end == text.len() || text[end..].chars().next().is_some_and(is_whitespace);
        if starts_word && ends_word && is_hex(&text[body..end], false) {
            out.push_str(&text[copied..at]);
            out.push_str("token=<redacted>");
            copied = end;
        }
        search = body;
    }
    out.push_str(&text[copied..]);
    out
}

impl TokenStore {
    /// Read [`store_path`]; a missing or malformed file reads as empty (the
    /// web client does the same for unavailable or corrupt localStorage).
    pub fn load() -> Self {
        Self::load_from(&store_path())
    }

    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Self>(&text).ok())
            .unwrap_or_default()
    }

    /// Write the store; `parent` directories are created on demand and the
    /// file is created `0600` — it holds the LAN access token, which is full
    /// target access on the local network.
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&store_path())
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let payload = serde_json::to_string(self)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        // Write to a sibling and rename over the target: a crash between
        // `truncate` and `write` on the real file would leave an empty store,
        // and `load` reads that as "no tokens at all" — every device silently
        // back to needs-auth. `rename` within one directory is atomic, so a
        // reader sees either the old file or the whole new one.
        let tmp = {
            let mut name = path
                .file_name()
                .map(|n| n.to_os_string())
                .unwrap_or_else(|| std::ffi::OsString::from("lan_tokens.json"));
            name.push(".tmp");
            path.with_file_name(name)
        };
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        {
            let mut file = options.open(&tmp)?;
            file.write_all(payload.as_bytes())?;
            file.sync_all()?;
        }
        #[cfg(unix)]
        {
            // `mode` only binds when the file is created; a `.tmp` left behind
            // by an earlier crash would keep whatever permissions it already
            // had, so set them again before the rename publishes it.
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, path)
    }

    /// Token stored for a device (web `selectDevice`). A value that is neither
    /// empty nor a full lowercase token is not handed out: this file is only
    /// ever written by [`Self::capture`], so anything else is damage, and the
    /// web client would refuse it at connect time anyway.
    pub fn select_device(&self, device_id: &str) -> Option<&str> {
        let value = self.tokens.get(&device_key(device_id))?;
        usable(value).then_some(value.as_str())
    }

    /// Token for a host alias (web `selectHost`): the alias resolves to the
    /// device that owns the token, so a token is never keyed by address alone.
    pub fn select_host(&self, host: &str) -> Option<&str> {
        let key = lan_host_key(host);
        if key.is_empty() {
            return None;
        }
        let device = self.hosts.get(&key)?;
        let value = self.tokens.get(device)?;
        usable(value).then_some(value.as_str())
    }

    /// Store `token` for `device_id` and link `host` to it (web `capture`),
    /// in memory — the caller persists with [`Self::save`] once it holds a
    /// file-backed store, so a store built for a test never reaches disk.
    /// Returns the token when it was accepted and `None` when the id or the
    /// token is unusable: the caller then leaves its field alone instead of
    /// overwriting something the user typed.
    pub fn capture(&mut self, device_id: &str, token: &str, host: &str) -> Option<&str> {
        if device_id.is_empty() || !(token.is_empty() || is_hex(token, true)) {
            return None;
        }
        self.tokens.insert(device_key(device_id), token.to_string());
        if !host.is_empty() {
            let key = lan_host_key(host);
            if !key.is_empty() {
                self.hosts.insert(key, device_key(device_id));
            }
        }
        self.tokens.get(&device_key(device_id)).map(String::as_str)
    }
}

fn usable(value: &str) -> bool {
    value.is_empty() || is_hex(value, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    const DEVICE: &str = "4c494e4b52424c45010058bf2533078c";

    fn temp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("linkr-store-{}-{name}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn lan_host_key_reads_the_host_the_way_the_web_url_parser_does() {
        assert_eq!(lan_host_key("192.168.0.104"), "192.168.0.104");
        assert_eq!(lan_host_key("ws://192.168.0.104/ws"), "192.168.0.104");
        assert_eq!(lan_host_key("ws://192.168.0.104"), "192.168.0.104");
        // The web check for a scheme is case sensitive, so an upper-case one
        // falls into the default-scheme branch there too.
        assert_eq!(lan_host_key("WSS://Host.Example:8443/ws"), "wss:");
        assert_eq!(
            lan_host_key("wss://Host.Example:8443/ws"),
            "host.example:8443"
        );
        // No scheme → the default `ws://…/ws` is supplied first, so the bare
        // address still yields itself.
        assert_eq!(lan_host_key("HOST.EXAMPLE"), "host.example");
        assert_eq!(lan_host_key(""), "");
        assert_eq!(lan_host_key("ws:///ws"), "");
    }

    #[test]
    fn the_payload_is_the_web_store_json() {
        let mut store = TokenStore::default();
        assert_eq!(
            serde_json::to_string(&store).unwrap(),
            r#"{"tokens":{},"hosts":{}}"#
        );
        store
            .capture(DEVICE, TOKEN, "192.168.0.104")
            .expect("accepted");
        assert_eq!(
            serde_json::to_string(&store).unwrap(),
            format!(
                r#"{{"tokens":{{"device:{DEVICE}":"{TOKEN}"}},"hosts":{{"192.168.0.104":"device:{DEVICE}"}}}}"#
            )
        );
    }

    #[test]
    fn a_token_is_found_again_by_device_and_by_host() {
        let mut store = TokenStore::default();
        store
            .capture(DEVICE, TOKEN, "192.168.0.104")
            .expect("accepted");
        assert_eq!(store.select_device(DEVICE), Some(TOKEN));
        assert_eq!(store.select_host("192.168.0.104"), Some(TOKEN));
        assert_eq!(store.select_host("ws://192.168.0.104/ws"), Some(TOKEN));
        // Another peripheral has no token of its own, and the alias does not
        // hand out someone else's.
        assert_eq!(
            store.select_device("ffffffffffffffffffffffffffffffff"),
            None
        );
        assert_eq!(store.select_host("192.168.0.99"), None);
    }

    #[test]
    fn capture_refuses_what_the_web_store_refuses() {
        let mut store = TokenStore::default();
        // No device id, no store entry.
        assert!(store.capture("", TOKEN, "").is_none());
        // Uppercase or short tokens are not silently kept either.
        assert!(store
            .capture(DEVICE, "0123456789ABCDEF0123456789ABCDEF", "")
            .is_none());
        assert!(store.capture(DEVICE, "0123456789abcdef", "").is_none());
        assert!(store.tokens.is_empty());
        // `token=none` (auth disabled) is stored as an empty token, which is
        // the value the web client puts in its field.
        assert_eq!(store.capture(DEVICE, "", ""), Some(""));
        assert_eq!(store.select_device(DEVICE), Some(""));
        // …and a host alias pointing at auth-less device stays empty too.
        assert_eq!(store.capture(DEVICE, "", "192.168.0.104"), Some(""));
        assert_eq!(store.select_host("192.168.0.104"), Some(""));
    }

    #[test]
    fn a_damaged_store_never_hands_out_a_broken_token() {
        let mut store = TokenStore::default();
        store
            .tokens
            .insert(device_key(DEVICE), "not-a-token".to_string());
        store
            .hosts
            .insert(lan_host_key("192.168.0.104"), device_key(DEVICE));
        assert_eq!(store.select_device(DEVICE), None);
        assert_eq!(store.select_host("192.168.0.104"), None);
    }

    #[test]
    fn a_missing_or_unreadable_store_reads_as_empty() {
        let path = temp_path("missing");
        assert_eq!(TokenStore::load_from(&path), TokenStore::default());
        std::fs::write(&path, b"{ not json").expect("write");
        assert_eq!(TokenStore::load_from(&path), TokenStore::default());
    }

    #[test]
    fn the_store_round_trips_through_its_file() {
        let path = temp_path("roundtrip");
        let mut store = TokenStore::default();
        store
            .capture(DEVICE, TOKEN, "192.168.0.104")
            .expect("accepted");
        store.save_to(&path).expect("save");
        assert_eq!(TokenStore::load_from(&path), store);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "the token file must not be world readable"
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    /// The store is written through a sibling and renamed into place, so a
    /// crash cannot leave a half-written (or empty) file at the real path —
    /// `load` reads an empty file as "no tokens at all". Two observable
    /// halves: the temporary never survives a save, and the 0600 permission
    /// holds even when the file already existed with wider bits (a plain
    /// `mode(0o600)` only binds on create).
    #[test]
    fn saving_uses_a_sibling_and_tightens_an_existing_file() {
        let path = temp_path("atomic");
        // Seed the destination as an older build could have left it: valid,
        // but world-readable.
        std::fs::write(&path, r#"{"tokens":{},"hosts":{}}"#).expect("seed");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("widen");
        }

        let mut store = TokenStore::default();
        store
            .capture(DEVICE, TOKEN, "192.168.0.104")
            .expect("accepted");
        store.save_to(&path).expect("save");

        assert_eq!(TokenStore::load_from(&path), store);

        let mut tmp = path.clone().into_os_string();
        tmp.push(".tmp");
        assert!(
            !Path::new(&tmp).exists(),
            "the sibling write-through file was left behind"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "an existing wider mode must be tightened on save"
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_bridge_reply_yields_the_token() {
        assert_eq!(
            parse_socket_status_line(&format!("OK ws=up port=80 token={TOKEN}")),
            Some(TOKEN)
        );
        assert_eq!(
            parse_socket_status_line(&format!("  OK ws=up token={TOKEN} rx=0  ")),
            Some(TOKEN)
        );
        // Authentication disabled: an empty token, not a missing one.
        assert_eq!(parse_socket_status_line("OK ws=up token=none"), Some(""));
        // Anything that is not the status line, or carries no usable token,
        // is ignored rather than guessed at.
        assert_eq!(parse_socket_status_line(&format!("token={TOKEN}")), None);
        assert_eq!(parse_socket_status_line("OK ws=up port=80"), None);
        assert_eq!(
            parse_socket_status_line(&format!("OK ws=up token={}", &TOKEN[..16])),
            None
        );
        assert_eq!(
            parse_socket_status_line(&format!(
                "OK ws=up token={}",
                "0123456789ABCDEF0123456789ABCDEF"
            )),
            None,
            "the bridge writes lowercase"
        );
        assert_eq!(
            parse_socket_status_line(&format!("note token={TOKEN}x")),
            None,
            "the token has to end where the value ends"
        );
    }

    /// The real bridge answers `@s?` with a `token=none` line *before* the one
    /// that carries the token. The web lets every match overwrite the field, so
    /// the last line is the value — taking the first one stores an empty token
    /// and dials a bridge that does require one without any.
    #[test]
    fn the_last_status_line_of_a_reply_carries_the_token() {
        let reply = [
            "OK ws=up token=none".to_string(),
            "@info done".to_string(),
            format!("OK ws=up port=80 clients=0 token={TOKEN} rx=0"),
        ];
        let borrowed: Vec<&str> = reply.iter().map(String::as_str).collect();
        assert_eq!(token_from_reply(borrowed.iter().copied()), Some(TOKEN));

        // A bridge with authentication off reports it as an empty token …
        assert_eq!(token_from_reply(["OK ws=up token=none"]), Some(""));
        // … and nothing to report is not a token at all.
        assert_eq!(token_from_reply(["@info done"]), None);
        assert_eq!(token_from_reply(Vec::<&str>::new()), None);
    }

    #[test]
    fn the_token_command_is_asked_only_where_the_bridge_advertises_it() {
        use crate::protocol::mgmt::{MGMT_CAP_WEBSOCKET, MGMT_CAP_WIFI};
        assert_eq!(socket_query_command(MGMT_CAP_WEBSOCKET), Some("@s?"));
        assert_eq!(
            socket_query_command(MGMT_CAP_WEBSOCKET | MGMT_CAP_WIFI),
            Some("@s?")
        );
        assert_eq!(socket_query_command(MGMT_CAP_WIFI), None);
        assert_eq!(socket_query_command(0), None);
    }

    #[test]
    fn a_capture_replaces_only_what_the_store_handed_out() {
        // Nothing typed yet, and the value the connect-time fill left behind.
        assert!(may_replace_field("", None));
        assert!(may_replace_field("", Some(TOKEN)));
        assert!(may_replace_field(TOKEN, Some(TOKEN)));
        // Everything else was typed by hand and stays.
        assert!(!may_replace_field("0123456789abcdef", Some(TOKEN)));
        assert!(!may_replace_field(TOKEN, None));
    }

    #[test]
    fn redaction_blanks_exactly_a_full_token() {
        assert_eq!(
            redact_secrets(&format!("OK ws=up token={TOKEN}\n")),
            "OK ws=up token=<redacted>\n"
        );
        assert_eq!(
            redact_secrets(&format!("a token={TOKEN} and b token={TOKEN} ")),
            "a token=<redacted> and b token=<redacted> "
        );
        assert_eq!(
            redact_secrets(&format!("token={TOKEN}")),
            "token=<redacted>"
        );
        // The delimiter rule is the web pattern's own: whitespace or the end
        // of the text, so a run that only starts like a token stays visible.
        assert_eq!(
            redact_secrets(&format!("note: token={TOKEN}.")),
            format!("note: token={TOKEN}.")
        );
        // `none`, short numbers and runs of hex that are not 32 wide stay.
        assert_eq!(redact_secrets("token=none"), "token=none");
        assert_eq!(redact_secrets("token=1234"), "token=1234");
        assert_eq!(
            redact_secrets(&format!("token={}x", &TOKEN[..31])),
            format!("token={}x", &TOKEN[..31])
        );
        let untouched = "@info fw version=0.2.0 uptime=12345678901234567890";
        assert_eq!(redact_secrets(untouched), untouched);
    }
}
