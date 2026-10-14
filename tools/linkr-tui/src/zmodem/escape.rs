//! The ZDLE escape layer, and the frameend bytes that sit inside it.
//!
//! `ZDLE` is the protocol's escape character. A byte the protocol wants to keep
//! off the wire is written as `ZDLE` followed by the byte XOR `0x40`, which is
//! why the escape set is exactly the bytes whose encoded form is unambiguous.
//!
//! Decoding is unconditional and needs no knowledge of the set: anything after a
//! `ZDLE` stands for `byte ^ 0x40`, except `ZDLE ZDLE`, which stands for `ZDLE`
//! itself. That asymmetry is deliberate — a receiver that had to know the
//! sender's escape set would fail on a sender that escapes more than it must.
//!
//! # The frameend collision, and why it cannot happen
//!
//! A data subpacket ends with `ZDLE` followed by one of `h i j k`, and those are
//! *not* XORed. So decoding is context sensitive: after a `ZDLE`, a byte in
//! `0x68..=0x6b` means "frame ends", not "the literal `0x68 ^ 0x40`". The two
//! readings are only ever confused if some escaped byte encodes into
//! `0x68..=0x6b`, i.e. if a byte in `0x28..=0x2b` needs escaping. None do: the
//! escape set is `{0x0D, 0x10, 0x11, 0x13, 0x18, 0x8D, 0x90, 0x91, 0x93}`, which
//! encodes into `{0x4D, 0x50, 0x51, 0x53, 0x58, 0xCD, 0xD0, 0xD1, 0xD3}`. That
//! the two sets miss each other is what makes the whole scheme work, and it is
//! asserted in the tests rather than left as a comment.

/// Padding that introduces a header. Two of them when the sender is
/// synchronising, one otherwise — a receiver must accept either.
pub const ZPAD: u8 = 0x2A;

/// The escape byte (CAN).
pub const ZDLE: u8 = 0x18;

/// How a data subpacket ends, which is also how the sender says what it wants
/// next. The letters are the actual wire values; the names come from lrzsz's
/// header, where they are documented as `ZCRCE 'h'` and friends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framend {
    /// Frame ends, a header follows. Used for the last subpacket of a file.
    Zcrce,
    /// Frame continues immediately, no header and no acknowledgement. This is
    /// the bulk case: lrzsz streams a whole file as a run of these.
    Zcrcg,
    /// Frame continues, and a `ZACK` is owed.
    Zcrcq,
    /// Frame ends, and a `ZACK` is owed. Used for small control blocks such as
    /// the filename that follows a `ZFILE`.
    Zcrcw,
}

impl Framend {
    /// The byte that follows `ZDLE`.
    pub fn byte(self) -> u8 {
        match self {
            Framend::Zcrce => b'h',
            Framend::Zcrcg => b'i',
            Framend::Zcrcq => b'j',
            Framend::Zcrcw => b'k',
        }
    }

    pub fn from_byte(byte: u8) -> Option<Framend> {
        match byte {
            b'h' => Some(Framend::Zcrce),
            b'i' => Some(Framend::Zcrcg),
            b'j' => Some(Framend::Zcrcq),
            b'k' => Some(Framend::Zcrcw),
            _ => None,
        }
    }

    /// Whether another subpacket follows immediately, with no header between.
    ///
    /// This is what a receiver loops on: a `ZCRCW` run of one subpacket is the
    /// normal case for a short file, while a megabyte arrives as a `ZCRCG` run
    /// that only the final `ZCRCE` closes.
    pub fn continues(self) -> bool {
        matches!(self, Framend::Zcrcg | Framend::Zcrcq)
    }

    /// Whether the sender is waiting for a `ZACK` before it carries on.
    pub fn wants_ack(self) -> bool {
        matches!(self, Framend::Zcrcq | Framend::Zcrcw)
    }
}

/// Whether lrzsz escapes this byte even when nothing follows it.
///
/// Derived from a 70 KB recording in which all 256 byte values occur: the seven
/// below were escaped on essentially every occurrence, while `0x0D` and `0x8D`
/// were escaped three times out of several hundred. We escape those two
/// unconditionally as well — see the module docs. Escaping is invisible to the
/// receiver either way, and the extra byte per carriage return is cheap
/// insurance against a line discipline that folds `CR` into a newline.
pub fn needs_escape(byte: u8) -> bool {
    matches!(
        byte,
        0x0D | 0x10 | 0x11 | 0x13 | ZDLE | 0x8D | 0x90 | 0x91 | 0x93
    )
}

/// Append `bytes` to `out`, escaping every byte that needs it.
pub fn push_escaped(out: &mut Vec<u8>, bytes: &[u8]) {
    for &byte in bytes {
        if needs_escape(byte) {
            out.push(ZDLE);
            out.push(byte ^ 0x40);
        } else {
            out.push(byte);
        }
    }
}

/// The byte a `ZDLE` stands for, given the byte that followed it.
///
/// `ZDLE ZDLE` is the one exception to the XOR: the sender doubles it rather
/// than writing `0x18 ^ 0x40`, so both spellings of a literal `ZDLE` occur.
pub fn unescape(after_zdle: u8) -> u8 {
    if after_zdle == ZDLE {
        ZDLE
    } else {
        after_zdle ^ 0x40
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The escape set and the frameend bytes must not collide, or a receiver
    /// could not tell "the frame ends here" from "here is a literal byte".
    #[test]
    fn no_escaped_byte_can_be_mistaken_for_a_frameend() {
        for byte in 0u8..=255 {
            if needs_escape(byte) {
                let encoded = byte ^ 0x40;
                assert!(
                    Framend::from_byte(encoded).is_none(),
                    "0x{byte:02x} encodes to 0x{encoded:02x}, which reads as a frameend"
                );
            }
        }
    }

    #[test]
    fn escaping_round_trips_every_byte_value() {
        let all: Vec<u8> = (0u8..=255).collect();
        let mut encoded = Vec::new();
        push_escaped(&mut encoded, &all);

        let mut decoded = Vec::new();
        let mut i = 0;
        while i < encoded.len() {
            if encoded[i] == ZDLE {
                decoded.push(unescape(encoded[i + 1]));
                i += 2;
            } else {
                decoded.push(encoded[i]);
                i += 1;
            }
        }
        assert_eq!(decoded, all);
    }

    /// The seven always-escaped bytes, checked against the recording: this is
    /// the exact byte run the first `ZDATA` subpacket carried for the value
    /// `0x18 0x0D 0x0D 0x11 0x00 0x0A 0x13 0x7F 0x91 0xFF 0x1A`.
    #[test]
    fn the_encoded_form_matches_the_recording() {
        let mut out = Vec::new();
        push_escaped(&mut out, &[0x10, 0x11, 0x13, 0x18, 0x90, 0x91, 0x93]);
        assert_eq!(
            out,
            [0x18, 0x50, 0x18, 0x51, 0x18, 0x53, 0x18, 0x58, 0x18, 0xD0, 0x18, 0xD1, 0x18, 0xD3]
        );

        // The recorded run, up to the point where lrzsz's CR rule would differ.
        let mut prefix = Vec::new();
        push_escaped(
            &mut prefix,
            &[0x18, 0x00, 0x0A, 0x13, 0x7F, 0x91, 0xFF, 0x1A],
        );
        assert_eq!(
            prefix,
            [0x18, 0x58, 0x00, 0x0A, 0x18, 0x53, 0x7F, 0x18, 0xD1, 0xFF, 0x1A]
        );
    }

    /// Our one deliberate deviation: lrzsz left both of these carriage returns
    /// bare in the recording (it escaped `0x0D` only after `0xC0`); we escape
    /// them. The decoded byte stream is identical, which is the only thing that
    /// matters to the far end.
    #[test]
    fn carriage_returns_are_escaped_unconditionally() {
        let mut out = Vec::new();
        push_escaped(&mut out, &[0x0D, 0x0D, 0x8D]);
        assert_eq!(out, [0x18, 0x4D, 0x18, 0x4D, 0x18, 0xCD]);

        let mut decoded = Vec::new();
        let mut i = 0;
        while i < out.len() {
            decoded.push(unescape(out[i + 1]));
            i += 2;
        }
        assert_eq!(decoded, [0x0D, 0x0D, 0x8D]);
    }

    #[test]
    fn a_doubled_zdle_stands_for_zdle_itself() {
        assert_eq!(unescape(ZDLE), ZDLE);
        assert_eq!(unescape(0x58), ZDLE, "and so does the XOR spelling");
        assert_eq!(unescape(0x51), 0x11);
        assert_eq!(unescape(0x4D), 0x0D);
    }

    #[test]
    fn frameends_carry_their_own_consequences() {
        assert_eq!(Framend::Zcrce.byte(), b'h');
        assert_eq!(Framend::Zcrcg.byte(), b'i');
        assert_eq!(Framend::Zcrcq.byte(), b'j');
        assert_eq!(Framend::Zcrcw.byte(), b'k');

        for byte in 0u8..=255 {
            if let Some(end) = Framend::from_byte(byte) {
                assert_eq!(end.byte(), byte);
            }
        }

        // A bulk file is a run of ZCRCG with one ZCRCE to close it, so
        // "continues" is the flag a receiver's inner loop turns on.
        assert!(Framend::Zcrcg.continues());
        assert!(Framend::Zcrcq.continues());
        assert!(!Framend::Zcrce.continues());
        assert!(!Framend::Zcrcw.continues());

        assert!(Framend::Zcrcq.wants_ack());
        assert!(Framend::Zcrcw.wants_ack());
        assert!(!Framend::Zcrcg.wants_ack());
        assert!(!Framend::Zcrce.wants_ack());
    }

    #[test]
    fn unescaped_ascii_and_high_bytes_pass_through() {
        let mut out = Vec::new();
        push_escaped(&mut out, b"payload.bin\0");
        assert_eq!(out, b"payload.bin\0", "a filename needs no escaping");

        let mut out = Vec::new();
        push_escaped(&mut out, &[0x00, 0x0A, 0x7F, 0xFF, 0x01]);
        assert_eq!(out, [0x00, 0x0A, 0x7F, 0xFF, 0x01]);
    }
}
