//! Target-side download planning (`web/download_plan.js`).

use super::files::quote_shell;

/// The target tool probe for downloads.
pub const DOWNLOAD_PROBE: &str =
    "for t in curl wget sha256sum shasum openssl; do command -v \"$t\" >/dev/null 2>&1 && printf 'LINKR_TOOL:%s\\n' \"$t\"; done; :";

/// A validated download request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRequest {
    pub url: String,
    pub sha256: String,
}

/// Validate a download URL and optional expected digest.
pub fn validate_download(url: &str, sha256: &str) -> Result<DownloadRequest, String> {
    // Require http(s) without credentials.
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("Download requires an HTTP(S) URL without credentials.".to_string());
    }
    if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        if rest.contains('@')
            && rest
                .split('/')
                .next()
                .is_some_and(|host| host.contains('@'))
        {
            return Err("Download requires an HTTP(S) URL without credentials.".to_string());
        }
    }
    let digest = sha256.trim().to_lowercase();
    if !digest.is_empty() && (digest.len() != 64 || !digest.chars().all(|c| c.is_ascii_hexdigit()))
    {
        return Err("Expected SHA-256 must contain 64 hexadecimal characters.".to_string());
    }
    Ok(DownloadRequest {
        url: url.to_string(),
        sha256: digest,
    })
}

/// A planned target download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetDownloadPlan {
    pub command: String,
    pub path: String,
    pub url: String,
    pub downloader: &'static str,
    pub checksum: &'static str,
    pub expected_sha256: Option<String>,
}

/// Plan a target download against a completed tool probe's evidence.
///
/// `probe_evidence` is the output of `DOWNLOAD_PROBE`; `probe_ok` reports
/// whether that probe completed with exit code 0 (and wasn't truncated).
pub fn target_download_plan(
    url: &str,
    sha256: &str,
    path: &str,
    probe_evidence: &str,
    probe_ok: bool,
) -> Result<TargetDownloadPlan, String> {
    let req = validate_download(url, sha256)?;
    if !probe_ok {
        return Err(
            "Complete probe_download_tools and inspect its result before downloading.".to_string(),
        );
    }
    let available: Vec<&str> = probe_evidence
        .lines()
        .filter_map(|line| {
            let line = line.trim_end_matches('\r');
            line.strip_prefix("LINKR_TOOL:")
        })
        .collect();
    let has = |tool: &str| available.contains(&tool);
    let downloader = if has("curl") {
        "curl"
    } else if has("wget") {
        "wget"
    } else {
        return Err("Target needs curl/wget and a SHA-256 tool. Report missing tools before choosing another action.".to_string());
    };
    let checksum = if has("sha256sum") {
        "sha256sum"
    } else if has("shasum") {
        "shasum"
    } else if has("openssl") {
        "openssl"
    } else {
        return Err("Target needs curl/wget and a SHA-256 tool. Report missing tools before choosing another action.".to_string());
    };

    if !path.starts_with('/') || path.ends_with('/') || path.chars().any(|c| c.is_control()) {
        return Err(
            "Target destination must be an absolute file path without control characters."
                .to_string(),
        );
    }

    let fetch = if downloader == "curl" {
        "curl -fL --progress-bar -o \"$p\" "
    } else {
        "wget -O \"$p\" "
    };
    let hash = match checksum {
        "sha256sum" => "sha256sum \"$p\"",
        "shasum" => "shasum -a 256 \"$p\"",
        _ => "openssl dgst -sha256 \"$p\"",
    };
    let hash_trim = if checksum == "openssl" {
        "##* "
    } else {
        "%% *"
    };
    let d = quote_shell(path)?;
    let url_q = quote_shell(&req.url)?;
    let expected = if req.sha256.is_empty() {
        String::new()
    } else {
        format!("test \"$h\" = {} || exit 65; ", quote_shell(&req.sha256)?)
    };
    let command = format!(
        "d={d}; test ! -e \"$d\" || exit 73; p=$(mktemp \"$d.part.XXXXXX\") || exit; printf 'LINKR_PART:%s\\n' \"$p\"; {fetch}{url_q} || exit; h=$({hash}) || exit; h=${{h{hash_trim}}}; printf '\\nLINKR_SHA256:%s\\n' \"$h\"; {expected}ln \"$p\" \"$d\" || exit; rm \"$p\"; printf 'LINKR_BYTES:'; wc -c < \"$d\""
    );

    Ok(TargetDownloadPlan {
        command,
        path: path.to_string(),
        url: req.url,
        downloader,
        checksum,
        expected_sha256: if req.sha256.is_empty() {
            None
        } else {
            Some(req.sha256)
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROBE_OK: &str = "LINKR_TOOL:curl\nLINKR_TOOL:sha256sum\n";

    #[test]
    fn validates_urls_and_digests() {
        assert!(validate_download("https://example.com/x", "").is_ok());
        assert!(validate_download("http://example.com/x", "").is_ok());
        assert!(validate_download("ftp://example.com/x", "").is_err());
        assert!(validate_download("https://user:pass@example.com/x", "").is_err());
        assert!(validate_download("https://example.com/x", "not-a-sha").is_err());
        let ok = validate_download("https://example.com/x", "a".repeat(64).as_str()).unwrap();
        assert_eq!(ok.sha256, "a".repeat(64));
    }

    #[test]
    fn plans_a_curl_download() {
        let plan = target_download_plan(
            "https://example.com/f.bin",
            "",
            "/tmp/f.bin",
            PROBE_OK,
            true,
        )
        .unwrap();
        assert_eq!(plan.downloader, "curl");
        assert_eq!(plan.checksum, "sha256sum");
        assert!(plan.command.contains("curl -fL"));
        assert!(plan.command.contains("LINKR_SHA256"));
        assert!(plan.command.contains("LINKR_BYTES"));
        assert_eq!(plan.expected_sha256, None);
    }

    #[test]
    fn plans_a_wget_download_with_digest() {
        let digest = "a".repeat(64);
        let probe = "LINKR_TOOL:wget\nLINKR_TOOL:shasum\n";
        let plan =
            target_download_plan("https://example.com/f", &digest, "/tmp/f", probe, true).unwrap();
        assert_eq!(plan.downloader, "wget");
        assert_eq!(plan.checksum, "shasum");
        assert_eq!(plan.expected_sha256.as_deref(), Some(digest.as_str()));
        assert!(plan.command.contains("exit 65"));
    }

    #[test]
    fn refuses_without_a_completed_probe() {
        assert!(target_download_plan("https://x", "", "/tmp/x", "", false).is_err());
    }

    #[test]
    fn refuses_when_tools_are_missing() {
        assert!(
            target_download_plan("https://x", "", "/tmp/x", "LINKR_TOOL:curl\n", true).is_err()
        );
        assert!(target_download_plan("https://x", "", "/tmp/x", "", true).is_err());
    }

    #[test]
    fn refuses_bad_destinations() {
        assert!(target_download_plan("https://x", "", "relative", PROBE_OK, true).is_err());
        assert!(target_download_plan("https://x", "", "/tmp/dir/", PROBE_OK, true).is_err());
    }
}
