//! Why a transfer did not finish.
//!
//! Both directions fail in the same few ways, so they say so with one type: the
//! far end aborted, the far end offered or asked for something this side will
//! not do, the declared length and the bytes that arrived disagreed, or a frame
//! arrived that makes no sense at this point in the dialogue.
//!
//! None of these are panics. A corrupt serial line is a thing that happens, and
//! the answer is to tell the caller which file to discard, not to take the
//! session down.

use super::frame::FrameKind;

/// Why a transfer did not complete. The caller shows this and discards whatever
/// it wrote for the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The far end aborted, or sent `ZCAN`.
    Aborted,
    /// The far end offered or asked for something this side will not do.
    Unsupported(String),
    /// The declared length and the bytes that arrived disagreed, in either
    /// direction: a receive measured against what came in, or a resume asked
    /// for from a position the file does not have.
    LengthMismatch { declared: u64, received: u64 },
    /// A frame arrived that makes no sense at this point in the dialogue, such
    /// as an end-of-file with no file in flight.
    Unexpected(FrameKind),
}
