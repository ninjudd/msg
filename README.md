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

$ msg chat "Dana" -n 3
Dana Reyes

5:30 PM  Dana Reyes: are you around later
5:31 PM  me: after 6, yeah
9:46 PM  Dana Reyes: works, see you then
```

## Requirements

- macOS, with Messages signed in
- Full Disk Access, held by [the daemon](#the-daemon) or by your terminal

`msg` is two self-contained binaries and needs no runtime installed. Building it
needs Rust and the Xcode command line tools, for `codesign`.

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
./scripts/build.sh                        # both binaries, and the signed daemon
mkdir -p ~/.local/bin                     # macOS does not create this
ln -s "$PWD/build/msg" ~/.local/bin/msg
```

macOS puts neither `~/.local/bin` on your `PATH` nor the directory itself on
disk, so if `msg` is not found afterwards:

```sh
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
exec zsh
```

Anywhere already on your `PATH` works just as well; `~/.local/bin` is only a
convention that needs no `sudo`.

`build/msg` is the command and `build/msgd.app` is the daemon. `msg daemon
install` looks for `msgd.app` beside the real binary, following the symlink
first, so a link into `~/.local/bin` is fine — but a *copy* of `msg` needs
`msgd.app` copied alongside it, or `--from` pointing at one.

To run without installing, `cargo run --bin msg -- <command>` works from the
checkout.

## Usage

### Conversations

```sh
msg chats                      # by most recent activity, one row per person
msg chats dana                 # filter by contact name, handle, or identifier
msg chats -n 100               # more of them
msg chats --unknown            # include the ones Messages filters away
```

Someone you reach at two addresses is one row, not two — the same merge
[reading](#reading) describes, so the count here agrees with what `msg chat`
prints. `-n` counts conversations, so it means the same thing on both.

Conversations that Messages filters as unknown senders are hidden, matching
what your phone shows. On one real database that removed 1,395 of 2,559
conversations: verification codes, delivery notifications, and one-off numbers
that were never saved as contacts.

A chat can be named by its rowid, by a handle, or by part of its name. A name
matches from the start of a word, so `ana` reaches Ana Duarte and not Dana
Reyes, while typing a prefix still works and `rob` still reaches Robin. An
address is different and matches anywhere, because the middle of a phone number
is exactly how anyone types a fragment of one. When a name still matches more
than one conversation, `msg` lists the candidates instead of guessing.

A [nickname](#contacts) counts as one of those names, so someone filed under
their full name and known to you as something else is found by either — and is
shown as the nickname, since that is what you call them.

**Several conversations with one person are not an ambiguity.** Messages keeps a
conversation per address, so someone you reach at a phone number and an email
address has two of them, and naming that person matches both. They are one
Contacts record, so `msg` answers with whichever was last active rather than
asking a question that has no answer. A fragment counts as naming them: typing
fewer letters does not make it two people.

**Several names are a room.** Each argument names a person, so `msg chat dana
sam` is the conversation whose members are exactly Dana and Sam and you. Exactly
those: a room that merely contains them is not the answer, because a command
that quietly hands back a conversation with somebody in it you did not name is
worse than one that says there is no such conversation. A room with a name of
its own is still reached by that name.

Because each argument is a person, **a name with a space in it has to be
quoted** — `msg chat "Ana Duarte"` is one person and `msg chat ana duarte` is
two.

**A name is a person before it is anything else.** Someone's own conversation
wins over every group they are in, because being in a room is not being the
person — which is what makes a first name usable, since a first name reaches
every room its owner is a member of. A group is still reached by its own name,
or by its rowid, and somebody you only ever share a room with still finds the
room. A group named after somebody you also have a conversation with is the one
case where both claims are equally good, so that is reported as an ambiguity
rather than decided for you.

What still reports an ambiguity is a question that has an answer. Two
*different* people who happen to share a name are two people, and what gets
collapsed is the Contacts record, never the name it renders as.

### Reading

`msg chats` lists conversations and `msg chat` prints one, which is a letter
apart and worth reading twice. The plural is the index; the singular opens
something in it.

```sh
msg chat "Ship Room"           # a conversation
msg chat dana                  # one person: their own conversation
msg chat dana sam              # two people: the room with exactly those two
msg chat 42 -n 200             # by rowid, with more history
msg chat dana --since 7d       # only the last week
msg chat dana --since 2026-01-15
msg chat dana --tapbacks       # include reactions
```

`--since` accepts a duration (`30m`, `2h`, `7d`, `4w`), an ISO date like
`2026-01-15`, or a full timestamp. A bare date means midnight UTC.

**A name reads the person; a rowid reads the thread.** Messages keeps a separate
conversation for every address someone has, so a person you reach at a phone
number and an email address has two — and naming them shows both as one
transcript, in the order the messages arrived:

```sh
msg chat dana                  # everything Dana said, at any of her addresses
msg chat 42                    # only the conversation with rowid 42
```

An address sits across the two: when a person's threads fold together, the
address you typed keeps its own thread at the head of the answer — and the
leading thread is where a send goes, so the address is how you choose the
route.

The rowid is the way out when that is not what you want. `msg chats` merges on
the same rule, so it lists one row per person rather than one per thread, and
that row carries their total message count and the rowid a send would go to —
the newest of their threads. The older ones are not in the listing; `msg chat
<name> --json` names them in `merged`, which is where to look for the rowid of a
thread the listing no longer shows.

A conversation Messages files under Unknown Senders is left out of the merge
unless `--unknown` is passed, so filtered content never arrives inside a
conversation you think of as known. Groups never merge.

`--json` names the thread a reply would go to in `chat`, as it always has, and
lists any others in `merged`. A person with one conversation is unchanged in
both output modes.

An inline reply says what it is answering, so it stops reading as an unrelated
remark that happens to come later:

```
5:46 PM  Dana Reyes: Btw do you think we can find the inflator?
         ↳ replying to me: Just read about it but not fully understanding it
5:50 PM  me: It is the underlying model that generates music
```

Chronology is untouched — the quote sits above the reply rather than moving it.
`--json` carries the same as a `replyTo` object with the answered message's
rowid, sender, and excerpt. A reply whose original has been deleted still prints
as an ordinary message.

Attachments read as what they are, in the place they occupy in the message:

```
5:31 PM  Dana Reyes: [#48213 IMG_4821.HEIC, 3.2 MB]
5:32 PM  Dana Reyes: from the trip [#48214 clip.mov, 41.8 MB]
```

Messages stores a photo as a single invisible character in the body and keeps
the file elsewhere, so without this a message that is only a photo prints as
nothing at all. `--json` carries the same as an `attachments` array.

### Saving an attachment

The number is the attachment's id, and it is how you get the file:

```sh
msg save 48213                     # into the current directory
msg save 48213 --to ~/Downloads
msg save 48213 --to ~/Downloads --force   # replace one already there
```

The file lives under `~/Library/Messages/Attachments`, which your terminal has
no permission to read — so `msg` names it by id and never by path. The daemon
opens the file, hands over the bytes, and the CLI writes them where you asked,
with your permissions. That is the same shape sending uses, in reverse: **the
daemon never accepts a path and never writes one**, so it can be neither an
arbitrary-file reader nor an arbitrary-file writer.

Large attachments stream in chunks rather than arriving whole, so a 500MB video
costs the same memory as a small photo. An interrupted save leaves nothing
behind, and an existing file is refused rather than overwritten.

Messages sometimes keeps the row after deleting the file. `msg save` says so
plainly rather than writing an empty file.

### Searching

```sh
msg search "dinner"                       # across every conversation
msg search "deploy" -c "Ship Room"        # within one
msg search "dinner" --with dana           # one person, wherever you talk
msg search "dinner" --from dana           # only what they sent
msg search "invoice" --since 30d -n 50
msg search "dinner" -C 2                  # two messages either side of each hit
msg search "dinner" -A 3 -B 1             # or each side separately
```

**A match on its own is a line torn out of a conversation**, and it is often the
least informative line in it — `sounds good`, `yeah ok`. `-A`, `-B` and `-C`
put it back:

```
$ msg search "the inflator" -C 1
  5:44 PM  [Dana Reyes] me: heading to the lake tomorrow
> 5:46 PM  [Dana Reyes] Dana Reyes: Btw do you think we can find the inflator?
  5:50 PM  [Dana Reyes] me: it is in the garage
--
  2:11 PM  [Ship Room] Sam Oyelaran: who packed the car
> 2:12 PM  [Ship Room] Sam Oyelaran: no inflator either
```

`> ` marks the hit and two spaces mark its context, so the distinction survives
being piped, pasted, or grepped again — this program writes no colour anywhere.
`--` separates runs, as in grep. Windows that overlap **or touch** merge into
one run, so several hits in one exchange print once rather than repeating the
messages between them.

The window is a slice of the conversation, not a continuation of the search.
`--from dana -A 2` means "what Dana said, plus what came next, whoever sent it";
filtering the window by the same person would return Dana's next two messages,
which is almost never the reply she got. `--since` bounds what counts as a hit
too, so a window on a hit near that boundary reaches back past it. And a window
holds reactions even though a search can never match one, because `Liked "see
you at 8"` is frequently the entire reply.

`-C` is shorthand for both sides and loses to either given outright, including
zero — `-C 2 -B 0` is two messages after each hit and none before, as in grep.

`-n` still counts hits, not printed lines, so `-n 25 -C 5` is twenty-five
matches and however many messages that comes to. `--json` carries the same as
`matched: false` on context messages and a `group` number per run; a search
without context is byte-identical to what it was before these existed.

**`-C` and `-c` differ only in case and mean entirely different things** — `-c`
scopes to a conversation, `-C` sets the context width. `-c 3` is a legitimate
way to name chat 3, so a case slip cannot be guarded against. The `-A`/`-B`/`-C`
muscle memory was judged worth it.

Search matches the message body wherever it lives. Most bodies are not in
`message.text` at all but archived into `attributedBody`, so matching has to look
inside that blob rather than cast it to text — a cast stops at the first NUL byte,
which in an archived body comes long before the words.

`-c` scopes to a conversation; `--with` and `--from` scope to a *person*, which
is not the same thing. Someone you message one to one and in three group chats
is four conversations and one contact, and Messages stores each of their
addresses — phone, email, a second number — as a separate handle. Both flags
gather all of them, so a search is of the person rather than of whichever
address happened to be used.

Naming any one of a person's addresses reaches all of them, because what is
being resolved is the Contacts record rather than the address you typed. Two
records can carry the same name, though, and those are two people: naming them
reports the ambiguity, with an address alongside each, rather than answering with
both people's messages under one name. Name an address to say which.

The two differ in whose messages come back. `--from` is only theirs. `--with` is
the exchange: theirs everywhere, plus your own where the conversation is just
the two of you — in a group your messages went to the room rather than to them,
so they are left out.

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

`msg daemon automation` reports both gates and sends nothing to anyone. Either
one can be closed to take away the ability to send:

```sh
msg daemon automation --settings         # the switch, under Automation
tccutil reset AppleEvents com.ninjudd.msgd
```

`--dry-run` works whatever the gates say, so the disabled state stays
inspectable.

**A confirmation names the address, not just the person.**

```
$ msg send dana "on my way" --dry-run
would send to Dana Reyes (dana@example.com): on my way
```

Somebody you reach at a phone number and an email address has two
conversations that display the same name, and a *name* picks the one last
active. An *address* pins it: the typed address's own thread leads, so the
send goes by that route whatever spoke last. The address is the only thing
that distinguishes them, and the routes are not interchangeable — one of them
may be an SMS fallback, and the most recent is not always the one that gets
read. A dry run that printed only the name would be identical in both cases,
which would make it useless as the check it is for.
Conversations with a name of their own are named by it, since a room is not
ambiguous the way a person is.

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
./scripts/build.sh     # compile msgd and sign it inside msgd.app
msg daemon install     # copy it into place and start it
```

Then switch on `msgd` under System Settings > Privacy & Security > Full Disk
Access, which the install opens for you the first time. It appears in that list
because the daemon has already tried to read and been refused — a denied access
is what creates the entry, and there is no command that can add one. Give it a
minute if it is not there yet.

Reinstalling a daemon that is already granted opens nothing: the install asks it
whether it can read before deciding there is anything left for you to do. The
grant is keyed to the bundle identifier and the signing certificate rather than
to the build, so it survives a rebuild.

```sh
msg daemon status      # installed? running? granted? how many watchers?
msg daemon automation  # may it drive Messages? sends nothing
msg daemon uninstall   # stop and remove it
```

It ships as an app bundle in `~/.local/libexec/msgd.app`, which nothing ever
launches. The bundle exists so macOS keys its permissions by bundle identifier
rather than by executable path: a path-keyed permission cannot be switched off —
the toggle asks for Touch ID and then silently does nothing — which makes
granting Automation a one-way door. The reasoning and the measurements are in
[daemon-and-permissions.md §13](docs/projects/all/daemon-and-permissions.md).

Being a bundle, it has an icon, which is how you find it in those two lists.
`assets/msgd.svg` is the source and `assets/msgd.icns` is what ships; both are
committed and the build only copies the `.icns`. `./scripts/build-icon.sh`
regenerates it,
rasterizing with `qlmanage` so the pipeline needs nothing but macOS, and is
deliberately not part of the build.

`msg` talks to the daemon whenever one is listening and reads the database
directly when one is not, so nothing breaks if you never install it. `--db`
always reads locally and never reaches the daemon.

**Signing.** The grant is pinned to the daemon's code signature, so an ad-hoc
signature — matched by hash — dies on every rebuild and has to be granted again.
To avoid that, **the first `./scripts/build.sh` creates a self-signed `msg dev`
certificate in your login keychain** and signs with it; the requirement then
anchors to the certificate and survives rebuilds. Nothing is submitted anywhere,
`codesign` is offline, and the certificate is never added to any trust store.

macOS asks before `codesign` uses that key, once per build. Answering "Always
Allow" removes the prompt and, with it, the thing that stops local code from
signing its own daemon and inheriting the grant — see
[signing-identity.md](docs/projects/all/signing-identity.md).

```sh
MSG_SIGN_IDENTITY="my identity" ./scripts/build.sh   # a different certificate
MSG_SIGN_IDENTITY=- ./scripts/build.sh               # ad-hoc, no certificate
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

**Uninstalling does not withdraw the grants.** They outlive the bundle they were
granted to. Both are switches in System Settings, and both can be revoked from a
script, because they are keyed to the bundle identifier:

```sh
tccutil reset SystemPolicyAllFiles com.ninjudd.msgd   # stop it reading
tccutil reset AppleEvents com.ninjudd.msgd            # stop it sending
```

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
| `-n, --limit <count>` | `chats`, `chat`, `search` | how many results |
| `--since <when>` | `chat`, `search` | duration or date lower bound |
| `-c, --chat <chat>` | `search`, `watch` | restrict to one conversation |
| `--tapbacks` | `chat`, `watch` | include reactions |
| `-A, --after <count>` | `search` | messages to show after each hit |
| `-B, --before <count>` | `search` | messages to show before each hit |
| `-C, --context <count>` | `search` | both, and note the clash with `-c` |
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
| `MSG_SIGN_IDENTITY` | Code Signing identity for `./scripts/build.sh` |

`MSG_DB` steers the CLI, which reads that database itself rather than asking the
daemon — the same as `--db`, so a fixture stays a fixture even with a daemon
running.

`MSG_SOCKET`, `MSG_STATE_DIR`, `MSG_CONFIG` and `MSG_CONTACTS_SOURCE` are read by
the daemon, and **a launchd job inherits nothing from your shell**. `msg daemon
install` copies whichever of them are set into the agent and prints what it
carried; changing one afterwards means installing again.

`MSG_DB` is never carried into the agent, deliberately. It would outlive the
shell that set it, leaving a daemon pinned to a fixture and a CLI with no way to
know. To serve one, run `msgd` yourself with `MSG_DB` set.

## How it works

The Messages schema has a number of sharp edges. Most of the code here exists
to handle them.

**Dates are nanoseconds since 2001-01-01**, not Unix seconds. Current values sit
around 8.1e17. That is an ordinary `i64` here, but it is past
`Number.MAX_SAFE_INTEGER` at 9.0e15, which cost the JavaScript build a layer of
BigInt arithmetic in every query and conversion. Dividing by a thousand at the
wrong moment is the failure mode, and the seconds/nanoseconds threshold is what
tells the two apart: rows written before the 2011 schema change are in seconds.

**`message.text` is usually NULL.** On a sample of 20,000 messages from a live
database, 97.6% carried their body only in `attributedBody`, an NSArchiver
typedstream blob left over from the days when messages were archived
`NSAttributedString` objects. `src/apple.rs` decodes that format by hand, with no
Objective-C bridge and no dependency on the deprecated `NSUnarchiver`. It decoded
every one of those 19,524 blobs.

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

**A nickname is what you call someone, so it is what they are called here.**
Someone filed as Robert Chen with a nickname of Bob reads as Bob — in the chat
list, at the head of a conversation, and against every message he sent. You told
Contacts what he is called; a transcript that says Robert Chen throughout is
answering a question nobody asked.

Both names still find him. `msg chat bob` and `msg chat "Robert Chen"` open the
same conversation, and it reads as Bob either way — the display does not follow
whichever name you typed. The filed name is displaced, not discarded, which
matters because it is the one you have when a nickname is all you remember of
somebody and the one somebody else would search by.

`msg contacts` shows both, because identifying somebody is its whole job rather
than a label on something else — and it takes a name as readily as an address,
since a name is the ordinary way to ask who somebody is:

```
$ msg contacts bob
+13105551234        Bob (Robert Chen)
bob@example.com     Bob (Robert Chen)

$ msg contacts +13105551234
+13105551234        Bob (Robert Chen)
```

A name matches either of somebody's two names, as a substring, so it can reach
more than one person and lists every address of each. An address given in full
answers on its own instead — otherwise naming somebody exactly could drag in
whoever else happens to contain those characters. A term matching nobody is
echoed back as `(unknown)`.

That is the one place the pair appears. A transcript labelled `Bob (Robert
Chen)` on every line would be unreadable, and a chat list of them worse.

`--json` keeps them apart, as `name` and `filedAs`, and omits `filedAs` when
nothing was displaced. The line above is composed for a person to read; a
program should not have to take it apart again.

Both names are matched exactly where a name is matched, and nowhere else. A
conversation with a name of its own is found by that name rather than by who is
in it, so a group called Ship Room is not reachable through a member's name or
nickname. And because a nickname is short enough to be a fragment of plenty
else, typing a whole one settles the ambiguity it creates: `bob` prefers the
person called exactly Bob over everyone merely containing those letters.

A group of people you have nicknames for reads as those nicknames, since a group
with no name of its own is named after its members.

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
cargo test           # unit tests, the CLI, and the daemon over a real socket
cargo clippy --all-targets
./scripts/build.sh   # both binaries, and the signed daemon bundle
```

Point `MSG_DB` at another database to develop against a fixture rather than your
own messages. The tests cover the pieces with real logic in them: the Apple
timestamp conversions and typedstream decoding in `src/apple.rs`, the handle
normalization in `src/contacts.rs`, the exit statuses in `tests/cli.rs`, and the
daemon end to end over a real socket in `tests/daemon.rs`. No test reads a real
database, every daemon test asks for `names: false` so none of them touches
Contacts, and the daemon tests point at a config file that does not exist so
sending stays shut.

```
src/
  apple.rs       Apple epoch conversion, typedstream decoding
  contacts.rs    Contacts lookup and handle normalization
  db.rs          read-only queries against chat.db
  format.rs      terminal and JSON rendering
  source.rs      the daemon when one is listening, the database when not
  lib.rs         the error type and the shapes both binaries share
  bin/
    msg.rs       command definitions
    msgd.rs      the daemon process
  daemon/
    protocol.rs  the wire: requests, frames, socket path
    server.rs    the daemon itself
    client.rs    connecting and reading answers
    config.rs    the one config key, read by the daemon
    send.rs      driving Messages.app over Apple Events
    install.rs   the launchd agent and where the bundle lives
tests/
  cli.rs         the binary as a user meets it, including exit statuses
  daemon.rs      the daemon over a real socket
```

## Limitations

- Reading requires Full Disk Access, which cannot be scoped to just Messages.
- Attachments cannot be listed on their own; ids come from reading or searching
  the conversation they are in.
- Editing, unsending, and reactions cannot be sent. Those need the private
  APIs, which are not reachable from AppleScript.
- Without the daemon, `watch` polls rather than subscribing, so a new message
  appears within one poll interval rather than instantly.
- Group membership changes, read receipts, and typing indicators are not
  surfaced.
