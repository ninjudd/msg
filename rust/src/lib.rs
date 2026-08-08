//! `msg` reads and sends iMessages from the command line.
//!
//! The library half is shared by both binaries: `msg`, which is what a person
//! runs, and `msgd`, the launchd agent that holds Full Disk Access so the
//! terminal does not. See docs/projects/all/daemon-and-permissions.md.

use std::path::PathBuf;

pub mod apple;
pub mod contacts;
pub mod db;

/// One version string, shared by the CLI, the daemon, and their handshake.
pub const VERSION: &str = "0.1.0";

/// The user's home directory.
///
/// `$HOME` first, like every other tool, falling back to the passwd entry for
/// the running uid. launchd sets `$HOME` for an agent, so the fallback is for
/// the odd shell that unsets it rather than for the daemon.
pub fn home() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home);
    }
    use std::os::unix::ffi::OsStrExt;

    // SAFETY: getpwuid returns a pointer into a static buffer, copied out here
    // before any other passwd call could overwrite it.
    let directory = unsafe {
        let entry = libc::getpwuid(libc::getuid());
        if entry.is_null() {
            return PathBuf::from("/");
        }
        std::ffi::CStr::from_ptr((*entry).pw_dir)
            .to_bytes()
            .to_vec()
    };
    PathBuf::from(std::ffi::OsStr::from_bytes(&directory))
}

/// What went wrong, in the three shapes the wire protocol distinguishes.
///
/// `AccessDenied` is the one the CLI acts on, since it maps to the exit status
/// the README documents.
#[derive(Debug)]
pub enum Error {
    /// The data is there and the grant is not. Exit status 2.
    AccessDenied(String),
    /// Sending is switched off in the config, which is not a failure.
    SendDisabled(String),
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn other(message: impl Into<String>) -> Self {
        Self::Other(message.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AccessDenied(message) | Self::SendDisabled(message) | Self::Other(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self::Other(error.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Other(error.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Other(error.to_string())
    }
}

impl From<apple::ParseTimeError> for Error {
    fn from(error: apple::ParseTimeError) -> Self {
        Self::Other(error.to_string())
    }
}

/// Dates on the wire, as `2026-01-15T17:30:00.000Z`.
///
/// Always three fractional digits and a literal `Z`, which is what
/// `JSON.stringify` does with a `Date` — so a Rust daemon and a TypeScript
/// client produce and accept the same bytes while both exist (rust-rewrite §4).
pub mod iso {
    use chrono::{DateTime, SecondsFormat, Utc};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<DateTime<Utc>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(date) => {
                serializer.serialize_str(&date.to_rfc3339_opts(SecondsFormat::Millis, true))
            }
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<DateTime<Utc>>, D::Error> {
        let raw = Option::<String>::deserialize(deserializer)?;
        Ok(raw.and_then(|text| {
            DateTime::parse_from_rfc3339(&text)
                .ok()
                .map(|parsed| parsed.with_timezone(&Utc))
        }))
    }
}
