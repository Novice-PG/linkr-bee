# Vendored macaron cursors

Three PNGs taken from the deepin macaron cursor theme and used as the light
theme's mouse pointers. The light theme is nearly all white surfaces -- the
terminal body is `#f8fafc` and the header is a white gradient -- where a white
pointer (the Windows default, and every white cursor scheme) all but
disappears. These are teal with a dark outline and a pale halo, so they stay
readable whatever the system cursor scheme happens to be. The dark theme keeps
the system pointer, which was never a problem there.

| shape | purpose | file | hotspot |
| --- | --- | --- | --- |
| `left_ptr` | arrow: window, terminal viewport, panels | `left_ptr-32.png` | `3 2` |
| `xterm` | I-beam over the terminal text | `xterm-32.png` | `15 15` |
| `hand2` | hand on buttons and other controls | `hand2-32.png` | `12 2` |

`web/style.css` picks them up with plain `cursor: url(...)` rules, and
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

- Repository: [linuxdeepin/deepin-desktop-theme](https://github.com/linuxdeepin/deepin-desktop-theme)
- Path: `macaron/icons/macaron/cursors/cursors/`
- Revision: `a0c79e31467ab602d96605475dca69eb835c5d90` (2026-06-24)

Upstream ships these as XCursor (X11) bundles only: one file per shape holding
every size as raw ARGB pixels, with no PNG or vector source anywhere in the
theme. Two of the shapes are git symlinks there -- `xterm` to `text` and `hand2`
to `pointing_hand` -- so the aliases are what this directory is named after.

## Regenerating

```sh
node tools/extract_macaron_cursors.mjs
```

That fetches the revision above, extracts the three shapes at the size the
script pins, and rewrites the PNGs together with `cursors.json`. Run it to move
to a newer theme revision or to change the size. Do not edit the PNGs by hand.

## Licence and attribution

The artwork is **CC-BY-4.0**, Copyright UnionTech Software Technology Co., Ltd.
The licence text sits next to this file as `LICENSE-CC-BY-4.0.txt`.

What was changed: a subset of the theme was taken (three shapes, two sizes each)
and converted from XCursor to PNG. The artwork itself is untouched.

CC-BY-4.0 requires attribution and an indication of changes, which is what this
file and the licence text beside it provide. Keep both next to the images.
