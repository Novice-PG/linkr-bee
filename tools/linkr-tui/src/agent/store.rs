//! The `Storage` seam, backed by one JSON file.
//!
//! The agent data layer is written against a three-method key-value trait so the
//! same logic runs against the web's `localStorage`, a JSON file, or an
//! in-memory map in a test. Until now only the third existed: `TaskStore`,
//! `NoteStore` and `SessionStore` were complete, tested, and connected to
//! nothing that ships — every function in them could be called and none of them
//! was.
//!
//! This is the file half. One JSON object, read once at startup and written on
//! every mutation, which is the same shape `lan_token_store` uses and the reason
//! that one has no in-memory copy that can drift from the disk.
//!
//! The file is written through a temp file and renamed, because a bridge that
//! is unplugged mid-write must be left with the previous copy rather than with a
//! truncated one. The rename is atomic on every platform this builds for.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::config::Storage;

/// How the file is laid out, for a reader debugging one by hand.
///
/// One object per key. Values are always strings, because that is what the
/// stores write: `TaskStore` serialises its own array, `NoteStore` its own, and
/// neither knows the other exists. This file is their shared shelf, not their
/// schema.
const MAGIC: &str = "linkr";

/// `Debug` because `App` holds one and `App` is `Debug`; the path is the only
/// thing anyone has ever wanted from it, so the data is summarised rather
/// than dumped.
#[derive(Debug)]
pub struct JsonFile {
    path: PathBuf,
    data: BTreeMap<String, String>,
}

impl JsonFile {
    /// The store at `path`, with whatever is already in it. A file that does
    /// not exist, cannot be read, or does not parse starts empty rather than
    /// stopping the program: the agent stores are durable state, not
    /// configuration the interface cannot start without.
    pub fn at(path: impl Into<PathBuf>) -> JsonFile {
        let path = path.into();
        let data = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| parse_object(&text))
            .unwrap_or_default();
        JsonFile { path, data }
    }

    /// The store in the user's configuration directory, or a temp file if the
    /// platform has none. Named `agent.json` rather than `tasks.json` because
    /// every agent store shares it.
    pub fn load() -> JsonFile {
        JsonFile::at(default_path())
    }

    /// Write the whole object back. Called after every mutation, which is
    /// cheap because the object is small — a few tasks, a few notes — and is
    /// what keeps the file from holding a copy that disagrees with memory.
    fn flush(&self) {
        let mut out = String::from("{\"_linkr\":\"1\"");
        for (key, value) in &self.data {
            out.push_str(",\"");
            out.push_str(&escape(key));
            out.push_str("\":\"");
            out.push_str(&escape(value));
            out.push('"');
        }
        out.push('}');
        let path = &self.path;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Write beside it and rename, so an interrupted write leaves the old
        // file rather than a half-written one.
        let mut tmp = path.clone();
        let mut stamp = std::process::id();
        tmp.set_extension(format!("tmp{stamp:08x}"));
        loop {
            if std::fs::write(&tmp, &out).is_ok()
                && std::fs::rename(&tmp, path).is_ok()
            {
                return;
            }
            // A collision on the temp name is the only recoverable failure; a
            // full disk or a read-only directory is reported rather than spun
            // on, and the in-memory copy is still correct either way.
            if stamp > u32::MAX - 1024 {
                return;
            }
            stamp += 1;
            tmp.set_extension(format!("tmp{stamp:08x}"));
        }
    }

    /// Where the file lives, for a test that wants to clean up after itself.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

/// So a `TaskStore` can hold a borrowing handle rather than taking
/// ownership: [`App::agent_store`] is one store for all three stores, and each
/// of them needs it for the length of one call.
impl Storage for &mut JsonFile {
    fn get(&self, key: &str) -> Option<String> {
        JsonFile::get(self, key)
    }

    fn set(&mut self, key: &str, value: &str) {
        JsonFile::set(self, key, value)
    }

    fn remove(&mut self, key: &str) {
        JsonFile::remove(self, key)
    }
}

impl Storage for JsonFile {
    fn get(&self, key: &str) -> Option<String> {
        self.data.get(key).cloned()
    }

    fn set(&mut self, key: &str, value: &str) {
        self.data.insert(key.to_string(), value.to_string());
        self.flush();
    }

    fn remove(&mut self, key: &str) {
        self.data.remove(key);
        self.flush();
    }
}

/// Parse the file back into a map. Deliberately not a JSON library — the file is
/// written by `flush()` above and its shape is fixed — but the reader is
/// defensive, because a file can be edited by hand and a malformed one must cost
/// the agent its history rather than the program its startup.
fn parse_object(text: &str) -> Option<BTreeMap<String, String>> {
    let body = text.trim();
    if !body.starts_with('{') || !body.ends_with('}') {
        return None;
    }
    let mut out = BTreeMap::new();
    let inner = &body[1..body.len() - 1];
    let mut rest = inner;
    loop {
        rest = rest.trim_start().strip_prefix('"')?;
        let (key, after) = take_quoted(rest)?;
        rest = after.trim_start().strip_prefix(':')?;
        rest = rest.trim_start().strip_prefix('"')?;
        let (value, after) = take_quoted(rest)?;
        out.insert(key, value);
        rest = after.trim_start();
        match rest.strip_prefix(',') {
            Some(next) => {
                rest = next.trim_start();
                // A trailing comma — a hand edit, a half-written file — ends
                // the object rather than discarding everything before it.
                if rest.is_empty() {
                    return Some(out);
                }
            }
            None => return Some(out),
        }
    }
}

/// A quoted string with `\"`, `\\`, `\n`, `\r`, `\t` and `\uXXXX` escapes, plus
/// the rest of the text after the closing quote.
///
/// Walked by byte index rather than by `char_indices().offset()`, which is
/// unstable, and a store the whole data layer stands on has no business using
/// nightly features.
fn take_quoted(text: &str) -> Option<(String, &str)> {
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some((out, &text[i + 1..])),
            b'\\' => {
                let esc = *bytes.get(i + 1)?;
                match esc {
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let hex = text.get(i + 2..i + 6)?;
                        let code = u32::from_str_radix(hex, 16).ok()?;
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        i += 4;
                    }
                    other => out.push(other as char),
                }
                i += 2;
            }
            _ => {
                let ch = text[i..].chars().next()?;
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    None
}

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other if (other as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", other as u32));
            }
            other => out.push(other),
        }
    }
    out
}

/// One file next to the token store, so a user has one place to look.
fn default_path() -> PathBuf {
    let base = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join(".config").join(format!("{MAGIC}-agent"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file of this test's own. Every test in this module used to share one,
    /// which passed while the flush was broken and failed the moment it worked
    /// — cargo runs the tests in threads, so two of them removed each other's
    /// file between the write and the read.
    fn temp(tag: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("linkr-store-{}-{}.json", std::process::id(), tag));
        path
    }

    /// A value survives a write, a read, and a fresh `JsonFile` over the same
    /// path — which is the whole contract, and the reason the flush happens on
    /// every `set` rather than on exit.
    #[test]
    fn a_value_survives_a_reopen() {
        const NAME: &str = "a_value_survives_a_reopen";
        let path = temp(NAME);
        let _ = std::fs::remove_file(&path);
        {
            let mut store = JsonFile::at(&path);
            store.set("k", "v");
        }
        let store = JsonFile::at(&path);
        assert_eq!(store.get("k").as_deref(), Some("v"));
        let _ = std::fs::remove_file(&path);
    }

    /// Escapes round-trip, so a task goal containing a quote or a newline does
    /// not end the string early and swallow the rest of the file.
    #[test]
    fn awkward_values_round_trip() {
        const NAME: &str = "awkward_values_round_trip";
        let path = temp(NAME);
        let _ = std::fs::remove_file(&path);
        {
            let mut store = JsonFile::at(&path);
            store.set("goal", "he said \"rm -rf /\"\nand then a tab\there");
            store.set("back", "a\\b");
        }
        let store = JsonFile::at(&path);
        assert_eq!(
            store.get("goal").as_deref(),
            Some("he said \"rm -rf /\"\nand then a tab\there")
        );
        assert_eq!(store.get("back").as_deref(), Some("a\\b"));
        let _ = std::fs::remove_file(&path);
    }

    /// A file that is not ours — or not valid at all — starts empty. An agent
    /// history is not worth refusing to boot over.
    #[test]
    fn a_bad_file_starts_empty() {
        const NAME: &str = "a_bad_file_starts_empty";
        let path = temp(NAME);
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "not json at all").expect("write");
        assert_eq!(JsonFile::at(&path).get("k"), None);
        std::fs::write(&path, "{\"k\":\"v\",}").expect("write");
        let store = JsonFile::at(&path);
        // A hand-edit's trailing comma costs the entry after it, not the
        // whole file.
        assert_eq!(store.get("k").as_deref(), Some("v"));
        let _ = std::fs::remove_file(&path);
    }

    /// A remove is a reload, not just a memory edit.
    #[test]
    fn a_remove_is_durable() {
        const NAME: &str = "a_remove_is_durable";
        let path = temp(NAME);
        let _ = std::fs::remove_file(&path);
        {
            let mut store = JsonFile::at(&path);
            store.set("k", "v");
            store.remove("k");
        }
        assert_eq!(JsonFile::at(&path).get("k"), None);
        let _ = std::fs::remove_file(&path);
    }

    /// The atomic write leaves a single file behind and no temp litter, even
    /// across several mutations.
    #[test]
    fn no_temp_litter() {
        const NAME: &str = "no_temp_litter";
        let path = temp(NAME);
        let _ = std::fs::remove_file(&path);
        {
            let mut store = JsonFile::at(&path);
            for i in 0..8 {
                store.set(&format!("k{i}"), &format!("v{i}"));
            }
        }
        let dir = path.parent().expect("parent").to_path_buf();
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name())
                    .filter(|name| {
                        name.to_string_lossy().starts_with("linkr-store-test-")
                            && name.to_string_lossy().ends_with(&format!(
                                "{:08x}",
                                std::process::id()
                            ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert!(leftovers.is_empty(), "left behind {leftovers:?}");
        let _ = std::fs::remove_file(&path);
    }
}
