//! The Linkr Bee host terminal: a CLI and a TUI over one core.
//!
//! `main` does three things and nothing else: read the command line, set up or
//! hand over the terminal, and run the event loop. Everything else lives in the
//! library so it can be tested without a TTY.

use std::io::{self, Stdout};
use std::panic;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use linkr_tui::app::{App, Focus, Transport};
use linkr_tui::event::{ConnectionState, View};
use linkr_tui::i18n::Lang;
use linkr_tui::theme::ThemeChoice;
use linkr_tui::ui;
use linkr_tui::views;

/// How often a frame is repainted with no input: the clock and the busy
/// indicator move on this beat, and it is slow enough to cost nothing.
const TICK: Duration = Duration::from_millis(250);

const ABOUT: &str = "Terminal over BLE Nordic UART Service for Linkr Bee bridge";

fn usage(prog: &str) -> String {
    format!(
        "{ABOUT}\n\
         \n\
         Usage: {prog} [OPTIONS]\n\
         \n\
         Options:\n\
         \x20     --lang <LANG>\n\
         \x20         interface language: zh or en [default: zh]\n\
         \x20     --theme <THEME>\n\
         \x20         colour theme: dark or light [default: dark]\n\
         \x20     --transport <LINK>\n\
         \x20         pin the transport and report it as locked: ble or lan\n\
         \x20     --print-frame [<COLSxROWS>]\n\
         \x20         render one frame as text and exit (no terminal needed)\n\
         \x20     --ansi\n\
         \x20         with --print-frame, emit true-colour escapes\n\
         \x20     --state <STATE>\n\
         \x20         preview a connection state: disconnected, connecting,\n\
         \x20         connected or failed [default: connecting]\n\
         \x20     --view <VIEW>\n\
         \x20         open on a view: terminal, diagnostics, network, assistant\n\
         \x20         or transfer [default: terminal]\n\
         \x20     --sample\n\
         \x20         fill the frame with the documented sample reply, so a\n\
         \x20         --print-frame shows a populated view instead of an empty one\n\
         \x20 -h, --help\n\
         \x20         Print help\n\
         \x20 -V, --version\n\
         \x20         Print version\n"
    )
}

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Args {
    lang: Lang,
    theme: ThemeChoice,
    /// The previewed connection state, when the command line named one. Left
    /// unset so the default can depend on what else was asked for.
    state: Option<ConnectionState>,
    /// The view to open on, when the command line named one.
    view: Option<View>,
    print_frame: Option<(u16, u16)>,
    /// Dump the frame with true-colour escapes instead of plain text.
    ansi: bool,
    /// Load the documented sample reply, so an inspection shows content.
    sample: bool,
    /// A transport pinned on the command line: the sidebar then says `locked`,
    /// because the keyboard cannot switch what the flag decided.
    transport: Option<Transport>,
}

/// A command-line error, printed in the shape the rest of the toolchain uses.
fn fail(prog: &str, message: &str) -> ! {
    eprintln!("error: {message}\n");
    eprintln!("Usage: {prog} [OPTIONS]\n");
    eprintln!("For more information, try '--help'.");
    std::process::exit(2);
}

fn parse_args(prog: &str, argv: &[String]) -> Args {
    let mut args = Args {
        lang: Lang::default(),
        theme: ThemeChoice::default(),
        state: None,
        view: None,
        print_frame: None,
        ansi: false,
        sample: false,
        transport: None,
    };
    let mut i = 0;
    while i < argv.len() {
        let flag = argv[i].as_str();
        let mut value = |name: &str| -> String {
            i += 1;
            match argv.get(i) {
                Some(value) => value.clone(),
                None => fail(
                    prog,
                    &format!("a value is required for '{name}' but none was supplied"),
                ),
            }
        };
        match flag {
            "-h" | "--help" => {
                print!("{}", usage(prog));
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("linkr {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--lang" => {
                let raw = value("--lang");
                args.lang = match raw.as_str() {
                    "zh" => Lang::Zh,
                    "en" => Lang::En,
                    other => fail(
                        prog,
                        &format!("invalid value '{other}' for '--lang <LANG>'"),
                    ),
                };
            }
            "--theme" => {
                let raw = value("--theme");
                args.theme = match raw.as_str() {
                    "dark" => ThemeChoice::Dark,
                    "light" => ThemeChoice::Light,
                    other => fail(
                        prog,
                        &format!("invalid value '{other}' for '--theme <THEME>'"),
                    ),
                };
            }
            "--state" => {
                let raw = value("--state");
                args.state = Some(match raw.as_str() {
                    "disconnected" => ConnectionState::Disconnected,
                    "connecting" => ConnectionState::Connecting,
                    "connected" => ConnectionState::Connected,
                    "failed" => ConnectionState::Failed,
                    other => fail(
                        prog,
                        &format!("invalid value '{other}' for '--state <STATE>'"),
                    ),
                });
            }
            "--view" => {
                let raw = value("--view");
                args.view = Some(View::from_name(&raw).unwrap_or_else(|| {
                    fail(prog, &format!("invalid value '{raw}' for '--view <VIEW>'"))
                }));
            }
            "--print-frame" => {
                // The size is optional: `--print-frame 100x30` or bare.
                let mut size = (100u16, 30u16);
                if let Some(next) = argv.get(i + 1) {
                    if !next.starts_with('-') {
                        size = parse_size(prog, next);
                        i += 1;
                    }
                }
                args.print_frame = Some(size);
            }
            "--ansi" => args.ansi = true,
            "--sample" => args.sample = true,
            "--transport" => {
                let raw = value("--transport");
                args.transport = match raw.as_str() {
                    "ble" => Some(Transport::Ble),
                    "lan" => Some(Transport::Lan),
                    other => fail(
                        prog,
                        &format!("invalid value '{other}' for '--transport <LINK>'"),
                    ),
                };
            }
            other => fail(prog, &format!("unrecognized option '{other}'")),
        }
        i += 1;
    }
    args
}

fn parse_size(prog: &str, raw: &str) -> (u16, u16) {
    let (cols, rows) = raw
        .split_once('x')
        .unwrap_or_else(|| fail(prog, &format!("invalid value '{raw}' for '[<COLSxROWS>]'")));
    let parse = |part: &str| -> u16 {
        part.parse::<u16>()
            .ok()
            .filter(|size| *size > 0)
            .unwrap_or_else(|| fail(prog, &format!("invalid value '{raw}' for '[<COLSxROWS>]'")))
    };
    (parse(cols), parse(rows))
}

/// Restores the terminal however the program leaves: on a normal return, on an
/// error, and on a panic. Leaving a terminal in raw mode is the bug that makes
/// a TUI unshippable, so it is handled in one place rather than at each exit.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn enter() -> io::Result<TerminalGuard> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(err) = execute!(stdout, EnterAlternateScreen) {
            // Raw mode is already on; undo it before giving up.
            let _ = disable_raw_mode();
            return Err(err);
        }
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(TerminalGuard { terminal })
    }

    fn restore(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

fn main() -> io::Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let prog = std::env::args()
        .next()
        .and_then(|path| {
            std::path::Path::new(&path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "linkr".to_string());

    let args = parse_args(&prog, &argv);

    // Without `--state`, the preview is a link being dialled — except under
    // `--sample`, where the sample reply means the link answered.
    let state = args.state.unwrap_or(if args.sample {
        ConnectionState::Connected
    } else {
        ConnectionState::Connecting
    });

    let mut app = App {
        lang: args.lang,
        theme: args.theme,
        state,
        transport_locked: args.transport.is_some(),
        ..App::default()
    };
    if let Some(transport) = args.transport {
        app.transport = transport;
    }
    if let Some(view) = args.view {
        app.view = view;
    }
    app.clock = local_clock();
    if args.sample {
        app.diagnostics.apply(&views::sample_reply(), local_clock());
    }

    if let Some((cols, rows)) = args.print_frame {
        if args.ansi {
            print!("{}", ui::render_to_ansi(&app, cols, rows));
        } else {
            print!("{}", ui::render_to_text(&app, cols, rows));
        }
        return Ok(());
    }

    run(&mut app)
}

/// The event loop: paint, then wait for input or the next tick.
fn run(app: &mut App) -> io::Result<()> {
    let mut guard = TerminalGuard::enter()?;

    // A panic must not leave the terminal in raw mode; the hook runs before the
    // process aborts, so the restore below is the last thing that happens.
    let fallback = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen);
        fallback(info);
    }));

    let outcome = event_loop(&mut guard.terminal, app);
    guard.restore();
    outcome
}

fn event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> io::Result<()> {
    loop {
        app.clock = local_clock();
        terminal.draw(|frame| ui::draw(frame, app))?;

        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if handle_key(app, key) {
                        return Ok(());
                    }
                }
                // A resize is picked up by the next draw; nothing to do.
                Event::Resize(_, _) => {}
                _ => {}
            }
        } else {
            // The beat that moves the busy indicator and the clock.
            app.tick = app.tick.wrapping_add(1);
        }
    }
}

/// Handle one key press. Returns true when the loop should stop.
fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match (key.code, ctrl) {
        (KeyCode::Char('q'), true) | (KeyCode::Char('c'), true) => return true,
        (KeyCode::Char('t'), true) => {
            let notice = app.toggle_theme();
            app.push_notice(notice);
        }
        (KeyCode::Tab, _) => {
            app.focus = match app.focus {
                Focus::Terminal => Focus::Sidebar,
                Focus::Sidebar => Focus::Terminal,
            };
        }
        (KeyCode::F(number), _) => {
            if let Some(view) = View::from_fkey(number) {
                app.set_view(view);
            }
        }
        (KeyCode::Down, _) if app.focus == Focus::Sidebar => app.cursor_down(),
        (KeyCode::Up, _) if app.focus == Focus::Sidebar => app.cursor_up(),
        _ => {}
    }
    false
}

/// The wall clock in `HH:MM:SS`, local time.
fn local_clock() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Args {
        let owned: Vec<String> = list.iter().map(|s| s.to_string()).collect();
        parse_args("linkr", &owned)
    }

    /// The defaults are the designed first frame: Chinese, dark, connecting.
    #[test]
    fn defaults_are_the_designed_first_frame() {
        let parsed = args(&[]);
        assert_eq!(parsed.lang, Lang::Zh);
        assert_eq!(parsed.theme, ThemeChoice::Dark);
        assert_eq!(parsed.state, None);
        assert_eq!(parsed.print_frame, None);
        assert!(!parsed.sample);
    }

    /// Flags parse to the state they name.
    #[test]
    fn flags_parse_to_their_state() {
        let parsed = args(&["--lang", "en", "--theme", "light", "--state", "connected"]);
        assert_eq!(parsed.lang, Lang::En);
        assert_eq!(parsed.theme, ThemeChoice::Light);
        assert_eq!(parsed.state, Some(ConnectionState::Connected));
        for (raw, expected) in [
            ("disconnected", ConnectionState::Disconnected),
            ("connecting", ConnectionState::Connecting),
            ("connected", ConnectionState::Connected),
            ("failed", ConnectionState::Failed),
        ] {
            assert_eq!(args(&["--state", raw]).state, Some(expected), "{raw}");
        }
    }

    /// `--view` opens on the view it names, and a name that is not a view is
    /// refused rather than silently ignored.
    #[test]
    fn the_view_flag_selects_a_view_by_name() {
        assert_eq!(args(&[]).view, None);
        for view in View::ALL {
            assert_eq!(args(&["--view", view.as_str()]).view, Some(view));
        }
        assert_eq!(args(&["--view", "network"]).view, Some(View::Network));
    }

    /// `--sample` fills the frame, and it implies an answered link unless the
    /// command line named a state of its own.
    #[test]
    fn sample_fills_the_frame_and_implies_a_connected_link() {
        let mut app = App::default();
        let parsed = args(&["--sample", "--print-frame"]);
        assert!(parsed.sample);
        assert_eq!(parsed.state, None, "no state was named");

        // The resolution `main` performs, without a terminal.
        app.diagnostics
            .apply(&views::sample_reply(), "19:08:06".into());
        assert!(app.diagnostics.has_data());
        assert!(app.diagnostics.done);
        assert_eq!(app.diagnostics.updated.as_deref(), Some("19:08:06"));

        // An explicit state still wins.
        assert_eq!(
            args(&["--sample", "--state", "failed"]).state,
            Some(ConnectionState::Failed)
        );
    }

    /// The frame size is optional and parses both ways.
    #[test]
    fn the_frame_size_is_optional() {
        assert_eq!(args(&["--print-frame"]).print_frame, Some((100, 30)));
        assert_eq!(
            args(&["--print-frame", "80x24"]).print_frame,
            Some((80, 24))
        );
        assert_eq!(args(&["--print-frame", "1x1"]).print_frame, Some((1, 1)));
        // A flag straight after --print-frame is not swallowed as a size.
        let parsed = args(&["--print-frame", "--theme", "light"]);
        assert_eq!(parsed.print_frame, Some((100, 30)));
        assert_eq!(parsed.theme, ThemeChoice::Light);
    }

    /// The clock is always `HH:MM:SS`: eight characters, two colons.
    #[test]
    fn the_clock_is_well_formed() {
        let clock = local_clock();
        assert_eq!(clock.len(), 8, "{clock:?}");
        assert_eq!(clock.matches(':').count(), 2, "{clock:?}");
        for part in clock.split(':') {
            assert_eq!(part.len(), 2);
            assert!(part.chars().all(|c| c.is_ascii_digit()));
        }
    }

    /// F2–F6 select the views; any other function key changes nothing.
    #[test]
    fn the_view_keys_select_views() {
        let mut app = App::default();
        assert!(!handle_key(
            &mut app,
            KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE)
        ));
        assert_eq!(app.view, View::Network);
        handle_key(&mut app, KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE));
        assert_eq!(app.view, View::Transfer);
        handle_key(&mut app, KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE));
        assert_eq!(
            app.view,
            View::Transfer,
            "an unbound F-key must not move the view"
        );
        handle_key(&mut app, KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
        assert_eq!(app.view, View::Transfer, "F1 is help, not a view");
    }

    /// Ctrl+Q and Ctrl+C stop the loop; a bare 'q' does not, so the terminal
    /// stays open while the user is typing to the device.
    #[test]
    fn only_the_control_keys_quit() {
        let mut app = App::default();
        assert!(!handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)
        ));
        assert!(handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)
        ));
        assert!(handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
        ));
    }

    /// Ctrl+T toggles the theme and says so, once.
    #[test]
    fn control_t_toggles_the_theme_and_announces_it() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.theme, ThemeChoice::Light);
        assert_eq!(app.notices.len(), 1);
        assert_eq!(app.notices[0].text, "Theme: light");
    }

    /// Tab moves the focus between the sidebar and the terminal, and the
    /// sidebar cursor never walks past the last selectable row.
    #[test]
    fn tab_moves_the_focus_and_the_cursor_stops_at_the_end() {
        let mut app = App::default();
        assert_eq!(app.focus, Focus::Terminal);
        handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.focus, Focus::Sidebar);
        for _ in 0..100 {
            handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        let last = linkr_tui::ui::sidebar::targets(&app).len() - 1;
        assert_eq!(app.sidebar_cursor, last, "the cursor ran past the last row");
        for _ in 0..100 {
            handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        }
        assert_eq!(app.sidebar_cursor, 0);
    }
}
