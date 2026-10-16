//! The Linkr Bee host terminal: a CLI and a TUI over one core.
//!
//! `main` does three things and nothing else: read the command line, set up or
//! hand over the terminal, and run the event loop. Everything else lives in the
//! library so it can be tested without a TTY.

use std::io::{self, Stdout};

// Only the build with a renderer installs a panic hook: a CLI build that fails
// prints its own error and exits, and a hook that restored a terminal nobody
// took over would be a hook doing nothing.
#[cfg(feature = "tui")]
use std::panic;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
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
/// The interface's beat. Only the build with a renderer pings it: the CLI half
/// ends without a loop, so a tick constant there is a constant nobody reads.
#[cfg(feature = "tui")]
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
    /// The prompt the network form's SSID row opens, so the text field and its
    /// caret can be looked at without a device — which is the only way to look
    /// at a dialog that is normally raised to *do* something.
    WifiSsid,
    /// The prompt the WebDAV row opens.
    WebdavUrl,
    /// The assistant's settings: five fields, a note list and a caret that has
    /// nowhere else to be looked at.
    Settings,
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
            "wifi" => Some(OverlayPreview::WifiSsid),
            "webdav" => Some(OverlayPreview::WebdavUrl),
            "settings" => Some(OverlayPreview::Settings),
            _ => None,
        }
    }

    /// Raise this modal on `app`, through the same doors the real keystrokes use.
    ///
    /// One function rather than a `match` in the run loop and another in the
    /// tests: a preview that opens a window a differently is a preview of
    /// something the user will never meet.
    fn raise(self, app: &mut App) {
        match self {
            OverlayPreview::Palette => palette::open(app),
            OverlayPreview::Help => dialogs::open_help(app),
            OverlayPreview::Notices => dialogs::open_notices(app),
            OverlayPreview::Confirm => {
                // The guarded preset this build ships, so the preview shows the
                // confirmation the user will really meet.
                let command = app
                    .quick_send
                    .last()
                    .map_or_else(|| "reboot".to_string(), |quick| quick.command.clone());
                dialogs::ask(app, dialogs::ConfirmAction::Send(command));
            }
            OverlayPreview::WifiSsid => dialogs::prompt_wifi_ssid(app),
            OverlayPreview::WebdavUrl => dialogs::prompt_webdav_url(app),
            // On the endpoint, which is the row a first-time user lands on.
            OverlayPreview::Settings => {
                views::assistant::open_settings(app, views::assistant::AgentField::Endpoint);
            }
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
    /// Run the asked-for commands first, then take the terminal over. The
    /// middle of the three modes, and the one nothing had a way to ask for:
    /// the command line either exited when it was done or never ran anything.
    cli_then_tui: bool,
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

/// The three ways the command line can be used, and the one thing that
/// decides between them.
///
/// A tool that is both a terminal and a script has to know which of the two it
/// is this time. The answer has always been one of three, but until now only
/// two were reachable: a command line that asked for something exited when it
/// was done, and one that asked for nothing opened the terminal. The middle —
/// configure the board, then *watch it* — had no spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Take the terminal over and stay there.
    Tui,
    /// Configure and report, then leave.
    Cli,
    /// Configure and report, then take the terminal over and stay there.
    CliThenTui,
}

impl Mode {
    /// Which mode the command line meant.
    ///
    /// `--no-terminal` and `--cli-then-tui` name a mode directly. The rest is
    /// inferred, because a command line that names `--query-info` is asking a
    /// question and not asking for an interface, and printing the answer and
    /// leaving is what it meant.
    fn from(args: &Args, prog: &str) -> Mode {
        if args.no_terminal && args.cli_then_tui {
            fail(
                prog,
                "the script mode and the configure-then-watch mode ask for \
                 opposite things; pick one",
            );
        }
        if args.no_terminal || args.cli_then_tui {
            return if args.cli_then_tui {
                Mode::CliThenTui
            } else {
                Mode::Cli
            };
        }
        // Nothing asked for, or a plain launch: the terminal is the answer.
        if args.requests.is_empty() && args.loopback.is_none() {
            return Mode::Tui;
        }
        // Something was asked for, so the answer is the thing being asked
        // for. This is the inference that has always been made.
        Mode::Cli
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
        cli_then_tui: false,
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
            "--cli-then-tui" => args.cli_then_tui = true,
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
/// Entered when the interface takes the terminal over, undone however the
/// program leaves. Only the build with a renderer has one: a CLI build never
/// takes the terminal, so a guard would be guarding nothing.
#[cfg(feature = "tui")]
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

#[cfg(feature = "tui")]
impl TerminalGuard {
    fn enter() -> io::Result<TerminalGuard> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(err) = execute!(stdout, EnterAlternateScreen) {
            // Raw mode is already on; undo it before giving up.
            let _ = disable_raw_mode();
            return Err(err);
        }
        // Bracketed paste, or the paste arrives as its keystrokes. Without this
        // a multi-line paste reaches the device's line editor as a burst of
        // separate edits — every newline an Enter the user never pressed, on a
        // shell that has already run the first half of it. With it, the whole
        // paste is one event carrying the whole payload, which is the only
        // shape in which a paste can be sent as one thing.
        if let Err(err) = execute!(stdout, EnableBracketedPaste) {
            let _ = disable_raw_mode();
            let _ = execute!(stdout, LeaveAlternateScreen);
            return Err(err);
        }
        /* Mouse capture, or the wheel belongs to the terminal rather than to
         * this program. Without it a wheel notch is whatever the emulator
         * decides it is - a scrollback jump, a page, nothing - and the rail
         * under the cursor moves by an amount nobody chose. With it, one notch
         * is one row, which is what a wheel means.
         *
         * It is also the only way a drag can be seen at all, and a drag is how
         * text gets copied out of the pane: the terminal's own selection is
         * unreachable from inside an alternate screen, so the program has to
         * make its own. */
        if let Err(err) = execute!(stdout, EnableMouseCapture) {
            let _ = disable_raw_mode();
            let _ = execute!(stdout, LeaveAlternateScreen);
            let _ = execute!(stdout, DisableBracketedPaste);
            return Err(err);
        }
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(TerminalGuard { terminal })
    }

    fn restore(&mut self) {
        let _ = execute!(self.terminal.backend_mut(), DisableMouseCapture);
        let _ = execute!(self.terminal.backend_mut(), DisableBracketedPaste);
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

#[cfg(feature = "tui")]
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Undo raw mode and the alternate screen without a handle to the terminal,
/// for the panic hook and for the guard's own drop.
/// Undo raw mode and the alternate screen without a handle, for the panic hook.
#[cfg(feature = "tui")]
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
    //
    // Gated on `cli` because it is the one request that needs no interface at
    // all: in a build with the interface only, a `--scan` would have to open a
    // terminal to do something that takes three seconds and prints three lines.
    #[cfg(feature = "cli")]
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
        transport_pinned: args.transport.is_some(),
        ..App::default()
    };
    // The agent's durable state, opened once here rather than lazily per call:
    // one store file for tasks, notes and the session, so a task written by
    // this run and a note written by the last one are the same file.
    app.agent_store = Some(linkr_tui::agent::store::JsonFile::load());
    load_agent_state(&mut app);
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
    if let Some(overlay) = args.overlay {
        overlay.raise(&mut app);
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
        // What the command line dialled is also what the sidebar's LAN rows
        // describe from now on: an address named once must not have to be
        // typed again to reconnect.
        if let Dial::Lan { host, token } = &dial {
            app.lan_host = host.clone();
            app.lan_token = token.clone().unwrap_or_default();
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

    // A script run: connect, ask, report, hang up. Reached for the two modes
    // that do the asking first, which is `--no-terminal` and
    // `--cli-then-tui` alike — what separates them is what happens *after*,
    // and that belongs below rather than in the condition here.
    //
    // Gated on `cli`. The mode is read with it, because in a build without it
    // there is no branch that reads the mode and an unused binding is a warning
    // the next reader has to explain.
    #[cfg(feature = "cli")]
    let mode = Mode::from(&args, &prog);
    #[cfg(feature = "cli")]
    if mode != Mode::Tui {
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
        // Only the pure script mode hangs up. The middle mode keeps the link
        // it just configured and takes the terminal over on it: a board whose
        // serial settings were just changed is exactly the board the user now
        // wants to watch.
        if mode == Mode::Cli {
            app.session.close();
            if failed > 0 {
                std::process::exit(1);
            }
            return Ok(());
        }
        if failed > 0 {
            // The link stays up either way: a failed request is a report, not
            // a disconnection, and the terminal is still the better way to
            // find out what went wrong.
            eprintln!(
                "note: {} request{} failed; opening the terminal anyway",
                failed,
                if failed == 1 { "" } else { "s" }
            );
        }
    }

    // The interface half. Reached when the command line asked for it and the
    // asking half did not end the program; compiled out of a build with no
    // renderer, where there is nothing to hand `app` to. The result is bound
    // rather than returned so a build with the interface off still has
    // something to answer, and an error out of the event loop is not dropped
    // on the floor.
    // Open listening. A user who runs this on a bench has one board in front of
    // them and did not ask for a chooser; asking them to open a scan is asking
    // them to do the work the program can do itself. The scan runs, and one
    // that hears exactly one board that speaks the serial service dials it —
    // or, if two are in the air, the list is shown to pick from.
    //
    // Not on a link the command line already asked for: `--address` and
    // `--bridge` name a board, and scanning past a named one would replace a
    // deliberate choice with whatever else was advertising.
    #[cfg(feature = "tui")]
    if !app.session.is_live() && app.transport == linkr_tui::app::Transport::Ble {
        let _ = app.start_ble_scan_connecting_one();
    }

    #[cfg(feature = "tui")]
    let outcome = run(&mut app);
    #[cfg(not(feature = "tui"))]
    let outcome: io::Result<()> = Ok(());
    outcome
}

/// Bring the agent's durable state back into the view.
///
/// Without this the store is write-only: a task saved and a model configured
/// both survive on disk and neither is ever read back, so the view is blank
/// after every restart and the form asks for the endpoint it already has. The
/// stores were built for a `Storage` that nobody opened, and this is the other
/// half of opening it.
fn load_agent_state(app: &mut App) {
    // The key is taken before the store is borrowed, because `agent_device_key`
    // reads `app` and a mutable borrow of `app.agent_store` is still a borrow
    // of `app`.
    let key = app.agent_device_key();
    // A mutable borrow, even for a read: the `Storage` trait's `set` and
    // `remove` take `&mut self`, so there is no read-only handle to a store.
    if let Some(store) = app.agent_store.as_mut() {
        app.assistant.tasks = linkr_tui::agent::tasks::TaskStore::new(&mut *store).list(&key);
    }
    let saved = app
        .agent_store
        .as_ref()
        .map(linkr_tui::agent::config::load_agent_config)
        .unwrap_or_default();
    // The key is read once and copied, because it is used twice — into the form,
    // so a user can see there is one, and into the validation that decides
    // whether the model is configured at all.
    let mut input = linkr_tui::agent::config::AgentConfigInput::new();
    input.endpoint = saved.endpoint.clone();
    input.model = saved.model.clone();
    input.api_key = saved.api_key.clone();
    // A stored config that no longer validates must not leave the view claiming
    // a model it cannot use: the answer here is the same one the Enter key
    // gives, so a bad endpoint is caught the same way wherever it came from.
    app.assistant.config = linkr_tui::agent::config::validate_agent_config(&input).ok();
    app.assistant.configured = app.assistant.config.is_some();
    app.assistant.endpoint = input.endpoint;
    app.assistant.model = input.model;
    app.assistant.api_key = input.api_key;
    app.assistant.headers = saved.headers;
    // The conversation comes back too, oldest first, which is the order it was
    // written in — `load_session` keeps the last N turns and drops the rest, so
    // a long thread loses its beginning rather than its end.
    app.assistant.display = app
        .agent_store
        .as_mut()
        .and_then(|store| {
            linkr_tui::agent::session::load_session(store, &key).map(|session| session.display)
        })
        .unwrap_or_default();
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
    ui::regions(Rect::new(0, 0, cols, rows), true).centre
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
    // The frame itself, kept where a key handler can read it: keys are handled
    // between two paints, and the settings window keeps its caret inside a
    // body whose height comes from these numbers.
    app.screen = Rect::new(0, 0, cols, rows);
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
/// The interface half, which a build without a renderer has no call to.
#[cfg(feature = "tui")]
fn run(app: &mut App) -> io::Result<()> {
    // Entering raw mode fails on anything that is not a terminal, and the OS
    // error it gives for that is a number — ENXIO, ENOTTY — which is not a
    // sentence a user can act on. A user who pipes this into a file or runs it
    // from a service has done something ordinary and deserves to be told what
    // it was, not handed errno to interpret.
    let mut guard = match TerminalGuard::enter() {
        Ok(guard) => guard,
        Err(why) if !std::io::IsTerminal::is_terminal(&std::io::stdout()) => {
            eprintln!("linkr: the TUI needs an interactive terminal ({why})");
            std::process::exit(2);
        }
        Err(why) => return Err(why),
    };

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

/// The loop that drains the keyboard, on the same runtime the transport built.
#[cfg(feature = "tui")]
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
                // A paste arrives as one event carrying the whole payload, not
                // as the keystrokes that would have produced it. Bracketed
                // paste is enabled below, so the bytes in a paste never reach
                // the device's own line editor as stray keys — which is the
                // entire reason the terminal introduced the mode.
                Event::Paste(text) => {
                    // A paste is a *typing* gesture, so it goes through the
                    // door a keystroke uses: line mode holds it until Enter
                    // exactly as it would have held the characters, and the
                    // echo, the trace and the tx counter all see it. The text
                    // can span newlines, and a target that sees the whole block
                    // is what was asked for — what a paste must not be is
                    // chopped into per-line sends with an Enter guessed after
                    // each one.
                    app.paste(&text);
                }
                // The wheel and the drag. Both need the mouse captured by the
                // guard, and both are one gesture's worth of work: a notch
                // walks the rail one row and the pane three lines, and a drag
                // selects text because the terminal's own selection cannot be
                // read from inside an alternate screen.
                Event::Mouse(mouse) => {
                    if app.quit {
                        return Ok(());
                    }
                    let size = terminal.size().unwrap_or_default();
                    handle_mouse(app, mouse, size.width, size.height);
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
/// The order matters and is the whole policy. The modals are one stack, and
/// the one opened last is the one that answers: the painter draws the palette
/// when both are up, so the palette is asked first to agree with it. A dialog
/// takes the keyboard by closing the palette when it opens — `Ctrl+P` over a
/// dialog is what raises the palette back on top. A *control* chord falls
/// through both, so `Ctrl+Q` still quits and `Ctrl+T` still repaints with an
/// overlay up. Then the global chords, then the sidebar, and only what is left
/// is the device's.
fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    // The two modals are a stack: whichever was opened last is on top, takes
    // the keys, and is the one painted. Dispatch has to agree with the
    // painter, and the painter puts the palette first — so the palette is
    // checked first. Before this, a dialog was checked first while the painter
    // also preferred the dialog, which made `Ctrl+P` look dead with a form
    // open: the palette *did* open, over the dialog, but it was painted under
    // it and fed no keys, so Escape was the only way to find it again.
    if app.palette.is_some() && palette::handle_key(app, key) {
        return app.quit;
    }
    if app.dialog.is_some() && dialogs::handle_key(app, key) {
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
        // Ctrl+Shift+C puts the screen on the terminal's clipboard. Not the
        // plain Ctrl+C, which is the quit chord and has been since the first
        // version: a paste's counterpart must not be next to the key that
        // ends the program.
        (KeyCode::Char('C'), true) if key.modifiers.contains(KeyModifiers::SHIFT) => {
            let notice = app.copy_screen();
            app.push_notice(notice);
            return false;
        }
        (KeyCode::Char('l'), true) => {
            app.clear_terminal();
            return false;
        }
        // Ctrl+Up puts the keyboard on the rail. Up alone is the cursor or
        // scrollback in every focus, so focusing needs a chord; and Esc takes
        // the keyboard back out again, which is why this is a one-way trip.
        //
        // The rail's *visibility* is a different thing and is not bound here:
        // a key that could hide the sidebar from under a user who is trying to
        // focus it is a key that fights them. It is in the palette.
        (KeyCode::Up, true) => {
            app.focus = Focus::Sidebar;
            return false;
        }
        // --- the assistant ---------------------------------------------------
        // Five chords, each landing where the work is: the composer for a
        // question or a paste, the settings window for a setting, and the mode
        // cycle and the clear on the view that shows them. A shortcut that
        // opens the view but leaves the keyboard somewhere else is one that
        // saves a keystroke and costs two — and the settings are a window now,
        // so the configure chord raises it wherever the keyboard happened to
        // be.
        (KeyCode::Char('K'), true) if key.modifiers.contains(KeyModifiers::SHIFT) => {
            views::assistant::focus_composer(app);
            return false;
        }
        (KeyCode::Char('S'), true) if key.modifiers.contains(KeyModifiers::SHIFT) => {
            views::assistant::open_settings(app, views::assistant::AgentField::Endpoint);
            return false;
        }
        (KeyCode::Char('M'), true) if key.modifiers.contains(KeyModifiers::SHIFT) => {
            app.set_view(View::Assistant);
            crate::views::assistant::cycle_mode(app);
            return false;
        }
        (KeyCode::Char('N'), true) if key.modifiers.contains(KeyModifiers::SHIFT) => {
            app.set_view(View::Assistant);
            crate::views::assistant::clear_conversation(app);
            return false;
        }
        // The clipboard, pasted as text. `V` is not a device key the way a
        // letter is, and a paste that reached the backend would be retyped by
        // it as so many keystrokes.
        (KeyCode::Char('V'), true) if key.modifiers.contains(KeyModifiers::SHIFT) => {
            crate::views::assistant::paste_into_focus(app);
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
            // Esc hands the keyboard back to the centre column. It is the only
            // key that means "stop being in a pane" in every other layer of
            // this interface, so it means it here too — and it is consumed
            // here rather than falling through, because Esc is otherwise the
            // escape byte the wire carries and a focused rail must not type to
            // the target by accident.
            KeyCode::Esc => app.focus = Focus::Terminal,
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

    // The assistant is a form too, and the same reasoning applies with more
    // force: it has a composer, and a composer that types a newline into the
    // console on every Enter is not a composer.
    if app.view == View::Assistant && views::assistant::handle_key(app, key) {
        return false;
    }

    // The keyboard is on the pane: everything left is the device's.
    match key.code {
        // PageUp/PageDown walk the pane's own history rather than the device's
        // scroll buffer: without a VT screen there is no device scroll to walk.
        KeyCode::PageUp => app.scroll_terminal(10),
        KeyCode::PageDown => app.scroll_terminal_down(10),
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

/// One mouse gesture, folded into whatever the focused thing wants.
///
/// A wheel notch is one row over the rail and three lines over the pane, never
/// a page and never "whatever the terminal felt like". The wheel turns the way
/// a wheel turns everywhere — away from the user is up, towards the user is
/// down — and the pane resumes following when the wheel brings it home. That
/// is the whole reason the mouse gets captured at all: without it the wheel
/// belongs to the emulator, and an emulator in an alternate screen has nothing
/// to scroll, so it either does nothing or jumps.
///
/// A modal takes the wheel too: while one is up, the pane and the rail behind
/// it are not what the user is scrolling.
fn handle_mouse(app: &mut App, mouse: crossterm::event::MouseEvent,
                cols: u16, rows: u16) {
    use crossterm::event::MouseEventKind;

    // The modal answers the keyboard, and the wheel is a gesture the same way a
    // key is. The box's own paging keys are how it moves; a notch over it does
    // nothing rather than move a view the box is covering — moving the rail's
    // cursor there would be half an action, since the Enter that follows would
    // go to the modal.
    if app.overlay_open() {
        return;
    }

    match mouse.kind {
        // Away from the user: the rail walks up, the pane reads back into its
        // history.
        MouseEventKind::ScrollUp => {
            if app.focus == linkr_tui::app::Focus::Sidebar {
                app.cursor_up();
            } else {
                app.scroll_terminal(3);
            }
        }
        // Towards the user: the rail walks down, the pane walks back towards
        // the tail.
        MouseEventKind::ScrollDown => {
            if app.focus == linkr_tui::app::Focus::Sidebar {
                app.cursor_down();
            } else {
                app.scroll_terminal_down(3);
            }
        }
        _ => {}
    }
    // A wheel over the rail scrolls the rail; over the pane it scrolls the
    // pane; neither is a page. The size is here for the drag that follows, and
    // until then it is what a drag would need rather than a wheel.
    let _ = (cols, rows);
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

    /// The three modes, and the one thing that tells them apart.
    ///
    /// Nothing else in the tool has had three reachable states: a command line
    /// either exited when it was done or never ran anything, and the configure
    /// a board and then watch it case had no spelling at all.
    #[test]
    fn the_three_modes_are_reachable_and_distinguishable() {
        // A plain launch is the terminal.
        assert_eq!(Mode::from(&args(&[]), "linkr"), Mode::Tui);

        // Asking a question is asking for an answer, so the answer is what
        // arrives and then the program leaves.
        assert_eq!(
            Mode::from(&args(&["--query-info"]), "linkr"),
            Mode::Cli,
            "a query asked and left"
        );

        // The script mode says so.
        assert_eq!(Mode::from(&args(&["--no-terminal"]), "linkr"), Mode::Cli);
        // And it wins even with nothing asked for, which is the point: a
        // dial with no questions is still a script run.
        assert_eq!(
            Mode::from(&args(&["--no-terminal"]), "linkr"),
            Mode::Cli,
            "a bare --no-terminal is a script run"
        );

        // The middle mode: leave the terminal until the questions are
        // answered, then take it over on the link that answered them.
        assert_eq!(
            Mode::from(&args(&["--cli-then-tui", "--query-uart"]), "linkr"),
            Mode::CliThenTui
        );
        // And with nothing asked for, it is still the middle mode — that is a
        // way to open the terminal only, which the terminal alone cannot say.
        assert_eq!(
            Mode::from(&args(&["--cli-then-tui"]), "linkr"),
            Mode::CliThenTui
        );
    }

    /// The two flags that name a mode are mutually exclusive, and a user who
    /// typed both has two sessions in mind and would get the wrong one. Saying
    /// so beats guessing which — and saying so exits, so what is checked here
    /// is that neither flag on its own is treated as the conflict.
    #[test]
    fn the_two_mode_flags_are_not_a_conflict_on_their_own() {
        assert_eq!(Mode::from(&args(&["--no-terminal"]), "linkr"), Mode::Cli);
        assert_eq!(
            Mode::from(&args(&["--cli-then-tui"]), "linkr"),
            Mode::CliThenTui
        );
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

    /// F2–F5 select the views; any other function key changes nothing.
    #[test]
    fn the_view_keys_select_views() {
        let mut app = App::default();
        assert!(!handle_key(
            &mut app,
            KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE)
        ));
        assert_eq!(app.view, View::Network);
        // F5 is the last view, so F6 and F7 are unbound and must leave it be.
        handle_key(&mut app, KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE));
        assert_eq!(app.view, View::Assistant);
        handle_key(&mut app, KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE));
        assert_eq!(
            app.view,
            View::Assistant,
            "an unbound F-key must not move the view"
        );
        // F1 is help, not a view: it opens the overlay and leaves the view be.
        handle_key(&mut app, KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
        assert_eq!(app.view, View::Assistant, "F1 is help, not a view");
        assert!(
            matches!(app.dialog, Some(linkr_tui::dialogs::Dialog::Help(0))),
            "F1 opened the help: {:?}",
            app.dialog
        );
    }

    /// Ctrl+Shift+K lands on the composer; Ctrl+Shift+S raises the settings
    /// window on the endpoint. Both are doors into the assistant, and a door
    /// that opened the view but left the keyboard elsewhere would save a
    /// keystroke and cost two.
    #[test]
    fn the_assistant_chords_land_where_the_work_is() {
        let mut app = App::default();
        handle_key(
            &mut app,
            KeyEvent::new(
                KeyCode::Char('K'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
        );
        assert_eq!(app.view, View::Assistant);
        assert_eq!(
            app.assistant.focus,
            linkr_tui::views::assistant::AgentField::Composer
        );
        assert!(app.dialog.is_none(), "the composer is not a window");

        handle_key(
            &mut app,
            KeyEvent::new(
                KeyCode::Char('S'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
        );
        assert!(
            matches!(app.dialog, Some(linkr_tui::dialogs::Dialog::Settings(_))),
            "Ctrl+Shift+S did not raise the window: {:?}",
            app.dialog
        );
        assert_eq!(
            app.assistant.focus,
            linkr_tui::views::assistant::AgentField::Endpoint,
            "the configure chord lands on the field the notice names"
        );

        // Escape from the window hands the caret back to the composer, or the
        // next keystroke would be typed into a row nothing is painting.
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.dialog.is_none(), "Escape did not close the window");
        assert_eq!(
            app.assistant.focus,
            linkr_tui::views::assistant::AgentField::Composer
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
            ("wifi", OverlayPreview::WifiSsid),
            ("webdav", OverlayPreview::WebdavUrl),
            ("settings", OverlayPreview::Settings),
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
            (OverlayPreview::WifiSsid, "加入 WiFi 网络"),
            (OverlayPreview::WebdavUrl, "日志上传目标"),
            (OverlayPreview::Settings, "助手设置"),
        ] {
            let mut app = App::default();
            overlay.raise(&mut app);
            let frame = linkr_tui::ui::render_to_text(&app, 100, 30);
            assert!(frame.contains(needle), "{overlay:?}:\n{frame}");
        }
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

    /// A palette opened over a dialog is *live*: the keys reach it and it can
    /// be closed again. It used to open behind the dialog and take no keys, so
    /// the chord read as dead until Escape cleared the box in front of it.
    #[test]
    fn control_p_wakes_the_palette_over_a_dialog() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        // The form's own modal: a prompt is what a network view raises, and
        // the state the report came from.
        linkr_tui::dialogs::prompt_wifi_ssid(&mut app);
        assert!(app.dialog.is_some(), "the prompt is up");

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        );
        assert!(app.palette.is_some(), "Ctrl+P woke the palette");
        assert!(app.dialog.is_some(), "the prompt is still underneath");

        // The keys are the palette's, not the prompt's: a letter is a query
        // character and the prompt's field does not move.
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
        );
        assert_eq!(
            app.palette.as_ref().map(|p| p.query.as_str()),
            Some("z"),
            "the palette took the key"
        );

        // Escape closes the top of the stack, and the prompt comes back.
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.palette.is_none(), "Escape closed the palette");
        assert!(app.dialog.is_some(), "and the prompt is back");

        // Ctrl+P again, then Ctrl+P to close it: the chord is a toggle even
        // with a dialog up.
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        );
        assert!(app.palette.is_some());
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        );
        assert!(app.palette.is_none(), "the chord put it away");
        assert!(app.dialog.is_some(), "the prompt never moved");
    }

    /// A dialog raised while the palette is open takes the keyboard: the two
    /// are a stack, and the one opened last is the one answering.
    #[test]
    fn a_dialog_raised_over_the_palette_takes_the_keys() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        );
        assert!(app.palette.is_some());

        // F1 is the help, and it is legal with the palette up.
        handle_key(&mut app, KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
        assert!(app.dialog.is_some(), "the help is up");
        assert!(app.palette.is_none(), "and gave up the keyboard");

        // A letter now belongs to the dialog, not to a palette query.
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        assert_eq!(app.tx, 0, "the dialog swallowed the keystroke");
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
        // Switching device now *looks* for one. It used to answer "no other
        // device to switch to" without ever having asked the radio — the one
        // action in the interface that answered a question it had not posed.
        // The row opens the scan, so what it leaves behind is a scan running.
        assert!(
            app.ble_scan.is_running(),
            "switch device did not start listening"
        );
        assert!(
            app.dialog.is_some(),
            "the scan's list is not open"
        );
        assert!(
            app.log.last().map(|n| n.text.as_str()) != Some("No other device to switch to yet."),
            "the old warning is gone"
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

    /// The wheel turns the way a wheel turns in both focuses: away from the
    /// user walks up — a row in the rail, three lines back into the pane — and
    /// towards the user walks back down, where reaching the tail resumes
    /// following.
    #[test]
    fn the_wheel_walks_the_rail_and_the_pane_the_way_wheels_walk() {
        use crossterm::event::{MouseEvent, MouseEventKind};

        fn wheel(app: &mut App, kind: MouseEventKind) {
            handle_mouse(
                app,
                MouseEvent {
                    kind,
                    column: 10,
                    row: 10,
                    modifiers: KeyModifiers::NONE,
                },
                100,
                30,
            );
        }

        // The rail: one row per notch.
        let mut app = App {
            focus: Focus::Sidebar,
            sidebar_cursor: 1,
            ..App::default()
        };
        wheel(&mut app, MouseEventKind::ScrollDown);
        assert_eq!(app.sidebar_cursor, 2);
        wheel(&mut app, MouseEventKind::ScrollUp);
        assert_eq!(app.sidebar_cursor, 1);

        // The pane: away from the user reads back, towards the user walks home.
        let mut app = App::default();
        app.terminal.push_text("one\ntwo\nthree\n");
        assert!(app.terminal.autoscroll);
        wheel(&mut app, MouseEventKind::ScrollUp);
        wheel(&mut app, MouseEventKind::ScrollUp);
        assert_eq!(app.terminal.scroll, 6, "two notches, three lines each");
        assert!(!app.terminal.autoscroll, "reading back stops following");
        wheel(&mut app, MouseEventKind::ScrollDown);
        assert_eq!(app.terminal.scroll, 3);
        wheel(&mut app, MouseEventKind::ScrollDown);
        assert_eq!(app.terminal.scroll, 0);
        assert!(app.terminal.autoscroll, "the tail is where following resumes");
        wheel(&mut app, MouseEventKind::ScrollDown);
        assert_eq!(app.terminal.scroll, 0, "the walk stops at the tail");
    }

    /// A modal answers the mouse the way it answers the keyboard: the pane and
    /// the rail underneath it do not move while the user is answering the box.
    #[test]
    fn the_wheel_leaves_the_background_alone_under_a_modal() {
        use crossterm::event::{MouseEvent, MouseEventKind};

        fn wheel(app: &mut App, kind: MouseEventKind) {
            handle_mouse(
                app,
                MouseEvent {
                    kind,
                    column: 10,
                    row: 10,
                    modifiers: KeyModifiers::NONE,
                },
                100,
                30,
            );
        }

        // The rail's cursor under a dialog, and under the palette.
        let mut app = App {
            focus: Focus::Sidebar,
            sidebar_cursor: 3,
            ..App::default()
        };
        linkr_tui::dialogs::open_help(&mut app);
        wheel(&mut app, MouseEventKind::ScrollUp);
        assert_eq!(app.sidebar_cursor, 3, "the rail moved under a dialog");
        app.dialog = None;
        linkr_tui::palette::open(&mut app);
        wheel(&mut app, MouseEventKind::ScrollUp);
        assert_eq!(app.sidebar_cursor, 3, "the rail moved under the palette");

        // The pane, which was following its tail: still following, still on it.
        let mut app = App::default();
        app.terminal.push_text("one\ntwo\nthree\n");
        linkr_tui::dialogs::open_help(&mut app);
        wheel(&mut app, MouseEventKind::ScrollUp);
        assert_eq!(app.terminal.scroll, 0, "the pane read back under a modal");
        assert!(
            app.terminal.autoscroll,
            "the pane stopped following under a modal"
        );
        // The same notch without the box in the way reads back, which is what
        // makes the two assertions above mean something.
        app.dialog = None;
        wheel(&mut app, MouseEventKind::ScrollUp);
        assert_eq!(app.terminal.scroll, 3);
    }
}
