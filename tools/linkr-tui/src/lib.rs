//! The Linkr Bee host terminal, as a library.
//!
//! The binary in `main.rs` is a thin shell over this: every piece of behaviour
//! worth testing — state, theme, strings, and the composition of a frame — is
//! reachable without a terminal, so the test suite runs headless.

pub mod accessory;
pub mod agent;
pub mod app;
pub mod event;
pub mod exec;
pub mod i18n;
pub mod protocol;
pub mod serial;
pub mod text;
pub mod theme;
pub mod transfer;
pub mod ui;
pub mod views;
pub mod watch;
