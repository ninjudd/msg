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
Messages.app. Everything is local; nothing leaves the machine.

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
filtered out the way Messages filters them. `--unknown` includes them, so a
"read me the code I was just texted" errand is
`msg chats --unknown` or `msg search "code" --unknown --since 10m`.

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

## Searching

```sh
msg search "dinner"                    # across every conversation
msg search "deploy" -c "Ship Room"     # within one conversation
msg search "dinner" --with dana        # the exchange with one person, all chats
msg search "dinner" --from dana        # only what they sent
msg search "invoice" --since 30d -n 50
msg search "dinner" -C 2               # context: 2 messages either side of each hit
```

`> ` marks hits, two spaces mark context, `--` separates runs, as in grep.
**`-c` and `-C` differ only in case**: `-c` scopes to a conversation, `-C` sets
context width. Do not confuse them — `-c 3` is a legitimate chat rowid.

## Machine-readable output

Every read command takes `--json`; `watch --json` emits one object per line.
Prefer it when filtering or counting rather than parsing the human layout:

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
msg send dana "on my way"              # only after the dry run named the right address
```

The dry run names the exact address (`would send to Dana Reyes
(dana@example.com): on my way`) because one person can have several routes.
Show that line to the user before sending when there is any doubt about the
recipient. `--dry-run` works whether or not sending is enabled, so the preview
is always available.

**Enabling sending is the user's decision, confirmed twice — never a fix you
apply.** Two gates hold it shut: `send = true` in `~/.config/msg/config.toml`,
and macOS Automation for `msgd` (`msg daemon automation` reports both without
sending anything). If a send fails because a gate is closed, say which gate
and stop. First ask, as its own question rather than a step of the task,
whether they want this machine able to text people from the command line at
all; only after a yes, confirm again before each gate is opened — the config
edit, and the Automation switch under System Settings > Privacy & Security >
Automation, which is theirs to flip.

## When it does not work

- **Exit status 2** means nothing holds Full Disk Access. The error explains
  the fix; the short version is `msg daemon install`, then switching `msgd` on
  under System Settings > Privacy & Security > Full Disk Access. Relay the
  message rather than retrying.
- Exit status 1 is an ordinary error, including usage errors.
- `msg daemon status` reports installed / running / granted in one place.
- `--db <path>` or `MSG_DB` read a different `chat.db` (fixtures, backups);
  both bypass the daemon.
