//! The daemon. It is the only process holding Full Disk Access, so it answers a
//! deliberately small set of questions and takes no filesystem path from anyone:
//! see docs/projects/all/daemon-and-permissions.md §6.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use rusqlite::Connection;

use crate::apple::since_to_apple_date;
use crate::contacts::{ContactIndex, load_contacts};
use crate::daemon::config::{config_path, disabled_message, read_config};
use crate::daemon::protocol::{
    AutomationReply, COMMANDS, ContactsReply, ErrorCode, Frame, PROTOCOL_VERSION, ReadReply,
    Request, ResolvedHandle, SavePart, SaveReply, SendReply, StatusReply, WatchRequest, encode,
    is_chat_guid, socket_path,
};
use crate::daemon::send::{check_automation, send_attachment, send_message};
use crate::db::{
    Chat, FetchMessages, Message, PersonFilter, database_path, describe_target, fetch_chats,
    fetch_messages, latest_rowid, open_database, person_filter, resolve_chat, unreadable,
};
use crate::{Error, Result, VERSION};

/// How often to look for new messages while somebody is watching.
///
/// The TypeScript build paired a 2-second timer with an `fs.watch` on the
/// database directory, so a new message surfaced in about 100ms. There is no
/// directory watch here: `SELECT MAX(rowid)` against a local SQLite file is
/// cheap enough to simply ask more often, and only while a watcher is attached.
/// Idle costs one wakeup every two seconds and no query at all.
const ACTIVE_POLL: Duration = Duration::from_millis(200);
const IDLE_POLL: Duration = Duration::from_secs(2);

/// Contacts changes are rare, so the index is reloaded on a slow timer.
const CONTACTS_TTL: Duration = Duration::from_secs(10 * 60);

/// How soon to try again when the index came back empty.
const CONTACTS_RETRY: Duration = Duration::from_secs(10);

/// How many new messages a single tick will hand to one watcher.
const WATCH_BATCH: i64 = 200;

/// How long a stalled watcher may block delivery before it is dropped.
///
/// The TypeScript daemon buffered writes in userspace and never blocked. Here a
/// client that stops reading eventually fills the socket buffer, and the write
/// happens while the watcher list is locked — so without this, one wedged
/// `msg watch` would stop the daemon answering anybody.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// `sun_path` is 104 bytes on macOS, and the kernel reports a bare EINVAL.
const MAX_SOCKET_PATH: usize = 103;

fn empty_index() -> Arc<ContactIndex> {
    static EMPTY: OnceLock<Arc<ContactIndex>> = OnceLock::new();
    EMPTY
        .get_or_init(|| Arc::new(ContactIndex::empty()))
        .clone()
}

fn write_frame(mut stream: &UnixStream, frame: &Frame) -> std::io::Result<()> {
    stream.write_all(encode(frame).as_bytes())
}

fn fail(stream: &UnixStream, code: ErrorCode, message: String) -> std::io::Result<()> {
    write_frame(stream, &Frame::Error { code, message })?;
    stream.shutdown(Shutdown::Both).ok();
    Ok(())
}

struct Watcher {
    id: u64,
    stream: UnixStream,
    request: WatchRequest,
    chat_id: Option<i64>,
    contacts: Arc<ContactIndex>,
    watermark: i64,
}

struct Cached {
    index: Arc<ContactIndex>,
    loaded_at: Instant,
}

/// What the daemon reads, as opposed to what a client may ask for.
///
/// Both fields come from the daemon's own environment or its test harness, and
/// never from the wire — a client that could name either would be naming a
/// filesystem path, which is the one thing the protocol refuses (§6).
#[derive(Debug, Default, Clone)]
pub struct DaemonOptions {
    pub db_path: Option<String>,
    /// Where the `send = true` gate is read from. `None` means the documented
    /// location, which is what the installed daemon uses.
    pub config_path: Option<PathBuf>,
}

struct Shared {
    db_path: Option<String>,
    config_path: Option<PathBuf>,
    db: Mutex<Option<Connection>>,
    contacts: Mutex<Option<Cached>>,
    watchers: Mutex<Vec<Watcher>>,
    started: Instant,
    shutdown: AtomicBool,
    next_watcher: AtomicU64,
}

impl Shared {
    /// Open the database, or fail.
    ///
    /// A failure is not cached. The install flow depends on that: the daemon
    /// runs before the grant exists, fails, and that failure is what creates the
    /// entry in the Full Disk Access list (§9). It has to start working once the
    /// switch is flipped, without a restart.
    ///
    /// Lock order is watchers before database, never the reverse.
    fn with_db<T>(&self, body: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let mut guard = self.db.lock().expect("database lock");
        if guard.is_none() {
            // A snapshot copy would be frozen at the moment it was taken, which
            // is wrong for a process that stays up and streams new messages.
            *guard = Some(open_database(self.db_path.as_deref(), false)?);
        }
        body(guard.as_ref().expect("just opened"))
    }

    fn contacts(&self, wanted: bool) -> Arc<ContactIndex> {
        if !wanted {
            return empty_index();
        }
        let mut guard = self.contacts.lock().expect("contacts lock");
        // A load that came back empty is retried soon rather than held for the
        // full interval: the daemon runs before its grant exists, so the first
        // attempt is expected to fail, and caching that failure makes names stay
        // broken long after the grant is given.
        let stale = guard.as_ref().is_none_or(|cached| {
            let ttl = if cached.index.is_empty() {
                CONTACTS_RETRY
            } else {
                CONTACTS_TTL
            };
            cached.loaded_at.elapsed() > ttl
        });
        if stale {
            let index = load_contacts(None);
            if !index.problems().is_empty() {
                eprintln!("msgd contacts: {}", index.problems().join(" | "));
            }
            *guard = Some(Cached {
                index: Arc::new(index),
                loaded_at: Instant::now(),
            });
        }
        guard.as_ref().expect("just loaded").index.clone()
    }

    fn watcher_count(&self) -> usize {
        self.watchers.lock().expect("watchers lock").len()
    }

    fn config(&self) -> PathBuf {
        self.config_path.clone().unwrap_or_else(config_path)
    }
}

pub struct Daemon {
    shared: Arc<Shared>,
    socket: Mutex<Option<PathBuf>>,
}

impl Daemon {
    pub fn new(options: DaemonOptions) -> Self {
        Self {
            shared: Arc::new(Shared {
                db_path: options.db_path,
                config_path: options.config_path,
                db: Mutex::new(None),
                contacts: Mutex::new(None),
                watchers: Mutex::new(Vec::new()),
                started: Instant::now(),
                shutdown: AtomicBool::new(false),
                next_watcher: AtomicU64::new(1),
            }),
            socket: Mutex::new(None),
        }
    }

    /// Touch the database, so a denial happens now rather than on first use.
    ///
    /// Under launchd the *failure* is the point: a denied access is what creates
    /// this bundle's entry in the Full Disk Access list, and there is no other
    /// way to make it appear there to be switched on (§9).
    pub fn probe_database(&self) -> Result<()> {
        self.shared.with_db(|_| Ok(()))
    }

    pub fn watcher_count(&self) -> usize {
        self.shared.watcher_count()
    }

    pub fn listen(&self, path: Option<PathBuf>) -> Result<PathBuf> {
        let path = path.unwrap_or_else(socket_path);
        if path.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(Error::other(format!(
                "socket path is too long for macOS ({} > {MAX_SOCKET_PATH}): {}",
                path.as_os_str().len(),
                path.display()
            )));
        }

        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory)?;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        }
        // A socket left behind by a killed daemon would block the bind.
        if path.exists() {
            std::fs::remove_file(&path)?;
        }

        let listener = UnixListener::bind(&path)?;
        // The kernel restricts the socket to this uid, which is the whole of the
        // access control the daemon has or wants (§5).
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        *self.socket.lock().expect("socket lock") = Some(path.clone());

        let accepting = self.shared.clone();
        std::thread::spawn(move || accept_loop(&accepting, &listener));

        let ticking = self.shared.clone();
        std::thread::spawn(move || tick_loop(&ticking));

        Ok(path)
    }

    pub fn close(&self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        for watcher in self
            .shared
            .watchers
            .lock()
            .expect("watchers lock")
            .drain(..)
        {
            // Shutting the socket down rather than dropping our copy: the
            // connection thread is blocked reading the same socket through its
            // own descriptor, and only this wakes it.
            watcher.stream.shutdown(Shutdown::Both).ok();
        }
        if let Some(path) = self.socket.lock().expect("socket lock").take() {
            // Wake the accept loop, which is blocked in accept(2), so it can see
            // the shutdown flag and return.
            UnixStream::connect(&path).ok();
            std::fs::remove_file(&path).ok();
        }
        *self.shared.db.lock().expect("database lock") = None;
    }
}

fn accept_loop(shared: &Arc<Shared>, listener: &UnixListener) {
    for stream in listener.incoming() {
        if shared.shutdown.load(Ordering::SeqCst) {
            return;
        }
        let Ok(stream) = stream else { continue };
        let shared = shared.clone();
        std::thread::spawn(move || {
            if let Err(error) = accept(&shared, stream) {
                eprintln!("msgd connection: {error}");
            }
        });
    }
}

fn accept(shared: &Arc<Shared>, stream: UnixStream) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    // One request per connection; a watch leaves the socket open behind us.
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    serve(shared, stream, line.trim(), reader)
}

fn serve(
    shared: &Arc<Shared>,
    stream: UnixStream,
    line: &str,
    reader: BufReader<UnixStream>,
) -> Result<()> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        fail(&stream, ErrorCode::Error, "malformed request".into())?;
        return Ok(());
    };

    let version = value.get("v").and_then(serde_json::Value::as_u64);
    if version != Some(u64::from(PROTOCOL_VERSION)) {
        let spoken = version.map_or_else(|| "nothing".to_string(), |value| value.to_string());
        fail(
            &stream,
            ErrorCode::Version,
            format!(
                "msgd speaks protocol {PROTOCOL_VERSION}, this client speaks {spoken}. \
                 Reinstall the daemon with `msg daemon install`."
            ),
        )?;
        return Ok(());
    }

    // Checked by name before parsing, so a command this daemon predates is
    // reported as such. Without it the dispatch would answer `result` with no
    // value, which the caller reads a field off and crashes on. The version
    // check should have caught it first; this is what makes forgetting to bump
    // it a clear error rather than a crash in the CLI.
    let cmd = value
        .get("cmd")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !COMMANDS.contains(&cmd.as_str()) {
        fail(
            &stream,
            ErrorCode::Error,
            format!("msgd does not understand `{cmd}`. Reinstall it with `msg daemon install`."),
        )?;
        return Ok(());
    }

    let request = match serde_json::from_value::<Request>(value) {
        Ok(request) => request,
        Err(error) => {
            fail(
                &stream,
                ErrorCode::Error,
                format!("msgd could not read this `{cmd}` request: {error}"),
            )?;
            return Ok(());
        }
    };

    if let Request::Watch(watch) = request {
        return subscribe(shared, stream, watch, reader);
    }

    // Streamed rather than returned, so it is routed like `watch` and for the
    // same reason: the answer does not fit comfortably in one frame.
    if let Request::Save(ask) = request {
        return stream_attachment(shared, &stream, ask.id);
    }

    match answer(shared, request) {
        Ok(value) => write_frame(&stream, &Frame::Result { value })?,
        Err(error) => write_frame(&stream, &Frame::from_error(&error))?,
    }
    stream.shutdown(Shutdown::Both).ok();
    Ok(())
}

fn answer(shared: &Arc<Shared>, request: Request) -> Result<serde_json::Value> {
    match request {
        Request::Chats(ask) => {
            let contacts = shared.contacts(ask.names != Some(false));
            let chats: Vec<Chat> = shared.with_db(|db| {
                fetch_chats(
                    db,
                    ask.query.as_deref(),
                    ask.limit.unwrap_or(30),
                    &contacts,
                    ask.unknown == Some(true),
                )
            })?;
            Ok(serde_json::to_value(chats)?)
        }
        Request::Read(ask) => {
            let contacts = shared.contacts(ask.names != Some(false));
            let after_date = ask.since.as_deref().map(since_to_apple_date).transpose()?;
            let reply = shared.with_db(|db| {
                let chat = resolve_chat(db, &ask.chat, &contacts)?;
                let messages = fetch_messages(
                    db,
                    &FetchMessages {
                        chat_id: Some(chat.rowid),
                        after_date,
                        limit: ask.limit.unwrap_or(50),
                        include_tapbacks: ask.tapbacks == Some(true),
                        ..Default::default()
                    },
                    &contacts,
                )?;
                Ok(ReadReply { chat, messages })
            })?;
            Ok(serde_json::to_value(reply)?)
        }
        Request::Search(ask) => {
            let contacts = shared.contacts(ask.names != Some(false));
            let after_date = ask.since.as_deref().map(since_to_apple_date).transpose()?;
            let messages: Vec<Message> = shared.with_db(|db| {
                let chat_id = ask
                    .chat
                    .as_deref()
                    .map(|spec| resolve_chat(db, spec, &contacts).map(|chat| chat.rowid))
                    .transpose()?;
                // Resolved against the same contact index the results are named
                // with, so the name that comes back is the name that was asked
                // for. `--no-names` leaves only the addresses to match on.
                let person =
                    person_filter(db, ask.with.as_deref(), ask.from.as_deref(), &contacts)?;
                fetch_messages(
                    db,
                    &FetchMessages {
                        query: Some(&ask.query),
                        chat_id,
                        person: person.as_ref().map(|(person, sender)| PersonFilter {
                            person,
                            sender: *sender,
                        }),
                        after_date,
                        limit: ask.limit.unwrap_or(25),
                        include_filtered: ask.unknown == Some(true),
                        ..Default::default()
                    },
                    &contacts,
                )
            })?;
            Ok(serde_json::to_value(messages)?)
        }
        Request::Resolve(ask) => {
            let contacts = shared.contacts(ask.names != Some(false));
            let chat = shared.with_db(|db| resolve_chat(db, &ask.chat, &contacts))?;
            Ok(serde_json::to_value(chat)?)
        }
        Request::Send(ask) => {
            // The config key is checked here rather than in the client, because
            // a check the caller runs on itself is advice rather than a gate (§7).
            let path = shared.config();
            if !read_config(&path).send {
                return Err(Error::SendDisabled(disabled_message(&path)));
            }

            // A guid needs no lookup, so a daemon holding Automation but not
            // Full Disk Access can still send.
            let (guid, name) = if is_chat_guid(&ask.chat) {
                (ask.chat.clone(), ask.chat)
            } else {
                let contacts = shared.contacts(ask.names != Some(false));
                let chat = shared.with_db(|db| resolve_chat(db, &ask.chat, &contacts))?;
                // Described with its address, so the confirmation names which
                // of a person's conversations this went to.
                (chat.guid.clone(), describe_target(&chat))
            };

            if let Some(file) = ask.file {
                let data = base64::engine::general_purpose::STANDARD
                    .decode(file.base64.as_bytes())
                    .map_err(|error| Error::other(format!("unreadable attachment: {error}")))?;
                send_attachment(&guid, &file.name, &data)?;
            } else {
                let body = ask.body.unwrap_or_default();
                if body.is_empty() {
                    return Err(Error::other("nothing to send"));
                }
                send_message(&guid, &body)?;
            }
            Ok(serde_json::to_value(SendReply { guid, name })?)
        }
        Request::Automation(_) => {
            // Not behind the config key: this sends nothing, and refusing to run
            // it when sending is off would hide the permission exactly when
            // someone is trying to inspect or revoke it.
            let (allowed, detail) = check_automation();
            Ok(serde_json::to_value(AutomationReply {
                allowed,
                detail,
                config_enabled: read_config(&shared.config()).send,
            })?)
        }
        Request::Contacts(ask) => {
            let index = shared.contacts(true);
            Ok(serde_json::to_value(ContactsReply {
                size: index.len(),
                resolved: ask
                    .handles
                    .into_iter()
                    .map(|handle| ResolvedHandle {
                        name: index.lookup(Some(&handle)).map(str::to_string),
                        handle,
                    })
                    .collect(),
            })?)
        }
        Request::Status(_) => {
            let index = shared.contacts(true);
            let message_count = shared.with_db(latest_rowid)?;
            Ok(serde_json::to_value(StatusReply {
                version: VERSION.to_string(),
                protocol: PROTOCOL_VERSION,
                pid: std::process::id(),
                uptime_seconds: shared.started.elapsed().as_secs_f64().round() as u64,
                database: database_path(shared.db_path.as_deref())
                    .to_string_lossy()
                    .into_owned(),
                message_count,
                contact_count: index.len(),
                contact_problems: index.problems().to_vec(),
                watchers: shared.watcher_count(),
            })?)
        }
        Request::Watch(_) => unreachable!("watch is handled before dispatch"),
        Request::Save(_) => unreachable!("save is handled before dispatch"),
    }
}

/// The size of one `save` frame's worth of file, before base64 inflates it by a
/// third. Small enough that the daemon's peak memory does not follow the size of
/// what is being copied, large enough that a 548MB video is not 140,000 frames.
const SAVE_CHUNK: usize = 4 * 1024 * 1024;

const BASE64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

/// Send one attachment's bytes to a client that cannot open the file itself.
///
/// The only place the daemon spends its Full Disk Access on something other than
/// `chat.db`, and the shape is what keeps that narrow: the caller names a rowid,
/// `attachment_path` turns it into a path, and nothing takes a path from the
/// wire. See attachments.md §3.
fn stream_attachment(shared: &Arc<Shared>, stream: &UnixStream, id: i64) -> Result<()> {
    let found = shared.with_db(|db| crate::db::attachment_path(db, id));
    let (path, attachment) = match found {
        Ok(found) => found,
        Err(error) => {
            write_frame(stream, &Frame::from_error(&error))?;
            stream.shutdown(Shutdown::Both).ok();
            return Ok(());
        }
    };

    let result = (|| -> Result<i64> {
        let mut file = std::fs::File::open(&path).map_err(|error| unreadable(id, &error))?;
        write_frame(
            stream,
            &Frame::item(&SavePart::Head {
                name: attachment
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("attachment-{id}")),
                mime_type: attachment.mime_type.clone(),
                total_bytes: attachment.total_bytes,
            })?,
        )?;

        let mut sent = 0i64;
        let mut buffer = vec![0u8; SAVE_CHUNK];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            write_frame(
                stream,
                &Frame::item(&SavePart::Chunk {
                    base64: BASE64.encode(&buffer[..read]),
                })?,
            )?;
            sent += i64::try_from(read).unwrap_or(0);
        }
        Ok(sent)
    })();

    match result {
        Ok(bytes) => write_frame(
            stream,
            &Frame::result(&SaveReply {
                name: attachment
                    .name
                    .unwrap_or_else(|| format!("attachment-{id}")),
                bytes,
            })?,
        )?,
        Err(error) => write_frame(stream, &Frame::from_error(&error))?,
    }
    stream.shutdown(Shutdown::Both).ok();
    Ok(())
}

/// Register a watcher, then hold the connection open until the client goes away.
fn subscribe(
    shared: &Arc<Shared>,
    stream: UnixStream,
    request: WatchRequest,
    mut reader: BufReader<UnixStream>,
) -> Result<()> {
    let contacts = shared.contacts(request.names != Some(false));
    let started = shared.with_db(|db| {
        let chat_id = request
            .chat
            .as_deref()
            .map(|spec| resolve_chat(db, spec, &contacts).map(|chat| chat.rowid))
            .transpose()?;
        // Where this watcher starts. There is no shared cursor: each watcher is
        // compared against the current maximum on every tick, so one subscribing
        // mid-burst cannot be skipped by another's progress.
        Ok((chat_id, latest_rowid(db)?))
    });

    let (chat_id, watermark) = match started {
        Ok(started) => started,
        Err(error) => {
            write_frame(&stream, &Frame::from_error(&error))?;
            stream.shutdown(Shutdown::Both).ok();
            return Ok(());
        }
    };

    let delivery = stream.try_clone()?;
    delivery.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let id = shared.next_watcher.fetch_add(1, Ordering::SeqCst);
    shared
        .watchers
        .lock()
        .expect("watchers lock")
        .push(Watcher {
            id,
            stream: delivery,
            request,
            chat_id,
            contacts,
            watermark,
        });

    // Blocks until the client disconnects or the daemon shuts the socket down.
    let mut ignored = Vec::new();
    reader.read_to_end(&mut ignored).ok();

    shared
        .watchers
        .lock()
        .expect("watchers lock")
        .retain(|watcher| watcher.id != id);
    Ok(())
}

fn tick_loop(shared: &Arc<Shared>) {
    loop {
        let idle = shared.watcher_count() == 0;
        std::thread::sleep(if idle { IDLE_POLL } else { ACTIVE_POLL });
        if shared.shutdown.load(Ordering::SeqCst) {
            return;
        }
        tick(shared);
    }
}

fn tick(shared: &Arc<Shared>) {
    if shared.watcher_count() == 0 {
        return;
    }
    let latest = match shared.with_db(latest_rowid) {
        Ok(latest) => latest,
        Err(error) => {
            // A watcher that stops receiving with a live connection looks like a
            // quiet conversation. Say so instead.
            fail_watchers(shared, &error);
            return;
        }
    };

    // Rowids only climb, so one query answers "is there anything new" for every
    // watcher at once. This is the reason to have a daemon at all. The question
    // is per watcher, though: one that is still behind has messages waiting even
    // when nothing has arrived since the last tick.
    let mut watchers = shared.watchers.lock().expect("watchers lock");
    let mut failed: Vec<(u64, Error)> = Vec::new();
    for watcher in watchers.iter_mut() {
        if watcher.watermark >= latest {
            continue;
        }
        if let Err(error) = deliver(shared, watcher, latest) {
            failed.push((watcher.id, error));
        }
    }
    for (id, error) in failed {
        if let Some(index) = watchers.iter().position(|watcher| watcher.id == id) {
            let watcher = watchers.remove(index);
            report(&watcher, &error);
        }
    }
}

/// Send everything the watcher has not seen, a batch at a time.
///
/// Draining matters: a single batch is capped, and stopping after one would
/// strand the rest until another message happened to arrive.
fn deliver(shared: &Arc<Shared>, watcher: &mut Watcher, latest: i64) -> Result<()> {
    while watcher.watermark < latest {
        let messages = shared.with_db(|db| {
            fetch_messages(
                db,
                &FetchMessages {
                    chat_id: watcher.chat_id,
                    after_rowid: Some(watcher.watermark),
                    limit: WATCH_BATCH,
                    include_tapbacks: watcher.request.tapbacks == Some(true),
                    include_filtered: watcher.request.unknown == Some(true),
                    oldest_first: true,
                    ..Default::default()
                },
                &watcher.contacts,
            )
        })?;

        let count = i64::try_from(messages.len()).unwrap_or(WATCH_BATCH);
        for message in messages {
            watcher.watermark = watcher.watermark.max(message.rowid);
            write_frame(&watcher.stream, &Frame::item(&message)?)?;
        }

        // Filters can hide every row in a batch, so advance past what was read
        // rather than only past what was sent, or this loop never ends.
        if count < WATCH_BATCH {
            watcher.watermark = latest;
            return Ok(());
        }
    }
    Ok(())
}

fn report(watcher: &Watcher, error: &Error) {
    write_frame(&watcher.stream, &Frame::from_error(error)).ok();
    watcher.stream.shutdown(Shutdown::Both).ok();
}

fn fail_watchers(shared: &Arc<Shared>, error: &Error) {
    for watcher in shared.watchers.lock().expect("watchers lock").drain(..) {
        report(&watcher, error);
    }
}
