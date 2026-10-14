//! The Linkr Bee host terminal: a CLI and a TUI over one core.
//!
//! `main` does three things and nothing else: read the command line, set up or
//! hand over the terminal, and run the event loop. Everything else lives in the
//! library so it can be tested without a TTY.

use std::io::{self, Stdout};
use std::panic;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use linkr_tui::app::{App, Focus, Transport};
use linkr_tui::cli;
use linkr_tui::device;
use linkr_tui::event::{ConnectionState, View};
use linkr_tui::i18n::Lang;
use linkr_tui::lan_token_store::{self, TokenStore};
use linkr_tui::serial::keys::{self, EnterMode, KeyModifiers as TerminalModifiers};
use linkr_tui::session::Dial;
use linkr_tui::theme::ThemeChoice;
use linkr_tui::transport::ble::Selection;
use linkr_tui::ui;
use linkr_tui::views;
use linkr_tui::{dialogs, palette};

/// How often a frame is repainted with no input: the clock and the busy
/// indicator move on this beat, and it is slow enough to cost nothing.
const TICK: Duration = Duration::from_millis(250);

const ABOUT: &str = "Terminal over BLE Nordic UART Service for Linkr Bee bridge";

/// The help text, generated from the one table `cli` keeps.
///
/// It used to be written out here by hand, which is how a flag comes to exist
/// in the parser but not in the help — the last flag added is the one nobody
/// can find out about.
fn usage(prog: &str) -> String {
    cli::usage(prog, ABOUT)
}

/// A modal to open on, so `--print-frame` can show one without a TTY.
///
/// The same reason `--view` exists: a surface that can only be looked at by
/// running the TUI cannot be reviewed, diffed or put in a test — and the modal
/// layer is exactly the kind of thing that looks right until it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverlayPreview {
    Palette,
    Help,
    Notices,
    Confirm,
    /// The prompt a send starts from, so the text field and its caret can be
    /// looked at without a device — which is the only way to look at a dialog
    /// that is normally raised to *do* something.
    SendFile,
    /// The prompt a receive starts from.
    Receive,
}

impl OverlayPreview {
    /// The modal a name refers to. An unknown name gives `None`, so a caller
    /// can report the bad value instead of quietly showing nothing.
    fn from_name(name: &str) -> Option<OverlayPreview> {
        match name.trim().to_ascii_lowercase().as_str() {
            "palette" => Some(OverlayPreview::Palette),
            "help" => Some(OverlayPreview::Help),
            "notices" => Some(OverlayPreview::Notices),
            "confirm" => Some(OverlayPreview::Confirm),
            "send" => Some(OverlayPreview::SendFile),
            "receive" => Some(OverlayPreview::Receive),
            _ => None,
        }
    }
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
    /// The process bridge to dial, as `--bridge` names it.
    bridge: Option<String>,
    /// The advertised name of the board to look for, as `--ble-name` gives it.
    ble_name: Option<String>,
    /// The address of the board to dial, as `--ble-address` gives it. This
    /// wins over a name when both are given: a name finds a board *like* the
    /// one meant, and an address finds it.
    ble_address: Option<String>,
    /// The LAN bridge to dial, as `--lan-host` names it.
    lan_host: Option<String>,
    /// The token that bridge is to be given, as `--lan-token` names it.
    lan_token: Option<String>,
    /// A file that token is read from instead, for a token that would
    /// otherwise be visible to anyone who can read another user's processes.
    lan_token_file: Option<PathBuf>,
    /// The modal to open on, for inspection.
    overlay: Option<OverlayPreview>,
    /// Text handed to the terminal pane before the frame is drawn, for
    /// inspection: the emulator is the one view whose content comes from the
    /// wire, so without this it cannot be looked at without a device.
    feed: Option<String>,
    /// List the nearby boards and stop.
    scan: bool,
    /// How long `--scan` listens, in seconds.
    scan_timeout: Duration,
    /// Connect, do what was asked, hang up — no terminal is taken over.
    no_terminal: bool,
    /// Report what happened as JSON lines rather than as sentences.
    json: bool,
    /// Say nothing on stderr that is not an error.
    quiet: bool,
    /// What the bridge was asked to do or report, when it was asked anything.
    requests: cli::Requests,
    /// A file the WiFi password is read from, so it never reaches `argv` —
    /// where every other process on the machine can see it.
    wifi_key_file: Option<PathBuf>,
    /// How the Enter key is sent on the wire.
    enter: EnterMode,
    /// Echo what is typed into the pane, for a link that does not echo.
    local_echo: bool,
    /// Hold a typed line until Enter arrives, rather than sending it a key at
    /// a time.
    line_mode: bool,
    /// The byte that ends the session when it is typed: the wire's own "get me
    /// out of here", which has to exist because a byte stream has no other way
    /// to say it.
    escape: u8,
    /// Where every byte the device sends is appended.
    log_file: Option<PathBuf>,
    /// Whether both directions are traced to standard error.
    debug_io: bool,
    /// `--loopback-test`: send these bytes and require the same back.
    loopback: Option<String>,
    /// How long that echo is waited for.
    loopback_timeout: Duration,
}

impl Args {
    /// The link the command line asked to dial, if any, or the reason asking
    /// for it fails.
    ///
    /// Only a LAN dial can fail here: the token it was offered comes from one
    /// of four places and any of them may be unreadable or mis-shaped, which
    /// is worth saying before a socket is opened rather than after.
    ///
    /// `--bridge` wins over everything: a bridge is how a radio is reached
    /// today, and a command line that names two links is better read as "the
    /// bridge is the way in" than as an error.
    fn dial(&self) -> Result<Option<Dial>, String> {
        if let Some(command) = &self.bridge {
            return Ok(Some(Dial::Stdio(command.clone())));
        }
        if self.lan_host.is_some() || self.transport == Some(Transport::Lan) {
            return self.lan().map(Some);
        }
        Ok(self.ble())
    }

    /// The LAN dial, and the token it carries. The four places a token may
    /// come from are tried in [`lan_token_store::resolve`]'s order — the flag,
    /// the file, the environment, the store — so that what was typed beats
    /// what was remembered, and a bridge that never asked for a token gets
    /// none rather than a stale one from last year's board.
    fn lan(&self) -> Result<Dial, String> {
        let host = self.lan_host.clone().unwrap_or_default();
        let store = TokenStore::load();
        let token = lan_token_store::resolve(
            self.lan_token.as_deref(),
            self.lan_token_file.as_deref(),
            std::env::var(lan_token_store::TOKEN_ENV).ok().as_deref(),
            &store,
            &host,
        )?
        .map(|(token, _)| token);
        Ok(Dial::Lan { host, token })
    }

    /// The Bluetooth dial, when the command line asked for one.
    ///
    /// Reached in three ways, because a bench has three situations: the board
    /// is named (several are on the air and only one is wanted), it is
    /// addressed (a script dialling one specific MAC), or it is simply the
    /// only one advertising a serial service at all.
    fn ble(&self) -> Option<Dial> {
        let named = self.ble_name.is_some() || self.ble_address.is_some();
        if !named && self.transport != Some(Transport::Ble) {
            return None;
        }
        Some(Dial::Ble(if let Some(address) = &self.ble_address {
            Selection::Address(address.clone())
        } else if let Some(name) = &self.ble_name {
            Selection::Name(name.clone())
        } else {
            Selection::Any
        }))
    }
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
        bridge: None,
        ble_name: None,
        ble_address: None,
        lan_host: None,
        lan_token: None,
        lan_token_file: None,
        overlay: None,
        feed: None,
        scan: false,
        scan_timeout: Duration::from_secs(10),
        no_terminal: false,
        json: bool::default(),
        quiet: false,
        requests: cli::Requests::default(),
        wifi_key_file: None,
        enter: EnterMode::default(),
        local_echo: false,
        line_mode: false,
        escape: cli::parse_escape("^]").expect("the documented default parses"),
        log_file: None,
        debug_io: false,
        loopback: None,
        loopback_timeout: Duration::from_secs(3),
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
            "--print-completion" => {
                let raw = value("--print-completion");
                let script = match cli::completion_script(&raw, prog) {
                    Some(script) => script,
                    None => fail(
                        prog,
                        &format!("invalid value '{raw}' for '--print-completion <SHELL>'"),
                    ),
                };
                print!("{script}");
                std::process::exit(0);
            }
            "--scan" => args.scan = true,
            "--timeout" => {
                let raw = value("--timeout");
                let seconds: f64 = raw.parse().ok().filter(|s| *s > 0.0).unwrap_or_else(|| {
                    fail(
                        prog,
                        &format!("invalid value '{raw}' for '--timeout <SECONDS>'"),
                    )
                });
                args.scan_timeout = Duration::from_secs_f64(seconds);
            }
            "--no-terminal" => args.no_terminal = true,
            "--json" => args.json = true,
            "--quiet" => args.quiet = true,
            "--query-info" => args.requests.queries.push(device::Query::Diagnostics),
            "--query-uart" => args.requests.queries.push(device::Query::Uart),
            "--query-wifi" => args.requests.queries.push(device::Query::Wifi),
            "--query-webdav" => args.requests.queries.push(device::Query::Webdav),
            "--uart" => args.requests.uart = Some(value("--uart")),
            "--wifi" => {
                let raw = value("--wifi");
                // `SSID[,PASSWORD]`: the password is optional here so that it
                // can come from --wifi-key-file instead, and stay out of argv.
                let (ssid, password) = match raw.split_once(',') {
                    Some((ssid, password)) => (ssid.to_string(), password.to_string()),
                    None => (raw, String::new()),
                };
                args.requests.wifi = Some((ssid, password));
            }
            "--wifi-key-file" => args.wifi_key_file = Some(PathBuf::from(value("--wifi-key-file"))),
            "--wifi-off" => args.requests.wifi_off = true,
            "--wifi-scan" => args.requests.wifi_scan = true,
            "--webdav" => args.requests.webdav = Some(value("--webdav")),
            "--webdav-off" => args.requests.webdav_off = true,
            "--loopback-test" => {
                // `nargs="?"` in the reference client: `--loopback-test` alone
                // sends `A`, which is the shortest thing that can fail in an
                // interesting way and the least likely to be swallowed by a
                // target that is doing something else.
                let mut payload = "A".to_string();
                if let Some(next) = argv.get(i + 1) {
                    if !next.starts_with('-') {
                        payload = next.clone();
                        i += 1;
                    }
                }
                args.loopback = Some(payload);
            }
            "--loopback-timeout" => {
                let raw = value("--loopback-timeout");
                let seconds: f64 = raw.parse().ok().filter(|s| *s > 0.0).unwrap_or_else(|| {
                    fail(
                        prog,
                        &format!("invalid value '{raw}' for '--loopback-timeout <SECONDS>'"),
                    )
                });
                args.loopback_timeout = Duration::from_secs_f64(seconds);
            }
            "--enter" => {
                let raw = value("--enter");
                args.enter = EnterMode::from_name(&raw).unwrap_or_else(|| {
                    fail(prog, &format!("invalid value '{raw}' for '--enter <MODE>'"))
                });
            }
            "--local-echo" => args.local_echo = true,
            "--line-mode" => args.line_mode = true,
            "--escape" => {
                let raw = value("--escape");
                args.escape = cli::parse_escape(&raw).unwrap_or_else(|why| {
                    fail(
                        prog,
                        &format!("invalid value '{raw}' for '--escape <BYTE>': {why}"),
                    )
                });
            }
            "--log-file" => args.log_file = Some(PathBuf::from(value("--log-file"))),
            "--debug-io" => args.debug_io = true,
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
                args.transport = Some(Transport::from_name(&raw).unwrap_or_else(|| {
                    fail(
                        prog,
                        &format!("invalid value '{raw}' for '--transport <LINK>'"),
                    )
                }));
            }
            "--bridge" => args.bridge = Some(value("--bridge")),
            "--ble-name" => args.ble_name = Some(value("--ble-name")),
            "--ble-address" => args.ble_address = Some(value("--ble-address")),
            "--lan-host" => args.lan_host = Some(value("--lan-host")),
            "--lan-token" => args.lan_token = Some(value("--lan-token")),
            "--lan-token-file" => {
                args.lan_token_file = Some(PathBuf::from(value("--lan-token-file")))
            }
            "--overlay" => {
                let raw = value("--overlay");
                args.overlay = Some(OverlayPreview::from_name(&raw).unwrap_or_else(|| {
                    fail(
                        prog,
                        &format!("invalid value '{raw}' for '--overlay <NAME>'"),
                    )
                }));
            }
            "--feed" => args.feed = Some(value("--feed")),
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

/// Undo raw mode and the alternate screen without a handle to the terminal,
/// for the panic hook and for the guard's own drop.
fn restore_best_effort() {
    let _ = disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = execute!(stdout, LeaveAlternateScreen);
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

    let mut args = parse_args(&prog, &argv);
    // The password is read as late as possible and never put back: the point of
    // `--wifi-key-file` is that `argv` is visible to every process on the
    // machine, and a password copied from the command line into a struct is a
    // password that travelled anyway.
    if let Some(path) = &args.wifi_key_file {
        let password = match read_first_line(path) {
            Ok(password) => password,
            Err(why) => {
                eprintln!("error: {why}");
                std::process::exit(2);
            }
        };
        let ssid = args
            .requests
            .wifi
            .as_ref()
            .map_or("", |pair| pair.0.as_str());
        if ssid.trim().is_empty() {
            eprintln!("error: --wifi-key-file needs --wifi to name the network too");
            std::process::exit(2);
        }
        args.requests.wifi = Some((ssid.to_string(), password));
    }

    // A scan answers a question nothing else can — is the board on the air at
    // all — so it runs before a dial is even considered and stops afterwards.
    if args.scan {
        match cli::scan_devices(args.scan_timeout) {
            Ok(devices) => {
                for device in &devices {
                    let name = device.name.as_deref().unwrap_or("(no name advertised)");
                    // Marked, because a board that is advertising but not
                    // offering the serial service is a firmware problem and not
                    // a radio one, and the difference is what to do next.
                    let kind = if device.nus { "nus" } else { "-" };
                    println!("{kind:>3}  {name}  {}", device.id);
                }
                if devices.is_empty() && !args.quiet {
                    eprintln!(
                        "no Bluetooth device was seen in {} s",
                        args.scan_timeout.as_secs()
                    );
                }
                return Ok(());
            }
            Err(why) => {
                eprintln!("error: {why}");
                std::process::exit(1);
            }
        }
    }

    // A dial that cannot be *asked for* — an unreadable token file, a
    // mis-shaped token — is said here rather than after a socket is opened:
    // it is a fact about the command line, not about the network.
    let dial = match args.dial() {
        Ok(dial) => dial,
        Err(why) => {
            eprintln!("error: {why}");
            std::process::exit(2);
        }
    };

    // Without `--state`, a *preview* shows the design's first frame — a link
    // being dialled. A live session starts from the truth instead: nothing is
    // connected until something has been dialled, and a frame that claimed
    // otherwise would be lying about a radio that may not exist.
    let preview_state = args.state.unwrap_or(if args.sample {
        ConnectionState::Connected
    } else {
        ConnectionState::Connecting
    });

    let mut app = App {
        lang: args.lang,
        theme: args.theme,
        state: preview_state,
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
        app.terminal.push_text(views::terminal::SAMPLE);
        // Two announcements as well, so a previewed notice log has something to
        // show. They are the strings the notices really use, composed the way
        // the pump composes them, so the preview cannot show wording the
        // interface never produces.
        let s = app.strings();
        app.push_notice(linkr_tui::app::Notice {
            text: format!("{}{}", s.link_opened, Dial::Stdio("-".into()).describe()),
            level: linkr_tui::event::NoticeLevel::Info,
        });
        app.push_notice(linkr_tui::app::Notice {
            text: s.not_connected.to_string(),
            level: linkr_tui::event::NoticeLevel::Warn,
        });
    }
    match args.overlay {
        Some(OverlayPreview::Palette) => palette::open(&mut app),
        Some(OverlayPreview::Help) => dialogs::open_help(&mut app),
        Some(OverlayPreview::Notices) => dialogs::open_notices(&mut app),
        Some(OverlayPreview::Confirm) => {
            // The guarded preset this build ships, so the preview shows the
            // confirmation the user will really meet.
            let command = app
                .quick_send
                .last()
                .map_or_else(|| "reboot".to_string(), |quick| quick.command.clone());
            dialogs::ask(&mut app, dialogs::ConfirmAction::Send(command));
        }
        Some(OverlayPreview::SendFile) => dialogs::prompt_send_file(&mut app),
        Some(OverlayPreview::Receive) => dialogs::prompt_receive(&mut app),
        None => {}
    }

    if let Some(text) = &args.feed {
        // Straight into the pane, through the same decoder and parser the wire
        // uses, so an inspection cannot show an emulator other than the one the
        // device talks to.
        app.terminal.push_text(text);
    }

    if let Some((cols, rows)) = args.print_frame {
        // The frame is drawn for a *sized* terminal, so the emulator is told the
        // size the paint will use — the same arithmetic the event loop runs
        // every tick. A preview drawn by an 80-column emulator inside a
        // 110-column frame would wrap the device's lines early and read as an
        // emulator bug when it is a preview bug.
        fit_pane_to(cols, rows, &mut app);
        if args.ansi {
            print!("{}", ui::render_to_ansi(&app, cols, rows));
        } else {
            print!("{}", ui::render_to_text(&app, cols, rows));
        }
        return Ok(());
    }

    if let Some(dial) = dial {
        // The dial happens before the terminal is taken over, so a link that
        // cannot be opened prints an ordinary error instead of a corrupted
        // screen.
        if let Err(err) = app.dial(&dial) {
            eprintln!("error: {err}");
            std::process::exit(2);
        }
        // `dial` already pinned it; a dial the command line decided cannot be
        // switched from the keyboard.
    } else {
        app.state = ConnectionState::Disconnected;
    }

    // Wire settings the command line named, applied to the state rather than
    // read back from it: the pane asks `app` what to do with a keystroke, and
    // a flag that only changed a variable here would leave the pane using its
    // own default.
    app.enter = args.enter;
    app.escape = args.escape;
    app.line_mode = args.line_mode;
    if args.local_echo {
        app.terminal.echo = true;
    }
    // The trace is opened before anything is dialled, so the bytes of the dial
    // itself are in it: a log that starts after the handshake is a log missing
    // the part where the handshake is negotiated, which is the part that fails.
    if let Some(path) = &args.log_file {
        app.trace = match linkr_tui::trace::ByteTrace::to_file(path) {
            Ok(trace) => trace,
            Err(why) => {
                eprintln!(
                    "error: {} could not be opened for appending: {why}",
                    path.display()
                );
                std::process::exit(2);
            }
        };
    }
    if args.debug_io {
        app.trace = std::mem::take(&mut app.trace).with_stderr();
    }

    // A script run: connect, ask, report, hang up. It is reached when
    // `--no-terminal` was given *or* when something was asked for, because a
    // command line that names `--query-info` is asking for an answer and not
    // for an interface — printing the answer and leaving is what it meant.
    if args.no_terminal || !args.requests.is_empty() || args.loopback.is_some() {
        if !args.quiet && app.session.is_live() {
            eprintln!("connected to {}", app.session.describe());
        }
        let format = if args.json {
            cli::Format::Json
        } else {
            cli::Format::Text
        };
        let mut failed = cli::run_requests(&mut app, &args.requests, format);
        // The loopback goes last and stands alone: it is answered by the
        // target's UART rather than by the bridge, so running it before the
        // management requests would put its echo in what they read back.
        if let Some(payload) = &args.loopback {
            if !cli::loopback_test(&mut app, payload.as_bytes(), args.loopback_timeout, format) {
                failed += 1;
            }
        }
        app.session.close();
        if failed > 0 {
            std::process::exit(1);
        }
        return Ok(());
    }

    run(&mut app)
}

/// The first line of `path`, which is how a secret is handed over: a file the
/// shell cannot put in `argv` and `ps` cannot read back.
fn read_first_line(path: &std::path::Path) -> Result<String, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|why| format!("{} could not be read: {why}", path.display()))?;
    // The reference client takes the first line and ignores the rest, so a
    // file with a trailing newline behaves the way a key file is expected to.
    Ok(text.lines().next().unwrap_or("").trim().to_string())
}

/// The centre region — the terminal pane's area — for a frame of this size.
fn pane_area(cols: u16, rows: u16) -> Rect {
    ui::regions(Rect::new(0, 0, cols, rows)).centre
}

/// Size the emulator for a frame of `cols`×`rows`, and hand back the pane it
/// got.
///
/// Both callers go through here — the preview, which knows the size it was asked
/// to draw, and the event loop, which asks the terminal — so a preview cannot
/// size the pane differently from the live loop.
fn fit_pane_to(cols: u16, rows: u16, app: &mut App) -> Rect {
    let pane = pane_area(cols, rows);
    app.terminal.set_size(pane.width, pane.height);
    pane
}

/// Tell the emulator how big its pane is, from the terminal's own size.
///
/// The paint cannot do this: a frame is a pure function of the state, so it may
/// not reshape the screen it is drawing. The loop does it instead, from the same
/// layout arithmetic the paint uses — and it re-reads the size rather than
/// caching it, so a resize needs no path of its own.
fn fit_pane(terminal: &Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> io::Result<()> {
    let size = terminal.size()?;
    fit_pane_to(size.width, size.height, app);
    Ok(())
}

/// The event loop: paint, then wait for input or the next tick.
fn run(app: &mut App) -> io::Result<()> {
    let mut guard = TerminalGuard::enter()?;

    // A panic must not leave the terminal in raw mode; the hook runs before the
    // process aborts, so the restore below is the last thing that happens.
    let fallback = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_best_effort();
        fallback(info);
    }));

    let outcome = event_loop(&mut guard.terminal, app);
    guard.restore();
    outcome
}

fn event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> io::Result<()> {
    loop {
        // Resize before the pump: the bytes about to arrive are painted at the
        // size the pane is now, not the size it was when the user dragged the
        // window.
        fit_pane(terminal, app)?;
        app.pump();
        app.clock = local_clock();
        terminal.draw(|frame| ui::draw(frame, app))?;

        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    // A quit can arrive two ways: the chord says so directly,
                    // and a palette action sets the flag — both end the loop,
                    // so the two routes cannot diverge.
                    if handle_key(app, key) || app.quit {
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
///
/// The order matters and is the whole policy. A modal takes the keyboard first
/// — the dialog before the palette, because the dialog is the one the user is
/// answering — but a *control* chord falls through both, so `Ctrl+Q` still
/// quits and `Ctrl+T` still repaints with an overlay up. Then the global
/// chords, then the sidebar, and only what is left is the device's.
fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.dialog.is_some() && dialogs::handle_key(app, key) {
        return app.quit;
    }
    if app.palette.is_some() && palette::handle_key(app, key) {
        return app.quit;
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // Chords the interface owns, in every focus and every view.
    match (key.code, ctrl) {
        (KeyCode::Char('q'), true) | (KeyCode::Char('c'), true) => {
            app.request_quit();
            return true;
        }
        // The palette's chord is a toggle: pressing it again puts the overlay
        // away, which is what a key that opened a window is expected to do.
        (KeyCode::Char('p'), true) => {
            if app.palette.is_some() {
                app.palette = None;
            } else {
                palette::open(app);
            }
            return false;
        }
        (KeyCode::Char('t'), true) => {
            let notice = app.toggle_theme();
            app.push_notice(notice);
            return false;
        }
        (KeyCode::Char('l'), true) => {
            app.clear_terminal();
            return false;
        }
        // The transfer's own chord, and only while one is running: with nothing
        // in flight `Ctrl+X` is the device's, like every other control
        // character, and taking it always would break a program that wants it.
        (KeyCode::Char('x'), true) if app.transferring() => {
            app.cancel_transfer();
            return false;
        }
        // F1 is the help, not a view: the view rota starts at F2.
        (KeyCode::F(1), _) => {
            dialogs::open_help(app);
            return false;
        }
        (KeyCode::Tab, _) => {
            app.focus = match app.focus {
                Focus::Terminal => Focus::Sidebar,
                Focus::Sidebar => Focus::Terminal,
            };
            return false;
        }
        (KeyCode::F(number), _) => {
            if let Some(view) = View::from_fkey(number) {
                app.set_view(view);
            }
            return false;
        }
        _ => {}
    }

    if app.focus == Focus::Sidebar {
        match key.code {
            KeyCode::Down => app.cursor_down(),
            KeyCode::Up => app.cursor_up(),
            // Enter does what the row says: change the transport, dial, open a
            // view, or send a preset — the guarded ones asking first.
            KeyCode::Enter => ui::sidebar::activate(app),
            _ => {}
        }
        return false;
    }

    // The network view is a form and takes the keyboard first: its rows are
    // what Enter means there, and falling through to "the device's" would type
    // a newline into the console every time somebody pressed an action.
    if app.view == View::Network && linkr_tui::views::network::handle_key(app, key) {
        return false;
    }

    // The transfer form is the same kind of thing, and it owns the arrow keys
    // too: its rows are walked with them, and a device being typed at with the
    // arrows of a form would see escape sequences it never asked for.
    if app.view == View::Transfer && linkr_tui::views::transfer::handle_key(app, key) {
        return false;
    }

    // The keyboard is on the pane: everything left is the device's.
    match key.code {
        // PageUp/PageDown walk the pane's own history rather than the device's
        // scroll buffer: without a VT screen there is no device scroll to walk.
        KeyCode::PageUp => app.scroll_terminal(10),
        KeyCode::PageDown => {
            let back = app.terminal.scroll.saturating_sub(10);
            app.terminal.scroll = back;
            if back == 0 {
                app.scroll_terminal_to_bottom();
            }
        }
        KeyCode::End => app.scroll_terminal_to_bottom(),
        _ => {
            if let Some(bytes) = key_bytes(key, app.enter) {
                // The escape byte is the wire's way out, and it is checked
                // before the keystroke is sent: sending it and *then* leaving
                // would put a stray control character into the device's input,
                // which is the one thing an escape key must never do.
                if bytes.as_bytes() == [app.escape] {
                    app.request_quit();
                    return true;
                }
                app.type_bytes(key.code == KeyCode::Enter, &bytes);
            }
        }
    }
    false
}

/// The bytes a keystroke sends to the device, or `None` for a key the device
/// has no use for. The named keys go through the ported `terminal_keys.js`
/// table, so the two clients cannot drift apart on what `F5` or `ArrowUp`
/// means.
fn key_bytes(key: KeyEvent, enter: EnterMode) -> Option<String> {
    let modifiers = TerminalModifiers {
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
    };
    // Application cursor keys are a mode the device turns on; until the VT
    // screen tracks it, the normal form is what is sent.
    let application_cursor_keys = false;
    let named = |name: &str| keys::terminal_key_sequence(name, application_cursor_keys, &modifiers);
    let bytes = match key.code {
        KeyCode::Char(c) => keys::apply_input_modifiers(&c.to_string(), &modifiers),
        // Enter is the one key the command line may re-wire, because a target
        // that wants `\r` and is given `\n` simply never answers — and the
        // symptom looks exactly like a dead link.
        KeyCode::Enter => match enter.bytes() {
            Some(bytes) => bytes.to_string(),
            None => named("Enter"),
        },
        KeyCode::Backspace => named("Backspace"),
        KeyCode::Tab => named("Tab"),
        KeyCode::BackTab => named("Tab"),
        KeyCode::Esc => named("Escape"),
        KeyCode::Up => named("ArrowUp"),
        KeyCode::Down => named("ArrowDown"),
        KeyCode::Left => named("ArrowLeft"),
        KeyCode::Right => named("ArrowRight"),
        KeyCode::Home => named("Home"),
        KeyCode::End => named("End"),
        KeyCode::Delete => named("Delete"),
        KeyCode::Insert => named("Insert"),
        _ => return None,
    };
    if bytes.is_empty() {
        None
    } else {
        Some(bytes)
    }
}

/// The wall clock in `HH:MM:SS`, local time.
fn local_clock() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only a test names a link by kind: the binary itself deals in `Dial`s.
    use linkr_tui::transport::LinkKind;

    fn args(list: &[&str]) -> Args {
        let owned: Vec<String> = list.iter().map(|s| s.to_string()).collect();
        parse_args("linkr", &owned)
    }

    /// The wire settings are read off the command line in the forms the
    /// reference client takes, and the escape byte defaults to the one it
    /// reserves.
    #[test]
    fn the_wire_settings_parse_and_default_to_the_reference_clients() {
        assert_eq!(args(&[]).escape, 0x1d, "^] by default");
        assert_eq!(args(&["--escape", "^]"]).escape, 0x1d);
        assert_eq!(args(&["--escape", "0x02"]).escape, 0x02);
        assert!(!args(&[]).line_mode);
        assert!(args(&["--line-mode"]).line_mode);
        assert!(!args(&[]).debug_io);
        assert!(args(&["--debug-io"]).debug_io);
        assert_eq!(
            args(&["--log-file", "/tmp/rx.log"]).log_file.as_deref(),
            Some(std::path::Path::new("/tmp/rx.log"))
        );
    }

    /// `--loopback-test` takes its payload or falls back to `A`, and the next
    /// flag is never eaten as a payload — `--loopback-test --json` is a test
    /// with the default payload.
    #[test]
    fn the_loopback_payload_is_optional_and_never_a_flag() {
        assert_eq!(args(&[]).loopback, None);
        assert_eq!(args(&["--loopback-test"]).loopback.as_deref(), Some("A"));
        assert_eq!(
            args(&["--loopback-test", "hello"]).loopback.as_deref(),
            Some("hello")
        );
        let parsed = args(&["--loopback-test", "--json"]);
        assert_eq!(parsed.loopback.as_deref(), Some("A"));
        assert!(parsed.json, "--json was still read as a flag");
        assert_eq!(
            args(&["--loopback-timeout", "1.5"])
                .loopback_timeout
                .as_millis(),
            1500
        );
    }

    /// A WiFi scan is one of the things the bridge can be asked for, and it is
    /// what makes a script run rather than a terminal.
    #[test]
    fn a_wifi_scan_is_a_request_and_not_a_view() {
        let parsed = args(&["--wifi-scan"]);
        assert!(parsed.requests.wifi_scan);
        assert!(!parsed.requests.is_empty(), "so it runs and exits");
        assert!(args(&[]).requests.is_empty());
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
        assert_eq!(
            parsed.dial().expect("the command line parses"),
            None,
            "nothing is dialled by default"
        );
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
        app.terminal.push_text(views::terminal::SAMPLE);
        assert!(app.diagnostics.has_data());
        assert!(app.diagnostics.done);
        assert_eq!(app.diagnostics.updated.as_deref(), Some("19:08:06"));
        assert!(
            !app.terminal.is_empty(),
            "the sample fills the pane as well as the grid"
        );

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
        // F1 is help, not a view: it opens the overlay and leaves the view be.
        handle_key(&mut app, KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
        assert_eq!(app.view, View::Transfer, "F1 is help, not a view");
        assert!(
            matches!(app.dialog, Some(linkr_tui::dialogs::Dialog::Help(0))),
            "F1 opened the help: {:?}",
            app.dialog
        );
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

    /// Ctrl+L clears the pane and nothing else — the device is not sent a
    /// character, and the counters are not moved.
    #[test]
    fn control_l_clears_the_pane() {
        let mut app = App::default();
        app.terminal.push_text("stale output\n");
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL),
        );
        assert!(app.terminal.is_empty());
        assert_eq!(app.tx, 0, "clear is local, not a keystroke");
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

    /// The pane's keys are the device's, and they go out as the byte sequences
    /// `terminal_keys.js` specifies — the two clients cannot drift apart here.
    #[test]
    fn the_pane_keys_send_the_ported_sequences() {
        assert_eq!(
            key_bytes(
                KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
                EnterMode::Raw
            ),
            Some("a".into())
        );
        assert_eq!(
            key_bytes(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                EnterMode::Raw
            ),
            Some("\r".into())
        );
        assert_eq!(
            key_bytes(
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
                EnterMode::Raw
            ),
            Some("\u{7f}".into())
        );
        assert_eq!(
            key_bytes(
                KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
                EnterMode::Raw
            ),
            Some("\t".into())
        );
        assert_eq!(
            key_bytes(
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                EnterMode::Raw
            ),
            Some("\u{1b}".into())
        );
        assert_eq!(
            key_bytes(
                KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
                EnterMode::Raw
            ),
            Some("\u{1b}[A".into())
        );
        assert_eq!(
            key_bytes(
                KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
                EnterMode::Raw
            ),
            Some("\u{1b}[3~".into())
        );
        // Ctrl+C on the pane is the interrupt, not a quit: the chord is the
        // device's once the global binding has been checked.
        assert_eq!(
            key_bytes(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                EnterMode::Raw
            ),
            Some("\u{3}".into())
        );
        // Shift+Tab is the back-tab the device expects.
        assert_eq!(
            key_bytes(
                KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT),
                EnterMode::Raw
            ),
            Some("\u{1b}[Z".into())
        );
        // A key the device has no use for sends nothing at all.
        assert_eq!(
            key_bytes(
                KeyEvent::new(KeyCode::F(9), KeyModifiers::NONE),
                EnterMode::Raw
            ),
            None
        );
        assert_eq!(
            key_bytes(
                KeyEvent::new(KeyCode::Null, KeyModifiers::NONE),
                EnterMode::Raw
            ),
            None
        );
    }

    /// A dial is what the command line asked for: `--bridge` reaches a process,
    /// `--bridge` names a process and nothing else names a link at all.
    #[test]
    fn the_dial_follows_the_command_line() {
        assert_eq!(
            args(&["--bridge", "cat"])
                .dial()
                .expect("the command line parses"),
            Some(Dial::Stdio("cat".into()))
        );
        assert_eq!(
            args(&["--bridge", "-"])
                .dial()
                .expect("the command line parses"),
            Some(Dial::Stdio("-".into())),
            "the loopback is spelled -"
        );
        // Both: the bridge is the way in, and it wins rather than erroring.
        assert_eq!(
            args(&["--ble-name", "Linkr", "--bridge", "cat"])
                .dial()
                .expect("the command line parses"),
            Some(Dial::Stdio("cat".into()))
        );
    }

    /// A Bluetooth dial is aimed either by address or by name, and an address
    /// is the stronger claim of the two: a name finds a board *like* the one
    /// meant, an address finds it. With neither, it takes whatever is on the
    /// air — right for a bench with one board, which most are.
    #[test]
    fn a_bluetooth_dial_is_aimed_by_address_then_by_name() {
        assert_eq!(
            args(&["--transport", "ble"])
                .dial()
                .expect("the command line parses"),
            Some(Dial::Ble(Selection::Any)),
            "pinned to Bluetooth and nothing named: whichever one it finds"
        );
        assert_eq!(
            args(&["--ble-name", "Linkr"])
                .dial()
                .expect("the command line parses"),
            Some(Dial::Ble(Selection::Name("Linkr".into())))
        );
        assert_eq!(
            args(&["--ble-address", "EE:C7:42:34:48:CF"])
                .dial()
                .expect("the command line parses"),
            Some(Dial::Ble(Selection::Address("EE:C7:42:34:48:CF".into())))
        );
        assert_eq!(
            args(&["--ble-name", "Linkr", "--ble-address", "EE:C7:42:34:48:CF"])
                .dial()
                .expect("the command line parses"),
            Some(Dial::Ble(Selection::Address("EE:C7:42:34:48:CF".into()))),
            "and the address wins when both are given"
        );
    }

    /// A LAN dial carries the bridge's address and, when one was offered, the
    /// token to answer with. Offering none is not a failure: a bridge that did
    /// not ask for one still gets dialled, and sending a token nobody wants is
    /// how a board comes to believe it is secured.
    #[test]
    fn a_lan_dial_carries_the_token_it_was_given() {
        const TOKEN: &str = "0123456789abcdef0123456789abcdef";

        // An invented host: nothing can have been remembered for it, so "no
        // token" here is the store contributing nothing rather than a lookup
        // that happened to miss.
        let bare = args(&["--lan-host", "lan-none.example.invalid"])
            .lan()
            .expect("a host alone is a complete dial");
        assert_eq!(
            bare,
            Dial::Lan {
                host: "lan-none.example.invalid".into(),
                token: None
            },
            "nothing was offered, so nothing is sent"
        );
        assert_eq!(bare.describe(), "lan:lan-none.example.invalid");

        let named = args(&["--lan-host", "192.168.0.44", "--lan-token", TOKEN])
            .lan()
            .expect("a real token");
        assert_eq!(
            named,
            Dial::Lan {
                host: "192.168.0.44".into(),
                token: Some(TOKEN.into())
            }
        );
    }

    /// A token that cannot be used is refused while the command line is being
    /// read, and the refusal names the place it came from — "not 32 lowercase
    /// hex" is not a sentence about a file the caller did not choose.
    #[test]
    fn an_unusable_token_is_refused_before_the_dial() {
        let why = args(&["--lan-host", "192.168.0.44", "--lan-token", "NOPE"])
            .dial()
            .expect_err("not a token");
        assert!(why.starts_with("--lan-token"), "{why}");

        let why = args(&[
            "--lan-host",
            "192.168.0.44",
            "--lan-token-file",
            "/nonexistent/linkr-token",
        ])
        .dial()
        .expect_err("no such file");
        assert!(why.starts_with("/nonexistent/linkr-token"), "{why}");
    }

    /// Pinning the LAN transport asks for the LAN link even before an address
    /// is named; what it cannot do is dial nowhere, which is the dial's own
    /// error rather than a usage failure.
    #[test]
    fn pinning_the_lan_transport_asks_for_the_lan_link() {
        let dial = args(&["--transport", "lan"])
            .dial()
            .expect("a pin is not an error")
            .expect("and it names a link");
        assert_eq!(dial.kind(), LinkKind::Lan);
    }

    /// `--transport` names a link, and now names all four: the pin is the
    /// sidebar's word for it, not a second list that can drift.
    #[test]
    fn the_transport_flag_takes_every_link() {
        for kind in Transport::ALL {
            assert_eq!(
                args(&["--transport", kind.as_str()]).transport,
                Some(kind),
                "{kind:?}"
            );
        }
        assert_eq!(
            args(&["--transport", "STDIO"]).transport,
            Some(Transport::Stdio)
        );
    }

    /// `--overlay` opens a modal by name, and a name that is not a modal is
    /// refused rather than silently showing nothing.
    #[test]
    fn the_overlay_flag_selects_a_modal_by_name() {
        assert_eq!(args(&[]).overlay, None);
        for (name, expected) in [
            ("palette", OverlayPreview::Palette),
            ("help", OverlayPreview::Help),
            ("notices", OverlayPreview::Notices),
            ("confirm", OverlayPreview::Confirm),
            ("send", OverlayPreview::SendFile),
            ("receive", OverlayPreview::Receive),
        ] {
            assert_eq!(args(&["--overlay", name]).overlay, Some(expected), "{name}");
        }
        assert_eq!(
            args(&["--overlay", "PALETTE"]).overlay,
            Some(OverlayPreview::Palette),
            "the name is case-blind"
        );
        assert_eq!(OverlayPreview::from_name("nope"), None);
    }

    /// `--feed` takes the text it is handed, escapes and all: it exists to look
    /// at the emulator without a device, so it must not mangle what it is given.
    #[test]
    fn the_feed_flag_takes_text_verbatim() {
        assert_eq!(args(&[]).feed, None);
        let parsed = args(&["--feed", "\x1b[31mred\x1b[0m"]);
        assert_eq!(parsed.feed.as_deref(), Some("\x1b[31mred\x1b[0m"));
    }

    /// A fed frame is painted by an emulator the size of the frame's pane.
    ///
    /// The pane's size is not the frame's: it is the centre region, after the
    /// bars and the sidebar. That is exactly the arithmetic the event loop runs
    /// every tick, so a preview cannot show a device wrapped at the default 80
    /// columns inside a wider frame and read as an emulator bug.
    #[test]
    fn a_fed_frame_is_painted_by_a_pane_of_the_frames_size() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        // The default the emulator starts at, which is what a preview used to
        // keep however wide the frame was.
        assert_eq!(app.terminal.size(), (80, 24));

        let pane = fit_pane_to(140, 30, &mut app);
        assert!(
            pane.width > 80,
            "the pane is wider than the default: {pane:?}"
        );
        assert_eq!(app.terminal.size(), (pane.width, pane.height));

        app.terminal.push_text(&"x".repeat(pane.width as usize));
        // The whole screen, because a one-row window is the *bottom* of the
        // screen and the fed line is at the top of it.
        let rows = views::terminal::rows(&app.terminal, pane.width as usize, pane.height as usize);
        assert_eq!(
            rows[0].chars().count(),
            pane.width as usize,
            "a full-width line does not wrap early: {rows:?}"
        );
        assert_eq!(rows[1], "", "and it is one line, not two: {rows:?}");
    }

    /// `--print-frame --overlay` draws the modal, in the language asked for —
    /// this is how the modal layer is reviewed without a terminal.
    #[test]
    fn a_previewed_overlay_reaches_the_frame() {
        for (overlay, needle) in [
            (OverlayPreview::Palette, "命令面板"),
            (OverlayPreview::Help, "键盘帮助"),
            (OverlayPreview::Notices, "通知记录"),
            (OverlayPreview::Confirm, "请确认"),
            (OverlayPreview::SendFile, "发送文件"),
            (OverlayPreview::Receive, "接收文件"),
        ] {
            let mut app = App::default();
            match overlay {
                OverlayPreview::Palette => palette::open(&mut app),
                OverlayPreview::Help => dialogs::open_help(&mut app),
                OverlayPreview::Notices => dialogs::open_notices(&mut app),
                OverlayPreview::Confirm => {
                    dialogs::ask(&mut app, dialogs::ConfirmAction::Send("reboot".into()))
                }
                OverlayPreview::SendFile => dialogs::prompt_send_file(&mut app),
                OverlayPreview::Receive => dialogs::prompt_receive(&mut app),
            }
            let frame = linkr_tui::ui::render_to_text(&app, 100, 30);
            assert!(frame.contains(needle), "{overlay:?}:\n{frame}");
        }
    }

    /// Ctrl+X is the transfer's own chord, and only while one is running. With
    /// nothing in flight it stays a control character for the device, which is
    /// what a terminal owes a program that wants it.
    #[test]
    fn control_x_cancels_a_transfer_and_is_the_devices_otherwise() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        app.dial(&Dial::Stdio("-".into()))
            .expect("the loopback opens");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline && !app.connected() {
            app.pump();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(app.connected());

        // No transfer: the chord is not consumed, so the byte goes out.
        assert!(!handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)
        ));
        assert_eq!(app.tx, 1, "the control character reached the device");
        assert!(!app.transferring());

        // With one running it is the transfer's, and it cancels.
        assert!(app.start_receive(std::path::PathBuf::from(".")));
        assert!(app.transferring());
        assert!(!handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)
        ));
        assert!(!app.transferring(), "the chord cancelled the transfer");
        assert_eq!(app.tx, 1, "and nothing was sent while it was running");
    }

    /// `--sample` fills the notice log as well as the grid and the pane, so a
    /// previewed log shows the *real* wording rather than a placeholder.
    #[test]
    fn sample_fills_the_notice_log() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        let s = app.strings();
        app.push_notice(linkr_tui::app::Notice {
            text: format!("{}{}", s.link_opened, Dial::Stdio("-".into()).describe()),
            level: linkr_tui::event::NoticeLevel::Info,
        });
        assert!(
            app.log[0].text.starts_with("link open: "),
            "{:?}",
            app.log[0].text
        );
        assert!(
            app.log[0].text.ends_with("stdio: -"),
            "{:?}",
            app.log[0].text
        );
        assert_eq!(
            app.log[0].text,
            format!("{}{}", s.link_opened, "stdio: -"),
            "the preview cannot show wording the pump would not produce"
        );
    }

    /// `Ctrl+P` opens the palette and opens it *again* to put it away, the way
    /// a key that opened a window is expected to behave. While it is up it owns
    /// the keyboard, so a plain letter is a query character and not a keystroke
    /// for the device.
    #[test]
    fn control_p_toggles_the_palette_and_it_takes_the_keyboard() {
        let mut app = App::default();
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        );
        assert!(app.palette.is_some(), "Ctrl+P opened the palette");

        // A letter edits the query; the device hears nothing.
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
        );
        assert_eq!(
            app.palette.as_ref().map(|s| s.query.as_str()),
            Some("t"),
            "the letter went into the query"
        );
        assert_eq!(app.tx, 0, "and not to the device");

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        );
        assert!(app.palette.is_none(), "Ctrl+P again put it away");
    }

    /// With the palette open, a control chord is still the interface's: `Ctrl+Q`
    /// quits and `Ctrl+T` repaints rather than typing into the search box.
    #[test]
    fn a_control_chord_still_works_with_the_palette_open() {
        let mut app = App::default();
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        );

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.theme, ThemeChoice::Light, "Ctrl+T still switches");
        assert_eq!(app.notices.len(), 1);

        // Ctrl+Q sets the flag the loop reads.
        assert!(handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)
        ));
        assert!(app.quit);
    }

    /// A function key reaches the view rota even with the palette up: the user
    /// who reaches for `F2` wants the view, not an unmatched search.
    #[test]
    fn a_function_key_reaches_the_view_rota_from_the_palette() {
        let mut app = App::default();
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        );
        handle_key(&mut app, KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE));
        assert_eq!(app.view, View::Network);
        assert!(app.palette.is_some(), "the palette stayed open");
    }

    /// A dialog owns the keyboard while it is up: the pane hears nothing, the
    /// global chords still work, and Escape gives the keyboard back.
    #[test]
    fn a_dialog_owns_the_keyboard_until_it_is_closed() {
        let mut app = App::default();
        handle_key(&mut app, KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
        assert!(app.dialog.is_some());

        // A letter that would otherwise be a device keystroke is consumed.
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        assert_eq!(app.tx, 0, "the dialog swallowed the keystroke");
        assert!(app.dialog.is_some(), "and stayed open");

        // The global chords are not swallowed.
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.theme, ThemeChoice::Light, "Ctrl+T still switches");
        assert!(app.dialog.is_some(), "and the dialog is still up");

        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.dialog.is_none(), "Escape closed it");
        // Now a letter is the device's again — refused out loud, because there
        // is no link, rather than silently dropped.
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        assert_eq!(app.tx, 0);
    }

    /// Enter on a sidebar row does what the row says: it opens a view, sends a
    /// preset, or — for the guarded one — asks first. The index the cursor
    /// reaches is the index `activate` understands, which is the same list
    /// `targets` builds.
    #[test]
    fn enter_activates_the_sidebar_row_under_the_cursor() {
        let mut app = App {
            focus: Focus::Sidebar,
            ..App::default()
        };

        // Row 3 is the first view, in `View::ALL` order.
        for _ in 0..3 {
            handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        assert_eq!(app.sidebar_cursor, 3);
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.view, View::ALL[0], "the row opened its view");

        // Walk to the `reboot` row — the last one — and ask for it. It must not
        // fire: it must raise a confirmation.
        let last = app.sidebar_rows() - 1;
        while app.sidebar_cursor < last {
            handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        assert_eq!(
            app.quick_send[last - (3 + View::ALL.len())].command,
            "reboot"
        );
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.tx, 0, "reboot was not sent");
        assert!(
            matches!(app.dialog, Some(linkr_tui::dialogs::Dialog::Confirm(_))),
            "reboot asked first: {:?}",
            app.dialog
        );
    }

    /// Enter on `Switch device` is a no-op that says so, rather than a button
    /// that looks live and does nothing.
    #[test]
    fn enter_on_switch_device_says_there_is_nothing_to_switch_to() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        app.focus = Focus::Sidebar;
        for _ in 0..2 {
            handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        assert_eq!(app.sidebar_cursor, 2);
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            app.log.last().map(|n| n.text.as_str()),
            Some("No other device to switch to yet.")
        );
    }

    /// A pane keystroke still reaches the device — the sidebar routing did not
    /// swallow it — and the palette's `clear` action is the same code `Ctrl+L`
    /// runs, so the two cannot drift.
    #[test]
    fn the_pane_still_gets_its_keystrokes() {
        use linkr_tui::session::Dial;
        let mut app = App::default();
        app.dial(&Dial::Stdio("-".into()))
            .expect("the loopback opens");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline
            && app.state != linkr_tui::event::ConnectionState::Connected
        {
            app.pump();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(app.connected());
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert_eq!(app.tx, 1, "the byte went out");
    }
}
