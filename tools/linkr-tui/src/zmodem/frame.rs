//! Headers: the frame types, the three wire shapes, and the field reversal.
//!
//! A header always carries five logical bytes — a type and four flag/position
//! bytes — but the wire order is `TYPE F3 F2 F1 F0`, so `F0` arrives *last*.
//! [`Header::data`] is kept in the logical `F0..F3` order and the reversal lives
//! in [`Header::to_wire`] / [`Header::from_wire`], because a caller that asked
//! for "the flags byte" should not have to remember that it is the fourth one on
//! the wire. A round trip cannot catch a mistake here — swapping the ends still
//! round-trips — so the tests pin the order against headers lrzsz actually sent.
//!
//! The four bytes are read two ways depending on the frame: as a flag byte in
//! `F0`, or as a 32-bit position/length across `F3 F2 F1 F0` (which is to say
//! big-endian once the reversal is undone, `F0` being the low byte). A `ZEOF`
//! from a 4112-byte transfer arrived as `F3 F2 F1 F0 = 00 00 10 10`, which is
//! where the second reading comes from.

use super::crc::{Crc16, Crc32, Width};
use super::escape::{push_escaped, ZDLE, ZPAD};

/// The frame types ZMODEM defines, in the order of their wire values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// Sender's opening invitation. A receive program answers `RInit`.
    RqInit,
    /// Receiver's capabilities: flag byte, buffer size. Sent repeatedly until
    /// the sender answers, which is why a capture of a stalled session is full
    /// of them.
    RInit,
    /// Sender's attention string, and the `ESCCTL` capability reply.
    SInit,
    /// Acknowledges a frame that asked for one.
    Ack,
    /// A file follows: name and metadata in the subpacket, contents after.
    File,
    /// Receiver refuses this file (already present, or not wanted).
    Skip,
    /// The last header did not check out. This is the only retry mechanism.
    Nak,
    /// The sender is giving up mid-transfer.
    Abort,
    /// The sender is done; the receiver answers `Fin` in turn.
    Fin,
    /// Receiver asks the sender to resume at a byte offset — used on retry.
    RPos,
    /// A subpacket of file contents follows. Also the type used for the
    /// continuation subpackets of a bulk run, though lrzsz omits the header for
    /// those and relies on `Framend::Zcrcg`.
    Data,
    /// End of file, with the final length in the position field.
    Eof,
    /// A file-level error, e.g. no space.
    FErr,
    /// CRC request, or its answer.
    Crc,
    /// Obsolete challenge/response for access control; answered by refusing.
    Challenge,
    /// The sender acknowledges a completed file.
    Compl,
    /// Abort request, in either direction.
    Can,
    /// How much space is left, if the receiver knows.
    FreeCnt,
    /// Run a command on the far end. Not implemented here — see the module docs.
    Command,
    /// The far end wants something shown to the human rather than parsed.
    StdErr,
}

impl FrameKind {
    pub fn to_byte(self) -> u8 {
        match self {
            FrameKind::RqInit => 0x00,
            FrameKind::RInit => 0x01,
            FrameKind::SInit => 0x02,
            FrameKind::Ack => 0x03,
            FrameKind::File => 0x04,
            FrameKind::Skip => 0x05,
            FrameKind::Nak => 0x06,
            FrameKind::Abort => 0x07,
            FrameKind::Fin => 0x08,
            FrameKind::RPos => 0x09,
            FrameKind::Data => 0x0A,
            FrameKind::Eof => 0x0B,
            FrameKind::FErr => 0x0C,
            FrameKind::Crc => 0x0D,
            FrameKind::Challenge => 0x0E,
            FrameKind::Compl => 0x0F,
            FrameKind::Can => 0x10,
            FrameKind::FreeCnt => 0x11,
            FrameKind::Command => 0x12,
            FrameKind::StdErr => 0x13,
        }
    }

    pub fn from_byte(byte: u8) -> Option<FrameKind> {
        let kind = match byte {
            0x00 => FrameKind::RqInit,
            0x01 => FrameKind::RInit,
            0x02 => FrameKind::SInit,
            0x03 => FrameKind::Ack,
            0x04 => FrameKind::File,
            0x05 => FrameKind::Skip,
            0x06 => FrameKind::Nak,
            0x07 => FrameKind::Abort,
            0x08 => FrameKind::Fin,
            0x09 => FrameKind::RPos,
            0x0A => FrameKind::Data,
            0x0B => FrameKind::Eof,
            0x0C => FrameKind::FErr,
            0x0D => FrameKind::Crc,
            0x0E => FrameKind::Challenge,
            0x0F => FrameKind::Compl,
            0x10 => FrameKind::Can,
            0x11 => FrameKind::FreeCnt,
            0x12 => FrameKind::Command,
            0x13 => FrameKind::StdErr,
            _ => return None,
        };
        Some(kind)
    }

    /// The name lrzsz uses, for logs and for the diagnostics pane.
    pub fn name(self) -> &'static str {
        match self {
            FrameKind::RqInit => "ZRQINIT",
            FrameKind::RInit => "ZRINIT",
            FrameKind::SInit => "ZSINIT",
            FrameKind::Ack => "ZACK",
            FrameKind::File => "ZFILE",
            FrameKind::Skip => "ZSKIP",
            FrameKind::Nak => "ZNAK",
            FrameKind::Abort => "ZABORT",
            FrameKind::Fin => "ZFIN",
            FrameKind::RPos => "ZRPOS",
            FrameKind::Data => "ZDATA",
            FrameKind::Eof => "ZEOF",
            FrameKind::FErr => "ZFERR",
            FrameKind::Crc => "ZCRC",
            FrameKind::Challenge => "ZCHALLENGE",
            FrameKind::Compl => "ZCOMPL",
            FrameKind::Can => "ZCAN",
            FrameKind::FreeCnt => "ZFREECNT",
            FrameKind::Command => "ZCOMMAND",
            FrameKind::StdErr => "ZSTDERR",
        }
    }

    /// Whether a data subpacket follows this header immediately.
    ///
    /// The parser uses this to know when to stop looking for headers and start
    /// collecting data. Getting it wrong in the permissive direction means
    /// swallowing the next header as data, so the list is deliberately short:
    /// these are the three frames the reference implementation follows with a
    /// subpacket.
    pub fn carries_subpacket(self) -> bool {
        matches!(self, FrameKind::File | FrameKind::Data | FrameKind::SInit)
    }
}

/// The three ways a header can be written, which is also how the sender tells
/// the receiver which frame check the following subpackets will use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// `* * ZDLE B` plus 14 lowercase hex digits. Used for anything not followed
    /// by data, and always for the receiver's own replies. 16-bit checks.
    Hex,
    /// `* ZDLE A` plus five bytes plus a 16-bit check. What `sz -o` uses.
    Bin16,
    /// `* ZDLE C` plus five bytes plus a 32-bit check. lrzsz's default, and the
    /// form a modern receiver asks for by advertising `CANFC32`.
    Bin32,
}

impl Encoding {
    /// The check width this encoding implies for the subpackets that follow.
    pub fn check(self) -> Width {
        match self {
            Encoding::Hex | Encoding::Bin16 => Width::Sixteen,
            Encoding::Bin32 => Width::ThirtyTwo,
        }
    }

    /// The character after `ZDLE`, or `None` for the hex form.
    pub fn marker(self) -> Option<u8> {
        match self {
            Encoding::Hex => None,
            Encoding::Bin16 => Some(b'A'),
            Encoding::Bin32 => Some(b'C'),
        }
    }
}

/// A frame type plus its four flag bytes, in logical `F0..F3` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: FrameKind,
    pub data: [u8; 4],
}

impl Header {
    pub fn new(kind: FrameKind, data: [u8; 4]) -> Header {
        Header { kind, data }
    }

    /// A header with no flag bytes set, which is what most of them are.
    pub fn bare(kind: FrameKind) -> Header {
        Header { kind, data: [0; 4] }
    }

    pub fn f0(&self) -> u8 {
        self.data[0]
    }

    pub fn f1(&self) -> u8 {
        self.data[1]
    }

    pub fn f2(&self) -> u8 {
        self.data[2]
    }

    pub fn f3(&self) -> u8 {
        self.data[3]
    }

    /// The four bytes read as the 32-bit value the protocol stores there.
    ///
    /// The value travels little-endian — its low byte goes first — and the
    /// transmitted order is `F3 F2 F1 F0`, so this is the transmitted bytes read
    /// little-endian, or equivalently `data` read big-endian.
    ///
    /// This is the file offset in `ZRPOS`/`ZDATA` and the final size in `ZEOF`.
    /// A 4112-byte transfer ended with a `ZEOF` whose data bytes were
    /// `10 10 00 00`, and 0x1010 is 4112. Reading that as big-endian gives
    /// 269484032, which is how the byte order here was found out.
    pub fn position(&self) -> u32 {
        u32::from_le_bytes([self.f3(), self.f2(), self.f1(), self.f0()])
    }

    pub fn with_position(self, position: u32) -> Header {
        // `to_le_bytes` is exactly the transmitted order, which is the reverse
        // of `data`.
        let [f3, f2, f1, f0] = position.to_le_bytes();
        Header {
            kind: self.kind,
            data: [f0, f1, f2, f3],
        }
    }

    /// The five bytes as they travel: `TYPE F3 F2 F1 F0`.
    pub fn to_wire(&self) -> [u8; 5] {
        [
            self.kind.to_byte(),
            self.f3(),
            self.f2(),
            self.f1(),
            self.f0(),
        ]
    }

    /// Undo [`Header::to_wire`]. `None` if the type byte is not one ZMODEM
    /// defines, which is how a false sync is rejected before the check even runs.
    pub fn from_wire(wire: [u8; 5]) -> Option<Header> {
        Some(Header {
            kind: FrameKind::from_byte(wire[0])?,
            data: [wire[4], wire[3], wire[2], wire[1]],
        })
    }
}

/// The largest file the protocol can describe.
///
/// Every position on the wire — `ZRPOS`'s request, `ZEOF`'s final length — is a
/// four-byte field, so a longer file cannot be named at all. A sender that
/// truncated the field would have its claim believed, which is worse than
/// refusing to start.
pub const MAX_FILE: u64 = u32::MAX as u64;

/// The receiver's capability bits, which live in `F0` of a `ZRINIT`.
///
/// Only the ones that change behaviour are named. `CANFC32` is the load-bearing
/// one: without it the sender must fall back to 16-bit checks, and lrzsz honours
/// that, which is exactly how the `crc16` recording was produced.
pub mod cap {
    /// Full duplex: the receiver can send while it receives.
    pub const CANFDX: u8 = 0x01;
    /// The receiver can overlap I/O.
    pub const CANOVIO: u8 = 0x02;
    /// The receiver can send a break.
    pub const CANBRK: u8 = 0x04;
    /// The receiver can do 32-bit frame checks. Advertised by every modern
    /// implementation, and by ours, since there is no reason to prefer the
    /// weaker check.
    pub const CANFC32: u8 = 0x20;
    /// Control characters in the data will be escaped.
    pub const ESCCTL: u8 = 0x40;
    /// The receiver is using 8-bit data.
    pub const ESC8: u8 = 0x80;
}

/// Encode a header, ready to write to the link.
///
/// The hex tail is `CR LF` plus an `XON` that releases the sender from a
/// spurious `XOFF` picked up as line noise — except after `ZACK` and `ZFIN`,
/// where the `XON` would interfere with flow control or with clean session
/// teardown. lrzsz writes `0x8A` where the `LF` belongs; we write the `LF`, which
/// a real `sz` accepts.
pub fn encode(header: Header, encoding: Encoding) -> Vec<u8> {
    let wire = header.to_wire();
    let mut out = Vec::with_capacity(24);
    match encoding {
        Encoding::Hex => {
            let mut crc = Crc16::new();
            crc.update(&wire);
            out.extend_from_slice(&[ZPAD, ZPAD, ZDLE, b'B']);
            for byte in wire.iter().chain(crc.bytes().iter()) {
                out.extend_from_slice(format!("{byte:02x}").as_bytes());
            }
            out.extend_from_slice(b"\r\n");
            if !matches!(header.kind, FrameKind::Ack | FrameKind::Fin) {
                out.push(0x11);
            }
        }
        Encoding::Bin16 => {
            let mut crc = Crc16::new();
            crc.update(&wire);
            out.extend_from_slice(&[ZPAD, ZDLE, b'A']);
            push_escaped(&mut out, &wire);
            push_escaped(&mut out, &crc.bytes());
        }
        Encoding::Bin32 => {
            let mut crc = Crc32::new();
            crc.update(&wire);
            out.extend_from_slice(&[ZPAD, ZDLE, b'C']);
            push_escaped(&mut out, &wire);
            push_escaped(&mut out, &crc.bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_frame_type_survives_a_round_trip() {
        for byte in 0u8..0x14 {
            let kind = FrameKind::from_byte(byte).expect("0x00..0x13 are all defined");
            assert_eq!(kind.to_byte(), byte);
            assert!(!kind.name().is_empty());
        }
        assert!(FrameKind::from_byte(0x14).is_none());
        assert!(FrameKind::from_byte(0xff).is_none());
    }

    /// The field reversal, pinned against headers recorded from lrzsz. A round
    /// trip through `to_wire`/`from_wire` would pass even if the reversal were
    /// dropped, so these go through fixed wire bytes instead.
    #[test]
    fn the_wire_order_is_the_reverse_of_the_logical_order() {
        // rz's ZRINIT arrived as the hex digits `0100000023be50`.
        let zrinit = Header::from_wire([0x01, 0x00, 0x00, 0x00, 0x23]).unwrap();
        assert_eq!(zrinit.kind, FrameKind::RInit);
        assert_eq!(zrinit.f0(), 0x23, "the flags are the byte sent last");
        assert_eq!(
            zrinit.f0() & cap::CANFC32,
            cap::CANFC32,
            "rz advertised 32-bit checks"
        );
        assert_eq!(zrinit.data, [0x23, 0x00, 0x00, 0x00]);

        // sz's ZFILE arrived as `0400000001` after its `* ZDLE C` marker.
        let zfile = Header::from_wire([0x04, 0x00, 0x00, 0x00, 0x01]).unwrap();
        assert_eq!(zfile.kind, FrameKind::File);
        assert_eq!(zfile.f0(), 0x01, "the conversion option, ZCBIN");
        assert_eq!(zfile.to_wire(), [0x04, 0x00, 0x00, 0x00, 0x01]);
    }

    #[test]
    fn the_position_is_little_endian_across_the_reversed_field() {
        // A real ZEOF for a 4112-byte file: the data bytes travelled
        // `10 10 00 00`, low byte first.
        let eof = Header::from_wire([0x0b, 0x10, 0x10, 0x00, 0x00]).unwrap();
        assert_eq!(eof.position(), 4112);
        assert_eq!(
            eof.with_position(4112).to_wire(),
            [0x0b, 0x10, 0x10, 0x00, 0x00]
        );

        // A ZDATA header starting at offset 0.
        let data = Header::from_wire([0x0a, 0x00, 0x00, 0x00, 0x00]).unwrap();
        assert_eq!(data.position(), 0);
        assert_eq!(
            data.with_position(0xdead_beef).to_wire()[1..],
            [0xef, 0xbe, 0xad, 0xde],
            "the transmitted order is the value in little-endian"
        );

        // A real ZRPOS from rz asked to restart at zero.
        let rpos = Header::from_wire([0x09, 0x00, 0x00, 0x00, 0x00]).unwrap();
        assert_eq!(rpos.position(), 0);
    }

    #[test]
    fn only_three_frames_are_followed_by_a_subpacket() {
        let carriers: Vec<&str> = (0u8..0x14)
            .map(|b| FrameKind::from_byte(b).unwrap())
            .filter(|k| k.carries_subpacket())
            .map(|k| k.name())
            .collect();
        assert_eq!(carriers, ["ZSINIT", "ZFILE", "ZDATA"]);
    }

    #[test]
    fn the_encoding_announces_the_check_width() {
        assert_eq!(Encoding::Hex.check(), Width::Sixteen);
        assert_eq!(Encoding::Bin16.check(), Width::Sixteen);
        assert_eq!(Encoding::Bin32.check(), Width::ThirtyTwo);
        assert_eq!(Encoding::Hex.marker(), None);
        assert_eq!(Encoding::Bin16.marker(), Some(b'A'));
        assert_eq!(Encoding::Bin32.marker(), Some(b'C'));
    }

    /// Byte-for-byte against the recording. rz opened with this exact run.
    #[test]
    fn a_hex_header_encodes_exactly_as_the_recording_shows() {
        let zrinit = Header::new(FrameKind::RInit, [0x23, 0x00, 0x00, 0x00]);
        let encoded = encode(zrinit, Encoding::Hex);
        let mut expected = b"**\x18B".to_vec();
        expected.extend_from_slice(b"0100000023be50");
        expected.extend_from_slice(b"\r\n\x11");
        assert_eq!(encoded, expected);

        // ZFIN is the exception: no XON, because the session is ending.
        let fin = Header::bare(FrameKind::Fin);
        let encoded = encode(fin, Encoding::Hex);
        let mut expected = b"**\x18B".to_vec();
        expected.extend_from_slice(b"0800000000022d");
        expected.extend_from_slice(b"\r\n");
        assert_eq!(encoded, expected);
    }

    #[test]
    fn a_binary32_header_encodes_exactly_as_the_recording_shows() {
        let zfile = Header::new(FrameKind::File, [0x01, 0x00, 0x00, 0x00]);
        let encoded = encode(zfile, Encoding::Bin32);
        assert_eq!(
            encoded,
            [
                0x2A, 0x18, 0x43, // ZPAD ZDLE 'C'
                0x04, 0x00, 0x00, 0x00, 0x01, // TYPE F3 F2 F1 F0
                0x4B, 0x61, 0xA5, 0x44, // the check, little-endian
            ]
        );

        // The ZEOF from the same session carried 4112 as `10 10 00 00`, and
        // both of the 0x10 data bytes need escaping.
        let eof = Header::bare(FrameKind::Eof).with_position(4112);
        let encoded = encode(eof, Encoding::Bin32);
        assert_eq!(
            encoded[..9],
            [0x2A, 0x18, 0x43, 0x0B, 0x18, 0x50, 0x18, 0x50, 0x00],
            "the two 0x10 data bytes arrive escaped as ZDLE 0x50"
        );
    }

    #[test]
    fn a_binary16_header_uses_the_two_byte_check() {
        let zfile = Header::new(FrameKind::File, [0x01, 0x00, 0x00, 0x00]);
        let encoded = encode(zfile, Encoding::Bin16);
        assert_eq!(&encoded[..3], &[0x2A, 0x18, 0x41], "ZBIN is 'A'");
        assert_eq!(encoded.len(), 3 + 5 + 2);

        let mut crc = Crc16::new();
        crc.update(&[0x04, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&encoded[8..], &crc.bytes());
    }
}
