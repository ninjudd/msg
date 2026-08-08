//! Read-only access to the Messages database.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, Row, params_from_iter};
use serde::{Deserialize, Serialize};

use crate::apple::{from_apple_date, message_body};
use crate::contacts::{ContactIndex, name_handles};
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

fn try_open(location: &Path) -> rusqlite::Result<Connection> {
    let db = Connection::open_with_flags(location, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
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
    Ok(Connection::open_with_flags(
        &copy,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?)
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

pub struct FetchMessages<'a> {
    pub chat_id: Option<i64>,
    pub after_date: Option<i64>,
    pub after_rowid: Option<i64>,
    pub query: Option<&'a str>,
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
            limit: 50,
            include_tapbacks: false,
            include_filtered: false,
            oldest_first: false,
        }
    }
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
    if let Some(query) = options.query {
        // The body lives in attributedBody when text is NULL, so match the raw
        // blob too and filter precisely once decoded.
        clauses.push("(message.text LIKE ? OR CAST(message.attributedBody AS TEXT) LIKE ?)".into());
        params.push(format!("%{query}%").into());
        params.push(format!("%{query}%").into());
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
             (SELECT MAX(message.date) FROM message
                JOIN chat_message_join ON chat_message_join.message_id = message.rowid
               WHERE chat_message_join.chat_id = chat.rowid) AS lastDate,
             (SELECT COUNT(*) FROM chat_message_join
               WHERE chat_message_join.chat_id = chat.rowid) AS messageCount
        FROM chat
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
      CREATE TABLE chat_message_join (chat_id INTEGER, message_id INTEGER);
      CREATE TABLE chat_handle_join (chat_id INTEGER, handle_id INTEGER);
    ";

    /// 2026-01-15T17:30:00Z, plus a minute per message.
    pub(crate) fn at(minutes: i64) -> i64 {
        let start = DateTime::parse_from_rfc3339("2026-01-15T17:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        to_apple_date(start + chrono::Duration::minutes(minutes))
    }

    pub(crate) fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
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
            db.execute(
                "INSERT INTO chat_message_join (chat_id, message_id) VALUES (?, ?)",
                rusqlite::params![chat, rowid],
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
