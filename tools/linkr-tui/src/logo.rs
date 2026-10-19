//! The project's mark, as cells.
//!
//! `build.rs` rasterises `assets/logo.svg` at compile time and writes the
//! result to `OUT_DIR/logo.rs`; this module includes that, chooses the variant
//! the terminal's own capability calls for, and lays the picture out. Nothing
//! here — or in the shipped binary — knows what an SVG is.
//!
//! One mark, five spellings, because the picture has to arrive everywhere the
//! interface does: half-block cells in 24-bit, 256 or 16 colours, braille dots
//! where there is no colour at all, and a grey ASCII ramp where even those
//! cannot be trusted (a bare console, the old Windows one, a locale that is
//! not UTF-8). [`Variant`] is the whole policy, and `LINKR_LOGO` overrides it
//! by name — `truecolor`, `256`, `16`, `braille` or `ascii` — so a variant can
//! be looked at on a terminal that would never have asked for it.
//!
//! Three sizes are generated: the rail's mark, the card's mark, and the
//! splash. [`fitted`] is how a caller gets the largest one its room allows,
//! and nothing where even the smallest does not fit — a mark that pushed a
//! version number out of a window would be a logo about itself.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::capability::{Capability, ColorCap, GlyphSet};

// `LogoCell`, `LogoRow` and the `LOGO_*` statics, as `build.rs` wrote them.
include!(concat!(env!("OUT_DIR"), "/logo.rs"));

/// The sizes the interface asks for: the rail's mark, a card's mark, and the
/// splash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoSize {
    Sm,
    Md,
    Lg,
}

/// How the mark is drawn, which is a question of what the terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// Half-block cells, 24-bit colour.
    TrueColor,
    /// Half-block cells, xterm 256 entries.
    Color256,
    /// Half-block cells, the two greens the sixteen names have.
    Color16,
    /// Braille dots, in the terminal's own ink.
    Braille,
    /// The grey ASCII ramp, one pixel per cell.
    Ascii,
}

impl Variant {
    /// The spelling this terminal's capability calls for.
    pub fn for_capability(capability: Capability) -> Variant {
        Variant::for_parts(
            capability,
            std::env::var("LINKR_LOGO").ok().as_deref(),
            crate::ui::legacy::legacy_console(),
        )
    }

    /// The rule, apart from the environment, so the tests can walk it.
    ///
    /// An explicit `LINKR_LOGO` wins outright — it is the one knob that exists
    /// to look at a variant the terminal would not otherwise get. A terminal
    /// whose glyph set is the ASCII one has already said its font is not to be
    /// counted on for the drawn mark — the old console, a `TERM` of `linux` or
    /// `dumb`, a locale that is not UTF-8 — so it gets the ramp, colour or no
    /// colour. Everything else is the colour question: the three depths, and
    /// the dots where there is no colour at all.
    fn for_parts(capability: Capability, forced: Option<&str>, legacy: bool) -> Variant {
        if let Some(variant) = forced.and_then(forced_variant) {
            return variant;
        }
        if capability.glyphs == GlyphSet::Ascii {
            return Variant::Ascii;
        }
        match capability.color {
            ColorCap::TrueColor => Variant::TrueColor,
            ColorCap::Color256 => Variant::Color256,
            ColorCap::Color16 => Variant::Color16,
            // No colour: the dots are the mark itself, where the font can be
            // counted on for them. The old console's cannot.
            ColorCap::Mono if legacy => Variant::Ascii,
            ColorCap::Mono => Variant::Braille,
        }
    }

    /// This variant, at `size`.
    pub fn rows(self, size: LogoSize) -> &'static [LogoRow] {
        use LogoSize::{Lg, Md, Sm};
        match self {
            Variant::TrueColor => match size {
                Sm => LOGO_TRUECOLOR_SM,
                Md => LOGO_TRUECOLOR_MD,
                Lg => LOGO_TRUECOLOR_LG,
            },
            Variant::Color256 => match size {
                Sm => LOGO_256_SM,
                Md => LOGO_256_MD,
                Lg => LOGO_256_LG,
            },
            Variant::Color16 => match size {
                Sm => LOGO_16_SM,
                Md => LOGO_16_MD,
                Lg => LOGO_16_LG,
            },
            Variant::Braille => match size {
                Sm => LOGO_BRAILLE_SM,
                Md => LOGO_BRAILLE_MD,
                Lg => LOGO_BRAILLE_LG,
            },
            Variant::Ascii => match size {
                Sm => LOGO_ASCII_SM,
                Md => LOGO_ASCII_MD,
                Lg => LOGO_ASCII_LG,
            },
        }
    }
}

/// The variant an override names, when it names one.
fn forced_variant(raw: &str) -> Option<Variant> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "truecolor" | "24bit" => Some(Variant::TrueColor),
        "256" | "256color" => Some(Variant::Color256),
        "16" | "ansi" => Some(Variant::Color16),
        "braille" => Some(Variant::Braille),
        "ascii" => Some(Variant::Ascii),
        _ => None,
    }
}

// --- the picture -------------------------------------------------------------

/// How many cells a variant's picture at `size` takes: `(columns, rows)`.
pub fn size_in_cells(capability: Capability, size: LogoSize) -> (u16, u16) {
    let rows = Variant::for_capability(capability).rows(size);
    (
        rows.first().map_or(0, |row| row.cells.len()) as u16,
        rows.len() as u16,
    )
}

/// The mark at `size`, as paintable lines: one span per cell, so the picture
/// carries its own colours whatever theme is in force.
pub fn lines(capability: Capability, size: LogoSize) -> Vec<Line<'static>> {
    Variant::for_capability(capability)
        .rows(size)
        .iter()
        .map(|row| {
            Line::from(
                row.cells
                    .iter()
                    .map(|cell| {
                        Span::styled(
                            cell.ch.to_string(),
                            Style::default().fg(cell.fg).bg(cell.bg),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// The largest size whose picture fits in `room`, or nothing when even the
/// small one is too big.
pub fn fitted(capability: Capability, room: (u16, u16)) -> Option<(LogoSize, Vec<Line<'static>>)> {
    [LogoSize::Lg, LogoSize::Md, LogoSize::Sm]
        .into_iter()
        .find_map(|size| {
            let (width, height) = size_in_cells(capability, size);
            (width <= room.0 && height <= room.1).then(|| (size, lines(capability, size)))
        })
}

/// [`fitted`]'s lines, or an empty body when nothing fits.
pub fn fitted_lines(capability: Capability, room: (u16, u16)) -> Vec<Line<'static>> {
    fitted(capability, room)
        .map(|(_, lines)| lines)
        .unwrap_or_default()
}

/// Paint the largest mark that fits, centred in `area`.
///
/// For the one caller that paints a frame of its own — the splash — rather
/// than composing lines into a view: the Layout does the two centres, and a
/// terminal too small for even the small mark simply shows nothing.
pub fn render(frame: &mut Frame, area: Rect, capability: Capability) {
    let Some((_, lines)) = fitted(capability, (area.width, area.height)) else {
        return;
    };
    let height = lines.len() as u16;
    let width = lines
        .first()
        .map(|line| line.width() as u16)
        .unwrap_or_default();
    let rows = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(height),
        Constraint::Min(0),
    ])
    .split(area)[1];
    let rect = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(width),
        Constraint::Min(0),
    ])
    .split(rows)[1];
    frame.render_widget(Paragraph::new(lines), rect);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(color: ColorCap, glyphs: GlyphSet) -> Capability {
        Capability { color, glyphs }
    }

    /// Every colour the interface can be forced into gets its own spelling,
    /// the ones without colour get the spellings that need none, and a
    /// terminal that asked for ASCII glyphs gets the ramp whatever colours it
    /// can print.
    #[test]
    fn the_variant_follows_the_capability() {
        assert_eq!(
            Variant::for_parts(cap(ColorCap::TrueColor, GlyphSet::Unicode), None, false),
            Variant::TrueColor
        );
        assert_eq!(
            Variant::for_parts(cap(ColorCap::Color256, GlyphSet::Unicode), None, false),
            Variant::Color256
        );
        assert_eq!(
            Variant::for_parts(cap(ColorCap::Color16, GlyphSet::Unicode), None, false),
            Variant::Color16
        );
        assert_eq!(
            Variant::for_parts(cap(ColorCap::Mono, GlyphSet::Unicode), None, false),
            Variant::Braille
        );
        assert_eq!(
            Variant::for_parts(cap(ColorCap::Mono, GlyphSet::Ascii), None, false),
            Variant::Ascii
        );
        // A font that asked for ASCII is a font the drawn mark cannot be
        // counted on for — `TERM=linux` is sixteen-colour and still gets the
        // ramp.
        for color in [ColorCap::TrueColor, ColorCap::Color256, ColorCap::Color16] {
            assert_eq!(
                Variant::for_parts(cap(color, GlyphSet::Ascii), None, false),
                Variant::Ascii,
                "{color:?} with ASCII glyphs did not take the ramp"
            );
        }
        // The old console cannot be counted on for braille, and a coloured
        // terminal that has been given no colour still has its font.
        assert_eq!(
            Variant::for_parts(cap(ColorCap::Mono, GlyphSet::Unicode), None, true),
            Variant::Ascii
        );
        assert_eq!(
            Variant::for_parts(cap(ColorCap::TrueColor, GlyphSet::Unicode), None, true),
            Variant::TrueColor
        );
    }

    /// The override names every variant, and a word that names none leaves the
    /// terminal's own answer standing.
    #[test]
    fn an_override_wins_by_name() {
        let mono = cap(ColorCap::Mono, GlyphSet::Ascii);
        for (word, expected) in [
            ("truecolor", Variant::TrueColor),
            ("24bit", Variant::TrueColor),
            ("256", Variant::Color256),
            ("256color", Variant::Color256),
            ("16", Variant::Color16),
            ("ansi", Variant::Color16),
            ("braille", Variant::Braille),
            ("ascii", Variant::Ascii),
            (" TRUEcolor ", Variant::TrueColor),
        ] {
            assert_eq!(
                Variant::for_parts(mono, Some(word), false),
                expected,
                "{word} did not name {expected:?}"
            );
        }
        assert_eq!(
            Variant::for_parts(mono, Some("stained glass"), false),
            Variant::Ascii,
            "an unknown word is not a variant"
        );
    }

    /// The three sizes arrive at the sizes the design asked for: the mark is
    /// as wide as it was generated, and half as tall as it is wide — square
    /// on screen for the coloured and braille cells, and 2:1 for the ramp.
    #[test]
    fn the_sizes_are_the_designed_ones() {
        for variant in [
            Variant::TrueColor,
            Variant::Color256,
            Variant::Color16,
            Variant::Braille,
        ] {
            for (size, expected) in [(LogoSize::Sm, 24), (LogoSize::Md, 48), (LogoSize::Lg, 72)] {
                let rows = variant.rows(size);
                assert_eq!(
                    rows.len(),
                    expected / 2,
                    "{variant:?} {size:?} is not half as tall"
                );
                for row in rows {
                    assert_eq!(
                        row.cells.len(),
                        expected,
                        "{variant:?} {size:?} is not {expected} wide"
                    );
                }
            }
        }
        for (size, expected) in [(LogoSize::Sm, 40), (LogoSize::Md, 72), (LogoSize::Lg, 108)] {
            let rows = Variant::Ascii.rows(size);
            assert_eq!(rows.len(), expected / 2);
            for row in rows {
                assert_eq!(row.cells.len(), expected);
            }
        }
    }

    /// The ramp is ASCII, and every other variant is one cell wide in the
    /// widths the layout counts with.
    #[test]
    fn the_ramp_is_ascii() {
        for size in [LogoSize::Sm, LogoSize::Md, LogoSize::Lg] {
            for row in Variant::Ascii.rows(size) {
                for cell in row.cells {
                    assert!(cell.ch.is_ascii(), "{:?} is not ASCII", cell.ch);
                    assert_eq!(crate::text::columns(&cell.ch.to_string()), 1);
                }
            }
        }
    }

    /// The braille variant is braille: one dot cell per cell, and nothing
    /// else — a font that has U+2800 at all has the block.
    #[test]
    fn the_dots_are_braille() {
        for size in [LogoSize::Sm, LogoSize::Md, LogoSize::Lg] {
            for row in Variant::Braille.rows(size) {
                for cell in row.cells {
                    assert!(
                        ('\u{2800}'..='\u{28ff}').contains(&cell.ch),
                        "{:?} is not a braille cell",
                        cell.ch
                    );
                    assert_eq!(cell.fg, ratatui::style::Color::Reset);
                    assert_eq!(cell.bg, ratatui::style::Color::Reset);
                }
            }
        }
    }

    /// The coloured variants use their own colour depth, and the sixteen-colour
    /// one uses the two names the brand green has — never a grey or a white
    /// speckle where an anti-aliased edge was.
    #[test]
    fn the_colours_are_the_depth_they_claim() {
        use ratatui::style::Color;
        for row in Variant::TrueColor.rows(LogoSize::Sm) {
            for cell in row.cells {
                assert!(
                    matches!(cell.fg, Color::Rgb(..) | Color::Reset),
                    "{:?}",
                    cell.fg
                );
                assert!(
                    matches!(cell.bg, Color::Rgb(..) | Color::Reset),
                    "{:?}",
                    cell.bg
                );
            }
        }
        for row in Variant::Color256.rows(LogoSize::Sm) {
            for cell in row.cells {
                assert!(
                    matches!(cell.fg, Color::Indexed(16..=255) | Color::Reset),
                    "{:?} is not an xterm entry",
                    cell.fg
                );
                assert!(
                    matches!(cell.bg, Color::Indexed(16..=255) | Color::Reset),
                    "{:?} is not an xterm entry",
                    cell.bg
                );
            }
        }
        for row in Variant::Color16.rows(LogoSize::Sm) {
            for cell in row.cells {
                for colour in [cell.fg, cell.bg] {
                    assert!(
                        matches!(colour, Color::Green | Color::LightGreen | Color::Reset),
                        "{colour:?} is not one of the two greens"
                    );
                }
            }
        }
    }

    /// The mark is a mark and not a blank: the coloured variants paint the
    /// stroke, and the inkless ones put their own characters on the cells.
    #[test]
    fn every_variant_draws_something() {
        use ratatui::style::Color;
        for variant in [
            Variant::TrueColor,
            Variant::Color256,
            Variant::Color16,
            Variant::Braille,
            Variant::Ascii,
        ] {
            let rows = variant.rows(LogoSize::Sm);
            let inked = rows
                .iter()
                .flat_map(|row| row.cells)
                .filter(|cell| match variant {
                    // The coloured variants are ink: a cell that colours
                    // anything at all is a cell of the mark.
                    Variant::TrueColor | Variant::Color256 | Variant::Color16 => {
                        cell.fg != Color::Reset || cell.bg != Color::Reset
                    }
                    // The inkless ones are characters, on the terminal's own
                    // ground: a blank cell and an empty braille cell are the
                    // transparent ones.
                    Variant::Braille => cell.ch != '\u{2800}',
                    Variant::Ascii => cell.ch != ' ',
                })
                .count();
            assert!(inked > 20, "{variant:?} drew {inked} cells of a mark");
        }
    }

    /// A room fits the largest size it can hold, and a room too small for even
    /// the small mark gets nothing rather than a clipped picture.
    #[test]
    fn the_fit_takes_the_largest_that_fits() {
        let full = Capability::FULL;
        let (size, lines) = fitted(full, (24, 12)).expect("the small mark fits 24×12");
        assert_eq!(size, LogoSize::Sm);
        assert_eq!(lines.len(), 12);
        let (size, _) = fitted(full, (48, 24)).expect("the middle mark fits 48×24");
        assert_eq!(size, LogoSize::Md);
        let (size, _) = fitted(full, (200, 200)).expect("the splash mark fits a big window");
        assert_eq!(size, LogoSize::Lg);
        assert!(
            fitted(full, (23, 12)).is_none(),
            "a column short is nothing"
        );
        assert!(fitted(full, (24, 11)).is_none(), "a row short is nothing");
        assert!(fitted_lines(full, (0, 0)).is_empty());
        // The ramp is wider than the half-blocks, and the fit follows the
        // variant's own shape rather than a shared guess.
        let ascii = cap(ColorCap::Mono, GlyphSet::Ascii);
        assert!(fitted(ascii, (30, 20)).is_none(), "the ramp is 40 wide");
        assert_eq!(
            fitted(ascii, (40, 20)).map(|(size, _)| size),
            Some(LogoSize::Sm)
        );
    }
}
