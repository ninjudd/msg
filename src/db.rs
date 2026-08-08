//! Read-only access to the Messages database.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, Row, params_from_iter};
use serde::{Deserialize, Serialize};

use crate::apple::{from_apple_date, message_body};
use crate::contacts::{Contact, ContactIndex, name_handles};
use crate::{Error, Result};

pub fn default_db() -> PathBuf {
    crate::home().join("Library/Messages/chat.db")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub rowid: i64,
    pub guid: String,
    pub is_from_me: bool,
    pub body: Option<String>,
    pub associated_message_type: i64,
    pub is_tapback: bool,
    #[serde(with = "crate::iso")]
    pub date: Option<DateTime<Utc>>,
    pub handle: Option<String>,
    pub contact_name: Option<String>,
    pub sender: String,
    pub chat_id: i64,
    pub chat_name: Option<String>,
    pub service: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chat {
    pub rowid: i64,
    pub guid: String,
    pub identifier: String,
    pub display_name: Option<String>,
    pub handles: Option<String>,
    pub named_handles: Option<String>,
    pub is_filtered: bool,
    pub member_count: i64,
    pub is_group: bool,
    #[serde(with = "crate::iso")]
    pub last_date: Option<DateTime<Utc>>,
    pub message_count: i64,
    pub name: String,
}

pub fn database_path(over: Option<&str>) -> PathBuf {
    if let Some(path) = over.filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    match std::env::var("MSG_DB") {
        Ok(path) if !path.is_empty() => PathBuf::from(path),
        _ => default_db(),
    }
}

/// Two ways out, and the daemon is the better one: it holds the grant instead of
/// the terminal, so nothing else run from that shell inherits it. See
/// docs/projects/all/daemon-and-permissions.md §1.
fn denied_message(location: &Path) -> String {
    format!(
        "cannot read {}\n\n\
         Install the daemon, which holds Full Disk Access so your terminal does not:\n  \
         msg daemon install\n\n\
         Or grant Full Disk Access to your terminal in System Settings > Privacy & Security >\n\
         Full Disk Access, then restart it.",
        location.display()
    )
}

fn reads_as_permission(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("authorization denied") || lower.contains("permission")
}

/// Does `needle` occur in `haystack`, ignoring ASCII case?
///
/// Byte-wise on purpose. The haystack is usually a typedstream blob rather than
/// a string, and the point is to look at all of it.
fn contains_ignoring_case(haystack: &[u8], needle: &[u8]) -> bool {
    let Some((first, rest)) = needle.split_first() else {
        return true;
    };
    let lower = first.to_ascii_lowercase();
    let upper = first.to_ascii_uppercase();
    haystack.windows(needle.len()).any(|window| {
        (window[0] == lower || window[0] == upper)
            && window[1..]
                .iter()
                .zip(rest)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
    })
}

/// `msg_body_has(text, attributedBody, needle)` — is the needle in this
/// message's body?
///
/// This exists because `CAST(attributedBody AS TEXT) LIKE ?` does not work, and
/// did not work in the TypeScript build either. SQLite hands a cast blob to
/// `LIKE` as a NUL-terminated string, and a typedstream blob is full of NULs
/// well before the text: measured, an 88-byte blob casts to 41 bytes. So the
/// match only ever saw the archive header, and the 97.6% of messages whose body
/// lives in `attributedBody` were unsearchable. Only the 2.4% that also fill
/// `message.text` ever matched, which is why searching for a common word
/// returned something and made the bug look like sparse results rather than a
/// broken predicate.
///
/// Scanning the raw blob is sound rather than approximate. `decode_attributed_body`
/// takes a slice of these same bytes and reads it as UTF-8, so any needle that
/// survives into the decoded body is present in the blob — this is a superset of
/// what the decoded filter accepts, which is exactly what a prefilter must be.
/// It over-matches when the needle also appears in an archived class name, and
/// the decode-and-check afterwards is what narrows that.
fn register_body_match(db: &Connection) -> rusqlite::Result<()> {
    use rusqlite::functions::FunctionFlags;

    db.create_scalar_function(
        "msg_body_has",
        3,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |context| {
            let needle = context.get::<String>(2)?;
            if needle.is_empty() {
                return Ok(true);
            }
            let needle = needle.as_bytes();
            // `message.text` is set for a minority of messages, and when it is
            // set it is the cheaper of the two to look at.
            if let Ok(text) = context.get::<String>(0)
                && contains_ignoring_case(text.as_bytes(), needle)
            {
                return Ok(true);
            }
            if let Ok(body) = context.get::<Vec<u8>>(1)
                && contains_ignoring_case(&body, needle)
            {
                return Ok(true);
            }
            Ok(false)
        },
    )
}

fn try_open(location: &Path) -> rusqlite::Result<Connection> {
    let db = Connection::open_with_flags(location, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    register_body_match(&db)?;
    {
        // Stepping rather than `query_row`, which reports an empty table as an
        // error — that would send a perfectly readable but empty database down
        // the snapshot path.
        let mut statement = db.prepare("SELECT 1 FROM message LIMIT 1")?;
        statement.query([])?.next()?;
    }
    Ok(db)
}

/// Open the Messages database read-only, falling back to a temporary snapshot
/// when the write-ahead log cannot be opened alongside it.
///
/// `allow_snapshot` is false for the daemon: the copy it read would never see
/// another message arrive. A one-shot CLI wants it.
pub fn open_database(path: Option<&str>, allow_snapshot: bool) -> Result<Connection> {
    let location = database_path(path);
    if !location.exists() {
        return Err(Error::AccessDenied(format!(
            "no Messages database at {}",
            location.display()
        )));
    }

    match try_open(&location) {
        Ok(db) => Ok(db),
        Err(error) => {
            let text = error.to_string();
            if reads_as_permission(&text) {
                return Err(Error::AccessDenied(denied_message(&location)));
            }
            if !allow_snapshot {
                // TCC denial reaches us as SQLite's "unable to open database
                // file", which is indistinguishable from a locked write-ahead
                // log. The CLI resolves that ambiguity by trying a snapshot; a
                // daemon has no snapshot to fall back to, so for it the answer
                // is always the permission.
                return Err(Error::AccessDenied(format!(
                    "{}\n\n{text}",
                    denied_message(&location)
                )));
            }
            open_snapshot(&location)
        }
    }
}

/// `mkdtemp(3)`, which creates the directory atomically with mode 0700.
pub fn temporary_directory(prefix: &str) -> std::io::Result<PathBuf> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let template = std::env::temp_dir().join(format!("{prefix}XXXXXX"));
    let mut bytes = template.into_os_string().into_vec();
    bytes.push(0);
    // SAFETY: `bytes` is a writable, NUL-terminated buffer ending in six Xs,
    // which is what mkdtemp requires; it edits those six bytes in place.
    let created = unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast::<libc::c_char>()) };
    if created.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    bytes.pop();
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

fn open_snapshot(location: &Path) -> Result<Connection> {
    let directory = temporary_directory("msg-")?;
    let copy = directory.join("chat.db");

    for suffix in ["", "-wal", "-shm"] {
        let from = with_suffix(location, suffix);
        if !from.exists() {
            continue;
        }
        if let Err(error) = std::fs::copy(&from, with_suffix(&copy, suffix)) {
            // TCC refuses the copy the same way it refused the open, but as an
            // errno rather than a SQLite message. Without this the user meets a
            // raw EPERM.
            return Err(
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
                ) {
                    Error::AccessDenied(denied_message(location))
                } else {
                    Error::from(error)
                },
            );
        }
    }
    let db = Connection::open_with_flags(&copy, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    register_body_match(&db)?;
    Ok(db)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    if suffix.is_empty() {
        return path.to_path_buf();
    }
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

const MESSAGE_COLUMNS: &str = "
  message.rowid AS rowid, message.guid AS guid, message.is_from_me AS isFromMe,
  message.text AS text, message.attributedBody AS attributedBody,
  message.associated_message_type AS associatedMessageType, message.date AS date,
  handle.id AS handle, chat_message_join.chat_id AS chatId,
  NULLIF(chat.display_name, '') AS chatName, message.service AS service
";

const MESSAGE_FROM: &str = "
  FROM message
    LEFT JOIN handle ON message.handle_id = handle.rowid
    JOIN chat_message_join ON chat_message_join.message_id = message.rowid
    JOIN chat ON chat.rowid = chat_message_join.chat_id
";

/// A column as text, with the empty string read as absent — SQLite is
/// dynamically typed, so a column holding a number reads as absent too, which is
/// what the TypeScript `typeof value === 'string'` check did.
fn text(row: &Row<'_>, column: &str) -> Option<String> {
    row.get::<_, Option<String>>(column)
        .ok()
        .flatten()
        .filter(|value| !value.is_empty())
}

fn number(row: &Row<'_>, column: &str) -> i64 {
    row.get::<_, Option<i64>>(column)
        .ok()
        .flatten()
        .unwrap_or(0)
}

fn to_message(row: &Row<'_>, contacts: &ContactIndex) -> Message {
    let is_from_me = number(row, "isFromMe") == 1;
    let handle = text(row, "handle");
    let associated_message_type = number(row, "associatedMessageType");
    let name = contacts.lookup(handle.as_deref()).map(str::to_string);
    let blob = row
        .get::<_, Option<Vec<u8>>>("attributedBody")
        .ok()
        .flatten();

    Message {
        rowid: number(row, "rowid"),
        guid: text(row, "guid").unwrap_or_default(),
        is_from_me,
        body: message_body(text(row, "text"), blob.as_deref()),
        associated_message_type,
        is_tapback: associated_message_type != 0,
        date: from_apple_date(row.get::<_, Option<i64>>("date").ok().flatten()),
        sender: if is_from_me {
            "me".to_string()
        } else {
            name.clone()
                .or_else(|| handle.clone())
                .unwrap_or_else(|| "unknown".to_string())
        },
        handle,
        contact_name: name,
        chat_id: number(row, "chatId"),
        chat_name: text(row, "chatName"),
        service: text(row, "service"),
    }
}

/// One person, and every address Messages knows them by.
///
/// A contact is not a handle. The same person arrives as a phone number in one
/// conversation and an email address in another, and Messages keeps a separate
/// `handle` row for each — which is why searching "what did they say" cannot be
/// a search of one handle, or of one conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    /// What to call them: the contact name when there is one, else the address.
    pub name: String,
    /// `handle.rowid` for every address that resolves to this person.
    pub handle_ids: Vec<i64>,
    /// The addresses themselves, for saying who was matched.
    pub handles: Vec<String>,
}

/// Whose messages a person filter keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sender {
    /// Only what they sent — `--from`.
    Only,
    /// What they sent, plus what I sent in a conversation that is just the two
    /// of us — `--with`.
    ///
    /// My own messages in a group are left out on purpose: they were addressed
    /// to the room, not to this person, so counting them would make every
    /// `--with` in a busy group chat return most of my own history.
    BothWays,
}

/// One person, and which direction of the traffic is wanted.
pub struct PersonFilter<'a> {
    pub person: &'a Person,
    pub sender: Sender,
}

pub struct FetchMessages<'a> {
    pub chat_id: Option<i64>,
    pub after_date: Option<i64>,
    pub after_rowid: Option<i64>,
    pub query: Option<&'a str>,
    /// Restrict to one person, across every conversation they appear in.
    pub person: Option<PersonFilter<'a>>,
    pub limit: i64,
    pub include_tapbacks: bool,
    pub include_filtered: bool,
    /// Take the oldest matches rather than the newest.
    ///
    /// Following a conversation wants this: the newest N above a watermark
    /// silently skips everything between, so a burst larger than one batch would
    /// lose its beginning. Reading wants the default, since a limit there means
    /// "the last N".
    pub oldest_first: bool,
}

impl Default for FetchMessages<'_> {
    fn default() -> Self {
        Self {
            chat_id: None,
            after_date: None,
            after_rowid: None,
            query: None,
            person: None,
            limit: 50,
            include_tapbacks: false,
            include_filtered: false,
            oldest_first: false,
        }
    }
}

/// The conversations that are just me and this person.
///
/// A chat whose membership is exactly one handle, and that handle is theirs.
/// `chat_handle_join` holds one row per membership rather than per message, so
/// this is a small table however long the history is.
fn one_to_one_chats(db: &Connection, person: &Person) -> Result<Vec<i64>> {
    let slots = std::iter::repeat_n("?", person.handle_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT mine.chat_id FROM chat_handle_join AS mine
          WHERE mine.handle_id IN ({slots})
            AND (SELECT COUNT(*) FROM chat_handle_join AS everyone
                  WHERE everyone.chat_id = mine.chat_id) = 1"
    );
    let mut statement = db.prepare(&sql)?;
    let ids = statement
        .query_map(
            params_from_iter(person.handle_ids.iter().map(|id| Value::from(*id))),
            |row| row.get::<_, i64>(0),
        )?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok(ids)
}

/// Fetch messages newest-first from the database, returned oldest-first.
pub fn fetch_messages(
    db: &Connection,
    options: &FetchMessages<'_>,
    contacts: &ContactIndex,
) -> Result<Vec<Message>> {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if let Some(chat_id) = options.chat_id {
        clauses.push("chat_message_join.chat_id = ?".into());
        params.push(chat_id.into());
    } else if !options.include_filtered {
        // Only when sweeping every conversation; naming one is explicit enough.
        clauses.push("chat.is_filtered = 0".into());
    }
    if let Some(after_date) = options.after_date {
        clauses.push("message.date > ?".into());
        params.push(after_date.into());
    }
    if let Some(after_rowid) = options.after_rowid {
        clauses.push("message.rowid > ?".into());
        params.push(after_rowid.into());
    }
    if !options.include_tapbacks {
        clauses.push("message.associated_message_type = 0".into());
    }
    // Before the body match, deliberately. This is an integer test against a
    // handful of ids, and putting it ahead of the `LIKE` means the blob is never
    // cast for a message this search was never going to return.
    if let Some(filter) = &options.person {
        let ids = &filter.person.handle_ids;
        let slots = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(",");
        // `is_from_me = 0` is not redundant. Messages stamps outgoing messages
        // with the *recipient's* handle, so matching the handle alone would
        // return my own messages as though they had sent them — the fixture
        // carries a row exactly like that.
        let sent_by_them = format!("(message.is_from_me = 0 AND message.handle_id IN ({slots}))");
        match filter.sender {
            Sender::Only => {
                clauses.push(sent_by_them);
                params.extend(ids.iter().map(|id| Value::from(*id)));
            }
            Sender::BothWays => {
                // Mine count only where the conversation is just the two of us.
                // Answered once, up front, rather than as a subquery: written
                // inline it became a correlated `COUNT` that SQLite re-ran per
                // candidate message, which cost more than the body scan it was
                // meant to be narrowing. `chat_handle_join` has one row per
                // membership, so asking separately is thousands of rows, once.
                let ours = one_to_one_chats(db, filter.person)?;
                if ours.is_empty() {
                    // No one-to-one with them, so there is no "my side" to add
                    // and `IN ()` is not valid SQL anyway.
                    clauses.push(sent_by_them);
                    params.extend(ids.iter().map(|id| Value::from(*id)));
                } else {
                    let chats = std::iter::repeat_n("?", ours.len())
                        .collect::<Vec<_>>()
                        .join(",");
                    clauses.push(format!(
                        "({sent_by_them}
                          OR (message.is_from_me = 1
                              AND chat_message_join.chat_id IN ({chats})))"
                    ));
                    params.extend(ids.iter().map(|id| Value::from(*id)));
                    params.extend(ours.iter().map(|id| Value::from(*id)));
                }
            }
        }
    }
    if let Some(query) = options.query {
        // Not `CAST(attributedBody AS TEXT) LIKE ?`, which silently matched
        // nothing but the archive header — see `register_body_match`.
        clauses.push("msg_body_has(message.text, message.attributedBody, ?)".into());
        params.push(Value::Text(query.to_string()));
    }

    let where_clause = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    // Ordering by rowid rather than date when taking the oldest: a watcher walks
    // rowids, and the two orders disagree for messages that arrive out of order.
    let order = if options.oldest_first {
        "ORDER BY message.rowid ASC"
    } else {
        "ORDER BY message.date DESC"
    };
    params.push(options.limit.into());

    let sql = format!("SELECT {MESSAGE_COLUMNS} {MESSAGE_FROM} {where_clause} {order} LIMIT ?");
    let mut statement = db.prepare(&sql)?;
    let mut rows = statement.query(params_from_iter(params))?;

    let mut messages = Vec::new();
    while let Some(row) = rows.next()? {
        messages.push(to_message(row, contacts));
    }

    if let Some(query) = options.query {
        let needle = query.to_lowercase();
        messages.retain(|message| {
            message
                .body
                .as_ref()
                .is_some_and(|body| body.to_lowercase().contains(&needle))
        });
    }
    if !options.oldest_first {
        messages.reverse();
    }
    Ok(messages)
}

/// How many chats to consider when a query has to be matched against names.
const NAME_SEARCH_SCAN: i64 = 5_000;

/// The chat list, with the last-activity date and message count coming from one
/// grouped pass rather than a subquery per conversation.
///
/// The dates come from `chat_message_join.message_date`, not from
/// `message.date`. That column is a copy Messages maintains by trigger, and it
/// exists in an index — `chat_message_join(chat_id, message_date, message_id)` —
/// so the whole aggregate is answered by a covering-index scan that never opens
/// `message` at all. Reading `message.date` instead means one random probe per
/// row, and there are as many rows as there are messages.
///
/// The copy was checked against the original before this was relied on, over a
/// database with 733,690 of these rows: none were zero, none were NULL, none
/// disagreed with `message.date`, and no conversation's maximum differed. On the
/// same database it took this query from 2213ms to 202ms, and made the cost flat
/// in the limit — `LIMIT 30` and `LIMIT 3000` now differ by 8ms, where before
/// both paid for every conversation.
///
/// `message_count` deliberately counts join rows without touching `message`,
/// which is what the subquery it replaced did.
const CHATS_SQL: &str = "
    SELECT * FROM (
      SELECT chat.rowid AS rowid, chat.guid AS guid,
             chat.chat_identifier AS chatIdentifier,
             NULLIF(chat.display_name, '') AS displayName,
             chat.is_filtered AS isFiltered,
             (SELECT GROUP_CONCAT(handle.id) FROM chat_handle_join
                JOIN handle ON handle.rowid = chat_handle_join.handle_id
               WHERE chat_handle_join.chat_id = chat.rowid) AS handles,
             (SELECT COUNT(*) FROM chat_handle_join
               WHERE chat_handle_join.chat_id = chat.rowid) AS memberCount,
             recent.lastDate AS lastDate,
             COALESCE(recent.messageCount, 0) AS messageCount
        FROM chat
        LEFT JOIN (SELECT chat_id, MAX(message_date) AS lastDate,
                          COUNT(*) AS messageCount
                     FROM chat_message_join GROUP BY chat_id) AS recent
          ON recent.chat_id = chat.rowid
    )
";

/// Fetch chats ordered by most recent activity.
pub fn fetch_chats(
    db: &Connection,
    query: Option<&str>,
    limit: i64,
    contacts: &ContactIndex,
    include_filtered: bool,
) -> Result<Vec<Chat>> {
    let mut params: Vec<Value> = Vec::new();
    let mut conditions: Vec<String> = if include_filtered {
        Vec::new()
    } else {
        vec!["isFiltered = 0".into()]
    };

    // Contact names live in the Contacts database, so a query that might match
    // one is filtered after the rows are named rather than in SQL.
    let filter_by_name = query.is_some() && !contacts.is_empty();
    if let Some(query) = query
        && !filter_by_name
    {
        conditions.push("(displayName LIKE ? OR chatIdentifier LIKE ? OR handles LIKE ?)".into());
        for _ in 0..3 {
            params.push(format!("%{query}%").into());
        }
    }

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };
    params.push(
        if filter_by_name {
            NAME_SEARCH_SCAN
        } else {
            limit
        }
        .into(),
    );

    let sql = format!("{CHATS_SQL} {where_clause} ORDER BY lastDate DESC LIMIT ?");
    let mut statement = db.prepare(&sql)?;
    let mut rows = statement.query(params_from_iter(params))?;

    let mut chats = Vec::new();
    while let Some(row) = rows.next()? {
        let display_name = text(row, "displayName");
        let handles = text(row, "handles");
        let identifier = text(row, "chatIdentifier").unwrap_or_default();
        let member_count = number(row, "memberCount");
        let named_handles = name_handles(contacts, handles.as_deref());
        chats.push(Chat {
            rowid: number(row, "rowid"),
            guid: text(row, "guid").unwrap_or_default(),
            name: display_name
                .clone()
                .or_else(|| named_handles.clone())
                .unwrap_or_else(|| identifier.clone()),
            identifier,
            display_name,
            handles,
            named_handles,
            // Messages sorts filtered conversations into categories, so any
            // nonzero value means the conversation is filtered.
            is_filtered: number(row, "isFiltered") != 0,
            member_count,
            is_group: member_count > 1,
            last_date: from_apple_date(row.get::<_, Option<i64>>("lastDate").ok().flatten()),
            message_count: number(row, "messageCount"),
        });
    }

    let Some(query) = query.filter(|_| filter_by_name) else {
        return Ok(chats);
    };
    let needle = query.to_lowercase();
    let matches =
        |value: Option<&String>| value.is_some_and(|value| value.to_lowercase().contains(&needle));
    chats.retain(|chat| {
        matches(Some(&chat.name))
            || matches(chat.display_name.as_ref())
            || matches(chat.handles.as_ref())
            || matches(Some(&chat.identifier))
    });
    chats.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    Ok(chats)
}

/// Turn the two person flags into one resolved filter.
///
/// Enforced here rather than only in the argument parser, because the daemon
/// answers whatever a client sends and a client is not obliged to be the CLI.
/// Silently preferring one flag over the other would answer a different question
/// from the one asked.
pub fn person_filter(
    db: &Connection,
    with: Option<&str>,
    from: Option<&str>,
    contacts: &ContactIndex,
) -> Result<Option<(Person, Sender)>> {
    match (with, from) {
        (Some(_), Some(_)) => Err(Error::other(
            "--with and --from ask different questions; use one",
        )),
        (Some(spec), None) => Ok(Some((
            resolve_person(db, spec, contacts)?,
            Sender::BothWays,
        ))),
        (None, Some(spec)) => Ok(Some((resolve_person(db, spec, contacts)?, Sender::Only))),
        (None, None) => Ok(None),
    }
}

/// Find one person, and gather every address they use.
///
/// The `handle` table is a few thousand rows even on a decade-old database, so
/// this reads it whole and matches in Rust rather than asking SQLite to. That
/// keeps the matching rules the same ones the rest of the program already uses:
/// [`handle_key`] for addresses, so `+13105551234` and `(310) 555-1234` are one
/// person, and the contact index for names.
///
/// Two addresses belong to the same person when they belong to the same Contacts
/// record — the record, not the name it renders as, because two records can
/// legitimately share a name and merging them would answer with two people's
/// messages under one. Addresses with no contact behind them are each their own
/// person, since there is nothing to join them by.
///
/// Matching happens in two passes, and the second is the point. Naming any one
/// address has to reach the rest: someone whose phone and email are one contact
/// is one person, so `--from <their email>` must find what they sent from their
/// phone. So the first pass decides *who* matched and the second gathers every
/// address those people have, whether or not it looks anything like what was
/// typed.
pub fn resolve_person(db: &Connection, spec: &str, contacts: &ContactIndex) -> Result<Person> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err(Error::other("no one to search for"));
    }

    let mut statement = db.prepare("SELECT rowid, id FROM handle")?;
    let mut rows = statement.query([])?;
    let mut known: Vec<(i64, String, Option<Contact>)> = Vec::new();
    while let Some(row) = rows.next()? {
        let rowid: i64 = row.get(0)?;
        let Ok(handle) = row.get::<_, String>(1) else {
            continue;
        };
        let contact = contacts.contact(Some(&handle)).cloned();
        known.push((rowid, handle, contact));
    }

    let lowered = trimmed.to_lowercase();
    let wanted_key = crate::contacts::handle_key(trimmed);
    // An address given outright matches on the same key the contact index uses,
    // so the shape it was typed in does not matter.
    let exactly = |handle: &str| match (wanted_key.as_ref(), crate::contacts::handle_key(handle)) {
        (Some(wanted), Some(key)) => *wanted == key,
        _ => false,
    };
    let loosely = |handle: &str, contact: Option<&Contact>| {
        contact.is_some_and(|contact| contact.name.to_lowercase().contains(&lowered))
            || handle.to_lowercase().contains(&lowered)
    };

    let identity = |handle: &str, contact: Option<&Contact>| match contact {
        Some(contact) => format!("contact:{}", contact.id),
        // Two shapes of one unknown number are still one person, which is the
        // most that can be said without a contact to join them by.
        None => format!(
            "handle:{}",
            crate::contacts::handle_key(handle).unwrap_or_else(|| handle.to_string())
        ),
    };

    let owners = |pick: &dyn Fn(&str, Option<&Contact>) -> bool| -> BTreeSet<String> {
        known
            .iter()
            .filter(|(_, handle, contact)| pick(handle, contact.as_ref()))
            .map(|(_, handle, contact)| identity(handle, contact.as_ref()))
            .collect()
    };

    // An address typed in full names one person outright, and only when none
    // does is the spec read as a fragment to search for. Otherwise an address
    // that happens to read as part of a longer one — `someone@example.com`
    // inside `notsomeone@example.com` — drags a stranger in and turns naming
    // somebody exactly into an ambiguity, which is the opposite of what naming
    // an address is for.
    let mut matched = owners(&|handle, _| exactly(handle));
    if matched.is_empty() {
        matched = owners(&loosely);
    }

    let mut people: BTreeMap<String, Person> = BTreeMap::new();
    for (rowid, handle, contact) in &known {
        let key = identity(handle, contact.as_ref());
        if !matched.contains(&key) {
            continue;
        }
        let person = people.entry(key).or_insert_with(|| Person {
            name: contact
                .as_ref()
                .map_or_else(|| handle.clone(), |contact| contact.name.clone()),
            handle_ids: Vec::new(),
            handles: Vec::new(),
        });
        person.handle_ids.push(*rowid);
        person.handles.push(handle.clone());
    }

    if people.is_empty() {
        return Err(Error::other(format!("no one matching {spec}")));
    }
    if people.len() == 1 {
        return Ok(people.into_values().next().expect("one match"));
    }

    // An exact name breaks a tie, the same way it does for a chat — unless two
    // records answer to it, which is the case this cannot silently pick from.
    let exact: Vec<Person> = people
        .values()
        .filter(|person| person.name.to_lowercase() == lowered)
        .cloned()
        .collect();
    if exact.len() == 1 {
        return Ok(exact.into_iter().next().expect("one match"));
    }

    Err(Error::other(format!(
        "{} people match {spec}: {}",
        people.len(),
        describe(people.values().take(6))
    )))
}

/// Label each person well enough to tell them apart, which their names alone may
/// not do. An address is added only where the name is ambiguous, so the usual
/// message stays a list of names.
fn describe<'a>(people: impl Iterator<Item = &'a Person> + Clone) -> String {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for person in people.clone() {
        *seen.entry(person.name.as_str()).or_default() += 1;
    }
    people
        .map(
            |person| match (seen.get(person.name.as_str()), person.handles.first()) {
                (Some(2..), Some(handle)) => format!("{} ({handle})", person.name),
                _ => person.name.clone(),
            },
        )
        .collect::<Vec<_>>()
        .join(", ")
}

/// Find a single chat by rowid, identifier, or name substring.
pub fn resolve_chat(db: &Connection, spec: &str, contacts: &ContactIndex) -> Result<Chat> {
    // Naming a chat outright reaches it even when Messages filters it.
    let is_rowid = !spec.is_empty() && spec.bytes().all(|byte| byte.is_ascii_digit());
    let matches: Vec<Chat> = if is_rowid {
        let wanted: i64 = spec
            .parse()
            .map_err(|_| Error::other(format!("no chat matching {spec}")))?;
        fetch_chats(db, None, 10_000, contacts, true)?
            .into_iter()
            .filter(|chat| chat.rowid == wanted)
            .collect()
    } else {
        fetch_chats(db, Some(spec), 50, contacts, true)?
    };

    if matches.is_empty() {
        return Err(Error::other(format!("no chat matching {spec}")));
    }
    if matches.len() == 1 {
        return Ok(matches.into_iter().next().expect("one match"));
    }

    let lowered = spec.to_lowercase();
    let mut exact: Vec<Chat> = matches
        .iter()
        .filter(|chat| chat.name.to_lowercase() == lowered)
        .cloned()
        .collect();
    if exact.len() == 1 {
        return Ok(exact.remove(0));
    }

    let names = matches
        .iter()
        .take(6)
        .map(|chat| format!("{} ({})", chat.name, chat.rowid))
        .collect::<Vec<_>>()
        .join(", ");
    Err(Error::other(format!(
        "{} chats match {spec}: {names}",
        matches.len()
    )))
}

/// The highest message rowid, used as a watermark when following new messages.
pub fn latest_rowid(db: &Connection) -> Result<i64> {
    let mut statement = db.prepare("SELECT MAX(rowid) AS max FROM message")?;
    let mut rows = statement.query([])?;
    Ok(match rows.next()? {
        Some(row) => number(row, "max"),
        None => 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apple::to_apple_date;

    /// The schema the daemon tests build, so both ends read the same fixture.
    const SCHEMA: &str = "
      CREATE TABLE handle (rowid INTEGER PRIMARY KEY, id TEXT);
      CREATE TABLE chat (
        rowid INTEGER PRIMARY KEY, guid TEXT, chat_identifier TEXT,
        display_name TEXT, is_filtered INTEGER DEFAULT 0
      );
      CREATE TABLE message (
        rowid INTEGER PRIMARY KEY, guid TEXT, text TEXT, attributedBody BLOB,
        is_from_me INTEGER DEFAULT 0, handle_id INTEGER,
        associated_message_type INTEGER DEFAULT 0, date INTEGER, service TEXT
      );
      CREATE TABLE chat_message_join (
        chat_id INTEGER, message_id INTEGER, message_date INTEGER DEFAULT 0
      );
      CREATE TABLE chat_handle_join (chat_id INTEGER, handle_id INTEGER);
    ";

    /// 2026-01-15T17:30:00Z, plus a minute per message.
    pub(crate) fn at(minutes: i64) -> i64 {
        let start = DateTime::parse_from_rfc3339("2026-01-15T17:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        to_apple_date(start + chrono::Duration::minutes(minutes)).expect("in range")
    }

    pub(crate) fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        register_body_match(&db).unwrap();
        db.execute_batch(SCHEMA).unwrap();
        db.execute_batch(
            "
            INSERT INTO handle (rowid, id) VALUES (1, '+13105551234'), (2, 'someone@example.com');
            INSERT INTO chat (rowid, guid, chat_identifier, display_name, is_filtered) VALUES
              (1, 'iMessage;-;+13105551234', '+13105551234', '', 0),
              (2, 'iMessage;+;chat9', 'chat9', 'Ship Room', 0),
              (3, 'SMS;-;+18885550000', '+18885550000', '', 1);
            INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (1, 1), (2, 1), (2, 2), (3, 1);
            ",
        )
        .unwrap();

        /// rowid, guid, body, from me, handle, associated type, date, chat.
        type Row = (i64, &'static str, &'static str, i64, i64, i64, i64, i64);

        let rows: [Row; 5] = [
            (1, "m1", "are you around later", 0, 1, 0, at(0), 1),
            (2, "m2", "after 6, yeah", 1, 1, 0, at(1), 1),
            (3, "m3", "deploy is green", 0, 2, 0, at(2), 2),
            (4, "m4", "your code is 123456", 0, 1, 0, at(3), 3),
            (5, "m5", "Liked \"after 6, yeah\"", 0, 1, 2000, at(4), 1),
        ];
        for (rowid, guid, body, from_me, handle, associated, date, chat) in rows {
            db.execute(
                "INSERT INTO message (rowid, guid, text, is_from_me, handle_id,
                     associated_message_type, date, service)
                 VALUES (?, ?, ?, ?, ?, ?, ?, 'iMessage')",
                rusqlite::params![rowid, guid, body, from_me, handle, associated, date],
            )
            .unwrap();
            // `message_date` is a copy Messages keeps in step by trigger, and
            // the chat list reads it instead of joining `message`. A fixture
            // that left it at zero would not exercise the query that ships.
            db.execute(
                "INSERT INTO chat_message_join (chat_id, message_id, message_date)
                 VALUES (?, ?, ?)",
                rusqlite::params![chat, rowid, date],
            )
            .unwrap();
        }
        db
    }

    fn bodies(messages: &[Message]) -> Vec<&str> {
        messages
            .iter()
            .map(|message| message.body.as_deref().unwrap_or(""))
            .collect()
    }

    #[test]
    fn lists_conversations_and_hides_the_filtered_ones() {
        let db = fixture();
        let chats = fetch_chats(&db, None, 30, &ContactIndex::empty(), false).unwrap();
        // Ordered by most recent activity, and chat 3 is the filtered one.
        let names: Vec<&str> = chats.iter().map(|chat| chat.name.as_str()).collect();
        assert_eq!(names, ["+13105551234", "Ship Room"]);
    }

    #[test]
    fn includes_filtered_conversations_when_asked() {
        let db = fixture();
        let chats = fetch_chats(&db, None, 30, &ContactIndex::empty(), true).unwrap();
        assert_eq!(chats.len(), 3);
        assert!(chats.iter().any(|chat| chat.is_filtered));
    }

    #[test]
    fn filters_chats_by_name() {
        let db = fixture();
        let chats = fetch_chats(&db, Some("Ship"), 30, &ContactIndex::empty(), false).unwrap();
        assert_eq!(chats.iter().map(|chat| chat.rowid).collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn counts_members_and_marks_groups() {
        let db = fixture();
        let chats = fetch_chats(&db, Some("Ship"), 30, &ContactIndex::empty(), false).unwrap();
        assert_eq!(chats[0].member_count, 2);
        assert!(chats[0].is_group);
    }

    #[test]
    fn returns_a_conversation_oldest_first_without_tapbacks() {
        let db = fixture();
        let messages = fetch_messages(
            &db,
            &FetchMessages {
                chat_id: Some(1),
                ..Default::default()
            },
            &ContactIndex::empty(),
        )
        .unwrap();
        assert_eq!(bodies(&messages), ["are you around later", "after 6, yeah"]);
        assert_eq!(messages[1].sender, "me");
    }

    #[test]
    fn includes_tapbacks_when_asked() {
        let db = fixture();
        let messages = fetch_messages(
            &db,
            &FetchMessages {
                chat_id: Some(1),
                include_tapbacks: true,
                ..Default::default()
            },
            &ContactIndex::empty(),
        )
        .unwrap();
        assert_eq!(messages.len(), 3);
        assert!(messages[2].is_tapback);
    }

    #[test]
    fn reaches_a_filtered_conversation_when_it_is_named_outright() {
        let db = fixture();
        let chat = resolve_chat(&db, "3", &ContactIndex::empty()).unwrap();
        let messages = fetch_messages(
            &db,
            &FetchMessages {
                chat_id: Some(chat.rowid),
                ..Default::default()
            },
            &ContactIndex::empty(),
        )
        .unwrap();
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn searches_message_bodies() {
        let db = fixture();
        let messages = fetch_messages(
            &db,
            &FetchMessages {
                query: Some("deploy"),
                ..Default::default()
            },
            &ContactIndex::empty(),
        )
        .unwrap();
        assert_eq!(bodies(&messages), ["deploy is green"]);
    }

    /// Both flags, side by side on the same fixture, because the whole point of
    /// having two is that they answer differently.
    ///
    /// Handle 1 talks in chat 1, a one-to-one, and in chat 2, a group. `m2` is
    /// mine, sent to them in the one-to-one.
    #[test]
    fn from_is_what_they_sent_and_with_is_the_exchange() {
        let db = fixture();
        let contacts = ContactIndex::empty();
        let person = resolve_person(&db, "+13105551234", &contacts).unwrap();

        let ask = |sender| {
            fetch_messages(
                &db,
                &FetchMessages {
                    person: Some(PersonFilter {
                        person: &person,
                        sender,
                    }),
                    ..Default::default()
                },
                &contacts,
            )
            .unwrap()
        };

        // Theirs only. `m2` is mine even though it carries their handle.
        assert_eq!(bodies(&ask(Sender::Only)), ["are you around later"]);
        // Theirs plus mine, and mine only because chat 1 is just the two of us.
        assert_eq!(
            bodies(&ask(Sender::BothWays)),
            ["are you around later", "after 6, yeah"]
        );
    }

    /// The bug this would have shipped with: Messages stamps outgoing messages
    /// with the recipient's handle, so a filter that only tested `handle_id`
    /// would report my own messages as theirs.
    #[test]
    fn a_message_i_sent_is_never_reported_as_one_they_sent() {
        let db = fixture();
        let contacts = ContactIndex::empty();
        let person = resolve_person(&db, "+13105551234", &contacts).unwrap();
        let mine: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM message WHERE is_from_me = 1 AND handle_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(mine > 0, "the fixture no longer covers the case");

        let messages = fetch_messages(
            &db,
            &FetchMessages {
                person: Some(PersonFilter {
                    person: &person,
                    sender: Sender::Only,
                }),
                ..Default::default()
            },
            &contacts,
        )
        .unwrap();
        assert!(
            messages.iter().all(|message| !message.is_from_me),
            "a message I sent came back from --from"
        );
    }

    /// A group is where the two flags could quietly become the same thing.
    #[test]
    fn with_does_not_drag_in_my_half_of_a_group_chat() {
        let db = fixture();
        db.execute(
            "INSERT INTO message (rowid, guid, text, is_from_me, handle_id,
                 associated_message_type, date, service)
             VALUES (6, 'm6', 'shipping now', 1, 2, 0, ?, 'iMessage')",
            rusqlite::params![at(5)],
        )
        .unwrap();
        db.execute(
            "INSERT INTO chat_message_join (chat_id, message_id, message_date)
             VALUES (2, 6, ?)",
            rusqlite::params![at(5)],
        )
        .unwrap();

        let contacts = ContactIndex::empty();
        // Handle 2 is only ever in chat 2, which is a group.
        let person = resolve_person(&db, "someone@example.com", &contacts).unwrap();
        let messages = fetch_messages(
            &db,
            &FetchMessages {
                person: Some(PersonFilter {
                    person: &person,
                    sender: Sender::BothWays,
                }),
                ..Default::default()
            },
            &contacts,
        )
        .unwrap();
        // Not "shipping now": I said that to the room, not to them.
        assert_eq!(bodies(&messages), ["deploy is green"]);
    }

    /// One person, two addresses, one answer — however they were named.
    ///
    /// The spec cases matter separately. Naming the *name* matches both handles
    /// on its own, so it would pass even if an address reached only itself;
    /// naming one address is what proves the rest of the contact comes with it.
    #[test]
    fn a_contact_is_searched_by_every_address_they_use() {
        let db = fixture();
        let contacts = ContactIndex::for_test([
            ("+13105551234", "source:7", "Sam Rivera"),
            ("someone@example.com", "source:7", "Sam Rivera"),
        ]);

        for spec in [
            "Sam",
            "+13105551234",
            "someone@example.com",
            "(310) 555-1234",
        ] {
            let person = resolve_person(&db, spec, &contacts).unwrap();
            assert_eq!(person.name, "Sam Rivera", "resolving {spec}");
            assert_eq!(person.handle_ids.len(), 2, "resolving {spec}: {person:?}");

            let messages = fetch_messages(
                &db,
                &FetchMessages {
                    person: Some(PersonFilter {
                        person: &person,
                        sender: Sender::Only,
                    }),
                    ..Default::default()
                },
                &contacts,
            )
            .unwrap();
            // One from each address, gathered under the one contact.
            assert_eq!(
                bodies(&messages),
                ["are you around later", "deploy is green"],
                "resolving {spec}"
            );
        }
    }

    /// An address names one person outright, even when a longer address
    /// contains it.
    ///
    /// `someone@example.com` reads as part of `notsomeone@example.com`, so the
    /// substring arm matched a second, unrelated contact and naming somebody
    /// exactly came back as an ambiguity. Nothing could recover it either: the
    /// tie-break after it compares *display names* against what was typed, and
    /// no name equals an email address. That contradicts the README, which says
    /// to name an address when a name is ambiguous.
    #[test]
    fn an_address_typed_in_full_beats_a_longer_one_containing_it() {
        let db = fixture();
        db.execute(
            "INSERT INTO handle (rowid, id) VALUES (3, 'notsomeone@example.com')",
            [],
        )
        .unwrap();
        let contacts = ContactIndex::for_test([
            ("someone@example.com", "source:7", "Sam Rivera"),
            ("notsomeone@example.com", "source:9", "Kit Alvarez"),
        ]);

        let person = resolve_person(&db, "someone@example.com", &contacts).unwrap();
        assert_eq!(person.name, "Sam Rivera", "{person:?}");
        assert_eq!(person.handles, ["someone@example.com"], "{person:?}");

        // The longer address is still reachable, and reaches only itself.
        let other = resolve_person(&db, "notsomeone@example.com", &contacts).unwrap();
        assert_eq!(other.name, "Kit Alvarez", "{other:?}");
        assert_eq!(other.handles, ["notsomeone@example.com"], "{other:?}");

        // A fragment of an address is still a fragment, and still ambiguous.
        let error = resolve_person(&db, "example.com", &contacts)
            .unwrap_err()
            .to_string();
        assert!(error.contains("2 people match"), "{error}");
    }

    /// Two people can answer to one name, and a rendered name is not an identity.
    ///
    /// Keying people by what they render as merges them, and a search then
    /// quietly returns two people's messages as one person's. Reporting the
    /// ambiguity is the least this can do; picking one silently is the thing it
    /// must not.
    #[test]
    fn two_contacts_sharing_a_name_are_two_people() {
        let db = fixture();
        let contacts = ContactIndex::for_test([
            ("+13105551234", "source:7", "Sam Rivera"),
            ("someone@example.com", "source:9", "Sam Rivera"),
        ]);

        let error = resolve_person(&db, "Sam Rivera", &contacts)
            .unwrap_err()
            .to_string();
        assert!(error.contains("2 people match"), "{error}");
        // Named twice over, so the addresses are what tell them apart.
        assert!(error.contains("+13105551234"), "{error}");
        assert!(error.contains("someone@example.com"), "{error}");

        // Naming one address still reaches exactly one of them.
        let person = resolve_person(&db, "someone@example.com", &contacts).unwrap();
        assert_eq!(person.handles, ["someone@example.com"], "{person:?}");
    }

    #[test]
    fn asking_for_both_directions_at_once_is_refused() {
        let db = fixture();
        let error = person_filter(
            &db,
            Some("+13105551234"),
            Some("+13105551234"),
            &ContactIndex::empty(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("--with and --from"), "{error}");
    }

    #[test]
    fn naming_nobody_says_so_rather_than_matching_everybody() {
        let db = fixture();
        let contacts = ContactIndex::empty();
        let error = resolve_person(&db, "nobody-by-that-name", &contacts)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no one matching"), "{error}");
    }

    /// The bug that made search look broken: a body that exists only in
    /// `attributedBody`, behind the NUL bytes of the archive header.
    ///
    /// `CAST(... AS TEXT)` hands `LIKE` a NUL-terminated string, so it never saw
    /// past the header and this message was unfindable. The blob here is shaped
    /// like a real one — header, class names, NULs, then the text.
    #[test]
    fn finds_a_body_that_lives_only_in_the_archived_blob() {
        let db = fixture();
        let mut blob: Vec<u8> = Vec::new();
        blob.extend_from_slice(b"\x04\x0bstreamtyped\x81\xe8\x03\x84\x01\x40\x84\x84\x84");
        blob.extend_from_slice(
            b"NSAttributedString\x00\x84\x84\x08NSObject\x00\x85\x92\x84\x84\x84",
        );
        blob.extend_from_slice(b"NSString\x01\x94\x84\x01\x2b");
        let text = b"pinball tonight";
        blob.push(u8::try_from(text.len()).unwrap());
        blob.extend_from_slice(text);

        // A NUL well before the text is what defeated the old predicate.
        assert!(blob.contains(&0), "the fixture blob must carry a NUL");
        assert!(
            blob.iter().position(|b| *b == 0).unwrap() < blob.len() - text.len(),
            "the NUL must come before the text, or this proves nothing"
        );

        db.execute(
            "INSERT INTO message (rowid, guid, text, attributedBody, is_from_me,
                 handle_id, associated_message_type, date, service)
             VALUES (7, 'm7', NULL, ?, 0, 1, 0, ?, 'iMessage')",
            rusqlite::params![blob, at(6)],
        )
        .unwrap();
        db.execute(
            "INSERT INTO chat_message_join (chat_id, message_id, message_date)
             VALUES (1, 7, ?)",
            rusqlite::params![at(6)],
        )
        .unwrap();

        let found = fetch_messages(
            &db,
            &FetchMessages {
                query: Some("pinball"),
                ..Default::default()
            },
            &ContactIndex::empty(),
        )
        .unwrap();
        assert_eq!(bodies(&found), ["pinball tonight"]);

        // And case-insensitively, which is what `LIKE` gave for free and a
        // byte-wise `instr` would have quietly taken away.
        for needle in ["PINBALL", "PinBall", "TONIGHT"] {
            let hit = fetch_messages(
                &db,
                &FetchMessages {
                    query: Some(needle),
                    ..Default::default()
                },
                &ContactIndex::empty(),
            )
            .unwrap();
            assert_eq!(bodies(&hit), ["pinball tonight"], "searching {needle}");
        }
    }

    #[test]
    fn the_body_predicate_looks_past_a_nul() {
        // Directly, so a failure points at the predicate rather than at SQL.
        assert!(contains_ignoring_case(b"abc\x00def", b"def"));
        assert!(contains_ignoring_case(b"abc\x00DEF", b"def"));
        assert!(contains_ignoring_case(b"\x00\x00hello", b"HELLO"));
        assert!(!contains_ignoring_case(b"abc\x00def", b"xyz"));
        // An empty needle matches, matching what a `%%` LIKE did.
        assert!(contains_ignoring_case(b"anything", b""));
        // Not a match that runs off the end.
        assert!(!contains_ignoring_case(b"ab", b"abc"));
    }

    #[test]
    fn search_skips_filtered_conversations_unless_asked() {
        let db = fixture();
        let hidden = fetch_messages(
            &db,
            &FetchMessages {
                query: Some("123456"),
                ..Default::default()
            },
            &ContactIndex::empty(),
        )
        .unwrap();
        assert!(hidden.is_empty());

        let shown = fetch_messages(
            &db,
            &FetchMessages {
                query: Some("123456"),
                include_filtered: true,
                ..Default::default()
            },
            &ContactIndex::empty(),
        )
        .unwrap();
        assert_eq!(shown.len(), 1);
    }

    /// The LIKE runs against the raw blob and can match bytes that are not in
    /// the decoded text, so the decoded filter has to run after it.
    #[test]
    fn search_filters_precisely_once_the_body_is_decoded() {
        let db = fixture();
        let messages = fetch_messages(
            &db,
            &FetchMessages {
                query: Some("YEAH"),
                ..Default::default()
            },
            &ContactIndex::empty(),
        )
        .unwrap();
        assert_eq!(bodies(&messages), ["after 6, yeah"]);
    }

    #[test]
    fn resolves_one_conversation_for_send_to_address() {
        let db = fixture();
        let chat = resolve_chat(&db, "Ship Room", &ContactIndex::empty()).unwrap();
        assert_eq!(chat.guid, "iMessage;+;chat9");
    }

    #[test]
    fn reports_a_name_that_matches_nothing() {
        let db = fixture();
        let error = resolve_chat(&db, "nothing-matches", &ContactIndex::empty())
            .unwrap_err()
            .to_string();
        assert!(error.contains("no chat matching"), "{error}");
    }

    /// `CHATS_SQL` reads `chat_message_join.message_date` rather than joining
    /// `message`, which is only correct because Messages keeps the two in step.
    /// Verified over 733,690 rows of a real database before it was relied on;
    /// this keeps the fixture honest, so a row added without a date fails here
    /// rather than silently making the chat list a liar.
    #[test]
    fn the_copied_date_agrees_with_the_message_it_copies() {
        let db = fixture();
        let differing: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM chat_message_join j
                   JOIN message m ON m.rowid = j.message_id
                  WHERE j.message_date IS NOT m.date",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            differing, 0,
            "the fixture's message_date is not the message's date"
        );

        let zero: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM chat_message_join WHERE message_date = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(zero, 0, "the fixture left message_date at its default");
    }

    /// The rewrite exists to make the cost flat in the limit. Ordering is what
    /// could regress silently, so it is asserted rather than assumed.
    #[test]
    fn chats_come_back_newest_first_whatever_the_limit() {
        let db = fixture();
        for limit in [1, 2, 3, 30] {
            let chats = fetch_chats(&db, None, limit, &ContactIndex::empty(), true).unwrap();
            let dates: Vec<_> = chats.iter().map(|chat| chat.last_date).collect();
            let mut sorted = dates.clone();
            sorted.sort_by(|a, b| b.cmp(a));
            assert_eq!(dates, sorted, "limit {limit} came back out of order");
        }
    }

    /// A conversation with no messages still appears, with no date and a count
    /// of zero — the LEFT JOIN is what preserves that.
    #[test]
    fn a_conversation_with_no_messages_still_appears() {
        let db = fixture();
        db.execute(
            "INSERT INTO chat (rowid, guid, chat_identifier, display_name, is_filtered)
             VALUES (9, 'iMessage;+;empty', 'empty', 'Empty Room', 0)",
            [],
        )
        .unwrap();
        let chats = fetch_chats(&db, Some("Empty Room"), 30, &ContactIndex::empty(), true).unwrap();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].last_date, None);
        assert_eq!(chats[0].message_count, 0);
    }

    #[test]
    fn latest_rowid_is_the_watermark() {
        let db = fixture();
        assert_eq!(latest_rowid(&db).unwrap(), 5);
    }

    #[test]
    fn oldest_first_walks_rowids_forward() {
        let db = fixture();
        let messages = fetch_messages(
            &db,
            &FetchMessages {
                after_rowid: Some(0),
                limit: 2,
                include_filtered: true,
                oldest_first: true,
                ..Default::default()
            },
            &ContactIndex::empty(),
        )
        .unwrap();
        assert_eq!(
            messages
                .iter()
                .map(|message| message.rowid)
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }

    /// The wire format is the seam the TypeScript client still reads
    /// (rust-rewrite §4), so the field names and the date shape are load-bearing.
    #[test]
    fn serialises_the_way_the_typescript_client_expects() {
        let db = fixture();
        let messages = fetch_messages(
            &db,
            &FetchMessages {
                chat_id: Some(1),
                ..Default::default()
            },
            &ContactIndex::empty(),
        )
        .unwrap();
        let json = serde_json::to_value(&messages[0]).unwrap();
        assert_eq!(json["isFromMe"], serde_json::json!(false));
        assert_eq!(json["chatId"], serde_json::json!(1));
        assert_eq!(json["isTapback"], serde_json::json!(false));
        assert_eq!(json["contactName"], serde_json::Value::Null);
        assert_eq!(json["date"], serde_json::json!("2026-01-15T17:30:00.000Z"));

        let chats = fetch_chats(&db, None, 30, &ContactIndex::empty(), false).unwrap();
        let json = serde_json::to_value(&chats[0]).unwrap();
        assert!(json.get("displayName").is_some());
        assert!(json.get("namedHandles").is_some());
        assert!(json.get("memberCount").is_some());
        assert!(json.get("lastDate").is_some());
    }
}
