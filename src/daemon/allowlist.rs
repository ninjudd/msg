//! The addresses `msg send --listed` may reach.
//!
//! `--listed` asks the daemon to refuse the send unless every member of the
//! conversation it resolved is on `/etc/msg/allowlist`. It is what lets an
//! agent send without asking first: the flag is safe to approve in advance,
//! because the worst it can do is text someone the user already listed. See
//! docs/projects/send-allowlist/readme.md.
//!
//! The list only works as a limit if the process it limits cannot change it,
//! and that process runs as the user. So the file, and every directory above
//! it, has to be owned by root and writable by nobody else — the check OpenSSH
//! applies to `authorized_keys` under `StrictModes`, drawn one uid tighter. A
//! list that fails the check, or does not exist, admits nobody (§3).

use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::db::{Chat, describe_target};
use crate::{Error, Result};

/// Where the list lives. Not taken from the environment, as `MSG_CONFIG` is:
/// a path the caller can choose is a list the caller can write (§3).
pub const ALLOWLIST_PATH: &str = "/etc/msg/allowlist";

/// Where to read the list, and who has to own it.
///
/// Always the default outside tests. A test cannot create a root-owned file,
/// so it names its own uid as the owner instead, which exercises the same
/// check against a file it can write.
#[derive(Debug, Clone)]
pub struct Location {
    pub path: PathBuf,
    /// The uid, besides root, that the file and its directories may belong to.
    /// Root in production, so that nobody else qualifies.
    pub owner: u32,
}

impl Default for Location {
    fn default() -> Self {
        Self {
            path: PathBuf::from(ALLOWLIST_PATH),
            owner: 0,
        }
    }
}

/// The addresses a `--listed` send may reach, each reduced by [`address_key`].
#[derive(Debug, Clone, Default)]
pub struct Allowlist {
    keys: Vec<String>,
}

impl Allowlist {
    /// One address per line. `#` starts a comment, and blank lines are
    /// skipped. Anything else that is not an address is an error rather than
    /// a line to skip, so a typo is reported instead of quietly listing
    /// nobody.
    pub fn parse(text: &str, path: &Path) -> Result<Self> {
        let mut keys = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let entry = line.split('#').next().unwrap_or_default().trim();
            if entry.is_empty() {
                continue;
            }
            let Some(key) = address_key(entry) else {
                return Err(Error::other(format!(
                    "{} line {} is not an address: {entry}\n\
                     List an email address, or a phone number with its country code, like +13105551234.",
                    path.display(),
                    index + 1
                )));
            };
            keys.push(key);
        }
        Ok(Self { keys })
    }

    pub fn admits(&self, handle: &str) -> bool {
        address_key(handle).is_some_and(|key| self.keys.contains(&key))
    }
}

/// An address reduced to the form two spellings of it share, or `None` for
/// anything that is not an address.
///
/// Stricter than `contacts::handle_key` on purpose. That one matches phone
/// numbers on their last ten digits, which is right for finding a person and
/// wrong for a limit: it would let `+13105551234` on the list admit
/// `+443105551234`. Here a number has to carry its country code and match in
/// full, and only the punctuation `contacts::emit_phone` drops for Contacts is
/// dropped here.
fn address_key(address: &str) -> Option<String> {
    let trimmed = address.trim();
    if trimmed.contains('@') {
        return (!trimmed.contains(char::is_whitespace)).then(|| trimmed.to_lowercase());
    }
    // `emit_phone` strips only a string that is digits once the punctuation is
    // gone, and hands anything else back as it was, which the digit check
    // below then refuses.
    let phone = crate::contacts::emit_phone(trimmed);
    let digits = phone.strip_prefix('+')?;
    (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())).then_some(phone)
}

/// Read the list, refusing one that anyone but root could have written.
pub fn read(location: &Location) -> Result<Allowlist> {
    let path = &location.path;
    let canonical = match std::fs::canonicalize(path) {
        Ok(canonical) => canonical,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::other(missing_message(path)));
        }
        Err(error) => {
            return Err(Error::other(format!(
                "could not read {}: {error}",
                path.display()
            )));
        }
    };

    // Every directory above the file, since whoever can write one of them can
    // rename the file away and put their own in its place.
    for directory in canonical.ancestors().skip(1) {
        trusted(
            directory,
            &std::fs::metadata(directory)?,
            location.owner,
            path,
        )?;
    }

    // The checked metadata comes from the open file, not the path, so a file
    // swapped in between the check and the read is the file that is checked.
    let mut file = std::fs::File::open(&canonical)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(Error::other(format!(
            "{} is not a file",
            canonical.display()
        )));
    }
    trusted(&canonical, &metadata, location.owner, path)?;

    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Allowlist::parse(&text, path)
}

fn trusted(at: &Path, metadata: &std::fs::Metadata, owner: u32, list: &Path) -> Result<()> {
    let uid = metadata.uid();
    let mode = metadata.mode();
    if (uid == 0 || uid == owner) && mode & 0o022 == 0 {
        return Ok(());
    }
    Err(Error::other(format!(
        "refusing {}: {} is owned by uid {uid} with mode {:o}.\n\
         Only root may be able to change the list, or anything running as you could add to it.\n\
         Fix it with: sudo chown root:wheel {} && sudo chmod go-w {}",
        list.display(),
        at.display(),
        mode & 0o7777,
        at.display(),
        at.display()
    )))
}

fn missing_message(path: &Path) -> String {
    let directory = path.parent().unwrap_or(Path::new("/"));
    format!(
        "--listed sends only to addresses in {}, and there is no such file.\n\
         Create it as root, one address per line:\n\n  \
         sudo mkdir -p {}\n  \
         echo '+13105551234' | sudo tee -a {}",
        path.display(),
        directory.display(),
        path.display()
    )
}

/// Refuse the send unless every member of `chat` is on the list at `location`.
///
/// The chat is the one already resolved, so the address checked is the
/// address the send goes to; nothing is resolved a second time in between.
pub fn check(chat: &Chat, location: &Location) -> Result<()> {
    let list = read(location)?;
    let members: Vec<&str> = chat
        .handles
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|handle| !handle.is_empty())
        .collect();
    if members.is_empty() {
        return Err(Error::other(format!(
            "not sent: {} has no members to check against {}",
            describe_target(chat),
            location.path.display()
        )));
    }
    let unlisted: Vec<&str> = members
        .into_iter()
        .filter(|handle| !list.admits(handle))
        .collect();
    if unlisted.is_empty() {
        return Ok(());
    }
    Err(Error::other(format!(
        "not sent: {} is not on {} ({} {})",
        describe_target(chat),
        location.path.display(),
        if unlisted.len() == 1 {
            "unlisted:"
        } else {
            "unlisted members:"
        },
        unlisted.join(", ")
    )))
}

/// The refusal for a chat guid under `--listed`.
///
/// A guid is sent without a database read, so there would be no members to
/// check; an address or a name resolves to a conversation whose members are
/// known.
pub fn guid_message() -> String {
    "--listed takes an address or a name, not a chat guid, so that the members it checks are the ones the send reaches".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn parsed(text: &str) -> Result<Allowlist> {
        Allowlist::parse(text, Path::new("/etc/msg/allowlist"))
    }

    #[test]
    fn reads_addresses_and_skips_comments_and_blank_lines() {
        let list = parsed(
            "# family\n\n+1 (310) 555-1234   # dana\nRobin@Example.com\n  +44 20 7946 0000\n",
        )
        .unwrap();
        assert!(list.admits("+13105551234"));
        assert!(list.admits("robin@example.com"));
        assert!(list.admits("ROBIN@EXAMPLE.COM"));
        assert!(list.admits("+442079460000"));
        assert!(!list.admits("kit@example.com"));
    }

    #[test]
    fn refuses_a_line_that_is_not_an_address_and_names_it() {
        let error = parsed("+13105551234\ndana\n").unwrap_err().to_string();
        assert!(error.contains("line 2"), "{error}");
        assert!(error.contains("dana"), "{error}");
    }

    /// A number without its country code could be anyone's in another
    /// country, so it is refused rather than guessed at.
    #[test]
    fn refuses_a_number_without_a_country_code() {
        assert!(parsed("(310) 555-1234\n").is_err());
        assert!(parsed("+\n").is_err());
        assert!(parsed("+1310555abcd\n").is_err());
    }

    /// The reason this does not reuse `handle_key`, which matches numbers on
    /// their last ten digits.
    #[test]
    fn does_not_admit_a_number_that_only_ends_the_same_way() {
        let list = parsed("+13105551234\n").unwrap();
        assert!(!list.admits("+443105551234"));
        assert!(!list.admits("3105551234"));
    }

    #[test]
    fn an_empty_list_admits_nobody() {
        let list = parsed("# nobody yet\n").unwrap();
        assert!(!list.admits("+13105551234"));
        assert!(!list.admits(""));
    }

    struct Fixture {
        directory: PathBuf,
        location: Location,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.directory).ok();
        }
    }

    /// A list in a fresh directory, owned by whoever runs the tests and
    /// trusted as if that were root. The temporary directory has to sit below
    /// directories nobody else can write, which `$TMPDIR` does on macOS.
    fn fixture(contents: &str) -> Fixture {
        let directory = crate::db::temporary_directory("msg-allowlist-").unwrap();
        let path = directory.join("allowlist");
        std::fs::write(&path, contents).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let owner = std::fs::metadata(&directory).unwrap().uid();
        Fixture {
            directory,
            location: Location { path, owner },
        }
    }

    #[test]
    fn reads_a_list_only_its_owner_can_write() {
        let fixture = fixture("+13105551234\n");
        assert!(read(&fixture.location).unwrap().admits("+13105551234"));
    }

    /// Production names root as the owner, and nobody running the tests is
    /// root, so a list anyone else owns is what a user-writable list looks
    /// like there.
    #[test]
    fn refuses_a_list_owned_by_anyone_but_the_owner_and_root() {
        let mut fixture = fixture("+13105551234\n");
        fixture.location.owner += 1;
        let error = read(&fixture.location).unwrap_err().to_string();
        assert!(error.contains("refusing"), "{error}");
        assert!(error.contains("sudo chown root:wheel"), "{error}");
    }

    #[test]
    fn refuses_a_list_others_can_write() {
        let fixture = fixture("+13105551234\n");
        std::fs::set_permissions(
            &fixture.location.path,
            std::fs::Permissions::from_mode(0o666),
        )
        .unwrap();
        let error = read(&fixture.location).unwrap_err().to_string();
        assert!(error.contains("mode 666"), "{error}");
    }

    #[test]
    fn refuses_a_list_in_a_directory_others_can_write() {
        let fixture = fixture("+13105551234\n");
        std::fs::set_permissions(&fixture.directory, std::fs::Permissions::from_mode(0o777))
            .unwrap();
        let error = read(&fixture.location).unwrap_err().to_string();
        assert!(error.contains("mode 777"), "{error}");
    }

    #[test]
    fn a_missing_list_admits_nobody_and_says_how_to_create_it() {
        let fixture = fixture("");
        let location = Location {
            path: fixture.directory.join("missing"),
            owner: fixture.location.owner,
        };
        let error = read(&location).unwrap_err().to_string();
        assert!(error.contains("no such file"), "{error}");
        assert!(error.contains("sudo tee -a"), "{error}");
    }

    fn chat(handles: Option<&str>, is_group: bool) -> Chat {
        Chat {
            rowid: 1,
            guid: "iMessage;-;+13105551234".into(),
            identifier: "+13105551234".into(),
            display_name: None,
            handles: handles.map(String::from),
            named_handles: None,
            is_filtered: false,
            member_count: 1,
            is_group,
            last_date: None,
            message_count: 0,
            name: "+13105551234".into(),
        }
    }

    #[test]
    fn admits_a_conversation_whose_members_are_all_listed() {
        let fixture = fixture("+13105551234\nrobin@example.com\n");
        check(&chat(Some("+13105551234"), false), &fixture.location).unwrap();
        check(
            &chat(Some("+13105551234,robin@example.com"), true),
            &fixture.location,
        )
        .unwrap();
    }

    #[test]
    fn refuses_a_room_with_one_unlisted_member_and_names_them() {
        let fixture = fixture("+13105551234\n");
        let error = check(
            &chat(Some("+13105551234,kit@example.com"), true),
            &fixture.location,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("not sent"), "{error}");
        assert!(error.contains("kit@example.com"), "{error}");
        assert!(!error.contains("unlisted: +13105551234"), "{error}");
    }

    #[test]
    fn refuses_a_conversation_with_no_members_to_check() {
        let fixture = fixture("+13105551234\n");
        let error = check(&chat(None, false), &fixture.location)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no members"), "{error}");
    }
}
