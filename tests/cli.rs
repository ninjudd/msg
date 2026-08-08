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
          associated_message_type INTEGER DEFAULT 0, date INTEGER, service TEXT);
        CREATE TABLE chat_message_join (chat_id INTEGER, message_id INTEGER);
        CREATE TABLE chat_handle_join (chat_id INTEGER, handle_id INTEGER);
        INSERT INTO handle (rowid, id) VALUES (1, '+13105551234');
        INSERT INTO chat (rowid, guid, chat_identifier, display_name) VALUES
          (1, 'iMessage;-;+13105551234', '+13105551234', ''),
          (2, 'iMessage;+;chat9', 'chat9', 'Ship Room');
        INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (1, 1), (2, 1);
        INSERT INTO message (rowid, guid, text, is_from_me, handle_id, date, service)
          VALUES (1, 'm1', 'are you around later', 0, 1, 790000000000000000, 'iMessage');
        INSERT INTO chat_message_join (chat_id, message_id) VALUES (1, 1);
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
    let output = msg(&["--no-names", "read", "1"]);
    assert_eq!(code(&output), 0);
    assert!(stdout(&output).contains("are you around later"));
}

#[test]
fn it_emits_json_when_asked() {
    let output = msg(&["--no-names", "read", "1", "--json"]);
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
    let output = msg(&["--no-names", "read", "nothing-matches-this"]);
    assert_eq!(code(&output), 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("no chat matching"));
}

/// clap exits 2 for a usage error by default, which would collide with the
/// permission status above and make a mistyped flag look like a missing grant.
#[test]
fn a_usage_error_exits_one_rather_than_claiming_the_permission_status() {
    for args in [
        vec!["read", "1", "-n", "0"],
        vec!["read", "1", "-n", "-4"],
        vec!["read", "1", "-n", "many"],
        vec!["nosuchcommand"],
        vec!["read"],
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
