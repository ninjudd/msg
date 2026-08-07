# Plan: A daemon, so the terminal stops holding Full Disk Access

**Status:** Shipped. The daemon reads and sends, and the CLI holds no grant of
its own — Full Disk Access and Automation are both attributed to `msgd`, and
each can be given or withheld without the other.
[§9](#9-what-the-spike-measured-2026-08-07) records the spike that validated the
permission model, [§10](#10-what-shipped-2026-08-07) what building it settled,
[§11](#11-what-sending-needed-2026-08-07) the one thing §7 assumed that turned
out to need work, and [§12](#12-what-contacts-needed-2026-08-07) why a grant
that plainly worked stopped applying halfway through a function.

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

A **LaunchAgent** in `~/Library/LaunchAgents`, resident: `RunAtLoad` and
`KeepAlive`, binding its own `0600` socket. It is a LaunchAgent rather than a
LaunchDaemon because agents run inside the user's GUI session, which is required
for any TCC prompt to appear at all. A LaunchDaemon would fail to prompt with no
way to approve.

The CLI connects to the socket and needs no permission of any kind.

**Correction (2026-08-07).** This section previously specified a `Sockets` key,
with launchd owning the listening socket, starting the daemon on first connect,
and the daemon idle-exiting after a timeout. That is not reachable from Node: a
socket-activated job collects its listener with `launch_activate_socket(3)`, a C
API with no Node binding and no FFI in Node 24 to reach it. Every Apple job
using `SockPathName` is a C binary. Two ways out were weighed:

- **`inetdCompatibility`**, where launchd hands over the already-accepted
  connection on stdin and stdout, needing no C at all. It costs a process spawn
  per connection, and every Apple example uses network rather than unix sockets,
  so it would have to be tested before being relied on.
- **Resident, with no socket activation.** Chosen. It costs an idle node
  process, and it is what makes `watch` better rather than merely possible: one
  process tailing `chat.db-wal` and pushing to subscribers beats N terminals
  each polling on their own timer. Idle-exit was a nicety that cost a C
  dependency.

## 4 The daemon's TCC identity is its main executable (DECIDED)

A plist that runs `node /path/to/msgd.js` makes the TCC client **node**, not the
script. That is wrong in two ways: the grant lands on
`~/.nvm/versions/node/<version>/bin/node` and therefore covers every
launchd-spawned node process, and an nvm upgrade replaces the binary and voids
it.

**Decided:** compile the daemon to its own executable with Node's Single
Executable Application support, so `msgd` is the TCC client.

The decisive reason is not the one first written down here. A copy of the node
binary sitting at a granted path is a **confused deputy**: the binary holds the
grant, but the code it runs is whatever it is handed. The plist supplies the
script path, and `~/Library/LaunchAgents` is user-writable, so rewriting
`ProgramArguments` and calling `launchctl kickstart` runs arbitrary code with
Full Disk Access using nothing but public tooling. The script file is equally
exposed: §9 overwrote one in place, re-signed nothing, and the replacement ran
with the grant intact, because TCC does not check a bundle's resource seal.
Root ownership closes both routes, but it leaves the design resting on file
permissions around an interpreter. A SEA has no script argument to redirect and
no resource file to swap, and its JS lives in the signed Mach-O the kernel does
enforce — measured in §9, a SEA handed `/tmp/attacker.js --evil` recorded the
argument and ran its own embedded code regardless. It also makes the writable
plist stop mattering, since pointing it at anything else runs a binary that
holds no grant.

Rejected alternatives:

- **Run node directly.** Simplest, but the grant is both too broad and too
  fragile, which defeats the point of the exercise.
- **Copy the node binary and pass it a script path.** Works, and needs no build
  step at all — §9 measured it reading `chat.db` under launchd. It is the
  confused deputy above.
- **Wrap it in a minimal `.app` bundle.** Rejected, though it was left open for
  a while. §9 confirmed it works and registers as `client_type=0`, keyed by
  bundle identifier, so its grant is independent of where the app lives. What
  kept it open was the belief that a bundle is the only way to carry an
  `Info.plist`, which §7 needs for Apple Events. That is wrong, and §11 replaces
  it: a bare executable carries one in `__TEXT,__info_plist`. The install-step
  advantage it was expected to have did not materialize either — it appears in
  the Full Disk Access list under its executable's filename rather than the
  bundle name, which is what made it unfindable during the spike.

**Correction (2026-08-07).** This section previously stated that an unsigned or
ad-hoc-signed binary is matched by cdhash, so rebuilding can invalidate the
grant, and called that the roughest edge in the design. Only the ad-hoc half is
true. Signed with a stable identity the requirement anchors to the certificate
instead: §9 rebuilt a SEA with changed code and a changed cdhash at the same
path, and the grant held. Two practical notes follow. Sign with an explicit
`--identifier com.ninjudd.msgd`, because the identifier otherwise defaults to
the filename and a rename would void the grant. Prefer a self-signed Code
Signing certificate to an Apple Development one, which expires annually and
would take the grant with it.

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

Two consequences for the API as it grows. **`watch` belongs in the daemon**, and
is the one command a daemon makes better rather than merely possible, so the
protocol needs a streaming response from the start rather than bolted on later.
**Attachments are addressed by rowid, never by path** — `later.md` wants them,
and an API taking an attachment path is precisely the general-purpose reader
this rule exists to prevent. `--db` and `MSG_DB` stay client-side and never
reach the daemon, which keeps fixtures and tests working without widening what
the daemon will answer.

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

**The CLI's direct send path has to go when the daemon lands.** While
`src/commands/send.ts` shells out to `osascript` itself, withholding Automation
from the daemon gates nothing, because the CLI never asked it. For the same
reason the config key is checked by the daemon rather than by the client: a
check a caller performs on itself is advice, not a gate.

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

- How the install step explains adding a binary to Full Disk Access without it
  reading as something a reasonable person should refuse to do. §9 turned up
  three hazards to write around: the list can take minutes to show a newly added
  entry, entries are labelled by executable filename so two installs stack as
  identical rows, and a grant outlives its binary — deleting the app leaves
  `auth=2` behind, and `tccutil reset SystemPolicyAllFiles <bundle-id>` is the
  only way to withdraw one that no longer shows in the list.

Resolved by building it, and previously listed here:

- The wire protocol, and whether it could stay simple. It did: see §10.
- What happens when the daemon is not installed. The CLI reads the database
  itself, for the reason in §10.
- Whether `msgd` ships bare or inside an `.app` bundle. Bare. The bundle was
  only ever wanted as somewhere to put `NSAppleEventsUsageDescription`, and a
  bare executable can carry one: see §11.

Resolved by the spike, and previously listed here:

- Whether the SEA build is worth it over an `.app` bundle on grant-churn
  grounds. The churn does not exist when the binary is signed with a stable
  identity (§4).
- Whether the daemon should hold the Contacts read. It should, and it costs
  nothing: Full Disk Access covers the AddressBook databases, so no second grant
  is needed. A job holding no grant could not even list the `Sources` directory,
  so the alternative is not "names still work", it is "names do not work".

## 9 What the spike measured (2026-08-07)

Two throwaway experiments under launchd, run with **no terminal holding Full
Disk Access** for the duration. Each variant was a stub that opened the
databases and reported row counts, nothing else. Every variant was run once
before being granted anything: the denial is the control, and it is what rules
out a stray terminal grant as the explanation for the success that follows.

| Variant | Identity TCC recorded | Read `chat.db` once granted |
| --- | --- | --- |
| `.app` bundle, ad-hoc signed | `client_type=0`, bundle identifier | yes |
| bare copy of `node` plus a script | `client_type=1`, absolute path | yes |
| SEA, signed with a stable identity | `client_type=1`, absolute path | yes |

Findings, in the order they changed the plan:

1. **The premise holds.** A launchd job reads `chat.db` while no terminal on the
   machine holds Full Disk Access (§2, §3).
2. **A rebuild keeps the grant** when the binary carries a stable signing
   identity, changed cdhash and all (§4).
3. **A SEA ignores a script path in argv.** A copied `node` runs it (§4).
4. **Modifying a bundle's `Resources` broke `codesign --verify` but not the
   grant.** The replacement code ran with Full Disk Access, unsigned and
   unnoticed (§4).
5. **Full Disk Access covers Contacts** (§8).
6. **Grants outlive the binaries and apps they were granted to** (§8).
7. **A denied attempt is what creates the TCC entry.** There is no CLI to add a
   Full Disk Access grant — only `tccutil` to remove one — so the install flow
   is: run the daemon, let it fail, then switch on the row that failure created.

## 10 What shipped (2026-08-07)

Everything in §1 through §6, and none of §7.

**The wire is newline-delimited JSON, one request per connection.** `chats`,
`read`, `search`, `resolve`, `contacts` and `status` answer with a single
`result` frame and close; `watch` streams `item` frames until the client
disconnects. The request carries its protocol version and a mismatch is refused
by name, so a CLI left behind by an upgrade gets an instruction rather than a
parse error. Length-prefixing was never needed.

**`resolve` was not in §6's list of commands.** `send` has to turn a name into a
chat guid before it can address anything, and that lookup is the one every other
command already does internally. It returns a chat and takes no path, so the
rule §6 sets still holds.

**The CLI still reads the database when no daemon is listening.** §8 asked which
way this should go, and the fallback wins for a reason external to this plan:
AGENTS.md requires the `AccessDeniedError` path to keep working because it is
the first thing a new user meets, and without the fallback a machine that has
not installed the daemon has no working `msg` at all. `--db` is answered locally
in every case, so a path argument never reaches the daemon.

**Sending stayed in the CLI in the first pass**, and moved into the daemon in
the second. §11 records what that took.

**Two things building it turned up.** A unix socket path is capped at 104 bytes
on macOS and the kernel reports a bare `EINVAL`, so the daemon checks the length
and says so itself. And the snapshot fallback in `openDatabase` threw a raw
`EPERM` from `copyFileSync` rather than `AccessDeniedError`: the documented
"exits 2 with an explanation" path had been broken for as long as the
development machine held the grant, and revoking it is what exposed that.

## 11 What sending needed (2026-08-07)

§7 assumed the Automation grant was there to be withheld. It was not there to be
given either, and finding out took a second round of measurement.

**A client with no usage description cannot ask.** A bare SEA under launchd was
refused with `-1743`, with **no prompt and no entry created** in Privacy &
Security > Automation. That last part is what makes it different from Full Disk
Access, where §9 found that the denial is exactly what creates the row to switch
on. Here there is nothing to switch on, so the grant is unreachable rather than
merely absent.

**A bare executable can carry `NSAppleEventsUsageDescription` after all.** It
goes in a `__TEXT,__info_plist` section, which is what `/usr/bin/osascript` and
`/usr/libexec/sshd-keygen-wrapper` do — the same wrapper §2 cites for holding
Full Disk Access without a bundle. This retires the `.app` question in §8: the
bundle was only ever wanted as somewhere to put this string.

**Nothing on a stock machine can add that section**, which is the part that cost
real work. `postject` refuses, because it needs a sentinel in the binary that
only exists for the SEA blob, and there is no `llvm-objcopy`. `src/macho.ts`
does it directly: a `section_64` is inserted into `__TEXT`'s section list, the
load commands after it shift, and the payload goes in the padding between the
load commands and the first section's data. Nothing outside the header moves and
the file does not change length.

**The payload goes at the far end of that padding, not the near end.** Placed
immediately after the load commands it was silently overwritten, because
`codesign` appends `LC_CODE_SIGNATURE` there afterwards. The only symptom was
`codesign --verify` reporting an invalid Info.plist, which reads like a signing
problem rather than a layout one.

With the section embedded, the same binary prompts — naming `msgd`, not the
terminal — and the event goes through once approved. Both halves of §7 are now
real: the config key is checked by the daemon, and the Automation grant is a
switch macOS enforces underneath it.

**What the CLI lost.** `src/commands/send.ts` is gone. Sending without a daemon
now fails with an instruction to install one, because a send path in the CLI
would make the Automation gate decorative — the CLI would simply send without
asking macOS for anything, which is the situation §7 exists to end.

## 12 What contacts needed (2026-08-07)

Names stopped resolving the moment reading moved into the daemon. `msg read
"<a contact name>"` answered `no chat matching`, while reading by rowid or
handle worked, because the daemon's contact index was empty.

**Full Disk Access was granted and working.** The daemon read 763,232 messages
from `chat.db`, and its TCC row was there to see:
`kTCCServiceSystemPolicyAllFiles type=1 auth=2 /Users/…/.local/libexec/msgd`.
There was no Contacts row, and §9 had already measured that a launchd binary
with Full Disk Access reads the AddressBook databases without one.

**The order of two calls decided whether that stayed true.** `loadContacts`
consulted `defaults read com.apple.AddressBook` for the preferred source before
opening any database. Asking for those preferences appears to make TCC start
enforcing the Contacts service against the process, and every subsequent access
to `~/Library/Application Support/AddressBook` is then refused with `EPERM` —
Full Disk Access notwithstanding. Two builds differing only in whether the
directory was read before or after that call saw 1,123 handles and none.

Reading the databases first and consulting the preference afterwards costs
nothing, since the preference only decides which source wins a tie.

**Three things hid it**, and all three are now fixed:

- `loadContacts` caught every error and returned an empty index, so a refused
  read looked exactly like a machine with no contacts.
- The enumeration used `globSync`, which swallowed the `EPERM` and returned no
  matches. `readdirSync` raises it, which is how the cause became visible at
  all. A glob that cannot distinguish "nothing there" from "not allowed" has no
  place on a TCC-protected path.
- The daemon cached the empty index for ten minutes. Since the install flow
  guarantees the daemon runs before its grant exists, the first load is expected
  to fail, and caching that failure kept names broken long after the grant.

The general lesson is worth more than the fix: **Full Disk Access is not a
property of the process, it is a property of each access.** Something the
process did earlier can change the answer.
