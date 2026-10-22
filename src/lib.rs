//! nerdmux: a terminal multiplexer with sessions, windows and panes.
//!
//! The binary in `main.rs` is both the client and the background server;
//! this library holds everything they share so tests can drive the server
//! through the same protocol a real client speaks.

pub mod client;
pub mod config;
pub mod input;
pub mod keys;
pub mod layout;
pub mod overlay;
pub mod pane;
pub mod procinfo;
pub mod prompt;
pub mod proto;
pub mod render;
pub mod server;
pub mod settings;
pub mod snapshot;
pub mod status;
