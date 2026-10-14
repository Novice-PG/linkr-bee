//! The sending side of a transfer: the dialogue, as behaviour.
//!
//! This is the half the host actually needs. The web client can push bytes to
//! the far end only as base64 text through a shell, which is fine for a
//! configuration file and hopeless for anything with a `0x18` in it — so
//! uploading a firmware image or a log file means driving `sz`'s side of ZMODEM
//! from here.
//!
//! [`super::receiver`] is the same machine with the arrows reversed, and the two
//! are tested against each other: a session driven between this module and that
//! one, with the bytes of a real file on the wire, has to come out byte for byte
//! correct. Neither module touches a socket, a disk, or a clock.
//!
//! # The dialogue, as lrzsz plays it
//!
//! | sent | answered with | then |
//! | --- | --- | --- |
//! | `ZRQINIT` | `ZRINIT` | offer the first file |
//! | `ZFILE` + its name block, ending `ZCRCW` | `ZRPOS 0` | send the contents |
//! | `ZDATA` + blocks, `ZCRCG` … `ZCRCE` | nothing | — |
//! | `ZEOF` | `ZRINIT` | offer the next file, or close |
//! | `ZFIN` | `ZFIN` | `OO` |
//!
//! Two details are worth stating because the obvious choice is the wrong one.
//!
//! **The last data block ends `ZCRCE`, not `ZCRCG`.** A recorded 4112-byte
//! transfer is four 1024-byte blocks ending `ZCRCG` and a 16-byte tail ending
//! `ZCRCE`, and only then does the `ZEOF` follow. `ZCRCG` means "more of this
//! run is coming"; a run that ends has to say so, and `ZCRCE` is how.
//!
//! **`ZEOF` carries the file's length, not the last offset.** They are the same
//! number here, because the offset after the final byte *is* the length. The
//! field is also 32 bits and little-endian on the wire, and its bytes are inside
//! the escape set — a real `ZEOF` for 4112 bytes travels as `0b 18 50 18 50 00
//! 00`, with both `0x10` bytes escaped.
//!
//! # The file's contents are pulled, not held
//!
//! The sender emits [`Request::Read`] and waits for [`Sender::supply`]. That
//! keeps the module free of I/O and free of the whole file at once: a 200 MB
//! firmware image goes out in 1024-byte blocks with 1024 bytes held at a time.
//! Each request carries a `round` token, so a block that the receiver has since
//! asked to be resent can be recognised as stale and dropped rather than
//! silently sent as if it were the block that was asked for.

use std::collections::VecDeque;

use super::error::Failure;
use super::escape::Framend;
use super::frame::{cap, encode, Encoding, FrameKind, Header, MAX_FILE};
use super::parser::{Corruption, Event};
use super::subpacket::{self, FileInfo};

/// The block size lrzsz sends, apparent from the recording: a 4112-byte file
/// goes out as four 1024-byte blocks and a 16-byte tail.
pub const BLOCK: usize = 1024;

/// The string `sz` writes before it says anything else, naming the program the
/// far end should run. Only meaningful when the far side is a shell, so it is
/// opt-in — see [`Sender::wakeup`].
pub const WAKEUP: &[u8] = b"rz\r";

/// lrzsz's sign-off, written after the far end's closing `ZFIN`.
pub const OVER_AND_OUT: &[u8] = b"OO";

/// The `ZFILE` flag byte that says the file is to be transferred as it is.
///
/// It shares the value `0x01` with `cap::CANFDX`, but it is a different field of
/// a different frame: this one describes the file, that one describes the
/// receiver. Naming it separately is what stops the two being confused.
const ZCBIN: u8 = 0x01;

/// What the sender wants done, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Write these bytes to the link.
    Send(Vec<u8>),
    /// Hand [`Sender::supply`] `len` bytes of the current file from `at`, and
    /// pass the same `round` back with them. A request whose round is no longer
    /// the current one has been overtaken and its bytes are not wanted.
    Read { round: u64, at: u64, len: usize },
    /// The file went through: every byte was delivered and the far end said it
    /// was ready for more.
    Sent(FileInfo),
    /// The far end refused this file (`ZSKIP`) — a directory, or a file it
    /// already has.
    Skipped(FileInfo),
    /// The session is over; the caller can release whatever it held.
    Finished,
    /// Give up. Anything sent from here on is not part of a session.
    Failed(Failure),
}

/// Where the dialogue has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// `ZRQINIT` sent, waiting for the far end to say it is ready.
    Opening,
    /// A file has been offered; waiting for `ZRPOS` saying where to start.
    AwaitingPos,
    /// The contents are going out.
    Sending,
    /// The last `ZEOF` was sent; waiting for the far end to say it is ready for
    /// the next file.
    AwaitingNext,
    /// `ZFIN` sent, waiting for the far end's `ZFIN`.
    Closing,
    /// The session is over.
    Closed,
}

/// A block the sender has asked for and not yet been given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Wanted {
    round: u64,
    at: u64,
    len: usize,
}

/// A ZMODEM sender: give it file descriptions, answer its requests.
pub struct Sender {
    queue: VecDeque<FileInfo>,
    /// The file being offered or sent.
    current: Option<FileInfo>,
    /// The file whose `ZEOF` is out and unanswered. Reported as [`Request::Sent`]
    /// only once the far end acknowledges it — reporting it when the last byte
    /// was written would call a file sent that the far end may still reject.
    owed: Option<FileInfo>,
    /// How much of the current file has been written to the link.
    offset: u64,
    phase: Phase,
    /// What the far end said it can do, from its first `ZRINIT`.
    peer: u8,
    /// The outstanding read, if any.
    wanted: Option<Wanted>,
    /// Bumped whenever the dialogue restarts a file, so that a block asked for
    /// before the restart can be told apart from one asked for after it.
    round: u64,
    /// Whether to write the `rz\r` wakeup before the first `ZRQINIT`.
    wakeup: bool,
    started: bool,
}

impl Sender {
    /// Queue the files to send. Their contents are pulled, not taken here.
    pub fn new(files: impl IntoIterator<Item = FileInfo>) -> Sender {
        Sender {
            queue: files.into_iter().collect(),
            current: None,
            owed: None,
            offset: 0,
            phase: Phase::Opening,
            peer: 0,
            wanted: None,
            round: 0,
            wakeup: false,
            started: false,
        }
    }

    /// Write `rz\r` first, so that a shell at the far end starts a receiver.
    ///
    /// Off by default: the far end of this bridge is a serial line to another
    /// machine's UART, and `rz\r` there is five bytes of noise unless something
    /// is waiting to read it as a command.
    pub fn wakeup(mut self, yes: bool) -> Sender {
        self.wakeup = yes;
        self
    }

    /// Open the session. Writes the wakeup if it was asked for, then `ZRQINIT`.
    ///
    /// Calling it twice does nothing the second time, so a caller that is not
    /// sure whether it has started yet cannot open two sessions.
    pub fn start(&mut self) -> Vec<Request> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        let mut out = Vec::new();
        if self.wakeup {
            out.push(Request::Send(WAKEUP.to_vec()));
        }
        // `ZRQINIT` is a hex header: nothing follows it, and the far end is
        // entitled to answer before it has settled on a check width.
        out.push(send(Header::bare(FrameKind::RqInit), Encoding::Hex));
        out
    }

    /// Whether the session has closed.
    pub fn is_done(&self) -> bool {
        self.phase == Phase::Closed
    }

    /// The file being offered or sent, if any.
    pub fn current(&self) -> Option<&FileInfo> {
        self.current.as_ref()
    }

    /// The block the sender is waiting to be given, as `(round, at, len)`.
    pub fn waiting_for(&self) -> Option<(u64, u64, usize)> {
        self.wanted
            .map(|wanted| (wanted.round, wanted.at, wanted.len))
    }

    /// Feed one frame and get back what to do about it.
    pub fn on_event(&mut self, event: Event) -> Vec<Request> {
        match event {
            Event::Header { header, .. } => self.on_header(header),
            // A sender is never handed a subpacket; the far end only speaks in
            // headers. If one arrives it is line noise that happened to check
            // out, and acting on it would be worse than ignoring it.
            Event::Subpacket { .. } => Vec::new(),
            Event::Corrupt(what) => self.on_corrupt(what),
        }
    }

    fn on_header(&mut self, header: Header) -> Vec<Request> {
        match header.kind {
            FrameKind::RInit => self.on_rinit(header.f0()),
            FrameKind::RPos => self.begin_data(u64::from(header.position())),
            FrameKind::Skip => self.skip(),
            FrameKind::Nak => self.retry(),
            FrameKind::Fin => {
                let mut out = Vec::new();
                // A far end that closes first still gets an answer, so that it
                // is not left waiting for a reply that will never come.
                if self.phase != Phase::Closing {
                    out.push(send(Header::bare(FrameKind::Fin), Encoding::Hex));
                }
                self.phase = Phase::Closed;
                out.push(Request::Send(OVER_AND_OUT.to_vec()));
                out.push(Request::Finished);
                out
            }
            FrameKind::Abort | FrameKind::Can => {
                self.phase = Phase::Closed;
                vec![Request::Failed(Failure::Aborted)]
            }
            _ => Vec::new(),
        }
    }

    fn on_rinit(&mut self, capabilities: u8) -> Vec<Request> {
        match self.phase {
            Phase::Opening | Phase::AwaitingNext => {
                // The first `ZRINIT` is remembered and used to pick a check
                // width; later ones are allowed to change nothing, because a
                // sender that re-reads them mid-session could switch encoding
                // between the header and the block it describes.
                if self.phase == Phase::Opening {
                    self.peer = capabilities;
                }
                self.advance()
            }
            // A session's opening `ZRINIT` is answered twice over: once
            // spontaneously and once as the reply to our `ZRQINIT`. Only the
            // first moves the dialogue on — acting on the second would offer the
            // same file twice, and the far end would store it twice.
            _ => Vec::new(),
        }
    }

    /// Report the file the far end acknowledged, then offer the next one, or
    /// close if there are no more.
    fn advance(&mut self) -> Vec<Request> {
        let mut out = Vec::new();
        if let Some(info) = self.owed.take() {
            out.push(Request::Sent(info));
        }
        match self.queue.pop_front() {
            Some(info) => out.extend(self.offer(info)),
            None => out.extend(self.close()),
        }
        out
    }

    fn offer(&mut self, info: FileInfo) -> Vec<Request> {
        // The position field is 32 bits on the wire, so a longer file cannot be
        // described at all. Refusing up front beats sending a truncated `ZEOF`
        // and being believed.
        if info.length > MAX_FILE {
            return self.fail(Failure::Unsupported(format!(
                "{} is {} bytes, past the 32-bit position field",
                info.name(),
                info.length
            )));
        }

        let frame = encode(
            Header::new(FrameKind::File, [ZCBIN, 0, 0, 0]),
            self.binary(),
        );
        let block = subpacket::encode(&info.to_block(), Framend::Zcrcw, self.width());
        self.current = Some(info);
        self.offset = 0;
        self.wanted = None;
        self.round += 1;
        self.phase = Phase::AwaitingPos;
        vec![Request::Send(frame), Request::Send(block)]
    }

    /// Start, or restart, the contents of the current file at `at`.
    fn begin_data(&mut self, at: u64) -> Vec<Request> {
        let Some(info) = self.current.clone() else {
            // A `ZRPOS` with nothing offered is the far end out of step. The
            // cheap answer is silence: the sender drives the offers, so there is
            // no earlier point to go back to.
            return Vec::new();
        };
        if at > info.length {
            return self.fail(Failure::LengthMismatch {
                declared: info.length,
                received: at,
            });
        }

        // Any block already asked for belongs to a stream the far end has just
        // rejected, so its token is retired rather than cancelled — the caller
        // may already be reading it, and the bytes it comes back with must not
        // be mistaken for the ones now wanted.
        self.wanted = None;
        self.round += 1;
        self.offset = at;
        self.phase = Phase::Sending;

        let mut out = vec![send(Header::bare(FrameKind::Data), self.binary())];
        if at == info.length {
            // Nothing left but the end: either the file is empty, or the far end
            // already has all of it.
            //
            // An empty file still gets one empty subpacket. A bare `ZDATA`
            // header with no subpacket behind it is read as a truncated
            // transfer — the recorded `empty` session shows `rz` answering one
            // with a second `ZRPOS` rather than moving on — while lrzsz's `sz`
            // writes `ZDATA`, an empty `ZCRCE`, then the `ZEOF`.
            if info.length == 0 {
                out.push(Request::Send(subpacket::encode(
                    &[],
                    Framend::Zcrce,
                    self.width(),
                )));
            }
            out.extend(self.finish_file());
        } else {
            out.extend(self.request_block());
        }
        out
    }

    /// Ask for the next block, or finish the file if there is nothing left.
    fn request_block(&mut self) -> Vec<Request> {
        let Some(info) = self.current.as_ref() else {
            return Vec::new();
        };
        let remaining = info.length - self.offset;
        if remaining == 0 {
            return self.finish_file();
        }
        let len = remaining.min(BLOCK as u64) as usize;
        let wanted = Wanted {
            round: self.round,
            at: self.offset,
            len,
        };
        self.wanted = Some(wanted);
        vec![Request::Read {
            round: wanted.round,
            at: wanted.at,
            len: wanted.len,
        }]
    }

    /// Take the bytes that were asked for, put them on the wire, and continue.
    ///
    /// Bytes that do not answer the outstanding request are dropped: a caller
    /// reading a large file will not stop mid-read because the far end changed
    /// its mind, and sending those bytes now would corrupt the file rather than
    /// merely waste the read.
    pub fn supply(&mut self, round: u64, bytes: &[u8]) -> Vec<Request> {
        let Some(wanted) = self.wanted else {
            return Vec::new();
        };
        if wanted.round != round || bytes.len() != wanted.len {
            return Vec::new();
        }
        self.wanted = None;

        let length = self.current.as_ref().map_or(0, |info| info.length);
        let last = wanted.at + wanted.len as u64 >= length;
        // The run ends on `ZCRCE`; every block before it continues with `ZCRCG`.
        let end = if last { Framend::Zcrce } else { Framend::Zcrcg };
        self.offset = wanted.at + wanted.len as u64;

        let mut out = vec![Request::Send(subpacket::encode(bytes, end, self.width()))];
        if last {
            out.extend(self.finish_file());
        } else {
            out.extend(self.request_block());
        }
        out
    }

    /// Send the end-of-file header that declares the length, and hold the file
    /// as owed until the far end acknowledges it.
    fn finish_file(&mut self) -> Vec<Request> {
        let Some(info) = self.current.take() else {
            return Vec::new();
        };
        // `offer` refuses a file past the 32-bit position field, so this fits.
        let position = info.length as u32;
        self.phase = Phase::AwaitingNext;
        self.owed = Some(info);
        vec![send(
            Header::bare(FrameKind::Eof).with_position(position),
            self.binary(),
        )]
    }

    fn skip(&mut self) -> Vec<Request> {
        let mut out = Vec::new();
        if let Some(info) = self.current.take() {
            out.push(Request::Skipped(info));
        }
        self.wanted = None;
        // A skip leaves the far end still ready, so the next file can be offered
        // without waiting for anything.
        self.phase = Phase::Opening;
        out.extend(self.advance());
        out
    }

    fn close(&mut self) -> Vec<Request> {
        self.phase = Phase::Closing;
        vec![send(Header::bare(FrameKind::Fin), Encoding::Hex)]
    }

    fn retry(&mut self) -> Vec<Request> {
        match self.phase {
            // A `ZNAK` while the far end has not yet said where to start from is
            // about the offer, so the offer is what is written again.
            Phase::AwaitingPos => {
                let Some(info) = self.current.clone() else {
                    return Vec::new();
                };
                self.current = None;
                self.offer(info)
            }
            // Otherwise it is about the header in front of the data, and that
            // header plus the stream from the current offset is the answer.
            Phase::Sending => {
                let at = self.offset;
                self.begin_data(at)
            }
            Phase::Opening => self.start(),
            _ => Vec::new(),
        }
    }

    fn on_corrupt(&mut self, what: Corruption) -> Vec<Request> {
        match what {
            // A damaged header on the reverse channel means the far end's
            // "ready" may have been missed. Before anything has been offered,
            // saying hello again is free; once a file is in flight the dialogue
            // is under way, and a repeated `ZRQINIT` would be a second session.
            Corruption::Header => {
                if self.phase == Phase::Opening {
                    vec![send(Header::bare(FrameKind::RqInit), Encoding::Hex)]
                } else {
                    Vec::new()
                }
            }
            // A sender is never handed subpackets, so this cannot arise from the
            // dialogue; the framer drops the frame either way.
            Corruption::Subpacket | Corruption::Oversize => Vec::new(),
        }
    }

    /// The encoding for headers that carry data: `bin32` when the far end
    /// advertised 32-bit checks, and `bin16` when it did not. This is the one
    /// thing a receiver's capabilities legitimately change.
    fn binary(&self) -> Encoding {
        if self.peer & cap::CANFC32 != 0 {
            Encoding::Bin32
        } else {
            Encoding::Bin16
        }
    }

    fn width(&self) -> super::crc::Width {
        self.binary().check()
    }

    fn fail(&mut self, why: Failure) -> Vec<Request> {
        self.phase = Phase::Closed;
        // lrzsz writes `ZCAN` eight times so that some copy survives a noisy
        // line. One is enough for a link with a checksum, and the caller is
        // told the reason rather than being left to infer it.
        vec![
            Request::Failed(why),
            send(Header::bare(FrameKind::Can), Encoding::Hex),
        ]
    }
}

fn send(header: Header, encoding: Encoding) -> Request {
    Request::Send(encode(header, encoding))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zmodem::receiver::{Instruction, Receiver, CAPABILITIES};
    use crate::zmodem::Parser;

    fn info(name: &str, length: u64) -> FileInfo {
        let mut info = FileInfo::new(name.as_bytes().to_vec(), length);
        info.mode = 0o100664;
        info.modified = 1_791_949_125;
        info.remaining = 1;
        info
    }

    /// Every byte value in turn, repeated past several block boundaries, so the
    /// escape layer meets `ZDLE`, the control characters and the high bytes, and
    /// the run has full and partial blocks in it.
    fn payload(length: usize) -> Vec<u8> {
        (0..length).map(|index| (index % 256) as u8).collect()
    }

    fn rinit(capabilities: u8) -> Header {
        Header::new(FrameKind::RInit, [capabilities, 0, 0, 0])
    }

    /// Hand the sender one header, as if it had arrived from the link.
    fn deliver(sender: &mut Sender, header: Header) -> Vec<Request> {
        let bytes = encode(header, Encoding::Hex);
        let mut parser = Parser::new();
        parser.feed(&bytes);
        let mut out = Vec::new();
        while let Some(event) = parser.next_event() {
            out.extend(sender.on_event(event));
        }
        out
    }

    /// The headers in a run of requests, decoded again — which also proves the
    /// frames are well formed rather than merely the right variant.
    fn frames(requests: &[Request]) -> Vec<FrameKind> {
        let mut parser = Parser::new();
        let mut kinds = Vec::new();
        for request in requests {
            let Request::Send(bytes) = request else {
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

    /// The subpackets in a stream, as `(length, frameend)`.
    fn blocks(bytes: &[u8]) -> Vec<(usize, Framend)> {
        let mut parser = Parser::new();
        parser.feed(bytes);
        let mut out = Vec::new();
        while let Some(event) = parser.next_event() {
            if let Event::Subpacket { data, end } = event {
                out.push((data.len(), end));
            }
        }
        out
    }

    /// Everything the two halves said to each other, once neither has anything
    /// left to say.
    struct Log {
        requests: Vec<Request>,
        instructions: Vec<Instruction>,
        /// Every byte the sender put on the wire.
        written: Vec<u8>,
        /// Every byte the receiver put on the wire.
        answered: Vec<u8>,
    }

    fn converse(
        sender: &mut Sender,
        receiver: &mut Receiver,
        files: &[(FileInfo, Vec<u8>)],
    ) -> Log {
        let mut log = Log {
            requests: Vec::new(),
            instructions: Vec::new(),
            written: Vec::new(),
            answered: Vec::new(),
        };
        let mut into_receiver = Parser::new();
        let mut into_sender = Parser::new();
        let mut work: std::collections::VecDeque<Request> = sender.start().into();
        let mut steps = 0;

        while let Some(request) = work.pop_front() {
            steps += 1;
            assert!(
                steps < 100_000,
                "a dialogue that does not end is not a dialogue"
            );
            log.requests.push(request.clone());

            if let Request::Read { round, at, len } = request {
                let current = sender.current().expect("a read belongs to a file").clone();
                let (_, body) = files
                    .iter()
                    .find(|(info, _)| info.name() == current.name())
                    .unwrap_or_else(|| panic!("no body for {}", current.name()));
                let slice = &body[at as usize..at as usize + len];
                work.extend(sender.supply(round, slice));
                continue;
            }

            let Request::Send(bytes) = request else {
                continue;
            };
            log.written.extend_from_slice(&bytes);
            into_receiver.feed(&bytes);
            while let Some(event) = into_receiver.next_event() {
                for instruction in receiver.on_event(event) {
                    if let Instruction::Send(reply) = &instruction {
                        log.answered.extend_from_slice(reply);
                        into_sender.feed(reply);
                    }
                    log.instructions.push(instruction);
                }
            }
            while let Some(event) = into_sender.next_event() {
                work.extend(sender.on_event(event));
            }
        }
        log
    }

    /// A payload of one every-eight-bytes is not the same file for very long, so
    /// the reassembled bytes are compared rather than a checksum of them.
    #[test]
    fn a_session_between_the_two_halves_delivers_the_file_intact() {
        let body = payload(2500);
        let files = [(info("notes.bin", body.len() as u64), body.clone())];
        let mut sender = Sender::new(files.iter().map(|(info, _)| info.clone()));
        let mut receiver = Receiver::new();
        let log = converse(&mut sender, &mut receiver, &files);

        let arrived: Vec<u8> = log
            .instructions
            .iter()
            .filter_map(|instruction| match instruction {
                Instruction::Chunk(data) => Some(data.as_slice()),
                _ => None,
            })
            .flatten()
            .copied()
            .collect();
        assert_eq!(arrived, body, "the file arrives byte for byte");

        assert!(
            sender.is_done(),
            "the sender closes when there is nothing left"
        );
        assert!(receiver.is_done(), "and so does the receiver");
        assert!(
            log.requests.iter().any(
                |request| matches!(request, Request::Sent(info) if info.name() == "notes.bin")
            ),
            "the file is reported sent"
        );
        assert!(
            matches!(log.requests.last(), Some(Request::Finished)),
            "and the session is reported over last"
        );
        assert_eq!(
            frames_of(&log.answered),
            [
                FrameKind::RInit,
                FrameKind::RPos,
                FrameKind::RInit,
                FrameKind::Fin
            ],
            "the receiver answers ZRQINIT, then ZFILE, then ZEOF, then ZFIN"
        );
    }

    /// The header kinds in a stream of raw bytes.
    fn frames_of(bytes: &[u8]) -> Vec<FrameKind> {
        let mut parser = Parser::new();
        parser.feed(bytes);
        let mut kinds = Vec::new();
        while let Some(event) = parser.next_event() {
            if let Event::Header { header, .. } = event {
                kinds.push(header.kind);
            }
        }
        kinds
    }

    /// The run has to be `ZCRCG` throughout and `ZCRCE` only at the end: a
    /// `ZCRCE` in the middle would make the far end wait for a header that never
    /// arrives, and a `ZCRCG` at the end leaves the run open.
    #[test]
    fn every_block_but_the_last_continues_the_run() {
        let body = payload(2500);
        let files = [(info("notes.bin", 2500), body.clone())];
        let mut sender = Sender::new(files.iter().map(|(info, _)| info.clone()));
        let mut receiver = Receiver::new();
        let log = converse(&mut sender, &mut receiver, &files);

        let blocks = blocks(&log.written);
        assert_eq!(blocks.len(), 4, "a name block and three blocks of file");
        assert_eq!(blocks[0].1, Framend::Zcrcw, "the name block ends the frame");
        assert_eq!(
            blocks[1..],
            [
                (1024, Framend::Zcrcg),
                (1024, Framend::Zcrcg),
                (452, Framend::Zcrce),
            ],
            "1024-byte blocks, the last one closing the run"
        );
    }

    /// The one thing a receiver's capabilities legitimately change.
    #[test]
    fn the_check_width_follows_what_the_far_end_advertised() {
        for (capabilities, expected) in [
            (cap::CANFDX, Encoding::Bin16),
            (CAPABILITIES, Encoding::Bin32),
        ] {
            let files = [(info("f.bin", 10), payload(10))];
            let mut sender = Sender::new(files.iter().map(|(info, _)| info.clone()));
            sender.start();
            let offered = deliver(&mut sender, rinit(capabilities));

            let mut parser = Parser::new();
            let mut seen = None;
            for request in &offered {
                if let Request::Send(bytes) = request {
                    parser.feed(bytes);
                    while let Some(event) = parser.next_event() {
                        if let Event::Header { encoding, .. } = event {
                            seen = Some(encoding);
                        }
                    }
                }
            }
            assert_eq!(
                seen,
                Some(expected),
                "a far end that offers no 32-bit checks gets 16-bit ones"
            );
        }
    }

    /// A block the far end has since asked to be resent belongs to the stream it
    /// rejected. Sending it as though it were the block now wanted would put the
    /// wrong bytes at the wrong offset — a file that reassembles to something
    /// plausible and wrong.
    #[test]
    fn a_block_the_far_end_has_since_asked_again_is_dropped() {
        let body = payload(3000);
        let files = [(info("f.bin", 3000), body.clone())];
        let mut sender = Sender::new(files.iter().map(|(info, _)| info.clone()));
        sender.start();
        deliver(&mut sender, rinit(CAPABILITIES));
        deliver(&mut sender, Header::bare(FrameKind::RPos).with_position(0));
        let (first, at, len) = sender.waiting_for().expect("the first block is asked for");
        assert_eq!((at, len), (0, 1024));

        // The far end restarts from the second block, which retires the first.
        deliver(
            &mut sender,
            Header::bare(FrameKind::RPos).with_position(1024),
        );
        let (second, at, len) = sender.waiting_for().expect("a block is asked for again");
        assert_eq!((at, len), (1024, 1024));
        assert_ne!(
            first, second,
            "the token has to change, or the answer is ambiguous"
        );

        assert!(
            sender.supply(first, &body[0..1024]).is_empty(),
            "the overtaken block is dropped"
        );
        assert!(
            !sender.supply(second, &body[1024..2048]).is_empty(),
            "and the block that was actually asked for is not"
        );
    }

    #[test]
    fn a_file_the_far_end_refuses_is_reported_and_the_next_one_is_offered() {
        let files = [
            (info("first.bin", 4), payload(4)),
            (info("second.bin", 4), payload(4)),
        ];
        let mut sender = Sender::new(files.iter().map(|(info, _)| info.clone()));
        sender.start();
        assert_eq!(
            frames(&deliver(&mut sender, rinit(CAPABILITIES))),
            [FrameKind::File]
        );

        let refused = deliver(&mut sender, Header::bare(FrameKind::Skip));
        assert!(
            matches!(refused.first(), Some(Request::Skipped(info)) if info.name() == "first.bin"),
            "the caller is told which file was refused"
        );
        assert_eq!(
            frames(&refused),
            [FrameKind::File],
            "and the next one is offered straight away, without waiting for anything"
        );
    }

    /// The position field is four bytes, so a larger file cannot be described at
    /// all. Refusing beats sending a `ZEOF` the far end would believe.
    #[test]
    fn a_file_past_the_position_field_is_refused_before_any_block_is_asked_for() {
        let mut sender = Sender::new([info("huge.bin", MAX_FILE + 1)]);
        sender.start();
        let out = deliver(&mut sender, rinit(CAPABILITIES));

        assert!(
            matches!(out.first(), Some(Request::Failed(Failure::Unsupported(_)))),
            "the reason is reported"
        );
        assert!(
            !out.iter()
                .any(|request| matches!(request, Request::Read { .. })),
            "and not one byte is read"
        );
        assert!(sender.is_done());
        assert_eq!(
            frames(&out),
            [FrameKind::Can],
            "the far end is told to stop"
        );
    }

    /// An empty file is a real case with a real answer: lrzsz sends `ZDATA`, an
    /// empty `ZCRCE` and `ZEOF`, and a bare `ZDATA` makes `rz` ask again.
    #[test]
    fn an_empty_file_sends_one_empty_block_and_no_data() {
        let files = [(info("empty.txt", 0), Vec::new())];
        let mut sender = Sender::new(files.iter().map(|(info, _)| info.clone()));
        let mut receiver = Receiver::new();
        let log = converse(&mut sender, &mut receiver, &files);

        assert!(
            !log.requests
                .iter()
                .any(|request| matches!(request, Request::Read { .. })),
            "there is nothing to read"
        );
        let blocks = blocks(&log.written);
        assert_eq!(
            blocks.iter().map(|(_, end)| *end).collect::<Vec<_>>(),
            [Framend::Zcrcw, Framend::Zcrce],
            "the name block, then one empty block"
        );
        assert_eq!(blocks[1].0, 0, "and the empty block carries nothing");
        assert!(
            log.instructions.iter().any(
                |instruction| matches!(instruction, Instruction::Finish(info) if info.length == 0)
            ),
            "the receiver still completes a zero-byte file"
        );
    }

    /// The far end can close first. It still has to be answered, and it still
    /// owes the caller nothing — the file in flight is not reported sent.
    #[test]
    fn the_far_end_closing_first_is_still_answered() {
        let files = [(info("f.bin", 2), payload(2))];
        let mut sender = Sender::new(files.iter().map(|(info, _)| info.clone()));
        sender.start();
        deliver(&mut sender, rinit(CAPABILITIES));
        deliver(&mut sender, Header::bare(FrameKind::RPos).with_position(0));
        let (round, _, _) = sender.waiting_for().expect("a block is asked for");
        sender.supply(round, b"ab");

        let out = deliver(&mut sender, Header::bare(FrameKind::Fin));
        assert_eq!(frames(&out), [FrameKind::Fin], "their ZFIN is answered");
        assert!(
            matches!(out.last(), Some(Request::Finished)),
            "and the session is reported over"
        );
        assert!(
            matches!(out.get(out.len() - 2), Some(Request::Send(bytes)) if bytes.as_slice() == OVER_AND_OUT),
            "with lrzsz's sign-off after the closing header"
        );
    }

    /// The closing header goes out once. A sender that answered `ZFIN` with
    /// `ZFIN` and then answered the answer would trade them forever.
    #[test]
    fn the_closing_header_is_written_exactly_once() {
        let body = payload(600);
        let files = [(info("f.bin", 600), body.clone())];
        let mut sender = Sender::new(files.iter().map(|(info, _)| info.clone()));
        let mut receiver = Receiver::new();
        let log = converse(&mut sender, &mut receiver, &files);

        assert_eq!(
            frames_of(&log.written)
                .into_iter()
                .filter(|kind| *kind == FrameKind::Fin)
                .count(),
            1,
            "one ZFIN, not a volley"
        );
        assert!(
            log.written.ends_with(OVER_AND_OUT),
            "and lrzsz's sign-off is the last thing written"
        );
    }

    /// Before a file is offered, a damaged hello costs nothing to repeat. After
    /// one is offered, the same repeat would be a second session.
    #[test]
    fn a_damaged_hello_is_repeated_only_before_a_file_is_offered() {
        let mut sender = Sender::new([info("f.bin", 2)]);
        sender.start();
        let retried = sender.on_event(Event::Corrupt(Corruption::Header));
        assert_eq!(frames(&retried), [FrameKind::RqInit]);

        deliver(&mut sender, rinit(CAPABILITIES));
        assert!(
            sender
                .on_event(Event::Corrupt(Corruption::Header))
                .is_empty(),
            "once a file is offered the dialogue has moved on"
        );
    }
}
