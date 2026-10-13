//! Target identity binding (`web/target_binding.js`).

/// The directory the identity file is created in.
pub const TARGET_DIRECTORY_SETUP: &str =
    "sudo install -d -m 0755 -o \"$(id -u)\" -g \"$(id -g)\" /var/lib/linkr";
/// The canonical identity path.
pub const TARGET_ID_PATH: &str = "/var/lib/linkr/device-id";

/// The UUID shape a valid target id must match.
pub const UUID_PATTERN: &str = "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$";

/// Whether a string is a valid target UUID.
pub fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, &b)| matches!(i, 8 | 13 | 18 | 23) || b.is_ascii_hexdigit())
}

/// Parse a `OK target=<none|uuid>` binding reply. Returns `None` for "none" and
/// the uuid otherwise; errors on anything else.
pub fn binding_reply(text: &str) -> Result<Option<String>, String> {
    let value = text.trim();
    let rest = value.strip_prefix("OK target=").ok_or_else(|| {
        "Bee binding unsupported or invalid response; update firmware.".to_string()
    })?;
    if rest == "none" {
        return Ok(None);
    }
    if is_uuid(rest) {
        return Ok(Some(rest.to_string()));
    }
    Err("Bee binding unsupported or invalid response; update firmware.".to_string())
}

/// Whether the marker is valid, matching `/^LINKR_ID_[a-f0-9]+$/`.
fn valid_marker(marker: &str) -> bool {
    marker.starts_with("LINKR_ID_")
        && marker.len() > "LINKR_ID_".len()
        && marker["LINKR_ID_".len()..]
            .chars()
            .all(|c| c.is_ascii_hexdigit())
}

/// Build the `targetIdentityCommand` for `verify`, `bind` or `regenerate`.
///
/// This is a faithful transcription of the upstream shell; it is long and
/// deliberately so — existing identities always win, and `sudo` only runs this
/// fixed id operation.
pub fn target_identity_command(action: &str, uuid: &str, marker: &str) -> Result<String, String> {
    if !matches!(action, "verify" | "bind" | "regenerate")
        || !is_uuid(uuid)
        || !valid_marker(marker)
    {
        return Err("Invalid identity request".to_string());
    }
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let quoted_uuid = quote(uuid);
    let quoted_marker = quote(marker);

    let write = match action {
        "verify" => String::new(),
        "bind" => "if test ! -e \"$p\"; then ".to_string(),
        _ => String::new(),
    };
    let write = format!(
        "{write}mkdir -p \"$d\"; t=$(mktemp \"$d/.id.XXXXXX\"); trap 'rm -f \"$t\"' EXIT; printf '%s\\n' \"$2\" > \"$t\"; chmod 644 \"$t\"; {}",
        if action == "bind" {
            "ln \"$t\" \"$p\"; fi;"
        } else if action == "regenerate" {
            "mv -f \"$t\" \"$p\";"
        } else {
            ""
        }
    );
    let operation = format!(
        "set -eu; p=$1; d=${{p%/*}}; test ! -L \"$d\"; test ! -L \"$p\"; {write}test -f \"$p\"; id=$(cat \"$p\"); printf '\\n%s:%s\\n%s_PATH:%s\\n' \"$3\" \"$id\" \"$3\" \"$p\""
    );
    let run = "sh -c \"$op\" sh";
    let args = format!("{quoted_uuid} {quoted_marker}");
    let existing = format!(
        "if test -e \"$s\" || test -L \"$s\"; then {run} \"$s\" {args}; exit $?; fi; if test -e \"$u\" || test -L \"$u\"; then {run} \"$u\" {args}; exit $?; fi;"
    );
    let existing_system = if action == "regenerate" {
        existing.replace(
            &format!("{run} \"$s\" {args};"),
            &format!("if test -w \"${{s%/*}}\"; then {run} \"$s\" {args}; else sudo -p '[sudo] password: ' {run} \"$s\" {args}; fi;"),
        )
    } else {
        existing
    };
    let create = if action == "verify" {
        "exit 1;".to_string()
    } else {
        format!(
            "if test -w \"${{s%/*}}\" || test \"$(id -u)\" = 0; then {run} \"$s\" {args} && exit 0; elif command -v sudo >/dev/null 2>&1; then sudo -p '[sudo] password: ' {run} \"$s\" {args} && exit 0; fi; test ! -e \"$s\" && test ! -L \"$s\" || exit 1; {run} \"$u\" {args};"
        )
    };
    Ok(format!(
        "op={}; s={TARGET_ID_PATH}; u=\"${{HOME:?}}/.local/share/linkr/device-id\"; {existing_system} {create}",
        quote(&operation)
    ))
}

/// Extract the observed target path from an execution's evidence.
pub fn observed_target_path(evidence: &str, marker: &str) -> Option<String> {
    let needle = format!("{marker}_PATH:");
    evidence
        .split('\n')
        .filter_map(|line| {
            let line = line.trim_end_matches('\r');
            line.strip_prefix(&needle).map(|p| p.to_string())
        })
        .collect::<Vec<_>>()
        .into_iter()
        .next()
        .filter(|_| {
            // A path is observed only when exactly one such line exists.
            evidence.matches(&needle).count() == 1
        })
}

/// Extract the observed target id from a completed execution's evidence.
pub fn observed_target_id(
    evidence: &str,
    marker: &str,
    exit_code: i32,
    execution_completed: bool,
) -> Result<String, String> {
    if execution_completed
        && exit_code != 0
        && evidence.to_lowercase().contains("permission denied")
    {
        return Err("Target directory permission denied.".to_string());
    }
    if !execution_completed || exit_code != 0 {
        return Err("Target identity command failed or is unverified; inspect the terminal. Root permissions may be required.".to_string());
    }
    let needle = format!("{marker}:");
    let mut ids: Vec<&str> = evidence
        .split('\n')
        .filter_map(|line| {
            let line = line.trim_end_matches('\r');
            line.strip_prefix(&needle)
        })
        .collect();
    if ids.len() != 1 || !is_uuid(ids[0]) {
        return Err(
            "Invalid or missing target UUID. Existing file was not automatically overwritten."
                .to_string(),
        );
    }
    Ok(ids.remove(0).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_valid_uuids() {
        assert!(is_uuid("12345678-1234-1234-1234-123456789abc"));
        assert!(!is_uuid("not-a-uuid"));
        assert!(!is_uuid("12345678-1234-1234-1234-123456789ab"));
        assert!(!is_uuid("12345678-1234-1234-1234-123456789abg"));
    }

    #[test]
    fn parses_binding_replies() {
        assert_eq!(binding_reply("OK target=none").unwrap(), None);
        let id = "12345678-1234-1234-1234-123456789abc";
        assert_eq!(
            binding_reply(&format!("OK target={id}")).unwrap(),
            Some(id.to_string())
        );
        assert!(binding_reply("ERR").is_err());
        assert!(binding_reply("OK target=bad").is_err());
    }

    #[test]
    fn identity_command_is_well_formed() {
        let id = "12345678-1234-1234-1234-123456789abc";
        let cmd = target_identity_command("bind", id, "LINKR_ID_deadbeef").unwrap();
        assert!(cmd.contains(TARGET_ID_PATH));
        assert!(cmd.contains(id));
        assert!(cmd.contains("LINKR_ID_deadbeef"));
        assert!(target_identity_command("nonsense", id, "LINKR_ID_ab").is_err());
        assert!(target_identity_command("bind", "bad", "LINKR_ID_ab").is_err());
        assert!(target_identity_command("bind", id, "bad-marker").is_err());
    }

    #[test]
    fn extracts_observed_path_and_id() {
        let id = "12345678-1234-1234-1234-123456789abc";
        let evidence = format!("LINKR_ID_dead:{id}\nLINKR_ID_dead_PATH:/var/lib/linkr/device-id\n");
        assert_eq!(
            observed_target_path(&evidence, "LINKR_ID_dead").as_deref(),
            Some("/var/lib/linkr/device-id")
        );
        assert_eq!(
            observed_target_id(&evidence, "LINKR_ID_dead", 0, true).unwrap(),
            id
        );
    }

    #[test]
    fn a_failed_identity_command_is_unverified() {
        assert!(observed_target_id("", "LINKR_ID_dead", 1, true).is_err());
        assert!(observed_target_id("", "LINKR_ID_dead", 0, false).is_err());
    }
}
