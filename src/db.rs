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
    /// Empty for the overwhelming majority of messages, so it stays out of the
    /// JSON rather than writing `[]` on every one of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// Set when this message is an inline reply. 0.75% of a real database.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<ReplyTo>,
}

/// One file attached to a message.
///
/// Deliberately not the path on disk. `attachment.filename` is absolute and
/// discloses the layout of the user's home directory to anything reading
/// `--json`; `transfer_name` is what the sender called it, which is what a
/// reader wants. See attachments.md §4.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub rowid: i64,
    pub name: Option<String>,
    pub mime_type: Option<String>,
    /// Apple's own type identifier, kept beside `mime_type` rather than folded
    /// into it. A UTI is not a MIME type, and slice 2 of attachments.md §6
    /// exists so a consumer can act on this field instead of parsing the body —
    /// which it cannot do if the field sometimes holds the other thing.
    pub uti: Option<String>,
    pub total_bytes: i64,
    pub is_sticker: bool,
    /// False when Messages kept the row but not the file — never downloaded, or
    /// purged since. 1,301 of 76,317 on a real database.
    pub is_downloaded: bool,
}

impl Attachment {
    /// What type to call this, for a reader rather than for a consumer.
    ///
    /// A MIME type when Messages recorded one, and otherwise Apple's UTI — which
    /// on a real database is the only type information 225 visible attachments
    /// have. The `dyn.…` kind macOS synthesizes for a file it cannot type names
    /// the extension back to itself, so those are dropped and the fallback can
    /// never make a description worse than saying nothing.
    fn kind(&self) -> Option<&str> {
        self.mime_type
            .as_deref()
            .or_else(|| self.uti.as_deref().filter(|uti| !uti.starts_with("dyn.")))
    }

    /// What stands in for this attachment in the body, in place of U+FFFC.
    ///
    /// The rowid leads, because it is the only way to name this file: the path
    /// is deliberately not published, and `msg save` takes an id. Printing it
    /// here is what makes it findable without a second command to list it.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.is_sticker {
            parts.push("sticker".to_string());
        }
        match (self.name.as_deref(), self.kind()) {
            (Some(name), _) => parts.push(name.to_string()),
            (None, Some(kind)) => parts.push(kind.to_string()),
            (None, None) if parts.is_empty() => parts.push("attachment".to_string()),
            (None, None) => {}
        }
        if !self.is_downloaded {
            parts.push("not downloaded".to_string());
        } else if self.total_bytes > 0 {
            parts.push(human_bytes(self.total_bytes));
        }
        format!("[#{} {}]", self.rowid, parts.join(", "))
    }
}

/// Sizes as a reader thinks of them, to one decimal place above a kilobyte.
pub fn human_bytes(bytes: i64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64 / 1024.0;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

/// The message a reply is answering.
///
/// Deliberately an excerpt rather than the whole thing: enough to recognise
/// which message is meant without reprinting a conversation inside itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplyTo {
    pub rowid: i64,
    pub sender: String,
    pub excerpt: Option<String>,
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

/// Does `needle` occur in `haystack`, ignoring case?
///
/// Bytes rather than a string because the haystack is usually a typedstream
/// blob: UTF-8 text with binary framing wrapped around it.
///
/// Case folds per character rather than per byte once the needle leaves ASCII.
/// `É` and `é` differ in both of their bytes, so a byte-wise fold reads them as
/// different letters — and this is the prefilter, so a row it rejects never
/// reaches the decoded filter that would have accepted it.
///
/// An ASCII needle skips all of that and folds bytes over the whole blob, which
/// is both the common case and the cheap one: lowercasing an ASCII character
/// never leaves ASCII, so only an ASCII character can match. Not quite only —
/// `K` U+212A lowercases to `k` — and that curiosity is knowingly not found.
fn contains_ignoring_case(haystack: &[u8], needle: &str) -> bool {
    if needle.is_ascii() {
        return contains_ignoring_ascii_case(haystack, needle.as_bytes());
    }
    // Characters have to be decoded to be folded, and the framing is not valid
    // UTF-8. Text never spans it, so each valid run is searched on its own and
    // the framing simply never matches.
    let mut rest = haystack;
    loop {
        match std::str::from_utf8(rest) {
            Ok(run) => return run_contains(run, needle),
            Err(error) => {
                let (valid, invalid) = rest.split_at(error.valid_up_to());
                if run_contains(std::str::from_utf8(valid).unwrap_or_default(), needle) {
                    return true;
                }
                // `error_len` is `None` only when the bytes end mid-character,
                // where the remainder is that partial character and nothing
                // follows it. Never zero either way, so this terminates.
                rest = &invalid[error.error_len().unwrap_or(invalid.len()).max(1)..];
            }
        }
    }
}

fn run_contains(haystack: &str, needle: &str) -> bool {
    let Some(first) = needle.chars().flat_map(char::to_lowercase).next() else {
        return true;
    };
    // Almost every position fails on its first character, so that test is worth
    // making cheap: comparing it before building the two folding iterators is
    // most of the difference between this and a scan that folds everything.
    haystack
        .char_indices()
        .any(|(at, ch)| folds_to(ch, first) && starts_with_ignoring_case(&haystack[at..], needle))
}

fn folds_to(ch: char, lowered: char) -> bool {
    if ch.is_ascii() {
        return ch.to_ascii_lowercase() == lowered;
    }
    ch.to_lowercase().next() == Some(lowered)
}

fn starts_with_ignoring_case(haystack: &str, needle: &str) -> bool {
    let mut found = haystack.chars().flat_map(char::to_lowercase);
    needle
        .chars()
        .flat_map(char::to_lowercase)
        .all(|wanted| found.next() == Some(wanted))
}

fn contains_ignoring_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
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
/// the decode-and-check afterwards is what narrows that. Both run
/// `contains_ignoring_case`, so the two cannot disagree about what a match is;
/// they differ only in what they are looking at.
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
            // `message.text` is set for a minority of messages, and when it is
            // set it is the cheaper of the two to look at.
            if let Ok(text) = context.get::<String>(0)
                && contains_ignoring_case(text.as_bytes(), &needle)
            {
                return Ok(true);
            }
            if let Ok(body) = context.get::<Vec<u8>>(1)
                && contains_ignoring_case(&body, &needle)
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
            // Whose advice this is depends on who is denied. `allow_snapshot`
            // is the daemon's tell — it is the one caller that has no snapshot
            // to fall back on — and telling a daemon to install a daemon helps
            // nobody. Put the right words in here rather than substituting them
            // on the way out: the wire now carries whatever an `AccessDenied`
            // was given, so this is the only place that decides.
            let denied = if allow_snapshot {
                denied_message(&location)
            } else {
                crate::daemon::protocol::DENIED.to_string()
            };
            if reads_as_permission(&text) {
                return Err(Error::AccessDenied(denied));
            }
            if !allow_snapshot {
                // TCC denial reaches us as SQLite's "unable to open database
                // file", which is indistinguishable from a locked write-ahead
                // log. The CLI resolves that ambiguity by trying a snapshot; a
                // daemon has no snapshot to fall back to, so for it the answer
                // is always the permission.
                return Err(Error::AccessDenied(format!("{denied}\n\n{text}")));
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
  NULLIF(chat.display_name, '') AS chatName, message.service AS service,
  NULLIF(message.thread_originator_guid, '') AS threadOriginatorGuid
";

const MESSAGE_FROM: &str = "
  FROM message
    LEFT JOIN handle ON message.handle_id = handle.rowid
    JOIN chat_message_join ON chat_message_join.message_id = message.rowid
    JOIN chat ON chat.rowid = chat_message_join.chat_id
";

/// What Messages leaves in the body where an attachment sits: U+FFFC OBJECT
/// REPLACEMENT CHARACTER. The file itself lives outside the database.
const PLACEHOLDER: char = '\u{fffc}';

/// How many message rowids one attachment lookup binds at a time.
///
/// `IN (?, ?, ...)` costs one host parameter per rowid, and SQLite refuses more
/// than `SQLITE_LIMIT_VARIABLE_NUMBER` of them — 32,766 in the bundled build,
/// but only 999 in versions before 3.32 and in anything built with the older
/// default. Nothing upstream caps a result set: `-n` takes any positive `i64`.
/// So this is batched rather than trusted, and 900 leaves room under every build
/// rather than under the one that happens to be linked here.
const ATTACHMENT_BATCH: usize = 900;

/// Every attachment on these messages, grouped by message rowid.
///
/// A second query rather than a join, because `MESSAGE_FROM` already joins
/// `chat_message_join` and adding another one-to-many would multiply the
/// message rows and make the limit mean something else again.
///
/// `hide_attachment` rows are excluded: measured on a real database, 483 of
/// 17,832 are referenced by their message body against 57,884 of 58,485 visible
/// ones, and 16,931 of them are extensionless files named `Attachment` with no
/// mime type. They are Messages' bookkeeping, not part of the conversation —
/// see attachments.md §7.
fn attachments_for(db: &Connection, rowids: &[i64]) -> Result<BTreeMap<i64, Vec<Attachment>>> {
    let mut found: BTreeMap<i64, Vec<Attachment>> = BTreeMap::new();

    // Deduplicated before batching, and the two are not independent. A message
    // in two conversations is two returned rows with one rowid, so this list can
    // hold it twice; `IN` is set membership, so within one batch that is
    // harmless, but across two batches it is two queries both pushing into the
    // same entry and the attachment is described twice. Sorting also means the
    // duplicates are never bound in the first place.
    let mut wanted = rowids.to_vec();
    wanted.sort_unstable();
    wanted.dedup();

    for batch in wanted.chunks(ATTACHMENT_BATCH) {
        let slots = vec!["?"; batch.len()].join(",");
        let sql = format!(
            "SELECT j.message_id AS messageId, a.ROWID AS rowid,
                    a.transfer_name AS name, a.mime_type AS mimeType, a.uti AS uti,
                    a.total_bytes AS totalBytes, a.is_sticker AS isSticker,
                    a.filename AS filename
               FROM message_attachment_join j
               JOIN attachment a ON a.ROWID = j.attachment_id
              WHERE j.message_id IN ({slots}) AND COALESCE(a.hide_attachment, 0) = 0
              ORDER BY j.message_id, a.ROWID"
        );
        let mut statement = db.prepare(&sql)?;
        let mut rows = statement.query(params_from_iter(batch.iter().copied()))?;
        while let Some(row) = rows.next()? {
            found
                .entry(number(row, "messageId"))
                .or_default()
                .push(Attachment {
                    rowid: number(row, "rowid"),
                    name: text(row, "name"),
                    mime_type: text(row, "mimeType"),
                    uti: text(row, "uti"),
                    total_bytes: number(row, "totalBytes"),
                    is_sticker: number(row, "isSticker") == 1,
                    is_downloaded: text(row, "filename").is_some(),
                });
        }
    }
    Ok(found)
}

/// Why one attachment could not be opened, told apart rather than guessed at.
///
/// "Gone" is a diagnosis, and `File::open` fails for more than one reason. A
/// permission error is the opposite of a missing file — it is there and this
/// process may not have it — and reporting it as absence sends a reader looking
/// for the wrong thing. It also has to exit 2 rather than 1, which is the
/// documented "the data is there, the grant is not".
///
/// Not a first-run problem: a database this program cannot read at all becomes
/// `AccessDenied` in `open_database` long before anything asks for an
/// attachment. This is the per-file case, which the daemon can meet while
/// holding the grant — see daemon-and-permissions.md §12 for the time it did.
pub fn unreadable(id: i64, error: &std::io::Error) -> Error {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        return Error::AccessDenied(format!(
            "attachment {id} is there, but reading it was refused ({error})"
        ));
    }
    // Messages purges files behind its own back, so a missing one is a normal
    // state rather than a broken database.
    Error::other(format!(
        "attachment {id} is recorded but its file is gone ({error})"
    ))
}

/// How much of the answered message to quote. Long enough to recognise, short
/// enough that a transcript does not contain itself twice over.
const EXCERPT: usize = 60;

/// What each of these messages is replying to, keyed by the reply's rowid.
///
/// A second query, like attachments, and for a sharper reason than shape: 19
/// replies on a real database sit in a *different* conversation from the message
/// they answer, so this cannot be scoped to the chat being read. `message.guid`
/// is `UNIQUE NOT NULL`, so the lookup rides an index Messages already keeps.
///
/// Built on `thread_originator_guid` rather than `reply_to_guid`. The second is
/// set on 15% of every message in the database and 115,304 of those rows have no
/// thread at all — it is not the user's reply, and threading on it would connect
/// a sixth of history at random. See threading.md §2.
fn replies_for(
    db: &Connection,
    wanted: &[(i64, String)],
    contacts: &ContactIndex,
) -> Result<BTreeMap<i64, ReplyTo>> {
    let mut found: BTreeMap<i64, ReplyTo> = BTreeMap::new();
    if wanted.is_empty() {
        return Ok(found);
    }

    let mut guids: Vec<&str> = wanted.iter().map(|(_, guid)| guid.as_str()).collect();
    guids.sort_unstable();
    guids.dedup();

    let mut originators: BTreeMap<String, (ReplyTo, Option<String>)> = BTreeMap::new();
    for batch in guids.chunks(ATTACHMENT_BATCH) {
        let slots = vec!["?"; batch.len()].join(",");
        let sql = format!(
            "SELECT message.guid AS guid, message.rowid AS rowid,
                    message.is_from_me AS isFromMe, message.text AS text,
                    message.attributedBody AS attributedBody, handle.id AS handle
               FROM message LEFT JOIN handle ON message.handle_id = handle.rowid
              WHERE message.guid IN ({slots})"
        );
        let mut statement = db.prepare(&sql)?;
        let mut rows = statement.query(params_from_iter(batch.iter().copied()))?;
        while let Some(row) = rows.next()? {
            let Some(guid) = text(row, "guid") else {
                continue;
            };
            let handle = text(row, "handle");
            let blob = row
                .get::<_, Option<Vec<u8>>>("attributedBody")
                .ok()
                .flatten();
            let sender = if number(row, "isFromMe") == 1 {
                "me".to_string()
            } else {
                contacts
                    .lookup(handle.as_deref())
                    .map(str::to_string)
                    .or(handle)
                    .unwrap_or_else(|| "unknown".to_string())
            };
            originators.insert(
                guid,
                (
                    ReplyTo {
                        rowid: number(row, "rowid"),
                        sender,
                        excerpt: None,
                    },
                    message_body(text(row, "text"), blob.as_deref()),
                ),
            );
        }
    }

    // An originator can have attachments of its own, and quoting the raw body
    // would put a bare U+FFFC in the excerpt — the very hole this program went
    // and fixed everywhere else. Described through the same function, so a photo
    // reads the same way in a quote as it does in the transcript.
    let quoted: Vec<i64> = originators.values().map(|(reply, _)| reply.rowid).collect();
    let attachments = attachments_for(db, &quoted)?;
    for (reply, body) in originators.values_mut() {
        let found = attachments.get(&reply.rowid).cloned().unwrap_or_default();
        reply.excerpt = describe_attachments(body.take(), &found).map(|body| excerpt(&body));
    }

    for (rowid, guid) in wanted {
        if let Some((originator, _)) = originators.get(guid) {
            found.insert(*rowid, originator.clone());
        }
    }
    Ok(found)
}

/// One line of the answered message, cut on a character boundary.
fn excerpt(body: &str) -> String {
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= EXCERPT {
        return flat;
    }
    let kept: String = flat.chars().take(EXCERPT).collect();
    format!("{}…", kept.trim_end())
}

/// Where one attachment's bytes are, by rowid.
///
/// This is the only function that turns an identifier into a path, and it goes
/// that way only. A caller names a row in `chat.db` and never a file, which is
/// what keeps the daemon from being a general-purpose reader with Full Disk
/// Access behind it — see daemon-and-permissions.md §6 and attachments.md §3.
///
/// Deliberately not filtered by `hide_attachment`: hiding governs what is worth
/// *showing* in a body, and someone who has an id in their hand has already got
/// past that question.
pub fn attachment_path(db: &Connection, rowid: i64) -> Result<(PathBuf, Attachment)> {
    let mut statement = db.prepare(
        "SELECT ROWID AS rowid, transfer_name AS name, mime_type AS mimeType,
                uti AS uti, total_bytes AS totalBytes, is_sticker AS isSticker,
                filename AS filename
           FROM attachment WHERE ROWID = ?",
    )?;
    let mut rows = statement.query([rowid])?;
    let Some(row) = rows.next()? else {
        return Err(Error::other(format!("no attachment {rowid}")));
    };

    let filename = text(row, "filename");
    let attachment = Attachment {
        rowid: number(row, "rowid"),
        name: text(row, "name"),
        mime_type: text(row, "mimeType"),
        uti: text(row, "uti"),
        total_bytes: number(row, "totalBytes"),
        is_sticker: number(row, "isSticker") == 1,
        is_downloaded: filename.is_some(),
    };
    let Some(filename) = filename else {
        return Err(Error::other(format!(
            "attachment {rowid} was never downloaded, so there is no file to save"
        )));
    };

    // `~` is Messages' own shorthand in this column, not a shell convention, so
    // it is expanded here rather than hoped over.
    let path = match filename.strip_prefix("~/") {
        Some(rest) => crate::home().join(rest),
        None => PathBuf::from(&filename),
    };
    Ok((path, attachment))
}

/// Put each attachment where its placeholder is, in order.
///
/// Positional because 13,148 messages on a real database carry more than one.
/// Neither side can be assumed to line up with the other, so both leftovers have
/// a defined rendering: a placeholder with nothing behind it becomes a generic
/// description, and an attachment the body never points at is appended rather
/// than dropped. A message that is nothing but a photo has no body at all, and
/// that is the case this whole slice exists for.
fn describe_attachments(body: Option<String>, attachments: &[Attachment]) -> Option<String> {
    // The fast path is every message that has nothing to do with attachments,
    // which is most of them. A body still carrying a placeholder does not take
    // it, even with no attachments to put there: §7 excludes the hidden rows,
    // and leaving their U+FFFC behind would reintroduce the hole for exactly
    // the messages this is meant to fix.
    if attachments.is_empty() && !body.as_deref().is_some_and(|b| b.contains(PLACEHOLDER)) {
        return body;
    }

    let mut next = attachments.iter();
    let mut written = String::new();
    for character in body.as_deref().unwrap_or_default().chars() {
        if character == PLACEHOLDER {
            match next.next() {
                Some(attachment) => written.push_str(&attachment.describe()),
                None => written.push_str("[attachment]"),
            }
        } else {
            written.push(character);
        }
    }
    for leftover in next {
        if !written.is_empty() && !written.ends_with(' ') {
            written.push(' ');
        }
        written.push_str(&leftover.describe());
    }

    (!written.is_empty()).then_some(written)
}

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
        attachments: Vec::new(),
        reply_to: None,
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
    /// The nickname behind that name, when Contacts holds one. Never shown; it
    /// is here so that naming it exactly settles a tie the way a name does.
    pub nickname: Option<String>,
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
    let sql = format!("SELECT {MESSAGE_COLUMNS} {MESSAGE_FROM} {where_clause} {order} LIMIT ?");
    let mut statement = db.prepare(&sql)?;

    // Ask for more than was wanted when the first answer came up short.
    //
    // The `LIMIT` bounds *raw* matches, and the decode below then drops the ones
    // where the needle was in the archive metadata rather than in the visible
    // body. So asking for 100 could return 99 while hundreds more matched: one
    // false positive inside the limit cost a real result instead of being
    // replaced by the next one. Fetching wider and trimming afterwards is what
    // makes the limit mean "up to this many matches" rather than "this many
    // candidates, minus however many were wrong".
    // The first ask is already generous, because a wider `LIMIT` is close to
    // free and a second pass is not. There is no early exit — §8 of
    // query-performance.md measured a tenfold limit costing the same — so the
    // scan dominates either way, and re-running it to recover from a handful of
    // false positives roughly doubles the query. Over-fetching once instead
    // keeps almost every search to a single pass.
    let wanted = usize::try_from(options.limit).unwrap_or(usize::MAX);
    let mut asking = if options.query.is_some() {
        options.limit.saturating_mul(4).saturating_add(64)
    } else {
        options.limit
    };
    let mut messages;
    let mut answering: BTreeMap<i64, String> = BTreeMap::new();
    loop {
        let mut round = params.clone();
        round.push(asking.into());
        let mut rows = statement.query(params_from_iter(round))?;

        let mut candidates = Vec::new();
        answering.clear();
        while let Some(row) = rows.next()? {
            // Read beside the message rather than stored on it. A guid names a
            // message and is no use to a reader; `reply_to` is the same question
            // answered in terms they can use, and it is the only one published.
            let message = to_message(row, contacts);
            if let Some(guid) = text(row, "threadOriginatorGuid") {
                answering.insert(message.rowid, guid);
            }
            candidates.push(message);
        }
        let exhausted = i64::try_from(candidates.len()).unwrap_or(i64::MAX) < asking;

        messages = candidates;
        if let Some(query) = options.query {
            // Deliberately the same predicate the SQL prefilter ran, so the two
            // agree on what a match is by construction rather than by matching
            // rules written twice.
            messages.retain(|message| {
                message
                    .body
                    .as_ref()
                    .is_some_and(|body| contains_ignoring_case(body.as_bytes(), query))
            });
        }

        // Nothing was dropped, or there is nothing further back to find. The
        // second is what stops this looping forever on a genuinely short result.
        if messages.len() >= wanted || exhausted || options.query.is_none() {
            break;
        }
        let Some(wider) = asking.checked_mul(4) else {
            break;
        };
        asking = wider;
    }
    messages.truncate(wanted);

    // After the limit and the body filter, so this only ever asks about the
    // messages actually being returned. Searching still matches the real body
    // rather than the description standing in for an attachment.
    let rowids: Vec<i64> = messages.iter().map(|message| message.rowid).collect();
    let attachments = attachments_for(db, &rowids)?;

    // Only the rows that survived the limit and are replies, which is 0.75% of
    // a real database — so this query is usually not made at all.
    let wanted: Vec<(i64, String)> = messages
        .iter()
        .filter_map(|message| {
            answering
                .get(&message.rowid)
                .map(|guid| (message.rowid, guid.clone()))
        })
        .collect();
    let replies = replies_for(db, &wanted, contacts)?;

    for message in &mut messages {
        // Cloned rather than taken. `MESSAGE_FROM` joins `chat_message_join`, so
        // a message in two conversations comes back as two rows with one rowid,
        // and removing would give the attachments to whichever arrived first and
        // leave the other reading `[attachment]` against an empty list.
        let found = attachments.get(&message.rowid).cloned().unwrap_or_default();
        message.body = describe_attachments(message.body.take(), &found);
        message.attachments = found;
        // Cloned for the same reason as the line above, and it is the same bug
        // when it is not: one rowid, two rows, and `remove` gives the quote to
        // whichever arrives first.
        message.reply_to = replies.get(&message.rowid).cloned();
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
            // A nickname is shown nowhere, so it is searched separately — but
            // only where the name it stands in for is searched too. A
            // conversation with a display name of its own is found by that name
            // and not by its members', and a nickname that reached inside one
            // would make itself easier to find someone by than their own name.
            || (chat.display_name.is_none()
                && contacts.any_answers_to(chat.handles.as_deref(), &needle))
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
        contact.is_some_and(|contact| contact.answers_to(&lowered))
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
            nickname: contact
                .as_ref()
                .and_then(|contact| contact.nickname.clone()),
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
    // A nickname counts as one of those names: typing someone's whole nickname
    // is as definite as typing their whole name, and it is the shape a nickname
    // is usually typed in.
    let exact: Vec<Person> = people
        .values()
        .filter(|person| {
            person.name.to_lowercase() == lowered
                || person
                    .nickname
                    .as_ref()
                    .is_some_and(|nickname| nickname.to_lowercase() == lowered)
        })
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
        .filter(|chat| {
            chat.name.to_lowercase() == lowered
                || (chat.display_name.is_none()
                    && contacts.any_named(chat.handles.as_deref(), &lowered))
        })
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
        associated_message_type INTEGER DEFAULT 0, date INTEGER, service TEXT,
        thread_originator_guid TEXT, thread_originator_part TEXT
      );
      CREATE TABLE chat_message_join (
        chat_id INTEGER, message_id INTEGER, message_date INTEGER DEFAULT 0
      );
      CREATE TABLE attachment (
        ROWID INTEGER PRIMARY KEY, guid TEXT, filename TEXT, uti TEXT,
        mime_type TEXT, transfer_state INTEGER DEFAULT 0, transfer_name TEXT,
        total_bytes INTEGER DEFAULT 0, is_sticker INTEGER DEFAULT 0,
        hide_attachment INTEGER DEFAULT 0
      );
      CREATE TABLE message_attachment_join (
        message_id INTEGER, attachment_id INTEGER
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

    /// A one-to-one conversation with somebody new, numbered `n` in both tables.
    ///
    /// The fixture's first handle is spread across three conversations, so
    /// resolving anyone by it is ambiguous however they were named — which is
    /// the fixture's doing and would hide what these tests are about.
    fn one_to_one(db: &Connection, n: i64, address: &str) -> i64 {
        db.execute(
            "INSERT INTO handle (rowid, id) VALUES (?, ?)",
            rusqlite::params![n, address],
        )
        .unwrap();
        db.execute(
            "INSERT INTO chat (rowid, guid, chat_identifier, display_name, is_filtered)
             VALUES (?, ?, ?, '', 0)",
            rusqlite::params![n, format!("iMessage;-;{address}"), address],
        )
        .unwrap();
        db.execute(
            "INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (?, ?)",
            rusqlite::params![n, n],
        )
        .unwrap();
        n
    }

    /// A nickname is shown nowhere, so typing it is the only use it has.
    #[test]
    fn a_conversation_is_found_by_the_nickname_behind_the_name() {
        let db = fixture();
        let rowid = one_to_one(&db, 4, "+16175550147");
        let contacts = ContactIndex::for_test([("+16175550147", "source:7", "Robin Adeyemi")])
            .nicknamed("+16175550147", "Rocket");

        let chats = fetch_chats(&db, Some("rocket"), 30, &contacts, false).unwrap();
        let names: Vec<&str> = chats.iter().map(|chat| chat.name.as_str()).collect();
        // Found by the nickname, and still shown as the name.
        assert_eq!(names, ["Robin Adeyemi"]);

        // Which is what reading and sending resolve through, so both take it.
        let chat = resolve_chat(&db, "Rocket", &contacts).unwrap();
        assert_eq!((chat.rowid, chat.name.as_str()), (rowid, "Robin Adeyemi"));
    }

    /// A person is found by their nickname too, so `--with` and `--from` take
    /// one — and gather every address, since what resolved is the contact.
    #[test]
    fn a_person_is_found_by_the_nickname_behind_the_name() {
        let db = fixture();
        let contacts = ContactIndex::for_test([
            ("+13105551234", "source:7", "Robin Adeyemi"),
            ("someone@example.com", "source:7", "Robin Adeyemi"),
        ])
        .nicknamed("+13105551234", "Rocket")
        .nicknamed("someone@example.com", "Rocket");

        let person = resolve_person(&db, "rocket", &contacts).unwrap();
        assert_eq!(person.name, "Robin Adeyemi", "{person:?}");
        assert_eq!(person.handle_ids.len(), 2, "{person:?}");
    }

    /// A nickname is short, so it is a fragment of plenty else. Typing the whole
    /// of one is as definite as typing a whole name, and settles the tie.
    #[test]
    fn an_exact_nickname_settles_the_tie_a_fragment_creates() {
        let db = fixture();
        let rowid = one_to_one(&db, 4, "+16175550147");
        one_to_one(&db, 5, "rocketry@example.com");
        let contacts = ContactIndex::for_test([("+16175550147", "source:7", "Robin Adeyemi")])
            .nicknamed("+16175550147", "Rocket");

        // Both match the fragment: one by nickname, one by address.
        let matched = fetch_chats(&db, Some("rocket"), 30, &contacts, false).unwrap();
        assert_eq!(matched.len(), 2, "{matched:?}");

        let chat = resolve_chat(&db, "rocket", &contacts).unwrap();
        assert_eq!(chat.rowid, rowid, "{chat:?}");
        let person = resolve_person(&db, "rocket", &contacts).unwrap();
        assert_eq!(person.handles, ["+16175550147"], "{person:?}");
    }

    /// A conversation with a name of its own is found by that name rather than
    /// by who is in it. A nickname must not be a way around that, or it would be
    /// easier to find someone by the name they are never shown as.
    #[test]
    fn a_named_group_is_no_more_findable_by_a_nickname_than_by_a_name() {
        let db = fixture();
        // Handle 2 is only in the Ship Room, which carries a display name.
        let contacts = ContactIndex::for_test([("someone@example.com", "source:7", "Kit Alvarez")])
            .nicknamed("someone@example.com", "Sparrow");

        for spec in ["sparrow", "Kit Alvarez"] {
            let chats = fetch_chats(&db, Some(spec), 30, &contacts, false).unwrap();
            assert!(chats.is_empty(), "searching {spec}: {chats:?}");
        }
    }

    /// rowid, sender's name for it, mime type, bytes, hidden, on disk.
    type Attached = (
        i64,
        Option<&'static str>,
        Option<&'static str>,
        i64,
        bool,
        bool,
    );

    /// Attach files to one message.
    fn attach(db: &Connection, message: i64, rows: &[Attached]) {
        for (rowid, name, mime, bytes, hidden, on_disk) in rows {
            db.execute(
                "INSERT INTO attachment (ROWID, guid, filename, mime_type, transfer_name,
                     total_bytes, is_sticker, hide_attachment)
                 VALUES (?, 'a', ?, ?, ?, ?, 0, ?)",
                rusqlite::params![
                    rowid,
                    on_disk.then_some("/some/path"),
                    mime,
                    name,
                    bytes,
                    hidden
                ],
            )
            .unwrap();
            db.execute(
                "INSERT INTO message_attachment_join (message_id, attachment_id) VALUES (?, ?)",
                rusqlite::params![message, rowid],
            )
            .unwrap();
        }
    }

    fn body_of(db: &Connection, rowid: i64) -> String {
        let found = fetch_messages(db, &FetchMessages::default(), &ContactIndex::empty()).unwrap();
        found
            .into_iter()
            .find(|message| message.rowid == rowid)
            .unwrap_or_else(|| panic!("message {rowid} missing"))
            .body
            .unwrap_or_default()
    }

    /// A message that is nothing but a photo used to print as one invisible
    /// character, which is the whole complaint this answers.
    #[test]
    fn an_attachment_stands_in_for_its_placeholder() {
        let db = fixture();
        db.execute("UPDATE message SET text = char(65532) WHERE rowid = 1", [])
            .unwrap();
        attach(
            &db,
            1,
            &[(
                1,
                Some("IMG_1234.HEIC"),
                Some("image/heic"),
                3_355_443,
                false,
                true,
            )],
        );
        assert_eq!(body_of(&db, 1), "[#1 IMG_1234.HEIC, 3.2 MB]");
    }

    /// 13,148 messages on a real database carry more than one, so the mapping
    /// has to be positional rather than one-per-message.
    #[test]
    fn several_attachments_land_in_the_order_their_placeholders_do() {
        let db = fixture();
        db.execute(
            "UPDATE message SET text = char(65532) || ' and ' || char(65532) WHERE rowid = 1",
            [],
        )
        .unwrap();
        attach(
            &db,
            1,
            &[
                (1, Some("first.png"), Some("image/png"), 2048, false, true),
                (
                    2,
                    Some("second.pdf"),
                    Some("application/pdf"),
                    1024,
                    false,
                    true,
                ),
            ],
        );
        assert_eq!(
            body_of(&db, 1),
            "[#1 first.png, 2.0 KB] and [#2 second.pdf, 1.0 KB]"
        );
    }

    /// Neither side can be assumed to line up with the other.
    #[test]
    fn leftovers_on_either_side_still_render() {
        let db = fixture();
        // Two placeholders, one attachment: the spare says only that it is one.
        db.execute(
            "UPDATE message SET text = char(65532) || char(65532) WHERE rowid = 1",
            [],
        )
        .unwrap();
        attach(
            &db,
            1,
            &[(1, Some("only.png"), Some("image/png"), 512, false, true)],
        );
        assert_eq!(body_of(&db, 1), "[#1 only.png, 512 B][attachment]");

        // One placeholder, two attachments: the spare is appended, not dropped.
        let db = fixture();
        db.execute("UPDATE message SET text = char(65532) WHERE rowid = 1", [])
            .unwrap();
        attach(
            &db,
            1,
            &[
                (1, Some("shown.png"), Some("image/png"), 512, false, true),
                (2, Some("spare.png"), Some("image/png"), 512, false, true),
            ],
        );
        assert_eq!(
            body_of(&db, 1),
            "[#1 shown.png, 512 B] [#2 spare.png, 512 B]"
        );
    }

    /// Hidden rows are Messages' bookkeeping — see attachments.md §7.
    #[test]
    fn hidden_attachments_are_left_out() {
        let db = fixture();
        db.execute("UPDATE message SET text = char(65532) WHERE rowid = 1", [])
            .unwrap();
        attach(&db, 1, &[(1, None, None, 0, true, true)]);

        // The row exists, so leaving it out has to leave the body readable
        // rather than leaving a bare U+FFFC behind.
        assert_eq!(body_of(&db, 1), "[attachment]");
        let found = fetch_messages(&db, &FetchMessages::default(), &ContactIndex::empty()).unwrap();
        let message = found.iter().find(|m| m.rowid == 1).unwrap();
        assert!(message.attachments.is_empty(), "{:?}", message.attachments);
    }

    /// A row Messages kept without the file behind it: 1,301 of 76,317.
    #[test]
    fn an_attachment_with_no_file_says_so_rather_than_guessing_a_size() {
        let db = fixture();
        db.execute("UPDATE message SET text = char(65532) WHERE rowid = 1", [])
            .unwrap();
        attach(
            &db,
            1,
            &[(
                1,
                Some("gone.jpg"),
                Some("image/jpeg"),
                900_000,
                false,
                false,
            )],
        );
        assert_eq!(body_of(&db, 1), "[#1 gone.jpg, not downloaded]");
    }

    #[test]
    fn sizes_read_the_way_a_person_thinks_of_them() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(1_048_576), "1.0 MB");
        assert_eq!(human_bytes(1_073_741_824), "1.0 GB");
        // Saturates at the largest unit rather than inventing another.
        assert_eq!(human_bytes(i64::MAX), "8388608.0 TB");
    }

    /// Searching matches what was said, not what stands in for a photo.
    #[test]
    fn search_does_not_match_the_description_of_an_attachment() {
        let db = fixture();
        db.execute("UPDATE message SET text = char(65532) WHERE rowid = 1", [])
            .unwrap();
        attach(
            &db,
            1,
            &[(
                1,
                Some("invoice.pdf"),
                Some("application/pdf"),
                2048,
                false,
                true,
            )],
        );

        let found = fetch_messages(
            &db,
            &FetchMessages {
                query: Some("invoice"),
                ..Default::default()
            },
            &ContactIndex::empty(),
        )
        .unwrap();
        assert!(bodies(&found).is_empty(), "{:?}", bodies(&found));
    }

    /// `IN (?, ?, ...)` costs one host parameter per rowid, and SQLite refuses
    /// more than `SQLITE_LIMIT_VARIABLE_NUMBER` of them.
    ///
    /// Aimed at `attachments_for` directly rather than through a fixture of
    /// 33,000 messages: the failure needs only a long enough rowid list, not any
    /// attachments or even any messages, and this way the test is instant.
    #[test]
    fn an_attachment_lookup_survives_more_rowids_than_sqlite_binds() {
        let db = fixture();
        // Comfortably past the bundled build's 32,766, and past the 999 that
        // older SQLite allows.
        let rowids: Vec<i64> = (1..=40_000).collect();
        let found = attachments_for(&db, &rowids).expect("a long rowid list");
        assert!(found.is_empty(), "{found:?}");

        // And it still finds what is there when the list is that long.
        attach(
            &db,
            1,
            &[(1, Some("late.png"), Some("image/png"), 512, false, true)],
        );
        let found = attachments_for(&db, &rowids).expect("a long rowid list");
        assert_eq!(found.get(&1).map(Vec::len), Some(1), "{found:?}");
    }

    /// A message in two conversations comes back as two rows with one rowid,
    /// because `MESSAGE_FROM` joins `chat_message_join`.
    #[test]
    fn a_message_in_two_chats_shows_its_attachment_in_both() {
        let db = fixture();
        db.execute("UPDATE message SET text = char(65532) WHERE rowid = 1", [])
            .unwrap();
        attach(
            &db,
            1,
            &[(1, Some("photo.png"), Some("image/png"), 2048, false, true)],
        );
        db.execute(
            "INSERT INTO chat_message_join (chat_id, message_id, message_date)
             VALUES (2, 1, ?)",
            rusqlite::params![at(0)],
        )
        .unwrap();

        let found = fetch_messages(&db, &FetchMessages::default(), &ContactIndex::empty()).unwrap();
        let copies: Vec<_> = found.iter().filter(|m| m.rowid == 1).collect();
        assert_eq!(copies.len(), 2, "the fixture must produce two copies");
        for copy in copies {
            assert_eq!(copy.body.as_deref(), Some("[#1 photo.png, 2.0 KB]"));
            assert_eq!(copy.attachments.len(), 1, "chat {}", copy.chat_id);
        }
    }

    /// 225 visible attachments on a real database have no mime type but a real
    /// `uti`; 2 have the `dyn.` kind macOS invents for a file it cannot type.
    #[test]
    fn a_uti_stands_in_for_a_missing_mime_type_unless_it_says_nothing() {
        let db = fixture();
        db.execute(
            "UPDATE message SET text = char(65532) || char(65532) WHERE rowid = 1",
            [],
        )
        .unwrap();
        for (rowid, uti) in [(1, "com.apple.coreaudio-format"), (2, "dyn.age81a8dr")] {
            db.execute(
                "INSERT INTO attachment (ROWID, guid, filename, uti, mime_type,
                     transfer_name, total_bytes, is_sticker, hide_attachment)
                 VALUES (?, 'a', '/some/path', ?, NULL, NULL, 0, 0, 0)",
                rusqlite::params![rowid, uti],
            )
            .unwrap();
            db.execute(
                "INSERT INTO message_attachment_join (message_id, attachment_id) VALUES (1, ?)",
                rusqlite::params![rowid],
            )
            .unwrap();
        }

        assert_eq!(
            body_of(&db, 1),
            "[#1 com.apple.coreaudio-format][#2 attachment]"
        );

        // And the structured field stays honest: a UTI is not a MIME type, and
        // a consumer reading `mimeType` must not be handed one.
        let found = fetch_messages(&db, &FetchMessages::default(), &ContactIndex::empty()).unwrap();
        let message = found.iter().find(|m| m.rowid == 1).unwrap();
        assert_eq!(message.attachments[0].mime_type, None);
        assert_eq!(
            message.attachments[0].uti.as_deref(),
            Some("com.apple.coreaudio-format")
        );
    }

    /// Deduplication and batching are one fix, not two.
    ///
    /// A message in two conversations puts its rowid in the list twice. Within
    /// one batch `IN` is set membership and that is harmless; straddling a batch
    /// boundary it is two queries pushing into the same entry, so the photo is
    /// described twice and listed twice. The pair here sits either side of
    /// `ATTACHMENT_BATCH` on purpose.
    #[test]
    fn a_rowid_asked_for_twice_is_answered_once() {
        let db = fixture();
        attach(
            &db,
            1,
            &[(1, Some("one.png"), Some("image/png"), 512, false, true)],
        );

        let mut rowids: Vec<i64> = (2..=ATTACHMENT_BATCH as i64).collect();
        rowids.insert(0, 1);
        rowids.push(1);
        assert!(
            rowids.len() > ATTACHMENT_BATCH,
            "the pair must straddle a batch boundary"
        );

        let found = attachments_for(&db, &rowids).unwrap();
        assert_eq!(found.get(&1).map(Vec::len), Some(1), "{found:?}");

        // And within a single batch, which was always fine and must stay so.
        let found = attachments_for(&db, &[1, 1, 1]).unwrap();
        assert_eq!(found.get(&1).map(Vec::len), Some(1), "{found:?}");
    }

    /// A reply says what it is answering, and the answer comes from the same
    /// decoded body everything else uses.
    #[test]
    fn a_reply_carries_the_message_it_answers() {
        let db = fixture();
        db.execute(
            "INSERT INTO message (rowid, guid, text, is_from_me, handle_id,
                 associated_message_type, date, service, thread_originator_guid)
             VALUES (20, 'm20', 'yes, that works', 1, 1, 0, ?, 'iMessage', 'm1')",
            rusqlite::params![at(9)],
        )
        .unwrap();
        db.execute(
            "INSERT INTO chat_message_join (chat_id, message_id, message_date)
             VALUES (1, 20, ?)",
            rusqlite::params![at(9)],
        )
        .unwrap();

        let found = fetch_messages(&db, &FetchMessages::default(), &ContactIndex::empty()).unwrap();
        let reply = found.iter().find(|m| m.rowid == 20).expect("the reply");
        let answering = reply.reply_to.as_ref().expect("what it answers");
        assert_eq!(answering.rowid, 1);
        assert_eq!(answering.excerpt.as_deref(), Some("are you around later"));

        // Everything else is left alone.
        let other = found
            .iter()
            .find(|m| m.rowid == 2)
            .expect("an ordinary one");
        assert!(other.reply_to.is_none());
    }

    /// 19 replies on a real database sit in a different conversation from the
    /// message they answer, so the lookup must not be scoped to one chat.
    #[test]
    fn a_reply_finds_an_originator_in_another_conversation() {
        let db = fixture();
        // Message 3 lives in chat 2; this reply lives in chat 1.
        db.execute(
            "INSERT INTO message (rowid, guid, text, is_from_me, handle_id,
                 associated_message_type, date, service, thread_originator_guid)
             VALUES (21, 'm21', 'across the room', 0, 1, 0, ?, 'iMessage', 'm3')",
            rusqlite::params![at(9)],
        )
        .unwrap();
        db.execute(
            "INSERT INTO chat_message_join (chat_id, message_id, message_date)
             VALUES (1, 21, ?)",
            rusqlite::params![at(9)],
        )
        .unwrap();

        let found = fetch_messages(
            &db,
            &FetchMessages {
                chat_id: Some(1),
                ..Default::default()
            },
            &ContactIndex::empty(),
        )
        .unwrap();
        let reply = found.iter().find(|m| m.rowid == 21).expect("the reply");
        let answering = reply.reply_to.as_ref().expect("what it answers");
        assert_eq!(answering.rowid, 3, "{answering:?}");
        assert_eq!(answering.excerpt.as_deref(), Some("deploy is green"));
    }

    /// 3 of 5,649 originators on a real database have been deleted. A reply to
    /// one of them is still a message and must still be returned.
    #[test]
    fn a_reply_whose_originator_is_gone_is_still_a_message() {
        let db = fixture();
        db.execute(
            "INSERT INTO message (rowid, guid, text, is_from_me, handle_id,
                 associated_message_type, date, service, thread_originator_guid)
             VALUES (22, 'm22', 'answering a ghost', 0, 1, 0, ?, 'iMessage', 'no-such-guid')",
            rusqlite::params![at(9)],
        )
        .unwrap();
        db.execute(
            "INSERT INTO chat_message_join (chat_id, message_id, message_date)
             VALUES (1, 22, ?)",
            rusqlite::params![at(9)],
        )
        .unwrap();

        let found = fetch_messages(&db, &FetchMessages::default(), &ContactIndex::empty()).unwrap();
        let reply = found.iter().find(|m| m.rowid == 22).expect("the reply");
        assert!(reply.reply_to.is_none(), "{:?}", reply.reply_to);
        assert_eq!(reply.body.as_deref(), Some("answering a ghost"));
    }

    /// A guid names a message and is no use to a reader, so it never ships. The
    /// assertion outlives the private field it once guarded: what matters is
    /// what a consumer receives, not how the code happens to carry it.
    #[test]
    fn the_originator_guid_stays_off_the_wire() {
        let db = fixture();
        db.execute(
            "INSERT INTO message (rowid, guid, text, is_from_me, handle_id,
                 associated_message_type, date, service, thread_originator_guid)
             VALUES (23, 'm23', 'noted', 0, 1, 0, ?, 'iMessage', 'm1')",
            rusqlite::params![at(9)],
        )
        .unwrap();
        db.execute(
            "INSERT INTO chat_message_join (chat_id, message_id, message_date)
             VALUES (1, 23, ?)",
            rusqlite::params![at(9)],
        )
        .unwrap();

        let found = fetch_messages(&db, &FetchMessages::default(), &ContactIndex::empty()).unwrap();
        let reply = found.iter().find(|m| m.rowid == 23).unwrap();
        let json = serde_json::to_value(reply).unwrap();
        assert!(json.get("threadOriginatorGuid").is_none(), "{json}");
        assert_eq!(json["replyTo"]["rowid"], serde_json::json!(1));
    }

    #[test]
    fn an_excerpt_is_one_line_and_bounded() {
        assert_eq!(excerpt("short"), "short");
        // Newlines and runs of spaces collapse, so a quote stays on one line.
        assert_eq!(excerpt("two\n lines"), "two lines");
        let long = "x".repeat(200);
        let cut = excerpt(&long);
        assert_eq!(cut.chars().count(), EXCERPT + 1, "{cut}");
        assert!(cut.ends_with('…'));
        // Cut on characters, not bytes, so this does not panic or split one.
        let wide = "é".repeat(200);
        assert_eq!(excerpt(&wide).chars().count(), EXCERPT + 1);
    }

    /// Quoting the raw body would put a bare U+FFFC in the excerpt — the hole
    /// this program fixed everywhere else. A quote reads the way the transcript
    /// does.
    #[test]
    fn a_reply_to_a_photo_quotes_the_photo_not_the_placeholder() {
        let db = fixture();
        db.execute("UPDATE message SET text = char(65532) WHERE rowid = 1", [])
            .unwrap();
        attach(
            &db,
            1,
            &[(1, Some("beach.heic"), Some("image/heic"), 2048, false, true)],
        );
        db.execute(
            "INSERT INTO message (rowid, guid, text, is_from_me, handle_id,
                 associated_message_type, date, service, thread_originator_guid)
             VALUES (24, 'm24', 'lovely', 0, 1, 0, ?, 'iMessage', 'm1')",
            rusqlite::params![at(9)],
        )
        .unwrap();
        db.execute(
            "INSERT INTO chat_message_join (chat_id, message_id, message_date)
             VALUES (1, 24, ?)",
            rusqlite::params![at(9)],
        )
        .unwrap();

        let found = fetch_messages(&db, &FetchMessages::default(), &ContactIndex::empty()).unwrap();
        let reply = found.iter().find(|m| m.rowid == 24).expect("the reply");
        let quote = reply.reply_to.as_ref().expect("what it answers");
        assert_eq!(quote.excerpt.as_deref(), Some("[#1 beach.heic, 2.0 KB]"));
        assert!(
            !quote
                .excerpt
                .as_deref()
                .unwrap_or_default()
                .contains('\u{fffc}'),
            "{quote:?}"
        );
    }

    /// The same shape as `a_message_in_two_chats_shows_its_attachment_in_both`,
    /// and for the same reason: one rowid comes back as two rows, so taking the
    /// quote out of the map gives it to whichever copy is reached first.
    #[test]
    fn a_reply_in_two_chats_keeps_its_quote_in_both() {
        let db = fixture();
        db.execute(
            "INSERT INTO message (rowid, guid, text, is_from_me, handle_id,
                 associated_message_type, date, service, thread_originator_guid)
             VALUES (25, 'm25', 'yes, that works', 1, 1, 0, ?, 'iMessage', 'm1')",
            rusqlite::params![at(9)],
        )
        .unwrap();
        for chat in [1, 2] {
            db.execute(
                "INSERT INTO chat_message_join (chat_id, message_id, message_date)
                 VALUES (?, 25, ?)",
                rusqlite::params![chat, at(9)],
            )
            .unwrap();
        }

        let found = fetch_messages(&db, &FetchMessages::default(), &ContactIndex::empty()).unwrap();
        let copies: Vec<_> = found.iter().filter(|m| m.rowid == 25).collect();
        assert_eq!(copies.len(), 2, "the fixture must produce two copies");
        for copy in copies {
            let quote = copy
                .reply_to
                .as_ref()
                .unwrap_or_else(|| panic!("chat {} lost the quote", copy.chat_id));
            assert_eq!(quote.excerpt.as_deref(), Some("are you around later"));
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

    /// The same fold, through SQL, on both of the columns a body can live in.
    #[test]
    fn search_folds_case_beyond_ascii_end_to_end() {
        let db = fixture();
        let mut blob: Vec<u8> = Vec::new();
        blob.extend_from_slice(b"\x04\x0bstreamtyped\x81\xe8\x03\x84\x01\x40\x84\x84\x84");
        blob.extend_from_slice(b"NSString\x01\x94\x84\x01\x2b");
        let text = "ZÜRICH in the archive".as_bytes();
        blob.push(u8::try_from(text.len()).unwrap());
        blob.extend_from_slice(text);

        for (rowid, at_days, text, body) in [
            (8, 7, Some("CAFÉ in the text"), None),
            (9, 8, None, Some(blob)),
        ] {
            db.execute(
                "INSERT INTO message (rowid, guid, text, attributedBody, is_from_me,
                     handle_id, associated_message_type, date, service)
                 VALUES (?, 'm', ?, ?, 0, 1, 0, ?, 'iMessage')",
                rusqlite::params![rowid, text, body, at(at_days)],
            )
            .unwrap();
            db.execute(
                "INSERT INTO chat_message_join (chat_id, message_id, message_date)
                 VALUES (1, ?, ?)",
                rusqlite::params![rowid, at(at_days)],
            )
            .unwrap();
        }

        for (needle, expected) in [
            ("café", "CAFÉ in the text"),
            ("CAFÉ", "CAFÉ in the text"),
            ("zürich", "ZÜRICH in the archive"),
            ("ZÜRICH", "ZÜRICH in the archive"),
        ] {
            let found = fetch_messages(
                &db,
                &FetchMessages {
                    query: Some(needle),
                    ..Default::default()
                },
                &ContactIndex::empty(),
            )
            .unwrap();
            assert_eq!(bodies(&found), [expected], "searching {needle}");
        }
    }

    /// A limit means "up to this many matches", not "this many candidates minus
    /// however many turned out to be metadata hits".
    ///
    /// The blob here contains the needle only in an archived class name, so the
    /// SQL predicate accepts it and the decode rejects it. Before the widening
    /// retry, one of these inside the limit silently cost a real result.
    #[test]
    fn a_false_positive_does_not_eat_one_of_the_results_asked_for() {
        let db = fixture();
        let decoy = b"\x04\x0bstreamtyped\x81\xe8\x03NSMutableStringPINBALL\x00\x84\x84";
        let mut rowid = 100;
        // Newer than everything real, so they sort first and are seen first.
        for offset in 0..3 {
            db.execute(
                "INSERT INTO message (rowid, guid, text, attributedBody, is_from_me,
                     handle_id, associated_message_type, date, service)
                 VALUES (?, ?, NULL, ?, 0, 1, 0, ?, 'iMessage')",
                rusqlite::params![rowid, format!("d{offset}"), decoy.to_vec(), at(50 + offset)],
            )
            .unwrap();
            db.execute(
                "INSERT INTO chat_message_join (chat_id, message_id, message_date)
                 VALUES (1, ?, ?)",
                rusqlite::params![rowid, at(50 + offset)],
            )
            .unwrap();
            rowid += 1;
        }
        // Two real ones, older than the decoys.
        for offset in 0..2 {
            db.execute(
                "INSERT INTO message (rowid, guid, text, is_from_me, handle_id,
                     associated_message_type, date, service)
                 VALUES (?, ?, 'pinball night', 0, 1, 0, ?, 'iMessage')",
                rusqlite::params![rowid, format!("r{offset}"), at(40 + offset)],
            )
            .unwrap();
            db.execute(
                "INSERT INTO chat_message_join (chat_id, message_id, message_date)
                 VALUES (1, ?, ?)",
                rusqlite::params![rowid, at(40 + offset)],
            )
            .unwrap();
            rowid += 1;
        }

        let found = fetch_messages(
            &db,
            &FetchMessages {
                query: Some("pinball"),
                limit: 2,
                ..Default::default()
            },
            &ContactIndex::empty(),
        )
        .unwrap();
        // Both real matches, despite three decoys sorting ahead of them.
        assert_eq!(found.len(), 2, "{:?}", bodies(&found));
        assert!(
            found
                .iter()
                .all(|m| m.body.as_deref() == Some("pinball night"))
        );
    }

    /// The widening retry has to stop when there is genuinely nothing more.
    #[test]
    fn asking_for_more_than_exists_stops_rather_than_looping() {
        let db = fixture();
        let found = fetch_messages(
            &db,
            &FetchMessages {
                query: Some("deploy"),
                limit: 10_000,
                ..Default::default()
            },
            &ContactIndex::empty(),
        )
        .unwrap();
        assert_eq!(bodies(&found), ["deploy is green"]);
    }

    #[test]
    fn the_body_predicate_looks_past_a_nul() {
        // Directly, so a failure points at the predicate rather than at SQL.
        assert!(contains_ignoring_case(b"abc\x00def", "def"));
        assert!(contains_ignoring_case(b"abc\x00DEF", "def"));
        assert!(contains_ignoring_case(b"\x00\x00hello", "HELLO"));
        assert!(!contains_ignoring_case(b"abc\x00def", "xyz"));
        // An empty needle matches, matching what a `%%` LIKE did.
        assert!(contains_ignoring_case(b"anything", ""));
        // Not a match that runs off the end.
        assert!(!contains_ignoring_case(b"ab", "abc"));
    }

    /// Case is a property of characters, not of bytes.
    ///
    /// A byte-wise fold leaves anything outside ASCII case-sensitive, so `café`
    /// would not find `CAFÉ`. That is worse than it sounds: this predicate is
    /// the SQL prefilter, so the row is gone before the decoded filter — which
    /// folds properly — ever sees it.
    #[test]
    fn the_body_predicate_folds_case_beyond_ascii() {
        for needle in ["café", "CAFÉ", "Café", "É"] {
            assert!(
                contains_ignoring_case("meet at the CAFÉ".as_bytes(), needle),
                "searching {needle}"
            );
            assert!(
                contains_ignoring_case("meet at the café".as_bytes(), needle),
                "searching {needle}"
            );
        }
        // Still a substring match, not a fuzzy one: no accent-stripping.
        assert!(!contains_ignoring_case(
            "meet at the cafe".as_bytes(),
            "café"
        ));

        // Around the binary framing of a typedstream, where the invalid bytes
        // split the blob into runs and a match must be found inside one.
        let blob = b"\x84\x84NSString\x01\x94\x84\x01\x2b\x0bZ\xc3\x9cRICH \xff\x84 caf\xc3\x89";
        assert!(contains_ignoring_case(blob, "zürich"));
        assert!(contains_ignoring_case(blob, "café"));
        // Not across the run boundary that the framing byte introduces.
        assert!(!contains_ignoring_case(blob, "zürich  caf"));
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
