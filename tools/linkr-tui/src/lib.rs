//! The Linkr Bee host terminal, as a library.
//!
//! The binary in `main.rs` is a thin shell over this: every piece of behaviour
//! worth testing — state, theme, strings, and the composition of a frame — is
//! reachable without a terminal, so the test suite runs headless.

pub mod app;
pub mod event;
pub mod i18n;
pub mod theme;
pub mod ui;
