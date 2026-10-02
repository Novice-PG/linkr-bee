# Implementation Spec: Port `tools/linkr_ble_terminal.py` (v1.0.0) to Rust

Target file: `/home/rock/Documents/linkr-bee/tools/linkr_ble_terminal.py` (1424 lines). Everything below is derived line-by-line from that file and is sufficient to write a behavior-identical Rust port.

---

## 1. Constants

### 1.1 Identity / metadata
```text
__version__      = "1.0.0"
DEFAULT_NAME     = "Linkr BLE UART"
program help description = "Terminal over BLE Nordic UART Service for Linkr Bee bridge"
--version output = "{prog} 1.0.0"     (prog = argparse's basename of argv[0])
```

### 1.2 GATT UUIDs (all lowercase strings)
```text
NUS_RX_UUID              = "6e400002-b5a3-f393-e0a9-e50e24dcca9e"   # host -> device UART data (write)
NUS_TX_UUID              = "6e400003-b5a3-f393-e0a9-e50e24dcca9e"   # DEFINED BUT NEVER SUBSCRIBED (dead constant)
MGMT_PROTOCOL_UUID       = "4c4b0002-9a7e-4f4e-8b8a-3d6f12a0c001"   # read once at connect
MGMT_DEVICE_ID_UUID      = "4c4b0003-9a7e-4f4e-8b8a-3d6f12a0c001"   # read once at connect
MGMT_COMMAND_UUID        = "4c4b0004-9a7e-4f4e-8b8a-3d6f12a0c001"   # host writes command frames
MGMT_RESPONSE_UUID       = "4c4b0005-9a7e-4f4e-8b8a-3d6f12a0c001"   # start_notify -> ManagementChannel.on_indication
RELIABLE_UART_RX_UUID    = "4c4b0011-9a7e-4f4e-8b8a-3d6f12a0c001"   # host writes data frames
RELIABLE_UART_TX_UUID    = "4c4b0012-9a7e-4f4e-8b8a-3d6f12a0c001"   # start_notify -> ReliableUartChannel.on_indication
RELIABLE_UART_STATE_UUID = "4c4b0013-9a7e-4f4e-8b8a-3d6f12a0c001"   # read once at connect (16 bytes)
```

**Important:** there is NO `start_notify(NUS_TX_UUID, ...)` anywhere in the file. All inbound UART bytes arrive via the Reliable UART indication handler; the NUS path is write-only and unreachable (see §5).

### 1.3 Protocol constants
```text
MGMT_HEADER         = struct("<2sBBIHH")   # 12 bytes
RELIABLE_UART_HEADER= struct("<2sBBIHH")    # 12 bytes (same layout, different magic)
MGMT_API_MAJOR      = 1
MGMT_CAP_WIFI        = 1 << 0   # 0x01
MGMT_CAP_WEBDAV      = 1 << 1   # 0x02
MGMT_CAP_DEVICE_ID   = 1 << 3   # 0x08
MGMT_CAP_ASYNC_EVENTS= 1 << 4   # 0x10
MGMT_CAP_RELIABLE_UART = 1 << 5 # 0x20
MGMT_FLAG_FINAL      = 1 << 0   # 0x01
MGMT_FLAG_ERROR      = 1 << 1   # 0x02

WIFI_OPERATION_TIMEOUT = 35.0   # seconds, wait-for-FINAL timeout
WIFI_SCAN_TIMEOUT      = 35.0   # seconds, wait-for-FINAL timeout
MGMT_RESPONSE_TIMEOUT  = 5.0    # hard-coded in ManagementChannel.send (first response)

UART_BAUD_MIN  = 300
UART_BAUD_MAX  = 3_000_000
UART_DATA_BITS = ("5","6","7","8")
UART_STOP_BITS = ("1","2")
UART_PARITY       = {"n":"n","none":"n","o":"o","odd":"o","e":"e","even":"e"}
UART_FLOW_CONTROL = {"n":"n","none":"n","off":"n","rtscts":"rtscts","hw":"rtscts"}

MIN_TERMINAL_COLUMNS  = 2
MIN_TERMINAL_ROWS     = 2
MAX_TERMINAL_DIMENSION= 1000
GEOMETRY_LINE_BUFFER  = 1024   # _line keeps last 1024 chars

WIFI_PASSWORD_ENV = "LINKR_WIFI_PASSWORD"

EXIT_OK                   = 0
EXIT_ERROR                = 1
EXIT_USAGE                = 2
EXIT_DEVICE_DISCONNECTED  = 3
EXIT_KEYBOARD_INTERRUPT   = 130  # returned from main() on KeyboardInterrupt

RELIABLE_MAX_PAYLOAD_CAP  = 232  # clamp applied to device-advertised reliable payload
```

### 1.4 Output helpers (exact prefixes)
```text
stderr(msg) : write msg + "\n" to stderr, flush; NO prefix
info(msg)   : if not QUIET: stderr("linkr: " + msg)
warn(msg)   : stderr("linkr: warning: " + msg)
error(msg)  : stderr("linkr: error: " + msg)
```
`QUIET` is a global set from `--quiet` **before** `run()` executes.

### 1.5 Python `bytes`/`str` repr rules needed for byte-identical traces
Debug/loopback lines use Python `!r` on bytes:
`b'...'`, inner `'` escaped as `\'`, `\` as `\\`, tab=`\t`, CR=`\r`, LF=`\n`, other bytes <0x20 or >=0x7F as `\xNN` (lowercase hex). Printable ASCII (0x20–0x7E) emitted literally. Examples: `TX b'hello\r\n'`, `RX b'\x1b'`.
For `{str!r}` (used in `f"invalid UART baud rate: {baud!r}"`) Python uses single quotes: `'abc'`.

---

## 2. MGMT_HEADER — exact byte layout and reassembly

### 2.1 Layout (`struct "<2sBBIHH"`, little-endian, no padding, total 12 bytes)

| Offset | Size | Type | Field | TX value | RX meaning |
|---|---|---|---|---|---|
| 0 | 2 | `2s` | magic | `b"LK"` | must equal `b"LK"` |
| 2 | 1 | `B` | version | `1` (`MGMT_API_MAJOR`) | must equal `1` |
| 3 | 1 | `B` | message_type | `1` (host command) | `2` = response, `3` = event; anything else invalid |
| 4 | 4 | `I` | request_id | monotonically increasing u32 | correlates to outstanding request |
| 8 | 2 | `H` | length ("expected") | `len(command)` (payload byte count) | payload byte count; `0` invalid; must be `<= max_payload` |
| 10 | 2 | `H` | flags | `0` | bit0 `FINAL` (0x01), bit1 `ERROR` (0x02) |

### 2.2 TX frame (`ManagementChannel.send`)
```
frame = pack("<2sBBIHH", "LK", 1, 1, request_id, len(command), 0) + command
```
`request_id` allocation:
```
id = next_request_id           # starts at 1
next_request_id = 1 if id == 0xFFFFFFFF else id + 1     # wrap: 0xFFFFFFFF -> 1
```
The pending waiter futures are registered **before** any GATT write (a response arriving mid-write must resolve).

### 2.3 TX chunking
```
size = max(20, cfg.write_size)          # note: NOT clamped to 244 here (write_size already <=244)
for offset in 0..frame.len() step size:
    chunk = frame[offset .. offset+size]
    if debug_io: stderr(sensitive ? f"MGMT TX #{id} <redacted {len(chunk)} bytes>"
                                 : f"MGMT TX #{id} {python_repr(chunk)}")
    write_gatt_char(MGMT_COMMAND_UUID, chunk, response = true)   # ALWAYS with-response
```
No write delay is applied on this path.

### 2.4 RX reassembly state machine (`on_indication`, one message at a time)

State: `current: Option<{type, request_id, expected, flags, payload: Vec<u8>}>`, initially `None`.

```
on_indication(fragment_bytes):
  if current is None:
      if len(fragment) < 12 or fragment[0..2] != b"LK":
          warn("management <- orphaned response fragment"); return          # state unchanged
      (magic, version, msg_type, request_id, expected, flags) = unpack(fragment[0..12])
      if magic != b"LK" or version != 1 or msg_type not in (2,3)
         or expected == 0 or expected > max_payload:
          warn("management <- invalid response header"); current = None; return
      current = {type: msg_type, request_id, expected, flags, payload: []}
      fragment = fragment[12..]
  msg = current
  if len(msg.payload) + len(fragment) > msg.expected:
      warn("management <- oversized response"); current = None; return      # whole message dropped
  msg.payload += fragment
  if len(msg.payload) != msg.expected: return                               # wait for more fragments
  current = None
  ... complete-message handling (§2.5) ...
```
Notes for the port:
- Only the **first** fragment of a message carries the header; subsequent fragments are pure payload and are not validated.
- A new header arriving while `current` is Some is consumed as payload (usually triggers the "oversized" drop).
- There is **no reassembly timeout**; a stalled message keeps `current` set forever.
- `max_payload` here is the device-advertised management payload limit read from `MGMT_PROTOCOL_UUID` (§3.1).

### 2.5 Complete-message handling
```
body  = bytes(payload)
kind  = (msg.type == 3) ? "event" : "response"
text  = String::from_utf8_lossy(body) with trailing '\r'/'\n' stripped (rstrip "\r\n")
lines = text.split_lines() ; if empty -> [""]           # Python splitlines(): splits \n, \r\n, \r
failed = (flags & MGMT_FLAG_ERROR) != 0
command_text = commands.pop(request_id)                 # Option<String>, may be None

if json_output:
    record = {"type": kind, "requestId": request_id, "ok": !failed, "lines": lines}
    if command_text is Some: record["command"] = command_text
    println_line_stdout(json.dumps(record, ensure_ascii=False)); flush
    # Python separators are (", ", ": ")
else:
    for line in lines: stderr(f"{kind} #{request_id} <- {line}")
    # e.g. "response #1 <- fw=1.2.3"
```
**Correlation / resolution:**
```
if msg.type == 2:                                     # RESPONSE
    fut = pending.pop(request_id)
    if fut not done:
        fut.set_exception(RuntimeError(text or "management request failed"))  if failed
        else fut.set_result(body)
elif flags & MGMT_FLAG_FINAL:                         # EVENT with FINAL
    fut = pending_final.pop(request_id)
    if fut not done:
        fut.set_exception(RuntimeError(text or "management operation failed")) if failed
        else fut.set_result(body)
# EVENT without FINAL: printed only, resolves nothing.
# Unknown request_id: still printed (no "command" key), pending lookup misses silently.
```
**Critical subtlety:** the `type==2` branch is `if`, the FINAL branch is `elif`. A single **response (type 2) that also carries FINAL resolves only `pending`**; a caller waiting on `pending_final` for that request will hit its timeout. Behavior-identical code must reproduce this (`wait_final` only succeeds if a type-3 FINAL event arrives).

### 2.6 `send()` full algorithm
```
async send(command: &[u8], delay=0.0, wait_final_timeout=0.0) -> Result<Vec<u8>>:
  if command.is_empty() or command.len() > max_payload:
      raise ValueError("management command is outside the advertised limit")   # -> exit 1
  sensitive = command.starts_with(b"@w=") or command.starts_with(b"@d=")
  display   = sensitive ? format!("{}=<redacted>", first 2 bytes as UTF-8)   # "@w=<redacted>" / "@d=<redacted>"
                        : String::from_utf8_lossy(command)
  info("control -> " + display)                     # "linkr: control -> @i?"
  request_id = allocate (§2.3)
  commands[request_id] = display
  frame = header + command (§2.2)
  pending[request_id] = new future                   # registered BEFORE writing
  final_future = wait_final_timeout != 0 ? new future in pending_final[request_id] : None
  try:
      chunked write (§2.3)
      response  = wait(pending[request_id],  timeout = 5.0)      # TimeoutError propagates -> exit 1
      if final: wait(final_future, timeout = wait_final_timeout)
      if delay > 0: sleep(delay)                                 # no caller passes delay today
      return response
  finally:
      pending.remove(request_id); pending_final.remove(request_id); commands.remove(request_id)
```
**Error propagation summary**
- Device `MGMT_FLAG_ERROR` → `RuntimeError(text or "management request failed"/"management operation failed")` → caught by `main()` → `linkr: error: {msg}` → exit **1**.
- No response within 5 s → `asyncio.TimeoutError` (`str()` is empty → prints literally `linkr: error: `) → exit **1**.
- No FINAL within wait window → same → exit **1**.
- Malformed frames → warnings only, request then times out (5 s) → exit 1.
- `ValueError` (command too big/empty) → exit 1.

---

## 3. Management command set

### 3.1 Capability/protocol handshake (before any command)

```
protocol = read_gatt_char(MGMT_PROTOCOL_UUID)          # at least 10 bytes
if len(protocol) < 10 or protocol[0] != 1: raise "unsupported Linkr Management API version"
capabilities     = LE u32 at protocol[4..8]
management_max   = LE u16 at protocol[2..4]
if management_max == 0: raise "invalid Linkr Management payload limit"
if !(capabilities & 0x08): raise "device does not advertise Device ID support"
if !(capabilities & 0x20): raise "device does not advertise Reliable UART support"
wifi_actions       = --wifi || --wifi-scan || --wifi-off || --query-wifi
if wifi_actions && !(capabilities & 0x01): raise "device does not advertise WiFi support"
async_wifi_actions = --wifi || --wifi-scan || --wifi-off          # NOTE: --query-wifi excluded
if async_wifi_actions && !(capabilities & 0x10): raise "device does not advertise async event support"
webdav_actions     = --webdav || --webdav-off || --query-webdav
if webdav_actions && !(capabilities & 0x02): raise "device does not advertise WebDAV support"

device_id = read_gatt_char(MGMT_DEVICE_ID_UUID)
if len != 16: raise "invalid Linkr Device ID length"
info(f"management API v{protocol[0]}.{protocol[1]}, device ID {device_id.hex()}")   # lowercase hex

management = ManagementChannel(client, cfg, management_max, json=args.json)
start_notify(MGMT_RESPONSE_UUID, management.on_indication)

state = read_gatt_char(RELIABLE_UART_STATE_UUID)
if len(state) != 16 or state[0] != 1: raise "unsupported Reliable UART version"
rel_max_payload = LE u16 at [2..4]
rel_tx_sequence = LE u32 at [4..8]
rel_rx_sequence = LE u32 at [8..12]
if rel_max_payload == 0 or rel_tx_sequence == 0 or rel_rx_sequence == 0: raise "invalid Reliable UART state"
reliable = ReliableUartChannel(..., data_handler = on_notify)
start_notify(RELIABLE_UART_TX_UUID, reliable.on_indication)
```

### 3.2 Command table (exact literals, in the exact execution order inside the connected session)

| # | Flag(s) | Command bytes | Channel | Capability gate | wait_final | Redacted in traces/JSON |
|---|---|---|---|---|---|---|
| 1 | `--query-info` | `@i?` | MGMT_COMMAND_UUID | none | no | no |
| 2 | `--uart SPEC` | `@u=` + canonical spec (§7.3) | MGMT | none | no | no |
| 3 | `--query-uart` | `@u?` | MGMT | none | no | no |
| 4 | `--wifi ...` | `@w={ssid},{password}` | MGMT | WIFI + ASYNC_EVENTS | **yes, 35.0 s** | **yes** → `@w=<redacted>` |
| 5 | `--wifi-off` | `@w off` | MGMT | WIFI + ASYNC_EVENTS | **yes, 35.0 s** | no |
| 6 | `--query-wifi` | `@w?` | MGMT | WIFI only | no | no |
| 7 | `--wifi-scan` | `@w scan` | MGMT | WIFI + ASYNC_EVENTS | **yes, 35.0 s** | no |
| 8 | `--webdav URL` | `@d=` + raw value | MGMT | WEBDAV | no | **yes** → `@d=<redacted>` |
| 9 | `--webdav-off` | `@d off` | MGMT | WEBDAV | no | no |
| 10 | `--query-webdav` | `@d?` | MGMT | WEBDAV | no | no |
| — | `--loopback-test [P]` | payload bytes (default `"A"`) | UART data path (reliable) | RELIABLE_UART | n/a | no |
| — | geometry sync | `stty rows R cols C >/dev/null 2>&1\r` | UART data path (reliable) | RELIABLE_UART | n/a | no |
| — | terminal stdin | arbitrary bytes | UART data path (reliable) | RELIABLE_UART | n/a | no |

Notes:
- Redaction trigger is **prefix-based**: only `@w=` and `@d=`. `@w off` / `@w scan` / `@d off` are transmitted in the clear in traces and JSON.
- Redacted display = first two bytes decoded + `"=<redacted>"` → `@w=<redacted>`, `@d=<redacted>`.
- The `info("control -> {display}")` line prints **before** the request id is allocated (suppressed by `--quiet`).
- All `@` commands go over the BLE management characteristic; nothing here uses a serial/USB transport (the tool is BLE-only).
- There is **no mutual exclusion**: e.g. `--wifi ... --wifi-off` sends both, in table order (connect then forget).

### 3.3 JSON output shape (`--json`)
One JSON object per complete management message, printed to **stdout**, one line, `ensure_ascii=False`, Python default separators `", "` / `": "`, flushed:
```json
{"type": "response", "requestId": 1, "ok": true, "lines": ["..."], "command": "@i?"}
{"type": "event", "requestId": 4, "ok": false, "lines": ["ERR ..."], "command": "@w=<redacted>"}
```
- `type`: `"response"` (msg_type 2) or `"event"` (msg_type 3).
- `ok`: `not (flags & MGMT_FLAG_ERROR)`.
- `lines`: payload decoded UTF-8 with `errors="replace"`, `rstrip("\r\n")`, `splitlines()`, or `[""]` if empty.
- `command`: present only if a request with that id is still tracked (removed as soon as the first message for that id arrives, and always in `send()`'s `finally`).
- UART/terminal bytes are written raw to stdout independently → JSON and terminal output **interleave on stdout**. Non-JSON management lines go to **stderr**; terminal bytes to **stdout**.

---

## 4. RELIABLE_UART_HEADER — framing, sequencing, write path

### 4.1 Layout (`struct "<2sBBIHH"`, 12 bytes)

| Offset | Size | Type | Field | TX | RX |
|---|---|---|---|---|---|
| 0 | 2 | `2s` | magic | `b"LR"` | must be `b"LR"` |
| 2 | 1 | `B` | version | `1` | must be `1` |
| 3 | 1 | `B` | flags | `0` | **ignored** (`_flags`) |
| 4 | 4 | `I` | sequence | current `tx_sequence` | frame sequence |
| 8 | 2 | `H` | length | `len(payload)` | expected payload bytes; `0` invalid; `> max_payload` invalid |
| 10 | 2 | `H` | reserved | `0` | **ignored** (`_reserved`) |

### 4.2 Construction / state
```
max_payload = max(1, min(device_advertised_max_payload, 232))    # 232 = 244 - 12
tx_sequence = advertised_tx or 1        # (0 already rejected by validation)
rx_sequence = advertised_rx or 1
next_sequence(s) = (s == 0xFFFFFFFF) ? 1 : s + 1
```

### 4.3 RX reassembly + sequence rules (`on_indication`)
Identical structure to §2.4 with these exact differences:
- Orphan/magic failure → `info("reliable UART <- orphaned fragment")` (info, not warn).
- Invalid header (magic≠`LR`, version≠1, `sequence==0`, `expected==0`, `expected>max_payload`) → `warn("reliable UART <- invalid frame header")`, drop.
- Oversized → `warn("reliable UART <- oversized frame")`, drop whole message.
- On complete frame:
```
previous = (rx_sequence == 1) ? 0xFFFFFFFF : rx_sequence - 1
if sequence == previous:      return                       # duplicate of last accepted frame: silent drop
if sequence != rx_sequence:
    warn(f"reliable UART sequence gap: expected {rx_sequence}, got {sequence}")
    return                                                  # drop; rx_sequence NOT advanced
rx_sequence = next_sequence(rx_sequence)
data_handler(None, bytearray(payload))                     # -> on_notify (§6.4)
```
Consequence: after one lost frame every later frame is reported as a gap until a frame with exactly `rx_sequence` arrives.

### 4.4 Write path (`write`, one sequence number per logical frame)
```
async write(data, sensitive=false):
  payload_size = self.max_payload                       # <= 232
  for payload_offset in 0..data.len() step payload_size:
      payload = data[payload_offset .. +payload_size]
      sequence = tx_sequence                            # SAME sequence for all ATT chunks of this frame
      frame = pack("<2sBBIHH", "LR", 1, 0, sequence, len(payload), 0) + payload
      att_size = max(20, min(cfg.write_size, 244))
      for offset in 0..frame.len() step att_size:
          chunk = frame[offset .. +att_size]
          if debug_io: stderr(sensitive ? f"UART TX #{sequence} <redacted {len(chunk)} bytes>"
                                       : f"UART TX #{sequence} {python_repr(chunk)}")
          write_gatt_char(RELIABLE_UART_RX_UUID, chunk, response = true)   # ALWAYS with-response
      tx_sequence = next_sequence(sequence)              # advanced only after the whole frame is written
```
- **Always write-with-response**; `--write-response` is ignored here.
- **No write delay** is applied here; `--write-delay-ms` is ignored here.
- No retry/ACK logic locally: the "ACK" is the peer's next sequence state.

---

## 5. NUS fallback write path + `configure_ble_write_size`

### 5.1 `ble_write(client, data, cfg, sensitive=false, reliable=Some/None)`
```
if reliable.is_some():
    reliable.write(data, sensitive); return                # ALWAYS taken in this CLI (reliable cap required)
if cfg.write_size <= 0: cfg.write_size = 20                # mutates cfg in place
for offset in 0..data.len() step cfg.write_size:
    chunk = data[offset .. +cfg.write_size]
    if debug_io: stderr(sensitive ? f"TX <redacted {len(chunk)} bytes>" : f"TX {python_repr(chunk)}")
    write_gatt_char(NUS_RX_UUID, chunk, response = cfg.write_response)
    if cfg.write_delay > 0: sleep(cfg.write_delay)         # after EVERY chunk incl. the last
```
All three call sites (`send_payload`, `send_geometry`, `loopback_test`) pass `reliable=Some(...)`, so **`--write-response` and `--write-delay-ms` are dead options in practice**; a faithful port should still implement the branch.

### 5.2 `configure_ble_write_size(client, cfg, requested)` (called once, before any write)
```
if requested > 0:                                    # --ble-write-size (validated 0..=244 by argparse)
    cfg.write_size = min(requested, 244)             # NOTE: small values like 5 are NOT raised to 20 here
    info(f"BLE write chunk size: {cfg.write_size} bytes (manual)")
    return
# auto:
reported = 0
try:
    reported = int(characteristic(NUS_RX_UUID).max_write_without_response_size or 0)
except Exception: reported = 0
try:    mtu = int(getattr(client, "mtu_size", 0) or 0)
except (TypeError, ValueError): mtu = 0
size = reported != 0 ? reported : (mtu > 3 ? mtu - 3 : 0)
cfg.write_size = max(20, min(size != 0 ? size : 20, 244))       # -> always 20..=244
info(f"BLE write chunk size: {cfg.write_size} bytes")
```
`cfg.write_size` (as clamped) is what §2.3 (`max(20, write_size)`) and §4.4 (`max(20, min(write_size,244))`) consume, so a manual value of e.g. 5 becomes an effective ATT chunk of 20 on both real paths.

---

## 6. Terminal session

### 6.1 `TerminalConfig`
```
{ escape: [u8;1], enter: "raw"|"cr"|"lf"|"crlf", local_echo: bool, line_mode: bool,
  debug_io: bool, write_size: usize (starts 0), write_response: bool, write_delay: f64 seconds }
```
`write_delay = --write-delay-ms / 1000.0` (default 5.0 ms → 0.005 s).

### 6.2 Enter translation (`translate_enter`)
```
raw : return data unchanged
else:
  data = data.replace(b"\r\n", b"\n")
  data = data.replace(b"\r",     b"\n")
  repl = {"cr": b"\r", "lf": b"\n", "crlf": b"\r\n"}[mode]
  return data.replace(b"\n", repl)
```
(`lf` mode therefore normalizes CRLF/CR → LF.)

### 6.3 `terminal_loop` structure
```
done = Event; queue: Option<bytes> channel
fd = sys.stdin.fileno()        # (run() pre-checks; see §8.6)

STDIN THREAD (daemon):
  loop while !done:
      data = line_mode ? stdin.read_until_newline() : os.read(fd, 1024)
      on OSError -> queue.push(None); return
      if data.is_empty() (EOF) -> queue.push(None); return
      queue.push(data)

SENDER (async task):
  loop while !done:
      data = queue.pop()
      if data is None: sleep(0.2); done.set(); break
      pos = data.find(escape_bytes)               # search WITHIN THIS CHUNK ONLY
      if pos >= 0:
          if pos != 0: send_payload(data[0..pos])
          done.set(); break                       # escape at index 0 exits without sending anything
      send_payload(data)

send_payload(data):
  data = translate_enter(data, cfg.enter)
  if local_echo: stdout.write(data); stdout.flush()
  if geometry: geometry.mark_busy()
  ble_write(client, data, cfg, reliable=reliable)

SIGWINCH (only if geometry.is_some() && tty_module_available && signal::SIGWINCH exists):
  loop.add_signal_handler(SIGWINCH, on_resize)   # wrapped in suppress(NotImplementedError, ValueError, OSError)
  on_resize() once immediately
on_resize: size = os.get_terminal_size(fd) (ignore OSError); geometry.set_size(cols, rows)

info(f"terminal open. press {describe_escape(escape)} to exit.")
context = (line_mode ? null-context : RawTerminal(fd))    # entering may raise (§10)
with context:
    spawn stdin thread (daemon)
    task = spawn sender
    try: await done.wait()
    finally: cancel task; await it swallowing CancelledError
finally: remove SIGWINCH handler if installed
info("terminal closed.")
```
Escape-detection caveat to replicate: the escape byte must lie **entirely inside one read chunk** (1024 bytes / one line) to be recognized; a chunk boundary splitting it is not handled.

### 6.4 `on_notify` (reliable UART data sink; registered as `data_handler`)
Order of side effects per received payload:
1. `if notify_queue is Some: queue.put_nowait(payload)` (only during `--loopback-test`),
2. `if debug_io: stderr("RX " + python_repr(payload))`,
3. `if log_file: log_file.write(payload)` (raw bytes),
4. `if geometry: loop.call_soon_threadsafe(handle_geometry, payload)`,
5. `stdout.write(payload); stdout.flush()` (raw bytes).

`notify_queue` is a module-closure variable set to `Some` only for the duration of the loopback test and reset to `None` in a `finally`.

### 6.5 `describe_escape` (for the "terminal open" hint)
```
0x1B          -> "Esc"
1..=26        -> "Ctrl-{A + v - 1}"        # Ctrl-A .. Ctrl-Z
0x1C..=0x1F   -> "Ctrl-" + "\\]^_"[v-0x1C] # Ctrl-\ Ctrl-] Ctrl-^ Ctrl-_
0x20..=0x7E   -> Python repr of the char, e.g. "'a'"   # single quotes included in message
else          -> format!("0x{:02x}", v)
```
Message: `linkr: terminal open. press {name} to exit.`

### 6.6 Loopback test
```
drain notify_queue
info(f"loopback -> {python_repr(payload)}")
ble_write(payload, reliable)
received = []
deadline = now + timeout
while now < deadline:
    remaining = max(0.1, deadline - now)
    try: received += wait(queue.get(), remaining)
    except Timeout: break
    if payload is substring of received:
        stderr(f"loopback PASS <- {python_repr(bytes(received))}"); return true
stderr(f"loopback FAIL <- {python_repr(bytes(received))}"); return false
```
`run()`: on `false` → `raise RuntimeError("loopback test failed")` → exit 1. `notify_queue` reset to `None` in `finally`.

### 6.7 Session orchestration and exit codes
```
terminal_task   = spawn terminal_loop(...)
disconnect_task = spawn disconnected.wait()
await first_completed(terminal_task, disconnect_task)
cancel + await the pending one (swallow CancelledError)
if disconnect_task completed:
    stderr("")                                  # raw mode left cursor mid-line
    error("BLE disconnected during the terminal session")   # -> exit 3
terminal_task.result()                          # re-raises terminal errors -> exit 1
stop_channels(client)                           # best-effort
return 0
```
`stop_channels`: `stop_notify(MGMT_RESPONSE_UUID)` then `stop_notify(RELIABLE_UART_TX_UUID)`, each swallowing **all** exceptions.

`finally` (always): cancel all pending geometry tasks, `gather(..., return_exceptions=True)` swallowing errors, close `log_file` if opened.

### 6.8 Raw terminal (POSIX)
```
available() = termios module present && tty module present
enter():
    if !available: raise RuntimeError("raw terminal mode needs POSIX termios; use --line-mode instead")
    if os.isatty(fd): saved = tcgetattr(fd); tty.setraw(fd)
exit(): if saved.is_some(): tcsetattr(fd, TCSADRAIN, saved)
```
In `--line-mode` a null context is used (no raw mode at all).

### 6.9 Log file
`open(path, "ab", buffering=0)` — binary, unbuffered append — opened **before** connecting (after the two "connecting" info lines), closed in `run()`'s `finally`. Only reliable-UART payloads are written (management responses/events are never logged).

---

## 7. Terminal geometry sync (`TerminalGeometrySync`)

### 7.1 Clamping (`terminal_geometry`, mirrors `web/terminal_geometry.js`)
```
clamp(value, minimum):
    n = float(value)  -> on TypeError/ValueError: return minimum
    if !isfinite(n): return minimum
    return max(minimum, min(1000, int(n)))          # int() truncates toward zero
cols = clamp(cols, 2); rows = clamp(rows, 2)
return { cols, rows, key: f"{cols}x{rows}" }
```

### 7.2 Command format (byte-identical to web client)
```
f"stty rows {rows} cols {cols} >/dev/null 2>&1\r"
```
e.g. `stty rows 40 cols 120 >/dev/null 2>&1\r`

### 7.3 Prompt detection (`looks_like_shell_prompt(text)`)
```
if not re.search(r"[$#] $", text): return False          # must end with '$ ' or '# '
prefix = text[:-2].strip()                                # drop last 2 chars, strip whitespace
if prefix == "": return True
return any of:
  re.search (endswith-match) r"@\S+(?::\S*)?$"            # e.g. "user@host", "user@host:..."
  re.match  r"^(?:ba|da|a|z)?sh(?:-[\d.]+)?$"             # sh, bash, dash, ash, zsh, sh-5.1
  re.match  r"^(?:~|/\S*)$"                               # ~ or /path
  re.match  r"^\[[^\]]+\]$"                               # [anything]
```

### 7.4 State machine
State: `geometry`, `synced: ""`, `in_flight: ""`, `prompt_visible: false`, `line: ""`.
```
set_size(cols, rows): geometry = terminal_geometry(cols, rows)
mark_busy():  prompt_visible = false; line = ""            # called on EVERY local send_payload
observe(text):                                                   # called on every RX payload (UTF-8 lossy)
    for ch in text:
        if ch in "\r\n": line = ""
        else: line = (line + ch) last 1024 chars
    prompt_visible = looks_like_shell_prompt(line)
take_pending_command() -> Option<String>:
    key = geometry.key
    if !prompt_visible or key in (synced, in_flight): return None
    in_flight = key; prompt_visible = false
    return terminal_geometry_command(cols, rows)
confirm_sent(): synced = in_flight; in_flight = ""
abort_sent():   in_flight = ""
```
`handle_geometry` runs on the event loop (scheduled thread-safely from the BLE callback): `observe(decode(payload, "utf-8", "replace"))`; if a command is due, spawn fire-and-forget `send_geometry` task (tracked in a `geometry_tasks` set with a done-callback that removes it).
```
send_geometry(cmd):
    try  : ble_write(cmd.encode(), reliable) ; geometry.confirm_sent()
    except e: geometry.abort_sent(); warn(f"terminal size sync failed: {e}")
```
Retry rules: failed send → `in_flight` cleared, `synced` unchanged → re-armed on the next observed prompt; size change → new `key` differs from `synced` → re-armed; issuing a send clears `prompt_visible` so a second send cannot fire until a new prompt is observed.

Geometry is enabled iff: `!--no-terminal && stdin fd available && os.isatty(fd) && os.get_terminal_size(fd)` succeeds (initial size captured **before** connecting).

---

## 8. Scanning, matching, connection setup

### 8.1 `--scan` listing (`scan_devices(timeout)`)
```
devices = BleakScanner.discover(timeout)
sort key = ((dev.name or "\u{ffff}").lower(), dev.address or "")      # ascending; unnamed sorts last
for dev in sorted: rssi = getattr(dev,"rssi",None)
    signal = f"{rssi} dBm" if isinstance(rssi, int) else "-"
    println_stdout(f"{dev.address}\t{dev.name or '(unknown)'}\t{signal}")
return devices (UNSORTED original list)
```

### 8.2 `--scan` exit rule
```
if args.scan:
    scanned = scan_devices(timeout)
    if !args.address && !has_control_action(args): return 0
has_control_action = any of: --query-info, --query-uart, --uart, --wifi, --wifi-scan,
    --wifi-off, --query-wifi, --webdav, --webdav-off, --query-webdav,
    --loopback-test is not None, --pair
    # NOT counted: --no-terminal, --scan, --name, --json, etc.
```
So `--scan` alone (or `--scan --no-terminal`) prints the table and exits 0 without connecting; `--scan --address X` proceeds to connect.

### 8.3 `normalize_name_prefix(name)`
```
name = name.strip()
if name.ends_with('*'): return name[..-1].rstrip()      # removes exactly one '*', then trailing spaces
return name
```

### 8.4 `match_device(devices, name)` — returns address or None
```
1) EXACT PASS: for dev in devices (in given order): if dev.name == name: return dev.address
   # exact compare uses the RAW --name (incl. a trailing '*'); None names never match
2) PREFIX PASS: prefix = normalize_name_prefix(name)
   prefixed = [dev for dev in devices if dev.name is not None and dev.name.startswith(prefix)]
   if empty: return None
   if len(prefixed) > 1:
       matches = ", ".join(f"{dev.name} ({dev.address})" for dev in prefixed[:4])   # first 4 only
       warn(f"multiple devices match {prefix + '*'}; using "
            f"{prefixed[0].name} ({prefixed[0].address}); matches: {matches}")
   return prefixed[0].address        # "best" = first in the LIST order, NOT re-sorted
```
When reusing `--scan` results the list is the **unsorted** discovery order; the sorted order is used only for printing.

### 8.5 `find_device`
```
if address given: return address                    # no scan, no match
prefix = normalize_name_prefix(name)
if devices is None:
    info(f"scanning for BLE device matching {prefix + '*'}")
    devices = discover(timeout)
m = match_device(devices, name)
if m is None: raise RuntimeError(f"device not found matching: {prefix + '*'}")   # exit 1
return m
```

### 8.6 Connect sequence (after find_device)
1. Build `cfg` (§6.1).
2. `disconnected` event; disconnect callback → `loop.call_soon_threadsafe(disconnected.set)`.
3. Geometry init (§7).
4. `log_file` opened (before connect).
5. `BleakClient(target, disconnected_callback, timeout=args.timeout)`; `info("connecting...")`, `info("new host: hold Bee GPIO1 to GND before pairing. Bonded hosts reconnect without GPIO1.")` printed before opening the log file; then `info(f"connected: {client.address}")`.
6. `--pair`: on `sys.platform == "darwin"` → `info("macOS requests pairing when the encrypted service is read; accept the system dialog.")`, **no** `pair()` call; otherwise `await client.pair()`.
7. `configure_ble_write_size` → handshake (§3.1) → commands (§3.2) → loopback → terminal or `--no-terminal` (which returns 0 after `stop_channels`).

Order of stderr chatter during startup (quiet off): `connecting...` → GPIO hint → `connected: {addr}` → `BLE write chunk size: ...` → `management API v{maj}.{min}, device ID {hex}` → `control -> ...` per command → `terminal open. ...`.

---

## 9. Argument parsing and validation

### 9.1 Validators (exact messages; argparse prefixes them with `argument {flag}: ` and exits **2**)
```
positive_float:   f = float(s); if !isfinite(f) or f <= 0: Err("value must be greater than zero")
nonnegative_float:if !isfinite(f) or f <  0:            Err("value must not be negative")
ble_write_size:   i = int(s);  if !(0 <= i <= 244):     Err("BLE write size must be between 0 and 244")
                  (a non-integer string raises ValueError -> argparse's generic "invalid ... value")
```

`parse_escape(s) -> [u8;1]`:
```
if s == "^":                       Err("escape must be one byte, like ^] or 0x1d")
if s.len() == 2 and s[0] == '^':   return [ (s[1].to_ascii_uppercase() as u8) & 0x1F ]
        # ^] -> 0x1D, ^\ -> 0x1C, ^^ -> 0x1E, ^_ -> 0x1F; note MASKING, not mapping:
        # ^? -> 0x1F, ^2 -> 0x12, ^a/^A -> 0x01
if s.starts_with("0x"):            # lowercase prefix only; "0X..." falls through to byte form
        n = int(s, 16) or Err("escape must be one byte, like ^] or 0x1d")
        if !(0 <= n <= 0xFF):      Err("hex escape must be between 0x00 and 0xff")
        return [n]
raw = s.encode(utf-8); if raw.len() != 1: Err("escape must be one byte, like ^] or 0x1d")
return raw
```
Default: `parse_escape("^]")` → `[0x1D]`, computed at parser construction (non-string default → not re-converted).

`normalize_uart_spec(s) -> canonical String`:
```
fields = s.split(',').map(strip_whitespace)
if fields.len() != 5: Err("UART spec must be baud,data,parity,stop,flow, like 115200,8,n,1,n")
(baud, data_bits, parity, stop_bits, flow) = fields
baud = int(baud, 10) or Err(f"invalid UART baud rate: {baud!r}")        # Python repr quotes
if !(300 <= baud <= 3_000_000): Err("UART baud rate must be between 300 and 3000000")
if data_bits not in {"5","6","7","8"}: Err("UART data bits must be one of 5, 6, 7, 8")
if stop_bits not in {"1","2"}:         Err("UART stop bits must be 1 or 2")
if parity.lower()  not in UART_PARITY:       Err("UART parity must be none, odd or even (n/o/e)")
if flow.lower()    not in UART_FLOW_CONTROL: Err("UART flow control must be none or rtscts")
return f"{baud},{data_bits},{UART_PARITY[parity.lower()]},{stop_bits},{UART_FLOW_CONTROL[flow.lower()]}"
# data_bits/stop_bits are echoed verbatim (membership is exact-string, so no "08" oddity);
# parity canonicalizes none/odd/even -> n/o/e; flow canonicalizes none/off -> n, hw/none -> rtscts... 
# precisely: n,none,off -> "n"; rtscts,hw -> "rtscts"
```
Canonical example: ` 115200 , 8 , EVEN , 2 , HW ` → `115200,8,e,2,rtscts`.

### 9.2 WiFi credential resolution (`resolve_wifi_credentials(spec, key_file, environ, prompt)`)
Order (first hit wins):
```
ssid, sep, inline = spec.partition(',')
ssid = ssid.strip()
if ssid == "": raise ValueError("WiFi SSID must not be empty")
if sep != "": return (ssid, inline)                 # inline password may be EMPTY (comma present => accepted)
if key_file:
    text = read file as UTF-8 or Err(f"cannot read --wifi-key-file: {oserror}")
    lines = text.splitlines()
    if lines empty: Err(f"--wifi-key-file {key_file} is empty")     # "" only; "\n" yields [""] -> password ""
    return (ssid, lines[0])
env = os.environ.get("LINKR_WIFI_PASSWORD")
if env (non-empty): return (ssid, env)
if prompt is Some:                                     # prompt = getpass.getpass if stdin.isatty() else None
    pw = prompt(f"WiFi password for {ssid}: ")         # hidden input, no echo
    if pw == "": raise ValueError("WiFi password must not be empty")
    return (ssid, pw)
raise ValueError("no WiFi password available: pass --wifi ssid,pass, or set --wifi-key-file, or export LINKR_WIFI_PASSWORD")
```
Any `ValueError` here → `error(msg)` + **exit 2** (`EXIT_USAGE`). Resolution happens **before** bleak is imported and before any radio work.
`--wifi-key-file` without `--wifi` is silently ignored.

### 9.3 Flag inventory (defaults in parentheses; all flags are single-purpose booleans unless noted)
```
--version                       (action: print "{prog} 1.0.0", exit 0)
--name STR                      ("Linkr BLE UART")   help: "BLE device name or prefix; default matches Linkr BLE UART*"
--address STR                   (None)               help: "BLE address/UUID; skips name scan"
--scan                          false                help: "list nearby BLE devices (all of them, named or not)"
--timeout F:positive_float      (8.0)                help: "scan timeout seconds"   (also used as BleakClient timeout)
--query-info                    false                help: "send @i? device diagnostics before terminal"
--query-uart                    false                help: "send @u? before terminal"
--uart SPEC:type=normalize_uart_spec (None)          help: "set UART as baud,data,parity,stop,flow (baud 300-3000000, data 5-8, parity n/o/e, stop 1/2, flow n/rtscts)"
--wifi SSID[,PASSWORD]          (None)               help: "connect ESP32 to WiFi; prefer ssid alone with --wifi-key-file so the password stays out of argv"
--wifi-key-file PATH            (None)               help: "read the WiFi password from the first line of PATH"
--wifi-off                      false                help: "forget saved WiFi"
--query-wifi                    false                help: "send @w? before terminal"
--wifi-scan                     false                help: "scan nearby 2.4 GHz WiFi networks"
--webdav URL                    (None, plain str)    help: "set anonymous HTTP WebDAV upload URL"
--webdav-off                    false                help: "disable WebDAV upload"
--query-webdav                  false                help: "send @d? before terminal"
--pair                          false                help: "request OS bonding (hold Bee GPIO1 low); macOS pairs on encrypted reads"
--loopback-test [STR]           (None; const "A" when flag present w/o value)
                                help: "send payload and require the same bytes back"
--loopback-timeout F:positive   (3.0)                help: "seconds to wait for --loopback-test echo"
--no-terminal                   false                help: "connect, run commands, exit"
--json                          false                help: "write management responses/events to stdout as JSON lines (sensitive command text stays redacted)"
--quiet                         false                help: "suppress progress messages; errors and results still print"
--print-completion {bash,fish,zsh} (None)            help: "print a shell completion script and exit"
--ble-write-size int:0..244     (0 = auto)            help: "max bytes per BLE RX write; default auto"
--write-response                false                help: "use GATT write-with-response"
--write-delay-ms F:nonneg       (5.0)                help: "delay between BLE write chunks"
--enter {raw,cr,lf,crlf}        ("raw")              help: "translate Enter key bytes before BLE write"
--local-echo                    false                help: "echo typed bytes locally"
--line-mode                     false                help: "do not use raw terminal; send one visible line at a time"
--debug-io                      false                help: "print BLE TX/RX byte traces to stderr"
--log-file PATH                 (None)               help: "append raw BLE RX bytes to a file"
--escape TYPE:parse_escape      (0x1D)               help: "terminal escape byte, default ^]"
```

### 9.4 Combos that fail (and where)
- Any argparse type/choices violation → argparse usage error, exit **2**.
- `--wifi` with empty/whitespace SSID, unreadable/empty key file, empty prompted password, or **no password source at all** (non-TTY stdin and no comma, no key file, no env) → exit **2**. Note: `--wifi` with a comma and empty password is accepted.
- Missing `bleak` package → `RuntimeError("the 'bleak' package is required to talk to the device; install it with 'python3 -m pip install bleak'")` → exit **1** (imported *after* WiFi resolution; `--help`/`--version`/`--print-completion` work without it).
- Capability mismatches (§3.1) → exit **1**.
- Device not found / bad UART-style management reply / loopback fail / 5 s management timeout → exit **1**.
- Raw terminal without termios (non-`--line-mode` on Windows) → exit **1**.
- No hard mutual exclusions exist; conflicting flags are simply executed in the fixed order (§3.2). E.g. `--wifi ... --wifi-off` sends `@w=...` then `@w off`; `--no-terminal --scan` still exits early after the scan unless a control action/`--address` is present.

### 9.5 `main()` control flow
```
parser = build_parser()
args = parser.parse_args()          # outside try: argparse SystemExit(2) propagates untouched
QUIET = args.quiet
if args.print_completion: print(completion_script(...)); return 0   # BEFORE any radio/bleak use
try:    return run(args)
except KeyboardInterrupt: return 130
except Exception as exc:  error(str(exc)); return 1
```

---

## 10. Windows / termios notes

```python
try:
    import termios; import tty
except ModuleNotFoundError:
    termios = None; tty = None
```
Consequences without termios (must be mirrored by cfg/feature gating in Rust):

1. **`RawTerminal.available()` = false.** Any terminal session that is *not* `--line-mode` raises on context entry: `raw terminal mode needs POSIX termios; use --line-mode instead` → `linkr: error: ...` → exit **1**. So on Windows the terminal requires `--line-mode`. Scanning, all control actions, `--no-terminal`, `--scan`, `--json` still work.
2. **No SIGWINCH path.** The condition is `geometry.is_some() && tty_module_available && SIGWINCH exists && add_signal_handler succeeds (suppress NotImplementedError/ValueError/OSError)`. Without it: no resize handler is installed *and the initial `on_resize()` call is skipped*. Geometry is still initialized pre-connect from `os.get_terminal_size(fd)` (available on Windows), so the **initial** size is still pushed when the first prompt is seen; subsequent resizes are never observed.
3. `loop.remove_signal_handler` only if installed (no-op on Windows).
4. Everything else (stdin thread with `os.read`/`readline`, getpass, log file, JSON) is platform-neutral.
5. macOS-specific branch: `sys.platform == "darwin"` changes `--pair` behavior (info message instead of `client.pair()`).

---

## 11. Appendix — `--print-completion` (deterministic; must run without bleak)

Constants: `COMPLETION_SHELLS = ("bash","fish","zsh")`; `COMPLETION_COMMANDS = ("linkr_ble_terminal.py","linkr-ble-terminal")`; `COMPLETION_FILE_OPTIONS = ("--log-file","--wifi-key-file")`.

Entry per option string (in `--help` order): `kind` = `"flag"` if option takes no value, else `"file"` if flag ∈ FILE_OPTIONS, else `"choices"` if the action has `choices`, else `"value"`; `values` = stringified choices; `help` = `completion_label(action.help)`; `label` = `completion_label(action.metavar or action.dest) or "value"`.

`completion_label`: collapse all whitespace runs to single spaces, then replace each char of `[](){}"\`:,*_` (i.e. `[ ] ( ) { } " \` : , * _`) with a space, then collapse whitespace again. Apostrophes survive.

Output: `print(script)` where `script` already ends with `"\n"`, so stdout ends with `"\n\n"`. Exit 0.

- **bash**: header comment block (`# bash completion for the Linkr BLE host CLI.` / `# Generated by --print-completion; do not edit by hand.` / `#` / `#   source <(linkr_ble_terminal.py --print-completion bash)` / `# or install it once:` / `#   linkr_ble_terminal.py --print-completion bash > \` / `#       ~/.local/share/bash-completion/completions/linkr_ble_terminal.py`), then `_linkr_ble_terminal() {`, `local cur prev`, `cur="${COMP_WORDS[COMP_CWORD]}"`, `prev="${COMP_WORDS[COMP_CWORD-1]}"`, blank, `case "$prev" in`, one `        {flag}) COMPREPLY=( $(compgen -W "{words}" -- "$cur") ); return ;;` per choices flag, one `        {f1|f2}) COMPREPLY=( $(compgen -f -- "$cur") ); return ;;` line for file flags, `esac`, blank; if any value-taking entries: `case "$cur" in`, per choices flag `        {flag}=*) COMPREPLY=( $(compgen -W "{words}" -P "${cur%%{*}}=" -- "${cur#*=}") ); return ;;` (literal `${{cur%%=*}}` / `${{cur#*=}}` in generated text), analogous file line, `esac`, blank; then `if [[ "$cur" == -* ]]; then`, `COMPREPLY=( $(compgen -W "{all flags}" -- "$cur") )`, `fi`, `return 0`, `}`, `complete -F _linkr_ble_terminal linkr_ble_terminal.py linkr-ble-terminal`.
- **zsh**: `#compdef linkr_ble_terminal.py linkr-ble-terminal`, same 3 comment lines, `_linkr_ble_terminal() {`, `_arguments -s -S \`, one single-quoted spec per entry joined by ` \\\n        `: choices → `{flag}=[{help}]:{label}:({v1} {v2})`; file → `{flag}=[{help}]:file:_files`; value → `{flag}=[{help}]:{label}:`; flag → `{flag}[{help}]`; apostrophes inside quoted parts escaped as `'\''`; closing `}`, blank, then the `if [ "$funcstack[1]" = "_linkr_ble_terminal" ] ... else compdef ... fi` block.
- **fish**: 4-line header comment (`# fish completion for the Linkr BLE host CLI.` etc. with both command names), then per command × per entry a line starting `complete -c {cmd}` + `--long {name}` for `--*` / `-s {ch}` for short, + ` -x -a '{values}'` (choices) / ` -r -F` (file) / ` -r -f` (value; flags get neither), + ` -d '{help}'` where help escapes `\` → `\\` and `'` → `\'`; final newline.

---

## 12. Appendix — complete message catalog (for golden-output tests)

```
info:  "connecting..."
info:  "new host: hold Bee GPIO1 to GND before pairing. Bonded hosts reconnect without GPIO1."
info:  "connected: {address}"
info:  "BLE write chunk size: {n} bytes" | "BLE write chunk size: {n} bytes (manual)"
info:  "management API v{maj}.{min}, device ID {hex16}"
info:  "scanning for BLE device matching {prefix}*"
info:  "control -> {display}"
info:  "loopback -> {bytes!r}"
info:  "terminal open. press {escape_name} to exit."
info:  "terminal closed."
info:  "macOS requests pairing when the encrypted service is read; accept the system dialog."
info:  "reliable UART <- orphaned fragment"
warn:  "management <- orphaned response fragment"
warn:  "management <- invalid response header"
warn:  "management <- oversized response"
warn:  "reliable UART <- invalid frame header"
warn:  "reliable UART <- oversized frame"
warn:  "reliable UART sequence gap: expected {n}, got {m}"
warn:  "multiple devices match {prefix}*; using {name} ({addr}); matches: {n1} ({a1}), ..."
warn:  "terminal size sync failed: {exc}"
error: "BLE disconnected during the terminal session"     (preceded by an empty stderr line)
error: "linkr: error: " (empty str of TimeoutError)
raw stderr: "response #{id} <- {line}" / "event #{id} <- {line}"
raw stderr: "MGMT TX #{id} {bytes!r}" / "MGMT TX #{id} <redacted {n} bytes>"
raw stderr: "UART TX #{seq} {bytes!r}" / "UART TX #{seq} <redacted {n} bytes>"
raw stderr: "TX {bytes!r}" / "TX <redacted {n} bytes>" / "RX {bytes!r}"
raw stderr: "loopback PASS <- {bytes!r}" / "loopback FAIL <- {bytes!r}"
RuntimeError texts (all -> exit 1): "device not found matching: {prefix}*", "loopback test failed",
  "unsupported Linkr Management API version", "invalid Linkr Management payload limit",
  "device does not advertise Device ID support", "device does not advertise Reliable UART support",
  "device does not advertise WiFi support", "device does not advertise async event support",
  "device does not advertise WebDAV support", "invalid Linkr Device ID length",
  "unsupported Reliable UART version", "invalid Reliable UART state",
  "management command is outside the advertised limit" (ValueError),
  "raw terminal mode needs POSIX termios; use --line-mode instead",
  "the 'bleak' package is required to talk to the device; install it with 'python3 -m pip install bleak'"
```
