//! The wire between `msg` and `msgd`: newline-delimited JSON over a unix socket.
//!
//! One request per connection. The daemon answers with `result` and closes, or
//! with a stream of `item` frames that ends when the client disconnects.
//!
//! No request carries a filesystem path. That is the rule the daemon's whole
//! security value rests on, since it holds Full Disk Access and a path argument
//! would turn it into a general-purpose reader: see
//! docs/projects/all/daemon-and-permissions.md §6.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::db::{Chat, Message};

/// Bump this whenever a request is added, not only when one changes shape.
///
/// A daemon that predates a command does not reject it unless something makes
/// it: the TypeScript switch ran off the end and answered `result` with no
/// value, and the CLI then read a field off `undefined` and threw. The version
/// check is the only thing standing between a stale daemon and a crash, so a new
/// command is a new protocol.
///
/// 2 added `automation`.
///
/// 3 added `with` and `from` to `search`. A new *field* is a new protocol here
/// for the same reason a new command is, and a sharper one: a daemon that does
/// not know the field ignores it and answers with everyone's messages when one
/// person was asked for. A stale daemon crashing is better than a stale daemon
/// quietly answering a question nobody asked.
///
/// 4 added `attachments` to every message — `rowid`, `name`, `mimeType`, `uti`,
/// `totalBytes`, `isSticker`, `isDownloaded` — and put a description of each one
/// in the body where Messages leaves a U+FFFC. These are new *reply* fields, and
/// a stale daemon answering them would be wrong in the mild direction — the body
/// simply reads as it did before. It is still a version bump, because "reinstall
/// the daemon" is a better thing to be told than to wonder why a photo prints as
/// an invisible character in one build and not another.
///
/// `uti` arrived after the rest of 4, in review, and is listed here rather than
/// given a 5 of its own. A version names a protocol somebody can speak, and this
/// branch squash-merges into one commit, so no build outside it ever answered a
/// 4 without `uti`. The cost is narrow and belongs to whoever is working on the
/// branch: between rebuilding the CLI and reinstalling the daemon, an attachment
/// whose only type is a `uti` reads as `[attachment]`. That is the ordinary
/// half-rebuild AGENTS.md already warns about, not a protocol two releases can
/// disagree over.
///
/// 5 added `save`, which streams one attachment's bytes to the client by rowid.
///
/// 6 added `replyTo` to every message: what an inline reply is answering, as the
/// answered message's rowid, sender, and an excerpt. A new *reply* field again,
/// and again wrong only in the mild direction — a stale daemon omits it and a
/// reply reads as it did before.
///
/// This one is not the `uti` case, and the difference is the whole of why that
/// one stayed at 4. That argument was that no build outside the branch had ever
/// answered a 4 without `uti`, because 4 came into being complete in a single
/// squash. 5 is already merged and installed, so builds that speak 5 *without*
/// `replyTo` exist right now. A version names a protocol somebody can speak, and
/// two of them cannot both be 5.
///
/// 7 added `filedAs` to a resolved handle: the name a Contacts record is filed
/// under, when a nickname is being shown in its place. A new *reply* field
/// again, and a stale daemon omitting it is mild — `msg contacts` prints the
/// name somebody goes by and simply cannot add the other one. It is still a
/// bump, for the reason 6 was: builds that speak 6 without it are merged and
/// installed right now, so 6 cannot also name the protocol that has it.
///
/// 8 changed what `contacts` answers without changing the shape it answers in,
/// which is the case this constant is easiest to forget for. A term may now be
/// a name or a nickname; it may resolve to several rows rather than exactly
/// one; and `handle` comes back as the contact's stored address rather than
/// echoing what was asked. Same three fields, different contract.
///
/// It is a bump for the reason 6 was, not the reason `uti` was not: 7 is merged
/// and installed with the old lookup, so builds answering 7 the old way exist
/// right now, and two protocols cannot both be 7.
///
/// The failure it prevents is the quiet one. The gate compares numbers, so a
/// new CLI against a 7 daemon would pass it and print `(unknown)` for a name —
/// which is exactly the symptom this change exists to remove, with nothing
/// pointing at the daemon as the reason. Being told to reinstall is the whole
/// value of the number.
///
/// 9 added `before` and `after` to `search`, and `matched` and `group` to the
/// messages it answers with. A new *request* field, which is the sharper case:
/// a daemon that does not know them ignores them and answers with bare hits, so
/// `-C 3` would silently produce exactly what the flag was asked to change.
///
/// 10 merges a person's conversations in `read`, and adds `merged` to the
/// reply and `unknown` to the request. Sharper still, because the behaviour is
/// the daemon's: an older one answers a name with a single thread and no field
/// saying so, which looks exactly like a person who only has one. The reply is
/// not wrong in a way anything can see — it is simply missing half the
/// conversation, which is the failure this number exists to make loud.
pub const PROTOCOL_VERSION: u32 = 10;

/// The launchd job label, and the bundle identifier the TCC grant lands on.
pub const LABEL: &str = "com.ninjudd.msgd";

/// Owner-only directory holding the socket and the daemon's log.
pub fn state_directory() -> PathBuf {
    match std::env::var("MSG_STATE_DIR") {
        Ok(path) if !path.is_empty() => PathBuf::from(path),
        _ => crate::home().join(".local/state/msg"),
    }
}

pub fn socket_path() -> PathBuf {
    match std::env::var("MSG_SOCKET") {
        Ok(path) if !path.is_empty() => PathBuf::from(path),
        _ => state_directory().join("msgd.sock"),
    }
}

// Every optional field carries `skip_serializing_if`, so an absent one is
// absent from the JSON rather than present and null. That is what
// `JSON.stringify` does with `undefined`, and the difference is load-bearing:
// the daemon reads `names !== false`, so a literal null would mean "false"
// where an omitted field means "true".

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReadRequest {
    pub chat: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tapbacks: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<bool>,
    /// Let a thread Messages files under Unknown Senders join the merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    /// Messages to show before and after each hit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    /// One person, across every conversation: their messages and mine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub with: Option<String>,
    /// One person, across every conversation: only what they sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WatchRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tapbacks: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<bool>,
}

/// Naming one conversation, which is what `send` needs before it can address it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResolveRequest {
    pub chat: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<bool>,
}

/// An attachment, as bytes rather than a path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub base64: String,
}

/// Asking for one attachment's bytes. An id, never a path (§6).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SaveRequest {
    pub id: i64,
}

/// One piece of the answer to `save`.
///
/// Streamed rather than returned whole, because these are not small: on a real
/// database the median attachment is 1.9MB and the largest is 548MB, which as
/// one base64 field would cost the better part of a gigabyte in the daemon and
/// again in the client. `Head` arrives first, then `Chunk` until the file is
/// done, so peak memory is one chunk rather than one file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "part",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SavePart {
    Head {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        total_bytes: i64,
    },
    Chunk {
        base64: String,
    },
}

/// What `save` says when the last chunk has gone.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveReply {
    pub name: String,
    pub bytes: i64,
}

/// Sending, which needs Automation rather than Full Disk Access.
///
/// `chat` may be a guid, in which case the daemon addresses it without reading
/// the database at all — so a daemon granted Automation and refused Full Disk
/// Access can still send. An attachment arrives as bytes rather than a path,
/// because a path argument would make the daemon read arbitrary files (§6).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SendRequest {
    pub chat: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<Attachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContactsRequest {
    pub handles: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Empty {}

/// Every request the daemon answers, tagged by `cmd` the way the wire has it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum Request {
    Chats(ChatsRequest),
    Read(ReadRequest),
    Search(SearchRequest),
    Watch(WatchRequest),
    Resolve(ResolveRequest),
    Send(SendRequest),
    Contacts(ContactsRequest),
    Save(SaveRequest),
    /// Exercise the Automation permission without sending anything, so the
    /// entry in System Settings exists to be switched off.
    Automation(Empty),
    Status(Empty),
}

/// The command names this daemon knows, so an unknown one is reported as such
/// rather than as a parse failure.
pub const COMMANDS: &[&str] = &[
    "chats",
    "read",
    "search",
    "watch",
    "resolve",
    "send",
    "contacts",
    "save",
    "automation",
    "status",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendReply {
    pub guid: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationReply {
    pub allowed: bool,
    pub detail: String,
    /// Whether the config would let a send through, independent of macOS.
    pub config_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusReply {
    pub version: String,
    pub protocol: u32,
    pub pid: u32,
    pub uptime_seconds: u64,
    pub database: String,
    pub message_count: i64,
    pub contact_count: usize,
    /// Why the contact index is empty, when it is. Empty when all is well.
    pub contact_problems: Vec<String>,
    pub watchers: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
// `handle` and `name` are single words and unaffected; this is here so the new
// field goes out as `filedAs`, matching `mimeType` and `totalBytes` elsewhere.
#[serde(rename_all = "camelCase")]
pub struct ResolvedHandle {
    pub handle: String,
    /// What this person is called: the nickname when Contacts holds one.
    pub name: Option<String>,
    /// The name their record is filed under, when a nickname displaced it.
    ///
    /// Its own field rather than parentheses inside `name`, because a field
    /// that sometimes holds one name and sometimes holds two is one a consumer
    /// cannot act on — it would have to parse a format this program invented,
    /// and that parse breaks the first time a filed name legitimately contains
    /// a bracket. The CLI composes the two into a line for a person to read;
    /// nothing on the wire has to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filed_as: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactsReply {
    pub size: usize,
    pub resolved: Vec<ResolvedHandle>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadReply {
    /// The thread a reply would go to, which is the most recently active when a
    /// person has several. Unchanged in meaning from before merging existed, so
    /// a consumer reading it keeps reading what it always read.
    pub chat: Chat,
    pub messages: Vec<Message>,
    /// The other threads folded into this transcript, if any. Skipped when
    /// empty, so an unmerged conversation serializes exactly as it did at
    /// version 9 (conversation-merging.md §6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged: Vec<i64>,
}

impl ReadReply {
    /// Build a reply from the threads a conversation resolved to, most recently
    /// active first. Shared so the two read paths cannot disagree about which
    /// thread is `chat` and which are `merged`.
    pub fn new(threads: Vec<Chat>, messages: Vec<Message>) -> Self {
        let merged = threads.iter().skip(1).map(|chat| chat.rowid).collect();
        let chat = threads
            .into_iter()
            .next()
            .expect("a resolved conversation holds at least one chat");
        Self {
            chat,
            messages,
            merged,
        }
    }
}

/// `access-denied` is the one code the CLI acts on, since it maps to the exit
/// status the README documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    AccessDenied,
    SendDisabled,
    Error,
    Version,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Frame {
    Result { value: serde_json::Value },
    Item { value: serde_json::Value },
    Error { code: ErrorCode, message: String },
}

impl Frame {
    pub fn result<T: Serialize>(value: &T) -> crate::Result<Self> {
        Ok(Self::Result {
            value: serde_json::to_value(value)?,
        })
    }

    pub fn item<T: Serialize>(value: &T) -> crate::Result<Self> {
        Ok(Self::Item {
            value: serde_json::to_value(value)?,
        })
    }

    pub fn from_error(error: &crate::Error) -> Self {
        let (code, message) = match error {
            // The message is kept rather than replaced. It used to be swapped
            // for `DENIED` because the only denial that reached here was about
            // `chat.db`, and the words it carried were aimed at a CLI without a
            // daemon. Now a refused *attachment* can reach here too, and telling
            // that caller the database is unreadable is untrue — the daemon just
            // read it to resolve the rowid — and names a remedy that would not
            // touch a per-file refusal. `open_database` puts `DENIED` in itself.
            crate::Error::AccessDenied(message) => (ErrorCode::AccessDenied, message.clone()),
            crate::Error::SendDisabled(message) => (ErrorCode::SendDisabled, message.clone()),
            crate::Error::Other(message) => (ErrorCode::Error, message.clone()),
        };
        Self::Error { code, message }
    }
}

/// The client holds no grant and cannot be given one, so a denied read is always
/// fixed on the daemon's side. The delay is worth mentioning: the Full Disk
/// Access list took minutes to show a new entry during the spike (§9).
pub const DENIED: &str = "msgd cannot read the Messages database.
Grant Full Disk Access to msgd in System Settings > Privacy & Security > Full Disk Access,
then try again. `msg daemon status` prints the path to add, and a new entry can take a
minute to appear in that list.";

/// One request plus the protocol version the client speaks.
///
/// Written flattened — `{"cmd":"status","v":2}` — because that is the shape the
/// TypeScript client sends and the shape its daemon reads.
pub fn envelope(request: &Request) -> crate::Result<String> {
    let mut value = serde_json::to_value(request)?;
    if let Some(map) = value.as_object_mut() {
        map.insert("v".into(), serde_json::json!(PROTOCOL_VERSION));
    }
    Ok(format!("{value}\n"))
}

pub fn encode(frame: &Frame) -> String {
    format!(
        "{}\n",
        serde_json::to_string(frame).unwrap_or_else(|_| {
            r#"{"type":"error","code":"error","message":"msgd could not encode its answer"}"#
                .to_string()
        })
    )
}

/// Whether a string is a chat guid rather than a name to look up.
///
/// Messages writes them as `<service>;<kind>;<identifier>`, as in
/// `iMessage;-;+13105551234` or `iMessage;+;chat9`. Matched by shape rather than
/// by punctuation: chat names contain semicolons often enough, and treating one
/// as a guid would hand a display name to AppleScript as an address.
pub fn is_chat_guid(value: &str) -> bool {
    let mut parts = value.splitn(3, ';');
    let (Some(service), Some(kind), Some(identifier)) = (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };

    let mut characters = service.chars();
    let starts_alphabetic = characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic());
    if !starts_alphabetic || !characters.all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    if kind != "-" && kind != "+" {
        return false;
    }
    // `.+$` in the original, and `.` does not match a newline in JavaScript. A
    // guid never contains one, and this value reaches AppleScript as an address.
    !identifier.is_empty() && !identifier.contains(['\n', '\r'])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A consumer can read the two names apart, which is the whole reason
    /// `filedAs` is a field instead of parentheses inside `name`.
    ///
    /// A field that sometimes holds one name and sometimes holds two can only
    /// be used by parsing a format this program invented, and that parse breaks
    /// on the first filed name containing a bracket.
    #[test]
    fn a_resolved_handle_keeps_its_two_names_in_two_fields() {
        let paired = serde_json::to_value(ResolvedHandle {
            handle: "+13105551234".into(),
            name: Some("Dee".into()),
            filed_as: Some("Dana Reyes".into()),
        })
        .unwrap();
        assert_eq!(paired["name"], "Dee");
        assert_eq!(paired["filedAs"], "Dana Reyes");

        // Nothing displaced: the key is absent rather than null, so a consumer
        // reading `name` alone sees exactly what it saw before this existed.
        let bare = serde_json::to_value(ResolvedHandle {
            handle: "+14155559876".into(),
            name: Some("Sam Oyelaran".into()),
            filed_as: None,
        })
        .unwrap();
        assert_eq!(bare["name"], "Sam Oyelaran");
        assert!(bare.get("filedAs").is_none(), "{bare}");
    }

    #[test]
    fn recognises_the_guids_messages_writes() {
        for guid in [
            "iMessage;-;+13105551234",
            "iMessage;+;chat9",
            "SMS;-;+18885550000",
            "iMessage;-;someone@example.com",
        ] {
            assert!(is_chat_guid(guid), "{guid}");
        }
    }

    #[test]
    fn does_not_mistake_a_chat_name_that_contains_a_semicolon() {
        // `send` skips resolution for a guid, so a name matched here would be
        // handed to AppleScript as an address and reach nobody.
        for name in [
            "Lunch; also dinner",
            "a;b;c",
            ";-;x",
            "iMessage;-;",
            "iMessage;x;chat9",
            "iMessage;-;\n",
        ] {
            assert!(!is_chat_guid(name), "{name}");
        }
    }

    #[test]
    fn does_not_mistake_an_ordinary_name_or_a_rowid() {
        for name in ["Ship Room", "42", "+13105551234"] {
            assert!(!is_chat_guid(name), "{name}");
        }
    }

    /// The envelope is what the TypeScript daemon parses, so its shape is a
    /// contract rather than an implementation detail (rust-rewrite §4).
    #[test]
    fn writes_the_envelope_flat_with_a_version() {
        let line = envelope(&Request::Status(Empty {})).unwrap();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["cmd"], serde_json::json!("status"));
        assert_eq!(value["v"], serde_json::json!(PROTOCOL_VERSION));
        assert!(line.ends_with('\n'));
    }

    /// `JSON.stringify` drops undefined, so an unset option must be absent
    /// rather than null — a daemon reading `names: null` would take it for
    /// "false" where the TypeScript one saw "unset, so true".
    #[test]
    fn omits_unset_options_rather_than_writing_null() {
        let line = envelope(&Request::Chats(ChatsRequest::default())).unwrap();
        // Built from the constant rather than written out, so a protocol bump
        // does not look like a regression in what this is actually asserting:
        // that every unset field is absent, leaving only `cmd` and `v`.
        assert_eq!(
            line.trim(),
            format!(r#"{{"cmd":"chats","v":{PROTOCOL_VERSION}}}"#)
        );
    }

    #[test]
    fn reads_the_error_codes_the_typescript_end_writes() {
        let frame: Frame =
            serde_json::from_str(r#"{"type":"error","code":"access-denied","message":"no"}"#)
                .unwrap();
        match frame {
            Frame::Error { code, .. } => assert_eq!(code, ErrorCode::AccessDenied),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn writes_the_error_codes_the_typescript_end_reads() {
        let frame = Frame::Error {
            code: ErrorCode::SendDisabled,
            message: "off".into(),
        };
        assert_eq!(
            encode(&frame).trim(),
            r#"{"type":"error","code":"send-disabled","message":"off"}"#
        );
    }

    #[test]
    fn round_trips_every_request_through_its_wire_form() {
        let requests = [
            Request::Chats(ChatsRequest::default()),
            Request::Read(ReadRequest {
                chat: "1".into(),
                ..Default::default()
            }),
            Request::Search(SearchRequest {
                query: "x".into(),
                ..Default::default()
            }),
            Request::Watch(WatchRequest::default()),
            Request::Resolve(ResolveRequest {
                chat: "1".into(),
                ..Default::default()
            }),
            Request::Send(SendRequest {
                chat: "1".into(),
                ..Default::default()
            }),
            Request::Contacts(ContactsRequest::default()),
            Request::Automation(Empty {}),
            Request::Status(Empty {}),
        ];
        for request in requests {
            let line = envelope(&request).unwrap();
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            let name = value["cmd"].as_str().unwrap().to_string();
            assert!(
                COMMANDS.contains(&name.as_str()),
                "{name} missing from COMMANDS"
            );
            serde_json::from_value::<Request>(value).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
}
