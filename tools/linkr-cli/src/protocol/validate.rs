//! Argument validation, byte-translation helpers and device matching.
//! Behavior must match specs/PYTHON_CLI_SPEC.md section 9 exactly.

use std::path::Path;

pub const UART_BAUD_MIN: u64 = 300;
pub const UART_BAUD_MAX: u64 = 3_000_000;
pub const WIFI_PASSWORD_ENV: &str = "LINKR_WIFI_PASSWORD";

const UART_DATA_BITS: [&str; 4] = ["5", "6", "7", "8"];
const UART_STOP_BITS: [&str; 2] = ["1", "2"];
const UART_PARITY: [(&str, &str); 6] = [
    ("n", "n"),
    ("none", "n"),
    ("o", "o"),
    ("odd", "o"),
    ("e", "e"),
    ("even", "e"),
];
const UART_FLOW_CONTROL: [(&str, &str); 5] = [
    ("n", "n"),
    ("none", "n"),
    ("off", "n"),
    ("rtscts", "rtscts"),
    ("hw", "rtscts"),
];

/// Pick the quote CPython's `repr()` uses: `'` unless the text contains a
/// single quote and no double quote, in which case `"` (then the inner quote
/// is backslash-escaped — the `b'...'`/`\'` shorthand of PYTHON_CLI_SPEC
/// section 1.5; both quote kinds present also falls back to `'`).
fn repr_quote(has_single: bool, has_double: bool) -> char {
    if has_single && !has_double {
        '"'
    } else {
        '\''
    }
}

/// Python `str.__repr__`, used for `f"invalid UART baud rate: {baud!r}"`.
fn python_repr_str(value: &str) -> String {
    let quote = repr_quote(value.contains('\''), value.contains('"'));
    let mut out = String::with_capacity(value.len() + 2);
    out.push(quote);
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' if quote == '\'' => out.push_str("\\'"),
            '"' if quote == '"' => out.push_str("\\\""),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Parse an integer the way Python's `int(text, base)` does: optional sign
/// (base 10 only; CPython rejects a sign after the `0x` prefix), digits with
/// single underscores between them and nothing else. Returns `None` when the
/// text is not an integer, `Some(None)` when it is one but too large to fit
/// `u128` (Python would happily parse it and fail the range check).
fn parse_python_int(text: &str, base: u32) -> Option<Option<u128>> {
    let mut digits = text;
    if base == 10 {
        digits = digits
            .strip_prefix('-')
            .or_else(|| digits.strip_prefix('+'))
            .unwrap_or(digits);
    }
    if digits.is_empty() {
        return None;
    }
    let mut expect_digit = true;
    for ch in digits.chars() {
        if expect_digit {
            if !ch.is_digit(base) {
                return None;
            }
            expect_digit = false;
        } else if ch.is_digit(base) {
            // keep going
        } else if ch == '_' {
            expect_digit = true;
        } else {
            return None;
        }
    }
    if expect_digit {
        // trailing underscore
        return None;
    }
    let cleaned: String = digits.chars().filter(|c| *c != '_').collect();
    Some(u128::from_str_radix(&cleaned, base).ok())
}

fn baud_range_error() -> String {
    format!("UART baud rate must be between {UART_BAUD_MIN} and {UART_BAUD_MAX}")
}

fn canonical(map: &[(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    map.iter()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| *value)
}

/// Canonicalize `baud,data,parity,stop,flow`; `Err` carries the exact message
/// the Python CLI prints (usage exit 2).
pub fn normalize_uart_spec(spec: &str) -> Result<String, String> {
    let fields: Vec<&str> = spec.split(',').map(str::trim).collect();
    if fields.len() != 5 {
        return Err(
            "UART spec must be baud,data,parity,stop,flow, like 115200,8,n,1,n".to_string(),
        );
    }
    let (baud, data_bits, parity, stop_bits, flow) =
        (fields[0], fields[1], fields[2], fields[3], fields[4]);

    let baud_rate = match parse_python_int(baud, 10) {
        None => return Err(format!("invalid UART baud rate: {}", python_repr_str(baud))),
        // Python parses negatives (and oversized numbers) fine and then
        // reports the range error, so mirror that instead of blaming the
        // literal.
        Some(parsed) => match parsed {
            None => return Err(baud_range_error()),
            Some(_) if baud.starts_with('-') => return Err(baud_range_error()),
            Some(rate) if !(UART_BAUD_MIN as u128..=UART_BAUD_MAX as u128).contains(&rate) => {
                return Err(baud_range_error());
            }
            Some(rate) => rate as u64,
        },
    };
    if !UART_DATA_BITS.contains(&data_bits) {
        return Err("UART data bits must be one of 5, 6, 7, 8".to_string());
    }
    if !UART_STOP_BITS.contains(&stop_bits) {
        return Err("UART stop bits must be 1 or 2".to_string());
    }
    let Some(parity_canonical) = canonical(&UART_PARITY, &parity.to_ascii_lowercase()) else {
        return Err("UART parity must be none, odd or even (n/o/e)".to_string());
    };
    let Some(flow_canonical) = canonical(&UART_FLOW_CONTROL, &flow.to_ascii_lowercase()) else {
        return Err("UART flow control must be none or rtscts".to_string());
    };

    Ok(format!(
        "{baud_rate},{data_bits},{parity_canonical},{stop_bits},{flow_canonical}"
    ))
}

/// Parse `^]`, `0x1d` or a single raw byte into its byte value.
pub fn parse_escape(value: &str) -> Result<u8, String> {
    const NEED_ONE_BYTE: &str = "escape must be one byte, like ^] or 0x1d";
    if value == "^" {
        // A lone caret is the start of the ^X form, never a literal byte.
        return Err(NEED_ONE_BYTE.to_string());
    }
    let chars: Vec<char> = value.chars().collect();
    if chars.len() == 2 && chars[0] == '^' {
        // Mask, do not map: ^? becomes 0x1f and ^2 becomes 0x12.
        let upper = chars[1].to_ascii_uppercase() as u8;
        return Ok(upper & 0x1F);
    }
    if value.starts_with("0x") {
        // Lowercase prefix only; "0X1d" falls through to the byte form.
        // CPython allows exactly one underscore right after the prefix.
        let rest = value.strip_prefix("0x").unwrap_or(value);
        let rest = rest.strip_prefix('_').unwrap_or(rest);
        let Some(parsed) = parse_python_int(rest, 16) else {
            return Err(NEED_ONE_BYTE.to_string());
        };
        let Some(parsed) = parsed else {
            return Err("hex escape must be between 0x00 and 0xff".to_string());
        };
        if parsed > 0xFF {
            return Err("hex escape must be between 0x00 and 0xff".to_string());
        }
        return Ok(parsed as u8);
    }
    let raw = value.as_bytes();
    if raw.len() != 1 {
        return Err(NEED_ONE_BYTE.to_string());
    }
    Ok(raw[0])
}

/// Apply the `--enter` translation (raw/cr/lf/crlf).
pub fn translate_enter(data: &[u8], mode: &str) -> Vec<u8> {
    if mode == "raw" {
        return data.to_vec();
    }
    let normalized = replace_all(&replace_all(data, b"\r\n", b"\n"), b"\r", b"\n");
    let replacement: &[u8] = match mode {
        "cr" => b"\r",
        "lf" => b"\n",
        "crlf" => b"\r\n",
        // Python raises KeyError here; every caller passes a validated mode,
        // so behave like `raw` instead of panicking inside the library.
        _ => return data.to_vec(),
    };
    replace_all(&normalized, b"\n", replacement)
}

/// Non-overlapping left-to-right replacement, like `bytes.replace`.
fn replace_all(data: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    if needle.is_empty() {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len());
    let mut offset = 0;
    while offset < data.len() {
        if data[offset..].starts_with(needle) {
            out.extend_from_slice(replacement);
            offset += needle.len();
        } else {
            out.push(data[offset]);
            offset += 1;
        }
    }
    out
}

/// Human name of the escape byte, as printed in the `terminal open` hint.
pub fn describe_escape(escape: u8) -> String {
    match escape {
        0x1B => "Esc".to_string(),
        1..=26 => format!("Ctrl-{}", (b'A' + escape - 1) as char),
        0x1C..=0x1F => {
            const TAIL: [char; 4] = ['\\', ']', '^', '_'];
            format!("Ctrl-{}", TAIL[(escape - 0x1C) as usize])
        }
        0x20..=0x7E => python_repr_str(&(escape as char).to_string()),
        other => format!("0x{other:02x}"),
    }
}

/// Python `bytes.__repr__`, used by `--debug-io` traces and loopback output.
pub fn python_repr_bytes(data: &[u8]) -> String {
    let quote = repr_quote(data.contains(&b'\''), data.contains(&b'"'));
    let mut out = String::with_capacity(data.len() + 3);
    out.push('b');
    out.push(quote);
    for &byte in data {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'\'' if quote == '\'' => out.push_str("\\'"),
            b'"' if quote == '"' => out.push_str("\\\""),
            b'\t' => out.push_str("\\t"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            0x20..=0x7E => out.push(byte as char),
            other => out.push_str(&format!("\\x{other:02x}")),
        }
    }
    out.push(quote);
    out
}

/// Strip a trailing `*` and surrounding whitespace from a `--name` pattern.
pub fn normalize_name_prefix(name: &str) -> String {
    let name = name.trim();
    if let Some(stripped) = name.strip_suffix('*') {
        // Remove exactly one '*', then the trailing spaces.
        return stripped.trim_end().to_string();
    }
    name.to_string()
}

/// Pick the address for `name` among scanned devices (exact match first,
/// then prefix, warning on multiple matches) — see PYTHON_CLI_SPEC §8.4.
pub fn match_device(devices: &[(Option<String>, String)], name: &str) -> Option<String> {
    // Exact pass: compares the raw --name (trailing '*' included); a None
    // name never matches.
    for (device_name, address) in devices {
        if device_name.as_deref() == Some(name) {
            return Some(address.clone());
        }
    }

    let prefix = normalize_name_prefix(name);
    let prefixed: Vec<&(Option<String>, String)> = devices
        .iter()
        .filter(|(device_name, _)| match device_name.as_deref() {
            Some(device_name) => !device_name.is_empty() && device_name.starts_with(&prefix),
            None => false,
        })
        .collect();
    if prefixed.is_empty() {
        return None;
    }
    if prefixed.len() > 1 {
        // Python warns from inside match_device; route it through `cli::warn`
        // so the TUI can capture it instead of dropping it on the screen.
        let listed: Vec<(String, String)> = prefixed
            .iter()
            .take(4)
            .map(|(device_name, address)| {
                (device_name.clone().unwrap_or_default(), address.clone())
            })
            .collect();
        crate::cli::warn(multi_match_warning(&prefix, &listed));
    }
    Some(prefixed[0].1.clone())
}

/// The exact `multiple devices match ...` warning body from the message
/// catalog (§12), built from the first four matches.
fn multi_match_warning(prefix: &str, matches: &[(String, String)]) -> String {
    let listed: Vec<String> = matches
        .iter()
        .map(|(device_name, address)| format!("{device_name} ({address})"))
        .collect();
    format!(
        "multiple devices match {prefix}*; using {} ({}); matches: {}",
        matches[0].0,
        matches[0].1,
        listed.join(", ")
    )
}

/// Resolve `--wifi SSID[,PASSWORD]` into `(ssid, password)` without forcing
/// the password into argv: inline → key file → `LINKR_WIFI_PASSWORD` →
/// prompt callback (PYTHON_CLI_SPEC §9.2). `Err` carries the exact message;
/// the caller prints it and exits 2.
pub fn resolve_wifi_credentials(
    spec: &str,
    key_file: Option<&Path>,
    env: Option<&str>,
    prompt: Option<&mut dyn FnMut(&str) -> String>,
) -> Result<(String, String), String> {
    let (ssid_part, inline) = match spec.find(',') {
        Some(position) => (&spec[..position], Some(&spec[position + 1..])),
        None => (spec, None),
    };
    let ssid = ssid_part.trim();
    if ssid.is_empty() {
        return Err("WiFi SSID must not be empty".to_string());
    }
    // A comma present means the inline password is accepted even when empty.
    if let Some(inline) = inline {
        return Ok((ssid.to_string(), inline.to_string()));
    }

    if let Some(key_file) = key_file {
        let text = std::fs::read_to_string(key_file)
            .map_err(|error| format!("cannot read --wifi-key-file: {error}"))?;
        let lines = python_splitlines(&text);
        let Some(first) = lines.first() else {
            return Err(format!("--wifi-key-file {} is empty", key_file.display()));
        };
        return Ok((ssid.to_string(), first.clone()));
    }

    if let Some(source) = env {
        if !source.is_empty() {
            return Ok((ssid.to_string(), source.to_string()));
        }
    }

    if let Some(prompt) = prompt {
        let password = prompt(&format!("WiFi password for {ssid}: "));
        if password.is_empty() {
            return Err("WiFi password must not be empty".to_string());
        }
        return Ok((ssid.to_string(), password));
    }

    Err(format!(
        "no WiFi password available: pass --wifi ssid,pass, or set \
         --wifi-key-file, or export {WIFI_PASSWORD_ENV}"
    ))
}

/// Python `str.splitlines()`: split on \n, \r, \r\n and the remaining line
/// separators; `""` has no lines at all (so `""` is the empty file error).
fn python_splitlines(text: &str) -> Vec<String> {
    const SEPARATORS: [char; 10] = [
        '\n', '\r', '\u{0b}', '\u{0c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}',
        '\u{2029}',
    ];
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if SEPARATORS.contains(&ch) {
            if ch == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            lines.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ---- ValidatorTests -------------------------------------------------

    #[test]
    fn normalize_uart_spec_canonicalizes() {
        assert_eq!(
            normalize_uart_spec("9600,7,even,2,off").unwrap(),
            "9600,7,e,2,n"
        );
        assert_eq!(
            normalize_uart_spec(" 115200 , 8 , EVEN , 2 , HW ").unwrap(),
            "115200,8,e,2,rtscts"
        );
        assert_eq!(
            normalize_uart_spec("+3_00,8,none,1,RTSCTS").unwrap(),
            "300,8,n,1,rtscts"
        );
    }

    #[test]
    fn normalize_uart_spec_rejects_bad_fields() {
        let bad = [
            (
                "115200,8,n,1",
                "UART spec must be baud,data,parity,stop,flow, like 115200,8,n,1,n",
            ),
            (
                "299,8,n,1,n",
                "UART baud rate must be between 300 and 3000000",
            ),
            (
                "3000001,8,n,1,n",
                "UART baud rate must be between 300 and 3000000",
            ),
            ("115200,9,n,1,n", "UART data bits must be one of 5, 6, 7, 8"),
            ("115200,8,n,3,n", "UART stop bits must be 1 or 2"),
            (
                "115200,8,mark,1,n",
                "UART parity must be none, odd or even (n/o/e)",
            ),
            (
                "115200,8,n,1,xonxoff",
                "UART flow control must be none or rtscts",
            ),
            ("abc,8,n,1,n", "invalid UART baud rate: 'abc'"),
            (
                "-300,8,n,1,n",
                "UART baud rate must be between 300 and 3000000",
            ),
            (
                "99999999999999999999999,8,n,1,n",
                "UART baud rate must be between 300 and 3000000",
            ),
            ("115200x,8,n,1,n", "invalid UART baud rate: '115200x'"),
            ("115200_,8,n,1,n", "invalid UART baud rate: '115200_'"),
            (
                "300,8,n,1",
                "UART spec must be baud,data,parity,stop,flow, like 115200,8,n,1,n",
            ),
        ];
        for (spec, expected) in bad {
            assert_eq!(
                normalize_uart_spec(spec).unwrap_err(),
                expected,
                "spec {spec:?}"
            );
        }
    }

    #[test]
    fn parse_escape_forms() {
        assert_eq!(parse_escape("^]").unwrap(), 0x1d);
        assert_eq!(parse_escape("0x1d").unwrap(), 0x1d);
        assert_eq!(parse_escape("q").unwrap(), b'q');
        assert_eq!(
            parse_escape("^").unwrap_err(),
            "escape must be one byte, like ^] or 0x1d"
        );
        assert_eq!(
            parse_escape("0x1ff").unwrap_err(),
            "hex escape must be between 0x00 and 0xff"
        );
        assert_eq!(
            parse_escape("ab").unwrap_err(),
            "escape must be one byte, like ^] or 0x1d"
        );
        assert_eq!(
            parse_escape("0xzz").unwrap_err(),
            "escape must be one byte, like ^] or 0x1d"
        );
        // Masking, not mapping: ^? -> 0x1f, ^a -> 0x01, ^2 -> 0x12.
        assert_eq!(parse_escape("^?").unwrap(), 0x1f);
        assert_eq!(parse_escape("^a").unwrap(), 0x01);
        assert_eq!(parse_escape("^A").unwrap(), 0x01);
        assert_eq!(parse_escape("^2").unwrap(), 0x12);
        assert_eq!(parse_escape("^^").unwrap(), 0x1e);
        assert_eq!(parse_escape("^_").unwrap(), 0x1f);
        // The 0X prefix is not special: four bytes, no single byte.
        assert!(parse_escape("0X1d").is_err());
        assert_eq!(parse_escape("0x00").unwrap(), 0x00);
        assert_eq!(parse_escape("0xff").unwrap(), 0xff);
        // One underscore right after the 0x prefix is legal in Python int().
        assert_eq!(parse_escape("0x_1").unwrap(), 0x01);
        assert!(parse_escape("0x__1").is_err());
        assert!(parse_escape("0x_").is_err());
    }

    #[test]
    fn describe_escape_names_the_configured_byte() {
        assert_eq!(describe_escape(0x1d), "Ctrl-]");
        assert_eq!(describe_escape(0x03), "Ctrl-C");
        assert_eq!(describe_escape(b'q'), "'q'");
        assert_eq!(describe_escape(0x1b), "Esc");
        assert_eq!(describe_escape(0x1c), "Ctrl-\\");
        assert_eq!(describe_escape(0x1f), "Ctrl-_");
        assert_eq!(describe_escape(0x01), "Ctrl-A");
        assert_eq!(describe_escape(0x00), "0x00");
        assert_eq!(describe_escape(0x7f), "0x7f");
        assert_eq!(describe_escape(0x20), "' '");
        // repr() picks the other quote when the text holds only a `'`.
        assert_eq!(describe_escape(b'\''), "\"'\"");
    }

    #[test]
    fn translate_enter_modes() {
        assert_eq!(translate_enter(b"a\r\nb\r", "raw"), b"a\r\nb\r");
        assert_eq!(translate_enter(b"a\r\nb\r", "lf"), b"a\nb\n");
        assert_eq!(translate_enter(b"a\n", "cr"), b"a\r");
        assert_eq!(translate_enter(b"a\n", "crlf"), b"a\r\n");
        // bytes.replace is non-overlapping and left to right.
        assert_eq!(translate_enter(b"\r\r\n", "lf"), b"\n\n");
    }

    // ---- Python repr formatting -----------------------------------------

    #[test]
    fn python_repr_bytes_formats_like_python() {
        assert_eq!(python_repr_bytes(b"hello\r\n"), "b'hello\\r\\n'");
        assert_eq!(python_repr_bytes(b"\x1b"), "b'\\x1b'");
        assert_eq!(python_repr_bytes(b""), "b''");
        assert_eq!(python_repr_bytes(b"a\\b"), "b'a\\\\b'");
        assert_eq!(python_repr_bytes(&[0x7f, 0x00]), "b'\\x7f\\x00'");
        assert_eq!(python_repr_bytes(b" printable "), "b' printable '");
        // CPython switches to double quotes when only `'` appears...
        assert_eq!(python_repr_bytes(b"a'b"), "b\"a'b\"");
        // ...and keeps single quotes (escaping `'`) when both appear.
        assert_eq!(python_repr_bytes(b"a'b\"c"), "b'a\\'b\"c'");
        assert_eq!(python_repr_bytes(b"\""), "b'\"'");
    }

    #[test]
    fn python_repr_str_formats_like_python() {
        assert_eq!(python_repr_str("abc"), "'abc'");
        assert_eq!(python_repr_str("it's"), "\"it's\"");
        assert_eq!(python_repr_str("a\\b"), "'a\\\\b'");
        assert_eq!(python_repr_str("\u{7}"), "'\\x07'");
    }

    #[test]
    fn traces_carry_the_python_repr() {
        // `--debug-io` lines: `TX b'...'`, `MGMT TX #1 b'...'`.
        assert_eq!(format!("TX {}", python_repr_bytes(b"hi\r")), "TX b'hi\\r'");
        assert_eq!(
            format!("MGMT TX #{} {}", 1, python_repr_bytes(b"@i?")),
            "MGMT TX #1 b'@i?'"
        );
    }

    // ---- DeviceMatchTests -----------------------------------------------

    fn device(name: Option<&str>, address: &str) -> (Option<String>, String) {
        (name.map(str::to_string), address.to_string())
    }

    #[test]
    fn normalize_name_prefix_strips_star_and_whitespace() {
        assert_eq!(normalize_name_prefix("Linkr BLE UART*"), "Linkr BLE UART");
        assert_eq!(normalize_name_prefix(" Linkr * "), "Linkr");
        assert_eq!(normalize_name_prefix("plain"), "plain");
        assert_eq!(normalize_name_prefix("a*b"), "a*b");
        assert_eq!(normalize_name_prefix("*"), "");
    }

    #[test]
    fn exact_name_wins_over_prefix() {
        let devices = vec![
            device(Some("Linkr BLE UART 2"), "AA:2"),
            device(Some("Linkr BLE UART"), "AA:1"),
        ];
        assert_eq!(
            match_device(&devices, "Linkr BLE UART"),
            Some("AA:1".to_string())
        );
    }

    #[test]
    fn prefix_matches_the_default_name() {
        let devices = vec![
            device(Some("Other"), "BB:1"),
            device(Some("Linkr BLE UART 7F2C"), "AA:7"),
        ];
        assert_eq!(
            match_device(&devices, "Linkr BLE UART*"),
            Some("AA:7".to_string())
        );
    }

    #[test]
    fn no_match_returns_none() {
        let devices = vec![device(Some("Other"), "BB:1")];
        assert_eq!(match_device(&devices, "x*"), None);
        // A None name never matches, not even an exact comparison.
        assert_eq!(match_device(&[device(None, "AA:1")], "x"), None);
    }

    #[test]
    fn multiple_matches_warn_with_the_catalog_text() {
        let prefix = "Linkr BLE UART";
        let matches = [
            ("Linkr BLE UART A".to_string(), "AA:1".to_string()),
            ("Linkr BLE UART B".to_string(), "AA:2".to_string()),
            ("Linkr BLE UART C".to_string(), "AA:3".to_string()),
            ("Linkr BLE UART D".to_string(), "AA:4".to_string()),
            ("Linkr BLE UART E".to_string(), "AA:5".to_string()),
        ];
        assert_eq!(
            multi_match_warning(prefix, &matches[..4]),
            "multiple devices match Linkr BLE UART*; using Linkr BLE UART A (AA:1); \
             matches: Linkr BLE UART A (AA:1), Linkr BLE UART B (AA:2), \
             Linkr BLE UART C (AA:3), Linkr BLE UART D (AA:4)"
        );
        let devices = vec![
            device(Some("Linkr BLE UART A"), "AA:1"),
            device(Some("Linkr BLE UART B"), "AA:2"),
        ];
        // The first device in list order wins (stderr shows the warning).
        assert_eq!(
            match_device(&devices, "Linkr BLE UART*"),
            Some("AA:1".to_string())
        );
    }

    // ---- WifiCredentialTests --------------------------------------------

    fn temp_key_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "linkr-cli-wifi-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn inline_credentials_still_work() {
        assert_eq!(
            resolve_wifi_credentials("ssid,secret", None, None, None).unwrap(),
            ("ssid".to_string(), "secret".to_string())
        );
        // A comma with an empty inline password is accepted.
        assert_eq!(
            resolve_wifi_credentials("ssid,", None, None, None).unwrap(),
            ("ssid".to_string(), String::new())
        );
        // Only the SSID is stripped; the inline password keeps its spaces.
        assert_eq!(
            resolve_wifi_credentials(" ssid , p w ", None, None, None).unwrap(),
            ("ssid".to_string(), " p w ".to_string())
        );
    }

    #[test]
    fn password_is_read_from_a_key_file() {
        let dir = temp_key_dir("file");
        let key = dir.join("wifi.key");
        std::fs::write(&key, "filesecret\nignored\n").unwrap();
        assert_eq!(
            resolve_wifi_credentials("ssid", Some(&key), None, None).unwrap(),
            ("ssid".to_string(), "filesecret".to_string())
        );
        std::fs::remove_file(&key).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn password_can_come_from_the_environment() {
        assert_eq!(
            resolve_wifi_credentials("ssid", None, Some("env"), None).unwrap(),
            ("ssid".to_string(), "env".to_string())
        );
        // An empty environment value counts as unset (Python `if source:`).
        let mut called = false;
        let mut prompt = |_: &str| {
            called = true;
            "typed".to_string()
        };
        assert_eq!(
            resolve_wifi_credentials("ssid", None, Some(""), Some(&mut prompt)).unwrap(),
            ("ssid".to_string(), "typed".to_string())
        );
        assert!(called);
    }

    #[test]
    fn password_prompt_is_the_last_resort() {
        let mut seen = String::new();
        let mut prompt = |message: &str| {
            seen = message.to_string();
            "typed".to_string()
        };
        assert_eq!(
            resolve_wifi_credentials("ssid", None, None, Some(&mut prompt)).unwrap(),
            ("ssid".to_string(), "typed".to_string())
        );
        assert_eq!(seen, "WiFi password for ssid: ");
    }

    #[test]
    fn missing_password_is_an_error_not_an_empty_password() {
        let error = resolve_wifi_credentials("ssid", None, None, None).unwrap_err();
        assert_eq!(
            error,
            "no WiFi password available: pass --wifi ssid,pass, or set \
             --wifi-key-file, or export LINKR_WIFI_PASSWORD"
        );
    }

    #[test]
    fn empty_ssid_and_empty_key_file_are_rejected() {
        assert_eq!(
            resolve_wifi_credentials(",secret", None, None, None).unwrap_err(),
            "WiFi SSID must not be empty"
        );
        assert_eq!(
            resolve_wifi_credentials("   ,secret", None, None, None).unwrap_err(),
            "WiFi SSID must not be empty"
        );

        let dir = temp_key_dir("empty");
        let key = dir.join("wifi.key");
        std::fs::write(&key, "").unwrap();
        assert_eq!(
            resolve_wifi_credentials("ssid", Some(&key), None, None).unwrap_err(),
            format!("--wifi-key-file {} is empty", key.display())
        );
        std::fs::remove_file(&key).unwrap();

        let missing = dir.join("nope.key");
        let error = resolve_wifi_credentials("ssid", Some(&missing), None, None).unwrap_err();
        assert!(
            error.starts_with("cannot read --wifi-key-file: "),
            "unexpected error {error:?}"
        );

        // An empty prompted password is an error too.
        let mut prompt = |_: &str| String::new();
        assert_eq!(
            resolve_wifi_credentials("ssid", None, None, Some(&mut prompt)).unwrap_err(),
            "WiFi password must not be empty"
        );
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn key_file_prompt_and_env_lose_to_inline_password() {
        // Order: inline wins over key file, env and prompt.
        let dir = temp_key_dir("order");
        let key = dir.join("wifi.key");
        std::fs::write(&key, "filesecret\n").unwrap();
        let mut called = false;
        let mut prompt = |_: &str| {
            called = true;
            "typed".to_string()
        };
        assert_eq!(
            resolve_wifi_credentials("ssid,inline", Some(&key), Some("env"), Some(&mut prompt))
                .unwrap(),
            ("ssid".to_string(), "inline".to_string())
        );
        // Key file wins over env and prompt.
        assert_eq!(
            resolve_wifi_credentials("ssid", Some(&key), Some("env"), Some(&mut prompt)).unwrap(),
            ("ssid".to_string(), "filesecret".to_string())
        );
        // Env wins over prompt.
        assert_eq!(
            resolve_wifi_credentials("ssid", None, Some("env"), Some(&mut prompt)).unwrap(),
            ("ssid".to_string(), "env".to_string())
        );
        assert!(!called);
        std::fs::remove_file(&key).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
