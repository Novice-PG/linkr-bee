//! Bilingual interface text (WEB_UX_SPEC section 9: `linkr-lang`, `en` | `zh`).
//!
//! Every string the user *reads* goes through [`strings!`], which defines a
//! constant per message plus an enumerable [`ALL`] table so a test can prove
//! the two languages stay in step. Constants give call sites compile-time
//! checking; `ALL` gives the test a list to walk.
//!
//! What deliberately does **not** go through here: protocol literals the parity
//! suites compare byte for byte — `@w scan`, `@scan result`, exit codes, VT
//! key encodings, capability flags — and identifiers such as action ids.

use serde::{Deserialize, Serialize};

/// One message: `[english, 中文]`.
pub type Entry = [&'static str; 2];

/// The interface language. Persisted in the settings file under the web's own
/// `linkr-lang` key name; the default is English so library behaviour stays
/// deterministic, and a first run with no settings file picks the locale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    En,
    Zh,
}

impl Lang {
    /// The `linkr-lang` value of this language.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Zh => "zh",
        }
    }

    /// Any `ll`, `ll_CC` or `ll-CC` tag: only Chinese selects 中文, everything
    /// else (including `C`, `POSIX` and unset variables) stays English.
    pub fn parse(raw: &str) -> Self {
        let raw = raw.trim();
        let lowered = raw.to_ascii_lowercase();
        if lowered == "zh" || lowered.starts_with("zh-") || lowered.starts_with("zh_") {
            Lang::Zh
        } else {
            Lang::En
        }
    }

    /// The language's own name. Endonyms are conventionally *not* translated
    /// (`English` reads as `English` in an English interface), so this is a
    /// plain match instead of a table entry — as an [`Entry`] it would trip
    /// [`assert_bilingual`].
    pub fn endonym(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Zh => "中文",
        }
    }

    /// What the palette's language action does: walk the two supported
    /// languages.
    pub fn toggled(self) -> Self {
        match self {
            Lang::En => Lang::Zh,
            Lang::Zh => Lang::En,
        }
    }

    /// First-run default, mirroring the web's `navigator.language` fallback:
    /// an explicit `LINKR_LANG` wins, then the POSIX locale variables.
    pub fn from_env() -> Self {
        for key in ["LINKR_LANG", "LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(raw) = std::env::var(key) {
                if !raw.is_empty() {
                    return Self::parse(&raw);
                }
            }
        }
        Lang::En
    }
}

/// `entry` in `lang`.
pub fn t(entry: Entry, lang: Lang) -> &'static str {
    match lang {
        Lang::En => entry[0],
        Lang::Zh => entry[1],
    }
}

/// Substitute the `{}` slots of a translated template, in order.
///
/// `format!` insists on a string *literal* as its template, so a message that
/// has been looked up in a language table cannot be passed to it. This walks
/// the template instead; a slot with no matching argument stays visible as
/// `{}` so a wiring mistake shows up on screen rather than vanishing.
///
/// ```
/// use linkr_cli::tui::i18n::{fill, t, Entry, Lang};
/// const NAME: Entry = ["Device: {}", "设备：{}"];
/// assert_eq!(fill(t(NAME, Lang::Zh), &[&"ttyS0"]), "设备：ttyS0");
/// ```
pub fn fill(template: &str, args: &[&dyn ::std::fmt::Display]) -> String {
    let mut out = String::with_capacity(template.len() + args.len() * 8);
    let mut rest = template;
    let mut next = args.iter();
    while let Some(at) = rest.find("{}") {
        out.push_str(&rest[..at]);
        match next.next() {
            Some(value) => out.push_str(&value.to_string()),
            None => out.push_str("{}"),
        }
        rest = &rest[at + 2..];
    }
    out.push_str(rest);
    out
}

/// [`fill`] with the arguments spelled out, so a call site never writes the
/// `/// `&[&dyn Display]` slice type: `tr(t(DEVICE, lang), value)`.[/// `&[&dyn Display]` slice type: `tr(t(DEVICE, lang), value)`.dyn Display]` slice type: `tr!(t(DEVICE, lang), value)`.
macro_rules! tr {
    ($template:expr $(, $arg:expr)* $(,)?) => {{
        let __slots: &[&dyn ::std::fmt::Display] = &[$(&$arg),*];
        $crate::tui::i18n::fill($template, __slots)
    }};
}

pub(crate) use tr;

/// Define bilingual messages: `NAME => "english", "中文";`.
///
/// Each name becomes a `pub const NAME: Entry` and is appended to [`ALL`],
/// which is what the completeness test walks.
macro_rules! strings {
    ($($name:ident => $en:literal, $zh:literal;)*) => {
        $(
            #[doc = $en]
            pub const $name: crate::tui::i18n::Entry = [$en, $zh];
        )*

        /// Every message, keyed by its constant name. Test-facing.
        pub const ALL: &[(&str, crate::tui::i18n::Entry)] = &[
            $( (stringify!($name), $name), )*
        ];
    };
}

pub(crate) use strings;

/// Invariants of a message table (called by each view's own test): unique
/// constant names, text in both languages, and nothing left untranslated — a
/// copied English string is a missing translation, not a translation.
pub fn assert_bilingual(all: &[(&str, Entry)]) {
    let mut seen = std::collections::HashSet::new();
    for (name, entry) in all {
        assert!(seen.insert(*name), "duplicate message constant {name}");
        assert!(!entry[0].trim().is_empty(), "{name}: empty english text");
        assert!(!entry[1].trim().is_empty(), "{name}: empty chinese text");
        assert_ne!(
            entry[0], entry[1],
            "{name} was never translated: {}",
            entry[0]
        );
    }
}

// --- shared -----------------------------------------------------------------

strings! {
    OK => "OK", "确定";
    CANCEL => "Cancel", "取消";
    YES => "Yes", "是";
    NO => "No", "否";
    SAVE => "Save", "保存";
    CLOSE => "Close", "关闭";
    RETRY => "Retry", "重试";
    DISCONNECTED => "Disconnected", "未连接";
    CONNECTED => "Connected", "已连接";
    CONNECTING => "Connecting…", "连接中…";
    SCROLL_HINT => "PgUp/PgDn pages this pane", "PgUp/PgDn 翻动此窗格";
    MSG_SAVE_SETTINGS => "Could not save settings: {}", "设置保存失败：{}";
    MODE_MANUAL => "Manual", "手动";
    MODE_AUTO => "Auto", "自动";
    MODE_FULL_AUTO => "Full Auto", "全自动";
    MODE_LABEL => "mode: {}", "模式：{}";
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both languages must carry text, and they must differ — a copied English
    /// string is an untranslated message, not a translation.
    #[test]
    fn every_message_is_translated() {
        assert_bilingual(ALL);
        assert!(ALL.len() >= 16, "the shared block alone has 16 messages");
    }

    #[test]
    fn the_language_tag_round_trips() {
        assert_eq!(Lang::En.code(), "en");
        assert_eq!(Lang::Zh.code(), "zh");
        assert_eq!(Lang::parse("zh"), Lang::Zh);
        assert_eq!(Lang::parse("zh-CN"), Lang::Zh);
        assert_eq!(Lang::parse("zh_CN.UTF-8"), Lang::Zh);
        assert_eq!(Lang::parse("en_GB"), Lang::En);
        assert_eq!(Lang::parse("C"), Lang::En);
        assert_eq!(Lang::parse("POSIX"), Lang::En);
        assert_eq!(Lang::parse(""), Lang::En);
        assert_eq!(Lang::parse("  ZH-HANS  "), Lang::Zh);
    }

    /// The palette action's whole behaviour lives here, so it is testable
    /// without a live `App`.
    #[test]
    fn the_language_action_walks_both_languages() {
        assert_eq!(Lang::default().toggled(), Lang::Zh);
        assert_eq!(Lang::Zh.toggled(), Lang::En);
        assert_eq!(Lang::En.toggled().toggled(), Lang::En, "two steps are home");
        assert_eq!(Lang::En.endonym(), "English");
        assert_eq!(Lang::Zh.endonym(), "中文");
    }

    #[test]
    fn t_selects_the_language_of_the_entry() {
        assert_eq!(t(OK, Lang::En), "OK");
        assert_eq!(t(OK, Lang::Zh), "确定");
        assert_eq!(Lang::default(), Lang::En, "the default keeps tests stable");
    }

    /// `format!` cannot take a looked-up template, so the interpolation is a
    /// helper: slots fill left to right, and an unwired slot stays visible.
    #[test]
    fn fill_interpolates_slots_in_order() {
        const TWO: Entry = ["{} then {}", "先 {} 后 {}"];
        assert_eq!(fill(t(TWO, Lang::En), &[&"a", &"b"]), "a then b");
        assert_eq!(fill(t(TWO, Lang::Zh), &[&"a", &"b"]), "先 a 后 b");
        assert_eq!(
            fill(t(TWO, Lang::En), &[&"a"]),
            "a then {}",
            "a missing slot stays visible"
        );
        assert_eq!(fill(t(OK, Lang::En), &[]), "OK", "no slots, no change");
        assert_eq!(tr!(t(TWO, Lang::Zh), "x", "y"), "先 x 后 y");
    }
}
