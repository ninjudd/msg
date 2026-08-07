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
- Full Disk Access for your terminal

Grant Full Disk Access in System Settings > Privacy & Security > Full Disk
Access, add your terminal application, then restart it. macOS only applies the
permission to processes started after the change, so an already-open terminal
keeps failing until it is relaunched.

Without it, every command exits with status 2 and an explanation. The Messages
database is protected by TCC, so this step is unavoidable for any tool that
reads it.

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
```

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

`watch` polls every 3 seconds by default; change it with `--interval`.

### Sending

```sh
msg send dana "on my way"
msg send dana --file ~/diagram.png
msg send dana "hi" --dry-run   # print what would be sent, send nothing
```

Sending is handled by Messages.app through AppleScript. The chat identifier and
the message body are passed to the script as arguments rather than interpolated
into it, so quotes, backslashes, and newlines in a message need no escaping and
cannot alter the script.

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

## Options

| Option | Applies to | Meaning |
| --- | --- | --- |
| `--db <path>` | all | read a different `chat.db` |
| `--no-names` | all | skip Contacts, show raw handles |
| `-n, --limit <count>` | `chats`, `read`, `search` | how many results |
| `--since <when>` | `read`, `search` | duration or date lower bound |
| `-c, --chat <chat>` | `search`, `watch` | restrict to one conversation |
| `--tapbacks` | `read`, `watch` | include reactions |
| `--interval <seconds>` | `watch` | poll frequency |
| `-f, --file <path>` | `send` | send a file instead of text |
| `--dry-run` | `send` | show without sending |
| `--json` | all read commands | machine-readable output |

### Environment

| Variable | Meaning |
| --- | --- |
| `MSG_DB` | path to an alternate `chat.db`, same as `--db` |
| `MSG_CONTACTS_SOURCE` | UUID of the Contacts source whose names win |

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
```

Point `MSG_DB` at another database to develop against a fixture rather than your
own messages. The tests cover the two pieces with real logic in them, the Apple
timestamp conversions and typedstream decoding in `src/apple.ts`, and the handle
normalization in `src/contacts.ts`. Neither test suite touches a real database.

```
src/
  apple.ts       Apple epoch conversion, typedstream decoding
  contacts.ts    Contacts lookup and handle normalization
  db.ts          read-only queries against chat.db
  format.ts      terminal and JSON rendering
  cli.ts         command definitions
  commands/
    send.ts      sending through Messages.app
```

## Limitations

- Reading requires Full Disk Access, which cannot be scoped to just Messages.
- Attachments are not listed or downloaded; a message containing one shows the
  U+FFFC placeholder that Messages stores in its place.
- Editing, unsending, and reactions cannot be sent. Those need the private
  APIs, which are not reachable from AppleScript.
- `watch` polls rather than subscribing, so a new message appears within one
  poll interval rather than instantly.
- Group membership changes, read receipts, and typing indicators are not
  surfaced.
