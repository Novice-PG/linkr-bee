//! Target file reads and uploads: the shell protocol only (`web/target_files.js`).
//!
//! Every marker is one whole line and matched only as a complete line, so the
//! shell's echo of a command can never be mistaken for its output. `printf`
//! prints every marker, never `echo -e`, because busybox `echo` has no portable
//! escape handling.

use super::base64;

/// One read page: 1024 raw bytes encode to 1368 base64 characters, keeping the
/// whole exchange around 2 KiB — inside the 8192-byte RX ring.
pub const MAX_READ_BYTES: usize = 1024;
/// The default upload chunk size.
pub const DEFAULT_CHUNK_BYTES: usize = 720;
/// The largest upload chunk.
pub const MAX_CHUNK_BYTES: usize = 2048;
/// The suffix appended to the destination to form the part-file path.
pub const PART_SUFFIX: &str = ".linkr-part";
/// The command-size budget for one upload chunk.
pub const MAX_UPLOAD_COMMAND_BYTES: usize = 4096;
/// The most chunks a plan may produce (≈14 MiB at the default chunk size).
pub const MAX_UPLOAD_CHUNKS: usize = 20_000;

/// The target-side tool probe for file operations.
pub const TARGET_FILE_PROBE: &str =
    "for t in dd base64 wc tr sha256sum shasum; do command -v \"$t\" >/dev/null 2>&1 && printf 'LINKR_TOOL:%s\\n' \"$t\"; done; :";

/// Quote a string for POSIX shell, refusing NUL (which cannot cross argv).
pub fn quote_shell(text: &str) -> Result<String, String> {
    if text.contains('\0') {
        return Err("Shell quoting cannot represent a NUL byte.".to_string());
    }
    Ok(format!("'{}'", text.replace('\'', "'\\''")))
}

/// Format a byte count as a human string (`8 KiB`, `1.5 MiB`), matching the
/// web's `formatBytes`.
pub fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 || value.fract() == 0.0 {
        format!("{} {}", value as u64, UNITS[unit])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

// --- reads ------------------------------------------------------------------

const READ_BEGIN: &str = "LINKR_FILE:begin";
const READ_END: &str = "LINKR_FILE:end";
const READ_ERROR: &str = "LINKR_FILE:error";

/// The result of parsing a file read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRead {
    /// `ok` when the page is whole and consistent.
    pub status: FileReadStatus,
    /// The file's total size, when known.
    pub total_bytes: Option<u64>,
    /// The read offset.
    pub from: Option<u64>,
    /// The number of bytes in this page.
    pub bytes: Option<u64>,
    /// The decoded payload.
    pub data: Option<Vec<u8>>,
    /// Why the read is not `ok`, already worded for the reader.
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileReadStatus {
    Ok,
    Incomplete,
    /// The path was refused by the shell (missing, directory, not-absolute,
    /// denied, not-regular).
    Denied,
}

/// Build the `readFileCommand` for a path, offset and byte count.
///
/// The page is clamped in the shell from the size `wc -c` reports, and that
/// clamped count is what `end bytes=` announces. Every refusal `exit 0`s on
/// purpose: a missing file is a result the caller reads from the marker, not a
/// transport failure.
pub fn read_file_command(path: &str, offset: u64, bytes: usize) -> Result<String, String> {
    if bytes == 0 || bytes > MAX_READ_BYTES {
        return Err(format!("Read size must not exceed {MAX_READ_BYTES} bytes."));
    }
    if path.is_empty() {
        return Err("Target path must be a non-empty string.".to_string());
    }
    if path.chars().any(|c| c.is_control()) {
        return Err("Target path must not contain control characters.".to_string());
    }
    let target = quote_shell(path)?;
    let from = offset;
    let count = bytes;
    Ok([
        format!("p={target}"),
        "case \"$p\" in /*) ;; *) printf '\\nLINKR_FILE:error not-absolute\\n'; exit 0 ;; esac"
            .to_string(),
        "[ -e \"$p\" ] || { printf '\\nLINKR_FILE:error missing\\n'; exit 0; }".to_string(),
        "[ -d \"$p\" ] && { printf '\\nLINKR_FILE:error directory\\n'; exit 0; }".to_string(),
        "[ -r \"$p\" ] || { printf '\\nLINKR_FILE:error denied\\n'; exit 0; }".to_string(),
        "[ -f \"$p\" ] || { printf '\\nLINKR_FILE:error not-regular\\n'; exit 0; }".to_string(),
        "t=$(wc -c < \"$p\" | tr -d ' \\t') || { printf '\\nLINKR_FILE:error denied\\n'; exit 0; }"
            .to_string(),
        format!("n=$((t - {from})); [ \"$n\" -gt 0 ] || n=0"),
        format!("[ \"$n\" -gt {count} ] && n={count}"),
        format!("printf '\\nLINKR_FILE:begin total=%s from={from}\\n' \"$t\""),
        format!("dd bs=1 skip={from} count=$n < \"$p\" 2>/dev/null | base64"),
        "printf '\\nLINKR_FILE:end bytes=%s\\n' \"$n\"".to_string(),
    ]
    .join("; "))
}

/// The refusal reasons the shell can print before a read starts, mapped to the
/// status the caller sees.
fn read_failure(kind: &str) -> (FileReadStatus, &'static str) {
    match kind {
        "missing" => (FileReadStatus::Denied, "Target file does not exist."),
        "directory" => (FileReadStatus::Denied, "Target path is a directory."),
        "not-absolute" => (FileReadStatus::Denied, "Target path is not absolute."),
        "denied" => (FileReadStatus::Denied, "Target file is not readable."),
        "not-regular" => (FileReadStatus::Denied, "Target path is not a regular file."),
        _ => (
            FileReadStatus::Incomplete,
            "Target reported an unknown error.",
        ),
    }
}

/// Parse the output of a `readFileCommand`.
pub fn parse_file_read(text: &str) -> FileRead {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').map(|l| l.trim_end()).collect();

    let miss = |reason: String| FileRead {
        status: FileReadStatus::Incomplete,
        total_bytes: None,
        from: None,
        bytes: None,
        data: None,
        reason,
    };

    // The LAST begin marker wins: a journal tail can still hold an earlier read.
    let begin = lines.iter().rposition(|l| l.starts_with(READ_BEGIN));
    let Some(begin) = begin else {
        // No begin: only then is an error marker trusted.
        let failure = lines.iter().find_map(|l| {
            l.strip_prefix(&format!("{READ_ERROR} "))
                .map(|r| r.split_whitespace().next().unwrap_or("").to_string())
        });
        if let Some(kind) = failure {
            let (status, reason) = read_failure(&kind);
            return FileRead {
                status,
                total_bytes: None,
                from: None,
                bytes: None,
                data: None,
                reason: reason.to_string(),
            };
        }
        return miss("No LINKR_FILE markers in the output: the command did not run or its output was truncated.".to_string());
    };

    let begin_line = lines[begin];
    let Some((total, from)) = parse_begin(begin_line) else {
        return miss("The begin marker is malformed.".to_string());
    };

    let end = lines
        .iter()
        .skip(begin + 1)
        .position(|l| l.starts_with(READ_END))
        .map(|p| p + begin + 1);
    let Some(end) = end else {
        return FileRead {
            status: FileReadStatus::Incomplete,
            total_bytes: Some(total),
            from: Some(from),
            bytes: None,
            data: None,
            reason: "The read stopped before its end marker: the payload is truncated.".to_string(),
        };
    };

    let end_bytes = lines[end]
        .strip_prefix(&format!("{READ_END} bytes="))
        .and_then(|s| s.parse::<u64>().ok());
    let Some(end_bytes) = end_bytes else {
        return FileRead {
            status: FileReadStatus::Incomplete,
            total_bytes: Some(total),
            from: Some(from),
            bytes: None,
            data: None,
            reason: "The end marker is malformed.".to_string(),
        };
    };

    // Wrapped base64 lines are slices of one stream, so join before stripping.
    let payload: String = lines[begin + 1..end]
        .join("")
        .replace(char::is_whitespace, "");
    if !base64::is_valid_base64(&payload) {
        return FileRead {
            status: FileReadStatus::Incomplete,
            total_bytes: Some(total),
            from: Some(from),
            bytes: Some(end_bytes),
            data: None,
            reason: "The payload is not valid base64: the transfer was corrupted or truncated."
                .to_string(),
        };
    }
    let data = base64::decode_base64(&payload).unwrap_or_default();
    if data.len() as u64 != end_bytes {
        return FileRead {
            status: FileReadStatus::Incomplete,
            total_bytes: Some(total),
            from: Some(from),
            bytes: Some(end_bytes),
            data: None,
            reason: format!(
                "The payload holds {} bytes but the end marker declared {end_bytes}.",
                data.len()
            ),
        };
    }
    if end_bytes > 0 && from + end_bytes > total {
        return FileRead {
            status: FileReadStatus::Incomplete,
            total_bytes: Some(total),
            from: Some(from),
            bytes: Some(end_bytes),
            data: None,
            reason: format!(
                "The read claims bytes {from}..{} of a {total}-byte file.",
                from + end_bytes
            ),
        };
    }
    FileRead {
        status: FileReadStatus::Ok,
        total_bytes: Some(total),
        from: Some(from),
        bytes: Some(end_bytes),
        data: Some(data),
        reason: String::new(),
    }
}

fn parse_begin(line: &str) -> Option<(u64, u64)> {
    // LINKR_FILE:begin total=<n> from=<offset>
    let rest = line.strip_prefix(&format!("{READ_BEGIN} total="))?;
    let (total, rest) = rest.split_once(" from=")?;
    let total = total.parse().ok()?;
    let from = rest.parse().ok()?;
    Some((total, from))
}

// --- uploads ----------------------------------------------------------------

const UPLOAD_CHUNK: &str = "LINKR_UPLOAD:chunk";

/// A chunked upload plan.
#[derive(Debug, Clone)]
pub struct UploadPlan {
    pub path: String,
    pub size: u64,
    pub temp_path: String,
    pub from: u64,
    pub chunk_bytes: usize,
    pub expected_sha256: String,
    pub chunks: Vec<UploadChunk>,
    pub prepare_command: Option<String>,
    pub verify_command: String,
    pub complete_command: String,
}

/// One chunk of an upload plan.
#[derive(Debug, Clone)]
pub struct UploadChunk {
    pub index: usize,
    pub offset: u64,
    pub bytes: usize,
    pub base64: Option<String>,
    pub command: Option<String>,
}

fn upload_path(path: &str) -> Result<String, String> {
    if path.is_empty() {
        return Err("Target path must be a non-empty string.".to_string());
    }
    if !path.starts_with('/') {
        return Err("Target upload path must be absolute.".to_string());
    }
    if path.ends_with('/') {
        return Err("Target upload path must name a file, not a directory.".to_string());
    }
    if path
        .chars()
        .any(|c| c.is_whitespace() || c == '\'' || c == '"')
    {
        return Err(
            "Target upload path must not contain whitespace or quote characters.".to_string(),
        );
    }
    if path.contains('\\') {
        return Err("Target upload path must not contain a backslash.".to_string());
    }
    if path.chars().any(|c| c.is_control()) {
        return Err("Target path must not contain control characters.".to_string());
    }
    Ok(path.to_string())
}

fn upload_sha256(value: &str) -> Result<String, String> {
    let sha = value.trim().to_lowercase();
    if !sha.is_empty() && (sha.len() != 64 || !sha.chars().all(|c| c.is_ascii_hexdigit())) {
        return Err("Expected SHA-256 must contain 64 hexadecimal characters.".to_string());
    }
    Ok(sha)
}

/// Build an upload plan. `data` is optional; without it the plan carries the
/// geometry and commands but `chunks[].base64`/`command` are empty.
pub fn upload_plan(
    path: &str,
    size: u64,
    chunk_bytes: usize,
    sha256: &str,
    start_at: u64,
    data: Option<&[u8]>,
) -> Result<UploadPlan, String> {
    let destination = upload_path(path)?;
    let chunk = if chunk_bytes == 0 || chunk_bytes > MAX_CHUNK_BYTES {
        return Err(format!(
            "Upload chunk size must not exceed {MAX_CHUNK_BYTES} bytes."
        ));
    } else {
        chunk_bytes
    };
    let expected = upload_sha256(sha256)?;
    if start_at > size {
        return Err(
            "Upload resume offset must be a non-negative integer no greater than the upload size."
                .to_string(),
        );
    }
    let from = start_at;
    let temp_path = format!("{destination}{PART_SUFFIX}");
    let remaining = size - from;
    if let Some(data) = data {
        if data.len() as u64 != remaining {
            return Err(format!(
                "Upload data holds {} bytes but {remaining} bytes remain to send.",
                data.len()
            ));
        }
    }
    if remaining.div_ceil(chunk as u64) as usize > MAX_UPLOAD_CHUNKS {
        return Err(format!(
            "Upload would need more than {MAX_UPLOAD_CHUNKS} chunks; send a smaller file or a larger chunk size."
        ));
    }

    let mut chunks = Vec::new();
    let mut offset = from;
    while offset < size {
        let bytes = ((size - offset) as usize).min(chunk);
        let index = (offset / chunk as u64) as usize;
        let encoded = match data {
            Some(d) => {
                let s = (offset - from) as usize;
                base64::encode_base64(&d[s..s + bytes])
            }
            None => "A".repeat(bytes.div_ceil(3) * 4),
        };
        let command = upload_chunk_command(&temp_path, index, offset, bytes, &encoded)?;
        if command.len() > MAX_UPLOAD_COMMAND_BYTES {
            return Err(format!(
                "Upload command would be {} characters, over the {MAX_UPLOAD_COMMAND_BYTES}-character UART budget; shorten the path or lower chunkBytes.",
                command.len()
            ));
        }
        let entry = UploadChunk {
            index,
            offset,
            bytes,
            base64: data.map(|_| encoded.clone()),
            command: data.map(|_| command.clone()),
        };
        chunks.push(entry);
        offset += bytes as u64;
    }

    Ok(UploadPlan {
        path: destination.clone(),
        size,
        temp_path: temp_path.clone(),
        from,
        chunk_bytes: chunk,
        expected_sha256: expected.clone(),
        chunks,
        prepare_command: if from == 0 {
            Some(format!(": > {}", quote_shell(&temp_path)?))
        } else {
            None
        },
        verify_command: upload_verify_command(&temp_path, &expected)?,
        complete_command: upload_complete_command(&destination, &temp_path, size, &expected)?,
    })
}

/// Build one chunk command: decode the base64 and append it to the part file.
pub fn upload_chunk_command(
    temp_path: &str,
    index: usize,
    offset: u64,
    bytes: usize,
    base64: &str,
) -> Result<String, String> {
    if bytes == 0 {
        return Err("Upload chunk length must be a positive integer.".to_string());
    }
    if !base64::is_valid_base64(base64) {
        return Err("Upload chunk payload must be padded base64.".to_string());
    }
    let part = quote_shell(temp_path)?;
    let payload = quote_shell(base64)?;
    let fresh = if offset == 0 { ">" } else { ">>" };
    Ok(format!(
        "p={part}; printf '%s' {payload} | base64 -d {fresh} \"$p\" && m=$(wc -c < \"$p\" | tr -d ' \\t') && printf '\\nLINKR_UPLOAD:chunk index={index} offset={offset} bytes={bytes} total=%s\\n' \"$m\""
    ))
}

/// Build the verify command: measure the part file, and hash it when a digest
/// is expected.
pub fn upload_verify_command(temp_path: &str, sha256: &str) -> Result<String, String> {
    let p = quote_shell(temp_path)?;
    let mut parts = vec![
        format!("p={p}"),
        "[ -f \"$p\" ] || { printf '\\nLINKR_UPLOAD:missing\\n'; exit 0; }".to_string(),
        "n=$(wc -c < \"$p\" | tr -d ' \\t') || { printf '\\nLINKR_UPLOAD:missing\\n'; exit 0; }"
            .to_string(),
        "printf '\\nLINKR_UPLOAD:bytes=%s\\n' \"$n\"".to_string(),
    ];
    if !sha256.is_empty() {
        parts.push(
            "if command -v sha256sum >/dev/null 2>&1; then h=$(sha256sum \"$p\" || printf unavailable); elif command -v shasum >/dev/null 2>&1; then h=$(shasum -a 256 \"$p\" || printf unavailable); else h=unavailable; fi".to_string(),
        );
        parts.push("h=${h%% *}".to_string());
        parts.push("printf '\\nLINKR_UPLOAD:sha256=%s\\n' \"$h\"".to_string());
    }
    Ok(parts.join("; "))
}

/// Build the complete command: re-verify size (and digest) then move the part
/// file over the destination.
pub fn upload_complete_command(
    path: &str,
    temp_path: &str,
    size: u64,
    sha256: &str,
) -> Result<String, String> {
    let t = quote_shell(temp_path)?;
    let d = quote_shell(path)?;
    let mut parts = vec![
        format!("t={t}"),
        format!("d={d}"),
        "[ -f \"$t\" ] || { printf '\\nLINKR_UPLOAD:missing\\n'; exit 66; }".to_string(),
        "n=$(wc -c < \"$t\" | tr -d ' \\t') || { printf '\\nLINKR_UPLOAD:missing\\n'; exit 66; }".to_string(),
        format!("[ \"$n\" = '{size}' ] || {{ printf '\\nLINKR_UPLOAD:error size-mismatch actual=%s\\n' \"$n\"; exit 65; }}"),
    ];
    if !sha256.is_empty() {
        parts.push(
            "if command -v sha256sum >/dev/null 2>&1; then h=$(sha256sum \"$t\" || printf unavailable); elif command -v shasum >/dev/null 2>&1; then h=$(shasum -a 256 \"$t\" || printf unavailable); else h=unavailable; fi".to_string(),
        );
        parts.push("h=${h%% *}".to_string());
        parts.push("printf '\\nLINKR_UPLOAD:sha256=%s\\n' \"$h\"".to_string());
        parts.push(format!("[ \"$h\" = {} ] || {{ printf '\\nLINKR_UPLOAD:error hash-mismatch actual=%s\\n' \"$h\"; exit 65; }}", quote_shell(sha256)?));
    }
    parts.push("printf '\\nLINKR_UPLOAD:bytes=%s\\n' \"$n\"".to_string());
    parts.push("mv -f \"$t\" \"$d\" && printf '\\nLINKR_UPLOAD:complete\\n'".to_string());
    Ok(parts.join("; "))
}

/// One parsed chunk confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadChunkMarker {
    pub index: u64,
    pub offset: u64,
    pub bytes: u64,
    pub total: u64,
}

/// The result of parsing per-chunk confirmations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadProgress {
    pub status: UploadProgressStatus,
    pub count: usize,
    pub from: Option<u64>,
    pub bytes: u64,
    pub next_offset: Option<u64>,
    pub markers: Vec<UploadChunkMarker>,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadProgressStatus {
    Ok,
    Incomplete,
}

/// Read back the per-chunk confirmations. `ok` means every confirmation is
/// contiguous and matches its own announced length — not that the whole plan
/// arrived. `next_offset` is `None` when nothing consistent was confirmed.
pub fn parse_upload_progress(text: &str) -> UploadProgress {
    let markers: Vec<UploadChunkMarker> = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .filter_map(|line| {
            let rest = line.strip_prefix(&format!("{UPLOAD_CHUNK} index="))?;
            let (index, rest) = rest.split_once(" offset=")?;
            let (offset, rest) = rest.split_once(" bytes=")?;
            let (bytes, total) = rest.split_once(" total=")?;
            Some(UploadChunkMarker {
                index: index.parse().ok()?,
                offset: offset.parse().ok()?,
                bytes: bytes.parse().ok()?,
                total: total.parse().ok()?,
            })
        })
        .collect();

    if markers.is_empty() {
        return UploadProgress {
            status: UploadProgressStatus::Incomplete,
            count: 0,
            from: None,
            bytes: 0,
            next_offset: None,
            markers,
            reason: "No LINKR_UPLOAD:chunk confirmations in the output.".to_string(),
        };
    }
    let fail = |reason: String| UploadProgress {
        status: UploadProgressStatus::Incomplete,
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
        if marker.total != marker.offset + marker.bytes {
            return fail(format!(
                "Chunk {} appended {} bytes instead of {}: run verifyCommand and resume from its byte count.",
                marker.index,
                marker.total - marker.offset,
                marker.bytes
            ));
        }
    }
    let last = markers.last().unwrap();
    UploadProgress {
        status: UploadProgressStatus::Ok,
        count: markers.len(),
        from: Some(markers[0].offset),
        bytes: last.total - markers[0].offset,
        next_offset: Some(last.total),
        markers,
        reason: String::new(),
    }
}

/// The result of parsing verify/complete output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadResult {
    pub status: UploadResultStatus,
    pub bytes: u64,
    pub sha256: String,
    pub expected_sha256: String,
    pub moved: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadResultStatus {
    Ok,
    Incomplete,
    Mismatch,
}

/// Read back `verifyCommand` or `completeCommand` output, compared against an
/// expected size and (optionally) digest.
pub fn parse_upload_result(
    text: &str,
    expected_size: Option<u64>,
    expected_sha256: &str,
) -> UploadResult {
    let expect_sha = upload_sha256(expected_sha256).unwrap_or_default();
    let mut bytes: Option<u64> = None;
    let mut sha256 = String::new();
    let mut sha_unavailable = false;
    let mut missing = false;
    let mut moved = false;
    let mut size_mismatch: Option<u64> = None;
    let mut size_mismatch_seen = false;
    let mut hash_mismatch = false;

    for line in text.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
        let line = line.trim_end();
        if line == "LINKR_UPLOAD:missing" {
            missing = true;
        } else if line == "LINKR_UPLOAD:complete" {
            moved = true;
        } else if line == "LINKR_UPLOAD:sha256=unavailable" {
            sha_unavailable = true;
        } else if let Some(rest) = line.strip_prefix("LINKR_UPLOAD:bytes=") {
            if let Ok(n) = rest.parse() {
                bytes = Some(n);
            }
        } else if let Some(rest) = line.strip_prefix("LINKR_UPLOAD:sha256=") {
            if rest.len() == 64 && rest.chars().all(|c| c.is_ascii_hexdigit()) {
                sha256 = rest.to_string();
            }
        } else if line.starts_with("LINKR_UPLOAD:error size-mismatch") {
            size_mismatch_seen = true;
            size_mismatch = line
                .strip_prefix("LINKR_UPLOAD:error size-mismatch actual=")
                .and_then(|s| s.parse().ok());
        } else if line.starts_with("LINKR_UPLOAD:error hash-mismatch") {
            hash_mismatch = true;
            if let Some(rest) = line.strip_prefix("LINKR_UPLOAD:error hash-mismatch actual=") {
                if rest.len() == 64 && rest.chars().all(|c| c.is_ascii_hexdigit()) {
                    sha256 = rest.to_string();
                }
            }
        }
    }

    let base = UploadResult {
        status: UploadResultStatus::Incomplete,
        bytes: bytes.unwrap_or(0),
        sha256: sha256.clone(),
        expected_sha256: expect_sha.clone(),
        moved,
        reason: String::new(),
    };
    let mismatch = |reason: String, b: u64| UploadResult {
        status: UploadResultStatus::Mismatch,
        bytes: b,
        reason,
        ..base.clone()
    };
    let incomplete = |reason: String| UploadResult {
        status: UploadResultStatus::Incomplete,
        reason,
        ..base.clone()
    };

    if missing {
        return incomplete(
            "The target has no part file for this upload; nothing was written.".to_string(),
        );
    }
    if size_mismatch_seen {
        return mismatch(
            match size_mismatch {
                Some(n) => format!(
                    "The target reported {n} bytes{}.",
                    expected_size
                        .map(|e| format!(" instead of {e}"))
                        .unwrap_or_default()
                ),
                None => "The target reported a size mismatch without the measured byte count."
                    .to_string(),
            },
            size_mismatch.unwrap_or(0),
        );
    }
    if let (Some(expect), Some(actual)) = (expected_size, bytes) {
        if actual != expect {
            return mismatch(
                format!("The target reported {actual} bytes instead of {expect}."),
                actual,
            );
        }
    }
    if !expect_sha.is_empty() {
        if hash_mismatch || (!sha256.is_empty() && sha256 != expect_sha) {
            return mismatch(
                format!(
                    "The target's sha256 is {}, not {expect_sha}.",
                    if sha256.is_empty() {
                        "unreadable"
                    } else {
                        &sha256
                    }
                ),
                bytes.unwrap_or(0),
            );
        }
        if sha_unavailable {
            return incomplete("The target has neither sha256sum nor shasum, so the expected digest could not be checked.".to_string());
        }
        if sha256.is_empty() {
            return incomplete(
                "The output has no sha256 result, so the expected digest could not be checked."
                    .to_string(),
            );
        }
    } else if hash_mismatch {
        return mismatch(
            "The target reported a sha256 mismatch.".to_string(),
            bytes.unwrap_or(0),
        );
    }
    if expected_size.is_some() && bytes.is_none() {
        return incomplete(
            "The output has no byte count, so the upload cannot be confirmed.".to_string(),
        );
    }
    if expected_size.is_none() && !moved {
        return incomplete("Without an expected size only the LINKR_UPLOAD:complete marker confirms an upload; pass the plan or { size }.".to_string());
    }
    UploadResult {
        status: UploadResultStatus::Ok,
        reason: String::new(),
        ..base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_shell_wraps_and_escapes() {
        assert_eq!(quote_shell("a b").unwrap(), "'a b'");
        assert_eq!(quote_shell("a'b").unwrap(), "'a'\\''b'");
        assert!(quote_shell("a\0b").is_err());
    }

    #[test]
    fn format_bytes_matches_the_web() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1 KiB");
        assert_eq!(format_bytes(2048), "2 KiB");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(1024 * 1024), "1 MiB");
    }

    #[test]
    fn the_read_command_refuses_bad_paths_in_shell() {
        let cmd = read_file_command("/etc/os-release", 0, 1024).unwrap();
        assert!(cmd.contains("dd bs=1 skip=0 count=$n"));
        assert!(cmd.contains("LINKR_FILE:begin"));
        // not-absolute is let through: the shell reports it.
        let cmd = read_file_command("relative", 0, 100).unwrap();
        assert!(cmd.contains("LINKR_FILE:error not-absolute"));
        // over the size budget is refused up front.
        assert!(read_file_command("/x", 0, 2048).is_err());
    }

    fn read_output(payload: &[u8], total: u64, from: u64) -> String {
        let mut s = format!("LINKR_FILE:begin total={total} from={from}\n");
        for line in base64::encode_base64(payload).as_bytes().chunks(76) {
            s.push_str(&String::from_utf8_lossy(line));
            s.push('\n');
        }
        s.push_str(&format!("LINKR_FILE:end bytes={}\n", payload.len()));
        s
    }

    #[test]
    fn parses_a_complete_read() {
        let payload = b"hello world";
        let out = read_output(payload, payload.len() as u64, 0);
        let result = parse_file_read(&out);
        assert_eq!(result.status, FileReadStatus::Ok);
        assert_eq!(result.total_bytes, Some(11));
        assert_eq!(result.bytes, Some(11));
        assert_eq!(result.data.as_deref(), Some(&payload[..]));
    }

    #[test]
    fn parses_a_refusal() {
        let result = parse_file_read("LINKR_FILE:error missing\n");
        assert_eq!(result.status, FileReadStatus::Denied);
        assert_eq!(result.reason, "Target file does not exist.");
    }

    #[test]
    fn a_truncated_read_is_incomplete_not_denied() {
        let result = parse_file_read("LINKR_FILE:begin total=100 from=0\nZm9v\n");
        assert_eq!(result.status, FileReadStatus::Incomplete);
        assert!(result.reason.contains("truncated"));
    }

    #[test]
    fn the_last_begin_wins_over_a_stale_pair() {
        let mut out = read_output(b"old", 3, 0);
        out.push_str(&read_output(b"newer", 5, 0));
        let result = parse_file_read(&out);
        assert_eq!(result.status, FileReadStatus::Ok);
        assert_eq!(result.data.as_deref(), Some(&b"newer"[..]));
    }

    #[test]
    fn upload_plan_geometry_matches_the_web() {
        let data = vec![0u8; 2000];
        let plan = upload_plan("/tmp/out.bin", 2000, 720, "", 0, Some(&data)).unwrap();
        assert_eq!(plan.chunks.len(), 3); // 720 + 720 + 560
        assert_eq!(plan.chunks[0].offset, 0);
        assert_eq!(plan.chunks[0].bytes, 720);
        assert_eq!(plan.chunks[2].bytes, 560);
        assert_eq!(
            plan.prepare_command.as_deref(),
            Some(": > '/tmp/out.bin.linkr-part'")
        );
        assert!(plan.verify_command.contains("LINKR_UPLOAD:bytes="));
        assert!(plan.complete_command.contains("LINKR_UPLOAD:complete"));
    }

    #[test]
    fn upload_plan_without_data_still_carries_geometry() {
        let plan = upload_plan("/tmp/out.bin", 1500, 720, "", 0, None).unwrap();
        assert_eq!(plan.chunks.len(), 3);
        assert!(plan.chunks[0].base64.is_none());
        assert!(plan.chunks[0].command.is_none());
    }

    #[test]
    fn upload_plan_resume_skips_earlier_chunks() {
        // Resume at 720: only the remaining 720 bytes are handed over.
        let data = vec![0u8; 720];
        let plan = upload_plan("/tmp/out.bin", 1440, 720, "", 720, Some(&data)).unwrap();
        assert_eq!(plan.chunks.len(), 1);
        assert_eq!(plan.chunks[0].offset, 720);
        assert_eq!(
            plan.prepare_command, None,
            "resume must not truncate the part file"
        );
    }

    #[test]
    fn upload_plan_validates_size_and_digest() {
        assert!(upload_plan("/tmp/x", 10, 720, "not-a-sha", 0, None).is_err());
        assert!(upload_plan("relative", 10, 720, "", 0, None).is_err());
        assert!(upload_plan("/tmp/x", 10, 0, "", 0, None).is_err());
        // data length mismatch
        assert!(upload_plan("/tmp/x", 10, 720, "", 0, Some(&[0u8; 5])).is_err());
    }

    #[test]
    fn upload_progress_accepts_contiguous_chunks() {
        let out = "LINKR_UPLOAD:chunk index=0 offset=0 bytes=720 total=720\nLINKR_UPLOAD:chunk index=1 offset=720 bytes=720 total=1440\n";
        let progress = parse_upload_progress(out);
        assert_eq!(progress.status, UploadProgressStatus::Ok);
        assert_eq!(progress.next_offset, Some(1440));
        assert_eq!(progress.bytes, 1440);
    }

    #[test]
    fn upload_progress_refuses_non_contiguous_chunks() {
        let out = "LINKR_UPLOAD:chunk index=0 offset=0 bytes=720 total=720\nLINKR_UPLOAD:chunk index=1 offset=800 bytes=720 total=1520\n";
        let progress = parse_upload_progress(out);
        assert_eq!(progress.status, UploadProgressStatus::Incomplete);
        assert_eq!(progress.next_offset, None);
    }

    #[test]
    fn upload_result_reports_mismatch_and_ok() {
        let mismatch = parse_upload_result(
            "LINKR_UPLOAD:bytes=100\nLINKR_UPLOAD:complete\n",
            Some(200),
            "",
        );
        assert_eq!(mismatch.status, UploadResultStatus::Mismatch);

        let ok = parse_upload_result(
            "LINKR_UPLOAD:bytes=200\nLINKR_UPLOAD:complete\n",
            Some(200),
            "",
        );
        assert_eq!(ok.status, UploadResultStatus::Ok);
        assert!(ok.moved);
    }

    #[test]
    fn upload_result_checks_the_digest() {
        let digest = "a".repeat(64);
        let ok = parse_upload_result(
            &format!(
                "LINKR_UPLOAD:bytes=10\nLINKR_UPLOAD:sha256={digest}\nLINKR_UPLOAD:complete\n"
            ),
            Some(10),
            &digest,
        );
        assert_eq!(ok.status, UploadResultStatus::Ok);

        let bad = parse_upload_result(
            &format!(
                "LINKR_UPLOAD:bytes=10\nLINKR_UPLOAD:sha256={}\n",
                "b".repeat(64)
            ),
            Some(10),
            &digest,
        );
        assert_eq!(bad.status, UploadResultStatus::Mismatch);
    }
}
