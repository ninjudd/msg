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

/// One Contacts record: who it is, kept apart from what it renders as.
///
/// Two records can legitimately carry the same name — an old entry and a new
/// one, a father and a son — so anything that groups people has to group by
/// `id` and merely *display* `name`. Grouping by the name merges strangers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    /// Source plus record id. Unique for as long as the index lives, which is
    /// one run: these are Core Data primary keys, stable within a database but
    /// not across accounts, hence the source in front.
    pub id: String,
    /// What to call them: the nickname when Contacts holds one, else the name
    /// the record is filed under.
    ///
    /// The nickname wins because it is what you call the person. Someone filed
    /// as their full name and known to everyone as something else should read
    /// as the something else, in a transcript as much as in conversation —
    /// Contacts was told the nickname for exactly that reason.
    pub name: String,
    /// The name on the record, when a nickname is being shown instead of it.
    ///
    /// Kept so that the formal name still finds them. Both names reach the
    /// person; only one of them is shown.
    pub filed_as: Option<String>,
}

impl Contact {
    /// Whether this contact answers to `needle`: part of the name they are shown
    /// as, or part of the one they are filed under.
    ///
    /// `needle` must already be lowercased, since one query is matched against
    /// every contact rather than the other way round.
    pub fn answers_to(&self, needle: &str) -> bool {
        self.names().any(|name| name.contains(needle))
    }

    /// Whether one of those names is exactly `needle`, for breaking a tie
    /// between the people a fragment matched.
    pub fn is_named(&self, needle: &str) -> bool {
        self.names().any(|name| name == needle)
    }

    /// Every name this contact can be found by, lowercased for comparison.
    fn names(&self) -> impl Iterator<Item = String> {
        [Some(&self.name), self.filed_as.as_ref()]
            .into_iter()
            .flatten()
            .map(|name| name.to_lowercase())
    }
}

/// Handle-to-contact, plus why it is emptier than expected when it is.
///
/// Contacts is best-effort — messages still read without it — but silently
/// best-effort is how an empty index went unnoticed until names stopped
/// resolving with no explanation anywhere.
#[derive(Debug, Default, Clone)]
pub struct ContactIndex {
    contacts: HashMap<String, Contact>,
    problems: Vec<String>,
}

impl ContactIndex {
    /// The index that knows nothing, used whenever `--no-names` is in force.
    pub fn empty() -> Self {
        Self::default()
    }

    /// An index built from `(handle, record id, name)`, for tests.
    ///
    /// Keyed through [`handle_key`] like the real loader, so a test that writes
    /// a number in one shape and looks it up in another behaves the same way the
    /// program does. The record id is spelled out rather than derived from the
    /// name, so a test can hand two different people the same name.
    #[cfg(test)]
    pub fn for_test<'a>(records: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>) -> Self {
        Self {
            contacts: records
                .into_iter()
                .filter_map(|(handle, id, name)| {
                    let contact = Contact {
                        id: id.to_string(),
                        name: name.to_string(),
                        filed_as: None,
                    };
                    Some((handle_key(handle)?, contact))
                })
                .collect(),
            problems: Vec::new(),
        }
    }

    /// Give a handle's contact a nickname, which takes over as what they are
    /// shown as and pushes the name they had into `filed_as` — the same swap
    /// the loader performs, so a test cannot accidentally describe a contact
    /// the loader would never build.
    ///
    /// Separate from [`Self::for_test`] so the many tests that do not care
    /// about nicknames do not have to say so.
    #[cfg(test)]
    pub fn nicknamed(mut self, handle: &str, nickname: &str) -> Self {
        let key = handle_key(handle).expect("a handle to nickname");
        let contact = self.contacts.get_mut(&key).expect("a contact to nickname");
        contact.filed_as = Some(std::mem::replace(&mut contact.name, nickname.to_string()));
        self
    }

    /// The name for a handle, or `None` when it is unknown.
    pub fn lookup(&self, handle: Option<&str>) -> Option<&str> {
        Some(self.contact(handle)?.name.as_str())
    }

    /// The whole record, for callers that have to tell two people apart rather
    /// than print one.
    pub fn contact(&self, handle: Option<&str>) -> Option<&Contact> {
        let key = handle_key(handle?)?;
        self.contacts.get(&key)
    }

    /// Whether one of a conversation's `handles` — the comma-joined list the
    /// chat queries build — belongs to someone who answers to `needle`.
    ///
    /// The names these handles render as are already searched, because they are
    /// what the conversation is shown as. This is what reaches the other name —
    /// the one a nickname is displayed instead of, which appears nowhere and so
    /// can only be found on purpose.
    pub fn any_answers_to(&self, handles: Option<&str>, needle: &str) -> bool {
        self.any(handles, &|contact| contact.answers_to(needle))
    }

    /// The same question asked exactly, for breaking a tie between the
    /// conversations a fragment matched.
    pub fn any_named(&self, handles: Option<&str>, needle: &str) -> bool {
        self.any(handles, &|contact| contact.is_named(needle))
    }

    fn any(&self, handles: Option<&str>, matches: &dyn Fn(&Contact) -> bool) -> bool {
        handles.is_some_and(|handles| {
            handles
                .split(',')
                .filter_map(|handle| self.contact(Some(handle)))
                .any(matches)
        })
    }

    /// How many handles the index knows about.
    pub fn len(&self) -> usize {
        self.contacts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.contacts.is_empty()
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
    let sources: Vec<(PathBuf, HashMap<String, Contact>)> = databases
        .into_iter()
        .map(|path| {
            let contacts = read_source(&path, &mut problems);
            (path, contacts)
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
    let mut contacts = HashMap::new();
    let first = sources.iter().filter(|(path, _)| matches_preferred(path));
    let rest = sources.iter().filter(|(path, _)| !matches_preferred(path));
    for (_, source) in first.chain(rest) {
        merge(&mut contacts, source);
    }

    ContactIndex { contacts, problems }
}

fn merge(into: &mut HashMap<String, Contact>, from: &HashMap<String, Contact>) {
    for (key, contact) in from {
        into.entry(key.clone()).or_insert_with(|| contact.clone());
    }
}

const PHONES_SQL: &str = "SELECT p.ZFULLNUMBER AS handle, r.Z_PK AS record,
                r.ZFIRSTNAME, r.ZLASTNAME, r.ZNICKNAME, r.ZORGANIZATION
           FROM ZABCDPHONENUMBER p
           JOIN ZABCDRECORD r ON r.Z_PK = p.ZOWNER
          WHERE p.ZFULLNUMBER IS NOT NULL";

const EMAILS_SQL: &str = "SELECT e.ZADDRESS AS handle, r.Z_PK AS record,
                r.ZFIRSTNAME, r.ZLASTNAME, r.ZNICKNAME, r.ZORGANIZATION
           FROM ZABCDEMAILADDRESS e
           JOIN ZABCDRECORD r ON r.Z_PK = e.ZOWNER
          WHERE e.ZADDRESS IS NOT NULL";

/// Read one Contacts database. A source that cannot be opened is skipped; the
/// others still count.
fn read_source(path: &Path, problems: &mut Vec<String>) -> HashMap<String, Contact> {
    let mut contacts = HashMap::new();
    let db = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(db) => db,
        Err(error) => {
            problems.push(format!("{}: {error}", path.display()));
            return contacts;
        }
    };

    // Record ids are only unique within their own database, so the source they
    // came from goes in front of them. The legacy top-level database has no
    // directory to name it, and calling that one `local` is enough to keep it
    // from colliding with an account's.
    let source = source_id_of(path).unwrap_or_else(|| "local".to_string());
    for sql in [PHONES_SQL, EMAILS_SQL] {
        if let Err(error) = collect(&db, sql, &source, &mut contacts) {
            problems.push(format!("{}: {error}", path.display()));
        }
    }
    contacts
}

fn collect(
    db: &Connection,
    sql: &str,
    source: &str,
    contacts: &mut HashMap<String, Contact>,
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
        let Ok(record) = row.get::<_, i64>("record") else {
            continue;
        };
        let Some((name, filed_as)) = person_names(row) else {
            continue;
        };
        // Sources are visited primary first, so the first name for a handle is
        // the one from the account the user actually maintains.
        contacts.entry(key).or_insert(Contact {
            id: format!("{source}:{record}"),
            name,
            filed_as,
        });
    }
    Ok(())
}

/// What to call a record, and what else it answers to.
///
/// A nickname wins the display, because it is what the person is actually
/// called and recording it in Contacts is how you say so. The filed name comes
/// back as the second half of the pair so that it still finds them: both names
/// reach the person, and only the nickname is shown.
///
/// When there is no nickname the filed name is shown and there is no second
/// name at all, which is also the shape for a record that is only an
/// organization.
fn person_names(row: &rusqlite::Row<'_>) -> Option<(String, Option<String>)> {
    let text = |column: &str| -> Option<String> {
        row.get::<_, Option<String>>(column)
            .ok()
            .flatten()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let first = text("ZFIRSTNAME");
    let last = text("ZLASTNAME");
    let filed = if first.is_some() || last.is_some() {
        let parts: Vec<String> = [first, last].into_iter().flatten().collect();
        Some(parts.join(" "))
    } else {
        text("ZORGANIZATION")
    };
    match text("ZNICKNAME") {
        Some(nickname) => Some((nickname, filed)),
        None => Some((filed?, None)),
    }
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
        ContactIndex::for_test([
            ("3105551234", "test:1", "Dana Reyes"),
            ("4155559876", "test:2", "Sam Oyelaran"),
        ])
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

    /// The name/nickname split, read through the SQL that ships, so the column
    /// names being right is part of what passes.
    #[test]
    fn shows_the_nickname_and_keeps_the_filed_name_to_be_found_by() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE ZABCDRECORD (Z_PK INTEGER PRIMARY KEY, ZFIRSTNAME TEXT,
               ZLASTNAME TEXT, ZNICKNAME TEXT, ZORGANIZATION TEXT);
             CREATE TABLE ZABCDPHONENUMBER (ZOWNER INTEGER, ZFULLNUMBER TEXT);
             INSERT INTO ZABCDRECORD VALUES
               (1, 'Robin', 'Adeyemi', 'Rocket', NULL),
               (2, NULL, NULL, 'Rocket', NULL);
             INSERT INTO ZABCDPHONENUMBER (ZOWNER, ZFULLNUMBER) VALUES
               (1, '+13105551234'), (2, '(415) 555-9876');",
        )
        .unwrap();

        let mut contacts = HashMap::new();
        collect(&db, PHONES_SQL, "test", &mut contacts).unwrap();
        let index = ContactIndex {
            contacts,
            problems: Vec::new(),
        };

        // The nickname is what shows; the filed name is kept to be found by.
        let named = index.contact(Some("+13105551234")).unwrap();
        assert_eq!(named.name, "Rocket");
        assert_eq!(named.filed_as.as_deref(), Some("Robin Adeyemi"));

        // With no filed name there is only the nickname, and nothing left over.
        let unnamed = index.contact(Some("+14155559876")).unwrap();
        assert_eq!(unnamed.name, "Rocket");
        assert_eq!(unnamed.filed_as, None);

        // Either way both are found by the nickname, and the one with a filed
        // name is still found by that too — displacing it must not hide it.
        assert!(named.answers_to("rocket") && unnamed.answers_to("rocket"));
        assert!(named.answers_to("adeyemi"));
    }

    /// The pair is two facts and stays two facts.
    ///
    /// `msg contacts` shows both, but it composes them at the edge. The index
    /// hands out the shown name and the displaced one separately, because a
    /// single field holding sometimes one name and sometimes two is one nobody
    /// downstream can act on without parsing a format this program invented.
    #[test]
    fn keeps_the_shown_name_and_the_filed_one_apart() {
        let index = index().nicknamed("3105551234", "Dee");

        let dee = index.contact(Some("+13105551234")).unwrap();
        assert_eq!(dee.name, "Dee");
        assert_eq!(dee.filed_as.as_deref(), Some("Dana Reyes"));
        assert_eq!(index.lookup(Some("+13105551234")), Some("Dee"));

        // Nothing displaced, nothing held: no second fact to report.
        let sam = index.contact(Some("+14155559876")).unwrap();
        assert_eq!(sam.name, "Sam Oyelaran");
        assert_eq!(sam.filed_as, None);
    }

    #[test]
    fn answers_to_both_of_a_contacts_names_whatever_the_case() {
        let index = index().nicknamed("3105551234", "Dee");
        let dana = index.contact(Some("+13105551234")).unwrap();

        assert!(dana.answers_to("dee"), "the nickname");
        assert!(dana.answers_to("reyes"), "part of the name");
        assert!(!dana.answers_to("sam"), "somebody else");

        // Exactly means exactly, or a fragment would settle every tie it made.
        assert!(dana.is_named("dee") && dana.is_named("dana reyes"));
        assert!(!dana.is_named("de") && !dana.is_named("dana"));
    }

    #[test]
    fn asks_a_whole_conversations_worth_of_handles_at_once() {
        let index = index().nicknamed("4155559876", "Sammy");
        let handles = Some("+13105551234,+14155559876");

        assert!(index.any_answers_to(handles, "sammy"));
        assert!(index.any_named(handles, "sammy"));
        assert!(!index.any_answers_to(handles, "rocket"));
        assert!(!index.any_answers_to(None, "sammy"));
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
