# Linkr Bee brand assets

| File | Use |
| --- | --- |
| `linkr-bee-logo.svg` | Primary horizontal logo for README, web and documents |
| `linkr-bee-icon.svg` | Master app icon: the green terminal mark on a transparent ground |
| `linkr-bee-mark.svg` | The same drawing under the icon-only name: repository avatars, favicons, hardware labels |
| `linkr-bee-logo-mono.svg` | Single-color printing, laser marking and silkscreen reference |
| `linkr-bee-logo-concept.png` | White-background visual concept sheet |
| `screenshots/linkr-bee-terminal-desktop.jpg` | Desktop terminal-first layout |
| `screenshots/linkr-bee-terminal-controls.jpg` | Desktop layout with connection and settings panel |
| `screenshots/linkr-bee-terminal-mobile.jpg` | Compact mobile terminal layout |

## Brand colors

- Deep navy: `#101722` (primary)
- Honey amber: `#F6B73C` (accent)
- Terminal green: `#3DD68C` (the app icon)
- White: `#FFFFFF`

The icon is a honeycomb cell drawn as a rounded hexagon with a `>` prompt chevron
and a cursor bar inside: a terminal prompt in a cell. The master is 200×200 with
a 13-unit hexagon stroke and 15-unit marks, and every raster or platform-shaped
copy is generated from it:

    python3 assets/make-icons.py    # from the repository root; needs Pillow and numpy

That script writes the Windows `.ico` (16–256 px), the iOS app icon and splashes,
the Android launcher tiles, adaptive foregrounds and splashes, the Android vector
foreground, the HarmonyOS app icons and the web favicon. Keep clear space around
the icon equal to at least one stroke width. For PCB silkscreen or laser marking,
use the monochrome logo artwork and omit inner detail below 6 mm total mark
height.
