//! The daemon process, started by launchd and resident for as long as the user
//! is logged in. It holds the Full Disk Access grant; the CLI holds none.

use msg::VERSION;
use msg::daemon::server::{Daemon, DaemonOptions};

fn log(message: &str) {
    eprintln!(
        "msgd {} {message}",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    );
}

fn main() -> std::process::ExitCode {
    let daemon = Daemon::new(DaemonOptions::default());

    // Touch the database before anyone asks. Under launchd the *failure* is the
    // point: a denied access is what creates this bundle's entry in the Full
    // Disk Access list, and there is no other way to make it appear there to be
    // switched on (docs/projects/all/daemon-and-permissions.md §9).
    match daemon.probe_database() {
        Ok(()) => log("database readable"),
        Err(error) => {
            let first = error.to_string();
            let first = first.lines().next().unwrap_or("unknown error").to_string();
            log(&format!("database unreadable: {first}"));
        }
    }

    match daemon.listen(None) {
        Ok(path) => log(&format!(
            "msgd {VERSION} listening on {} as pid {}",
            path.display(),
            std::process::id()
        )),
        Err(error) => {
            log(&format!("fatal: {error}"));
            return std::process::ExitCode::FAILURE;
        }
    }

    // launchd stops this with SIGTERM, whose default action is what we want:
    // the socket is unlinked and rebound on the next start, and no watcher
    // outlives the process anyway.
    loop {
        std::thread::park();
    }
}
