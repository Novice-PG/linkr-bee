# linkr — Linkr Bee terminal, TUI and assistant

`linkr` is the cross-platform host client for the Linkr Bee wireless serial
bridge: a BLE (or LAN) terminal, an OpenCode-style TUI, and an assistant that
can probe the device, read and verify target files, and report on what it did.
It is written in Rust and builds on Linux, Windows and macOS, on x86_64 and
arm64, with no runtime dependency beyond the OS Bluetooth stack.

The command surface, the terminal behaviour, the TUI layout and the assistant
are specified against the clients that already exist:

| Spec | Pins down |
| --- | --- |
| [specs/PYTHON_CLI_SPEC.md](specs/PYTHON_CLI_SPEC.md) | flags, protocol, geometry, exit codes |
| [specs/WEB_UX_SPEC.md](specs/WEB_UX_SPEC.md) | TUI layout, views, settings, dialogs |
| [specs/AGENT_SPEC.md](specs/AGENT_SPEC.md) | tools, context, policy, providers, reports |
| [CONTRACTS.md](CONTRACTS.md) | module ownership and the acceptance bar |

## Build

### Linux and macOS

```sh
tools/build_terminal.sh          # fmt + clippy + tests, then a release build
dist/linkr-terminal-linux-aarch64/linkr --version
```

Options: `--target TRIPLE` to cross-build, `--debug` for the dev profile,
`--no-verify` to skip the checks. The result lands in
`dist/linkr-terminal-<slug>/` next to a `SHA256SUMS` file.

### Windows

```powershell
powershell -ExecutionPolicy Bypass -File tools\build_terminal.ps1
powershell -ExecutionPolicy Bypass -File tools\build_terminal.ps1 -Bundle
```

`-Target <triple>` cross-builds (for example `aarch64-pc-windows-msvc`),
`-Dev` builds the dev profile, `-NoVerify` skips the checks, and `-Bundle`
also wraps the result in the self-extracting script below.

### Self-extracting Windows bundle

`dist\linkr-bee-terminal.ps1` is one file that carries `linkr.exe` inside it:

```powershell
.\dist\linkr-bee-terminal.ps1 --help
.\dist\linkr-bee-terminal.ps1 --name "Linkr BLE UART" --scan
.\dist\linkr-bee-terminal.ps1 --tui
```

It unpacks the executable once into `%LOCALAPPDATA%\LinkrBee\bin`, verifies the
embedded SHA-256 before every run (a stale or edited copy is replaced), and
forwards the arguments, the console and the exit code of the terminal. Delete
that folder to force a fresh unpack. Generate it from any Windows executable:

```sh
python3 tools/build_terminal_bundle.py --exe path/to/linkr.exe \
    --output dist/linkr-bee-terminal.ps1
```

`tests/test_terminal_bundle.py` covers the generator.

CI (`.github/workflows/build.yml`, job `terminal`) builds the five targets —
linux x86_64 and arm64, windows x86_64, macOS arm64 and x86_64 — runs the test
suite and the lints, and uploads the binaries plus the bundle as artifacts.

### Cross-build the Windows binary from Linux

`tools/build_terminal.sh --target x86_64-pc-windows-gnu` emits
`dist/linkr-terminal-x86_64-pc-windows-gnu/linkr.exe`. It needs mingw-w64;
unpacking the packages somewhere local is enough, nothing is installed
system-wide:

```sh
rustup target add x86_64-pc-windows-gnu
mkdir -p mingw && cd mingw
apt-get download gcc-mingw-w64-x86-64 binutils-mingw-w64-x86-64 \
    mingw-w64-x86-64-dev mingw-w64-common
for f in *.deb; do dpkg-deb -x "$f" "$PWD"; done
# The plain `x86_64-w64-mingw32-gcc` name comes from update-alternatives,
# which does not run when a .deb is only unpacked.
ln -sf x86_64-w64-mingw32-gcc-posix usr/bin/x86_64-w64-mingw32-gcc
cd ..

PATH="$PWD/mingw/usr/bin:$PATH" \
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
    tools/build_terminal.sh --target x86_64-pc-windows-gnu
```

The exe links the mingw runtime statically and imports Windows system DLLs
only, so it runs without any MinGW installation. CI still ships the
`x86_64-pc-windows-msvc` build; use `build_terminal.ps1` on Windows for that
one. Either exe can be wrapped into the self-extracting script:

```sh
python3 tools/build_terminal_bundle.py \
    --exe dist/linkr-terminal-x86_64-pc-windows-gnu/linkr.exe \
    --output dist/linkr-bee-terminal.ps1
```

## Use

```sh
linkr --scan                        # list nearby BLE devices and exit
linkr --tui                         # connect, then open the TUI
linkr --query-info --no-terminal    # @i? diagnostics without a session
linkr --uart 115200,8,n,1,n --tui   # set the UART before handing over
linkr --lan 192.168.1.10 --lan-token <32 hex>   # through the LAN bridge
linkr completion zsh > _linkr       # bash, fish, zsh and powershell
```

`--help` lists every flag; the terminal-only flags (`--enter`, `--line-mode`,
`--escape`, `--ble-write-size`, `--log-file`, …) match the Python client, and
the geometry command the session sends to the target is byte-for-byte the one
from `web/terminal_geometry.js`.

### TUI

Four surfaces, switchable without leaving the keyboard:

| Key | Action |
| --- | --- |
| `F2` `F3` `F4` `F5` | Terminal · Diagnostics · Network · Assistant |
| `Ctrl+Shift+K` | jump straight to the assistant |
| `F1` | help |
| `Ctrl+P` | command palette |
| `Ctrl+L` | clear the terminal pane |
| `Ctrl+Q` | quit (asks first) |
| `Ctrl+Up` | focus the sidebar |
| `Ctrl+Shift+R` / `+C` / `+A` | latch Shift / Ctrl / Alt for the next key |

The latched modifiers are the TUI's stand-in for the web terminal's key bar.
The assistant view mirrors the browser panel: tools, approval prompts, tasks,
memory, and the report the run produces.

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | success |
| `1` | runtime error (device, transport, command failure) |
| `2` | usage error |
| `3` | the device disappeared mid-session |
| `130` | interrupted (Ctrl+C) |

## Development

```sh
cd tools/linkr-cli
cargo test              # unit and contract tests
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Layout:

```text
src/cli.rs          argument parsing, dispatch, exit codes
src/session.rs      BLE/LAN session, management requests, terminal loop
src/protocol/       geometry, management, UART, validation
src/transport/      BLE (btleplug) and LAN WebSocket
src/tui/            OpenCode-style TUI (ratatui + crossterm)
src/agent/          assistant: executor, provider, context, policy, report
```

The repository-wide Python suite (`python3 -m unittest discover -s tests` from
the repo root) must stay green alongside `cargo test`; it exercises the
scripting helpers in `tools/` as well.

## License

Apache-2.0
