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
| `views/diagnostics.rs` | The `@i?` read, end to end: `@info` parsing, the six web-client formatters, the read's state machine, and the grid |

`views/` is where a view lives: a *model* (what the device said, as plain data)
and a *painter* (a pure function from `&App` plus a pane width to lines). The
split is the reason a whole view can be pinned to the documented sample reply
without a socket, a clock or a terminal.

Network, Assistant and Transfer still say so in their body rather than showing
an empty frame. They are the next steps, followed by the command palette, the
help overlay and the dialogs; the transport, protocol, terminal emulation and
agent layers follow behind them.

## Gates — all four must pass before a commit

```sh
cargo fmt --check                                   # style
cargo clippy --all-targets -- -D warnings           # lints, warnings are errors
cargo test                                          # 111 tests, headless
for t in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu \
         x86_64-pc-windows-msvc x86_64-pc-windows-gnu aarch64-pc-windows-msvc \
         x86_64-apple-darwin aarch64-apple-darwin; do
  cargo check --target "$t" --all-targets || exit 1  # 7 targets, cross-platform
done
```

Every target is checked because the crate ships to Linux, macOS and Windows on
both x86-64 and ARM. There are no C dependencies: the only runtime requirement
on Linux is glibc's base libraries.

## Looking at a frame without a terminal

```sh
cargo run -- --print-frame 100x30                     # plain text
cargo run -- --print-frame 100x30 --ansi              # with true-colour escapes
cargo run -- --print-frame 100x30 --ansi --theme light --lang en
cargo run -- --print-frame 100x30 --ansi --transport ble   # transport reported as locked
cargo run -- --print-frame 110x32 --view diagnostics --state connected --sample
```

`--view` opens on a named view, `--state` previews a connection state, and
`--sample` loads the documented `@info` reply, so a content view can be inspected
with real data instead of its empty state.

The frame is rendered through ratatui's test backend, so this works over SSH,
in CI, and with no TTY at all. `dev/frame-to-html.py` turns an `--ansi` dump
into a standalone HTML preview with exact column alignment (wide CJK glyphs span
two columns), which is how the design reference and the implementation are
compared side by side:

```sh
cargo run --quiet -- --print-frame 100x30 --ansi > /tmp/frame.ansi
python3 dev/frame-to-html.py /tmp/frame.ansi preview.html "linkr TUI"
```

The previews are generated on demand and deliberately not committed: they are
large, and everything in them is reproducible from the source.

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

## Layout constants

`ui::BAR_HEIGHT` (1 row) and `ui::SIDEBAR_WIDTH` (30 columns) are the only
numbers the composition knows. The sidebar yields width to the centre column
when a window is too narrow for both, so a small terminal shows the view rather
than a wall of sidebar.
