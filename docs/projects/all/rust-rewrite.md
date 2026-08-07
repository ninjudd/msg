# Plan: Rewrite `msg` in Rust

**Status:** Idea, not committed to. Everything works as it is and nothing here
is a defect report. This records why a rewrite is worth considering, what it
would actually buy, and what it would cost, so the argument does not have to be
had again from scratch.

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

**The build collapses.** `src/macho.ts` disappears entirely: in Rust the
`Info.plist` is a link flag, `-Wl,-sectcreate,__TEXT,__info_plist,Info.plist`,
rather than 200 lines of load-command arithmetic pinned to node's binary layout.
So do esbuild, postject, `--experimental-sea-config`, the sentinel fuse, and the
rule that signing must come last because everything before it invalidates the
signature. It becomes `cargo build`.

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
