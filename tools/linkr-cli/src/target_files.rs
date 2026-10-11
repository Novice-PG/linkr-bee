//! Target file read/upload over the shell paging protocol.
//! Port of web/target_files.js (markers, dd|base64 paging, part-file upload).
//!
//! `cat` is the wrong way to read a target file through this bridge: a page
//! with `dd bs=1 skip=N count=M | base64` keeps the exchange inside one console
//! round trip, and an upload is staged into a sibling part file that is only
//! moved over the destination after the size (and, when asked for, the digest)
//! were re-read on the target.
//!
//! MARKERS. Every marker is one whole line and is matched only as a complete
//! line, so the shell's echo of a command can never be mistaken for its output.
//! `printf` prints every marker, never `echo -e`.

use anyhow::{bail, Result};
use regex::Regex;
use std::sync::OnceLock;

/// Marker prefixes; must stay byte-identical to the web client.
pub const FILE_BEGIN_PREFIX: &str = "LINKR_FILE:begin";
pub const FILE_END_PREFIX: &str = "LINKR_FILE:end";
pub const FILE_ERROR_PREFIX: &str = "LINKR_FILE:error";
pub const FILE_ACK_PREFIX: &str = "LINKR_FILE:ack";

/// One page must fit comfortably in a single console round trip.
pub const MAX_READ_BYTES: i64 = 1024;
pub const DEFAULT_CHUNK_BYTES: i64 = 720;
pub const MAX_CHUNK_BYTES: i64 = 2048;
pub const PART_SUFFIX: &str = ".linkr-part";
/// Command-size budget for one upload chunk (UART ring headroom).
pub const MAX_UPLOAD_COMMAND_BYTES: usize = 4096;
/// A plan is built in memory before anything is sent, so an absurd size must
/// fail while planning rather than freeze the panel.
pub const MAX_UPLOAD_CHUNKS: usize = 20000;

/// Probe for the tools the commands below need; the caller reports what is
/// missing instead of sending a command that dies halfway.
pub const TARGET_FILE_PROBE: &str = r#"for t in dd base64 wc tr sha256sum shasum; do command -v "$t" >/dev/null 2>&1 && printf 'LINKR_TOOL:%s\n' "$t"; done; :"#;

/// JavaScript `Number.isSafeInteger` bound.
const MAX_SAFE: u64 = 9_007_199_254_740_991;

fn re_read_begin() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^LINKR_FILE:begin total=(\d+) from=(\d+)$").unwrap())
}

fn re_read_end() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^LINKR_FILE:end bytes=(\d+)$").unwrap())
}

fn re_read_error() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^LINKR_FILE:error ([a-z-]+)$").unwrap())
}

fn re_upload_chunk() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"^LINKR_UPLOAD:chunk index=(\d+) offset=(\d+) bytes=(\d+) total=(\d+)$")
            .unwrap()
    })
}

fn re_upload_bytes() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^LINKR_UPLOAD:bytes=(\d+)$").unwrap())
}

fn re_upload_sha() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^LINKR_UPLOAD:sha256=([0-9a-f]{64})$").unwrap())
}

fn re_upload_size_mismatch() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^LINKR_UPLOAD:error size-mismatch(?: actual=(\d+))?$").unwrap())
}

fn re_upload_hash_mismatch() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^LINKR_UPLOAD:error hash-mismatch(?: actual=(\S*))?$").unwrap())
}

/// `^[A-Za-z0-9+/]*={0,2}$` with a multiple-of-four length: exactly what
/// `base64 -d` accepts.
fn is_padded_base64(text: &str) -> bool {
    text.len().is_multiple_of(4)
        && text.chars().rev().take_while(|&c| c == '=').count() <= 2
        && text
            .chars()
            .rev()
            .skip_while(|&c| c == '=')
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/')
}

/// Serial consoles send CRLF, and a bare CR is its own line break on progress
/// output. Shared with `crate::target_verify`, which splits lines the same way.
pub fn marker_lines(text: &str) -> Vec<String> {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(|line| line.trim_end().to_string())
        .collect()
}

/// Shell-quote as a single-quoted literal. A NUL cannot cross argv at all, so
/// it is refused rather than silently naming a different path.
pub fn quote_shell(text: &str) -> Result<String> {
    if text.contains('\0') {
        bail!("Shell quoting cannot represent a NUL byte.");
    }
    Ok(format!("'{}'", text.replace('\'', "'\\''")))
}

/// Human byte count: whole scaled values read without a decimal.
pub fn format_bytes(n: i64) -> Result<String> {
    if n < 0 {
        bail!("Byte count must be a non-negative number.");
    }
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 || value.fract() == 0.0 {
        Ok(format!("{} {}", value as i64, UNITS[unit]))
    } else {
        Ok(format!("{value:.1} {}", UNITS[unit]))
    }
}

const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_value(byte: u8) -> Option<u8> {
    B64_ALPHABET
        .iter()
        .position(|&b| b == byte)
        .map(|i| i as u8)
}

/// Standard base64 with padding, byte-identical to the JS encoder.
pub fn encode_base64(bytes: &[u8]) -> String {
    let mut text = String::with_capacity((bytes.len() / 3 + 1) * 4);
    let mut i = 0;
    while i < bytes.len() {
        let a = bytes[i];
        let b = *bytes.get(i + 1).unwrap_or(&0);
        let c = *bytes.get(i + 2).unwrap_or(&0);
        text.push(B64_ALPHABET[(a >> 2) as usize] as char);
        text.push(B64_ALPHABET[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        if i + 1 < bytes.len() {
            text.push(B64_ALPHABET[(((b & 15) << 2) | (c >> 6)) as usize] as char);
        } else {
            text.push('=');
        }
        if i + 2 < bytes.len() {
            text.push(B64_ALPHABET[(c & 63) as usize] as char);
        } else {
            text.push('=');
        }
        i += 3;
    }
    text
}

/// Decode what `base64 -d` would accept; `None` on anything malformed. Only
/// called with text that already passed the charset and length checks, but the
/// checks are repeated so the function is safe on its own.
pub fn decode_base64(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(4) || !text.is_ascii() {
        return None;
    }
    let padding = if text.ends_with("==") {
        2
    } else if text.ends_with('=') {
        1
    } else {
        0
    };
    let total = (text.len() / 4) * 3;
    let mut out = vec![0u8; total.checked_sub(padding)?];
    let bytes = text.as_bytes();
    let mut at = 0usize;
    let mut i = 0usize;
    while i < text.len() {
        let a = b64_value(bytes[i])?;
        let b = b64_value(bytes[i + 1])?;
        let c = if bytes[i + 2] == b'=' {
            0
        } else {
            b64_value(bytes[i + 2])?
        };
        let d = if bytes[i + 3] == b'=' {
            0
        } else {
            b64_value(bytes[i + 3])?
        };
        out[at] = (a << 2) | (b >> 4);
        at += 1;
        if bytes[i + 2] != b'=' {
            if at >= out.len() {
                return None;
            }
            out[at] = ((b & 15) << 4) | (c >> 2);
            at += 1;
        }
        if bytes[i + 3] != b'=' {
            if at >= out.len() {
                return None;
            }
            out[at] = ((c & 3) << 6) | d;
            at += 1;
        }
        i += 4;
    }
    (at == out.len()).then_some(out)
}

fn read_path(value: &str) -> Result<&str> {
    if value.is_empty() {
        bail!("Target path must be a non-empty string.");
    }
    /* Control characters (CR/LF/NUL) would break the line-oriented markers and
     * could let a crafted path forge a marker line. A path without a leading
     * `/` is deliberately let through: the shell reports it as not-absolute so
     * the caller gets a diagnosis instead of an exception. */
    if value.chars().any(|c| c.is_control()) {
        bail!("Target path must not contain control characters.");
    }
    Ok(value)
}

fn read_offset(value: i64) -> Result<u64> {
    if value < 0 {
        bail!("Read offset must be a non-negative integer.");
    }
    Ok(value as u64)
}

fn read_count(value: i64) -> Result<u64> {
    if value <= 0 {
        bail!("Read size must be a positive integer.");
    }
    if value > MAX_READ_BYTES {
        bail!("Read size must not exceed {MAX_READ_BYTES} bytes.");
    }
    Ok(value as u64)
}

/// Read at most `bytes` bytes from `offset`.
///
/// Every refusal `exit 0`s on purpose: a missing file is a result the caller
/// reads from the marker, not a transport failure. Paging is O(offset) on the
/// target (`bs=1 skip=N` reads through the file), so callers should walk a
/// large file sequentially.
pub fn read_file_command(path: &str, offset: i64, bytes: i64) -> Result<String> {
    let target = quote_shell(read_path(path)?)?;
    let from = read_offset(offset)?;
    let count = read_count(bytes)?;
    let parts = [
        format!("p={target}"),
        // `case` rather than `[[ ]]`: dash, ash and busybox sh all support it.
        r#"case "$p" in /*) ;; *) printf '\nLINKR_FILE:error not-absolute\n'; exit 0 ;; esac"#
            .to_string(),
        r#"[ -e "$p" ] || { printf '\nLINKR_FILE:error missing\n'; exit 0; }"#.to_string(),
        r#"[ -d "$p" ] && { printf '\nLINKR_FILE:error directory\n'; exit 0; }"#.to_string(),
        r#"[ -r "$p" ] || { printf '\nLINKR_FILE:error denied\n'; exit 0; }"#.to_string(),
        // A fifo or character device has no end and makes `wc -c` block forever.
        r#"[ -f "$p" ] || { printf '\nLINKR_FILE:error not-regular\n'; exit 0; }"#.to_string(),
        // `tr -d` because some `wc -c` builds pad the number into a column.
        r#"t=$(wc -c < "$p" | tr -d ' \t') || { printf '\nLINKR_FILE:error denied\n'; exit 0; }"#
            .to_string(),
        format!("n=$((t - {from})); [ \"$n\" -gt 0 ] || n=0"),
        format!("[ \"$n\" -gt {count} ] && n={count}"),
        format!(r#"printf '\nLINKR_FILE:begin total=%s from={from}\n' "$t""#),
        format!("dd bs=1 skip={from} count=$n < \"$p\" 2>/dev/null | base64"),
        r#"printf '\nLINKR_FILE:end bytes=%s\n' "$n""#.to_string(),
    ];
    Ok(parts.join("; "))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadStatus {
    Ok,
    Incomplete,
    Missing,
    Directory,
    Denied,
}

impl ReadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ReadStatus::Ok => "ok",
            ReadStatus::Incomplete => "incomplete",
            ReadStatus::Missing => "missing",
            ReadStatus::Directory => "directory",
            ReadStatus::Denied => "denied",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRead {
    pub status: ReadStatus,
    pub total_bytes: Option<u64>,
    pub from: Option<u64>,
    pub bytes: Option<u64>,
    pub data: Option<Vec<u8>>,
    pub reason: String,
}

fn miss(reason: &str, total_bytes: Option<u64>, from: Option<u64>, bytes: Option<u64>) -> FileRead {
    FileRead {
        status: ReadStatus::Incomplete,
        total_bytes,
        from,
        bytes,
        data: None,
        reason: reason.to_string(),
    }
}

fn safe_count(text: &str) -> Option<u64> {
    text.parse::<u64>().ok().filter(|v| *v <= MAX_SAFE)
}

/// Read back `read_file_command` output.
///
/// The LAST begin marker wins: a journal tail can still hold an earlier read of
/// the same file, and a stale complete pair must never be reported as this
/// read's data. With no begin at all, only then is an error marker trusted.
pub fn parse_file_read(text: &str) -> FileRead {
    let lines = marker_lines(text);
    let begin = lines
        .iter()
        .rposition(|line| re_read_begin().is_match(line));
    let Some(begin) = begin else {
        for line in &lines {
            if let Some(caps) = re_read_error().captures(line) {
                let detail = caps[1].to_string();
                let (status, reason) = match detail.as_str() {
                    "missing" => (ReadStatus::Missing, "Target file does not exist."),
                    "directory" => (ReadStatus::Directory, "Target path is a directory."),
                    "not-absolute" => (ReadStatus::Denied, "Target path is not absolute."),
                    "denied" => (ReadStatus::Denied, "Target file is not readable."),
                    "not-regular" => (ReadStatus::Denied, "Target path is not a regular file."),
                    other => {
                        return miss(
                            &format!("Target reported LINKR_FILE:error {other}."),
                            None,
                            None,
                            None,
                        )
                    }
                };
                return FileRead {
                    status,
                    total_bytes: None,
                    from: None,
                    bytes: None,
                    data: None,
                    reason: reason.to_string(),
                };
            }
        }
        return miss(
            "No LINKR_FILE markers in the output: the command did not run or its output was truncated.",
            None,
            None,
            None,
        );
    };

    let caps = re_read_begin().captures(&lines[begin]).unwrap();
    let total_bytes = safe_count(&caps[1]);
    let from = safe_count(&caps[2]);

    let end = lines
        .iter()
        .enumerate()
        .skip(begin + 1)
        .find(|(_, line)| re_read_end().is_match(line))
        .map(|(index, _)| index);
    let Some(end) = end else {
        return miss(
            "The read stopped before its end marker: the payload is truncated.",
            total_bytes,
            from,
            None,
        );
    };
    let end_bytes = safe_count(&re_read_end().captures(&lines[end]).unwrap()[1]);

    // Anything past 2^53 is a corrupted marker, not a file size.
    if total_bytes.is_none() || from.is_none() || end_bytes.is_none() {
        return miss(
            "The markers do not carry safe byte counts: the output was corrupted.",
            None,
            None,
            None,
        );
    }
    let end_bytes_v = end_bytes.unwrap();

    // Wrapped base64 lines are slices of one stream, so join before stripping.
    let payload: String = lines[begin + 1..end]
        .iter()
        .flat_map(|line| line.chars())
        .filter(|c| !c.is_whitespace())
        .collect();
    if !is_padded_base64(&payload) {
        return miss(
            "The payload is not valid base64: the transfer was corrupted or truncated.",
            total_bytes,
            from,
            Some(end_bytes_v),
        );
    }
    let Some(data) = decode_base64(&payload) else {
        return miss(
            "The payload is not valid base64: the transfer was corrupted or truncated.",
            total_bytes,
            from,
            Some(end_bytes_v),
        );
    };
    if data.len() as u64 != end_bytes_v {
        return miss(
            &format!(
                "The payload holds {} bytes but the end marker declared {end_bytes_v}.",
                data.len()
            ),
            total_bytes,
            from,
            Some(end_bytes_v),
        );
    }
    let from_v = from.unwrap();
    let total_v = total_bytes.unwrap();
    // A page that starts at or past EOF reports zero bytes from an offset
    // beyond the size, which is a legitimate short page; anything else must fit.
    if end_bytes_v > 0 && from_v + end_bytes_v > total_v {
        return miss(
            &format!(
                "The read claims bytes {}..{} of a {total_v}-byte file.",
                from_v,
                from_v + end_bytes_v
            ),
            total_bytes,
            from,
            Some(end_bytes_v),
        );
    }
    FileRead {
        status: ReadStatus::Ok,
        total_bytes,
        from,
        bytes: Some(end_bytes_v),
        data: Some(data),
        reason: String::new(),
    }
}

fn upload_path(value: &str) -> Result<&str> {
    let path = read_path(value)?;
    if !path.starts_with('/') {
        bail!("Target upload path must be absolute.");
    }
    if path.ends_with('/') {
        bail!("Target upload path must name a file, not a directory.");
    }
    /* Whitespace and quotes are refused rather than quoted away because the
     * part file name is derived from the destination by appending a suffix: a
     * path that cannot be reprinted verbatim is a path an operator cannot check
     * against the target's own `ls` before the move. */
    if path
        .chars()
        .any(|c| c.is_whitespace() || c == '\'' || c == '"')
    {
        bail!("Target upload path must not contain whitespace or quote characters.");
    }
    // coreutils `sha256sum` prefixes its line with a backslash when the file
    // name contains one, which would corrupt the LINKR_UPLOAD:sha256 marker.
    if path.contains('\\') {
        bail!("Target upload path must not contain a backslash.");
    }
    Ok(path)
}

fn upload_size(value: i64) -> Result<u64> {
    if value < 0 {
        bail!("Upload size must be a non-negative integer.");
    }
    Ok(value as u64)
}

fn upload_chunk_size(value: i64) -> Result<u64> {
    if value <= 0 {
        bail!("Upload chunk size must be a positive integer.");
    }
    if value > MAX_CHUNK_BYTES {
        bail!("Upload chunk size must not exceed {MAX_CHUNK_BYTES} bytes.");
    }
    Ok(value as u64)
}

/// Empty means "do not check", anything else must be a full digest.
fn upload_sha256(value: &str) -> Result<String> {
    let sha256 = value.trim().to_lowercase();
    if !sha256.is_empty() && !(sha256.len() == 64 && sha256.chars().all(|c| c.is_ascii_hexdigit()))
    {
        bail!("Expected SHA-256 must contain 64 hexadecimal characters.");
    }
    Ok(sha256)
}

fn upload_start(value: i64, size: u64) -> Result<u64> {
    if value < 0 || value as u64 > size {
        bail!(
            "Upload resume offset must be a non-negative integer no greater than the upload size."
        );
    }
    Ok(value as u64)
}

fn chunk_arg(name: &str, value: i64) -> Result<u64> {
    if value < 0 {
        bail!("Upload chunk {name} must be a non-negative integer.");
    }
    Ok(value as u64)
}

/// One chunk: decode the base64 and append it to the part file, then report the
/// size the target actually measured. `fresh` truncates instead of appending,
/// so a stale part file from an older attempt cannot be extended into a corrupt
/// upload; resume passes `fresh = false`.
pub fn upload_chunk_command(
    temp_path: &str,
    index: i64,
    offset: i64,
    bytes: i64,
    base64: &str,
    fresh: bool,
) -> Result<String> {
    let part = upload_path(temp_path)?;
    let index = chunk_arg("index", index)?;
    let offset = chunk_arg("offset", offset)?;
    if bytes <= 0 {
        bail!("Upload chunk length must be a positive integer.");
    }
    if !is_padded_base64(base64) {
        bail!("Upload chunk payload must be padded base64.");
    }
    let op = if fresh { ">" } else { ">>" };
    Ok(format!(
        "p={}; printf '%s' {} | base64 -d {op} \"$p\" && m=$(wc -c < \"$p\" | tr -d ' \\t') && printf '\\nLINKR_UPLOAD:chunk index={index} offset={offset} bytes={bytes} total=%s\\n' \"$m\"",
        quote_shell(part)?,
        quote_shell(base64)?,
    ))
}

/// Size probe plus optional digest of the part file.
pub fn upload_verify_command(temp_path: &str, sha256: &str) -> Result<String> {
    let mut parts = vec![
        format!("p={}", quote_shell(temp_path)?),
        r#"[ -f "$p" ] || { printf '\nLINKR_UPLOAD:missing\n'; exit 0; }"#.to_string(),
        r#"n=$(wc -c < "$p" | tr -d ' \t') || { printf '\nLINKR_UPLOAD:missing\n'; exit 0; }"#
            .to_string(),
        r#"printf '\nLINKR_UPLOAD:bytes=%s\n' "$n""#.to_string(),
    ];
    // The hash costs a full read of the part file, so it is only computed when
    // a digest is actually waiting to be compared against.
    if !sha256.is_empty() {
        parts.push(
            r#"if command -v sha256sum >/dev/null 2>&1; then h=$(sha256sum "$p" || printf unavailable); elif command -v shasum >/dev/null 2>&1; then h=$(shasum -a 256 "$p" || printf unavailable); else h=unavailable; fi"#
                .to_string(),
        );
        // Both printers write "<hash>  <file>"; keep only the digest column.
        parts.push(r#"h=${h%% *}"#.to_string());
        parts.push(r#"printf '\nLINKR_UPLOAD:sha256=%s\n' "$h""#.to_string());
    }
    Ok(parts.join("; "))
}

/// The only command that touches the destination, and it runs only after
/// re-reading the part file and confirming both the size and, when one was
/// expected, the digest. Exit 65 covers a wrong size or digest, 66 a missing
/// part file; the marker says which.
pub fn upload_complete_command(
    path: &str,
    temp_path: &str,
    size: u64,
    sha256: &str,
) -> Result<String> {
    let mut parts = vec![
        format!("t={}", quote_shell(temp_path)?),
        format!("d={}", quote_shell(path)?),
        r#"[ -f "$t" ] || { printf '\nLINKR_UPLOAD:missing\n'; exit 66; }"#.to_string(),
        r#"n=$(wc -c < "$t" | tr -d ' \t') || { printf '\nLINKR_UPLOAD:missing\n'; exit 66; }"#
            .to_string(),
        format!(
            r#"[ "$n" = '{size}' ] || {{ printf '\nLINKR_UPLOAD:error size-mismatch actual=%s\n' "$n"; exit 65; }}"#
        ),
    ];
    if !sha256.is_empty() {
        parts.push(
            r#"if command -v sha256sum >/dev/null 2>&1; then h=$(sha256sum "$t" || printf unavailable); elif command -v shasum >/dev/null 2>&1; then h=$(shasum -a 256 "$t" || printf unavailable); else h=unavailable; fi"#
                .to_string(),
        );
        parts.push(r#"h=${h%% *}"#.to_string());
        parts.push(r#"printf '\nLINKR_UPLOAD:sha256=%s\n' "$h""#.to_string());
        parts.push(format!(
            r#"[ "$h" = {} ] || {{ printf '\nLINKR_UPLOAD:error hash-mismatch actual=%s\n' "$h"; exit 65; }}"#,
            quote_shell(sha256)?
        ));
    }
    parts.push(r#"printf '\nLINKR_UPLOAD:bytes=%s\n' "$n""#.to_string());
    parts.push(r#"mv -f "$t" "$d" && printf '\nLINKR_UPLOAD:complete\n'"#.to_string());
    Ok(parts.join("; "))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadChunk {
    pub index: u64,
    pub offset: u64,
    pub bytes: u64,
    pub base64: Option<String>,
    pub command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadPlan {
    pub path: String,
    pub size: u64,
    pub temp_path: String,
    pub from: u64,
    pub chunk_bytes: u64,
    pub expected_sha256: String,
    pub chunks: Vec<UploadChunk>,
    /// Creates the part file before the first chunk, and is how a zero-byte
    /// upload gets a file to verify and move. Must not run when resuming.
    pub prepare_command: Option<String>,
    pub verify_command: String,
    pub complete_command: String,
}

/// Inputs for [`upload_plan`]. `chunk_bytes`, `sha256` and `start_at` default
/// to the JS module defaults through [`UploadRequest::new`].
#[derive(Debug, Clone)]
pub struct UploadRequest<'a> {
    pub path: &'a str,
    pub size: i64,
    pub chunk_bytes: i64,
    pub sha256: &'a str,
    pub start_at: i64,
    pub data: Option<&'a [u8]>,
}

impl<'a> UploadRequest<'a> {
    pub fn new(path: &'a str, size: i64) -> Self {
        Self {
            path,
            size,
            chunk_bytes: DEFAULT_CHUNK_BYTES,
            sha256: "",
            start_at: 0,
            data: None,
        }
    }
}

/// Plan a chunked upload.
///
/// Without `data` the plan still carries the geometry, the part path and the
/// verify/complete commands, but `chunks[].command` and `chunks[].base64` are
/// `None` because only the caller has the bytes. `start_at` is how a resume
/// reuses the same part file: chunks below that offset are not emitted and
/// `prepare_command` is `None`.
pub fn upload_plan(request: UploadRequest<'_>) -> Result<UploadPlan> {
    let destination = upload_path(request.path)?.to_string();
    let total = upload_size(request.size)?;
    let chunk = upload_chunk_size(request.chunk_bytes)?;
    let expected = upload_sha256(request.sha256)?;
    let from = upload_start(request.start_at, total)?;
    let temp_path = format!("{destination}{PART_SUFFIX}");
    let remaining = total - from;
    if let Some(data) = request.data {
        if data.len() as u64 != remaining {
            bail!(
                "Upload data holds {} bytes but {remaining} bytes remain to send.",
                data.len()
            );
        }
    }
    let needed = remaining.div_ceil(chunk) as usize;
    if needed > MAX_UPLOAD_CHUNKS {
        bail!("Upload would need more than {MAX_UPLOAD_CHUNKS} chunks; send a smaller file or a larger chunk size.");
    }
    let mut chunks = Vec::new();
    let mut offset = from;
    while offset < total {
        let bytes = chunk.min(total - offset);
        let entry_index = offset / chunk;
        /* The frame length depends only on the encoded size, so the budget is
         * checked even before the caller hands over the bytes: a destination
         * that cannot work should fail while planning, not halfway through
         * sending. */
        let encoded = match request.data {
            Some(data) => {
                let start = (offset - from) as usize;
                encode_base64(&data[start..start + bytes as usize])
            }
            None => "A".repeat(bytes.div_ceil(3) as usize * 4),
        };
        // `fresh` defaults to `offset === 0` in the JS call: only the very first
        // chunk truncates a stale part file, every later one appends.
        let command = upload_chunk_command(
            &temp_path,
            entry_index as i64,
            offset as i64,
            bytes as i64,
            &encoded,
            offset == 0,
        )?;
        if command.len() > MAX_UPLOAD_COMMAND_BYTES {
            bail!(
                "Upload command would be {} characters, over the {MAX_UPLOAD_COMMAND_BYTES}-character UART budget; shorten the path or lower chunkBytes.",
                command.len()
            );
        }
        chunks.push(UploadChunk {
            index: entry_index,
            offset,
            bytes,
            base64: request.data.map(|_| encoded),
            command: request.data.map(|_| command),
        });
        offset += chunk;
    }
    let verify_command = upload_verify_command(&temp_path, &expected)?;
    let complete_command = upload_complete_command(&destination, &temp_path, total, &expected)?;
    let prepare = if from == 0 {
        Some(format!(": > {}", quote_shell(&temp_path)?))
    } else {
        None
    };
    Ok(UploadPlan {
        path: destination,
        size: total,
        temp_path,
        from,
        chunk_bytes: chunk,
        expected_sha256: expected,
        chunks,
        prepare_command: prepare,
        verify_command,
        complete_command,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadStatus {
    Ok,
    Mismatch,
    Incomplete,
}

impl UploadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            UploadStatus::Ok => "ok",
            UploadStatus::Mismatch => "mismatch",
            UploadStatus::Incomplete => "incomplete",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadChunkMarker {
    pub index: u64,
    pub offset: u64,
    pub bytes: u64,
    pub total: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadProgress {
    pub status: UploadStatus,
    pub count: usize,
    pub from: Option<u64>,
    pub bytes: u64,
    pub next_offset: Option<u64>,
    pub markers: Vec<UploadChunkMarker>,
    pub reason: String,
}

/// Read back the per-chunk confirmations. `ok` means every confirmation that
/// arrived is contiguous and matches its own announced length — it does NOT
/// mean the whole plan arrived. `next_offset` is `None` when nothing consistent
/// was confirmed, so a resume never starts from a guessed offset.
pub fn parse_upload_progress(text: &str) -> UploadProgress {
    let markers: Vec<UploadChunkMarker> = marker_lines(text)
        .iter()
        .filter_map(|line| re_upload_chunk().captures(line))
        .map(|caps| UploadChunkMarker {
            index: caps[1].parse().unwrap_or(u64::MAX),
            offset: caps[2].parse().unwrap_or(u64::MAX),
            bytes: caps[3].parse().unwrap_or(u64::MAX),
            total: caps[4].parse().unwrap_or(u64::MAX),
        })
        .collect();
    if markers.is_empty() {
        return UploadProgress {
            status: UploadStatus::Incomplete,
            count: 0,
            from: None,
            bytes: 0,
            next_offset: None,
            markers,
            reason: "No LINKR_UPLOAD:chunk confirmations in the output.".to_string(),
        };
    }
    let fail = |reason: String| UploadProgress {
        status: UploadStatus::Incomplete,
        count: markers.len(),
        from: Some(markers[0].offset),
        bytes: 0,
        next_offset: None,
        markers: markers.clone(),
        reason,
    };
    for (i, marker) in markers.iter().enumerate() {
        let started_at = if i == 0 {
            marker.offset
        } else {
            markers[i - 1].total
        };
        if marker.offset != started_at {
            return fail(format!(
                "Chunk {} starts at {} but the target reported {started_at} bytes: run verifyCommand and resume from its byte count.",
                marker.index, marker.offset
            ));
        }
        // `checked_add`, not `+`: a marker is device output and all four counts
        // come from `\d+`, so a corrupt line can carry one near `u64::MAX`.
        // Plain addition wraps in release — which makes this very consistency
        // check pass on numbers that cannot be right — and panics under the
        // debug profile the tests run in.
        if marker.offset.checked_add(marker.bytes) != Some(marker.total) {
            return fail(format!(
                "Chunk {} appended {} bytes instead of {}: run verifyCommand and resume from its byte count.",
                marker.index,
                // The same hazard in the figure this message prints: `total`
                // may sit below `offset` here, which `u64` cannot express (the
                // JS reference prints the negative number; `i128` matches it
                // and cannot wrap).
                i128::from(marker.total) - i128::from(marker.offset),
                marker.bytes
            ));
        }
    }
    let first = &markers[0];
    let last = markers.last().unwrap();
    UploadProgress {
        status: UploadStatus::Ok,
        count: markers.len(),
        from: Some(first.offset),
        bytes: last.total - first.offset,
        next_offset: Some(last.total),
        markers: markers.clone(),
        reason: String::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadResult {
    pub status: UploadStatus,
    pub bytes: u64,
    pub sha256: String,
    pub expected_sha256: String,
    pub moved: bool,
    pub reason: String,
}

/// Read back `verifyCommand` or `completeCommand` output.
///
/// Pass the expected size (and digest) so the byte count and digest can be
/// compared against what was intended. Without an expected size the only
/// acceptable evidence is the `LINKR_UPLOAD:complete` marker. A malformed
/// expectation returns an error instead of quietly switching the check off.
pub fn parse_upload_result(text: &str, size: Option<i64>, sha256: &str) -> Result<UploadResult> {
    let expect_size = size.map(upload_size).transpose()?;
    let expect_sha = upload_sha256(sha256)?;
    let mut bytes: Option<u64> = None;
    let mut sha = String::new();
    let mut sha_unavailable = false;
    let mut missing = false;
    let mut moved = false;
    let mut size_mismatch: Option<Option<u64>> = None;
    let mut hash_mismatch = false;
    for line in marker_lines(text) {
        if line == "LINKR_UPLOAD:missing" {
            missing = true;
        } else if line == "LINKR_UPLOAD:complete" {
            moved = true;
        } else if line == "LINKR_UPLOAD:sha256=unavailable" {
            sha_unavailable = true;
        } else if let Some(caps) = re_upload_bytes().captures(&line) {
            bytes = caps[1].parse().ok();
        } else if let Some(caps) = re_upload_sha().captures(&line) {
            sha = caps[1].to_string();
        } else if let Some(caps) = re_upload_size_mismatch().captures(&line) {
            size_mismatch = Some(caps.get(1).and_then(|m| m.as_str().parse().ok()));
        } else if let Some(caps) = re_upload_hash_mismatch().captures(&line) {
            hash_mismatch = true;
            if let Some(actual) = caps.get(1) {
                let actual = actual.as_str();
                if actual.len() == 64 && actual.chars().all(|c| c.is_ascii_hexdigit()) {
                    sha = actual.to_string();
                }
            }
        }
    }
    let result = UploadResult {
        status: UploadStatus::Incomplete,
        bytes: bytes.unwrap_or(0),
        sha256: sha.clone(),
        expected_sha256: expect_sha.clone(),
        moved,
        reason: String::new(),
    };
    let mismatch = |reason: String, bytes: u64| UploadResult {
        status: UploadStatus::Mismatch,
        bytes,
        reason,
        ..result.clone()
    };
    let incomplete = |reason: &str| UploadResult {
        status: UploadStatus::Incomplete,
        reason: reason.to_string(),
        ..result.clone()
    };
    // Most specific failure first: a visible digest mismatch is a mismatch even
    // when the command stopped before it could report a byte count.
    if missing {
        return Ok(incomplete(
            "The target has no part file for this upload; nothing was written.",
        ));
    }
    /* A reported size mismatch is definitive even when it arrives without the
     * measured count: downgrading it to "not finished" would turn a real
     * disagreement into an unresolved transfer and invite a resume over a file
     * the target has already said is wrong. */
    if let Some(actual) = size_mismatch {
        let measured = actual.unwrap_or(0);
        let reason = match actual {
            None => {
                "The target reported a size mismatch without the measured byte count.".to_string()
            }
            Some(n) => format!(
                "The target reported {n} bytes{}.",
                match expect_size {
                    Some(want) => format!(" instead of {want}"),
                    None => String::new(),
                }
            ),
        };
        return Ok(mismatch(reason, measured));
    }
    if let Some(want) = expect_size {
        if let Some(actual) = bytes {
            if actual != want {
                return Ok(mismatch(
                    format!("The target reported {actual} bytes instead of {want}."),
                    result.bytes,
                ));
            }
        }
    }
    if !expect_sha.is_empty() {
        if hash_mismatch || (!sha.is_empty() && sha != expect_sha) {
            return Ok(mismatch(
                format!(
                    "The target's sha256 is {}, not {expect_sha}.",
                    if sha.is_empty() {
                        "unreadable".to_string()
                    } else {
                        sha.clone()
                    }
                ),
                result.bytes,
            ));
        }
        if sha_unavailable {
            return Ok(incomplete(
                "The target has neither sha256sum nor shasum, so the expected digest could not be checked.",
            ));
        }
        if sha.is_empty() {
            return Ok(incomplete(
                "The output has no sha256 result, so the expected digest could not be checked.",
            ));
        }
    } else if hash_mismatch {
        return Ok(mismatch(
            "The target reported a sha256 mismatch.".to_string(),
            result.bytes,
        ));
    }
    if expect_size.is_some() && bytes.is_none() {
        return Ok(incomplete(
            "The output has no byte count, so the upload cannot be confirmed.",
        ));
    }
    if expect_size.is_none() && !moved {
        return Ok(incomplete(
            "Without an expected size only the LINKR_UPLOAD:complete marker confirms an upload; pass the plan or { size }.",
        ));
    }
    Ok(UploadResult {
        status: UploadStatus::Ok,
        reason: String::new(),
        ..result
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn js() -> String {
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../web/target_files.js"
        ))
        .expect("read web/target_files.js")
    }

    /// The contract test: every marker and label we print must exist byte for
    /// byte in the web client's module.
    #[test]
    fn markers_match_web_client_byte_for_byte() {
        let src = js();
        for marker in [
            "LINKR_FILE:begin",
            "LINKR_FILE:end",
            "LINKR_FILE:error",
            "LINKR_UPLOAD:chunk index=",
            "LINKR_UPLOAD:bytes=",
            "LINKR_UPLOAD:sha256=",
            "LINKR_UPLOAD:complete",
            "LINKR_UPLOAD:missing",
            "LINKR_UPLOAD:error size-mismatch",
            "LINKR_UPLOAD:error hash-mismatch",
            "LINKR_UPLOAD:sha256=unavailable",
            ".linkr-part",
            "LINKR_TOOL:%s",
        ] {
            assert!(src.contains(marker), "web/target_files.js lost {marker:?}");
        }
        assert_eq!(FILE_BEGIN_PREFIX, "LINKR_FILE:begin");
        assert_eq!(FILE_END_PREFIX, "LINKR_FILE:end");
        assert_eq!(FILE_ERROR_PREFIX, "LINKR_FILE:error");
        assert_eq!(FILE_ACK_PREFIX, "LINKR_FILE:ack");
        assert_eq!(PART_SUFFIX, ".linkr-part");

        // The printf lines we emit carry the same text, un-escaped once.
        let cmd = read_file_command("/etc/hostname", 0, 16).unwrap();
        assert!(cmd.contains(r"printf '\nLINKR_FILE:begin total=%s from=0\n'"));
        assert!(cmd.contains(r"printf '\nLINKR_FILE:end bytes=%s\n'"));
        assert!(src.contains(r"printf '\\nLINKR_FILE:begin total=%s from="));
        let chunk = upload_chunk_command("/a/b.linkr-part", 0, 0, 3, "YWJj", true).unwrap();
        assert!(
            chunk.contains(r"printf '\nLINKR_UPLOAD:chunk index=0 offset=0 bytes=3 total=%s\n'")
        );
        assert!(src.contains(r"printf '\\nLINKR_UPLOAD:chunk index="));
        assert_eq!(TARGET_FILE_PROBE, js_probe());
    }

    /// TARGET_FILE_PROBE is a template literal in JS: the source escapes each
    /// backslash, the emitted command does not.
    fn js_probe() -> String {
        let src = js();
        let start = src.find("for t in dd").expect("probe in JS");
        let end = start + src[start..].find('`').unwrap();
        src[start..end].replace("\\\\", "\\")
    }

    #[test]
    fn quoting_rules() {
        assert_eq!(quote_shell("plain").unwrap(), "'plain'");
        assert_eq!(quote_shell("a'b").unwrap(), "'a'\\''b'");
        assert_eq!(
            quote_shell("\0").unwrap_err().to_string(),
            "Shell quoting cannot represent a NUL byte."
        );
    }

    #[test]
    fn format_byte_counts() {
        assert_eq!(format_bytes(0).unwrap(), "0 B");
        assert_eq!(format_bytes(8192).unwrap(), "8 KiB");
        assert_eq!(format_bytes(1536).unwrap(), "1.5 KiB");
        assert_eq!(format_bytes(1024 * 1024 * 3).unwrap(), "3 MiB");
        assert_eq!(
            format_bytes(-1).unwrap_err().to_string(),
            "Byte count must be a non-negative number."
        );
    }

    #[test]
    fn read_command_shape() {
        let cmd = read_file_command("/etc/hosts", 4, 1024).unwrap();
        assert_eq!(
            cmd,
            [
                "p='/etc/hosts'",
                r#"case "$p" in /*) ;; *) printf '\nLINKR_FILE:error not-absolute\n'; exit 0 ;; esac"#,
                r#"[ -e "$p" ] || { printf '\nLINKR_FILE:error missing\n'; exit 0; }"#,
                r#"[ -d "$p" ] && { printf '\nLINKR_FILE:error directory\n'; exit 0; }"#,
                r#"[ -r "$p" ] || { printf '\nLINKR_FILE:error denied\n'; exit 0; }"#,
                r#"[ -f "$p" ] || { printf '\nLINKR_FILE:error not-regular\n'; exit 0; }"#,
                r#"t=$(wc -c < "$p" | tr -d ' \t') || { printf '\nLINKR_FILE:error denied\n'; exit 0; }"#,
                r#"n=$((t - 4)); [ "$n" -gt 0 ] || n=0"#,
                r#"[ "$n" -gt 1024 ] && n=1024"#,
                r#"printf '\nLINKR_FILE:begin total=%s from=4\n' "$t""#,
                r#"dd bs=1 skip=4 count=$n < "$p" 2>/dev/null | base64"#,
                r#"printf '\nLINKR_FILE:end bytes=%s\n' "$n""#,
            ]
            .join("; ")
        );
    }

    #[test]
    fn read_command_validation_messages() {
        assert_eq!(
            read_file_command("", 0, 4).unwrap_err().to_string(),
            "Target path must be a non-empty string."
        );
        assert_eq!(
            read_file_command("/a\nb", 0, 4).unwrap_err().to_string(),
            "Target path must not contain control characters."
        );
        assert_eq!(
            read_file_command("/a", -1, 4).unwrap_err().to_string(),
            "Read offset must be a non-negative integer."
        );
        assert_eq!(
            read_file_command("/a", 0, 0).unwrap_err().to_string(),
            "Read size must be a positive integer."
        );
        assert_eq!(
            read_file_command("/a", 0, 1025).unwrap_err().to_string(),
            "Read size must not exceed 1024 bytes."
        );
    }

    #[test]
    fn base64_round_trip() {
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
        assert_eq!(encode_base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(decode_base64("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(decode_base64("Zg==").unwrap(), b"f");
        assert_eq!(decode_base64("Zm8=").unwrap(), b"fo");
        assert!(decode_base64("Zm9v!").is_none());
        assert!(decode_base64("Zm9").is_none());
        assert!(decode_base64("======").is_none());
        let data: Vec<u8> = (0u8..=255).collect();
        assert_eq!(decode_base64(&encode_base64(&data)).unwrap(), data);
    }

    fn page(total: u64, from: u64, payload: &[u8], end_bytes: u64) -> String {
        format!(
            "echoed\r\nLINKR_FILE:begin total={total} from={from}\r\n{}\r\nLINKR_FILE:end bytes={end_bytes}\r\n",
            encode_base64(payload)
        )
    }

    #[test]
    fn parse_ok_page() {
        let data = b"hello, target".to_vec();
        let text = page(100, 0, &data, data.len() as u64);
        let read = parse_file_read(&text);
        assert_eq!(read.status, ReadStatus::Ok);
        assert_eq!(read.total_bytes, Some(100));
        assert_eq!(read.from, Some(0));
        assert_eq!(read.bytes, Some(data.len() as u64));
        assert_eq!(read.data.as_deref(), Some(data.as_slice()));
        assert_eq!(read.reason, "");
    }

    #[test]
    fn parse_wrapped_payload_and_short_page() {
        let data: Vec<u8> = (0u8..200).collect();
        let b64 = encode_base64(&data);
        let wrapped: Vec<String> = b64
            .as_bytes()
            .chunks(64)
            .map(|c| String::from_utf8(c.to_vec()).unwrap())
            .collect();
        // A wrapped stream is joined before the whitespace is stripped, and a
        // page starting beyond the head of the file is a normal short page.
        let text = format!(
            "LINKR_FILE:begin total=400 from=200\n{}\nLINKR_FILE:end bytes=200\n",
            wrapped.join("\n")
        );
        let read = parse_file_read(&text);
        assert_eq!(read.status, ReadStatus::Ok);
        assert_eq!(read.from, Some(200));
        assert_eq!(read.bytes, Some(200));
        assert_eq!(read.data.as_ref().unwrap().len(), 200);
        // A page at or past EOF is legitimately empty.
        let text = "LINKR_FILE:begin total=10 from=10\n\nLINKR_FILE:end bytes=0\n";
        let read = parse_file_read(text);
        assert_eq!(read.status, ReadStatus::Ok);
        assert_eq!(read.data.as_deref(), Some(b"".as_slice()));
    }

    #[test]
    fn parse_last_begin_wins() {
        let stale = page(50, 0, b"stale", 5);
        let fresh = page(90, 10, b"fresh!", 6);
        let text = format!("{stale}\n{fresh}");
        let read = parse_file_read(&text);
        assert_eq!(read.status, ReadStatus::Ok);
        assert_eq!(read.from, Some(10));
        assert_eq!(read.data.as_deref(), Some(b"fresh!".as_slice()));
    }

    #[test]
    fn parse_error_markers() {
        for (detail, status, reason) in [
            (
                "missing",
                ReadStatus::Missing,
                "Target file does not exist.",
            ),
            (
                "directory",
                ReadStatus::Directory,
                "Target path is a directory.",
            ),
            (
                "not-absolute",
                ReadStatus::Denied,
                "Target path is not absolute.",
            ),
            ("denied", ReadStatus::Denied, "Target file is not readable."),
            (
                "not-regular",
                ReadStatus::Denied,
                "Target path is not a regular file.",
            ),
        ] {
            let read = parse_file_read(&format!("sh: can't open\nLINKR_FILE:error {detail}\n"));
            assert_eq!(read.status, status, "{detail}");
            assert_eq!(read.reason, reason, "{detail}");
        }
        let read = parse_file_read("LINKR_FILE:error sideways\n");
        assert_eq!(read.status, ReadStatus::Incomplete);
        assert_eq!(read.reason, "Target reported LINKR_FILE:error sideways.");
    }

    #[test]
    fn parse_failures() {
        assert_eq!(
            parse_file_read("nothing here").reason,
            "No LINKR_FILE markers in the output: the command did not run or its output was truncated."
        );
        let truncated = format!(
            "LINKR_FILE:begin total=10 from=0\n{}",
            encode_base64(b"1234")
        );
        assert_eq!(
            parse_file_read(&truncated).reason,
            "The read stopped before its end marker: the payload is truncated."
        );
        let corrupted = "LINKR_FILE:begin total=10 from=0\nnot base64!!\nLINKR_FILE:end bytes=4";
        assert_eq!(
            parse_file_read(corrupted).reason,
            "The payload is not valid base64: the transfer was corrupted or truncated."
        );
        let lying = format!(
            "LINKR_FILE:begin total=10 from=0\n{}\nLINKR_FILE:end bytes=9",
            encode_base64(b"1234")
        );
        assert_eq!(
            parse_file_read(&lying).reason,
            "The payload holds 4 bytes but the end marker declared 9."
        );
        let overreach = format!(
            "LINKR_FILE:begin total=4 from=2\n{}\nLINKR_FILE:end bytes=4",
            encode_base64(b"abcd")
        );
        assert_eq!(
            parse_file_read(&overreach).reason,
            "The read claims bytes 2..6 of a 4-byte file."
        );
        let unsafe_counts =
            "LINKR_FILE:begin total=99999999999999999999 from=0\n\nLINKR_FILE:end bytes=0";
        assert_eq!(
            parse_file_read(unsafe_counts).reason,
            "The markers do not carry safe byte counts: the output was corrupted."
        );
    }

    #[test]
    fn upload_path_messages() {
        let cases = [
            ("relative/path", "Target upload path must be absolute."),
            (
                "/dir/",
                "Target upload path must name a file, not a directory.",
            ),
            (
                "/a b",
                "Target upload path must not contain whitespace or quote characters.",
            ),
            (
                "/a'b",
                "Target upload path must not contain whitespace or quote characters.",
            ),
            ("/a\\b", "Target upload path must not contain a backslash."),
            ("", "Target path must be a non-empty string."),
        ];
        for (path, message) in cases {
            assert_eq!(
                upload_plan(UploadRequest::new(path, 1))
                    .unwrap_err()
                    .to_string(),
                message,
                "{path}"
            );
        }
    }

    #[test]
    fn upload_plan_geometry() {
        let data = vec![7u8; 1000];
        let plan = upload_plan(UploadRequest {
            data: Some(&data),
            ..UploadRequest::new("/etc/app.conf", 1000)
        })
        .unwrap();
        assert_eq!(plan.path, "/etc/app.conf");
        assert_eq!(plan.temp_path, "/etc/app.conf.linkr-part");
        assert_eq!(plan.from, 0);
        assert_eq!(plan.chunk_bytes, 720);
        assert_eq!(plan.chunks.len(), 2);
        assert_eq!(plan.chunks[0].bytes, 720);
        assert_eq!(plan.chunks[1].bytes, 280);
        assert_eq!(plan.chunks[0].index, 0);
        assert_eq!(plan.chunks[1].index, 1);
        assert_eq!(
            plan.prepare_command.as_deref(),
            Some(": > '/etc/app.conf.linkr-part'")
        );
        assert!(plan.verify_command.contains("LINKR_UPLOAD:bytes="));
        assert!(plan.complete_command.contains("mv -f \"$t\" \"$d\""));
        assert!(plan.complete_command.contains("LINKR_UPLOAD:complete"));
        let first = plan.chunks[0].command.as_deref().unwrap();
        assert!(first.starts_with("p='/etc/app.conf.linkr-part'; printf '%s' '"));
        assert!(first.contains("base64 -d > \"$p\""));
        // Only offset 0 truncates; every later chunk appends (JS `fresh = offset === 0`).
        assert!(plan.chunks[1]
            .command
            .as_deref()
            .unwrap()
            .contains("base64 -d >> \"$p\""));
        let decoded = decode_base64(plan.chunks[1].base64.as_deref().unwrap()).unwrap();
        assert_eq!(decoded, &data[720..]);
    }

    #[test]
    fn upload_plan_without_bytes_and_resume() {
        let plan = upload_plan(UploadRequest::new("/usr/bin/tool", 5000)).unwrap();
        assert_eq!(plan.chunks.len(), 7);
        assert!(plan
            .chunks
            .iter()
            .all(|c| c.command.is_none() && c.base64.is_none()));
        assert!(plan
            .chunks
            .iter()
            .all(|c| c.bytes == 720 || c.bytes == 5000 % 720));
        assert_eq!(
            plan.prepare_command.as_deref(),
            Some(": > '/usr/bin/tool.linkr-part'")
        );

        let resume = upload_plan(UploadRequest {
            start_at: 720,
            ..UploadRequest::new("/usr/bin/tool", 5000)
        })
        .unwrap();
        assert_eq!(resume.from, 720);
        assert_eq!(resume.chunks.len(), 6);
        assert_eq!(resume.chunks[0].offset, 720);
        assert_eq!(resume.chunks[0].index, 1);
        assert!(resume.prepare_command.is_none());

        // A resume point that is not on a chunk boundary still emits whole
        // chunks measured from `startAt`; `index` stays the plan-wide index.
        let uneven = upload_plan(UploadRequest {
            start_at: 100,
            ..UploadRequest::new("/usr/bin/tool", 1000)
        })
        .unwrap();
        assert_eq!(uneven.chunks[0].offset, 100);
        assert_eq!(uneven.chunks[0].bytes, 720);
        assert_eq!(uneven.chunks[0].index, 0);
        assert_eq!(uneven.chunks[1].offset, 820);
        assert_eq!(uneven.chunks[1].bytes, 180);
        assert_eq!(uneven.chunks[1].index, 1);
    }

    #[test]
    fn upload_plan_budget_messages() {
        let data = vec![0u8; 10];
        let err = upload_plan(UploadRequest {
            size: 10,
            data: Some(&data),
            ..UploadRequest::new("/tmp/x", 10)
        })
        .map(|_| ());
        assert!(err.is_ok());
        assert_eq!(
            upload_plan(UploadRequest::new("/tmp/x", 100 * 1024 * 1024))
                .unwrap_err()
                .to_string(),
            "Upload would need more than 20000 chunks; send a smaller file or a larger chunk size."
        );
        let long_path = format!("/tmp/{}", "a".repeat(4000));
        assert_eq!(
            upload_plan(UploadRequest::new(&long_path, 10)).unwrap_err().to_string(),
            format!(
                "Upload command would be {} characters, over the {MAX_UPLOAD_COMMAND_BYTES}-character UART budget; shorten the path or lower chunkBytes.",
                upload_chunk_command(&format!("{long_path}{PART_SUFFIX}"), 0, 0, 10, &"A".repeat(16), true)
                    .unwrap()
                    .len()
            )
        );
        assert_eq!(
            upload_plan(UploadRequest::new("/tmp/x", -1))
                .unwrap_err()
                .to_string(),
            "Upload size must be a non-negative integer."
        );
        assert_eq!(
            upload_plan(UploadRequest {
                chunk_bytes: 4096,
                ..UploadRequest::new("/tmp/x", 10)
            })
            .unwrap_err()
            .to_string(),
            "Upload chunk size must not exceed 2048 bytes."
        );
        assert_eq!(
            upload_plan(UploadRequest {
                sha256: "abc",
                ..UploadRequest::new("/tmp/x", 10)
            })
            .unwrap_err()
            .to_string(),
            "Expected SHA-256 must contain 64 hexadecimal characters."
        );
        assert_eq!(
            upload_plan(UploadRequest {
                start_at: 11,
                ..UploadRequest::new("/tmp/x", 10)
            })
            .unwrap_err()
            .to_string(),
            "Upload resume offset must be a non-negative integer no greater than the upload size."
        );
        assert_eq!(
            upload_plan(UploadRequest {
                data: Some(&data),
                ..UploadRequest::new("/tmp/x", 11)
            })
            .unwrap_err()
            .to_string(),
            "Upload data holds 10 bytes but 11 bytes remain to send."
        );
    }

    #[test]
    fn chunk_command_messages() {
        assert_eq!(
            upload_chunk_command("/a.linkr-part", -1, 0, 4, "YWJj", true)
                .unwrap_err()
                .to_string(),
            "Upload chunk index must be a non-negative integer."
        );
        assert_eq!(
            upload_chunk_command("/a.linkr-part", 0, -1, 4, "YWJj", true)
                .unwrap_err()
                .to_string(),
            "Upload chunk offset must be a non-negative integer."
        );
        assert_eq!(
            upload_chunk_command("/a.linkr-part", 0, 0, 0, "YWJj", true)
                .unwrap_err()
                .to_string(),
            "Upload chunk length must be a positive integer."
        );
        assert_eq!(
            upload_chunk_command("/a.linkr-part", 0, 0, 3, "YWJj!", true)
                .unwrap_err()
                .to_string(),
            "Upload chunk payload must be padded base64."
        );
        let fresh = upload_chunk_command("/a.linkr-part", 2, 720, 4, "YWJj", true).unwrap();
        assert_eq!(
            fresh,
            "p='/a.linkr-part'; printf '%s' 'YWJj' | base64 -d > \"$p\" && m=$(wc -c < \"$p\" | tr -d ' \\t') && printf '\\nLINKR_UPLOAD:chunk index=2 offset=720 bytes=4 total=%s\\n' \"$m\""
        );
        let append = upload_chunk_command("/a.linkr-part", 2, 720, 4, "YWJj", false).unwrap();
        assert!(append.contains("base64 -d >> \"$p\""));
    }

    #[test]
    fn progress_contiguous_is_ok() {
        let text = "\
LINKR_UPLOAD:chunk index=0 offset=0 bytes=720 total=720\n\
LINKR_UPLOAD:chunk index=1 offset=720 bytes=280 total=1000\n";
        let progress = parse_upload_progress(text);
        assert_eq!(progress.status, UploadStatus::Ok);
        assert_eq!(progress.count, 2);
        assert_eq!(progress.from, Some(0));
        assert_eq!(progress.bytes, 1000);
        assert_eq!(progress.next_offset, Some(1000));
    }

    #[test]
    fn progress_gap_or_short_write_is_incomplete() {
        let gap = "LINKR_UPLOAD:chunk index=0 offset=0 bytes=720 total=720\n\
                   LINKR_UPLOAD:chunk index=1 offset=900 bytes=100 total=1000\n";
        let progress = parse_upload_progress(gap);
        assert_eq!(progress.status, UploadStatus::Incomplete);
        assert_eq!(
            progress.reason,
            "Chunk 1 starts at 900 but the target reported 720 bytes: run verifyCommand and resume from its byte count."
        );
        let short = "LINKR_UPLOAD:chunk index=0 offset=0 bytes=720 total=700\n";
        let progress = parse_upload_progress(short);
        assert_eq!(
            progress.reason,
            "Chunk 0 appended 700 bytes instead of 720: run verifyCommand and resume from its byte count."
        );
        assert_eq!(progress.next_offset, None);
        assert_eq!(progress.from, Some(0));
        let none = parse_upload_progress("nothing");
        assert_eq!(
            none.reason,
            "No LINKR_UPLOAD:chunk confirmations in the output."
        );
        assert_eq!(none.count, 0);
    }

    /// A marker is device output, so its four counts are whatever the target
    /// printed — `\d+` accepts twenty digits. `offset + bytes` on those wraps
    /// in release (letting an impossible marker look consistent) and panics
    /// under the debug profile the tests run in, and the figure the mismatch
    /// message prints (`total - offset`) underflows the same way. Both now go
    /// through arithmetic that cannot wrap; the verdict is unchanged.
    #[test]
    fn a_chunk_marker_whose_counts_overflow_is_refused_not_wrapped() {
        let max = "18446744073709551615"; // u64::MAX, and a valid `\d+` run
        let text = format!("LINKR_UPLOAD:chunk index=1 offset={max} bytes={max} total=0\n");
        let progress = parse_upload_progress(&text);
        assert_eq!(progress.status, UploadStatus::Incomplete);
        assert_eq!(progress.count, 1);
        assert_eq!(progress.next_offset, None);
        assert_eq!(progress.bytes, 0);
        // `total - offset` is negative here, exactly as the JS reference
        // prints it; the point is that it is *printed*, not wrapped.
        assert_eq!(
            progress.reason,
            format!(
                "Chunk 1 appended -{max} bytes instead of {max}: run verifyCommand and resume from its byte count."
            )
        );
    }

    #[test]
    fn upload_result_ok_and_mismatch() {
        let sha = "a".repeat(64);
        let complete =
            format!("LINKR_UPLOAD:bytes=1000\nLINKR_UPLOAD:sha256={sha}\nLINKR_UPLOAD:complete\n");
        let result = parse_upload_result(&complete, Some(1000), &sha).unwrap();
        assert_eq!(result.status, UploadStatus::Ok);
        assert!(result.moved);
        assert_eq!(result.bytes, 1000);

        let result = parse_upload_result("LINKR_UPLOAD:complete\n", None, "").unwrap();
        assert_eq!(result.status, UploadStatus::Ok);
        let result = parse_upload_result("LINKR_UPLOAD:bytes=10\n", None, "").unwrap();
        assert_eq!(result.status, UploadStatus::Incomplete);
        assert_eq!(
            result.reason,
            "Without an expected size only the LINKR_UPLOAD:complete marker confirms an upload; pass the plan or { size }."
        );

        let short = format!("LINKR_UPLOAD:bytes=999\nLINKR_UPLOAD:sha256={sha}\n");
        let result = parse_upload_result(&short, Some(1000), &sha).unwrap();
        assert_eq!(result.status, UploadStatus::Mismatch);
        assert_eq!(
            result.reason,
            "The target reported 999 bytes instead of 1000."
        );

        let bad_hash = format!(
            "LINKR_UPLOAD:bytes=1000\nLINKR_UPLOAD:sha256={}\n",
            "b".repeat(64)
        );
        let result = parse_upload_result(&bad_hash, Some(1000), &sha).unwrap();
        assert_eq!(result.status, UploadStatus::Mismatch);
        assert_eq!(
            result.reason,
            format!("The target's sha256 is {}, not {sha}.", "b".repeat(64))
        );

        let missing = parse_upload_result("LINKR_UPLOAD:missing\n", Some(10), "").unwrap();
        assert_eq!(missing.status, UploadStatus::Incomplete);
        assert_eq!(
            missing.reason,
            "The target has no part file for this upload; nothing was written."
        );

        let size_mismatch = "LINKR_UPLOAD:error size-mismatch actual=42\n";
        let result = parse_upload_result(size_mismatch, Some(100), "").unwrap();
        assert_eq!(result.status, UploadStatus::Mismatch);
        assert_eq!(result.bytes, 42);
        assert_eq!(
            result.reason,
            "The target reported 42 bytes instead of 100."
        );

        let bare = "LINKR_UPLOAD:error size-mismatch\n";
        let result = parse_upload_result(bare, None, "").unwrap();
        assert_eq!(
            result.reason,
            "The target reported a size mismatch without the measured byte count."
        );

        let unavailable = "LINKR_UPLOAD:bytes=1000\nLINKR_UPLOAD:sha256=unavailable\n";
        let result = parse_upload_result(unavailable, Some(1000), &sha).unwrap();
        assert_eq!(result.status, UploadStatus::Incomplete);
        assert_eq!(
            result.reason,
            "The target has neither sha256sum nor shasum, so the expected digest could not be checked."
        );

        let no_bytes = format!("LINKR_UPLOAD:sha256={sha}\n");
        let result = parse_upload_result(&no_bytes, Some(1000), &sha).unwrap();
        assert_eq!(result.status, UploadStatus::Incomplete);
        assert_eq!(
            result.reason,
            "The output has no byte count, so the upload cannot be confirmed."
        );

        let hash_only = "LINKR_UPLOAD:error hash-mismatch actual=deadbeef\n";
        let result = parse_upload_result(hash_only, None, "").unwrap();
        assert_eq!(result.status, UploadStatus::Mismatch);
        assert_eq!(result.reason, "The target reported a sha256 mismatch.");

        assert_eq!(
            parse_upload_result("", Some(-1), "")
                .unwrap_err()
                .to_string(),
            "Upload size must be a non-negative integer."
        );
    }
}
