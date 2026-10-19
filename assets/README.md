# Linkr Bee logo assets

| File | Use |
| --- | --- |
| `linkr-bee-logo.svg` | Horizontal lockup on light backgrounds: README, docs, print |
| `linkr-bee-logo-dark.svg` | Same lockup on dark backgrounds |
| `linkr-bee-logo-mono.svg` | Single-colour printing, laser marking, silkscreen |
| `linkr-bee-mark.svg` | Icon-only mark, transparent, for dark surfaces |
| `linkr-bee-mark-light.svg` | Icon-only mark, darker green, for light surfaces |
| `linkr-bee-logo-concept.png` | Legacy concept sheet from the previous mark, kept for reference |
| `screenshots/linkr-bee-terminal-desktop.jpg` | Desktop terminal-first layout |
| `screenshots/linkr-bee-terminal-controls.jpg` | Desktop layout with connection and settings panel |
| `screenshots/linkr-bee-terminal-mobile.jpg` | Compact mobile terminal layout |

Web assets live with the app: `web/favicon.svg` is the same artwork on a
rounded tile, and the topbar inlines the mark so it inherits `currentColor`.

## proposals/

Fourteen files, not referenced by the app: the four initial directions
(`01-hive-node` … `04-comb-trio`, amber) and the three merged variants
(`a-hex-terminal` … `c-hex-negative`, green) that `app-icon.svg` was chosen
from. `a-hex-terminal` is the shipped direction — the production files one
level up are its final tuning, on a tighter view box.

## Brand colors

- Hive green: `#3FC88A` — dark theme accent, `--accent` in `web/skin-v2.css`
- Hive green, light surfaces: `#1F9D64` — light theme accent
- Ink: `#0B1118` (icon tile), `#05080D` (page background)

## The mark

One hexagon: the hive. Inside it sits a terminal prompt and a block cursor, so
the same shape reads as both "a board waiting for a command" and "a cell in a
hive". There is no second outline to collapse at small sizes.

Stroke weights are fixed on the 512 grid: outline 30, prompt 26. Do not rescale
them independently — the outline has to stay visibly heavier than the content,
otherwise the two merge into one thick glyph below 40 px.

## Sizes

- **88 px and up** — full mark, all three parts legible
- **44–88 px** — prompt and cursor still separate
- **32–44 px** — use `linkr-bee-mark.svg`; the cursor reads as a solid block
- **16 px** — only the hexagon survives. That is expected, and it is why the
  favicon uses the version with a filled tile: the silhouette carries the brand
  even when the interior is gone.

## Clear space

Keep at least 25% of the mark's height free on all sides. Do not place the mark
inside another outlined container — the hexagon is already the frame.

## Don't

- Do not rebuild the prompt from three lines. Two lines plus the vertex is the
  mark; a third line turns it into a list glyph.
- Do not use `#3FC88A` on white for anything smaller than 44 px; contrast drops
  below 3:1 and the outline disappears. Use `linkr-bee-mark-light.svg`.
- Do not rotate, skew, or outline the mark with a second stroke.
