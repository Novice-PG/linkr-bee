//! Device profile probe and parser (`web/device_profile.js`).

/// The profile probe: one command that prints bounded markers around `uname`,
/// `os-release`, the device-tree model, a boot id and the available tools.
pub const PROFILE_PROBE: &str = "printf 'LINKR_PROFILE_BEGIN\\n'; uname -a; printf 'LINKR_OS\\n'; cat /etc/os-release 2>/dev/null; printf 'LINKR_MODEL\\n'; cat /proc/device-tree/model 2>/dev/null; printf '\\nLINKR_BOOT\\n'; cat /proc/sys/kernel/random/boot_id 2>/dev/null; printf 'LINKR_DISK\\n'; df -Pk /; printf 'LINKR_TOOLS\\n'; for t in sh curl wget sha256sum shasum openssl sudo systemctl busybox; do command -v \"$t\" >/dev/null 2>&1 && printf 'TOOL:%s\\n' \"$t\"; done; printf 'LINKR_PROFILE_END\\n'";

/// A parsed device profile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceProfile {
    pub system: String,
    pub os: String,
    pub model: String,
    pub boot_id: String,
    pub storage: String,
    pub tools: Vec<String>,
}

/// Parse the output of `PROFILE_PROBE`. Returns `None` when the record
/// boundaries are absent.
pub fn parse_device_profile(text: &str) -> Option<DeviceProfile> {
    // UART shells can emit LF-CR; normalize before matching boundaries.
    let normalized = text.replace('\r', "");
    let begin = normalized.find("LINKR_PROFILE_BEGIN\n")?;
    let rest = &normalized[begin + "LINKR_PROFILE_BEGIN\n".len()..];
    let end = rest.find("\nLINKR_PROFILE_END\n")?;
    let source = &rest[..end];

    let field = |a: &str, b: &str| -> String {
        source
            .split(&format!("\n{a}\n"))
            .nth(1)
            .map(|after| after.split(&format!("\n{b}")).next().unwrap_or(""))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
            .chars()
            .take(1200)
            .collect()
    };

    let system = source
        .split("\nLINKR_OS")
        .next()
        .unwrap_or("")
        .chars()
        .take(500)
        .collect::<String>()
        .trim()
        .to_string();

    let tools = source
        .lines()
        .filter_map(|line| line.strip_prefix("TOOL:"))
        .map(|t| t.to_string())
        .collect();

    Some(DeviceProfile {
        system,
        os: field("LINKR_OS", "LINKR_MODEL"),
        model: field("LINKR_MODEL", "LINKR_BOOT"),
        boot_id: field("LINKR_BOOT", "LINKR_DISK"),
        storage: field("LINKR_DISK", "LINKR_TOOLS"),
        tools,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_profile() {
        let text = "LINKR_PROFILE_BEGIN\n\
            Linux radxa 6.1.0\n\
            LINKR_OS\n\
            PRETTY_NAME=\"Debian GNU/Linux 12\"\n\
            LINKR_MODEL\n\
            Radxa Rock 5B\n\
            \n\
            LINKR_BOOT\n\
            9f8e7d6c-5b4a-3210-fedc-ba9876543210\n\
            LINKR_DISK\n\
            Filesystem     1K-blocks  Used Available Use% Mounted on\n\
            /dev/root       12345678 1000 12344678   1% /\n\
            LINKR_TOOLS\n\
            TOOL:sh\n\
            TOOL:curl\n\
            TOOL:sudo\n\
            LINKR_PROFILE_END\n";
        let profile = parse_device_profile(text).unwrap();
        assert_eq!(profile.model, "Radxa Rock 5B");
        assert_eq!(profile.boot_id, "9f8e7d6c-5b4a-3210-fedc-ba9876543210");
        assert_eq!(profile.tools, vec!["sh", "curl", "sudo"]);
        assert!(profile.os.contains("Debian"));
    }

    #[test]
    fn returns_none_without_boundaries() {
        assert!(parse_device_profile("not a profile").is_none());
        assert!(parse_device_profile("LINKR_PROFILE_BEGIN\nbut no end").is_none());
    }

    #[test]
    fn tolerates_crlf() {
        let text = "LINKR_PROFILE_BEGIN\r\nuname -a\r\nLINKR_OS\r\nos\r\nLINKR_MODEL\r\nm\r\n\r\nLINKR_BOOT\r\nb\r\nLINKR_DISK\r\nd\r\nLINKR_TOOLS\r\nLINKR_PROFILE_END\r\n";
        let profile = parse_device_profile(text).unwrap();
        assert_eq!(profile.model, "m");
        assert_eq!(profile.os, "os");
    }
}
