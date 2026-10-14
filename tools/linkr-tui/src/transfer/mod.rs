//! File-transfer protocol modules carried over from the web client.
//!
//! The shell protocol is the *only* file path on this bridge, and it is the
//! same one the web client uses: `cat` would evict the console history the
//! operator is watching, and there is no way at all to push a local file the
//! other way. So reads page with `dd | base64` and uploads append base64 chunks
//! to a sibling part file that is verified before it is moved over the
//! destination.
//!
//! A block protocol was tried here and taken out again. ZMODEM cannot be made
//! lossless over this bridge: the ESP32's BLE→UART queue drops bytes when the
//! UART drains slower than the radio delivers, the sender at this end never sees
//! those drops, and the only real fix is hardware flow control — firmware this
//! client does not own. What is left is a session that retransmits its way
//! through every dropped byte, which is what made it too slow to use. The web
//! client never had it for the same underlying reason.
//!
//! Every protocol module here is pure: no socket, no serial port, no storage. The
//! caller decides when a command may be sent; these functions only build and parse
//! the wire.

pub mod base64;
pub mod binding;
pub mod download;
pub mod files;
pub mod probe;
pub mod profile;
pub mod verify;
