//! The command line's own shape: one table, two readers.
//!
//! Usage text and shell completion are the same list looked at twice, which is
//! why they live next to each other rather than in a help string here and a
//! script there. A flag added to one and not the other is a flag that works and
//! cannot be completed on — invisible until somebody tab-completes `--lang`
//! and finds nothing, which nobody reports as a bug.
//!
//! The reference clients (`tools/linkr_ble_terminal.py`,
//! `tools/linkr_ble_terminal_c`) print scripts for the same three shells from
//! the same kind of table, so `linkr --print-completion zsh` can be dropped
//! next to `linkr_ble_terminal.py --print-completion zsh` and behave.

use std::fmt::Write as _;
use std::thread;
use std::time::{Duration, Instant};

use btleplug::api::{Central, Manager as _, Peripheral as _};

use crate::app::App;
use btleplug::platform::Manager;

/// How a flag's value should be completed, which is also how its value reads
/// in the usage text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    /// The flag takes no value at all.
    None,
    /// The value is chosen from this list, and nothing else is accepted.
    Choice,
    /// A path that must exist or be created: a completion for it is a
    /// *filename* completion, which is not the same thing as a choice list.
    File,
    /// Anything at all. Completing it offers nothing, which is still better
    /// than offering the wrong thing.
    Free,
    /// An optional value, as in `--print-frame` with or without a size.
    Optional,
}

/// One flag the command line understands.
#[derive(Debug, Clone, Copy)]
pub struct Flag {
    pub name: &'static str,
    pub value: Value,
    /// The accepted values, when [`Value::Choice`]; what a completion offers.
    pub choices: &'static [&'static str],
    /// One line, in the imperative-less style the rest of the tool uses.
    pub help: &'static str,
}

impl Flag {
    const fn new(name: &'static str, value: Value, help: &'static str) -> Flag {
        Flag {
            name,
            value,
            choices: &[],
            help,
        }
    }

    const fn with_choices(
        name: &'static str,
        choices: &'static [&'static str],
        help: &'static str,
    ) -> Flag {
        Flag {
            name,
            value: Value::Choice,
            choices,
            help,
        }
    }
}

/// Every flag, in the order the usage text lists them.
///
/// Kept in one place and `const` so it can be walked twice — once to print
/// help, once to print a completion script — and, in the tests, once more to
/// assert that nothing drifts.
pub const FLAGS: &[Flag] = &[
    Flag::new("-h", Value::None, "print this help"),
    Flag::new("--help", Value::None, "print this help"),
    Flag::new("-V", Value::None, "print the version and exit"),
    Flag::new("--version", Value::None, "print the version and exit"),
    Flag::new(
        "--print-completion",
        Value::Choice,
        "print a shell completion script and exit",
    )
    ._choices(&["bash", "zsh", "fish"]),
    // Interface
    Flag::with_choices("--lang", &["zh", "en"], "interface language"),
    Flag::with_choices("--theme", &["dark", "light"], "colour theme"),
    Flag::with_choices(
        "--view",
        &[
            "terminal",
            "diagnostics",
            "network",
            "assistant",
        ],
        "open on this view",
    ),
    Flag::with_choices(
        "--overlay",
        &["palette", "help", "notices", "confirm", "send", "receive"],
        "open on this modal",
    ),
    Flag::with_choices(
        "--state",
        &["disconnected", "connecting", "connected", "failed"],
        "preview this connection state",
    ),
    Flag::new(
        "--print-frame",
        Value::Optional,
        "render one frame as text and exit",
    ),
    Flag::new(
        "--ansi",
        Value::None,
        "with --print-frame, emit colour escapes",
    ),
    Flag::new(
        "--sample",
        Value::None,
        "fill the preview with the sample reply",
    ),
    Flag::new(
        "--feed",
        Value::Free,
        "push text through the emulator before drawing",
    ),
    // Links
    Flag::with_choices(
        "--transport",
        &["ble", "lan", "stdio"],
        "pin the link and lock it",
    ),
    Flag::new(
        "--bridge",
        Value::Free,
        "run the session over a child process; - loops back",
    ),
    Flag::new(
        "--ble-address",
        Value::Free,
        "dial the board at this Bluetooth address",
    ),
    Flag::new(
        "--ble-name",
        Value::Free,
        "dial the board whose name contains this",
    ),
    Flag::new(
        "--lan-host",
        Value::Free,
        "dial the bridge's WebSocket; host, host:port or ws:// URL",
    ),
    Flag::new("--lan-token", Value::Free, "the token that bridge asks for"),
    Flag::new(
        "--lan-token-file",
        Value::File,
        "read that token from a file instead",
    ),
    Flag::new(
        "--scan",
        Value::None,
        "list nearby Bluetooth devices and exit",
    ),
    Flag::new(
        "--timeout",
        Value::Free,
        "seconds to listen for devices while scanning",
    ),
    // Scripting
    Flag::new(
        "--no-terminal",
        Value::None,
        "connect, run the asked-for commands, exit",
    ),
    Flag::new(
        "--cli-then-tui",
        Value::None,
        "run the asked-for commands, then open the terminal",
    ),
    Flag::new(
        "--json",
        Value::None,
        "write events to stdout as JSON lines",
    ),
    Flag::new("--quiet", Value::None, "suppress progress lines on stderr"),
    // The bridge itself
    Flag::new(
        "--query-info",
        Value::None,
        "ask @i? and print what the bridge answers",
    ),
    Flag::new(
        "--query-uart",
        Value::None,
        "ask @u? and print the bridge's UART settings",
    ),
    Flag::new(
        "--query-wifi",
        Value::None,
        "ask @w? and print the WiFi status",
    ),
    Flag::new(
        "--query-webdav",
        Value::None,
        "ask @d? and print the upload status",
    ),
    Flag::new(
        "--uart",
        Value::Free,
        "set UART as baud,data,parity,stop,flow",
    ),
    Flag::new("--wifi", Value::Free, "join a network as SSID[,PASSWORD]"),
    Flag::new(
        "--wifi-key-file",
        Value::File,
        "read that password from the first line of a file",
    ),
    Flag::new("--wifi-off", Value::None, "forget the saved WiFi network"),
    Flag::new(
        "--wifi-scan",
        Value::None,
        "list the networks the board can see",
    ),
    Flag::new(
        "--webdav",
        Value::Free,
        "set the HTTP upload URL for the log",
    ),
    Flag::new("--webdav-off", Value::None, "turn log upload off"),
    Flag::new(
        "--loopback-test",
        Value::Optional,
        "send these bytes and require the same back",
    ),
    Flag::new(
        "--loopback-timeout",
        Value::Free,
        "seconds to wait for that echo",
    ),
    // The terminal
    Flag::with_choices(
        "--enter",
        &["raw", "cr", "lf", "crlf"],
        "translate Enter before sending",
    ),
    Flag::new(
        "--local-echo",
        Value::None,
        "echo typed bytes into the pane locally",
    ),
    Flag::new(
        "--line-mode",
        Value::None,
        "hold a typed line until Enter before sending it",
    ),
    Flag::new(
        "--escape",
        Value::Free,
        "the byte that leaves, as ^] or 0x1d",
    ),
    // Records of the session
    Flag::new(
        "--log-file",
        Value::File,
        "append every byte the device sends to a file",
    ),
    Flag::new("--debug-io", Value::None, "trace both directions on stderr"),
];

impl Flag {
    /// A small builder kept private to the table above, so a choice list can
    /// never be written somewhere the completion script does not read it.
    const fn _choices(mut self, choices: &'static [&'static str]) -> Flag {
        self.value = Value::Choice;
        self.choices = choices;
        self
    }
}

/// The usage text, generated from the table rather than written beside it.
///
/// Two columns like the reference clients': a flag and what it does. Generated
/// because a hand-written block next to a machine-read list is how the two come
/// to disagree — and the disagreement is always in the newest flag, which is
/// the one most people are trying to use.
pub fn usage(prog: &str, about: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{about}");
    let _ = writeln!(out);
    let _ = writeln!(out, "Usage: {prog} [OPTIONS]");
    let _ = writeln!(out);
    let _ = writeln!(out, "Options:");
    let width = FLAGS
        .iter()
        .map(|flag| flag.name.len() + flag.value_width())
        .max()
        .unwrap_or(0);
    for flag in FLAGS {
        let mut rendered = flag.name.to_string();
        match flag.value {
            Value::None => {}
            Value::Optional => {
                rendered.push_str(" [");
                rendered.push_str(flag.value_name());
                rendered.push(']');
            }
            _ => {
                rendered.push(' ');
                rendered.push_str(flag.value_name());
            }
        }
        let _ = writeln!(out, "  {:<width$}  {}", rendered, flag.help, width = width);
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "A LAN token may also come from $LINKR_LAN_TOKEN, or from a token remembered for
that bridge. It is never echoed: anything the bridge says about itself has the
token scrubbed before it reaches the screen, the notices or the log."
    );
    out
}

impl Flag {
    /// How the value reads in a completion script's label.
    fn value_name(&self) -> &'static str {
        match self.value {
            Value::None => "",
            Value::File => "PATH",
            // Named per flag rather than once for all of them: the two
            // optional values are a terminal size and a byte string, and a
            // metavariable that said `COLSxROWS` on `--loopback-test` would be
            // telling whoever is typing it the wrong thing to type.
            Value::Optional => {
                if self.name == "--print-frame" {
                    "COLSxROWS"
                } else {
                    "VALUE"
                }
            }
            _ => {
                if self.name == "--print-completion" {
                    "SHELL"
                } else {
                    "VALUE"
                }
            }
        }
    }

    /// How wide the metavariable is, for aligning the usage text.
    fn value_width(&self) -> usize {
        match self.value {
            Value::None => 0,
            // `[`, `]` and the space before the metavariable.
            Value::Optional => self.value_name().len() + 3,
            _ => self.value_name().len() + 1,
        }
    }

    /// "--ble-write-size" -> "ble write size", the way zsh labels a value.
    fn zsh_label(&self) -> String {
        if self.value == Value::None {
            return String::new();
        }
        let trimmed = self.name.trim_start_matches('-');
        if trimmed.is_empty() {
            return self.value_name().to_string();
        }
        trimmed.replace('-', " ")
    }
}

/// The shells a completion script can be printed for.
pub const COMPLETION_SHELLS: [&str; 3] = ["bash", "zsh", "fish"];

/// Print a completion script for `shell`, or `None` if it is not one we know.
///
/// Generated from [`FLAGS`] at run time so a new flag completes itself: nothing
/// here has to be edited when one is added, which is the entire reason the
/// table exists.
pub fn completion_script(shell: &str, prog: &str) -> Option<String> {
    match shell {
        "bash" => Some(bash_completion(prog)),
        "zsh" => Some(zsh_completion(prog)),
        "fish" => Some(fish_completion(prog)),
        _ => None,
    }
}

/// Escape one single-quoted string for a shell script — the quoting every one
/// of these three shells uses for a literal.
fn shell_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for ch in text.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// The function name a completion serves under: one per program name, so two
/// builds installed side by side do not fight over `_linkr`.
fn function_name(prog: &str) -> String {
    let mut name = String::from("_");
    for ch in prog.chars() {
        name.push(if ch.is_ascii_alphanumeric() { ch } else { '_' });
    }
    name
}

fn bash_completion(prog: &str) -> String {
    let function = function_name(prog);
    let mut out = String::new();
    out.push_str("# bash completion for ");
    out.push_str(prog);
    out.push('\n');
    // Built by concatenation rather than by a format string, because a bash
    // completion is mostly `${...}` and `$(...)` — and escaping every brace to
    // get them past a formatter is how a script becomes unreadable and then
    // subtly wrong.
    out.push_str(&format!("{function}() {{\n"));
    out.push_str("    local cur opts\n");
    out.push_str("    cur=\"${COMP_WORDS[COMP_CWORD]}\"\n");
    out.push_str("    opts=\"");
    for flag in FLAGS {
        out.push(' ');
        out.push_str(flag.name);
    }
    out.push_str(" \"\n");
    // A `--flag=<partial>` completes to a filename where a filename belongs,
    // which is the useful default rather than silence.
    out.push_str("    case \"$cur\" in\n");
    for flag in FLAGS {
        if flag.value == Value::File {
            out.push_str(&format!(
                "        {}*) COMPREPLY=( $(compgen -f -- \"${{cur#{}=}}\") ); return 0 ;;\n",
                flag.name, flag.name
            ));
        }
    }
    out.push_str("    esac\n");
    out.push_str("    COMPREPLY=( $(compgen -W \"$opts\" -- \"$cur\") )\n");
    out.push_str("    return 0\n");
    out.push_str("}\n");
    out.push_str(&format!("complete -F {function} {prog}\n"));
    out
}

fn zsh_completion(prog: &str) -> String {
    let function = function_name(prog);
    let mut out = String::new();
    out.push_str("#compdef ");
    out.push_str(prog);
    out.push('\n');
    out.push_str(&format!("# zsh completion for {prog}\n"));
    out.push_str(&format!("{function}() {{\n"));
    out.push_str("    local -a flags\n");
    out.push_str("    flags=(\n");
    for flag in FLAGS {
        let name = shell_quote(flag.name);
        match flag.value {
            Value::None => {
                out.push_str(&format!("        {}:{}\n", name, shell_quote(flag.help)));
            }
            Value::Choice => {
                let choices: Vec<String> = flag.choices.iter().map(|c| shell_quote(c)).collect();
                out.push_str(&format!(
                    "        {}[{}]:{}:({})\n",
                    name,
                    shell_quote(flag.help),
                    flag.zsh_label(),
                    choices.join(" ")
                ));
            }
            Value::File => {
                out.push_str(&format!(
                    "        {}[{}]:{}:_files\n",
                    name,
                    shell_quote(flag.help),
                    flag.zsh_label()
                ));
            }
            Value::Free | Value::Optional => {
                out.push_str(&format!(
                    "        {}[{}]:{}:\n",
                    name,
                    shell_quote(flag.help),
                    flag.zsh_label()
                ));
            }
        }
    }
    out.push_str("    )\n");
    out.push_str("    _arguments $flags\n");
    out.push_str("}}\n");
    out.push_str(&format!("compdef {function} {prog}\n"));
    out
}

fn fish_completion(prog: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# fish completion for {prog}");
    for flag in FLAGS {
        // fish spells a short and a long flag differently — `h` is `-s h` and
        // `help` is `-l help` — and getting that wrong is a completion that
        // silently offers nothing rather than one that fails loudly.
        let (switch, condition) = if flag.name.starts_with("--") {
            (
                format!("-l {}", &flag.name[2..]),
                format!("-l {}", &flag.name[2..]),
            )
        } else {
            (
                format!("-s {}", &flag.name[1..]),
                format!("-s {}", &flag.name[1..]),
            )
        };
        let takes_value = match flag.value {
            Value::None => "",
            // `-r` requires an argument; without it fish completes the next
            // word as another flag, and a value typed after the flag is
            // completed into an error.
            _ => " -r",
        };
        let _ = writeln!(
            out,
            "complete -c {prog} {switch}{takes_value} -d {}",
            shell_quote(flag.help)
        );
        for choice in flag.choices {
            let _ = writeln!(
                out,
                "complete -c {prog} -n '__fish_seen_argument {condition}' -a {} -d {}",
                shell_quote(choice),
                shell_quote(choice)
            );
        }
    }
    out
}

/// Read an escape byte, as `^]`, as `0x1d`, or as the byte itself.
///
/// The caret form is `^` followed by the character you would press with
/// Control held: `^]` is Control-`]`, which is `0x1d`. It is the form the
/// reference client takes and the one everybody already knows from telnet, and
/// it exists because `0x1d` is not a thing anyone can remember.
///
/// A lone `^` is refused rather than read as a literal: it is unambiguously
/// the start of the caret form, and reading it as a byte would quietly make
/// `--escape ^` mean "leave when I type a caret".
pub fn parse_escape(value: &str) -> Result<u8, String> {
    const WRONG: &str = "escape must be one byte, like ^] or 0x1d";
    if value == "^" {
        return Err(WRONG.to_string());
    }
    if let Some(letter) = value.strip_prefix('^') {
        let mut chars = letter.chars();
        let Some(only) = chars.next() else {
            return Err(WRONG.to_string());
        };
        if chars.next().is_some() || !only.is_ascii() {
            return Err(WRONG.to_string());
        }
        // Control-<letter> is the letter's code with its two high bits
        // cleared, which is why `^]` and `^_` land where they do and why the
        // form cannot express every byte — `^@` is NUL and nothing else.
        return Ok(only as u8 & 0x1f);
    }
    if let Some(digits) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        let parsed = u32::from_str_radix(digits, 16)
            .map_err(|_| "hex escape must be between 0x00 and 0xff".to_string())?;
        if parsed > 0xff {
            return Err("hex escape must be between 0x00 and 0xff".to_string());
        }
        return Ok(parsed as u8);
    }
    let raw = value.as_bytes();
    if raw.len() != 1 {
        return Err(WRONG.to_string());
    }
    Ok(raw[0])
}

/// One device seen while scanning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundDevice {
    /// The advertised name, when it advertised one.
    pub name: Option<String>,
    /// How this platform identifies it: a MAC on BlueZ, a UUID on
    /// CoreBluetooth. Either is accepted by `--ble-address`, which is why both
    /// are shown.
    pub id: String,
    /// Whether it offers the Nordic UART Service, which is the only kind this
    /// program can dial.
    pub nus: bool,
}

/// Listen for `timeout` and report every device that answered.
///
/// The scan is the one command that answers a question nothing else can — "is
/// the board on the air at all" — so it runs before anything is dialled and
/// prints even the devices it cannot use: a Linkr Bee that is advertising but
/// not offering the serial service is a different problem from one that is
/// switched off, and the list is how you tell them apart.
pub fn scan_devices(timeout: std::time::Duration) -> Result<Vec<FoundDevice>, String> {
    // Its own reactor on its own thread, for the same reason the BLE link
    // builds one: this program is synchronous and the radio library is not.
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(why) => return Err(format!("the scan could not be started: {why}")),
        };
        runtime.block_on(async move {
            let manager = match Manager::new().await {
                Ok(manager) => manager,
                Err(why) => return Err(format!("no Bluetooth manager could be opened: {why}")),
            };
            let adapters = match manager.adapters().await {
                Ok(adapters) => adapters,
                Err(why) => return Err(format!("no Bluetooth adapter could be listed: {why}")),
            };
            let Some(adapter) = adapters.into_iter().next() else {
                return Err(
                    "this machine has no Bluetooth adapter — one is needed to look for the board"
                        .to_string(),
                );
            };
            adapter
                .start_scan(btleplug::api::ScanFilter::default())
                .await
                .map_err(|why| format!("the scan would not start: {why}"))?;
            tokio::time::sleep(timeout).await;

            let mut found = Vec::new();
            for candidate in adapter
                .peripherals()
                .await
                .map_err(|why| format!("the scan's results could not be read: {why}"))?
            {
                // A device that has just gone out of range cannot introduce
                // itself; that is not interesting and must not end the scan.
                let Ok(Some(properties)) = candidate.properties().await else {
                    continue;
                };
                found.push(FoundDevice {
                    name: properties.local_name,
                    id: candidate.id().to_string(),
                    nus: properties
                        .services
                        .contains(&crate::transport::ble::NUS_SERVICE),
                });
            }
            let _ = adapter.stop_scan().await;
            found.sort_by(|left, right| {
                right
                    .nus
                    .cmp(&left.nus)
                    .then_with(|| left.name.cmp(&right.name))
            });
            Ok(found)
        })
    })
    .join()
    .map_err(|_| "the scan's own thread panicked".to_string())?
}

/// What the command line asked the bridge to do instead of opening a terminal.
///
/// Empty unless a flag named one of these, which is how a script run is told
/// apart from a plain launch: nobody asked for anything to be *done*, so there
/// is nothing to exit on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Requests {
    /// Reads, in the order they were asked for.
    pub queries: Vec<crate::device::Query>,
    /// `@u=<spec>`, when `--uart` named one.
    pub uart: Option<String>,
    /// `@w=<ssid>,<password>`, when `--wifi` named one.
    pub wifi: Option<(String, String)>,
    /// `@w off`.
    pub wifi_off: bool,
    /// `@w scan`.
    pub wifi_scan: bool,
    /// `@d=<url>`.
    pub webdav: Option<String>,
    /// `@d off`.
    pub webdav_off: bool,
}

impl Requests {
    /// Whether anything at all was asked for.
    pub fn is_empty(&self) -> bool {
        self.queries.is_empty()
            && self.uart.is_none()
            && self.wifi.is_none()
            && !self.wifi_off
            && !self.wifi_scan
            && self.webdav.is_none()
            && !self.webdav_off
    }
}

/// How long an answer is waited for, which is not the same for every question.
///
/// A WiFi scan is the reason this exists. Everything else is answered out of
/// the bridge's own state in a few milliseconds, but a scan is the board's
/// radio listening on every channel in turn — the reference client gives it
/// minutes, and a client that gave it the ordinary reply timeout would report
/// "the bridge did not answer" for a scan that was simply still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Patience {
    /// The ordinary management reply timeout.
    Normal,
    /// Long enough for a scan of every 2.4 GHz channel.
    Scan,
    /// Waiting for a dialled link to finish coming up.
    Connect,
}

impl Patience {
    /// How long this waits, as a duration.
    pub fn timeout(self) -> Duration {
        match self {
            Patience::Normal => crate::device::REPLY_TIMEOUT,
            // Matches the reference client's `WIFI_SCAN_TIMEOUT`.
            Patience::Scan => Duration::from_secs(90),
            // The BLE transport itself gives a dial twenty seconds before
            // calling the link dead; this waits beside the same clock rather
            // than inventing a second one.
            Patience::Connect => Duration::from_secs(20),
        }
    }
}

/// How a script run reports what it saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Lines a person can read, the shape the reference client prints.
    Text,
    /// One JSON object per event, one per line.
    Json,
}

/// Run every request, wait for each answer, and report.
///
/// This is `--no-terminal`: connect, do what was asked, hang up. It exists
/// because a bridge is configured far more often than it is talked to — and
/// configuring one by opening a terminal, typing `@u=115200,8,n,1,none` and
/// reading the reply off a screen is how a value gets mistyped and then
/// scripted wrong.
///
/// Each request is sent and *waited for* rather than fired off together,
/// because the bridge answers one question at a time and two outstanding ones
/// would have two `OK`s arriving in no order anyone can rely on.
pub fn run_requests(app: &mut App, requests: &Requests, format: Format) -> i32 {
    app.collect_output = true;
    // The dial opened the link; the session reports itself connected a pump
    // later. Asking in between came back "not connected — nothing was sent"
    // for a link that was up a moment after the question, which is a wrong
    // answer about the one state the user had just watched come up.
    wait_for_connected(app);
    let mut failed = 0;

    for query in &requests.queries {
        match app.ask_device(*query) {
            Err(why) => failed += report(format, query_name(*query), false, &why),
            Ok(()) => failed += wait_and_report(app, *query, format),
        }
    }
    // A write is followed by the read that confirms it, so what gets reported
    // is what the bridge is doing and not what it was asked to do — see
    // `App::set_uart`.
    if let Some(spec) = &requests.uart {
        match parse_uart_spec(spec).and_then(|settings| app.set_uart(&settings)) {
            Err(why) => failed += report(format, "uart", false, &why),
            Ok(()) => failed += wait_and_report(app, crate::device::Query::Uart, format),
        }
    }
    if requests.wifi_off || requests.wifi.is_some() {
        let (ssid, password) = requests
            .wifi
            .as_ref()
            .map_or(("", ""), |pair| (pair.0.as_str(), pair.1.as_str()));
        match app.set_wifi(ssid, password) {
            Err(why) => failed += report(format, "wifi", false, &why),
            Ok(()) => failed += wait_and_report(app, crate::device::Query::Wifi, format),
        }
    }
    if requests.webdav_off || requests.webdav.is_some() {
        let url = requests.webdav.as_deref().unwrap_or("");
        match app.set_webdav(url) {
            Err(why) => failed += report(format, "webdav", false, &why),
            Ok(()) => failed += wait_and_report(app, crate::device::Query::Webdav, format),
        }
    }
    if requests.wifi_scan {
        match app.scan_wifi() {
            Err(why) => failed += report(format, "wifi-scan", false, &why),
            // The one request given the long timeout: a scan is the board's
            // radio working through every channel, and the ordinary reply
            // timeout would call a running scan a silent bridge.
            Ok(()) => {
                failed += wait_and_report_with(
                    app,
                    crate::device::Query::WifiScan,
                    format,
                    Patience::Scan,
                )
            }
        }
    }

    // What the target said while all this was going on, printed last so the
    // answers are above it: the output is a by-product here, the answers are
    // what was asked for.
    drain(app, format);
    failed
}

/// Send `payload` and require the same bytes back.
///
/// This is the wiring test, and it is the only check that proves the round trip
/// rather than one half of it: a board that accepts writes and never sends
/// anything back looks healthy to every other command here, because every
/// other command is answered by the bridge and not by the target. The
/// loopback is answered by the target's own UART with its TX and RX joined,
/// which is the thing that has to work before any of this is useful.
///
/// The payload is matched as a *substring* of what came back, because the
/// target may well have echoed it between other output — a console that is
/// printing a prompt is still a working wire.
pub fn loopback_test(app: &mut App, payload: &[u8], timeout: Duration, format: Format) -> bool {
    app.collect_output = true;
    // Anything already in flight belongs to whatever happened before the test,
    // and counting it as the echo would let a board pass without answering.
    app.take_output();
    // An empty payload would match at once — a window of length zero is found
    // at every position, including before anything arrived — and a test that
    // passes on silence is worse than no test.
    if payload.is_empty() {
        let _ = report(format, "loopback", false, "the payload is empty");
        return false;
    }
    if !app.send_bytes(payload) {
        let _ = report(format, "loopback", false, app.strings().not_connected);
        return false;
    }

    let mut seen: Vec<u8> = Vec::new();
    let deadline = Instant::now() + timeout;
    loop {
        app.pump();
        for chunk in app.take_output() {
            seen.extend_from_slice(&chunk);
        }
        if seen.windows(payload.len()).any(|window| window == payload) {
            let _ = report(format, "loopback", true, &crate::trace::debugged(payload));
            return true;
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let _ = report(
        format,
        "loopback",
        false,
        &format!(
            "timed out after {} s; heard {}",
            timeout.as_secs_f64(),
            crate::trace::debugged(&seen)
        ),
    );
    false
}

/// Pump until a dialled link is connected enough to be asked things.
///
/// The wait is bounded by [`Patience::Connect`]. A link that never gets there
/// is not this loop's to report: every request that follows says for itself
/// that nothing went out, which is the report a user can act on.
fn wait_for_connected(app: &mut App) {
    let deadline = Instant::now() + Patience::Connect.timeout();
    while app.session.is_live() && !app.connected() && Instant::now() < deadline {
        app.pump();
        thread::sleep(Duration::from_millis(5));
    }
}

/// Wait for one answer and print what it said. Returns 1 when it failed.
fn wait_and_report(app: &mut App, query: crate::device::Query, format: Format) -> i32 {
    wait_and_report_with(app, query, format, Patience::Normal)
}

/// As [`wait_and_report`], for a question the bridge takes longer to answer.
fn wait_and_report_with(
    app: &mut App,
    query: crate::device::Query,
    format: Format,
    patience: Patience,
) -> i32 {
    // Waited for here and not inside the send, because an answer arrives on a
    // later pump than the question that asked for it — and leaving before it
    // lands is how a script reports "the bridge said nothing" when in fact it
    // was simply not given time.
    if !settle(app, format, patience) {
        let why = app
            .diagnostics
            .error
            .clone()
            .or_else(|| app.network.error.clone())
            .unwrap_or_else(|| "the bridge did not answer".to_string());
        return report(format, query_name(query), false, &why);
    }
    report_query(app, query, format)
}

/// Pump until the question asked has been answered, or until the conversation
/// itself gives up. Returns whether an answer arrived.
fn settle(app: &mut App, format: Format, patience: Patience) -> bool {
    let deadline =
        std::time::Instant::now() + patience.timeout() + std::time::Duration::from_millis(500);
    while app.waiting_on_device() {
        if std::time::Instant::now() > deadline {
            return false;
        }
        app.pump();
        drain(app, format);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // One more pump: an answer may have arrived on the tick that ended the
    // wait, and a reply known about but not folded in is a reply the report
    // would call missing.
    app.pump();
    drain(app, format);
    true
}

/// Print whatever the target said while the bridge was being asked things.
fn drain(app: &mut App, format: Format) {
    for chunk in app.take_output() {
        match format {
            Format::Text => print_raw(&chunk),
            Format::Json => println!(
                "{}",
                json_line(&[("event", "output"), ("text", &lossy(&chunk))])
            ),
        }
    }
}

fn print_raw(bytes: &[u8]) {
    use std::io::Write as _;
    let out = std::io::stdout();
    let mut lock = out.lock();
    let _ = lock.write_all(bytes);
    let _ = lock.flush();
}

fn query_name(query: crate::device::Query) -> &'static str {
    match query {
        crate::device::Query::Diagnostics => "info",
        crate::device::Query::Uart => "uart",
        crate::device::Query::Wifi => "wifi",
        crate::device::Query::WifiScan => "wifi-scan",
        crate::device::Query::Webdav => "webdav",
        crate::device::Query::Lan => "lan",
    }
}

/// Print one request's outcome. Returns 1 when it failed, so the exit code
/// counts failures rather than merely mentioning them.
fn report(format: Format, what: &str, ok: bool, detail: &str) -> i32 {
    match format {
        Format::Text => {
            if detail.is_empty() {
                println!("{what}: ok");
            } else {
                println!("{what}: {detail}");
            }
        }
        Format::Json => println!(
            "{}",
            json_line(&[
                ("event", "result"),
                ("what", what),
                ("ok", &ok.to_string()),
                ("detail", detail),
            ])
        ),
    }
    if ok {
        0
    } else {
        1
    }
}

/// Print what a read came back with.
fn report_query(app: &App, query: crate::device::Query, format: Format) -> i32 {
    let ok = match query {
        crate::device::Query::Diagnostics => app.diagnostics.error.is_none(),
        _ => app.network.error.is_none(),
    };
    let detail = match query {
        crate::device::Query::Diagnostics => app.diagnostics.raw.join("\n"),
        crate::device::Query::Uart => app
            .baud
            .map(|baud| format!("uart={baud}"))
            .unwrap_or_default(),
        crate::device::Query::Wifi => match &app.network.wifi {
            Some(wifi) => format!("wifi={},ssid={},ip={}", wifi.state, wifi.ssid, wifi.ip),
            None => String::new(),
        },
        // One network per line, in the order the bridge heard them. The scan
        // stream's own lines (`OK accepted`, `@scan done`) are already off the
        // list — see `App`'s `scan_results`. A scan that failed while it was
        // running has no list to show, and the words it failed with are the
        // report a script can act on.
        crate::device::Query::WifiScan => {
            if app.network.scan_results.is_empty() {
                app.network.error.clone().unwrap_or_default()
            } else {
                app.network.scan_results.join("\n")
            }
        }
        crate::device::Query::Webdav => match &app.network.webdav {
            Some(webdav) => format!("webdav={},url={}", webdav.state, webdav.url),
            None => String::new(),
        },
        // The state is reported; the token never is. A CLI's output goes to a
        // pipe and a pipe goes to a log, and a token in a log is a token that
        // has to be rotated.
        crate::device::Query::Lan => {
            if app.lan_status.ws.is_empty() {
                String::new()
            } else {
                format!(
                    "ws={},token={}",
                    app.lan_status.ws,
                    if app.lan_status.token.is_empty() {
                        "none"
                    } else {
                        "captured"
                    }
                )
            }
        }
    };
    match format {
        Format::Text => {
            if detail.is_empty() {
                println!(
                    "{}: {}",
                    query_name(query),
                    if ok { "ok" } else { "failed" }
                );
            } else {
                for line in detail.lines() {
                    println!("{}: {}", query_name(query), line);
                }
            }
        }
        Format::Json => println!(
            "{}",
            json_line(&[
                ("event", "result"),
                ("what", query_name(query)),
                ("ok", &ok.to_string()),
                ("reply", &detail.replace('\n', "\\n")),
            ])
        ),
    }
    if ok {
        0
    } else {
        1
    }
}

/// Parse `baud,data,parity,stop,flow` the way `--uart` takes it.
///
/// Refused here rather than by the bridge, for the same reason
/// `accessory::set_uart_command` refuses: a value the firmware cannot take is a
/// value the user should be told about before a round trip and not after.
pub fn parse_uart_spec(spec: &str) -> Result<crate::accessory::UartSettings, String> {
    let fields: Vec<&str> = spec.split(',').map(str::trim).collect();
    if fields.len() != 5 {
        return Err(format!(
            "'{spec}' is not baud,data,parity,stop,flow — five fields, as in 115200,8,n,1,none"
        ));
    }
    let baud: u32 = fields[0]
        .parse()
        .map_err(|_| format!("'{}' is not a baud rate", fields[0]))?;
    let data_bits: u8 = fields[1]
        .parse()
        .map_err(|_| format!("'{}' is not a data-bit count", fields[1]))?;
    let stop_bits: u8 = fields[3]
        .parse()
        .map_err(|_| format!("'{}' is not a stop-bit count", fields[3]))?;
    Ok(crate::accessory::UartSettings {
        baud,
        data_bits,
        parity: fields[2].to_string(),
        stop_bits,
        flow: fields[4].to_string(),
    })
}

/// One JSON object on one line, escaped by hand.
///
/// Written rather than brought in as a dependency for two reasons: the payload
/// is five fields of known shape, and a crate that would let a bridge's answer
/// become malformed JSON is a crate that has to be trusted with a device's
/// output for no gain.
fn json_line(fields: &[(&str, &str)]) -> String {
    let mut out = String::from("{");
    for (index, (key, value)) in fields.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(&escape(key));
        out.push_str("\":\"");
        out.push_str(&escape(value));
        out.push('"');
    }
    out.push('}');
    out
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Bytes as text, replacing what cannot be read rather than failing: a JSON
/// line that could not be produced is worse than one with a replacement in it.
fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A completion that offers a flag the parser has never heard of completes
    /// a command line into an error, and a flag the parser knows but the
    /// completion does not is a flag nobody will ever find. The two lists are
    /// the same list here, but the *point* of the test is the second half:
    /// adding a flag to `main.rs` and forgetting the table is the mistake this
    /// catches.
    #[test]
    fn every_flag_the_parser_matches_on_has_a_completion() {
        let source = include_str!("main.rs");
        for flag in FLAGS {
            let needle = format!("\"{}\"", flag.name);
            assert!(
                source.contains(&needle),
                "{} is advertised but never parsed: it would tab-complete into an error",
                flag.name
            );
        }
        // And the other way round: a literal flag handled by the parser that is
        // absent from the table cannot be completed.
        for quoted in quoted_flag_literals(source) {
            assert!(
                FLAGS.iter().any(|flag| flag.name == quoted),
                "{quoted} is parsed but never advertised"
            );
        }
    }

    /// The flag literals a hand-written parser says it accepts. Read from the
    /// source rather than from a second list, because a second list is what the
    /// drift would be made of.
    fn quoted_flag_literals(source: &str) -> Vec<String> {
        let mut found = Vec::new();
        for line in source.lines() {
            let trimmed = line.trim();
            // The parser's own shape: `"--name" => …`, or `"-h" | "--help" =>`.
            if !trimmed.starts_with('"') {
                continue;
            }
            for token in trimmed.split(" | ") {
                let literal = token.trim().trim_start_matches('"');
                if !literal.starts_with("--") && !literal.starts_with("-[a-zA-Z]") {
                    continue;
                }
                let end = literal.find('"').unwrap_or(literal.len());
                let name = &literal[..end];
                if name.starts_with('-') && (name.len() > 2 || name != "-") {
                    found.push(name.to_string());
                }
            }
        }
        found.sort();
        found.dedup();
        found
    }

    /// Each shell gets a script that names every flag, which is what "complete"
    /// means — and each names the program it was asked about, so two builds
    /// installed under different names do not overwrite each other's
    /// completion.
    #[test]
    fn each_shell_names_every_flag_and_the_program_it_serves() {
        for shell in COMPLETION_SHELLS {
            let script = completion_script(shell, "linkr").expect("a script");
            for flag in FLAGS {
                // fish spells the dashes separately: `--lang` is offered as
                // `-l lang`, and `-h` as `-s h`. The flag is still the flag,
                // which is what this is checking.
                let needle = if shell == "fish" {
                    let (switch, bare) = flag
                        .name
                        .split_at(flag.name.len() - flag.name.trim_start_matches('-').len());
                    let long = switch.starts_with("--");
                    format!("-{} {}", if long { 'l' } else { 's' }, bare)
                } else {
                    flag.name.to_string()
                };
                assert!(
                    script.contains(&needle),
                    "{shell} does not offer {}: {script}",
                    flag.name
                );
            }
            assert!(script.contains("linkr"), "{shell} never names the program");
            // fish has no function of its own: `complete -c` is all it needs,
            // which is why it is the one shell with nothing to name.
            if shell != "fish" {
                assert!(
                    script.contains("_linkr"),
                    "{shell} misses its function name"
                );
            }
        }
        assert!(completion_script("csh", "linkr").is_none());
    }

    /// A program name that is not a shell identifier must not produce a broken
    /// function name: `linkr-tui` would otherwise be completed by a function
    /// nobody can define.
    #[test]
    fn a_program_with_a_dash_still_gets_a_usable_function_name() {
        let script = completion_script("bash", "linkr-tui").expect("a script");
        assert!(script.contains("_linkr_tui() {"), "{script}");
        assert!(script.contains("complete -F _linkr_tui linkr-tui"));
    }

    /// The escape byte is read in every form the reference client takes, and
    /// a lone caret is refused: it is unambiguously the start of the `^X` form,
    /// and reading it as a byte would make `--escape ^` mean "leave when I type
    /// a caret".
    #[test]
    fn an_escape_byte_is_read_in_every_form_the_reference_client_takes() {
        assert_eq!(parse_escape("^]").expect("^]"), 0x1d);
        assert_eq!(parse_escape("^C").expect("^C"), 0x03);
        assert_eq!(
            parse_escape("^c").expect("lowercase is the same chord"),
            0x03
        );
        assert_eq!(parse_escape("0x1d").expect("hex"), 0x1d);
        assert_eq!(parse_escape("]").expect("the byte itself"), b']');
        assert!(parse_escape("^").is_err(), "a lone caret is not a byte");
        assert!(parse_escape("0x1ff").is_err(), "one byte only");
        assert!(parse_escape("0xzz").is_err(), "hex that is not hex");
        assert!(parse_escape("ab").is_err(), "two characters is not a byte");
    }

    /// The default is the byte the reference client reserves, so `--escape` is
    /// a change and not a requirement.
    #[test]
    fn the_default_escape_is_the_one_the_reference_client_uses() {
        assert_eq!(parse_escape("^]").expect("parses"), 0x1d);
    }

    /// The loopback is the only check that proves the round trip: everything
    /// else here is answered by the bridge, and a board that accepts writes and
    /// never sends anything back looks healthy to all of it.
    #[test]
    fn a_loopback_passes_when_the_target_echoes_and_fails_when_it_does_not() {
        let (mut app, wire) = connected_app();
        // The device's TX and RX joined, which is what a loopback plug is.
        let plug = std::thread::spawn(move || {
            for _ in 0..400 {
                let sent = wire.take_sent();
                if !sent.is_empty() {
                    wire.feed(&sent);
                    return;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        assert!(
            loopback_test(&mut app, b"A", Duration::from_secs(2), Format::Text),
            "the same byte came back"
        );
        let _ = plug.join();

        // Silence is a failure. The timeout is short and the pass above is
        // long, so this cannot be a test that merely ran out of clock.
        let (mut app, _wire) = connected_app();
        assert!(
            !loopback_test(&mut app, b"A", Duration::from_millis(150), Format::Text),
            "nothing came back"
        );
        // An empty payload can never pass: a window of length zero is found
        // before anything arrives, so a test that accepted it would be one
        // that passes on silence.
        assert!(!loopback_test(
            &mut app,
            b"",
            Duration::from_millis(50),
            Format::Text
        ));
    }

    /// An app on the end of an in-memory link, which is what a script run is
    /// driven by here: no radio, and the pump is pumped by hand.
    fn connected_app() -> (App, crate::transport::testing::MemoryLink) {
        let mut app = App::default();
        let wire = crate::transport::testing::MemoryLink::new();
        app.connect(wire.link()).expect("the memory link opens");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !app.connected() && Instant::now() < deadline {
            app.pump();
            thread::sleep(Duration::from_millis(2));
        }
        assert!(app.connected(), "the memory link opens");
        (app, wire)
    }

    /// A dialled link is not asked anything until the session says it is
    /// connected. The bug this pins: a script run sent its first request
    /// between the dial and the session's own connected state, and the answer
    /// was "not connected — nothing was sent" for a link that was up a moment
    /// later.
    #[test]
    fn a_dialled_link_is_waited_for_before_it_is_asked() {
        let mut app = App::default();
        let wire = crate::transport::testing::MemoryLink::new();
        app.connect(wire.link()).expect("the memory link opens");
        assert!(
            !app.connected(),
            "the state arrives with a pump, not with the dial"
        );
        wait_for_connected(&mut app);
        assert!(app.connected(), "and the wait was what brought it");
    }

    /// Values the shell could offer are listed once, so `--transport` completes
    /// to the three links that exist and nothing invented.
    #[test]
    fn choices_are_offered_everywhere_a_value_is_chosen() {
        let transport = FLAGS
            .iter()
            .find(|flag| flag.name == "--transport")
            .expect("documented");
        assert_eq!(transport.choices, &["ble", "lan", "stdio"]);
        let zsh = completion_script("zsh", "linkr").expect("script");
        assert!(zsh.contains("('ble' 'lan' 'stdio')"), "{zsh}");
    }

    /// Help and completion come from the same table, so they cannot disagree
    /// about what a flag is called — and every flag appears in the usage text.
    #[test]
    fn usage_lists_the_same_flags_the_completion_does() {
        let text = usage("linkr", "about line");
        for flag in FLAGS {
            assert!(
                text.contains(flag.name),
                "usage omits {}: {text}",
                flag.name
            );
            assert!(
                text.contains(flag.help),
                "usage omits {}\u{2019}s help",
                flag.name
            );
        }
        assert!(text.starts_with("about line"));
        assert!(text.contains("Usage: linkr [OPTIONS]"));
    }

    /// Words with a quote in them must not end the string early: a flag's help
    /// is a quoted string in three different shells and a parenthesis is legal
    /// in every one of them.
    #[test]
    fn a_value_with_an_apostrophe_survives_being_quoted() {
        assert_eq!(shell_quote("don't"), "'don'\\''t'");
        assert_eq!(shell_quote("plain"), "'plain'");
    }
}
