# Bundled fallback font

`linkr-legacy-computing.woff2` is a subset of **BabelStone Pseudographica
16.0.0** covering the two Unicode blocks that desktop monospace fonts and most
system fallbacks do not ship:

- `U+1CC00–U+1CEBF` Symbols for Legacy Computing Supplement (Unicode 16.0)
- `U+1FB00–U+1FBFF` Symbols for Legacy Computing (Unicode 13.0)

BabelStone Pseudographica is drawn for Unicode text art and covers every
assigned character in these blocks; unassigned characters are omitted.

The terminal stack uses the subset as a last-resort family before `monospace`,
so ANSI art built from these block/octant/sextant glyphs has real shapes instead
of `.notdef` boxes. The source glyphs are designed on a square body, while a
terminal cell is roughly 1:2; the build therefore scales the outlines
horizontally by 0.5 and sets the advance to half an em, so each glyph occupies
exactly one terminal cell without changing its height. The family is renamed
**Linkr Legacy Computing** because the OFL reserves the name "BabelStone" for
modified versions.

- Source: <https://www.babelstone.co.uk/Fonts/Pseudographica.html>
- Version: 16.0.0 (May 30, 2024)
- License: SIL Open Font License 1.1. See `OFL.txt`; the same copyright and
  licence text is embedded in the font's name table.
- Regenerate (fonttools + brotli), starting from the upstream TTF:

```sh
python3 -m fontTools.subset BabelStonePseudographica.ttf \
  --unicodes="U+1CC00-1CEBF,U+1FB00-1FBFF" \
  --output-file=babel-legacy.ttf
# apply the 0.5 horizontal transform and rename, then:
python3 -m fontTools.subset linkr-legacy-computing.ttf \
  --unicodes="U+1CC00-1CEBF,U+1FB00-1FBFF" \
  --flavor=woff2 --output-file=linkr-legacy-computing.woff2
```
