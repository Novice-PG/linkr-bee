# Session Shell integration

The web terminal (including Android/HarmonyOS shells and its shared MCP executor)
can explicitly enable an in-memory POSIX Shell helper. This is opt-in for each
connection/conversation. It does not change firmware, Shell startup files,
`PROMPT_COMMAND`, traps, terminal echo or target permissions. The CLI/TUI retains
its existing tracking protocol.

The transport follows the [OSC 633 property envelope](https://code.visualstudio.com/docs/terminal/shell-integration#supported-escape-sequences).
`Linkr` is a custom property; it does not claim compatibility with VS Code's
command lifecycle. No clipboard, title or notification escape code is emitted.

## Wire format

```text
ESC ] 633 ; P ; Linkr=1:<nonce>:<sequence>:<phase>[:<exit-code>] BEL
```

The helper prefixes each property with CAN (`0x18`) to cancel any unfinished
terminal control string left by the child before emitting a new boundary.
The reader also accepts `ESC \` as the string terminator. Fields are strict:

| Field | Allowed values |
| --- | --- |
| nonce | 32 lowercase hexadecimal characters, fresh for each enable attempt |
| sequence | 0 for readiness; 1–999999999 for commands, no leading zeros |
| phase | `R` ready, `C` command started, `D` command finished |
| exit-code | Required only for `D`, integer 0–255 |

The one-time setup defines `__linkr_run_v1` and reserved `__linkr_*` variables. An
unrelated function with that name is refused; a helper marked `linkr-shell-v1`
can be replaced on a subsequent enable. Each ordinary wire command is:

```sh
__linkr_run_v1 'original command with POSIX single-quote escaping'
```

The helper advances its counter, emits `C`, executes `command sh -c "$1"`, then
emits `D` with the captured status. The helper itself returns 0 so a failing
child does not unexpectedly terminate a parent with `set -e`. Child `exit`, `cd`
and environment changes do not affect the interactive parent. Interrupts can
prevent a final boundary: absence of `D` never establishes success or failure.

## Correlation and recovery

- Activation requires an idle observed Shell, current connection, unchanged
  input revision, no active or unresolved executor action, and a matching `R`.
- Ordinary completion requires matching nonce/sequence and a `C` before `D` in
  the received raw cursor range. A prompt or a literal echoed escape is not a
  completion event. Old sessions, malformed frames and mismatched sequences
  cannot complete the operation.
- Recognition never grants approval. The original command and wire payload
  remain subject to existing policy, including Full Auto destructive guards.
- Handshake timeout, lost helper, stripped OSC or sequence mismatch do not resend
  input. Completion remains unknown. Disabling affects subsequent requests only;
  no cleanup command is sent. The Shell helper is not automatically reinstalled.
- A new connection/conversation, cleared logs or detected boot/login requires
  explicit enable again. Exit the Shell to discard its temporary function.
- The nonce is correlation data visible to the target, not a cryptographic
  identity or a trust boundary. Exit 0 does not verify the user's objective.

## Logging

Raw receive archives and firmware WebDAV retain all original bytes. The plain
`SerialJournal` masks terminal controls while preserving raw UTF-16 cursor
positions. A separate bounded parser records only complete execution events;
reset and ring eviction remove stale events. Nothing is reconstructed from an
already sanitized or replayed terminal view.

Reading presentation consumes only registered matching boundaries and exact
command echoes. It fails open on unknown/partial sequences. Historical view
replay never invokes the protocol reader or writes terminal replies to UART.

Validation uses unit tests, real local sh/bash subprocesses and browser tests
with a temporary POSIX Shell; physical Bee UART acceptance is separate.
