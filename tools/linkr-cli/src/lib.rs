//! Linkr Bee host CLI + TUI library.
//!
//! Module ownership and the public contract between modules are defined in
//! `CONTRACTS.md` at the crate root. Do not change public signatures that the
//! contract names without updating that document.

pub mod agent;
pub mod cli;
pub mod event;
pub mod journal;
pub mod lan_token_store;
pub mod protocol;
pub mod session;
pub mod target_files;
pub mod target_verify;
pub mod term;
pub mod transfer;
pub mod transport;
pub mod tui;
pub mod watch;

pub use event::{ConnectionState, CoreEvent, MgmtKind, NoticeLevel, RequestId};
