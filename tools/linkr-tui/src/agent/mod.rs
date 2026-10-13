//! The agent data layer, ported from `web/agent_*.js`.
//!
//! The assistant is an upstream feature and must reach parity, but its LLM
//! runtime is deliberately left out of this zero-dependency crate: the model is
//! a network service the host process speaks to, not something a TUI bundles.
//! What *is* ported, 1:1, is every piece of pure data logic the runtime and the
//! panel depend on — configuration validation, credential redaction, task/note
//! stores, session persistence, token accounting, and the diagnostic report.
//! All of it is plain functions over a `Storage` trait, so the same logic runs
//! against the web's localStorage, a JSON file, or an in-memory test map.

pub mod config;
pub mod notes;
pub mod redact;
pub mod report;
pub mod session;
pub mod tasks;
pub mod usage;
