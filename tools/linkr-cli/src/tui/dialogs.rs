//! Modal dialogs: confirmations, assistant approvals, UART settings, the help
//! overlay and the notice log (CONTRACTS.md section 5).
//!
//! One dialog is open at a time; `Esc` always closes. Confirmations reuse the
//! exact web strings so a test can pin them, and every string the user reads
//! goes through `strings!` (WEB_UX_SPEC section 9: `linkr-lang`).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::oneshot;

use super::agent_settings::AgentSettingsState;
use super::i18n::{strings, t, tr, Entry, Lang};
use super::replies::parse_uart_reply;
use super::state::{App, TextField};
use crate::agent::{ApprovalBroker, ApprovalDecision, ApprovalKind, ApprovalRequest};
use crate::event::NoticeLevel;

/// `confirmReboot` of the web client (WEB_UX_SPEC section 1.6). Parity
/// literals pinned by the web suite; the reboot / disconnect dialogs get
/// their translated text from `sidebar.rs`, so they stay English here.
pub const REBOOT_CONFIRM: &str = "Reboot the connected device now?";
pub const DISCONNECT_CONFIRM: &str = "Disconnect from the current device now?";

/// Web `#uartInput` default (`PYTHON_CLI_SPEC` / WEB_UX_SPEC section 1.3).
pub const DEFAULT_UART: &str = "115200,8,n,1,n";

strings! {
    // Box titles (`Dialog::title_lang`; the frame draws them).
    DLG_TITLE_CONFIRM => "Confirm", "确认";
    DLG_TITLE_APPROVAL => "Assistant approval", "助手审批";
    DLG_TITLE_UART => "UART settings", "UART 设置";
    DLG_TITLE_SETTINGS => "AI configuration", "AI 配置";
    DLG_TITLE_HELP => "Keyboard help", "键盘帮助";
    DLG_TITLE_NOTICES => "Notices", "通知";
    DLG_TITLE_FALLBACK => "Dialog", "对话框";

    // Confirmation body (`sidebar.rs` carries the reboot / disconnect texts).
    DLG_QUIT_MSG => "Quit the TUI now?", "立即退出 TUI？";
    DLG_TITLE_QUIT => "Quit", "退出";
    DLG_Y_CONFIRM => " confirm · ", " 确认 · ";
    DLG_ESC_CANCEL => " cancel", " 取消";

    // Approval cards.
    DLG_APPROVE_RUN => "Run a command on the target?", "在目标设备上运行命令？";
    DLG_APPROVE_SEND => "Send to the current target? (Control characters are escaped.)",
        "发送到当前目标设备？（控制字符会被转义。）";
    DLG_APPROVE_ACCESSORY => "This changes Linkr Bee itself (not the target), sent over the \
        encrypted Bluetooth management channel:",
        "这会修改 Linkr Bee 本体（非目标设备），通过加密蓝牙管理信道发送：";
    DLG_Y_APPROVE => " approve · ", " 批准 · ";
    DLG_N_REJECT => " reject · ", " 拒绝 · ";
    DLG_ESC_REJECT => " reject", " 拒绝";
    DLG_APPROVED => "Approved.", "已批准。";
    DLG_REJECTED => "Rejected.", "已拒绝。";

    // UART settings dialog.
    DLG_UART_PROMPT => "Set the bridge UART: baud,data,parity,stop,flow",
        "设置桥接 UART：baud,data,parity,stop,flow";
    DLG_UART_KEYS => "Enter query & set · Ctrl+S send · Esc close",
        "Enter 查询并设置 · Ctrl+S 发送 · Esc 关闭";
    DLG_UART_QUERYING => "querying uart…", "正在查询 uart…";
    DLG_UART_SETTING => "setting uart={}…", "正在设置 uart={}…";
    DLG_UART_APPLIED => "OK uart={}", "已设置 uart={}";
    DLG_UART_CLOSED => "disconnected before the UART reply", "UART 应答前连接已断开";
    DLG_UART_NEED_BLE => "Connect over BLE to change the bridge UART.",
        "请先通过 BLE 连接再修改桥接 UART。";

    // Notice log.
    DLG_NO_NOTICES => "No notices yet.", "暂无通知。";

    // F1 help overlay: key column.
    DLG_HELP_KEY_SIDEBAR => "↑↓ in sidebar", "侧栏中的 ↑↓";
    DLG_HELP_KEY_QUICK => "Quick send", "快捷发送";
    // F1 help overlay: description column.
    DLG_HELP_PALETTE => "Command palette (every action, searchable)",
        "命令面板（全部动作，可搜索）";
    DLG_HELP_THIS => "This help", "本帮助";
    DLG_HELP_TERMINAL => "Terminal view", "终端视图";
    DLG_HELP_DIAG => "Diagnostics view (@i?)", "诊断视图（@i?）";
    DLG_HELP_NETWORK => "Network view (WiFi / WebDAV)", "网络视图（WiFi / WebDAV）";
    DLG_HELP_ASSISTANT => "Assistant view", "助手视图";
    DLG_HELP_FOCUS_COMPOSER => "Focus the assistant composer", "聚焦助手输入框";
    DLG_HELP_MODE => "Assistant: pick the execution mode", "助手：选择执行模式";
    DLG_HELP_CONFIG => "Assistant: AI configuration", "助手：AI 配置";
    DLG_HELP_NEW_CHAT => "Assistant: start a new chat", "助手：新建对话";
    DLG_HELP_SEND => "Assistant: send the message", "助手：发送消息";
    DLG_HELP_SIDEBAR_KEYS => "Move the selection, Enter activates it",
        "移动选中项，Enter 确认";
    DLG_HELP_PRESETS => "Sidebar → help / version / uname / df / reboot",
        "侧栏 → help / version / uname / df / reboot";
    DLG_HELP_CLEAR => "Clear the terminal pane", "清屏终端窗格";
    DLG_HELP_QUIT => "Quit (asks first while connected)", "退出（连接中时先询问）";
    DLG_HELP_FOCUS_SIDEBAR => "Focus the sidebar", "聚焦侧栏";
    DLG_HELP_ESC => "Close an overlay / back to the terminal", "关闭浮层 / 返回终端";
    DLG_HELP_SCROLL => "Scroll the terminal scrollback", "滚动终端回滚缓冲";
    DLG_HELP_SCROLLBACK => "Scrollback: top / bottom", "回滚缓冲：顶部 / 底部";
    DLG_HELP_ARM_SHIFT => "Arm one-shot Shift for the next key",
        "为下一个按键启用一次性 Shift";
    DLG_HELP_ARM_CTRL => "Arm one-shot Ctrl for the next key",
        "为下一个按键启用一次性 Ctrl";
    DLG_HELP_ARM_ALT => "Arm one-shot Alt for the next key",
        "为下一个按键启用一次性 Alt";
    DLG_HELP_ENTER => "Send a line (Enter mode applies)", "发送一行（应用回车模式）";
    DLG_HELP_TAB => "Sent to the target as TAB / CSI Z", "作为 TAB / CSI Z 发送给目标";
    DLG_HELP_FKEYS => "Sent to the target unchanged", "原样发送给目标";
    DLG_HELP_TERM_ACTIONS => "Font size, autoscroll, echo, save log, copy",
        "字号、自动滚动、回显、保存日志、复制";
    DLG_HELP_APP_ACTIONS => "Notices, help, quit, focus switching",
        "通知、帮助、退出、焦点切换";
    DLG_HELP_FOOTER => "Byte sequences match web/terminal_keys.js.",
        "字节序列与 web/terminal_keys.js 一致。";
}

/// English half of [`DLG_QUIT_MSG`] (the web `confirmQuit` literal).
pub const QUIT_CONFIRM: &str = DLG_QUIT_MSG[0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    Reboot,
    Disconnect,
    Quit,
}

/// One approval request parked by [`TuiBroker`] until the user answers.
pub struct PendingApproval {
    pub request: ApprovalRequest,
    tx: oneshot::Sender<ApprovalDecision>,
}

impl PendingApproval {
    /// Answer the agent. Dropping the dialog without answering rejects the
    /// request (the agent sees a closed channel).
    pub fn resolve(self, decision: ApprovalDecision) {
        let _ = self.tx.send(decision);
    }
}

/// Queue shared between the agent task (producer) and the TUI (consumer).
#[derive(Clone, Default)]
pub struct TuiBroker {
    queue: std::sync::Arc<std::sync::Mutex<VecDeque<PendingApproval>>>,
}

use std::collections::VecDeque;

impl TuiBroker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take the oldest pending request, if any (called once per loop tick).
    pub fn take_pending(&self) -> Option<PendingApproval> {
        self.queue
            .lock()
            .map(|mut queue| queue.pop_front())
            .ok()
            .flatten()
    }
}

impl ApprovalBroker for TuiBroker {
    fn ask(&self, request: ApprovalRequest) -> oneshot::Receiver<ApprovalDecision> {
        let (tx, rx) = oneshot::channel();
        if let Ok(mut queue) = self.queue.lock() {
            queue.push_back(PendingApproval { request, tx });
        }
        // On a poisoned lock the sender is dropped: the agent sees a closed
        // channel, which reads as a rejection.
        rx
    }
}

/// Everything shown above the frame.
// The settings state is the fat variant; it lives for one dialog at a time,
// so it stays inline instead of paying an indirection per keystroke.
#[allow(clippy::large_enum_variant)]
pub enum Dialog {
    Confirm {
        kind: ConfirmKind,
        title: String,
        message: String,
    },
    Approval(Box<PendingApproval>),
    Uart {
        field: TextField,
        status: Option<String>,
        error: Option<String>,
        reply: Option<oneshot::Receiver<Result<crate::protocol::MgmtReply, String>>>,
    },
    Settings(AgentSettingsState),
    /// Scroll offset of the body: the help is longer than a short terminal
    /// can show, so the overlay scrolls instead of clipping the tail off.
    Help(u16),
    Notices(u16),
}

impl Dialog {
    /// English title (`layout.rs` draws it; `title_lang` is the bilingual one).
    pub fn title(&self) -> &'static str {
        self.title_lang(Lang::En)
    }

    /// Title of the open dialog in `lang`.
    pub fn title_lang(&self, lang: Lang) -> &'static str {
        match self {
            Dialog::Confirm { .. } => t(DLG_TITLE_CONFIRM, lang),
            Dialog::Approval(_) => t(DLG_TITLE_APPROVAL, lang),
            Dialog::Uart { .. } => t(DLG_TITLE_UART, lang),
            Dialog::Settings(_) => t(DLG_TITLE_SETTINGS, lang),
            Dialog::Help(_) => t(DLG_TITLE_HELP, lang),
            Dialog::Notices(_) => t(DLG_TITLE_NOTICES, lang),
        }
    }

    /// Body scroll offset of a scrollable overlay (0 for the others).
    pub fn scroll_offset(&self) -> u16 {
        match self {
            Dialog::Help(scroll) | Dialog::Notices(scroll) => *scroll,
            _ => 0,
        }
    }
}

/// Width the dialog body is laid out at (`draw_dialog` and the arrow keys
/// have to agree, or the scroll limit drifts away from what is painted).
pub const DIALOG_WIDTH: u16 = 74;

/// How many body rows a dialog with `lines` lines can scroll through at the
/// current terminal height — the exact mirror of `draw_dialog`'s box maths.
fn scroll_limit(screen_height: u16, lines: usize) -> u16 {
    let cap = screen_height.saturating_sub(2).max(3);
    let height = (lines as u16 + 2).min(cap);
    (lines as u16).saturating_sub(height.saturating_sub(2))
}

/// Arrow-key paging for a scrollable overlay; `None` means "not a scroll key".
fn next_scroll(current: u16, limit: u16, code: KeyCode) -> Option<u16> {
    match code {
        KeyCode::Up => Some(current.saturating_sub(1)),
        KeyCode::Down => Some((current + 1).min(limit)),
        KeyCode::PageUp => Some(current.saturating_sub(10)),
        KeyCode::PageDown => Some((current + 10).min(limit)),
        KeyCode::Home => Some(0),
        KeyCode::End => Some(limit),
        _ => None,
    }
}

// --- rendering ---------------------------------------------------------------

fn rule(width: u16) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width as usize),
        Style::default().fg(Color::DarkGray),
    ))
}

/// Split `(text, width)` at the first character that would overflow `width`.
fn hard_split(text: &str, width: usize) -> (&str, &str) {
    let mut used = 0usize;
    for (index, ch) in text.char_indices() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > width && index > 0 {
            return text.split_at(index);
        }
        used += w;
    }
    (text, "")
}

/// Break a single run wider than `width`: Chinese has no spaces to wrap at, so
/// without this a translated line stays one long row and the dialog (which is
/// sized from the line count) clips it. English words fit, so this never fires
/// for them.
fn push_overlong<'a>(out: &mut Vec<Line<'static>>, word: &'a str, width: usize) -> &'a str {
    let mut rest = word;
    while unicode_width::UnicodeWidthStr::width(rest) > width {
        let (head, tail) = hard_split(rest, width);
        if head.is_empty() {
            break;
        }
        out.push(Line::from(head.to_string()));
        rest = tail;
    }
    rest
}

pub fn wrap_line(text: &str, width: u16) -> Vec<Line<'static>> {
    let width = width.max(1) as usize;
    let mut out = Vec::new();
    let mut current = String::new();
    let mut len = 0usize;
    for word in text.split(' ') {
        let w = unicode_width::UnicodeWidthStr::width(word);
        if !current.is_empty() && len + 1 + w > width {
            out.push(Line::from(std::mem::take(&mut current)));
            len = 0;
        }
        if w > width && current.is_empty() {
            current.push_str(push_overlong(&mut out, word, width));
            len = unicode_width::UnicodeWidthStr::width(current.as_str());
            continue;
        }
        if !current.is_empty() {
            current.push(' ');
            len += 1;
        }
        current.push_str(word);
        len += w;
    }
    if !current.is_empty() {
        out.push(Line::from(current));
    }
    if out.is_empty() {
        out.push(Line::from(""));
    }
    out
}

/// As much of `text` as fits in `budget` display columns, with the ellipsis
/// *inside* the budget so the row still ends where the caller planned it to.
/// A wide glyph is never split: the column it cannot have is left empty.
///
/// Counting characters instead of columns put every CJK value one column over
/// for each glyph, and the dialog (which wraps with `trim: false`) shifted
/// every row below it by one.
pub fn clip_columns(text: &str, budget: usize) -> String {
    if unicode_width::UnicodeWidthStr::width(text) <= budget {
        return text.to_string();
    }
    if budget == 0 {
        return String::new();
    }
    let room = budget - 1;
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > room {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Body lines of the open dialog. `width` is the inner width available.
pub fn render_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let lang = app.lang();
    let Some(dialog) = &app.dialog else {
        return Vec::new();
    };
    match dialog {
        Dialog::Confirm { message, .. } => {
            let mut lines = wrap_line(message, width);
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "y",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(t(DLG_Y_CONFIRM, lang), Style::default().fg(Color::Gray)),
                Span::styled(
                    "n",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" / ", Style::default().fg(Color::Gray)),
                Span::styled("Esc", Style::default().fg(Color::Gray)),
                Span::styled(t(DLG_ESC_CANCEL, lang), Style::default().fg(Color::Gray)),
            ]));
            lines
        }
        Dialog::Approval(pending) => approval_lines(pending, width, lang),
        Dialog::Uart {
            field,
            status,
            error,
            ..
        } => {
            let mut lines = vec![
                Line::from(t(DLG_UART_PROMPT, lang)),
                Line::from(""),
                Line::from(vec![
                    Span::styled("> ", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        field.text.clone(),
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(""),
            ];
            if let Some(error) = error {
                lines.extend(wrap_line(error, width).into_iter().map(|line| {
                    Line::from(
                        line.spans
                            .into_iter()
                            .map(|span| span.style(Style::default().fg(Color::Red)))
                            .collect::<Vec<_>>(),
                    )
                }));
            }
            if let Some(status) = status {
                lines.extend(wrap_line(status, width).into_iter().map(|line| {
                    Line::from(
                        line.spans
                            .into_iter()
                            .map(|span| span.style(Style::default().fg(Color::LightGreen)))
                            .collect::<Vec<_>>(),
                    )
                }));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                t(DLG_UART_KEYS, lang),
                Style::default().fg(Color::DarkGray),
            )));
            lines
        }
        Dialog::Settings(state) => super::agent_settings::render_lines(state, width, lang),
        Dialog::Help(_) => help_lines(width, lang),
        Dialog::Notices(_) => {
            if app.notices.log.is_empty() {
                vec![Line::from(Span::styled(
                    t(DLG_NO_NOTICES, lang),
                    Style::default().fg(Color::DarkGray),
                ))]
            } else {
                app.notices
                    .log
                    .iter()
                    .rev()
                    .take(200)
                    .rev()
                    .map(|(level, text)| {
                        let color = match level {
                            NoticeLevel::Info => Color::Gray,
                            NoticeLevel::Warn => Color::LightYellow,
                            NoticeLevel::Error => Color::LightRed,
                        };
                        Line::from(Span::styled(text.clone(), Style::default().fg(color)))
                    })
                    .collect()
            }
        }
    }
}

fn approval_lines(
    pending: &PendingApproval,
    width: u16,
    lang: super::i18n::Lang,
) -> Vec<Line<'static>> {
    let request = &pending.request;
    let mut lines = Vec::new();
    match &request.kind {
        ApprovalKind::RunCommand { command, mode } => {
            lines.push(Line::from(Span::styled(
                t(DLG_APPROVE_RUN, lang),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                command.clone(),
                Style::default().fg(Color::White),
            )));
            lines.push(Line::from(Span::styled(
                tr!(
                    t(super::i18n::MODE_LABEL, lang),
                    super::status::mode_text(*mode, lang)
                ),
                Style::default().fg(Color::DarkGray),
            )));
        }
        ApprovalKind::SendInput { payload } => {
            lines.push(Line::from(Span::styled(
                t(DLG_APPROVE_SEND, lang),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            for line in payload.lines() {
                lines.push(Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(Color::White),
                )));
            }
        }
        ApprovalKind::AccessoryChange { summary, command } => {
            lines.push(Line::from(Span::styled(
                t(DLG_APPROVE_ACCESSORY, lang),
                Style::default().fg(Color::LightYellow),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                summary.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                command.clone(),
                Style::default().fg(Color::Gray),
            )));
        }
    }
    if !request.question.is_empty() {
        lines.push(Line::from(""));
        lines.extend(wrap_line(&request.question, width));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            "y",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(t(DLG_Y_APPROVE, lang), Style::default().fg(Color::Gray)),
        Span::styled(
            "n",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::styled(t(DLG_N_REJECT, lang), Style::default().fg(Color::Gray)),
        Span::styled("Esc", Style::default().fg(Color::Gray)),
        Span::styled(t(DLG_ESC_REJECT, lang), Style::default().fg(Color::Gray)),
    ]));
    lines
}

/// F1 overlay. Mirrors the global keys of CONTRACTS.md section 5.
fn help_lines(width: u16, lang: Lang) -> Vec<Line<'static>> {
    struct Row(&'static str, Entry);
    const ROWS: [Row; 27] = [
        Row("Ctrl+P", DLG_HELP_PALETTE),
        Row("F1", DLG_HELP_THIS),
        Row("F2", DLG_HELP_TERMINAL),
        Row("F3", DLG_HELP_DIAG),
        Row("F4", DLG_HELP_NETWORK),
        Row("F5", DLG_HELP_ASSISTANT),
        Row("Ctrl+Shift+K", DLG_HELP_FOCUS_COMPOSER),
        Row("Ctrl+Shift+M", DLG_HELP_MODE),
        Row("Ctrl+Shift+S", DLG_HELP_CONFIG),
        Row("Ctrl+Shift+N", DLG_HELP_NEW_CHAT),
        Row("Ctrl/Alt+Enter", DLG_HELP_SEND),
        Row("↑↓ in sidebar", DLG_HELP_SIDEBAR_KEYS),
        Row("Quick send", DLG_HELP_PRESETS),
        Row("Ctrl+L", DLG_HELP_CLEAR),
        Row("Ctrl+Q", DLG_HELP_QUIT),
        Row("Ctrl+Up", DLG_HELP_FOCUS_SIDEBAR),
        Row("Esc", DLG_HELP_ESC),
        Row("Shift+PgUp/PgDn", DLG_HELP_SCROLL),
        Row("Shift+Home/End", DLG_HELP_SCROLLBACK),
        Row("Ctrl+Shift+R", DLG_HELP_ARM_SHIFT),
        Row("Ctrl+Shift+C", DLG_HELP_ARM_CTRL),
        Row("Ctrl+Shift+A", DLG_HELP_ARM_ALT),
        Row("Enter", DLG_HELP_ENTER),
        Row("Tab / Shift+Tab", DLG_HELP_TAB),
        Row("F6..F12", DLG_HELP_FKEYS),
        Row("Ctrl+P → term.*", DLG_HELP_TERM_ACTIONS),
        Row("Ctrl+P → app.*", DLG_HELP_APP_ACTIONS),
    ];
    // Two of the left cells are labels rather than key chords, so they follow
    // the language; the padding is display-width aware (Chinese is 2 columns
    // per glyph), which changes nothing for the ASCII English keys.
    let key_text = |key: &'static str| match key {
        "↑↓ in sidebar" => t(DLG_HELP_KEY_SIDEBAR, lang),
        "Quick send" => t(DLG_HELP_KEY_QUICK, lang),
        other => other,
    };
    let mut lines = vec![rule(width)];
    for Row(key, text) in ROWS {
        let key = key_text(key);
        let pad = 16usize.saturating_sub(unicode_width::UnicodeWidthStr::width(key));
        lines.push(Line::from(vec![
            Span::styled(
                format!("{key}{}", " ".repeat(pad)),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(t(text, lang).to_string(), Style::default().fg(Color::Gray)),
        ]));
    }
    lines.push(rule(width));
    lines.push(Line::from(Span::styled(
        t(DLG_HELP_FOOTER, lang),
        Style::default().fg(Color::DarkGray),
    )));
    lines
}

// --- keys --------------------------------------------------------------------

/// Handle a key while a dialog is open. Every key is consumed by the dialog
/// layer; nothing falls through to the view underneath.
pub fn handle_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // Quit stays reachable even from inside a dialog.
    if ctrl && key.code == KeyCode::Char('q') {
        request_quit(app);
        return;
    }
    // Informational overlays close on any of the usual dismissal keys and
    // scroll with the arrows: the help lists more keys than a short terminal
    // can show, and the notice log holds up to 200 lines.
    if matches!(app.dialog, Some(Dialog::Help(_)) | Some(Dialog::Notices(_))) {
        let current = app.dialog.as_ref().map(Dialog::scroll_offset).unwrap_or(0);
        if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
            app.dialog = None;
            return;
        }
        let total = render_lines(app, DIALOG_WIDTH).len();
        let limit = scroll_limit(app.screen_height, total);
        let Some(next) = next_scroll(current, limit, key.code) else {
            return;
        };
        if let Some(Dialog::Help(scroll)) | Some(Dialog::Notices(scroll)) = app.dialog.as_mut() {
            *scroll = next;
        }
        return;
    }
    let Some(dialog) = app.dialog.take() else {
        return;
    };
    match dialog {
        Dialog::Confirm {
            kind,
            title,
            message,
        } => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => confirm(app, kind),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {}
            _ => {
                app.dialog = Some(Dialog::Confirm {
                    kind,
                    title,
                    message,
                })
            }
        },
        Dialog::Approval(pending) => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                pending.resolve(ApprovalDecision::Approved);
                app.toast(NoticeLevel::Info, t(DLG_APPROVED, app.lang()));
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                pending.resolve(ApprovalDecision::Rejected);
                app.toast(NoticeLevel::Info, t(DLG_REJECTED, app.lang()));
            }
            _ => app.dialog = Some(Dialog::Approval(pending)),
        },
        other => {
            app.dialog = Some(other);
            dialog_edit(app, key);
        }
    }
}

/// Route a key to the editable dialog (UART settings, AI configuration).
fn dialog_edit(app: &mut App, key: KeyEvent) {
    if key.code == KeyCode::Esc {
        app.dialog = None;
        return;
    }
    if matches!(app.dialog, Some(Dialog::Settings(_))) {
        super::agent_settings::handle_key(app, key);
        return;
    }
    uart_edit(app, key);
}

fn uart_edit(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let apply = matches!(key.code, KeyCode::Enter) || (ctrl && key.code == KeyCode::Char('s'));
    if !apply {
        let Some(Dialog::Uart { field, error, .. }) = &mut app.dialog else {
            return;
        };
        *error = None;
        match key.code {
            KeyCode::Char(c) if !ctrl => field.insert_char(c),
            KeyCode::Backspace => field.backspace(),
            KeyCode::Delete => field.delete(),
            KeyCode::Left => field.left(),
            KeyCode::Right => field.right(),
            KeyCode::Home => field.home(),
            KeyCode::End => field.end(),
            _ => {}
        }
        return;
    }

    // Enter / Ctrl+S: validate the spec and push it to the bridge.
    let lang = app.lang();
    let spec = match &app.dialog {
        Some(Dialog::Uart { field, .. }) => field.text.clone(),
        _ => return,
    };
    match crate::protocol::validate::normalize_uart_spec(&spec) {
        Ok(spec) => {
            let rx = app.session.request_mgmt(format!("@u={spec}"), None);
            if let Some(Dialog::Uart {
                status,
                error,
                reply,
                ..
            }) = &mut app.dialog
            {
                *status = Some(tr!(t(DLG_UART_SETTING, lang), spec));
                *error = None;
                *reply = Some(rx);
            }
        }
        Err(message) => {
            if let Some(Dialog::Uart { error, .. }) = &mut app.dialog {
                *error = Some(message);
            }
        }
    }
}

/// Drain the in-flight `@u?` / `@u=` reply of the UART dialog (once per tick).
pub fn poll(app: &mut App) {
    let lang = app.lang();
    let received = match &mut app.dialog {
        Some(Dialog::Uart {
            reply: Some(rx), ..
        }) => match rx.try_recv() {
            Ok(result) => Some(Ok(result)),
            Err(oneshot::error::TryRecvError::Empty) => None,
            Err(err) => Some(Err(err)),
        },
        _ => None,
    };
    let Some(received) = received else { return };
    let Some(Dialog::Uart {
        field,
        status,
        error,
        reply,
    }) = &mut app.dialog
    else {
        return;
    };
    *reply = None;
    let outcome = match received {
        Ok(outcome) => outcome,
        Err(oneshot::error::TryRecvError::Closed) => {
            *error = Some(t(DLG_UART_CLOSED, lang).to_string());
            return;
        }
        Err(_) => return,
    };
    match outcome {
        Ok(mgmt) => {
            let mut text = String::new();
            for line in mgmt.lines.iter().chain(mgmt.events.iter()) {
                text.push_str(line);
                text.push('\n');
            }
            match parse_uart_reply(&text) {
                Some(settings) => {
                    field.set(settings.to_string());
                    *status = Some(tr!(t(DLG_UART_APPLIED, lang), settings));
                    *error = None;
                }
                None if mgmt.ok => {
                    *status = Some(super::replies::redact_secrets(text.trim()).to_string());
                }
                None => {
                    *error = Some(super::replies::redact_secrets(text.trim()).to_string());
                }
            }
        }
        Err(message) => *error = Some(super::replies::redact_secrets(&message)),
    }
}

/// Open the UART dialog and query the current settings (`@u?`).
pub fn open_uart(app: &mut App) {
    let lang = app.lang();
    if !app.ble_connected() {
        app.toast(NoticeLevel::Warn, t(DLG_UART_NEED_BLE, lang));
        return;
    }
    let rx = app.session.request_mgmt("@u?".to_string(), None);
    app.dialog = Some(Dialog::Uart {
        field: TextField::new(DEFAULT_UART),
        status: Some(t(DLG_UART_QUERYING, lang).to_string()),
        error: None,
        reply: Some(rx),
    });
}

/// Run a confirmed action.
pub fn confirm(app: &mut App, kind: ConfirmKind) {
    match kind {
        ConfirmKind::Reboot => app.send_text("reboot\n"),
        ConfirmKind::Disconnect => app.session.disconnect(),
        ConfirmKind::Quit => app.quit = true,
    }
}

/// Ctrl+Q: quit right away when idle, ask first while connected
/// (`confirmQuit` parity of the web client).
pub fn request_quit(app: &mut App) {
    let lang = app.lang();
    app.palette = None;
    if app.connected() {
        app.dialog = Some(Dialog::Confirm {
            kind: ConfirmKind::Quit,
            title: t(DLG_TITLE_QUIT, lang).to_string(),
            message: t(DLG_QUIT_MSG, lang).to_string(),
        });
    } else {
        app.quit = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmation_strings_match_the_web_client() {
        assert_eq!(REBOOT_CONFIRM, "Reboot the connected device now?");
        assert_eq!(QUIT_CONFIRM, "Quit the TUI now?");
        assert_eq!(
            DISCONNECT_CONFIRM,
            "Disconnect from the current device now?"
        );
        assert_eq!(DEFAULT_UART, "115200,8,n,1,n");
    }

    #[test]
    fn help_lists_the_contract_global_keys() {
        let text: String = help_lines(80, Lang::En)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        for key in [
            "Ctrl+P",
            "F1",
            "F2",
            "F3",
            "F4",
            "F5",
            "Ctrl+Shift+K",
            "Ctrl+Shift+M",
            "Ctrl+Shift+S",
            "Ctrl+Shift+N",
            "Ctrl/Alt+Enter",
            "Ctrl+L",
            "Ctrl+Q",
            "Ctrl+Up",
            "Quick send",
        ] {
            assert!(text.contains(key), "help overlay is missing {key}");
        }
    }

    /// The help is longer than the box a 24-row terminal (the common default)
    /// can draw: it used to clip the tail off with no way to read it. The
    /// limit below mirrors `draw_dialog`'s box maths exactly, so the arrows
    /// stop where the painting stops.
    #[test]
    fn the_help_scrolls_when_the_terminal_is_short() {
        assert_eq!(scroll_limit(24, 40), 20, "24 rows -> box 22 -> inner 20");
        assert_eq!(scroll_limit(60, 40), 0, "a tall terminal shows everything");
        assert_eq!(scroll_limit(10, 40), 34, "tiny frame -> inner 6");

        let limit = scroll_limit(24, help_lines(80, Lang::En).len());
        assert!(
            limit > 0,
            "the help must be scrollable on an 80x24 terminal"
        );

        assert_eq!(next_scroll(limit, limit, KeyCode::End), Some(limit));
        assert_eq!(next_scroll(limit, limit, KeyCode::Up), Some(limit - 1));
        assert_eq!(next_scroll(limit, limit, KeyCode::Down), Some(limit));
        assert_eq!(next_scroll(0, limit, KeyCode::Down), Some(1));
        assert_eq!(next_scroll(5, limit, KeyCode::PageUp), Some(0));
        assert_eq!(
            next_scroll(0, limit, KeyCode::PageDown),
            Some(10.min(limit))
        );
        assert_eq!(next_scroll(5, limit, KeyCode::Home), Some(0));
        assert!(
            next_scroll(5, limit, KeyCode::Char('x')).is_none(),
            "ordinary keys must not move the scroll"
        );
    }

    #[test]
    fn approval_cards_cover_all_three_kinds() {
        let kinds = [
            ApprovalKind::RunCommand {
                command: "reboot".to_string(),
                mode: crate::agent::ExecMode::Auto,
            },
            ApprovalKind::SendInput {
                payload: "ls\n".to_string(),
            },
            ApprovalKind::AccessoryChange {
                summary: "set uart".to_string(),
                command: "@u=115200,8,n,1,n".to_string(),
            },
        ];
        for kind in kinds {
            let request = crate::agent::ApprovalRequest {
                id: 1,
                kind,
                question: String::new(),
            };
            let (tx, _rx) = oneshot::channel();
            let pending = PendingApproval { request, tx };
            let lines = approval_lines(&pending, 60, crate::tui::i18n::Lang::En);
            let text: String = lines
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("approve"), "missing approve hint:\n{text}");
            assert!(text.contains("reject"), "missing reject hint:\n{text}");
        }
    }

    fn overlay_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// English wraps at spaces exactly as it always did.
    #[test]
    fn wrap_line_keeps_the_english_word_breaks() {
        let joined = overlay_text(&wrap_line("a bb ccc dddd", 5));
        assert_eq!(joined, "a bb\nccc\ndddd");
        assert_eq!(overlay_text(&wrap_line("", 10)), "");
        assert_eq!(
            overlay_text(&wrap_line("Enter an HTTP(S) API base URL", 12)),
            "Enter an\nHTTP(S) API\nbase URL"
        );
    }

    /// Chinese has no spaces to break at: an over-long run has to be cut, or
    /// the dialog box (sized from the line count) clips the tail away.
    #[test]
    fn wrap_line_cuts_runs_that_do_not_fit() {
        let text = "配置已保存到本设备，配置已保存到本设备。";
        let lines = wrap_line(text, 20);
        assert!(lines.len() > 1, "a 80-column line must not stay on one row");
        for line in &lines {
            let width: usize = line
                .spans
                .iter()
                .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            assert!(width <= 20, "row wider than the box: {width}");
        }
        let joined = overlay_text(&lines);
        assert_eq!(
            joined.chars().filter(|c| !c.is_whitespace()).count(),
            text.chars().count(),
            "no character is lost while cutting"
        );
    }

    /// The ellipsis sits *inside* the budget and a wide glyph is never split:
    /// whatever comes out still has to leave room for the columns around it.
    #[test]
    fn clip_columns_never_exceeds_the_budget() {
        assert_eq!(clip_columns("short value", 12), "short value");
        assert_eq!(clip_columns("abcdef", 6), "abcdef");
        assert_eq!(clip_columns("abcdef", 4), "abc…");
        // Two columns per glyph: the cut lands on a glyph boundary, never in
        // the middle of one.
        assert_eq!(clip_columns("中文测试", 5), "中文…");
        assert_eq!(clip_columns("中文测试", 4), "中…");
        assert_eq!(clip_columns("中文", 1), "…");
        assert_eq!(clip_columns("anything", 0), "");

        for budget in 0..24 {
            for text in ["", "abc", "中文测试字符", "mix中文abc"] {
                let clipped = clip_columns(text, budget);
                let width = unicode_width::UnicodeWidthStr::width(clipped.as_str());
                assert!(
                    width <= budget,
                    "{text:?} clipped to {budget} columns rendered {width}: {clipped:?}"
                );
            }
        }
    }

    /// Both languages of every dialog message carry text and differ.
    #[test]
    fn every_dlg_message_is_translated() {
        super::super::i18n::assert_bilingual(ALL);
        assert!(ALL.len() >= 45, "dialogs alone carry 45 messages");
    }

    /// The English overlay stays byte for byte identical (key column included)
    /// while Chinese reaches the same rows.
    #[test]
    fn the_help_follows_the_language() {
        assert_eq!(DLG_HELP_KEY_QUICK[0], "Quick send");
        assert_eq!(DLG_HELP_KEY_SIDEBAR[0], "↑↓ in sidebar");
        assert_eq!(
            QUIT_CONFIRM, DLG_QUIT_MSG[0],
            "the parity literal is the english half"
        );

        let en = overlay_text(&help_lines(80, Lang::En));
        assert!(en.contains("Quick send"), "{en}");
        assert!(en.contains("↑↓ in sidebar"), "{en}");
        assert!(
            en.contains("Command palette (every action, searchable)"),
            "{en}"
        );
        assert!(
            en.contains("Byte sequences match web/terminal_keys.js."),
            "{en}"
        );

        let zh = overlay_text(&help_lines(80, Lang::Zh));
        assert!(zh.contains("快捷发送"), "{zh}");
        assert!(zh.contains("侧栏中的 ↑↓"), "{zh}");
        assert!(zh.contains("命令面板（全部动作，可搜索）"), "{zh}");
        assert!(!zh.contains("Command palette"), "{zh}");
        assert!(
            zh.contains("web/terminal_keys.js"),
            "file paths stay verbatim:\n{zh}"
        );

        // The key column still pads to the same 16 columns in both languages.
        for lines in [help_lines(80, Lang::En), help_lines(80, Lang::Zh)] {
            let first = overlay_text(&lines[1..2]);
            assert!(first.starts_with("Ctrl+P"), "{first}");
            assert!(
                first.contains("Command palette") || first.contains("命令面板"),
                "{first}"
            );
        }
    }

    #[test]
    fn dialog_titles_follow_the_language() {
        let help = Dialog::Help(0);
        assert_eq!(help.title(), "Keyboard help");
        assert_eq!(help.title_lang(Lang::En), "Keyboard help");
        assert_eq!(help.title_lang(Lang::Zh), "键盘帮助");
        assert_eq!(Dialog::Notices(0).title_lang(Lang::Zh), "通知");
        let confirm = Dialog::Confirm {
            kind: ConfirmKind::Quit,
            title: "退出".to_string(),
            message: String::new(),
        };
        assert_eq!(confirm.title(), "Confirm");
        assert_eq!(confirm.title_lang(Lang::Zh), "确认");
    }
}
