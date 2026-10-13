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
    };

    /// The theme behind [`ThemeChoice`], the enum settings persist.
    pub fn for_choice(choice: ThemeChoice) -> Theme {
        match choice {
            ThemeChoice::Dark => Theme::DARK,
            ThemeChoice::Light => Theme::LIGHT,
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
}

/// Which theme the user chose, as persisted in `tui.json`. The web remembers
/// the same choice in `localStorage["linkr-theme"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    #[default]
    Dark,
    Light,
}

impl ThemeChoice {
    /// The other one — what a toggle switches to.
    pub fn toggled(self) -> ThemeChoice {
        match self {
            ThemeChoice::Dark => ThemeChoice::Light,
            ThemeChoice::Light => ThemeChoice::Dark,
        }
    }

    /// Lowercase name, the spelling used in settings files.
    pub fn as_str(self) -> &'static str {
        match self {
            ThemeChoice::Dark => "dark",
            ThemeChoice::Light => "light",
        }
    }

    /// Parse the settings-file spelling. Unknown values fall back to the
    /// default rather than failing a whole settings load.
    pub fn from_str_lossy(raw: &str) -> ThemeChoice {
        match raw.trim().to_ascii_lowercase().as_str() {
            "light" => ThemeChoice::Light,
            _ => ThemeChoice::Dark,
        }
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
        assert_eq!(ThemeChoice::Light.toggled(), ThemeChoice::Dark);
        assert_eq!(ThemeChoice::from_str_lossy("light"), ThemeChoice::Light);
        assert_eq!(ThemeChoice::from_str_lossy(" LIGHT "), ThemeChoice::Light);
        assert_eq!(ThemeChoice::from_str_lossy("dark"), ThemeChoice::Dark);
        assert_eq!(ThemeChoice::from_str_lossy("nonsense"), ThemeChoice::Dark);
    }

    /// Every choice maps to a theme, and the pair stays in step with the
    /// constants the tests above pin.
    #[test]
    fn choices_resolve_to_the_shipped_themes() {
        assert_eq!(Theme::for_choice(ThemeChoice::Dark), Theme::DARK);
        assert_eq!(Theme::for_choice(ThemeChoice::Light), Theme::LIGHT);
    }
}
