//! Modal dialogs: the keyboard help, the notice log, the confirmation a guarded
//! command asks for before it goes out, and the one-line prompt that a file
//! transfer starts from.
//!
//! A dialog is *state*, not a widget. `App` holds an `Option<Dialog>` and this
//! module owns what each one means, what it says and which keys it answers to;
//! `ui::overlay` paints it. Keeping the two apart is what lets a dialog be
//! tested without a terminal, and it is why the scroll arithmetic below can be
//! asserted directly.
//!
//! The confirmation is the one that matters. `reboot` over a UART is not
//! undoable, and a quick-send row that fired on the first Enter would turn a
//! cursor slip into a rebooted bridge — so the guarded rows go through here,
//! once, with the exact command spelled out in the prompt. The highlighted
//! button starts on *Cancel* for the same reason: a dialog raised by one
//! keystroke must not be answerable by another.
//!
//! The prompt is the same idea with a text field: a transfer cannot be started
//! by a keystroke alone, because *which* file is not a thing a key says. It
//! carries its own labels as [`Entry`] pairs rather than looking them up in the
//! shared table, so a dialog's words stay next to the dialog.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::event::NoticeLevel;
use crate::i18n::{entry, Entry, Lang};
use crate::text::{self, fill};
use crate::theme::Theme;
use crate::views::terminal::wrap_rows;

// --- the overlay's own vocabulary -------------------------------------------
//
// Next to the code that paints it, the way `palette` keeps its own: these are
// the modal layer's words, not the chrome's.

const HELP_TITLE: Entry = [" Keyboard help ", " 键盘帮助 "];
const NOTICES_TITLE: Entry = [" Notice log ", " 通知记录 "];
const CONFIRM_TITLE: Entry = [" Confirm ", " 请确认 "];

const HELP_KEYS: Entry = [
    "↑↓ scroll · PgUp/PgDn page · Home/End · Esc close",
    "↑↓ 滚动 · PgUp/PgDn 翻页 · Home/End · Esc 关闭",
];
const NOTICES_KEYS: Entry = [
    "↑↓ scroll · PgUp/PgDn page · Home/End · Esc close",
    "↑↓ 滚动 · PgUp/PgDn 翻页 · Home/End · Esc 关闭",
];
const CONFIRM_KEYS: Entry = [
    "←→ choose · Enter run the highlighted button · y confirm · n/Esc cancel",
    "←→ 选择 · Enter 执行高亮按钮 · y 确认 · n/Esc 取消",
];

const NOTICES_EMPTY: Entry = ["Nothing has been announced yet.", "暂无通知。"];
const BUTTON_YES: Entry = [" Confirm ", " 确认 "];
const BUTTON_NO: Entry = [" Cancel ", " 取消 "];
const CONFIRM_SEND: Entry = ["Send “{}” to the device?", "将“{}”发送到设备？"];
const CONFIRM_SEND_DETAIL: Entry = [
    "The device runs it at once, and there is no undo.",
    "设备会立即执行，且无法撤销。",
];

// --- the transfer prompt ----------------------------------------------------

const PROMPT_KEYS: Entry = [
    "type the path · ←→ move · Enter start · Esc cancel",
    "输入路径 · ←→ 移动 · Enter 开始 · Esc 取消",
];
const SEND_TITLE: Entry = [" Send a file ", " 发送文件 "];
const SEND_LABEL: Entry = ["Local file to send (ZMODEM)", "要发送的本地文件（ZMODEM）"];
const SEND_PLACEHOLDER: Entry = ["for example firmware.bin", "例如 firmware.bin"];
const RECEIVE_TITLE: Entry = [" Receive files ", " 接收文件 "];
const RECEIVE_LABEL: Entry = ["Directory to receive into (ZMODEM)", "接收目录（ZMODEM）"];
const RECEIVE_PLACEHOLDER: Entry = [
    "for example . — the current directory",
    "例如 . —— 当前目录",
];
// --- the network prompts ----------------------------------------------------

const WIFI_TITLE: Entry = [" Join a WiFi network ", " 加入 WiFi 网络 "];
const WIFI_PASSWORD_TITLE: Entry = [" WiFi password ", " WiFi 密码 "];
const WEBDAV_TITLE: Entry = [" Log upload target ", " 日志上传目标 "];

/// What an empty answer means for a receive. A directory is something the user
/// is standing in; a file is not something to guess at.
const RECEIVE_DEFAULT: &str = ".";

const LEVEL_INFO: Entry = ["info", "信息"];
const LEVEL_WARN: Entry = ["warn", "警告"];
const LEVEL_ERROR: Entry = ["error", "错误"];

const HELP_NOTE: Entry = [
    "Every other key goes to the device: the cursor keys, Home/End, Delete.",
    "其余按键都会发送到设备：方向键、Home/End、Delete。",
];

/// The help overlay's chord list. The keys stay literal — they are what the
/// keyboard produces — and only the meaning is translated.
const HELP_ROWS: &[(&str, Entry)] = &[
    ("Ctrl+P", ["Command palette", "命令面板"]),
    ("F1", ["This help", "本帮助"]),
    ("F2–F6", ["Switch the view", "切换视图"]),
    ("Tab", ["Move the focus", "切换焦点"]),
    ("↑ / ↓", ["Walk the sidebar", "在侧栏中移动"]),
    ("Enter", ["Send the preset / activate", "发送预设 / 激活"]),
    ("PgUp / PgDn", ["Scroll the pane", "滚动终端"]),
    ("End", ["Jump to the newest line", "跳到最新一行"]),
    ("Ctrl+L", ["Clear the pane", "清屏"]),
    ("Ctrl+X", ["Cancel the transfer", "取消传输"]),
    ("Ctrl+T", ["Toggle the theme", "切换主题"]),
    ("Ctrl+Q", ["Quit", "退出"]),
];

/// Columns the help list gives the key before the meaning.
const HELP_KEY_WIDTH: usize = 14;

// --- the dialog state --------------------------------------------------------

/// Which modal is open, and how far it has been scrolled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dialog {
    /// The keyboard help, scrolled to a row.
    Help(usize),
    /// The notice log, scrolled to a row.
    Notices(usize),
    /// A guarded action waiting for a yes.
    Confirm(Confirm),
    /// A one-line answer waiting to be typed.
    Prompt(Prompt),
}

/// A text field: a question, an answer, and what agreeing to it does.
///
/// A prompt rather than a second command line: the interface has no command
/// line, and a transfer needs exactly one argument. Keeping it a dialog means it
/// takes the keyboard the same way the others do, and that the answer is a
/// value the tests can read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// The box's title, in both languages.
    pub title: Entry,
    /// The question, one line above the field.
    pub label: Entry,
    /// What the field shows while it is empty, so an empty box still says what
    /// shape of answer it wants.
    pub placeholder: Entry,
    /// What has been typed.
    pub text: String,
    /// Where the caret is, as a byte offset into `text`. It only ever sits on a
    /// character boundary: every move here shifts it by a whole character.
    pub cursor: usize,
    /// What agreeing to it does.
    pub action: PromptAction,
}

/// What agreeing to a prompt does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptAction {
    /// Offer the named local file to the far end.
    SendFile,
    /// Take whatever the far end offers into the named directory.
    ReceiveInto,
    /// Name the network to join. Answered by a second prompt for its password,
    /// because the two are one command on the wire and a half of that command
    /// is not a thing that can be sent.
    WifiSsid,
    /// The password for the network just named. Held nowhere: it goes straight
    /// from the field into the command and out of the process.
    WifiPassword,
    /// Where the device's log is uploaded to.
    WebdavUrl,
}

impl Prompt {
    /// An empty prompt.
    pub fn new(title: Entry, action: PromptAction) -> Prompt {
        Prompt {
            title,
            label: Self::label(action),
            placeholder: Self::placeholder(action),
            text: String::new(),
            cursor: 0,
            action,
        }
    }

    /// A prompt already holding `text`, with the caret after it.
    ///
    /// Used when the answer is a change to one already given — renaming a
    /// network that is half typed — so that correcting one character does not
    /// mean typing the whole name again.
    pub fn with_text(title: Entry, action: PromptAction, text: &str) -> Prompt {
        let mut prompt = Prompt::new(title, action);
        prompt.text = text.to_string();
        prompt.cursor = prompt.text.len();
        prompt
    }

    /// The question this action's field asks, in the interface language.
    fn label(action: PromptAction) -> Entry {
        match action {
            PromptAction::SendFile => SEND_LABEL,
            PromptAction::ReceiveInto => RECEIVE_LABEL,
            PromptAction::WifiSsid => ["Network to join (SSID)", "要加入的网络名称（SSID）"],
            PromptAction::WifiPassword => ["Its password (may be empty)", "该网络的密码（可留空）"],
            PromptAction::WebdavUrl => [
                "Upload URL (http:// or https://)",
                "上传地址（http:// 或 https://）",
            ],
        }
    }

    /// What the empty field says, which is how it tells you the shape of
    /// answer it wants.
    fn placeholder(action: PromptAction) -> Entry {
        match action {
            PromptAction::SendFile => SEND_PLACEHOLDER,
            PromptAction::ReceiveInto => RECEIVE_PLACEHOLDER,
            PromptAction::WifiSsid => ["for example BenchNet", "例如 BenchNet"],
            PromptAction::WifiPassword => ["empty for an open network", "留空表示开放网络"],
            PromptAction::WebdavUrl => ["for example https://host/dav/", "例如 https://host/dav/"],
        }
    }

    /// Insert `ch` at the caret.
    pub fn insert(&mut self, ch: char) {
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    /// Remove the character before the caret.
    pub fn backspace(&mut self) {
        if let Some(ch) = self.text[..self.cursor].chars().next_back() {
            let at = self.cursor - ch.len_utf8();
            self.text.remove(at);
            self.cursor = at;
        }
    }

    /// Remove the character under the caret.
    pub fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    /// Move the caret one character left, stopping at the start.
    pub fn left(&mut self) {
        if let Some(ch) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= ch.len_utf8();
        }
    }

    /// Move the caret one character right, stopping at the end.
    pub fn right(&mut self) {
        if let Some(ch) = self.text[self.cursor..].chars().next() {
            self.cursor += ch.len_utf8();
        }
    }

    /// Move the caret to the start.
    pub fn home(&mut self) {
        self.cursor = 0;
    }

    /// Move the caret to the end.
    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    /// The field in three pieces: before the caret, the character under it, and
    /// after. The middle is a space at the end of the line, so the caret is
    /// always exactly one cell and the painter never has to special-case it.
    pub fn split(&self) -> (&str, &str, &str) {
        let at = self.cursor.min(self.text.len());
        let (before, rest) = self.text.split_at(at);
        match rest.chars().next() {
            Some(ch) => {
                let (under, after) = rest.split_at(ch.len_utf8());
                (before, under, after)
            }
            None => (before, " ", ""),
        }
    }

    /// The answer, without the whitespace around it. A trailing space at the end
    /// of a path is easy to type and never meant, and a leading one hides.
    pub fn answer(&self) -> &str {
        self.text.trim()
    }
}

impl Dialog {
    /// The row the dialog is scrolled to. Nothing but the help and the log
    /// scrolls; a confirmation and a prompt are the size of their content.
    pub fn scroll(&self) -> usize {
        match self {
            Dialog::Help(offset) | Dialog::Notices(offset) => *offset,
            Dialog::Confirm(_) | Dialog::Prompt(_) => 0,
        }
    }
}

/// The dialog's title, spaces included so the painter passes it on untouched.
pub fn title(dialog: &Dialog, lang: Lang) -> &'static str {
    match dialog {
        Dialog::Help(_) => entry(HELP_TITLE, lang),
        Dialog::Notices(_) => entry(NOTICES_TITLE, lang),
        Dialog::Confirm(_) => entry(CONFIRM_TITLE, lang),
        Dialog::Prompt(prompt) => entry(prompt.title, lang),
    }
}

/// The key legend along the dialog's bottom row.
pub fn legend(dialog: &Dialog, lang: Lang, theme: &Theme) -> Line<'static> {
    let text = match dialog {
        Dialog::Help(_) => entry(HELP_KEYS, lang),
        Dialog::Notices(_) => entry(NOTICES_KEYS, lang),
        Dialog::Confirm(_) => entry(CONFIRM_KEYS, lang),
        Dialog::Prompt(_) => entry(PROMPT_KEYS, lang),
    };
    Line::from(Span::styled(text, theme.muted()))
}

// --- opening -----------------------------------------------------------------

/// Raise the keyboard help.
pub fn open_help(app: &mut App) {
    app.dialog = Some(Dialog::Help(0));
}

/// Raise the notice log.
pub fn open_notices(app: &mut App) {
    app.dialog = Some(Dialog::Notices(0));
}

/// Ask which local file to offer to the device.
pub fn prompt_send_file(app: &mut App) {
    app.dialog = Some(Dialog::Prompt(Prompt::new(
        SEND_TITLE,
        PromptAction::SendFile,
    )));
}

/// Ask which network to join. The password is asked for next, by the prompt
/// this one's answer opens.
pub fn prompt_wifi_ssid(app: &mut App) {
    let prompt = Prompt::with_text(
        WIFI_TITLE,
        PromptAction::WifiSsid,
        &app.network.ssid_input.clone(),
    );
    app.dialog = Some(Dialog::Prompt(prompt));
}

/// Ask where the device's log should be uploaded to.
pub fn prompt_webdav_url(app: &mut App) {
    let prompt = Prompt::with_text(
        WEBDAV_TITLE,
        PromptAction::WebdavUrl,
        &app.network
            .webdav
            .as_ref()
            .map(|d| d.url.clone())
            .unwrap_or_default(),
    );
    app.dialog = Some(Dialog::Prompt(prompt));
}

/// Ask which directory to take the device's files into.
pub fn prompt_receive(app: &mut App) {
    app.dialog = Some(Dialog::Prompt(Prompt::new(
        RECEIVE_TITLE,
        PromptAction::ReceiveInto,
    )));
}

/// A confirmation: what is about to happen, and what agreeing does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    /// The command or action, spelled out so the user confirms *what*, not just
    /// *that*.
    pub prompt: String,
    /// The consequence, in one line.
    pub detail: String,
    /// What agreeing does.
    pub action: ConfirmAction,
    /// Which button is highlighted. See the module note on why this starts off.
    pub yes: bool,
}

/// What agreeing to a confirmation does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    /// Send this line to the device.
    Send(String),
    /// Change how the bridge itself is set up: `@u=…`, `@w=…`, `@d=…`.
    ///
    /// Kept as the command rather than as parsed fields because the command is
    /// what the card shows, what the wire takes, and what the summary words —
    /// and a password parsed into a struct here would be a password with a
    /// longer life than it needs.
    Accessory(String),
}

/// Ask before doing something that cannot be taken back.
pub fn ask(app: &mut App, action: ConfirmAction) {
    let lang = app.lang;
    let (prompt, detail) = match &action {
        ConfirmAction::Send(command) => (
            fill(entry(CONFIRM_SEND, lang), std::slice::from_ref(command)),
            entry(CONFIRM_SEND_DETAIL, lang).to_string(),
        ),
        // Summarised rather than quoted: the command carries the WiFi password,
        // and a card that showed it would put it on the screen in front of
        // whoever is looking. The summary names the network and hides the rest.
        ConfirmAction::Accessory(command) => (
            crate::accessory::accessory_change_summary(command, lang == Lang::Zh),
            app.strings().net_confirm_detail.to_string(),
        ),
    };
    app.dialog = Some(Dialog::Confirm(Confirm {
        prompt,
        detail,
        action,
        yes: false,
    }));
}

// --- the body ----------------------------------------------------------------

/// The glyph and the word standing for a notice's level.
fn level_badge(level: NoticeLevel) -> (&'static str, Entry) {
    match level {
        NoticeLevel::Info => ("·", LEVEL_INFO),
        NoticeLevel::Warn => ("!", LEVEL_WARN),
        NoticeLevel::Error => ("×", LEVEL_ERROR),
    }
}

/// The prompt's field: what has been typed, with a caret on the character the
/// next keystroke will go before.
///
/// The caret is drawn *in* the text rather than placed with the terminal's own
/// cursor. The box is painted by a `Paragraph`, which has no cursor to move, and
/// a caret that only existed in the live TUI would make the dialog unreadable in
/// a `--print-frame` preview — and untestable.
fn prompt_field(prompt: &Prompt, lang: Lang, theme: &Theme) -> Line<'static> {
    let (before, under, after) = prompt.split();
    if prompt.text.is_empty() {
        // Nothing typed: the caret, then what shape of answer the field wants.
        return Line::from(vec![
            Span::styled(format!("  {before}"), theme.text()),
            Span::styled(under.to_string(), theme.selected()),
            Span::styled(entry(prompt.placeholder, lang), theme.muted()),
        ]);
    }
    Line::from(vec![
        Span::styled(format!("  {before}"), theme.text()),
        Span::styled(under.to_string(), theme.selected()),
        Span::styled(after.to_string(), theme.text()),
    ])
}

/// Every row of the dialog's body, before the window is taken.
///
/// `width` only matters to the confirmation, which wraps its prompt; the other
/// two bodies are one row per item at any width, which is what lets the scroll
/// keys work without knowing the box they are scrolling in.
fn body(dialog: &Dialog, app: &App, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let lang = app.lang;
    match dialog {
        Dialog::Help(_) => {
            let mut out: Vec<Line<'static>> = HELP_ROWS
                .iter()
                .map(|(key, meaning)| {
                    Line::from(vec![
                        Span::styled(text::pad_to(key, HELP_KEY_WIDTH), theme.focus()),
                        Span::styled(entry(*meaning, lang), theme.text()),
                    ])
                })
                .collect();
            out.push(Line::default());
            out.push(Line::from(Span::styled(
                entry(HELP_NOTE, lang),
                theme.muted(),
            )));
            out
        }
        Dialog::Notices(_) => {
            if app.log.is_empty() {
                return vec![Line::from(Span::styled(
                    entry(NOTICES_EMPTY, lang),
                    theme.muted(),
                ))];
            }
            // The log is shown newest last, the way it was announced — a log
            // that ran backwards would make "scroll down" mean "go back in
            // time". A long line is cut rather than wrapped, so one notice is
            // always one row and the scroll arithmetic stays exact.
            app.log
                .iter()
                .map(|notice| {
                    let (glyph, word) = level_badge(notice.level);
                    let head = format!("{glyph} {} ", entry(word, lang));
                    Line::from(vec![
                        Span::styled(head.clone(), theme.level_style(notice.level)),
                        Span::styled(
                            text::clip_to(&notice.text, width.saturating_sub(text::columns(&head))),
                            theme.text(),
                        ),
                    ])
                })
                .collect()
        }
        Dialog::Confirm(confirm) => {
            let mut out: Vec<Line<'static>> = wrap_rows(&confirm.prompt, width.max(1))
                .into_iter()
                .map(|row| Line::from(Span::styled(row, theme.text())))
                .collect();
            out.push(Line::default());
            out.extend(
                wrap_rows(&confirm.detail, width.max(1))
                    .into_iter()
                    .map(|row| Line::from(Span::styled(row, theme.muted()))),
            );
            out.push(Line::default());
            // The highlighted button takes the selection chip; the other one
            // keeps the accent ink, so the pair still reads as two buttons.
            let (yes_style, no_style) = if confirm.yes {
                (theme.selected(), theme.focus())
            } else {
                (theme.focus(), theme.selected())
            };
            out.push(Line::from(vec![
                Span::styled(entry(BUTTON_YES, lang), yes_style),
                Span::styled("    ", theme.text()),
                Span::styled(entry(BUTTON_NO, lang), no_style),
            ]));
            out
        }
        Dialog::Prompt(prompt) => vec![
            Line::from(Span::styled(entry(prompt.label, lang), theme.muted())),
            Line::default(),
            prompt_field(prompt, lang, theme),
        ],
    }
}

/// How many rows the body has. Derived from the body itself, so a row added to
/// the help list cannot leave the scroll keys behind.
fn row_count(app: &App, dialog: &Dialog) -> usize {
    // A width no line reaches: every row is one row, which is what the scroll
    // keys need to know. The confirmation is never scrolled, so its wrapping
    // does not matter here.
    body(dialog, app, 10_000, &app.theme()).len()
}

/// How many rows the body needs at `width`.
///
/// The painter asks this before it sizes the box, so a confirmation is as tall
/// as its prompt rather than as tall as the screen — a dialog that filled the
/// frame with blank ground would read as an empty box with a sentence in it.
pub fn body_height(app: &App, dialog: &Dialog, width: u16) -> usize {
    body(dialog, app, width as usize, &app.theme()).len()
}

/// The visible window of the dialog's body, padded to `height`.
pub fn lines(dialog: &Dialog, app: &App, width: u16, height: usize) -> Vec<Line<'static>> {
    if height == 0 {
        return Vec::new();
    }
    let theme = app.theme();
    let all = body(dialog, app, width as usize, &theme);
    // The offset can run past the end — the key handler clamps it to the body
    // rather than to a window it cannot see — so the *window* is what is
    // clamped here, and a scroll left over from a taller terminal still shows
    // the tail rather than blank space.
    let start = dialog.scroll().min(all.len().saturating_sub(height));
    let mut window: Vec<Line<'static>> = all.into_iter().skip(start).take(height).collect();
    while window.len() < height {
        window.push(Line::default());
    }
    window
}

// --- keys --------------------------------------------------------------------

/// Handle a key while a dialog is open. Returns true when the key was consumed.
///
/// A control chord is deliberately *not* consumed: `Ctrl+Q` must still quit and
/// `Ctrl+T` still repaint with a modal in the way, so those fall through to the
/// global handler rather than being swallowed here.
pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return false;
    }
    let Some(dialog) = app.dialog.take() else {
        return false;
    };
    match dialog {
        Dialog::Confirm(mut confirm) => match key.code {
            // Cancel, by the key, by the button, or by Escape.
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {}
            KeyCode::Char('y') | KeyCode::Char('Y') => run(app, confirm.action),
            KeyCode::Enter => {
                if confirm.yes {
                    run(app, confirm.action);
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                confirm.yes = !confirm.yes;
                app.dialog = Some(Dialog::Confirm(confirm));
            }
            _ => app.dialog = Some(Dialog::Confirm(confirm)),
        },
        Dialog::Prompt(mut prompt) => match key.code {
            // Escape gives the keyboard back without starting anything.
            KeyCode::Esc => {}
            KeyCode::Enter => start(app, prompt),
            KeyCode::Backspace => {
                prompt.backspace();
                app.dialog = Some(Dialog::Prompt(prompt));
            }
            KeyCode::Delete => {
                prompt.delete();
                app.dialog = Some(Dialog::Prompt(prompt));
            }
            KeyCode::Left => {
                prompt.left();
                app.dialog = Some(Dialog::Prompt(prompt));
            }
            KeyCode::Right => {
                prompt.right();
                app.dialog = Some(Dialog::Prompt(prompt));
            }
            KeyCode::Home => {
                prompt.home();
                app.dialog = Some(Dialog::Prompt(prompt));
            }
            KeyCode::End => {
                prompt.end();
                app.dialog = Some(Dialog::Prompt(prompt));
            }
            // Tab is a *character* here, not a focus key: a path may want one,
            // where the dialog has nothing else for it to mean.
            KeyCode::Char(c) => {
                prompt.insert(c);
                app.dialog = Some(Dialog::Prompt(prompt));
            }
            KeyCode::Tab => {
                prompt.insert('\t');
                app.dialog = Some(Dialog::Prompt(prompt));
            }
            // Anything else leaves the field exactly as it was. The dialog stays
            // up, so a key with no meaning here is not a key that cancels.
            _ => app.dialog = Some(Dialog::Prompt(prompt)),
        },
        Dialog::Help(offset) => app.dialog = scrolled(app, Dialog::Help(offset), offset, key),
        Dialog::Notices(offset) => app.dialog = scrolled(app, Dialog::Notices(offset), offset, key),
    }
    true
}

/// Do what the prompt's answer asked for.
///
/// An empty answer is a refusal, not a guess: a receive falls back to the
/// directory the user is standing in, because that is a place and not a name,
/// but a send has nothing to offer and says so rather than opening a file called
/// "".
fn start(app: &mut App, prompt: Prompt) {
    let answer = prompt.answer().to_string();
    match prompt.action {
        PromptAction::SendFile => {
            if answer.is_empty() {
                let text = app.strings().xfer_no_path.to_string();
                app.push_notice(crate::app::Notice {
                    text,
                    level: NoticeLevel::Warn,
                });
                return;
            }
            app.start_send(std::path::PathBuf::from(answer));
        }
        PromptAction::ReceiveInto => {
            let dir = if answer.is_empty() {
                RECEIVE_DEFAULT.to_string()
            } else {
                answer
            };
            app.start_receive(std::path::PathBuf::from(dir));
        }
        // A network name with no network to name is a refusal rather than a
        // guess: joining "" would send `@w=,` and the firmware would read it as
        // an empty SSID, which is a different failure and a much more confusing
        // one.
        PromptAction::WifiSsid => {
            if answer.trim().is_empty() {
                let text = app.strings().net_no_ssid.to_string();
                app.push_notice(crate::app::Notice {
                    text,
                    level: NoticeLevel::Warn,
                });
                return;
            }
            app.network.ssid_input = answer.trim().to_string();
            let prompt = Prompt::new(WIFI_PASSWORD_TITLE, PromptAction::WifiPassword);
            app.dialog = Some(Dialog::Prompt(prompt));
        }
        // The password is never stored: it goes from the field into the command
        // and out of the process, and the approval card that follows shows the
        // network's name and not its key.
        PromptAction::WifiPassword => {
            let command =
                match crate::accessory::wifi_command("connect", &app.network.ssid_input, &answer) {
                    Ok(command) => command,
                    Err(why) => {
                        app.push_notice(crate::app::Notice {
                            text: why,
                            level: NoticeLevel::Error,
                        });
                        return;
                    }
                };
            ask(app, ConfirmAction::Accessory(command));
        }
        PromptAction::WebdavUrl => {
            if answer.trim().is_empty() {
                let text = app.strings().net_no_url.to_string();
                app.push_notice(crate::app::Notice {
                    text,
                    level: NoticeLevel::Warn,
                });
                return;
            }
            let command = match crate::accessory::webdav_command("on", answer.trim()) {
                Ok(command) => command,
                Err(why) => {
                    app.push_notice(crate::app::Notice {
                        text: why,
                        level: NoticeLevel::Error,
                    });
                    return;
                }
            };
            ask(app, ConfirmAction::Accessory(command));
        }
    }
}

/// The dialog after a scroll key, or `None` when the key closes it.
fn scrolled(app: &App, dialog: Dialog, offset: usize, key: KeyEvent) -> Option<Dialog> {
    let last = row_count(app, &dialog).saturating_sub(1);
    let moved = match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::Enter => return None,
        KeyCode::Up => offset.saturating_sub(1),
        KeyCode::Down => offset.saturating_add(1),
        KeyCode::PageUp => offset.saturating_sub(10),
        KeyCode::PageDown => offset.saturating_add(10),
        KeyCode::Home => 0,
        KeyCode::End => last,
        _ => offset,
    };
    let moved = moved.min(last);
    Some(match dialog {
        Dialog::Notices(_) => Dialog::Notices(moved),
        _ => Dialog::Help(moved),
    })
}

/// Do what the confirmation agreed to.
fn run(app: &mut App, action: ConfirmAction) {
    match action {
        ConfirmAction::Send(command) => {
            app.send_command(&command);
        }
        // Says so when it cannot be sent, which `send_command` does not: a
        // keystroke refused in silence is right, but a command the user just
        // approved in a dialog is owed an answer.
        ConfirmAction::Accessory(command) => {
            if let Err(why) = app.run_accessory(&command) {
                app.push_notice(crate::app::Notice {
                    text: why,
                    level: NoticeLevel::Error,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Notice;
    use crate::event::ConnectionState;
    use crate::theme::Theme;

    fn text_of(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every string this module owns is written in both languages — the table
    /// is the contract, and a missing translation is the bug the flat tables
    /// exist to make impossible.
    #[test]
    fn every_dialog_string_is_bilingual() {
        let mut entries: Vec<Entry> = vec![
            HELP_TITLE,
            NOTICES_TITLE,
            CONFIRM_TITLE,
            HELP_KEYS,
            NOTICES_KEYS,
            CONFIRM_KEYS,
            NOTICES_EMPTY,
            BUTTON_YES,
            BUTTON_NO,
            CONFIRM_SEND,
            CONFIRM_SEND_DETAIL,
            LEVEL_INFO,
            LEVEL_WARN,
            LEVEL_ERROR,
            HELP_NOTE,
            PROMPT_KEYS,
            SEND_TITLE,
            SEND_LABEL,
            SEND_PLACEHOLDER,
            RECEIVE_TITLE,
            RECEIVE_LABEL,
            RECEIVE_PLACEHOLDER,
        ];
        entries.extend(HELP_ROWS.iter().map(|(_, meaning)| *meaning));
        crate::i18n::assert_bilingual(&entries);
    }

    /// The help lists every chord the interface answers to, and every entry
    /// names a key — a help overlay that forgot `Ctrl+Q` would be worse than
    /// none, because the user would trust it.
    #[test]
    fn the_help_names_the_chords() {
        let app = App::default();
        let shown = text_of(&lines(&Dialog::Help(0), &app, 60, 20));
        for key in [
            "Ctrl+P", "F1", "F2–F6", "Tab", "Enter", "PgUp", "Ctrl+L", "Ctrl+X", "Ctrl+T", "Ctrl+Q",
        ] {
            assert!(shown.contains(key), "help is missing {key:?}\n{shown}");
        }
        for (key, _) in HELP_ROWS {
            assert!(!key.trim().is_empty(), "a help row has no key");
        }
    }

    /// The help follows the language: the meaning translates, the chords do not.
    #[test]
    fn the_help_follows_the_language() {
        let zh = App {
            lang: Lang::Zh,
            ..App::default()
        };
        let en = App {
            lang: Lang::En,
            ..App::default()
        };
        let zh_text = text_of(&lines(&Dialog::Help(0), &zh, 60, 20));
        let en_text = text_of(&lines(&Dialog::Help(0), &en, 60, 20));
        assert!(zh_text.contains("命令面板"), "{zh_text}");
        assert!(en_text.contains("Command palette"), "{en_text}");
        assert!(zh_text.contains("Ctrl+P") && en_text.contains("Ctrl+P"));
        assert_eq!(title(&Dialog::Help(0), Lang::Zh), " 键盘帮助 ");
        assert_eq!(title(&Dialog::Help(0), Lang::En), " Keyboard help ");
    }

    /// The notice log shows what was announced, newest last, with the level
    /// spelled out — and an empty log says so rather than showing a bare box.
    #[test]
    fn the_notice_log_shows_what_was_announced() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        assert!(text_of(&lines(&Dialog::Notices(0), &app, 60, 10)).contains("Nothing has been"));
        app.push_notice(Notice {
            text: "link open: stdio: -".into(),
            level: NoticeLevel::Info,
        });
        app.push_notice(Notice {
            text: "connect failed: nope".into(),
            level: NoticeLevel::Error,
        });
        let shown = text_of(&lines(&Dialog::Notices(0), &app, 60, 10));
        assert!(shown.contains("info"), "{shown}");
        assert!(shown.contains("error"), "{shown}");
        let first = shown.find("link open").expect("the first is listed");
        let second = shown.find("connect failed").expect("the second is listed");
        assert!(first < second, "the log runs newest last:\n{shown}");
    }

    /// The notice log keeps more than the toast stack does, so the dialog is a
    /// *log* and not a second view of the three toasts already on screen.
    #[test]
    fn the_log_keeps_more_than_the_toasts() {
        let mut app = App::default();
        for i in 0..crate::app::MAX_NOTICES + 5 {
            app.push_notice(Notice {
                text: format!("n{i}"),
                level: NoticeLevel::Info,
            });
        }
        assert_eq!(
            app.notices.len(),
            crate::app::MAX_NOTICES,
            "the toasts stay bounded"
        );
        assert!(app.log.len() > crate::app::MAX_NOTICES, "the log is deeper");
    }

    /// Scrolling walks the window down one row at a time and stops at both
    /// ends: a help overlay that could be scrolled off its own text would leave
    /// the user staring at blanks.
    #[test]
    fn the_help_scrolls_and_stops_at_the_ends() {
        let mut app = App::default();
        let height = 6;
        let first = text_of(&lines(&Dialog::Help(0), &app, 60, height));
        assert!(first.contains("Ctrl+P"), "{first}");

        open_help(&mut app);
        handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.dialog, Some(Dialog::Help(1)));
        handle_key(&mut app, KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.dialog, Some(Dialog::Help(0)));
        handle_key(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        let last = row_count(&app, &Dialog::Help(0)) - 1;
        assert_eq!(app.dialog, Some(Dialog::Help(last)));
        // Past the end stays at the end, and the window shows the tail.
        handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.dialog, Some(Dialog::Help(last)));
        let tail = text_of(&lines(&Dialog::Help(last), &app, 60, height));
        assert!(tail.contains("Ctrl+Q"), "{tail}");
    }

    /// Esc, `q`, `?` and Enter all close a scrolled dialog, and every one of
    /// them is consumed — a key that closed the overlay and *also* went to the
    /// device would be a silent double action.
    #[test]
    fn the_close_keys_close_and_are_consumed() {
        for code in [
            KeyCode::Esc,
            KeyCode::Char('q'),
            KeyCode::Char('?'),
            KeyCode::Enter,
        ] {
            let mut app = App::default();
            open_help(&mut app);
            assert!(handle_key(
                &mut app,
                KeyEvent::new(code, KeyModifiers::NONE)
            ));
            assert!(app.dialog.is_none(), "{code:?} did not close");
        }
    }

    /// A confirmation names the command and starts on *Cancel*: the safe answer
    /// is the one a stray Enter gives.
    #[test]
    fn a_confirmation_names_the_command_and_defaults_to_cancel() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        ask(&mut app, ConfirmAction::Send("reboot".into()));
        let Some(Dialog::Confirm(confirm)) = &app.dialog else {
            panic!("no confirmation was raised");
        };
        assert!(confirm.prompt.contains("reboot"), "{}", confirm.prompt);
        assert!(!confirm.yes, "the safe button is highlighted");
        assert_eq!(title(&app.dialog.clone().unwrap(), Lang::En), " Confirm ");
    }

    /// Enter on the default highlight cancels; `y` confirms; `n` and Esc cancel
    /// — and confirming is what actually sends.
    #[test]
    fn a_confirmation_only_sends_when_it_is_agreed_to() {
        // Enter on the default highlight: nothing is sent.
        let mut app = App::default();
        ask(&mut app, ConfirmAction::Send("reboot".into()));
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.dialog.is_none());
        assert_eq!(app.tx, 0, "the default answer sent nothing");

        // Flip to yes and Enter: it goes out — or, with no link, it is refused
        // out loud rather than silently dropped.
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        ask(&mut app, ConfirmAction::Send("reboot".into()));
        handle_key(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert!(matches!(
            app.dialog,
            Some(Dialog::Confirm(Confirm { yes: true, .. }))
        ));
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.dialog.is_none());
        assert!(
            app.log
                .iter()
                .any(|notice| notice.text.contains("Not connected")),
            "a confirmed send with no link is refused out loud: {:?}",
            app.log
        );

        // `y` is the shortcut straight to the same place.
        let mut app = App::default();
        ask(&mut app, ConfirmAction::Send("reboot".into()));
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );
        assert!(app.dialog.is_none());

        // `n` cancels without doing anything.
        let mut app = App::default();
        ask(&mut app, ConfirmAction::Send("reboot".into()));
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        );
        assert!(app.dialog.is_none());
        assert!(app.log.is_empty(), "cancelling announced nothing");
    }

    /// A confirmed command rides the same path a preset does: with a live link
    /// it reaches the device, and the counter moves with it.
    #[test]
    fn a_confirmed_command_reaches_the_device() {
        use crate::session::Dial;
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        app.dial(&Dial::Stdio("-".into()))
            .expect("the loopback opens");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline && app.state != ConnectionState::Connected {
            app.pump();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(app.state, ConnectionState::Connected);

        ask(&mut app, ConfirmAction::Send("version".into()));
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );
        assert_eq!(app.tx, 8, "the carriage return is part of what was sent");
    }

    /// Joining a network is two questions and one command: the name is asked,
    /// then the password, and only then is the change put in front of the user.
    /// A half of that command is not a thing that can be sent.
    #[test]
    fn joining_a_network_asks_for_the_name_then_the_password_then_agreement() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        prompt_wifi_ssid(&mut app);
        for ch in "BenchNet".chars() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            );
        }
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // The password, not the wire.
        let Some(Dialog::Prompt(prompt)) = &app.dialog else {
            panic!("the password was asked for: {:?}", app.dialog);
        };
        assert_eq!(prompt.action, PromptAction::WifiPassword);
        assert_eq!(app.network.ssid_input, "BenchNet", "the name was kept");

        for ch in "s3cret".chars() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            );
        }
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let Some(Dialog::Confirm(card)) = &app.dialog else {
            panic!("and then the approval card: {:?}", app.dialog);
        };
        assert_eq!(
            card.action,
            ConfirmAction::Accessory("@w=BenchNet,s3cret".to_string())
        );
        // The card says what it is doing without saying the password: it is on
        // a screen somebody may be looking at.
        assert!(!card.prompt.contains("s3cret"), "{:?}", card.prompt);
        assert!(card.prompt.contains("BenchNet"), "{:?}", card.prompt);
    }

    /// Naming no network is a stop, not a join: `@w=,` would be read by the
    /// firmware as an empty SSID, which is a different and much more confusing
    /// failure than the one worth reporting.
    #[test]
    fn joining_a_network_nobody_named_is_refused() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        prompt_wifi_ssid(&mut app);
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.dialog.is_none(), "and nothing else was asked");
        assert!(app.network.ssid_input.is_empty());
        assert!(
            app.notices
                .iter()
                .any(|n| n.level == NoticeLevel::Warn && !n.text.is_empty()),
            "it said why: {:?}",
            app.notices
        );
    }

    /// A control chord is not consumed: the global handler still owns `Ctrl+Q`
    /// and `Ctrl+T` with a modal open.
    #[test]
    fn a_control_chord_falls_through_to_the_global_handler() {
        let mut app = App::default();
        open_help(&mut app);
        assert!(!handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)
        ));
        assert!(app.dialog.is_some(), "the help is still open");
    }

    /// The body is padded to the height it is given, so the legend stays on the
    /// box's bottom row however short the content is.
    #[test]
    fn the_body_fills_its_height() {
        let app = App::default();
        for height in [1usize, 3, 8, 40] {
            let shown = lines(&Dialog::Notices(0), &app, 60, height);
            assert_eq!(shown.len(), height, "height {height}");
        }
        assert!(lines(&Dialog::Notices(0), &app, 60, 0).is_empty());
    }

    /// The confirmation wraps a long prompt instead of cutting it: the command
    /// being confirmed is the one thing that must never be truncated.
    #[test]
    fn a_long_prompt_wraps_rather_than_cuts() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        let long = "a-very-long-preset-command-that-does-not-fit-on-one-row";
        ask(&mut app, ConfirmAction::Send(long.into()));
        let shown = text_of(&lines(&app.dialog.clone().unwrap(), &app, 24, 12));
        assert!(shown.contains("a-very-long-preset"), "{shown}");
        assert!(
            shown.contains("one-row"),
            "the tail of the command is there:\n{shown}"
        );
        assert!(
            !shown.contains('…'),
            "a prompt is wrapped, not cut:\n{shown}"
        );
    }

    /// The buttons take the right chip in each state, so the highlight really
    /// moves rather than both buttons looking chosen.
    #[test]
    fn the_highlight_moves_between_the_buttons() {
        let app = App {
            lang: Lang::En,
            ..App::default()
        };
        let no = Dialog::Confirm(Confirm {
            prompt: "p".into(),
            detail: "d".into(),
            action: ConfirmAction::Send("x".into()),
            yes: false,
        });
        let yes = Dialog::Confirm(Confirm {
            prompt: "p".into(),
            detail: "d".into(),
            action: ConfirmAction::Send("x".into()),
            yes: true,
        });
        let chips = |dialog: &Dialog| -> (usize, usize) {
            let theme: Theme = Theme::DARK;
            let rows = body(dialog, &app, 40, &theme);
            let last = rows.last().expect("the button row");
            let yes = last.spans[0].style;
            let no = last.spans[2].style;
            (
                usize::from(yes == theme.selected()),
                usize::from(no == theme.selected()),
            )
        };
        assert_eq!(chips(&no), (0, 1), "cancel is highlighted by default");
        assert_eq!(chips(&yes), (1, 0), "and the highlight moves to confirm");
    }

    // --- the transfer prompt ------------------------------------------------

    /// The field edits *at the caret*, the way a text field does — including in
    /// the middle of a path, which is the case an append-only field gets wrong.
    #[test]
    fn the_field_edits_at_the_caret() {
        let mut prompt = Prompt::new(SEND_TITLE, PromptAction::SendFile);
        assert_eq!(
            prompt.split(),
            ("", " ", ""),
            "an empty field still has a caret"
        );

        prompt.insert('a');
        prompt.insert('b');
        assert_eq!(prompt.text, "ab");
        prompt.left();
        prompt.insert('X');
        assert_eq!(prompt.text, "aXb");
        // Inserting moves the caret *past* what was typed, so the highlighted
        // glyph is the one now to the caret's right — not the one just written.
        assert_eq!(prompt.split(), ("aX", "b", ""));
        prompt.backspace();
        assert_eq!(prompt.text, "ab");
        prompt.delete();
        assert_eq!(prompt.text, "a");
        prompt.home();
        prompt.delete();
        assert_eq!(prompt.text, "");
        prompt.backspace();
        assert_eq!(prompt.text, "", "backspace at the start does nothing");
        prompt.insert('z');
        prompt.end();
        prompt.right();
        assert_eq!(prompt.cursor, 1, "right stops at the end");
        assert_eq!(prompt.answer(), "z");
    }

    /// The answer is trimmed: a trailing space at the end of a path is easy to
    /// type and never meant, and a leading one hides.
    #[test]
    fn the_answer_is_trimmed() {
        let mut prompt = Prompt::new(SEND_TITLE, PromptAction::SendFile);
        for c in "  fw.bin ".chars() {
            prompt.insert(c);
        }
        assert_eq!(prompt.text, "  fw.bin ");
        assert_eq!(prompt.answer(), "fw.bin");

        let mut blank = Prompt::new(SEND_TITLE, PromptAction::SendFile);
        for _ in 0..3 {
            blank.insert(' ');
        }
        assert_eq!(blank.answer(), "", "whitespace is not a file name");
    }

    /// The caret moves by whole characters. A caret that split one would panic
    /// the painter, and a path is the likeliest place to meet a two-byte one.
    #[test]
    fn the_caret_never_splits_a_character() {
        let mut prompt = Prompt::new(SEND_TITLE, PromptAction::SendFile);
        for c in "固件.bin".chars() {
            prompt.insert(c);
        }
        assert_eq!(prompt.text, "固件.bin");

        for _ in 0..20 {
            prompt.left();
            let (before, under, after) = prompt.split();
            if prompt.cursor < prompt.text.len() {
                assert_eq!(format!("{before}{under}{after}"), prompt.text);
            } else {
                assert_eq!(under, " ", "the caret is one cell at the end");
            }
        }
        assert_eq!(prompt.cursor, 0, "left stops at the start");
        for _ in 0..20 {
            prompt.right();
            let (before, under, after) = prompt.split();
            if prompt.cursor < prompt.text.len() {
                assert_eq!(format!("{before}{under}{after}"), prompt.text);
            }
        }
        assert_eq!(prompt.cursor, prompt.text.len(), "right stops at the end");
        assert_eq!(prompt.answer(), "固件.bin");
    }

    /// The field is painted with a one-cell caret in it, and the placeholder only
    /// while there is nothing to show.
    #[test]
    fn the_field_paints_its_caret() {
        let app = App {
            lang: Lang::En,
            ..App::default()
        };
        let theme = Theme::DARK;
        let row = |prompt: Prompt| -> Line<'static> {
            body(&Dialog::Prompt(prompt), &app, 60, &theme)
                .pop()
                .expect("the field row")
        };

        let empty = row(Prompt::new(SEND_TITLE, PromptAction::SendFile));
        let shown: String = empty.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            shown.contains("for example firmware.bin"),
            "an empty field says what it wants: {shown:?}"
        );
        assert_eq!(
            empty.spans[1].style,
            theme.selected(),
            "the caret is the highlighted cell"
        );

        let mut typed = Prompt::new(SEND_TITLE, PromptAction::SendFile);
        typed.insert('x');
        let shown: String = row(typed)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(shown, "  x ", "the caret sits after what was typed");
    }

    /// A prompt carries its own title and legend, in the interface language.
    #[test]
    fn a_prompt_carries_its_own_words() {
        let send = Dialog::Prompt(Prompt::new(SEND_TITLE, PromptAction::SendFile));
        let receive = Dialog::Prompt(Prompt::new(RECEIVE_TITLE, PromptAction::ReceiveInto));
        assert_eq!(title(&send, Lang::En), " Send a file ");
        assert_eq!(title(&receive, Lang::Zh), " 接收文件 ");
        assert_ne!(title(&send, Lang::Zh), title(&send, Lang::En));

        let theme = Theme::DARK;
        let en = legend(&send, Lang::En, &theme);
        assert!(en.spans[0].content.contains("Enter start"), "{en:?}");
        let zh = legend(&send, Lang::Zh, &theme);
        assert!(zh.spans[0].content.contains("Enter 开始"), "{zh:?}");
        // The two prompts do not share their words either.
        assert_ne!(title(&send, Lang::En), title(&receive, Lang::En));
    }

    /// A prompt owns the keyboard while it is up: a letter is a character in the
    /// field and not a keystroke for the device.
    #[test]
    fn a_prompt_takes_the_keyboard_off_the_device() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        prompt_send_file(&mut app);
        assert!(app.dialog.is_some());
        for c in "fw".chars() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            );
        }
        assert_eq!(app.tx, 0, "the device heard nothing");
        let Some(Dialog::Prompt(prompt)) = &app.dialog else {
            panic!("the prompt closed on a letter");
        };
        assert_eq!(prompt.text, "fw");

        // Escape gives the keyboard back without starting anything.
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.dialog.is_none());
        assert!(!app.transferring());
        assert!(app.log.is_empty(), "cancelling announced nothing");
    }

    /// An empty answer is a refusal for a send and a *place* for a receive: a
    /// file is not something to guess at, and the directory the user is standing
    /// in is.
    #[test]
    fn an_empty_answer_is_a_refusal_for_a_send_and_a_default_for_a_receive() {
        use crate::session::Dial;

        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        prompt_send_file(&mut app);
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.dialog.is_none(), "the prompt closed");
        assert!(!app.transferring());
        assert_eq!(
            app.log.last().map(|notice| notice.text.as_str()),
            Some("Name a file to send.")
        );

        // Answered properly *and* refused by the transfer: the prompt's job is
        // the answer, not the wiring, and a named file with no link is the
        // transfer's refusal to make.
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        prompt_send_file(&mut app);
        for c in "fw.bin".chars() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            );
        }
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.dialog.is_none());
        assert_eq!(
            app.log.last().map(|notice| notice.text.as_str()),
            Some("Not connected — no transfer was started.")
        );

        // A receive with an empty answer lands on the working directory.
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        app.dial(&Dial::Stdio("-".into()))
            .expect("the loopback opens");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline && app.state != ConnectionState::Connected {
            app.pump();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(app.state, ConnectionState::Connected);
        prompt_receive(&mut app);
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let live = app.live_transfer.as_ref().expect("the transfer started");
        assert_eq!(live.target, ".", "the directory the user is standing in");
        assert!(!live.upload);
    }
}
