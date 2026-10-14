//! The two frame checks ZMODEM uses, with the byte orders lrzsz writes them in.
//!
//! They are genuinely different algorithms, not one algorithm at two widths:
//! the 16-bit check is CRC-16/XMODEM (poly `0x1021`, init 0, MSB-first) while
//! the 32-bit check is the reflected CRC-32 that `zlib` implements (init `~0`,
//! final xor `~0`). The reference implementation proves it — see the vectors in
//! the tests, which are headers lifted out of a recorded `sz`/`rz` session.
//!
//! Both are offered incrementally because a data subpacket's check has to
//! accumulate bytes that arrive in pieces: the payload, and then the frameend
//! byte, which is appended *after* the data has already gone past.

/// A running CRC-16/XMODEM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crc16 {
    state: u16,
}

impl Crc16 {
    pub fn new() -> Crc16 {
        Crc16 { state: 0 }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        let mut state = self.state;
        for &byte in bytes {
            state = (state << 8) ^ TABLE[(((state >> 8) ^ u16::from(byte)) & 0xff) as usize];
        }
        self.state = state;
    }

    pub fn value(&self) -> u16 {
        self.state
    }

    /// The two check bytes as they appear on the wire: most significant first.
    pub fn bytes(&self) -> [u8; 2] {
        self.state.to_be_bytes()
    }
}

impl Default for Crc16 {
    fn default() -> Crc16 {
        Crc16::new()
    }
}

/// A running CRC-32, matching `zlib::crc32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crc32 {
    state: u32,
}

impl Crc32 {
    pub fn new() -> Crc32 {
        Crc32 { state: 0xffff_ffff }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        let mut state = self.state;
        for &byte in bytes {
            state = (state >> 8) ^ TABLE32[((state ^ u32::from(byte)) & 0xff) as usize];
        }
        self.state = state;
    }

    pub fn value(&self) -> u32 {
        self.state ^ 0xffff_ffff
    }

    /// The four check bytes as lrzsz writes them: least significant first.
    ///
    /// The two checks disagree about byte order, and this is not a quirk we get
    /// to pick a side on — sending `0x8c92efbc` big-endian where lrzsz expects
    /// `bc ef 92 8c` fails a frame check on every packet.
    pub fn bytes(&self) -> [u8; 4] {
        self.value().to_le_bytes()
    }
}

impl Default for Crc32 {
    fn default() -> Crc32 {
        Crc32::new()
    }
}

/// One-shot CRC-16/XMODEM.
pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = Crc16::new();
    crc.update(bytes);
    crc.value()
}

/// One-shot CRC-32.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(bytes);
    crc.value()
}

/// Which frame check the current transfer uses. The sender announces it by
/// choosing `A` or `C` for its binary headers, so the receiver learns it from
/// the framing rather than from a separate handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Width {
    /// 16-bit, written big-endian.
    Sixteen,
    /// 32-bit, written little-endian. lrzsz's default whenever the receiver
    /// advertises `CANFC32`, which every modern implementation does.
    #[default]
    ThirtyTwo,
}

impl Width {
    pub fn bytes(self) -> usize {
        match self {
            Width::Sixteen => 2,
            Width::ThirtyTwo => 4,
        }
    }

    /// Check a header or subpacket against the bytes that followed it.
    pub fn check(self, accumulated: &[u8], wire: &[u8]) -> bool {
        match self {
            Width::Sixteen => {
                wire.len() == 2 && crc16(accumulated) == u16::from_be_bytes([wire[0], wire[1]])
            }
            Width::ThirtyTwo => {
                wire.len() == 4
                    && crc32(accumulated)
                        == u32::from_le_bytes([wire[0], wire[1], wire[2], wire[3]])
            }
        }
    }
}

const fn table16() -> [u16; 256] {
    let mut table = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut value = (i as u16) << 8;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 0x8000 != 0 {
                (value << 1) ^ 0x1021
            } else {
                value << 1
            };
            bit += 1;
        }
        table[i] = value;
        i += 1;
    }
    table
}

const fn table32() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut value = i as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 != 0 {
                (value >> 1) ^ 0xedb8_8320
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[i] = value;
        i += 1;
    }
    table
}

static TABLE: [u16; 256] = table16();
static TABLE32: [u32; 256] = table32();

#[cfg(test)]
mod tests {
    use super::*;

    /// The check values every catalogue agrees on, to prove the tables are the
    /// algorithms they claim to be before trusting them on ZMODEM traffic.
    #[test]
    fn the_catalogued_check_values_match() {
        assert_eq!(crc16(b"123456789"), 0x31c3, "CRC-16/XMODEM");
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926, "CRC-32/zlib");
    }

    /// Headers lifted out of a recorded session, verbatim. If the algorithm or
    /// the field order were wrong, none of these would land.
    #[test]
    fn checks_match_headers_a_real_sender_and_receiver_emitted() {
        // rz's ZRINIT, arrived as the hex digits `0100000023be50`.
        let zrinit = [0x01, 0x00, 0x00, 0x00, 0x23];
        assert_eq!(crc16(&zrinit), 0xbe50);

        // sz's ZRQINIT, arrived as fourteen zeros.
        assert_eq!(crc16(&[0x00; 5]), 0x0000);

        // rz's ZRPOS after the first subpacket, and its ZFIN.
        assert_eq!(crc16(&[0x09, 0x00, 0x00, 0x00, 0x00]), 0xa87c);
        assert_eq!(crc16(&[0x08, 0x00, 0x00, 0x00, 0x00]), 0x022d);

        // sz's ZFILE and ZDATA binary32 headers, whose checks arrived as the
        // little-endian byte runs `4b 61 a5 44` and `bc ef 92 8c`.
        assert_eq!(crc32(&[0x04, 0x00, 0x00, 0x00, 0x01]), 0x44a5_614b);
        assert_eq!(crc32(&[0x0a, 0x00, 0x00, 0x00, 0x00]), 0x8c92_efbc);
    }

    #[test]
    fn the_two_checks_are_written_in_opposite_byte_orders() {
        let mut sixteen = Crc16::new();
        sixteen.update(&[0x01, 0x00, 0x00, 0x00, 0x23]);
        assert_eq!(sixteen.bytes(), [0xbe, 0x50], "16-bit goes big-endian");

        let mut thirty_two = Crc32::new();
        thirty_two.update(&[0x0a, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(
            thirty_two.bytes(),
            [0xbc, 0xef, 0x92, 0x8c],
            "32-bit goes little-endian"
        );
    }

    /// A subpacket's check covers the payload *and* the frameend byte, so the
    /// incremental form has to survive being fed in two calls.
    #[test]
    fn an_incremental_check_matches_the_one_shot_form() {
        let data = b"payload.bin\0" as &[u8];
        let mut crc = Crc32::new();
        crc.update(data);
        crc.update(b"k");
        assert_eq!(crc.value(), crc32(b"payload.bin\0k"));

        // The recorded ZFILE name block checked to `bca84a64`, sent as
        // `64 4a a8 bc`.
        let mut block = Crc32::new();
        block.update(
            b"payload.bin\0\
              4112 15263574505 100664 0 1 4112\0",
        );
        block.update(b"k");
        assert_eq!(block.value(), 0xbca8_4a64);
        assert_eq!(block.bytes(), [0x64, 0x4a, 0xa8, 0xbc]);
    }

    #[test]
    fn width_check_rejects_a_mismatched_length_or_value() {
        let header = [0x01u8, 0x00, 0x00, 0x00, 0x23];
        assert!(Width::Sixteen.check(&header, &[0xbe, 0x50]));
        assert!(!Width::Sixteen.check(&header, &[0xbe, 0x51]));
        assert!(!Width::Sixteen.check(&header, &[0xbe]));
        assert!(!Width::Sixteen.check(&header, &[0xbe, 0x50, 0x00]));

        let header = [0x0au8, 0x00, 0x00, 0x00, 0x00];
        assert!(Width::ThirtyTwo.check(&header, &[0xbc, 0xef, 0x92, 0x8c]));
        assert!(!Width::ThirtyTwo.check(&header, &[0x8c, 0x92, 0xef, 0xbc]));
    }

    #[test]
    fn the_default_width_is_the_32_bit_one() {
        assert_eq!(Width::default(), Width::ThirtyTwo);
        assert_eq!(Width::Sixteen.bytes(), 2);
        assert_eq!(Width::ThirtyTwo.bytes(), 4);
    }
}
