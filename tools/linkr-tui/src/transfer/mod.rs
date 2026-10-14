//! File-transfer protocol modules carried over from the web client.
//!
//! The shell protocol is the *only* file path on this bridge: `cat` would
//! evict the console history the operator is watching, and there is no way at
//! all to push a local file the other way. So reads page with `dd | base64` and
//! uploads append base64 chunks to a sibling part file that is verified before
//! it is moved over the destination.
//!
//! Every protocol module here is pure: no socket, no serial port, no storage. The
//! caller decides when a command may be sent; these functions only build and parse
//! the wire.
//!
//! `zmodem` is the exception, and is named as one: it is the engine that gives
//! the ZMODEM codec a directory to write into and a file to read from, so it
//! touches storage by design. It is here rather than beside the codec because it
//! is the same kind of thing as the modules next to it — a way of moving a file —
//! and the transfer view renders both.

pub mod base64;
pub mod binding;
pub mod download;
pub mod files;
pub mod probe;
pub mod profile;
pub mod verify;
pub mod zmodem;
