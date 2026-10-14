//! The streaming framer: bytes in from the link, events out.
//!
//! This is the same shape as [`crate::vt::parser`], for the same reason. A link
//! hands over whatever arrived — half a header, one byte, a whole file — and the
//! framer's job is to turn that into "here is a header" and "here is a subpacket"
//! without ever assuming a read boundary means anything. Everything else in the
//! protocol sits on top of this and can therefore be tested against recorded
//! traffic instead of a pretend channel.
//!
//! # The one piece of state that is not local
//!
//! A data subpacket has no length field. It ends at `ZDLE` plus a frameend byte,
//! so after a header that carries one, the framer stops looking for headers and
//! starts collecting. What tells it when to stop collecting and start looking
//! again is the frameend byte itself:
//!
//! - `ZCRCG`/`ZCRCQ` promise another subpacket follows with no header between,
//!   which is how a large file streams as back-to-back blocks.
//! - `ZCRCE`/`ZCRCW` end the run, so a header comes next.
//!
//! Getting that wrong in the permissive direction is the dangerous one: treating
//! a header as data swallows it, and the transfer then stalls in a way that looks
//! like a dead link rather than a decoding bug. A recorded 4112-byte transfer
//! shows the exact shape — one `ZFILE`, one `ZDATA`, four continuation
//! subpackets ending `i i i i`, then one `h` to close the run — and the
//! integration test replays it byte for byte.

use std::collections::VecDeque;

use super::crc::{crc16, Crc16, Crc32, Width};
use super::escape::{unescape, Framend, ZDLE, ZPAD};
use super::frame::{Encoding, Header};

/// The largest subpacket the protocol allows. lrzsz will go to 8 KB with `-8`;
/// 1 KB is the common case. Used only as a bound on a stream that never ends a
/// subpacket, so that a hostile or broken peer cannot grow the buffer without
/// limit.
pub const MAX_SUBPACKET: usize = 8192;

/// How much framed input to hold while waiting for the rest of a frame. A
/// subpacket's bytes can double under escaping, so this is generous relative to
/// [`MAX_SUBPACKET`].
const MAX_BUFFERED: usize = 4 * MAX_SUBPACKET;

/// Something arrived that did not check out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corruption {
    /// A header's frame check failed, or its type byte is not one ZMODEM
    /// defines. The receiver's answer to either is the same: `ZNAK`.
    Header,
    /// A subpacket's frame check failed. The receiver answers `ZRPOS` at the
    /// last offset it accepted, and the sender resends from there.
    Subpacket,
    /// A subpacket never ended. The buffer is dropped and the framer resyncs on
    /// the next header, which loses whatever was in flight.
    Oversize,
}

/// What the framer found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A header whose frame check passed. `encoding` says which of the three
    /// shapes it arrived in, which is also what the sender is using.
    Header { header: Header, encoding: Encoding },
    /// A complete subpacket. `end` says what the sender expects to happen next.
    Subpacket { data: Vec<u8>, end: Framend },
    /// A frame that failed its check.
    Corrupt(Corruption),
}

/// What a transfer has cost so far, for the diagnostics view. Cheap to keep, and
/// the alternative when a transfer stalls is guessing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Bytes dropped while looking for a header — banner text, the `rz\r`
    /// wakeup, flow-control noise.
    pub discarded: u64,
    /// Bytes handed out as subpacket payload.
    pub payload: u64,
    /// Headers accepted.
    pub headers: u64,
    /// Subpackets accepted.
    pub subpackets: u64,
    /// Frames that failed a check.
    pub corrupt: u64,
}

/// What the framer is currently looking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expecting {
    Header,
    Subpacket,
}

/// A byte-at-a-time ZMODEM framer.
pub struct Parser {
    buf: Vec<u8>,
    events: VecDeque<Event>,
    expecting: Expecting,
    width: Width,
    encoding: Encoding,
    stats: Stats,
}

impl Default for Parser {
    fn default() -> Parser {
        Parser::new()
    }
}

impl Parser {
    pub fn new() -> Parser {
        Parser {
            buf: Vec::new(),
            events: VecDeque::new(),
            expecting: Expecting::Header,
            width: Width::default(),
            encoding: Encoding::Bin32,
            stats: Stats::default(),
        }
    }

    /// Hand the framer whatever the link delivered. Any split is legal: a feed
    /// may fall in the middle of a header, an escape, or a check.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        self.run();
    }

    /// The next thing the framer found, if any.
    pub fn next_event(&mut self) -> Option<Event> {
        self.run();
        self.events.pop_front()
    }

    /// The check width in force for the subpacket currently being collected.
    pub fn width(&self) -> Width {
        self.width
    }

    /// The shape of the last header seen.
    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// Whether a subpacket is being collected rather than a header.
    pub fn collecting(&self) -> bool {
        self.expecting == Expecting::Subpacket
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Bytes sitting in the framer because a frame is still incomplete.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// Abandon any partial frame and start looking for a header again.
    pub fn reset(&mut self) {
        self.stats.discarded += self.buf.len() as u64;
        self.buf.clear();
        self.expecting = Expecting::Header;
    }

    fn run(&mut self) {
        while self.step() {}
    }

    fn step(&mut self) -> bool {
        match self.expecting {
            Expecting::Header => self.step_header(),
            Expecting::Subpacket => self.step_subpacket(),
        }
    }

    // -- headers ---------------------------------------------------------- //

    fn step_header(&mut self) -> bool {
        if self.find_marker().is_none() || self.buf.len() < 3 {
            return false;
        }
        match self.buf[2] {
            b'B' => self.read_hex_header(),
            b'A' => self.read_binary_header(Encoding::Bin16),
            b'C' => self.read_binary_header(Encoding::Bin32),
            _ => {
                // `ZPAD ZDLE` followed by anything else is not a header. Drop
                // the padding and resync; a header must begin with a fresh
                // `ZPAD ZDLE`, so nothing is lost by discarding this one.
                self.stats.discarded += 1;
                self.buf.drain(..1);
                true
            }
        }
    }

    /// Locate the `ZPAD ZDLE` that introduces a header, dropping anything ahead
    /// of it.
    ///
    /// Returns `None` when the buffer does not yet contain a complete pair, in
    /// which case a *trailing* `ZPAD` is kept: it may be the first half of one,
    /// and dropping it would lose a header that arrived split across two reads.
    fn find_marker(&mut self) -> Option<usize> {
        let mut i = 0;
        while i + 1 < self.buf.len() {
            if self.buf[i] == ZPAD && self.buf[i + 1] == ZDLE {
                if i > 0 {
                    self.stats.discarded += i as u64;
                    self.buf.drain(..i);
                }
                return Some(0);
            }
            i += 1;
        }
        let keep = usize::from(self.buf.last() == Some(&ZPAD));
        let drop = self.buf.len() - keep;
        if drop > 0 {
            self.stats.discarded += drop as u64;
            self.buf.drain(..drop);
        }
        None
    }

    fn read_hex_header(&mut self) -> bool {
        const DIGITS: usize = 14;
        if self.buf.len() < 3 + DIGITS {
            return false;
        }
        let mut raw = [0u8; 7];
        for (index, slot) in raw.iter_mut().enumerate() {
            let pair = &self.buf[3 + index * 2..5 + index * 2];
            match (hex_value(pair[0]), hex_value(pair[1])) {
                (Some(high), Some(low)) => *slot = (high << 4) | low,
                _ => {
                    // Not hex after all: a false sync. Resync one byte on.
                    self.stats.discarded += 1;
                    self.buf.drain(..1);
                    return true;
                }
            }
        }
        self.buf.drain(..3 + DIGITS);
        self.swallow_hex_tail();

        let wire: [u8; 5] = raw[..5].try_into().expect("five bytes");
        let check = u16::from_be_bytes([raw[5], raw[6]]);
        // A hex header always carries a 16-bit check, whatever the transfer is
        // running, and so does any subpacket that follows it.
        self.accept(wire, Width::Sixteen, Encoding::Hex, crc16(&wire) == check);
        true
    }

    /// Consume the `CR`, optional `LF` (or the `0x8A` lrzsz writes in its
    /// place), and optional `XON` that close a hex header.
    ///
    /// Bounded to those three positions on purpose: a hex header can be followed
    /// by a data subpacket, and a greedy "skip until `ZPAD`" would eat it.
    fn swallow_hex_tail(&mut self) {
        if self.buf.first() == Some(&0x0D) {
            self.buf.drain(..1);
            if matches!(self.buf.first(), Some(0x0A) | Some(0x8A)) {
                self.buf.drain(..1);
            }
        }
        if self.buf.first() == Some(&0x11) {
            self.buf.drain(..1);
        }
    }

    fn read_binary_header(&mut self, encoding: Encoding) -> bool {
        let width = encoding.check();
        let need = 5 + width.bytes();
        let mut decoded = [0u8; 9];
        let mut filled = 0;
        let mut i = 3;
        while filled < need {
            let Some(&byte) = self.buf.get(i) else {
                return false;
            };
            if byte == ZDLE {
                let Some(&escaped) = self.buf.get(i + 1) else {
                    return false;
                };
                decoded[filled] = unescape(escaped);
                i += 2;
            } else {
                decoded[filled] = byte;
                i += 1;
            }
            filled += 1;
        }
        self.buf.drain(..i);

        let wire: [u8; 5] = decoded[..5].try_into().expect("five bytes");
        let ok = width.check(&wire, &decoded[5..need]);
        self.accept(wire, width, encoding, ok);
        true
    }

    /// Record one header, valid or not, and decide what to look for next.
    fn accept(&mut self, wire: [u8; 5], width: Width, encoding: Encoding, ok: bool) {
        let header = if ok { Header::from_wire(wire) } else { None };
        let Some(header) = header else {
            self.stats.corrupt += 1;
            self.events.push_back(Event::Corrupt(Corruption::Header));
            self.expecting = Expecting::Header;
            return;
        };
        self.stats.headers += 1;
        self.width = width;
        self.encoding = encoding;
        self.expecting = if header.kind.carries_subpacket() {
            Expecting::Subpacket
        } else {
            Expecting::Header
        };
        self.events.push_back(Event::Header { header, encoding });
    }

    // -- subpackets ------------------------------------------------------- //

    fn step_subpacket(&mut self) -> bool {
        let mut i = 0;
        let mut decoded = 0usize;
        while i < self.buf.len() {
            if self.buf[i] == ZDLE {
                let Some(&after) = self.buf.get(i + 1) else {
                    // A trailing ZDLE: the frameend byte has not arrived yet.
                    break;
                };
                if let Some(end) = Framend::from_byte(after) {
                    return self.finish_subpacket(i, end, decoded);
                }
                decoded += 1;
                i += 2;
            } else {
                decoded += 1;
                i += 1;
            }
        }
        self.guard_oversize(decoded);
        false
    }

    fn finish_subpacket(&mut self, end_at: usize, end: Framend, decoded: usize) -> bool {
        let width = self.width;

        // The check is escaped like everything else, so it has to be decoded
        // rather than sliced: a check byte that lands in the escape set takes
        // two bytes on the wire. The recorded name block is exactly this case
        // -- its check is `e4 10 b3 e9`, and the `0x10` travels as `ZDLE 0x50`,
        // so reading four raw bytes runs off the end of the frame.
        let mut check = [0u8; 4];
        let mut filled = 0;
        let mut i = end_at + 2;
        while filled < width.bytes() {
            let Some(&byte) = self.buf.get(i) else {
                self.guard_oversize(decoded);
                return false;
            };
            if byte == ZDLE {
                let Some(&escaped) = self.buf.get(i + 1) else {
                    self.guard_oversize(decoded);
                    return false;
                };
                check[filled] = unescape(escaped);
                i += 2;
            } else {
                check[filled] = byte;
                i += 1;
            }
            filled += 1;
        }

        let data = unescape_all(&self.buf[..end_at], decoded);
        self.buf.drain(..i);

        let ok = match width {
            Width::Sixteen => {
                let mut crc = Crc16::new();
                crc.update(&data);
                crc.update(&[end.byte()]);
                crc.bytes() == [check[0], check[1]]
            }
            Width::ThirtyTwo => {
                let mut crc = Crc32::new();
                crc.update(&data);
                crc.update(&[end.byte()]);
                crc.bytes() == [check[0], check[1], check[2], check[3]]
            }
        };

        if ok {
            self.stats.payload += data.len() as u64;
            self.stats.subpackets += 1;
            self.events.push_back(Event::Subpacket { data, end });
        } else {
            self.stats.corrupt += 1;
            self.events.push_back(Event::Corrupt(Corruption::Subpacket));
        }

        // `ZCRCG`/`ZCRCQ` promise the next subpacket with no header in between.
        self.expecting = if end.continues() {
            Expecting::Subpacket
        } else {
            Expecting::Header
        };
        true
    }

    /// Refuse to grow without limit when nothing ever ends the subpacket.
    fn guard_oversize(&mut self, decoded: usize) {
        if decoded <= MAX_SUBPACKET && self.buf.len() <= MAX_BUFFERED {
            return;
        }
        self.stats.corrupt += 1;
        self.events.push_back(Event::Corrupt(Corruption::Oversize));
        self.reset();
    }
}

/// Undo ZDLE escaping over a whole run.
///
/// `decoded` is the caller's count of the bytes this run stands for, which lets
/// the result be reserved exactly once; it is not trusted for correctness.
fn unescape_all(encoded: &[u8], decoded: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(decoded);
    let mut i = 0;
    while i < encoded.len() {
        if encoded[i] == ZDLE && i + 1 < encoded.len() {
            out.push(unescape(encoded[i + 1]));
            i += 2;
        } else {
            out.push(encoded[i]);
            i += 1;
        }
    }
    out
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        // The spec forbids upper case -- it false-triggers X/YMODEM -- but a
        // sender that uses it means the same thing, so accept it and move on.
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zmodem::escape::push_escaped;
    use crate::zmodem::frame::{encode, FrameKind};

    fn collect(parser: &mut Parser) -> Vec<Event> {
        let mut out = Vec::new();
        while let Some(event) = parser.next_event() {
            out.push(event);
        }
        out
    }

    fn kinds(events: &[Event]) -> Vec<FrameKind> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Header { header, .. } => Some(header.kind),
                _ => None,
            })
            .collect()
    }

    fn check_bytes(data: &[u8], end: Framend, width: Width) -> Vec<u8> {
        let mut plain = data.to_vec();
        plain.push(end.byte());
        match width {
            Width::Sixteen => {
                let mut crc = Crc16::new();
                crc.update(&plain);
                crc.bytes().to_vec()
            }
            Width::ThirtyTwo => {
                let mut crc = Crc32::new();
                crc.update(&plain);
                crc.bytes().to_vec()
            }
        }
    }

    /// A subpacket as it appears on the wire: escaped data, `ZDLE` + frameend,
    /// escaped check.
    fn subpacket(data: &[u8], end: Framend, width: Width) -> Vec<u8> {
        let mut out = Vec::new();
        push_escaped(&mut out, data);
        out.extend_from_slice(&[ZDLE, end.byte()]);
        push_escaped(&mut out, &check_bytes(data, end, width));
        out
    }

    fn decode_in(first: &[u8], second: &[u8]) -> Vec<Event> {
        let mut parser = Parser::new();
        parser.feed(first);
        let mut events = collect(&mut parser);
        parser.feed(second);
        events.extend(collect(&mut parser));
        events
    }

    #[test]
    fn a_header_survives_arriving_one_byte_at_a_time() {
        let mut parser = Parser::new();
        for byte in encode(Header::bare(FrameKind::RInit), Encoding::Hex) {
            parser.feed(&[byte]);
        }
        assert_eq!(kinds(&collect(&mut parser)), [FrameKind::RInit]);
    }

    /// The failure mode this module exists to avoid: a link that splits frames
    /// wherever it likes. Every split of the same stream must decode the same.
    #[test]
    fn every_split_of_a_stream_decodes_the_same() {
        let mut stream = encode(Header::bare(FrameKind::RqInit), Encoding::Hex);
        stream.extend_from_slice(b"noise from the shell\n");
        stream.extend(encode(
            Header::new(FrameKind::File, [0x01, 0x00, 0x00, 0x00]),
            Encoding::Bin32,
        ));
        stream.extend(subpacket(b"a.txt\0", Framend::Zcrcw, Width::ThirtyTwo));

        let whole = decode_in(&stream, &[]);
        assert!(matches!(whole.last(), Some(Event::Subpacket { .. })));

        for split in 1..stream.len() {
            let pieces = decode_in(&stream[..split], &stream[split..]);
            assert_eq!(pieces, whole, "split at {split} decoded differently");
        }
    }

    #[test]
    fn a_subpacket_carries_its_payload_and_its_expectation() {
        let mut parser = Parser::new();
        parser.feed(&encode(Header::bare(FrameKind::File), Encoding::Bin32));
        assert!(matches!(
            collect(&mut parser).first(),
            Some(Event::Header { .. })
        ));
        assert!(parser.collecting(), "a ZFILE is followed by a subpacket");

        parser.feed(&subpacket(b"a.txt\0", Framend::Zcrcw, Width::ThirtyTwo));
        assert_eq!(
            collect(&mut parser),
            [Event::Subpacket {
                data: b"a.txt\0".to_vec(),
                end: Framend::Zcrcw,
            }]
        );
        assert!(
            !parser.collecting(),
            "a ZCRCW ends the run, so a header comes next"
        );
    }

    #[test]
    fn a_continuing_frameend_keeps_collecting_with_no_header() {
        let mut parser = Parser::new();
        parser.feed(&encode(Header::bare(FrameKind::Data), Encoding::Bin32));
        collect(&mut parser);
        parser.feed(&subpacket(b"first", Framend::Zcrcg, Width::ThirtyTwo));
        parser.feed(&subpacket(b"second", Framend::Zcrce, Width::ThirtyTwo));

        let payloads: Vec<Vec<u8>> = collect(&mut parser)
            .into_iter()
            .filter_map(|event| match event {
                Event::Subpacket { data, .. } => Some(data),
                _ => None,
            })
            .collect();
        assert_eq!(payloads, [b"first".to_vec(), b"second".to_vec()]);
        assert!(!parser.collecting(), "a ZCRCE hands back to headers");
    }

    #[test]
    fn a_corrupt_header_is_reported_rather_than_guessed_at() {
        let mut parser = Parser::new();
        let mut encoded = encode(Header::bare(FrameKind::RInit), Encoding::Bin32);
        let last = encoded.len() - 1;
        encoded[last] ^= 0xFF;
        parser.feed(&encoded);
        assert_eq!(collect(&mut parser), [Event::Corrupt(Corruption::Header)]);
        assert_eq!(parser.stats().corrupt, 1);
    }

    #[test]
    fn a_corrupt_subpacket_is_reported_and_the_stream_resyncs() {
        let mut parser = Parser::new();
        parser.feed(&encode(Header::bare(FrameKind::File), Encoding::Bin32));
        collect(&mut parser);

        let mut broken = subpacket(b"a.txt\0", Framend::Zcrcw, Width::ThirtyTwo);
        let last = broken.len() - 1;
        broken[last] ^= 0xFF;
        parser.feed(&broken);
        assert_eq!(
            collect(&mut parser),
            [Event::Corrupt(Corruption::Subpacket)]
        );

        // The next header still lands: a bad check costs a frame, not the session.
        parser.feed(&encode(
            Header::new(FrameKind::File, [0x01, 0x00, 0x00, 0x00]),
            Encoding::Bin32,
        ));
        assert_eq!(kinds(&collect(&mut parser)), [FrameKind::File]);
    }

    #[test]
    fn the_wakeup_string_and_banner_text_are_dropped() {
        let banner = b"login as: root\r\nPassword: \r\n";
        let mut parser = Parser::new();
        parser.feed(b"rz\r");
        parser.feed(banner);
        parser.feed(&encode(Header::bare(FrameKind::RqInit), Encoding::Hex));
        assert_eq!(kinds(&collect(&mut parser)), [FrameKind::RqInit]);
        assert_eq!(
            parser.stats().discarded,
            (3 + banner.len() + 1) as u64,
            "the wakeup, the banner, and the extra synchronising pad"
        );
    }

    #[test]
    fn the_hex_tail_lrzs_actually_writes_is_accepted() {
        // lrzsz writes CR, then 0x8A where the spec says LF, then XON.
        let mut stream = b"**\x18B".to_vec();
        stream.extend_from_slice(b"0100000023be50");
        stream.extend_from_slice(&[0x0D, 0x8A, 0x11]);

        let mut parser = Parser::new();
        parser.feed(&stream);
        assert_eq!(kinds(&collect(&mut parser)), [FrameKind::RInit]);
        assert_eq!(parser.encoding(), Encoding::Hex);
        assert_eq!(parser.width(), Width::Sixteen);
    }

    /// A hex header can be followed by a subpacket, so the tail swallow must
    /// stop after three bytes rather than run on.
    #[test]
    fn the_hex_tail_swallow_does_not_eat_what_follows() {
        let mut stream = encode(Header::bare(FrameKind::Data), Encoding::Hex);
        stream.extend(subpacket(b"x", Framend::Zcrce, Width::Sixteen));

        let mut parser = Parser::new();
        parser.feed(&stream);
        let events = collect(&mut parser);
        assert_eq!(kinds(&events), [FrameKind::Data]);
        assert!(
            matches!(events.last(), Some(Event::Subpacket { .. })),
            "the subpacket after a hex header is parsed, not swallowed"
        );
    }

    #[test]
    fn a_binary_header_announces_the_session_check_width() {
        let mut parser = Parser::new();
        assert_eq!(parser.width(), Width::ThirtyTwo, "the modern default");

        // ZEOF rather than ZFILE on purpose: a ZFILE is followed by a subpacket,
        // so feeding one header after another would post the second one as data.
        parser.feed(&encode(Header::bare(FrameKind::Eof), Encoding::Bin16));
        collect(&mut parser);
        assert_eq!(parser.width(), Width::Sixteen);
        assert_eq!(parser.encoding(), Encoding::Bin16);

        parser.feed(&encode(Header::bare(FrameKind::Eof), Encoding::Bin32));
        collect(&mut parser);
        assert_eq!(parser.width(), Width::ThirtyTwo);
        assert_eq!(parser.encoding(), Encoding::Bin32);
    }

    /// A 16-bit session has to have its subpackets checked with the narrow
    /// check, which is what `sz -o` produces.
    #[test]
    fn the_narrow_check_is_used_when_the_sender_announced_it() {
        let mut parser = Parser::new();
        parser.feed(&encode(Header::bare(FrameKind::File), Encoding::Bin16));
        collect(&mut parser);
        parser.feed(&subpacket(b"a.txt\0", Framend::Zcrcw, Width::Sixteen));
        assert_eq!(
            collect(&mut parser),
            [Event::Subpacket {
                data: b"a.txt\0".to_vec(),
                end: Framend::Zcrcw,
            }]
        );
    }

    #[test]
    fn a_subpacket_that_never_ends_is_cut_off_instead_of_growing() {
        let mut parser = Parser::new();
        parser.feed(&encode(Header::bare(FrameKind::Data), Encoding::Bin32));
        collect(&mut parser);

        parser.feed(&vec![0x41u8; MAX_SUBPACKET + 100]);
        assert_eq!(collect(&mut parser), [Event::Corrupt(Corruption::Oversize)]);
        assert_eq!(parser.buffered(), 0, "the buffer is released");
        assert!(!parser.collecting(), "and the framer resyncs on a header");
    }

    #[test]
    fn a_stray_pad_is_kept_because_a_header_may_arrive_split() {
        let mut parser = Parser::new();
        parser.feed(b"trailing text*");
        assert_eq!(
            parser.buffered(),
            1,
            "only the potential marker start is kept"
        );

        let mut rest = vec![ZDLE, b'B'];
        rest.extend_from_slice(b"0800000000022d\r\n");
        parser.feed(&rest);
        assert_eq!(kinds(&collect(&mut parser)), [FrameKind::Fin]);
    }

    /// A check whose own bytes need escaping. The recorded 4112-byte session
    /// hits this on its very first subpacket, and reading the check raw instead
    /// of decoding it costs that frame — which is how this test came to exist.
    #[test]
    fn a_check_byte_that_needs_escaping_is_decoded_not_sliced() {
        use crate::zmodem::escape::needs_escape;

        // Find a payload whose 32-bit check contains a byte the escape layer
        // must touch. Searching rather than hardcoding keeps the test honest:
        // the interesting property is a property of the check, not of one
        // magic payload.
        let mut counter = 0u32;
        let (data, check) = loop {
            let data = counter.to_le_bytes();
            let check = check_bytes(&data, Framend::Zcrcw, Width::ThirtyTwo);
            if check.iter().any(|byte| needs_escape(*byte)) {
                break (data, check);
            }
            counter += 1;
            assert!(counter < 100_000, "some payload must escape its own check");
        };

        let framed = subpacket(&data, Framend::Zcrcw, Width::ThirtyTwo);
        assert!(
            framed.len() > data.len() + 2 + 4,
            "the check really did take more than four bytes on the wire"
        );

        let mut parser = Parser::new();
        parser.feed(&encode(Header::bare(FrameKind::File), Encoding::Bin32));
        collect(&mut parser);
        parser.feed(&framed);
        assert_eq!(
            collect(&mut parser),
            [Event::Subpacket {
                data: data.to_vec(),
                end: Framend::Zcrcw,
            }],
            "check {check:02x?} escaped but did not decode"
        );
        assert_eq!(parser.stats().corrupt, 0);
        assert_eq!(parser.buffered(), 0);
    }

    #[test]
    fn hex_digits_are_read_case_insensitively() {
        assert_eq!(hex_value(b'0'), Some(0));
        assert_eq!(hex_value(b'9'), Some(9));
        assert_eq!(hex_value(b'a'), Some(10));
        assert_eq!(hex_value(b'F'), Some(15));
        assert_eq!(hex_value(b'g'), None);
        assert_eq!(hex_value(b' '), None);
    }
}
