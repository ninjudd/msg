//! The config file, which exists to hold one key.
//!
//! `send = true` in `~/.config/msg/config.toml` is the first of the two gates on
//! sending: it prevents accidents and produces a refusal that names the key,
//! instead of an opaque AppleScript -1743. The second gate is the Automation
//! grant, which is the one that holds when this file does not. See
//! docs/projects/all/daemon-and-permissions.md §7.
//!
//! The daemon reads it, not the client. A check a caller performs on itself is
//! advice, not a gate.

use std::path::{Path, PathBuf};

pub fn config_path() -> PathBuf {
    match std::env::var("MSG_CONFIG") {
        Ok(path) if !path.is_empty() => PathBuf::from(path),
        _ => crate::home().join(".config/msg/config.toml"),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub send: bool,
}

/// Read one `send = true|false` line, and nothing else.
///
/// Deliberately not a TOML parser. One flat `key = value` covers the whole of
/// what this file is allowed to say, and a dependency for that would cost more
/// than it explains. Anything unrecognised is ignored, so a file that grows real
/// TOML later still reads correctly for `send`.
fn parse_send(line: &str) -> Option<bool> {
    let rest = line.trim_start().strip_prefix("send")?;
    let rest = rest.trim_start().strip_prefix('=')?.trim_start();
    let (value, rest) = rest
        .strip_prefix("true")
        .map(|rest| (true, rest))
        .or_else(|| rest.strip_prefix("false").map(|rest| (false, rest)))?;
    // Whitespace, then an optional comment, then nothing. `send = truthy` and
    // `send = "true"` both fall out here rather than reading as the switch.
    let rest = rest.trim();
    (rest.is_empty() || rest.starts_with('#')).then_some(value)
}

pub fn read_config(path: &Path) -> Config {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Config { send: false };
    };
    for line in text.lines() {
        if let Some(send) = parse_send(line) {
            return Config { send };
        }
    }
    Config { send: false }
}

/// The refusal the daemon gives when sending is switched off.
pub fn disabled_message(path: &Path) -> String {
    format!(
        "sending is disabled.\n\
         Add `send = true` to {} to enable it.\n\
         macOS also has to allow msgd to control Messages, under System Settings >\n\
         Privacy & Security > Automation.",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send_of(contents: &str) -> bool {
        let directory = crate::db::temporary_directory("msg-config-").unwrap();
        let path = directory.join("config.toml");
        std::fs::write(&path, contents).unwrap();
        let config = read_config(&path);
        std::fs::remove_dir_all(&directory).ok();
        config.send
    }

    #[test]
    fn is_off_when_the_file_does_not_exist() {
        assert!(!read_config(Path::new("/nonexistent/msg/config.toml")).send);
    }

    #[test]
    fn is_off_when_the_file_exists_but_says_nothing() {
        assert!(!send_of("# nothing here\n"));
    }

    #[test]
    fn reads_send_true_and_false() {
        assert!(send_of("send = true\n"));
        assert!(!send_of("send = false\n"));
    }

    #[test]
    fn tolerates_whitespace_comments_and_unrelated_keys() {
        assert!(send_of(
            "# a comment\nnames = false\n\n   send   =   true   # yes\n"
        ));
    }

    #[test]
    fn does_not_treat_a_lookalike_key_as_the_switch() {
        assert!(!send_of("sendmail = true\n"));
        assert!(!send_of("resend = true\n"));
    }

    #[test]
    fn refuses_anything_that_is_not_a_bare_boolean() {
        assert!(!send_of("send = \"true\"\n"));
        assert!(!send_of("send = 1\n"));
        assert!(!send_of("send = truthy\n"));
    }

    #[test]
    fn takes_the_first_line_that_says_anything() {
        assert!(!send_of("send = false\nsend = true\n"));
    }

    #[test]
    fn survives_carriage_returns() {
        assert!(send_of("send = true\r\n"));
    }
}
