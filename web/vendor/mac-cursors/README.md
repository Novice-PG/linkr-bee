# Vendored macOS cursors

Five PNGs -- three shapes, and two contrast variants for the two that need
them -- used as the terminal's mouse pointers in both themes. The artwork is
the [macos-cursors](https://www.npmjs.com/package/macos-cursors) package
(macOS-style cursors for the web).

| shape | purpose | variant | file | hotspot |
| --- | --- | --- | --- | --- |
| `left_ptr` | arrow: window, terminal viewport, panels | black (light theme) | `left_ptr-32-black.png` | `7 3` |
| `left_ptr` | arrow | white (dark theme) | `left_ptr-32-white.png` | `7 3` |
| `xterm` | I-beam over the terminal text | black (light theme) | `xterm-32-black.png` | `12 12` |
| `xterm` | I-beam | white (dark theme) | `xterm-32-white.png` | `12 12` |
| `hand2` | hand on buttons and other controls | shared by both themes | `hand2-32.png` | `8 4` |

The two variants are what keeps the pointer readable on both themes: the black
arrow (white outline) disappears against the dark theme's near-black surfaces,
so a white variant -- RGB inverted, alpha kept -- is derived from the same
artwork, and the stylesheet picks the file per `data-theme`. The hand is one
file on purpose: it is a white glove with a dark outline in both directions and
reads on either background; inverting it would hollow it out.

`web/style.css` picks the files up with plain `cursor: url(...)` rules, and
`cursors.json` records the same files and hotspots so a test can hold the
stylesheet to them -- a hotspot typed wrong points the click a few pixels away
from the arrow's tip, and nothing else would notice.

## Why one size, and no image-set

32px is the size MDN recommends for cursors: Chromium and Firefox cap them at
128x128, but an image above 32x32 tends to be refused or downscaled on a
high-DPI screen.

The obvious way to stay sharp on those screens is `image-set()` with a 2x file,
and it does not work here: Chromium parses the unprefixed function inside
`cursor` but never uses it for the pointer, so the declaration stands and the
keyword at the end of it takes over -- the system cursor, with nothing
reporting why. A 2x file was vendored and shipped that way once; it looked, from
the stylesheet, exactly like the rules were not applying at all. `image-set()`
in `cursor` is therefore out of bounds here, and a test keeps it out.

## Source

- Package: `macos-cursors@1.1.0` on the npm registry
- Repository: [aw3r1se/macOS-cursors](https://github.com/aw3r1se/macOS-cursors)

Upstream ships one `.cur` (Windows cursor) file per shape: a 32-bit DIB frame
plus the hotspot. The extractor reads the pinned registry tarball directly --
this project's machines cannot reach GitHub, and the registry serves the same
bytes npm would install.

## Regenerating

```sh
node tools/extract_mac_cursors.mjs
```

That fetches the pinned tarball, converts the three shapes to PNG, derives the
white variants, and rewrites the PNGs together with `cursors.json`. Run it to
move to a newer package version or to change the shapes. Do not edit the PNGs
by hand.

## Licence and attribution

The artwork is **MIT**, Copyright (c) 2026 Bogdan Koch. The licence text sits
next to this file as `LICENSE-MIT.txt`.

What was changed: a subset of the package was taken (three shapes), converted
from `.cur` to PNG, and the arrow and the I-beam were additionally re-coloured
(RGB inverted, alpha kept) to produce the white variants the dark theme uses.
The shapes themselves are untouched.
