//! Reliable UART Service v1 framing, sequence handling and de-duplication.
//! See docs/LINKR_BLE_API.zh-CN.md section 7.

use crate::event::NoticeLevel;

pub const UART_HEADER_SIZE: usize = 12;
pub const UART_MAGIC: [u8; 2] = *b"LR";
pub const UART_VERSION: u8 = 1;
/// 12-byte header + 232-byte payload = 244 bytes, one ATT packet at MTU 247.
pub const UART_MAX_PAYLOAD_CAP: usize = 232;
pub const UART_ATT_MAX: usize = 244;

/// A sequence gap the caller must treat as fatal for the stream (mirror of the
/// Python warning `reliable UART sequence gap: expected X, got Y`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartGap {
    pub expected: u32,
    pub got: u32,
}

impl std::fmt::Display for UartGap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "reliable UART sequence gap: expected {}, got {}",
            self.expected, self.got
        )
    }
}

impl std::error::Error for UartGap {}

/// `--debug-io` trace for one ATT chunk: `UART TX #1 b'...'` or
/// `UART TX #1 <redacted 20 bytes>`.
pub fn tx_trace(sequence: u32, chunk: &[u8], sensitive: bool) -> String {
    if sensitive {
        format!("UART TX #{sequence} <redacted {} bytes>", chunk.len())
    } else {
        format!(
            "UART TX #{sequence} {}",
            crate::protocol::validate::python_repr_bytes(chunk)
        )
    }
}

struct PartialFrame {
    sequence: u32,
    expected: usize,
    payload: Vec<u8>,
}

pub struct UartCodec {
    max_payload: usize,
    tx_sequence: u32,
    rx_sequence: u32,
    current: Option<PartialFrame>,
    notices: Vec<(NoticeLevel, String)>,
}

impl UartCodec {
    pub fn new(max_payload: u16, tx_sequence: u32, rx_sequence: u32) -> Self {
        Self {
            max_payload: (max_payload as usize).clamp(1, UART_MAX_PAYLOAD_CAP),
            tx_sequence: if tx_sequence == 0 { 1 } else { tx_sequence },
            rx_sequence: if rx_sequence == 0 { 1 } else { rx_sequence },
            current: None,
            notices: Vec::new(),
        }
    }

    /// The sequence that follows `sequence` (0xFFFFFFFF wraps to 1).
    pub fn next_sequence(sequence: u32) -> u32 {
        if sequence == u32::MAX {
            1
        } else {
            sequence + 1
        }
    }

    pub fn tx_sequence(&self) -> u32 {
        self.tx_sequence
    }

    pub fn rx_sequence(&self) -> u32 {
        self.rx_sequence
    }

    /// The device-advertised payload cap this codec enforces (1..=232).
    pub fn max_payload(&self) -> usize {
        self.max_payload
    }

    /// Notices the Python channel would have printed (`info`/`warn`) during
    /// `on_indication`; the session drains them into `CoreEvent::Notice`s.
    pub fn take_notices(&mut self) -> Vec<(NoticeLevel, String)> {
        std::mem::take(&mut self.notices)
    }

    /// Split `data` into logical frames of at most `max_payload` bytes, each
    /// framed with the current tx sequence (one sequence per logical frame),
    /// then chunk each frame into ATT writes of `att_size` (>= 20).
    pub fn encode_write(&mut self, data: &[u8], att_size: usize) -> Vec<Vec<u8>> {
        let mut chunks = Vec::new();
        if data.is_empty() {
            return chunks;
        }
        // Python: att_size = max(20, min(write_size, 244)).
        let att_size = att_size.clamp(20, UART_ATT_MAX);
        let mut offset = 0;
        while offset < data.len() {
            let end = (offset + self.max_payload).min(data.len());
            let payload = &data[offset..end];
            let sequence = self.tx_sequence;

            let mut frame = Vec::with_capacity(UART_HEADER_SIZE + payload.len());
            frame.extend_from_slice(&UART_MAGIC);
            frame.push(UART_VERSION);
            frame.push(0); // flags: ignored on RX, always 0 on TX
            frame.extend_from_slice(&sequence.to_le_bytes());
            frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
            frame.extend_from_slice(&0u16.to_le_bytes()); // reserved
            frame.extend_from_slice(payload);
            chunks.extend(frame.chunks(att_size).map(<[u8]>::to_vec));

            // The sequence advances only after the whole frame is written.
            self.tx_sequence = Self::next_sequence(sequence);
            offset = end;
        }
        chunks
    }

    /// Feed one indication chunk. Returns the complete frame payload, or a
    /// sequence gap the caller must surface, or `None` for partial frames and
    /// dropped duplicates.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Option<Vec<u8>>, UartGap> {
        let mut fragment = bytes;
        if self.current.is_none() {
            if fragment.len() < UART_HEADER_SIZE || fragment[..2] != UART_MAGIC {
                self.notices.push((
                    NoticeLevel::Info,
                    "reliable UART <- orphaned fragment".to_string(),
                ));
                return Ok(None);
            }
            let version = fragment[2];
            let sequence = u32::from_le_bytes(fragment[4..8].try_into().expect("4 bytes"));
            let expected = u16::from_le_bytes(fragment[8..10].try_into().expect("2 bytes"));
            if version != UART_VERSION
                || sequence == 0
                || expected == 0
                || expected as usize > self.max_payload
            {
                self.notices.push((
                    NoticeLevel::Warn,
                    "reliable UART <- invalid frame header".to_string(),
                ));
                return Ok(None);
            }
            self.current = Some(PartialFrame {
                sequence,
                expected: expected as usize,
                payload: Vec::with_capacity(expected as usize),
            });
            fragment = &fragment[UART_HEADER_SIZE..];
        }

        let oversized = {
            let current = self.current.as_mut().expect("current frame");
            current.payload.len() + fragment.len() > current.expected
        };
        if oversized {
            // Oversized: drop the whole message, header included.
            self.notices.push((
                NoticeLevel::Warn,
                "reliable UART <- oversized frame".to_string(),
            ));
            self.current = None;
            return Ok(None);
        }
        let current = self.current.as_mut().expect("current frame");
        current.payload.extend_from_slice(fragment);
        if current.payload.len() != current.expected {
            return Ok(None);
        }
        let frame = self.current.take().expect("complete frame");

        let previous = if self.rx_sequence == 1 {
            u32::MAX
        } else {
            self.rx_sequence - 1
        };
        if frame.sequence == previous {
            // Duplicate of the last accepted frame: silent drop.
            return Ok(None);
        }
        if frame.sequence != self.rx_sequence {
            return Err(UartGap {
                expected: self.rx_sequence,
                got: frame.sequence,
            });
        }
        self.rx_sequence = Self::next_sequence(self.rx_sequence);
        Ok(Some(frame.payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uart_frame(sequence: u32, payload: &[u8]) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(&UART_MAGIC);
        frame.push(UART_VERSION);
        frame.push(0);
        frame.extend_from_slice(&sequence.to_le_bytes());
        frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        frame.extend_from_slice(&0u16.to_le_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    fn codec(max_payload: u16) -> UartCodec {
        UartCodec::new(max_payload, 1, 1)
    }

    // ---- ReliableUartTests ----------------------------------------------

    #[test]
    fn frame_is_reassembled_and_delivered_once() {
        let mut channel = codec(232);
        let delivered = channel.feed(&uart_frame(1, b"hello")).unwrap();
        assert_eq!(delivered, Some(b"hello".to_vec()));
        assert_eq!(channel.rx_sequence(), 2);
    }

    #[test]
    fn fragmented_frame_waits_for_the_rest() {
        let mut channel = codec(232);
        let frame = uart_frame(1, b"hello world");
        assert_eq!(channel.feed(&frame[..14]).unwrap(), None);
        assert_eq!(
            channel.feed(&frame[14..]).unwrap(),
            Some(b"hello world".to_vec())
        );
        assert_eq!(channel.rx_sequence(), 2);
    }

    #[test]
    fn duplicate_sequence_is_dropped_silently() {
        let mut channel = codec(232);
        let frame = uart_frame(1, b"one");
        assert_eq!(channel.feed(&frame).unwrap(), Some(b"one".to_vec()));
        assert_eq!(channel.feed(&frame).unwrap(), None);
        assert_eq!(channel.rx_sequence(), 2);
        assert!(channel.take_notices().is_empty());
    }

    #[test]
    fn sequence_gap_is_reported_and_not_delivered() {
        let mut channel = codec(232);
        let error = channel
            .feed(&uart_frame(5, b"skipped"))
            .expect_err("gap must be reported");
        assert_eq!(
            error,
            UartGap {
                expected: 1,
                got: 5
            }
        );
        assert_eq!(
            error.to_string(),
            "reliable UART sequence gap: expected 1, got 5"
        );
        assert_eq!(channel.rx_sequence(), 1);
        // Every later frame keeps reporting a gap until rx_sequence arrives.
        let error = channel.feed(&uart_frame(6, b"later")).unwrap_err();
        assert_eq!(error.expected, 1);
        assert_eq!(error.got, 6);
        // ...and the stream recovers when the expected sequence shows up.
        assert_eq!(
            channel.feed(&uart_frame(1, b"ok")).unwrap(),
            Some(b"ok".to_vec())
        );
        assert_eq!(channel.rx_sequence(), 2);
    }

    #[test]
    fn write_uses_one_sequence_per_frame() {
        let mut channel = UartCodec::new(8, 1, 1);
        let chunks = channel.encode_write(b"abcdefghijklmnopqr", 20);
        assert_eq!(channel.tx_sequence(), 4);

        // Re-parse the ATT stream the way the Python test does: every chunk
        // concatenates back into framed messages.
        let mut buffer = Vec::new();
        for chunk in &chunks {
            buffer.extend_from_slice(chunk);
        }
        let mut frames = Vec::new();
        let mut offset = 0;
        while buffer.len() - offset >= UART_HEADER_SIZE {
            let header = &buffer[offset..offset + UART_HEADER_SIZE];
            assert_eq!(&header[..2], &UART_MAGIC);
            let sequence = u32::from_le_bytes(header[4..8].try_into().expect("4 bytes"));
            let expected = u16::from_le_bytes(header[8..10].try_into().expect("2 bytes")) as usize;
            if buffer.len() - offset < UART_HEADER_SIZE + expected {
                break;
            }
            let payload =
                buffer[offset + UART_HEADER_SIZE..offset + UART_HEADER_SIZE + expected].to_vec();
            frames.push((sequence, payload));
            offset += UART_HEADER_SIZE + expected;
        }
        assert_eq!(
            frames
                .iter()
                .map(|(sequence, _)| *sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        let joined: Vec<u8> = frames
            .iter()
            .flat_map(|(_, payload)| payload.clone())
            .collect();
        assert_eq!(joined, b"abcdefghijklmnopqr");
    }

    #[test]
    fn att_chunk_size_is_clamped_to_20_and_244() {
        let mut small = UartCodec::new(50, 1, 1);
        let chunks = small.encode_write(&[b'x'; 60], 5);
        // write_size 5 becomes an effective ATT chunk of 20.
        assert!(chunks.iter().all(|chunk| chunk.len() <= 20));
        assert_eq!(
            chunks.iter().map(Vec::len).sum::<usize>(),
            60 + 2 * UART_HEADER_SIZE
        );

        let mut large = UartCodec::new(232, 1, 1);
        let chunks = large.encode_write(&vec![b'x'; 300], 1000);
        // write_size above 244 is clamped to 244.
        assert!(chunks.iter().all(|chunk| chunk.len() <= 244));
    }

    #[test]
    fn next_sequence_wraps_from_max_to_one() {
        assert_eq!(UartCodec::next_sequence(u32::MAX), 1);
        assert_eq!(UartCodec::next_sequence(7), 8);

        // A device advertising tx=0xFFFFFFFF keeps wrapping correctly.
        let mut channel = UartCodec::new(232, u32::MAX, 1);
        let chunks = channel.encode_write(b"a", 512);
        let sequence = u32::from_le_bytes(chunks[0][4..8].try_into().expect("4 bytes"));
        assert_eq!(sequence, u32::MAX);
        assert_eq!(channel.tx_sequence(), 1);
    }

    #[test]
    fn zero_sequences_and_payload_limits_are_normalized() {
        // Python: max_payload = max(1, min(advertised, 232)); tx/rx of 0 -> 1.
        let mut channel = UartCodec::new(0, 0, 0);
        assert_eq!(channel.max_payload(), 1);
        assert_eq!(channel.tx_sequence(), 1);
        assert_eq!(channel.rx_sequence(), 1);
        let chunks = channel.encode_write(b"ab", 512);
        assert_eq!(chunks.len(), 2);

        let channel = UartCodec::new(500, 1, 1);
        assert_eq!(channel.max_payload(), 232);
    }

    #[test]
    fn orphaned_and_invalid_fragments_are_noticed() {
        let mut channel = codec(232);
        assert_eq!(channel.feed(b"garbage").unwrap(), None);
        assert_eq!(
            channel.take_notices(),
            vec![(
                NoticeLevel::Info,
                "reliable UART <- orphaned fragment".to_string()
            )]
        );

        // Too short for a header.
        assert_eq!(channel.feed(&b"LR\x01"[..]).unwrap(), None);
        assert_eq!(
            channel.take_notices(),
            vec![(
                NoticeLevel::Info,
                "reliable UART <- orphaned fragment".to_string()
            )]
        );

        // Bad magic in a full-size header is an orphan too.
        let mut header = uart_frame(1, b"");
        header[0] = b'X';
        assert_eq!(channel.feed(&header).unwrap(), None);
        assert_eq!(
            channel.take_notices(),
            vec![(
                NoticeLevel::Info,
                "reliable UART <- orphaned fragment".to_string()
            )]
        );
        assert!(channel.take_notices().is_empty());
    }

    #[test]
    fn invalid_frame_headers_are_warned_and_dropped() {
        let mut channel = codec(232);
        // Version 2.
        let mut frame = uart_frame(1, b"data");
        frame[2] = 2;
        assert_eq!(channel.feed(&frame).unwrap(), None);
        // Sequence 0.
        let frame = uart_frame(0, b"data");
        assert_eq!(channel.feed(&frame).unwrap(), None);
        // Expected 0.
        let mut frame = uart_frame(1, b"data");
        frame[8] = 0;
        frame[9] = 0;
        assert_eq!(channel.feed(&frame).unwrap(), None);
        // Expected beyond the advertised limit.
        let mut frame = vec![0u8; UART_HEADER_SIZE + 2];
        frame[..2].copy_from_slice(&UART_MAGIC);
        frame[2] = UART_VERSION;
        frame[4..8].copy_from_slice(&1u32.to_le_bytes());
        frame[8..10].copy_from_slice(&231u16.to_le_bytes());
        let mut channel = codec(230);
        assert_eq!(channel.feed(&frame).unwrap(), None);

        let notices = channel.take_notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].0, NoticeLevel::Warn);
        assert_eq!(notices[0].1, "reliable UART <- invalid frame header");
    }

    #[test]
    fn oversized_frame_is_dropped_whole() {
        let mut channel = codec(232);
        // A header claiming 5 payload bytes, then 6 bytes of payload.
        let mut frame = uart_frame(1, b"12345");
        frame.push(b'6');
        assert_eq!(channel.feed(&frame).unwrap(), None);
        assert_eq!(
            channel.take_notices(),
            vec![(
                NoticeLevel::Warn,
                "reliable UART <- oversized frame".to_string()
            )]
        );
        // The state machine is idle again: a fresh header is parsed.
        assert_eq!(
            channel.feed(&uart_frame(1, b"ok")).unwrap(),
            Some(b"ok".to_vec())
        );
    }

    #[test]
    fn traces_carry_the_python_repr() {
        assert_eq!(
            tx_trace(3, b"hello\r\n", false),
            "UART TX #3 b'hello\\r\\n'"
        );
        assert_eq!(
            tx_trace(3, &[0u8; 20], true),
            "UART TX #3 <redacted 20 bytes>"
        );
    }
}
