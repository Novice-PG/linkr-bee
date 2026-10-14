//! The ZMODEM framer, replayed against sessions the real `sz` and `rz` held.
//!
//! There is no upstream implementation of this to port and therefore no
//! upstream test to inherit, so correctness is anchored to the reference
//! implementation instead: `dev/zmodem-capture.py` wires a real `sz` to a real
//! `rz` over a socketpair, records every byte in both directions, and verifies
//! the file that came out the far end. The recordings are checked in under
//! `tests/fixtures/zmodem/`, and this suite decodes them.
//!
//! That makes these tests unusually strong for a codec: the bytes were not
//! written by this code, the field order was not chosen by this code, and the
//! expected outcome is not "whatever we produced last time". A mistake in the
//! escape set, the field reversal, either CRC algorithm, either CRC byte order,
//! the subpacket framing, or the `ZCRCG`/`ZCRCE` continuation rule shows up here
//! as either a corrupt frame or a payload that does not match.
//!
//! The recordings are small on purpose — the largest is 4.4 KB — so that they
//! can be read in a review rather than taken on trust. Regenerate them with
//! `python3 dev/zmodem-capture.py`, which asserts the round trip before it
//! writes anything.

use linkr_tui::transfer::zmodem::{Engine, Event as EngineEvent};
use linkr_tui::zmodem::{
    Corruption, Encoding, Event, FileInfo, FrameKind, Framend, Instruction, Parser, Receiver,
    Request, Sender,
};

use std::collections::VecDeque;

fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/fixtures/zmodem/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|err| panic!("reading {path}: {err}"))
}

/// Feed a whole recording, in chunks of `chunk` bytes, and collect everything.
fn decode_at(chunk: usize, stream: &[u8]) -> (Vec<Event>, Parser) {
    let mut parser = Parser::new();
    let mut events = Vec::new();
    for piece in stream.chunks(chunk.max(1)) {
        parser.feed(piece);
        while let Some(event) = parser.next_event() {
            events.push(event);
        }
    }
    (events, parser)
}

fn headers(events: &[Event]) -> Vec<(FrameKind, Encoding)> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Header { header, encoding } => Some((header.kind, *encoding)),
            _ => None,
        })
        .collect()
}

fn payloads(events: &[Event]) -> Vec<(Vec<u8>, Framend)> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Subpacket { data, end } => Some((data.clone(), *end)),
            _ => None,
        })
        .collect()
}

fn corruption(events: &[Event]) -> Option<Corruption> {
    events.iter().find_map(|event| match event {
        Event::Corrupt(what) => Some(*what),
        _ => None,
    })
}

/// The dialogue a 4112-byte download actually had, in order.
///
/// This is the assertion that would have caught the two mistakes that are easy
/// to make here and hard to see: treating the continuation subpackets as needing
/// a header each (which would stall after the first block), and reading the
/// frameend bytes as ordinary data (which would swallow the rest of the file).
#[test]
fn the_recorded_download_decodes_to_the_dialogue_it_really_had() {
    let stream = fixture("basic-sz2rz.bin");
    let (events, parser) = decode_at(4096, &stream);

    assert_eq!(corruption(&events), None, "every frame must check out");
    assert_eq!(
        headers(&events),
        [
            (FrameKind::RqInit, Encoding::Hex),
            (FrameKind::File, Encoding::Bin32),
            (FrameKind::Data, Encoding::Bin32),
            (FrameKind::Eof, Encoding::Bin32),
            (FrameKind::Fin, Encoding::Hex),
        ]
    );

    // One subpacket of name and metadata, then five blocks of file: 4 x 1024
    // and a final 16, and the frameends say which of them continue the run.
    let blocks = payloads(&events);
    assert_eq!(blocks.len(), 6, "a name block plus five data blocks");
    assert_eq!(blocks[0].1, Framend::Zcrcw, "the name block ends the frame");
    assert_eq!(
        blocks[1..5].iter().map(|(_, end)| *end).collect::<Vec<_>>(),
        [Framend::Zcrcg; 4],
        "the bulk of the file is one unbroken run"
    );
    assert_eq!(blocks[5].1, Framend::Zcrce, "and the last block closes it");

    assert_eq!(blocks[1].0.len(), 1024);
    assert_eq!(blocks[5].0.len(), 16);
    // The width tracks the last header seen, so after the closing ZFIN it is
    // the hex form's 16 bits; that the four data blocks decoded at all is the
    // real evidence that the sender's Bin32 headers announced 32-bit checks.
    assert_eq!(
        parser.encoding(),
        Encoding::Hex,
        "the session closed on ZFIN"
    );
    assert_eq!(parser.stats().corrupt, 0);
    assert_eq!(parser.stats().subpackets, 6);
    assert_eq!(parser.stats().headers, 5);
}

/// The payload the sender meant to send, reassembled from the subpackets.
#[test]
fn the_reassembled_payload_is_byte_for_byte_the_file_that_was_sent() {
    let expected = fixture("basic-payload.bin");
    let (events, parser) = decode_at(4096, &fixture("basic-sz2rz.bin"));

    let file: Vec<u8> = payloads(&events)
        .into_iter()
        .skip(1) // the first subpacket describes the file; the rest are it
        .flat_map(|(data, _)| data)
        .collect();

    assert_eq!(file.len(), expected.len());
    assert_eq!(file, expected);
    // The tally counts the name block as payload too, since it is handed to the
    // caller the same way; it is 45 bytes here.
    assert_eq!(parser.stats().payload, (expected.len() + 45) as u64);

    // And the sender's own end-of-file header agrees about the size.
    let eof = events
        .iter()
        .find_map(|event| match event {
            Event::Header { header, .. } if header.kind == FrameKind::Eof => Some(header),
            _ => None,
        })
        .expect("a ZEOF");
    assert_eq!(u64::from(eof.position()), expected.len() as u64);
}

/// The name block is the only part of the protocol with a grammar, and it is
/// parsed here with every field checked — including the two octal ones, which
/// read perfectly well as decimal and would then be silently wrong.
#[test]
fn the_name_block_describes_the_file_the_sender_offered() {
    let (events, _) = decode_at(4096, &fixture("basic-sz2rz.bin"));
    let (block, _) = payloads(&events).into_iter().next().expect("a name block");
    let info = linkr_tui::zmodem::FileInfo::parse(&block).expect("a parseable control block");

    assert_eq!(info.name(), "payload.bin");
    assert_eq!(info.length, fixture("basic-payload.bin").len() as u64);
    assert!(info.is_regular());
    assert_eq!(
        info.mode & 0o777,
        0o664,
        "the permissions the sender's own umask produced"
    );
    assert_eq!(info.serial, 0);
    assert_eq!(info.remaining, 1);
    assert!(
        (1_600_000_000..2_000_000_000).contains(&info.modified),
        "the octal modification time is a real timestamp, not 15263574505 decimal"
    );
}

/// `sz -o` narrows the check to 16 bits and announces it by writing `* ZDLE A`
/// instead of `* ZDLE C`. Same payload, same framing rules, different check.
#[test]
fn the_narrow_check_recording_decodes_with_the_narrow_check() {
    let stream = fixture("crc16-sz2rz.bin");
    let (events, parser) = decode_at(64, &stream);

    assert_eq!(corruption(&events), None, "every frame must check out");
    assert_eq!(
        headers(&events)
            .into_iter()
            .filter(|(kind, _)| *kind != FrameKind::RqInit && *kind != FrameKind::Fin)
            .map(|(_, encoding)| encoding)
            .collect::<Vec<_>>(),
        [Encoding::Bin16, Encoding::Bin16, Encoding::Bin16],
        "the sender announced 16-bit checks for everything it sent in binary"
    );

    let file: Vec<u8> = payloads(&events)
        .into_iter()
        .skip(1)
        .flat_map(|(data, _)| data)
        .collect();
    assert_eq!(
        file,
        fixture("basic-payload.bin"),
        "the same bytes, a different check"
    );
    assert_eq!(parser.stats().corrupt, 0);
}

/// A short file in one subpacket, whose payload is mostly bytes the escape
/// layer has to touch: `ZDLE` itself, a carriage return, XON, XOFF, NUL, and
/// high bytes including the ones lrzsz escapes with the top bit set.
#[test]
fn the_hostile_payload_survives_escaping_intact() {
    let (events, parser) = decode_at(1, &fixture("hostile-small-sz2rz.bin"));

    assert_eq!(corruption(&events), None);
    let file: Vec<u8> = payloads(&events)
        .into_iter()
        .skip(1)
        .flat_map(|(data, _)| data)
        .collect();
    assert_eq!(file, fixture("hostile-small-payload.bin"));

    assert!(
        parser.stats().discarded < 40,
        "only the wakeup string and header tail are noise, not {} bytes",
        parser.stats().discarded
    );
    assert_eq!(parser.stats().corrupt, 0);
}

/// The receiver's half of the recording: the headers `rz` sent back. Their
/// contents are what a sender has to read to know the receiver is ready, which
/// is why the capability byte is asserted rather than the whole frame.
#[test]
fn the_recorded_replies_are_the_headers_a_receiver_sends() {
    let (events, _) = decode_at(3, &fixture("basic-rz2sz.bin"));
    assert_eq!(corruption(&events), None);

    assert_eq!(
        headers(&events),
        [
            (FrameKind::RInit, Encoding::Hex),
            (FrameKind::RInit, Encoding::Hex),
            (FrameKind::RPos, Encoding::Hex),
            (FrameKind::RInit, Encoding::Hex),
            (FrameKind::Fin, Encoding::Hex),
        ],
        "a receiver opens with ZRINIT, answers a restart with ZRPOS, and closes with ZFIN"
    );

    let rinit = events
        .iter()
        .find_map(|event| match event {
            Event::Header { header, .. } if header.kind == FrameKind::RInit => Some(header),
            _ => None,
        })
        .expect("a ZRINIT");
    assert_eq!(
        rinit.f0(),
        0x23,
        "full duplex, overlapped I/O, and 32-bit checks"
    );
    assert_ne!(rinit.f0() & linkr_tui::zmodem::cap::CANFC32, 0);
    assert_eq!(
        [rinit.f1(), rinit.f2(), rinit.f3()],
        [0, 0, 0],
        "rz named no buffer size, so the sender keeps its own"
    );

    // The restart request names offset zero, i.e. from the beginning.
    let rpos = events
        .iter()
        .find_map(|event| match event {
            Event::Header { header, .. } if header.kind == FrameKind::RPos => Some(header),
            _ => None,
        })
        .expect("a ZRPOS");
    assert_eq!(rpos.position(), 0);
}

/// How the recording arrives must not matter. A link is free to split a frame
/// anywhere, and a BLE notification is 512 bytes at best.
#[test]
fn the_decoded_dialogue_does_not_depend_on_how_the_bytes_are_split() {
    let stream = fixture("basic-sz2rz.bin");
    let (whole, _) = decode_at(stream.len(), &stream);
    let reference = headers(&whole);

    for chunk in [1, 2, 3, 7, 13, 64, 511, 512, 1024, 1025, 4399] {
        let (events, parser) = decode_at(chunk, &stream);
        assert_eq!(
            headers(&events),
            reference,
            "chunks of {chunk} decoded a different dialogue"
        );
        assert_eq!(
            corruption(&events),
            None,
            "chunks of {chunk} corrupted a frame"
        );
        assert_eq!(parser.stats().corrupt, 0, "chunks of {chunk}");
    }

    let payload = |events: &[Event]| -> Vec<u8> {
        payloads(events)
            .into_iter()
            .skip(1)
            .flat_map(|(data, _)| data)
            .collect()
    };
    assert_eq!(payload(&whole), fixture("basic-payload.bin"));
    let (odd, _) = decode_at(37, &stream);
    assert_eq!(payload(&odd), payload(&whole));
}

/// The parser's own claims about itself, checked on real traffic rather than on
/// a synthetic stream: nothing is left buffered, and the accounting adds up.
#[test]
fn the_framer_finishes_clean_and_its_tally_matches_the_recording() {
    let stream = fixture("basic-sz2rz.bin");
    let (_, parser) = decode_at(512, &stream);

    assert_eq!(parser.buffered(), 0, "nothing is stuck mid-frame");
    assert!(!parser.collecting(), "and no subpacket is left open");

    let stats = parser.stats();
    assert_eq!(stats.headers, 5);
    assert_eq!(stats.subpackets, 6);
    assert_eq!(stats.corrupt, 0);
    // The file, plus the name block, which is payload handed to the caller too.
    assert_eq!(
        stats.payload,
        (fixture("basic-payload.bin").len() + 45) as u64
    );
    // Eight bytes of the recording are not payload and not framing: `rz\r` and
    // the pad that makes the opening `**` a synchronising pair, a stray `XON`
    // after the name block, the extra pad before the closing `**`, and lrzsz's
    // `OO` over-and-out. Anything more than that would mean input being lost.
    assert_eq!(stats.discarded, 8);

    // Everything else the recording contains is framing — markers, checks, and
    // the escapes that make bytes safe — which the tally deliberately does not
    // count. 240 bytes of it, for this session: if the framer were quietly
    // dropping input, the shortfall would be much larger than that.
    let framing = stream.len() as u64 - stats.discarded - stats.payload;
    assert!(
        (200..300).contains(&framing),
        "expected about 240 bytes of framing, found {framing}"
    );
}

/// Decode the bytes a run of instructions wanted to put on the wire, and report
/// the headers in them. Going back through the parser proves the replies are
/// well-formed frames rather than merely the right enum variants.
fn reply_headers(instructions: &[Instruction]) -> Vec<(FrameKind, Encoding)> {
    let mut parser = Parser::new();
    let mut kinds = Vec::new();
    for instruction in instructions {
        let Instruction::Send(bytes) = instruction else {
            continue;
        };
        parser.feed(bytes);
        while let Some(event) = parser.next_event() {
            if let Event::Header { header, encoding } = event {
                kinds.push((header.kind, encoding));
            }
        }
    }
    kinds
}

/// The whole point of the dialogue machine, tested the only way that really
/// counts: the bytes a real `sz` sent go in, and the replies that come out are
/// compared against the bytes a real `rz` sent back in the same session.
///
/// The recordings were made in two directions of one socketpair, so the two
/// halves are causally linked rather than two independent fixtures that happen
/// to agree. Nothing here was written by this code — not an input frame, not an
/// expected reply — so a receiver that invented its own dialogue, answered a
/// `ZFILE` with a `ZACK` instead of a `ZRPOS`, or acknowledged the blocks of a
/// `ZCRCG` run would be caught.
///
/// The bytes are compared as *decoded headers* rather than as octets because
/// this implementation deliberately writes the documented `CR LF XON` hex tail
/// where lrzsz writes `CR 0x8A XON` (see `zmodem/mod.rs`). A real `sz` accepts
/// both; comparing octets would test that difference rather than the dialogue.
#[test]
fn the_receiver_reproduces_the_replies_the_real_rz_sent() {
    let (incoming, _) = decode_at(4096, &fixture("basic-sz2rz.bin"));
    let (recorded, _) = decode_at(3, &fixture("basic-rz2sz.bin"));

    let mut receiver = Receiver::new();
    let mut ours = reply_headers(&receiver.announce());
    for event in incoming {
        ours.extend(reply_headers(&receiver.on_event(event)));
    }

    assert_eq!(
        ours,
        headers(&recorded),
        "the dialogue this receiver holds must be the one a real rz holds"
    );
    assert!(receiver.is_done(), "ZFIN closes the session on both sides");

    // The announcement is compared field by field too: a receiver that offered
    // different capabilities would get a different sender behaviour — `sz -o`
    // narrows its checks when 32-bit ones are not offered.
    assert_eq!(
        ours.first().copied(),
        Some((FrameKind::RInit, Encoding::Hex)),
        "a receiver opens in hex, so the reverse channel cannot be corrupted by data"
    );
    let announcement = receiver_zrinit();
    assert_eq!(
        announcement.f0(),
        linkr_tui::zmodem::CAPABILITIES,
        "the advertised capabilities are the ones the module claims"
    );
    assert_ne!(
        announcement.f0() & linkr_tui::zmodem::cap::CANFC32,
        0,
        "32-bit checks are offered, so a real sz keeps them"
    );
}

/// The first `ZRINIT` this receiver would send, decoded.
fn receiver_zrinit() -> linkr_tui::zmodem::Header {
    let mut receiver = Receiver::new();
    let instructions = receiver.announce();
    let Some(Instruction::Send(bytes)) = instructions.first() else {
        panic!("a receiver announces itself first");
    };
    let mut parser = Parser::new();
    parser.feed(bytes);
    match parser.next_event() {
        Some(Event::Header { header, .. }) => header,
        other => panic!("the announcement is a header, got {other:?}"),
    }
}

/// And the file itself, reconstructed by the receiver from the real sender's
/// traffic — the end-to-end claim, rather than the codec's.
#[test]
fn the_receiver_rebuilds_the_sent_file_from_the_real_traffic() {
    let (incoming, _) = decode_at(512, &fixture("basic-sz2rz.bin"));
    let mut receiver = Receiver::new();
    receiver.announce();

    let mut named = None;
    let mut file = Vec::new();
    let mut finished = None;
    for event in incoming {
        for instruction in receiver.on_event(event) {
            match instruction {
                Instruction::Start(info) => named = Some(info),
                Instruction::Chunk(data) => file.extend(data),
                Instruction::Finish(info) => finished = Some(info),
                Instruction::Fail(why) => panic!("the real session must not fail: {why:?}"),
                Instruction::Resume { at } => panic!("nothing needed resuming, got {at}"),
                Instruction::Send(_) => {}
            }
        }
    }

    let expected = fixture("basic-payload.bin");
    assert_eq!(file, expected, "the file arrived byte for byte");
    let named = named.expect("the sender named the file");
    assert_eq!(named.name(), "payload.bin");
    let finished = finished.expect("the sender said end-of-file");
    assert_eq!(
        finished.length,
        expected.len() as u64,
        "and the length the caller stores is the one that actually arrived"
    );
    assert_eq!(receiver.received(), 0, "the file is no longer in flight");
}

// ---------------------------------------------------------------------------
// The sender, against the receiver that judged it
//
// `dev/zmodem-send-check.py` wires `examples/zmodem-send.rs` to a real `rz` over
// a socketpair, records both directions, and checks the file that came out the
// far end. The recordings are checked in, and these tests replay them. Unlike
// the download fixtures, these were not produced by lrzsz on one side and this
// code on the other: the replies are a real receiver's, and the bytes they have
// to provoke are the bytes this sender really wrote.
// ---------------------------------------------------------------------------

/// The first subpacket of a stream — for both these streams, the name block.
fn first_block(stream: &[u8]) -> Vec<u8> {
    let (events, _) = decode_at(4096, stream);
    payloads(&events)
        .into_iter()
        .next()
        .expect("a name block")
        .0
}

/// Feed every request the sender makes, reading the file's contents for it.
fn serve(sender: &mut Sender, pending: &mut VecDeque<Request>, written: &mut Vec<u8>, body: &[u8]) {
    let mut steps = 0;
    while let Some(request) = pending.pop_front() {
        steps += 1;
        assert!(
            steps < 100_000,
            "a dialogue that does not end is not a dialogue"
        );
        match request {
            Request::Send(bytes) => written.extend_from_slice(&bytes),
            Request::Read { round, at, len } => {
                let slice = body[at as usize..at as usize + len].to_vec();
                let more = sender.supply(round, &slice);
                pending.extend(more);
            }
            _ => {}
        }
    }
}

/// The sender's whole job, in one assertion: give it the replies a real `rz`
/// sent, and it must write the exact bytes that real `rz` provoked.
///
/// This is stronger than a self-consistency test in a way that matters. The
/// expected bytes were not chosen here; the file they carry was written to disk
/// by lrzsz and hash-checked against the original. So a mistake in the block
/// size, the `ZCRCG`/`ZCRCE` rule, the `ZFILE` flag byte, the check width, the
/// name block's grammar, or the order of the whole dialogue shows up as a
/// difference rather than as a passing test.
#[test]
fn the_sender_writes_exactly_the_bytes_a_real_rz_drove_it_to_write() {
    for name in ["basic", "empty"] {
        let sent = fixture(&format!("sender-{name}-sent.bin"));
        let reply = fixture(&format!("sender-{name}-reply.bin"));
        let body = fixture(&format!("sender-{name}-payload.bin"));

        // The file description comes out of the live session's own name block,
        // so the replay is driven by what really went on the wire.
        let info = FileInfo::parse(&first_block(&sent)).expect("a control block");
        let mut sender = Sender::new([info]);
        let mut written = Vec::new();
        let mut pending: VecDeque<Request> = sender.start().into();
        serve(&mut sender, &mut pending, &mut written, &body);

        for event in decode_at(1, &reply).0 {
            pending.extend(sender.on_event(event));
            serve(&mut sender, &mut pending, &mut written, &body);
        }

        assert!(sender.is_done(), "{name}: the session has to close");
        assert_eq!(
            written, sent,
            "{name}: replaying a real receiver's replies must reproduce the real stream"
        );
    }
}

/// The two binary headers a real `sz` wrote, read off `basic-sz2rz.bin`, where
/// each is the only header of its type. `ZFILE` and `ZDATA` are the two whose
/// contents do not depend on the file, so ours has to be identical to the octet.
#[test]
fn our_binary_headers_are_the_octets_the_real_sz_wrote() {
    const REAL_ZFILE: &[u8] = &[
        0x2a, 0x18, 0x43, 0x04, 0x00, 0x00, 0x00, 0x01, 0x4b, 0x61, 0xa5, 0x44,
    ];
    const REAL_ZDATA: &[u8] = &[
        0x2a, 0x18, 0x43, 0x0a, 0x00, 0x00, 0x00, 0x00, 0xbc, 0xef, 0x92, 0x8c,
    ];

    let ours = fixture("sender-basic-sent.bin");
    for (what, frame) in [("ZFILE", REAL_ZFILE), ("ZDATA", REAL_ZDATA)] {
        assert!(
            ours.windows(frame.len()).any(|window| window == frame),
            "our {what} header must be byte for byte the one a real sz wrote"
        );
    }
}

/// lrzsz writes one field more than the protocol documents: a repeat of the
/// length, so its block for a 4112-byte file ends `… 1 4112 NUL` while ours ends
/// `… 1 NUL`. That is the entire difference between a 45-byte block and our
/// 40-byte one, and it is worth pinning down rather than leaving as a suspicion:
/// it is the reason `Sender` can build a block without knowing lrzsz's habit, and
/// the reason the live check is the one that proves a real `rz` accepts it.
#[test]
fn lrzsz_ends_its_name_block_with_a_repeat_of_the_length_that_we_omit() {
    let ours = first_block(&fixture("sender-basic-sent.bin"));
    let theirs = first_block(&fixture("basic-sz2rz.bin"));

    let repeated = FileInfo::parse(&theirs).expect("a control block").length;
    let tail = format!(" {repeated}\0");
    assert!(
        theirs.ends_with(tail.as_bytes()),
        "lrzsz repeats the length as its last field"
    );
    assert!(!ours.ends_with(tail.as_bytes()), "we do not");
    assert_eq!(
        ours.len() + tail.len() - 1,
        theirs.len(),
        "and the repeat, sharing the terminating NUL, is the whole difference in length"
    );

    // Both are read by the same reader as the same file, which is what makes the
    // shorter form safe.
    let ours = FileInfo::parse(&ours).expect("a control block");
    let theirs = FileInfo::parse(&theirs).expect("a control block");
    assert_eq!(ours.name(), theirs.name());
    assert!(ours.is_regular() && theirs.is_regular());
    assert_eq!(ours.mode & 0o7777, theirs.mode & 0o7777);
}

/// The empty file is its own case: the sender has to decide whether a `ZDATA`
/// header is worth writing when there is nothing to put behind it. lrzsz writes
/// one and follows it with an empty block; a bare header makes a real `rz` ask
/// for the file again instead of moving on, which is recorded here.
#[test]
fn the_empty_session_uses_a_header_and_one_empty_block_like_lrzs() {
    let theirs = decode_at(4096, &fixture("empty-sz2rz.bin")).0;
    let ours = decode_at(4096, &fixture("sender-empty-sent.bin")).0;

    // The data blocks only: the name blocks differ in length for the reason the
    // test above records (lrzsz repeats the length), while the framing is what
    // this test is about.
    let bodies = |events: &[Event]| -> Vec<(usize, Framend)> {
        payloads(events)
            .into_iter()
            .skip(1)
            .map(|(data, end)| (data.len(), end))
            .collect()
    };
    assert_eq!(
        bodies(&ours),
        bodies(&theirs),
        "one empty block, ended the way lrzsz ends it"
    );
    assert_eq!(
        bodies(&ours),
        [(0, Framend::Zcrce)],
        "an empty block that closes the run"
    );

    let name_end = |events: &[Event]| payloads(events).first().map(|(_, end)| *end);
    assert_eq!(
        name_end(&ours),
        name_end(&theirs),
        "and the name block ends the frame the same way"
    );

    let eof_position = |events: &[Event]| -> Option<u32> {
        events.iter().find_map(|event| match event {
            Event::Header { header, .. } if header.kind == FrameKind::Eof => {
                Some(header.position())
            }
            _ => None,
        })
    };
    assert_eq!(
        eof_position(&theirs),
        Some(0),
        "the real sz declares nothing"
    );
    assert_eq!(eof_position(&ours), eof_position(&theirs), "and so do we");
}

// --- the download as a whole, not just its frames ---------------------------

/// The codec above finishes with the payload in memory; this is the rest of the
/// way: the same real `sz` traffic goes through the [`Engine`] and has to become
/// a file on disk. Nothing here writes that input, so neither the name that gets
/// opened nor the bytes that land in it are this code's own idea.
///
/// The awkward part of a receive lives below the codec — a file opened under a
/// name chosen by whoever is on the other end of the wire — and that is the part
/// a codec test cannot reach no matter how many real bytes it replays.
#[test]
fn a_real_sz_stream_lands_a_file_on_disk() {
    let dir = std::env::temp_dir().join(format!("linkr-engine-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory to receive into");

    let mut engine = Engine::receiving(dir.clone());
    let mut answers = engine.start();
    for piece in fixture("basic-sz2rz.bin").chunks(700) {
        answers.extend(engine.feed(piece));
    }

    assert!(engine.is_done(), "the session the recording holds is over");
    let saved = engine.take_events();
    let saved: Vec<_> = saved
        .iter()
        .filter_map(|event| match event {
            EngineEvent::Saved { name, bytes } => Some((name.as_str(), *bytes)),
            _ => None,
        })
        .collect();
    assert_eq!(saved.len(), 1, "one file in the recording: {saved:?}");

    // The name is the one `sz` chose, reduced to a component — see `safe_name`.
    let (name, bytes) = saved[0];
    let landed = std::fs::read(dir.join(name))
        .unwrap_or_else(|err| panic!("nothing named {name} landed: {err}"));
    let expected = fixture("basic-payload.bin");
    assert_eq!(
        landed.len(),
        expected.len(),
        "the whole file arrived, not part of it"
    );
    assert!(
        landed == expected,
        "and it is byte for byte the real payload"
    );
    assert_eq!(bytes as usize, expected.len(), "as the event counted it");

    // Every reply the recording provoked is ours; none of them were written by
    // this code, and the fact that `sz` was driven by them is why they match.
    assert!(
        !answers.is_empty(),
        "the engine answered the far end throughout"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// An empty file is where a receiver's plumbing is most likely to be wrong: no
/// bytes ever reach the writer, so anything that opens a file "later" opens
/// nothing. The recording has a real `sz` sending one.
#[test]
fn a_real_sz_stream_lands_an_empty_file_too() {
    let dir = std::env::temp_dir().join(format!("linkr-engine-empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory to receive into");

    let mut engine = Engine::receiving(dir.clone());
    let _answers = engine.start();
    for piece in fixture("empty-sz2rz.bin").chunks(700) {
        let _ = engine.feed(piece);
    }

    assert!(engine.is_done(), "the recorded session is over");
    let events = engine.take_events();
    let saved: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            EngineEvent::Saved { name, bytes } => Some((name.as_str(), *bytes)),
            _ => None,
        })
        .collect();
    assert_eq!(saved.len(), 1, "one empty file in the recording: {saved:?}");
    assert_eq!(saved[0].1, 0, "and nothing went into it: {saved:?}");

    let entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("the directory is readable")
        .filter_map(|entry| entry.ok())
        .collect();
    // The file has to exist. An empty file that never got opened and an empty
    // file that arrived look the same to anything comparing bytes, which is
    // exactly why this counts entries instead.
    assert_eq!(entries.len(), 1, "one empty file landed: {entries:?}");
    assert_eq!(
        std::fs::read(entries[0].path()).expect("the landed file reads"),
        Vec::<u8>::new(),
        "and it is empty, which is what was sent"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
