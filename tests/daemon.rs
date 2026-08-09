//! The daemon end to end, over a real unix socket, against a fixture database.
//!
//! Every request asks for `names: false`, so nothing here reads the Contacts or
//! any other database belonging to whoever is running the tests. Sending stays
//! switched off: the daemon is pointed at a config file that does not exist, and
//! `sending_is_off_for_every_test_here` asserts that rather than assuming it.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use msg::apple::to_apple_date;
use msg::daemon::client::{connect_daemon, connect_daemon_within, request};
use msg::daemon::protocol::{
    ChatsRequest, ContactsRequest, Empty, PROTOCOL_VERSION, ReadRequest, Request, ResolveRequest,
    SavePart, SaveRequest, SearchRequest, SendRequest, WatchRequest, envelope,
};
use msg::daemon::server::{Daemon, DaemonOptions};
use rusqlite::Connection;

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
fn at(minutes: i64) -> i64 {
    let start = chrono::DateTime::parse_from_rfc3339("2026-01-15T17:30:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    to_apple_date(start + chrono::Duration::minutes(minutes)).expect("in range")
}

struct Harness {
    directory: PathBuf,
    database: PathBuf,
    socket: PathBuf,
    config: PathBuf,
    _daemon: Daemon,
}

/// One daemon for the whole file, like the TypeScript `beforeAll`.
fn harness() -> &'static Harness {
    static HARNESS: OnceLock<Harness> = OnceLock::new();
    HARNESS.get_or_init(|| {
        let directory = msg::db::temporary_directory("msg-daemon-").unwrap();
        let database = directory.join("chat.db");
        let socket = directory.join("msgd.sock");
        // A path that is never created. No test here may enable sending: doing
        // so would drive Messages for real.
        let config = directory.join("config-that-does-not-exist.toml");
        build_fixture(&database);

        let daemon = Daemon::new(DaemonOptions {
            db_path: Some(database.to_string_lossy().into_owned()),
            config_path: Some(config.clone()),
        });
        daemon.listen(Some(socket.clone())).unwrap();
        Harness {
            directory,
            database,
            socket,
            config,
            _daemon: daemon,
        }
    })
}

fn build_fixture(path: &Path) {
    let db = Connection::open(path).unwrap();
    db.execute_batch(SCHEMA).unwrap();
    db.execute_batch(
        "
        INSERT INTO handle (rowid, id) VALUES (1, '+13105551234'), (2, 'someone@example.com'),
        -- One address in two casings, which `handle_key` folds to the same
        -- person without any Contacts record. That is what lets the merge be
        -- exercised here, where names are deliberately off.
          (4, 'robin@example.com'), (5, 'ROBIN@example.com'), (6, 'kit@example.com');
        INSERT INTO chat (rowid, guid, chat_identifier, display_name, is_filtered) VALUES
          (1, 'iMessage;-;+13105551234', '+13105551234', '', 0),
          (2, 'iMessage;+;chat9', 'chat9', 'Ship Room', 0),
          (3, 'SMS;-;+18885550000', '+18885550000', '', 1),
          (4, 'iMessage;-;robin@example.com', 'robin@example.com', '', 0),
          (5, 'iMessage;-;ROBIN@example.com', 'ROBIN@example.com', '', 0),
          -- A room of Robin and one other, for naming a conversation by who is
          -- in it. Robin joins it under both of his addresses, so it also
          -- proves membership counts people rather than addresses.
          (6, 'iMessage;-;kit@example.com', 'kit@example.com', '', 0),
          (7, 'iMessage;+;chat7', 'chat7', '', 0);
        INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (1, 1), (2, 1), (2, 2), (3, 1),
          (4, 4), (5, 5), (6, 6), (7, 4), (7, 5), (7, 6);
        ",
    )
    .unwrap();

    /// rowid, guid, body, from me, handle, associated type, date, chat.
    type Row = (i64, &'static str, &'static str, i64, i64, i64, i64, i64);

    let rows: [Row; 10] = [
        (1, "m1", "are you around later", 0, 1, 0, at(0), 1),
        (2, "m2", "after 6, yeah", 1, 1, 0, at(1), 1),
        (3, "m3", "deploy is green", 0, 2, 0, at(2), 2),
        (4, "m4", "your code is 123456", 0, 1, 0, at(3), 3),
        (5, "m5", "Liked \"after 6, yeah\"", 0, 1, 2000, at(4), 1),
        // The split conversation, interleaved across its two threads and older
        // than everything above so the chat list keeps a fixed order.
        (6, "m6", "sent from the phone", 0, 4, 0, at(-40), 4),
        (7, "m7", "and this from the other", 0, 5, 0, at(-30), 5),
        (8, "m8", "phone again", 0, 4, 0, at(-20), 4),
        (9, "m9", "newest of the pair", 0, 5, 0, at(-10), 5),
        (10, "m10", "in the room", 0, 6, 0, at(-5), 7),
    ];
    for (rowid, guid, body, from_me, handle, associated, date, chat) in rows {
        insert(
            &db, rowid, guid, body, from_me, handle, associated, date, chat,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn insert(
    db: &Connection,
    rowid: i64,
    guid: &str,
    body: &str,
    from_me: i64,
    handle: i64,
    associated: i64,
    date: i64,
    chat: i64,
) {
    db.execute(
        "INSERT INTO message (rowid, guid, text, is_from_me, handle_id,
             associated_message_type, date, service)
         VALUES (?, ?, ?, ?, ?, ?, ?, 'iMessage')",
        rusqlite::params![rowid, guid, body, from_me, handle, associated, date],
    )
    .unwrap();
    db.execute(
        "INSERT INTO chat_message_join (chat_id, message_id, message_date)
         VALUES (?, ?, ?)",
        rusqlite::params![chat, rowid, date],
    )
    .unwrap();
}

/// Rowids above everything the fixture holds, handed out so the watch tests can
/// write without colliding when the harness runs them concurrently.
fn next_rowids(count: i64) -> std::ops::Range<i64> {
    static NEXT: Mutex<i64> = Mutex::new(1_000);
    let mut guard = NEXT.lock().unwrap();
    let start = *guard;
    *guard += count;
    start..start + count
}

fn ask(message: &Request) -> msg::Result<serde_json::Value> {
    let stream = connect_daemon(Some(&harness().socket)).expect("daemon listening");
    request(stream, message)
}

/// One request, one frame back, undecoded — `request` raises an error frame as
/// an error, which is right for the CLI and wrong for asserting on the frame.
fn raw(line: &str) -> serde_json::Value {
    let mut stream = UnixStream::connect(&harness().socket).unwrap();
    stream.write_all(line.as_bytes()).unwrap();
    let mut reply = String::new();
    BufReader::new(stream.try_clone().unwrap())
        .read_line(&mut reply)
        .unwrap();
    serde_json::from_str(&reply).unwrap()
}

fn names_off_chats(query: Option<&str>, unknown: bool) -> Vec<serde_json::Value> {
    let value = ask(&Request::Chats(ChatsRequest {
        query: query.map(str::to_string),
        names: Some(false),
        unknown: unknown.then_some(true),
        ..Default::default()
    }))
    .unwrap();
    value.as_array().unwrap().clone()
}

// ---------------------------------------------------------------- chats

#[test]
fn lists_conversations_and_hides_the_filtered_ones() {
    let chats = names_off_chats(None, false);
    // Ordered by most recent activity, and chat 3 is the filtered one. The two
    // trailing rows are the split conversation, which the listing still shows
    // as two — merging it there is slice two of conversation-merging.md §10.
    let names: Vec<&str> = chats.iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "+13105551234",
            "Ship Room",
            // The room, named for its members since it has no name of its own.
            "robin@example.com, ROBIN@example.com, kit@example.com",
            "ROBIN@example.com",
            "robin@example.com",
            // No messages, so it sorts last.
            "kit@example.com"
        ]
    );
}

#[test]
fn includes_filtered_conversations_when_asked() {
    let chats = names_off_chats(None, true);
    assert_eq!(chats.len(), 7);
    assert!(
        chats
            .iter()
            .any(|c| c["isFiltered"] == serde_json::json!(true))
    );
}

#[test]
fn filters_by_name() {
    let chats = names_off_chats(Some("Ship"), false);
    let rowids: Vec<i64> = chats.iter().map(|c| c["rowid"].as_i64().unwrap()).collect();
    assert_eq!(rowids, [2]);
}

// ----------------------------------------------------------------- read

fn read(chat: &str, tapbacks: bool) -> serde_json::Value {
    ask(&Request::Read(ReadRequest {
        chat: vec![chat.into()],
        names: Some(false),
        tapbacks: tapbacks.then_some(true),
        ..Default::default()
    }))
    .unwrap()
}

#[test]
fn returns_a_conversation_oldest_first_without_tapbacks() {
    let value = read("1", false);
    assert_eq!(value["chat"]["rowid"], serde_json::json!(1));
    let bodies: Vec<&str> = value["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies, ["are you around later", "after 6, yeah"]);
    assert_eq!(value["messages"][1]["sender"], serde_json::json!("me"));
}

#[test]
fn includes_tapbacks_when_asked() {
    let value = read("1", true);
    assert_eq!(value["messages"].as_array().unwrap().len(), 3);
    assert_eq!(value["messages"][2]["isTapback"], serde_json::json!(true));
}

#[test]
fn reaches_a_filtered_conversation_when_it_is_named_outright() {
    let value = read("3", false);
    assert_eq!(value["messages"].as_array().unwrap().len(), 1);
}

/// The daemon serialises to ISO strings; the client is expected to revive them.
/// That shape is the contract the TypeScript client still reads (§4).
#[test]
fn dates_cross_the_wire_as_iso_strings() {
    let value = read("1", false);
    let date = value["messages"][0]["date"].as_str().unwrap();
    assert_eq!(date, "2026-01-15T17:30:00.000Z");
}

// --------------------------------------------------------------- search

fn search(query: &str, unknown: bool) -> Vec<serde_json::Value> {
    ask(&Request::Search(SearchRequest {
        query: query.into(),
        names: Some(false),
        unknown: unknown.then_some(true),
        ..Default::default()
    }))
    .unwrap()
    .as_array()
    .unwrap()
    .clone()
}

/// Several names reach the room with exactly those people, over the wire.
///
/// The bug this pins is the one the version bump to 11 exists for. `chat` is a
/// list now, and a daemon that reads it as a string cannot answer this at all —
/// which is the loud failure, and the reason the field changed shape rather
/// than the names being joined with a separator that a name could contain.
#[test]
fn naming_a_room_by_its_people_survives_the_wire() {
    let value = ask(&Request::Read(ReadRequest {
        chat: vec!["robin".into(), "kit".into()],
        names: Some(false),
        ..Default::default()
    }))
    .unwrap();

    assert_eq!(value["chat"]["rowid"], serde_json::json!(7), "{value}");
    let bodies: Vec<&str> = value["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies, ["in the room"]);
    // A room never merges, however many addresses its members have — and Robin
    // is in this one under two of them.
    assert!(value.get("merged").is_none(), "{value}");

    // One of the two names alone is that person, not the room.
    let alone = ask(&Request::Read(ReadRequest {
        chat: vec!["kit".into()],
        names: Some(false),
        ..Default::default()
    }))
    .unwrap();
    assert_eq!(alone["chat"]["rowid"], serde_json::json!(6), "{alone}");

    // And a pair with no room of its own says so rather than answering with
    // the room that merely contains them.
    let error = ask(&Request::Read(ReadRequest {
        chat: vec!["robin".into(), "someone".into()],
        names: Some(false),
        ..Default::default()
    }))
    .unwrap_err()
    .to_string();
    assert!(error.contains("no conversation with exactly"), "{error}");
}

/// A person's two threads arrive as one transcript, over the wire.
///
/// The bug this pins is the silent one protocol 10 exists to prevent. A daemon
/// that does not merge answers a name with a single thread and no field saying
/// so, which is indistinguishable from a person who only has one — the reply
/// looks correct and is missing half the conversation. `db.rs` cannot catch it,
/// because the omission happens on the other side of the socket.
#[test]
fn a_merged_conversation_survives_the_wire() {
    let value = ask(&Request::Read(ReadRequest {
        chat: vec!["robin".into()],
        names: Some(false),
        ..Default::default()
    }))
    .unwrap();

    let bodies: Vec<&str> = value["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["body"].as_str().unwrap())
        .collect();
    assert_eq!(
        bodies,
        [
            "sent from the phone",
            "and this from the other",
            "phone again",
            "newest of the pair"
        ],
        "interleaved by arrival, not one thread then the other: {bodies:?}"
    );

    // `chat` still names the thread a reply would continue, so a consumer
    // reading it gets what it always got, and `merged` is the new half.
    assert_eq!(value["chat"]["rowid"], serde_json::json!(5));
    assert_eq!(value["merged"], serde_json::json!([4]));

    // A rowid means the thread, so the escape hatch has to cross too.
    let one = ask(&Request::Read(ReadRequest {
        chat: vec!["4".into()],
        names: Some(false),
        ..Default::default()
    }))
    .unwrap();
    let bodies: Vec<&str> = one["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies, ["sent from the phone", "phone again"]);
    // Skipped when empty, so an unmerged read is byte-identical to version 9.
    assert!(one.get("merged").is_none(), "{one}");
}

/// The two lines that join `with_context` to the wire, which nothing else
/// reaches: the CLI tests drive the direct `--db` path and the `db.rs` tests
/// drive the windowing in isolation.
///
/// This is the path an installed `msg` actually takes, and the bug it would
/// hide is the silent one protocol 9 exists to prevent — a daemon answering
/// with bare hits while the version check passes looks exactly like a search
/// that found nothing to show.
#[test]
fn context_widths_survive_the_wire() {
    let messages = ask(&Request::Search(SearchRequest {
        query: "after 6".into(),
        names: Some(false),
        before: Some(1),
        after: Some(1),
        ..Default::default()
    }))
    .unwrap();
    let messages = messages.as_array().unwrap();

    let bodies: Vec<&str> = messages
        .iter()
        .map(|m| m["body"].as_str().unwrap())
        .collect();
    assert_eq!(
        bodies,
        [
            "are you around later",
            "after 6, yeah",
            // A reaction is context even though a search can never return one,
            // and this proves the window's `include_tapbacks` crosses too.
            "Liked \"after 6, yeah\""
        ],
        "{bodies:?}"
    );

    // `matched` rides on a serde default rather than a field always written, so
    // a context message that lost it in transit would arrive looking like a
    // hit — the same wrong answer as the marker bug, invisible to `db.rs`.
    let matched: Vec<bool> = messages
        .iter()
        .map(|m| m["matched"].as_bool().unwrap_or(true))
        .collect();
    assert_eq!(matched, [false, true, false], "{matched:?}");

    // And one run, so the separator is reproducible by whoever reads this.
    let groups: Vec<i64> = messages
        .iter()
        .map(|m| m["group"].as_i64().unwrap())
        .collect();
    assert_eq!(groups, [0, 0, 0], "{groups:?}");

    // Asymmetric, because equal widths cannot tell the two fields apart: a
    // daemon that swapped them would answer the case above correctly.
    let lopsided = ask(&Request::Search(SearchRequest {
        query: "after 6".into(),
        names: Some(false),
        before: Some(0),
        after: Some(1),
        ..Default::default()
    }))
    .unwrap();
    let bodies: Vec<&str> = lopsided
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["body"].as_str().unwrap())
        .collect();
    assert_eq!(
        bodies,
        ["after 6, yeah", "Liked \"after 6, yeah\""],
        "{bodies:?}"
    );
}

#[test]
fn matches_message_bodies() {
    let bodies: Vec<String> = search("deploy", false)
        .iter()
        .map(|m| m["body"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(bodies, ["deploy is green"]);
}

#[test]
fn skips_filtered_conversations_unless_asked() {
    assert!(search("123456", false).is_empty());
    assert_eq!(search("123456", true).len(), 1);
}

/// The person filters over the wire, since a filter that is dropped in
/// transit reads as "they never said it" rather than as an error.
///
/// `names: false` throughout, so the fixture's addresses are the only thing
/// being matched and nobody's real Contacts database is touched.
#[test]
fn searches_one_person_across_conversations() {
    let person = |with: Option<&str>, from: Option<&str>| {
        ask(&Request::Search(SearchRequest {
            // Every fixture body contains a vowel; the point here is the
            // person filter, not the text match.
            query: "e".into(),
            with: with.map(str::to_string),
            from: from.map(str::to_string),
            names: Some(false),
            ..Default::default()
        }))
        .map(|value| {
            value
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["isFromMe"].as_bool().unwrap())
                .collect::<Vec<_>>()
        })
    };

    let theirs = person(None, Some("+13105551234")).unwrap();
    assert!(!theirs.is_empty(), "found nothing to check");
    assert!(
        theirs.iter().all(|from_me| !from_me),
        "--from returned my own messages: {theirs:?}"
    );

    let both = person(Some("+13105551234"), None).unwrap();
    assert!(
        both.len() > theirs.len(),
        "--with should add my side: {both:?} vs {theirs:?}"
    );
    assert!(both.iter().any(|from_me| *from_me));
}

/// The daemon answers whatever a client sends, and a client is not the CLI, so
/// the flags being mutually exclusive cannot rest on argument parsing alone.
#[test]
fn refuses_both_person_filters_at_once() {
    let error = ask(&Request::Search(SearchRequest {
        query: "e".into(),
        with: Some("+13105551234".into()),
        from: Some("+13105551234".into()),
        names: Some(false),
        ..Default::default()
    }))
    .unwrap_err()
    .to_string();
    assert!(error.contains("--with and --from"), "{error}");
}

// -------------------------------------------------------------- resolve

#[test]
fn names_one_conversation_for_send_to_address() {
    let chat = ask(&Request::Resolve(ResolveRequest {
        chat: "Ship Room".into(),
        names: Some(false),
    }))
    .unwrap();
    assert_eq!(chat["guid"], serde_json::json!("iMessage;+;chat9"));
}

#[test]
fn reports_a_name_that_matches_nothing_rather_than_guessing() {
    let error = ask(&Request::Resolve(ResolveRequest {
        chat: "nothing-matches".into(),
        names: Some(false),
    }))
    .unwrap_err()
    .to_string();
    assert!(error.contains("no chat matching"), "{error}");
}

// ----------------------------------------------------------------- send
//
// Nothing here enables sending. The gate being shut is what is testable without
// texting a real person; the open path is covered by --dry-run and by the
// Automation grant macOS enforces underneath it.

/// The guard the rest of this section rests on. If this ever fails, the send
/// tests below would be driving Messages for real.
#[test]
fn sending_is_off_for_every_test_here() {
    let harness = harness();
    assert!(!harness.config.exists(), "the test config must not exist");
    assert!(!msg::daemon::config::read_config(&harness.config).send);
}

fn attempt_send(chat: &str) -> msg::Error {
    ask(&Request::Send(SendRequest {
        chat: chat.into(),
        body: Some("hi".into()),
        names: Some(false),
        ..Default::default()
    }))
    .unwrap_err()
}

#[test]
fn refuses_when_the_config_key_is_absent_and_names_the_key() {
    let error = attempt_send("iMessage;-;+13105551234").to_string();
    assert!(error.contains("send = true"), "{error}");
    assert!(error.contains("sending is disabled"), "{error}");
}

#[test]
fn reports_the_refusal_as_its_own_code_not_a_generic_failure() {
    assert!(matches!(
        attempt_send("iMessage;-;+13105551234"),
        msg::Error::SendDisabled(_)
    ));
}

#[test]
fn refuses_before_it_would_have_to_read_the_database() {
    // A guid needs no lookup, so this would still be refused on a daemon that
    // holds Automation and no Full Disk Access at all.
    let error = attempt_send("iMessage;+;chat9").to_string();
    assert!(error.contains("sending is disabled"), "{error}");
}

// --------------------------------------------------------------- status

#[test]
fn reports_the_database_it_is_reading() {
    let status = ask(&Request::Status(Empty {})).unwrap();
    assert_eq!(
        status["database"].as_str().unwrap(),
        harness().database.to_string_lossy()
    );
    assert_eq!(
        status["protocol"],
        serde_json::json!(msg::daemon::protocol::PROTOCOL_VERSION)
    );
    // Rowids only climb and the watch tests add more, so this is a floor.
    assert!(status["messageCount"].as_i64().unwrap() >= 5);
    assert_eq!(status["version"], serde_json::json!(msg::VERSION));
}

#[test]
fn resolves_handles_without_reading_the_testers_contacts() {
    let reply = ask(&Request::Contacts(ContactsRequest {
        handles: vec!["+13105551234".into()],
    }))
    .unwrap();
    assert_eq!(
        reply["resolved"][0]["handle"],
        serde_json::json!("+13105551234")
    );
}

// ------------------------------------------------------------- protocol

#[test]
fn refuses_a_version_it_does_not_speak() {
    let frame = raw("{\"cmd\":\"status\",\"v\":99}\n");
    assert_eq!(frame["type"], serde_json::json!("error"));
    assert_eq!(frame["code"], serde_json::json!("version"));
}

/// A daemon older than a command must not answer `result` with no value, which
/// the CLI reads a field off and crashes on. The version check is what prevents
/// that, so adding a request means bumping the version — and this is the
/// backstop for forgetting to.
#[test]
fn refuses_a_command_it_does_not_know_rather_than_answering_nothing() {
    let frame = raw(&format!(
        "{{\"cmd\":\"nonesuch\",\"v\":{PROTOCOL_VERSION}}}\n"
    ));
    assert_eq!(frame["type"], serde_json::json!("error"));
    assert_eq!(frame["code"], serde_json::json!("error"));
    assert!(
        frame["message"]
            .as_str()
            .unwrap()
            .contains("does not understand"),
        "{frame}"
    );
}

#[test]
fn refuses_a_request_that_is_not_json() {
    let frame = raw("this is not json\n");
    assert_eq!(frame["type"], serde_json::json!("error"));
    assert_eq!(frame["message"], serde_json::json!("malformed request"));
}

/// A known command missing a required field is a different failure from an
/// unknown command, and saying so is what the TypeScript build could not.
#[test]
fn reports_a_malformed_known_request_as_such() {
    let frame = raw(&format!("{{\"cmd\":\"read\",\"v\":{PROTOCOL_VERSION}}}\n"));
    assert_eq!(frame["type"], serde_json::json!("error"));
    let message = frame["message"].as_str().unwrap();
    assert!(
        message.contains("could not read this `read` request"),
        "{message}"
    );
}

/// The envelope this crate writes is the one the daemon reads, both ways.
#[test]
fn round_trips_its_own_envelope() {
    let line = envelope(&Request::Status(Empty {})).unwrap();
    let frame = raw(&line);
    assert_eq!(frame["type"], serde_json::json!("result"));
}

// ---------------------------------------------------------------- watch

/// Subscribe, then run `write` once the watermark is recorded, then collect
/// `wanted` messages matching `mine` or give up.
///
/// The filter is not incidental: these tests share one daemon and run
/// concurrently, so a watcher sees every other test's inserts too. Each takes
/// only the rowids it was handed by `next_rowids`.
fn watch_collect(
    wanted: usize,
    mine: impl Fn(i64) -> bool,
    write: impl FnOnce(),
) -> Vec<serde_json::Value> {
    let mut stream = UnixStream::connect(&harness().socket).unwrap();
    stream
        .write_all(
            envelope(&Request::Watch(WatchRequest {
                names: Some(false),
                ..Default::default()
            }))
            .unwrap()
            .as_bytes(),
        )
        .unwrap();

    // Subscribing is what records the watermark, so the write has to come after
    // the daemon has processed the request.
    std::thread::sleep(Duration::from_millis(300));
    write();

    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let reader = BufReader::new(stream);
    let mut received = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let frame: serde_json::Value = serde_json::from_str(&line).unwrap();
        if frame["type"] == serde_json::json!("item")
            && frame["value"]["rowid"].as_i64().is_some_and(&mine)
        {
            received.push(frame["value"].clone());
        }
        if received.len() >= wanted || Instant::now() > deadline {
            break;
        }
    }
    received
}

#[test]
fn streams_a_message_that_arrives_after_the_client_subscribed() {
    let rowid = next_rowids(1).start;
    let received = watch_collect(
        1,
        |seen| seen == rowid,
        || {
            let writer = Connection::open(&harness().database).unwrap();
            insert(&writer, rowid, "later", "just landed", 0, 1, 0, at(500), 1);
        },
    );
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["body"], serde_json::json!("just landed"));
    assert_eq!(received[0]["rowid"], serde_json::json!(rowid));
}

#[test]
fn drains_a_burst_larger_than_one_batch_oldest_first() {
    let target = 250usize;
    let rowids = next_rowids(target as i64);
    let start = rowids.start;
    let owned = start..start + target as i64;
    let received = watch_collect(
        target,
        move |seen| owned.contains(&seen),
        move || {
            let writer = Connection::open(&harness().database).unwrap();
            for (index, rowid) in (start..start + target as i64).enumerate() {
                insert(
                    &writer,
                    rowid,
                    &format!("burst-{rowid}"),
                    "burst",
                    0,
                    1,
                    0,
                    at(1_000 + index as i64),
                    1,
                );
            }
        },
    );

    let mine: Vec<i64> = received
        .iter()
        .map(|m| m["rowid"].as_i64().unwrap())
        .collect();
    assert_eq!(mine.len(), target, "dropped some of the burst");
    // A batch is capped below this, so arriving in order proves the drain walked
    // forwards rather than jumping to the newest and stranding the rest.
    let mut sorted = mine.clone();
    sorted.sort_unstable();
    assert_eq!(mine, sorted, "delivered out of order");
    sorted.dedup();
    assert_eq!(sorted.len(), target, "delivered a duplicate");
}

/// A socket that accepts and then says nothing is neither refused nor slow, and
/// it is what makes a caller's timeout the only thing that ends the wait.
///
/// `msg daemon install` probes a daemon it has just bootstrapped, so it is the
/// caller most likely to meet one. Without a caller-chosen deadline it inherits
/// the general thirty-second read timeout and the install looks hung.
#[test]
fn a_socket_that_accepts_and_never_answers_gives_up_on_the_callers_schedule() {
    use std::os::unix::net::UnixListener;

    let directory = msg::db::temporary_directory("msg-silent-").unwrap();
    let path = directory.join("silent.sock");
    let listener = UnixListener::bind(&path).unwrap();

    // Accept, hold the connection open, and never write a byte. Holding it is
    // the point: dropping it would send EOF and end the read straight away.
    let held = std::thread::spawn(move || {
        let mut kept = Vec::new();
        while let Ok((stream, _)) = listener.accept() {
            kept.push(stream);
            if kept.len() == 2 {
                break;
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    });

    let short = Duration::from_millis(300);
    let started = Instant::now();
    let stream = connect_daemon_within(Some(&path), short).unwrap();
    let outcome = request(stream, &Request::Status(Empty {}));
    let waited = started.elapsed();

    assert!(outcome.is_err(), "a silent socket somehow answered");
    assert!(
        waited < Duration::from_secs(2),
        "waited {waited:?} on a {short:?} deadline, so the timeout was ignored"
    );

    // And the general connect really does carry the long one, which is why the
    // install cannot just use it.
    let long = connect_daemon(Some(&path)).unwrap();
    assert_eq!(
        long.read_timeout().unwrap(),
        Some(Duration::from_secs(30)),
        "the default deadline changed; the install's own is sized against it"
    );
    drop(long);

    held.join().ok();
    std::fs::remove_dir_all(&directory).ok();
}

/// The socket is the whole of the access control the daemon has or wants (§5).
#[test]
fn the_socket_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&harness().socket)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "socket mode {:o}", mode & 0o777);
    let directory = std::fs::metadata(&harness().directory)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(directory & 0o777, 0o700);
}

/// The streamed path, over a real socket: bytes the client could not have read
/// itself, arriving in chunks and landing as one whole file.
///
/// The payload is deliberately larger than one chunk and not a multiple of it,
/// because a chunked transfer that is off by a boundary is exactly the bug this
/// is here to catch — and a smaller file would take the single-frame path
/// through the same code without exercising any of it.
#[test]
fn it_streams_an_attachment_larger_than_one_chunk() {
    let harness = harness();
    let source = harness.directory.join("big.bin");
    let payload: Vec<u8> = (0..(9 * 1024 * 1024 + 7u32))
        .map(|i| (i % 251) as u8)
        .collect();
    std::fs::write(&source, &payload).unwrap();

    let db = Connection::open(&harness.database).unwrap();
    db.execute(
        "INSERT INTO attachment (ROWID, guid, filename, mime_type, transfer_name,
             total_bytes, is_sticker, hide_attachment)
         VALUES (91, 'a91', ?, 'application/octet-stream', 'big.bin', ?, 0, 0)",
        rusqlite::params![source.to_string_lossy(), payload.len() as i64],
    )
    .unwrap();
    drop(db);

    let stream = UnixStream::connect(&harness.socket).unwrap();
    let mut head = None;
    let mut chunks = 0usize;
    let mut written: Vec<u8> = Vec::new();
    let reply = msg::daemon::client::save(stream, &SaveRequest { id: 91 }, |part| {
        match part {
            SavePart::Head {
                name, total_bytes, ..
            } => head = Some((name, total_bytes)),
            SavePart::Chunk { base64 } => {
                chunks += 1;
                written.extend_from_slice(
                    &base64::engine::general_purpose::STANDARD
                        .decode(base64)
                        .unwrap(),
                );
            }
        }
        Ok(())
    })
    .unwrap();

    assert_eq!(head, Some(("big.bin".to_string(), payload.len() as i64)));
    assert_eq!(reply.bytes, payload.len() as i64);
    assert_eq!(written.len(), payload.len(), "wrong number of bytes");
    assert!(written == payload, "bytes differ");
    // Counted rather than inferred from the payload size, which is two
    // constants agreeing with each other. Raising the chunk size to 16MB would
    // leave every other assertion here passing while the chunked path stopped
    // being exercised at all.
    assert!(chunks >= 3, "{chunks} chunks crossed the wire");
}

/// An id that is not there is an error, not an empty file.
#[test]
fn it_refuses_an_attachment_id_that_does_not_exist() {
    let harness = harness();
    let stream = UnixStream::connect(&harness.socket).unwrap();
    let error = msg::daemon::client::save(stream, &SaveRequest { id: 999_999 }, |_| Ok(()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("no attachment 999999"), "{error}");
}

/// Protocol 5 should spell a field one way, not two.
///
/// `rename_all` on an enum renames its *variants*; the fields inside a struct
/// variant need `rename_all_fields`. Both ends here are the same Rust type, so
/// nothing breaks either way and only a look at the wire catches it.
#[test]
fn a_save_frame_spells_its_fields_the_way_every_other_frame_does() {
    let head = serde_json::to_value(SavePart::Head {
        name: "a.png".into(),
        mime_type: Some("image/png".into()),
        total_bytes: 7,
    })
    .unwrap();
    assert_eq!(head["part"], serde_json::json!("head"));
    assert_eq!(head["mimeType"], serde_json::json!("image/png"));
    assert_eq!(head["totalBytes"], serde_json::json!(7));
    assert!(head.get("mime_type").is_none(), "{head}");
    assert!(head.get("total_bytes").is_none(), "{head}");
}

/// A file that is there and refused is not a file that is gone.
#[test]
fn an_attachment_it_may_not_read_is_not_reported_as_missing() {
    let harness = harness();
    let refused = harness.directory.join("refused.bin");
    std::fs::write(&refused, b"secret").unwrap();
    std::fs::set_permissions(
        &refused,
        std::os::unix::fs::PermissionsExt::from_mode(0o000),
    )
    .unwrap();

    let db = Connection::open(&harness.database).unwrap();
    db.execute(
        "INSERT INTO attachment (ROWID, guid, filename, transfer_name, total_bytes)
         VALUES (92, 'a92', ?, 'refused.bin', 6)",
        rusqlite::params![refused.to_string_lossy()],
    )
    .unwrap();
    drop(db);

    let stream = UnixStream::connect(&harness.socket).unwrap();
    let error = msg::daemon::client::save(stream, &SaveRequest { id: 92 }, |_| Ok(())).unwrap_err();
    let said = error.to_string();
    assert!(
        !said.contains("gone"),
        "a refused file is not a missing one: {said}"
    );
    // Exit 2 is the documented "the data is there, the grant is not".
    assert!(
        matches!(error, msg::Error::AccessDenied(_)),
        "{error:?} should be AccessDenied"
    );
    // What the caller is *told*, not only which variant was raised. Both
    // assertions above pass on a message that never mentions the attachment,
    // which is how a wire that replaced the words with a fixed sentence about
    // the database went unnoticed.
    assert!(said.contains("92"), "should name the attachment: {said}");
    assert!(
        !said.contains("cannot read the Messages database"),
        "msgd read the database to resolve the rowid, so this is untrue: {said}"
    );

    std::fs::set_permissions(
        &refused,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
}

/// A daemon that cannot open the database still says the right thing.
///
/// The wire used to substitute a fixed sentence for every `AccessDenied`, and
/// that substitution is what kept this text correct. Now `open_database` puts it
/// in directly, so this is the assertion holding it in place: the daemon must
/// not tell a client to "install the daemon", which is the wording the CLI uses
/// when *it* is the one refused.
#[test]
fn a_daemon_that_cannot_read_the_database_says_so_in_its_own_words() {
    let directory = msg::db::temporary_directory("msg-denied-").unwrap();
    let database = directory.join("chat.db");
    build_fixture(&database);
    std::fs::set_permissions(
        &database,
        std::os::unix::fs::PermissionsExt::from_mode(0o000),
    )
    .unwrap();

    let socket = directory.join("msgd.sock");
    let daemon = Daemon::new(DaemonOptions {
        db_path: Some(database.to_string_lossy().into_owned()),
        config_path: Some(directory.join("config-that-does-not-exist.toml")),
    });
    daemon.listen(Some(socket.clone())).unwrap();

    let stream = UnixStream::connect(&socket).unwrap();
    let error = request(stream, &Request::Chats(ChatsRequest::default()))
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("Grant Full Disk Access to msgd"),
        "the daemon's own advice: {error}"
    );
    assert!(
        !error.contains("Install the daemon"),
        "that is the CLI's advice, not the daemon's: {error}"
    );

    std::fs::set_permissions(
        &database,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
}
