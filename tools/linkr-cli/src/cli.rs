//! Command-line surface. Flag parity with `tools/linkr_ble_terminal.py` is a
//! hard requirement (specs/PYTHON_CLI_SPEC.md section 9); new flags are listed
//! in CONTRACTS.md.
//!
//! Parsing happens in two layers: clap builds the picture (names, defaults,
//! help text, subcommands) and a second pass validates what Python handed to
//! argparse's `type=` callables, so the messages stay identical
//! (`linkr: error: argument --timeout: value must be greater than zero`).

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::time::Duration;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use tokio::sync::{broadcast, oneshot};

use crate::agent::accessory::{
    DIAGNOSTICS_COMMAND, UART_QUERY_COMMAND, WEBDAV_QUERY_COMMAND, WIFI_QUERY_COMMAND,
};
use crate::event::{ConnectionState, CoreEvent, NoticeLevel};
use crate::protocol::mgmt::{
    MGMT_CAP_ASYNC_EVENTS, MGMT_CAP_WEBDAV, MGMT_CAP_WIFI, WIFI_OPERATION_TIMEOUT_SECS,
    WIFI_SCAN_TIMEOUT_SECS,
};
use crate::protocol::validate::{
    describe_escape, match_device, normalize_name_prefix, normalize_uart_spec, parse_escape,
    python_repr_bytes, resolve_wifi_credentials, translate_enter,
};
use crate::protocol::{json_record, plain_line, MgmtReply};
use crate::session::{CoreBus, SessionHandle, SessionOptions, SessionSetup, TransportSpec};
use crate::term;
use crate::transport::lan::{validate_token, EMPTY_HOST_ERROR};
use crate::transport::{ble, DiscoveredDevice};
use crate::tui::{PendingConnect, TuiContext};

/// Default `--name`; the Python parser's `DEFAULT_NAME`.
pub const DEFAULT_NAME: &str = "Linkr BLE UART";
const BIN_NAME: &str = "linkr";

const EXIT_OK: i32 = 0;
const EXIT_ERROR: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_DISCONNECTED: i32 = 3;
const EXIT_INTERRUPTED: i32 = 130;

/// Shells both completion entry points accept: Python's three plus
/// PowerShell (CONTRACTS section 4).
const COMPLETION_SHELLS: [&str; 4] = ["bash", "fish", "zsh", "powershell"];
const ENTER_MODES: [&str; 4] = ["raw", "cr", "lf", "crlf"];

// ---------------------------------------------------------------------------
// Output helpers (PYTHON_CLI_SPEC 1.4: exact prefixes)
// ---------------------------------------------------------------------------

static QUIET: AtomicBool = AtomicBool::new(false);
static YES: AtomicBool = AtomicBool::new(false);

/// Set the `--quiet` gate; called once after parsing, before any output.
pub fn set_quiet(value: bool) {
    QUIET.store(value, Ordering::Relaxed);
}

/// Whether progress messages are suppressed right now.
pub fn quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

/// Set the `--yes` flag the assistant/TUI approval broker reads.
pub fn set_yes(value: bool) {
    YES.store(value, Ordering::Relaxed);
}

/// Whether the user pre-approved assistant commands.
pub fn yes() -> bool {
    YES.load(Ordering::Relaxed)
}

/// `linkr: {msg}` on stderr, suppressed by `--quiet`.
pub fn info(msg: impl AsRef<str>) {
    if !quiet() {
        diagnostic(NoticeLevel::Info, format!("linkr: {}", msg.as_ref()));
    }
}

/// `linkr: warning: {msg}` on stderr, never suppressed.
pub fn warn(msg: impl AsRef<str>) {
    diagnostic(
        NoticeLevel::Warn,
        format!("linkr: warning: {}", msg.as_ref()),
    );
}

/// `linkr: error: {msg}` on stderr, never suppressed.
pub fn error(msg: impl AsRef<str>) {
    diagnostic(
        NoticeLevel::Error,
        format!("linkr: error: {}", msg.as_ref()),
    );
}

/// While the TUI owns the screen, `linkr:` diagnostics must not be printed:
/// the alternate screen is painted by ratatui, which diffs its own buffer and
/// never repaints cells a stray `eprintln!` overwrote, so the text would sit
/// on top of the interface. The TUI installs a sink and drains it into its
/// notice log instead. `None` (every other mode) keeps stderr, byte for byte.
static DIAGNOSTIC_SINK: Mutex<Option<Sender<(NoticeLevel, String)>>> = Mutex::new(None);

/// Install (or clear, with `None`) the TUI diagnostic sink.
pub fn set_diagnostic_sink(sink: Option<Sender<(NoticeLevel, String)>>) {
    *DIAGNOSTIC_SINK.lock().expect("diagnostic sink poisoned") = sink;
}

fn diagnostic(level: NoticeLevel, line: String) {
    let slot = DIAGNOSTIC_SINK.lock().expect("diagnostic sink poisoned");
    if let Some(tx) = slot.as_ref() {
        // A full or dropped channel must never take the caller down.
        let _ = tx.send((level, line));
        return;
    }
    drop(slot);
    eprintln!("{line}");
}

/// One line of output the renderer wants on the screen. Keeping rendering
/// pure (instead of printing) makes the message catalog unit-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Output {
    /// Written to stdout as-is: terminal payload bytes, `--json` records.
    Stdout(Vec<u8>),
    /// One line on stderr, newline appended by [`emit`].
    Stderr(String),
}

/// What a rendered event means for whoever is pumping the bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// Keep pumping.
    Continue,
    /// The transport went away (a terminal session exits 3).
    Disconnected,
    /// A UART write failed; Python would raise and exit 1.
    Failed,
}

fn emit(outputs: &[Output]) {
    let mut stdout = std::io::stdout();
    let mut flush = false;
    for output in outputs {
        match output {
            Output::Stdout(bytes) => {
                let _ = stdout.write_all(bytes);
                flush = true;
            }
            Output::Stderr(line) => eprintln!("{line}"),
        }
    }
    if flush {
        let _ = stdout.flush();
    }
}

/// Byte traces are Python's `stderr()` lines: no `linkr: ` prefix, not
/// suppressed by `--quiet` (PYTHON_CLI_SPEC 12 labels them "raw stderr").
fn is_raw_trace(text: &str) -> bool {
    text.starts_with("RX ")
        || text.starts_with("TX ")
        || text.starts_with("UART TX ")
        || text.starts_with("MGMT TX ")
}

fn render_notice(quiet: bool, level: NoticeLevel, text: &str) -> Vec<Output> {
    if is_raw_trace(text) {
        return vec![Output::Stderr(text.to_string())];
    }
    match level {
        NoticeLevel::Info if quiet => Vec::new(),
        NoticeLevel::Info => vec![Output::Stderr(format!("linkr: {text}"))],
        NoticeLevel::Warn => vec![Output::Stderr(format!("linkr: warning: {text}"))],
        NoticeLevel::Error => vec![Output::Stderr(format!("linkr: error: {text}"))],
    }
}

/// Render one bus event (PYTHON_CLI_SPEC 3.3 / 6.4 output rules).
fn render_event(json: bool, quiet: bool, event: CoreEvent) -> (Flow, Vec<Output>) {
    match event {
        CoreEvent::UartRx(bytes) => (Flow::Continue, vec![Output::Stdout(bytes)]),
        CoreEvent::Notice { level, text } => (Flow::Continue, render_notice(quiet, level, &text)),
        CoreEvent::MgmtMessage {
            kind,
            id,
            ok,
            lines,
            command,
            final_: _,
        } => {
            // `@s?` answers with the bridge's LAN access token; display and
            // export are redacted (web `redactSecrets`), parsing reads the
            // raw line.
            let lines: Vec<String> = lines
                .iter()
                .map(|line| crate::lan_token_store::redact_secrets(line))
                .collect();
            if json {
                let record = json_record(kind, id, ok, &lines, command.as_deref());
                let mut bytes = record.into_bytes();
                bytes.push(b'\n');
                (Flow::Continue, vec![Output::Stdout(bytes)])
            } else {
                let outputs = lines
                    .iter()
                    .map(|line| Output::Stderr(plain_line(kind, id, line)))
                    .collect();
                (Flow::Continue, outputs)
            }
        }
        CoreEvent::Connection { state, detail: _ } => match state {
            ConnectionState::Disconnected => (Flow::Disconnected, Vec::new()),
            // A failed connection is fatal: Python's `main()` turns the
            // exception into `linkr: error: ...` and exit 1.
            ConnectionState::Failed => (Flow::Failed, Vec::new()),
            _ => (Flow::Continue, Vec::new()),
        },
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
    name = BIN_NAME,
    version,
    about = "Terminal over BLE Nordic UART Service for Linkr Bee bridge",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[arg(
        long,
        global = true,
        default_value = DEFAULT_NAME,
        value_name = "NAME",
        help = "BLE device name or prefix; default matches Linkr BLE UART*"
    )]
    pub name: String,

    #[arg(
        long,
        global = true,
        value_name = "ADDRESS",
        help = "BLE address/UUID; skips name scan"
    )]
    pub address: Option<String>,

    #[arg(
        long,
        global = true,
        help = "list nearby BLE devices (all of them, named or not)"
    )]
    pub scan: bool,

    #[arg(
        long,
        global = true,
        default_value = "8.0",
        allow_negative_numbers = true,
        value_name = "TIMEOUT",
        help = "scan timeout seconds"
    )]
    pub timeout: String,

    #[arg(
        long,
        global = true,
        help = "send @i? device diagnostics before terminal"
    )]
    pub query_info: bool,

    #[arg(long, global = true, help = "send @u? before terminal")]
    pub query_uart: bool,

    #[arg(
        long,
        global = true,
        value_name = "SPEC",
        help = "set UART as baud,data,parity,stop,flow (baud 300-3000000, data 5-8, parity n/o/e, stop 1/2, flow n/rtscts)"
    )]
    pub uart: Option<String>,

    #[arg(
        long,
        global = true,
        value_name = "SSID[,PASSWORD]",
        help = "connect ESP32 to WiFi; prefer ssid alone with --wifi-key-file so the password stays out of argv"
    )]
    pub wifi: Option<String>,

    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "read the WiFi password from the first line of PATH"
    )]
    pub wifi_key_file: Option<PathBuf>,

    #[arg(long, global = true, help = "forget saved WiFi")]
    pub wifi_off: bool,

    #[arg(long, global = true, help = "send @w? before terminal")]
    pub query_wifi: bool,

    #[arg(long, global = true, help = "scan nearby 2.4 GHz WiFi networks")]
    pub wifi_scan: bool,

    #[arg(
        long,
        global = true,
        value_name = "URL",
        help = "set anonymous HTTP WebDAV upload URL"
    )]
    pub webdav: Option<String>,

    #[arg(long, global = true, help = "disable WebDAV upload")]
    pub webdav_off: bool,

    #[arg(long, global = true, help = "send @d? before terminal")]
    pub query_webdav: bool,

    #[arg(
        long,
        global = true,
        help = "request OS bonding (hold Bee GPIO1 low); macOS pairs on encrypted reads"
    )]
    pub pair: bool,

    #[arg(
        long,
        global = true,
        value_name = "STR",
        num_args = 0..=1,
        default_missing_value = "A",
        help = "send payload and require the same bytes back"
    )]
    pub loopback_test: Option<String>,

    #[arg(
        long,
        global = true,
        default_value = "3.0",
        allow_negative_numbers = true,
        value_name = "TIMEOUT",
        help = "seconds to wait for --loopback-test echo"
    )]
    pub loopback_timeout: String,

    #[arg(long, global = true, help = "connect, run commands, exit")]
    pub no_terminal: bool,

    #[arg(
        long,
        global = true,
        help = "write management responses/events to stdout as JSON lines (sensitive command text stays redacted)"
    )]
    pub json: bool,

    #[arg(
        long,
        global = true,
        help = "suppress progress messages; errors and results still print"
    )]
    pub quiet: bool,

    #[arg(
        long,
        global = true,
        value_name = "{bash,fish,zsh,powershell}",
        help = "print a shell completion script and exit"
    )]
    pub print_completion: Option<String>,

    #[arg(
        long,
        global = true,
        default_value = "0",
        allow_negative_numbers = true,
        value_name = "BYTES",
        help = "max bytes per BLE RX write; default auto"
    )]
    pub ble_write_size: String,

    #[arg(
        long,
        global = true,
        help = "use GATT write-with-response (NUS fallback only; Reliable UART always writes with-response)"
    )]
    pub write_response: bool,

    #[arg(
        long,
        global = true,
        default_value = "5.0",
        allow_negative_numbers = true,
        value_name = "MILLISECONDS",
        help = "delay between BLE write chunks (NUS fallback only; never applied on Reliable UART)"
    )]
    pub write_delay_ms: String,

    #[arg(
        long,
        global = true,
        default_value = "raw",
        value_name = "{raw,cr,lf,crlf}",
        help = "translate Enter key bytes before BLE write"
    )]
    pub enter: String,

    #[arg(long, global = true, help = "echo typed bytes locally")]
    pub local_echo: bool,

    #[arg(
        long,
        global = true,
        help = "do not use raw terminal; send one visible line at a time"
    )]
    pub line_mode: bool,

    #[arg(long, global = true, help = "print BLE TX/RX byte traces to stderr")]
    pub debug_io: bool,

    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "append raw BLE RX bytes to a file"
    )]
    pub log_file: Option<PathBuf>,

    #[arg(
        long,
        global = true,
        default_value = "^]",
        value_name = "TYPE",
        help = "terminal escape byte, default ^]"
    )]
    pub escape: String,

    // -- The additions below are listed in CONTRACTS.md section 4. --------
    #[arg(
        long,
        global = true,
        value_name = "HOST[:PORT]",
        help = "connect through the LAN WebSocket bridge instead of BLE"
    )]
    pub lan: Option<String>,

    #[arg(
        long,
        global = true,
        value_name = "HEX",
        help = "LAN bridge access token, 32 lowercase hex characters \
                (precedence: this flag, --lan-token-file, LINKR_LAN_TOKEN, \
                then the token captured during a BLE session)"
    )]
    pub lan_token: Option<String>,

    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "read the LAN access token from PATH"
    )]
    pub lan_token_file: Option<PathBuf>,

    #[arg(
        long,
        global = true,
        help = "hand the session to the TUI after connecting"
    )]
    pub tui: bool,

    #[arg(
        long,
        global = true,
        help = "let the assistant approve its own commands (TUI approval broker)"
    )]
    pub yes: bool,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Open the TUI after connecting (same as --tui).
    Tui,
    /// List nearby BLE devices (same as --scan).
    Scan,
    /// Print a shell completion script and exit.
    Completion {
        /// bash, fish, zsh or powershell.
        shell: String,
    },
}

/// Outcome of the parse layer: clap already printed whatever was needed.
enum Parsed {
    /// Help/version/completion/usage error: nothing left to run.
    Done(i32),
    /// A real invocation. The parsed command tree is boxed so this two-variant
    /// enum does not drag its largest variant through every match.
    Run(Box<Cli>),
}

fn parse_args<I, T>(args: I) -> Parsed
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let mut cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(parse_error) => {
            let code = parse_error.exit_code();
            let _ = parse_error.print();
            return Parsed::Done(code);
        }
    };
    let subcommand = cli.command.take();
    if let Some(shell) = cli.print_completion.take() {
        return print_completion(&shell, "--print-completion");
    }
    match subcommand {
        Some(Command::Completion { shell }) => print_completion(&shell, "completion"),
        Some(Command::Tui) => {
            cli.tui = true;
            Parsed::Run(Box::new(cli))
        }
        Some(Command::Scan) => {
            cli.scan = true;
            Parsed::Run(Box::new(cli))
        }
        None => Parsed::Run(Box::new(cli)),
    }
}

fn print_completion(shell: &str, flag: &str) -> Parsed {
    match completion_script(shell) {
        Ok(script) => {
            print!("{script}");
            let _ = std::io::stdout().flush();
            Parsed::Done(EXIT_OK)
        }
        Err(message) => Parsed::Done(fail_usage(&format!("argument {flag}: {message}"))),
    }
}

fn choice_list(values: [&str; 4]) -> String {
    values
        .iter()
        .map(|value| format!("'{value}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The completion script for `shell`. Deterministic and radio-free, like
/// Python's `--print-completion` (PYTHON_CLI_SPEC section 11), but generated
/// from the clap parser instead of hand-rolled.
pub fn completion_script(shell: &str) -> Result<String, String> {
    let generator = match shell {
        "bash" => Shell::Bash,
        "fish" => Shell::Fish,
        "zsh" => Shell::Zsh,
        "powershell" => Shell::PowerShell,
        other => {
            return Err(format!(
                "invalid choice: '{other}' (choose from {})",
                choice_list(COMPLETION_SHELLS)
            ))
        }
    };
    let mut command = Cli::command();
    let mut buffer: Vec<u8> = Vec::new();
    generate(generator, &mut command, BIN_NAME, &mut buffer);
    let mut script = String::from_utf8_lossy(&buffer).into_owned();
    if !script.ends_with('\n') {
        script.push('\n');
    }
    // Python `print(script)` with a script that already ends in "\n".
    script.push('\n');
    Ok(script)
}

/// argparse-style usage error: the usage line, then `linkr: error: {message}`.
fn fail_usage(message: &str) -> i32 {
    let mut command = Cli::command();
    let usage = command.render_usage().to_string();
    if !usage.is_empty() {
        eprintln!("{usage}");
    }
    error(message);
    EXIT_USAGE
}

// ---------------------------------------------------------------------------
// Validation (PYTHON_CLI_SPEC 9.1: exact messages, exit 2)
// ---------------------------------------------------------------------------

/// Everything the parser accepted, in the types the rest of the run needs.
#[derive(Debug)]
struct Validated {
    timeout: f64,
    loopback_timeout: f64,
    ble_write_size: usize,
    escape: u8,
    uart: Option<String>,
}

fn positive_float(flag: &str, raw: &str) -> Result<f64, String> {
    let parsed = raw
        .trim()
        .parse::<f64>()
        .map_err(|_| format!("argument --{flag}: invalid positive_float value: '{raw}'"))?;
    if !parsed.is_finite() || parsed <= 0.0 {
        return Err(format!(
            "argument --{flag}: value must be greater than zero"
        ));
    }
    Ok(parsed)
}

fn nonnegative_float(flag: &str, raw: &str) -> Result<f64, String> {
    let parsed = raw
        .trim()
        .parse::<f64>()
        .map_err(|_| format!("argument --{flag}: invalid nonnegative_float value: '{raw}'"))?;
    if !parsed.is_finite() || parsed < 0.0 {
        return Err(format!("argument --{flag}: value must not be negative"));
    }
    Ok(parsed)
}

fn ble_write_size(flag: &str, raw: &str) -> Result<usize, String> {
    let parsed = raw
        .trim()
        .parse::<i64>()
        .map_err(|_| format!("argument --{flag}: invalid ble_write_size value: '{raw}'"))?;
    if !(0..=244).contains(&parsed) {
        return Err(format!(
            "argument --{flag}: BLE write size must be between 0 and 244"
        ));
    }
    Ok(parsed as usize)
}

/// Second validation pass: everything argparse did through `type=` callables
/// and `choices=`, in the inventory order of PYTHON_CLI_SPEC 9.3.
fn validate(cli: &Cli) -> Result<Validated, String> {
    let timeout = positive_float("timeout", &cli.timeout)?;
    let loopback_timeout = positive_float("loopback-timeout", &cli.loopback_timeout)?;
    let _write_delay_ms = nonnegative_float("write-delay-ms", &cli.write_delay_ms)?;
    let ble_write_size = ble_write_size("ble-write-size", &cli.ble_write_size)?;
    if !ENTER_MODES.contains(&cli.enter.as_str()) {
        return Err(format!(
            "argument --enter: invalid choice: '{}' (choose from {})",
            cli.enter,
            choice_list(ENTER_MODES)
        ));
    }
    if let Some(host) = cli.lan.as_deref() {
        if host.trim().is_empty() {
            return Err(format!("argument --lan: {EMPTY_HOST_ERROR}"));
        }
    }
    let escape =
        parse_escape(&cli.escape).map_err(|message| format!("argument --escape: {message}"))?;
    let uart = match cli.uart.as_deref() {
        Some(spec) => Some(
            normalize_uart_spec(spec).map_err(|message| format!("argument --uart: {message}"))?,
        ),
        None => None,
    };
    Ok(Validated {
        timeout,
        loopback_timeout,
        ble_write_size,
        escape,
        uart,
    })
}

/// True when the invocation asks for more than opening a terminal; the exact
/// Python list (PYTHON_CLI_SPEC 8.2).
fn has_control_action(cli: &Cli) -> bool {
    cli.query_info
        || cli.query_uart
        || cli.uart.is_some()
        || cli.wifi.is_some()
        || cli.wifi_scan
        || cli.wifi_off
        || cli.query_wifi
        || cli.webdav.is_some()
        || cli.webdav_off
        || cli.query_webdav
        || cli.loopback_test.is_some()
        || cli.pair
}

/// `--scan` prints the table and stops when nothing else was asked for. The
/// LAN bridge and the TUI are additions to the Python rule.
fn exit_after_scan(cli: &Cli) -> bool {
    cli.address.is_none() && cli.lan.is_none() && !cli.tui && !has_control_action(cli)
}

/// PYTHON_CLI_SPEC 3.1: reject a run whose features the device does not
/// advertise before the first command goes out.
fn precheck_capabilities(cli: &Cli, capabilities: u32) -> Result<(), String> {
    let wifi_actions = cli.wifi.is_some() || cli.wifi_scan || cli.wifi_off || cli.query_wifi;
    if wifi_actions && capabilities & MGMT_CAP_WIFI == 0 {
        return Err("device does not advertise WiFi support".to_string());
    }
    let async_wifi_actions = cli.wifi.is_some() || cli.wifi_scan || cli.wifi_off;
    if async_wifi_actions && capabilities & MGMT_CAP_ASYNC_EVENTS == 0 {
        return Err("device does not advertise async event support".to_string());
    }
    let webdav_actions = cli.webdav.is_some() || cli.webdav_off || cli.query_webdav;
    if webdav_actions && capabilities & MGMT_CAP_WEBDAV == 0 {
        return Err("device does not advertise WebDAV support".to_string());
    }
    Ok(())
}

/// The management commands in the fixed execution order of PYTHON_CLI_SPEC
/// 3.2, paired with their wait-for-FINAL window.
fn build_commands(
    cli: &Cli,
    uart: Option<&str>,
    wifi: Option<String>,
) -> Vec<(String, Option<Duration>)> {
    let wifi_final = Some(Duration::from_secs_f64(WIFI_OPERATION_TIMEOUT_SECS));
    let scan_final = Some(Duration::from_secs_f64(WIFI_SCAN_TIMEOUT_SECS));
    let mut commands = Vec::new();
    if cli.query_info {
        commands.push((DIAGNOSTICS_COMMAND.to_string(), None));
    }
    if let Some(uart) = uart {
        commands.push((format!("@u={uart}"), None));
    }
    if cli.query_uart {
        commands.push((UART_QUERY_COMMAND.to_string(), None));
    }
    if let Some(wifi) = wifi {
        commands.push((wifi, wifi_final));
    }
    if cli.wifi_off {
        commands.push(("@w off".to_string(), wifi_final));
    }
    if cli.query_wifi {
        commands.push((WIFI_QUERY_COMMAND.to_string(), None));
    }
    if cli.wifi_scan {
        commands.push(("@w scan".to_string(), scan_final));
    }
    if let Some(webdav) = cli.webdav.as_deref() {
        commands.push((format!("@d={webdav}"), None));
    }
    if cli.webdav_off {
        commands.push(("@d off".to_string(), None));
    }
    if cli.query_webdav {
        commands.push((WEBDAV_QUERY_COMMAND.to_string(), None));
    }
    commands
}

// ---------------------------------------------------------------------------
// Credential resolution (before any radio work)
// ---------------------------------------------------------------------------

/// `--wifi` → the `@w=` command bytes, or an exit code (PYTHON_CLI_SPEC 9.2:
/// resolution errors are exit 2, Ctrl-C is 130).
fn resolve_wifi_command(cli: &Cli) -> Result<Option<String>, i32> {
    let Some(spec) = cli.wifi.as_deref() else {
        return Ok(None);
    };
    let env = std::env::var("LINKR_WIFI_PASSWORD").ok();
    let key_file = cli.wifi_key_file.clone();
    let mut interrupted = false;
    let mut prompt = |prompt_text: &str| -> String {
        match term::read_hidden(prompt_text) {
            Ok(secret) => secret,
            Err(_) => {
                interrupted = true;
                String::new()
            }
        }
    };
    let prompt_slot: Option<&mut dyn FnMut(&str) -> String> = if term::stdin_is_tty() {
        Some(&mut prompt)
    } else {
        None
    };
    let resolved = resolve_wifi_credentials(spec, key_file.as_deref(), env.as_deref(), prompt_slot);
    match resolved {
        Ok((ssid, password)) => Ok(Some(format!("@w={ssid},{password}"))),
        Err(message) => {
            if interrupted {
                Err(EXIT_INTERRUPTED)
            } else {
                error(message);
                Err(EXIT_USAGE)
            }
        }
    }
}

/// Where a `--lan` token came from (which flag the error names).
enum TokenSource {
    Flag,
    File,
    Env,
}

/// The store's token for the host `--lan` names — the last of the four
/// sources [`resolve_lan_token`] consults, and the only one that is not an
/// input on the command line. Kept out of the run itself so the lookup reads
/// a store the test owns instead of the user's config directory.
fn stored_lan_token(cli: &Cli, store: &crate::lan_token_store::TokenStore) -> Option<String> {
    cli.lan
        .as_deref()
        .and_then(|host| store.select_host(host).map(str::to_string))
}

/// `--lan-token` → `--lan-token-file` → `LINKR_LAN_TOKEN` → `stored`, the
/// token a BLE session captured for this host (the web dials with whatever
/// `lanTokens` already put in its field), validated before anything dials
/// out. `stored` is the store's own: `select_host` only hands out what
/// [`crate::lan_token_store::TokenStore::capture`] accepted, so it is a
/// fallback, never a usage error.
fn resolve_lan_token(cli: &Cli, stored: Option<String>) -> Result<Option<String>, i32> {
    let (raw, source) = if let Some(token) = cli.lan_token.as_deref() {
        (Some(token.to_string()), TokenSource::Flag)
    } else if let Some(path) = cli.lan_token_file.as_deref() {
        match std::fs::read_to_string(path) {
            Ok(text) => (Some(text.trim().to_string()), TokenSource::File),
            Err(failure) => {
                error(format!("cannot read --lan-token-file: {failure}"));
                return Err(EXIT_USAGE);
            }
        }
    } else {
        match std::env::var("LINKR_LAN_TOKEN")
            .ok()
            .map(|value| value.trim().to_string())
        {
            Some(token) => (Some(token), TokenSource::Env),
            // Nothing was asked for: the store is the last word. An empty
            // stored token means the bridge runs with authentication off, and
            // that dials as "no token supplied", like the empty file does.
            None => return Ok(stored.filter(|token| !token.is_empty())),
        }
    };
    let Some(token) = raw else {
        return Ok(None);
    };
    if token.is_empty() && !matches!(source, TokenSource::Flag) {
        return Ok(None);
    }
    if let Err(message) = validate_token(&token) {
        return match source {
            TokenSource::Flag => Err(fail_usage(&format!("argument --lan-token: {message}"))),
            TokenSource::File => Err(fail_usage(&format!("argument --lan-token-file: {message}"))),
            TokenSource::Env => {
                error(message);
                Err(EXIT_USAGE)
            }
        };
    }
    Ok(Some(token))
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Entry point: parse arguments, dispatch, return the process exit code.
pub fn run() -> i32 {
    match parse_args(std::env::args_os()) {
        Parsed::Done(code) => code,
        Parsed::Run(cli) => execute(*cli),
    }
}

/// `true` when the process can walk straight into the TUI and dial later
/// (A5). Anything that has to produce output or mutate the device over a live
/// session — `--query-*`, `@u`/`@w`/`@d` mutations, `--loopback-test`,
/// `--no-terminal` — keeps connecting up front, like Python.
fn tui_defers_connect(cli: &Cli, commands: &[(String, Option<Duration>)]) -> bool {
    cli.tui && !cli.no_terminal && cli.loopback_test.is_none() && commands.is_empty()
}

fn execute(cli: Cli) -> i32 {
    set_quiet(cli.quiet);
    set_yes(cli.yes);

    let validated = match validate(&cli) {
        Ok(validated) => validated,
        Err(message) => return fail_usage(&message),
    };
    // WiFi credentials come first: a bad spec or a missing password must
    // fail before the radio is touched (PYTHON_CLI_SPEC 9.2).
    let wifi = match resolve_wifi_command(&cli) {
        Ok(wifi) => wifi,
        Err(code) => return code,
    };
    // A LAN dial also knows the token a BLE session captured for this host;
    // nothing else in the run needs it.
    let stored = stored_lan_token(&cli, &crate::lan_token_store::TokenStore::load());
    let lan_token = match resolve_lan_token(&cli, stored) {
        Ok(token) => token,
        Err(code) => return code,
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(failure) => {
            error(failure.to_string());
            return EXIT_ERROR;
        }
    };
    runtime.block_on(drive(cli, validated, wifi, lan_token))
}

/// Geometry is on exactly when Python enables it: not `--no-terminal`, a TTY
/// and a known size (PYTHON_CLI_SPEC 7).
fn geometry_enabled(cli: &Cli) -> bool {
    !cli.no_terminal && term::stdin_is_tty() && term::terminal_size().is_some()
}

async fn drive(
    cli: Cli,
    validated: Validated,
    wifi: Option<String>,
    lan_token: Option<String>,
) -> i32 {
    let timeout = Duration::from_secs_f64(validated.timeout);
    let bus = CoreBus::new();
    let mut events = bus.subscribe();

    // 1. --scan: print the table, then maybe stop (PYTHON_CLI_SPEC 8.1/8.2).
    let mut scanned: Option<Vec<DiscoveredDevice>> = None;
    if cli.scan {
        match ble::scan(timeout).await {
            Ok(devices) => {
                let mut display = devices.clone();
                ble::sort_for_display(&mut display);
                for device in &display {
                    println!("{}", ble::format_scan_line(device));
                }
                if exit_after_scan(&cli) {
                    return EXIT_OK;
                }
                scanned = Some(devices);
            }
            Err(failure) => {
                error(failure.to_string());
                return EXIT_ERROR;
            }
        }
    }

    // A5: `--tui` on its own must not gate the interface on the radio — the
    // TUI opens first and dials in the background, so a dead adapter or a
    // missing device can only toast, never keep the user out. Anything that
    // needs a live session up front (queries, WiFi/WebDAV mutations,
    // `--loopback-test`) still connects first, exactly like Python.
    let pending_commands = build_commands(&cli, validated.uart.as_deref(), wifi.clone());
    let defer_connect = tui_defers_connect(&cli, &pending_commands);

    // 2. Pick the target: an explicit address wins, then a match against the
    //    scan we just ran, otherwise ble::connect scans (PYTHON_CLI_SPEC 8.5).
    let mut address = cli.address.clone();
    if address.is_none() && cli.lan.is_none() {
        if let Some(devices) = scanned.as_ref() {
            let pairs: Vec<(Option<String>, String)> = devices
                .iter()
                .map(|device| (device.name.clone(), device.address.clone()))
                .collect();
            match match_device(&pairs, &cli.name) {
                Some(found) => address = Some(found),
                None => {
                    error(format!(
                        "device not found matching: {}*",
                        normalize_name_prefix(&cli.name)
                    ));
                    return EXIT_ERROR;
                }
            }
        } else if !defer_connect {
            info(format!(
                "scanning for BLE device matching {}*",
                normalize_name_prefix(&cli.name)
            ));
        }
    }

    // 3. Connect (PYTHON_CLI_SPEC 8.6: connecting... -> GPIO hint -> log).
    //    Deferred connects report "connecting over …" from inside the TUI.
    let transport = match cli.lan.as_deref() {
        Some(host) => TransportSpec::Lan {
            host: host.to_string(),
            token: lan_token,
        },
        None => TransportSpec::Ble {
            name: cli.name.clone(),
            address,
            timeout,
        },
    };
    let opts = SessionOptions {
        transport,
        ble_write_size: validated.ble_write_size,
        log_file: cli.log_file.clone(),
        debug_io: cli.debug_io,
        geometry: geometry_enabled(&cli),
    };
    let setup = SessionSetup {
        pair: cli.pair,
        json: cli.json,
    };
    let session = if defer_connect {
        // Nothing to hand the UI but a disconnected shell; `connect::begin_cli`
        // starts the real session once the screen is up.
        SessionHandle::detached(bus.clone())
    } else {
        info("connecting...");
        if cli.lan.is_none() {
            info(
                "new host: hold Bee GPIO1 to GND before pairing. Bonded hosts reconnect without GPIO1.",
            );
        }
        let session = match crate::session::spawn_session_with(
            opts.clone(),
            bus.clone(),
            setup.clone(),
        )
        .await
        {
            Ok(session) => session,
            Err(failure) => {
                error(failure.to_string());
                return EXIT_ERROR;
            }
        };
        if cli.lan.is_none() {
            if let Err(message) = precheck_capabilities(&cli, session.info().capabilities) {
                error(message);
                session.disconnect();
                return EXIT_ERROR;
            }
        }
        session
    };
    let pending = defer_connect.then_some(PendingConnect { opts, setup });

    // Ctrl-C behaves like Python's KeyboardInterrupt: disconnect, then 130.
    tokio::select! {
        code = drive_connected(&cli, &validated, wifi, session.clone(), &mut events, pending) => code,
        () = interrupt() => {
            session.disconnect();
            // Let the teardown command reach the transport task.
            tokio::time::sleep(Duration::from_millis(100)).await;
            EXIT_INTERRUPTED
        }
    }
}

/// Resolves only on a real Ctrl-C; a missing signal listener would otherwise
/// win the select above.
async fn interrupt() {
    if tokio::signal::ctrl_c().await.is_ok() {
        return;
    }
    std::future::pending::<()>().await
}

async fn drive_connected(
    cli: &Cli,
    validated: &Validated,
    wifi: Option<String>,
    session: SessionHandle,
    events: &mut broadcast::Receiver<CoreEvent>,
    pending: Option<PendingConnect>,
) -> i32 {
    // 4. Management commands in table order (PYTHON_CLI_SPEC 3.2).
    for (command, wait_final) in build_commands(cli, validated.uart.as_deref(), wifi) {
        let pending = session.request_mgmt(command, wait_final);
        if let Err(message) = pump_until(events, pending, cli.json, quiet(), &mut emit).await {
            error(message);
            return EXIT_ERROR;
        }
    }

    // 5. Loopback test (PYTHON_CLI_SPEC 6.6).
    if let Some(payload) = cli.loopback_test.as_deref() {
        let ok = run_loopback(
            &session,
            events,
            payload.as_bytes(),
            validated.loopback_timeout,
            cli.json,
        )
        .await;
        if !ok {
            error("loopback test failed");
            return EXIT_ERROR;
        }
    }

    // 6. `--no-terminal` stops here; `--tui`/`tui` hands the session over.
    if cli.no_terminal {
        drain(events, cli.json, quiet(), &mut emit).await;
        return EXIT_OK;
    }
    if cli.tui {
        let bus = session.bus().clone();
        return crate::tui::run(TuiContext {
            session,
            bus,
            pending,
        });
    }
    terminal_loop(session, events, validated, cli).await
}

// ---------------------------------------------------------------------------
// Bus plumbing
// ---------------------------------------------------------------------------

type Sink<'a> = &'a mut dyn FnMut(&[Output]);

/// Render everything already published. The response's display event and the
/// reply leave the session in one step, so a yield plus a drain makes
/// "print the answer, then act on it" deterministic.
async fn drain(
    events: &mut broadcast::Receiver<CoreEvent>,
    json: bool,
    quiet: bool,
    sink: Sink<'_>,
) {
    for _ in 0..2 {
        tokio::task::yield_now().await;
        loop {
            match events.try_recv() {
                Ok(event) => {
                    let (_, outputs) = render_event(json, quiet, event);
                    sink(&outputs);
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    }
}

/// Pump the bus while one management request is in flight: every message is
/// rendered as it arrives, exactly like Python's `send()`, which runs on the
/// same loop as the indication handler.
async fn pump_until(
    events: &mut broadcast::Receiver<CoreEvent>,
    pending: oneshot::Receiver<Result<MgmtReply, String>>,
    json: bool,
    quiet: bool,
    sink: Sink<'_>,
) -> Result<MgmtReply, String> {
    tokio::pin!(pending);
    let result: Option<Result<MgmtReply, String>> = loop {
        tokio::select! {
            result = &mut pending => break result.ok(),
            event = events.recv() => match event {
                Ok(event) => {
                    let (_, outputs) = render_event(json, quiet, event);
                    sink(&outputs);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break None,
            },
        }
    };
    drain(events, json, quiet, sink).await;
    match result {
        Some(result) => result,
        None => Err("session gone".to_string()),
    }
}

/// PYTHON_CLI_SPEC 6.6: send a payload and require the same bytes back.
async fn run_loopback(
    session: &SessionHandle,
    events: &mut broadcast::Receiver<CoreEvent>,
    payload: &[u8],
    timeout: f64,
    json: bool,
) -> bool {
    info(format!("loopback -> {}", python_repr_bytes(payload)));
    if session.send_uart(payload.to_vec()).is_err() {
        eprintln!("loopback FAIL <- {}", python_repr_bytes(&[]));
        return false;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(timeout);
    let mut received: Vec<u8> = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        // Python waits `max(0.1, deadline - now)` per chunk.
        let wait = remaining.max(Duration::from_millis(100));
        tokio::select! {
            event = events.recv() => match event {
                Ok(CoreEvent::UartRx(bytes)) => {
                    received.extend_from_slice(&bytes);
                    if !payload.is_empty()
                        && received.windows(payload.len()).any(|window| window == payload)
                    {
                        eprintln!("loopback PASS <- {}", python_repr_bytes(&received));
                        return true;
                    }
                }
                Ok(event) => {
                    let (_, outputs) = render_event(json, quiet(), event);
                    emit(&outputs);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = tokio::time::sleep(wait) => {
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
            }
        }
    }
    eprintln!("loopback FAIL <- {}", python_repr_bytes(&received));
    false
}

// ---------------------------------------------------------------------------
// Terminal session
// ---------------------------------------------------------------------------

/// Why the terminal loop ended.
enum TerminalEnd {
    /// Escape byte or EOF: `terminal closed.` and exit 0.
    Closed,
    /// The link dropped: empty line + error and exit 3.
    Disconnected,
    /// A write failed: exit 1 (the error line was already printed).
    Failed,
}

async fn send_payload(session: &SessionHandle, data: &[u8], cli: &Cli) -> Result<(), String> {
    let data = translate_enter(data, &cli.enter);
    if cli.local_echo {
        emit(&[Output::Stdout(data.clone())]);
    }
    // Geometry marks itself busy on every outbound frame (session side).
    session
        .send_uart(data)
        .map_err(|failure| failure.to_string())
}

#[cfg(unix)]
async fn winch(signal: &mut Option<tokio::signal::unix::Signal>) {
    match signal {
        Some(signal) => {
            signal.recv().await;
        }
        None => std::future::pending::<()>().await,
    }
}

#[cfg(not(unix))]
async fn winch(_signal: &mut Option<()>) {
    std::future::pending::<()>().await
}

async fn terminal_loop(
    session: SessionHandle,
    events: &mut broadcast::Receiver<CoreEvent>,
    validated: &Validated,
    cli: &Cli,
) -> i32 {
    // Line mode is automatic when stdin is not a TTY: scripted use must keep
    // working (CONTRACTS section 4).
    let line_mode = cli.line_mode || !term::stdin_is_tty();
    let raw_guard = match term::RawModeGuard::enable(!line_mode) {
        Ok(guard) => guard,
        Err(failure) => {
            error(failure.to_string());
            return EXIT_ERROR;
        }
    };

    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::unbounded_channel::<Option<Vec<u8>>>();
    std::thread::spawn(move || loop {
        let data = term::read_stdin_line_or_chunk(line_mode);
        let eof = data.is_none();
        if stdin_tx.send(data).is_err() || eof {
            break;
        }
    });

    let geometry = geometry_enabled(cli);
    #[cfg(unix)]
    let mut winch_signal = if geometry {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change()).ok()
    } else {
        None
    };
    #[cfg(not(unix))]
    let mut winch_signal: Option<()> = None;

    info(format!(
        "terminal open. press {} to exit.",
        describe_escape(validated.escape)
    ));

    let end = loop {
        tokio::select! {
            item = stdin_rx.recv() => {
                let Some(item) = item else {
                    break TerminalEnd::Closed;
                };
                let Some(data) = item else {
                    // EOF: Python sleeps 0.2 s so trailing output lands first.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    break TerminalEnd::Closed;
                };
                // The escape byte only counts inside one read chunk.
                match data.iter().position(|byte| *byte == validated.escape) {
                    Some(position) => {
                        if position != 0 {
                            if let Err(failure) =
                                send_payload(&session, &data[..position], cli).await
                            {
                                error(failure);
                                break TerminalEnd::Failed;
                            }
                        }
                        break TerminalEnd::Closed;
                    }
                    None => {
                        if let Err(failure) = send_payload(&session, &data, cli).await {
                            error(failure);
                            break TerminalEnd::Failed;
                        }
                    }
                }
            }
            event = events.recv() => match event {
                Ok(event) => {
                    let (flow, outputs) = render_event(cli.json, quiet(), event);
                    emit(&outputs);
                    match flow {
                        Flow::Continue => {}
                        Flow::Failed => break TerminalEnd::Failed,
                        Flow::Disconnected => break TerminalEnd::Disconnected,
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break TerminalEnd::Disconnected,
            },
            () = winch(&mut winch_signal) => {
                if geometry {
                    if let Some((cols, rows)) = term::terminal_size() {
                        session.set_terminal_size(cols, rows);
                    }
                }
            }
        }
    };

    // Python restores the terminal before printing the closing hint.
    drop(raw_guard);
    match end {
        TerminalEnd::Closed => {
            info("terminal closed.");
            EXIT_OK
        }
        TerminalEnd::Disconnected => {
            // Raw mode left the cursor mid-line, so start a fresh one.
            eprintln!();
            error("BLE disconnected during the terminal session");
            EXIT_DISCONNECTED
        }
        TerminalEnd::Failed => EXIT_ERROR,
    }
}

// ---------------------------------------------------------------------------
// Tests: parser parity, validation messages, exit codes, rendering
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::MgmtKind;
    use std::sync::{Arc, Mutex};

    /// The TUI owns the screen, so `linkr:` diagnostics must reach its notice
    /// log instead of the console: ratatui diffs its own buffer and never
    /// repaints the cells a stray `eprintln!` overwrote. The routed line has
    /// to stay byte-identical to the stderr one, or the log would disagree
    /// with the CLI's own output.
    #[test]
    fn diagnostics_route_to_the_sink_while_the_screen_is_owned() {
        let (tx, rx) = std::sync::mpsc::channel();
        set_diagnostic_sink(Some(tx));

        warn("two devices match");
        error("management channel closed");
        // A sink whose reader already went away must not take us down.
        let (dead_tx, dead_rx) = std::sync::mpsc::channel::<(NoticeLevel, String)>();
        drop(dead_rx);
        set_diagnostic_sink(Some(dead_tx));
        warn("nobody is listening");

        set_diagnostic_sink(None);
        let seen: Vec<String> = rx.try_iter().map(|(_, text)| text).collect();
        assert!(
            seen.contains(&"linkr: warning: two devices match".to_string()),
            "warn must keep its exact prefix, got {seen:?}"
        );
        assert!(
            seen.contains(&"linkr: error: management channel closed".to_string()),
            "error must keep its exact prefix, got {seen:?}"
        );
    }

    /// Parse a full argv (`linkr ...`) through clap only.
    fn parse(args: &[&str]) -> Cli {
        let mut argv: Vec<&str> = vec![BIN_NAME];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).expect("test arguments must parse")
    }

    /// Parse through the two-layer `parse_args` and assert a `Done` code.
    fn done_code(args: &[&str]) -> i32 {
        let mut argv: Vec<String> = vec![BIN_NAME.to_string()];
        argv.extend(args.iter().map(|arg| arg.to_string()));
        match parse_args(argv) {
            Parsed::Done(code) => code,
            Parsed::Run(_) => panic!("{args:?} should not need a run"),
        }
    }

    /// A5: `--tui` alone opens the interface without dialling first (the TUI
    /// connects in the background); every flag whose output needs a live
    /// session still connects up front, exactly like Python.
    #[test]
    fn the_tui_only_defers_the_connect_when_nothing_needs_a_session() {
        let cli = parse(&["--tui"]);
        assert!(tui_defers_connect(&cli, &build_commands(&cli, None, None)));

        let cli = parse(&["--tui", "--query-info"]);
        let commands = build_commands(&cli, None, None);
        assert_eq!(commands.len(), 1, "--query-info asks the device");
        assert!(!tui_defers_connect(&cli, &commands));

        let cli = parse(&["--tui", "--loopback-test"]);
        assert!(!tui_defers_connect(&cli, &build_commands(&cli, None, None)));

        let cli = parse(&["--tui", "--no-terminal"]);
        assert!(!tui_defers_connect(&cli, &build_commands(&cli, None, None)));

        let cli = parse(&[]);
        assert!(
            !tui_defers_connect(&cli, &[]),
            "without --tui the CLI connects as it always did"
        );
    }

    fn outputs_text(outputs: &[Output]) -> String {
        let mut text = String::new();
        for output in outputs {
            match output {
                Output::Stdout(bytes) => text.push_str(&String::from_utf8_lossy(bytes)),
                Output::Stderr(line) => {
                    text.push_str(line);
                    text.push('\n');
                }
            }
        }
        text
    }

    /// B6: PYTHON_CLI_SPEC 2.3/4.4 keep `--write-response`/`--write-delay-ms`
    /// as dead options and note that "a faithful port should still implement
    /// the branch". This port cannot: `connect` rejects any device that does
    /// not advertise Reliable UART, and adding the two settings would change
    /// `SessionOptions`, whose shape CONTRACTS.md fixes. So the flags keep
    /// parsing and validating exactly like argparse (CLI parity) and `--help`
    /// says plainly that they never take effect.
    #[test]
    fn the_nus_only_flags_parse_and_say_that_they_are_inert() {
        let cli = parse(&["--write-response", "--write-delay-ms", "12"]);
        assert!(cli.write_response);
        assert_eq!(cli.write_delay_ms, "12");
        let rejected = validate(&parse(&["--write-delay-ms", "-1"]))
            .expect_err("a negative delay must not validate, inert or not");
        assert!(rejected.contains("argument --write-delay-ms"), "{rejected}");

        let mut command = Cli::command();
        let help = command.render_help().to_string();
        assert!(help.contains("NUS fallback only"), "{help}");
    }

    #[test]
    fn help_and_version_exit_zero() {
        for flag in ["--help", "-h"] {
            let error = Cli::try_parse_from([BIN_NAME, flag]).expect_err(flag);
            assert_eq!(error.exit_code(), 0, "{flag} must exit 0");
        }
        let error = Cli::try_parse_from([BIN_NAME, "--version"]).expect_err("--version");
        assert_eq!(error.exit_code(), 0);
        assert_eq!(done_code(&["--help"]), 0);
        assert_eq!(done_code(&["--version"]), 0);
    }

    #[test]
    fn usage_errors_exit_two() {
        assert_eq!(
            Cli::try_parse_from([BIN_NAME, "--nope"])
                .unwrap_err()
                .exit_code(),
            2
        );
        assert_eq!(
            Cli::try_parse_from([BIN_NAME, "--timeout"])
                .unwrap_err()
                .exit_code(),
            2
        );
        assert_eq!(
            Cli::try_parse_from([BIN_NAME, "bogus"])
                .unwrap_err()
                .exit_code(),
            2
        );
        // Both completion entry points reject unknown shells like argparse does.
        assert_eq!(done_code(&["completion", "csh"]), 2);
        assert_eq!(done_code(&["--print-completion", "csh"]), 2);
        assert_eq!(done_code(&["--nope"]), 2);
        // fail_usage is the single usage-error printer: usage line + exit 2.
        assert_eq!(fail_usage("boom"), EXIT_USAGE);
    }

    #[test]
    fn exit_codes_keep_their_python_values() {
        assert_eq!(
            (
                EXIT_OK,
                EXIT_ERROR,
                EXIT_USAGE,
                EXIT_DISCONNECTED,
                EXIT_INTERRUPTED
            ),
            (0, 1, 2, 3, 130)
        );
    }

    #[test]
    fn subcommands_set_the_equivalent_flags() {
        match parse_args([BIN_NAME.to_string(), "scan".to_string()]) {
            Parsed::Run(cli) => assert!(cli.scan),
            Parsed::Done(_) => panic!("scan subcommand must run"),
        }
        match parse_args([BIN_NAME.to_string(), "tui".to_string()]) {
            Parsed::Run(cli) => assert!(cli.tui),
            Parsed::Done(_) => panic!("tui subcommand must run"),
        }
    }

    #[test]
    fn defaults_match_the_python_parser() {
        let cli = parse(&[]);
        assert_eq!(cli.name, "Linkr BLE UART");
        assert_eq!(cli.timeout, "8.0");
        assert_eq!(cli.loopback_timeout, "3.0");
        assert_eq!(cli.write_delay_ms, "5.0");
        assert_eq!(cli.ble_write_size, "0");
        assert_eq!(cli.escape, "^]");
        assert_eq!(cli.enter, "raw");
        assert!(!cli.scan && !cli.tui && !cli.yes && !cli.quiet && !cli.json);
        assert!(!cli.no_terminal && !cli.pair && !cli.local_echo && !cli.line_mode);
        assert!(!cli.query_info && !cli.query_uart && !cli.query_wifi && !cli.query_webdav);
        assert!(!cli.wifi_scan && !cli.wifi_off && !cli.webdav_off && !cli.debug_io);
        assert!(!cli.write_response && !cli.loopback_test.is_some());
        assert!(cli.address.is_none() && cli.uart.is_none() && cli.wifi.is_none());
        assert!(cli.webdav.is_none() && cli.log_file.is_none() && cli.lan.is_none());
        assert!(cli.lan_token.is_none() && cli.lan_token_file.is_none());
        assert!(cli.print_completion.is_none() && cli.command.is_none());
        // Defaults must validate without a single complaint.
        let validated = validate(&cli).expect("defaults validate");
        assert_eq!(validated.timeout, 8.0);
        assert_eq!(validated.loopback_timeout, 3.0);
        assert_eq!(validated.ble_write_size, 0);
        assert_eq!(validated.escape, 0x1d);
        assert!(validated.uart.is_none());
    }

    #[test]
    fn loopback_test_takes_an_optional_value() {
        let cli = parse(&["--loopback-test"]);
        assert_eq!(cli.loopback_test.as_deref(), Some("A"));
        let cli = parse(&["--loopback-test", "ABCDEF"]);
        assert_eq!(cli.loopback_test.as_deref(), Some("ABCDEF"));
        let cli = parse(&[]);
        assert_eq!(cli.loopback_test, None);
    }

    #[test]
    fn numeric_validators_reproduce_the_argparse_messages() {
        assert_eq!(
            positive_float("timeout", "0").unwrap_err(),
            "argument --timeout: value must be greater than zero"
        );
        assert_eq!(
            positive_float("timeout", "-1").unwrap_err(),
            "argument --timeout: value must be greater than zero"
        );
        assert_eq!(
            positive_float("timeout", "abc").unwrap_err(),
            "argument --timeout: invalid positive_float value: 'abc'"
        );
        assert_eq!(positive_float("timeout", "2.5").unwrap(), 2.5);
        assert_eq!(
            nonnegative_float("write-delay-ms", "-0.5").unwrap_err(),
            "argument --write-delay-ms: value must not be negative"
        );
        assert_eq!(
            nonnegative_float("write-delay-ms", "x").unwrap_err(),
            "argument --write-delay-ms: invalid nonnegative_float value: 'x'"
        );
        assert_eq!(nonnegative_float("write-delay-ms", "0").unwrap(), 0.0);
        assert_eq!(
            ble_write_size("ble-write-size", "-1").unwrap_err(),
            "argument --ble-write-size: BLE write size must be between 0 and 244"
        );
        assert_eq!(
            ble_write_size("ble-write-size", "245").unwrap_err(),
            "argument --ble-write-size: BLE write size must be between 0 and 244"
        );
        assert_eq!(
            ble_write_size("ble-write-size", "abc").unwrap_err(),
            "argument --ble-write-size: invalid ble_write_size value: 'abc'"
        );
        assert_eq!(ble_write_size("ble-write-size", "244").unwrap(), 244);
    }

    #[test]
    fn validation_follows_the_inventory_order() {
        // Both `--timeout` and `--enter` are broken; the earlier flag wins,
        // exactly like argparse reporting the first violation it hits.
        let cli = parse(&["--timeout", "0", "--enter", "bogus"]);
        assert_eq!(
            validate(&cli).unwrap_err(),
            "argument --timeout: value must be greater than zero"
        );
        let cli = parse(&["--enter", "bogus"]);
        assert_eq!(
            validate(&cli).unwrap_err(),
            "argument --enter: invalid choice: 'bogus' (choose from 'raw', 'cr', 'lf', 'crlf')"
        );
        let cli = parse(&["--loopback-timeout", "-2"]);
        assert_eq!(
            validate(&cli).unwrap_err(),
            "argument --loopback-timeout: value must be greater than zero"
        );
        let cli = parse(&["--write-delay-ms", "-1"]);
        assert_eq!(
            validate(&cli).unwrap_err(),
            "argument --write-delay-ms: value must not be negative"
        );
        let cli = parse(&["--ble-write-size", "300"]);
        assert_eq!(
            validate(&cli).unwrap_err(),
            "argument --ble-write-size: BLE write size must be between 0 and 244"
        );
        let cli = parse(&["--lan", "   "]);
        assert_eq!(
            validate(&cli).unwrap_err(),
            "argument --lan: Enter the device address first."
        );
        let cli = parse(&["--escape", "abc"]);
        assert_eq!(
            validate(&cli).unwrap_err(),
            "argument --escape: escape must be one byte, like ^] or 0x1d"
        );
        let cli = parse(&["--uart", "115200,8,n,1"]);
        assert_eq!(
            validate(&cli).unwrap_err(),
            "argument --uart: UART spec must be baud,data,parity,stop,flow, like 115200,8,n,1,n"
        );
        let cli = parse(&["--uart", "100,8,n,1,n"]);
        assert_eq!(
            validate(&cli).unwrap_err(),
            "argument --uart: UART baud rate must be between 300 and 3000000"
        );
        let cli = parse(&["--uart", "115200,9,n,1,n"]);
        assert_eq!(
            validate(&cli).unwrap_err(),
            "argument --uart: UART data bits must be one of 5, 6, 7, 8"
        );
        let cli = parse(&["--uart", "115200,8,x,1,n"]);
        assert_eq!(
            validate(&cli).unwrap_err(),
            "argument --uart: UART parity must be none, odd or even (n/o/e)"
        );
        let cli = parse(&["--uart", "115200,8,n,1,q"]);
        assert_eq!(
            validate(&cli).unwrap_err(),
            "argument --uart: UART flow control must be none or rtscts"
        );
    }

    #[test]
    fn accepted_values_are_normalized_for_the_wire() {
        let cli = parse(&[
            "--timeout",
            "2.5",
            "--uart",
            "000115200, 8 , EVEN , 2 , HW",
            "--escape",
            "0x1d",
        ]);
        let validated = validate(&cli).expect("valid combination");
        assert_eq!(validated.timeout, 2.5);
        assert_eq!(validated.escape, 0x1d);
        assert_eq!(validated.uart.as_deref(), Some("115200,8,e,2,rtscts"));
        // The caret form of the default escape byte.
        let validated = validate(&parse(&[])).unwrap();
        assert_eq!(validated.escape, 0x1d);
        let cli = parse(&["--enter", "crlf", "--ble-write-size", "20"]);
        let validated = validate(&cli).unwrap();
        assert_eq!(validated.ble_write_size, 20);
    }

    #[test]
    fn completion_scripts_cover_every_supported_shell() {
        for shell in ["bash", "fish", "zsh", "powershell"] {
            let script = completion_script(shell).expect(shell);
            assert!(script.contains("linkr"), "{shell} must complete linkr");
            assert!(
                script.ends_with("\n\n"),
                "{shell} must end with a blank line, like Python's print(script)"
            );
            match shell {
                "bash" => assert!(script.contains("complete "), "{shell}"),
                "zsh" => assert!(script.contains("compdef linkr"), "{shell}"),
                "fish" => assert!(script.contains("complete -c linkr"), "{shell}"),
                "powershell" => assert!(
                    script.to_lowercase().contains("register-argumentcompleter"),
                    "{shell}"
                ),
                _ => unreachable!(),
            }
        }
        let error = completion_script("csh").unwrap_err();
        assert_eq!(
            error,
            "invalid choice: 'csh' (choose from 'bash', 'fish', 'zsh', 'powershell')"
        );
        // The scripts must offer the subcommands and every Python flag.
        let bash = completion_script("bash").unwrap();
        for needle in ["scan", "tui", "completion", "--scan", "--quiet", "--lan"] {
            assert!(bash.contains(needle), "bash completion must offer {needle}");
        }
        // One entry point prints and exits 0, the other reports usage and 2.
        assert!(matches!(
            print_completion("zsh", "--print-completion"),
            Parsed::Done(0)
        ));
        assert!(matches!(
            print_completion("csh", "--print-completion"),
            Parsed::Done(2)
        ));
    }

    #[test]
    fn control_action_inventory_matches_the_python_list() {
        // Every flag Python counts (PYTHON_CLI_SPEC 8.2).
        let mut cli = parse(&[]);
        assert!(!has_control_action(&cli));
        cli.query_info = true;
        assert!(has_control_action(&cli));
        let cli = parse(&["--query-uart"]);
        assert!(has_control_action(&cli));
        let cli = parse(&["--uart", "115200,8,n,1,n"]);
        assert!(has_control_action(&cli));
        let cli = parse(&["--wifi", "home,secret"]);
        assert!(has_control_action(&cli));
        let cli = parse(&["--wifi-scan"]);
        assert!(has_control_action(&cli));
        let cli = parse(&["--wifi-off"]);
        assert!(has_control_action(&cli));
        let cli = parse(&["--query-wifi"]);
        assert!(has_control_action(&cli));
        let cli = parse(&["--webdav", "http://x"]);
        assert!(has_control_action(&cli));
        let cli = parse(&["--webdav-off"]);
        assert!(has_control_action(&cli));
        let cli = parse(&["--query-webdav"]);
        assert!(has_control_action(&cli));
        let cli = parse(&["--loopback-test"]);
        assert!(has_control_action(&cli));
        let cli = parse(&["--pair"]);
        assert!(has_control_action(&cli));
        // Never counted: --no-terminal, --scan, --name, --json, --tui, ...
        let cli = parse(&[
            "--no-terminal",
            "--scan",
            "--json",
            "--name",
            "X",
            "--tui",
            "--yes",
            "--line-mode",
        ]);
        assert!(!has_control_action(&cli));
    }

    #[test]
    fn scan_alone_exits_unless_something_else_was_asked_for() {
        let cli = parse(&["--scan"]);
        assert!(exit_after_scan(&cli), "--scan alone exits 0");
        let cli = parse(&["--scan", "--no-terminal"]);
        assert!(exit_after_scan(&cli), "--scan --no-terminal still exits 0");
        let cli = parse(&["--scan", "--address", "AA:BB"]);
        assert!(!exit_after_scan(&cli), "--scan --address connects");
        let cli = parse(&["--scan", "--wifi", "home,secret"]);
        assert!(!exit_after_scan(&cli), "a control action connects");
        let cli = parse(&["--scan", "--tui"]);
        assert!(!exit_after_scan(&cli), "--scan --tui hands over to the TUI");
        let cli = parse(&["--scan", "--lan", "127.0.0.1:8080"]);
        assert!(
            !exit_after_scan(&cli),
            "--scan --lan connects over the bridge"
        );
    }

    #[test]
    fn commands_run_in_python_order_with_the_python_waits() {
        let mut cli = parse(&[]);
        cli.query_info = true;
        cli.query_uart = true;
        cli.wifi_off = true;
        cli.query_wifi = true;
        cli.wifi_scan = true;
        cli.webdav = Some("http://example.invalid/dav".to_string());
        cli.webdav_off = true;
        cli.query_webdav = true;
        let commands = build_commands(
            &cli,
            Some("115200,8,n,1,n"),
            Some("@w=home,secret".to_string()),
        );
        let seen: Vec<&str> = commands
            .iter()
            .map(|(command, _)| command.as_str())
            .collect();
        assert_eq!(
            seen,
            [
                "@i?",
                "@u=115200,8,n,1,n",
                "@u?",
                "@w=home,secret",
                "@w off",
                "@w?",
                "@w scan",
                "@d=http://example.invalid/dav",
                "@d off",
                "@d?",
            ]
        );
        // WiFi operations and the scan wait for their FINAL event (35 s).
        let wifi_wait = Some(Duration::from_secs_f64(WIFI_OPERATION_TIMEOUT_SECS));
        let scan_wait = Some(Duration::from_secs_f64(WIFI_SCAN_TIMEOUT_SECS));
        assert_eq!(commands[0].1, None);
        assert_eq!(commands[1].1, None);
        assert_eq!(commands[2].1, None);
        assert_eq!(commands[3].1, wifi_wait);
        assert_eq!(commands[4].1, wifi_wait);
        assert_eq!(commands[5].1, None);
        assert_eq!(commands[6].1, scan_wait);
        assert_eq!(commands[7].1, None);
        assert_eq!(commands[8].1, None);
        assert_eq!(commands[9].1, None);
        // Nothing asked for, nothing sent.
        let cli = parse(&[]);
        assert!(build_commands(&cli, None, None).is_empty());
    }

    #[test]
    fn capability_precheck_uses_the_python_strings() {
        let no_actions = parse(&[]);
        assert_eq!(precheck_capabilities(&no_actions, 0), Ok(()));

        let wifi = parse(&["--wifi", "home,secret"]);
        assert_eq!(
            precheck_capabilities(&wifi, 0).unwrap_err(),
            "device does not advertise WiFi support"
        );
        // WiFi without async events is the second gate.
        assert_eq!(
            precheck_capabilities(&wifi, MGMT_CAP_WIFI).unwrap_err(),
            "device does not advertise async event support"
        );
        assert_eq!(
            precheck_capabilities(&wifi, MGMT_CAP_WIFI | MGMT_CAP_ASYNC_EVENTS),
            Ok(())
        );

        let scan = parse(&["--wifi-scan"]);
        assert_eq!(
            precheck_capabilities(&scan, MGMT_CAP_WIFI).unwrap_err(),
            "device does not advertise async event support"
        );
        let query = parse(&["--query-wifi"]);
        assert_eq!(
            precheck_capabilities(&query, MGMT_CAP_WIFI | MGMT_CAP_ASYNC_EVENTS),
            Ok(()),
            "query-only actions skip the async gate"
        );

        let webdav = parse(&["--webdav", "http://example.invalid/dav"]);
        assert_eq!(
            precheck_capabilities(&webdav, MGMT_CAP_WIFI | MGMT_CAP_ASYNC_EVENTS).unwrap_err(),
            "device does not advertise WebDAV support"
        );
        assert_eq!(precheck_capabilities(&webdav, MGMT_CAP_WEBDAV), Ok(()));
        let off = parse(&["--webdav-off"]);
        assert_eq!(
            precheck_capabilities(&off, 0).unwrap_err(),
            "device does not advertise WebDAV support"
        );
    }

    #[test]
    fn notices_follow_the_message_catalog() {
        assert_eq!(
            outputs_text(&render_notice(false, NoticeLevel::Info, "connecting...")),
            "linkr: connecting...\n"
        );
        assert_eq!(
            outputs_text(&render_notice(true, NoticeLevel::Info, "connecting...")),
            "",
            "--quiet suppresses info lines"
        );
        assert_eq!(
            outputs_text(&render_notice(true, NoticeLevel::Warn, "careful")),
            "linkr: warning: careful\n"
        );
        assert_eq!(
            outputs_text(&render_notice(true, NoticeLevel::Error, "boom")),
            "linkr: error: boom\n"
        );
        // Byte traces are raw stderr: no prefix, never quieted.
        for trace in ["RX 3a 01", "TX b'hi'", "UART TX 0a", "MGMT TX #1 b'@i?'"] {
            assert_eq!(
                outputs_text(&render_notice(true, NoticeLevel::Info, trace)),
                format!("{trace}\n"),
                "{trace} must stay raw"
            );
        }
    }

    #[test]
    fn events_render_like_the_python_printer() {
        let (flow, outputs) = render_event(false, false, CoreEvent::UartRx(b"hello".to_vec()));
        assert_eq!(flow, Flow::Continue);
        assert_eq!(outputs, vec![Output::Stdout(b"hello".to_vec())]);

        let event = CoreEvent::MgmtMessage {
            kind: MgmtKind::Response,
            id: 7,
            ok: true,
            final_: false,
            lines: vec!["fw=1.2.3".to_string()],
            command: Some("@i?".to_string()),
        };
        let (flow, outputs) = render_event(false, false, event.clone());
        assert_eq!(flow, Flow::Continue);
        assert_eq!(outputs_text(&outputs), "response #7 <- fw=1.2.3\n");

        let (flow, outputs) = render_event(true, false, event);
        assert_eq!(flow, Flow::Continue);
        assert_eq!(
            outputs_text(&outputs),
            "{\"type\": \"response\", \"requestId\": 7, \"ok\": true, \
             \"lines\": [\"fw=1.2.3\"], \"command\": \"@i?\"}\n"
        );

        let event = CoreEvent::MgmtMessage {
            kind: MgmtKind::Event,
            id: 4,
            ok: false,
            final_: true,
            lines: vec!["ERR wifi".to_string()],
            command: None,
        };
        assert_eq!(
            outputs_text(&render_event(false, false, event.clone()).1),
            "event #4 <- ERR wifi\n"
        );
        assert_eq!(
            outputs_text(&render_event(true, false, event).1),
            "{\"type\": \"event\", \"requestId\": 4, \"ok\": false, \"lines\": [\"ERR wifi\"]}\n"
        );

        let notice = CoreEvent::Notice {
            level: NoticeLevel::Info,
            text: "loopback -> b'A'".to_string(),
        };
        assert_eq!(
            outputs_text(&render_event(false, false, notice).1),
            "linkr: loopback -> b'A'\n"
        );

        // A dropped link ends the terminal loop (exit 3), a healthy one does not.
        let (flow, outputs) = render_event(
            false,
            false,
            CoreEvent::Connection {
                state: ConnectionState::Disconnected,
                detail: "gone".to_string(),
            },
        );
        assert_eq!(flow, Flow::Disconnected);
        assert!(outputs.is_empty());
        let (flow, _) = render_event(
            false,
            false,
            CoreEvent::Connection {
                state: ConnectionState::Connected,
                detail: "ready".to_string(),
            },
        );
        assert_eq!(flow, Flow::Continue);
        // A failed connection is the only event that fails the loop (exit 1).
        let (flow, outputs) = render_event(
            false,
            false,
            CoreEvent::Connection {
                state: ConnectionState::Failed,
                detail: "no route".to_string(),
            },
        );
        assert_eq!(flow, Flow::Failed);
        assert!(outputs.is_empty());
    }

    #[tokio::test]
    async fn pump_renders_every_message_before_the_reply_resolves() {
        let (bus, mut events) = broadcast::channel::<CoreEvent>(16);
        bus.send(CoreEvent::MgmtMessage {
            kind: MgmtKind::Response,
            id: 1,
            ok: true,
            final_: false,
            lines: vec!["fw=1.2.3".to_string()],
            command: Some("@i?".to_string()),
        })
        .unwrap();
        bus.send(CoreEvent::UartRx(b"boot\n".to_vec())).unwrap();

        let (reply, pending) = oneshot::channel();
        reply
            .send(Ok(MgmtReply {
                id: 1,
                lines: vec!["fw=1.2.3".to_string()],
                events: Vec::new(),
                raw: "fw=1.2.3".to_string(),
                ok: true,
            }))
            .unwrap();

        let captured: Arc<Mutex<Vec<Output>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_buffer = Arc::clone(&captured);
        let mut sink = move |outputs: &[Output]| {
            sink_buffer
                .lock()
                .expect("sink")
                .extend(outputs.iter().cloned());
        };
        let reply = pump_until(&mut events, pending, false, false, &mut sink)
            .await
            .expect("the request resolves");
        assert_eq!(reply.id, 1);

        let text = outputs_text(&captured.lock().expect("sink"));
        assert!(text.contains("response #1 <- fw=1.2.3"), "got: {text}");
        assert!(text.contains("boot\n"), "UART bytes must print too: {text}");
    }

    #[tokio::test]
    async fn pump_json_mode_writes_one_object_per_message() {
        let (bus, mut events) = broadcast::channel::<CoreEvent>(16);
        let (reply, pending) = oneshot::channel();
        reply
            .send(Ok(MgmtReply {
                id: 2,
                lines: vec!["ok".to_string()],
                events: Vec::new(),
                raw: "ok".to_string(),
                ok: true,
            }))
            .unwrap();
        let captured: Arc<Mutex<Vec<Output>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_buffer = Arc::clone(&captured);
        let mut sink = move |outputs: &[Output]| {
            sink_buffer
                .lock()
                .expect("sink")
                .extend(outputs.iter().cloned());
        };
        pump_until(&mut events, pending, true, false, &mut sink)
            .await
            .expect("the request resolves");
        bus.send(CoreEvent::MgmtMessage {
            kind: MgmtKind::Response,
            id: 2,
            ok: true,
            final_: false,
            lines: vec!["ok".to_string()],
            command: None,
        })
        .unwrap();
        drain(&mut events, true, false, &mut sink).await;
        let text = outputs_text(&captured.lock().expect("sink"));
        assert!(
            text.contains(
                "{\"type\": \"response\", \"requestId\": 2, \"ok\": true, \"lines\": [\"ok\"]}\n"
            ),
            "got: {text}"
        );
    }

    #[tokio::test]
    async fn a_dead_bus_fails_the_request_instead_of_hanging() {
        let (bus, mut events) = broadcast::channel::<CoreEvent>(8);
        bus.send(CoreEvent::Connection {
            state: ConnectionState::Disconnected,
            detail: "gone".to_string(),
        })
        .unwrap();
        drop(bus);
        let (_pending_reply, pending) = oneshot::channel::<Result<MgmtReply, String>>();
        let mut sink = |_outputs: &[Output]| {};
        let error = pump_until(&mut events, pending, false, false, &mut sink)
            .await
            .unwrap_err();
        assert_eq!(error, "session gone");
    }

    #[test]
    fn quiet_and_yes_round_trip() {
        let was_quiet = quiet();
        let was_yes = yes();
        set_quiet(true);
        assert!(quiet());
        set_quiet(was_quiet);
        assert_eq!(quiet(), was_quiet);
        set_yes(true);
        assert!(yes());
        set_yes(was_yes);
        assert_eq!(yes(), was_yes);
    }

    #[test]
    fn wifi_credentials_resolve_before_the_radio() {
        // Inline credentials never prompt, so this holds with or without a TTY.
        let cli = parse(&["--wifi", "home,secret"]);
        assert_eq!(
            resolve_wifi_command(&cli).unwrap().as_deref(),
            Some("@w=home,secret")
        );
        // An empty SSID is a usage error (exit 2), never a connection attempt.
        let cli = parse(&["--wifi", ",secret"]);
        assert_eq!(resolve_wifi_command(&cli).unwrap_err(), EXIT_USAGE);
        assert!(resolve_wifi_command(&parse(&[])).unwrap().is_none());
    }

    /// B5/B7: `@s?` answers with the bridge's LAN access token and that line
    /// travels through `render_event` on its way out — display and export are
    /// redacted (web `redactSecrets`), in both output shapes.
    #[test]
    fn the_socket_status_line_is_redacted_wherever_it_is_printed() {
        const TOKEN: &str = "0123456789abcdef0123456789abcdef";
        let event = CoreEvent::MgmtMessage {
            kind: MgmtKind::Response,
            id: 9,
            ok: true,
            final_: true,
            lines: vec![format!("OK ws=up port=80 token={TOKEN}")],
            command: Some("@s?".to_string()),
        };
        let plain = outputs_text(&render_event(false, false, event.clone()).1);
        assert!(plain.contains("token=<redacted>"), "{plain}");
        assert!(!plain.contains(TOKEN), "{plain}");

        let json = outputs_text(&render_event(true, false, event).1);
        assert!(json.contains("token=<redacted>"), "{json}");
        assert!(!json.contains(TOKEN), "{json}");
    }

    #[test]
    fn lan_tokens_resolve_and_validate_before_dialing() {
        let valid = "0123456789abcdef0123456789abcdef";
        let dir = std::env::temp_dir().join(format!("linkr-cli-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let good = dir.join("token");
        std::fs::write(&good, format!("{valid}\n")).expect("write token");
        let bad = dir.join("bad-token");
        std::fs::write(&bad, "not-a-token").expect("write token");

        // Flag beats file; the file is trimmed.
        let mut cli = parse(&["--lan-token", valid, "--lan-token-file", "ignored"]);
        cli.lan_token_file = Some(bad.clone());
        assert_eq!(
            resolve_lan_token(&cli, None).unwrap().as_deref(),
            Some(valid)
        );

        let mut cli = parse(&[]);
        cli.lan_token_file = Some(good.clone());
        assert_eq!(
            resolve_lan_token(&cli, None).unwrap().as_deref(),
            Some(valid)
        );

        // An invalid file token is a usage error naming the flag.
        let mut cli = parse(&[]);
        cli.lan_token_file = Some(bad.clone());
        assert_eq!(resolve_lan_token(&cli, None).unwrap_err(), EXIT_USAGE);

        // An unreadable file is a usage error too.
        let mut cli = parse(&[]);
        cli.lan_token_file = Some(dir.join("missing"));
        assert_eq!(resolve_lan_token(&cli, None).unwrap_err(), EXIT_USAGE);

        // An invalid inline token names --lan-token.
        let cli = parse(&["--lan-token", "XYZ"]);
        assert_eq!(resolve_lan_token(&cli, None).unwrap_err(), EXIT_USAGE);

        // A captured token only ever fills in: it is last in the precedence
        // and it never gets to argue about validity — the store only hands
        // out what `capture` accepted. Both need a clear LINKR_LAN_TOKEN,
        // which outranks the store.
        if std::env::var_os("LINKR_LAN_TOKEN").is_none() {
            let empty = parse(&[]);
            assert_eq!(
                resolve_lan_token(&empty, Some(valid.to_string()))
                    .unwrap()
                    .as_deref(),
                Some(valid)
            );
            assert_eq!(
                resolve_lan_token(&empty, Some(String::new())).unwrap(),
                None,
                "auth-less bridge dials without a token"
            );
        }
        assert_eq!(
            resolve_lan_token(&parse(&["--lan-token", valid]), Some(String::new()))
                .unwrap()
                .as_deref(),
            Some(valid),
            "an explicit token outranks the captured one"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// B5/B7: four token sources, and `--help` names all of them — the
    /// captured one is the only source the user never typed themselves, so it
    /// has to be written down.
    #[test]
    fn the_lan_token_help_names_every_source() {
        let mut command = Cli::command();
        let help = command.render_help().to_string();
        assert!(help.contains("--lan-token-file"), "{help}");
        assert!(help.contains("LINKR_LAN_TOKEN"), "{help}");
        assert!(help.contains("captured"), "{help}");
    }

    /// The glue between `--lan` and the store: only a LAN dial looks, and it
    /// looks up exactly the host it is about to dial. The store is handed in
    /// so this runs without touching the user's config directory.
    #[test]
    fn the_store_serves_the_token_of_the_host_being_dialed() {
        const DEVICE: &str = "4c494e4b52424c45010058bf2533078c";
        const TOKEN: &str = "0123456789abcdef0123456789abcdef";
        let mut store = crate::lan_token_store::TokenStore::default();
        store
            .capture(DEVICE, TOKEN, "192.168.0.104")
            .expect("a valid capture is accepted");

        let cli = parse(&["--lan", "192.168.0.104"]);
        assert_eq!(
            stored_lan_token(&cli, &store).as_deref(),
            Some(TOKEN),
            "the alias written by the BLE session is the one that dials"
        );

        // A host the store does not know gets nothing …
        let cli = parse(&["--lan", "192.168.0.9"]);
        assert_eq!(stored_lan_token(&cli, &store), None);
        // … and a run that is not dialing LAN never asks.
        let cli = parse(&[]);
        assert_eq!(stored_lan_token(&cli, &store), None);

        // End to end through the precedence: with no flag, file or environment
        // the stored token is what dials (LINKR_LAN_TOKEN outranks it).
        if std::env::var_os("LINKR_LAN_TOKEN").is_none() {
            let cli = parse(&["--lan", "192.168.0.104"]);
            assert_eq!(
                resolve_lan_token(&cli, stored_lan_token(&cli, &store))
                    .unwrap()
                    .as_deref(),
                Some(TOKEN)
            );
        }
    }
}
