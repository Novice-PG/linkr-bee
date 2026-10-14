//! The receiving side of a transfer: the dialogue, as behaviour.
//!
//! [`super::parser`] turns bytes into frames; this turns frames into the
//! *replies* a receiver must send, and into the instructions a caller needs to
//! store the file. The split matters: nothing here touches a socket, a disk, or
//! a clock, so the whole dialogue can be driven from a recording and compared
//! against what the reference implementation actually said.
//!
//! # The dialogue, as lrzsz plays it
//!
//! Taken from a recorded 4112-byte download rather than from the specification,
//! because the two differ in a way that is worth knowing:
//!
//! | received | answered with |
//! | --- | --- |
//! | (start) | `ZRINIT` — a receiver announces itself |
//! | `ZRQINIT` | `ZRINIT` |
//! | `ZFILE` + its name block | `ZRPOS 0` — "send it, from the beginning" |
//! | `ZDATA` blocks ending `ZCRCG` | nothing; the run is unbroken |
//! | `ZEOF` | `ZRINIT` — ready for the next file |
//! | `ZFIN` | `ZFIN` |
//!
//! The one that surprises: **`ZFILE` is answered with `ZRPOS`, not `ZACK`.** The
//! name block ends with `ZCRCW`, which the specification says owes an
//! acknowledgement, and a receiver that sends one where lrzsz sends a `ZRPOS`
//! will hold a dialogue of its own invention. Since a `ZRPOS` also says "send
//! from here", it does the job of both.
//!
//! # Streaming
//!
//! The file's contents are handed out as they arrive, one [`Instruction::Chunk`]
//! per subpacket, rather than accumulated in memory: the caller decides whether
//! that becomes a file, a buffer, or something discarded. `received` in the
//! receiver is therefore the authority on how much of the file has been
//! accepted, and it is exactly the offset a retry must resume from — the framer
//! drops a frame that fails its check before this module ever sees it.

use super::error::Failure;
use super::escape::Framend;
use super::frame::{cap, encode, Encoding, FrameKind, Header, MAX_FILE};
use super::parser::{Corruption, Event};
use super::subpacket::FileInfo;

/// The capabilities we advertise in `ZRINIT`.
///
/// `CANFC32` is the one that changes anything: without it a sender must fall
/// back to 16-bit checks. Full duplex and overlapped I/O are true of a host that
/// has a reader and a writer on the same link, and claiming them is what a real
/// receiver does.
pub const CAPABILITIES: u8 = cap::CANFDX | cap::CANOVIO | cap::CANFC32;

/// What the receiver wants done, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Instruction {
    /// Write these bytes to the link.
    Send(Vec<u8>),
    /// A file has been accepted. Create it, then append every following chunk.
    Start(FileInfo),
    /// The next block of the file's contents.
    Chunk(Vec<u8>),
    /// The file is complete and its length agrees with what arrived.
    Finish(FileInfo),
    /// Discard everything after byte `at`; the sender is resending from there.
    Resume { at: u64 },
    /// Give up on this file.
    Fail(Failure),
}

/// Which offers to take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Policy {
    /// Take anything the sender names.
    #[default]
    Everything,
    /// Refuse anything that is not an ordinary file.
    ///
    /// A receive cannot meaningfully recreate a directory or a device node, and
    /// a sender that offers one is either confused or hostile, so the answer is
    /// `ZSKIP`. A zero mode means the sender did not say, and is taken at face
    /// value.
    RegularFilesOnly,
}

impl Policy {
    fn accepts(self, info: &FileInfo) -> Result<(), Failure> {
        match self {
            Policy::Everything => Ok(()),
            Policy::RegularFilesOnly => {
                if info.is_regular() || info.mode == 0 {
                    Ok(())
                } else if info.is_directory() {
                    Err(Failure::Unsupported(format!(
                        "{} is a directory",
                        info.name()
                    )))
                } else {
                    Err(Failure::Unsupported(format!(
                        "{} is not a regular file",
                        info.name()
                    )))
                }
            }
        }
    }
}

/// Where the dialogue has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Nothing has been offered yet.
    Opening,
    /// A `ZFILE` arrived; its name block is the next subpacket.
    AwaitingName,
    /// The file's contents are arriving.
    Receiving,
    /// The file ended; waiting for the sender to close the session.
    Closing,
    /// The session is over.
    Closed,
}

/// The file in flight, and how much of it has been accepted.
struct Pending {
    info: FileInfo,
    received: u64,
}

/// A ZMODEM receiver: feed it frames, act on the instructions.
pub struct Receiver {
    policy: Policy,
    phase: Phase,
    pending: Option<Pending>,
}

impl Default for Receiver {
    fn default() -> Receiver {
        Receiver::new()
    }
}

impl Receiver {
    pub fn new() -> Receiver {
        Receiver {
            policy: Policy::default(),
            phase: Phase::Opening,
            pending: None,
        }
    }

    pub fn with_policy(policy: Policy) -> Receiver {
        Receiver {
            policy,
            ..Receiver::new()
        }
    }

    /// The `ZRINIT` a receiver sends before it has heard anything.
    ///
    /// A real `rz` announces itself this way, and so must a host that is waiting
    /// for a download to be started at the far end: the sender may be waiting
    /// for exactly this before it says anything.
    pub fn announce(&mut self) -> Vec<Instruction> {
        vec![send_rinit()]
    }

    /// Whether the session has closed.
    pub fn is_done(&self) -> bool {
        self.phase == Phase::Closed
    }

    /// Bytes of the current file accepted so far.
    pub fn received(&self) -> u64 {
        self.pending.as_ref().map_or(0, |pending| pending.received)
    }

    /// The file in flight, if any.
    pub fn current(&self) -> Option<&FileInfo> {
        self.pending.as_ref().map(|pending| &pending.info)
    }

    /// Feed one frame and get back what to do about it.
    pub fn on_event(&mut self, event: Event) -> Vec<Instruction> {
        match event {
            Event::Header { header, .. } => self.on_header(header),
            Event::Subpacket { data, end } => self.on_subpacket(data, end),
            Event::Corrupt(what) => self.on_corrupt(what),
        }
    }

    fn on_header(&mut self, header: Header) -> Vec<Instruction> {
        match header.kind {
            FrameKind::RqInit => {
                // "Are you there?" A receiver answers with the same ZRINIT it
                // opened with, which is what a real rz does — and it may arrive
                // more than once while the sender is waiting.
                vec![send_rinit()]
            }
            FrameKind::File => {
                self.phase = Phase::AwaitingName;
                Vec::new()
            }
            FrameKind::Eof => {
                // A file has two declared lengths: the one in the name block
                // and the `ZEOF` position. Both must agree with the bytes that
                // actually arrived — a file stored on a length that nothing
                // corroborates is a truncated or padded file that looks fine.
                let at_eof = u64::from(header.position());
                let outcome = match self.pending.take() {
                    Some(pending) => {
                        let named = pending.info.length;
                        let received = pending.received;
                        let declared = if received != at_eof {
                            Some(at_eof)
                        } else if named != 0 && named != received {
                            Some(named)
                        } else {
                            None
                        };
                        match declared {
                            Some(declared) => {
                                Instruction::Fail(Failure::LengthMismatch { declared, received })
                            }
                            None => {
                                let mut info = pending.info;
                                info.length = received;
                                Instruction::Finish(info)
                            }
                        }
                    }
                    None => Instruction::Fail(Failure::Unexpected(FrameKind::Eof)),
                };
                self.phase = Phase::Closing;
                // `ZEOF` is answered with `ZRINIT`: "ready for the next file".
                vec![outcome, send_rinit()]
            }
            FrameKind::Fin => {
                self.phase = Phase::Closed;
                vec![send_bare(FrameKind::Fin)]
            }
            FrameKind::Abort | FrameKind::Can => {
                self.phase = Phase::Closed;
                vec![Instruction::Fail(Failure::Aborted)]
            }
            FrameKind::Nak => match self.pending {
                Some(_) => resume_from(self.received()),
                None => vec![send_rinit()],
            },
            // `ZDATA` is followed by its subpacket, and the rest are frames this
            // side never receives in a download. Ignoring an unknown frame is
            // the safe answer: the dialogue is driven by the sender.
            _ => Vec::new(),
        }
    }

    fn on_subpacket(&mut self, data: Vec<u8>, end: Framend) -> Vec<Instruction> {
        match self.phase {
            Phase::AwaitingName => self.on_name_block(data),
            Phase::Receiving => {
                let mut out = Vec::new();
                if let Some(pending) = self.pending.as_mut() {
                    pending.received += data.len() as u64;
                    out.push(Instruction::Chunk(data));
                }
                // A `ZCRCQ`/`ZCRCW` asks for an acknowledgement; a `ZCRCG` run
                // is unbroken by design.
                if end.wants_ack() {
                    out.push(send_bare(FrameKind::Ack));
                }
                out
            }
            _ => Vec::new(),
        }
    }

    fn on_name_block(&mut self, block: Vec<u8>) -> Vec<Instruction> {
        let Some(info) = FileInfo::parse(&block) else {
            self.phase = Phase::Opening;
            return vec![
                Instruction::Fail(Failure::Unsupported(
                    "the offered file has no usable name".to_string(),
                )),
                send_bare(FrameKind::Skip),
            ];
        };

        match self.policy.accepts(&info) {
            Ok(()) => {
                self.pending = Some(Pending {
                    info: info.clone(),
                    received: 0,
                });
                self.phase = Phase::Receiving;
                vec![
                    Instruction::Start(info),
                    // Zero, because the whole file is wanted. A resumable
                    // receive would put the offset it already has here.
                    send_at(FrameKind::RPos, 0),
                ]
            }
            Err(why) => {
                self.phase = Phase::Opening;
                vec![Instruction::Fail(why), send_bare(FrameKind::Skip)]
            }
        }
    }

    fn on_corrupt(&mut self, what: Corruption) -> Vec<Instruction> {
        match what {
            // A header that failed its check is either line noise or a false
            // sync, so the answer is the protocol's own "again, please".
            Corruption::Header => vec![send_bare(FrameKind::Nak)],
            // The framer drops a bad frame before this module sees it, so
            // `received` is already the offset the sender has to restart from.
            Corruption::Subpacket | Corruption::Oversize => {
                if self.pending.is_some() {
                    resume_from(self.received())
                } else {
                    vec![send_rinit()]
                }
            }
        }
    }
}

/// Ask the sender to restart the current file at `at`.
///
/// A resume offset that will not fit the 32-bit position field is refused
/// rather than truncated: a wrapped offset would silently ask for the wrong
/// part of the file and the transfer would finish with plausible-looking
/// garbage. The protocol's own `ZCAN` is the way to say so.
fn resume_from(at: u64) -> Vec<Instruction> {
    if at > MAX_FILE {
        return vec![
            Instruction::Fail(Failure::Unsupported(format!(
                "resuming past {MAX_FILE} bytes is outside the 32-bit position field"
            ))),
            send_bare(FrameKind::Can),
        ];
    }
    vec![
        Instruction::Resume { at },
        send_at(FrameKind::RPos, at as u32),
    ]
}

/// Replies are hex headers, as every implementation sends them: hex protects
/// the reverse channel from stray control characters, and nothing follows a
/// reply anyway.
fn send(header: Header) -> Instruction {
    Instruction::Send(encode(header, Encoding::Hex))
}

fn send_bare(kind: FrameKind) -> Instruction {
    send(Header::bare(kind))
}

fn send_rinit() -> Instruction {
    send(Header::new(FrameKind::RInit, [CAPABILITIES, 0, 0, 0]))
}

fn send_at(kind: FrameKind, position: u32) -> Instruction {
    send(Header::bare(kind).with_position(position))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zmodem::crc::Width;
    use crate::zmodem::subpacket;
    use crate::zmodem::Parser;

    /// The frames a download is made of, assembled with the encoder — so these
    /// tests exercise the dialogue rather than the bytes.
    struct Sender {
        stream: Vec<u8>,
        width: Width,
    }

    impl Sender {
        fn new() -> Sender {
            Sender {
                stream: encode(Header::bare(FrameKind::RqInit), Encoding::Hex),
                width: Width::ThirtyTwo,
            }
        }

        fn file(mut self, block: &[u8]) -> Sender {
            self.stream.extend(encode(
                Header::new(FrameKind::File, [0x01, 0x00, 0x00, 0x00]),
                Encoding::Bin32,
            ));
            self.stream
                .extend(subpacket::encode(block, Framend::Zcrcw, self.width));
            self
        }

        fn data(mut self, blocks: &[(&[u8], Framend)]) -> Sender {
            self.stream
                .extend(encode(Header::bare(FrameKind::Data), Encoding::Bin32));
            for (block, end) in blocks {
                self.stream
                    .extend(subpacket::encode(block, *end, self.width));
            }
            self
        }

        fn eof(mut self, length: u32) -> Sender {
            self.stream.extend(encode(
                Header::bare(FrameKind::Eof).with_position(length),
                Encoding::Bin32,
            ));
            self
        }

        fn fin(mut self) -> Sender {
            self.stream
                .extend(encode(Header::bare(FrameKind::Fin), Encoding::Hex));
            self
        }

        /// Run the whole stream through a receiver and collect the instructions.
        fn drive(self, receiver: &mut Receiver) -> Vec<Instruction> {
            let mut parser = Parser::new();
            let mut out = Vec::new();
            parser.feed(&self.stream);
            while let Some(event) = parser.next_event() {
                out.extend(receiver.on_event(event));
            }
            out
        }
    }

    /// The kinds of the headers a run of instructions put on the wire, by
    /// decoding them again — which also proves the replies are well formed.
    fn reply_kinds(instructions: &[Instruction]) -> Vec<FrameKind> {
        let mut parser = Parser::new();
        let mut kinds = Vec::new();
        for instruction in instructions {
            let Instruction::Send(bytes) = instruction else {
                continue;
            };
            parser.feed(bytes);
            while let Some(event) = parser.next_event() {
                if let Event::Header { header, .. } = event {
                    kinds.push(header.kind);
                }
            }
        }
        kinds
    }

    fn joined(mut info: FileInfo, length: u64) -> Vec<u8> {
        info.length = length;
        info.to_block()
    }

    #[test]
    fn a_whole_file_is_received_and_the_dialogue_matches_lrzs() {
        let block = joined(FileInfo::new(*b"notes.txt", 10), 10);
        let mut receiver = Receiver::new();
        let mut instructions = receiver.announce();

        instructions.extend(
            Sender::new()
                .file(&block)
                .data(&[(b"hello ", Framend::Zcrcg), (b"you!", Framend::Zcrce)])
                .eof(10)
                .fin()
                .drive(&mut receiver),
        );

        assert_eq!(
            reply_kinds(&instructions),
            [
                FrameKind::RInit, // the announcement
                FrameKind::RInit, // answered ZRQINIT
                FrameKind::RPos,  // accepted the ZFILE
                FrameKind::RInit, // answered ZEOF, ready for more
                FrameKind::Fin,   // and closed
            ],
            "the reply dialogue is exactly the one a real rz holds"
        );

        let chunks: Vec<u8> = instructions
            .iter()
            .filter_map(|instruction| match instruction {
                Instruction::Chunk(data) => Some(data.as_slice()),
                _ => None,
            })
            .flatten()
            .copied()
            .collect();
        assert_eq!(chunks, b"hello you!");

        assert!(matches!(instructions.first(), Some(Instruction::Send(_))));
        assert!(instructions
            .iter()
            .any(|i| matches!(i, Instruction::Start(info) if info.name() == "notes.txt")));
        assert!(instructions
            .iter()
            .any(|i| matches!(i, Instruction::Finish(info) if info.length == 10)));
        assert!(receiver.is_done());
        assert_eq!(receiver.received(), 0, "the file is no longer in flight");
    }

    /// A `ZCRCG` run is unbroken, so the receiver must not answer each block:
    /// an acknowledgement per kilobyte would slow the transfer and, on a
    /// half-duplex link, corrupt the sender's stream with our own.
    #[test]
    fn a_continuing_run_is_not_acknowledged_per_block() {
        let block = joined(FileInfo::new(*b"f", 3), 3);
        let mut receiver = Receiver::new();
        let instructions = Sender::new()
            .file(&block)
            .data(&[
                (b"a", Framend::Zcrcg),
                (b"b", Framend::Zcrcg),
                (b"c", Framend::Zcrce),
            ])
            .drive(&mut receiver);

        assert_eq!(
            reply_kinds(&instructions),
            [FrameKind::RInit, FrameKind::RPos],
            "no ZACK: neither ZCRCG nor ZCRCE asks for one"
        );
    }

    #[test]
    fn a_block_that_asks_for_an_acknowledgement_gets_one() {
        let block = joined(FileInfo::new(*b"f", 2), 2);
        let mut receiver = Receiver::new();
        let instructions = Sender::new()
            .file(&block)
            .data(&[(b"ab", Framend::Zcrcq)])
            .drive(&mut receiver);

        assert_eq!(
            reply_kinds(&instructions),
            [FrameKind::RInit, FrameKind::RPos, FrameKind::Ack]
        );
    }

    #[test]
    fn a_directory_is_skipped_when_only_regular_files_are_wanted() {
        let mut receiver = Receiver::with_policy(Policy::RegularFilesOnly);
        let block = b"subdir\0 0 0 40755 0 0\0";
        let instructions = Sender::new().file(block).drive(&mut receiver);

        assert_eq!(
            reply_kinds(&instructions),
            [FrameKind::RInit, FrameKind::Skip],
            "a ZFILE that is refused is answered with ZSKIP"
        );
        assert!(instructions.iter().any(|instruction| matches!(
            instruction,
            Instruction::Fail(Failure::Unsupported(why)) if why.contains("directory")
        )));
        assert_eq!(receiver.received(), 0);
    }

    #[test]
    fn an_offer_with_no_usable_name_is_skipped() {
        let mut receiver = Receiver::new();
        let instructions = Sender::new().file(b"\0").drive(&mut receiver);
        assert_eq!(
            reply_kinds(&instructions),
            [FrameKind::RInit, FrameKind::Skip]
        );
        assert!(instructions
            .iter()
            .any(|instruction| matches!(instruction, Instruction::Fail(Failure::Unsupported(_)))));
    }

    /// A length the far end claims that the bytes do not support is the failure
    /// a silent truncation would look like, so it is reported rather than
    /// accepted.
    #[test]
    fn a_length_that_disagrees_with_what_arrived_fails_the_file() {
        let block = joined(FileInfo::new(*b"f", 100), 100);
        let mut receiver = Receiver::new();
        let instructions = Sender::new()
            .file(&block)
            .data(&[(b"short", Framend::Zcrce)])
            .eof(100)
            .drive(&mut receiver);

        assert!(instructions.iter().any(|instruction| matches!(
            instruction,
            Instruction::Fail(Failure::LengthMismatch {
                declared: 100,
                received: 5
            })
        )));
        assert!(
            !instructions
                .iter()
                .any(|instruction| matches!(instruction, Instruction::Finish(_))),
            "a file that did not arrive in full is never reported complete"
        );
    }

    /// A corrupt subpacket is dropped by the framer before it reaches here, so
    /// the resume offset is where the bytes stopped — asking for anything else
    /// would duplicate or lose data.
    #[test]
    fn a_corrupt_block_asks_for_a_resume_where_the_bytes_stopped() {
        let block = joined(FileInfo::new(*b"f", 9), 9);
        let mut receiver = Receiver::new();
        let mut instructions = Sender::new()
            .file(&block)
            .data(&[(b"12345", Framend::Zcrcg)])
            .drive(&mut receiver);
        assert_eq!(receiver.received(), 5);

        instructions.extend(receiver.on_event(Event::Corrupt(Corruption::Subpacket)));
        assert!(
            instructions.contains(&Instruction::Resume { at: 5 }),
            "the caller is told to discard everything after the last good byte"
        );
        assert_eq!(
            reply_kinds(&instructions),
            [FrameKind::RInit, FrameKind::RPos, FrameKind::RPos],
            "the sender is asked for the bytes from that offset"
        );
    }

    /// The name block and the `ZEOF` position both claim a length. When they
    /// agree with the bytes, the file is good; when either disagrees, the file
    /// is short or long and must not be reported complete.
    #[test]
    fn a_length_only_one_of_the_two_declarations_supports_fails_the_file() {
        // The name block says 9, `ZEOF` says 10, ten bytes arrive.
        let block = joined(FileInfo::new(*b"f", 9), 9);
        let mut receiver = Receiver::new();
        let instructions = Sender::new()
            .file(&block)
            .data(&[(b"0123456789", Framend::Zcrce)])
            .eof(10)
            .drive(&mut receiver);

        assert!(
            instructions.iter().any(|instruction| matches!(
                instruction,
                Instruction::Fail(Failure::LengthMismatch {
                    declared: 9,
                    received: 10
                })
            )),
            "the name block's length is corroborated too, not just the ZEOF position"
        );
        assert!(!instructions
            .iter()
            .any(|instruction| matches!(instruction, Instruction::Finish(_))));
    }

    /// A `ZEOF` position is the authority the bytes are measured against, so a
    /// file whose name block omitted its length still completes.
    #[test]
    fn a_missing_named_length_does_not_block_a_complete_file() {
        let mut info = FileInfo::new(*b"f", 0);
        info.length = 0;
        let block = info.to_block();
        let mut receiver = Receiver::new();
        let instructions = Sender::new()
            .file(&block)
            .data(&[(b"abc", Framend::Zcrce)])
            .eof(3)
            .drive(&mut receiver);

        assert!(
            instructions.iter().any(
                |instruction| matches!(instruction, Instruction::Finish(info) if info.length == 3)
            ),
            "the received count is what the caller stores"
        );
    }

    #[test]
    fn a_bad_header_is_answered_with_a_nak() {
        let mut receiver = Receiver::new();
        let instructions = receiver.on_event(Event::Corrupt(Corruption::Header));
        assert_eq!(reply_kinds(&instructions), [FrameKind::Nak]);
    }

    /// The position field is 32 bits, and a wrapped offset would ask for the
    /// wrong part of the file while looking entirely well formed — so the
    /// boundary is refused rather than truncated.
    #[test]
    fn a_resume_past_the_32_bit_position_field_is_refused_not_wrapped() {
        assert_eq!(
            resume_from(4112),
            [
                Instruction::Resume { at: 4112 },
                send_at(FrameKind::RPos, 4112)
            ]
        );
        assert_eq!(
            resume_from(MAX_FILE),
            [
                Instruction::Resume { at: MAX_FILE },
                send_at(FrameKind::RPos, u32::MAX)
            ],
            "the last representable byte is still a valid resume point"
        );

        let over = resume_from(MAX_FILE + 1);
        assert!(
            matches!(over[0], Instruction::Fail(Failure::Unsupported(_))),
            "an offset that does not fit is a failure, not a silent wrap"
        );
        assert!(
            !over
                .iter()
                .any(|instruction| matches!(instruction, Instruction::Resume { .. })),
            "no offset is handed to the caller that the wire cannot carry"
        );
        assert_eq!(reply_kinds(&over), [FrameKind::Can]);
    }

    #[test]
    fn the_sender_giving_up_ends_the_session() {
        let block = joined(FileInfo::new(*b"f", 10), 10);
        for kind in [FrameKind::Abort, FrameKind::Can] {
            let mut receiver = Receiver::new();
            let mut sender = Sender::new().file(&block);
            sender
                .stream
                .extend(encode(Header::bare(kind), Encoding::Bin32));
            let instructions = sender.drive(&mut receiver);
            assert!(instructions.contains(&Instruction::Fail(Failure::Aborted)));
            assert!(receiver.is_done());
        }
    }

    /// An end-of-file with nothing in flight is not a file of zero bytes; it is
    /// a dialogue out of step, and saying so beats creating an empty file.
    #[test]
    fn an_end_of_file_with_no_file_in_flight_is_refused() {
        let mut receiver = Receiver::new();
        let sender = Sender::new().eof(0);
        let instructions = sender.drive(&mut receiver);
        assert!(instructions.contains(&Instruction::Fail(Failure::Unexpected(FrameKind::Eof))));
    }

    #[test]
    fn the_announced_capabilities_are_the_ones_a_sender_acts_on() {
        let mut receiver = Receiver::new();
        let instructions = receiver.announce();
        let Some(Instruction::Send(bytes)) = instructions.first() else {
            panic!("a receiver announces itself first");
        };

        let mut parser = Parser::new();
        parser.feed(bytes);
        let Some(Event::Header { header, encoding }) = parser.next_event() else {
            panic!("the announcement is a header");
        };
        assert_eq!(header.kind, FrameKind::RInit);
        assert_eq!(encoding, Encoding::Hex);
        assert_eq!(header.f0(), CAPABILITIES);
        assert_ne!(header.f0() & cap::CANFC32, 0, "32-bit checks are offered");
    }
}
