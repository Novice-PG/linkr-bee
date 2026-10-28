# Working in this repository

## Frontend changes need a visual review

The web terminal is part of the product, not a debug page. Before changing
anything under `web/` (including `index.html`, `*.js`, `*.css`, `skin-v2.css`
and the vendored agent bundle) read `PRODUCT.md` and `DESIGN.md` and follow the
existing design system: tokens, spacing scale, component patterns, one accent
colour, one shadow.

A frontend change is only complete after a visual review:

1. Run the demo surface without hardware:
   `sh tools/serve_web.sh 8090` (one terminal) and open
   `http://127.0.0.1:8090/?demo=board`.
2. Capture the reviewed surface with `cd mobile && node ui-review.mjs`.
   It writes phone/tablet/desktop screenshots in both themes and both
   languages to `build-demo/ui-review/` (gitignored).
3. Inspect them against `DESIGN.md` and the neighbouring panels. Fix clipping,
   misalignment, inconsistent type/colour/spacing, missing hover/focus/empty
   states, and touch targets below 36 px. Check both languages: translated copy
   must not break layout.
4. Include what was reviewed (widths, themes, languages) in the commit message
   or PR description. A frontend diff that was never rendered is not done.

Do not introduce new colours, shadows, radii, or component looks for a single
screen. Promote genuinely reusable values into the design tokens instead, and
update `DESIGN.md` when the system itself changes.

## Capabilities decide the UI

Different boards expose different hardware and firmware features. The UI must
hide a module that the connected board does not support rather than leaving
dead controls on screen: probe lazily, then set the feature flag to false and
hide the card, its section tab and its rail entry. Never show a disabled ghost
of an absent feature. When a whole section has no supported module left, hide
the section and fall back to a visible one. Keep the controls visible while a
board is not connected or the capability is still unknown.
