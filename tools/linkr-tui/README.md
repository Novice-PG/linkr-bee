# linkr-tui — the host terminal, rebuilt

A cross-platform host client for the Linkr Bee bridge: one binary, a CLI and a
TUI over a shared core. This crate is a **from-scratch rebuild** of the TUI that
PR #9 carried, built against the same interface design and the same web-client
palette, but structured so that every screen is testable without a terminal.

## Why a rebuild and not a patch

The previous TUI grew to ~55k lines across 48 files, and its review never
converged: defects kept surfacing in the hand-built panel skeleton and in the
protocol code that was rewritten module by module. The design was fine; the
structure was not. This crate keeps the design and starts the code again with
three rules:

1. **A frame is a pure function of state.** `ui::*` reads `&App`; nothing else
   mutates a widget during a draw. The same state always paints the same bytes,
   which a test asserts directly.
2. **Content and placement are separate.** Each region builds its content as
   plain `Line`s from a pure function; one thin layer places them. A test can
   therefore check a sidebar row, a bar or a toast without a terminal, and check
   the layout without re-testing the content.
3. **No literal colours in a view.** Every colour comes from `theme::Theme`,
   which carries the web client's design tokens. Views ask for a *role*
   (`accent`, `muted`, `danger`…), never for a hex value and never "is this the
   light theme?".

## What is built so far

| Module | What it holds |
|---|---|
| `theme.rs` | The design system: 16 tokens × two themes (the web client's `:root` and `[data-theme="light"]`, terminal pair from xterm.js), and every role-based style, block and colour helper |
| `event.rs` | `ConnectionState`, `NoticeLevel`, `View` — and the F2–F6 rota, which is derived from the view list rather than written out twice |
| `i18n.rs` | Every user-visible string, in Chinese and English, in two `const` tables so a missing translation cannot compile |
| `text.rs` | Display-width text: pad, clip and wrap measured in *columns*, because a CJK glyph is two of them and `{:<14}` is not |
| `app.rs` | The state a frame needs and its transitions (view rota, theme toggle, bounded notices, clamped sidebar cursor, `ble_connected`) |
| `ui/mod.rs` | Layout (`regions`) and composition (`draw`), plus headless rendering for tests and `--print-frame` |
| `ui/chrome.rs` | Top bar (state dot, busy indicator, counters, speed, autoscroll, clock) and bottom bar (focus, view, state, key hints) |
| `ui/sidebar.rs` | The four sidebar sections, the accent-chip selection, and the display-width wrap that keeps a long row inside its column |
| `ui/body.rs` | The centre column: which view's content goes in it, and the frame that does or does not wrap it |
| `ui/toast.rs` | The notice stack in the top-right corner |
| `ui/overlay.rs` | The modal layer: where a palette or a dialog is measured and centred, and the only place that knows a `Rect` |
| `views/diagnostics.rs` | The `@i?` read, end to end: `@info` parsing, the six web-client formatters, the read's state machine, and the grid |
| `transport/` | The wire: `LinkKind` (stdio, BLE, LAN), a `Link` split into a reader and a writer because the two must run at once, `LinkEvent`, and the threads that join them. There is deliberately no serial link: the device *is* the far end of every one of these — see `transport/mod.rs` |
| `transport/stdio.rs` | The process bridge — `--bridge <command>` — and the built-in loopback device, so the whole path runs on a laptop with no radio |
| `session.rs` | `Dial` (what the command line asked for) and `Session`: the counters, the connection state, and the pump that folds the link's events into the frame |
| `vt/parser.rs` | The escape-sequence state machine: CSI, OSC, ESC, and the parameter list |
| `vt/screen.rs` | The screen itself: a styled cell grid with a cursor, erase, insert/delete, a scroll region, the alternate screen and a bounded scrollback |
| `vt/mod.rs` | The emulator over the screen: which private modes do what, the alternate screen, the title, and the few sequences a host must *answer* |
| `views/terminal.rs` | The pane: a streaming UTF-8 decoder and a streaming parser in front of the emulator, plus the painter |
| `palette.rs` | The `Ctrl+P` command palette: twenty actions, each one a function pointer so the palette runs the same code the chord does |
| `dialogs.rs` | Help, the notice log, the guarded-command confirmation, and the prompts that ask for a file, a network name, a password or an upload URL |
| `protocol.rs` | The management service's framing: request and response headers, the version and capability handshake, the Reliable UART scheme, and the reassembler that puts a fragmented answer back together |
| `device.rs` | The request/response bookkeeping over that service: one question at a time, an answer claimed only by the id that asked for it, and a timeout that reports a slow bridge as slow rather than as silent |
| `accessory.rs` | The bridge's own settings as commands and as parsed data: `@u=…`, `@w=…`, `@d=…`, their limits, and the redaction that keeps a password off every screen but the wire |
| `cli.rs` | One table of flags read twice — as usage text and as a completion script for bash, zsh and fish — plus the scanner, the script runner, the escape-byte parser and the loopback test |
| `trace.rs` | `--log-file` and `--debug-io`: what the device said, appended; what went both ways, traced. Secrets are redacted before they reach either |
| `views/network.rs` | The WiFi and WebDAV form: ten rows the keyboard walks, each action raising an approval card that names the change rather than the command |
| `transfer/probe.rs` | The question the web client never asks: what file-transfer tools the device has, and therefore whether a file can move *at all* |
| `views/transfer.rs` | The transfer view: one precheck row that reports what the device has, plus the history of what has moved |

`views/` is where a view lives: a *model* (what the device said, as plain data)
and a *painter* (a pure function from `&App` plus a pane width to lines). The
split is the reason a whole view can be pinned to the documented sample reply
without a socket, a clock or a terminal.

The terminal pane is the exception, and deliberately so: it is not a document
but a *screen*. A device paints with escape sequences — it moves the cursor,
colours the next glyph, erases a line and repaints one cell for a spinner — so
the pane emulates rather than lays out. `vt/` holds the emulator and
`views/terminal.rs` the wiring around it; the pane's size is told to the
emulator by the event loop, never by a paint, because a frame is a pure function
of the state and may not reshape what it is drawing.

Network is a live form: `@w?`/`@w scan` fill it, `@w=…` and `@d=…` are raised
as approval cards that name the change, and a scan's answer goes to its own list
rather than into the status slot.

Transfer is a precheck, not a form. Opening it asks the device what it has,
once: a board with `dd`/`base64`/`wc`/`tr` can take a file, and one missing any
of them is told which, instead of being handed a start button that can only
fail. Files then move over the `dd | base64` shell pager — the same channel the
web client uses — and a `sha256sum` (or `shasum`) on the target is what turns a
move into a verified one; without either, the view says up front that a result
cannot be proved.

Assistant is the next view, and behind it the agent layer.

Every management question — `@i?`, `@u?`, `@w?`, `@d?` — is asked on the
bridge's own GATT service and not down the pipe the target prints to. That is
not a preference: an answer delivered on the same stream as the target's output
can be told apart from it only by guessing at the shape of the bytes, and a
guess that fails types the bridge's answer into somebody's shell prompt.

## Asking the bridge things from a shell

Everything the terminal can do to the bridge can also be done and exited:

```sh
cargo run -- --ble-name Linkr --query-info --query-uart --json
cargo run -- --ble-name Linkr --uart 921600,8,n,1,none
cargo run -- --ble-name Linkr --wifi BenchNet --wifi-key-file ~/.wifi
cargo run -- --ble-name Linkr --wifi-scan --loopback-test --no-terminal
cargo run -- --print-completion zsh
cargo run -- --scan --timeout 5
```

`--no-terminal` is implied by any flag that asks for something: a command line
naming `--query-info` wants an answer, not an interface. Each request is sent
and waited for rather than fired off together, because the bridge answers one
question at a time and two outstanding ones would have two `OK`s arriving in no
order anyone can rely on. `--wifi-scan` is given the long timeout on its own: a
scan is the board's radio working through every channel, and the ordinary reply
timeout would call a running scan a silent bridge.

`--loopback-test` is the one check that proves the round trip. Everything else
is answered by the bridge, so a board that accepts writes and never sends
anything looks healthy to all of them; the loopback is answered by the target's
own UART with its TX and RX joined.

`--log-file` appends what the device sent, and `--debug-io` traces both
directions on stderr. Neither is buffered, because a log missing its last line
is a log missing exactly the moment that mattered — and a management command is
traced redacted, since stderr is scrollback, a terminal recording or a CI log.

`--escape` reserves one byte for "stop" (`^]` by default, as `0x1d`, or the
byte itself). A byte stream has no out-of-band signal, so the reservation has to
be a byte — and it is checked *before* the keystroke is sent, because sending it
and then leaving would put a stray control character into the device's input.
`--line-mode` holds a typed line until Enter, which is what a target with a line
discipline wants, and `--enter` names what Enter actually puts on the wire
(`raw`, `cr`, `lf`, `crlf`) — a target that wants `\r` and is given `\n`
simply never answers, and the symptom looks exactly like a dead link.

## Gates — all four must pass before a commit

```sh
cargo fmt --check                                   # style
cargo clippy --all-targets -- -D warnings           # lints, warnings are errors
cargo test                                          # 679 tests, headless
for t in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu \
         x86_64-pc-windows-msvc x86_64-pc-windows-gnu aarch64-pc-windows-msvc \
         x86_64-apple-darwin aarch64-apple-darwin; do
  cargo check --target "$t" --all-targets || exit 1  # 7 targets, cross-platform
done
```

Every target is checked because the crate ships to Linux, macOS and Windows on
both x86-64 and ARM. There are no C dependencies: the only runtime requirement
on Linux is glibc's base libraries.

The toolchain is declared, not assumed: `rust-toolchain.toml` names the channel,
the `clippy`/`rustfmt` components and those seven targets, so rustup installs
exactly what the gates need and a laptop, CI and the packaging script cannot
disagree about the compiler.

## Looking at a frame without a terminal

```sh
cargo run -- --print-frame 100x30                     # plain text
cargo run -- --print-frame 100x30 --ansi              # with true-colour escapes
cargo run -- --print-frame 100x30 --ansi --theme light --lang en
cargo run -- --print-frame 100x30 --ansi --transport ble   # transport reported as locked
cargo run -- --print-frame 110x32 --view diagnostics --state connected --sample
cargo run -- --print-frame 110x32 --overlay palette   # palette, help, notices, confirm
cargo run -- --print-frame 110x32 --feed $'\e[31mred\e[0m ok'   # the emulator, no device
```

`--view` opens on a named view, `--state` previews a connection state, and
`--sample` loads the documented `@info` reply, so a content view can be inspected
with real data instead of its empty state. `--overlay` opens the modal layer, and
`--feed` pushes text through the pane's own decoder and parser — escapes
included — which is how the emulator is reviewed with no device attached. All of
them are previews of *state*: `--print-frame` draws one frame and exits without
dialling, so a preview cannot start a session behind the user's back.

The frame is rendered through ratatui's test backend, so this works over SSH,
in CI, and with no TTY at all. `dev/frame-to-html.py` turns an `--ansi` dump
into a standalone HTML preview with exact column alignment (wide CJK glyphs span
two columns), and `dev/overlay-sheet.py` runs the real binary once per surface to
put a whole set on one page — `--sheet overlays` for the modal layer, `--sheet
vt` for the emulator:

```sh
python3 dev/overlay-sheet.py /tmp/vt.html --sheet vt --lang zh --theme dark
```

The previews are generated on demand and deliberately not committed: they are
large, and everything in them is reproducible from the source.

## File transfer

There is one channel: the `dd | base64` shell pager that the web client uses.
Nothing here offers a second one.

A block protocol — ZMODEM — was written for this view and then taken out again.
The reason is not that it cannot work; it is that it cannot be made to work
*here*. The bridge pushes bytes from BLE into UART faster than a slow UART
drains them, and the queue in between drops what it cannot hold. The sender at
this end never sees those drops, so every one of them is discovered only by a
timeout, and a run made of timeouts is slow enough to be useless. Making it
reliable needs hardware flow control on the board, which is firmware this
program does not own. So the answer was to remove it rather than to ship a
transfer that quietly corrupts files.

What is left is the precheck. Opening the view sends one question — the same
`command -v` walk the web client's own probe uses — and reports the tools the
target actually has. `dd`, `base64`, `wc` and `tr` are what a move needs;
`sha256sum` or `shasum` is what makes a move *provable*, and the view says so
separately, because a file that landed but cannot be checked is a different
outcome from one that did not land.

Files land under the *far end's* name reduced to its last component, so a
malicious name cannot walk out of the directory.

## Releasing

`dev/package.sh <target>` builds the release binary and lays out the archive — the
binary, this README and the licence, in `linkr-<version>-<target>.tar.gz`. It is
the *only* definition of that layout: the CI matrix and a developer's shell call
the same script, so a release cannot be assembled two ways.

```sh
cd tools/linkr-tui
dev/package.sh x86_64-unknown-linux-gnu     # → dist/linkr-1.0.0-x86_64-unknown-linux-gnu.tar.gz
```

`.github/workflows/linkr-tui.yml` runs the four gates, then packages all seven
targets — Linux on x86-64 and ARM, Windows on both ABIs, macOS on both
architectures — and on a `linkr-tui-v<version>` tag it attaches the archives and
a `SHA256SUMS` to the release. The archives for the host's own targets are
*executed* before they are uploaded (`--version`, one frame, and a bridge to a
missing program that must exit `2`), because linking is not the same as running.
The tag is namespaced so it cannot collide with the firmware's `v*` releases.

## Deliberate differences from the previous TUI

- **The theme is passed, not global.** The old code kept the palette in a
  `static RwLock`. Here `&Theme` flows into every render function, so tests can
  paint either theme with no shared state and a repaint can never observe a
  half-changed palette.
- **Wrapping is ours.** A row too wide for its column is wrapped by display
  width, with a break at whitespace when there is one and between glyphs when
  there is not — Chinese has no spaces, and the old panel ran off the column
  instead of wrapping. The arithmetic lives once, in `text.rs`, because a second
  copy is how two views come to disagree about what a column is.
- **A body is told how wide its pane is and never paints past it.** Every line
  goes through a width-aware path, so a message that would wrap in a narrow pane
  is clipped instead — a wrapped body line shifts every row below it and stops
  the frame matching its own layout. A test walks every view at six widths.
- **The sidebar cursor is clamped by the state**, and a test pins the cursor's
  range to the rows the sidebar actually offers, so the two cannot drift.
- **The status dot is filled whenever there is anything to report** (connected,
  dialling, failed) and hollow only at rest, which is what the design's frames
  show; the colour carries which state it is.
- **The emulator is ours, because upstream has none.** The web client hands its
  terminal pane to a bundled xterm.js, so there is no emulator in this repository
  to port: `vt/` is written against the VT sequences and xterm's behaviour, and
  it is tested the way a screen can be tested — cells, cursor, scroll region,
  scrollback — rather than by comparing screenshots.
- **The device is answered, not just drawn.** `ESC [ 6 n` is a *blocking* read
  for the program that sends it and `CSI 18 t` is how a pager sizes itself, so
  the emulator generates those replies and the pump puts them back on the wire on
  the same tick. An emulator that only paints looks right until a program hangs.
- **The link is split, not guarded.** Reading and writing run on their own
  threads and hand the frame events over a channel, so a device that stops
  answering cannot freeze the keyboard, and a repaint never waits on a `read`.

## Layout constants

`ui::BAR_HEIGHT` (1 row) and `ui::SIDEBAR_WIDTH` (30 columns) are the only
numbers the composition knows. The sidebar yields width to the centre column
when a window is too narrow for both, so a small terminal shows the view rather
than a wall of sidebar.
