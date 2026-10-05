//! Shared parsers for management reply lines (WEB_UX_SPEC section 5.3).
//!
//! Every TUI surface that shows a reply uses these — never a private copy —
//! so `OK uart=…`, `OK wifi=…`, `OK webdav=…`, `@scan` lines and the
//! `replyStatus` rule behave exactly like the web client.
//!
//! The parsers keep every wire token verbatim; only the two helpers that
//! *word* a parsed state on screen ([`wifi_state_text`] /
//! [`webdav_state_text`]) follow the interface language.

use super::i18n::{strings, t, Lang};
use regex::Regex;
use std::sync::OnceLock;

strings! {
    RPL_STATE_CONNECTED => "connected", "已连接";
    RPL_STATE_CONNECTING => "connecting", "连接中";
    RPL_STATE_OFF => "off", "关闭";
    RPL_STATE_ON => "on", "开启";
    RPL_STATE_ERROR => "error", "错误";
    RPL_STATE_FAILED => "failed", "失败";
    RPL_STATE_UNKNOWN => "unknown", "未知";
}

/// How an `OK wifi=…` state is worded in a feedback line. The known states
/// get the localized wording, anything else the firmware reports is passed
/// through untouched (it is data, not interface text).
pub fn wifi_state_text(state: &str, lang: Lang) -> &str {
    match state {
        "connected" => t(RPL_STATE_CONNECTED, lang),
        "connecting" => t(RPL_STATE_CONNECTING, lang),
        "off" => t(RPL_STATE_OFF, lang),
        "error" => t(RPL_STATE_ERROR, lang),
        "failed" => t(RPL_STATE_FAILED, lang),
        "unknown" => t(RPL_STATE_UNKNOWN, lang),
        other => other,
    }
}

/// Same for an `OK webdav=…` state (`on` / `off` are its documented values).
pub fn webdav_state_text(state: &str, lang: Lang) -> &str {
    match state {
        "on" => t(RPL_STATE_ON, lang),
        "off" => t(RPL_STATE_OFF, lang),
        other => other,
    }
}

/// Result of the `replyStatus(text)` helper: the first `^ERR` line wins,
/// otherwise `ok` when any `^OK` line exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyStatus {
    pub ok: bool,
    pub error: Option<String>,
}

pub fn reply_status(text: &str) -> ReplyStatus {
    let mut saw_ok = false;
    let mut error = None;
    for line in text.lines() {
        let line = line.trim_end();
        if line.starts_with("ERR") {
            if error.is_none() {
                error = Some(line.to_string());
            }
        } else if line.starts_with("OK") {
            saw_ok = true;
        }
    }
    ReplyStatus {
        ok: error.is_none() && saw_ok,
        error,
    }
}

/// `OK uart=115200,8,N,1,none` — the web `parseUartSettings`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UartSettings {
    pub baud: u64,
    pub data_bits: u8,
    /// Lowercase parity letter: `n` | `e` | `o`.
    pub parity: String,
    pub stop_bits: u8,
    /// Lowercase flow: `none` | `rtscts`.
    pub flow: String,
}

impl std::fmt::Display for UartSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{},{},{},{},{}",
            self.baud, self.data_bits, self.parity, self.stop_bits, self.flow
        )
    }
}

pub fn parse_uart_reply(text: &str) -> Option<UartSettings> {
    for line in text.lines() {
        // Echoed commands and prompts sit next to the reply: only the `OK`
        // line is parsed, the rest of the page is skipped (like the wifi reply).
        let Some(rest) = line.trim().strip_prefix("OK uart=") else {
            continue;
        };
        let mut parts = rest.split(',');
        let baud = parts.next()?.trim().parse().ok()?;
        let data_bits = parts.next()?.trim().parse().ok()?;
        let parity = parts.next()?.trim().to_lowercase();
        let stop_bits = parts.next()?.trim().parse().ok()?;
        let flow = parts.next()?.trim().to_lowercase();
        if !matches!(parity.as_str(), "n" | "e" | "o") {
            return None;
        }
        let flow = match flow.as_str() {
            "none" | "n" => "none".to_string(),
            "rtscts" | "r" => "rtscts".to_string(),
            other => other.to_string(),
        };
        return Some(UartSettings {
            baud,
            data_bits,
            parity,
            stop_bits,
            flow,
        });
    }
    None
}

/// `OK wifi=connected,ssid=MyNet,ip=192.168.1.5` / `OK wifi off`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiStatus {
    /// `connected` | `connecting` | `off` | …
    pub state: String,
    pub ssid: String,
    pub ip: String,
}

pub fn parse_wifi_reply(text: &str) -> Option<WifiStatus> {
    for line in text.lines() {
        let line = line.trim();
        if line == "OK wifi off" {
            return Some(WifiStatus {
                state: "off".to_string(),
                ssid: String::new(),
                ip: String::new(),
            });
        }
        let rest = match line.strip_prefix("OK wifi=") {
            Some(rest) => rest,
            None => continue,
        };
        let mut state = String::new();
        let mut ssid = String::new();
        let mut ip = String::new();
        for field in rest.split(',') {
            let mut it = field.splitn(2, '=');
            let key = it.next().unwrap_or("");
            let value = it.next().unwrap_or("");
            match key {
                "" => {}
                "ssid" => {
                    ssid = if value == "-" {
                        String::new()
                    } else {
                        value.to_string()
                    }
                }
                // The IP is taken from the last `,ip=` field.
                "ip" => ip = value.to_string(),
                _ => {
                    if state.is_empty() && !field.contains('=') {
                        state = field.to_string();
                    } else if !key.is_empty() && state.is_empty() {
                        state = key.to_string();
                    }
                }
            }
        }
        // First field is the bare state (`connected,ssid=…`).
        if state.is_empty() {
            state = rest
                .split(',')
                .next()
                .unwrap_or("")
                .split('=')
                .next()
                .unwrap_or("")
                .to_string();
        }
        if state.is_empty() {
            state = "unknown".to_string();
        }
        return Some(WifiStatus { state, ssid, ip });
    }
    None
}

/// `OK webdav=on,url=http://host/dav/` / `OK webdav off`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebdavStatus {
    pub state: String,
    pub url: String,
}

pub fn parse_webdav_reply(text: &str) -> Option<WebdavStatus> {
    for line in text.lines() {
        let line = line.trim();
        if line == "OK webdav off" {
            return Some(WebdavStatus {
                state: "off".to_string(),
                url: String::new(),
            });
        }
        let Some(rest) = line.strip_prefix("OK webdav=") else {
            continue;
        };
        let mut state = String::new();
        let mut url = String::new();
        for field in rest.split(',') {
            if let Some(value) = field.strip_prefix("url=") {
                url = value.to_string();
            } else if !field.contains('=') && state.is_empty() {
                state = field.to_string();
            } else if let Some(key) = field.split('=').next() {
                if state.is_empty() {
                    state = key.to_string();
                }
            }
        }
        return Some(WebdavStatus { state, url });
    }
    None
}

/// One `@scan result <ssid> [-N dBm] [ch=N] [security]` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanResult {
    pub ssid: String,
    pub rssi: Option<i32>,
    pub channel: Option<u32>,
    pub security: Option<String>,
}

/// The patterns `web/app.js` matches with in `parseWifiScanResult`,
/// compiled once: the RSSI carries a `dB`/`dBm` suffix, the channel is
/// `ch=N`, and the security is one of the tokens of WEB_UX_SPEC §5.3.
struct ScanPats {
    rssi: Regex,
    channel: Regex,
    security: Regex,
    numbering: Regex,
}

fn scan_pats() -> &'static ScanPats {
    static PATS: OnceLock<ScanPats> = OnceLock::new();
    PATS.get_or_init(|| ScanPats {
        rssi: Regex::new(r"(?i)\s+(-?\d+)\s*dBm?$").expect("rssi pattern is valid"),
        channel: Regex::new(r"(?i)\s+ch=(\d+)$").expect("channel pattern is valid"),
        security: Regex::new(r"(?i)\s+(open|wep|wpa|wpa2|wpa2-sha256|wpa3|eap|wapi|unknown)$")
            .expect("security pattern is valid"),
        numbering: Regex::new(r"^\d+[).]\s*").expect("numbering pattern is valid"),
    })
}

/// Match `re` at the **right** edge of `value`, cut the match off — the web
/// does `value.slice(0, match.index).trim()` — and return its first group.
fn peel(re: &Regex, value: &mut String) -> Option<String> {
    let (start, group) = {
        let caps = re.captures(value)?;
        let start = caps.get(0)?.start();
        (start, caps.get(1).map(|m| m.as_str().to_string()))
    };
    value.truncate(start);
    value.truncate(value.trim_end().len());
    group
}

/// Drop one leading and one trailing quote (web `replace(/^["']|["']$/g, "")`).
fn strip_quotes(value: &str) -> &str {
    let mut out = value;
    if let Some(rest) = out.strip_prefix(['"', '\'']) {
        out = rest;
    }
    if let Some(rest) = out.strip_suffix(['"', '\'']) {
        out = rest;
    }
    out
}

/// Parse one `@scan result …` line exactly the way the web client does
/// (`web/app.js` → `parseWifiScanResult`): peel the known fields **off the
/// right** and keep whatever is left whole, so an SSID that contains spaces
/// survives — the old left-to-right split stopped the SSID at the first
/// space (K7). The SSID guards are the web's too: at most 32 UTF-16 units
/// (its `.length`), nothing starting with `[` or `@`, no leading `1)`
/// numbering, no surrounding quotes and never `<hidden>`.
pub fn parse_scan_line(line: &str) -> Option<ScanResult> {
    let rest = line.trim().strip_prefix("@scan result ")?.trim();
    let pats = scan_pats();
    let mut value = rest.to_string();

    let rssi = peel(&pats.rssi, &mut value).and_then(|v| v.parse().ok());
    let channel = peel(&pats.channel, &mut value).and_then(|v| v.parse().ok());
    let security = peel(&pats.security, &mut value).map(|v| v.to_lowercase());

    if value.is_empty() || value.encode_utf16().count() > 32 {
        return None;
    }
    if value.starts_with('[') || value.starts_with('@') {
        return None;
    }
    let value = pats.numbering.replace(&value, "").to_string();
    let ssid = strip_quotes(&value).trim();
    if ssid.is_empty() || ssid == "<hidden>" {
        return None;
    }
    Some(ScanResult {
        ssid: ssid.to_string(),
        rssi,
        channel,
        security,
    })
}

/// Redact secrets for display (`redactCommand`/`redactSecrets` in the web
/// client): `@w=`/`@d=` payloads and 32-hex tokens never reach the screen.
pub fn redact_command(cmd: &str) -> String {
    if cmd.starts_with("@w=") {
        return "@w=<redacted>".to_string();
    }
    if cmd.starts_with("@d=") {
        return "@d=<redacted>".to_string();
    }
    cmd.to_string()
}

pub fn redact_secrets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&redact_line(line));
    }
    out
}

fn redact_line(line: &str) -> String {
    let mut result = line.to_string();
    // `token=<32 hex>` → `token=<redacted>`
    if let Some(pos) = result.find("token=") {
        let after = &result[pos + "token=".len()..];
        let hex_len = after.chars().take_while(|c| c.is_ascii_hexdigit()).count();
        if hex_len == 32 {
            result.replace_range(
                pos + "token=".len()..pos + "token=".len() + hex_len,
                "<redacted>",
            );
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_status_prefers_err_then_ok() {
        let status = reply_status("OK uart=115200,8,N,1,none");
        assert!(status.ok);
        assert_eq!(status.error, None);
        let status = reply_status("ERR format: @u=115200,8,n,1,n");
        assert!(!status.ok);
        assert_eq!(
            status.error.as_deref(),
            Some("ERR format: @u=115200,8,n,1,n")
        );
        let status = reply_status("ERR first\nERR second\nOK anyway");
        assert!(!status.ok);
        assert_eq!(status.error.as_deref(), Some("ERR first"));
        let status = reply_status("nothing useful");
        assert!(!status.ok);
        assert_eq!(status.error, None);
    }

    #[test]
    fn uart_reply_parses_the_documented_sample() {
        let parsed = parse_uart_reply("OK uart=115200,8,N,1,none").unwrap();
        assert_eq!(
            parsed,
            UartSettings {
                baud: 115200,
                data_bits: 8,
                parity: "n".to_string(),
                stop_bits: 1,
                flow: "none".to_string(),
            }
        );
        assert_eq!(parsed.to_string(), "115200,8,n,1,none");
        let parsed = parse_uart_reply("OK uart=1500000,7,E,2,rtscts").unwrap();
        assert_eq!(parsed.baud, 1_500_000);
        assert_eq!(parsed.parity, "e");
        assert_eq!(parsed.flow, "rtscts");
        assert!(parse_uart_reply("ERR nope").is_none());
    }

    #[test]
    fn wifi_reply_parses_connected_and_off() {
        let status = parse_wifi_reply("OK wifi=connected,ssid=MyNet,ip=192.168.1.5").unwrap();
        assert_eq!(status.state, "connected");
        assert_eq!(status.ssid, "MyNet");
        assert_eq!(status.ip, "192.168.1.5");
        let status = parse_wifi_reply("OK wifi off").unwrap();
        assert_eq!(status.state, "off");
        assert_eq!(status.ssid, "");
        // SSID `-` becomes empty; the last `,ip=` wins.
        let status = parse_wifi_reply("OK wifi=connected,ssid=-,ip=old,ip=10.0.0.9").unwrap();
        assert_eq!(status.ssid, "");
        assert_eq!(status.ip, "10.0.0.9");
        assert!(parse_wifi_reply("ERR bad").is_none());
    }

    #[test]
    fn webdav_reply_parses_on_and_off() {
        let status = parse_webdav_reply("OK webdav=on,url=http://host/dav/").unwrap();
        assert_eq!(status.state, "on");
        assert_eq!(status.url, "http://host/dav/");
        let status = parse_webdav_reply("OK webdav off").unwrap();
        assert_eq!(status.state, "off");
        assert!(parse_webdav_reply("ERR nope").is_none());
    }

    #[test]
    fn scan_lines_parse_all_documented_shapes() {
        // Firmware order (`src/wifi.c`: `@scan result %.*s %s ch=%u %ddBm`).
        // The RSSI always carries its `dBm`, which is also what the web's
        // pattern `/\s+(-?\d+)\s*dBm?$/i` and WEB_UX_SPEC's `[-N dBm]`
        // require — a bare `-54` is emitted by nobody, so the sample this
        // test used to use could not occur on the wire.
        let r = parse_scan_line("@scan result MyNet wpa2 ch=6 -54dBm").unwrap();
        assert_eq!(r.ssid, "MyNet");
        assert_eq!(r.rssi, Some(-54));
        assert_eq!(r.channel, Some(6));
        assert_eq!(r.security.as_deref(), Some("wpa2"));
        let r = parse_scan_line("@scan result OpenNet open").unwrap();
        assert_eq!(r.ssid, "OpenNet");
        assert_eq!(r.rssi, None);
        assert_eq!(r.channel, None);
        assert_eq!(r.security.as_deref(), Some("open"));
        assert!(parse_scan_line("@scan done").is_none());
        assert!(parse_scan_line("@scan error").is_none());
    }

    /// K7: an SSID may contain spaces. The web peels the known fields off the
    /// right edge (`web/app.js` → `parseWifiScanResult`) and keeps the rest
    /// whole; the old parser took the first whitespace-delimited token, so
    /// `My Home Network` came out as `My` and everything after the first
    /// space was dropped.
    #[test]
    fn an_ssid_with_spaces_is_kept_whole() {
        let r = parse_scan_line("@scan result My Home Network wpa2 ch=6 -48dBm").unwrap();
        assert_eq!(r.ssid, "My Home Network");
        assert_eq!(r.rssi, Some(-48));
        assert_eq!(r.channel, Some(6));
        assert_eq!(r.security.as_deref(), Some("wpa2"));

        // The other shape the spec spells out (`WEB_UX_SPEC.md` §5: ssid,
        // rssi, ch, security) with the fields in the order the firmware
        // really emits them (`src/wifi.c`: `%.*s %s ch=%u %ddBm` — the RSSI
        // is last, which is exactly where the web's pattern looks for it):
        // the peeling is anchored at the right edge, so it stays whole.
        let r = parse_scan_line("@scan result Cafe Free WiFi open ch=1 -60 dBm").unwrap();
        assert_eq!(r.ssid, "Cafe Free WiFi");
        assert_eq!(r.rssi, Some(-60));
        assert_eq!(r.channel, Some(1));
        assert_eq!(r.security.as_deref(), Some("open"));
    }

    /// The SSID guards are the web's, byte for byte: 32 UTF-16 units (its
    /// `.length`), no `[`/`@` prefix, no `1)` numbering, no quotes, and
    /// `<hidden>` is not a name worth showing.
    #[test]
    fn the_scan_parser_rejects_what_the_web_rejects() {
        assert!(parse_scan_line("@scan result").is_none());
        assert!(parse_scan_line("@scan result <hidden> open").is_none());
        assert!(parse_scan_line("@scan result [redacted] open").is_none());
        assert!(parse_scan_line("@scan result @home open").is_none());
        let too_long = "x".repeat(33);
        assert!(parse_scan_line(&format!("@scan result {too_long} open")).is_none());
        assert!(parse_scan_line(&format!("@scan result {} open", "x".repeat(32))).is_some());

        let r = parse_scan_line("@scan result 2. Guest WiFi open").unwrap();
        assert_eq!(r.ssid, "Guest WiFi");
        let r = parse_scan_line("@scan result \"Quoted Net\" wpa2").unwrap();
        assert_eq!(r.ssid, "Quoted Net");
    }

    /// The parsed states are the only thing here that reaches the screen as
    /// words; the parsers themselves stay byte-for-byte on the wire tokens.
    #[test]
    fn state_words_follow_the_language_and_pass_unknown_values_through() {
        assert_eq!(wifi_state_text("connected", Lang::En), "connected");
        assert_eq!(wifi_state_text("connected", Lang::Zh), "已连接");
        assert_eq!(wifi_state_text("off", Lang::Zh), "关闭");
        assert_eq!(wifi_state_text("unknown", Lang::Zh), "未知");
        assert_eq!(wifi_state_text("dhcp", Lang::Zh), "dhcp");
        assert_eq!(webdav_state_text("on", Lang::Zh), "开启");
        assert_eq!(webdav_state_text("on", Lang::En), "on");
        assert_eq!(webdav_state_text("weird", Lang::Zh), "weird");
    }

    #[test]
    fn every_replies_message_is_translated() {
        super::super::i18n::assert_bilingual(ALL);
        assert!(ALL.len() >= 7, "replies carries 7 state words");
    }

    #[test]
    fn redaction_matches_the_web_client() {
        assert_eq!(redact_command("@w=ssid,secret"), "@w=<redacted>");
        assert_eq!(
            redact_command("@d=http://user:pass@host/d"),
            "@d=<redacted>"
        );
        assert_eq!(redact_command("@u?"), "@u?");
        let token = "0123456789abcdef0123456789abcdef";
        let redacted = redact_secrets(&format!("OK ws=up token={token}"));
        assert_eq!(redacted, "OK ws=up token=<redacted>");
        assert_eq!(redact_secrets("token=short"), "token=short");
        assert_eq!(redact_secrets("a\nb"), "a\nb");
    }
}
