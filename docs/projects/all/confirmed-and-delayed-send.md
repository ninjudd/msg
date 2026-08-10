# Plan: A person confirms every send, and a send can wait

**Status:** Designed, not started. §3's spike — now three questions, not one —
has not run, and it gates everything else here. An implementation was started
before this plan existed, on 2026-08-09, and was discarded in favour of
writing this; the decisions it forced are recorded below. A review on
2026-08-09 then found that the first draft trusted user-writable config and
state against an adversary the feature's own purpose makes a same-uid process;
§14 is the threat model that answer required, and §§2, 3, 5, 6, 9, 10 carry
the corrections back to it.

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

Two steps, both drawn by the daemon's own signed code (§3). First an alert
carrying the recipient as the dry run would describe them, the fire time when
one is set, and the **entire message body** in a scrolling view — approved
bytes have to be shown bytes, not a truncation. Then, on "Send…", the
`LocalAuthentication` sheet: `deviceOwnerAuthentication`, which is Touch ID
where it exists and falls back to the login password or an Apple Watch, so a
clamshell Mac can still answer. The property is "a person at this machine", not
"a finger on a sensor".

Rejected: putting the message in the LAContext reason string alone — it is a
sentence slot, and a real message clips long before it ends, which un-shows
the thing being approved. Rejected: `biometryOnly`, which fails closed in
clamshell mode for no security gain. Rejected: an osascript `display dialog`,
which any local process can draw and script — and which is itself an Apple
Event, so the confirmation would ride the very permission it guards.

What makes the answer evidence: the biometric sheet is system UI no local
process can dismiss or synthesize. But that is only half of it, and the half
this section originally got wrong. The evaluation has to run in code whose
integrity the daemon can vouch for — see §14, which replaces the earlier
claim that reading "the exit status of a child spawned from inside the
bundle" was enough. It is not: the bundle is user-writable, so a spawned
helper trusted by its exit code can be swapped for `exit 0` (§14).

## 3 Where it runs, and the spike that gates the plan

`msgd` is a launchd *agent* in the Aqua session, so it should be able to put
UI on the screen. "Should" is the load-bearing word: whether LAContext and an
alert actually present from launchd-agent context is the one thing this plan
cannot reason its way past, so the spike runs first and its measurements land
in this section, the way daemon-and-permissions.md §9 recorded its own. The
spike now has to answer three questions, not one, because §14 raised the
other two: does an alert present from this context, does `LAContext` present
from it, and does a Keychain item bound to the daemon's code identity read
without a prompt (§14).

The confirmation logic runs **inside `msgd`**, not in a helper trusted by its
exit code. The review's helper-authentication finding is why this section
changed: `~/.local/libexec/`
and everything in the bundle are user-writable, and the daemon investigation
measured that modifying a bundle's resources breaks `codesign --verify`
without invalidating the TCC grant (daemon-and-permissions.md §9), so a
spawned child cannot be authenticated by the fact that its path sits inside a
signed bundle. The daemon therefore links `LocalAuthentication` and AppKit
directly — a Swift compilation unit built into `msgd` by `scripts/build.sh`,
or an FFI binding — so the code that reports "approved" is the same signed
code that holds the grant. Rejected: keeping the helper and verifying its
signature against the daemon's Designated Requirement before spawn — it is
TOCTOU-open (swap between check and exec) and, by the measurement above, a
path inside the bundle is not a code identity anyway. The cost is real and
recorded: this is the first Objective-C surface the project links rather than
shells out to, so rust-rewrite.md §8's ledger gets the entry and the
"hand-written, no bridge" posture gains its first documented exception, taken
because the security property cannot be had any other way.

`msg daemon confirm` ships as the spike's harness and stays as surface: it
pops the dialog with test text and reports the answer, sending nothing — the
same inspectability `msg daemon automation` gives the other gate.

## 4 Every failure refuses to send (DECIDED)

A daemon built before the confirm surface existed, a dialog that fails to
present, an unanswered dialog (two minutes), and a decline are all the same
answer: no send, with a message naming which it was. Rejected: "confirm when
possible", where a gate that cannot present falls through to sending — that
demotes it to advice exactly where it matters, since the likeliest reason the
surface is absent is a stale daemon predating it, and a stale daemon is the
version constant's whole catalogue of quiet failures (§12).

## 5 The third gate can only tighten (DECIDED, corrected)

This section's first draft said `confirm = true` was an ordinary config key
and shrugged that "a process that rewrites the config can strip the question,
which is why the Automation grant remains the outer wall." The review found
that false, and fatal: Automation authorizes `msgd` *globally*, it
does not restore per-message approval, so a same-uid process that flips
`confirm = false` and sends has bypassed the whole feature while every gate
still reads as granted. A gate the adversary can turn off is not a gate (§14).

So confirmation is not a symmetric boolean the daemon reads passively.
**Config can only tighten it.** `confirm = true` in the config turns
confirmation on, because turning it on is safe. Turning it *off* is a
loosening, and a loosening has to be authenticated: it happens through
`msg daemon confirm --disable`, which pops the same Touch ID dialog, and the
daemon records "confirmation is off" only in the trust-anchored state of §14 —
a state a same-uid process cannot forge. Anything the daemon cannot prove is
an authenticated "off" — a missing marker, a tampered one, a bare
`confirm = false` typed into the config — reads as **on**. That is the
fail-closed invariant of §14 applied here: tampering can only demand
confirmation, never skip it. The config key keeps its old meaning for turning
the dialog *on*; what it can no longer do is turn it off. The flat
`key = value` parser still grows the key and stays deliberately not-TOML.

## 6 What a person approves is what is sent (DECIDED)

The dialog names the pinned target — the chat guid resolved before the dialog
opened, described with its address — and the exact body. After approval
nothing re-resolves: the send uses the guid and bytes that were shown,
whatever arrives in the meantime. This is the dry-run race (the P1 from the
Agent Skill review) made structural: a name that resolves twice can name two
different routes, so the thing approved has to be the thing dispatched, not
the recipe for re-deriving it. That the approved bytes then survive
*persistence* unedited is §9's job, via the row MAC; pinning before the
dialog is necessary and not sufficient, which is §14's point.

**A dialog can only vouch for what it renders.** The first draft said an
attachment is "named in the dialog by filename and size", as if that were
approval; the review's answer is that both are caller-controlled, so an agent
can put sensitive or unrelated bytes behind an innocuous name and the person
approves a label, not the content. So the dialog renders the attachment itself:
a thumbnail for an image, the first lines for text — and a type it cannot
preview cannot be agent-confirmed, so that send is refused with a note to send
it by hand (fail closed, §14). Images are the overwhelming majority of what
gets sent, so a thumbnail covers the real case; the unpreviewable tail fails
safe rather than shipping unseen bytes. The bytes are still in the daemon's
hands before the dialog opens, which is what lets it render them and what
closes the post-approval swap — but rendering is the part that makes them
*approved*.

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
mid-write leaves the previous queue rather than half of one. This *looks*
adjacent to the rule that the daemon takes no path from anyone, and is not:
no request names a file, and the daemon writes only inside its own directory,
so the security shape of the socket is unchanged — worth stating because the
resemblance will occur to a reviewer. The rest of this section is four
corrections the review forced, all downstream of §14: the state directory is
user-writable, so a queued row is data the adversary can read and edit.

**Payloads live beside the queue, not in it.** A row holds id, fire
time, pinned guid, described recipient, and the body; an attachment's bytes go
in a separate daemon-owned file named by a daemon-chosen id, and the row keeps
only that id and the metadata. The protocol already documents attachments up
to 548 MB — roughly 750 MB once base64'd — and inlining that into a
whole-file-rewritten queue would push gigabytes through a temp copy on every
enqueue and cancel, stalling the resident daemon. Small JSON rewrites; large
bytes are written once and unlinked when their row leaves.

**Rows are integrity-protected.** Because the row is caller-editable at
rest, pinning the guid and bytes before the dialog (§6) does not keep them
pinned through persistence: a same-uid process can approve a benign row and
then edit its guid, body, fire time, or payload id before it fires, and the
daemon would dispatch what the person never saw. So each row and its payload
carry a MAC keyed by the §14 anchor, written when the row is enqueued after
approval. At fire time a row whose MAC does not verify is not sent — it
becomes a visible failure in `msg queue` (fail closed, §14). This is what
actually preserves the approved-send invariant across time, and what makes
§10's confirm-at-schedule sound rather than a hole.

**An attempt is durable before Messages sees it.** Dispatch is not
atomic with the queue update: if the daemon crashed after Messages accepted
the Apple Event but before the row was removed, a naive queue would resend on
restart, breaking the no-retry rule below. So the sequence is: mark the row
`attempted` and persist, invoke Messages, then remove. A row still marked
`attempted` at startup is an *indeterminate* outcome — it may or may not have
gone — so it is never re-dispatched; it surfaces in `msg queue` as an
attempted-unknown failure for the person to judge, which is the honest state
and the safe one.

A row that fires and fails stays with the error on it, visible in `msg
queue`, and is not retried (DECIDED): a silently vanished send and a silently
repeated one are both worse than a row that says what happened.
Retry-with-backoff is a policy a real failure can argue for later. The gates
hold at fire time too: `send = false`, or confirmation reverting to on per §5,
disarms everything already queued, which is what a kill switch is for.

**Lateness is bounded, and uninstall clears the queue.** A machine
asleep at fire time fires on wake if the delay is small — late and saying so
beats never — but a row overdue beyond a bounded window (a few hours) fires as
an expired failure rather than sending, because a message the user expected
hours ago is no longer the message they meant. That bound also closes the
resurrection the review noticed: `msg daemon uninstall` today leaves the state
directory and the Automation grant in place, so a pending row could fire —
possibly instantly overdue — after a later reinstall, though the user had
stopped the service. Uninstall therefore deletes the queue and its payload
store outright: a send you can no longer see or cancel must not outlive the
tool that showed it. The overdue bound is the belt to that suspenders, in
case state is restored from a backup around the uninstall.

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

Confirm-at-schedule only holds if the approved row cannot be edited between
the dialog and the fire — otherwise an attacker approves a benign row and
swaps it, and scheduling has widened the window for exactly that. That is why
§9 MACs each row: confirm-at-schedule and the row integrity are one decision
seen from two sections, not two.

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

The spike is first and alone: §3's three questions answered on this machine,
their numbers written into this plan, before any feature code — the alert and
`LAContext` presenting from launchd-agent context, and the §14 Keychain item
reading without a prompt under the daemon's identity. If they pass, the rest
is one PR — the in-process confirm gate, the queue and schedule flags, the
docs — per the sizing convention, since the two features share the §1 argument
and the second is not worth reviewing without the first. If the UI questions
fail, §2 falls back to designing around a login-item helper app; if the
Keychain question fails, §14's fallback (mandatory confirmation, no
authenticated "off") is what ships. Either way the plan gets corrected here
rather than worked around.

## 14 The adversary is a same-uid process, and what that forces (DECIDED)

This is the section a review added after the first draft trusted, in six
places, something the attacker controls. It belongs at the front conceptually
and is at the end structurally, because sections here are cited by number and
none may move (docs/projects/README.md).

The feature exists to make a send safe to *initiate* by a process the user
does not fully trust — the agent — which runs as the user. So the adversary in
every section above is a same-uid process. It can read and write
`~/.config/msg/`, the daemon's whole state directory, and everything inside
the user-writable `~/.local/libexec/msgd.app`. What it cannot do is forge a
biometric, sign code with the `msg dev` certificate (that key lives in the
login keychain and prompts on use, signing-identity.md), or make the
daemon's own in-process code lie. Every correction below turns on that line
between what the adversary can and cannot touch.

**Fail closed.** Every uncertainty resolves toward *not sending* or
*requiring confirmation*, never toward sending. Tampering with the config, the
state directory, a queued row, or the confirmation code can force the safe
direction — confirmation demanded, send refused — and can never force the
unsafe one. A gate an attacker can only tighten is a gate; one they can loosen
is the prompt discipline §1 was trying to replace.

**One trust anchor the caller cannot forge.** Two corrections need the daemon
to hold a secret a same-uid process cannot read: the MAC over queued rows and
payloads (§9), and the "confirmation is off" marker that only an authenticated
disable may set (§5). The macOS primitive is a Keychain item the daemon
creates under a `SecAccessControl` bound to its own code identity, so the
signed `msgd` reads it without a prompt while other same-uid code is refused.
Whether that non-interactive access actually holds for a launchd agent's
cert-based identity is unmeasured, so it is the third question of §3's spike.
If it does not hold, the fallback is safe by construction: confirmation is
mandatory with no "off" at all (so no marker to protect), and a row that
cannot be integrity-checked is simply re-confirmed or dropped rather than
sent. Less convenient, never less safe.

**The in-process consequence.** The helper-authentication finding — that a
helper spawned from the writable bundle cannot be authenticated by its
path — is the same line drawn again: the code that says "approved" has to
be code the adversary cannot replace, which means the daemon's own signed
process, not a child of it. §3 records what that costs.
