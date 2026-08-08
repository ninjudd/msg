# Plan: Rewrite `msg` in Rust

**Status:** Done. Ported on the `rust-port` branch on 2026-08-07, on §7's second
trigger — this is meant to be installable by someone who does not already have
Node 24 and a clone of the repository. `msg` and `msgd` are Rust, the TypeScript
is deleted, and the daemon running on the author's machine is the Rust one,
holding the same Full Disk Access and Automation grants it held before with
nothing re-granted. §8 is the log of what landed and why.

Sections 1 to 7 are the argument as it was made before the work began, written
while this was still an idea. They are left as they were: nothing in them is a
defect report against the TypeScript build.

**Goal:** Decide when the runtime stops being worth its size, its build, and the
install it forces on anyone else.

**The whole tool, not just the daemon.** A Rust daemon behind a TypeScript CLI
would put the schema knowledge and the typedstream decoder in two languages,
because the CLI still reads the database itself when no daemon is listening.
That is worse than either end state. If this happens, it happens to all of it.

## 1 What it buys

**A smaller trusted computing base.** The daemon holds Full Disk Access, and
that grant today covers node, V8, libuv and OpenSSL — millions of lines, almost
none of which this program uses. A Rust binary links sqlite and std. Every bug
in the parts we do not use runs with the grant anyway.

**No JIT.** V8 needs writable-then-executable memory, so a hardened runtime
would have to carry `allow-jit` and `allow-unsigned-executable-memory`. A Rust
daemon needs neither and can be signed with a hardened runtime and no
exceptions. This is the sharpest difference, and it is categorical rather than a
matter of degree.

**Fewer primitives after a compromise.** Anything that got code execution inside
the daemon today lands in a process with a full interpreter: `child_process`,
`fs`, sockets, `eval`. [§4](daemon-and-permissions.md) removed the *script
argument* problem by embedding the code in the signed binary, but the
interpreter is still in there.

**The build collapses.** esbuild, postject, `--experimental-sea-config`, the
sentinel fuse, and the rule that signing must come last because everything
before it invalidates the signature: all gone. It becomes `cargo build`, plus
the four lines that assemble the app bundle and sign it, which are the same in
any language.

This argument used to lead with `src/macho.ts` disappearing — 200 lines of
load-command arithmetic pinned to node's binary layout, replaced in Rust by a
`-Wl,-sectcreate` link flag. That was true and is now moot:
[daemon-and-permissions.md §13](daemon-and-permissions.md) deleted the module
outright. The daemon ships as an app bundle so its TCC grants can be revoked,
and a bundle carries a real `Contents/Info.plist`, so nothing needs injecting
into the executable at all. **The bundle layout, the `Info.plist`, and the
`codesign --identifier` invocation all carry over unchanged**, and so does the
grant: the requirement is `identifier "com.ninjudd.msgd" and certificate leaf =
H"3347…"`, and neither term depends on what language the binary was written in.
A Rust `msgd` signed with the same certificate under the same identifier
inherits the permissions the current one holds, with nothing to re-grant.

**The install stops being a developer setup.** Today it is clone, `pnpm
install`, `npm link`, and Node 24 or newer for `node:sqlite`. A Rust `msg` is a
binary, installable the way people install command-line tools, with no runtime
to have first. That matters more than it sounds: it is the difference between a
tool this repository's author uses and a tool anyone can.

**Startup.** Every `msg chats` pays node's startup before it does anything. A
command that runs in milliseconds feels different from one that runs in tenths.

**Size.** 5MB rather than 116MB, twice over, since a copy lives in `build/` and
another in `~/.local/libexec`.

## 2 What it does not buy

**Memory safety over what is there now.** The comparison is Rust against
TypeScript, not Rust against C. The one input that arrives from outside the
machine — the typedstream blob in a message someone sends you — is parsed by
memory-safe code either way.

**Anything against the threat the design actually names.** §5 and §6 are about
hostile code running as the user, which does not need to exploit the daemon: it
can talk to the socket, run `msg`, or sign its own binary with the key in
[signing-identity.md](signing-identity.md). A rewrite shrinks a surface that is
not currently the likely one. Worth doing, but it does not reorder the risks.

## 3 What it costs

**One file carries almost all of the risk.** `src/apple.ts` decodes the Apple
epoch and the NSArchiver typedstream, it is the hardest-won code in the
repository, and its tests encode findings from a real database. Everything else
— sqlite, the socket, the protocol, the rendering, the launchd plist — ports
mechanically.

**The tests are the specification.** They should be ported before the code they
cover, and they are worth more than the code.

**Two dependencies get chosen rather than avoided.** `node:sqlite` and commander
become `rusqlite` and `clap`, and the hand-written typedstream decoder stays
hand-written because nothing else decodes it. The current no-runtime-dependency
rule survives in spirit but not in letter.

## 4 The protocol is the seam to port against

The CLI talks to the daemon over newline-delimited JSON and knows nothing about
how the other end is implemented. That is useful during the port rather than
after it: a Rust daemon can be checked against the existing TypeScript client
and vice versa, one end at a time, with the protocol tests as the contract.

## 5 Port order

1. `apple.ts` — epoch conversion and the typedstream decoder, tests first.
2. `db.ts` — the queries, against the same fixture the daemon tests build.
3. `contacts.ts`, including the access ordering in
   [§12](daemon-and-permissions.md).
4. The protocol and the socket, checked against the TypeScript client while both
   exist.
5. `format.ts` and the CLI — rendering, then `clap` in place of commander.
6. The launchd plist, the `Info.plist` link flag, and signing: unchanged in
   substance, much smaller in code.
7. Delete `src/`, `dist/`, and the Node toolchain in one commit, not gradually.

## 6 What is unresolved

- Whether `msg send` keeps shelling out to `osascript` or talks Apple Events
  directly through the Objective-C bridge. The subprocess is simpler and is what
  works today.
- Whether hardened runtime is worth adopting once the JIT entitlements are no
  longer needed, and whether that changes the TCC requirement in a way that
  costs another re-grant.
- How `msg` gets distributed once it is a binary, and whether that pulls in
  notarisation — which would end the self-signed certificate in
  [signing-identity.md](signing-identity.md) and its trade.

## 7 When to do it

Not on size alone, and not on security alone. Two triggers, either of which is
enough:

- **The Mach-O injection breaks on a Node upgrade**, which turns §1's build
  argument from aesthetics into maintenance.
- **Anyone other than the author is expected to install this**, at which point
  "clone the repo and have Node 24" stops being a reasonable ask and §1's
  install argument becomes the main one.

## 8 What has landed

Sections above are the argument; this one is the log. It is appended to as the
port proceeds, in §5's order.

**Where the code lives during the port.** The crate is `rust/`, not the
repository root, so `src/` keeps building and `pnpm test` keeps passing while
the port runs. That is what §4 asks for — one end checked against the other —
and it is why §5 ends by deleting the TypeScript in a single commit rather than
gradually. The crate moves to the root in that commit.

**Dependencies (DECIDED).** Four: `rusqlite` and `clap`, which §3 predicted,
plus `serde` and `chrono`. `serde` is what keeps the wire format byte-identical
to the TypeScript one while both ends exist. `chrono` is local-timezone
arithmetic, which is not worth hand-writing.

Two that were considered and refused:

- **No `regex`.** This program matches five patterns — a duration, a chat guid,
  a config line, a `find-identity` line, and non-digits in a phone number. Each
  is a dozen lines of hand-written parsing, and hand-writing them keeps the
  strictness explicit. The duration parser is the case that matters: `f64::from_str`
  would have accepted `-1h`, `1e3h`, `.5h`, and `infh`, none of which the
  original `\d+(?:\.\d+)?` pattern took.
- **No `tokio`.** The daemon is a thread per connection and a tick, which is
  what the TypeScript event loop was doing anyway. An async runtime inside the
  process holding Full Disk Access is exactly the kind of code §1 wants less of.

**§5.1 `apple.rs` is done, and was checked against the TypeScript rather than
against its own tests.** Unit tests were ported first as planned, but they only
prove the cases someone thought of, and §3 calls this the file carrying almost
all of the risk. So both decoders were run over the same 5,489 synthetic blobs —
valid bodies, every truncation of a valid body, 5,000 single-byte mutations, and
random noise — and agreed on every one, including which 1,786 of them decode to
nothing. Epoch conversion was compared the same way over 485 timestamps spanning
every power of ten either side of the seconds/nanoseconds threshold. The corpus
is synthetic and generated by a script; nothing in it came from a real database.

Three details that had to be preserved exactly, none of them obvious from
reading the TypeScript:

- A typedstream length is a **signed** byte unless a width marker escapes it, so
  `0x84` is −124 and means "not a string", not "132 bytes".
- The seconds/nanoseconds threshold is `>`, not `>=`, so exactly `1e12` reads as
  seconds and lands in the year 33689. Kept, because a real database will never
  hold that value and matching the original everywhere is worth more than
  correcting it here.
- A bare `2026-01-15` in `--since` is midnight **UTC**, because that is what
  JavaScript's `new Date` does with a date-only string. Local midnight is
  probably what a user means, but changing it silently during a port is worse
  than carrying the wart.

**§5.2 and §5.3, `db.rs` and `contacts.rs`, are done.** Contacts came first
despite §5's order, because `db.rs` needs the index type to name people in a
chat. Checked the same way `apple.rs` was: both implementations ran seventeen
query shapes against the same on-disk fixture — bodies in `attributedBody`
only, NULL handles, a zero date, unicode, tapbacks, filtered conversations,
limits, `--since` cutoffs, rowid and name resolution — and produced identical
JSON, 14,592 bytes of it, field for field.

Three things the port improved rather than preserved, each because Rust removes
the reason the workaround existed:

- **The BigInt dance is gone.** Apple dates are around 8.1e17, past
  `Number.MAX_SAFE_INTEGER`, so every TypeScript statement had to call
  `setReadBigInts(true)` and every arithmetic operation had to stay in BigInt.
  An `i64` is an `i64`. That removes a whole class of "worked until the number
  got big" from the file, and it is the one place where the rewrite makes the
  code plainly more correct rather than merely smaller.
- **The readiness probe no longer confuses "empty" with "unreadable".**
  `SELECT 1 FROM message LIMIT 1` was `.get()` in TypeScript, which returns
  undefined for no rows. `rusqlite`'s equivalent, `query_row`, reports no rows as
  an *error*, which would have sent a perfectly readable but empty database down
  the snapshot-copy path. It steps the statement instead.
- **The snapshot directory is `mkdtemp(3)`**, called through libc, rather than
  reimplemented. Same atomic create, same mode 0700.

One thing preserved that looks like a bug and is not: when macOS names no
default Contacts source, both sides of the source comparison are absent, which
makes the legacy top-level database the preferred one. That is what the
TypeScript `===` did, so it is what this does.

**§5.4, the protocol and the socket, is done, and §4's seam paid off exactly as
written.** The Rust daemon was checked by pointing the *TypeScript* CLI at it —
no Rust client involved — and it answers every command: `chats`, `read`,
`search`, `contacts`, `status`, `send --dry-run`, and streaming `watch`. Then
both daemons were run against the same fixture and asked the same ten questions
through that same client: 393 lines of JSON, identical. A message inserted while
the TypeScript client was watching the Rust daemon arrived as an `item` frame
with the fields in the right shape.

That is the check worth having. Ported tests prove the new code matches what
someone thought the old code did; running the old client against the new server
proves it matches what the old code *actually* did.

**No async runtime (DECIDED).** A thread per connection, one tick thread, and a
`Mutex` around the connection. The TypeScript daemon was a single event loop, so
this is not a concurrency model the design depended on. Two consequences worth
recording:

- **Lock order is watchers, then database, never the reverse.** The tick holds
  the watcher list while it queries and writes; every request handler takes the
  two in sequence and never nests them.
- **Watcher writes have a five-second timeout.** Node buffers socket writes in
  userspace and never blocks, so the TypeScript daemon could not be wedged by a
  client that stopped reading. A blocking `write` inside the watcher lock can be,
  so it is bounded, and a watcher that trips it is dropped.

**Watch latency is polled rather than event-driven.** The TypeScript daemon
paired a 2-second timer with `fs.watch` on the database directory, reaching
about 100ms. Rust has no equivalent without either a `notify` dependency or
hand-written FSEvents FFI, and both cost more than they buy: `SELECT MAX(rowid)`
against a local SQLite file is cheap, so the tick asks every 200ms while a
watcher is attached and every 2 seconds when none is. Idle costs a wakeup and no
query. Net latency is better than what it replaces, with no new dependency.

**One improvement over the original.** The TypeScript daemon reported a
malformed but *known* request the same way it reported an unknown command,
because both fell out of the same switch. Here the command name is checked
against the known set before the body is parsed, so `{"cmd":"nonesuch"}` and
`{"cmd":"read"}` with no chat give different, accurate errors. The protocol test
that guards the version-bump rule (§13 of daemon-and-permissions) is ported and
still passes.

**The daemon takes its config path as an option.** The TypeScript tests set
`MSG_CONFIG` in the process environment to keep sending switched off; in Rust
`set_var` is `unsafe` and racy against concurrent test threads. `DaemonOptions`
carries it instead, alongside the database path, which is the same
"the daemon's own environment, never a client" category. The test suite asserts
the gate is shut rather than assuming it.

**§5.5 and most of §5.6 are done: rendering, `source.rs`, the `clap` CLI, and
`install.rs`.** Both binaries build — `msg` at 3.0MB and `msgd` at 2.4MB against
the 116MB Node SEA each replaces.

The check was every combination that exists while both builds do. Sixteen
commands were run through six paths — each CLI against each daemon, and each CLI
reading the database directly — and five of the six are byte-identical to the
TypeScript-through-TypeScript baseline, 159 lines each.

The sixth, the TypeScript CLI talking to the Rust daemon, differs in one way:
`serde_json` sorts object keys, so a `--json` payload that the TypeScript client
re-serialises comes out alphabetical rather than in field order. Normalising key
order makes it identical too. This is not worth a dependency to fix
(`preserve_order` pulls in `indexmap` for cosmetics) and it disappears with the
TypeScript: a Rust CLI parses into the struct and prints in field order, which
is why the other five paths match exactly. Worth recording only so the next
person who diffs them is not surprised.

`watch` was checked separately across all three of its paths — Rust CLI to Rust
daemon, Rust CLI to TypeScript daemon, and Rust CLI direct — and all three
deliver an identical frame for a message inserted while they were listening.
That the Rust *client* satisfies the TypeScript *daemon* is §4's other
direction, and closes it.

**Two behaviour changes, both deliberate:**

- **Timestamps are English.** `Intl.DateTimeFormat` rendered `9:35 AM` and
  `Jan 15, 9:36 AM` in the system locale. There is no ICU here and it is not
  worth 10MB for two format strings, so they are fixed. Identical on this
  machine; English on a machine set to something else.
- **Column widths count terminal cells.** This took two goes and the second one
  came from review. `String.length` is UTF-16 units, so it measured `café` as
  four and an astral emoji as two. Switching to `chars().count()` looked like a
  fix and was half a regression: it made `😀` measure 1 where a terminal draws 2,
  so every column after an emoji-named conversation shifted a cell, and UTF-16
  had been accidentally right for exactly that case. The question being asked is
  display width, which is UAX #11, so `unicode-width` answers it. Truncation
  drops a wide character whole rather than splitting it.

**Exit status 2 was nearly lost.** The README documents 2 as "the data is there,
the grant is not" and tells people to branch on it. `clap` exits 2 for a usage
error by default, so `-n 0` would have claimed the permission status. Usage
errors are forced to 1, matching commander, and `tests/cli.rs` pins both that
and the permission path — including the case that broke once before, where a
readable-but-refused database raised a raw errno instead of the explanation.

**`built_bundle()` changed meaning.** It resolved from the source file's own URL,
which only made sense inside a checkout. A shipped `msg` has no checkout, so the
bundle now travels beside the binary and `--from` names it otherwise.

**§5.6 is done, and §1's central claim held.** `scripts/build.sh` replaces
`scripts/build-msgd.mjs`: a compile, a directory, and a signature. Gone with it
are esbuild, `--experimental-sea-config`, postject, the sentinel fuse, and the
rule that signing must come last because everything before it invalidated the
signature. The bundle layout, the `Info.plist`, and the `codesign --identifier`
invocation carry over unchanged, exactly as predicted.

The claim worth checking was that the grant survives the language change. It
does, and not by argument — the requirement is byte-identical:

```
designated => identifier "com.ninjudd.msgd" and certificate leaf = H"33473595…"
```

Neither term mentions a language or a hash of the executable, so the Rust daemon
signed with the same certificate under the same identifier satisfies the same
requirement the installed one does. `codesign --verify --strict` agrees. Nothing
has to be re-granted.

Sizes, for the record: the bundle is **2.9MB against 116MB**, and `msg` is 2.9MB
against a second copy of the same runtime.

**Checked against the real database, without reading it.** Both CLIs were run
against the live daemon — 763,304 messages, 1,123 contact handles — across seven
query shapes including a 1,164-conversation sweep and three searches, and
compared by hash of canonicalised JSON. All seven identical. Hashing rather than
diffing is the point: it proves agreement without any message body or contact
name leaving the process.

What this does *not* yet cover is the Rust reader against real data: both runs
went through the TypeScript daemon, so what was compared is the Rust client and
the wire. Exercising `db.rs` and the typedstream decoder against the real
database needs the Rust daemon installed, because it needs the Full Disk Access
grant. That is the one remaining check, and it is the one that changes the
machine.

## 9 What §5.7 removed, and what is left open

The TypeScript went in one commit, as §5 asked: `src/` (25 files), `package.json`,
`pnpm-lock.yaml`, `pnpm-workspace.yaml`, both `tsconfig`s, `vitest.config.ts`,
and `scripts/build-msgd.mjs`. The crate moved from `rust/` to the repository
root, so `src/` means the source again. Git recorded every move as a rename.

**The claim in §1 that mattered most held, measured rather than argued.** The
Rust daemon was installed over the Node one and started with both grants intact:
763,304 messages read and 1,123 contact handles resolved on first run, and
`msg daemon automation` reports *allowed*. Nothing was re-granted, because the
designated requirement is byte-identical:

```
identifier "com.ninjudd.msgd" and certificate leaf = H"33473595…"
```

The seven query hashes taken from the *TypeScript* reader against the real
database before the swap were re-run against the *Rust* reader after it, and all
seven match. That is the strongest check available: same database, same
questions, different implementations, identical answers, and no message body or
contact name printed to get it.

Measured, for the record:

| | Node | Rust |
| --- | --- | --- |
| `msgd.app` | 116MB | 2.9MB |
| `msg` startup (`--version`) | 67ms | 23ms |
| Runtime to install first | Node 24 | none |

**What did not get faster.** A real `msg chats` takes about 2.1 seconds on this
database in *both* builds. That is `CHATS_SQL` running correlated subqueries per
conversation over 1,164 of them, and it is unchanged behaviour rather than a
regression — but it is now the slowest thing in the program by two orders of
magnitude, and worth its own piece of work.

**Coverage lost.** `parseIdentities` had three tests; its replacement is a
`grep -q` inside `scripts/build.sh` and has none. The shell script as a whole is
untested, which is the price of it being twenty lines instead of a module.

**§6's open questions, revisited.** Sending still shells out to `osascript`, and
should stay there until there is a reason to move: the subprocess is simpler and
it works. Hardened runtime is now *possible* — there is no JIT to grant
exceptions for — but adopting it changes the code requirement, which is exactly
the thing the grant is anchored to, so it costs a re-grant and belongs in
[signing-identity.md](signing-identity.md) rather than here. Distribution is
untouched and still the reason notarisation may eventually matter.
