//! Command palette (`Ctrl+P`) — every action of the TUI in one searchable
//! list (CONTRACTS.md section 5).
//!
//! The registry is a plain `&'static [Action]` so a unit test can assert its
//! completeness against the canonical id list without building an `App`.
//!
//! The labels (titles, categories, the toasts the actions raise) are
//! bilingual per WEB_UX_SPEC section 9 (`linkr-lang`); the action ids and the
//! key hints are protocol / key literals and stay identical in both
//! languages.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::i18n::{strings, t, tr, Entry, Lang, MSG_SAVE_SETTINGS};
use super::settings::TransportChoice;
use super::sidebar::MSG_TRANSPORT_LOCKED;
use super::state::{App, Focus, View};
use crate::event::NoticeLevel;

strings! {
    PAL_TITLE => " Command palette (Ctrl+P) ", " 命令面板（Ctrl+P） ";
    PAL_NO_MATCH => "No matching action.", "无匹配动作。";
    // Categories
    PAL_CAT_VIEW => "View", "视图";
    PAL_CAT_FOCUS => "Focus", "焦点";
    PAL_CAT_CONNECTION => "Connection", "连接";
    PAL_CAT_TERMINAL => "Terminal", "终端";
    PAL_CAT_DIAGNOSTICS => "Diagnostics", "诊断";
    PAL_CAT_NETWORK => "Network", "网络";
    PAL_CAT_ASSISTANT => "Assistant", "助手";
    PAL_CAT_APP => "App", "应用";
    // Views
    PAL_T_VIEW_TERMINAL => "Open terminal view", "打开终端视图";
    PAL_T_VIEW_DIAGNOSTICS => "Open diagnostics view", "打开诊断视图";
    PAL_T_VIEW_NETWORK => "Open network view", "打开网络视图";
    PAL_T_VIEW_ASSISTANT => "Open assistant view", "打开助手视图";
    // Focus
    PAL_T_FOCUS_SIDEBAR => "Focus the sidebar", "聚焦侧栏";
    PAL_T_FOCUS_TERMINAL => "Focus the terminal", "聚焦终端";
    PAL_T_FOCUS_ASSISTANT => "Focus the assistant composer", "聚焦助手输入框";
    // Connection
    PAL_T_CONNECT => "Connect", "连接";
    PAL_T_DISCONNECT => "Disconnect", "断开连接";
    PAL_T_TRANSPORT => "Toggle BLE / LAN transport", "切换 BLE / LAN 传输方式";
    PAL_T_UART => "UART settings…", "UART 设置…";
    // Terminal
    PAL_T_FONT_BIGGER => "Bigger font", "增大字号";
    PAL_T_FONT_SMALLER => "Smaller font", "减小字号";
    PAL_T_FONT_RESET => "Reset font size", "重置字号";
    PAL_T_AUTOSCROLL => "Toggle autoscroll", "切换自动滚动";
    PAL_T_ECHO => "Toggle local echo", "切换本地回显";
    PAL_T_ENTER_MODE => "Cycle Enter mode", "循环切换回车模式";
    PAL_T_CLEAR => "Clear the terminal", "清屏";
    PAL_T_SAVE_LOG => "Save log to file", "保存日志到文件";
    PAL_T_COPY => "Copy visible output (OSC 52)", "复制可见输出（OSC 52）";
    // Diagnostics / network
    PAL_T_DIAG_REFRESH => "Refresh diagnostics (@i?)", "刷新诊断（@i?）";
    PAL_T_WIFI_SCAN => "Scan WiFi networks", "扫描 WiFi 网络";
    PAL_T_WIFI_STATUS => "Query WiFi status", "查询 WiFi 状态";
    PAL_T_WEBDAV_STATUS => "Query WebDAV status", "查询 WebDAV 状态";
    // Assistant
    PAL_T_AGENT_ASK => "Ask the assistant", "向助手提问";
    PAL_T_AGENT_MODE => "Cycle execution mode", "循环切换执行模式";
    PAL_T_AGENT_SETTINGS => "AI configuration…", "AI 配置…";
    PAL_T_NEW_CHAT => "New chat", "新建对话";
    PAL_T_AGENT_STOP => "Stop the running turn", "停止当前轮次";
    PAL_T_EXPORT => "Export report", "导出报告";
    // App
    PAL_T_HELP => "Keyboard help", "键盘帮助";
    PAL_T_NOTICES => "Notice log", "通知记录";
    PAL_T_LANGUAGE => "Switch language", "切换界面语言";
    PAL_MSG_LANGUAGE => "Interface language: {}", "界面语言：{}";
    PAL_T_QUIT => "Quit the TUI", "退出 TUI";
    // Toasts and notices raised by the actions below
    PAL_MSG_ENTER_MODE => "Enter mode: {}", "回车模式：{}";
    PAL_MSG_SAVED_LOG => "Saved {} bytes to {}", "已保存 {} 字节到 {}";
    PAL_MSG_SAVE_FAILED => "Save failed: {}", "保存失败：{}";
    PAL_MSG_COPIED => "Copied {} characters (OSC 52).", "已复制 {} 个字符（OSC 52）。";
    PAL_MSG_COPY_FAILED => "Copy failed: {}", "复制失败：{}";
    PAL_MSG_BLE_DIAGNOSTICS => "Connect over BLE to read diagnostics.",
        "请先通过 BLE 连接再读取诊断。";
    PAL_MSG_NEW_CHAT => "New chat.", "新建对话。";
    PAL_MSG_EXPORT_EMPTY => "Nothing to export yet: no tasks or notes for this device.",
        "暂无可导出内容：此设备没有任务或笔记。";
    PAL_MSG_REPORT_WRITTEN => "Report written to {}", "报告已写入 {}";
    PAL_MSG_REPORT_FAILED => "Could not write the report: {}", "报告写入失败：{}";
}

/// One palette entry. `run` is a plain function so the table stays `const`.
///
/// The title and category are [`Entry`]s rather than bare strings: the id is
/// the protocol identifier and stays English in both languages, the labels
/// are interface text and follow `linkr-lang`.
pub struct Action {
    pub id: &'static str,
    pub title: Entry,
    pub category: Entry,
    pub shortcut: &'static str,
    pub run: fn(&mut App),
}

// --- action implementations --------------------------------------------------

fn focus_sidebar(app: &mut App) {
    app.focus = Focus::Sidebar;
}

fn view_terminal(app: &mut App) {
    app.set_view(View::Terminal);
}

fn view_diagnostics(app: &mut App) {
    app.set_view(View::Diagnostics);
}

fn view_network(app: &mut App) {
    app.set_view(View::Network);
}

fn view_assistant(app: &mut App) {
    app.set_view(View::Assistant);
}

fn focus_center(app: &mut App) {
    // The action promises the **terminal**, and focus alone is half of it:
    // `Focus::Center` hands the keys to whatever panel the view shows. Left in
    // the assistant view the keystrokes then land nowhere (its panel reads
    // `Focus::Assistant`, and the terminal branch below is skipped because the
    // view is not `Terminal`) — the keyboard went dead, so the user pressed a
    // key, saw nothing, and assumed the TUI had hung.
    app.set_view(View::Terminal);
    app.focus = Focus::Center;
}

fn focus_assistant(app: &mut App) {
    app.set_view(View::Assistant);
    app.focus = Focus::Assistant;
}

fn font_bigger(app: &mut App) {
    let next = (app.settings.font_size + 1).clamp(
        super::settings::MIN_FONT_SIZE,
        super::settings::MAX_FONT_SIZE,
    );
    if next != app.settings.font_size {
        app.settings.font_size = next;
        persist(app);
        app.force_redraw = true;
    }
}

fn font_smaller(app: &mut App) {
    let next = app.settings.font_size.saturating_sub(1).clamp(
        super::settings::MIN_FONT_SIZE,
        super::settings::MAX_FONT_SIZE,
    );
    if next != app.settings.font_size {
        app.settings.font_size = next;
        persist(app);
        app.force_redraw = true;
    }
}

fn font_reset(app: &mut App) {
    app.settings.font_size = super::settings::DEFAULT_FONT_SIZE;
    persist(app);
    app.force_redraw = true;
}

fn toggle_autoscroll(app: &mut App) {
    let on = !app.terminal.autoscroll;
    app.terminal.set_autoscroll(on);
    app.settings.autoscroll = on;
    persist(app);
}

fn toggle_echo(app: &mut App) {
    app.settings.local_echo = !app.settings.local_echo;
    persist(app);
}

fn cycle_enter(app: &mut App) {
    app.settings.enter_mode = app.settings.enter_mode.next();
    persist(app);
    let label = app.settings.enter_mode.label();
    app.toast(
        NoticeLevel::Info,
        tr!(t(PAL_MSG_ENTER_MODE, app.lang()), label),
    );
}

fn clear_terminal(app: &mut App) {
    app.terminal.clear();
}

fn save_log(app: &mut App) {
    let lang = app.lang();
    let name = super::terminal_view::TerminalPane::default_log_name();
    let path = std::path::PathBuf::from(&name);
    match app.terminal.save_log(&path) {
        Ok(bytes) => app.notices.push(
            NoticeLevel::Info,
            tr!(t(PAL_MSG_SAVED_LOG, lang), bytes, name),
        ),
        Err(err) => app
            .notices
            .push(NoticeLevel::Error, tr!(t(PAL_MSG_SAVE_FAILED, lang), err)),
    }
}

fn copy_visible(app: &mut App) {
    // One exit for every `OSC 52`: the mouse selection's release (`handle_mouse`)
    // goes through the same call, so both report the same toast and the same
    // cap.
    super::copy_to_host(app, &app.terminal.visible_text());
}

fn toggle_transport(app: &mut App) {
    if app.transport_locked() {
        app.toast(
            NoticeLevel::Warn,
            t(MSG_TRANSPORT_LOCKED, app.lang()).to_string(),
        );
        return;
    }
    app.settings.transport = match app.settings.transport {
        TransportChoice::Ble => TransportChoice::Lan,
        TransportChoice::Lan => TransportChoice::Ble,
    };
    persist(app);
}

fn connect(app: &mut App) {
    super::connect::connect(app);
}

fn disconnect(app: &mut App) {
    app.session.disconnect();
}

fn open_uart(app: &mut App) {
    super::dialogs::open_uart(app);
}

fn refresh_diagnostics(app: &mut App) {
    app.set_view(View::Diagnostics);
    if app.ble_connected() {
        let session = app.session.clone();
        app.diagnostics.refresh(&session);
    } else {
        app.toast(
            NoticeLevel::Warn,
            t(PAL_MSG_BLE_DIAGNOSTICS, app.lang()).to_string(),
        );
    }
}

fn wifi_scan(app: &mut App) {
    app.set_view(View::Network);
    super::network_view::action(app, super::network_view::NetEntry::Scan);
}

fn wifi_status(app: &mut App) {
    app.set_view(View::Network);
    super::network_view::action(app, super::network_view::NetEntry::WifiStatus);
}

fn webdav_status(app: &mut App) {
    app.set_view(View::Network);
    super::network_view::action(app, super::network_view::NetEntry::WebdavStatus);
}

fn ask_assistant(app: &mut App) {
    focus_assistant(app);
}

fn cycle_mode(app: &mut App) {
    let next = match app.exec_mode {
        crate::agent::ExecMode::Manual => crate::agent::ExecMode::Auto,
        crate::agent::ExecMode::Auto => crate::agent::ExecMode::FullAuto,
        crate::agent::ExecMode::FullAuto => crate::agent::ExecMode::Manual,
    };
    super::assistant_view::set_mode(app, next);
}

fn agent_settings(app: &mut App) {
    super::assistant_view::open_settings(app);
}

fn new_chat(app: &mut App) {
    let lang = app.lang();
    if app.exec_mode == crate::agent::ExecMode::FullAuto {
        // `reset()` of `web/device_executor.js`: a new conversation ends the
        // unattended execution window. Done before the status line is written
        // so `set_mode`'s own caption cannot overwrite the new-chat one.
        super::assistant_view::set_mode(app, crate::agent::ExecMode::Auto);
    }
    let status = t(PAL_MSG_NEW_CHAT, lang).to_string();
    app.assistant = super::assistant_view::AssistantState::default();
    app.assistant.status = status;
}

fn stop_agent(app: &mut App) {
    if let Some(agent) = &app.agent {
        agent.handle.stop();
    }
    if app.assistant.busy {
        app.assistant.busy = false;
        app.assistant
            .entries
            .push(super::assistant_view::Entry::System(
                super::i18n::t(super::assistant_view::ASST_STOP_MESSAGE, app.lang()).to_string(),
            ));
    }
}

/// Export the Markdown report the web's `agentExport` button downloads
/// (`web/agent_panel.js` → `buildTaskReport`), from the same two stores the
/// agent writes. The offer is gated the same way: with nothing to report it
/// says so instead of writing an empty file.
///
/// The Rust agent never records per-command executions (the web keeps those
/// for the live session only), so `records` is empty and `build_report`
/// simply omits that section.
/// The report text for `key`, or `None` when there is nothing to report —
/// the same condition that disables the web's export button. Kept pure so the
/// gate and the body can be pinned without a live session.
///
/// The body itself follows `linkr-lang` (`build_report` carries the headings
/// in both languages), so the file a Chinese user downloads is Chinese too.
fn report_body(
    key: &str,
    tasks: &[crate::agent::memory::Task],
    notes: &[crate::agent::memory::Note],
    generated_at: Option<&str>,
    lang: Lang,
) -> Option<String> {
    if tasks.is_empty() && notes.is_empty() {
        return None;
    }
    Some(crate::agent::build_report(&crate::agent::ReportInput {
        lang: lang.code(),
        device: key,
        task: None,
        tasks,
        records: &[],
        notes,
        generated_at,
    }))
}

fn export_report(app: &mut App) {
    let lang = app.lang();
    let info = app.session.info();
    let transport = match info.kind {
        Some(crate::transport::TransportKind::Ble) => "ble",
        _ => "ws",
    };
    let key = crate::agent::memory::device_identity(transport, info.device_id.as_deref(), None)
        .unwrap_or_default();
    let tasks = crate::agent::memory::TaskStore::open(crate::agent::store_path("agent_tasks.json"))
        .list(&key);
    let notes = crate::agent::memory::NoteStore::open(crate::agent::store_path("agent_notes.json"))
        .list(&key);
    let Some(report) = report_body(&key, &tasks, &notes, None, lang) else {
        app.toast(NoticeLevel::Warn, t(PAL_MSG_EXPORT_EMPTY, lang).to_string());
        return;
    };
    let stamp = chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        .replace([':', '.'], "-");
    let path = crate::agent::store_path(&format!("linkr-agent-{stamp}.md"));
    match std::fs::write(&path, &report) {
        Ok(()) => app.toast(
            NoticeLevel::Info,
            tr!(t(PAL_MSG_REPORT_WRITTEN, lang), path.display()),
        ),
        Err(err) => app.toast(NoticeLevel::Error, tr!(t(PAL_MSG_REPORT_FAILED, lang), err)),
    }
}

fn show_help(app: &mut App) {
    app.dialog = Some(super::dialogs::Dialog::Help(0));
}

fn show_notices(app: &mut App) {
    app.dialog = Some(super::dialogs::Dialog::Notices(0));
}

/// `app.language`: flip `linkr-lang` and persist it, exactly like the web's
/// language buttons. Everything on screen re-reads `app.lang()` every frame,
/// so the switch takes effect on the next draw.
fn toggle_language(app: &mut App) {
    app.settings.lang = app.settings.lang.toggled();
    if let Err(err) = super::settings::save(&app.settings) {
        app.toast(
            NoticeLevel::Warn,
            tr!(t(MSG_SAVE_SETTINGS, app.lang()), err),
        );
        return;
    }
    app.toast(
        NoticeLevel::Info,
        tr!(t(PAL_MSG_LANGUAGE, app.lang()), app.lang().endonym()),
    );
}

fn quit(app: &mut App) {
    super::dialogs::request_quit(app);
}

fn persist(app: &mut App) {
    if let Err(err) = super::settings::save(&app.settings) {
        let lang = app.lang();
        app.notices
            .push(NoticeLevel::Warn, tr!(t(MSG_SAVE_SETTINGS, lang), err));
    }
}

/// The canonical registry, grouped for display. Order = render order.
///
/// Only `id` and `shortcut` are protocol / key literals; `title` and
/// `category` are bilingual entries resolved at render time.
pub const ACTIONS: &[Action] = &[
    // Views
    Action {
        id: "view.terminal",
        title: PAL_T_VIEW_TERMINAL,
        category: PAL_CAT_VIEW,
        shortcut: "F2",
        run: view_terminal,
    },
    Action {
        id: "view.diagnostics",
        title: PAL_T_VIEW_DIAGNOSTICS,
        category: PAL_CAT_VIEW,
        shortcut: "F3",
        run: view_diagnostics,
    },
    Action {
        id: "view.network",
        title: PAL_T_VIEW_NETWORK,
        category: PAL_CAT_VIEW,
        shortcut: "F4",
        run: view_network,
    },
    Action {
        id: "view.assistant",
        title: PAL_T_VIEW_ASSISTANT,
        category: PAL_CAT_VIEW,
        shortcut: "F5",
        run: view_assistant,
    },
    // Focus
    Action {
        id: "focus.sidebar",
        title: PAL_T_FOCUS_SIDEBAR,
        category: PAL_CAT_FOCUS,
        shortcut: "Ctrl+Up",
        run: focus_sidebar,
    },
    Action {
        id: "focus.terminal",
        title: PAL_T_FOCUS_TERMINAL,
        category: PAL_CAT_FOCUS,
        shortcut: "Esc",
        run: focus_center,
    },
    Action {
        id: "focus.assistant",
        title: PAL_T_FOCUS_ASSISTANT,
        category: PAL_CAT_FOCUS,
        shortcut: "Ctrl+Shift+K",
        run: focus_assistant,
    },
    // Connection
    Action {
        id: "connect",
        title: PAL_T_CONNECT,
        category: PAL_CAT_CONNECTION,
        shortcut: "",
        run: connect,
    },
    Action {
        id: "disconnect",
        title: PAL_T_DISCONNECT,
        category: PAL_CAT_CONNECTION,
        shortcut: "",
        run: disconnect,
    },
    Action {
        id: "transport.toggle",
        title: PAL_T_TRANSPORT,
        category: PAL_CAT_CONNECTION,
        shortcut: "",
        run: toggle_transport,
    },
    Action {
        id: "uart.settings",
        title: PAL_T_UART,
        category: PAL_CAT_CONNECTION,
        shortcut: "",
        run: open_uart,
    },
    // Terminal
    Action {
        id: "term.font_bigger",
        title: PAL_T_FONT_BIGGER,
        category: PAL_CAT_TERMINAL,
        shortcut: "Ctrl+=",
        run: font_bigger,
    },
    Action {
        id: "term.font_smaller",
        title: PAL_T_FONT_SMALLER,
        category: PAL_CAT_TERMINAL,
        shortcut: "Ctrl+-",
        run: font_smaller,
    },
    Action {
        id: "term.font_reset",
        title: PAL_T_FONT_RESET,
        category: PAL_CAT_TERMINAL,
        shortcut: "Ctrl+0",
        run: font_reset,
    },
    Action {
        id: "term.autoscroll",
        title: PAL_T_AUTOSCROLL,
        category: PAL_CAT_TERMINAL,
        shortcut: "",
        run: toggle_autoscroll,
    },
    Action {
        id: "term.echo",
        title: PAL_T_ECHO,
        category: PAL_CAT_TERMINAL,
        shortcut: "",
        run: toggle_echo,
    },
    Action {
        id: "term.enter_mode",
        title: PAL_T_ENTER_MODE,
        category: PAL_CAT_TERMINAL,
        shortcut: "",
        run: cycle_enter,
    },
    Action {
        id: "term.clear",
        title: PAL_T_CLEAR,
        category: PAL_CAT_TERMINAL,
        shortcut: "Ctrl+L",
        run: clear_terminal,
    },
    Action {
        id: "term.save_log",
        title: PAL_T_SAVE_LOG,
        category: PAL_CAT_TERMINAL,
        shortcut: "",
        run: save_log,
    },
    Action {
        id: "term.copy",
        title: PAL_T_COPY,
        category: PAL_CAT_TERMINAL,
        shortcut: "",
        run: copy_visible,
    },
    // Diagnostics / network
    Action {
        id: "diag.refresh",
        title: PAL_T_DIAG_REFRESH,
        category: PAL_CAT_DIAGNOSTICS,
        shortcut: "",
        run: refresh_diagnostics,
    },
    Action {
        id: "wifi.scan",
        title: PAL_T_WIFI_SCAN,
        category: PAL_CAT_NETWORK,
        shortcut: "",
        run: wifi_scan,
    },
    Action {
        id: "wifi.status",
        title: PAL_T_WIFI_STATUS,
        category: PAL_CAT_NETWORK,
        shortcut: "",
        run: wifi_status,
    },
    Action {
        id: "webdav.status",
        title: PAL_T_WEBDAV_STATUS,
        category: PAL_CAT_NETWORK,
        shortcut: "",
        run: webdav_status,
    },
    // Assistant
    Action {
        id: "agent.ask",
        title: PAL_T_AGENT_ASK,
        category: PAL_CAT_ASSISTANT,
        shortcut: "",
        run: ask_assistant,
    },
    Action {
        id: "agent.mode",
        title: PAL_T_AGENT_MODE,
        category: PAL_CAT_ASSISTANT,
        shortcut: "",
        run: cycle_mode,
    },
    Action {
        id: "agent.settings",
        title: PAL_T_AGENT_SETTINGS,
        category: PAL_CAT_ASSISTANT,
        shortcut: "",
        run: agent_settings,
    },
    Action {
        id: "agent.new_chat",
        title: PAL_T_NEW_CHAT,
        category: PAL_CAT_ASSISTANT,
        shortcut: "",
        run: new_chat,
    },
    Action {
        id: "agent.stop",
        title: PAL_T_AGENT_STOP,
        category: PAL_CAT_ASSISTANT,
        shortcut: "",
        run: stop_agent,
    },
    Action {
        id: "agent.export",
        title: PAL_T_EXPORT,
        category: PAL_CAT_ASSISTANT,
        shortcut: "",
        run: export_report,
    },
    // App
    Action {
        id: "app.help",
        title: PAL_T_HELP,
        category: PAL_CAT_APP,
        shortcut: "F1",
        run: show_help,
    },
    Action {
        id: "app.notices",
        title: PAL_T_NOTICES,
        category: PAL_CAT_APP,
        shortcut: "",
        run: show_notices,
    },
    Action {
        id: "app.language",
        title: PAL_T_LANGUAGE,
        category: PAL_CAT_APP,
        shortcut: "",
        run: toggle_language,
    },
    Action {
        id: "app.quit",
        title: PAL_T_QUIT,
        category: PAL_CAT_APP,
        shortcut: "Ctrl+Q",
        run: quit,
    },
];

/// Open state of the palette.
#[derive(Default)]
pub struct PaletteState {
    pub query: String,
    pub selected: usize,
    /// `linkr-lang` of this overlay. The default stays `Lang::En` so an
    /// opener that only calls `PaletteState::default()` is still correct for
    /// English; pass [`PaletteState::new`] with `app.lang()` for 中文.
    pub lang: Lang,
}

impl PaletteState {
    /// Fresh overlay in `lang`, so the very first frame is already in the
    /// right language (the opener has the `App`, `render_lines` does not).
    pub fn new(lang: Lang) -> Self {
        Self {
            query: String::new(),
            selected: 0,
            lang,
        }
    }

    /// Move down one match, wrapping past the last row back to the first.
    pub fn step_down(&mut self, total: usize) {
        self.selected = if self.selected + 1 >= total {
            0
        } else {
            self.selected + 1
        };
        self.clamp_to(total);
    }

    /// Move up one match, wrapping past the first row back to the last, so the
    /// top row is one keypress away from the bottom of the list.
    pub fn step_up(&mut self, total: usize) {
        self.selected = if self.selected == 0 {
            total.saturating_sub(1)
        } else {
            self.selected - 1
        };
        self.clamp_to(total);
    }

    /// Keep the selection inside `total` matches — typing may have shrunk the
    /// list underneath it.
    pub fn clamp_to(&mut self, total: usize) {
        self.selected = self.selected.min(total.saturating_sub(1));
    }
}

/// Indices of the actions matching `query` (id / title / category substring,
/// in either language — a 中文 needle finds the Chinese labels, an English
/// one still finds them through the English half of the entry).
pub fn matches(query: &str) -> Vec<usize> {
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return (0..ACTIONS.len()).collect();
    }
    let hit = |text: &str| text.to_ascii_lowercase().contains(&needle);
    ACTIONS
        .iter()
        .enumerate()
        .filter(|(_, action)| {
            action.id.to_ascii_lowercase().contains(&needle)
                || action.title.iter().any(|text| hit(text))
                || action.category.iter().any(|text| hit(text))
        })
        .map(|(index, _)| index)
        .collect()
}

/// Rows of actions painted at once. The overlay box keeps this height while
/// the selection moves; only the window slides, so the box never shrinks.
pub const MAX_VISIBLE: usize = 18;

/// First visible row for `selected`. Keeps the selection near the middle and
/// slides back when the end approaches, so the window stays *full* while it
/// scrolls instead of walking off the bottom of the list.
pub fn window_start(selected: usize, total: usize, visible: usize) -> usize {
    selected
        .saturating_sub(visible / 2)
        .min(total.saturating_sub(visible))
}

/// Title of the overlay border, spaces included so the caller can pass it to
/// `Block::title` untouched (`layout.rs::draw_palette`).
pub fn panel_title(lang: Lang) -> &'static str {
    t(PAL_TITLE, lang)
}

/// Column widths of the two padded columns (28 + 12, as before).
const TITLE_WIDTH: usize = 28;
const CATEGORY_WIDTH: usize = 12;

/// Left-align `text` inside `width` **display** columns.
///
/// `format!("{:<28}")` counts characters, which walks the id column out of
/// line as soon as a title is 中文 (one glyph, two columns). Padding by
/// width keeps the columns in the same place in both languages, and leaves
/// English byte for byte unchanged: every English label is ASCII plus `…`,
/// and `…` is one column wide.
fn pad_to(text: &str, width: usize) -> String {
    let mut out = String::with_capacity(width);
    out.push_str(text);
    out.push_str(&" ".repeat(width.saturating_sub(unicode_width::UnicodeWidthStr::width(text))));
    out
}

/// Body lines of the palette overlay: prompt, separator, then the visible
/// window of matches. Pure over the state, so a unit test can pin the height.
pub fn render_lines(state: &PaletteState, visible: usize) -> Vec<Line<'static>> {
    let lang = state.lang;
    let found = matches(&state.query);
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "› ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            state.query.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    lines.push(Line::from(Span::styled(
        "─".repeat(56),
        Style::default().fg(Color::DarkGray),
    )));
    if found.is_empty() {
        lines.push(Line::from(Span::styled(
            t(PAL_NO_MATCH, lang),
            Style::default().fg(Color::DarkGray),
        )));
        return lines;
    }
    let visible = visible.clamp(1, MAX_VISIBLE);
    let start = window_start(state.selected, found.len(), visible);
    for (row, index) in found.iter().skip(start).take(visible).enumerate() {
        let action = &ACTIONS[*index];
        let selected = start + row == state.selected;
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "▸ " } else { "  " },
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                pad_to(t(action.title, lang), TITLE_WIDTH),
                Style::default()
                    .fg(if selected {
                        Color::Yellow
                    } else {
                        Color::White
                    })
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(
                pad_to(t(action.category, lang), CATEGORY_WIDTH),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(action.id.to_string(), Style::default().fg(Color::DarkGray)),
            Span::styled(
                if action.shortcut.is_empty() {
                    String::new()
                } else {
                    format!("  {}", action.shortcut)
                },
                Style::default().fg(Color::Magenta),
            ),
        ]));
    }
    lines
}

/// Keys while the palette is open. Returns `true` when consumed.
pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    // The overlay follows the session language: this is the only hook that
    // runs while it is open, so it also repairs an opener that left the
    // default `Lang::En` in place.
    let lang = app.lang();
    let Some(state) = app.palette.as_mut() else {
        return false;
    };
    state.lang = lang;
    let found = matches(&state.query);
    match key.code {
        KeyCode::Esc => {
            app.palette = None;
        }
        KeyCode::Enter => {
            let picked = found.get(state.selected).copied();
            app.palette = None;
            if let Some(index) = picked {
                (ACTIONS[index].run)(app);
            }
        }
        KeyCode::Up | KeyCode::BackTab => state.step_up(found.len()),
        KeyCode::Down | KeyCode::Tab => state.step_down(found.len()),
        KeyCode::Home => state.selected = 0,
        KeyCode::End => state.selected = found.len().saturating_sub(1),
        KeyCode::Backspace => {
            state.query.pop();
            state.selected = 0;
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.query.push(c);
            state.selected = 0;
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.query.clear();
            state.selected = 0;
        }
        _ => {}
    }
    // Re-clamp: typing may have shrunk the match list under the selection.
    if let Some(state) = app.palette.as_mut() {
        state.clamp_to(matches(&state.query).len());
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `reset()` of `web/device_executor.js`: a new conversation ends the
    /// unattended execution window. Manual and Auto stay where they are.
    #[test]
    fn a_new_chat_ends_the_unattended_window() {
        let mut app = crate::tui::test_app();
        crate::tui::assistant_view::set_mode(&mut app, crate::agent::ExecMode::FullAuto);
        assert_eq!(app.exec_mode, crate::agent::ExecMode::FullAuto);

        new_chat(&mut app);

        assert_eq!(
            app.exec_mode,
            crate::agent::ExecMode::Auto,
            "the way device.reset() does"
        );
        assert_eq!(
            app.assistant.status,
            t(PAL_MSG_NEW_CHAT, app.lang()).to_string()
        );

        crate::tui::assistant_view::set_mode(&mut app, crate::agent::ExecMode::Manual);
        new_chat(&mut app);
        assert_eq!(
            app.exec_mode,
            crate::agent::ExecMode::Manual,
            "the other two modes are left alone"
        );
    }

    /// "Focus the terminal" has to take you there. Setting `Focus::Center`
    /// without moving the view left the keystrokes on the panel the view was
    /// already showing — and on nothing at all in the assistant view, where
    /// the panel only reads `Focus::Assistant`.
    #[test]
    fn focusing_the_terminal_also_takes_you_there() {
        let mut app = crate::tui::test_app();
        app.set_view(View::Assistant);
        app.focus = Focus::Sidebar;

        focus_center(&mut app);

        assert_eq!(app.view, View::Terminal, "the action's own title");
        assert_eq!(app.focus, Focus::Center, "…and the keys land on it");
    }

    /// The gate matches the web's `agentExport` disabled state, and the body
    /// is the same Markdown `buildTaskReport` would produce for these stores.
    /// The Rust agent keeps no per-command executions, so that section must
    /// be absent rather than fabricated.
    #[test]
    fn the_report_export_offers_only_what_exists() {
        let key = "[\"ble\",\"aa:bb\",\"\"]";
        assert!(
            report_body(key, &[], &[], None, Lang::En).is_none(),
            "with nothing stored the action has nothing to offer"
        );

        let tasks = vec![crate::agent::memory::Task {
            id: "task-1".to_string(),
            device_key: key.to_string(),
            updated_at: 0,
            goal: "read the sensor registers".to_string(),
            summary: String::new(),
            status: "open".to_string(),
            plan: Vec::new(),
            executions: Vec::new(),
        }];
        let body = report_body(key, &tasks, &[], Some("2026-10-03T00:00:00.000Z"), Lang::En)
            .expect("a stored task is reportable");
        assert!(body.contains("read the sensor registers"), "{body}");
        assert!(body.contains("2026-10-03T00:00:00.000Z"), "{body}");
        assert!(
            !body.contains("Target executions"),
            "no executions are recorded, so the section must not appear"
        );

        // A note alone is enough, exactly as `!tasks.length &&
        // !notes.length && !records.length` works in agent_panel.js.
        let notes = vec![crate::agent::memory::Note {
            id: "note-1".to_string(),
            device_key: key.to_string(),
            text: "the board reports 3V3".to_string(),
            evidence: String::new(),
            created_at: 0,
            duplicate: false,
        }];
        let body = report_body(key, &[], &notes, Some("2026-10-03T00:00:00.000Z"), Lang::En)
            .expect("a stored note is reportable");
        assert!(body.contains("the board reports 3V3"), "{body}");

        // The body follows `linkr-lang` — headings translate, the stored text
        // and the timestamps do not.
        let zh = report_body(key, &tasks, &[], Some("2026-10-03T00:00:00.000Z"), Lang::Zh)
            .expect("a stored task is reportable");
        assert!(zh.contains("# Linkr Bee 排查报告"), "{zh}");
        assert!(zh.contains("read the sensor registers"), "{zh}");
        assert!(!zh.contains("Target executions"), "{zh}");
    }

    /// The canonical id list of CONTRACTS.md section 5 ("palette lists every
    /// action"). The registry must match it exactly, in order.
    const CANONICAL: &[&str] = &[
        "view.terminal",
        "view.diagnostics",
        "view.network",
        "view.assistant",
        "focus.sidebar",
        "focus.terminal",
        "focus.assistant",
        "connect",
        "disconnect",
        "transport.toggle",
        "uart.settings",
        "term.font_bigger",
        "term.font_smaller",
        "term.font_reset",
        "term.autoscroll",
        "term.echo",
        "term.enter_mode",
        "term.clear",
        "term.save_log",
        "term.copy",
        "diag.refresh",
        "wifi.scan",
        "wifi.status",
        "webdav.status",
        "agent.ask",
        "agent.mode",
        "agent.settings",
        "agent.new_chat",
        "agent.stop",
        "agent.export",
        "app.help",
        "app.notices",
        "app.language",
        "app.quit",
    ];

    #[test]
    fn registry_is_complete_and_ordered() {
        let ids: Vec<&str> = ACTIONS.iter().map(|action| action.id).collect();
        assert_eq!(ids, CANONICAL, "palette registry drifted from the contract");
    }

    /// The window must stay full while the selection moves: the old
    /// `selected - 9` walk produced a shorter window near the end, which
    /// shrank the box and, with a second scroll on top, emptied it.
    #[test]
    fn window_keeps_the_list_full_while_scrolling() {
        let total = ACTIONS.len();
        let visible = MAX_VISIBLE;
        let mut previous = 0;
        for selected in 0..total {
            let start = window_start(selected, total, visible);
            assert!(
                start + visible <= total,
                "selected {selected}: window {start}.. overflows {total}"
            );
            assert!(
                start <= selected && selected < start + visible,
                "selected {selected} is off-screen (window {start}, {visible} rows)"
            );
            assert!(
                start - previous <= 1,
                "window jumped from {previous} to {start} — it must scroll one row at a time"
            );
            previous = start;
        }
        // The end of the list still shows a full window.
        assert_eq!(
            window_start(total - 1, total, visible),
            total - visible,
            "the last selection must pin the window to the bottom"
        );
    }

    /// Same line count at the top, the middle and the bottom of the list: the
    /// overlay box is sized from this, so it must not resize while scrolling.
    #[test]
    fn the_box_does_not_shrink_while_scrolling() {
        let mut state = PaletteState::default();
        let top = render_lines(&state, MAX_VISIBLE);
        state.selected = ACTIONS.len() / 2;
        let middle = render_lines(&state, MAX_VISIBLE);
        state.selected = ACTIONS.len() - 1;
        let bottom = render_lines(&state, MAX_VISIBLE);

        assert_eq!(top.len(), 2 + MAX_VISIBLE, "prompt, separator, 18 rows");
        assert_eq!(top.len(), middle.len(), "box resized mid-list");
        assert_eq!(top.len(), bottom.len(), "box shrank at the end");

        // The prompt line stays first: that is where the cursor is drawn.
        assert_eq!(top[0].spans[0].content, "› ");
        assert_eq!(middle[0].spans[0].content, "› ");
        assert_eq!(bottom[0].spans[0].content, "› ");

        // Exactly one row is marked selected, and it is the right one.
        for lines in [&top, &middle, &bottom] {
            let marked = lines.iter().filter(|l| l.spans[0].content == "▸ ").count();
            assert_eq!(marked, 1, "expected exactly one selected row");
        }
        let start = window_start(ACTIONS.len() - 1, ACTIONS.len(), MAX_VISIBLE);
        assert_eq!(
            bottom[2 + (ACTIONS.len() - 1 - start)].spans[0].content,
            "▸ ",
            "the last action must be the marked row"
        );
    }

    /// The top row is one keypress away from the bottom of the list.
    #[test]
    fn the_selection_wraps_at_both_ends() {
        let total = ACTIONS.len();
        let mut state = PaletteState::default();

        state.step_down(total);
        assert_eq!(state.selected, 1);
        state.step_up(total);
        assert_eq!(state.selected, 0);

        state.step_up(total);
        assert_eq!(
            state.selected,
            total - 1,
            "Up on the first row wraps to the last"
        );
        state.step_down(total);
        assert_eq!(state.selected, 0, "Down on the last row wraps to the first");

        // A filtered list wraps inside its own matches.
        let filtered = matches("wifi").len();
        assert!(filtered > 1);
        state.clamp_to(filtered);
        state.step_up(filtered);
        assert_eq!(state.selected, filtered - 1);

        // An empty result set stays put instead of underflowing.
        let mut empty = PaletteState {
            query: "no-such-action".to_string(),
            ..PaletteState::default()
        };
        empty.step_up(0);
        empty.step_down(0);
        assert_eq!(empty.selected, 0);
    }

    /// Typing shrinks the match list under the selection: it must clamp, not
    /// point past the end (which used to render nothing at all).
    #[test]
    fn a_shrinking_query_clamps_the_selection() {
        let mut state = PaletteState {
            query: "wifi".to_string(),
            selected: ACTIONS.len() - 1,
            ..PaletteState::default()
        };
        state.clamp_to(matches(&state.query).len());
        assert_eq!(state.selected, matches(&state.query).len() - 1);

        let lines = render_lines(&state, MAX_VISIBLE);
        assert_eq!(lines.len(), 2 + matches(&state.query).len());
        assert!(
            lines.iter().any(|line| line.spans[0].content == "▸ "),
            "the selection must still be visible after filtering"
        );
    }

    #[test]
    fn every_action_is_labelled() {
        for action in ACTIONS {
            assert!(!action.title[0].is_empty(), "{} has no title", action.id);
            assert!(
                !action.title[1].is_empty(),
                "{} has no chinese title",
                action.id
            );
            assert!(
                !action.category[0].is_empty(),
                "{} has no category",
                action.id
            );
            assert!(
                !action.category[1].is_empty(),
                "{} has no chinese category",
                action.id
            );
        }
    }

    #[test]
    fn ids_are_unique() {
        for (index, action) in ACTIONS.iter().enumerate() {
            let count = ACTIONS.iter().filter(|a| a.id == action.id).count();
            assert_eq!(count, 1, "duplicate id {}", action.id);
            let _ = index;
        }
    }

    #[test]
    fn required_contract_actions_exist() {
        for id in [
            "connect",
            "uart.settings",
            "wifi.scan",
            "term.save_log",
            "view.network",
            "term.autoscroll",
            "agent.ask",
            "app.quit",
        ] {
            assert!(
                ACTIONS.iter().any(|action| action.id == id),
                "palette is missing {id}"
            );
        }
    }

    #[test]
    fn search_matches_id_title_and_category() {
        let found = matches("save");
        let ids: Vec<&str> = found.iter().map(|i| ACTIONS[*i].id).collect();
        assert!(ids.contains(&"term.save_log"), "{ids:?}");

        // Substring match across id, title and category (lowercased).
        let found = matches("assistant");
        assert!(!found.is_empty());
        let ids: Vec<&str> = found.iter().map(|i| ACTIONS[*i].id).collect();
        assert!(ids.contains(&"agent.ask"), "{ids:?}");
        assert!(ids.contains(&"view.assistant"), "{ids:?}");
        assert!(ids.contains(&"focus.assistant"), "{ids:?}");
        for action in ACTIONS
            .iter()
            .filter(|a| t(a.category, Lang::En) == "Assistant")
        {
            assert!(
                ids.contains(&action.id),
                "{} not found for \"assistant\"",
                action.id
            );
        }

        let found = matches("wifi.scan");
        assert_eq!(
            found,
            vec![ACTIONS.iter().position(|a| a.id == "wifi.scan").unwrap()]
        );

        assert!(matches("nothing-matches-this").is_empty());
        assert_eq!(matches("").len(), ACTIONS.len());
    }

    /// Both languages of every palette message carry text and differ — a
    /// copied English string is a missing translation, not a translation.
    #[test]
    fn every_palette_message_is_translated() {
        super::super::i18n::assert_bilingual(ALL);
        assert!(
            ALL.len() >= 53,
            "the palette alone carries 53 messages, got {}",
            ALL.len()
        );
    }

    /// Chinese must reach the rendered rows: labels translate, the protocol
    /// ids and the key hints stay byte for byte.
    #[test]
    fn the_palette_follows_the_language() {
        let paint = |state: &PaletteState| {
            render_lines(state, MAX_VISIBLE)
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.to_string())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let zh = paint(&PaletteState::new(Lang::Zh));
        assert!(zh.contains("打开终端视图"), "{zh}");
        assert!(zh.contains("聚焦侧栏"), "{zh}");
        assert!(zh.contains("连接"), "{zh}");
        assert!(!zh.contains("Open terminal view"), "{zh}");
        assert!(
            zh.contains("view.terminal"),
            "ids stay protocol-literal: {zh}"
        );
        assert!(zh.contains("F2"), "key hints stay literal: {zh}");

        let en = paint(&PaletteState::new(Lang::En));
        assert!(en.contains("Open terminal view"), "{en}");
        assert!(!en.contains("打开终端视图"), "{en}");

        // The border title is painted by the caller (`layout.rs`).
        assert_eq!(panel_title(Lang::En), " Command palette (Ctrl+P) ");
        assert_eq!(panel_title(Lang::Zh), " 命令面板（Ctrl+P） ");

        // The empty-result line follows the language too.
        let none = paint(&PaletteState {
            query: "no-such-action".to_string(),
            lang: Lang::Zh,
            ..PaletteState::default()
        });
        assert!(none.contains("无匹配动作。"), "{none}");
        assert!(!none.contains("No matching action."), "{none}");

        // A 中文 needle filters like an English one.
        assert_eq!(
            matches("扫描"),
            vec![ACTIONS.iter().position(|a| a.id == "wifi.scan").unwrap()]
        );
    }

    /// The padded columns are measured in display columns now, so 中文 rows
    /// line up; for the English labels — ASCII plus `…`, one column wide —
    /// the bytes must still be the ones `format!` used to produce.
    #[test]
    fn english_rows_keep_their_exact_padding() {
        for action in ACTIONS {
            let title = t(action.title, Lang::En);
            let category = t(action.category, Lang::En);
            assert_eq!(
                pad_to(title, TITLE_WIDTH),
                format!("{:<28}", title),
                "{title}"
            );
            assert_eq!(
                pad_to(category, CATEGORY_WIDTH),
                format!("{:<12}", category),
                "{category}"
            );
        }
    }
}
