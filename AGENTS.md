# Project Guidelines

## Docs

This repo follows the Projector `docs/projects/` convention, with one difference:
there is no `docs/` overview, because [README.md](README.md) plays that role —
it describes how `msg` works today, for someone using it. The work itself is in
[`docs/projects/`](docs/projects/README.md).

[skills/msg/SKILL.md](skills/msg/SKILL.md) is user-facing documentation too —
the Agent Skill that teaches Claude, Codex, and anything else speaking the
format to drive `msg`. A behaviour change that touches the README's usage
sections probably touches the skill as well; keep the two telling one story.

## Never send a message you did not mean to send

`msg send` texts real people. Treat it the way you would treat a production
write.

- **Use `--dry-run` when verifying anything.** Not "usually" — always, unless
  the explicit task is to send a message the user asked for.
- A send attempt has a second consequence beyond the message: the first one
  triggers a macOS Automation prompt, and approving it permanently grants the
  asking process the right to drive Messages. That already happened once here
  (`com.googlecode.iterm2 | com.apple.MobileSMS`, 2026-08-06), from a
  verification command labeled `--dry-run` that omitted the flag. It only failed
  to send because the fixture chat id did not exist. That grant has since been
  revoked.
- Sending now runs in the daemon, off unless `send = true` is in
  `~/.config/msg/config.toml` *and* macOS has granted `msgd` Automation
  ([daemon-and-permissions.md §7](docs/projects/daemon-and-permissions/readme.md)).
  Neither gate is a substitute for the flag: on a machine where both are open,
  a missing `--dry-run` texts someone.

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

- **Dates are nanoseconds since 2001-01-01**, around 8.1e17. An ordinary `i64`,
  but past `Number.MAX_SAFE_INTEGER`, which is why the JavaScript build carried
  BigInt arithmetic everywhere. Values below 1e12 are legacy seconds, not
  nanoseconds.
- **`message.text` is usually NULL.** The body lives in `attributedBody` as an
  NSArchiver typedstream. `src/apple.rs` decodes it by hand.
- **Tapbacks are messages**, with `associated_message_type != 0`.
- **Filtered chats are the Unknown Senders bucket**, `chat.is_filtered`.
- **Reading Contacts preferences poisons Contacts file access.** Calling
  `defaults read com.apple.AddressBook` before opening the AddressBook databases
  makes TCC refuse them with `EPERM` for the rest of the process, even with Full
  Disk Access. Read the databases first
  ([daemon-and-permissions.md §12](docs/projects/daemon-and-permissions/readme.md)).
  Full Disk Access is a property of each access, not of the process.

Verify a schema assumption against the database before building on it. Column
names, value ranges, and which flags are actually populated all vary by macOS
version, and this repo has already been wrong about each.

## Don't ship speculative complexity

A fix for a case that has not been observed is worse than no fix: it is
untested code that implies a problem exists. When investigation shows the
suspected problem is not real, delete the fix rather than keeping it as
insurance. Add it back with a failing case attached if one ever appears.

## Toolchain

Rust, edition 2024. Four dependencies — `rusqlite`, `clap`, `serde`, `chrono` —
and the reasons each was chosen, along with the two that were refused (`regex`
and `tokio`), are in
[rust-rewrite.md §8](docs/projects/rust-rewrite/readme.md). The typedstream decoder
is hand-written because nothing else decodes it.

```sh
cargo test                    # unit tests, tests/cli.rs, tests/daemon.rs
cargo clippy --all-targets    # expected to be silent
cargo fmt
./scripts/build.sh            # both binaries, and the signed daemon bundle
```

`cargo run --bin msg -- <cmd>` runs from source. `build/` is gitignored, and
`~/.local/bin/msg` is a symlink into it, so a source change is not live until
`./scripts/build.sh`.

**Both binaries have to be rebuilt and the daemon reinstalled** for a change to
reach the process holding Full Disk Access: `./scripts/build.sh && msg daemon
install`. The grant survives that, because it is anchored to the signing
certificate and the bundle identifier rather than to the executable's hash.

Do not add a lint allow to silence clippy; fix the code or say in a comment why
the lint is wrong here.

## Permissions

Reading requires Full Disk Access, held either by
[the daemon](docs/projects/daemon-and-permissions/readme.md) or by the terminal.
TCC attributes access to the responsible process, so granting it to a
CLI-spawned child does nothing; a launchd job is its own responsible process,
which is why `msgd` exists.

The CLI reads the database itself when no daemon is listening. With a grant on
neither side, `open_database` returns `Error::AccessDenied` and the CLI exits
with status 2 and an explanation. Keep that path working, since it is the first
thing a new user hits — and it had already broken once: the snapshot fallback
raised a raw `EPERM` from the copy instead of the explanation, and nothing caught
it because the development machine held the grant. `tests/cli.rs` now pins both
that path and the exit statuses, but revoke the grant before trusting either.
