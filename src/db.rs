//! Read-only access to the Messages database.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, Row, params_from_iter};
use serde::{Deserialize, Serialize};

use crate::apple::{from_apple_date, message_body};
use crate::contacts::{Contact, ContactIndex, name_handles};
use crate::matching::run_contains;
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
    /// The reactions on this message, oldest first, removals already cancelled.
    /// Empty for most messages, so it stays out of the JSON like `attachments`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tapbacks: Vec<Tapback>,
    /// Whether this is a search hit, as against context shown around one.
    ///
    /// Defaulted true and omitted when true, so a search asked for without
    /// context serializes byte-identically to what it did before context
    /// existed, and only the surrounding messages carry `"matched": false`.
    #[serde(default = "matched_unless_said", skip_serializing_if = "is_matched")]
    pub matched: bool,
    /// Which run of adjacent messages this belongs to, when context was asked
    /// for.
    ///
    /// A consumer cannot work the runs out for itself: rowids are global, so
    /// two messages adjacent in one conversation are not adjacent numbers, and
    /// the separator between runs would be unreproducible without this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<i64>,
}

/// A message is a hit unless something says otherwise, which is what keeps the
/// field out of the JSON for every search that asked for no context.
fn matched_unless_said() -> bool {
    true
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_matched(matched: &bool) -> bool {
    *matched
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

/// One reaction on a message.
///
/// The raw type is published beside the symbol deliberately: a consumer that
/// wants Messages' own glyphs, or wants to count Love separately from a heart
/// emoji, should not have to re-derive it from a string this program chose
/// (tapbacks.md §7). `is_from_me` is there because it is the only way the
/// sender survives for a reaction of mine — my own rows carry no handle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tapback {
    pub associated_message_type: i64,
    pub symbol: String,
    #[serde(with = "crate::iso")]
    pub date: Option<DateTime<Utc>>,
    pub is_from_me: bool,
    pub handle: Option<String>,
    /// Written as null when there is no saved contact, matching the message
    /// this rides on — one shape for the same key, not two. `default` stays
    /// for reading: a daemon built before the skip came off omits the key,
    /// and both builds answer protocol 16.
    #[serde(default)]
    pub contact_name: Option<String>,
}

/// The symbol table from tapbacks.md §4: the classic six as their Messages
/// emoji, a type 2006 as the emoji the sender chose, read from the column §9
/// measured as always populated. `None` is a type the table does not know —
/// 2007 exists in real data and nothing yet identifies it — which renders
/// nowhere rather than guessing.
fn tapback_symbol(kind: i64, emoji: Option<&str>) -> Option<String> {
    match kind {
        2000 => Some("❤️".to_string()),
        2001 => Some("👍".to_string()),
        2002 => Some("👎".to_string()),
        2003 => Some("😂".to_string()),
        2004 => Some("‼️".to_string()),
        2005 => Some("❓".to_string()),
        2006 => emoji.filter(|emoji| !emoji.is_empty()).map(str::to_string),
        _ => None,
    }
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
/// It over-matches when the needle also appears in an archived class name, or
/// when the occurrence does not start a word: the decoded filter runs
/// `matching::begins_a_word`, and that rule cannot run here, because the
/// framing puts an arbitrary byte before the text and a boundary test on raw
/// bytes rejects real matches (search-boundaries.md §3). The decode-and-check
/// afterwards is what narrows both.
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

/// The reactions on these messages, keyed by message rowid, removals cancelled.
///
/// A second query beside `attachments_for` and `replies_for`, keyed on the
/// guids of the messages being returned (tapbacks.md §5). The stored target
/// takes three forms — part-prefixed `p:N/<guid>` 96% of the time, `bp:<guid>`
/// for most of the rest, and bare — all measured in §9, where the second form
/// first hid inside an implausible orphan count. A join written
/// `= message.guid` would have matched 1.3% of tapbacks while looking correct,
/// so the join strips both prefixes and accepts the bare form. The 56 rows
/// whose target genuinely no longer exists simply match nothing.
fn tapbacks_for(
    db: &Connection,
    wanted: &[(i64, String)],
    contacts: &ContactIndex,
) -> Result<BTreeMap<i64, Vec<Tapback>>> {
    let mut found: BTreeMap<i64, Vec<Tapback>> = BTreeMap::new();
    if wanted.is_empty() {
        return Ok(found);
    }

    let mut guids: Vec<&str> = wanted.iter().map(|(_, guid)| guid.as_str()).collect();
    guids.sort_unstable();
    guids.dedup();

    // The emoji column arrived with a macOS version and §9 measured only this
    // machine, so its absence is a schema to survive rather than an error —
    // without it a type 2006 has no symbol and renders nowhere.
    let has_emoji: i64 = db.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('message')
          WHERE name = 'associated_message_emoji'",
        [],
        |row| row.get(0),
    )?;
    let emoji = if has_emoji > 0 {
        "m.associated_message_emoji"
    } else {
        "NULL"
    };

    /// kind, emoji, date, from me, handle — one reaction row, pre-cancellation.
    type Reaction = (i64, Option<String>, Option<i64>, bool, Option<String>);
    let target = "CASE WHEN m.associated_message_guid LIKE 'p:%' \
                  THEN substr(m.associated_message_guid, instr(m.associated_message_guid, '/') + 1) \
                  WHEN m.associated_message_guid LIKE 'bp:%' \
                  THEN substr(m.associated_message_guid, 4) \
                  ELSE m.associated_message_guid END";
    let mut raw: BTreeMap<String, Vec<Reaction>> = BTreeMap::new();
    for batch in guids.chunks(ATTACHMENT_BATCH) {
        let slots = vec!["?"; batch.len()].join(",");
        let sql = format!(
            "SELECT {target} AS target, m.associated_message_type AS kind,
                    {emoji} AS emoji, m.date AS date,
                    m.is_from_me AS isFromMe, handle.id AS handle
               FROM message m LEFT JOIN handle ON m.handle_id = handle.rowid
              WHERE m.associated_message_guid IS NOT NULL
                AND m.associated_message_type BETWEEN 2000 AND 3999
                AND {target} IN ({slots})
              ORDER BY m.date"
        );
        let mut statement = db.prepare(&sql)?;
        let mut rows = statement.query(params_from_iter(batch.iter().copied()))?;
        while let Some(row) = rows.next()? {
            let Some(target) = text(row, "target") else {
                continue;
            };
            raw.entry(target).or_default().push((
                number(row, "kind"),
                text(row, "emoji"),
                row.get::<_, Option<i64>>("date").ok().flatten(),
                number(row, "isFromMe") == 1,
                text(row, "handle"),
            ));
        }
    }

    let mut per_guid: BTreeMap<&str, Vec<Tapback>> = BTreeMap::new();
    for (guid, reactions) in &raw {
        // A removal cancels the latest surviving add from the same sender in
        // its type family, and drops itself either way — an unmatched removal
        // retracts a reaction this page never saw (tapbacks.md §6).
        let mut kept: Vec<&Reaction> = Vec::new();
        for reaction in reactions {
            let (kind, _, _, from_me, handle) = reaction;
            if *kind >= 3000 {
                let family = kind - 1000;
                if let Some(at) =
                    kept.iter()
                        .rposition(|(kind, _, _, kept_from_me, kept_handle)| {
                            *kind == family && kept_from_me == from_me && kept_handle == handle
                        })
                {
                    kept.remove(at);
                }
                continue;
            }
            kept.push(reaction);
        }
        let rendered: Vec<Tapback> = kept
            .into_iter()
            .filter_map(|(kind, emoji, date, is_from_me, handle)| {
                let symbol = tapback_symbol(*kind, emoji.as_deref())?;
                Some(Tapback {
                    associated_message_type: *kind,
                    symbol,
                    date: from_apple_date(*date),
                    is_from_me: *is_from_me,
                    handle: handle.clone(),
                    contact_name: contacts.lookup(handle.as_deref()).map(str::to_string),
                })
            })
            .collect();
        if !rendered.is_empty() {
            per_guid.insert(guid.as_str(), rendered);
        }
    }

    for (rowid, guid) in wanted {
        // Cloned rather than taken, for the reason `attachments_for` documents:
        // one message in two conversations is two rows with one rowid.
        if let Some(list) = per_guid.get(guid.as_str()) {
            found.insert(*rowid, list.clone());
        }
    }
    Ok(found)
}

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
        tapbacks: Vec::new(),
        matched: true,
        group: None,
    }
}

/// How much of the conversation to show around each hit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Context {
    pub before: i64,
    pub after: i64,
}

impl Context {
    pub fn wanted(self) -> bool {
        self.before > 0 || self.after > 0
    }
}

/// One hit and the messages around it, contiguous within its conversation.
struct Window {
    messages: Vec<Message>,
    /// The rowid of one message past the end, fetched but never shown.
    ///
    /// It is what makes "overlap **or touch**" answerable. Two runs touch when
    /// nothing sits between them, and nothing in a rowid can say that, because
    /// rowids are global and two adjacent messages in one conversation are not
    /// adjacent numbers. Asking for one message more than will be displayed
    /// turns that question into a comparison, at no extra query.
    reach: i64,
}

/// Put each hit back into the conversation it came from.
///
/// Every hit gets its own two queries — backwards and forwards, both bounded to
/// that hit's chat — because search results interleave conversations and "the
/// three messages after this one" means three within *that* conversation, not
/// the next three rows of the result stream.
///
/// The windows are then merged where they meet, so a stretch of conversation
/// containing several hits prints once rather than once per hit. Printing each
/// window whole would repeat those messages and, worse, disguise the fact that
/// the hits were all the same exchange.
///
/// Nothing here narrows the window the way the search was narrowed. `--from`,
/// `--since` and the body match all bound what counts as a hit; a window is a
/// slice of the conversation around one, so it holds whatever was actually
/// said. Tapback rows are the one exception, since brackets: the reaction
/// rides its target's bracket instead, and a window that also held the row
/// showed the same reaction twice (tapbacks.md §6). What that trades away is
/// the reaction whose target sits outside the window — it renders nowhere,
/// where the row once stood in for the reply (search-context.md §3).
pub fn with_context(
    db: &Connection,
    hits: Vec<Message>,
    context: Context,
    contacts: &ContactIndex,
) -> Result<Vec<Message>> {
    // A width is a count of messages, and a negative count is not a smaller
    // window but a larger one: it reaches SQLite as `LIMIT -1`, which means no
    // limit, so the whole conversation before each hit would be fetched and
    // decoded. The CLI cannot produce one, but the daemon takes these off a
    // socket, and clamping here covers every caller rather than the one path
    // that happens to be untrusted today.
    let context = Context {
        before: context.before.max(0),
        after: context.after.max(0),
    };
    if !context.wanted() || hits.is_empty() {
        return Ok(hits);
    }

    // Which messages matched, decided once and up front.
    //
    // Stamping it per window instead is what made a later hit lose its marker:
    // it arrives twice, as context around an earlier hit and as itself, and
    // whichever copy the merge happened to keep decided the answer. A message
    // is a hit because the search returned it, not because of which window it
    // reached the run through.
    let matched: BTreeSet<i64> = hits.iter().map(|hit| hit.rowid).collect();

    let mut windows: BTreeMap<i64, Vec<Window>> = BTreeMap::new();
    for hit in hits {
        let chat_id = hit.chat_id;
        let rowid = hit.rowid;
        let around = |options: FetchMessages<'_>| -> Result<Vec<Message>> {
            fetch_messages(
                db,
                &FetchMessages {
                    chat_id: Some(chat_id),
                    limit: options.limit,
                    after_rowid: options.after_rowid,
                    before_rowid: options.before_rowid,
                    oldest_first: options.oldest_first,
                    // Reactions used to cross into the window as rows, on "a
                    // conversation is what was said in it" — until brackets
                    // put them on the message they react to, and a window
                    // showed the same reaction twice (tapbacks.md §6).
                    include_tapbacks: false,
                    include_filtered: true,
                    ..Default::default()
                },
                contacts,
            )
        };

        // Taken newest-first so the `LIMIT` keeps the nearest ones rather than
        // the oldest in the conversation; `fetch_messages` turns them back into
        // reading order on the way out, as it does for every other caller.
        let earlier = around(FetchMessages {
            before_rowid: Some(rowid),
            limit: context.before,
            ..Default::default()
        })?;

        // One more than will be shown, for `reach`.
        let mut later = around(FetchMessages {
            after_rowid: Some(rowid),
            limit: context.after.saturating_add(1),
            oldest_first: true,
            ..Default::default()
        })?;
        let probe = if i64::try_from(later.len()).unwrap_or(i64::MAX) > context.after {
            later.pop().map(|message| message.rowid)
        } else {
            None
        };

        let mut messages = earlier;
        messages.push(hit);
        messages.extend(later);
        let reach = probe.unwrap_or_else(|| messages.last().map_or(rowid, |last| last.rowid));
        windows
            .entry(chat_id)
            .or_default()
            .push(Window { messages, reach });
    }

    // Merge within each conversation, then order the runs the way the messages
    // themselves are ordered, so the output reads oldest-first as it always has.
    let mut runs: Vec<Vec<Message>> = Vec::new();
    for (_, mut chat_windows) in windows {
        chat_windows.sort_by_key(|window| window.messages.first().map_or(0, |first| first.rowid));
        let mut open: Option<(Vec<Message>, i64)> = None;
        for window in chat_windows {
            let starts = window.messages.first().map_or(0, |first| first.rowid);
            match &mut open {
                // Touching counts, not only overlapping: `reach` is one past the
                // run's last shown message, so a window starting at or before it
                // has nothing between the two.
                Some((run, reach)) if starts <= *reach => {
                    let known: BTreeSet<i64> = run.iter().map(|message| message.rowid).collect();
                    run.extend(
                        window
                            .messages
                            .into_iter()
                            .filter(|message| !known.contains(&message.rowid)),
                    );
                    run.sort_by_key(|message| message.rowid);
                    *reach = (*reach).max(window.reach);
                }
                _ => {
                    if let Some((run, _)) = open.take() {
                        runs.push(run);
                    }
                    open = Some((window.messages, window.reach));
                }
            }
        }
        if let Some((run, _)) = open.take() {
            runs.push(run);
        }
    }

    runs.sort_by_key(|run| run.first().map_or(0, |first| first.rowid));
    let mut out = Vec::new();
    for (group, run) in runs.into_iter().enumerate() {
        let group = i64::try_from(group).unwrap_or(i64::MAX);
        for mut message in run {
            message.matched = matched.contains(&message.rowid);
            message.group = Some(group);
            out.push(message);
        }
    }
    Ok(out)
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
    /// The name their Contacts record is filed under, when a nickname is being
    /// shown instead of it. Never displayed; it is here so that naming it
    /// exactly settles a tie the way the shown name does.
    pub filed_as: Option<String>,
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
    /// Stop below this rowid, for the half of a context window that reaches
    /// backwards. Setting it also orders by rowid rather than by date, since a
    /// window is "what came just before this in the conversation" and that is
    /// arrival order — the same argument the watcher already makes.
    pub before_rowid: Option<i64>,
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
            before_rowid: None,
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
    // Bounded against `chat_message_join.message_id` rather than
    // `message.rowid` whenever a chat is named, though the two hold the same
    // number. `chat_message_join` is keyed by (chat_id, message_id), so this
    // shape is an index range scan; the other one makes SQLite walk message
    // rowids and discard everything belonging to other conversations, which for
    // a quiet chat means thousands of rows to find three. Measured at ~100ms
    // per window against a real database before this, and unmeasurable after.
    let ordinal = if options.chat_id.is_some() {
        "chat_message_join.message_id"
    } else {
        "message.rowid"
    };
    if let Some(after_rowid) = options.after_rowid {
        clauses.push(format!("{ordinal} > ?"));
        params.push(after_rowid.into());
    }
    if let Some(before_rowid) = options.before_rowid {
        clauses.push(format!("{ordinal} < ?"));
        params.push(before_rowid.into());
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
        format!("ORDER BY {ordinal} ASC")
    } else if options.before_rowid.is_some() {
        // The messages immediately before a hit are the largest rowids below
        // it, and taking them by date would pick the wrong ones whenever the
        // two orders disagree.
        format!("ORDER BY {ordinal} DESC")
    } else {
        "ORDER BY message.date DESC".to_string()
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
    // Folded once here because `begins_a_word` expects both sides lowercased,
    // and one query is matched against every candidate in every round.
    let needle = options.query.map(str::to_lowercase);
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
        if let Some(needle) = &needle {
            // Deliberately *narrower* than the SQL prefilter, which stays a
            // plain substring test: a hit has to start where a word starts,
            // and that rule can only run here. The blob's framing puts an
            // arbitrary byte — often a letter — immediately before the text,
            // so a boundary test on the raw bytes rejects real matches, and
            // the preceding "character" is not even guaranteed to be one
            // (search-boundaries.md §3). The body goes in unfolded — the
            // predicate reads the boundary from the original characters.
            messages.retain(|message| {
                message
                    .body
                    .as_ref()
                    .is_some_and(|body| crate::matching::begins_a_word(body, needle))
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

    // Reactions land on the messages they react to, so only non-tapback rows
    // are targets — a reaction to a reaction renders nowhere (tapbacks.md §6).
    let targets: Vec<(i64, String)> = messages
        .iter()
        .filter(|message| !message.is_tapback)
        .map(|message| (message.rowid, message.guid.clone()))
        .collect();
    let tapbacks = tapbacks_for(db, &targets, contacts)?;

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
        message.tapbacks = tapbacks.get(&message.rowid).cloned().unwrap_or_default();
    }

    if !options.oldest_first {
        messages.reverse();
    }
    Ok(messages)
}

/// Read a conversation that may span more than one thread, oldest-first.
///
/// One `fetch_messages` per thread rather than one query with `chat_id IN (…)`,
/// which is the shape conversation-merging.md §4 was corrected to. The `IN`
/// keeps the index and loses the ordering: two ranges of
/// `chat_message_join` are each ordered and not jointly, so SQLite sorts the
/// whole conversation before the limit applies. Measured on a synthetic
/// database with the real schema, `LIMIT 50` over a 300,000-message
/// conversation cost 70-80ms that way and under a millisecond this way, and the
/// gap grows with the conversation rather than with the limit.
///
/// Merged on `rowid`, which is arrival order and is the same number in every
/// thread — `chat_message_join.message_id` joins to `message.rowid` on
/// equality, so nothing has to be invented to interleave them.
pub fn fetch_conversation(
    db: &Connection,
    chats: &[Chat],
    after_date: Option<i64>,
    limit: i64,
    include_tapbacks: bool,
    contacts: &ContactIndex,
) -> Result<Vec<Message>> {
    // Clamped here rather than at the callers, for the reason `with_context`
    // gives for the same guard: the shared function covers every caller instead
    // of the one path that happens to be untrusted today. A negative reaches
    // `LIMIT ?` as no limit at all, and the merge would then decode every
    // message of every thread the person has rather than of one.
    let limit = limit.max(0);
    let mut merged: Vec<Message> = Vec::new();
    for chat in chats {
        merged.extend(fetch_messages(
            db,
            &FetchMessages {
                chat_id: Some(chat.rowid),
                after_date,
                limit,
                include_tapbacks,
                ..Default::default()
            },
            contacts,
        )?);
    }
    if chats.len() < 2 {
        return Ok(merged);
    }

    // By date, and by rowid only to break a tie.
    //
    // Date because that is what `read` has always meant: `oldest_first` and
    // `before_rowid` are what select rowid ordering in `fetch_messages`, and
    // reading sets neither, so a single-thread transcript is in clock order.
    // Sorting a merge by arrival would print a late-arriving message last where
    // an unmerged read prints it first, so the same messages would read in one
    // order for somebody with one conversation and another for somebody with
    // two.
    //
    // And because the fetch and the trim have to agree. Each thread is selected
    // with `ORDER BY message.date DESC`, so trimming the union by anything else
    // drops a message a thread returned and keeps one it never offered.
    merged.sort_by_key(|message| (message.date, message.rowid));
    // A message joined to two of these threads arrives from both fetches, the
    // same one rowid and two rows that `attachments_for` and the reply lookup
    // above already have to allow for. Adjacent after the sort, since two rows
    // for one message agree about its date.
    merged.dedup_by_key(|message| message.rowid);
    // Each thread returned at most `limit` of its own newest, so the newest
    // `limit` of the union is the answer: anything dropped here is older than
    // something kept, and its thread had more to give.
    let wanted = usize::try_from(limit).unwrap_or(usize::MAX);
    if merged.len() > wanted {
        merged.drain(..merged.len() - wanted);
    }
    Ok(merged)
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
/// One row of [`CHATS_SQL`] as a [`Chat`].
fn chat_from_row(row: &rusqlite::Row<'_>, contacts: &ContactIndex) -> Chat {
    let display_name = text(row, "displayName");
    let handles = text(row, "handles");
    let identifier = text(row, "chatIdentifier").unwrap_or_default();
    let member_count = number(row, "memberCount");
    let named_handles = name_handles(contacts, handles.as_deref());
    Chat {
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
        // Messages sorts filtered conversations into categories, so any nonzero
        // value means the conversation is filtered.
        is_filtered: number(row, "isFiltered") != 0,
        member_count,
        is_group: member_count > 1,
        last_date: from_apple_date(row.get::<_, Option<i64>>("lastDate").ok().flatten()),
        message_count: number(row, "messageCount"),
    }
}

/// The chats with these rowids, most recently active first.
///
/// Named rather than scanned. Both callers used to take the 10,000 most recent
/// chats and filter, which is a listing query to answer a question about
/// specific rows — and worse than wasteful: a conversation outside that window
/// simply was not found, so naming it by its own rowid, or reaching a room by
/// its members, failed with the error for "no such conversation" while the row
/// sat there. The window is generous enough that it does not bite on any
/// database seen so far, which is exactly why it would have gone unnoticed.
fn chats_by_id(db: &Connection, ids: &[i64], contacts: &ContactIndex) -> Result<Vec<Chat>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let slots = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("{CHATS_SQL} WHERE rowid IN ({slots}) ORDER BY lastDate DESC");
    let mut statement = db.prepare(&sql)?;
    let mut rows = statement.query(params_from_iter(ids.iter().map(|id| Value::from(*id))))?;
    let mut chats = Vec::new();
    while let Some(row) = rows.next()? {
        chats.push(chat_from_row(row, contacts));
    }
    Ok(chats)
}

/// Threads matching a query, newest first.
///
/// **A query is matched in Rust, never in SQL.** There used to be two
/// implementations here, chosen on whether a contact index had loaded: with one,
/// the rows came back unfiltered and were matched in Rust; without one, SQL
/// carried `displayName LIKE ?` and the Rust rule never ran. That is the second
/// definition `naming-a-conversation.md §8` argues against, and the two
/// disagreed — `LIKE` finds the middle of a word where the Rust rule wants a
/// word start, so `oom` reached a room called Ship Room down one branch and not
/// the other. An empty index is not the exotic case it looks: `--no-names`
/// makes one, and so does any machine where Contacts cannot be read, which
/// `server.rs` expects of the daemon's first load.
pub fn fetch_chats(
    db: &Connection,
    query: Option<&str>,
    limit: i64,
    contacts: &ContactIndex,
    include_filtered: bool,
) -> Result<Vec<Chat>> {
    let where_clause = if include_filtered {
        ""
    } else {
        "WHERE isFiltered = 0"
    };
    // Matching in Rust means SQL cannot narrow, so a query reads a window of the
    // newest rows instead. The listing does not come through here — it has no
    // budget at all, see `scan_all_chats` — and the resolver wants the most
    // recently active, which is the front of the window.
    let scan = if query.is_some() {
        NAME_SEARCH_SCAN
    } else {
        limit
    };
    let sql = format!("{CHATS_SQL} {where_clause} ORDER BY lastDate DESC LIMIT ?");
    let mut statement = db.prepare(&sql)?;
    let mut rows = statement.query([scan])?;

    let mut chats = Vec::new();
    while let Some(row) = rows.next()? {
        chats.push(chat_from_row(row, contacts));
    }

    let Some(query) = query else {
        return Ok(chats);
    };
    retain_matching(&mut chats, query, contacts);
    chats.truncate(want(limit));
    Ok(chats)
}

/// Every chat, newest first, with no upper bound on how many.
///
/// The listing cannot use a `LIMIT`, and no budget would do instead. It merges,
/// so a number of threads does not become a knowable number of conversations
/// until after the merge has run — and a budget generous enough to look safe
/// would still be a cap nobody was told about, which is the failure that hides
/// rather than the one that complains. A scan of 5,000 measured as a listing of
/// 4,998 conversations for a caller who asked for 100,000.
///
/// The cost is the chat table, which is the thing being listed: 1,165 rows on a
/// decade-old database, measured at the same 0.11s as asking for five, because
/// the aggregate `CHATS_SQL` joins against is uncorrelated and was always
/// computed in full whatever the `LIMIT` said.
fn scan_all_chats(
    db: &Connection,
    contacts: &ContactIndex,
    include_filtered: bool,
) -> Result<Vec<Chat>> {
    let where_clause = if include_filtered {
        ""
    } else {
        "WHERE isFiltered = 0"
    };
    let sql = format!("{CHATS_SQL} {where_clause} ORDER BY lastDate DESC");
    let mut statement = db.prepare(&sql)?;
    let mut rows = statement.query([])?;
    let mut chats = Vec::new();
    while let Some(row) = rows.next()? {
        chats.push(chat_from_row(row, contacts));
    }
    Ok(chats)
}

/// Keep the chats a query names, matched against contact names as well as rows.
///
/// Separate from the SQL because contact names live in the Contacts database,
/// and separate from [`fetch_chats`] because the listing has to apply it after
/// merging rather than before (see [`fetch_conversations`]).
fn retain_matching(chats: &mut Vec<Chat>, query: &str, contacts: &ContactIndex) {
    let needle = query.to_lowercase();
    // A name matches from the start of a word, an address matches anywhere.
    //
    // The split is the same one `resolve_person` draws, and it has to be: `ana`
    // must not reach Dana Reyes, while a partial number has to keep reaching the
    // address it is part of, since the middle of a phone number is exactly how
    // anyone types a fragment of one (naming-a-conversation.md §2, §8).
    let named = |value: Option<&String>| {
        value.is_some_and(|value| crate::matching::begins_a_word(value, &needle))
    };
    let addressed =
        |value: Option<&String>| value.is_some_and(|value| value.to_lowercase().contains(&needle));
    chats.retain(|chat| {
        named(Some(&chat.name))
            || named(chat.display_name.as_ref())
            || addressed(chat.handles.as_ref())
            || addressed(Some(&chat.identifier))
            // A displaced filed name is shown nowhere, so it is searched
            // separately — but only where the name shown in its place is
            // searched too. A conversation with a display name of its own is
            // found by that name and not by its members', and reaching inside
            // one here would make a formal name easier to find someone by than
            // the name they actually go under.
            || (chat.display_name.is_none()
                && contacts.any_answers_to(chat.handles.as_deref(), &needle))
    });
}

/// The conversation listing: [`fetch_chats`], with each person's threads
/// collapsed into the one conversation they are.
///
/// The pairing is the same one `fetch_messages` and [`fetch_conversation`]
/// already have, and for the same reason: a thread is what the database stores
/// and a conversation is what a person has. `msg chats` wants the second, and
/// the resolver — which reaches [`fetch_chats`] directly — wants the first,
/// because a merged row cannot be told apart from a thread once it exists.
///
/// Neither the query nor the limit can be pushed down. The limit counts
/// conversations, and applying it to threads answers with fewer than were asked
/// for; the query has to run after the merge, so it can reach a person by an
/// address that is not the one their newest thread uses. Which is why the fetch
/// behind this is [`scan_all_chats`] rather than [`fetch_chats`] with a budget:
/// a budget would be a cap on how many conversations exist, imposed before
/// anything knew how many that was.
pub fn fetch_conversations(
    db: &Connection,
    query: Option<&str>,
    limit: i64,
    contacts: &ContactIndex,
    include_filtered: bool,
) -> Result<Vec<Chat>> {
    let threads = scan_all_chats(db, contacts, include_filtered)?;
    let mut chats = merge_listing(threads, contacts);
    if let Some(query) = query {
        retain_matching(&mut chats, query, contacts);
    }
    chats.truncate(want(limit));
    Ok(chats)
}

/// How many rows a caller asking for `limit` gets.
///
/// Clamped, because a negative limit does not convert to a small `usize` — it
/// fails to convert at all, and the fallback behind it would hand back every row
/// there is. `fetch_conversation` guards the same trap.
fn want(limit: i64) -> usize {
    usize::try_from(limit.max(0)).unwrap_or(usize::MAX)
}

/// Collapse each person's threads into the one conversation they are.
///
/// `msg chats` printing a row per thread contradicts `msg chat`, which merges,
/// and the disagreement lands where it is worst: the listing is where rowids
/// come from, so it is where someone goes to find the thread they mean to
/// address (`conversation-merging.md §5`).
///
/// The leading thread is the conversation. It is the most recently active, so it
/// is already both the send target §7 promises not to move and the date the
/// conversation was last touched; only what is genuinely a sum of the parts is
/// recomputed. Emitting it in place keeps the order the SQL established.
///
/// **The filtered rule needs nothing here**, which is worth saying because the
/// merge in `chat` needs `one_bucket` for it. There, threads are gathered for a
/// named person whatever their bucket, so a filtered one can turn up beside an
/// unfiltered one. Here the same flag that would permit the merge is the one
/// that put the rows in the query at all: without `--unknown` no filtered chat
/// is in `chats` to merge, and with it every one of them is fair game. There is
/// no mixed bucket for this function to keep apart.
///
/// Groups pass through untouched. `sole_person` answers `None` for one, and two
/// rooms with the same membership are two rooms.
fn merge_listing(chats: Vec<Chat>, contacts: &ContactIndex) -> Vec<Chat> {
    let mut leader: BTreeMap<String, usize> = BTreeMap::new();
    let mut merged: Vec<Chat> = Vec::with_capacity(chats.len());
    for chat in chats {
        let Some(person) = sole_person(&chat, contacts) else {
            merged.push(chat);
            continue;
        };
        match leader.get(&person) {
            Some(&at) => fold_into(&mut merged[at], chat),
            None => {
                leader.insert(person, merged.len());
                merged.push(chat);
            }
        }
    }
    merged
}

/// Add a later thread to the conversation its person's leading thread opened.
fn fold_into(conversation: &mut Chat, thread: Chat) {
    conversation.message_count += thread.message_count;
    // Unioned so a search still reaches someone by an address that is not the
    // one their newest thread uses. `named_handles` is deliberately not extended
    // to match: every address here belongs to the same person, so naming them
    // all would print that person's name once per address they own.
    if let Some(handles) = thread.handles {
        conversation.handles = Some(match conversation.handles.take() {
            Some(mine) => format!("{mine},{handles}"),
            None => handles,
        });
    }
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

/// How to name a conversation when confirming a message sent to it.
///
/// The address goes in the line, for a one-to-one. `Chat::name` comes from
/// Contacts, so somebody reachable two ways has two conversations that render
/// as the same string, and the address is precisely what differs — which makes
/// it precisely what a confirmation has to say. The two are not interchangeable
/// in delivery: a number and an email are different routes, one may fall back
/// to SMS, and resolution picks the most recently active rather than the one
/// they actually read.
///
/// `--dry-run` is the reason this is not cosmetic. The repository requires one
/// before any real send, and a check that prints the same line whichever
/// address was chosen cannot catch the mistake it exists to catch.
///
/// A conversation with a name of its own is named by it. Its identifier is an
/// opaque `chat42`, noise rather than a second fact, and a room's name is
/// already unambiguous in the way a person's is not.
///
/// Keyed on having a display name rather than on `is_group`, because a room
/// can have one participant — everybody else left, or never joined — and it is
/// still a room rather than a conversation with a person.
pub fn describe_target(chat: &Chat) -> String {
    if chat.display_name.is_some()
        || chat.is_group
        || chat.identifier.is_empty()
        || chat.identifier == chat.name
    {
        return chat.name.clone();
    }
    format!("{} ({})", chat.name, chat.identifier)
}

/// Who an address belongs to, as a key two addresses can share.
///
/// The Contacts record where there is one, because a phone number and an email
/// address on one record are one person — that is the whole reason `--from
/// <their email>` finds what they sent from their phone. The normalized handle
/// otherwise, which is the most that can be said without a record to join them
/// by: two shapes of one unknown number are still one person.
///
/// Shared so that "the same person" means the same thing to everything that
/// asks. A conversation and a search that disagreed about it would be a subtle
/// and very confusing bug.
fn person_identity(handle: &str, contact: Option<&Contact>) -> String {
    match contact {
        Some(contact) => format!("contact:{}", contact.id),
        None => format!(
            "handle:{}",
            crate::contacts::handle_key(handle).unwrap_or_else(|| handle.to_string())
        ),
    }
}

/// The person a one-to-one conversation is with, or `None` for a group.
///
/// A group has no single person to be, which is exactly why it cannot stand in
/// for one when conversations are being told apart.
fn sole_person(chat: &Chat, contacts: &ContactIndex) -> Option<String> {
    if chat.is_group {
        return None;
    }
    // For a one-to-one this is the single handle, since it is the whole of the
    // conversation's membership.
    let handle = chat.handles.as_deref()?;
    Some(person_identity(handle, contacts.contact(Some(handle))))
}

/// Everyone a spec could name, keyed by identity, and every address they use.
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
///
/// A tie among several is narrowed by an exact name before it is reported:
/// either of someone's two names counts — whichever is shown, and the filed
/// name a nickname displaced — since typing the whole of one is as definite as
/// typing the whole of the other. Unless two records answer to it exactly,
/// which is the tie this cannot silently pick from.
fn people_matching(
    db: &Connection,
    spec: &str,
    contacts: &ContactIndex,
) -> Result<BTreeMap<String, Person>> {
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

    let identity = |handle: &str, contact: Option<&Contact>| person_identity(handle, contact);

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
            filed_as: contact
                .as_ref()
                .and_then(|contact| contact.filed_as.clone()),
            handle_ids: Vec::new(),
            handles: Vec::new(),
        });
        person.handle_ids.push(*rowid);
        person.handles.push(handle.clone());
    }

    if people.len() > 1 {
        let exactly_named = |person: &Person| {
            person.name.to_lowercase() == lowered
                || person
                    .filed_as
                    .as_ref()
                    .is_some_and(|filed| filed.to_lowercase() == lowered)
        };
        if people
            .values()
            .filter(|person| exactly_named(person))
            .count()
            == 1
        {
            people.retain(|_, person| exactly_named(person));
        }
    }
    Ok(people)
}

/// Find one person, and gather every address they use — [`people_matching`],
/// required to name exactly one.
pub fn resolve_person(db: &Connection, spec: &str, contacts: &ContactIndex) -> Result<Person> {
    let mut people = people_matching(db, spec, contacts)?;
    match people.len() {
        0 => Err(Error::other(format!("no one matching {spec}"))),
        1 => Ok(people
            .pop_first()
            .map(|(_, person)| person)
            .expect("one match")),
        _ => Err(several_people(spec, &people)),
    }
}

/// The error for a spec that names more than one person, listing them.
fn several_people(spec: &str, people: &BTreeMap<String, Person>) -> Error {
    Error::other(format!(
        "{} people match {spec}: {}",
        people.len(),
        describe(people.values().take(6))
    ))
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

/// How many candidates a name is resolved against before giving up on listing
/// them. A cap rather than a total, which is why an error that reaches it says
/// "at least" instead of naming a number it cannot stand behind.
const CHAT_MATCH_SCAN: i64 = 50;

/// Find the room whose members are exactly these people, and me.
///
/// Each argument named a person, so the conversation asked for is the one whose
/// membership is that set — `naming-a-conversation.md §4`. Exactly that set:
/// there is no fallback to a room that merely contains them, because answering
/// with a conversation holding people nobody named is the failure this rule
/// exists to prevent, and "no conversation with exactly those people" is an
/// error the caller can act on by naming the rest.
///
/// Compared on identities rather than on handle ids, for the reason everything
/// else here compares on them: one person reachable at two addresses is one
/// member, and a room holding both of their addresses is still a room with one
/// of them in it.
pub fn resolve_room(db: &Connection, people: &[Person], contacts: &ContactIndex) -> Result<Chat> {
    let wanted: BTreeSet<String> = people
        .iter()
        .flat_map(|person| person.handles.iter())
        .map(|handle| person_identity(handle, contacts.contact(Some(handle))))
        .collect();

    // Naming one person twice describes a room of one, which is a one-to-one
    // and not a room. Counted on identities rather than on rendered names,
    // because two records can carry one name and are two people — counting
    // names refused their room as though one person had been named twice.
    //
    // Only that direction. Resolving the same person by two different specs
    // yields the same rendered name from both, since `Person.name` comes from
    // the resolved record rather than from what was typed, so a name count
    // caught that case too. Identity is the right key regardless — it is what
    // `wanted` is built from one line below, so the guard and the membership
    // comparison read one set instead of two rules kept in step.
    if wanted.len() < people.len() {
        return Err(Error::other(
            "the same person named twice; a room needs different people",
        ));
    }

    // Every chat any of them is in, then their whole membership, so a room with
    // an extra member can be told apart from an exact one. `chat_handle_join`
    // holds the other participants only — I am never a row — so the set below
    // is the set the caller named.
    let ids: Vec<i64> = people
        .iter()
        .flat_map(|person| person.handle_ids.iter().copied())
        .collect();
    let slots = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT theirs.chat_id, handle.id
           FROM chat_handle_join AS theirs
           JOIN handle ON handle.rowid = theirs.handle_id
          WHERE theirs.chat_id IN (
                SELECT chat_id FROM chat_handle_join WHERE handle_id IN ({slots}))"
    );
    let mut statement = db.prepare(&sql)?;
    let mut rows = statement.query(params_from_iter(ids.iter().map(|id| Value::from(*id))))?;
    let mut membership: BTreeMap<i64, BTreeSet<String>> = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let chat_id: i64 = row.get(0)?;
        let handle: String = row.get(1)?;
        membership
            .entry(chat_id)
            .or_default()
            .insert(person_identity(&handle, contacts.contact(Some(&handle))));
    }

    let exact: Vec<i64> = membership
        .into_iter()
        .filter(|(_, who)| *who == wanted)
        .map(|(chat_id, _)| chat_id)
        .collect();
    let named = people
        .iter()
        .map(|person| person.name.as_str())
        .collect::<Vec<_>>()
        .join(" and ");
    if exact.is_empty() {
        return Err(Error::other(format!(
            "no conversation with exactly {named}"
        )));
    }

    // Ordered by activity, so the most recent wins if Messages somehow holds
    // two rooms with identical membership — which it does, for a room left and
    // rejoined. Looked up by id rather than scanned, so a room that has been
    // quiet for years is found as readily as one from this morning.
    let mut found = chats_by_id(db, &exact, contacts)?;
    if found.is_empty() {
        return Err(Error::other(format!(
            "no conversation with exactly {named}"
        )));
    }
    Ok(found.remove(0))
}

/// Keep a merged conversation inside one filtered bucket.
///
/// The leading thread is kept whatever it is: naming a conversation reaches it
/// even when Messages filters it, which predates merging, and dropping it would
/// move the send target `conversation-merging.md §7` promises not to move. But
/// nothing merges into a filtered head, so whether Unknown Senders content
/// appears never turns on which thread happens to be more recently active.
fn one_bucket(mut threads: Vec<Chat>, unknown: bool) -> Vec<Chat> {
    let leading = threads.remove(0);
    if !unknown && leading.is_filtered {
        return vec![leading];
    }
    let mut conversation = vec![leading];
    conversation.extend(
        threads
            .into_iter()
            .filter(|chat| unknown || !chat.is_filtered),
    );
    conversation
}

/// Find a single chat by rowid, identifier, or name substring.
///
/// The one a message would be sent to when a person has several, which is the
/// most recently active — [`resolve_conversation`] decides that, and this takes
/// its first answer. Kept separate because sending needs exactly one
/// conversation and reading does not (conversation-merging.md §7).
pub fn resolve_chat(db: &Connection, spec: &str, contacts: &ContactIndex) -> Result<Chat> {
    Ok(resolve_conversation(db, spec, contacts, false)?
        .into_iter()
        .next()
        .expect("a resolved conversation holds at least one chat"))
}

/// The conversation several specs name, each spec being one person.
///
/// One spec is the person's own conversation, which is
/// [`resolve_conversation`]. Several are the room whose members are exactly
/// those people (`naming-a-conversation.md §4`), which is one room and never
/// merges — two rooms with the same membership are two rooms, and a room is not
/// a person with several addresses.
///
/// Every spec resolves through `resolve_person`, the one primitive (§8), so a
/// name means the same thing here as it does to `--with` and `--from`. A spec
/// that names nobody is an error rather than a fallback: with several people
/// named there is no other reading of it, and guessing would answer with a room
/// the caller did not describe.
/// Whether any room's own name is exactly this spec.
///
/// Asked of the whole chat table rather than of a scan window: a label
/// somebody chose does not expire by going quiet, and the person-first path
/// must not win a tie it cannot see. Compared in Rust so the case folding is
/// the one every other name comparison uses, not SQLite's ASCII-only NOCASE.
fn exactly_names_a_room(db: &Connection, spec: &str) -> Result<bool> {
    let lowered = spec.to_lowercase();
    let mut statement = db.prepare(
        "SELECT display_name FROM chat
          WHERE display_name IS NOT NULL AND display_name != ''
            AND (SELECT COUNT(*) FROM chat_handle_join
                  WHERE chat_id = chat.rowid) > 1",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(0)?;
        if name.to_lowercase() == lowered {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn resolve_conversations(
    db: &Connection,
    specs: &[String],
    contacts: &ContactIndex,
    unknown: bool,
) -> Result<Vec<Chat>> {
    match specs {
        [] => Err(Error::other("no conversation named")),
        [one] => resolve_conversation(db, one, contacts, unknown),
        several => {
            let people = several
                .iter()
                .map(|spec| resolve_person(db, spec, contacts))
                .collect::<Result<Vec<Person>>>()?;
            Ok(vec![resolve_room(db, &people, contacts)?])
        }
    }
}

/// Every conversation with the person a spec names, most recently active first.
///
/// A name means the person and a rowid means the thread
/// (conversation-merging.md §5), so a rowid answers with exactly the thread
/// asked for and a name answers with all of them. One element is the ordinary
/// case; more than one is somebody reachable at more than one address, whose
/// messages Messages splits across a conversation per address.
///
/// `unknown` is what lets a filtered thread join the merge. Without it a
/// conversation Messages files under Unknown Senders stays out, since merging
/// it would quietly promote filtered content into one the user considers known.
pub fn resolve_conversation(
    db: &Connection,
    spec: &str,
    contacts: &ContactIndex,
    unknown: bool,
) -> Result<Vec<Chat>> {
    // Naming a chat outright reaches it even when Messages filters it.
    let is_rowid = !spec.is_empty() && spec.bytes().all(|byte| byte.is_ascii_digit());
    if is_rowid {
        let wanted: i64 = spec
            .parse()
            .map_err(|_| Error::other(format!("no chat matching {spec}")))?;
        let found = chats_by_id(db, &[wanted], contacts)?;
        if found.is_empty() {
            return Err(Error::other(format!("no chat matching {spec}")));
        }
        return Ok(found);
    }

    // The person first, always. A spec that names somebody is resolved to the
    // person and the person to their threads — the contact, then every address
    // it holds, then the chats those addresses are in
    // (naming-a-conversation.md §3, §8). Chat rows are never matched directly
    // for a person: rows are matched inside a scan window, and whose
    // conversation a name means must not depend on what else has been noisy
    // lately (resolver-windows.md §3).
    //
    // A spec that names several people is an error, not a pick — unless none
    // of them has a contact, in which case it was a fragment of an address,
    // and fragments of addresses are text to search below like anything else
    // no contact claims.
    // A room named exactly this falls through to the text match instead of
    // being beaten — or being *spoken over*: a room's own name is a label
    // somebody chose, so typing the whole of it claims the room as definitely
    // as a name claims the person (§4). The text match below is where whole
    // claims meet: the room wins outright unless somebody is *named* exactly
    // the string too, and then the two whole claims are the tie it reports.
    // The axis is whether another claim on the string is whole, never how
    // many people answer to part of it. A room that merely matched by
    // membership never competes — being in a room is not being the person
    // (§3).
    let mut people = people_matching(db, spec, contacts)?;
    let named_room = !people.is_empty() && exactly_names_a_room(db, spec)?;
    if people.len() > 1 && !named_room && people.keys().any(|key| key.starts_with("contact:")) {
        return Err(several_people(spec, &people));
    }
    if people.len() == 1 && !named_room {
        let person = people
            .pop_first()
            .map(|(_, person)| person)
            .expect("one match");
        let theirs = one_to_one_chats(db, &person)?;
        if !theirs.is_empty() {
            // Named rather than scanned, for the reason `chats_by_id` exists.
            let mut mine = chats_by_id(db, &theirs, contacts)?;
            if !mine.is_empty() {
                // An address leads with its own thread. Reading still gets
                // the whole conversation either way, but the leading thread
                // is where a send goes, and a send addressed to a number must
                // not drift to whichever thread spoke last
                // (conversation-merging.md §7).
                //
                // A whole address leads on its key, and a fragment on the
                // substring rule every other address fragment uses — the
                // `addressed` arm of `retain_matching`, the `loosely` pass of
                // `resolve_person` — so typing less of the address does not
                // quietly hand the aim back to activity order. A name has no
                // key, so a name cannot reorder anything.
                if let Some(wanted) = crate::contacts::handle_key(spec) {
                    let typed = spec.to_lowercase();
                    mine.sort_by_key(|chat| {
                        let handles = chat.handles.as_deref().unwrap_or("");
                        (
                            crate::contacts::handle_key(handles) != Some(wanted.clone()),
                            !handles.to_lowercase().contains(&typed),
                        )
                    });
                }
                return Ok(one_bucket(mine, unknown));
            }
        }
        // No conversation of their own: the rooms they are in are all there
        // is, and those are found the way rooms are found, below.
    }

    // What is left is not a person: a room's own name, a membership fragment,
    // a group identifier, an address fragment. Those are text over chat rows.
    let matches = fetch_chats(db, Some(spec), CHAT_MATCH_SCAN, contacts, true)?;
    if matches.is_empty() {
        return Err(Error::other(format!("no chat matching {spec}")));
    }
    if matches.len() == 1 {
        return Ok(matches);
    }

    let lowered = spec.to_lowercase();
    let mut exact: Vec<Chat> = matches
        .iter()
        .filter(|chat| {
            chat.name.to_lowercase() == lowered
                // Only where the conversation *is* that person, which is what
                // the case above says for a name and what a group can never
                // say. A group is found by a member's whole name or nickname —
                // that is the clause in `fetch_chats` — but being one of the
                // people in a room is not being the room, so counting it here
                // would leave every tie unbroken for anyone you also share an
                // unnamed group with. Which is most people you talk to.
                || (chat.display_name.is_none()
                    && !chat.is_group
                    && contacts.any_named(chat.handles.as_deref(), &lowered))
        })
        .cloned()
        .collect();
    if exact.len() == 1 {
        return Ok(vec![exact.remove(0)]);
    }

    // Say how many are not being shown, rather than printing six and reporting
    // a bigger number with nothing to explain the gap. The total is itself a
    // floor: the search stops at `CHAT_MATCH_SCAN`, so a count that reaches it
    // says "at least", not "exactly".
    const SHOWN: usize = 6;
    let names = matches
        .iter()
        .take(SHOWN)
        .map(|chat| format!("{} ({})", chat.name, chat.rowid))
        .collect::<Vec<_>>()
        .join(", ");
    let total = matches.len();
    let at_least = if i64::try_from(total) == Ok(CHAT_MATCH_SCAN) {
        "at least "
    } else {
        ""
    };
    let and_more = match total.saturating_sub(SHOWN) {
        0 => String::new(),
        rest => format!(", and {rest} more"),
    };
    Err(Error::other(format!(
        "{at_least}{total} chats match {spec}: {names}{and_more}"
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
        associated_message_type INTEGER DEFAULT 0, associated_message_guid TEXT,
          associated_message_emoji TEXT, date INTEGER, service TEXT,
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

    /// The whole of tapbacks.md §4–§6 in one conversation: all three stored
    /// guid forms reach their target, a removal cancels its add by sender and
    /// type family, a type 2006 reads its emoji off the column, an
    /// unidentified type renders nowhere, and an orphaned target matches
    /// nothing.
    #[test]
    fn reactions_land_on_the_message_they_react_to() {
        let db = fixture();
        /// rowid, type, from me, handle, target, emoji.
        type Reaction = (i64, i64, i64, i64, &'static str, Option<&'static str>);
        let reactions: [Reaction; 7] = [
            (10, 2000, 0, 2, "p:0/m1", None), // ❤️, cancelled by 13
            (11, 2001, 1, 1, "m1", None),     // 👍, the bare form
            (12, 2006, 0, 1, "p:0/m1", Some("🙏")),
            (13, 3000, 0, 2, "p:0/m1", None), // removes 10: same sender, family
            (14, 2007, 0, 1, "p:0/m1", None), // nothing identifies it (§9)
            (15, 2000, 0, 1, "p:0/ghost", None), // target no longer exists
            (16, 2001, 0, 2, "bp:m1", None),  // 👍, the second prefix form
        ];
        for (rowid, kind, from_me, handle, target, emoji) in reactions {
            db.execute(
                "INSERT INTO message (rowid, guid, text, is_from_me, handle_id,
                     associated_message_type, associated_message_guid,
                     associated_message_emoji, date, service)
                 VALUES (?, ?, NULL, ?, ?, ?, ?, ?, ?, 'iMessage')",
                rusqlite::params![
                    rowid,
                    format!("t{rowid}"),
                    from_me,
                    handle,
                    kind,
                    target,
                    emoji,
                    at(10 + rowid)
                ],
            )
            .unwrap();
            db.execute(
                "INSERT INTO chat_message_join (chat_id, message_id, message_date)
                 VALUES (1, ?, ?)",
                rusqlite::params![rowid, at(10 + rowid)],
            )
            .unwrap();
        }

        let messages = fetch_messages(
            &db,
            &FetchMessages {
                chat_id: Some(1),
                ..Default::default()
            },
            &ContactIndex::empty(),
        )
        .unwrap();
        let m1 = messages
            .iter()
            .find(|message| message.guid == "m1")
            .expect("the target");
        let symbols: Vec<&str> = m1
            .tapbacks
            .iter()
            .map(|tapback| tapback.symbol.as_str())
            .collect();
        assert_eq!(symbols, ["👍", "🙏", "👍"], "{:?}", m1.tapbacks);
        assert_eq!(m1.tapbacks[0].associated_message_type, 2001);
        assert!(m1.tapbacks[0].is_from_me);
        assert_eq!(m1.tapbacks[1].handle.as_deref(), Some("+13105551234"));
        // Everything else on the page stays bare, including the message the
        // orphaned reaction pointed at before its target vanished.
        assert!(
            messages
                .iter()
                .filter(|message| message.guid != "m1")
                .all(|message| message.tapbacks.is_empty())
        );
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

    /// A conversation long enough to have a middle, so a window has something
    /// to reach in both directions.
    fn talkative(db: &Connection) -> i64 {
        let chat = one_to_one(db, 4, "+16175550147");
        for n in 0..12 {
            message_in(db, chat, 100 + n, 10 + n);
        }
        // The hit sits in the middle, with room either side.
        db.execute(
            "UPDATE message SET text = 'the needle' WHERE rowid = 106",
            [],
        )
        .unwrap();
        chat
    }

    fn found(db: &Connection, context: Context) -> Vec<Message> {
        let hits = fetch_messages(
            db,
            &FetchMessages {
                query: Some("needle"),
                ..Default::default()
            },
            &ContactIndex::empty(),
        )
        .unwrap();
        with_context(db, hits, context, &ContactIndex::empty()).unwrap()
    }

    /// A window reaches both ways, in conversation order, and says which line
    /// was the hit.
    #[test]
    fn a_hit_comes_back_inside_its_conversation() {
        let db = fixture();
        talkative(&db);

        let out = found(
            &db,
            Context {
                before: 2,
                after: 3,
            },
        );
        let rowids: Vec<i64> = out.iter().map(|message| message.rowid).collect();
        assert_eq!(rowids, [104, 105, 106, 107, 108, 109], "{rowids:?}");

        // Exactly one of them is the hit, and it is the one that matched.
        let hits: Vec<i64> = out
            .iter()
            .filter(|message| message.matched)
            .map(|message| message.rowid)
            .collect();
        assert_eq!(hits, [106]);
        // All one run, since there is only one hit.
        assert!(out.iter().all(|message| message.group == Some(0)));
    }

    /// Asking for no context leaves the answer exactly as it was, which is what
    /// lets the JSON stay byte-identical for every caller that never asks.
    #[test]
    fn no_context_asked_for_changes_nothing() {
        let db = fixture();
        talkative(&db);

        let out = found(&db, Context::default());
        assert_eq!(out.len(), 1);
        assert!(out[0].matched && out[0].group.is_none());
    }

    /// A reaction is never a hit, and since brackets it is not a row in the
    /// window either: it rides its target, so a window cannot show the same
    /// reaction twice. This test used to pin the row *crossing into* the
    /// window — right when rows were the only way a reaction showed, and
    /// tapbacks.md §6's exact complaint once they were not.
    #[test]
    fn a_window_shows_a_reaction_on_its_target_not_as_a_row() {
        let db = fixture();
        let chat = talkative(&db);
        // A reaction to the hit itself, dated inside the window's reach.
        db.execute(
            "INSERT INTO message (rowid, guid, text, is_from_me, handle_id,
                 associated_message_type, associated_message_guid, date, service)
             VALUES (200, 'react', 'Liked \"the needle\"', 0, 4, 2000, 'p:0/g106', ?, 'iMessage')",
            [at(23)],
        )
        .unwrap();
        db.execute(
            "INSERT INTO chat_message_join (chat_id, message_id, message_date) VALUES (?, 200, ?)",
            rusqlite::params![chat, at(23)],
        )
        .unwrap();

        // A tapback can never be a hit...
        let bare = found(&db, Context::default());
        assert_eq!(bare.len(), 1, "{bare:?}");
        assert_eq!(bare[0].tapbacks.len(), 1, "{:?}", bare[0].tapbacks);

        // ...and the window carries it on the hit, not as a row of its own.
        let out = found(
            &db,
            Context {
                before: 0,
                after: 8,
            },
        );
        assert!(
            !out.iter().any(|message| message.rowid == 200),
            "{:?}",
            out.iter().map(|m| m.rowid).collect::<Vec<_>>()
        );
        let hit = out.iter().find(|message| message.rowid == 106).unwrap();
        assert_eq!(hit.tapbacks[0].symbol, "❤️");
    }

    /// Two hits close together are one stretch of conversation, and print once.
    ///
    /// Printing a window each would repeat the messages between them and read
    /// as though the exchange happened twice.
    #[test]
    fn windows_that_meet_become_one_run() {
        let db = fixture();
        talkative(&db);
        db.execute(
            "UPDATE message SET text = 'the needle' WHERE rowid = 109",
            [],
        )
        .unwrap();

        // 106 and 109, with two either side: 104-108 and 107-111 overlap.
        let out = found(
            &db,
            Context {
                before: 2,
                after: 2,
            },
        );
        let rowids: Vec<i64> = out.iter().map(|message| message.rowid).collect();
        assert_eq!(
            rowids,
            [104, 105, 106, 107, 108, 109, 110, 111],
            "{rowids:?}"
        );
        // Nothing repeated, and all of it one run.
        assert!(out.iter().all(|message| message.group == Some(0)));

        let hits: Vec<i64> = out
            .iter()
            .filter(|message| message.matched)
            .map(|message| message.rowid)
            .collect();
        assert_eq!(hits, [106, 109]);
    }

    /// A negative width asks for nothing, not for everything.
    ///
    /// The CLI cannot send one — `counted` refuses it — but the daemon takes
    /// these off a socket from a client that need not hold Full Disk Access,
    /// which makes them the one input here not already checked by the binary
    /// that produced it. Unclamped, `limit: -1` reaches SQLite as `LIMIT -1`,
    /// which means no limit at all, and the whole conversation is fetched and
    /// decoded once per hit.
    #[test]
    fn a_negative_width_is_no_window_rather_than_the_whole_chat() {
        let db = fixture();
        talkative(&db);

        let out = found(
            &db,
            Context {
                before: -1,
                after: 1,
            },
        );
        let rowids: Vec<i64> = out.iter().map(|message| message.rowid).collect();
        assert_eq!(rowids, [106, 107], "{rowids:?}");
    }

    /// A hit inside another hit's window is still a hit.
    ///
    /// The case `-C` exists for — several hits in one exchange — and the one
    /// where the marker is easiest to lose, because the later hit arrives twice:
    /// once as context around the earlier one, once as itself. Whichever copy
    /// survives the merge has to be the one that says it matched.
    #[test]
    fn a_hit_inside_another_hits_window_keeps_its_marker() {
        let db = fixture();
        talkative(&db);
        db.execute(
            "UPDATE message SET text = 'the needle' WHERE rowid = 107",
            [],
        )
        .unwrap();

        let out = found(
            &db,
            Context {
                before: 2,
                after: 2,
            },
        );
        let rowids: Vec<i64> = out.iter().map(|message| message.rowid).collect();
        assert_eq!(rowids, [104, 105, 106, 107, 108, 109], "{rowids:?}");

        let hits: Vec<i64> = out
            .iter()
            .filter(|message| message.matched)
            .map(|message| message.rowid)
            .collect();
        assert_eq!(hits, [106, 107], "{hits:?}");
    }

    /// Touching counts as meeting, not only overlapping.
    ///
    /// With one either side, 106 reaches 107 and 109 reaches 108 — the two
    /// windows share no message but leave no gap, so they are one stretch of
    /// conversation and a separator between them would be a lie.
    #[test]
    fn windows_that_only_touch_become_one_run() {
        let db = fixture();
        talkative(&db);
        db.execute(
            "UPDATE message SET text = 'the needle' WHERE rowid = 109",
            [],
        )
        .unwrap();

        let out = found(
            &db,
            Context {
                before: 1,
                after: 1,
            },
        );
        let rowids: Vec<i64> = out.iter().map(|message| message.rowid).collect();
        assert_eq!(rowids, [105, 106, 107, 108, 109, 110], "{rowids:?}");
        assert!(out.iter().all(|message| message.group == Some(0)));
    }

    /// And a real gap stays a gap, or the separator would mean nothing.
    #[test]
    fn windows_with_a_gap_stay_separate_runs() {
        let db = fixture();
        talkative(&db);
        db.execute(
            "UPDATE message SET text = 'the needle' WHERE rowid = 111",
            [],
        )
        .unwrap();

        // 106 reaches 107, 111 reaches back to 110: 108 and 109 sit between.
        // 111 is the last message there is, so its run has no forward half.
        let out = found(
            &db,
            Context {
                before: 1,
                after: 1,
            },
        );
        let rowids: Vec<i64> = out.iter().map(|message| message.rowid).collect();
        assert_eq!(rowids, [105, 106, 107, 110, 111], "{rowids:?}");
        let groups: Vec<Option<i64>> = out.iter().map(|message| message.group).collect();
        assert_eq!(
            groups,
            [Some(0), Some(0), Some(0), Some(1), Some(1)],
            "{rowids:?}"
        );
    }

    /// A window belongs to its own conversation. Two hits in two chats have
    /// nothing to do with each other, however close their rowids happen to be.
    #[test]
    fn a_window_never_reaches_into_another_conversation() {
        let db = fixture();
        let chat = talkative(&db);
        let other = one_to_one(&db, 5, "+16175550148");
        // Interleaved rowids, so a window that ignored the chat would grab them.
        for n in 0..4 {
            message_in(&db, other, 300 + n, 40 + n);
        }
        db.execute(
            "UPDATE message SET text = 'the needle' WHERE rowid = 301",
            [],
        )
        .unwrap();

        let out = found(
            &db,
            Context {
                before: 9,
                after: 9,
            },
        );
        let strayed: Vec<i64> = out
            .iter()
            .filter(|message| message.chat_id != chat && message.chat_id != other)
            .map(|message| message.rowid)
            .collect();
        assert!(strayed.is_empty(), "{strayed:?}");

        // Each conversation's run holds only its own messages.
        for group in [Some(0), Some(1)] {
            let chats: BTreeSet<i64> = out
                .iter()
                .filter(|message| message.group == group)
                .map(|message| message.chat_id)
                .collect();
            assert_eq!(chats.len(), 1, "run {group:?} spans {chats:?}");
        }
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

    /// A conversation reads as what you call the person, and answers to both
    /// names. The filed name is displaced, not discarded.
    #[test]
    fn a_conversation_is_shown_as_the_nickname_and_found_by_either_name() {
        let db = fixture();
        let rowid = one_to_one(&db, 4, "+16175550147");
        let contacts = ContactIndex::for_test([("+16175550147", "source:7", "Robin Adeyemi")])
            .nicknamed("+16175550147", "Rocket");

        // Either name finds it, and it reads as the nickname whichever was
        // typed — the display does not follow the query.
        for spec in ["rocket", "adeyemi"] {
            let chats = fetch_chats(&db, Some(spec), 30, &contacts, false).unwrap();
            let names: Vec<&str> = chats.iter().map(|chat| chat.name.as_str()).collect();
            assert_eq!(names, ["Rocket"], "searching {spec}");
        }

        // Which is what reading and sending resolve through, so both take it.
        for spec in ["Rocket", "Robin Adeyemi"] {
            let chat = resolve_chat(&db, spec, &contacts).unwrap();
            assert_eq!(
                (chat.rowid, chat.name.as_str()),
                (rowid, "Rocket"),
                "{spec}"
            );
        }
    }

    /// The same for a person, so `--with` and `--from` take either name — and
    /// gather every address, since what resolved is the contact.
    #[test]
    fn a_person_is_shown_as_the_nickname_and_found_by_either_name() {
        let db = fixture();
        let contacts = ContactIndex::for_test([
            ("+13105551234", "source:7", "Robin Adeyemi"),
            ("someone@example.com", "source:7", "Robin Adeyemi"),
        ])
        .nicknamed("+13105551234", "Rocket")
        .nicknamed("someone@example.com", "Rocket");

        for spec in ["rocket", "robin adeyemi"] {
            let person = resolve_person(&db, spec, &contacts).unwrap();
            assert_eq!(person.name, "Rocket", "resolving {spec}: {person:?}");
            assert_eq!(person.handle_ids.len(), 2, "resolving {spec}: {person:?}");
        }
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

    /// The same person, in a group that has no name of its own.
    ///
    /// The ordinary case, and the one that breaks a tie-break reaching into
    /// membership: someone you message directly and also share a group with.
    /// A room with no name of its own, holding exactly these handles.
    fn room(db: &Connection, rowid: i64, members: &[i64]) {
        db.execute(
            "INSERT INTO chat (rowid, guid, chat_identifier, display_name, is_filtered)
             VALUES (?, ?, ?, '', 0)",
            rusqlite::params![
                rowid,
                format!("iMessage;+;chat{rowid}"),
                format!("chat{rowid}")
            ],
        )
        .unwrap();
        for member in members {
            db.execute(
                "INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (?, ?)",
                rusqlite::params![rowid, member],
            )
            .unwrap();
        }
    }

    fn also_in_an_unnamed_group(db: &Connection, member: i64) {
        db.execute(
            "INSERT INTO handle (rowid, id) VALUES (9, '+16175550148')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO chat (rowid, guid, chat_identifier, display_name, is_filtered)
             VALUES (9, 'iMessage;+;chat9x', 'chat9x', '', 0)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (9, ?), (9, 9)",
            [member],
        )
        .unwrap();
    }

    /// A whole name still picks the one-to-one, even when that person is also in
    /// a group with no name of its own.
    ///
    /// Breaking that tie is the entire job of the exact filter, and asking about
    /// membership without asking whether the conversation *is* that person made
    /// it stop doing the job: the one-to-one qualified by its name, the group
    /// qualified on the member's behalf, and resolving anyone you share an
    /// unnamed group with started erroring instead of answering. No nickname is
    /// involved, which is what makes it a regression rather than a rough edge.
    #[test]
    fn a_whole_name_still_picks_the_one_to_one() {
        let db = fixture();
        let alone = one_to_one(&db, 4, "+16175550147");
        also_in_an_unnamed_group(&db, 4);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("+16175550148", "source:8", "Kit Alvarez"),
        ]);

        let chat = resolve_chat(&db, "Robin Adeyemi", &contacts).unwrap();
        assert_eq!(chat.rowid, alone, "{chat:?}");
    }

    /// And so does a whole nickname, which is the headline of this branch and
    /// fails in exactly the same shape — a nickname is only worth typing if
    /// typing it lands somewhere.
    #[test]
    fn a_whole_nickname_still_picks_the_one_to_one() {
        let db = fixture();
        let alone = one_to_one(&db, 4, "+16175550147");
        also_in_an_unnamed_group(&db, 4);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("+16175550148", "source:8", "Kit Alvarez"),
        ])
        .nicknamed("+16175550147", "Rocket");

        // The group is still *found* by the nickname, the way it is found by the
        // member's name — it is only the tie-break that must not count it.
        let matched = fetch_chats(&db, Some("rocket"), 30, &contacts, false).unwrap();
        assert_eq!(matched.len(), 2, "{matched:?}");

        let chat = resolve_chat(&db, "Rocket", &contacts).unwrap();
        assert_eq!(chat.rowid, alone, "{chat:?}");
    }

    /// Put a message in a conversation, so `lastDate` orders it against others.
    fn message_in(db: &Connection, chat: i64, rowid: i64, minutes: i64) {
        let date = at(minutes);
        db.execute(
            "INSERT INTO message (rowid, guid, text, is_from_me, handle_id, date, service)
             VALUES (?, ?, 'hi', 0, ?, ?, 'iMessage')",
            rusqlite::params![rowid, format!("g{rowid}"), chat, date],
        )
        .unwrap();
        db.execute(
            "INSERT INTO chat_message_join (chat_id, message_id, message_date) VALUES (?, ?, ?)",
            rusqlite::params![chat, rowid, date],
        )
        .unwrap();
    }

    /// One person reachable two ways is two conversations and one answer.
    ///
    /// Messages keeps a conversation per address, so naming someone who has a
    /// phone number and an email address matched both exactly and asked which —
    /// a question with no answer, since it is the same person either way. The
    /// one last active is the conversation you would be continuing.
    #[test]
    fn two_conversations_with_one_person_resolve_to_the_latest() {
        let db = fixture();
        let older = one_to_one(&db, 4, "+16175550147");
        let newer = one_to_one(&db, 5, "robin@example.com");
        message_in(&db, older, 10, 5);
        message_in(&db, newer, 11, 90);

        // One Contacts record, so one person however they were reached.
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("robin@example.com", "source:7", "Robin Adeyemi"),
        ])
        .nicknamed("+16175550147", "Rocket")
        .nicknamed("robin@example.com", "Rocket");

        // A fragment too. Fewer letters does not make it two people, and a
        // substring is a documented way to name a chat, so it is how this
        // actually gets typed.
        for spec in ["Robin Adeyemi", "Rocket", "adeyemi", "robin"] {
            let chat = resolve_chat(&db, spec, &contacts).unwrap();
            assert_eq!(chat.rowid, newer, "resolving {spec}: {chat:?}");
        }
    }

    /// A two-address person reads as one conversation, in arrival order.
    ///
    /// The half `resolve_chat` does not answer with used to be unreachable —
    /// not merged, not mentioned, absent. Both halves now arrive interleaved by
    /// rowid, which is arrival and is the same number in either thread.
    #[test]
    fn both_of_a_persons_threads_are_read_as_one_conversation() {
        let db = fixture();
        let phone = one_to_one(&db, 4, "+16175550147");
        let email = one_to_one(&db, 5, "robin@example.com");
        // Interleaved on purpose: neither thread is wholly older than the other,
        // so concatenating them in either order gives the wrong transcript.
        message_in(&db, phone, 10, 5);
        message_in(&db, email, 11, 6);
        message_in(&db, phone, 12, 7);
        message_in(&db, email, 13, 90);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("robin@example.com", "source:7", "Robin Adeyemi"),
        ]);

        let threads = resolve_conversation(&db, "Robin", &contacts, false).unwrap();
        let rowids: Vec<i64> = threads.iter().map(|chat| chat.rowid).collect();
        assert_eq!(rowids, [email, phone], "most recently active first");

        let messages = fetch_conversation(&db, &threads, None, 50, false, &contacts).unwrap();
        let ids: Vec<i64> = messages.iter().map(|message| message.rowid).collect();
        assert_eq!(ids, [10, 11, 12, 13], "one transcript in arrival order");

        // And the reply says which thread a send would continue, and which were
        // folded in behind it.
        let reply = crate::daemon::protocol::ChatReply::new(threads, messages);
        assert_eq!(reply.chat.rowid, email);
        assert_eq!(reply.merged, [phone]);
    }

    /// The limit counts the merged conversation, not each thread.
    #[test]
    fn the_newest_are_taken_across_both_threads() {
        let db = fixture();
        let phone = one_to_one(&db, 4, "+16175550147");
        let email = one_to_one(&db, 5, "robin@example.com");
        for rowid in [10, 12, 14] {
            message_in(&db, phone, rowid, rowid);
        }
        for rowid in [11, 13, 15] {
            message_in(&db, email, rowid, rowid);
        }
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("robin@example.com", "source:7", "Robin Adeyemi"),
        ]);

        let threads = resolve_conversation(&db, "Robin", &contacts, false).unwrap();
        let messages = fetch_conversation(&db, &threads, None, 3, false, &contacts).unwrap();
        let ids: Vec<i64> = messages.iter().map(|message| message.rowid).collect();
        // Not the newest three of each, which would be all six, and not the
        // newest three of whichever thread was asked first.
        assert_eq!(ids, [13, 14, 15]);
    }

    /// A rowid names one thread, so it is the way out of a merge that is wrong.
    #[test]
    fn a_rowid_names_one_thread_and_never_merges() {
        let db = fixture();
        let phone = one_to_one(&db, 4, "+16175550147");
        let email = one_to_one(&db, 5, "robin@example.com");
        message_in(&db, phone, 10, 5);
        message_in(&db, email, 11, 90);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("robin@example.com", "source:7", "Robin Adeyemi"),
        ]);

        let threads = resolve_conversation(&db, &phone.to_string(), &contacts, false).unwrap();
        let rowids: Vec<i64> = threads.iter().map(|chat| chat.rowid).collect();
        assert_eq!(rowids, [phone], "a rowid is a thread, not a person");
        let messages = fetch_conversation(&db, &threads, None, 50, false, &contacts).unwrap();
        let ids: Vec<i64> = messages.iter().map(|message| message.rowid).collect();
        assert_eq!(ids, [10]);
    }

    /// The rooms a person is in cannot crowd their own thread out of the answer.
    ///
    /// The name matches both of Robin's threads and every room Robin is in,
    /// and the resolver keeps only the newest `CHAT_MATCH_SCAN` matches. With
    /// more rooms than the window holds, all more recently active than the
    /// phone thread, the phone thread falls outside it. The answer used to be
    /// intersected with that window, so the reader returned half a
    /// conversation — the defect resolver-windows.md measured, reduced to a
    /// fixture. A resolved person's threads are looked up directly now, so how
    /// many rooms they share cannot change what the reader answers.
    #[test]
    fn rooms_cannot_crowd_a_persons_own_thread_out_of_the_answer() {
        let db = fixture();
        let phone = one_to_one(&db, 4, "+16175550147");
        let email = one_to_one(&db, 5, "robin@example.com");
        message_in(&db, phone, 10, 5);
        message_in(&db, email, 11, 90);
        for i in 0..CHAT_MATCH_SCAN + 5 {
            let room = 200 + i;
            db.execute(
                "INSERT INTO chat (rowid, guid, chat_identifier, display_name, is_filtered)
                 VALUES (?, ?, ?, '', 0)",
                rusqlite::params![room, format!("iMessage;+;room{i}"), format!("room{i}")],
            )
            .unwrap();
            db.execute(
                "INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (?, 1), (?, 4)",
                rusqlite::params![room, room],
            )
            .unwrap();
            message_in(&db, room, 1000 + i, 10 + i);
        }
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("robin@example.com", "source:7", "Robin Adeyemi"),
        ]);

        let threads = resolve_conversation(&db, "Robin", &contacts, false).unwrap();
        let rowids: Vec<i64> = threads.iter().map(|chat| chat.rowid).collect();
        assert_eq!(rowids, [email, phone], "both threads, however many rooms");
    }

    /// An address typed in full keeps its thread at the head of the answer.
    ///
    /// Reading merges either way. But `resolve_chat` takes the first element,
    /// and `send` resolves through it, so the leading thread is where a
    /// message goes — and a send addressed to a number must not drift to
    /// whichever thread spoke last. The routes are not interchangeable: one
    /// of them may be an SMS fallback, and naming an address is how you say
    /// which. The room carries both handles so the spec matches two rows and
    /// the merge runs rather than the single-match return.
    #[test]
    fn an_address_keeps_the_send_target_when_threads_merge() {
        let db = fixture();
        let phone = one_to_one(&db, 4, "+16175550147");
        let email = one_to_one(&db, 5, "robin@example.com");
        message_in(&db, phone, 10, 5);
        message_in(&db, email, 11, 90);
        room(&db, 20, &[4, 5]);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("robin@example.com", "source:7", "Robin Adeyemi"),
        ]);

        // The email thread is the newer, so this is the direction that
        // drifts: activity order would put the email thread first.
        let by_phone = resolve_conversation(&db, "+16175550147", &contacts, false).unwrap();
        let rowids: Vec<i64> = by_phone.iter().map(|chat| chat.rowid).collect();
        assert_eq!(rowids, [phone, email], "the typed address leads");

        // And the quiet direction holds too, rather than being order luck.
        let by_email = resolve_conversation(&db, "robin@example.com", &contacts, false).unwrap();
        let rowids: Vec<i64> = by_email.iter().map(|chat| chat.rowid).collect();
        assert_eq!(
            rowids,
            [email, phone],
            "either address, its own thread first"
        );

        // A fragment of an address aims the same way. It has no whole key,
        // but it reaches the person all the same — a partial email is a plain
        // substring — and typing less of the address must not quietly hand
        // the aim back to activity order.
        let by_part = resolve_conversation(&db, "+1617555014", &contacts, false).unwrap();
        let rowids: Vec<i64> = by_part.iter().map(|chat| chat.rowid).collect();
        assert_eq!(rowids, [phone, email], "a partial number still aims");

        // The phone thread is now the newest, so a partial email that fell
        // back to activity order would answer with the phone thread leading.
        message_in(&db, phone, 12, 200);
        for spec in ["robin@example", "robin@"] {
            let threads = resolve_conversation(&db, spec, &contacts, false).unwrap();
            let rowids: Vec<i64> = threads.iter().map(|chat| chat.rowid).collect();
            assert_eq!(rowids, [email, phone], "a partial email still aims: {spec}");
        }
    }

    /// Unknown Senders content does not arrive inside a known conversation.
    #[test]
    fn a_filtered_thread_stays_out_of_the_merge_unless_asked() {
        let db = fixture();
        let known = one_to_one(&db, 4, "+16175550147");
        let filtered = one_to_one(&db, 5, "robin@example.com");
        db.execute(
            "UPDATE chat SET is_filtered = 1 WHERE rowid = ?",
            [filtered],
        )
        .unwrap();
        message_in(&db, filtered, 10, 5);
        message_in(&db, known, 11, 90);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("robin@example.com", "source:7", "Robin Adeyemi"),
        ]);

        let threads = resolve_conversation(&db, "Robin", &contacts, false).unwrap();
        assert_eq!(
            threads.iter().map(|chat| chat.rowid).collect::<Vec<_>>(),
            [known],
            "the filtered half is left out"
        );

        let asked = resolve_conversation(&db, "Robin", &contacts, true).unwrap();
        assert_eq!(
            asked.iter().map(|chat| chat.rowid).collect::<Vec<_>>(),
            [known, filtered],
            "--unknown lets it in"
        );
    }

    /// The rule holds when the filtered thread is the more recent one.
    ///
    /// Naming a conversation reaches it even when Messages filters it, so a
    /// filtered thread can legitimately lead. What it may not do is bring the
    /// known thread in behind it, which is the mixing the case above forbids in
    /// the other direction.
    #[test]
    fn a_filtered_thread_does_not_merge_the_known_one_into_itself() {
        let db = fixture();
        let known = one_to_one(&db, 4, "+16175550147");
        let filtered = one_to_one(&db, 5, "robin@example.com");
        db.execute(
            "UPDATE chat SET is_filtered = 1 WHERE rowid = ?",
            [filtered],
        )
        .unwrap();
        message_in(&db, known, 10, 5);
        // The filtered one is now the most recently active, so it leads.
        message_in(&db, filtered, 11, 90);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("robin@example.com", "source:7", "Robin Adeyemi"),
        ]);

        let threads = resolve_conversation(&db, "Robin", &contacts, false).unwrap();
        assert_eq!(
            threads.iter().map(|chat| chat.rowid).collect::<Vec<_>>(),
            [filtered],
            "the known thread stays out of a filtered conversation"
        );

        let asked = resolve_conversation(&db, "Robin", &contacts, true).unwrap();
        assert_eq!(
            asked.iter().map(|chat| chat.rowid).collect::<Vec<_>>(),
            [filtered, known],
            "--unknown merges in both directions"
        );
    }

    /// A person is found however long their conversation has been quiet.
    ///
    /// The resolver used to enter through a scan of the newest
    /// `NAME_SEARCH_SCAN` chat rows, so a thread past that window answered
    /// "no chat matching" while the person sat in Contacts and their chat sat
    /// in the table. Resolving the person first reads the handle table, which
    /// has no window — whose conversation a name means cannot depend on what
    /// else has been noisy lately.
    #[test]
    fn a_person_is_reached_past_the_resolvers_scan_window() {
        let db = fixture();
        let theirs = one_to_one(&db, 4, "+16175550147");
        message_in(&db, theirs, 10, 5);
        // Enough newer activity that the old entry scan never saw their chat.
        many_chats(&db, NAME_SEARCH_SCAN + 200);
        db.execute_batch("BEGIN").unwrap();
        for i in 0..NAME_SEARCH_SCAN + 200 {
            message_in(&db, 100 + i, 10_000 + i, 10 + i);
        }
        db.execute_batch("COMMIT").unwrap();
        let contacts = ContactIndex::for_test([("+16175550147", "source:7", "Robin Adeyemi")]);

        let threads = resolve_conversation(&db, "Robin", &contacts, false).unwrap();
        let rowids: Vec<i64> = threads.iter().map(|chat| chat.rowid).collect();
        assert_eq!(rowids, [theirs], "found past the entry scan");
    }

    /// An address typed in full names the person, and leads with its thread.
    ///
    /// Naming any one address reaches the whole conversation — the contact is
    /// what was named, so reading must not stop at the one thread that
    /// address carries. But the leading thread is where a send goes, and a
    /// send addressed to a number must not drift to whichever thread spoke
    /// last, so the typed address's own thread leads even when it is the
    /// quiet one.
    #[test]
    fn an_address_reaches_every_thread_and_leads_with_its_own() {
        let db = fixture();
        let phone = one_to_one(&db, 4, "+16175550147");
        let email = one_to_one(&db, 5, "robin@example.com");
        message_in(&db, phone, 10, 5);
        message_in(&db, email, 11, 90);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("robin@example.com", "source:7", "Robin Adeyemi"),
        ]);

        let by_phone = resolve_conversation(&db, "+16175550147", &contacts, false).unwrap();
        let rowids: Vec<i64> = by_phone.iter().map(|chat| chat.rowid).collect();
        assert_eq!(rowids, [phone, email], "the typed address leads");

        let by_email = resolve_conversation(&db, "robin@example.com", &contacts, false).unwrap();
        let rowids: Vec<i64> = by_email.iter().map(|chat| chat.rowid).collect();
        assert_eq!(rowids, [email, phone], "either address, the whole answer");
    }

    /// A first name two contacts share is an error, not a pick.
    ///
    /// Nothing narrows it — neither record is named exactly what was typed —
    /// so answering with either would be answering for the wrong person half
    /// the time. The error says who it could have been.
    #[test]
    fn a_first_name_shared_by_two_contacts_errors() {
        let db = fixture();
        let older = one_to_one(&db, 4, "+16175550147");
        let newer = one_to_one(&db, 5, "+16175550148");
        message_in(&db, older, 10, 5);
        message_in(&db, newer, 11, 90);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("+16175550148", "source:9", "Robin Chen"),
        ]);

        let error = resolve_chat(&db, "robin", &contacts)
            .unwrap_err()
            .to_string();
        assert!(error.contains("2 people match"), "{error}");
    }

    /// An unknown number is its own person: the handle stands in for the
    /// contact it does not have, so two service threads with it still merge.
    #[test]
    fn an_unknown_number_merges_its_own_threads() {
        let db = fixture();
        db.execute(
            "INSERT INTO handle (rowid, id) VALUES (6, '+19995551212')",
            [],
        )
        .unwrap();
        for (chat, minutes) in [(30, 5_i64), (31, 90)] {
            db.execute(
                "INSERT INTO chat (rowid, guid, chat_identifier, display_name, is_filtered)
                 VALUES (?, ?, '+19995551212', '', 0)",
                rusqlite::params![chat, format!("chat{chat}")],
            )
            .unwrap();
            db.execute(
                "INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (?, 6)",
                [chat],
            )
            .unwrap();
            message_in(&db, chat, 40 + chat, minutes);
        }

        let threads =
            resolve_conversation(&db, "+19995551212", &ContactIndex::empty(), false).unwrap();
        let rowids: Vec<i64> = threads.iter().map(|chat| chat.rowid).collect();
        assert_eq!(rowids, [31, 30], "one number, one conversation");
    }

    /// A negative limit is nothing, not everything.
    ///
    /// `LIMIT ?` reads a negative as no limit at all, and the merge would then
    /// decode every message of every thread the person has. The CLI cannot
    /// produce one, so this is about the daemon's own request shape.
    #[test]
    fn a_negative_limit_does_not_fetch_the_whole_conversation() {
        let db = fixture();
        let phone = one_to_one(&db, 4, "+16175550147");
        let email = one_to_one(&db, 5, "robin@example.com");
        for rowid in [10, 11, 12] {
            message_in(&db, phone, rowid, rowid);
        }
        for rowid in [13, 14, 15] {
            message_in(&db, email, rowid, rowid);
        }
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("robin@example.com", "source:7", "Robin Adeyemi"),
        ]);

        let threads = resolve_conversation(&db, "Robin", &contacts, false).unwrap();
        let messages = fetch_conversation(&db, &threads, None, -1, false, &contacts).unwrap();
        assert!(messages.is_empty(), "{} returned", messages.len());
    }

    /// The limit is taken in the order each thread was fetched in.
    ///
    /// `fetch_messages` takes the newest `limit` by date, so the merge has to
    /// trim by date. Trimming by rowid instead answers with messages the
    /// threads never offered and drops ones they did, whenever a sender's clock
    /// disagrees with arrival.
    #[test]
    fn the_limit_agrees_with_the_order_each_thread_was_fetched_in() {
        let db = fixture();
        let phone = one_to_one(&db, 4, "+16175550147");
        let email = one_to_one(&db, 5, "robin@example.com");
        // A late arrival: the highest rowid anywhere and the oldest date.
        message_in(&db, phone, 30, 1);
        message_in(&db, phone, 10, 80);
        message_in(&db, phone, 11, 81);
        message_in(&db, email, 20, 40);
        message_in(&db, email, 21, 41);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("robin@example.com", "source:7", "Robin Adeyemi"),
        ]);

        let threads = resolve_conversation(&db, "Robin", &contacts, false).unwrap();
        let messages = fetch_conversation(&db, &threads, None, 2, false, &contacts).unwrap();
        let ids: Vec<i64> = messages.iter().map(|message| message.rowid).collect();
        // The two newest by date, which is what a single-thread read means by
        // `-n 2`. Rowid 30 is newest by arrival and its own thread did not
        // return it, so it was never a candidate.
        assert_eq!(ids, [10, 11]);
    }

    /// One rowid in two of the merged threads is one message.
    ///
    /// `MESSAGE_FROM` joins `chat_message_join`, so such a message comes back
    /// from both fetches — the same shape `attachments_for` and the reply lookup
    /// already allow for, reaching the read path for the first time here because
    /// `chat_id = ?` could only ever match one join row.
    #[test]
    fn a_message_in_both_threads_appears_once() {
        let db = fixture();
        let phone = one_to_one(&db, 4, "+16175550147");
        let email = one_to_one(&db, 5, "robin@example.com");
        message_in(&db, phone, 10, 5);
        message_in(&db, email, 11, 90);
        db.execute(
            "INSERT INTO chat_message_join (chat_id, message_id, message_date)
             VALUES (?, 10, ?)",
            rusqlite::params![email, at(5)],
        )
        .unwrap();
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("robin@example.com", "source:7", "Robin Adeyemi"),
        ]);

        let threads = resolve_conversation(&db, "Robin", &contacts, false).unwrap();
        let messages = fetch_conversation(&db, &threads, None, 50, false, &contacts).unwrap();
        let ids: Vec<i64> = messages.iter().map(|message| message.rowid).collect();
        assert_eq!(ids, [10, 11]);
    }

    /// The listing takes the same rule, so its count agrees with what reading
    /// a name resolves to.
    ///
    /// A name matches from a word start and an address matches anywhere: `ana`
    /// must not list Dana Reyes, while a fragment of a phone number has to keep
    /// reaching the number it is part of, which is how anyone types one.
    #[test]
    fn the_listing_matches_a_name_from_a_word_start_and_an_address_anywhere() {
        let db = fixture();
        let ana = one_to_one(&db, 4, "+16175550147");
        let dana = one_to_one(&db, 5, "+16175550148");
        message_in(&db, ana, 10, 5);
        message_in(&db, dana, 11, 6);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:1", "Ana Duarte"),
            ("+16175550148", "source:2", "Dana Reyes"),
        ]);

        let listed = |spec: &str| -> Vec<i64> {
            fetch_chats(&db, Some(spec), 30, &contacts, false)
                .unwrap()
                .iter()
                .map(|chat| chat.rowid)
                .collect()
        };
        assert_eq!(listed("ana"), [ana], "not the one it is spelled inside");
        assert_eq!(listed("dana"), [dana]);
        // The middle of an address still matches, which the name rule must not
        // take away.
        assert_eq!(listed("5550148"), [dana], "a fragment of a number");
        assert_eq!(listed("617555"), [dana, ana], "a fragment of both");
    }

    /// A room the person is in does not compete with the person.
    ///
    /// This reverses what the collapse used to do, deliberately
    /// (naming-a-conversation.md §3). Reaching someone's own conversation and a
    /// group they are in used to be reported as a question about which was
    /// meant; it is not one, because being in a room is not being the person.
    /// The old behaviour made a first name almost unusable, since a first name
    /// reaches every room its owner is in.
    #[test]
    fn a_room_the_person_is_in_does_not_beat_the_person() {
        let db = fixture();
        let alone = one_to_one(&db, 4, "+16175550147");
        message_in(&db, alone, 10, 5);
        also_in_an_unnamed_group(&db, 4);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("+16175550148", "source:8", "Kit Alvarez"),
        ]);

        // The fragment reaches his one-to-one and the group he is in, and the
        // one-to-one is the answer — by surname, by first name, and by the
        // whole of it.
        for spec in ["adeyemi", "robin", "Robin Adeyemi"] {
            let chat = resolve_chat(&db, spec, &contacts).unwrap();
            assert_eq!(chat.rowid, alone, "resolving {spec}");
        }
    }

    /// Two names reach the room with exactly those two people in it.
    #[test]
    fn several_names_reach_the_room_with_exactly_those_people() {
        let db = fixture();
        one_to_one(&db, 4, "+16175550147");
        one_to_one(&db, 5, "+16175550148");
        one_to_one(&db, 6, "+16175550149");
        // A room of the first two, and a bigger one holding all three.
        room(&db, 20, &[4, 5]);
        room(&db, 21, &[4, 5, 6]);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:1", "Ana Duarte"),
            ("+16175550148", "source:2", "Kit Alvarez"),
            ("+16175550149", "source:3", "Sam Oyelaran"),
        ]);

        let pair = resolve_conversations(
            &db,
            &["ana".to_string(), "kit".to_string()],
            &contacts,
            false,
        )
        .unwrap();
        assert_eq!(pair.len(), 1, "a room never merges");
        assert_eq!(pair[0].rowid, 20);

        // Order is a way of naming the same set, not a different question.
        let other_way = resolve_conversations(
            &db,
            &["kit".to_string(), "ana".to_string()],
            &contacts,
            false,
        )
        .unwrap();
        assert_eq!(other_way[0].rowid, 20);

        // And all three reach the bigger one.
        let trio = resolve_conversations(
            &db,
            &["ana".to_string(), "kit".to_string(), "sam".to_string()],
            &contacts,
            false,
        )
        .unwrap();
        assert_eq!(trio[0].rowid, 21);
    }

    /// Exactly those people, with no fallback to a room that merely holds them.
    ///
    /// The decision in `naming-a-conversation.md §4`: answering with a
    /// conversation containing somebody nobody named is worse than saying there
    /// is no such conversation, because it is silent when it is wrong.
    #[test]
    fn a_room_that_merely_contains_them_is_not_the_room() {
        let db = fixture();
        one_to_one(&db, 4, "+16175550147");
        one_to_one(&db, 5, "+16175550148");
        one_to_one(&db, 6, "+16175550149");
        // Only the room of all three exists; the pair has no room of its own.
        room(&db, 21, &[4, 5, 6]);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:1", "Ana Duarte"),
            ("+16175550148", "source:2", "Kit Alvarez"),
            ("+16175550149", "source:3", "Sam Oyelaran"),
        ]);

        let error = resolve_conversations(
            &db,
            &["ana".to_string(), "kit".to_string()],
            &contacts,
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("no conversation with exactly"), "{error}");
        assert!(error.contains("Ana Duarte"), "it says who: {error}");
    }

    /// One person reachable two ways is one member of a room, not two.
    #[test]
    fn a_room_counts_people_rather_than_addresses() {
        let db = fixture();
        one_to_one(&db, 4, "+16175550147");
        one_to_one(&db, 5, "robin@example.com");
        one_to_one(&db, 6, "+16175550149");
        // Robin is in this room under both of his addresses.
        room(&db, 20, &[4, 5, 6]);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("robin@example.com", "source:7", "Robin Adeyemi"),
            ("+16175550149", "source:3", "Sam Oyelaran"),
        ]);

        let found = resolve_conversations(
            &db,
            &["robin".to_string(), "sam".to_string()],
            &contacts,
            false,
        )
        .unwrap();
        assert_eq!(found[0].rowid, 20, "two addresses, one member");
    }

    /// A spec naming nobody stops the command rather than guessing.
    #[test]
    fn a_name_that_resolves_to_nobody_is_an_error_not_a_fallback() {
        let db = fixture();
        one_to_one(&db, 4, "+16175550147");
        one_to_one(&db, 5, "+16175550148");
        room(&db, 20, &[4, 5]);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:1", "Ana Duarte"),
            ("+16175550148", "source:2", "Kit Alvarez"),
        ]);

        let error = resolve_conversations(
            &db,
            &["ana".to_string(), "nobody-by-that-name".to_string()],
            &contacts,
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("no one matching"), "{error}");

        // And naming one person twice is not a room of one.
        let twice = resolve_conversations(
            &db,
            &["ana".to_string(), "duarte".to_string()],
            &contacts,
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(twice.contains("same person named twice"), "{twice}");
    }

    /// Duplicate members are counted by identity, in both directions.
    ///
    /// Rendered names get this wrong twice over: two records can carry one
    /// name and are two people, and one person named once by address and once
    /// by name renders as two different strings. The second is the dangerous
    /// one — it leaves a single identity wanted, so a two-argument question
    /// would quietly be answered with a one-to-one.
    #[test]
    fn duplicate_members_are_counted_by_identity_not_by_name() {
        let db = fixture();
        one_to_one(&db, 4, "+16175550147");
        one_to_one(&db, 5, "+16175550148");
        room(&db, 20, &[4, 5]);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:1", "Ana Duarte"),
            // A second record that happens to carry the same name. Two people.
            ("+16175550148", "source:2", "Ana Duarte"),
        ]);

        // Two people who share a name are two people, so their room resolves.
        let found = resolve_conversations(
            &db,
            &["+16175550147".to_string(), "+16175550148".to_string()],
            &contacts,
            false,
        )
        .unwrap();
        assert_eq!(found[0].rowid, 20, "same name, different records");
    }

    /// One person named two ways is not a room.
    ///
    /// Nothing else asserts this, so it is worth pinning: naming somebody by
    /// their number and again by their name describes a room of one, and a room
    /// of one is a one-to-one. It does not distinguish counting identities from
    /// counting names — `Person.name` renders the resolved record rather than
    /// the spec, so both specs give one name and either rule refuses this. The
    /// test that separates them is the one above.
    #[test]
    fn one_person_named_two_ways_is_not_a_room() {
        let db = fixture();
        let alone = one_to_one(&db, 4, "+16175550147");
        one_to_one(&db, 5, "robin@example.com");
        message_in(&db, alone, 10, 5);
        // One record, two addresses.
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("robin@example.com", "source:7", "Robin Adeyemi"),
        ]);

        for pair in [
            ["+16175550147", "adeyemi"],
            // Both of his addresses, which is the same person twice over.
            ["+16175550147", "robin@example.com"],
        ] {
            let error = resolve_conversations(
                &db,
                &[pair[0].to_string(), pair[1].to_string()],
                &contacts,
                false,
            )
            .unwrap_err()
            .to_string();
            assert!(
                error.contains("same person named twice"),
                "naming {pair:?}: {error}"
            );
        }
    }

    /// A conversation is looked up by its id, not taken from a listing.
    ///
    /// The scan this replaced was bounded by recent activity, so a quiet
    /// conversation past the window could not be reached by its own rowid or by
    /// its membership — and answered with the error for "no such conversation"
    /// while the row sat there. The window is far larger than any fixture, so
    /// this pins the mechanism rather than the scale: a room with no messages
    /// at all sorts last by activity and is still found.
    #[test]
    fn a_conversation_with_no_activity_is_still_found_by_id() {
        let db = fixture();
        one_to_one(&db, 4, "+16175550147");
        one_to_one(&db, 5, "+16175550148");
        room(&db, 20, &[4, 5]);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:1", "Ana Duarte"),
            ("+16175550148", "source:2", "Kit Alvarez"),
        ]);

        // The room has never carried a message, so `lastDate` is NULL.
        let found = resolve_conversations(
            &db,
            &["ana".to_string(), "kit".to_string()],
            &contacts,
            false,
        )
        .unwrap();
        assert_eq!(found[0].rowid, 20);

        // And by its rowid, which takes the same lookup.
        let by_id = resolve_chat(&db, "20", &contacts).unwrap();
        assert_eq!(by_id.rowid, 20);
    }

    /// A room named exactly after a person is a real question, not a preference.
    ///
    /// §3's rule is that rooms someone is *in* do not compete with them. A room
    /// whose own name is the string typed did not match by membership — it
    /// matched by the label somebody chose for it — so it is as definite a
    /// claim as the person's, and answering silently with either would be
    /// wrong. Without this the person always won and §4's promise that a named
    /// room is reachable by its name quietly stopped holding.
    #[test]
    fn a_room_named_after_a_person_keeps_the_ambiguity() {
        let db = fixture();
        let alone = one_to_one(&db, 4, "+16175550147");
        message_in(&db, alone, 10, 5);
        db.execute(
            "UPDATE chat SET display_name = 'Robin Adeyemi' WHERE rowid = 2",
            [],
        )
        .unwrap();
        let contacts = ContactIndex::for_test([("+16175550147", "source:7", "Robin Adeyemi")]);

        let error = resolve_chat(&db, "Robin Adeyemi", &contacts)
            .unwrap_err()
            .to_string();
        assert!(error.contains("2 chats match"), "{error}");

        // A fragment is not a claim on the room's name, so the person still
        // wins — which is the whole point of §3.
        let chat = resolve_chat(&db, "robin", &contacts).unwrap();
        assert_eq!(chat.rowid, alone);
    }

    /// A room named outright is still reached when the name is also two
    /// people's.
    ///
    /// The ambiguity between the people is real, but the room's claim is the
    /// stronger one — its label is the whole string, theirs is a fragment —
    /// and answering "2 people match" as if no room existed would take away
    /// the one unambiguous meaning along with the two ambiguous ones (§4).
    #[test]
    fn a_room_named_outright_beats_an_ambiguity_between_people() {
        let db = fixture();
        let one = one_to_one(&db, 4, "+16175550147");
        let two = one_to_one(&db, 5, "+16175550148");
        message_in(&db, one, 10, 5);
        message_in(&db, two, 11, 6);
        db.execute("UPDATE chat SET display_name = 'Robin' WHERE rowid = 2", [])
            .unwrap();
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("+16175550148", "source:9", "Robin Chen"),
        ]);

        let chat = resolve_chat(&db, "Robin", &contacts).unwrap();
        assert_eq!((chat.rowid, chat.is_group), (2, true), "the label wins");

        // Take the label away and the same spec is the ambiguity it should
        // be — the room was the reason it was not.
        db.execute(
            "UPDATE chat SET display_name = 'Ship Room' WHERE rowid = 2",
            [],
        )
        .unwrap();
        let error = resolve_chat(&db, "Robin", &contacts)
            .unwrap_err()
            .to_string();
        assert!(error.contains("2 people match"), "{error}");
    }

    /// The room is still reachable, by naming the room rather than a member.
    ///
    /// §3 decides which conversation a *person's* name means. It says nothing
    /// about a group's own name, which still resolves to the group.
    #[test]
    fn a_named_room_still_resolves_to_the_room() {
        let db = fixture();
        let contacts = ContactIndex::empty();
        let chat = resolve_chat(&db, "Ship Room", &contacts).unwrap();
        assert_eq!(chat.rowid, 2);
        assert!(chat.is_group);
    }

    /// A person with no conversation of their own falls through to the rooms.
    ///
    /// There is nothing to prefer, so inventing a conversation that does not
    /// exist would be worse than listing what does.
    #[test]
    fn somebody_you_only_share_a_room_with_still_finds_the_room() {
        let db = fixture();
        // The handle exists and has no conversation of its own, which is the
        // whole point — `one_to_one` would give it one.
        db.execute(
            "INSERT INTO handle (rowid, id) VALUES (4, '+16175550147')",
            [],
        )
        .unwrap();
        also_in_an_unnamed_group(&db, 4);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("+16175550148", "source:8", "Kit Alvarez"),
        ]);

        let chat = resolve_chat(&db, "adeyemi", &contacts).unwrap();
        assert!(chat.is_group, "{chat:?}");
    }

    /// The same shape, and the opposite answer, because these are two people.
    ///
    /// Collapsing by the rendered name rather than by the record would answer
    /// with a stranger's conversation, which is the one outcome worse than
    /// reporting the ambiguity.
    #[test]
    fn two_people_sharing_a_name_stay_ambiguous() {
        let db = fixture();
        let older = one_to_one(&db, 4, "+16175550147");
        let newer = one_to_one(&db, 5, "robin@example.com");
        message_in(&db, older, 10, 5);
        message_in(&db, newer, 11, 90);

        // Two records that happen to agree on a name: an old entry and a new
        // one, a father and a son.
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("robin@example.com", "source:9", "Robin Adeyemi"),
        ]);

        // Said as people, with the address that tells them apart — a list of
        // chats would print one name twice and explain nothing.
        let error = resolve_chat(&db, "Robin Adeyemi", &contacts)
            .unwrap_err()
            .to_string();
        assert!(error.contains("2 people match"), "{error}");
        assert!(error.contains("+16175550147"), "{error}");
    }

    /// An error that shows six of fifty has to say so, or the list reads as the
    /// whole answer and the count reads as a mistake.
    #[test]
    fn the_ambiguity_says_how_many_it_is_not_showing() {
        let db = fixture();
        // Ten conversations that all match, none of them exactly.
        for n in 0..10 {
            let chat = one_to_one(&db, 10 + n, &format!("+161755501{:02}", 40 + n));
            message_in(&db, chat, 100 + n, 5 + n);
        }
        let error = resolve_chat(&db, "+16175550", &ContactIndex::empty())
            .unwrap_err()
            .to_string();

        assert!(error.contains("10 chats match"), "{error}");
        assert!(error.contains("and 4 more"), "{error}");
        // Not a cap, so it must not claim to be one.
        assert!(!error.contains("at least"), "{error}");
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

    /// A first name reaches the person it names, not everyone it is spelled
    /// inside.
    ///
    /// The case that prompted `naming-a-conversation.md`: on a real database a
    /// four-letter first name resolved to three people, two of them strangers
    /// whose surnames happen to contain it. `answers_to` was a plain substring
    /// test, so being inside a name counted as being the name.
    #[test]
    fn a_first_name_does_not_resolve_to_a_stranger_who_spells_it_inside_theirs() {
        let db = fixture();
        let ana = one_to_one(&db, 4, "+16175550147");
        one_to_one(&db, 5, "+16175550148");
        one_to_one(&db, 6, "+16175550149");
        message_in(&db, ana, 10, 5);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:1", "Ana Duarte"),
            // Sus-ana and D-ana: the needle sits inside the name rather than
            // starting a word in it, the same shape as the real case.
            ("+16175550148", "source:2", "Susana Vidal"),
            ("+16175550149", "source:3", "Dana Reyes"),
        ]);

        let person = resolve_person(&db, "ana", &contacts).unwrap();
        assert_eq!(person.name, "Ana Duarte");

        // Their own names still reach them, from any word.
        for (spec, expected) in [
            ("susana", "Susana Vidal"),
            ("vidal", "Susana Vidal"),
            ("dana", "Dana Reyes"),
            ("reyes", "Dana Reyes"),
            ("duarte", "Ana Duarte"),
        ] {
            let found = resolve_person(&db, spec, &contacts).unwrap();
            assert_eq!(found.name, expected, "resolving {spec}");
        }
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

    /// A send confirmation has to name the address, because the name alone
    /// cannot tell one person's two conversations apart — and telling them
    /// apart is the entire job of the dry run this repository requires.
    #[test]
    fn a_send_target_is_named_by_its_address() {
        let db = fixture();
        let older = one_to_one(&db, 4, "+16175550147");
        let newer = one_to_one(&db, 5, "robin@example.com");
        message_in(&db, older, 10, 5);
        message_in(&db, newer, 11, 90);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("robin@example.com", "source:7", "Robin Adeyemi"),
        ]);

        // The two render identically by name, which is the whole problem.
        let picked = resolve_chat(&db, "adeyemi", &contacts).unwrap();
        let other = resolve_chat(&db, "+16175550147", &contacts).unwrap();
        assert_eq!(picked.name, other.name);
        assert_ne!(
            describe_target(&picked),
            describe_target(&other),
            "a confirmation that cannot separate these is not a confirmation"
        );
        assert_eq!(
            describe_target(&picked),
            "Robin Adeyemi (robin@example.com)"
        );

        // A room is named by its name; `chat9` would be noise, not a fact.
        let group = resolve_chat(&db, "Ship Room", &contacts).unwrap();
        assert_eq!(describe_target(&group), "Ship Room");

        // And an unknown handle, which the name already is, is not said twice.
        let bare = one_to_one(&db, 6, "+19995551212");
        message_in(&db, bare, 12, 3);
        let unknown = resolve_chat(&db, "+19995551212", &contacts).unwrap();
        assert_eq!(describe_target(&unknown), "+19995551212");
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

    /// A person on two threads, for the listing tests below. The email thread is
    /// the most recently active, so it is the one a send would continue.
    fn split_across_two_threads(db: &Connection) -> (i64, i64, ContactIndex) {
        let phone = one_to_one(db, 4, "+16175550147");
        let email = one_to_one(db, 5, "robin@example.com");
        message_in(db, phone, 10, 5);
        message_in(db, phone, 11, 6);
        message_in(db, email, 12, 90);
        let contacts = ContactIndex::for_test([
            ("+16175550147", "source:7", "Robin Adeyemi"),
            ("robin@example.com", "source:7", "Robin Adeyemi"),
        ]);
        (phone, email, contacts)
    }

    /// The listing and `chat` have to agree about how many conversations there
    /// are, which is the whole of `conversation-merging.md §5`.
    #[test]
    fn a_persons_threads_are_one_row_in_the_listing() {
        let db = fixture();
        let (_, email, contacts) = split_across_two_threads(&db);

        let chats = fetch_conversations(&db, None, 30, &contacts, false).unwrap();
        let theirs: Vec<&Chat> = chats
            .iter()
            .filter(|chat| chat.name == "Robin Adeyemi")
            .collect();
        assert_eq!(theirs.len(), 1, "two threads, one conversation");
        assert_eq!(theirs[0].rowid, email, "the rowid a send would go to");
        assert_eq!(theirs[0].message_count, 3, "counted across both threads");
    }

    /// The limit counts conversations, so it cannot be given to SQL any more.
    ///
    /// Both of this person's threads are newer than anything else in the
    /// fixture, so a `LIMIT 2` applied before the merge fetches exactly those
    /// two and collapses them to one — asking for two conversations and getting
    /// one, which is the same off-by-a-merge the listing exists to remove.
    #[test]
    fn the_limit_counts_conversations_rather_than_threads() {
        let db = fixture();
        let (_, _, contacts) = split_across_two_threads(&db);

        let chats = fetch_conversations(&db, None, 2, &contacts, false).unwrap();
        assert_eq!(chats.len(), 2, "two conversations were asked for");
    }

    /// Merging must not hide an address behind the newest one.
    ///
    /// Their phone thread is the older of the two, so the merged row is built
    /// from the email thread. Searching the phone number still has to reach
    /// them: before merging it matched a row of its own, and a rename of the
    /// conversation is no reason for an address to stop being findable.
    #[test]
    fn a_search_reaches_an_address_the_newest_thread_does_not_use() {
        let db = fixture();
        let (_, email, contacts) = split_across_two_threads(&db);

        let chats = fetch_conversations(&db, Some("6175550147"), 30, &contacts, false).unwrap();
        let rowids: Vec<i64> = chats.iter().map(|chat| chat.rowid).collect();
        assert_eq!(
            rowids,
            [email],
            "found by an address the row does not lead with"
        );
    }

    /// Two rooms with the same membership are two rooms.
    #[test]
    fn groups_are_never_merged() {
        let db = fixture();
        db.execute(
            "INSERT INTO chat (rowid, guid, chat_identifier, display_name, is_filtered)
             VALUES (9, 'iMessage;+;chat10', 'chat10', '', 0)",
            [],
        )
        .unwrap();
        // The same two people as chat 2, which is the case a membership-based
        // merge would collapse and this one must not.
        db.execute(
            "INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (9, 1), (9, 2)",
            [],
        )
        .unwrap();
        message_in(&db, 9, 10, 7);

        let chats = fetch_conversations(&db, None, 30, &ContactIndex::empty(), false).unwrap();
        let rooms = chats.iter().filter(|chat| chat.is_group).count();
        assert_eq!(rooms, 2, "same membership, still two rooms");
    }

    /// Insert `n` one-to-one chats, each with its own address and no messages.
    fn many_chats(db: &Connection, n: i64) {
        db.execute_batch("BEGIN").unwrap();
        for i in 100..(100 + n) {
            db.execute(
                "INSERT INTO handle (rowid, id) VALUES (?, ?)",
                rusqlite::params![i, format!("+1310555{i}")],
            )
            .unwrap();
            db.execute(
                "INSERT INTO chat (rowid, guid, chat_identifier, display_name, is_filtered)
                 VALUES (?, ?, ?, '', 0)",
                rusqlite::params![i, format!("iMessage;-;p{i}"), format!("+1310555{i}")],
            )
            .unwrap();
            db.execute(
                "INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (?, ?)",
                rusqlite::params![i, i],
            )
            .unwrap();
        }
        db.execute_batch("COMMIT").unwrap();
    }

    /// The listing has no scan budget, because a budget is a cap.
    ///
    /// It cannot push the limit into SQL — merging means a count of threads is
    /// not a count of conversations — so the tempting fix is a generous budget
    /// instead. That is worse than the bug it replaces: with one of 5,000, a
    /// caller asking for 100,000 conversations got 4,998 and nothing said why.
    #[test]
    fn the_listing_has_no_upper_bound_of_its_own() {
        let db = fixture();
        let n = NAME_SEARCH_SCAN + 200;
        many_chats(&db, n);

        let all = fetch_conversations(&db, None, 100_000, &ContactIndex::empty(), true).unwrap();
        assert!(
            i64::try_from(all.len()).unwrap() > NAME_SEARCH_SCAN,
            "{} conversations came back, capped at the old budget",
            all.len()
        );
    }

    /// And a query still reaches past where that budget used to stop.
    ///
    /// With names off there is no contact index, so this is the path that used
    /// to hand the query to SQL. It is matched in Rust now, after the merge, and
    /// the chat it has to find is the oldest one there is.
    #[test]
    fn a_search_reaches_the_oldest_conversation() {
        let db = fixture();
        many_chats(&db, NAME_SEARCH_SCAN + 200);
        // Every chat above has no messages, so they sort last as a block; this
        // one is given the address that is searched for.
        db.execute(
            "UPDATE chat SET chat_identifier = '+13105559999' WHERE rowid = 5299",
            [],
        )
        .unwrap();

        let found =
            fetch_conversations(&db, Some("5559999"), 30, &ContactIndex::empty(), true).unwrap();
        let rowids: Vec<i64> = found.iter().map(|chat| chat.rowid).collect();
        assert_eq!(rowids, [5299], "found past where the scan used to stop");
    }

    /// `--no-names` matches a room's name by the same rule everything else does.
    ///
    /// It did not before this: with no contact index there was nothing to match
    /// in Rust, so the query went to SQL as `displayName LIKE '%q%'` and found
    /// the middle of a word. The rule is now the one
    /// `naming-a-conversation.md §2` decided, on every path — which took
    /// deleting the SQL branch rather than routing around it. Moving the
    /// listing off it first only moved the disagreement: the listing stopped
    /// finding `hip` and the resolver went on finding it, so the index and the
    /// thing it indexes contradicted each other, which is §5's own defect.
    /// `the_listing_and_the_resolver_match_alike_without_contacts` is what pins
    /// the two together; this pins which rule they landed on.
    ///
    /// An address is unchanged and still matches anywhere, because a fragment of
    /// a phone number is how anyone types part of one.
    #[test]
    fn a_room_is_matched_from_a_word_start_with_names_off() {
        let db = fixture();
        let empty = ContactIndex::empty();
        let found = |spec: &str| {
            fetch_conversations(&db, Some(spec), 30, &empty, true)
                .unwrap()
                .iter()
                .any(|chat| chat.rowid == 2)
        };
        assert!(found("Ship"), "the start of the name");
        assert!(found("room"), "the start of its second word");
        assert!(!found("hip"), "inside a word, which SQL LIKE used to find");
        // The same conversation by an address, which is a substring match.
        assert!(found("5551234"), "the middle of a member's number");
    }

    /// The index and the thing it indexes agree about what a query matches.
    ///
    /// With no contact index the query used to go to SQL as a `LIKE`, so the
    /// listing and the resolver ran different rules: `oom` listed nothing and
    /// opened Ship Room. That is §5's defect moved rather than removed, and it
    /// arrives without a flag on any machine where Contacts cannot be read.
    #[test]
    fn the_listing_and_the_resolver_match_alike_without_contacts() {
        let db = fixture();
        let empty = ContactIndex::empty();
        // Each of these names at most one conversation. A spec matching several
        // is not a disagreement — the listing shows them all and the resolver
        // reports the ambiguity, which is what both are supposed to do.
        for spec in ["Ship", "room", "oom", "hip", "someone@example"] {
            let listed = fetch_conversations(&db, Some(spec), 30, &empty, true)
                .unwrap()
                .iter()
                .any(|chat| chat.rowid == 2);
            let resolved = resolve_chat(&db, spec, &empty).is_ok_and(|chat| chat.rowid == 2);
            assert_eq!(
                listed, resolved,
                "`{spec}` reaches chat 2 down one path only"
            );
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

    /// The two new fields are invisible until context is asked for.
    ///
    /// That is the whole of what keeps this change from reaching consumers who
    /// never wanted it: a hit is a hit by default, so `matched` is omitted, and
    /// `group` exists only once there are runs to belong to.
    #[test]
    fn context_fields_stay_out_of_the_json_until_they_mean_something() {
        let db = fixture();
        talkative(&db);

        let bare = serde_json::to_value(found(&db, Context::default())).unwrap();
        assert!(bare[0].get("matched").is_none(), "{bare}");
        assert!(bare[0].get("group").is_none(), "{bare}");

        let with = serde_json::to_value(found(
            &db,
            Context {
                before: 1,
                after: 0,
            },
        ))
        .unwrap();
        // Context says so; the hit still says nothing, exactly as before.
        assert_eq!(with[0]["matched"], serde_json::json!(false), "{with}");
        assert_eq!(with[0]["group"], serde_json::json!(0), "{with}");
        assert!(with[1].get("matched").is_none(), "{with}");
        assert_eq!(with[1]["group"], serde_json::json!(0), "{with}");
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
