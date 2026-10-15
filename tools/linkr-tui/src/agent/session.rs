//! Conversation continuity across a reload (`web/agent_session.js`).
//!
//! Only the panel's transcript persists, keyed by device identity: the display
//! lines the user saw, and the model messages the runtime needs to continue.
//! Restored history is history, not evidence — the runtime compacts it like an
//! interrupted run. Storage is bounded three times: messages per device, the
//! serialized size of one record, and how many devices are remembered.

use super::config::Storage;

pub const SESSION_KEY: &str = "linkr-agent-session-v1";
pub const SESSION_MESSAGES: usize = 40;
pub const SESSION_DISPLAY: usize = 60;
pub const SESSION_ENTRY_CHARS: usize = 2000;
pub const SESSION_MAX_BYTES: usize = 96000;
pub const SESSION_DEVICES: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayLine {
    pub role: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub display: Vec<DisplayLine>,
    pub messages: Vec<DisplayLine>,
    pub updated_at: u64,
}

fn trim_display(display: &[DisplayLine]) -> Vec<DisplayLine> {
    display
        .iter()
        .filter(|e| (e.role == "user" || e.role == "assistant") && !e.text.trim().is_empty())
        .map(|e| DisplayLine {
            role: e.role.clone(),
            text: e.text.chars().take(SESSION_ENTRY_CHARS).collect(),
        })
        .rev()
        .take(SESSION_DISPLAY)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn size(messages: &[DisplayLine]) -> usize {
    serialize_messages(messages).len()
}

/// Save a session for a device, bounding messages and dropping the oldest turns
/// until the record fits. Returns the stored session, or `None` when it could
/// not be stored (empty, or too large even after trimming).
pub fn save_session(
    storage: &mut dyn Storage,
    device_key: &str,
    display: &[DisplayLine],
    messages: &[DisplayLine],
    now_ms: u64,
) -> Option<Session> {
    if device_key.is_empty() {
        return None;
    }
    let mut kept: Vec<DisplayLine> = messages
        .iter()
        .rev()
        .take(SESSION_MESSAGES)
        .cloned()
        .collect();
    kept.reverse();
    while kept.len() > 1 && size(&kept) > SESSION_MAX_BYTES {
        kept.remove(0);
    }

    let session = if size(&kept) > SESSION_MAX_BYTES || (kept.is_empty() && display.is_empty()) {
        None
    } else {
        Some(Session {
            display: trim_display(display),
            messages: kept,
            updated_at: now_ms,
        })
    };

    let mut data = load_all(storage);
    match &session {
        Some(s) => {
            data.insert(device_key.to_string(), s.clone());
        }
        None => {
            data.remove(device_key);
        }
    }

    // One record per board seen, oldest first out.
    let mut keys: Vec<(String, u64)> = data
        .iter()
        .map(|(k, v)| (k.clone(), v.updated_at))
        .collect();
    keys.sort_by_key(|(_, ts)| *ts);
    let excess = keys.len().saturating_sub(SESSION_DEVICES);
    for (key, _) in keys.into_iter().take(excess) {
        data.remove(&key);
    }

    save_all(storage, &data);
    session
}

pub fn load_session(storage: &dyn Storage, device_key: &str) -> Option<Session> {
    if device_key.is_empty() {
        return None;
    }
    let data = load_all(storage);
    let session = data.get(device_key)?.clone();
    let messages: Vec<DisplayLine> = session
        .messages
        .into_iter()
        .rev()
        .take(SESSION_MESSAGES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let display = session.display;
    if messages.is_empty() && display.is_empty() {
        return None;
    }
    Some(Session {
        display,
        messages,
        updated_at: session.updated_at,
    })
}

pub fn clear_session(storage: &mut dyn Storage, device_key: &str) {
    if device_key.is_empty() {
        return;
    }
    let mut data = load_all(storage);
    data.remove(device_key);
    save_all(storage, &data);
}

// A fixed-schema JSON codec: a top-level object mapping deviceKey -> session,
// where each session is `{display:[{role,text}], messages:[{role,text}], updatedAt}`.

fn load_all(storage: &dyn Storage) -> std::collections::BTreeMap<String, Session> {
    let raw = match storage.get(SESSION_KEY) {
        Some(r) => r,
        None => return Default::default(),
    };
    parse_sessions(&raw).unwrap_or_default()
}

fn save_all(storage: &mut dyn Storage, data: &std::collections::BTreeMap<String, Session>) {
    storage.set(SESSION_KEY, &serialize_sessions(data));
}

fn serialize_messages(messages: &[DisplayLine]) -> String {
    let items: Vec<String> = messages
        .iter()
        .map(|m| {
            format!(
                "{{\"role\":{},\"text\":{}}}",
                json_string(&m.role),
                json_string(&m.text)
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}

fn serialize_sessions(data: &std::collections::BTreeMap<String, Session>) -> String {
    let items: Vec<String> = data
        .iter()
        .map(|(key, session)| {
            format!(
                "{}:{{\"display\":{},\"messages\":{},\"updatedAt\":{}}}",
                json_string(key),
                serialize_messages(&session.display),
                serialize_messages(&session.messages),
                session.updated_at,
            )
        })
        .collect();
    format!("{{{}}}", items.join(","))
}

fn parse_sessions(raw: &str) -> Option<std::collections::BTreeMap<String, Session>> {
    let raw = raw.trim();
    if !raw.starts_with('{') || !raw.ends_with('}') {
        return None;
    }
    let inner = &raw[1..raw.len() - 1];
    if inner.trim().is_empty() {
        return Some(Default::default());
    }
    let mut map = std::collections::BTreeMap::new();
    for pair in super::tasks::split_top_level(inner, ',') {
        let colon = super::tasks::find_top_level_colon(pair)?;
        let key = parse_json_string_literal(pair[..colon].trim())?;
        let value = parse_session_object(pair[colon + 1..].trim())?;
        map.insert(key, value);
    }
    Some(map)
}

fn parse_session_object(s: &str) -> Option<Session> {
    let s = s.trim();
    if !s.starts_with('{') || !s.ends_with('}') {
        return None;
    }
    let inner = &s[1..s.len() - 1];
    let mut display = Vec::new();
    let mut messages = Vec::new();
    let mut updated_at = 0;
    for pair in super::tasks::split_top_level(inner, ',') {
        let colon = super::tasks::find_top_level_colon(pair)?;
        let key = parse_json_string_literal(pair[..colon].trim())?;
        let value = pair[colon + 1..].trim();
        match key.as_str() {
            "display" => display = parse_messages(value)?,
            "messages" => messages = parse_messages(value)?,
            "updatedAt" => updated_at = value.parse().ok()?,
            _ => {}
        }
    }
    Some(Session {
        display,
        messages,
        updated_at,
    })
}

fn parse_messages(s: &str) -> Option<Vec<DisplayLine>> {
    let arr = super::tasks::parse_json_array(s)?;
    arr.into_iter()
        .map(|obj| {
            Some(DisplayLine {
                role: obj.get("role")?.clone(),
                text: obj.get("text")?.clone(),
            })
        })
        .collect()
}

fn parse_json_string_literal(s: &str) -> Option<String> {
    super::tasks::parse_json_string_literal(s)
}

fn json_string(s: &str) -> String {
    super::tasks::json_string(s)
}

#[cfg(test)]
mod round_trip {
    use super::super::store::JsonFile;
    use super::*;

    /// A conversation survives a reopen, which is the whole point of a session
    /// store: without it the agent is amnesiac between restarts and the thread
    /// on screen has nothing behind it.
    #[test]
    fn a_conversation_survives_a_reopen() {
        let mut path = std::env::temp_dir();
        path.push(format!("linkr-session-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let turns = vec![
            DisplayLine {
                role: "user".into(),
                text: "what is the uptime".into(),
            },
            DisplayLine {
                role: "assistant".into(),
                text: "up 4 hours".into(),
            },
        ];
        {
            let mut store = JsonFile::at(&path);
            let saved = save_session(&mut store, "board", &turns, &turns, 1_000);
            assert!(saved.is_some(), "the session was not saved");
        }
        let loaded = load_session(&JsonFile::at(&path), "board").expect("loaded");
        assert_eq!(loaded.display, turns);
        // A different device does not see it: the store is per target.
        assert!(load_session(&JsonFile::at(&path), "other").is_none());
        let _ = std::fs::remove_file(&path);
    }

    /// An empty store has no session rather than an error.
    #[test]
    fn an_empty_store_has_no_session() {
        let mut path = std::env::temp_dir();
        path.push(format!("linkr-session-empty-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert!(load_session(&JsonFile::at(&path), "board").is_none());
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MemStorage(std::collections::HashMap<String, String>);

    impl Storage for MemStorage {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
        fn set(&mut self, key: &str, value: &str) {
            self.0.insert(key.to_string(), value.to_string());
        }
        fn remove(&mut self, key: &str) {
            self.0.remove(key);
        }
    }

    fn line(role: &str, text: &str) -> DisplayLine {
        DisplayLine {
            role: role.to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn a_session_round_trips() {
        let mut storage = MemStorage::default();
        let display = vec![
            line("user", "check dmesg"),
            line("assistant", "no panic found"),
        ];
        let messages = vec![line("user", "check dmesg")];
        let saved = save_session(&mut storage, "k", &display, &messages, 100).unwrap();
        assert_eq!(saved.display, trim_display(&display));
        let loaded = load_session(&storage, "k").unwrap();
        assert_eq!(loaded.messages, messages);
    }

    #[test]
    fn an_empty_session_is_not_stored() {
        let mut storage = MemStorage::default();
        assert!(save_session(&mut storage, "k", &[], &[], 100).is_none());
        assert!(load_session(&storage, "k").is_none());
    }

    #[test]
    fn display_is_trimmed_to_role_and_chars() {
        let mut storage = MemStorage::default();
        let display = vec![
            line("system", "hidden"),        // wrong role, dropped
            line("user", "  "),              // blank, dropped
            line("user", &"x".repeat(2500)), // capped to 2000
        ];
        save_session(&mut storage, "k", &display, &[line("user", "hi")], 100);
        let loaded = load_session(&storage, "k").unwrap();
        assert_eq!(loaded.display.len(), 1);
        assert_eq!(loaded.display[0].text.chars().count(), SESSION_ENTRY_CHARS);
    }

    #[test]
    fn clear_session_removes_only_that_device() {
        let mut storage = MemStorage::default();
        save_session(
            &mut storage,
            "a",
            &[line("user", "hi")],
            &[line("user", "hi")],
            1,
        );
        save_session(
            &mut storage,
            "b",
            &[line("user", "hi")],
            &[line("user", "hi")],
            1,
        );
        clear_session(&mut storage, "a");
        assert!(load_session(&storage, "a").is_none());
        assert!(load_session(&storage, "b").is_some());
    }
}
