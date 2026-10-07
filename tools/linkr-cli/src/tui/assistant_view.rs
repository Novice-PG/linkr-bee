//! Assistant panel (CONTRACTS.md section 5 / WEB_UX_SPEC section 7): message
//! log with tool-call blocks, execution-mode picker, composer, usage line and
//! the lazy agent runtime.
//!
//! The agent task is spawned on the first question through
//! [`crate::agent::spawn`], guarded with `catch_unwind` while that workstream
//! is still landing: a panic turns into a toast instead of killing the TUI.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::agent_settings;
use super::i18n::{strings, t, tr, Lang};
use super::state::{AgentRuntime, App, TextField};
use crate::agent::{AgentEvent, ExecMode};
use crate::event::NoticeLevel;

strings! {
    ASST_AGENT => "Agent", "助手";
    ASST_ACTIONS => "  Settings · New chat · Exit", "  设置 · 新建对话 · 退出";
    ASST_MODE_MANUAL => "Manual", "手动";
    ASST_MODE_AUTO => "Auto · Recommended", "Auto · 推荐";
    ASST_MODE_FULL_AUTO => "Full Auto", "Full Auto";
    ASST_MODE_MANUAL_HELP => "AI proposes commands; click Send to enter them on the target.",
        "AI 提出命令建议；点击发送后才会输入到被控机。";
    ASST_MODE_AUTO_HELP => "Low-risk queries run at a recognized shell prompt; other input needs approval. Destructive commands need approval in every mode.",
        "识别到 Shell 提示符时自动执行低风险查询，其余输入需确认；破坏性命令在任何档位都需确认。";
    ASST_MODE_FULL_AUTO_HELP => "Commands run without confirmation. Recognized destructive or irreversible commands — recursive/forced deletes, disk and filesystem tools, dd, flashing and bootloader tools, downloaded content piped into a shell, privilege escalation, recursive permission changes — still need your approval. Detection does not cover every operation inside scripts or indirect execution.",
        "命令直接执行；识别出的破坏性或不可逆命令（递归/强制删除、磁盘与文件系统工具、dd、刷写与引导工具、下载内容管道进 shell、提权、递归改权限）仍需确认。该检测不覆盖脚本或间接执行中的所有操作。";
    ASST_PICKER_TITLE => "Change execution mode", "切换执行档位";
    ASST_PICKER_HINT => "↑/↓ pick · Enter engage · Esc close",
        "↑/↓ 选择 · Enter 应用 · Esc 关闭";
    ASST_EMPTY_HINT => "Ask a question about the device; the assistant reads the serial journal.",
        "就设备提问；助手会读取串口日志。";
    ASST_SPEAKER_YOU => "you › ", "你 › ";
    ASST_SPEAKER_AI => "ai › ", "AI › ";
    ASST_COMPOSER_HINT => "Describe the problem", "描述问题";
    ASST_KEYS_LINE_1 => "Enter newline · Ctrl/Alt+Enter send · Ctrl+Shift+M mode · Ctrl+Shift+S settings",
        "Enter 换行 · Ctrl/Alt+Enter 发送 · Ctrl+Shift+M 模式 · Ctrl+Shift+S 设置";
    ASST_KEYS_LINE_2 => "Ctrl+Shift+N new chat · Esc exit to the terminal · Ctrl+Q quit the TUI",
        "Ctrl+Shift+N 新建对话 · Esc 返回终端 · Ctrl+Q 退出 TUI";
    ASST_NEW_CHAT_STATUS => "New chat.", "已新建对话。";
    ASST_MODE_CHANGED => "Mode changed; conversation retained. This run stopped and pending input was cancelled; sent input cannot be recalled. Ask again to continue.",
        "档位已切换，对话已保留。本轮已停止，待确认输入已取消；已发送的输入无法撤回。请继续提问。";
    ASST_MODE_STATUS => "Mode: {}", "模式：{}";
    ASST_SET_CONFIG_HINT => "Set the AI configuration first (Ctrl+P → agent.settings).",
        "请先设置 AI 配置（Ctrl+P → agent.settings）。";
    ASST_NO_CONFIG => "No AI configuration saved.", "未保存 AI 配置。";
    ASST_RUNTIME_MISSING => "The assistant runtime is not available in this build.",
        "此构建不含助手运行时。";
    ASST_UNAVAILABLE => "Assistant unavailable.", "助手不可用。";
    ASST_STOP_MESSAGE => "Stopped. Already sent input cannot be recalled; use Ctrl-C in the terminal to interrupt the target program.",
        "已停止。已发送的输入无法撤回；在终端用 Ctrl-C 中断目标程序。";
    ASST_USAGE_TOTAL => "Tokens this conversation {} · ↑{} ↓{}",
        "本次对话 tokens {} · ↑{} ↓{}";
    ASST_USAGE_COST => " · Estimated cost ~{}", " · 预估成本 ~{}";
    ASST_USAGE_NO_PRICES => " · Prices not set", " · 未设置价格";
}

/// Help text of the three modes (WEB_UX_SPEC section 7.2): label and help in
/// both languages. The test pins the English column byte for byte.
pub const MODE_HELP: [(super::i18n::Entry, super::i18n::Entry); 3] = [
    (ASST_MODE_MANUAL, ASST_MODE_MANUAL_HELP),
    (ASST_MODE_AUTO, ASST_MODE_AUTO_HELP),
    (ASST_MODE_FULL_AUTO, ASST_MODE_FULL_AUTO_HELP),
];

pub const COMPOSER_MAX: usize = 4000;

/// One rendered row of the chat log.
#[derive(Debug, Clone)]
pub enum Entry {
    User(String),
    Assistant(String),
    ToolStart {
        name: String,
        args: String,
    },
    ToolEnd {
        name: String,
        result: String,
        ok: bool,
    },
    System(String),
}

/// State of the assistant panel.
#[derive(Default)]
pub struct AssistantState {
    pub entries: Vec<Entry>,
    pub composer: TextField,
    /// Mode picker open (web `#agentModePicker`).
    pub picker_open: bool,
    pub picker_sel: usize,
    /// Accumulated streaming reply of the running turn.
    pub streaming: String,
    pub busy: bool,
    pub status: String,
    pub usage: Option<(u64, u64, u64, f64)>,
}

impl AssistantState {
    pub fn mode_index(exec: ExecMode) -> usize {
        match exec {
            ExecMode::Manual => 0,
            ExecMode::Auto => 1,
            ExecMode::FullAuto => 2,
        }
    }

    pub fn exec_of(index: usize) -> ExecMode {
        match index {
            0 => ExecMode::Manual,
            2 => ExecMode::FullAuto,
            _ => ExecMode::Auto,
        }
    }

    /// `Tokens this conversation …` line (WEB_UX_SPEC section 7.5).
    ///
    /// The language is a parameter rather than an `AssistantState` field: the
    /// state is rebuilt from `AssistantState::default()` in `mod.rs` and
    /// `palette.rs`, files this workstream cannot edit, so a stored field
    /// would silently fall back to English after "New chat".
    pub fn usage_line(&self, lang: Lang) -> Option<String> {
        let (input, output, cache, cost) = self.usage?;
        let total = input + output;
        let mut line = tr!(
            t(ASST_USAGE_TOTAL, lang),
            super::status::format_count(total),
            super::status::format_count(input),
            super::status::format_count(output),
        );
        if cache > 0 {
            line.push_str(&format!(" ⚡{}", super::status::format_count(cache)));
        }
        if cost > 0.0 {
            line.push_str(&tr!(t(ASST_USAGE_COST, lang), format_cost(cost)));
        } else {
            line.push_str(t(ASST_USAGE_NO_PRICES, lang));
        }
        Some(line)
    }
}

/// Cost display rule of `formatCost()` in `agent_usage.js`: <0.01 → 4 dp,
/// <1 → 3 dp, otherwise 2 dp.
pub fn format_cost(cost: f64) -> String {
    if cost < 0.01 {
        format!("${cost:.4}")
    } else if cost < 1.0 {
        format!("${cost:.3}")
    } else {
        format!("${cost:.2}")
    }
}

// --- markdown ----------------------------------------------------------------

/// Minimal inline markdown: `**bold**` and `` `code` `` runs become spans.
pub fn markdown(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0usize;
    let mut plain_start = 0usize;
    while index < bytes.len() {
        let marker = if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'*') {
            Some(2usize)
        } else if bytes[index] == b'`' {
            Some(1usize)
        } else {
            None
        };
        let Some(width) = marker else {
            index += 1;
            continue;
        };
        let token = &text[index..index + width];
        let close = text[index + width..].find(token);
        let Some(close) = close else {
            index += width;
            continue;
        };
        let start = index + width;
        let end = start + close;
        if plain_start < index {
            spans.push(Span::raw(text[plain_start..index].to_string()));
        }
        if width == 2 {
            spans.push(Span::styled(
                text[start..end].to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                text[start..end].to_string(),
                Style::default().fg(Color::LightGreen),
            ));
        }
        index = end + width;
        plain_start = index;
    }
    if plain_start < text.len() {
        spans.push(Span::raw(text[plain_start..].to_string()));
    }
    spans
}

fn wrapped(text: &str, width: u16, style: Style) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let spans = markdown(raw);
        let mut current: Vec<Span<'static>> = Vec::new();
        let mut len = 0usize;
        for span in spans {
            let w = unicode_width::UnicodeWidthStr::width(span.content.as_ref());
            if len + w > width as usize && !current.is_empty() {
                out.push(Line::from(std::mem::take(&mut current)));
                len = 0;
            }
            current.push(span.style(style));
            len += w;
        }
        out.push(Line::from(current));
    }
    out
}

// --- rendering ---------------------------------------------------------------

/// The composer row is `› ` plus what is typed, and it has to fit the pane:
/// the center pane wraps with `trim: false`, so an over-long row was carried
/// onto the next line and pushed the key hints down with it. Measured in
/// columns — a CJK glyph is two of them — because the character count let the
/// Chinese line run one column over for every glyph it kept, and the old cut
/// built the row two columns wider than the pane even in English.
///
/// Returns the `…` prefix (present only when something was dropped) and the
/// tail of the text that still fits: the composer shows what you just typed,
/// so the end is the part worth keeping.
fn composer_tail(text: &str, width: u16) -> (String, String) {
    const PROMPT_COLS: usize = 2;
    let budget = (width as usize).saturating_sub(PROMPT_COLS);
    if budget == 0 {
        return (String::new(), String::new());
    }
    if unicode_width::UnicodeWidthStr::width(text) <= budget {
        return (String::new(), text.to_string());
    }
    let room = budget - 1; // one column of the budget is the ellipsis
    let mut kept = String::new();
    let mut used = 0usize;
    for ch in text.chars().rev() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > room {
            break;
        }
        kept.push(ch);
        used += w;
    }
    (String::from("…"), kept.chars().rev().collect())
}

/// Body of the Assistant view.
pub fn render_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let lang = app.lang();
    let state = &app.assistant;
    let width = width.max(20);
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Header: mode button + actions. The caption follows the panel button of
    // `web/agent_panel.js` (`refreshMode`): the auto mode prints `Auto` in
    // every language, the picker keeps the longer `Auto · Recommended`.
    let mode_help = MODE_HELP[AssistantState::mode_index(app.exec_mode)].1;
    lines.push(Line::from(vec![
        Span::styled(
            t(ASST_AGENT, lang),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default()),
        Span::styled(
            format!("[{}]", super::status::mode_text(app.exec_mode, lang)),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(t(ASST_ACTIONS, lang), Style::default().fg(Color::DarkGray)),
    ]));
    lines.push(Line::from(Span::styled(
        t(mode_help, lang),
        Style::default().fg(Color::DarkGray),
    )));

    if state.picker_open {
        lines.push(Line::from(Span::styled(
            t(ASST_PICKER_TITLE, lang),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        for (index, (label, help)) in MODE_HELP.iter().enumerate() {
            let selected = index == state.picker_sel;
            lines.push(Line::from(vec![
                Span::styled(
                    if selected { "▸ " } else { "  " },
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    if app.exec_mode == AssistantState::exec_of(index) {
                        "● "
                    } else {
                        "○ "
                    },
                    Style::default().fg(Color::Green),
                ),
                Span::styled(
                    t(*label, lang).to_string(),
                    Style::default()
                        .fg(if selected { Color::White } else { Color::Gray })
                        .add_modifier(if selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                ),
            ]));
            if selected {
                for line in wrapped(
                    t(*help, lang),
                    width.saturating_sub(4),
                    Style::default().fg(Color::DarkGray),
                ) {
                    lines.push(Line::from(
                        line.spans
                            .into_iter()
                            .map(|s| s.style(Style::default().fg(Color::DarkGray)))
                            .collect::<Vec<_>>(),
                    ));
                }
            }
        }
        lines.push(Line::from(Span::styled(
            t(ASST_PICKER_HINT, lang),
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));
    }

    if state.entries.is_empty() && state.streaming.is_empty() {
        lines.push(Line::from(Span::styled(
            t(ASST_EMPTY_HINT, lang),
            Style::default().fg(Color::DarkGray),
        )));
    }

    for entry in &state.entries {
        match entry {
            Entry::User(text) => {
                lines.push(Line::from(Span::styled(
                    t(ASST_SPEAKER_YOU, lang),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.extend(wrapped(
                    text,
                    width.saturating_sub(6),
                    Style::default().fg(Color::White),
                ));
            }
            Entry::Assistant(text) => {
                lines.push(Line::from(Span::styled(
                    t(ASST_SPEAKER_AI, lang),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.extend(wrapped(
                    text,
                    width.saturating_sub(5),
                    Style::default().fg(Color::Gray),
                ));
            }
            Entry::ToolStart { name, args } => {
                lines.push(Line::from(vec![
                    Span::styled("  ⚙ ", Style::default().fg(Color::Magenta)),
                    Span::styled(
                        name.clone(),
                        Style::default()
                            .fg(Color::Magenta)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" {args}"), Style::default().fg(Color::DarkGray)),
                ]));
            }
            Entry::ToolEnd { name, result, ok } => {
                let color = if *ok {
                    Color::DarkGray
                } else {
                    Color::LightRed
                };
                let clip: String = result.chars().take(width as usize).collect();
                lines.push(Line::from(vec![
                    Span::styled(
                        if *ok { "  ✓ " } else { "  ✗ " },
                        Style::default().fg(color),
                    ),
                    Span::styled(name.clone(), Style::default().fg(color)),
                    Span::styled(format!(" {clip}"), Style::default().fg(color)),
                ]));
            }
            Entry::System(text) => {
                lines.extend(wrapped(text, width, Style::default().fg(Color::DarkGray)));
            }
        }
    }

    if !state.streaming.is_empty() {
        lines.extend(wrapped(
            &state.streaming,
            width.saturating_sub(5),
            Style::default().fg(Color::Gray),
        ));
    }
    if state.busy {
        lines.push(Line::from(Span::styled(
            "· · ·",
            Style::default().fg(Color::Yellow),
        )));
    }

    lines.push(Line::from(""));
    if let Some(usage) = state.usage_line(lang) {
        lines.push(Line::from(Span::styled(
            usage,
            Style::default().fg(Color::DarkGray),
        )));
    }
    if !state.status.is_empty() {
        lines.push(Line::from(Span::styled(
            state.status.clone(),
            Style::default().fg(Color::LightYellow),
        )));
    }

    // Composer.
    lines.push(Line::from(Span::styled(
        t(ASST_COMPOSER_HINT, lang),
        Style::default().fg(Color::DarkGray),
    )));
    let (text, _) = state.composer.display(None);
    let shown = if text.is_empty() { String::new() } else { text };
    let (head, tail) = composer_tail(&shown, width);
    lines.push(Line::from(vec![
        Span::styled(
            "› ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(head, Style::default().fg(Color::DarkGray)),
        Span::styled(
            tail,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        t(ASST_KEYS_LINE_1, lang),
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled(
        t(ASST_KEYS_LINE_2, lang),
        Style::default().fg(Color::DarkGray),
    )));
    lines
}

// --- keys --------------------------------------------------------------------

/// Keys of the Assistant view / focused composer.
pub fn handle_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    // The legacy terminal encoding cannot distinguish Ctrl+Enter from Enter at
    // all (both are 0x0d), so `Alt+Enter` — `0x1b 0x0d`, accepted by every
    // terminal, unlike the kitty keyboard protocol GNOME Terminal still lacks
    // — is a second way to send. Terminals that do speak kitty report the real
    // Ctrl+Enter as `CSI 13;5u`.
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    if app.assistant.picker_open {
        match key.code {
            KeyCode::Esc => {
                app.assistant.picker_open = false;
                return;
            }
            KeyCode::Up | KeyCode::Left => {
                app.assistant.picker_sel = app.assistant.picker_sel.saturating_sub(1);
                return;
            }
            KeyCode::Down | KeyCode::Right => {
                app.assistant.picker_sel = (app.assistant.picker_sel + 1).min(MODE_HELP.len() - 1);
                return;
            }
            KeyCode::Home => {
                app.assistant.picker_sel = 0;
                return;
            }
            KeyCode::End => {
                app.assistant.picker_sel = MODE_HELP.len() - 1;
                return;
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let index = app.assistant.picker_sel;
                app.assistant.picker_open = false;
                set_mode(app, AssistantState::exec_of(index));
                return;
            }
            _ => return,
        }
    }

    match key.code {
        KeyCode::Char('m') if ctrl && shift => {
            app.assistant.picker_open = true;
            app.assistant.picker_sel = AssistantState::mode_index(app.exec_mode);
        }
        KeyCode::Char('s') if ctrl && shift => open_settings(app),
        KeyCode::Char('n') if ctrl && shift => {
            let lang = app.lang();
            // web `#agentNew` stops the run *before* it drops the transcript
            // (`agent_panel.js:943-947`): without this the old reply keeps
            // streaming into the new conversation, and `busy` going false while
            // the runtime is still working defeats the `ASST_BUSY` guards.
            if app.assistant.busy {
                if let Some(agent) = &app.agent {
                    agent.handle.stop();
                }
            }
            app.assistant = super::assistant_view::AssistantState::default();
            app.assistant.status = t(ASST_NEW_CHAT_STATUS, lang).to_string();
        }
        KeyCode::Enter if ctrl || alt => submit(app),
        KeyCode::Enter => {
            // Pasted lines arrive as bare Enters (`mod.rs` → `paste_keys`), so
            // the 4000-character cap has to cover them too — `maxlength` does
            // on the web textarea.
            if app.assistant.composer.as_str().chars().count() < COMPOSER_MAX {
                app.assistant.composer.insert_char('\n');
            }
        }
        KeyCode::Char(c) if !ctrl => {
            let len = app.assistant.composer.as_str().chars().count();
            if len < COMPOSER_MAX {
                app.assistant.composer.insert_char(c);
            }
        }
        KeyCode::Backspace => app.assistant.composer.backspace(),
        KeyCode::Delete => app.assistant.composer.delete(),
        KeyCode::Left => app.assistant.composer.left(),
        KeyCode::Right => app.assistant.composer.right(),
        KeyCode::Up if !shift => app.assistant.composer.home(),
        KeyCode::Down if !shift => app.assistant.composer.end(),
        KeyCode::Home => app.assistant.composer.home(),
        KeyCode::End => app.assistant.composer.end(),
        KeyCode::PageUp => {
            let limit = super::layout::center_scroll_limit(app);
            app.center_scroll = super::layout::page_scroll(app.center_scroll, limit, 5, true);
        }
        KeyCode::PageDown => {
            let limit = super::layout::center_scroll_limit(app);
            app.center_scroll = super::layout::page_scroll(app.center_scroll, limit, 5, false);
        }
        _ => {}
    }
}

/// Open the AI configuration dialog.
pub fn open_settings(app: &mut App) {
    if app.assistant.busy {
        app.toast(
            NoticeLevel::Warn,
            super::i18n::t(agent_settings::ASST_BUSY, app.lang()).to_string(),
        );
        return;
    }
    app.dialog = Some(super::dialogs::Dialog::Settings(
        super::agent_settings::AgentSettingsState::load(),
    ));
}

/// Engage an execution mode (the running turn is stopped first, web
/// `stop("modeChanged")`).
pub fn set_mode(app: &mut App, mode: ExecMode) {
    let lang = app.lang();
    if app.exec_mode == mode {
        return;
    }
    if app.assistant.busy {
        push_system(app, t(ASST_MODE_CHANGED, lang));
        if let Some(agent) = &app.agent {
            agent.handle.stop();
        }
        app.assistant.busy = false;
    }
    app.exec_mode = mode;
    let name = MODE_HELP[AssistantState::mode_index(mode)].0;
    app.assistant.status = tr!(t(ASST_MODE_STATUS, lang), t(name, lang));
}

fn push_system(app: &mut App, text: &str) {
    app.assistant.entries.push(Entry::System(text.to_string()));
}

/// Spawn the runtime on the first question (guarded: `agent::spawn` may still
/// be a `todo!()` while workstream A lands).
fn ensure_agent(app: &mut App) -> bool {
    if app.agent.is_some() {
        return true;
    }
    let lang = app.lang();
    let config = agent_settings::runtime_config();
    if config.is_none() {
        app.toast(NoticeLevel::Warn, t(ASST_SET_CONFIG_HINT, lang).to_string());
        app.assistant.status = t(ASST_NO_CONFIG, lang).to_string();
        return false;
    }
    let broker = Arc::new(app.broker.clone()) as Arc<dyn crate::agent::ApprovalBroker>;
    let session = app.session.clone();
    let bus = app.bus.clone();
    // `agent::spawn` starts tokio tasks, and the frame loop lives outside the
    // runtime (it only `block_on`s the frame tick in `mod.rs`): without an
    // entered handle `tokio::spawn` panics with "there is no reactor running",
    // which a release build turns into a SIGABRT of the whole TUI. The guard
    // borrows the cloned `Arc`, never `app`, so `app.agent` stays assignable.
    let rt = app.rt.clone();
    let _guard = rt.handle().enter();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        crate::agent::spawn(config, broker, session, bus)
    }));
    match outcome {
        Ok(handle) => {
            let events = handle.subscribe();
            app.agent = Some(AgentRuntime {
                handle,
                events,
                last_error: None,
            });
            true
        }
        Err(_) => {
            app.toast(
                NoticeLevel::Error,
                t(ASST_RUNTIME_MISSING, lang).to_string(),
            );
            app.assistant.status = t(ASST_UNAVAILABLE, lang).to_string();
            false
        }
    }
}

/// Send the composed question (Ctrl+Enter).
pub fn submit(app: &mut App) {
    let question = app.assistant.composer.as_str().trim().to_string();
    if question.is_empty() || app.assistant.busy {
        return;
    }
    if !ensure_agent(app) {
        return;
    }
    app.assistant.composer.clear();
    app.assistant.entries.push(Entry::User(question.clone()));
    // The reply lands at the end of the transcript: follow it instead of
    // leaving the view pinned where the previous turn ended.
    app.center_scroll = super::layout::PIN_END;
    app.assistant.streaming.clear();
    app.assistant.busy = true;
    app.assistant.status = String::new();
    if let Some(agent) = &app.agent {
        agent.handle.ask(question);
    }
}

/// Drain pending agent events (once per loop tick).
pub fn poll(app: &mut App) {
    let mut batch = Vec::new();
    if let Some(agent) = &mut app.agent {
        loop {
            match agent.events.try_recv() {
                Ok(event) => batch.push(event),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
            }
        }
    }
    for event in batch {
        apply_event(app, event);
    }
}

/// Fold one [`AgentEvent`] into the panel.
pub fn apply_event(app: &mut App, event: AgentEvent) {
    match event {
        AgentEvent::MessageStart { role } => {
            if role == "assistant" {
                app.assistant.streaming.clear();
            }
        }
        AgentEvent::AssistantDelta(text) => app.assistant.streaming.push_str(&text),
        AgentEvent::ToolStart { name, args } => {
            if !app.assistant.streaming.is_empty() {
                let text = std::mem::take(&mut app.assistant.streaming);
                app.assistant.entries.push(Entry::Assistant(text));
            }
            // The panel prints these arguments verbatim, and `set_wifi` carries
            // the device password in them — the approval dialog and the
            // `ToolEnd` result below both redact first.
            app.assistant.entries.push(Entry::ToolStart {
                name,
                args: super::replies::redact_tool_args(&args),
            });
        }
        AgentEvent::ToolEnd { name, result, ok } => {
            let result = super::replies::redact_secrets(&result);
            app.assistant
                .entries
                .push(Entry::ToolEnd { name, result, ok });
        }
        AgentEvent::Usage {
            input,
            output,
            cache_read,
            cost,
            ..
        } => {
            app.assistant.usage = Some((input, output, cache_read, cost));
        }
        AgentEvent::RunFinished { reason } => {
            if !app.assistant.streaming.is_empty() {
                let text = std::mem::take(&mut app.assistant.streaming);
                app.assistant.entries.push(Entry::Assistant(text));
            }
            app.assistant.busy = false;
            // Same text as the toast two lines below: the status line must not
            // be the one surface that keeps the unredacted copy.
            app.assistant.status = super::replies::redact_secrets(&reason);
        }
        AgentEvent::Error(message) => {
            if !app.assistant.streaming.is_empty() {
                let text = std::mem::take(&mut app.assistant.streaming);
                app.assistant.entries.push(Entry::Assistant(text));
            }
            app.assistant.busy = false;
            app.assistant.status = super::replies::redact_secrets(&message);
            app.toast(NoticeLevel::Error, super::replies::redact_secrets(&message));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_labels_and_help_match_the_spec() {
        // Picker rows: WEB_UX_SPEC §7.2 and `web/agent_panel.js` radio list.
        assert_eq!(t(MODE_HELP[0].0, Lang::En), "Manual");
        assert_eq!(t(MODE_HELP[1].0, Lang::En), "Auto · Recommended");
        assert_eq!(t(MODE_HELP[2].0, Lang::En), "Full Auto");
        assert_eq!(
            t(MODE_HELP[1].1, Lang::En),
            "Low-risk queries run at a recognized shell prompt; other input needs approval. Destructive commands need approval in every mode."
        );
        assert_eq!(
            t(MODE_HELP[2].1, Lang::En),
            "Commands run without confirmation. Recognized destructive or irreversible commands — recursive/forced deletes, disk and filesystem tools, dd, flashing and bootloader tools, downloaded content piped into a shell, privilege escalation, recursive permission changes — still need your approval. Detection does not cover every operation inside scripts or indirect execution."
        );
        // The Chinese column carries the same three modes with the panel's own
        // wording: the web panel leaves `Auto · 推荐` and `Full Auto` as they are.
        assert_eq!(t(MODE_HELP[0].0, Lang::Zh), "手动");
        assert_eq!(t(MODE_HELP[1].0, Lang::Zh), "Auto · 推荐");
        assert_eq!(t(MODE_HELP[2].0, Lang::Zh), "Full Auto");
        assert_eq!(
            t(MODE_HELP[0].1, Lang::Zh),
            "AI 提出命令建议；点击发送后才会输入到被控机。"
        );
        assert_eq!(
            t(MODE_HELP[1].1, Lang::Zh),
            "识别到 Shell 提示符时自动执行低风险查询，其余输入需确认；破坏性命令在任何档位都需确认。"
        );
        assert_eq!(
            t(MODE_HELP[2].1, Lang::Zh),
            "命令直接执行；识别出的破坏性或不可逆命令（递归/强制删除、磁盘与文件系统工具、dd、刷写与引导工具、下载内容管道进 shell、提权、递归改权限）仍需确认。该检测不覆盖脚本或间接执行中的所有操作。"
        );
        // The header caption is the panel button, not the picker row: `Auto` in
        // every language (`web/agent_panel.js` `refreshMode`).
        assert_eq!(
            super::super::status::mode_text(ExecMode::Auto, Lang::En),
            "Auto"
        );
        assert_eq!(
            super::super::status::mode_text(ExecMode::Auto, Lang::Zh),
            "Auto"
        );
        assert_eq!(
            super::super::status::mode_text(ExecMode::FullAuto, Lang::Zh),
            "Full Auto"
        );
    }

    #[test]
    fn mode_index_round_trips() {
        for index in 0..3 {
            let mode = AssistantState::exec_of(index);
            assert_eq!(AssistantState::mode_index(mode), index);
        }
        assert_eq!(AssistantState::exec_of(9), ExecMode::Auto);
    }

    /// A bare Enter is a newline; both send chords take the submit path.
    /// Regression: only CONTROL was checked, and no terminal can encode
    /// `Ctrl+Enter` in the legacy key set (both are the byte `0x0d`), so
    /// sending was impossible on e.g. GNOME Terminal. `Alt+Enter` is the chord
    /// every terminal can actually transmit.
    #[test]
    fn alt_enter_sends_exactly_like_ctrl_enter() {
        let mut app = crate::tui::test_app();

        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            app.assistant.composer.as_str(),
            "\n",
            "Enter inserts a newline"
        );

        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            app.assistant.composer.clear();
            handle_key(&mut app, KeyEvent::new(KeyCode::Enter, modifiers));
            assert_eq!(
                app.assistant.composer.as_str(),
                "",
                "{modifiers:?} + Enter must submit, not insert a newline"
            );
        }
    }

    #[test]
    fn cost_formatting_follows_the_spec_thresholds() {
        assert_eq!(format_cost(1.5), "$1.50");
        assert_eq!(format_cost(0.5), "$0.500");
        assert_eq!(format_cost(0.005), "$0.0050");
        assert_eq!(format_cost(0.02), "$0.020");
    }

    #[test]
    fn markdown_marks_bold_and_code_runs() {
        let spans = markdown("run `ls` and **verify** it");
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "run ls and verify it");
        assert!(
            spans[1].style.add_modifier.contains(Modifier::BOLD) || spans[1].style.fg.is_some()
        );
    }

    #[test]
    fn usage_line_uses_grouped_counts() {
        let mut state = AssistantState {
            usage: Some((1234, 56, 7, 0.0)),
            ..AssistantState::default()
        };
        let line = state.usage_line(Lang::En).unwrap();
        assert_eq!(
            line,
            "Tokens this conversation 1,290 · ↑1,234 ↓56 ⚡7 · Prices not set"
        );
        state.usage = Some((100, 50, 0, 0.5));
        let line = state.usage_line(Lang::En).unwrap();
        assert!(line.ends_with("· Estimated cost ~$0.500"), "{line}");

        // Same numbers in Chinese, counts and all.
        let zh = state.usage_line(Lang::Zh).unwrap();
        assert_eq!(zh, "本次对话 tokens 150 · ↑100 ↓50 · 预估成本 ~$0.500");
    }

    /// Both languages of every assistant message carry text and differ.
    #[test]
    fn every_assistant_message_is_translated() {
        super::super::i18n::assert_bilingual(ALL);
        assert!(ALL.len() >= 27, "the assistant view carries 27 messages");
    }

    /// Chinese has to reach the rendered panel, and the stop notice — which
    /// `palette::stop_agent` pushes when a run is cancelled — keeps its
    /// wording in both languages.
    #[test]
    fn chinese_reaches_the_panel_and_the_stop_notice_is_pinned() {
        assert_eq!(t(ASST_AGENT, Lang::Zh), "助手");
        assert_eq!(t(ASST_SPEAKER_YOU, Lang::En), "you › ");
        assert_eq!(
            t(ASST_KEYS_LINE_2, Lang::Zh),
            "Ctrl+Shift+N 新建对话 · Esc 返回终端 · Ctrl+Q 退出 TUI"
        );
        assert!(!t(ASST_EMPTY_HINT, Lang::Zh).contains("Ask a question about the device"));
        assert_eq!(
            t(ASST_STOP_MESSAGE, Lang::En),
            "Stopped. Already sent input cannot be recalled; use Ctrl-C in the terminal to interrupt the target program."
        );
        assert_eq!(
            t(ASST_STOP_MESSAGE, Lang::Zh),
            "已停止。已发送的输入无法撤回；在终端用 Ctrl-C 中断目标程序。"
        );
    }

    /// The composer row is `› ` plus what is typed and has to fit the pane:
    /// the center pane wraps with `trim: false`, so an over-long row was
    /// carried onto the next line and pushed the key hints down with it.
    /// Counting characters put the Chinese line one column over for every
    /// glyph it kept, and the old cut built the row two columns wider than
    /// the pane even in English.
    #[test]
    fn the_composer_row_stays_inside_the_pane() {
        // `render_lines` floors the layout at 20 columns, so the pane widths
        // it can actually be given start there.
        for width in [20u16, 21, 24, 40, 74, 120] {
            for text in [
                String::new(),
                "hello".to_string(),
                "中文".repeat(60),
                "mix中文abc".repeat(30),
            ] {
                let mut app = crate::tui::test_app();
                app.assistant.composer.set(text.clone());
                let lines = render_lines(&app, width);
                let row = lines
                    .iter()
                    .find(|line| line.spans.first().map(|s| s.content.as_ref()) == Some("› "))
                    .expect("the composer row is rendered");
                let spans: Vec<&str> = row.spans.iter().map(|s| s.content.as_ref()).collect();
                let cols: usize = spans
                    .iter()
                    .map(|s| unicode_width::UnicodeWidthStr::width(*s))
                    .sum();
                assert!(
                    cols <= width as usize,
                    "width {width}: the row is {cols} columns: {spans:?}"
                );

                // What survives is the *end* of the line: that is what the
                // composer shows you of the text you are typing.
                let head = spans.get(1).copied().unwrap_or_default();
                let tail = spans.get(2).copied().unwrap_or_default();
                let budget = (width as usize).saturating_sub(2);
                if unicode_width::UnicodeWidthStr::width(text.as_str()) <= budget {
                    assert_eq!(head, "", "width {width}: the text fits, nothing is cut");
                    assert_eq!(tail, text.as_str());
                } else {
                    assert_eq!(head, "…", "width {width}: something has to be cut");
                    assert!(
                        text.ends_with(tail),
                        "width {width}: {tail:?} is not the end of what was typed"
                    );
                }
            }
        }
    }

    #[test]
    fn tool_arguments_are_redacted_before_they_reach_the_transcript() {
        let mut app = crate::tui::test_app();
        apply_event(
            &mut app,
            AgentEvent::ToolStart {
                name: "set_wifi".to_string(),
                args: r#"{"action":"connect","ssid":"MyNet","password":"s3cret"}"#.to_string(),
            },
        );
        let row = match app.assistant.entries.last() {
            Some(Entry::ToolStart { name, args }) => (name.clone(), args.clone()),
            _ => panic!("expected a tool row"),
        };
        assert_eq!(row.0, "set_wifi");
        assert!(
            !row.1.contains("s3cret"),
            "the password reached the panel: {}",
            row.1
        );
        assert!(row.1.contains("<redacted>"), "{}", row.1);
        assert!(row.1.contains("MyNet"), "the SSID stays visible: {}", row.1);
    }

    #[test]
    fn the_status_line_is_redacted_like_the_toast() {
        let mut app = crate::tui::test_app();
        let token = "0123456789abcdef0123456789abcdef";
        apply_event(&mut app, AgentEvent::Error(format!("token={token}")));
        assert!(
            !app.assistant.status.contains(token),
            "status kept the token: {}",
            app.assistant.status
        );
        assert!(
            app.assistant.status.contains("<redacted>"),
            "{}",
            app.assistant.status
        );
    }

    #[test]
    fn a_pasted_line_cannot_push_the_composer_past_its_cap() {
        let mut app = crate::tui::test_app();
        app.assistant.composer.set("x".repeat(COMPOSER_MAX));
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            app.assistant.composer.as_str().chars().count(),
            COMPOSER_MAX
        );
    }
}
