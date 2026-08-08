//! `msg`, the command a person runs. It holds no permission of its own: reads
//! go through the daemon when one is listening, and straight to the database
//! when it is not.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use base64::Engine;
use clap::{Args, Parser, Subcommand};

use msg::daemon::install::{
    Grant, built_bundle, bundle_path, install, is_loaded, log_path, open_automation,
    open_full_disk_access, plist_path, signature_of, uninstall,
};
use msg::daemon::protocol::{Attachment, socket_path};
use msg::daemon::server::{Daemon, DaemonOptions};
use msg::db::human_bytes;
use msg::format::{render_chats, render_messages, to_json};
use msg::source::{
    ChatsQuery, ReadQuery, SearchQuery, SendQuery, Source, WatchQuery, daemon_status,
    daemon_status_within, open_source,
};
use msg::{Error, VERSION};

#[derive(Parser)]
#[command(
    name = "msg",
    version = VERSION,
    about = "read and send iMessages from the command line"
)]
struct Cli {
    /// path to chat.db (defaults to the Messages database)
    #[arg(long, global = true)]
    db: Option<String>,

    /// show raw handles instead of looking up contact names
    #[arg(long = "no-names", global = true)]
    no_names: bool,

    /// include conversations Messages filters as unknown senders
    #[arg(long, global = true)]
    unknown: bool,

    #[command(subcommand)]
    command: Command,
}

impl Cli {
    fn names(&self) -> bool {
        !self.no_names
    }
}

#[derive(Subcommand)]
enum Command {
    /// list conversations by most recent activity
    Chats {
        /// filter by name, handle, or identifier
        query: Option<String>,
        #[arg(short = 'n', long, default_value_t = 30, value_parser = positive)]
        limit: i64,
        #[arg(long)]
        json: bool,
    },
    /// print a conversation
    Read {
        /// chat rowid, handle, or name substring
        chat: String,
        #[arg(short = 'n', long, default_value_t = 50, value_parser = positive)]
        limit: i64,
        /// only messages after a duration like 2h or a date
        #[arg(long)]
        since: Option<String>,
        /// include reactions
        #[arg(long)]
        tapbacks: bool,
        #[arg(long)]
        json: bool,
    },
    /// search message bodies
    Search {
        /// text to look for
        query: String,
        #[arg(short = 'n', long, default_value_t = 25, value_parser = positive)]
        limit: i64,
        /// restrict to one conversation
        #[arg(short = 'c', long)]
        chat: Option<String>,
        /// one person's conversation with you, across every chat
        #[arg(short = 'w', long)]
        with: Option<String>,
        /// only what one person sent, across every chat
        #[arg(short = 'f', long, conflicts_with = "with")]
        from: Option<String>,
        /// only messages after a duration like 30d or a date
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// follow new messages as they arrive
    Watch {
        /// restrict to one conversation
        #[arg(short = 'c', long)]
        chat: Option<String>,
        /// how often to poll without a daemon
        #[arg(long, default_value_t = 3, value_parser = positive_seconds)]
        interval: u64,
        /// include reactions
        #[arg(long)]
        tapbacks: bool,
        /// emit JSON lines
        #[arg(long)]
        json: bool,
    },
    /// look up the name behind a handle
    Contacts {
        /// phone numbers or email addresses
        handles: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// send a message to a conversation
    Send(SendArgs),
    /// write an attachment to a directory, by the id shown beside it
    Save {
        /// attachment id, as printed in a message body
        id: i64,
        /// where to write it
        #[arg(long, default_value = ".")]
        to: PathBuf,
        /// replace a file of the same name
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// manage the background reader that holds Full Disk Access
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
}

#[derive(Args)]
struct SendArgs {
    /// chat rowid, handle, or name substring
    chat: String,
    /// message text
    body: Vec<String>,
    /// send a file instead of text
    #[arg(short = 'f', long)]
    file: Option<PathBuf>,
    /// show what would be sent without sending
    #[arg(long = "dry-run")]
    dry_run: bool,
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// install and start the launchd agent
    Install {
        /// bundle to install
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// stop and remove the launchd agent
    Uninstall,
    /// report whether the daemon is installed, running, and granted
    Status {
        #[arg(long)]
        json: bool,
    },
    /// check whether macOS lets the daemon drive Messages, sending nothing
    Automation {
        /// open the pane holding the switch
        #[arg(long)]
        settings: bool,
    },
    /// run the daemon in the foreground, for debugging
    Run,
}

fn positive(value: &str) -> Result<i64, String> {
    match value.parse::<i64>() {
        Ok(count) if count > 0 => Ok(count),
        _ => Err(format!("expected a positive integer, got {value}")),
    }
}

fn positive_seconds(value: &str) -> Result<u64, String> {
    match value.parse::<u64>() {
        Ok(count) if count > 0 => Ok(count),
        _ => Err(format!("expected a positive integer, got {value}")),
    }
}

/// Exit 2 is the documented status for "the data is there, the grant is not".
fn status_for(error: &Error) -> ExitCode {
    if matches!(error, Error::AccessDenied(_)) {
        ExitCode::from(2)
    } else {
        ExitCode::FAILURE
    }
}

fn print(text: &str) {
    print!("{text}");
    std::io::stdout().flush().ok();
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            error.print().ok();
            // clap exits 2 for a usage error. Here 2 means "the data is there,
            // the grant is not", which the README documents and scripts branch
            // on, so a mistyped flag must not claim it. `use_stderr` is false
            // for --help and --version, which are successes.
            return ExitCode::from(u8::from(error.use_stderr()));
        }
    };
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            status_for(&error)
        }
    }
}

fn run(cli: &Cli) -> msg::Result<()> {
    match &cli.command {
        Command::Daemon { command } => daemon(cli, command),
        _ => {
            let mut source = open_source(cli.db.clone());
            data(cli, &mut source)
        }
    }
}

fn data(cli: &Cli, source: &mut Source) -> msg::Result<()> {
    match &cli.command {
        Command::Chats { query, limit, json } => {
            let chats = source.chats(&ChatsQuery {
                query: query.clone(),
                limit: *limit,
                unknown: cli.unknown,
                names: cli.names(),
            })?;
            print(&if *json {
                to_json(&chats)
            } else {
                render_chats(&chats)
            });
        }

        Command::Read {
            chat,
            limit,
            since,
            tapbacks,
            json,
        } => {
            let reply = source.read(&ReadQuery {
                chat: chat.clone(),
                limit: *limit,
                since: since.clone(),
                tapbacks: *tapbacks,
                names: cli.names(),
            })?;
            print(&if *json {
                to_json(&reply)
            } else {
                format!(
                    "{}\n\n{}",
                    reply.chat.name,
                    render_messages(&reply.messages, false)
                )
            });
        }

        Command::Search {
            query,
            limit,
            chat,
            with,
            from,
            since,
            json,
        } => {
            let messages = source.search(&SearchQuery {
                query: query.clone(),
                chat: chat.clone(),
                with: with.clone(),
                from: from.clone(),
                limit: *limit,
                since: since.clone(),
                unknown: cli.unknown,
                names: cli.names(),
            })?;
            print(&if *json {
                to_json(&messages)
            } else {
                render_messages(&messages, true)
            });
        }

        Command::Watch {
            chat,
            interval,
            tapbacks,
            json,
        } => {
            let show_chat = chat.is_none();
            let as_json = *json;
            source.watch(
                &WatchQuery {
                    chat: chat.clone(),
                    tapbacks: *tapbacks,
                    unknown: cli.unknown,
                    names: cli.names(),
                    interval: *interval,
                },
                |message| {
                    print(&if as_json {
                        format!("{}\n", serde_json::to_string(message)?)
                    } else {
                        render_messages(std::slice::from_ref(message), show_chat)
                    });
                    Ok(())
                },
            )?;
        }

        Command::Contacts { handles, json } => {
            let reply = source.contacts(handles)?;
            if handles.is_empty() {
                print(&format!("{} handles known from Contacts\n", reply.size));
            } else if *json {
                print(&to_json(&reply.resolved));
            } else {
                for resolved in &reply.resolved {
                    // Composed here rather than on the wire. Identifying
                    // somebody is this command's job, so a person wants both
                    // names on the line — but a consumer wants two fields it
                    // can read separately, and `--json` hands it those.
                    let name = match (&resolved.name, &resolved.filed_as) {
                        (Some(name), Some(filed)) => format!("{name} ({filed})"),
                        (Some(name), None) => name.clone(),
                        (None, _) => "(unknown)".to_string(),
                    };
                    print(&format!("{}\t{name}\n", resolved.handle));
                }
            }
        }

        Command::Send(args) => send(cli, source, args)?,
        Command::Save {
            id,
            to,
            force,
            json,
        } => {
            let saved = source.save(*id, to, *force)?;
            if *json {
                println!(
                    "{}",
                    serde_json::json!({
                        "path": saved.path.display().to_string(),
                        "bytes": saved.bytes,
                    })
                );
            } else {
                println!("{} ({})", saved.path.display(), human_bytes(saved.bytes));
            }
        }
        Command::Daemon { .. } => unreachable!("handled in run"),
    }
    Ok(())
}

fn send(cli: &Cli, source: &mut Source, args: &SendArgs) -> msg::Result<()> {
    let text = args.body.join(" ");
    if args.file.is_none() && text.is_empty() {
        return Err(Error::other(
            "nothing to send, provide message text or --file",
        ));
    }
    let what = args
        .file
        .as_ref()
        .map_or_else(|| text.clone(), |path| path.to_string_lossy().into_owned());

    if args.dry_run {
        // Unconditional, so the disabled state stays inspectable (§7).
        let chat = source.resolve(&args.chat, cli.names())?;
        print(&format!(
            "would send to {}: {what}\n",
            msg::db::describe_target(&chat)
        ));
        return Ok(());
    }

    // The client reads the attachment with its own permissions and hands over
    // the bytes. The daemon holds Full Disk Access and must never be given a
    // path to read (§6).
    let file = match &args.file {
        None => None,
        Some(path) => Some(Attachment {
            name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            base64: base64::engine::general_purpose::STANDARD.encode(std::fs::read(path)?),
        }),
    };

    let sent = source.send(&SendQuery {
        chat: args.chat.clone(),
        body: (!text.is_empty()).then_some(text),
        file,
        names: cli.names(),
    })?;
    print(&format!("sent to {}: {what}\n", sent.name));
    Ok(())
}

fn daemon(cli: &Cli, command: &DaemonCommand) -> msg::Result<()> {
    match command {
        DaemonCommand::Install { from } => {
            let source = from.clone().unwrap_or_else(built_bundle);
            let installed = install(&source)?;
            let signature = signature_of(&installed.bundle);
            print(&format!(
                "installed {} (signed {signature})\nstarted {}\n",
                installed.bundle.display(),
                installed.plist.display()
            ));
            for (name, value) in &installed.environment {
                print(&format!("carried {name}={value}\n"));
            }
            let grant = grant_after_install();
            match grant {
                Grant::Held { message_count } => print(&format!(
                    "\nNothing left to do: msgd is already reading the database, {message_count} messages in.\n\
                     Its Full Disk Access grant is keyed to the bundle identifier and the\n\
                     certificate, not to this build, so it survived the reinstall.\n"
                )),
                Grant::Missing => print(
                    "\nOne step left, and it cannot be automated: switch on msgd under\n\
                     Privacy & Security > Full Disk Access, which is now open.\n\n\
                     It is listed there because it has already tried to read and been refused —\n\
                     a denied access is what creates the entry — so give it a minute if it is\n\
                     not there yet, then run `msg daemon status`.\n",
                ),
                Grant::Unknown => print(
                    "\nmsgd did not answer, so whether it can read is unknown. Opening Privacy &\n\
                     Security > Full Disk Access in case the grant is what it is waiting for;\n\
                     `msg daemon status` will say once it is up.\n",
                ),
            }
            if installed.replaced_legacy {
                print(
                    "\nThis replaced an unbundled daemon, whose own entries are still listed and\n\
                     now point at nothing. Remove them with the \"-\" button under Full Disk\n\
                     Access; the Automation one cannot be removed singly, only by\n\
                     `tccutil reset AppleEvents`, which clears every app on the machine.\n",
                );
            }
            if signature == "ad-hoc" {
                print(
                    "\nThis build is signed ad-hoc, so the grant is pinned to its hash and the\n\
                     next rebuild will need granting again. Rebuild without MSG_SIGN_IDENTITY\n\
                     to sign with a stable certificate instead.\n",
                );
            }
            if grant.needs_pane() {
                open_full_disk_access();
            }
        }

        DaemonCommand::Uninstall => {
            let removed = uninstall();
            for path in &removed {
                print(&format!("removed {}\n", path.display()));
            }
            if removed.is_empty() {
                print("nothing to remove\n");
            }
            print(
                "\nThe grants outlive the bundle, and both are keyed to its identifier:\n\n  \
                 tccutil reset SystemPolicyAllFiles com.ninjudd.msgd\n  \
                 tccutil reset AppleEvents com.ninjudd.msgd\n\n\
                 So does the certificate the build signed it with:\n\n  \
                 security delete-identity -c \"msg dev\"\n",
            );
        }

        DaemonCommand::Status { json } => return status(*json),

        DaemonCommand::Automation { settings } => {
            // Before the probe, not after it. The grant outlives the daemon, so
            // the case where someone most wants to revoke it — daemon stopped or
            // uninstalled, permission still granted — is exactly the case where
            // the probe fails and anything after it never runs.
            if *settings {
                open_automation();
            }
            let mut source = open_source(cli.db.clone());
            let reply = source.automation()?;
            print(&format!(
                "automation  {}\nconfig      send = {}\n{}",
                if reply.allowed { "allowed" } else { "refused" },
                reply.config_enabled,
                if reply.allowed {
                    String::new()
                } else {
                    format!("\n{}\n", reply.detail)
                }
            ));
            if reply.allowed && reply.config_enabled {
                print(
                    "\nBoth gates are open, so `msg send` will reach real people. Close either:\n\n  \
                     msg daemon automation --settings   switch msgd off under Automation\n  \
                     tccutil reset AppleEvents com.ninjudd.msgd\n  \
                     send = false                       in ~/.config/msg/config.toml\n",
                );
            } else if reply.allowed {
                print("\nmacOS would allow a send; the config is what is refusing it.\n");
            }
        }

        DaemonCommand::Run => {
            let server = Daemon::new(DaemonOptions {
                db_path: cli.db.clone(),
                config_path: None,
            });
            let path = server.listen(None)?;
            eprintln!("msgd {VERSION} listening on {}", path.display());
            loop {
                std::thread::park();
            }
        }
    }
    Ok(())
}

/// How long the install will wait to learn whether the daemon can read before
/// giving up and opening the pane anyway.
///
/// A cold daemon on this machine answers its first status in about 120ms, and
/// that first one is the expensive one because it builds the contact index. So
/// three seconds is roughly twenty times the observed cost, which leaves room
/// for a slower machine without leaving anyone watching a stalled install.
const PROBE_DEADLINE: Duration = Duration::from_secs(3);

/// Ask the just-installed daemon whether it can read, so the install knows
/// whether anything is left for a human to do.
///
/// It has to poll: `install` returns as soon as `launchctl bootstrap` succeeds,
/// which is before the daemon has created its socket. A daemon refused Full Disk
/// Access still starts and still listens — it just says so — so both real
/// answers arrive in well under the deadline.
///
/// The deadline bounds the whole probe rather than each attempt, and every
/// request is given only the time still left. Bounding the attempts alone would
/// not bound anything: a socket that accepts and then never answers is not
/// refused and not slow, it is silent, and it would otherwise sit on the
/// client's general thirty-second read timeout on the very first pass.
fn grant_after_install() -> Grant {
    let deadline = Instant::now() + PROBE_DEADLINE;
    loop {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return Grant::Unknown;
        };
        // A read timeout of zero means "never time out" to the kernel, which is
        // the one thing this must not ask for.
        match daemon_status_within(left.max(Duration::from_millis(50))) {
            Ok(Some(status)) => {
                return Grant::Held {
                    message_count: status.message_count,
                };
            }
            Err(Error::AccessDenied(_)) => return Grant::Missing,
            // Not listening yet, so wait and ask again.
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            // A timed-out read, a protocol mismatch, a half-open socket: no
            // evidence either way, and asking again would only spend the
            // deadline twice.
            Err(_) => return Grant::Unknown,
        }
    }
}

fn status(json: bool) -> msg::Result<()> {
    let loaded = is_loaded();
    let outcome = daemon_status();
    let problem = outcome.as_ref().err().map(std::string::ToString::to_string);
    let status = outcome.as_ref().ok().and_then(Clone::clone);

    if json {
        print(&to_json(&serde_json::json!({
            "loaded": loaded,
            "bundle": bundle_path().to_string_lossy(),
            "status": status,
            "error": problem,
        })));
        return Ok(());
    }

    let mut lines = vec![
        format!(
            "agent      {} ({})",
            if loaded { "loaded" } else { "not loaded" },
            plist_path().display()
        ),
        format!("bundle     {}", bundle_path().display()),
        format!(
            "socket     {}{}",
            socket_path().display(),
            if status.is_none() {
                " (not listening)"
            } else {
                ""
            }
        ),
        format!("log        {}", log_path().display()),
    ];
    if let Some(status) = &status {
        lines.push(format!(
            "version    {} (protocol {}), pid {}, up {}s",
            status.version, status.protocol, status.pid, status.uptime_seconds
        ));
        lines.push(format!("database   {}", status.database));
        lines.push(format!("messages   {}", status.message_count));
        lines.push(format!("contacts   {} handles", status.contact_count));
        for problem in &status.contact_problems {
            lines.push(format!("           {problem}"));
        }
        lines.push(format!("watchers   {}", status.watchers));
    }
    print(&format!("{}\n", lines.join("\n")));

    // Re-raised rather than rewrapped, so a denied read still exits 2.
    if let Err(error) = outcome {
        eprintln!();
        return Err(error);
    }
    let _ = problem;
    if !loaded {
        eprintln!("\nNo agent installed. Run `msg daemon install`.");
    }
    Ok(())
}
