//! Where the CLI gets its data.
//!
//! The daemon when one is listening, the database directly when it is not. The
//! fallback is deliberate: without it, a machine that has not installed the
//! daemon has no working `msg` at all, and a denied read is the first thing a
//! new user meets.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use base64::Engine;
use rusqlite::Connection;

use crate::apple::since_to_apple_date;
use crate::contacts::{ContactIndex, load_contacts};
use crate::daemon::client::{connect_daemon, connect_daemon_within, request, save, watch};
use crate::daemon::protocol::{
    AutomationReply, ChatsRequest, ContactsReply, ContactsRequest, Empty, ReadReply, ReadRequest,
    Request, ResolveRequest, ResolvedHandle, SavePart, SaveRequest, SearchRequest, SendReply,
    SendRequest, StatusReply, WatchRequest,
};
use crate::daemon::send::attachment_name;
use crate::db::{
    Chat, FetchMessages, Message, PersonFilter, attachment_path, fetch_chats, fetch_conversation,
    fetch_messages, latest_rowid, open_database, person_filter, resolve_chat, resolve_conversation,
    unreadable, with_context,
};
use crate::{Error, Result};

const BASE64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

/// How many new messages one poll of the direct path will report.
const WATCH_BATCH: i64 = 200;

pub struct ChatsQuery {
    pub query: Option<String>,
    pub limit: i64,
    pub unknown: bool,
    pub names: bool,
}

pub struct ReadQuery {
    pub chat: String,
    pub limit: i64,
    pub since: Option<String>,
    pub tapbacks: bool,
    pub names: bool,
    pub unknown: bool,
}

pub struct SearchQuery {
    pub query: String,
    pub chat: Option<String>,
    /// One person, across every conversation: their messages and mine.
    pub with: Option<String>,
    /// One person, across every conversation: only what they sent.
    pub from: Option<String>,
    pub limit: i64,
    pub since: Option<String>,
    pub unknown: bool,
    pub names: bool,
    /// How much of the conversation to show around each hit.
    pub context: crate::db::Context,
}

pub struct WatchQuery {
    pub chat: Option<String>,
    pub tapbacks: bool,
    pub unknown: bool,
    pub names: bool,
    /// Poll frequency for the direct path. The daemon has its own tick.
    pub interval: u64,
}

pub struct SendQuery {
    pub chat: String,
    pub body: Option<String>,
    /// Bytes rather than a path: the daemon never reads a path a client named (§6).
    pub file: Option<crate::daemon::protocol::Attachment>,
    pub names: bool,
}

/// Which end answered, which `msg daemon status` and the tests both care about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Daemon,
    Direct,
}

/// Where an attachment ended up.
#[derive(Debug, Clone)]
pub struct Saved {
    pub path: std::path::PathBuf,
    pub bytes: i64,
}

pub struct Source {
    kind: Kind,
    db_path: Option<String>,
    db: Option<Connection>,
    contacts: Option<ContactIndex>,
}

/// `--db` names a path, so it is answered locally and never sent to the daemon
/// (§6). `MSG_DB` is documented as `--db` by another name, so it has to steer
/// the same way: without this, pointing it at a fixture while a daemon is
/// listening would read the real database instead, the opposite of what it is
/// for.
pub fn open_source(db: Option<String>) -> Source {
    let db_path = db.or_else(|| std::env::var("MSG_DB").ok().filter(|v| !v.is_empty()));
    let kind = if db_path.is_some() {
        Kind::Direct
    } else if connect_daemon(None).is_some() {
        Kind::Daemon
    } else {
        Kind::Direct
    };
    Source {
        kind,
        db_path,
        db: None,
        contacts: None,
    }
}

fn call(message: &Request) -> Result<serde_json::Value> {
    let stream = connect_daemon(None)
        .ok_or_else(|| Error::other("msgd stopped listening; try `msg daemon status`"))?;
    request(stream, message)
}

/// Ask the daemon how it is doing, or `None` when it is not listening.
pub fn daemon_status() -> Result<Option<StatusReply>> {
    status_from(connect_daemon(None))
}

/// The same question, given up on after `timeout` rather than after the general
/// thirty seconds. For callers that would rather be told nothing than wait.
pub fn daemon_status_within(timeout: std::time::Duration) -> Result<Option<StatusReply>> {
    status_from(connect_daemon_within(None, timeout))
}

fn status_from(stream: Option<std::os::unix::net::UnixStream>) -> Result<Option<StatusReply>> {
    let Some(stream) = stream else {
        return Ok(None);
    };
    let value = request(stream, &Request::Status(Empty {}))?;
    Ok(Some(serde_json::from_value(value)?))
}

impl Source {
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The index and the connection, borrowed together — the borrow checker
    /// will not hand out both through separate `&mut self` calls.
    fn parts(&mut self, wanted: bool) -> Result<(&Connection, &ContactIndex)> {
        if self.db.is_none() {
            self.db = Some(open_database(self.db_path.as_deref(), true)?);
        }
        if wanted {
            if self.contacts.is_none() {
                self.contacts = Some(load_contacts(None));
            }
        } else if self.contacts.is_none() {
            self.contacts = Some(ContactIndex::empty());
        }
        Ok((
            self.db.as_ref().expect("just opened"),
            self.contacts.as_ref().expect("just loaded"),
        ))
    }

    pub fn chats(&mut self, query: &ChatsQuery) -> Result<Vec<Chat>> {
        if self.kind == Kind::Daemon {
            let value = call(&Request::Chats(ChatsRequest {
                query: query.query.clone(),
                limit: Some(query.limit),
                unknown: query.unknown.then_some(true),
                names: (!query.names).then_some(false),
            }))?;
            return Ok(serde_json::from_value(value)?);
        }
        let unknown = query.unknown;
        let limit = query.limit;
        let text = query.query.clone();
        let (db, contacts) = self.parts(query.names)?;
        fetch_chats(db, text.as_deref(), limit, contacts, unknown)
    }

    pub fn read(&mut self, query: &ReadQuery) -> Result<ReadReply> {
        if self.kind == Kind::Daemon {
            let value = call(&Request::Read(ReadRequest {
                chat: query.chat.clone(),
                limit: Some(query.limit),
                since: query.since.clone(),
                tapbacks: query.tapbacks.then_some(true),
                names: (!query.names).then_some(false),
                unknown: query.unknown.then_some(true),
            }))?;
            return Ok(serde_json::from_value(value)?);
        }
        let after_date = query
            .since
            .as_deref()
            .map(since_to_apple_date)
            .transpose()?;
        let (spec, limit, tapbacks) = (query.chat.clone(), query.limit, query.tapbacks);
        let unknown = query.unknown;
        let (db, contacts) = self.parts(query.names)?;
        let threads = resolve_conversation(db, &spec, contacts, unknown)?;
        let messages = fetch_conversation(db, &threads, after_date, limit, tapbacks, contacts)?;
        Ok(ReadReply::new(threads, messages))
    }

    pub fn search(&mut self, query: &SearchQuery) -> Result<Vec<Message>> {
        if self.kind == Kind::Daemon {
            let value = call(&Request::Search(SearchRequest {
                query: query.query.clone(),
                chat: query.chat.clone(),
                with: query.with.clone(),
                from: query.from.clone(),
                limit: Some(query.limit),
                since: query.since.clone(),
                unknown: query.unknown.then_some(true),
                names: (!query.names).then_some(false),
                before: (query.context.before > 0).then_some(query.context.before),
                after: (query.context.after > 0).then_some(query.context.after),
            }))?;
            return Ok(serde_json::from_value(value)?);
        }
        let after_date = query
            .since
            .as_deref()
            .map(since_to_apple_date)
            .transpose()?;
        let context = query.context;
        let (text, spec, with, from, limit, unknown) = (
            query.query.clone(),
            query.chat.clone(),
            query.with.clone(),
            query.from.clone(),
            query.limit,
            query.unknown,
        );
        let (db, contacts) = self.parts(query.names)?;
        let chat_id = spec
            .as_deref()
            .map(|spec| resolve_chat(db, spec, contacts).map(|chat| chat.rowid))
            .transpose()?;
        let person = person_filter(db, with.as_deref(), from.as_deref(), contacts)?;
        let hits = fetch_messages(
            db,
            &FetchMessages {
                query: Some(&text),
                chat_id,
                person: person.as_ref().map(|(person, sender)| PersonFilter {
                    person,
                    sender: *sender,
                }),
                after_date,
                limit,
                include_filtered: unknown,
                ..Default::default()
            },
            contacts,
        )?;
        with_context(db, hits, context, contacts)
    }

    pub fn resolve(&mut self, chat: &str, names: bool) -> Result<Chat> {
        if self.kind == Kind::Daemon {
            let value = call(&Request::Resolve(ResolveRequest {
                chat: chat.to_string(),
                names: (!names).then_some(false),
            }))?;
            return Ok(serde_json::from_value(value)?);
        }
        let spec = chat.to_string();
        let (db, contacts) = self.parts(names)?;
        resolve_chat(db, &spec, contacts)
    }

    pub fn contacts(&mut self, handles: &[String]) -> Result<ContactsReply> {
        if self.kind == Kind::Daemon {
            let value = call(&Request::Contacts(ContactsRequest {
                handles: handles.to_vec(),
            }))?;
            return Ok(serde_json::from_value(value)?);
        }
        // Loaded here rather than reused: `msg contacts` is the one command
        // whose whole subject is the index, so it always reads a fresh one.
        let index = load_contacts(None);
        Ok(ContactsReply {
            size: index.len(),
            resolved: handles
                .iter()
                .flat_map(|term| {
                    let found = index.find(term);
                    if found.is_empty() {
                        // Nothing matched, so the term is echoed back
                        // unresolved — the same answer an unknown address
                        // has always given.
                        return vec![ResolvedHandle {
                            handle: term.clone(),
                            name: None,
                            filed_as: None,
                        }];
                    }
                    found
                        .into_iter()
                        .map(|contact| ResolvedHandle {
                            handle: contact.handle.clone(),
                            name: Some(contact.name.clone()),
                            filed_as: contact.filed_as.clone(),
                        })
                        .collect()
                })
                .collect(),
        })
    }

    /// Sending needs Automation, and the CLI deliberately no longer asks for it.
    /// A gate the sending process enforces on itself is not a gate, so the only
    /// way to send is through a daemon macOS has been told may drive Messages
    /// (§7).
    pub fn send(&mut self, query: &SendQuery) -> Result<SendReply> {
        if self.kind != Kind::Daemon {
            return Err(Error::other(
                "sending needs the daemon, which holds the Automation permission.\n\
                 Install it with `msg daemon install`.",
            ));
        }
        let value = call(&Request::Send(SendRequest {
            chat: query.chat.clone(),
            body: query.body.clone(),
            file: query.file.clone(),
            names: (!query.names).then_some(false),
        }))?;
        Ok(serde_json::from_value(value)?)
    }

    pub fn automation(&mut self) -> Result<AutomationReply> {
        if self.kind != Kind::Daemon {
            return Err(Error::other(
                "the Automation permission belongs to the daemon, which is not running.\n\
                 Install it with `msg daemon install`.",
            ));
        }
        Ok(serde_json::from_value(call(&Request::Automation(
            Empty {},
        ))?)?)
    }

    /// Write one attachment to `directory`, and say what it was called.
    ///
    /// The client writes the file, always. The daemon holds Full Disk Access and
    /// hands over bytes; it never takes a destination, because a daemon that
    /// wrote to a caller-named path would be an arbitrary-file writer with a
    /// grant behind it — the mirror of the rule that keeps it from being an
    /// arbitrary-file reader (daemon-and-permissions.md §6).
    ///
    /// Written to a temporary name in the destination and renamed at the end, so
    /// an interrupted transfer leaves nothing that looks like a whole file.
    pub fn save(&mut self, id: i64, directory: &Path, force: bool) -> Result<Saved> {
        std::fs::create_dir_all(directory)?;

        let mut name = None;
        let temporary = directory.join(format!(".msg-save-{}-{id}", std::process::id()));
        let outcome = (|| -> Result<i64> {
            let mut file = std::fs::File::create(&temporary)?;
            if self.kind == Kind::Daemon {
                let stream = connect_daemon(None).ok_or_else(|| {
                    Error::other("msgd stopped listening; try `msg daemon status`")
                })?;
                let reply = save(stream, &SaveRequest { id }, |part| match part {
                    SavePart::Head { name: called, .. } => {
                        name = Some(called);
                        Ok(())
                    }
                    SavePart::Chunk { base64 } => {
                        let bytes = BASE64
                            .decode(base64)
                            .map_err(|error| Error::other(format!("msgd sent {error}")))?;
                        Ok(file.write_all(&bytes)?)
                    }
                })?;
                name.get_or_insert(reply.name);
                return Ok(reply.bytes);
            }

            let (db, _) = self.parts(false)?;
            let (path, attachment) = attachment_path(db, id)?;
            let mut source = std::fs::File::open(&path).map_err(|error| unreadable(id, &error))?;
            name = Some(
                attachment
                    .name
                    .unwrap_or_else(|| format!("attachment-{id}")),
            );
            Ok(i64::try_from(std::io::copy(&mut source, &mut file)?).unwrap_or(0))
        })();

        let bytes = match outcome {
            Ok(bytes) => bytes,
            Err(error) => {
                std::fs::remove_file(&temporary).ok();
                return Err(error);
            }
        };

        let name = attachment_name(&name.unwrap_or_else(|| format!("attachment-{id}")));
        let mut destination = directory.join(&name);
        if destination.exists() && !force {
            std::fs::remove_file(&temporary).ok();
            return Err(Error::other(format!(
                "{} already exists; pass --force to replace it",
                destination.display()
            )));
        }
        if let Err(error) = std::fs::rename(&temporary, &destination) {
            std::fs::remove_file(&temporary).ok();
            return Err(error.into());
        }
        destination = destination.canonicalize().unwrap_or(destination);
        Ok(Saved {
            path: destination,
            bytes,
        })
    }

    pub fn watch(
        &mut self,
        query: &WatchQuery,
        mut on_message: impl FnMut(&Message) -> Result<()>,
    ) -> Result<()> {
        if self.kind == Kind::Daemon {
            let stream = connect_daemon(None)
                .ok_or_else(|| Error::other("msgd stopped listening; try `msg daemon status`"))?;
            return watch(
                stream,
                &WatchRequest {
                    chat: query.chat.clone(),
                    tapbacks: query.tapbacks.then_some(true),
                    unknown: query.unknown.then_some(true),
                    names: (!query.names).then_some(false),
                },
                |value| on_message(&serde_json::from_value::<Message>(value)?),
            );
        }

        let (spec, tapbacks, unknown, interval) = (
            query.chat.clone(),
            query.tapbacks,
            query.unknown,
            query.interval,
        );
        let (db, contacts) = self.parts(query.names)?;
        let chat_id = spec
            .as_deref()
            .map(|spec| resolve_chat(db, spec, contacts).map(|chat| chat.rowid))
            .transpose()?;
        let mut watermark = latest_rowid(db)?;

        loop {
            let messages = fetch_messages(
                db,
                &FetchMessages {
                    chat_id,
                    after_rowid: Some(watermark),
                    limit: WATCH_BATCH,
                    include_tapbacks: tapbacks,
                    include_filtered: unknown,
                    oldest_first: true,
                    ..Default::default()
                },
                contacts,
            )?;
            for message in &messages {
                watermark = watermark.max(message.rowid);
                on_message(message)?;
            }
            std::thread::sleep(Duration::from_secs(interval));
        }
    }
}
