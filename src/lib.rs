//! nerdmux: a terminal multiplexer with sessions, windows and panes.
//!
//! [`embed`] is the library API: a terminal and a theme for other programs
//! to use. [`cli`] is the `nerdmux` command, which the binary runs. Everything
//! else is private to the crate, free to change between versions.

pub mod cli;
pub mod embed;

// The wire protocol, public only for the end-to-end tests, which speak it
// to a real server.
#[doc(hidden)]
pub mod proto;

mod client;
mod config;
mod input;
mod keys;
mod layout;
mod overlay;
mod pane;
mod procinfo;
mod prompt;
mod render;
mod server;
mod settings;
mod snapshot;
mod status;
