//! TRACE32 backend used by the rustvisor board runner and the tracebridge CLI.
#![deny(unsafe_code)]
#![warn(clippy::undocumented_unsafe_blocks)]

mod config;
mod dap;
mod debug;
mod errors;
mod flash;
mod init;
mod powerview;
mod pycompat;
mod remote;
mod rtt;
mod rustrover;
mod signals;
mod skill;
mod style;
mod t32config;
mod target;
mod ui;
mod vscode;

pub mod backend;
mod cli;

pub use cli::main as cli_main;
pub use errors::BridgeError;
