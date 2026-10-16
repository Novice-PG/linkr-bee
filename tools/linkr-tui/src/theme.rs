//! Visual theme: every colour and piece of chrome the TUI paints, in one place.
//!
//! The tokens are the web client's design system (`web/style.css`) carried over
//! to ratatui true colour, so the terminal and the browser stay the same
//! product: near-black ground, panel surfaces, one green accent for everything
//! live, blue for focus, red for danger, amber for warnings. A view never asks
//! "is this the light theme?" — it asks for a *role* (accent, muted, danger…)
//! and gets the current theme's answer, which is why switching themes repaints
//! every surface on the next frame with no view aware of it.
//!
//! The theme is passed explicitly rather than kept in a global: render code
//! takes `&Theme`, so tests can paint either theme with no shared state and a
//! repaint can never observe a half-changed palette.
//!
//! Terminal emulators without true colour approximate the RGB values; every
//! token stays legible when they do, because each pair keeps its contrast role.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Borders};

use crate::event::{ConnectionState, NoticeLevel};

/// Every colour the interface paints. Both themes carry the same roles; only
/// the values move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Page ground behind every surface (`--bg`).
    pub bg: Color,
    /// Sidebar, bars and framed views (`--panel`).
    pub panel: Color,
    /// Secondary surface: table headers, the composer (`--panel-2`).
    pub panel_2: Color,
    /// Selected rows, toasts (`--panel-3`).
    pub panel_3: Color,
    /// Primary text (`--text`).
    pub text: Color,
    /// Secondary text: hints, separators, counters (`--muted`).
    pub muted: Color,
    /// Resting borders and separators (`--line`).
    pub line: Color,
    /// Borders of the window that has the keyboard (`--line-strong`).
    pub line_strong: Color,
    /// The one live colour: selection, connect, ready (`--accent`).
    pub accent: Color,
    /// Bright accent: the connected dot, bright markers (`--accent-strong`).
    pub accent_strong: Color,
    /// Text on an accent fill (`--accent-ink`).
    pub accent_ink: Color,
    /// Keyboard focus: labels, prompts, the caret (`--focus`).
    pub focus: Color,
    /// Errors and destructive actions (`--danger`).
    pub danger: Color,
    /// Warnings and the connecting state (the web amber family).
    pub warn: Color,
    /// The serial terminal's own ground, as xterm.js paints it (`--terminal`).
    pub terminal_bg: Color,
    /// The serial terminal's default foreground (xterm theme `fg`).
    pub terminal_fg: Color,
    /// The sixteen ANSI colours a device's output can name.
    ///
    /// These are the palette the browser's xterm.js instance resolves to, so a
    /// red error in the pane is the same red in both clients. The web client
    /// only overrides a handful of xterm's defaults and leaves the rest; the
    /// one it *does* override, `brightBlack`, is the one carried here — it is
    /// what a great many tools paint their comments and separators in.
    pub ansi: [Color; 16],
}

impl Theme {
    /// The web client's `:root` — the default, and the one the browser shows.
    pub const DARK: Theme = Theme {
        bg: Color::Rgb(0x09, 0x0d, 0x14),
        panel: Color::Rgb(0x11, 0x16, 0x1f),
        panel_2: Color::Rgb(0x15, 0x1b, 0x26),
        panel_3: Color::Rgb(0x1c, 0x23, 0x30),
        text: Color::Rgb(0xe8, 0xee, 0xf7),
        muted: Color::Rgb(0x8d, 0x9b, 0xb0),
        line: Color::Rgb(0x24, 0x2d, 0x3d),
        line_strong: Color::Rgb(0x33, 0x40, 0x5a),
        accent: Color::Rgb(0x3f, 0xc8, 0x8a),
        accent_strong: Color::Rgb(0x6f, 0xe0, 0xab),
        accent_ink: Color::Rgb(0x05, 0x25, 0x17),
        focus: Color::Rgb(0x7a, 0xa8, 0xf8),
        danger: Color::Rgb(0xf2, 0x77, 0x7c),
        warn: Color::Rgb(0xf2, 0xc9, 0x4c),
        terminal_bg: Color::Rgb(0x05, 0x08, 0x0d),
        terminal_fg: Color::Rgb(0xd8, 0xf3, 0xdc),
        ansi: [
            Color::Rgb(0x2e, 0x34, 0x36),
            Color::Rgb(0xcc, 0x00, 0x00),
            Color::Rgb(0x4e, 0x9a, 0x06),
            Color::Rgb(0xc4, 0xa0, 0x00),
            Color::Rgb(0x34, 0x65, 0xa4),
            Color::Rgb(0x75, 0x50, 0x7b),
            Color::Rgb(0x06, 0x98, 0x9a),
            Color::Rgb(0xd3, 0xd7, 0xcf),
            Color::Rgb(0x8d, 0x9b, 0xb0),
            Color::Rgb(0xef, 0x29, 0x29),
            Color::Rgb(0x8a, 0xe2, 0x34),
            Color::Rgb(0xfc, 0xe9, 0x4f),
            Color::Rgb(0x72, 0x9f, 0xcf),
            Color::Rgb(0xad, 0x7f, 0xa8),
            Color::Rgb(0x34, 0xe2, 0xe2),
            Color::Rgb(0xee, 0xee, 0xec),
        ],
    };

    /// The web client's `[data-theme="light"]`. The terminal pair is the
    /// xterm.js `XTERM_LIGHT` theme, so the serial pane follows the chrome the
    /// way the browser's xterm instance does.
    pub const LIGHT: Theme = Theme {
        bg: Color::Rgb(0xee, 0xf2, 0xf7),
        panel: Color::Rgb(0xff, 0xff, 0xff),
        panel_2: Color::Rgb(0xf6, 0xf8, 0xfb),
        panel_3: Color::Rgb(0xe9, 0xee, 0xf5),
        text: Color::Rgb(0x0f, 0x1c, 0x2e),
        muted: Color::Rgb(0x5c, 0x6a, 0x82),
        line: Color::Rgb(0xdb, 0xe3, 0xee),
        line_strong: Color::Rgb(0xc3, 0xcf, 0xdf),
        accent: Color::Rgb(0x1f, 0x9d, 0x64),
        accent_strong: Color::Rgb(0x15, 0x7a, 0x4c),
        accent_ink: Color::Rgb(0xff, 0xff, 0xff),
        focus: Color::Rgb(0x3b, 0x82, 0xf6),
        danger: Color::Rgb(0xdc, 0x34, 0x34),
        // The web ships no `--warn`: toasts borrow the amber family. On a light
        // ground the dark amber is the one that keeps its contrast role.
        warn: Color::Rgb(0xb4, 0x53, 0x09),
        terminal_bg: Color::Rgb(0xf8, 0xfa, 0xfc),
        terminal_fg: Color::Rgb(0x1f, 0x6b, 0x43),
        // The same sixteen, with the light theme's own `brightBlack`. The rest
        // stay put because the browser's light terminal keeps them: changing
        // them here would make the pane disagree with xterm.js next to it.
        ansi: [
            Color::Rgb(0x2e, 0x34, 0x36),
            Color::Rgb(0xcc, 0x00, 0x00),
            Color::Rgb(0x4e, 0x9a, 0x06),
            Color::Rgb(0xc4, 0xa0, 0x00),
            Color::Rgb(0x34, 0x65, 0xa4),
            Color::Rgb(0x75, 0x50, 0x7b),
            Color::Rgb(0x06, 0x98, 0x9a),
            Color::Rgb(0xd3, 0xd7, 0xcf),
            Color::Rgb(0x5c, 0x6a, 0x82),
            Color::Rgb(0xef, 0x29, 0x29),
            Color::Rgb(0x8a, 0xe2, 0x34),
            Color::Rgb(0xfc, 0xe9, 0x4f),
            Color::Rgb(0x72, 0x9f, 0xcf),
            Color::Rgb(0xad, 0x7f, 0xa8),
            Color::Rgb(0x34, 0xe2, 0xe2),
            Color::Rgb(0xee, 0xee, 0xec),
        ],
    };

    /// The constants of the user's terminal classics, carried in as *roles*:
    /// the same green-live/blue-focus/red-danger grammar as the web pair, with
    /// each palette's own values. The terminal's sixteen are the palettes the
    /// projects ship for xterm, so a device's own colours stay in character.
    pub const NORD: Theme = Theme {
        bg: Color::Rgb(0x2e, 0x34, 0x40),
        panel: Color::Rgb(0x3b, 0x42, 0x52),
        panel_2: Color::Rgb(0x40, 0x48, 0x59),
        panel_3: Color::Rgb(0x4c, 0x56, 0x6a),
        text: Color::Rgb(0xec, 0xef, 0xf4),
        muted: Color::Rgb(0x8b, 0x95, 0xa9),
        line: Color::Rgb(0x43, 0x4c, 0x5e),
        line_strong: Color::Rgb(0x5a, 0x66, 0x7d),
        accent: Color::Rgb(0xa3, 0xbe, 0x8c),
        accent_strong: Color::Rgb(0xbc, 0xd3, 0xa6),
        accent_ink: Color::Rgb(0x2e, 0x34, 0x40),
        focus: Color::Rgb(0x88, 0xc0, 0xd0),
        danger: Color::Rgb(0xbf, 0x61, 0x6a),
        warn: Color::Rgb(0xeb, 0xcb, 0x8b),
        terminal_bg: Color::Rgb(0x24, 0x29, 0x33),
        terminal_fg: Color::Rgb(0xd8, 0xde, 0xe9),
        ansi: [
            Color::Rgb(0x3b, 0x42, 0x52),
            Color::Rgb(0xbf, 0x61, 0x6a),
            Color::Rgb(0xa3, 0xbe, 0x8c),
            Color::Rgb(0xeb, 0xcb, 0x8b),
            Color::Rgb(0x81, 0xa1, 0xc1),
            Color::Rgb(0xb4, 0x8e, 0xad),
            Color::Rgb(0x88, 0xc0, 0xd0),
            Color::Rgb(0xe5, 0xe9, 0xf0),
            Color::Rgb(0x4c, 0x56, 0x6a),
            Color::Rgb(0xbf, 0x61, 0x6a),
            Color::Rgb(0xa3, 0xbe, 0x8c),
            Color::Rgb(0xeb, 0xcb, 0x8b),
            Color::Rgb(0x81, 0xa1, 0xc1),
            Color::Rgb(0xb4, 0x8e, 0xad),
            Color::Rgb(0x8f, 0xbc, 0xbb),
            Color::Rgb(0xec, 0xef, 0xf4),
        ],
    };

    /// Warm and low-contrast, the way the Gruvbox project keeps it: the lime
    /// green is the live colour and the aqua the focus, over four browns.
    pub const GRUVBOX: Theme = Theme {
        bg: Color::Rgb(0x28, 0x28, 0x28),
        panel: Color::Rgb(0x32, 0x30, 0x2f),
        panel_2: Color::Rgb(0x3c, 0x38, 0x36),
        panel_3: Color::Rgb(0x50, 0x49, 0x45),
        text: Color::Rgb(0xeb, 0xdb, 0xb2),
        muted: Color::Rgb(0xa8, 0x99, 0x84),
        line: Color::Rgb(0x3c, 0x38, 0x36),
        line_strong: Color::Rgb(0x50, 0x49, 0x45),
        accent: Color::Rgb(0xb8, 0xbb, 0x26),
        accent_strong: Color::Rgb(0xd0, 0xd3, 0x4a),
        accent_ink: Color::Rgb(0x28, 0x28, 0x28),
        focus: Color::Rgb(0x83, 0xa5, 0x98),
        danger: Color::Rgb(0xfb, 0x49, 0x34),
        warn: Color::Rgb(0xfa, 0xbd, 0x2f),
        terminal_bg: Color::Rgb(0x1d, 0x20, 0x21),
        terminal_fg: Color::Rgb(0xeb, 0xdb, 0xb2),
        ansi: [
            Color::Rgb(0x28, 0x28, 0x28),
            Color::Rgb(0xcc, 0x24, 0x1d),
            Color::Rgb(0x98, 0x97, 0x1a),
            Color::Rgb(0xd7, 0x99, 0x21),
            Color::Rgb(0x45, 0x85, 0x88),
            Color::Rgb(0xb1, 0x62, 0x86),
            Color::Rgb(0x68, 0x9d, 0x6a),
            Color::Rgb(0xa8, 0x99, 0x84),
            Color::Rgb(0x92, 0x83, 0x74),
            Color::Rgb(0xfb, 0x49, 0x34),
            Color::Rgb(0xb8, 0xbb, 0x26),
            Color::Rgb(0xfa, 0xbd, 0x2f),
            Color::Rgb(0x83, 0xa5, 0x98),
            Color::Rgb(0xd3, 0x86, 0x9b),
            Color::Rgb(0x8e, 0xc0, 0x7c),
            Color::Rgb(0xeb, 0xdb, 0xb2),
        ],
    };

    /// The Solarized dark set: one neutral ramp, one accent per meaning. The
    /// terminal's sixteen are exactly Solarized's own.
    pub const SOLARIZED: Theme = Theme {
        bg: Color::Rgb(0x00, 0x2b, 0x36),
        panel: Color::Rgb(0x07, 0x36, 0x42),
        panel_2: Color::Rgb(0x08, 0x40, 0x4f),
        panel_3: Color::Rgb(0x0e, 0x4c, 0x5c),
        text: Color::Rgb(0x93, 0xa1, 0xa1),
        muted: Color::Rgb(0x58, 0x6e, 0x75),
        line: Color::Rgb(0x0d, 0x46, 0x54),
        line_strong: Color::Rgb(0x1c, 0x5b, 0x68),
        accent: Color::Rgb(0x2a, 0xa1, 0x98),
        accent_strong: Color::Rgb(0x4c, 0xc4, 0xb8),
        accent_ink: Color::Rgb(0x00, 0x2b, 0x36),
        focus: Color::Rgb(0x26, 0x8b, 0xd2),
        danger: Color::Rgb(0xdc, 0x32, 0x2f),
        warn: Color::Rgb(0xb5, 0x89, 0x00),
        terminal_bg: Color::Rgb(0x00, 0x1f, 0x28),
        terminal_fg: Color::Rgb(0x93, 0xa1, 0xa1),
        ansi: [
            Color::Rgb(0x07, 0x36, 0x42),
            Color::Rgb(0xdc, 0x32, 0x2f),
            Color::Rgb(0x85, 0x99, 0x00),
            Color::Rgb(0xb5, 0x89, 0x00),
            Color::Rgb(0x26, 0x8b, 0xd2),
            Color::Rgb(0xd3, 0x36, 0x82),
            Color::Rgb(0x2a, 0xa1, 0x98),
            Color::Rgb(0xee, 0xe8, 0xd5),
            Color::Rgb(0x00, 0x2b, 0x36),
            Color::Rgb(0xcb, 0x4b, 0x16),
            Color::Rgb(0x58, 0x6e, 0x75),
            Color::Rgb(0x65, 0x7b, 0x83),
            Color::Rgb(0x83, 0x94, 0x96),
            Color::Rgb(0x6c, 0x71, 0xc4),
            Color::Rgb(0x93, 0xa1, 0xa1),
            Color::Rgb(0xfd, 0xf6, 0xe3),
        ],
    };

    /// Dracula: a violet night with the project's own neon streaks, its
    /// sixteen as the project ships them.
    pub const DRACULA: Theme = Theme {
        bg: Color::Rgb(0x28, 0x2a, 0x36),
        panel: Color::Rgb(0x34, 0x37, 0x46),
        panel_2: Color::Rgb(0x3c, 0x3f, 0x52),
        panel_3: Color::Rgb(0x44, 0x47, 0x5a),
        text: Color::Rgb(0xf8, 0xf8, 0xf2),
        muted: Color::Rgb(0x62, 0x72, 0xa4),
        line: Color::Rgb(0x3d, 0x41, 0x55),
        line_strong: Color::Rgb(0x52, 0x5a, 0x7a),
        accent: Color::Rgb(0x50, 0xfa, 0x7b),
        accent_strong: Color::Rgb(0x7c, 0xff, 0xa0),
        accent_ink: Color::Rgb(0x10, 0x32, 0x1c),
        focus: Color::Rgb(0x8b, 0xe9, 0xfd),
        danger: Color::Rgb(0xff, 0x55, 0x55),
        warn: Color::Rgb(0xff, 0xb8, 0x6c),
        terminal_bg: Color::Rgb(0x21, 0x22, 0x2c),
        terminal_fg: Color::Rgb(0xf8, 0xf8, 0xf2),
        ansi: [
            Color::Rgb(0x21, 0x22, 0x2c),
            Color::Rgb(0xff, 0x55, 0x55),
            Color::Rgb(0x50, 0xfa, 0x7b),
            Color::Rgb(0xf1, 0xfa, 0x8c),
            Color::Rgb(0xbd, 0x93, 0xf9),
            Color::Rgb(0xff, 0x79, 0xc6),
            Color::Rgb(0x8b, 0xe9, 0xfd),
            Color::Rgb(0xf8, 0xf8, 0xf2),
            Color::Rgb(0x62, 0x72, 0xa4),
            Color::Rgb(0xff, 0x6e, 0x6e),
            Color::Rgb(0x69, 0xff, 0x94),
            Color::Rgb(0xff, 0xff, 0xa5),
            Color::Rgb(0xd6, 0xac, 0xff),
            Color::Rgb(0xff, 0x92, 0xdf),
            Color::Rgb(0xa4, 0xff, 0xff),
            Color::Rgb(0xff, 0xff, 0xff),
        ],
    };

    /// Resolve a 256-colour palette index.
    ///
    /// The layout is the one the palette has had since xterm and every terminal
    /// shares: sixteen named colours, then a 6×6×6 cube, then a 24-step grey
    /// ramp. Keeping the arithmetic here means the serial pane only has to know
    /// *that* the device named a colour, not how the name becomes a value.
    pub fn indexed(&self, index: u8) -> Color {
        match index {
            0..=15 => self.ansi[index as usize],
            16..=231 => {
                // 16 + 36r + 6g + b, over the cube's six levels.
                const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
                let i = index as usize - 16;
                Color::Rgb(LEVELS[(i / 36) % 6], LEVELS[(i / 6) % 6], LEVELS[i % 6])
            }
            // 232..=255: a 24-step grey ramp from 8 to 238.
            _ => {
                let level = 8 + (index as u16 - 232) * 10;
                let value = level as u8;
                Color::Rgb(value, value, value)
            }
        }
    }

    /// The theme behind [`ThemeChoice`], the enum settings persist.
    pub fn for_choice(choice: ThemeChoice) -> Theme {
        match choice {
            ThemeChoice::Dark => Theme::DARK,
            ThemeChoice::Light => Theme::LIGHT,
            ThemeChoice::Nord => Theme::NORD,
            ThemeChoice::Gruvbox => Theme::GRUVBOX,
            ThemeChoice::Dracula => Theme::DRACULA,
            ThemeChoice::Solarized => Theme::SOLARIZED,
        }
    }

    // --- base styles --------------------------------------------------------

    /// Body text on any surface.
    pub fn text(&self) -> Style {
        Style::default().fg(self.text)
    }

    /// Secondary text: hints, counters, anything that must not compete.
    pub fn muted(&self) -> Style {
        Style::default().fg(self.muted)
    }

    /// The live colour, for what the user can act on right now.
    pub fn accent(&self) -> Style {
        Style::default().fg(self.accent)
    }

    /// Bright accent, for emphasis inside accent-coloured text.
    pub fn accent_strong(&self) -> Style {
        Style::default().fg(self.accent_strong)
    }

    /// Focus colour: labels, prompts, the caret.
    pub fn focus(&self) -> Style {
        Style::default().fg(self.focus)
    }

    /// Errors and destructive actions.
    pub fn danger(&self) -> Style {
        Style::default().fg(self.danger)
    }

    /// Warnings.
    pub fn warn(&self) -> Style {
        Style::default().fg(self.warn)
    }

    /// Resting borders and separators.
    pub fn line(&self) -> Style {
        Style::default().fg(self.line)
    }

    /// The scrollbar's thumb: a touch stronger than the resting hairlines,
    /// because the bar has to be findable on a ground that is not a border.
    pub fn scrollbar(&self) -> Style {
        Style::default().fg(self.line_strong)
    }

    /// A panel surface (`--panel`), the ground bars and the sidebar sit on.
    pub fn panel(&self) -> Style {
        Style::default().bg(self.panel)
    }

    /// The serial terminal's ground (`--terminal`), darker than every panel so
    /// the pane reads as a hole the device writes into.
    pub fn terminal(&self) -> Style {
        Style::default().bg(self.terminal_bg)
    }

    /// The selected row of a list: accent fill with the ink the web puts on
    /// primary buttons. Used for the sidebar selection, list cursors and the
    /// command palette's highlighted action.
    pub fn selected(&self) -> Style {
        Style::default()
            .bg(self.accent)
            .fg(self.accent_ink)
            .add_modifier(Modifier::BOLD)
    }

    /// A mouse selection over painted text: the same accent ground as the
    /// sidebar's selected row, so the two highlights read as one idea. The
    /// pane `patch`es this over the device's own colours rather than replacing
    /// their style, which is what keeps bold and underline in the text the
    /// selection covers.
    pub fn selection(&self) -> Style {
        Style::default().bg(self.accent).fg(self.accent_ink)
    }

    /// A sidebar / panel section header: the web's uppercase drawer heads.
    pub fn section_header(&self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    /// Notice colouring, shared by toasts, the notice log and inline notices.
    pub fn level_style(&self, level: NoticeLevel) -> Style {
        match level {
            NoticeLevel::Info => self.focus(),
            NoticeLevel::Warn => self.warn(),
            NoticeLevel::Error => self.danger(),
        }
    }

    /// The connection dot, in the web's status-dot colours: green while
    /// connected, amber while dialling, red on failure, grey at rest.
    pub fn state_dot(&self, state: ConnectionState) -> Style {
        match state {
            ConnectionState::Connected => self.accent_strong(),
            ConnectionState::Connecting => self.warn(),
            ConnectionState::Failed => self.danger(),
            ConnectionState::Disconnected => self.muted(),
        }
    }

    // --- chrome -------------------------------------------------------------

    /// A framed view in the centre column: panel surface, resting border, the
    /// view's name as an accent title. The serial terminal view does not take a
    /// frame — it is the pane the device writes into.
    pub fn view_block(&self, title: impl Into<String>) -> Block<'static> {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(self.line())
            .style(self.panel())
            .title(Span::styled(
                format!(" {} ", title.into()),
                self.section_header(),
            ))
    }

    /// A window that took the keyboard (command palette, dialogs): the stronger
    /// border the web gives a focused surface, panel ground, accent title.
    pub fn overlay_block(&self, title: impl Into<String>) -> Block<'static> {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(self.line_strong))
            .style(self.panel())
            .title(Span::styled(
                format!(" {} ", title.into()),
                self.section_header(),
            ))
    }

    /// A toast's border: the web outlines `.toast` with the accent and
    /// `.toast-error` with danger.
    pub fn toast_border(&self, level: NoticeLevel) -> Color {
        match level {
            NoticeLevel::Info => self.accent,
            NoticeLevel::Warn => self.warn,
            NoticeLevel::Error => self.danger,
        }
    }

    /// A toast's text: panel-3 ground, primary text. The notice's colour is
    /// carried by [`Theme::toast_border`], the way the web borders its toasts.
    pub fn toast(&self) -> Style {
        Style::default().bg(self.panel_3).fg(self.text)
    }

    /// The status pill on the centre pane's head row: a `--panel-3` chip with
    /// primary text, so it reads as a surface laid over the pane's own ground
    /// rather than as another line of the pane.
    pub fn pill(&self) -> Style {
        Style::default().bg(self.panel_3).fg(self.text)
    }

    /// The pill's dot: a filled circle in the state's own colour, on the pill's
    /// ground. One style for every state — the colour is what carries the
    /// state, which is how the web client paints its `.status-dot`.
    pub fn pill_dot(&self, state: ConnectionState) -> Style {
        self.state_dot(state).bg(self.panel_3)
    }
}

/// Which theme the user chose, as persisted in `tui.json`. The web remembers
/// the same choice in `localStorage["linkr-theme"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    #[default]
    Dark,
    Light,
    /// Jim Schmitz's Nord.
    Nord,
    /// Pavel Pertsev's Gruvbox.
    Gruvbox,
    /// Ethan Schoonover's Solarized, the dark set.
    Solarized,
    /// Zeno Rocha's Dracula.
    Dracula,
}

/// Every theme the interface ships, in the order a toggle walks them: the
/// web's own pair first, so the first press of Ctrl+T still swaps dark for
/// light the way it always did, then the terminal classics.
pub const THEMES: [ThemeChoice; 6] = [
    ThemeChoice::Dark,
    ThemeChoice::Light,
    ThemeChoice::Nord,
    ThemeChoice::Gruvbox,
    ThemeChoice::Dracula,
    ThemeChoice::Solarized,
];

impl ThemeChoice {
    /// The next one around the ring — what a toggle switches to.
    pub fn toggled(self) -> ThemeChoice {
        let at = THEMES.iter().position(|choice| *choice == self).unwrap_or(0);
        THEMES[(at + 1) % THEMES.len()]
    }

    /// Lowercase name, the spelling used in settings files.
    pub fn as_str(self) -> &'static str {
        match self {
            ThemeChoice::Dark => "dark",
            ThemeChoice::Light => "light",
            ThemeChoice::Nord => "nord",
            ThemeChoice::Gruvbox => "gruvbox",
            ThemeChoice::Solarized => "solarized",
            ThemeChoice::Dracula => "dracula",
        }
    }

    /// The theme a name refers to: the settings-file spelling, case and
    /// padding forgiven. Unknown names give `None`, so a caller can fall back
    /// to its own default rather than to a guess.
    pub fn from_name(raw: &str) -> Option<ThemeChoice> {
        let raw = raw.trim().to_ascii_lowercase();
        THEMES
            .iter()
            .copied()
            .find(|choice| choice.as_str() == raw)
    }

    /// Parse the settings-file spelling. Unknown values fall back to the
    /// default rather than failing a whole settings load.
    pub fn from_str_lossy(raw: &str) -> ThemeChoice {
        ThemeChoice::from_name(raw).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dark palette is the web `:root` block, value for value.
    #[test]
    fn dark_tokens_match_the_web_root() {
        let t = Theme::DARK;
        assert_eq!(t.bg, Color::Rgb(0x09, 0x0d, 0x14));
        assert_eq!(t.panel, Color::Rgb(0x11, 0x16, 0x1f));
        assert_eq!(t.panel_2, Color::Rgb(0x15, 0x1b, 0x26));
        assert_eq!(t.panel_3, Color::Rgb(0x1c, 0x23, 0x30));
        assert_eq!(t.text, Color::Rgb(0xe8, 0xee, 0xf7));
        assert_eq!(t.muted, Color::Rgb(0x8d, 0x9b, 0xb0));
        assert_eq!(t.line, Color::Rgb(0x24, 0x2d, 0x3d));
        assert_eq!(t.line_strong, Color::Rgb(0x33, 0x40, 0x5a));
        assert_eq!(t.accent, Color::Rgb(0x3f, 0xc8, 0x8a));
        assert_eq!(t.accent_strong, Color::Rgb(0x6f, 0xe0, 0xab));
        assert_eq!(t.accent_ink, Color::Rgb(0x05, 0x25, 0x17));
        assert_eq!(t.focus, Color::Rgb(0x7a, 0xa8, 0xf8));
        assert_eq!(t.danger, Color::Rgb(0xf2, 0x77, 0x7c));
        assert_eq!(t.warn, Color::Rgb(0xf2, 0xc9, 0x4c));
        assert_eq!(t.terminal_bg, Color::Rgb(0x05, 0x08, 0x0d));
        assert_eq!(t.terminal_fg, Color::Rgb(0xd8, 0xf3, 0xdc));
    }

    /// The light palette is the web `[data-theme="light"]` block; the terminal
    /// pair is xterm.js `XTERM_LIGHT`.
    #[test]
    fn light_tokens_match_the_web_light_block() {
        let t = Theme::LIGHT;
        assert_eq!(t.bg, Color::Rgb(0xee, 0xf2, 0xf7));
        assert_eq!(t.panel, Color::Rgb(0xff, 0xff, 0xff));
        assert_eq!(t.panel_2, Color::Rgb(0xf6, 0xf8, 0xfb));
        assert_eq!(t.panel_3, Color::Rgb(0xe9, 0xee, 0xf5));
        assert_eq!(t.text, Color::Rgb(0x0f, 0x1c, 0x2e));
        assert_eq!(t.muted, Color::Rgb(0x5c, 0x6a, 0x82));
        assert_eq!(t.line, Color::Rgb(0xdb, 0xe3, 0xee));
        assert_eq!(t.line_strong, Color::Rgb(0xc3, 0xcf, 0xdf));
        assert_eq!(t.accent, Color::Rgb(0x1f, 0x9d, 0x64));
        assert_eq!(t.accent_strong, Color::Rgb(0x15, 0x7a, 0x4c));
        assert_eq!(t.accent_ink, Color::Rgb(0xff, 0xff, 0xff));
        assert_eq!(t.focus, Color::Rgb(0x3b, 0x82, 0xf6));
        assert_eq!(t.danger, Color::Rgb(0xdc, 0x34, 0x34));
        assert_eq!(t.warn, Color::Rgb(0xb4, 0x53, 0x09));
        assert_eq!(t.terminal_bg, Color::Rgb(0xf8, 0xfa, 0xfc));
        assert_eq!(t.terminal_fg, Color::Rgb(0x1f, 0x6b, 0x43));
    }

    /// The two themes never agree on a ground: a light-mode screenshot must not
    /// be able to pass as the dark one by accident.
    #[test]
    fn the_themes_disagree_on_every_ground() {
        for (dark, light) in [
            (Theme::DARK.bg, Theme::LIGHT.bg),
            (Theme::DARK.panel, Theme::LIGHT.panel),
            (Theme::DARK.panel_3, Theme::LIGHT.panel_3),
            (Theme::DARK.text, Theme::LIGHT.text),
            (Theme::DARK.accent, Theme::LIGHT.accent),
            (Theme::DARK.terminal_bg, Theme::LIGHT.terminal_bg),
        ] {
            assert_ne!(dark, light);
        }
    }

    /// Roles resolve to the current theme's values, so a switch is visible to
    /// every helper with no other change.
    #[test]
    fn roles_follow_the_theme_they_are_asked_of() {
        assert_eq!(Theme::DARK.text().fg, Some(Theme::DARK.text));
        assert_eq!(Theme::LIGHT.text().fg, Some(Theme::LIGHT.text));
        assert_eq!(Theme::DARK.selected().bg, Some(Theme::DARK.accent));
        assert_eq!(Theme::DARK.selected().fg, Some(Theme::DARK.accent_ink));
        assert_eq!(Theme::LIGHT.selected().bg, Some(Theme::LIGHT.accent));
        assert_eq!(Theme::LIGHT.selected().fg, Some(Theme::LIGHT.accent_ink));
    }

    /// A selected row is always an accent fill, in both themes: that is the
    /// "primary button" look the sidebar chip copies.
    #[test]
    fn selection_is_an_accent_chip_in_both_themes() {
        for theme in [Theme::DARK, Theme::LIGHT] {
            let style = theme.selected();
            assert_eq!(style.bg, Some(theme.accent));
            assert_eq!(style.fg, Some(theme.accent_ink));
            assert!(style.add_modifier.contains(Modifier::BOLD));
        }
    }

    /// Toasts borrow the notice's colour for their border, never for the text:
    /// the text stays on a panel ground so it is readable in both themes.
    #[test]
    fn toast_borders_carry_the_notice_level() {
        for theme in [Theme::DARK, Theme::LIGHT] {
            assert_eq!(theme.toast_border(NoticeLevel::Info), theme.accent);
            assert_eq!(theme.toast_border(NoticeLevel::Warn), theme.warn);
            assert_eq!(theme.toast_border(NoticeLevel::Error), theme.danger);
            assert_eq!(theme.toast().bg, Some(theme.panel_3));
            assert_eq!(theme.toast().fg, Some(theme.text));
        }
    }

    /// The status dot maps one state to one role, in both themes.
    #[test]
    fn the_status_dot_maps_states_to_roles() {
        for theme in [Theme::DARK, Theme::LIGHT] {
            assert_eq!(
                theme.state_dot(ConnectionState::Connected).fg,
                Some(theme.accent_strong)
            );
            assert_eq!(
                theme.state_dot(ConnectionState::Connecting).fg,
                Some(theme.warn)
            );
            assert_eq!(
                theme.state_dot(ConnectionState::Failed).fg,
                Some(theme.danger)
            );
            assert_eq!(
                theme.state_dot(ConnectionState::Disconnected).fg,
                Some(theme.muted)
            );
        }
    }

    /// The theme choice round-trips through its settings spelling, and an
    /// unknown value degrades to the default instead of failing.
    #[test]
    fn theme_choice_toggles_and_parses() {
        assert_eq!(ThemeChoice::default(), ThemeChoice::Dark);
        assert_eq!(ThemeChoice::Dark.toggled(), ThemeChoice::Light);
        for choice in THEMES {
            assert_eq!(
                ThemeChoice::from_name(choice.as_str()),
                Some(choice),
                "{choice:?} does not parse back"
            );
            assert_eq!(
                ThemeChoice::from_str_lossy(&choice.as_str().to_uppercase()),
                choice
            );
        }
        assert_eq!(ThemeChoice::from_str_lossy(" LIGHT "), ThemeChoice::Light);
        assert_eq!(ThemeChoice::from_str_lossy("nonsense"), ThemeChoice::Dark);
        assert_eq!(ThemeChoice::from_name("nonsense"), None);
    }

    /// A toggle walks the whole ring before it comes home: every theme is
    /// reachable from Ctrl+T, and no press is a no-op.
    #[test]
    fn the_toggle_visits_every_theme() {
        let mut seen = Vec::new();
        let mut choice = ThemeChoice::Dark;
        for _ in 0..THEMES.len() {
            choice = choice.toggled();
            seen.push(choice);
        }
        assert_eq!(choice, ThemeChoice::Dark, "the ring does not close");
        for theme in THEMES {
            assert!(seen.contains(&theme), "{theme:?} was never reached");
        }
    }

    /// Every choice maps to a theme, and the pair stays in step with the
    /// constants the tests above pin.
    #[test]
    fn choices_resolve_to_the_shipped_themes() {
        assert_eq!(Theme::for_choice(ThemeChoice::Dark), Theme::DARK);
        assert_eq!(Theme::for_choice(ThemeChoice::Light), Theme::LIGHT);
        assert_eq!(Theme::for_choice(ThemeChoice::Nord), Theme::NORD);
        assert_eq!(Theme::for_choice(ThemeChoice::Gruvbox), Theme::GRUVBOX);
        assert_eq!(Theme::for_choice(ThemeChoice::Solarized), Theme::SOLARIZED);
        assert_eq!(Theme::for_choice(ThemeChoice::Dracula), Theme::DRACULA);
    }

    /// Each classic palette is the one it says it is: ground and live colour
    /// pinned, so a miscopied hex is caught here and not on the user's screen.
    #[test]
    fn the_classic_themes_are_the_ones_they_say() {
        assert_eq!(Theme::NORD.bg, Color::Rgb(0x2e, 0x34, 0x40));
        assert_eq!(Theme::NORD.accent, Color::Rgb(0xa3, 0xbe, 0x8c));
        assert_eq!(Theme::GRUVBOX.bg, Color::Rgb(0x28, 0x28, 0x28));
        assert_eq!(Theme::GRUVBOX.accent, Color::Rgb(0xb8, 0xbb, 0x26));
        assert_eq!(Theme::SOLARIZED.bg, Color::Rgb(0x00, 0x2b, 0x36));
        assert_eq!(Theme::SOLARIZED.accent, Color::Rgb(0x2a, 0xa1, 0x98));
        assert_eq!(Theme::DRACULA.bg, Color::Rgb(0x28, 0x2a, 0x36));
        assert_eq!(Theme::DRACULA.accent, Color::Rgb(0x50, 0xfa, 0x7b));
    }

    /// No two themes share a ground, and no palette loses a role into its own
    /// ground: a vanished role is a bug no single screenshot catches.
    #[test]
    fn every_theme_stands_apart() {
        for (index, choice) in THEMES.iter().enumerate() {
            let theme = Theme::for_choice(*choice);
            for other in THEMES.iter().skip(index + 1) {
                let other = Theme::for_choice(*other);
                assert_ne!(theme.bg, other.bg, "{choice:?} shares a ground");
            }
            for (role, value) in [
                ("panel", theme.panel),
                ("panel_3", theme.panel_3),
                ("muted", theme.muted),
                ("line", theme.line),
                ("accent", theme.accent),
                ("terminal_bg", theme.terminal_bg),
            ] {
                assert_ne!(value, theme.bg, "{choice:?}: {role} is the ground");
                assert_ne!(value, theme.text, "{choice:?}: {role} is the text");
            }
        }
    }
}
