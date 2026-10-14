//! ZMODEM, written against the reference implementation instead of a spec.
//!
//! Why this is not a port: the upstream web client has no ZMODEM at all, and
//! what it does have is a different design for file transfer. So this module was
//! built by reading a real `sz`↔`rz` dialogue byte by byte and reproducing what
//! lrzsz actually does — `dev/zmodem-capture.py` records those dialogues, and
//! `tests/zmodem_reference.rs` replays the recordings.
//!
//! The published descriptions of ZMODEM disagree with the reference
//! implementation in several places, and each disagreement is a bug waiting to
//! happen. Everything below was measured, not assumed.
//!
//! # Both directions
//!
//! - [`receiver`] is the memory-free half: bytes in, [`Instruction`]s out. Built
//!   by replaying a recorded download, and required to reproduce the replies a
//!   real `rz` sent.
//! - [`sender`] is the half the host needs, because the web client can only push
//!   text. Files are pulled a block at a time rather than held. Verified against
//!   a real `rz` — `dev/zmodem-send-check.py` drives it at one and checks the
//!   file that comes out the far end — and the recording of that session is
//!   replayed by `the_sender_writes_exactly_the_bytes_a_real_rz_drove_it_to_write`.
//!
//! Both are pure: no socket, no disk, no clock. The caller owns the link and the
//! file, which is what makes the whole dialogue testable from a recording.
//!
//! # The wire
//!
//! A header is one of three shapes, and the shape also announces the frame-check
//! width used by the data subpackets that follow it:
//!
//! ```text
//! hex    * * ZDLE 'B' <14 hex digits>            CR LF XON     16-bit check
//! bin16  * ZDLE 'A'   <5 bytes> <2 byte check>                  16-bit check
//! bin32  * ZDLE 'C'   <5 bytes> <4 byte check>                  32-bit check
//! ```
//!
//! The 14 hex digits are `TYPE` then `F3 F2 F1 F0` then the check — the four
//! flag bytes travel in *reverse* order, so `Header::data` here is in the
//! logical `F0..F3` order and the reversal lives in the codec. (Getting this
//! backwards still round-trips perfectly, which is why it is worth a test
//! against a recorded header rather than a self-consistency check.)
//!
//! The receiver answers in hex headers; the sender uses hex only when no data
//! subpacket follows (ZRQINIT, ZFIN, …) and binary otherwise.
//!
//! A data subpacket is a header of type `ZDATA` — or a continuation with no
//! header at all — followed by data, then `ZDLE <frameend>`, then the check.
//! The check accumulates the data *and the frameend byte*, and lrzsz sends the
//! four check bytes least-significant first.
//!
//! # Checks — two different algorithms, two different byte orders
//!
//! - 16-bit is CRC-16/XMODEM: poly `0x1021`, init 0, **MSB-first**, and the
//!   common claim that ZMODEM uses CRC-16/X-25 describes the wrong algorithm.
//!   It is written big-endian.
//! - 32-bit is the usual reflected CRC-32 with init `~0` and xor `~0`, i.e.
//!   exactly what `zlib` calls the CRC-32. It is written **little-endian**.
//!
//! Both were confirmed against headers lrzsz emitted — a real `ZRINIT` checks to
//! `0xbe50`, a real `ZDATA` header to `0x8c92efbc` on the wire as `bc ef 92 8c`.
//!
//! # Deliberate differences from lrzsz
//!
//! - **We escape two bytes lrzsz usually does not.** lrzsz always escapes
//!   `{0x10, 0x11, 0x13, 0x18, 0x90, 0x91, 0x93}`, and escapes `0x0d`/`0x8d` only
//!   in a context that ten thousand bytes of recorded traffic hit three times
//!   (always immediately after `0xC0`). Rather than reproduce a rule whose
//!   purpose is unclear, we escape those two unconditionally. Both are decoded
//!   identically by any receiver, and escaping more is the safe direction: it
//!   costs one byte per CR and keeps a stray carriage return off a serial line
//!   whose line discipline might otherwise fold it into a newline.
//! - **We send the documented hex-header tail** (`CR LF XON`) rather than the
//!   `CR 0x8A XON` lrzsz writes, where `0x8A` is `0x0A` with the top bit set.
//!   A real `sz` accepts the documented form.
//! - **Our control block omits lrzsz's last field.** lrzsz writes the length
//!   twice, ending a 4112-byte file's block `… 1 4112 NUL`; [`FileInfo::to_block`]
//!   stops after the first `remaining`. A reader stops at the terminating NUL, so
//!   a real `rz` took the shorter block and wrote the file byte for byte —
//!   recorded, not assumed.
//! - **We do not implement the ZMODEM "crash recovery"/resume negotiation**
//!   beyond honouring `ZRPOS` on the receiver side, and we do not implement the
//!   `ZCOMMAND`/`ZSTDERR` remote-command channel.

pub mod crc;
pub mod error;
pub mod escape;
pub mod frame;
pub mod parser;
pub mod receiver;
pub mod sender;
pub mod subpacket;

pub use crc::{Crc16, Crc32, Width};
pub use error::Failure;
pub use escape::{unescape, Framend, ZDLE, ZPAD};
pub use frame::{cap, encode as encode_header, Encoding, FrameKind, Header, MAX_FILE};
pub use parser::{Corruption, Event, Parser, Stats, MAX_SUBPACKET};
pub use receiver::{Instruction, Policy, Receiver, CAPABILITIES};
pub use sender::{Request, Sender};
pub use subpacket::FileInfo;
