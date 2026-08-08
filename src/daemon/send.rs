//! Driving Messages.app over Apple Events.
//!
//! Only the daemon does this. Apple Events are gated by Automation, which is a
//! separate TCC service from Full Disk Access, so `msgd` can be allowed to send
//! while being refused the database, or the reverse. That separation is the
//! reason sending belongs here rather than in the CLI, where the only gate would
//! be the process's opinion of itself (§7).
//!
//! macOS refuses an Apple Event from a client with no
//! `NSAppleEventsUsageDescription`, without even prompting, so this only works
//! because the daemon ships as a bundle carrying one in its `Info.plist`.

use std::path::Path;
use std::process::Command;

use crate::{Error, Result};

/// Arguments are passed to the script rather than interpolated into it, so
/// quotes and backslashes in the body need no escaping.
const SEND_TEXT: &str = r#"
on run {chatGuid, body}
  tell application "Messages"
    send body to chat id chatGuid
  end tell
end run
"#;

const SEND_FILE: &str = r#"
on run {chatGuid, filePath}
  tell application "Messages"
    send POSIX file filePath to chat id chatGuid
  end tell
end run
"#;

fn run_script(script: &str, args: &[&str]) -> Result<()> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .args(args)
        .output()
        .map_err(|error| Error::other(format!("could not run osascript: {error}")))?;

    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(Error::other(if stderr.is_empty() {
        format!("osascript failed: {}", output.status)
    } else {
        stderr
    }))
}

pub fn send_message(chat_guid: &str, body: &str) -> Result<()> {
    run_script(SEND_TEXT, &[chat_guid, body])
}

/// Ask Messages how many conversations it has, which sends nothing.
///
/// This exists so the Automation permission can be established and inspected
/// without texting anyone. macOS creates the entry in Privacy & Security >
/// Automation when a client first asks — so before this, the only way to get an
/// entry you could switch off was to send a real message to a real person.
///
/// It has to be an event that TCC actually gates. Standard Suite events like
/// `get name` are answered without any grant at all, so probing with one reports
/// "allowed" on a machine where sending is refused, and creates no entry.
/// Reaching the app's own data is what triggers the check; a count reveals
/// nothing about who anyone is talking to.
pub fn check_automation() -> (bool, String) {
    let output = Command::new("osascript")
        .args(["-e", "tell application \"Messages\" to count of chats"])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let count = String::from_utf8_lossy(&output.stdout).trim().to_string();
            (true, format!("{count} conversations visible to Messages"))
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            (
                false,
                if stderr.is_empty() {
                    format!("osascript exited {}", output.status)
                } else {
                    stderr
                },
            )
        }
        Err(error) => (false, format!("could not run osascript: {error}")),
    }
}

/// The name an attachment is written under, with anything that could escape the
/// directory removed.
///
/// `basename` then leading dots, matching what the TypeScript did: the client
/// chooses this string, and it becomes a path the daemon writes to.
pub fn attachment_name(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    let safe = base.trim_start_matches('.');
    if safe.is_empty() {
        "attachment".to_string()
    } else {
        safe.to_string()
    }
}

/// Send an attachment the client handed over as bytes.
///
/// The daemon never takes a path. It holds Full Disk Access, so accepting one
/// here would turn `send` into "read any file on the disk and text it to
/// someone" — the exact shape §6 exists to prevent. The client reads the file
/// with its own permissions; the daemon only writes what it is given, into a
/// directory it owns.
pub fn send_attachment(chat_guid: &str, name: &str, data: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let directory = crate::db::temporary_directory("msgd-")?;
    let path = directory.join(attachment_name(name));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        std::io::Write::write_all(&mut file, data)?;
    }

    let sent = run_script(SEND_FILE, &[chat_guid, &path.to_string_lossy()]);

    // Messages reads the file after the event returns, so it cannot be removed
    // immediately. A detached thread outlives the request without holding it up.
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(60));
        std::fs::remove_dir_all(&directory).ok();
    });
    sent
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The client picks this name, so it is untrusted input that becomes a path.
    #[test]
    fn strips_anything_that_could_escape_the_directory() {
        assert_eq!(attachment_name("photo.png"), "photo.png");
        assert_eq!(attachment_name("/etc/passwd"), "passwd");
        assert_eq!(attachment_name("../../../etc/passwd"), "passwd");
        assert_eq!(attachment_name(".ssh"), "ssh");
        assert_eq!(attachment_name("..."), "attachment");
        assert_eq!(attachment_name(""), "attachment");
        assert_eq!(attachment_name("/"), "attachment");
        assert_eq!(attachment_name(".."), "attachment");
        assert_eq!(attachment_name("a/b/../c.txt"), "c.txt");
    }
}
