//! `msg` reads and sends iMessages from the command line.
//!
//! The library half is shared by both binaries: `msg`, which is what a person
//! runs, and `msgd`, the launchd agent that holds Full Disk Access so the
//! terminal does not. See docs/projects/all/daemon-and-permissions.md.

pub mod apple;

/// One version string, shared by the CLI, the daemon, and their handshake.
pub const VERSION: &str = "0.1.0";
