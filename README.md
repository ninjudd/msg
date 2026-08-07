# msg

Read and send iMessages from the command line.

Talks directly to the Messages database on your Mac. No server, no REST API, no
third-party service holding your messages.

## Requirements

- macOS with Messages signed in
- Node 24 or newer (uses the built-in `node:sqlite`)
- Full Disk Access for your terminal, in System Settings > Privacy & Security >
  Full Disk Access. The terminal must be restarted after granting it.

## Install

```sh
pnpm install
pnpm build
```

Run it from the source tree with `pnpm msg <command>`, or link the built
binary with `pnpm link --global` to get `msg` on your PATH.

## Usage

```sh
msg chats                      # conversations by most recent activity
msg chats dana                 # filter by contact name, handle, or identifier

msg contacts                   # how many handles are known
msg contacts +13105551234      # the name behind a handle

msg read "Ship Room"           # print a conversation
msg read 42 -n 200             # by chat id, more history
msg read dana --since 7d       # only the last week
msg read dana --tapbacks       # include reactions

msg search "dinner"            # search every conversation
msg search "deploy" -c "Ship Room" --since 30d

msg watch                      # follow new messages as they arrive
msg watch -c dana --json       # JSON lines, one per message

msg send dana "on my way"      # send text
msg send dana --file ~/pic.png # send a file
msg send dana "hi" --dry-run   # show what would be sent
```

Every read command takes `--json` for piping into other tools.

Handles are resolved to contact names automatically, in rendered output and in
JSON alike. Pass `--no-names` to see raw phone numbers and addresses, which also
skips reading Contacts entirely.

A chat can be named by rowid, by handle, or by any substring of its name. If a
substring matches more than one conversation, `msg` lists the candidates rather
than guessing.

## Notes on the Messages database

The schema has a few traps, which this tool handles for you:

- **Dates are nanoseconds since 2001-01-01**, not Unix seconds. They exceed
  `Number.MAX_SAFE_INTEGER`, so every integer is read as a BigInt. Reading them
  as JavaScript numbers throws.
- **`message.text` is almost always NULL.** In a 20,000 message sample from a
  real database, 97.6% of messages carried their body only in
  `attributedBody`, an NSArchiver typedstream blob. `src/apple.ts` decodes it
  without any Objective-C bridge.
- **Tapbacks are messages**, distinguished by `associated_message_type != 0`.
  They are hidden unless you pass `--tapbacks`.
- **A chat's name** comes from `display_name`, which is empty for direct
  messages, so it falls back to the participant handles.
- The database is opened read-only. If the write-ahead log cannot be opened
  alongside it, `msg` copies the database to a temporary file and reads that.

## Notes on Contacts

Names come from the Contacts databases under
`~/Library/Application Support/AddressBook`, which `msg` reads directly. There
is one database per source (iCloud, local, and any other account), and all of
them are merged.

Contacts stores a number in whatever shape it was typed. A real database here
held ten different formats for the same kind of number, including
`+13105551234`, `(310) 555-1234`, `310.555.1234` and bare `3105551234`. Both
sides of a comparison are therefore stripped to digits, and numbers long enough
to carry a country code are matched on their last ten digits. Short codes are
matched whole, and email handles are matched case-insensitively.

Contacts is read once per run and only when names are wanted, so `--no-names`
costs nothing. If the databases are missing or unreadable, lookups return
nothing and messages still read normally.

## Development

```sh
pnpm test        # vitest
pnpm typecheck   # tsc --noEmit
```

Point `MSG_DB` at a different database to run against a fixture instead of your
own messages.
