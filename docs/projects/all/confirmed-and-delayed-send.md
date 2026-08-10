# Plan: A person confirms every send, and a send can wait

**Status:** Designed, not started. §3's spike — LAContext from launchd-agent
context — has not run, and it gates everything else here. An implementation
was started before this plan existed, on 2026-08-09, and was discarded in
favour of writing this; the decisions it forced are recorded below.

**Goal:** Two features that share one argument. `confirm = true` puts a
dialog in front of every send — the whole message, then Touch ID — so
approval means a person at this Mac, not a process claiming to be one. And
`--in`/`--at` let a send wait, approved now, fired later by the daemon.

## 1 Why

The two gates sending has today answer "may this tool text people?" — a
config key and an OS permission, both standing decisions about the machine.
Neither answers "may it send *this* text?" A confirmation dialog anchored to
`deviceOwnerAuthentication` converts that question into one only a person
present at the Mac can answer, per message.

That flips the agent posture. Today the Agent Skill compensates behaviorally:
never send unless explicitly asked, dry-run first, show the user the
recipient line. Those rules are prompt discipline, and prompt discipline is
advice. With `confirm = true` the human approval is structural — an agent may
*initiate* a send precisely because it cannot *complete* one, which is what
makes agent-initiated sending safe by construction rather than by good
behaviour.

## 2 The dialog shows everything, then asks for a person (DECIDED)

Two steps, in one helper. First an alert carrying the recipient as the dry
run would describe them, the fire time when one is set, and the **entire
message body** in a scrolling view — approved bytes have to be shown bytes,
not a truncation. Then, on "Send…", the `LocalAuthentication` sheet:
`deviceOwnerAuthentication`, which is Touch ID where it exists and falls back
to the login password or an Apple Watch, so a clamshell Mac can still answer.
The property is "a person at this machine", not "a finger on a sensor".

Rejected: putting the message in the LAContext reason string alone — it is a
sentence slot, and a real message clips long before it ends, which un-shows
the thing being approved. Rejected: `biometryOnly`, which fails closed in
clamshell mode for no security gain. Rejected: an osascript `display dialog`,
which any local process can draw and script — and which is itself an Apple
Event, so the confirmation would ride the very permission it guards.

What makes the answer evidence: the biometric sheet is system UI no local
process can dismiss or synthesize, and the daemon reads only the exit status
of a child it spawned itself from inside its own bundle.

## 3 Where it runs, and the spike that gates the plan

`msgd` is a launchd *agent* in the Aqua session, so it should be able to put
UI on the screen. "Should" is the load-bearing word: whether LAContext and an
alert actually present from launchd-agent context is the one thing this plan
cannot reason its way past, so the spike runs first and its measurements land
in this section, the way daemon-and-permissions.md §9 recorded its own.

The helper is a small Swift executable, AppKit for the alert and nothing
else, compiled by `scripts/build.sh` with the `swiftc` the command line tools
already provide, placed inside `msgd.app`, and signed with the bundle. Rust
keeps zero Objective-C and zero framework bindings: the helper is a
subprocess with an exit status, not a dependency, so the four-crate doctrine
and the hand-written-decoder posture survive intact.

`msg daemon confirm` ships as the spike's harness and stays as surface: it
pops the dialog with test text and reports the answer, sending nothing — the
same inspectability `msg daemon automation` gives the other gate.

## 4 Every failure refuses to send (DECIDED)

A missing helper, a crashed helper, an unanswered dialog (two minutes), and a
decline are all the same answer: no send, with a message naming which it was.
Rejected: "confirm when possible", where a missing helper falls through to
sending — that demotes the gate to advice exactly where it matters, since the
likeliest reason the helper is missing is a build that predates it.

## 5 The third key

`confirm = true` joins `send = true` in `~/.config/msg/config.toml`, read by
the daemon for the reason the others are: a check a caller runs on itself is
advice rather than a gate. The flat `key = value` parser grows a second key
and stays deliberately not-TOML. The asymmetry with §1 is worth stating: the
key switches the dialog on, but the dialog's *answer* is OS-enforced — a
process that rewrites the config can strip the question, which is why the
Automation grant remains the outer wall, and why revoking either still kills
sending outright.

## 6 What a person approves is what is sent (DECIDED)

The dialog names the pinned target — the chat guid resolved before the dialog
opened, described with its address — and the exact body or attachment. After
approval nothing re-resolves: the send uses the guid and bytes that were
shown, whatever arrives in the meantime. This is the dry-run race (the P1
from the Agent Skill review) made structural: a name that resolves twice can
name two different routes, so the thing approved has to be the thing
dispatched, not the recipe for re-deriving it. An attachment is named in the
dialog by filename and size; its bytes were already in the daemon's hands
when the dialog opened.

## 7 What it changes for agents

The Agent Skill and README revisions ride the shipping PR. Enabling the gates
stays a twice-confirmed human decision — that section of the skill does not
soften. What softens is the per-send ritual once `confirm = true` holds: an
agent may run `msg send` directly, because the approval it used to simulate
with dry-runs and questions is now a dialog only the user can answer. The
dry-run stays recommended for aim; it stops being the safety mechanism.

## 8 Delayed send: a duration, or a time with a zone (DECIDED)

`msg send dana "..." --in 45m` and `--at 17:00`, `--at "2026-08-12 09:30"`,
`--at "17:00 America/New_York"`, or a full RFC 3339 stamp. A trailing word
containing `/` (or `UTC`) is an IANA zone and the rest is read in it; no zone
means local. A bare time means its next occurrence — today if still ahead,
tomorrow otherwise. A dated time already past is refused rather than fired
immediately, because "send this yesterday" is a mistake and hiding it would
send a message nobody expects. A time inside a spring-forward gap is an
error; across a fall-back, the earlier reading wins, matching `--since`.

Zones mean `chrono-tz`, a fifth dependency, and rust-rewrite.md §8's ledger
gets the entry. The alternatives lose on their own: bare UTC offsets make the
user resolve DST by hand — wrong half the year for exactly the "5pm New York"
ask the feature exists for; shelling to `date(1)` trades a data crate for a
subprocess parse; and shipping without zones ships the foot-gun as the only
mode. `chrono-tz` is compiled-in IANA data with no I/O, maintained beside the
chrono already here.

## 9 The queue is the daemon's own state

Queued sends persist as one JSON object per line in the daemon's state
directory, rewritten whole through a temporary file and rename, so a crash
mid-write leaves the previous queue rather than half of one. Each row is what
was approved: id, fire time, pinned guid, the described recipient, body or
attachment bytes. This *looks* adjacent to the rule that the daemon takes no
path from anyone, and is not: no request names a file, and the daemon writes
only inside its own directory, so the security shape of the socket is
unchanged — worth stating because the resemblance will occur to a reviewer.

A row that fires and fails stays in the file with the error on it, visible in
`msg queue`, and is not retried (DECIDED): a silently vanished send and a
silently repeated one are both worse than a row that says what happened.
Retry-with-backoff is a policy a real failure can argue for later. A machine
asleep at fire time fires on wake — late and saying so beats never. And the
gates hold at fire time too: `send = false` disarms everything already
queued, which is what a kill switch is for.

## 10 Approval happens at schedule time, not fire time (DECIDED)

The dialog for a scheduled send opens when the send is scheduled and names
the fire time — "Send to Dana Reyes (dana@example.com), at 2026-08-12 09:30
-0700 (in 3h 12m)?". Rejected: confirming at fire time, which interrupts
whoever is at the keyboard hours later, or fails because nobody is — and
which turns the queue into a list of undecided questions rather than of
approved sends. Cancelling stays cheap the whole way to the deadline, which
is the honest half of "scheduled": `msg queue --cancel <id>` needs no
authentication, because refusing to send is the direction every failure
already points.

## 11 The CLI surface

`--in` and `--at` on `msg send`, mutually exclusive, parsed client-side so a
typo is a usage error before anything reaches the daemon — and validated
again by the daemon, which stores only what it parsed itself. `--dry-run`
gains the schedule line ("would send to … at …: …") and stays send-free
whatever the gates say. `msg queue` lists rows with id, fire time, recipient,
and body; `--cancel <id>` removes one and says so; a failed row prints its
error. `msg daemon confirm` per §3.

## 12 The wire

`send` gains `at`; `queue` and `confirm` are new requests; the protocol
version bumps. The stale-daemon failure is the sharp kind the version
constant's log keeps cataloguing: an older daemon ignores the unknown `at`
field and sends a scheduled message *immediately*, which is the worst
available misreading of "later" — that alone justifies the bump. Note for
whoever lands second: the contact-resolution plan also bumps this constant on
its own branch, and two protocols cannot share a number, so the later merge
renumbers.

## 13 Slices

The spike is first and alone: §3's question answered on this machine, its
numbers written into this plan, before any feature code. If it passes, the
rest is one PR — the helper and confirm gate, the queue and schedule flags,
the docs — per the sizing convention, since the two features share the §1
argument and the second is not worth reviewing without the first. If the
spike fails, §2 falls back to designing around a login-item helper app, and
this plan gets corrected rather than worked around.
