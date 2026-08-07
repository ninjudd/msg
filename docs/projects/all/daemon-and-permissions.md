# Plan: A daemon, so the terminal stops holding Full Disk Access

**Status:** Designed, not started. `msg` currently requires Full Disk Access on
the terminal, which is what this replaces.

**Goal:** Move the privileged read into a launchd agent that holds Full Disk
Access on its own, so the CLI needs no permission at all and a compromised shell
gets messages rather than the whole disk.

## 1 Why

`msg` reads `~/Library/Messages/chat.db`, which TCC protects. Today that means
granting Full Disk Access to the terminal, and Full Disk Access is not scoped to
messages: it covers Mail, Safari history, Photos, every application container,
and every file the user can read. Every command run in that terminal inherits
it, including anything an agent runs.

That is a large grant to buy one database. The daemon exists to make the grant
proportional to what the tool actually does.

## 2 How TCC decides, and why the obvious fixes fail

TCC attributes a file access to the process **responsible** for it, not to the
binary performing the read. A process spawned from a shell is charged to the
terminal application. Two consequences follow, and both have been verified on
this machine:

- **Granting Full Disk Access to `node` does nothing.** The responsible process
  is still the terminal, so the grant is never consulted.
- **A daemon spawned by the CLI inherits the same problem.** This is worth
  stating plainly because the obvious model to copy gets it wrong: bloom's
  `audiod` (`src/audiod/client.ts`) spawns itself with `child_process.spawn`
  when nobody owns its socket. Spawned that way, the daemon's responsible
  process is the terminal, and its reads are charged to the terminal exactly as
  before. The pattern is right for audio and useless for permissions.

What breaks the chain is **launchd**. A launchd-managed job is its own
responsible process and holds TCC grants in its own right. The system TCC
database confirms a bare executable can hold Full Disk Access without being an
application bundle:

```
/usr/libexec/sshd-keygen-wrapper | client_type=1 (absolute path) | auth_value=2
```

`client_type=1` means the client is identified by path. No `.app` required.

## 3 The shape

A **LaunchAgent** in `~/Library/LaunchAgents` declaring a `Sockets` key. launchd
owns the listening socket, starts the daemon on first connect, and the daemon
idle-exits after a timeout. That is on-demand in the real sense, and it is why
this is a LaunchAgent rather than a LaunchDaemon: agents run inside the user's
GUI session, which is required for any TCC prompt to appear at all. A
LaunchDaemon would fail to prompt with no way to approve.

The CLI connects to the socket and needs no permission of any kind.

## 4 The daemon's TCC identity is its main executable (DECIDED)

A plist that runs `node /path/to/msgd.js` makes the TCC client **node**, not the
script. That is wrong in two ways: the grant lands on
`~/.nvm/versions/node/<version>/bin/node` and therefore covers every
launchd-spawned node process, and an nvm upgrade replaces the binary and voids
it.

**Decided:** compile the daemon to its own executable with Node's Single
Executable Application support, so `msgd` is the TCC client.

Rejected alternatives:

- **Run node directly.** Simplest, but the grant is both too broad and too
  fragile, which defeats the point of the exercise.
- **Wrap it in a minimal `.app` bundle.** Works, and a bundle identifier
  (`client_type=0`) survives rebuilds better than a cdhash. Heavier to build and
  harder to explain in an install step; worth revisiting if grant churn becomes
  annoying in practice.

Either way, an unsigned or ad-hoc-signed binary is matched by cdhash, so
rebuilding can invalidate the grant and require re-adding it in System Settings.
Signing with a stable identity avoids that. For a personal tool, re-adding it
occasionally is acceptable; for anyone else installing this, it is the roughest
edge in the design.

## 5 The socket carries no authentication (DECIDED)

The socket is created `0600` in an owner-only directory. The kernel then
restricts it to the owning uid, which fully handles other accounts on the
machine. Beyond that, **nothing**.

This is deliberate, and the reasoning is worth keeping because the opposite
conclusion is intuitive. Any authentication scheme has to answer "is the caller
the legitimate client", and the attacker's move is to *be* the legitimate
client: `msg` is on `PATH`, so hostile code running as the user simply executes
it and reads stdout. That defeats every variant:

- **A shared secret** in a file, environment variable, or keychain item is read
  by any process running as the user, exactly as the real client reads it.
- **Peer code-signature verification** (`SecCodeCheckValidity` against a
  requirement string) authenticates the binary, not the intent. It is the
  mechanism Apple's privileged helpers use, and it is a real boundary in cases
  where the client is not a general-purpose tool. Here the client *is* the
  attack tool, so it verifies nothing that matters.
- **Requiring user presence** (Touch ID per access) is the only scheme that
  survives, because it demands something the attacker cannot supply. It is also
  incompatible with a CLI meant to be piped into scripts.

Anything short of user presence is security theater against the only threat that
matters, and buys nothing over the filesystem permission.

## 6 The security property is scope reduction, not access control

Following §5 to its end: hostile code running as the user can still read the
messages. The daemon does not prevent that and cannot.

What it changes is the size of the prize:

| | Hostile same-uid code gets |
| --- | --- |
| Terminal holds Full Disk Access | `chat.db`, Mail, Safari history, Photos, every container, every readable file |
| Daemon holds it, narrow API | the messages |

That is the entire value of the design, and it survives the fact that the socket
is unauthenticated. It also sets the rule the API has to follow: **the daemon
never takes a path argument.** It answers `chats`, `read`, and `search`, and
nothing that could turn it into a general-purpose reader with Full Disk Access
behind it. Peer pid (`LOCAL_PEERPID`) is worth logging for auditing, but it is
not a control and should not be described as one.

## 7 Sending is off by default, gated twice (DECIDED)

Sending goes through Messages.app over Apple Events, which needs Automation
permission rather than Full Disk Access. Putting it in the daemon makes "may
this tool send?" an **operating system permission** instead of a flag the
process enforces on itself: a single visible toggle under Privacy & Security >
Automation that no config rewrite or runaway script can flip.

Both gates are kept, because they answer different threats:

1. **A config key** (`send = true` in `~/.config/msg/config.toml`). Prevents
   accidents, is self-documenting, and produces a legible refusal naming the key
   instead of an opaque AppleScript `-1743`. Checked first for that reason.
2. **The Automation grant.** Withheld, macOS refuses the event whatever the code
   attempts. This is the layer that holds when the config does not.

A config key alone is honored by the process, so anything that can rewrite the
file can send. An environment variable is worse still: a runaway script or an
agent can set it inline in the same command it uses to send, which is precisely
the accident the gate exists to prevent.

`--dry-run` works unconditionally, so the disabled state stays inspectable.

**This is off for the author's own account and expected to stay off.** The case
it exists for is a Mac running a separate iCloud account, where sending is the
point and the blast radius is a mailbox nobody's personal life runs through.

### 7.1 The grant that already exists

Worth knowing before trusting the Automation lever: on the development machine
the terminal already holds it.

```
com.googlecode.iterm2 | com.apple.MobileSMS | 2 | 2026-08-06 21:17:20
```

That grant was created by a verification command labeled `--dry-run` that
omitted the flag and actually invoked `osascript` against Messages. macOS
prompted, it was approved, and the permission persists. Two lessons, both
already encoded in [AGENTS.md](../../../AGENTS.md): a near-miss can permanently
widen a permission, and until that grant is revoked, denying the daemon
Automation gates `msg` but not the machine, because two lines of `osascript`
still send from any shell.

## 8 What is unresolved

- Whether the Single Executable Application build is stable enough across Node
  upgrades to be worth it over an `.app` bundle (§4).
- The wire protocol. Length-prefixed JSON over the socket is the obvious start,
  and the API surface in §6 is small enough that it does not need to be clever.
- Whether the daemon should hold the Contacts read as well. Contacts sits under
  a different TCC service from Full Disk Access, so `--no-names` already
  sidesteps it independently, and folding it in may not earn its complexity.
- How the install step explains adding a binary to Full Disk Access without it
  reading as something a reasonable person should refuse to do.
