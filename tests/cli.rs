//! The `msg` binary as a user meets it: what it prints, and what it exits with.
//!
//! Every test drives a fixture through `--db`, so nothing here reads the
//! Messages or Contacts database of whoever is running them, and `--no-names`
//! keeps the Contacts lookup out of it entirely.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rusqlite::Connection;

fn fixture() -> PathBuf {
    use std::sync::OnceLock;
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let directory = msg::db::temporary_directory("msg-cli-").unwrap();
        let path = directory.join("chat.db");
        build(&path);
        path
    })
    .clone()
}

fn build(path: &Path) {
    let db = Connection::open(path).unwrap();
    db.execute_batch(
        "
        CREATE TABLE handle (rowid INTEGER PRIMARY KEY, id TEXT);
        CREATE TABLE chat (rowid INTEGER PRIMARY KEY, guid TEXT, chat_identifier TEXT,
          display_name TEXT, is_filtered INTEGER DEFAULT 0);
        CREATE TABLE message (rowid INTEGER PRIMARY KEY, guid TEXT, text TEXT,
          attributedBody BLOB, is_from_me INTEGER DEFAULT 0, handle_id INTEGER,
          associated_message_type INTEGER DEFAULT 0, date INTEGER, service TEXT,
        thread_originator_guid TEXT, thread_originator_part TEXT);
        CREATE TABLE chat_message_join (chat_id INTEGER, message_id INTEGER,
          message_date INTEGER DEFAULT 0);
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
        INSERT INTO handle (rowid, id) VALUES (1, '+13105551234'), (2, 'dana@example.com');
        INSERT INTO chat (rowid, guid, chat_identifier, display_name) VALUES
          (1, 'iMessage;-;+13105551234', '+13105551234', ''),
          (2, 'iMessage;+;chat9', 'chat9', 'Ship Room'),
          (3, 'iMessage;-;dana@example.com', 'dana@example.com', '');
        INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (1, 1), (2, 1), (3, 2);
        INSERT INTO message (rowid, guid, text, is_from_me, handle_id, date, service)
          VALUES (1, 'm1', 'are you around later', 0, 1, 790000000000000000, 'iMessage'),
                 (2, 'm2', 'after 6, yeah', 1, 1, 790000060000000000, 'iMessage'),
                 (3, 'm3', 'works, see you then', 0, 1, 790000120000000000, 'iMessage'),
                 (4, 'm4', 'we started at six, is that art deco', 0, 2, 790000180000000000, 'iMessage'),
                 (5, 'm5', 'the apartment above ours', 0, 2, 790000240000000000, 'iMessage');
        INSERT INTO chat_message_join (chat_id, message_id, message_date)
          VALUES (1, 1, 790000000000000000),
                 (1, 2, 790000060000000000),
                 (1, 3, 790000120000000000),
                 (3, 4, 790000180000000000),
                 (3, 5, 790000240000000000);
        ",
    )
    .unwrap();
}

/// Run the binary with a socket path that nothing is listening on, so the
/// direct path is taken and a stray daemon on the developer's machine cannot
/// change the answer.
fn msg(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_msg"))
        .args(args)
        .env("MSG_SOCKET", "/tmp/msg-tests-nothing-here.sock")
        .env("MSG_DB", fixture())
        .output()
        .expect("run msg")
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("an exit status")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn it_lists_conversations() {
    let output = msg(&["--no-names", "chats"]);
    assert_eq!(code(&output), 0);
    let text = stdout(&output);
    assert!(text.contains("+13105551234"), "{text}");
    assert!(text.contains("direct"), "{text}");
}

#[test]
fn it_reads_a_conversation() {
    let output = msg(&["--no-names", "chat", "1"]);
    assert_eq!(code(&output), 0);
    assert!(stdout(&output).contains("are you around later"));
}

#[test]
fn it_emits_json_when_asked() {
    let output = msg(&["--no-names", "chat", "1", "--json"]);
    assert_eq!(code(&output), 0);
    let value: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(value["chat"]["rowid"], serde_json::json!(1));
    assert_eq!(
        value["messages"][0]["body"],
        serde_json::json!("are you around later")
    );
}

fn msg_against(db: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_msg"))
        .args(["chats"])
        .env("MSG_SOCKET", "/tmp/msg-tests-nothing-here.sock")
        .env("MSG_DB", db)
        .output()
        .unwrap()
}

/// Exit 2 is the documented status for "the data is there, the grant is not",
/// and the README tells people to branch on it.
#[test]
fn a_database_that_is_not_there_exits_two() {
    let output = msg_against(Path::new("/nonexistent/chat.db"));
    assert_eq!(code(&output), 2);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no Messages database at"), "{stderr}");
}

/// The shape TCC actually produces: the file is there and cannot be read. This
/// is the path that broke once, when the snapshot fallback raised a raw errno
/// instead of the explanation, and nothing caught it because the development
/// machine held the grant.
#[test]
fn a_database_it_cannot_read_exits_two_with_the_explanation() {
    use std::os::unix::fs::PermissionsExt;

    let directory = msg::db::temporary_directory("msg-denied-").unwrap();
    let path = directory.join("chat.db");
    build(&path);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let output = msg_against(&path);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).ok();
    std::fs::remove_dir_all(&directory).ok();

    assert_eq!(code(&output), 2);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot read"), "{stderr}");
    assert!(stderr.contains("msg daemon install"), "{stderr}");
}

#[test]
fn an_ordinary_failure_exits_one() {
    let output = msg(&["--no-names", "chat", "nothing-matches-this"]);
    assert_eq!(code(&output), 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("no chat matching"));
}

/// clap exits 2 for a usage error by default, which would collide with the
/// permission status above and make a mistyped flag look like a missing grant.
#[test]
fn a_usage_error_exits_one_rather_than_claiming_the_permission_status() {
    for args in [
        vec!["chat", "1", "-n", "0"],
        vec!["chat", "1", "-n", "-4"],
        vec!["chat", "1", "-n", "many"],
        vec!["nosuchcommand"],
        vec!["chat"],
        vec!["watch", "--interval", "0"],
    ] {
        let output = msg(&args);
        assert_eq!(code(&output), 1, "for {args:?}");
    }
}

#[test]
fn help_and_version_are_successes() {
    for args in [vec!["--help"], vec!["--version"], vec!["daemon", "--help"]] {
        let output = msg(&args);
        assert_eq!(code(&output), 0, "for {args:?}");
        assert!(!stdout(&output).is_empty(), "for {args:?}");
    }
    assert!(stdout(&msg(&["--version"])).contains(msg::VERSION));
}

/// Sending needs Automation, which lives in the daemon. With none listening the
/// CLI must refuse rather than reach for Messages itself (§7).
#[test]
fn sending_without_a_daemon_refuses_rather_than_trying() {
    let output = msg(&["--no-names", "send", "1", "hi"]);
    assert_eq!(code(&output), 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("sending needs the daemon"), "{stderr}");
}

/// `--dry-run` resolves the conversation and prints, and must not need a daemon
/// — it is what AGENTS.md tells everyone to verify with.
#[test]
fn dry_run_resolves_and_prints_without_sending() {
    let output = msg(&[
        "--no-names",
        "send",
        "Ship Room",
        "hello",
        "there",
        "--dry-run",
    ]);
    assert_eq!(code(&output), 0);
    assert_eq!(stdout(&output), "would send to Ship Room: hello there\n");
}

/// The flag reaches the window, the gutter marks the hit, and the messages
/// around it come back — the whole path, through the binary rather than the
/// library, since that is where the argument parsing lives.
#[test]
fn context_flags_show_the_conversation_around_a_hit() {
    let bare = msg(&["--no-names", "search", "after 6"]);
    assert_eq!(code(&bare), 0);
    let bare = String::from_utf8_lossy(&bare.stdout);
    assert_eq!(bare.lines().count(), 1, "{bare}");
    // Nothing gains a gutter when no context was asked for.
    assert!(!bare.starts_with("> "), "{bare}");

    let output = msg(&["--no-names", "search", "after 6", "-C", "1"]);
    assert_eq!(code(&output), 0);
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = text.lines().collect();

    assert_eq!(lines.len(), 3, "{text}");
    assert!(lines[0].contains("are you around later"), "{text}");
    assert!(lines[1].contains("after 6, yeah"), "{text}");
    assert!(lines[2].contains("works, see you then"), "{text}");

    // Only the hit is marked, and the context lines up under it.
    assert!(lines[1].starts_with("> "), "{text}");
    assert!(
        lines[0].starts_with("  ") && lines[2].starts_with("  "),
        "{text}"
    );
}

/// The rule `search-boundaries.md §2` asks for, through the whole search path:
/// a hit starts where a word starts, and nothing is asserted about where it
/// ends. The matching body is the plan's own fourth case — its first `art` is
/// interior to `started` and only its second begins a word — so this also pins
/// the any-occurrence quantifier, which a first-occurrence-only check fails.
#[test]
fn a_search_hit_starts_where_a_word_starts() {
    let output = msg(&["--no-names", "search", "art"]);
    assert_eq!(code(&output), 0);
    let text = stdout(&output);
    assert!(text.contains("art deco"), "{text}");
    assert!(!text.contains("apartment"), "{text}");

    // Asymmetric on purpose: a prefix still matches, so `start` finds
    // `started` — whole-word matching would be the wrong trade.
    let output = msg(&["--no-names", "search", "start"]);
    assert_eq!(code(&output), 0);
    assert!(stdout(&output).contains("we started at six"));
}

#[test]
fn a_query_matching_nothing_says_so_rather_than_printing_nothing() {
    let output = msg(&["--no-names", "search", "zzzznotpresent"]);
    assert_eq!(code(&output), 0);
    assert_eq!(stdout(&output), "no messages found\n");
}

/// The README tells people to install by symlinking `build/msg` onto their PATH,
/// and `current_exe` on macOS reports the symlink rather than its target. So
/// `msg daemon install` has to look beside the *real* binary, not beside the
/// link — which it did not, and which only showed up on a real install.
#[test]
fn install_finds_the_bundle_beside_the_real_binary_not_the_symlink() {
    let real = PathBuf::from(env!("CARGO_BIN_EXE_msg"));
    let beside = real.parent().unwrap().join("msgd.app");
    if beside.exists() {
        // Would install for real. Nothing in this suite may do that.
        eprintln!("skipping: {} exists", beside.display());
        return;
    }

    let directory = msg::db::temporary_directory("msg-link-").unwrap();
    let link = directory.join("msg");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let output = Command::new(&link)
        .args(["daemon", "install"])
        .env("MSG_SOCKET", "/tmp/msg-tests-nothing-here.sock")
        .output()
        .unwrap();
    std::fs::remove_dir_all(&directory).ok();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(code(&output), 1, "{stderr}");
    assert!(
        stderr.contains(&beside.to_string_lossy().to_string()),
        "looked in the wrong place: {stderr}"
    );
    assert!(
        !stderr.contains(&directory.to_string_lossy().to_string()),
        "resolved to the symlink's directory: {stderr}"
    );
}

/// A database of this test's own, because the shared fixture is a singleton and
/// a test that inserts into it changes what every other test reads.
fn private_fixture(prefix: &str) -> (PathBuf, PathBuf) {
    let directory = msg::db::temporary_directory(prefix).unwrap();
    let path = directory.join("chat.db");
    build(&path);
    (directory, path)
}

fn msg_in(db: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_msg"))
        .args(args)
        .env("MSG_SOCKET", "/tmp/msg-tests-nothing-here.sock")
        .env("MSG_DB", db)
        .output()
        .expect("run msg")
}

/// The end a user actually meets: an id printed beside an attachment, and a
/// command that turns that id into a file they can open.
///
/// Driven through `--db`, so this is the direct path — the one that runs when
/// no daemon is listening. `tests/daemon.rs` covers the streamed path.
#[test]
fn it_saves_an_attachment_by_the_id_it_printed() {
    let (directory, database) = private_fixture("msg-save-");
    let source = directory.join("original.bin");
    // Larger than one chunk would be if this went through the daemon, and not a
    // round number, so a truncated or padded copy fails rather than passes.
    let payload: Vec<u8> = (0..5_000_003u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&source, &payload).unwrap();

    let db = Connection::open(&database).unwrap();
    db.execute(
        "INSERT INTO attachment (ROWID, guid, filename, mime_type, transfer_name,
             total_bytes, is_sticker, hide_attachment)
         VALUES (77, 'a77', ?, 'application/octet-stream', 'holiday.bin', ?, 0, 0)",
        rusqlite::params![source.to_string_lossy(), payload.len() as i64],
    )
    .unwrap();
    db.execute(
        "INSERT INTO message (rowid, guid, text, is_from_me, handle_id, date, service)
         VALUES (77, 'm77', char(65532), 0, 1, 790000000000000000, 'iMessage')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO chat_message_join (chat_id, message_id, message_date)
         VALUES (1, 77, 790000000000000000)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO message_attachment_join (message_id, attachment_id) VALUES (77, 77)",
        [],
    )
    .unwrap();
    drop(db);

    // The id has to be discoverable from the output, or it cannot be used.
    let chat = msg_in(&database, &["chat", "1", "--no-names"]);
    assert!(
        stdout(&chat).contains("[#77 holiday.bin,"),
        "{}",
        stdout(&chat)
    );

    let into = directory.join("out");
    let saved = msg_in(&database, &["save", "77", "--to", into.to_str().unwrap()]);
    assert_eq!(
        code(&saved),
        0,
        "{}",
        String::from_utf8_lossy(&saved.stderr)
    );

    let written = into.join("holiday.bin");
    assert_eq!(std::fs::read(&written).unwrap(), payload, "bytes differ");
    assert!(stdout(&saved).contains("holiday.bin"), "{}", stdout(&saved));

    // A second save refuses rather than replacing what is already there.
    let again = msg_in(&database, &["save", "77", "--to", into.to_str().unwrap()]);
    assert_eq!(code(&again), 1);
    assert!(
        String::from_utf8_lossy(&again.stderr).contains("--force"),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );

    // And with --force it does replace it, leaving no temporary behind.
    let forced = msg_in(
        &database,
        &["save", "77", "--to", into.to_str().unwrap(), "--force"],
    );
    assert_eq!(
        code(&forced),
        0,
        "{}",
        String::from_utf8_lossy(&forced.stderr)
    );
    let strays: Vec<_> = std::fs::read_dir(&into)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".msg-save-"))
        .collect();
    assert!(strays.is_empty(), "left behind {strays:?}");
}

/// An attachment Messages recorded but no longer has on disk.
#[test]
fn it_reports_an_attachment_whose_file_is_gone() {
    let (_directory, database) = private_fixture("msg-gone-");
    let db = Connection::open(&database).unwrap();
    db.execute(
        "INSERT INTO attachment (ROWID, guid, filename, transfer_name, total_bytes)
         VALUES (78, 'a78', '~/nowhere/gone.jpg', 'gone.jpg', 10)",
        [],
    )
    .unwrap();
    drop(db);

    let directory = msg::db::temporary_directory("msg-gone-out-").unwrap();
    let output = msg_in(
        &database,
        &["save", "78", "--to", directory.to_str().unwrap()],
    );
    assert_eq!(code(&output), 1);
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("its file is gone"), "{error}");
    // Nothing at all should be left in the destination.
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
}

/// An explicit zero beats `-C`, the way it does in grep.
///
/// `-C 2 -B 0` asks for two messages after each hit and none before, and the
/// difference between "not given" and "given as zero" is the whole of what
/// makes that expressible.
#[test]
fn an_explicit_zero_width_beats_the_shorthand() {
    let output = msg(&["--no-names", "search", "after 6", "-C", "1", "-B", "0"]);
    assert_eq!(code(&output), 0);
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = text.lines().collect();

    // The hit and the one after it, and nothing before.
    assert_eq!(lines.len(), 2, "{text}");
    assert!(
        lines[0].starts_with("> ") && lines[0].contains("after 6"),
        "{text}"
    );
    assert!(
        lines[1].starts_with("  ") && lines[1].contains("works, see"),
        "{text}"
    );
}
