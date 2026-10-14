//! The command palette (`Ctrl+P`): everything the interface can do, in one
//! searchable list.
//!
//! The registry is a plain `&'static [Action]`, so a test can assert its
//! completeness and its order without building an `App`. An action is a
//! function pointer rather than a message: the palette runs the same code the
//! keyboard shortcut runs, so the two can never diverge — a palette entry that
//! only *described* an action would be a second, untested implementation of it.
//!
//! Only actions that do something are listed. A registry padded out with
//! features that are not built is worse than a short one: the user cannot tell
//! a stubbed entry from a broken one.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::app::{App, Focus};
use crate::event::View;
use crate::i18n::{entry, Entry, Lang};
use crate::text::{self, fill};

/// The overlay's own vocabulary, next to the code that paints it.
const TITLE: Entry = [" Command palette (Ctrl+P) ", " 命令面板（Ctrl+P） "];
const NO_MATCH: Entry = ["No matching action.", "无匹配动作。"];
const SHORTCUT_HINT: Entry = ["run", "执行"];
const KEYS: Entry = [
    "↑↓ choose · Enter run · Esc close",
    "↑↓ 选择 · Enter 执行 · Esc 关闭",
];
const MSG_SAVED_LOG: Entry = ["Saved {} bytes to {}", "已保存 {} 字节到 {}"];
const MSG_SAVE_FAILED: Entry = ["Save failed: {}", "保存失败：{}"];

// Categories.
const CAT_VIEW: Entry = ["View", "视图"];
const CAT_FOCUS: Entry = ["Focus", "焦点"];
const CAT_CONNECTION: Entry = ["Connection", "连接"];
const CAT_TRANSFER: Entry = ["Transfer", "传输"];
const CAT_TERMINAL: Entry = ["Terminal", "终端"];
const CAT_APP: Entry = ["App", "应用"];

// Titles.
const T_VIEW_TERMINAL: Entry = ["Open terminal view", "打开终端视图"];
const T_VIEW_DIAGNOSTICS: Entry = ["Open diagnostics view", "打开诊断视图"];
const T_VIEW_NETWORK: Entry = ["Open network view", "打开网络视图"];
const T_VIEW_ASSISTANT: Entry = ["Open assistant view", "打开助手视图"];
const T_VIEW_TRANSFER: Entry = ["Open file transfer view", "打开文件传输视图"];
const T_FOCUS_SIDEBAR: Entry = ["Focus the sidebar", "聚焦侧栏"];
const T_FOCUS_TERMINAL: Entry = ["Focus the terminal", "聚焦终端"];
const T_CONNECT: Entry = ["Connect", "连接"];
const T_DISCONNECT: Entry = ["Disconnect", "断开连接"];
const T_TRANSPORT: Entry = ["Switch the transport", "切换传输方式"];
const T_XFER_SEND: Entry = ["Send a file… (ZMODEM)", "发送文件…（ZMODEM）"];
const T_XFER_RECEIVE: Entry = ["Receive files… (ZMODEM)", "接收文件…（ZMODEM）"];
const T_XFER_CANCEL: Entry = ["Cancel the transfer", "取消传输"];
const T_AUTOSCROLL: Entry = ["Toggle autoscroll", "切换自动滚动"];
const T_ECHO: Entry = ["Toggle local echo", "切换本地回显"];
const T_CLEAR: Entry = ["Clear the terminal", "清屏"];
const T_SAVE_LOG: Entry = ["Save the terminal log", "保存终端日志"];
const T_SCROLL_BOTTOM: Entry = ["Scroll to the newest line", "滚动到最新一行"];
const T_HELP: Entry = ["Keyboard help", "键盘帮助"];
const T_NOTICES: Entry = ["Notice log", "通知记录"];
const T_LANGUAGE: Entry = ["Switch language", "切换界面语言"];
const T_THEME: Entry = ["Toggle theme", "切换主题"];
const T_QUIT: Entry = ["Quit the TUI", "退出 TUI"];

/// One entry in the palette.
pub struct Action {
    /// The protocol identifier. Stays English in both languages, the way a
    /// command name does, and it is what the search matches first — including
    /// for an action whose title has been translated past recognition. It is a
    /// search key rather than a column: see [`lines_in`].
    pub id: &'static str,
    pub title: Entry,
    pub category: Entry,
    /// The chord that runs it from anywhere, or `""` when the palette is the
    /// only way in.
    pub shortcut: &'static str,
    pub run: fn(&mut App),
}

// --- the actions -------------------------------------------------------------

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

fn view_transfer(app: &mut App) {
    app.set_view(View::Transfer);
}

fn focus_sidebar(app: &mut App) {
    app.focus = Focus::Sidebar;
}

/// Focus the pane *and* show it. Setting the focus without moving the view
/// would leave the keys on a panel the current view does not read, which is the
/// same as a dead keyboard.
fn focus_terminal(app: &mut App) {
    app.set_view(View::Terminal);
    app.focus = Focus::Terminal;
}

fn connect(app: &mut App) {
    app.reconnect();
}

fn disconnect(app: &mut App) {
    app.disconnect();
}

fn toggle_transport(app: &mut App) {
    let notice = app.toggle_transport();
    app.push_notice(notice);
}

/// A transfer needs an argument, so these two open the dialog that asks for it
/// rather than starting anything: *which* file is not something a palette row
/// can say, and a row that guessed would be a row that transferred the wrong
/// thing.
fn send_file(app: &mut App) {
    crate::dialogs::prompt_send_file(app);
}

fn receive_files(app: &mut App) {
    crate::dialogs::prompt_receive(app);
}

fn cancel_transfer(app: &mut App) {
    app.cancel_transfer();
}

fn toggle_autoscroll(app: &mut App) {
    let on = !app.terminal.autoscroll;
    app.terminal.autoscroll = on;
    if on {
        app.terminal.scroll = 0;
    }
}

fn toggle_echo(app: &mut App) {
    app.terminal.echo = !app.terminal.echo;
}

fn clear_terminal(app: &mut App) {
    app.clear_terminal();
}

/// Save the pane to the default file and report it — the bytes and the name,
/// so the user knows where the log went rather than merely that it went.
fn save_log(app: &mut App) {
    let lang = app.lang;
    match app.save_terminal_log() {
        Ok((bytes, name)) => {
            let message = fill(entry(MSG_SAVED_LOG, lang), &[bytes.to_string(), name]);
            app.push_notice(crate::app::Notice {
                text: message,
                level: crate::event::NoticeLevel::Info,
            });
        }
        Err(err) => {
            let message = fill(entry(MSG_SAVE_FAILED, lang), &[err.to_string()]);
            app.push_notice(crate::app::Notice {
                text: message,
                level: crate::event::NoticeLevel::Error,
            });
        }
    }
}

fn scroll_bottom(app: &mut App) {
    app.scroll_terminal_to_bottom();
}

fn show_help(app: &mut App) {
    crate::dialogs::open_help(app);
}

fn show_notices(app: &mut App) {
    crate::dialogs::open_notices(app);
}

fn toggle_language(app: &mut App) {
    let message = app.toggle_language();
    app.push_notice(message);
}

fn toggle_theme(app: &mut App) {
    let notice = app.toggle_theme();
    app.push_notice(notice);
}

fn quit(app: &mut App) {
    app.request_quit();
}

/// The registry, in render order. Grouped by category, and within a
/// group in the order the design lists them.
pub const ACTIONS: &[Action] = &[
    // Views
    Action {
        id: "view.terminal",
        title: T_VIEW_TERMINAL,
        category: CAT_VIEW,
        shortcut: "F2",
        run: view_terminal,
    },
    Action {
        id: "view.diagnostics",
        title: T_VIEW_DIAGNOSTICS,
        category: CAT_VIEW,
        shortcut: "F3",
        run: view_diagnostics,
    },
    Action {
        id: "view.network",
        title: T_VIEW_NETWORK,
        category: CAT_VIEW,
        shortcut: "F4",
        run: view_network,
    },
    Action {
        id: "view.assistant",
        title: T_VIEW_ASSISTANT,
        category: CAT_VIEW,
        shortcut: "F5",
        run: view_assistant,
    },
    Action {
        id: "view.transfer",
        title: T_VIEW_TRANSFER,
        category: CAT_VIEW,
        shortcut: "F6",
        run: view_transfer,
    },
    // Focus
    Action {
        id: "focus.sidebar",
        title: T_FOCUS_SIDEBAR,
        category: CAT_FOCUS,
        shortcut: "Ctrl+Up",
        run: focus_sidebar,
    },
    Action {
        id: "focus.terminal",
        title: T_FOCUS_TERMINAL,
        category: CAT_FOCUS,
        shortcut: "Esc",
        run: focus_terminal,
    },
    // Connection
    Action {
        id: "connect",
        title: T_CONNECT,
        category: CAT_CONNECTION,
        shortcut: "",
        run: connect,
    },
    Action {
        id: "disconnect",
        title: T_DISCONNECT,
        category: CAT_CONNECTION,
        shortcut: "",
        run: disconnect,
    },
    Action {
        id: "transport.toggle",
        title: T_TRANSPORT,
        category: CAT_CONNECTION,
        shortcut: "",
        run: toggle_transport,
    },
    // Transfer
    Action {
        id: "xfer.send",
        title: T_XFER_SEND,
        category: CAT_TRANSFER,
        shortcut: "",
        run: send_file,
    },
    Action {
        id: "xfer.receive",
        title: T_XFER_RECEIVE,
        category: CAT_TRANSFER,
        shortcut: "",
        run: receive_files,
    },
    Action {
        id: "xfer.cancel",
        title: T_XFER_CANCEL,
        category: CAT_TRANSFER,
        shortcut: "Ctrl+X",
        run: cancel_transfer,
    },
    // Terminal
    Action {
        id: "term.autoscroll",
        title: T_AUTOSCROLL,
        category: CAT_TERMINAL,
        shortcut: "",
        run: toggle_autoscroll,
    },
    Action {
        id: "term.echo",
        title: T_ECHO,
        category: CAT_TERMINAL,
        shortcut: "",
        run: toggle_echo,
    },
    Action {
        id: "term.clear",
        title: T_CLEAR,
        category: CAT_TERMINAL,
        shortcut: "Ctrl+L",
        run: clear_terminal,
    },
    Action {
        id: "term.save_log",
        title: T_SAVE_LOG,
        category: CAT_TERMINAL,
        shortcut: "",
        run: save_log,
    },
    Action {
        id: "term.scroll_bottom",
        title: T_SCROLL_BOTTOM,
        category: CAT_TERMINAL,
        shortcut: "Shift+End",
        run: scroll_bottom,
    },
    // App
    Action {
        id: "app.help",
        title: T_HELP,
        category: CAT_APP,
        shortcut: "F1",
        run: show_help,
    },
    Action {
        id: "app.notices",
        title: T_NOTICES,
        category: CAT_APP,
        shortcut: "",
        run: show_notices,
    },
    Action {
        id: "app.language",
        title: T_LANGUAGE,
        category: CAT_APP,
        shortcut: "",
        run: toggle_language,
    },
    Action {
        id: "app.theme",
        title: T_THEME,
        category: CAT_APP,
        shortcut: "Ctrl+T",
        run: toggle_theme,
    },
    Action {
        id: "app.quit",
        title: T_QUIT,
        category: CAT_APP,
        shortcut: "Ctrl+Q",
        run: quit,
    },
];

/// The open state of the palette.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaletteState {
    pub query: String,
    pub selected: usize,
}

impl PaletteState {
    /// A fresh overlay on an empty query.
    pub fn new() -> PaletteState {
        PaletteState::default()
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

    /// Move up one match, wrapping past the first row back to the last.
    pub fn step_up(&mut self, total: usize) {
        self.selected = if self.selected == 0 {
            total.saturating_sub(1)
        } else {
            self.selected - 1
        };
        self.clamp_to(total);
    }

    /// Keep the selection inside `total` — typing may have shrunk the list
    /// under it, and a selection past the end renders nothing at all.
    pub fn clamp_to(&mut self, total: usize) {
        self.selected = self.selected.min(total.saturating_sub(1));
    }
}

/// Indices of the actions matching `query`: a case-insensitive substring of the
/// id, either half of the title, or either half of the category. A Chinese
/// needle finds the Chinese labels; an English one still finds the same rows
/// through the English half.
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

/// How many rows the overlay paints at once.
pub const MAX_VISIBLE: usize = 14;

/// First visible row for `selected`. The window stays full while it scrolls:
/// a window that shrank near the end would resize the overlay under the user's
/// cursor.
pub fn window_start(selected: usize, total: usize, visible: usize) -> usize {
    selected
        .saturating_sub(visible / 2)
        .min(total.saturating_sub(visible))
}

/// The overlay's border title, spaces included so the painter can pass it on
/// untouched.
pub fn panel_title(lang: Lang) -> &'static str {
    entry(TITLE, lang)
}

/// Columns of the padded title and category fields. Measured in *display*
/// columns: `{:<28}` counts characters, which walks the columns out of line as
/// soon as a label is Chinese and two columns wide per glyph.
const TITLE_WIDTH: usize = 28;
const CATEGORY_WIDTH: usize = 12;

/// The body lines, painted in `theme`. `visible` is how many rows there is room
/// for, so the painter's height and the key handler's arithmetic agree.
pub fn lines_in(
    state: &PaletteState,
    lang: Lang,
    visible: usize,
    theme: &crate::theme::Theme,
) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(vec![
        Span::styled("› ", theme.focus().add_modifier(Modifier::BOLD)),
        Span::styled(
            state.query.clone(),
            theme.text().add_modifier(Modifier::BOLD),
        ),
    ])];
    out.push(Line::from(Span::styled("─".repeat(56), theme.muted())));
    let found = matches(&state.query);
    if found.is_empty() {
        out.push(Line::from(Span::styled(
            entry(NO_MATCH, lang),
            theme.muted(),
        )));
        return out;
    }
    let visible = visible.clamp(1, MAX_VISIBLE);
    let start = window_start(state.selected, found.len(), visible);
    for (row, index) in found.iter().skip(start).take(visible).enumerate() {
        let action = &ACTIONS[*index];
        let here = start + row == state.selected;
        // The highlighted row is one chip: the ground and the ink that sits on
        // it, padding included, so the fill spans the whole line.
        let (title, category, key, marker) = if here {
            let ink = theme.selected();
            (ink, ink, ink, ink)
        } else {
            (theme.text(), theme.focus(), theme.focus(), theme.accent())
        };
        let shortcut = if action.shortcut.is_empty() {
            String::new()
        } else {
            format!("  {}", action.shortcut)
        };
        // The id is the *search key*, not a column: the centre column of a
        // real terminal is about fifty columns, and a title, a category, a
        // shortcut and a protocol id do not fit in it. The title and the
        // shortcut are what a user reads; the id is what they can type.
        out.push(Line::from(vec![
            Span::styled(if here { "▸ " } else { "  " }, marker),
            Span::styled(text::pad_to(entry(action.title, lang), TITLE_WIDTH), title),
            Span::styled(
                text::pad_to(entry(action.category, lang), CATEGORY_WIDTH),
                category,
            ),
            Span::styled(shortcut, key),
        ]));
    }
    out
}

/// The key legend under the window, so the overlay is operable without the help
/// overlay to explain it.
pub fn legend(lang: Lang, theme: &crate::theme::Theme) -> Line<'static> {
    Line::from(Span::styled(
        format!("{} · {}", entry(SHORTCUT_HINT, lang), entry(KEYS, lang)),
        theme.muted(),
    ))
}

/// Handle a key while the palette is open. Returns true when the key was
/// consumed.
///
/// A control chord is deliberately *not* consumed — `Ctrl+T` must still repaint
/// and `Ctrl+Q` still quit with the palette in the way — except `Ctrl+U`, the
/// one control chord that means something *inside* a search box.
pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.palette.is_none() {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        if key.code != KeyCode::Char('u') {
            return false;
        }
        if let Some(state) = app.palette.as_mut() {
            state.query.clear();
            state.selected = 0;
        }
        return true;
    }
    let Some(state) = app.palette.as_mut() else {
        return false;
    };
    let total = matches(&state.query).len();
    match key.code {
        KeyCode::Esc => app.palette = None,
        KeyCode::Enter => {
            let picked = matches(&state.query).get(state.selected).copied();
            app.palette = None;
            if let Some(index) = picked {
                (ACTIONS[index].run)(app);
            }
        }
        KeyCode::Up | KeyCode::BackTab => state.step_up(total),
        KeyCode::Down | KeyCode::Tab => state.step_down(total),
        KeyCode::Home => state.selected = 0,
        KeyCode::End => state.selected = total.saturating_sub(1),
        KeyCode::Backspace => {
            state.query.pop();
            state.selected = 0;
        }
        // A function key is the *view* rota's, not a query character: the user
        // who reaches for F2 with the palette open wants the view, not an `F2`
        // typed into a search box that cannot match it.
        KeyCode::F(_) => return false,
        // Anything else printable is a query character.
        KeyCode::Char(c) => {
            state.query.push(c);
            state.selected = 0;
        }
        _ => {}
    }
    // Typing may have shrunk the list under the selection; put it back in range
    // before the next frame paints nothing.
    if let Some(state) = app.palette.as_mut() {
        state.clamp_to(matches(&state.query).len());
    }
    true
}

/// Open the palette.
pub fn open(app: &mut App) {
    app.palette = Some(PaletteState::new());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    /// The registry is the contract: every id, in order. Adding an action means
    /// adding it here too, which is the point — the list is what the palette
    /// promises the user.
    const CANONICAL: &[&str] = &[
        "view.terminal",
        "view.diagnostics",
        "view.network",
        "view.assistant",
        "view.transfer",
        "focus.sidebar",
        "focus.terminal",
        "connect",
        "disconnect",
        "transport.toggle",
        "xfer.send",
        "xfer.receive",
        "xfer.cancel",
        "term.autoscroll",
        "term.echo",
        "term.clear",
        "term.save_log",
        "term.scroll_bottom",
        "app.help",
        "app.notices",
        "app.language",
        "app.theme",
        "app.quit",
    ];

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

    #[test]
    fn the_registry_is_complete_ordered_and_unique() {
        let ids: Vec<&str> = ACTIONS.iter().map(|action| action.id).collect();
        assert_eq!(ids, CANONICAL, "the palette drifted from its contract");
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "two actions share an id");
    }

    /// Every action is labelled in both languages, and the ids and shorthands
    /// stay protocol / key literals — a translated id would break a script that
    /// searched for it.
    #[test]
    fn every_action_is_bilingual() {
        let mut entries = Vec::new();
        for action in ACTIONS {
            entries.push(action.title);
            entries.push(action.category);
            assert!(
                !action.id.is_empty() && action.id.is_ascii(),
                "{} is not a protocol literal",
                action.id
            );
        }
        crate::i18n::assert_bilingual(&entries);
        crate::i18n::assert_bilingual(&[
            TITLE,
            NO_MATCH,
            SHORTCUT_HINT,
            KEYS,
            MSG_SAVED_LOG,
            MSG_SAVE_FAILED,
            CAT_VIEW,
            CAT_FOCUS,
            CAT_CONNECTION,
            CAT_TRANSFER,
            CAT_TERMINAL,
            CAT_APP,
        ]);
    }

    /// The search matches the id, the title and the category, in both
    /// languages, and an empty query matches everything.
    #[test]
    fn the_search_matches_id_title_and_category() {
        assert_eq!(matches("").len(), ACTIONS.len());
        assert_eq!(
            matches("term.save_log"),
            vec![ACTIONS
                .iter()
                .position(|action| action.id == "term.save_log")
                .expect("the action exists")]
        );
        let found = matches("assistant");
        let ids: Vec<&str> = found.iter().map(|i| ACTIONS[*i].id).collect();
        assert!(ids.contains(&"view.assistant"), "{ids:?}");

        // Chinese finds the Chinese labels.
        assert_eq!(
            matches("清屏"),
            vec![ACTIONS
                .iter()
                .position(|action| action.id == "term.clear")
                .expect("the action exists")]
        );
        assert!(matches("nothing-matches-this-at-all").is_empty());
    }

    /// The window stays full and scrolls one row at a time, so the overlay does
    /// not resize under the cursor.
    #[test]
    fn the_window_stays_full_while_scrolling() {
        let total = ACTIONS.len();
        let visible = MAX_VISIBLE;
        let mut previous = 0;
        for selected in 0..total {
            let start = window_start(selected, total, visible);
            assert!(
                start + visible <= total,
                "selected {selected}: window overflows"
            );
            assert!(
                start <= selected && selected < start + visible,
                "selected {selected} is off screen"
            );
            assert!(start - previous <= 1, "the window jumped");
            previous = start;
        }
        assert_eq!(window_start(total - 1, total, visible), total - visible);
    }

    /// The selection wraps at both ends and survives a shrinking query.
    #[test]
    fn the_selection_wraps_and_clamps() {
        let total = ACTIONS.len();
        let mut state = PaletteState::new();
        state.step_up(total);
        assert_eq!(state.selected, total - 1);
        state.step_down(total);
        assert_eq!(state.selected, 0);

        let mut state = PaletteState {
            query: "term".to_string(),
            selected: 99,
        };
        state.clamp_to(matches(&state.query).len());
        assert!(state.selected < matches(&state.query).len());

        // An empty result set stays put rather than underflowing.
        let mut empty = PaletteState::new();
        empty.step_up(0);
        empty.step_down(0);
        assert_eq!(empty.selected, 0);
    }

    /// The rendered body has the prompt first (that is where the cursor goes),
    /// one marked row, and the same height at the top, middle and bottom.
    #[test]
    fn the_body_keeps_its_shape_and_marks_exactly_one_row() {
        let mut state = PaletteState::new();
        let top = lines_in(&state, Lang::En, MAX_VISIBLE, &Theme::DARK);
        state.selected = ACTIONS.len() / 2;
        let middle = lines_in(&state, Lang::En, MAX_VISIBLE, &Theme::DARK);
        state.selected = ACTIONS.len() - 1;
        let bottom = lines_in(&state, Lang::En, MAX_VISIBLE, &Theme::DARK);

        // prompt, separator, then a full window (the list is longer than one).
        assert_eq!(top.len(), 2 + MAX_VISIBLE);
        assert_eq!(top.len(), middle.len());
        assert_eq!(bottom.len(), middle.len());
        for body in [&top, &middle, &bottom] {
            assert_eq!(body[0].spans[0].content, "› ");
            let marked = body.iter().filter(|l| l.spans[0].content == "▸ ").count();
            assert_eq!(marked, 1, "one row is the selection");
        }
    }

    /// Chinese reaches the rows and the categories; the key hints stay literal.
    #[test]
    fn the_overlay_follows_the_language() {
        let state = PaletteState::new();
        let zh = text_of(&lines_in(&state, Lang::Zh, MAX_VISIBLE, &Theme::DARK));
        assert!(zh.contains("打开终端视图"), "{zh}");
        assert!(zh.contains("视图"), "the category translates: {zh}");
        assert!(zh.contains("F2"), "key hints stay literal: {zh}");
        assert!(!zh.contains("Open terminal view"), "{zh}");
        assert!(!zh.contains("View "), "the English category is gone: {zh}");

        let en = text_of(&lines_in(&state, Lang::En, MAX_VISIBLE, &Theme::DARK));
        assert!(en.contains("Open terminal view"), "{en}");
        assert!(!en.contains("打开终端视图"), "{en}");

        assert_eq!(panel_title(Lang::En), " Command palette (Ctrl+P) ");
        assert_eq!(panel_title(Lang::Zh), " 命令面板（Ctrl+P） ");

        let none = text_of(&lines_in(
            &PaletteState {
                query: "no-such-action".into(),
                selected: 0,
            },
            Lang::Zh,
            MAX_VISIBLE,
            &Theme::DARK,
        ));
        assert!(none.contains("无匹配动作。"), "{none}");
    }

    /// The padded columns are measured in display columns, so a Chinese row
    /// lines its columns up with an English one; for ASCII labels the bytes are
    /// exactly what `format!("{:<28}")` would have produced.
    #[test]
    fn the_columns_are_measured_in_display_columns() {
        for action in ACTIONS {
            let en = entry(action.title, Lang::En);
            assert_eq!(text::pad_to(en, TITLE_WIDTH), format!("{en:<28}"), "{en}");
        }
        // A Chinese title is two columns per glyph, so the padding shrinks by
        // twice the character count — never to nothing, never negative.
        let zh = entry(T_VIEW_TERMINAL, Lang::Zh);
        assert_eq!(text::columns(&text::pad_to(zh, TITLE_WIDTH)), TITLE_WIDTH);
    }

    /// Every key the overlay claims to handle really does something, and a key
    /// it does not handle is not swallowed by accident.
    #[test]
    fn the_keys_edit_the_query_and_move_the_selection() {
        let mut app = App::default();
        open(&mut app);
        assert!(app.palette.is_some());

        for c in "term".chars() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            );
        }
        assert_eq!(app.palette.as_ref().map(|s| s.query.as_str()), Some("term"));
        assert_eq!(app.palette.as_ref().map(|s| s.selected), Some(0));

        handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.palette.as_ref().map(|s| s.selected), Some(1));
        handle_key(&mut app, KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.palette.as_ref().map(|s| s.selected), Some(0));
        handle_key(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        let last = matches("term").len() - 1;
        assert_eq!(app.palette.as_ref().map(|s| s.selected), Some(last));

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.palette.as_ref().map(|s| s.query.as_str()), Some(""));

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        assert_eq!(app.palette.as_ref().map(|s| s.query.as_str()), Some(""));

        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.palette.is_none(), "Esc closes");
    }

    /// Enter runs the highlighted action and closes the overlay — the palette
    /// *does* the thing rather than describing it.
    #[test]
    fn enter_runs_the_highlighted_action() {
        let mut app = App::default();
        app.set_view(View::Network);
        open(&mut app);
        for c in "view.transfer".chars() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            );
        }
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.palette.is_none());
        assert_eq!(app.view, View::Transfer);
    }

    /// A Control chord is not a query character: `Ctrl+T` still switches the
    /// theme with the palette open.
    #[test]
    fn a_control_chord_is_not_typed_into_the_query() {
        let mut app = App::default();
        open(&mut app);
        let handled = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        );
        assert!(!handled, "the global handler owns the chord");
        assert_eq!(app.palette.as_ref().map(|s| s.query.as_str()), Some(""));
    }

    /// `fill` substitutes in order and leaves a template with more placeholders
    /// than arguments readable rather than eating the text.
    #[test]
    fn fill_substitutes_in_order() {
        assert_eq!(fill("a {} b {}", &["1".into(), "2".into()]), "a 1 b 2");
        assert_eq!(fill("no placeholders", &[]), "no placeholders");
        assert_eq!(fill("{} and {}", &["1".into()]), "1 and {}");
        assert_eq!(
            fill("已保存 {} 字节到 {}", &["41".into(), "x.log".into()]),
            "已保存 41 字节到 x.log"
        );
    }
}
