//! Name lookup for handles, backed by the macOS Contacts database.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::Error;

/// Digits kept when matching phone numbers, which covers NANP numbers in full.
const MATCH_DIGITS: usize = 10;

/// `MSG_ADDRESSBOOK` points the loader at another AddressBook directory, the
/// way `MSG_DB` points at another `chat.db` — a fixture stays a fixture. Like
/// `MSG_DB` it is deliberately not carried into the installed daemon, which
/// would pin a long-lived process to a directory that outlives the shell that
/// named it.
fn address_book() -> PathBuf {
    match std::env::var("MSG_ADDRESSBOOK") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => crate::home().join("Library/Application Support/AddressBook"),
    }
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
    /// The address this entry was reached by, in the shape Contacts stored it.
    ///
    /// The map is keyed by [`handle_key`], which is lossy on purpose — it drops
    /// a country code so two shapes of one number compare equal. That is right
    /// for matching and wrong for showing, so the original is kept here. One
    /// record appears once per address it has, each carrying its own.
    pub handle: String,
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
    /// What Contacts calls this address: `mobile`, `home`, a custom string, or
    /// nothing. Apple's `_$!<Mobile>!$_` constants are unwrapped and
    /// lowercased; a label somebody typed themselves is kept as typed.
    pub label: Option<String>,
}

impl Contact {
    /// Whether this contact answers to `needle`: part of the name they are shown
    /// as, or part of the one they are filed under.
    ///
    /// Part, but from the start of a word. A first name inside somebody else's
    /// surname is not a match — `ana` reaches Ana Duarte and not Dana Reyes —
    /// which is `naming-a-conversation.md §2`, found by a first name resolving
    /// to three different people.
    ///
    /// `needle` must already be lowercased, since one query is matched against
    /// every contact rather than the other way round.
    pub fn answers_to(&self, needle: &str) -> bool {
        self.names()
            .any(|name| crate::matching::begins_a_word(name, needle))
    }

    /// Whether one of those names is exactly `needle`, for breaking a tie
    /// between the people a fragment matched.
    pub fn is_named(&self, needle: &str) -> bool {
        exactly_named(&self.name, self.filed_as.as_deref(), needle)
    }

    /// Every name this contact can be found by, in its stored case — the
    /// predicate folds per character, and reads word boundaries from the
    /// unfolded name.
    fn names(&self) -> impl Iterator<Item = &String> {
        [Some(&self.name), self.filed_as.as_ref()]
            .into_iter()
            .flatten()
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
                        handle: handle.to_string(),
                        name: name.to_string(),
                        filed_as: None,
                        label: None,
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

    /// Give a handle's entry a label, the way the loader would have read one.
    #[cfg(test)]
    pub fn labeled(mut self, handle: &str, label: &str) -> Self {
        let key = handle_key(handle).expect("a handle to label");
        let contact = self.contacts.get_mut(&key).expect("a contact to label");
        contact.label = Some(label.to_string());
        self
    }

    /// An index that failed to read a source, for pinning what `person` does
    /// about it.
    #[cfg(test)]
    pub fn with_problem(mut self, problem: &str) -> Self {
        self.problems.push(problem.to_string());
        self
    }

    /// The name for a handle, or `None` when it is unknown.
    pub fn lookup(&self, handle: Option<&str>) -> Option<&str> {
        Some(self.contact(handle)?.name.as_str())
    }

    /// Everyone a lookup term reaches: an address if it is one, otherwise
    /// everyone who answers to it as a name.
    ///
    /// `msg contacts` is asking who somebody is, and a name is the ordinary way
    /// to ask that — a number is what you have when you are asking *about* a
    /// number. Only addresses worked before, which meant the command could not
    /// answer the question it is for unless you already had the answer.
    ///
    /// An address given outright wins outright, the way it does when resolving
    /// a person: otherwise a name that happened to read as part of an address
    /// would drag strangers into a question that had exactly one answer.
    ///
    /// Stated exactly, because the two differ and the difference is visible:
    /// what wins is a term whose [`handle_key`] is one the index holds, and
    /// that key is every digit in the term with everything else discarded. So a
    /// name carrying digits can be read as an address — `R2D2` keys to `22`,
    /// and someone with a short code saved as `22` would answer a lookup meant
    /// for a company. Left alone deliberately: two digits colliding with a
    /// saved short code is plausible and has not been seen, and a guard for an
    /// unobserved case is untested code implying a problem exists. The shape it
    /// would take, if one ever turns up, is requiring an `@` or nothing but
    /// digits and separators before a term counts as an address.
    ///
    /// Ordered by name and then address, so repeating a lookup repeats its
    /// output. The map underneath is a `HashMap`, whose order is not stable
    /// even within one run.
    pub fn find(&self, term: &str) -> Vec<&Contact> {
        if let Some(exact) = self.contact(Some(term)) {
            return vec![exact];
        }
        let needle = term.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut found: Vec<&Contact> = self
            .contacts
            .values()
            .filter(|contact| contact.answers_to(&needle))
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name).then(a.handle.cmp(&b.handle)));
        found
    }

    /// The whole record, for callers that have to tell two people apart rather
    /// than print one.
    pub fn contact(&self, handle: Option<&str>) -> Option<&Contact> {
        let key = handle_key(handle?)?;
        self.contacts.get(&key)
    }

    /// One person from a term, assembled whole — the public resolver
    /// (contact-resolution.md §3).
    ///
    /// The rules are the ones conversation resolution uses: an address given
    /// outright wins outright, a name matches from the start of a word, and a
    /// whole name settles the tie a fragment created. What differs is the
    /// domain — everyone in Contacts rather than everyone in Messages — and
    /// that several survivors are an error naming them, never a pick.
    pub fn person(&self, term: &str) -> crate::Result<PersonRecord> {
        // An identity answer built on a partial read is not an answer. A
        // transcript renders names best-effort because a blank is better than
        // no transcript; a resolver that said "nobody" while a database sat
        // unread would state as fact what it did not check
        // (contact-resolution.md §7).
        if !self.problems.is_empty() {
            return Err(Error::AccessDenied(format!(
                "Contacts could not all be read:\n{}",
                self.problems.join("\n")
            )));
        }
        if let Some(exact) = self.contact(Some(term)) {
            let id = exact.id.clone();
            return Ok(self.assemble(&id));
        }
        let needle = term.trim().to_lowercase();
        if needle.is_empty() {
            return Err(Error::other("no one to resolve"));
        }
        // BTreeMap so candidates come out in one order however the underlying
        // map hashed them; one representative entry per record is enough to
        // name and to tie-break.
        let mut people: std::collections::BTreeMap<&str, &Contact> =
            std::collections::BTreeMap::new();
        for contact in self.contacts.values() {
            if contact.answers_to(&needle) {
                people.entry(contact.id.as_str()).or_insert(contact);
            }
        }
        if people.len() > 1 {
            let exact: Vec<&str> = people
                .iter()
                .filter(|(_, contact)| contact.is_named(&needle))
                .map(|(id, _)| *id)
                .collect();
            if exact.len() == 1 {
                people.retain(|id, _| *id == exact[0]);
            }
        }
        match people.len() {
            0 => Err(Error::other(format!("no one matching {term}"))),
            1 => {
                let id = people.keys().next().expect("one match").to_string();
                Ok(self.assemble(&id))
            }
            _ => Err(self.several(term, &people)),
        }
    }

    /// The error for a term naming more than one person: one candidate per
    /// line with an address alongside, which is what a caller disambiguates
    /// by (contact-resolution.md §7).
    fn several(&self, term: &str, people: &std::collections::BTreeMap<&str, &Contact>) -> Error {
        const LISTED: usize = 12;
        let mut lines: Vec<String> = people
            .keys()
            .take(LISTED)
            .map(|id| {
                let person = self.assemble(id);
                let address = person
                    .emails
                    .first()
                    .or_else(|| person.phones.first())
                    .map(|entry| entry.value.clone())
                    .unwrap_or_default();
                format!("  {} ({address})", person.name)
            })
            .collect();
        if people.len() > LISTED {
            lines.push(format!("  … and {} more", people.len() - LISTED));
        }
        Error::Ambiguous(format!(
            "{} people match {term}:\n{}",
            people.len(),
            lines.join("\n")
        ))
    }

    /// Everything the index holds for one record, as the person it is.
    fn assemble(&self, id: &str) -> PersonRecord {
        let mut named: Option<(String, Option<String>)> = None;
        let mut emails = Vec::new();
        let mut phones = Vec::new();
        for contact in self.contacts.values().filter(|contact| contact.id == id) {
            named = Some((contact.name.clone(), contact.filed_as.clone()));
            if contact.handle.contains('@') {
                emails.push(LabeledValue {
                    value: contact.handle.trim().to_string(),
                    label: contact.label.clone(),
                });
            } else {
                phones.push(LabeledValue {
                    value: emit_phone(&contact.handle),
                    label: contact.label.clone(),
                });
            }
        }
        let (name, filed_as) = named.expect("assemble called with a held record id");
        emails.sort_by(|a, b| a.value.cmp(&b.value));
        phones.sort_by(|a, b| a.value.cmp(&b.value));
        PersonRecord {
            id: id.to_string(),
            name,
            filed_as,
            emails,
            phones,
        }
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

/// Whether `needle` — already lowercased — is exactly one of somebody's two
/// names, whichever is shown. Shared between conversation resolution and
/// `contacts resolve`, so "exactly named" cannot drift into meaning two
/// different things (contact-resolution.md §3).
pub fn exactly_named(name: &str, filed_as: Option<&str>, needle: &str) -> bool {
    name.to_lowercase() == needle || filed_as.is_some_and(|filed| filed.to_lowercase() == needle)
}

/// One identifier and what Contacts calls it, the shape `resolve` publishes.
///
/// An object rather than a bare string from day one, because string → object
/// is the one breaking change that was visible from the start; anything this
/// grows later is additive (contact-resolution.md §5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabeledValue {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// A person assembled whole: one Contacts record and every address it
/// carries. This is the `--json` shape and the wire shape, one struct so the
/// two cannot disagree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonRecord {
    /// Opaque, and not durable across resyncs: the source UUID plus the
    /// record's Core Data primary key, unique for as long as anybody should
    /// hold it (contact-resolution.md §5).
    pub id: String,
    /// What to call them: the nickname when Contacts holds one.
    pub name: String,
    /// The name the record is filed under, when a nickname displaced it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filed_as: Option<String>,
    /// Always present, even empty, so a consumer never branches on a field
    /// that is sometimes missing.
    pub emails: Vec<LabeledValue>,
    pub phones: Vec<LabeledValue>,
}

/// A phone number the way a script wants it: separators dropped, digits and a
/// leading `+` kept, anything stranger passed through as stored.
///
/// The `+` is never invented. On a real database 64% of stored numbers
/// carried a country code and 36% did not, and `ZCOUNTRYCODE` was empty on
/// every row — so there is nothing to derive the missing ones from, and
/// assuming a region would be silently wrong outside it
/// (contact-resolution.md §9).
pub fn emit_phone(stored: &str) -> String {
    let trimmed = stored.trim();
    let stripped: String = trimmed
        .chars()
        .filter(|c| !matches!(c, ' ' | '(' | ')' | '-' | '.' | '/' | '\u{a0}'))
        .collect();
    let digits = stripped.strip_prefix('+').unwrap_or(&stripped);
    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
        stripped
    } else {
        trimmed.to_string()
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
fn source_databases(book: &Path, problems: &mut Vec<String>) -> Vec<PathBuf> {
    let directory = book.join("Sources");
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
fn contact_databases(book: &Path, problems: &mut Vec<String>) -> Vec<PathBuf> {
    if !book.exists() {
        return Vec::new();
    }
    let mut all = source_databases(book, problems);
    all.push(book.join("AddressBook-v22.abcddb"));
    all.retain(|path| path.exists());
    all
}

/// Read every Contacts source into a single handle-to-name map.
///
/// A missing or unreadable database yields an empty index rather than an error,
/// so reading messages still works without Contacts access.
pub fn load_contacts(preferred_source: Option<&str>) -> ContactIndex {
    load_contacts_from(None, preferred_source)
}

/// [`load_contacts`], from a named AddressBook directory instead of the real
/// one. The daemon's test harness passes the directory here the way it passes
/// `db_path` — an in-process daemon cannot be steered by an environment
/// variable without racing every other test in the binary.
pub fn load_contacts_from(book: Option<&Path>, preferred_source: Option<&str>) -> ContactIndex {
    let mut problems = Vec::new();
    let book = book.map_or_else(address_book, Path::to_path_buf);
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
    let databases = contact_databases(&book, &mut problems);
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

const PHONES_SQL: &str = "SELECT p.ZFULLNUMBER AS handle, p.ZLABEL AS label, r.Z_PK AS record,
                r.ZFIRSTNAME, r.ZLASTNAME, r.ZNICKNAME, r.ZORGANIZATION
           FROM ZABCDPHONENUMBER p
           JOIN ZABCDRECORD r ON r.Z_PK = p.ZOWNER
          WHERE p.ZFULLNUMBER IS NOT NULL";

const EMAILS_SQL: &str = "SELECT e.ZADDRESS AS handle, e.ZLABEL AS label, r.Z_PK AS record,
                r.ZFIRSTNAME, r.ZLASTNAME, r.ZNICKNAME, r.ZORGANIZATION
           FROM ZABCDEMAILADDRESS e
           JOIN ZABCDRECORD r ON r.Z_PK = e.ZOWNER
          WHERE e.ZADDRESS IS NOT NULL";

/// The label as the database stores it, made presentable. Apple's constants
/// arrive as `_$!<Mobile>!$_` and unwrap to `mobile`; a label somebody typed
/// themselves is kept as typed; empty and missing are the same absence. All
/// three shapes were observed on a real database, alongside a large
/// unlabeled bucket (contact-resolution.md §6).
fn tidy_label(raw: Option<String>) -> Option<String> {
    let label = raw?.trim().to_string();
    match label
        .strip_prefix("_$!<")
        .and_then(|rest| rest.strip_suffix(">!$_"))
    {
        Some(constant) => Some(constant.to_lowercase()),
        None if label.is_empty() => None,
        None => Some(label),
    }
}

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
            handle,
            name,
            filed_as,
            label: tidy_label(row.get::<_, Option<String>>("label").ok().flatten()),
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
             CREATE TABLE ZABCDPHONENUMBER (ZOWNER INTEGER, ZFULLNUMBER TEXT, ZLABEL TEXT);
             INSERT INTO ZABCDRECORD VALUES
               (1, 'Robin', 'Adeyemi', 'Rocket', NULL),
               (2, NULL, NULL, 'Rocket', NULL);
             INSERT INTO ZABCDPHONENUMBER (ZOWNER, ZFULLNUMBER, ZLABEL) VALUES
               (1, '+13105551234', '_$!<Mobile>!$_'), (2, '(415) 555-9876', NULL);",
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
        // Apple's label constant is unwrapped on the way in; no label is none.
        assert_eq!(named.label.as_deref(), Some("mobile"));
        assert_eq!(index.contact(Some("+14155559876")).unwrap().label, None);

        // With no filed name there is only the nickname, and nothing left over.
        let unnamed = index.contact(Some("+14155559876")).unwrap();
        assert_eq!(unnamed.name, "Rocket");
        assert_eq!(unnamed.filed_as, None);

        // Either way both are found by the nickname, and the one with a filed
        // name is still found by that too — displacing it must not hide it.
        assert!(named.answers_to("rocket") && unnamed.answers_to("rocket"));
        assert!(named.answers_to("adeyemi"));
    }

    /// A name is the ordinary way to ask who somebody is.
    ///
    /// A number is what you have when you are asking *about* a number, and
    /// before this the command took nothing else — so it could not answer its
    /// own question unless you already had the answer.
    #[test]
    fn finds_people_by_either_name_and_by_address() {
        let index = index().nicknamed("3105551234", "Dee");

        // The nickname, the filed name, and a fragment of either.
        for term in ["Dee", "dee", "Dana Reyes", "reyes"] {
            let found = index.find(term);
            assert_eq!(found.len(), 1, "looking up {term}: {found:?}");
            assert_eq!(found[0].name, "Dee", "looking up {term}");
            // The address comes back in the shape Contacts stored it, not the
            // digits the map is keyed by.
            assert_eq!(found[0].handle, "3105551234", "looking up {term}");
        }

        // An address still works, and still reaches exactly one person.
        let by_address = index.find("+1 (310) 555-1234");
        assert_eq!(by_address.len(), 1);
        assert_eq!(by_address[0].name, "Dee");

        assert!(index.find("nobody").is_empty());
        assert!(index.find("  ").is_empty());
    }

    /// An address given outright answers on its own, the way it does when
    /// resolving a person — otherwise naming somebody exactly could drag in
    /// whoever else happens to contain those characters.
    #[test]
    fn an_address_beats_a_name_that_contains_it() {
        let index = ContactIndex::for_test([
            ("dana@example.com", "test:1", "Dana Reyes"),
            ("+13105551234", "test:2", "dana@example.com is my email"),
        ]);

        let found = index.find("dana@example.com");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].name, "Dana Reyes", "{found:?}");
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

    #[test]
    fn resolves_one_person_whole() {
        let index = ContactIndex::for_test([
            ("+1 (310) 555-1234", "a:1", "Dana Reyes"),
            ("dana@example.com", "a:1", "Dana Reyes"),
            ("+14155550000", "a:2", "Sam Oyelaran"),
        ])
        .labeled("+13105551234", "mobile");
        let person = index.person("dana").unwrap();
        assert_eq!(person.id, "a:1");
        assert_eq!(person.name, "Dana Reyes");
        assert_eq!(person.emails.len(), 1);
        assert_eq!(person.emails[0].value, "dana@example.com");
        // Separators drop on the way out; the stored `+` survives (§9).
        assert_eq!(person.phones[0].value, "+13105551234");
        assert_eq!(person.phones[0].label.as_deref(), Some("mobile"));
    }

    #[test]
    fn several_people_are_an_error_naming_them() {
        let index = ContactIndex::for_test([
            ("+13105551234", "a:1", "Dana Reyes"),
            ("+14155550000", "a:2", "Dana Smith"),
        ]);
        match index.person("dana") {
            Err(crate::Error::Ambiguous(message)) => {
                assert!(message.contains("2 people match dana"), "{message}");
                assert!(
                    message.contains("Dana Reyes") && message.contains("Dana Smith"),
                    "{message}"
                );
                // An address beside each name, since it is what a caller
                // disambiguates by.
                assert!(message.contains("+13105551234"), "{message}");
            }
            other => panic!("expected ambiguity, got {other:?}"),
        }
    }

    #[test]
    fn a_whole_name_settles_what_a_fragment_started() {
        let index = ContactIndex::for_test([
            ("+13105551234", "a:1", "Bob"),
            ("+14155550000", "a:2", "Bobby Tables"),
        ]);
        assert_eq!(index.person("bob").unwrap().name, "Bob");
        assert!(matches!(
            index.person("bo"),
            Err(crate::Error::Ambiguous(_))
        ));
    }

    #[test]
    fn an_address_wins_outright() {
        let index = ContactIndex::for_test([
            ("dana@example.com", "a:1", "Dana Reyes"),
            ("+14155550000", "a:2", "Dana Smith"),
        ]);
        assert_eq!(index.person("dana@example.com").unwrap().id, "a:1");
    }

    #[test]
    fn a_partial_load_refuses_to_resolve() {
        let index = ContactIndex::for_test([("+13105551234", "a:1", "Dana Reyes")])
            .with_problem("Sources/x/AddressBook-v22.abcddb: unable to open database file");
        // The match is there and healthy, and that is not enough: an answer
        // from a partial read states as fact what was not checked (§7).
        match index.person("dana") {
            Err(crate::Error::AccessDenied(message)) => {
                assert!(message.contains("AddressBook-v22.abcddb"), "{message}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn nobody_matching_is_an_ordinary_error() {
        let index = ContactIndex::for_test([("+13105551234", "a:1", "Dana Reyes")]);
        assert!(matches!(index.person("zelda"), Err(crate::Error::Other(_))));
    }

    #[test]
    fn emit_phone_strips_separators_and_never_invents() {
        assert_eq!(emit_phone("+1 (310) 555-1234"), "+13105551234");
        assert_eq!(emit_phone("310.555.1234"), "3105551234");
        assert_eq!(emit_phone("  22000 "), "22000");
        // Anything stranger than separators passes through as stored.
        assert_eq!(emit_phone("310-555-1234 x89"), "310-555-1234 x89");
    }

    /// The documented shape, byte for byte: camelCase `filedAs`, `label`
    /// absent when there is none, and the arrays present even when empty
    /// (contact-resolution.md §5).
    #[test]
    fn the_json_shape_is_the_documented_one() {
        let index = ContactIndex::for_test([("+13105551234", "a:1", "Robert Chen")])
            .nicknamed("+13105551234", "Bob");
        let person = index.person("bob").unwrap();
        assert_eq!(
            serde_json::to_value(&person).unwrap(),
            serde_json::json!({
                "id": "a:1",
                "name": "Bob",
                "filedAs": "Robert Chen",
                "emails": [],
                "phones": [{"value": "+13105551234"}],
            })
        );
    }
}
