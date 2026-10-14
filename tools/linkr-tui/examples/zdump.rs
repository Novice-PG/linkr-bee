//! Describe the frames and subpackets in a recorded ZMODEM stream.
//!
//! The reference workflow for this module is "record a real session, then read
//! it", and this is the reading half: one line per header, one per subpacket,
//! with the flags in logical `f0..f3` order, the decoded position, and the tail of
//! each payload so a truncation is visible rather than merely short.
//!
//! It exists because the answers that matter here are not in the specification.
//! The block size, the `ZCRCG`/`ZCRCE` rule, whether a zero-byte file gets a
//! `ZDATA` header, and which byte lrzsz puts in a `ZFILE`'s first flag are all
//! questions the recordings answer and the documents disagree about.
//!
//! ```text
//! cargo run --example zdump -- tests/fixtures/zmodem/basic-sz2rz.bin
//! ```
//!
//! `dev/zmodem-capture.py` carries the same walk in miniature, and writes it into
//! each case's `transcript.txt`.

use linkr_tui::zmodem::{Event, Parser};

fn main() {
    let path = match std::env::args().nth(1) {
        Some(path) => path,
        None => {
            eprintln!("usage: zdump <recording>");
            std::process::exit(2);
        }
    };
    let stream = match std::fs::read(&path) {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("{path}: {err}");
            std::process::exit(2);
        }
    };

    let mut parser = Parser::new();
    parser.feed(&stream);
    let mut subpacket = 0;
    while let Some(event) = parser.next_event() {
        match event {
            Event::Header { header, encoding } => println!(
                "hdr  {:<8} {:<6} f0={:02x} f1={:02x} f2={:02x} f3={:02x} pos={}",
                format!("{:?}", header.kind),
                format!("{encoding:?}"),
                header.f0(),
                header.f1(),
                header.f2(),
                header.f3(),
                u64::from(header.position()),
            ),
            Event::Subpacket { data, end } => {
                let tail: Vec<u8> = data.iter().rev().take(12).rev().copied().collect();
                println!(
                    "sub{:<2} len={:<6} end={:<6} tail={:?}",
                    subpacket,
                    data.len(),
                    format!("{end:?}"),
                    String::from_utf8_lossy(&tail),
                );
                subpacket += 1;
            }
            Event::Corrupt(what) => println!("CORRUPT {what:?}"),
        }
    }
    println!("stats    {:?}", parser.stats());
    println!("buffered {}", parser.buffered());
}
