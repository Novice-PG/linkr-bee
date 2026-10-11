//! Accessory management: builders and parsers shared by the tool layer and
//! the approval card. Port of the pure parts of `web/accessory_control.js`.
//!
//! These commands configure Linkr Bee itself (UART format, WiFi, WebDAV) over
//! the encrypted management channel. They never touch the target UART, so they
//! cannot be confused with terminal input: every change needs one explicit
//! approval in every execution mode, and credentials are redacted everywhere
//! they could be displayed.

use std::collections::BTreeMap;

use regex::Regex;
use serde_json::{json, Value};
use std::sync::{LazyLock, OnceLock};

/// Mirrors the firmware limits: `parse_uart_line()` / `linkr_wifi_set_config_op`
/// reject anything outside these, and the Kconfig maxima bound the strings.
pub static ACCESSORY_LIMITS: LazyLock<Vec<(&'static str, Value)>> = LazyLock::new(|| {
    vec![
        ("minBaud", json!(300)),
        ("maxBaud", json!(3_000_000)),
        ("ssidMax", json!(32)),
        ("passwordMax", json!(64)),
        ("webdavUrlMax", json!(256)),
    ]
});

pub const MIN_BAUD: u32 = 300;
pub const MAX_BAUD: u32 = 3_000_000;
pub const SSID_MAX: usize = 32;
pub const PASSWORD_MAX: usize = 64;
pub const WEBDAV_URL_MAX: usize = 256;

pub const DIAGNOSTICS_COMMAND: &str = "@i?";
pub const UART_QUERY_COMMAND: &str = "@u?";
pub const WIFI_QUERY_COMMAND: &str = "@w?";
pub const WEBDAV_QUERY_COMMAND: &str = "@d?";

fn has_control_chars(text: &str) -> bool {
    text.bytes().any(|b| b < 0x20 || b == 0x7f)
}

fn has_whitespace_or_control(text: &str) -> bool {
    text.chars()
        .any(|c| c.is_whitespace() || (c as u32) < 0x20 || c as u32 == 0x7f)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UartSettings {
    pub baud: u32,
    pub data_bits: u8,
    pub parity: String,
    pub stop_bits: u8,
    pub flow: String,
}

/// `@u=<baud>,<dataBits>,<parity>,<stopBits>,<flow>`.
pub fn set_uart_command(
    baud: u32,
    data_bits: Option<u8>,
    parity: Option<&str>,
    stop_bits: Option<u8>,
    flow: Option<&str>,
) -> Result<String, String> {
    if !(MIN_BAUD..=MAX_BAUD).contains(&baud) {
        return Err(format!(
            "baud must be an integer between {} and {}",
            MIN_BAUD, MAX_BAUD
        ));
    }
    let data_bits = data_bits.unwrap_or(8);
    if ![5u8, 6, 7, 8].contains(&data_bits) {
        return Err("dataBits must be 5, 6, 7 or 8".to_string());
    }
    let parity_value = parity.unwrap_or("n").to_lowercase();
    if !["n", "e", "o"].contains(&parity_value.as_str()) {
        return Err("parity must be n, e or o".to_string());
    }
    let stop_bits = stop_bits.unwrap_or(1);
    if stop_bits != 1 && stop_bits != 2 {
        return Err("stopBits must be 1 or 2".to_string());
    }
    let flow_value = flow.unwrap_or("none").to_lowercase();
    if !["none", "rtscts"].contains(&flow_value.as_str()) {
        return Err("flow must be none or rtscts".to_string());
    }
    Ok(format!(
        "@u={},{},{},{},{}",
        baud, data_bits, parity_value, stop_bits, flow_value
    ))
}

/// A comma separates SSID from password in the firmware parser, so an SSID
/// containing one would silently move the rest of the name into the password.
pub fn wifi_command(
    action: &str,
    ssid: Option<&str>,
    password: Option<&str>,
) -> Result<String, String> {
    let mode = action.to_lowercase();
    if mode == "off" {
        return Ok("@w off".to_string());
    }
    if mode != "connect" {
        return Err("action must be connect or off".to_string());
    }
    let name = ssid.unwrap_or("");
    if name.trim().is_empty() {
        return Err("ssid is required".to_string());
    }
    if name.chars().count() > SSID_MAX {
        return Err(format!("ssid must be at most {} characters", SSID_MAX));
    }
    if name.contains(',') {
        return Err("ssid must not contain a comma".to_string());
    }
    if has_control_chars(name) {
        return Err("ssid must not contain control characters".to_string());
    }
    let password = password.unwrap_or("");
    if password.chars().count() > PASSWORD_MAX {
        return Err(format!(
            "password must be at most {} characters",
            PASSWORD_MAX
        ));
    }
    // The command is newline-terminated on the wire, so a control character in
    // the password would truncate the command or inject a second one.
    if has_control_chars(password) {
        return Err("password must not contain control characters".to_string());
    }
    Ok(format!("@w={},{}", name, password))
}

pub fn webdav_command(action: &str, url: Option<&str>) -> Result<String, String> {
    let mode = action.to_lowercase();
    if mode == "off" {
        return Ok("@d off".to_string());
    }
    if mode != "on" {
        return Err("action must be on or off".to_string());
    }
    let target = url.unwrap_or("").trim();
    if target.is_empty() {
        return Err("url is required when enabling WebDAV upload".to_string());
    }
    if target.chars().count() > WEBDAV_URL_MAX {
        return Err(format!("url must be at most {} characters", WEBDAV_URL_MAX));
    }
    let lower = target.to_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("url must start with http:// or https://".to_string());
    }
    if has_whitespace_or_control(target) {
        return Err("url must not contain whitespace or control characters".to_string());
    }
    Ok(format!("@d={}", target))
}

/// The WiFi password travels in the command, so anything shown to the user or
/// returned to the model is redacted. A WebDAV URL may embed credentials too.
pub fn redact_command(command: &str) -> String {
    if command.starts_with("@w=") {
        return match command.find(',') {
            Some(comma) => format!("{},<redacted>", &command[..comma]),
            None => command.to_string(),
        };
    }
    if command.starts_with("@d=") {
        static RE: OnceLock<Regex> = OnceLock::new();
        let re = RE.get_or_init(|| Regex::new(r"(?i)^(@d=[a-z]+://)[^/@\s]*@").unwrap());
        return re
            .replace(command, |caps: &regex::Captures| {
                format!("{}<redacted>@", &caps[1])
            })
            .to_string();
    }
    command.to_string()
}

fn starts_word(line: &str, prefix: &str) -> bool {
    match line.strip_prefix(prefix) {
        Some(rest) => rest
            .chars()
            .next()
            .map(|c| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(true),
        None => false,
    }
}

/// Firmware replies begin with `OK` or `ERR`; an `ERR` line carries the reason
/// the model needs to choose a different action.
pub fn reply_status(text: &str) -> (bool, String) {
    let lines: Vec<&str> = text
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if let Some(error) = lines.iter().find(|line| starts_word(line, "ERR")) {
        return (false, (*error).to_string());
    }
    let ok = lines.iter().any(|line| starts_word(line, "OK"));
    (ok, String::new())
}

/// `"OK uart=115200,8,N,1,none"`.
pub fn parse_uart_settings(text: &str) -> Option<UartSettings> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"\buart=([0-9]+),([0-9]+),([a-zA-Z]+),([0-9]+),([a-z]+)").unwrap()
    });
    let caps = re.captures(text)?;
    Some(UartSettings {
        baud: caps[1].parse().ok()?,
        data_bits: caps[2].parse().ok()?,
        parity: caps[3].to_lowercase(),
        stop_bits: caps[4].parse().ok()?,
        flow: caps[5].to_lowercase(),
    })
}

/// `"OK wifi=connected,ssid=MyNet,ip=192.168.1.5"` or `"OK wifi off"`. The
/// address comes from the last `,ip=` because an SSID may itself contain one.
pub fn parse_wifi_status(text: &str) -> Option<(String, String, String)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let line = text.split(['\r', '\n']).find(|candidate| {
        let lower = candidate.to_lowercase();
        lower.contains("wifi=") || lower.contains("wifi ")
    })?;
    if line.to_lowercase().contains("wifi off") {
        return Some(("off".into(), String::new(), String::new()));
    }
    let re = RE.get_or_init(|| Regex::new(r"(?i)wifi=([^,]+)(?:,ssid=(.*))?").unwrap());
    let mut payload = line.trim().to_string();
    let mut ip = String::new();
    if let Some(index) = payload.rfind(",ip=") {
        ip = payload[index + 4..].trim().to_string();
        payload = payload[..index].to_string();
    }
    let caps = re.captures(&payload)?;
    let ssid = match caps.get(2) {
        Some(value) if value.as_str() != "-" => value.as_str().to_string(),
        _ => String::new(),
    };
    Some((caps[1].to_lowercase(), ssid, ip))
}

/// `"OK webdav=on,url=http://host/dav/"`. Everything after `url=` is the
/// target, so a URL containing a query string survives the parse.
pub fn parse_webdav_status(text: &str) -> Option<(String, String)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let line = text.split(['\r', '\n']).find(|candidate| {
        let lower = candidate.to_lowercase();
        lower.contains("webdav=") || lower.contains("webdav ")
    })?;
    if line.to_lowercase().contains("webdav off") {
        return Some(("off".into(), String::new()));
    }
    let re = RE.get_or_init(|| Regex::new(r"(?i)webdav=([a-z_]+)(?:,url=(.*))?").unwrap());
    let caps = re.captures(line)?;
    Some((
        caps[1].to_lowercase(),
        caps.get(2)
            .map(|value| value.as_str().trim().to_string())
            .unwrap_or_default(),
    ))
}

fn fields(payload: &str) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    for token in payload.split_whitespace() {
        if let Some(separator) = token.find('=') {
            if separator > 0 {
                found.insert(
                    token[..separator].to_string(),
                    token[separator + 1..].to_string(),
                );
            }
        }
    }
    found
}

/// `@i?` answers with several `@info <group> key=value …` frames terminated by
/// `@info done`; this only shapes them, the caller accumulates the lines.
pub fn parse_info_groups(lines: &[String]) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut groups: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for raw in lines {
        let line = raw.trim();
        let Some(payload) = line.strip_prefix("@info ") else {
            continue;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "done" {
            continue;
        }
        let mut parts = payload.split_whitespace();
        let Some(group) = parts.next() else { continue };
        let rest: Vec<&str> = parts.collect();
        let entry = groups.entry(group.to_string()).or_default();
        for (key, value) in fields(&rest.join(" ")) {
            entry.insert(key, value);
        }
    }
    groups
}

pub fn is_diagnostics_done(line: &str) -> bool {
    line.trim() == "@info done"
}

/// One-line summary for the approval card; the command itself is shown
/// redacted so approving never displays a credential.
pub fn accessory_change_summary(command: &str, zh: bool) -> String {
    if let Some(settings) = command.strip_prefix("@u=") {
        return if zh {
            format!("把桥接串口改为 {}", settings)
        } else {
            format!("Set the bridge UART to {}", settings)
        };
    }
    if let Some(rest) = command.strip_prefix("@w=") {
        let ssid = match rest.find(',') {
            Some(comma) => &rest[..comma],
            None => rest,
        };
        return if zh {
            format!("连接 WiFi「{}」（密码不显示）", ssid)
        } else {
            format!("Join WiFi \"{}\" (password hidden)", ssid)
        };
    }
    if command == "@w off" {
        return if zh {
            "断开 WiFi 并清除当前配置".to_string()
        } else {
            "Disconnect WiFi and clear the current configuration".to_string()
        };
    }
    if let Some(url) = command.strip_prefix("@d=") {
        return if zh {
            format!("启用日志上传到 {}", url)
        } else {
            format!("Enable log upload to {}", url)
        };
    }
    if command == "@d off" {
        return if zh {
            "关闭 WebDAV 日志上传".to_string()
        } else {
            "Disable WebDAV log upload".to_string()
        };
    }
    command.to_string()
}

/// Every mutating accessory action needs one explicit approval, in every
/// execution mode (spec §4.6).
pub fn require_approval(_action: &str) -> bool {
    true
}

pub fn capability_missing_message(capability: &str) -> String {
    format!(
        "This accessory does not report {} capability. The change was not requested.",
        capability
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const JS: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../web/accessory_control.js"
    );
    const SPEC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/specs/AGENT_SPEC.md");

    fn js() -> String {
        std::fs::read_to_string(JS).expect("read web/accessory_control.js")
    }

    #[test]
    fn uart_builder_validation_messages_are_exact() {
        let source = js();
        assert!(source.contains("minBaud: 300"));
        assert!(source.contains("maxBaud: 3000000"));
        assert_eq!(
            set_uart_command(115_200, None, None, None, None).unwrap(),
            "@u=115200,8,n,1,none"
        );
        assert_eq!(
            set_uart_command(200, None, None, None, None).unwrap_err(),
            "baud must be an integer between 300 and 3000000"
        );
        assert_eq!(
            set_uart_command(9600, Some(9), None, None, None).unwrap_err(),
            "dataBits must be 5, 6, 7 or 8"
        );
        assert_eq!(
            set_uart_command(9600, None, Some("mark"), None, None).unwrap_err(),
            "parity must be n, e or o"
        );
        assert_eq!(
            set_uart_command(9600, None, None, Some(3), None).unwrap_err(),
            "stopBits must be 1 or 2"
        );
        assert_eq!(
            set_uart_command(9600, None, None, None, Some("x")).unwrap_err(),
            "flow must be none or rtscts"
        );
        assert_eq!(
            set_uart_command(9600, Some(7), Some("E"), Some(2), Some("RTSCTS")).unwrap(),
            "@u=9600,7,e,2,rtscts"
        );
    }

    #[test]
    fn wifi_and_webdav_builders_reject_dangerous_values() {
        let source = js();
        assert!(source.contains("ssid must not contain a comma"));
        assert_eq!(wifi_command("off", None, None).unwrap(), "@w off");
        assert_eq!(
            wifi_command("connect", None, None).unwrap_err(),
            "ssid is required"
        );
        assert_eq!(
            wifi_command("connect", Some("net,work"), None).unwrap_err(),
            "ssid must not contain a comma"
        );
        assert_eq!(
            wifi_command("connect", Some(&"s".repeat(33)), None).unwrap_err(),
            "ssid must be at most 32 characters"
        );
        assert_eq!(
            wifi_command("connect", Some("net"), Some(&"p".repeat(65))).unwrap_err(),
            "password must be at most 64 characters"
        );
        assert_eq!(
            wifi_command("connect", Some("net"), Some("secret")).unwrap(),
            "@w=net,secret"
        );
        assert_eq!(
            wifi_command("dance", Some("net"), None).unwrap_err(),
            "action must be connect or off"
        );

        assert_eq!(webdav_command("off", None).unwrap(), "@d off");
        assert_eq!(
            webdav_command("on", None).unwrap_err(),
            "url is required when enabling WebDAV upload"
        );
        assert_eq!(
            webdav_command("on", Some("ftp://host/d")).unwrap_err(),
            "url must start with http:// or https://"
        );
        assert_eq!(
            webdav_command("on", Some("http://host/a b")).unwrap_err(),
            "url must not contain whitespace or control characters"
        );
        assert_eq!(
            webdav_command("on", Some("https://host/dav/")).unwrap(),
            "@d=https://host/dav/"
        );
        assert_eq!(
            webdav_command("on", Some(&format!("https://h/{}", "x".repeat(257)))).unwrap_err(),
            "url must be at most 256 characters"
        );
    }

    #[test]
    fn credentials_are_redacted_everywhere() {
        assert_eq!(redact_command("@w=MyNet,s3cret"), "@w=MyNet,<redacted>");
        assert_eq!(redact_command("@w off"), "@w off");
        assert_eq!(
            redact_command("@d=https://user:pass@host/dav/"),
            "@d=https://<redacted>@host/dav/"
        );
        assert_eq!(
            redact_command("@d=https://host/dav/"),
            "@d=https://host/dav/"
        );
        assert_eq!(
            redact_command("@u=115200,8,n,1,none"),
            "@u=115200,8,n,1,none"
        );

        assert_eq!(
            accessory_change_summary("@w=MyNet,s3cret", false),
            "Join WiFi \"MyNet\" (password hidden)"
        );
        assert_eq!(
            accessory_change_summary("@w=MyNet,s3cret", true),
            "连接 WiFi「MyNet」（密码不显示）"
        );
        assert_eq!(
            accessory_change_summary("@u=9600,8,n,1,none", false),
            "Set the bridge UART to 9600,8,n,1,none"
        );
        assert_eq!(
            accessory_change_summary("@d off", false),
            "Disable WebDAV log upload"
        );
    }

    #[test]
    fn reply_parsers_shape_the_tool_result() {
        assert_eq!(
            reply_status("OK uart=115200,8,N,1,none"),
            (true, String::new())
        );
        assert_eq!(
            reply_status("ERR unsupported"),
            (false, "ERR unsupported".to_string())
        );
        assert_eq!(reply_status("junk\nOK\n"), (true, String::new()));

        let uart = parse_uart_settings("OK uart=115200,8,N,1,none").expect("uart");
        assert_eq!(uart.baud, 115_200);
        assert_eq!(uart.parity, "n");
        assert_eq!(uart.flow, "none");

        let (state, ssid, ip) =
            parse_wifi_status("OK wifi=connected,ssid=MyNet,ip=192.168.1.5").expect("wifi");
        assert_eq!(state, "connected");
        assert_eq!(ssid, "MyNet");
        assert_eq!(ip, "192.168.1.5");
        let (state, _, _) = parse_wifi_status("OK wifi off").expect("wifi off");
        assert_eq!(state, "off");

        let (state, url) =
            parse_webdav_status("OK webdav=on,url=http://host/dav/").expect("webdav");
        assert_eq!(state, "on");
        assert_eq!(url, "http://host/dav/");
        let (state, _) = parse_webdav_status("OK webdav off").expect("webdav off");
        assert_eq!(state, "off");
    }

    #[test]
    fn info_groups_are_shaped_not_interpreted() {
        let lines: Vec<String> = [
            "@info uart baud=115200 dataBits=8",
            "@info wifi state=connected ssid=MyNet",
            "@info done",
            "junk",
        ]
        .iter()
        .map(|line| line.to_string())
        .collect();
        let groups = parse_info_groups(&lines);
        assert_eq!(groups["uart"]["baud"], "115200");
        assert_eq!(groups["wifi"]["ssid"], "MyNet");
        assert!(is_diagnostics_done("@info done"));
        assert!(!is_diagnostics_done("@info wifi state=connected"));
    }

    #[test]
    fn every_mutation_needs_approval_and_capability_errors_are_exact() {
        assert!(require_approval("uart"));
        assert!(require_approval("wifi"));
        assert!(require_approval("webdav"));
        assert!(require_approval("scan"));
        assert_eq!(
            capability_missing_message("wifi"),
            "This accessory does not report wifi capability. The change was not requested."
        );
        // `web/accessory_control.js` carries the UART/wifi/webdav builders but
        // not these two exports; the spec quotes them verbatim (§4.6).
        let spec = std::fs::read_to_string(SPEC).expect("read specs/AGENT_SPEC.md");
        assert!(spec.contains("export function capabilityMissingMessage(capability) {"));
        assert!(spec.contains("This accessory does not report ${capability} capability."));
        assert!(spec.contains("export function requireApproval(action) { return true; }"));
    }
}
