//! The daemon, its wire protocol, and both ends of the socket.
//!
//! `msgd` exists because TCC attributes a file access to the responsible
//! process: granting Full Disk Access to a terminal grants it to everything
//! started from that terminal, while a launchd agent is its own responsible
//! process and can hold the grant alone. See
//! docs/projects/daemon-and-permissions/readme.md.

pub mod client;
pub mod config;
pub mod install;
pub mod protocol;
pub mod send;
pub mod server;
