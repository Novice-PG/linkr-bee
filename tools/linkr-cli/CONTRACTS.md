# linkr-cli implementation contract

This crate is the cross-platform (Linux / Windows / macOS, every common
architecture) host client for Linkr Bee: a feature-complete CLI plus an
OpenCode-style TUI whose behavior mirrors `web/` (the browser client) and the
assistant in `mobile/src/pi-agent.mjs`.

Four workstreams build in parallel against this document. **This document and
the skeleton files it names are the integration contract.** If you must change
a public signature, change this document in the same commit and say so in your
final report.

## 0. Ground rules

1. Only edit files you own (§1). Never edit another workstream's files,
   `Cargo.toml`, `Cargo.lock`, or the spec files in `specs/`.
2. No new dependencies. Everything needed is already in `Cargo.toml`
   (tokio, futures, anyhow, thiserror, serde, serde_json, clap, clap_complete,
   btleplug 0.13, tokio-tungstenite 0.30, ratatui 0.30, crossterm 0.29,
   vte 0.15, unicode-width, reqwest 0.13 [json,stream,rustls], regex, base64,
   sha2, uuid [v4], chrono, dirs, tokio-stream, async-trait). If you truly need
   another crate, stop and report instead of adding it.
3. `cargo check` and `cargo test` must pass when you finish (and as often as
   you can while working). Other workstreams are compiling in parallel; cargo
   serializes on a lock, that is expected — wait, do not delete `target/`.
4. Any GitHub download (release assets, raw files) must go through the proxy
   prefix `https://gh.acmsz.top/https://github.com/...`. crates.io needs no
   proxy.
5. Comments and user-visible strings: English, matching the tone of the
   existing Python CLI (`linkr: ...` progress lines on stderr).
6. Behavior parity is measured against the specs, not against your intuition:
   `specs/PYTHON_CLI_SPEC.md` (CLI/port), `specs/WEB_UX_SPEC.md` (UI/UX),
   `specs/AGENT_SPEC.md` (assistant), plus `docs/LINKR_BLE_API.zh-CN.md`
   (wire protocol, authoritative for bytes on the air).

## 1. File ownership

| Workstream | Owns (create/edit) | Reads (never edits) |
| --- | --- | --- |
| **P** protocol | `src/protocol/mgmt.rs`, `uart.rs`, `geometry.rs`, `validate.rs`, `src/protocol/mod.rs` | specs/PYTHON_CLI_SPEC.md |
| **S** session/CLI | `src/transport/*`, `src/session.rs`, `src/cli.rs`, `src/term.rs`, `src/main.rs` | everything |
| **T** TUI | `src/tui/*` | specs/WEB_UX_SPEC.md, `src/session.rs`, `src/agent/mod.rs` |
| **A** assistant | `src/agent/*`, `src/journal.rs`, `src/watch.rs`, `src/target_files.rs`, `src/target_verify.rs` | specs/AGENT_SPEC.md |

Shared, integrator-owned (do not edit without updating this file):
`src/lib.rs`, `src/event.rs`, `CONTRACTS.md`, `Cargo.toml`.

Tests: put unit tests in `#[cfg(test)]` modules inside the files you own.
Cross-cutting contract tests (byte parity with the web client, marker strings)
belong to the workstream that owns the string.

## 2. Cross-module types (already written — do not redefine)

`src/event.rs`:

- `CoreEvent::{Connection{state,detail}, UartRx(bytes), MgmtMessage{kind,id,ok,final_,lines,command}, Notice{level,text}}`
- `ConnectionState`, `MgmtKind`, `NoticeLevel`, `RequestId`.

`src/session.rs`:

- `TransportSpec::{Ble{name,address,timeout}, Lan{host,token}}`
- `SessionOptions{transport, ble_write_size, log_file, debug_io, geometry}`
- `SessionHandle` (clone): `send_uart(Vec<u8>)`, `request_mgmt(String, Option<Duration>) -> oneshot::Receiver<Result<MgmtReply,String>>`, `set_terminal_size(u16,u16)`, `disconnect()`, `info() -> SessionInfo`, `bus() -> &CoreBus`
- `CoreBus` (clone): `subscribe() -> broadcast::Receiver<CoreEvent>`, `publish(CoreEvent)`
- `spawn_session(SessionOptions, CoreBus) -> anyhow::Result<SessionHandle>` — resolves after connect + handshake reads succeed.

`src/transport/mod.rs`: `Transport` trait (`kind`, `write_mgmt`, `write_uart`,
`disconnect`, `write_size`), `TransportEvent`, `TransportChannel`,
`DiscoveredDevice`, `ProtocolInfo`, `ReliableState`.

`src/agent/mod.rs`: `AgentConfig`, `Provider`, `ExecMode`,
`ApprovalKind`, `ApprovalRequest`, `ApprovalDecision`, `ApprovalBroker`,
`AgentEvent`, `AgentHandle::{ask,stop,subscribe}`, `spawn(...)`.

Hard rules:

- The session owns framing, geometry sync, capability gating and the log
  file. Components only call handles and subscribe to the bus.
- Every subscriber receives every `CoreEvent`; duplicated UART delivery to the
  TUI pane and the assistant journal is intentional (the web client keeps an
  xterm scrollback and a 128 KiB journal in parallel).
- `request_mgmt` resolves only when the matching type=2 response arrives (or
  after the FINAL event when `wait_final` was requested) — the Python
  `response`/`elif FINAL` split in PYTHON_CLI_SPEC §2.5 must be reproduced.

## 3. Workstream P — protocol

Port `tools/linkr_ble_terminal.py` logic **behavior-identically**:

- `mgmt.rs`: 12-byte `LK` header encode, request-id wrap (0xFFFFFFFF → 1),
  chunking (`max(20, write_size)` first chunk carries the full header),
  reassembly state machine incl. its exact warning drops, response/FINAL
  resolution, redaction of `@w=`/`@d=` to `@w=<redacted>`/`@d=<redacted>`,
  `lines` splitting (rstrip `\r\n`, then Python `splitlines`, `[""]` when
  empty). Constants already in the file stay put.
- `uart.rs`: `LR` header, one sequence per logical frame, `next_sequence`
  wrap, duplicate-of-last drop, gap report, ATT chunking `max(20, min(write_size,244))`.
- `geometry.rs`: clamps 2..=1000, command
  `stty rows {rows} cols {cols} >/dev/null 2>&1\r` byte-identical to
  `web/terminal_geometry.js`, prompt detection regexes, the
  busy/observe/take_pending/confirm/abort state machine, 1024-char line cap.
- `validate.rs`: `normalize_uart_spec`, `parse_escape`, `translate_enter`,
  `describe_escape`, `python_repr_bytes`, `normalize_name_prefix`,
  `match_device` — exact messages and canonicalization from
  PYTHON_CLI_SPEC §9 (they are asserted by tests).
  `resolve_wifi_credentials(spec, key_file, env, prompt)` also belongs here:
  add it with the order inline → key file → `LINKR_WIFI_PASSWORD` → prompt
  callback, and the exact error strings from §9.2.

Acceptance: port the Python test cases for these areas
(`tests/test_terminal_cli.py`: ValidatorTests, TerminalGeometryTests,
ManagementChannelTests, ReliableUartTests, WifiCredentialTests,
DeviceMatchTests — read it) into `#[cfg(test)]` tests. They must pass without
a device. Byte-exactness with `web/terminal_geometry.js` is asserted by
`tests/test_terminal_cli.py::test_geometry_command_is_byte_identical_to_the_web_client` — reproduce that assertion against the JS source file
(`../../web/terminal_geometry.js`).

## 4. Workstream S — transport, session, CLI

- `transport/ble.rs`: scan filtered by Management Service UUID
  `4c4b0001-9a7e-4f4e-8b8a-3d6f12a0c001`, name matching rules from
  PYTHON_CLI_SPEC §8 (exact then prefix, `*` handling, multi-match warning,
  unnamed devices sort last with `\u{ffff}` key for `--scan` output),
  connect + handshake reads (Protocol Info ≥10 bytes/major==1, Device ID 16
  bytes, Reliable State 16 bytes/version==1/seq≠0), capability checks with
  the exact error strings, `configure_ble_write_size` auto rule
  (char-reported → mtu-3 → 20, clamp 20..=244; manual clamps to 244),
  subscriptions (MGMT_RESPONSE then RELIABLE_UART_TX), write-with-response
  everywhere on BLE, `--pair` macOS special case, disconnect callback.
- `transport/lan.rs`: URL normalization (`wss?://` as-is else `ws://host/ws`),
  15 s connect timeout, 5 s handshake timeout, `@ws auth=none|required` /
  token text frame / `@ws auth=ok` from `docs/LINKR_BLE_API.zh-CN.md` §8,
  binary frames → `TransportEvent::Data{UartTx}`, text frames after handshake
  → notices, pre-handshake foreign text → warn notice, token validation
  `^[0-9a-f]{32}$` with the web client's message.
- `session.rs`: spawn/connect orchestration in the documented order
  (§PYTHON_CLI_SPEC 3.1/3.2), capability gating per command, request pipeline
  (5 s response timeout; 35 s wait-final for `@w=`/`@w off`/`@w scan`),
  geometry sync loop (observe every `UartRx` before publishing, send pending
  `stty` via `send_uart`, `set_terminal_size` from resize), log file append
  (raw RX bytes only), `SessionInfo` counters (rx/tx bytes, baud, label),
  debug-io notices (`TX b'...'` style via `python_repr_bytes`),
  clean teardown (`fail_all`, publish `Connection{Disconnected}`).
  LAN mode: no reliable framing, no geometry management-over-serial? — geometry
  still sends `stty` over the UART channel (it is target shell input), so it
  works on LAN too; management commands are rejected with a clear error when
  the transport is LAN.
- `cli.rs`: full flag parity with the Python parser (PYTHON_CLI_SPEC §9.3 —
  same names, defaults, validators, help text style) **plus**:
  `--lan HOST[:PORT]`, `--lan-token HEX`, `--lan-token-file PATH`
  (env `LINKR_LAN_TOKEN`), `--tui`, `--yes`, subcommands `tui`, `scan`,
  `completion <bash|zsh|fish|powershell>`, `--print-completion <shell>` alias,
  `--version`. Exit codes 0/1/2/3/130. `--json` emits one JSON object per
  management message exactly like Python (`type,requestId,ok,lines,command`).
  Terminal loop: raw mode (crossterm), escape byte exit rule, enter
  translation, local echo, line mode, `terminal open. press X to exit.` and
  the rest of the message catalog (PYTHON_CLI_SPEC §12), loopback test,
  `--scan` alone exits 0, `--no-terminal` exits after commands.
  Non-TTY stdin: keep working (line mode automatically when not a TTY, do not
  error like Python did on Windows — that is the Windows completion fix;
  print the same hints).
  Dispatch: with `--tui` or subcommand `tui`, after a successful connect hand
  `TuiContext` to `linkr_cli::tui::run` (approval broker for `--yes` mode is
  TUI-side; in plain CLI mode the assistant is not started).
- `term.rs`: raw-mode guard used by the CLI loop; line mode works on every OS.

### 4.1 Additions recorded by workstream S

Public items the workstream added on top of the contract above (nothing
existing was removed or re-typed; consumers may rely on these):

- `transport/mod.rs`:
  - `TransportEvent::Text(String)` — text frame outside the LAN handshake,
    surfaced by the session as a notice.
  - `Transport::events(&self) -> broadcast::Receiver<TransportEvent>` — the
    session's single subscription point; the default implementation returns an
    already-closed receiver so external transports keep compiling.
  - `EventHub` (`new`, `publish`, `subscribe`, `Default`) — buffers events
    emitted between connect and the session's subscription.
- `transport/ble.rs`: `BleHandshake.label` (display name of the connected
  device) and `ble::connect(..., pair: bool)` — `pair` is the `--pair`
  hint from the CLI; it is a documented no-op on platforms where btleplug
  exposes no `pair()`.
- `session.rs`:
  - `SessionSetup { pair, json }` plus `spawn_session_with(opts, bus, setup)`;
    `spawn_session(opts, bus)` delegates to it with the defaults.
  - `SessionHandle::bus()` (the TUI/assistant share the CLI's event bus) and
    `SessionOptions`/`SessionInfo` fields as already listed.
  - `SessionHandle::test_detached()` — `#[cfg(test)]`-only constructor for a
    handle with no session task, so TUI tests can render without a transport.
- `cli.rs`: `DEFAULT_NAME`, `run()`, `Cli`/`Command` (the clap parser),
  `completion_script(shell)`, the output helpers
  (`set_quiet`/`quiet`/`set_yes`/`yes`/`info`/`warn`/`error`) that
  `ble.rs`, `lan.rs` and `session.rs` print through, and the private
  `Output`/`Flow` rendering types that make the message catalog unit-testable.

Deliberate deviations from `specs/PYTHON_CLI_SPEC.md` (all covered by tests):

- Completion scripts are produced by `clap_complete` from the clap parser
  (§11's hand-rolled scripts are Python-specific); both entry points accept
  `bash`, `fish`, `zsh` **and** `powershell`, print to stdout ending in a
  blank line and exit 0 — unknown shells exit 2 with argparse's
  `invalid choice: '…' (choose from …)` wording.
- clap's own wording differs from argparse's for unknown/ambiguous flags
  (`error: unexpected argument '--nope' found` vs `unrecognized arguments:`);
  our own validators emit the exact §9.1/§12 strings.
- The capability precheck (§3.1) runs right after the connect messages, so it
  prints after `management API …` rather than before `connecting...`.
- The automatic write size skips the contract's "char-reported" step and goes
  straight to `mtu - 3` (fallback 20, clamp 20..=244): btleplug 0.13 exposes
  no maximum-write-length field.
- `--scan` also stays alive for `--lan`/`--tui` (§8.2 only knows
  `--address` and the control actions), because those invocations connect
  after printing the table.
- A management request that loses its transport reports
  `disconnected before management response` where Python prints an empty
  timeout message (`linkr: error: `).
- `--pair` is accepted everywhere but only has an effect where the platform
  stack can bond (macOS); NUS fallback writes and `--write-response` /
  `--write-delay-ms` are parsed and validated but unused — the Reliable UART
  path is mandatory after the handshake. Their `--help` says so plainly
  (`NUS fallback only; …`) so the flags cannot look silently broken —
  `the_nus_only_flags_parse_and_say_that_they_are_inert`.
- The BLE transport retries adapter-busy failures (`org.bluez.Error.InProgress`)
  three times over 750 ms per operation, and `Connect` stops dialling while
  somebody else is dialling the same accessory, watching for their link and
  joining it instead — Python/bleak surfaces `InProgress` immediately. Covered
  by `busy_failures_are_recognised_in_every_spelling`,
  `only_busy_failures_gain_the_adapter_hint` and
  `transient_link_drops_are_retried_and_explained`; `term::restore_sigpipe`
  (exit 134 from `linkr … | head`) is likewise Unix-only behaviour Python gets
  for free from CPython.
- `--tui`/`tui` is ignored when `--no-terminal` is also given.
- `--lan-token` takes a fourth source after the flag, the file and
  `LINKR_LAN_TOKEN`: the token a TUI BLE session captured from `@s?` (the web
  dials with whatever `lanTokens` already holds, and it has no script mode of
  its own). Management reply lines are printed with `token=<32 hex>` replaced
  by `token=<redacted>` in both output shapes, because `@s?` is the one line
  that carries the token — Python has no LAN mode and so never sends it.
  Covered by `lan_tokens_resolve_and_validate_before_dialing`,
  `the_lan_token_help_names_every_source` and
  `the_socket_status_line_is_redacted_wherever_it_is_printed`.

## 5. Workstream T — TUI (`src/tui/`)

OpenCode-style shell rendered with ratatui; feature parity with
`specs/WEB_UX_SPEC.md`. Layout:

- **Top status bar**: connection state dot + text, transport (BLE/LAN),
  device label/id, RX/TX counters, baud, execution mode, clock.
- **Left sidebar**: connection card (connect/disconnect/switch device/transport
  toggle + LAN host/token fields), quick-send presets
  (`help`, `version`, `uname -a`, `df -h`, `reboot` with confirm), watch
  findings (from `watch::SerialWatch` fed by `CoreEvent::UartRx`), section
  links.
- **Center**: terminal pane — a real VT grid: feed `UartRx` through `vte` into
  a scrollback buffer (≥10 000 lines), render with ANSI colors incl. 256-color
  SGR, cursor, local echo, Enter mode, click-free keyboard input with the key
  bar equivalents (Esc/Tab/arrows/Ctrl combos — encode like
  `web/terminal_keys.js`: CSI `\x1b[A..D`, modifiers `\x1b[1;<mod>A`, etc.).
  Toolbar actions: font size ±, autoscroll toggle, copy (OSC 52 or selection),
  clear, save log to file.
- **Right/bottom panel**: assistant chat (messages, tool-call blocks,
  approval dialogs, usage line, mode picker manual/auto/full-auto, settings
  dialog, export report). Implements `ApprovalBroker` with a modal dialog.
- **Views**: switchable Terminal / Diagnostics (`@i?` parsed into the grid of
  the web panel: Firmware, Uptime, BLE access, UART Buffer, WiFi, Upload
  Queue) / Network (WiFi scan list, connect form, WebDAV) / Assistant.
  BLE-only controls disabled when the transport is LAN (mirror
  `specs/WEB_UX_SPEC.md` §3.6 matrix).
- **Bottom status line + command palette** (`Ctrl+P`) + help overlay (`?`):
  palette lists every action (connect, set uart, scan wifi, save log, switch
  view, toggle autoscroll, switch language, ask assistant, quit...). Global keys: `Ctrl+Q`
  quit (confirm when connected), `Ctrl+Shift+K` focus assistant (web parity),
  `F1` help, `F2..F4` views, `Ctrl+L` clear terminal.
- Modal dialogs: UART settings (validated by `protocol::validate`), WiFi
  (scan results, password masked), WebDAV, device info, confirmations
  (reboot preset, quit, disconnect), toasts/notice log (`CoreEvent::Notice`).

Reuse library code, never fork it: UART specs go through
`protocol::validate::normalize_uart_spec`, management replies through
`session::SessionHandle::request_mgmt`, watch through `watch::SerialWatch`.

Config persistence under `dirs::config_dir()/linkr/`: `tui.json`
(font size, enter mode, transport, last host, view) — same spirit as the web
`linkr-*` localStorage keys (WEB_UX_SPEC §9 lists them). The LAN access token
is deliberately **not** in `tui.json`: it lives in `lan_tokens.json`, written
`0600` in the web store's own `{tokens, hosts}` shape
(`web/lan_token_store.js`), filled from `@s?` during a BLE session
(`requestDeviceState()` in `web/app.js`: diagnostics + `@s?` when the
`MGMT_CAP_WEBSOCKET` bit is set) and read back as the token of a host alias —
which is what makes a LAN dial work with an empty token field.

## 6. Workstream A — assistant

Port `mobile/src/pi-agent.mjs` + `web/agent_*` per `specs/AGENT_SPEC.md`:

- Agent loop (OpenAI-compatible chat completions with streaming SSE via
  `reqwest`; also Anthropic Messages and Google Gemini request shapes as
  specified), system prompt parity, budgets (32 rounds / 96 tool calls /
  15 min — confirm in the spec), context assembly with the serial journal.
- All tools from the spec (15 base + 5 accessory + notes + 2 verify + watch),
  exact JSON schemas and result shapes, `untrusted` flags, evidence
  discipline literals.
- Execution policy: three modes, destructive-command regexes, low-risk
  allowlist (exact strings), per-device `alwaysAsk`/`allow` lists
  (storage `linkr-agent-command-policy-v1` equivalent file), approval flow
  through `ApprovalBroker`.
- Journal (`journal.rs`), watch (`watch.rs`), target file paging
  (`target_files.rs`), verify (`target_verify.rs`) as pure, unit-tested ports —
  marker strings must match `web/target_files.js` byte-for-byte (add a test
  that greps the JS source, like the repo's Python contract tests do).
- Config: `agent::load_config/save_config/clear_config` over
  `dirs::config_dir()/linkr/agent.json`, validation messages from the spec,
  plaintext-http consent rule, pricing file `agent_pricing.json`.
- Report export: `build_report(...) -> String` (Markdown, structure from
  `web/agent_report.js`) — the TUI writes it to a file.
- Usage accounting: token counts + cost from configured prices, emitted as
  `AgentEvent::Usage`.
- Stop/cancel semantics and the exact abort/rejection strings.

`spawn(config, broker, session, bus)` subscribes to `bus` itself (maintains
the journal and watch), owns the task queue, and streams `AgentEvent`s.

## 7. Verification

- `cargo test` (unit + contract tests) and `cargo check --all-targets` green.
- Existing Python suite must stay green: `python3 -m unittest discover -s tests`
  (run from the repo root; `tests/test_regressions.py` and others read source
  files — do not modify files outside your ownership).
- After integration the integrator runs: `cargo clippy --all-targets`,
  `cargo fmt --check` (format your files with `cargo fmt` before finishing),
  the Python suite, and a scripted smoke run of the CLI
  (`linkr --help`, `linkr --version`, `linkr completion bash`,
  `linkr --print-completion zsh`, `linkr --scan --timeout 1` without hardware).

## 8. Packaging (integrator, after merge)

`build_terminal.sh` / `build_terminal.ps1` (cargo release build), a
self-extracting `linkr.ps1` bundle generator, CI matrix for
linux/windows/macos × x86_64/arm64 (+ armv7 where zigbuild allows), docs.
