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
    DLG_TITLE_CHUNK => "BLE write chunk", "BLE 写入分块";
    DLG_TITLE_SETTINGS => "AI configuration", "AI 配置";
    DLG_TITLE_HELP => "Keyboard help", "键盘帮助";
    DLG_TITLE_NOTICES => "Notices", "通知";
    DLG_TITLE_DEVICES => "Select a device", "选择设备";
    DLG_TITLE_FALLBACK => "Dialog", "对话框";

    // Device picker. The web hands `switchDeviceButton` to
    // `connect({ chooseDevice: true })`, which calls `requestDevice()` and
    // lets the browser scan *and* list; a terminal has to do both itself.
    DLG_DEVICES_SCANNING => "scanning for Linkr devices…", "正在搜索 Linkr 设备…";
    DLG_DEVICES_EMPTY => "No Linkr device found.", "没有发现 Linkr 设备。";
    DLG_DEVICES_KEYS => "↑↓ choose · Enter connect · Esc cancel",
        "↑↓ 选择 · Enter 连接 · Esc 取消";

    // Confirmation body (`sidebar.rs` carries the reboot / disconnect texts).
    DLG_QUIT_MSG => "Quit the TUI now?", "立即退出 TUI？";
    DLG_QUIT_WAIT_APPROVAL => "Answer the pending request first.",
        "请先处理待确认的请求。";
    DLG_Y_CONFIRM => " confirm · ", " 确认 · ";
    DLG_ESC_CANCEL => " cancel", " 取消";
    // An action with nowhere to go: the web keeps the presets and the key bar
    // disabled until `setConnected()`, and `send_text`'s gate would swallow
    // the command without a word — which for `reboot` reads as success.
    DLG_NOT_SENT => "Not connected — nothing was sent.", "未连接——没有发送任何内容。";

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

    // Chunk size dialog (`linkr-chunk`, WEB_UX_SPEC section 9.1). The range
    // is the CLI's own `--ble-write-size` one, widened at the bottom by 0 =
    // "ask the link what it can take" — the web's `20` is what that answer
    // usually is.
    DLG_CHUNK_PROMPT => "Bytes per BLE write (0–244, 0 = auto)",
        "每次 BLE 写入的字节数（0–244，0 ＝ 自动）";
    DLG_CHUNK_KEYS => "Enter save · Esc close", "Enter 保存 · Esc 关闭";
    DLG_CHUNK_RANGE => "BLE write size must be between 0 and 244 (0 = auto)",
        "BLE 写入分块必须在 0–244 之间（0 ＝ 自动）";
    DLG_CHUNK_SAVED => "BLE write chunk size: {}", "BLE 写入分块大小：{}";

    // Cheat Sheet card (WEB_UX_SPEC section 4): the title, the summary of the
    // open `<details>`, the four group titles, the tip and the key line. The
    // groups and the tip are the web's own strings; the key line replaces
    // "click a command" with what a terminal actually offers.
    DLG_TITLE_CHEAT => "Cheat Sheet", "速查参考";
    DLG_CHEAT_LINUX => "Linux Commands", "Linux 命令";
    DLG_CHEAT_GRP_FILES => "Files & Dirs", "文件与目录";
    DLG_CHEAT_GRP_SYS => "System", "系统信息";
    DLG_CHEAT_GRP_NET => "Network", "网络";
    DLG_CHEAT_GRP_PERM => "Permissions & Processes", "权限与进程";
    DLG_CHEAT_HINT => "Tip: the highlighted command is sent when you press Enter.",
        "提示：按 Enter 发送高亮的命令。";
    DLG_CHEAT_KEYS => "↑↓ choose · Enter send · Esc close",
        "↑↓ 选择 · Enter 发送 · Esc 关闭";

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
    DLG_HELP_TRANSFER => "Transfer view (precheck, then send / receive)",
        "传输视图（先预检，再发送 / 接收）";
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
    DLG_HELP_PASTE => "Paste the clipboard (focused field, else the device)",
        "粘贴剪贴板（当前输入框，否则发给设备）";
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
        message: String,
    },
    Approval(Box<PendingApproval>),
    Uart {
        field: TextField,
        status: Option<String>,
        error: Option<String>,
        reply: Option<oneshot::Receiver<Result<crate::protocol::MgmtReply, String>>>,
    },
    /// `linkr-chunk`: one number, typed. Unlike [`Dialog::Uart`] it needs no
    /// link to open — the value is read at the next dial, so it can be set
    /// while disconnected, which is when somebody usually discovers they need
    /// to change it.
    Chunk {
        field: TextField,
        error: Option<String>,
    },
    /// The web's `#cheatList`: four groups of Linux commands with a cursor
    /// over them and Enter to send. `selected` walks the commands only (the
    /// group titles are headings, not rows); `scroll` is the body offset the
    /// painter applies, kept in step with the cursor so a short terminal
    /// still shows the line it is pointing at.
    Cheat {
        selected: usize,
        scroll: u16,
    },
    Settings(AgentSettingsState),
    /// Scroll offset of the body: the help is longer than a short terminal
    /// can show, so the overlay scrolls instead of clipping the tail off.
    Help(u16),
    Notices(u16),
    /// The scan behind the sidebar's "Switch device" entry. `scanning` stays
    /// true until the oneshot answers, so the box opens at once instead of
    /// leaving the user staring at a radio that is quietly working.
    Devices {
        items: Vec<crate::transport::DiscoveredDevice>,
        selected: usize,
        scanning: bool,
    },
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
            Dialog::Chunk { .. } => t(DLG_TITLE_CHUNK, lang),
            Dialog::Cheat { .. } => t(DLG_TITLE_CHEAT, lang),
            Dialog::Settings(_) => t(DLG_TITLE_SETTINGS, lang),
            Dialog::Help(_) => t(DLG_TITLE_HELP, lang),
            Dialog::Notices(_) => t(DLG_TITLE_NOTICES, lang),
            Dialog::Devices { .. } => t(DLG_TITLE_DEVICES, lang),
        }
    }

    /// Body scroll offset of a scrollable overlay (0 for the others).
    pub fn scroll_offset(&self) -> u16 {
        match self {
            Dialog::Help(scroll) | Dialog::Notices(scroll) => *scroll,
            Dialog::Cheat { scroll, .. } => *scroll,
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

/// Body of the two single-field boxes: prompt, `> ` field, an optional error
/// in red, an optional status in green, then the key hint. The UART box and
/// the chunk box are this same shape, so they share one renderer — a colour
/// or spacing tweak then reaches both instead of leaving them to drift apart.
fn field_lines(
    prompt: Entry,
    field: &TextField,
    error: &Option<String>,
    status: &Option<String>,
    keys: Entry,
    width: u16,
    lang: Lang,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(t(prompt, lang)),
        Line::from(""),
        Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Cyan)),
            Span::styled(
                field.as_str().to_string(),
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
        t(keys, lang),
        Style::default().fg(Color::DarkGray),
    )));
    lines
}

/// One row of the web's `#cheatList`: the literal command and its en/zh
/// description (`web/app.js:863` `CHEATS`).
///
/// The commands go out **verbatim, placeholders included** — `cd <dir>` is
/// sent as `cd <dir>`, because that is what the web's click handler does with
/// `data-cmd` (`web/app.js:3537`). Mirroring the string beats "fixing" it:
/// the two clients must not disagree about what a chip sends.
const CHEAT_GROUPS: &[(Entry, &[(&str, Entry)])] = &[
    (
        DLG_CHEAT_GRP_FILES,
        &[
            ("ls -l", ["List in long format", "详细列表"]),
            ("cd <dir>", ["Change directory", "切换目录"]),
            ("pwd", ["Print working directory", "显示当前路径"]),
            ("mkdir <dir>", ["Make directory", "创建目录"]),
            ("cp -r a b", ["Copy recursively", "递归复制"]),
            ("mv a b", ["Move / rename", "移动或重命名"]),
            ("rm -rf <dir>", ["Force remove", "强制删除"]),
            ("cat <file>", ["Show file content", "查看文件内容"]),
            ("grep \"x\" <f>", ["Search text", "搜索文本"]),
            ("find . -name \"*.c\"", ["Find files", "查找文件"]),
        ],
    ),
    (
        DLG_CHEAT_GRP_SYS,
        &[
            ("uname -a", ["Kernel info", "内核信息"]),
            ("df -h", ["Disk usage", "磁盘使用"]),
            ("free -h", ["Memory usage", "内存使用"]),
            ("top", ["Process monitor", "进程监控"]),
            ("uptime", ["System uptime", "运行时长"]),
        ],
    ),
    (
        DLG_CHEAT_GRP_NET,
        &[
            ("ip a", ["Network interfaces", "网络接口"]),
            ("ping <host>", ["Ping a host", "连通测试"]),
            ("ssh u@host", ["Remote login", "远程登录"]),
            ("scp a u@h:", ["Secure copy", "安全拷贝"]),
            ("curl -I <url>", ["Fetch headers", "请求响应头"]),
        ],
    ),
    (
        DLG_CHEAT_GRP_PERM,
        &[
            ("chmod 755 <f>", ["Change mode", "修改权限"]),
            ("chown u:g <f>", ["Change owner", "修改属主"]),
            ("ps aux", ["List processes", "进程列表"]),
            ("kill -9 <pid>", ["Kill process", "终止进程"]),
            ("sudo <cmd>", ["Run as root", "提权执行"]),
        ],
    ),
];

/// How many commands the cursor can point at (the group titles are headings).
fn cheat_count() -> usize {
    CHEAT_GROUPS.iter().map(|(_, items)| items.len()).sum()
}

/// The `index`th command, or `None` past the end.
fn cheat_command(index: usize) -> Option<&'static str> {
    CHEAT_GROUPS
        .iter()
        .flat_map(|(_, items)| items.iter())
        .map(|(cmd, _)| *cmd)
        .nth(index)
}

/// Body line the `selected` command is painted on. The key handler moves the
/// scroll from this number, so it has to be the same arithmetic the renderer
/// uses or the cursor walks off the visible window.
fn cheat_line_of(selected: usize) -> usize {
    let mut line = 2; // "Linux Commands" summary, then a blank
    let mut index = 0;
    for (_, items) in CHEAT_GROUPS {
        if selected < index + items.len() {
            return line + 1 + (selected - index);
        }
        line += 1 + items.len() + 1; // group title, its items, trailing blank
        index += items.len();
    }
    line
}

/// Body of the cheat sheet: the open `<details>` summary, then every group and
/// every command, with `▸` on the one the cursor is on.
fn cheat_lines(selected: usize, width: u16, lang: Lang) -> Vec<Line<'static>> {
    // Two columns: the command, then the description. The command column is
    // as wide as the widest command but never eats the room the descriptions
    // need, so the second column starts at the same place in every group.
    let widest = CHEAT_GROUPS
        .iter()
        .flat_map(|(_, items)| items.iter())
        .map(|(cmd, _)| cmd.chars().count())
        .max()
        .unwrap_or(0);
    let cmd_col = widest.min((width as usize).saturating_sub(24).max(8));
    let word = Style::default().add_modifier(Modifier::BOLD);

    let mut lines = vec![
        Line::from(Span::styled(
            t(DLG_CHEAT_LINUX, lang),
            word.fg(Color::Yellow),
        )),
        Line::from(""),
    ];
    let mut index = 0;
    for (group, items) in CHEAT_GROUPS {
        lines.push(Line::from(Span::styled(
            t(*group, lang),
            word.fg(Color::Cyan),
        )));
        for (cmd, desc) in items.iter() {
            let here = index == selected;
            index += 1;
            lines.push(Line::from(vec![
                Span::styled(
                    if here { "▸ " } else { "  " },
                    Style::default().fg(if here { Color::Green } else { Color::DarkGray }),
                ),
                Span::styled(
                    format!("{cmd:<cmd_col$}"),
                    Style::default().fg(if here { Color::White } else { Color::Gray }),
                ),
                Span::styled("  ", Style::default()),
                Span::styled(
                    desc[usize::from(lang == Lang::Zh)],
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        t(DLG_CHEAT_HINT, lang),
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled(
        t(DLG_CHEAT_KEYS, lang),
        Style::default().fg(Color::DarkGray),
    )));
    lines
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
        } => field_lines(
            DLG_UART_PROMPT,
            field,
            error,
            status,
            DLG_UART_KEYS,
            width,
            lang,
        ),
        Dialog::Chunk { field, error } => field_lines(
            DLG_CHUNK_PROMPT,
            field,
            error,
            &None,
            DLG_CHUNK_KEYS,
            width,
            lang,
        ),
        Dialog::Cheat { selected, .. } => cheat_lines(*selected, width, lang),
        Dialog::Settings(state) => super::agent_settings::render_lines(state, width, lang),
        Dialog::Devices {
            items,
            selected,
            scanning,
        } => device_lines(items, *selected, *scanning, width, lang),
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

/// Body of the device picker. One row per discovered peripheral, printed as
/// the same `name · address · rssi` triple the CLI's `--scan` table shows so
/// the two listings can be read against each other, with `▸` on the row the
/// arrow keys are on (the sidebar draws its cursor the same way).
fn device_lines(
    items: &[crate::transport::DiscoveredDevice],
    selected: usize,
    scanning: bool,
    width: u16,
    lang: Lang,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if items.is_empty() {
        let (text, color) = if scanning {
            (t(DLG_DEVICES_SCANNING, lang), Color::DarkGray)
        } else {
            (t(DLG_DEVICES_EMPTY, lang), Color::LightYellow)
        };
        lines.push(Line::from(Span::styled(
            text.to_string(),
            Style::default().fg(color),
        )));
    } else {
        for (index, device) in items.iter().enumerate() {
            let is_sel = index == selected;
            let name = device.name.as_deref().unwrap_or("(unknown)");
            let rssi = device
                .rssi
                .map(|v| format!("  {v} dBm"))
                .unwrap_or_default();
            // `▸ ` + name + `  ` + address + rssi has to fit the overlay, so the
            // name is the part that gives way — the address is what is dialled.
            let room = (width as usize)
                .saturating_sub(2 + 2 + device.address.len() + rssi.chars().count())
                .max(1);
            let name = clip_name(name, room);
            lines.push(Line::from(vec![
                Span::styled(
                    if is_sel { "▸ " } else { "  " },
                    Style::default().fg(Color::Green),
                ),
                Span::styled(
                    name,
                    Style::default().fg(if is_sel { Color::White } else { Color::Gray }),
                ),
                Span::styled(
                    format!("  {}", device.address),
                    Style::default().fg(if is_sel {
                        Color::White
                    } else {
                        Color::DarkGray
                    }),
                ),
                Span::styled(rssi, Style::default().fg(Color::DarkGray)),
            ]));
        }
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        t(DLG_DEVICES_KEYS, lang).to_string(),
        Style::default().fg(Color::DarkGray),
    )));
    lines
}

/// Truncate to `room` *columns*, marking the cut with `…` (a name that lost
/// its tail silently would be exactly the "name is not complete" complaint
/// again). `clip_columns` measures in terminal columns — a CJK device name is
/// two columns per glyph — where the old `chars().take` measured characters
/// and let a Chinese name overrun the row by up to `room` columns.
fn clip_name(name: &str, room: usize) -> String {
    clip_columns(name, room)
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
    const ROWS: [Row; 29] = [
        Row("Ctrl+P", DLG_HELP_PALETTE),
        Row("F1", DLG_HELP_THIS),
        Row("F2", DLG_HELP_TERMINAL),
        Row("F3", DLG_HELP_DIAG),
        Row("F4", DLG_HELP_NETWORK),
        Row("F5", DLG_HELP_ASSISTANT),
        Row("F6", DLG_HELP_TRANSFER),
        Row("Ctrl+Shift+K", DLG_HELP_FOCUS_COMPOSER),
        Row("Ctrl+Shift+M", DLG_HELP_MODE),
        Row("Ctrl+Shift+S", DLG_HELP_CONFIG),
        Row("Ctrl+Shift+N", DLG_HELP_NEW_CHAT),
        Row("Alt+Enter", DLG_HELP_SEND),
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
        Row("Ctrl+Shift+V", DLG_HELP_PASTE),
        Row("Enter", DLG_HELP_ENTER),
        Row("Tab / Shift+Tab", DLG_HELP_TAB),
        Row("F7..F12", DLG_HELP_FKEYS),
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
    // The cheat sheet is a list with a cursor, not a wall of text: ↑↓ move
    // the cursor and the body scrolls to keep it on screen, Enter sends the
    // highlighted command, Esc closes.
    if matches!(app.dialog, Some(Dialog::Cheat { .. })) {
        cheat_edit(app, key);
        return;
    }
    let Some(dialog) = app.dialog.take() else {
        return;
    };
    match dialog {
        Dialog::Confirm { kind, message } => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                app.dialog_return = None;
                confirm(app, kind);
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                // Declining the ask gives back whatever `request_quit`
                // displaced instead of leaving the user with nothing.
                app.dialog = app.dialog_return.take().map(|dialog| *dialog);
            }
            _ => app.dialog = Some(Dialog::Confirm { kind, message }),
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
        Dialog::Devices {
            items,
            mut selected,
            scanning,
        } => {
            // Esc drops the box; the pending scan, if any, is abandoned by
            // `poll_scan` when its oneshot finally lands.
            if key.code == KeyCode::Esc {
                return;
            }
            // Still sweeping the air: there is nothing to point at yet, and
            // swallowing the keys instead of swallowing the result later is
            // what keeps the list from appearing under a moved cursor.
            if scanning {
                app.dialog = Some(Dialog::Devices {
                    items,
                    selected,
                    scanning,
                });
                return;
            }
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    if selected + 1 < items.len() {
                        selected += 1;
                    }
                }
                KeyCode::Home => selected = 0,
                KeyCode::End => selected = items.len().saturating_sub(1),
                KeyCode::Enter => {
                    // Dial the highlighted row: the address is what gets
                    // remembered, and the real advertised name replaces
                    // whatever prefix the form was holding (so the sidebar
                    // stops showing a half name).
                    if let Some(device) = items.get(selected).cloned() {
                        super::connect::pick(app, device);
                        return;
                    }
                }
                _ => {}
            }
            app.dialog = Some(Dialog::Devices {
                items,
                selected,
                scanning,
            });
        }
        other => {
            app.dialog = Some(other);
            dialog_edit(app, key);
        }
    }
}

/// Route a key to the editable dialog (UART settings, chunk size, AI
/// configuration).
fn dialog_edit(app: &mut App, key: KeyEvent) {
    if key.code == KeyCode::Esc {
        app.dialog = None;
        return;
    }
    if matches!(app.dialog, Some(Dialog::Settings(_))) {
        super::agent_settings::handle_key(app, key);
        return;
    }
    if matches!(app.dialog, Some(Dialog::Chunk { .. })) {
        chunk_edit(app, key);
        return;
    }
    uart_edit(app, key);
}

/// Edit and save `linkr-chunk`. Only digits ever reach the field, so the one
/// thing left to object to is a number the input's own range forbids.
fn chunk_edit(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let apply = matches!(key.code, KeyCode::Enter) || (ctrl && key.code == KeyCode::Char('s'));
    if !apply {
        let Some(Dialog::Chunk { field, error }) = &mut app.dialog else {
            return;
        };
        *error = None;
        match key.code {
            KeyCode::Char(c) if !ctrl && c.is_ascii_digit() => field.insert_char(c),
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

    let lang = app.lang();
    let raw = match &app.dialog {
        Some(Dialog::Chunk { field, .. }) => field.as_str().trim().to_string(),
        _ => return,
    };
    // The field only ever holds digits, so `None` is the empty box and the
    // `filter` is the top of the input's own range.
    let Some(size) = raw
        .parse::<usize>()
        .ok()
        .filter(|size| *size <= super::settings::MAX_BLE_WRITE_SIZE)
    else {
        if let Some(Dialog::Chunk { error, .. }) = &mut app.dialog {
            *error = Some(t(DLG_CHUNK_RANGE, lang).to_string());
        }
        return;
    };

    app.settings.ble_write_size = size;
    let saved = super::settings::save(&app.settings);
    app.dialog = None;
    if let Err(err) = saved {
        app.toast(
            NoticeLevel::Warn,
            tr!(t(super::i18n::MSG_SAVE_SETTINGS, lang), err),
        );
        return;
    }
    app.toast(NoticeLevel::Info, tr!(t(DLG_CHUNK_SAVED, lang), size));
}

/// Open the chunk-size box with the value currently in force. No link is
/// needed: `connect::dial_options` reads the field at the next dial.
pub fn open_chunk(app: &mut App) {
    app.dialog = Some(Dialog::Chunk {
        field: TextField::new(app.settings.ble_write_size.to_string()),
        error: None,
    });
}

/// Open the cheat sheet on its first command.
pub fn open_cheat(app: &mut App) {
    app.dialog = Some(Dialog::Cheat {
        selected: 0,
        scroll: 0,
    });
}

/// Keys of the cheat sheet: a cursor over the commands, Enter to send.
fn cheat_edit(app: &mut App, key: KeyEvent) {
    if key.code == KeyCode::Esc {
        app.dialog = None;
        return;
    }
    let total = cheat_count();
    if total == 0 {
        return;
    }
    let last = total - 1;

    if key.code == KeyCode::Enter {
        let selected = match app.dialog.as_ref() {
            Some(Dialog::Cheat { selected, .. }) => *selected,
            _ => return,
        };
        let Some(cmd) = cheat_command(selected) else {
            return;
        };
        let lang = app.lang();
        // The web's chips are `disabled` until `setConnected()`, so a press
        // with no link has to answer here: running the command into
        // `send_text`'s gate would report nothing at all (the presets make the
        // same point in `sidebar.rs`).
        if !app.connected() {
            app.toast(NoticeLevel::Warn, t(DLG_NOT_SENT, lang).to_string());
            return;
        }
        // `data-cmd` verbatim, placeholders included, and the sheet stays open
        // so several commands can go out in a row — clicking twice in the web
        // does the same.
        app.send_text(&format!("{cmd}\n"));
        return;
    }

    let lang = app.lang();
    let next = match app.dialog.as_ref() {
        Some(Dialog::Cheat { selected, .. }) => match key.code {
            KeyCode::Up | KeyCode::Char('k') => selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => (*selected + 1).min(last),
            KeyCode::PageUp => selected.saturating_sub(10),
            KeyCode::PageDown => (*selected + 10).min(last),
            KeyCode::Home => 0,
            KeyCode::End => last,
            _ => return,
        },
        _ => return,
    };

    // Move the body so the line the cursor points at is inside the window
    // `draw_dialog` is about to paint — `scroll_limit` is that window's exact
    // mirror, so the scroll the keys pick is the one the painter applies.
    let lines = cheat_lines(next, DIALOG_WIDTH, lang).len();
    let limit = scroll_limit(app.screen_height, lines);
    let viewport = (lines as u16).saturating_sub(limit);
    let target = cheat_line_of(next) as u16;
    let Some(Dialog::Cheat { selected, scroll }) = &mut app.dialog else {
        return;
    };
    *selected = next;
    if target < *scroll {
        *scroll = target;
    } else if target + 1 > *scroll + viewport {
        *scroll = (target + 1).saturating_sub(viewport);
    }
    *scroll = (*scroll).min(limit);
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
        Some(Dialog::Uart { field, .. }) => field.as_str().to_string(),
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
        ConfirmKind::Reboot => {
            // The link can drop between the ask and the `y`. `send_text` then
            // refuses the payload behind its own gate — silently, because a
            // keystroke spamming "session gone" was the bug that gate fixed —
            // so the one-shot command has to say it did not go itself.
            if !app.connected() {
                app.toast(NoticeLevel::Warn, t(DLG_NOT_SENT, app.lang()).to_string());
                return;
            }
            app.send_text("reboot\n")
        }
        ConfirmKind::Disconnect => {
            // The break characters have to leave before the link does; after
            // it, `send_bytes` drops them and the target keeps running.
            super::transfer_view::abort_if_busy(app);
            app.session.disconnect()
        }
        ConfirmKind::Quit => {
            super::transfer_view::abort_if_busy(app);
            app.quit = true
        }
    }
}

/// Ctrl+Q: quit right away when idle, ask first while connected
/// (`confirmQuit` parity of the web client).
pub fn request_quit(app: &mut App) {
    let lang = app.lang();
    // A pending approval must not be displaced: dropping the box closes its
    // channel, and the agent reads a closed channel as a rejection
    // ([`PendingApproval`]) — a stray quit keystroke would decide the request
    // for the user without a keystroke on the request itself.
    if matches!(app.dialog, Some(Dialog::Approval(_))) {
        app.toast(NoticeLevel::Info, t(DLG_QUIT_WAIT_APPROVAL, lang));
        return;
    }
    if matches!(
        app.dialog,
        Some(Dialog::Confirm {
            kind: ConfirmKind::Quit,
            ..
        })
    ) {
        return;
    }
    app.palette = None;
    // Keep what was under the confirm: declining it must not cost the user a
    // half-typed AI configuration, an open UART dialog or a scan result list.
    app.dialog_return = app.dialog.take().map(Box::new);
    if app.connected() {
        app.dialog = Some(Dialog::Confirm {
            kind: ConfirmKind::Quit,
            message: t(DLG_QUIT_MSG, lang).to_string(),
        });
    } else {
        app.quit = true;
        app.dialog_return = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(address: &str, name: Option<&str>, rssi: i32) -> crate::transport::DiscoveredDevice {
        crate::transport::DiscoveredDevice {
            address: address.to_string(),
            name: name.map(str::to_string),
            rssi: Some(rssi),
        }
    }

    fn row_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// The picker is what "Switch device" opens: it has its own frame title,
    /// and the row the cursor sits on is marked the way the sidebar marks its
    /// own cursor, so the two cannot be confused for different widgets.
    #[test]
    fn the_picker_titles_itself_and_marks_the_cursor_row() {
        let picker = Dialog::Devices {
            items: Vec::new(),
            selected: 0,
            scanning: true,
        };
        assert_eq!(picker.title_lang(Lang::En), "Select a device");
        assert_eq!(picker.title_lang(Lang::Zh), "选择设备");

        let devices = vec![
            device("AA:1", Some("Linkr BLE UART"), -70),
            device("AA:2", Some("Linkr BLE UART-3"), -58),
        ];
        let lines = device_lines(&devices, 1, false, 80, Lang::En);
        assert!(row_text(&lines[0]).starts_with("  "), "{:?}", lines[0]);
        assert!(row_text(&lines[1]).starts_with("▸ "), "{:?}", lines[1]);
        // The board broadcasts `Linkr BLE UART-3`; a picker that dropped the
        // suffix would rebuild the very complaint it was written to fix.
        assert!(
            row_text(&lines[1]).contains("Linkr BLE UART-3"),
            "{:?}",
            lines[1]
        );
        assert!(row_text(&lines[1]).contains("AA:2"), "{:?}", lines[1]);
    }

    /// Before the radio answers the box says so, and afterwards an empty sweep
    /// says that too — never a blank overlay that looks like a hang.
    #[test]
    fn the_picker_reports_a_running_and_an_empty_sweep() {
        let running = overlay_text(&device_lines(&[], 0, true, 80, Lang::En));
        assert!(running.contains("scanning for Linkr devices"), "{running}");
        let empty = overlay_text(&device_lines(&[], 0, false, 80, Lang::Zh));
        assert!(empty.contains("没有发现 Linkr 设备"), "{empty}");
        // Both states keep the key legend, or the box is unoperable.
        for text in [running, empty] {
            assert!(text.contains("Esc"), "{text}");
        }
    }

    /// A name wider than the overlay is cut with `…`: silently losing the tail
    /// is how the `-3` went missing in the first place.
    #[test]
    fn a_name_wider_than_the_row_keeps_the_cut_visible() {
        assert_eq!(clip_name("Linkr BLE UART-3", 32), "Linkr BLE UART-3");
        // room 8 → seven columns of name plus the `…`, so the cut is visible
        // and the row never overflows into the address column.
        let clipped = clip_name("Linkr BLE UART-3", 8);
        assert_eq!(clipped.chars().count(), 8, "{clipped}");
        assert!(clipped.ends_with('…'), "{clipped}");
        assert!(clipped.starts_with("Linkr B"), "{clipped}");
    }

    /// A CJK device name is two columns per glyph: the same `room` has to cut
    /// *columns*, or a six-glyph name fills twelve columns of an eight-column
    /// field and overruns the address column beside it.
    #[test]
    fn a_cjk_name_is_cut_by_columns() {
        let clipped = clip_name("链路串口设备", 8);
        let width = unicode_width::UnicodeWidthStr::width(clipped.as_str());
        assert!(
            width <= 8,
            "{clipped:?} renders {width} columns in an 8-column room"
        );
        assert!(clipped.ends_with('…'), "{clipped:?}");
    }

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
            "F6",
            "Ctrl+Shift+K",
            "Ctrl+Shift+M",
            "Ctrl+Shift+S",
            "Ctrl+Shift+N",
            "Ctrl+Shift+V",
            "Alt+Enter",
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

    /// `Ctrl+Q` used to be handled before anything else and to overwrite
    /// `app.dialog`. Dropping a pending approval closes its channel, which the
    /// agent reads as a rejection (`PendingApproval::resolve` doc) — so an
    /// unrelated quit keystroke decided the request with no keystroke on it.
    #[test]
    fn ctrl_q_never_displaces_a_pending_approval() {
        let mut app = crate::tui::test_app();
        let request = ApprovalRequest {
            id: 1,
            kind: ApprovalKind::SendInput {
                payload: "ls\n".to_string(),
            },
            question: String::new(),
        };
        let (tx, _rx) = oneshot::channel();
        app.dialog = Some(Dialog::Approval(Box::new(PendingApproval { request, tx })));

        request_quit(&mut app);

        assert!(
            matches!(app.dialog, Some(Dialog::Approval(_))),
            "the request stays on screen"
        );
        assert!(!app.quit, "…and nothing quit behind it");
        assert!(
            app.notices
                .toasts
                .last()
                .is_some_and(|t| t.text.contains("pending request")),
            "the user is told why: {:?}",
            app.notices.toasts.last()
        );
    }

    /// Declining the quit ask has to hand back the box it displaced, not leave
    /// the user with a bare view and a lost half-typed form.
    #[test]
    fn declining_the_quit_ask_gives_back_the_dialog_it_displaced() {
        let mut app = crate::tui::test_app();
        app.dialog = Some(Dialog::Settings(AgentSettingsState::default()));

        request_quit(&mut app);
        assert!(matches!(
            app.dialog,
            Some(Dialog::Confirm {
                kind: ConfirmKind::Quit,
                ..
            })
        ));

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        );
        assert!(
            matches!(app.dialog, Some(Dialog::Settings(_))),
            "the settings dialog comes back: {:?}",
            app.dialog.is_some()
        );
        assert!(!app.quit);
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
    /// The link can drop between the ask and the `y`. `send_text` then
    /// refuses the payload behind its own gate — on purpose, since a
    /// keystroke per "session gone" was the bug that gate fixed — so the
    /// one-shot command has to be the one that says it did not go.
    #[test]
    fn confirming_a_reboot_with_the_link_gone_reports_that_nothing_was_sent() {
        let mut app = crate::tui::test_app();
        app.state = crate::event::ConnectionState::Disconnected;

        confirm(&mut app, ConfirmKind::Reboot);

        let toast = app.notices.toasts.last().expect("it explains itself");
        assert_eq!(toast.text, t(DLG_NOT_SENT, app.lang()));
    }

    /// Quit used to set `app.quit` and hand the transport straight to teardown,
    /// which hung up on a live `rz`/`sz`: the target was left with an orphan
    /// holding its console, and from the next screen nothing could free it.
    /// The break characters therefore go out while the link is still up.
    #[test]
    fn confirming_the_quit_ask_stops_a_transfer_in_flight() {
        let mut app = crate::tui::test_app();
        app.transfer.engine.phase = crate::transfer::Phase::Run {
            at: std::time::Instant::now(),
        };
        assert!(app.transfer.engine.busy());

        confirm(&mut app, ConfirmKind::Quit);

        assert!(app.quit, "the quit still happens");
        assert!(
            !app.transfer.engine.busy(),
            "the run is stopped first: {:?}",
            app.transfer.engine.outcome
        );
        assert_eq!(
            app.transfer.message,
            t(super::super::transfer_view::XFER_ABORTED, app.lang()).to_string()
        );
    }

    /// Disconnect is the same exit by another door (the sidebar's ask), so it
    /// takes the same stop: the two control characters are the only thing that
    /// can still reach the target after this.
    #[test]
    fn confirming_the_disconnect_ask_stops_a_transfer_in_flight() {
        let mut app = crate::tui::test_app();
        app.transfer.engine.phase = crate::transfer::Phase::Run {
            at: std::time::Instant::now(),
        };

        confirm(&mut app, ConfirmKind::Disconnect);

        assert!(!app.transfer.engine.busy());
        assert_eq!(
            app.transfer.engine.detail(),
            "Aborted.",
            "the same reason Esc leaves behind"
        );
    }

    /// The gate both exits share: an engine with nothing running must not come
    /// away wearing an abort it never had.
    #[test]
    fn an_idle_transfer_is_left_alone_on_the_way_out() {
        let mut app = crate::tui::test_app();
        let detail = app.transfer.engine.detail();

        confirm(&mut app, ConfirmKind::Quit);
        assert_eq!(app.transfer.engine.detail(), detail);

        confirm(&mut app, ConfirmKind::Disconnect);
        assert_eq!(app.transfer.engine.detail(), detail);
        assert!(app.transfer.message.is_empty());
    }

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

    /// `linkr-chunk` typed and saved: the number reaches the settings a dial
    /// reads and the box closes. It opens with no link, because the value is
    /// only spent at the next connect — which is when somebody finds out they
    /// needed a different one.
    #[test]
    fn the_chunk_box_saves_a_number_into_the_settings() {
        let mut app = crate::tui::test_app();
        // The UART box refuses to open without a link (`open_uart`); this one
        // must not, and that difference is the point.
        app.state = crate::event::ConnectionState::Disconnected;
        assert!(!app.connected(), "the premise: no link is needed");

        open_chunk(&mut app);
        assert_eq!(
            app.dialog.as_ref().map(Dialog::title),
            Some("BLE write chunk")
        );
        let Some(Dialog::Chunk { field, error }) = app.dialog.as_ref() else {
            panic!("the chunk box opened");
        };
        assert_eq!(field.as_str(), "0", "it starts on auto");
        assert!(error.is_none());

        for c in ['1', '8', '0'] {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            );
        }
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(app.dialog.is_none(), "Enter saves and closes");
        assert_eq!(app.settings.ble_write_size, 180);
        assert!(
            app.notices
                .toasts
                .last()
                .is_some_and(|toast| toast.text.ends_with("180")),
            "the new value is reported: {:?}",
            app.notices.toasts.last().map(|toast| toast.text.as_str())
        );
    }

    /// The range is the input's own (`0`–`244`): a longer number is refused
    /// *in* the box, leaving what was saved alone.
    #[test]
    fn the_chunk_box_refuses_a_number_the_input_cannot_hold() {
        let mut app = crate::tui::test_app();
        app.settings.ble_write_size = 64;
        open_chunk(&mut app);

        for _ in 0..2 {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            );
        }
        for c in "3000".chars() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            );
        }
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let Some(Dialog::Chunk { error, .. }) = app.dialog.as_ref() else {
            panic!("the box stays open to show why");
        };
        assert_eq!(error.as_deref(), Some(t(DLG_CHUNK_RANGE, Lang::En)));
        assert_eq!(app.settings.ble_write_size, 64, "nothing was saved");

        // Esc closes without touching the setting either.
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.dialog.is_none());
        assert_eq!(app.settings.ble_write_size, 64);
    }

    /// The card's contents are the web's, in the web's order: `#cheatList`
    /// as `web/app.js:863` builds it and WEB_UX_SPEC section 4 lists it. The
    /// descriptions are not in the `strings!` table (they are table data, not
    /// interface text), so this is where the missing-translation guard lives.
    #[test]
    fn the_cheat_sheet_lists_the_web_commands_in_order() {
        let groups: Vec<&str> = CHEAT_GROUPS.iter().map(|(g, _)| t(*g, Lang::En)).collect();
        assert_eq!(
            groups,
            [
                "Files & Dirs",
                "System",
                "Network",
                "Permissions & Processes"
            ]
        );
        assert_eq!(
            CHEAT_GROUPS
                .iter()
                .map(|(_, items)| items.len())
                .collect::<Vec<_>>(),
            [10, 5, 5, 5],
            "four groups, twenty-five commands"
        );
        assert_eq!(cheat_count(), 25);

        let commands: Vec<&str> = CHEAT_GROUPS
            .iter()
            .flat_map(|(_, items)| items.iter())
            .map(|(cmd, _)| *cmd)
            .collect();
        assert_eq!(
            commands,
            [
                "ls -l",
                "cd <dir>",
                "pwd",
                "mkdir <dir>",
                "cp -r a b",
                "mv a b",
                "rm -rf <dir>",
                "cat <file>",
                "grep \"x\" <f>",
                "find . -name \"*.c\"",
                "uname -a",
                "df -h",
                "free -h",
                "top",
                "uptime",
                "ip a",
                "ping <host>",
                "ssh u@host",
                "scp a u@h:",
                "curl -I <url>",
                "chmod 755 <f>",
                "chown u:g <f>",
                "ps aux",
                "kill -9 <pid>",
                "sudo <cmd>",
            ]
        );

        assert_eq!(cheat_command(0), Some("ls -l"));
        assert_eq!(cheat_command(24), Some("sudo <cmd>"));
        assert_eq!(
            cheat_command(25),
            None,
            "the cursor cannot point past the end"
        );

        for (_, items) in CHEAT_GROUPS.iter() {
            for (cmd, desc) in items.iter() {
                assert!(!desc[0].trim().is_empty(), "{cmd}: empty english");
                assert!(!desc[1].trim().is_empty(), "{cmd}: empty chinese");
                assert_ne!(desc[0], desc[1], "{cmd} was never translated");
            }
        }
    }

    /// Enter sends the highlighted command and leaves the sheet up, so a run
    /// of commands can go out one after another (clicking twice in the web
    /// does the same). The command is sent verbatim, `cd <dir>` and all.
    #[test]
    fn the_cheat_sheet_sends_the_highlighted_command_and_stays_open() {
        let mut app = crate::tui::test_app();
        app.settings.local_echo = true;
        open_cheat(&mut app);

        for _ in 0..2 {
            handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        assert_eq!(
            app.dialog.as_ref().map(|dialog| dialog.title()),
            Some("Cheat Sheet")
        );

        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(
            app.terminal.visible_text().contains("pwd"),
            "the third command of the first group went out: {:?}",
            app.terminal.visible_text()
        );
        assert!(
            app.dialog.is_some(),
            "the sheet stays open, unlike a one-shot prompt"
        );
        assert!(
            !app.notices
                .toasts
                .iter()
                .any(|toast| toast.text == t(DLG_NOT_SENT, Lang::En)),
            "it went out, so nothing refused it: {:?}",
            app.notices.toasts
        );
    }

    /// The web's chips are `disabled` until `setConnected()`; ours are only
    /// dimmed, so a press with no link has to *say* so instead of running
    /// into `send_text`'s silent gate.
    #[test]
    fn the_cheat_sheet_says_so_when_there_is_no_link() {
        let mut app = crate::tui::test_app();
        app.settings.local_echo = true;
        app.state = crate::event::ConnectionState::Disconnected;
        open_cheat(&mut app);

        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let toast = app.notices.toasts.last().expect("it explains itself");
        assert_eq!(toast.text, t(DLG_NOT_SENT, Lang::En));
        assert!(
            !app.terminal.visible_text().contains("ls -l"),
            "nothing was sent"
        );
        assert!(app.dialog.is_some(), "the sheet stays up to be read");
    }

    /// On a terminal too short for the whole card the body scrolls with the
    /// cursor: the line it points at is inside the window `draw_dialog` will
    /// paint, at the bottom of the list and back at the top.
    #[test]
    fn the_cheat_sheet_keeps_the_cursor_line_on_screen() {
        let mut app = crate::tui::test_app();
        app.screen_height = 16;
        open_cheat(&mut app);

        for _ in 0..25 {
            handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        let Some(Dialog::Cheat { selected, scroll }) = app.dialog.as_ref() else {
            panic!("the sheet is open");
        };
        assert_eq!(*selected, 24, "the cursor stops on the last command");
        let lines = cheat_lines(*selected, DIALOG_WIDTH, Lang::En).len();
        let limit = scroll_limit(app.screen_height, lines);
        let viewport = (lines as u16).saturating_sub(limit);
        let target = cheat_line_of(*selected) as u16;
        assert!(
            *scroll <= target && target < *scroll + viewport,
            "line {target} fell outside the window {scroll}..+{viewport}"
        );
        assert_eq!(
            *scroll + viewport - 1,
            target,
            "the last command sits at the bottom of a window that short"
        );

        for _ in 0..40 {
            handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        }
        let Some(Dialog::Cheat { selected, scroll }) = app.dialog.as_ref() else {
            panic!("the sheet is open");
        };
        assert_eq!(*selected, 0, "the cursor runs back to the first command");
        let target = cheat_line_of(*selected) as u16;
        assert!(
            *scroll <= target,
            "the first command must be in view, scroll {scroll}, line {target}"
        );

        handle_key(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(
            app.dialog
                .as_ref()
                .map(|dialog| matches!(dialog, Dialog::Cheat { selected: 24, .. })),
            Some(true),
            "End jumps to the last command"
        );
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.dialog.is_none(), "Esc closes the sheet");
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
            message: String::new(),
        };
        assert_eq!(confirm.title(), "Confirm");
        assert_eq!(confirm.title_lang(Lang::Zh), "确认");
    }
}
