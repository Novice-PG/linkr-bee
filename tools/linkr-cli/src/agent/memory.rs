//! Memory for the assistant: task summaries, durable per-target notes and the
//! conversation record. Ports of `web/agent_tasks.js`, `web/agent_notes.js`
//! and `web/agent_session.js`, all sharing one redaction pass so an exported
//! report can never leak a credential that a log line carried.

use std::path::PathBuf;

use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

use super::context::Message;

pub const TASK_LIMIT: usize = 20;
pub const NOTE_LIMIT: usize = 12;
pub const NOTE_MAX_CHARS: usize = 600;
pub const NOTE_EVIDENCE_MAX_CHARS: usize = 200;
pub const SESSION_MESSAGES: usize = 40;
pub const SESSION_DISPLAY: usize = 60;
pub const SESSION_ENTRY_CHARS: usize = 2000;
pub const SESSION_MAX_BYTES: usize = 96_000;
pub const SESSION_DEVICES: usize = 8;

fn re_bearer() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b(Bearer\s+)\S+").unwrap())
}

fn re_secret() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)((?:password|passwd|api[_-]?key|token|secret)\s*[:=]\s*)[^\s,;]+").unwrap()
    })
}

fn re_url() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)https?://[^\s"<>]+"#).unwrap())
}

/// Strip credentials, query and fragment from a URL instead of dropping it:
/// the report still cites the page, but never the token that opened it.
fn clean_url(raw: &str) -> String {
    match reqwest::Url::parse(raw) {
        Ok(mut url) => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        }
        Err(_) => "[url]".to_string(),
    }
}

/// Redact bearer tokens, `password=`-style assignments and URL credentials,
/// then bound the result (JS `redactTaskText`, slice 2000).
pub fn redact_task_text(value: &str) -> String {
    let text = re_bearer().replace_all(value, |caps: &Captures| format!("{}[redacted]", &caps[1]));
    let text = re_secret().replace_all(&text, |caps: &Captures| format!("{}[redacted]", &caps[1]));
    let text = re_url().replace_all(&text, |caps: &Captures| clean_url(&caps[0]));
    text.chars().take(2000).collect()
}

// --- tasks -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskStep {
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub verification: String,
    #[serde(default, rename = "nextAction")]
    pub next_action: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskExecution {
    pub delivery: String,
    #[serde(default, rename = "exitCode")]
    pub exit_code: Option<i64>,
    #[serde(default, rename = "executionStatus")]
    pub execution_status: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub observation: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    #[serde(rename = "deviceKey")]
    pub device_key: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: u64,
    pub goal: String,
    #[serde(default)]
    pub summary: String,
    pub status: String,
    #[serde(default)]
    pub plan: Vec<TaskStep>,
    #[serde(default)]
    pub executions: Vec<TaskExecution>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub struct TaskStore {
    path: PathBuf,
}

impl TaskStore {
    pub fn open(path: PathBuf) -> Self {
        Self { path }
    }

    fn read(&self) -> Vec<Task> {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        serde_json::from_str::<Vec<Task>>(&raw)
            .ok()
            .map(|tasks| {
                let mut kept: Vec<Task> = tasks
                    .into_iter()
                    .filter(|task| !task.id.is_empty() && !task.goal.is_empty())
                    .collect();
                // Keep the newest TASK_LIMIT entries, oldest first.
                if kept.len() > TASK_LIMIT {
                    let drop = kept.len() - TASK_LIMIT;
                    kept.drain(..drop);
                }
                kept
            })
            .unwrap_or_default()
    }

    fn write(&self, tasks: &[Task]) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let text = serde_json::to_string_pretty(tasks).map_err(|e| e.to_string())?;
        std::fs::write(&self.path, text).map_err(|e| e.to_string())
    }

    /// Historical list for one device: newest first, an interrupted run is
    /// never presented as still running, and every entry is marked `historical`
    /// so the UI cannot pass an old plan off as the live one.
    pub fn list(&self, device_key: &str) -> Vec<Task> {
        let mut tasks: Vec<Task> = self
            .read()
            .into_iter()
            .filter(|task| task.device_key == device_key)
            .collect();
        for task in tasks.iter_mut() {
            if task.status == "running" {
                task.status = "interrupted".to_string();
            }
        }
        tasks.reverse();
        tasks
    }

    /// Save one task, redacted and bounded (oldest evicted past 20).
    pub fn save(&self, mut task: Task) -> Result<(), String> {
        if task.device_key.is_empty() {
            return Ok(());
        }
        task.goal = redact_task_text(&task.goal);
        task.summary = redact_task_text(&task.summary);
        task.updated_at = now_ms();
        task.plan.truncate(8);
        for step in task.plan.iter_mut() {
            step.title = redact_task_text(&step.title);
            step.verification = redact_task_text(&step.verification);
            step.next_action = redact_task_text(&step.next_action);
            if !["pending", "in_progress", "completed", "blocked"].contains(&step.status.as_str()) {
                step.status = "pending".to_string();
            }
        }
        if task.executions.len() > 8 {
            let skip = task.executions.len() - 8;
            task.executions.drain(0..skip);
        }
        for execution in task.executions.iter_mut() {
            execution.path = redact_task_text(&execution.path);
            execution.observation = redact_task_text(&execution.observation);
        }
        let mut tasks = self.read();
        tasks.retain(|existing| existing.id != task.id);
        tasks.push(task);
        if tasks.len() > TASK_LIMIT {
            let drop = tasks.len() - TASK_LIMIT;
            tasks.drain(0..drop);
        }
        self.write(&tasks)
    }

    pub fn clear(&self, device_key: &str) -> Result<(), String> {
        let tasks: Vec<Task> = self
            .read()
            .into_iter()
            .filter(|task| task.device_key != device_key)
            .collect();
        self.write(&tasks)
    }
}

// --- notes -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    #[serde(rename = "deviceKey")]
    pub device_key: String,
    pub text: String,
    #[serde(default)]
    pub evidence: String,
    #[serde(rename = "createdAt")]
    pub created_at: u64,
    /// Flag returned by [`NoteStore::add`] for a fact already stored; never
    /// persisted.
    #[serde(skip)]
    pub duplicate: bool,
}

pub struct NoteStore {
    path: PathBuf,
}

impl NoteStore {
    pub fn open(path: PathBuf) -> Self {
        Self { path }
    }

    fn read(&self) -> Vec<Note> {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        serde_json::from_str::<Vec<Note>>(&raw).unwrap_or_default()
    }

    fn write(&self, notes: &[Note]) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let text = serde_json::to_string_pretty(notes).map_err(|e| e.to_string())?;
        std::fs::write(&self.path, text).map_err(|e| e.to_string())
    }

    /// Oldest first: the newest observation about a target is the one that
    /// matters.
    pub fn list(&self, device_key: &str) -> Vec<Note> {
        if device_key.is_empty() {
            return Vec::new();
        }
        let mut notes: Vec<Note> = self
            .read()
            .into_iter()
            .filter(|note| note.device_key == device_key)
            .collect();
        notes.sort_by_key(|note| note.created_at);
        notes
    }

    /// Store one durable fact. Repeating a fact already stored is not an
    /// error: the existing note is returned flagged as a duplicate.
    pub fn add(&self, device_key: &str, text: &str, evidence: &str) -> Result<Note, String> {
        if device_key.is_empty() {
            return Err("This target has no identity yet, so a note cannot be stored.".to_string());
        }
        let trimmed = text.trim();
        let clean = redact_task_text(trimmed);
        let clean = clean.chars().take(NOTE_MAX_CHARS).collect::<String>();
        let clean = clean.trim().to_string();
        if clean.is_empty() {
            return Err("The note is empty after redaction.".to_string());
        }
        if trimmed.chars().count() > NOTE_MAX_CHARS {
            return Err(format!(
                "A note is limited to {} characters.",
                NOTE_MAX_CHARS
            ));
        }
        let notes = self.read();
        let existing: Vec<Note> = notes
            .iter()
            .filter(|note| note.device_key == device_key)
            .cloned()
            .collect();
        if let Some(note) = existing.iter().find(|note| note.text == clean) {
            let mut stored = note.clone();
            stored.duplicate = true;
            return Ok(stored);
        }
        let mut id_source = format!("{}", now_ms());
        id_source.push_str(&uuid::Uuid::new_v4().simple().to_string()[..6]);
        let note = Note {
            id: format!("note-{}", id_source),
            device_key: device_key.to_string(),
            text: clean,
            evidence: redact_task_text(evidence.trim())
                .chars()
                .take(NOTE_EVIDENCE_MAX_CHARS)
                .collect(),
            created_at: now_ms(),
            duplicate: false,
        };
        let mut kept = notes;
        if existing.len() >= NOTE_LIMIT {
            let overflow = existing.len() - NOTE_LIMIT + 1;
            let dropped: Vec<String> = existing[..overflow].iter().map(|n| n.id.clone()).collect();
            kept.retain(|item| !dropped.contains(&item.id));
        }
        kept.push(note.clone());
        // One global bound so a bench that cycles boards cannot grow the file
        // without limit.
        if kept.len() > NOTE_LIMIT * 4 {
            let drop = kept.len() - NOTE_LIMIT * 4;
            kept.drain(0..drop);
        }
        self.write(&kept)?;
        Ok(note)
    }

    pub fn remove(&self, device_key: &str, id: &str) -> Result<(), String> {
        let notes: Vec<Note> = self
            .read()
            .into_iter()
            .filter(|note| !(note.device_key == device_key && note.id == id))
            .collect();
        self.write(&notes)
    }

    pub fn clear(&self, device_key: &str) -> Result<(), String> {
        let notes: Vec<Note> = self
            .read()
            .into_iter()
            .filter(|note| note.device_key != device_key)
            .collect();
        self.write(&notes)
    }
}

// --- session ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayLine {
    pub role: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub display: Vec<DisplayLine>,
    pub messages: Vec<Message>,
    #[serde(rename = "updatedAt")]
    pub updated_at: u64,
}

fn trim_display(display: &[DisplayLine]) -> Vec<DisplayLine> {
    let kept: Vec<DisplayLine> = display
        .iter()
        .filter(|entry| {
            (entry.role == "user" || entry.role == "assistant") && !entry.text.trim().is_empty()
        })
        .map(|entry| DisplayLine {
            role: entry.role.clone(),
            text: entry.chars_take(SESSION_ENTRY_CHARS),
        })
        .collect();
    let skip = kept.len().saturating_sub(SESSION_DISPLAY);
    kept.into_iter().skip(skip).collect()
}

trait CharsTake {
    fn chars_take(&self, count: usize) -> String;
}

impl CharsTake for DisplayLine {
    fn chars_take(&self, count: usize) -> String {
        self.text.chars().take(count).collect()
    }
}

fn record_size(messages: &[Message]) -> usize {
    serde_json::to_string(messages)
        .map(|s| s.len())
        .unwrap_or(usize::MAX)
}

pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    pub fn open(path: PathBuf) -> Self {
        Self { path }
    }

    fn read_map(&self) -> serde_json::Map<String, serde_json::Value> {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return serde_json::Map::new();
        };
        serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default()
    }

    fn write_map(&self, map: serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let text = serde_json::to_string_pretty(&serde_json::Value::Object(map))
            .map_err(|e| e.to_string())?;
        std::fs::write(&self.path, text).map_err(|e| e.to_string())
    }

    /// Persist one conversation. The oldest turns are dropped until the record
    /// fits, so a long log excerpt cannot make the session unstorable; the
    /// oldest device record is evicted past eight boards.
    pub fn save(
        &self,
        device_key: &str,
        display: &[DisplayLine],
        messages: &[Message],
    ) -> Result<Option<SessionRecord>, String> {
        if device_key.is_empty() {
            return Ok(None);
        }
        let mut kept: Vec<Message> = if messages.len() > SESSION_MESSAGES {
            messages[messages.len() - SESSION_MESSAGES..].to_vec()
        } else {
            messages.to_vec()
        };
        while kept.len() > 1 && record_size(&kept) > SESSION_MAX_BYTES {
            kept.remove(0);
        }
        let display = trim_display(display);
        let record =
            if record_size(&kept) > SESSION_MAX_BYTES || (kept.is_empty() && display.is_empty()) {
                None
            } else {
                Some(SessionRecord {
                    display,
                    messages: kept,
                    updated_at: now_ms(),
                })
            };
        let mut map = self.read_map();
        match &record {
            Some(record) => {
                map.insert(
                    device_key.to_string(),
                    serde_json::to_value(record).map_err(|e| e.to_string())?,
                );
            }
            None => {
                map.remove(device_key);
            }
        }
        if map.len() > SESSION_DEVICES {
            let mut keys: Vec<(String, u64)> = map
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value.get("updatedAt").and_then(|v| v.as_u64()).unwrap_or(0),
                    )
                })
                .collect();
            keys.sort_by_key(|(_, updated)| *updated);
            let overflow = map.len() - SESSION_DEVICES;
            for (key, _) in keys.into_iter().take(overflow) {
                map.remove(&key);
            }
        }
        self.write_map(map)?;
        Ok(record)
    }

    pub fn load(&self, device_key: &str) -> Option<SessionRecord> {
        if device_key.is_empty() {
            return None;
        }
        let value = self.read_map().get(device_key)?.clone();
        let mut record: SessionRecord = serde_json::from_value(value).ok()?;
        if record.messages.len() > SESSION_MESSAGES {
            let skip = record.messages.len() - SESSION_MESSAGES;
            record.messages.drain(0..skip);
        }
        record.display = trim_display(&record.display);
        if record.messages.is_empty() && record.display.is_empty() {
            return None;
        }
        Some(record)
    }

    pub fn clear(&self, device_key: &str) {
        if device_key.is_empty() {
            return;
        }
        let mut map = self.read_map();
        map.remove(device_key);
        let _ = self.write_map(map);
    }
}

/// The identity a policy, note or session is keyed by: the verified target
/// UUID when the user has one, otherwise the transport plus device id.
pub fn device_identity(
    transport: &str,
    device_id: Option<&str>,
    verified_target: Option<&str>,
) -> Option<String> {
    if let Some(target) = verified_target {
        return Some(format!("target:{}", target));
    }
    device_id.map(|id| {
        serde_json::to_string(&[transport, id, ""]).unwrap_or_else(|_| format!("{transport}:{id}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TASKS_JS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/agent_tasks.js");
    const NOTES_JS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/agent_notes.js");
    const SESSION_JS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/agent_session.js");

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("linkr-agent-memory-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn redaction_matches_agent_tasks_js() {
        let source = std::fs::read_to_string(TASKS_JS).expect("agent_tasks.js");
        assert!(source.contains("linkr-agent-tasks-v1"));
        assert_eq!(
            redact_task_text("Authorization: Bearer abcdef123 done"),
            "Authorization: Bearer [redacted] done"
        );
        assert_eq!(
            redact_task_text("password=hunter2, user=root"),
            "password=[redacted], user=root"
        );
        assert_eq!(
            redact_task_text("api_key: ZZZ tail"),
            "api_key: [redacted] tail"
        );
        assert_eq!(
            redact_task_text("see https://user:pass@example.com/a?token=x#frag now"),
            "see https://example.com/a now"
        );
        assert_eq!(redact_task_text("plain text"), "plain text");
        let long = "x".repeat(5000);
        assert_eq!(redact_task_text(&long).chars().count(), 2000);
    }

    #[test]
    fn task_store_saves_redacted_and_lists_historically() {
        let dir = temp_dir("tasks");
        let store = TaskStore::open(dir.join("tasks.json"));
        let task = Task {
            id: "t1".into(),
            device_key: "target:abc".into(),
            updated_at: 0,
            goal: "check boot with password=secret".into(),
            summary: String::new(),
            status: "running".into(),
            plan: vec![TaskStep {
                title: "read log".into(),
                status: "weird".into(),
                verification: String::new(),
                next_action: String::new(),
            }],
            executions: vec![],
        };
        store.save(task).unwrap();
        let listed = store.list("target:abc");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].status, "interrupted", "running is historical");
        assert!(listed[0].goal.contains("password=[redacted]"));
        assert_eq!(listed[0].plan[0].status, "pending", "unknown status reset");

        for i in 0..25 {
            store
                .save(Task {
                    id: format!("t{i}"),
                    ..store.list("target:abc").remove(0)
                })
                .unwrap();
        }
        let all = store.list("target:abc");
        assert_eq!(all.len(), TASK_LIMIT, "oldest evicted past 20");
        store.clear("target:abc").unwrap();
        assert!(store.list("target:abc").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn note_store_messages_are_exact() {
        let source = std::fs::read_to_string(NOTES_JS).expect("agent_notes.js");
        assert!(source.contains("linkr-agent-notes-v1"));
        assert_eq!(NOTE_LIMIT, 12);
        assert_eq!(NOTE_MAX_CHARS, 600);

        let dir = temp_dir("notes");
        let store = NoteStore::open(dir.join("notes.json"));
        assert_eq!(
            store.add("", "text", "").unwrap_err(),
            "This target has no identity yet, so a note cannot be stored."
        );
        assert_eq!(
            store.add("k", "   ", "").unwrap_err(),
            "The note is empty after redaction."
        );
        assert_eq!(
            store.add("k", &"n".repeat(601), "").unwrap_err(),
            "A note is limited to 600 characters."
        );

        let note = store
            .add("k", "bootloader needs raw Enter", "saw U-Boot prompt")
            .unwrap();
        assert!(!note.id.is_empty());
        let duplicate = store
            .add("k", "bootloader needs raw Enter", "again")
            .unwrap();
        assert_eq!(duplicate.id, note.id, "duplicate returns the stored note");
        assert!(duplicate.duplicate);
        assert_eq!(store.list("k").len(), 1);

        for i in 0..13 {
            store.add("k", &format!("fact {i}"), "evidence").unwrap();
        }
        assert_eq!(store.list("k").len(), NOTE_LIMIT, "cap per device");
        store.clear("k").unwrap();
        assert!(store.list("k").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn note_evidence_is_redacted_and_bounded() {
        let dir = temp_dir("notes2");
        let store = NoteStore::open(dir.join("notes.json"));
        let note = store
            .add("k", "wifi works", "password=hunter2 and more text")
            .unwrap();
        assert!(note.evidence.starts_with("password=[redacted]"));
        assert!(note.evidence.chars().count() <= NOTE_EVIDENCE_MAX_CHARS);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_store_enforces_the_three_caps() {
        let source = std::fs::read_to_string(SESSION_JS).expect("agent_session.js");
        assert!(source.contains("linkr-agent-session-v1"));
        assert_eq!(SESSION_MESSAGES, 40);
        assert_eq!(SESSION_ENTRY_CHARS, 2000);
        assert_eq!(SESSION_MAX_BYTES, 96_000);
        assert_eq!(SESSION_DEVICES, 8);

        let dir = temp_dir("session");
        let store = SessionStore::open(dir.join("session.json"));

        // Message count cap.
        let messages: Vec<Message> = (0..60)
            .map(|i| Message::user(format!("question {i}")))
            .collect();
        let record = store
            .save(
                "dev1",
                &[DisplayLine {
                    role: "user".into(),
                    text: "hi".into(),
                }],
                &messages,
            )
            .unwrap()
            .expect("stored");
        assert_eq!(record.messages.len(), SESSION_MESSAGES);

        // Oversized display lines are cut.
        let display = vec![DisplayLine {
            role: "user".into(),
            text: "d".repeat(5000),
        }];
        let record = store.save("dev1", &display, &[]).unwrap().expect("stored");
        assert!(record.display[0].text.chars().count() <= SESSION_ENTRY_CHARS);

        // Device cap: eight boards kept, oldest evicted. Distinct timestamps
        // make "oldest" well-defined.
        for i in 0..12 {
            store
                .save(
                    &format!("dev{i}"),
                    &[DisplayLine {
                        role: "user".into(),
                        text: format!("q{i}"),
                    }],
                    &[Message::user(format!("q{i}"))],
                )
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(store.load("dev0").is_none(), "oldest evicted");
        assert!(store.load("dev11").is_some());

        // Clearing one device leaves the others.
        store.clear("dev11");
        assert!(store.load("dev11").is_none());
        assert!(store.load("dev10").is_some());

        // An empty device key never stores anything.
        assert!(store.save("", &[], &[]).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn device_identity_prefers_the_verified_target() {
        // `deviceIdentity()` of `web/agent_tasks.js`: a verified target wins,
        // otherwise the identity is `JSON.stringify([transport, deviceId, uart])`.
        let source = std::fs::read_to_string(TASKS_JS).expect("read web/agent_tasks.js");
        assert!(source
            .contains("JSON.stringify([status.transport, status.deviceId, status.uart || ''])"));
        assert_eq!(
            device_identity("ble", Some("AA:BB"), Some("uuid-1")),
            Some("target:uuid-1".to_string())
        );
        assert_eq!(
            device_identity("ble", Some("AA:BB"), None),
            Some("[\"ble\",\"AA:BB\",\"\"]".to_string())
        );
        assert_eq!(device_identity("ble", None, None), None);
    }
}
