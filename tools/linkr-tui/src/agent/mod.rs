//! The agent data layer, ported from `web/agent_*.js`.
//!
//! The assistant is an upstream feature and must reach parity. What is ported
//! here is everything the runtime and the panel depend on that is *not* the
//! runtime itself: configuration validation, credential redaction, task/note
//! stores, session persistence, token accounting, the diagnostic report, and
//! the execution policy. All of it is plain functions over a `Storage` trait, so
//! the same logic runs against the web's localStorage, a JSON file, or an
//! in-memory test map.
//!
//! The LLM transport is still outside this crate. A model is a network service
//! the host process speaks to over HTTPS, and this crate is deliberately
//! zero-dependency — so the transport is a seam, not a missing piece, and what
//! went in around it is the part that decides what may be sent at all:
//! [`policy`]. That module is where a prompt injection either lands or does not,
//! and it was rewritten rather than transliterated because the version it is
//! based on misses `rm -Rf` and `chmod -R`. See its header.

pub mod config;
pub mod notes;
pub mod policy;
pub mod redact;
pub mod report;
pub mod session;
pub mod tasks;
pub mod usage;
