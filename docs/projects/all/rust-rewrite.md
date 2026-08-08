# Plan: Rewrite `msg` in Rust

**Status:** In progress on the `rust-port` branch, started 2026-08-07 on §7's
second trigger — this is meant to be installable by someone who does not already
have Node 24 and a clone of the repository. §8 records what has landed and the
decisions taken while porting.

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
