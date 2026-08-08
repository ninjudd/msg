//! Name lookup for handles, backed by the macOS Contacts database.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{Connection, OpenFlags};

/// Digits kept when matching phone numbers, which covers NANP numbers in full.
const MATCH_DIGITS: usize = 10;

fn address_book() -> PathBuf {
    crate::home().join("Library/Application Support/AddressBook")
}

/// Handle-to-name, plus why it is emptier than expected when it is.
///
/// Contacts is best-effort — messages still read without it — but silently
/// best-effort is how an empty index went unnoticed until names stopped
/// resolving with no explanation anywhere.
#[derive(Debug, Default, Clone)]
pub struct ContactIndex {
    names: HashMap<String, String>,
    problems: Vec<String>,
}

impl ContactIndex {
    /// The index that knows nothing, used whenever `--no-names` is in force.
    pub fn empty() -> Self {
        Self::default()
    }

    /// An index built from handle-to-name pairs, for tests.
    ///
    /// Keyed through [`handle_key`] like the real loader, so a test that writes
    /// a number in one shape and looks it up in another behaves the same way the
    /// program does.
    #[cfg(test)]
    pub fn for_test(names: std::collections::HashMap<String, String>) -> Self {
        Self {
            names: names
                .into_iter()
                .filter_map(|(handle, name)| Some((handle_key(&handle)?, name)))
                .collect(),
            problems: Vec::new(),
        }
    }

    /// The name for a handle, or `None` when it is unknown.
    pub fn lookup(&self, handle: Option<&str>) -> Option<&str> {
        let key = handle_key(handle?)?;
        self.names.get(&key).map(String::as_str)
    }

    /// How many handles the index knows about.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub fn problems(&self) -> &[String] {
        &self.problems
    }
}

/// Reduce a handle to a comparable key.
///
/// Contacts stores numbers in whatever shape they were typed, so both sides are
/// stripped to digits. Numbers long enough to carry a country code are matched
/// on their final digits, which keeps `+13105551234` and `(310) 555-1234`
/// together.
pub fn handle_key(handle: &str) -> Option<String> {
    let trimmed = handle.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains('@') {
        return Some(trimmed.to_lowercase());
    }

    let digits: String = trimmed.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    Some(if digits.len() > MATCH_DIGITS {
        digits[digits.len() - MATCH_DIGITS..].to_string()
    } else {
        digits
    })
}

/// The Contacts source macOS treats as primary.
///
/// Several accounts can hold a record for the same number under different
/// names, so the account the user actually writes to decides the winner.
pub fn default_source_id() -> Option<String> {
    let output = Command::new("defaults")
        .args(["read", "com.apple.AddressBook", "ABDefaultSourceID"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// The source identifier a database belongs to, taken from its directory.
fn source_id_of(path: &Path) -> Option<String> {
    let parent = path.parent()?.file_name()?.to_string_lossy().into_owned();
    (parent != "AddressBook").then_some(parent)
}

/// Every per-account database under `Sources/`.
///
/// Walked with `read_dir` rather than matched with a glob. A glob over this
/// path found nothing from inside the daemon while the very same files opened
/// fine when reached by directory walk, and a lookup that silently finds no
/// sources is indistinguishable from a machine with no contacts.
fn source_databases(problems: &mut Vec<String>) -> Vec<PathBuf> {
    let directory = address_book().join("Sources");
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) => {
            problems.push(format!("{}: {error}", directory.display()));
            return Vec::new();
        }
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("AddressBook-v22.abcddb"))
        .filter(|path| path.exists())
        .collect();
    found.sort();
    found
}

/// Every Contacts database: one per account, plus the legacy top-level one.
fn contact_databases(problems: &mut Vec<String>) -> Vec<PathBuf> {
    if !address_book().exists() {
        return Vec::new();
    }
    let mut all = source_databases(problems);
    all.push(address_book().join("AddressBook-v22.abcddb"));
    all.retain(|path| path.exists());
    all
}

/// Read every Contacts source into a single handle-to-name map.
///
/// A missing or unreadable database yields an empty index rather than an error,
/// so reading messages still works without Contacts access.
pub fn load_contacts(preferred_source: Option<&str>) -> ContactIndex {
    let mut problems = Vec::new();
    let book = address_book();
    if !book.exists() {
        problems.push(format!("no Contacts directory at {}", book.display()));
    }

    // Read every database before asking which source is preferred.
    //
    // The order matters, which is not obvious and cost a long afternoon. Asking
    // for `com.apple.AddressBook` preferences appears to make TCC start
    // enforcing the Contacts service against this process, and from then on
    // these files are refused with EPERM even though the process holds Full Disk
    // Access. Two builds differing only in whether the directory was read first
    // behaved differently: the one that read first saw 1,123 handles, the one
    // that read after saw none. Reading first costs nothing, since the
    // preference only decides which source wins a tie
    // (daemon-and-permissions.md §12).
    let databases = contact_databases(&mut problems);
    if databases.is_empty() && problems.is_empty() {
        problems.push(format!("no Contacts databases under {}", book.display()));
    }
    let sources: Vec<(PathBuf, HashMap<String, String>)> = databases
        .into_iter()
        .map(|path| {
            let names = read_source(&path, &mut problems);
            (path, names)
        })
        .collect();

    let preferred = preferred_source
        .map(str::to_string)
        .or_else(|| std::env::var("MSG_CONTACTS_SOURCE").ok())
        .or_else(default_source_id);

    // The preferred source is merged first, so its name for a handle wins. When
    // macOS names no default source both sides of this comparison are `None`,
    // which makes the legacy top-level database the preferred one — the same
    // tie-break the TypeScript `===` produced.
    let matches_preferred = |path: &Path| source_id_of(path) == preferred;
    let mut names = HashMap::new();
    let first = sources.iter().filter(|(path, _)| matches_preferred(path));
    let rest = sources.iter().filter(|(path, _)| !matches_preferred(path));
    for (_, source) in first.chain(rest) {
        merge(&mut names, source);
    }

    ContactIndex { names, problems }
}

fn merge(into: &mut HashMap<String, String>, from: &HashMap<String, String>) {
    for (key, name) in from {
        into.entry(key.clone()).or_insert_with(|| name.clone());
    }
}

/// Read one Contacts database. A source that cannot be opened is skipped; the
/// others still count.
fn read_source(path: &Path, problems: &mut Vec<String>) -> HashMap<String, String> {
    let mut names = HashMap::new();
    let db = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(db) => db,
        Err(error) => {
            problems.push(format!("{}: {error}", path.display()));
            return names;
        }
    };

    for sql in [
        "SELECT p.ZFULLNUMBER AS handle, r.ZFIRSTNAME, r.ZLASTNAME, r.ZNICKNAME, r.ZORGANIZATION
           FROM ZABCDPHONENUMBER p
           JOIN ZABCDRECORD r ON r.Z_PK = p.ZOWNER
          WHERE p.ZFULLNUMBER IS NOT NULL",
        "SELECT e.ZADDRESS AS handle, r.ZFIRSTNAME, r.ZLASTNAME, r.ZNICKNAME, r.ZORGANIZATION
           FROM ZABCDEMAILADDRESS e
           JOIN ZABCDRECORD r ON r.Z_PK = e.ZOWNER
          WHERE e.ZADDRESS IS NOT NULL",
    ] {
        if let Err(error) = collect(&db, sql, &mut names) {
            problems.push(format!("{}: {error}", path.display()));
        }
    }
    names
}

fn collect(
    db: &Connection,
    sql: &str,
    names: &mut HashMap<String, String>,
) -> rusqlite::Result<()> {
    let mut statement = db.prepare(sql)?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let Ok(Some(handle)) = row.get::<_, Option<String>>("handle") else {
            continue;
        };
        let Some(key) = handle_key(&handle) else {
            continue;
        };
        let Some(name) = person_name(row) else {
            continue;
        };
        // Sources are visited primary first, so the first name for a handle is
        // the one from the account the user actually maintains.
        names.entry(key).or_insert(name);
    }
    Ok(())
}

fn person_name(row: &rusqlite::Row<'_>) -> Option<String> {
    let text = |column: &str| -> Option<String> {
        row.get::<_, Option<String>>(column)
            .ok()
            .flatten()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let first = text("ZFIRSTNAME");
    let last = text("ZLASTNAME");
    if first.is_some() || last.is_some() {
        let parts: Vec<String> = [first, last].into_iter().flatten().collect();
        return Some(parts.join(" "));
    }
    text("ZNICKNAME").or_else(|| text("ZORGANIZATION"))
}

/// Replace handles with names where they are known, keeping order.
pub fn name_handles(contacts: &ContactIndex, handles: Option<&str>) -> Option<String> {
    let handles = handles?;
    Some(
        handles
            .split(',')
            .map(|handle| contacts.lookup(Some(handle)).unwrap_or(handle).to_string())
            .collect::<Vec<_>>()
            .join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An index built by hand, so no test here reads the Contacts of whoever is
    /// running them.
    fn index() -> ContactIndex {
        let mut names = HashMap::new();
        names.insert("3105551234".to_string(), "Dana Reyes".to_string());
        names.insert("4155559876".to_string(), "Sam Oyelaran".to_string());
        ContactIndex {
            names,
            problems: Vec::new(),
        }
    }

    #[test]
    fn reduces_every_stored_phone_format_to_the_same_key() {
        let formats = [
            "+13105551234",
            "(310) 555-1234",
            "310-555-1234",
            "3105551234",
            "+1 (310) 555-1234",
            "1 (310) 555-1234",
            "310.555.1234",
            "1-310-555-1234",
        ];
        for format in formats {
            assert_eq!(
                handle_key(format).as_deref(),
                Some("3105551234"),
                "{format}"
            );
        }
    }

    #[test]
    fn lowercases_email_addresses() {
        assert_eq!(
            handle_key("Dana@Example.COM").as_deref(),
            Some("dana@example.com")
        );
        assert_eq!(
            handle_key("  dana@example.com  ").as_deref(),
            Some("dana@example.com")
        );
    }

    #[test]
    fn keeps_short_codes_intact() {
        assert_eq!(handle_key("22000").as_deref(), Some("22000"));
    }

    #[test]
    fn matches_international_numbers_on_their_final_digits() {
        assert_eq!(handle_key("+442071234567").as_deref(), Some("2071234567"));
    }

    #[test]
    fn returns_none_for_empty_or_digitless_handles() {
        assert_eq!(handle_key(""), None);
        assert_eq!(handle_key("   "), None);
        assert_eq!(handle_key("---"), None);
    }

    #[test]
    fn names_every_handle_it_recognizes() {
        assert_eq!(
            name_handles(&index(), Some("+13105551234,+14155559876")).as_deref(),
            Some("Dana Reyes, Sam Oyelaran")
        );
    }

    #[test]
    fn leaves_unknown_handles_as_they_were() {
        assert_eq!(
            name_handles(&index(), Some("+13105551234,+19998887777")).as_deref(),
            Some("Dana Reyes, +19998887777")
        );
    }

    #[test]
    fn passes_none_through() {
        assert_eq!(name_handles(&index(), None), None);
    }

    #[test]
    fn is_a_no_op_against_the_empty_index() {
        let empty = ContactIndex::empty();
        assert_eq!(
            name_handles(&empty, Some("+13105551234")).as_deref(),
            Some("+13105551234")
        );
        assert_eq!(empty.lookup(Some("+13105551234")), None);
    }
}
