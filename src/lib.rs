//! Blink launcher library.
//!
//! Exposing the application logic as a lib target allows integration tests,
//! benchmarks and fuzz harnesses to reuse `Engine` and the providers without
//! going through the GTK binary entrypoint. The `main.rs` binary is a thin
//! wrapper around this crate.

pub mod config;
pub mod engine;
pub mod ipc;
pub mod providers;
pub mod theme;
pub mod typos;
pub mod ui;
pub mod usage;

#[cfg(feature = "bench")]
pub mod bench;
