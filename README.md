<p align="center">
  <img src="assets/linkr-bee-logo.svg" alt="Linkr Bee" width="520">
</p>

<p align="center">
  A wireless serial console for your board, over Bluetooth LE or your local network.
</p>

<p align="center">
  <a href="README.zh-CN.md">中文</a> ·
  <a href="docs/README.md">Documentation</a> ·
  <a href="docs/DEVELOPMENT.md">Development guide</a>
</p>

Linkr Bee is a small ESP32 accessory that puts a board's UART console on a browser,
a phone, or a command line, over Bluetooth LE or your local network.

UART is usually the last interface still working: board bring-up, bootloader work,
a network that will not come up, recovery. Linkr Bee keeps that console reachable
without a USB serial cable and a computer parked next to the target. It carries the
target's own UART bytes and does not replace the target shell.

## What it does

- Terminal in the browser, in the phone apps, or on the command line, with ANSI and
  256-color output.
- Change baud rate, data bits, parity, stop bits, and flow control while connected.
  Default is `115200,8,n,1,n`.
- Send Enter as raw, CR, LF, or CRLF for bootloaders and different shells.
- Save logs, upload them to WebDAV, and check firmware, UART buffer, WiFi, and
  transfer counters from the diagnostics panel.
- Join a 2.4 GHz network and keep using the console over LAN, without Bluetooth.
- Assistant in the browser and the phone apps: searches the serial log and can run
  commands on the target once you approve them.

## What you need

```text
Browser / desktop / mobile / Linkr
                 │
          Bluetooth LE or WiFi
                 │
        Linkr Bee accessory (ESP32-C3 / ESP32-C5)
                 │
             3.3 V UART
                 │
        SBC or embedded target
```

- A Linkr Bee accessory: ESP32-C3 Super Mini, ESP32-C3 DevKitM, ESP32-C3 DevKitC,
  or ESP32-C5 DevKitC.
- A target with a UART console, and three wires: target TX to accessory RX, target RX
  to accessory TX, and a shared ground.
- A host: Chrome or Chromium for the web terminal, one of the phone apps, or the
  Python / C terminal on macOS and Linux.

The UART side is 3.3 V logic, not RS-232. See the
[hardware requirements](docs/HARDWARE.md) before wiring a custom board.

## Getting started

1. Flash the accessory. Download the image for your board (names are listed in the
   [development guide](docs/DEVELOPMENT.md#github-actions-build)) and flash it from
   macOS or Linux:

   ```sh
   python3 -m pip install esptool
   ./flash_firmware.sh
   ```

   The script finds the image and the serial port, verifies `SHA256SUMS` when it is
   present, and keeps the saved settings. Building from source is covered in the
   [development guide](docs/DEVELOPMENT.md#build).

2. Wire it to the target, crossing TX and RX and connecting ground. On an ESP32-C3
   Super Mini the bridge uses GPIO20 (RX) and GPIO21 (TX); other boards are in the
   [hardware requirements](docs/HARDWARE.md#3-引脚分配).

3. Power both sides. The blue LED flashes while bytes cross the bridge.

4. Authorize this host: hold **GPIO1 to GND**, connect, and accept the pairing
   prompt. Release GPIO1 afterwards; the host is remembered.

5. Open a client. For the web terminal:

   ```sh
   tools/serve_web.sh        # then open http://127.0.0.1:8765/
   ```

6. Match the UART format to the target, then reset it and watch the console.

## Using the terminal

![Linkr Bee desktop terminal](assets/screenshots/linkr-bee-terminal-desktop.jpg)

- Top bar: connection state, connect/reconnect, panel toggle, theme, language.
- Terminal: click and type; font size, fullscreen, auto-scroll, copy, and clear are
  in its toolbar.
- Control panel: transport, UART settings, Enter mode, local echo, chunk size,
  diagnostics, WiFi, WebDAV, and log actions.

UART settings apply immediately, so a target that boots at an unusual baud rate
needs no reflash. The terminal also keeps the target's window size in sync after
login and on layout changes, which full-screen programs need. If output arrives but
typing does nothing, check that the target RX goes to the accessory TX, and turn
hardware flow control off unless CTS and RTS are both wired.

<p align="center">
  <img src="assets/screenshots/linkr-bee-terminal-mobile.jpg" alt="Linkr Bee terminal on a phone" width="360">
</p>

On phones the terminal takes the available space and secondary settings move into a
drawer. The assistant is available in the browser and in the phone apps. A browser
page calls the model endpoint directly, so the endpoint has to accept requests from
the page: local model servers do, most hosted providers need a proxy. See the
[serial assistant notes](mobile/README.md#built-in-serial-assistant-pi) for setup.

## LAN mode

After provisioning the accessory onto a 2.4 GHz network over Bluetooth LE, the same
terminal can reach it through its WebSocket bridge. Bluetooth LE stays the
configuration path: UART settings, WiFi, WebDAV, and diagnostics are managed over
it, while LAN mode carries the terminal data.

The bridge needs a 128-bit access token, generated on first boot and readable only
over the encrypted Bluetooth LE link. The web terminal picks it up automatically
while Bluetooth is connected; the Python and C terminals print it. The token gates
access but does not encrypt anything, so `ws://` traffic is visible on the same
network. Use LAN mode on a network you trust.

## Pairing and security

- The Bluetooth LE link is encrypted and bonded. Each new host is authorized once by
  holding **GPIO1 to GND** during pairing.
- Eight hosts are remembered across reboots and are not evicted automatically, so
  remove an unused host or factory-reset before adding a ninth.
- Pairing uses LE Secure Connections Just Works, which has no man-in-the-middle
  protection. Authorize only where you can see the accessory.
- Factory reset clears every bond and setting. Hold **GPIO0 to GND** on ESP32-C3, or
  **GPIO28** on ESP32-C5, while the board boots, for at least two seconds. On an
  ESP32-C3 Super Mini the BOOT pin is not GPIO0, so check the board wiring first.
- Per-client steps and recovery: [Bluetooth pairing](docs/BLE_PAIRING.md) (Chinese).

## Troubleshooting

| Symptom | Check |
| --- | --- |
| No device in the client | Accessory powered and in range, and not already connected to another host; only one Bluetooth LE connection is served at a time |
| Browser cannot connect | Chrome or Chromium, served over HTTPS or `localhost`. `tools/serve_web.sh` uses `localhost` |
| No output | Target TX to accessory RX, target is printing, baud rate matches |
| Output but no input | Target RX to accessory TX; hardware flow control off unless CTS and RTS are both wired |
| Garbled output | UART format matches the target: baud rate, data bits, parity, stop bits |
| No pairing prompt | GPIO1 grounded before connecting |
| LAN mode fails | Accessory on 2.4 GHz, host on the same network, token matches the one reported over Bluetooth LE |

## Limits

- Devices are matched by the Linkr management service, so any `Linkr BLE UART-*`
  suffix works.
- Web Bluetooth means Chrome or Chromium. Safari and Firefox do not implement it.
- WiFi provisioning is 2.4 GHz only.
- iOS simulators cannot do Bluetooth LE; use a real device.

## Documentation

- [Development guide](docs/DEVELOPMENT.md): build, flash, packaging, Kconfig, protocol
- [Hardware requirements](docs/HARDWARE.md): pinout, electrical, production notes
- [Bluetooth pairing](docs/BLE_PAIRING.md): authorization and recovery (Chinese)
- [BLE accessory API](docs/LINKR_BLE_API.zh-CN.md): services and framing for integrators (Chinese)
- [Android / iOS](mobile/README.md) and [HarmonyOS](harmonyos/README.md) clients
- [Documentation index](docs/README.md) · [中文 README](README.zh-CN.md)

## License

[LICENSE](LICENSE)
