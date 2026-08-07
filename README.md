# msg

Read and send iMessages from the command line.

`msg` talks straight to the Messages database on your Mac. There is no server to
run, no REST API to authenticate against, and no third-party service that ever
sees your messages. Nothing leaves the machine.

```
$ msg chats
    1  Priya Raman             9m ago   direct
   75  Dana Reyes              32m ago  direct
    6  Thursday Climbing       1h ago   2 people
   23  Dana Reyes, Sam Oyelaran  7h ago  3 people

$ msg read "Dana" -n 3
Dana Reyes

5:30 PM  Dana Reyes: are you around later
5:31 PM  me: after 6, yeah
9:46 PM  Dana Reyes: works, see you then
```

## Requirements

- macOS, with Messages signed in
- Node 24 or newer, for the built-in `node:sqlite` module
- Full Disk Access, held by [the daemon](#the-daemon) or by your terminal

The Messages database is protected by TCC, so something has to hold Full Disk
Access. The daemon is the better place for it, because a grant on your terminal
covers Mail, Safari history, Photos, and every file you can read, and every
command you run there inherits it.

To grant it to the terminal instead, add your terminal application under System
Settings > Privacy & Security > Full Disk Access and restart it. macOS only
applies the permission to processes started after the change, so an already-open
terminal keeps failing until it is relaunched.

Without either, every command exits with status 2 and an explanation.

## Install

```sh
git clone git@github.com:ninjudd/msg.git
cd msg
pnpm install     # runs the build automatically
npm link         # puts `msg` on your PATH
```

`pnpm install` triggers the `prepare` script, which compiles `src/` to `dist/`.
Either `npm link` or `pnpm link --global` will install the command; if pnpm
reports that its global bin directory is not on your PATH, `npm link` sidesteps
the issue entirely.

To run without installing globally, use `pnpm msg <command>`, which executes
straight from the TypeScript sources via tsx.

## Usage

### Conversations

```sh
msg chats                      # by most recent activity
msg chats dana                 # filter by contact name, handle, or identifier
msg chats -n 100               # more of them
msg chats --unknown            # include the ones Messages filters away
```

Conversations that Messages filters as unknown senders are hidden, matching
what your phone shows. On one real database that removed 1,395 of 2,559
conversations: verification codes, delivery notifications, and one-off numbers
that were never saved as contacts.

A chat can be named by its rowid, by a handle, or by any substring of its name.
When a substring matches more than one conversation, `msg` lists the candidates
instead of guessing.

### Reading

```sh
msg read "Ship Room"           # a conversation
msg read 42 -n 200             # by rowid, with more history
msg read dana --since 7d       # only the last week
msg read dana --since 2026-01-15
msg read dana --tapbacks       # include reactions
```

`--since` accepts a duration (`30m`, `2h`, `7d`, `4w`) or any date Node can
parse.

### Searching

```sh
msg search "dinner"                       # across every conversation
msg search "deploy" -c "Ship Room"        # within one
msg search "invoice" --since 30d -n 50
```

### Following

```sh
msg watch                      # print new messages as they arrive
msg watch -c dana              # just one conversation
msg watch --json               # JSON lines, one object per message
```

With [the daemon](#the-daemon) running, new messages arrive as they land. Without
it, `watch` polls every 3 seconds; change that with `--interval`.

### Sending

```sh
msg send dana "on my way"
msg send dana --file ~/diagram.png
msg send dana "hi" --dry-run   # print what would be sent, send nothing
```

**Sending is off until you switch it on, and it goes through
[the daemon](#the-daemon).** Two independent gates stand in front of it:

```toml
# ~/.config/msg/config.toml
send = true
```

and macOS has to allow `msgd` to control Messages, under System Settings >
Privacy & Security > Automation. The first is a switch you can find and read;
the second is enforced by the operating system, so it holds even when something
rewrites the config. The daemon checks the config key, not the CLI — a check a
program runs on itself is advice rather than a gate.

Automation is a separate permission from Full Disk Access, so the two can be set
independently: a daemon allowed to send but refused the database, or the
reverse. Sending by chat guid needs no database read at all.

`--dry-run` works whatever the gates say, so the disabled state stays
inspectable.

The chat identifier and the message body are passed to AppleScript as arguments
rather than interpolated into it, so quotes, backslashes, and newlines in a
message need no escaping and cannot alter the script. `--file` is read by the
CLI with your own permissions and handed to the daemon as bytes; the daemon
never opens a path a caller named, since it holds Full Disk Access and that
would make it read anything.

### Names

Handles are resolved to contact names automatically, in rendered output and in
`--json` alike. `--no-names` shows raw phone numbers and addresses, and skips
reading Contacts altogether.

### Machine-readable output

Every read command accepts `--json`. `watch --json` emits newline-delimited
JSON, one object per message, suitable for streaming into another process.

```sh
msg search "invoice" --json | jq '.[] | {date, sender, body}'
```

## The daemon

`msg` can read the Messages database itself, but that means granting Full Disk
Access to your terminal, and that grant is not scoped to messages: it covers
Mail, Safari history, Photos, every application container, and every file you
can read. Everything you run in that terminal inherits it.

`msgd` moves the grant to one binary. It is a launchd agent that holds Full Disk
Access on its own, answers a fixed set of questions over a unix socket, and
takes no filesystem path from anyone. The CLI needs no permission at all.

```sh
pnpm build:msgd        # compile msgd to a single executable
msg daemon install     # copy it into place and start it
```

Then add the printed path under System Settings > Privacy & Security > Full Disk
Access and switch it on. It appears in that list because the daemon has already
tried to read and been refused — a denied access is what creates the entry, and
there is no command that can add one. Give it a minute if it is not there yet.

```sh
msg daemon status      # installed? running? granted? how many watchers?
msg daemon uninstall   # stop and remove it
```

`msg` talks to the daemon whenever one is listening and reads the database
directly when one is not, so nothing breaks if you never install it. `--db`
always reads locally and never reaches the daemon.

**Signing.** The grant is pinned to the daemon's code signature, so an ad-hoc
signature — matched by hash — dies on every rebuild and has to be granted again.
To avoid that, **the first `pnpm build:msgd` creates a self-signed `msg dev`
certificate in your login keychain** and signs with it; the requirement then
anchors to the certificate and survives rebuilds. Nothing is submitted anywhere,
`codesign` is offline, and the certificate is never added to any trust store.

macOS asks before `codesign` uses that key, once per build. Answering "Always
Allow" removes the prompt and, with it, the thing that stops local code from
signing its own daemon and inheriting the grant — see
[signing-identity.md](docs/projects/all/signing-identity.md).

```sh
MSG_SIGN_IDENTITY="my identity" pnpm build:msgd   # use a different certificate
MSG_SIGN_IDENTITY=- pnpm build:msgd               # ad-hoc, no certificate
security delete-identity -c "msg dev"             # remove the one msg created
```

**What it changes.** `watch` stops polling — the daemon tails the write-ahead log
and pushes to every watcher, so one process does the work no matter how many
terminals are following. Contact names are resolved by the daemon too, so
Contacts needs no permission of its own. And [sending](#sending) runs from the
daemon, which is what makes "may this tool text people?" an operating system
permission rather than a flag a program honours about itself.

The two permissions are independent. Granting Full Disk Access does not let
`msgd` send, granting Automation does not let it read, and each is a separate
switch in a separate list.

**Uninstalling does not withdraw the grant.** A Full Disk Access entry outlives
the binary it was granted to. Remove it in System Settings, or with
`sudo tccutil reset SystemPolicyAllFiles com.ninjudd.msgd`.

The reasoning behind the design — including why the socket carries no
authentication, and why the daemon is a single executable rather than a copy of
`node` — is in
[docs/projects/all/daemon-and-permissions.md](docs/projects/all/daemon-and-permissions.md).

## Options

| Option | Applies to | Meaning |
| --- | --- | --- |
| `--db <path>` | all | read a different `chat.db` |
| `--no-names` | all | skip Contacts, show raw handles |
| `--unknown` | `chats`, `search`, `watch` | include filtered unknown senders |
| `-n, --limit <count>` | `chats`, `read`, `search` | how many results |
| `--since <when>` | `read`, `search` | duration or date lower bound |
| `-c, --chat <chat>` | `search`, `watch` | restrict to one conversation |
| `--tapbacks` | `read`, `watch` | include reactions |
| `--interval <seconds>` | `watch` | poll frequency, without a daemon |
| `-f, --file <path>` | `send` | send a file instead of text |
| `--dry-run` | `send` | show without sending |
| `--json` | all read commands | machine-readable output |

### Environment

| Variable | Meaning |
| --- | --- |
| `MSG_DB` | path to an alternate `chat.db`, same as `--db` |
| `MSG_CONTACTS_SOURCE` | UUID of the Contacts source whose names win |
| `MSG_SOCKET` | where the daemon listens, default `~/.local/state/msg/msgd.sock` |
| `MSG_STATE_DIR` | socket and log directory, default `~/.local/state/msg` |
| `MSG_CONFIG` | config file, default `~/.config/msg/config.toml` |
| `MSG_SIGN_IDENTITY` | Code Signing identity for `pnpm build:msgd` |

`MSG_DB` steers the CLI, which reads that database itself rather than asking the
daemon — the same as `--db`, so a fixture stays a fixture even with a daemon
running.

The rest are read by the daemon, and **a launchd job inherits nothing from your
shell**. `msg daemon install` copies whichever of them are set into the agent
and prints what it carried; changing one afterwards means installing again.

## How it works

The Messages schema has a number of sharp edges. Most of the code here exists
to handle them.

**Dates are nanoseconds since 2001-01-01**, not Unix seconds. Current values sit
around 8.1e17, well past `Number.MAX_SAFE_INTEGER` at 9.0e15, and `node:sqlite`
refuses to narrow them: reading one as a JavaScript number throws `Value is too
large to be represented as a JavaScript number`. Every statement therefore reads
integers as BigInt, and conversion to a `Date` happens in BigInt arithmetic.

**`message.text` is usually NULL.** On a sample of 20,000 messages from a live
database, 97.6% carried their body only in `attributedBody`, an NSArchiver
typedstream blob left over from the days when messages were archived
`NSAttributedString` objects. `src/apple.ts` decodes that format in plain
TypeScript, with no PyObjC or Objective-C bridge and no dependency on the
deprecated `NSUnarchiver`. It decoded every one of those 19,524 blobs.

**Tapbacks are messages.** A reaction is stored as an ordinary row with
`associated_message_type != 0`, so a naive query mixes `Liked "see you then"`
into the conversation. They are filtered out unless `--tapbacks` is passed.

**Filtering is a category, not a flag.** `chat.is_filtered` is not a boolean.
It holds `0` for ordinary conversations and a nonzero category for the ones
Messages sets aside, so a `= 1` test silently lets a whole category through. One
database here used `1` for 1,352 conversations and `2` for another 43, none of
which had a saved contact. `msg` treats any nonzero value as filtered.

**A chat's name is often absent.** `display_name` is set for named group chats
and empty for everything else, so direct messages fall back to participant
handles, and from there to contact names.

**The database is read-only and may be locked.** Messages.app holds it open in
WAL mode. `msg` opens it read-only, and if the write-ahead log cannot be opened
alongside it, copies the database and its sidecar files to a temporary location
and reads the copy.

## Contacts

Names come from the Contacts databases under
`~/Library/Application Support/AddressBook`, which `msg` reads directly. There
is one database per account (iCloud, local, Google, and so on), and all of them
are merged.

**Numbers are stored in whatever shape they were typed.** One real database held
ten distinct formats for the same kind of number, including `+13105551234`,
`(310) 555-1234`, `310.555.1234`, `1-310-555-1234` and a bare `3105551234`. Both
sides of a comparison are stripped to digits, and numbers long enough to carry a
country code are matched on their last ten digits. Short codes are matched
whole, and email handles are matched case-insensitively.

**Accounts disagree.** The same number can carry a different name in each
account, so the order they are merged in decides the winner. `msg` reads
`ABDefaultSourceID` from your Contacts preferences and visits that source first,
which is the account you actually maintain. Set `MSG_CONTACTS_SOURCE` to a
source UUID to prefer a different one. Source UUIDs are the directory names
under `~/Library/Application Support/AddressBook/Sources`.

Contacts is read once per run, and only when names are wanted, so `--no-names`
costs nothing. If the databases are missing or unreadable, lookups return
nothing and messages still read normally.

## Development

```sh
pnpm test        # vitest
pnpm typecheck   # tsc --noEmit
pnpm build       # tsc -p tsconfig.build.json
pnpm build:msgd  # bundle and sign the daemon executable
```

Point `MSG_DB` at another database to develop against a fixture rather than your
own messages. The tests cover the pieces with real logic in them: the Apple
timestamp conversions and typedstream decoding in `src/apple.ts`, the handle
normalization in `src/contacts.ts`, and the daemon end to end over a real socket
against a database the test builds. No test reads a real database, and every
daemon test asks for `names: false` so none of them touches Contacts.

```
src/
  apple.ts       Apple epoch conversion, typedstream decoding
  contacts.ts    Contacts lookup and handle normalization
  db.ts          read-only queries against chat.db
  format.ts      terminal and JSON rendering
  source.ts      the daemon when one is listening, the database when not
  macho.ts       embedding an Info.plist in the daemon executable
  cli.ts         command definitions
  msgd.ts        the daemon process
  daemon/
    protocol.ts  the wire: requests, frames, socket path
    server.ts    the daemon itself
    client.ts    connecting and reading answers
    config.ts    the one config key, read by the daemon
    send.ts      driving Messages.app over Apple Events
    install.ts   the launchd agent and where the binary lives
```

## Limitations

- Reading requires Full Disk Access, which cannot be scoped to just Messages.
- Attachments are not listed or downloaded; a message containing one shows the
  U+FFFC placeholder that Messages stores in its place.
- Editing, unsending, and reactions cannot be sent. Those need the private
  APIs, which are not reachable from AppleScript.
- Without the daemon, `watch` polls rather than subscribing, so a new message
  appears within one poll interval rather than instantly.
- Group membership changes, read receipts, and typing indicators are not
  surfaced.
