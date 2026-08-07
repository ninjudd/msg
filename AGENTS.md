# Project Guidelines

## Docs

[README.md](README.md) describes how `msg` works today, for someone using it.
Keep it current when behaviour changes.

[`docs/projects/`](docs/projects/README.md) is the work itself: three lists
([now](docs/projects/now.md), [next](docs/projects/next.md),
[later](docs/projects/later.md)) pointing into
[`all/`](docs/projects/all), where every plan lives and nothing ever moves. Read
[docs/projects/README.md](docs/projects/README.md) before adding to it. Plans
are cited by section (`daemon-and-permissions.md §5`), so renumbering a section
silently breaks references, and a plan whose status line is stale is worse than
one with no status at all.

## Never send a message you did not mean to send

`msg send` texts real people. Treat it the way you would treat a production
write.

- **Use `--dry-run` when verifying anything.** Not "usually" — always, unless
  the explicit task is to send a message the user asked for.
- A send attempt has a second consequence beyond the message: the first one
  triggers a macOS Automation prompt, and approving it permanently grants the
  terminal the right to drive Messages. That already happened once here
  (`com.googlecode.iterm2 | com.apple.MobileSMS`, 2026-08-06), from a
  verification command labeled `--dry-run` that omitted the flag. It only failed
  to send because the fixture chat id did not exist.
- Sending is planned to be off by default behind a config key
  ([daemon-and-permissions.md §7](docs/projects/all/daemon-and-permissions.md)).
  Until that lands, the only thing between a careless command and a real message
  is the flag.

## This repository is public

`github.com/ninjudd/msg` is public. Nothing derived from the author's own data
belongs in it.

- No real names, phone numbers, email addresses, or message text in code, tests,
  fixtures, documentation, commit messages, or README examples. Invent them
  (`+13105551234`, `dana@example.com`) and keep invented values obviously fake.
- Aggregate statistics measured from a real database are acceptable when they
  are non-identifying and earn their place, as in "97.6% of messages carried
  their body only in `attributedBody`". Prefer them to anecdotes, and do not
  reach for a real example when a synthetic one makes the same point.

## Reading the user's messages

Validating against the real database is often the only way to know something
works, and it is expected. What is not expected is spraying private content into
a transcript.

- Report aggregates and shapes: counts, decode rates, digit-masked formats,
  character classes. Not bodies, not contact names.
- Print real message text only when the user asked to read their messages.
- `MSG_DB` and `--db` point at any database. Build a fixture with the real
  schema and drive the CLI against that whenever the question does not require
  live data.

## The Messages database has sharp edges

Most of the code exists to handle these, and each was found by testing rather
than by reasoning. Assume there are more.

- **Dates are nanoseconds since 2001-01-01**, around 8.1e17, past
  `Number.MAX_SAFE_INTEGER`. `node:sqlite` throws rather than narrowing them, so
  every statement calls `setReadBigInts(true)` and conversion happens in BigInt
  arithmetic.
- **`message.text` is usually NULL.** The body lives in `attributedBody` as an
  NSArchiver typedstream. `src/apple.ts` decodes it in plain TypeScript.
- **Tapbacks are messages**, with `associated_message_type != 0`.
- **Filtered chats are the Unknown Senders bucket**, `chat.is_filtered`.

Verify a schema assumption against the database before building on it. Column
names, value ranges, and which flags are actually populated all vary by macOS
version, and this repo has already been wrong about each.

## Don't ship speculative complexity

A fix for a case that has not been observed is worse than no fix: it is
untested code that implies a problem exists. When investigation shows the
suspected problem is not real, delete the fix rather than keeping it as
insurance. Add it back with a failing case attached if one ever appears.

## Toolchain

pnpm, Node 24 or newer, TypeScript in strict mode with
`noUncheckedIndexedAccess` and `exactOptionalPropertyTypes`. Commander for the
CLI. No runtime dependency beyond commander: SQLite is `node:sqlite` and the
typedstream decoder is hand-written, both deliberately.

```sh
pnpm test        # vitest
pnpm typecheck   # tsc --noEmit
pnpm build       # compiles src/ to dist/
pnpm msg <cmd>   # run from source via tsx
```

Install the command with `npm link`, not `pnpm link --global`. pnpm's global bin
directory is not on this machine's PATH and `pnpm setup` does not fix it
cleanly; `npm link` puts `msg` in the nvm prefix, which is already on PATH.

`dist/` is gitignored, and the globally linked `msg` runs from it, so a source
change is not live until `pnpm build`.

## Permissions

Reading requires Full Disk Access, held either by
[the daemon](docs/projects/all/daemon-and-permissions.md) or by the terminal.
TCC attributes access to the responsible process, so granting it to `node` or to
a CLI-spawned child does nothing; a launchd job is its own responsible process,
which is why `msgd` exists.

The CLI reads the database itself when no daemon is listening. With a grant on
neither side, `openDatabase` throws `AccessDeniedError` and the CLI exits with
status 2 and an explanation. Keep that path working, since it is the first thing
a new user hits — and it had already broken once: the snapshot fallback raised a
raw `EPERM` from `copyFileSync` instead of the explanation, and nothing caught it
because the development machine held the grant. Revoke it before trusting that
path.
