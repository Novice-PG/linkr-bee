# bee-cli — the Linkr Bee command-line client

This archive is the **CLI-only** build of Linkr Bee. The executable is
`bee-cli`: the same protocol and command-line implementation as the interface
build, compiled without the terminal interface. It is meant for scripts and
agent harnesses that want one request, one answer and an exit code.

The GUI build is shipped separately as `bee`. Both builds speak the same
management protocol over Bluetooth LE or the LAN WebSocket bridge. There is no
interactive terminal in this archive, so `bee-cli` with no action reports that
this build has no interface and exits with status 2.

## Check the build

```sh
./bee-cli --version          # bee-cli 0.5.0
./bee-cli --capabilities     # operations, defaults and exit codes
./bee-cli --licenses         # third-party notice (also in THIRD_PARTY_LICENSES.md)
```

## Connect

| Transport | Flag |
|---|---|
| Bluetooth address | `--ble-address EE:C7:42:34:48:CF` |
| Bluetooth name fragment | `--ble-name "UART-3"` |
| LAN WebSocket | `--lan-host 192.168.0.102` |
| LAN token | `--lan-token <TOKEN>`, `--lan-token-file <FILE>` or `$LINKR_LAN_TOKEN` |
| Offline loopback | `--bridge -` |

Management commands (`@i?`, `@u?`, `@w?`, `@d?`, `@s?`) are available only over
BLE. On a LAN or stdio link they report that the management channel is absent
and exit with status 1.

## Common requests

```sh
bee-cli --scan                         # list nearby Bluetooth devices
bee-cli --query-info                   # @i?
bee-cli --query-uart                   # @u?
bee-cli --query-wifi                   # @w?
bee-cli --query-webdav                 # @d?
bee-cli --query-lan                    # @s?
bee-cli --wifi "MySSID,MyPassword"     # join a network
bee-cli --wifi "MySSID" --wifi-key-file pw.txt
bee-cli --wifi-scan
bee-cli --wifi-off
bee-cli --webdav http://dav.example/logs/
bee-cli --webdav-off
bee-cli --uart 115200,8,n,1,none
bee-cli --loopback-test 414243
bee-cli --exec "uname -a"
```

A command line may contain several requests; they are sent one at a time
because the bridge answers one management question at a time. Add `--json` for
JSON Lines suitable for a program, and `--quiet` to suppress progress on stderr.
`--no-terminal` is accepted for compatibility; this build has no interface to
open.

WebDAV currently accepts only anonymous `http://host[:port]/path/` endpoints;
HTTPS, credentials, queries and fragments are unsupported. Empty URLs are
rejected; use `--webdav-off` explicitly to disable upload.

WebDAV upload is part of the protocol, but a particular firmware build may
leave it out (the ESP32-WROOM build disables it for DRAM). In that case the
board's own error is reported; the host does not hide the request.

## Keep a session open

`--serve` dials once and answers JSON requests from standard input until the
input ends or a request says `quit`:

```sh
bee-cli --ble-name "UART-3" --serve
{"event":"ready","program":"bee-cli","version":"0.5.0","link":"ble:name:UART-3","ops":[…]}
{"id":1,"op":"state"}
{"event":"begin","id":1,"op":"state"}
{"event":"result","what":"state","live":false,…}
{"event":"end","id":1,"code":0}
{"id":2,"op":"exec","cmd":"uname -a"}
{"event":"begin","id":2,"op":"exec"}
…
{"event":"end","id":2,"code":0}
{"id":3,"op":"quit"}
{"event":"end","id":3,"code":0}
```

The `ops` list in the ready event reports what the current transport can serve.
The same operations are available as flags; `--capabilities` lists the full set
for this build. Request fields are documented by the `capabilities` output and
the project repository.

## Exit codes

| Code | Meaning |
|---|---|
| 0–125 | With a single `--exec`, the target command's own status |
| 1 | The request ran but failed (timeout, refusal, missing management channel, or any failed request in a mixed batch) |
| 2 | Usage error: unknown or missing arguments, or a conflicting command line |
| 255 | The link could not be opened or was lost |

A mixed batch reports failure as 1; the target command's own status remains in
its `result` event.
