//! Subpackets: the payload carrier, and the file description that rides in the
//! first one.
//!
//! A `ZFILE` header is followed by one subpacket whose *contents* are plain
//! text — the file's name and metadata — and then by the file itself in
//! `ZDATA` subpackets. This module owns both the framing (data, `ZDLE` +
//! frameend, check) and the text block, because the block is the only part of
//! the protocol with a grammar worth parsing carefully.
//!
//! # The control block
//!
//! ```text
//! name NUL length SP mtime SP mode SP serial SP remaining NUL
//! ```
//!
//! `length` and `remaining` are decimal; `mtime`, `mode` and `serial` are
//! octal. The octal fields are the trap: lrzsz sent
//! `4112 15263574505 100664 0 1 4112` for a 4112-byte file, and reading
//! `15263574505` as decimal gives a date in 2453 while reading it as octal gives
//! 1791949125 — a real timestamp in this project's own lifetime. `100664` only
//! makes sense as octal S_IFREG with mode 644.
//!
//! The name is kept as bytes rather than a `String`. It comes from the far end
//! and ends up as a path on this machine, so a name that is not valid UTF-8 must
//! survive rather than be mangled into the replacement character and written to
//! disk under a different name than the sender asked for.

use super::crc::{Crc16, Crc32, Width};
use super::escape::{push_escaped, Framend, ZDLE};

/// A file the far end is offering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    name: Vec<u8>,
    /// Size in bytes.
    pub length: u64,
    /// Modification time, seconds since the epoch.
    pub modified: u64,
    /// Unix mode bits, including the file-type bits.
    pub mode: u32,
    /// The transfer serial number, used to detect a resumed session.
    pub serial: u32,
    /// How many files are still to come after this one.
    pub remaining: u32,
}

/// The file-type mask and the two types worth distinguishing.
const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;

impl FileInfo {
    pub fn new(name: impl Into<Vec<u8>>, length: u64) -> FileInfo {
        FileInfo {
            name: name.into(),
            length,
            modified: 0,
            // A regular file with the usual permissions, which is what an
            // upload almost always is.
            mode: S_IFREG | 0o644,
            serial: 0,
            remaining: 0,
        }
    }

    /// The raw name, as the sender wrote it.
    pub fn name_bytes(&self) -> &[u8] {
        &self.name
    }

    /// The name for display. Lossy on purpose: a name that is not UTF-8 is
    /// still worth showing, and the writable form is [`FileInfo::name_bytes`].
    pub fn name(&self) -> String {
        String::from_utf8_lossy(&self.name).into_owned()
    }

    pub fn is_regular(&self) -> bool {
        self.mode & S_IFMT == S_IFREG
    }

    pub fn is_directory(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }

    /// Parse the control block. `None` when it is not the shape the protocol
    /// describes — a truncated block, or text that is not one at all.
    ///
    /// The name and the length are required; the fields after them may be
    /// omitted, which is how lrzsz talks to peers that do not want them. An
    /// omitted trailing field is zero.
    pub fn parse(block: &[u8]) -> Option<FileInfo> {
        let nul = block.iter().position(|&byte| byte == 0)?;
        let name = block[..nul].to_vec();
        if name.is_empty() {
            return None;
        }

        let rest = &block[nul + 1..];
        let end = rest
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(rest.len());
        let mut fields = rest[..end]
            .split(|&byte| byte == b' ')
            .filter(|field| !field.is_empty());

        // A trailing field that is absent means zero, not an error: lrzsz omits
        // the fields after the length for peers that do not want them. The
        // length itself is not optional — a block without one is not this
        // protocol's block, and defaulting it to zero would silently offer an
        // empty file.
        let optional = |field: Option<&[u8]>, parse: fn(&[u8]) -> Option<u64>| -> Option<u64> {
            match field {
                Some(field) => parse(field),
                None => Some(0),
            }
        };
        let length = parse_decimal(fields.next()?)?;
        let modified = optional(fields.next(), parse_octal)?;
        let mode = optional(fields.next(), parse_octal)?;
        let serial = optional(fields.next(), parse_octal)?;
        let remaining = optional(fields.next(), parse_decimal)?;

        Some(FileInfo {
            name,
            length,
            modified,
            mode: mode as u32,
            serial: serial as u32,
            remaining: remaining as u32,
        })
    }

    /// Build the control block, terminated the way the protocol expects.
    ///
    /// lrzsz writes one field more than this: a repeat of the length, so its
    /// block for a 4112-byte file ends `… 1 4112 NUL` while this one ends `… 1
    /// NUL`. A reader stops at the terminating NUL and the trailing fields are
    /// optional, so the shorter block is accepted — which was checked against a
    /// real `rz` rather than inferred, because a sender's output is only as good
    /// as a receiver's willingness to read it. See `dev/zmodem-send-check.py`.
    pub fn to_block(&self) -> Vec<u8> {
        let mut out = self.name.clone();
        out.push(0);
        out.extend_from_slice(
            format!(
                "{} {:o} {:o} {:o} {}",
                self.length, self.modified, self.mode, self.serial, self.remaining
            )
            .as_bytes(),
        );
        out.push(0);
        out
    }
}

fn parse_decimal(field: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(field).ok()?;
    text.parse().ok()
}

fn parse_octal(field: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(field).ok()?;
    u64::from_str_radix(text, 8).ok()
}

/// Frame a payload as a subpacket, ready to write to the link.
///
/// The check accumulates the payload *and the frameend byte*, which is why the
/// frameend cannot be chosen after the check is computed.
pub fn encode(data: &[u8], end: Framend, width: Width) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 8);
    push_escaped(&mut out, data);
    out.extend_from_slice(&[ZDLE, end.byte()]);
    let mut plain = Vec::with_capacity(data.len() + 1);
    plain.extend_from_slice(data);
    plain.push(end.byte());
    match width {
        Width::Sixteen => {
            let mut crc = Crc16::new();
            crc.update(&plain);
            push_escaped(&mut out, &crc.bytes());
        }
        Width::ThirtyTwo => {
            let mut crc = Crc32::new();
            crc.update(&plain);
            push_escaped(&mut out, &crc.bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recorded_control_block_parses_field_by_field() {
        // Exactly the 45 bytes a real `sz -b` put in the subpacket after its
        // ZFILE header, for a 4112-byte payload.
        let block = b"payload.bin\0\
                      4112 15263574505 100664 0 1 4112\0";
        let info = FileInfo::parse(block).expect("a real block");

        assert_eq!(info.name(), "payload.bin");
        assert_eq!(info.name_bytes(), b"payload.bin");
        assert_eq!(info.length, 4112);
        assert!(
            info.is_regular(),
            "the mode's file-type bits say regular file"
        );
        assert!(!info.is_directory());
        assert_eq!(info.mode & 0o777, 0o664);
        assert_eq!(info.serial, 0);
        assert_eq!(info.remaining, 1);
    }

    /// The octal reading is the one that is checked, because the decimal reading
    /// is also a valid integer and a test that only asserted "it parsed" would
    /// pass with the wrong base.
    #[test]
    fn the_octal_fields_are_read_as_octal() {
        let info = FileInfo::parse(b"f\0 4112 15263574505 100664 0 1 1\0").unwrap();
        assert_eq!(
            info.modified, 0o15263574505,
            "15263574505 is octal, i.e. 1791949125 seconds"
        );
        assert_eq!(info.modified, 1_791_949_125);
        assert_eq!(info.mode, 0o100664);

        // If it had been parsed as decimal, the timestamp would be absurd.
        assert!(info.modified < 4_000_000_000, "a plausible epoch second");
    }

    #[test]
    fn a_block_round_trips_through_its_wire_form() {
        let mut info = FileInfo::new(b"notes.txt".to_vec(), 4096);
        info.modified = 1_791_949_125;
        info.mode = S_IFREG | 0o644;
        info.remaining = 0;

        let block = info.to_block();
        assert_eq!(
            block,
            b"notes.txt\0\
              4096 15263574505 100644 0 0\0"
                .to_vec()
        );
        assert_eq!(FileInfo::parse(&block).unwrap(), info);
    }

    #[test]
    fn a_name_that_is_not_utf8_survives_being_parsed() {
        let mut block = vec![0xFF, 0xFE, b'x', 0];
        block.extend_from_slice(b"1 0 100644 0 0\0");
        let info = FileInfo::parse(&block).unwrap();
        assert_eq!(info.name_bytes(), &[0xFF, 0xFE, b'x']);
        assert_eq!(info.length, 1, "the fields after the name still parse");
    }

    #[test]
    fn a_truncated_or_alien_block_is_refused_rather_than_guessed_at() {
        assert!(FileInfo::parse(b"").is_none(), "no name terminator");
        assert!(FileInfo::parse(b"\0").is_none(), "an empty name");
        assert!(
            FileInfo::parse(b"name-only\0").is_none(),
            "a name with no length field at all"
        );

        // A name and a length is enough: lrzsz omits trailing fields for peers
        // that do not want them.
        let info = FileInfo::parse(b"name-only\0 12\0").unwrap();
        assert_eq!(info.length, 12);
        assert_eq!(info.modified, 0);
        assert_eq!(info.remaining, 0);

        // Something that is plainly not a number is not silently coerced to one.
        assert!(FileInfo::parse(b"f\0 abcdef\0").is_none());
        assert!(FileInfo::parse(b"f\0 12 zz 100644 0 0\0").is_none());
    }

    #[test]
    fn a_directory_is_recognised_so_it_can_be_refused() {
        let info = FileInfo::parse(b"subdir\0 0 0 40755 0 0\0").unwrap();
        assert!(info.is_directory());
        assert!(!info.is_regular());
        assert_eq!(info.mode & 0o777, 0o755);
    }

    #[test]
    fn the_framing_matches_what_the_recording_shows() {
        // The recorded name block, framed: data, ZDLE 'k', then a 32-bit check
        // that arrived as `64 4a a8 bc`.
        let block = b"payload.bin\0\
                      4112 15263574505 100664 0 1 4112\0";
        let framed = encode(block, Framend::Zcrcw, Width::ThirtyTwo);
        assert!(framed.starts_with(block), "the payload needs no escaping");
        assert_eq!(&framed[block.len()..block.len() + 2], &[ZDLE, b'k']);
        assert_eq!(&framed[block.len() + 2..], &[0x64, 0x4A, 0xA8, 0xBC]);
    }

    #[test]
    fn an_escaped_payload_is_framed_and_still_checked_over_the_plain_bytes() {
        let data = [0x18u8, 0x11, 0x00, 0x0D];
        let framed = encode(&data, Framend::Zcrce, Width::ThirtyTwo);
        assert_eq!(
            &framed[..8],
            &[0x18, 0x58, 0x18, 0x51, 0x00, 0x18, 0x4D, 0x18, 0x68][..8],
            "escaped, then ZDLE 'h'"
        );

        // The check is over the plain bytes plus the frameend, not the escapes.
        let mut plain = data.to_vec();
        plain.push(b'h');
        let mut crc = Crc32::new();
        crc.update(&plain);
        assert_eq!(&framed[9..], &crc.bytes());
    }
}
