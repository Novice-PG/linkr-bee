//! The two things this crate makes at build time: the Windows executable's
//! resource section, and the logo.
//!
//! The icon: only Windows carries resources, and only a Windows build should
//! pay for it — but "is the target Windows" is a question for the environment
//! Cargo sets and not for `#[cfg(windows)]`: a build script is compiled for the
//! *host*, and the GNU-ABI Windows binaries this project ships are
//! cross-compiled from Linux, where that attribute is false and the icon would
//! go missing without a word.
//!
//! The logo: `assets/logo.svg` is rasterised here, at compile time, with resvg
//! — pure Rust, running on the host, so cross-compilation is untouched and no
//! part of the SVG renderer reaches the shipped binary. Each cell of the
//! picture becomes a `(char, fg, bg)` triple in `OUT_DIR/logo.rs`, which
//! `src/logo.rs` includes; the runtime then picks the variant the terminal's
//! own capability calls for, and never links a renderer to do it.

use std::fs;
use std::path::PathBuf;

use resvg::tiny_skia::{Pixmap, Transform};
use usvg::Tree;

/// The box `assets/logo.svg` draws in. Every raster is this, scaled.
const SVG_SIZE: f32 = 200.0;

/// (suffix, half cols, braille cols, ascii cols) for the three sizes the
/// interface asks for: the rail's mark, the card's mark, the splash.
///
/// Half-block and braille cells are twice as tall as they are wide, and the
/// SVG is square, so those rasters are square and their cells come out square
/// on screen. A plain ASCII cell is 1:1, so that raster is 2:1 instead.
const SIZES: &[(&str, u32, u32, u32)] =
    &[("SM", 24, 24, 40), ("MD", 48, 48, 72), ("LG", 72, 72, 108)];

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/logo.svg");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Relative to this crate's root, which is where a build script runs.
        winresource::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .compile()
            .expect("the Windows icon could not be embedded");
    }
    render_logo();
}

/// The SVG, drawn at `w`×`h` pixels.
fn raster(w: u32, h: u32) -> Pixmap {
    let data = fs::read("assets/logo.svg").expect("assets/logo.svg could not be read");
    let tree = Tree::from_data(&data, &usvg::Options::default()).expect("the logo is not an SVG");
    let mut pm = Pixmap::new(w, h).expect("a pixmap at the asked-for size");
    let tr = Transform::from_scale(w as f32 / SVG_SIZE, h as f32 / SVG_SIZE);
    resvg::render(&tree, tr, &mut pm.as_mut());
    pm
}

// --- colour quantisation -----------------------------------------------------

/// The xterm 6×6×6 cube, for the 256-colour variant.
fn xterm256(r: u8, g: u8, b: u8) -> u8 {
    fn cv(v: u8) -> u8 {
        if v < 75 {
            0
        } else {
            ((u16::from(v) - 35) / 40) as u8
        }
    }
    16 + 36 * cv(r) + 6 * cv(g) + cv(b)
}

/// A pixel's colour over the terminal's own background: the SVG has
/// transparency in it, and a cell is one colour, not one alpha.
fn over_bg(p: &[u8]) -> (u8, u8, u8) {
    let a = f32::from(p[3]) / 255.0;
    (
        (f32::from(p[0]) * a) as u8,
        (f32::from(p[1]) * a) as u8,
        (f32::from(p[2]) * a) as u8,
    )
}

// --- code emission: a cell is (char, fg token, bg token) ---------------------

fn col_tok_true(c: (u8, u8, u8)) -> String {
    format!("Color::Rgb({},{},{})", c.0, c.1, c.2)
}

fn col_tok_256(c: (u8, u8, u8)) -> String {
    format!("Color::Indexed({})", xterm256(c.0, c.1, c.2))
}

/// The brand green, as one of the sixteen: 0 none, 1 green, 2 bright green.
///
/// The mark is one hue on a transparent ground, so the sixteen-colour variant
/// is not a degraded picture but the picture in the two greens an ANSI palette
/// spells: the stroke, and the fill the anti-aliased edge becomes.
fn green_level(c: (u8, u8, u8)) -> u8 {
    let (r, g, b) = c;
    if u16::from(g) < 70 {
        return 0;
    }
    if u16::from(g) > u16::from(r) * 115 / 100 && u16::from(g) > u16::from(b) * 105 / 100 {
        if g > 150 {
            2
        } else {
            1
        }
    } else {
        0
    }
}

fn col_tok_16(v: u8) -> &'static str {
    match v {
        2 => "Color::LightGreen",
        1 => "Color::Green",
        _ => "Color::Reset",
    }
}

/// One cell, as a `LogoCell { .. }` source fragment.
fn cell(ch: char, fg: &str, bg: &str) -> String {
    let ch_s = match ch {
        ' ' => "' '".to_string(),
        c => format!("'{}'", c.escape_default().collect::<String>()),
    };
    format!("LogoCell{{ch:{},fg:{},bg:{}}}", ch_s, fg, bg)
}

/// Half-block cells: two pixel rows per cell, level 3 truecolour, 2 indexed,
/// 1 brand green.
///
/// The four cases are the whole art: both pixels transparent is a space, and
/// each of the others is a `▀` with the colours that carry it — never a `▀`
/// with the *background* standing in for a missing pixel, which paints the
/// terminal's default foreground over half the cell.
fn half_cells(pm: &Pixmap, cols: u32, level: u8) -> Vec<Vec<String>> {
    let rows = cols / 2;
    let d = pm.data();
    let pick = |p: &[u8]| if p[3] < 12 { None } else { Some(over_bg(p)) };
    let mut out = Vec::new();
    for row in 0..rows {
        let mut r = Vec::new();
        for col in 0..cols {
            let top = &d[((row * 2) * cols + col) as usize * 4..][..4];
            let bot = &d[((row * 2 + 1) * cols + col) as usize * 4..][..4];
            let t = pick(top);
            let b = pick(bot);
            let (ch, fg, bg) = match level {
                3 => match (t, b) {
                    (None, None) => (' ', "Color::Reset", "Color::Reset"),
                    (Some(c), None) => ('\u{2580}', &*col_tok_true(c), "Color::Reset"),
                    (None, Some(c)) => ('\u{2584}', &*col_tok_true(c), "Color::Reset"),
                    (Some(c1), Some(c2)) => ('\u{2580}', &*col_tok_true(c1), &*col_tok_true(c2)),
                },
                2 => match (t, b) {
                    (None, None) => (' ', "Color::Reset", "Color::Reset"),
                    (Some(c), None) => ('\u{2580}', &*col_tok_256(c), "Color::Reset"),
                    (None, Some(c)) => ('\u{2584}', &*col_tok_256(c), "Color::Reset"),
                    (Some(c1), Some(c2)) => ('\u{2580}', &*col_tok_256(c1), &*col_tok_256(c2)),
                },
                _ => {
                    let fv = green_level(t.unwrap_or((0, 0, 0)));
                    let bv = green_level(b.unwrap_or((0, 0, 0)));
                    match (fv, bv) {
                        (0, 0) => (' ', "Color::Reset", "Color::Reset"),
                        (f, 0) => ('\u{2580}', col_tok_16(f), "Color::Reset"),
                        (0, b) => ('\u{2584}', col_tok_16(b), "Color::Reset"),
                        (f, b) => ('\u{2580}', col_tok_16(f), col_tok_16(b)),
                    }
                }
            };
            r.push(cell(ch, fg, bg));
        }
        out.push(r);
    }
    out
}

/// Braille cells: a 2×4 dot grid per cell, in the terminal's own ink.
///
/// Two pixel columns and four pixel rows per cell make a square picture out of
/// one-cell-wide, two-cell-wide glyphs, and the dots themselves are what a
/// console with no colour at all still renders.
fn braille_cells(pm: &Pixmap, cols: u32) -> Vec<Vec<String>> {
    let pw = cols * 2;
    let rows = cols / 2;
    let d = pm.data();
    let dots = [
        (0u32, 0u32, 0x01u16),
        (0, 1, 0x02),
        (0, 2, 0x04),
        (1, 0, 0x08),
        (1, 1, 0x10),
        (1, 2, 0x20),
        (0, 3, 0x40),
        (1, 3, 0x80),
    ];
    let mut out = Vec::new();
    for row in 0..rows {
        let mut r = Vec::new();
        for col in 0..cols {
            let mut code = 0x2800u16;
            for (dx, dy, bit) in dots {
                if d[((row * 4 + dy) * pw + col * 2 + dx) as usize * 4 + 3] > 120 {
                    code |= bit;
                }
            }
            r.push(cell(
                char::from_u32(u32::from(code)).expect("a braille code point"),
                "Color::Reset",
                "Color::Reset",
            ));
        }
        out.push(r);
    }
    out
}

/// ASCII cells: one pixel per cell, down a grey ramp.
fn ascii_cells(pm: &Pixmap, cols: u32) -> Vec<Vec<String>> {
    let rows = cols / 2;
    let d = pm.data();
    let ramp = b" .'`,:;Il!i><~+_-?][}{1)(|/tfjrxnuvczXYUJCLQ0OZmwqpdbkhao*#MW&8%B@$";
    let mut out = Vec::new();
    for row in 0..rows {
        let mut r = Vec::new();
        for col in 0..cols {
            let p = &d[(row * cols + col) as usize * 4..][..4];
            let c = over_bg(p);
            // Wide enough for the weights: 255×587 does not fit in a `u16`,
            // and a luminance that wrapped would paint the ramp backwards.
            let v = (u32::from(c.0) * 299 + u32::from(c.1) * 587 + u32::from(c.2) * 114) / 1000;
            let idx = v as usize * (ramp.len() - 1) / 255;
            r.push(cell(ramp[idx] as char, "Color::Reset", "Color::Reset"));
        }
        out.push(r);
    }
    out
}

/// One variant, as a named static.
fn static_logo(name: &str, rows: &[Vec<String>]) -> String {
    let body = rows
        .iter()
        .map(|r| format!("LogoRow{{cells:&[{}]}}", r.join(",")))
        .collect::<Vec<_>>()
        .join(",\n");
    format!("pub static {name}: &[LogoRow] = &[{body}];\n")
}

/// Write `OUT_DIR/logo.rs`: the cell data, and nothing that knows what a
/// terminal is.
fn render_logo() {
    let mut code = String::from(
        "use ratatui::style::Color;\n\
         #[derive(Clone, Copy)] pub struct LogoCell { pub ch: char, pub fg: Color, pub bg: Color }\n\
         #[derive(Clone, Copy)] pub struct LogoRow { pub cells: &'static [LogoCell] }\n",
    );
    for (suffix, half_cols, br_cols, as_cols) in SIZES {
        let pm_half = raster(*half_cols, *half_cols);
        let pm_br = raster(*br_cols * 2, *br_cols * 2);
        let pm_as = raster(*as_cols, *as_cols / 2);
        let n = |base: &str| format!("{base}_{suffix}");
        code.push_str(&static_logo(
            &n("LOGO_TRUECOLOR"),
            &half_cells(&pm_half, *half_cols, 3),
        ));
        code.push_str(&static_logo(
            &n("LOGO_256"),
            &half_cells(&pm_half, *half_cols, 2),
        ));
        code.push_str(&static_logo(
            &n("LOGO_16"),
            &half_cells(&pm_half, *half_cols, 1),
        ));
        code.push_str(&static_logo(
            &n("LOGO_BRAILLE"),
            &braille_cells(&pm_br, *br_cols),
        ));
        code.push_str(&static_logo(
            &n("LOGO_ASCII"),
            &ascii_cells(&pm_as, *as_cols),
        ));
    }
    let out =
        PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by Cargo")).join("logo.rs");
    fs::write(out, code).expect("OUT_DIR/logo.rs could not be written");
}
