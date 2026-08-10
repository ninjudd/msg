---
name: msg
description: >-
  Read, search, and send iMessages on this Mac with the msg CLI. Use when the
  user asks about their text messages or iMessage conversations — what someone
  said, finding or summarizing a conversation, following new messages, looking
  up a contact, saving an attachment they were sent — or asks to send a text.
---

# msg — iMessages from the command line

`msg` reads the Messages database on this Mac directly and sends through
Messages.app. Reading is entirely local — no server, no third-party service,
nothing leaves the machine. Sending is the exception by definition: it
delivers a message to a real person.

Messages are private. Read what the task needs and no more, and quote message
text only when the user asked to read their messages.

## Reading

```sh
msg chats                      # conversations by recent activity, one row per person
msg chats dana                 # filter by name, handle, or identifier
msg chat dana                  # print one conversation (plural lists, singular opens)
msg chat dana -n 200           # more history (default 50)
msg chat dana --since 7d       # durations: 30m, 2h, 7d, 4w — or 2026-01-15
msg chat dana --who            # name who reacted: ← ❤️ sam
msg chat dana --tapbacks       # reactions as their own rows, with timestamps
msg contacts bob               # who somebody is: every address, name + nickname
msg watch                      # follow new messages as they arrive; --json for NDJSON
```

Senders with no saved contact — verification codes, delivery texts — are
filtered out the way Messages filters them; `--unknown` includes them. A
verification code is often just a number, which no text search finds, so a
"read me the code I was just texted" errand is two steps, not a search:

```sh
msg chats --unknown -n 5       # the code's sender is usually the top row
msg chat 1287                  # then open that row's id — no --unknown needed
```

Reactions trail the message they react to (`works, see you then ← ❤️`), inline
replies print a `↳ replying to` line above the reply, and attachments print as
`[#48213 IMG_4821.HEIC, 3.2 MB]` in the place they occupy in the message.

## Naming a conversation

- A name matches people from the start of a word: `ana` reaches Ana, not Dana.
- **Quote a name with a space in it.** Each argument is one person:
  `msg chat "Ana Duarte"` is one person, and `msg chat dana sam` is the group
  chat whose members are exactly Dana, Sam, and the user — not any group that
  merely contains them.
- A person with several addresses is one merged conversation; a chat rowid (the
  number in `msg chats`) pins a single thread when the merge is not wanted.
- An address (`dana@example.com`, a phone fragment) matches anywhere and also
  names the person.
- Ambiguity is reported, never guessed: if `msg` lists candidates instead of
  answering, pick one (by rowid or address) rather than retrying the same name.

## Resolving who someone is

`msg contacts resolve` turns a name into identifiers for *other* tools —
an email for Gmail, a number for anything that dials. Same naming rules as
above, but it answers from all of Contacts, messaged or not, and it never
guesses between two people:

```sh
msg contacts resolve dana --json     # {id, name, filedAs?, emails[], phones[]}
msg contacts resolve dana --emails   # every email address, one per line
msg contacts resolve dana --email    # exactly one, or exit 3 naming candidates
msg contacts resolve dana --phone    # exactly one phone number
```

The exit status is the contract: 0 one person, 1 nobody (or none of the kind
a singular flag asked for), 2 Contacts unreadable even in part, 3 not unique
— several people, or several values under `--email`/`--phone`. stdout is
empty on every failure, so gate the composition on the substitution:

```sh
email=$(msg contacts resolve dana --email) && gog gmail search "from:$email"
```

On exit 3 the candidates are on stderr, each with an address — re-run with
that address (or the exact full name), not the same fragment again.

## Searching

```sh
msg search "dinner"                    # across every conversation
msg search "deploy" -c "Ship Room"     # within one conversation
msg search "dinner" --with dana        # theirs everywhere + yours where it's just you two
msg search "dinner" --from dana        # only what they sent
msg search "invoice" --since 30d -n 50
msg search "dinner" -C 2               # context: 2 messages either side of each hit
```

`> ` marks hits, two spaces mark context, `--` separates runs, as in grep.
**`-c` and `-C` differ only in case**: `-c` scopes to a conversation, `-C` sets
context width. Do not confuse them — `-c 3` is a legitimate chat rowid.

## Machine-readable output

Every read command takes `--json`; `watch --json` emits one object per line.
The one gap: `msg contacts` with no operands prints a human count, never
JSON. Prefer `--json` when filtering or counting rather than parsing the
human layout:

```sh
msg search "invoice" --json | jq '.[] | {date, sender, body}'
```

## Attachments

The `#48213` in a body is an attachment id. `msg save 48213 --to ~/Downloads`
writes the file there; an existing file is refused unless `--force`.

## Sending

Sending is off by default, and most people never turn it on — reading and
searching answer "what did they say", and everything else in this skill works
with sending disabled.

**Never send unless the user explicitly asked to send that message**, and
always preview first:

```sh
msg send dana "on my way" --dry-run    # prints recipient + body, sends nothing
msg send dana@example.com "on my way"  # send to the address the dry run named
```

The dry run names the exact address (`would send to Dana Reyes
(dana@example.com): on my way`) because one person can have several routes,
and a bare name resolves to whichever route spoke last — which can change
between preview and send if a message arrives in the meantime. **Repeat the
real send with the address the dry run printed**, which pins the route; a
name re-resolved a second time is not the verification the dry run was. Show
the dry-run line to the user before sending when there is any doubt about the
recipient. `--dry-run` works whether or not sending is enabled, so the
preview is always available.

**Enabling sending is the user's decision, confirmed twice — never a fix you
apply.** Two gates hold it shut: `send = true` in `~/.config/msg/config.toml`,
and macOS Automation for `msgd`. If a send fails because a gate is closed, say
which gate and stop. First ask, as its own question rather than a step of the
task, whether they want this machine able to text people from the command
line at all; only after a yes, confirm again before each gate is opened — the
config edit, and the Automation switch under System Settings > Privacy &
Security > Automation, which is theirs to flip.

**`msg daemon automation` texts nobody, but it is not a passive check.** It
probes by sending Messages a TCC-gated Apple Event, which on a machine where
`msgd` has never asked pops the macOS Automation prompt — and approving that
prompt is a persistent grant, the second gate opening. Run it only inside the
enabling flow above, behind the same confirmations. To learn the state
without side effects, read `~/.config/msg/config.toml`; the Automation gate
has no side-effect-free probe, so leave it alone until the user has said yes.

## When it does not work

- **Exit status 2** means the database could not be opened. On the default
  database that is a permission problem: nothing holds Full Disk Access, and
  the fix is `msg daemon install`, then switching `msgd` on under System
  Settings > Privacy & Security > Full Disk Access. Under `--db` or `MSG_DB`
  it is usually a wrong path, which no permission fixes. The error says
  which; relay it rather than retrying.
- Exit status 1 is an ordinary error, including usage errors.
- `msg daemon status` reports installed / running / granted in one place.
- `--db <path>` or `MSG_DB` read a different `chat.db` (fixtures, backups);
  both bypass the daemon.
