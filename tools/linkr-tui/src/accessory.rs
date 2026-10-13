//! Accessory management (`web/accessory_control.js`).
//!
//! These commands configure Linkr Bee itself (UART, WiFi, WebDAV) over the
//! encrypted Bluetooth LE management channel. They never touch the target
//! UART, so they cannot be confused with terminal input. Builders and parsers
//! are pure so the tool layer, the panel and the tests share one definition of
//! every command and reply.

/// The firmware's own limits, mirrored so a command is refused here rather
/// than by the device after a round trip.
pub struct AccessoryLimits;

impl AccessoryLimits {
    pub const MIN_BAUD: u32 = 300;
    pub const MAX_BAUD: u32 = 3_000_000;
    pub const DATA_BITS: [u8; 4] = [5, 6, 7, 8];
    pub const PARITY: [&str; 3] = ["n", "e", "o"];
    pub const STOP_BITS: [u8; 2] = [1, 2];
    pub const FLOW: [&str; 2] = ["none", "rtscts"];
    pub const SSID_MAX: usize = 32;
    pub const PASSWORD_MAX: usize = 64;
    pub const WEBDAV_URL_MAX: usize = 256;
}

pub const DIAGNOSTICS_COMMAND: &str = "@i?";
pub const UART_QUERY_COMMAND: &str = "@u?";
pub const WIFI_QUERY_COMMAND: &str = "@w?";
pub const WEBDAV_QUERY_COMMAND: &str = "@d?";

/// A UART settings set, as both the command builder and the parser use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UartSettings {
    pub baud: u32,
    pub data_bits: u8,
    pub parity: String,
    pub stop_bits: u8,
    pub flow: String,
}

/// Build `@u=<baud>,<data>,<parity>,<stop>,<flow>`.
pub fn set_uart_command(s: &UartSettings) -> Result<String, String> {
    if !(AccessoryLimits::MIN_BAUD..=AccessoryLimits::MAX_BAUD).contains(&s.baud) {
        return Err(format!(
            "baud must be an integer between {} and {}",
            AccessoryLimits::MIN_BAUD,
            AccessoryLimits::MAX_BAUD
        ));
    }
    if !AccessoryLimits::DATA_BITS.contains(&s.data_bits) {
        return Err("dataBits must be 5, 6, 7 or 8".to_string());
    }
    let parity = s.parity.to_ascii_lowercase();
    if !AccessoryLimits::PARITY.contains(&parity.as_str()) {
        return Err("parity must be n, e or o".to_string());
    }
    if !AccessoryLimits::STOP_BITS.contains(&s.stop_bits) {
        return Err("stopBits must be 1 or 2".to_string());
    }
    let flow = s.flow.to_ascii_lowercase();
    if !AccessoryLimits::FLOW.contains(&flow.as_str()) {
        return Err("flow must be none or rtscts".to_string());
    }
    Ok(format!(
        "@u={},{},{},{},{}",
        s.baud, s.data_bits, parity, s.stop_bits, flow
    ))
}

/// Build `@w=<ssid>,<password>` or `@w off`. A comma in an SSID would move the
/// rest of the name into the password in the firmware parser, so it is refused.
pub fn wifi_command(action: &str, ssid: &str, password: &str) -> Result<String, String> {
    let mode = action.to_ascii_lowercase();
    if mode == "off" {
        return Ok("@w off".to_string());
    }
    if mode != "connect" {
        return Err("action must be connect or off".to_string());
    }
    if ssid.trim().is_empty() {
        return Err("ssid is required".to_string());
    }
    if ssid.chars().count() > AccessoryLimits::SSID_MAX {
        return Err(format!(
            "ssid must be at most {} characters",
            AccessoryLimits::SSID_MAX
        ));
    }
    if ssid.contains(',') {
        return Err("ssid must not contain a comma".to_string());
    }
    if ssid.chars().any(|c| c.is_control()) {
        return Err("ssid must not contain control characters".to_string());
    }
    if password.chars().count() > AccessoryLimits::PASSWORD_MAX {
        return Err(format!(
            "password must be at most {} characters",
            AccessoryLimits::PASSWORD_MAX
        ));
    }
    Ok(format!("@w={ssid},{password}"))
}

/// Build `@d=<url>` or `@d off`.
pub fn webdav_command(action: &str, url: &str) -> Result<String, String> {
    let mode = action.to_ascii_lowercase();
    if mode == "off" {
        return Ok("@d off".to_string());
    }
    if mode != "on" {
        return Err("action must be on or off".to_string());
    }
    let target = url.trim();
    if target.is_empty() {
        return Err("url is required when enabling WebDAV upload".to_string());
    }
    if target.chars().count() > AccessoryLimits::WEBDAV_URL_MAX {
        return Err(format!(
            "url must be at most {} characters",
            AccessoryLimits::WEBDAV_URL_MAX
        ));
    }
    let lower = target.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("url must start with http:// or https://".to_string());
    }
    if target.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("url must not contain whitespace or control characters".to_string());
    }
    Ok(format!("@d={target}"))
}

/// The WiFi password travels in the command, so anything shown to the user or
/// returned to the model is redacted. A WebDAV URL may embed credentials too.
pub fn redact_command(command: &str) -> String {
    if let Some(rest) = command.strip_prefix("@w=") {
        return match rest.find(',') {
            Some(comma) => format!("@w={},<redacted>", &rest[..comma]),
            None => command.to_string(),
        };
    }
    if let Some(rest) = command.strip_prefix("@d=") {
        // `@d=<scheme>://<creds>@<host>…` -> credentials become `<redacted>`.
        if let Some(scheme_end) = rest.find("://") {
            let after_scheme = &rest[scheme_end + 3..];
            if let Some(at) = after_scheme.find('@') {
                let (creds, tail) = after_scheme.split_at(at);
                if !creds.is_empty() && !creds.contains('/') {
                    return format!("@d={}://<redacted>{tail}", &rest[..scheme_end]);
                }
            }
        }
        return command.to_string();
    }
    command.to_string()
}

/// Firmware replies begin with `OK` or `ERR`; an ERR line carries the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyStatus {
    pub ok: bool,
    pub error: String,
}

/// Whether a line begins with `word` followed by a word boundary (`\b`): a
/// non-word character or the end of the line.
fn starts_with_word(line: &str, word: &str) -> bool {
    if line.len() < word.len() || !line[..word.len()].eq_ignore_ascii_case(word) {
        return false;
    }
    match line[word.len()..].chars().next() {
        None => true,
        Some(c) => !c.is_alphanumeric() && c != '_',
    }
}

pub fn reply_status(text: &str) -> ReplyStatus {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if let Some(error) = lines.iter().find(|l| starts_with_word(l, "err")) {
        return ReplyStatus {
            ok: false,
            error: (*error).to_string(),
        };
    }
    ReplyStatus {
        ok: lines.iter().any(|l| starts_with_word(l, "ok")),
        error: String::new(),
    }
}

/// `OK uart=115200,8,N,1,none` -> settings.
pub fn parse_uart_settings(text: &str) -> Option<UartSettings> {
    // Find the `uart=` payload and split its five comma fields.
    let idx = text.find("uart=")?;
    let payload: &str = text[idx + 5..].split_whitespace().next()?;
    let mut parts = payload.split(',');
    let baud = parts.next()?.parse().ok()?;
    let data_bits = parts.next()?.parse().ok()?;
    let parity = parts.next()?.to_ascii_lowercase();
    let stop_bits = parts.next()?.parse().ok()?;
    let flow = parts.next()?.to_ascii_lowercase();
    Some(UartSettings {
        baud,
        data_bits,
        parity,
        stop_bits,
        flow,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiStatus {
    pub state: String,
    pub ssid: String,
    pub ip: String,
}

/// `OK wifi=connected,ssid=MyNet,ip=192.168.1.5` or `OK wifi off`. The address
/// is taken from the last `,ip=` because an SSID may itself contain a comma.
pub fn parse_wifi_status(text: &str) -> Option<WifiStatus> {
    let line = text.lines().find(|l| {
        let lower = l.to_ascii_lowercase();
        lower.contains("wifi=") || lower.contains("wifi ")
    })?;
    let lower = line.to_ascii_lowercase();
    if contains_word(&lower, "wifi off") {
        return Some(WifiStatus {
            state: "off".to_string(),
            ssid: String::new(),
            ip: String::new(),
        });
    }
    // IP from the last ",ip=".
    let lower = line.to_ascii_lowercase();
    let (payload, ip) = match lower.rfind(",ip=") {
        Some(i) => (&line[..i], line[i + 4..].trim().to_string()),
        None => (line, String::new()),
    };
    // `wifi=<state>` optionally followed by `,ssid=<ssid>`.
    let idx = lower.find("wifi=")?;
    let payload = &payload[idx + 5..];
    let (state, ssid) = match payload.find(',') {
        Some(i) => (
            &payload[..i],
            payload[i + 1..].strip_prefix("ssid=").unwrap_or(""),
        ),
        None => (payload, ""),
    };
    Some(WifiStatus {
        state: state.to_ascii_lowercase(),
        ssid: if ssid == "-" {
            String::new()
        } else {
            ssid.to_string()
        },
        ip,
    })
}

/// Word-delimited substring search, so `wifi off` does not match `twifi off`.
fn contains_word(haystack: &str, needle: &str) -> bool {
    let mut search = 0;
    while let Some(rel) = haystack[search..].find(needle) {
        let start = search + rel;
        let end = start + needle.len();
        let before_ok = haystack[..start]
            .chars()
            .next_back()
            .map(|c| !c.is_ascii_alphanumeric())
            .unwrap_or(true);
        let after_ok = haystack[end..]
            .chars()
            .next()
            .map(|c| !c.is_ascii_alphanumeric())
            .unwrap_or(true);
        if before_ok && after_ok {
            return true;
        }
        search = start + 1;
    }
    false
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebdavStatus {
    pub state: String,
    pub url: String,
}

/// `OK webdav=on,url=http://host/dav/`. Everything after `url=` is the target,
/// so a URL containing a comma or a query string survives the parse.
pub fn parse_webdav_status(text: &str) -> Option<WebdavStatus> {
    let line = text.lines().find(|l| {
        let lower = l.to_ascii_lowercase();
        lower.contains("webdav=") || lower.contains("webdav ")
    })?;
    let lower = line.to_ascii_lowercase();
    if contains_word(&lower, "webdav off") {
        return Some(WebdavStatus {
            state: "off".to_string(),
            url: String::new(),
        });
    }
    let idx = lower.find("webdav=")?;
    let payload = &line[idx + 7..];
    let (state, url) = match payload.find(",url=") {
        Some(i) => (&payload[..i], payload[i + 5..].trim()),
        None => (payload, ""),
    };
    Some(WebdavStatus {
        state: state.to_ascii_lowercase(),
        url: url.to_string(),
    })
}

/// One-line summary for the approval card, worded in the interface language.
pub fn accessory_change_summary(command: &str, zh: bool) -> String {
    let text = command;
    if let Some(settings) = text.strip_prefix("@u=") {
        return if zh {
            format!("把桥接串口改为 {settings}")
        } else {
            format!("Set the bridge UART to {settings}")
        };
    }
    if let Some(rest) = text.strip_prefix("@w=") {
        let ssid = match rest.find(',') {
            Some(i) => &rest[..i],
            None => rest,
        };
        return if zh {
            format!("连接 WiFi「{ssid}」（密码不显示）")
        } else {
            format!("Join WiFi \"{ssid}\" (password hidden)")
        };
    }
    if text == "@w off" {
        return if zh {
            "断开 WiFi 并清除当前配置".to_string()
        } else {
            "Disconnect WiFi and clear the current configuration".to_string()
        };
    }
    if let Some(url) = text.strip_prefix("@d=") {
        return if zh {
            format!("启用日志上传到 {url}")
        } else {
            format!("Enable log upload to {url}")
        };
    }
    if text == "@d off" {
        return if zh {
            "关闭 WebDAV 日志上传".to_string()
        } else {
            "Disable WebDAV log upload".to_string()
        };
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uart_settings_round_trip() {
        let cmd = set_uart_command(&UartSettings {
            baud: 115200,
            data_bits: 8,
            parity: "N".to_string(),
            stop_bits: 1,
            flow: "none".to_string(),
        })
        .unwrap();
        assert_eq!(cmd, "@u=115200,8,n,1,none");
        let parsed = parse_uart_settings("OK uart=115200,8,N,1,none").unwrap();
        assert_eq!(parsed.baud, 115200);
        assert_eq!(parsed.parity, "n");
        assert_eq!(parsed.flow, "none");
    }

    #[test]
    fn uart_bounds_are_enforced_here_not_on_the_device() {
        let mut s = UartSettings {
            baud: 299,
            data_bits: 8,
            parity: "n".into(),
            stop_bits: 1,
            flow: "none".into(),
        };
        assert!(set_uart_command(&s).is_err());
        s.baud = 3_000_001;
        assert!(set_uart_command(&s).is_err());
        s.baud = 115200;
        s.parity = "x".into();
        assert!(set_uart_command(&s).is_err());
    }

    #[test]
    fn wifi_commands_and_their_redaction() {
        assert_eq!(
            wifi_command("connect", "MyNet", "pw").unwrap(),
            "@w=MyNet,pw"
        );
        assert_eq!(wifi_command("off", "", "").unwrap(), "@w off");
        assert!(wifi_command("connect", "", "pw").is_err());
        assert!(wifi_command("connect", "a,b", "pw").is_err());
        assert!(wifi_command("connect", "x".repeat(33).as_str(), "").is_err());
        // The password is never shown.
        assert_eq!(redact_command("@w=MyNet,pw"), "@w=MyNet,<redacted>");
        assert_eq!(redact_command("@w=MyNet"), "@w=MyNet");
    }

    #[test]
    fn webdav_commands_and_url_credential_redaction() {
        assert_eq!(
            webdav_command("on", "https://host/dav/").unwrap(),
            "@d=https://host/dav/"
        );
        assert_eq!(webdav_command("off", "").unwrap(), "@d off");
        assert!(webdav_command("on", "ftp://host").is_err());
        assert!(webdav_command("on", "https://host/dav/ with space").is_err());
        assert_eq!(
            redact_command("@d=https://user:pw@host/dav/"),
            "@d=https://<redacted>@host/dav/"
        );
    }

    #[test]
    fn reply_status_reads_ok_and_err() {
        assert!(reply_status("OK").ok);
        assert!(reply_status("ok wifi=connected").ok);
        let bad = reply_status("ERR ssid too long");
        assert!(!bad.ok);
        assert_eq!(bad.error, "ERR ssid too long");
        assert!(!reply_status("nothing here").ok);
    }

    #[test]
    fn wifi_status_parsing_including_commas_in_the_ssid() {
        let s = parse_wifi_status("OK wifi=connected,ssid=My,Net,ip=192.168.1.5").unwrap();
        assert_eq!(s.state, "connected");
        assert_eq!(s.ssid, "My,Net");
        assert_eq!(s.ip, "192.168.1.5");
        let off = parse_wifi_status("OK wifi off").unwrap();
        assert_eq!(off.state, "off");
        assert_eq!(off.ssid, "");
        let dash = parse_wifi_status("OK wifi=connected,ssid=-").unwrap();
        assert_eq!(dash.ssid, "");
    }

    #[test]
    fn webdav_status_parsing_keeps_the_whole_url() {
        let s = parse_webdav_status("OK webdav=on,url=https://host/dav/?a=1").unwrap();
        assert_eq!(s.state, "on");
        assert_eq!(s.url, "https://host/dav/?a=1");
        let off = parse_webdav_status("OK webdav off").unwrap();
        assert_eq!(off.state, "off");
    }

    #[test]
    fn approval_summaries_are_worded_per_language_and_never_show_a_password() {
        assert_eq!(
            accessory_change_summary("@w=MyNet,pw", true),
            "连接 WiFi「MyNet」（密码不显示）"
        );
        assert_eq!(
            accessory_change_summary("@w=MyNet,pw", false),
            "Join WiFi \"MyNet\" (password hidden)"
        );
        assert_eq!(
            accessory_change_summary("@w off", true),
            "断开 WiFi 并清除当前配置"
        );
        assert_eq!(
            accessory_change_summary("@d=https://h/d/", false),
            "Enable log upload to https://h/d/"
        );
    }
}
