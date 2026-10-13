//! File-transfer protocol modules carried over from the web client.
//!
//! The shell protocol is the *only* file path on this bridge: `cat` would
//! evict the console history the operator is watching, and there is no way at
//! all to push a local file the other way. So reads page with `dd | base64` and
//! uploads append base64 chunks to a sibling part file that is verified before
//! it is moved over the destination.
//!
//! Every module here is pure: no socket, no serial port, no storage. The caller
//! decides when a command may be sent; these functions only build and parse the
//! wire.

pub mod base64;
pub mod binding;
pub mod download;
pub mod files;
pub mod profile;
pub mod verify;
