# Linkr Bee terminal tutorial

> [Project home](../README.md) · [中文](TUTORIAL.zh-CN.md) ·
> [Documentation index](README.md) ·
> [Rust terminal reference](../tools/linkr-cli/README.md)

This is a hands-on guide to `linkr`, the terminal client: how to install it,
open the TUI, and drive every screen from the keyboard. It assumes you already
have a Linkr Bee accessory flashed and wired — the
[README](../README.md#getting-started) covers flashing, wiring and pairing in
five steps.

Every screen exists in English and Chinese; [step 12](#12-switch-the-language)
shows how to switch.

## 1. What you get

`linkr` is one binary with two personalities:

- **The TUI** — an OpenCode-style interface with a sidebar, four switchable
  views, a command palette and modal dialogs. All of it is keyboard-driven.
- **Plain CLI mode** — same flags as the Python client, for scripts and CI:
  `linkr --scan`, `linkr --query-info --no-terminal`, `linkr --wifi …`.

```text
┌─────────────┬──────────────────────────────────────────────┐
│ Connection  │                                              │
│ ● Connected │              the active view                 │
│ Transport   │         (F2 terminal / F3 diagnostics /      │
│ Device      │          F4 network / F5 assistant)          │
│             │                                              │
│ Quick send  │                                              │
│             ├──────────────────────────────────────────────┤
│ Watch       │ [Focus] View · detail   Ctrl+P · F1 · Ctrl+Q │
└─────────────┴──────────────────────────────────────────────┘
```

## 2. Install

### Build from source

```sh
tools/build_terminal.sh          # fmt + clippy + tests, then a release build
dist/linkr-terminal-linux-aarch64/linkr --version
```

The script runs the whole verification suite first and refuses to package a
tree that fails it. `--target <triple>` cross-builds, `--no-verify` skips the
checks (use it only while iterating).

### Get a prebuilt binary

Pushing a `v*` tag makes CI publish everything it built to the repository's
**Releases** page, where the files stay put:

```
https://github.com/radxa/linkr-bee/releases/latest
```

Linux ships as `linkr-terminal-<slug>.tar.gz`, Windows as
`linkr-terminal-<slug>.zip`, and each unpacks to the same folder
`tools/build_terminal.sh` produces locally, so the check is the one you would
run on a local build; a `SHA256SUMS` at the top of the release covers every
file on the page:

```sh
tar xzf linkr-terminal-linux-aarch64.tar.gz
cd linkr-terminal-linux-aarch64
sha256sum -c SHA256SUMS
chmod +x linkr
./linkr --version
```

```powershell
Expand-Archive .\linkr-terminal-x86_64-pc-windows-gnu.zip
cd linkr-terminal-x86_64-pc-windows-gnu
(Get-FileHash .\linkr.exe -Algorithm SHA256).Hash   # must match SHA256SUMS
.\linkr.exe --version
```

**Antivirus note.** Take the `.zip` first: it is a plain archive with
`linkr.exe` inside, and nothing in it looks like malware. The alternative
`linkr-bee-terminal.ps1` is self-extracting — the exe is embedded as base64,
decoded and started by PowerShell — which is exactly the dropper pattern
antivirus looks for, so it is often quarantined even though the payload is the
same binary. If you want to use it anyway, add it to your antivirus trust list
and confirm the SHA-256 above. The executable is not code-signed, so Windows
may report it as an "unknown publisher"; that is a signature status, not a
detection.

Until something has been tagged, CI (`.github/workflows/build.yml`, job
`terminal`) still uploads the binaries for six targets — Linux x86_64, i686,
arm64 and armv7, and Windows x86_64 and i686 — as artifacts on the workflow
run. macOS is deliberately not among them: nothing in this repository has ever
been run on one, and shipping an untested binary would be a promise nobody can
keep. Those artifacts expire after 30 days, which is why the Releases page is
the link to keep; the check is the same, minus the `tar` line.

### Windows

```powershell
powershell -ExecutionPolicy Bypass -File tools\build_terminal.ps1 -Bundle
```

That produces `dist\linkr-bee-terminal.ps1`, a single self-extracting file with
`linkr.exe` inside. It unpacks once into `%LOCALAPPDATA%\LinkrBee\bin`,
verifies the embedded SHA-256 before every run, and forwards your arguments and
the exit code:

```powershell
.\dist\linkr-bee-terminal.ps1 --scan
.\dist\linkr-bee-terminal.ps1 --tui
```

Because the bundle is self-extracting, antivirus may flag it as described in
section 2 — build it only if you are going to use it.

Full detail: [Rust terminal reference](../tools/linkr-cli/README.md#build).

## 3. Your first session

**1. Find the accessory.** It broadcasts as `Linkr BLE UART…`:

```sh
linkr --scan
```

**2. Open the TUI.** Without a `--name` it connects to the first device that
matches `Linkr BLE UART*`:

```sh
linkr --tui
```

Useful variants:

```sh
linkr --name "Linkr BLE UART-3" --tui      # pick one accessory
linkr --address AA:BB:CC:DD:EE:FF --tui    # skip the name scan entirely
linkr --uart 115200,8,n,1,n --tui          # set the UART before handing over
linkr --query-info --tui                   # show @i? diagnostics on the way in
```

Plain `--tui` no longer waits for the radio: the interface opens immediately
and the connect runs in the background (the status line shows `connecting...`).
A device that never answers only raises a toast — the interface stays up and
the sidebar's `Connect` retries by hand. Flags that need a live session
(`--query-info`, `--uart`, `--loopback-test`, …) still connect before the TUI,
exactly as before.

**3. Hold GPIO1 to GND and accept the pairing prompt** the first time you
connect this host. Release it afterwards — the host is remembered, so later
connections need no button press.

**4. Type.** The terminal pane takes keys directly once it has focus (press
`Esc` if you are in another view). Reset the target and watch the console.

If nothing arrives, check the three wires — target TX to accessory RX, target
RX to accessory TX, and a shared ground — before touching anything else.

## 4. CLI mode

None of this needs the TUI: the same binary runs one-shot commands for
scripts and CI, or hands you a raw serial session. `linkr --help` lists every
flag; this section is the working set, and the
[reference](../tools/linkr-cli/README.md#use) covers the packaging around it.

There are three shapes:

| Shape | Command | Use it for |
| --- | --- | --- |
| Find the accessory | `linkr --scan` (or `linkr scan`) | a first look, a scripted check |
| Connect, run commands, exit | any flag below **plus** `--no-terminal` | scripts, CI, health checks |
| Interactive session | plain `linkr`, or `--tui` to continue in the TUI | daily driving |

### Connection

| Flag | What it does |
| --- | --- |
| `--name <NAME>` | BLE name or prefix; the default `Linkr BLE UART` matches `Linkr BLE UART*` |
| `--address <ADDR>` | connect by address or UUID and skip the name scan |
| `--scan` | list every nearby BLE device, named or not |
| `--timeout <SEC>` | scan budget, default `8.0` |
| `--pair` | request OS bonding — hold the Bee's GPIO1 to GND while it runs; on macOS the system dialog pops when the encrypted service is read, and the bond is remembered afterwards |
| `--lan <HOST[:PORT]>` | talk to the LAN WebSocket bridge instead of the radio |
| `--lan-token <HEX>` · `--lan-token-file <PATH>` | the 32 hex characters of the bridge token; [step 10](#10-lan-mode) says where they come from |

### Management commands

Pass as many as you like. They run as soon as the link is up, **before** any
terminal opens, and they are **BLE-only**: over `--lan` each one answers
`management commands are not available over the LAN bridge; connect over BLE
to run them`.

| Flag | What it does |
| --- | --- |
| `--query-info` | send `@i?` and print the diagnostics — firmware, uptime, WiFi, queues |
| `--query-uart` | send `@u?` and print the UART settings |
| `--uart <SPEC>` | set the UART as `baud,data,parity,stop,flow`, e.g. `115200,8,n,1,n` (baud 300–3000000, data 5–8, parity `n`/`o`/`e`, stop 1/2, flow `n`/`rtscts`) |
| `--wifi-scan` | scan the 2.4 GHz networks the accessory can see |
| `--wifi <SSID[,PASSWORD]>` | connect the accessory to a network |
| `--wifi-key-file <PATH>` | read the password from the first line of `PATH`; pair it with a bare `--wifi <SSID>` so the secret stays out of `argv` and your shell history |
| `--wifi-off` | forget the saved network |
| `--query-wifi` | send `@w?` and print the WiFi state |
| `--webdav <URL>` · `--webdav-off` · `--query-webdav` | set, clear and print the log-upload target |
| `--loopback-test [<STR>]` | send a payload and require the same bytes back — the cheapest proof that the whole link works |
| `--loopback-timeout <SEC>` | how long to wait for the echo, default `3.0` |

### Output

| Flag | What it does |
| --- | --- |
| `--json` | management replies and events as JSON lines on stdout; sensitive command text stays redacted |
| `--quiet` | drop progress messages — results, warnings and errors still print |
| `--debug-io` | byte-level TX/RX traces on stderr |
| `--log-file <PATH>` | append every received byte to a file |

### The terminal session

| Flag | What it does |
| --- | --- |
| `--no-terminal` | connect, run the commands above, exit — no raw mode, no scrollback |
| `--enter <raw\|cr\|lf\|crlf>` | what `Enter` sends, default `raw` |
| `--local-echo` | echo typed bytes locally, for a target that does not |
| `--line-mode` | send one visible line at a time instead of raw terminal mode |
| `--escape <TYPE>` | the key that leaves the session, default `^]` |
| `--ble-write-size <BYTES>` | cap each NUS write; `0` (the default) negotiates one automatically |
| `--write-response` | acknowledge every chunk — NUS fallback only; Reliable UART always does |
| `--write-delay-ms <MS>` | pause between chunks, default `5.0` — NUS fallback only |
| `--tui` | hand the connected session to the TUI, exactly what `linkr tui` does |
| `--yes` | let the assistant approve its own commands (TUI approval broker) |

### Subcommands

| Command | Equivalent |
| --- | --- |
| `linkr scan` | `linkr --scan` |
| `linkr tui` | `linkr --tui` |
| `linkr completion <bash\|fish\|zsh\|powershell>` | `linkr --print-completion <shell>` |

### Recipes

```sh
linkr --scan                                    # is the accessory there?
linkr --query-info --no-terminal                # @i? diagnostics, then exit
linkr --json --query-info --no-terminal | jq .  # the same, machine-readable
linkr --wifi-scan --no-terminal                 # what the accessory can see
linkr --wifi MySSID --wifi-key-file ./pw --no-terminal   # secret out of argv
linkr --loopback-test ping --no-terminal        # end-to-end sanity check
linkr --uart 115200,8,n,1,n --tui               # set the UART, then type
linkr --lan 192.168.1.10 --tui                  # through the LAN bridge
linkr completion zsh > _linkr                   # completions for your shell
```

Every one of them ends in an [exit code](#11-exit-codes), so a health check
stays a single line:

```sh
linkr --loopback-test ping --no-terminal || echo "accessory is not answering"
```

## 5. The screen at a glance

| Region | Where | What it does |
| --- | --- | --- |
| Connection card | left, top | transport, device name, connect / disconnect, LAN host and token |
| Quick send | left, middle | `help`, `version`, `uname -a`, `df -h`, `reboot` |
| Watch | left, bottom | serial findings; select one with `↑` `↓` and press `Enter` |
| Active view | centre | terminal, diagnostics, network or assistant |
| Status line | bottom | focus, view, detail, and the key hints |
| Toast | bottom right | a one-line notice, visible for 2.2 s |
| Notice log | `Ctrl+P` → *Notice log* | the last 200 notices, scrollable |

Everything you can do is also reachable from the command palette, so the key
map below is a shortcut, not a requirement.

## 6. Keyboard

This is the whole `F1` help, in the order it is shown:

| Key | Action |
| --- | --- |
| `Ctrl+P` | command palette (every action, searchable) |
| `F1` | this help |
| `F2` `F3` `F4` `F5` | terminal · diagnostics · network · assistant views |
| `Ctrl+Shift+K` | focus the assistant composer |
| `Ctrl+Shift+M` | assistant: pick the execution mode |
| `Ctrl+Shift+S` | assistant: AI configuration |
| `Ctrl+Shift+N` | assistant: start a new chat |
| `Ctrl+Enter` or `Alt+Enter` | assistant: send the message |
| `↑` `↓` in sidebar | move the selection, `Enter` activates it |
| Quick send | sidebar → `help` / `version` / `uname` / `df` / `reboot` |
| `Ctrl+L` | clear the terminal pane |
| `Ctrl+Q` | quit (asks first while connected) |
| `Ctrl+Up` | focus the sidebar |
| `Esc` | close an overlay / back to the terminal |
| `Shift+PgUp` / `Shift+PgDn` | scroll the terminal scrollback |
| `Shift+Home` / `Shift+End` | scrollback: top / bottom |
| `Ctrl+Shift+R` | arm one-shot `Shift` for the next key |
| `Ctrl+Shift+C` | arm one-shot `Ctrl` for the next key |
| `Ctrl+Shift+A` | arm one-shot `Alt` for the next key |
| `Ctrl+Shift+V` | paste the clipboard into the focused field (or the device) |
| drag in the terminal | select text — releasing copies it |
| `Enter` | send a line (the Enter mode applies) |
| `Tab` / `Shift+Tab` | sent to the target as `TAB` / `CSI Z` |
| `F6`..`F12` | sent to the target unchanged |
| `Ctrl+P` → `term.*` | font size, autoscroll, echo, save log, copy |
| `Ctrl+P` → `app.*` | notices, help, quit, focus switching |

Byte sequences match `web/terminal_keys.js`, so a target that behaves in the
browser behaves identically here.

`Ctrl+Shift+V` reads the system clipboard through the helper your desktop
ships — `wl-paste` (Wayland), `xclip` / `xsel` (X11), `pbpaste` (macOS),
PowerShell (Windows) — the way the web toolbar button reads
`navigator.clipboard.readText()`, and toasts the same kind of refusal when
nothing answers. With none of those installed it can read nothing at all: use
your terminal's own paste key instead, which arrives as a bracketed paste and
lands in the same field. Paste is also the one key an overlay passes through
— it carries text, not a keystroke.

Copying is the same contract as the web toolbar button, with the gesture in
place of the button: press inside the terminal, drag over what you want, and
the release is the copy. Two routes are taken at once — mouse capture has
taken the host's own selection away, so these are the routes a terminal
programme has: the system clipboard through the same helper chain
`Ctrl+Shift+V` reads (`wl-copy`, `xclip` / `xsel`, `pbpaste`, `clip`), which
answers, and `OSC 52` handed to the emulator, which never answers. So the
toast says what actually happened instead of assuming — `Copied N
characters.` when a helper took it, `Sent N characters over OSC 52 — the
terminal may ignore it.` when none did. Some emulators parse that sequence
and drop it on the floor (GNOME Terminal and every other VTE terminal, GNOME
bug 795774), which is why a desktop with no helper installed also hears, once
per session, that nothing reached the system clipboard: `sudo apt install
wl-clipboard` on Wayland, `sudo apt install xclip` on X11. A double click
takes the whole word under the pointer, even a one-character one, and `Ctrl+L`
clears the pane together with the selection. `Ctrl+P → term.copy` puts the
whole visible pane through those same two routes when there is nothing
selected.

Two rules worth remembering:

- **The latched modifiers** (`Ctrl+Shift+R` / `+C` / `+A`) are how the TUI
  stands in for the web terminal's key bar. Arm one, then press the next key:
  it arrives with that modifier applied, once.
- **Any overlay owns every key.** While help, the notice log or a dialog is
  open, nothing falls through to the view underneath — press `Esc` first.

Inside a view, `PgUp` / `PgDn` / `Home` / `End` page that pane once a view has
focus; hold `Shift` when you mean the terminal's scrollback specifically.

## 7. Command palette

Press `Ctrl+P` and type. The search matches action titles in **both**
languages, plus the action id, so `view`, `视图`, `wifi` and `term.copy` all
land. The full registry (34 actions, in order):

| Category | Actions |
| --- | --- |
| View | `view.terminal` · `view.diagnostics` · `view.network` · `view.assistant` |
| Focus | `focus.sidebar` · `focus.terminal` · `focus.assistant` |
| Connection | `connect` · `disconnect` · `transport.toggle` · `uart.settings` |
| Terminal | `term.font_bigger` · `term.font_smaller` · `term.font_reset` · `term.autoscroll` · `term.echo` · `term.enter_mode` · `term.clear` · `term.save_log` · `term.copy` |
| Diagnostics | `diag.refresh` |
| Network | `wifi.scan` · `wifi.status` · `webdav.status` |
| Assistant | `agent.ask` · `agent.mode` · `agent.settings` · `agent.new_chat` · `agent.stop` · `agent.export` |
| App | `app.help` · `app.notices` · `app.language` · `app.quit` |

`↑` `↓` move, `Enter` runs, `Esc` closes. Actions you cannot use right now are
disabled rather than hidden — for example the WiFi actions until you are
connected over BLE — and say why when you try them.

## 8. The sidebar

### Connection card

- **Transport** toggles between BLE and LAN (`◂▸` marks the toggle; it reads
  `(locked)` while an attempt is in flight and while a session is up — wait
  for the attempt, then disconnect. The web client is the same: it disables
  the transport buttons and *Switch device* while `connecting`).
- **Device** is the BLE name or prefix. Leave it empty to match any Linkr
  accessory.
- **Connect / Disconnect** — `Disconnect` asks for confirmation.
- In LAN mode you get **LAN host** and **LAN token** instead (32 hex
  characters; blank when LAN authentication is off).

### Quick send

Five presets, reachable without leaving the keyboard. `Enter` runs the
selected one. `reboot` is flagged `⚠` and always asks first — it is the only
one that restarts your target.

### Watch

The serial watch engine watches the console for known failure signatures.
Findings appear here as they are noticed; select one and press `Enter` to see
the evidence as a toast. If it reads *watch engine not ready*, the engine
could not start on this host.

## 9. The four views

### F2 — Terminal

Click or focus the pane and type. The pane keeps the target's window size in
sync, which full-screen programs need.

| Action | How |
| --- | --- |
| Bigger / smaller font | `Ctrl+=` / `Ctrl+-`, or palette `term.font_*` |
| Reset font size | `Ctrl+0` (range 10–28, default 13) |
| Clear the pane | `Ctrl+L` |
| Scroll the scrollback | `Shift+PgUp` / `Shift+PgDn`, `Shift+Home` / `Shift+End` |
| Copy what you see | palette `term.copy` (the same two routes as a drag) |
| Save a log | palette `term.save_log` |
| Toggle local echo | palette `term.echo` |
| Cycle the Enter mode | palette `term.enter_mode` (`raw` → `cr` → `lf` → `crlf`) |

Use the Enter mode when a bootloader or a shell wants `\r\n` rather than a bare
`\n`. Turn local echo on when the target does not echo what you type.

### F3 — Diagnostics

Sends `@i?` and renders the reply as a grid:

| Field | Meaning |
| --- | --- |
| Firmware | version reported by the accessory |
| Uptime | how long it has been up |
| BLE access | `open` or `scoped`, plus `link Ln` |
| UART Buffer | bytes buffered in the bridge |
| WiFi | current network, when provisioned |
| Upload Queue | log uploads waiting to be sent |

Keys inside the view: `r` refresh, `PgUp` / `PgDn` scroll, `F2` back to the
terminal. Management commands are BLE-only, so a LAN session shows a note
asking you to connect over BLE instead of a grid.

### F4 — Network

- **Scan** lists nearby 2.4 GHz networks. Results appear in the *Scan results*
  block and the feedback line says how many were found — the count comes from
  the live event stream, so it updates as the scan runs.
- Select a row, type the password, then **Connect WiFi**. Validation catches a
  missing SSID, an over-long name or password, commas in the name, and control
  characters.
- **WiFi status**, **WiFi off**, and the **WebDAV** block (target URL, `Set`,
  `Off`, `Status`) are on the same screen.

Keys: `↑` `↓` select, `Enter` acts or edits, `Esc` returns to the terminal.
The whole screen is BLE-only — a LAN transport has no management channel.

### F5 — Assistant

| Key | Action |
| --- | --- |
| `Ctrl+Shift+K` | focus the composer |
| `Ctrl+Enter` / `Alt+Enter` | send |
| `Ctrl+Shift+M` | pick the execution mode |
| `Ctrl+Shift+S` | AI configuration |
| `Ctrl+Shift+N` | new chat |
| `Esc` | back to the terminal |

`Ctrl+Enter` is only a distinct key in terminals that speak the kitty keyboard
protocol — the TUI enables it on entry when the terminal supports it. Where it
cannot be encoded (GNOME Terminal, for instance), use `Alt+Enter`: both chords
send.

There is no dedicated stop key: `Ctrl+P` → **Stop the running turn**
(`agent.stop`) cancels the current run, and so does switching the execution
mode or quitting. Stopping does not recall input that was already sent — use
`Ctrl-C` in the terminal to interrupt the target program.

The three modes, and what they promise:

| Mode | Behaviour |
| --- | --- |
| Manual | AI proposes commands; you press Send to enter them on the target |
| Auto · Recommended | low-risk queries run at a recognized shell prompt; other input needs approval; destructive commands need approval in every mode |
| Full Auto | commands run without confirmation; recognized destructive or irreversible commands — recursive/forced deletes, disk and filesystem tools, dd, flashing and bootloader tools, downloaded content piped into a shell, privilege escalation, recursive permission changes — still need your approval; detection does not cover every operation inside scripts or indirect execution |

Changing the mode keeps the conversation but stops the running turn and
cancels pending input.

Before the first question, configure a model endpoint (`Ctrl+Shift+S`): API
base URL, model ID, API key, protocol (OpenAI-compatible, Anthropic, Google
AI), reasoning effort, context window, max output tokens and per-token prices.
`Ctrl+S` saves, `Esc` closes. The key is stored on this machine only — see
[step 13](#13-settings-on-disk). If the endpoint is plain `http` and not
loopback, the dialog warns before it stores a key.

`Ctrl+P` → **Export report** writes the diagnosis as Markdown to
`<config-dir>/linkr/linkr-agent-<timestamp>.md`, including the tasks and notes
the assistant kept for this target. It says so when there is nothing to export
rather than writing an empty file.

## 10. LAN mode

Once the accessory is on your 2.4 GHz network you can drop Bluetooth:

```sh
linkr --lan 192.168.1.10 --lan-token <32 hex> --tui
```

or start over BLE and switch from the sidebar (`Transport`, then reconnect)
once you know the address. The token comes from `--lan-token`,
`--lan-token-file`, `LINKR_LAN_TOKEN` — in that order — and from the store:
if the TUI has ever been connected over BLE you do not need to supply one at
all. While a BLE session is up the TUI reads the token from the device itself
(`@s?`, exactly what the web page does), keeps it in
`<config-dir>/linkr/lan_tokens.json` (mode `0600`, keyed by device and by
host) and fills the token field for you, so switching transport is all it
takes. Leave the field empty only when the bridge runs without
authentication, and remember the captured token is a bearer credential: the
CLI prints `token=<redacted>` wherever such a line would show up.

Remember that diagnostics, WiFi provisioning and WebDAV stay BLE-only: over
LAN you get the terminal and the assistant, and those screens explain why they
are disabled.

## 11. Exit codes

| Code | Meaning |
| --- | --- |
| `0` | success |
| `1` | runtime error (device, transport, command failure) |
| `2` | usage error |
| `3` | the device disappeared mid-session |
| `130` | interrupted (`Ctrl+C`) |

`Ctrl+Q` asks for confirmation while connected, so an accidental keystroke
cannot drop your session.

## 12. Switch the language

`Ctrl+P` → **Switch language** (`app.language`) flips the interface between
English and Chinese and saves it. Every screen re-reads the setting on the
next frame, so the change is immediate. The same setting is `linkr-lang` in
the web client.

The language is picked up from `LC_ALL` / `LC_MESSAGES` / `LANG` on the first
run, and remembered afterwards. To change it by hand, edit `lang` in the
settings file — or just use the palette action.

## 13. Settings on disk

| File | Holds |
| --- | --- |
| `<config-dir>/linkr/tui.json` | font size, Enter mode, local echo, transport, last LAN host, last BLE address, active view, autoscroll, language |
| `<config-dir>/linkr/agent.json` | API base URL, model, key, protocol, pricing |
| `<config-dir>/linkr/agent_tasks.json` | the assistant's tasks per target |
| `<config-dir>/linkr/agent_notes.json` | the assistant's notes per target |
| `<config-dir>/linkr/command_policy.json` | the command approval policy |
| `<config-dir>/linkr/linkr-agent-*.md` | exported reports |

`<config-dir>` is `~/.config` on Linux, `~/Library/Application Support` on
macOS, and `%APPDATA%` on Windows.

`~/.config/linkr/tui.json`, for example:

```json
{
  "font_size": 14,
  "enter_mode": "raw",
  "local_echo": false,
  "transport": "ble",
  "last_lan_host": "192.168.1.10",
  "active_view": "terminal",
  "autoscroll": true,
  "lang": "zh"
}
```

`transport` is `ble` or `lan`, `enter_mode` is `raw`, `cr`, `lf` or `crlf`,
`active_view` is `terminal`, `diagnostics`, `network` or `assistant`, and `lang`
is `en` or `zh`. An unknown or missing field falls back to its default, so an
older file keeps working.

## 14. Troubleshooting

| Symptom | Check |
| --- | --- |
| `--scan` finds nothing | the accessory is powered; the host has an active Bluetooth adapter; `--timeout 10` if it is slow to advertise |
| It connects, then the console is silent | target TX → accessory RX, target RX → accessory TX, shared ground; reset the target |
| You see output but typing does nothing | target RX → accessory TX; turn hardware flow control off unless CTS and RTS are both wired |
| Pairing is rejected | hold GPIO1 to GND while connecting, then accept the prompt |
| `linkr: error:` with nothing after it | a management request timed out — the Python CLI prints the same empty message (`str(TimeoutError())` is empty), so the accessory was probably busy; another `linkr` may be holding it, run the command again |
| `Bluetooth adapter is busy` / `BLE link dropped` | a second `linkr` (or a phone app) is using the same accessory — the CLI backs off and retries three times, and joins an existing link instead of fighting for it; if it still fails, wait a second and rerun |
| The TUI will not start | it needs an interactive terminal; do not redirect stdin or stdout |
| The TUI opens but the status line never turns connected | the connect runs in the background — press `Connect` in the sidebar to retry, give a slow radio more room with `--timeout 15`, or pass `--address` to skip the scan |
| "Switch device" answers "Connecting…" | an attempt is in flight — wait for it to settle (worst case about 15 s over LAN, 8 s over BLE); the web client disables that button *and* the transport toggle while `connecting` |
| WiFi scan reports 0 networks | the scan runs over BLE — switch the transport to BLE first; results arrive while the scan is still running |
| The assistant refuses to edit its configuration | a turn is running; return to the conversation and stop it first |
| Save failed for the AI configuration | the OS or browser storage is restricted; clear the configuration and retry |
| Chinese text looks clipped in a dialog | the terminal font has no CJK glyphs — pick a font with them, or the width calculation cannot help you |

More: [README troubleshooting](../README.md#troubleshooting) and the
[development guide](DEVELOPMENT.md) for building and flashing.

## 15. See also

- [Project README](../README.md) · [中文](../README.zh-CN.md) — what Linkr Bee
  is, flashing, wiring, pairing
- [Rust terminal reference](../tools/linkr-cli/README.md) — build, packaging,
  flags, exit codes, module layout
- [Development guide](DEVELOPMENT.md) ·
  [开发指南](DEVELOPMENT.zh-CN.md) — firmware builds, test modes, Kconfig
- [Bluetooth pairing](BLE_PAIRING.md) — GPIO1 authorization and recovery
- [Hardware requirements](HARDWARE.md) — pinout and electrical limits
- [Documentation index](README.md)
