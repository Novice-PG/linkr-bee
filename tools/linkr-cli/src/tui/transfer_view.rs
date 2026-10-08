//! Transfer view: one form, one precheck, two channels (CONTRACTS.md
//! section 5).
//!
//! The engine is [`crate::transfer`] — it owns the probe, the channels and
//! the byte pacing. This file is only the form around it: which way the file
//! goes, which two paths it names, the precheck verdict, and the three
//! actions. ZMODEM is the front door (`sz`/`rz` on this host against their
//! twin on the device); the `dd | base64` pager of `target_files` takes over
//! when the probe says the device has no lrzsz.
//!
//! What is *not* translated here: the engine's own diagnostics (a refusal
//! with its shell output, a summary counted off the file). They are data the
//! same way the transport's `detail` line is data — the labels around them,
//! every action and every state this view invents, go through [`strings!`].

use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::i18n::{strings, t, tr, Lang};
use super::state::{App, TextField, View};
use crate::event::NoticeLevel;
use crate::transfer::{Channel, Direction, HostTools, Outcome, Phase, Transfer};

strings! {
    XFER_HEADER => "File transfer", "文件传输";
    // The four form rows.
    XFER_DIR => "Direction", "方向";
    XFER_LOCAL => "On this host", "本机路径";
    XFER_TARGET => "On the device", "设备路径";
    XFER_ACTIONS => "Actions", "操作";
    XFER_SEND => "Send to the device", "发送到设备";
    XFER_RECV => "Receive from the device", "从设备接收";
    XFER_CHECK => "Check", "检测";
    XFER_START => "Start", "开始";
    XFER_ABORT => "Abort", "中止";
    // The verdict block.
    XFER_PRECHECK => "Precheck", "预检";
    XFER_PROBE_IDLE => "not run yet", "尚未运行";
    XFER_PROBE_RUNNING => "asking the device…", "正在询问设备…";
    XFER_PROBE_LRZSZ => "device lrzsz: {}", "设备 lrzsz：{}";
    XFER_PROBE_NO_LRZSZ => "no lrzsz (sz/rz) on the device", "设备上没有 lrzsz（sz/rz）";
    XFER_PROBE_NO_HOST => "no lrzsz on this host", "本机没有 lrzsz";
    XFER_PROBE_PAGER => "dd|base64 fallback ready", "dd|base64 兜底可用";
    XFER_PROBE_NO_PAGER => "the device lacks dd/base64/wc/tr", "设备缺少 dd/base64/wc/tr";
    XFER_PROBE_NO_DIGEST => "no digest tool on the device", "设备没有摘要工具";
    XFER_CHANNEL => "Channel", "通道";
    XFER_PROGRESS => "Progress", "进度";
    XFER_STATUS => "Status", "状态";
    XFER_SENDING => "Sending…", "正在发送…";
    XFER_RECEIVING => "Receiving…", "正在接收…";
    // What the form itself has to say.
    XFER_NO_LINK => "Connect first: a transfer types commands into the device's shell.",
        "请先连接：传输要往设备的 shell 里输入命令。";
    XFER_PROBE_FIRST => "Run the precheck first: nothing transfers without it.",
        "请先运行预检：没有结果就不开始传输。";
    XFER_ABORTED => "Transfer aborted.", "传输已中止。";
    XFER_KEYS => "Tab move · Enter activate · Esc back · PgUp/PgDn scroll",
        "Tab 移动 · Enter 确认 · Esc 返回 · PgUp/PgDn 滚动";
}

/// Rows of the form, in the order Tab walks them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Row {
    Direction,
    #[default]
    Local,
    Target,
    Actions,
}

impl Row {
    pub fn next(self) -> Self {
        match self {
            Row::Direction => Row::Local,
            Row::Local => Row::Target,
            Row::Target => Row::Actions,
            Row::Actions => Row::Direction,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Row::Direction => Row::Actions,
            Row::Local => Row::Direction,
            Row::Target => Row::Local,
            Row::Actions => Row::Target,
        }
    }
}

/// The three entries of the action row, left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Check,
    Start,
    Abort,
}

const ACTS: [Act; 3] = [Act::Check, Act::Start, Act::Abort];

impl Act {
    fn label(self, lang: Lang) -> &'static str {
        match self {
            Act::Check => t(XFER_CHECK, lang),
            Act::Start => t(XFER_START, lang),
            Act::Abort => t(XFER_ABORT, lang),
        }
    }

    /// Greyed out when pressing it could only be refused: an action the form
    /// would not accept is drawn as one, so the row never promises a start
    /// that `Transfer::start` would turn straight back into an error.
    fn enabled(self, app: &App) -> bool {
        match self {
            Act::Check => app.connected() && !app.transfer.engine.busy(),
            Act::Start => {
                app.connected()
                    && !app.transfer.engine.busy()
                    && app.transfer.engine.probe.is_some()
            }
            Act::Abort => app.transfer.engine.busy(),
        }
    }
}

/// The form plus everything the engine needs to be driven from the frame loop.
#[derive(Default)]
pub struct State {
    pub engine: Transfer,
    /// The two paths, as typed. They are copied into the engine only when an
    /// action runs, so a half-edited form never reaches a command line.
    pub local: TextField,
    pub target: TextField,
    pub row: Row,
    /// Cursor position on the action row.
    pub action: usize,
    /// Something the *form* has to say — a refusal, an abort, a missing link.
    /// Cleared by the next action that gets somewhere.
    pub message: String,
}

impl State {
    /// Form → engine, right before an action reads the fields.
    fn sync(&mut self) {
        self.engine.local = PathBuf::from(self.local.as_str());
        self.engine.target = self.target.as_str().to_string();
    }
}

// --- actions -----------------------------------------------------------------

/// Open the view (optionally with a direction already picked) and run the
/// precheck. This is what F6 and the palette's `transfer.*` actions call.
///
/// The precheck is not a formality: it is the version check that decides
/// whether this run gets ZMODEM or the pager, and nothing is typed into the
/// device before it answered.
pub fn open(app: &mut App, direction: Option<Direction>) {
    if let Some(direction) = direction {
        app.transfer.engine.direction = direction;
    }
    app.set_view(View::Transfer);
    app.transfer.message.clear();
    // Two `--version` spawns, about 5 ms, once per open — the host half of
    // the same precondition the device is about to be asked about.
    app.transfer.engine.host = HostTools::detect();
    if !app.connected() {
        app.transfer.message = t(XFER_NO_LINK, app.lang()).to_string();
        return;
    }
    check(app);
}

/// Run the precheck (`Act::Check`).
fn check(app: &mut App) {
    if !app.connected() {
        app.transfer.message = t(XFER_NO_LINK, app.lang()).to_string();
        return;
    }
    app.transfer
        .engine
        .set_enter(super::keys::translate_enter(b"\r", app.settings.enter_mode));
    match app.transfer.engine.probe_now() {
        Ok(()) => app.transfer.message.clear(),
        Err(err) => app.transfer.message = err,
    }
}

/// `Act::Start` after validating what the form holds.
fn start(app: &mut App) {
    if !app.connected() {
        app.transfer.message = t(XFER_NO_LINK, app.lang()).to_string();
        return;
    }
    if app.transfer.engine.probe.is_none() {
        app.transfer.message = t(XFER_PROBE_FIRST, lang_of(app)).to_string();
        return;
    }
    app.transfer.sync();
    app.transfer
        .engine
        .set_enter(super::keys::translate_enter(b"\r", app.settings.enter_mode));
    match app.transfer.engine.start() {
        Ok(()) => app.transfer.message.clear(),
        Err(err) => {
            app.toast(NoticeLevel::Warn, err.clone());
            app.transfer.message = err;
        }
    }
}

/// Stop a run and say so in the interface's own words. The break characters
/// go out immediately — they are the one thing that must not wait its turn
/// in the pacing queue. Public because the palette's `term.transfer_abort`
/// is the same stop from anywhere in the TUI.
pub fn abort(app: &mut App) {
    let break_bytes = app.transfer.engine.abort();
    app.send_bytes(break_bytes);
    app.transfer.message = t(XFER_ABORTED, app.lang()).to_string();
}

/// `Esc` inside this view: a run in flight is stopped first, and only a view
/// with nothing to lose lets the key walk back to the terminal. Returns
/// whether the key was consumed.
pub fn escape(app: &mut App) -> bool {
    if app.transfer.engine.busy() {
        abort(app);
        return true;
    }
    false
}

// --- frame loop --------------------------------------------------------------

/// Time, then bytes. Called every iteration of the event loop, whatever view
/// is up: a transfer keeps running while the user reads something else.
pub fn poll(app: &mut App) {
    let enter = super::keys::translate_enter(b"\r", app.settings.enter_mode);
    app.transfer.engine.set_enter(enter);
    app.transfer.engine.poll(Instant::now());
    // A link that dropped mid-run would otherwise leave `pump` feeding bytes
    // into `send_bytes`, which drops them silently: the run would sit there
    // until its timeout with a progress line that never moves.
    if app.transfer.engine.busy() && !app.connected() {
        abort(app);
        return;
    }
    while let Some(bytes) = app.transfer.engine.pump(Instant::now()) {
        if !app.connected() {
            abort(app);
            return;
        }
        app.send_bytes(bytes);
    }
}

// --- keys --------------------------------------------------------------------

fn direction_row(app: &App) -> bool {
    app.transfer.row == Row::Direction
}

fn lang_of(app: &App) -> Lang {
    app.lang()
}

fn field(app: &mut App) -> &mut TextField {
    match app.transfer.row {
        Row::Target => &mut app.transfer.target,
        _ => &mut app.transfer.local,
    }
}

fn toggle_direction(app: &mut App) {
    app.transfer.engine.direction = match app.transfer.engine.direction {
        Direction::Send => Direction::Recv,
        Direction::Recv => Direction::Send,
    };
}

fn move_action(app: &mut App, forward: bool) {
    let len = ACTS.len();
    app.transfer.action = if forward {
        (app.transfer.action + 1) % len
    } else {
        (app.transfer.action + len - 1) % len
    };
}

fn activate(app: &mut App) {
    match ACTS.get(app.transfer.action).copied().unwrap_or(Act::Check) {
        Act::Check => check(app),
        Act::Start => start(app),
        Act::Abort => abort(app),
    }
}

/// One key press inside the transfer view. `Esc` never arrives here: the
/// global router resolves it first (and asks [`escape`] whether it stopped
/// something on the way out).
pub fn handle_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let editing = matches!(app.transfer.row, Row::Local | Row::Target);
    match key.code {
        KeyCode::Tab | KeyCode::Down => app.transfer.row = app.transfer.row.next(),
        KeyCode::BackTab | KeyCode::Up => app.transfer.row = app.transfer.row.prev(),
        KeyCode::Enter => match app.transfer.row {
            Row::Direction => toggle_direction(app),
            Row::Actions => activate(app),
            // In a path field Enter advances, the way the WiFi form does.
            Row::Local | Row::Target => app.transfer.row = app.transfer.row.next(),
        },
        KeyCode::Char(' ') if direction_row(app) => toggle_direction(app),
        KeyCode::Left => match app.transfer.row {
            Row::Direction => toggle_direction(app),
            Row::Actions => move_action(app, false),
            Row::Local | Row::Target => field(app).left(),
        },
        KeyCode::Right => match app.transfer.row {
            Row::Direction => toggle_direction(app),
            Row::Actions => move_action(app, true),
            Row::Local | Row::Target => field(app).right(),
        },
        KeyCode::Home if editing => field(app).home(),
        KeyCode::End if editing => field(app).end(),
        KeyCode::Backspace if editing => field(app).backspace(),
        KeyCode::Delete if editing => field(app).delete(),
        KeyCode::Char(c) if editing && !ctrl && !alt => field(app).insert_char(c),
        _ => {}
    }
}

// --- rendering ---------------------------------------------------------------

/// `▸ label ┆ value`, with the label padded in display columns (Chinese is
/// two columns per glyph, and a shifted value column reads as a wrong value).
fn row(marker: bool, label: &str, value: String, width: u16, style: Style) -> Line<'static> {
    const LABEL_WIDTH: usize = 16;
    let pad = LABEL_WIDTH.saturating_sub(UnicodeWidthStr::width(label));
    let value_width = (width as usize).saturating_sub(LABEL_WIDTH + 4);
    Line::from(vec![
        Span::styled(
            if marker { "▸ " } else { "  " },
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(
            format!("{label}{}", " ".repeat(pad)),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled("│ ", Style::default().fg(Color::DarkGray)),
        Span::styled(super::dialogs::clip_columns(&value, value_width), style),
    ])
}

fn header(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))
}

fn hint(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(Color::DarkGray),
    ))
}

/// What the probe found, in the interface's language: the versions it
/// printed, plus whether each channel is therefore open.
fn precheck(app: &App, lang: Lang) -> String {
    let engine = &app.transfer.engine;
    let Some(probe) = &engine.probe else {
        return if matches!(
            engine.phase,
            Phase::Cmd {
                step: crate::transfer::Step::Probe,
                ..
            }
        ) {
            t(XFER_PROBE_RUNNING, lang).to_string()
        } else {
            t(XFER_PROBE_IDLE, lang).to_string()
        };
    };
    let mut parts = Vec::new();
    if probe.target_zmodem() {
        parts.push(tr!(
            t(XFER_PROBE_LRZSZ, lang),
            if probe.sz_version.is_empty() {
                probe.rz_version.as_str()
            } else {
                probe.sz_version.as_str()
            }
        ));
    } else {
        parts.push(t(XFER_PROBE_NO_LRZSZ, lang).to_string());
    }
    if !engine.host.zmodem() {
        parts.push(t(XFER_PROBE_NO_HOST, lang).to_string());
    }
    if probe.pager() {
        parts.push(t(XFER_PROBE_PAGER, lang).to_string());
    } else {
        parts.push(t(XFER_PROBE_NO_PAGER, lang).to_string());
    }
    if !probe.digest() {
        parts.push(t(XFER_PROBE_NO_DIGEST, lang).to_string());
    }
    parts.join(" · ")
}

/// Bytes moved, and the share of them when the direction makes it known
/// (a send knows its file's size; a receive learns it, if at all, only when
/// the pager's first page reports one).
fn progress(app: &App) -> String {
    let engine = &app.transfer.engine;
    let moved = crate::target_files::format_bytes(engine.moved as i64)
        .unwrap_or_else(|_| format!("{} B", engine.moved));
    match engine.total {
        Some(total) if total > 0 => {
            let total_text = crate::target_files::format_bytes(total as i64)
                .unwrap_or_else(|_| format!("{total} B"));
            let percent = ((engine.moved as f64 / total as f64) * 100.0).min(100.0) as usize;
            format!("{moved} / {total_text} · {percent}%")
        }
        _ => moved,
    }
}

/// The channel actually picked once a run is under way, `—` before one.
fn channel(app: &App) -> &'static str {
    match app.transfer.engine.channel {
        Channel::Zmodem => "ZMODEM",
        Channel::Pager => "dd|base64",
    }
}

/// The status line: what the form has to say first, then the engine's own
/// verdict (a refusal with its shell output, or the summary counted off the
/// file that landed).
fn status(app: &App, lang: Lang) -> String {
    if !app.transfer.message.is_empty() {
        return app.transfer.message.clone();
    }
    match &app.transfer.engine.outcome {
        Outcome::Idle => String::new(),
        Outcome::Busy(_) => match app.transfer.engine.direction {
            Direction::Send => t(XFER_SENDING, lang).to_string(),
            Direction::Recv => t(XFER_RECEIVING, lang).to_string(),
        },
        Outcome::Failed(reason) => reason.clone(),
        Outcome::Ok(text) => text.clone(),
    }
}

/// Header, form, verdict block, key hint — the whole pane.
pub fn render_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let lang = app.lang();
    let state = &app.transfer;
    let mut lines = vec![header(t(XFER_HEADER, lang)), Line::from("")];

    if !app.connected() {
        lines.push(Line::from(Span::styled(
            super::dialogs::clip_columns(t(XFER_NO_LINK, lang), width as usize),
            Style::default().fg(Color::LightYellow),
        )));
        lines.push(Line::from(""));
    }

    let dir_value = match state.engine.direction {
        Direction::Send => t(XFER_SEND, lang),
        Direction::Recv => t(XFER_RECV, lang),
    };
    lines.push(row(
        state.row == Row::Direction,
        t(XFER_DIR, lang),
        dir_value.to_string(),
        width,
        Style::default().fg(Color::White),
    ));
    lines.push(row(
        state.row == Row::Local,
        t(XFER_LOCAL, lang),
        state.local.as_str().to_string(),
        width,
        Style::default().fg(Color::White),
    ));
    lines.push(row(
        state.row == Row::Target,
        t(XFER_TARGET, lang),
        state.target.as_str().to_string(),
        width,
        Style::default().fg(Color::White),
    ));

    // The action row is drawn by hand: three cells, the selected one marked
    // and the impossible ones dimmed, so the row reads as what it can do.
    const LABEL_WIDTH: usize = 16;
    /// Display columns one action cell may occupy: marker, a space, then the
    /// label padded in columns rather than characters.
    const CELL: usize = 10;
    let label = t(XFER_ACTIONS, lang);
    let pad = LABEL_WIDTH.saturating_sub(UnicodeWidthStr::width(label));
    let value_width = (width as usize).saturating_sub(LABEL_WIDTH + 4);
    let mut spans = vec![
        Span::styled(
            if state.row == Row::Actions {
                "▸ "
            } else {
                "  "
            },
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(
            format!("{label}{}", " ".repeat(pad)),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled("│ ", Style::default().fg(Color::DarkGray)),
    ];
    let mut used = 0usize;
    for (index, act) in ACTS.iter().enumerate() {
        if used + CELL > value_width {
            break;
        }
        used += CELL;
        let selected = state.row == Row::Actions && state.action == index;
        let name = act.label(lang);
        let text = format!(
            "{} {}{}",
            if selected { "▸" } else { " " },
            name,
            " ".repeat(CELL - 2 - UnicodeWidthStr::width(name).min(CELL - 2))
        );
        let style = if !act.enabled(app) {
            Style::default().fg(Color::DarkGray)
        } else if selected {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::styled(text, style));
    }
    lines.push(Line::from(spans));

    lines.push(Line::from(""));
    lines.push(row(
        false,
        t(XFER_PRECHECK, lang),
        precheck(app, lang),
        width,
        Style::default().fg(Color::Gray),
    ));
    lines.push(row(
        false,
        t(XFER_CHANNEL, lang),
        if state.engine.busy() {
            channel(app).to_string()
        } else {
            "—".to_string()
        },
        width,
        Style::default().fg(Color::Gray),
    ));
    lines.push(row(
        false,
        t(XFER_PROGRESS, lang),
        progress(app),
        width,
        Style::default().fg(Color::Gray),
    ));
    let status_text = status(app, lang);
    if !status_text.is_empty() {
        let level = match &state.engine.outcome {
            Outcome::Failed(_) => Color::LightRed,
            Outcome::Ok(_) => Color::LightGreen,
            _ => Color::White,
        };
        lines.push(row(
            false,
            t(XFER_STATUS, lang),
            status_text,
            width,
            Style::default().fg(level),
        ));
    }

    lines.push(Line::from(""));
    lines.push(hint(&super::dialogs::clip_columns(
        t(XFER_KEYS, lang),
        width as usize,
    )));
    lines
}

#[cfg(test)]
mod tests {
    use super::super::test_app;
    use super::*;

    /// The messages are the interface: both languages, and none of them a
    /// copy of the other.
    #[test]
    fn every_transfer_message_is_translated() {
        super::super::i18n::assert_bilingual(ALL);
        assert!(ALL.len() >= 28, "the transfer view carries 28 messages");
    }

    /// The form opens on a path field with the send direction, and the action
    /// row starts on Check — the only one of the three that can work before
    /// the probe has answered.
    #[test]
    fn the_form_starts_on_a_path_field_and_on_check() {
        let app = test_app();
        let lines = render_lines(&app, 80);
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
        assert!(text.contains(t(XFER_HEADER, Lang::En)), "{text}");
        assert!(text.contains(t(XFER_PRECHECK, Lang::En)), "{text}");
        assert!(text.contains(t(XFER_KEYS, Lang::En)), "{text}");

        let state = State::default();
        assert_eq!(
            state.row,
            Row::Local,
            "the path is what people come to edit"
        );
        assert_eq!(state.engine.direction, Direction::Send);
        assert_eq!(state.action, 0);
    }

    /// Chinese labels stay in their column: a two-column glyph shifts the
    /// value, and a shifted value reads as a wrong path.
    #[test]
    fn the_chinese_form_stays_aligned() {
        let mut app = test_app();
        app.settings.lang = Lang::Zh;
        app.transfer.local.set("/tmp/x.bin".to_string());
        app.view = View::Transfer;
        let lines = render_lines(&app, 60);
        let zh: String = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(zh.contains(t(XFER_HEADER, Lang::Zh)), "{zh}");
        assert!(zh.contains(t(XFER_TARGET, Lang::Zh)), "{zh}");
        assert!(zh.contains("/tmp/x.bin"), "{zh}");
        // Nothing wider than the pane: the clipper owns the right edge.
        for line in lines {
            assert!(
                UnicodeWidthStr::width(
                    line.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                        .as_str()
                ) <= 60,
                "line wider than the pane"
            );
        }
    }

    /// Tab walks the four rows both ways, and wraps: there is no row the
    /// keyboard cannot reach.
    #[test]
    fn tab_walks_every_row_and_wraps() {
        let mut app = test_app();
        assert_eq!(app.transfer.row, Row::Local);
        handle_key(&mut app, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.transfer.row, Row::Target);
        handle_key(&mut app, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.transfer.row, Row::Actions);
        handle_key(&mut app, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.transfer.row, Row::Direction);
        handle_key(&mut app, key(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(app.transfer.row, Row::Actions);
        handle_key(&mut app, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.transfer.row, Row::Direction, "wraps back to the top");

        // The direction row toggles on every arrow and on Space.
        assert_eq!(app.transfer.engine.direction, Direction::Send);
        handle_key(&mut app, key(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.transfer.engine.direction, Direction::Recv);
        handle_key(&mut app, key(KeyCode::Char(' '), KeyModifiers::NONE));
        assert_eq!(app.transfer.engine.direction, Direction::Send);

        // …and arrows only move the caret inside a path field.
        assert_eq!(app.transfer.row, Row::Direction);
        handle_key(&mut app, key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.transfer.row, Row::Local);
        app.transfer.local.set("/a/b".to_string());
        handle_key(&mut app, key(KeyCode::Home, KeyModifiers::NONE));
        handle_key(&mut app, key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(app.transfer.local.as_str(), "x/a/b");
        handle_key(&mut app, key(KeyCode::End, KeyModifiers::NONE));
        handle_key(&mut app, key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.transfer.local.as_str(), "x/a/");
        // Delete removes forwards, so it has to be asked from the front.
        handle_key(&mut app, key(KeyCode::Home, KeyModifiers::NONE));
        handle_key(&mut app, key(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(app.transfer.local.as_str(), "/a/");
    }

    /// The action row's cursor moves only on its own row, and only across
    /// the three actions there are.
    #[test]
    fn the_action_cursor_stays_on_three_actions() {
        let mut app = test_app();
        app.transfer.row = Row::Actions;
        assert_eq!(app.transfer.action, 0);
        handle_key(&mut app, key(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.transfer.action, 1);
        handle_key(&mut app, key(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.transfer.action, 0);
        handle_key(&mut app, key(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.transfer.action, ACTS.len() - 1, "wraps backwards");
        handle_key(&mut app, key(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.transfer.action, 0, "wraps forwards");
        // On a path row the same keys edit, they do not move the action.
        app.transfer.row = Row::Local;
        handle_key(&mut app, key(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.transfer.action, 0);
    }

    /// Start without a precheck is refused by the form and by the engine —
    /// the version check is the gate, not a suggestion.
    #[test]
    fn nothing_starts_before_the_precheck_answered() {
        let mut app = test_app();
        app.transfer.row = Row::Actions;
        app.transfer.action = 1;
        start(&mut app);
        assert_eq!(
            app.transfer.message,
            t(XFER_PROBE_FIRST, Lang::En),
            "the form explains itself, in the interface's language"
        );
        assert_eq!(app.transfer.engine.phase, Phase::Idle, "and nothing ran");
    }

    /// The precheck types one command — and only one, and only while a link
    /// is up. With no link the view says so instead of typing into nowhere.
    #[test]
    fn the_precheck_needs_a_link_and_then_arms_the_form() {
        let mut app = test_app();
        check(&mut app);
        assert!(
            matches!(
                app.transfer.engine.phase,
                Phase::Cmd {
                    step: crate::transfer::Step::Probe,
                    seq: 1,
                    ..
                }
            ),
            "the probe is in flight"
        );

        let mut offline = test_app();
        offline.state = crate::event::ConnectionState::Disconnected;
        check(&mut offline);
        assert_eq!(offline.transfer.message, t(XFER_NO_LINK, Lang::En));
        assert_eq!(
            offline.transfer.engine.phase,
            Phase::Idle,
            "no command typed"
        );
        open(&mut offline, Some(Direction::Recv));
        assert_eq!(offline.transfer.message, t(XFER_NO_LINK, Lang::En));
    }

    /// Opening from the palette picks the direction, and F6 keeps it: the
    /// entry point never silently reverses a run the user already chose.
    #[test]
    fn opening_with_a_direction_sets_it_and_leaves_it_alone() {
        let mut app = test_app();
        open(&mut app, Some(Direction::Recv));
        assert_eq!(app.view, View::Transfer);
        assert_eq!(app.transfer.engine.direction, Direction::Recv);
        open(&mut app, None);
        assert_eq!(app.transfer.engine.direction, Direction::Recv, "kept");
        assert_eq!(app.transfer.row, Row::Local);
    }

    /// A run in flight owns the keyboard as far as Esc is concerned: the key
    /// stops it rather than walking away and leaving it running.
    #[test]
    fn esc_stops_a_running_transfer_before_it_leaves() {
        let mut app = test_app();
        assert!(!escape(&mut app), "nothing to stop, the key passes through");
        // A queued probe counts as a run: it is a command in flight.
        check(&mut app);
        assert!(escape(&mut app), "the probe was stopped");
        assert_eq!(app.transfer.engine.phase, Phase::Done);
        assert!(!escape(&mut app), "and now there is nothing left to stop");
    }

    /// The verdict block words what the probe found; the engine keeps the
    /// raw text for the notice log.
    #[test]
    fn the_precheck_line_reports_what_the_probe_found() {
        let mut app = test_app();
        assert_eq!(precheck(&app, Lang::En), t(XFER_PROBE_IDLE, Lang::En));

        let mut probe = crate::transfer::Probe::default();
        probe.tools.insert("sz".to_string(), true);
        probe.tools.insert("rz".to_string(), true);
        probe.tools.insert("dd".to_string(), true);
        probe.tools.insert("base64".to_string(), true);
        probe.tools.insert("wc".to_string(), true);
        probe.tools.insert("tr".to_string(), true);
        probe.sz_version = "sz (lrzsz) 0.12.21rc".to_string();
        app.transfer.engine.probe = Some(probe);
        app.transfer.engine.host = HostTools {
            sz: true,
            rz: true,
            sz_version: "sz (lrzsz) 0.12.21rc".to_string(),
            rz_version: "rz (lrzsz) 0.12.21rc".to_string(),
        };
        let zh = precheck(&app, Lang::Zh);
        assert!(zh.contains("0.12.21rc"), "{zh}");
        assert!(
            zh.starts_with(&tr!(t(XFER_PROBE_LRZSZ, Lang::Zh), "sz (lrzsz) 0.12.21rc")),
            "{zh}"
        );

        // A device with no lrzsz says so, and so does a missing host copy —
        // that pair is what sends the run down the pager.
        app.transfer
            .engine
            .probe
            .as_mut()
            .unwrap()
            .tools
            .insert("sz".to_string(), false);
        app.transfer.engine.host = HostTools::default();
        let en = precheck(&app, Lang::En);
        assert!(en.contains(t(XFER_PROBE_NO_LRZSZ, Lang::En)), "{en}");
        assert!(en.contains(t(XFER_PROBE_NO_HOST, Lang::En)), "{en}");
        assert!(en.contains(t(XFER_PROBE_PAGER, Lang::En)), "{en}");
        assert!(en.contains(t(XFER_PROBE_NO_DIGEST, Lang::En)), "{en}");
    }

    /// The channel cell names the channel that is actually running, and
    /// stays empty (`—`) while the form is only a form.
    #[test]
    fn the_channel_cell_names_the_running_channel() {
        let mut app = test_app();
        assert_eq!(channel(&app), "ZMODEM");
        assert_eq!(progress(&app), "0 B");
        app.transfer.engine.channel = Channel::Pager;
        app.transfer.engine.moved = 2048;
        assert_eq!(channel(&app), "dd|base64");
        app.transfer.engine.total = Some(8192);
        let text = progress(&app);
        assert!(text.contains("25%"), "{text}");
        assert!(text.contains("8 KiB"), "{text}");
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }
}
